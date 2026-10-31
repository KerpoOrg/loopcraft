//! Running a render callback on a device's IO thread.

use super::*;
use std::marker::PhantomData;

/// The input buffers of one IO cycle: one interleaved buffer per stream.
pub struct Buffers<'a> {
    list: *const AudioBufferList,
    _cycle: PhantomData<&'a ()>,
}

/// The output buffers of one IO cycle.
pub struct BuffersMut<'a> {
    list: *mut AudioBufferList,
    _cycle: PhantomData<&'a mut ()>,
}

unsafe fn buffer_at(list: *const AudioBufferList, i: usize) -> Option<*const AudioBuffer> {
    if list.is_null() || i >= (*list).count as usize {
        return None;
    }
    Some((*list).buffers.as_ptr().add(i))
}

impl Buffers<'_> {
    pub fn count(&self) -> usize {
        if self.list.is_null() { 0 } else { unsafe { (*self.list).count as usize } }
    }

    /// Samples and channel count of buffer `i`.
    pub fn get(&self, i: usize) -> Option<(&[f32], usize)> {
        unsafe {
            let b = &*buffer_at(self.list, i)?;
            if b.data.is_null() {
                return None;
            }
            Some((std::slice::from_raw_parts(b.data as *const f32, b.byte_size as usize / 4), b.channels as usize))
        }
    }
}

impl BuffersMut<'_> {
    pub fn count(&self) -> usize {
        if self.list.is_null() { 0 } else { unsafe { (*self.list).count as usize } }
    }

    pub fn get_mut(&mut self, i: usize) -> Option<(&mut [f32], usize)> {
        unsafe {
            let b = &*buffer_at(self.list, i)?;
            if b.data.is_null() {
                return None;
            }
            Some((std::slice::from_raw_parts_mut(b.data as *mut f32, b.byte_size as usize / 4), b.channels as usize))
        }
    }
}

/// Called on the real-time IO thread: no allocation, locks or syscalls.
pub type Render = Box<dyn FnMut(&Buffers, &mut BuffersMut) + Send>;

/// A started IOProc; dropping it stops IO and frees the callback.
pub struct IoProc {
    device: AudioObjectID,
    id: IOProcID,
    render: *mut Render,
}

// The raw pointers are only used to stop and free the IOProc.
unsafe impl Send for IoProc {}

unsafe extern "C" fn trampoline(
    _dev: AudioObjectID,
    _now: *const c_void,
    input: *const AudioBufferList,
    _in_time: *const c_void,
    output: *mut AudioBufferList,
    _out_time: *const c_void,
    client: *mut c_void,
) -> OSStatus {
    let render = &mut *(client as *mut Render);
    let inputs = Buffers { list: input, _cycle: PhantomData };
    let mut outputs = BuffersMut { list: output, _cycle: PhantomData };
    // A panic must not unwind into Core Audio.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| render(&inputs, &mut outputs)));
    0
}

/// Which of a device's streams an IOProc uses, per direction, in stream order.
pub struct StreamUsage<'a> {
    pub inputs: &'a [bool],
    pub outputs: &'a [bool],
}

impl IoProc {
    /// Starts `render` on the device. With `usage`, streams marked unused are
    /// switched off for this IOProc and arrive as null buffers.
    pub fn start(device: AudioObjectID, usage: Option<StreamUsage>, render: Render) -> Result<IoProc> {
        let render = Box::into_raw(Box::new(render));
        let mut id: IOProcID = std::ptr::null_mut();
        if let Err(e) = check(unsafe { AudioDeviceCreateIOProcID(device, trampoline, render as *mut c_void, &mut id) }, || format!("creating an IOProc on device {device}")) {
            drop(unsafe { Box::from_raw(render) });
            return Err(e);
        }
        let io = IoProc { device, id, render };
        if let Some(u) = usage {
            set_stream_usage(device, id, Scope::Input, u.inputs)?;
            set_stream_usage(device, id, Scope::Output, u.outputs)?;
        }
        check(unsafe { AudioDeviceStart(device, id) }, || format!("starting IO on device {device}"))?;
        Ok(io)
    }
}

/// `kAudioDevicePropertyIOProcStreamUsage`
const STREAM_USAGE: u32 = u32::from_be_bytes(*b"suse");

/// Sets `AudioHardwareIOProcStreamUsage`: the IOProc ID, the stream count,
/// then one UInt32 on/off flag per stream.
fn set_stream_usage(device: AudioObjectID, id: IOProcID, scope: Scope, on: &[bool]) -> Result<()> {
    if on.is_empty() {
        return Ok(());
    }
    let mut data = Vec::with_capacity(12 + 4 * on.len());
    data.extend_from_slice(&(id as usize).to_ne_bytes());
    data.extend_from_slice(&(on.len() as u32).to_ne_bytes());
    for &s in on {
        data.extend_from_slice(&(s as u32).to_ne_bytes());
    }
    let a = address(STREAM_USAGE, scope);
    check(unsafe { AudioObjectSetPropertyData(device, &a, 0, std::ptr::null(), data.len() as u32, data.as_ptr() as *const c_void) }, || {
        format!("setting stream usage on device {device}")
    })
}

impl Drop for IoProc {
    fn drop(&mut self) {
        // Stop waits for a running cycle, so the callback is not used afterwards.
        unsafe {
            AudioDeviceStop(self.device, self.id);
            AudioDeviceDestroyIOProcID(self.device, self.id);
            drop(Box::from_raw(self.render));
        }
    }
}
