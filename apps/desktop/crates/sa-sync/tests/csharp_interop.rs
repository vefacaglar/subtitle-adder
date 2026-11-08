//! Interop with the C# app's real `StateSyncBroker` (linked read-only into `tools/fixtures/BrokerInterop`): both
//! hosting directions, events both ways, `W:`/`A:` lines both ways, CurrentUserOnly (F6) on both sides, slot-file
//! counting across runtimes, hosting takeover, STJ round trips of Rust-written JSON and the default scope.
//!
//! The fixture is built with `dotnet build -c Release` on first use. Without a .NET SDK the tests print SKIPPED and
//! pass, unless `SA_SYNC_REQUIRE_CSHARP=1` is set.
#![cfg(windows)]

mod common;

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use common::{Recorder, fake_factory, test_scope, wait_until};
use crossbeam_channel::{Receiver, unbounded};
use sa_sync::win_testing::RawClient;
use sa_sync::wire::{self, BatchListItem, BatchRow};
use sa_sync::{
    EventKind, NamedPipeBus, ProfileWriteBroker, StateSyncEvent, StateSyncWriteRequest, SyncBus, Topic, TopicFilter,
    WriteKind,
};

const READY: Duration = Duration::from_secs(10);

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tools/fixtures/BrokerInterop")
}

/// Path of the built fixture, building it once; `None` = skip.
fn fixture() -> Option<PathBuf> {
    static F: OnceLock<Option<PathBuf>> = OnceLock::new();
    F.get_or_init(|| {
        let exe = fixture_dir().join("bin/Release/net10.0/BrokerInterop.exe");
        let built = Command::new("dotnet")
            .args(["build", "-c", "Release", "--nologo", "-v", "q"])
            .current_dir(fixture_dir())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if (built || exe.exists()) && exe.exists() {
            Some(exe)
        } else {
            assert!(std::env::var("SA_SYNC_REQUIRE_CSHARP").is_err(), "C# interop fixture could not be built");
            eprintln!("SKIPPED: C# interop fixture not available (no .NET SDK?)");
            None
        }
    })
    .clone()
}

struct CSharp {
    child: Child,
    lines: Receiver<String>,
    /// Lines read but not consumed yet (an expectation never discards other lines).
    backlog: std::cell::RefCell<Vec<String>>,
}

impl CSharp {
    fn start(exe: &PathBuf, scope: &str) -> CSharp {
        let mut child = Command::new(exe)
            .arg(scope)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("start fixture");
        let (tx, rx) = unbounded();
        let out = child.stdout.take().unwrap();
        std::thread::spawn(move || {
            for l in BufReader::new(out).lines().map_while(Result::ok) {
                let _ = tx.send(l);
            }
        });
        CSharp { child, lines: rx, backlog: Default::default() }
    }

    fn send(&mut self, line: &str) {
        let stdin = self.child.stdin.as_mut().unwrap();
        writeln!(stdin, "{line}").unwrap();
        stdin.flush().unwrap();
    }

    fn expect(&self, prefix: &str, timeout: Duration) -> String {
        self.expect_where(prefix, timeout, |_| true)
    }

    /// The first line (backlog first) with `prefix` whose rest satisfies `pred`; other lines stay in the backlog.
    fn expect_where(&self, prefix: &str, timeout: Duration, pred: impl Fn(&str) -> bool) -> String {
        let take = |l: &str| l.strip_prefix(prefix).map(|r| r.trim_start().to_owned()).filter(|r| pred(r));
        {
            let mut b = self.backlog.borrow_mut();
            if let Some(i) = b.iter().position(|l| take(l).is_some()) {
                let l = b.remove(i);
                return take(&l).unwrap();
            }
        }
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline
                .checked_duration_since(Instant::now())
                .unwrap_or_else(|| panic!("timed out waiting for {prefix}; unconsumed: {:?}", self.backlog.borrow()));
            match self.lines.recv_timeout(left) {
                Ok(l) if l.starts_with("CS-ERR") => panic!("fixture error: {l}"),
                Ok(l) => {
                    if let Some(r) = take(&l) {
                        return r;
                    }
                    self.backlog.borrow_mut().push(l);
                }
                Err(_) => panic!("timed out waiting for {prefix}; unconsumed: {:?}", self.backlog.borrow()),
            }
        }
    }

    /// Polls `status` (every 50 ms, up to 5 s) until `pred` holds.
    fn status_until(&mut self, pred: impl Fn(&str) -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            self.send("status");
            let st = self.expect("CS-STATUS", READY);
            if pred(&st) {
                return true;
            }
            if Instant::now() >= deadline {
                eprintln!("last C# status: {st}");
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// The next `CS-RECV` event with that id.
    fn expect_event(&self, id: &str, timeout: Duration) -> StateSyncEvent {
        let json = self.expect_where("CS-RECV", timeout, |j| wire::parse_event(j).is_some_and(|e| e.event_id == id));
        wire::parse_event(&json).unwrap()
    }
}

impl Drop for CSharp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn rich_event() -> StateSyncEvent {
    let mut e = StateSyncEvent::new(Topic::WatchProgress, EventKind::ProgressUpdated, "rust-manager", r"C:\Users\Şule\AppData\Local\SubtitleAdder\profiles.json");
    e.username = Some("şule+'<&>".into());
    e.video_key = Some("hk:abc".into());
    e.video_path = Some(r"D:\Videolar\bölüm 1.mp4".into());
    e.position_seconds = Some(77.25);
    e.duration_seconds = Some(100.0);
    e.view_count = Some(3);
    e.is_liked = Some(true);
    e.rating = Some(4);
    e.list_id = Some("list-1".into());
    e.payload_base64 = Some("AQIDBA==".into());
    e.sealed_payload_base64 = Some("c2VhbGVk".into());
    e.timestamp_utc = "2026-11-07T19:22:33.1234567+00:00".into();
    e
}

fn rich_request(db: &str) -> StateSyncWriteRequest {
    let mut r = StateSyncWriteRequest::new("rust-manager", db, WriteKind::ReplaceAllVideosAndProgress);
    r.username = "emre".into();
    r.key = "hk:key".into();
    r.secondary_key = "hk:second".into();
    r.ord = 7;
    r.blob = Some("AAEC".into());
    r.secondary_blob = Some("AwQF".into());
    r.updated_ticks = 639_000_000_000_000_000;
    r.view_count = 2;
    r.view_count_delta = 1;
    r.salt_base64 = Some("c2FsdA==".into());
    r.password_hash_base64 = Some("aGFzaA==".into());
    r.iterations = 600_000;
    r.created_at_utc = "2026-11-07T19:22:33.5+00:00".into();
    r.batch_videos = Some(vec![BatchRow { key: Some("v1".into()), encrypted_blob: Some("AQ==".into()), updated_ticks: 1 }]);
    r.batch_progress = Some(vec![BatchRow { key: Some("p1".into()), encrypted_blob: Some("Ag==".into()), updated_ticks: 2 }]);
    r.batch_list_items = Some(vec![BatchListItem {
        list_id: Some("l".into()),
        item_hash: Some("h".into()),
        ord: 3,
        encrypted_path_blob: Some("Aw==".into()),
    }]);
    let mut pc = rich_event();
    pc.event_id = format!("pc-{}", uuid::Uuid::new_v4().simple());
    r.post_commit_events = Some(vec![pc]);
    r
}

#[test]
fn stj_reads_and_rewrites_rust_json_unchanged_and_scopes_agree() {
    let Some(exe) = fixture() else { return };
    let mut cs = CSharp::start(&exe, &test_scope("interop-roundtrip"));
    cs.expect("CS-READY", READY);

    let e = rich_event();
    cs.send(&format!("roundtrip-event {}", wire::event_line(&e).unwrap()));
    let back = wire::parse_event(&cs.expect("CS-EVT", READY)).unwrap();
    assert_eq!(back, e);

    let r = rich_request(r"C:\data\profiles.db");
    cs.send(&format!("roundtrip-request {}", wire::write_request_line(&r).unwrap().strip_prefix("W:").unwrap()));
    let json = cs.expect("CS-REQ", READY);
    let back = wire::parse_write_request(&json).unwrap();
    let mut expected = r.clone();
    // STJ normalizes DateTimeOffset text ("…33.5+00:00" stays) — compare parsed instants.
    assert_eq!(back.created_at_utc, expected.created_at_utc);
    expected.created_at_utc = back.created_at_utc.clone();
    assert_eq!(back, expected);

    // Minimal request: C# defaults survive the trip.
    let minimal = StateSyncWriteRequest::new("s", "db", WriteKind::UpsertVideo);
    cs.send(&format!("roundtrip-request {}", wire::write_request_line(&minimal).unwrap().strip_prefix("W:").unwrap()));
    assert_eq!(wire::parse_write_request(&cs.expect("CS-REQ", READY)).unwrap(), minimal);

    cs.send("scope");
    assert_eq!(cs.expect("CS-SCOPE", READY), sa_sync::default_scope(), "same default channel scope as .NET");
    cs.send("quit");
    cs.expect("CS-BYE", READY);
}

#[test]
fn csharp_hosts_and_rust_joins_then_takes_over() {
    let Some(exe) = fixture() else { return };
    let scope = test_scope("interop-cs-host");
    let mut cs = CSharp::start(&exe, &scope);
    assert_eq!(cs.expect("CS-READY", READY), "true leader=true");

    let bus = NamedPipeBus::new(Some(&scope));
    assert!(bus.wait_until_ready(READY), "Rust client accepted the C# CurrentUserOnly pipe");
    assert!(!bus.is_leader());
    assert_eq!(bus.active_instance_count(), 2, "C# slot file counted");
    assert!(cs.status_until(|s| s.contains("count=2 clients=2")), "C# host counts the Rust slot and session");

    // Rust → C#
    let e = rich_event();
    bus.publish(Topic::WatchProgress, e.clone());
    assert_eq!(cs.expect_event(&e.event_id, READY), e);

    // C# → Rust
    let rec = Recorder::default();
    let _s = bus.subscribe(TopicFilter::All, rec.handler());
    let mut from_cs = rich_event();
    from_cs.event_id = "from-csharp-1".into();
    from_cs.topic = "Thumbnail".into();
    cs.send(&format!("publish {}", wire::event_line(&from_cs).unwrap()));
    cs.expect("CS-OK", READY);
    assert!(wait_until(3000, || rec.count_id("from-csharp-1") == 1));
    let got = rec.0.lock().iter().find(|x| x.event_id == "from-csharp-1").cloned().unwrap();
    assert_eq!(got, from_cs);

    // Rust W: → C# host store → A:
    let mut raw = RawClient::connect(&scope, READY).unwrap();
    let req = rich_request(r"C:\data\profiles.db");
    assert!(raw.send_line(&wire::write_request_line(&req).unwrap()));
    let ack = next_ack(&mut raw);
    assert!(ack.committed && !ack.is_duplicate, "{ack:?}");
    assert_eq!(ack.request_id, req.request_id);
    let exec = cs.expect("CS-EXEC", READY);
    assert!(exec.starts_with(&format!("ReplaceAllVideosAndProgress {} dup=false blob=AAEC videos=1 items=1", req.request_id)), "{exec}");
    let pc_id = req.post_commit_events.as_ref().unwrap()[0].event_id.clone();
    cs.expect_event(&pc_id, READY);
    if !wait_until(3000, || rec.count_id(&pc_id) == 1) {
        cs.send("status");
        let st = cs.expect("CS-STATUS", READY);
        panic!("C# host did not broadcast the post-commit event to Rust: {:?} ready={} status={st}", rec.ids(), bus.is_ready());
    }
    assert!(raw.send_line(&wire::write_request_line(&req).unwrap()));
    let dup = next_ack(&mut raw);
    assert!(dup.committed && dup.is_duplicate);
    let mut bad = StateSyncWriteRequest::new("rust-manager", r"C:\data\profiles.db", WriteKind::UpsertVideo);
    bad.kind = "Nope".into();
    assert!(raw.send_line(&wire::write_request_line(&bad).unwrap()));
    let nack = next_ack(&mut raw);
    assert_eq!(nack.error.as_deref(), Some("Unknown WriteKind 'Nope'."));
    raw.close();

    // The C# host exits: the Rust engine takes over hosting.
    cs.send("quit");
    assert_eq!(cs.expect("CS-BYE", READY), "closed=false", "the Rust instance keeps the channel alive");
    assert!(wait_until(5000, || bus.is_leader() && bus.is_ready()));
    assert_eq!(bus.active_instance_count(), 1);
    bus.dispose();
    assert!(bus.closed_engine_on_dispose());
}

#[test]
fn rust_hosts_and_csharp_joins() {
    let Some(exe) = fixture() else { return };
    let scope = test_scope("interop-rust-host");
    let bus = NamedPipeBus::new(Some(&scope));
    bus.set_data_worker_factory(fake_factory());
    assert!(bus.wait_until_ready(READY));
    assert!(bus.is_leader());
    let rec = Recorder::default();
    let _s = bus.subscribe(TopicFilter::All, rec.handler());

    let mut cs = CSharp::start(&exe, &scope);
    assert_eq!(cs.expect("CS-READY", READY), "true leader=false", "C# CurrentUserOnly client accepted the Rust pipe (F6)");
    assert!(
        wait_until(3000, || bus.active_instance_count() == 2 && bus.connected_client_count() == 2),
        "count={} clients={}",
        bus.active_instance_count(),
        bus.connected_client_count()
    );
    assert!(cs.status_until(|s| s.starts_with("leader=false ready=true count=2")));

    let e = rich_event();
    bus.publish(Topic::WatchProgress, e.clone());
    assert_eq!(cs.expect_event(&e.event_id, READY), e);

    let mut from_cs = rich_event();
    from_cs.event_id = "from-csharp-2".into();
    cs.send(&format!("publish {}", wire::event_line(&from_cs).unwrap()));
    assert!(wait_until(3000, || rec.count_id("from-csharp-2") == 1));

    // C# W: (STJ-written) → Rust host → A: (read by STJ).
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("profiles.db");
    let req = rich_request(&db.to_string_lossy());
    cs.send(&format!("rawwrite {}", wire::write_request_line(&req).unwrap().strip_prefix("W:").unwrap()));
    let ack = wire::parse_ack(&cs.expect("CS-ACK", READY)).unwrap();
    assert!(ack.committed && !ack.is_duplicate);
    assert_eq!(ack.request_id, req.request_id);
    let pc_id = req.post_commit_events.as_ref().unwrap()[0].event_id.clone();
    assert!(wait_until(3000, || rec.count_id(&pc_id) == 1));
    cs.expect_event(&pc_id, READY);
    let mut fb = StateSyncWriteRequest::new("cs", &db.to_string_lossy(), WriteKind::FlushBarrier);
    fb.request_id = "fb-cs".into();
    cs.send(&format!("rawwrite {}", wire::write_request_line(&fb).unwrap().strip_prefix("W:").unwrap()));
    assert_eq!(
        cs.expect("CS-ACK", READY),
        r#"{"requestId":"fb-cs","committed":false,"isDuplicate":false,"error":"Unknown WriteKind \u0027FlushBarrier\u0027."}"#
    );

    cs.send("quit");
    assert_eq!(cs.expect("CS-BYE", READY), "closed=false");
    assert!(wait_until(3000, || bus.active_instance_count() == 1));
    let _ = bus.flush_pending_writes(&db, None);
    bus.dispose();
    assert!(bus.closed_engine_on_dispose());
}

fn next_ack(raw: &mut RawClient) -> sa_sync::StateSyncWriteAck {
    loop {
        let l = raw.read_line(READY).expect("ack");
        if let Some(j) = l.strip_prefix("A:") {
            return wire::parse_ack(j).unwrap();
        }
    }
}

/// What STJ (via the fixture) made of one input: `None` = dropped (threw, or a JSON `null`), `Some(json)` = its
/// re-serialization.
fn cs_try(cs: &mut CSharp, cmd: &str, json: &str) -> Option<String> {
    cs.send(&format!("{cmd} {json}"));
    cs.expect("CS-TRY", READY).strip_prefix("ok ").map(str::to_owned)
}

/// Removes members STJ fills with fresh random/now values when they are absent from `input`, and folds the one known,
/// behavior-neutral representation difference: Rust models C#'s nullable-in-practice `string` members as `String`, so a
/// JSON `null` there is read as `""` (`topic`: `"profiles"`, C#'s `Topic ?? "profiles"`). Every C# use treats both the
/// same (`IsNullOrEmpty` for ids, ordinal/ignore-case comparisons that neither value can satisfy).
fn strip_generated(mut v: serde_json::Value, input: &str) -> serde_json::Value {
    let inp: serde_json::Value = serde_json::from_str(input).unwrap_or(serde_json::Value::Null);
    if let Some(o) = v.as_object_mut() {
        for k in ["eventId", "requestId", "timestampUtc"] {
            if inp.get(k).is_none() {
                o.remove(k);
            }
        }
        for k in ["eventId", "senderInstanceId", "scopePath", "requestId", "dbPath", "kind"] {
            if o.get(k).is_some_and(|x| x.as_str() == Some("")) && inp.get(k).is_some_and(serde_json::Value::is_null) {
                o.remove(k);
            }
        }
        if o.get("kind").is_some_and(serde_json::Value::is_number) && !o.contains_key("topic") {
            o.insert("topic".into(), "profiles".into());
        }
    }
    v
}

/// Compares the accept/drop decision of STJ and Rust for `input`; when both accept, C# re-reading Rust's output must
/// give what C# made of the original (nothing STJ keeps is lost or changed when Rust re-writes the line).
fn parity(cs: &mut CSharp, cmd: &str, input: &str, rust: Option<String>, mismatches: &mut Vec<String>) {
    let cs_out = cs_try(cs, cmd, input);
    match (&cs_out, &rust) {
        (None, None) => {}
        (Some(c), Some(r)) => {
            let again = cs_try(cs, cmd, r);
            let norm = |s: &str| strip_generated(serde_json::from_str(s).unwrap(), input);
            if again.as_deref().map(norm) != Some(norm(c)) {
                mismatches.push(format!("{cmd} {input}\n   C#: {c}\n   Rust: {r}\n   C# of Rust: {again:?}"));
            }
        }
        _ => mismatches.push(format!("{cmd} {input}\n   C#: {cs_out:?}\n   Rust: {rust:?}")),
    }
}

/// Same accept/drop decision as STJ for every edge case of the three line types.
#[test]
fn rust_accepts_and_drops_exactly_the_lines_stj_does() {
    let Some(exe) = fixture() else { return };
    let mut cs = CSharp::start(&exe, &test_scope("interop-parse"));
    cs.expect("CS-READY", READY);
    let mut mismatches = Vec::new();

    let base = r#""kind":1,"senderInstanceId":"s","scopePath":"p""#;
    let ev = |extra: &str| format!(r#"{{{base},"eventId":"e"{extra}}}"#);
    let mut events: Vec<String> = vec![
        format!("{{{base}}}"),
        r#"{"senderInstanceId":"s","scopePath":"p"}"#.into(),
        r#"{"kind":1,"scopePath":"p"}"#.into(),
        r#"{"kind":1,"senderInstanceId":"s"}"#.into(),
        r#"{"kind":1,"senderInstanceId":null,"scopePath":null,"eventId":"e"}"#.into(),
        r#"{"kind":1.0,"senderInstanceId":"s","scopePath":"p"}"#.into(),
        r#"{"kind":"1","senderInstanceId":"s","scopePath":"p"}"#.into(),
        r#"{"kind":"ProfileLogout","senderInstanceId":"s","scopePath":"p"}"#.into(),
        r#"{"kind":99,"senderInstanceId":"s","scopePath":"p","eventId":"e"}"#.into(),
        r#"{"kind":-5,"senderInstanceId":"s","scopePath":"p","eventId":"e"}"#.into(),
        r#"{"kind":2147483648,"senderInstanceId":"s","scopePath":"p"}"#.into(),
        r#"{"kind":null,"senderInstanceId":"s","scopePath":"p"}"#.into(),
        r#"{"Kind":1,"senderInstanceId":"s","scopePath":"p"}"#.into(),
        r#"{"kind":1,"kind":2,"senderInstanceId":"s","scopePath":"p","eventId":"e"}"#.into(),
        format!(r#"{{{base},"eventId":null}}"#),
        ev(r#","topic":null"#),
        ev(r#","topic":"""#),
        ev(r#","topic":"Watch-Progress""#),
        ev(r#","positionSeconds":77"#),
        ev(r#","positionSeconds":77.5,"durationSeconds":-0.0"#),
        ev(r#","positionSeconds":1e400"#),
        ev(r#","positionSeconds":1e20"#),
        ev(r#","positionSeconds":"77""#),
        ev(r#","positionSeconds":null"#),
        ev(r#","viewCount":1.0"#),
        ev(r#","viewCount":2147483648"#),
        ev(r#","viewCount":-3"#),
        ev(r#","isLiked":1"#),
        ev(r#","isLiked":"true""#),
        ev(r#","rating":5,"listId":"l","videoKey":"k""#),
        ev(r#","username":"\u015Fule\u002B\u0027""#),
        ev(r#","username":"\uD800""#),
        ev(r#","payloadBase64":"not base64!""#),
        ev(r#","sealedPayloadBase64":"c2VhbGVk""#),
        ev(r#","unknown":{"a":[1,2]}"#),
        format!("{}   ", ev("")),
        format!("{} x", ev("")),
        ev(","),
        "null".into(),
        "[]".into(),
        "42".into(),
        // STJ's default MaxDepth is 64 (the event object itself is depth 1).
        ev(&format!(r#","unknown":{}{}"#, "[".repeat(62), "]".repeat(62))),
        ev(&format!(r#","unknown":{}{}"#, "[".repeat(63), "]".repeat(63))),
        ev(&format!(r#","unknown":{}{}"#, "[".repeat(64), "]".repeat(64))),
        ev(&format!(r#","unknown":{}{}"#, "[".repeat(200), "]".repeat(200))),
    ];
    for t in [
        r#""2026-11-07T19:22:33.1234567+00:00""#,
        r#""2026-11-07T19:22:33Z""#,
        r#""2026-11-07T19:22:33z""#,
        r#""2026-11-07T19:22:33.123456789+03:00""#,
        r#""2026-11-07T19:22:33.1234567890123456789Z""#,
        r#""2026-11-07T19:22:33-00:00""#,
        r#""2026-11-07T19:22:33+14:00""#,
        r#""2026-11-07T19:22:33+15:00""#,
        r#""2026-11-07T19:22:33+0300""#,
        r#""2026-11-07T19:22:33+03""#,
        r#""2026-11-07T19:22:33+03:60""#,
        r#""2026-11-07""#,
        r#""2026-11-07T19""#,
        r#""2026-11-07T19:22""#,
        r#""2026-11-07T19:22Z""#,
        r#""2026-11-07T19:22:33""#,
        r#""2026-11-07 19:22:33Z""#,
        r#""2026-11-07t19:22:33Z""#,
        r#""2026-11-07T19:22:33.Z""#,
        r#""2026-11-07T19:22:33,5Z""#,
        r#""2026-11-07T24:00:00Z""#,
        r#""2026-11-07T19:22:60Z""#,
        r#""2026-02-30T00:00:00Z""#,
        r#""0001-01-01T00:00:00+00:00""#,
        r#""0001-01-01T00:00:00+01:00""#,
        r#""9999-12-31T23:59:59.9999999+00:00""#,
        r#""9999-12-31T23:59:59-01:00""#,
        r#""2026-1-07T19:22:33Z""#,
        r#"" 2026-11-07T19:22:33Z""#,
        r#""2026-11-07T19:22:33Z ""#,
        r#""2026-11-07T19:22:33.1234567890123456Z""#,
        r#""2026-11-07T19:22:33.12345678901234567Z""#,
        r#""2026-11-07T19:22:33.1234567890123456+14:00""#,
        r#""2026-11-07T19:22:33.""#,
        r#""2026-11-07T19:22:33.+03:00""#,
        r#""2026-11-07T19:22.5Z""#,
        r#""2026-11-07T19:22:33.5""#,
        r#""2026-11-07T19:22:33+14:30""#,
        r#""2026-11-07T19:22:33-14:00""#,
        r#""2026-11-07T19:22:33+3:00""#,
        r#""2026-11-07T19:22:33-03""#,
        r#""2026-11-07T19:22:33+03:""#,
        r#""2026-11-07T19:22:33+03:0""#,
        r#""2026-11-07T19:22+03""#,
        r#""2026-11-07Z""#,
        r#""2026-11-07+03:00""#,
        r#""2026-11-07T""#,
        r#""0000-01-01T00:00:00Z""#,
        r#""0001-01-01T00:59:59+01:00""#,
        r#""0001-01-01T01:00:00+01:00""#,
        r#""9999-12-31T22:59:59-01:00""#,
        r#""2026-11-07T19:22:33Z""#,
        r#""２026-11-07T19:22:33Z""#,
        r#""""#,
        "null",
        "5",
    ] {
        events.push(ev(&format!(r#","timestampUtc":{t}"#)));
    }
    for e in &events {
        let rust = wire::parse_event(e).and_then(|x| wire::event_line(&x));
        parity(&mut cs, "try-event", e, rust, &mut mismatches);
    }

    let rbase = r#""senderInstanceId":"s","dbPath":"C:\\d\\profiles.db","kind":"UpsertVideo""#;
    let req = |extra: &str| format!(r#"{{"requestId":"r",{rbase}{extra}}}"#);
    let requests: Vec<String> = vec![
        format!("{{{rbase}}}"),
        req(""),
        r#"{"requestId":"r","dbPath":"d","kind":"UpsertVideo"}"#.into(),
        r#"{"requestId":"r","senderInstanceId":"s","kind":"UpsertVideo"}"#.into(),
        r#"{"requestId":"r","senderInstanceId":"s","dbPath":"d"}"#.into(),
        r#"{"requestId":"r","senderInstanceId":"s","dbPath":"d","kind":2}"#.into(),
        req(r#","blob":"AQID","secondaryBlob":"""#),
        req(r#","blob":"AQI""#),
        req(r#","blob":"AQJ=""#),
        req(r#","blob":"AQID AA==""#),
        req(r#","blob":"AQ\nID""#),
        req(r#","blob":"AQ\tID\r""#),
        req(r#","blob":" AQID""#),
        req(r#","blob":"AQ ID""#),
        req(r#","blob":"AQID\f""#),
        req(r#","blob":"AQID ""#),
        req(r#","blob":"AQ=  =""#),
        req(r#","blob":"   ""#),
        req(r#","blob":"AQ-_""#),
        req(r#","blob":"====""#),
        req(r#","blob":5"#),
        req(r#","blob":null"#),
        req(r#","viewCount":null"#),
        req(r#","viewCount":7,"viewCountDelta":-1,"ord":3,"iterations":600000"#),
        req(r#","updatedTicks":1.0"#),
        req(r#","updatedTicks":9223372036854775807"#),
        req(r#","updatedTicks":9223372036854775808"#),
        req(r#","createdAtUtc":null"#),
        req(r#","createdAtUtc":"2026-11-07T19:22:33.5+00:00""#),
        req(r#","createdAtUtc":"2026-11-07T19:22:33+03:00""#),
        req(r#","saltBase64":null,"passwordHashBase64":"x""#),
        req(r#","batchVideos":[]"#),
        req(r#","batchVideos":[{"key":"v","encryptedBlob":"AQ==","updatedTicks":5}]"#),
        req(r#","batchVideos":[{"key":"v"}]"#),
        req(r#","batchVideos":[{}]"#),
        req(r#","batchVideos":[{"Key":"v","EncryptedBlob":"AQ==","UpdatedTicks":5}]"#),
        req(r#","batchVideos":[{"key":"v","encryptedBlob":"bad","updatedTicks":5}]"#),
        req(r#","batchVideos":null,"batchProgress":[{"key":"p","encryptedBlob":"Ag==","updatedTicks":1}]"#),
        req(r#","batchListItems":[{"listId":"l","itemHash":"h","ord":2,"encryptedPathBlob":"Aw=="}]"#),
        req(r#","batchListItems":[{"listId":"l","ord":2.5}]"#),
        req(&format!(r#","postCommitEvents":[{{{base},"eventId":"pc","timestampUtc":"2026-11-07T19:22:33Z"}}]"#)),
        req(&format!(r#","postCommitEvents":[{{{base},"eventId":"pc","timestampUtc":"2026-11-07T19:22:33z"}}]"#)),
        req(r#","postCommitEvents":[{"senderInstanceId":"s","scopePath":"p"}]"#),
        req(r#","postCommitEvents":[]"#),
        req(r#","postCommitEvents":null"#),
    ];
    for r in &requests {
        let rust = wire::parse_write_request(r).and_then(|x| wire::write_request_line(&x)).map(|l| l[2..].to_owned());
        parity(&mut cs, "try-request", r, rust, &mut mismatches);
    }

    let acks = [
        r#"{"requestId":"r","committed":true,"isDuplicate":false}"#,
        r#"{"requestId":"r","committed":false,"isDuplicate":false,"error":"Unknown WriteKind \u0027x\u0027."}"#,
        r#"{"requestId":"r"}"#,
        r#"{"committed":true}"#,
        r#"{"requestId":"r","committed":"true"}"#,
        r#"{"requestId":"r","committed":1}"#,
        r#"{"requestId":"r","committed":null}"#,
        r#"{"requestId":"r","error":null}"#,
    ];
    for a in acks {
        let rust = wire::parse_ack(a).map(|x| wire::ack_line(&x)[2..].to_owned());
        parity(&mut cs, "try-ack", a, rust, &mut mismatches);
    }

    cs.send("quit");
    cs.expect("CS-BYE", READY);
    assert!(mismatches.is_empty(), "{} STJ parity mismatches:\n{}", mismatches.len(), mismatches.join("\n"));
}

/// The server's kind check matches `Enum.TryParse<WriteKind>(ignoreCase)` except the approved F4 rejections
/// (`FlushBarrier`, undefined numbers, flag lists).
#[test]
fn write_kind_parsing_matches_enum_try_parse_except_f4() {
    let Some(exe) = fixture() else { return };
    let mut cs = CSharp::start(&exe, &test_scope("interop-kind"));
    cs.expect("CS-READY", READY);
    let mut mismatches = Vec::new();
    let inputs = [
        "UpsertVideo", "upsertvideo", "UPSERTVIDEO", " UpsertVideo ", "RetagOwner", "PurgeLegacyVideoKeys", "0", "2", "14",
        "+3", " 7 ", "007", "15", "16", "-1", "99", "FlushBarrier", "flushbarrier", "UpsertVideo, DeleteVideo", "2,3",
        "Upsert Video", "UpsertVideo.", "1.0", "0x2", "\u{130}psertVideo", "upsertv\u{131}deo",
    ];
    for k in inputs {
        cs.send(&format!("try-kind {k}"));
        let c = cs.expect("CS-TRY", READY);
        let cs_kind = c.strip_prefix("ok ").map(|n| n.parse::<i64>().unwrap());
        let rust = WriteKind::parse_request_kind(k).map(|w| WriteKind::ALL.iter().position(|x| *x == w).unwrap() as i64);
        // F4: C# also accepts FlushBarrier (15), undefined numbers and flag lists; Rust rejects them.
        let expected = cs_kind.filter(|n| (0..15).contains(n) && !k.contains(','));
        if rust != expected {
            mismatches.push(format!("{k:?}: C# {c}, Rust {rust:?}"));
        }
    }
    cs.send("quit");
    cs.expect("CS-BYE", READY);
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

/// Subscription topics match like C#'s `OrdinalIgnoreCase` (one-to-one simple case mapping, not full Unicode
/// folding), checked against .NET for tricky pairs.
#[test]
fn topic_matching_equals_ordinal_ignore_case() {
    let Some(exe) = fixture() else { return };
    let mut cs = CSharp::start(&exe, &test_scope("interop-ieq"));
    cs.expect("CS-READY", READY);
    let pairs = [
        ("settings", "SETTINGS"),
        ("settings", "\u{17F}ettings"),
        ("media-index", "MEDIA-\u{130}NDEX"),
        ("media-index", "media-\u{131}ndex"),
        ("thumbnail", "THUMBNA\u{130}L"),
        ("ok", "o\u{212A}"),
        ("stra\u{DF}e", "STRASSE"),
        ("stra\u{DF}e", "STRA\u{1E9E}E"),
        ("\u{1C5}", "\u{1C4}"),
        ("\u{3C3}", "\u{3A3}"),
        ("\u{3C2}", "\u{3A3}"),
        ("watch-progress", "watch-progress "),
    ];
    let bus = sa_sync::InProcessBus::new();
    let mut mismatches = Vec::new();
    for (a, b) in pairs {
        cs.send(&format!("ieq {a}\t{b}"));
        let expected = cs.expect("CS-IEQ", READY) == "true";
        let rec = Recorder::default();
        let _s = bus.subscribe_topic(a, rec.handler());
        let mut e = StateSyncEvent::new(Topic::Profiles, EventKind::ListChanged, "c", "p");
        e.topic = b.into();
        bus.publish_event(e);
        if (rec.len() == 1) != expected {
            mismatches.push(format!("{a:?} vs {b:?}: C# {expected}"));
        }
    }
    cs.send("quit");
    cs.expect("CS-BYE", READY);
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}
