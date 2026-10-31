//! Process taps: capture what applications play, as a stereo input stream
//! that joins an aggregate device.
//!
//! Taps follow apps by bundle ID (macOS 26), and Core Audio re-attaches a
//! restarted app by itself, so nothing needs rebuilding when an app quits or
//! starts. Creating a tap needs the user's audio-capture permission.

use super::*;
use std::ffi::{c_char, CString};

#[link(name = "objc")]
extern "C" {
    fn objc_getClass(name: *const c_char) -> *mut c_void;
    fn sel_registerName(name: *const c_char) -> *mut c_void;
    fn objc_msgSend();
}

#[link(name = "CoreAudio", kind = "framework")]
extern "C" {
    fn AudioHardwareCreateProcessTap(description: *mut c_void, out: *mut AudioObjectID) -> OSStatus;
    fn AudioHardwareDestroyProcessTap(id: AudioObjectID) -> OSStatus;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFTypeArrayCallBacks: u8;
    fn CFArrayCreate(alloc: *const c_void, values: *const *const c_void, count: isize, callbacks: *const c_void) -> *const c_void;
    fn CFURLCreateFromFileSystemRepresentation(alloc: *const c_void, path: *const u8, len: isize, is_dir: u8) -> *const c_void;
    fn CFBundleCreate(alloc: *const c_void, url: *const c_void) -> *const c_void;
    fn CFBundleGetIdentifier(bundle: *const c_void) -> *const c_void;
    fn CFRetain(cf: *const c_void) -> *const c_void;
}

/// The bundle identifier of the app (or other bundle) at `path`.
pub fn bundle_identifier(path: &std::path::Path) -> Option<String> {
    use std::os::unix::ffi::OsStrExt;
    let bytes = path.as_os_str().as_bytes();
    let url = aggregate::Cf(unsafe { CFURLCreateFromFileSystemRepresentation(std::ptr::null(), bytes.as_ptr(), bytes.len() as isize, 1) });
    if url.0.is_null() {
        return None;
    }
    let bundle = aggregate::Cf(unsafe { CFBundleCreate(std::ptr::null(), url.0) });
    if bundle.0.is_null() {
        return None;
    }
    let id = unsafe { CFBundleGetIdentifier(bundle.0) };
    // take_cfstring releases what it is given; the identifier is borrowed.
    (!id.is_null()).then(|| take_cfstring(unsafe { CFRetain(id) })).filter(|s| !s.is_empty())
}

const TAP_UID: u32 = u32::from_be_bytes(*b"tuid");
/// `CATapMutedWhenTapped`: the app is silent at its own output only while
/// the tap is being read.
const MUTED_WHEN_TAPPED: isize = 2;
const UNMUTED: isize = 0;

fn sel(name: &str) -> *mut c_void {
    let c = CString::new(name).expect("selector");
    unsafe { sel_registerName(c.as_ptr()) }
}

// objc_msgSend must be called through a pointer of the method's exact type.
unsafe fn send(obj: *mut c_void, name: &str) -> *mut c_void {
    let f: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
    f(obj, sel(name))
}

unsafe fn send_ptr(obj: *mut c_void, name: &str, arg: *const c_void) -> *mut c_void {
    let f: unsafe extern "C" fn(*mut c_void, *mut c_void, *const c_void) -> *mut c_void = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
    f(obj, sel(name), arg)
}

unsafe fn send_isize(obj: *mut c_void, name: &str, arg: isize) {
    let f: unsafe extern "C" fn(*mut c_void, *mut c_void, isize) = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
    f(obj, sel(name), arg)
}

unsafe fn send_bool(obj: *mut c_void, name: &str, arg: bool) {
    let f: unsafe extern "C" fn(*mut c_void, *mut c_void, bool) = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
    f(obj, sel(name), arg)
}

unsafe fn responds(obj: *mut c_void, name: &str) -> bool {
    let f: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> bool = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
    f(obj, sel("respondsToSelector:"), sel(name))
}

/// What to tap.
pub struct TapSpec<'a> {
    pub name: &'a str,
    /// The app's bundle ID plus those of its helper processes.
    pub bundle_ids: &'a [String],
    /// Silence the app at its own output while the tap is read.
    pub mute_original: bool,
}

/// A created tap; dropping it destroys the tap.
pub struct Tap {
    pub id: AudioObjectID,
    pub uid: String,
}

impl Drop for Tap {
    fn drop(&mut self) {
        unsafe { AudioHardwareDestroyProcessTap(self.id) };
    }
}

/// Creates a private stereo tap of the given apps.
pub fn create_tap(spec: &TapSpec) -> Result<Tap> {
    let fail = |what: &str| Error { what: what.into(), status: 0 };
    let class = unsafe { objc_getClass(c"CATapDescription".as_ptr()) };
    if class.is_null() {
        return Err(fail("process taps need macOS 14.2 or later"));
    }
    let ids: Vec<aggregate::Cf> = spec.bundle_ids.iter().map(|b| aggregate::cf_str(b)).collect();
    let ptrs: Vec<*const c_void> = ids.iter().map(|c| c.0).collect();
    let bundle_ids = aggregate::Cf(unsafe { CFArrayCreate(std::ptr::null(), ptrs.as_ptr(), ptrs.len() as isize, &kCFTypeArrayCallBacks as *const u8 as _) });
    let no_processes = aggregate::Cf(unsafe { CFArrayCreate(std::ptr::null(), std::ptr::null(), 0, &kCFTypeArrayCallBacks as *const u8 as _) });
    let name = aggregate::cf_str(spec.name);
    unsafe {
        let desc = send_ptr(send(class, "alloc"), "initStereoMixdownOfProcesses:", no_processes.0);
        if desc.is_null() {
            return Err(fail("cannot create a tap description"));
        }
        if !responds(desc, "setBundleIDs:") || !responds(desc, "setProcessRestoreEnabled:") {
            send(desc, "release");
            return Err(fail("tapping apps by bundle ID needs macOS 26 or later"));
        }
        send_ptr(desc, "setBundleIDs:", bundle_ids.0);
        send_bool(desc, "setProcessRestoreEnabled:", true);
        send_bool(desc, "setPrivate:", true);
        send_isize(desc, "setMuteBehavior:", if spec.mute_original { MUTED_WHEN_TAPPED } else { UNMUTED });
        send_ptr(desc, "setName:", name.0);
        let mut id: AudioObjectID = 0;
        let status = AudioHardwareCreateProcessTap(desc, &mut id);
        send(desc, "release");
        check(status, || format!("creating a tap for {}", spec.name))?;
        match get_string(id, TAP_UID, Scope::Global) {
            Ok(uid) => Ok(Tap { id, uid }),
            Err(e) => {
                AudioHardwareDestroyProcessTap(id);
                Err(e)
            }
        }
    }
}
