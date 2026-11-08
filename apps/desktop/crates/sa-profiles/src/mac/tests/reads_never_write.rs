//! `ProfileReadsNeverWriteTests` (9): following a moved file while reading changes the result and the in-memory
//! profile, never the file; the move is saved with the next write or `persist_pending_migrations()`.

use super::*;
use crate::{FingerprintMode, ProfileStore, UserVideoList, VideoFeedFilter as F};

struct Moved {
    folder: TestFolder,
    secrets: Arc<TestSecrets>,
    store: Arc<MacProfileStore>,
    original: PathBuf,
    moved: PathBuf,
    list: UserVideoList,
}

impl Moved {
    fn make_store(&self) -> Arc<MacProfileStore> {
        self.folder.store_with(&self.secrets)
    }
}

fn data(seed: usize) -> Vec<u8> {
    content(3000, 247, seed)
}

/// A profile with a watched, liked file in a list, which is then renamed on disk.
fn moved_setup() -> Moved {
    let folder = TestFolder::new();
    let secrets = TestSecrets::new();
    let store = folder.store_with(&secrets);
    store.register_user("emre", "pw", true).unwrap();
    let original = folder.video("clip.mp4");
    std::fs::write(&original, data(1)).unwrap();
    store.try_record_watch(&original, 7.0, 70.0).unwrap();
    store.try_set_liked(&original, true).unwrap();
    let list = store.try_create_list("Liste").unwrap().unwrap();
    store.try_add_video(&original, &list.id).unwrap();
    let moved = folder.video("renamed.mp4");
    std::fs::rename(&original, &moved).unwrap();
    Moved { folder, secrets, store, original, moved, list }
}

#[test]
fn lists_containing_a_just_moved_video_find_it_before_anything_else_looked() {
    let m = moved_setup();
    let saved = m.folder.file_data();
    assert_eq!(m.store.lists_containing(&m.moved), vec![m.list.id.clone()]);
    assert_eq!(m.store.lists()[0].video_paths, vec![normalized(&m.moved)]);
    assert_eq!(m.folder.file_data(), saved);
}

#[test]
fn is_video_in_list_finds_a_just_moved_video() {
    let m = moved_setup();
    assert!(m.store.is_video_in_list(&m.moved, &m.list.id, FingerprintMode::Compute));
}

#[test]
fn stats_of_a_moved_file_reflect_the_move_without_writing_it() {
    let m = moved_setup();
    let saved = m.folder.file_data();
    assert!(!m.store.has_unsaved_changes());
    let stats = m.store.stats_mode(&m.moved, FingerprintMode::Compute).unwrap();
    assert_eq!(stats.video_path, normalized(&m.moved));
    assert!(stats.is_liked);
    assert_eq!(stats.view_count, 1);
    assert_eq!(stats.last_position_seconds, 7.0);
    let old = m.store.stats_mode(&m.original, FingerprintMode::Compute);
    assert!(old.is_none() || old.unwrap().video_path == normalized(&m.moved));
    assert_eq!(m.folder.file_data(), saved, "a read never writes");
    assert!(m.store.has_unsaved_changes());
}

#[test]
fn a_feed_follows_moved_files_in_the_result_but_not_in_the_file() {
    let m = moved_setup();
    let saved = m.folder.file_data();
    let items = feed_in(&m.store, list(&m.list.id), None, std::slice::from_ref(&m.moved));
    assert_eq!(paths_of(&items), vec![normalized(&m.moved)]);
    assert!(items[0].is_liked);
    assert_eq!(paths_of(&feed_in(&m.store, F::Liked, None, std::slice::from_ref(&m.moved))), vec![normalized(&m.moved)]);
    assert_eq!(m.folder.file_data(), saved);
    assert!(m.store.has_unsaved_changes());
}

#[test]
fn persist_pending_migrations_writes_the_followed_move_once() {
    let m = moved_setup();
    let other = m.make_store();
    assert!(other.is_logged_in());
    let _ = m.store.stats_mode(&m.moved, FingerprintMode::Compute);
    assert_eq!(other.lists()[0].video_paths, vec![normalized(&m.original)], "nobody else sees the move yet");
    let saved = m.folder.file_data();
    assert!(m.store.try_persist_pending_migrations().unwrap());
    assert!(!m.store.has_unsaved_changes());
    assert_ne!(m.folder.file_data(), saved);
    let written = m.folder.file_data();
    assert!(!m.store.try_persist_pending_migrations().unwrap());
    assert_eq!(m.folder.file_data(), written);
    assert_eq!(other.lists()[0].video_paths, vec![normalized(&m.moved)]);
    assert_eq!(other.lists_containing(&m.moved), vec![m.list.id.clone()]);
    assert!(!other.has_unsaved_changes());
    let reopened = m.make_store();
    assert!(reopened.stats_mode(&m.moved, FingerprintMode::Compute).unwrap().is_liked);
    assert!(!reopened.has_unsaved_changes(), "already migrated, nothing to follow");
}

#[test]
fn a_move_found_while_reading_rides_along_with_the_next_write() {
    let m = moved_setup();
    let _ = m.store.lists_containing(&m.moved);
    assert!(m.store.has_unsaved_changes());
    m.store.try_set_rating(&m.folder.video("unrelated.mp4"), 3).unwrap();
    assert!(!m.store.has_unsaved_changes());
    let reopened = m.make_store();
    assert_eq!(reopened.lists()[0].video_paths, vec![normalized(&m.moved)]);
    assert_eq!(paths_of(&feed(&reopened, list(&m.list.id))), vec![normalized(&m.moved)]);
}

#[test]
fn a_move_not_yet_written_is_found_again_after_another_instances_write() {
    let m = moved_setup();
    let other = m.make_store();
    let _ = m.store.stats_mode(&m.moved, FingerprintMode::Compute);
    other.try_set_rating(&m.folder.video("elsewhere.mp4"), 2).unwrap();
    assert_eq!(m.store.lists_containing(&m.moved).len(), 1);
    assert!(m.store.stats_mode(&m.moved, FingerprintMode::Compute).unwrap().is_liked);
}

#[test]
fn path_only_looks_files_up_by_path_and_never_follows_a_move() {
    let m = moved_setup();
    let stay = m.folder.video("stay.mp4");
    std::fs::write(&stay, data(5)).unwrap();
    m.store.try_record_watch(&stay, 0.0, 0.0).unwrap();
    let saved = m.folder.file_data();
    assert_eq!(m.store.stats_mode(&m.moved, FingerprintMode::PathOnly), None);
    assert!(m.store.lists_containing_mode(&m.moved, FingerprintMode::PathOnly).is_empty());
    let placeholders = m.store.feed_with(&F::All, None, std::slice::from_ref(&m.moved), FingerprintMode::PathOnly, None);
    let shown = placeholders.iter().find(|v| v.video_path == normalized(&m.moved)).unwrap();
    assert!(shown.view_count == 0 && !shown.is_liked && shown.content_fingerprint.is_none());
    assert!(!m.store.has_unsaved_changes());
    assert_eq!(m.folder.file_data(), saved);
    assert_eq!(m.store.stats_mode(&stay, FingerprintMode::PathOnly).unwrap().view_count, 1);
    assert!(m.store.stats_mode(&m.moved, FingerprintMode::Compute).unwrap().is_liked);
    assert!(m.store.has_unsaved_changes());
}

#[test]
fn cached_only_uses_what_an_earlier_full_lookup_fingerprinted_and_reads_nothing_new() {
    let folder = TestFolder::new();
    let store = folder.make_store();
    store.register_user("emre", "pw", true).unwrap();
    let original = folder.video("shared.mp4");
    std::fs::write(&original, data(9)).unwrap();
    store.try_record_watch(&original, 3.0, 30.0).unwrap();
    let copy = folder.video("shared-copy.mp4");
    std::fs::copy(&original, &copy).unwrap();
    assert_eq!(store.stats_mode(&copy, FingerprintMode::CachedOnly), None);
    assert!(folder.cache.fingerprint(&normalized(&copy)).is_some());
    let shared = store.stats_mode(&copy, FingerprintMode::CachedOnly).unwrap();
    assert_eq!(shared.view_count, 1);
    assert_eq!(shared.video_path, normalized(&original), "a copy shares the record, it does not take the path");
}

#[test]
fn stats_fast_in_memory_needs_no_file() {
    let m = moved_setup();
    let stay = m.folder.video("stay.mp4");
    m.store.try_record_watch(&stay, 4.0, 0.0).unwrap();
    assert_eq!(m.store.stats_fast_in_memory(&stay).unwrap().view_count, 1);
    assert_eq!(m.store.stats_fast_in_memory(&m.moved), None, "no fingerprint, no move followed");
    m.store.logout_user().unwrap();
    assert_eq!(m.store.stats_fast_in_memory(&stay), None);
}
