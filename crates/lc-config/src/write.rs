//! Writing a configuration back as YAML, for the editor.
//!
//! The output reads like a hand-written file and parses back to the same
//! [`Config`]. Comments in the original file are not kept: the editor
//! writes its own short header instead.

use std::fmt::Write;

use crate::raw::shorthand_channels;
use crate::{Config, Device, Input, InputKind, Output, OutputKind};

const HEADER: &str = "\
# LoopCraft configuration, written by the LoopCraft editor. Hand edits are
# fine (the editor reloads them), but comments are not kept when the editor
# saves. Gains are in dB: 0 = unity, -inf = silent, at most +12.
";

/// A string as a double-quoted YAML scalar.
fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// A gain in dB as YAML: `0`, `-6`, `3.5`, `-inf`.
fn decibels(db: f64) -> String {
    if db == f64::NEG_INFINITY {
        "-inf".into()
    } else if db.fract() == 0.0 {
        format!("{db:.0}")
    } else {
        let s = format!("{db:.2}");
        s.trim_end_matches('0').to_string()
    }
}

/// How wires name a bus channel: `<bus id>.left` / `.right`, a mono bus by its id.
fn bus_side(d: &Device, c: u32) -> String {
    match d.bus_of(c) {
        Some(k) if d.bus_list[k].mono => d.bus_list[k].id.clone(),
        Some(k) => format!("{}.{}", d.bus_list[k].id, if c == d.bus_channels(k).0 { "left" } else { "right" }),
        None => c.to_string(),
    }
}

/// A bus's channels for people: "7" or "7/8".
fn bus_place(d: &Device, k: usize) -> String {
    let (l, r) = d.bus_channels(k);
    r.map_or(l.to_string(), |r| format!("{l}/{r}"))
}

/// A bus for people: its name, else its channels.
fn bus_label(d: &Device, k: usize) -> String {
    match d.bus_list.get(k) {
        Some(b) if !b.name.is_empty() => b.name.clone(),
        _ => bus_place(d, k),
    }
}

/// The busses a set of bus channels belongs to, in bus order.
fn busses_of(d: &Device, chans: impl Iterator<Item = u32>) -> Vec<usize> {
    let mut ks: Vec<usize> = chans.filter_map(|c| d.bus_of(c)).collect();
    ks.sort_unstable();
    ks.dedup();
    ks
}

/// The wires a `to:`/`from:` over these busses stands for, as (from, to).
fn stereo(d: &Device, ks: &[usize], unit_channels: u32, inbound: bool) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    for &k in ks {
        let (l, r) = d.bus_channels(k);
        let second = if unit_channels == 1 { 1 } else { 2 };
        // Mono bus: both sides of a stereo unit into it, or out of it to both.
        let other = r.unwrap_or(l);
        if inbound {
            out.push((1, l));
            if r.is_some() || unit_channels > 1 {
                out.push((second, other));
            }
        } else {
            out.push((l, 1));
            if r.is_some() || unit_channels > 1 {
                out.push((other, second));
            }
        }
    }
    out.sort_unstable();
    out
}

fn list(items: &[String]) -> String {
    match items {
        [one] => one.clone(),
        many => format!("[{}]", many.join(", ")),
    }
}

/// `to: "<bus>"` when the wires are plain stereo onto busses, else
/// `wires: {1: "<bus>.left", …}`; with the busses' names as a comment.
fn input_wires(out: &mut String, d: &Device, i: &Input) {
    let ks = busses_of(d, i.wires.iter().map(|w| w.to));
    let names: Vec<String> = ks.iter().map(|&k| bus_label(d, k)).collect();
    let comment = if names.is_empty() { String::new() } else { format!("   # → {}", names.join(", ")) };
    let mut have: Vec<(u32, u32)> = i.wires.iter().map(|w| (w.from, w.to)).collect();
    have.sort_unstable();
    have.dedup();
    let channels = shorthand_channels(Some(&i.kind), None, d.virtual_inputs);
    if !ks.is_empty() && have == stereo(d, &ks, channels, true) {
        let ids: Vec<String> = ks.iter().map(|&k| quote(&d.bus_list[k].id)).collect();
        let _ = writeln!(out, "        to: {}{comment}", list(&ids));
        return;
    }
    let app = matches!(i.kind, InputKind::App { .. });
    let mut entries: Vec<(u32, Vec<String>)> = Vec::new();
    for w in &i.wires {
        let to = quote(&bus_side(d, w.to));
        match entries.iter_mut().find(|(f, _)| *f == w.from) {
            Some((_, ts)) => ts.push(to),
            None => entries.push((w.from, vec![to])),
        }
    }
    let parts: Vec<String> = entries
        .iter()
        .map(|(f, ts)| {
            let f = match (app, f) {
                (true, 1) => "left".to_string(),
                (true, 2) => "right".to_string(),
                _ => f.to_string(),
            };
            format!("{f}: {}", list(ts))
        })
        .collect();
    let _ = writeln!(out, "        wires: {{{}}}{comment}", parts.join(", "));
}

/// `from: "<bus>"` when the wires are plain stereo from busses to the first
/// two channels, else `wires: {"<bus>.left": 1, …}`.
fn output_wires(out: &mut String, d: &Device, o: &Output) {
    let ks = busses_of(d, o.wires.iter().map(|w| w.from));
    let names: Vec<String> = ks.iter().map(|&k| bus_label(d, k)).collect();
    let comment = if names.is_empty() { String::new() } else { format!("   # {} →", names.join(", ")) };
    let mut have: Vec<(u32, u32)> = o.wires.iter().map(|w| (w.from, w.to)).collect();
    have.sort_unstable();
    have.dedup();
    let channels = shorthand_channels(None, Some(&o.kind), d.virtual_inputs);
    if !ks.is_empty() && have == stereo(d, &ks, channels, false) {
        let ids: Vec<String> = ks.iter().map(|&k| quote(&d.bus_list[k].id)).collect();
        let _ = writeln!(out, "        from: {}{comment}", list(&ids));
        return;
    }
    let mut entries: Vec<(u32, Vec<String>)> = Vec::new();
    for w in &o.wires {
        match entries.iter_mut().find(|(f, _)| *f == w.from) {
            Some((_, ts)) => ts.push(w.to.to_string()),
            None => entries.push((w.from, vec![w.to.to_string()])),
        }
    }
    let parts: Vec<String> = entries.iter().map(|(f, ts)| format!("{}: {}", quote(&bus_side(d, *f)), list(ts))).collect();
    let _ = writeln!(out, "        wires: {{{}}}{comment}", parts.join(", "));
}

fn common(out: &mut String, enabled: bool, gain_db: f64, muted: bool, mono: &[u32], half: bool) {
    if !mono.is_empty() {
        let list: Vec<String> = mono.iter().map(u32::to_string).collect();
        let _ = writeln!(out, "        mono: [{}]", list.join(", "));
    }
    if half {
        let _ = writeln!(out, "        half: true");
    }
    if gain_db != 0.0 {
        let _ = writeln!(out, "        gain: {}", decibels(gain_db));
    }
    if muted {
        let _ = writeln!(out, "        muted: true");
    }
    if !enabled {
        let _ = writeln!(out, "        enabled: false");
    }
}

fn virtual_lines(out: &mut String, v: &crate::VirtualDevice) {
    let _ = writeln!(out, "      - virtual: {}", quote(&v.name));
    let _ = writeln!(out, "        id: {}", quote(&v.id));
    let _ = writeln!(out, "        channels: {}", v.channels);
    if let Some(icon) = &v.icon {
        let _ = writeln!(out, "        icon: {}", quote(icon));
    }
}

fn input(out: &mut String, d: &Device, i: &Input) {
    match &i.kind {
        InputKind::Virtual => {
            let _ = writeln!(out, "      - virtual: true");
            let _ = writeln!(out, "        id: {}", quote(&i.id));
        }
        InputKind::VirtualDevice(v) => virtual_lines(out, v),
        InputKind::Device(name) => {
            let _ = writeln!(out, "      - device: {}", quote(name));
            let _ = writeln!(out, "        id: {}", quote(&i.id));
        }
        InputKind::App { name, mute_original, tap } => {
            let _ = writeln!(out, "      - app: {}", quote(name));
            let _ = writeln!(out, "        id: {}", quote(&i.id));
            if *mute_original {
                let _ = writeln!(out, "        mute_original: true");
            }
            if *tap {
                let _ = writeln!(out, "        tap: true");
            }
        }
    }
    if i.wires.is_empty() {
        out.push_str("        wires: {}\n");
    } else {
        input_wires(out, d, i);
    }
    common(out, i.enabled, i.gain_db, i.muted, &i.mono, i.half);
}

fn output(out: &mut String, d: &Device, o: &Output) {
    match &o.kind {
        OutputKind::Device(name) => {
            let _ = writeln!(out, "      - device: {}", quote(name));
            let _ = writeln!(out, "        id: {}", quote(&o.id));
        }
        OutputKind::Virtual(v) => virtual_lines(out, v),
    }
    if o.wires.is_empty() {
        out.push_str("        wires: {}\n");
    } else {
        output_wires(out, d, o);
    }
    common(out, o.enabled, o.gain_db, o.muted, &o.mono, o.half);
    if o.follow_volume {
        let _ = writeln!(out, "        follow_volume: true");
    }
}

/// The configuration as YAML that [`crate::parse`] reads back unchanged.
pub fn to_yaml(config: &Config) -> String {
    let mut out = String::from(HEADER);
    if config.devices.is_empty() {
        out.push_str("devices: []\n");
        return out;
    }
    out.push_str("devices:\n");
    for d in &config.devices {
        let _ = writeln!(out, "  - name: {}", quote(&d.name));
        let _ = writeln!(out, "    id: {}", quote(&d.id));
        if !d.groups.is_empty() {
            out.push_str("    groups:                # bus groups, each a unit of its own in the editor\n");
            for g in &d.groups {
                let _ = writeln!(out, "      - {{id: {}, name: {}}}", quote(&g.id), quote(&g.name));
            }
        }
        out.push_str("    busses:                # stereo unless mono; wires name them by id\n");
        for (k, b) in d.bus_list.iter().enumerate() {
            let mut fields = vec![format!("id: {}", quote(&b.id))];
            if !b.name.is_empty() {
                fields.push(format!("name: {}", quote(&b.name)));
            }
            if b.mono {
                fields.push("mono: true".into());
            }
            if let Some(g) = &b.group {
                fields.push(format!("group: {}", quote(g)));
            }
            let entry = format!("{{{}}}", fields.join(", "));
            let _ = writeln!(out, "      - {entry:<44} # {}", bus_place(d, k));
        }
        let _ = writeln!(out, "    virtual_inputs: {}", d.virtual_inputs);
        let _ = writeln!(out, "    sample_rate: {}", d.sample_rate);
        if let Some(b) = d.buffer_frames {
            let _ = writeln!(out, "    buffer_frames: {b}");
        }
        if !d.inputs.is_empty() {
            out.push_str("    inputs:\n");
            for i in &d.inputs {
                input(&mut out, d, i);
            }
        }
        if !d.outputs.is_empty() {
            out.push_str("    outputs:\n");
            for o in &d.outputs {
                output(&mut out, d, o);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use crate::{parse, to_yaml};

    #[test]
    fn round_trips() {
        for text in [
            include_str!("../example.yaml"),
            concat!(
                "devices:\n  - name: 'Mixer \"A\"'\n    id: mixer\n    busses: 10\n    virtual_inputs: 0\n    buffer_frames: 32\n",
                "    inputs:\n",
                "      - {virtual: Comms, channels: 2, icon: Discord, wires: {1: 7, 2: 8}, gain: -3.5, muted: true}\n",
                "      - {app: Spotify, wires: {left: 3, right: 4}, mute_original: true, tap: true, enabled: false}\n",
                "      - {device: PRO X, wires: {1: [5, 6]}, gain: -inf}\n",
                "    outputs:\n",
                "      - {virtual: Daw Send, id: daw-output, channels: 8, wires: {1: 1, 2: 2}}\n",
                "      - {device: Rane FOUR, wires: {1: 9, 2: 10}, gain: 12, follow_volume: true}\n",
            ),
            "devices: []\n",
            "devices:\n  - name: Named\n    id: named\n    busses: [Program, \"Music \\\"Mix\\\"\", DJ]\n    inputs: [{app: Spotify, wires: {left: 3, right: 4}}]\n",
        ] {
            let config = parse(text).unwrap();
            let yaml = to_yaml(&config);
            assert_eq!(parse(&yaml).unwrap(), config, "round trip of:\n{yaml}");
            assert_eq!(to_yaml(&parse(&yaml).unwrap()), yaml, "stable");
        }
    }

    #[test]
    fn reads_like_a_hand_written_file() {
        let c = parse("devices:\n  - {name: M, id: m, inputs: [{app: Spotify, wires: {left: 1, right: 2}}, {device: Mic, wires: {1: [1, 2]}, gain: -6}]}\n").unwrap();
        let yaml = to_yaml(&c);
        let bus = &c.devices[0].bus_list[0].id;
        assert!(yaml.contains(&format!("      - app: \"Spotify\"\n        id: \"{}\"\n        to: \"{bus}\"   # → 1/2\n", c.devices[0].inputs[0].id)), "{yaml}");
        assert!(yaml.contains(&format!("        wires: {{1: [\"{bus}.left\", \"{bus}.right\"]}}   # → 1/2\n        gain: -6\n")), "{yaml}");
        assert!(yaml.contains(&format!("      - {{id: \"{bus}\"}}")), "{yaml}");
        assert!(!yaml.contains("gain: 0"), "unity gain is left out");
    }
}
