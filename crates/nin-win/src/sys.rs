//! System information, process listing and well-known directories.

use std::io;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct ProcInfo {
    pub pid: u32,
    pub ppid: u32,
    pub name: String,
    pub threads: u32,
}

#[derive(Clone, Debug)]
pub struct SysInfo {
    pub os: String,
    pub arch: String,
    pub hostname: String,
    pub username: String,
    pub cpu_count: usize,
    pub total_memory: u64,
    pub free_memory: u64,
    pub os_version: String,
    pub uptime_secs: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KnownDir {
    Home,
    AppData,
    LocalAppData,
    Desktop,
    Temp,
    Documents,
    Downloads,
}

pub fn env_vars() -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = std::env::vars_os().map(|(k, v)| (k.to_string_lossy().into_owned(), v.to_string_lossy().into_owned())).collect();
    v.sort_by(|a, b| a.0.to_ascii_lowercase().cmp(&b.0.to_ascii_lowercase()));
    v
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from)
}

pub fn known_dir(kind: KnownDir) -> Option<PathBuf> {
    let home = || env_path("USERPROFILE").or_else(|| env_path("HOME"));
    match kind {
        KnownDir::Home => home(),
        KnownDir::AppData => env_path("APPDATA").or_else(|| env_path("XDG_CONFIG_HOME")).or_else(|| home().map(|h| h.join(".config"))),
        KnownDir::LocalAppData => env_path("LOCALAPPDATA").or_else(|| env_path("XDG_DATA_HOME")).or_else(|| home().map(|h| h.join(".local").join("share"))),
        KnownDir::Desktop => home().map(|h| h.join("Desktop")),
        KnownDir::Documents => home().map(|h| h.join("Documents")),
        KnownDir::Downloads => home().map(|h| h.join("Downloads")),
        KnownDir::Temp => Some(std::env::temp_dir()),
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use crate::winutil::Handle;
    use windows_sys::Win32::System::Diagnostics::ToolHelp::*;
    use windows_sys::Win32::System::SystemInformation::*;
    use windows_sys::Win32::System::Threading::*;

    pub fn list_processes() -> io::Result<Vec<ProcInfo>> {
        // SAFETY: standard ToolHelp enumeration; PROCESSENTRY32W is zero-initialised with dwSize set.
        unsafe {
            let snap = Handle(CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0));
            if !snap.is_valid() {
                return Err(io::Error::last_os_error());
            }
            let mut e: PROCESSENTRY32W = std::mem::zeroed();
            e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            let mut out = Vec::new();
            if Process32FirstW(snap.0, &mut e) != 0 {
                loop {
                    let n = e.szExeFile.iter().position(|c| *c == 0).unwrap_or(e.szExeFile.len());
                    out.push(ProcInfo { pid: e.th32ProcessID, ppid: e.th32ParentProcessID, name: String::from_utf16_lossy(&e.szExeFile[..n]), threads: e.cntThreads });
                    if Process32NextW(snap.0, &mut e) == 0 {
                        break;
                    }
                }
            }
            Ok(out)
        }
    }

    pub fn kill_pid(pid: u32) -> io::Result<()> {
        // SAFETY: OpenProcess/TerminateProcess with a handle owned by RAII.
        unsafe {
            let h = Handle(OpenProcess(PROCESS_TERMINATE, 0, pid));
            if !h.is_valid() {
                return Err(io::Error::last_os_error());
            }
            if TerminateProcess(h.0, 1) == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
    }

    pub fn sys_info() -> SysInfo {
        // SAFETY: MEMORYSTATUSEX is initialised with dwLength; other calls write into local buffers.
        unsafe {
            let mut m: MEMORYSTATUSEX = std::mem::zeroed();
            m.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
            let (total, free) = if GlobalMemoryStatusEx(&mut m) != 0 { (m.ullTotalPhys, m.ullAvailPhys) } else { (0, 0) };
            let mut buf = [0u16; 256];
            let mut n = buf.len() as u32;
            let hostname = if GetComputerNameExW(ComputerNameDnsHostname, buf.as_mut_ptr(), &mut n) != 0 { String::from_utf16_lossy(&buf[..n as usize]) } else { std::env::var("COMPUTERNAME").unwrap_or_default() };
            let mut vi: OSVERSIONINFOW = std::mem::zeroed();
            vi.dwOSVersionInfoSize = std::mem::size_of::<OSVERSIONINFOW>() as u32;
            let version = rtl_version().unwrap_or_else(|| "unknown".to_string());
            let _ = vi;
            SysInfo {
                os: "windows".to_string(),
                arch: std::env::consts::ARCH.to_string(),
                hostname,
                username: std::env::var("USERNAME").unwrap_or_default(),
                cpu_count: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1),
                total_memory: total,
                free_memory: free,
                os_version: version,
                uptime_secs: GetTickCount64() / 1000,
            }
        }
    }

    /// `RtlGetVersion` is not subject to the manifest-based version lie of `GetVersionEx`.
    unsafe fn rtl_version() -> Option<String> {
        #[repr(C)]
        struct VersionInfo {
            size: u32,
            major: u32,
            minor: u32,
            build: u32,
            platform: u32,
            csd: [u16; 128],
        }
        #[link(name = "ntdll")]
        extern "system" {
            fn RtlGetVersion(info: *mut VersionInfo) -> i32;
        }
        let mut v: VersionInfo = std::mem::zeroed();
        v.size = std::mem::size_of::<VersionInfo>() as u32;
        if RtlGetVersion(&mut v) == 0 {
            Some(format!("{}.{}.{}", v.major, v.minor, v.build))
        } else {
            None
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    pub fn list_processes() -> io::Result<Vec<ProcInfo>> {
        let mut out = Vec::new();
        for e in std::fs::read_dir("/proc")? {
            let e = match e {
                Ok(e) => e,
                Err(_) => continue,
            };
            let pid: u32 = match e.file_name().to_string_lossy().parse() {
                Ok(p) => p,
                Err(_) => continue,
            };
            let stat = match std::fs::read_to_string(format!("/proc/{}/stat", pid)) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let (l, r) = match (stat.find('('), stat.rfind(')')) {
                (Some(l), Some(r)) if r > l => (l, r),
                _ => continue,
            };
            let name = stat[l + 1..r].to_string();
            let rest: Vec<&str> = stat[r + 1..].split_whitespace().collect();
            let ppid = rest.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
            let threads = rest.get(17).and_then(|s| s.parse().ok()).unwrap_or(1);
            out.push(ProcInfo { pid, ppid, name, threads });
        }
        Ok(out)
    }

    pub fn kill_pid(pid: u32) -> io::Result<()> {
        let st = std::process::Command::new("kill").arg("-KILL").arg(pid.to_string()).status()?;
        if st.success() {
            Ok(())
        } else {
            Err(io::Error::new(io::ErrorKind::NotFound, format!("cannot kill process {}", pid)))
        }
    }

    fn meminfo(key: &str) -> u64 {
        std::fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|s| s.lines().find(|l| l.starts_with(key)).and_then(|l| l.split_whitespace().nth(1).and_then(|v| v.parse::<u64>().ok())))
            .map(|kb| kb * 1024)
            .unwrap_or(0)
    }

    pub fn sys_info() -> SysInfo {
        SysInfo {
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            hostname: std::fs::read_to_string("/etc/hostname").map(|s| s.trim().to_string()).unwrap_or_else(|_| std::env::var("HOSTNAME").unwrap_or_default()),
            username: std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_default(),
            cpu_count: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1),
            total_memory: meminfo("MemTotal:"),
            free_memory: meminfo("MemAvailable:"),
            os_version: std::fs::read_to_string("/proc/sys/kernel/osrelease").map(|s| s.trim().to_string()).unwrap_or_default(),
            uptime_secs: std::fs::read_to_string("/proc/uptime").ok().and_then(|s| s.split_whitespace().next().and_then(|v| v.parse::<f64>().ok())).map(|f| f as u64).unwrap_or(0),
        }
    }
}

pub fn list_processes() -> io::Result<Vec<ProcInfo>> {
    imp::list_processes()
}

pub fn kill_pid(pid: u32) -> io::Result<()> {
    imp::kill_pid(pid)
}

/// Processes whose image name equals `name` (case-insensitive; `.exe` optional on Windows).
pub fn find_processes(name: &str) -> Vec<ProcInfo> {
    let want = name.to_ascii_lowercase();
    let want_noext = want.strip_suffix(".exe").unwrap_or(&want).to_string();
    list_processes()
        .unwrap_or_default()
        .into_iter()
        .filter(|p| {
            let n = p.name.to_ascii_lowercase();
            n == want || n.strip_suffix(".exe").unwrap_or(&n) == want_noext
        })
        .collect()
}

pub fn sys_info() -> SysInfo {
    imp::sys_info()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sysinfo_is_populated() {
        let s = sys_info();
        assert!(!s.os.is_empty() && !s.arch.is_empty());
        assert!(s.cpu_count >= 1);
        assert!(s.total_memory > 0, "{:?}", s);
    }

    #[test]
    fn process_list_contains_self() {
        let me = std::process::id();
        let all = list_processes().unwrap();
        let p = all.iter().find(|p| p.pid == me);
        assert!(p.is_some(), "own pid {} not listed among {} processes", me, all.len());
    }

    #[test]
    fn find_by_name_is_case_insensitive() {
        let exe = std::env::current_exe().unwrap();
        let stem = exe.file_stem().unwrap().to_string_lossy().to_string();
        // process names may be truncated (Linux: 15 chars)
        let short: String = stem.chars().take(15).collect();
        assert!(!find_processes(&short.to_ascii_uppercase()).is_empty() || !find_processes(&stem).is_empty());
    }

    #[test]
    fn known_dirs() {
        assert!(known_dir(KnownDir::Temp).is_some());
        assert!(known_dir(KnownDir::Home).is_some());
        assert!(env_vars().iter().any(|(k, _)| k.eq_ignore_ascii_case("PATH")));
    }

    #[test]
    fn kill_pid_terminates_child() {
        let mut c = if cfg!(windows) { std::process::Command::new("ping").args(["-n", "30", "127.0.0.1"]).stdout(std::process::Stdio::null()).spawn().unwrap() } else { std::process::Command::new("sleep").arg("30").spawn().unwrap() };
        kill_pid(c.id()).unwrap();
        let st = c.wait().unwrap();
        assert!(!st.success());
    }
}
