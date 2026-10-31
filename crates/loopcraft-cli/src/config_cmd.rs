//! `loopcraft config …`: create and check the YAML configuration.

use std::path::PathBuf;

use lc_config::{find_device, HardwareDevice, InputKind, OutputKind, UID_PREFIX};
use lc_spec::{DeviceSpec, BUNDLE_ID, DEVICES_PROPERTY};

use crate::{ca, Failure, Result};

const HEADER: &str = "\
# LoopCraft configuration. loopcraftd applies saved changes automatically;
# check an edit with `loopcraft config check`.
#
# Each device is a small mixer: inputs are wired onto busses, recording apps
# receive the busses, and busses are wired to hardware outputs. (macOS sees it
# from the other side: virtual inputs are the device's output channels and
# busses its input channels.) Channels are numbered from 1. Hardware devices
# are matched by exact UID, then exact name, then a part of the name that only
# one device has.
#
# Each device:
#   name:           shown in apps
#   id:             stable ID; apps remember the device by it. Don't change it later.
#   busses:         what recording apps receive, a stereo bus each unless mono:
#                     - Program               # 1/2  (or {id: b3f9a2c1d, name: Program})
#                     - {name: Mic, mono: true}   # 3   (wired by name alone: {1: Mic})
#                     - Music                 # 4/5
#                   (a plain number, busses: 10, is 10 numbered channels)
#   groups:         bus groups the editor shows as units of their own:
#                     [{name: DAW}], and a bus joins one with group: DAW
#   virtual_inputs: channels apps can play into it (0 = record-only)
#   sample_rate:    44100, 48000, 88200, 96000, 176400 or 192000
#   buffer_frames:  routing IO buffer (16-4096); each pass costs 2 buffers of delay
#   inputs:         what is wired onto the busses (a bus by name or id), e.g.
#                     - virtual: true         # what apps play into this device
#                       to: Program           # stereo: left to left, right to right
#                     - device: Rane FOUR     # a hardware input
#                       wires: {5: Music.left, 6: Music.right}   # channel: bus side
#                     - app: Spotify          # an app, by name or bundle ID
#                       to: Music
#                       mute_original: true   # leave it out of what it plays elsewhere
#   outputs:        hardware outputs the busses are sent to, e.g.
#                     - device: PRO X Wireless Gaming Headset
#                       from: [Program, Music]   # each to channels 1/2
#                       gain: -3              # dB
#                       follow_volume: true   # also follows the macOS volume (keys, wheel)
#   Virtual devices of their own (\"<mixer name>: <name>\" in macOS, on the
#   mixer's clock):
#                     inputs:  - virtual: Comms       # playback-only (e.g. Discord)
#                                channels: 2
#                                to: Program
#                     outputs: - virtual: Daw Output  # record-only (e.g. a DAW's input)
#                                channels: 16
#                                wires: {Music.left: 1, Music.right: 2}   # bus side: channel
#   Busses, inputs and outputs get ids (the editor writes them in); they never
#   change, so renaming or reordering keeps every patch. Older files with bus
#   channel numbers in wires are still read.
# Inputs and outputs also accept enabled: false, muted: true and gain: in dB
# (0 = unity, the default; -inf = silent; at most +12). An output with
# follow_volume: true also follows the device's own volume (the macOS slider
# and volume keys); the others stay at their gain.
";

fn target(path: Option<&str>) -> PathBuf {
    path.map(PathBuf::from).unwrap_or_else(lc_config::default_path)
}

pub fn show_path() -> Result<()> {
    let p = lc_config::default_path();
    println!("{}{}", p.display(), if p.exists() { "" } else { "  (does not exist yet)" });
    Ok(())
}

/// Writes a config describing the devices the plug-in has now, so that
/// starting the daemon changes nothing.
pub fn init(path: Option<&str>) -> Result<()> {
    let path = target(path);
    if path.exists() {
        return Err(Failure::Error(format!("{} already exists; not overwriting it", path.display())));
    }
    let plugin = ca::plugin(BUNDLE_ID).ok_or_else(|| Failure::Error("the LoopCraft plug-in is not loaded".into()))?;
    let specs = lc_spec::parse(&ca::get_string(plugin, DEVICES_PROPERTY, ca::Scope::Global)?).map_err(Failure::Error)?;
    let text = render(&specs).map_err(Failure::Error)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| Failure::Error(format!("cannot create {}: {e}", dir.display())))?;
    }
    std::fs::write(&path, text).map_err(|e| Failure::Error(format!("cannot write {}: {e}", path.display())))?;
    println!("wrote {} with {} device(s): {}", path.display(), specs.len(), names(&specs));
    Ok(())
}

fn names(specs: &[DeviceSpec]) -> String {
    specs.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join(", ")
}

/// YAML for a device list; checked to read back as exactly that list.
fn render(specs: &[DeviceSpec]) -> std::result::Result<String, String> {
    let mut out = String::from(HEADER);
    if specs.is_empty() {
        out.push_str("devices: []\n");
    } else {
        out.push_str("devices:\n");
    }
    for s in specs {
        let id = s.uid.strip_prefix(UID_PREFIX).ok_or_else(|| format!("device '{}' has a foreign UID {}", s.name, s.uid))?;
        out.push_str(&format!(
            "  - name: {}\n    id: {}\n    busses: {}\n    virtual_inputs: {}\n    sample_rate: {}\n",
            quote(&s.name),
            quote(id),
            s.channels,
            s.pass_thru,
            s.sample_rate as u32
        ));
    }
    let back = lc_config::parse(&out).map_err(|e| format!("generated config does not parse: {e}"))?;
    // App slots come from app inputs, which a fresh config doesn't have yet.
    // Likewise virtual devices and volume following, which come from inputs and outputs.
    let without_apps: Vec<DeviceSpec> = specs.iter().map(|s| DeviceSpec { apps: vec![], virtuals: vec![], apply_gain: true, ..s.clone() }).collect();
    if back.to_specs() != without_apps {
        return Err("generated config does not describe the current devices".into());
    }
    Ok(out)
}

fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Validates the config and checks its hardware references against the
/// devices present now. Missing hardware is a warning: it may be unplugged.
pub fn check(path: Option<&str>) -> Result<()> {
    let path = target(path);
    let config = lc_config::load(&path).map_err(|e| Failure::Error(format!("{}: {e}", path.display())))?;
    println!("{}: valid, {} device(s)", path.display(), config.devices.len());

    // Other LoopCraft devices can be inputs and outputs too (chaining).
    let present: Vec<ca::Device> = ca::devices()?;
    let with = |pick: fn(&ca::Device) -> u32| -> Vec<(HardwareDevice, u32)> {
        present
            .iter()
            .filter(|d| pick(d) > 0)
            .map(|d| (HardwareDevice { uid: d.uid.clone(), name: d.name.clone() }, pick(d)))
            .collect()
    };
    let inputs = with(|d| d.inputs);
    let outputs = with(|d| d.outputs);

    let mut warnings = 0;
    for d in &config.devices {
        let buffer = d.buffer_frames.map_or(String::new(), |b| format!(", buffer {b} frames"));
        println!("  {}  ({}, {} busses / {} virtual inputs, {} Hz{buffer})", d.name, d.uid(), d.busses, d.virtual_inputs, d.sample_rate);
        let lines = d
            .inputs
            .iter()
            .map(|s| (&s.kind, &s.wires, s.enabled))
            .map(|(kind, wires, enabled)| match kind {
                InputKind::Virtual => ("input virtual".to_string(), None, wires, enabled),
                InputKind::VirtualDevice(v) => (format!("input virtual '{}' ({} ch, device \"{}: {}\")", v.name, v.channels, d.name, v.name), None, wires, enabled),
                InputKind::Device(n) => (format!("input device '{n}'"), Some((n, &inputs, "input")), wires, enabled),
                InputKind::App { name, mute_original, tap } => {
                    let how = match (*tap, *mute_original) {
                        (true, true) => " (tap, muted at its own output)",
                        (true, false) => " (tap)",
                        (false, true) => " (left out of the virtual inputs)",
                        (false, false) => "",
                    };
                    (format!("input app '{name}'{how}"), None, wires, enabled)
                }
            })
            .chain(d.outputs.iter().map(|o| {
                let follows = if o.follow_volume { " (follows the volume)" } else { "" };
                match &o.kind {
                    OutputKind::Device(dev) => (format!("output device '{dev}'{follows}"), Some((dev, &outputs, "output")), &o.wires, o.enabled),
                    OutputKind::Virtual(v) => {
                        (format!("output virtual '{}' ({} ch, device \"{}: {}\"){follows}", v.name, v.channels, d.name, v.name), None, &o.wires, o.enabled)
                    }
                }
            }));
        for (label, hw, wires, enabled) in lines {
            let pairs: Vec<String> = wires.iter().map(|w| format!("{}→{}", w.from, w.to)).collect();
            let mut line = format!("    {label}: {}{}", pairs.join(" "), if enabled { "" } else { "  [disabled]" });
            if let Some((query, list, dir)) = hw {
                let own = d.uid();
                let devices: Vec<HardwareDevice> = list.iter().map(|(h, _)| h.clone()).filter(|h| h.uid != own).collect();
                match find_device(query, &devices) {
                    Ok(found) => {
                        let count = list.iter().find(|(h, _)| h.uid == found.uid).map_or(0, |(_, c)| *c);
                        line.push_str(&format!("  = {} ({count} {dir} ch)", found.name));
                        let hw_side = |w: &lc_config::Wire| if dir == "input" { w.from } else { w.to };
                        if let Some(w) = wires.iter().find(|w| hw_side(w) > count) {
                            line.push_str(&format!("\n      warning: it has no {dir} channel {}", hw_side(w)));
                            warnings += 1;
                        }
                    }
                    Err(e) => {
                        line.push_str(&format!("\n      warning: no {dir} device '{query}' right now: {e}"));
                        warnings += 1;
                    }
                }
            }
            println!("{line}");
        }
    }
    if warnings > 0 {
        println!("{warnings} warning(s); the configuration itself is valid");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_a_config_that_reads_back() {
        let specs = vec![
            DeviceSpec { uid: lc_spec::DEFAULT_UID.into(), name: "LoopCraft \"Loop\"back".into(), channels: 2, pass_thru: 2, sample_rate: 48_000.0, apps: vec![], apply_gain: true, virtuals: vec![] },
            DeviceSpec { uid: format!("{UID_PREFIX}desk"), name: "Desktop".into(), channels: 10, pass_thru: 0, sample_rate: 44_100.0, apps: vec![], apply_gain: true, virtuals: vec![] },
        ];
        let text = render(&specs).unwrap();
        assert!(text.contains("id: \"8E0F3C52-6B7A-4C1E-9D2F-3A5B7C9E1F20\""), "{text}");
        assert_eq!(render(&[]).unwrap().lines().last(), Some("devices: []"));
        assert!(render(&[DeviceSpec { uid: "other:x".into(), ..specs[1].clone() }]).is_err());
    }

    #[test]
    fn missing_path_is_reported() {
        assert!(check(Some("/nonexistent/config.yaml")).is_err());
    }
}
