//! UTF-16 helpers for Win32 calls.

use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows::core::{PCWSTR, PWSTR};

/// A NUL-terminated UTF-16 buffer that outlives the `PCWSTR` taken from it.
pub struct Wide(Vec<u16>);

impl Wide {
    pub fn new(s: &str) -> Self {
        Wide(s.encode_utf16().chain(std::iter::once(0)).collect())
    }
    pub fn os(s: &std::ffi::OsStr) -> Self {
        Wide(s.encode_wide().chain(std::iter::once(0)).collect())
    }
    pub fn path(p: &Path) -> Self {
        Self::os(p.as_os_str())
    }
    pub fn pcwstr(&self) -> PCWSTR {
        PCWSTR(self.0.as_ptr())
    }
}

/// Text up to the first NUL.
pub fn from_buf(buf: &[u16]) -> String {
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..n])
}

/// `PWSTR` owned by the callee (caller frees).
pub unsafe fn from_pwstr(p: PWSTR) -> Option<String> {
    if p.is_null() {
        return None;
    }
    unsafe { p.to_string().ok() }
}
