//! LoopCraft AudioServerPlugIn.
//!
//! coreaudiod loads this library, calls [`LoopCraftFactory`] and drives the
//! returned driver through the C vtable below. Every entry point catches
//! panics so a bug can never abort coreaudiod.

#![allow(non_upper_case_globals)]

pub use lc_sys::consts;
pub mod driver;
pub mod ffi;
pub use lc_spec as spec;

use consts::*;
use driver::{Driver, Host};
use ffi::*;
use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicPtr, AtomicU32, Ordering};
use std::sync::{mpsc, OnceLock};

static DRIVER: OnceLock<Driver> = OnceLock::new();
static HOST: AtomicPtr<AudioServerPlugInHostInterface> = AtomicPtr::new(std::ptr::null_mut());
static REFS: AtomicU32 = AtomicU32::new(0);

/// The driver reference handed to the HAL: a pointer to a pointer to the vtable.
#[allow(dead_code)] // read by the HAL through the raw pointer
struct DriverRefCell(*const AudioServerPlugInDriverInterface);
unsafe impl Sync for DriverRefCell {}
static DRIVER_REF: DriverRefCell = DriverRefCell(&VTABLE);

fn driver_ref() -> *mut c_void {
    &DRIVER_REF as *const DriverRefCell as *mut c_void
}

fn is_ours(d: *const c_void) -> bool {
    std::ptr::eq(d, driver_ref() as *const c_void)
}

/// Host callbacks that must not run inside a call the host is waiting on.
enum HostCall {
    Changed(AudioObjectID, Vec<AudioObjectPropertyAddress>),
    Reconfigure(AudioObjectID, u64),
    Store(String, String),
}

/// Calls back into the real HAL host.
///
/// Notifications, configuration-change requests and storage writes are queued
/// and delivered in order on a dedicated thread. Making them synchronously from
/// inside SetPropertyData deadlocked coreaudiod: with third-party drivers
/// hosted out of process, coreaudiod is still waiting for the set to finish.
struct HalHost;

impl HalHost {
    fn host() -> Option<&'static AudioServerPlugInHostInterface> {
        let h = HOST.load(Ordering::Acquire);
        (!h.is_null()).then(|| unsafe { &*h })
    }

    fn queue() -> &'static mpsc::Sender<HostCall> {
        static QUEUE: OnceLock<mpsc::Sender<HostCall>> = OnceLock::new();
        QUEUE.get_or_init(|| {
            let (tx, rx) = mpsc::channel::<HostCall>();
            let spawned = std::thread::Builder::new().name("LoopCraft host callbacks".into()).spawn(move || {
                for call in rx {
                    let _ = catch_unwind(AssertUnwindSafe(|| Self::deliver(call)));
                }
            });
            if spawned.is_err() {
                log("could not start the host callback thread");
            }
            tx
        })
    }

    fn deliver(call: HostCall) {
        let Some(h) = Self::host() else { return };
        match call {
            HostCall::Changed(object, addresses) => unsafe {
                (h.PropertiesChanged)(h, object, addresses.len() as u32, addresses.as_ptr());
            },
            HostCall::Reconfigure(device, action) => unsafe {
                (h.RequestDeviceConfigurationChange)(h, device, action, std::ptr::null_mut());
            },
            HostCall::Store(key, value) => {
                let (k, v) = (cfstring(&key), cfstring(&value));
                let st = unsafe { (h.WriteToStorage)(h, k, v) };
                unsafe {
                    CFRelease(k);
                    CFRelease(v);
                }
                if st != 0 {
                    log(&format!("saving the device list failed ({st})"));
                }
            }
        }
    }
}

impl Host for HalHost {
    fn properties_changed(&self, object: AudioObjectID, addresses: &[AudioObjectPropertyAddress]) {
        let _ = Self::queue().send(HostCall::Changed(object, addresses.to_vec()));
    }

    fn request_config_change(&self, device: AudioObjectID, action: u64) {
        let _ = Self::queue().send(HostCall::Reconfigure(device, action));
    }

    fn copy_from_storage(&self, key: &str) -> Option<String> {
        let h = Self::host()?;
        let k = cfstring(key);
        let mut out: CFTypeRef = std::ptr::null();
        let st = unsafe { (h.CopyFromStorage)(h, k, &mut out) };
        unsafe { CFRelease(k) };
        if st != 0 || out.is_null() {
            return None;
        }
        let s = unsafe { string_from_cf(out) };
        unsafe { CFRelease(out) };
        s
    }

    fn write_to_storage(&self, key: &str, value: &str) {
        let _ = Self::queue().send(HostCall::Store(key.into(), value.into()));
    }
}

fn driver() -> &'static Driver {
    DRIVER.get_or_init(|| {
        let mut d = Driver::new(lc_clock::HostTimeBase::system());
        d.set_host(Box::new(HalHost));
        d
    })
}

/// Runs `f`, turning a panic into an error status.
fn guard(f: impl FnOnce() -> OSStatus) -> OSStatus {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(status(kAudioHardwareUnspecifiedError))
}

fn result(r: Result<(), OSStatus>) -> OSStatus {
    r.err().unwrap_or(0)
}

macro_rules! check_driver {
    ($d:expr) => {
        if !is_ours($d as *const c_void) {
            return status(kAudioHardwareBadObjectError);
        }
    };
}

macro_rules! check_device {
    ($device:expr) => {
        if !driver().is_device($device) {
            return status(kAudioHardwareBadObjectError);
        }
    };
}

// ------------------------------------------------------------------ factory

/// CFPlugIn factory named in Info.plist.
///
/// # Safety
/// `requested_type` must be null or a valid `CFUUIDRef`.
#[no_mangle]
pub unsafe extern "C" fn LoopCraftFactory(_allocator: CFAllocatorRef, requested_type: CFUUIDRef) -> *mut c_void {
    let r = catch_unwind(|| {
        if requested_type.is_null() {
            return std::ptr::null_mut();
        }
        let bytes = unsafe { CFUUIDGetUUIDBytes(requested_type) };
        if bytes != AUDIO_SERVER_PLUGIN_TYPE_UUID {
            return std::ptr::null_mut();
        }
        driver();
        REFS.fetch_add(1, Ordering::AcqRel);
        driver_ref()
    });
    r.unwrap_or(std::ptr::null_mut())
}

// ---------------------------------------------------------------- IUnknown

unsafe extern "C" fn query_interface(d: *mut c_void, iid: CFUUIDBytes, out: *mut *mut c_void) -> HRESULT {
    if !is_ours(d) || out.is_null() {
        return E_NOINTERFACE;
    }
    if iid == IUNKNOWN_UUID || iid == AUDIO_SERVER_PLUGIN_DRIVER_INTERFACE_UUID {
        REFS.fetch_add(1, Ordering::AcqRel);
        *out = driver_ref();
        S_OK
    } else {
        *out = std::ptr::null_mut();
        E_NOINTERFACE
    }
}

unsafe extern "C" fn add_ref(d: *mut c_void) -> ULONG {
    if !is_ours(d) {
        return 0;
    }
    REFS.fetch_add(1, Ordering::AcqRel) + 1
}

unsafe extern "C" fn release(d: *mut c_void) -> ULONG {
    if !is_ours(d) {
        return 0;
    }
    // The driver is a process-wide singleton; the count is informational.
    REFS.try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1)).map(|n| n - 1).unwrap_or(0)
}

// --------------------------------------------------------- basic operations

unsafe extern "C" fn initialize(d: DriverRef, host: HostRef) -> OSStatus {
    check_driver!(d);
    guard(|| {
        HOST.store(host as *mut _, Ordering::Release);
        driver().initialize();
        log(&format!("initialised (version {}, {} device(s))", env!("CARGO_PKG_VERSION"), driver().specs().len()));
        0
    })
}

unsafe extern "C" fn create_device(d: DriverRef, _desc: CFDictionaryRef, _client: *const AudioServerPlugInClientInfo, _out: *mut AudioObjectID) -> OSStatus {
    check_driver!(d);
    status(kAudioHardwareUnsupportedOperationError)
}

unsafe extern "C" fn destroy_device(d: DriverRef, _device: AudioObjectID) -> OSStatus {
    check_driver!(d);
    status(kAudioHardwareUnsupportedOperationError)
}

unsafe extern "C" fn add_device_client(d: DriverRef, device: AudioObjectID, client: *const AudioServerPlugInClientInfo) -> OSStatus {
    check_driver!(d);
    guard(|| {
        check_device!(device);
        if let Some(c) = client.as_ref() {
            driver().add_client(device, c.mClientID, string_from_cf(c.mBundleID));
        }
        0
    })
}

unsafe extern "C" fn remove_device_client(d: DriverRef, device: AudioObjectID, client: *const AudioServerPlugInClientInfo) -> OSStatus {
    check_driver!(d);
    guard(|| {
        check_device!(device);
        if let Some(c) = client.as_ref() {
            driver().remove_client(device, c.mClientID);
        }
        0
    })
}

unsafe extern "C" fn perform_config_change(d: DriverRef, device: AudioObjectID, _action: u64, _info: *mut c_void) -> OSStatus {
    check_driver!(d);
    guard(|| result(driver().perform_config_change(device)))
}

unsafe extern "C" fn abort_config_change(d: DriverRef, device: AudioObjectID, _action: u64, _info: *mut c_void) -> OSStatus {
    check_driver!(d);
    guard(|| {
        check_device!(device);
        0
    })
}

// ------------------------------------------------------------- properties

unsafe fn slice<'a>(p: *const c_void, len: u32) -> &'a [u8] {
    if p.is_null() || len == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(p as *const u8, len as usize)
    }
}

unsafe extern "C" fn has_property(d: DriverRef, object: AudioObjectID, _pid: pid_t, a: *const AudioObjectPropertyAddress) -> Boolean {
    if !is_ours(d as *const c_void) || a.is_null() {
        return 0;
    }
    catch_unwind(AssertUnwindSafe(|| driver().has(object, &*a) as Boolean)).unwrap_or(0)
}

unsafe extern "C" fn is_property_settable(d: DriverRef, object: AudioObjectID, _pid: pid_t, a: *const AudioObjectPropertyAddress, out: *mut Boolean) -> OSStatus {
    check_driver!(d);
    if a.is_null() || out.is_null() {
        return status(kAudioHardwareIllegalOperationError);
    }
    guard(|| match driver().is_settable(object, &*a) {
        Ok(s) => {
            *out = s as Boolean;
            0
        }
        Err(e) => e,
    })
}

unsafe extern "C" fn get_property_data_size(d: DriverRef, object: AudioObjectID, _pid: pid_t, a: *const AudioObjectPropertyAddress, qsize: u32, qdata: *const c_void, out: *mut u32) -> OSStatus {
    check_driver!(d);
    if a.is_null() || out.is_null() {
        return status(kAudioHardwareIllegalOperationError);
    }
    guard(|| match driver().get(object, &*a, slice(qdata, qsize), &[0u8; 4]) {
        Ok(v) => {
            *out = v.size() as u32;
            0
        }
        Err(e) => e,
    })
}

unsafe extern "C" fn get_property_data(d: DriverRef, object: AudioObjectID, _pid: pid_t, a: *const AudioObjectPropertyAddress, qsize: u32, qdata: *const c_void, size: u32, out_size: *mut u32, out: *mut c_void) -> OSStatus {
    check_driver!(d);
    if a.is_null() || out_size.is_null() || (out.is_null() && size > 0) {
        return status(kAudioHardwareIllegalOperationError);
    }
    guard(|| {
        // "Convert" properties read their argument from the output buffer.
        let input: Vec<u8> = slice(out, size).to_vec();
        match driver().get(object, &*a, slice(qdata, qsize), &input) {
            Ok(v) => {
                let buf = if size == 0 { &mut [][..] } else { std::slice::from_raw_parts_mut(out as *mut u8, size as usize) };
                match v.write(buf) {
                    Ok(n) => {
                        *out_size = n as u32;
                        0
                    }
                    Err(e) => e,
                }
            }
            Err(e) => e,
        }
    })
}

unsafe extern "C" fn set_property_data(d: DriverRef, object: AudioObjectID, _pid: pid_t, a: *const AudioObjectPropertyAddress, _qsize: u32, _qdata: *const c_void, size: u32, data: *const c_void) -> OSStatus {
    check_driver!(d);
    if a.is_null() {
        return status(kAudioHardwareIllegalOperationError);
    }
    guard(|| result(driver().set(object, &*a, slice(data, size))))
}

// --------------------------------------------------------------------- IO

unsafe extern "C" fn start_io(d: DriverRef, device: AudioObjectID, _client: u32) -> OSStatus {
    check_driver!(d);
    guard(|| result(driver().start_io(device, lc_clock::host_now())))
}

unsafe extern "C" fn stop_io(d: DriverRef, device: AudioObjectID, _client: u32) -> OSStatus {
    check_driver!(d);
    guard(|| result(driver().stop_io(device)))
}

unsafe extern "C" fn get_zero_timestamp(d: DriverRef, device: AudioObjectID, _client: u32, sample: *mut f64, host: *mut u64, seed: *mut u64) -> OSStatus {
    check_driver!(d);
    if sample.is_null() || host.is_null() || seed.is_null() {
        return status(kAudioHardwareIllegalOperationError);
    }
    guard(|| match driver().zero_timestamp(device, lc_clock::host_now()) {
        Ok(z) => {
            *sample = z.sample_time;
            *host = z.host_time;
            *seed = z.seed;
            0
        }
        Err(e) => e,
    })
}

unsafe extern "C" fn will_do_io_operation(d: DriverRef, device: AudioObjectID, _client: u32, op: u32, will_do: *mut Boolean, in_place: *mut Boolean) -> OSStatus {
    check_driver!(d);
    let (w, p) = Driver::will_do(op);
    if !will_do.is_null() {
        *will_do = w as Boolean;
    }
    if !in_place.is_null() {
        *in_place = p as Boolean;
    }
    let _ = device;
    0
}

unsafe extern "C" fn begin_io_operation(d: DriverRef, _device: AudioObjectID, _client: u32, _op: u32, _frames: u32, _cycle: *const AudioServerPlugInIOCycleInfo) -> OSStatus {
    check_driver!(d);
    0
}

unsafe extern "C" fn do_io_operation(d: DriverRef, device: AudioObjectID, stream: AudioObjectID, client: u32, op: u32, frames: u32, cycle: *const AudioServerPlugInIOCycleInfo, main: *mut c_void, _secondary: *mut c_void) -> OSStatus {
    check_driver!(d);
    if cycle.is_null() || main.is_null() {
        return 0;
    }
    guard(|| driver().do_io(device, stream, client, op, frames, &*cycle, main as *mut f32))
}

unsafe extern "C" fn end_io_operation(d: DriverRef, _device: AudioObjectID, _client: u32, _op: u32, _frames: u32, _cycle: *const AudioServerPlugInIOCycleInfo) -> OSStatus {
    check_driver!(d);
    0
}

static VTABLE: AudioServerPlugInDriverInterface = AudioServerPlugInDriverInterface {
    _reserved: std::ptr::null_mut(),
    QueryInterface: query_interface,
    AddRef: add_ref,
    Release: release,
    Initialize: initialize,
    CreateDevice: create_device,
    DestroyDevice: destroy_device,
    AddDeviceClient: add_device_client,
    RemoveDeviceClient: remove_device_client,
    PerformDeviceConfigurationChange: perform_config_change,
    AbortDeviceConfigurationChange: abort_config_change,
    HasProperty: has_property,
    IsPropertySettable: is_property_settable,
    GetPropertyDataSize: get_property_data_size,
    GetPropertyData: get_property_data,
    SetPropertyData: set_property_data,
    StartIO: start_io,
    StopIO: stop_io,
    GetZeroTimeStamp: get_zero_timestamp,
    WillDoIOOperation: will_do_io_operation,
    BeginIOOperation: begin_io_operation,
    DoIOOperation: do_io_operation,
    EndIOOperation: end_io_operation,
};

#[cfg(test)]
mod tests {
    use super::*;

    extern "C" {
        fn CFUUIDGetConstantUUIDWithBytes(alloc: CFAllocatorRef, b0: u8, b1: u8, b2: u8, b3: u8, b4: u8, b5: u8, b6: u8, b7: u8, b8: u8, b9: u8, b10: u8, b11: u8, b12: u8, b13: u8, b14: u8, b15: u8) -> CFUUIDRef;
    }

    fn uuid(b: CFUUIDBytes) -> CFUUIDRef {
        let b = b.0;
        unsafe { CFUUIDGetConstantUUIDWithBytes(std::ptr::null(), b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]) }
    }

    /// Drives the plug-in through the C ABI without a host (no Initialize).
    #[test]
    fn factory_and_vtable_round_trip() {
        assert!(unsafe { LoopCraftFactory(std::ptr::null(), uuid(IUNKNOWN_UUID)) }.is_null(), "wrong type is refused");
        let d = unsafe { LoopCraftFactory(std::ptr::null(), uuid(AUDIO_SERVER_PLUGIN_TYPE_UUID)) };
        assert!(!d.is_null());
        driver().initialize();
        unsafe {
            let vt = &**(d as *const *const AudioServerPlugInDriverInterface);
            let mut iface: *mut c_void = std::ptr::null_mut();
            assert_eq!((vt.QueryInterface)(d, AUDIO_SERVER_PLUGIN_DRIVER_INTERFACE_UUID, &mut iface), S_OK);
            assert_eq!(iface, d);
            assert_eq!((vt.QueryInterface)(d, AUDIO_SERVER_PLUGIN_TYPE_UUID, &mut iface), E_NOINTERFACE);

            let dr = d as DriverRef;
            let a = AudioObjectPropertyAddress { mSelector: kAudioPlugInPropertyDeviceList, mScope: kAudioObjectPropertyScopeGlobal, mElement: 0 };
            assert_eq!((vt.HasProperty)(dr, kAudioObjectPlugInObject, 0, &a), 1);
            let mut size = 0u32;
            assert_eq!((vt.GetPropertyDataSize)(dr, kAudioObjectPlugInObject, 0, &a, 0, std::ptr::null(), &mut size), 0);
            assert_eq!(size, 8, "device + engine twin");
            let mut ids = [0u32; 2];
            let mut written = 0u32;
            assert_eq!((vt.GetPropertyData)(dr, kAudioObjectPlugInObject, 0, &a, 0, std::ptr::null(), 8, &mut written, ids.as_mut_ptr() as *mut c_void), 0);
            let dev = ids[0];

            let rate = AudioObjectPropertyAddress { mSelector: kAudioDevicePropertyNominalSampleRate, mScope: kAudioObjectPropertyScopeGlobal, mElement: 0 };
            let mut settable = 0u8;
            assert_eq!((vt.IsPropertySettable)(dr, dev, 0, &rate, &mut settable), 0);
            assert_eq!(settable, 1);

            assert_eq!((vt.StartIO)(dr, dev, 1), 0);
            let (mut s, mut h, mut seed) = (0f64, 0u64, 0u64);
            assert_eq!((vt.GetZeroTimeStamp)(dr, dev, 1, &mut s, &mut h, &mut seed), 0);
            assert!(seed > 0);
            let (mut w, mut p) = (0u8, 0u8);
            assert_eq!((vt.WillDoIOOperation)(dr, dev, 1, kAudioServerPlugInIOOperationWriteMix, &mut w, &mut p), 0);
            assert_eq!((w, p), (1, 1));
            assert_eq!((vt.StopIO)(dr, dev, 1), 0);
            assert_eq!((vt.StartIO)(dr, 9999, 1), status(kAudioHardwareBadObjectError));

            let bogus = 0x1234usize as DriverRef;
            assert_eq!((vt.StartIO)(bogus, dev, 1), status(kAudioHardwareBadObjectError));
            assert!((vt.Release)(d) < u32::MAX);
        }
    }
}
