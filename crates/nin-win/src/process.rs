//! Process execution. Child processes are created with `std::process::Command`
//! (which uses `CreateProcessW` with correct `CommandLineToArgvW` quoting on Windows)
//! and, on Windows, assigned to a Job Object that provides tree-kill, kill-on-close
//! and memory / process-count / CPU-time limits.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub enum StdinSpec {
    Inherit,
    Null,
    Text(String),
    Bytes(Vec<u8>),
    File(PathBuf),
}

#[derive(Clone, Debug)]
pub enum OutSpec {
    Inherit,
    Null,
    Capture,
    File { path: PathBuf, append: bool },
    /// stderr only: interleave into the captured stdout stream
    MergeIntoStdout,
}

#[derive(Clone, Debug)]
pub struct ProcSpec {
    pub program: String,
    pub args: Vec<String>,
    /// when set, the process is a shell command line (`cmd.exe /d /s /c "<line>"` on Windows, `sh -c` elsewhere)
    pub shell_line: Option<String>,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    pub clear_env: bool,
    pub stdin: StdinSpec,
    pub stdout: OutSpec,
    pub stderr: OutSpec,
    pub hide_window: bool,
}

impl Default for ProcSpec {
    fn default() -> Self {
        ProcSpec {
            program: String::new(),
            args: Vec::new(),
            shell_line: None,
            cwd: None,
            env: Vec::new(),
            clear_env: false,
            stdin: StdinSpec::Inherit,
            stdout: OutSpec::Inherit,
            stderr: OutSpec::Inherit,
            hide_window: false,
        }
    }
}

impl ProcSpec {
    pub fn new(program: impl Into<String>) -> ProcSpec {
        ProcSpec { program: program.into(), ..Default::default() }
    }
    pub fn shell(line: impl Into<String>) -> ProcSpec {
        ProcSpec { shell_line: Some(line.into()), ..Default::default() }
    }
    pub fn arg(mut self, a: impl Into<String>) -> ProcSpec {
        self.args.push(a.into());
        self
    }
    pub fn args<I, S>(mut self, it: I) -> ProcSpec
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(it.into_iter().map(Into::into));
        self
    }
    pub fn cwd(mut self, p: impl Into<PathBuf>) -> ProcSpec {
        self.cwd = Some(p.into());
        self
    }
    pub fn env(mut self, k: impl Into<String>, v: impl Into<String>) -> ProcSpec {
        self.env.push((k.into(), v.into()));
        self
    }
    pub fn capture(mut self) -> ProcSpec {
        self.stdout = OutSpec::Capture;
        self.stderr = OutSpec::Capture;
        self
    }
    pub fn stdin(mut self, s: StdinSpec) -> ProcSpec {
        self.stdin = s;
        self
    }
}

#[derive(Clone, Debug, Default)]
pub struct Limits {
    pub memory_bytes: Option<u64>,
    pub max_processes: Option<u32>,
    pub cpu_time_ms: Option<u64>,
    pub kill_on_close: bool,
}

#[derive(Clone, Debug)]
pub struct ProcResult {
    pub pid: u32,
    pub code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub duration: Duration,
    pub timed_out: bool,
    pub cancelled: bool,
    pub limit_exceeded: bool,
}

impl ProcResult {
    pub fn ok(&self) -> bool {
        self.code == 0 && !self.timed_out && !self.cancelled
    }
    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }
    pub fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}

// ───────────────────────────── quoting ─────────────────────────────

/// Quote one argument so that `CommandLineToArgvW` / the MSVCRT parser reads it back unchanged.
pub fn quote_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.chars().any(|c| c == ' ' || c == '\t' || c == '\n' || c == '\x0b' || c == '"') {
        return arg.to_string();
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    let mut backslashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.extend(std::iter::repeat('\\').take(backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat('\\').take(backslashes));
                backslashes = 0;
                out.push(c);
            }
        }
    }
    out.extend(std::iter::repeat('\\').take(backslashes * 2));
    out.push('"');
    out
}

pub fn build_command_line(program: &str, args: &[String]) -> String {
    let mut s = quote_arg(program);
    for a in args {
        s.push(' ');
        s.push_str(&quote_arg(a));
    }
    s
}

// ───────────────────────────── program lookup ─────────────────────────────

#[cfg(windows)]
fn path_exts() -> Vec<String> {
    let v = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
    v.split(';').filter(|s| !s.is_empty()).map(|s| s.to_ascii_lowercase()).collect()
}

fn is_file(p: &Path) -> bool {
    std::fs::metadata(p).map(|m| m.is_file()).unwrap_or(false)
}

fn try_with_exts(base: &Path) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        if base.extension().is_some() && is_file(base) {
            return Some(base.to_path_buf());
        }
        for ext in path_exts() {
            let mut s = base.as_os_str().to_os_string();
            s.push(&ext);
            let p = PathBuf::from(s);
            if is_file(&p) {
                return Some(p);
            }
        }
        if is_file(base) {
            return Some(base.to_path_buf());
        }
        None
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;
        let ok = std::fs::metadata(base).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false);
        if ok {
            Some(base.to_path_buf())
        } else {
            None
        }
    }
}

/// Resolve a program name: explicit paths (absolute or containing a separator) are checked
/// against `cwd`; bare names are searched in `cwd` (Windows only) and `PATH` (× `PATHEXT` on Windows).
pub fn resolve_program(name: &str, cwd: Option<&Path>) -> Option<PathBuf> {
    if name.is_empty() {
        return None;
    }
    let p = Path::new(name);
    let has_sep = name.contains('/') || name.contains('\\');
    if p.is_absolute() || has_sep {
        let full = if p.is_absolute() {
            p.to_path_buf()
        } else {
            match cwd {
                Some(c) => c.join(p),
                None => p.to_path_buf(),
            }
        };
        return try_with_exts(&full);
    }
    #[cfg(windows)]
    {
        let dir = cwd.map(|c| c.to_path_buf()).or_else(|| std::env::current_dir().ok());
        if let Some(d) = dir {
            if let Some(f) = try_with_exts(&d.join(name)) {
                return Some(f);
            }
        }
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        if let Some(f) = try_with_exts(&dir.join(name)) {
            return Some(f);
        }
    }
    None
}

// ───────────────────────────── job object / process group ─────────────────────────────

struct GroupInner {
    limits: Limits,
    #[cfg(windows)]
    job: crate::winutil::Handle,
    #[cfg(not(windows))]
    pids: std::sync::Mutex<Vec<u32>>,
}

#[cfg(windows)]
mod job {
    use super::*;
    use crate::winutil::Handle;
    use std::mem::{size_of, zeroed};
    use windows_sys::Win32::System::JobObjects::*;

    pub fn create(limits: &Limits) -> io::Result<Handle> {
        // SAFETY: plain Win32 calls with properly initialised structures; the handle is wrapped in RAII.
        unsafe {
            let h = Handle(CreateJobObjectW(std::ptr::null(), std::ptr::null()));
            if !h.is_valid() {
                return Err(io::Error::last_os_error());
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = zeroed();
            let mut flags = 0u32;
            if limits.kill_on_close {
                flags |= JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            }
            if let Some(m) = limits.memory_bytes {
                flags |= JOB_OBJECT_LIMIT_JOB_MEMORY;
                info.JobMemoryLimit = m as usize;
            }
            if let Some(n) = limits.max_processes {
                flags |= JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
                info.BasicLimitInformation.ActiveProcessLimit = n;
            }
            if let Some(ms) = limits.cpu_time_ms {
                flags |= JOB_OBJECT_LIMIT_JOB_TIME;
                info.BasicLimitInformation.PerJobUserTimeLimit = (ms as i64).saturating_mul(10_000);
            }
            info.BasicLimitInformation.LimitFlags = flags;
            if flags != 0 {
                let ok = SetInformationJobObject(h.0, JobObjectExtendedLimitInformation, &info as *const _ as *const _, size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32);
                if ok == 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(h)
        }
    }

    pub fn assign(job: &Handle, process: *mut std::ffi::c_void) -> io::Result<()> {
        // SAFETY: `process` is a live process handle owned by the std Child.
        let ok = unsafe { AssignProcessToJobObject(job.0, process) };
        if ok == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    pub fn terminate(job: &Handle) {
        // SAFETY: valid job handle.
        unsafe { TerminateJobObject(job.0, 1) };
    }

    pub fn accounting(job: &Handle) -> Option<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION> {
        // SAFETY: the output buffer is a correctly sized, zero-initialised struct.
        unsafe {
            let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = zeroed();
            let ok = QueryInformationJobObject(job.0, JobObjectBasicAccountingInformation, &mut info as *mut _ as *mut _, size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32, std::ptr::null_mut());
            if ok == 0 {
                None
            } else {
                Some(info)
            }
        }
    }
}

/// A set of processes sharing limits and a common kill switch.
/// Windows: a Job Object. Elsewhere: tracked process groups.
#[derive(Clone)]
pub struct ProcessGroup {
    inner: Arc<GroupInner>,
}

impl ProcessGroup {
    pub fn new(limits: Limits) -> io::Result<ProcessGroup> {
        #[cfg(windows)]
        {
            let job = job::create(&limits)?;
            Ok(ProcessGroup { inner: Arc::new(GroupInner { limits, job }) })
        }
        #[cfg(not(windows))]
        {
            Ok(ProcessGroup { inner: Arc::new(GroupInner { limits, pids: std::sync::Mutex::new(Vec::new()) }) })
        }
    }

    pub fn limits(&self) -> &Limits {
        &self.inner.limits
    }

    pub fn run(&self, spec: &ProcSpec, timeout: Option<Duration>, cancel: Option<&AtomicBool>) -> io::Result<ProcResult> {
        let mut c = self.spawn(spec)?;
        c.wait(timeout, cancel)
    }

    pub fn spawn(&self, spec: &ProcSpec) -> io::Result<Child> {
        spawn_inner(Some(self.inner.clone()), spec)
    }

    /// Terminate every process of the group (and their descendants).
    pub fn kill_all(&self) {
        self.inner.kill_all();
    }

    pub fn active_processes(&self) -> u32 {
        self.inner.active()
    }
}

impl GroupInner {
    fn kill_all(&self) {
        #[cfg(windows)]
        job::terminate(&self.job);
        #[cfg(not(windows))]
        {
            if let Ok(pids) = self.pids.lock() {
                for p in pids.iter() {
                    unix_kill_group(*p);
                }
            }
        }
    }

    fn active(&self) -> u32 {
        #[cfg(windows)]
        {
            job::accounting(&self.job).map(|a| a.ActiveProcesses).unwrap_or(0)
        }
        #[cfg(not(windows))]
        {
            match self.pids.lock() {
                Ok(p) => p.iter().filter(|pid| std::path::Path::new(&format!("/proc/{}", pid)).exists() && !unix_is_zombie(**pid)).count() as u32,
                Err(_) => 0,
            }
        }
    }

    fn limit_exceeded(&self) -> bool {
        #[cfg(windows)]
        {
            job::accounting(&self.job).map(|a| a.TotalTerminatedProcesses > 0).unwrap_or(false)
        }
        #[cfg(not(windows))]
        {
            false
        }
    }
}

#[cfg(not(windows))]
fn unix_is_zombie(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{}/stat", pid)).map(|s| s.rsplit(") ").next().map_or(false, |r| r.starts_with('Z'))).unwrap_or(false)
}

#[cfg(not(windows))]
fn unix_kill_group(pid: u32) {
    let _ = Command::new("kill").arg("-KILL").arg(format!("-{}", pid)).stdout(Stdio::null()).stderr(Stdio::null()).status();
    let _ = Command::new("kill").arg("-KILL").arg(pid.to_string()).stdout(Stdio::null()).stderr(Stdio::null()).status();
}

// ───────────────────────────── child ─────────────────────────────

enum Msg {
    Out(Vec<u8>),
    Err(Vec<u8>),
    Eof,
}

pub struct Child {
    std: std::process::Child,
    pid: u32,
    start: Instant,
    group: Option<Arc<GroupInner>>,
    rx: Receiver<Msg>,
    readers: usize,
    eofs: usize,
    stdin: Option<std::process::ChildStdin>,
    merge: bool,
    out: Vec<u8>,
    err: Vec<u8>,
    timed_out: bool,
    cancelled: bool,
    done: Option<ProcResult>,
}

fn spawn_reader<R: Read + Send + 'static>(mut r: R, tx: mpsc::Sender<Msg>, is_err: bool) {
    thread::spawn(move || {
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            match r.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let chunk = buf[..n].to_vec();
                    let msg = if is_err { Msg::Err(chunk) } else { Msg::Out(chunk) };
                    if tx.send(msg).is_err() {
                        return;
                    }
                }
            }
        }
        let _ = tx.send(Msg::Eof);
    });
}

fn open_out(path: &Path, append: bool) -> io::Result<std::fs::File> {
    let mut o = std::fs::OpenOptions::new();
    o.create(true).write(true);
    if append {
        o.append(true);
    } else {
        o.truncate(true);
    }
    o.open(path)
}

fn spawn_inner(group: Option<Arc<GroupInner>>, spec: &ProcSpec) -> io::Result<Child> {
    let mut cmd = if let Some(line) = &spec.shell_line {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            let mut c = Command::new("cmd.exe");
            c.raw_arg(format!("/d /s /c \"{}\"", line));
            c
        }
        #[cfg(not(windows))]
        {
            let mut c = Command::new("sh");
            c.arg("-c").arg(line);
            c
        }
    } else {
        let resolved = resolve_program(&spec.program, spec.cwd.as_deref());
        let prog = match resolved {
            Some(p) => p,
            None => return Err(io::Error::new(io::ErrorKind::NotFound, format!("program `{}` was not found (searched PATH{})", spec.program, if cfg!(windows) { " and PATHEXT" } else { "" }))),
        };
        let mut c = Command::new(prog);
        c.args(&spec.args);
        c
    };
    if let Some(d) = &spec.cwd {
        cmd.current_dir(d);
    }
    if spec.clear_env {
        cmd.env_clear();
    }
    for (k, v) in &spec.env {
        cmd.env(k, v);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        if spec.hide_window {
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    let mut stdin_data: Option<Vec<u8>> = None;
    match &spec.stdin {
        StdinSpec::Inherit => {
            cmd.stdin(Stdio::inherit());
        }
        StdinSpec::Null => {
            cmd.stdin(Stdio::null());
        }
        StdinSpec::Text(t) => {
            cmd.stdin(Stdio::piped());
            stdin_data = Some(t.clone().into_bytes());
        }
        StdinSpec::Bytes(b) => {
            cmd.stdin(Stdio::piped());
            stdin_data = Some(b.clone());
        }
        StdinSpec::File(p) => {
            cmd.stdin(Stdio::from(std::fs::File::open(p)?));
        }
    }
    let merge = matches!(spec.stderr, OutSpec::MergeIntoStdout);
    let mut read_out = false;
    let mut read_err = false;
    match &spec.stdout {
        OutSpec::Inherit => {
            cmd.stdout(Stdio::inherit());
        }
        OutSpec::Null => {
            cmd.stdout(Stdio::null());
        }
        OutSpec::Capture => {
            cmd.stdout(Stdio::piped());
            read_out = true;
        }
        OutSpec::File { path, append } => {
            cmd.stdout(Stdio::from(open_out(path, *append)?));
        }
        OutSpec::MergeIntoStdout => {
            cmd.stdout(Stdio::piped());
            read_out = true;
        }
    }
    match &spec.stderr {
        OutSpec::Inherit => {
            cmd.stderr(Stdio::inherit());
        }
        OutSpec::Null => {
            cmd.stderr(Stdio::null());
        }
        OutSpec::Capture | OutSpec::MergeIntoStdout => {
            cmd.stderr(Stdio::piped());
            read_err = true;
        }
        OutSpec::File { path, append } => {
            cmd.stderr(Stdio::from(open_out(path, *append)?));
        }
    }

    let start = Instant::now();
    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            io::Error::new(io::ErrorKind::NotFound, format!("program `{}` was not found", spec.program))
        } else {
            e
        }
    })?;
    let pid = child.id();
    if let Some(g) = &group {
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            // A tiny window exists in which the child could spawn grandchildren before it joins the job.
            if let Err(e) = job::assign(&g.job, child.as_raw_handle() as *mut _) {
                let _ = child.kill();
                return Err(io::Error::new(e.kind(), format!("cannot assign process to job object: {}", e)));
            }
        }
        #[cfg(not(windows))]
        {
            if let Ok(mut p) = g.pids.lock() {
                p.push(pid);
            }
        }
    }
    let (tx, rx) = mpsc::channel();
    let mut readers = 0;
    if read_out {
        if let Some(o) = child.stdout.take() {
            spawn_reader(o, tx.clone(), false);
            readers += 1;
        }
    }
    if read_err {
        if let Some(e) = child.stderr.take() {
            spawn_reader(e, tx.clone(), true);
            readers += 1;
        }
    }
    let mut stdin = child.stdin.take();
    if let Some(data) = stdin_data {
        if let Some(mut si) = stdin.take() {
            thread::spawn(move || {
                let _ = si.write_all(&data);
                // dropping closes the pipe → EOF for the child
            });
        }
    }
    drop(tx);
    Ok(Child { std: child, pid, start, group, rx, readers, eofs: 0, stdin, merge, out: Vec::new(), err: Vec::new(), timed_out: false, cancelled: false, done: None })
}

impl Child {
    pub fn pid(&self) -> u32 {
        self.pid
    }

    fn absorb(&mut self, m: Msg) {
        match m {
            Msg::Out(b) => self.out.extend_from_slice(&b),
            Msg::Err(b) => {
                if self.merge {
                    self.out.extend_from_slice(&b)
                } else {
                    self.err.extend_from_slice(&b)
                }
            }
            Msg::Eof => self.eofs += 1,
        }
    }

    fn drain_nonblocking(&mut self) {
        while let Ok(m) = self.rx.try_recv() {
            self.absorb(m);
        }
    }

    /// Kill the process and everything it started.
    pub fn kill(&mut self) -> io::Result<()> {
        match &self.group {
            Some(g) => {
                #[cfg(windows)]
                job::terminate(&g.job);
                #[cfg(not(windows))]
                unix_kill_group(self.pid);
                #[cfg(not(windows))]
                let _ = g;
            }
            None => {}
        }
        match self.std.kill() {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::InvalidInput => Ok(()), // already exited
            Err(e) => Err(e),
        }
    }

    pub fn write_stdin(&mut self, data: &[u8]) -> io::Result<()> {
        match self.stdin.as_mut() {
            Some(s) => {
                s.write_all(data)?;
                s.flush()
            }
            None => Err(io::Error::new(io::ErrorKind::BrokenPipe, "stdin is not piped or already closed")),
        }
    }

    pub fn close_stdin(&mut self) {
        self.stdin = None;
    }

    fn finish(&mut self, status: std::process::ExitStatus) -> ProcResult {
        // Collect remaining output; descendants that keep the pipes open must not hang us forever.
        let deadline = Instant::now() + Duration::from_millis(if self.timed_out || self.cancelled { 300 } else { 1500 });
        while self.eofs < self.readers {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            match self.rx.recv_timeout(deadline - now) {
                Ok(m) => self.absorb(m),
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        self.drain_nonblocking();
        let code = match status.code() {
            Some(c) => c,
            None => {
                #[cfg(unix)]
                {
                    use std::os::unix::process::ExitStatusExt;
                    status.signal().map(|s| 128 + s).unwrap_or(-1)
                }
                #[cfg(not(unix))]
                {
                    -1
                }
            }
        };
        let limit_exceeded = self.group.as_ref().map(|g| g.limit_exceeded()).unwrap_or(false);
        ProcResult {
            pid: self.pid,
            code: if self.timed_out || self.cancelled { -1 } else { code },
            stdout: std::mem::take(&mut self.out),
            stderr: std::mem::take(&mut self.err),
            duration: self.start.elapsed(),
            timed_out: self.timed_out,
            cancelled: self.cancelled,
            limit_exceeded,
        }
    }

    /// Non-blocking poll.
    pub fn try_wait(&mut self) -> io::Result<Option<ProcResult>> {
        if let Some(r) = &self.done {
            return Ok(Some(r.clone()));
        }
        self.drain_nonblocking();
        match self.std.try_wait()? {
            Some(status) => {
                let r = self.finish(status);
                self.done = Some(r.clone());
                Ok(Some(r))
            }
            None => Ok(None),
        }
    }

    /// Wait for exit. On `timeout` or when `cancel` becomes true the whole process tree is killed.
    pub fn wait(&mut self, timeout: Option<Duration>, cancel: Option<&AtomicBool>) -> io::Result<ProcResult> {
        if let Some(r) = &self.done {
            return Ok(r.clone());
        }
        loop {
            match self.std.try_wait()? {
                Some(status) => {
                    let r = self.finish(status);
                    self.done = Some(r.clone());
                    return Ok(r);
                }
                None => {}
            }
            if !self.timed_out && !self.cancelled {
                if let Some(t) = timeout {
                    if self.start.elapsed() >= t {
                        self.timed_out = true;
                        self.kill()?;
                    }
                }
                if let Some(c) = cancel {
                    if c.load(Ordering::SeqCst) {
                        self.cancelled = true;
                        self.kill()?;
                    }
                }
            }
            match self.rx.recv_timeout(Duration::from_millis(10)) {
                Ok(m) => self.absorb(m),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => thread::sleep(Duration::from_millis(5)),
            }
        }
    }
}

/// Run a process to completion. A private job object guarantees that timeouts kill the whole tree.
pub fn run(spec: &ProcSpec, timeout: Option<Duration>, cancel: Option<&AtomicBool>) -> io::Result<ProcResult> {
    let g = ProcessGroup::new(Limits { kill_on_close: true, ..Default::default() })?;
    g.run(spec, timeout, cancel)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(line: &str) -> ProcSpec {
        ProcSpec::shell(line).capture()
    }

    #[test]
    fn quoting() {
        assert_eq!(quote_arg("abc"), "abc");
        assert_eq!(quote_arg(""), "\"\"");
        assert_eq!(quote_arg("a b"), "\"a b\"");
        assert_eq!(quote_arg("a\"b"), "\"a\\\"b\"");
        assert_eq!(quote_arg("a\\"), "a\\");
        assert_eq!(quote_arg("a b\\"), "\"a b\\\\\"");
        assert_eq!(quote_arg("a\\\"b c"), "\"a\\\\\\\"b c\"");
        assert_eq!(quote_arg("tab\there"), "\"tab\there\"");
        assert_eq!(quote_arg("한글 경로"), "\"한글 경로\"");
        assert_eq!(build_command_line("C:\\Program Files\\x.exe", &["a b".into(), "c".into()]), "\"C:\\Program Files\\x.exe\" \"a b\" c");
    }

    #[test]
    fn capture_stdout_stderr_and_exit_code() {
        let r = run(&sh("echo out&& echo err 1>&2&& exit 3"), None, None).unwrap();
        assert_eq!(r.code, 3);
        assert!(r.stdout_text().contains("out"));
        assert!(r.stderr_text().contains("err"));
        assert!(!r.ok());
    }

    #[test]
    fn merge_stderr_into_stdout() {
        let mut s = sh("echo a&& echo b 1>&2");
        s.stderr = OutSpec::MergeIntoStdout;
        let r = run(&s, None, None).unwrap();
        let t = r.stdout_text();
        assert!(t.contains('a') && t.contains('b'), "{:?}", t);
        assert!(r.stderr.is_empty());
    }

    #[test]
    fn not_found() {
        let e = run(&ProcSpec::new("definitely-not-a-program-xyz").capture(), None, None).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn stdin_text_is_delivered() {
        let line = if cfg!(windows) { "more" } else { "cat" };
        let s = ProcSpec::shell(line).capture().stdin(StdinSpec::Text("hello stdin\n".into()));
        let r = run(&s, Some(Duration::from_secs(20)), None).unwrap();
        assert!(r.stdout_text().contains("hello stdin"), "{:?}", r.stdout_text());
    }

    #[test]
    fn env_and_cwd() {
        let dir = std::env::temp_dir();
        let line = if cfg!(windows) { "echo %NIN_T%&& cd" } else { "echo $NIN_T; pwd" };
        let s = ProcSpec::shell(line).capture().env("NIN_T", "val42").cwd(dir.clone());
        let r = run(&s, None, None).unwrap();
        assert!(r.stdout_text().contains("val42"), "{:?}", r.stdout_text());
    }

    #[test]
    fn big_output_does_not_deadlock() {
        let path = std::env::temp_dir().join(format!("nin_big_{}.txt", std::process::id()));
        std::fs::write(&path, vec![b'x'; 3_000_000]).unwrap();
        let line = if cfg!(windows) { format!("type \"{}\"", path.display()) } else { format!("cat '{}'", path.display()) };
        let r = run(&sh(&line), Some(Duration::from_secs(60)), None).unwrap();
        let _ = std::fs::remove_file(&path);
        assert!(r.stdout.len() >= 3_000_000, "got {}", r.stdout.len());
    }

    fn sleeper() -> ProcSpec {
        let line = if cfg!(windows) { "ping -n 30 127.0.0.1 >nul" } else { "sleep 30" };
        sh(line)
    }

    #[test]
    fn timeout_kills_process_tree() {
        let t0 = Instant::now();
        let r = run(&sleeper(), Some(Duration::from_millis(400)), None).unwrap();
        assert!(r.timed_out);
        assert_eq!(r.code, -1);
        assert!(t0.elapsed() < Duration::from_secs(15), "took {:?}", t0.elapsed());
    }

    #[test]
    fn cancel_flag_kills() {
        let flag = Arc::new(AtomicBool::new(false));
        let f2 = flag.clone();
        let h = thread::spawn(move || {
            thread::sleep(Duration::from_millis(300));
            f2.store(true, Ordering::SeqCst);
        });
        let r = run(&sleeper(), None, Some(&flag)).unwrap();
        h.join().unwrap();
        assert!(r.cancelled);
    }

    #[test]
    fn group_kill_all_and_active_count() {
        let g = ProcessGroup::new(Limits { kill_on_close: true, ..Default::default() }).unwrap();
        let mut c = g.spawn(&sleeper()).unwrap();
        thread::sleep(Duration::from_millis(300));
        assert!(g.active_processes() >= 1);
        g.kill_all();
        let r = c.wait(Some(Duration::from_secs(20)), None).unwrap();
        assert!(!r.timed_out);
        assert!(c.try_wait().unwrap().is_some());
    }

    #[test]
    fn spawn_write_stdin_and_wait() {
        let line = if cfg!(windows) { "more" } else { "cat" };
        let mut s = ProcSpec::shell(line).capture();
        s.stdin = StdinSpec::Inherit;
        // piped stdin via explicit Bytes + close
        s.stdin = StdinSpec::Bytes(b"abc\n".to_vec());
        let mut c = ProcessGroup::new(Limits::default()).unwrap().spawn(&s).unwrap();
        let r = c.wait(Some(Duration::from_secs(20)), None).unwrap();
        assert!(r.stdout_text().contains("abc"));
    }

    #[test]
    fn file_redirects() {
        let p = std::env::temp_dir().join(format!("nin_redir_{}.txt", std::process::id()));
        let mut s = ProcSpec::shell("echo to-file");
        s.stdout = OutSpec::File { path: p.clone(), append: false };
        s.stderr = OutSpec::Null;
        let r = run(&s, None, None).unwrap();
        assert_eq!(r.code, 0);
        assert!(std::fs::read_to_string(&p).unwrap().contains("to-file"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn resolve_finds_shell_and_rejects_garbage() {
        let name = if cfg!(windows) { "cmd" } else { "sh" };
        assert!(resolve_program(name, None).is_some());
        assert!(resolve_program("no-such-prog-123", None).is_none());
        assert!(resolve_program("", None).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn exit_by_signal_is_reported() {
        let r = run(&sh("kill -9 $$"), None, None).unwrap();
        assert_eq!(r.code, 137);
    }
}
