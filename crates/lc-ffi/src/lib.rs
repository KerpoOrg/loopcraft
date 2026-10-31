//! C interface to the LoopCraft core for the macOS editor (`include/lc_ffi.h`).
//!
//! Data crosses as JSON. The editor's model (`EConfig`) is a flat mirror of
//! `lc_config::Config` that Swift can decode directly; every save goes
//! through `lc_config` (YAML out, parsed back in), so the editor writes only
//! what the daemon accepts.

use std::ffi::{c_char, c_double, c_int, CStr, CString};
use std::path::Path;

use lc_ca as ca;
use lc_config::{Config, Device, Input, InputKind, Output, OutputKind, VirtualDevice, Wire};
use serde::{Deserialize, Serialize};
use serde_json::json;

/// The first line the editor writes; a file without it was written by hand.
const EDITOR_MARK: &str = "written by the LoopCraft editor";
/// In files written with bus and unit ids (see lc-config's writer).
const IDS_MARK: &str = "wires name them by id";

#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct EConfig {
    pub devices: Vec<EDevice>,
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct EDevice {
    pub name: String,
    pub id: String,
    /// Bus channels.
    pub busses: u32,
    /// In channel order, two channels each or one when mono. Wires are
    /// channel numbers here; the file names busses by these ids.
    #[serde(default)]
    pub bus_list: Vec<EBus>,
    #[serde(default)]
    pub groups: Vec<EGroup>,
    pub virtual_inputs: u32,
    pub sample_rate: u32,
    pub buffer_frames: Option<u32>,
    pub inputs: Vec<EPort>,
    pub outputs: Vec<EPort>,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct EBus {
    pub id: String,
    /// Empty: shown by its channel numbers.
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub mono: bool,
    /// A group's id; none: shown in the mixer itself.
    #[serde(default)]
    pub group: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct EGroup {
    pub id: String,
    pub name: String,
}

/// An input or an output. `kind`: inputs "virtual" | "virtual_device" |
/// "device" | "app"; outputs "device" | "virtual_device".
#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct EPort {
    pub kind: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub channels: Option<u32>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub mute_original: bool,
    #[serde(default)]
    pub tap: bool,
    /// [from, to] channel pairs, numbered from 1.
    pub wires: Vec<[u32; 2]>,
    pub enabled: bool,
    /// None = -inf (silent).
    pub gain_db: Option<f64>,
    pub muted: bool,
    #[serde(default)]
    pub follow_volume: bool,
    /// Channels shown one by one rather than as stereo pairs.
    #[serde(default)]
    pub mono: Vec<u32>,
    /// A half-width unit.
    #[serde(default)]
    pub half: bool,
}

fn gain_to(db: f64) -> Option<f64> {
    (db != f64::NEG_INFINITY).then_some(db)
}

fn gain_from(db: Option<f64>) -> f64 {
    db.unwrap_or(f64::NEG_INFINITY)
}

fn wires_to(w: &[Wire]) -> Vec<[u32; 2]> {
    w.iter().map(|w| [w.from, w.to]).collect()
}

fn wires_from(w: &[[u32; 2]]) -> Vec<Wire> {
    w.iter().map(|[from, to]| Wire { from: *from, to: *to }).collect()
}

fn virtual_port(v: &VirtualDevice) -> (Option<String>, Option<String>, Option<u32>, Option<String>) {
    (Some(v.name.clone()), Some(v.id.clone()), Some(v.channels), v.icon.clone())
}

pub fn to_editor(c: &Config) -> EConfig {
    let devices = c
        .devices
        .iter()
        .map(|d| EDevice {
            name: d.name.clone(),
            id: d.id.clone(),
            busses: d.busses,
            bus_list: d.bus_list.iter().map(|b| EBus { id: b.id.clone(), name: b.name.clone(), mono: b.mono, group: b.group.clone() }).collect(),
            groups: d.groups.iter().map(|g| EGroup { id: g.id.clone(), name: g.name.clone() }).collect(),
            virtual_inputs: d.virtual_inputs,
            sample_rate: d.sample_rate,
            buffer_frames: d.buffer_frames,
            inputs: d
                .inputs
                .iter()
                .map(|i| {
                    let (kind, (name, id, channels, icon), mute_original, tap) = match &i.kind {
                        InputKind::Virtual => ("virtual", (None, None, None, None), false, false),
                        InputKind::VirtualDevice(v) => ("virtual_device", virtual_port(v), false, false),
                        InputKind::Device(n) => ("device", (Some(n.clone()), None, None, None), false, false),
                        InputKind::App { name, mute_original, tap } => ("app", (Some(name.clone()), None, None, None), *mute_original, *tap),
                    };
                    EPort {
                        kind: kind.into(),
                        name,
                        id: id.or_else(|| Some(i.id.clone())),
                        channels,
                        icon,
                        mute_original,
                        tap,
                        wires: wires_to(&i.wires),
                        enabled: i.enabled,
                        gain_db: gain_to(i.gain_db),
                        muted: i.muted,
                        follow_volume: false,
                        mono: i.mono.clone(),
                        half: i.half,
                    }
                })
                .collect(),
            outputs: d
                .outputs
                .iter()
                .map(|o| {
                    let (kind, (name, id, channels, icon)) = match &o.kind {
                        OutputKind::Device(n) => ("device", (Some(n.clone()), None, None, None)),
                        OutputKind::Virtual(v) => ("virtual_device", virtual_port(v)),
                    };
                    EPort {
                        kind: kind.into(),
                        name,
                        id: id.or_else(|| Some(o.id.clone())),
                        channels,
                        icon,
                        mute_original: false,
                        tap: false,
                        wires: wires_to(&o.wires),
                        enabled: o.enabled,
                        gain_db: gain_to(o.gain_db),
                        muted: o.muted,
                        follow_volume: o.follow_volume,
                        mono: o.mono.clone(),
                        half: o.half,
                    }
                })
                .collect(),
        })
        .collect();
    EConfig { devices }
}

/// A unit's id; the editor gives new units one, so this only fills in for
/// a caller that didn't.
fn unit_id(p: &EPort, kind: char) -> String {
    p.id.clone().filter(|id| !id.trim().is_empty()).unwrap_or_else(|| lc_config::new_id(kind))
}

fn virtual_from(p: &EPort) -> Result<VirtualDevice, String> {
    let name = p.name.clone().ok_or("a virtual device needs a name")?;
    // Without one, the id the config file would give it, from its name.
    let id = p.id.clone().filter(|id| !id.is_empty()).unwrap_or_else(|| lc_config::id_from_name(&name));
    Ok(VirtualDevice {
        name,
        id,
        channels: p.channels.ok_or("a virtual device needs a channel count")?,
        icon: p.icon.clone(),
    })
}

pub fn from_editor(e: &EConfig) -> Result<Config, String> {
    let mut devices = Vec::new();
    for d in &e.devices {
        let mut inputs = Vec::new();
        for p in &d.inputs {
            let name = || p.name.clone().ok_or_else(|| format!("a '{}' input needs a name", p.kind));
            let kind = match p.kind.as_str() {
                "virtual" => InputKind::Virtual,
                "virtual_device" => InputKind::VirtualDevice(virtual_from(p)?),
                "device" => InputKind::Device(name()?),
                "app" => InputKind::App { name: name()?, mute_original: p.mute_original, tap: p.tap },
                other => return Err(format!("unknown input kind '{other}'")),
            };
            let id = match &kind {
                InputKind::VirtualDevice(v) => v.id.clone(),
                _ => unit_id(p, 'i'),
            };
            inputs.push(Input {
                id,
                kind,
                wires: wires_from(&p.wires),
                enabled: p.enabled,
                gain_db: gain_from(p.gain_db),
                muted: p.muted,
                mono: p.mono.clone(),
                half: p.half,
            });
        }
        let mut outputs = Vec::new();
        for p in &d.outputs {
            let kind = match p.kind.as_str() {
                "device" => OutputKind::Device(p.name.clone().ok_or("a device output needs a name")?),
                "virtual_device" => OutputKind::Virtual(virtual_from(p)?),
                other => return Err(format!("unknown output kind '{other}'")),
            };
            let id = match &kind {
                OutputKind::Virtual(v) => v.id.clone(),
                OutputKind::Device(_) => unit_id(p, 'o'),
            };
            outputs.push(Output {
                id,
                kind,
                wires: wires_from(&p.wires),
                enabled: p.enabled,
                gain_db: gain_from(p.gain_db),
                muted: p.muted,
                follow_volume: p.follow_volume,
                mono: p.mono.clone(),
                half: p.half,
            });
        }
        let mut bus_list: Vec<lc_config::Bus> = d
            .bus_list
            .iter()
            .map(|b| lc_config::Bus { id: b.id.clone(), name: b.name.trim().to_string(), mono: b.mono, group: b.group.clone() })
            .collect();
        // A caller that sent only a count gets busses with new ids; a list is what it says.
        let mut covered: u32 = bus_list.iter().map(lc_config::Bus::channels).sum();
        while bus_list.is_empty() || (d.bus_list.is_empty() && covered < d.busses) {
            let mono = covered + 1 == d.busses;
            bus_list.push(lc_config::Bus { id: lc_config::new_id('b'), name: String::new(), mono, group: None });
            covered += if mono { 1 } else { 2 };
        }
        devices.push(Device {
            name: d.name.clone(),
            id: d.id.clone(),
            busses: covered,
            bus_list,
            groups: d.groups.iter().map(|g| lc_config::BusGroup { id: g.id.clone(), name: g.name.trim().to_string() }).collect(),
            virtual_inputs: d.virtual_inputs,
            sample_rate: d.sample_rate,
            buffer_frames: d.buffer_frames,
            inputs,
            outputs,
        });
    }
    Ok(Config { devices })
}

/// Validates the editor's model the way the daemon will and returns the YAML.
pub fn render(e: &EConfig) -> Result<String, String> {
    let config = from_editor(e)?;
    let yaml = lc_config::to_yaml(&config);
    // Parsing back applies every rule (ids derived from names, wires checked).
    lc_config::parse(&yaml).map_err(|e| e.to_string())?;
    Ok(yaml)
}

pub fn save(path: &Path, e: &EConfig) -> Result<(), String> {
    let yaml = render(e)?;
    if let Ok(old) = std::fs::read_to_string(path) {
        let backup = path.with_extension("yaml.bak-before-editor");
        if !old.contains(EDITOR_MARK) && !backup.exists() {
            std::fs::write(&backup, &old).map_err(|e| format!("cannot keep a backup at {}: {e}", backup.display()))?;
        }
        // The first save in the id form keeps the file as it was before.
        let before_ids = path.with_extension("yaml.bak-before-ids");
        if !old.contains(IDS_MARK) && !before_ids.exists() && old.contains("devices:") {
            std::fs::write(&before_ids, &old).map_err(|e| format!("cannot keep a backup at {}: {e}", before_ids.display()))?;
        }
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    let tmp = path.with_extension("yaml.editor-tmp");
    std::fs::write(&tmp, yaml).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("cannot replace {}: {e}", path.display()))
}

fn out(v: serde_json::Value) -> *mut c_char {
    CString::new(v.to_string()).unwrap_or_default().into_raw()
}

unsafe fn arg(p: *const c_char) -> Option<String> {
    (!p.is_null()).then(|| CStr::from_ptr(p).to_string_lossy().into_owned())
}

#[no_mangle]
pub extern "C" fn lc_config_default_path() -> *mut c_char {
    out(json!(lc_config::default_path().display().to_string()))
}

/// # Safety
/// `path` is a NUL-terminated string or null.
#[no_mangle]
pub unsafe extern "C" fn lc_config_load(path: *const c_char) -> *mut c_char {
    let Some(path) = arg(path) else { return out(json!({"ok": false, "error": "no path"})) };
    let p = Path::new(&path);
    if !p.exists() {
        return out(json!({"ok": true, "config": EConfig { devices: vec![] }, "missing": true}));
    }
    match lc_config::load(p) {
        Ok(c) => out(json!({"ok": true, "config": to_editor(&c)})),
        Err(e) => out(json!({"ok": false, "error": e.to_string()})),
    }
}

/// # Safety
/// Both arguments are NUL-terminated strings or null.
#[no_mangle]
pub unsafe extern "C" fn lc_config_save(path: *const c_char, config_json: *const c_char) -> *mut c_char {
    let (Some(path), Some(text)) = (arg(path), arg(config_json)) else { return out(json!({"ok": false, "error": "missing argument"})) };
    let result = serde_json::from_str::<EConfig>(&text).map_err(|e| format!("bad editor data: {e}")).and_then(|e| save(Path::new(&path), &e));
    match result {
        Ok(()) => out(json!({"ok": true})),
        Err(e) => out(json!({"ok": false, "error": e})),
    }
}

#[no_mangle]
pub extern "C" fn lc_devices() -> *mut c_char {
    use lc_config::profile;
    use lc_sys::consts::{kAudioDevicePropertyNominalSampleRate, kAudioObjectPropertyManufacturer};
    let list = ca::devices().unwrap_or_default();
    let (profiles, _) = profile::load(&profile::user_dir());
    let devices: Vec<_> = list
        .iter()
        .map(|d| {
            let manufacturer = ca::get_string(d.id, kAudioObjectPropertyManufacturer, ca::Scope::Global).unwrap_or_default();
            let found = profile::find(&profiles, &d.name, &manufacturer);
            // A direction the profile doesn't describe keeps the device's own names.
            let groups = |scope: ca::Scope, from: Option<&Vec<profile::Group>>| match from.filter(|g| !g.is_empty()) {
                Some(g) => g.clone(),
                None => profile::groups_from_names(&ca::channel_names(d.id, scope)),
            };
            json!({
                "uid": d.uid,
                "name": d.name,
                "manufacturer": manufacturer,
                "inputs": d.inputs,
                "outputs": d.outputs,
                "rate": ca::get_f64(d.id, kAudioDevicePropertyNominalSampleRate, ca::Scope::Global).unwrap_or(0.0),
                "loopcraft": d.uid.starts_with(lc_config::UID_PREFIX),
                "input_groups": groups(ca::Scope::Input, found.map(|p| &p.inputs)),
                "output_groups": groups(ca::Scope::Output, found.map(|p| &p.outputs)),
                "default_input": found.map(|p| p.default.input.clone()).unwrap_or_default(),
                "default_output": found.map(|p| p.default.output.clone()).unwrap_or_default(),
                "profile": found.map(|p| p.source.clone()),
            })
        })
        .collect();
    out(json!(devices))
}

/// # Safety
/// `uid` is a NUL-terminated string or null.
#[no_mangle]
pub unsafe extern "C" fn lc_device_volume(uid: *const c_char) -> *mut c_char {
    use lc_sys::consts::{kAudioDevicePropertyMute, kAudioDevicePropertyVolumeDecibels, kAudioDevicePropertyVolumeScalar};
    let Some(id) = arg(uid).and_then(|u| ca::device_by_uid(&u)) else { return out(json!({"error": "no such device"})) };
    let scalar = ca::get_f32(id, kAudioDevicePropertyVolumeScalar, ca::Scope::Output);
    let db = ca::get_f32(id, kAudioDevicePropertyVolumeDecibels, ca::Scope::Output);
    let muted = ca::get_u32(id, kAudioDevicePropertyMute, ca::Scope::Output).unwrap_or(0) != 0;
    match (scalar, db) {
        (Ok(s), Ok(db)) => out(json!({"db": if s <= 0.0 { None } else { Some(db) }, "muted": muted})),
        _ => out(json!({"error": "the device has no volume control"})),
    }
}

/// # Safety
/// `uid` is a NUL-terminated string or null.
#[no_mangle]
pub unsafe extern "C" fn lc_set_device_volume(uid: *const c_char, db: c_double) -> c_int {
    use lc_sys::consts::{kAudioDevicePropertyVolumeDecibels, kAudioDevicePropertyVolumeScalar};
    let Some(id) = arg(uid).and_then(|u| ca::device_by_uid(&u)) else { return -1 };
    let r = if db < -200.0 {
        ca::set_f32(id, kAudioDevicePropertyVolumeScalar, ca::Scope::Output, 0.0)
    } else {
        ca::set_f32(id, kAudioDevicePropertyVolumeDecibels, ca::Scope::Output, db as f32)
    };
    if r.is_ok() {
        0
    } else {
        -1
    }
}

/// Writes a profile for this device to your profiles folder, to edit: a copy
/// of the one it has, or one made from its channel names; an existing file is
/// kept. Returns the file's path.
pub fn save_profile(uid: &str) -> Result<std::path::PathBuf, String> {
    use lc_config::profile;
    use lc_sys::consts::kAudioObjectPropertyManufacturer;
    let id = ca::device_by_uid(uid).ok_or("no such device")?;
    let d = ca::device(id);
    let manufacturer = ca::get_string(id, kAudioObjectPropertyManufacturer, ca::Scope::Global).unwrap_or_default();
    let dir = profile::user_dir();
    let (profiles, _) = profile::load(&dir);
    let p = match profile::find(&profiles, &d.name, &manufacturer) {
        Some(p) if !p.source.starts_with("built-in:") => return Ok(std::path::PathBuf::from(&p.source)),
        Some(p) => p.clone(),
        None => profile::from_names(&d.name, &manufacturer, &ca::channel_names(id, ca::Scope::Input), &ca::channel_names(id, ca::Scope::Output)),
    };
    let file: String = d.name.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
    let path = dir.join(format!("{}.yaml", file.trim_matches('-')));
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    if !path.exists() {
        std::fs::write(&path, profile::render(&p)).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    }
    Ok(path)
}

/// # Safety
/// `uid` is a NUL-terminated string or null.
#[no_mangle]
pub unsafe extern "C" fn lc_save_profile(uid: *const c_char) -> *mut c_char {
    match arg(uid).ok_or_else(|| "no device".to_string()).and_then(|u| save_profile(&u)) {
        Ok(path) => out(json!({"ok": true, "path": path.display().to_string()})),
        Err(e) => out(json!({"ok": false, "error": e})),
    }
}

/// # Safety
/// No arguments; returns a string to free.
#[no_mangle]
pub extern "C" fn lc_profiles_status() -> *mut c_char {
    let dir = lc_config::profile::user_dir();
    let (_, errors) = lc_config::profile::load(&dir);
    out(json!({"dir": dir.display().to_string(), "errors": errors}))
}

/// Presets: list, store (a mixer in the editor's model), load, delete.
pub fn presets() -> serde_json::Value {
    let dir = lc_config::preset::dir();
    json!({"dir": dir.display().to_string(), "names": lc_config::preset::list(&dir)})
}

pub fn store_preset(name: &str, device: &str) -> Result<(), String> {
    let d: EDevice = serde_json::from_str(device).map_err(|e| format!("bad mixer: {e}"))?;
    // `save` checks it like a config file.
    let c = from_editor(&EConfig { devices: vec![d] })?;
    lc_config::preset::save(&lc_config::preset::dir(), name, &c.devices[0])
}

pub fn load_preset(name: &str) -> Result<EDevice, String> {
    let d = lc_config::preset::load(&lc_config::preset::dir(), name)?;
    Ok(to_editor(&Config { devices: vec![d] }).devices.remove(0))
}

/// # Safety
/// No arguments; returns a string to free.
#[no_mangle]
pub extern "C" fn lc_presets() -> *mut c_char {
    out(presets())
}

/// # Safety
/// `name` and `device` are NUL-terminated strings or null.
#[no_mangle]
pub unsafe extern "C" fn lc_preset_store(name: *const c_char, device: *const c_char) -> *mut c_char {
    let r = match (arg(name), arg(device)) {
        (Some(n), Some(d)) => store_preset(&n, &d),
        _ => Err("a preset needs a name and a mixer".into()),
    };
    out(match r {
        Ok(()) => json!({"ok": true}),
        Err(e) => json!({"ok": false, "error": e}),
    })
}

/// # Safety
/// `name` is a NUL-terminated string or null.
#[no_mangle]
pub unsafe extern "C" fn lc_preset_load(name: *const c_char) -> *mut c_char {
    out(match arg(name).ok_or_else(|| "no preset name".to_string()).and_then(|n| load_preset(&n)) {
        Ok(d) => json!({"ok": true, "device": d}),
        Err(e) => json!({"ok": false, "error": e}),
    })
}

/// # Safety
/// `name` is a NUL-terminated string or null.
#[no_mangle]
pub unsafe extern "C" fn lc_preset_delete(name: *const c_char) -> *mut c_char {
    let r = arg(name).ok_or_else(|| "no preset name".to_string()).and_then(|n| lc_config::preset::delete(&lc_config::preset::dir(), &n));
    out(match r {
        Ok(()) => json!({"ok": true}),
        Err(e) => json!({"ok": false, "error": e}),
    })
}

/// Levels files stay mapped between calls.
static LEVELS: std::sync::Mutex<Vec<(String, lc_meter::Levels)>> = std::sync::Mutex::new(Vec::new());

pub fn levels(device_id: &str) -> serde_json::Value {
    let mut open = LEVELS.lock().unwrap_or_else(|e| e.into_inner());
    if !open.iter().any(|(id, _)| id == device_id) {
        match lc_meter::Levels::open(&lc_meter::path(device_id)) {
            Ok(l) => open.push((device_id.to_string(), l)),
            Err(e) => return json!({"error": format!("no levels for '{device_id}': {e}")}),
        }
    }
    let l = &open.iter().find(|(id, _)| id == device_id).unwrap().1;
    let Some(shape) = l.shape() else { return json!({"error": "levels file has a bad layout"}) };
    let mut next = 0;
    let mut take = |n: u32| -> Vec<f32> {
        (0..n)
            .map(|_| {
                next += 1;
                l.take(next - 1)
            })
            .collect()
    };
    let inputs: Vec<Vec<f32>> = shape.inputs.iter().map(|&n| take(n)).collect();
    let busses = take(shape.busses);
    let outputs: Vec<Vec<f32>> = shape.outputs.iter().map(|&n| take(n)).collect();
    json!({"generation": l.generation(), "inputs": inputs, "busses": busses, "outputs": outputs})
}

/// Asks the daemon to fade a mixer's outputs out and back in around the
/// next change (a preset switch). False when the daemon isn't routing it.
pub fn request_transition(device_id: &str) -> bool {
    let mut open = LEVELS.lock().unwrap_or_else(|e| e.into_inner());
    if !open.iter().any(|(id, _)| id == device_id) {
        match lc_meter::Levels::open(&lc_meter::path(device_id)) {
            Ok(l) => open.push((device_id.to_string(), l)),
            Err(_) => return false,
        }
    }
    open.iter().find(|(id, _)| id == device_id).map(|(_, l)| l.request_transition()).is_some()
}

/// # Safety
/// `device_id` is a NUL-terminated string or null.
#[no_mangle]
pub unsafe extern "C" fn lc_request_transition(device_id: *const c_char) -> c_int {
    match arg(device_id) {
        Some(id) if request_transition(&id) => 0,
        _ => -1,
    }
}

/// # Safety
/// `device_id` is a NUL-terminated string or null.
#[no_mangle]
pub unsafe extern "C" fn lc_levels(device_id: *const c_char) -> *mut c_char {
    match arg(device_id) {
        Some(id) => out(levels(&id)),
        None => out(json!({"error": "no device id"})),
    }
}

/// # Safety
/// `uid` is a NUL-terminated string or null.
/// Sets a device's sample rate, if it offers that rate. Only on request:
/// LoopCraft never changes a rate by itself. 0 on success.
#[no_mangle]
pub unsafe extern "C" fn lc_set_device_rate(uid: *const c_char, rate: c_double) -> c_int {
    use lc_sys::consts::{kAudioDevicePropertyAvailableNominalSampleRates, kAudioDevicePropertyNominalSampleRate};
    let Some(id) = arg(uid).and_then(|u| ca::device_by_uid(&u)) else { return -1 };
    let offered = ca::get_ranges(id, kAudioDevicePropertyAvailableNominalSampleRates, ca::Scope::Global).unwrap_or_default();
    if !offered.iter().any(|r| r.min <= rate && rate <= r.max) {
        return -2;
    }
    match ca::set_f64(id, kAudioDevicePropertyNominalSampleRate, ca::Scope::Global, rate) {
        Ok(()) => 0,
        Err(_) => -1,
    }
}

/// # Safety
/// `s` was returned by one of these functions, or is null.
#[no_mangle]
pub unsafe extern "C" fn lc_free(s: *mut c_char) {
    if !s.is_null() {
        drop(CString::from_raw(s));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SETUP: &str = concat!(
        "devices:\n  - name: LoopCraft Virtual Sound\n    id: lvs\n    busses: 10\n    virtual_inputs: 2\n    buffer_frames: 32\n",
        "    inputs:\n",
        "      - {virtual: true, wires: {1: 1, 2: 2}}\n",
        "      - {app: Spotify, wires: {left: 3, right: 4}, mute_original: true}\n",
        "      - {virtual: Comms, channels: 2, icon: Discord, wires: {1: 7, 2: 8}}\n",
        "    outputs:\n",
        "      - {device: PRO X, wires: {1: 1, 2: 2, 3: 1, 4: 2}, follow_volume: true}\n",
        "      - {virtual: Daw Send, id: daw-output, channels: 8, wires: {1: 1, 2: 2}, gain: -inf}\n",
    );

    #[test]
    fn editor_model_round_trips() {
        let c = lc_config::parse(SETUP).unwrap();
        let e = to_editor(&c);
        let json = serde_json::to_string(&e).unwrap();
        let back: EConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back, e);
        assert_eq!(from_editor(&back).unwrap(), c);
        assert!(json.contains("\"gain_db\":null"), "-inf as null: {json}");
        assert_eq!(e.devices[0].inputs[2].icon.as_deref(), Some("Discord"));
    }

    #[test]
    fn new_virtual_devices_get_an_id() {
        let mut e = to_editor(&lc_config::parse(SETUP).unwrap());
        let added = r#"{"kind":"virtual_device","name":"Comms Send","channels":2,"wires":[[7,1],[8,2]],"enabled":true,"gain_db":0,"muted":false}"#;
        e.devices[0].outputs.push(serde_json::from_str(added).unwrap());
        let c = lc_config::parse(&render(&e).unwrap()).unwrap();
        assert!(c.devices[0].virtual_specs().iter().any(|v| v.id == "comms-send" && v.name == "Comms Send"));
    }

    #[test]
    fn renaming_keeps_the_ids() {
        let mut e = to_editor(&lc_config::parse(SETUP).unwrap());
        let (comms, send) = (e.devices[0].inputs[2].id.clone(), e.devices[0].outputs[1].id.clone());
        assert_eq!(comms.as_deref(), Some("comms"), "derived from the name when loaded");
        e.devices[0].inputs[2].name = Some("Discord".into());
        e.devices[0].outputs[1].name = Some("To FL Studio".into());
        e.devices[0].name = "Studio".into();
        let c = lc_config::parse(&render(&e).unwrap()).unwrap();
        let d = &c.devices[0];
        assert_eq!((d.name.as_str(), d.id.as_str()), ("Studio", "lvs"), "the mixer keeps its id");
        let specs = d.virtual_specs();
        let by_id = |id: &str| specs.iter().find(|v| v.id == id).map(|v| v.name.clone());
        assert_eq!(by_id(comms.as_deref().unwrap()).as_deref(), Some("Discord"));
        assert_eq!(by_id(send.as_deref().unwrap()).as_deref(), Some("To FL Studio"));
    }

    #[test]
    fn saving_validates_and_keeps_a_backup() {
        let dir = std::env::temp_dir().join(format!("lc-ffi-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.yaml");
        std::fs::write(&path, "# my own notes\n".to_string() + SETUP).unwrap();
        let mut e = to_editor(&lc_config::parse(SETUP).unwrap());
        e.devices[0].outputs[0].gain_db = Some(-6.0);
        save(&path, &e).unwrap();
        let backup = dir.join("config.yaml.bak-before-editor");
        assert!(std::fs::read_to_string(&backup).unwrap().contains("my own notes"));
        assert!(std::fs::read_to_string(dir.join("config.yaml.bak-before-ids")).unwrap().contains("my own notes"), "the file before ids");
        assert!(std::fs::read_to_string(&path).unwrap().contains("wires name them by id"));
        assert_eq!(lc_config::load(&path).unwrap().devices[0].outputs[0].gain_db, -6.0);
        // A second save keeps the original backup.
        save(&path, &e).unwrap();
        assert!(std::fs::read_to_string(&backup).unwrap().contains("my own notes"));
        // Invalid data is refused and the file stays as it was.
        e.devices[0].inputs[0].wires.push([5, 1]);
        assert!(save(&path, &e).unwrap_err().contains("does not exist"));
        assert_eq!(lc_config::load(&path).unwrap().devices[0].inputs[0].wires.len(), 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn c_interface() {
        unsafe {
            let p = CString::new("/nonexistent/loopcraft/config.yaml").unwrap();
            let r = lc_config_load(p.as_ptr());
            let v: serde_json::Value = serde_json::from_str(CStr::from_ptr(r).to_str().unwrap()).unwrap();
            assert_eq!(v["ok"], true);
            assert_eq!(v["missing"], true);
            lc_free(r);
            let bad = CString::new("{").unwrap();
            let r = lc_config_save(p.as_ptr(), bad.as_ptr());
            assert!(CStr::from_ptr(r).to_str().unwrap().contains("bad editor data"));
            lc_free(r);
        }
    }
}
