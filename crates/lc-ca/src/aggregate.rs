//! Private aggregate devices: several devices on one IO cycle, with Core
//! Audio's drift compensation for the members that don't own the clock.
//!
//! A private aggregate is visible only to the process that created it, and
//! coreaudiod destroys it when that process exits.

use super::*;

#[link(name = "CoreAudio", kind = "framework")]
extern "C" {
    fn AudioHardwareCreateAggregateDevice(description: *const c_void, out: *mut AudioObjectID) -> OSStatus;
    fn AudioHardwareDestroyAggregateDevice(id: AudioObjectID) -> OSStatus;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    // Only their addresses are used.
    static kCFTypeDictionaryKeyCallBacks: u8;
    static kCFTypeDictionaryValueCallBacks: u8;
    static kCFTypeArrayCallBacks: u8;
    fn CFDictionaryCreateMutable(alloc: *const c_void, capacity: isize, keys: *const c_void, values: *const c_void) -> *const c_void;
    fn CFDictionarySetValue(dict: *const c_void, key: *const c_void, value: *const c_void);
    fn CFArrayCreateMutable(alloc: *const c_void, capacity: isize, callbacks: *const c_void) -> *const c_void;
    fn CFArrayAppendValue(array: *const c_void, value: *const c_void);
    fn CFNumberCreate(alloc: *const c_void, kind: isize, value: *const c_void) -> *const c_void;
}

const CF_NUMBER_SINT32: isize = 3;
const RUN_LOOP: u32 = u32::from_be_bytes(*b"rnlp");

/// An owned CoreFoundation object.
pub(crate) struct Cf(pub(crate) *const c_void);

impl Drop for Cf {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) };
        }
    }
}

pub(crate) fn cf_str(s: &str) -> Cf {
    Cf(unsafe { CFStringCreateWithBytes(std::ptr::null(), s.as_ptr(), s.len() as isize, UTF8, 0) })
}

fn cf_int(v: i32) -> Cf {
    Cf(unsafe { CFNumberCreate(std::ptr::null(), CF_NUMBER_SINT32, &v as *const i32 as *const c_void) })
}

fn cf_dict() -> Cf {
    Cf(unsafe { CFDictionaryCreateMutable(std::ptr::null(), 0, &kCFTypeDictionaryKeyCallBacks as *const u8 as _, &kCFTypeDictionaryValueCallBacks as *const u8 as _) })
}

fn set(dict: &Cf, key: &str, value: &Cf) {
    let k = cf_str(key);
    unsafe { CFDictionarySetValue(dict.0, k.0, value.0) };
}

/// A member of an aggregate.
pub struct SubDevice<'a> {
    pub uid: &'a str,
    /// Resample this member to follow the aggregate's clock.
    pub drift_compensation: bool,
}

/// Lets the HAL deliver notifications on its own thread. Processes without a
/// run loop (like the daemon) call this once at start.
pub fn use_hal_notification_thread() -> Result<()> {
    set_raw::<usize>(kAudioObjectSystemObject, RUN_LOOP, Scope::Global, 0)
}

/// Creates a private, unstacked aggregate whose clock comes from `main`.
/// Taps join after the devices, each as one input stream.
pub fn create_aggregate(name: &str, uid: &str, main: &str, members: &[SubDevice], taps: &[SubDevice]) -> Result<AudioObjectID> {
    let desc = cf_dict();
    set(&desc, "name", &cf_str(name));
    set(&desc, "uid", &cf_str(uid));
    set(&desc, "private", &cf_int(1));
    set(&desc, "stacked", &cf_int(0));
    set(&desc, "master", &cf_str(main));
    let list = Cf(unsafe { CFArrayCreateMutable(std::ptr::null(), 0, &kCFTypeArrayCallBacks as *const u8 as _) });
    for m in members {
        let sub = cf_dict();
        set(&sub, "uid", &cf_str(m.uid));
        set(&sub, "drift", &cf_int(m.drift_compensation as i32));
        unsafe { CFArrayAppendValue(list.0, sub.0) };
    }
    set(&desc, "subdevices", &list);
    if !taps.is_empty() {
        let tap_list = Cf(unsafe { CFArrayCreateMutable(std::ptr::null(), 0, &kCFTypeArrayCallBacks as *const u8 as _) });
        for t in taps {
            let tap = cf_dict();
            set(&tap, "uid", &cf_str(t.uid));
            set(&tap, "drift", &cf_int(t.drift_compensation as i32));
            unsafe { CFArrayAppendValue(tap_list.0, tap.0) };
        }
        set(&desc, "taps", &tap_list);
    }
    let mut id: AudioObjectID = 0;
    check(unsafe { AudioHardwareCreateAggregateDevice(desc.0, &mut id) }, || format!("creating aggregate device {uid}"))?;
    Ok(id)
}

pub fn destroy_aggregate(id: AudioObjectID) -> Result<()> {
    check(unsafe { AudioHardwareDestroyAggregateDevice(id) }, || format!("destroying aggregate device {id}"))
}
