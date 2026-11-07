//! Python `pathlib` / `os.path` parity and the WAV naming helpers of `pipeline.py`.
//!
//! WAV names must be identical to the Python engine's (a WAV extracted by one engine version is reused by
//! the other, and `extract-audio` → `transcribe` hand-offs depend on it), so every helper reproduces the
//! Python text operation exactly: `str(path)`, `Path.stem`, `Path.resolve()`, `os.path.normcase`, code-point
//! lengths, `sha256(... .encode("utf-8", errors="replace"))`.
//!
//! Strings produced from OS paths map undecodable units (lone UTF-16 surrogates on Windows, invalid UTF-8
//! bytes on POSIX) to `?`, which is exactly what Python's `encode("utf-8", errors="replace")` yields for
//! them, and keeps code-point counts equal.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// `pipeline.MAX_SAFE_PATH_CHARS`.
pub const MAX_SAFE_PATH_CHARS: usize = 210;
/// `pipeline.SOURCE_HASH_CHARS`.
pub const SOURCE_HASH_CHARS: usize = 8;
/// `pipeline.SOURCE_MARKER_SUFFIX`.
pub const SOURCE_MARKER_SUFFIX: &str = ".source.json";
/// `pipeline.SOURCE_MARKER_VERSION`.
pub const SOURCE_MARKER_VERSION: i64 = 1;

/// Path separator Python uses in `str(path)`.
#[cfg(windows)]
pub const SEP: char = '\\';
#[cfg(not(windows))]
pub const SEP: char = '/';

/// OS string → Python-like text (undecodable units become `?`).
pub fn os_to_py_string(s: &OsStr) -> String {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        char::decode_utf16(s.encode_wide()).map(|r| r.unwrap_or('?')).collect()
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let mut out = String::new();
        for chunk in s.as_bytes().utf8_chunks() {
            out.push_str(chunk.valid());
            for _ in chunk.invalid() {
                out.push('?');
            }
        }
        out
    }
    #[cfg(not(any(windows, unix)))]
    {
        s.to_string_lossy().into_owned()
    }
}

fn is_sep(c: char) -> bool {
    if cfg!(windows) { c == '\\' || c == '/' } else { c == '/' }
}

/// Splits a path text into (drive, root, rest) like Python's `splitroot` (Windows: `C:`, `\\server\share`;
/// POSIX: root `/` or exactly `//`).
fn split_root(s: &str) -> (String, String, &str) {
    if cfg!(windows) {
        let chars: Vec<char> = s.chars().collect();
        // UNC: \\server\share
        if chars.len() >= 2 && is_sep(chars[0]) && is_sep(chars[1]) && !(chars.len() >= 3 && is_sep(chars[2])) {
            // find end of server and share
            let rest: String = chars[2..].iter().collect();
            let mut parts = rest.splitn(3, |c| is_sep(c));
            let server = parts.next().unwrap_or("");
            let share = parts.next();
            if !server.is_empty() {
                if let Some(share) = share {
                    let drive = format!("\\\\{server}\\{share}");
                    let consumed = 2 + server.len() + 1 + share.len();
                    let after = &s[consumed..];
                    let (root, rest) = match after.chars().next() {
                        Some(c) if is_sep(c) => ("\\".to_string(), &after[c.len_utf8()..]),
                        _ => (String::new(), after),
                    };
                    return (drive, root, rest);
                }
            }
        }
        if chars.len() >= 2 && chars[1] == ':' && chars[0].is_ascii_alphabetic() {
            let after = &s[2..];
            let (root, rest) = match after.chars().next() {
                Some(c) if is_sep(c) => ("\\".to_string(), &after[c.len_utf8()..]),
                _ => (String::new(), after),
            };
            return (s[..2].to_string(), root, rest);
        }
        match s.chars().next() {
            Some(c) if is_sep(c) => (String::new(), "\\".to_string(), s.trim_start_matches(is_sep)),
            _ => (String::new(), String::new(), s),
        }
    } else if s.starts_with("//") && !s.starts_with("///") {
        (String::new(), "//".to_string(), &s[2..])
    } else if s.starts_with('/') {
        (String::new(), "/".to_string(), s.trim_start_matches('/'))
    } else {
        (String::new(), String::new(), s)
    }
}

fn join_parts(drive: &str, root: &str, parts: &[&str]) -> String {
    let mut out = String::new();
    out.push_str(drive);
    out.push_str(root);
    out.push_str(&parts.join(&SEP.to_string()));
    if out.is_empty() { ".".to_string() } else { out }
}

/// Python `str(PurePath(text))`: separators unified (Windows `\`), duplicate separators and `.` components
/// removed, no trailing separator, `..` kept; empty → `.`.
pub fn pure_path_str(text: &str) -> String {
    let (drive, root, rest) = split_root(text);
    let parts: Vec<&str> = rest.split(is_sep).filter(|p| !p.is_empty() && *p != ".").collect();
    join_parts(&drive, &root, &parts)
}

/// Python `str(path)` for a `Path` (see [`pure_path_str`]).
pub fn py_path_str(path: &Path) -> String {
    pure_path_str(&os_to_py_string(path.as_os_str()))
}

/// Python `os.path.normpath(text)` (lexical; `..` pops the previous component, is dropped at the root and
/// kept at the start of a relative path).
pub fn normpath(text: &str) -> String {
    let (drive, root, rest) = split_root(text);
    let mut parts: Vec<&str> = Vec::new();
    for p in rest.split(is_sep) {
        if p.is_empty() || p == "." {
            continue;
        }
        if p == ".." {
            if matches!(parts.last(), Some(last) if *last != "..") {
                parts.pop();
                continue;
            }
            if !root.is_empty() {
                continue;
            }
        }
        parts.push(p);
    }
    join_parts(&drive, &root, &parts)
}

/// Python `os.path.abspath(path)`: absolute and lexically normalized (Windows: `GetFullPathNameW`).
pub fn abspath(path: &Path) -> PathBuf {
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    PathBuf::from(normpath(&os_to_py_string(abs.as_os_str())))
}

/// Python `Path.name` of a path text.
pub fn py_name(text: &str) -> &str {
    let (_, _, rest) = split_root(text);
    rest.rsplit(is_sep).find(|p| !p.is_empty() && *p != ".").unwrap_or("")
}

/// Python `PurePath.suffix` of a file name: `name[i:]` with `i = name.rfind('.')` if `0 < i < len(name)-1`.
pub fn py_suffix(name: &str) -> &str {
    match name.rfind('.') {
        Some(i) if i > 0 && i < name.len() - 1 => &name[i..],
        _ => "",
    }
}

/// Python `PurePath.stem` of a file name (`.bashrc` → `.bashrc`, `a.tar.gz` → `a.tar`, `foo.` → `foo.`).
pub fn py_stem(name: &str) -> &str {
    &name[..name.len() - py_suffix(name).len()]
}

/// `str(Path(dir_text) / name)`.
pub fn py_join(dir_text: &str, name: &str) -> String {
    if dir_text == "." || dir_text.is_empty() {
        return pure_path_str(name);
    }
    if dir_text.ends_with(is_sep) || (cfg!(windows) && dir_text.len() == 2 && dir_text.ends_with(':')) {
        return format!("{dir_text}{name}");
    }
    format!("{dir_text}{SEP}{name}")
}

/// Python `Path.resolve()` (non-strict, 3.11): absolute, symlinks resolved on the longest existing prefix
/// (Windows: final path name with real casing, `\\?\` stripped), the missing tail appended as given.
pub fn resolve(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let abs = abspath(path);
        let mut head = os_to_py_string(abs.as_os_str());
        let mut tail: Vec<String> = Vec::new();
        loop {
            if let Ok(real) = dunce::canonicalize(&head) {
                let mut s = os_to_py_string(real.as_os_str());
                for name in tail.iter().rev() {
                    s = py_join(&pure_path_str(&s), name);
                }
                return PathBuf::from(pure_path_str(&s));
            }
            let (drive, root, rest) = split_root(&head);
            let rest = rest.trim_end_matches(is_sep);
            match rest.rfind(is_sep) {
                Some(i) => {
                    tail.push(rest[i + 1..].to_string());
                    head = format!("{drive}{root}{}", &rest[..i]);
                }
                None if !rest.is_empty() => {
                    tail.push(rest.to_string());
                    head = format!("{drive}{root}");
                }
                None => {
                    // Reached a root that does not exist (e.g. a missing drive).
                    let mut s = head.clone();
                    for name in tail.iter().rev() {
                        s = py_join(&pure_path_str(&s), name);
                    }
                    return PathBuf::from(pure_path_str(&s));
                }
            }
        }
    }
    #[cfg(not(windows))]
    {
        let text = os_to_py_string(path.as_os_str());
        let start = if text.starts_with('/') {
            PathBuf::from("/")
        } else {
            std::env::current_dir().map(|d| resolve_posix_existing(&d)).unwrap_or_else(|_| PathBuf::from("/"))
        };
        let mut cur = start;
        for comp in text.split('/') {
            if comp.is_empty() || comp == "." {
                continue;
            }
            if comp == ".." {
                cur.pop();
                continue;
            }
            let next = cur.join(comp);
            match std::fs::symlink_metadata(&next) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    cur = std::fs::canonicalize(&next).unwrap_or(next);
                }
                _ => cur = next,
            }
        }
        PathBuf::from(pure_path_str(&os_to_py_string(cur.as_os_str())))
    }
}

#[cfg(not(windows))]
fn resolve_posix_existing(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Python `os.path.normcase(text)`: Windows → `/`→`\`, then `LCMapStringEx(LOCALE_NAME_INVARIANT,
/// LCMAP_LOWERCASE)` (CPython 3.11 `ntpath.normcase`; NOT `str.lower()`: `İ` stays `İ`); POSIX (macOS too) → as is.
pub fn normcase(text: &str) -> String {
    #[cfg(windows)]
    {
        win_lowercase_invariant(&text.replace('/', "\\"))
    }
    #[cfg(not(windows))]
    {
        text.to_string()
    }
}

#[cfg(windows)]
fn win_lowercase_invariant(text: &str) -> String {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LCMapStringEx(
            locale_name: *const u16,
            map_flags: u32,
            src: *const u16,
            src_len: i32,
            dest: *mut u16,
            dest_len: i32,
            version_info: *mut std::ffi::c_void,
            reserved: *mut std::ffi::c_void,
            sort_handle: isize,
        ) -> i32;
    }
    const LCMAP_LOWERCASE: u32 = 0x0000_0100;
    let src: Vec<u16> = text.encode_utf16().collect();
    if src.is_empty() {
        return String::new();
    }
    let invariant: [u16; 1] = [0];
    // SAFETY: valid buffers with the lengths passed; the invariant locale name is an empty NUL-terminated string.
    unsafe {
        let n = LCMapStringEx(invariant.as_ptr(), LCMAP_LOWERCASE, src.as_ptr(), src.len() as i32,
            std::ptr::null_mut(), 0, std::ptr::null_mut(), std::ptr::null_mut(), 0);
        if n <= 0 {
            return text.to_lowercase();
        }
        let mut dest = vec![0u16; n as usize];
        let m = LCMapStringEx(invariant.as_ptr(), LCMAP_LOWERCASE, src.as_ptr(), src.len() as i32,
            dest.as_mut_ptr(), n, std::ptr::null_mut(), std::ptr::null_mut(), 0);
        if m <= 0 {
            return text.to_lowercase();
        }
        dest.truncate(m as usize);
        char::decode_utf16(dest).map(|r| r.unwrap_or('?')).collect()
    }
}

/// Lowercase hex SHA-256 of the UTF-8 bytes of `text`.
pub fn sha256_hex(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

/// `pipeline._source_hash(video)`: first 8 hex chars of `sha256(normcase(str(video.resolve())))`.
pub fn source_hash(video: &Path) -> String {
    let resolved = py_path_str(&resolve(video));
    sha256_hex(&normcase(&resolved))[..SOURCE_HASH_CHARS].to_string()
}

/// `pipeline._safe_out_stem(video, out_dir)`: the video stem (trailing spaces/dots removed, `video` if
/// empty), shortened with a 10-hex-digit hash when `<out_dir>/<stem>---------.wav` would exceed 210 code points.
pub fn safe_out_stem(video: &Path, out_dir: &Path) -> String {
    let video_text = py_path_str(video);
    let stem = py_stem(py_name(&video_text)).trim_end_matches([' ', '.']);
    let stem = if stem.is_empty() { "video" } else { stem };
    let out_text = py_path_str(out_dir);
    let candidate = py_join(&out_text, &format!("{stem}---------.wav"));
    if candidate.chars().count() <= MAX_SAFE_PATH_CHARS {
        return stem.to_string();
    }
    let digest = &sha256_hex(stem)[..10];
    let allowed = 24i64.max(MAX_SAFE_PATH_CHARS as i64 - out_text.chars().count() as i64 - 20 - 9) as usize;
    let prefix = crate::pyfmt::char_prefix(stem, allowed).trim_end_matches([' ', '.']);
    format!("{prefix}_{digest}")
}

/// `pipeline.wav_path_for(video, out_dir)`: `<out_dir>/<safe stem>-<source hash>.wav`.
pub fn wav_path_for(video: &Path, out_dir: &Path) -> PathBuf {
    let name = format!("{}-{}.wav", safe_out_stem(video, out_dir), source_hash(video));
    PathBuf::from(py_join(&py_path_str(out_dir), &name))
}

/// The temp WAV next to `wav`: `<wav stem>.<pid>.tmp.wav`.
pub fn tmp_wav_path(wav: &Path, pid: u32) -> PathBuf {
    let text = py_path_str(wav);
    let name = py_name(&text);
    let parent = &text[..text.len() - name.len()];
    PathBuf::from(format!("{parent}{}.{pid}.tmp.wav", py_stem(name)))
}

/// The identity marker of `wav`: `<wav name>.source.json`.
pub fn marker_path(wav: &Path) -> PathBuf {
    PathBuf::from(format!("{}{SOURCE_MARKER_SUFFIX}", py_path_str(wav)))
}

/// The temp marker of `wav`: `<wav name>.source.json.<pid>.tmp`.
pub fn marker_tmp_path(wav: &Path, pid: u32) -> PathBuf {
    PathBuf::from(format!("{}{SOURCE_MARKER_SUFFIX}.{pid}.tmp", py_path_str(wav)))
}

/// A subtitle output: `<out_dir>/<safe stem>.<fmt>`.
pub fn output_path(out_dir: &Path, safe_stem: &str, fmt: &str) -> PathBuf {
    PathBuf::from(py_join(&py_path_str(out_dir), &format!("{safe_stem}.{fmt}")))
}

/// The fallback subtitle output when writing [`output_path`] fails: `<out_dir>/<safe stem>.<pid>.<fmt>`.
pub fn fallback_output_path(out_dir: &Path, safe_stem: &str, pid: u32, fmt: &str) -> PathBuf {
    PathBuf::from(py_join(&py_path_str(out_dir), &format!("{safe_stem}.{pid}.{fmt}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stem_rules() {
        assert_eq!(py_stem(".bashrc"), ".bashrc");
        assert_eq!(py_stem("a.tar.gz"), "a.tar");
        assert_eq!(py_stem("foo."), "foo.");
        assert_eq!(py_stem("Clip.mp4"), "Clip");
        assert_eq!(py_stem(""), "");
    }

    #[cfg(windows)]
    #[test]
    fn test_windows_pure_paths() {
        assert_eq!(pure_path_str("C:/a//b/./c/"), "C:\\a\\b\\c");
        assert_eq!(pure_path_str(""), ".");
        assert_eq!(pure_path_str("a/../b"), "a\\..\\b");
        assert_eq!(normpath("C:\\a\\..\\..\\b"), "C:\\b");
        assert_eq!(normpath("..\\a"), "..\\a");
        assert_eq!(pure_path_str("\\\\srv\\share\\x\\"), "\\\\srv\\share\\x");
        assert_eq!(py_join("C:\\", "x"), "C:\\x");
        assert_eq!(py_join(".", "x"), "x");
        assert_eq!(normcase("C:/Users/İşÜ"), "c:\\users\\İşü");
    }

    #[cfg(windows)]
    #[test]
    fn test_companion_names() {
        let wav = Path::new("C:\\o\\Clip-0123abcd.wav");
        assert_eq!(py_path_str(&tmp_wav_path(wav, 42)), "C:\\o\\Clip-0123abcd.42.tmp.wav");
        assert_eq!(py_path_str(&marker_path(wav)), "C:\\o\\Clip-0123abcd.wav.source.json");
        assert_eq!(py_path_str(&marker_tmp_path(wav, 42)), "C:\\o\\Clip-0123abcd.wav.source.json.42.tmp");
        assert_eq!(py_path_str(&fallback_output_path(Path::new("C:\\o"), "Clip", 7, "srt")), "C:\\o\\Clip.7.srt");
    }
}
