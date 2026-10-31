//! LoopCraft's hand-editable YAML configuration.
//!
//! Each device is a small mixer: **inputs** (what apps play into the device,
//! captured apps, hardware inputs) are wired onto **busses**, which recording
//! apps receive as the device's channels, and busses are wired to hardware
//! **outputs**. macOS sees it from the other side: the virtual inputs are the
//! device's output channels and the busses its input channels.
//!
//! `parse` turns the file into a validated [`Config`]: unknown keys, wrong
//! types and impossible wiring are all errors, and validation reports every
//! problem at once so a hand edit can be fixed in one go. Hardware devices are
//! kept as the names or UIDs the user wrote; [`find_device`] resolves them
//! against the devices present at the time.

mod ids;
pub mod preset;
pub mod profile;
mod raw;
mod write;

pub use ids::new_id;
pub use write::to_yaml;
/// The id a virtual device gets from its name when it has none: "Daw Send" → "daw-send".
pub use raw::slug as id_from_name;

use std::fmt;
use std::path::{Path, PathBuf};

/// Prefix that turns a device `id` into its Core Audio UID.
pub const UID_PREFIX: &str = "org.kerpo.loopcraft:";
/// Environment variable that overrides the config path.
pub const PATH_ENV: &str = "LOOPCRAFT_CONFIG";

pub use lc_spec::{DeviceSpec, VirtualSpec, MAX_CHANNELS, MAX_DEVICES, MAX_VIRTUALS, SAMPLE_RATES};

/// Highest gain in dB; 0 dB is unity.
pub const MAX_GAIN_DB: f64 = 12.0;

/// Range of a device engine's IO buffer, in frames.
pub const MIN_BUFFER_FRAMES: u32 = 16;
pub const MAX_BUFFER_FRAMES: u32 = 4096;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Config {
    pub devices: Vec<Device>,
}

impl Config {
    /// The device list for the plug-in, in config order. App slots stay empty:
    /// the daemon fills them, since it resolves app names to bundle IDs.
    pub fn to_specs(&self) -> Vec<DeviceSpec> {
        self.devices
            .iter()
            .map(|d| DeviceSpec {
                uid: d.uid(),
                name: d.name.clone(),
                channels: d.busses,
                pass_thru: d.virtual_inputs,
                sample_rate: f64::from(d.sample_rate),
                apps: vec![],
                apply_gain: !d.volume_follows_outputs(),
                virtuals: d.virtual_specs(),
            })
            .collect()
    }
}

/// A LoopCraft virtual device.
#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    pub name: String,
    /// Stable ID; the device's UID is derived from it.
    pub id: String,
    /// Bus channels: what inputs are summed onto and recording apps receive.
    pub busses: u32,
    /// The busses on those channels, in channel order: a stereo bus takes
    /// two, a mono bus one.
    pub bus_list: Vec<Bus>,
    /// Named groups the editor shows busses in, each as a unit of its own.
    pub groups: Vec<BusGroup>,
    /// Channels apps can play into the device; 0 makes it record-only.
    pub virtual_inputs: u32,
    pub sample_rate: u32,
    /// IO buffer of the device's routing engine. Smaller means less delay
    /// (each pass through the engine costs two buffers) and more CPU;
    /// `None` leaves Core Audio's default.
    pub buffer_frames: Option<u32>,
    pub inputs: Vec<Input>,
    pub outputs: Vec<Output>,
}

impl Device {
    pub fn uid(&self) -> String {
        format!("{UID_PREFIX}{}", self.id)
    }

    /// The bus channels of `bus_list[k]`: its first and, for a stereo bus, second.
    pub fn bus_channels(&self, k: usize) -> (u32, Option<u32>) {
        bus_channels(&self.bus_list, k)
    }

    /// The bus a bus channel belongs to.
    pub fn bus_of(&self, channel: u32) -> Option<usize> {
        bus_of(&self.bus_list, channel)
    }
}

/// Channels of `list[k]`, counting the busses before it.
pub(crate) fn bus_channels(list: &[Bus], k: usize) -> (u32, Option<u32>) {
    let first = 1 + list[..k].iter().map(Bus::channels).sum::<u32>();
    (first, (!list[k].mono).then_some(first + 1))
}

pub(crate) fn bus_of(list: &[Bus], channel: u32) -> Option<usize> {
    let mut next = 1;
    list.iter().position(|b| {
        next += b.channels();
        channel < next
    })
}

/// A bus: what config files and the editor know it by. Wires in the file
/// name the bus by `id`, which never changes; its channels follow from its
/// place in the list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bus {
    pub id: String,
    /// Empty: shown by its channel numbers.
    pub name: String,
    /// One channel instead of two (a mic, a call).
    pub mono: bool,
    /// The id of the group it is shown in; none: in the mixer itself.
    pub group: Option<String>,
}

impl Bus {
    pub fn channels(&self) -> u32 {
        if self.mono { 1 } else { 2 }
    }
}

/// Busses shown together, as a unit of their own ("DAW", "Apps"). Only the
/// editor uses groups; the busses' channels don't depend on them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BusGroup {
    pub id: String,
    pub name: String,
}

/// A device of the mixer's own, on its clock: playback-only as an input
/// (apps play into it), record-only as an output (apps record from it).
/// macOS shows it as "<mixer name>: <name>".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VirtualDevice {
    pub name: String,
    /// Stable ID (from `id:`, else from the name); apps remember the device by it.
    pub id: String,
    pub channels: u32,
    /// The app whose icon the editor shows for it (`icon: Discord`), by name or bundle ID.
    pub icon: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputKind {
    /// The virtual inputs: what apps play into the device.
    Virtual,
    /// A virtual input device of its own (`virtual: Comms`).
    VirtualDevice(VirtualDevice),
    /// A hardware input device, by name or UID.
    Device(String),
    /// An application, by name or bundle ID. Captured inside the plug-in as
    /// it plays into this device; with `tap`, through a Core Audio process
    /// tap instead, which also works for apps playing to other devices but
    /// adds about 65 ms (and delays the device's other routes too).
    App { name: String, mute_original: bool, tap: bool },
}

/// Something wired onto the device's busses.
#[derive(Clone, Debug, PartialEq)]
pub struct Input {
    /// Stable ID within the mixer (for a virtual device, its device ID).
    pub id: String,
    pub kind: InputKind,
    /// Input channel → bus.
    pub wires: Vec<Wire>,
    pub enabled: bool,
    /// Gain in dB: 0 = unity, `-inf` = silent, at most +12.
    pub gain_db: f64,
    pub muted: bool,
    /// How the editor shows it: channels whose jacks and meters are one per
    /// channel instead of a stereo pair, and a half-width unit.
    pub mono: Vec<u32>,
    pub half: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OutputKind {
    /// A hardware output device, by name or UID.
    Device(String),
    /// A virtual output device of its own (`virtual: Daw Output`).
    Virtual(VirtualDevice),
}

/// Where busses are sent: hardware, or a virtual output device apps record.
#[derive(Clone, Debug, PartialEq)]
pub struct Output {
    /// Stable ID within the mixer (for a virtual device, its device ID).
    pub id: String,
    pub kind: OutputKind,
    /// Bus → output channel.
    pub wires: Vec<Wire>,
    pub enabled: bool,
    /// Gain in dB: 0 = unity, `-inf` = silent, at most +12.
    pub gain_db: f64,
    pub muted: bool,
    /// Also apply the LoopCraft device's own volume (the macOS slider and
    /// volume keys, e.g. a headset's wheel) and mute, on top of `gain`. For
    /// outputs without a volume knob of their own. Once any output follows,
    /// the device's volume no longer scales what apps play in.
    pub follow_volume: bool,
    /// As for inputs: mono channels and a half-width unit, in the editor.
    pub mono: Vec<u32>,
    pub half: bool,
}

impl Device {
    /// Whether the device's volume is applied to outputs instead of inputs.
    pub fn volume_follows_outputs(&self) -> bool {
        self.outputs.iter().any(|o| o.follow_volume)
    }

    /// The virtual devices for the plug-in: virtual inputs then virtual
    /// outputs, in config order (the order of their engine streams).
    pub fn virtual_specs(&self) -> Vec<VirtualSpec> {
        let spec = |v: &VirtualDevice, output| VirtualSpec { id: v.id.clone(), name: v.name.clone(), channels: v.channels, output };
        let ins = self.inputs.iter().filter_map(|i| match &i.kind {
            InputKind::VirtualDevice(v) => Some(spec(v, false)),
            _ => None,
        });
        let outs = self.outputs.iter().filter_map(|o| match &o.kind {
            OutputKind::Virtual(v) => Some(spec(v, true)),
            _ => None,
        });
        ins.chain(outs).collect()
    }
}

/// One channel connection; channels are numbered from 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Wire {
    pub from: u32,
    pub to: u32,
}

#[derive(Debug)]
pub enum Error {
    Read { path: PathBuf, source: std::io::Error },
    /// Not valid YAML, or a key or value of the wrong shape.
    Syntax(String),
    /// Well-formed, but describes something LoopCraft can't do.
    Invalid(Vec<String>),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Read { path, source } => write!(f, "cannot read {}: {source}", path.display()),
            Error::Syntax(msg) => write!(f, "{msg}"),
            Error::Invalid(problems) => {
                write!(f, "invalid configuration:")?;
                for p in problems {
                    write!(f, "\n  - {p}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Read { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// `$LOOPCRAFT_CONFIG`, else `~/.config/loopcraft/config.yaml`.
pub fn default_path() -> PathBuf {
    if let Some(p) = std::env::var_os(PATH_ENV).filter(|p| !p.is_empty()) {
        return PathBuf::from(p);
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    home.join(".config/loopcraft/config.yaml")
}

pub fn load(path: &Path) -> Result<Config, Error> {
    let text = std::fs::read_to_string(path).map_err(|source| Error::Read { path: path.into(), source })?;
    parse(&text)
}

/// Parses and validates a configuration. An empty file is an empty config.
pub fn parse(text: &str) -> Result<Config, Error> {
    if text.trim().is_empty() {
        return Ok(Config::default());
    }
    let raw: Option<raw::Config> = serde_saphyr::from_str(text).map_err(|e| Error::Syntax(e.to_string()))?;
    raw::validate(raw.unwrap_or_default()).map_err(Error::Invalid)
}

/// A hardware audio device present on the system.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HardwareDevice {
    pub uid: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MatchError {
    NotFound,
    /// Several devices match; their names.
    Ambiguous(Vec<String>),
}

impl fmt::Display for MatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MatchError::NotFound => write!(f, "no such device"),
            MatchError::Ambiguous(names) => write!(f, "matches several devices: {}", names.join(", ")),
        }
    }
}

/// Resolves a device as written in the config: exact UID, then exact name,
/// then a part of the name (ignoring case) that only one device has.
pub fn find_device<'a>(query: &str, devices: &'a [HardwareDevice]) -> Result<&'a HardwareDevice, MatchError> {
    if let Some(d) = devices.iter().find(|d| d.uid == query) {
        return Ok(d);
    }
    let unique = |hits: Vec<&'a HardwareDevice>| match hits.len() {
        0 => Err(MatchError::NotFound),
        1 => Ok(hits[0]),
        _ => Err(MatchError::Ambiguous(hits.iter().map(|d| d.name.clone()).collect())),
    };
    match unique(devices.iter().filter(|d| d.name == query).collect()) {
        Err(MatchError::NotFound) => {}
        result => return result,
    }
    let needle = query.to_lowercase();
    if needle.trim().is_empty() {
        return Err(MatchError::NotFound);
    }
    unique(devices.iter().filter(|d| d.name.to_lowercase().contains(&needle)).collect())
}

#[cfg(test)]
mod tests;
