//! Host-side plugin loader (feature `host`).

use crate::abi::*;
use libloading::Library;
use std::collections::HashMap;
use std::ffi::CStr;
use std::fmt;
use std::os::raw::c_char;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::RwLock;

/// Maximum functions a plugin may export.
pub const MAX_FUNCTIONS: u32 = 4096;
/// Maximum size of a result / error buffer accepted from a plugin.
pub const MAX_RESULT_BYTES: usize = 16 * 1024 * 1024;
/// Maximum size of a serialized argument array sent to a plugin.
pub const MAX_ARGS_BYTES: usize = 16 * 1024 * 1024;
const MAX_STR: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub enum PluginError {
    Load(String),
    MissingEntry,
    AbiMismatch { host: u32, plugin: u32 },
    BadInfo(String),
    NoSuchFunction(String),
    Arity { func: String, min: i32, max: i32, got: usize },
    Call { code: i32, message: String },
    Json(String),
}

impl fmt::Display for PluginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PluginError::Load(m) => write!(f, "cannot load plugin: {m}"),
            PluginError::MissingEntry => write!(f, "plugin does not export nin_plugin_entry"),
            PluginError::AbiMismatch { host, plugin } => {
                write!(f, "plugin ABI mismatch (host {host}, plugin {plugin})")
            }
            PluginError::BadInfo(m) => write!(f, "invalid plugin metadata: {m}"),
            PluginError::NoSuchFunction(n) => write!(f, "plugin has no function '{n}'"),
            PluginError::Arity { func, min, max, got } => {
                let want = if *max < 0 { format!("at least {min}") } else if min == max { format!("{min}") } else { format!("{min}..{max}") };
                write!(f, "'{func}' expects {want} argument(s), got {got}")
            }
            PluginError::Call { code, message } => write!(f, "plugin error ({code}): {message}"),
            PluginError::Json(m) => write!(f, "plugin JSON error: {m}"),
        }
    }
}
impl std::error::Error for PluginError {}

#[derive(Debug, Clone, PartialEq)]
pub struct PluginFunction {
    pub name: String,
    pub doc: String,
    pub min_args: i32,
    pub max_args: i32,
}

/// Metadata only (result of [`probe`]).
#[derive(Debug, Clone, PartialEq)]
pub struct PluginMeta {
    pub name: String,
    pub version: String,
    pub description: String,
    pub functions: Vec<PluginFunction>,
}

/// Pure ABI-version check.
pub fn check_abi(host: u32, plugin: u32) -> Result<(), PluginError> {
    if host == plugin { Ok(()) } else { Err(PluginError::AbiMismatch { host, plugin }) }
}

/// Pure arity check.
pub fn check_arity(func: &str, min: i32, max: i32, got: usize) -> Result<(), PluginError> {
    let too_few = (got as u64) < min.max(0) as u64;
    let too_many = max >= 0 && got as u64 > max as u64;
    if too_few || too_many {
        Err(PluginError::Arity { func: func.to_string(), min, max, got })
    } else {
        Ok(())
    }
}

// ---- host API handed to plugins -------------------------------------------------------

type Logger = Box<dyn Fn(i32, &str) + Send + Sync>;
static LOGGER: RwLock<Option<Logger>> = RwLock::new(None);

/// Route plugin `log()` calls to `f(level, message)`. Default: stderr.
pub fn set_host_logger(f: Box<dyn Fn(i32, &str) + Send + Sync>) {
    *LOGGER.write().unwrap_or_else(|e| e.into_inner()) = Some(f);
}

unsafe extern "C" fn host_log(level: i32, msg: *const c_char) {
    if msg.is_null() {
        return;
    }
    let s = CStr::from_ptr(msg).to_string_lossy().into_owned();
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let g = LOGGER.read().unwrap_or_else(|e| e.into_inner());
        match g.as_ref() {
            Some(f) => f(level, &s),
            None => eprintln!("[plugin:{level}] {s}"),
        }
    }));
}

static HOST_API: NinHostApi = NinHostApi {
    size: std::mem::size_of::<NinHostApi>(),
    abi_version: NIN_PLUGIN_ABI_VERSION,
    log: Some(host_log),
    host_name: b"NIN Shell\0".as_ptr() as *const c_char,
    host_version: concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr() as *const c_char,
};

// ---- loader ---------------------------------------------------------------------------

struct Entry {
    meta: PluginFunction,
    call: NinCallFn,
}

/// A loaded plugin. Keeps the library alive; thread-safe.
pub struct LoadedPlugin {
    pub name: String,
    pub version: String,
    pub description: String,
    pub path: PathBuf,
    pub functions: Vec<PluginFunction>,
    entries: HashMap<String, Entry>,
    free_buf: NinFreeBufFn,
    shutdown: Option<NinShutdownFn>,
    _lib: Library, // dropped last (declared last among fields that use its code)
}

impl fmt::Debug for LoadedPlugin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoadedPlugin").field("name", &self.name).field("version", &self.version)
            .field("path", &self.path).field("functions", &self.functions).finish()
    }
}

unsafe fn read_cstr(p: *const c_char, what: &str, required: bool) -> Result<String, PluginError> {
    if p.is_null() {
        return if required { Err(PluginError::BadInfo(format!("{what} is null"))) } else { Ok(String::new()) };
    }
    // Bounded scan so a missing terminator cannot run away.
    let mut n = 0usize;
    while n <= MAX_STR {
        if *p.add(n) == 0 { break; }
        n += 1;
    }
    if n > MAX_STR {
        return Err(PluginError::BadInfo(format!("{what} too long or unterminated")));
    }
    let bytes = std::slice::from_raw_parts(p as *const u8, n);
    let s = std::str::from_utf8(bytes).map_err(|_| PluginError::BadInfo(format!("{what} is not valid UTF-8")))?;
    if required && s.is_empty() {
        return Err(PluginError::BadInfo(format!("{what} is empty")));
    }
    Ok(s.to_string())
}

impl LoadedPlugin {
    pub fn load(path: &Path) -> Result<LoadedPlugin, PluginError> {
        // SAFETY: loading a library runs its initializers; that is inherent to plugins.
        let lib = unsafe { Library::new(path) }.map_err(|e| PluginError::Load(e.to_string()))?;
        let entry: NinEntryFn = unsafe {
            *lib.get::<NinEntryFn>(ENTRY_SYMBOL).map_err(|_| PluginError::MissingEntry)?
        };
        let shutdown: Option<NinShutdownFn> = unsafe { lib.get::<NinShutdownFn>(SHUTDOWN_SYMBOL).ok().map(|s| *s) };

        let info_ptr = unsafe { entry(NIN_PLUGIN_ABI_VERSION, &HOST_API) };
        if info_ptr.is_null() {
            // Plugin declined our ABI version; we cannot know its version.
            return Err(PluginError::AbiMismatch { host: NIN_PLUGIN_ABI_VERSION, plugin: 0 });
        }
        // From here on a failure must still call shutdown.
        let parsed = unsafe { Self::parse_info(info_ptr) };
        let (name, version, description, funcs, free_buf) = match parsed {
            Ok(v) => v,
            Err(e) => {
                if let Some(s) = shutdown { unsafe { s() } }
                return Err(e);
            }
        };
        let mut entries = HashMap::new();
        let mut functions = Vec::new();
        for (m, c) in funcs {
            functions.push(m.clone());
            entries.insert(m.name.clone(), Entry { meta: m, call: c });
        }
        Ok(LoadedPlugin {
            name, version, description, path: path.to_path_buf(), functions, entries, free_buf, shutdown, _lib: lib,
        })
    }

    #[allow(clippy::type_complexity)]
    unsafe fn parse_info(
        info: *const NinPluginInfo,
    ) -> Result<(String, String, String, Vec<(PluginFunction, NinCallFn)>, NinFreeBufFn), PluginError> {
        let info = &*info;
        check_abi(NIN_PLUGIN_ABI_VERSION, info.abi_version)?;
        let name = read_cstr(info.name, "plugin name", true)?;
        let version = read_cstr(info.version, "plugin version", true)?;
        let description = read_cstr(info.description, "plugin description", false)?;
        let free_buf = info.free_buf.ok_or_else(|| PluginError::BadInfo("free_buf is null".into()))?;
        if info.function_count > MAX_FUNCTIONS {
            return Err(PluginError::BadInfo(format!("too many functions ({})", info.function_count)));
        }
        if info.function_count > 0 && info.functions.is_null() {
            return Err(PluginError::BadInfo("functions pointer is null".into()));
        }
        let raw: &[NinFunction] = if info.function_count == 0 {
            &[]
        } else {
            std::slice::from_raw_parts(info.functions, info.function_count as usize)
        };
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for (i, f) in raw.iter().enumerate() {
            let fname = read_cstr(f.name, &format!("function #{i} name"), true)?;
            let doc = read_cstr(f.doc, &format!("function '{fname}' doc"), false)?;
            if !seen.insert(fname.clone()) {
                return Err(PluginError::BadInfo(format!("duplicate function '{fname}'")));
            }
            if f.min_args < 0 || f.max_args < -1 || (f.max_args >= 0 && f.max_args < f.min_args) {
                return Err(PluginError::BadInfo(format!(
                    "function '{fname}' has invalid arity {}..{}", f.min_args, f.max_args
                )));
            }
            let call = f.call.ok_or_else(|| PluginError::BadInfo(format!("function '{fname}' has null call pointer")))?;
            out.push((PluginFunction { name: fname, doc, min_args: f.min_args, max_args: f.max_args }, call));
        }
        Ok((name, version, description, out, free_buf))
    }

    pub fn has_function(&self, name: &str) -> bool {
        self.entries.contains_key(name)
    }

    pub fn meta(&self) -> PluginMeta {
        PluginMeta {
            name: self.name.clone(),
            version: self.version.clone(),
            description: self.description.clone(),
            functions: self.functions.clone(),
        }
    }

    /// Call `func` with `args` (must be a JSON array). Safe to call concurrently.
    pub fn call(&self, func: &str, args: &serde_json::Value) -> Result<serde_json::Value, PluginError> {
        let e = self.entries.get(func).ok_or_else(|| PluginError::NoSuchFunction(func.to_string()))?;
        let arr = args.as_array().ok_or_else(|| PluginError::Json("arguments must be a JSON array".into()))?;
        check_arity(func, e.meta.min_args, e.meta.max_args, arr.len())?;
        let payload = serde_json::to_vec(args).map_err(|er| PluginError::Json(er.to_string()))?;
        if payload.len() > MAX_ARGS_BYTES {
            return Err(PluginError::Json("arguments too large".into()));
        }
        let mut out = NinBuf::empty();
        // SAFETY: payload valid for its length; out is a valid NinBuf.
        let code = unsafe { (e.call)(payload.as_ptr() as *const c_char, payload.len(), &mut out) };
        let bytes: Result<Vec<u8>, PluginError> = if out.ptr.is_null() {
            Ok(Vec::new())
        } else if out.len > out.cap || out.len > MAX_RESULT_BYTES {
            Err(PluginError::Json(format!("plugin result too large or corrupt ({} bytes)", out.len)))
        } else {
            // SAFETY: plugin promised ptr/len valid.
            Ok(unsafe { std::slice::from_raw_parts(out.ptr, out.len) }.to_vec())
        };
        if !out.ptr.is_null() {
            // SAFETY: buffer came from this plugin; released by its own allocator.
            unsafe { (self.free_buf)(out) };
        }
        let bytes = bytes?;
        if code != 0 {
            return Err(PluginError::Call { code, message: String::from_utf8_lossy(&bytes).into_owned() });
        }
        serde_json::from_slice(&bytes).map_err(|er| PluginError::Json(format!("invalid result JSON: {er}")))
    }
}

impl Drop for LoadedPlugin {
    fn drop(&mut self) {
        if let Some(s) = self.shutdown {
            let _ = catch_unwind(|| unsafe { s() });
        }
    }
}

/// Load a plugin, read its metadata, unload it.
pub fn probe(path: &Path) -> Result<PluginMeta, PluginError> {
    Ok(LoadedPlugin::load(path)?.meta())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abi_check() {
        assert!(check_abi(1, 1).is_ok());
        assert_eq!(check_abi(1, 2), Err(PluginError::AbiMismatch { host: 1, plugin: 2 }));
    }

    #[test]
    fn arity_check() {
        assert!(check_arity("f", 0, 1, 0).is_ok());
        assert!(check_arity("f", 0, 1, 1).is_ok());
        assert!(check_arity("f", 0, 1, 2).is_err());
        assert!(check_arity("f", 2, -1, 1).is_err());
        assert!(check_arity("f", 2, -1, 99).is_ok());
        let e = check_arity("f", 2, 2, 3).unwrap_err();
        assert!(e.to_string().contains("expects 2"));
    }

    #[test]
    fn load_missing_file() {
        assert!(matches!(LoadedPlugin::load(Path::new("/nonexistent/x.so")), Err(PluginError::Load(_))));
    }

    #[test]
    fn load_non_plugin_library() {
        // libc/libm is a valid shared object without nin_plugin_entry (Linux only).
        #[cfg(target_os = "linux")]
        {
            let r = LoadedPlugin::load(Path::new("libm.so.6"));
            assert!(matches!(r, Err(PluginError::MissingEntry)), "{r:?}");
        }
    }

    #[test]
    fn cstr_validation() {
        unsafe {
            assert!(read_cstr(std::ptr::null(), "x", true).is_err());
            assert_eq!(read_cstr(std::ptr::null(), "x", false).unwrap(), "");
            assert!(read_cstr(b"\xff\0".as_ptr() as *const c_char, "x", true).is_err());
            assert!(read_cstr(b"\0".as_ptr() as *const c_char, "x", true).is_err());
            assert_eq!(read_cstr(b"ok\0".as_ptr() as *const c_char, "x", true).unwrap(), "ok");
        }
    }

    fn info_with(funcs: &[NinFunction]) -> NinPluginInfo {
        NinPluginInfo {
            abi_version: 1,
            name: b"n\0".as_ptr() as *const c_char,
            version: b"1\0".as_ptr() as *const c_char,
            description: std::ptr::null(),
            function_count: funcs.len() as u32,
            functions: funcs.as_ptr(),
            free_buf: Some(crate::sdk::free_buf),
        }
    }
    unsafe extern "C" fn nop(_: *const c_char, _: usize, _: *mut NinBuf) -> i32 { 0 }
    fn func(name: &'static [u8], min: i32, max: i32, call: Option<NinCallFn>) -> NinFunction {
        NinFunction { name: name.as_ptr() as *const c_char, doc: std::ptr::null(), min_args: min, max_args: max, call }
    }

    #[test]
    fn info_validation() {
        let good = [func(b"a\0", 0, 1, Some(nop)), func(b"b\0", 1, -1, Some(nop))];
        assert!(unsafe { LoadedPlugin::parse_info(&info_with(&good)) }.is_ok());
        let dup = [func(b"a\0", 0, 1, Some(nop)), func(b"a\0", 0, 1, Some(nop))];
        assert!(matches!(unsafe { LoadedPlugin::parse_info(&info_with(&dup)) }, Err(PluginError::BadInfo(_))));
        let badar = [func(b"a\0", 3, 1, Some(nop))];
        assert!(unsafe { LoadedPlugin::parse_info(&info_with(&badar)) }.is_err());
        let nullcall = [func(b"a\0", 0, 0, None)];
        assert!(unsafe { LoadedPlugin::parse_info(&info_with(&nullcall)) }.is_err());
        let mut wrong = info_with(&good);
        wrong.abi_version = 9;
        assert_eq!(
            unsafe { LoadedPlugin::parse_info(&wrong) }.err(),
            Some(PluginError::AbiMismatch { host: 1, plugin: 9 })
        );
        let mut many = info_with(&good);
        many.function_count = MAX_FUNCTIONS + 1;
        assert!(unsafe { LoadedPlugin::parse_info(&many) }.is_err());
        let mut nullf = info_with(&good);
        nullf.functions = std::ptr::null();
        assert!(unsafe { LoadedPlugin::parse_info(&nullf) }.is_err());
    }
}
