//! From a configured device and the devices present now to an engine layout:
//! which devices join the aggregate, and the mix plan for its IO cycle.
//!
//! Routes that can't work right now (device missing, different sample rate,
//! channel that doesn't exist) are skipped with a warning; the rest still run.
//! A member's sample rate is never changed. App inputs become taps, whose
//! stereo input streams follow all the devices' input streams.

use lc_config::{find_device, HardwareDevice, InputKind, OutputKind};
use lc_spec::AppSpec;

use crate::mix::{gain, Plan, Route};

/// A device as the engine sees it: its stream layout in both directions.
#[derive(Clone, Debug, PartialEq)]
pub struct Member {
    pub uid: String,
    pub name: String,
    pub rate: f64,
    /// Channels of each input / output stream, in order.
    pub inputs: Vec<usize>,
    pub outputs: Vec<usize>,
}

/// A tap capturing one app source.
#[derive(Clone, Debug, PartialEq)]
pub struct AppTap {
    pub name: String,
    pub bundle_ids: Vec<String>,
    pub mute_original: bool,
}

#[derive(Debug, PartialEq)]
pub struct Layout {
    /// The aggregate's members; the twin is first and owns the clock.
    pub members: Vec<Member>,
    pub taps: Vec<AppTap>,
    pub plan: Plan,
    pub warnings: Vec<String>,
}

/// Where a route's audio comes from or goes to, before buffer indices exist.
#[derive(Clone, Copy)]
enum Origin {
    Member(usize),
    Tap(usize),
}

fn same(a: Origin, b: Origin) -> bool {
    match (a, b) {
        (Origin::Member(x), Origin::Member(y)) | (Origin::Tap(x), Origin::Tap(y)) => x == y,
        _ => false,
    }
}

/// An output's wires as (bus channel, output channel, gain): its own, and
/// a MIX's as every bus channel the MIX sums.
fn output_wires(device: &lc_config::Device, o: &lc_config::Output) -> Vec<(u32, u32, f32)> {
    let mut out: Vec<(u32, u32, f32)> = o.wires.iter().map(|w| (w.from, w.to, 1.0)).collect();
    for w in &o.mix_wires {
        out.extend(device.mix_sources(w.group.as_deref(), w.from).into_iter().map(|(bus, gain)| (bus, w.to, gain as f32)));
    }
    out
}

/// Channels of a tap's stream (a stereo mixdown).
const TAP_CHANNELS: usize = 2;

/// Does this device need an engine at all? Without inputs and outputs it
/// keeps the plug-in's direct pass-through (virtual input n → bus n).
pub fn wants_engine(device: &lc_config::Device) -> bool {
    !device.inputs.is_empty() || !device.outputs.is_empty()
}

/// The app slots the plug-in should give this device: one per distinct
/// (app, mute_original) among its app inputs that aren't tapped, in config
/// order. Disabled inputs keep their slot so enabling one doesn't reshape the
/// device. Apps that can't be resolved get none (the planner warns).
pub fn app_slots(device: &lc_config::Device, apps: &dyn Fn(&str) -> Result<Vec<String>, String>) -> Vec<AppSpec> {
    let mut slots: Vec<AppSpec> = Vec::new();
    if device.virtual_inputs == 0 {
        return slots;
    }
    for input in &device.inputs {
        if let InputKind::App { name, mute_original, tap: false } = &input.kind {
            if let Ok(bundle_ids) = apps(name) {
                let slot = AppSpec { bundle_ids, exclusive: *mute_original };
                if !slots.contains(&slot) && slots.len() < lc_spec::MAX_APPS {
                    slots.push(slot);
                }
            }
        }
    }
    slots
}

/// Position of a virtual device among those of one direction.
fn virtual_device_index(device: &lc_config::Device, id: &str, output: bool) -> usize {
    device.virtual_specs().iter().filter(|v| v.output == output).position(|v| v.id == id).unwrap_or(0)
}

/// 1-based device channel → (stream index, channel within the stream).
fn locate(streams: &[usize], channel: u32) -> Option<(usize, usize)> {
    let mut first = 1u32;
    for (i, &n) in streams.iter().enumerate() {
        let n = n as u32;
        if (first..first + n).contains(&channel) {
            return Some((i, (channel - first) as usize));
        }
        first += n;
    }
    None
}

/// What the installed plug-in does for a device.
pub struct Caps<'a> {
    /// The device's app slots (empty with a plug-in that can't capture apps,
    /// in which case every app input is tapped).
    pub slots: &'a [AppSpec],
    /// Whether the plug-in can leave the device volume to following outputs.
    pub follow_volume: bool,
    /// Whether the plug-in has virtual devices.
    pub virtuals: bool,
}

/// The engine twin's streams, in the plug-in's order: inputs are the virtual
/// inputs, the virtual input devices, then the app slots; outputs are the
/// busses, then the virtual output devices.
struct TwinStreams {
    first_virtual_device: usize,
    first_app: usize,
}

impl TwinStreams {
    /// The stream positions, and how many input and output streams there are.
    fn new(device: &lc_config::Device, caps: &Caps) -> (Self, usize, usize) {
        let first_virtual_device = usize::from(device.virtual_inputs > 0);
        let specs = if caps.virtuals { device.virtual_specs() } else { vec![] };
        let playback = specs.iter().filter(|v| !v.output).count();
        let s = TwinStreams { first_virtual_device, first_app: first_virtual_device + playback };
        let inputs = s.first_app + caps.slots.len();
        (s, inputs, 1 + specs.len() - playback)
    }
}

/// `present` is every other device that could join (not the twin, not the
/// device itself); `apps` turns an app input into its bundle IDs; `caps` is
/// what the installed plug-in does. Returns `Err` when the twin isn't ready
/// for an engine.
pub fn layout(
    device: &lc_config::Device,
    twin: &Member,
    present: &[Member],
    apps: &dyn Fn(&str) -> Result<Vec<String>, String>,
    caps: &Caps,
) -> Result<Layout, String> {
    let slots = caps.slots;
    let (streams, twin_inputs, twin_outputs) = TwinStreams::new(device, caps);
    if twin.outputs.len() != twin_outputs || twin.outputs[0] != device.busses as usize || twin.inputs.len() != twin_inputs {
        return Err(format!(
            "device '{}' is not ready yet (its engine side has inputs {:?} and outputs {:?})",
            device.name, twin.inputs, twin.outputs
        ));
    }
    let mut members = vec![twin.clone()];
    let mut taps: Vec<AppTap> = Vec::new();
    let mut warnings = Vec::new();
    // (origin, stream, channel, mix channel, gain), resolved to buffers at the end.
    let mut sources = Vec::new();
    let mut monitors = Vec::new();
    // What each configured input's and output's meters read: (origin, stream, channel).
    let mut meter_in: Vec<Vec<(Origin, usize, usize)>> = vec![Vec::new(); device.inputs.len()];
    let mut meter_out: Vec<Vec<(Origin, usize, usize)>> = vec![Vec::new(); device.outputs.len()];

    let join = |members: &mut Vec<Member>, query: &str, input: bool, at: &str, warnings: &mut Vec<String>| -> Option<usize> {
        let candidates: Vec<&Member> = present.iter().filter(|m| if input { !m.inputs.is_empty() } else { !m.outputs.is_empty() }).collect();
        let hw: Vec<HardwareDevice> = candidates.iter().map(|m| HardwareDevice { uid: m.uid.clone(), name: m.name.clone() }).collect();
        let dir = if input { "input" } else { "output" };
        let found = match find_device(query, &hw) {
            Ok(h) => candidates.iter().find(|m| m.uid == h.uid).copied()?,
            Err(e) => {
                warnings.push(format!("{at}: no {dir} device '{query}' right now ({e}); skipped"));
                return None;
            }
        };
        if found.rate != twin.rate {
            warnings.push(format!(
                "{at}: '{}' runs at {} Hz but '{}' at {} Hz; skipped (LoopCraft never changes a device's sample rate)",
                found.name, found.rate, device.name, twin.rate
            ));
            return None;
        }
        Some(members.iter().position(|m| m.uid == found.uid).unwrap_or_else(|| {
            members.push(found.clone());
            members.len() - 1
        }))
    };

    for (i, s) in device.inputs.iter().enumerate().filter(|(_, s)| s.enabled) {
        let at = format!("'{}' input {}", device.name, i + 1);
        let origin = match &s.kind {
            InputKind::Virtual => Origin::Member(0),
            // A virtual input device: the twin input stream after the virtual inputs.
            InputKind::VirtualDevice(v) => {
                if !caps.virtuals {
                    warnings.push(format!("{at}: virtual devices need a newer LoopCraft plug-in; skipped"));
                    continue;
                }
                let k = virtual_device_index(device, &v.id, false);
                let g = if s.muted { 0.0 } else { gain(s.gain_db) };
                for w in &s.wires {
                    sources.push((Origin::Member(0), streams.first_virtual_device + k, w.from as usize - 1, w.to as usize - 1, g, false));
                }
                meter_in[i] = (0..v.channels as usize).map(|c| (Origin::Member(0), streams.first_virtual_device + k, c)).collect();
                continue;
            }
            InputKind::Device(name) => match join(&mut members, name, true, &at, &mut warnings) {
                Some(m) => Origin::Member(m),
                None => continue,
            },
            InputKind::App { name, mute_original, tap } => match apps(name) {
                // Captured in the plug-in: the twin's app stream for its slot.
                Ok(bundle_ids) if !tap && slots.contains(&AppSpec { bundle_ids: bundle_ids.clone(), exclusive: *mute_original }) => {
                    let k = slots.iter().position(|s| s.bundle_ids == bundle_ids && s.exclusive == *mute_original).unwrap_or(0);
                    let g = if s.muted { 0.0 } else { gain(s.gain_db) };
                    for w in &s.wires {
                        if w.from > device.virtual_inputs {
                            warnings.push(format!("{at}: '{name}' plays into {} channels, not {}; wire skipped", device.virtual_inputs, w.from));
                        } else {
                            sources.push((Origin::Member(0), streams.first_app + k, w.from as usize - 1, w.to as usize - 1, g, false));
                        }
                    }
                    meter_in[i] = (0..device.virtual_inputs as usize).map(|c| (Origin::Member(0), streams.first_app + k, c)).collect();
                    continue;
                }
                Ok(bundle_ids) => {
                    let tap = AppTap { name: name.clone(), bundle_ids, mute_original: *mute_original };
                    Origin::Tap(taps.iter().position(|t| *t == tap).unwrap_or_else(|| {
                        taps.push(tap);
                        taps.len() - 1
                    }))
                }
                Err(e) => {
                    warnings.push(format!("{at}: {e}; skipped"));
                    continue;
                }
            },
        };
        let (streams, label) = match origin {
            Origin::Member(m) => (members[m].inputs.clone(), members[m].name.clone()),
            Origin::Tap(t) => (vec![TAP_CHANNELS], taps[t].name.clone()),
        };
        // The twin's first stream is the virtual inputs; the others are metered as their own inputs.
        let total = if matches!(s.kind, InputKind::Virtual) { device.virtual_inputs } else { streams.iter().sum::<usize>() as u32 };
        meter_in[i] = (1..=total).filter_map(|n| locate(&streams, n).map(|(stream, c)| (origin, stream, c))).collect();
        let g = if s.muted { 0.0 } else { gain(s.gain_db) };
        for w in &s.wires {
            match locate(&streams, w.from) {
                Some((stream, ch)) => sources.push((origin, stream, ch, w.to as usize - 1, g, false)),
                None => warnings.push(format!("{at}: '{label}' has no input channel {}; wire skipped", w.from)),
            }
        }
    }
    if device.volume_follows_outputs() && !caps.follow_volume {
        warnings.push(format!("'{}': follow_volume needs a newer LoopCraft plug-in; the device volume still applies to everything", device.name));
    }
    for (i, m) in device.outputs.iter().enumerate().filter(|(_, m)| m.enabled) {
        let at = format!("'{}' output {}", device.name, i + 1);
        let hardware = match &m.kind {
            OutputKind::Device(name) => name,
            // A virtual output device: the twin output stream after the busses.
            OutputKind::Virtual(v) => {
                if !caps.virtuals {
                    warnings.push(format!("{at}: virtual devices need a newer LoopCraft plug-in; skipped"));
                    continue;
                }
                let j = virtual_device_index(device, &v.id, true);
                let g = if m.muted { 0.0 } else { gain(m.gain_db) };
                for (from, to, scale) in output_wires(device, m) {
                    monitors.push((Origin::Member(0), 1 + j, to as usize - 1, from as usize - 1, g * scale, m.follow_volume && caps.follow_volume));
                }
                meter_out[i] = (0..v.channels as usize).map(|c| (Origin::Member(0), 1 + j, c)).collect();
                continue;
            }
        };
        let Some(member) = join(&mut members, hardware, false, &at, &mut warnings) else { continue };
        let total = members[member].outputs.iter().sum::<usize>() as u32;
        meter_out[i] = (1..=total)
            .filter_map(|n| locate(&members[member].outputs, n).map(|(stream, c)| (Origin::Member(member), stream, c)))
            .collect();
        let g = if m.muted { 0.0 } else { gain(m.gain_db) };
        let mut missing = Vec::new();
        for (from, to, scale) in output_wires(device, m) {
            match locate(&members[member].outputs, to) {
                Some((stream, ch)) => monitors.push((Origin::Member(member), stream, ch, from as usize - 1, g * scale, m.follow_volume && caps.follow_volume)),
                None if !missing.contains(&to) => missing.push(to),
                None => {}
            }
        }
        for to in missing {
            warnings.push(format!("{at}: '{}' has no output channel {to}; wire skipped", members[member].name));
        }
    }

    let bases = |streams: fn(&Member) -> &Vec<usize>| -> Vec<usize> {
        members.iter().scan(0, |next, m| {
            let base = *next;
            *next += streams(m).len();
            Some(base)
        }).collect()
    };
    let (in_base, out_base) = (bases(|m| &m.inputs), bases(|m| &m.outputs));
    let mut inputs: Vec<usize> = members.iter().flat_map(|m| m.inputs.iter().copied()).collect();
    let tap_base = inputs.len();
    inputs.extend(taps.iter().map(|_| TAP_CHANNELS));
    let outputs: Vec<usize> = members.iter().flat_map(|m| m.outputs.iter().copied()).collect();
    let buffer = |origin: Origin, stream: usize, base: &[usize]| match origin {
        Origin::Member(m) => base[m] + stream,
        Origin::Tap(t) => tap_base + t,
    };
    // A bus reaching the same channel twice (wired, and in a MIX) is one route.
    let mut merged: Vec<(Origin, usize, usize, usize, f32, bool)> = Vec::with_capacity(monitors.len());
    for m in monitors {
        match merged.iter_mut().find(|o| same(o.0, m.0) && (o.1, o.2, o.3, o.5) == (m.1, m.2, m.3, m.5)) {
            Some(o) => o.4 += m.4,
            None => merged.push(m),
        }
    }
    let monitors = merged;
    // Silent (muted, -inf) routes stay in the plan at gain 0: muting and
    // unmuting change only a gain, which the mix fades, never the routing.
    let to_routes = |list: &[(Origin, usize, usize, usize, f32, bool)], base: &[usize]| -> Vec<Route> {
        list.iter()
            .map(|&(origin, stream, ch, mix_ch, gain, follows)| Route { buf: buffer(origin, stream, base), ch, mix_ch, gain, follows })
            .collect()
    };
    // Streams with a route, silent ones included, stay on; the rest are
    // switched off so coreaudiod doesn't move their audio for nothing.
    let mut inputs_used = vec![false; inputs.len()];
    let mut outputs_used = vec![false; outputs.len()];
    outputs_used[out_base[0]] = true;
    for &(origin, stream, ..) in &sources {
        inputs_used[buffer(origin, stream, &in_base)] = true;
    }
    for &(origin, stream, ..) in &monitors {
        outputs_used[buffer(origin, stream, &out_base)] = true;
    }
    let plan = Plan {
        mix_channels: device.busses as usize,
        twin_out: out_base[0],
        inputs,
        outputs,
        inputs_used,
        outputs_used,
        sources: to_routes(&sources, &in_base),
        monitors: to_routes(&monitors, &out_base),
        follows_volume: device.volume_follows_outputs() && caps.follow_volume,
        meter_inputs: meter_in.iter().map(|u| u.iter().map(|&(o, stream, c)| Some((buffer(o, stream, &in_base), c))).collect()).collect(),
        meter_outputs: meter_out.iter().map(|u| u.iter().map(|&(o, stream, c)| Some((buffer(o, stream, &out_base), c))).collect()).collect(),
        meter_gains: device.inputs.iter().map(|s| gain(s.gain_db)).collect(),
    };
    Ok(Layout { members, taps, plan, warnings })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(uid: &str, name: &str, rate: f64, inputs: &[usize], outputs: &[usize]) -> Member {
        Member { uid: uid.into(), name: name.into(), rate, inputs: inputs.to_vec(), outputs: outputs.to_vec() }
    }

    fn device(yaml_body: &str) -> lc_config::Device {
        let yaml = format!("devices:\n  - name: Desktop\n    id: desktop\n    busses: 10\n    virtual_inputs: 2\n{yaml_body}");
        lc_config::parse(&yaml).unwrap().devices.remove(0)
    }

    fn twin() -> Member {
        member("org.kerpo.loopcraft:desktop#engine", "Desktop (LoopCraft engine)", 48_000.0, &[2], &[10])
    }

    fn present() -> Vec<Member> {
        vec![
            // Rane FOUR: inputs split over two streams, like many USB interfaces.
            member("rane", "Rane FOUR", 48_000.0, &[8, 4], &[14]),
            member("prox", "PRO X Wireless Gaming Headset", 48_000.0, &[1], &[2]),
            member("mbp", "MacBook Pro Speakers", 44_100.0, &[], &[2]),
        ]
    }

    fn apps(name: &str) -> Result<Vec<String>, String> {
        match name {
            "Spotify" => Ok(vec!["com.spotify.client".into()]),
            _ => Err(format!("no app named '{name}'")),
        }
    }

    fn r(buf: usize, ch: usize, mix_ch: usize, gain: f32) -> Route {
        Route { buf, ch, mix_ch, gain, follows: false }
    }

    #[test]
    fn reference_setup() {
        let d = device(include_str!("../testdata/desktop-routes.yaml"));
        let l = layout(&d, &twin(), &present(), &apps, &Caps { slots: &[], follow_volume: true, virtuals: true }).unwrap();
        let uids: Vec<&str> = l.members.iter().map(|m| m.uid.as_str()).collect();
        assert_eq!(uids, ["org.kerpo.loopcraft:desktop#engine", "rane", "prox"]);
        // Inputs: twin [2] | rane [8, 4] | prox [1] | Spotify tap [2];
        // outputs: twin [10] | rane [14] | prox [2].
        assert_eq!(l.plan.inputs, [2, 8, 4, 1, 2]);
        assert_eq!(l.plan.outputs, [10, 14, 2]);
        assert_eq!(l.taps, [AppTap { name: "Spotify".into(), bundle_ids: vec!["com.spotify.client".into()], mute_original: false }]);
        assert_eq!(l.plan.inputs_used, [true, true, true, true, true]);
        assert_eq!(l.plan.outputs_used, [true, true, true]);
        assert_eq!(l.plan.twin_out, 0);
        assert_eq!(
            l.plan.sources,
            [
                r(0, 0, 0, 1.0),
                r(0, 1, 1, 1.0),
                r(1, 4, 4, 1.0),  // Rane in 5 → out 5
                r(1, 5, 5, 1.0),  // Rane in 6 → out 6
                r(2, 0, 8, 1.0),  // Rane in 9 is the first channel of its second stream
                r(3, 0, 2, gain(-12.0)), // PRO X mic → out 3 and 4 at -12 dB
                r(3, 0, 3, gain(-12.0)),
                r(4, 0, 6, 1.0), // Spotify left/right → out 7/8
                r(4, 1, 7, 1.0),
            ]
        );
        assert_eq!(l.plan.monitors, [r(1, 12, 8, 1.0), r(1, 13, 9, 1.0), r(2, 0, 6, gain(-3.0)), r(2, 1, 7, gain(-3.0))]);
        let all = l.warnings.join("\n");
        assert!(all.contains("'MacBook Pro Speakers' runs at 44100 Hz"), "{all}");
        assert_eq!(l.warnings.len(), 1, "{all}");
    }

    #[test]
    fn missing_devices_and_channels_are_skipped() {
        let d = device(concat!(
            "    inputs:\n",
            "      - {device: Focusrite, wires: {1: 1}}\n",
            "      - {device: PRO X, wires: {1: 1, 2: 2}}\n",
            "    outputs:\n",
            "      - {device: Rane FOUR, wires: {1: 15}}\n",
        ));
        let l = layout(&d, &twin(), &present(), &apps, &Caps { slots: &[], follow_volume: true, virtuals: true }).unwrap();
        let all = l.warnings.join("\n");
        assert!(all.contains("no input device 'Focusrite' right now"), "{all}");
        assert!(all.contains("has no input channel 2"), "{all}");
        assert!(all.contains("has no output channel 15"), "{all}");
        assert_eq!(l.plan.sources, [r(1, 0, 0, 1.0)]);
        assert!(l.plan.monitors.is_empty());
        // Rane's streams are unused (no wire survived); PRO X's input is used.
        assert_eq!(l.plan.inputs_used, [false, true, false, false]);
        assert_eq!(l.plan.outputs_used, [true, false, false]);
        // Rane joined (for its monitor) even though no wire survived.
        assert_eq!(l.members.len(), 3);
    }

    #[test]
    fn app_inputs() {
        let d = device(concat!(
            "    inputs:\n",
            "      - {app: Spotify, wires: {left: 1, right: 2}, mute_original: true}\n",
            "      - {app: Spotify, wires: {left: 3}, mute_original: true}\n",
            "      - {app: Spotify, wires: {3: 5}}\n",
            "      - {app: Winamp, wires: {1: 1}}\n",
        ));
        let l = layout(&d, &twin(), &present(), &apps, &Caps { slots: &[], follow_volume: true, virtuals: true }).unwrap();
        // One tap per (app, mute) pair; channel 3 doesn't exist on a stereo tap.
        assert_eq!(l.taps.len(), 2);
        assert!(l.taps[0].mute_original && !l.taps[1].mute_original);
        assert_eq!(l.plan.inputs, [2, 2, 2]);
        assert_eq!(l.plan.inputs_used, [false, true, false]);
        assert_eq!(l.plan.sources, [r(1, 0, 0, 1.0), r(1, 1, 1, 1.0), r(1, 0, 2, 1.0)]);
        let all = l.warnings.join("\n");
        assert!(all.contains("'Spotify' has no input channel 3"), "{all}");
        assert!(all.contains("no app named 'Winamp'"), "{all}");
    }

    #[test]
    fn apps_in_plugin_slots() {
        let d = device(concat!(
            "    inputs:\n",
            "      - {virtual: true, wires: {1: 1, 2: 2}}\n",
            "      - {app: Spotify, wires: {left: 3, right: 4}, mute_original: true}\n",
            "      - {app: Spotify, wires: {left: 5}, tap: true}\n",
            "      - {app: Spotify, wires: {3: 9}, mute_original: true}\n",
        ));
        let slots = app_slots(&d, &apps);
        assert_eq!(slots, [AppSpec { bundle_ids: vec!["com.spotify.client".into()], exclusive: true }], "tapped inputs get no slot");
        // The twin has the virtual-input stream plus one app stream.
        let with_app = member("org.kerpo.loopcraft:desktop#engine", "Desktop (LoopCraft engine)", 48_000.0, &[2, 2], &[10]);
        let l = layout(&d, &with_app, &present(), &apps, &Caps { slots: &slots, follow_volume: true, virtuals: true }).unwrap();
        assert_eq!(l.plan.inputs, [2, 2, 2], "virtual inputs, Spotify's slot, the explicit tap");
        assert_eq!(l.taps.len(), 1);
        assert_eq!(l.plan.sources, [r(0, 0, 0, 1.0), r(0, 1, 1, 1.0), r(1, 0, 2, 1.0), r(1, 1, 3, 1.0), r(2, 0, 4, 1.0)]);
        assert!(l.warnings.join("\n").contains("plays into 2 channels, not 3"), "{:?}", l.warnings);
        // A twin without the app stream yet isn't ready.
        assert!(layout(&d, &twin(), &present(), &apps, &Caps { slots: &slots, follow_volume: true, virtuals: true }).is_err());
        // No slots for a record-only device.
        let rec = lc_config::parse("devices:\n  - {name: R, id: r, virtual_inputs: 0, inputs: [{app: Spotify, wires: {1: 1}}]}\n").unwrap();
        assert!(app_slots(&rec.devices[0], &apps).is_empty());
    }

    #[test]
    fn virtual_devices_are_twin_streams() {
        let yaml = concat!(
            "devices:\n  - name: Desktop\n    id: desktop\n    busses: 8\n    virtual_inputs: 2\n",
            "    inputs:\n",
            "      - {virtual: true, wires: {1: 1, 2: 2}}\n",
            "      - {virtual: Comms, channels: 2, wires: {1: 3, 2: 4}}\n",
            "      - {app: Spotify, wires: {left: 5, right: 6}}\n",
            "    outputs:\n",
            "      - {virtual: Daw Output, channels: 4, wires: {1: 1, 2: 2}, follow_volume: true}\n",
            "      - {device: PRO X, wires: {1: 1}}\n",
        );
        let d = lc_config::parse(yaml).unwrap().devices.remove(0);
        let slots = app_slots(&d, &apps);
        // Twin: inputs = virtual inputs, Comms, Spotify's slot; outputs = busses, Daw Output.
        let full = member("org.kerpo.loopcraft:desktop#engine", "Desktop (LoopCraft engine)", 48_000.0, &[2, 2, 2], &[8, 4]);
        let caps = Caps { slots: &slots, follow_volume: true, virtuals: true };
        let l = layout(&d, &full, &present(), &apps, &caps).unwrap();
        assert!(l.warnings.is_empty(), "{:?}", l.warnings);
        assert_eq!(l.plan.sources, [r(0, 0, 0, 1.0), r(0, 1, 1, 1.0), r(1, 0, 2, 1.0), r(1, 1, 3, 1.0), r(2, 0, 4, 1.0), r(2, 1, 5, 1.0)]);
        let follow = |buf, ch, mix_ch| Route { follows: true, ..r(buf, ch, mix_ch, 1.0) };
        assert_eq!(l.plan.monitors, [follow(1, 0, 0), follow(1, 1, 1), r(2, 0, 0, 1.0)], "Daw Output is twin output 1; the headset comes after");
        assert_eq!(l.plan.twin_out, 0);
        assert_eq!(l.plan.outputs_used, [true, true, true]);
        // Meters: each input's and output's own channels, wired or not.
        let m = |buf, chans: usize| (0..chans).map(|c| Some((buf, c))).collect::<Vec<_>>();
        assert_eq!(l.plan.meter_inputs, [m(0, 2), m(1, 2), m(2, 2)]);
        assert_eq!(l.plan.meter_outputs, [m(1, 4), m(2, 2)], "PRO X is aggregate output buffer 2");
        // A twin without those streams (old plug-in, or not applied yet) isn't ready.
        assert!(layout(&d, &twin(), &present(), &apps, &caps).is_err());
        // With a plug-in that has no virtual devices they are skipped, with a warning.
        let old_twin = member("t", "t", 48_000.0, &[2, 2], &[8]);
        let old = layout(&d, &old_twin, &present(), &apps, &Caps { slots: &slots, follow_volume: true, virtuals: false }).unwrap();
        assert_eq!(old.warnings.iter().filter(|w| w.contains("newer LoopCraft plug-in")).count(), 2);
    }

    #[test]
    fn outputs_following_the_volume() {
        let d = device(concat!(
            "    outputs:\n",
            "      - {device: PRO X, wires: {1: 1, 2: 2}, follow_volume: true}\n",
            "      - {device: Rane FOUR, wires: {1: 9, 2: 10}}\n",
        ));
        let l = layout(&d, &twin(), &present(), &apps, &Caps { slots: &[], follow_volume: true, virtuals: true }).unwrap();
        assert!(l.plan.follows_volume, "decided by the config, not by which routes are audible");
        let follows: Vec<bool> = l.plan.monitors.iter().map(|r| r.follows).collect();
        assert_eq!(follows, [true, true, false, false]);
        // With a plug-in that can't leave the volume to the daemon, nothing
        // follows (it would be applied twice) and the user is told.
        let old = layout(&d, &twin(), &present(), &apps, &Caps { slots: &[], follow_volume: false, virtuals: true }).unwrap();
        assert!(old.plan.monitors.iter().all(|r| !r.follows));
        assert!(old.warnings.iter().any(|w| w.contains("needs a newer LoopCraft plug-in")));
        // Muting the following output keeps the volume watcher: no rebuild.
        let muted = device(concat!(
            "    outputs:\n",
            "      - {device: PRO X, wires: {1: 1, 2: 2}, follow_volume: true, muted: true}\n",
        ));
        let m = layout(&muted, &twin(), &present(), &apps, &Caps { slots: &[], follow_volume: true, virtuals: true }).unwrap();
        assert!(m.plan.follows_volume);
    }

    #[test]
    fn disabled_and_muted_routes() {
        let d = device(concat!(
            "    inputs:\n",
            "      - {virtual: true, wires: {1: 1}, muted: true}\n",
            "      - {device: Rane FOUR, wires: {1: 1}, enabled: false}\n",
            "    outputs:\n",
            "      - {device: PRO X, wires: {1: 1}, gain: -inf}\n",
        ));
        let l = layout(&d, &twin(), &present(), &apps, &Caps { slots: &[], follow_volume: true, virtuals: true }).unwrap();
        assert!(l.warnings.is_empty(), "{:?}", l.warnings);
        // Muted and -inf routes stay in the plan at gain 0, so muting only
        // changes a gain, which the mix fades; disabled ones are left out.
        assert_eq!(l.plan.sources, [r(0, 0, 0, 0.0)]);
        assert_eq!(l.plan.monitors, [r(1, 0, 0, 0.0)], "the headset is output buffer 1 (the Rane is disabled)");
        // Silent routes keep their streams on, so unmuting changes only the plan.
        assert_eq!(l.plan.inputs_used, [true, false]);
        assert_eq!(l.plan.outputs_used, [true, true]);
        // Muted stays in the aggregate (unmuting is glitch-free); disabled does not.
        let uids: Vec<&str> = l.members.iter().map(|m| m.uid.as_str()).collect();
        assert_eq!(uids, ["org.kerpo.loopcraft:desktop#engine", "prox"]);
    }

    #[test]
    fn twin_must_match_the_device() {
        let d = device("    inputs:\n      - {virtual: true, wires: {1: 1}}\n");
        let stale = member("t", "t", 48_000.0, &[2], &[2]);
        assert!(layout(&d, &stale, &present(), &apps, &Caps { slots: &[], follow_volume: true, virtuals: true }).is_err());
        assert!(wants_engine(&d));
        assert!(!wants_engine(&device("")));
    }

    #[test]
    fn a_mix_sums_every_bus() {
        let d = device("    outputs:\n      - {device: Rane FOUR, wires: {mix.left: 1, mix.right: 2, 1: 1}, gain: -6}\n");
        let l = layout(&d, &twin(), &present(), &apps, &Caps { slots: &[], follow_volume: true, virtuals: true }).unwrap();
        let g = gain(-6.0);
        // Bus 1 is wired and in the MIX: one route at twice the gain.
        let mut want = vec![r(1, 0, 0, 2.0 * g)];
        want.extend([2, 4, 6, 8].map(|c| r(1, 0, c, g)));
        want.extend([1, 3, 5, 7, 9].map(|c| r(1, 1, c, g)));
        assert_eq!(l.plan.monitors, want);
        assert!(l.warnings.is_empty(), "{:?}", l.warnings);

        let d = device("    mix_mono: true\n    outputs:\n      - {device: Rane FOUR, wires: {mix: 3}}\n");
        let l = layout(&d, &twin(), &present(), &apps, &Caps { slots: &[], follow_volume: true, virtuals: true }).unwrap();
        assert_eq!(l.plan.monitors, (0..10).map(|c| r(1, 2, c, 0.5)).collect::<Vec<_>>());
    }

    #[test]
    fn locates_channels_across_streams() {
        assert_eq!(locate(&[8, 4], 1), Some((0, 0)));
        assert_eq!(locate(&[8, 4], 8), Some((0, 7)));
        assert_eq!(locate(&[8, 4], 9), Some((1, 0)));
        assert_eq!(locate(&[8, 4], 12), Some((1, 3)));
        assert_eq!(locate(&[8, 4], 13), None);
        assert_eq!(locate(&[], 1), None);
    }
}
