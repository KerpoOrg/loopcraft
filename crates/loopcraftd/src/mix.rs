//! The real-time mix of one engine.
//!
//! Each cycle, sources are summed into the device's output channels (a
//! scratch buffer), which then go to the twin's output (what recording apps
//! receive) and are copied to the monitors. Everything here runs on the IO
//! thread: no allocation, locks or syscalls.

/// A buffer list as seen by the mix: interleaved buffers with channel counts.
pub trait Inputs {
    fn count(&self) -> usize;
    fn get(&self, i: usize) -> Option<(&[f32], usize)>;
}

pub trait Outputs {
    fn count(&self) -> usize;
    fn get_mut(&mut self, i: usize) -> Option<(&mut [f32], usize)>;
}

/// One channel connection with its gain.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Route {
    /// Buffer index in the aggregate's input (sources) or output (monitors) list.
    pub buf: usize,
    /// Channel within that buffer.
    pub ch: usize,
    /// Device output channel (0-based).
    pub mix_ch: usize,
    pub gain: f32,
    /// Also scaled by the device's volume (outputs with `follow_volume`).
    pub follows: bool,
}

/// Everything the IO thread needs, precomputed by the control thread.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    /// The device's output channels.
    pub mix_channels: usize,
    /// Output buffer that feeds the device's output channels (the twin's).
    pub twin_out: usize,
    /// Expected channels of each input and output buffer; any other layout
    /// means the aggregate changed under us, and the cycle is silenced.
    pub inputs: Vec<usize>,
    pub outputs: Vec<usize>,
    /// Buffers the routes use. The others are switched off in the aggregate
    /// (their buffers arrive as null) and are not checked.
    pub inputs_used: Vec<bool>,
    pub outputs_used: Vec<bool>,
    /// Input buffer channel → device output channel.
    pub sources: Vec<Route>,
    /// Device output channel → output buffer channel.
    pub monitors: Vec<Route>,
    /// Some output follows the device's volume, so it is watched. From the
    /// config, so muting that output doesn't change it (or rebuild anything).
    pub follows_volume: bool,
    /// What the meters read, per channel of each configured input and output
    /// (in config order; disabled or missing ones have none): an input or
    /// output buffer and channel. The busses are read from the twin's output.
    pub meter_inputs: Vec<Vec<Option<(usize, usize)>>>,
    pub meter_outputs: Vec<Vec<Option<(usize, usize)>>>,
    /// Each input's gain (not its mute): its meters show the level after it.
    /// Outputs are metered on their buffers, after everything.
    pub meter_gains: Vec<f32>,
}

/// The gains the mix is playing at. A change of gain (mute, unmute, a knob,
/// a new patch) fades toward the plan's over a few milliseconds instead of
/// stepping, which would click. Kept by the IO thread from cycle to cycle;
/// it allocates only if a plan has more routes than it was made for.
pub struct Smooth {
    /// How far each sample moves toward the target: 1 − e^(−1/(τ·rate)), τ = 3 ms.
    k: f32,
    /// Before the first plan: it plays as it is, without fading in.
    primed: bool,
    /// The master gain on every output (busses included), and how much it
    /// moves per sample: a linear fade over [`MASTER_FADE`].
    master: f32,
    master_step: f32,
    sources: Vec<(RouteKey, f32)>,
    monitors: Vec<(RouteKey, f32)>,
    spare: Vec<(RouteKey, f32)>,
}

/// A route's identity across plans: where it reads, where it goes.
type RouteKey = (usize, usize, usize, bool);

fn key(r: &Route) -> RouteKey {
    (r.buf, r.ch, r.mix_ch, r.follows)
}

/// Gains closer than this are the same (−120 dB).
const SETTLED: f32 = 1e-6;

/// How long the master takes from full to silent and back: long enough not
/// to click, short enough to feel immediate.
pub const MASTER_FADE: f64 = 0.030;

impl Smooth {
    pub fn new(sample_rate: f64) -> Smooth {
        const ROUTES: usize = 4096;
        Smooth {
            k: (1.0 - (-1.0 / (0.003 * sample_rate)).exp()) as f32,
            primed: false,
            master: 1.0,
            master_step: (1.0 / (MASTER_FADE * sample_rate)) as f32,
            sources: Vec::with_capacity(ROUTES),
            monitors: Vec::with_capacity(ROUTES),
            spare: Vec::with_capacity(ROUTES),
        }
    }

    /// Starting from silence, so a new engine fades in.
    pub fn silent(mut self) -> Smooth {
        self.master = 0.0;
        self
    }

    /// The master gain now.
    pub fn master(&self) -> f32 {
        self.master
    }

    /// Lines the gains up with a plan's routes: a route that was there keeps
    /// its gain (and fades from it), a new one fades in from silence.
    fn sync(list: &mut Vec<(RouteKey, f32)>, spare: &mut Vec<(RouteKey, f32)>, routes: &[Route], primed: bool) {
        if list.len() == routes.len() && list.iter().zip(routes).all(|((k, _), r)| *k == key(r)) {
            return;
        }
        spare.clear();
        for r in routes {
            let k = key(r);
            let g = if primed { list.iter().find(|(o, _)| *o == k).map_or(0.0, |&(_, g)| g) } else { r.gain };
            spare.push((k, g));
        }
        std::mem::swap(list, spare);
    }
}

/// One route's samples at its fading gain: `apply(frame, gain)` per frame.
#[inline]
fn fade(gain: &mut f32, target: f32, k: f32, frames: usize, mut apply: impl FnMut(usize, f32)) {
    if *gain == target {
        if target != 0.0 {
            (0..frames).for_each(|f| apply(f, target));
        }
        return;
    }
    let mut g = *gain;
    for f in 0..frames {
        g += (target - g) * k;
        apply(f, g);
    }
    *gain = if (target - g).abs() < SETTLED { target } else { g };
}

/// Mixes one cycle. `scratch` must hold `frames × mix_channels` samples.
/// Gain changes fade (see [`Smooth`]). Routes that follow the device volume
/// also ramp linearly from `volume` (the previous cycle's) to `target` over
/// the cycle; `volume` is then set to `target`. Returns false (and outputs
/// silence) when the buffers don't match the plan.
// Each argument is a separate piece of the IO thread's per-cycle state.
#[allow(clippy::too_many_arguments)]
pub fn render(
    plan: &Plan,
    scratch: &mut [f32],
    inputs: &dyn Inputs,
    outputs: &mut dyn Outputs,
    smooth: &mut Smooth,
    master: f32,
    target: f32,
    volume: &mut f32,
) -> bool {
    let frames = match layout_frames(plan, inputs, outputs) {
        Some(f) if f * plan.mix_channels <= scratch.len() => f,
        _ => {
            silence(outputs);
            return false;
        }
    };
    // While the master is silent nothing is heard, so a new plan's gains apply at once.
    let fade_gains = smooth.primed && smooth.master > 0.0;
    Smooth::sync(&mut smooth.sources, &mut smooth.spare, &plan.sources, fade_gains);
    Smooth::sync(&mut smooth.monitors, &mut smooth.spare, &plan.monitors, fade_gains);
    smooth.primed = true;
    let k = smooth.k;
    let n = plan.mix_channels;
    let mix = &mut scratch[..frames * n];
    mix.fill(0.0);
    for (r, (_, gain)) in plan.sources.iter().zip(smooth.sources.iter_mut()) {
        let Some((buf, ch)) = inputs.get(r.buf) else { continue };
        fade(gain, r.gain, k, frames, |f, g| mix[f * n + r.mix_ch] += buf[f * ch + r.ch] * g);
    }
    let step = (target - *volume) / frames as f32;
    for i in 0..outputs.count() {
        let Some((buf, ch)) = outputs.get_mut(i) else { continue };
        if i == plan.twin_out {
            buf[..mix.len()].copy_from_slice(mix);
            continue;
        }
        buf.fill(0.0);
        for (r, (_, gain)) in plan.monitors.iter().zip(smooth.monitors.iter_mut()).filter(|(r, _)| r.buf == i) {
            let v = *volume;
            fade(gain, r.gain, k, frames, |f, g| {
                let g = if r.follows { g * (v + step * (f + 1) as f32) } else { g };
                buf[f * ch + r.ch] += mix[f * n + r.mix_ch] * g;
            });
        }
    }
    *volume = target;
    // The master: every output buffer the same, the busses apps record included.
    let (from, step) = (smooth.master, smooth.master_step);
    if from != 1.0 || master != 1.0 {
        let at = |f: usize| {
            let moved = step * (f + 1) as f32;
            if master > from { (from + moved).min(master) } else { (from - moved).max(master) }
        };
        for i in 0..outputs.count() {
            if let Some((buf, ch)) = outputs.get_mut(i) {
                for (f, frame) in buf.chunks_exact_mut(ch).enumerate() {
                    let m = at(f);
                    frame.iter_mut().for_each(|s| *s *= m);
                }
            }
        }
        smooth.master = at(frames - 1);
    }
    true
}

/// Peaks of this cycle for the meters, in the order of the levels file:
/// every input channel, every bus, every output channel. Call after `render`.
pub fn levels(plan: &Plan, inputs: &dyn Inputs, outputs: &mut dyn Outputs, mut raise: impl FnMut(usize, f32)) {
    fn peak(buf: &[f32], ch: usize, c: usize) -> f32 {
        buf.iter().skip(c).step_by(ch.max(1)).fold(0.0f32, |m, s| m.max(s.abs()))
    }
    let mut slot = 0;
    for (k, unit) in plan.meter_inputs.iter().enumerate() {
        let g = plan.meter_gains.get(k).copied().unwrap_or(1.0);
        for &at in unit {
            if let Some((b, c)) = at {
                if let Some((buf, ch)) = inputs.get(b).filter(|(_, ch)| c < *ch) {
                    raise(slot, peak(buf, ch, c) * g);
                }
            }
            slot += 1;
        }
    }
    if let Some((buf, ch)) = outputs.get_mut(plan.twin_out) {
        for c in 0..plan.mix_channels.min(ch) {
            raise(slot + c, peak(buf, ch, c));
        }
    }
    slot += plan.mix_channels;
    for unit in &plan.meter_outputs {
        for &at in unit {
            if let Some((b, c)) = at {
                if let Some((buf, ch)) = outputs.get_mut(b).filter(|(_, ch)| c < *ch) {
                    raise(slot, peak(buf, ch, c));
                }
            }
            slot += 1;
        }
    }
}

/// Frames in this cycle, if every used buffer has the planned channel count
/// and all agree on the length.
fn layout_frames(plan: &Plan, inputs: &dyn Inputs, outputs: &mut dyn Outputs) -> Option<usize> {
    if inputs.count() != plan.inputs.len() || outputs.count() != plan.outputs.len() {
        return None;
    }
    let mut frames = None;
    let mut agree = |len: usize, ch: usize, want: usize| {
        if ch != want || ch == 0 || !len.is_multiple_of(ch) {
            return false;
        }
        *frames.get_or_insert(len / ch) == len / ch
    };
    for (i, &want) in plan.inputs.iter().enumerate().filter(|(i, _)| plan.inputs_used[*i]) {
        let (b, ch) = inputs.get(i)?;
        if !agree(b.len(), ch, want) {
            return None;
        }
    }
    for (i, &want) in plan.outputs.iter().enumerate().filter(|(i, _)| plan.outputs_used[*i]) {
        let (b, ch) = outputs.get_mut(i)?;
        if !agree(b.len(), ch, want) {
            return None;
        }
    }
    frames
}

fn silence(outputs: &mut dyn Outputs) {
    for i in 0..outputs.count() {
        if let Some((b, _)) = outputs.get_mut(i) {
            b.fill(0.0);
        }
    }
}

/// A level in dB to a linear amplitude gain: 0 dB → 1, −6.02 dB → ½, −∞ → 0.
pub fn gain(db: f64) -> f32 {
    if db == f64::NEG_INFINITY {
        0.0
    } else {
        10f64.powf(db / 20.0) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Buffers like Core Audio's; an empty buffer stands for a switched-off
    /// stream (null data).
    struct Bufs(Vec<(Vec<f32>, usize)>);

    impl Inputs for Bufs {
        fn count(&self) -> usize {
            self.0.len()
        }
        fn get(&self, i: usize) -> Option<(&[f32], usize)> {
            self.0.get(i).filter(|(b, _)| !b.is_empty()).map(|(b, c)| (b.as_slice(), *c))
        }
    }

    impl Outputs for Bufs {
        fn count(&self) -> usize {
            self.0.len()
        }
        fn get_mut(&mut self, i: usize) -> Option<(&mut [f32], usize)> {
            self.0.get_mut(i).filter(|(b, _)| !b.is_empty()).map(|(b, c)| (b.as_mut_slice(), *c))
        }
    }

    /// `frames` frames of `ch` channels where channel c of frame f is `base + 10c + f`.
    fn ramp(frames: usize, ch: usize, base: f32) -> (Vec<f32>, usize) {
        ((0..frames).flat_map(|f| (0..ch).map(move |c| base + 10.0 * c as f32 + f as f32)).collect(), ch)
    }

    fn route(buf: usize, ch: usize, mix_ch: usize, gain: f32) -> Route {
        Route { buf, ch, mix_ch, gain, follows: false }
    }

    /// Like the reference setup: twin (2 pass-thru in, 4 out), a 2-in
    /// interface and a 2-out headset.
    fn plan() -> Plan {
        Plan {
            mix_channels: 4,
            twin_out: 0,
            inputs: vec![2, 2],
            outputs: vec![4, 2],
            inputs_used: vec![true, true],
            outputs_used: vec![true, true],
            sources: vec![route(0, 0, 0, 1.0), route(0, 1, 1, 1.0), route(1, 0, 2, 0.5), route(1, 0, 3, 0.5), route(1, 1, 0, 1.0)],
            monitors: vec![route(1, 0, 2, 1.0), route(1, 1, 3, 0.25)],
            follows_volume: false,
            meter_inputs: vec![vec![Some((0, 0)), Some((0, 1))], vec![], vec![Some((1, 0)), Some((1, 1))]],
            meter_outputs: vec![vec![Some((1, 0)), None]],
            meter_gains: vec![],
        }
    }

    #[test]
    fn meters_read_inputs_busses_and_outputs() {
        let inputs = Bufs(vec![(vec![0.1, -0.2, 0.3, 0.0], 2), (vec![0.5, 0.0, -0.7, 0.0], 2)]);
        let mut outputs = Bufs(vec![(vec![0.0; 8], 4), (vec![0.0; 4], 2)]);
        let p = plan();
        assert!(render(&p, &mut [0.0; 64], &inputs, &mut outputs, &mut Smooth::new(48_000.0), 1.0, 1.0, &mut 1.0));
        let mut got = [0.0f32; 2 + 2 + 4 + 2];
        levels(&p, &inputs, &mut outputs, |i, v| got[i] = got[i].max(v));
        // inputs: twin 0.3 / 0.2, interface 0.7 / 0
        assert_eq!(got[..4], [0.3, 0.2, 0.7, 0.0]);
        // busses: 1 = pt L + hw R; 3, 4 = hw L / 2
        assert_eq!(got[4..8], [0.3, 0.2, 0.35, 0.35]);
        // headset L = bus 3; the second channel has no meter
        assert_eq!(got[8..], [0.35, 0.0]);
    }

    #[test]
    fn mixes_sources_and_feeds_monitors() {
        let frames = 3;
        let inputs = Bufs(vec![ramp(frames, 2, 100.0), ramp(frames, 2, 1000.0)]);
        let mut outputs = Bufs(vec![(vec![7.0; frames * 4], 4), (vec![7.0; frames * 2], 2)]);
        let mut scratch = vec![0.0; 64];
        assert!(render(&plan(), &mut scratch, &inputs, &mut outputs, &mut Smooth::new(48_000.0), 1.0, 1.0, &mut 1.0));
        for f in 0..frames {
            let f32_ = f as f32;
            let pt_l = 100.0 + f32_;
            let pt_r = 110.0 + f32_;
            let hw1 = 1000.0 + f32_;
            let hw2 = 1010.0 + f32_;
            let out = &outputs.0[0].0[f * 4..f * 4 + 4];
            assert_eq!(out, [pt_l + hw2, pt_r, hw1 * 0.5, hw1 * 0.5], "frame {f}");
            let mon = &outputs.0[1].0[f * 2..f * 2 + 2];
            assert_eq!(mon, [hw1 * 0.5, hw1 * 0.5 * 0.25], "frame {f}");
        }
    }

    #[test]
    fn unrouted_outputs_are_silent() {
        let mut p = plan();
        p.monitors.clear();
        let inputs = Bufs(vec![ramp(2, 2, 1.0), ramp(2, 2, 1.0)]);
        let mut outputs = Bufs(vec![(vec![7.0; 8], 4), (vec![7.0; 4], 2)]);
        assert!(render(&p, &mut [0.0; 8], &inputs, &mut outputs, &mut Smooth::new(48_000.0), 1.0, 1.0, &mut 1.0));
        assert!(outputs.0[1].0.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn unexpected_layouts_are_silenced() {
        let mut scratch = vec![0.0; 64];
        let cases: Vec<(Bufs, Bufs)> = vec![
            // a buffer missing
            (Bufs(vec![ramp(2, 2, 1.0)]), Bufs(vec![(vec![7.0; 8], 4), (vec![7.0; 4], 2)])),
            // wrong channel count
            (Bufs(vec![ramp(2, 2, 1.0), ramp(2, 3, 1.0)]), Bufs(vec![(vec![7.0; 8], 4), (vec![7.0; 4], 2)])),
            // buffers disagree on the frame count
            (Bufs(vec![ramp(2, 2, 1.0), ramp(3, 2, 1.0)]), Bufs(vec![(vec![7.0; 8], 4), (vec![7.0; 4], 2)])),
        ];
        for (inputs, mut outputs) in cases {
            assert!(!render(&plan(), &mut scratch, &inputs, &mut outputs, &mut Smooth::new(48_000.0), 1.0, 1.0, &mut 1.0));
            assert!(outputs.0.iter().all(|(b, _)| b.iter().all(|&s| s == 0.0)));
        }
        // More frames than the scratch buffer holds.
        let inputs = Bufs(vec![ramp(20, 2, 1.0), ramp(20, 2, 1.0)]);
        let mut outputs = Bufs(vec![(vec![7.0; 80], 4), (vec![7.0; 40], 2)]);
        assert!(!render(&plan(), &mut scratch, &inputs, &mut outputs, &mut Smooth::new(48_000.0), 1.0, 1.0, &mut 1.0));
    }

    #[test]
    fn switched_off_streams_are_skipped() {
        // Only the twin is used: the interface's input and the headset's
        // output are switched off and arrive as null buffers.
        let p = Plan {
            mix_channels: 4,
            twin_out: 0,
            inputs: vec![2, 2],
            outputs: vec![4, 2],
            inputs_used: vec![true, false],
            outputs_used: vec![true, false],
            sources: vec![route(0, 0, 0, 1.0)],
            monitors: vec![],
            follows_volume: false,
            meter_inputs: vec![],
            meter_outputs: vec![],
            meter_gains: vec![],
        };
        let inputs = Bufs(vec![ramp(2, 2, 1.0), (vec![], 2)]);
        let mut outputs = Bufs(vec![(vec![7.0; 8], 4), (vec![], 2)]);
        assert!(render(&p, &mut [0.0; 8], &inputs, &mut outputs, &mut Smooth::new(48_000.0), 1.0, 1.0, &mut 1.0));
        assert_eq!(outputs.0[0].0, [1.0, 0.0, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0]);
        // A used stream arriving as null is a layout problem.
        let inputs = Bufs(vec![(vec![], 2), (vec![], 2)]);
        assert!(!render(&p, &mut [0.0; 8], &inputs, &mut outputs, &mut Smooth::new(48_000.0), 1.0, 1.0, &mut 1.0));
    }

    #[test]
    fn following_outputs_ramp_to_the_device_volume() {
        let mut p = plan();
        p.monitors = vec![Route { follows: true, ..route(1, 0, 0, 0.5) }, route(1, 1, 1, 1.0)];
        let frames = 4;
        let inputs = Bufs(vec![(vec![1.0; frames * 2], 2), (vec![0.0; frames * 2], 2)]);
        let mut outputs = Bufs(vec![(vec![0.0; frames * 4], 4), (vec![0.0; frames * 2], 2)]);
        let mut volume = 1.0;
        assert!(render(&p, &mut [0.0; 64], &inputs, &mut outputs, &mut Smooth::new(48_000.0), 1.0, 0.0, &mut volume));
        let follow: Vec<f32> = outputs.0[1].0.chunks(2).map(|f| f[0]).collect();
        let fixed: Vec<f32> = outputs.0[1].0.chunks(2).map(|f| f[1]).collect();
        assert_eq!(follow, [0.375, 0.25, 0.125, 0.0], "gain 0.5 ramped from volume 1 to 0");
        assert_eq!(fixed, [1.0; 4], "the other output keeps its gain");
        assert_eq!(volume, 0.0);
        assert!(outputs.0[0].0.iter().step_by(4).all(|&s| s == 1.0), "recording busses are not affected");
        // Steady state: no ramp.
        assert!(render(&p, &mut [0.0; 64], &inputs, &mut outputs, &mut Smooth::new(48_000.0), 1.0, 0.0, &mut volume));
        assert!(outputs.0[1].0.chunks(2).all(|f| f[0] == 0.0));
    }

    /// One input channel straight to one output channel through the mix.
    fn one_route(gain: f32) -> Plan {
        Plan {
            mix_channels: 1,
            twin_out: 0,
            inputs: vec![1],
            outputs: vec![1, 1],
            inputs_used: vec![true],
            outputs_used: vec![true, true],
            sources: vec![route(0, 0, 0, 1.0)],
            monitors: vec![route(1, 0, 0, gain)],
            follows_volume: false,
            meter_inputs: vec![],
            meter_outputs: vec![],
            meter_gains: vec![],
        }
    }

    fn play(p: &Plan, smooth: &mut Smooth, frames: usize) -> Vec<f32> {
        let inputs = Bufs(vec![(vec![1.0; frames], 1)]);
        let mut outputs = Bufs(vec![(vec![0.0; frames], 1), (vec![0.0; frames], 1)]);
        assert!(render(p, &mut vec![0.0; frames], &inputs, &mut outputs, smooth, 1.0, 1.0, &mut 1.0));
        outputs.0[1].0.clone()
    }

    #[test]
    fn gain_changes_fade_instead_of_stepping() {
        let mut smooth = Smooth::new(48_000.0);
        assert_eq!(play(&one_route(1.0), &mut smooth, 24), [1.0; 24], "the first plan plays as it is");
        // Mute: no step to silence, a fade over a few milliseconds.
        let muted = play(&one_route(0.0), &mut smooth, 24);
        assert!(muted[0] > 0.9 && muted[0] < 1.0, "first sample {}", muted[0]);
        assert!(muted.windows(2).all(|w| w[1] < w[0]), "falls smoothly");
        let mut last = 1.0;
        for _ in 0..200 {
            last = *play(&one_route(0.0), &mut smooth, 24).last().unwrap();
        }
        assert_eq!(last, 0.0, "and arrives at silence");
        // Unmute fades back up the same way.
        let up = play(&one_route(1.0), &mut smooth, 24);
        assert!(up[0] > 0.0 && up[0] < 0.1 && up.windows(2).all(|w| w[1] > w[0]));
    }

    #[test]
    fn a_new_route_fades_in_and_others_keep_their_gain() {
        let mut smooth = Smooth::new(48_000.0);
        let mut p = one_route(1.0);
        p.mix_channels = 2;
        p.outputs = vec![2, 1];
        p.sources = vec![route(0, 0, 1, 1.0)];
        p.monitors = vec![route(1, 0, 1, 1.0)];
        let inputs = Bufs(vec![(vec![1.0; 24], 1)]);
        let mut outputs = Bufs(vec![(vec![0.0; 48], 2), (vec![0.0; 24], 1)]);
        assert!(render(&p, &mut [0.0; 64], &inputs, &mut outputs, &mut smooth, 1.0, 1.0, &mut 1.0));
        // Patch the input onto mix channel 0 as well: the new route comes in
        // with a fade; the one that was there plays on at full gain.
        p.sources.insert(0, route(0, 0, 0, 1.0));
        assert!(render(&p, &mut [0.0; 64], &inputs, &mut outputs, &mut smooth, 1.0, 1.0, &mut 1.0));
        let new: Vec<f32> = outputs.0[0].0.chunks(2).map(|f| f[0]).collect();
        let old: Vec<f32> = outputs.0[0].0.chunks(2).map(|f| f[1]).collect();
        assert!(new[0] > 0.0 && new[0] < 0.1 && new.windows(2).all(|w| w[1] > w[0]), "{new:?}");
        assert_eq!(old, [1.0; 24]);
    }

    #[test]
    fn master_fades_every_output_in_30_ms() {
        let mut smooth = Smooth::new(48_000.0);
        let p = one_route(1.0);
        let inputs = Bufs(vec![(vec![1.0; 480], 1)]);
        let cycle = |smooth: &mut Smooth, master: f32| {
            let mut outputs = Bufs(vec![(vec![0.0; 480], 1), (vec![0.0; 480], 1)]);
            assert!(render(&p, &mut [0.0; 480], &inputs, &mut outputs, smooth, master, 1.0, &mut 1.0));
            (outputs.0[0].0.clone(), outputs.0[1].0.clone())
        };
        assert!(cycle(&mut smooth, 1.0).1.iter().all(|&s| s == 1.0));
        // Fading out: 10 ms in, two thirds of the way; both the busses
        // (what recording apps get) and the hardware output.
        let (busses, out) = cycle(&mut smooth, 0.0);
        assert!((out[479] - 2.0 / 3.0).abs() < 0.01, "{}", out[479]);
        assert_eq!(busses, out);
        assert!(out.windows(2).all(|w| w[1] < w[0]));
        cycle(&mut smooth, 0.0);
        assert_eq!(*cycle(&mut smooth, 0.0).1.last().unwrap(), 0.0, "silent after 30 ms");
        // While silent, plan changes apply at once (nothing to hear), then it fades in.
        let mut faded_in = cycle(&mut smooth, 1.0).1;
        assert!(faded_in[0] > 0.0 && faded_in[0] < 0.01);
        faded_in = cycle(&mut smooth, 1.0).1;
        assert!(faded_in[479] < 1.0);
        assert_eq!(*cycle(&mut smooth, 1.0).1.last().unwrap(), 1.0);
    }

    #[test]
    fn a_new_engine_starts_silent_and_fades_in() {
        let mut smooth = Smooth::new(48_000.0).silent();
        assert_eq!(play(&one_route(1.0), &mut smooth, 24)[0], 1.0 / 1440.0);
    }

    #[test]
    fn decibels_to_gain() {
        assert_eq!(gain(0.0), 1.0);
        assert_eq!(gain(f64::NEG_INFINITY), 0.0);
        assert!((gain(-20.0) - 0.1).abs() < 1e-7);
        assert!((gain(20.0 * 2f64.log10()) - 2.0).abs() < 1e-6, "+6.02 dB doubles the amplitude");
        assert!((gain(3.0).powi(2) - 2.0).abs() < 0.01, "+3 dB doubles the power");
        assert!((gain(12.0) - 3.981).abs() < 1e-3);
    }
}
