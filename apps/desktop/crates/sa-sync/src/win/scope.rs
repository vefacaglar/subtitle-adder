//! Default channel scope = .NET `$"{Environment.UserDomainName}\{Environment.UserName}"` (win-sync §2).

use windows::Win32::Security::Authentication::Identity::{GetUserNameExW, NameSamCompatible};
use windows::Win32::System::WindowsProgramming::GetUserNameW;
use windows::core::PWSTR;

fn sam_compatible() -> Option<String> {
    let mut size = 0u32;
    let _ = unsafe { GetUserNameExW(NameSamCompatible, None, &mut size) };
    if size == 0 {
        return None;
    }
    let mut buf = vec![0u16; size as usize + 1];
    let mut len = buf.len() as u32;
    if !unsafe { GetUserNameExW(NameSamCompatible, Some(PWSTR(buf.as_mut_ptr())), &mut len) } {
        return None;
    }
    Some(String::from_utf16_lossy(&buf[..len as usize]))
}

fn user_name() -> String {
    let mut size = 0u32;
    let _ = unsafe { GetUserNameW(None, &mut size) };
    if size == 0 {
        return std::env::var("USERNAME").unwrap_or_default();
    }
    let mut buf = vec![0u16; size as usize];
    if unsafe { GetUserNameW(Some(PWSTR(buf.as_mut_ptr())), &mut size) }.is_err() {
        return std::env::var("USERNAME").unwrap_or_default();
    }
    let n = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..n])
}

/// `DOMAIN\user` from `GetUserNameExW(NameSamCompatible)` (as returned); otherwise `%USERDOMAIN%\GetUserNameW`.
pub(crate) fn default_scope() -> String {
    if let Some(s) = sam_compatible().filter(|s| s.contains('\\')) {
        return s;
    }
    format!("{}\\{}", std::env::var("USERDOMAIN").unwrap_or_default(), user_name())
}
