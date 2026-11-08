//! `ProfileListsAndFeedTests` (13).

use std::collections::HashSet;

use sa_core::folding::fold;

use super::*;
use crate::mac::paths::directory;
use crate::mac::vault::{VaultContents, VideoProbe};
use crate::{FingerprintMode, ProfileError, ProfileStore, UserVideoList, VideoFeedFilter as F, VideoUserStats};

fn logged_in(folder: &TestFolder, clock: &ManualClock) -> Arc<MacProfileStore> {
    let store = folder.store(StoreOpts { clock: Some(clock.clock()), ..StoreOpts::default() });
    store.register_user("emre", "pw", false).unwrap();
    store
}

#[test]
fn list_names_are_trimmed_and_unique_ignoring_case_and_diacritics() {
    let folder = TestFolder::new();
    let store = logged_in(&folder, &ManualClock::new());
    let first = store.try_create_list("  Daha Sonra İzle  ").unwrap().unwrap();
    assert_eq!(first.name, "Daha Sonra İzle");
    assert!(first.id.len() == 32 && first.id == first.id.to_lowercase() && !first.id.contains('-'));
    for duplicate in ["daha sonra izle", "DAHA SONRA İZLE", "Daha Sonra Izle", "daha sonra izlé"] {
        assert_eq!(store.try_create_list(duplicate), Err(ProfileError::DuplicateListName), "{duplicate}");
    }
    assert_eq!(store.try_create_list("   "), Err(ProfileError::EmptyListName));
    assert!(store.try_create_list("Başka").unwrap().is_some());
    let names: Vec<String> = store.lists().into_iter().map(|l| l.name).collect();
    assert_eq!(names, ["Daha Sonra İzle", "Başka"]);
}

#[test]
fn renaming_checks_the_other_lists_but_allows_a_case_only_change() {
    let folder = TestFolder::new();
    let store = logged_in(&folder, &ManualClock::new());
    let a = store.try_create_list("Alpha").unwrap().unwrap();
    let b = store.try_create_list("Beta").unwrap().unwrap();
    assert_eq!(store.try_rename_list(&b.id, " alpha "), Err(ProfileError::DuplicateListName));
    assert_eq!(store.try_rename_list(&b.id, ""), Err(ProfileError::EmptyListName));
    assert!(store.try_rename_list(&a.id, "ALPHA").unwrap());
    assert!(store.try_rename_list(&b.id, "Gamma").unwrap());
    assert!(!store.try_rename_list("no-such-id", "x").unwrap());
    let names: Vec<String> = store.lists().into_iter().map(|l| l.name).collect();
    assert_eq!(names, ["ALPHA", "Gamma"]);
    assert!(store.try_rename_list(&a.id.to_uppercase(), "Delta").unwrap(), "ids match ignoring case");
}

#[test]
fn adding_is_idempotent_order_is_kept_and_toggling_and_removing_work() {
    let folder = TestFolder::new();
    let store = logged_in(&folder, &ManualClock::new());
    let l = store.try_create_list("L").unwrap().unwrap();
    let (v1, v2, v3) = (folder.video("1.mp4"), folder.video("2.mp4"), folder.video("3.mp4"));
    assert!(store.try_add_video(&v2, &l.id).unwrap());
    assert!(store.try_add_video(&v1, &l.id).unwrap());
    assert!(store.try_add_video(&v2, &l.id).unwrap(), "adding again succeeds");
    assert!(store.try_add_video(&v3, &l.id).unwrap());
    assert_eq!(store.lists()[0].video_paths, vec![normalized(&v2), normalized(&v1), normalized(&v3)]);
    assert_eq!(names_of(&feed(&store, list(&l.id))), ["2.mp4", "1.mp4", "3.mp4"]);

    assert!(store.try_remove_video(&v1, &l.id).unwrap());
    assert!(!store.try_remove_video(&v1, &l.id).unwrap());
    assert!(store.try_toggle_video(&v1, &l.id).unwrap(), "toggled back in");
    assert!(!store.try_toggle_video(&v1, &l.id).unwrap());
    assert!(store.is_video_in_list(&v2, &l.id, FingerprintMode::Compute));
    assert!(!store.is_video_in_list(&v1, &l.id, FingerprintMode::Compute));
    assert_eq!(store.lists_containing(&v3), vec![l.id.clone()]);
    assert!(store.lists_containing(&v1).is_empty());

    assert!(!store.try_add_video(&v1, "unknown").unwrap());
    assert!(!store.try_toggle_video(&v1, "unknown").unwrap());
    assert!(store.try_delete_list(&l.id).unwrap());
    assert!(!store.try_delete_list(&l.id).unwrap());
    assert!(store.lists().is_empty());
    assert!(feed(&store, list(&l.id)).is_empty());
}

#[test]
fn a_moved_video_replaces_its_old_path_in_every_list() {
    let folder = TestFolder::new();
    let store = logged_in(&folder, &ManualClock::new());
    let original = folder.video("clip.mp4");
    std::fs::write(&original, content(2000, 251, 0)).unwrap();
    let one = store.try_create_list("One").unwrap().unwrap();
    let two = store.try_create_list("Two").unwrap().unwrap();
    store.try_add_video(&original, &one.id).unwrap();
    store.try_add_video(&original, &two.id).unwrap();
    let moved = folder.video("moved.mp4");
    std::fs::rename(&original, &moved).unwrap();

    let names: HashSet<String> =
        store.lists_containing_mode(&moved, FingerprintMode::Compute).into_iter().map(|l| l.name).collect();
    assert_eq!(names, HashSet::from(["One".to_string(), "Two".to_string()]));
    let all: Vec<Vec<String>> = store.lists().into_iter().map(|l| l.video_paths).collect();
    assert_eq!(all, vec![vec![normalized(&moved)], vec![normalized(&moved)]]);
    assert!(store.lists_containing(&original).is_empty());
}

#[test]
fn migration_deduplicates_a_list_that_already_had_the_new_path() {
    let folder = TestFolder::new();
    let file = folder.video("here.mp4");
    std::fs::write(&file, content(3000, 241, 0)).unwrap();
    let probe = VideoProbe::new(&file, FingerprintMode::Compute, &folder.cache).unwrap();
    let fp = probe.fingerprint.clone().unwrap();
    let old_path = normalized(&folder.video("old/there.mp4"));
    let now = Ticks::now();
    let mut c = VaultContents::new();
    c.set_video(
        VideoUserStats {
            content_fingerprint: Some(fp.clone()),
            video_path: old_path.clone(),
            file_name: "there.mp4".into(),
            directory_path: directory(&old_path),
            view_count: 3,
            is_liked: false,
            rating: 0,
            last_position_seconds: 0.0,
            duration_seconds: 0.0,
            last_watched_at: None,
            updated_at: now,
            thumbnail: None,
        },
        &fp,
    );
    let mk = |id: &str, paths: Vec<String>| UserVideoList {
        id: id.into(),
        name: id.to_uppercase(),
        video_paths: paths,
        created_at: now,
        updated_at: now,
    };
    c.custom_lists = vec![
        mk("a", vec!["/x".into(), old_path.clone(), probe.path.clone(), "/y".into()]),
        mk("b", vec![probe.path.clone(), old_path.to_uppercase()]),
    ];
    let resolution = c.resolve(&probe, now).unwrap();
    assert!(resolution.migrated);
    assert_eq!(resolution.stats.video_path, probe.path);
    assert_eq!(resolution.stats.view_count, 3);
    assert_eq!(c.custom_lists[0].video_paths, vec!["/x".to_string(), probe.path.clone(), "/y".to_string()]);
    assert_eq!(c.custom_lists[1].video_paths, vec![probe.path.clone()]);
    assert!(!c.resolve(&probe, now).unwrap().migrated);
}

#[test]
fn a_copy_of_a_watched_file_does_not_steal_its_record_until_the_original_is_gone() {
    let folder = TestFolder::new();
    let store = logged_in(&folder, &ManualClock::new());
    let original = folder.video("original.mp4");
    std::fs::write(&original, content(4000, 233, 0)).unwrap();
    store.try_record_watch(&original, 7.0, 70.0).unwrap();
    let copy = folder.video("copy.mp4");
    std::fs::copy(&original, &copy).unwrap();

    let saved = folder.file_data();
    for _ in 0..2 {
        let items = feed_in(&store, F::All, None, &[original.clone(), copy.clone()]);
        assert_eq!(items.iter().find(|v| v.file_name == "original.mp4").unwrap().view_count, 1);
    }
    assert_eq!(store.stats_mode(&original, FingerprintMode::Compute).unwrap().video_path, normalized(&original));
    assert_eq!(store.stats_mode(&copy, FingerprintMode::Compute).unwrap().view_count, 1, "the copy shares the record");
    assert_eq!(store.stats_mode(&copy, FingerprintMode::Compute).unwrap().video_path, normalized(&original));
    assert_eq!(folder.file_data(), saved, "looking never rewrites the file");

    std::fs::remove_file(&original).unwrap();
    let followed = store.stats_mode(&copy, FingerprintMode::Compute).unwrap();
    assert_eq!(followed.video_path, normalized(&copy));
    assert_eq!(followed.view_count, 1);
}

#[test]
fn feed_orders_follow_the_documented_rules() {
    let folder = TestFolder::new();
    let clock = ManualClock::new();
    let store = logged_in(&folder, &clock);
    let (a, b, c, d) = (folder.video("a.mp4"), folder.video("b.mp4"), folder.video("c.mp4"), folder.video("d.mp4"));
    let idle = folder.video("idle.mp4");
    store.try_record_watch(&a, 0.0, 0.0).unwrap();
    clock.advance(1.0);
    store.try_record_watch(&b, 0.0, 0.0).unwrap();
    clock.advance(1.0);
    store.try_record_watch(&b, 0.0, 0.0).unwrap();
    clock.advance(1.0);
    store.try_record_watch(&c, 0.0, 0.0).unwrap();
    clock.advance(1.0);
    store.try_set_liked(&a, true).unwrap();
    clock.advance(1.0);
    store.try_set_rating(&d, 5).unwrap();
    clock.advance(1.0);
    store.try_set_rating(&c, 5).unwrap();
    clock.advance(1.0);
    store.try_set_liked(&d, true).unwrap();
    let discovered = vec![idle];
    let names = |f: F| names_of(&feed_in(&store, f, None, &discovered));
    assert_eq!(names(F::History), ["c.mp4", "b.mp4", "a.mp4"]);
    assert_eq!(names(F::MostWatched), ["b.mp4", "c.mp4", "a.mp4"]);
    assert_eq!(names(F::Liked), ["d.mp4", "a.mp4"]);
    assert_eq!(names(F::TopRated), ["c.mp4", "d.mp4"]);
    assert_eq!(names(F::All), ["c.mp4", "b.mp4", "a.mp4", "d.mp4", "idle.mp4"]);
}

#[test]
fn discovered_files_are_never_persisted_until_touched() {
    let folder = TestFolder::new();
    let store = logged_in(&folder, &ManualClock::new());
    let seen = folder.video("seen.mp4");
    let before = folder.file_data();
    let items = feed_in(&store, F::All, None, &[seen.clone(), seen.clone(), folder.video("other.mp4")]);
    assert_eq!(names_of(&items), ["other.mp4", "seen.mp4"]);
    assert!(items.iter().all(|v| !v.has_watched() && v.view_count == 0));
    assert_eq!(folder.file_data(), before);
    assert_eq!(store.stats_mode(&seen, FingerprintMode::Compute), None);
}

#[test]
fn feed_search_uses_the_library_search_rules() {
    let folder = TestFolder::new();
    let store = logged_in(&folder, &ManualClock::new());
    let files = vec![folder.video("Sezon_2_Bolum_05.mkv"), folder.video("Belgesel Uzay.mp4"), folder.video("İstanbul Notları.mov")];
    assert_eq!(feed_in(&store, F::All, Some("sezon 2 bolum"), &files).len(), 1);
    assert_eq!(names_of(&feed_in(&store, F::All, Some("UZAY"), &files)), ["Belgesel Uzay.mp4"]);
    assert_eq!(feed_in(&store, F::All, Some("istanbul not"), &files).len(), 1);
    assert_eq!(feed_in(&store, F::All, Some("  "), &files).len(), 3);
    assert!(feed_in(&store, F::All, Some("yok boyle"), &files).is_empty());
}

#[test]
fn list_names_fold_the_dotless_i_too() {
    let folder = TestFolder::new();
    let store = logged_in(&folder, &ManualClock::new());
    store.try_create_list("Kısa Filmler").unwrap();
    for duplicate in ["KISA FILMLER", "kisa filmler", "Kısa Fılmler", "KISA FİLMLER"] {
        assert_eq!(store.try_create_list(duplicate), Err(ProfileError::DuplicateListName), "{duplicate}");
    }
    let other = store.try_create_list("Uzun Filmler").unwrap().unwrap();
    assert_eq!(store.try_rename_list(&other.id, "kısa filmler"), Err(ProfileError::DuplicateListName));
}

#[test]
fn the_library_index_and_the_profile_search_fold_identically() {
    // RUST: both call `sa_core::folding::fold`; this pins the shared results.
    for (text, folded) in [("KISA FİLM", "kisa film"), ("kısa film", "kisa film"), ("Ağaç ILIK", "agac ilik"), ("ǅ ﬁ Ｆｕｌｌ", "ǆ fi full")] {
        assert_eq!(fold(text), folded, "{text}");
    }
}

#[test]
fn should_continue_stops_a_feed() {
    let folder = TestFolder::new();
    let store = logged_in(&folder, &ManualClock::new());
    let files = vec![folder.video("a.mp4"), folder.video("b.mp4")];
    let never = || false;
    assert!(store.feed_with(&F::All, None, &files, FingerprintMode::Compute, Some(&never)).is_empty());
    let query = crate::FeedQuery { filter: F::All, search: None, discovered: files, mode: FingerprintMode::PathOnly };
    assert_eq!(ProfileStore::feed(&*store, &query, &|| true).len(), 2);
}
