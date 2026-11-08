//! `AtomicFileStore` (win-core §1.5): shared reads and atomic replace-writes used by settings.json,
//! media-index.json, profiles.json and translation-providers.json. Never panics on I/O.
//!
//! macOS stores (LibraryKit formats) use their own `write_atomically` + `fsync` + aside-copy helpers in
//! `sa-profiles::mac::fileio` (mac-librarykit §9.3); this module is the Windows/C# variant.

use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Default attempt count.
pub const DEFAULT_ATTEMPTS: u32 = 5;

fn is_blank(path: &Path) -> bool {
    path.as_os_str().to_string_lossy().trim().is_empty()
}

fn backoff(attempt: u32) {
    std::thread::sleep(Duration::from_millis(15 * u64::from(attempt)));
}

/// Reads the whole file with BOM detection (UTF-8/16/32; no BOM → lossy UTF-8). Missing file → `None` at once;
/// other errors retry `max_attempts` times sleeping `15 * attempt` ms. Opened with share Read|Write|Delete
/// (the Rust standard library's default share mode on Windows).
pub fn read_all_text_shared(path: &Path, max_attempts: u32) -> Option<String> {
    if is_blank(path) {
        return None;
    }
    let attempts = max_attempts.max(1);
    for attempt in 1..=attempts {
        // C# `File.Exists`: false for a directory or when the path cannot be inspected.
        if !path.is_file() {
            return None;
        }
        let result = std::fs::File::open(path).and_then(|mut f| {
            let mut bytes = Vec::new();
            f.read_to_end(&mut bytes).map(|_| bytes)
        });
        match result {
            Ok(bytes) => return Some(decode_with_bom(&bytes)),
            Err(e) if e.kind() == ErrorKind::NotFound => return None,
            Err(_) => {
                if attempt == attempts {
                    return None;
                }
                backoff(attempt);
            }
        }
    }
    None
}

/// The temp file name used by [`write_all_text_atomic`]: `"{path}.{pid}.{guid_n}.tmp"`.
pub fn temp_path_for(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(format!(".{}.{}.tmp", std::process::id(), crate::text::guid_n()));
    PathBuf::from(s)
}

/// Writes `content` (UTF-8, no BOM) to `"{path}.{pid}.{guid_n}.tmp"`, then renames over `path`
/// (`MOVEFILE_REPLACE_EXISTING`), retrying `max_attempts` times with `15 * attempt` ms sleeps. The temp file is
/// always removed. Returns success.
pub fn write_all_text_atomic(path: &Path, content: &str, max_attempts: u32) -> bool {
    if is_blank(path) {
        return false;
    }
    let temp = temp_path_for(path);
    let ok = write_then_rename(path, &temp, content, max_attempts.max(1));
    if temp.exists() {
        let _ = std::fs::remove_file(&temp);
    }
    ok
}

fn write_then_rename(path: &Path, temp: &Path, content: &str, attempts: u32) -> bool {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty())
        && std::fs::create_dir_all(dir).is_err()
    {
        return false;
    }
    let written = std::fs::File::create(temp).and_then(|mut f| f.write_all(content.as_bytes()));
    if written.is_err() {
        return false;
    }
    for attempt in 1..=attempts {
        match std::fs::rename(temp, path) {
            Ok(()) => return true,
            Err(_) => {
                if attempt == attempts {
                    return false;
                }
                backoff(attempt);
            }
        }
    }
    false
}

/// Decodes bytes with BOM detection (UTF-8 `EF BB BF`, UTF-32 LE `FF FE 00 00`, UTF-32 BE `00 00 FE FF`,
/// UTF-16 LE `FF FE`, UTF-16 BE `FE FF`; no BOM → lossy UTF-8). Invalid sequences become U+FFFD (.NET
/// `StreamReader` with `detectEncodingFromByteOrderMarks`).
pub fn decode_with_bom(bytes: &[u8]) -> String {
    match bytes {
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
        [0xFF, 0xFE, 0x00, 0x00, rest @ ..] => decode_utf32(rest, false),
        [0x00, 0x00, 0xFE, 0xFF, rest @ ..] => decode_utf32(rest, true),
        [0xFF, 0xFE, rest @ ..] => decode_utf16_lossy(rest, false),
        [0xFE, 0xFF, rest @ ..] => decode_utf16_lossy(rest, true),
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// UTF-16 with U+FFFD for unpaired surrogates and a dangling odd byte.
pub fn decode_utf16_lossy(bytes: &[u8], big_endian: bool) -> String {
    let units = bytes.chunks_exact(2).map(|c| if big_endian { u16::from_be_bytes([c[0], c[1]]) } else { u16::from_le_bytes([c[0], c[1]]) });
    let mut out: String = char::decode_utf16(units).map(|r| r.unwrap_or('\u{FFFD}')).collect();
    if bytes.len() % 2 == 1 {
        out.push('\u{FFFD}');
    }
    out
}

/// UTF-16 that fails on any invalid sequence (macOS `String(data:encoding:)`).
pub fn decode_utf16_strict(bytes: &[u8], big_endian: bool) -> Option<String> {
    if bytes.len() % 2 == 1 {
        return None;
    }
    let units = bytes.chunks_exact(2).map(|c| if big_endian { u16::from_be_bytes([c[0], c[1]]) } else { u16::from_le_bytes([c[0], c[1]]) });
    char::decode_utf16(units).collect::<Result<String, _>>().ok()
}

fn decode_utf32(bytes: &[u8], big_endian: bool) -> String {
    let mut out = String::with_capacity(bytes.len() / 4);
    for c in bytes.chunks(4) {
        if c.len() < 4 {
            out.push('\u{FFFD}');
            break;
        }
        let v = if big_endian { u32::from_be_bytes([c[0], c[1], c[2], c[3]]) } else { u32::from_le_bytes([c[0], c[1], c[2], c[3]]) };
        out.push(char::from_u32(v).unwrap_or('\u{FFFD}'));
    }
    out
}
