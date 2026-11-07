//! The stable plugin ABI (version 1). Everything here is `#[repr(C)]`.
//!
//! Contract:
//! * A plugin DLL exports `nin_plugin_entry` (required) and `nin_plugin_shutdown` (optional):
//!   `extern "C" fn nin_plugin_entry(host_abi_version: u32, host: *const NinHostApi) -> *const NinPluginInfo`
//!   `extern "C" fn nin_plugin_shutdown()`
//! * `nin_plugin_entry` returns null if it does not support `host_abi_version`. The returned
//!   info (and everything it points to) must stay valid until `nin_plugin_shutdown`/unload.
//! * Functions take a JSON array (UTF-8, not necessarily NUL-terminated, `args_len` bytes) and
//!   write a result into `out`. Return 0 = success (`out` holds JSON text), non-zero = error
//!   (`out` holds a UTF-8 message). Codes used by the SDK: 1 = function error, 2 = panic,
//!   3 = bad arguments.
//! * Buffers written to `out` are allocated by the plugin; the host MUST release them with
//!   `NinPluginInfo::free_buf` (never with its own allocator).
//! * `NinHostApi` is append-only: new fields are only added at the end; `size` tells how many
//!   bytes the host struct really has.
//! * Panics must never unwind across these functions.

use std::os::raw::c_char;

/// Current ABI version.
pub const NIN_PLUGIN_ABI_VERSION: u32 = 1;

/// Error code: function returned `Err`.
pub const NIN_ERR_FUNCTION: i32 = 1;
/// Error code: function panicked (panic was contained).
pub const NIN_ERR_PANIC: i32 = 2;
/// Error code: arguments were not valid JSON / null pointers.
pub const NIN_ERR_BAD_ARGS: i32 = 3;

/// Log levels for [`NinHostApi::log`].
pub const NIN_LOG_DEBUG: i32 = 0;
pub const NIN_LOG_INFO: i32 = 1;
pub const NIN_LOG_WARN: i32 = 2;
pub const NIN_LOG_ERROR: i32 = 3;

/// Plugin-allocated byte buffer (a decomposed `Vec<u8>`).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct NinBuf {
    pub ptr: *mut u8,
    pub len: usize,
    pub cap: usize,
}

impl NinBuf {
    pub const fn empty() -> NinBuf {
        NinBuf { ptr: std::ptr::null_mut(), len: 0, cap: 0 }
    }
}

/// `args_json` points to `args_len` bytes of a JSON array.
pub type NinCallFn = unsafe extern "C" fn(args_json: *const c_char, args_len: usize, out: *mut NinBuf) -> i32;
/// Releases a buffer previously produced by the same plugin.
pub type NinFreeBufFn = unsafe extern "C" fn(buf: NinBuf);
/// Host log callback.
pub type NinLogFn = unsafe extern "C" fn(level: i32, msg: *const c_char);
/// Plugin entry point signature.
pub type NinEntryFn = unsafe extern "C" fn(host_abi_version: u32, host: *const NinHostApi) -> *const NinPluginInfo;
/// Plugin shutdown signature.
pub type NinShutdownFn = unsafe extern "C" fn();

/// Name of the required export.
pub const ENTRY_SYMBOL: &[u8] = b"nin_plugin_entry\0";
/// Name of the optional export.
pub const SHUTDOWN_SYMBOL: &[u8] = b"nin_plugin_shutdown\0";

/// Host services offered to plugins. Append-only; `size` is `size_of` the host's struct.
#[repr(C)]
pub struct NinHostApi {
    pub size: usize,
    pub abi_version: u32,
    pub log: Option<NinLogFn>,
    pub host_name: *const c_char,
    pub host_version: *const c_char,
}
// SAFETY: contains only immutable pointers to static data and fn pointers.
unsafe impl Sync for NinHostApi {}
unsafe impl Send for NinHostApi {}

/// One exported plugin function.
#[repr(C)]
pub struct NinFunction {
    pub name: *const c_char,
    pub doc: *const c_char,
    pub min_args: i32,
    /// -1 = variadic
    pub max_args: i32,
    /// `Option` is layout-identical to a plain fn pointer; null is rejected by the host.
    pub call: Option<NinCallFn>,
}
unsafe impl Sync for NinFunction {}
unsafe impl Send for NinFunction {}

/// Plugin metadata returned by `nin_plugin_entry`.
#[repr(C)]
pub struct NinPluginInfo {
    pub abi_version: u32,
    pub name: *const c_char,
    pub version: *const c_char,
    pub description: *const c_char,
    pub function_count: u32,
    pub functions: *const NinFunction,
    pub free_buf: Option<NinFreeBufFn>,
}
unsafe impl Sync for NinPluginInfo {}
unsafe impl Send for NinPluginInfo {}
