//! In-process transport (win-sync §14) and the transport-independent broker semantics (§7.9, §8).

mod common;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use common::{FakeStore, Recorder, created_stores, fake_factory, test_scope};
use sa_sync::{
    EventKind, InProcessBus, ProfileWriteBroker, StateSyncEvent, StateSyncWriteRequest, SyncBus, Topic, TopicFilter,
    WriteKind,
};

fn ev(topic: Topic, kind: EventKind) -> StateSyncEvent {
    StateSyncEvent::new(topic, kind, "component-a", r"C:\data\profiles.json")
}

#[test]
fn publish_reaches_own_subscribers_synchronously_with_topic_filtering() {
    let bus = InProcessBus::new();
    let all = Recorder::default();
    let progress = Recorder::default();
    let _a = bus.subscribe(TopicFilter::All, all.handler());
    let _p = bus.subscribe(TopicFilter::Only(Topic::WatchProgress), progress.handler());
    bus.publish(Topic::WatchProgress, ev(Topic::Profiles, EventKind::ProgressUpdated));
    bus.publish(Topic::Settings, ev(Topic::Settings, EventKind::SettingsUpdated));
    assert_eq!(all.len(), 2);
    assert_eq!(progress.len(), 1);
    // `publish` overrides the event's topic.
    assert_eq!(progress.0.lock()[0].topic, "watch-progress");
}

#[test]
fn raw_topics_match_case_insensitively_and_blank_means_all() {
    let bus = InProcessBus::new();
    let upper = Recorder::default();
    let blank = Recorder::default();
    let filt = Recorder::default();
    let _u = bus.subscribe_topic("  WATCH-progress ", upper.handler());
    let _b = bus.subscribe_topic("   ", blank.handler());
    let _f = bus.subscribe(TopicFilter::Only(Topic::WatchProgress), filt.handler());
    let mut e = ev(Topic::WatchProgress, EventKind::ProgressUpdated);
    e.topic = "Watch-Progress".into();
    bus.publish_event(e);
    assert_eq!(upper.len(), 1);
    assert_eq!(filt.len(), 1);
    // C# `Publish(evt)` keeps a blank topic (only `*` matches it); `Publish(topic, evt)` trims, blank → profiles.
    let profiles = Recorder::default();
    let _p = bus.subscribe(TopicFilter::Only(Topic::Profiles), profiles.handler());
    let mut e = ev(Topic::Profiles, EventKind::ListChanged);
    e.topic = "  ".into();
    bus.publish_event(e);
    assert_eq!(blank.0.lock()[1].topic, "  ");
    assert_eq!(profiles.len(), 0);
    bus.publish_to("  ", ev(Topic::Settings, EventKind::ListChanged));
    bus.publish_to(" Watch-Progress\t", ev(Topic::Settings, EventKind::ProgressUpdated));
    assert_eq!(upper.len(), 2);
    assert_eq!(filt.len(), 2);
    assert_eq!(profiles.len(), 1);
    assert_eq!(blank.len(), 4);
    assert_eq!(blank.0.lock()[2].topic, "profiles");
    assert_eq!(blank.0.lock()[3].topic, "Watch-Progress");
    // OrdinalIgnoreCase = .NET ordinal casing: U+017F LONG S is not folded into ASCII `S`; `ß` has no one-char
    // uppercase and stays itself.
    assert!(!Topic::Settings.matches("\u{17F}ettings"));
    assert!(!Topic::Settings.matches("settingß"));
    assert!(Topic::Settings.matches("SETTINGS"));
}

#[test]
fn dedupe_by_event_id_with_a_2048_window() {
    let bus = InProcessBus::new();
    let r = Recorder::default();
    let _s = bus.subscribe(TopicFilter::All, r.handler());
    let first = ev(Topic::Profiles, EventKind::ListChanged);
    bus.publish_event(first.clone());
    bus.publish_event(first.clone());
    assert_eq!(r.count_id(&first.event_id), 1);
    // An empty id disables dedupe.
    let mut anon = ev(Topic::Profiles, EventKind::ListChanged);
    anon.event_id.clear();
    bus.publish_event(anon.clone());
    bus.publish_event(anon);
    assert_eq!(r.count_id(""), 2);
    // 2047 newer ids keep the first one inside the window; the 2048th pushes it out.
    for _ in 0..2047 {
        bus.publish_event(ev(Topic::Profiles, EventKind::ListChanged));
    }
    bus.publish_event(first.clone());
    assert_eq!(r.count_id(&first.event_id), 1, "still inside the window");
    bus.publish_event(ev(Topic::Profiles, EventKind::ListChanged));
    bus.publish_event(first.clone());
    assert_eq!(r.count_id(&first.event_id), 2, "evicted after 2048 newer ids");
}

#[test]
fn a_panicking_handler_does_not_stop_the_others() {
    let bus = InProcessBus::new();
    let r = Recorder::default();
    let _p = bus.subscribe(TopicFilter::All, Box::new(|_| panic!("handler failure")));
    let _r = bus.subscribe(TopicFilter::All, r.handler());
    bus.publish(Topic::Profiles, ev(Topic::Profiles, EventKind::ProfileLogin));
    assert_eq!(r.len(), 1);
}

#[test]
fn dropping_the_subscription_unsubscribes() {
    let bus = InProcessBus::new();
    let r = Recorder::default();
    let sub = bus.subscribe(TopicFilter::All, r.handler());
    bus.publish(Topic::Profiles, ev(Topic::Profiles, EventKind::ListChanged));
    drop(sub);
    bus.publish(Topic::Profiles, ev(Topic::Profiles, EventKind::ListChanged));
    assert_eq!(r.len(), 1);
}

#[test]
fn buses_of_one_channel_reach_each_other_once_and_count_instances() {
    let scope = test_scope("inproc-channel");
    let a = InProcessBus::with_scope(&scope);
    let b = InProcessBus::with_scope(&scope);
    let isolated = InProcessBus::new();
    let (ra, rb, ri) = (Recorder::default(), Recorder::default(), Recorder::default());
    let _a = a.subscribe(TopicFilter::All, ra.handler());
    let _b = b.subscribe(TopicFilter::All, rb.handler());
    let _i = isolated.subscribe(TopicFilter::All, ri.handler());
    let e = ev(Topic::Profiles, EventKind::ProfileLogout);
    a.publish(Topic::Profiles, e.clone());
    b.publish_event(e.clone()); // echo: deduped everywhere
    assert_eq!((ra.len(), rb.len(), ri.len()), (1, 1, 0));
    assert_eq!(a.active_instance_count(), 2);
    assert!(a.has_other_active_instances() && !a.is_last_instance());
    assert_eq!(isolated.active_instance_count(), 1);
    assert!(!isolated.has_other_active_instances() && isolated.is_last_instance());
    assert_ne!(a.instance_id(), b.instance_id());
    assert_eq!(a.instance_id().len(), 32);
    assert!(sa_sync::total_active_in_process_brokers() >= 3, "in-process buses count as brokers");

    a.dispose();
    assert!(!a.closed_channel_on_dispose());
    assert_eq!(b.active_instance_count(), 1);
    // A disposed bus is a no-op.
    a.publish(Topic::Profiles, ev(Topic::Profiles, EventKind::ListChanged));
    assert_eq!(rb.len(), 1);
    b.dispose();
    assert!(b.closed_channel_on_dispose());
    assert_eq!(b.active_instance_count(), 0);
    b.dispose(); // idempotent
}

#[test]
fn write_requests_ack_post_commit_dispatch_and_last_bus_disposes_stores() {
    let scope = test_scope("inproc-writes");
    let a = InProcessBus::with_scope(&scope);
    a.set_data_worker_factory(fake_factory());
    let b = InProcessBus::with_scope(&scope);
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("profiles.db");
    // Same store object, case-insensitive full-path key.
    let s1 = a.data_worker_store(&db);
    let s2 = b.data_worker_store(Path::new(&db.to_string_lossy().to_uppercase()));
    assert!(Arc::ptr_eq(&s1, &s2));
    let store: Arc<FakeStore> = created_stores().lock().iter().find(|s| s.path == std::path::absolute(&db).unwrap()).unwrap().clone();

    let (ra, rb) = (Recorder::default(), Recorder::default());
    let _a = a.subscribe(TopicFilter::All, ra.handler());
    let _b = b.subscribe(TopicFilter::All, rb.handler());

    let mut req = StateSyncWriteRequest::new("mgr-a", &db.to_string_lossy(), WriteKind::UpsertVideo);
    let pc = ev(Topic::Profiles, EventKind::VideoWatched);
    req.post_commit_events = Some(vec![pc.clone()]);
    a.submit_write_request(req.clone());
    assert_eq!(a.pending_write_request_count(), 0, "committed ack removes the pending entry");
    assert_eq!((ra.count_id(&pc.event_id), rb.count_id(&pc.event_id)), (1, 1));

    // Same id again: duplicate, post-commit events dropped.
    b.submit_write_request(req.clone());
    assert_eq!(store.duplicates.load(Ordering::SeqCst), 1);
    assert_eq!(rb.len(), 1);

    // Unknown kinds (and FlushBarrier, F4) get a negative ack and stay pending.
    let mut bad = StateSyncWriteRequest::new("mgr-a", &db.to_string_lossy(), WriteKind::FlushBarrier);
    bad.request_id = "bad-1".into();
    a.submit_write_request(bad);
    assert_eq!(a.pending_write_request_count(), 1);
    assert!(!store.is_processed("bad-1"));

    // Held commits: flush pushes and commits them.
    store.hold.store(true, std::sync::atomic::Ordering::SeqCst);
    let held = StateSyncWriteRequest::new("mgr-b", &db.to_string_lossy(), WriteKind::UpsertWatchProgress);
    b.submit_write_request(held.clone());
    assert_eq!(b.pending_write_request_count(), 1);
    assert!(b.flush_pending_writes(&db, None));
    assert!(store.is_processed(&held.request_id));
    assert_eq!(b.pending_write_request_count(), 0);

    a.dispose();
    assert!(!store.disposed.load(Ordering::SeqCst), "other bus still open");
    b.dispose();
    assert!(store.disposed.load(Ordering::SeqCst), "last bus flushes and disposes the stores");
    assert!(!b.flush_pending_writes(&db, None));
}

#[test]
fn held_writes_are_committed_by_the_shutdown_flush() {
    let scope = test_scope("inproc-shutdown");
    let bus = InProcessBus::with_scope(&scope);
    bus.set_data_worker_factory(fake_factory());
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("p.db");
    let _ = bus.data_worker_store(&db);
    let store: Arc<FakeStore> = created_stores().lock().iter().find(|s| s.path == std::path::absolute(&db).unwrap()).unwrap().clone();
    store.hold.store(true, Ordering::SeqCst);
    let req = StateSyncWriteRequest::new("mgr", &db.to_string_lossy(), WriteKind::UpsertVideo);
    bus.submit_write_request(req.clone());
    assert_eq!(bus.pending_write_request_count(), 1);
    bus.dispose();
    assert!(store.is_processed(&req.request_id));
    assert!(store.flushes.load(Ordering::SeqCst) >= 1);
    assert!(store.disposed.load(Ordering::SeqCst));
}

#[test]
fn shared_bus_is_one_object() {
    let a = InProcessBus::shared();
    let b = InProcessBus::shared();
    assert!(Arc::ptr_eq(a, b));
    assert_eq!(a.channel_scope(), sa_sync::default_scope());
}
