//! Plugin-author SDK: the [`export_plugin!`](crate::export_plugin) macro and its runtime support.
//! Does not depend on libloading.

use crate::abi::*;
use serde_json::Value;
use std::ffi::CString;
use std::os::raw::c_char;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicPtr, Ordering};

/// Signature of a plugin function written by plugin authors.
pub type PluginFn = fn(&Value) -> Result<Value, String>;

static HOST: AtomicPtr<NinHostApi> = AtomicPtr::new(std::ptr::null_mut());

#[doc(hidden)]
pub unsafe fn __set_host(h: *const NinHostApi) {
    let ok = !h.is_null() && (*h).size >= std::mem::size_of::<NinHostApi>();
    HOST.store(if ok { h as *mut NinHostApi } else { std::ptr::null_mut() }, Ordering::Release);
}

/// The host API given at entry, if any (and complete).
pub fn host_api() -> Option<&'static NinHostApi> {
    let p = HOST.load(Ordering::Acquire);
    // SAFETY: per ABI contract the host struct outlives the plugin.
    if p.is_null() { None } else { Some(unsafe { &*p }) }
}

unsafe fn cstr(p: *const c_char) -> Option<String> {
    if p.is_null() { None } else { Some(std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()) }
}

/// `(host_name, host_version)` if the host supplied them.
pub fn host_info() -> Option<(String, String)> {
    let h = host_api()?;
    // SAFETY: pointers come from the host and are NUL-terminated by contract.
    unsafe { Some((cstr(h.host_name)?, cstr(h.host_version)?)) }
}

/// Send a log line to the host (no-op without a host).
pub fn log(level: i32, msg: &str) {
    if let Some(Some(f)) = host_api().map(|h| h.log) {
        let c = CString::new(msg.replace('\0', " ")).unwrap_or_default();
        // SAFETY: valid NUL-terminated string; callback is host-provided.
        unsafe { f(level, c.as_ptr()) }
    }
}

fn write_out(out: *mut NinBuf, bytes: Vec<u8>) {
    let mut v = std::mem::ManuallyDrop::new(bytes);
    let buf = NinBuf { ptr: v.as_mut_ptr(), len: v.len(), cap: v.capacity() };
    // SAFETY: caller verified `out` is non-null.
    unsafe { *out = buf };
}

/// Frees a buffer produced by this plugin. Exposed through `NinPluginInfo::free_buf`.
pub unsafe extern "C" fn free_buf(buf: NinBuf) {
    if !buf.ptr.is_null() {
        drop(Vec::from_raw_parts(buf.ptr, buf.len, buf.cap));
    }
}

fn panic_message(p: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}

/// Generic trampoline used by the macro: parse args, call `f` under `catch_unwind`,
/// marshal result into `out`.
///
/// # Safety
/// `args` must be valid for `len` bytes (or null with `len == 0`), `out` valid for writes.
pub unsafe fn call_trampoline(f: PluginFn, args: *const c_char, len: usize, out: *mut NinBuf) -> i32 {
    if out.is_null() {
        return NIN_ERR_BAD_ARGS;
    }
    let r = catch_unwind(AssertUnwindSafe(|| -> Result<Vec<u8>, (i32, String)> {
        if args.is_null() && len != 0 {
            return Err((NIN_ERR_BAD_ARGS, "null arguments pointer".into()));
        }
        let bytes: &[u8] = if len == 0 { b"[]" } else { std::slice::from_raw_parts(args as *const u8, len) };
        let v: Value = serde_json::from_slice(bytes)
            .map_err(|e| (NIN_ERR_BAD_ARGS, format!("invalid arguments JSON: {e}")))?;
        let res = f(&v).map_err(|m| (NIN_ERR_FUNCTION, m))?;
        serde_json::to_vec(&res).map_err(|e| (NIN_ERR_FUNCTION, format!("result not serializable: {e}")))
    }));
    match r {
        Ok(Ok(b)) => {
            write_out(out, b);
            0
        }
        Ok(Err((code, msg))) => {
            write_out(out, msg.into_bytes());
            code
        }
        Err(p) => {
            write_out(out, format!("panic: {}", panic_message(p)).into_bytes());
            NIN_ERR_PANIC
        }
    }
}

/// Exports a NIN plugin.
///
/// ```ignore
/// nin_plugin::export_plugin! {
///     name: "hello", version: "0.1.0", description: "demo",
///     functions: [ ("greet", "greet(name?)", 0, 1, greet) ]
/// }
/// ```
/// Each function has type `fn(&serde_json::Value) -> Result<serde_json::Value, String>` and
/// receives the argument array. Generates `nin_plugin_entry` and `nin_plugin_shutdown`.
#[macro_export]
macro_rules! export_plugin {
    (
        name: $name:literal, version: $version:literal, description: $desc:literal,
        functions: [ $( ($fname:literal, $fdoc:literal, $min:expr, $max:expr, $func:path) ),* $(,)? ]
    ) => {
        const __NIN_FUNCS: &[$crate::abi::NinFunction] = &[
            $( $crate::abi::NinFunction {
                name: concat!($fname, "\0").as_ptr() as *const ::std::os::raw::c_char,
                doc: concat!($fdoc, "\0").as_ptr() as *const ::std::os::raw::c_char,
                min_args: $min,
                max_args: $max,
                call: Some({
                    unsafe extern "C" fn __tramp(
                        a: *const ::std::os::raw::c_char,
                        n: usize,
                        out: *mut $crate::abi::NinBuf,
                    ) -> i32 {
                        $crate::sdk::call_trampoline($func, a, n, out)
                    }
                    __tramp
                }),
            } ),*
        ];

        static __NIN_INFO: $crate::abi::NinPluginInfo = $crate::abi::NinPluginInfo {
            abi_version: $crate::abi::NIN_PLUGIN_ABI_VERSION,
            name: concat!($name, "\0").as_ptr() as *const ::std::os::raw::c_char,
            version: concat!($version, "\0").as_ptr() as *const ::std::os::raw::c_char,
            description: concat!($desc, "\0").as_ptr() as *const ::std::os::raw::c_char,
            function_count: __NIN_FUNCS.len() as u32,
            functions: __NIN_FUNCS.as_ptr(),
            free_buf: Some($crate::sdk::free_buf),
        };

        #[no_mangle]
        pub unsafe extern "C" fn nin_plugin_entry(
            host_abi_version: u32,
            host: *const $crate::abi::NinHostApi,
        ) -> *const $crate::abi::NinPluginInfo {
            if host_abi_version != $crate::abi::NIN_PLUGIN_ABI_VERSION {
                return ::std::ptr::null();
            }
            $crate::sdk::__set_host(host);
            &__NIN_INFO
        }

        #[no_mangle]
        pub unsafe extern "C" fn nin_plugin_shutdown() {}
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn run(f: PluginFn, args: &[u8]) -> (i32, String) {
        let mut out = NinBuf::empty();
        let code = unsafe { call_trampoline(f, args.as_ptr() as *const c_char, args.len(), &mut out) };
        let s = unsafe { String::from_utf8_lossy(std::slice::from_raw_parts(out.ptr, out.len)).into_owned() };
        unsafe { free_buf(out) };
        (code, s)
    }
    fn ok(v: &Value) -> Result<Value, String> { Ok(json!({"got": v})) }
    fn err(_: &Value) -> Result<Value, String> { Err("nope".into()) }
    fn boom(_: &Value) -> Result<Value, String> { panic!("kaboom") }

    #[test]
    fn success() { assert_eq!(run(ok, b"[1,2]"), (0, r#"{"got":[1,2]}"#.into())); }
    #[test]
    fn error() { assert_eq!(run(err, b"[]"), (NIN_ERR_FUNCTION, "nope".into())); }
    #[test]
    fn panic_contained() {
        let (c, m) = run(boom, b"[]");
        assert_eq!(c, NIN_ERR_PANIC);
        assert!(m.contains("kaboom"));
    }
    #[test]
    fn bad_json() { assert_eq!(run(ok, b"[1,").0, NIN_ERR_BAD_ARGS); }
    #[test]
    fn null_out() {
        assert_eq!(unsafe { call_trampoline(ok, std::ptr::null(), 0, std::ptr::null_mut()) }, NIN_ERR_BAD_ARGS);
    }
    #[test]
    fn null_args_nonzero_len() {
        let mut out = NinBuf::empty();
        assert_eq!(unsafe { call_trampoline(ok, std::ptr::null(), 4, &mut out) }, NIN_ERR_BAD_ARGS);
        unsafe { free_buf(out) };
    }
}
