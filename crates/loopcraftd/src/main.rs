//! loopcraftd: applies `config.yaml` to LoopCraft and keeps it applied.
//!
//! It manages the devices themselves (names, channel counts, sample rate)
//! through the plug-in, and routes their inputs, busses and outputs through one
//! engine per device. A config with errors is reported and ignored, so the
//! last good configuration stays in effect; without a config file the devices
//! are left alone and nothing is routed.

macro_rules! log {
    ($($arg:tt)*) => { eprintln!("{} {}", $crate::timestamp(), format!($($arg)*)) };
}

mod apps;
mod engine;
mod mix;
mod plan;
mod sync;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use lc_ca as ca;
use lc_spec::{DeviceSpec, BUNDLE_ID, DEVICES_PROPERTY};
use engine::Engines;
use sync::{Outcome, Plugin, Syncer};

const POLL: Duration = Duration::from_secs(1);
/// The config file is checked this often, so a mute or a patch made in the
/// editor is heard at once.
const WATCH: Duration = Duration::from_millis(50);
/// Also compare with the plug-in every this many polls, to catch coreaudiod
/// restarts and changes made by other tools.
const RESYNC_POLLS: u32 = 5;
/// Editors that write a file in place may do it in several steps; wait for
/// it to settle. A file replaced in one step (a new inode, as the LoopCraft
/// editor and most text editors save) is complete at once.
const SETTLE: Duration = Duration::from_millis(200);

const USAGE: &str = "\
loopcraftd — applies the LoopCraft configuration and keeps it applied

USAGE:
  loopcraftd [--config PATH] [--once]

  --config PATH   config file (default: $LOOPCRAFT_CONFIG, else ~/.config/loopcraft/config.yaml)
  --once          apply the devices once and exit (exit status 1 if that failed); no routing
";

/// Set by SIGTERM (launchd stopping the daemon) or SIGINT: stop routing
/// cleanly, then exit. Leaving coreaudiod to tear down a running route after
/// the daemon is killed preceded a coreaudiod IO hang (docs/incidents.md).
static QUIT: AtomicBool = AtomicBool::new(false);

extern "C" fn on_quit(_signal: i32) {
    QUIT.store(true, Ordering::Relaxed); // async-signal-safe: one atomic store
}

fn catch_quit_signals() {
    extern "C" {
        fn signal(sig: i32, handler: extern "C" fn(i32)) -> usize;
    }
    const SIGINT: i32 = 2;
    const SIGTERM: i32 = 15;
    unsafe {
        signal(SIGTERM, on_quit);
        signal(SIGINT, on_quit);
    }
}

/// Sleeps up to `d`, waking early when asked to quit or when `wake` says so.
fn nap(d: Duration, wake: impl Fn() -> bool) {
    let mut left = d;
    while !left.is_zero() && !QUIT.load(Ordering::Relaxed) && !wake() {
        let s = left.min(WATCH);
        std::thread::sleep(s);
        left -= s;
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut path = None;
    let mut once = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--config" => match it.next() {
                Some(p) => path = Some(PathBuf::from(p)),
                None => return usage_error("--config needs a path"),
            },
            "--once" => once = true,
            "-h" | "--help" | "help" => {
                print!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            other => return usage_error(&format!("unknown argument '{other}'")),
        }
    }
    let path = path.unwrap_or_else(lc_config::default_path);
    log!("loopcraftd {} started; config {}", env!("CARGO_PKG_VERSION"), path.display());

    if let Err(e) = ca::use_hal_notification_thread() {
        log!("warning: {e}");
    }
    let mut watch = Watch::new(path);
    let mut syncer = Syncer::default();
    catch_quit_signals();
    let mut engines = Engines::default();
    let mut config: Option<lc_config::Config> = None;
    let mut last: Option<std::mem::Discriminant<Outcome>> = None;
    let mut polls = 0u32;
    let mut soon = false;
    loop {
        if QUIT.load(Ordering::Relaxed) {
            engines.stop_all();
            log!("loopcraftd stopped");
            return ExitCode::SUCCESS;
        }
        let changed = match watch.poll() {
            Some(change) => {
                load(watch.path(), change, &mut syncer, &mut config);
                engines.config_changed();
                last = None;
                true
            }
            None => false,
        };
        if changed || soon || polls.is_multiple_of(RESYNC_POLLS) {
            if let Some(c) = &config {
                syncer.set_desired(Some(engines.specs(c, CoreAudio.fields())));
            }
            let outcome = syncer.tick(&CoreAudio);
            let ok = matches!(outcome, Outcome::InSync | Outcome::Applied(_) | Outcome::Idle);
            // New devices take a moment to appear; route them on the next poll.
            soon = matches!(outcome, Outcome::Applied(_));
            report(outcome, &mut last);
            if once {
                return if ok { ExitCode::SUCCESS } else { ExitCode::FAILURE };
            }
            engines.reconcile(config.as_ref());
        }
        polls = polls.wrapping_add(1);
        nap(POLL, || watch.pending());
    }
}

fn usage_error(msg: &str) -> ExitCode {
    eprint!("loopcraftd: {msg}\n\n{USAGE}");
    ExitCode::from(2)
}

/// Turns a changed config file into the desired device list.
fn load(path: &Path, change: Change, syncer: &mut Syncer, current: &mut Option<lc_config::Config>) {
    match change {
        Change::Missing => {
            log!("no config at {}: leaving LoopCraft devices as they are (`loopcraft config init` creates one)", path.display());
            syncer.set_desired(None);
            *current = None;
        }
        Change::Unreadable(e) => log!("cannot read {}: {e}; keeping the last good configuration", path.display()),
        Change::Text(text) => match lc_config::parse(&text) {
            Ok(config) => {
                let names: Vec<&str> = config.devices.iter().map(|d| d.name.as_str()).collect();
                log!("loaded {}: {} device(s) {}", path.display(), names.len(), names.join(", "));
                // The device list (with app slots) is derived from it before each sync.
                *current = Some(config);
            }
            Err(e) => log!("{} has errors; keeping the last good configuration\n{e}", path.display()),
        },
    }
}

/// Logs outcomes worth knowing about: every change and failure, and steady
/// states only when they begin.
fn report(outcome: Outcome, last: &mut Option<std::mem::Discriminant<Outcome>>) {
    let kind = std::mem::discriminant(&outcome);
    let new_state = *last != Some(kind);
    *last = Some(kind);
    match outcome {
        Outcome::Applied(what) => log!("applied: {what}"),
        Outcome::Failed(e) => log!("{e}"),
        Outcome::InSync if new_state => log!("devices match the configuration"),
        Outcome::NoPlugin if new_state => log!("the LoopCraft plug-in is not loaded; waiting for it"),
        Outcome::GaveUp | Outcome::InSync | Outcome::NoPlugin | Outcome::Idle => {}
    }
}

/// The installed plug-in, looked up afresh each time because its object ID
/// changes when coreaudiod restarts.
struct CoreAudio;

impl CoreAudio {
    /// The optional device-list fields the installed plug-in understands.
    fn fields(&self) -> Vec<&'static str> {
        ca::plugin(BUNDLE_ID)
            .and_then(|p| ca::get_string(p, DEVICES_PROPERTY, ca::Scope::Global).ok())
            .map(|json| lc_spec::reported_fields(&json))
            .unwrap_or_default()
    }
}

impl Plugin for CoreAudio {
    fn read(&self) -> Result<Option<Vec<DeviceSpec>>, String> {
        let Some(plugin) = ca::plugin(BUNDLE_ID) else { return Ok(None) };
        let json = ca::get_string(plugin, DEVICES_PROPERTY, ca::Scope::Global).map_err(|e| e.to_string())?;
        lc_spec::parse(&json).map(Some)
    }

    fn write(&self, specs: &[DeviceSpec]) -> Result<(), String> {
        let plugin = ca::plugin(BUNDLE_ID).ok_or("the plug-in is not loaded")?;
        // Older plug-ins reject fields they don't know.
        let json = lc_spec::to_json_for(specs, &self.fields())?;
        ca::set_plist_string(plugin, DEVICES_PROPERTY, &json).map_err(|e| e.to_string())
    }
}

enum Change {
    Missing,
    Unreadable(std::io::Error),
    Text(String),
}

/// Notices changes to the config file by polling its metadata, which also
/// works for editors that save by replacing the file.
struct Watch {
    path: PathBuf,
    /// `None` before the first poll.
    stamp: Option<Option<Stamp>>,
}

#[derive(Clone, Copy, PartialEq)]
struct Stamp {
    modified: Option<SystemTime>,
    len: u64,
    inode: u64,
}

impl Watch {
    fn new(path: PathBuf) -> Self {
        Watch { path, stamp: None }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn stamp(&self) -> Option<Stamp> {
        use std::os::unix::fs::MetadataExt;
        let m = std::fs::metadata(&self.path).ok()?;
        Some(Stamp { modified: m.modified().ok(), len: m.len(), inode: m.ino() })
    }

    /// Has the file changed since the last poll?
    fn pending(&self) -> bool {
        self.stamp.is_some_and(|s| s != self.stamp())
    }

    fn poll(&mut self) -> Option<Change> {
        let mut now = self.stamp();
        if self.stamp == Some(now) {
            return None;
        }
        let replaced = matches!((self.stamp.flatten(), now), (Some(a), Some(b)) if a.inode != b.inode);
        if self.stamp.is_some() && !replaced {
            // Changed rather than first seen: let the writer finish.
            loop {
                std::thread::sleep(SETTLE);
                let again = self.stamp();
                if again == now {
                    break;
                }
                now = again;
            }
        }
        self.stamp = Some(now);
        Some(match now {
            None => Change::Missing,
            Some(_) => match std::fs::read_to_string(&self.path) {
                Ok(text) => Change::Text(text),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Change::Missing,
                Err(e) => Change::Unreadable(e),
            },
        })
    }
}

/// Local time as "2026-10-30 01:18:35".
pub(crate) fn timestamp() -> String {
    #[repr(C)]
    struct Tm {
        sec: i32,
        min: i32,
        hour: i32,
        mday: i32,
        mon: i32,
        year: i32,
        wday: i32,
        yday: i32,
        isdst: i32,
        gmtoff: std::ffi::c_long,
        zone: *const std::ffi::c_char,
    }
    extern "C" {
        fn localtime_r(t: *const i64, out: *mut Tm) -> *mut Tm;
    }
    let secs = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    // SAFETY: Tm matches `struct tm` on macOS; localtime_r only writes into it.
    let mut tm: Tm = unsafe { std::mem::zeroed() };
    if unsafe { localtime_r(&secs, &mut tm) }.is_null() {
        return secs.to_string();
    }
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", tm.year + 1900, tm.mon + 1, tm.mday, tm.hour, tm.min, tm.sec)
}
