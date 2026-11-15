//! macOS backend (mac-librarykit §15–§16): port of LibraryKit's `MediaLibraryIndex` with the same
//! `D/media-index.json` (version 1, compact, sorted keys, millisecond `Z` dates).
//!
//! * Construction does no file work; the file is read on the first query (`ensure_loaded`), reads never write.
//! * Folder keys are `nfc(lowercase(normalized path))`; folders keep the case they were scanned with.
//! * Scans may overlap: the listing itself runs without the state lock, every scan takes a ticket when it begins and a
//!   result older than the newest applied scan of that folder (or of anything below it) is dropped, so the order
//!   scans finish in never decides which listing wins (`MediaIndexOverlapTests`).
//! * A missing folder drops its subtree (and its entry in the parent); an unreadable one keeps its cached entry.
//! * Change notifications fire after the file was written; [`MacMediaIndex::changes`] buffers the newest one.
//!
//! The module compiles on every platform for the tests (virtual listers); the real lister is macOS-only.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender, TrySendError};
use parking_lot::Mutex;
use sa_core::cancel::CancellationToken;
use sa_core::subscription::{Subscribers, Subscription};

use crate::dates;
use crate::schema::{DateStyle, DirectorySnapshot, IndexedMediaFile, MediaIndexDocument};
use crate::text::{fold, ordered_cmp, swift_cmp};
use crate::{MediaIndex, ScanResult};

pub mod listing;
pub mod path;
#[cfg(target_os = "macos")]
pub mod scanner;

#[cfg(test)]
mod tests;

pub use listing::{RawEntry, SubtitleBadgeLookup, build_listing};

/// Folder lister (injectable for the overlap tests). Returns `Listed`, `Missing` or `Unreadable` for an already
/// normalized folder path.
pub trait FolderLister: Send + Sync {
    fn list(&self, directory: &Path) -> ScanResult;
}

/// The lister used when none is injected: the real file system on macOS; elsewhere (tests only) every folder is
/// unreadable.
struct DefaultLister;

impl FolderLister for DefaultLister {
    fn list(&self, directory: &Path) -> ScanResult {
        #[cfg(target_os = "macos")]
        {
            scanner::list(&path::from_path(directory), dates::now_ticks())
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = directory;
            ScanResult::Unreadable
        }
    }
}

struct Entry {
    snapshot: DirectorySnapshot,
    folded_path: String,
    folded_names: Vec<String>,
}

impl Entry {
    fn new(snapshot: DirectorySnapshot) -> Self {
        let folded_path = fold(&snapshot.directory_path);
        let folded_names = snapshot.media_files.iter().map(|f| fold(&f.file_name)).collect();
        Self { snapshot, folded_path, folded_names }
    }
}

#[derive(Default)]
struct State {
    loaded: bool,
    entries: HashMap<String, Entry>,
    scan_counter: i64,
    applied_scan: HashMap<String, i64>,
}

/// What a scan found (LibraryKit `ScanOutcome` with its snapshot).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MacScanOutcome {
    /// The folder was listed and its entry replaced (or a newer scan's entry is reported).
    Scanned(DirectorySnapshot),
    /// Gone; it and everything below it was dropped.
    Missing(DirectorySnapshot),
    /// Exists but could not be listed; the kept entry (empty if never indexed).
    Unreadable(DirectorySnapshot),
}

impl MacScanOutcome {
    pub fn snapshot(&self) -> &DirectorySnapshot {
        match self {
            MacScanOutcome::Scanned(s) | MacScanOutcome::Missing(s) | MacScanOutcome::Unreadable(s) => s,
        }
    }
    fn into_scan_result(self) -> ScanResult {
        match self {
            MacScanOutcome::Scanned(s) => ScanResult::Listed(s),
            MacScanOutcome::Missing(_) => ScanResult::Missing,
            MacScanOutcome::Unreadable(_) => ScanResult::Unreadable,
        }
    }
}

/// `media-index.json` (macOS rules).
pub struct MacMediaIndex {
    file: PathBuf,
    lister: Arc<dyn FolderLister>,
    state: Mutex<State>,
    write_lock: Mutex<()>,
    subscribers: Subscribers<()>,
    channels: Mutex<Vec<Sender<()>>>,
}

fn is_superseded(st: &State, key: &str, ticket: i64) -> bool {
    st.applied_scan.get(key).copied().unwrap_or(0) > ticket
}

fn under(candidate: &str, key: &str) -> bool {
    let prefix = if key == "/" { "/".to_owned() } else { format!("{key}/") };
    candidate == key || candidate.starts_with(&prefix)
}

/// Whether a scan that began after `ticket` applied a listing of `key` or of a folder below it still indexed.
fn has_newer_entry(st: &State, key: &str, ticket: i64) -> bool {
    st.entries.keys().any(|k| under(k, key) && st.applied_scan.get(k).copied().unwrap_or(0) > ticket)
}

fn current(st: &State, key: &str, fallback: DirectorySnapshot) -> MacScanOutcome {
    match st.entries.get(key) {
        Some(e) => MacScanOutcome::Scanned(e.snapshot.clone()),
        None => MacScanOutcome::Missing(fallback),
    }
}

/// Drops `path` and everything below it, and `path` from its parent's subfolder list.
fn remove_subtree(st: &mut State, folder: &str, ticket: i64) -> bool {
    let key = path::key(folder);
    let doomed: Vec<String> = st.entries.keys().filter(|k| under(k, &key)).cloned().collect();
    for k in &doomed {
        st.entries.remove(k);
        st.applied_scan.insert(k.clone(), ticket);
    }
    st.applied_scan.insert(key.clone(), ticket);
    let mut changed = !doomed.is_empty();
    if let Some(parent) = path::parent(&path::normalized(folder)).map(|p| path::key(&p))
        && let Some(entry) = st.entries.get_mut(&parent)
        && entry.snapshot.subdirectories.iter().any(|s| path::key(s) == key)
    {
        entry.snapshot.subdirectories.retain(|s| path::key(s) != key);
        changed = true;
    }
    changed
}

impl MacMediaIndex {
    /// Nothing is read or created here. `file = None` is `~/Library/Application Support/SubtitleAdder/
    /// media-index.json`; `lister = None` is the real file system.
    pub fn new(file: Option<PathBuf>, lister: Option<Arc<dyn FolderLister>>) -> Arc<Self> {
        Arc::new(Self {
            file: file.unwrap_or_else(Self::default_file),
            lister: lister.unwrap_or_else(|| Arc::new(DefaultLister)),
            state: Mutex::new(State::default()),
            write_lock: Mutex::new(()),
            subscribers: Subscribers::new(),
            channels: Mutex::new(Vec::new()),
        })
    }

    /// `<Application Support>/SubtitleAdder/media-index.json`.
    pub fn default_file() -> PathBuf {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"));
        home.join("Library").join("Application Support").join("SubtitleAdder").join("media-index.json")
    }

    pub fn file(&self) -> &Path {
        &self.file
    }

    /// A new coalescing change stream (buffers the newest change); disconnected when the index is dropped.
    pub fn changes(&self) -> Receiver<()> {
        let (tx, rx) = crossbeam_channel::bounded(1);
        self.channels.lock().push(tx);
        rx
    }

    fn ensure_loaded(&self, st: &mut State) {
        if st.loaded {
            return;
        }
        st.loaded = true;
        // A missing, unreadable, corrupt or newer-format file starts an empty index; the next scan rewrites it.
        let Ok(bytes) = std::fs::read(&self.file) else { return };
        let Some(text) = decode_json_bytes(&bytes) else { return };
        let Some(doc) = MediaIndexDocument::parse_apple(&text) else { return };
        for mut snapshot in doc.directories {
            snapshot.directory_path = path::normalized(&snapshot.directory_path);
            if snapshot.directory_path.is_empty() {
                continue;
            }
            snapshot.subdirectories = snapshot.subdirectories.iter().map(|s| path::normalized(s)).filter(|s| !s.is_empty()).collect();
            snapshot.subdirectories.sort_by(|a, b| ordered_cmp(a, b));
            snapshot.media_files.retain(|f| !f.full_path.is_empty());
            snapshot.media_files.sort_by(|a, b| ordered_cmp(&a.file_name, &b.file_name));
            st.entries.insert(path::key(&snapshot.directory_path), Entry::new(snapshot));
        }
    }

    /// Every indexed folder, sorted by `ordered_before`.
    pub fn indexed_directories(&self) -> Vec<String> {
        let mut st = self.state.lock();
        self.ensure_loaded(&mut st);
        let mut dirs: Vec<String> = st.entries.values().map(|e| e.snapshot.directory_path.clone()).collect();
        dirs.sort_by(|a, b| ordered_cmp(a, b));
        dirs
    }

    /// The media files of a folder as last scanned; empty if never indexed.
    pub fn cached_directory(&self, dir: &Path) -> Vec<IndexedMediaFile> {
        self.cached_snapshot(dir).map(|s| s.media_files).unwrap_or_default()
    }

    /// The subfolders of a folder as last scanned; empty if never indexed.
    pub fn cached_subdirectories(&self, dir: &Path) -> Vec<String> {
        self.cached_snapshot(dir).map(|s| s.subdirectories).unwrap_or_default()
    }

    /// `scan(directory:)` with LibraryKit's outcome.
    pub fn scan_folder_outcome(&self, dir: &Path) -> MacScanOutcome {
        {
            let mut st = self.state.lock();
            self.ensure_loaded(&mut st);
        }
        let (result, changed) = self.scan_folder(&path::normalized(&path::from_path(dir)));
        if changed {
            self.persist_and_notify();
        }
        result
    }

    /// `crawl(from:maxDepth:maxFolders:progress:)`: breadth-first, depth clamped to 0…5 (root = 0), at most
    /// `max_folders`; cancellation is checked at folder boundaries; written once at the end if anything changed.
    /// Returns the number of folders listed.
    pub fn crawl_folders(&self, root: &Path, max_depth: i32, max_folders: usize, cancel: &CancellationToken, progress: &dyn Fn(usize)) -> usize {
        {
            let mut st = self.state.lock();
            self.ensure_loaded(&mut st);
        }
        let depth_limit = max_depth.clamp(0, 5);
        let root = path::normalized(&path::from_path(root));
        let mut queue: VecDeque<(String, i32)> = VecDeque::from([(root.clone(), 0)]);
        let mut visited: HashSet<String> = HashSet::from([path::key(&root)]);
        let (mut next, mut listed, mut changed) = (0usize, 0usize, false);
        while next < max_folders && !cancel.is_cancelled() {
            let Some((folder, depth)) = queue.pop_front() else { break };
            next += 1;
            let (result, did_change) = self.scan_folder(&folder);
            changed |= did_change;
            progress(next);
            let MacScanOutcome::Scanned(snapshot) = result else { continue };
            listed += 1;
            if depth < depth_limit {
                for sub in snapshot.subdirectories {
                    if visited.insert(path::key(&sub)) {
                        queue.push_back((sub, depth + 1));
                    }
                }
            }
        }
        if changed {
            self.persist_and_notify();
        }
        listed
    }

    /// Scans one normalized folder into memory without writing; `changed` says whether memory was modified.
    fn scan_folder(&self, folder: &str) -> (MacScanOutcome, bool) {
        let now = dates::now_ticks();
        let empty = DirectorySnapshot::empty(folder, now);
        if folder.is_empty() {
            return (MacScanOutcome::Missing(empty), false);
        }
        let ticket = {
            let mut st = self.state.lock();
            st.scan_counter += 1;
            st.scan_counter
        };
        let key = path::key(folder);

        // The listing runs without the lock: queries stay responsive and scans may overlap.
        let listing = self.lister.list(Path::new(folder));

        let mut st = self.state.lock();
        match listing {
            ScanResult::Missing => {
                if is_superseded(&st, &key, ticket) || has_newer_entry(&st, &key, ticket) {
                    return (current(&st, &key, empty), false);
                }
                let changed = remove_subtree(&mut st, folder, ticket);
                (MacScanOutcome::Missing(empty), changed)
            }
            ScanResult::Unreadable | ScanResult::Cancelled => {
                let kept = st.entries.get(&key).map(|e| e.snapshot.clone()).unwrap_or(empty);
                (MacScanOutcome::Unreadable(kept), false)
            }
            ScanResult::Listed(mut snapshot) => {
                if is_superseded(&st, &key, ticket) {
                    return (current(&st, &key, empty), false);
                }
                let previous = st.entries.get(&key).map(|e| e.snapshot.subdirectories.clone()).unwrap_or_default();
                let listed: HashSet<String> = snapshot.subdirectories.iter().map(|s| path::key(s)).collect();
                let mut gone = Vec::new();
                let mut kept = Vec::new();
                for old in previous.into_iter().filter(|o| !listed.contains(&path::key(o))) {
                    if has_newer_entry(&st, &path::key(&old), ticket) {
                        kept.push(old);
                    } else {
                        gone.push(old);
                    }
                }
                if !kept.is_empty() {
                    snapshot.subdirectories.extend(kept);
                    snapshot.subdirectories.sort_by(|a, b| ordered_cmp(a, b));
                }
                st.entries.insert(key.clone(), Entry::new(snapshot.clone()));
                st.applied_scan.insert(key, ticket);
                for old in gone {
                    remove_subtree(&mut st, &old, ticket);
                }
                (MacScanOutcome::Scanned(snapshot), true)
            }
        }
    }

    /// Writes atomically (best effort: the index is a cache), then notifies (also after a failed write).
    fn persist_and_notify(&self) {
        {
            let _write = self.write_lock.lock();
            let doc = {
                let st = self.state.lock();
                let mut directories: Vec<DirectorySnapshot> = st.entries.values().map(|e| e.snapshot.clone()).collect();
                directories.sort_by(|a, b| ordered_cmp(&a.directory_path, &b.directory_path));
                MediaIndexDocument { version: MediaIndexDocument::APPLE_VERSION, directories }
            };
            let json = doc.to_json(DateStyle::Apple);
            let _ = write_atomically(&self.file, json.as_bytes());
        }
        self.subscribers.notify(&());
        self.channels.lock().retain(|tx| !matches!(tx.try_send(()), Err(TrySendError::Disconnected(_))));
    }
}

/// The text `JSONDecoder` reads from `Data`: UTF-8, or UTF-16/UTF-32 announced by a byte order mark (the mark itself
/// is skipped, a UTF-8 one included). Invalid sequences make the file unusable (`None`), as a decoding error does.
fn decode_json_bytes(bytes: &[u8]) -> Option<String> {
    fn utf16(rest: &[u8], le: bool) -> Option<String> {
        if rest.len() % 2 != 0 {
            return None;
        }
        let units = rest.chunks_exact(2).map(|c| if le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) });
        char::decode_utf16(units).collect::<Result<String, _>>().ok()
    }
    fn utf32(rest: &[u8], le: bool) -> Option<String> {
        if rest.len() % 4 != 0 {
            return None;
        }
        rest.chunks_exact(4)
            .map(|c| char::from_u32(if le { u32::from_le_bytes([c[0], c[1], c[2], c[3]]) } else { u32::from_be_bytes([c[0], c[1], c[2], c[3]]) }))
            .collect()
    }
    match bytes {
        [0xEF, 0xBB, 0xBF, rest @ ..] => std::str::from_utf8(rest).ok().map(str::to_owned),
        [0xFF, 0xFE, 0x00, 0x00, rest @ ..] => utf32(rest, true),
        [0x00, 0x00, 0xFE, 0xFF, rest @ ..] => utf32(rest, false),
        [0xFF, 0xFE, rest @ ..] => utf16(rest, true),
        [0xFE, 0xFF, rest @ ..] => utf16(rest, false),
        _ => std::str::from_utf8(bytes).ok().map(str::to_owned),
    }
}

/// `Data.write(to:options: .atomic)`: a temp file in the same folder, then a rename; the folder is created first.
fn write_atomically(file: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let temp = file.with_file_name(format!(".{name}.tmp-{}", uuid::Uuid::new_v4()));
    let result = std::fs::write(&temp, bytes).and_then(|_| std::fs::rename(&temp, file));
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

impl MediaIndex for MacMediaIndex {
    fn scan(&self, dir: &Path, cancel: &CancellationToken) -> ScanResult {
        if cancel.is_cancelled() {
            return ScanResult::Cancelled;
        }
        self.scan_folder_outcome(dir).into_scan_result()
    }

    fn crawl(&self, root: &Path, max_depth: u8, max_folders: usize, cancel: &CancellationToken, progress: &dyn Fn(usize)) -> usize {
        self.crawl_folders(root, i32::from(max_depth), max_folders, cancel, progress)
    }

    fn cached_snapshot(&self, dir: &Path) -> Option<DirectorySnapshot> {
        let mut st = self.state.lock();
        self.ensure_loaded(&mut st);
        st.entries.get(&path::key(&path::from_path(dir))).map(|e| e.snapshot.clone())
    }

    fn total_indexed_files(&self) -> usize {
        let mut st = self.state.lock();
        self.ensure_loaded(&mut st);
        st.entries.values().map(|e| e.snapshot.media_files.len()).sum()
    }

    /// Name starts with the folded query (3), contains it (2), or lies in a folder whose path contains it (1); best
    /// score, then newest, then name. A blank query finds nothing (use `all_files`). **Never called logged out.**
    fn search(&self, query: &str, limit: usize) -> Vec<IndexedMediaFile> {
        let mut st = self.state.lock();
        self.ensure_loaded(&mut st);
        let needle = fold(query.trim());
        if needle.is_empty() || limit == 0 {
            return Vec::new();
        }
        let mut hits: Vec<(&IndexedMediaFile, u8, &str)> = Vec::new();
        for entry in st.entries.values() {
            let path_matches = entry.folded_path.contains(&needle);
            for (file, name) in entry.snapshot.media_files.iter().zip(&entry.folded_names) {
                let score = if name.starts_with(&needle) {
                    3
                } else if name.contains(&needle) {
                    2
                } else if path_matches {
                    1
                } else {
                    0
                };
                if score > 0 {
                    hits.push((file, score, name));
                }
            }
        }
        hits.sort_by(|a, b| {
            b.1.cmp(&a.1)
                .then_with(|| b.0.last_write_time_utc.cmp(&a.0.last_write_time_utc))
                .then_with(|| swift_cmp(a.2, b.2))
                .then_with(|| swift_cmp(&a.0.full_path, &b.0.full_path))
        });
        hits.into_iter().take(limit).map(|(f, _, _)| f.clone()).collect()
    }

    /// Every indexed file, most recently modified first, then by name. **Never called logged out.**
    fn all_files(&self, limit: usize) -> Vec<IndexedMediaFile> {
        let mut st = self.state.lock();
        self.ensure_loaded(&mut st);
        if limit == 0 {
            return Vec::new();
        }
        let mut files: Vec<IndexedMediaFile> = st.entries.values().flat_map(|e| e.snapshot.media_files.iter().cloned()).collect();
        files.sort_by(|a, b| {
            b.last_write_time_utc.cmp(&a.last_write_time_utc).then_with(|| {
                // Swift `!=` compares canonically (NFC), not byte by byte.
                if swift_cmp(&a.file_name, &b.file_name) != std::cmp::Ordering::Equal {
                    ordered_cmp(&a.file_name, &b.file_name)
                } else {
                    swift_cmp(&a.full_path, &b.full_path)
                }
            })
        });
        files.truncate(limit);
        files
    }

    fn subscribe_changes(&self, handler: Box<dyn Fn() + Send + Sync>) -> Subscription {
        self.subscribers.subscribe(move |_| handler())
    }

    fn invalidate_sidecar(&self, video_path: &Path) {
        let video = path::normalized(&path::from_path(video_path));
        let Some(parent) = path::parent(&video) else { return };
        if self.cached_snapshot(Path::new(&parent)).is_some() {
            let _ = self.scan_folder_outcome(Path::new(&parent));
        }
    }

    /// The folder as it is on disk, without touching the index in memory or on disk and without a change
    /// notification (the explorer's search below the current folder).
    fn scan_transient(&self, dir: &Path, cancel: &CancellationToken) -> ScanResult {
        if cancel.is_cancelled() {
            return ScanResult::Cancelled;
        }
        let folder = path::normalized(&path::from_path(dir));
        if folder.is_empty() {
            return ScanResult::Missing;
        }
        self.lister.list(Path::new(&folder))
    }
}
