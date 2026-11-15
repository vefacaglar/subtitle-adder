//! File explorer sidebar (win-viewmodels §21, mac-viewmodels §11; UNIFIED.md §3.10).
//! Rule 6: `explorerLastFolder` removed at startup and never written; logged out the search covers the current
//! folder and the folders below it (listed live, never written to the index), with no index-wide results. The
//! constructor touches no file system; listing and scanning run on workers through the media index.

use std::cell::RefCell;
use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use sa_core::cancel::CancellationToken;
use sa_core::l;
use sa_core::platform::{QuickLocation, QuickLocationKind};
use sa_core::settings::keys;
use sa_core::subscription::Subscription;
use sa_media::{DirectorySnapshot, IndexedMediaFile, ScanResult};
use unicode_normalization::UnicodeNormalization;

use crate::policy::{ByteSizeFormat, ExplorerCompare};
use crate::services::AppServices;
use crate::uiref::UiKey;

/// Crawl depth below a navigated folder.
pub const CRAWL_DEPTH: u8 = 2;
/// Crawl folder budget.
pub const CRAWL_FOLDERS: usize = 64;
/// Index-wide search hits.
pub const SEARCH_LIMIT: usize = 100;
/// Search typing debounce (macOS).
pub const SEARCH_DELAY: Duration = Duration::from_millis(200);
/// Search below the current folder: how deep the folders are listed.
pub const SUBTREE_DEPTH: u8 = 16;
/// Search below the current folder: folders listed at most (breadth-first, so the nearest ones come first).
pub const SUBTREE_FOLDERS: usize = 5000;
/// Search below the current folder: media files kept at most.
pub const SUBTREE_FILES: usize = 20_000;
/// Search below the current folder: matching rows shown at most.
pub const SUBTREE_ROWS: usize = 500;
/// Search below the current folder: how often the files found so far reach the list.
const SUBTREE_BATCH: Duration = Duration::from_millis(150);

/// One row.
#[derive(Debug, Clone, PartialEq)]
pub struct ExplorerRow {
    pub path: PathBuf,
    pub name: String,
    pub is_folder: bool,
    pub size_text: String,
    pub sidecar_badge: Option<String>,
    /// A search result outside the current folder (below it, or from the index).
    pub is_index_result: bool,
    /// The video open in the editor.
    pub is_active: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExplorerChange {
    Folder,
    Rows,
    Status,
    QuickLocations,
    Visibility,
}

/// Folder state (macOS `FolderAvailability`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FolderAvailability {
    #[default]
    Available,
    NotFound,
    Unreadable,
}

/// Case/diacritic/width folding with `ı` → `i` (macOS search; equal to the index's fold).
pub fn fold(t: &str) -> String {
    let decomposed: String = t.nfkd().filter(|c| !is_combining_mark(*c)).collect();
    decomposed.to_lowercase().replace('\u{131}', "i")
}

fn is_combining_mark(c: char) -> bool {
    matches!(c as u32, 0x300..=0x36F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF | 0xFE20..=0xFE2F)
}

/// Windows OrdinalIgnoreCase `contains` (.NET simple invariant uppercase per code point).
fn contains_ignore_case(hay: &str, needle: &str) -> bool {
    sa_core::text::upper_invariant(hay).contains(&sa_core::text::upper_invariant(needle))
}

/// The text a search compares under `compare`: two texts match when one's key contains the other's.
fn search_key(compare: ExplorerCompare, text: &str) -> String {
    match compare {
        ExplorerCompare::OrdinalIgnoreCase => sa_core::text::upper_invariant(text),
        ExplorerCompare::Fold => fold(text),
    }
}

/// Row size texts: Windows rows show the index's `FormattedSize` (`MediaLibraryIndex.FormatFileSize`: B/KB/MB/GB,
/// invariant culture); macOS rows show `ByteCountFormatter` `.file` (decimal units).
pub fn size_text(bytes: i64, format: ByteSizeFormat) -> String {
    let tr = sa_core::l10n::LanguageState::shared().language() == sa_core::l10n::UiLanguage::Tr;
    let sep = |s: String| if tr { s.replace('.', ",") } else { s };
    let trim = |s: String| {
        if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_owned() } else { s }
    };
    match format {
        ByteSizeFormat::WindowsBinary => sa_media::format_file_size(bytes),
        ByteSizeFormat::AppleDecimal => {
            let b = bytes.max(0) as f64;
            if b < 1000.0 {
                format!("{} bytes", bytes.max(0))
            } else if b < 1e6 {
                format!("{} KB", (b / 1e3).round())
            } else if b < 1e9 {
                format!("{} MB", sep(trim(format!("{:.1}", b / 1e6))))
            } else {
                format!("{} GB", sep(trim(format!("{:.2}", b / 1e9))))
            }
        }
    }
}

/// The launch folder: the Videos/Movies location, else home (C# `GetInitialFolder`, Swift `defaultFolder`).
fn initial_folder(quick: &[QuickLocation]) -> Option<PathBuf> {
    quick
        .iter()
        .find(|q| q.kind == QuickLocationKind::Videos && q.path.is_dir())
        .map(|q| q.path.clone())
        .or_else(|| quick.iter().find(|q| q.kind == QuickLocationKind::Home && q.path.is_dir()).map(|q| q.path.clone()))
        .or_else(|| std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from))
}

fn parent_of(p: &Path) -> Option<PathBuf> {
    p.parent().filter(|x| !x.as_os_str().is_empty()).map(Path::to_path_buf)
}

fn name_of(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.to_string_lossy().into_owned())
}

/// How a folder is listed (Windows `NavigateCoreAsync` `transient` / `persist`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScanMode {
    /// Lists, updates the index and writes it (explicit browsing).
    Persist,
    /// Lists without touching the index (the folder of a video that was merely opened, Windows).
    Transient,
    /// Updates the in-memory index only, never the file (Windows refresh after opening a video).
    InMemory,
}

struct State {
    current: Option<PathBuf>,
    /// Windows: the current folder is shown only because a video from it was opened (never written to the index).
    transient: bool,
    /// Back stack with each folder's transient flag.
    back: Vec<(PathBuf, bool)>,
    folders: Vec<String>,
    files: Vec<IndexedMediaFile>,
    rows: Vec<ExplorerRow>,
    query: String,
    logged_in: bool,
    index_hits: Vec<IndexedMediaFile>,
    /// Media files below the current folder, each with the [`search_key`] of its path from that folder
    /// (`Series\Season 1\Ep1.mp4`), listed while a search is active: memory only, never written to the index, dropped
    /// when the search or the folder ends.
    subtree: Vec<(IndexedMediaFile, String)>,
    /// The folder `subtree` was listed for.
    subtree_root: Option<PathBuf>,
    subtree_walking: bool,
    subtree_gen: u64,
    subtree_cancel: Option<CancellationToken>,
    quick: Vec<QuickLocation>,
    visible: bool,
    scanning: bool,
    availability: FolderAvailability,
    total: usize,
    follows_video: bool,
    active_video: Option<String>,
    scan_gen: u64,
    filter_gen: u64,
    scan_cancel: Option<CancellationToken>,
    started: bool,
    /// The profile whose session the browsed folders belong to (a logout or switch forgets them).
    session_user: Option<String>,
}

struct Inner {
    svc: AppServices,
    key: std::cell::Cell<Option<UiKey<Inner>>>,
    st: RefCell<State>,
    handlers: RefCell<Vec<Rc<dyn Fn(ExplorerChange)>>>,
    subs: RefCell<Vec<Subscription>>,
}

impl Inner {
    fn key(&self) -> UiKey<Inner> {
        self.key.get().expect("registered")
    }

    fn notify(&self, changes: &[ExplorerChange]) {
        let hs: Vec<Rc<dyn Fn(ExplorerChange)>> = self.handlers.borrow().clone();
        for c in changes {
            for h in &hs {
                h(*c);
            }
        }
    }

    fn same(&self, a: &Path, b: &Path) -> bool {
        self.svc.policy.same_path(a, b)
    }

    fn matches(&self, hay: &str, needle: &str) -> bool {
        match self.svc.policy.explorer_compare {
            ExplorerCompare::OrdinalIgnoreCase => contains_ignore_case(hay, needle),
            ExplorerCompare::Fold => fold(hay).contains(&fold(needle)),
        }
    }

    fn rebuild_rows(&self) {
        let rows = {
            let st = self.st.borrow();
            let q = st.query.trim().to_owned();
            let fmt = self.svc.policy.byte_size_format;
            let mut rows: Vec<ExplorerRow> = Vec::new();
            for f in &st.folders {
                let p = PathBuf::from(f);
                let name = name_of(&p);
                if q.is_empty() || self.matches(&name, &q) {
                    rows.push(ExplorerRow { name, path: p, is_folder: true, size_text: String::new(), sidecar_badge: None, is_index_result: false, is_active: false });
                }
            }
            let mut seen: HashSet<String> = HashSet::new();
            let push_file = |rows: &mut Vec<ExplorerRow>, seen: &mut HashSet<String>, f: &IndexedMediaFile, index: bool| {
                let k = self.svc.policy.path_key(Path::new(&f.full_path));
                if !seen.insert(k.clone()) {
                    return;
                }
                rows.push(ExplorerRow {
                    path: PathBuf::from(&f.full_path),
                    name: f.file_name.clone(),
                    is_folder: false,
                    size_text: size_text(f.size_bytes, fmt),
                    sidecar_badge: f.sidecar_badge(),
                    is_index_result: index,
                    is_active: st.active_video.as_deref() == Some(k.as_str()),
                });
            };
            for f in &st.files {
                let hit = q.is_empty()
                    || self.matches(&f.file_name, &q)
                    || (self.svc.policy.explorer_compare == ExplorerCompare::OrdinalIgnoreCase && self.matches(&f.directory_path, &q));
                if hit {
                    push_file(&mut rows, &mut seen, f, false);
                }
            }
            if !q.is_empty() {
                // Below the current folder: by the path from it (the file's name, or a folder's name finds its files).
                let needle = search_key(self.svc.policy.explorer_compare, &q);
                let mut shown = 0;
                for (f, key) in &st.subtree {
                    if shown == SUBTREE_ROWS {
                        break;
                    }
                    if key.contains(&needle) {
                        let before = rows.len();
                        push_file(&mut rows, &mut seen, f, true);
                        shown += rows.len() - before;
                    }
                }
            }
            if !q.is_empty() && st.logged_in {
                for f in &st.index_hits {
                    let outside = st.current.as_ref().is_none_or(|c| !self.same(Path::new(&f.directory_path), c));
                    push_file(&mut rows, &mut seen, f, outside);
                }
            }
            rows
        };
        let changed = {
            let mut st = self.st.borrow_mut();
            if st.rows != rows {
                st.rows = rows;
                true
            } else {
                false
            }
        };
        if changed {
            self.notify(&[ExplorerChange::Rows, ExplorerChange::Status]);
        }
    }

    fn set_listing(&self, snap: Option<DirectorySnapshot>) {
        {
            let mut st = self.st.borrow_mut();
            match snap {
                Some(s) => {
                    st.folders = s.subdirectories;
                    st.files = s.media_files;
                }
                None => {
                    st.folders.clear();
                    st.files.clear();
                }
            }
            st.total = self.svc.media_index.total_indexed_files();
        }
        self.rebuild_rows();
    }

    fn run_scan(self: &Rc<Self>, folder: PathBuf, show_cached: bool, crawl: bool, mode: ScanMode) {
        // A folder that is not written to the index is never crawled either.
        let crawl = crawl && mode == ScanMode::Persist;
        let (generation_id, token) = {
            let mut st = self.st.borrow_mut();
            if let Some(t) = st.scan_cancel.take() {
                t.cancel();
            }
            st.scan_gen += 1;
            st.scanning = true;
            st.availability = FolderAvailability::Available;
            let t = CancellationToken::new();
            st.scan_cancel = Some(t.clone());
            (st.scan_gen, t)
        };
        if show_cached {
            let cached = self.svc.media_index.cached_snapshot(&folder);
            self.set_listing(cached);
        }
        self.notify(&[ExplorerChange::Status]);
        let index = self.svc.media_index.clone();
        let key = self.key();
        let ui = self.svc.ui.clone();
        let _ = std::thread::Builder::new().name("sa-explorer-scan".into()).spawn(move || {
            let result = match mode {
                ScanMode::Persist => index.scan(&folder, &token),
                ScanMode::Transient => index.scan_transient(&folder, &token),
                ScanMode::InMemory => index.refresh_in_memory(&folder, &token),
            };
            if token.is_cancelled() {
                return;
            }
            let listed = matches!(result, ScanResult::Listed(_));
            let r2 = result.clone();
            key.post(&ui, move |inner| {
                if inner.st.borrow().scan_gen != generation_id {
                    return;
                }
                let availability = match &r2 {
                    ScanResult::Listed(_) => FolderAvailability::Available,
                    ScanResult::Missing => FolderAvailability::NotFound,
                    ScanResult::Unreadable => FolderAvailability::Unreadable,
                    ScanResult::Cancelled => return,
                };
                inner.st.borrow_mut().availability = availability;
                match r2 {
                    ScanResult::Listed(s) => inner.set_listing(Some(s)),
                    ScanResult::Unreadable => {
                        // Keep what the index remembers.
                        let f = inner.st.borrow().current.clone();
                        let cached = f.and_then(|f| inner.svc.media_index.cached_snapshot(&f));
                        inner.set_listing(cached);
                    }
                    _ => inner.set_listing(None),
                }
                if !crawl {
                    inner.st.borrow_mut().scanning = false;
                }
                inner.notify(&[ExplorerChange::Status, ExplorerChange::Folder]);
            });
            if crawl && listed {
                index.crawl(&folder, CRAWL_DEPTH, CRAWL_FOLDERS, &token, &|_| {});
                key.post(&ui, move |inner| {
                    if inner.st.borrow().scan_gen != generation_id {
                        return;
                    }
                    inner.st.borrow_mut().scanning = false;
                    inner.st.borrow_mut().total = inner.svc.media_index.total_indexed_files();
                    inner.refilter(false);
                    inner.notify(&[ExplorerChange::Status]);
                });
            } else if crawl {
                key.post(&ui, move |inner| {
                    if inner.st.borrow().scan_gen == generation_id {
                        inner.st.borrow_mut().scanning = false;
                        inner.notify(&[ExplorerChange::Status]);
                    }
                });
            }
        });
    }

    fn scan_other(self: &Rc<Self>, folder: PathBuf) {
        let index = self.svc.media_index.clone();
        let key = self.key();
        let ui = self.svc.ui.clone();
        let _ = std::thread::Builder::new().name("sa-explorer-scan-other".into()).spawn(move || {
            let _ = index.scan(&folder, &CancellationToken::new());
            key.post(&ui, |inner| {
                inner.st.borrow_mut().total = inner.svc.media_index.total_indexed_files();
                inner.refilter(false);
                inner.notify(&[ExplorerChange::Status]);
            });
        });
    }

    fn go(self: &Rc<Self>, folder: PathBuf, push: bool, crawl: bool) {
        self.go_with(folder, push, crawl, ScanMode::Persist);
    }

    fn go_with(self: &Rc<Self>, folder: PathBuf, push: bool, crawl: bool, mode: ScanMode) {
        {
            let mut st = self.st.borrow_mut();
            if push
                && let Some(cur) = st.current.clone()
                && !self.same(&cur, &folder)
            {
                let t = st.transient;
                st.back.push((cur, t));
            }
            st.follows_video = false;
            st.query.clear();
            st.index_hits.clear();
            st.current = Some(folder.clone());
            st.transient = mode == ScanMode::Transient;
        }
        self.drop_subtree();
        // Rule 6: never persisted (explorerLastFolder).
        self.notify(&[ExplorerChange::Folder]);
        self.run_scan(folder, true, crawl, mode);
    }

    /// Windows `IsDirectoryIndexed` + `RefreshDirectoryInMemoryAsync` for a folder that is not the current one.
    fn refresh_other_in_memory(self: &Rc<Self>, folder: PathBuf) {
        let index = self.svc.media_index.clone();
        let key = self.key();
        let ui = self.svc.ui.clone();
        let _ = std::thread::Builder::new().name("sa-explorer-refresh".into()).spawn(move || {
            if !index.is_directory_indexed(&folder) {
                return;
            }
            let _ = index.refresh_in_memory(&folder, &CancellationToken::new());
            key.post(&ui, |inner| {
                inner.st.borrow_mut().total = inner.svc.media_index.total_indexed_files();
                inner.refilter(false);
                inner.notify(&[ExplorerChange::Status]);
            });
        });
    }

    /// The current folder's listing already shows `video` with the subtitle state it has now (checked on a worker:
    /// the sidecar lookup touches the file system); otherwise `rescan` runs on the UI thread.
    fn rescan_unless_current(self: &Rc<Self>, video: PathBuf, rescan: impl FnOnce(&Rc<Inner>) + Send + 'static) {
        let listed = {
            let st = self.st.borrow();
            let has_listing = !st.files.is_empty() || !st.folders.is_empty();
            let available = st.availability == FolderAvailability::Available;
            st.files
                .iter()
                .find(|f| has_listing && available && self.same(Path::new(&f.full_path), &video))
                .map(|f| (f.has_sidecar_subtitle, f.sidecar_extension.clone()))
        };
        let Some((had, ext)) = listed else {
            rescan(self);
            return;
        };
        let windows = self.svc.policy.explorer_compare == ExplorerCompare::OrdinalIgnoreCase;
        let key = self.key();
        let ui = self.svc.ui.clone();
        let _ = std::thread::Builder::new().name("sa-explorer-sidecar".into()).spawn(move || {
            let found = sa_core::subtitle_import::find_sidecar_subtitle(&video);
            let unchanged = if windows {
                had == found.is_some()
            } else {
                let offered = found.and_then(|p| p.extension().map(|e| e.to_string_lossy().to_lowercase()));
                let known = ext.map(|e| e.trim_start_matches('.').to_lowercase());
                known == offered
            };
            if !unchanged {
                key.post(&ui, move |inner| rescan(inner));
            }
        });
    }

    /// Forgets the files listed below the current folder and stops their listing.
    fn drop_subtree(&self) {
        let was_walking = {
            let mut st = self.st.borrow_mut();
            if let Some(t) = st.subtree_cancel.take() {
                t.cancel();
            }
            st.subtree_gen += 1;
            st.subtree = Vec::new();
            st.subtree_root = None;
            std::mem::replace(&mut st.subtree_walking, false)
        };
        if was_walking {
            self.notify(&[ExplorerChange::Status]);
        }
    }

    /// While a search is active, lists the folders below the current one once (breadth-first, on a worker, bounded by
    /// [`SUBTREE_DEPTH`], [`SUBTREE_FOLDERS`] and [`SUBTREE_FILES`]) so the search also finds files in subfolders.
    /// The folders are listed without touching the index ([`sa_media::MediaIndex::scan_transient`]): the listing
    /// lives in this model only, logged in or not, and typing further filters it without listing again.
    fn ensure_subtree(self: &Rc<Self>) {
        let (root, generation_id, token) = {
            let mut st = self.st.borrow_mut();
            if st.query.trim().is_empty() {
                return;
            }
            let Some(root) = st.current.clone() else { return };
            if st.subtree_root.as_ref().is_some_and(|r| self.same(r, &root)) {
                return;
            }
            if let Some(t) = st.subtree_cancel.take() {
                t.cancel();
            }
            st.subtree_gen += 1;
            st.subtree = Vec::new();
            st.subtree_root = Some(root.clone());
            st.subtree_walking = true;
            let t = CancellationToken::new();
            st.subtree_cancel = Some(t.clone());
            (root, st.subtree_gen, t)
        };
        self.notify(&[ExplorerChange::Status]);
        let index = self.svc.media_index.clone();
        let key = self.key();
        let ui = self.svc.ui.clone();
        let compare = self.svc.policy.explorer_compare;
        let _ = std::thread::Builder::new().name("sa-explorer-subtree".into()).spawn(move || {
            let deliver = |batch: Vec<(IndexedMediaFile, String)>, done: bool| {
                key.post(&ui, move |inner| {
                    {
                        let mut st = inner.st.borrow_mut();
                        if st.subtree_gen != generation_id {
                            return;
                        }
                        st.subtree.extend(batch);
                        if done {
                            st.subtree_walking = false;
                            st.subtree_cancel = None;
                        }
                    }
                    inner.rebuild_rows();
                    if done {
                        inner.notify(&[ExplorerChange::Status]);
                    }
                });
            };
            // (folder, the folders from the root down to it, depth)
            let mut queue: VecDeque<(PathBuf, String, u8)> = VecDeque::from([(root, String::new(), 0)]);
            // Folders by their real location: a link or junction back into the tree is listed once.
            let mut visited: HashSet<PathBuf> = HashSet::new();
            let (mut folders, mut found) = (0usize, 0usize);
            let mut batch: Vec<(IndexedMediaFile, String)> = Vec::new();
            let mut delivered = Instant::now();
            while let Some((dir, below, depth)) = queue.pop_front() {
                if token.is_cancelled() {
                    return;
                }
                if folders == SUBTREE_FOLDERS || found == SUBTREE_FILES {
                    break;
                }
                if !visited.insert(std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone())) {
                    continue;
                }
                folders += 1;
                let ScanResult::Listed(snapshot) = index.scan_transient(&dir, &token) else { continue };
                // The root's own files are the current listing.
                if depth > 0 {
                    for f in snapshot.media_files.into_iter().take(SUBTREE_FILES - found) {
                        found += 1;
                        let key = search_key(compare, &format!("{below}{}{}", std::path::MAIN_SEPARATOR, f.file_name));
                        batch.push((f, key));
                    }
                }
                if depth < SUBTREE_DEPTH {
                    for sub in snapshot.subdirectories {
                        let sub = PathBuf::from(sub);
                        let name = name_of(&sub);
                        let below = if below.is_empty() { name } else { format!("{below}{}{name}", std::path::MAIN_SEPARATOR) };
                        queue.push_back((sub, below, depth + 1));
                    }
                }
                if !batch.is_empty() && delivered.elapsed() >= SUBTREE_BATCH {
                    deliver(std::mem::take(&mut batch), false);
                    delivered = Instant::now();
                }
            }
            if !token.is_cancelled() {
                deliver(batch, true);
            }
        });
    }

    /// A subtitle was saved in `folder`, which is not the current one: while a search lists the folders below, the
    /// files it holds from that folder get their sidecar state again (one transient listing of that folder).
    fn relist_subtree_folder(self: &Rc<Self>, folder: PathBuf) {
        let generation_id = {
            let st = self.st.borrow();
            if st.subtree_root.is_none() {
                return;
            }
            st.subtree_gen
        };
        let index = self.svc.media_index.clone();
        let key = self.key();
        let ui = self.svc.ui.clone();
        let _ = std::thread::Builder::new().name("sa-explorer-subtree-folder".into()).spawn(move || {
            let ScanResult::Listed(snapshot) = index.scan_transient(&folder, &CancellationToken::new()) else { return };
            key.post(&ui, move |inner| {
                {
                    let mut st = inner.st.borrow_mut();
                    if st.subtree_gen != generation_id {
                        return;
                    }
                    for fresh in snapshot.media_files {
                        let at = st.subtree.iter().position(|(f, _)| inner.same(Path::new(&f.full_path), Path::new(&fresh.full_path)));
                        if let Some(at) = at {
                            st.subtree[at].0 = fresh;
                        }
                    }
                }
                inner.rebuild_rows();
            });
        });
    }

    /// Re-applies the search (below the current folder always, index-wide only when logged in).
    fn refilter(self: &Rc<Self>, debounce: bool) {
        let (q, logged_in, generation_id) = {
            let mut st = self.st.borrow_mut();
            st.filter_gen += 1;
            (st.query.trim().to_owned(), st.logged_in, st.filter_gen)
        };
        if q.is_empty() {
            self.drop_subtree();
        } else {
            self.ensure_subtree();
        }
        if q.is_empty() || !logged_in {
            self.st.borrow_mut().index_hits.clear();
            self.rebuild_rows();
            return;
        }
        self.rebuild_rows();
        let index = self.svc.media_index.clone();
        let key = self.key();
        let ui = self.svc.ui.clone();
        let _ = std::thread::Builder::new().name("sa-explorer-search".into()).spawn(move || {
            if debounce {
                std::thread::sleep(SEARCH_DELAY);
            }
            // Bail early when superseded meanwhile.
            let still = key.invoke(&ui, move |inner| inner.st.borrow().filter_gen == generation_id).unwrap_or(false);
            if !still {
                return;
            }
            let hits = index.search(&q, SEARCH_LIMIT);
            key.post(&ui, move |inner| {
                let ok = {
                    let st = inner.st.borrow();
                    st.filter_gen == generation_id && st.logged_in
                };
                if ok {
                    inner.st.borrow_mut().index_hits = hits;
                    inner.rebuild_rows();
                }
            });
        });
    }
}

/// The sidebar model (no file-system access in the constructor).
pub struct FileExplorerModel {
    inner: Rc<Inner>,
}

impl Drop for FileExplorerModel {
    fn drop(&mut self) {
        if let Some(k) = self.inner.key.get() {
            k.unregister();
        }
        // A search still listing the folders below stops at the next folder.
        if let Some(t) = self.inner.st.borrow_mut().subtree_cancel.take() {
            t.cancel();
        }
    }
}

impl FileExplorerModel {
    pub fn new(services: AppServices) -> Self {
        // Rule 6 (both platforms): a plaintext record of browsing is removed and never written again.
        services.settings.remove(keys::EXPLORER_LAST_FOLDER);
        let visible = services.settings.get_bool(keys::SHOW_FILE_EXPLORER).unwrap_or(sa_core::settings::defaults::SHOW_FILE_EXPLORER);
        let inner = Rc::new(Inner {
            svc: services,
            key: std::cell::Cell::new(None),
            st: RefCell::new(State {
                current: None,
                transient: false,
                back: Vec::new(),
                folders: Vec::new(),
                files: Vec::new(),
                rows: Vec::new(),
                query: String::new(),
                logged_in: false,
                index_hits: Vec::new(),
                subtree: Vec::new(),
                subtree_root: None,
                subtree_walking: false,
                subtree_gen: 0,
                subtree_cancel: None,
                quick: Vec::new(),
                visible,
                scanning: true,
                availability: FolderAvailability::Available,
                total: 0,
                follows_video: true,
                active_video: None,
                scan_gen: 0,
                filter_gen: 0,
                scan_cancel: None,
                started: false,
                session_user: None,
            }),
            handlers: RefCell::new(Vec::new()),
            subs: RefCell::new(Vec::new()),
        });
        let key = UiKey::register(&inner);
        inner.key.set(Some(key));
        // UI-safe (memory only): the session the browsed folders belong to, and macOS `tracksFolders`.
        if inner.svc.profiles.is_logged_in() {
            let mut st = inner.st.borrow_mut();
            st.logged_in = true;
            st.session_user = inner.svc.profiles.current_username();
        }
        FileExplorerModel { inner }
    }

    pub fn on_change(&self, handler: Box<dyn Fn(ExplorerChange)>) {
        self.inner.handlers.borrow_mut().push(Rc::from(handler));
    }

    /// Background start: quick locations and the initial folder (Videos/Movies, else home), then the first scan.
    pub fn start(&self) {
        if std::mem::replace(&mut self.inner.st.borrow_mut().started, true) {
            return;
        }
        let key = self.inner.key();
        let ui = self.inner.svc.ui.clone();
        {
            let ui2 = ui.clone();
            let sub = self.inner.svc.media_index.subscribe_changes(Box::new(move || {
                key.post(&ui2, |inner| {
                    inner.st.borrow_mut().total = inner.svc.media_index.total_indexed_files();
                    let searching = !inner.st.borrow().query.trim().is_empty();
                    if searching {
                        inner.refilter(false);
                    }
                    inner.notify(&[ExplorerChange::Status]);
                });
            }));
            self.inner.subs.borrow_mut().push(sub);
        }
        let shell = self.inner.svc.shell.clone();
        let _ = std::thread::Builder::new().name("sa-explorer-start".into()).spawn(move || {
            let quick = shell.quick_locations();
            let initial = initial_folder(&quick);
            key.post(&ui, move |inner| {
                inner.st.borrow_mut().quick = quick;
                inner.notify(&[ExplorerChange::QuickLocations]);
                let (navigated, current) = {
                    let st = inner.st.borrow();
                    (st.current.is_some(), st.current.clone())
                };
                if !navigated && let Some(f) = initial.clone() {
                    inner.st.borrow_mut().current = Some(f.clone());
                    inner.notify(&[ExplorerChange::Folder]);
                    inner.run_scan(f, true, false, ScanMode::Persist);
                    // A search typed before the launch folder was known.
                    inner.ensure_subtree();
                } else if let Some(c) = current {
                    let (scanned, transient) = {
                        let st = inner.st.borrow();
                        (st.scan_gen > 0, st.transient)
                    };
                    if !scanned {
                        inner.run_scan(c, true, false, if transient { ScanMode::Transient } else { ScanMode::Persist });
                    }
                }
            });
        });
    }

    /// `showFileExplorer` (default true).
    pub fn is_sidebar_visible(&self) -> bool {
        self.inner.st.borrow().visible
    }
    pub fn set_sidebar_visible(&self, v: bool) {
        let changed = std::mem::replace(&mut self.inner.st.borrow_mut().visible, v) != v;
        if changed {
            self.inner.svc.settings.set_bool(keys::SHOW_FILE_EXPLORER, v);
            self.inner.notify(&[ExplorerChange::Visibility]);
        }
    }
    pub fn toggle_sidebar(&self) {
        let v = self.is_sidebar_visible();
        self.set_sidebar_visible(!v);
    }
    pub fn quick_locations(&self) -> Vec<QuickLocation> {
        self.inner.st.borrow().quick.clone()
    }
    /// Re-reads quick locations (drives, volumes) in the background.
    pub fn refresh_quick_locations(&self) {
        let key = self.inner.key();
        let ui = self.inner.svc.ui.clone();
        let shell = self.inner.svc.shell.clone();
        let _ = std::thread::Builder::new().spawn(move || {
            let quick = shell.quick_locations();
            key.post(&ui, move |inner| {
                inner.st.borrow_mut().quick = quick;
                inner.notify(&[ExplorerChange::QuickLocations]);
            });
        });
    }
    pub fn current_folder(&self) -> Option<PathBuf> {
        self.inner.st.borrow().current.clone()
    }
    /// `Dosya Gezgini` when none; the quick location's title (macOS) or the last component.
    pub fn current_folder_name(&self) -> String {
        let st = self.inner.st.borrow();
        let Some(c) = st.current.as_ref() else { return l!("Dosya Gezgini") };
        if let Some(q) = st.quick.iter().find(|q| self.inner.same(&q.path, c)) {
            return q.title.clone();
        }
        name_of(c)
    }
    pub fn can_go_up(&self) -> bool {
        self.inner.st.borrow().current.as_deref().and_then(parent_of).is_some()
    }
    pub fn can_go_back(&self) -> bool {
        !self.inner.st.borrow().back.is_empty()
    }
    /// The current folder is being listed or crawled, or a search is still listing the folders below it.
    pub fn is_scanning(&self) -> bool {
        let st = self.inner.st.borrow();
        st.scanning || st.subtree_walking
    }
    pub fn availability(&self) -> FolderAvailability {
        self.inner.st.borrow().availability
    }
    pub fn total_indexed_files(&self) -> usize {
        self.inner.st.borrow().total
    }
    pub fn navigate(&self, folder: &Path) {
        self.inner.go(folder.to_path_buf(), true, true);
    }
    pub fn go_up(&self) {
        let parent = self.inner.st.borrow().current.as_deref().and_then(parent_of);
        if let Some(p) = parent {
            self.inner.go(p, true, false);
        }
    }
    pub fn go_back(&self) {
        let prev = self.inner.st.borrow_mut().back.pop();
        if let Some((p, transient)) = prev {
            // A folder shown only for an opened video stays out of the index when it is returned to (Windows).
            let mode = if transient { ScanMode::Transient } else { ScanMode::Persist };
            self.inner.go_with(p, false, false, mode);
        }
    }
    /// Re-scans the current folder (and quick locations); a transient folder stays transient.
    pub fn refresh(&self) {
        self.refresh_quick_locations();
        let (cur, transient) = {
            let st = self.inner.st.borrow();
            (st.current.clone(), st.transient)
        };
        if let Some(c) = cur {
            self.inner.run_scan(c, false, false, if transient { ScanMode::Transient } else { ScanMode::Persist });
        }
        // An active search lists the folders below again.
        self.inner.drop_subtree();
        self.inner.ensure_subtree();
    }
    pub fn rows(&self) -> Vec<ExplorerRow> {
        self.inner.st.borrow().rows.clone()
    }
    pub fn search_query(&self) -> String {
        self.inner.st.borrow().query.clone()
    }
    /// Filters the current folder and the folders below it; logged in, the index is searched too (rule 6: never
    /// logged out).
    pub fn set_search(&self, query: &str, logged_in: bool) {
        let changed = {
            let mut st = self.inner.st.borrow_mut();
            let changed = st.query != query || st.logged_in != logged_in;
            st.query = query.to_owned();
            st.logged_in = logged_in;
            if !logged_in {
                st.index_hits.clear();
            }
            changed
        };
        if changed {
            let debounce = self.inner.svc.policy.explorer_compare == ExplorerCompare::Fold;
            self.inner.refilter(debounce);
        }
    }
    /// The hub's session ended or began: drops index hits immediately when logged out. A logout or a switch to
    /// another profile also forgets the folders browsed in the previous session ([`Self::reset_for_session_change`];
    /// C# `App` / Swift `ContentView` `onChange(of: platform.username)`).
    pub fn set_logged_in(&self, logged_in: bool) {
        let user = if logged_in { self.inner.svc.profiles.current_username() } else { None };
        let previous = {
            let mut st = self.inner.st.borrow_mut();
            let same = match (&st.session_user, &user) {
                (Some(a), Some(b)) => a.to_lowercase() == b.to_lowercase(),
                (None, None) => true,
                _ => false,
            };
            if same { None } else { Some(std::mem::replace(&mut st.session_user, user)) }
        };
        let q = self.search_query();
        self.set_search(&q, logged_in);
        if let Some(Some(_)) = previous {
            self.reset_for_session_change();
        }
    }

    /// Forgets the folders of the previous profile session (back stack, followed folder, search) and starts again
    /// from the launch folder (Videos/Movies, else home). The persistent index is not touched.
    pub fn reset_for_session_change(&self) {
        {
            let mut st = self.inner.st.borrow_mut();
            if let Some(t) = st.scan_cancel.take() {
                t.cancel();
            }
            st.back.clear();
            st.query.clear();
            st.index_hits.clear();
            st.transient = false;
        }
        self.inner.drop_subtree();
        self.inner.rebuild_rows();
        self.inner.notify(&[ExplorerChange::Folder]);
        let key = self.inner.key();
        let ui = self.inner.svc.ui.clone();
        let shell = self.inner.svc.shell.clone();
        let _ = std::thread::Builder::new().name("sa-explorer-reset".into()).spawn(move || {
            let initial = initial_folder(&shell.quick_locations());
            key.post(&ui, move |inner| {
                if let Some(f) = initial {
                    inner.go_with(f, false, false, ScanMode::Persist);
                }
                inner.st.borrow_mut().follows_video = true;
            });
        });
    }
    /// `%lld video · SSD indeksinde %lld dosya` (W) / `%lld medya · indekste %lld dosya` (M).
    pub fn status_text(&self) -> String {
        let st = self.inner.st.borrow();
        let n = st.rows.iter().filter(|r| !r.is_folder).count() as i64;
        let total = st.total as i64;
        match self.inner.svc.policy.explorer_compare {
            ExplorerCompare::OrdinalIgnoreCase => l!("%lld video · SSD indeksinde %lld dosya", n, total),
            ExplorerCompare::Fold => l!("%lld medya · indekste %lld dosya", n, total),
        }
    }
    /// Brings the explorer in step with the video that was opened (or closed, `None`). The row is highlighted in
    /// every case.
    ///
    /// * Windows (`SyncToVideoPathAsync`): with no current folder the video's folder is shown **transiently** (never
    ///   written to the index, not crawled); its folder being current, the listing is refreshed in memory unless it
    ///   already shows the video with its subtitle state; another folder is refreshed in memory only when it is
    ///   already indexed. Opening a video never writes the index file.
    /// * macOS (`sync(toVideo:tracksFolders:)`, `tracksFolders` = logged in): logged out only the folder being
    ///   browsed anyway is kept current; logged in, the first video moves a fresh explorer to its folder and another
    ///   folder is scanned into the index.
    pub fn sync_to_video(&self, video: Option<&Path>) {
        let Some(video) = video else {
            self.inner.st.borrow_mut().active_video = None;
            self.inner.rebuild_rows();
            return;
        };
        let k = self.inner.svc.policy.path_key(video);
        let Some(folder) = parent_of(video) else { return };
        let (current, logged_in) = {
            let mut st = self.inner.st.borrow_mut();
            st.active_video = Some(k);
            (st.current.clone(), st.logged_in)
        };
        self.inner.rebuild_rows();
        let same = current.as_ref().is_some_and(|c| self.inner.same(c, &folder));
        let video = video.to_path_buf();
        if self.inner.svc.policy.explorer_compare == ExplorerCompare::OrdinalIgnoreCase {
            if current.is_none() {
                self.inner.go_with(folder, true, false, ScanMode::Transient);
            } else if same {
                self.inner.rescan_unless_current(video, move |inner| {
                    let (cur, transient) = {
                        let st = inner.st.borrow();
                        (st.current.clone(), st.transient)
                    };
                    if let Some(c) = cur {
                        inner.run_scan(c, false, false, if transient { ScanMode::Transient } else { ScanMode::InMemory });
                    }
                });
            } else {
                self.inner.refresh_other_in_memory(folder);
            }
            return;
        }
        if !logged_in && !same {
            return;
        }
        let follows = std::mem::replace(&mut self.inner.st.borrow_mut().follows_video, false);
        if follows && !same {
            self.inner.go(folder, false, true);
            return;
        }
        if !same {
            self.inner.scan_other(folder);
            return;
        }
        self.inner.rescan_unless_current(video, |inner| {
            let cur = inner.st.borrow().current.clone();
            if let Some(c) = cur {
                inner.run_scan(c, false, false, ScanMode::Persist);
            }
        });
    }
    /// Sidebar badge refresh after a save. Windows refreshes in memory only (the index file is never written for a
    /// save); macOS rescans the current folder, and scans another folder only while logged in.
    pub fn subtitle_saved(&self, path: &Path) {
        let Some(folder) = parent_of(path) else { return };
        let (current, transient, logged_in) = {
            let st = self.inner.st.borrow();
            (st.current.clone(), st.transient, st.logged_in)
        };
        let same = current.as_ref().is_some_and(|c| self.inner.same(c, &folder));
        if !same {
            // A search result from a folder below shows its new badge too.
            self.inner.relist_subtree_folder(folder.clone());
        }
        if self.inner.svc.policy.explorer_compare == ExplorerCompare::OrdinalIgnoreCase {
            if same {
                self.inner.run_scan(folder, false, false, if transient { ScanMode::Transient } else { ScanMode::InMemory });
            } else if current.is_some() {
                self.inner.refresh_other_in_memory(folder);
            }
            return;
        }
        if same {
            self.inner.run_scan(folder, false, false, ScanMode::Persist);
        } else if logged_in {
            self.inner.scan_other(folder);
        }
    }
}
