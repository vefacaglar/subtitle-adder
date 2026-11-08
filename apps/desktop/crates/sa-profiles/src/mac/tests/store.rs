//! `ProfileStoreTests` (12): ports of the Windows `LocalProfileTests`.

use std::collections::HashSet;

use super::*;
use crate::{FingerprintMode, ProfileCounts, ProfileError, ProfileStore, VideoFeedFilter as F};

#[test]
fn register_hashes_password_with_pbkdf2_and_never_stores_plaintext() {
    let folder = TestFolder::new();
    let store = folder.make_store();
    assert!(!store.is_logged_in());
    assert_eq!(store.current_username(), None);
    let secret = "MySuperSecretPassword!2026";
    store.register_user("emre", secret, true).unwrap();
    assert!(store.is_logged_in());
    assert_eq!(store.current_username().as_deref(), Some("emre"));
    assert!(store.registered_usernames().contains(&"emre".to_string()));
    let raw = folder.raw_profiles();
    assert!(!raw.to_lowercase().contains(&secret.to_lowercase()));
    assert!(raw.contains("saltBase64"));
    assert!(raw.contains("passwordHashBase64"));
    assert!(raw.contains("100000"));
}

#[test]
fn login_and_logout_enforce_password_and_session_state() {
    let folder = TestFolder::new();
    let store = folder.make_store();
    store.register_user("alice", "CorrectHorseBatteryStaple", true).unwrap();
    store.logout_user().unwrap();
    assert!(!store.is_logged_in());
    assert_eq!(store.current_username(), None);
    assert_eq!(store.login_user("alice", "WrongPassword", true), Err(ProfileError::InvalidCredentials));
    assert!(!store.is_logged_in());
    assert_eq!(store.login_user("unknown_user", "CorrectHorseBatteryStaple", true), Err(ProfileError::InvalidCredentials));
    store.login_user("ALICE", "CorrectHorseBatteryStaple", true).unwrap();
    assert_eq!(store.current_username().as_deref(), Some("alice"));
}

#[test]
fn logged_out_state_does_not_record_watch_history_or_expose_stats() {
    let folder = TestFolder::new();
    let video = folder.video("lecture.mp4");
    let store = folder.make_store();
    store.register_user("alice", "pw123", true).unwrap();
    store.try_record_watch(&video, 42.0, 120.0).unwrap();
    store.try_set_liked(&video, true).unwrap();
    store.try_set_rating(&video, 5).unwrap();
    store.logout_user().unwrap();

    assert_eq!(store.stats_mode(&video, FingerprintMode::Compute), None);
    assert!(feed(&store, F::All).is_empty());
    assert!(feed(&store, F::History).is_empty());
    assert_eq!(store.totals(), ProfileCounts::default());
    assert_eq!(store.try_record_watch(&video, 99.0, 120.0).unwrap(), None);
    assert!(!store.try_toggle_like(&video).unwrap());
    assert_eq!(store.try_set_rating(&video, 1).unwrap(), None);
    assert_eq!(store.try_create_list("Test List").unwrap(), None);
    assert!(folder.cache.is_empty(), "PRIVACY OVERRIDE: nothing is fingerprinted while logged out");

    store.login_user("alice", "pw123", true).unwrap();
    let stats = store.stats_mode(&video, FingerprintMode::Compute).unwrap();
    assert_eq!(stats.view_count, 1);
    assert!(stats.is_liked);
    assert_eq!(stats.rating, 5);
    assert_eq!(stats.last_position_seconds, 42.0);
}

#[test]
fn persistence_reloads_view_count_likes_ratings_and_last_watched_from_disk() {
    let folder = TestFolder::new();
    let video = folder.video("movie.mkv");
    let first = folder.make_store();
    first.register_user("cinema_fan", "pass99", false).unwrap();
    first.try_record_watch(&video, 15.0, 300.0).unwrap();
    first.try_record_watch(&video, 180.0, 300.0).unwrap();
    first.try_update_playback_progress(&video, 210.0, 300.0).unwrap();
    first.try_set_liked(&video, true).unwrap();
    first.try_set_rating(&video, 4).unwrap();

    let second = folder.make_store();
    assert!(!second.is_logged_in());
    assert!(second.registered_usernames().contains(&"cinema_fan".to_string()));
    second.login_user("cinema_fan", "pass99", true).unwrap();
    let restored = second.stats_mode(&video, FingerprintMode::Compute).unwrap();
    assert_eq!(restored.view_count, 2);
    assert!(restored.is_liked);
    assert_eq!(restored.rating, 4);
    assert_eq!(restored.last_position_seconds, 210.0);
    assert_eq!(restored.duration_seconds, 300.0);
    assert!(restored.last_watched_at.is_some());
}

#[test]
fn remember_me_restores_the_session_across_new_instances_until_logout() {
    let folder = TestFolder::new();
    let video = folder.video("remembered_show.mp4");
    let secrets = TestSecrets::new();
    let first = folder.store_with(&secrets);
    first.register_user("emre", "pass2026", true).unwrap();
    assert!(first.remember_me_flag());
    assert!(first.remember_me(), "the trait reports a remembered session");
    first.try_record_watch(&video, 50.0, 200.0).unwrap();
    first.try_set_liked(&video, true).unwrap();

    let second = folder.store_with(&secrets);
    assert!(second.is_logged_in());
    assert_eq!(second.current_username().as_deref(), Some("emre"));
    assert_eq!(second.last_username().as_deref(), Some("emre"));
    let stats = second.stats_mode(&video, FingerprintMode::Compute).unwrap();
    assert_eq!(stats.view_count, 1);
    assert!(stats.is_liked);

    second.logout_user().unwrap();
    let third = folder.store_with(&secrets);
    assert!(!third.is_logged_in());
    assert_eq!(third.last_username().as_deref(), Some("emre"));
}

#[test]
fn encrypted_vault_and_thumbnails_never_expose_plaintext_on_disk() {
    let folder = TestFolder::new();
    let secret_video = folder.video("TopSecret_Project_Alpha_2026.mkv");
    let secret_list = "Gizli Özel Koleksiyonum";
    let thumbnail = b"FAKE_PNG_THUMBNAIL_PAYLOAD_987654321".to_vec();
    let store = folder.make_store();
    store.register_user("vault_user", "UltraStrongVaultPass!99", false).unwrap();
    store.try_record_watch(&secret_video, 33.0, 100.0).unwrap();
    store.try_set_liked(&secret_video, true).unwrap();
    store.try_set_rating(&secret_video, 5).unwrap();
    let list = store.try_create_list(secret_list).unwrap().unwrap();
    assert!(store.try_add_video(&secret_video, &list.id).unwrap());
    assert!(store.try_set_thumbnail(&secret_video, &thumbnail).unwrap());

    let raw = folder.raw_profiles();
    assert!(raw.contains("encryptedVaultBase64"));
    let cached = folder.cache_files();
    assert_eq!(cached.len(), 1);
    for file in &cached {
        let bytes = std::fs::read(file).unwrap();
        assert!(!bytes.windows(thumbnail.len()).any(|w| w == thumbnail.as_slice()));
        let lower = file.to_string_lossy().to_lowercase();
        assert!(!lower.contains("topsecret"));
        assert!(!lower.contains("vault_user"));
    }
    assert!(!raw.to_lowercase().contains("topsecret_project_alpha_2026"));
    assert!(!raw.to_lowercase().contains(&secret_list.to_lowercase()));
    use base64::Engine as _;
    assert!(!raw.contains(&base64::engine::general_purpose::STANDARD.encode(&thumbnail)));

    let reloaded = folder.make_store();
    assert!(!reloaded.is_logged_in());
    assert_eq!(reloaded.thumbnail(&secret_video), None);
    reloaded.login_user("vault_user", "UltraStrongVaultPass!99", true).unwrap();
    let stats = reloaded.stats_mode(&secret_video, FingerprintMode::Compute).unwrap();
    assert_eq!(stats.view_count, 1);
    assert!(stats.is_liked);
    assert_eq!(stats.rating, 5);
    assert_eq!(reloaded.thumbnail(&secret_video), Some(thumbnail));
    assert_eq!(stats.thumbnail, None);
    let history = feed(&reloaded, F::History);
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].thumbnail, None);
}

#[test]
fn two_instances_logged_into_the_same_profile_merge_history_likes_ratings_and_lists() {
    let folder = TestFolder::new();
    let video1 = folder.video("Instance1_Episode01.mp4");
    let video2 = folder.video("Instance2_Episode02.mp4");
    let secrets = TestSecrets::new();
    let instance1 = folder.store_with(&secrets);
    instance1.register_user("shared_user", "shared_pass_123", true).unwrap();
    let instance2 = folder.store_with(&secrets);
    assert!(instance2.is_logged_in());
    assert_eq!(instance2.current_username().as_deref(), Some("shared_user"));

    instance1.try_record_watch(&video1, 30.0, 120.0).unwrap();
    instance1.try_set_liked(&video1, true).unwrap();
    instance2.try_record_watch(&video2, 90.0, 180.0).unwrap();
    instance2.try_set_rating(&video2, 5).unwrap();
    let list = instance2.try_create_list("İki Pencere Ortak Liste").unwrap().unwrap();
    assert!(instance2.try_add_video(&video2, &list.id).unwrap());
    instance1.try_record_watch(&video1, 60.0, 120.0).unwrap();
    assert!(instance1.try_add_video(&video1, &list.id).unwrap());

    assert_eq!(feed(&instance1, F::History).len(), 2);
    assert_eq!(feed(&instance2, F::History).len(), 2);
    let v1 = instance2.stats_mode(&video1, FingerprintMode::Compute).unwrap();
    assert_eq!(v1.view_count, 2);
    assert!(v1.is_liked);
    let v2 = instance1.stats_mode(&video2, FingerprintMode::Compute).unwrap();
    assert_eq!(v2.view_count, 1);
    assert_eq!(v2.rating, 5);
    assert_eq!(instance1.lists().len(), 1);
    assert_eq!(instance2.lists().len(), 1);
    assert_eq!(instance1.lists()[0].video_paths.len(), 2);
    assert_eq!(instance2.lists()[0].video_paths.len(), 2);
}

#[test]
fn progress_and_thumbnail_writes_never_raise_the_local_change_signal() {
    let folder = TestFolder::new();
    let video = folder.video("Playing_Movie.mp4");
    let store = folder.store(StoreOpts { watches: true, ..StoreOpts::default() });
    store.register_user("emre", "pass123", true).unwrap();
    store.try_record_watch(&video, 0.0, 0.0).unwrap();
    let log = ChangeLog::new(&store);
    for position in [15.0, 30.0, 45.0] {
        store.try_update_playback_progress(&video, position, 300.0).unwrap();
    }
    let thumbnail = b"SAMPLE_THUMBNAIL_BYTES".to_vec();
    assert!(store.try_set_thumbnail(&video, &thumbnail).unwrap());
    assert!(store.try_set_thumbnail(&video, &thumbnail).unwrap());
    std::thread::sleep(Duration::from_millis(700));
    assert!(log.events().is_empty(), "{:?}", log.events());
    assert_eq!(store.stats_mode(&video, FingerprintMode::Compute).unwrap().last_position_seconds, 45.0);
    assert_eq!(store.thumbnail(&video), Some(thumbnail));
    store.try_set_liked(&video, true).unwrap();
    assert!(eventually(|| log.events() == vec![ProfileChange::Local]));
}

#[test]
fn multiple_profiles_have_isolated_history_likes_ratings_and_lists() {
    let folder = TestFolder::new();
    let a = folder.video("ep1.mp4");
    let b = folder.video("ep2.mp4");
    let store = folder.make_store();
    store.register_user("user_one", "pass1", true).unwrap();
    for _ in 0..3 {
        store.try_record_watch(&a, 0.0, 0.0).unwrap();
    }
    store.try_set_liked(&a, true).unwrap();
    store.try_set_rating(&a, 5).unwrap();
    let sci = store.try_create_list("Bilim Kurgu").unwrap().unwrap();
    assert!(store.try_add_video(&a, &sci.id).unwrap());
    store.logout_user().unwrap();

    store.register_user("user_two", "pass2", true).unwrap();
    assert_eq!(store.stats_mode(&a, FingerprintMode::Compute), None);
    assert!(store.lists().is_empty());
    store.try_record_watch(&a, 0.0, 0.0).unwrap();
    store.try_set_rating(&a, 2).unwrap();
    store.try_record_watch(&b, 0.0, 0.0).unwrap();
    store.try_record_watch(&b, 0.0, 0.0).unwrap();
    store.try_set_liked(&b, true).unwrap();
    store.try_set_rating(&b, 4).unwrap();
    let comedy = store.try_create_list("Komedi").unwrap().unwrap();
    assert!(store.try_add_video(&b, &comedy.id).unwrap());
    store.logout_user().unwrap();

    let reloaded = folder.make_store();
    reloaded.login_user("user_one", "pass1", true).unwrap();
    let one_a = reloaded.stats_mode(&a, FingerprintMode::Compute).unwrap();
    assert_eq!((one_a.view_count, one_a.is_liked, one_a.rating), (3, true, 5));
    assert_eq!(reloaded.stats_mode(&b, FingerprintMode::Compute), None);
    let lists = reloaded.lists();
    assert_eq!(lists.len(), 1);
    assert_eq!(lists[0].name, "Bilim Kurgu");
    assert_eq!(lists[0].video_paths, vec![normalized(&a)]);

    reloaded.login_user("user_two", "pass2", true).unwrap();
    let two_a = reloaded.stats_mode(&a, FingerprintMode::Compute).unwrap();
    let two_b = reloaded.stats_mode(&b, FingerprintMode::Compute).unwrap();
    assert_eq!((two_a.view_count, two_a.is_liked, two_a.rating), (1, false, 2));
    assert_eq!((two_b.view_count, two_b.is_liked, two_b.rating), (2, true, 4));
    let lists = reloaded.lists();
    assert_eq!(lists.len(), 1);
    assert_eq!(lists[0].name, "Komedi");
    assert_eq!(lists[0].video_paths, vec![normalized(&b)]);
}

#[test]
fn custom_lists_and_clear_watch_history_preserve_favorites_ratings_and_lists() {
    let folder = TestFolder::new();
    let v1 = folder.video("Sezon_1_Bolum_01.mkv");
    let v2 = folder.video("Sezon_2_Bolum_05.mkv");
    let v3 = folder.video("Belgesel_Uzay.mp4");
    let store = folder.make_store();
    store.register_user("emre", "pw123", true).unwrap();
    let watch_later = store.try_create_list("Daha Sonra İzle").unwrap().unwrap();
    let best = store.try_create_list("En İyi Bölümler").unwrap().unwrap();
    assert_eq!(store.try_create_list("daha sonra izle"), Err(ProfileError::DuplicateListName));
    assert_eq!(store.lists().len(), 2);

    assert!(store.try_toggle_video(&v2, &watch_later.id).unwrap());
    assert!(store.try_toggle_video(&v1, &best.id).unwrap());
    assert!(store.try_toggle_video(&v2, &best.id).unwrap());
    store.try_record_watch(&v1, 0.0, 0.0).unwrap();
    store.try_toggle_like(&v1).unwrap();
    store.try_record_watch(&v2, 0.0, 0.0).unwrap();
    store.try_set_rating(&v2, 5).unwrap();
    store.try_record_watch(&v3, 0.0, 0.0).unwrap();
    let doomed = b"DOOMED".to_vec();
    let kept = b"KEPT".to_vec();
    store.try_set_thumbnail(&v3, &doomed).unwrap();
    store.try_set_thumbnail(&v1, &kept).unwrap();

    let in_best = feed(&store, list(&best.id));
    assert_eq!(paths_of(&in_best), vec![normalized(&v1), normalized(&v2)]);
    assert_eq!(in_best.last().unwrap().rating, 5);
    let names: HashSet<String> =
        store.lists_containing_mode(&v2, FingerprintMode::Compute).into_iter().map(|l| l.name).collect();
    assert_eq!(names, HashSet::from(["Daha Sonra İzle".to_string(), "En İyi Bölümler".to_string()]));

    assert!(store.try_clear_watch_history().unwrap());
    assert_eq!(store.totals(), ProfileCounts { watched: 0, views: 0, liked: 1, rated: 1 });
    assert!(feed(&store, F::History).is_empty());
    let liked = feed(&store, F::Liked);
    assert_eq!(paths_of(&liked), vec![normalized(&v1)]);
    assert_eq!(liked[0].view_count, 0);
    assert_eq!(store.thumbnail(&v1), Some(kept));
    assert_eq!(paths_of(&feed(&store, F::TopRated)), vec![normalized(&v2)]);
    assert_eq!(feed(&store, list(&best.id)).len(), 2);
    assert_eq!(store.stats_mode(&v3, FingerprintMode::Compute), None);
    assert_eq!(store.thumbnail(&v3), None);
}

#[test]
fn filters_multi_token_search_and_engagement_actions_work_correctly() {
    let folder = TestFolder::new();
    let v1 = folder.video("Alpha_Tutorial.mp4");
    let v2 = folder.video("Beta_Documentary.mkv");
    let v3 = folder.video("Gamma_Clip.mov");
    let discovered = folder.video("Sezon_2_Bolum_05.mp4");
    let store = folder.make_store();
    assert!(!store.is_logged_in());
    assert!(feed(&store, F::All).is_empty());
    store.register_user("emre", "secret123", true).unwrap();
    let all = vec![v1.clone(), v2.clone(), v3.clone(), discovered.clone()];

    store.try_record_watch(&v1, 0.0, 0.0).unwrap();
    store.try_toggle_like(&v1).unwrap();
    store.try_set_rating(&v1, 3).unwrap();
    for _ in 0..3 {
        store.try_record_watch(&v2, 0.0, 0.0).unwrap();
    }
    store.try_set_rating(&v2, 5).unwrap();
    store.try_record_watch(&v3, 0.0, 0.0).unwrap();
    store.try_record_watch(&v3, 0.0, 0.0).unwrap();
    store.try_toggle_like(&v3).unwrap();
    store.try_set_rating(&v3, 4).unwrap();

    let paths = |filter: F, search: Option<&str>| paths_of(&feed_in(&store, filter, search, &all));
    assert_eq!(paths(F::MostWatched, None), vec![normalized(&v2), normalized(&v3), normalized(&v1)]);
    assert_eq!(paths(F::TopRated, None), vec![normalized(&v2), normalized(&v3), normalized(&v1)]);
    assert_eq!(paths(F::Liked, None).into_iter().collect::<HashSet<_>>(), HashSet::from([normalized(&v1), normalized(&v3)]));
    assert_eq!(
        paths(F::History, None).into_iter().collect::<HashSet<_>>(),
        HashSet::from([normalized(&v1), normalized(&v2), normalized(&v3)])
    );
    assert_eq!(paths(F::All, None).len(), 4);
    assert_eq!(paths(F::All, Some("sezon 2 bolum")), vec![normalized(&discovered)]);
    assert_eq!(paths(F::All, Some("")).len(), 4);

    // PRIVACY OVERRIDE (UNIFIED.md §4 item 1): logged out, every filter is empty, even "all" with discovered files.
    store.logout_user().unwrap();
    assert!(!store.is_logged_in());
    folder.cache.clear();
    assert!(feed_in(&store, F::All, None, &all).is_empty());
    assert!(feed_in(&store, F::History, None, &all).is_empty());
    assert!(feed_in(&store, F::All, Some("gamma"), &all).is_empty());
    assert!(folder.cache.is_empty(), "the discovered files were not even fingerprinted");
}

#[test]
fn moving_or_renaming_a_video_file_preserves_history_likes_ratings_and_lists() {
    let folder = TestFolder::new();
    let original = folder.video("video_v1.mp4");
    let mut bytes = vec![0u8; 150_000];
    crate::crypto::random_fill(&mut bytes);
    std::fs::write(&original, &bytes).unwrap();
    let expected = folder.cache.video_id(&original);
    assert!(expected.starts_with("fp:150000:"));

    let store = folder.make_store();
    store.register_user("emre", "pass123", true).unwrap();
    store.try_record_watch(&original, 65.0, 200.0).unwrap();
    store.try_record_watch(&original, 120.0, 200.0).unwrap();
    store.try_set_liked(&original, true).unwrap();
    store.try_set_rating(&original, 5).unwrap();
    let archive = store.try_create_list("Arşivim").unwrap().unwrap();
    assert!(store.try_add_video(&original, &archive.id).unwrap());
    store.try_set_thumbnail(&original, b"THUMB").unwrap();

    let subfolder = folder.url.join("subfolder");
    std::fs::create_dir_all(&subfolder).unwrap();
    let moved = subfolder.join("renamed_video.mp4");
    std::fs::rename(&original, &moved).unwrap();
    assert_eq!(folder.cache.video_id(&moved), expected);

    let migrated = store.stats_mode(&moved, FingerprintMode::Compute).unwrap();
    assert_eq!(migrated.content_fingerprint.as_deref(), Some(expected.as_str()));
    assert_eq!(migrated.video_path, normalized(&moved));
    assert_eq!(migrated.file_name, "renamed_video.mp4");
    assert_eq!(migrated.directory_path, normalized(&subfolder));
    assert_eq!((migrated.view_count, migrated.is_liked, migrated.rating), (2, true, 5));
    assert_eq!(migrated.last_position_seconds, 120.0);
    assert_eq!(store.thumbnail(&moved), Some(b"THUMB".to_vec()));

    assert!(store.is_video_in_list(&moved, &archive.id, FingerprintMode::Compute));
    assert!(!store.is_video_in_list(&original, &archive.id, FingerprintMode::Compute));
    let list_feed = feed(&store, list(&archive.id));
    assert_eq!(list_feed.len(), 1);
    assert_eq!(list_feed[0].video_path, normalized(&moved));
    assert_eq!((list_feed[0].view_count, list_feed[0].is_liked, list_feed[0].rating), (2, true, 5));

    // A second store follows the move itself by fingerprint (reads never write; see mac-librarykit §21).
    let other = folder.store(StoreOpts { secrets: TestSecrets::new(), ..StoreOpts::default() });
    other.login_user("emre", "pass123", true).unwrap();
    assert_eq!(other.stats_mode(&moved, FingerprintMode::Compute).unwrap().view_count, 2);
    assert_eq!(other.lists()[0].video_paths, vec![normalized(&moved)]);
}
