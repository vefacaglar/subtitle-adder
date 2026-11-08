//! Slot files ("this broker is alive") and the engine lock file ("this process hosts the pipe server"),
//! win-sync §3. Both are zero-length; the open handle is the signal.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const DELETE: u32 = 0x0001_0000;
const FILE_FLAG_DELETE_ON_CLOSE: u32 = 0x0400_0000;

/// `<instances>\instance-<id>.slot`.
pub(crate) fn slot_path(instances: &Path, id: &str) -> PathBuf {
    instances.join(format!("instance-{id}.slot"))
}

/// C# `FileStream(Create, ReadWrite, FileShare.None, DeleteOnClose)`; `None` on any failure.
pub(crate) fn create_slot(instances: &Path, id: &str) -> Option<File> {
    std::fs::create_dir_all(instances).ok()?;
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .access_mode(GENERIC_READ | GENERIC_WRITE | DELETE)
        .share_mode(0)
        .custom_flags(FILE_FLAG_DELETE_ON_CLOSE)
        .open(slot_path(instances, id))
        .ok()
}

/// `<base>\engine-<hash16>.lock`.
pub(crate) fn lock_path(base: &Path, hash: &str) -> PathBuf {
    base.join(format!("engine-{hash}.lock"))
}

/// C# `FileStream(OpenOrCreate, ReadWrite, FileShare.None)`; `None` = held elsewhere (or not creatable).
pub(crate) fn try_acquire_lock(base: &Path, hash: &str) -> Option<File> {
    let _ = std::fs::create_dir_all(base);
    OpenOptions::new().read(true).write(true).create(true).share_mode(0).open(lock_path(base, hash)).ok()
}

fn is_slot_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.starts_with("instance-") && n.ends_with(".slot")
}

/// C# `CountActiveSlotFiles` (win-sync §3.3): tries to delete every `instance-*.slot`; a deletable one was an
/// orphan, a refused one (sharing violation / access denied) is a live broker. Missing dir → `Some(0)`; an
/// enumeration failure → `None` (the caller falls back to its in-process count).
pub(crate) fn count_active_slots(instances: &Path) -> Option<usize> {
    let rd = match std::fs::read_dir(instances) {
        Ok(rd) => rd,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Some(0),
        Err(_) => return None,
    };
    let mut active = 0;
    for entry in rd {
        let entry = entry.ok()?;
        if !entry.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        if !is_slot_name(&entry.file_name().to_string_lossy()) {
            continue;
        }
        match std::fs::remove_file(entry.path()) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(_) => active += 1,
        }
    }
    Some(active)
}
