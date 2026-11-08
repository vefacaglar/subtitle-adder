//! File-system helpers with the C# semantics the Windows stores rely on: `AtomicFileStore` (win-media §0.4),
//! drive readiness without "insert disk" dialogs, fixed file times and reads that leave the access time alone.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::windows::fs::{FileTimesExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::time::{Duration, SystemTime};

const FILE_SHARE_ALL: u32 = 0x1 | 0x2 | 0x4;
pub(crate) const FILE_FLAG_SEQUENTIAL_SCAN: u32 = 0x0800_0000;
const FILE_FLAG_WRITE_THROUGH: u32 = 0x8000_0000;
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
const GENERIC_READ: u32 = 0x8000_0000;
const FILE_WRITE_ATTRIBUTES: u32 = 0x0000_0100;
pub(crate) const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
pub(crate) const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;
pub(crate) const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
pub(crate) const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

/// Default retry count of `AtomicFileStore`.
pub const DEFAULT_ATTEMPTS: u32 = 5;

/// Decodes text with BOM detection (UTF-8, UTF-16 LE/BE, UTF-32 LE/BE; no BOM → UTF-8, invalid sequences replaced).
pub fn decode_with_bom(bytes: &[u8]) -> String {
    fn utf16(bytes: &[u8], le: bool) -> String {
        let units = bytes.chunks_exact(2).map(|c| if le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) });
        char::decode_utf16(units).map(|r| r.unwrap_or('\u{FFFD}')).collect()
    }
    fn utf32(bytes: &[u8], le: bool) -> String {
        bytes
            .chunks_exact(4)
            .map(|c| {
                let v = if le { u32::from_le_bytes([c[0], c[1], c[2], c[3]]) } else { u32::from_be_bytes([c[0], c[1], c[2], c[3]]) };
                char::from_u32(v).unwrap_or('\u{FFFD}')
            })
            .collect()
    }
    match bytes {
        [0xFF, 0xFE, 0x00, 0x00, rest @ ..] => utf32(rest, true),
        [0x00, 0x00, 0xFE, 0xFF, rest @ ..] => utf32(rest, false),
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
        [0xFF, 0xFE, rest @ ..] => utf16(rest, true),
        [0xFE, 0xFF, rest @ ..] => utf16(rest, false),
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// `AtomicFileStore.ReadAllTextShared`: `None` for a blank path or a missing file (at once); other errors are retried
/// `max_attempts` times with `15 * attempt` ms sleeps. Opened with share Read|Write|Delete.
pub fn read_all_text_shared(path: &Path, max_attempts: u32) -> Option<String> {
    if path.as_os_str().is_empty() {
        return None;
    }
    let attempts = max_attempts.max(1);
    for attempt in 1..=attempts {
        let result = OpenOptions::new().read(true).share_mode(FILE_SHARE_ALL).open(path).and_then(|mut f| {
            let mut bytes = Vec::new();
            f.read_to_end(&mut bytes).map(|_| bytes)
        });
        match result {
            Ok(bytes) => return Some(decode_with_bom(&bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(_) if attempt < attempts => std::thread::sleep(Duration::from_millis(15 * u64::from(attempt))),
            Err(_) => return None,
        }
    }
    None
}

/// `AtomicFileStore.WriteAllTextAtomic`: creates the folder and writes UTF-8 without BOM to
/// `"{path}.{pid}.{guid_n}.tmp"` once (a failure there is final), then renames it over `path`, retrying only the
/// rename `max_attempts` times with `15 * attempt` ms sleeps; the temp file is always removed. Never panics.
pub fn write_all_text_atomic(path: &Path, content: &str, max_attempts: u32) -> bool {
    if path.as_os_str().is_empty() {
        return false;
    }
    let temp = {
        let mut s = path.as_os_str().to_owned();
        s.push(format!(".{}.{}.tmp", std::process::id(), uuid::Uuid::new_v4().simple()));
        std::path::PathBuf::from(s)
    };
    let written = (|| -> std::io::Result<()> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let mut f = File::create(&temp)?;
        f.write_all(content.as_bytes())?;
        f.flush()
    })();
    let mut ok = false;
    if written.is_ok() {
        let attempts = max_attempts.max(1);
        for attempt in 1..=attempts {
            match std::fs::rename(&temp, path) {
                Ok(()) => {
                    ok = true;
                    break;
                }
                Err(_) if attempt < attempts => std::thread::sleep(Duration::from_millis(15 * u64::from(attempt))),
                Err(_) => {}
            }
        }
    }
    if std::fs::symlink_metadata(&temp).is_ok() {
        let _ = std::fs::remove_file(&temp);
    }
    ok
}

/// Writes `bytes` to a new file with no sharing and `FILE_FLAG_WRITE_THROUGH` (the C# chunk temp file).
pub(crate) fn write_through(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut f = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .share_mode(0)
        .custom_flags(FILE_FLAG_WRITE_THROUGH)
        .open(path)?;
    f.write_all(bytes)?;
    f.flush()
}

/// Opens for reading on a handle that never updates the last-access time (`SetFileTime` with
/// `0xFFFFFFFF'FFFFFFFF`); falls back to a plain shared read-only handle.
pub(crate) fn open_for_read_without_access_time(path: &Path) -> std::io::Result<File> {
    use windows::Win32::Foundation::{FILETIME, HANDLE};
    use windows::Win32::Storage::FileSystem::SetFileTime;
    if let Ok(f) = OpenOptions::new()
        .access_mode(GENERIC_READ | FILE_WRITE_ATTRIBUTES)
        .share_mode(FILE_SHARE_ALL)
        .custom_flags(FILE_FLAG_SEQUENTIAL_SCAN)
        .open(path)
    {
        let preserve = FILETIME { dwLowDateTime: u32::MAX, dwHighDateTime: u32::MAX };
        // SAFETY: the handle belongs to `f`, which outlives the call; the FILETIME pointer is valid for the call.
        let _ = unsafe { SetFileTime(HANDLE(f.as_raw_handle()), None, Some(&preserve), None) };
        return Ok(f);
    }
    OpenOptions::new().read(true).share_mode(FILE_SHARE_ALL).custom_flags(FILE_FLAG_SEQUENTIAL_SCAN).open(path)
}

/// Sets creation, last-write and last-access time of a file or folder (errors ignored).
pub(crate) fn set_fixed_times(path: &Path, is_directory: bool, at: SystemTime) {
    let flags = if is_directory { FILE_FLAG_BACKUP_SEMANTICS } else { 0 };
    if let Ok(f) = OpenOptions::new().access_mode(FILE_WRITE_ATTRIBUTES).share_mode(FILE_SHARE_ALL).custom_flags(flags).open(path) {
        let times = std::fs::FileTimes::new().set_accessed(at).set_modified(at).set_created(at);
        let _ = f.set_times(times);
    }
}

/// `DriveInfo.IsReady` / `Directory.Exists` of a root, probed under `SEM_FAILCRITICALERRORS` so an empty card reader
/// or optical drive never shows an "insert disk" dialog.
pub(crate) fn root_exists_quietly(root: &str) -> bool {
    use windows::Win32::System::Diagnostics::Debug::{SEM_FAILCRITICALERRORS, SetThreadErrorMode, THREAD_ERROR_MODE};
    let mut old = THREAD_ERROR_MODE(0);
    // SAFETY: plain Win32 calls on the current thread; `old` is a valid out pointer.
    let changed = unsafe { SetThreadErrorMode(SEM_FAILCRITICALERRORS, Some(&mut old)) }.is_ok();
    let exists = Path::new(root).is_dir();
    if changed {
        // SAFETY: restores the previous mode of this thread.
        let _ = unsafe { SetThreadErrorMode(old, None) };
    }
    exists
}

/// `%LOCALAPPDATA%` (`SHGetKnownFolderPath(FOLDERID_LocalAppData)`), `LOCALAPPDATA` as a fallback.
pub fn local_app_data() -> Option<std::path::PathBuf> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{FOLDERID_LocalAppData, KF_FLAG_DEFAULT, SHGetKnownFolderPath};
    // SAFETY: the returned PWSTR is owned by us and freed with CoTaskMemFree after copying.
    unsafe {
        if let Ok(p) = SHGetKnownFolderPath(&FOLDERID_LocalAppData, KF_FLAG_DEFAULT, None) {
            let s = p.to_string().ok();
            CoTaskMemFree(Some(p.0 as *const _));
            if let Some(s) = s.filter(|s| !s.is_empty()) {
                return Some(s.into());
            }
        }
    }
    std::env::var_os("LOCALAPPDATA").filter(|v| !v.is_empty()).map(Into::into)
}

/// The entries directly in `dir`, split like .NET `EnumerateDirectories` / `EnumerateFiles` (`TopDirectoryOnly`): by
/// the `FILE_ATTRIBUTE_DIRECTORY` attribute of the entry itself, so a directory symbolic link or junction is a
/// directory (deleted as a link, never followed).
pub(crate) fn top_level_entries(dir: &Path) -> (Vec<std::path::PathBuf>, Vec<std::path::PathBuf>) {
    use std::os::windows::fs::MetadataExt;
    let (mut dirs, mut files) = (Vec::new(), Vec::new());
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            if meta.file_attributes() & FILE_ATTRIBUTE_DIRECTORY != 0 {
                dirs.push(entry.path());
            } else {
                files.push(entry.path());
            }
        }
    }
    (dirs, files)
}

/// `Directory.Delete(dir, recursive: true)`: a directory link or junction is removed itself (its target is never
/// touched); a real folder with everything below it.
pub(crate) fn delete_directory_recursive(dir: &Path) {
    use std::os::windows::fs::MetadataExt;
    match std::fs::symlink_metadata(dir) {
        Ok(meta) if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 => {
            let _ = std::fs::remove_dir(dir);
        }
        Ok(_) => {
            let _ = std::fs::remove_dir_all(dir);
        }
        Err(_) => {}
    }
}

/// Every file below `root` whose name ends with `suffix` (ASCII case-insensitive; `None` = every file), without
/// descending into reparse points (as .NET `SearchOption.AllDirectories`). Errors on single entries are skipped.
pub(crate) fn walk_files(root: &Path, suffix: Option<&str>, out: &mut Vec<(std::path::PathBuf, std::fs::Metadata)>) {
    use std::os::windows::fs::MetadataExt;
    let Ok(entries) = std::fs::read_dir(root) else { return };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        let attrs = meta.file_attributes();
        if attrs & FILE_ATTRIBUTE_DIRECTORY != 0 {
            if attrs & FILE_ATTRIBUTE_REPARSE_POINT == 0 {
                walk_files(&entry.path(), suffix, out);
            }
            continue;
        }
        let matches = suffix.is_none_or(|s| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.len() >= s.len() && name[name.len() - s.len()..].eq_ignore_ascii_case(s)
        });
        if matches {
            out.push((entry.path(), meta));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_round_trip_and_bom() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub").join("x.json");
        assert_eq!(read_all_text_shared(&p, DEFAULT_ATTEMPTS), None);
        assert!(write_all_text_atomic(&p, "{\"ş\":1}", DEFAULT_ATTEMPTS));
        assert_eq!(read_all_text_shared(&p, DEFAULT_ATTEMPTS).as_deref(), Some("{\"ş\":1}"));
        assert_eq!(std::fs::read(&p).unwrap()[0], b'{', "no BOM");
        let leftovers: Vec<_> = std::fs::read_dir(p.parent().unwrap()).unwrap().flatten().collect();
        assert_eq!(leftovers.len(), 1);
        assert_eq!(decode_with_bom(&[0xEF, 0xBB, 0xBF, b'a']), "a");
        assert_eq!(decode_with_bom(&[0xFF, 0xFE, b'a', 0]), "a");
        assert_eq!(decode_with_bom(&[0xFE, 0xFF, 0, b'a']), "a");
        assert_eq!(decode_with_bom(&[0xFF, 0xFE, 0, 0, b'a', 0, 0, 0]), "a");
    }

    #[test]
    fn readiness() {
        assert!(root_exists_quietly("C:\\"));
        assert!(local_app_data().is_some());
    }
}
