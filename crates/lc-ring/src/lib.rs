//! Time-indexed audio ring.
//!
//! Audio is addressed by absolute sample time instead of read/write cursors: the
//! slot for sample time `t` is `t & (capacity - 1)`. The header records the
//! sample time up to which data is valid (`written_end`). Readers are stateless
//! and may read any window; frames that are too old or not yet written read as
//! silence.
//!
//! Each ring has exactly one writer. Samples are stored as relaxed `AtomicU32`
//! bit patterns, which compile to plain loads and stores on arm64 while keeping
//! concurrent access well defined. The end marker is published with release
//! ordering and read with acquire ordering; readers re-check it after copying
//! and zero any frames that the writer may have overwritten meanwhile.
//!
//! The backing memory is abstracted by [`SharedRegion`] so the same ring can
//! live in process-local memory (M1) or in memory shared between processes
//! (later milestones).

use std::mem::{align_of, size_of};
use std::sync::atomic::{fence, AtomicI64, AtomicU32, AtomicU64, Ordering};

/// Marker for "nothing written yet".
pub const EMPTY: i64 = i64::MIN;

/// Stack staging size for mixing reads (samples, not frames).
const MIX_CHUNK_SAMPLES: usize = 1024;

const MAGIC: u32 = u32::from_be_bytes(*b"LCRG");
const LAYOUT_VERSION: u32 = 1;
/// The header occupies the first 4 KiB of the region; samples follow.
pub const HEADER_BYTES: usize = 4096;

/// Memory that backs a ring.
///
/// # Safety
/// `as_ptr` must point to `len` bytes that stay valid and 8-byte aligned for the
/// lifetime of the value, and the memory must only be accessed through rings.
pub unsafe trait SharedRegion {
    fn as_ptr(&self) -> *mut u8;
    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Process-local, zero-initialised memory.
pub struct HeapRegion {
    words: Box<[AtomicU64]>,
}

impl HeapRegion {
    pub fn new(bytes: usize) -> Self {
        let n = bytes.div_ceil(8);
        let words = (0..n).map(|_| AtomicU64::new(0)).collect();
        Self { words }
    }
}

unsafe impl SharedRegion for HeapRegion {
    fn as_ptr(&self) -> *mut u8 {
        self.words.as_ptr() as *mut u8
    }

    fn len(&self) -> usize {
        self.words.len() * 8
    }
}

#[repr(C)]
struct Header {
    magic: u32,
    layout_version: u32,
    channels: u32,
    capacity: u32,
    sample_rate_bits: AtomicU64,
    generation: AtomicU64,
    written_end: AtomicI64,
    /// End of the write in progress. Raised before any slot is touched, so
    /// readers can tell which frames a concurrent write may be replacing.
    claim_end: AtomicI64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum RingError {
    /// Capacity must be a non-zero power of two.
    BadCapacity,
    BadChannels,
    RegionTooSmall { needed: usize, available: usize },
    Misaligned,
    /// The region does not contain a ring of a known layout.
    BadHeader,
}

/// Bytes needed for a ring of the given shape.
pub fn region_bytes(channels: u32, capacity: u32) -> usize {
    HEADER_BYTES + channels as usize * capacity as usize * size_of::<u32>()
}

pub struct Ring<R: SharedRegion> {
    region: R,
    channels: usize,
    capacity: u32,
    mask: u64,
}

// The ring only hands out atomic access to the region.
unsafe impl<R: SharedRegion + Send> Send for Ring<R> {}
unsafe impl<R: SharedRegion + Sync> Sync for Ring<R> {}

impl<R: SharedRegion> Ring<R> {
    /// Formats `region` as an empty ring.
    pub fn create(region: R, channels: u32, capacity: u32, sample_rate: f64) -> Result<Self, RingError> {
        if capacity == 0 || !capacity.is_power_of_two() {
            return Err(RingError::BadCapacity);
        }
        if channels == 0 {
            return Err(RingError::BadChannels);
        }
        check_region(&region, region_bytes(channels, capacity))?;
        let ring = Self { region, channels: channels as usize, capacity, mask: capacity as u64 - 1 };
        // Plain writes are fine here: no other party can see the ring yet.
        unsafe {
            let h = ring.region.as_ptr() as *mut Header;
            (*h).magic = MAGIC;
            (*h).layout_version = LAYOUT_VERSION;
            (*h).channels = channels;
            (*h).capacity = capacity;
        }
        ring.header().sample_rate_bits.store(sample_rate.to_bits(), Ordering::Relaxed);
        ring.header().generation.store(1, Ordering::Relaxed);
        ring.header().claim_end.store(EMPTY, Ordering::Relaxed);
        ring.header().written_end.store(EMPTY, Ordering::Release);
        Ok(ring)
    }

    /// Opens a ring that another party has already formatted.
    pub fn attach(region: R) -> Result<Self, RingError> {
        check_region(&region, HEADER_BYTES)?;
        let (magic, version, channels, capacity) = unsafe {
            let h = region.as_ptr() as *const Header;
            ((*h).magic, (*h).layout_version, (*h).channels, (*h).capacity)
        };
        if magic != MAGIC || version != LAYOUT_VERSION || channels == 0 || !capacity.is_power_of_two() {
            return Err(RingError::BadHeader);
        }
        check_region(&region, region_bytes(channels, capacity))?;
        Ok(Self { region, channels: channels as usize, capacity, mask: capacity as u64 - 1 })
    }

    pub fn channels(&self) -> u32 {
        self.channels as u32
    }

    pub fn capacity(&self) -> u32 {
        self.capacity
    }

    pub fn sample_rate(&self) -> f64 {
        f64::from_bits(self.header().sample_rate_bits.load(Ordering::Relaxed))
    }

    pub fn generation(&self) -> u64 {
        self.header().generation.load(Ordering::Acquire)
    }

    /// Sample time up to which data is valid, or [`EMPTY`].
    pub fn written_end(&self) -> i64 {
        self.header().written_end.load(Ordering::Acquire)
    }

    /// Discards all audio and records a new sample rate. Writer side only, and
    /// only while no IO is running against the ring.
    pub fn reset(&self, sample_rate: f64) {
        self.header().claim_end.store(EMPTY, Ordering::Relaxed);
        self.header().written_end.store(EMPTY, Ordering::Release);
        self.header().sample_rate_bits.store(sample_rate.to_bits(), Ordering::Relaxed);
        self.header().generation.fetch_add(1, Ordering::AcqRel);
    }

    /// Writes interleaved frames starting at `sample_time`, scaled by `gain`,
    /// replacing whatever the slots held. Writer side only.
    pub fn write(&self, sample_time: i64, frames: &[f32], gain: f32) {
        self.write_impl(sample_time, frames, gain, false)
    }

    /// Like [`Ring::write`] but adds to the existing samples. Writer side only.
    pub fn write_mix(&self, sample_time: i64, frames: &[f32], gain: f32) {
        self.write_impl(sample_time, frames, gain, true)
    }

    fn write_impl(&self, start: i64, frames: &[f32], gain: f32, mix: bool) {
        let n = (frames.len() / self.channels) as i64;
        if n == 0 {
            return;
        }
        let end = start.saturating_add(n);
        let old_end = self.header().written_end.load(Ordering::Relaxed);
        let cap = self.capacity as i64;
        let new_end = if old_end == EMPTY { end } else { old_end.max(end) };

        // Seqlock-style claim: readers that observe any slot store below also
        // observe this claim after their acquire fence.
        self.header().claim_end.store(new_end, Ordering::Relaxed);
        fence(Ordering::Release);

        // Clear the part of the ring that becomes newly valid but is not covered
        // by this write, i.e. a gap between the old end and `start`.
        if old_end != EMPTY && start > old_end {
            let gap_from = old_end.max(end - cap);
            for t in gap_from..start {
                self.clear_frame(t);
            }
        }

        // Only the last `capacity` frames of an oversized write can be kept.
        let first = start.max(end - cap);
        let samples = self.samples();
        for t in first..end {
            let src = &frames[((t - start) as usize) * self.channels..][..self.channels];
            let base = self.slot(t) * self.channels;
            for (c, &s) in src.iter().enumerate() {
                let cell = &samples[base + c];
                let v = if mix && old_end != EMPTY && t < old_end && t >= old_end - cap {
                    f32::from_bits(cell.load(Ordering::Relaxed)) + s * gain
                } else {
                    s * gain
                };
                cell.store(v.to_bits(), Ordering::Relaxed);
            }
        }

        self.header().written_end.store(new_end, Ordering::Release);
    }

    /// Reads interleaved frames for `[sample_time, sample_time + n)` into `out`,
    /// scaled by `gain`. Missing frames read as silence. With `mix`, adds into
    /// `out` instead of overwriting it.
    pub fn read(&self, sample_time: i64, out: &mut [f32], gain: f32, mix: bool) {
        let n = (out.len() / self.channels) as i64;
        if !mix {
            out.fill(0.0);
        }
        let end = self.header().written_end.load(Ordering::Acquire);
        if end == EMPTY || n == 0 {
            return;
        }
        let cap = self.capacity as i64;
        let lo = sample_time.max(end.saturating_sub(cap));
        let hi = sample_time.saturating_add(n).min(end);
        if lo >= hi {
            return;
        }

        if mix {
            // Mixed-in samples cannot be taken back, so stage small chunks on the
            // stack, validate them, and only then add them to `out`.
            let mut stage = [0.0f32; MIX_CHUNK_SAMPLES];
            let chunk_frames = (MIX_CHUNK_SAMPLES / self.channels).max(1) as i64;
            let mut t = lo;
            while t < hi {
                let t_end = (t + chunk_frames).min(hi);
                let len = (t_end - t) as usize * self.channels;
                let valid_from = self.copy_validated(t, t_end, &mut stage[..len], self.channels, gain);
                for (i, s) in stage[..len].iter().enumerate().skip(((valid_from - t) as usize) * self.channels) {
                    out[((t - sample_time) as usize) * self.channels + i] += *s;
                }
                t = t_end;
            }
            return;
        }

        let off = ((lo - sample_time) as usize) * self.channels;
        let len = ((hi - lo) as usize) * self.channels;
        let valid_from = self.copy_validated(lo, hi, &mut out[off..off + len], self.channels, gain);
        out[off..off + ((valid_from - lo) as usize) * self.channels].fill(0.0);
    }

    /// Reads `[sample_time, sample_time + n)` into an interleaved buffer that has
    /// `out_channels` channels: ring channel `c` lands in output channel `c` for
    /// every channel both sides have; other output channels are silent.
    pub fn read_into(&self, sample_time: i64, out: &mut [f32], out_channels: usize, gain: f32) {
        if out_channels == self.channels {
            return self.read(sample_time, out, gain, false);
        }
        out.fill(0.0);
        if out_channels == 0 {
            return;
        }
        let n = (out.len() / out_channels) as i64;
        let end = self.header().written_end.load(Ordering::Acquire);
        if end == EMPTY || n == 0 {
            return;
        }
        let lo = sample_time.max(end.saturating_sub(self.capacity as i64));
        let hi = sample_time.saturating_add(n).min(end);
        if lo >= hi {
            return;
        }
        let off = ((lo - sample_time) as usize) * out_channels;
        let len = ((hi - lo) as usize) * out_channels;
        let valid_from = self.copy_validated(lo, hi, &mut out[off..off + len], out_channels, gain);
        out[off..off + ((valid_from - lo) as usize) * out_channels].fill(0.0);
    }

    /// Copies frames `[lo, hi)` into `dst` and returns the first sample time
    /// whose data is guaranteed intact; frames before it may have been replaced
    /// by a concurrent write and must be discarded by the caller.
    fn copy_validated(&self, lo: i64, hi: i64, dst: &mut [f32], dst_channels: usize, gain: f32) -> i64 {
        let samples = self.samples();
        let shared = self.channels.min(dst_channels);
        for t in lo..hi {
            let base = self.slot(t) * self.channels;
            let d = &mut dst[((t - lo) as usize) * dst_channels..][..shared];
            for (c, v) in d.iter_mut().enumerate() {
                *v = f32::from_bits(samples[base + c].load(Ordering::Relaxed)) * gain;
            }
        }
        fence(Ordering::Acquire);
        let claim = self.header().claim_end.load(Ordering::Relaxed);
        claim.saturating_sub(self.capacity as i64).clamp(lo, hi)
    }

    fn clear_frame(&self, t: i64) {
        let base = self.slot(t) * self.channels;
        for cell in &self.samples()[base..base + self.channels] {
            cell.store(0, Ordering::Relaxed);
        }
    }

    #[inline]
    fn slot(&self, t: i64) -> usize {
        ((t as u64) & self.mask) as usize
    }

    fn header(&self) -> &Header {
        unsafe { &*(self.region.as_ptr() as *const Header) }
    }

    fn samples(&self) -> &[AtomicU32] {
        unsafe {
            std::slice::from_raw_parts(
                self.region.as_ptr().add(HEADER_BYTES) as *const AtomicU32,
                self.channels * self.capacity as usize,
            )
        }
    }
}

fn check_region<R: SharedRegion>(region: &R, needed: usize) -> Result<(), RingError> {
    if region.len() < needed {
        return Err(RingError::RegionTooSmall { needed, available: region.len() });
    }
    if !(region.as_ptr() as usize).is_multiple_of(align_of::<AtomicU64>()) {
        return Err(RingError::Misaligned);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ring(channels: u32, capacity: u32) -> Ring<HeapRegion> {
        Ring::create(HeapRegion::new(region_bytes(channels, capacity)), channels, capacity, 48_000.0).unwrap()
    }

    fn ramp(n: usize, channels: usize, start: f32) -> Vec<f32> {
        (0..n * channels).map(|i| start + (i / channels) as f32 + (i % channels) as f32 * 0.5).collect()
    }

    #[test]
    fn empty_ring_reads_silence() {
        let r = ring(2, 16);
        let mut out = vec![1.0; 8];
        r.read(0, &mut out, 1.0, false);
        assert!(out.iter().all(|&s| s == 0.0));
        assert_eq!(r.written_end(), EMPTY);
    }

    #[test]
    fn write_then_read_round_trips() {
        let r = ring(2, 16);
        let data = ramp(4, 2, 10.0);
        r.write(100, &data, 1.0);
        assert_eq!(r.written_end(), 104);
        let mut out = vec![0.0; 8];
        r.read(100, &mut out, 1.0, false);
        assert_eq!(out, data);
    }

    #[test]
    fn unwritten_and_too_old_frames_are_silent() {
        let r = ring(1, 8);
        r.write(0, &ramp(20, 1, 1.0), 1.0); // keeps sample times 12..20
        let mut out = vec![9.0; 12];
        r.read(10, &mut out, 1.0, false); // 10,11 too old; 20,21 not written
        assert_eq!(out[0..2], [0.0, 0.0]);
        assert_eq!(out[2..10], ramp(8, 1, 13.0)[..]);
        assert_eq!(out[10..12], [0.0, 0.0]);
    }

    #[test]
    fn wraps_around_capacity() {
        let r = ring(2, 8);
        for block in 0..10 {
            r.write(block * 3, &ramp(3, 2, (block * 3) as f32), 1.0);
        }
        let mut out = vec![0.0; 16];
        r.read(22, &mut out, 1.0, false);
        assert_eq!(out, ramp(8, 2, 22.0));
    }

    #[test]
    fn gap_between_writes_reads_as_silence() {
        let r = ring(1, 16);
        r.write(0, &[1.0; 4], 1.0);
        r.write(8, &[2.0; 4], 1.0);
        let mut out = vec![9.0; 12];
        r.read(0, &mut out, 1.0, false);
        assert_eq!(out, [1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0, 2.0, 2.0, 2.0, 2.0]);
    }

    #[test]
    fn gap_clears_stale_data_from_a_previous_lap() {
        let r = ring(1, 8);
        r.write(0, &[5.0; 8], 1.0);
        r.write(12, &[1.0; 2], 1.0); // 8..12 must not show the old 5.0s
        let mut out = vec![9.0; 6];
        r.read(8, &mut out, 1.0, false);
        assert_eq!(out, [0.0, 0.0, 0.0, 0.0, 1.0, 1.0]);
    }

    #[test]
    fn write_mix_adds_inside_valid_window() {
        let r = ring(1, 16);
        r.write(0, &[1.0; 4], 1.0);
        r.write_mix(2, &[0.5; 4], 1.0); // 2,3 overlap; 4,5 are new
        let mut out = vec![0.0; 6];
        r.read(0, &mut out, 1.0, false);
        assert_eq!(out, [1.0, 1.0, 1.5, 1.5, 0.5, 0.5]);
    }

    #[test]
    fn gain_applies_on_write_and_read() {
        let r = ring(1, 8);
        r.write(0, &[1.0, 1.0], 0.5);
        let mut out = vec![0.0; 2];
        r.read(0, &mut out, 0.5, false);
        assert_eq!(out, [0.25, 0.25]);
    }

    #[test]
    fn read_mix_adds_into_destination() {
        let r = ring(1, 8);
        r.write(0, &[1.0; 4], 1.0);
        let mut out = vec![2.0; 4];
        r.read(0, &mut out, 1.0, true);
        assert_eq!(out, [3.0; 4]);
    }

    #[test]
    fn read_into_maps_channels_between_shapes() {
        let r = ring(2, 8);
        r.write(0, &[1.0, 2.0, 3.0, 4.0], 1.0); // two stereo frames
        let mut wide = vec![9.0; 8]; // 4-channel destination
        r.read_into(0, &mut wide, 4, 1.0);
        assert_eq!(wide, [1.0, 2.0, 0.0, 0.0, 3.0, 4.0, 0.0, 0.0]);
        let mut mono = vec![9.0; 2];
        r.read_into(0, &mut mono, 1, 0.5);
        assert_eq!(mono, [0.5, 1.5]);
    }

    #[test]
    fn negative_sample_times_work() {
        let r = ring(1, 8);
        r.write(-6, &ramp(4, 1, -6.0), 1.0);
        let mut out = vec![0.0; 4];
        r.read(-6, &mut out, 1.0, false);
        assert_eq!(out, ramp(4, 1, -6.0));
    }

    #[test]
    fn oversized_write_keeps_the_newest_frames() {
        let r = ring(1, 4);
        r.write(0, &ramp(10, 1, 0.0), 1.0);
        let mut out = vec![0.0; 4];
        r.read(6, &mut out, 1.0, false);
        assert_eq!(out, ramp(4, 1, 6.0));
    }

    #[test]
    fn reset_discards_audio_and_bumps_generation() {
        let r = ring(1, 8);
        r.write(0, &[1.0; 4], 1.0);
        let g = r.generation();
        r.reset(44_100.0);
        assert_eq!(r.written_end(), EMPTY);
        assert_eq!(r.sample_rate(), 44_100.0);
        assert_eq!(r.generation(), g + 1);
        let mut out = vec![9.0; 4];
        r.read(0, &mut out, 1.0, false);
        assert_eq!(out, [0.0; 4]);
    }

    #[test]
    fn attach_sees_the_same_ring() {
        struct Borrowed(*mut u8, usize);
        unsafe impl SharedRegion for Borrowed {
            fn as_ptr(&self) -> *mut u8 { self.0 }
            fn len(&self) -> usize { self.1 }
        }
        let r = ring(2, 8);
        r.write(0, &ramp(2, 2, 1.0), 1.0);
        let peer = Ring::attach(Borrowed(r.region.as_ptr(), r.region.len())).unwrap();
        assert_eq!((peer.channels(), peer.capacity(), peer.sample_rate()), (2, 8, 48_000.0));
        let mut out = vec![0.0; 4];
        peer.read(0, &mut out, 1.0, false);
        assert_eq!(out, ramp(2, 2, 1.0));
    }

    #[test]
    fn rejects_bad_shapes() {
        assert_eq!(Ring::create(HeapRegion::new(region_bytes(1, 6)), 1, 6, 48_000.0).err(), Some(RingError::BadCapacity));
        assert_eq!(Ring::create(HeapRegion::new(region_bytes(1, 8)), 0, 8, 48_000.0).err(), Some(RingError::BadChannels));
        assert!(matches!(
            Ring::create(HeapRegion::new(16), 1, 8, 48_000.0),
            Err(RingError::RegionTooSmall { .. })
        ));
        assert_eq!(Ring::attach(HeapRegion::new(region_bytes(1, 8))).err(), Some(RingError::BadHeader));
    }

    #[test]
    fn concurrent_reader_never_sees_torn_frames() {
        use std::sync::Arc;
        use std::thread;
        // Every frame carries its own sample time; a reader must only ever see
        // either that value or silence.
        let r = Arc::new(ring(2, 64));
        let w = Arc::clone(&r);
        let writer = thread::spawn(move || {
            let mut t = 0i64;
            while t < 200_000 {
                let block: Vec<f32> = (0..16).flat_map(|i| [(t + i) as f32; 2]).collect();
                w.write(t, &block, 1.0);
                t += 16;
            }
        });
        let mut out = vec![0.0f32; 32];
        while !writer.is_finished() {
            let end = r.written_end();
            if end == EMPTY {
                continue;
            }
            let start = end - 48;
            r.read(start, &mut out, 1.0, false);
            for (i, f) in out.chunks(2).enumerate() {
                let expect = (start + i as i64) as f32;
                assert!(f[0] == 0.0 || f[0] == expect, "frame {} = {} expected {}", i, f[0], expect);
                assert_eq!(f[0], f[1]);
            }
        }
        writer.join().unwrap();
    }
}
