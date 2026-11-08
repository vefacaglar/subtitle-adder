//! Ports of LibraryKit's `MediaLibraryIndexTests`, `MediaIndexOverlapTests` and `MediaIndexPersistenceTests`.
//!
//! The index logic runs on every platform; the folder tree is virtual (a [`VirtualFs`] lister that produces the
//! [`RawEntry`] values the macOS scanner would) so these tests run on the Windows build machine too. The index file
//! itself is a real temp file. Tests that need APFS (resource flags, real symlinks) are covered by the virtual tree's
//! hidden/package/link nodes; `scanner.rs` is exercised on a Mac.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use parking_lot::Mutex;
use sa_core::cancel::CancellationToken;
use serde_json::Value;

use super::listing::{RawEntry, build_listing};
use super::{FolderLister, MacMediaIndex, MacScanOutcome, path};
use crate::dates::{self, unix_seconds_ticks};
use crate::schema::{DirectorySnapshot, IndexedMediaFile};
use crate::text::nfc;
use crate::{MediaIndex, ScanResult};

// ---- virtual file system -----------------------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum Node {
    Dir { unreadable: bool, hidden: bool },
    File { size: i64, modified: i64, hidden: bool },
    Link { target: String },
}

#[derive(Default)]
struct VirtualFs {
    nodes: Mutex<BTreeMap<String, Node>>,
}

impl VirtualFs {
    fn ensure_parents(nodes: &mut BTreeMap<String, Node>, p: &str) {
        let mut cur = path::parent(p);
        while let Some(parent) = cur {
            nodes.entry(parent.clone()).or_insert(Node::Dir { unreadable: false, hidden: false });
            cur = path::parent(&parent);
        }
    }
    fn dir(&self, p: &str) {
        let mut n = self.nodes.lock();
        Self::ensure_parents(&mut n, p);
        n.insert(p.into(), Node::Dir { unreadable: false, hidden: false });
    }
    fn file(&self, p: &str, size: i64, modified: i64) {
        let mut n = self.nodes.lock();
        Self::ensure_parents(&mut n, p);
        n.insert(p.into(), Node::File { size, modified, hidden: false });
    }
    fn set(&self, p: &str, node: Node) {
        let mut n = self.nodes.lock();
        Self::ensure_parents(&mut n, p);
        n.insert(p.into(), node);
    }
    fn remove(&self, p: &str) {
        let key = path::key(p);
        self.nodes.lock().retain(|k, _| {
            let kk = path::key(k);
            kk != key && !kk.starts_with(&format!("{key}/"))
        });
    }
    /// Case-insensitive lookup (APFS).
    fn find(&self, p: &str) -> Option<(String, Node)> {
        let key = path::key(p);
        self.nodes.lock().iter().find(|(k, _)| path::key(k) == key).map(|(k, v)| (k.clone(), v.clone()))
    }
    fn resolve(&self, node: Node) -> Option<Node> {
        match node {
            Node::Link { target } => self.find(&target).map(|(_, n)| n),
            n => Some(n),
        }
    }
}

impl FolderLister for VirtualFs {
    fn list(&self, directory: &Path) -> ScanResult {
        let p = path::from_path(directory);
        let Some((stored, node)) = self.find(&p) else { return ScanResult::Missing };
        match self.resolve(node) {
            Some(Node::Dir { unreadable: true, .. }) => return ScanResult::Unreadable,
            Some(Node::Dir { .. }) => {}
            _ => return ScanResult::Missing,
        }
        let children: Vec<(String, Node)> = self
            .nodes
            .lock()
            .iter()
            .filter(|(k, _)| path::parent(k).is_some_and(|par| path::key(&par) == path::key(&stored)))
            .map(|(k, v)| (path::from_path(Path::new(k)).rsplit('/').next().unwrap().to_owned(), v.clone()))
            .collect();
        let entries = children
            .into_iter()
            .map(|(name, node)| {
                let link_hidden = matches!(node, Node::Dir { hidden: true, .. } | Node::File { hidden: true, .. });
                let is_link = matches!(node, Node::Link { .. });
                let target = self.resolve(node);
                let dangling = is_link && target.is_none();
                let (is_dir, size, modified) = match target {
                    Some(Node::Dir { .. }) => (true, None, None),
                    Some(Node::File { size, modified, .. }) => (false, Some(size), Some(modified)),
                    _ => (false, None, None),
                };
                let is_package = is_dir
                    && name.rfind('.').is_some_and(|d| ["app", "photoslibrary", "bundle"].contains(&&name[d + 1..]));
                RawEntry { name, hidden: link_hidden, is_dir, is_package, dangling, size, modified }
            })
            .collect::<Vec<_>>();
        ScanResult::Listed(build_listing(&p, &entries, dates::now_ticks()))
    }
}

struct Sandbox {
    base: tempfile::TempDir,
    fs: Arc<VirtualFs>,
    root: String,
}

impl Sandbox {
    fn new() -> Self {
        let fs = Arc::new(VirtualFs::default());
        let root = "/sandbox/Media".to_owned();
        fs.dir(&root);
        Self { base: tempfile::Builder::new().prefix("MediaIndexTests-").tempdir().unwrap(), fs, root }
    }
    fn index_file(&self) -> PathBuf {
        self.base.path().join("media-index.json")
    }
    fn index(&self) -> Arc<MacMediaIndex> {
        self.index_at(self.index_file())
    }
    fn index_at(&self, file: PathBuf) -> Arc<MacMediaIndex> {
        MacMediaIndex::new(Some(file), Some(self.fs.clone()))
    }
    fn p(&self, rel: &str) -> String {
        if rel.is_empty() { self.root.clone() } else { format!("{}/{rel}", self.root) }
    }
    fn folder(&self, rel: &str) -> PathBuf {
        self.fs.dir(&self.p(rel));
        PathBuf::from(self.p(rel))
    }
    fn file(&self, rel: &str) {
        self.file_sized(rel, 1, unix_seconds_ticks(1_700_000_000));
    }
    fn file_sized(&self, rel: &str, size: i64, modified: i64) {
        self.fs.file(&self.p(rel), size, modified);
    }
    fn remove(&self, rel: &str) {
        self.fs.remove(&self.p(rel));
    }
    /// The path the index uses for `root/<rel>`.
    fn path(&self, rel: &str) -> String {
        path::normalized(&self.p(rel))
    }
}

fn pb(s: &str) -> PathBuf {
    PathBuf::from(s)
}

fn names(files: &[IndexedMediaFile]) -> Vec<String> {
    files.iter().map(|f| f.file_name.clone()).collect()
}

fn scanned(o: MacScanOutcome) -> DirectorySnapshot {
    match o {
        MacScanOutcome::Scanned(s) => s,
        other => panic!("expected Scanned, got {other:?}"),
    }
}

fn crawl(index: &MacMediaIndex, root: &Path, depth: i32) -> usize {
    index.crawl_folders(root, depth, 64, &CancellationToken::new(), &|_| {})
}

// ---- MediaLibraryIndexTests -------------------------------------------------------------------------------------------

#[test]
fn scan_populates_media_files_and_detects_sidecar_subtitles() {
    let b = Sandbox::new();
    let videos = b.folder("Videos");
    b.folder("Videos/SubfolderA");
    b.file_sized("Videos/episode01.mp4", 2048, 0);
    b.file("Videos/episode01.srt");
    b.file_sized("Videos/movie.mkv", 4096, 0);
    b.file("Videos/notes.txt");
    b.file("Videos/Film.mp4");
    b.file("Videos/Film.tr.srt");
    let index = b.index();
    let result = scanned(index.scan_folder_outcome(&videos));
    assert_eq!(result.subdirectories, [b.path("Videos/SubfolderA")]);
    assert_eq!(names(&result.media_files), ["episode01.mp4", "Film.mp4", "movie.mkv"]);
    assert_eq!(index.total_indexed_files(), 3);
    let ep = &result.media_files[0];
    assert!(ep.has_sidecar_subtitle);
    assert_eq!(ep.sidecar_extension.as_deref(), Some(".srt"));
    assert_eq!(ep.size_bytes, 2048);
    assert_eq!(ep.full_path, b.path("Videos/episode01.mp4"));
    assert_eq!(ep.directory_path, b.path("Videos"));
    let film = &result.media_files[1];
    assert!(film.has_sidecar_subtitle, "a language-tagged subtitle shows the badge");
    assert_eq!(film.sidecar_extension.as_deref(), Some(".srt"));
    let movie = &result.media_files[2];
    assert!(!movie.has_sidecar_subtitle);
    assert_eq!(movie.sidecar_extension, None);
    assert_eq!(movie.size_bytes, 4096);
}

#[test]
fn persistence_reloads_cached_entries_in_a_new_instance() {
    let b = Sandbox::new();
    let library = b.folder("Library");
    b.file_sized("Library/lecture.mp4", 1024, 0);
    b.file("Library/lecture.vtt");
    b.index().scan_folder_outcome(&library);
    assert!(b.index_file().exists());
    let reloaded = b.index();
    assert_eq!(reloaded.total_indexed_files(), 1);
    assert_eq!(reloaded.indexed_directories(), [b.path("Library")]);
    let item = reloaded.cached_directory(&library).remove(0);
    assert_eq!(item.file_name, "lecture.mp4");
    assert!(item.has_sidecar_subtitle);
    assert_eq!(item.sidecar_extension.as_deref(), Some(".vtt"));
}

#[test]
fn rescanning_prunes_deleted_files() {
    let b = Sandbox::new();
    let folder = b.folder("PruneTest");
    b.file("PruneTest/keep.mp4");
    b.file("PruneTest/remove.mkv");
    let index = b.index();
    assert_eq!(index.scan_folder_outcome(&folder).snapshot().media_files.len(), 2);
    assert_eq!(index.total_indexed_files(), 2);
    b.remove("PruneTest/remove.mkv");
    assert_eq!(names(&index.scan_folder_outcome(&folder).snapshot().media_files), ["keep.mp4"]);
    assert_eq!(index.total_indexed_files(), 1);
    assert!(index.search("remove", 100).is_empty());
}

#[test]
fn crawl_and_search_find_files_across_nested_folders() {
    let b = Sandbox::new();
    b.file("Root/intro.mp4");
    b.file("Root/Season1/s01e01_pilot.mkv");
    b.file("Root/Season2/Extras/behind_the_scenes.mov");
    let index = b.index();
    crawl(&index, &pb(&b.p("Root")), 2);
    assert_eq!(index.total_indexed_files(), 3);
    assert_eq!(names(&index.search("pilot", 100)), ["s01e01_pilot.mkv"]);
    assert_eq!(names(&index.search("behind", 100)), ["behind_the_scenes.mov"]);
    assert_eq!(names(&index.search("Season1", 100)), ["s01e01_pilot.mkv"]);
}

#[test]
fn navigation_queries_serve_cached_folders_and_global_search() {
    let b = Sandbox::new();
    let parent = b.folder("Parent");
    let child = b.folder("Parent/Child");
    let sibling = b.folder("OtherIndexed");
    b.file_sized("Parent/parent_clip.mp4", 512, 0);
    b.file_sized("Parent/Child/child_movie.mkv", 1024, 0);
    b.file_sized("OtherIndexed/global_special_doc.mp4", 2048, 0);
    let index = b.index();
    index.scan_folder_outcome(&sibling);
    crawl(&index, &parent, 2);
    assert_eq!(index.cached_subdirectories(&parent), [path::normalized(&path::from_path(&child))]);
    assert_eq!(names(&index.cached_directory(&parent)), ["parent_clip.mp4"]);
    assert_eq!(names(&index.cached_directory(&child)), ["child_movie.mkv"]);
    assert_eq!(index.total_indexed_files(), 3);
    assert_eq!(names(&index.search("global_special", 100)), ["global_special_doc.mp4"]);
}

#[test]
fn rescan_picks_up_a_new_sidecar() {
    let b = Sandbox::new();
    let folder = b.folder("SyncFolder");
    b.file_sized("SyncFolder/interview.mp4", 1024, 0);
    let index = b.index();
    assert!(!index.scan_folder_outcome(&folder).snapshot().media_files[0].has_sidecar_subtitle);
    b.file("SyncFolder/interview.srt");
    let after = scanned(index.scan_folder_outcome(&folder));
    assert!(after.media_files[0].has_sidecar_subtitle);
    assert_eq!(after.media_files[0].sidecar_extension.as_deref(), Some(".srt"));
    assert_eq!(index.cached_directory(&folder)[0].sidecar_extension.as_deref(), Some(".srt"));
    // The trait's invalidation rescans an indexed folder.
    b.remove("SyncFolder/interview.srt");
    b.file("SyncFolder/interview.vtt");
    index.invalidate_sidecar(&pb(&b.p("SyncFolder/interview.mp4")));
    assert_eq!(index.cached_directory(&folder)[0].sidecar_extension.as_deref(), Some(".vtt"));
    // …but never indexes a folder that was not indexed.
    b.file("Elsewhere/v.mp4");
    index.invalidate_sidecar(&pb(&b.p("Elsewhere/v.mp4")));
    assert!(index.cached_snapshot(&pb(&b.p("Elsewhere"))).is_none());
}

#[test]
fn hidden_dot_and_package_entries_are_skipped() {
    let b = Sandbox::new();
    let folder = b.folder("Mixed");
    b.file("Mixed/visible.mp4");
    b.file("Mixed/.dotfile.mp4");
    b.file("Mixed/._visible.mp4");
    b.fs.set(&b.p("Mixed/flagged.mp4"), Node::File { size: 1, modified: 0, hidden: true });
    b.folder("Mixed/Shows");
    b.folder("Mixed/.git");
    b.folder("Mixed/Tool.app");
    b.folder("Mixed/Pictures.photoslibrary");
    b.file("Mixed/Tool.app/inside.mp4");
    b.file(".hiddenfolder/secret.mp4");
    b.file("Mixed/.git/hook.mp4");
    let index = b.index();
    let snap = scanned(index.scan_folder_outcome(&folder));
    assert_eq!(names(&snap.media_files), ["visible.mp4"]);
    assert_eq!(snap.subdirectories, [b.path("Mixed/Shows")]);
    crawl(&index, &pb(&b.root), 3);
    assert_eq!(index.total_indexed_files(), 1);
    assert!(index.search("secret", 100).is_empty());
    assert!(index.search("inside", 100).is_empty());
    assert!(index.search("hook", 100).is_empty());
}

#[test]
fn files_are_sorted_by_name_with_numbers_as_numbers() {
    let b = Sandbox::new();
    let folder = b.folder("Order");
    for n in ["Episode 10.mp4", "episode 2.mp4", "Episode 1.mp4", "Zeta.mp4", "alpha.mp4"] {
        b.file(&format!("Order/{n}"));
    }
    for n in ["b10", "B9", "a"] {
        b.folder(&format!("Order/{n}"));
    }
    let snap = scanned(b.index().scan_folder_outcome(&folder));
    assert_eq!(names(&snap.media_files), ["alpha.mp4", "Episode 1.mp4", "episode 2.mp4", "Episode 10.mp4", "Zeta.mp4"]);
    let last: Vec<&str> = snap.subdirectories.iter().map(|s| s.rsplit('/').next().unwrap()).collect();
    assert_eq!(last, ["a", "B9", "b10"]);
}

#[test]
fn names_are_stored_nfc_normalized() {
    let b = Sandbox::new();
    let folder = b.folder("Names");
    let nfc_name = "Çalışma Şarkı.mp4";
    let nfd: String = unicode_normalization::UnicodeNormalization::nfd(nfc_name).collect();
    assert_ne!(nfd, nfc_name);
    b.file(&format!("Names/{nfd}"));
    let file = scanned(b.index().scan_folder_outcome(&folder)).media_files.remove(0);
    assert_eq!(file.file_name, nfc(nfc_name));
    assert_eq!(file.full_path, nfc(&file.full_path));
}

#[test]
fn symbolic_links_count_as_their_targets() {
    let b = Sandbox::new();
    let folder = b.folder("Links");
    b.file_sized("Elsewhere/real.mp4", 777, 0);
    b.folder("Elsewhere/Season");
    b.fs.set(&b.p("Links/alias.mp4"), Node::Link { target: b.p("Elsewhere/real.mp4") });
    b.fs.set(&b.p("Links/Season"), Node::Link { target: b.p("Elsewhere/Season") });
    b.fs.set(&b.p("Links/dangling.mp4"), Node::Link { target: b.p("nowhere.mp4") });
    let snap = scanned(b.index().scan_folder_outcome(&folder));
    assert_eq!(names(&snap.media_files), ["alias.mp4"]);
    assert_eq!(snap.media_files[0].size_bytes, 777);
    assert_eq!(snap.subdirectories, [b.path("Links/Season")]);
}

#[test]
fn deleted_subtree_is_removed_from_search_and_from_its_parent() {
    let b = Sandbox::new();
    let root = b.folder("Root");
    b.file("Root/keep.mp4");
    b.file("Root/Show/Season1/ep1.mp4");
    b.file("Root/Show/Season2/ep2.mp4");
    let show = pb(&b.p("Root/Show"));
    let index = b.index();
    crawl(&index, &root, 3);
    assert_eq!(index.total_indexed_files(), 3);

    b.remove("Root/Show/Season1");
    let season = index.scan_folder_outcome(&pb(&b.p("Root/Show/Season1")));
    assert!(matches!(season, MacScanOutcome::Missing(ref s) if s.media_files.is_empty()));
    assert!(index.search("ep1", 100).is_empty());
    let subs: Vec<String> = index.cached_subdirectories(&show).iter().map(|s| s.rsplit('/').next().unwrap().to_owned()).collect();
    assert_eq!(subs, ["Season2"]);
    assert_eq!(index.total_indexed_files(), 2);

    b.remove("Root/Show");
    assert!(matches!(index.scan_folder_outcome(&show), MacScanOutcome::Missing(_)));
    assert!(index.search("ep2", 100).is_empty());
    assert!(index.cached_snapshot(&show.join("Season2")).is_none());
    assert!(index.cached_subdirectories(&root).is_empty());
    assert_eq!(index.indexed_directories(), [b.path("Root")]);
    let reloaded = b.index();
    assert_eq!(reloaded.total_indexed_files(), 1);
    assert_eq!(reloaded.indexed_directories().len(), 1);
}

#[test]
fn rescanning_a_parent_drops_folders_that_vanished_from_it() {
    let b = Sandbox::new();
    let root = b.folder("Root");
    b.file("Root/Old/Deep/clip.mp4");
    b.file("Root/New/fresh.mp4");
    let index = b.index();
    crawl(&index, &root, 3);
    assert_eq!(index.total_indexed_files(), 2);
    b.remove("Root/Old");
    assert_eq!(scanned(index.scan_folder_outcome(&root)).subdirectories, [b.path("Root/New")]);
    assert!(index.search("clip", 100).is_empty());
    assert_eq!(index.indexed_directories(), [b.path("Root"), b.path("Root/New")]);
}

#[test]
fn scanning_a_folder_that_was_never_indexed_and_does_not_exist_changes_nothing() {
    let b = Sandbox::new();
    let index = b.index();
    assert!(matches!(index.scan_folder_outcome(&pb(&b.p("Nope"))), MacScanOutcome::Missing(_)));
    assert!(index.indexed_directories().is_empty());
    assert!(!b.index_file().exists());
    b.file("plain.mp4");
    assert!(matches!(index.scan_folder_outcome(&pb(&b.p("plain.mp4"))), MacScanOutcome::Missing(_)), "a file is not a folder");
    assert_eq!(index.scan(&pb(&b.p("Nope")), &CancellationToken::new()), ScanResult::Missing);
}

#[test]
fn failed_enumeration_keeps_the_cached_entry() {
    let b = Sandbox::new();
    let locked = b.folder("Locked");
    b.file("Locked/keep.mp4");
    b.folder("Locked/Sub");
    let never = b.folder("Never");
    b.file("Never/x.mp4");
    let index = b.index();
    assert_eq!(index.scan_folder_outcome(&locked).snapshot().media_files.len(), 1);
    let before = index.cached_snapshot(&locked).unwrap();
    b.fs.set(&b.p("Locked"), Node::Dir { unreadable: true, hidden: false });
    b.fs.set(&b.p("Never"), Node::Dir { unreadable: true, hidden: false });

    let result = index.scan_folder_outcome(&locked);
    assert_eq!(result, MacScanOutcome::Unreadable(before.clone()));
    assert_eq!(index.cached_snapshot(&locked).unwrap(), before);
    assert_eq!(names(&index.cached_directory(&locked)), ["keep.mp4"]);
    assert_eq!(index.cached_subdirectories(&locked), [b.path("Locked/Sub")]);
    assert_eq!(index.total_indexed_files(), 1);
    assert_eq!(index.scan(&locked, &CancellationToken::new()), ScanResult::Unreadable);

    let fresh = index.scan_folder_outcome(&never);
    assert!(matches!(fresh, MacScanOutcome::Unreadable(ref s) if s.media_files.is_empty()));
    assert!(index.cached_snapshot(&never).is_none());

    b.fs.set(&b.p("Locked"), Node::Dir { unreadable: false, hidden: false });
    b.file("Locked/second.mp4");
    assert_eq!(names(&scanned(index.scan_folder_outcome(&locked)).media_files), ["keep.mp4", "second.mp4"]);
}

#[test]
fn crawl_honors_depth_and_folder_limits() {
    let b = Sandbox::new();
    let root = b.folder("Deep");
    b.file("Deep/a.mp4");
    b.file("Deep/L1/b.mp4");
    b.file("Deep/L1/L2/c.mp4");
    b.file("Deep/L1/L2/L3/d.mp4");
    b.file("Deep/L1/L2/L3/L4/e.mp4");
    b.file("Deep/L1/L2/L3/L4/L5/f.mp4");
    b.file("Deep/L1/L2/L3/L4/L5/L6/g.mp4");
    let shallow = b.index();
    assert_eq!(crawl(&shallow, &root, 0), 1);
    assert_eq!(shallow.total_indexed_files(), 1);
    assert_eq!(crawl(&shallow, &root, 1), 2);
    assert_eq!(shallow.total_indexed_files(), 2);
    let deep = b.index_at(b.base.path().join("other.json"));
    assert_eq!(crawl(&deep, &root, 99), 6);
    assert_eq!(deep.search("f.mp4", 100).len(), 1);
    assert!(deep.search("g.mp4", 100).is_empty());
    let negative = b.index_at(b.base.path().join("negative.json"));
    assert_eq!(crawl(&negative, &root, -3), 1);
    let limited = b.index_at(b.base.path().join("limited.json"));
    assert_eq!(limited.crawl_folders(&root, 5, 3, &CancellationToken::new(), &|_| {}), 3);
    assert_eq!(limited.total_indexed_files(), 3);
}

#[test]
fn crawl_goes_breadth_first() {
    let b = Sandbox::new();
    let root = b.folder("Wide");
    b.file("Wide/A/Deep/a.mp4");
    b.file("Wide/B/b.mp4");
    b.file("Wide/C/c.mp4");
    let index = b.index();
    assert_eq!(index.crawl_folders(&root, 5, 4, &CancellationToken::new(), &|_| {}), 4);
    assert!(index.search("a.mp4", 100).is_empty());
    assert_eq!(index.search("b.mp4", 100).len(), 1);
    assert_eq!(index.search("c.mp4", 100).len(), 1);
}

#[test]
fn crawl_of_a_missing_root_is_harmless() {
    let b = Sandbox::new();
    let index = b.index();
    assert_eq!(crawl(&index, &pb(&b.p("Gone")), 2), 0);
    assert!(!b.index_file().exists());
}

#[test]
fn cancelled_crawl_persists_what_was_scanned() {
    let b = Sandbox::new();
    let root = b.folder("Root");
    b.file("Root/first.mp4");
    for n in ["One", "Two", "Three"] {
        b.file(&format!("Root/{n}/{}.mp4", n.to_lowercase()));
    }
    let index = b.index();
    let cancel = CancellationToken::new();
    let c = cancel.clone();
    assert_eq!(index.crawl_folders(&root, 1, 64, &cancel, &move |_| c.cancel()), 1);
    assert_eq!(index.total_indexed_files(), 1);
    let reloaded = b.index();
    assert_eq!(names(&reloaded.cached_directory(&root)), ["first.mp4"]);
    assert_eq!(reloaded.cached_subdirectories(&root).len(), 3);
    assert_eq!(reloaded.total_indexed_files(), 1);
    assert_eq!(reloaded.indexed_directories().len(), 1);
}

#[test]
fn crawl_reports_progress_per_folder() {
    let b = Sandbox::new();
    let root = b.folder("Root");
    b.file("Root/A/a.mp4");
    b.file("Root/B/b.mp4");
    let counts = Mutex::new(Vec::new());
    b.index().crawl_folders(&root, 2, 64, &CancellationToken::new(), &|n| counts.lock().push(n));
    assert_eq!(*counts.lock(), [1, 2, 3]);
}

#[test]
fn search_ranks_prefix_then_name_then_folder_then_newest() {
    let b = Sandbox::new();
    let (old, mid, new) = (unix_seconds_ticks(1_000_000), unix_seconds_ticks(2_000_000), unix_seconds_ticks(3_000_000));
    b.file_sized("Season Alpha/beta.mp4", 1, new);
    b.file_sized("misc/the alpha.mp4", 1, old);
    b.file_sized("misc/alpha one.mp4", 1, old);
    b.file_sized("misc/alpha b.mp4", 1, mid);
    b.file_sized("misc/alpha a.mp4", 1, mid);
    b.file_sized("misc/unrelated.mp4", 1, new);
    let index = b.index();
    crawl(&index, &pb(&b.root), 2);
    assert_eq!(names(&index.search("  Alpha ", 100)), ["alpha a.mp4", "alpha b.mp4", "alpha one.mp4", "the alpha.mp4", "beta.mp4"]);
    assert_eq!(names(&index.search("alpha", 2)), ["alpha a.mp4", "alpha b.mp4"]);
    assert!(index.search("alpha", 0).is_empty());
}

#[test]
fn blank_query_finds_nothing() {
    let b = Sandbox::new();
    b.file("a.mp4");
    let index = b.index();
    crawl(&index, &pb(&b.root), 2);
    assert!(index.search("", 100).is_empty());
    assert!(index.search("   \n", 100).is_empty());
    assert_eq!(index.all_files(usize::MAX).len(), 1);
}

#[test]
fn search_ignores_case_and_diacritics() {
    let b = Sandbox::new();
    b.file("Işık Şarkı.mp4");
    b.file("İstanbul Gezisi.mp4");
    b.file("Crème Brûlée.mkv");
    let index = b.index();
    crawl(&index, &pb(&b.root), 2);
    assert_eq!(names(&index.search("isik", 100)), ["Işık Şarkı.mp4"]);
    assert_eq!(names(&index.search("SARKI", 100)), ["Işık Şarkı.mp4"]);
    assert_eq!(names(&index.search("ışık şarkı", 100)), ["Işık Şarkı.mp4"]);
    assert_eq!(names(&index.search("istanbul", 100)), ["İstanbul Gezisi.mp4"]);
    assert_eq!(names(&index.search("İSTANBUL", 100)), ["İstanbul Gezisi.mp4"]);
    assert_eq!(names(&index.search("creme brulee", 100)), ["Crème Brûlée.mkv"]);
}

#[test]
fn all_files_lists_everything_newest_first() {
    let b = Sandbox::new();
    b.file_sized("one/oldest.mp4", 1, unix_seconds_ticks(1_000));
    b.file_sized("two/newest.mkv", 1, unix_seconds_ticks(3_000));
    b.file_sized("one/middle.mov", 1, unix_seconds_ticks(2_000));
    b.file_sized("two/also middle.mp3", 1, unix_seconds_ticks(2_000));
    let index = b.index();
    assert!(index.all_files(usize::MAX).is_empty());
    crawl(&index, &pb(&b.root), 2);
    assert_eq!(names(&index.all_files(usize::MAX)), ["newest.mkv", "also middle.mp3", "middle.mov", "oldest.mp4"]);
    assert_eq!(names(&index.all_files(2)), ["newest.mkv", "also middle.mp3"]);
    assert!(index.all_files(0).is_empty());
}

#[test]
fn directories_are_matched_case_insensitively_but_keep_their_case() {
    let b = Sandbox::new();
    let folder = b.folder("Videos");
    b.file("Videos/a.mp4");
    let index = b.index();
    index.scan_folder_outcome(&folder);
    assert_eq!(names(&index.cached_directory(&pb(&b.p("VIDEOS")))), ["a.mp4"]);
    assert_eq!(names(&index.cached_directory(&pb(&format!("{}/../Videos/", b.p("Videos"))))), ["a.mp4"]);
    assert_eq!(index.indexed_directories(), [b.path("Videos")]);
    assert!(index.cached_directory(&pb(&b.p("Other"))).is_empty());
    assert!(index.cached_subdirectories(&pb(&b.p("Other"))).is_empty());
}

// ---- MediaIndexOverlapTests -----------------------------------------------------------------------------------------

/// A one-shot latch.
#[derive(Clone)]
struct Gate {
    tx: Sender<()>,
    rx: Receiver<()>,
}

impl Gate {
    fn new() -> Self {
        let (tx, rx) = crossbeam_channel::bounded(1);
        Self { tx, rx }
    }
    fn open(&self) {
        let _ = self.tx.try_send(());
    }
    fn wait(&self) {
        let _ = self.rx.recv_timeout(Duration::from_secs(10));
    }
}

#[derive(Clone)]
struct Step {
    listing: ScanResult,
    gate: Option<Gate>,
}

/// Answers each listing of a folder from a script, in call order (the last step repeats).
#[derive(Default)]
struct ScriptedFolders {
    script: Mutex<BTreeMap<String, Vec<Step>>>,
    started: Mutex<BTreeMap<String, usize>>,
}

impl ScriptedFolders {
    fn script(&self, p: &str, steps: Vec<Step>) {
        self.script.lock().entry(p.into()).or_default().extend(steps);
    }
    fn started_count(&self, p: &str) -> usize {
        self.started.lock().get(p).copied().unwrap_or(0)
    }
}

impl FolderLister for ScriptedFolders {
    fn list(&self, directory: &Path) -> ScanResult {
        let p = path::from_path(directory);
        let step = {
            let mut started = self.started.lock();
            let i = started.entry(p.clone()).or_insert(0);
            let index = *i;
            *i += 1;
            let script = self.script.lock();
            script.get(&p).and_then(|steps| steps.get(index).or(steps.last()).cloned())
        };
        let Some(step) = step else { return ScanResult::Missing };
        if let Some(g) = &step.gate {
            g.wait();
        }
        step.listing
    }
}

fn vfile(name: &str, dir: &str) -> IndexedMediaFile {
    IndexedMediaFile {
        full_path: format!("{dir}/{name}"),
        file_name: name.into(),
        directory_path: dir.into(),
        size_bytes: 1,
        last_write_time_utc: unix_seconds_ticks(1_700_000_000),
        has_sidecar_subtitle: false,
        sidecar_extension: None,
        indexed_at_utc: unix_seconds_ticks(1_700_000_000),
    }
}

fn vlisted(dir: &str, subs: &[&str], files: &[&str]) -> ScanResult {
    ScanResult::Listed(DirectorySnapshot {
        directory_path: dir.into(),
        subdirectories: subs.iter().map(|s| (*s).to_owned()).collect(),
        media_files: files.iter().map(|f| vfile(f, dir)).collect(),
        scanned_at_utc: dates::now_ticks(),
    })
}

fn step(listing: ScanResult) -> Step {
    Step { listing, gate: None }
}

fn gated(listing: ScanResult, gate: &Gate) -> Step {
    Step { listing, gate: Some(gate.clone()) }
}

fn wait_until(cond: impl Fn() -> bool) {
    for _ in 0..400 {
        if cond() {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

const ROOT: &str = "/virtual/library";

fn overlap_index(folders: &Arc<ScriptedFolders>) -> (Arc<MacMediaIndex>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    (MacMediaIndex::new(Some(dir.path().join("media-index.json")), Some(folders.clone())), dir)
}

#[test]
fn an_older_scan_that_finishes_last_does_not_overwrite_a_fresher_listing() {
    let folders = Arc::new(ScriptedFolders::default());
    let gate = Gate::new();
    folders.script(ROOT, vec![gated(vlisted(ROOT, &[], &["old.mp4"]), &gate), step(vlisted(ROOT, &[], &["new.mp4"]))]);
    let (index, _d) = overlap_index(&folders);
    let older = {
        let index = index.clone();
        std::thread::spawn(move || index.scan_folder_outcome(&pb(ROOT)))
    };
    wait_until(|| folders.started_count(ROOT) == 1);
    let fresher = index.scan_folder_outcome(&pb(ROOT));
    assert_eq!(names(&fresher.snapshot().media_files), ["new.mp4"]);
    gate.open();
    let result = older.join().unwrap();
    assert_eq!(names(&scanned(result).media_files), ["new.mp4"], "the stale scan reports the fresher listing");
    assert_eq!(names(&index.cached_directory(&pb(ROOT))), ["new.mp4"]);
    assert!(index.search("old", 100).is_empty());
    assert_eq!(index.total_indexed_files(), 1);
}

#[test]
fn a_late_arrival_of_an_older_listing_of_two_files_leaves_the_fresher_one_file() {
    let folders = Arc::new(ScriptedFolders::default());
    let gate = Gate::new();
    folders.script(ROOT, vec![gated(vlisted(ROOT, &[], &["a.mp4", "b.mp4"]), &gate), step(vlisted(ROOT, &[], &["a.mp4"]))]);
    let (index, _d) = overlap_index(&folders);
    let older = {
        let index = index.clone();
        std::thread::spawn(move || index.scan_folder_outcome(&pb(ROOT)))
    };
    wait_until(|| folders.started_count(ROOT) == 1);
    index.scan_folder_outcome(&pb(ROOT));
    gate.open();
    older.join().unwrap();
    assert_eq!(names(&index.cached_directory(&pb(ROOT))), ["a.mp4"]);
}

#[test]
fn a_stale_listing_does_not_drop_a_folder_a_fresher_scan_found() {
    let folders = Arc::new(ScriptedFolders::default());
    let gate = Gate::new();
    let a = format!("{ROOT}/a");
    let c = format!("{ROOT}/c");
    folders.script(ROOT, vec![step(vlisted(ROOT, &[&a, &c], &[])), gated(vlisted(ROOT, &[&a], &[]), &gate)]);
    folders.script(&c, vec![step(vlisted(&c, &[], &["early.mp4"])), step(vlisted(&c, &[], &["fresh.mp4"]))]);
    let (index, _d) = overlap_index(&folders);
    index.scan_folder_outcome(&pb(ROOT));
    index.scan_folder_outcome(&pb(&c));
    assert_eq!(names(&index.cached_directory(&pb(&c))), ["early.mp4"]);
    let slow = {
        let index = index.clone();
        std::thread::spawn(move || index.scan_folder_outcome(&pb(ROOT)))
    };
    wait_until(|| folders.started_count(ROOT) == 2);
    index.scan_folder_outcome(&pb(&c));
    gate.open();
    let result = scanned(slow.join().unwrap());
    assert_eq!(names(&index.cached_directory(&pb(&c))), ["fresh.mp4"], "the fresher folder survives");
    assert_eq!(index.cached_subdirectories(&pb(ROOT)), [a.clone(), c.clone()]);
    assert_eq!(result.subdirectories, [a, c]);
}

#[test]
fn a_folder_no_scan_has_seen_since_is_still_dropped_when_a_later_listing_lacks_it() {
    let folders = Arc::new(ScriptedFolders::default());
    let a = format!("{ROOT}/a");
    let c = format!("{ROOT}/c");
    folders.script(ROOT, vec![step(vlisted(ROOT, &[&a, &c], &[])), step(vlisted(ROOT, &[&a], &[]))]);
    folders.script(&c, vec![step(vlisted(&c, &[], &["x.mp4"]))]);
    let (index, _d) = overlap_index(&folders);
    index.scan_folder_outcome(&pb(ROOT));
    index.scan_folder_outcome(&pb(&c));
    index.scan_folder_outcome(&pb(ROOT));
    assert!(index.cached_snapshot(&pb(&c)).is_none());
    assert_eq!(index.cached_subdirectories(&pb(ROOT)), [a]);
}

#[test]
fn a_stale_gone_result_does_not_remove_a_folder_the_fresher_scan_found() {
    let folders = Arc::new(ScriptedFolders::default());
    let gate = Gate::new();
    folders.script(ROOT, vec![gated(ScanResult::Missing, &gate), step(vlisted(ROOT, &[], &["back.mp4"]))]);
    let (index, _d) = overlap_index(&folders);
    let stale = {
        let index = index.clone();
        std::thread::spawn(move || index.scan_folder_outcome(&pb(ROOT)))
    };
    wait_until(|| folders.started_count(ROOT) == 1);
    index.scan_folder_outcome(&pb(ROOT));
    gate.open();
    assert!(matches!(stale.join().unwrap(), MacScanOutcome::Scanned(_)));
    assert_eq!(names(&index.cached_directory(&pb(ROOT))), ["back.mp4"]);
}

#[test]
fn a_stale_listing_does_not_bring_back_a_folder_a_fresher_scan_found_missing() {
    let folders = Arc::new(ScriptedFolders::default());
    let gate = Gate::new();
    let sub = format!("{ROOT}/sub");
    folders.script(ROOT, vec![step(vlisted(ROOT, &[&sub], &[]))]);
    folders.script(&sub, vec![gated(vlisted(&sub, &[], &["x.mp4"]), &gate), step(ScanResult::Missing)]);
    let (index, _d) = overlap_index(&folders);
    index.scan_folder_outcome(&pb(ROOT));
    let stale = {
        let index = index.clone();
        let sub = sub.clone();
        std::thread::spawn(move || index.scan_folder_outcome(&pb(&sub)))
    };
    wait_until(|| folders.started_count(&sub) == 1);
    assert!(matches!(index.scan_folder_outcome(&pb(&sub)), MacScanOutcome::Missing(_)));
    gate.open();
    assert!(matches!(stale.join().unwrap(), MacScanOutcome::Missing(_)), "the stale scan reports the fresher state");
    assert!(index.cached_snapshot(&pb(&sub)).is_none());
    assert!(index.cached_subdirectories(&pb(ROOT)).is_empty());
}

#[test]
fn scans_of_different_folders_never_supersede_each_other() {
    let folders = Arc::new(ScriptedFolders::default());
    let gate = Gate::new();
    let other = "/virtual/other";
    folders.script(ROOT, vec![gated(vlisted(ROOT, &[], &["a.mp4"]), &gate)]);
    folders.script(other, vec![step(vlisted(other, &[], &["b.mp4"]))]);
    let (index, _d) = overlap_index(&folders);
    let slow = {
        let index = index.clone();
        std::thread::spawn(move || index.scan_folder_outcome(&pb(ROOT)))
    };
    wait_until(|| folders.started_count(ROOT) == 1);
    index.scan_folder_outcome(&pb(other));
    gate.open();
    slow.join().unwrap();
    assert_eq!(index.total_indexed_files(), 2);
}

// ---- MediaIndexPersistenceTests -------------------------------------------------------------------------------------

#[test]
fn init_does_no_file_work() {
    let b = Sandbox::new();
    let folder = b.base.path().join("not").join("yet").join("created");
    let file = folder.join("media-index.json");
    let index = b.index_at(file.clone());
    assert_eq!(index.file(), file);
    assert!(!folder.exists());
    assert_eq!(index.total_indexed_files(), 0);
    assert!(index.search("x", 100).is_empty());
    assert!(!folder.exists(), "reads never write");
    b.file("a.mp4");
    index.scan_folder_outcome(&pb(&b.root));
    assert!(file.exists());
}

#[test]
fn default_file_is_media_index_json_in_the_app_data_folder() {
    let f = MacMediaIndex::default_file();
    assert_eq!(f.file_name().unwrap(), "media-index.json");
    let parent = f.parent().unwrap();
    assert_eq!(parent.file_name().unwrap(), "SubtitleAdder");
    assert_eq!(parent.parent().unwrap().file_name().unwrap(), "Application Support");
}

#[test]
fn file_follows_the_library_kit_schema() {
    let b = Sandbox::new();
    let folder = b.folder("Shows");
    b.file_sized("Shows/b.mp4", 10, unix_seconds_ticks(1_700_000_000) + 1_234_567);
    b.file("Shows/b.srt");
    b.file_sized("Shows/a.mkv", 20, 0);
    b.folder("Shows/Zed");
    b.folder("Shows/Alpha");
    b.index().scan_folder_outcome(&folder);
    let text = std::fs::read_to_string(b.index_file()).unwrap();
    assert!(!text.contains("\\/"), "slashes are not escaped");
    assert!(!text.contains('\n'), "compact");
    let json: Value = serde_json::from_str(&text).unwrap();
    let obj = json.as_object().unwrap();
    assert_eq!(obj.keys().collect::<Vec<_>>(), ["directories", "updatedAtUtc", "version"], "sorted keys");
    assert_eq!(obj["version"], 1);
    let is_iso = |v: &Value| v.as_str().is_some_and(|s| s.ends_with('Z') && s.len() == 24 && dates::parse_apple(s).is_some());
    assert!(is_iso(&obj["updatedAtUtc"]));
    let dirs = obj["directories"].as_array().unwrap();
    assert_eq!(dirs.len(), 1);
    let d = dirs[0].as_object().unwrap();
    assert_eq!(d.keys().collect::<Vec<_>>(), ["directoryPath", "mediaFiles", "scannedAtUtc", "subdirectories"]);
    assert_eq!(d["directoryPath"], b.path("Shows").as_str());
    assert_eq!(d["subdirectories"], serde_json::json!([b.path("Shows/Alpha"), b.path("Shows/Zed")]));
    assert!(is_iso(&d["scannedAtUtc"]));
    let files = d["mediaFiles"].as_array().unwrap();
    assert_eq!(files.iter().map(|f| f["fileName"].as_str().unwrap()).collect::<Vec<_>>(), ["a.mkv", "b.mp4"]);
    let keys = ["directoryPath", "fileName", "fullPath", "hasSidecarSubtitle", "indexedAtUtc", "lastWriteTimeUtc", "sidecarExtension", "sizeBytes"];
    for f in files {
        assert_eq!(f.as_object().unwrap().keys().collect::<Vec<_>>(), keys);
    }
    assert_eq!(files[0]["fullPath"], b.path("Shows/a.mkv").as_str());
    assert_eq!(files[0]["sizeBytes"], 20);
    assert_eq!(files[0]["hasSidecarSubtitle"], false);
    assert!(files[0]["sidecarExtension"].is_null(), "explicit null");
    assert_eq!(files[1]["hasSidecarSubtitle"], true);
    assert_eq!(files[1]["sidecarExtension"], ".srt");
    assert_eq!(files[1]["lastWriteTimeUtc"], "2023-11-14T22:13:20.123Z", "milliseconds, truncated");
    assert!(is_iso(&files[1]["indexedAtUtc"]));
}

#[test]
fn reads_a_file_the_windows_app_wrote() {
    let b = Sandbox::new();
    let json = r#"{
      "version": 1,
      "updatedAtUtc": "2026-01-02T03:04:05.1234567+00:00",
      "directories": [
        {
          "directoryPath": "/Volumes/Movies/",
          "subdirectories": ["/Volumes/Movies/b", "/Volumes/Movies/A/", ""],
          "mediaFiles": [
            {
              "fullPath": "/Volumes/Movies/zeta.mp4", "fileName": "zeta.mp4", "directoryPath": "/Volumes/Movies",
              "sizeBytes": 5000000000, "lastWriteTimeUtc": "2026-01-02T03:04:05.1234567+00:00",
              "hasSidecarSubtitle": true, "sidecarExtension": ".srt", "indexedAtUtc": "2026-01-02T03:04:05Z"
            },
            {
              "fullPath": "/Volumes/Movies/alpha.mkv", "fileName": "alpha.mkv", "directoryPath": "/Volumes/Movies",
              "sizeBytes": 1, "lastWriteTimeUtc": "2026-01-02T03:04:05+03:00",
              "hasSidecarSubtitle": false, "sidecarExtension": null, "indexedAtUtc": "2026-01-02T03:04:05.5Z"
            }
          ],
          "scannedAtUtc": "2026-01-02T03:04:05.1234567+00:00"
        },
        { "directoryPath": "", "subdirectories": [], "mediaFiles": [], "scannedAtUtc": "2026-01-02T03:04:05Z" }
      ]
    }"#;
    std::fs::write(b.index_file(), json).unwrap();
    let index = b.index();
    let movies = pb("/Volumes/movies");
    let files = index.cached_directory(&movies);
    assert_eq!(names(&files), ["alpha.mkv", "zeta.mp4"]);
    assert_eq!(files[1].size_bytes, 5_000_000_000);
    assert_eq!(files[1].sidecar_extension.as_deref(), Some(".srt"));
    assert_eq!(files[0].sidecar_extension, None);
    assert_eq!(index.cached_subdirectories(&movies), ["/Volumes/Movies/A", "/Volumes/Movies/b"]);
    assert_eq!(index.total_indexed_files(), 2);
    assert_eq!(index.indexed_directories(), ["/Volumes/Movies"]);
    let base = dates::parse_apple("2026-01-02T03:04:05Z").unwrap();
    assert_eq!(files[1].last_write_time_utc - base, 1_234_567);
    assert_eq!(files[0].last_write_time_utc, base - 3 * 3600 * dates::TICKS_PER_SECOND);
    assert_eq!(names(&index.all_files(usize::MAX)), ["zeta.mp4", "alpha.mkv"]);
}

#[test]
fn unusable_files_start_an_empty_index_that_the_next_scan_rewrites() {
    let b = Sandbox::new();
    let bad = [
        "not json at all",
        "",
        "{\"version\": 1}",
        "[]",
        "{\"version\": 1, \"updatedAtUtc\": \"yesterday\", \"directories\": []}",
        "{\"version\": 2, \"updatedAtUtc\": \"2026-01-02T03:04:05Z\", \"directories\": [{\"directoryPath\": \"/x\", \
         \"subdirectories\": [], \"mediaFiles\": [], \"scannedAtUtc\": \"2026-01-02T03:04:05Z\"}]}",
    ];
    for contents in bad {
        std::fs::write(b.index_file(), contents).unwrap();
        let index = b.index();
        assert!(index.indexed_directories().is_empty(), "{contents}");
        assert_eq!(index.total_indexed_files(), 0);
    }
    b.file("a.mp4");
    b.index().scan_folder_outcome(&pb(&b.root));
    assert_eq!(b.index().total_indexed_files(), 1);
}

#[test]
fn interleaved_scans_of_different_folders_all_end_up_in_the_file() {
    let b = Sandbox::new();
    let names: Vec<String> = (1..=8).map(|i| format!("Folder{i}")).collect();
    for n in &names {
        b.file(&format!("{n}/clip.mp4"));
    }
    let index = b.index();
    std::thread::scope(|s| {
        for n in &names {
            let (index, p) = (index.clone(), pb(&b.p(n)));
            s.spawn(move || index.scan_folder_outcome(&p));
        }
    });
    assert_eq!(index.total_indexed_files(), 8);
    assert_eq!(b.index().total_indexed_files(), 8);
}

#[test]
fn changes_yield_after_the_write_and_reach_every_subscriber() {
    let b = Sandbox::new();
    b.file("a.mp4");
    let index = b.index();
    let first = index.changes();
    let second = index.changes();
    let on_disk = Arc::new(Mutex::new(Vec::new()));
    let (seen, file) = (on_disk.clone(), b.index_file());
    let _sub = index.subscribe_changes(Box::new(move || seen.lock().push(file.exists())));
    index.scan_folder_outcome(&pb(&b.root));
    assert!(first.try_recv().is_ok());
    assert!(second.try_recv().is_ok());
    assert_eq!(*on_disk.lock(), [true], "the subscriber hears of the change once it is on disk");
    assert_eq!(b.index().total_indexed_files(), 1);
    b.file("b.mp4");
    crawl(&index, &pb(&b.root), 2);
    assert!(first.try_recv().is_ok());
    assert_eq!(b.index().total_indexed_files(), 2);
}

#[test]
fn many_changes_while_busy_coalesce() {
    let b = Sandbox::new();
    b.file("a.mp4");
    let index = b.index();
    let changes = index.changes();
    for _ in 0..5 {
        index.scan_folder_outcome(&pb(&b.root));
    }
    assert!(changes.try_recv().is_ok());
    assert!(changes.try_recv().is_err(), "only one event was kept");
    b.file("b.mp4");
    index.scan_folder_outcome(&pb(&b.root));
    assert!(changes.try_recv().is_ok());
    assert_eq!(b.index().total_indexed_files(), 2);
}

#[test]
fn changes_end_when_the_index_is_gone() {
    let b = Sandbox::new();
    let changes = b.index().changes();
    assert!(matches!(changes.recv_timeout(Duration::from_secs(1)), Err(crossbeam_channel::RecvTimeoutError::Disconnected)));
}

#[test]
fn trait_scan_maps_outcomes_and_cancellation() {
    let b = Sandbox::new();
    b.file("Show/a.mp4");
    let index = b.index();
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert_eq!(index.scan(&pb(&b.p("Show")), &cancelled), ScanResult::Cancelled);
    let ScanResult::Listed(snap) = index.scan(&pb(&b.p("Show")), &CancellationToken::new()) else { panic!() };
    assert_eq!(names(&snap.media_files), ["a.mp4"]);
    assert_eq!(index.crawl(&pb(&b.root), 2, 64, &CancellationToken::new(), &|_| {}), 2);
}

/// The real lister (macOS only): `UF_HIDDEN`, dot names, packages, symbolic links (file, folder, dangling), NFD names
/// and the `.tr.srt` badge, against a temporary folder. Runs on a Mac (`cargo test -p sa-media`).
#[cfg(target_os = "macos")]
#[test]
fn the_real_scanner_follows_the_library_kit_rules() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let w = |rel: &str, bytes: &[u8]| {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, bytes).unwrap();
    };
    w("Film.mp4", &[0u8; 1500]);
    w("Film.tr.srt", b"1\n");
    w(".secret.mp4", b"x");
    w("._Film.mp4", b"x");
    w("flagged.mp4", b"x");
    w("Tool.app/Contents/Info.plist", b"<plist/>");
    w("Pictures.photoslibrary/database/x", b"x");
    w(".git/HEAD", b"ref");
    w("Shows/a.mkv", b"x");
    let nfd: String = unicode_normalization::UnicodeNormalization::nfd("Çalışma.mov").collect();
    w(&nfd, b"x");
    std::os::unix::fs::symlink(root.join("Film.mp4"), root.join("link.mp4")).unwrap();
    std::os::unix::fs::symlink(root.join("Shows"), root.join("Linked Shows")).unwrap();
    std::os::unix::fs::symlink(root.join("missing.mp4"), root.join("dangling.mp4")).unwrap();
    let flagged = CString::new(root.join("flagged.mp4").as_os_str().as_bytes()).unwrap();
    // SAFETY: a valid path; UF_HIDDEN is a user flag the owner may set.
    assert_eq!(unsafe { libc::chflags(flagged.as_ptr(), 0x8000) }, 0);

    let base = super::path::normalized(root.to_str().unwrap());
    let ScanResult::Listed(snap) = super::scanner::list(&base, 0) else { panic!("listed") };
    assert_eq!(names(&snap.media_files), ["Film.mp4", "link.mp4", "Çalışma.mov"], "hidden, dot, dangling skipped; NFC");
    assert_eq!(snap.media_files[0].sidecar_extension.as_deref(), Some(".srt"));
    assert_eq!(snap.media_files[1].size_bytes, 1500, "a linked file counts with its target's size");
    assert_eq!(snap.subdirectories, [format!("{base}/Linked Shows"), format!("{base}/Shows")], "packages and .git skipped");
    assert_eq!(super::scanner::list(&format!("{base}/nope"), 0), ScanResult::Missing);
    assert_eq!(super::scanner::list(&format!("{base}/Film.mp4"), 0), ScanResult::Missing, "a file is not a folder");
}
