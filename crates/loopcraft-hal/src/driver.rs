//! LoopCraft driver logic: any number of virtual devices, each with a hidden
//! engine twin on the same clock.
//!
//! For a device with N output channels and M Pass-Thru channels:
//! - apps play M channels into the device (its output stream); that audio goes
//!   into the Pass-Thru ring;
//! - the engine twin's input stream reads the Pass-Thru ring (M channels) and
//!   its output stream writes the output-channel ring (N channels);
//! - apps recording from the device (its input stream) read the output-channel
//!   ring. While no engine is running, they read the Pass-Thru ring directly
//!   instead (channel n → channel n), i.e. a plain pass-through.
//!
//! Object IDs come in blocks of 16 per device (see [`Role`]). The registry is
//! swapped atomically so the IO path never takes a lock.

use crate::consts::*;
use crate::ffi::{
    cfstring, status, string_from_cf, AudioChannelDescription, AudioChannelLayoutStereo, AudioObjectID,
    AudioObjectPropertyAddress, AudioServerPlugInIOCycleInfo, AudioStreamBasicDescription,
    AudioStreamRangedDescription, AudioValueRange, OSStatus,
};
use crate::spec::{self, DeviceSpec, SAMPLE_RATES, TWIN_SUFFIX};
use arc_swap::ArcSwap;
use lc_clock::{DeviceClock, HostTimeBase, ZeroTimeStamp};
use lc_ring::{region_bytes, HeapRegion, Ring};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

pub const PLUGIN: AudioObjectID = kAudioObjectPlugInObject;
pub use crate::spec::{BUNDLE_ID, DEVICES_PROPERTY};
pub const MODEL_UID: &str = "org.kerpo.loopcraft:model:virtual";
pub const MANUFACTURER: &str = "LoopCraft";
const PLIST_TYPE: u32 = u32::from_be_bytes(*b"plst");
const STORAGE_KEY: &str = "devices";

pub const RING_FRAMES: u32 = 32_768;
pub const ZERO_TIMESTAMP_PERIOD: u32 = 8_192;
/// Gain controls: the slider's bottom is silence, the lowest finite level
/// reported is MIN_DB, unity (0 dB) sits at UNITY_SCALAR and the top is +12 dB.
pub const MIN_DB: f32 = -96.0;
pub const MAX_DB: f32 = 12.0;
pub const UNITY_SCALAR: f32 = 0.75;
const FIRST_BLOCK: u32 = 128;
/// Object IDs per mixer: the fixed roles, one engine stream per app slot and
/// per virtual device, and four objects per virtual device.
const BLOCK: u32 = 128;
const _: () = assert!(Role::VIRT_BASE as usize + 4 * spec::MAX_VIRTUALS <= BLOCK as usize);
/// Devices per mixer: main, twin and the virtual devices.
const DEVICE_SLOTS: usize = 2 + spec::MAX_VIRTUALS;
/// `kAudioServerPlugInIOOperationProcessOutput`: one client's output, before mixing.
const PROCESS_OUTPUT: u32 = u32::from_be_bytes(*b"pout");

const S_UNKNOWN_PROPERTY: OSStatus = status(kAudioHardwareUnknownPropertyError);
const S_BAD_SIZE: OSStatus = status(kAudioHardwareBadPropertySizeError);
const S_BAD_OBJECT: OSStatus = status(kAudioHardwareBadObjectError);
const S_ILLEGAL: OSStatus = status(kAudioHardwareIllegalOperationError);
const S_UNSUPPORTED_FORMAT: OSStatus = status(kAudioDeviceUnsupportedFormatError);

// ------------------------------------------------------------------ values

/// A property value ready to be serialised for the HAL.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    U32(u32),
    F32(f32),
    F64(f64),
    /// A string returned as a +1 CFStringRef.
    Str(String),
    Ids(Vec<AudioObjectID>),
    U32Pair([u32; 2]),
    Range(AudioValueRange),
    Ranges(Vec<AudioValueRange>),
    Format(AudioStreamBasicDescription),
    Formats(Vec<AudioStreamRangedDescription>),
    Layout(AudioChannelLayoutStereo),
    /// Custom property descriptions: (selector, data type, qualifier type).
    CustomInfo(Vec<[u32; 3]>),
}

impl PartialEq for AudioChannelLayoutStereo {
    fn eq(&self, other: &Self) -> bool {
        self.mChannelLayoutTag == other.mChannelLayoutTag
            && self.mNumberChannelDescriptions == other.mNumberChannelDescriptions
            && self.mChannelDescriptions.iter().zip(other.mChannelDescriptions.iter()).all(|(a, b)| a.mChannelLabel == b.mChannelLabel)
    }
}

fn bytes_of<T: Copy>(v: &T) -> &[u8] {
    unsafe { std::slice::from_raw_parts(v as *const T as *const u8, std::mem::size_of::<T>()) }
}

fn bytes_of_slice<T: Copy>(v: &[T]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, std::mem::size_of_val(v)) }
}

impl Value {
    pub fn size(&self) -> usize {
        use std::mem::size_of;
        match self {
            Value::U32(_) | Value::F32(_) => 4,
            Value::F64(_) => 8,
            Value::Str(_) => size_of::<*const ()>(),
            Value::Ids(v) => v.len() * 4,
            Value::U32Pair(_) => 8,
            Value::Range(_) => size_of::<AudioValueRange>(),
            Value::Ranges(v) => v.len() * size_of::<AudioValueRange>(),
            Value::Format(_) => size_of::<AudioStreamBasicDescription>(),
            Value::Formats(v) => v.len() * size_of::<AudioStreamRangedDescription>(),
            Value::Layout(_) => size_of::<AudioChannelLayoutStereo>(),
            Value::CustomInfo(v) => v.len() * 12,
        }
    }

    /// Writes the value into `out`. Arrays are truncated to whole elements;
    /// scalars need the full size. Returns the number of bytes written.
    pub fn write(&self, out: &mut [u8]) -> Result<usize, OSStatus> {
        fn put(out: &mut [u8], src: &[u8]) -> Result<usize, OSStatus> {
            if out.len() < src.len() {
                return Err(S_BAD_SIZE);
            }
            out[..src.len()].copy_from_slice(src);
            Ok(src.len())
        }
        fn put_array(out: &mut [u8], src: &[u8], elem: usize) -> Result<usize, OSStatus> {
            let n = (out.len() / elem * elem).min(src.len());
            out[..n].copy_from_slice(&src[..n]);
            Ok(n)
        }
        use std::mem::size_of;
        match self {
            Value::U32(v) => put(out, &v.to_ne_bytes()),
            Value::F32(v) => put(out, &v.to_ne_bytes()),
            Value::F64(v) => put(out, &v.to_ne_bytes()),
            Value::Str(s) => {
                if out.len() < size_of::<*const ()>() {
                    return Err(S_BAD_SIZE);
                }
                put(out, &(cfstring(s) as usize).to_ne_bytes())
            }
            Value::Ids(v) => put_array(out, bytes_of_slice(v), 4),
            Value::U32Pair(v) => put(out, bytes_of(v)),
            Value::Range(v) => put(out, bytes_of(v)),
            Value::Ranges(v) => put_array(out, bytes_of_slice(v), size_of::<AudioValueRange>()),
            Value::Format(v) => put(out, bytes_of(v)),
            Value::Formats(v) => put_array(out, bytes_of_slice(v), size_of::<AudioStreamRangedDescription>()),
            Value::Layout(v) => put(out, bytes_of(v)),
            Value::CustomInfo(v) => put_array(out, bytes_of_slice(v), 12),
        }
    }
}

/// Callbacks into the HAL host, abstracted for testing.
pub trait Host: Send + Sync {
    fn properties_changed(&self, object: AudioObjectID, addresses: &[AudioObjectPropertyAddress]);
    fn request_config_change(&self, device: AudioObjectID, action: u64);
    fn copy_from_storage(&self, key: &str) -> Option<String>;
    fn write_to_storage(&self, key: &str, value: &str);
}

fn addr(selector: u32, scope: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress { mSelector: selector, mScope: scope, mElement: kAudioObjectPropertyElementMain }
}

pub fn format_for(rate: f64, channels: u32) -> AudioStreamBasicDescription {
    AudioStreamBasicDescription {
        mSampleRate: rate,
        mFormatID: kAudioFormatLinearPCM,
        mFormatFlags: kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked | kAudioFormatFlagsNativeEndian,
        mBytesPerPacket: 4 * channels,
        mFramesPerPacket: 1,
        mBytesPerFrame: 4 * channels,
        mChannelsPerFrame: channels,
        mBitsPerChannel: 32,
        mReserved: 0,
    }
}

/// Slider position (0..1) to dB, on a console-style fader law: unity at
/// UNITY_SCALAR, linear in dB up to +12 dB above it, 40·log10 below it
/// (half way to unity ≈ −12 dB), and silence at the bottom.
pub fn scalar_to_db(scalar: f32) -> f32 {
    let s = scalar.clamp(0.0, 1.0);
    if s >= UNITY_SCALAR {
        (s - UNITY_SCALAR) / (1.0 - UNITY_SCALAR) * MAX_DB
    } else if s <= 0.0 {
        MIN_DB
    } else {
        (40.0 * (s / UNITY_SCALAR).log10()).max(MIN_DB)
    }
}

pub fn db_to_scalar(db: f32) -> f32 {
    let db = db.clamp(MIN_DB, MAX_DB);
    if db >= 0.0 {
        UNITY_SCALAR + db / MAX_DB * (1.0 - UNITY_SCALAR)
    } else if db <= MIN_DB {
        0.0
    } else {
        UNITY_SCALAR * 10f32.powf(db / 40.0)
    }
}

/// Linear gain for a slider position; the bottom of the range is silence.
pub fn scalar_to_gain(scalar: f32) -> f32 {
    if scalar <= 0.0 {
        0.0
    } else {
        10f32.powf(scalar_to_db(scalar) / 20.0)
    }
}

/// Per-direction volume and mute, readable from the IO thread without locks.
struct Level {
    scalar_bits: AtomicU32,
    gain_bits: AtomicU32,
    muted: AtomicBool,
}

impl Level {
    fn new() -> Self {
        Self { scalar_bits: AtomicU32::new(UNITY_SCALAR.to_bits()), gain_bits: AtomicU32::new(1f32.to_bits()), muted: AtomicBool::new(false) }
    }

    fn scalar(&self) -> f32 {
        f32::from_bits(self.scalar_bits.load(Ordering::Relaxed))
    }

    fn set_scalar(&self, scalar: f32) {
        let s = scalar.clamp(0.0, 1.0);
        self.scalar_bits.store(s.to_bits(), Ordering::Relaxed);
        self.gain_bits.store(scalar_to_gain(s).to_bits(), Ordering::Relaxed);
    }

    fn effective_gain(&self) -> f32 {
        if self.muted.load(Ordering::Relaxed) {
            0.0
        } else {
            f32::from_bits(self.gain_bits.load(Ordering::Relaxed))
        }
    }
}

// ----------------------------------------------------------------- objects

/// Position of an object inside its device's ID block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Main,
    MainIn,
    MainOut,
    VolumeIn,
    MuteIn,
    VolumeOut,
    MuteOut,
    Twin,
    TwinIn,
    TwinOut,
    /// Twin input stream carrying app slot k.
    TwinApp(u8),
    /// Twin stream for the virtual device at spec index i: an input for a
    /// virtual input, an output for a virtual output.
    TwinVirt(u8),
    /// Virtual device in slot s (a slot stays with its virtual id), its one
    /// stream (output for a virtual input, input for a virtual output) and its
    /// gain and mute controls.
    Virt(u8),
    VirtStream(u8),
    VirtGain(u8),
    VirtMute(u8),
}

impl Role {
    const FIXED: [Role; 10] = [
        Role::Main,
        Role::MainIn,
        Role::MainOut,
        Role::VolumeIn,
        Role::MuteIn,
        Role::VolumeOut,
        Role::MuteOut,
        Role::Twin,
        Role::TwinIn,
        Role::TwinOut,
    ];

    const APP_BASE: u32 = Role::FIXED.len() as u32;
    const TWIN_VIRT_BASE: u32 = Role::APP_BASE + spec::MAX_APPS as u32;
    const VIRT_BASE: u32 = Role::TWIN_VIRT_BASE + spec::MAX_VIRTUALS as u32;

    fn offset(self) -> u32 {
        match self {
            Role::TwinApp(k) => Role::APP_BASE + u32::from(k),
            Role::TwinVirt(i) => Role::TWIN_VIRT_BASE + u32::from(i),
            Role::Virt(s) => Role::VIRT_BASE + 4 * u32::from(s),
            Role::VirtStream(s) => Role::VIRT_BASE + 4 * u32::from(s) + 1,
            Role::VirtGain(s) => Role::VIRT_BASE + 4 * u32::from(s) + 2,
            Role::VirtMute(s) => Role::VIRT_BASE + 4 * u32::from(s) + 3,
            r => Role::FIXED.iter().position(|f| *f == r).unwrap() as u32,
        }
    }

    fn from_offset(o: u32) -> Option<Role> {
        if let Some(r) = Role::FIXED.get(o as usize) {
            return Some(*r);
        }
        if o < Role::TWIN_VIRT_BASE {
            return Some(Role::TwinApp((o - Role::APP_BASE) as u8));
        }
        if o < Role::VIRT_BASE {
            return Some(Role::TwinVirt((o - Role::TWIN_VIRT_BASE) as u8));
        }
        let (s, kind) = ((o - Role::VIRT_BASE) / 4, (o - Role::VIRT_BASE) % 4);
        if s as usize >= spec::MAX_VIRTUALS {
            return None;
        }
        let s = s as u8;
        Some([Role::Virt(s), Role::VirtStream(s), Role::VirtGain(s), Role::VirtMute(s)][kind as usize])
    }

    fn is_stream(self) -> bool {
        matches!(self, Role::MainIn | Role::MainOut | Role::TwinIn | Role::TwinOut | Role::TwinApp(_) | Role::TwinVirt(_) | Role::VirtStream(_))
    }

    fn is_device(self) -> bool {
        matches!(self, Role::Main | Role::Twin | Role::Virt(_))
    }

    fn device(self) -> Role {
        match self {
            Role::Twin | Role::TwinIn | Role::TwinOut | Role::TwinApp(_) | Role::TwinVirt(_) => Role::Twin,
            Role::Virt(s) | Role::VirtStream(s) | Role::VirtGain(s) | Role::VirtMute(s) => Role::Virt(s),
            _ => Role::Main,
        }
    }

    /// Index of the role's device in per-device state (layouts, IO counts).
    fn device_slot(self) -> usize {
        match self.device() {
            Role::Main => 0,
            Role::Twin => 1,
            Role::Virt(s) => 2 + usize::from(s),
            _ => unreachable!("device() returns a device"),
        }
    }
}

/// Stream shapes and the rings behind them.
struct Layout {
    channels: u32,
    pass_thru: u32,
    /// Audio apps play into the device (M channels); absent when M = 0.
    pass_thru_ring: Option<Ring<HeapRegion>>,
    /// The output channels recording apps receive (N channels).
    output_ring: Ring<HeapRegion>,
    /// One per app slot: that app's output, captured before mixing (M channels).
    app_rings: Vec<Ring<HeapRegion>>,
    /// Virtual devices in spec order: id, channels, direction and the ring
    /// between the device and the engine.
    virt_ids: Vec<String>,
    virt_channels: Vec<u32>,
    virt_outputs: Vec<bool>,
    virt_rings: Vec<Ring<HeapRegion>>,
    /// Virtual-device slot → spec index.
    virt_index: [Option<u8>; spec::MAX_VIRTUALS],
}

impl Layout {
    fn new(spec: &DeviceSpec, slots: &[Option<String>], rate: f64) -> Self {
        let ring = |ch| Ring::create(HeapRegion::new(region_bytes(ch, RING_FRAMES)), ch, RING_FRAMES, rate).expect("valid ring shape");
        let (channels, pass_thru) = (spec.channels, spec.pass_thru);
        let apps = if pass_thru > 0 { spec.apps.len() } else { 0 };
        let mut virt_index = [None; spec::MAX_VIRTUALS];
        for (s, id) in slots.iter().enumerate() {
            if let Some(id) = id {
                virt_index[s] = spec.virtuals.iter().position(|v| &v.id == id).map(|i| i as u8);
            }
        }
        Self {
            channels,
            pass_thru,
            pass_thru_ring: (pass_thru > 0).then(|| ring(pass_thru)),
            output_ring: ring(channels),
            app_rings: (0..apps).map(|_| ring(pass_thru)).collect(),
            virt_ids: spec.virtuals.iter().map(|v| v.id.clone()).collect(),
            virt_channels: spec.virtuals.iter().map(|v| v.channels).collect(),
            virt_outputs: spec.virtuals.iter().map(|v| v.output).collect(),
            virt_rings: spec.virtuals.iter().map(|v| ring(v.channels)).collect(),
            virt_index,
        }
    }

    fn reset(&self, rate: f64) {
        if let Some(r) = &self.pass_thru_ring {
            r.reset(rate);
        }
        self.output_ring.reset(rate);
        for r in self.app_rings.iter().chain(&self.virt_rings) {
            r.reset(rate);
        }
    }

    /// Spec index of the virtual device in slot `s`.
    fn virt(&self, s: u8) -> Option<usize> {
        self.virt_index.get(usize::from(s)).copied().flatten().map(usize::from)
    }

    /// The twin's streams for virtual devices of one direction.
    fn twin_virts(&self, output: bool) -> impl Iterator<Item = (Role, u32)> + '_ {
        (0..self.virt_rings.len()).filter(move |&i| self.virt_outputs[i] == output).map(|i| (Role::TwinVirt(i as u8), self.virt_channels[i]))
    }

    /// The twin's input streams and their channel counts, in order: virtual
    /// inputs, virtual input devices, app slots.
    fn twin_inputs(&self) -> Vec<(Role, u32)> {
        let mut v = vec![];
        if self.pass_thru > 0 {
            v.push((Role::TwinIn, self.pass_thru));
        }
        v.extend(self.twin_virts(false));
        v.extend((0..self.app_rings.len()).map(|k| (Role::TwinApp(k as u8), self.pass_thru)));
        v
    }

    /// The twin's output streams: the busses, then the virtual output devices.
    fn twin_outputs(&self) -> Vec<(Role, u32)> {
        let mut v = vec![(Role::TwinOut, self.channels)];
        v.extend(self.twin_virts(true));
        v
    }

    /// Whether the twin or virtual-device stream for spec index i is an output
    /// of the twin (i.e. a virtual output).
    fn is_virtual_output(&self, i: usize) -> bool {
        self.virt_outputs.get(i).copied().unwrap_or(false)
    }
}

/// Assigns each virtual id a slot: kept ids keep theirs, new ids take a slot
/// that was free before (a slot just given up is not reused in the same
/// change, so the HAL never sees one object ID turn into another device).
fn assign_slots(old: &[Option<String>], virtuals: &[spec::VirtualSpec]) -> Vec<Option<String>> {
    let mut slots: Vec<Option<String>> = old.iter().map(|id| id.clone().filter(|id| virtuals.iter().any(|v| &v.id == id))).collect();
    for v in virtuals {
        if !slots.iter().any(|s| s.as_ref() == Some(&v.id)) {
            if let Some(free) = (0..slots.len()).find(|&i| slots[i].is_none() && old[i].is_none()) {
                slots[free] = Some(v.id.clone());
            }
        }
    }
    slots
}

/// One virtual device plus its engine twin.
pub struct Pair {
    base: AudioObjectID,
    uid: String,
    spec: Mutex<DeviceSpec>,
    /// Layout requested by the latest configuration.
    pending: ArcSwap<Layout>,
    /// Layout each device (see `Role::device_slot`) is currently doing IO
    /// with. A device switches only in its own configuration change, while
    /// the HAL has stopped its IO.
    active: Vec<ArcSwap<Layout>>,
    anchor_host: AtomicU64,
    rate_bits: AtomicU64,
    pending_rate_bits: AtomicU64,
    seed: AtomicU64,
    /// Running IO per device slot.
    io: Vec<AtomicU32>,
    input: Level,
    output: Level,
    /// Gain and mute of each virtual device, by slot.
    virt_levels: Vec<Level>,
    /// Virtual id in each slot. Control threads only.
    virt_slots: Mutex<Vec<Option<String>>>,
    /// Clients of the main device and their bundle IDs. Control threads only.
    clients: Mutex<HashMap<u32, String>>,
    /// Client ID → (app slot, exclusive), for the IO thread.
    client_slots: ArcSwap<HashMap<u32, (u8, bool)>>,
    /// Whether the output gain and mute scale what apps play in (see
    /// `DeviceSpec::apply_gain`).
    apply_gain: AtomicBool,
}

impl Pair {
    fn new(base: AudioObjectID, spec: DeviceSpec) -> Self {
        let slots = assign_slots(&vec![None; spec::MAX_VIRTUALS], &spec.virtuals);
        let layout = Arc::new(Layout::new(&spec, &slots, spec.sample_rate));
        Self {
            base,
            uid: spec.uid.clone(),
            pending: ArcSwap::from(Arc::clone(&layout)),
            active: (0..DEVICE_SLOTS).map(|_| ArcSwap::from(Arc::clone(&layout))).collect(),
            anchor_host: AtomicU64::new(0),
            rate_bits: AtomicU64::new(spec.sample_rate.to_bits()),
            pending_rate_bits: AtomicU64::new(spec.sample_rate.to_bits()),
            seed: AtomicU64::new(1),
            io: (0..DEVICE_SLOTS).map(|_| AtomicU32::new(0)).collect(),
            input: Level::new(),
            output: Level::new(),
            virt_levels: (0..spec::MAX_VIRTUALS).map(|_| Level::new()).collect(),
            virt_slots: Mutex::new(slots),
            clients: Mutex::new(HashMap::new()),
            client_slots: ArcSwap::from_pointee(HashMap::new()),
            apply_gain: AtomicBool::new(spec.apply_gain),
            spec: Mutex::new(spec),
        }
    }

    /// Gain for what apps play in: the output control's, unless the daemon
    /// applies it to particular outputs instead.
    fn play_gain(&self) -> f32 {
        if self.apply_gain.load(Ordering::Relaxed) {
            self.output.effective_gain()
        } else {
            1.0
        }
    }

    /// Recomputes which clients feed which app slot, after a client came or
    /// went or the app list changed.
    fn refresh_slots(&self) {
        let apps = self.spec().apps;
        let clients = self.clients.lock().unwrap_or_else(|e| e.into_inner());
        let slots = clients
            .iter()
            .filter_map(|(&id, bundle)| {
                let k = apps.iter().position(|a| a.bundle_ids.iter().any(|b| b == bundle))?;
                Some((id, (k as u8, apps[k].exclusive)))
            })
            .collect();
        self.client_slots.store(Arc::new(slots));
    }

    pub fn id(&self, role: Role) -> AudioObjectID {
        self.base + role.offset()
    }

    fn spec(&self) -> DeviceSpec {
        self.spec.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn rate(&self) -> f64 {
        f64::from_bits(self.rate_bits.load(Ordering::Acquire))
    }

    fn active(&self, role: Role) -> arc_swap::Guard<Arc<Layout>> {
        self.active[role.device_slot()].load()
    }

    /// Whether an object of this role currently exists.
    fn exists(&self, role: Role) -> bool {
        let l = self.active(role);
        match role {
            Role::MainOut | Role::VolumeOut | Role::MuteOut | Role::TwinIn => l.pass_thru > 0,
            Role::TwinApp(k) => usize::from(k) < l.app_rings.len(),
            Role::TwinVirt(i) => usize::from(i) < l.virt_rings.len(),
            Role::Virt(s) | Role::VirtStream(s) | Role::VirtGain(s) | Role::VirtMute(s) => l.virt(s).is_some(),
            _ => true,
        }
    }

    fn level(&self, role: Role) -> Option<&Level> {
        match role {
            Role::VolumeIn | Role::MuteIn => Some(&self.input),
            Role::VolumeOut | Role::MuteOut => Some(&self.output),
            Role::VirtGain(s) | Role::VirtMute(s) => self.virt_levels.get(usize::from(s)),
            _ => None,
        }
    }

    /// The id of the virtual device in slot `s`.
    fn virt_id(&self, s: u8) -> Option<String> {
        let l = self.active(Role::Virt(s));
        l.virt(s).map(|i| l.virt_ids[i].clone())
    }

    fn uid_of(&self, role: Role) -> String {
        match role {
            Role::Twin => format!("{}{TWIN_SUFFIX}", self.uid),
            Role::Virt(s) => format!("{}/{}", self.uid, self.virt_id(s).unwrap_or_default()),
            _ => self.uid.clone(),
        }
    }

    fn name_of(&self, role: Role) -> String {
        let spec = self.spec();
        match role {
            Role::Twin => format!("{} (LoopCraft engine)", spec.name),
            Role::Virt(s) => {
                let id = self.virt_id(s).unwrap_or_default();
                let name = spec.virtuals.iter().find(|v| v.id == id).map_or(id, |v| v.name.clone());
                format!("{}: {name}", spec.name)
            }
            _ => spec.name,
        }
    }

    /// The main device and its twin, which share streams and are related.
    fn pair_ids(&self) -> [AudioObjectID; 2] {
        [self.id(Role::Main), self.id(Role::Twin)]
    }

    /// Every device of the mixer: main, twin and the virtual devices.
    fn device_ids(&self) -> Vec<AudioObjectID> {
        let slots = self.virt_slots.lock().unwrap_or_else(|e| e.into_inner());
        let virts = slots.iter().enumerate().filter(|(_, id)| id.is_some()).map(|(s, _)| self.id(Role::Virt(s as u8)));
        self.pair_ids().into_iter().chain(virts).collect()
    }
}

struct Registry {
    pairs: Vec<Arc<Pair>>,
}

pub struct Driver {
    base: HostTimeBase,
    host: Option<Box<dyn Host>>,
    registry: ArcSwap<Registry>,
    next_base: AtomicU32,
    apply_lock: Mutex<()>,
    /// False until `initialize` has returned. The host reads the initial state
    /// itself; calling back into it before then is not allowed.
    initialized: AtomicBool,
}

impl Driver {
    pub fn new(base: HostTimeBase) -> Self {
        Self {
            base,
            host: None,
            registry: ArcSwap::from_pointee(Registry { pairs: vec![] }),
            next_base: AtomicU32::new(FIRST_BLOCK),
            apply_lock: Mutex::new(()),
            initialized: AtomicBool::new(false),
        }
    }

    pub fn set_host(&mut self, host: Box<dyn Host>) {
        self.host = Some(host);
    }

    /// The host, but only once it may be called back.
    fn live_host(&self) -> Option<&dyn Host> {
        if self.initialized.load(Ordering::Acquire) {
            self.host.as_deref()
        } else {
            None
        }
    }

    fn notify(&self, object: AudioObjectID, addresses: &[AudioObjectPropertyAddress]) {
        if let Some(h) = self.live_host() {
            h.properties_changed(object, addresses);
        }
    }

    /// Loads the persisted device list, or the default device.
    pub fn initialize(&self) {
        let stored = self.host.as_ref().and_then(|h| h.copy_from_storage(STORAGE_KEY));
        let specs = stored.and_then(|s| spec::parse(&s).ok()).unwrap_or_else(spec::default_specs);
        let _ = self.apply(specs, false);
        self.initialized.store(true, Ordering::Release);
    }

    pub fn specs(&self) -> Vec<DeviceSpec> {
        self.registry.load().pairs.iter().map(|p| p.spec()).collect()
    }

    /// Applies a new device list: adds, removes, renames and reshapes devices.
    pub fn apply(&self, specs: Vec<DeviceSpec>, persist: bool) -> Result<(), String> {
        let _guard = self.apply_lock.lock().unwrap_or_else(|e| e.into_inner());
        let old = self.registry.load_full();
        let mut pairs = Vec::with_capacity(specs.len());
        let mut list_changed = specs.len() != old.pairs.len();
        let g = kAudioObjectPropertyScopeGlobal;

        for s in &specs {
            match old.pairs.iter().find(|p| p.uid == s.uid) {
                Some(p) => {
                    let current = p.spec();
                    *p.spec.lock().unwrap_or_else(|e| e.into_inner()) = s.clone();
                    let names = |s: &DeviceSpec| (s.name.clone(), s.virtuals.iter().map(|v| (v.id.clone(), v.name.clone())).collect::<Vec<_>>());
                    if names(&current) != names(s) {
                        for id in p.device_ids() {
                            self.notify(id, &[addr(kAudioObjectPropertyName, g)]);
                        }
                    }
                    let old_slots = p.virt_slots.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    let slots = assign_slots(&old_slots, &s.virtuals);
                    // Devices doing IO switch layouts in their own configuration
                    // change; virtual devices that appear or go away have no IO.
                    let mut stable: Vec<AudioObjectID> = p.pair_ids().to_vec();
                    stable.extend((0..slots.len()).filter(|&i| slots[i].is_some() && slots[i] == old_slots[i]).map(|i| p.id(Role::Virt(i as u8))));
                    let mut reconfigure = false;
                    if current.layout_differs(s) {
                        let layout = Arc::new(Layout::new(s, &slots, p.rate()));
                        p.pending.store(Arc::clone(&layout));
                        for i in (0..slots.len()).filter(|&i| slots[i] != old_slots[i]) {
                            p.active[2 + i].store(Arc::clone(&layout));
                        }
                        reconfigure = true;
                    }
                    if slots != old_slots {
                        *p.virt_slots.lock().unwrap_or_else(|e| e.into_inner()) = slots;
                        list_changed = true;
                    }
                    if current.sample_rate != s.sample_rate {
                        p.pending_rate_bits.store(s.sample_rate.to_bits(), Ordering::Release);
                        reconfigure = true;
                    }
                    if reconfigure {
                        self.request_reconfigure(&stable);
                    }
                    if current.apps != s.apps {
                        p.refresh_slots();
                    }
                    p.apply_gain.store(s.apply_gain, Ordering::Relaxed);
                    pairs.push(Arc::clone(p));
                }
                None => {
                    let base = self.next_base.fetch_add(BLOCK, Ordering::AcqRel);
                    pairs.push(Arc::new(Pair::new(base, s.clone())));
                    list_changed = true;
                }
            }
        }
        list_changed |= old.pairs.iter().any(|p| !specs.iter().any(|s| s.uid == p.uid));
        self.registry.store(Arc::new(Registry { pairs }));

        if list_changed {
            self.notify(PLUGIN, &[addr(kAudioPlugInPropertyDeviceList, g), addr(kAudioObjectPropertyOwnedObjects, g)]);
        }
        if persist {
            if let Some(h) = self.live_host() {
                h.write_to_storage(STORAGE_KEY, &spec::to_json(&specs));
            }
        }
        Ok(())
    }

    fn request_reconfigure(&self, devices: &[AudioObjectID]) {
        match self.live_host() {
            Some(h) => {
                for &id in devices {
                    h.request_config_change(id, 0);
                }
            }
            // Without a host nobody is doing IO; apply directly.
            None => {
                for &id in devices {
                    let _ = self.perform_config_change(id);
                }
            }
        }
    }

    /// Finds the device and role an object ID belongs to.
    pub fn find(&self, object: AudioObjectID) -> Option<(Arc<Pair>, Role)> {
        let reg = self.registry.load();
        reg.pairs.iter().find_map(|p| {
            let off = object.checked_sub(p.base)?;
            let role = Role::from_offset(off).filter(|_| off < BLOCK)?;
            p.exists(role).then(|| (Arc::clone(p), role))
        })
    }

    fn device(&self, object: AudioObjectID) -> Result<(Arc<Pair>, Role), OSStatus> {
        match self.find(object) {
            Some((p, r)) if r.is_device() => Ok((p, r)),
            _ => Err(S_BAD_OBJECT),
        }
    }

    /// A client (an app's connection) started using a device. Only clients of
    /// the main device matter: their output can be captured per app.
    pub fn add_client(&self, device: AudioObjectID, client: u32, bundle_id: Option<String>) {
        if let Ok((p, Role::Main)) = self.device(device) {
            p.clients.lock().unwrap_or_else(|e| e.into_inner()).insert(client, bundle_id.unwrap_or_default());
            p.refresh_slots();
        }
    }

    pub fn remove_client(&self, device: AudioObjectID, client: u32) {
        if let Ok((p, Role::Main)) = self.device(device) {
            p.clients.lock().unwrap_or_else(|e| e.into_inner()).remove(&client);
            p.refresh_slots();
        }
    }

    pub fn is_device(&self, object: AudioObjectID) -> bool {
        self.device(object).is_ok()
    }

    fn class_of(&self, object: AudioObjectID) -> Option<(u32, u32)> {
        if object == PLUGIN {
            return Some((kAudioPlugInClassID, kAudioObjectClassID));
        }
        let (_, role) = self.find(object)?;
        Some(match role {
            r if r.is_device() => (kAudioDeviceClassID, kAudioObjectClassID),
            r if r.is_stream() => (kAudioStreamClassID, kAudioObjectClassID),
            Role::VolumeIn | Role::VolumeOut | Role::VirtGain(_) => (kAudioVolumeControlClassID, kAudioLevelControlClassID),
            Role::MuteIn | Role::MuteOut | Role::VirtMute(_) => (kAudioMuteControlClassID, kAudioBooleanControlClassID),
            _ => return None,
        })
    }

    fn owned_objects(&self, object: AudioObjectID) -> Vec<AudioObjectID> {
        if object == PLUGIN {
            return self.registry.load().pairs.iter().flat_map(|p| p.device_ids()).collect();
        }
        match self.find(object) {
            Some((p, Role::Main)) => {
                let mut v = Self::streams(&p, Role::Main, kAudioObjectPropertyScopeGlobal);
                v.extend(Self::controls(&p, Role::Main));
                v
            }
            Some((p, Role::Twin)) => Self::streams(&p, Role::Twin, kAudioObjectPropertyScopeGlobal),
            Some((p, role @ Role::Virt(_))) => {
                let mut v = Self::streams(&p, role, kAudioObjectPropertyScopeGlobal);
                v.extend(Self::controls(&p, role));
                v
            }
            _ => vec![],
        }
    }

    fn streams(p: &Pair, device: Role, scope: u32) -> Vec<AudioObjectID> {
        let l = p.active(device);
        let (ins, outs): (Vec<Role>, Vec<Role>) = match device {
            Role::Twin => (l.twin_inputs().into_iter().map(|s| s.0).collect(), l.twin_outputs().into_iter().map(|s| s.0).collect()),
            Role::Virt(s) => match l.virt(s) {
                Some(i) if l.is_virtual_output(i) => (vec![Role::VirtStream(s)], vec![]),
                Some(_) => (vec![], vec![Role::VirtStream(s)]),
                None => (vec![], vec![]),
            },
            _ => (vec![Role::MainIn], vec![Role::MainOut]),
        };
        let mut v = vec![];
        if scope != kAudioObjectPropertyScopeOutput {
            v.extend(ins.into_iter().filter(|r| p.exists(*r)).map(|r| p.id(r)));
        }
        if scope != kAudioObjectPropertyScopeInput {
            v.extend(outs.into_iter().filter(|r| p.exists(*r)).map(|r| p.id(r)));
        }
        v
    }

    fn controls(p: &Pair, device: Role) -> Vec<AudioObjectID> {
        let roles: Vec<Role> = match device {
            Role::Main => vec![Role::VolumeIn, Role::MuteIn, Role::VolumeOut, Role::MuteOut],
            Role::Virt(s) => vec![Role::VirtGain(s), Role::VirtMute(s)],
            _ => vec![],
        };
        roles.into_iter().filter(|r| p.exists(*r)).map(|r| p.id(r)).collect()
    }

    fn filter_by_class(&self, objects: Vec<AudioObjectID>, qualifier: &[u8]) -> Vec<AudioObjectID> {
        if qualifier.len() < 4 {
            return objects;
        }
        let classes: Vec<u32> = qualifier.as_chunks::<4>().0.iter().map(|c| u32::from_ne_bytes(*c)).collect();
        objects
            .into_iter()
            .filter(|o| match self.class_of(*o) {
                Some((c, b)) => classes.iter().any(|q| *q == c || *q == b || *q == kAudioObjectClassID),
                None => false,
            })
            .collect()
    }

    // ------------------------------------------------------------ properties

    /// Reads a property. `qualifier` is the raw qualifier data; `input` carries
    /// the incoming value for "convert" style properties.
    pub fn get(&self, object: AudioObjectID, a: &AudioObjectPropertyAddress, qualifier: &[u8], input: &[u8]) -> Result<Value, OSStatus> {
        let (class, base) = self.class_of(object).ok_or(S_BAD_OBJECT)?;
        match a.mSelector {
            kAudioObjectPropertyBaseClass => return Ok(Value::U32(base)),
            kAudioObjectPropertyClass => return Ok(Value::U32(class)),
            kAudioObjectPropertyOwnedObjects => return Ok(Value::Ids(self.filter_by_class(self.owned_objects(object), qualifier))),
            _ => {}
        }
        if object == PLUGIN {
            return self.get_plugin(a, qualifier);
        }
        let (p, role) = self.find(object).ok_or(S_BAD_OBJECT)?;
        match role {
            r if r.is_device() => self.get_device(&p, role, a),
            r if r.is_stream() => Self::get_stream(&p, role, a),
            _ => Self::get_control(&p, role, a, input),
        }
    }

    fn get_plugin(&self, a: &AudioObjectPropertyAddress, qualifier: &[u8]) -> Result<Value, OSStatus> {
        Ok(match a.mSelector {
            kAudioObjectPropertyOwner => Value::U32(kAudioObjectUnknown),
            kAudioObjectPropertyManufacturer => Value::Str(MANUFACTURER.into()),
            kAudioObjectPropertyName => Value::Str("LoopCraft".into()),
            kAudioPlugInPropertyBundleID => Value::Str(BUNDLE_ID.into()),
            kAudioPlugInPropertyDeviceList => Value::Ids(self.owned_objects(PLUGIN)),
            kAudioPlugInPropertyBoxList | kAudioPlugInPropertyClockDeviceList => Value::Ids(vec![]),
            kAudioPlugInPropertyResourceBundle => Value::Str(String::new()),
            kAudioPlugInPropertyTranslateUIDToDevice => {
                let uid = qualifier_string(qualifier).unwrap_or_default();
                let reg = self.registry.load();
                let found = reg.pairs.iter().find_map(|p| {
                    p.device_ids().into_iter().find(|&id| self.find(id).is_some_and(|(_, role)| p.uid_of(role) == uid))
                });
                Value::U32(found.unwrap_or(kAudioObjectUnknown))
            }
            kAudioObjectPropertyCustomPropertyInfoList => Value::CustomInfo(vec![[DEVICES_PROPERTY, PLIST_TYPE, 0]]),
            DEVICES_PROPERTY => Value::Str(spec::to_json(&self.specs())),
            _ => return Err(S_UNKNOWN_PROPERTY),
        })
    }

    fn get_device(&self, p: &Pair, role: Role, a: &AudioObjectPropertyAddress) -> Result<Value, OSStatus> {
        let twin = role == Role::Twin;
        let layout = p.active(role);
        let sum = |streams: Vec<(Role, u32)>| streams.iter().map(|s| s.1).sum::<u32>();
        let (in_ch, out_ch) = match role {
            Role::Twin => (sum(layout.twin_inputs()), sum(layout.twin_outputs())),
            Role::Virt(s) => match layout.virt(s) {
                Some(i) if layout.is_virtual_output(i) => (layout.virt_channels[i], 0),
                Some(i) => (0, layout.virt_channels[i]),
                None => (0, 0),
            },
            _ => (layout.channels, layout.pass_thru),
        };
        let scope_channels = match a.mScope {
            kAudioObjectPropertyScopeInput => in_ch,
            kAudioObjectPropertyScopeOutput => out_ch,
            _ => in_ch.max(out_ch),
        };
        let running = p.io[role.device_slot()].load(Ordering::Acquire) > 0;
        Ok(match a.mSelector {
            kAudioObjectPropertyOwner => Value::U32(PLUGIN),
            kAudioObjectPropertyName => Value::Str(p.name_of(role)),
            kAudioObjectPropertyManufacturer => Value::Str(MANUFACTURER.into()),
            kAudioObjectPropertyElementName => {
                if a.mElement == 0 || a.mElement > scope_channels {
                    return Err(S_UNKNOWN_PROPERTY);
                }
                Value::Str(if scope_channels == 2 {
                    if a.mElement == 1 { "Left".into() } else { "Right".into() }
                } else {
                    format!("Channel {}", a.mElement)
                })
            }
            kAudioDevicePropertyDeviceUID => Value::Str(p.uid_of(role)),
            kAudioDevicePropertyModelUID => Value::Str(MODEL_UID.into()),
            kAudioDevicePropertyTransportType => Value::U32(kAudioDeviceTransportTypeVirtual),
            kAudioDevicePropertyRelatedDevices => Value::Ids(if matches!(role, Role::Virt(_)) { vec![p.id(role)] } else { p.pair_ids().to_vec() }),
            // Device and twin share a clock; tell the HAL so.
            kAudioDevicePropertyClockDomain => Value::U32(p.base),
            kAudioDevicePropertyDeviceIsAlive => Value::U32(1),
            kAudioDevicePropertyDeviceIsRunning => Value::U32(running as u32),
            kAudioDevicePropertyDeviceCanBeDefaultDevice => Value::U32(!twin as u32),
            kAudioDevicePropertyDeviceCanBeDefaultSystemDevice => Value::U32(!twin as u32),
            kAudioDevicePropertyLatency | kAudioDevicePropertySafetyOffset => Value::U32(0),
            kAudioDevicePropertyStreams => Value::Ids(Self::streams(p, role, a.mScope)),
            kAudioObjectPropertyControlList => Value::Ids(Self::controls(p, role)),
            kAudioDevicePropertyNominalSampleRate => Value::F64(p.rate()),
            kAudioDevicePropertyAvailableNominalSampleRates => Value::Ranges(SAMPLE_RATES.iter().map(|&r| AudioValueRange { mMinimum: r, mMaximum: r }).collect()),
            kAudioDevicePropertyIsHidden => Value::U32(twin as u32),
            kAudioDevicePropertyClockIsStable => Value::U32(1),
            kAudioDevicePropertyZeroTimeStampPeriod => Value::U32(ZERO_TIMESTAMP_PERIOD),
            kAudioDevicePropertyPreferredChannelsForStereo => Value::U32Pair(if scope_channels >= 2 { [1, 2] } else { [1, 1] }),
            kAudioDevicePropertyPreferredChannelLayout if scope_channels == 2 => Value::Layout(stereo_layout()),
            _ => return Err(S_UNKNOWN_PROPERTY),
        })
    }

    fn get_stream(p: &Pair, role: Role, a: &AudioObjectPropertyAddress) -> Result<Value, OSStatus> {
        let layout = p.active(role);
        let virt = match role {
            Role::TwinVirt(i) => Some(usize::from(i)),
            Role::VirtStream(s) => layout.virt(s),
            _ => None,
        };
        let channels = match role {
            Role::MainIn | Role::TwinOut => layout.channels,
            Role::TwinVirt(_) | Role::VirtStream(_) => virt.map_or(0, |i| layout.virt_channels[i]),
            _ => layout.pass_thru,
        };
        let is_input = match role {
            Role::MainIn | Role::TwinIn | Role::TwinApp(_) => true,
            // The engine reads virtual inputs and writes virtual outputs; the
            // virtual device itself is the other way round.
            Role::TwinVirt(_) => !virt.is_some_and(|i| layout.is_virtual_output(i)),
            Role::VirtStream(_) => virt.is_some_and(|i| layout.is_virtual_output(i)),
            _ => false,
        };
        // Twin streams number their channels one after another.
        let starting_channel = match role.device() {
            Role::Twin => {
                let list = if is_input { layout.twin_inputs() } else { layout.twin_outputs() };
                1 + list.iter().take_while(|s| s.0 != role).map(|s| s.1).sum::<u32>()
            }
            _ => 1,
        };
        let rate = p.rate();
        Ok(match a.mSelector {
            kAudioObjectPropertyOwner => Value::U32(p.id(role.device())),
            kAudioObjectPropertyName => Value::Str(match role {
                Role::TwinApp(k) => format!("App {}", u32::from(k) + 1),
                Role::TwinVirt(i) => format!("Virtual {}", u32::from(i) + 1),
                _ if is_input => "Input".into(),
                _ => "Output".into(),
            }),
            kAudioStreamPropertyIsActive => Value::U32(1),
            kAudioStreamPropertyDirection => Value::U32(is_input as u32),
            kAudioStreamPropertyTerminalType => Value::U32(kAudioStreamTerminalTypeLine),
            kAudioStreamPropertyStartingChannel => Value::U32(starting_channel),
            kAudioStreamPropertyLatency => Value::U32(0),
            kAudioStreamPropertyVirtualFormat | kAudioStreamPropertyPhysicalFormat => Value::Format(format_for(rate, channels)),
            kAudioStreamPropertyAvailableVirtualFormats | kAudioStreamPropertyAvailablePhysicalFormats => Value::Formats(
                SAMPLE_RATES
                    .iter()
                    .map(|&r| AudioStreamRangedDescription { mFormat: format_for(r, channels), mSampleRateRange: AudioValueRange { mMinimum: r, mMaximum: r } })
                    .collect(),
            ),
            _ => return Err(S_UNKNOWN_PROPERTY),
        })
    }

    fn get_control(p: &Pair, role: Role, a: &AudioObjectPropertyAddress, input: &[u8]) -> Result<Value, OSStatus> {
        let level = p.level(role).ok_or(S_BAD_OBJECT)?;
        let is_volume = matches!(role, Role::VolumeIn | Role::VolumeOut | Role::VirtGain(_));
        let virtual_output = match role {
            Role::VirtGain(s) | Role::VirtMute(s) => {
                let l = p.active(role);
                l.virt(s).is_some_and(|i| l.is_virtual_output(i))
            }
            _ => false,
        };
        let scope = if matches!(role, Role::VolumeIn | Role::MuteIn) || virtual_output { kAudioObjectPropertyScopeInput } else { kAudioObjectPropertyScopeOutput };
        Ok(match (a.mSelector, is_volume) {
            (kAudioObjectPropertyOwner, _) => Value::U32(p.id(role.device())),
            (kAudioControlPropertyScope, _) => Value::U32(scope),
            (kAudioControlPropertyElement, _) => Value::U32(kAudioObjectPropertyElementMain),
            (kAudioLevelControlPropertyScalarValue, true) => Value::F32(level.scalar()),
            (kAudioLevelControlPropertyDecibelValue, true) => Value::F32(scalar_to_db(level.scalar())),
            (kAudioLevelControlPropertyDecibelRange, true) => Value::Range(AudioValueRange { mMinimum: MIN_DB as f64, mMaximum: MAX_DB as f64 }),
            (kAudioLevelControlPropertyConvertScalarToDecibels, true) => Value::F32(scalar_to_db(read_f32(input)?)),
            (kAudioLevelControlPropertyConvertDecibelsToScalar, true) => Value::F32(db_to_scalar(read_f32(input)?)),
            (kAudioBooleanControlPropertyValue, false) => Value::U32(level.muted.load(Ordering::Relaxed) as u32),
            _ => return Err(S_UNKNOWN_PROPERTY),
        })
    }

    pub fn has(&self, object: AudioObjectID, a: &AudioObjectPropertyAddress) -> bool {
        self.get(object, a, &[], &[0u8; 4]).is_ok()
    }

    pub fn is_settable(&self, object: AudioObjectID, a: &AudioObjectPropertyAddress) -> Result<bool, OSStatus> {
        if !self.has(object, a) {
            return Err(if self.class_of(object).is_some() { S_UNKNOWN_PROPERTY } else { S_BAD_OBJECT });
        }
        if object == PLUGIN {
            return Ok(a.mSelector == DEVICES_PROPERTY);
        }
        let (_, role) = self.find(object).ok_or(S_BAD_OBJECT)?;
        Ok(match role {
            r if r.is_device() => a.mSelector == kAudioDevicePropertyNominalSampleRate,
            r if r.is_stream() => matches!(a.mSelector, kAudioStreamPropertyVirtualFormat | kAudioStreamPropertyPhysicalFormat),
            Role::VolumeIn | Role::VolumeOut | Role::VirtGain(_) => matches!(a.mSelector, kAudioLevelControlPropertyScalarValue | kAudioLevelControlPropertyDecibelValue),
            Role::MuteIn | Role::MuteOut | Role::VirtMute(_) => a.mSelector == kAudioBooleanControlPropertyValue,
            _ => false,
        })
    }

    pub fn set(&self, object: AudioObjectID, a: &AudioObjectPropertyAddress, data: &[u8]) -> Result<(), OSStatus> {
        if !self.is_settable(object, a)? {
            return Err(status(kAudioHardwareUnsupportedOperationError));
        }
        if object == PLUGIN {
            let json = qualifier_string(data).ok_or(S_ILLEGAL)?;
            let specs = spec::parse(&json).map_err(|e| {
                crate::ffi::log(&e);
                S_ILLEGAL
            })?;
            return self.apply(specs, true).map_err(|_| S_ILLEGAL);
        }
        let (p, role) = self.find(object).ok_or(S_BAD_OBJECT)?;
        match role {
            r if r.is_device() => self.request_rate(&p, read_f64(data)?),
            r if r.is_stream() => {
                if data.len() < std::mem::size_of::<AudioStreamBasicDescription>() {
                    return Err(S_BAD_SIZE);
                }
                let f: AudioStreamBasicDescription = unsafe { std::ptr::read_unaligned(data.as_ptr() as *const _) };
                let current = match Self::get_stream(&p, role, &addr(kAudioStreamPropertyVirtualFormat, kAudioObjectPropertyScopeGlobal))? {
                    Value::Format(f) => f,
                    _ => return Err(S_ILLEGAL),
                };
                if f != format_for(f.mSampleRate, current.mChannelsPerFrame) {
                    return Err(S_UNSUPPORTED_FORMAT);
                }
                self.request_rate(&p, f.mSampleRate)
            }
            Role::VolumeIn | Role::VolumeOut | Role::VirtGain(_) => {
                let v = read_f32(data)?;
                let level = p.level(role).ok_or(S_BAD_OBJECT)?;
                level.set_scalar(if a.mSelector == kAudioLevelControlPropertyDecibelValue { db_to_scalar(v) } else { v });
                self.notify(object, &[
                    addr(kAudioLevelControlPropertyScalarValue, kAudioObjectPropertyScopeGlobal),
                    addr(kAudioLevelControlPropertyDecibelValue, kAudioObjectPropertyScopeGlobal),
                ]);
                Ok(())
            }
            Role::MuteIn | Role::MuteOut | Role::VirtMute(_) => {
                let v = read_u32(data)? != 0;
                p.level(role).ok_or(S_BAD_OBJECT)?.muted.store(v, Ordering::Relaxed);
                self.notify(object, &[addr(kAudioBooleanControlPropertyValue, kAudioObjectPropertyScopeGlobal)]);
                Ok(())
            }
            _ => Err(S_BAD_OBJECT),
        }
    }

    /// Rate changes go through the host's configuration-change protocol and
    /// always apply to the device and its twin together.
    fn request_rate(&self, p: &Pair, rate: f64) -> Result<(), OSStatus> {
        if !SAMPLE_RATES.contains(&rate) {
            return Err(S_UNSUPPORTED_FORMAT);
        }
        if rate == p.rate() {
            return Ok(());
        }
        p.pending_rate_bits.store(rate.to_bits(), Ordering::Release);
        p.spec.lock().unwrap_or_else(|e| e.into_inner()).sample_rate = rate;
        self.request_reconfigure(&p.device_ids());
        Ok(())
    }

    /// Applies pending layout and rate changes to one device. The HAL has
    /// stopped that device's IO.
    pub fn perform_config_change(&self, device: AudioObjectID) -> Result<(), OSStatus> {
        let (p, role) = self.device(device)?;
        let g = kAudioObjectPropertyScopeGlobal;
        let mut changed = vec![];

        let pending_rate = f64::from_bits(p.pending_rate_bits.load(Ordering::Acquire));
        if pending_rate != p.rate() {
            p.rate_bits.store(pending_rate.to_bits(), Ordering::Release);
            p.seed.fetch_add(1, Ordering::AcqRel);
            p.pending.load().reset(pending_rate);
            for d in p.device_ids() {
                self.notify(d, &[addr(kAudioDevicePropertyNominalSampleRate, g)]);
            }
        }

        let slot = &p.active[role.device_slot()];
        let pending = p.pending.load_full();
        if !Arc::ptr_eq(&slot.load_full(), &pending) {
            slot.store(pending);
            changed.extend([
                addr(kAudioDevicePropertyStreams, g),
                addr(kAudioDevicePropertyStreams, kAudioObjectPropertyScopeInput),
                addr(kAudioDevicePropertyStreams, kAudioObjectPropertyScopeOutput),
                addr(kAudioObjectPropertyOwnedObjects, g),
                addr(kAudioObjectPropertyControlList, g),
                addr(kAudioDevicePropertyPreferredChannelLayout, kAudioObjectPropertyScopeInput),
                addr(kAudioDevicePropertyPreferredChannelLayout, kAudioObjectPropertyScopeOutput),
            ]);
        }
        for s in Self::streams(&p, role, g) {
            self.notify(s, &[addr(kAudioStreamPropertyVirtualFormat, g), addr(kAudioStreamPropertyPhysicalFormat, g)]);
        }
        if !changed.is_empty() {
            self.notify(device, &changed);
        }
        Ok(())
    }

    // ------------------------------------------------------------------- IO

    pub fn start_io(&self, device: AudioObjectID, now: u64) -> Result<(), OSStatus> {
        let (p, role) = self.device(device)?;
        let me = role.device_slot();
        let first = p.io[me].fetch_add(1, Ordering::AcqRel) == 0;
        if first && p.io.iter().enumerate().all(|(i, c)| i == me || c.load(Ordering::Acquire) == 0) {
            // New timeline for the mixer: anchor the clock, forget old audio.
            p.anchor_host.store(now, Ordering::Release);
            p.seed.fetch_add(1, Ordering::AcqRel);
            let rate = p.rate();
            for slot in &p.active {
                slot.load().reset(rate);
            }
        }
        Ok(())
    }

    pub fn stop_io(&self, device: AudioObjectID) -> Result<(), OSStatus> {
        let (p, role) = self.device(device)?;
        p.io[role.device_slot()]
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1))
            .map(|_| ())
            .map_err(|_| status(kAudioHardwareNotRunningError))
    }

    pub fn zero_timestamp(&self, device: AudioObjectID, now: u64) -> Result<ZeroTimeStamp, OSStatus> {
        let (p, _) = self.device(device)?;
        let mut c = DeviceClock::new(self.base, p.rate(), ZERO_TIMESTAMP_PERIOD);
        c.anchor(p.anchor_host.load(Ordering::Acquire));
        let mut z = c.zero_timestamp(now);
        z.seed = p.seed.load(Ordering::Acquire);
        Ok(z)
    }

    pub fn will_do(operation: u32) -> (bool, bool) {
        match operation {
            kAudioServerPlugInIOOperationReadInput | kAudioServerPlugInIOOperationWriteMix | PROCESS_OUTPUT => (true, true),
            _ => (false, true),
        }
    }

    /// Real-time path: no allocation, no locks, no logging.
    ///
    /// # Safety
    /// `buffer` must point to `frames` frames of the stream's current format.
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn do_io(&self, device: AudioObjectID, stream: AudioObjectID, client: u32, operation: u32, frames: u32, cycle: &AudioServerPlugInIOCycleInfo, buffer: *mut f32) -> OSStatus {
        let Some((p, stream_role)) = self.find(stream) else { return S_BAD_OBJECT };
        if p.id(stream_role.device()) != device || buffer.is_null() {
            return S_BAD_OBJECT;
        }
        let layout = p.active(stream_role);
        let t_in = cycle.mInputTime.mSampleTime as i64;
        let t_out = cycle.mOutputTime.mSampleTime as i64;
        let slice = |ch: u32| std::slice::from_raw_parts_mut(buffer, (frames * ch) as usize);
        match (stream_role, operation) {
            // One client's output before mixing: copy it to its app's slot, and
            // for an exclusive app silence it so it stays out of the mix.
            (Role::MainOut, PROCESS_OUTPUT) => {
                if let Some(&(k, exclusive)) = p.client_slots.load().get(&client) {
                    if let Some(r) = layout.app_rings.get(usize::from(k)) {
                        let buf = slice(layout.pass_thru);
                        r.write_mix(t_out, buf, p.play_gain());
                        if exclusive {
                            buf.fill(0.0);
                        }
                    }
                }
            }
            (Role::TwinApp(k), kAudioServerPlugInIOOperationReadInput) => {
                if let Some(r) = layout.app_rings.get(usize::from(k)) {
                    r.read(t_in, slice(layout.pass_thru), 1.0, false);
                }
            }
            (Role::MainOut, kAudioServerPlugInIOOperationWriteMix) => {
                if let Some(r) = &layout.pass_thru_ring {
                    r.write(t_out, slice(layout.pass_thru), p.play_gain());
                }
            }
            (Role::MainIn, kAudioServerPlugInIOOperationReadInput) => {
                let buf = slice(layout.channels);
                let gain = p.input.effective_gain();
                if p.io[1].load(Ordering::Acquire) > 0 {
                    layout.output_ring.read(t_in, buf, gain, false);
                } else if let Some(r) = &layout.pass_thru_ring {
                    r.read_into(t_in, buf, layout.channels as usize, gain);
                } else {
                    buf.fill(0.0);
                }
            }
            (Role::TwinIn, kAudioServerPlugInIOOperationReadInput) => {
                if let Some(r) = &layout.pass_thru_ring {
                    r.read(t_in, slice(layout.pass_thru), 1.0, false);
                }
            }
            (Role::TwinOut, kAudioServerPlugInIOOperationWriteMix) => {
                layout.output_ring.write(t_out, slice(layout.channels), 1.0);
            }
            // A virtual input: apps play into it, the engine reads it.
            (Role::VirtStream(s), kAudioServerPlugInIOOperationWriteMix) => {
                if let Some(i) = layout.virt(s).filter(|&i| !layout.is_virtual_output(i)) {
                    let gain = p.virt_levels[usize::from(s)].effective_gain();
                    layout.virt_rings[i].write(t_out, slice(layout.virt_channels[i]), gain);
                }
            }
            (Role::TwinVirt(i), kAudioServerPlugInIOOperationReadInput) => {
                let i = usize::from(i);
                if let Some(r) = layout.virt_rings.get(i).filter(|_| !layout.is_virtual_output(i)) {
                    r.read(t_in, slice(layout.virt_channels[i]), 1.0, false);
                }
            }
            // A virtual output: the engine writes it, apps record it.
            (Role::TwinVirt(i), kAudioServerPlugInIOOperationWriteMix) => {
                let i = usize::from(i);
                if let Some(r) = layout.virt_rings.get(i).filter(|_| layout.is_virtual_output(i)) {
                    r.write(t_out, slice(layout.virt_channels[i]), 1.0);
                }
            }
            (Role::VirtStream(s), kAudioServerPlugInIOOperationReadInput) => {
                if let Some(i) = layout.virt(s).filter(|&i| layout.is_virtual_output(i)) {
                    let gain = p.virt_levels[usize::from(s)].effective_gain();
                    layout.virt_rings[i].read(t_in, slice(layout.virt_channels[i]), gain, false);
                }
            }
            _ => {}
        }
        0
    }
}

fn stereo_layout() -> AudioChannelLayoutStereo {
    let d = |label| AudioChannelDescription { mChannelLabel: label, mChannelFlags: 0, mCoordinates: [0.0; 3] };
    AudioChannelLayoutStereo {
        mChannelLayoutTag: kAudioChannelLayoutTag_UseChannelDescriptions,
        mChannelBitmap: 0,
        mNumberChannelDescriptions: 2,
        mChannelDescriptions: [d(kAudioChannelLabel_Left), d(kAudioChannelLabel_Right)],
    }
}

fn read_f32(data: &[u8]) -> Result<f32, OSStatus> {
    data.get(..4).map(|b| f32::from_ne_bytes([b[0], b[1], b[2], b[3]])).ok_or(S_BAD_SIZE)
}

fn read_u32(data: &[u8]) -> Result<u32, OSStatus> {
    data.get(..4).map(|b| u32::from_ne_bytes([b[0], b[1], b[2], b[3]])).ok_or(S_BAD_SIZE)
}

fn read_f64(data: &[u8]) -> Result<f64, OSStatus> {
    data.get(..8).map(|b| f64::from_ne_bytes(b.try_into().unwrap())).ok_or(S_BAD_SIZE)
}

/// Strings arrive as a CFStringRef passed by pointer value.
fn qualifier_string(qualifier: &[u8]) -> Option<String> {
    let n = std::mem::size_of::<usize>();
    let bytes = qualifier.get(..n)?;
    let p = usize::from_ne_bytes(bytes.try_into().ok()?) as *const std::ffi::c_void;
    // SAFETY: the HAL passes CFString/CFPropertyList references here.
    unsafe { string_from_cf(p) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ffi::{CFRelease, CFStringRef};

    const G: u32 = kAudioObjectPropertyScopeGlobal;

    #[derive(Default)]
    struct FakeHost {
        changed: Mutex<Vec<(AudioObjectID, u32)>>,
        requests: Mutex<Vec<AudioObjectID>>,
        storage: Mutex<std::collections::HashMap<String, String>>,
    }

    struct HostHandle(Arc<FakeHost>);

    impl Host for HostHandle {
        fn properties_changed(&self, object: AudioObjectID, addresses: &[AudioObjectPropertyAddress]) {
            let mut v = self.0.changed.lock().unwrap();
            for a in addresses {
                v.push((object, a.mSelector));
            }
        }
        fn request_config_change(&self, device: AudioObjectID, _action: u64) {
            self.0.requests.lock().unwrap().push(device);
        }
        fn copy_from_storage(&self, key: &str) -> Option<String> {
            self.0.storage.lock().unwrap().get(key).cloned()
        }
        fn write_to_storage(&self, key: &str, value: &str) {
            self.0.storage.lock().unwrap().insert(key.into(), value.into());
        }
    }

    fn driver() -> (Driver, Arc<FakeHost>) {
        let mut d = Driver::new(HostTimeBase::new(24_000_000.0));
        let host = Arc::new(FakeHost::default());
        d.set_host(Box::new(HostHandle(Arc::clone(&host))));
        d.initialize();
        (d, host)
    }

    /// Performs every requested configuration change, like the HAL would.
    fn perform_requests(d: &Driver, host: &FakeHost) {
        let reqs: Vec<_> = host.requests.lock().unwrap().drain(..).collect();
        for id in reqs {
            d.perform_config_change(id).unwrap();
        }
    }

    fn spec(uid: &str, name: &str, channels: u32, pass_thru: u32) -> DeviceSpec {
        DeviceSpec { uid: uid.into(), name: name.into(), channels, pass_thru, sample_rate: 48_000.0, apps: vec![], apply_gain: true, virtuals: vec![] }
    }

    fn get(d: &Driver, object: AudioObjectID, selector: u32) -> Value {
        d.get(object, &addr(selector, G), &[], &[]).unwrap()
    }

    fn main_id(d: &Driver, idx: usize) -> AudioObjectID {
        d.registry.load().pairs[idx].id(Role::Main)
    }

    fn pair(d: &Driver, idx: usize) -> Arc<Pair> {
        Arc::clone(&d.registry.load().pairs[idx])
    }

    fn cycle(input: f64, output: f64) -> AudioServerPlugInIOCycleInfo {
        let mut c = AudioServerPlugInIOCycleInfo::default();
        c.mInputTime.mSampleTime = input;
        c.mOutputTime.mSampleTime = output;
        c
    }

    fn io(d: &Driver, p: &Pair, role: Role, op: u32, frames: u32, c: &AudioServerPlugInIOCycleInfo, buf: &mut [f32]) {
        io_client(d, p, role, 0, op, frames, c, buf);
    }

    #[allow(clippy::too_many_arguments)]
    fn io_client(d: &Driver, p: &Pair, role: Role, client: u32, op: u32, frames: u32, c: &AudioServerPlugInIOCycleInfo, buf: &mut [f32]) {
        assert_eq!(unsafe { d.do_io(p.id(role.device()), p.id(role), client, op, frames, c, buf.as_mut_ptr()) }, 0);
    }

    #[test]
    fn default_device_and_twin() {
        let (d, _) = driver();
        let p = pair(&d, 0);
        assert_eq!(get(&d, PLUGIN, kAudioPlugInPropertyDeviceList), Value::Ids(vec![p.id(Role::Main), p.id(Role::Twin)]));
        assert_eq!(get(&d, p.id(Role::Main), kAudioObjectPropertyName), Value::Str("LoopCraft Rack".into()));
        assert_eq!(get(&d, p.id(Role::Main), kAudioDevicePropertyDeviceUID), Value::Str(spec::DEFAULT_UID.into()));
        assert_eq!(get(&d, p.id(Role::Twin), kAudioDevicePropertyIsHidden), Value::U32(1));
        assert_eq!(get(&d, p.id(Role::Twin), kAudioDevicePropertyDeviceCanBeDefaultDevice), Value::U32(0));
        assert_eq!(get(&d, p.id(Role::Main), kAudioDevicePropertyClockDomain), get(&d, p.id(Role::Twin), kAudioDevicePropertyClockDomain));
        assert_eq!(get(&d, p.id(Role::MainOut), kAudioObjectPropertyClass), Value::U32(kAudioStreamClassID));
        assert_eq!(get(&d, p.id(Role::VolumeOut), kAudioObjectPropertyBaseClass), Value::U32(kAudioLevelControlClassID));
        assert_eq!(get(&d, p.id(Role::Twin), kAudioObjectPropertyControlList), Value::Ids(vec![]));
    }

    #[test]
    fn translate_uid_finds_device_and_twin() {
        let (d, _) = driver();
        let p = pair(&d, 0);
        for (uid, want) in [(spec::DEFAULT_UID.to_string(), p.id(Role::Main)), (format!("{}{TWIN_SUFFIX}", spec::DEFAULT_UID), p.id(Role::Twin))] {
            let s = cfstring(&uid);
            let q = (s as usize).to_ne_bytes();
            assert_eq!(d.get(PLUGIN, &addr(kAudioPlugInPropertyTranslateUIDToDevice, G), &q, &[]), Ok(Value::U32(want)));
            unsafe { CFRelease(s) };
        }
    }

    #[test]
    fn devices_property_roundtrip_adds_renames_and_removes() {
        let (d, host) = driver();
        let json = spec::to_json(&[spec("u:desk", "Desktop", 10, 2), spec("u:fl", "FL Studio", 4, 4)]);
        let s = cfstring(&json);
        d.set(PLUGIN, &addr(DEVICES_PROPERTY, G), &(s as usize).to_ne_bytes()).unwrap();
        unsafe { CFRelease(s) };
        assert_eq!(d.specs().len(), 2);
        assert!(host.changed.lock().unwrap().contains(&(PLUGIN, kAudioPlugInPropertyDeviceList)));
        assert_eq!(host.storage.lock().unwrap().get(STORAGE_KEY).map(|s| spec::parse(s).unwrap().len()), Some(2));
        let desk = main_id(&d, 0);
        assert_eq!(get(&d, desk, kAudioObjectPropertyName), Value::Str("Desktop".into()));

        // Rename keeps the object ID.
        d.apply(vec![spec("u:desk", "Desk 2", 10, 2), spec("u:fl", "FL Studio", 4, 4)], true).unwrap();
        assert_eq!(main_id(&d, 0), desk);
        assert_eq!(get(&d, desk, kAudioObjectPropertyName), Value::Str("Desk 2".into()));

        // Removal drops the objects.
        d.apply(vec![spec("u:fl", "FL Studio", 4, 4)], true).unwrap();
        assert_eq!(d.get(desk, &addr(kAudioObjectPropertyName, G), &[], &[]), Err(S_BAD_OBJECT));
        match get(&d, PLUGIN, DEVICES_PROPERTY) {
            Value::Str(j) => assert_eq!(spec::parse(&j).unwrap()[0].name, "FL Studio"),
            v => panic!("{v:?}"),
        }
    }

    #[test]
    fn initialize_makes_no_host_callbacks() {
        let (_, host) = driver();
        assert!(host.changed.lock().unwrap().is_empty(), "no notifications during initialisation");
        assert!(host.requests.lock().unwrap().is_empty());
    }

    #[test]
    fn persisted_devices_are_restored_on_initialize() {
        let host = Arc::new(FakeHost::default());
        host.storage.lock().unwrap().insert(STORAGE_KEY.into(), spec::to_json(&[spec("u:a", "Alpha", 6, 0)]));
        let mut d = Driver::new(HostTimeBase::new(24_000_000.0));
        d.set_host(Box::new(HostHandle(Arc::clone(&host))));
        d.initialize();
        assert_eq!(d.specs()[0].name, "Alpha");
    }

    #[test]
    fn layout_shapes_streams_and_controls() {
        let (d, _) = driver();
        d.apply(vec![spec("u:rec", "Record only", 10, 0)], false).unwrap();
        let p = pair(&d, 0);
        let m = p.id(Role::Main);
        assert_eq!(get(&d, m, kAudioDevicePropertyStreams), Value::Ids(vec![p.id(Role::MainIn)]));
        assert_eq!(get(&d, m, kAudioObjectPropertyControlList), Value::Ids(vec![p.id(Role::VolumeIn), p.id(Role::MuteIn)]));
        assert_eq!(get(&d, p.id(Role::Twin), kAudioDevicePropertyStreams), Value::Ids(vec![p.id(Role::TwinOut)]));
        match get(&d, p.id(Role::MainIn), kAudioStreamPropertyVirtualFormat) {
            Value::Format(f) => assert_eq!(f.mChannelsPerFrame, 10),
            v => panic!("{v:?}"),
        }
        assert!(!d.has(p.id(Role::MainOut), &addr(kAudioObjectPropertyClass, G)));
        let ch3 = AudioObjectPropertyAddress { mSelector: kAudioObjectPropertyElementName, mScope: kAudioObjectPropertyScopeInput, mElement: 3 };
        assert_eq!(d.get(m, &ch3, &[], &[]), Ok(Value::Str("Channel 3".into())));
        let ch11 = AudioObjectPropertyAddress { mElement: 11, ..ch3 };
        assert!(d.get(m, &ch11, &[], &[]).is_err());
    }

    #[test]
    fn channel_count_changes_go_through_config_changes() {
        let (d, host) = driver();
        let p = pair(&d, 0);
        let m = p.id(Role::Main);
        d.apply(vec![spec(spec::DEFAULT_UID, "LoopCraft Rack", 8, 2)], true).unwrap();
        // Not visible until the HAL performs the change on each device.
        match get(&d, p.id(Role::MainIn), kAudioStreamPropertyVirtualFormat) {
            Value::Format(f) => assert_eq!(f.mChannelsPerFrame, 2),
            v => panic!("{v:?}"),
        }
        assert_eq!(*host.requests.lock().unwrap(), vec![m, p.id(Role::Twin)]);
        perform_requests(&d, &host);
        match get(&d, p.id(Role::MainIn), kAudioStreamPropertyVirtualFormat) {
            Value::Format(f) => assert_eq!(f.mChannelsPerFrame, 8),
            v => panic!("{v:?}"),
        }
        match get(&d, p.id(Role::TwinOut), kAudioStreamPropertyVirtualFormat) {
            Value::Format(f) => assert_eq!(f.mChannelsPerFrame, 8),
            v => panic!("{v:?}"),
        }
        assert!(host.changed.lock().unwrap().contains(&(m, kAudioDevicePropertyStreams)));
    }

    #[test]
    fn rate_changes_apply_to_device_and_twin() {
        let (d, host) = driver();
        let p = pair(&d, 0);
        d.set(p.id(Role::Main), &addr(kAudioDevicePropertyNominalSampleRate, G), &44_100f64.to_ne_bytes()).unwrap();
        assert_eq!(get(&d, p.id(Role::Twin), kAudioDevicePropertyNominalSampleRate), Value::F64(48_000.0));
        perform_requests(&d, &host);
        assert_eq!(get(&d, p.id(Role::Twin), kAudioDevicePropertyNominalSampleRate), Value::F64(44_100.0));
        assert_eq!(d.specs()[0].sample_rate, 44_100.0);
        assert_eq!(d.set(p.id(Role::Main), &addr(kAudioDevicePropertyNominalSampleRate, G), &12_345f64.to_ne_bytes()), Err(S_UNSUPPORTED_FORMAT));
    }

    #[test]
    fn without_engine_the_device_loops_back() {
        let (d, _) = driver();
        let p = pair(&d, 0);
        d.start_io(p.id(Role::Main), 1_000).unwrap();
        let played: Vec<f32> = (0..512 * 2).map(|i| i as f32 / 1000.0).collect();
        let mut out = played.clone();
        io(&d, &p, Role::MainOut, kAudioServerPlugInIOOperationWriteMix, 512, &cycle(0.0, 1024.0), &mut out);
        let mut rec = vec![0.0f32; 1024];
        io(&d, &p, Role::MainIn, kAudioServerPlugInIOOperationReadInput, 512, &cycle(1024.0, 2048.0), &mut rec);
        assert_eq!(rec, played);
    }

    #[test]
    fn engine_twin_carries_pass_thru_and_output_channels() {
        let (d, _) = driver();
        d.apply(vec![spec("u:d", "Desktop", 4, 2)], false).unwrap();
        let p = pair(&d, 0);
        d.start_io(p.id(Role::Main), 0).unwrap();
        d.start_io(p.id(Role::Twin), 0).unwrap();

        // An app plays stereo into the device...
        let mut app = vec![0.5f32; 64 * 2];
        io(&d, &p, Role::MainOut, kAudioServerPlugInIOOperationWriteMix, 64, &cycle(0.0, 100.0), &mut app);
        // ...the engine reads it as Pass-Thru...
        let mut pt = vec![0.0f32; 64 * 2];
        io(&d, &p, Role::TwinIn, kAudioServerPlugInIOOperationReadInput, 64, &cycle(100.0, 200.0), &mut pt);
        assert!(pt.iter().all(|&s| s == 0.5));
        // ...and writes 4 output channels (Pass-Thru L/R to 3/4 here).
        let mut mixed = vec![0.0f32; 64 * 4];
        for f in mixed.chunks_mut(4) {
            f[2] = 0.5;
            f[3] = 0.5;
        }
        io(&d, &p, Role::TwinOut, kAudioServerPlugInIOOperationWriteMix, 64, &cycle(100.0, 300.0), &mut mixed.clone());
        // Recording apps get the engine's output, not the raw Pass-Thru.
        let mut rec = vec![9.0f32; 64 * 4];
        io(&d, &p, Role::MainIn, kAudioServerPlugInIOOperationReadInput, 64, &cycle(300.0, 400.0), &mut rec);
        assert_eq!(rec, mixed);
    }

    #[test]
    fn fader_law() {
        let close = |a: f32, b: f32| (a - b).abs() < 0.01;
        assert_eq!(scalar_to_db(UNITY_SCALAR), 0.0);
        assert_eq!(scalar_to_db(1.0), MAX_DB);
        assert_eq!(scalar_to_db(0.0), MIN_DB);
        assert!(close(scalar_to_db(0.875), 6.0), "linear in dB above unity");
        assert!(close(scalar_to_db(0.375), -12.04), "half way to unity");
        assert!(close(scalar_to_db(0.5), -7.04));
        assert!(close(scalar_to_db(0.25), -19.08));
        assert_eq!(scalar_to_gain(0.0), 0.0, "the bottom is silence");
        assert_eq!(scalar_to_gain(UNITY_SCALAR), 1.0);
        assert!(close(scalar_to_gain(1.0), 3.981));
        for db in [-90.0, -40.0, -12.0, -3.0, 0.0, 3.0, 12.0] {
            assert!(close(scalar_to_db(db_to_scalar(db)), db), "{db} dB round-trips");
        }
        assert_eq!(db_to_scalar(-200.0), 0.0);
        assert_eq!(db_to_scalar(40.0), 1.0);
    }

    #[test]
    fn gain_and_mute_controls() {
        let (d, host) = driver();
        let p = pair(&d, 0);
        assert_eq!(get(&d, p.id(Role::VolumeOut), kAudioLevelControlPropertyDecibelValue), Value::F32(0.0), "starts at unity");
        assert_eq!(get(&d, p.id(Role::VolumeOut), kAudioLevelControlPropertyDecibelRange), Value::Range(AudioValueRange { mMinimum: -96.0, mMaximum: 12.0 }));
        d.set(p.id(Role::VolumeOut), &addr(kAudioLevelControlPropertyDecibelValue, G), &(-12f32).to_ne_bytes()).unwrap();
        match get(&d, p.id(Role::VolumeOut), kAudioLevelControlPropertyScalarValue) {
            Value::F32(s) => assert!((s - 0.3759).abs() < 0.001, "{s}"),
            v => panic!("{v:?}"),
        }
        assert!((p.output.effective_gain() - 0.2512).abs() < 0.001);
        assert!(host.changed.lock().unwrap().contains(&(p.id(Role::VolumeOut), kAudioLevelControlPropertyScalarValue)));
        d.set(p.id(Role::MuteIn), &addr(kAudioBooleanControlPropertyValue, G), &1u32.to_ne_bytes()).unwrap();
        assert_eq!(get(&d, p.id(Role::MuteIn), kAudioBooleanControlPropertyValue), Value::U32(1));
    }

    /// A device with Spotify (exclusive) and Chrome (shared) in app slots.
    fn app_device() -> (Driver, Arc<Pair>) {
        let (d, host) = driver();
        let mut s = spec("u:d", "Desktop", 6, 2);
        s.apps = vec![
            spec::AppSpec { bundle_ids: vec!["com.spotify.client".into()], exclusive: true },
            spec::AppSpec { bundle_ids: vec!["com.google.Chrome".into(), "com.google.Chrome.helper".into()], exclusive: false },
        ];
        d.apply(vec![s], false).unwrap();
        perform_requests(&d, &host);
        let p = pair(&d, 0);
        (d, p)
    }

    #[test]
    fn output_gain_can_be_left_to_the_daemon() {
        let (d, _) = driver();
        let p = pair(&d, 0);
        let out = p.id(Role::VolumeOut);
        d.set(out, &addr(kAudioLevelControlPropertyDecibelValue, G), &(-12f32).to_ne_bytes()).unwrap();
        let play = |at: f64| {
            let mut buf = vec![0.5f32; 64 * 2];
            io(&d, &p, Role::MainOut, kAudioServerPlugInIOOperationWriteMix, 64, &cycle(0.0, at), &mut buf);
            let mut rec = vec![0.0f32; 64 * 2];
            io(&d, &p, Role::MainIn, kAudioServerPlugInIOOperationReadInput, 64, &cycle(at, at + 100.0), &mut rec);
            rec[0]
        };
        d.start_io(p.id(Role::Main), 0).unwrap();
        assert!((play(100.0) - 0.5 * 0.2512).abs() < 0.001, "gain applies by default");
        let mut s = p.spec();
        s.apply_gain = false;
        d.apply(vec![s], false).unwrap();
        assert_eq!(play(300.0), 0.5, "left to the daemon: unity");
        d.set(p.id(Role::MuteOut), &addr(kAudioBooleanControlPropertyValue, G), &1u32.to_ne_bytes()).unwrap();
        assert_eq!(play(500.0), 0.5, "mute too");
        // The control itself still works and reports its value.
        assert_eq!(get(&d, out, kAudioLevelControlPropertyDecibelValue), Value::F32(-12.0));
    }

    fn virt(id: &str, name: &str, channels: u32, output: bool) -> spec::VirtualSpec {
        spec::VirtualSpec { id: id.into(), name: name.into(), channels, output }
    }

    /// A mixer with a virtual input "Comms" (2 ch) and a virtual output
    /// "Daw Output" (4 ch).
    fn virtual_device() -> (Driver, Arc<FakeHost>, Arc<Pair>) {
        let (d, host) = driver();
        let mut s = spec("u:d", "Desktop", 6, 2);
        s.virtuals = vec![virt("comms", "Comms", 2, false), virt("daw-out", "Daw Output", 4, true)];
        d.apply(vec![s], false).unwrap();
        perform_requests(&d, &host);
        let p = pair(&d, 0);
        (d, host, p)
    }

    fn ids(v: Value) -> Vec<AudioObjectID> {
        match v {
            Value::Ids(ids) => ids,
            v => panic!("{v:?}"),
        }
    }

    #[test]
    fn virtual_devices_are_devices_of_their_own() {
        let (d, _, p) = virtual_device();
        let (comms, daw) = (p.id(Role::Virt(0)), p.id(Role::Virt(1)));
        assert_eq!(ids(get(&d, PLUGIN, kAudioPlugInPropertyDeviceList)), vec![p.id(Role::Main), p.id(Role::Twin), comms, daw]);
        assert_eq!(get(&d, comms, kAudioObjectPropertyName), Value::Str("Desktop: Comms".into()));
        assert_eq!(get(&d, daw, kAudioDevicePropertyDeviceUID), Value::Str("u:d/daw-out".into()));
        assert_eq!(get(&d, comms, kAudioDevicePropertyClockDomain), get(&d, p.id(Role::Main), kAudioDevicePropertyClockDomain), "one clock");
        assert_eq!(get(&d, comms, kAudioDevicePropertyIsHidden), Value::U32(0));
        // Comms is playback-only, Daw Output record-only.
        let scoped = |dev, scope| ids(d.get(dev, &addr(kAudioDevicePropertyStreams, scope), &[], &[]).unwrap());
        assert_eq!((scoped(comms, kAudioObjectPropertyScopeInput), scoped(comms, kAudioObjectPropertyScopeOutput)), (vec![], vec![p.id(Role::VirtStream(0))]));
        assert_eq!((scoped(daw, kAudioObjectPropertyScopeInput), scoped(daw, kAudioObjectPropertyScopeOutput)), (vec![p.id(Role::VirtStream(1))], vec![]));
        assert_eq!(get(&d, p.id(Role::VirtStream(1)), kAudioStreamPropertyDirection), Value::U32(1));
        // Each has its own gain and mute.
        assert_eq!(ids(get(&d, comms, kAudioObjectPropertyControlList)), vec![p.id(Role::VirtGain(0)), p.id(Role::VirtMute(0))]);
        assert_eq!(get(&d, p.id(Role::VirtGain(1)), kAudioControlPropertyScope), Value::U32(kAudioObjectPropertyScopeInput));
        // The engine twin: reads Comms after the virtual inputs, writes Daw
        // Output after the busses.
        let twin = p.id(Role::Twin);
        assert_eq!(scoped(twin, kAudioObjectPropertyScopeInput), vec![p.id(Role::TwinIn), p.id(Role::TwinVirt(0))]);
        assert_eq!(scoped(twin, kAudioObjectPropertyScopeOutput), vec![p.id(Role::TwinOut), p.id(Role::TwinVirt(1))]);
        assert_eq!(get(&d, p.id(Role::TwinVirt(0)), kAudioStreamPropertyStartingChannel), Value::U32(3));
        assert_eq!(get(&d, p.id(Role::TwinVirt(1)), kAudioStreamPropertyStartingChannel), Value::U32(7));
    }

    #[test]
    fn virtual_devices_carry_audio_on_the_mixer_clock() {
        let (d, _, p) = virtual_device();
        let (comms, daw, twin) = (p.id(Role::Virt(0)), p.id(Role::Virt(1)), p.id(Role::Twin));
        for dev in [comms, daw, twin] {
            d.start_io(dev, 0).unwrap();
        }
        // Discord plays into Comms; the engine reads it.
        io(&d, &p, Role::VirtStream(0), kAudioServerPlugInIOOperationWriteMix, 64, &cycle(0.0, 100.0), &mut vec![0.5; 128]);
        let mut got = vec![9.0f32; 128];
        io(&d, &p, Role::TwinVirt(0), kAudioServerPlugInIOOperationReadInput, 64, &cycle(100.0, 200.0), &mut got);
        assert!(got.iter().all(|&s| s == 0.5));
        // The engine writes Daw Output; the DAW records it.
        io(&d, &p, Role::TwinVirt(1), kAudioServerPlugInIOOperationWriteMix, 64, &cycle(100.0, 300.0), &mut vec![0.25; 256]);
        let mut rec = vec![9.0f32; 256];
        io(&d, &p, Role::VirtStream(1), kAudioServerPlugInIOOperationReadInput, 64, &cycle(300.0, 400.0), &mut rec);
        assert!(rec.iter().all(|&s| s == 0.25));
        // A virtual device's own gain applies to what passes through it.
        d.set(p.id(Role::VirtGain(0)), &addr(kAudioLevelControlPropertyDecibelValue, G), &(-6.0206f32).to_ne_bytes()).unwrap();
        io(&d, &p, Role::VirtStream(0), kAudioServerPlugInIOOperationWriteMix, 64, &cycle(64.0, 164.0), &mut vec![0.5; 128]);
        io(&d, &p, Role::TwinVirt(0), kAudioServerPlugInIOOperationReadInput, 64, &cycle(164.0, 264.0), &mut got);
        assert!((got[0] - 0.25).abs() < 1e-4, "{}", got[0]);
        // Starting a virtual device while others run keeps the shared timeline.
        let z1 = d.zero_timestamp(comms, 1_000_000).unwrap();
        let z2 = d.zero_timestamp(p.id(Role::Main), 1_000_000).unwrap();
        assert_eq!(z1, z2);
    }

    #[test]
    fn virtual_devices_keep_their_identity() {
        let (d, host, p) = virtual_device();
        let (comms, daw) = (p.id(Role::Virt(0)), p.id(Role::Virt(1)));
        // Remove Comms, add Talk, reorder: Daw Output keeps its object, Talk
        // does not take the slot Comms just gave up.
        let mut s = p.spec();
        s.virtuals = vec![virt("talk", "Talk", 2, false), virt("daw-out", "Daw Output", 8, true)];
        d.apply(vec![s], false).unwrap();
        perform_requests(&d, &host);
        assert_eq!(get(&d, daw, kAudioDevicePropertyDeviceUID), Value::Str("u:d/daw-out".into()));
        assert!(d.find(comms).is_none(), "Comms is gone");
        let talk = p.id(Role::Virt(2));
        assert_eq!(get(&d, talk, kAudioObjectPropertyName), Value::Str("Desktop: Talk".into()));
        assert_eq!(ids(get(&d, PLUGIN, kAudioPlugInPropertyDeviceList)), vec![p.id(Role::Main), p.id(Role::Twin), daw, talk]);
        match get(&d, p.id(Role::VirtStream(1)), kAudioStreamPropertyVirtualFormat) {
            Value::Format(f) => assert_eq!(f.mChannelsPerFrame, 8, "reshaped in place"),
            v => panic!("{v:?}"),
        }
        assert!(host.changed.lock().unwrap().iter().any(|(o, s)| *o == PLUGIN && *s == kAudioPlugInPropertyDeviceList));
        // A later change may reuse the freed slot.
        let mut s = p.spec();
        s.virtuals.push(virt("fx", "FX", 2, false));
        d.apply(vec![s], false).unwrap();
        assert_eq!(get(&d, p.id(Role::Virt(0)), kAudioDevicePropertyDeviceUID), Value::Str("u:d/fx".into()));
    }

    #[test]
    fn slot_assignment() {
        let none = vec![None; 4];
        let v = |ids: &[&str]| ids.iter().map(|i| virt(i, i, 2, false)).collect::<Vec<_>>();
        let s1 = assign_slots(&none, &v(&["a", "b"]));
        assert_eq!(s1, [Some("a".into()), Some("b".into()), None, None]);
        let s2 = assign_slots(&s1, &v(&["c", "b"]));
        assert_eq!(s2, [None, Some("b".into()), Some("c".into()), None], "a's slot is not reused at once");
        assert_eq!(assign_slots(&s2, &v(&["c", "b", "d"])), [Some("d".into()), Some("b".into()), Some("c".into()), None]);
    }

    #[test]
    fn app_slots_are_twin_input_streams() {
        let (d, p) = app_device();
        let twin = p.id(Role::Twin);
        let ins = d.get(twin, &addr(kAudioDevicePropertyStreams, kAudioObjectPropertyScopeInput), &[], &[]).unwrap();
        assert_eq!(ins, Value::Ids(vec![p.id(Role::TwinIn), p.id(Role::TwinApp(0)), p.id(Role::TwinApp(1))]));
        assert_eq!(get(&d, p.id(Role::TwinApp(1)), kAudioStreamPropertyStartingChannel), Value::U32(5));
        assert_eq!(get(&d, p.id(Role::TwinApp(0)), kAudioStreamPropertyDirection), Value::U32(1));
        assert!(d.find(p.id(Role::TwinApp(2))).is_none());
        // The main device is unchanged: 6 busses in, 2 virtual inputs out.
        let main_ins = d.get(p.id(Role::Main), &addr(kAudioDevicePropertyStreams, kAudioObjectPropertyScopeInput), &[], &[]).unwrap();
        assert_eq!(main_ins, Value::Ids(vec![p.id(Role::MainIn)]));
    }

    #[test]
    fn apps_are_captured_per_client() {
        let (d, p) = app_device();
        let main = p.id(Role::Main);
        d.add_client(main, 7, Some("com.spotify.client".into()));
        d.add_client(main, 8, Some("com.google.Chrome.helper".into()));
        d.add_client(main, 9, Some("com.apple.Music".into()));
        d.add_client(main, 10, None);
        d.start_io(main, 0).unwrap();
        d.start_io(p.id(Role::Twin), 0).unwrap();
        let c = cycle(0.0, 100.0);
        let play = |client: u32, v: f32| {
            let mut buf = vec![v; 64 * 2];
            io_client(&d, &p, Role::MainOut, client, PROCESS_OUTPUT, 64, &c, &mut buf);
            buf[0]
        };
        assert_eq!(play(7, 0.5), 0.0, "exclusive: left out of the mix");
        assert_eq!(play(8, 0.25), 0.25, "shared: stays in the mix");
        assert_eq!(play(9, 0.125), 0.125, "not an app slot");
        assert_eq!(play(10, 0.0625), 0.0625, "no bundle ID");
        // The HAL then mixes what is left and writes it.
        let mut mix = vec![0.25 + 0.125 + 0.0625; 64 * 2];
        io(&d, &p, Role::MainOut, kAudioServerPlugInIOOperationWriteMix, 64, &c, &mut mix);

        let read = |role: Role| {
            let mut buf = vec![9.0f32; 64 * 2];
            io(&d, &p, role, kAudioServerPlugInIOOperationReadInput, 64, &cycle(100.0, 200.0), &mut buf);
            buf
        };
        assert!(read(Role::TwinIn).iter().all(|&s| s == 0.4375), "virtual inputs without Spotify");
        assert!(read(Role::TwinApp(0)).iter().all(|&s| s == 0.5), "Spotify");
        assert!(read(Role::TwinApp(1)).iter().all(|&s| s == 0.25), "Chrome");
    }

    #[test]
    fn helpers_of_one_app_sum_into_its_slot() {
        let (d, p) = app_device();
        let main = p.id(Role::Main);
        d.add_client(main, 1, Some("com.google.Chrome".into()));
        d.add_client(main, 2, Some("com.google.Chrome.helper".into()));
        d.start_io(main, 0).unwrap();
        d.start_io(p.id(Role::Twin), 0).unwrap();
        for client in [1, 2] {
            io_client(&d, &p, Role::MainOut, client, PROCESS_OUTPUT, 64, &cycle(0.0, 100.0), &mut vec![0.25; 128]);
        }
        // Next cycle: only the helper plays; the slot must not keep old audio.
        io_client(&d, &p, Role::MainOut, 2, PROCESS_OUTPUT, 64, &cycle(64.0, 164.0), &mut vec![0.125; 128]);
        let mut buf = vec![9.0f32; 128 * 2];
        io(&d, &p, Role::TwinApp(1), kAudioServerPlugInIOOperationReadInput, 128, &cycle(100.0, 300.0), &mut buf);
        assert!(buf[..128].iter().all(|&s| s == 0.5), "two clients summed");
        assert!(buf[128..].iter().all(|&s| s == 0.125), "then one");
    }

    #[test]
    fn clients_and_app_lists_can_change() {
        let (d, p) = app_device();
        let main = p.id(Role::Main);
        d.add_client(main, 7, Some("com.spotify.client".into()));
        assert_eq!(p.client_slots.load().get(&7), Some(&(0, true)));
        d.remove_client(main, 7);
        assert!(p.client_slots.load().is_empty());

        // Same number of apps, different bundle IDs: remapped without new streams.
        d.add_client(main, 7, Some("com.spotify.client".into()));
        d.add_client(main, 8, Some("com.apple.Music".into()));
        let mut s = p.spec();
        s.apps[0] = spec::AppSpec { bundle_ids: vec!["com.apple.Music".into()], exclusive: false };
        d.apply(vec![s], false).unwrap();
        assert_eq!(p.client_slots.load().get(&8), Some(&(0, false)));
        assert_eq!(p.client_slots.load().get(&7), None);
        assert!(Arc::ptr_eq(&p.pending.load_full(), &p.active[0].load_full()), "no new layout");
        // Clients of the twin (the daemon) are not tracked.
        d.add_client(p.id(Role::Twin), 99, Some("com.spotify.client".into()));
        assert_eq!(p.client_slots.load().get(&99), None);
    }

    #[test]
    fn restart_forgets_old_audio_and_io_counts_are_checked() {
        let (d, _) = driver();
        let p = pair(&d, 0);
        d.start_io(p.id(Role::Main), 0).unwrap();
        let mut buf = vec![1.0f32; 64];
        io(&d, &p, Role::MainOut, kAudioServerPlugInIOOperationWriteMix, 32, &cycle(0.0, 0.0), &mut buf);
        d.stop_io(p.id(Role::Main)).unwrap();
        d.start_io(p.id(Role::Main), 5_000).unwrap();
        let mut rec = vec![9.0f32; 64];
        io(&d, &p, Role::MainIn, kAudioServerPlugInIOOperationReadInput, 32, &cycle(0.0, 0.0), &mut rec);
        assert!(rec.iter().all(|&s| s == 0.0));
        d.stop_io(p.id(Role::Main)).unwrap();
        assert_eq!(d.stop_io(p.id(Role::Main)), Err(status(kAudioHardwareNotRunningError)));
        assert_eq!(d.start_io(p.id(Role::MainIn), 0), Err(S_BAD_OBJECT), "streams are not devices");
    }

    #[test]
    fn zero_timestamps_follow_the_shared_anchor() {
        let (d, _) = driver();
        let p = pair(&d, 0);
        d.start_io(p.id(Role::Twin), 1_000_000).unwrap();
        let a = d.zero_timestamp(p.id(Role::Main), 1_000_000 + 24_000_000).unwrap();
        let b = d.zero_timestamp(p.id(Role::Twin), 1_000_000 + 24_000_000).unwrap();
        assert_eq!(a, b, "device and twin share one clock");
        assert_eq!(a.sample_time, (48_000 / 8192 * 8192) as f64);
    }

    #[test]
    fn strings_come_back_as_cfstrings() {
        let (d, _) = driver();
        let v = get(&d, main_id(&d, 0), kAudioDevicePropertyDeviceUID);
        let mut buf = [0u8; 8];
        assert_eq!(v.write(&mut buf), Ok(8));
        let p = usize::from_ne_bytes(buf) as CFStringRef;
        assert_eq!(unsafe { string_from_cf(p) }.as_deref(), Some(spec::DEFAULT_UID));
        unsafe { CFRelease(p) };
    }
}
