//! `.nin` file association in `HKCU\Software\Classes` (no administrator rights needed).
//!
//! * `.nin` → ProgID `NIN.Script` (Content Type `text/x-nin`, PerceivedType `text`)
//! * `NIN.Script`: friendly name "NIN Script", icon `"<exe>",1` (icon resource #1 = document icon),
//!   `shell\open\command` = `"<exe>" run "%1"`, extra verb `edit` → notepad.

use std::io;
use std::path::Path;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AssocInfo {
    pub installed: bool,
    pub command: Option<String>,
    pub icon: Option<String>,
}

/// The registry entries that [`install_assoc`] writes, as `(subkey under HKCU\Software\Classes, value name, data)`.
/// Pure so it can be tested on every platform.
pub fn assoc_entries(exe: &Path) -> Vec<(String, String, String)> {
    let exe = exe.display().to_string();
    vec![
        (".nin".into(), "".into(), "NIN.Script".into()),
        (".nin".into(), "Content Type".into(), "text/x-nin".into()),
        (".nin".into(), "PerceivedType".into(), "text".into()),
        ("NIN.Script".into(), "".into(), "NIN Script".into()),
        ("NIN.Script".into(), "FriendlyTypeName".into(), "NIN Script".into()),
        ("NIN.Script\\DefaultIcon".into(), "".into(), format!("\"{}\",1", exe)),
        ("NIN.Script\\shell".into(), "".into(), "open".into()),
        ("NIN.Script\\shell\\open".into(), "".into(), "Run with NIN".into()),
        ("NIN.Script\\shell\\open\\command".into(), "".into(), format!("\"{}\" run \"%1\"", exe)),
        ("NIN.Script\\shell\\edit".into(), "".into(), "Edit".into()),
        ("NIN.Script\\shell\\edit\\command".into(), "".into(), "notepad.exe \"%1\"".into()),
    ]
}

#[cfg(windows)]
mod imp {
    use super::*;
    use crate::winutil::wide;
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::*;
    use windows_sys::Win32::UI::Shell::{SHChangeNotify, SHCNE_ASSOCCHANGED, SHCNF_IDLIST};

    const CLASSES: &str = "Software\\Classes\\";

    fn err(code: u32) -> io::Error {
        io::Error::from_raw_os_error(code as i32)
    }

    pub fn set_value(subkey: &str, name: &str, data: &str) -> io::Result<()> {
        let key = wide(format!("{}{}", CLASSES, subkey));
        let mut hkey: HKEY = std::ptr::null_mut();
        // SAFETY: all pointers refer to live, NUL-terminated buffers; the key handle is closed below.
        unsafe {
            let rc = RegCreateKeyExW(HKEY_CURRENT_USER, key.as_ptr(), 0, std::ptr::null(), 0, KEY_WRITE, std::ptr::null(), &mut hkey, std::ptr::null_mut());
            if rc != ERROR_SUCCESS {
                return Err(err(rc));
            }
            let wname = wide(name);
            let wdata = wide(data);
            let rc = RegSetValueExW(hkey, wname.as_ptr(), 0, REG_SZ, wdata.as_ptr() as *const u8, (wdata.len() * 2) as u32);
            RegCloseKey(hkey);
            if rc != ERROR_SUCCESS {
                return Err(err(rc));
            }
        }
        Ok(())
    }

    pub fn get_value(subkey: &str, name: &str) -> Option<String> {
        let key = wide(format!("{}{}", CLASSES, subkey));
        let wname = wide(name);
        let mut hkey: HKEY = std::ptr::null_mut();
        // SAFETY: buffers are valid for the sizes passed; handle closed before return.
        unsafe {
            if RegOpenKeyExW(HKEY_CURRENT_USER, key.as_ptr(), 0, KEY_READ, &mut hkey) != ERROR_SUCCESS {
                return None;
            }
            let mut size = 0u32;
            let mut ty = 0u32;
            let name_ptr = if name.is_empty() { std::ptr::null() } else { wname.as_ptr() };
            let mut result = None;
            if RegQueryValueExW(hkey, name_ptr, std::ptr::null(), &mut ty, std::ptr::null_mut(), &mut size) == ERROR_SUCCESS && ty == REG_SZ && size >= 2 {
                let mut buf = vec![0u16; (size as usize + 1) / 2];
                if RegQueryValueExW(hkey, name_ptr, std::ptr::null(), &mut ty, buf.as_mut_ptr() as *mut u8, &mut size) == ERROR_SUCCESS {
                    let n = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
                    result = Some(String::from_utf16_lossy(&buf[..n]));
                }
            }
            RegCloseKey(hkey);
            result
        }
    }

    pub fn delete_tree(subkey: &str) -> io::Result<()> {
        let key = wide(format!("{}{}", CLASSES, subkey));
        // SAFETY: valid NUL-terminated key path.
        let rc = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, key.as_ptr()) };
        if rc == ERROR_SUCCESS || rc == 2 {
            Ok(())
        } else {
            Err(err(rc))
        }
    }

    pub fn notify() {
        // SAFETY: documented usage with null item pointers.
        unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED as i32, SHCNF_IDLIST, std::ptr::null(), std::ptr::null()) };
    }
}

/// Register `.nin` for the current user so double-clicking runs `exe run "<file>"`.
pub fn install_assoc(exe: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        if !exe.is_absolute() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "the executable path must be absolute"));
        }
        for (k, n, v) in assoc_entries(exe) {
            imp::set_value(&k, &n, &v)?;
        }
        imp::notify();
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = exe;
        Err(io::Error::new(io::ErrorKind::Unsupported, "file associations are only supported on Windows"))
    }
}

/// Remove everything written by [`install_assoc`].
pub fn uninstall_assoc() -> io::Result<()> {
    #[cfg(windows)]
    {
        imp::delete_tree(".nin")?;
        imp::delete_tree("NIN.Script")?;
        imp::notify();
        Ok(())
    }
    #[cfg(not(windows))]
    {
        Err(io::Error::new(io::ErrorKind::Unsupported, "file associations are only supported on Windows"))
    }
}

pub fn assoc_status() -> AssocInfo {
    #[cfg(windows)]
    {
        let prog = imp::get_value(".nin", "");
        let command = imp::get_value("NIN.Script\\shell\\open\\command", "");
        let icon = imp::get_value("NIN.Script\\DefaultIcon", "");
        AssocInfo { installed: prog.as_deref() == Some("NIN.Script") && command.is_some(), command, icon }
    }
    #[cfg(not(windows))]
    {
        AssocInfo::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_are_complete() {
        let e = assoc_entries(Path::new("C:\\Tools\\nin.exe"));
        assert!(e.iter().any(|(k, n, v)| k == ".nin" && n.is_empty() && v == "NIN.Script"));
        assert!(e.iter().any(|(k, _, v)| k == "NIN.Script\\shell\\open\\command" && v == "\"C:\\Tools\\nin.exe\" run \"%1\""));
        assert!(e.iter().any(|(k, _, v)| k == "NIN.Script\\DefaultIcon" && v == "\"C:\\Tools\\nin.exe\",1"));
        assert!(e.iter().any(|(_, n, v)| n == "Content Type" && v == "text/x-nin"));
    }

    #[cfg(not(windows))]
    #[test]
    fn unsupported_off_windows() {
        assert!(install_assoc(Path::new("/x")).is_err());
        assert!(!assoc_status().installed);
    }

    #[cfg(windows)]
    #[test]
    fn install_status_uninstall_roundtrip() {
        let exe = std::env::current_exe().unwrap();
        install_assoc(&exe).unwrap();
        let st = assoc_status();
        assert!(st.installed, "{:?}", st);
        assert!(st.command.unwrap().contains("run \"%1\""));
        assert!(st.icon.unwrap().ends_with(",1"));
        uninstall_assoc().unwrap();
        assert!(!assoc_status().installed);
        uninstall_assoc().unwrap(); // idempotent
    }
}
