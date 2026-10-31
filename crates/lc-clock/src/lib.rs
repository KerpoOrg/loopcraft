//! Clock maths for LoopCraft devices.
//!
//! A device clock is a straight line through an anchor point
//! (host time, sample time) with the nominal sample rate as its slope. Zero
//! timestamps are points on that line, aligned to whole periods, and the seed
//! changes whenever the line is re-anchored so the HAL knows to resynchronise.

/// Conversion between host ticks (`mach_absolute_time`) and seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HostTimeBase {
    ticks_per_second: f64,
}

impl HostTimeBase {
    pub fn new(ticks_per_second: f64) -> Self {
        Self { ticks_per_second }
    }

    /// Time base of the running machine.
    #[cfg(target_os = "macos")]
    pub fn system() -> Self {
        let mut info = MachTimebaseInfo { numer: 0, denom: 0 };
        // SAFETY: plain out-parameter call.
        unsafe { mach_timebase_info(&mut info) };
        // ns = ticks * numer / denom  =>  ticks per second = 1e9 * denom / numer
        Self::new(1e9 * info.denom as f64 / info.numer as f64)
    }

    pub fn ticks_per_second(&self) -> f64 {
        self.ticks_per_second
    }
}

/// Current host time in ticks.
#[cfg(target_os = "macos")]
pub fn host_now() -> u64 {
    // SAFETY: no arguments, no side effects.
    unsafe { mach_absolute_time() }
}

#[cfg(target_os = "macos")]
#[repr(C)]
struct MachTimebaseInfo {
    numer: u32,
    denom: u32,
}

#[cfg(target_os = "macos")]
extern "C" {
    fn mach_absolute_time() -> u64;
    fn mach_timebase_info(info: *mut MachTimebaseInfo) -> i32;
}

/// A zero timestamp as reported to the HAL.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ZeroTimeStamp {
    pub sample_time: f64,
    pub host_time: u64,
    pub seed: u64,
}

/// Free-running device clock.
#[derive(Clone, Copy, Debug)]
pub struct DeviceClock {
    base: HostTimeBase,
    anchor_host: u64,
    sample_rate: f64,
    period: u32,
    seed: u64,
}

impl DeviceClock {
    /// `period` is the zero-timestamp period in frames.
    pub fn new(base: HostTimeBase, sample_rate: f64, period: u32) -> Self {
        assert!(sample_rate > 0.0 && period > 0);
        Self { base, anchor_host: 0, sample_rate, period, seed: 1 }
    }

    /// Starts a new timeline at `host_time` (sample time 0) and bumps the seed.
    pub fn anchor(&mut self, host_time: u64) {
        self.anchor_host = host_time;
        self.seed = self.seed.wrapping_add(1).max(1);
    }

    /// Changes the rate; the timeline restarts at `host_time`.
    pub fn set_sample_rate(&mut self, sample_rate: f64, host_time: u64) {
        assert!(sample_rate > 0.0);
        self.sample_rate = sample_rate;
        self.anchor(host_time);
    }

    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    pub fn period(&self) -> u32 {
        self.period
    }

    pub fn seed(&self) -> u64 {
        self.seed
    }

    pub fn anchor_host(&self) -> u64 {
        self.anchor_host
    }

    fn ticks_per_frame(&self) -> f64 {
        self.base.ticks_per_second() / self.sample_rate
    }

    /// Sample time (fractional) at `host_time`.
    pub fn sample_at(&self, host_time: u64) -> f64 {
        let elapsed = host_time as i128 - self.anchor_host as i128;
        elapsed as f64 / self.ticks_per_frame()
    }

    /// Host time at `sample_time`, rounded to the nearest tick.
    pub fn host_at(&self, sample_time: f64) -> u64 {
        let ticks = (sample_time * self.ticks_per_frame()).round();
        (self.anchor_host as i128 + ticks as i128).max(0) as u64
    }

    /// The most recent period boundary at or before `now`.
    pub fn zero_timestamp(&self, now: u64) -> ZeroTimeStamp {
        let period = self.period as f64;
        let sample_time = (self.sample_at(now) / period).floor().max(0.0) * period;
        ZeroTimeStamp { sample_time, host_time: self.host_at(sample_time), seed: self.seed }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 24 MHz, the Apple Silicon host clock.
    const TPS: f64 = 24_000_000.0;

    fn clock(rate: f64) -> DeviceClock {
        let mut c = DeviceClock::new(HostTimeBase::new(TPS), rate, 8192);
        c.anchor(1_000_000);
        c
    }

    #[test]
    fn sample_and_host_time_round_trip() {
        let c = clock(48_000.0);
        assert_eq!(c.sample_at(1_000_000), 0.0);
        assert_eq!(c.sample_at(1_000_000 + 24_000_000), 48_000.0);
        assert_eq!(c.host_at(48_000.0), 1_000_000 + 24_000_000);
        assert_eq!(c.host_at(c.sample_at(123_456_789)), 123_456_789);
    }

    #[test]
    fn zero_timestamps_land_on_period_boundaries() {
        let c = clock(48_000.0);
        let one_period_ticks = (8192.0 * TPS / 48_000.0) as u64;
        let z0 = c.zero_timestamp(1_000_000 + one_period_ticks / 2);
        assert_eq!(z0.sample_time, 0.0);
        assert_eq!(z0.host_time, 1_000_000);
        let z2 = c.zero_timestamp(1_000_000 + one_period_ticks * 2 + 10);
        assert_eq!(z2.sample_time, 16_384.0);
        assert_eq!(z2.host_time, c.host_at(16_384.0));
        assert_eq!(z0.seed, z2.seed);
    }

    #[test]
    fn consecutive_zero_timestamps_advance_by_exactly_one_period() {
        let c = clock(44_100.0);
        let mut last = c.zero_timestamp(1_000_000);
        for k in 1..1000u64 {
            let now = 1_000_000 + k * 7_777_777;
            let z = c.zero_timestamp(now);
            assert_eq!(z.sample_time % 8192.0, 0.0);
            assert!(z.sample_time >= last.sample_time);
            assert!(z.host_time <= now);
            last = z;
        }
    }

    #[test]
    fn before_anchor_clamps_to_zero() {
        let c = clock(48_000.0);
        assert_eq!(c.zero_timestamp(10).sample_time, 0.0);
    }

    #[test]
    fn reanchoring_and_rate_changes_bump_the_seed() {
        let mut c = clock(48_000.0);
        let s1 = c.seed();
        c.anchor(5_000_000);
        assert_ne!(c.seed(), s1);
        let s2 = c.seed();
        c.set_sample_rate(96_000.0, 6_000_000);
        assert_ne!(c.seed(), s2);
        assert_eq!(c.sample_at(6_000_000 + 24_000_000), 96_000.0);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn system_time_base_is_sane() {
        let tps = HostTimeBase::system().ticks_per_second();
        assert!((1e6..=1e10).contains(&tps), "ticks per second {tps}");
        let a = host_now();
        let b = host_now();
        assert!(b >= a);
    }
}
