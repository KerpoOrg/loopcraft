//! The file's shape as serde sees it, and its validation into the public model.

use std::collections::HashSet;
use std::fmt;

use serde::de::{self, Deserializer, MapAccess, SeqAccess, Visitor};
use serde::Deserialize;

use crate::ids::derived_id;
use crate::{bus_channels, Bus, BusGroup, Device, Input, InputKind, MixWire, Output, OutputKind, RackSpot, VirtualDevice, Wire, MAX_VIRTUALS, MAX_BUFFER_FRAMES, MAX_CHANNELS, MAX_DEVICES, MAX_GAIN_DB, MIN_BUFFER_FRAMES, SAMPLE_RATES};

fn two() -> u32 {
    2
}

fn default_rate() -> u32 {
    48_000
}

fn yes() -> bool {
    true
}

/// A level in dB: a number, optionally with a "dB" suffix, or `-inf`.
#[derive(Clone, Copy, Debug, Default)]
struct Decibels(f64);

impl<'de> Deserialize<'de> for Decibels {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl Visitor<'_> for V {
            type Value = Decibels;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a level in dB, e.g. 0, -6, 3.5 or -inf")
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Decibels, E> {
                if v.is_nan() || v == f64::INFINITY {
                    return Err(E::invalid_value(de::Unexpected::Float(v), &self));
                }
                Ok(Decibels(v))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Decibels, E> {
                Ok(Decibels(v as f64))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Decibels, E> {
                Ok(Decibels(v as f64))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Decibels, E> {
                let s = v.trim();
                let s = s.strip_suffix("dB").or_else(|| s.strip_suffix("db")).or_else(|| s.strip_suffix("DB")).unwrap_or(s).trim();
                match s {
                    "-inf" | "-∞" => Ok(Decibels(f64::NEG_INFINITY)),
                    _ => match s.parse::<f64>() {
                        Ok(x) => self.visit_f64(x),
                        Err(_) => Err(E::invalid_value(de::Unexpected::Str(v), &self)),
                    },
                }
            }
        }
        d.deserialize_any(V)
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    #[serde(default)]
    devices: Vec<RawDevice>,
}

/// Earlier key names are still read, only to say what they are called now.
type Renamed = Option<de::IgnoredAny>;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDevice {
    name: String,
    id: String,
    #[serde(default, alias = "buses")]
    busses: RawBusses,
    #[serde(default)]
    groups: Vec<RawGroup>,
    #[serde(default)]
    mix_mono: bool,
    mixer_rack: Option<RawSpot>,
    #[serde(default = "two")]
    virtual_inputs: u32,
    #[serde(default = "default_rate")]
    sample_rate: u32,
    buffer_frames: Option<u32>,
    #[serde(default)]
    inputs: Vec<RawInput>,
    #[serde(default)]
    outputs: Vec<RawOutput>,
    channels: Renamed,
    pass_thru: Renamed,
    sources: Renamed,
    monitors: Renamed,
}

/// A bus group: `{id: g1a2b3c4d, name: DAW}`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGroup {
    id: Option<String>,
    name: String,
    #[serde(default)]
    mix_mono: bool,
    rack: Option<RawSpot>,
}

/// `rack: 5` (the rack unit, from 1 below the preset unit), or `5L` / `5R`
/// for the left or right half of it.
#[derive(Debug)]
struct RawSpot(String);

impl<'de> Deserialize<'de> for RawSpot {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl Visitor<'_> for V {
            type Value = RawSpot;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a rack unit: a number, or a number and L or R for a half")
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<RawSpot, E> {
                Ok(RawSpot(v.to_string()))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<RawSpot, E> {
                Ok(RawSpot(v.to_string()))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<RawSpot, E> {
                Ok(RawSpot(v.to_string()))
            }
        }
        d.deserialize_any(V)
    }
}

/// A rack spot as written; `half`: the unit is half-width, so it may name a half.
fn rack_spot(raw: Option<RawSpot>, half: bool, at: &str, problems: &mut Vec<String>) -> Option<RackSpot> {
    let raw = raw?;
    let t = raw.0.trim();
    let (digits, right) = match t.char_indices().last() {
        Some((i, 'R' | 'r')) => (&t[..i], Some(true)),
        Some((i, 'L' | 'l')) => (&t[..i], Some(false)),
        _ => (t, None),
    };
    match digits.trim().parse::<u32>() {
        Ok(u) if u >= 1 => {}
        _ => {
            problems.push(format!("{at}: rack: '{t}' is not a rack unit (1, 2, …, or 5L / 5R for a half)"));
            return None;
        }
    }
    if right.is_some() && !half {
        problems.push(format!("{at}: rack: '{t}' names a half, but the unit is full width"));
        return None;
    }
    Some(RackSpot { u: digits.trim().parse().unwrap_or(1), right: right.unwrap_or(false) })
}

/// `busses: 10` (numbered), or a list, one bus each: names (`[Program,
/// Music]`) or entries with ids (`{id: b3f9a2c1d, name: Music}`), stereo
/// unless `mono: true`, in a group with `group: <group id or name>`.
#[derive(Debug)]
enum RawBusses {
    Count(u32),
    List(Vec<RawBus>),
}

#[derive(Debug)]
struct RawBus {
    id: Option<String>,
    name: Option<String>,
    mono: bool,
    group: Option<String>,
    /// Written as a plain name (the older form), which must not be empty.
    bare: bool,
}

impl<'de> Deserialize<'de> for RawBus {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = RawBus;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a bus name, or {id: ..., name: ..., mono: true, group: ...}")
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<RawBus, E> {
                Ok(RawBus { id: None, name: Some(v.to_string()), mono: false, group: None, bare: true })
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<RawBus, A::Error> {
                let mut bus = RawBus { id: None, name: None, mono: false, group: None, bare: false };
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "id" => bus.id = Some(map.next_value()?),
                        "name" => bus.name = Some(map.next_value()?),
                        "mono" => bus.mono = map.next_value()?,
                        "group" => bus.group = Some(map.next_value()?),
                        other => return Err(de::Error::unknown_field(other, &["id", "name", "mono", "group"])),
                    }
                }
                Ok(bus)
            }
        }
        d.deserialize_any(V)
    }
}

impl Default for RawBusses {
    fn default() -> Self {
        RawBusses::Count(two())
    }
}

impl<'de> Deserialize<'de> for RawBusses {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = RawBusses;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a number of busses, or a list of bus names (one stereo bus each)")
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<RawBusses, E> {
                u32::try_from(v).map(RawBusses::Count).map_err(|_| E::custom("too many busses"))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<RawBusses, E> {
                u32::try_from(v).map(RawBusses::Count).map_err(|_| E::custom("busses must be a positive number"))
            }
            fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<RawBusses, A::Error> {
                let mut busses = Vec::new();
                while let Some(b) = seq.next_element::<RawBus>()? {
                    busses.push(b);
                }
                Ok(RawBusses::List(busses))
            }
        }
        d.deserialize_any(V)
    }
}

/// `virtual: true` (the device's own virtual inputs) or `virtual: <name>` (a
/// virtual device of its own).
#[derive(Debug)]
enum VirtualKey {
    Flag(bool),
    Name(String),
}

impl<'de> Deserialize<'de> for VirtualKey {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl Visitor<'_> for V {
            type Value = VirtualKey;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("true, or the name of a virtual device")
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> Result<VirtualKey, E> {
                Ok(VirtualKey::Flag(v))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<VirtualKey, E> {
                Ok(VirtualKey::Name(v.to_string()))
            }
        }
        d.deserialize_any(V)
    }
}

/// Exactly one of `virtual`, `device` and `app` says what kind it is.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawInput {
    #[serde(rename = "virtual")]
    virtual_: Option<VirtualKey>,
    channels: Option<u32>,
    id: Option<String>,
    icon: Option<String>,
    device: Option<String>,
    app: Option<String>,
    #[serde(default)]
    wires: Wires,
    /// Stereo onto busses: input left → bus left, right → right.
    #[serde(default)]
    to: OneOrMany,
    #[serde(default = "yes")]
    enabled: bool,
    #[serde(default)]
    gain: Decibels,
    volume: Renamed,
    #[serde(default)]
    muted: bool,
    mute_original: Option<bool>,
    tap: Option<bool>,
    /// Editor only: channels shown one by one, and a half-width unit.
    #[serde(default)]
    mono: Vec<u32>,
    #[serde(default)]
    half: bool,
    rack: Option<RawSpot>,
    pass_thru: Renamed,
    input: Renamed,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawOutput {
    #[serde(rename = "virtual")]
    virtual_: Option<VirtualKey>,
    channels: Option<u32>,
    id: Option<String>,
    icon: Option<String>,
    device: Option<String>,
    #[serde(default)]
    wires: Wires,
    /// Busses in stereo to the output's first two channels.
    #[serde(default)]
    from: OneOrMany,
    #[serde(default = "yes")]
    enabled: bool,
    #[serde(default)]
    gain: Decibels,
    volume: Renamed,
    #[serde(default)]
    muted: bool,
    #[serde(default)]
    follow_volume: bool,
    #[serde(default)]
    mono: Vec<u32>,
    #[serde(default)]
    half: bool,
    rack: Option<RawSpot>,
    output: Renamed,
}

/// `{from: to}` or `{from: [to, to]}`, in file order. Each end is a channel
/// number, `left`/`right`, or on the bus side a bus: `<bus>.left` /
/// `<bus>.right`, the bus by id or by name. Resolved once the busses are known.
#[derive(Debug, Default)]
struct Wires(Vec<(End, Vec<End>)>);

/// One end of a wire as written.
#[derive(Clone, Debug)]
enum End {
    Number(u32),
    Text(String),
}

impl<'de> Deserialize<'de> for End {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl Visitor<'_> for V {
            type Value = End;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a channel number, left/right, or a bus side like 'Music.left'")
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<End, E> {
                u32::try_from(v).map(End::Number).map_err(|_| E::custom(format!("channel {v} is too large")))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<End, E> {
                match u64::try_from(v) {
                    Ok(v) => self.visit_u64(v),
                    Err(_) => Err(E::custom(format!("channel {v} is negative"))),
                }
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<End, E> {
                Ok(End::Text(v.to_string()))
            }
        }
        d.deserialize_any(V)
    }
}

/// `to: Music` or `to: [Program, Music]`: busses by id or name.
#[derive(Debug, Default)]
struct OneOrMany(Vec<String>);

impl<'de> Deserialize<'de> for OneOrMany {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = OneOrMany;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a bus (by id or name) or a list of busses")
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<OneOrMany, E> {
                Ok(OneOrMany(vec![v.to_string()]))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<OneOrMany, A::Error> {
                let mut out = Vec::new();
                while let Some(s) = seq.next_element::<String>()? {
                    out.push(s);
                }
                Ok(OneOrMany(out))
            }
        }
        d.deserialize_any(V)
    }
}

struct Targets(Vec<End>);

impl<'de> Deserialize<'de> for Targets {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Targets;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a channel or bus side, or a list of them")
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Targets, E> {
                End::deserialize(de::value::U64Deserializer::new(v)).map(|e| Targets(vec![e]))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Targets, E> {
                End::deserialize(de::value::I64Deserializer::new(v)).map(|e| Targets(vec![e]))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Targets, E> {
                Ok(Targets(vec![End::Text(v.to_string())]))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Targets, A::Error> {
                let mut out = Vec::new();
                while let Some(e) = seq.next_element()? {
                    out.push(e);
                }
                Ok(Targets(out))
            }
        }
        d.deserialize_any(V)
    }
}

impl<'de> Deserialize<'de> for Wires {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Wires;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a map of channel: bus side, e.g. {1: Music.left, 2: Music.right}")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Wires, A::Error> {
                let mut out = Vec::new();
                while let Some((from, Targets(to))) = map.next_entry::<End, Targets>()? {
                    out.push((from, to));
                }
                Ok(Wires(out))
            }
        }
        d.deserialize_map(V)
    }
}

/// A device channel as written: a number, quoted digits, or `left`/`right`.
fn device_channel(e: &End) -> Result<u32, String> {
    match e {
        End::Number(n) => Ok(*n),
        End::Text(t) => match t.trim() {
            "left" | "L" => Ok(1),
            "right" | "R" => Ok(2),
            s => s.parse().map_err(|_| format!("'{t}' is not a channel (a number, or left/right)")),
        },
    }
}

/// The busses as wires see them.
struct BusRefs<'a> {
    list: &'a [Bus],
}

impl BusRefs<'_> {
    /// A bus by id, else by name (any case).
    fn find(&self, r: &str) -> Result<usize, String> {
        let r = r.trim();
        if let Some(k) = self.list.iter().position(|b| b.id == r) {
            return Ok(k);
        }
        match self.list.iter().position(|b| !b.name.is_empty() && b.name.eq_ignore_ascii_case(r)) {
            Some(k) => Ok(k),
            None => Err(format!("there is no bus '{r}'")),
        }
    }

    fn left(&self, k: usize) -> u32 {
        bus_channels(self.list, k).0
    }

    /// A MIX as a wire's source: `mix.left` / `mix.right` (or `mix` when
    /// it is mono), a group's as `<group>.mix.left`; the group by id or
    /// name. `None`: the text doesn't name a MIX at all.
    fn mix(&self, t: &str, groups: &[BusGroup], mixer_mono: bool) -> Option<Result<(Option<String>, u32), String>> {
        let t = t.trim();
        let lower = t.to_ascii_lowercase();
        let (head, side) = match lower.rsplit_once('.') {
            Some((h, "left" | "l")) => (&t[..h.len()], Some(1)),
            Some((h, "right" | "r")) => (&t[..h.len()], Some(2)),
            _ => (t, None),
        };
        let group = if head.eq_ignore_ascii_case("mix") {
            None
        } else {
            let g = head.len().checked_sub(4).filter(|&i| head.is_char_boundary(i) && head[i..].eq_ignore_ascii_case(".mix"))?;
            let r = head[..g].trim();
            match groups.iter().find(|x| x.id == r).or_else(|| groups.iter().find(|x| x.name.eq_ignore_ascii_case(r))) {
                Some(x) => Some(x),
                None => return Some(Err(format!("there is no bus group '{r}' for '{t}'"))),
            }
        };
        let mono = group.map_or(mixer_mono, |g| g.mix_mono);
        let what = group.map_or("the mixer's MIX".to_string(), |g| format!("the MIX of '{}'", g.name));
        Some(match (side, mono) {
            (None, true) => Ok((group.map(|g| g.id.clone()), 1)),
            (Some(s), false) => Ok((group.map(|g| g.id.clone()), s)),
            (None, false) => Err(format!("'{t}': {what} is stereo; say '{head}.left' or '{head}.right'")),
            (Some(_), true) => Err(format!("'{t}': {what} is mono; wire from '{head}' itself")),
        })
    }

    fn right(&self, k: usize) -> Option<u32> {
        bus_channels(self.list, k).1
    }

    /// A bus channel as written: a channel number, `<bus>.left` /
    /// `<bus>.right`, or a mono bus by itself.
    fn channel(&self, e: &End) -> Result<u32, String> {
        let t = match e {
            End::Number(n) => return Ok(*n),
            End::Text(t) => t.trim(),
        };
        if let Ok(n) = t.parse() {
            return Ok(n);
        }
        if let Ok(k) = self.find(t) {
            return match self.right(k) {
                None => Ok(self.left(k)),
                Some(_) => Err(format!("'{t}': say which side of the bus, '{t}.left' or '{t}.right'")),
            };
        }
        let Some((r, side)) = t.rsplit_once('.') else {
            return Err(format!("there is no bus '{t}'"));
        };
        let k = self.find(r)?;
        match side.to_ascii_lowercase().as_str() {
            "left" | "l" => Ok(self.left(k)),
            "right" | "r" => self.right(k).ok_or_else(|| format!("bus '{r}' is mono; wire to '{r}' itself")),
            _ => Err(format!("'{t}': the side is .left or .right, not .{side}")),
        }
    }
}

/// Wires as channel numbers, grouped by the channel they come from, for
/// [`wires`]. Problems resolving an end are reported and the end skipped.
fn resolve(
    raw: Wires,
    from: impl Fn(&End) -> Result<u32, String>,
    to: impl Fn(&End) -> Result<u32, String>,
    extra: Vec<(u32, u32)>,
    at: &str,
    problems: &mut Vec<String>,
) -> Vec<(u32, Vec<u32>)> {
    let mut out: Vec<(u32, Vec<u32>)> = Vec::new();
    let mut add = |f: u32, t: Vec<u32>, merge: bool| match out.iter_mut().find(|(g, _)| *g == f) {
        Some((_, ts)) if merge => ts.extend(t),
        _ => out.push((f, t)),
    };
    for (f, ts) in raw.0 {
        let f = match from(&f) {
            Ok(f) => f,
            Err(e) => {
                problems.push(format!("{at}: wires: {e}"));
                continue;
            }
        };
        let resolved: Vec<u32> = ts
            .iter()
            .filter_map(|t| to(t).map_err(|e| problems.push(format!("{at}: wires: {e}"))).ok())
            .collect();
        if resolved.len() == ts.len() {
            add(f, resolved, false);
        }
    }
    for (f, t) in extra {
        add(f, vec![t], true);
    }
    out
}

/// Stereo shorthand onto busses (`to:`), as (input channel, bus channel):
/// left → left and right → right, a mono input onto both sides, both sides
/// of a stereo input into a mono bus.
fn to_pairs(refs: &BusRefs, busses: &[String], source_channels: u32, at: &str, problems: &mut Vec<String>) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    for b in busses {
        match refs.find(b) {
            Ok(k) => {
                out.push((1, refs.left(k)));
                match refs.right(k) {
                    Some(r) => out.push((if source_channels == 1 { 1 } else { 2 }, r)),
                    None if source_channels > 1 => out.push((2, refs.left(k))),
                    None => {}
                }
            }
            Err(e) => problems.push(format!("{at}: to: {e}")),
        }
    }
    out
}

/// Stereo shorthand from busses (`from:`), as (bus channel, output channel).
fn from_pairs(refs: &BusRefs, busses: &[String], output_channels: u32, at: &str, problems: &mut Vec<String>) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    for b in busses {
        match refs.find(b) {
            Ok(k) => {
                out.push((refs.left(k), 1));
                match refs.right(k) {
                    Some(r) => out.push((r, if output_channels == 1 { 1 } else { 2 })),
                    None if output_channels > 1 => out.push((refs.left(k), 2)),
                    None => {}
                }
            }
            Err(e) => problems.push(format!("{at}: from: {e}")),
        }
    }
    out
}

/// Channels a `to:` or `from:` shorthand assumes a unit has: what it is
/// known to have, else stereo.
pub(crate) fn shorthand_channels(input: Option<&InputKind>, output: Option<&OutputKind>, virtual_inputs: u32) -> u32 {
    match (input, output) {
        (Some(InputKind::Virtual), _) => virtual_inputs,
        (Some(InputKind::VirtualDevice(v)), _) | (_, Some(OutputKind::Virtual(v))) => v.channels,
        _ => 2,
    }
}

/// Checks a wire map against the channel counts on both ends (`None` when the
/// count is only known at run time) and flattens it.
fn wires(
    raw: Vec<(u32, Vec<u32>)>,
    from_max: Option<u32>,
    from_what: &str,
    to_max: Option<u32>,
    to_what: &str,
    at: &str,
    problems: &mut Vec<String>,
) -> Vec<Wire> {
    let check = |c: u32, max: Option<u32>, what: &str, problems: &mut Vec<String>| match max {
        _ if c == 0 => {
            problems.push(format!("{at}: wires: channels are numbered from 1, not 0"));
            false
        }
        Some(m) if c > m => {
            problems.push(format!("{at}: wires: {what} channel {c} does not exist (it has {m})"));
            false
        }
        _ => true,
    };
    let mut seen_from = HashSet::new();
    let mut out = Vec::new();
    for (from, targets) in raw {
        if !seen_from.insert(from) {
            problems.push(format!("{at}: wires: {from_what} channel {from} is listed twice"));
            continue;
        }
        if targets.is_empty() {
            problems.push(format!("{at}: wires: {from_what} channel {from} has an empty list"));
        }
        let from_ok = check(from, from_max, from_what, problems);
        for to in targets {
            let wire = Wire { from, to };
            if check(to, to_max, to_what, problems) && from_ok {
                if out.contains(&wire) {
                    problems.push(format!("{at}: wires: {from} → {to} is listed twice"));
                } else {
                    out.push(wire);
                }
            }
        }
    }
    out.sort();
    out
}

/// Wires from a MIX to output channels, checked like [`wires`].
fn mix_wires(
    raw: Vec<(End, Vec<End>)>,
    refs: &BusRefs,
    groups: &[BusGroup],
    mixer_mono: bool,
    to_max: Option<u32>,
    at: &str,
    problems: &mut Vec<String>,
) -> Vec<MixWire> {
    let mut out: Vec<MixWire> = Vec::new();
    for (f, targets) in raw {
        let End::Text(t) = &f else { continue };
        let (group, from) = match refs.mix(t, groups, mixer_mono) {
            Some(Ok(m)) => m,
            Some(Err(e)) => {
                problems.push(format!("{at}: wires: {e}"));
                continue;
            }
            None => continue,
        };
        if out.iter().any(|w| w.group == group && w.from == from) {
            problems.push(format!("{at}: wires: '{}' is listed twice", t.trim()));
            continue;
        }
        if targets.is_empty() {
            problems.push(format!("{at}: wires: '{}' has an empty list", t.trim()));
        }
        for e in &targets {
            match device_channel(e) {
                Ok(0) => problems.push(format!("{at}: wires: channels are numbered from 1, not 0")),
                Ok(c) if to_max.is_some_and(|m| c > m) => {
                    problems.push(format!("{at}: wires: device channel {c} does not exist (it has {})", to_max.unwrap_or(0)));
                }
                Ok(to) => {
                    let w = MixWire { group: group.clone(), from, to };
                    if out.contains(&w) {
                        problems.push(format!("{at}: wires: '{}' → {to} is listed twice", t.trim()));
                    } else {
                        out.push(w);
                    }
                }
                Err(e) => problems.push(format!("{at}: wires: {e}")),
            }
        }
    }
    out.sort();
    out
}

/// `mono: [1, 2]`: real channels, sorted, each once.
fn mono_channels(mut list: Vec<u32>, max: Option<u32>, at: &str, problems: &mut Vec<String>) -> Vec<u32> {
    list.sort_unstable();
    list.dedup();
    list.retain(|&c| match max {
        _ if c == 0 => {
            problems.push(format!("{at}: mono: channels are numbered from 1, not 0"));
            false
        }
        Some(m) if c > m => {
            problems.push(format!("{at}: mono: channel {c} does not exist (it has {m})"));
            false
        }
        _ => true,
    });
    list
}

/// Reports a key under its earlier name.
fn renamed(key: &Renamed, old: &str, new: &str, at: &str, problems: &mut Vec<String>) {
    if key.is_some() {
        problems.push(format!("{at}: '{old}' is now '{new}'"));
    }
}

fn gain(v: Decibels, at: &str, problems: &mut Vec<String>) -> f64 {
    if v.0 > MAX_GAIN_DB {
        problems.push(format!("{at}: gain is in dB (0 = unity, -inf = silent) and at most +{MAX_GAIN_DB}, not {}", v.0));
    }
    v.0.min(MAX_GAIN_DB)
}

/// An ID from a display name: "Daw Output" → "daw-output".
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

/// Checks a virtual device's name, channels and id (unique within the mixer).
fn virtual_device(
    name: String,
    channels: Option<u32>,
    id: Option<String>,
    icon: Option<String>,
    ids: &mut HashSet<String>,
    at: &str,
    problems: &mut Vec<String>,
) -> Option<VirtualDevice> {
    if name.trim().is_empty() {
        problems.push(format!("{at}: a virtual device needs a name"));
        return None;
    }
    let Some(channels) = channels else {
        problems.push(format!("{at}: needs 'channels:', how many channels the device has"));
        return None;
    };
    if !(1..=MAX_CHANNELS).contains(&channels) {
        problems.push(format!("{at}: channels must be 1-{MAX_CHANNELS}, not {channels}"));
        return None;
    }
    let id = id.unwrap_or_else(|| slug(&name));
    if !valid_id(&id) {
        problems.push(format!("{at}: id '{id}' may only contain letters, digits, '-', '_' and '.'; set one with 'id:'"));
        return None;
    }
    if !ids.insert(id.clone()) {
        problems.push(format!("{at}: another virtual device of this mixer has the id '{id}'"));
        return None;
    }
    Some(VirtualDevice { name, id, channels, icon: icon.filter(|i| !i.trim().is_empty()) })
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

pub(crate) fn validate(raw: Config) -> Result<crate::Config, Vec<String>> {
    let mut problems = Vec::new();
    if raw.devices.len() > MAX_DEVICES {
        problems.push(format!("at most {MAX_DEVICES} devices are supported, found {}", raw.devices.len()));
    }
    let mut ids = HashSet::new();
    let mut names = HashSet::new();
    let mut devices = Vec::new();
    for (i, d) in raw.devices.into_iter().enumerate() {
        let at = if d.name.trim().is_empty() { format!("device {}", i + 1) } else { format!("device '{}'", d.name) };
        if d.name.trim().is_empty() {
            problems.push(format!("{at}: name must not be empty"));
        } else if !names.insert(d.name.clone()) {
            problems.push(format!("{at}: another device has the same name"));
        }
        if !valid_id(&d.id) {
            problems.push(format!("{at}: id '{}' may only contain letters, digits, '-', '_' and '.'", d.id));
        } else if !ids.insert(d.id.clone()) {
            problems.push(format!("{at}: another device has the id '{}'", d.id));
        }
        renamed(&d.channels, "channels", "busses", &at, &mut problems);
        renamed(&d.pass_thru, "pass_thru", "virtual_inputs", &at, &mut problems);
        renamed(&d.sources, "sources", "inputs", &at, &mut problems);
        renamed(&d.monitors, "monitors", "outputs", &at, &mut problems);
        // Ids of busses, inputs and outputs: one namespace per mixer.
        let mut unit_ids: HashSet<String> = HashSet::new();
        let mut claim = |id: &str, what: &str, problems: &mut Vec<String>| {
            if !valid_id(id) {
                problems.push(format!("{at}: {what}: id '{id}' may only contain letters, digits, '-', '_' and '.'"));
            } else if !unit_ids.insert(id.to_string()) {
                problems.push(format!("{at}: {what}: another bus, input or output of this mixer has the id '{id}'"));
            }
        };
        let mut groups: Vec<BusGroup> = Vec::new();
        for (k, g) in d.groups.into_iter().enumerate() {
            let name = g.name.trim().to_string();
            if name.is_empty() {
                problems.push(format!("{at}: bus group {} needs a name", k + 1));
            } else if groups.iter().any(|o| o.name.eq_ignore_ascii_case(&name)) {
                problems.push(format!("{at}: two bus groups are called '{name}'"));
            }
            let id = g.id.map(|i| i.trim().to_string()).unwrap_or_else(|| derived_id('g', &format!("{}/group/{k}", d.id)));
            claim(&id, &format!("bus group '{name}'"), &mut problems);
            let rack = rack_spot(g.rack, false, &format!("{at}: bus group '{name}'"), &mut problems);
            groups.push(BusGroup { id, name, mix_mono: g.mix_mono, rack });
        }
        // `busses: 10` is 10 channels (a last odd one a mono bus); a list is a bus each.
        let listed = matches!(d.busses, RawBusses::List(_));
        let raw_busses: Vec<RawBus> = match d.busses {
            RawBusses::Count(n) => (0..n.div_ceil(2))
                .map(|k| RawBus { id: None, name: None, mono: 2 * k + 1 == n, group: None, bare: false })
                .collect(),
            RawBusses::List(list) => list,
        };
        let mut seen_names = HashSet::new();
        let mut bus_list = Vec::new();
        let mut next = 1;
        for (k, b) in raw_busses.into_iter().enumerate() {
            let place = if b.mono { format!("bus {next}") } else { format!("bus {}/{}", next, next + 1) };
            next += if b.mono { 1 } else { 2 };
            let name = b.name.unwrap_or_default().trim().to_string();
            if b.bare && name.is_empty() {
                problems.push(format!("{at}: {place} needs a name"));
            } else if !name.is_empty() && !seen_names.insert(name.to_lowercase()) {
                problems.push(format!("{at}: two busses are called '{name}'"));
            } else if name.eq_ignore_ascii_case("mix") {
                problems.push(format!("{at}: {place}: 'mix' is what wires call the mixer's MIX; give the bus another name"));
            }
            let id = b.id.map(|i| i.trim().to_string()).unwrap_or_else(|| derived_id('b', &format!("{}/bus/{k}", d.id)));
            if id.eq_ignore_ascii_case("mix") {
                problems.push(format!("{at}: {place}: 'mix' is what wires call the mixer's MIX; give the bus another id"));
            }
            claim(&id, &place, &mut problems);
            let group = b.group.and_then(|r| {
                let r = r.trim();
                let found = groups.iter().find(|g| g.id == r).or_else(|| groups.iter().find(|g| g.name.eq_ignore_ascii_case(r)));
                if found.is_none() {
                    problems.push(format!("{at}: {place}: there is no bus group '{r}'"));
                }
                found.map(|g| g.id.clone())
            });
            bus_list.push(Bus { id, name, mono: b.mono, group });
        }
        let busses: u32 = bus_list.iter().map(Bus::channels).sum();
        if !(1..=MAX_CHANNELS).contains(&busses) {
            match listed {
                false => problems.push(format!("{at}: busses must be 1-{MAX_CHANNELS}, not {busses}")),
                true => problems.push(format!("{at}: the busses have 1-{MAX_CHANNELS} channels in all, not {busses}")),
            }
        }
        let refs = BusRefs { list: &bus_list };
        if d.virtual_inputs > MAX_CHANNELS {
            problems.push(format!("{at}: virtual_inputs must be 0-{MAX_CHANNELS}, not {}", d.virtual_inputs));
        }
        if !SAMPLE_RATES.contains(&f64::from(d.sample_rate)) {
            let rates: Vec<String> = SAMPLE_RATES.iter().map(|r| r.to_string()).collect();
            problems.push(format!("{at}: sample_rate must be one of {}, not {}", rates.join(", "), d.sample_rate));
        }
        if let Some(b) = d.buffer_frames.filter(|b| !(MIN_BUFFER_FRAMES..=MAX_BUFFER_FRAMES).contains(b)) {
            problems.push(format!("{at}: buffer_frames must be {MIN_BUFFER_FRAMES}-{MAX_BUFFER_FRAMES}, not {b}"));
        }

        let mut inputs = Vec::new();
        let mut has_virtual = false;
        let mut virtual_ids = HashSet::new();
        for (j, s) in d.inputs.into_iter().enumerate() {
            let at = format!("{at}, input {}", j + 1);
            renamed(&s.volume, "volume", "gain", &at, &mut problems);
            renamed(&s.pass_thru, "pass_thru: true", "virtual: true", &at, &mut problems);
            renamed(&s.input, "input:", "device:", &at, &mut problems);
            let named = matches!(s.virtual_, Some(VirtualKey::Name(_)));
            if !named && (s.channels.is_some() || s.icon.is_some()) {
                problems.push(format!("{at}: 'channels' and 'icon' only apply to a virtual device ('virtual: <name>')"));
            }
            let given_id = s.id.clone();
            let kind = match (s.virtual_, s.device, s.app) {
                (Some(VirtualKey::Flag(true)), None, None) => InputKind::Virtual,
                (Some(VirtualKey::Flag(false)), None, None) => {
                    problems.push(format!("{at}: 'virtual: false' is not an input; use 'enabled: false' to switch it off"));
                    continue;
                }
                (Some(VirtualKey::Name(name)), None, None) => {
                    match virtual_device(name, s.channels, s.id, s.icon, &mut virtual_ids, &at, &mut problems) {
                        Some(v) => InputKind::VirtualDevice(v),
                        None => continue,
                    }
                }
                (None, Some(device), None) => InputKind::Device(device),
                (None, None, Some(app)) => InputKind::App { name: app, mute_original: s.mute_original.unwrap_or(false), tap: s.tap.unwrap_or(false) },
                (None, None, None) => {
                    if s.pass_thru.is_none() && s.input.is_none() {
                        problems.push(format!("{at}: needs one of 'virtual: true', 'virtual: <name>', 'device:' or 'app:'"));
                    }
                    continue;
                }
                _ => {
                    problems.push(format!("{at}: has more than one of 'virtual', 'device' and 'app'"));
                    continue;
                }
            };
            let at = match &kind {
                InputKind::Virtual => format!("{at} (virtual)"),
                InputKind::VirtualDevice(v) => format!("{at} (virtual '{}')", v.name),
                InputKind::Device(n) => format!("{at} (device '{n}')"),
                InputKind::App { name, .. } => format!("{at} (app '{name}')"),
            };
            match &kind {
                InputKind::Virtual if d.virtual_inputs == 0 => {
                    problems.push(format!("{at}: the device has 'virtual_inputs: 0', so nothing can play into it"));
                }
                InputKind::Virtual if has_virtual => {
                    problems.push(format!("{at}: only one 'virtual: true' input per device; put all its wires in one"));
                }
                InputKind::Virtual => has_virtual = true,
                InputKind::Device(n) | InputKind::App { name: n, .. } if n.trim().is_empty() => {
                    problems.push(format!("{at}: name must not be empty"));
                }
                _ => {}
            }
            if s.mute_original.is_some() && !matches!(kind, InputKind::App { .. }) {
                problems.push(format!("{at}: mute_original only applies to app inputs"));
            }
            if s.tap.is_some() && !matches!(kind, InputKind::App { .. }) {
                problems.push(format!("{at}: tap only applies to app inputs"));
            }
            let from_max = match &kind {
                InputKind::Virtual => Some(d.virtual_inputs),
                InputKind::VirtualDevice(v) => Some(v.channels),
                _ => None,
            };
            let id = match &kind {
                InputKind::VirtualDevice(v) => v.id.clone(),
                _ => given_id.map(|i| i.trim().to_string()).unwrap_or_else(|| derived_id('i', &format!("{}/input/{j}", d.id))),
            };
            claim(&id, &format!("input {}", j + 1), &mut problems);
            let shorthand = to_pairs(&refs, &s.to.0, shorthand_channels(Some(&kind), None, d.virtual_inputs), &at, &mut problems);
            let raw = resolve(s.wires, device_channel, |e| refs.channel(e), shorthand, &at, &mut problems);
            let wires = wires(raw, from_max, "input", Some(busses), "bus", &at, &mut problems);
            let mono = mono_channels(s.mono, from_max, &at, &mut problems);
            let rack = rack_spot(s.rack, s.half, &at, &mut problems);
            inputs.push(Input { id, kind, wires, enabled: s.enabled, gain_db: gain(s.gain, &at, &mut problems), muted: s.muted, mono, half: s.half, rack });
        }

        let mut outputs = Vec::new();
        for (j, o) in d.outputs.into_iter().enumerate() {
            let label = match (&o.virtual_, &o.device) {
                (Some(VirtualKey::Name(n)), _) => format!("virtual '{n}'"),
                (_, Some(dev)) => format!("device '{dev}'"),
                _ => "device ''".to_string(),
            };
            let at = format!("{at}, output {} ({label})", j + 1);
            renamed(&o.volume, "volume", "gain", &at, &mut problems);
            renamed(&o.output, "output:", "device:", &at, &mut problems);
            let named = matches!(o.virtual_, Some(VirtualKey::Name(_)));
            if !named && (o.channels.is_some() || o.icon.is_some()) {
                problems.push(format!("{at}: 'channels' and 'icon' only apply to a virtual device ('virtual: <name>')"));
            }
            let given_id = o.id.clone();
            let kind = match (o.virtual_, o.device) {
                (None, Some(dev)) if !dev.trim().is_empty() => OutputKind::Device(dev),
                (Some(VirtualKey::Name(name)), None) => match virtual_device(name, o.channels, o.id, o.icon, &mut virtual_ids, &at, &mut problems) {
                    Some(v) => OutputKind::Virtual(v),
                    None => continue,
                },
                (Some(VirtualKey::Flag(_)), _) => {
                    problems.push(format!("{at}: an output needs 'virtual: <name>' (a device of its own apps record from), not true/false"));
                    continue;
                }
                (Some(_), Some(_)) => {
                    problems.push(format!("{at}: has both 'virtual' and 'device'"));
                    continue;
                }
                _ => {
                    if o.output.is_none() {
                        problems.push(format!("{at}: needs 'device:' (a hardware output) or 'virtual: <name>'"));
                    }
                    continue;
                }
            };
            let to_max = match &kind {
                OutputKind::Virtual(v) => Some(v.channels),
                OutputKind::Device(_) => None,
            };
            let id = match &kind {
                OutputKind::Virtual(v) => v.id.clone(),
                OutputKind::Device(_) => given_id.map(|i| i.trim().to_string()).unwrap_or_else(|| derived_id('o', &format!("{}/output/{j}", d.id))),
            };
            claim(&id, &format!("output {}", j + 1), &mut problems);
            let shorthand = from_pairs(&refs, &o.from.0, shorthand_channels(None, Some(&kind), d.virtual_inputs), &at, &mut problems);
            // Wires from a MIX: what doesn't name a bus but does name a MIX.
            let (mix_raw, bus_raw): (Vec<_>, Vec<_>) = o.wires.0.into_iter().partition(|(f, _)| match f {
                End::Text(t) => refs.channel(f).is_err() && refs.mix(t, &groups, d.mix_mono).is_some(),
                End::Number(_) => false,
            });
            let mix_wires = mix_wires(mix_raw, &refs, &groups, d.mix_mono, to_max, &at, &mut problems);
            let raw = resolve(Wires(bus_raw), |e| refs.channel(e), device_channel, shorthand, &at, &mut problems);
            let wires = wires(raw, Some(busses), "bus", to_max, "device", &at, &mut problems);
            let mono = mono_channels(o.mono, to_max, &at, &mut problems);
            let rack = rack_spot(o.rack, o.half, &at, &mut problems);
            outputs.push(Output {
                id,
                kind,
                wires,
                mix_wires,
                enabled: o.enabled,
                gain_db: gain(o.gain, &at, &mut problems),
                muted: o.muted,
                follow_volume: o.follow_volume,
                mono,
                half: o.half,
                rack,
            });
        }

        if virtual_ids.len() > MAX_VIRTUALS {
            problems.push(format!("{at}: at most {MAX_VIRTUALS} virtual devices per mixer, found {}", virtual_ids.len()));
        }
        devices.push(Device {
            name: d.name,
            id: d.id,
            busses,
            bus_list,
            mixer_rack: rack_spot(d.mixer_rack, false, &format!("{at}: mixer_rack"), &mut problems),
            groups,
            mix_mono: d.mix_mono,
            virtual_inputs: d.virtual_inputs,
            sample_rate: d.sample_rate,
            buffer_frames: d.buffer_frames,
            inputs,
            outputs,
        });
    }
    if problems.is_empty() {
        Ok(crate::Config { devices })
    } else {
        Err(problems)
    }
}
