//! Registries: local directories of `*.nnpkg` (with an optional `index.toml`
//! cache) and plain `http://` servers serving the same layout.
//!
//! Layout of a registry (directory or URL base):
//!
//! ```text
//! index.toml                 # [[package]] name, version, description, file, size, [package.dependencies]
//! <name>-<version>.nnpkg
//! ```
//!
//! For directories the index is only a cache: entries whose file is missing or
//! whose size changed are ignored/re-read, and files not in the index are
//! indexed on the fly by reading their manifest. HTTP registries must serve
//! `index.toml`. HTTPS is not supported (no TLS stack): a clear error is returned.

use crate::archive::{parse_archive, parse_manifest_only};
use crate::error::{PkgError, Result};
use crate::manifest::is_valid_name;
use semver::{Version, VersionReq};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const INDEX_FILE: &str = "index.toml";
const MAX_HTTP_BODY: u64 = 300 * 1024 * 1024;
const MAX_INDEX_BYTES: u64 = 32 * 1024 * 1024;

/// Where the bytes of a candidate package come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Already installed on disk.
    Installed,
    /// A `.nnpkg` inside a local registry directory.
    Local(PathBuf),
    /// A `.nnpkg` served by an `http://` registry (full URL).
    Http(String),
    /// A `.nnpkg` file given explicitly by the user.
    Archive(PathBuf),
}

impl Source {
    /// Read the archive bytes.
    pub fn fetch(&self) -> Result<Vec<u8>> {
        match self {
            Source::Installed => Err(PkgError::Registry("package is already installed; nothing to fetch".into())),
            Source::Local(p) | Source::Archive(p) => fs::read(p).map_err(|e| PkgError::io(p, e)),
            Source::Http(u) => http_get(u, MAX_HTTP_BODY),
        }
    }
}

/// One published package version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexEntry {
    pub name: String,
    pub version: Version,
    pub description: String,
    pub dependencies: BTreeMap<String, VersionReq>,
    /// File name inside the registry.
    pub file: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Registry {
    Dir(PathBuf),
    Http { base: String },
    Https(String),
}

fn rerr<T>(m: impl Into<String>) -> Result<T> {
    Err(PkgError::Registry(m.into()))
}

fn safe_file_name(f: &str) -> bool {
    !f.is_empty()
        && f.len() <= 200
        && f.ends_with(".nnpkg")
        && !f.starts_with('.')
        && f.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-' | b'+'))
}

impl Registry {
    pub fn parse(s: &str) -> Registry {
        let lower = s.to_ascii_lowercase();
        if lower.starts_with("http://") {
            Registry::Http { base: s.trim_end_matches('/').to_string() }
        } else if lower.starts_with("https://") {
            Registry::Https(s.to_string())
        } else {
            Registry::Dir(PathBuf::from(s))
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Registry::Dir(p) => p.display().to_string(),
            Registry::Http { base } => base.clone(),
            Registry::Https(u) => u.clone(),
        }
    }

    /// All published versions with the source to fetch each from.
    pub fn entries(&self) -> Result<Vec<(IndexEntry, Source)>> {
        match self {
            Registry::Dir(d) => Ok(scan_dir(d)?
                .into_iter()
                .map(|e| {
                    let s = Source::Local(d.join(&e.file));
                    (e, s)
                })
                .collect()),
            Registry::Http { base } => {
                let url = format!("{base}/{INDEX_FILE}");
                let body = http_get(&url, MAX_INDEX_BYTES)?;
                let text = String::from_utf8(body)
                    .map_err(|_| PkgError::Registry(format!("{url}: index is not valid UTF-8")))?;
                let list = parse_index(&text).map_err(|e| match e {
                    PkgError::Registry(m) => PkgError::Registry(format!("{url}: {m}")),
                    o => o,
                })?;
                Ok(list
                    .into_iter()
                    .map(|e| {
                        let s = Source::Http(format!("{base}/{}", e.file));
                        (e, s)
                    })
                    .collect())
            }
            Registry::Https(u) => rerr(format!(
                "https registries are not supported (no TLS in nin-pkg): `{u}`; \
                 use a local directory or a plain http:// registry"
            )),
        }
    }
}

// ---------------------------------------------------------------- index.toml

pub fn render_index(entries: &[IndexEntry]) -> String {
    let mut arr = Vec::new();
    for e in entries {
        let mut t = toml::Table::new();
        t.insert("name".into(), e.name.clone().into());
        t.insert("version".into(), e.version.to_string().into());
        t.insert("description".into(), e.description.clone().into());
        t.insert("file".into(), e.file.clone().into());
        t.insert("size".into(), toml::Value::Integer(e.size.min(i64::MAX as u64) as i64));
        let mut deps = toml::Table::new();
        for (k, v) in &e.dependencies {
            deps.insert(k.clone(), v.to_string().into());
        }
        t.insert("dependencies".into(), toml::Value::Table(deps));
        arr.push(toml::Value::Table(t));
    }
    let mut doc = toml::Table::new();
    doc.insert("package".into(), toml::Value::Array(arr));
    toml::to_string(&doc).unwrap_or_default()
}

pub fn parse_index(text: &str) -> Result<Vec<IndexEntry>> {
    let doc: toml::Table = text
        .parse()
        .map_err(|e: toml::de::Error| PkgError::Registry(format!("bad index.toml: {e}")))?;
    let arr = match doc.get("package") {
        None => return Ok(Vec::new()),
        Some(toml::Value::Array(a)) => a,
        Some(_) => return rerr("index.toml: `package` must be an array of tables"),
    };
    let mut out = Vec::new();
    for v in arr {
        let t = match v {
            toml::Value::Table(t) => t,
            _ => return rerr("index.toml: package entry must be a table"),
        };
        let s = |k: &str| -> Result<String> {
            match t.get(k) {
                Some(toml::Value::String(s)) => Ok(s.clone()),
                _ => Err(PkgError::Registry(format!("index.toml: entry missing string `{k}`"))),
            }
        };
        let name = s("name")?;
        if !is_valid_name(&name) {
            return rerr(format!("index.toml: invalid package name `{name}`"));
        }
        let version = Version::parse(&s("version")?)
            .map_err(|e| PkgError::Registry(format!("index.toml: bad version for `{name}`: {e}")))?;
        let file = s("file")?;
        if !safe_file_name(&file) {
            return rerr(format!("index.toml: unsafe file name `{file}`"));
        }
        let description = match t.get("description") {
            Some(toml::Value::String(d)) => d.clone(),
            _ => String::new(),
        };
        let size = match t.get("size") {
            Some(toml::Value::Integer(i)) if *i >= 0 => *i as u64,
            _ => 0,
        };
        let mut dependencies = BTreeMap::new();
        if let Some(toml::Value::Table(d)) = t.get("dependencies") {
            for (k, rv) in d {
                let rs = match rv {
                    toml::Value::String(s) => s,
                    _ => return rerr(format!("index.toml: dependency `{k}` of `{name}` must be a string")),
                };
                let req = VersionReq::parse(rs)
                    .map_err(|e| PkgError::Registry(format!("index.toml: bad requirement `{rs}`: {e}")))?;
                dependencies.insert(k.clone(), req);
            }
        }
        out.push(IndexEntry { name, version, description, dependencies, file, size });
    }
    Ok(out)
}

// ------------------------------------------------------------- local dirs

/// List a registry directory (uses `index.toml` as a size-validated cache).
pub fn scan_dir(dir: &Path) -> Result<Vec<IndexEntry>> {
    if !dir.is_dir() {
        return rerr(format!("registry directory `{}` does not exist", dir.display()));
    }
    let mut cached: HashMap<String, IndexEntry> = HashMap::new();
    if let Ok(text) = fs::read_to_string(dir.join(INDEX_FILE)) {
        if let Ok(list) = parse_index(&text) {
            for e in list {
                cached.insert(e.file.clone(), e);
            }
        }
    }
    let mut out = Vec::new();
    let rd = fs::read_dir(dir).map_err(|e| PkgError::io(dir, e))?;
    for ent in rd {
        let ent = ent.map_err(|e| PkgError::io(dir, e))?;
        let fname = match ent.file_name().into_string() {
            Ok(f) => f,
            Err(_) => continue,
        };
        if !fname.ends_with(".nnpkg") || !safe_file_name(&fname) {
            continue;
        }
        let md = match ent.metadata() {
            Ok(m) if m.is_file() => m,
            _ => continue,
        };
        if let Some(c) = cached.get(&fname) {
            if c.size == md.len() {
                out.push(c.clone());
                continue;
            }
        }
        let path = ent.path();
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        // A corrupt file must not break the whole registry: skip it.
        if let Ok(m) = parse_manifest_only(&bytes) {
            out.push(IndexEntry {
                name: m.name,
                version: m.version,
                description: m.description,
                dependencies: m.dependencies,
                file: fname,
                size: md.len(),
            });
        }
    }
    out.sort_by(|a, b| (&a.name, &a.version, &a.file).cmp(&(&b.name, &b.version, &b.file)));
    Ok(out)
}

fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".tmp{}", std::process::id()));
    let tmp = PathBuf::from(tmp);
    fs::write(&tmp, data).map_err(|e| PkgError::io(&tmp, e))?;
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(PkgError::io(path, e));
    }
    Ok(())
}

/// Atomic file write helper shared with the manager.
pub(crate) fn write_file_atomic(path: &Path, data: &[u8]) -> Result<()> {
    write_atomic(path, data)
}

/// Validate `nnpkg` and add it to the registry directory, refreshing `index.toml`.
/// Re-publishing byte-identical content is a no-op; different content under an
/// existing name+version is refused.
pub fn publish(nnpkg: &Path, registry_dir: &Path) -> Result<()> {
    let bytes = fs::read(nnpkg).map_err(|e| PkgError::io(nnpkg, e))?;
    let archive = parse_archive(&bytes)?;
    let m = &archive.manifest;
    fs::create_dir_all(registry_dir).map_err(|e| PkgError::io(registry_dir, e))?;
    let fname = format!("{}-{}.nnpkg", m.name, m.version);
    if !safe_file_name(&fname) {
        return rerr(format!("cannot publish: unsafe file name `{fname}`"));
    }
    let dest = registry_dir.join(&fname);
    for e in scan_dir(registry_dir)? {
        if e.name == m.name && e.version == m.version {
            let existing = fs::read(registry_dir.join(&e.file)).map_err(|er| PkgError::io(&dest, er))?;
            if existing == bytes {
                return refresh_index(registry_dir);
            }
            return Err(PkgError::AlreadyPublished(format!("{} {}", m.name, m.version)));
        }
    }
    write_atomic(&dest, &bytes)?;
    refresh_index(registry_dir)
}

/// Rewrite `index.toml` from the directory contents.
pub fn refresh_index(dir: &Path) -> Result<()> {
    let entries = scan_dir(dir)?;
    write_atomic(&dir.join(INDEX_FILE), render_index(&entries).as_bytes())
}

// -------------------------------------------------------------------- http

struct Url {
    host: String,
    port: u16,
    path: String,
    host_header: String,
}

fn parse_http_url(url: &str) -> Result<Url> {
    let rest = match url.get(..7) {
        Some(p) if p.eq_ignore_ascii_case("http://") => &url[7..],
        _ => return rerr(format!("unsupported URL `{url}` (only http:// is supported)")),
    };
    let (hp, path) = match rest.find('/') {
        Some(i) => (&rest[..i], rest[i..].to_string()),
        None => (rest, "/".to_string()),
    };
    if hp.is_empty() || path.bytes().any(|c| c <= b' ' || c == 0x7f) {
        return rerr(format!("malformed URL `{url}`"));
    }
    let (host, port) = if let Some(stripped) = hp.strip_prefix('[') {
        let end = match stripped.find(']') {
            Some(e) => e,
            None => return rerr(format!("malformed URL `{url}`")),
        };
        let after = &stripped[end + 1..];
        let port = match after.strip_prefix(':') {
            Some(p) => p.parse::<u16>().map_err(|_| PkgError::Registry(format!("bad port in `{url}`")))?,
            None if after.is_empty() => 80,
            None => return rerr(format!("malformed URL `{url}`")),
        };
        (stripped[..end].to_string(), port)
    } else if let Some((h, p)) = hp.rsplit_once(':') {
        (h.to_string(), p.parse::<u16>().map_err(|_| PkgError::Registry(format!("bad port in `{url}`")))?)
    } else {
        (hp.to_string(), 80)
    };
    if host.is_empty() {
        return rerr(format!("malformed URL `{url}`"));
    }
    Ok(Url { host, port, path, host_header: hp.to_string() })
}

/// Minimal HTTP/1.1 GET (no redirects, no TLS); decodes chunked bodies.
pub fn http_get(url: &str, max_body: u64) -> Result<Vec<u8>> {
    let u = parse_http_url(url)?;
    let addrs = (u.host.as_str(), u.port)
        .to_socket_addrs()
        .map_err(|e| PkgError::Registry(format!("cannot resolve `{}`: {e}", u.host)))?;
    let mut last = None;
    let mut stream = None;
    for a in addrs {
        match TcpStream::connect_timeout(&a, Duration::from_secs(10)) {
            Ok(s) => {
                stream = Some(s);
                break;
            }
            Err(e) => last = Some(e),
        }
    }
    let mut stream = match stream {
        Some(s) => s,
        None => {
            return rerr(format!(
                "cannot connect to {}:{}: {}",
                u.host,
                u.port,
                last.map(|e| e.to_string()).unwrap_or_else(|| "no address".into())
            ))
        }
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));
    let req = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: nin-pkg\r\nAccept: */*\r\nConnection: close\r\n\r\n",
        u.path, u.host_header
    );
    stream
        .write_all(req.as_bytes())
        .map_err(|e| PkgError::Registry(format!("{url}: write failed: {e}")))?;
    let mut raw = Vec::new();
    stream
        .take(max_body + 64 * 1024)
        .read_to_end(&mut raw)
        .map_err(|e| PkgError::Registry(format!("{url}: read failed: {e}")))?;
    parse_http_response(url, &raw, max_body)
}

fn parse_http_response(url: &str, raw: &[u8], max_body: u64) -> Result<Vec<u8>> {
    let sep = match raw.windows(4).position(|w| w == b"\r\n\r\n") {
        Some(p) => p,
        None => return rerr(format!("{url}: malformed or truncated HTTP response")),
    };
    let head = String::from_utf8_lossy(&raw[..sep]).to_string();
    let body = &raw[sep + 4..];
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or("");
    let mut parts = status_line.split_whitespace();
    let proto = parts.next().unwrap_or("");
    if !proto.starts_with("HTTP/1.") {
        return rerr(format!("{url}: not an HTTP/1.x response"));
    }
    let code: u16 = parts.next().and_then(|c| c.parse().ok()).unwrap_or(0);
    if (300..400).contains(&code) {
        return rerr(format!("{url}: HTTP {code} redirect (redirects are not followed)"));
    }
    if code != 200 {
        return rerr(format!("{url}: HTTP status {code}"));
    }
    let mut chunked = false;
    let mut clen: Option<u64> = None;
    for l in lines {
        if let Some((k, v)) = l.split_once(':') {
            let k = k.trim().to_ascii_lowercase();
            let v = v.trim();
            if k == "transfer-encoding" && v.to_ascii_lowercase().contains("chunked") {
                chunked = true;
            } else if k == "content-length" {
                clen = v.parse().ok();
            }
        }
    }
    let out = if chunked {
        dechunk(url, body, max_body)?
    } else {
        if let Some(n) = clen {
            if (body.len() as u64) < n {
                return rerr(format!("{url}: truncated body ({} of {n} bytes)", body.len()));
            }
            body[..n as usize].to_vec()
        } else {
            body.to_vec()
        }
    };
    if out.len() as u64 > max_body {
        return rerr(format!("{url}: response exceeds the {max_body} byte limit"));
    }
    Ok(out)
}

fn dechunk(url: &str, mut b: &[u8], max_body: u64) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let nl = match b.windows(2).position(|w| w == b"\r\n") {
            Some(p) => p,
            None => return rerr(format!("{url}: truncated chunked body")),
        };
        let line = String::from_utf8_lossy(&b[..nl]).to_string();
        let size_str = line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_str, 16)
            .map_err(|_| PkgError::Registry(format!("{url}: bad chunk size `{size_str}`")))?;
        b = &b[nl + 2..];
        if size == 0 {
            return Ok(out);
        }
        if b.len() < size + 2 {
            return rerr(format!("{url}: truncated chunk"));
        }
        out.extend_from_slice(&b[..size]);
        if out.len() as u64 > max_body {
            return rerr(format!("{url}: response exceeds the {max_body} byte limit"));
        }
        b = &b[size + 2..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::{write_archive, Compression};
    use crate::manifest::Manifest;
    use crate::testutil::TempDir;
    use std::net::TcpListener;

    fn pkg_bytes(name: &str, ver: &str, deps: &str) -> Vec<u8> {
        let m = Manifest::parse(&format!(
            "[package]\nname=\"{name}\"\nversion=\"{ver}\"\ndescription=\"d {name}\"\n[dependencies]\n{deps}"
        ))
        .unwrap();
        write_archive(&m, &[("a.txt".into(), b"hi".to_vec())], Compression::Auto).unwrap()
    }

    #[test]
    fn parse_registry_kinds() {
        assert!(matches!(Registry::parse("http://x:1/r/"), Registry::Http { base } if base == "http://x:1/r"));
        assert!(matches!(Registry::parse("HTTPS://x"), Registry::Https(_)));
        assert!(matches!(Registry::parse("/tmp/x"), Registry::Dir(_)));
        let e = Registry::parse("https://x").entries().unwrap_err().to_string();
        assert!(e.contains("not supported"), "{e}");
    }

    #[test]
    fn index_roundtrip_and_rejects_unsafe_file() {
        let e = IndexEntry {
            name: "a".into(),
            version: Version::new(1, 0, 0),
            description: "x \"q\"".into(),
            dependencies: [("b".to_string(), VersionReq::parse("^1").unwrap())].into(),
            file: "a-1.0.0.nnpkg".into(),
            size: 5,
        };
        assert_eq!(parse_index(&render_index(&[e.clone()])).unwrap(), vec![e]);
        let bad = "[[package]]\nname=\"a\"\nversion=\"1.0.0\"\nfile=\"../x.nnpkg\"";
        assert!(parse_index(bad).is_err());
        assert!(parse_index("[[package]\n").is_err());
    }

    #[test]
    fn publish_scan_and_stale_index() {
        let t = TempDir::new("reg");
        let src = t.path().join("p.nnpkg");
        fs::write(&src, pkg_bytes("foo", "1.0.0", "")).unwrap();
        let reg = t.path().join("reg");
        publish(&src, &reg).unwrap();
        publish(&src, &reg).unwrap(); // idempotent
        assert!(reg.join(INDEX_FILE).exists());
        // different content, same version
        fs::write(&src, pkg_bytes("foo", "1.0.0", "bar=\"1\"")).unwrap();
        assert!(matches!(publish(&src, &reg), Err(PkgError::AlreadyPublished(_))));
        // drop an unindexed file in; it is picked up despite a stale index
        fs::write(reg.join("foo-2.0.0.nnpkg"), pkg_bytes("foo", "2.0.0", "")).unwrap();
        fs::write(reg.join("junk.nnpkg"), b"garbage").unwrap();
        let v: Vec<_> = scan_dir(&reg).unwrap().into_iter().map(|e| e.version.to_string()).collect();
        assert_eq!(v, vec!["1.0.0", "2.0.0"]);
        // delete a file: index entry for it vanishes
        fs::remove_file(reg.join("foo-1.0.0.nnpkg")).unwrap();
        assert_eq!(scan_dir(&reg).unwrap().len(), 1);
        assert!(scan_dir(&t.path().join("nope")).is_err());
    }

    #[test]
    fn corrupt_index_is_ignored() {
        let t = TempDir::new("badidx");
        fs::write(t.path().join("foo-1.0.0.nnpkg"), pkg_bytes("foo", "1.0.0", "")).unwrap();
        fs::write(t.path().join(INDEX_FILE), "][ not toml").unwrap();
        assert_eq!(scan_dir(t.path()).unwrap().len(), 1);
    }

    #[test]
    fn http_response_parsing() {
        let ok = b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nabcEXTRA";
        assert_eq!(parse_http_response("u", ok, 100).unwrap(), b"abc");
        let ch = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2;x=y\r\nde\r\n0\r\n\r\n";
        assert_eq!(parse_http_response("u", ch, 100).unwrap(), b"abcde");
        assert!(parse_http_response("u", b"HTTP/1.1 404 Not Found\r\n\r\n", 100).is_err());
        assert!(parse_http_response("u", b"HTTP/1.1 301 Moved\r\nLocation: x\r\n\r\n", 100).is_err());
        assert!(parse_http_response("u", b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\nabc", 100).is_err());
        assert!(parse_http_response("u", b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nab", 100).is_err());
        assert!(parse_http_response("u", b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n", 100).is_err());
        assert!(parse_http_response("u", b"junk", 100).is_err());
        assert!(parse_http_response("u", ok, 2).is_err());
        assert!(parse_http_url("ftp://x").is_err());
        assert!(parse_http_url("http://[::1]:8080/a").is_ok());
        assert!(parse_http_url("http://h:99999/").is_err());
    }

    /// Tiny single-purpose server: serves files of `dir` (chunked for `.nnpkg`).
    fn serve(dir: PathBuf, requests: usize) -> (String, std::thread::JoinHandle<()>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let h = std::thread::spawn(move || {
            for _ in 0..requests {
                let (mut s, _) = l.accept().unwrap();
                let mut buf = [0u8; 4096];
                let n = s.read(&mut buf).unwrap();
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = req.split_whitespace().nth(1).unwrap_or("/").trim_start_matches('/').to_string();
                match fs::read(dir.join(&path)) {
                    Ok(data) if path.ends_with(".nnpkg") => {
                        let mut out = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n".to_vec();
                        for c in data.chunks(7) {
                            out.extend_from_slice(format!("{:x}\r\n", c.len()).as_bytes());
                            out.extend_from_slice(c);
                            out.extend_from_slice(b"\r\n");
                        }
                        out.extend_from_slice(b"0\r\n\r\n");
                        s.write_all(&out).unwrap();
                    }
                    Ok(data) => {
                        let mut out = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", data.len()).into_bytes();
                        out.extend_from_slice(&data);
                        s.write_all(&out).unwrap();
                    }
                    Err(_) => s.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n").unwrap(),
                }
            }
        });
        (format!("http://127.0.0.1:{}", addr.port()), h)
    }

    #[test]
    fn http_registry_end_to_end() {
        let t = TempDir::new("http");
        let src = t.path().join("p.nnpkg");
        let bytes = pkg_bytes("web", "1.2.3", "");
        fs::write(&src, &bytes).unwrap();
        publish(&src, t.path()).unwrap();
        let (base, h) = serve(t.path().to_path_buf(), 2);
        let reg = Registry::parse(&format!("{base}/"));
        let e = reg.entries().unwrap();
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].0.name, "web");
        assert_eq!(e[0].1.fetch().unwrap(), bytes);
        h.join().unwrap();
    }

    #[test]
    fn http_errors() {
        let t = TempDir::new("http404");
        let (base, h) = serve(t.path().to_path_buf(), 1);
        let e = Registry::parse(&base).entries().unwrap_err().to_string();
        assert!(e.contains("404"), "{e}");
        h.join().unwrap();
        // connection refused (port freed by closing the listener)
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        assert!(Registry::parse(&format!("http://127.0.0.1:{port}")).entries().is_err());
    }
}
