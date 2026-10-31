//! `loopcraft`: command-line control for LoopCraft devices.

mod config_cmd;

use lc_ca as ca;

use ca::{Device, Scope};
use lc_sys::consts::*;
use std::process::ExitCode;
use std::time::Duration;

const LOOPCRAFT_UID_PREFIX: &str = "org.kerpo.loopcraft:";
/// Highest gain the device controls take, in dB.
const MAX_GAIN_DB: f32 = lc_config::MAX_GAIN_DB as f32;

const USAGE: &str = "\
loopcraft — control LoopCraft audio devices

USAGE:
  loopcraft status                         Show the LoopCraft device and who uses it
  loopcraft devices                        List all audio devices
  loopcraft rate [HZ]                      Show or set the sample rate
  loopcraft gain <virtual|busses> [DB]     Show or set gain in dB (0 = unity, -inf..+12)
  loopcraft mute <virtual|busses> [on|off] Show or set mute
  loopcraft default <output|input|system> [DEVICE]
                                           Show or set the macOS default device
  loopcraft level [SECONDS]                Meter the audio coming out of LoopCraft
  loopcraft config                         Show where the configuration file is
  loopcraft config init [PATH]             Create it from the current LoopCraft devices
  loopcraft config check [PATH]            Validate it and look up its hardware devices
  loopcraft help                           Show this help

\"virtual\" is what apps play into the device (also: out), \"busses\" what
recording apps receive (also: in). DEVICE is \"loopcraft\", a device UID, a
name, a unique part of a name, or an ID.
";

fn main() -> ExitCode {
    // Exit quietly when the reader goes away (`loopcraft devices | head`).
    extern "C" {
        fn signal(sig: i32, handler: usize) -> usize;
    }
    const SIGPIPE: i32 = 13;
    const SIG_DFL: usize = 0;
    unsafe { signal(SIGPIPE, SIG_DFL) };
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(Failure::Usage(msg)) => {
            eprintln!("{msg}\n\n{USAGE}");
            ExitCode::from(2)
        }
        Err(Failure::Error(msg)) => {
            eprintln!("loopcraft: {msg}");
            ExitCode::FAILURE
        }
    }
}

#[derive(Debug, PartialEq)]
enum Failure {
    Usage(String),
    Error(String),
}

impl From<ca::Error> for Failure {
    fn from(e: ca::Error) -> Self {
        Failure::Error(e.to_string())
    }
}

type Result<T> = std::result::Result<T, Failure>;

fn usage(msg: impl Into<String>) -> Failure {
    Failure::Usage(msg.into())
}

fn run(args: &[String]) -> Result<()> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        [] | ["help"] | ["-h"] | ["--help"] => {
            print!("{USAGE}");
            Ok(())
        }
        ["status"] => status(),
        ["devices"] => list_devices(),
        ["rate"] => show_rate(),
        ["rate", hz] => set_rate(parse_rate(hz)?),
        ["gain", dir] => show_gain(parse_direction(dir)?),
        ["gain", dir, db] => set_gain(parse_direction(dir)?, parse_db(db)?),
        ["volume", ..] => Err(usage("'volume' is now 'gain', in dB: loopcraft gain <virtual|busses> [DB]")),
        ["mute", dir] => show_mute(parse_direction(dir)?),
        ["mute", dir, state] => set_mute(parse_direction(dir)?, parse_on_off(state)?),
        ["default", which] => show_default(parse_default(which)?),
        ["default", which, rest @ ..] if !rest.is_empty() => set_default(parse_default(which)?, &rest.join(" ")),
        ["level"] => level(5.0),
        ["config"] => config_cmd::show_path(),
        ["config", "init"] => config_cmd::init(None),
        ["config", "init", path] => config_cmd::init(Some(path)),
        ["config", "check"] => config_cmd::check(None),
        ["config", "check", path] => config_cmd::check(Some(path)),
        ["debug", "devices"] => debug_devices(None),
        ["debug", "devices", json] => debug_devices(Some(json)),
        ["level", secs] => level(secs.parse::<f64>().ok().filter(|s| *s > 0.0 && *s <= 3600.0).ok_or_else(|| usage(format!("bad duration '{secs}'")))?),
        [cmd, ..] => Err(usage(format!("unknown or incomplete command '{cmd}'"))),
    }
}

// ---------------------------------------------------------------- parsing

fn parse_direction(s: &str) -> Result<Scope> {
    match s {
        "busses" | "buses" | "in" | "input" => Ok(Scope::Input),
        "virtual" | "out" | "output" => Ok(Scope::Output),
        _ => Err(usage(format!("expected 'virtual' or 'busses', got '{s}'"))),
    }
}

/// A gain in dB: a number (optionally with "dB") or -inf, at most +12.
fn parse_db(s: &str) -> Result<f32> {
    let t = s.trim();
    let t = t.strip_suffix("dB").or_else(|| t.strip_suffix("db")).unwrap_or(t).trim();
    let v: f32 = if t == "-inf" { f32::NEG_INFINITY } else { t.parse().map_err(|_| usage(format!("bad gain '{s}': expected dB, e.g. 0, -6 or -inf")))? };
    if v.is_nan() || v > MAX_GAIN_DB {
        return Err(usage(format!("gain must be -inf..+{MAX_GAIN_DB} dB, got {s}")));
    }
    Ok(v)
}

/// The device's gain as text, "-inf" at the bottom of the fader.
fn gain_label(id: u32, dir: Scope) -> String {
    let scalar = ca::get_f32(id, kAudioDevicePropertyVolumeScalar, dir).unwrap_or(f32::NAN);
    match ca::get_f32(id, kAudioDevicePropertyVolumeDecibels, dir) {
        _ if scalar == 0.0 => "-inf dB".into(),
        Ok(db) => format!("{:+.1} dB", db + 0.0), // + 0.0 turns -0.0 into 0.0
        Err(_) => "?".into(),
    }
}

fn parse_on_off(s: &str) -> Result<bool> {
    match s {
        "on" | "1" | "true" | "yes" => Ok(true),
        "off" | "0" | "false" | "no" => Ok(false),
        _ => Err(usage(format!("expected 'on' or 'off', got '{s}'"))),
    }
}

fn parse_rate(s: &str) -> Result<f64> {
    let s = s.to_ascii_lowercase();
    let v: f64 = if let Some(k) = s.strip_suffix('k') {
        k.parse::<f64>().map(|k| k * 1000.0)
    } else {
        s.parse()
    }
    .map_err(|_| usage(format!("bad sample rate '{s}'")))?;
    // Accept "44.1k" spelled out as 44100.
    Ok((v * 10.0).round() / 10.0)
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum DefaultKind {
    Output,
    Input,
    System,
}

impl DefaultKind {
    fn selector(self) -> u32 {
        match self {
            DefaultKind::Output => kAudioHardwarePropertyDefaultOutputDevice,
            DefaultKind::Input => kAudioHardwarePropertyDefaultInputDevice,
            DefaultKind::System => kAudioHardwarePropertyDefaultSystemOutputDevice,
        }
    }

    fn label(self) -> &'static str {
        match self {
            DefaultKind::Output => "default output",
            DefaultKind::Input => "default input",
            DefaultKind::System => "system (alerts) output",
        }
    }
}

fn parse_default(s: &str) -> Result<DefaultKind> {
    match s {
        "output" | "out" => Ok(DefaultKind::Output),
        "input" | "in" => Ok(DefaultKind::Input),
        "system" | "alerts" => Ok(DefaultKind::System),
        _ => Err(usage(format!("expected 'output', 'input' or 'system', got '{s}'"))),
    }
}

/// Picks a device by alias, UID, ID, exact name or unique name fragment.
fn resolve<'a>(spec: &str, devices: &'a [Device]) -> std::result::Result<&'a Device, String> {
    let lower = spec.to_lowercase();
    if lower == "loopcraft" || lower == "lc" {
        return devices
            .iter()
            .find(|d| d.uid.starts_with(LOOPCRAFT_UID_PREFIX))
            .ok_or_else(|| "no LoopCraft device found; is the plug-in installed?".into());
    }
    if let Some(d) = devices.iter().find(|d| d.uid == spec) {
        return Ok(d);
    }
    if let Ok(id) = spec.parse::<u32>() {
        if let Some(d) = devices.iter().find(|d| d.id == id) {
            return Ok(d);
        }
    }
    if let Some(d) = devices.iter().find(|d| d.name.to_lowercase() == lower) {
        return Ok(d);
    }
    let matches: Vec<&Device> = devices.iter().filter(|d| d.name.to_lowercase().contains(&lower)).collect();
    match matches.as_slice() {
        [d] => Ok(d),
        [] => Err(format!("no device matches '{spec}' (see 'loopcraft devices')")),
        many => Err(format!(
            "'{spec}' matches several devices: {}",
            many.iter().map(|d| d.name.as_str()).collect::<Vec<_>>().join(", ")
        )),
    }
}

// ---------------------------------------------------------------- helpers

fn loopcraft_device() -> Result<Device> {
    let devices = ca::devices()?;
    resolve("loopcraft", &devices).cloned().map_err(Failure::Error)
}

fn db(x: f32) -> String {
    if x > 0.0 {
        format!("{:.1} dBFS", 20.0 * x.log10())
    } else {
        "-inf dBFS".into()
    }
}

fn rate_label(hz: f64) -> String {
    if hz.fract() == 0.0 {
        format!("{} Hz", hz as u64)
    } else {
        format!("{hz} Hz")
    }
}

fn dir_label(s: Scope) -> &'static str {
    if s == Scope::Input { "busses" } else { "virtual inputs" }
}

fn default_device(kind: DefaultKind) -> Result<u32> {
    Ok(ca::get_u32(kAudioObjectSystemObject, kind.selector(), Scope::Global)?)
}

// --------------------------------------------------------------- commands

fn status() -> Result<()> {
    let d = loopcraft_device()?;
    let devices = ca::devices()?;
    let name_of = |id: u32| devices.iter().find(|x| x.id == id).map_or(format!("#{id}"), |x| x.name.clone());

    println!("{}  (id {}, {} in / {} out)", d.name, d.id, d.inputs, d.outputs);
    println!("  uid            {}", d.uid);
    let rate = ca::get_f64(d.id, kAudioDevicePropertyNominalSampleRate, Scope::Global)?;
    let rates = ca::get_ranges(d.id, kAudioDevicePropertyAvailableNominalSampleRates, Scope::Global).unwrap_or_default();
    println!(
        "  sample rate    {}  (available: {})",
        rate_label(rate),
        rates.iter().map(|r| format!("{}", r.min as u64)).collect::<Vec<_>>().join(", ")
    );
    if let Ok(frames) = ca::get_u32(d.id, kAudioDevicePropertyBufferFrameSize, Scope::Global) {
        println!("  buffer         {frames} frames");
    }
    for dir in [Scope::Output, Scope::Input] {
        let muted = ca::get_u32(d.id, kAudioDevicePropertyMute, dir).unwrap_or(0) != 0;
        println!("  {:<14} {}{}", format!("{} gain", dir_label(dir)), gain_label(d.id, dir), if muted { "  MUTED" } else { "" });
    }
    let running = ca::get_u32(d.id, kAudioDevicePropertyDeviceIsRunningSomewhere, Scope::Global).unwrap_or(0) != 0;
    println!("  running        {}", if running { "yes" } else { "no" });

    for kind in [DefaultKind::Output, DefaultKind::Input, DefaultKind::System] {
        let cur = default_device(kind)?;
        let mark = if cur == d.id { "LoopCraft".to_string() } else { name_of(cur) };
        println!("  {:<14} {}", kind.label(), mark);
    }

    let procs = ca::processes()?;
    let playing: Vec<_> = procs.iter().filter(|p| p.running_output && p.output_devices.contains(&d.id)).collect();
    let recording: Vec<_> = procs.iter().filter(|p| p.running_input && p.input_devices.contains(&d.id)).collect();
    let show = |ps: &[&ca::Process]| {
        if ps.is_empty() {
            "nobody".to_string()
        } else {
            ps.iter().map(|p| format!("{} ({})", if p.bundle_id.is_empty() { "?" } else { &p.bundle_id }, p.pid)).collect::<Vec<_>>().join(", ")
        }
    };
    println!("  playing into   {}", show(&playing));
    println!("  recording from {}", show(&recording));
    Ok(())
}

fn list_devices() -> Result<()> {
    let devices = ca::devices()?;
    let out = default_device(DefaultKind::Output)?;
    let inp = default_device(DefaultKind::Input)?;
    println!("{:>5}  {:<36} {:>4} {:>4}  DEFAULT  UID", "ID", "NAME", "IN", "OUT");
    for d in &devices {
        let mut flags = String::new();
        if d.id == out {
            flags.push('O');
        }
        if d.id == inp {
            flags.push('I');
        }
        let lc = if d.uid.starts_with(LOOPCRAFT_UID_PREFIX) { "*" } else { " " };
        println!("{:>5}{lc} {:<36} {:>4} {:>4}  {:<7}  {}", d.id, d.name, d.inputs, d.outputs, flags, d.uid);
    }
    println!("\n* = LoopCraft device, O/I = default output/input");
    Ok(())
}

fn show_rate() -> Result<()> {
    let d = loopcraft_device()?;
    println!("{}", rate_label(ca::get_f64(d.id, kAudioDevicePropertyNominalSampleRate, Scope::Global)?));
    Ok(())
}

fn set_rate(hz: f64) -> Result<()> {
    let d = loopcraft_device()?;
    let rates = ca::get_ranges(d.id, kAudioDevicePropertyAvailableNominalSampleRates, Scope::Global)?;
    if !rates.iter().any(|r| r.min <= hz && hz <= r.max) {
        let list = rates.iter().map(|r| format!("{}", r.min as u64)).collect::<Vec<_>>().join(", ");
        return Err(Failure::Error(format!("{} is not supported (available: {list})", rate_label(hz))));
    }
    ca::set_f64(d.id, kAudioDevicePropertyNominalSampleRate, Scope::Global, hz)?;
    // The change is applied asynchronously by coreaudiod.
    for _ in 0..20 {
        if ca::get_f64(d.id, kAudioDevicePropertyNominalSampleRate, Scope::Global)? == hz {
            println!("sample rate set to {}", rate_label(hz));
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(Failure::Error(format!("coreaudiod did not apply {} within 1 s", rate_label(hz))))
}

fn show_gain(dir: Scope) -> Result<()> {
    let d = loopcraft_device()?;
    ca::get_f32(d.id, kAudioDevicePropertyVolumeDecibels, dir)?;
    println!("{} gain {}", dir_label(dir), gain_label(d.id, dir));
    Ok(())
}

fn set_gain(dir: Scope, db: f32) -> Result<()> {
    let d = loopcraft_device()?;
    if db == f32::NEG_INFINITY {
        ca::set_f32(d.id, kAudioDevicePropertyVolumeScalar, dir, 0.0)?;
    } else {
        ca::set_f32(d.id, kAudioDevicePropertyVolumeDecibels, dir, db)?;
    }
    show_gain(dir)
}

fn show_mute(dir: Scope) -> Result<()> {
    let d = loopcraft_device()?;
    let m = ca::get_u32(d.id, kAudioDevicePropertyMute, dir)? != 0;
    println!("{} {}", dir_label(dir), if m { "muted" } else { "not muted" });
    Ok(())
}

fn set_mute(dir: Scope, on: bool) -> Result<()> {
    let d = loopcraft_device()?;
    ca::set_u32(d.id, kAudioDevicePropertyMute, dir, on as u32)?;
    show_mute(dir)
}

fn show_default(kind: DefaultKind) -> Result<()> {
    let id = default_device(kind)?;
    let d = ca::device(id);
    println!("{}: {} (id {})", kind.label(), d.name, d.id);
    Ok(())
}

fn set_default(kind: DefaultKind, spec: &str) -> Result<()> {
    let devices = ca::devices()?;
    let d = resolve(spec, &devices).map_err(Failure::Error)?;
    let needs = if kind == DefaultKind::Input { d.inputs } else { d.outputs };
    if needs == 0 {
        return Err(Failure::Error(format!("{} has no {} channels", d.name, if kind == DefaultKind::Input { "input" } else { "output" })));
    }
    ca::set_u32(kAudioObjectSystemObject, kind.selector(), Scope::Global, d.id)?;
    show_default(kind)
}

/// Low-level access to the plug-in's device list (JSON), for testing.
fn debug_devices(json: Option<&str>) -> Result<()> {
    use lc_spec::{BUNDLE_ID, DEVICES_PROPERTY};
    let plugin = ca::plugin(BUNDLE_ID).ok_or_else(|| Failure::Error("the LoopCraft plug-in is not loaded".into()))?;
    if let Some(json) = json {
        ca::set_plist_string(plugin, DEVICES_PROPERTY, json)?;
    }
    println!("{}", ca::get_string(plugin, DEVICES_PROPERTY, Scope::Global)?);
    Ok(())
}

fn bar(peak: f32) -> String {
    // -60 dBFS .. 0 dBFS over 30 cells.
    let level = if peak > 0.0 { ((20.0 * peak.log10() + 60.0) / 60.0).clamp(0.0, 1.0) } else { 0.0 };
    let filled = (level * 30.0).round() as usize;
    format!("{}{}", "█".repeat(filled), "·".repeat(30 - filled))
}

fn level(seconds: f64) -> Result<()> {
    let d = loopcraft_device()?;
    println!("metering {} input for {seconds} s (peak per 0.5 s)", d.name);
    let mut overall: Vec<f32> = vec![0.0; d.inputs.max(1) as usize];
    ca::meter_input(d.id, Duration::from_secs_f64(seconds), Duration::from_millis(500), |m| {
        let vals = m.take();
        let line: Vec<String> = vals
            .iter()
            .enumerate()
            .map(|(c, (peak, _))| {
                overall[c] = overall[c].max(*peak);
                format!("{} {} {:>10}", if c == 0 { "L" } else { "R" }, bar(*peak), db(*peak))
            })
            .collect();
        println!("  {}", line.join("   "));
    })?;
    println!("peak over the whole run: {}", overall.iter().map(|p| db(*p)).collect::<Vec<_>>().join(" / "));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(id: u32, name: &str, uid: &str) -> Device {
        Device { id, name: name.into(), uid: uid.into(), inputs: 2, outputs: 2 }
    }

    fn sample() -> Vec<Device> {
        vec![
            dev(10, "MacBook Pro Speakers", "BuiltInSpeakerDevice"),
            dev(11, "PRO X Wireless Gaming Headset", "usb-prox"),
            dev(12, "LoopCraft Rack", "org.kerpo.loopcraft:8E0F"),
            dev(13, "Rane FOUR", "usb-rane"),
            dev(14, "MacBook Pro Microphone", "BuiltInMicrophoneDevice"),
        ]
    }

    #[test]
    fn resolves_devices_by_alias_uid_id_and_name() {
        let d = sample();
        assert_eq!(resolve("loopcraft", &d).unwrap().id, 12);
        assert_eq!(resolve("LC", &d).unwrap().id, 12);
        assert_eq!(resolve("usb-rane", &d).unwrap().id, 13);
        assert_eq!(resolve("11", &d).unwrap().id, 11);
        assert_eq!(resolve("rane four", &d).unwrap().id, 13);
        assert_eq!(resolve("pro x", &d).unwrap().id, 11);
        assert!(resolve("macbook", &d).unwrap_err().contains("several"));
        assert!(resolve("nothing", &d).unwrap_err().contains("no device"));
    }

    #[test]
    fn parses_arguments() {
        assert_eq!(parse_db("0"), Ok(0.0));
        assert_eq!(parse_db("-6"), Ok(-6.0));
        assert_eq!(parse_db("+3dB"), Ok(3.0));
        assert_eq!(parse_db("-4.5 dB"), Ok(-4.5));
        assert_eq!(parse_db("-inf"), Ok(f32::NEG_INFINITY));
        assert!(parse_db("12.5").is_err());
        assert!(parse_db("50%").is_err());
        assert_eq!(parse_rate("44.1k"), Ok(44_100.0));
        assert_eq!(parse_rate("48000"), Ok(48_000.0));
        assert!(parse_rate("fast").is_err());
        assert_eq!(parse_on_off("on"), Ok(true));
        assert_eq!(parse_on_off("off"), Ok(false));
        assert_eq!(parse_direction("in"), Ok(Scope::Input));
        assert_eq!(parse_direction("busses"), Ok(Scope::Input));
        assert_eq!(parse_direction("virtual"), Ok(Scope::Output));
        assert_eq!(parse_default("system"), Ok(DefaultKind::System));
    }

    #[test]
    fn usage_errors_for_bad_commands() {
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(matches!(run(&args(&["frobnicate"])), Err(Failure::Usage(_))));
        assert!(matches!(run(&args(&["gain", "sideways"])), Err(Failure::Usage(_))));
        assert!(matches!(run(&args(&["volume", "out", "50"])), Err(Failure::Usage(m)) if m.contains("now 'gain'")));
        assert!(matches!(run(&args(&["level", "-3"])), Err(Failure::Usage(_))));
        assert_eq!(run(&args(&["help"])), Ok(()));
    }

    #[test]
    fn meter_bar_scale() {
        assert_eq!(bar(1.0).chars().filter(|c| *c == '█').count(), 30);
        assert_eq!(bar(0.0).chars().filter(|c| *c == '█').count(), 0);
        assert_eq!(bar(0.001).chars().filter(|c| *c == '█').count(), 0); // -60 dB
    }
}
