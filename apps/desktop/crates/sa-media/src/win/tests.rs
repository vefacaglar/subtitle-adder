//! Ports of the C# `MediaLibraryIndexTests`, `PlaybackTimelineTests` (buffered stream and cache parts) and
//! `WatchHistoryPrivacyTests` (video cache and media index parts), plus the Rust-only checks of win-media §5.4.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

use parking_lot::Mutex;
use sa_core::cancel::CancellationToken;
use sa_core::secrets::SecretKey32;
use sa_core::subscription::{Subscribers, Subscription};
use sa_sync::{StateSyncEvent, SyncBus, SyncHandler, Topic, TopicFilter};

use super::cache::{VaultKeyProvider, VideoCacheStore, fixed_file_time};
use super::fsio;
use super::index::WinMediaIndex;
use super::paths::normalize_directory_path;
use super::stream::{BufferedVideoStream, mime_type};
use crate::{MediaIndex, ScanResult};

const MIB: usize = 1024 * 1024;
const CHUNK: i32 = 64 * 1024;

fn write(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn listed(r: ScanResult) -> crate::DirectorySnapshot {
    match r {
        ScanResult::Listed(s) => s,
        other => panic!("expected a listing, got {other:?}"),
    }
}

fn names(files: &[crate::IndexedMediaFile]) -> Vec<String> {
    files.iter().map(|f| f.file_name.clone()).collect()
}

fn tok() -> CancellationToken {
    CancellationToken::new()
}

/// An in-process stand-in for the C# broker: every subscriber sees every published event.
#[derive(Default)]
struct FakeBus {
    subscribers: Subscribers<StateSyncEvent>,
    published: AtomicUsize,
}

impl SyncBus for FakeBus {
    fn publish(&self, topic: Topic, event: StateSyncEvent) {
        assert_eq!(topic, Topic::MediaIndex);
        self.published.fetch_add(1, Ordering::SeqCst);
        self.subscribers.notify(&event);
    }
    fn subscribe(&self, filter: TopicFilter, handler: SyncHandler) -> Subscription {
        assert_eq!(filter, TopicFilter::Only(Topic::MediaIndex));
        self.subscribers.subscribe(move |e| handler(e))
    }
    fn active_instance_count(&self) -> usize {
        1
    }
    fn has_other_active_instances(&self) -> bool {
        false
    }
    fn instance_id(&self) -> &str {
        "fakebus"
    }
    fn dispose(&self) {}
}

// ---- media index ---------------------------------------------------------------------------------------------------

struct IndexFixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    index_file: PathBuf,
}

impl IndexFixture {
    fn new() -> Self {
        let dir = tempfile::Builder::new().prefix("SubtitleAdderIndexTests_").tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let index_file = root.join("media-index.json");
        Self { _dir: dir, root, index_file }
    }
    fn index(&self) -> Arc<WinMediaIndex> {
        WinMediaIndex::new(Some(self.index_file.clone()), None)
    }
    fn p(&self, rel: &str) -> String {
        self.root.join(rel).to_string_lossy().into_owned()
    }
}

#[test]
fn scan_directory_populates_media_files_and_detects_sidecar_subtitles() {
    let fx = IndexFixture::new();
    std::fs::create_dir_all(fx.root.join("Videos/SubfolderA")).unwrap();
    write(&fx.root.join("Videos/episode01.mp4"), &[0u8; 2048]);
    write(&fx.root.join("Videos/episode01.srt"), b"1\n00:00:01,000 --> 00:00:02,000\nHello\n");
    write(&fx.root.join("Videos/movie.mkv"), &[0u8; 4096]);
    write(&fx.root.join("Videos/notes.txt"), b"ignore me");

    let index = fx.index();
    let snapshot = listed(index.scan_directory(&fx.p("Videos"), &tok()));
    assert_eq!(snapshot.subdirectories.len(), 1);
    assert!(snapshot.subdirectories[0].to_ascii_lowercase().ends_with("subfoldera"));
    assert_eq!(snapshot.media_files.len(), 2);
    assert_eq!(index.total_indexed_files(), 2);
    assert_eq!(snapshot.scanned_at_utc, 0, "no scan time is recorded");

    let ep1 = snapshot.media_files.iter().find(|f| f.file_name == "episode01.mp4").unwrap();
    assert!(ep1.has_sidecar_subtitle);
    assert_eq!(ep1.sidecar_extension.as_deref(), Some(".srt"));
    assert_eq!(ep1.sidecar_badge().as_deref(), Some("SRT"));
    assert_eq!(ep1.size_bytes, 2048);
    assert_eq!(ep1.full_path, fx.p("Videos\\episode01.mp4"));
    assert_eq!(ep1.directory_path, normalize_directory_path(&fx.p("Videos")));
    assert_eq!(ep1.indexed_at_utc, 0);

    let movie = snapshot.media_files.iter().find(|f| f.file_name == "movie.mkv").unwrap();
    assert!(!movie.has_sidecar_subtitle);
    assert_eq!(movie.sidecar_extension, None);
    assert_eq!(movie.sidecar_badge(), None);
    assert_eq!(movie.size_bytes, 4096);
}

#[test]
fn persistence_reloads_cached_entries_in_new_instance() {
    let fx = IndexFixture::new();
    write(&fx.root.join("Library/lecture.mp4"), &[0u8; 1024]);
    write(&fx.root.join("Library/lecture.vtt"), b"WEBVTT\n\n00:00.000 --> 00:01.000\nTest");
    fx.index().scan_directory(&fx.p("Library"), &tok());
    assert!(fx.index_file.exists());

    let index2 = fx.index();
    assert_eq!(index2.total_indexed_files(), 1);
    assert_eq!(index2.indexed_directories().len(), 1);
    let cached = index2.cached_directory(&fx.p("Library"));
    assert_eq!(cached.len(), 1);
    assert_eq!(cached[0].file_name, "lecture.mp4");
    assert!(cached[0].has_sidecar_subtitle);
    assert_eq!(cached[0].sidecar_extension.as_deref(), Some(".vtt"));
    // Case-insensitive, trailing-separator tolerant lookups.
    assert_eq!(index2.cached_directory(&fx.p("LIBRARY\\")).len(), 1);
    assert!(index2.cached_snapshot(Path::new(&fx.p("library"))).is_some());
}

#[test]
fn rescanning_directory_prunes_deleted_files() {
    let fx = IndexFixture::new();
    write(&fx.root.join("PruneTest/keep.mp4"), &[1, 2, 3]);
    write(&fx.root.join("PruneTest/remove.mkv"), &[4, 5, 6]);
    let index = fx.index();
    assert_eq!(listed(index.scan_directory(&fx.p("PruneTest"), &tok())).media_files.len(), 2);
    assert_eq!(index.total_indexed_files(), 2);
    std::fs::remove_file(fx.root.join("PruneTest/remove.mkv")).unwrap();
    let second = listed(index.scan_directory(&fx.p("PruneTest"), &tok()));
    assert_eq!(names(&second.media_files), ["keep.mp4"]);
    assert_eq!(index.total_indexed_files(), 1);
    assert!(index.search("remove", 100).is_empty());
}

#[test]
fn crawl_subtree_and_search_finds_files_across_nested_directories() {
    let fx = IndexFixture::new();
    write(&fx.root.join("Root/intro.mp4"), &[1, 2]);
    write(&fx.root.join("Root/Season1/s01e01_pilot.mkv"), &[1, 2, 3]);
    write(&fx.root.join("Root/Season2/Extras/behind_the_scenes.mov"), &[1, 2, 3, 4]);
    let index = fx.index();
    let progress = Mutex::new(Vec::new());
    let scanned = index.crawl(Path::new(&fx.p("Root")), 2, 64, &tok(), &|n| progress.lock().push(n));
    assert_eq!(scanned, 4);
    assert_eq!(*progress.lock(), [1, 2, 3, 4]);
    assert_eq!(index.total_indexed_files(), 3);
    assert_eq!(names(&index.search("pilot", 100)), ["s01e01_pilot.mkv"]);
    assert_eq!(names(&index.search("behind", 100)), ["behind_the_scenes.mov"]);
    assert_eq!(names(&index.search("Season1", 100)), ["s01e01_pilot.mkv"], "directory-path match");
    // Persisted once at the end.
    assert_eq!(fx.index().total_indexed_files(), 3);
}

#[test]
fn navigation_queries_serve_cached_folders_and_global_search() {
    // The index side of `FileExplorerViewModel_Navigation_Back_Up_AndSearch` (the view model test lives with the
    // explorer port).
    let fx = IndexFixture::new();
    write(&fx.root.join("Parent/parent_clip.mp4"), &[0u8; 512]);
    write(&fx.root.join("Parent/Child/child_movie.mkv"), &[0u8; 1024]);
    write(&fx.root.join("OtherIndexed/global_special_doc.mp4"), &[0u8; 2048]);
    let index = fx.index();
    index.scan_directory(&fx.p("OtherIndexed"), &tok());
    index.scan_directory(&fx.p("Parent"), &tok());
    index.crawl_subtree(&fx.p("Parent"), 2, 64, &tok(), &|_| {});
    assert_eq!(index.cached_subdirectories(&fx.p("Parent")), [normalize_directory_path(&fx.p("Parent\\Child"))]);
    assert_eq!(names(&index.cached_directory(&fx.p("Parent"))), ["parent_clip.mp4"]);
    assert_eq!(names(&index.cached_directory(&fx.p("Parent\\Child"))), ["child_movie.mkv"]);
    assert_eq!(index.total_indexed_files(), 3);
    assert_eq!(names(&index.search("global_special", 100)), ["global_special_doc.mp4"]);
}

#[test]
fn sync_to_video_path_refresh_updates_sidecar_status_in_memory_only() {
    let fx = IndexFixture::new();
    write(&fx.root.join("SyncFolder/interview.mp4"), &[0u8; 1024]);
    let index = fx.index();
    let before = listed(index.scan_directory(&fx.p("SyncFolder"), &tok()));
    assert!(!before.media_files[0].has_sidecar_subtitle);
    let written = std::fs::read_to_string(&fx.index_file).unwrap();

    write(&fx.root.join("SyncFolder/interview.srt"), b"1\n00:00:01,000 --> 00:00:02,000\nx\n");
    index.invalidate_sidecar(&fx.root.join("SyncFolder/interview.mp4"));
    let after = index.cached_directory(&fx.p("SyncFolder"));
    assert!(after[0].has_sidecar_subtitle);
    assert_eq!(after[0].sidecar_extension.as_deref(), Some(".srt"));
    assert_eq!(std::fs::read_to_string(&fx.index_file).unwrap(), written, "the badge refresh never rewrites the file");
}

#[test]
fn opening_a_video_in_an_indexed_folder_never_rewrites_the_index_file() {
    let fx = IndexFixture::new();
    write(&fx.root.join("Browsed/episode.mp4"), &[0u8; 512]);
    let index = fx.index();
    index.scan_directory(&fx.p("Browsed"), &tok());
    assert!(index.is_directory_indexed(&fx.p("Browsed")));
    let before = std::fs::read_to_string(&fx.index_file).unwrap();
    assert!(!before.to_ascii_lowercase().contains("scannedatutc"));
    assert!(!before.to_ascii_lowercase().contains("updatedatutc"));
    let stamp = fixed_file_time();
    let set_stamp = || std::fs::File::options().write(true).open(&fx.index_file).unwrap().set_modified(stamp).unwrap();
    set_stamp();

    // A rescan with an unchanged listing writes nothing, a transient listing and an in-memory refresh neither.
    index.scan_directory(&fx.p("Browsed"), &tok());
    index.scan_transient(&fx.p("Browsed"), &tok());
    index.refresh_in_memory(&fx.p("Browsed"), &tok());
    index.invalidate_sidecar(&fx.root.join("Browsed/episode.mp4"));
    assert_eq!(std::fs::metadata(&fx.index_file).unwrap().modified().unwrap(), stamp);
    assert_eq!(std::fs::read_to_string(&fx.index_file).unwrap(), before);

    write(&fx.root.join("Browsed/episode.srt"), b"x");
    index.invalidate_sidecar(&fx.root.join("Browsed/episode.mp4"));
    assert!(index.cached_directory(&fx.p("Browsed"))[0].has_sidecar_subtitle);
    assert_eq!(std::fs::metadata(&fx.index_file).unwrap().modified().unwrap(), stamp);
}

#[test]
fn transient_scans_never_add_a_folder() {
    let fx = IndexFixture::new();
    write(&fx.root.join("Private/clip_secret.mp4"), &[0u8; 512]);
    let index = fx.index();
    let snap = listed(index.scan_transient(&fx.p("Private"), &tok()));
    assert_eq!(names(&snap.media_files), ["clip_secret.mp4"]);
    assert!(!index.is_directory_indexed(&fx.p("Private")));
    assert_eq!(index.total_indexed_files(), 0);
    assert!(!fx.index_file.exists());
    // An invalidation for a folder that is not indexed does not index it either.
    index.invalidate_sidecar(&fx.root.join("Private/clip_secret.mp4"));
    assert!(!index.is_directory_indexed(&fx.p("Private")));
}

#[test]
fn legacy_media_index_is_discarded_once() {
    let fx = IndexFixture::new();
    std::fs::write(
        &fx.index_file,
        r#"{
          "version": 1,
          "updatedAtUtc": "2025-05-05T10:00:00+00:00",
          "directories": [
            {
              "directoryPath": "D:\\Private",
              "subdirectories": [],
              "mediaFiles": [
                { "fullPath": "D:\\Private\\a.mp4", "fileName": "a.mp4", "directoryPath": "D:\\Private", "sizeBytes": 1,
                  "lastWriteTimeUtc": "2025-05-05T10:00:00+00:00", "hasSidecarSubtitle": false, "sidecarExtension": null,
                  "indexedAtUtc": "2025-05-05T10:00:00+00:00" }
              ],
              "scannedAtUtc": "2025-05-05T10:00:00+00:00"
            }
          ]
        }"#,
    )
    .unwrap();
    let index = fx.index();
    assert_eq!(index.total_indexed_files(), 0);
    let text = std::fs::read_to_string(&fx.index_file).unwrap();
    assert!(!text.contains("Private"));
    assert!(!text.contains("2025-05-05"));
    assert_eq!(text, "{\r\n  \"version\": 2,\r\n  \"directories\": []\r\n}", "the C# bytes: CRLF, no final newline");
    assert_eq!(fx.index().total_indexed_files(), 0);
}

#[test]
fn corrupt_index_starts_empty_and_is_replaced_by_the_next_change() {
    let fx = IndexFixture::new();
    std::fs::write(&fx.index_file, "{ this is not json").unwrap();
    let index = fx.index();
    assert_eq!(index.total_indexed_files(), 0);
    assert_eq!(std::fs::read_to_string(&fx.index_file).unwrap(), "{ this is not json", "a read never writes");
    write(&fx.root.join("New/a.mp4"), b"x");
    index.scan_directory(&fx.p("New"), &tok());
    assert_eq!(fx.index().total_indexed_files(), 1);
}

#[test]
fn reads_a_csharp_written_file_with_escapes_and_missing_properties() {
    let fx = IndexFixture::new();
    let dir = fx.p("Vidéo");
    let json = format!(
        "{{\n  \"version\": 2,\n  \"directories\": [\n    {{\n      \"directoryPath\": {d},\n      \"subdirectories\": [ {s}, null, \"\" ],\n      \
         \"mediaFiles\": [\n        {{ \"fullPath\": {f}, \"fileName\": \"b\\u002Bc.mp4\", \"directoryPath\": {d}, \"sizeBytes\": 7,\n          \
         \"lastWriteTimeUtc\": \"2026-11-01T08:12:44.1234567+00:00\", \"hasSidecarSubtitle\": false, \"sidecarExtension\": null, \
         \"indexedAtUtc\": \"0001-01-01T00:00:00+00:00\" }},\n        {{ \"fullPath\": {g}, \"fileName\": \"a.mkv\" }},\n        {{ \"fileName\": \"nopath.mp4\" }}\n      ]\n    }}\n  ]\n}}",
        d = serde_json::to_string(&format!("{dir}\\")).unwrap().replace('é', "\\u00E9"),
        s = serde_json::to_string(&format!("{dir}\\Sub\\")).unwrap(),
        f = serde_json::to_string(&format!("{dir}\\b+c.mp4")).unwrap().replace('+', "\\u002B"),
        g = serde_json::to_string(&format!("{dir}\\a.mkv")).unwrap(),
    );
    std::fs::write(&fx.index_file, json).unwrap();
    let index = fx.index();
    assert_eq!(index.total_indexed_files(), 2);
    let files = index.cached_directory(&dir);
    assert_eq!(names(&files), ["a.mkv", "b+c.mp4"], "sorted by name, entries without a path dropped");
    assert_eq!(index.cached_subdirectories(&dir), [normalize_directory_path(&format!("{dir}\\Sub"))]);
    assert_eq!(crate::dates::format_dotnet(files[1].last_write_time_utc), "2026-11-01T08:12:44.1234567+00:00");
    assert_eq!(index.indexed_directories(), [normalize_directory_path(&dir)]);
}

#[test]
fn missing_folder_on_a_connected_drive_is_removed_but_an_unplugged_drive_keeps_its_cache() {
    let fx = IndexFixture::new();
    write(&fx.root.join("Gone/a.mp4"), b"x");
    let index = fx.index();
    index.scan_directory(&fx.p("Gone"), &tok());
    std::fs::remove_dir_all(fx.root.join("Gone")).unwrap();
    assert_eq!(index.scan_directory(&fx.p("Gone"), &tok()), ScanResult::Missing);
    assert!(!index.is_directory_indexed(&fx.p("Gone")));
    assert_eq!(fx.index().total_indexed_files(), 0);

    // A drive letter that is not mounted: its entries stay indexed but are hidden from search.
    let Some(letter) = (b'D'..=b'Z').rev().map(char::from).find(|l| !Path::new(&format!("{l}:\\")).exists()) else { return };
    let json = format!(
        "{{\"version\":2,\"directories\":[{{\"directoryPath\":\"{l}:\\\\Movies\",\"subdirectories\":[],\"mediaFiles\":[\
         {{\"fullPath\":\"{l}:\\\\Movies\\\\offline.mp4\",\"fileName\":\"offline.mp4\",\"directoryPath\":\"{l}:\\\\Movies\",\
         \"sizeBytes\":1,\"lastWriteTimeUtc\":\"2026-01-01T00:00:00+00:00\",\"hasSidecarSubtitle\":false,\
         \"sidecarExtension\":null,\"indexedAtUtc\":\"0001-01-01T00:00:00+00:00\"}}]}}]}}",
        l = letter
    );
    std::fs::write(&fx.index_file, json).unwrap();
    let index = fx.index();
    assert_eq!(index.total_indexed_files(), 1);
    assert!(index.search("offline", 100).is_empty());
    assert!(index.search("", 100).is_empty());
    assert_eq!(index.scan_directory(&format!("{letter}:\\Movies"), &tok()), ScanResult::Missing);
    assert_eq!(index.total_indexed_files(), 1, "an unplugged drive keeps its cached index");
}

#[test]
fn hidden_system_and_dot_folders_are_skipped_but_dot_files_are_not() {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_HIDDEN, SetFileAttributesW};
    let fx = IndexFixture::new();
    write(&fx.root.join("Mixed/visible.mp4"), b"x");
    write(&fx.root.join("Mixed/.dotfile.mp4"), b"x");
    write(&fx.root.join("Mixed/hidden.mp4"), b"x");
    std::fs::create_dir_all(fx.root.join("Mixed/.git")).unwrap();
    std::fs::create_dir_all(fx.root.join("Mixed/Shows")).unwrap();
    std::fs::create_dir_all(fx.root.join("Mixed/HiddenDir")).unwrap();
    for p in ["Mixed/hidden.mp4", "Mixed/HiddenDir"] {
        let wide: Vec<u16> = fx.root.join(p).as_os_str().encode_wide().chain([0]).collect();
        // SAFETY: NUL-terminated wide path.
        unsafe { SetFileAttributesW(windows::core::PCWSTR(wide.as_ptr()), FILE_ATTRIBUTE_HIDDEN).unwrap() };
    }
    let snap = listed(fx.index().scan_directory(&fx.p("Mixed"), &tok()));
    assert_eq!(names(&snap.media_files), [".dotfile.mp4", "visible.mp4"]);
    assert_eq!(snap.subdirectories, [normalize_directory_path(&fx.p("Mixed\\Shows"))]);
}

#[test]
fn search_ranks_prefix_then_name_then_folder_then_newest() {
    let fx = IndexFixture::new();
    let at = |secs: u64| SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
    let mk = |rel: &str, t: SystemTime| {
        write(&fx.root.join(rel), b"x");
        std::fs::File::options().write(true).open(fx.root.join(rel)).unwrap().set_modified(t).unwrap();
    };
    mk("Season Alpha/beta.mp4", at(3_000_000));
    mk("misc/the alpha.mp4", at(1_000_000));
    mk("misc/alpha one.mp4", at(1_000_000));
    mk("misc/Alpha b.mp4", at(2_000_000));
    mk("misc/alpha a.mp4", at(2_000_000));
    mk("misc/unrelated.mp4", at(3_000_000));
    let index = fx.index();
    index.crawl_subtree(&fx.p(""), 2, 64, &tok(), &|_| {});
    assert_eq!(names(&index.search("  alpha ", 100)), ["alpha a.mp4", "Alpha b.mp4", "alpha one.mp4", "the alpha.mp4", "beta.mp4"]);
    assert_eq!(names(&index.search("ALPHA", 2)), ["alpha a.mp4", "Alpha b.mp4"]);
    assert!(index.search("alpha", 0).is_empty());
    // A blank query lists everything newest first, then by name (the hub's discovered paths on Windows).
    assert_eq!(names(&index.all_files(3)), ["beta.mp4", "unrelated.mp4", "alpha a.mp4"]);
}

#[test]
fn crawl_limits_depth_and_folder_count() {
    let fx = IndexFixture::new();
    let mut rel = String::from("Deep");
    write(&fx.root.join("Deep/a.mp4"), b"x");
    for i in 1..=7 {
        rel.push_str(&format!("\\L{i}"));
        write(&fx.root.join(&rel).join("f.mp4"), b"x");
    }
    let index = fx.index();
    assert_eq!(index.crawl_subtree(&fx.p("Deep"), 0, 64, &tok(), &|_| {}), 1);
    assert_eq!(index.crawl_subtree(&fx.p("Deep"), 99, 64, &tok(), &|_| {}), 6, "depth clamped to 5");

    let wide = IndexFixture::new();
    for i in 0..80 {
        write(&wide.root.join(format!("W/d{i:02}/x.mp4")), b"x");
    }
    let index = wide.index();
    assert_eq!(index.crawl_subtree(&wide.p("W"), 2, CRAWL_MAX, &tok(), &|_| {}), CRAWL_MAX, "root included");
    assert_eq!(index.indexed_directories().len(), CRAWL_MAX);
}

const CRAWL_MAX: usize = super::index::CRAWL_MAX_DIRECTORIES;

#[test]
fn cancelled_scan_and_crawl_write_nothing() {
    let fx = IndexFixture::new();
    write(&fx.root.join("C/a.mp4"), b"x");
    write(&fx.root.join("C/S/b.mp4"), b"x");
    let index = fx.index();
    let cancelled = tok();
    cancelled.cancel();
    assert_eq!(index.scan_directory(&fx.p("C"), &cancelled), ScanResult::Cancelled);
    let cancel = tok();
    let c2 = cancel.clone();
    let n = index.crawl_subtree(&fx.p("C"), 2, 64, &cancel, &move |_| c2.cancel());
    assert_eq!(n, 1);
    assert_eq!(index.total_indexed_files(), 1, "scanned folders stay updated in memory");
    assert!(!fx.index_file.exists(), "but nothing is saved");
}

#[test]
fn broker_events_reload_other_instances_and_ignore_own_and_foreign_scopes() {
    let fx = IndexFixture::new();
    write(&fx.root.join("Shared/a.mp4"), b"x");
    let bus = Arc::new(FakeBus::default());
    let a = WinMediaIndex::new(Some(fx.index_file.clone()), Some(bus.clone()));
    let b = WinMediaIndex::new(Some(fx.index_file.clone()), Some(bus.clone()));
    let other = WinMediaIndex::new(Some(fx.root.join("other-index.json")), Some(bus.clone()));
    let b_changes = Arc::new(AtomicUsize::new(0));
    let counter = b_changes.clone();
    let _sub = b.subscribe_changes(Box::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    }));
    let a_changes = a.changes();

    a.scan_directory(&fx.p("Shared"), &tok());
    assert_eq!(bus.published.load(Ordering::SeqCst), 1);
    assert_eq!(b.total_indexed_files(), 1, "b reloaded from disk");
    assert_eq!(b_changes.load(Ordering::SeqCst), 1);
    assert_eq!(other.total_indexed_files(), 0, "another file's instance ignores the event");
    assert!(a_changes.try_recv().is_ok());
    assert!(a_changes.try_recv().is_err(), "a's own event does not reload or notify it again");

    // Scope comparison is case-insensitive.
    let mut ev = StateSyncEvent::new(Topic::MediaIndex, sa_sync::EventKind::MediaIndexed, "someone", &fx.index_file.to_string_lossy().to_uppercase());
    ev.username = None;
    bus.publish(Topic::MediaIndex, ev);
    assert_eq!(b_changes.load(Ordering::SeqCst), 2);
}

#[test]
fn multi_instance_concurrent_scans_and_searches_never_corrupt_the_file() {
    let fx = IndexFixture::new();
    let a = fx.index();
    let b = fx.index();
    let root = fx.p("Concurrent");
    std::fs::create_dir_all(&root).unwrap();
    std::thread::scope(|s| {
        for i in 0..12 {
            let (a, b, root) = (a.clone(), b.clone(), root.clone());
            s.spawn(move || {
                std::fs::write(Path::new(&root).join(format!("v{i}.mp4")), b"x").unwrap();
                a.scan_directory(&root, &CancellationToken::new());
                let _ = b.search("v", 100);
                b.scan_directory(&root, &CancellationToken::new());
            });
        }
    });
    let text = fsio::read_all_text_shared(&fx.index_file, 5).unwrap();
    assert!(crate::MediaIndexDocument::parse(&text).is_some(), "never a torn file");
    // Without a broker the two instances do not see each other's writes, and a listing taken before the last file
    // appeared may be saved last (as in the C# app); the next scan of a fresh instance repairs the file.
    let healed = fx.index();
    healed.scan_directory(&root, &CancellationToken::new());
    assert_eq!(fx.index().total_indexed_files(), 12);
}

// ---- encrypted video cache -----------------------------------------------------------------------------------------

/// Stand-in for `LocalProfileManager.TryGetActiveProfileVaultKey`.
#[derive(Default)]
struct FakeProfiles {
    active: Mutex<Option<(String, [u8; 32])>>,
}

impl FakeProfiles {
    fn login(&self, user: &str, seed: u8) {
        *self.active.lock() = Some((user.into(), std::array::from_fn(|i| seed.wrapping_add(i as u8))));
    }
    fn logout(&self) {
        *self.active.lock() = None;
    }
    fn provider(self: &Arc<Self>) -> VaultKeyProvider {
        let me = self.clone();
        Arc::new(move || me.active.lock().as_ref().map(|(u, k)| (u.clone(), SecretKey32::new(*k))))
    }
}

struct CacheFixture {
    dir: tempfile::TempDir,
    profiles: Arc<FakeProfiles>,
}

impl CacheFixture {
    fn new() -> Self {
        Self { dir: tempfile::Builder::new().prefix("SubtitleAdderWatchPrivacy_").tempdir().unwrap(), profiles: Arc::default() }
    }
    fn cache_dir(&self) -> PathBuf {
        self.dir.path().join("VideoCache")
    }
    fn store(&self) -> Arc<VideoCacheStore> {
        let store = VideoCacheStore::new(self.cache_dir(), self.profiles.provider(), Some(1 << 30));
        // Let the background legacy purge of an empty folder finish first (it never touches `v3`).
        store.purge_legacy_layout();
        store
    }
    fn video(&self, name: &str, len: usize, mul: usize, add: usize) -> (String, Vec<u8>) {
        let raw: Vec<u8> = (0..len).map(|i| ((i * mul + add) & 0xFF) as u8).collect();
        let path = self.dir.path().join(name);
        std::fs::write(&path, &raw).unwrap();
        (path.to_string_lossy().into_owned(), raw)
    }
    fn vcache_files(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        fsio::walk_files(&self.cache_dir(), Some(".vcache"), &mut out);
        out.into_iter().map(|(p, _)| p).collect()
    }
}

fn memory_stream(raw: &[u8], max_cached: i32, store: Option<Arc<VideoCacheStore>>, path: Option<&str>) -> BufferedVideoStream {
    BufferedVideoStream::from_stream(Box::new(std::io::Cursor::new(raw.to_vec())), false, CHUNK, max_cached, 0, store, None, path.map(str::to_owned))
        .unwrap()
}

fn play_through(raw: &[u8], store: &Arc<VideoCacheStore>, path: &str) {
    let mut stream = memory_stream(raw, 2, Some(store.clone()), Some(path));
    let mut buf = vec![0u8; raw.len()];
    let mut read = 0;
    while read < raw.len() {
        let n = std::io::Read::read(&mut stream, &mut buf[read..(read + 16 * 1024).min(raw.len())]).unwrap();
        assert!(n > 0);
        read += n;
    }
    assert_eq!(buf, raw);
}

#[test]
fn video_identity_matches_the_csharp_vectors() {
    assert_eq!(
        VideoCacheStore::compute_video_identity(Some("C:\\Videos\\episode_01.mp4"), 262_144, 65_536),
        "C:\\VIDEOS\\EPISODE_01.MP4|len:262144|cs:65536"
    );
    assert_eq!(VideoCacheStore::compute_video_identity(Some(" \"c:/videos/ı.mp4\" "), 1, 2), "C:\\VIDEOS\\ı.MP4|len:1|cs:2");
    assert_eq!(VideoCacheStore::compute_video_identity(None, 10, 64), "stream|len:10|cs:64");
    assert_eq!(VideoCacheStore::compute_video_identity(Some("  "), 10, 64), "stream|len:10|cs:64");
}

#[test]
fn logged_out_playback_writes_nothing_to_the_video_cache() {
    let fx = CacheFixture::new();
    let (path, raw) = fx.video("logged_out_secret.mp4", 4 * CHUNK as usize, 73, 19);
    let store = fx.store();
    assert!(store.try_begin_session().is_none());
    play_through(&raw, &store, &path);
    let id = VideoCacheStore::compute_video_identity(Some(&path), raw.len() as i64, CHUNK);
    assert!(!store.try_write_chunk(&id, CHUNK, 0, &raw[..CHUNK as usize]));
    assert!(store.cached_chunk_indices(&id, 4, CHUNK, raw.len() as i64).is_empty());
    let entries: Vec<_> = std::fs::read_dir(fx.cache_dir()).map(|d| d.flatten().collect()).unwrap_or_default();
    assert!(entries.is_empty(), "nothing at all under VideoCache");
}

#[test]
fn logged_in_cache_uses_keyed_names_fixed_times_and_padding_in_one_flat_folder() {
    let fx = CacheFixture::new();
    let (path, raw) = fx.video("odd_length.mkv", 3 * CHUNK as usize + 1234, 31, 7);
    fx.profiles.login("bob", 1);
    let store = fx.store();
    play_through(&raw, &store, &path);
    fx.profiles.login("alice", 2);
    play_through(&raw, &store, &path);
    play_through(&raw, &store, &path);

    let layout = fx.cache_dir().join(VideoCacheStore::LAYOUT_FOLDER);
    assert_eq!(std::fs::read_dir(&layout).unwrap().flatten().filter(|e| e.file_type().unwrap().is_dir()).count(), 0);
    let files = fx.vcache_files();
    assert_eq!(files.len(), 8);
    for f in &files {
        let meta = std::fs::metadata(f).unwrap();
        assert_eq!(meta.len(), (VideoCacheStore::HEADER_SIZE + i64::from(CHUNK)) as u64, "padded to the chunk size");
        let stem = f.file_stem().unwrap().to_string_lossy().into_owned();
        assert!(stem.len() == 32 && stem.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()), "{stem}");
        assert_eq!(meta.modified().unwrap(), fixed_file_time());
        assert_eq!(meta.created().unwrap(), fixed_file_time());
        assert_eq!(meta.accessed().unwrap(), fixed_file_time());
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut fsio::open_for_read_without_access_time(f).unwrap(), &mut bytes).unwrap();
        assert_eq!(&bytes[..8], b"SAVC0003");
        assert_eq!(i32::from_le_bytes(bytes[8..12].try_into().unwrap()), CHUNK);
    }
    assert_eq!(std::fs::metadata(&layout).unwrap().modified().unwrap(), fixed_file_time());

    let id = VideoCacheStore::compute_video_identity(Some(&path), raw.len() as i64, CHUNK);
    assert_eq!(store.cached_chunk_indices(&id, 4, CHUNK, raw.len() as i64).len(), 4);
    assert_eq!(store.try_read_chunk(&id, CHUNK, 3, 1234).unwrap(), &raw[3 * CHUNK as usize..]);
    assert_eq!(store.try_read_chunk(&id, CHUNK, 0, CHUNK).unwrap(), &raw[..CHUNK as usize]);
    for f in &files {
        assert_eq!(std::fs::metadata(f).unwrap().accessed().unwrap(), fixed_file_time(), "reads keep the access time");
    }

    // The cache survives a logout and comes back with the next login of the same key.
    fx.profiles.logout();
    assert!(store.cached_chunk_indices(&id, 4, CHUNK, raw.len() as i64).is_empty());
    fx.profiles.login("alice", 2);
    assert_eq!(store.cached_chunk_indices(&id, 4, CHUNK, raw.len() as i64).len(), 4);
}

#[test]
fn chunks_persist_per_profile_with_zero_source_reads_on_reopen_and_isolation() {
    // `EncryptedVideoCacheStore_PersistsAesGcmChunksPerProfile_ZeroDiskReadsOnSeekAndReopen_AndIsolatesProfiles`.
    let fx = CacheFixture::new();
    let mut raw: Vec<u8> = (0..256 * 1024).map(|i: usize| ((i * 73 + 19) & 0xFF) as u8).collect();
    let marker = b"PLAINTEXT_VIDEO_HEADER_MARKER_DO_NOT_LEAK_ON_DISK";
    raw[128..128 + marker.len()].copy_from_slice(marker);
    let path = fx.dir.path().join("episode_01.mp4");
    std::fs::write(&path, &raw).unwrap();
    let path = path.to_string_lossy().into_owned();
    fx.profiles.login("alice", 9);
    let store = fx.store();
    let id = VideoCacheStore::compute_video_identity(Some(&path), raw.len() as i64, CHUNK);

    {
        let mut s = memory_stream(&raw, 2, Some(store.clone()), Some(&path));
        let mut all = Vec::new();
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            let n = std::io::Read::read(&mut s, &mut buf).unwrap();
            if n == 0 {
                break;
            }
            all.extend_from_slice(&buf[..n]);
        }
        assert_eq!(all, raw);
        assert_eq!(s.underlying_read_count(), 4);
        std::io::Seek::seek(&mut s, std::io::SeekFrom::Start(0)).unwrap();
        let mut first = vec![0u8; CHUNK as usize];
        std::io::Read::read_exact(&mut s, &mut first).unwrap();
        assert_eq!(first, raw[..CHUNK as usize]);
        assert_eq!(s.underlying_read_count(), 4);
        assert_eq!(s.encrypted_cache_read_count(), 1);
    }
    let files = fx.vcache_files();
    assert_eq!(files.len(), 4);
    for f in &files {
        let bytes = std::fs::read(f).unwrap();
        assert!(!bytes.windows(marker.len()).any(|w| w == marker), "no plaintext on disk");
    }

    {
        let mut s = memory_stream(&raw, 2, Some(store.clone()), Some(&path));
        assert!((s.buffered_ratio(0.0) - 1.0).abs() < 1e-3, "persisted set loaded synchronously");
        let mut all = Vec::new();
        let mut buf = vec![0u8; 32 * 1024];
        loop {
            let n = std::io::Read::read(&mut s, &mut buf).unwrap();
            if n == 0 {
                break;
            }
            all.extend_from_slice(&buf[..n]);
        }
        assert_eq!(all, raw);
        assert_eq!(s.underlying_read_count(), 0);
        assert_eq!(s.encrypted_cache_read_count(), 4);
    }

    // Another profile sees nothing, and alice's file under bob's name fails authentication (and is deleted).
    fx.profiles.login("bob", 50);
    assert!(store.cached_chunk_indices(&id, 4, CHUNK, raw.len() as i64).is_empty());
    assert!(store.try_read_chunk(&id, CHUNK, 0, CHUNK).is_none());
    assert!(store.try_write_chunk(&id, CHUNK, 0, &vec![0u8; CHUNK as usize]));
    let bob_file = fx.vcache_files().into_iter().find(|f| !files.contains(f)).unwrap();
    std::fs::copy(&files[0], &bob_file).unwrap();
    // Copying any of alice's four files over bob's chunk 0 must fail; the copied file is removed.
    assert!(store.try_read_chunk(&id, CHUNK, 0, CHUNK).is_none());
    assert!(!bob_file.exists());
    fx.profiles.logout();
    assert!(store.try_read_chunk(&id, CHUNK, 0, CHUNK).is_none());
}

#[test]
fn cache_session_ends_with_logout_so_chunks_never_land_in_the_next_profile() {
    let fx = CacheFixture::new();
    let (path, raw) = fx.video("switch.mp4", 4 * CHUNK as usize, 73, 19);
    fx.profiles.login("alice", 3);
    let store = fx.store();
    let id = VideoCacheStore::compute_video_identity(Some(&path), raw.len() as i64, CHUNK);
    let alice = store.try_begin_session().unwrap();
    let mut stream = memory_stream(&raw, 2, Some(store.clone()), Some(&path));

    fx.profiles.logout();
    store.end_stale_sessions();
    assert!(alice.is_ended(), "ended at logout, key copy zeroed");
    fx.profiles.login("bob", 4);
    assert!(!store.try_write_chunk_for(Some(&alice), &id, CHUNK, 0, &raw[..CHUNK as usize]));
    assert!(store.try_read_chunk_for(Some(&alice), &id, CHUNK, 0, CHUNK).is_none());

    let mut buf = vec![0u8; raw.len()];
    std::io::Read::read_exact(&mut stream, &mut buf).unwrap();
    assert_eq!(buf, raw);
    assert!(fx.vcache_files().is_empty());
    assert!(store.cached_chunk_indices(&id, 4, CHUNK, raw.len() as i64).is_empty());
}

/// C# ends open streams' sessions on `StateChanged`: after a logout the key copy of a paused stream is zeroed
/// without any further read, also when nobody calls `end_stale_sessions`.
#[test]
fn a_paused_streams_session_ends_soon_after_logout_without_a_read() {
    let fx = CacheFixture::new();
    fx.profiles.login("alice", 1);
    let store = fx.store();
    let session = store.try_begin_session().expect("logged in");
    let other = store.try_begin_session().expect("logged in");
    std::thread::sleep(VideoCacheStore::SESSION_WATCH_INTERVAL * 3);
    assert!(!session.is_ended(), "a current session stays");
    drop(other);
    fx.profiles.logout();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !session.is_ended() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(session.is_ended(), "ended by the watcher");
    // A new login starts a new watcher for its own sessions; a switch ends the old profile's session.
    fx.profiles.login("bob", 2);
    let bob = store.try_begin_session().expect("logged in");
    fx.profiles.login("carol", 3);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !bob.is_ended() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(bob.is_ended(), "a profile switch ends the previous session");
}

#[test]
fn a_session_is_bound_to_the_user_and_the_key() {
    let fx = CacheFixture::new();
    fx.profiles.login("Alice", 3);
    let store = fx.store();
    let s = store.try_begin_session().unwrap();
    fx.profiles.login("ALICE ", 3);
    assert!(store.is_session_current(&s), "username compared OrdinalIgnoreCase, trimmed");
    fx.profiles.login("alice", 4);
    assert!(!store.is_session_current(&s), "same name, another key");
    fx.profiles.login("  ", 4);
    assert!(store.try_begin_session().is_none(), "a blank username is no session");
}

#[test]
fn cache_reads_do_not_wait_behind_writes_of_the_same_session() {
    let fx = CacheFixture::new();
    let (path, raw) = fx.video("concurrent.mp4", 4 * CHUNK as usize, 73, 19);
    fx.profiles.login("alice", 5);
    let store = fx.store();
    let session = store.try_begin_session().unwrap();
    let id = VideoCacheStore::compute_video_identity(Some(&path), raw.len() as i64, CHUNK);
    let c = CHUNK as usize;
    assert!(store.try_write_chunk_for(Some(&session), &id, CHUNK, 0, &raw[..c]));
    std::thread::scope(|s| {
        s.spawn(|| {
            for i in 0..20usize {
                let idx = 1 + i % 3;
                assert!(store.try_write_chunk_for(Some(&session), &id, CHUNK, idx as i64, &raw[idx * c..(idx + 1) * c]));
            }
        });
        for _ in 0..20 {
            assert_eq!(store.try_read_chunk_for(Some(&session), &id, CHUNK, 0, CHUNK).unwrap(), &raw[..c]);
        }
    });
}

#[test]
fn legacy_cache_layout_is_purged_and_the_current_layout_kept() {
    let fx = CacheFixture::new();
    let legacy = fx.cache_dir().join("0123456789abcdef0123456789abcdef").join("fedcba9876543210fedcba9876543210").join("chunk_000007.vcache");
    write(&legacy, &[0u8; 64]);
    let anonymous = fx.cache_dir().join(VideoCacheStore::ANONYMOUS_FOLDER).join("x").join("chunk_000000.vcache");
    write(&anonymous, &[0u8; 64]);
    write(&fx.cache_dir().join("stray.tmp"), b"x");
    let current = fx.cache_dir().join("v3").join("p").join("v").join("c.vcache");
    write(&current, &[0u8; 64]);
    let store = fx.store();
    store.delete_anonymous_folder();
    assert!(!legacy.exists());
    assert!(!anonymous.exists());
    assert!(!fx.cache_dir().join("stray.tmp").exists());
    assert_eq!(std::fs::read_dir(fx.cache_dir()).unwrap().count(), 1);
    assert!(current.exists());
}

/// `Directory.Delete(dir, recursive: true)` on a junction removes the link only: the purge and `clear_all` never
/// reach files outside the cache folder through one.
#[test]
fn purge_and_clear_remove_junctions_without_touching_their_targets() {
    let fx = CacheFixture::new();
    let outside = fx.dir.path().join("outside");
    write(&outside.join("keep.vcache"), &[1u8; 8]);
    std::fs::create_dir_all(fx.cache_dir()).unwrap();
    let junction = fx.cache_dir().join("legacy-link");
    let made = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&junction)
        .arg(&outside)
        .output()
        .is_ok_and(|o| o.status.success());
    if !made {
        eprintln!("skipped: mklink /J is not available");
        return;
    }
    let store = fx.store();
    assert!(!junction.exists() && std::fs::symlink_metadata(&junction).is_err(), "the junction itself is gone");
    assert!(outside.join("keep.vcache").exists(), "its target is untouched");
    assert_eq!(store.total_used_bytes(), 0);

    let v3_link = fx.cache_dir().join("v3").join("link");
    std::fs::create_dir_all(fx.cache_dir().join("v3")).unwrap();
    std::process::Command::new("cmd").args(["/C", "mklink", "/J"]).arg(&v3_link).arg(&outside).output().unwrap();
    assert_eq!(store.total_used_bytes(), 0, "a junction is not followed when summing");
    store.clear_all();
    assert!(outside.join("keep.vcache").exists(), "clear_all never follows a junction");
    assert!(std::fs::symlink_metadata(&v3_link).is_err());
}

#[test]
fn damaged_and_mismatched_chunks() {
    let fx = CacheFixture::new();
    fx.profiles.login("alice", 6);
    let store = fx.store();
    let id = "stream|len:200000|cs:65536";
    let data = vec![7u8; CHUNK as usize];
    assert!(store.try_write_chunk(id, CHUNK, 0, &data));
    let file = fx.vcache_files().pop().unwrap();
    // Wrong expected length: the AAD carries the real length, so authentication fails and the file goes.
    assert!(store.try_read_chunk(id, CHUNK, 0, 100).is_none());
    assert!(!file.exists());

    assert!(store.try_write_chunk(id, CHUNK, 0, &data));
    let file = fx.vcache_files().pop().unwrap();
    // A file of the wrong size is a miss but is kept.
    let mut bytes = std::fs::read(&file).unwrap();
    bytes.push(0);
    std::fs::write(&file, &bytes).unwrap();
    assert!(store.try_read_chunk(id, CHUNK, 0, CHUNK).is_none());
    assert!(file.exists());
    // Flipped ciphertext with the right size: deleted.
    bytes.pop();
    bytes[100] ^= 1;
    std::fs::write(&file, &bytes).unwrap();
    assert!(store.try_read_chunk(id, CHUNK, 0, CHUNK).is_none());
    assert!(!file.exists());
    // Wrong magic is a miss but is kept.
    assert!(store.try_write_chunk(id, CHUNK, 0, &data));
    let file = fx.vcache_files().pop().unwrap();
    let mut bytes = std::fs::read(&file).unwrap();
    bytes[7] = b'1';
    std::fs::write(&file, &bytes).unwrap();
    assert!(store.try_read_chunk(id, CHUNK, 0, CHUNK).is_none());
    assert!(file.exists());
    // Invalid arguments.
    assert!(!store.try_write_chunk(id, CHUNK, 0, &[]));
    assert!(!store.try_write_chunk(id, CHUNK, -1, &data));
    assert!(!store.try_write_chunk(id, CHUNK, 0, &vec![0u8; CHUNK as usize + 1]));
    assert!(!store.try_write_chunk(" ", CHUNK, 0, &data));
    assert!(store.try_read_chunk(id, CHUNK, 0, CHUNK + 1).is_none());
}

#[test]
fn quota_eviction_usage_and_clear() {
    let fx = CacheFixture::new();
    fx.profiles.login("alice", 7);
    let store = fx.store();
    let per_file = VideoCacheStore::HEADER_SIZE + i64::from(CHUNK);
    for i in 0..6 {
        assert!(store.try_write_chunk("stream|len:1|cs:1", CHUNK, i, &[i as u8; 10]));
    }
    assert_eq!(store.total_used_bytes(), 6 * per_file);
    let mut names: Vec<String> = fx.vcache_files().iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
    names.sort();
    store.enforce_quota(Some(4 * per_file));
    let mut left: Vec<String> = fx.vcache_files().iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
    left.sort();
    assert_eq!(left, names[2..], "the first names in ordinal order are evicted (file times are fixed)");
    // A write larger than the limit is refused.
    store.enforce_quota(Some(i64::MAX));
    let tiny = VideoCacheStore::new(fx.dir.path().join("Tiny"), fx.profiles.provider(), Some(1000));
    assert!(!tiny.try_write_chunk("stream|len:1|cs:1", CHUNK, 0, &[1u8; 10]));
    store.clear_all();
    assert_eq!(store.total_used_bytes(), 0);
    assert_eq!(std::fs::read_dir(fx.cache_dir()).unwrap().count(), 0);
}

#[test]
fn max_cache_size_defaults_to_20_gb_and_clamps() {
    let fx = CacheFixture::new();
    let store = VideoCacheStore::new(fx.cache_dir(), fx.profiles.provider(), None);
    assert_eq!(VideoCacheStore::DEFAULT_MAX_GB, 20);
    assert_eq!(store.max_cache_size_gb(), 20);
    store.set_max_cache_size_gb(50);
    assert_eq!(store.max_cache_size_gb(), 50);
    assert_eq!(store.max_cache_size_bytes(), 50 << 30);
    store.set_max_cache_size_gb(0);
    assert_eq!(store.max_cache_size_gb(), 1);
    store.set_max_cache_size_gb(9999);
    assert_eq!(store.max_cache_size_gb(), 500);
    store.set_max_cache_size_bytes(5);
    assert_eq!(store.max_cache_size_bytes(), 64 * 1024);
    assert_eq!(store.max_cache_size_gb(), 1);
}

// ---- buffered stream ------------------------------------------------------------------------------------------------

#[test]
fn buffered_stream_coalesces_small_reads_into_large_sequential_chunks_and_caches_seeks() {
    let raw: Vec<u8> = (0..MIB).map(|i| ((i * 31 + 17) & 0xFF) as u8).collect();
    let mut s = BufferedVideoStream::from_stream(Box::new(std::io::Cursor::new(raw.clone())), false, 256 * 1024, 4, 0, None, None, None).unwrap();
    let mut buf = vec![0u8; 4096];
    for i in 0..256 {
        assert_eq!(std::io::Read::read(&mut s, &mut buf).unwrap(), 4096);
        assert_eq!(buf, raw[i * 4096..(i + 1) * 4096]);
    }
    assert_eq!(s.underlying_read_count(), 4);
    assert_eq!(s.underlying_bytes_read(), MIB as i64);
    assert_eq!(s.buffered_ratio(0.25), 1.0);
    std::io::Seek::seek(&mut s, std::io::SeekFrom::Start(128 * 1024)).unwrap();
    let mut eight = vec![0u8; 8192];
    assert_eq!(std::io::Read::read(&mut s, &mut eight).unwrap(), 8192);
    assert_eq!(eight, raw[128 * 1024..136 * 1024]);
    assert_eq!(s.underlying_read_count(), 4);
    assert_eq!(s.cached_chunk_count(), 4);
}

#[test]
fn buffered_stream_reports_progressive_buffered_ratio_ahead_of_playback() {
    let raw = vec![0u8; 4 * 256 * 1024];
    let mut s = BufferedVideoStream::from_stream(Box::new(std::io::Cursor::new(raw)), false, 256 * 1024, 4, 0, None, None, None).unwrap();
    assert_eq!(s.buffered_ratio(0.0), 0.0);
    let mut one = [0u8; 1];
    std::io::Read::read(&mut s, &mut one).unwrap();
    assert_eq!(s.underlying_read_count(), 1);
    assert!((s.buffered_ratio(0.0) - 0.25).abs() < 1e-3);
    std::io::Seek::seek(&mut s, std::io::SeekFrom::Start(256 * 1024)).unwrap();
    std::io::Read::read(&mut s, &mut one).unwrap();
    assert_eq!(s.underlying_read_count(), 2);
    assert!((s.buffered_ratio(0.10) - 0.50).abs() < 1e-3);
    assert_eq!(s.buffered_ratio(f64::NAN), 0.5);
}

#[test]
fn buffered_stream_surface() {
    let raw: Vec<u8> = (0..300_000).map(|i: usize| (i % 251) as u8).collect();
    let s = BufferedVideoStream::from_stream(Box::new(std::io::Cursor::new(raw.clone())), false, 1000, 1, 5, None, None, None).unwrap();
    assert_eq!(s.chunk_size_bytes(), 64 * 1024, "chunk size floor");
    assert_eq!(s.length(), 300_000);
    assert!(s.seek_shared(std::io::SeekFrom::Current(-1)).is_err(), "cannot seek before the beginning");
    assert_eq!(s.seek_shared(std::io::SeekFrom::End(10)).unwrap(), 300_010);
    let mut buf = [0u8; 10];
    assert_eq!(s.read_shared(&mut buf).unwrap(), 0, "reads past the end return 0");
    assert_eq!(s.seek_shared(std::io::SeekFrom::End(-10)).unwrap(), 299_990);
    let mut r: &BufferedVideoStream = &s;
    assert_eq!(std::io::Read::read(&mut r, &mut [0u8; 100]).unwrap(), 10, "a partial read only at the end");
    assert!(s.cached_chunk_count() <= 2, "RAM window never exceeds max_cached (floor 2)");
    assert_eq!(mime_type(Some("C:\\a.MKV")), "video/x-matroska");
    assert_eq!(mime_type(Some("a.opus")), "audio/ogg");
    assert_eq!(mime_type(None), "video/mp4");
    assert_eq!(mime_type(Some("a.xyz")), "video/mp4");
}

#[test]
fn read_ahead_prefetches_and_the_file_stream_persists_in_the_background() {
    let fx = CacheFixture::new();
    let (path, raw) = fx.video("readahead.mp4", 8 * CHUNK as usize + 5, 13, 1);
    fx.profiles.login("alice", 8);
    let store = fx.store();
    {
        let s = BufferedVideoStream::open_file(Path::new(&path), CHUNK, 8, 3, Some(store.clone())).unwrap();
        let mut buf = vec![0u8; 1000];
        s.read_shared(&mut buf).unwrap();
        assert_eq!(buf, raw[..1000]);
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while s.cached_chunk_count() < 4 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(s.cached_chunk_count(), 4, "chunk 0 plus three chunks of read-ahead");
        let id = VideoCacheStore::compute_video_identity(Some(&path), raw.len() as i64, CHUNK);
        while store.cached_chunk_indices(&id, 9, CHUNK, raw.len() as i64).len() < 4 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    let id = VideoCacheStore::compute_video_identity(Some(&path), raw.len() as i64, CHUNK);
    assert_eq!(store.cached_chunk_indices(&id, 9, CHUNK, raw.len() as i64).len(), 4);

    // Reopening reads the cached chunks from the SSD, not from the file.
    let s = BufferedVideoStream::open_file(Path::new(&path), CHUNK, 8, 0, Some(store.clone())).unwrap();
    let mut all = vec![0u8; raw.len()];
    let mut r: &BufferedVideoStream = &s;
    std::io::Read::read_exact(&mut r, &mut all).unwrap();
    assert_eq!(all, raw);
    assert_eq!(s.encrypted_cache_read_count(), 4);
    assert_eq!(s.underlying_read_count(), 5);
}

/// A source whose first read of the second chunk fails after moving its position (a flaky drive mid-burst).
struct FlakySource {
    inner: std::io::Cursor<Vec<u8>>,
    fail_once_at: Option<u64>,
}

impl std::io::Read for FlakySource {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.fail_once_at == Some(self.inner.position()) {
            self.fail_once_at = None;
            self.inner.set_position(self.inner.position() + 10);
            return Err(std::io::Error::other("device error"));
        }
        self.inner.read(buf)
    }
}

impl std::io::Seek for FlakySource {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

/// C# seeks whenever the source's live `Position` is not the chunk start; after a failed burst the position is
/// unknown, so the retry seeks again and returns the right bytes.
#[test]
fn a_failed_source_read_is_retried_from_the_chunk_start() {
    let raw: Vec<u8> = (0..3 * CHUNK as usize).map(|i| (i % 249) as u8).collect();
    let source = FlakySource { inner: std::io::Cursor::new(raw.clone()), fail_once_at: Some(CHUNK as u64) };
    let s = BufferedVideoStream::from_stream(Box::new(source), false, CHUNK, 4, 0, None, None, None).unwrap();
    let mut first = vec![0u8; CHUNK as usize];
    assert_eq!(s.read_shared(&mut first).unwrap(), CHUNK as usize);
    let mut buf = vec![0u8; 100];
    assert!(s.read_shared(&mut buf).is_err(), "the failed burst surfaces as an error");
    assert_eq!(s.position(), CHUNK as u64, "a failed read does not move the stream");
    assert_eq!(s.read_shared(&mut buf).unwrap(), 100);
    assert_eq!(buf, raw[CHUNK as usize..CHUNK as usize + 100]);
}
