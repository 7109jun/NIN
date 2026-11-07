//! Diagnostics for NIN Shell: spans, severities, rich rustc-style rendering and
//! "did you mean" suggestions. Every compiler / runtime stage reports problems
//! through [`Diagnostic`].
//!
//! Code ranges: E00xx JSON syntax, E01xx schema, E02xx lowering/expression
//! parsing, E03xx semantic analysis, E04xx types, E05xx runtime, E06xx workflow,
//! E07xx packages/plugins/scheduler, E08xx CLI/IO.

use std::fmt;

/// Byte range inside one source file (`file` indexes a [`SourceMap`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Span {
    pub file: u32,
    pub start: u32,
    pub end: u32,
}

impl Default for Span {
    fn default() -> Self {
        Span::DUMMY
    }
}

impl Span {
    /// A span that points nowhere (synthetic nodes).
    pub const DUMMY: Span = Span { file: u32::MAX, start: 0, end: 0 };

    pub fn new(file: u32, start: usize, end: usize) -> Span {
        Span { file, start: start as u32, end: end.max(start) as u32 }
    }
    pub fn is_dummy(&self) -> bool {
        self.file == u32::MAX
    }
    pub fn len(&self) -> usize {
        (self.end - self.start) as usize
    }
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }
    /// Smallest span covering both (a dummy operand is ignored).
    pub fn to(self, other: Span) -> Span {
        if self.is_dummy() {
            return other;
        }
        if other.is_dummy() || other.file != self.file {
            return self;
        }
        Span { file: self.file, start: self.start.min(other.start), end: self.end.max(other.end) }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Help,
    Note,
    Warning,
    Error,
}

impl Severity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Note => "note",
            Severity::Help => "help",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Label {
    pub span: Span,
    pub message: String,
    pub primary: bool,
}

#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: &'static str,
    pub message: String,
    pub labels: Vec<Label>,
    pub notes: Vec<String>,
    pub help: Option<String>,
}

impl Diagnostic {
    pub fn new(severity: Severity, code: &'static str, message: impl Into<String>) -> Diagnostic {
        Diagnostic { severity, code, message: message.into(), labels: Vec::new(), notes: Vec::new(), help: None }
    }
    pub fn error(code: &'static str, message: impl Into<String>) -> Diagnostic {
        Diagnostic::new(Severity::Error, code, message)
    }
    pub fn warning(code: &'static str, message: impl Into<String>) -> Diagnostic {
        Diagnostic::new(Severity::Warning, code, message)
    }
    /// Primary label (the main location).
    pub fn at(mut self, span: Span, message: impl Into<String>) -> Diagnostic {
        self.labels.push(Label { span, message: message.into(), primary: true });
        self
    }
    /// Secondary label (related location).
    pub fn also(mut self, span: Span, message: impl Into<String>) -> Diagnostic {
        self.labels.push(Label { span, message: message.into(), primary: false });
        self
    }
    pub fn note(mut self, note: impl Into<String>) -> Diagnostic {
        self.notes.push(note.into());
        self
    }
    pub fn help(mut self, help: impl Into<String>) -> Diagnostic {
        self.help = Some(help.into());
        self
    }
    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
    /// Span of the primary label, if any.
    pub fn primary_span(&self) -> Option<Span> {
        self.labels.iter().find(|l| l.primary && !l.span.is_dummy()).map(|l| l.span)
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}[{}]: {}", self.severity.as_str(), self.code, self.message)
    }
}

impl std::error::Error for Diagnostic {}

pub type DResult<T> = Result<T, Diagnostic>;

/// Collector used by multi-error stages (schema validation, semantic analysis).
#[derive(Clone, Debug, Default)]
pub struct Diagnostics {
    pub items: Vec<Diagnostic>,
}

impl Diagnostics {
    pub fn new() -> Diagnostics {
        Diagnostics { items: Vec::new() }
    }
    pub fn push(&mut self, d: Diagnostic) {
        self.items.push(d);
    }
    pub fn extend(&mut self, other: impl IntoIterator<Item = Diagnostic>) {
        self.items.extend(other);
    }
    pub fn has_errors(&self) -> bool {
        self.items.iter().any(|d| d.is_error())
    }
    pub fn error_count(&self) -> usize {
        self.items.iter().filter(|d| d.is_error()).count()
    }
    pub fn warning_count(&self) -> usize {
        self.items.iter().filter(|d| d.severity == Severity::Warning).count()
    }
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
    pub fn into_result<T>(self, value: T) -> Result<T, Diagnostics> {
        if self.has_errors() {
            Err(self)
        } else {
            Ok(value)
        }
    }
}

/// One loaded source file with a line index.
#[derive(Clone, Debug)]
pub struct SourceFile {
    pub name: String,
    pub text: String,
    line_starts: Vec<usize>,
}

impl SourceFile {
    fn new(name: String, text: String) -> SourceFile {
        let mut line_starts = vec![0];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        SourceFile { name, text, line_starts }
    }
    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }
    /// 1-based (line, column) where column counts Unicode scalar values.
    pub fn line_col(&self, offset: usize) -> (usize, usize) {
        let offset = offset.min(self.text.len());
        let line = match self.line_starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i - 1,
        };
        let start = self.line_starts[line];
        let mut end = offset;
        while end > start && !self.text.is_char_boundary(end) {
            end -= 1;
        }
        (line + 1, self.text[start..end].chars().count() + 1)
    }
    /// Text of the 1-based line without the trailing newline.
    pub fn line_text(&self, line: usize) -> &str {
        if line == 0 || line > self.line_starts.len() {
            return "";
        }
        let start = self.line_starts[line - 1];
        let end = if line < self.line_starts.len() { self.line_starts[line] } else { self.text.len() };
        self.text[start..end].trim_end_matches(['\n', '\r'])
    }
    fn line_start(&self, line: usize) -> usize {
        self.line_starts[line - 1]
    }
}

#[derive(Clone, Debug, Default)]
pub struct SourceMap {
    files: Vec<SourceFile>,
}

const TAB_WIDTH: usize = 4;

impl SourceMap {
    pub fn new() -> SourceMap {
        SourceMap { files: Vec::new() }
    }
    pub fn add_file(&mut self, name: impl Into<String>, text: impl Into<String>) -> u32 {
        self.files.push(SourceFile::new(name.into(), text.into()));
        (self.files.len() - 1) as u32
    }
    pub fn file(&self, id: u32) -> Option<&SourceFile> {
        self.files.get(id as usize)
    }
    pub fn name(&self, id: u32) -> &str {
        self.file(id).map(|f| f.name.as_str()).unwrap_or("<unknown>")
    }
    /// `file:line:col` for a span, or `<unknown>`.
    pub fn location(&self, span: Span) -> String {
        match self.file(span.file) {
            Some(f) if !span.is_dummy() => {
                let (l, c) = f.line_col(span.start as usize);
                format!("{}:{}:{}", f.name, l, c)
            }
            _ => "<unknown>".to_string(),
        }
    }

    /// Render one diagnostic in rustc style. `color` enables ANSI escapes.
    pub fn render(&self, d: &Diagnostic, color: bool) -> String {
        let paint = |code: &str, s: &str| -> String {
            if color {
                format!("\x1b[{}m{}\x1b[0m", code, s)
            } else {
                s.to_string()
            }
        };
        let sev_color = match d.severity {
            Severity::Error => "1;31",
            Severity::Warning => "1;33",
            Severity::Note => "1;36",
            Severity::Help => "1;32",
        };
        let mut out = String::new();
        let head = if d.code.is_empty() {
            d.severity.as_str().to_string()
        } else {
            format!("{}[{}]", d.severity.as_str(), d.code)
        };
        out.push_str(&paint(sev_color, &head));
        out.push_str(&paint("1", &format!(": {}", d.message)));
        out.push('\n');

        // Labels that can be shown as snippets.
        let shown: Vec<&Label> = d
            .labels
            .iter()
            .filter(|l| !l.span.is_dummy() && self.file(l.span.file).is_some())
            .collect();
        let width = shown
            .iter()
            .filter_map(|l| self.file(l.span.file).map(|f| f.line_col(l.span.start as usize).0))
            .max()
            .unwrap_or(1)
            .to_string()
            .len();
        let pad = " ".repeat(width);
        let gutter = paint("1;34", "|");
        let arrow = paint("1;34", "-->");
        if let Some(first) = shown.iter().find(|l| l.primary).or(shown.first()) {
            out.push_str(&format!("{} {} {}\n", pad, arrow, self.location(first.span)));
        }
        let mut last_file_line: Option<(u32, usize)> = None;
        for label in &shown {
            let file = match self.file(label.span.file) {
                Some(f) => f,
                None => continue,
            };
            let (line, col) = file.line_col(label.span.start as usize);
            if last_file_line.is_none() {
                out.push_str(&format!("{} {}\n", pad, gutter));
            } else if last_file_line != Some((label.span.file, line)) {
                out.push_str(&format!("{} {}\n", pad, paint("1;34", "...")));
            }
            last_file_line = Some((label.span.file, line));
            let text = file.line_text(line);
            let shown_text = expand_tabs(text);
            out.push_str(&format!("{} {} {}\n", paint("1;34", &format!("{:>w$}", line, w = width)), gutter, shown_text));
            // Underline: from col to span end on this line.
            let line_start = file.line_start(line);
            let line_end = line_start + text.len();
            let span_end = (label.span.end as usize).min(line_end).max(label.span.start as usize);
            let before = &file.text[line_start..(label.span.start as usize).min(line_end)];
            let underlined = &file.text[(label.span.start as usize).min(line_end)..span_end.max((label.span.start as usize).min(line_end))];
            let lead = display_width(before);
            let mut ulen = display_width(underlined);
            if ulen == 0 {
                ulen = 1;
            }
            let _ = col;
            let ch = if label.primary { '^' } else { '-' };
            let marks: String = std::iter::repeat(ch).take(ulen).collect();
            let marks = if label.primary { paint(sev_color, &marks) } else { paint("1;34", &marks) };
            let msg = if label.message.is_empty() {
                String::new()
            } else if label.primary {
                format!(" {}", paint(sev_color, &label.message))
            } else {
                format!(" {}", paint("1;34", &label.message))
            };
            out.push_str(&format!("{} {} {}{}{}\n", pad, gutter, " ".repeat(lead), marks, msg));
        }
        if !shown.is_empty() {
            out.push_str(&format!("{} {}\n", pad, gutter));
        }
        for n in &d.notes {
            out.push_str(&format!("{} {} {}: {}\n", pad, paint("1;34", "="), paint("1", "note"), n));
        }
        if let Some(h) = &d.help {
            out.push_str(&format!("{} {} {}: {}\n", pad, paint("1;34", "="), paint("1", "help"), h));
        }
        out
    }

    pub fn render_all(&self, ds: &[Diagnostic], color: bool) -> String {
        let mut out = String::new();
        for d in ds {
            out.push_str(&self.render(d, color));
            out.push('\n');
        }
        out
    }
}

fn expand_tabs(s: &str) -> String {
    s.replace('\t', &" ".repeat(TAB_WIDTH))
}

fn display_width(s: &str) -> usize {
    s.chars().map(|c| if c == '\t' { TAB_WIDTH } else if is_wide(c) { 2 } else { 1 }).sum()
}

/// Rough East-Asian-wide detection so carets line up under Korean/CJK text.
fn is_wide(c: char) -> bool {
    let u = c as u32;
    (0x1100..=0x115F).contains(&u)
        || (0x2E80..=0xA4CF).contains(&u)
        || (0xAC00..=0xD7A3).contains(&u)
        || (0xF900..=0xFAFF).contains(&u)
        || (0xFE30..=0xFE6F).contains(&u)
        || (0xFF00..=0xFF60).contains(&u)
        || (0xFFE0..=0xFFE6).contains(&u)
        || (0x1F300..=0x1F64F).contains(&u)
        || (0x20000..=0x3FFFD).contains(&u)
}

/// Levenshtein edit distance (case-sensitive).
pub fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        prev = cur;
    }
    prev[b.len()]
}

fn is_anagram(a: &str, b: &str) -> bool {
    let mut x: Vec<char> = a.chars().collect();
    let mut y: Vec<char> = b.chars().collect();
    x.sort_unstable();
    y.sort_unstable();
    x == y
}

/// Best "did you mean" candidate for `name`, if one is close enough.
pub fn suggest<'a, I>(name: &str, candidates: I) -> Option<String>
where
    I: IntoIterator<Item = &'a str>,
{
    let lname = name.to_lowercase();
    let limit = (name.chars().count() / 3).max(2).min(3);
    let mut best: Option<(usize, &str)> = None;
    for c in candidates {
        if c == name {
            continue;
        }
        let lc = c.to_lowercase();
        let d = if lc == lname {
            0
        } else if is_anagram(&lname, &lc) {
            1 // transposed letters ("mian" → "main")
        } else {
            edit_distance(&lname, &lc)
        };
        if d <= limit && best.map_or(true, |(bd, _)| d < bd) {
            best = Some((d, c));
        }
    }
    best.map(|(_, c)| c.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn span_join() {
        let a = Span::new(0, 2, 5);
        let b = Span::new(0, 4, 9);
        assert_eq!(a.to(b), Span::new(0, 2, 9));
        assert_eq!(Span::DUMMY.to(a), a);
        assert_eq!(a.to(Span::DUMMY), a);
    }

    #[test]
    fn line_col_counts_chars() {
        let mut sm = SourceMap::new();
        let id = sm.add_file("a.nin", "가나다\nabc\n");
        let f = sm.file(id).unwrap();
        assert_eq!(f.line_col(0), (1, 1));
        assert_eq!(f.line_col("가나".len()), (1, 3));
        assert_eq!(f.line_col("가나다\n".len()), (2, 1));
        assert_eq!(f.line_text(2), "abc");
        assert_eq!(f.line_text(9), "");
    }

    #[test]
    fn render_points_at_span() {
        let mut sm = SourceMap::new();
        let id = sm.add_file("t.nin", "{\n  \"x\": ,\n}\n");
        let start = "{\n  \"x\": ".len();
        let d = Diagnostic::error("E0001", "unexpected `,`").at(Span::new(id, start, start + 1), "expected a value").help("remove the comma");
        let s = sm.render(&d, false);
        assert!(s.contains("error[E0001]: unexpected `,`"), "{}", s);
        assert!(s.contains("--> t.nin:2:8"), "{}", s);
        assert!(s.contains("^ expected a value"), "{}", s);
        assert!(s.contains("= help: remove the comma"), "{}", s);
    }

    #[test]
    fn render_without_location() {
        let sm = SourceMap::new();
        let d = Diagnostic::error("E0500", "boom").note("n");
        let s = sm.render(&d, false);
        assert!(s.starts_with("error[E0500]: boom"));
        assert!(s.contains("= note: n"));
    }

    #[test]
    fn suggestions() {
        assert_eq!(suggest("clases", ["classes", "functions"]), Some("classes".into()));
        assert_eq!(suggest("Main", ["main"]), Some("main".into()));
        assert_eq!(suggest("zzzz", ["classes", "functions"]), None);
        assert_eq!(edit_distance("kitten", "sitting"), 3);
    }

    #[test]
    fn diagnostics_collect() {
        let mut ds = Diagnostics::new();
        ds.push(Diagnostic::warning("W1", "w"));
        assert!(!ds.has_errors());
        ds.push(Diagnostic::error("E1", "e"));
        assert!(ds.has_errors());
        assert_eq!(ds.error_count(), 1);
        assert_eq!(ds.warning_count(), 1);
    }
}
