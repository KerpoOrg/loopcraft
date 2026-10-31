//! Minimal Core Audio HAL client: property access, devices, processes and an
//! input meter. Public API only.

#![allow(non_snake_case)]

mod aggregate;
mod io;
mod tap;

pub use aggregate::{create_aggregate, destroy_aggregate, use_hal_notification_thread, SubDevice};
pub use io::{Buffers, BuffersMut, IoProc, Render, StreamUsage};
pub use tap::{bundle_identifier, create_tap, Tap, TapSpec};

use lc_sys::consts::*;
use std::ffi::c_void;
use std::fmt;

pub type AudioObjectID = u32;
type OSStatus = i32;

#[repr(C)]
#[derive(Clone, Copy)]
struct Address {
    selector: u32,
    scope: u32,
    element: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct StreamFormat {
    pub sample_rate: f64,
    pub format_id: u32,
    pub format_flags: u32,
    pub bytes_per_packet: u32,
    pub frames_per_packet: u32,
    pub bytes_per_frame: u32,
    pub channels_per_frame: u32,
    pub bits_per_channel: u32,
    pub reserved: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct ValueRange {
    pub min: f64,
    pub max: f64,
}

#[repr(C)]
struct AudioBuffer {
    channels: u32,
    byte_size: u32,
    data: *mut c_void,
}

#[repr(C)]
struct AudioBufferList {
    count: u32,
    buffers: [AudioBuffer; 1],
}

type IOProc = unsafe extern "C" fn(
    AudioObjectID,
    *const c_void,
    *const AudioBufferList,
    *const c_void,
    *mut AudioBufferList,
    *const c_void,
    *mut c_void,
) -> OSStatus;
type IOProcID = *mut c_void;

#[link(name = "CoreAudio", kind = "framework")]
extern "C" {
    fn AudioObjectGetPropertyDataSize(id: AudioObjectID, a: *const Address, qs: u32, q: *const c_void, size: *mut u32) -> OSStatus;
    fn AudioObjectGetPropertyData(id: AudioObjectID, a: *const Address, qs: u32, q: *const c_void, size: *mut u32, out: *mut c_void) -> OSStatus;
    fn AudioObjectSetPropertyData(id: AudioObjectID, a: *const Address, qs: u32, q: *const c_void, size: u32, data: *const c_void) -> OSStatus;
    fn AudioDeviceCreateIOProcID(dev: AudioObjectID, proc_: IOProc, client: *mut c_void, id: *mut IOProcID) -> OSStatus;
    fn AudioDeviceDestroyIOProcID(dev: AudioObjectID, id: IOProcID) -> OSStatus;
    fn AudioDeviceStart(dev: AudioObjectID, id: IOProcID) -> OSStatus;
    fn AudioDeviceStop(dev: AudioObjectID, id: IOProcID) -> OSStatus;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFStringGetCString(s: *const c_void, buf: *mut u8, size: isize, enc: u32) -> u8;
    fn CFStringCreateWithBytes(alloc: *const c_void, bytes: *const u8, len: isize, enc: u32, ext: u8) -> *const c_void;
    fn CFRelease(cf: *const c_void);
}

const UTF8: u32 = 0x0800_0100;

/// Four-character code if printable, otherwise hex.
pub fn fourcc(v: u32) -> String {
    let b = v.to_be_bytes();
    if b.iter().all(|c| (0x20..0x7f).contains(c)) {
        format!("'{}'", String::from_utf8_lossy(&b))
    } else {
        format!("{v:#010x}")
    }
}

#[derive(Debug)]
pub struct Error {
    pub what: String,
    pub status: OSStatus,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} failed (status {} {})", self.what, self.status, fourcc(self.status as u32))
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

fn check(status: OSStatus, what: impl FnOnce() -> String) -> Result<()> {
    if status == 0 {
        Ok(())
    } else {
        Err(Error { what: what(), status })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Global,
    Input,
    Output,
}

impl Scope {
    fn raw(self) -> u32 {
        match self {
            Scope::Global => kAudioObjectPropertyScopeGlobal,
            Scope::Input => kAudioObjectPropertyScopeInput,
            Scope::Output => kAudioObjectPropertyScopeOutput,
        }
    }
}

fn address(selector: u32, scope: Scope) -> Address {
    Address { selector, scope: scope.raw(), element: kAudioObjectPropertyElementMain }
}

fn get_raw<T: Copy + Default>(id: AudioObjectID, selector: u32, scope: Scope, qualifier: Option<&[u8]>) -> Result<T> {
    let a = address(selector, scope);
    let mut v = T::default();
    let mut size = std::mem::size_of::<T>() as u32;
    let (qs, q) = qualifier.map_or((0, std::ptr::null()), |q| (q.len() as u32, q.as_ptr() as *const c_void));
    let st = unsafe { AudioObjectGetPropertyData(id, &a, qs, q, &mut size, &mut v as *mut T as *mut c_void) };
    check(st, || format!("reading {} of object {id}", fourcc(selector)))?;
    Ok(v)
}

fn set_raw<T: Copy>(id: AudioObjectID, selector: u32, scope: Scope, v: T) -> Result<()> {
    let a = address(selector, scope);
    let st = unsafe { AudioObjectSetPropertyData(id, &a, 0, std::ptr::null(), std::mem::size_of::<T>() as u32, &v as *const T as *const c_void) };
    check(st, || format!("setting {} of object {id}", fourcc(selector)))
}

pub fn get_u32(id: AudioObjectID, selector: u32, scope: Scope) -> Result<u32> {
    get_raw(id, selector, scope, None)
}

pub fn get_f32(id: AudioObjectID, selector: u32, scope: Scope) -> Result<f32> {
    get_raw(id, selector, scope, None)
}

pub fn get_f64(id: AudioObjectID, selector: u32, scope: Scope) -> Result<f64> {
    get_raw(id, selector, scope, None)
}

pub fn set_u32(id: AudioObjectID, selector: u32, scope: Scope, v: u32) -> Result<()> {
    set_raw(id, selector, scope, v)
}

pub fn set_f32(id: AudioObjectID, selector: u32, scope: Scope, v: f32) -> Result<()> {
    set_raw(id, selector, scope, v)
}

pub fn set_f64(id: AudioObjectID, selector: u32, scope: Scope, v: f64) -> Result<()> {
    set_raw(id, selector, scope, v)
}

fn get_array<T: Copy + Default>(id: AudioObjectID, selector: u32, scope: Scope) -> Result<Vec<T>> {
    let a = address(selector, scope);
    let mut size = 0u32;
    let st = unsafe { AudioObjectGetPropertyDataSize(id, &a, 0, std::ptr::null(), &mut size) };
    check(st, || format!("sizing {} of object {id}", fourcc(selector)))?;
    let mut v = vec![T::default(); size as usize / std::mem::size_of::<T>()];
    let st = unsafe { AudioObjectGetPropertyData(id, &a, 0, std::ptr::null(), &mut size, v.as_mut_ptr() as *mut c_void) };
    check(st, || format!("reading {} of object {id}", fourcc(selector)))?;
    v.truncate(size as usize / std::mem::size_of::<T>());
    Ok(v)
}

pub fn get_ids(id: AudioObjectID, selector: u32, scope: Scope) -> Result<Vec<AudioObjectID>> {
    get_array(id, selector, scope)
}

pub fn get_ranges(id: AudioObjectID, selector: u32, scope: Scope) -> Result<Vec<ValueRange>> {
    get_array(id, selector, scope)
}

pub fn get_string(id: AudioObjectID, selector: u32, scope: Scope) -> Result<String> {
    let p: usize = get_raw(id, selector, scope, None)?;
    Ok(take_cfstring(p as *const c_void))
}

/// The name the device gives each channel in a direction (empty when it gives none).
pub fn channel_names(id: AudioObjectID, scope: Scope) -> Vec<String> {
    (1..=channel_count(id, scope))
        .map(|ch| {
            let a = Address { selector: kAudioObjectPropertyElementName, scope: scope.raw(), element: ch };
            let mut p: usize = 0;
            let mut size = std::mem::size_of::<usize>() as u32;
            let st = unsafe { AudioObjectGetPropertyData(id, &a, 0, std::ptr::null(), &mut size, &mut p as *mut usize as *mut c_void) };
            if st == 0 && p != 0 { take_cfstring(p as *const c_void) } else { String::new() }
        })
        .collect()
}

/// Size coreaudiod reports for a property.
pub fn data_size(id: AudioObjectID, selector: u32, scope: Scope) -> Result<u32> {
    let a = address(selector, scope);
    let mut size = 0u32;
    let st = unsafe { AudioObjectGetPropertyDataSize(id, &a, 0, std::ptr::null(), &mut size) };
    check(st, || format!("sizing {} of object {id}", fourcc(selector)))?;
    Ok(size)
}

/// Custom properties an object declares: (selector, data type, qualifier type).
pub fn custom_properties(id: AudioObjectID) -> Vec<[u32; 3]> {
    get_array::<[u32; 3]>(id, kAudioObjectPropertyCustomPropertyInfoList, Scope::Global).unwrap_or_default()
}

/// Sets a property-list custom property to a string, but only after checking
/// that coreaudiod agrees it is a property-list reference. Writing data that
/// coreaudiod types differently can crash coreaudiod, so this refuses instead.
pub fn set_plist_string(id: AudioObjectID, selector: u32, value: &str) -> Result<()> {
    const PLIST: u32 = u32::from_be_bytes(*b"plst");
    let declared = custom_properties(id).iter().any(|c| c[0] == selector && c[1] == PLIST);
    let size = data_size(id, selector, Scope::Global)?;
    if !declared || size as usize != std::mem::size_of::<usize>() {
        return Err(Error {
            what: format!(
                "refusing to write {}: coreaudiod reports it as {} ({} bytes) instead of a property list",
                fourcc(selector),
                if declared { "declared" } else { "undeclared" },
                size
            ),
            status: 0,
        });
    }
    set_string(id, selector, Scope::Global, value)
}

/// Sets a string (CFString / property-list) property.
fn set_string(id: AudioObjectID, selector: u32, scope: Scope, value: &str) -> Result<()> {
    let cf = unsafe { CFStringCreateWithBytes(std::ptr::null(), value.as_ptr(), value.len() as isize, UTF8, 0) };
    let r = set_raw(id, selector, scope, cf as usize);
    unsafe { CFRelease(cf) };
    r
}

/// Object ID of the loaded plug-in with this bundle ID.
pub fn plugin(bundle_id: &str) -> Option<AudioObjectID> {
    get_ids(kAudioObjectSystemObject, kAudioHardwarePropertyPlugInList, Scope::Global)
        .ok()?
        .into_iter()
        .find(|p| get_string(*p, kAudioPlugInPropertyBundleID, Scope::Global).ok().as_deref() == Some(bundle_id))
}

fn take_cfstring(p: *const c_void) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut buf = vec![0u8; 256 * 1024];
    let ok = unsafe { CFStringGetCString(p, buf.as_mut_ptr(), buf.len() as isize, UTF8) };
    unsafe { CFRelease(p) };
    if ok == 0 {
        return String::new();
    }
    let n = buf.iter().position(|&b| b == 0).unwrap_or(0);
    String::from_utf8_lossy(&buf[..n]).into_owned()
}

#[derive(Clone, Debug)]
pub struct Device {
    pub id: AudioObjectID,
    pub name: String,
    pub uid: String,
    pub inputs: u32,
    pub outputs: u32,
}

/// Channel count of a device in one direction (sum over its streams).
pub fn channel_count(id: AudioObjectID, scope: Scope) -> u32 {
    get_ids(id, kAudioDevicePropertyStreams, scope)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|s| get_raw::<StreamFormat>(s, kAudioStreamPropertyVirtualFormat, Scope::Global, None).ok())
        .map(|f| f.channels_per_frame)
        .sum()
}

/// Channel count of each of a device's streams in one direction, in order.
pub fn stream_channels(id: AudioObjectID, scope: Scope) -> Vec<usize> {
    get_ids(id, kAudioDevicePropertyStreams, scope)
        .unwrap_or_default()
        .into_iter()
        .map(|s| get_raw::<StreamFormat>(s, kAudioStreamPropertyVirtualFormat, Scope::Global, None).map_or(0, |f| f.channels_per_frame as usize))
        .collect()
}

/// The device with this UID, including hidden devices.
pub fn device_by_uid(uid: &str) -> Option<AudioObjectID> {
    let cf = unsafe { CFStringCreateWithBytes(std::ptr::null(), uid.as_ptr(), uid.len() as isize, UTF8, 0) };
    let qualifier = (cf as usize).to_ne_bytes();
    let id = get_raw::<AudioObjectID>(kAudioObjectSystemObject, kAudioHardwarePropertyTranslateUIDToDevice, Scope::Global, Some(&qualifier));
    unsafe { CFRelease(cf) };
    id.ok().filter(|&id| id != kAudioObjectUnknown)
}

pub fn is_alive(id: AudioObjectID) -> bool {
    get_u32(id, kAudioDevicePropertyDeviceIsAlive, Scope::Global).is_ok_and(|v| v != 0)
}

pub fn device(id: AudioObjectID) -> Device {
    Device {
        id,
        name: get_string(id, kAudioObjectPropertyName, Scope::Global).unwrap_or_default(),
        uid: get_string(id, kAudioDevicePropertyDeviceUID, Scope::Global).unwrap_or_default(),
        inputs: channel_count(id, Scope::Input),
        outputs: channel_count(id, Scope::Output),
    }
}

pub fn devices() -> Result<Vec<Device>> {
    Ok(get_ids(kAudioObjectSystemObject, kAudioHardwarePropertyDevices, Scope::Global)?.into_iter().map(device).collect())
}

#[derive(Clone, Debug)]
pub struct Process {
    pub pid: i32,
    pub bundle_id: String,
    pub running_input: bool,
    pub running_output: bool,
    pub input_devices: Vec<AudioObjectID>,
    pub output_devices: Vec<AudioObjectID>,
}

pub fn processes() -> Result<Vec<Process>> {
    Ok(get_ids(kAudioObjectSystemObject, kAudioHardwarePropertyProcessObjectList, Scope::Global)?
        .into_iter()
        .map(|p| Process {
            pid: get_raw::<i32>(p, kAudioProcessPropertyPID, Scope::Global, None).unwrap_or(-1),
            bundle_id: get_string(p, kAudioProcessPropertyBundleID, Scope::Global).unwrap_or_default(),
            running_input: get_u32(p, kAudioProcessPropertyIsRunningInput, Scope::Global).unwrap_or(0) != 0,
            running_output: get_u32(p, kAudioProcessPropertyIsRunningOutput, Scope::Global).unwrap_or(0) != 0,
            input_devices: get_ids(p, kAudioProcessPropertyDevices, Scope::Input).unwrap_or_default(),
            output_devices: get_ids(p, kAudioProcessPropertyDevices, Scope::Output).unwrap_or_default(),
        })
        .collect())
}

// ------------------------------------------------------------------ meter

/// Running peak/energy per channel, updated from the IO thread.
pub struct Meter {
    pub channels: usize,
    pub peak: Vec<std::sync::atomic::AtomicU32>,
    pub sum_sq_milli: Vec<std::sync::atomic::AtomicU64>,
    pub frames: std::sync::atomic::AtomicU64,
}

impl Meter {
    fn new(channels: usize) -> Self {
        use std::sync::atomic::{AtomicU32, AtomicU64};
        Self {
            channels,
            peak: (0..channels).map(|_| AtomicU32::new(0)).collect(),
            sum_sq_milli: (0..channels).map(|_| AtomicU64::new(0)).collect(),
            frames: AtomicU64::new(0),
        }
    }

    /// Returns (peak, rms) per channel since the last call and resets.
    pub fn take(&self) -> Vec<(f32, f32)> {
        use std::sync::atomic::Ordering::Relaxed;
        let frames = self.frames.swap(0, Relaxed).max(1) as f64;
        (0..self.channels)
            .map(|c| {
                let peak = f32::from_bits(self.peak[c].swap(0, Relaxed));
                let sum = self.sum_sq_milli[c].swap(0, Relaxed) as f64 / 1e9;
                (peak, (sum / frames).sqrt() as f32)
            })
            .collect()
    }
}

unsafe extern "C" fn meter_proc(
    _dev: AudioObjectID,
    _now: *const c_void,
    input: *const AudioBufferList,
    _in_time: *const c_void,
    _output: *mut AudioBufferList,
    _out_time: *const c_void,
    client: *mut c_void,
) -> OSStatus {
    use std::sync::atomic::Ordering::Relaxed;
    let meter = &*(client as *const Meter);
    if input.is_null() || (*input).count == 0 {
        return 0;
    }
    let b = &(*input).buffers[0];
    let ch = b.channels.max(1) as usize;
    let n = b.byte_size as usize / 4;
    let samples = std::slice::from_raw_parts(b.data as *const f32, n);
    for c in 0..ch.min(meter.channels) {
        let mut peak = f32::from_bits(meter.peak[c].load(Relaxed));
        let mut sum = 0f64;
        for s in samples.iter().skip(c).step_by(ch) {
            peak = peak.max(s.abs());
            sum += (*s as f64) * (*s as f64);
        }
        meter.peak[c].store(peak.to_bits(), Relaxed);
        meter.sum_sq_milli[c].fetch_add((sum * 1e9) as u64, Relaxed);
    }
    meter.frames.fetch_add((n / ch) as u64, Relaxed);
    0
}

/// Records from a device's input and hands the meter to `each` every
/// `interval` until `total` has elapsed. No audio is written to the device.
pub fn meter_input(id: AudioObjectID, total: std::time::Duration, interval: std::time::Duration, mut each: impl FnMut(&Meter)) -> Result<()> {
    let channels = channel_count(id, Scope::Input).max(1) as usize;
    let meter = Box::new(Meter::new(channels));
    let mut proc_id: IOProcID = std::ptr::null_mut();
    let client = &*meter as *const Meter as *mut c_void;
    check(unsafe { AudioDeviceCreateIOProcID(id, meter_proc, client, &mut proc_id) }, || "creating IOProc".into())?;
    let started = check(unsafe { AudioDeviceStart(id, proc_id) }, || "starting device IO".into());
    if started.is_ok() {
        let end = std::time::Instant::now() + total;
        while std::time::Instant::now() < end {
            std::thread::sleep(interval.min(end.saturating_duration_since(std::time::Instant::now())));
            each(&meter);
        }
        unsafe { AudioDeviceStop(id, proc_id) };
    }
    unsafe { AudioDeviceDestroyIOProcID(id, proc_id) };
    started
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fourcc_formats_printable_codes() {
        assert_eq!(fourcc(0x6E737274), "'nsrt'");
        assert_eq!(fourcc(1), "0x00000001");
    }

    #[test]
    fn struct_sizes_match_the_sdk() {
        assert_eq!(std::mem::size_of::<Address>(), SIZEOF_AudioObjectPropertyAddress);
        assert_eq!(std::mem::size_of::<StreamFormat>(), SIZEOF_AudioStreamBasicDescription);
        assert_eq!(std::mem::size_of::<ValueRange>(), SIZEOF_AudioValueRange);
    }
}
