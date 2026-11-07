//! `nin build`: bundles a validated program and every file it imports into a copy of the
//! runtime executable. Layout of the produced file:
//!
//! ```text
//! [runtime exe bytes][deflate(JSON payload)][MAGIC 10B][payload_len u64 LE][crc32 u32 LE]
//! ```
//! At startup `nin.exe` looks for the trailer in its own image and, when present, runs the embedded
//! program instead of the CLI.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const MAGIC: &[u8; 10] = b"NINBUNDLE\x01";
const TRAILER: usize = 10 + 8 + 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bundle {
    /// key of the entry file in `files`
    pub main: String,
    /// normalized path → source text (includes the entry file)
    pub files: BTreeMap<String, String>,
}

/// Collect `main` and everything it imports through `file`/`package` imports.
/// `pkg_entry` maps a package name to the entry file of the installed package.
pub fn collect(main: &Path, pkg_entry: &dyn Fn(&str) -> Option<PathBuf>) -> Result<Bundle, String> {
    let canon = std::fs::canonicalize(main).map_err(|e| format!("cannot open {}: {}", main.display(), e))?;
    let main_key = nin_runtime::norm_key(&canon);
    let mut files = BTreeMap::new();
    let mut stack = vec![canon];
    while let Some(path) = stack.pop() {
        let key = nin_runtime::norm_key(&path);
        if files.contains_key(&key) {
            continue;
        }
        let text = std::fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {}", path.display(), e))?;
        let text = text.trim_start_matches('\u{feff}').to_string();
        let prog = nin_ast::parse_program(&text, 0).map_err(|ds| format!("{} has errors ({} diagnostic(s)); run `nin check` first", path.display(), ds.items.len()))?;
        let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
        for imp in &prog.imports {
            match &imp.kind {
                nin_ast::ImportKind::File(f) => {
                    let p = dir.join(f.replace('\\', "/"));
                    let c = std::fs::canonicalize(&p).map_err(|e| format!("{}: cannot resolve import `{}`: {}", path.display(), f, e))?;
                    stack.push(c);
                }
                nin_ast::ImportKind::Package(name) => match pkg_entry(name) {
                    Some(p) => stack.push(std::fs::canonicalize(&p).map_err(|e| format!("package `{}`: {}", name, e))?),
                    None => return Err(format!("{}: package `{}` is not installed", path.display(), name)),
                },
                nin_ast::ImportKind::Std(n) => {
                    if n.starts_with("plugin:") {
                        return Err(format!("{}: plugin imports (`{}`) cannot be embedded; ship the DLL next to the executable and use a path relative to it at run time is not supported in bundles yet", path.display(), n));
                    }
                }
            }
        }
        files.insert(key, text);
    }
    Ok(Bundle { main: main_key, files })
}

fn encode(b: &Bundle) -> Vec<u8> {
    let j = serde_json::json!({"main": b.main, "files": b.files});
    miniz_oxide::deflate::compress_to_vec(j.to_string().as_bytes(), 6)
}

fn decode(data: &[u8]) -> Result<Bundle, String> {
    let raw = miniz_oxide::inflate::decompress_to_vec(data).map_err(|e| format!("corrupt bundle: {:?}", e))?;
    let j: serde_json::Value = serde_json::from_slice(&raw).map_err(|e| format!("corrupt bundle: {}", e))?;
    let main = j["main"].as_str().ok_or("corrupt bundle: no entry")?.to_string();
    let mut files = BTreeMap::new();
    for (k, v) in j["files"].as_object().ok_or("corrupt bundle: no files")? {
        files.insert(k.clone(), v.as_str().ok_or("corrupt bundle: bad file")?.to_string());
    }
    if !files.contains_key(&main) {
        return Err("corrupt bundle: entry file missing".into());
    }
    Ok(Bundle { main, files })
}

/// Length of the runtime image inside `bytes` (excluding an existing bundle).
fn runtime_len(bytes: &[u8]) -> usize {
    if let Some((_, plen)) = trailer(bytes) {
        return bytes.len() - TRAILER - plen;
    }
    bytes.len()
}

fn trailer(bytes: &[u8]) -> Option<(u32, usize)> {
    if bytes.len() < TRAILER {
        return None;
    }
    let t = &bytes[bytes.len() - TRAILER..];
    if &t[..10] != MAGIC {
        return None;
    }
    let plen = u64::from_le_bytes(t[10..18].try_into().ok()?) as usize;
    let crc = u32::from_le_bytes(t[18..22].try_into().ok()?);
    if plen > bytes.len() - TRAILER {
        return None;
    }
    Some((crc, plen))
}

/// Create `out` = runtime image + bundle.
pub fn write_bundle(runtime_exe: &Path, bundle: &Bundle, out: &Path) -> Result<(), String> {
    let rt = std::fs::read(runtime_exe).map_err(|e| format!("cannot read runtime {}: {}", runtime_exe.display(), e))?;
    let n = runtime_len(&rt);
    let payload = encode(bundle);
    let mut data = rt[..n].to_vec();
    data.extend_from_slice(&payload);
    data.extend_from_slice(MAGIC);
    data.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    data.extend_from_slice(&nin_pkg::crc::crc32(&payload).to_le_bytes());
    std::fs::write(out, data).map_err(|e| format!("cannot write {}: {}", out.display(), e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(out, std::fs::Permissions::from_mode(0o755));
    }
    Ok(())
}

/// Read the bundle embedded in `exe`, if any.
pub fn read_embedded(exe: &Path) -> Result<Option<Bundle>, String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(exe).map_err(|e| e.to_string())?;
    let len = f.metadata().map_err(|e| e.to_string())?.len() as usize;
    if len < TRAILER {
        return Ok(None);
    }
    let mut t = vec![0u8; TRAILER];
    f.seek(SeekFrom::Start((len - TRAILER) as u64)).map_err(|e| e.to_string())?;
    f.read_exact(&mut t).map_err(|e| e.to_string())?;
    if &t[..10] != MAGIC {
        return Ok(None);
    }
    let plen = u64::from_le_bytes(t[10..18].try_into().unwrap()) as usize;
    if plen > len - TRAILER {
        return Ok(None);
    }
    let crc = u32::from_le_bytes(t[18..22].try_into().unwrap());
    let mut payload = vec![0u8; plen];
    f.seek(SeekFrom::Start((len - TRAILER - plen) as u64)).map_err(|e| e.to_string())?;
    f.read_exact(&mut payload).map_err(|e| e.to_string())?;
    if nin_pkg::crc::crc32(&payload) != crc {
        return Err("embedded program is corrupt (CRC mismatch)".into());
    }
    decode(&payload).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(n: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nin-compile-{}-{}", std::process::id(), n));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn collect_follows_imports() {
        let d = tmpdir("collect");
        std::fs::create_dir_all(d.join("lib")).unwrap();
        std::fs::write(d.join("main.nin"), r#"{"nin":"1","imports":[{"file":"./lib/a.nin"}],"main":[]}"#).unwrap();
        std::fs::write(d.join("lib/a.nin"), r#"{"nin":"1","imports":[{"file":"../b.nin"}]}"#).unwrap();
        std::fs::write(d.join("b.nin"), r#"{"nin":"1"}"#).unwrap();
        let b = collect(&d.join("main.nin"), &|_| None).unwrap();
        assert_eq!(b.files.len(), 3);
        assert!(b.files.contains_key(&b.main));
    }

    #[test]
    fn collect_reports_missing_and_broken() {
        let d = tmpdir("bad");
        std::fs::write(d.join("m.nin"), r#"{"nin":"1","imports":[{"file":"./nope.nin"}]}"#).unwrap();
        assert!(collect(&d.join("m.nin"), &|_| None).unwrap_err().contains("cannot resolve import"));
        std::fs::write(d.join("m.nin"), r#"{"nin":"1","imports":[{"package":"zz"}]}"#).unwrap();
        assert!(collect(&d.join("m.nin"), &|_| None).unwrap_err().contains("not installed"));
        std::fs::write(d.join("m.nin"), "{ broken").unwrap();
        assert!(collect(&d.join("m.nin"), &|_| None).unwrap_err().contains("has errors"));
    }

    #[test]
    fn roundtrip_and_rebundle() {
        let d = tmpdir("rt");
        let rt = d.join("rt.bin");
        std::fs::write(&rt, vec![7u8; 5000]).unwrap();
        let mut files = BTreeMap::new();
        files.insert("/x/main.nin".to_string(), "{\"nin\":\"1\"}".to_string());
        let b = Bundle { main: "/x/main.nin".into(), files };
        let out = d.join("out.bin");
        write_bundle(&rt, &b, &out).unwrap();
        assert_eq!(read_embedded(&out).unwrap().unwrap(), b);
        assert_eq!(read_embedded(&rt).unwrap(), None);
        // bundling a bundle replaces the old payload
        let out2 = d.join("out2.bin");
        let mut b2 = b.clone();
        b2.files.insert("/x/other.nin".into(), "{}".into());
        write_bundle(&out, &b2, &out2).unwrap();
        assert_eq!(read_embedded(&out2).unwrap().unwrap(), b2);
        assert_eq!(std::fs::read(&out2).unwrap()[..5000], vec![7u8; 5000][..]);
    }

    #[test]
    fn corrupt_payload_detected() {
        let d = tmpdir("corrupt");
        let rt = d.join("rt.bin");
        std::fs::write(&rt, vec![1u8; 100]).unwrap();
        let mut files = BTreeMap::new();
        files.insert("/m".to_string(), "{}".to_string());
        let out = d.join("o.bin");
        write_bundle(&rt, &Bundle { main: "/m".into(), files }, &out).unwrap();
        let mut bytes = std::fs::read(&out).unwrap();
        bytes[105] ^= 0xff;
        std::fs::write(&out, bytes).unwrap();
        assert!(read_embedded(&out).is_err());
    }
}
