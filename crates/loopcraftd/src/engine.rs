//! Engines: one private aggregate and IOProc per LoopCraft device that routes
//! audio.
//!
//! The aggregate's clock comes from the device's hidden engine twin; the
//! hardware it uses joins with drift compensation. Rewiring and gain
//! changes swap the mix plan without stopping IO; only a change in the set
//! of member devices (or their stream layouts) rebuilds the aggregate.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use lc_ca as ca;
use lc_config::{Config, Device};
use lc_spec::TWIN_SUFFIX;
use lc_sys::consts::kAudioDevicePropertyBufferFrameSize;

use crate::mix::{self, Inputs, Outputs, Plan};
use crate::plan::{app_slots, layout, wants_engine, AppTap, Caps, Layout, Member};
use lc_spec::{AppSpec, DeviceSpec};

/// UID prefix of the daemon's private aggregates.
const AGGREGATE_PREFIX: &str = "org.kerpo.loopcraft.engine:";
/// Largest IO buffer the mix handles; larger cycles are silenced.
const MAX_FRAMES: usize = 8192;
/// Replaced plans are freed this long after the swap, on the control thread,
/// so the IO thread never frees one.
const RETIRE_AFTER: Duration = Duration::from_secs(5);
/// A failed engine start is retried after this long, or when its layout changes.
const RETRY_AFTER: Duration = Duration::from_secs(30);
/// How long a new aggregate may take to show its streams.
const ASSEMBLE_TIMEOUT: Duration = Duration::from_secs(3);
/// How often the device volume is read for outputs that follow it. Each IO
/// cycle ramps to the latest value, so this is the only delay a turn of a
/// volume knob sees.
const VOLUME_POLL: Duration = Duration::from_millis(20);

/// The device's own volume (output gain and mute) as a linear gain.
fn device_volume(id: ca::AudioObjectID) -> Option<f32> {
    use lc_sys::consts::{kAudioDevicePropertyMute, kAudioDevicePropertyVolumeDecibels, kAudioDevicePropertyVolumeScalar};
    let muted = ca::get_u32(id, kAudioDevicePropertyMute, ca::Scope::Output).ok()? != 0;
    let scalar = ca::get_f32(id, kAudioDevicePropertyVolumeScalar, ca::Scope::Output).ok()?;
    let db = ca::get_f32(id, kAudioDevicePropertyVolumeDecibels, ca::Scope::Output).ok()?;
    Some(if muted || scalar <= 0.0 { 0.0 } else { 10f32.powf(db / 20.0) })
}

/// Keeps `volume` equal to the device's volume until stopped.
struct VolumeWatcher {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl VolumeWatcher {
    fn start(device_uid: String, volume: Arc<AtomicU32>) -> std::io::Result<VolumeWatcher> {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::Builder::new().name("volume".into()).spawn(move || {
            let mut id = None;
            while !flag.load(Ordering::Relaxed) {
                // The ID changes when coreaudiod restarts; look it up again then.
                let v = id.and_then(device_volume).or_else(|| {
                    id = ca::device_by_uid(&device_uid);
                    id.and_then(device_volume)
                });
                if let Some(v) = v {
                    volume.store(v.to_bits(), Ordering::Relaxed);
                }
                std::thread::sleep(VOLUME_POLL);
            }
        })?;
        Ok(VolumeWatcher { stop, thread: Some(thread) })
    }
}

impl Drop for VolumeWatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Inputs for ca::Buffers<'_> {
    fn count(&self) -> usize {
        ca::Buffers::count(self)
    }
    fn get(&self, i: usize) -> Option<(&[f32], usize)> {
        ca::Buffers::get(self, i)
    }
}

impl Outputs for ca::BuffersMut<'_> {
    fn count(&self) -> usize {
        ca::BuffersMut::count(self)
    }
    fn get_mut(&mut self, i: usize) -> Option<(&mut [f32], usize)> {
        ca::BuffersMut::get_mut(self, i)
    }
}

#[derive(Default)]
struct Stats {
    cycles: AtomicU64,
    /// Cycles silenced because the buffers didn't match the plan.
    silenced: AtomicU64,
}

struct Engine {
    members: Vec<Member>,
    taps: Vec<AppTap>,
    buffer_frames: Option<u32>,
    /// The IO buffer the aggregate actually runs with.
    actual_buffer: u32,
    aggregate: ca::AudioObjectID,
    /// Held only to keep the taps alive; destroyed after the aggregate
    /// (fields drop after `Drop::drop`).
    _tap_objects: Vec<ca::Tap>,
    io: Option<ca::IoProc>,
    plan: Arc<ArcSwap<Plan>>,
    retired: Vec<(Instant, Arc<Plan>)>,
    stats: Arc<Stats>,
    silenced_seen: u64,
    /// The device volume for outputs that follow it, as linear-gain bits.
    volume: Arc<AtomicU32>,
    watcher: Option<VolumeWatcher>,
    /// Live levels for the editor; none if the file can't be made.
    levels: Option<Arc<lc_meter::Levels>>,
    /// The master gain over all outputs: what it should be (set here) and
    /// where the IO thread has it now, as f32 bits.
    master: Arc<AtomicU32>,
    master_now: Arc<AtomicU32>,
    /// Transitions the editor asked for that this engine has done.
    transitions_seen: u32,
}

fn shape(plan: &Plan) -> lc_meter::Shape {
    lc_meter::Shape {
        inputs: plan.meter_inputs.iter().map(|u| u.len() as u32).collect(),
        busses: plan.mix_channels as u32,
        outputs: plan.meter_outputs.iter().map(|u| u.len() as u32).collect(),
    }
}

enum StartError {
    /// This app could not be tapped (typically: no audio-capture permission).
    Tap { app: String, error: String },
    Other(String),
}

impl Engine {
    fn start(device: &Device, layout: Layout) -> Result<Engine, StartError> {
        let other = |e: ca::Error| StartError::Other(e.to_string());
        let mut tap_objects = Vec::new();
        for t in &layout.taps {
            let name = format!("{} → {} (LoopCraft)", t.name, device.name);
            let spec = ca::TapSpec { name: &name, bundle_ids: &t.bundle_ids, mute_original: t.mute_original };
            tap_objects.push(ca::create_tap(&spec).map_err(|e| StartError::Tap { app: t.name.clone(), error: e.to_string() })?);
        }
        // Turning drift compensation off for taps measured no lower latency
        // (2026-10-30), so taps keep it.
        let tap_subs: Vec<ca::SubDevice> = tap_objects.iter().map(|t| ca::SubDevice { uid: &t.uid, drift_compensation: true }).collect();
        let uid = format!("{AGGREGATE_PREFIX}{}", device.id);
        let name = format!("{} (LoopCraft routing)", device.name);
        let subs: Vec<ca::SubDevice> = layout
            .members
            .iter()
            .enumerate()
            .map(|(i, m)| ca::SubDevice { uid: &m.uid, drift_compensation: i > 0 })
            .collect();
        let aggregate = ca::create_aggregate(&name, &uid, &layout.members[0].uid, &subs, &tap_subs).map_err(other)?;
        let mut engine = Engine {
            members: layout.members,
            taps: layout.taps,
            buffer_frames: device.buffer_frames,
            actual_buffer: 0,
            aggregate,
            _tap_objects: tap_objects,
            io: None,
            plan: Arc::new(ArcSwap::from_pointee(layout.plan)),
            retired: Vec::new(),
            stats: Arc::default(),
            silenced_seen: 0,
            volume: Arc::new(AtomicU32::new(1f32.to_bits())),
            watcher: None,
            master: Arc::new(AtomicU32::new(1f32.to_bits())),
            master_now: Arc::new(AtomicU32::new(0f32.to_bits())),
            transitions_seen: 0,
            levels: match lc_meter::Levels::create(&lc_meter::path(&device.id)) {
                Ok(l) => Some(Arc::new(l)),
                Err(e) => {
                    log!("'{}': no live levels for the editor ({e})", device.name);
                    None
                }
            },
        };
        if let Some(l) = &engine.levels {
            l.set_shape(&shape(&engine.plan.load()));
            engine.transitions_seen = l.transitions();
        }
        // Dropping `engine` on an error below destroys the aggregate.
        let want = engine.plan.load();
        let deadline = Instant::now() + ASSEMBLE_TIMEOUT;
        loop {
            let (inputs, outputs) = (ca::stream_channels(aggregate, ca::Scope::Input), ca::stream_channels(aggregate, ca::Scope::Output));
            if inputs == want.inputs && outputs == want.outputs {
                break;
            }
            if Instant::now() > deadline {
                return Err(StartError::Other(format!(
                    "the aggregate has streams in {inputs:?} / out {outputs:?}, expected in {:?} / out {:?}",
                    want.inputs, want.outputs
                )));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        if let Some(n) = device.buffer_frames {
            ca::set_u32(aggregate, kAudioDevicePropertyBufferFrameSize, ca::Scope::Global, n).map_err(other)?;
        }
        engine.actual_buffer = ca::get_u32(aggregate, kAudioDevicePropertyBufferFrameSize, ca::Scope::Global).unwrap_or(0);
        if want.follows_volume {
            if let Some(v) = ca::device_by_uid(&device.uid()).and_then(device_volume) {
                engine.volume.store(v.to_bits(), Ordering::Relaxed);
            }
            engine.watcher = Some(VolumeWatcher::start(device.uid(), engine.volume.clone()).map_err(|e| StartError::Other(e.to_string()))?);
        }
        let mut scratch = vec![0.0f32; MAX_FRAMES * want.mix_channels];
        drop(want);
        let (plan, stats, volume, levels) = (engine.plan.clone(), engine.stats.clone(), engine.volume.clone(), engine.levels.clone());
        let mut current = f32::from_bits(volume.load(Ordering::Relaxed));
        // A new routing fades in rather than starting at full level.
        let mut smooth = mix::Smooth::new(engine.members[0].rate).silent();
        let (master, master_now) = (engine.master.clone(), engine.master_now.clone());
        let render: ca::Render = Box::new(move |inputs, outputs| {
            let target = f32::from_bits(volume.load(Ordering::Relaxed));
            let plan = plan.load();
            let m = f32::from_bits(master.load(Ordering::Relaxed));
            let ok = mix::render(&plan, &mut scratch, inputs, outputs, &mut smooth, m, target, &mut current);
            master_now.store(smooth.master().to_bits(), Ordering::Relaxed);
            if let (true, Some(l)) = (ok, &levels) {
                mix::levels(&plan, inputs, outputs, |i, peak| l.raise(i, peak));
            }
            stats.cycles.fetch_add(1, Ordering::Relaxed);
            if !ok {
                stats.silenced.fetch_add(1, Ordering::Relaxed);
            }
        });
        let want = engine.plan.load();
        let usage = ca::StreamUsage { inputs: &want.inputs_used, outputs: &want.outputs_used };
        engine.io = Some(ca::IoProc::start(aggregate, Some(usage), render).map_err(other)?);
        Ok(engine)
    }

    /// Fades every output to silence and waits until it is (at most a few
    /// fades' time, should the IO thread not be running).
    fn fade_out(&self) {
        self.master.store(0f32.to_bits(), Ordering::Relaxed);
        if self.io.is_none() {
            return;
        }
        let deadline = Instant::now() + Duration::from_secs_f64(mix::MASTER_FADE * 3.0);
        while f32::from_bits(self.master_now.load(Ordering::Relaxed)) > 0.0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn fade_in(&self) {
        self.master.store(1f32.to_bits(), Ordering::Relaxed);
    }

    /// A transition (preset switch) the editor asked for and this engine hasn't done.
    fn transition_pending(&self) -> Option<u32> {
        self.levels.as_ref().map(|l| l.transitions()).filter(|&t| t != self.transitions_seen)
    }

    /// Swaps in a new plan; returns whether it differed.
    fn update(&mut self, plan: Plan) -> bool {
        if **self.plan.load() == plan {
            return false;
        }
        if let Some(l) = &self.levels {
            l.set_shape(&shape(&plan));
        }
        let old = self.plan.swap(Arc::new(plan));
        self.retired.push((Instant::now(), old));
        true
    }

    fn housekeeping(&mut self) {
        self.retired.retain(|(at, _)| at.elapsed() < RETIRE_AFTER);
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.io = None; // stop IO before the aggregate goes away
        let _ = ca::destroy_aggregate(self.aggregate);
    }
}

/// All engines, reconciled with the configuration.
#[derive(Default)]
pub struct Engines {
    running: HashMap<String, Engine>,
    warnings: HashMap<String, Vec<String>>,
    failed: HashMap<String, (Vec<Member>, Vec<AppTap>, Instant)>,
    /// App lookups, kept until the config changes.
    apps: HashMap<String, Result<Vec<String>, String>>,
    /// Apps whose tap failed, with why; skipped until the config changes.
    blocked: HashMap<String, String>,
    /// Whether the installed plug-in can capture apps itself (app slots).
    /// Optional device-list fields the installed plug-in understands.
    plugin_fields: Vec<&'static str>,
}

fn member(id: ca::AudioObjectID, uid: String, name: String) -> Member {
    Member {
        uid,
        name,
        rate: ca::get_f64(id, lc_sys::consts::kAudioDevicePropertyNominalSampleRate, ca::Scope::Global).unwrap_or(0.0),
        inputs: ca::stream_channels(id, ca::Scope::Input),
        outputs: ca::stream_channels(id, ca::Scope::Output),
    }
}

impl Engines {
    /// Stops every route cleanly: IO first, then the aggregates and taps.
    pub fn stop_all(&mut self) {
        for engine in self.running.values() {
            engine.master.store(0f32.to_bits(), Ordering::Relaxed);
        }
        // Fade them all out together, then stop.
        for engine in self.running.values() {
            engine.fade_out();
        }
        for (uid, engine) in self.running.drain() {
            drop(engine);
            log!("stopped routing for {uid}");
        }
    }

    /// Forgets failures and app lookups so a new config gets a fresh try.
    pub fn config_changed(&mut self) {
        self.failed.clear();
        self.apps.clear();
        self.blocked.clear();
    }

    /// Looks up the device's app inputs (cached until the config changes).
    fn resolve_apps(&mut self, device: &Device) {
        for s in &device.inputs {
            if let lc_config::InputKind::App { name, .. } = &s.kind {
                self.apps.entry(name.clone()).or_insert_with(|| crate::apps::resolve(name));
            }
        }
    }

    /// The device's app slots in the plug-in; none when it can't capture apps.
    fn slots(&mut self, device: &Device) -> Vec<AppSpec> {
        if !self.plugin_fields.contains(&"apps") {
            return vec![];
        }
        self.resolve_apps(device);
        let cache = &self.apps;
        app_slots(device, &|name| cache.get(name).cloned().unwrap_or_else(|| Err(format!("app '{name}' was not looked up"))))
    }

    /// The device list for the plug-in: the config's devices with their app
    /// slots, if the installed plug-in supports them.
    pub fn specs(&mut self, config: &Config, plugin_fields: Vec<&'static str>) -> Vec<DeviceSpec> {
        if plugin_fields != self.plugin_fields {
            let apps = plugin_fields.contains(&"apps");
            log!("{}", if apps { "the plug-in captures apps itself; app inputs use it" } else { "the plug-in can't capture apps; app inputs use taps" });
            if !plugin_fields.contains(&"apply_gain") {
                log!("the plug-in can't leave its volume to outputs; follow_volume has no effect");
            }
            if !plugin_fields.contains(&"virtuals") {
                log!("the plug-in has no virtual devices; 'virtual: <name>' inputs and outputs are skipped");
            }
            self.plugin_fields = plugin_fields;
        }
        let follow_ok = self.plugin_fields.contains(&"apply_gain");
        let virtuals_ok = self.plugin_fields.contains(&"virtuals");
        let mut specs = config.to_specs();
        for (spec, device) in specs.iter_mut().zip(&config.devices) {
            spec.apps = self.slots(device);
            spec.apply_gain = !(follow_ok && device.volume_follows_outputs());
            if !virtuals_ok {
                spec.virtuals.clear();
            }
        }
        specs
    }

    pub fn reconcile(&mut self, config: Option<&Config>) {
        let wanted: Vec<&Device> = config.map(|c| c.devices.iter().filter(|d| wants_engine(d)).collect()).unwrap_or_default();
        let wanted_uids: HashSet<String> = wanted.iter().map(|d| d.uid()).collect();
        self.running.retain(|uid, engine| {
            let keep = wanted_uids.contains(uid);
            if !keep {
                engine.fade_out();
                log!("stopped routing for {uid}");
            }
            keep
        });
        self.warnings.retain(|uid, _| wanted_uids.contains(uid));
        self.failed.retain(|uid, _| wanted_uids.contains(uid));
        if wanted.is_empty() {
            return;
        }

        let present: Vec<Member> = match ca::devices() {
            Ok(list) => list.into_iter().filter(|d| !d.uid.starts_with(AGGREGATE_PREFIX)).map(|d| member(d.id, d.uid, d.name)).collect(),
            Err(e) => {
                log!("cannot list audio devices: {e}");
                return;
            }
        };
        for device in wanted {
            self.reconcile_one(device, &present);
        }
    }

    fn reconcile_one(&mut self, device: &Device, present: &[Member]) {
        let uid = device.uid();
        let twin_uid = format!("{uid}{TWIN_SUFFIX}");
        let Some(twin_id) = ca::device_by_uid(&twin_uid) else {
            self.warn(&uid, vec![format!("'{}': waiting for the device to appear", device.name)]);
            return;
        };
        let twin = member(twin_id, twin_uid, format!("{} (LoopCraft engine)", device.name));
        // Other LoopCraft devices may join (chaining), but never the device
        // itself or its own virtual devices.
        let own = format!("{uid}/");
        let others: Vec<Member> = present.iter().filter(|m| m.uid != uid && !m.uid.starts_with(&own)).cloned().collect();
        let slots = self.slots(device);
        self.resolve_apps(device);
        let resolve = |name: &str| match self.blocked.get(name) {
            Some(why) => Err(why.clone()),
            None => self.apps.get(name).cloned().unwrap_or_else(|| Err(format!("app '{name}' was not looked up"))),
        };
        let caps = Caps { slots: &slots, follow_volume: self.plugin_fields.contains(&"apply_gain"), virtuals: self.plugin_fields.contains(&"virtuals") };
        let layout = match layout(device, &twin, &others, &resolve, &caps) {
            Ok(l) => l,
            Err(e) => {
                self.warn(&uid, vec![e]);
                return;
            }
        };
        self.warn(&uid, layout.warnings.clone());

        if let Some(engine) = self.running.get_mut(&uid) {
            let alive = ca::is_alive(engine.aggregate);
            let silenced = engine.stats.silenced.load(Ordering::Relaxed);
            let newly_silenced = silenced - engine.silenced_seen;
            engine.silenced_seen = silenced;
            if newly_silenced > 0 {
                log!("'{}': {newly_silenced} IO cycle(s) silenced because the buffers did not match the plan", device.name);
            }
            let plan = engine.plan.load();
            let same_streams = plan.inputs_used == layout.plan.inputs_used && plan.outputs_used == layout.plan.outputs_used;
            drop(plan);
            let same_buffer = engine.buffer_frames == device.buffer_frames
                && engine.watcher.is_some() == layout.plan.follows_volume;
            if alive && engine.members == layout.members && engine.taps == layout.taps && same_buffer && same_streams && newly_silenced == 0 {
                // A preset switch fades all outputs out, changes, fades back in;
                // anything else changes under the mix's own per-route fades.
                let changed = **engine.plan.load() != layout.plan;
                match engine.transition_pending() {
                    Some(t) if changed => {
                        engine.fade_out();
                        engine.update(layout.plan);
                        engine.fade_in();
                        engine.transitions_seen = t;
                        log!("'{}': routing updated (faded out and in)", device.name);
                    }
                    _ if changed => {
                        engine.update(layout.plan);
                        log!("'{}': routing updated", device.name);
                    }
                    _ => {}
                }
                engine.housekeeping();
                return;
            }
            let why = if !alive {
                "its aggregate disappeared".to_string()
            } else if engine.members != layout.members {
                format!("members changed to {}", names(&layout.members))
            } else if !same_buffer {
                "its buffer size or volume following changed".to_string()
            } else if engine.taps != layout.taps {
                "its app inputs changed".to_string()
            } else if !same_streams {
                "the streams in use changed".to_string()
            } else {
                "its buffer layout changed".to_string()
            };
            log!("'{}': rebuilding routing ({why})", device.name);
            // Fade out rather than cut; the new routing fades in.
            engine.fade_out();
            self.running.remove(&uid);
        }

        if let Some((members, taps, at)) = self.failed.get(&uid) {
            if *members == layout.members && *taps == layout.taps && at.elapsed() < RETRY_AFTER {
                return;
            }
        }
        let (members, taps) = (layout.members.clone(), layout.taps.clone());
        let started = Instant::now();
        match Engine::start(device, layout) {
            Ok(engine) => {
                let apps: Vec<&str> = taps.iter().map(|t| t.name.as_str()).collect();
                let apps = if apps.is_empty() { String::new() } else { format!("; apps {}", apps.join(", ")) };
                let buffer = engine.actual_buffer;
                log!(
                    "'{}': routing started in {} ms ({}{apps}); IO buffer {buffer} frames ({:.1} ms)",
                    device.name,
                    started.elapsed().as_millis(),
                    names(&members),
                    buffer as f64 / f64::from(device.sample_rate) * 1000.0
                );
                self.failed.remove(&uid);
                self.running.insert(uid, engine);
            }
            Err(StartError::Tap { app, error }) => {
                // Route everything else; this app waits for a config change.
                log!("'{}': cannot capture app '{app}': {error}", device.name);
                self.blocked.insert(app, format!("could not be captured ({error}); save the config to retry"));
                self.reconcile_one(device, present);
            }
            Err(StartError::Other(e)) => {
                log!("'{}': cannot start routing: {e}", device.name);
                self.failed.insert(uid, (members, taps, Instant::now()));
            }
        }
    }

    /// Logs a device's warnings when they change.
    fn warn(&mut self, uid: &str, warnings: Vec<String>) {
        if self.warnings.get(uid) != Some(&warnings) {
            for w in &warnings {
                log!("warning: {w}");
            }
            self.warnings.insert(uid.to_string(), warnings);
        }
    }
}

fn names(members: &[Member]) -> String {
    members.iter().map(|m| m.name.as_str()).collect::<Vec<_>>().join(" + ")
}
