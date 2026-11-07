//! Windows-only internals: RAII handle wrapper and handle/pipe helpers.

use std::ffi::c_void;
use std::io;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{
    CloseHandle, DuplicateHandle, DUPLICATE_SAME_ACCESS, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::GetCurrentProcess;

/// Owned kernel handle, closed on drop.
pub struct Handle(HANDLE);

// SAFETY: a kernel HANDLE is just an identifier usable from any thread.
unsafe impl Send for Handle {}
// SAFETY: see above; all operations we perform on it are thread-safe kernel calls.
unsafe impl Sync for Handle {}

impl Handle {
    /// Take ownership of `h`; `None` for NULL / INVALID_HANDLE_VALUE.
    pub fn from_raw(h: HANDLE) -> Option<Handle> {
        if h.is_null() || h == INVALID_HANDLE_VALUE {
            None
        } else {
            Some(Handle(h))
        }
    }

    pub fn raw(&self) -> HANDLE {
        self.0
    }

    /// Release ownership without closing.
    pub fn into_raw(self) -> HANDLE {
        let h = self.0;
        std::mem::forget(self);
        h
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: we own the handle and close it exactly once.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

/// Duplicate `h` within this process, optionally making the copy inheritable.
pub fn dup_handle(h: HANDLE, inherit: bool) -> io::Result<Handle> {
    let mut out: HANDLE = null_mut();
    // SAFETY: plain DuplicateHandle on the current process pseudo handle; `out` is a valid out ptr.
    let ok = unsafe {
        let cur = GetCurrentProcess();
        DuplicateHandle(cur, h, cur, &mut out, 0, inherit as i32, DUPLICATE_SAME_ACCESS)
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Handle::from_raw(out).ok_or_else(|| io::Error::new(io::ErrorKind::Other, "invalid duplicate"))
}

/// Anonymous pipe; both ends are NON-inheritable. Returns (read, write).
pub fn create_pipe() -> io::Result<(Handle, Handle)> {
    let mut r: HANDLE = null_mut();
    let mut w: HANDLE = null_mut();
    // SAFETY: valid out pointers, default security (non-inheritable), 64 KiB buffer.
    let ok = unsafe { CreatePipe(&mut r, &mut w, null::<c_void>() as *const _, 65536) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let rh = Handle::from_raw(r);
    let wh = Handle::from_raw(w);
    match (rh, wh) {
        (Some(a), Some(b)) => Ok((a, b)),
        _ => Err(io::Error::new(io::ErrorKind::Other, "CreatePipe returned invalid handles")),
    }
}
