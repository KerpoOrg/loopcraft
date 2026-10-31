//! Hand-written mirrors of the AudioServerPlugIn C ABI.
//!
//! Layouts are checked at compile time against sizes generated from the SDK
//! headers (`consts.rs`, produced by `tools/gen_consts.c`).

#![allow(non_snake_case, non_camel_case_types, dead_code)]

use crate::consts::*;
use std::ffi::c_void;

pub type OSStatus = i32;
pub type HRESULT = i32;
pub type ULONG = u32;
pub type AudioObjectID = u32;
pub type Boolean = u8;
pub type pid_t = i32;
pub type CFTypeRef = *const c_void;
pub type CFStringRef = *const c_void;
pub type CFAllocatorRef = *const c_void;
pub type CFUUIDRef = *const c_void;
pub type CFDictionaryRef = *const c_void;
pub type CFIndex = isize;

pub const S_OK: HRESULT = 0;
pub const E_NOINTERFACE: HRESULT = 0x8000_0004_u32 as i32;

/// Status codes as the signed values the ABI returns.
pub const fn status(code: u32) -> OSStatus {
    code as i32
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CFUUIDBytes(pub [u8; 16]);

pub const IUNKNOWN_UUID: CFUUIDBytes =
    CFUUIDBytes([0, 0, 0, 0, 0, 0, 0, 0, 0xC0, 0, 0, 0, 0, 0, 0, 0x46]);
pub const AUDIO_SERVER_PLUGIN_TYPE_UUID: CFUUIDBytes = CFUUIDBytes([
    0x44, 0x3A, 0xBA, 0xB8, 0xE7, 0xB3, 0x49, 0x1A, 0xB9, 0x85, 0xBE, 0xB9, 0x18, 0x70, 0x30, 0xDB,
]);
pub const AUDIO_SERVER_PLUGIN_DRIVER_INTERFACE_UUID: CFUUIDBytes = CFUUIDBytes([
    0xEE, 0xA5, 0x77, 0x3D, 0xCC, 0x43, 0x49, 0xF1, 0x8E, 0x00, 0x8F, 0x96, 0xE7, 0xD2, 0x3B, 0x17,
]);

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioObjectPropertyAddress {
    pub mSelector: u32,
    pub mScope: u32,
    pub mElement: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AudioStreamBasicDescription {
    pub mSampleRate: f64,
    pub mFormatID: u32,
    pub mFormatFlags: u32,
    pub mBytesPerPacket: u32,
    pub mFramesPerPacket: u32,
    pub mBytesPerFrame: u32,
    pub mChannelsPerFrame: u32,
    pub mBitsPerChannel: u32,
    pub mReserved: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AudioValueRange {
    pub mMinimum: f64,
    pub mMaximum: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AudioStreamRangedDescription {
    pub mFormat: AudioStreamBasicDescription,
    pub mSampleRateRange: AudioValueRange,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct SMPTETime {
    pub mSubframes: i16,
    pub mSubframeDivisor: i16,
    pub mCounter: u32,
    pub mType: u32,
    pub mFlags: u32,
    pub mHours: i16,
    pub mMinutes: i16,
    pub mSeconds: i16,
    pub mFrames: i16,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct AudioTimeStamp {
    pub mSampleTime: f64,
    pub mHostTime: u64,
    pub mRateScalar: f64,
    pub mWordClockTime: u64,
    pub mSMPTETime: SMPTETime,
    pub mFlags: u32,
    pub mReserved: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct AudioServerPlugInIOCycleInfo {
    pub mIOCycleCounter: u64,
    pub mNominalIOBufferFrameSize: u32,
    pub mCurrentTime: AudioTimeStamp,
    pub mInputTime: AudioTimeStamp,
    pub mOutputTime: AudioTimeStamp,
    pub mMainHostTicksPerFrame: f64,
    pub mDeviceHostTicksPerFrame: f64,
}

#[repr(C)]
pub struct AudioServerPlugInClientInfo {
    pub mClientID: u32,
    pub mProcessID: pid_t,
    pub mIsNativeEndian: Boolean,
    pub mBundleID: CFStringRef,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AudioChannelDescription {
    pub mChannelLabel: u32,
    pub mChannelFlags: u32,
    pub mCoordinates: [f32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AudioChannelLayoutStereo {
    pub mChannelLayoutTag: u32,
    pub mChannelBitmap: u32,
    pub mNumberChannelDescriptions: u32,
    pub mChannelDescriptions: [AudioChannelDescription; 2],
}

pub type HostRef = *const AudioServerPlugInHostInterface;
pub type DriverRef = *mut *const AudioServerPlugInDriverInterface;

#[repr(C)]
pub struct AudioServerPlugInHostInterface {
    pub PropertiesChanged: unsafe extern "C" fn(HostRef, AudioObjectID, u32, *const AudioObjectPropertyAddress) -> OSStatus,
    pub CopyFromStorage: unsafe extern "C" fn(HostRef, CFStringRef, *mut CFTypeRef) -> OSStatus,
    pub WriteToStorage: unsafe extern "C" fn(HostRef, CFStringRef, CFTypeRef) -> OSStatus,
    pub DeleteFromStorage: unsafe extern "C" fn(HostRef, CFStringRef) -> OSStatus,
    pub RequestDeviceConfigurationChange: unsafe extern "C" fn(HostRef, AudioObjectID, u64, *mut c_void) -> OSStatus,
}

#[repr(C)]
pub struct AudioServerPlugInDriverInterface {
    pub _reserved: *mut c_void,
    pub QueryInterface: unsafe extern "C" fn(*mut c_void, CFUUIDBytes, *mut *mut c_void) -> HRESULT,
    pub AddRef: unsafe extern "C" fn(*mut c_void) -> ULONG,
    pub Release: unsafe extern "C" fn(*mut c_void) -> ULONG,
    pub Initialize: unsafe extern "C" fn(DriverRef, HostRef) -> OSStatus,
    pub CreateDevice: unsafe extern "C" fn(DriverRef, CFDictionaryRef, *const AudioServerPlugInClientInfo, *mut AudioObjectID) -> OSStatus,
    pub DestroyDevice: unsafe extern "C" fn(DriverRef, AudioObjectID) -> OSStatus,
    pub AddDeviceClient: unsafe extern "C" fn(DriverRef, AudioObjectID, *const AudioServerPlugInClientInfo) -> OSStatus,
    pub RemoveDeviceClient: unsafe extern "C" fn(DriverRef, AudioObjectID, *const AudioServerPlugInClientInfo) -> OSStatus,
    pub PerformDeviceConfigurationChange: unsafe extern "C" fn(DriverRef, AudioObjectID, u64, *mut c_void) -> OSStatus,
    pub AbortDeviceConfigurationChange: unsafe extern "C" fn(DriverRef, AudioObjectID, u64, *mut c_void) -> OSStatus,
    pub HasProperty: unsafe extern "C" fn(DriverRef, AudioObjectID, pid_t, *const AudioObjectPropertyAddress) -> Boolean,
    pub IsPropertySettable: unsafe extern "C" fn(DriverRef, AudioObjectID, pid_t, *const AudioObjectPropertyAddress, *mut Boolean) -> OSStatus,
    pub GetPropertyDataSize: unsafe extern "C" fn(DriverRef, AudioObjectID, pid_t, *const AudioObjectPropertyAddress, u32, *const c_void, *mut u32) -> OSStatus,
    pub GetPropertyData: unsafe extern "C" fn(DriverRef, AudioObjectID, pid_t, *const AudioObjectPropertyAddress, u32, *const c_void, u32, *mut u32, *mut c_void) -> OSStatus,
    pub SetPropertyData: unsafe extern "C" fn(DriverRef, AudioObjectID, pid_t, *const AudioObjectPropertyAddress, u32, *const c_void, u32, *const c_void) -> OSStatus,
    pub StartIO: unsafe extern "C" fn(DriverRef, AudioObjectID, u32) -> OSStatus,
    pub StopIO: unsafe extern "C" fn(DriverRef, AudioObjectID, u32) -> OSStatus,
    pub GetZeroTimeStamp: unsafe extern "C" fn(DriverRef, AudioObjectID, u32, *mut f64, *mut u64, *mut u64) -> OSStatus,
    pub WillDoIOOperation: unsafe extern "C" fn(DriverRef, AudioObjectID, u32, u32, *mut Boolean, *mut Boolean) -> OSStatus,
    pub BeginIOOperation: unsafe extern "C" fn(DriverRef, AudioObjectID, u32, u32, u32, *const AudioServerPlugInIOCycleInfo) -> OSStatus,
    pub DoIOOperation: unsafe extern "C" fn(DriverRef, AudioObjectID, AudioObjectID, u32, u32, u32, *const AudioServerPlugInIOCycleInfo, *mut c_void, *mut c_void) -> OSStatus,
    pub EndIOOperation: unsafe extern "C" fn(DriverRef, AudioObjectID, u32, u32, u32, *const AudioServerPlugInIOCycleInfo) -> OSStatus,
}

// The vtable is immutable after construction.
unsafe impl Sync for AudioServerPlugInDriverInterface {}

// Layout checks against the SDK.
const _: () = {
    use std::mem::{offset_of, size_of};
    assert!(size_of::<AudioObjectPropertyAddress>() == SIZEOF_AudioObjectPropertyAddress);
    assert!(size_of::<AudioStreamBasicDescription>() == SIZEOF_AudioStreamBasicDescription);
    assert!(size_of::<AudioValueRange>() == SIZEOF_AudioValueRange);
    assert!(size_of::<AudioStreamRangedDescription>() == SIZEOF_AudioStreamRangedDescription);
    assert!(size_of::<AudioTimeStamp>() == SIZEOF_AudioTimeStamp);
    assert!(size_of::<AudioServerPlugInIOCycleInfo>() == SIZEOF_AudioServerPlugInIOCycleInfo);
    assert!(offset_of!(AudioServerPlugInIOCycleInfo, mInputTime) == OFFSETOF_AudioServerPlugInIOCycleInfo_mInputTime);
    assert!(offset_of!(AudioServerPlugInIOCycleInfo, mOutputTime) == OFFSETOF_AudioServerPlugInIOCycleInfo_mOutputTime);
    assert!(offset_of!(AudioServerPlugInIOCycleInfo, mMainHostTicksPerFrame) == OFFSETOF_AudioServerPlugInIOCycleInfo_mMainHostTicksPerFrame);
    assert!(size_of::<AudioServerPlugInClientInfo>() == SIZEOF_AudioServerPlugInClientInfo);
    assert!(offset_of!(AudioServerPlugInClientInfo, mBundleID) == OFFSETOF_AudioServerPlugInClientInfo_mBundleID);
    assert!(size_of::<AudioServerPlugInDriverInterface>() == SIZEOF_AudioServerPlugInDriverInterface);
    assert!(offset_of!(AudioServerPlugInDriverInterface, EndIOOperation) == OFFSETOF_AudioServerPlugInDriverInterface_EndIOOperation);
    assert!(size_of::<AudioServerPlugInHostInterface>() == SIZEOF_AudioServerPlugInHostInterface);
    assert!(offset_of!(AudioServerPlugInHostInterface, RequestDeviceConfigurationChange) == OFFSETOF_AudioServerPlugInHostInterface_RequestDeviceConfigurationChange);
    assert!(size_of::<AudioChannelDescription>() == SIZEOF_AudioChannelDescription);
    assert!(offset_of!(AudioChannelLayoutStereo, mChannelDescriptions) == OFFSETOF_AudioChannelLayout_mChannelDescriptions);
};

pub const kCFStringEncodingUTF8: u32 = 0x0800_0100;

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    pub fn CFStringCreateWithBytes(alloc: CFAllocatorRef, bytes: *const u8, len: CFIndex, encoding: u32, external: Boolean) -> CFStringRef;
    pub fn CFStringGetCString(s: CFStringRef, buf: *mut u8, size: CFIndex, encoding: u32) -> Boolean;
    pub fn CFUUIDGetUUIDBytes(uuid: CFUUIDRef) -> CFUUIDBytes;
    pub fn CFRetain(cf: CFTypeRef) -> CFTypeRef;
    pub fn CFRelease(cf: CFTypeRef);
    pub fn CFGetTypeID(cf: CFTypeRef) -> usize;
    pub fn CFStringGetTypeID() -> usize;
    pub fn CFStringGetLength(s: CFStringRef) -> CFIndex;
    pub fn CFStringGetMaximumSizeForEncoding(len: CFIndex, encoding: u32) -> CFIndex;
}

/// Creates a +1 CFString. The caller (usually the HAL) releases it.
pub fn cfstring(s: &str) -> CFStringRef {
    unsafe { CFStringCreateWithBytes(std::ptr::null(), s.as_ptr(), s.len() as CFIndex, kCFStringEncodingUTF8, 0) }
}

/// Reads a CFString into a Rust string; `None` for null, non-string or
/// non-UTF-8 input.
///
/// # Safety
/// `s` must be null or a valid CoreFoundation object.
pub unsafe fn string_from_cf(s: CFStringRef) -> Option<String> {
    if s.is_null() || CFGetTypeID(s) != CFStringGetTypeID() {
        return None;
    }
    let cap = CFStringGetMaximumSizeForEncoding(CFStringGetLength(s), kCFStringEncodingUTF8) as usize + 1;
    let mut buf = vec![0u8; cap];
    let ok = CFStringGetCString(s, buf.as_mut_ptr(), cap as CFIndex, kCFStringEncodingUTF8);
    if ok == 0 {
        return None;
    }
    let len = buf.iter().position(|&b| b == 0).unwrap_or(cap);
    buf.truncate(len);
    String::from_utf8(buf).ok()
}

extern "C" {
    fn syslog(priority: i32, fmt: *const u8, ...);
}

/// Logs to the unified log via syslog(3). Never call from the IO thread.
pub fn log(msg: &str) {
    const LOG_NOTICE: i32 = 5;
    let mut line = String::with_capacity(msg.len() + 16);
    line.push_str("LoopCraft: ");
    line.push_str(msg);
    line.push('\0');
    unsafe { syslog(LOG_NOTICE, c"%s".as_ptr() as *const u8, line.as_ptr()) };
}
