//! File explorer sidebar (win-viewmodels §21, mac-viewmodels §11; UNIFIED.md §3.10).
//! Rule 6: `explorerLastFolder` removed at startup and never written; logged out the search filters the current
//! folder only (no index-wide results). The constructor touches no file system; listing and scanning run on workers
//! through the media index.

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

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

/// One row.
#[derive(Debug, Clone, PartialEq)]
pub struct ExplorerRow {
    pub path: PathBuf,
    pub name: String,
    pub is_folder: bool,
    pub size_text: String,
    pub sidecar_badge: Option<String>,
    /// From the index (search results outside the current folder).
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

/// Windows OrdinalIgnoreCase `contains`.
fn contains_ignore_case(hay: &str, needle: &str) -> bool {
    hay.to_uppercase().contains(&needle.to_uppercase())
}

/// Windows `FormatByteSize`-like and macOS decimal size texts.
pub fn size_text(bytes: i64, format: ByteSizeFormat) -> String {
    let tr = sa_core::l10n::LanguageState::shared().language() == sa_core::l10n::UiLanguage::Tr;
    let sep = |s: String| if tr { s.replace('.', ",") } else { s };
    let trim = |s: String| {
        if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_owned() } else { s }
    };
    match format {
        ByteSizeFormat::WindowsBinary => {
            if bytes <= 0 {
                return "0 MB".into();
            }
            let mb = bytes as f64 / 1_048_576.0;
            if mb < 1024.0 {
                let s = if mb < 1.0 { format!("{mb:.2}") } else { format!("{mb:.1}") };
                format!("{} MB", sep(trim(s)))
            } else {
                format!("{} GB", sep(trim(format!("{:.2}", mb / 1024.0))))
            }
        }
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

fn parent_of(p: &Path) -> Option<PathBuf> {
    p.parent().filter(|x| !x.as_os_str().is_empty()).map(Path::to_path_buf)
}

fn name_of(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.to_string_lossy().into_owned())
}

struct State {
    current: Option<PathBuf>,
    back: Vec<PathBuf>,
    folders: Vec<String>,
    files: Vec<IndexedMediaFile>,
    rows: Vec<ExplorerRow>,
    query: String,
    logged_in: bool,
    index_hits: Vec<IndexedMediaFile>,
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

    fn run_scan(self: &Rc<Self>, folder: PathBuf, show_cached: bool, crawl: bool) {
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
            let result = index.scan(&folder, &token);
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
        {
            let mut st = self.st.borrow_mut();
            if push
                && let Some(cur) = st.current.clone()
                && !self.same(&cur, &folder)
            {
                st.back.push(cur);
            }
            st.follows_video = false;
            st.query.clear();
            st.index_hits.clear();
            st.current = Some(folder.clone());
        }
        // Rule 6: never persisted (explorerLastFolder).
        self.notify(&[ExplorerChange::Folder]);
        self.run_scan(folder, true, crawl);
    }

    /// Re-applies the search (index-wide only when logged in).
    fn refilter(self: &Rc<Self>, debounce: bool) {
        let (q, logged_in, generation_id) = {
            let mut st = self.st.borrow_mut();
            st.filter_gen += 1;
            (st.query.trim().to_owned(), st.logged_in, st.filter_gen)
        };
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
                back: Vec::new(),
                folders: Vec::new(),
                files: Vec::new(),
                rows: Vec::new(),
                query: String::new(),
                logged_in: false,
                index_hits: Vec::new(),
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
            }),
            handlers: RefCell::new(Vec::new()),
            subs: RefCell::new(Vec::new()),
        });
        let key = UiKey::register(&inner);
        inner.key.set(Some(key));
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
            let initial = quick
                .iter()
                .find(|q| q.kind == QuickLocationKind::Videos && q.path.is_dir())
                .map(|q| q.path.clone())
                .or_else(|| quick.iter().find(|q| q.kind == QuickLocationKind::Home && q.path.is_dir()).map(|q| q.path.clone()))
                .or_else(|| std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from));
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
                    inner.run_scan(f, true, false);
                } else if let Some(c) = current {
                    let scanned = inner.st.borrow().scan_gen > 0;
                    if !scanned {
                        inner.run_scan(c, true, false);
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
    pub fn is_scanning(&self) -> bool {
        self.inner.st.borrow().scanning
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
        if let Some(p) = prev {
            self.inner.go(p, false, false);
        }
    }
    /// Re-scans the current folder (and quick locations).
    pub fn refresh(&self) {
        self.refresh_quick_locations();
        let cur = self.inner.st.borrow().current.clone();
        if let Some(c) = cur {
            self.inner.run_scan(c, false, false);
        }
    }
    pub fn rows(&self) -> Vec<ExplorerRow> {
        self.inner.st.borrow().rows.clone()
    }
    pub fn search_query(&self) -> String {
        self.inner.st.borrow().query.clone()
    }
    /// Logged out: only the current folder is filtered (rule 6).
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
    /// The hub's session ended or began: drops index hits immediately when logged out.
    pub fn set_logged_in(&self, logged_in: bool) {
        let q = self.search_query();
        self.set_search(&q, logged_in);
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
    /// The first opened video moves a fresh explorer to its folder; later ones refresh the index for their folder.
    pub fn sync_to_video(&self, video: Option<&Path>) {
        let Some(video) = video else {
            self.inner.st.borrow_mut().active_video = None;
            self.inner.rebuild_rows();
            return;
        };
        let k = self.inner.svc.policy.path_key(video);
        let Some(folder) = parent_of(video) else { return };
        let (follows, current) = {
            let mut st = self.inner.st.borrow_mut();
            st.active_video = Some(k);
            (std::mem::replace(&mut st.follows_video, false), st.current.clone())
        };
        self.inner.rebuild_rows();
        let same = current.as_ref().is_some_and(|c| self.inner.same(c, &folder));
        if follows && !same {
            self.inner.go(folder, false, true);
            return;
        }
        if !same {
            self.inner.scan_other(folder);
            return;
        }
        self.inner.run_scan(folder, false, false);
    }
    /// Sidebar badge refresh after a save.
    pub fn subtitle_saved(&self, path: &Path) {
        let Some(folder) = parent_of(path) else { return };
        let current = self.inner.st.borrow().current.clone();
        if current.as_ref().is_some_and(|c| self.inner.same(c, &folder)) {
            self.inner.run_scan(folder, false, false);
        } else {
            self.inner.scan_other(folder);
        }
    }
}
