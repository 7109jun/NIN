//! std.net: TCP probe, DNS, plain-HTTP client, HTTPS through curl.exe.

use crate::util::*;
use indexmap::IndexMap;
use nin_runtime::builtins::{a_int, a_str, value_to_json};
use nin_runtime::*;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs, UdpSocket};
use std::time::Duration;

pub struct Url {
    pub https: bool,
    pub host: String,
    pub port: u16,
    pub path: String,
}

pub fn parse_url(u: &str) -> Result<Url, String> {
    let (https, rest) = if let Some(r) = u.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = u.strip_prefix("http://") {
        (false, r)
    } else {
        return Err(format!("unsupported URL `{}` (use http:// or https://)", u));
    };
    let (hostport, path) = match rest.find(['/', '?']) {
        Some(i) => (&rest[..i], if rest[i..].starts_with('/') { rest[i..].to_string() } else { format!("/{}", &rest[i..]) }),
        None => (rest, "/".to_string()),
    };
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) if !h.ends_with(']') || h.starts_with('[') => (h.trim_matches(['[', ']']).to_string(), p.parse::<u16>().map_err(|_| format!("bad port in `{}`", u))?),
        _ => (hostport.to_string(), if https { 443 } else { 80 }),
    };
    if host.is_empty() {
        return Err(format!("missing host in `{}`", u));
    }
    Ok(Url { https, host, port, path })
}

pub struct Resp {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

fn find(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).position(|w| w == n)
}

fn dechunk(mut b: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = vec![];
    loop {
        let i = find(b, b"\r\n").ok_or("truncated chunked body")?;
        let size = usize::from_str_radix(std::str::from_utf8(&b[..i]).map_err(|_| "bad chunk")?.split(';').next().unwrap().trim(), 16).map_err(|_| "bad chunk size")?;
        b = &b[i + 2..];
        if size == 0 {
            return Ok(out);
        }
        if b.len() < size + 2 {
            return Err("truncated chunk".into());
        }
        out.extend_from_slice(&b[..size]);
        b = &b[size + 2..];
    }
}

/// Parse a raw HTTP response (skips `100 Continue` / proxy blocks).
pub fn parse_response(raw: &[u8]) -> Result<Resp, String> {
    let mut data = raw;
    loop {
        let i = find(data, b"\r\n\r\n").ok_or("malformed HTTP response (no header terminator)")?;
        let head = String::from_utf8_lossy(&data[..i]).to_string();
        let mut lines = head.lines();
        let status_line = lines.next().ok_or("empty response")?;
        let status: u16 = status_line.split_whitespace().nth(1).and_then(|s| s.parse().ok()).ok_or_else(|| format!("bad status line `{}`", status_line))?;
        let headers: Vec<(String, String)> = lines.filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string())).collect();
        let body = &data[i + 4..];
        if status == 100 || (status == 200 && status_line.contains("Connection established")) {
            data = body;
            continue;
        }
        let chunked = headers.iter().any(|(k, v)| k == "transfer-encoding" && v.to_ascii_lowercase().contains("chunked"));
        let body = if chunked { dechunk(body)? } else { body.to_vec() };
        return Ok(Resp { status, headers, body });
    }
}

pub fn http_plain(method: &str, u: &Url, headers: &[(String, String)], body: &[u8], timeout: Duration) -> Result<Resp, String> {
    let addr = (u.host.as_str(), u.port).to_socket_addrs().map_err(|e| format!("cannot resolve {}: {}", u.host, e))?.next().ok_or_else(|| format!("no address for {}", u.host))?;
    let mut s = TcpStream::connect_timeout(&addr, timeout).map_err(|e| format!("cannot connect to {}:{}: {}", u.host, u.port, e))?;
    s.set_read_timeout(Some(timeout)).ok();
    s.set_write_timeout(Some(timeout)).ok();
    let mut req = format!("{} {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nUser-Agent: nin/0.1\r\nAccept: */*\r\n", method, u.path, if u.port == 80 { u.host.clone() } else { format!("{}:{}", u.host, u.port) });
    for (k, v) in headers {
        req.push_str(&format!("{}: {}\r\n", k, v));
    }
    if !body.is_empty() || matches!(method, "POST" | "PUT" | "PATCH") {
        req.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    req.push_str("\r\n");
    let mut out = req.into_bytes();
    out.extend_from_slice(body);
    s.write_all(&out).map_err(|e| format!("send failed: {}", e))?;
    let mut raw = vec![];
    match s.read_to_end(&mut raw) {
        Ok(_) => {}
        Err(e) if !raw.is_empty() && matches!(e.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock) => {}
        Err(e) => return Err(format!("receive failed: {}", e)),
    }
    parse_response(&raw)
}

fn http_curl(ex: &mut Exec, method: &str, url: &str, headers: &[(String, String)], body: &[u8], timeout: Duration) -> R<Resp> {
    let mut args: Vec<String> = vec!["-sS".into(), "-i".into(), "-X".into(), method.into(), "--max-time".into(), format!("{}", timeout.as_secs().max(1))];
    for (k, v) in headers {
        args.push("-H".into());
        args.push(format!("{}: {}", k, v));
    }
    let mut spec = nin_win::ProcSpec::new("curl").args(args);
    if !body.is_empty() {
        spec.args.push("--data-binary".into());
        spec.args.push("@-".into());
        spec.stdin = nin_win::StdinSpec::Bytes(body.to_vec());
    } else {
        spec.stdin = nin_win::StdinSpec::Null;
    }
    spec.args.push(url.to_string());
    spec.stdout = nin_win::OutSpec::Capture;
    spec.stderr = nin_win::OutSpec::Capture;
    let r = ex.run_process(&spec, Some(timeout + Duration::from_secs(5)))?;
    if r.code != 0 {
        return ex.err("IOError", format!("HTTPS request failed (curl exit {}): {}", r.code, r.stderr_text().trim()));
    }
    match parse_response(&r.stdout) {
        Ok(r) => Ok(r),
        Err(e) => ex.err("IOError", format!("HTTPS request failed: {}", e)),
    }
}

fn request(ex: &mut Exec, method: &str, url: &str, opts: &IndexMap<String, Value>) -> R<Resp> {
    let mut headers: Vec<(String, String)> = vec![];
    if let Some(Value::Map(h)) = opts.get("headers") {
        for (k, v) in lk(h).clone() {
            headers.push((k, ex.to_display(&v)?));
        }
    }
    let mut body: Vec<u8> = vec![];
    match opts.get("body") {
        None | Some(Value::Null) => {}
        Some(Value::Str(s)) => body = s.as_bytes().to_vec(),
        Some(v) => {
            match value_to_json(v) {
                Ok(j) => body = j.to_string().into_bytes(),
                Err(e) => return ex.err("ValueError", format!("http body: {}", e)),
            }
            if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("content-type")) {
                headers.push(("Content-Type".into(), "application/json".into()));
            }
        }
    }
    let timeout = match opts.get("timeout") {
        Some(v) if !matches!(v, Value::Null) => Duration::from_millis(ex.duration_ms(v)?),
        _ => Duration::from_secs(30),
    };
    let follow = !matches!(opts.get("follow"), Some(Value::Bool(false)));
    let mut cur = url.to_string();
    let mut method = method.to_string();
    for _ in 0..6 {
        let u = match parse_url(&cur) {
            Ok(u) => u,
            Err(e) => return ex.err("ValueError", e),
        };
        ex.check_cancel()?;
        let resp = if u.https {
            http_curl(ex, &method, &cur, &headers, &body, timeout)?
        } else {
            match http_plain(&method, &u, &headers, &body, timeout) {
                Ok(r) => r,
                Err(e) => return ex.err("IOError", e),
            }
        };
        if follow && matches!(resp.status, 301 | 302 | 303 | 307 | 308) {
            if let Some((_, loc)) = resp.headers.iter().find(|(k, _)| k == "location") {
                cur = if loc.starts_with("http") { loc.clone() } else if loc.starts_with('/') { format!("{}://{}:{}{}", if u.https { "https" } else { "http" }, u.host, u.port, loc) } else { format!("{}/{}", cur.trim_end_matches('/'), loc) };
                if resp.status == 303 {
                    method = "GET".into();
                }
                continue;
            }
        }
        return Ok(resp);
    }
    ex.err("IOError", "too many redirects")
}

fn resp_value(r: &Resp) -> Value {
    let mut h = IndexMap::new();
    for (k, v) in &r.headers {
        h.insert(k.clone(), Value::str(v));
    }
    Value::map_from([("status", Value::Int(r.status as i64)), ("ok", Value::Bool((200..300).contains(&r.status))), ("headers", Value::map(h)), ("body", Value::str(String::from_utf8_lossy(&r.body)))])
}

pub fn net() -> IndexMap<String, Value> {
    let mut m = Mod::new();
    m.f("tcp_connect", 2, Some(3), |ex, a| {
        let host = a_str(ex, a, 0, "tcp_connect")?;
        let port = a_int(ex, a, 1, "tcp_connect")? as u16;
        let ms = match a.get(2) { Some(v) => ex.duration_ms(v)?, None => 3000 };
        let ok = (host.as_str(), port).to_socket_addrs().ok().and_then(|mut it| it.find_map(|ad| TcpStream::connect_timeout(&ad, Duration::from_millis(ms.max(1))).ok())).is_some();
        Ok(Value::Bool(ok))
    });
    m.f("resolve", 1, Some(1), |ex, a| {
        let host = a_str(ex, a, 0, "resolve")?;
        match (host.as_str(), 0u16).to_socket_addrs() {
            Ok(it) => {
                let mut v: Vec<String> = it.map(|s| s.ip().to_string()).collect();
                v.dedup();
                Ok(Value::list(v.into_iter().map(Value::str).collect()))
            }
            Err(e) => io_err(ex, e, &format!("cannot resolve {}", host)),
        }
    });
    m.f("local_ip", 0, Some(0), |_, _| {
        let s = UdpSocket::bind("0.0.0.0:0").ok();
        let ip = s.and_then(|s| s.connect("192.0.2.1:9").ok().map(|_| s)).and_then(|s| s.local_addr().ok()).map(|a| a.ip().to_string());
        Ok(ip.map(Value::str).unwrap_or(Value::Null))
    });
    m.f("http_request", 2, Some(3), |ex, a| {
        let method = a_str(ex, a, 0, "http_request")?.to_ascii_uppercase();
        let url = a_str(ex, a, 1, "http_request")?;
        let opts = opt_map(ex, a, 2, "http_request")?;
        let r = request(ex, &method, &url, &opts)?;
        Ok(resp_value(&r))
    });
    m.f("http_get", 1, Some(2), |ex, a| {
        let url = a_str(ex, a, 0, "http_get")?;
        let opts = opt_map(ex, a, 1, "http_get")?;
        let r = request(ex, "GET", &url, &opts)?;
        Ok(resp_value(&r))
    });
    m.f("http_post", 2, Some(3), |ex, a| {
        let url = a_str(ex, a, 0, "http_post")?;
        let mut opts = opt_map(ex, a, 2, "http_post")?;
        opts.insert("body".into(), a[1].clone());
        let r = request(ex, "POST", &url, &opts)?;
        Ok(resp_value(&r))
    });
    m.f("download", 2, Some(3), |ex, a| {
        let url = a_str(ex, a, 0, "download")?;
        let path = a_str(ex, a, 1, "download")?;
        let opts = opt_map(ex, a, 2, "download")?;
        let r = request(ex, "GET", &url, &opts)?;
        if !(200..300).contains(&r.status) {
            return ex.err("IOError", format!("download failed: HTTP {}", r.status));
        }
        match std::fs::write(&path, &r.body) {
            Ok(_) => Ok(Value::Int(r.body.len() as i64)),
            Err(e) => io_err(ex, e, &format!("cannot write {}", path)),
        }
    });
    m.done()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn urls() {
        let u = parse_url("http://example.com:8080/a/b?x=1").unwrap();
        assert_eq!((u.host.as_str(), u.port, u.path.as_str(), u.https), ("example.com", 8080, "/a/b?x=1", false));
        let u = parse_url("https://h.io?q=1").unwrap();
        assert_eq!((u.port, u.path.as_str()), (443, "/?q=1"));
        assert!(parse_url("ftp://x").is_err());
    }
    #[test]
    fn responses() {
        let r = parse_response(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n").unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(r.body, b"hello world");
        let r = parse_response(b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 404 Not Found\r\nX: y\r\n\r\nnope").unwrap();
        assert_eq!((r.status, r.body.as_slice()), (404, &b"nope"[..]));
        assert!(parse_response(b"garbage").is_err());
    }
}
