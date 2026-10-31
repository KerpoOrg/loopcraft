//! Device definitions as exchanged with the daemon/CLI (JSON inside a CFString
//! custom property) and persisted in coreaudiod's plug-in storage.

use serde::{Deserialize, Serialize};

/// The plug-in's bundle ID, which clients look it up by.
pub const BUNDLE_ID: &str = "org.kerpo.loopcraft.hal";
/// Custom plug-in property carrying the device list as JSON.
///
/// Apple reserves all-lowercase codes; an earlier all-lowercase code ('lcdv')
/// was typed differently by coreaudiod and a client write crashed coreaudiod.
/// Third-party codes therefore always contain uppercase letters or '#'.
pub const DEVICES_PROPERTY: u32 = u32::from_be_bytes(*b"LCd#");

pub const MAX_CHANNELS: u32 = 64;
pub const MAX_DEVICES: usize = 32;
/// App slots per device: apps whose output the plug-in captures separately.
pub const MAX_APPS: usize = 16;
/// Virtual input devices per mixer.
pub const MAX_VIRTUALS: usize = 16;
pub const SAMPLE_RATES: [f64; 6] = [44_100.0, 48_000.0, 88_200.0, 96_000.0, 176_400.0, 192_000.0];
pub const DEFAULT_SAMPLE_RATE: f64 = 48_000.0;

/// The M1 device keeps its UID so existing app selections stay valid.
pub const DEFAULT_UID: &str = "org.kerpo.loopcraft:8E0F3C52-6B7A-4C1E-9D2F-3A5B7C9E1F20";
pub const DEFAULT_NAME: &str = "LoopCraft Rack";
/// Suffix that turns a device UID into its engine twin's UID.
pub const TWIN_SUFFIX: &str = "#engine";

fn two() -> u32 {
    2
}

fn default_rate() -> f64 {
    DEFAULT_SAMPLE_RATE
}

fn yes() -> bool {
    true
}

/// An app captured separately as it plays into the device: its audio goes to
/// its own input stream on the engine twin instead of (or as well as) the
/// virtual-input mix.
/// A device of its own on the mixer's clock ("<mixer name>: <name>" in macOS).
/// A virtual input is playback-only: apps choose it as their output, and the
/// engine reads it as another input stream. A virtual output (`output`) is
/// record-only: the engine writes the busses wired to it, and apps such as a
/// DAW choose it as their input.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VirtualSpec {
    /// Stable within the mixer; the device's UID is `<mixer uid>/<id>`.
    pub id: String,
    pub name: String,
    pub channels: u32,
    #[serde(default)]
    pub output: bool,
}

impl VirtualSpec {
    pub fn uid(&self, mixer_uid: &str) -> String {
        format!("{mixer_uid}/{}", self.id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppSpec {
    /// The app's bundle ID and those of its helper processes.
    pub bundle_ids: Vec<String>,
    /// Leave the app out of the virtual-input mix.
    #[serde(default)]
    pub exclusive: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceSpec {
    pub uid: String,
    pub name: String,
    /// Output channels: what recording apps receive.
    #[serde(default = "two")]
    pub channels: u32,
    /// Channels apps can play into the device; 0 makes it record-only.
    #[serde(default = "two")]
    pub pass_thru: u32,
    #[serde(default = "default_rate")]
    pub sample_rate: f64,
    /// Always written, so clients can tell a plug-in that knows app slots
    /// from an older one (see [`reported_fields`]).
    #[serde(default)]
    pub apps: Vec<AppSpec>,
    /// Apply the device's output gain and mute (the macOS volume slider and
    /// keys) to what apps play into it. False when the daemon applies that
    /// volume to particular outputs instead.
    #[serde(default = "yes")]
    pub apply_gain: bool,
    /// Virtual input devices, in the order of their engine streams.
    #[serde(default)]
    pub virtuals: Vec<VirtualSpec>,
}

impl DeviceSpec {
    /// True when the change needs new streams or rings rather than a rename.
    pub fn layout_differs(&self, other: &DeviceSpec) -> bool {
        self.channels != other.channels || self.pass_thru != other.pass_thru || self.apps.len() != other.apps.len() || self.virtuals != other.virtuals
    }
}

pub fn default_specs() -> Vec<DeviceSpec> {
    vec![DeviceSpec {
        uid: DEFAULT_UID.into(),
        name: DEFAULT_NAME.into(),
        channels: 2,
        pass_thru: 2,
        sample_rate: DEFAULT_SAMPLE_RATE,
        apps: vec![],
        apply_gain: true,
        virtuals: vec![],
    }]
}

/// Optional fields, newest last. A plug-in writes every field it knows, so
/// the fields in its device list tell what it understands.
pub const OPTIONAL_FIELDS: [&str; 3] = ["apps", "apply_gain", "virtuals"];

/// The optional fields a plug-in's device list shows it understands.
pub fn reported_fields(json: &str) -> Vec<&'static str> {
    let Ok(serde_json::Value::Array(devices)) = serde_json::from_str::<serde_json::Value>(json) else { return vec![] };
    OPTIONAL_FIELDS.into_iter().filter(|f| !devices.is_empty() && devices.iter().all(|d| d.get(*f).is_some())).collect()
}

/// Like [`to_json`], but without the optional fields a plug-in doesn't know
/// (older plug-ins reject unknown fields). Fails if a left-out field would
/// carry anything but its default.
pub fn to_json_for(specs: &[DeviceSpec], known: &[&str]) -> Result<String, String> {
    let defaults = serde_json::to_value(&default_specs()[0]).map_err(|e| e.to_string())?;
    let mut list = serde_json::to_value(specs).map_err(|e| e.to_string())?;
    for device in list.as_array_mut().into_iter().flatten() {
        let Some(fields) = device.as_object_mut() else { continue };
        for f in OPTIONAL_FIELDS.iter().filter(|f| !known.contains(f)) {
            if let Some(v) = fields.remove(*f) {
                if Some(&v) != defaults.get(*f) {
                    return Err(format!("the installed plug-in doesn't support '{f}'"));
                }
            }
        }
    }
    Ok(list.to_string())
}

pub fn to_json(specs: &[DeviceSpec]) -> String {
    serde_json::to_string(specs).unwrap_or_else(|_| "[]".into())
}

/// Parses and validates a device list.
pub fn parse(json: &str) -> Result<Vec<DeviceSpec>, String> {
    let specs: Vec<DeviceSpec> = serde_json::from_str(json).map_err(|e| format!("invalid device list: {e}"))?;
    if specs.len() > MAX_DEVICES {
        return Err(format!("at most {MAX_DEVICES} devices are supported"));
    }
    let mut seen = std::collections::HashSet::new();
    for s in &specs {
        if s.uid.trim().is_empty() || s.uid.contains('#') {
            return Err(format!("device '{}': UID must be non-empty and must not contain '#'", s.name));
        }
        if !seen.insert(s.uid.as_str()) {
            return Err(format!("duplicate device UID '{}'", s.uid));
        }
        if s.name.trim().is_empty() {
            return Err(format!("device '{}': name must not be empty", s.uid));
        }
        if !(1..=MAX_CHANNELS).contains(&s.channels) {
            return Err(format!("device '{}': channels must be 1-{MAX_CHANNELS}", s.name));
        }
        if s.pass_thru > MAX_CHANNELS {
            return Err(format!("device '{}': pass_thru must be 0-{MAX_CHANNELS}", s.name));
        }
        if !SAMPLE_RATES.contains(&s.sample_rate) {
            return Err(format!("device '{}': unsupported sample rate {}", s.name, s.sample_rate));
        }
        if s.apps.len() > MAX_APPS {
            return Err(format!("device '{}': at most {MAX_APPS} apps", s.name));
        }
        if !s.apps.is_empty() && s.pass_thru == 0 {
            return Err(format!("device '{}': apps need channels to play into (pass_thru > 0)", s.name));
        }
        if s.virtuals.len() > MAX_VIRTUALS {
            return Err(format!("device '{}': at most {MAX_VIRTUALS} virtual input devices", s.name));
        }
        let mut ids = std::collections::HashSet::new();
        for v in &s.virtuals {
            let id_ok = !v.id.is_empty() && v.id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
            if !id_ok || !ids.insert(v.id.as_str()) {
                return Err(format!("device '{}': virtual input id '{}' is invalid or repeated", s.name, v.id));
            }
            if v.name.trim().is_empty() || !(1..=MAX_CHANNELS).contains(&v.channels) {
                return Err(format!("device '{}': virtual input '{}' needs a name and 1-{MAX_CHANNELS} channels", s.name, v.id));
            }
        }
        if s.apps.iter().any(|a| a.bundle_ids.is_empty() || a.bundle_ids.iter().any(|b| b.trim().is_empty())) {
            return Err(format!("device '{}': every app needs bundle IDs", s.name));
        }
    }
    Ok(specs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_with_defaults() {
        let s = parse(r#"[{"uid":"org.kerpo.loopcraft:desktop","name":"Desktop","channels":10}]"#).unwrap();
        assert_eq!(s[0].channels, 10);
        assert_eq!(s[0].pass_thru, 2);
        assert_eq!(s[0].sample_rate, 48_000.0);
        assert_eq!(parse(&to_json(&s)).unwrap(), s);
    }

    #[test]
    fn rejects_bad_lists() {
        assert!(parse("not json").is_err());
        assert!(parse(r#"[{"uid":"a","name":"A","channels":0}]"#).is_err());
        assert!(parse(r#"[{"uid":"a","name":"A","channels":65}]"#).is_err());
        assert!(parse(r#"[{"uid":"a#b","name":"A"}]"#).is_err());
        assert!(parse(r#"[{"uid":"a","name":"A"},{"uid":"a","name":"B"}]"#).is_err());
        assert!(parse(r#"[{"uid":"a","name":"A","sample_rate":12345}]"#).is_err());
        assert!(parse(r#"[{"uid":"a","name":"A","colour":"red"}]"#).is_err());
        assert!(parse("[]").unwrap().is_empty());
        assert!(parse(r#"[{"uid":"a","name":"A","apps":[{"bundle_ids":[]}]}]"#).is_err());
        assert!(parse(r#"[{"uid":"a","name":"A","pass_thru":0,"apps":[{"bundle_ids":["x"]}]}]"#).is_err());
    }

    #[test]
    fn optional_fields_are_always_reported() {
        let mut s = default_specs();
        assert_eq!(reported_fields(&to_json(&s)), ["apps", "apply_gain", "virtuals"], "defaults are still written");
        s[0].apps.push(AppSpec { bundle_ids: vec!["com.spotify.client".into()], exclusive: true });
        s[0].apply_gain = false;
        assert_eq!(parse(&to_json(&s)).unwrap(), s);
        // What earlier plug-in versions report.
        let v02 = r#"[{"uid":"a","name":"A","channels":2,"pass_thru":2,"sample_rate":48000.0}]"#;
        let v03 = r#"[{"uid":"a","name":"A","channels":2,"pass_thru":2,"sample_rate":48000.0,"apps":[]}]"#;
        assert!(reported_fields(v02).is_empty());
        assert_eq!(reported_fields(v03), ["apps"]);
        let v031 = r#"[{"uid":"a","name":"A","apps":[],"apply_gain":true}]"#;
        assert_eq!(reported_fields(v031), ["apps", "apply_gain"]);
        assert!(parse(v02).unwrap()[0].apps.is_empty() && parse(v02).unwrap()[0].apply_gain);
        assert!(reported_fields("not json").is_empty());
        assert!(reported_fields("[]").is_empty());
    }

    #[test]
    fn virtual_devices() {
        let mut s = default_specs();
        s[0].virtuals = vec![
            VirtualSpec { id: "comms".into(), name: "Comms".into(), channels: 2, output: false },
            VirtualSpec { id: "daw-out".into(), name: "Daw Output".into(), channels: 16, output: true },
        ];
        assert_eq!(parse(&to_json(&s)).unwrap(), s);
        assert_eq!(s[0].virtuals[0].uid(&s[0].uid), format!("{DEFAULT_UID}/comms"));
        let mut other = s.clone();
        other[0].virtuals[1].channels = 8;
        assert!(s[0].layout_differs(&other[0]));
        let bad = |v: VirtualSpec| {
            let mut b = default_specs();
            b[0].virtuals = vec![v.clone(), VirtualSpec { id: "x".into(), ..v }];
            parse(&to_json(&b))
        };
        assert!(bad(VirtualSpec { id: "x".into(), name: "X".into(), channels: 2, output: false }).is_err(), "repeated id");
        assert!(bad(VirtualSpec { id: "a/b".into(), name: "X".into(), channels: 2, output: false }).is_err(), "bad id");
        assert!(bad(VirtualSpec { id: "y".into(), name: "X".into(), channels: 0, output: false }).is_err(), "no channels");
    }

    #[test]
    fn writes_only_what_the_plugin_knows() {
        let plain = default_specs();
        let v02 = to_json_for(&plain, &[]).unwrap();
        assert!(!v02.contains("apps") && !v02.contains("apply_gain"), "{v02}");
        assert_eq!(parse(&v02).unwrap(), plain);
        assert!(to_json_for(&plain, &["apps"]).unwrap().contains("\"apps\":[]"));
        let mut needs_gain = plain.clone();
        needs_gain[0].apply_gain = false;
        assert!(to_json_for(&needs_gain, &["apps"]).unwrap_err().contains("apply_gain"));
        assert_eq!(parse(&to_json_for(&needs_gain, &OPTIONAL_FIELDS).unwrap()).unwrap(), needs_gain);
    }
}
