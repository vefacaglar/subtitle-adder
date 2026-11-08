//! `VaultContentsTests` (8).

use super::*;
use crate::mac::document::VaultPayload;
use crate::mac::paths::{directory, file_name};
use crate::mac::vault::{VaultContents, VideoProbe};
use crate::{FingerprintMode, VideoUserStats};

const FP: &str = "fp:100:aaaaaaaaaaaaaaaaaaaaaaaa";

fn early() -> Ticks {
    dates::from_unix_seconds(1_700_000_000.0)
}

fn later(seconds: f64) -> Ticks {
    dates::from_unix_seconds(1_700_000_000.0 + seconds)
}

#[allow(clippy::too_many_arguments)]
fn record(
    path: &str,
    views: i64,
    liked: bool,
    rating: i64,
    position: f64,
    duration: f64,
    watched: Option<Ticks>,
    updated: Ticks,
    fingerprint: Option<&str>,
) -> VideoUserStats {
    VideoUserStats {
        content_fingerprint: Some(fingerprint.unwrap_or(FP).to_string()),
        video_path: path.into(),
        file_name: file_name(path),
        directory_path: directory(path),
        view_count: views,
        is_liked: liked,
        rating,
        last_position_seconds: position,
        duration_seconds: duration,
        last_watched_at: watched,
        updated_at: updated,
        thumbnail: None,
    }
}

fn simple(path: &str, views: i64, updated: Ticks) -> VideoUserStats {
    record(path, views, false, 0, 0.0, 0.0, None, updated, None)
}

fn contents(videos: Vec<VideoUserStats>) -> VaultContents {
    VaultContents::from_payload(&VaultPayload { videos, custom_lists: vec![], thumbnails: vec![] })
}

#[test]
fn two_records_of_one_file_are_merged_instead_of_the_later_overwriting_the_earlier() {
    let late = later(100.0);
    let older = record("/a/one.mp4", 3, true, 2, 10.0, 100.0, Some(early()), early(), None);
    let newer = record("/b/two.mp4", 4, false, 5, 60.0, 100.0, Some(late), late, None);
    for payload in [vec![older.clone(), newer.clone()], vec![newer.clone(), older.clone()]] {
        let merged = contents(payload);
        assert_eq!(merged.videos().len(), 1);
        let stats = merged.videos().get(FP).unwrap();
        assert_eq!(stats.view_count, 7, "views add up");
        assert!(stats.is_liked);
        assert_eq!(stats.rating, 5);
        assert_eq!(stats.last_watched_at, Some(late));
        assert_eq!(stats.last_position_seconds, 60.0);
        assert_eq!(stats.duration_seconds, 100.0);
        assert_eq!(stats.updated_at, late);
        assert_eq!(stats.video_path, "/b/two.mp4");
        assert_eq!(stats.content_fingerprint.as_deref(), Some(FP));
    }
}

#[test]
fn both_old_paths_still_find_the_merged_record() {
    let merged = contents(vec![simple("/a/one.mp4", 1, early()), simple("/b/two.mp4", 1, later(1.0))]);
    let cache = FingerprintCache::default();
    for path in ["/a/one.mp4", "/b/two.mp4", "/B/TWO.mp4"] {
        let probe = VideoProbe::from_normalized(path.into(), FingerprintMode::PathOnly, &cache).unwrap();
        assert_eq!(merged.find(&probe).as_deref(), Some(FP), "{path}");
    }
}

#[test]
fn a_watched_record_supplies_the_resume_point_over_an_unwatched_newer_one() {
    let watched = record("/a.mp4", 2, false, 0, 40.0, 90.0, Some(early()), early(), None);
    let touched = record("/b.mp4", 0, true, 0, 0.0, 0.0, None, later(500.0), None);
    let c = contents(vec![watched, touched]);
    let stats = c.videos().get(FP).unwrap();
    assert_eq!(stats.last_position_seconds, 40.0);
    assert_eq!(stats.duration_seconds, 90.0);
    assert!(stats.is_liked);
    assert_eq!(stats.view_count, 2);
    assert_eq!(stats.video_path, "/b.mp4");
}

#[test]
fn three_records_merge_into_one() {
    let c = contents(vec![
        record("/1.mp4", 1, false, 1, 0.0, 0.0, None, early(), None),
        record("/2.mp4", 2, true, 3, 0.0, 0.0, None, later(1.0), None),
        record("/3.mp4", 4, false, 2, 0.0, 0.0, None, later(2.0), None),
    ]);
    let stats = c.videos().get(FP).unwrap();
    assert_eq!(stats.view_count, 7);
    assert_eq!(stats.rating, 3);
    assert!(stats.is_liked);
}

#[test]
fn records_of_different_files_stay_separate_and_the_case_of_a_fingerprint_does_not_split_one() {
    let other = "fp:200:bbbbbbbbbbbbbbbbbbbbbbbb";
    let two = contents(vec![
        simple("/1.mp4", 0, early()),
        record("/2.mp4", 0, false, 0, 0.0, 0.0, None, early(), Some(other)),
    ]);
    assert_eq!(two.videos().len(), 2);
    let upper = FP.to_uppercase();
    let one = contents(vec![
        simple("/1.mp4", 1, early()),
        record("/2.mp4", 1, false, 0, 0.0, 0.0, None, early(), Some(&upper)),
    ]);
    assert_eq!(one.videos().len(), 1);
}

#[test]
fn a_merged_vault_writes_one_record() {
    let merged = contents(vec![
        record("/a/one.mp4", 3, true, 0, 0.0, 0.0, None, early(), None),
        record("/b/two.mp4", 4, false, 4, 0.0, 0.0, None, later(9.0), None),
    ]);
    let payload = merged.payload();
    assert_eq!(payload.videos.len(), 1);
    assert_eq!(payload.videos[0].view_count, 7);
}

#[test]
fn loading_a_record_without_a_fingerprint_does_not_read_its_file() {
    let folder = TestFolder::new();
    let file = folder.video("kept.mp4");
    std::fs::write(&file, content(2000, 199, 0)).unwrap();
    let path = normalized(&file);
    let mut bare = simple(&path, 2, early());
    bare.content_fingerprint = None;
    bare.file_name = String::new();
    bare.directory_path = String::new();
    let loaded = contents(vec![bare]);

    let stats = loaded.videos().get(&paths::ci(&path)).expect("keyed by path");
    assert_eq!(stats.content_fingerprint, None, "the file was not read to give it one");
    assert_eq!(stats.file_name, "kept.mp4");
    assert!(folder.cache.is_empty());

    let mut c = loaded.clone();
    let probe = VideoProbe::new(&file, FingerprintMode::Compute, &folder.cache).unwrap();
    let resolution = c.resolve(&probe, early()).unwrap();
    assert!(resolution.migrated);
    assert_eq!(resolution.stats.content_fingerprint, probe.fingerprint);
    assert_eq!(resolution.stats.view_count, 2);
    assert!(!c.resolve(&probe, early()).unwrap().migrated);
}

#[test]
fn resolving_never_reads_the_recorded_path_itself() {
    let folder = TestFolder::new();
    let original = folder.video("original.mp4");
    std::fs::write(&original, content(2000, 211, 0)).unwrap();
    let copy = folder.video("copy.mp4");
    std::fs::copy(&original, &copy).unwrap();
    let original_probe = VideoProbe::new(&original, FingerprintMode::Compute, &folder.cache).unwrap();
    let fp = original_probe.fingerprint.clone().unwrap();

    let mut c = VaultContents::new();
    let mut stats = simple(&original_probe.path, 1, early());
    stats.content_fingerprint = Some(fp.clone());
    c.set_video(stats, &fp);

    let bare = VideoProbe::new(&copy, FingerprintMode::Compute, &folder.cache).unwrap();
    assert_eq!(c.takeover_path(&bare).as_deref(), Some(original_probe.path.as_str()));
    let mut unprepared = c.clone();
    assert!(unprepared.resolve(&bare, early()).unwrap().migrated, "no prefetched fingerprint, so no disk access");

    let prepared = bare.clone().with_recorded_fingerprint(fp.clone(), &original_probe.path);
    let mut prefetched = c.clone();
    assert!(!prefetched.resolve(&prepared, early()).unwrap().migrated, "the original still holds the content: a copy");
    assert_eq!(c.takeover_path(&original_probe), None);
}

#[test]
fn a_legacy_picture_moves_to_the_cache_under_its_raw_lowercased_key() {
    // mac-librarykit §2.3 exception: cache file names are hashed from the key lowercased exactly as Swift saw it
    // (no NFC), so a picture keyed by a decomposed path stays findable by that path after it leaves the vault.
    let decomposed = "/Movies/Cafe\u{301}.MKV";
    let picture = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, b"IMG");
    let mut vault = VaultContents::from_payload(&VaultPayload {
        videos: vec![],
        custom_lists: vec![],
        thumbnails: vec![(decomposed.to_string(), picture)],
    });
    assert!(vault.has_thumbnails());
    let extracted = vault.extract_thumbnails();
    assert_eq!(extracted, vec![(decomposed.to_lowercase(), b"IMG".to_vec())]);
    assert_ne!(extracted[0].0, paths::ci(decomposed), "not the NFC identity key");
    assert!(!vault.has_thumbnails());
}
