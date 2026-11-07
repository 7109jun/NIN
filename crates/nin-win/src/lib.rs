//! NIN Windows backend: process execution (with Windows Job Objects for tree-kill and
//! resource limits), console helpers, `.nin` file association and system information.
//!
//! Everything has a portable fallback so the logic is unit-testable on Linux; the
//! Windows implementation uses the Win32 API directly (`windows-sys`).

#![cfg_attr(not(windows), allow(dead_code))]

pub mod assoc;
pub mod console;
pub mod process;
pub mod sys;

pub use assoc::{assoc_status, install_assoc, uninstall_assoc, AssocInfo};
pub use console::{enable_vt, install_ctrlc_handler, is_exclusive_console, is_terminal_stdout, pause_for_key, set_title, set_utf8_codepage, terminal_width};
pub use process::{
    build_command_line, quote_arg, resolve_program, run, Child, Limits, OutSpec, ProcResult, ProcSpec, ProcessGroup, StdinSpec,
};
pub use sys::{env_vars, find_processes, kill_pid, known_dir, list_processes, sys_info, KnownDir, ProcInfo, SysInfo};

#[cfg(windows)]
pub(crate) mod winutil {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};

    /// NUL-terminated UTF-16 for Win32 calls.
    pub fn wide(s: impl AsRef<OsStr>) -> Vec<u16> {
        s.as_ref().encode_wide().chain(std::iter::once(0)).collect()
    }

    /// RAII wrapper that closes the handle on drop.
    pub struct Handle(pub HANDLE);
    // SAFETY: Win32 kernel object handles may be used and closed from any thread.
    unsafe impl Send for Handle {}
    unsafe impl Sync for Handle {}
    impl Handle {
        pub fn is_valid(&self) -> bool {
            !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE
        }
    }
    impl Drop for Handle {
        fn drop(&mut self) {
            if self.is_valid() {
                // SAFETY: the handle is owned by this wrapper and closed exactly once.
                unsafe { CloseHandle(self.0) };
            }
        }
    }
}
