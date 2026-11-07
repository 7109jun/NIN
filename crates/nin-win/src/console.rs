//! Console helpers (VT mode, UTF-8, exclusive-console detection, Ctrl+C).

use std::io::{self, BufRead, Write};
use std::sync::atomic::AtomicBool;

#[cfg(windows)]
mod imp {
    use super::*;
    use std::sync::atomic::Ordering;
    use windows_sys::Win32::Foundation::{BOOL, FALSE, TRUE};
    use windows_sys::Win32::System::Console::*;

    pub fn is_exclusive_console() -> bool {
        let mut list = [0u32; 4];
        // SAFETY: buffer pointer/length are valid.
        let n = unsafe { GetConsoleProcessList(list.as_mut_ptr(), list.len() as u32) };
        n == 1
    }

    pub fn enable_vt() -> bool {
        // SAFETY: standard console API usage with handles obtained from GetStdHandle.
        unsafe {
            let mut ok = false;
            for std in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
                let h = GetStdHandle(std);
                let mut mode = 0u32;
                if GetConsoleMode(h, &mut mode) != 0 && SetConsoleMode(h, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) != 0 {
                    ok = true;
                }
            }
            ok
        }
    }

    pub fn set_utf8_codepage() {
        // SAFETY: simple Win32 calls.
        unsafe {
            SetConsoleOutputCP(65001);
            SetConsoleCP(65001);
        }
    }

    pub fn set_title(title: &str) {
        let w = crate::winutil::wide(title);
        // SAFETY: `w` is NUL-terminated.
        unsafe { SetConsoleTitleW(w.as_ptr()) };
    }

    pub fn terminal_width() -> usize {
        // SAFETY: zeroed struct is a valid output buffer.
        unsafe {
            let h = GetStdHandle(STD_OUTPUT_HANDLE);
            let mut info: CONSOLE_SCREEN_BUFFER_INFO = std::mem::zeroed();
            if GetConsoleScreenBufferInfo(h, &mut info) != 0 {
                let w = (info.srWindow.Right - info.srWindow.Left + 1) as i32;
                if w > 0 {
                    return w as usize;
                }
            }
        }
        80
    }

    pub fn is_terminal_stdout() -> bool {
        // SAFETY: handle from GetStdHandle.
        unsafe {
            let h = GetStdHandle(STD_OUTPUT_HANDLE);
            let mut mode = 0u32;
            GetConsoleMode(h, &mut mode) != 0
        }
    }

    static FLAG: std::sync::atomic::AtomicPtr<AtomicBool> = std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

    unsafe extern "system" fn handler(ty: u32) -> BOOL {
        if ty == CTRL_C_EVENT || ty == CTRL_BREAK_EVENT || ty == CTRL_CLOSE_EVENT {
            let p = FLAG.load(Ordering::SeqCst);
            if !p.is_null() {
                (*p).store(true, Ordering::SeqCst);
                return TRUE;
            }
        }
        FALSE
    }

    pub fn install_ctrlc_handler(flag: &'static AtomicBool) -> io::Result<()> {
        FLAG.store(flag as *const AtomicBool as *mut AtomicBool, Ordering::SeqCst);
        // SAFETY: `handler` has the required signature and 'static lifetime.
        let ok = unsafe { SetConsoleCtrlHandler(Some(handler), TRUE) };
        if ok == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;
    pub fn is_exclusive_console() -> bool {
        false
    }
    pub fn enable_vt() -> bool {
        true
    }
    pub fn set_utf8_codepage() {}
    pub fn set_title(title: &str) {
        if is_terminal_stdout() {
            print!("\x1b]0;{}\x07", title);
            let _ = io::stdout().flush();
        }
    }
    pub fn terminal_width() -> usize {
        std::env::var("COLUMNS").ok().and_then(|c| c.parse().ok()).unwrap_or(80)
    }
    pub fn is_terminal_stdout() -> bool {
        std::fs::read_link("/proc/self/fd/1").map(|p| p.to_string_lossy().starts_with("/dev/pts") || p.to_string_lossy().starts_with("/dev/tty")).unwrap_or(false)
    }
    pub fn install_ctrlc_handler(_flag: &'static AtomicBool) -> io::Result<()> {
        Ok(())
    }
}

/// True when this process is the only one attached to its console — i.e. it was started by
/// double-click / Explorer and the window will vanish on exit.
pub fn is_exclusive_console() -> bool {
    imp::is_exclusive_console()
}
pub fn enable_vt() -> bool {
    imp::enable_vt()
}
pub fn set_utf8_codepage() {
    imp::set_utf8_codepage()
}
pub fn set_title(title: &str) {
    imp::set_title(title)
}
pub fn terminal_width() -> usize {
    imp::terminal_width()
}
pub fn is_terminal_stdout() -> bool {
    imp::is_terminal_stdout()
}
/// Set `flag` when the user presses Ctrl+C / Ctrl+Break / closes the console.
pub fn install_ctrlc_handler(flag: &'static AtomicBool) -> io::Result<()> {
    imp::install_ctrlc_handler(flag)
}

/// Print `message` and wait for Enter.
pub fn pause_for_key(message: &str) {
    print!("{}", message);
    let _ = io::stdout().flush();
    let mut line = String::new();
    let _ = io::stdin().lock().read_line(&mut line);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers_do_not_panic() {
        let _ = is_exclusive_console();
        let _ = enable_vt();
        set_utf8_codepage();
        assert!(terminal_width() >= 1);
        let _ = is_terminal_stdout();
        static F: AtomicBool = AtomicBool::new(false);
        assert!(install_ctrlc_handler(&F).is_ok());
    }
}
