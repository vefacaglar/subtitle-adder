//! Path and string identity helpers of LibraryKit (mac-librarykit §2.1, §2.3, §2.4, §3).
//!
//! Stored paths are plain strings with POSIX semantics (`NSString`). Swift `String ==` and dictionary keys use
//! canonical equivalence, so every case-insensitive key here is `nfc(to_lowercase(s))` ([`ci`]).
//!
//! On Windows (tests only) `\` is read as `/` and a drive prefix (`C:/`) is kept as the root, so the same lexical
//! rules apply to temp folders there.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

use sa_core::folding::fold;
use unicode_normalization::UnicodeNormalization;

/// NFC of `s`.
pub fn nfc(s: &str) -> String {
    s.nfc().collect()
}

/// The case-insensitive identity key: `nfc(to_lowercase(s))` (Swift `lowercased()` + canonical equivalence).
pub fn ci(s: &str) -> String {
    nfc(&s.to_lowercase())
}

/// Swift `String <`: lexicographic over the Unicode scalars of the NFC forms.
pub fn swift_cmp(a: &str, b: &str) -> Ordering {
    a.nfc().cmp(b.nfc())
}

/// `localizedCaseInsensitiveCompare` stand-in (registered usernames, the feed's name tie-break), close to ICU's
/// root collation for Latin text: base letters first ([`fold`]: case, accents and width ignored, so `Çağla` sorts
/// between `ali` and `zeynep` as on a Mac, not after `z` as scalar order would), then accents (lowercased NFC), ties
/// broken by [`swift_cmp`] so the order never depends on map iteration.
pub fn localized_ci_cmp(a: &str, b: &str) -> Ordering {
    fold(a).cmp(&fold(b)).then_with(|| ci(a).cmp(&ci(b))).then_with(|| swift_cmp(a, b))
}

/// Trims whitespace and newlines (`.whitespacesAndNewlines`).
pub fn trim_ws_nl(s: &str) -> &str {
    s.trim()
}

/// Trims whitespace without newlines (`.whitespaces`: tab and the space separators).
pub fn trim_ws(s: &str) -> &str {
    s.trim_matches(|c: char| c == '\t' || (c.is_whitespace() && !is_newline(c)))
}

fn is_newline(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{0B}' | '\u{0C}' | '\u{85}' | '\u{2028}' | '\u{2029}')
}

/// `PathParts::file_name` (`NSString.lastPathComponent`): `""` → `""`, `"/"` → `"/"`, `"/a/b/"` → `"b"`.
pub fn file_name(path: &str) -> String {
    if path.is_empty() {
        return String::new();
    }
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return "/".into();
    }
    match trimmed.rfind('/') {
        Some(i) => trimmed[i + 1..].to_string(),
        None => trimmed.to_string(),
    }
}

/// `PathParts::directory` (`deletingLastPathComponent`, `""` when that equals the input): `"/a/b.mp4"` → `"/a"`,
/// `"/a.mp4"` → `"/"`, `"a.mp4"` → `""`, `"/"` → `""`.
pub fn directory(path: &str) -> String {
    let parent = deleting_last_component(path);
    if parent == path { String::new() } else { parent }
}

fn deleting_last_component(path: &str) -> String {
    if path.is_empty() {
        return String::new();
    }
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return "/".into();
    }
    match trimmed.rfind('/') {
        None => String::new(),
        Some(0) => "/".into(),
        Some(i) => {
            let parent = trimmed[..i].trim_end_matches('/');
            if parent.is_empty() { "/".into() } else { parent.to_string() }
        }
    }
}

/// `VideoFingerprint.normalizedPath(String)`: trims whitespace/newlines, then `"` from both ends, then
/// [`normalize_lexically`]. Blank → `""`.
pub fn normalized_path_str(raw: &str) -> String {
    let trimmed = trim_ws_nl(raw).trim_matches('"');
    if trimmed.is_empty() {
        return String::new();
    }
    normalize_lexically(trimmed)
}

/// `VideoFingerprint.normalizedPath(URL)`: no trimming; absolute, `.`/`..` resolved lexically, no trailing `/`, no
/// symlink resolution, no case or Unicode change. A non-UTF-8 path → `""`.
pub fn normalized_path(path: &Path) -> String {
    match path.to_str() {
        Some(s) if !s.is_empty() => normalize_lexically(s),
        _ => String::new(),
    }
}

/// The lexical standardization shared by both inputs.
pub fn normalize_lexically(path: &str) -> String {
    let path = to_forward(path);
    let absolute = if root_len(&path).is_some() {
        path
    } else {
        let cwd = std::env::current_dir().map(|d| to_forward(&d.to_string_lossy())).unwrap_or_else(|_| "/".into());
        format!("{}/{}", cwd.trim_end_matches('/'), path)
    };
    let root_len = root_len(&absolute).unwrap_or(0);
    let (root, rest) = absolute.split_at(root_len);
    let mut parts: Vec<&str> = Vec::new();
    for part in rest.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    let mut out = String::with_capacity(absolute.len());
    out.push_str(root);
    out.push_str(&parts.join("/"));
    out
}

#[cfg(windows)]
fn to_forward(s: &str) -> String {
    s.replace('\\', "/")
}

#[cfg(not(windows))]
fn to_forward(s: &str) -> String {
    s.to_string()
}

/// Length of the root prefix (`"/"`, or on Windows `"C:/"`), `None` for a relative path.
fn root_len(path: &str) -> Option<usize> {
    if path.starts_with('/') {
        return Some(1);
    }
    #[cfg(windows)]
    {
        let b = path.as_bytes();
        if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b.len() == 2 || b[2] == b'/') {
            return Some(if b.len() == 2 { 2 } else { 3 });
        }
    }
    None
}

/// `AppDataDirectory::url()`: `~/Library/Application Support/SubtitleAdder`.
pub fn app_data_directory() -> PathBuf {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"));
    home.join("Library").join("Application Support").join("SubtitleAdder")
}

/// `AppDataDirectory::file(name, folder)`: `folder.unwrap_or(url())/name`, creating the folder (errors ignored).
pub fn app_data_file(name: &str, folder: Option<&Path>) -> PathBuf {
    let folder = folder.map(Path::to_path_buf).unwrap_or_else(app_data_directory);
    let _ = std::fs::create_dir_all(&folder);
    folder.join(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_parts_follow_nsstring() {
        assert_eq!(file_name(""), "");
        assert_eq!(file_name("/a/b.mp4"), "b.mp4");
        assert_eq!(file_name("/a/b/"), "b");
        assert_eq!(file_name("/"), "/");
        assert_eq!(directory("/a/b.mp4"), "/a");
        assert_eq!(directory("/a.mp4"), "/");
        assert_eq!(directory("a.mp4"), "");
        assert_eq!(directory("/"), "");
    }

    #[test]
    fn normalized_path_trims_quotes_resolves_dots_and_drops_trailing_slash() {
        assert_eq!(normalized_path_str("  \"/tmp/a/../b/c.mp4\"  "), "/tmp/b/c.mp4");
        assert_eq!(normalized_path_str("/tmp/dir/"), "/tmp/dir");
        assert_eq!(normalized_path_str("   "), "");
        assert_eq!(normalized_path_str(""), "");
        assert_eq!(normalized_path_str("/.."), "/");
        assert_eq!(normalized_path_str("//a//b/./c"), "/a/b/c");
        assert!(normalized_path(Path::new("rel/x.mp4")).ends_with("/rel/x.mp4"));
    }

    #[test]
    fn identity_keys_use_canonical_equivalence() {
        assert_eq!(ci("CAFE\u{301}"), ci("caf\u{e9}"));
        assert_eq!(swift_cmp("e\u{301}", "\u{e9}"), Ordering::Equal);
        let mut names = vec!["bob", "Alice", "alice"];
        names.sort_by(|a, b| localized_ci_cmp(a, b));
        assert_eq!(names, ["Alice", "alice", "bob"]);
        let mut turkish = vec!["zeynep", "Çağla", "ali", "oya", "Ömer", "Cem", "çınar"];
        turkish.sort_by(|a, b| localized_ci_cmp(a, b));
        assert_eq!(turkish, ["ali", "Çağla", "Cem", "çınar", "Ömer", "oya", "zeynep"]);
        assert!(app_data_directory().ends_with("Library/Application Support/SubtitleAdder"));
    }
}
