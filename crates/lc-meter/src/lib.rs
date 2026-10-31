//! Live levels, from the daemon's IO thread to the editor, through a small
//! memory-mapped file per device (no sockets: LoopCraft never networks).
//!
//! The file is an array of little-endian u32 words:
//!
//! | word | what |
//! |---|---|
//! | 0 | magic `LCm1` |
//! | 1 | layout generation, bumped when the counts change |
//! | 2, 3, 4 | number of inputs, number of outputs, busses |
//! | 5 | transitions asked for by the editor (a preset switch): the daemon fades all outputs out, applies the change, fades back in |
//! | `COUNTS..` | channels of each input, then of each output |
//! | `VALUES..` | peak per channel (f32 bits): inputs, busses, outputs |
//!
//! The IO thread raises each peak with `fetch_max` (non-negative floats
//! order like their bits); the reader takes it and leaves zero, so it sees
//! the loudest sample since its last look.

use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

const MAGIC: u32 = u32::from_le_bytes(*b"LCm1");
const COUNTS: usize = 8;
pub const MAX_UNITS: usize = 128;
const VALUES: usize = COUNTS + 2 * MAX_UNITS;
pub const MAX_VALUES: usize = 2048;
const WORDS: usize = VALUES + MAX_VALUES;

/// Where a device's levels are published.
pub fn path(device_id: &str) -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"));
    let safe: String = device_id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    home.join("Library/Application Support/LoopCraft/meters").join(format!("{safe}.levels"))
}

extern "C" {
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, offset: i64) -> *mut u8;
    fn munmap(addr: *mut u8, len: usize) -> i32;
}
const PROT_READ: i32 = 1;
const PROT_WRITE: i32 = 2;
const MAP_SHARED: i32 = 1;
const MAP_FAILED: *mut u8 = !0usize as *mut u8;

/// A mapped levels file.
pub struct Levels {
    words: *const AtomicU32,
    _file: File,
}

// The mapping is only accessed through atomics.
unsafe impl Send for Levels {}
unsafe impl Sync for Levels {}

/// What the editor needs to know about the layout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shape {
    pub inputs: Vec<u32>,
    pub busses: u32,
    pub outputs: Vec<u32>,
}

impl Shape {
    pub fn values(&self) -> usize {
        (self.inputs.iter().sum::<u32>() + self.busses + self.outputs.iter().sum::<u32>()) as usize
    }
}

impl Levels {
    fn map(path: &Path, create: bool) -> std::io::Result<Levels> {
        if create {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
        }
        let file = OpenOptions::new().read(true).write(true).create(create).truncate(false).open(path)?;
        let len = (WORDS * 4) as u64;
        if file.metadata()?.len() < len {
            if !create {
                return Err(std::io::Error::other("levels file too short"));
            }
            file.set_len(len)?;
        }
        let p = unsafe { mmap(std::ptr::null_mut(), WORDS * 4, PROT_READ | PROT_WRITE, MAP_SHARED, file.as_raw_fd(), 0) };
        if p == MAP_FAILED || p.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        let levels = Levels { words: p as *const AtomicU32, _file: file };
        if create {
            levels.word(0).store(MAGIC, Ordering::Release);
        } else if levels.word(0).load(Ordering::Acquire) != MAGIC {
            return Err(std::io::Error::other("not a levels file"));
        }
        Ok(levels)
    }

    /// Creates (or reuses) the file, for the daemon.
    pub fn create(path: &Path) -> std::io::Result<Levels> {
        Levels::map(path, true)
    }

    /// Opens an existing file, for the editor.
    pub fn open(path: &Path) -> std::io::Result<Levels> {
        Levels::map(path, false)
    }

    fn word(&self, i: usize) -> &AtomicU32 {
        assert!(i < WORDS);
        unsafe { &*self.words.add(i) }
    }

    /// Publishes a new layout (control thread). Units beyond the limits are
    /// left out.
    pub fn set_shape(&self, shape: &Shape) {
        let inputs = shape.inputs.len().min(MAX_UNITS);
        let outputs = shape.outputs.len().min(MAX_UNITS);
        if self.shape().as_ref() == Some(shape) {
            return;
        }
        for (k, &n) in shape.inputs.iter().take(inputs).chain(shape.outputs.iter().take(outputs)).enumerate() {
            self.word(COUNTS + k).store(n, Ordering::Relaxed);
        }
        self.word(2).store(inputs as u32, Ordering::Relaxed);
        self.word(3).store(outputs as u32, Ordering::Relaxed);
        self.word(4).store(shape.busses, Ordering::Relaxed);
        for i in 0..MAX_VALUES {
            self.word(VALUES + i).store(0, Ordering::Relaxed);
        }
        self.word(1).fetch_add(1, Ordering::Release);
    }

    pub fn shape(&self) -> Option<Shape> {
        let (inputs, outputs) = (self.word(2).load(Ordering::Acquire) as usize, self.word(3).load(Ordering::Acquire) as usize);
        if inputs > MAX_UNITS || outputs > MAX_UNITS {
            return None;
        }
        let count = |k: usize| self.word(COUNTS + k).load(Ordering::Relaxed);
        Some(Shape {
            inputs: (0..inputs).map(count).collect(),
            busses: self.word(4).load(Ordering::Relaxed),
            outputs: (inputs..inputs + outputs).map(count).collect(),
        })
    }

    /// Asks the daemon to fade out and back in around the next change
    /// (editor, before saving a preset switch).
    pub fn request_transition(&self) {
        self.word(5).fetch_add(1, Ordering::Release);
    }

    /// How many transitions have been asked for (daemon).
    pub fn transitions(&self) -> u32 {
        self.word(5).load(Ordering::Acquire)
    }

    pub fn generation(&self) -> u32 {
        self.word(1).load(Ordering::Acquire)
    }

    /// Raises value `i` to `peak` (IO thread: one atomic, no syscall).
    #[inline]
    pub fn raise(&self, i: usize, peak: f32) {
        if i < MAX_VALUES {
            self.word(VALUES + i).fetch_max(peak.abs().to_bits(), Ordering::Relaxed);
        }
    }

    /// Takes value `i`, leaving zero (reader).
    pub fn take(&self, i: usize) -> f32 {
        if i < MAX_VALUES {
            f32::from_bits(self.word(VALUES + i).swap(0, Ordering::Relaxed))
        } else {
            0.0
        }
    }
}

impl Drop for Levels {
    fn drop(&mut self) {
        unsafe { munmap(self.words as *mut u8, WORDS * 4) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_and_editor_share_levels() {
        let dir = std::env::temp_dir().join(format!("lc-meter-{}", std::process::id()));
        let path = dir.join("m.levels");
        let daemon = Levels::create(&path).unwrap();
        let shape = Shape { inputs: vec![2, 0, 14], busses: 10, outputs: vec![2, 8] };
        daemon.set_shape(&shape);
        let g = daemon.generation();
        daemon.set_shape(&shape);
        assert_eq!(daemon.generation(), g, "same shape, same generation");

        let editor = Levels::open(&path).unwrap();
        assert_eq!(editor.shape(), Some(shape.clone()));
        assert_eq!(shape.values(), 36);
        daemon.raise(3, 0.25);
        daemon.raise(3, -0.5);
        daemon.raise(3, 0.1);
        assert_eq!(editor.take(3), 0.5, "the loudest since the last look");
        assert_eq!(editor.take(3), 0.0);
        assert!(Levels::open(&dir.join("missing")).is_err());
        let t = daemon.transitions();
        editor.request_transition();
        assert_eq!(daemon.transitions(), t + 1);
        daemon.set_shape(&Shape { inputs: vec![1], busses: 2, outputs: vec![] });
        assert_eq!(daemon.transitions(), t + 1, "a new layout keeps it");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
