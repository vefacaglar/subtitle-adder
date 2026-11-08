//! `WinMediaIndex`: port of the current C# `MediaLibraryIndex` (`media-index.json` version 2).
//!
//! * Keys are OrdinalIgnoreCase; folders are normalized with `NormalizeDirectoryPath`.
//! * No scan or update time is recorded (`scannedAtUtc`/`updatedAtUtc` are not written, `indexedAtUtc` is
//!   `0001-01-01T00:00:00+00:00`), and the file is rewritten only when a folder's listing actually changed, so neither
//!   the content nor the file's modification time records when a video was opened.
//! * A file below version 2 is a legacy index (it recorded the folders of opened videos with scan times) and is
//!   replaced by an empty index once, in the constructor.
//! * Cross-instance freshness: after a successful write a `MediaIndexed` event is published on the `media-index`
//!   topic; events of other instances for the same file reload it.
//! * Lock order: file-write lock, then state lock; subscribers are never called while holding either.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};

use crossbeam_channel::{Receiver, Sender, TrySendError};
use parking_lot::Mutex;
use sa_core::cancel::CancellationToken;
use sa_core::subscription::{Subscribers, Subscription};
use sa_sync::{EventKind, StateSyncEvent, SyncBus, Topic, TopicFilter};

use super::dict::DotNetDict;
use super::fsio::{self, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_SYSTEM};
use super::paths;
use crate::dates;
use crate::schema::{DateStyle, DirectorySnapshot, DotNetRead, IndexedMediaFile, MediaIndexDocument};
use crate::text::{CiKey, cmp_ordinal_ignore_case, contains_ordinal_ignore_case, eq_ordinal_ignore_case, starts_with_ordinal_ignore_case};
use crate::{MediaIndex, ScanResult};

/// `CrawlSubtreeAsync` clamp of `max_depth`.
pub const CRAWL_MAX_DEPTH: u8 = 5;
/// Folders scanned per crawl at most (root included).
pub const CRAWL_MAX_DIRECTORIES: usize = 64;
/// `Search` default `maxResults`.
pub const SEARCH_DEFAULT_MAX_RESULTS: usize = 100;

#[derive(Debug, Clone)]
struct DirEntry {
    directory_path: String,
    subdirectories: Vec<String>,
    media_files: Vec<IndexedMediaFile>,
}

impl DirEntry {
    fn snapshot(&self) -> DirectorySnapshot {
        DirectorySnapshot {
            directory_path: self.directory_path.clone(),
            subdirectories: self.subdirectories.clone(),
            media_files: self.media_files.clone(),
            scanned_at_utc: 0,
        }
    }
}

/// Both maps enumerate like the C# `Dictionary`s (free-slot reuse included), so search ties keep the C# order.
#[derive(Default)]
struct State {
    directories: DotNetDict<DirEntry>,
    files_by_path: DotNetDict<IndexedMediaFile>,
}

struct Cancelled;

/// `media-index.json` (Windows rules).
pub struct WinMediaIndex {
    file_path: PathBuf,
    file_path_text: String,
    instance_id: String,
    state: Mutex<State>,
    file_write: Mutex<()>,
    bus: Option<Arc<dyn SyncBus>>,
    bus_subscription: Mutex<Option<Subscription>>,
    changed: Subscribers<()>,
    channels: Mutex<Vec<Sender<()>>>,
}

fn subdirs_equal(a: &[String], b: &[String]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| eq_ordinal_ignore_case(x, y))
}

impl WinMediaIndex {
    /// Loads from disk (a legacy file is replaced by an empty index at once) and subscribes to the `media-index`
    /// topic. `file = None` is `%LOCALAPPDATA%\SubtitleAdder\media-index.json`.
    pub fn new(file: Option<PathBuf>, bus: Option<Arc<dyn SyncBus>>) -> Arc<Self> {
        let file_path = match file.filter(|f| !f.as_os_str().to_string_lossy().trim().is_empty()) {
            Some(f) => std::path::absolute(&f).unwrap_or(f),
            None => Self::default_path(&fsio::local_app_data().unwrap_or_default().join("SubtitleAdder")),
        };
        let index = Arc::new(Self {
            file_path_text: file_path.to_string_lossy().into_owned(),
            file_path,
            instance_id: sa_core::text::guid_n(),
            state: Mutex::new(State::default()),
            file_write: Mutex::new(()),
            bus,
            bus_subscription: Mutex::new(None),
            changed: Subscribers::new(),
            channels: Mutex::new(Vec::new()),
        });
        if index.load_from_disk() == LoadResult::Legacy {
            // Earlier versions persisted the folder of every opened video with its scan time: replace the whole file.
            index.save_to_disk();
        }
        if let Some(bus) = &index.bus {
            let weak: Weak<Self> = Arc::downgrade(&index);
            let sub = bus.subscribe(
                TopicFilter::Only(Topic::MediaIndex),
                Box::new(move |event| {
                    if let Some(index) = weak.upgrade() {
                        index.on_bus_event(event);
                    }
                }),
            );
            *index.bus_subscription.lock() = Some(sub);
        }
        index
    }

    pub fn default_path(app_data_dir: &Path) -> PathBuf {
        app_data_dir.join("media-index.json")
    }

    /// win-media §0.1.
    pub fn normalize_directory_path(path: &str) -> String {
        paths::normalize_directory_path(path)
    }

    pub fn is_media_file(path: &str) -> bool {
        paths::is_media_file(path)
    }

    pub fn is_audio_file(path: &str) -> bool {
        paths::is_audio_file(path)
    }

    pub fn file_path(&self) -> &Path {
        &self.file_path
    }

    /// This instance's broker sender id (32 lowercase hex).
    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    /// A coalescing change receiver (capacity 1: a burst of changes wakes the consumer once). Disconnected when the
    /// index is dropped.
    pub fn changes(&self) -> Receiver<()> {
        let (tx, rx) = crossbeam_channel::bounded(1);
        self.channels.lock().push(tx);
        rx
    }

    /// Every indexed folder, sorted OrdinalIgnoreCase.
    pub fn indexed_directories(&self) -> Vec<String> {
        let mut dirs: Vec<String> = self.state.lock().directories.keys().map(|k| k.as_str().to_owned()).collect();
        dirs.sort_by(|a, b| cmp_ordinal_ignore_case(a, b));
        dirs
    }

    /// The cached media files of a folder; `[]` if not indexed or blank.
    pub fn cached_directory(&self, dir: &str) -> Vec<IndexedMediaFile> {
        let normalized = paths::normalize_directory_path(dir);
        if normalized.is_empty() {
            return Vec::new();
        }
        self.state.lock().directories.get(&CiKey::new(&normalized)).map(|e| e.media_files.clone()).unwrap_or_default()
    }

    /// The cached subfolders of a folder; `[]` if not indexed or blank.
    pub fn cached_subdirectories(&self, dir: &str) -> Vec<String> {
        let normalized = paths::normalize_directory_path(dir);
        if normalized.is_empty() {
            return Vec::new();
        }
        self.state.lock().directories.get(&CiKey::new(&normalized)).map(|e| e.subdirectories.clone()).unwrap_or_default()
    }

    /// `IsDirectoryIndexed`.
    pub fn is_directory_indexed(&self, dir: &str) -> bool {
        let normalized = paths::normalize_directory_path(dir);
        !normalized.is_empty() && self.state.lock().directories.contains_key(&CiKey::new(&normalized))
    }

    /// `ScanDirectoryAsync`: lists the folder, updates the index and writes it when the listing changed.
    pub fn scan_directory(&self, dir: &str, cancel: &CancellationToken) -> ScanResult {
        self.scan_entry(dir, true, true, cancel)
    }

    /// `ScanDirectoryTransientAsync`: lists the folder without touching the index (memory or disk). Used for the
    /// folder of a video that was merely opened, which must not become a record.
    pub fn scan_transient(&self, dir: &str, cancel: &CancellationToken) -> ScanResult {
        self.scan_entry(dir, false, false, cancel)
    }

    /// `RefreshDirectoryInMemoryAsync`: rescans and updates the in-memory index (badges, search) without writing the
    /// file.
    pub fn refresh_in_memory(&self, dir: &str, cancel: &CancellationToken) -> ScanResult {
        self.scan_entry(dir, false, true, cancel)
    }

    fn scan_entry(&self, dir: &str, persist: bool, update_index: bool, cancel: &CancellationToken) -> ScanResult {
        if cancel.is_cancelled() {
            return ScanResult::Cancelled;
        }
        match self.scan_core(dir, persist, update_index, cancel) {
            Err(Cancelled) => ScanResult::Cancelled,
            Ok((_, true)) => ScanResult::Missing,
            Ok((snapshot, false)) => ScanResult::Listed(snapshot),
        }
    }

    /// `CrawlSubtreeAsync(root, max_depth)`: breadth-first, at most `max_folders` folders (C#: 64), saved and
    /// announced once at the end when anything changed. Cancellation stops at the next folder and saves nothing
    /// (the scanned folders stay updated in memory). Returns the number of folders scanned.
    pub fn crawl_subtree(&self, root: &str, max_depth: u8, max_folders: usize, cancel: &CancellationToken, progress: &dyn Fn(usize)) -> usize {
        let normalized_root = paths::normalize_directory_path(root);
        if normalized_root.is_empty() || !Path::new(&normalized_root).is_dir() {
            return 0;
        }
        let depth_limit = max_depth.min(CRAWL_MAX_DEPTH);
        let max_folders = max_folders.max(1);
        let mut visited: std::collections::HashSet<CiKey> = std::collections::HashSet::new();
        visited.insert(CiKey::new(&normalized_root));
        let mut queue = std::collections::VecDeque::from([(normalized_root, 0u8)]);
        let mut changed_any = false;
        let mut scanned = 0usize;
        while !queue.is_empty() && visited.len() <= max_folders {
            if cancel.is_cancelled() {
                return scanned;
            }
            let Some((dir, depth)) = queue.pop_front() else { break };
            let Ok((snapshot, changed)) = self.scan_for_crawl(&dir, cancel) else { return scanned };
            scanned += 1;
            changed_any |= changed;
            progress(scanned);
            if depth < depth_limit {
                for sub in &snapshot.subdirectories {
                    if visited.len() >= max_folders {
                        break;
                    }
                    if visited.insert(CiKey::new(sub)) {
                        queue.push_back((sub.clone(), depth + 1));
                    }
                }
            }
        }
        if changed_any {
            self.save_to_disk();
            self.notify();
        }
        scanned
    }

    fn scan_for_crawl(&self, dir: &str, cancel: &CancellationToken) -> Result<(DirectorySnapshot, bool), Cancelled> {
        let before = self.cached_directory(dir);
        let before_subdirs = self.cached_subdirectories(dir);
        let indexed = self.is_directory_indexed(dir);
        let (snapshot, _) = self.scan_core(dir, false, true, cancel)?;
        let changed = indexed != self.is_directory_indexed(dir)
            || before != snapshot.media_files
            || !subdirs_equal(&before_subdirs, &snapshot.subdirectories);
        Ok((snapshot, changed))
    }

    /// `Search(query, maxResults)` (win-media §1.6): files on unplugged drives stay indexed but are hidden.
    pub fn search_files(&self, query: &str, max_results: usize) -> Vec<IndexedMediaFile> {
        if max_results == 0 {
            return Vec::new();
        }
        let q = query.trim();
        let candidates: Vec<IndexedMediaFile> = self.state.lock().files_by_path.values().cloned().collect();
        let mut root_cache: HashMap<String, bool> = HashMap::new();
        let available = candidates.into_iter().filter(|f| is_drive_root_ready(&f.full_path, Some(&mut root_cache)));
        if q.is_empty() {
            let mut files: Vec<IndexedMediaFile> = available.collect();
            files.sort_by(|a, b| {
                b.last_write_time_utc.cmp(&a.last_write_time_utc).then_with(|| cmp_ordinal_ignore_case(&a.file_name, &b.file_name))
            });
            files.truncate(max_results);
            return files;
        }
        let mut scored: Vec<(IndexedMediaFile, u8)> =
            available.map(|f| { let s = match_score(&f, q); (f, s) }).filter(|(_, s)| *s > 0).collect();
        scored.sort_by(|(a, sa), (b, sb)| {
            sb.cmp(sa)
                .then_with(|| b.last_write_time_utc.cmp(&a.last_write_time_utc))
                .then_with(|| cmp_ordinal_ignore_case(&a.file_name, &b.file_name))
        });
        scored.into_iter().take(max_results).map(|(f, _)| f).collect()
    }

    // ---- scanning --------------------------------------------------------------------------------------------

    /// `ScanDirectoryCore`: returns the snapshot and whether the folder is missing.
    fn scan_core(&self, dir: &str, persist: bool, update_index: bool, cancel: &CancellationToken) -> Result<(DirectorySnapshot, bool), Cancelled> {
        let normalized = paths::normalize_directory_path(dir);
        // No scan time is recorded: a persisted "folder X was scanned at T" is a record of when a video was opened.
        let now = 0i64;
        if normalized.is_empty() || !Path::new(&normalized).is_dir() {
            // Only drop the folder when its drive is connected (deleted); an unplugged drive keeps its cache.
            if update_index && !normalized.is_empty() && is_drive_root_ready(&normalized, None) && self.remove_directory(&normalized) {
                if persist {
                    self.save_to_disk();
                }
                self.notify();
            }
            return Ok((DirectorySnapshot::empty(normalized, now), true));
        }

        use std::os::windows::fs::MetadataExt;
        let skip = FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM;
        let base = Path::new(&normalized);

        let mut subdirs = Vec::new();
        if let Ok(entries) = std::fs::read_dir(base) {
            for entry in entries {
                if cancel.is_cancelled() {
                    return Err(Cancelled);
                }
                let Ok(entry) = entry else { break };
                let Ok(meta) = entry.metadata() else { continue };
                let attrs = meta.file_attributes();
                if attrs & skip != 0 || attrs & FILE_ATTRIBUTE_DIRECTORY == 0 {
                    continue;
                }
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.is_empty() || name.starts_with('.') {
                    continue;
                }
                subdirs.push(paths::normalize_directory_path(&base.join(&*name).to_string_lossy()));
            }
        }
        subdirs.sort_by(|a, b| cmp_ordinal_ignore_case(a, b));

        let mut media_files = Vec::new();
        if let Ok(entries) = std::fs::read_dir(base) {
            for entry in entries {
                if cancel.is_cancelled() {
                    return Err(Cancelled);
                }
                let Ok(entry) = entry else { break };
                let Ok(meta) = entry.metadata() else { continue };
                let attrs = meta.file_attributes();
                if attrs & skip != 0 || attrs & FILE_ATTRIBUTE_DIRECTORY != 0 {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().into_owned();
                if !paths::is_media_file(&name) {
                    continue;
                }
                let full_path = base.join(&name).to_string_lossy().into_owned();
                let sidecar = paths::find_sidecar_subtitle(&full_path);
                let last_write = meta.modified().map(dates::system_time_ticks).unwrap_or(0);
                media_files.push(IndexedMediaFile {
                    full_path,
                    file_name: name,
                    directory_path: normalized.clone(),
                    size_bytes: i64::try_from(meta.len()).unwrap_or(i64::MAX),
                    last_write_time_utc: last_write,
                    has_sidecar_subtitle: sidecar.is_some(),
                    sidecar_extension: sidecar.map(|(_, ext)| ext.to_owned()),
                    indexed_at_utc: now,
                });
            }
        }
        media_files.sort_by(|a, b| cmp_ordinal_ignore_case(&a.file_name, &b.file_name));

        let snapshot = DirectorySnapshot { directory_path: normalized, subdirectories: subdirs, media_files, scanned_at_utc: now };
        if update_index && self.update_directory(&snapshot) {
            if persist {
                self.save_to_disk();
            }
            self.notify();
        }
        Ok((snapshot, false))
    }

    fn remove_directory(&self, normalized: &str) -> bool {
        let mut st = self.state.lock();
        let Some(existing) = st.directories.remove(&CiKey::new(normalized)) else { return false };
        for f in &existing.media_files {
            st.files_by_path.remove(&CiKey::new(&f.full_path));
        }
        true
    }

    /// False when the folder was already indexed with exactly this listing.
    fn update_directory(&self, snapshot: &DirectorySnapshot) -> bool {
        let mut st = self.state.lock();
        let key = CiKey::new(&snapshot.directory_path);
        if let Some(existing) = st.directories.get(&key) {
            if existing.media_files == snapshot.media_files && subdirs_equal(&existing.subdirectories, &snapshot.subdirectories) {
                return false;
            }
            let old: Vec<String> = existing.media_files.iter().map(|f| f.full_path.clone()).collect();
            for p in old {
                st.files_by_path.remove(&CiKey::new(&p));
            }
        }
        let entry = DirEntry {
            directory_path: snapshot.directory_path.clone(),
            subdirectories: snapshot.subdirectories.clone(),
            media_files: snapshot.media_files.clone(),
        };
        // An existing key keeps its slot and its original spelling, as a C# dictionary assignment does.
        st.directories.insert(key, entry);
        for f in &snapshot.media_files {
            st.files_by_path.insert(CiKey::new(&f.full_path), f.clone());
        }
        true
    }

    // ---- persistence -----------------------------------------------------------------------------------------

    fn load_from_disk(&self) -> LoadResult {
        let Some(json) = fsio::read_all_text_shared(&self.file_path, fsio::DEFAULT_ATTEMPTS) else { return LoadResult::None };
        match MediaIndexDocument::read_dotnet(&json) {
            DotNetRead::Invalid | DotNetRead::NoDirectories => LoadResult::None,
            DotNetRead::Legacy(_) => {
                *self.state.lock() = State::default();
                LoadResult::Legacy
            }
            DotNetRead::Loaded(doc) => {
                let mut st = State::default();
                for dir in doc.directories {
                    if dir.directory_path.trim().is_empty() {
                        continue;
                    }
                    let normalized = paths::normalize_directory_path(&dir.directory_path);
                    let mut files: Vec<IndexedMediaFile> =
                        dir.media_files.into_iter().filter(|f| !f.full_path.trim().is_empty()).collect();
                    files.sort_by(|a, b| cmp_ordinal_ignore_case(&a.file_name, &b.file_name));
                    let mut subdirs: Vec<String> = dir
                        .subdirectories
                        .iter()
                        .filter(|s| !s.trim().is_empty())
                        .map(|s| paths::normalize_directory_path(s))
                        .collect();
                    subdirs.sort_by(|a, b| cmp_ordinal_ignore_case(a, b));
                    for f in &files {
                        st.files_by_path.insert(CiKey::new(&f.full_path), f.clone());
                    }
                    let key = CiKey::new(&normalized);
                    st.directories.insert(key, DirEntry { directory_path: normalized, subdirectories: subdirs, media_files: files });
                }
                *self.state.lock() = st;
                LoadResult::Loaded
            }
        }
    }

    fn save_to_disk(&self) {
        let wrote = {
            let _write = self.file_write.lock();
            let doc = {
                let st = self.state.lock();
                let mut directories: Vec<DirectorySnapshot> = st.directories.values().map(DirEntry::snapshot).collect();
                directories.sort_by(|a, b| cmp_ordinal_ignore_case(&a.directory_path, &b.directory_path));
                MediaIndexDocument { version: MediaIndexDocument::WINDOWS_VERSION, directories }
            };
            fsio::write_all_text_atomic(&self.file_path, &doc.to_json(DateStyle::DotNet), fsio::DEFAULT_ATTEMPTS)
        };
        if wrote && let Some(bus) = &self.bus {
            bus.publish(
                Topic::MediaIndex,
                StateSyncEvent::new(Topic::MediaIndex, EventKind::MediaIndexed, &self.instance_id, &self.file_path_text),
            );
        }
    }

    fn on_bus_event(&self, event: &StateSyncEvent) {
        if event.sender_instance_id == self.instance_id || !eq_ordinal_ignore_case(&event.scope_path, &self.file_path_text) {
            return;
        }
        self.load_from_disk();
        self.notify();
    }

    fn notify(&self) {
        self.changed.notify(&());
        let mut channels = self.channels.lock();
        channels.retain(|tx| !matches!(tx.try_send(()), Err(TrySendError::Disconnected(_))));
    }
}

#[derive(Debug, PartialEq, Eq)]
enum LoadResult {
    None,
    Loaded,
    Legacy,
}

fn match_score(f: &IndexedMediaFile, q: &str) -> u8 {
    if starts_with_ordinal_ignore_case(&f.file_name, q) {
        3
    } else if contains_ordinal_ignore_case(&f.file_name, q) {
        2
    } else if contains_ordinal_ignore_case(&f.directory_path, q) {
        1
    } else {
        0
    }
}

/// `IsDriveRootReady`: a blank root is ready; a root starting with `\` (UNC, device) must exist; anything else goes
/// through `new DriveInfo(root).IsReady`, which accepts only a drive letter (`C:` / `C:\`, probed as `C:\`) and
/// throws for the rest (`\`, `//server/share`), which counts as not ready. Probed quietly; the cache (per search)
/// probes each root once.
fn is_drive_root_ready(path: &str, cache: Option<&mut HashMap<String, bool>>) -> bool {
    let root = paths::path_root(path);
    if root.trim().is_empty() {
        return true;
    }
    let probe = || {
        if root.starts_with("\\\\") {
            fsio::root_exists_quietly(root)
        } else if root.as_bytes().first().is_some_and(u8::is_ascii_alphabetic) && root.as_bytes().get(1) == Some(&b':') {
            fsio::root_exists_quietly(&format!("{}\\", &root[..2]))
        } else {
            false
        }
    };
    let Some(cache) = cache else { return probe() };
    let key = paths::ci(root);
    if let Some(v) = cache.get(&key) {
        return *v;
    }
    let ready = probe();
    cache.entry(key).or_insert(ready);
    ready
}

impl Drop for WinMediaIndex {
    fn drop(&mut self) {
        self.bus_subscription.lock().take();
    }
}

impl MediaIndex for WinMediaIndex {
    fn scan(&self, dir: &Path, cancel: &CancellationToken) -> ScanResult {
        self.scan_directory(&dir.to_string_lossy(), cancel)
    }

    fn crawl(&self, root: &Path, max_depth: u8, max_folders: usize, cancel: &CancellationToken, progress: &dyn Fn(usize)) -> usize {
        self.crawl_subtree(&root.to_string_lossy(), max_depth, max_folders, cancel, progress)
    }

    fn cached_snapshot(&self, dir: &Path) -> Option<DirectorySnapshot> {
        let normalized = paths::normalize_directory_path(&dir.to_string_lossy());
        if normalized.is_empty() {
            return None;
        }
        self.state.lock().directories.get(&CiKey::new(&normalized)).map(DirEntry::snapshot)
    }

    fn total_indexed_files(&self) -> usize {
        self.state.lock().files_by_path.len()
    }

    fn search(&self, query: &str, limit: usize) -> Vec<IndexedMediaFile> {
        self.search_files(query, limit)
    }

    fn all_files(&self, limit: usize) -> Vec<IndexedMediaFile> {
        self.search_files("", limit)
    }

    fn subscribe_changes(&self, handler: Box<dyn Fn() + Send + Sync>) -> Subscription {
        self.changed.subscribe(move |_| handler())
    }

    fn invalidate_sidecar(&self, video_path: &Path) {
        let path = video_path.to_string_lossy();
        let Some(dir) = paths::directory_name(&path) else { return };
        if self.is_directory_indexed(dir) {
            let _ = self.refresh_in_memory(dir, &CancellationToken::new());
        }
    }

    fn scan_transient(&self, dir: &Path, cancel: &CancellationToken) -> ScanResult {
        WinMediaIndex::scan_transient(self, &dir.to_string_lossy(), cancel)
    }

    fn refresh_in_memory(&self, dir: &Path, cancel: &CancellationToken) -> ScanResult {
        WinMediaIndex::refresh_in_memory(self, &dir.to_string_lossy(), cancel)
    }

    fn is_directory_indexed(&self, dir: &Path) -> bool {
        WinMediaIndex::is_directory_indexed(self, &dir.to_string_lossy())
    }
}
