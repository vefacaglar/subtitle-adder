//! `ProfileMultiInstanceTests` (13): several stores (instances) on one `profiles.json`.

use super::*;
use crate::mac::thumbnails::ThumbnailCache;
use crate::{FingerprintMode, ProfileError, ProfileStore};

fn watched(folder: &TestFolder, secrets: &Arc<TestSecrets>) -> Arc<MacProfileStore> {
    folder.store(StoreOpts { secrets: secrets.clone(), watches: true, ..StoreOpts::default() })
}

#[test]
fn two_stores_on_the_same_file_see_each_others_writes_without_a_watcher() {
    let folder = TestFolder::new();
    let video = folder.video("shared.mp4");
    let a = folder.make_store();
    let b = folder.make_store();
    a.register_user("alice", "pw", false).unwrap();
    assert_eq!(b.registered_usernames(), ["alice"]);
    b.login_user("alice", "pw", false).unwrap();
    a.try_record_watch(&video, 5.0, 50.0).unwrap();
    assert_eq!(b.stats_mode(&video, FingerprintMode::Compute).unwrap().view_count, 1);
    b.try_record_watch(&video, 9.0, 50.0).unwrap();
    b.try_set_rating(&video, 4).unwrap();
    let l = b.try_create_list("Ortak").unwrap().unwrap();
    assert_eq!(a.stats_mode(&video, FingerprintMode::Compute).unwrap().view_count, 2);
    assert_eq!(a.stats_mode(&video, FingerprintMode::Compute).unwrap().rating, 4);
    assert_eq!(a.lists().into_iter().map(|l| l.name).collect::<Vec<_>>(), ["Ortak"]);
    a.try_add_video(&video, &l.id).unwrap();
    b.try_record_watch(&video, 0.0, 0.0).unwrap();
    assert_eq!(a.stats_mode(&video, FingerprintMode::Compute).unwrap().view_count, 3);
    assert_eq!(b.lists()[0].video_paths, vec![normalized(&video)]);
}

#[test]
fn a_list_name_taken_in_one_instance_is_a_duplicate_in_the_other() {
    let folder = TestFolder::new();
    let a = folder.make_store();
    let b = folder.make_store();
    a.register_user("alice", "pw", false).unwrap();
    b.login_user("alice", "pw", false).unwrap();
    a.try_create_list("Daha Sonra İzle").unwrap();
    assert_eq!(b.try_create_list("daha sonra izle"), Err(ProfileError::DuplicateListName));
    assert_eq!(b.lists().len(), 1);
}

#[test]
fn concurrent_writers_from_two_stores_lose_nothing() {
    let folder = TestFolder::new();
    let video = folder.video("busy.mp4");
    let secrets = TestSecrets::new();
    let opts = || StoreOpts { secrets: secrets.clone(), lock_timeout: Duration::from_secs(30), ..StoreOpts::default() };
    let a = folder.store(opts());
    a.register_user("alice", "pw", true).unwrap();
    let b = folder.store(opts());
    assert!(b.is_logged_in());
    let failures = Arc::new(Mutex::new(Vec::<String>::new()));
    let workers: Vec<_> = (0..8)
        .map(|worker| {
            let store = if worker % 2 == 0 { a.clone() } else { b.clone() };
            let video = video.clone();
            let failures = failures.clone();
            std::thread::spawn(move || {
                for _ in 0..5 {
                    if let Err(e) = store.try_record_watch(&video, 0.0, 0.0) {
                        failures.lock().push(format!("{e:?}"));
                    }
                }
            })
        })
        .collect();
    for w in workers {
        w.join().unwrap();
    }
    assert!(failures.lock().is_empty(), "{:?}", failures.lock());
    assert_eq!(a.stats_mode(&video, FingerprintMode::Compute).unwrap().view_count, 40);
    assert_eq!(b.stats_mode(&video, FingerprintMode::Compute).unwrap().view_count, 40);
}

#[test]
fn a_change_made_by_another_instance_raises_the_external_signal_through_the_watcher() {
    let folder = TestFolder::new();
    let video = folder.video("watched.mp4");
    let secrets = TestSecrets::new();
    let a = watched(&folder, &secrets);
    a.register_user("alice", "pw", true).unwrap();
    let b = folder.store_with(&secrets);
    assert!(b.is_logged_in());
    let log = ChangeLog::new(&a);
    b.try_record_watch(&video, 3.0, 30.0).unwrap();
    b.try_set_liked(&video, true).unwrap();
    assert!(eventually(|| log.events().contains(&ProfileChange::External)));
    // Applied without anyone calling a.stats(...) first (memory only, no sync).
    assert!(eventually(|| {
        let l = a.inner.lock();
        l.state.session.as_ref().is_some_and(|s| s.contents.videos().values().any(|v| v.is_liked))
    }));
    assert_eq!(a.totals().liked, 1);
    assert!(!log.events().contains(&ProfileChange::Local), "outside changes are not local ones");
}

#[test]
fn another_instances_progress_only_write_is_merged_silently() {
    let folder = TestFolder::new();
    let video = folder.video("watched.mp4");
    let secrets = TestSecrets::new();
    let a = watched(&folder, &secrets);
    a.register_user("alice", "pw", true).unwrap();
    a.try_record_watch(&video, 1.0, 100.0).unwrap();
    let b = folder.store_with(&secrets);
    assert!(b.is_logged_in());
    std::thread::sleep(Duration::from_millis(700));
    let log = ChangeLog::new(&a);
    b.try_update_playback_progress(&video, 60.0, 100.0).unwrap();
    b.try_flush_playback_progress().unwrap();
    b.try_set_thumbnail(&video, b"T").unwrap();
    std::thread::sleep(Duration::from_millis(900));
    assert!(log.events().is_empty(), "{:?}", log.events());
    assert_eq!(a.stats_mode(&video, FingerprintMode::Compute).unwrap().last_position_seconds, 60.0);
    assert_eq!(a.thumbnail(&video), Some(b"T".to_vec()));
}

#[test]
fn a_remembered_login_made_elsewhere_logs_in_an_instance_that_has_no_user_live() {
    let folder = TestFolder::new();
    let secrets = TestSecrets::new();
    let a = watched(&folder, &secrets);
    a.register_user("alice", "pw", false).unwrap();
    a.logout_user().unwrap();
    let b = watched(&folder, &secrets);
    assert!(!b.is_logged_in());
    let log = ChangeLog::new(&b);
    a.login_user("alice", "pw", true).unwrap();
    assert!(eventually(|| log.events().contains(&ProfileChange::External)));
    assert_eq!(b.current_username().as_deref(), Some("alice"));
    assert!(b.is_logged_in());
}

#[test]
fn a_remembered_login_is_also_picked_up_by_the_next_call_without_a_watcher() {
    let folder = TestFolder::new();
    let secrets = TestSecrets::new();
    let a = folder.store_with(&secrets);
    a.register_user("alice", "pw", false).unwrap();
    a.logout_user().unwrap();
    let b = folder.store_with(&secrets);
    assert!(!b.is_logged_in());
    a.login_user("alice", "pw", true).unwrap();
    assert_eq!(b.current_username().as_deref(), Some("alice"));
    let c = folder.store_with(&TestSecrets::new());
    assert!(!c.is_logged_in(), "not if the key is not in this machine's secret store");
}

#[test]
fn logging_out_in_one_instance_logs_out_the_one_that_followed_the_remembered_login() {
    let folder = TestFolder::new();
    let secrets = TestSecrets::new();
    let a = watched(&folder, &secrets);
    a.register_user("alice", "pw", true).unwrap();
    let b = watched(&folder, &secrets);
    assert_eq!(b.current_username().as_deref(), Some("alice"));
    let log = ChangeLog::new(&b);
    a.logout_user().unwrap();
    assert!(eventually(|| log.events().contains(&ProfileChange::External)));
    assert!(!b.is_logged_in());
    assert!(secrets.accounts().is_empty());
    assert_eq!(b.last_username().as_deref(), Some("alice"));
}

#[test]
fn logging_out_elsewhere_is_also_seen_by_the_next_call_without_a_watcher() {
    let folder = TestFolder::new();
    let secrets = TestSecrets::new();
    let a = folder.store_with(&secrets);
    a.register_user("alice", "pw", true).unwrap();
    let b = folder.store_with(&secrets);
    assert!(b.is_logged_in());
    a.logout_user().unwrap();
    assert_eq!(b.current_username(), None);
    assert_eq!(b.try_record_watch(&folder.video("x.mp4"), 0.0, 0.0).unwrap(), None, "writes by a logged-out instance are no-ops");
}

#[test]
fn an_instance_logged_in_without_remember_me_is_not_logged_out_by_an_unattributable_change() {
    let folder = TestFolder::new();
    let secrets = TestSecrets::new();
    let a = folder.store_with(&secrets);
    a.register_user("alice", "pw", true).unwrap();
    let b = folder.store_with(&secrets);
    assert!(b.is_logged_in());
    b.login_user("alice", "pw", false).unwrap();
    assert_eq!(a.current_username(), None, "that did end the remembered login for a");
    a.login_user("alice", "pw", true).unwrap();
    assert_eq!(b.current_username().as_deref(), Some("alice"));
    assert_eq!(b.remembered_username().as_deref(), Some("alice"));
    a.logout_user().unwrap();
    assert_eq!(b.current_username().as_deref(), Some("alice"));
    assert_eq!(b.remembered_username(), None);
    let still = folder.video("still-works.mp4");
    b.try_record_watch(&still, 0.0, 0.0).unwrap();
    assert_eq!(b.stats_mode(&still, FingerprintMode::Compute).unwrap().view_count, 1);
}

#[test]
fn a_store_without_any_user_is_not_logged_in_by_its_own_logout() {
    let folder = TestFolder::new();
    let secrets = TestSecrets::new();
    let a = folder.store_with(&secrets);
    a.register_user("alice", "pw", true).unwrap();
    a.logout_user().unwrap();
    a.refresh();
    assert!(!a.is_logged_in());
    assert!(!folder.store_with(&secrets).is_logged_in());
}

#[test]
fn logout_always_wins_over_a_remembered_login_made_elsewhere() {
    // UNIFIED.md §3.11: a store whose user logged out never restores a remembered login until it logs in again.
    let folder = TestFolder::new();
    let secrets = TestSecrets::new();
    let a = folder.store_with(&secrets);
    a.register_user("alice", "pw", false).unwrap();
    a.logout_user().unwrap();
    let b = folder.store_with(&secrets);
    b.login_user("alice", "pw", true).unwrap();
    a.refresh();
    assert!(!a.is_logged_in(), "the user of `a` logged out in this run");
    assert!(folder.store_with(&secrets).is_logged_in(), "a fresh instance still follows the remembered login");
    a.login_user("alice", "pw", false).unwrap();
    assert!(a.is_logged_in(), "an explicit login is always possible");
}

#[test]
fn a_new_login_lifts_the_logout_block_on_remembered_restores() {
    // RUST: Windows `user_logged_out_this_session` is reset by a successful login or registration (win-profiles §6);
    // the shared rule of UNIFIED.md §3.11 follows it.
    let folder = TestFolder::new();
    let secrets = TestSecrets::new();
    let a = folder.store_with(&secrets);
    a.register_user("alice", "pw", false).unwrap();
    a.logout_user().unwrap();
    a.register_user("veli", "pw", false).unwrap();
    let b = folder.store_with(&secrets);
    b.login_user("alice", "pw", true).unwrap();
    assert_eq!(a.current_username().as_deref(), Some("veli"), "a has its own user, nothing to restore");
    b.delete_user("veli", "pw").unwrap();
    a.refresh();
    assert_eq!(a.current_username().as_deref(), Some("alice"), "a logged in again since its logout, so it follows");
}

#[test]
fn a_profile_typed_in_decomposed_form_keeps_one_picture_folder() {
    // Swift hashes the picture folder from the name lowercased as typed (no NFC); deleting with the same spelling
    // removes that folder.
    let folder = TestFolder::new();
    let store = folder.make_store();
    let name = "Ze\u{301}ynep";
    store.register_user(name, "pw", false).unwrap();
    let video = folder.video("pic.mp4");
    std::fs::write(&video, b"some video bytes").unwrap();
    assert!(store.try_set_thumbnail(&video, b"PIC").unwrap());
    let expected = folder.url.join("thumbnails").join(ThumbnailCache::folder_name(&name.to_lowercase()));
    assert!(expected.is_dir(), "folder named from the raw lowercased name");
    assert_eq!(store.thumbnail(&video), Some(b"PIC".to_vec()));
    store.delete_user(name, "pw").unwrap();
    assert!(!expected.exists());
    assert!(folder.cache_files().is_empty());
}

#[test]
fn deleting_the_active_profile_elsewhere_locks_this_instance() {
    let folder = TestFolder::new();
    let a = folder.make_store();
    let b = folder.make_store();
    a.register_user("alice", "pw", false).unwrap();
    b.login_user("alice", "pw", false).unwrap();
    a.delete_user("alice", "pw").unwrap();
    assert!(!b.is_logged_in());
    assert!(b.registered_usernames().is_empty());
}

#[test]
fn each_change_stream_gets_its_own_events() {
    let folder = TestFolder::new();
    let store = folder.make_store();
    let first = ChangeLog::new(&store);
    let second = ChangeLog::new(&store);
    store.register_user("alice", "pw", false).unwrap();
    store.logout_user().unwrap();
    let both = vec![ProfileChange::Local, ProfileChange::Local];
    assert!(eventually(|| first.events() == both && second.events() == both));
    first.stop();
    store.login_user("alice", "pw", false).unwrap();
    assert!(eventually(|| second.events().len() == 3));
    assert_eq!(first.events().len(), 2);
}

#[test]
fn trait_subscribers_get_the_same_signals() {
    let folder = TestFolder::new();
    let store = folder.make_store();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    let subscription = store.subscribe_changes(Box::new(move |c| sink.lock().push(c)));
    store.register_user("alice", "pw", false).unwrap();
    assert_eq!(*seen.lock(), vec![ProfileChange::Local]);
    drop(subscription);
    store.logout_user().unwrap();
    assert_eq!(seen.lock().len(), 1, "dropping the subscription unsubscribes");
}
