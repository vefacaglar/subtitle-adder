//! Windows path rules (win-profiles §10.8, win-media §0.1): .NET `Path.GetFullPath`, `GetPathRoot`, `GetFileName`,
//! `GetDirectoryName` and the drive-readiness check used to hide unplugged drives.

use std::path::Path;

use windows::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_DIRECTORY, GetFileAttributesW, GetFullPathNameW, GetLongPathNameW, INVALID_FILE_ATTRIBUTES,
};
use windows::Win32::System::Diagnostics::Debug::{SEM_FAILCRITICALERRORS, SEM_NOOPENFILEERRORBOX, SetThreadErrorMode, THREAD_ERROR_MODE};
use windows::core::PCWSTR;

fn is_sep(c: char) -> bool {
    c == '\\' || c == '/'
}

/// .NET `PathInternal.GetRootLength` (Windows), in chars.
pub fn root_length(chars: &[char]) -> usize {
    let len = chars.len();
    let is_extended = len >= 4
        && ((chars[0] == '\\' && chars[1] == '\\' && chars[2] == '?' && chars[3] == '\\')
            || (chars[0] == '\\' && chars[1] == '?' && chars[2] == '?' && chars[3] == '\\'));
    let is_device = is_extended
        || (len >= 4 && is_sep(chars[0]) && is_sep(chars[1]) && (chars[2] == '.' || chars[2] == '?') && is_sep(chars[3]));
    let is_device_unc = is_device
        && len >= 8
        && chars[4].eq_ignore_ascii_case(&'U')
        && chars[5].eq_ignore_ascii_case(&'N')
        && chars[6].eq_ignore_ascii_case(&'C')
        && is_sep(chars[7]);
    if (!is_device || is_device_unc) && len > 0 && is_sep(chars[0]) {
        if is_device_unc || (len > 1 && is_sep(chars[1])) {
            let mut i = if is_device_unc { 8 } else { 2 };
            let mut n = 2;
            while i < len {
                if is_sep(chars[i]) {
                    n -= 1;
                    if n == 0 {
                        break;
                    }
                }
                i += 1;
            }
            i
        } else {
            1
        }
    } else if is_device {
        let mut i = 4;
        while i < len && !is_sep(chars[i]) {
            i += 1;
        }
        if i < len && i > 4 && is_sep(chars[i]) {
            i += 1;
        }
        i
    } else if len >= 2 && chars[1] == ':' && chars[0].is_ascii_alphabetic() {
        if len > 2 && is_sep(chars[2]) { 3 } else { 2 }
    } else {
        0
    }
}

/// .NET `Path.GetPathRoot` (`""` for a relative path).
pub fn path_root(path: &str) -> String {
    let chars: Vec<char> = path.chars().collect();
    let n = root_length(&chars);
    normalize_separators(&chars[..n].iter().collect::<String>())
}

/// .NET `PathInternal.NormalizeDirectorySeparators` (Windows): `/` → `\`, runs of separators collapsed (a leading
/// separator is kept, so `\\server` stays).
fn normalize_separators(path: &str) -> String {
    let chars: Vec<char> = path.chars().collect();
    if chars.is_empty() {
        return String::new();
    }
    let mut out = String::with_capacity(path.len());
    let mut start = 0;
    if is_sep(chars[0]) {
        out.push('\\');
        start = 1;
    }
    let mut i = start;
    while i < chars.len() {
        let c = chars[i];
        if is_sep(c) {
            if i + 1 < chars.len() && is_sep(chars[i + 1]) {
                i += 1;
                continue;
            }
            out.push('\\');
        } else {
            out.push(c);
        }
        i += 1;
    }
    out
}

/// .NET `Path.GetFileName`.
pub fn file_name(path: &str) -> String {
    let chars: Vec<char> = path.chars().collect();
    let root = root_length(&chars);
    let last = chars.iter().rposition(|&c| is_sep(c));
    let start = match last {
        Some(i) if i + 1 > root => i + 1,
        _ => root,
    };
    chars[start.min(chars.len())..].iter().collect()
}

/// .NET `Path.GetDirectoryName` (`None` for a root or an empty path; keeps the root's `\`).
pub fn directory_name_opt(path: &str) -> Option<String> {
    if path.chars().all(|c| c == ' ') {
        return None;
    }
    let chars: Vec<char> = path.chars().collect();
    let root = root_length(&chars);
    let mut end = chars.len();
    if end <= root {
        return None;
    }
    while end > root {
        end -= 1;
        if is_sep(chars[end]) {
            break;
        }
    }
    while end > root && is_sep(chars[end - 1]) {
        end -= 1;
    }
    Some(normalize_separators(&chars[..end].iter().collect::<String>()))
}

/// .NET `Path.GetDirectoryName(p) ?? ""`.
pub fn directory_name(path: &str) -> String {
    directory_name_opt(path).unwrap_or_default()
}

fn get_full_path_name(path: &str) -> Option<String> {
    if path.contains('\0') {
        return None;
    }
    let w: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    let mut buf = vec![0u16; 512];
    loop {
        let n = unsafe { GetFullPathNameW(PCWSTR(w.as_ptr()), Some(&mut buf), None) } as usize;
        if n == 0 {
            return None;
        }
        if n < buf.len() {
            return Some(String::from_utf16_lossy(&buf[..n]));
        }
        if n > 32_768 {
            return None;
        }
        buf = vec![0u16; n + 1];
    }
}

fn get_long_path_name(path: &str) -> Option<String> {
    let w: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    let mut buf = vec![0u16; 512];
    loop {
        let n = unsafe { GetLongPathNameW(PCWSTR(w.as_ptr()), Some(&mut buf)) } as usize;
        if n == 0 {
            return None;
        }
        if n < buf.len() {
            return Some(String::from_utf16_lossy(&buf[..n]));
        }
        if n > 32_768 {
            return None;
        }
        buf = vec![0u16; n + 1];
    }
}

/// .NET `PathHelper.TryExpandShortFileName`: expands 8.3 segments of the longest existing prefix.
fn expand_short_names(full: &str) -> String {
    if !full.contains('~') {
        return full.to_owned();
    }
    if let Some(long) = get_long_path_name(full) {
        return long;
    }
    let chars: Vec<char> = full.chars().collect();
    let root = root_length(&chars);
    let mut cut = chars.len();
    while cut > root {
        let Some(sep) = chars[..cut].iter().rposition(|&c| is_sep(c)) else { break };
        if sep < root {
            break;
        }
        let prefix: String = chars[..sep].iter().collect();
        if prefix.is_empty() {
            break;
        }
        if let Some(long) = get_long_path_name(&prefix) {
            let rest: String = chars[sep..].iter().collect();
            return long + &rest;
        }
        cut = sep;
    }
    full.to_owned()
}

/// .NET `Path.GetFullPath` (Windows): `\\?\` paths unchanged, otherwise `GetFullPathNameW` + short-name expansion.
/// `None` where .NET throws (embedded NUL, too long).
pub fn get_full_path(path: &str) -> Option<String> {
    if path.is_empty() || path.contains('\0') {
        return None;
    }
    if path.starts_with("\\\\?\\") {
        return Some(path.to_owned());
    }
    let full = get_full_path_name(path)?;
    Some(expand_short_names(&full))
}

/// .NET `Path.ChangeExtension(path, ".db")`.
pub fn change_extension(path: &str, ext: &str) -> String {
    let chars: Vec<char> = path.chars().collect();
    let mut i = chars.len();
    while i > 0 {
        i -= 1;
        let c = chars[i];
        if c == '.' {
            let base: String = chars[..i].iter().collect();
            return base + ext;
        }
        if is_sep(c) || c == ':' {
            break;
        }
    }
    path.to_owned() + ext
}

/// .NET `string.Trim()` whitespace (Unicode White_Space).
fn dotnet_trim(s: &str) -> &str {
    s.trim_matches(char::is_whitespace)
}

/// Trim whitespace and `"`, `GetFullPathNameW`, trim trailing `\`/`/`; fallback on failure.
pub fn normalize_video_path(raw: &str) -> String {
    let trimmed = dotnet_trim(raw).trim_matches('"');
    if trimmed.is_empty() {
        return String::new();
    }
    match get_full_path(trimmed) {
        Some(full) => full.trim_end_matches(['\\', '/']).to_owned(),
        None => trimmed.replace('/', "\\").trim_end_matches('\\').to_owned(),
    }
}

/// [`normalize_video_path`] for a `Path`.
pub fn normalize_path(path: &Path) -> String {
    normalize_video_path(&path.to_string_lossy())
}

/// True when `root` (a drive or UNC root) is a ready directory. Never shows a "no disk" dialog.
pub fn is_root_ready(root: &str) -> bool {
    let mut probe = root.to_owned();
    if !probe.ends_with('\\') {
        probe.push('\\');
    }
    let w: Vec<u16> = probe.encode_utf16().chain(std::iter::once(0)).collect();
    let mut old = THREAD_ERROR_MODE(0);
    let set = unsafe { SetThreadErrorMode(SEM_FAILCRITICALERRORS | SEM_NOOPENFILEERRORBOX, Some(&mut old)) }.is_ok();
    let attrs = unsafe { GetFileAttributesW(PCWSTR(w.as_ptr())) };
    if set {
        unsafe {
            let _ = SetThreadErrorMode(old, None);
        }
    }
    attrs != INVALID_FILE_ATTRIBUTES && attrs & FILE_ATTRIBUTE_DIRECTORY.0 != 0
}

/// Root ready (`GetFileAttributesW(root)` under `SEM_FAILCRITICALERRORS`); never touches files.
pub fn is_video_storage_available(path: &Path) -> bool {
    let norm = normalize_path(path);
    if norm.trim().is_empty() {
        return false;
    }
    let root = path_root(&norm);
    if root.trim().is_empty() {
        return true;
    }
    // C# `IsVideoStorageAvailableCore`: roots starting with two backslashes (UNC and device paths) use
    // `Directory.Exists(root)`, drive roots `DriveInfo.IsReady`.
    is_root_ready(&root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotnet_path_functions() {
        assert_eq!(file_name("C:\\a\\b.mp4"), "b.mp4");
        assert_eq!(file_name("C:\\"), "");
        assert_eq!(file_name("C:b.mp4"), "b.mp4");
        assert_eq!(directory_name("C:\\a.mp4"), "C:\\");
        assert_eq!(directory_name("C:\\dir\\a.mp4"), "C:\\dir");
        assert_eq!(directory_name("C:\\dir\\\\a.mp4"), "C:\\dir");
        assert_eq!(directory_name("C:\\"), "");
        assert_eq!(directory_name("\\\\server\\share\\x.mp4"), "\\\\server\\share");
        assert_eq!(path_root("\\\\server\\share\\x.mp4"), "\\\\server\\share");
        assert_eq!(path_root("C:\\x"), "C:\\");
        assert_eq!(change_extension("C:\\a\\profiles.json", ".db"), "C:\\a\\profiles.db");
        assert_eq!(change_extension("C:\\a.b\\profiles", ".db"), "C:\\a.b\\profiles.db");
    }

    #[test]
    fn normalization() {
        assert_eq!(normalize_video_path("  \"C:/Videos/a.mp4\"  "), "C:\\Videos\\a.mp4");
        assert_eq!(normalize_video_path("C:\\Videos\\..\\b.mp4"), "C:\\b.mp4");
        assert_eq!(normalize_video_path("C:\\"), "C:");
        assert_eq!(normalize_video_path("   "), "");
        assert!(is_video_storage_available(Path::new("C:\\definitely\\missing.mp4")));
        // `Directory.Exists(@"\\?\C:\")` is true in .NET, so device-path videos on a ready drive stay visible.
        assert!(is_video_storage_available(Path::new("\\\\?\\C:\\definitely\\missing.mp4")));    }
}
