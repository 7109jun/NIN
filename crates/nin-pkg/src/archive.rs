//! The `.nnpkg` container format.
//!
//! ```text
//! magic        8 bytes   "NNPKG\x01\0\0"
//! manifest_len u32 LE
//! manifest     UTF-8 TOML (nin-package.toml), <= 64 KiB
//! file_count   u32 LE    (<= 20_000)
//! file entry * file_count:
//!   path_len   u16 LE
//!   path       UTF-8, relative, '/' separated
//!   method     u8        0 = stored, 1 = raw deflate (miniz_oxide)
//!   raw_len    u32 LE    uncompressed size
//!   comp_len   u32 LE    size of the data that follows
//!   crc32      u32 LE    CRC32 of the uncompressed data
//!   data       comp_len bytes
//! trailer      u32 LE    CRC32 of every byte before the trailer
//! ```
//!
//! The reader validates magic, every length against the buffer, path safety
//! (zip-slip: no `..`, absolute paths, drive letters / `:`, NUL, backslashes,
//! Windows reserved names), duplicate paths, per-file CRC, the trailer CRC,
//! and size caps (256 MiB total uncompressed, 20k files).
//! It never panics on malformed input.

use crate::crc::crc32;
use crate::error::{PkgError, Result};
use crate::manifest::Manifest;
use std::fs;
use std::path::{Path, PathBuf};

pub const MAGIC: &[u8; 8] = b"NNPKG\x01\0\0";
pub const MANIFEST_FILE: &str = "nin-package.toml";
pub const MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_FILES: usize = 20_000;
pub const MAX_MANIFEST: usize = 64 * 1024;

/// A fully-validated, in-memory archive.
#[derive(Debug, Clone)]
pub struct Archive {
    pub manifest: Manifest,
    pub files: Vec<(String, Vec<u8>)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    /// Deflate each file when that makes it smaller.
    Auto,
    Stored,
    Deflate,
}

fn aerr<T>(m: impl Into<String>) -> Result<T> {
    Err(PkgError::Archive(m.into()))
}

/// Check a relative archive path; returns the reason on failure.
pub fn validate_path(p: &str) -> std::result::Result<(), String> {
    if p.is_empty() {
        return Err("empty path".into());
    }
    if p.len() > 1024 {
        return Err("path too long".into());
    }
    if p.contains('\0') {
        return Err("path contains NUL".into());
    }
    if p.contains('\\') {
        return Err("path contains a backslash".into());
    }
    if p.starts_with('/') {
        return Err("absolute path".into());
    }
    if p.contains(':') {
        return Err("path contains ':' (drive letter or stream)".into());
    }
    if p.chars().any(|c| c.is_control()) {
        return Err("path contains control characters".into());
    }
    for comp in p.split('/') {
        if comp.is_empty() {
            return Err("empty path component".into());
        }
        if comp == "." || comp == ".." {
            return Err(format!("path component `{comp}` not allowed"));
        }
        if comp.ends_with('.') || comp.ends_with(' ') {
            return Err(format!("path component `{comp}` ends with '.' or space"));
        }
        let stem = comp.split('.').next().unwrap_or("").to_ascii_uppercase();
        const RESERVED: [&str; 22] = [
            "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
            "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8",
            "LPT9",
        ];
        if RESERVED.contains(&stem.as_str()) {
            return Err(format!("reserved device name `{comp}`"));
        }
    }
    Ok(())
}

fn encode(
    manifest_toml: &str,
    files: &[(String, Vec<u8>)],
    comp: Compression,
    validate: bool,
) -> Result<Vec<u8>> {
    if files.len() > MAX_FILES {
        return aerr(format!("too many files ({} > {MAX_FILES})", files.len()));
    }
    if manifest_toml.len() > MAX_MANIFEST {
        return aerr("manifest too large");
    }
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(manifest_toml.len() as u32).to_le_bytes());
    out.extend_from_slice(manifest_toml.as_bytes());
    out.extend_from_slice(&(files.len() as u32).to_le_bytes());
    let mut total: u64 = 0;
    for (path, data) in files {
        if validate {
            if let Err(w) = validate_path(path) {
                return aerr(format!("unsafe path `{path}`: {w}"));
            }
        }
        if path.len() > u16::MAX as usize {
            return aerr(format!("path too long: {path}"));
        }
        if data.len() > u32::MAX as usize {
            return aerr(format!("file too large: {path}"));
        }
        total += data.len() as u64;
        if total > MAX_TOTAL_BYTES {
            return aerr("archive exceeds the 256 MiB size cap");
        }
        let deflated = match comp {
            Compression::Stored => None,
            _ => Some(miniz_oxide::deflate::compress_to_vec(data, 6)),
        };
        let (method, body): (u8, &[u8]) = match (&deflated, comp) {
            (Some(d), Compression::Deflate) => (1, d),
            (Some(d), _) if d.len() < data.len() => (1, d),
            _ => (0, data),
        };
        out.extend_from_slice(&(path.len() as u16).to_le_bytes());
        out.extend_from_slice(path.as_bytes());
        out.push(method);
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&crc32(data).to_le_bytes());
        out.extend_from_slice(body);
    }
    let c = crc32(&out);
    out.extend_from_slice(&c.to_le_bytes());
    Ok(out)
}

/// Serialize an archive to bytes.
pub fn write_archive(manifest: &Manifest, files: &[(String, Vec<u8>)], comp: Compression) -> Result<Vec<u8>> {
    encode(&manifest.to_toml(), files, comp, true)
}

/// Test hook: encode with an arbitrary manifest text and no path validation.
#[doc(hidden)]
pub fn write_archive_unchecked(manifest_toml: &str, files: &[(String, Vec<u8>)], comp: Compression) -> Vec<u8> {
    encode(manifest_toml, files, comp, false).expect("encode")
}

struct Cur<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Cur<'a> {
    fn take(&mut self, n: usize, what: &str) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n);
        match end {
            Some(e) if e <= self.b.len() => {
                let s = &self.b[self.pos..e];
                self.pos = e;
                Ok(s)
            }
            _ => aerr(format!(
                "truncated archive: needed {n} bytes for {what} at offset {}, only {} remain",
                self.pos,
                self.b.len() - self.pos.min(self.b.len())
            )),
        }
    }
    fn u8(&mut self, w: &str) -> Result<u8> {
        Ok(self.take(1, w)?[0])
    }
    fn u16(&mut self, w: &str) -> Result<u16> {
        let s = self.take(2, w)?;
        Ok(u16::from_le_bytes([s[0], s[1]]))
    }
    fn u32(&mut self, w: &str) -> Result<u32> {
        let s = self.take(4, w)?;
        Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }
}

fn read_manifest_part<'a>(c: &mut Cur<'a>) -> Result<Manifest> {
    let magic = c.take(8, "magic")?;
    if magic != MAGIC {
        return aerr("bad magic: not a .nnpkg file (or unsupported version)");
    }
    let mlen = c.u32("manifest length")? as usize;
    if mlen > MAX_MANIFEST {
        return aerr(format!("manifest length {mlen} exceeds the 64 KiB cap"));
    }
    let mb = c.take(mlen, "manifest")?;
    let text = std::str::from_utf8(mb).map_err(|_| PkgError::Archive("manifest is not valid UTF-8".into()))?;
    Manifest::parse(text)
}

/// Read only the manifest (no CRC checks); used for indexing.
pub fn parse_manifest_only(bytes: &[u8]) -> Result<Manifest> {
    read_manifest_part(&mut Cur { b: bytes, pos: 0 })
}

/// Parse and fully validate an archive held in memory.
pub fn parse_archive(bytes: &[u8]) -> Result<Archive> {
    let mut c = Cur { b: bytes, pos: 0 };
    let manifest = read_manifest_part(&mut c)?;
    let count = c.u32("file count")? as usize;
    if count > MAX_FILES {
        return aerr(format!("file count {count} exceeds the cap of {MAX_FILES}"));
    }
    let mut files: Vec<(String, Vec<u8>)> = Vec::with_capacity(count.min(1024));
    let mut seen = std::collections::BTreeSet::new();
    let mut total: u64 = 0;
    for i in 0..count {
        let plen = c.u16("path length")? as usize;
        let pb = c.take(plen, "path")?;
        let path = std::str::from_utf8(pb)
            .map_err(|_| PkgError::Archive(format!("file #{i}: path is not valid UTF-8")))?
            .to_string();
        if let Err(w) = validate_path(&path) {
            return aerr(format!("unsafe path `{}` rejected: {w}", path.escape_debug()));
        }
        if !seen.insert(path.to_ascii_lowercase()) {
            return aerr(format!("duplicate path `{path}`"));
        }
        let method = c.u8("method")?;
        let raw_len = c.u32("raw length")? as u64;
        let comp_len = c.u32("compressed length")? as usize;
        let crc = c.u32("crc")?;
        total += raw_len;
        if total > MAX_TOTAL_BYTES {
            return aerr("archive exceeds the 256 MiB uncompressed size cap");
        }
        let body = c.take(comp_len, &format!("data of `{path}`"))?;
        let data = match method {
            0 => {
                if comp_len as u64 != raw_len {
                    return aerr(format!("`{path}`: stored size mismatch ({comp_len} vs {raw_len})"));
                }
                body.to_vec()
            }
            1 => {
                let d = miniz_oxide::inflate::decompress_to_vec_with_limit(body, raw_len as usize)
                    .map_err(|e| PkgError::Archive(format!("`{path}`: deflate error: {:?}", e.status)))?;
                if d.len() as u64 != raw_len {
                    return aerr(format!("`{path}`: decompressed size {} != declared {raw_len}", d.len()));
                }
                d
            }
            m => return aerr(format!("`{path}`: unknown compression method {m}")),
        };
        let actual = crc32(&data);
        if actual != crc {
            return aerr(format!(
                "CRC mismatch in `{path}`: expected {crc:08x}, got {actual:08x}"
            ));
        }
        files.push((path, data));
    }
    let body_end = c.pos;
    let trailer = c.u32("trailer checksum")?;
    if c.pos != bytes.len() {
        return aerr(format!("{} unexpected trailing bytes", bytes.len() - c.pos));
    }
    let actual = crc32(&bytes[..body_end]);
    if actual != trailer {
        return aerr(format!("archive checksum mismatch: expected {trailer:08x}, got {actual:08x}"));
    }
    Ok(Archive { manifest, files })
}

pub fn read_archive(path: &Path) -> Result<Archive> {
    let bytes = fs::read(path).map_err(|e| PkgError::io(path, e))?;
    parse_archive(&bytes)
}

/// Extract into `dest` (created if needed). Paths are re-validated.
pub fn extract(archive: &Archive, dest: &Path) -> Result<()> {
    fs::create_dir_all(dest).map_err(|e| PkgError::io(dest, e))?;
    let mut has_manifest = false;
    for (rel, data) in &archive.files {
        if let Err(w) = validate_path(rel) {
            return aerr(format!("unsafe path `{rel}`: {w}"));
        }
        if rel == MANIFEST_FILE {
            has_manifest = true;
        }
        let mut target = dest.to_path_buf();
        for comp in rel.split('/') {
            target.push(comp);
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| PkgError::io(parent, e))?;
        }
        fs::write(&target, data).map_err(|e| PkgError::io(&target, e))?;
    }
    if !has_manifest {
        let t = dest.join(MANIFEST_FILE);
        fs::write(&t, archive.manifest.to_toml()).map_err(|e| PkgError::io(&t, e))?;
    }
    Ok(())
}

fn collect(dir: &Path, rel: &str, out: &mut Vec<(String, PathBuf)>) -> Result<()> {
    let rd = fs::read_dir(dir).map_err(|e| PkgError::io(dir, e))?;
    for ent in rd {
        let ent = ent.map_err(|e| PkgError::io(dir, e))?;
        let name = ent
            .file_name()
            .into_string()
            .map_err(|n| PkgError::Archive(format!("non-UTF-8 file name {n:?} in {}", dir.display())))?;
        let ft = ent.file_type().map_err(|e| PkgError::io(&ent.path(), e))?;
        if ft.is_symlink() {
            continue;
        }
        let r = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
        if ft.is_dir() {
            if name.starts_with('.') || name == "target" {
                continue;
            }
            collect(&ent.path(), &r, out)?;
        } else if ft.is_file() {
            if name.ends_with(".nnpkg") {
                continue;
            }
            out.push((r, ent.path()));
        }
    }
    Ok(())
}

/// Pack `src_dir` (needs `nin-package.toml`) into a `.nnpkg`.
/// If `out` is an existing directory the file is named `name-version.nnpkg`.
pub fn pack(src_dir: &Path, out: &Path) -> Result<PathBuf> {
    pack_with(src_dir, out, Compression::Auto)
}

pub fn pack_with(src_dir: &Path, out: &Path, comp: Compression) -> Result<PathBuf> {
    let mpath = src_dir.join(MANIFEST_FILE);
    let text = fs::read_to_string(&mpath).map_err(|e| PkgError::io(&mpath, e))?;
    let manifest = Manifest::parse(&text)?;
    let mut list = Vec::new();
    collect(src_dir, "", &mut list)?;
    list.sort_by(|a, b| a.0.cmp(&b.0));
    let mut files = Vec::with_capacity(list.len());
    for (rel, p) in list {
        files.push((rel, fs::read(&p).map_err(|e| PkgError::io(&p, e))?));
    }
    if let Some(e) = &manifest.entry {
        if !files.iter().any(|(p, _)| p == e) {
            return Err(PkgError::Manifest(format!("entry file `{e}` does not exist in the package")));
        }
    }
    let bytes = encode(&text, &files, comp, true)?;
    let dest = if out.is_dir() {
        out.join(format!("{}-{}.nnpkg", manifest.name, manifest.version))
    } else {
        out.to_path_buf()
    };
    if let Some(parent) = dest.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|e| PkgError::io(parent, e))?;
        }
    }
    fs::write(&dest, bytes).map_err(|e| PkgError::io(&dest, e))?;
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn man() -> Manifest {
        Manifest::parse("[package]\nname=\"t\"\nversion=\"1.0.0\"").unwrap()
    }
    fn sample() -> Vec<(String, Vec<u8>)> {
        vec![
            ("a.txt".into(), b"hello hello hello hello hello hello".to_vec()),
            ("dir/b.bin".into(), (0..=255u8).collect()),
            ("empty".into(), vec![]),
        ]
    }

    #[test]
    fn roundtrip_all_modes() {
        for comp in [Compression::Auto, Compression::Stored, Compression::Deflate] {
            let bytes = write_archive(&man(), &sample(), comp).unwrap();
            let a = parse_archive(&bytes).unwrap();
            assert_eq!(a.files, sample());
            assert_eq!(a.manifest, man());
        }
    }

    #[test]
    fn deflate_is_smaller_than_stored_for_compressible() {
        let files = vec![("x".to_string(), vec![b'a'; 10_000])];
        let s = write_archive(&man(), &files, Compression::Stored).unwrap();
        let d = write_archive(&man(), &files, Compression::Deflate).unwrap();
        assert!(d.len() < s.len() / 5);
        assert_eq!(parse_archive(&d).unwrap().files, files);
        // Auto falls back to stored for incompressible data
        let tiny = vec![("x".to_string(), vec![7u8])];
        let auto = write_archive(&man(), &tiny, Compression::Auto).unwrap();
        let st = write_archive(&man(), &tiny, Compression::Stored).unwrap();
        assert_eq!(auto, st);
    }

    #[test]
    fn every_truncation_errors_without_panic() {
        let bytes = write_archive(&man(), &sample(), Compression::Auto).unwrap();
        for n in 0..bytes.len() {
            assert!(parse_archive(&bytes[..n]).is_err(), "truncated to {n} accepted");
        }
    }

    #[test]
    fn every_byte_flip_detected() {
        let bytes = write_archive(&man(), &sample(), Compression::Deflate).unwrap();
        for i in 0..bytes.len() {
            let mut b = bytes.clone();
            b[i] ^= 0x55;
            assert!(parse_archive(&b).is_err(), "flip at {i} accepted");
        }
    }

    #[test]
    fn trailing_garbage_and_bad_magic() {
        let mut bytes = write_archive(&man(), &sample(), Compression::Auto).unwrap();
        let mut g = bytes.clone();
        g.push(0);
        assert!(parse_archive(&g).is_err());
        bytes[0] = b'X';
        let e = parse_archive(&bytes).unwrap_err();
        assert!(e.to_string().contains("magic"));
    }

    #[test]
    fn crc_mismatch_message_names_file() {
        let mut bytes = write_archive(&man(), &[("f.txt".into(), b"data".to_vec())], Compression::Stored).unwrap();
        let pos = bytes.windows(4).position(|w| w == b"data").unwrap();
        bytes[pos] = b'D';
        let e = parse_archive(&bytes).unwrap_err().to_string();
        assert!(e.contains("CRC mismatch") && e.contains("f.txt"), "{e}");
    }

    #[test]
    fn zip_slip_rejected_on_read() {
        let bad = [
            "../evil.txt", "a/../../evil", "/abs", "C:/win", "a\\b", "a//b", "./x", "a/./b",
            "nul", "dir/COM1.txt", "x:y", "trail.", "a\0b",
        ];
        for p in bad {
            let bytes = write_archive_unchecked(
                &man().to_toml(),
                &[(p.to_string(), b"x".to_vec())],
                Compression::Stored,
            );
            let e = parse_archive(&bytes).unwrap_err();
            assert!(matches!(e, PkgError::Archive(_)), "{p}");
        }
        // and the safe writer refuses them too
        assert!(write_archive(&man(), &[("../x".into(), vec![])], Compression::Stored).is_err());
    }

    #[test]
    fn duplicate_paths_rejected() {
        let bytes = write_archive_unchecked(
            &man().to_toml(),
            &[("a".into(), vec![1]), ("A".into(), vec![2])],
            Compression::Stored,
        );
        assert!(parse_archive(&bytes).is_err());
    }

    #[test]
    fn bad_manifest_in_archive() {
        let bytes = write_archive_unchecked("not toml [", &[], Compression::Stored);
        assert!(matches!(parse_archive(&bytes), Err(PkgError::Manifest(_))));
    }

    #[test]
    fn oversized_declared_length_is_bounded() {
        // forge: huge raw_len with tiny deflate stream must not allocate/succeed
        let mut b = Vec::new();
        b.extend_from_slice(MAGIC);
        let m = man().to_toml();
        b.extend_from_slice(&(m.len() as u32).to_le_bytes());
        b.extend_from_slice(m.as_bytes());
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.push(b'f');
        b.push(1);
        b.extend_from_slice(&u32::MAX.to_le_bytes());
        b.extend_from_slice(&2u32.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(&[0, 0]);
        assert!(parse_archive(&b).is_err());
    }

    #[test]
    fn pack_extract_roundtrip() {
        let t = TempDir::new("pack");
        let src = t.path().join("src");
        fs::create_dir_all(src.join("sub")).unwrap();
        fs::create_dir_all(src.join(".git")).unwrap();
        fs::create_dir_all(src.join("target")).unwrap();
        fs::write(src.join("nin-package.toml"), "[package]\nname=\"pk\"\nversion=\"0.3.0\"\nentry=\"main.nin\"\n").unwrap();
        fs::write(src.join("main.nin"), "x").unwrap();
        fs::write(src.join("sub/y.nin"), "y").unwrap();
        fs::write(src.join(".git/config"), "g").unwrap();
        fs::write(src.join("target/o"), "g").unwrap();
        fs::write(src.join("old.nnpkg"), "g").unwrap();
        let out = t.path().join("out");
        fs::create_dir_all(&out).unwrap();
        let p = pack(&src, &out).unwrap();
        assert_eq!(p.file_name().unwrap(), "pk-0.3.0.nnpkg");
        let a = read_archive(&p).unwrap();
        let names: Vec<_> = a.files.iter().map(|f| f.0.as_str()).collect();
        assert_eq!(names, vec!["main.nin", "nin-package.toml", "sub/y.nin"]);
        // deterministic
        let p2 = pack(&src, &t.path().join("again.nnpkg")).unwrap();
        assert_eq!(fs::read(&p).unwrap(), fs::read(&p2).unwrap());
        let dest = t.path().join("ex");
        extract(&a, &dest).unwrap();
        assert_eq!(fs::read_to_string(dest.join("sub/y.nin")).unwrap(), "y");
        assert!(!dest.join(".git").exists());
    }

    #[test]
    fn pack_requires_manifest_and_entry() {
        let t = TempDir::new("pack2");
        assert!(matches!(pack(t.path(), &t.path().join("o.nnpkg")), Err(PkgError::Io { .. })));
        fs::write(t.path().join("nin-package.toml"), "[package]\nname=\"pk\"\nversion=\"0.3.0\"\nentry=\"m.nin\"\n").unwrap();
        assert!(matches!(pack(t.path(), &t.path().join("o.nnpkg")), Err(PkgError::Manifest(_))));
    }
}
