//! json, yaml, toml, object, hash, regex, random.

use crate::util::*;
use indexmap::IndexMap;
use nin_runtime::builtins::{a_int, a_list, a_str, json_to_value, value_to_json};
use nin_runtime::*;
use serde_json::Value as J;
use std::sync::atomic::{AtomicU64, Ordering};

fn to_j(ex: &mut Exec, v: &Value, f: &str) -> R<J> {
    match value_to_json(v) {
        Ok(j) => Ok(j),
        Err(e) => ex.err("ValueError", format!("{}: {}", f, e)),
    }
}

// ───────── json ─────────

pub fn json() -> IndexMap<String, Value> {
    let mut m = Mod::new();
    m.f("parse", 1, Some(1), |ex, a| {
        let s = a_str(ex, a, 0, "parse")?;
        match serde_json::from_str::<J>(&s) {
            Ok(j) => Ok(json_to_value(&j)),
            Err(e) => ex.err("ValueError", format!("invalid JSON: {}", e)),
        }
    });
    m.f("stringify", 1, Some(1), |ex, a| {
        let j = to_j(ex, &a[0], "stringify")?;
        Ok(Value::str(serde_json::to_string(&j).unwrap_or_default()))
    });
    m.f("pretty", 1, Some(2), |ex, a| {
        let j = to_j(ex, &a[0], "pretty")?;
        Ok(Value::str(serde_json::to_string_pretty(&j).unwrap_or_default()))
    });
    m.done()
}

// ───────── yaml ─────────

fn yaml_to_j(y: &yaml_rust2::Yaml) -> J {
    use yaml_rust2::Yaml::*;
    match y {
        Real(s) => s.parse::<f64>().ok().and_then(serde_json::Number::from_f64).map(J::Number).unwrap_or_else(|| J::String(s.clone())),
        Integer(i) => J::from(*i),
        String(s) => J::String(s.clone()),
        Boolean(b) => J::Bool(*b),
        Array(a) => J::Array(a.iter().map(yaml_to_j).collect()),
        Hash(h) => {
            let mut m = serde_json::Map::new();
            for (k, v) in h {
                let key = match k {
                    String(s) => s.clone(),
                    Integer(i) => i.to_string(),
                    Boolean(b) => b.to_string(),
                    Real(r) => r.clone(),
                    other => format!("{:?}", other),
                };
                m.insert(key, yaml_to_j(v));
            }
            J::Object(m)
        }
        Null | BadValue | Alias(_) => J::Null,
    }
}

fn yaml_scalar_plain_ok(s: &str) -> bool {
    if s.is_empty() || s.trim() != s {
        return false;
    }
    if matches!(s.to_ascii_lowercase().as_str(), "null" | "~" | "true" | "false" | "yes" | "no" | "on" | "off" | "y" | "n") {
        return false;
    }
    if s.parse::<f64>().is_ok() || s.starts_with("0x") || s.starts_with("0o") {
        return false;
    }
    let first = s.chars().next().unwrap();
    if "-?:,[]{}#&*!|>'\"%@`".contains(first) {
        return false;
    }
    if s.contains(": ") || s.contains(" #") || s.ends_with(':') {
        return false;
    }
    s.chars().all(|c| !c.is_control())
}

fn yaml_scalar(j: &J) -> String {
    match j {
        J::Null => "null".into(),
        J::Bool(b) => b.to_string(),
        J::Number(n) => n.to_string(),
        J::String(s) => if yaml_scalar_plain_ok(s) { s.clone() } else { serde_json::to_string(s).unwrap_or_default() },
        _ => unreachable!(),
    }
}

fn yaml_emit(j: &J, indent: usize, out: &mut String) {
    let pad = " ".repeat(indent);
    match j {
        J::Object(m) if !m.is_empty() => {
            for (k, v) in m {
                let key = yaml_scalar(&J::String(k.clone()));
                match v {
                    J::Object(x) if !x.is_empty() => { out.push_str(&format!("{}{}:\n", pad, key)); yaml_emit(v, indent + 2, out); }
                    J::Array(x) if !x.is_empty() => { out.push_str(&format!("{}{}:\n", pad, key)); yaml_emit(v, indent + 2, out); }
                    J::Object(_) => out.push_str(&format!("{}{}: {{}}\n", pad, key)),
                    J::Array(_) => out.push_str(&format!("{}{}: []\n", pad, key)),
                    s => out.push_str(&format!("{}{}: {}\n", pad, key, yaml_scalar(s))),
                }
            }
        }
        J::Array(a) if !a.is_empty() => {
            for v in a {
                match v {
                    J::Object(x) if !x.is_empty() => { out.push_str(&format!("{}-\n", pad)); yaml_emit(v, indent + 2, out); }
                    J::Array(x) if !x.is_empty() => { out.push_str(&format!("{}-\n", pad)); yaml_emit(v, indent + 2, out); }
                    J::Object(_) => out.push_str(&format!("{}- {{}}\n", pad)),
                    J::Array(_) => out.push_str(&format!("{}- []\n", pad)),
                    s => out.push_str(&format!("{}- {}\n", pad, yaml_scalar(s))),
                }
            }
        }
        J::Object(_) => out.push_str(&format!("{}{{}}\n", pad)),
        J::Array(_) => out.push_str(&format!("{}[]\n", pad)),
        s => out.push_str(&format!("{}{}\n", pad, yaml_scalar(s))),
    }
}

pub fn yaml() -> IndexMap<String, Value> {
    let mut m = Mod::new();
    m.f("parse", 1, Some(1), |ex, a| {
        let s = a_str(ex, a, 0, "parse")?;
        match yaml_rust2::YamlLoader::load_from_str(&s) {
            Ok(docs) => Ok(docs.first().map(|d| json_to_value(&yaml_to_j(d))).unwrap_or(Value::Null)),
            Err(e) => ex.err("ValueError", format!("invalid YAML: {}", e)),
        }
    });
    m.f("parse_all", 1, Some(1), |ex, a| {
        let s = a_str(ex, a, 0, "parse_all")?;
        match yaml_rust2::YamlLoader::load_from_str(&s) {
            Ok(docs) => Ok(Value::list(docs.iter().map(|d| json_to_value(&yaml_to_j(d))).collect())),
            Err(e) => ex.err("ValueError", format!("invalid YAML: {}", e)),
        }
    });
    m.f("stringify", 1, Some(1), |ex, a| {
        let j = to_j(ex, &a[0], "stringify")?;
        let mut out = String::new();
        yaml_emit(&j, 0, &mut out);
        Ok(Value::str(out))
    });
    m.done()
}

// ───────── toml ─────────

fn toml_to_j(t: &toml::Value) -> J {
    match t {
        toml::Value::String(s) => J::String(s.clone()),
        toml::Value::Integer(i) => J::from(*i),
        toml::Value::Float(f) => serde_json::Number::from_f64(*f).map(J::Number).unwrap_or(J::Null),
        toml::Value::Boolean(b) => J::Bool(*b),
        toml::Value::Datetime(d) => J::String(d.to_string()),
        toml::Value::Array(a) => J::Array(a.iter().map(toml_to_j).collect()),
        toml::Value::Table(t) => J::Object(t.iter().map(|(k, v)| (k.clone(), toml_to_j(v))).collect()),
    }
}

fn j_to_toml(j: &J) -> Result<toml::Value, String> {
    Ok(match j {
        J::Null => return Err("TOML has no null value".into()),
        J::Bool(b) => toml::Value::Boolean(*b),
        J::Number(n) => match n.as_i64() {
            Some(i) => toml::Value::Integer(i),
            None => toml::Value::Float(n.as_f64().unwrap_or(0.0)),
        },
        J::String(s) => toml::Value::String(s.clone()),
        J::Array(a) => toml::Value::Array(a.iter().map(j_to_toml).collect::<Result<_, _>>()?),
        J::Object(o) => {
            let mut t = toml::map::Map::new();
            for (k, v) in o {
                t.insert(k.clone(), j_to_toml(v)?);
            }
            toml::Value::Table(t)
        }
    })
}

pub fn toml() -> IndexMap<String, Value> {
    let mut m = Mod::new();
    m.f("parse", 1, Some(1), |ex, a| {
        let s = a_str(ex, a, 0, "parse")?;
        match s.parse::<toml::Table>() {
            Ok(t) => Ok(json_to_value(&toml_to_j(&toml::Value::Table(t)))),
            Err(e) => ex.err("ValueError", format!("invalid TOML: {}", e)),
        }
    });
    m.f("stringify", 1, Some(1), |ex, a| {
        let j = to_j(ex, &a[0], "stringify")?;
        match j_to_toml(&j) {
            Ok(toml::Value::Table(t)) => match toml::to_string(&t) {
                Ok(s) => Ok(Value::str(s)),
                Err(e) => ex.err("ValueError", format!("cannot encode TOML: {}", e)),
            },
            Ok(_) => ex.err("ValueError", "TOML documents must be maps at the top level"),
            Err(e) => ex.err("ValueError", e),
        }
    });
    m.done()
}

// ───────── object ─────────

fn fields_of(ex: &mut Exec, v: &Value, f: &str) -> R<IndexMap<String, Value>> {
    match v {
        Value::Map(m) => Ok(lk(m).clone()),
        Value::Object(o) => Ok(lk(&o.fields).clone()),
        o => ex.err("TypeError", format!("{}: expected map or object, got {}", f, o.type_name())),
    }
}

fn path_segments(p: &str) -> Vec<String> {
    p.split('.').filter(|s| !s.is_empty()).map(String::from).collect()
}

fn get_path(cur: &Value, segs: &[String]) -> Option<Value> {
    let mut cur = cur.clone();
    for s in segs {
        cur = match &cur {
            Value::Map(m) => lk(m).get(s)?.clone(),
            Value::Object(o) => lk(&o.fields).get(s)?.clone(),
            Value::List(l) => {
                let i: i64 = s.parse().ok()?;
                let g = lk(l);
                let idx = if i < 0 { g.len() as i64 + i } else { i };
                g.get(usize::try_from(idx).ok()?)?.clone()
            }
            _ => return None,
        };
    }
    Some(cur)
}

fn deep_merge(a: &IndexMap<String, Value>, b: &IndexMap<String, Value>) -> IndexMap<String, Value> {
    let mut out = a.clone();
    for (k, v) in b {
        let merged = match (out.get(k), v) {
            (Some(Value::Map(x)), Value::Map(y)) => Value::map(deep_merge(&lk(x).clone(), &lk(y).clone())),
            _ => v.clone(),
        };
        out.insert(k.clone(), merged);
    }
    out
}

pub fn object() -> IndexMap<String, Value> {
    let mut m = Mod::new();
    m.f("keys", 1, Some(1), |ex, a| Ok(Value::list(fields_of(ex, &a[0], "keys")?.keys().map(Value::str).collect())));
    m.f("values", 1, Some(1), |ex, a| Ok(Value::list(fields_of(ex, &a[0], "values")?.values().cloned().collect())));
    m.f("entries", 1, Some(1), |ex, a| Ok(Value::list(fields_of(ex, &a[0], "entries")?.iter().map(|(k, v)| Value::list(vec![Value::str(k), v.clone()])).collect())));
    m.f("from_entries", 1, Some(1), |ex, a| {
        let items = a_list(ex, a, 0, "from_entries")?;
        let mut out = IndexMap::new();
        for it in items {
            match &it {
                Value::List(p) if lk(p).len() == 2 => {
                    let p = lk(p).clone();
                    match &p[0] {
                        Value::Str(k) => { out.insert(k.to_string(), p[1].clone()); }
                        o => return ex.err("TypeError", format!("from_entries: key must be string, got {}", o.type_name())),
                    }
                }
                _ => return ex.err("TypeError", "from_entries: items must be [key, value] pairs"),
            }
        }
        Ok(Value::map(out))
    });
    m.f("merge", 1, None, |ex, a| {
        let mut out = IndexMap::new();
        for v in a {
            out.extend(fields_of(ex, v, "merge")?);
        }
        Ok(Value::map(out))
    });
    m.f("deep_merge", 1, None, |ex, a| {
        let mut out = IndexMap::new();
        for v in a {
            let f = fields_of(ex, v, "deep_merge")?;
            out = deep_merge(&out, &f);
        }
        Ok(Value::map(out))
    });
    m.f("pick", 2, Some(2), |ex, a| {
        let f = fields_of(ex, &a[0], "pick")?;
        let ks = strs(ex, a, 1, "pick")?;
        Ok(Value::map(ks.into_iter().filter_map(|k| f.get(&k).map(|v| (k, v.clone()))).collect()))
    });
    m.f("omit", 2, Some(2), |ex, a| {
        let f = fields_of(ex, &a[0], "omit")?;
        let ks = strs(ex, a, 1, "omit")?;
        Ok(Value::map(f.into_iter().filter(|(k, _)| !ks.contains(k)).collect()))
    });
    m.f("get_path", 2, Some(3), |ex, a| {
        let p = a_str(ex, a, 1, "get_path")?;
        Ok(get_path(&a[0], &path_segments(&p)).unwrap_or_else(|| a.get(2).cloned().unwrap_or(Value::Null)))
    });
    m.f("set_path", 3, Some(3), |ex, a| {
        let p = a_str(ex, a, 1, "set_path")?;
        let segs = path_segments(&p);
        if segs.is_empty() {
            return ex.err("ValueError", "set_path: empty path");
        }
        let mut cur = a[0].clone();
        for (i, s) in segs.iter().enumerate() {
            let last = i == segs.len() - 1;
            match &cur {
                Value::Map(m) => {
                    if last {
                        lk(m).insert(s.clone(), a[2].clone());
                    } else {
                        let next = lk(m).get(s).cloned();
                        let next = match next {
                            Some(v @ (Value::Map(_) | Value::List(_))) => v,
                            _ => { let n = Value::map(IndexMap::new()); lk(m).insert(s.clone(), n.clone()); n }
                        };
                        cur = next;
                    }
                }
                Value::List(l) => {
                    let idx: usize = match s.parse() { Ok(i) => i, Err(_) => return ex.err("ValueError", format!("set_path: `{}` is not a list index", s)) };
                    let mut g = lk(l);
                    if idx >= g.len() {
                        drop(g);
                        return ex.err("IndexError", format!("set_path: index {} out of range", idx));
                    }
                    if last {
                        g[idx] = a[2].clone();
                    } else {
                        let n = g[idx].clone();
                        drop(g);
                        cur = n;
                    }
                }
                o => return ex.err("TypeError", format!("set_path: cannot descend into {}", o.type_name())),
            }
        }
        Ok(a[0].clone())
    });
    m.f("deep_equal", 2, Some(2), |_, a| Ok(Value::Bool(values_equal(&a[0], &a[1]))));
    m.f("freeze", 1, Some(1), |_, a| Ok(nin_runtime::builtins::deep_clone(&a[0])));
    m.done()
}

// ───────── hash / encoding ─────────

fn primes(n: usize) -> Vec<u64> {
    let mut v = vec![];
    let mut c = 2u64;
    while v.len() < n {
        if v.iter().all(|p| c % p != 0) {
            v.push(c);
        }
        c += 1;
    }
    v
}

pub fn sha256(data: &[u8]) -> [u8; 32] {
    let ps = primes(64);
    let frac = |x: f64| ((x - x.floor()) * 4294967296.0).floor() as u32;
    let k: Vec<u32> = ps.iter().map(|p| frac((*p as f64).cbrt())).collect();
    let mut h: [u32; 8] = [0; 8];
    for i in 0..8 {
        h[i] = frac((ps[i] as f64).sqrt());
    }
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([chunk[4 * i], chunk[4 * i + 1], chunk[4 * i + 2], chunk[4 * i + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7].wrapping_add(s1).wrapping_add(ch).wrapping_add(k[i]).wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v = [t1.wrapping_add(t2), v[0], v[1], v[2], v[3].wrapping_add(t1), v[4], v[5], v[6]];
        }
        for i in 0..8 {
            h[i] = h[i].wrapping_add(v[i]);
        }
    }
    let mut out = [0u8; 32];
    for i in 0..8 {
        out[4 * i..4 * i + 4].copy_from_slice(&h[i].to_be_bytes());
    }
    out
}

pub fn md5(data: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21];
    let k: Vec<u32> = (0..64).map(|i| ((i as f64 + 1.0).sin().abs() * 4294967296.0).floor() as u32).collect();
    let (mut a0, mut b0, mut c0, mut d0) = (0x67452301u32, 0xefcdab89u32, 0x98badcfeu32, 0x10325476u32);
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64) * 8).to_le_bytes());
    for chunk in msg.chunks(64) {
        let m: Vec<u32> = (0..16).map(|i| u32::from_le_bytes([chunk[4 * i], chunk[4 * i + 1], chunk[4 * i + 2], chunk[4 * i + 3]])).collect();
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (mut f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            f = f.wrapping_add(a).wrapping_add(k[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(S[i]));
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }
    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&a0.to_le_bytes());
    out[4..8].copy_from_slice(&b0.to_le_bytes());
    out[8..12].copy_from_slice(&c0.to_le_bytes());
    out[12..16].copy_from_slice(&d0.to_le_bytes());
    out
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{:02x}", x)).collect()
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn b64_encode(d: &[u8]) -> String {
    let mut out = String::new();
    for c in d.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { B64[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { B64[n as usize & 63] as char } else { '=' });
    }
    out
}

pub fn b64_decode(s: &str) -> Option<Vec<u8>> {
    let mut buf = 0u32;
    let mut bits = 0;
    let mut out = vec![];
    for c in s.trim_end_matches('=').bytes() {
        if c.is_ascii_whitespace() {
            continue;
        }
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        } as u32;
        buf = buf << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Some(out)
}

fn text_or_bytes(b: Vec<u8>) -> Value {
    match String::from_utf8(b) {
        Ok(s) => Value::str(s),
        Err(e) => bytes_value(e.as_bytes()),
    }
}

pub fn hash() -> IndexMap<String, Value> {
    let mut m = Mod::new();
    m.f("sha256", 1, Some(1), |ex, a| Ok(Value::str(hex(&sha256(&bytes_of(ex, &a[0], "sha256")?)))));
    m.f("md5", 1, Some(1), |ex, a| Ok(Value::str(hex(&md5(&bytes_of(ex, &a[0], "md5")?)))));
    m.f("crc32", 1, Some(1), |ex, a| Ok(Value::Int(nin_pkg::crc::crc32(&bytes_of(ex, &a[0], "crc32")?) as i64)));
    m.f("base64_encode", 1, Some(1), |ex, a| Ok(Value::str(b64_encode(&bytes_of(ex, &a[0], "base64_encode")?))));
    m.f("base64_decode", 1, Some(1), |ex, a| {
        let s = a_str(ex, a, 0, "base64_decode")?;
        match b64_decode(&s) {
            Some(b) => Ok(text_or_bytes(b)),
            None => ex.err("ValueError", "invalid base64"),
        }
    });
    m.f("hex_encode", 1, Some(1), |ex, a| Ok(Value::str(hex(&bytes_of(ex, &a[0], "hex_encode")?))));
    m.f("hex_decode", 1, Some(1), |ex, a| {
        let s = a_str(ex, a, 0, "hex_decode")?;
        if s.len() % 2 != 0 || !s.is_ascii() {
            return ex.err("ValueError", "invalid hex string");
        }
        let mut out = vec![];
        for i in (0..s.len()).step_by(2) {
            match u8::from_str_radix(&s[i..i + 2], 16) {
                Ok(b) => out.push(b),
                Err(_) => return ex.err("ValueError", "invalid hex string"),
            }
        }
        Ok(text_or_bytes(out))
    });
    m.done()
}

// ───────── regex ─────────

fn re(ex: &mut Exec, a: &[Value], f: &str) -> R<regex::Regex> {
    let p = a_str(ex, a, 0, f)?;
    match regex::Regex::new(&p) {
        Ok(r) => Ok(r),
        Err(e) => ex.err("ValueError", format!("invalid regex: {}", e)),
    }
}

fn match_value(text: &str, caps: &regex::Captures, r: &regex::Regex) -> Value {
    let ci = |b: usize| text[..b].chars().count() as i64;
    let m0 = caps.get(0).unwrap();
    let groups: Vec<Value> = (1..caps.len()).map(|i| caps.get(i).map(|g| Value::str(g.as_str())).unwrap_or(Value::Null)).collect();
    let mut named = IndexMap::new();
    for n in r.capture_names().flatten() {
        named.insert(n.to_string(), caps.name(n).map(|g| Value::str(g.as_str())).unwrap_or(Value::Null));
    }
    Value::map_from([("text", Value::str(m0.as_str())), ("start", Value::Int(ci(m0.start()))), ("end", Value::Int(ci(m0.end()))), ("groups", Value::list(groups)), ("named", Value::map(named))])
}

pub fn regex() -> IndexMap<String, Value> {
    let mut m = Mod::new();
    m.f("is_match", 2, Some(2), |ex, a| {
        let r = re(ex, a, "is_match")?;
        Ok(Value::Bool(r.is_match(&a_str(ex, a, 1, "is_match")?)))
    });
    m.f("find", 2, Some(2), |ex, a| {
        let r = re(ex, a, "find")?;
        let t = a_str(ex, a, 1, "find")?;
        Ok(r.captures(&t).map(|c| match_value(&t, &c, &r)).unwrap_or(Value::Null))
    });
    m.f("find_all", 2, Some(2), |ex, a| {
        let r = re(ex, a, "find_all")?;
        let t = a_str(ex, a, 1, "find_all")?;
        Ok(Value::list(r.captures_iter(&t).map(|c| match_value(&t, &c, &r)).collect()))
    });
    m.f("replace", 3, Some(3), |ex, a| {
        let r = re(ex, a, "replace")?;
        let (t, rep) = (a_str(ex, a, 1, "replace")?, a_str(ex, a, 2, "replace")?);
        Ok(Value::str(r.replace_all(&t, rep.as_str())))
    });
    m.f("split", 2, Some(2), |ex, a| {
        let r = re(ex, a, "split")?;
        let t = a_str(ex, a, 1, "split")?;
        Ok(Value::list(r.split(&t).map(Value::str).collect()))
    });
    m.done()
}

// ───────── random ─────────

static STATE: AtomicU64 = AtomicU64::new(0);

fn next_u64() -> u64 {
    let mut s = STATE.load(Ordering::Relaxed);
    if s == 0 {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1);
        s = t ^ ((std::process::id() as u64) << 32) | 1;
    }
    loop {
        let mut x = s;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        match STATE.compare_exchange(STATE.load(Ordering::Relaxed), x, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => return x.wrapping_mul(0x2545F4914F6CDD1D),
            Err(cur) => s = cur.max(1),
        }
    }
}

pub fn random() -> IndexMap<String, Value> {
    let mut m = Mod::new();
    m.f("seed", 1, Some(1), |ex, a| {
        STATE.store((a_int(ex, a, 0, "seed")? as u64) | 1, Ordering::SeqCst);
        Ok(Value::Null)
    });
    m.f("int", 1, Some(2), |ex, a| {
        let (lo, hi) = if a.len() == 1 { (0, a_int(ex, a, 0, "int")?) } else { (a_int(ex, a, 0, "int")?, a_int(ex, a, 1, "int")?) };
        if hi < lo {
            return ex.err("ValueError", "random.int: empty range");
        }
        let span = (hi as i128 - lo as i128 + 1) as u128;
        Ok(Value::Int((lo as i128 + (next_u64() as u128 % span) as i128) as i64))
    });
    m.f("float", 0, Some(0), |_, _| Ok(Value::Float((next_u64() >> 11) as f64 / (1u64 << 53) as f64)));
    m.f("choice", 1, Some(1), |ex, a| {
        let l = a_list(ex, a, 0, "choice")?;
        if l.is_empty() {
            return ex.err("ValueError", "random.choice: empty list");
        }
        Ok(l[(next_u64() % l.len() as u64) as usize].clone())
    });
    m.f("shuffle", 1, Some(1), |ex, a| {
        let mut l = a_list(ex, a, 0, "shuffle")?;
        for i in (1..l.len()).rev() {
            l.swap(i, (next_u64() % (i as u64 + 1)) as usize);
        }
        Ok(Value::list(l))
    });
    m.f("bytes", 1, Some(1), |ex, a| {
        let n = a_int(ex, a, 0, "bytes")?.clamp(0, 1 << 20) as usize;
        Ok(bytes_value(&(0..n).map(|_| next_u64() as u8).collect::<Vec<_>>()))
    });
    m.f("uuid", 0, Some(0), |_, _| {
        let mut b: Vec<u8> = (0..16).map(|_| (next_u64() >> 24) as u8).collect();
        b[6] = (b[6] & 0x0f) | 0x40;
        b[8] = (b[8] & 0x3f) | 0x80;
        let h = hex(&b);
        Ok(Value::str(format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])))
    });
    m.done()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sha256_vectors() {
        assert_eq!(hex(&sha256(b"abc")), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(hex(&sha256(b"")), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(hex(&sha256(&vec![b'a'; 1000])).len(), 64);
    }
    #[test]
    fn md5_vectors() {
        assert_eq!(hex(&md5(b"abc")), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(hex(&md5(b"")), "d41d8cd98f00b204e9800998ecf8427e");
    }
    #[test]
    fn base64_roundtrip() {
        assert_eq!(b64_encode(b"hello!"), "aGVsbG8h");
        assert_eq!(b64_encode(b"hi"), "aGk=");
        assert_eq!(b64_decode("aGk=").unwrap(), b"hi");
        for n in 0..20 {
            let d: Vec<u8> = (0..n).map(|i| (i * 37) as u8).collect();
            assert_eq!(b64_decode(&b64_encode(&d)).unwrap(), d);
        }
        assert!(b64_decode("a$b").is_none());
    }
    #[test]
    fn yaml_emit_roundtrip() {
        let j: J = serde_json::json!({"a": 1, "b": [1, "two", {"c": null}], "s": "x: y", "e": [], "t": "true"});
        let mut out = String::new();
        yaml_emit(&j, 0, &mut out);
        let docs = yaml_rust2::YamlLoader::load_from_str(&out).unwrap();
        assert_eq!(yaml_to_j(&docs[0]), j, "{}", out);
    }
}
