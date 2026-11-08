//! File primitives of this crate (kept private so the providers and the secret store do not depend on unfinished
//! helpers elsewhere):
//!
//! * [`read_file`]: whole-file read that tells *missing* from *unreadable* (C# `AtomicFileStore` retries, win-providers
//!   §2.1; Swift `readFile`, mac-librarykit §14.2).
//! * [`write_atomic`]: temp file + rename. Windows uses the C# temp name `"{path}.{pid}.{guid_n}.tmp"` with up to five
//!   attempts; unix uses the LibraryKit `write_atomically` shape (`.<name>.tmp-<UUID>`, mode 0600, `fsync`).
//! * [`preserve_damaged`]: the aside copy `<stem>.corrupt-<yyyyMMdd-HHmmss>[-N].<ext>` (hard link, else copy).
//! * [`CrossProcessLock`]: Windows named mutex / unix `flock`, with a timeout; never proceeds unlocked.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;
#[cfg(unix)]
use std::time::Instant;

/// Result of reading a whole file.
pub(crate) enum ReadOutcome {
    Missing,
    /// It exists (or its state cannot be determined) but cannot be read.
    Unreadable,
    Content(Vec<u8>),
}

/// Reads the whole file. A missing file (or folder) is `Missing` at once; other errors are retried on Windows
/// (`15 * attempt` ms, five attempts, as the C# reader) and then reported as `Unreadable`.
pub(crate) fn read_file(path: &Path) -> ReadOutcome {
    let attempts = if cfg!(windows) { 5 } else { 1 };
    for attempt in 1..=attempts {
        match std::fs::File::open(path) {
            Ok(mut f) => {
                let mut buf = Vec::new();
                match f.read_to_end(&mut buf) {
                    Ok(_) => return ReadOutcome::Content(buf),
                    Err(_) if attempt < attempts => {}
                    Err(_) => return ReadOutcome::Unreadable,
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => return ReadOutcome::Missing,
            Err(_) if attempt < attempts => {}
            Err(_) => {
                // A directory or a dangling path component also reads as missing when `metadata` says so.
                return match std::fs::metadata(path) {
                    Err(e) if e.kind() == io::ErrorKind::NotFound => ReadOutcome::Missing,
                    _ => ReadOutcome::Unreadable,
                };
            }
        }
        std::thread::sleep(Duration::from_millis(15 * attempt as u64));
    }
    ReadOutcome::Unreadable
}

/// Decodes text with the BOM detection of .NET `StreamReader` (UTF-8, UTF-32 LE/BE, UTF-16 LE/BE; UTF-32 LE is
/// checked before UTF-16 LE, whose BOM it starts with); without a BOM, lossy UTF-8.
pub(crate) fn decode_text(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    let utf32 = |rest: &[u8], be: bool| -> String {
        rest.chunks_exact(4)
            .map(|c| {
                let v = if be { u32::from_be_bytes([c[0], c[1], c[2], c[3]]) } else { u32::from_le_bytes([c[0], c[1], c[2], c[3]]) };
                char::from_u32(v).unwrap_or('\u{fffd}')
            })
            .collect()
    };
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE, 0x00, 0x00]) {
        return utf32(rest, false);
    }
    if let Some(rest) = bytes.strip_prefix(&[0x00, 0x00, 0xFE, 0xFF]) {
        return utf32(rest, true);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let units: Vec<u16> = rest.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        return String::from_utf16_lossy(&units);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let units: Vec<u16> = rest.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
        return String::from_utf16_lossy(&units);
    }
    String::from_utf8_lossy(bytes).into_owned()
}

/// Atomic replace-write; creates the parent folder. See the module docs for the per-platform shape.
pub(crate) fn write_atomic(path: &Path, content: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    #[cfg(windows)]
    {
        write_atomic_windows(path, content)
    }
    #[cfg(not(windows))]
    {
        write_atomic_private(path, content)
    }
}

#[cfg(windows)]
fn write_atomic_windows(path: &Path, content: &[u8]) -> io::Result<()> {
    let mut temp_name = path.as_os_str().to_owned();
    temp_name.push(format!(".{}.{}.tmp", std::process::id(), uuid::Uuid::new_v4().simple()));
    let temp = PathBuf::from(temp_name);
    let result = (|| {
        let mut last = None;
        for attempt in 1..=5u64 {
            let r = (|| {
                let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).open(&temp)?;
                f.write_all(content)?;
                f.sync_all()?;
                drop(f);
                std::fs::rename(&temp, path)
            })();
            match r {
                Ok(()) => return Ok(()),
                Err(e) => last = Some(e),
            }
            if attempt < 5 {
                std::thread::sleep(Duration::from_millis(15 * attempt));
            }
        }
        Err(last.unwrap_or_else(|| io::Error::other("write failed")))
    })();
    let _ = std::fs::remove_file(&temp);
    result
}

#[cfg(not(windows))]
fn write_atomic_private(path: &Path, content: &[u8]) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let temp = dir.join(format!(".{name}.tmp-{}", uuid::Uuid::new_v4().hyphenated().to_string().to_uppercase()));
    let result = (|| {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC)
            .open(&temp)?;
        f.write_all(content)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// `yyyyMMdd-HHmmss` in UTC.
pub(crate) fn compact_timestamp(now: chrono::DateTime<chrono::Utc>) -> String {
    now.format("%Y%m%d-%H%M%S").to_string()
}

/// Keeps a damaged file aside as `<stem>.corrupt-<ts>[-N].<ext>` next to it (hard link so the original never
/// disappears; a copy where links are not possible). `Ok(None)` when the file is gone.
pub(crate) fn preserve_damaged(path: &Path, now: chrono::DateTime<chrono::Utc>) -> io::Result<Option<PathBuf>> {
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = path.extension().map(|s| s.to_string_lossy().into_owned());
    let ts = compact_timestamp(now);
    for attempt in 0..1000 {
        let suffix = if attempt == 0 { String::new() } else { format!("-{attempt}") };
        let name = match &ext {
            Some(ext) => format!("{stem}.corrupt-{ts}{suffix}.{ext}"),
            None => format!("{stem}.corrupt-{ts}{suffix}"),
        };
        let target = dir.join(name);
        if target.exists() {
            continue;
        }
        match std::fs::hard_link(path, &target) {
            Ok(()) => return Ok(Some(target)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(_) => match std::fs::copy(path, &target) {
                Ok(_) => return Ok(Some(target)),
                Err(e) if e.kind() == io::ErrorKind::NotFound && !path.exists() => return Ok(None),
                Err(e) => return Err(e),
            },
        }
    }
    Err(io::Error::other("corrupt file not preserved"))
}

/// Default wait for the sidecar lock.
pub(crate) const LOCK_TIMEOUT: Duration = Duration::from_secs(5);

/// A held cross-process lock; released on drop.
pub(crate) struct CrossProcessLock {
    #[cfg(windows)]
    handle: windows::Win32::Foundation::HANDLE,
    #[cfg(unix)]
    file: std::fs::File,
}

// SAFETY: the mutex handle / file descriptor is owned by this value and only released on drop.
unsafe impl Send for CrossProcessLock {}

impl CrossProcessLock {
    /// Windows: the named mutex `Local\SubtitleAdder.ProviderKeys.<h16>` (h16 = first 16 lowercase hex characters of
    /// SHA-256 over the UTF-8 of the lowercased full sidecar path). Unix: `flock(LOCK_EX)` on `<file>.lock` (0600).
    /// Polls until `timeout`; `None` when the lock could not be taken (callers then do nothing).
    pub(crate) fn acquire(file: &Path, timeout: Duration) -> Option<Self> {
        #[cfg(windows)]
        {
            Self::acquire_windows(file, timeout)
        }
        #[cfg(unix)]
        {
            Self::acquire_unix(file, timeout)
        }
    }

    /// The mutex name for a sidecar path (Windows).
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn mutex_name(file: &Path) -> String {
        use sha2::Digest;
        let full = std::path::absolute(file).unwrap_or_else(|_| file.to_path_buf());
        let lower = full.to_string_lossy().to_lowercase();
        let digest = sha2::Sha256::digest(lower.as_bytes());
        let hex: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
        format!("Local\\SubtitleAdder.ProviderKeys.{hex}")
    }

    #[cfg(windows)]
    fn acquire_windows(file: &Path, timeout: Duration) -> Option<Self> {
        use windows::Win32::Foundation::{CloseHandle, WAIT_ABANDONED, WAIT_OBJECT_0};
        use windows::Win32::System::Threading::{CreateMutexW, WaitForSingleObject};
        use windows::core::HSTRING;
        let name = HSTRING::from(Self::mutex_name(file));
        // SAFETY: plain Win32 calls with a valid, NUL-terminated name; the handle is closed on every path.
        unsafe {
            let handle = CreateMutexW(None, false, &name).ok()?;
            let ms = timeout.as_millis().min(u32::MAX as u128 - 1) as u32;
            let wait = WaitForSingleObject(handle, ms);
            if wait == WAIT_OBJECT_0 || wait == WAIT_ABANDONED {
                Some(Self { handle })
            } else {
                let _ = CloseHandle(handle);
                None
            }
        }
    }

    #[cfg(unix)]
    fn acquire_unix(file: &Path, timeout: Duration) -> Option<Self> {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::OpenOptionsExt;
        let mut lock_name = file.as_os_str().to_owned();
        lock_name.push(".lock");
        let lock_path = PathBuf::from(lock_name);
        if let Some(parent) = lock_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC)
            .open(&lock_path)
            .ok()?;
        let deadline = Instant::now() + timeout;
        loop {
            // SAFETY: `f` owns a valid descriptor for the whole call.
            let rc = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if rc == 0 {
                return Some(Self { file: f });
            }
            let err = io::Error::last_os_error().raw_os_error().unwrap_or(0);
            if err == libc::EINTR {
                continue;
            }
            if (err == libc::EWOULDBLOCK || err == libc::EAGAIN) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
            return None;
        }
    }
}

impl Drop for CrossProcessLock {
    fn drop(&mut self) {
        #[cfg(windows)]
        // SAFETY: the handle was returned by CreateMutexW and is owned (and held) by this value.
        unsafe {
            use windows::Win32::Foundation::CloseHandle;
            use windows::Win32::System::Threading::ReleaseMutex;
            let _ = ReleaseMutex(self.handle);
            let _ = CloseHandle(self.handle);
        }
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            // SAFETY: the descriptor is valid until `self.file` drops right after this.
            unsafe {
                libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_and_present_files() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.json");
        assert!(matches!(read_file(&p), ReadOutcome::Missing));
        assert!(matches!(read_file(&dir.path().join("no/such/dir.json")), ReadOutcome::Missing));
        write_atomic(&p, b"{}").unwrap();
        assert!(matches!(read_file(&p), ReadOutcome::Content(c) if c == b"{}"));
        // No temp files are left behind.
        let names: Vec<_> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names.len(), 1);
    }

    #[test]
    fn bom_detection() {
        assert_eq!(decode_text(b"\xEF\xBB\xBF{}"), "{}");
        assert_eq!(decode_text(&[0xFF, 0xFE, b'{', 0, b'}', 0]), "{}");
        assert_eq!(decode_text(&[0xFE, 0xFF, 0, b'{', 0, b'}']), "{}");
        assert_eq!(decode_text(&[0xFF, 0xFE, 0, 0, b'{', 0, 0, 0, b'}', 0, 0, 0]), "{}");
        assert_eq!(decode_text(&[0, 0, 0xFE, 0xFF, 0, 0, 0, b'{', 0, 0, 0, b'}']), "{}");
        assert_eq!(decode_text("ğ".as_bytes()), "ğ");
    }

    #[test]
    fn aside_copies_get_numbered_names() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("translation-providers.json");
        std::fs::write(&p, b"broken").unwrap();
        let now = chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).unwrap();
        let a = preserve_damaged(&p, now).unwrap().unwrap();
        let b = preserve_damaged(&p, now).unwrap().unwrap();
        assert_eq!(a.file_name().unwrap(), "translation-providers.corrupt-19700101-000000.json");
        assert_eq!(b.file_name().unwrap(), "translation-providers.corrupt-19700101-000000-1.json");
        assert_eq!(std::fs::read(&a).unwrap(), b"broken");
        assert!(p.exists());
        assert!(preserve_damaged(&dir.path().join("gone.json"), now).unwrap().is_none());
    }

    #[test]
    fn lock_excludes_other_threads_until_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("k.json");
        let held = CrossProcessLock::acquire(&p, LOCK_TIMEOUT).expect("first lock");
        let p2 = p.clone();
        let other = std::thread::spawn(move || CrossProcessLock::acquire(&p2, Duration::from_millis(60)).is_some());
        assert!(!other.join().unwrap(), "a second holder must wait");
        drop(held);
        let p3 = p.clone();
        let again = std::thread::spawn(move || CrossProcessLock::acquire(&p3, Duration::from_millis(500)).is_some());
        assert!(again.join().unwrap());
    }

    #[test]
    fn mutex_name_shape() {
        let n = CrossProcessLock::mutex_name(Path::new("C:\\Data\\translation-provider-keys.json"));
        assert!(n.starts_with("Local\\SubtitleAdder.ProviderKeys."));
        assert_eq!(n.len(), "Local\\SubtitleAdder.ProviderKeys.".len() + 16);
        assert_eq!(n, CrossProcessLock::mutex_name(Path::new("c:\\data\\TRANSLATION-PROVIDER-KEYS.json")));
    }
}
