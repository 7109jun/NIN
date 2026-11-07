//! UTF-16 helpers (pure, available on every platform so they can be unit-tested on the host).

use std::ffi::OsStr;

/// UTF-8 to NUL-terminated UTF-16.
pub fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// `OsStr` to NUL-terminated UTF-16 (lossless on Windows, lossy elsewhere).
pub fn os_to_wide(s: &OsStr) -> Vec<u16> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        s.encode_wide().chain(std::iter::once(0)).collect()
    }
    #[cfg(not(windows))]
    {
        to_wide(&s.to_string_lossy())
    }
}

/// UTF-16 (no terminator expected) to `String`, replacing invalid sequences.
pub fn from_wide(w: &[u16]) -> String {
    String::from_utf16_lossy(w)
}

/// UTF-16 up to the first NUL (or the whole slice) to `String`.
pub fn from_wide_nul(w: &[u16]) -> String {
    let end = w.iter().position(|&c| c == 0).unwrap_or(w.len());
    String::from_utf16_lossy(&w[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let w = to_wide("héllo 한글 😀");
        assert_eq!(*w.last().unwrap_or(&1), 0);
        assert_eq!(from_wide_nul(&w), "héllo 한글 😀");
        assert_eq!(from_wide(&w[..w.len() - 1]), "héllo 한글 😀");
        assert_eq!(from_wide_nul(&[104, 105, 0, 120]), "hi");
        assert_eq!(os_to_wide(OsStr::new("ab")), vec![97, 98, 0]);
    }
}
