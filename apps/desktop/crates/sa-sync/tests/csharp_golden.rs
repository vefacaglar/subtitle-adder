//! Interop with the C# app's `StateSyncBroker`, replayed from golden transcripts frozen from the real C# broker
//! (`tests/fixtures/csharp/`, see the README there): both hosting directions, events both ways, `W:`/`A:` lines both
//! ways, the pipe security each side accepted (F6), slot/lock files, hosting takeover, resend order, STJ round trips
//! and accept/drop decisions, `Enum.TryParse<WriteKind>`, `OrdinalIgnoreCase`, the default scope and
//! `StreamReader.ReadLine` framing.
//!
//! C# → Rust: the exact bytes the C# broker wrote are fed to the Rust broker. Rust → C#: what Rust writes must equal
//! what C# wrote for the same value (byte for byte, modulo JSON string escaping, which STJ applies to every non-ASCII
//! and HTML-sensitive character), or, where the two runtimes legitimately differ, the frozen Rust output that the C#
//! broker read back into the same value.
#![cfg(windows)]

mod common;

use std::fs::{File, OpenOptions};
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{Recorder, fake_factory, test_scope, wait_until};
use sa_sync::win_testing::{self, RawClient, RawServer};
use sa_sync::wire::{self, BatchListItem, BatchRow};
use sa_sync::{
    EventKind, NamedPipeBus, ProfileWriteBroker, StateSyncEvent, StateSyncWriteRequest, SyncBus, Topic, TopicFilter,
    WriteKind,
};
use serde_json::Value;
use sha2::{Digest, Sha256};

const READY: Duration = Duration::from_secs(10);
/// DB path of the rich request (never opened: the C# stub store and the fake store keep everything in memory).
const GOLDEN_DB: &str = r"C:\sa-sync-golden\profiles.db";
/// DB path of the resent requests.
const RESEND_DB: &str = r"C:\sa-sync-golden\resend.db";

// ------------------------------------------------------------------------------------------------ fixture values

fn rich_event() -> StateSyncEvent {
    let mut e = StateSyncEvent::new(
        Topic::WatchProgress,
        EventKind::ProgressUpdated,
        "rust-manager",
        r"C:\Users\Şule\AppData\Local\SubtitleAdder\profiles.json",
    );
    e.event_id = "golden-rust-event".into();
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

/// The event the C# broker published (`Publish(evt)`: its own topic).
fn csharp_event() -> StateSyncEvent {
    let mut e = rich_event();
    e.event_id = "golden-from-csharp".into();
    e.topic = "Thumbnail".into();
    e
}

fn post_commit_event() -> StateSyncEvent {
    let mut e = rich_event();
    e.event_id = "golden-post-commit".into();
    e
}

fn rich_request() -> StateSyncWriteRequest {
    let mut r = StateSyncWriteRequest::new("rust-manager", GOLDEN_DB, WriteKind::ReplaceAllVideosAndProgress);
    r.request_id = "golden-rich-request".into();
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
    r.post_commit_events = Some(vec![post_commit_event()]);
    r
}

fn minimal_request() -> StateSyncWriteRequest {
    let mut r = StateSyncWriteRequest::new("s", "db", WriteKind::UpsertVideo);
    r.request_id = "golden-minimal".into();
    r
}

fn bad_kind_request() -> StateSyncWriteRequest {
    let mut r = StateSyncWriteRequest::new("rust-manager", GOLDEN_DB, WriteKind::UpsertVideo);
    r.request_id = "golden-bad-kind".into();
    r.kind = "Nope".into();
    r
}

fn flush_barrier_request() -> StateSyncWriteRequest {
    let mut r = StateSyncWriteRequest::new("cs", GOLDEN_DB, WriteKind::FlushBarrier);
    r.request_id = "fb-cs".into();
    r
}

/// 24 requests of four kinds, resent in submission order.
fn resend_requests() -> Vec<StateSyncWriteRequest> {
    let kinds =
        [WriteKind::UpsertVideo, WriteKind::UpsertWatchProgress, WriteKind::ReplaceAllVideosAndProgress, WriteKind::RetagOwner];
    (0..24)
        .map(|i| {
            let mut r = StateSyncWriteRequest::new("rust-manager", RESEND_DB, kinds[i % kinds.len()]);
            r.request_id = format!("{:08x}{i:02}", (i as u32).wrapping_mul(2_654_435_761));
            r
        })
        .collect()
}

// ------------------------------------------------------------------------------------------------ helpers

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/csharp")
}

fn fixture(name: &str) -> Value {
    let path = fixtures_dir().join(name);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn s(v: &Value) -> &str {
    v.as_str().unwrap_or_else(|| panic!("not a string: {v}"))
}

fn sha256_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// `a` and `b` are the same JSON text up to string escaping: same members in the same order, same values, same
/// number spelling (both are re-written by serde_json with `preserve_order`).
fn same_json(a: &str, b: &str) -> bool {
    match (serde_json::from_str::<Value>(a), serde_json::from_str::<Value>(b)) {
        (Ok(x), Ok(y)) => serde_json::to_string(&x).unwrap() == serde_json::to_string(&y).unwrap(),
        _ => false,
    }
}

/// The lines of a transcript step in one direction (`"sent"`/`"received"`), as the exact strings (terminator
/// included) that were on the pipe.
fn lines<'a>(step: &'a Value, dir: &str) -> Vec<&'a str> {
    step[dir].as_array().unwrap_or_else(|| panic!("{dir} of {step}")).iter().map(s).collect()
}

fn step<'a>(transcript: &'a Value, name: &str) -> &'a Value {
    transcript["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|st| st["step"] == name)
        .unwrap_or_else(|| panic!("no step {name}"))
}

/// The C# fixture's stdout line of a step with that prefix (prefix removed).
fn stdout<'a>(step: &'a Value, prefix: &str) -> &'a str {
    step["csharp_stdout"]
        .as_array()
        .unwrap()
        .iter()
        .map(s)
        .find_map(|l| l.strip_prefix(prefix).map(str::trim_start))
        .unwrap_or_else(|| panic!("no {prefix} in {step}"))
}

/// Puts the current token's SIDs into an SDDL template (`{owner}`, `{user}`).
fn unmask_sddl(template: &str) -> String {
    template
        .replace("{owner}", &win_testing::current_owner_sid().unwrap())
        .replace("{user}", &win_testing::current_user_sid().unwrap())
}

fn mask_sddl(sddl: &str) -> String {
    sddl.replace(&win_testing::current_owner_sid().unwrap(), "{owner}")
        .replace(&win_testing::current_user_sid().unwrap(), "{user}")
}

/// Holds a file the way the C# broker does: `FileStream(mode, ReadWrite, FileShare.None[, DeleteOnClose])`.
fn hold_like_csharp(path: &Path, delete_on_close: bool) -> File {
    const FILE_FLAG_DELETE_ON_CLOSE: u32 = 0x0400_0000;
    const DELETE: u32 = 0x0001_0000;
    const GENERIC_RW: u32 = 0x8000_0000 | 0x4000_0000;
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut o = OpenOptions::new();
    o.read(true).write(true).create(true).share_mode(0);
    if delete_on_close {
        o.truncate(true).access_mode(GENERIC_RW | DELETE).custom_flags(FILE_FLAG_DELETE_ON_CLOSE);
    }
    o.open(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn broker_dir(scope: &str) -> PathBuf {
    sa_sync::broker_dir(&std::env::temp_dir(), scope)
}

/// The frozen C# file names with `{hash}` resolved for `scope`.
fn csharp_file(scope: &str, template: &str) -> PathBuf {
    broker_dir(scope).join(template.replace('/', "\\").replace("{hash}", &sa_sync::scope_hash(scope)))
}

fn line_text(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes).expect("UTF-8 line")
}

fn json_of_line(line: &str) -> &str {
    line.strip_suffix('\n').unwrap_or(line)
}

// ------------------------------------------------------------------------------------------------ JSON (STJ)

#[test]
fn stj_round_trips_match_the_frozen_csharp_output() {
    let f = fixture("stj-roundtrip.json");

    // Event: C# read the Rust line and wrote it back.
    let e = rich_event();
    let rust = wire::event_line(&e).unwrap();
    assert!(same_json(&rust, s(&f["event"]["csharp"])), "Rust event JSON differs from STJ's:\n{rust}\n{}", f["event"]["csharp"]);
    assert_eq!(wire::parse_event(s(&f["event"]["csharp"])).unwrap(), e, "Rust reads the STJ event back");

    // Rich request (DateTimeOffset text included).
    let r = rich_request();
    let rust = wire::write_request_line(&r).unwrap();
    let cs = s(&f["request"]["csharp"]);
    assert!(same_json(&rust[2..], cs), "Rust request JSON differs from STJ's:\n{rust}\n{cs}");
    assert_eq!(wire::parse_write_request(cs).unwrap(), r);

    // Minimal request: C# defaults survive the trip.
    let m = minimal_request();
    let rust = wire::write_request_line(&m).unwrap();
    let cs = s(&f["minimal_request"]["csharp"]);
    assert!(same_json(&rust[2..], cs), "{rust}\n{cs}");
    assert_eq!(wire::parse_write_request(cs).unwrap(), m);

    // Acks (STJ writes them for the C# host's `A:` lines).
    for a in f["acks"].as_array().unwrap() {
        let ack = sa_sync::StateSyncWriteAck::new(
            s(&a["requestId"]),
            a["committed"].as_bool().unwrap(),
            a["isDuplicate"].as_bool().unwrap(),
            a["error"].as_str().map(str::to_owned),
        );
        let cs = s(&a["csharp"]);
        assert!(same_json(&wire::ack_line(&ack)[2..], cs), "{:?}\n{cs}", wire::ack_line(&ack));
        assert_eq!(wire::parse_ack(cs).unwrap(), ack);
    }
}

/// Members STJ fills with fresh random/now values when they are absent from the input.
const GENERATED: [&str; 3] = ["eventId", "requestId", "timestampUtc"];

/// Replaces generated members (absent from `input`, also inside `postCommitEvents`) by `"*"`, keeping their place.
fn mask_generated(v: &mut Value, input: &Value) {
    let Some(o) = v.as_object_mut() else { return };
    for k in GENERATED {
        if o.contains_key(k) && input.get(k).is_none() {
            o.insert(k.into(), "*".into());
        }
    }
    if let (Some(Value::Array(out)), Some(Value::Array(inp))) = (o.get_mut("postCommitEvents"), input.get("postCommitEvents")) {
        for (x, i) in out.iter_mut().zip(inp) {
            mask_generated(x, i);
        }
    }
}

fn masked(json: &str, input: &str) -> String {
    let inp: Value = serde_json::from_str(input).unwrap_or(Value::Null);
    let mut v: Value = serde_json::from_str(json).unwrap();
    mask_generated(&mut v, &inp);
    serde_json::to_string(&v).unwrap()
}

/// The original live comparison (C# reading Rust's output gives what C# made of the input): generated members are
/// removed, and the one known, behavior-neutral representation difference is folded: Rust models C#'s
/// nullable-in-practice `string` members as `String`, so a JSON `null` there is read as `""` (`topic`: `"profiles"`,
/// C#'s `Topic ?? "profiles"`).
fn strip_generated(mut v: Value, input: &str) -> Value {
    let inp: Value = serde_json::from_str(input).unwrap_or(Value::Null);
    if let Some(o) = v.as_object_mut() {
        for k in GENERATED {
            if inp.get(k).is_none() {
                o.remove(k);
            }
        }
        for k in ["eventId", "senderInstanceId", "scopePath", "requestId", "dbPath", "kind"] {
            if o.get(k).is_some_and(|x| x.as_str() == Some("")) && inp.get(k).is_some_and(Value::is_null) {
                o.remove(k);
            }
        }
        if o.get("kind").is_some_and(Value::is_number) && !o.contains_key("topic") {
            o.insert("topic".into(), "profiles".into());
        }
    }
    v
}

fn rust_try(cmd: &str, input: &str) -> Option<String> {
    match cmd {
        "try-event" => wire::parse_event(input).and_then(|x| wire::event_line(&x)),
        "try-request" => wire::parse_write_request(input).and_then(|x| wire::write_request_line(&x)).map(|l| l[2..].to_owned()),
        "try-ack" => wire::parse_ack(input).map(|x| wire::ack_line(&x)[2..].to_owned()),
        _ => panic!("unknown command {cmd}"),
    }
}

/// Same accept/drop decision as STJ for every frozen edge case of the three line types; an accepted line is written
/// by Rust exactly as STJ wrote it (up to escaping), or, where the two differ, exactly as the frozen Rust output that
/// STJ read back into what it made of the original.
#[test]
fn rust_accepts_and_drops_exactly_the_lines_stj_did() {
    let f = fixture("stj-parity.json");
    let cases = f["cases"].as_array().unwrap();
    assert!(cases.len() > 150, "{} cases", cases.len());
    let mut mismatches = Vec::new();
    let (mut like_stj, mut frozen_rust) = (0, 0);
    for c in cases {
        let (cmd, input) = (s(&c["cmd"]), s(&c["input"]));
        let cs = c["csharp"].as_str();
        let rust = rust_try(cmd, input);
        match (cs, &rust) {
            (None, None) => {}
            (Some(cs), Some(r)) => {
                let then = s(&c["rust"]);
                let again = s(&c["csharp_of_rust"]);
                let norm = |x: &str| strip_generated(serde_json::from_str(x).unwrap(), input);
                assert_eq!(norm(again), norm(cs), "fixture: STJ did not read the frozen Rust output back for {input}");
                let now = masked(r, input);
                if same_json(&masked(then, input), &masked(cs, input)) {
                    like_stj += 1;
                    if !same_json(&now, &masked(cs, input)) {
                        mismatches.push(format!("{cmd} {input}\n   C#:   {cs}\n   Rust: {r}"));
                    }
                } else {
                    frozen_rust += 1;
                    if now != masked(then, input) {
                        mismatches.push(format!("{cmd} {input}\n   C#-validated Rust: {then}\n   Rust now: {r}"));
                    }
                }
            }
            _ => mismatches.push(format!("{cmd} {input}\n   C#: {cs:?}\n   Rust: {rust:?}")),
        }
    }
    eprintln!("{like_stj} accepted lines written like STJ, {frozen_rust} like the C#-validated Rust output");
    assert!(mismatches.is_empty(), "{} STJ parity mismatches:\n{}", mismatches.len(), mismatches.join("\n"));
}

/// The server's kind check matches `Enum.TryParse<WriteKind>(ignoreCase)` except the approved F4 rejections
/// (`FlushBarrier`, undefined numbers, flag lists).
#[test]
fn write_kind_parsing_matches_frozen_enum_try_parse_except_f4() {
    let f = fixture("write-kind-try-parse.json");
    let mut mismatches = Vec::new();
    for c in f["cases"].as_array().unwrap() {
        let k = s(&c["input"]);
        let cs_kind = c["csharp"].as_i64();
        let rust = WriteKind::parse_request_kind(k).map(|w| WriteKind::ALL.iter().position(|x| *x == w).unwrap() as i64);
        // F4: C# also accepts FlushBarrier (15), undefined numbers and flag lists; Rust rejects them.
        let expected = cs_kind.filter(|n| (0..15).contains(n) && !k.contains(','));
        if rust != expected {
            mismatches.push(format!("{k:?}: C# {cs_kind:?}, Rust {rust:?}"));
        }
    }
    // The enum order itself (C# ordinals = the frozen names).
    let names: Vec<&str> = f["names"].as_array().unwrap().iter().map(s).collect();
    assert_eq!(names, WriteKind::ALL.iter().map(|k| k.name()).collect::<Vec<_>>());
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

/// Subscription topics match like C#'s `OrdinalIgnoreCase` (one-to-one simple case mapping, not full Unicode
/// folding), checked against the frozen .NET answers for tricky pairs.
#[test]
fn topic_matching_equals_frozen_ordinal_ignore_case() {
    let f = fixture("ordinal-ignore-case.json");
    let bus = sa_sync::InProcessBus::new();
    let mut mismatches = Vec::new();
    for c in f["cases"].as_array().unwrap() {
        let (a, b, expected) = (s(&c["a"]), s(&c["b"]), c["csharp"].as_bool().unwrap());
        let rec = Recorder::default();
        let _s = bus.subscribe_topic(a, rec.handler());
        let mut e = StateSyncEvent::new(Topic::Profiles, EventKind::ListChanged, "c", "p");
        e.topic = b.into();
        bus.publish_event(e);
        if (rec.len() == 1) != expected {
            mismatches.push(format!("{a:?} vs {b:?}: C# {expected}"));
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

/// The pipe readers (client loop and server sessions) split and decode a byte stream into exactly the lines .NET's
/// `StreamReader(UTF8, detectEncodingFromByteOrderMarks: false, 4096).ReadLine` produced, and skip exactly the lines
/// `string.IsNullOrWhiteSpace` skipped, however the bytes are chunked by the pipe.
#[test]
fn line_framing_and_blank_lines_match_frozen_stream_reader() {
    use base64::Engine as _;
    use sa_sync::lines::{LineDecoder, is_blank};

    let f = fixture("stream-reader-lines.json");
    let mut mismatches = Vec::new();
    for c in f["cases"].as_array().unwrap() {
        let bytes = base64::engine::general_purpose::STANDARD.decode(s(&c["bytes"])).unwrap();
        let expected: Vec<(String, bool)> = c["csharp"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| (s(&v["t"]).to_owned(), v["b"].as_bool().unwrap()))
            .collect();
        for chunk in [1usize, 2, 3, 5, 4096, usize::MAX] {
            let mut decoder = LineDecoder::new();
            let mut out = Vec::new();
            for part in bytes.chunks(chunk.min(bytes.len().max(1))) {
                decoder.push(part, &mut out).unwrap();
            }
            out.extend(decoder.finish_stream());
            let rust: Vec<(String, bool)> = out
                .into_iter()
                .map(|l| {
                    let blank = is_blank(&l);
                    (l, blank)
                })
                .collect();
            if rust != expected {
                mismatches.push(format!("{bytes:?} (chunks of {chunk})\n   C#:   {expected:?}\n   Rust: {rust:?}"));
            }
        }
    }
    assert!(mismatches.is_empty(), "{} framing mismatches:\n{}", mismatches.len(), mismatches.join("\n"));
}

/// The default channel scope equals .NET's `$"{UserDomainName}\{UserName}"`. The fixture keeps only hashes (no
/// account names in the repository), so this is checked when the tests run as the account that recorded it.
#[test]
fn default_scope_matches_the_frozen_dotnet_scope_for_the_recording_account() {
    let f = fixture("default-scope.json");
    let sid = win_testing::current_user_sid().unwrap();
    if sha256_hex(&sid) != s(&f["user_sid_sha256"]) {
        eprintln!("SKIPPED: not the account that recorded the .NET default scope");
        return;
    }
    assert_eq!(sha256_hex(&sa_sync::default_scope()), s(&f["scope_sha256"]), "same default channel scope as .NET");
}

// ------------------------------------------------------------------------------------------------ the C# host

/// A replayed C# host: the frozen C# pipe security, slot file and engine lock, and the exact bytes the C# host wrote.
/// The Rust broker joins it, publishes and receives events both ways, resends its pending writes (which the C# host
/// executed in submission order) and is cleared by the C# acks, then takes over hosting when the C# host exits.
#[test]
fn rust_joins_a_replayed_csharp_host_then_takes_over() {
    let t = fixture("csharp-host-transcript.json");
    let scope = test_scope("golden-cs-host");

    // C# named its files with the same scope hash as Rust (the recorded scope and its hash).
    assert_eq!(s(&t["scope_hash"]), sa_sync::scope_hash(s(&t["scope"])), "C# scope hash = Rust scope hash");
    let names: Vec<&str> = t["files_while_hosting"].as_array().unwrap().iter().map(|f| s(&f["name"])).collect();
    assert!(names.contains(&"engine-{hash}.lock"), "{names:?}");
    assert!(names.iter().any(|n| n.starts_with("instances/instance-") && n.ends_with(".slot")), "{names:?}");

    let mut held = Vec::new();
    for file in t["files_while_hosting"].as_array().unwrap() {
        let name = s(&file["name"]);
        assert_eq!(file["size"], 0, "{name}");
        assert_eq!(file["delete_error"], 32, "{name}: C# holds it with FileShare.None");
        held.push(hold_like_csharp(&csharp_file(&scope, name), name.ends_with(".slot")));
    }
    let sddl = unmask_sddl(s(&t["pipe_sddl"]));
    let server = std::thread::spawn({
        let scope = scope.clone();
        move || RawServer::accept(&scope, Some(&sddl), READY).expect("the Rust client connects to the C# pipe")
    });

    let bus = NamedPipeBus::new(Some(&scope));
    bus.set_data_worker_factory(fake_factory());
    let host = server.join().unwrap();
    assert!(bus.wait_until_ready(READY), "Rust client accepted the C# CurrentUserOnly pipe");
    assert!(!bus.is_leader());
    assert_eq!(bus.active_instance_count(), 2, "C# slot file counted");
    // Neither side sends anything on connect (no handshake).
    assert!(t["connect"]["received"].as_array().unwrap().is_empty());
    assert_eq!(host.read_raw_line(Duration::from_millis(300)), None, "the Rust client sends nothing on connect");

    let rec = Recorder::default();
    let _s = bus.subscribe(TopicFilter::All, rec.handler());

    // Rust → C#: the C# host relayed exactly these bytes (verbatim) and C# read them as the event.
    let st = step(&t, "rust_publishes");
    let e = rich_event();
    bus.publish(Topic::WatchProgress, e.clone());
    let got = line_text(host.read_raw_line(READY).expect("published line"));
    assert_eq!(got, lines(st, "received")[0], "Rust event line = the bytes the C# host relayed");
    assert_eq!(wire::parse_event(stdout(st, "CS-RECV")).unwrap(), e, "C# read the event Rust published");

    // C# → Rust: the C# client's line relayed by the C# host.
    let st = step(&t, "csharp_publishes");
    let line = lines(st, "received")[0];
    assert!(host.send_raw(line.as_bytes()));
    assert!(wait_until(3000, || rec.count_id("golden-from-csharp") == 1));
    let got = rec.0.lock().iter().find(|x| x.event_id == "golden-from-csharp").cloned().unwrap();
    assert_eq!(got, csharp_event());

    // Rust W: → C# host store → A: + post-commit broadcast.
    let st = step(&t, "rust_writes");
    let req = rich_request();
    assert_eq!(lines(st, "sent"), [format!("{}\n", wire::write_request_line(&req).unwrap())], "Rust W: line = the frozen one");
    let exec = stdout(st, "CS-EXEC");
    assert!(exec.starts_with("ReplaceAllVideosAndProgress golden-rich-request dup=false blob=AAEC videos=1 items=1"), "{exec}");
    let received = lines(st, "received");
    let ack = received.iter().find_map(|l| l.strip_prefix("A:")).expect("A: line");
    let ack = wire::parse_ack(json_of_line(ack)).unwrap();
    assert!(ack.committed && !ack.is_duplicate, "{ack:?}");
    assert_eq!(ack.request_id, req.request_id);
    let pc_line = received.iter().find(|l| !l.starts_with("A:")).expect("post-commit line");
    assert_eq!(wire::parse_event(json_of_line(pc_line)).unwrap(), post_commit_event());
    assert_eq!(wire::parse_event(stdout(st, "CS-RECV")).unwrap(), post_commit_event());
    assert!(host.send_raw(pc_line.as_bytes()));
    assert!(wait_until(3000, || rec.count_id("golden-post-commit") == 1), "the C# host's post-commit broadcast reached Rust");

    let st = step(&t, "rust_writes_again");
    assert_eq!(lines(st, "sent"), [format!("{}\n", wire::write_request_line(&req).unwrap())]);
    let received = lines(st, "received");
    assert_eq!(received.len(), 1, "a duplicate's post-commit events are dropped: {received:?}");
    let dup = wire::parse_ack(json_of_line(received[0].strip_prefix("A:").unwrap())).unwrap();
    assert!(dup.committed && dup.is_duplicate && dup.request_id == req.request_id, "{dup:?}");

    let st = step(&t, "rust_writes_unknown_kind");
    assert_eq!(lines(st, "sent"), [format!("{}\n", wire::write_request_line(&bad_kind_request()).unwrap())]);
    let nack = wire::parse_ack(json_of_line(lines(st, "received")[0].strip_prefix("A:").unwrap())).unwrap();
    assert!(!nack.committed && !nack.is_duplicate && nack.request_id == "golden-bad-kind", "{nack:?}");
    assert_eq!(nack.error.as_deref(), Some("Unknown WriteKind 'Nope'."));
    assert_eq!(nack.error.unwrap(), WriteKind::unknown_kind_error("Nope"), "the Rust host's text for the same case");

    // Unacknowledged requests reach the C# host as W: lines in submission order; its A: acks clear them.
    let st = step(&t, "rust_resends");
    let _ = bus.data_worker_store(Path::new(RESEND_DB));
    let resend_db = std::path::absolute(RESEND_DB).unwrap();
    let store = common::created_stores().lock().iter().rev().find(|s| s.path == resend_db).cloned().unwrap();
    // This process's store holds its copies, so only the C# host's acks can complete the requests.
    store.hold.store(true, std::sync::atomic::Ordering::SeqCst);
    let requests = resend_requests();
    for r in &requests {
        bus.submit_write_request(r.clone());
    }
    assert_eq!(bus.pending_write_request_count(), requests.len());
    bus.resend_pending_write_requests();
    let sent = lines(st, "sent");
    assert_eq!(sent.len(), requests.len());
    for (i, frozen) in sent.iter().enumerate() {
        let got = line_text(host.read_raw_line(READY).unwrap_or_else(|| panic!("resent line {i}")));
        assert_eq!(&got, frozen, "resent line {i}");
    }
    let executed: Vec<&str> = st["csharp_stdout"]
        .as_array()
        .unwrap()
        .iter()
        .map(s)
        .filter_map(|l| l.strip_prefix("CS-EXEC "))
        .filter(|l| l.contains("dup=false"))
        .map(|l| l.split_whitespace().nth(1).unwrap())
        .collect();
    let ids: Vec<&str> = requests.iter().map(|r| r.request_id.as_str()).collect();
    assert_eq!(executed, ids, "the C# host executed the resent requests in submission order");
    for ack in lines(st, "received") {
        assert!(host.send_raw(ack.as_bytes()));
    }
    assert!(
        wait_until(3000, || bus.pending_write_request_count() == 0),
        "{} requests still pending after the C# acks",
        bus.pending_write_request_count()
    );

    // The C# host exits (pipe closed, lock and slot released): the Rust engine takes over hosting.
    assert_eq!(s(&t["bye"]), "closed=false", "the Rust instance kept the channel alive");
    host.close();
    drop(host);
    drop(held);
    assert!(wait_until(5000, || bus.is_leader() && bus.is_ready()));
    assert_eq!(bus.active_instance_count(), 1);
    bus.dispose();
    assert!(bus.closed_engine_on_dispose());
}


// ------------------------------------------------------------------------------------------------ the C# client

/// A replayed C# client on the Rust host: the Rust pipe security is the one the C# `CurrentUserOnly` client
/// accepted, the C# slot file is counted, the exact bytes the C# client wrote (event, `W:` lines) are relayed and
/// executed, and what the Rust host writes back (events, `A:` lines) is what C# read.
#[test]
fn a_replayed_csharp_client_talks_to_the_rust_host() {
    let spy = fixture("csharp-client-transcript.json");
    let scope = test_scope("golden-rust-host");
    let bus = NamedPipeBus::new(Some(&scope));
    bus.set_data_worker_factory(fake_factory());
    assert!(bus.wait_until_ready(READY));
    assert!(bus.is_leader());
    let rec = Recorder::default();
    let _s = bus.subscribe(TopicFilter::All, rec.handler());

    // F6: C# accepted a pipe with exactly this descriptor.
    let rust_host = &spy["rust_host"];
    assert_eq!(s(&rust_host["ready"]), "true leader=false", "C# CurrentUserOnly client accepted the Rust pipe");
    assert_eq!(mask_sddl(&win_testing::server_instance_sddl(&scope).unwrap()), s(&rust_host["pipe_sddl"]));

    // The C# instance: its slot file and its pipe session.
    let slot = s(&spy["csharp_slot"]);
    let held = hold_like_csharp(&csharp_file(&scope, slot), true);
    let cs = RawClient::connect_raw(&scope, READY).unwrap();
    assert!(
        wait_until(3000, || bus.active_instance_count() == 2 && bus.connected_client_count() == 2),
        "count={} clients={}",
        bus.active_instance_count(),
        bus.connected_client_count()
    );
    assert!(s(&rust_host["status"]).starts_with("leader=false ready=true count=2"));
    // The C# client sent nothing on connect (no handshake).
    assert!(spy["spy"]["connect"]["received"].as_array().unwrap().is_empty());
    let watcher = RawClient::connect_raw(&scope, READY).unwrap();

    // Rust → C#: the bytes the Rust host relayed, which C# read as the event (on the spy host and on the Rust host).
    let e = rich_event();
    bus.publish(Topic::WatchProgress, e.clone());
    let got = line_text(cs.read_raw_line(READY).expect("relayed event"));
    let st = step(rust_host, "rust_publishes");
    assert_eq!(got, lines(st, "relayed")[0]);
    assert_eq!(wire::parse_event(stdout(st, "CS-RECV")).unwrap(), e);
    let st = step(&spy["spy"], "host_sends_event");
    assert_eq!(got, lines(st, "sent")[0], "the same line C# read from the spy host");
    assert_eq!(wire::parse_event(stdout(st, "CS-RECV")).unwrap(), e);
    assert_eq!(line_text(watcher.read_raw_line(READY).unwrap()), got);

    // C# → Rust: the C# client's own STJ line; relayed verbatim to the other clients.
    let st = step(&spy["spy"], "csharp_publishes");
    let line = lines(st, "received")[0];
    assert!(cs.send_raw(line.as_bytes()));
    assert!(wait_until(3000, || rec.count_id("golden-from-csharp") == 1));
    assert_eq!(rec.0.lock().iter().find(|x| x.event_id == "golden-from-csharp").cloned().unwrap(), csharp_event());
    assert_eq!(line_text(watcher.read_raw_line(READY).unwrap()), line, "relayed verbatim");
    assert_eq!(lines(step(rust_host, "csharp_publishes"), "relayed")[0], line);

    // C# W: (STJ-written) → Rust host → A: (read by STJ) + the post-commit broadcast.
    let st = step(rust_host, "csharp_writes");
    assert!(cs.send_raw(lines(st, "sent")[0].as_bytes()));
    let ack_line = line_text(cs.read_raw_line(READY).expect("A: line"));
    let ack = json_of_line(ack_line.strip_prefix("A:").expect("A: prefix"));
    assert!(same_json(ack, stdout(st, "CS-ACK")), "Rust ack {ack} vs what C# read {}", stdout(st, "CS-ACK"));
    let ack = wire::parse_ack(ack).unwrap();
    assert!(ack.committed && !ack.is_duplicate && ack.request_id == "golden-rich-request", "{ack:?}");
    assert!(wait_until(3000, || rec.count_id("golden-post-commit") == 1));
    let pc = line_text(cs.read_raw_line(READY).expect("post-commit line"));
    assert_eq!(pc, lines(st, "relayed")[0], "post-commit line = the bytes C# read");
    assert_eq!(wire::parse_event(stdout(st, "CS-RECV")).unwrap(), post_commit_event());
    assert_eq!(line_text(watcher.read_raw_line(READY).unwrap()), pc);

    let st = step(rust_host, "csharp_writes_flush_barrier");
    assert!(cs.send_raw(lines(st, "sent")[0].as_bytes()));
    let ack_line = line_text(cs.read_raw_line(READY).expect("A: line"));
    let ack = json_of_line(ack_line.strip_prefix("A:").unwrap());
    assert_eq!(
        stdout(st, "CS-ACK"),
        r#"{"requestId":"fb-cs","committed":false,"isDuplicate":false,"error":"Unknown WriteKind \u0027FlushBarrier\u0027."}"#
    );
    assert!(same_json(ack, stdout(st, "CS-ACK")), "{ack}");

    // The C# instance exits.
    assert_eq!(s(&rust_host["bye"]), "closed=false");
    cs.close();
    watcher.close();
    drop(held);
    assert!(wait_until(3000, || bus.active_instance_count() == 1));
    let _ = bus.flush_pending_writes(Path::new(GOLDEN_DB), None);
    bus.dispose();
    assert!(bus.closed_engine_on_dispose());
}

/// The C# client accepted the same Rust-written bytes on a host that only relays (the spy host), and closed the
/// channel on exit when it was the last instance.
#[test]
fn the_frozen_csharp_client_lines_are_the_rust_wire_format() {
    let spy = fixture("csharp-client-transcript.json");
    let st = step(&spy["spy"], "csharp_publishes");
    let line = lines(st, "received")[0];
    assert!(line.ends_with('\n') && !line.ends_with("\r\n"), "C# terminates lines with \\n");
    let json = json_of_line(line);
    assert_eq!(wire::parse_event(json).unwrap(), csharp_event());
    assert!(same_json(&wire::event_line(&csharp_event()).unwrap(), json), "Rust writes the C# event like STJ");
    assert_eq!(s(&spy["spy"]["ready"]), "true leader=false");
    assert_eq!(s(&spy["spy"]["bye"]), "closed=true");
    // The W: lines the C# broker writes: `"W:" + Serialize(request)`.
    for (name, expected) in
        [("csharp_writes", rich_request()), ("csharp_writes_flush_barrier", flush_barrier_request())]
    {
        let w = lines(step(&spy["rust_host"], name), "sent")[0];
        let req = wire::parse_write_request(json_of_line(w.strip_prefix("W:").unwrap())).unwrap();
        assert_eq!(req, expected, "{name}");
        assert!(same_json(&wire::write_request_line(&req).unwrap()[2..], json_of_line(&w[2..])), "{name}");
    }
}
