use nin_runtime::{lk, Sink};
use std::sync::{Arc, Mutex};

static CONSOLE: Mutex<()> = Mutex::new(());

/// Writes to the process stdout/stderr (serialized so lines never interleave).
pub struct ConsoleSink;

impl Sink for ConsoleSink {
    fn write(&self, text: &str, is_err: bool) {
        use std::io::Write;
        let _g = lk(&CONSOLE);
        if is_err {
            let mut e = std::io::stderr().lock();
            let _ = e.write_all(text.as_bytes());
            let _ = e.flush();
        } else {
            let mut o = std::io::stdout().lock();
            let _ = o.write_all(text.as_bytes());
            let _ = o.flush();
        }
    }
}

/// Prefixes every complete line with `[prefix] ` before forwarding it.
pub struct PrefixSink {
    prefix: String,
    inner: Arc<dyn Sink>,
    buf: Mutex<(String, String)>,
}

impl PrefixSink {
    pub fn new(prefix: impl Into<String>, inner: Arc<dyn Sink>) -> PrefixSink {
        PrefixSink { prefix: prefix.into(), inner, buf: Mutex::new((String::new(), String::new())) }
    }
    /// Emit any partial trailing line.
    pub fn flush(&self) {
        let mut b = lk(&self.buf);
        for (i, is_err) in [(0, false), (1, true)] {
            let s = if i == 0 { &mut b.0 } else { &mut b.1 };
            if !s.is_empty() {
                let line = std::mem::take(s);
                self.inner.write(&format!("[{}] {}\n", self.prefix, line), is_err);
            }
        }
    }
}

impl Sink for PrefixSink {
    fn write(&self, text: &str, is_err: bool) {
        let mut b = lk(&self.buf);
        let buf = if is_err { &mut b.1 } else { &mut b.0 };
        buf.push_str(text);
        while let Some(i) = buf.find('\n') {
            let line: String = buf.drain(..=i).collect();
            self.inner.write(&format!("[{}] {}\n", self.prefix, line.trim_end_matches(['\n', '\r'])), is_err);
        }
    }
}
