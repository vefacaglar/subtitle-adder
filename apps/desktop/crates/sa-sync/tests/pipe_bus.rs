//! Windows named-pipe broker (win-sync §3–§8, §16): the broker parts of the C# `LocalProfileTests` plus the new
//! Rust tests (orphan slots, outbound cap, F1–F6, host takeover from a second process).
#![cfg(windows)]

mod common;

use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use common::{FakeStore, Recorder, created_stores, fake_factory, test_scope, wait_until};
use crossbeam_channel::{Receiver, unbounded};
use sa_sync::win_testing::{self, RawClient};
use sa_sync::{
    EventKind, NamedPipeBus, ProfileWriteBroker, StateSyncEvent, StateSyncWriteRequest, SyncBus, Topic, TopicFilter,
    WriteKind, broker_dir, is_channel_engine_running, scope_hash, total_active_in_process_brokers, wire,
};

const READY: Duration = Duration::from_secs(5);

fn ev(kind: EventKind) -> StateSyncEvent {
    StateSyncEvent::new(Topic::Profiles, kind, "manager-x", r"C:\data\profiles.json")
}

#[test]
fn five_concurrent_instances_communicate_and_last_window_closes_engine() {
    let scope = test_scope("five-window");
    assert!(!is_channel_engine_running(Some(&scope)));
    let first = NamedPipeBus::new(Some(&scope));
    assert!(first.is_engine_running());
    assert!(is_channel_engine_running(Some(&scope)));
    assert_eq!(first.active_instance_count(), 1);
    assert!(first.is_last_instance());
    assert_eq!(first.channel_scope(), scope);

    let mut brokers = vec![first];
    for _ in 0..4 {
        brokers.push(NamedPipeBus::new(Some(&scope)));
    }
    for b in &brokers {
        assert!(b.wait_until_ready(READY));
    }
    assert!(wait_until(3000, || brokers[0].connected_client_count() == 5));
    for b in &brokers {
        assert_eq!(b.active_instance_count(), 5);
        assert!(b.is_engine_running() && b.is_connected_to_leader() && b.has_other_active_instances());
        assert!(!b.is_last_instance());
        assert!(b.is_leader(), "one process: its engine hosts");
    }

    let recorders: Vec<Recorder> = brokers.iter().map(|_| Recorder::default()).collect();
    let subs: Vec<_> = brokers.iter().zip(&recorders).map(|(b, r)| b.subscribe(TopicFilter::All, r.handler())).collect();
    let e = ev(EventKind::VideoWatched);
    brokers[2].publish(Topic::Profiles, e.clone());
    for r in &recorders {
        assert!(wait_until(3000, || r.count_id(&e.event_id) == 1));
    }
    std::thread::sleep(Duration::from_millis(100));
    for r in &recorders {
        assert_eq!(r.count_id(&e.event_id), 1, "delivered exactly once per broker");
    }

    let b0 = brokers.remove(0);
    b0.dispose();
    assert!(!b0.closed_engine_on_dispose());
    assert!(brokers[0].is_engine_running());
    assert_eq!(brokers[0].active_instance_count(), 4);
    assert!(wait_until(3000, || brokers[0].connected_client_count() == 4));

    let e2 = ev(EventKind::ListChanged);
    brokers[1].publish(Topic::Profiles, e2.clone());
    for r in &recorders[1..] {
        assert!(wait_until(3000, || r.count_id(&e2.event_id) == 1));
    }
    assert_eq!(recorders[0].count_id(&e2.event_id), 0, "disposed broker is silent");

    let last = brokers.pop().unwrap();
    for (i, b) in brokers.iter().enumerate() {
        b.dispose();
        assert!(!b.closed_engine_on_dispose());
        assert!(b.is_engine_running());
        assert_eq!(last.active_instance_count(), 4 - (i + 1));
    }
    assert!(last.is_last_instance());
    assert!(!last.has_other_active_instances());
    assert!(last.is_engine_running());
    last.dispose();
    assert!(last.closed_engine_on_dispose());
    assert!(!last.is_engine_running());
    assert!(!is_channel_engine_running(Some(&scope)));
    assert!(!broker_dir(&std::env::temp_dir(), &scope).exists(), "empty broker dir removed");
    drop(subs);
}

#[test]
fn one_shared_store_across_windows_idempotent_requests_and_last_closes_it() {
    let scope = test_scope("data-worker");
    let b1 = NamedPipeBus::new(Some(&scope));
    b1.set_data_worker_factory(fake_factory());
    let b2 = NamedPipeBus::new(Some(&scope));
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("profiles.db");
    let s1 = b1.data_worker_store(&db);
    let s2 = b2.data_worker_store(Path::new(&db.to_string_lossy().to_uppercase()));
    assert!(Arc::ptr_eq(&s1, &s2));
    let store: Arc<FakeStore> =
        created_stores().lock().iter().find(|s| s.path == std::path::absolute(&db).unwrap()).unwrap().clone();

    let mut req = StateSyncWriteRequest::new(b2.instance_id(), &db.to_string_lossy(), WriteKind::UpsertVideo);
    req.key = "video-key".into();
    req.blob = Some("AQID".into());
    req.view_count = 99;
    req.view_count_delta = 1;
    b1.submit_write_request(req.clone());
    b2.submit_write_request(req.clone());
    assert!(b2.flush_pending_writes(&db, None));
    assert!(store.duplicates.load(Ordering::SeqCst) >= 1);
    assert!(store.is_processed(&req.request_id));
    assert_eq!(b1.pending_write_request_count() + b2.pending_write_request_count(), 0);

    b1.dispose();
    assert!(!b1.closed_engine_on_dispose());
    assert!(!store.disposed.load(Ordering::SeqCst));
    let later = StateSyncWriteRequest::new(b2.instance_id(), &db.to_string_lossy(), WriteKind::UpsertWatchProgress);
    b2.submit_write_request(later.clone());
    assert!(b2.flush_pending_writes(&db, Some(Duration::from_secs(5))));
    b2.dispose();
    assert!(b2.closed_engine_on_dispose());
    assert!(store.disposed.load(Ordering::SeqCst));
    assert!(store.is_processed(&later.request_id));
    assert!(!b2.flush_pending_writes(&db, None), "disposed → false");
}

#[test]
fn orphan_slot_files_are_deleted_and_live_ones_counted() {
    let scope = test_scope("orphans");
    let instances = broker_dir(&std::env::temp_dir(), &scope).join("instances");
    std::fs::create_dir_all(&instances).unwrap();
    let orphan = instances.join("instance-0123456789abcdef0123456789abcdef.slot");
    std::fs::write(&orphan, b"").unwrap();
    // A slot held exclusively by "another process".
    let held_path = instances.join("instance-fedcba9876543210fedcba9876543210.slot");
    let held = OpenOptions::new().read(true).write(true).create(true).truncate(true).share_mode(0).open(&held_path).unwrap();
    let unrelated = instances.join("notes.txt");
    std::fs::write(&unrelated, b"x").unwrap();

    let b = NamedPipeBus::new(Some(&scope));
    assert_eq!(b.active_instance_count(), 2);
    assert!(!orphan.exists());
    assert!(unrelated.exists());
    assert!(b.has_other_active_instances());
    drop(held);
    assert_eq!(b.active_instance_count(), 1, "released slot becomes an orphan");
    assert!(!held_path.exists());
    std::fs::remove_file(&unrelated).unwrap();
    let slot = instances.join(format!("instance-{}.slot", b.instance_id()));
    assert!(slot.exists());
    b.dispose();
    assert!(!slot.exists(), "delete-on-close");
}

#[test]
fn outbound_queue_keeps_the_newest_256_lines_until_a_server_exists() {
    let scope = test_scope("outbound");
    let base = broker_dir(&std::env::temp_dir(), &scope);
    std::fs::create_dir_all(&base).unwrap();
    // Hold the engine lock: no process hosts, nobody can connect.
    let lock_path = base.join(format!("engine-{}.lock", scope_hash(&scope)));
    let lock = OpenOptions::new().read(true).write(true).create(true).share_mode(0).open(&lock_path).unwrap();

    let sender = NamedPipeBus::new(Some(&scope));
    let receiver = NamedPipeBus::new(Some(&scope));
    let rec = Recorder::default();
    let _s = receiver.subscribe(TopicFilter::All, rec.handler());
    std::thread::sleep(Duration::from_millis(100));
    assert!(!sender.is_ready() && !sender.is_leader());
    let ids: Vec<String> = (0..300)
        .map(|_| {
            let e = ev(EventKind::ListChanged);
            let id = e.event_id.clone();
            sender.publish(Topic::Profiles, e);
            id
        })
        .collect();
    assert_eq!(sender.queued_outbound_count(), 256);

    drop(lock);
    assert!(sender.wait_until_ready(READY));
    assert!(receiver.wait_until_ready(READY));
    assert!(sender.is_leader());
    assert!(wait_until(3000, || sender.queued_outbound_count() == 0));
    std::thread::sleep(Duration::from_millis(200));
    let got = rec.ids();
    for dropped in &ids[..44] {
        assert!(!got.contains(dropped), "oldest lines are dropped");
    }
    for id in &got {
        assert!(ids[44..].contains(id));
    }
    sender.dispose();
    receiver.dispose();
}

#[test]
fn pipe_dacl_allows_only_the_current_user_and_system() {
    let scope = test_scope("dacl");
    let sddl = win_testing::server_instance_sddl(&scope).unwrap();
    let expected = win_testing::expected_server_sddl().unwrap();
    let user = win_testing::current_user_sid().unwrap();
    let user_alias = win_testing::current_user_sddl_alias().unwrap();
    let owner = expected.strip_prefix("O:").unwrap().split("D:").next().unwrap().to_owned();
    let owner_alias = win_testing::current_owner_sddl_alias().unwrap();
    assert!(
        sddl.starts_with(&format!("O:{owner}D:")) || sddl.starts_with(&format!("O:{owner_alias}D:")),
        "{sddl}"
    );
    let dacl = sddl.split("D:").nth(1).unwrap();
    assert!(dacl.starts_with('P'), "protected DACL: {sddl}");
    let aces: Vec<&str> = dacl.trim_start_matches(|c: char| c.is_ascii_uppercase()).split(')').filter(|s| !s.is_empty()).collect();
    assert!(!aces.is_empty());
    for ace in aces {
        let ace = ace.trim_start_matches('(');
        let parts: Vec<&str> = ace.split(';').collect();
        assert_eq!(parts[0], "A", "{ace}");
        let trustee = parts[5];
        assert!(
            trustee == owner || trustee == owner_alias || trustee == user || trustee == user_alias || trustee == "SY",
            "unexpected trustee {trustee} in {sddl}"
        );
    }
    for forbidden in [";WD)", ";AN)", ";BU)", ";AU)", ";IU)", ";NU)"] {
        assert!(!sddl.contains(forbidden), "{sddl}");
    }
}

#[test]
fn server_relays_lines_and_executes_wire_write_requests() {
    let scope = test_scope("raw-wire");
    let bus = NamedPipeBus::new(Some(&scope));
    bus.set_data_worker_factory(fake_factory());
    assert!(bus.wait_until_ready(READY));
    let rec = Recorder::default();
    let _s = bus.subscribe(TopicFilter::All, rec.handler());
    let mut a = RawClient::connect(&scope, READY).unwrap();
    let mut b = RawClient::connect(&scope, READY).unwrap();
    assert!(wait_until(3000, || bus.connected_client_count() == 3));

    // Un-prefixed lines are relayed verbatim to every other client (garbage too), never echoed to the sender.
    assert!(a.send_line("not json at all"));
    assert_eq!(b.read_line(READY).as_deref(), Some("not json at all"));
    let e = ev(EventKind::ProfileLogin);
    let line = wire::event_line(&e).unwrap();
    assert!(a.send_line(&line));
    assert_eq!(b.read_line(READY).as_deref(), Some(line.as_str()));
    assert!(wait_until(3000, || rec.count_id(&e.event_id) == 1));
    assert_eq!(a.read_line(Duration::from_millis(200)), None, "no echo to the sender");

    // A broker publish reaches raw clients.
    let p = ev(EventKind::ProfileLogout);
    bus.publish(Topic::Profiles, p.clone());
    let got = a.read_line(READY).unwrap();
    assert_eq!(wire::parse_event(&got).unwrap().event_id, p.event_id);
    let _ = b.read_line(READY);

    // W: requests run on the host's store and are acked to that client only.
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("profiles.db");
    let mut req = StateSyncWriteRequest::new("external-window", &db.to_string_lossy(), WriteKind::UpsertVideo);
    let pc = ev(EventKind::VideoWatched);
    req.post_commit_events = Some(vec![pc.clone()]);
    assert!(a.send_line(&wire::write_request_line(&req).unwrap()));
    let ack = a.read_line(READY).unwrap();
    // The host's post-commit broadcast may arrive before the ack.
    let ack = if ack.starts_with("A:") { ack } else { a.read_line(READY).unwrap() };
    let ack = wire::parse_ack(ack.strip_prefix("A:").unwrap()).unwrap();
    assert!(ack.committed && !ack.is_duplicate && ack.error.is_none());
    assert!(wait_until(3000, || rec.count_id(&pc.event_id) == 1), "post-commit event dispatched in process");
    let relayed = b.read_line(READY).unwrap();
    assert_eq!(wire::parse_event(&relayed).unwrap().event_id, pc.event_id, "post-commit event broadcast to clients");

    assert!(a.send_line(&wire::write_request_line(&req).unwrap()));
    let dup = loop {
        let l = a.read_line(READY).unwrap();
        if let Some(j) = l.strip_prefix("A:") {
            break wire::parse_ack(j).unwrap();
        }
    };
    assert!(dup.committed && dup.is_duplicate);

    let mut fb = StateSyncWriteRequest::new("external-window", &db.to_string_lossy(), WriteKind::FlushBarrier);
    fb.request_id = "fb-1".into();
    assert!(a.send_line(&wire::write_request_line(&fb).unwrap()));
    let nack = loop {
        let l = a.read_line(READY).unwrap();
        if let Some(j) = l.strip_prefix("A:") {
            break wire::parse_ack(j).unwrap();
        }
    };
    assert_eq!(nack, sa_sync::StateSyncWriteAck::new("fb-1", false, false, Some("Unknown WriteKind 'FlushBarrier'.".into())));
    a.close();
    b.close();
    bus.dispose();
}

#[test]
fn server_reads_cr_crlf_and_an_unterminated_last_line() {
    let scope = test_scope("framing");
    let bus = NamedPipeBus::new(Some(&scope));
    assert!(bus.wait_until_ready(READY));
    let a = RawClient::connect(&scope, READY).unwrap();
    let mut b = RawClient::connect(&scope, READY).unwrap();
    assert!(wait_until(3000, || bus.connected_client_count() == 3));
    assert!(a.send_raw(b"one\rtwo\r\nthree\n  \n\r\nfour"));
    for expected in ["one", "two", "three"] {
        assert_eq!(b.read_line(READY).as_deref(), Some(expected));
    }
    // "four" has no terminator: relayed only once the sender disconnects (end of stream).
    assert_eq!(b.read_line(Duration::from_millis(150)), None);
    std::thread::sleep(Duration::from_millis(100));
    a.close();
    drop(a); // the last handle reference: the host sees end of stream
    assert_eq!(b.read_line(READY).as_deref(), Some("four"));
    b.close();
    bus.dispose();
}

#[test]
fn shared_broker_and_process_counters() {
    let before = total_active_in_process_brokers();
    let scope = test_scope("counters");
    let b = NamedPipeBus::new(Some(&scope));
    assert!(total_active_in_process_brokers() > before);
    b.dispose();
    b.dispose();
    assert_eq!(b.active_instance_count(), 0);
    assert!(!b.has_other_active_instances());
    // Disposed broker: no-ops.
    b.publish(Topic::Profiles, ev(EventKind::ListChanged));
    assert_eq!(b.queued_outbound_count(), 0);
    let scope_default = sa_sync::default_scope();
    assert!(scope_default.contains('\\'), "{scope_default}");
}

// ------------------------------------------------------------------------------------------------ second process

/// Child entry point: only active when the parent sets `SA_SYNC_CHILD_SCOPE`.
#[test]
#[ignore = "child process entry, run by the cross-process tests"]
fn child_process_entry() {
    let Ok(scope) = std::env::var("SA_SYNC_CHILD_SCOPE") else { return };
    let bus = NamedPipeBus::new(Some(&scope));
    bus.set_data_worker_factory(fake_factory());
    let out = Arc::new(parking_lot::Mutex::new(std::io::stdout()));
    let out2 = out.clone();
    let _s = bus.subscribe(
        TopicFilter::All,
        Box::new(move |e: &StateSyncEvent| {
            let mut o = out2.lock();
            let _ = writeln!(o, "SA-RECV {}", e.event_id);
            let _ = o.flush();
        }),
    );
    let ready = bus.wait_until_ready(READY);
    {
        let mut o = out.lock();
        let _ = writeln!(o, "SA-READY {ready} leader={}", bus.is_leader());
        let _ = o.flush();
    }
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let parts: Vec<&str> = line.split_whitespace().collect();
        match parts.as_slice() {
            ["publish", id] => {
                let mut e = ev(EventKind::ListChanged);
                e.event_id = (*id).to_owned();
                bus.publish(Topic::Profiles, e);
            }
            ["submit", db, id] => {
                let mut req = StateSyncWriteRequest::new("child-manager", db, WriteKind::UpsertVideo);
                let mut e = ev(EventKind::VideoWatched);
                e.event_id = (*id).to_owned();
                req.post_commit_events = Some(vec![e]);
                bus.submit_write_request(req);
            }
            ["status"] => {
                let mut o = out.lock();
                let _ = writeln!(
                    o,
                    "SA-STATUS leader={} ready={} count={} clients={}",
                    bus.is_leader(),
                    bus.is_ready(),
                    bus.active_instance_count(),
                    bus.connected_client_count()
                );
                let _ = o.flush();
            }
            ["quit"] => break,
            _ => {}
        }
    }
    bus.dispose();
}

struct ChildProc {
    child: Child,
    lines: Receiver<String>,
}

impl ChildProc {
    fn start(scope: &str) -> ChildProc {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["child_process_entry", "--exact", "--ignored", "--nocapture", "--test-threads=1"])
            .env("SA_SYNC_CHILD_SCOPE", scope)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let (tx, rx) = unbounded();
        let stdout = child.stdout.take().unwrap();
        std::thread::spawn(move || {
            for l in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Some(i) = l.find("SA-") {
                    let _ = tx.send(l[i..].to_owned());
                }
            }
        });
        ChildProc { child, lines: rx }
    }

    fn send(&mut self, cmd: &str) {
        let stdin = self.child.stdin.as_mut().unwrap();
        writeln!(stdin, "{cmd}").unwrap();
        stdin.flush().unwrap();
    }

    /// Polls `status` (every 20 ms, up to 5 s) until `pred` holds.
    fn status_until(&mut self, pred: impl Fn(&str) -> bool) -> bool {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            self.send("status");
            let st = self.expect("SA-STATUS", READY).unwrap_or_default();
            if pred(&st) {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                eprintln!("last child status: {st}");
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn expect(&self, prefix: &str, timeout: Duration) -> Option<String> {
        let deadline = std::time::Instant::now() + timeout;
        while let Some(left) = deadline.checked_duration_since(std::time::Instant::now()) {
            match self.lines.recv_timeout(left) {
                Ok(l) if l.starts_with(prefix) => return Some(l),
                Ok(_) => continue,
                Err(_) => return None,
            }
        }
        None
    }
}

impl Drop for ChildProc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn hosting_moves_to_the_remaining_process_when_the_host_dies() {
    let scope = test_scope("takeover");
    let mut child = ChildProc::start(&scope);
    let ready = child.expect("SA-READY", Duration::from_secs(20)).expect("child ready");
    assert_eq!(ready, "SA-READY true leader=true");

    let bus = NamedPipeBus::new(Some(&scope));
    assert!(bus.wait_until_ready(READY));
    assert!(!bus.is_leader(), "the child hosts");
    assert_eq!(bus.connected_client_count(), 0);
    assert_eq!(bus.active_instance_count(), 2);
    assert!(bus.has_other_active_instances());
    let rec = Recorder::default();
    let _s = bus.subscribe(TopicFilter::All, rec.handler());
    // The host relays only to sessions it has registered; wait until it sees both clients.
    assert!(child.status_until(|s| s.ends_with("count=2 clients=2")), "child host registered both sessions");
    child.send("publish from-child-1");
    assert!(wait_until(3000, || rec.count_id("from-child-1") == 1));
    let mine = ev(EventKind::ProfileLogin);
    bus.publish(Topic::Profiles, mine.clone());
    assert!(child.expect(&format!("SA-RECV {}", mine.event_id), READY).is_some());

    // Crash the host: the OS releases its lock and slot; this process takes over.
    let _ = child.child.kill();
    let _ = child.child.wait();
    assert!(wait_until(5000, || bus.is_leader() && bus.is_ready()));
    assert_eq!(bus.active_instance_count(), 1);
    assert!(bus.is_last_instance());
    bus.dispose();
    assert!(bus.closed_engine_on_dispose());
}

#[test]
fn post_commit_events_of_a_non_hosting_process_reach_other_processes() {
    // F1: in C# a non-hosting process never broadcast its post-commit events.
    let scope = test_scope("f1");
    let host = NamedPipeBus::new(Some(&scope));
    assert!(host.wait_until_ready(READY));
    assert!(host.is_leader());
    let rec = Recorder::default();
    let _s = host.subscribe(TopicFilter::All, rec.handler());
    let mut child = ChildProc::start(&scope);
    let ready = child.expect("SA-READY", Duration::from_secs(20)).expect("child ready");
    assert_eq!(ready, "SA-READY true leader=false");
    assert!(wait_until(3000, || host.active_instance_count() == 2));
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("child.db");
    child.send(&format!("submit {} pc-from-child", db.display()));
    assert!(child.expect("SA-RECV pc-from-child", READY).is_some(), "dispatched in the child process");
    assert!(wait_until(3000, || rec.count_id("pc-from-child") == 1), "relayed to the host process");
    child.send("quit");
    assert!(wait_until(5000, || host.active_instance_count() == 1));
    host.dispose();
    assert!(host.closed_engine_on_dispose());
}

/// The C# broker writes synchronously, so everything published before `Dispose` is in the pipe when it returns; the
/// queued writer (F5) must keep that: a burst published right before dispose (with a line far larger than the pipe
/// buffer) still reaches the other window.
#[test]
fn lines_published_right_before_dispose_still_reach_the_others() {
    let scope = test_scope("dispose-drain");
    let host = NamedPipeBus::new(Some(&scope));
    assert!(host.wait_until_ready(READY));
    let rec = Recorder::default();
    let _s = host.subscribe(TopicFilter::All, rec.handler());
    let leaving = NamedPipeBus::new(Some(&scope));
    assert!(leaving.wait_until_ready(READY));
    assert!(wait_until(3000, || host.connected_client_count() == 2));
    let mut ids = Vec::new();
    for i in 0..40 {
        let mut e = ev(EventKind::ThumbnailCaptured);
        if i == 20 {
            e.payload_base64 = Some("A".repeat(3 * 1024 * 1024));
        }
        ids.push(e.event_id.clone());
        leaving.publish(Topic::Thumbnail, e);
    }
    let mut logout = ev(EventKind::ProfileLogout);
    logout.event_id = "last-logout".into();
    leaving.publish(Topic::Profiles, logout);
    ids.push("last-logout".into());
    leaving.dispose();
    assert!(!leaving.closed_engine_on_dispose());
    assert!(wait_until(5000, || rec.len() == ids.len()), "received {} of {}", rec.len(), ids.len());
    assert_eq!(rec.ids(), ids, "in publish order");
    host.dispose();
    assert!(host.closed_engine_on_dispose());
}

/// F2: when another process still holds the channel, the last broker of this process does not shut the engine down
/// (C# keeps it for cross-process counting), but it commits and closes this process's stores, which C# skipped (the
/// app then exits and queued SQLite writes were lost).
#[test]
fn the_last_broker_of_a_process_commits_and_closes_its_stores_while_another_process_runs() {
    let scope = test_scope("f2");
    let instances = broker_dir(&std::env::temp_dir(), &scope).join("instances");
    std::fs::create_dir_all(&instances).unwrap();
    // A live slot of "another process" (held exclusively).
    let other = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .share_mode(0)
        .open(instances.join("instance-00000000000000000000000000000f2a.slot"))
        .unwrap();

    let bus = NamedPipeBus::new(Some(&scope));
    bus.set_data_worker_factory(fake_factory());
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("profiles.db");
    let _ = bus.data_worker_store(&db);
    let store: Arc<FakeStore> =
        created_stores().lock().iter().find(|s| s.path == std::path::absolute(&db).unwrap()).unwrap().clone();
    store.hold.store(true, Ordering::SeqCst);
    let req = StateSyncWriteRequest::new("manager-f2", &db.to_string_lossy(), WriteKind::UpsertVideo);
    bus.submit_write_request(req.clone());
    assert!(!store.is_processed(&req.request_id), "held in the store queue");
    assert_eq!(bus.active_instance_count(), 2);

    bus.dispose();
    assert!(!bus.closed_engine_on_dispose(), "another process keeps the channel");
    assert!(is_channel_engine_running(Some(&scope)));
    assert!(store.is_processed(&req.request_id), "queued write committed before the process leaves");
    assert!(store.disposed.load(Ordering::SeqCst), "this process's store released");

    // The other process leaves too: the next broker here is the last one and closes the engine.
    drop(other);
    let again = NamedPipeBus::new(Some(&scope));
    assert_eq!(again.active_instance_count(), 1);
    again.dispose();
    assert!(again.closed_engine_on_dispose());
    assert!(!is_channel_engine_running(Some(&scope)));
}

/// F5: a peer that stops reading never blocks a publisher (C# writes synchronously, so its publisher would hang); the
/// host disconnects it once its bounded queue overflows, and healthy peers keep receiving every line in order.
#[test]
fn a_stalled_peer_never_blocks_publishers_and_is_disconnected() {
    let scope = test_scope("f5");
    let host = NamedPipeBus::new(Some(&scope));
    assert!(host.wait_until_ready(READY));
    let other = NamedPipeBus::new(Some(&scope));
    assert!(other.wait_until_ready(READY));
    let rec = Recorder::default();
    let _s = other.subscribe(TopicFilter::All, rec.handler());
    let stalled = RawClient::connect_stalled(&scope, READY).unwrap();
    assert!(wait_until(3000, || host.connected_client_count() == 3));

    let mut ids = Vec::new();
    let mut slowest = Duration::ZERO;
    for i in 0..1600 {
        let mut e = ev(EventKind::ThumbnailCaptured);
        e.payload_base64 = Some("A".repeat(512));
        ids.push(e.event_id.clone());
        let t = std::time::Instant::now();
        host.publish(Topic::Thumbnail, e);
        slowest = slowest.max(t.elapsed());
        // Paced like real producers: the publisher's own connection never overflows.
        if i % 50 == 49 {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    assert!(slowest < Duration::from_millis(250), "a publish blocked for {slowest:?}");
    assert!(wait_until(5000, || host.connected_client_count() == 2), "the stalled peer is disconnected");
    assert!(wait_until(10000, || rec.len() == ids.len()), "received {} of {}", rec.len(), ids.len());
    assert_eq!(rec.ids(), ids, "healthy peers get every line in order");
    stalled.close();
    other.dispose();
    host.dispose();
    assert!(host.closed_engine_on_dispose());
}
