//! JSON parser with source spans — the first stage of the NIN pipeline.
//!
//! `.nin` files are JSON documents. The "script" dialect additionally tolerates
//! `//` and `/* */` comments, trailing commas and a UTF-8 BOM so that scripts
//! stay pleasant to write; [`ParseOptions::strict`] is RFC 8259 JSON.
//! Duplicate object keys are always rejected (they would silently drop
//! declarations in a DSL).
//!
//! Error codes: E0001 unexpected character, E0002 unexpected end of input,
//! E0003 bad string escape, E0004 unterminated string, E0005 bad number,
//! E0006 duplicate key, E0007 trailing characters, E0008 comment not allowed,
//! E0009 trailing comma not allowed, E0010 nesting too deep, E0011 control
//! character in string, E0012 expected `:` / `,` / closing bracket.

use nin_diag::{DResult, Diagnostic, Span};

#[derive(Clone, Debug, PartialEq)]
pub struct Json {
    pub kind: JsonKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum JsonKind {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Array(Vec<Json>),
    /// Members keep document order.
    Object(Vec<Member>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Member {
    pub key: String,
    pub key_span: Span,
    pub value: Json,
}

impl Json {
    pub fn new(kind: JsonKind, span: Span) -> Json {
        Json { kind, span }
    }
    /// Synthetic node without a source location.
    pub fn synthetic(kind: JsonKind) -> Json {
        Json { kind, span: Span::DUMMY }
    }
    /// Human readable JSON type name used in diagnostics.
    pub fn type_name(&self) -> &'static str {
        match self.kind {
            JsonKind::Null => "null",
            JsonKind::Bool(_) => "boolean",
            JsonKind::Int(_) => "integer",
            JsonKind::Float(_) => "number",
            JsonKind::Str(_) => "string",
            JsonKind::Array(_) => "array",
            JsonKind::Object(_) => "object",
        }
    }
    pub fn is_null(&self) -> bool {
        matches!(self.kind, JsonKind::Null)
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self.kind {
            JsonKind::Bool(b) => Some(b),
            _ => None,
        }
    }
    pub fn as_i64(&self) -> Option<i64> {
        match self.kind {
            JsonKind::Int(i) => Some(i),
            _ => None,
        }
    }
    /// Integers and floats as `f64`.
    pub fn as_f64(&self) -> Option<f64> {
        match self.kind {
            JsonKind::Int(i) => Some(i as f64),
            JsonKind::Float(f) => Some(f),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match &self.kind {
            JsonKind::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_array(&self) -> Option<&[Json]> {
        match &self.kind {
            JsonKind::Array(a) => Some(a),
            _ => None,
        }
    }
    pub fn as_object(&self) -> Option<&[Member]> {
        match &self.kind {
            JsonKind::Object(o) => Some(o),
            _ => None,
        }
    }
    pub fn get_member(&self, key: &str) -> Option<&Member> {
        self.as_object()?.iter().find(|m| m.key == key)
    }
    pub fn get(&self, key: &str) -> Option<&Json> {
        self.get_member(key).map(|m| &m.value)
    }

    /// Compact serialization.
    pub fn to_json_string(&self) -> String {
        let mut s = String::new();
        write_json(self, &mut s, None, 0);
        s
    }
    /// Pretty serialization with `indent` spaces per level.
    pub fn to_json_pretty(&self, indent: usize) -> String {
        let mut s = String::new();
        write_json(self, &mut s, Some(indent), 0);
        s
    }
}

#[derive(Clone, Debug)]
pub struct ParseOptions {
    pub allow_comments: bool,
    pub allow_trailing_commas: bool,
    pub max_depth: usize,
}

impl ParseOptions {
    /// RFC 8259 JSON.
    pub fn strict() -> ParseOptions {
        ParseOptions { allow_comments: false, allow_trailing_commas: false, max_depth: 256 }
    }
    /// The `.nin` dialect: comments and trailing commas allowed.
    pub fn script() -> ParseOptions {
        ParseOptions { allow_comments: true, allow_trailing_commas: true, max_depth: 256 }
    }
}

impl Default for ParseOptions {
    fn default() -> Self {
        ParseOptions::script()
    }
}

/// Parse a `.nin` document (script dialect).
pub fn parse_script(src: &str, file: u32) -> DResult<Json> {
    parse(src, file, &ParseOptions::script())
}

/// Parse strict JSON.
pub fn parse_strict(src: &str, file: u32) -> DResult<Json> {
    parse(src, file, &ParseOptions::strict())
}

pub fn parse(src: &str, file: u32, opts: &ParseOptions) -> DResult<Json> {
    let mut p = Parser { src, bytes: src.as_bytes(), pos: 0, file, opts, depth: 0 };
    if p.bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        p.pos = 3;
    }
    p.skip_ws()?;
    if p.pos >= p.bytes.len() {
        return Err(p.err("E0002", "unexpected end of input: the document is empty", p.pos, p.pos).help("a .nin file must contain a JSON object, e.g. {\"nin\": \"1\", \"main\": []}"));
    }
    let v = p.parse_value()?;
    p.skip_ws()?;
    if p.pos < p.bytes.len() {
        let end = p.pos + p.char_len_at(p.pos);
        return Err(p.err("E0007", "unexpected trailing characters after the JSON document", p.pos, end).at(Span::new(file, p.pos, end), "nothing may follow the top-level value"));
    }
    Ok(v)
}

struct Parser<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    file: u32,
    opts: &'a ParseOptions,
    depth: usize,
}

impl<'a> Parser<'a> {
    fn span(&self, a: usize, b: usize) -> Span {
        Span::new(self.file, a, b)
    }

    fn err(&self, code: &'static str, msg: impl Into<String>, a: usize, b: usize) -> Diagnostic {
        let _ = (a, b);
        Diagnostic::error(code, msg)
    }

    fn err_at(&self, code: &'static str, msg: impl Into<String>, a: usize, b: usize, label: impl Into<String>) -> Diagnostic {
        Diagnostic::error(code, msg).at(self.span(a, b), label)
    }

    fn char_len_at(&self, pos: usize) -> usize {
        self.src[pos..].chars().next().map(|c| c.len_utf8()).unwrap_or(1)
    }

    fn describe_at(&self, pos: usize) -> String {
        match self.src[pos..].chars().next() {
            Some(c) if c.is_control() => format!("character U+{:04X}", c as u32),
            Some(c) => format!("`{}`", c),
            None => "end of input".to_string(),
        }
    }

    fn skip_ws(&mut self) -> DResult<()> {
        loop {
            while self.pos < self.bytes.len() && matches!(self.bytes[self.pos], b' ' | b'\t' | b'\n' | b'\r') {
                self.pos += 1;
            }
            if self.pos + 1 < self.bytes.len() && self.bytes[self.pos] == b'/' {
                let next = self.bytes[self.pos + 1];
                if next == b'/' || next == b'*' {
                    let start = self.pos;
                    if !self.opts.allow_comments {
                        return Err(self
                            .err_at("E0008", "comments are not allowed in strict JSON", start, start + 2, "comment starts here")
                            .help("the .nin script dialect allows `//` and `/* */` comments"));
                    }
                    if next == b'/' {
                        while self.pos < self.bytes.len() && self.bytes[self.pos] != b'\n' {
                            self.pos += 1;
                        }
                    } else {
                        self.pos += 2;
                        loop {
                            if self.pos + 1 >= self.bytes.len() {
                                return Err(self.err_at("E0002", "unterminated block comment", start, start + 2, "this comment is never closed"));
                            }
                            if self.bytes[self.pos] == b'*' && self.bytes[self.pos + 1] == b'/' {
                                self.pos += 2;
                                break;
                            }
                            self.pos += 1;
                        }
                    }
                    continue;
                }
            }
            return Ok(());
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn parse_value(&mut self) -> DResult<Json> {
        let start = self.pos;
        match self.peek() {
            None => Err(self.err_at("E0002", "unexpected end of input, expected a value", start, start, "a JSON value is expected here")),
            Some(b'{') => self.parse_object(),
            Some(b'[') => self.parse_array(),
            Some(b'"') => {
                let s = self.parse_string()?;
                Ok(Json::new(JsonKind::Str(s), self.span(start, self.pos)))
            }
            Some(b't') => self.parse_keyword("true", JsonKind::Bool(true)),
            Some(b'f') => self.parse_keyword("false", JsonKind::Bool(false)),
            Some(b'n') => self.parse_keyword("null", JsonKind::Null),
            Some(b'-') | Some(b'0'..=b'9') => self.parse_number(),
            Some(_) => {
                let end = start + self.char_len_at(start);
                let what = self.describe_at(start);
                let mut d = self.err_at("E0001", format!("unexpected {}, expected a JSON value", what), start, end, "not a valid start of a value");
                if self.bytes[start] == b'\'' {
                    d = d.help("JSON strings use double quotes: \"text\"");
                } else if self.bytes[start].is_ascii_alphabetic() {
                    d = d.help("bare words are not allowed; quote strings, or use true / false / null");
                } else if self.bytes[start] == b',' || self.bytes[start] == b'}' || self.bytes[start] == b']' {
                    d = d.help("a value is missing before this character");
                }
                Err(d)
            }
        }
    }

    fn parse_keyword(&mut self, word: &str, kind: JsonKind) -> DResult<Json> {
        let start = self.pos;
        if self.src[start..].starts_with(word) {
            let after = start + word.len();
            let boundary = self.bytes.get(after).map_or(true, |b| !(b.is_ascii_alphanumeric() || *b == b'_'));
            if boundary {
                self.pos = after;
                return Ok(Json::new(kind, self.span(start, after)));
            }
        }
        let mut end = start;
        while end < self.bytes.len() && (self.bytes[end].is_ascii_alphanumeric() || self.bytes[end] == b'_') {
            end += 1;
        }
        let found = &self.src[start..end.max(start + 1)];
        Err(self
            .err_at("E0001", format!("unexpected word `{}`, expected a JSON value", found), start, end.max(start + 1), "not a valid value")
            .help(format!("did you mean `{}`? strings must be wrapped in double quotes", word)))
    }

    fn parse_number(&mut self) -> DResult<Json> {
        let start = self.pos;
        let b = self.bytes;
        let mut i = self.pos;
        if b[i] == b'-' {
            i += 1;
        }
        let int_start = i;
        if i < b.len() && b[i] == b'0' {
            i += 1;
            if i < b.len() && b[i].is_ascii_digit() {
                return Err(self.err_at("E0005", "invalid number: leading zeros are not allowed", start, i + 1, "remove the leading zero"));
            }
        } else {
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
        }
        if i == int_start {
            return Err(self.err_at("E0005", "invalid number: expected digits", start, (i + 1).min(b.len()).max(start + 1), "digits expected after `-`"));
        }
        let mut is_float = false;
        if i < b.len() && b[i] == b'.' {
            is_float = true;
            i += 1;
            let fs = i;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            if i == fs {
                return Err(self.err_at("E0005", "invalid number: expected digits after the decimal point", start, i.max(start + 1), "digits expected after `.`"));
            }
        }
        if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
            is_float = true;
            i += 1;
            if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
                i += 1;
            }
            let es = i;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            if i == es {
                return Err(self.err_at("E0005", "invalid number: expected digits in the exponent", start, i.max(start + 1), "exponent digits expected"));
            }
        }
        let text = &self.src[start..i];
        self.pos = i;
        let span = self.span(start, i);
        if !is_float {
            if let Ok(v) = text.parse::<i64>() {
                return Ok(Json::new(JsonKind::Int(v), span));
            }
        }
        match text.parse::<f64>() {
            Ok(f) if f.is_finite() => Ok(Json::new(JsonKind::Float(f), span)),
            _ => Err(Diagnostic::error("E0005", format!("number `{}` is out of range", text)).at(span, "does not fit in a 64-bit number")),
        }
    }

    fn parse_hex4(&mut self, esc_start: usize) -> DResult<u32> {
        if self.pos + 4 > self.bytes.len() {
            return Err(self.err_at("E0003", "incomplete \\u escape", esc_start, self.bytes.len(), "expected 4 hex digits"));
        }
        let mut v = 0u32;
        for k in 0..4 {
            let c = self.bytes[self.pos + k];
            let d = match c {
                b'0'..=b'9' => c - b'0',
                b'a'..=b'f' => c - b'a' + 10,
                b'A'..=b'F' => c - b'A' + 10,
                _ => return Err(self.err_at("E0003", "invalid \\u escape: expected 4 hex digits", esc_start, self.pos + k + 1, "not a hex digit")),
            };
            v = v * 16 + d as u32;
        }
        self.pos += 4;
        Ok(v)
    }

    fn parse_string(&mut self) -> DResult<String> {
        let start = self.pos;
        self.pos += 1; // opening quote
        let mut out = String::new();
        loop {
            let run_start = self.pos;
            while self.pos < self.bytes.len() {
                let c = self.bytes[self.pos];
                if c == b'"' || c == b'\\' || c < 0x20 {
                    break;
                }
                self.pos += 1;
            }
            out.push_str(&self.src[run_start..self.pos]);
            match self.peek() {
                None => return Err(self.err_at("E0004", "unterminated string", start, (start + 1).min(self.bytes.len()), "this string is never closed").help("add a closing double quote")),
                Some(b'"') => {
                    self.pos += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    let esc = self.pos;
                    self.pos += 1;
                    let c = match self.peek() {
                        None => return Err(self.err_at("E0004", "unterminated string", start, start + 1, "this string is never closed")),
                        Some(c) => c,
                    };
                    self.pos += 1;
                    match c {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let hi = self.parse_hex4(esc)?;
                            if (0xD800..0xDC00).contains(&hi) {
                                if self.src[self.pos..].starts_with("\\u") {
                                    self.pos += 2;
                                    let lo = self.parse_hex4(esc)?;
                                    if (0xDC00..0xE000).contains(&lo) {
                                        let cp = 0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00);
                                        match char::from_u32(cp) {
                                            Some(ch) => out.push(ch),
                                            None => return Err(self.err_at("E0003", "invalid unicode escape", esc, self.pos, "not a Unicode scalar value")),
                                        }
                                    } else {
                                        return Err(self.err_at("E0003", "invalid surrogate pair in \\u escape", esc, self.pos, "a high surrogate must be followed by a low surrogate"));
                                    }
                                } else {
                                    return Err(self.err_at("E0003", "lone high surrogate in \\u escape", esc, self.pos, "must be followed by a \\uDC00-\\uDFFF escape"));
                                }
                            } else if (0xDC00..0xE000).contains(&hi) {
                                return Err(self.err_at("E0003", "lone low surrogate in \\u escape", esc, self.pos, "unpaired surrogate"));
                            } else {
                                match char::from_u32(hi) {
                                    Some(ch) => out.push(ch),
                                    None => return Err(self.err_at("E0003", "invalid unicode escape", esc, self.pos, "not a Unicode scalar value")),
                                }
                            }
                        }
                        _ => {
                            let end = esc + 1 + self.src[esc + 1..].chars().next().map_or(1, |c| c.len_utf8());
                            self.pos = end;
                            return Err(self
                                .err_at("E0003", format!("invalid escape sequence `{}`", &self.src[esc..end]), esc, end, "unknown escape")
                                .help("valid escapes: \\\" \\\\ \\/ \\b \\f \\n \\r \\t \\uXXXX"));
                        }
                    }
                }
                Some(c) => {
                    // control character
                    return Err(self
                        .err_at("E0011", format!("control character U+{:04X} in string", c), self.pos, self.pos + 1, "must be escaped")
                        .help("write newlines as \\n and tabs as \\t inside strings"));
                }
            }
        }
    }

    fn enter(&mut self, at: usize) -> DResult<()> {
        self.depth += 1;
        if self.depth > self.opts.max_depth {
            return Err(self.err_at("E0010", format!("nesting is deeper than {} levels", self.opts.max_depth), at, at + 1, "too deeply nested"));
        }
        Ok(())
    }

    fn parse_array(&mut self) -> DResult<Json> {
        let start = self.pos;
        self.enter(start)?;
        self.pos += 1;
        let mut items = Vec::new();
        self.skip_ws()?;
        if self.peek() == Some(b']') {
            self.pos += 1;
            self.depth -= 1;
            return Ok(Json::new(JsonKind::Array(items), self.span(start, self.pos)));
        }
        loop {
            self.skip_ws()?;
            items.push(self.parse_value()?);
            self.skip_ws()?;
            match self.peek() {
                Some(b',') => {
                    let comma = self.pos;
                    self.pos += 1;
                    self.skip_ws()?;
                    if self.peek() == Some(b']') {
                        if !self.opts.allow_trailing_commas {
                            return Err(self.err_at("E0009", "trailing comma is not allowed in strict JSON", comma, comma + 1, "remove this comma"));
                        }
                        self.pos += 1;
                        break;
                    }
                }
                Some(b']') => {
                    self.pos += 1;
                    break;
                }
                None => {
                    return Err(self
                        .err_at("E0002", "unexpected end of input inside an array", start, start + 1, "this array is never closed")
                        .help("add a closing `]`"))
                }
                Some(_) => {
                    let end = self.pos + self.char_len_at(self.pos);
                    return Err(self
                        .err_at("E0012", format!("expected `,` or `]` but found {}", self.describe_at(self.pos)), self.pos, end, "expected `,` or `]`")
                        .also(self.span(start, start + 1), "array opened here"));
                }
            }
        }
        self.depth -= 1;
        Ok(Json::new(JsonKind::Array(items), self.span(start, self.pos)))
    }

    fn parse_object(&mut self) -> DResult<Json> {
        let start = self.pos;
        self.enter(start)?;
        self.pos += 1;
        let mut members: Vec<Member> = Vec::new();
        self.skip_ws()?;
        if self.peek() == Some(b'}') {
            self.pos += 1;
            self.depth -= 1;
            return Ok(Json::new(JsonKind::Object(members), self.span(start, self.pos)));
        }
        loop {
            self.skip_ws()?;
            let key_start = self.pos;
            match self.peek() {
                Some(b'"') => {}
                None => {
                    return Err(self
                        .err_at("E0002", "unexpected end of input inside an object", start, start + 1, "this object is never closed")
                        .help("add a closing `}`"))
                }
                Some(_) => {
                    let end = key_start + self.char_len_at(key_start);
                    let mut d = self
                        .err_at("E0001", format!("expected a string key but found {}", self.describe_at(key_start)), key_start, end, "object keys must be double-quoted strings")
                        .also(self.span(start, start + 1), "object opened here");
                    if self.bytes[key_start] == b'}' {
                        d = d.help("remove the trailing comma before `}`");
                    }
                    return Err(d);
                }
            }
            let key = self.parse_string()?;
            let key_span = self.span(key_start, self.pos);
            self.skip_ws()?;
            match self.peek() {
                Some(b':') => self.pos += 1,
                None => return Err(self.err_at("E0002", "unexpected end of input, expected `:`", self.pos, self.pos, "`:` expected after the key")),
                Some(_) => {
                    let end = self.pos + self.char_len_at(self.pos);
                    return Err(self.err_at("E0012", format!("expected `:` after key but found {}", self.describe_at(self.pos)), self.pos, end, "expected `:`"));
                }
            }
            self.skip_ws()?;
            let value = self.parse_value()?;
            if let Some(prev) = members.iter().find(|m| m.key == key) {
                return Err(Diagnostic::error("E0006", format!("duplicate key `{}`", key))
                    .at(key_span, "this key is already defined")
                    .also(prev.key_span, "first defined here"));
            }
            members.push(Member { key, key_span, value });
            self.skip_ws()?;
            match self.peek() {
                Some(b',') => {
                    let comma = self.pos;
                    self.pos += 1;
                    self.skip_ws()?;
                    if self.peek() == Some(b'}') {
                        if !self.opts.allow_trailing_commas {
                            return Err(self.err_at("E0009", "trailing comma is not allowed in strict JSON", comma, comma + 1, "remove this comma"));
                        }
                        self.pos += 1;
                        break;
                    }
                }
                Some(b'}') => {
                    self.pos += 1;
                    break;
                }
                None => {
                    return Err(self
                        .err_at("E0002", "unexpected end of input inside an object", start, start + 1, "this object is never closed")
                        .help("add a closing `}`"))
                }
                Some(_) => {
                    let end = self.pos + self.char_len_at(self.pos);
                    return Err(self
                        .err_at("E0012", format!("expected `,` or `}}` but found {}", self.describe_at(self.pos)), self.pos, end, "expected `,` or `}`")
                        .also(self.span(start, start + 1), "object opened here")
                        .help("members must be separated by commas"));
                }
            }
        }
        self.depth -= 1;
        Ok(Json::new(JsonKind::Object(members), self.span(start, self.pos)))
    }
}

/// Escape a string as a JSON string literal (with quotes).
pub fn escape_json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Format an `f64` so that it re-parses as a float (never as an integer).
pub fn format_float(f: f64) -> String {
    if !f.is_finite() {
        return "null".to_string();
    }
    let s = format!("{}", f);
    if s.contains('.') || s.contains('e') || s.contains('E') {
        s
    } else {
        format!("{}.0", s)
    }
}

fn write_json(j: &Json, out: &mut String, indent: Option<usize>, level: usize) {
    match &j.kind {
        JsonKind::Null => out.push_str("null"),
        JsonKind::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        JsonKind::Int(i) => out.push_str(&i.to_string()),
        JsonKind::Float(f) => out.push_str(&format_float(*f)),
        JsonKind::Str(s) => out.push_str(&escape_json_string(s)),
        JsonKind::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (i, it) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                newline(out, indent, level + 1);
                write_json(it, out, indent, level + 1);
            }
            newline(out, indent, level);
            out.push(']');
        }
        JsonKind::Object(ms) => {
            if ms.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push('{');
            for (i, m) in ms.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                newline(out, indent, level + 1);
                out.push_str(&escape_json_string(&m.key));
                out.push(':');
                if indent.is_some() {
                    out.push(' ');
                }
                write_json(&m.value, out, indent, level + 1);
            }
            newline(out, indent, level);
            out.push('}');
        }
    }
}

fn newline(out: &mut String, indent: Option<usize>, level: usize) {
    if let Some(n) = indent {
        out.push('\n');
        for _ in 0..n * level {
            out.push(' ');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> DResult<Json> {
        parse_script(s, 0)
    }

    #[test]
    fn scalars() {
        assert_eq!(p("null").unwrap().kind, JsonKind::Null);
        assert_eq!(p(" true ").unwrap().kind, JsonKind::Bool(true));
        assert_eq!(p("-12").unwrap().kind, JsonKind::Int(-12));
        assert_eq!(p("1.5e2").unwrap().kind, JsonKind::Float(150.0));
        assert_eq!(p("9223372036854775808").unwrap().kind, JsonKind::Float(9223372036854775808.0));
        assert_eq!(p("\"a\\n\\u0041\\ud83d\\ude00\"").unwrap().kind, JsonKind::Str("a\nA😀".into()));
        assert_eq!(p("\"가나다\"").unwrap().kind, JsonKind::Str("가나다".into()));
    }

    #[test]
    fn structures_and_spans() {
        let src = "{\"a\": [1, 2, {\"b\": null}], \"c\": \"x\"}";
        let j = p(src).unwrap();
        assert_eq!(j.type_name(), "object");
        let a = j.get("a").unwrap();
        assert_eq!(a.as_array().unwrap().len(), 3);
        let m = j.get_member("c").unwrap();
        assert_eq!(&src[m.key_span.start as usize..m.key_span.end as usize], "\"c\"");
        assert_eq!(&src[m.value.span.start as usize..m.value.span.end as usize], "\"x\"");
        assert_eq!(j.get("zzz"), None);
    }

    #[test]
    fn comments_and_trailing_commas() {
        let j = p("// head\n{ /* k */ \"a\": [1,2,], // t\n \"b\": {},\n}").unwrap();
        assert_eq!(j.get("a").unwrap().as_array().unwrap().len(), 2);
        assert!(parse_strict("{\"a\":1,}", 0).is_err());
        assert_eq!(parse_strict("{\"a\":1,}", 0).unwrap_err().code, "E0009");
        assert_eq!(parse_strict("// x\n1", 0).unwrap_err().code, "E0008");
        assert!(parse_strict("[1,]", 0).is_err());
    }

    #[test]
    fn bom_is_skipped() {
        assert!(p("\u{feff}{\"a\":1}").is_ok());
    }

    #[test]
    fn errors_have_codes_and_locations() {
        assert_eq!(p("").unwrap_err().code, "E0002");
        assert_eq!(p("{\"a\" 1}").unwrap_err().code, "E0012");
        assert_eq!(p("{\"a\": }").unwrap_err().code, "E0001");
        assert_eq!(p("{a: 1}").unwrap_err().code, "E0001");
        assert_eq!(p("[1 2]").unwrap_err().code, "E0012");
        assert_eq!(p("\"abc").unwrap_err().code, "E0004");
        assert_eq!(p("\"a\\q\"").unwrap_err().code, "E0003");
        assert_eq!(p("\"a\nb\"").unwrap_err().code, "E0011");
        assert_eq!(p("01").unwrap_err().code, "E0005");
        assert_eq!(p("1.").unwrap_err().code, "E0005");
        assert_eq!(p("1e").unwrap_err().code, "E0005");
        assert_eq!(p("-").unwrap_err().code, "E0005");
        assert_eq!(p("{} x").unwrap_err().code, "E0007");
        assert_eq!(p("{\"a\":1,\"a\":2}").unwrap_err().code, "E0006");
        assert_eq!(p("[").unwrap_err().code, "E0002");
        assert_eq!(p("{\"a\":1").unwrap_err().code, "E0002");
        assert_eq!(p("'x'").unwrap_err().code, "E0001");
        assert_eq!(p("tru").unwrap_err().code, "E0001");
        assert_eq!(p("\"\\ud800\"").unwrap_err().code, "E0003");
        assert_eq!(p("/* open").unwrap_err().code, "E0002");
        let e = p("{\"a\":1,\"a\":2}").unwrap_err();
        assert_eq!(e.labels.len(), 2);
        assert!(e.primary_span().is_some());
    }

    #[test]
    fn depth_limit() {
        let deep = "[".repeat(300) + &"]".repeat(300);
        assert_eq!(p(&deep).unwrap_err().code, "E0010");
        let ok = "[".repeat(100) + &"]".repeat(100);
        assert!(p(&ok).is_ok());
    }

    #[test]
    fn roundtrip() {
        let src = r#"{"a":[1,2.5,"x\n",true,null],"b":{"c":[]},"d":{}}"#;
        let j = p(src).unwrap();
        assert_eq!(j.to_json_string(), src);
        let pretty = j.to_json_pretty(2);
        let j2 = p(&pretty).unwrap();
        assert_eq!(j2.to_json_string(), src);
        assert!(pretty.contains("\n  \"a\": ["));
    }

    #[test]
    fn float_formatting_reparses_as_float() {
        assert_eq!(format_float(3.0), "3.0");
        assert_eq!(format_float(0.5), "0.5");
        assert_eq!(p(&format_float(1e300)).unwrap().kind, JsonKind::Float(1e300));
    }

    #[test]
    fn rendered_error_points_to_line() {
        let mut sm = nin_diag::SourceMap::new();
        let src = "{\n  \"a\": 1\n  \"b\": 2\n}";
        let id = sm.add_file("x.nin", src);
        let e = parse_script(src, id).unwrap_err();
        let r = sm.render(&e, false);
        assert!(r.contains("x.nin:3:3"), "{}", r);
    }
}
