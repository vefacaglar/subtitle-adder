//! The POSIX side of `profiles.json` (mac-librarykit §9.3, §9.5): signatures, snapshots, atomic replacement, the
//! damaged-file aside copy, the cross-process `flock` and the folder watcher.
//!
//! Built on `std` so it also runs (for the tests) on Windows: `File::try_lock` is `flock(2)` on macOS and
//! `LockFileEx` on Windows; the signature's inode is the NTFS file index there.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::{ProfileError, Ticks};

/// `EINVAL`, `EIO`, `EACCES` (the codes the store reports itself).
pub const EINVAL: i32 = 22;
pub const EIO: i32 = 5;

/// What `profiles.json` held when it was last read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Snapshot {
    Missing,
    /// Exists but could not be read (permissions, I/O error).
    Unreadable,
    Content(Arc<[u8]>),
}

/// Cheap change detector: `stat` fields that change whenever the file is replaced or rewritten.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileSignature {
    pub size: u64,
    pub seconds: i64,
    pub nanoseconds: i64,
    pub inode: u64,
}

pub fn signature(path: &Path) -> Option<FileSignature> {
    let meta = std::fs::metadata(path).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some(FileSignature { size: meta.size(), seconds: meta.mtime(), nanoseconds: meta.mtime_nsec(), inode: meta.ino() })
    }
    #[cfg(not(unix))]
    {
        let (seconds, nanoseconds) = mtime(&meta);
        Some(FileSignature { size: meta.len(), seconds, nanoseconds, inode: file_index(path).unwrap_or(0) })
    }
}

/// Modification time as `(seconds, nanoseconds)` since the Unix epoch (negative seconds before it).
pub fn mtime(meta: &std::fs::Metadata) -> (i64, i64) {
    match meta.modified() {
        Ok(t) => match t.duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => (d.as_secs() as i64, i64::from(d.subsec_nanos())),
            Err(e) => {
                let d = e.duration();
                let nanos = i64::from(d.subsec_nanos());
                if nanos == 0 { (-(d.as_secs() as i64), 0) } else { (-(d.as_secs() as i64) - 1, 1_000_000_000 - nanos) }
            }
        },
        Err(_) => (0, 0),
    }
}

#[cfg(windows)]
fn file_index(path: &Path) -> Option<u64> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle};
    let file = File::open(path).ok()?;
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: the handle is a valid open file handle owned by `file` for the duration of the call.
    unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }.ok()?;
    Some((u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow))
}

#[cfg(not(any(unix, windows)))]
fn file_index(_path: &Path) -> Option<u64> {
    None
}

pub fn snapshot(path: &Path) -> Snapshot {
    match std::fs::read(path) {
        Ok(bytes) => Snapshot::Content(bytes.into()),
        Err(_) => {
            if signature(path).is_none() {
                Snapshot::Missing
            } else {
                Snapshot::Unreadable
            }
        }
    }
}

fn errno(e: &std::io::Error) -> i32 {
    e.raw_os_error().unwrap_or(EIO)
}

/// Opens a new file exclusively with mode 0600 (unix).
fn create_private(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// Writes `data` to `<dir>/.<name>.tmp-<UUID>` (mode 0600, fsynced) and renames it over `path`, so readers see either
/// the old or the new file, never half of one. Any failure removes the temp and reports `WriteFailed(errno)`.
pub fn write_atomically(data: &[u8], path: &Path) -> Result<(), ProfileError> {
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let temp = dir.join(format!(".{name}.tmp-{}", uuid::Uuid::new_v4().to_string().to_uppercase()));
    let mut file = create_private(&temp).map_err(|e| ProfileError::WriteFailed { errno: errno(&e) })?;
    let result = file.write_all(data).and_then(|()| fsync(&file));
    drop(file);
    let result = result.and_then(|()| rename_replacing(&temp, path));
    if let Err(e) = result {
        let _ = std::fs::remove_file(&temp);
        return Err(ProfileError::WriteFailed { errno: errno(&e) });
    }
    Ok(())
}

/// Plain `fsync(2)` like the Swift app (std's `sync_all` is `F_FULLFSYNC` on macOS, far slower per write).
fn fsync(file: &File) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;
        loop {
            // SAFETY: the descriptor is owned by `file` and open for the duration of the call.
            if unsafe { libc::fsync(file.as_raw_fd()) } == 0 {
                return Ok(());
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        file.sync_all()
    }
}

/// `rename(2)`; on Windows a few retries ride out a scanner holding the target for a moment.
fn rename_replacing(from: &Path, to: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        let mut attempt = 0;
        loop {
            match std::fs::rename(from, to) {
                Err(e) if attempt < 5 && e.kind() == std::io::ErrorKind::PermissionDenied => {
                    attempt += 1;
                    std::thread::sleep(Duration::from_millis(10 * attempt));
                }
                other => return other,
            }
        }
    }
    #[cfg(not(windows))]
    {
        std::fs::rename(from, to)
    }
}

/// Keeps the current (damaged) file as `<stem>.corrupt-<yyyyMMdd-HHmmss>[-N].<ext>` before anything new replaces it:
/// a hard link (the name never disappears for lock-free readers), a copy where links fail. `Ok(None)` when there was
/// nothing left to keep.
pub fn preserve_damaged(path: &Path, now: Ticks) -> Result<Option<PathBuf>, ProfileError> {
    let folder = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = path.extension().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let stamp = super::dates::compact_timestamp(now);
    for attempt in 0..1000 {
        let suffix = if attempt == 0 { String::new() } else { format!("-{attempt}") };
        let target = folder.join(format!("{stem}.corrupt-{stamp}{suffix}.{ext}"));
        if target.exists() {
            continue;
        }
        match std::fs::hard_link(path, &target) {
            Ok(()) => return Ok(Some(target)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => {
                return match std::fs::copy(path, &target) {
                    Ok(_) => Ok(Some(target)),
                    Err(_) => Err(ProfileError::CorruptFileNotPreserved),
                };
            }
        }
    }
    Err(ProfileError::CorruptFileNotPreserved)
}

/// An exclusive lock on `profiles.json.lock`, held around every read-modify-write. The kernel drops it if the
/// process dies; two stores in one process contend like two processes (separate open files).
#[derive(Debug)]
pub struct FileLock {
    file: Option<File>,
}

impl FileLock {
    /// Polls (5 ms) for up to `timeout`; never proceeds without the lock. `timeout = 0` is a single try.
    pub fn acquire(path: &Path, timeout: Duration) -> Result<FileLock, ProfileError> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(path).map_err(|_| ProfileError::LockUnavailable)?;
        let deadline = Instant::now() + timeout;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(FileLock { file: Some(file) }),
                Err(std::fs::TryLockError::WouldBlock) => {
                    if Instant::now() >= deadline {
                        return Err(ProfileError::LockUnavailable);
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(std::fs::TryLockError::Error(e)) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(std::fs::TryLockError::Error(_)) => return Err(ProfileError::LockUnavailable),
            }
        }
    }

    pub fn release(&mut self) {
        if let Some(file) = self.file.take() {
            let _ = file.unlock();
        }
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        self.release();
    }
}

/// Watches the folder that holds `profiles.json` (a file watch goes stale at the first atomic replace) and calls
/// `on_change` once per burst of activity, `debounce` after the last event. macOS: kqueue `EVFILT_VNODE` on the
/// folder (write/rename/delete); elsewhere (tests) a 100 ms poll of the file's signature.
pub struct FolderWatcher {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl FolderWatcher {
    /// Arms the watch before returning (so a change made right after construction is never missed), then runs the
    /// event loop on its own thread.
    pub fn start(file: PathBuf, debounce: Duration, on_change: Box<dyn Fn() + Send>) -> Option<FolderWatcher> {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let source = EventSource::arm(&file);
        let thread = std::thread::Builder::new()
            .name("profile-watcher".into())
            .spawn(move || run_watcher(source, debounce, flag, on_change))
            .ok()?;
        Some(FolderWatcher { stop, thread: Some(thread) })
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            // The last owner may be the watcher thread itself (its callback held the store for a moment).
            if thread.thread().id() != std::thread::current().id() {
                let _ = thread.join();
            }
        }
    }
}

impl Drop for FolderWatcher {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Debounce state shared by both event sources.
struct Debounce {
    pending: Option<Instant>,
    delay: Duration,
}

impl Debounce {
    fn event(&mut self) {
        self.pending = Some(Instant::now() + self.delay);
    }
    fn due(&mut self) -> bool {
        match self.pending {
            Some(at) if Instant::now() >= at => {
                self.pending = None;
                true
            }
            _ => false,
        }
    }
}

/// Where change events come from: kqueue on the folder (macOS), else a 100 ms poll of the file's signature.
enum EventSource {
    #[cfg(target_os = "macos")]
    Kqueue(kqueue::Watch),
    Poll { file: PathBuf, last: Option<FileSignature> },
}

impl EventSource {
    fn arm(file: &Path) -> EventSource {
        #[cfg(target_os = "macos")]
        if let Some(watch) = kqueue::Watch::arm(file) {
            return EventSource::Kqueue(watch);
        }
        EventSource::Poll { file: file.to_path_buf(), last: signature(file) }
    }

    /// Waits up to 100 ms; true if something changed.
    fn wait(&mut self) -> bool {
        match self {
            #[cfg(target_os = "macos")]
            EventSource::Kqueue(watch) => watch.wait(),
            EventSource::Poll { file, last } => {
                std::thread::sleep(Duration::from_millis(100));
                let now = signature(file);
                if now != *last {
                    *last = now;
                    true
                } else {
                    false
                }
            }
        }
    }
}

fn run_watcher(mut source: EventSource, debounce: Duration, stop: Arc<AtomicBool>, on_change: Box<dyn Fn() + Send>) {
    let mut state = Debounce { pending: None, delay: debounce };
    while !stop.load(Ordering::SeqCst) {
        if source.wait() {
            state.event();
        }
        if state.due() && !stop.load(Ordering::SeqCst) {
            on_change();
        }
    }
}

#[cfg(target_os = "macos")]
mod kqueue {
    use std::ffi::CString;
    use std::path::Path;

    /// A kqueue with an `EVFILT_VNODE` filter (write/rename/delete) on the folder's descriptor.
    pub struct Watch {
        kq: libc::c_int,
        dir: libc::c_int,
    }

    // SAFETY: the descriptors are plain integers owned by this value; kqueue calls are thread-safe.
    unsafe impl Send for Watch {}

    impl Watch {
        pub fn arm(file: &Path) -> Option<Watch> {
            use std::os::unix::ffi::OsStrExt;
            let folder = file.parent()?;
            let c_path = CString::new(folder.as_os_str().as_bytes()).ok()?;
            // SAFETY: plain libc calls on descriptors this function owns (closed on failure or by `Drop`).
            unsafe {
                let dir = libc::open(c_path.as_ptr(), libc::O_EVTONLY | libc::O_CLOEXEC);
                if dir < 0 {
                    return None;
                }
                let kq = libc::kqueue();
                if kq < 0 {
                    libc::close(dir);
                    return None;
                }
                let mut change: libc::kevent = std::mem::zeroed();
                change.ident = dir as libc::uintptr_t;
                change.filter = libc::EVFILT_VNODE;
                change.flags = libc::EV_ADD | libc::EV_CLEAR;
                change.fflags = libc::NOTE_WRITE | libc::NOTE_RENAME | libc::NOTE_DELETE;
                if libc::kevent(kq, &change, 1, std::ptr::null_mut(), 0, std::ptr::null()) < 0 {
                    libc::close(kq);
                    libc::close(dir);
                    return None;
                }
                Some(Watch { kq, dir })
            }
        }

        /// Waits up to 100 ms for an event on the folder.
        pub fn wait(&mut self) -> bool {
            let timeout = libc::timespec { tv_sec: 0, tv_nsec: 100_000_000 };
            // SAFETY: `self.kq` is a live kqueue; `event` is a valid out buffer of one element.
            unsafe {
                let mut event: libc::kevent = std::mem::zeroed();
                libc::kevent(self.kq, std::ptr::null(), 0, &mut event, 1, &timeout) > 0
            }
        }
    }

    impl Drop for Watch {
        fn drop(&mut self) {
            // SAFETY: both descriptors are owned by this value and closed once.
            unsafe {
                libc::close(self.kq);
                libc::close(self.dir);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_replaces_and_leaves_no_temp() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("profiles.json");
        write_atomically(b"one", &file).unwrap();
        let first = signature(&file).unwrap();
        write_atomically(b"two", &file).unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"two");
        assert_ne!(signature(&file).unwrap(), first, "a replaced file has a new signature");
        let names: Vec<_> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names.len(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }

    #[test]
    fn damaged_files_are_kept_under_numbered_names() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("profiles.json");
        std::fs::write(&file, b"broken").unwrap();
        let now = super::super::dates::parse("2026-05-01T10:20:30Z").unwrap();
        let a = preserve_damaged(&file, now).unwrap().unwrap();
        let b = preserve_damaged(&file, now).unwrap().unwrap();
        assert_eq!(a.file_name().unwrap(), "profiles.corrupt-20260501-102030.json");
        assert_eq!(b.file_name().unwrap(), "profiles.corrupt-20260501-102030-1.json");
        assert_eq!(std::fs::read(&b).unwrap(), b"broken");
        assert!(file.exists());
        assert_eq!(preserve_damaged(&dir.path().join("missing.json"), now).unwrap(), None);
    }

    #[test]
    fn the_file_lock_excludes_a_second_holder_and_times_out() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.json.lock");
        let mut held = FileLock::acquire(&path, Duration::from_secs(1)).unwrap();
        let started = Instant::now();
        assert_eq!(FileLock::acquire(&path, Duration::from_millis(50)).unwrap_err(), ProfileError::LockUnavailable);
        assert!(started.elapsed() >= Duration::from_millis(45));
        assert!(FileLock::acquire(&path, Duration::ZERO).is_err());
        held.release();
        assert!(FileLock::acquire(&path, Duration::ZERO).is_ok());
    }

    #[test]
    fn snapshots_tell_missing_from_content() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("x.json");
        assert_eq!(snapshot(&file), Snapshot::Missing);
        std::fs::write(&file, b"{}").unwrap();
        assert_eq!(snapshot(&file), Snapshot::Content(Arc::from(&b"{}"[..])));
    }
}
