//! Wire format (win-sync §4–§5): golden serialization against what System.Text.Json writes, tolerant parsing of
//! STJ output, dropped malformed lines, framing.

use chrono::{TimeZone, Utc};
use sa_sync::lines::{LineDecoder, MAX_LINE_BYTES, encode_line, is_blank};
use sa_sync::wire::{
    self, BatchListItem, BatchRow, EventKind, MIN_DATE_TEXT, StateSyncEvent, StateSyncWriteAck, StateSyncWriteRequest,
    WriteKind, format_timestamp, parse_timestamp,
};
use sa_sync::{Topic, broker_dir, scope_hash};

fn sample_event() -> StateSyncEvent {
    let mut e = StateSyncEvent::new(Topic::WatchProgress, EventKind::ProgressUpdated, "5be1", r"C:\Users\User\AppData\Local\SubtitleAdder\profiles.json");
    e.event_id = "9d0c".into();
    e.username = Some("emre".into());
    e.video_path = Some(r"D:\Videos\ep1.mp4".into());
    e.position_seconds = Some(77.0);
    e.duration_seconds = Some(100.0);
    e.timestamp_utc = "2026-11-07T19:22:33.1234567+00:00".into();
    e
}

#[test]
fn event_golden_matches_stj_key_order_and_omits_nulls() {
    let line = wire::event_line(&sample_event()).unwrap();
    assert_eq!(
        line,
        r#"{"eventId":"9d0c","topic":"watch-progress","kind":4,"senderInstanceId":"5be1","scopePath":"C:\\Users\\User\\AppData\\Local\\SubtitleAdder\\profiles.json","username":"emre","videoPath":"D:\\Videos\\ep1.mp4","positionSeconds":77,"durationSeconds":100,"timestampUtc":"2026-11-07T19:22:33.1234567+00:00"}"#
    );
}

#[test]
fn event_all_fields_in_declaration_order() {
    let mut e = sample_event();
    e.video_key = Some("k".into());
    e.position_seconds = Some(10.5);
    e.view_count = Some(0);
    e.is_liked = Some(false);
    e.rating = Some(-1);
    e.list_id = Some("l".into());
    e.payload_base64 = Some("AQID".into());
    e.sealed_payload_base64 = Some("BAUG".into());
    let line = wire::event_line(&e).unwrap();
    let keys: Vec<String> = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&line).unwrap().keys().cloned().collect();
    assert_eq!(
        keys,
        [
            "eventId", "topic", "kind", "senderInstanceId", "scopePath", "username", "videoKey", "videoPath",
            "positionSeconds", "durationSeconds", "viewCount", "isLiked", "rating", "listId", "payloadBase64",
            "sealedPayloadBase64", "timestampUtc"
        ]
    );
    assert!(line.contains(r#""positionSeconds":10.5"#));
    assert!(line.contains(r#""viewCount":0"#));
    assert!(line.contains(r#""isLiked":false"#));
}

#[test]
fn every_kind_keeps_its_wire_number() {
    let expected = [
        (EventKind::ProfileLogin, 0),
        (EventKind::ProfileLogout, 1),
        (EventKind::ProfileDeleted, 2),
        (EventKind::VideoWatched, 3),
        (EventKind::ProgressUpdated, 4),
        (EventKind::VideoLiked, 5),
        (EventKind::VideoRated, 6),
        (EventKind::HistoryCleared, 7),
        (EventKind::ListChanged, 8),
        (EventKind::ThumbnailCaptured, 9),
        (EventKind::MediaIndexed, 10),
        (EventKind::ProvidersUpdated, 11),
        (EventKind::SettingsUpdated, 12),
    ];
    for (k, n) in expected {
        assert_eq!(k.to_wire(), n);
        assert_eq!(EventKind::from_wire(n), k);
    }
    assert_eq!(EventKind::from_wire(42), EventKind::Other(42));
    let topics: Vec<&str> = Topic::ALL.iter().map(|t| t.wire()).collect();
    assert_eq!(topics, ["profiles", "watch-progress", "thumbnail", "media-index", "providers", "settings"]);
}

#[test]
fn parses_stj_escaped_output_and_unknown_members() {
    // STJ escapes non-ASCII and HTML-sensitive characters.
    let line = r#"{"eventId":"e1","topic":"profiles","kind":0,"senderInstanceId":"s","scopePath":"C:\\p.json","username":"\u015Fule\u0027s \u002B","futureField":{"x":[1,2]},"timestampUtc":"2026-11-07T19:22:33+00:00"}"#;
    let e = wire::parse_event(line).unwrap();
    assert_eq!(e.username.as_deref(), Some("şule's +"));
    assert_eq!(e.kind, EventKind::ProfileLogin);
    assert_eq!(e.scope_path, r"C:\p.json");
}

#[test]
fn defaults_when_optional_members_are_absent() {
    let e = wire::parse_event(r#"{"kind":99,"senderInstanceId":"s","scopePath":"p"}"#).unwrap();
    assert_eq!(e.kind, EventKind::Other(99));
    assert_eq!(e.topic, "profiles");
    assert_eq!(e.event_id.len(), 32);
    assert!(e.timestamp().is_some());
    // Present-but-null strings are accepted (STJ has no nullable enforcement).
    let e = wire::parse_event(r#"{"eventId":null,"topic":null,"kind":1,"senderInstanceId":null,"scopePath":null}"#).unwrap();
    assert_eq!(e.event_id, "");
    assert_eq!(e.topic, "profiles");
    assert_eq!(e.sender_instance_id, "");
}

#[test]
fn malformed_lines_are_dropped() {
    for bad in [
        r#"{"topic":"profiles","senderInstanceId":"s","scopePath":"p"}"#,       // kind missing (required)
        r#"{"kind":1,"scopePath":"p"}"#,                                         // senderInstanceId missing
        r#"{"kind":1,"senderInstanceId":"s"}"#,                                  // scopePath missing
        r#"{"kind":1.0,"senderInstanceId":"s","scopePath":"p"}"#,                // non-integral enum
        r#"{"kind":"1","senderInstanceId":"s","scopePath":"p"}"#,                // quoted number
        r#"{"kind":null,"senderInstanceId":"s","scopePath":"p"}"#,               // null into a value type
        r#"{"Kind":1,"SenderInstanceId":"s","ScopePath":"p"}"#,                  // case-sensitive names
        r#"{"kind":1,"senderInstanceId":"s","scopePath":"p","viewCount":1.5}"#,  // non-integral int
        r#"{"kind":1,"senderInstanceId":"s","scopePath":"p","timestampUtc":"yesterday"}"#,
        r#"{"kind":1,"senderInstanceId":"s","scopePath":"p","timestampUtc":null}"#,
        "W:{}",
        "not json",
    ] {
        assert!(wire::parse_event(bad).is_none(), "{bad}");
    }
}

/// STJ behaviors the C# corpus test found (kept here so they are checked without a .NET SDK too).
#[test]
fn stj_reading_quirks() {
    // A repeated member: the last value wins.
    let e = wire::parse_event(r#"{"kind":1,"kind":2,"senderInstanceId":"s","scopePath":"p"}"#).unwrap();
    assert_eq!(e.kind, EventKind::ProfileDeleted);
    // byte[]: spaces, tabs, CR, LF are skipped anywhere (kept canonical); other whitespace fails.
    let req = |blob: &str| {
        wire::parse_write_request(&format!(r#"{{"senderInstanceId":"s","dbPath":"d","kind":"UpsertVideo","blob":"{blob}"}}"#))
    };
    assert_eq!(req(r"AQ\tID AA\r\n==").unwrap().blob.as_deref(), Some("AQIDAA=="));
    assert_eq!(req("AQ=  =").unwrap().blob.as_deref(), Some("AQ=="));
    assert_eq!(req("   ").unwrap().blob.as_deref(), Some(""));
    assert!(req(r"AQID\f").is_none());
    assert!(req("AQI").is_none());
    assert_eq!(wire::canonical_base64("AQID"), Some("AQID".into()));
    // MaxDepth 64.
    let nested = |n: usize| {
        format!(r#"{{"kind":1,"senderInstanceId":"s","scopePath":"p","x":{}{}}}"#, "[".repeat(n), "]".repeat(n))
    };
    assert!(wire::parse_event(&nested(63)).is_some());
    assert!(wire::parse_event(&nested(64)).is_none());
    // Brackets inside strings do not count.
    assert!(wire::parse_event(&format!(r#"{{"kind":1,"senderInstanceId":"{}","scopePath":"p"}}"#, "[".repeat(100))).is_some());
}

#[test]
fn non_finite_doubles_never_reach_the_pipe() {
    let mut e = sample_event();
    e.position_seconds = Some(f64::NAN);
    assert!(wire::event_line(&e).is_none());
    e.position_seconds = Some(1.0);
    e.duration_seconds = Some(f64::INFINITY);
    assert!(wire::event_line(&e).is_none());
    let mut r = StateSyncWriteRequest::new("s", "db", WriteKind::UpsertVideo);
    r.post_commit_events = Some(vec![e]);
    assert!(wire::write_request_line(&r).is_none());
}

#[test]
fn timestamps_follow_datetimeoffset_round_trip_text() {
    let t = Utc.with_ymd_and_hms(2026, 11, 7, 19, 22, 33).unwrap();
    assert_eq!(format_timestamp(t), "2026-11-07T19:22:33+00:00");
    let t2 = t + chrono::Duration::nanoseconds(123_450_000);
    assert_eq!(format_timestamp(t2), "2026-11-07T19:22:33.12345+00:00");
    let t3 = t + chrono::Duration::nanoseconds(123_456_789);
    assert_eq!(format_timestamp(t3), "2026-11-07T19:22:33.1234567+00:00");
    for (text, expect) in [
        ("2026-11-07T19:22:33.1234567+00:00", "2026-11-07T19:22:33.1234567+00:00"),
        ("2026-11-07T19:22:33.123456789Z", "2026-11-07T19:22:33.1234567+00:00"),
        ("2026-11-07T22:22:33+03:00", "2026-11-07T19:22:33+00:00"),
        ("2026-11-07T19:22:33Z", "2026-11-07T19:22:33+00:00"),
        ("0001-01-01T00:00:00+00:00", "0001-01-01T00:00:00+00:00"),
    ] {
        let p = parse_timestamp(text).unwrap_or_else(|| panic!("{text}"));
        assert_eq!(format_timestamp(p.with_timezone(&Utc)), expect, "{text}");
    }
    // Accept/reject decisions as .NET 10 STJ makes them (the full corpus runs against the C# fixture).
    for good in [
        "2026-11-07",
        "2026-11-07T19:22",
        "2026-11-07T19:22Z",
        "2026-11-07T19:22+03",
        "2026-11-07T19:22:33.Z",
        "2026-11-07T19:22:33.+03:00",
        "2026-11-07T19:22:33-03",
        "2026-11-07T19:22:33-14:00",
        "2026-11-07T19:22:33.1234567890123456Z",
        "0001-01-01T01:00:00+01:00",
        "9999-12-31T22:59:59-01:00",
    ] {
        assert!(parse_timestamp(good).is_some(), "{good}");
    }
    for bad in [
        "",
        "2026-13-01T00:00:00Z",
        "2026-11-07 19:22:33Z",
        "2026-11-07T19:22:33z",
        "2026-11-07T19:22:33.",
        "2026-11-07T19:22:33+0300",
        "2026-11-07T19:22:33+14:30",
        "2026-11-07T19:22:33.12345678901234567Z",
        "2026-11-07Z",
        "2026-11-07+03:00",
        "0001-01-01T00:59:59+01:00",
        "9999-12-31T23:59:59-01:00",
    ] {
        assert!(parse_timestamp(bad).is_none(), "{bad}");
    }
    assert_eq!(
        format_timestamp(parse_timestamp("2026-11-07T22:22+03").unwrap().with_timezone(&Utc)),
        "2026-11-07T19:22:00+00:00"
    );
    assert_eq!(wire::dotnet_ticks(Utc.with_ymd_and_hms(1970, 1, 1, 0, 0, 0).unwrap()), 621_355_968_000_000_000);
}

#[test]
fn write_request_defaults_and_golden() {
    let r = wire::parse_write_request(r#"{"senderInstanceId":"s","dbPath":"C:\\p.db","kind":"UpsertVideo"}"#).unwrap();
    assert_eq!(r.view_count, -1);
    assert_eq!(r.created_at_utc, MIN_DATE_TEXT);
    assert_eq!(r.request_id.len(), 32);
    assert_eq!((r.username.as_str(), r.key.as_str(), r.ord, r.updated_ticks), ("", "", 0, 0));

    let mut r = StateSyncWriteRequest::new("s", r"C:\p.db", WriteKind::UpsertVideo);
    r.request_id = "r1".into();
    r.key = "k".into();
    r.blob = Some("AQID".into());
    r.view_count_delta = 1;
    r.batch_videos = Some(vec![BatchRow { key: Some("a".into()), encrypted_blob: Some("AA==".into()), updated_ticks: 5 }]);
    r.batch_list_items =
        Some(vec![BatchListItem { list_id: Some("l".into()), item_hash: Some("h".into()), ord: 2, encrypted_path_blob: Some("AQ==".into()) }]);
    let line = wire::write_request_line(&r).unwrap();
    assert_eq!(
        line,
        r#"W:{"requestId":"r1","senderInstanceId":"s","dbPath":"C:\\p.db","kind":"UpsertVideo","username":"","key":"k","secondaryKey":"","ord":0,"blob":"AQID","updatedTicks":0,"viewCount":-1,"viewCountDelta":1,"iterations":0,"createdAtUtc":"0001-01-01T00:00:00+00:00","batchVideos":[{"key":"a","encryptedBlob":"AA==","updatedTicks":5}],"batchListItems":[{"listId":"l","itemHash":"h","ord":2,"encryptedPathBlob":"AQ=="}]}"#
    );
    let back = wire::parse_write_request(line.strip_prefix("W:").unwrap()).unwrap();
    assert_eq!(back, r);
    // byte[] members must be valid base64 (STJ fails otherwise).
    assert!(wire::parse_write_request(r#"{"senderInstanceId":"s","dbPath":"p","kind":"UpsertVideo","blob":"***"}"#).is_none());
    // required members
    assert!(wire::parse_write_request(r#"{"senderInstanceId":"s","kind":"UpsertVideo"}"#).is_none());
}

#[test]
fn ack_golden() {
    let a = StateSyncWriteAck::new("r1", true, false, None);
    assert_eq!(wire::ack_line(&a), r#"A:{"requestId":"r1","committed":true,"isDuplicate":false}"#);
    let n = StateSyncWriteAck::new("r2", false, false, Some(WriteKind::unknown_kind_error("Nope")));
    assert_eq!(wire::ack_line(&n), r#"A:{"requestId":"r2","committed":false,"isDuplicate":false,"error":"Unknown WriteKind 'Nope'."}"#);
    assert_eq!(wire::parse_ack(r#"{"requestId":"r2","committed":false,"isDuplicate":true,"error":"x"}"#).unwrap().error.as_deref(), Some("x"));
    assert!(wire::parse_ack(r#"{"committed":true}"#).is_none());
}

#[test]
fn write_kind_parsing_rejects_flush_barrier() {
    assert_eq!(WriteKind::parse_request_kind("UpsertVideo"), Some(WriteKind::UpsertVideo));
    assert_eq!(WriteKind::parse_request_kind("upsertvideo"), Some(WriteKind::UpsertVideo));
    assert_eq!(WriteKind::parse_request_kind(" MigrateVideoKey "), Some(WriteKind::MigrateVideoKey));
    assert_eq!(WriteKind::parse_request_kind("0"), Some(WriteKind::UpsertProfile));
    assert_eq!(WriteKind::parse_request_kind("12"), Some(WriteKind::MigrateVideoKey));
    assert_eq!(WriteKind::parse_request_kind("13"), Some(WriteKind::PurgeLegacyVideoKeys));
    assert_eq!(WriteKind::parse_request_kind("retagowner"), Some(WriteKind::RetagOwner));
    for bad in ["FlushBarrier", "flushbarrier", "15", "16", "-1", "", "UpsertVideo,DeleteVideo", "Nope"] {
        assert_eq!(WriteKind::parse_request_kind(bad), None, "{bad}");
    }
    for (i, k) in WriteKind::ALL.iter().enumerate() {
        assert_eq!(serde_json::to_string(k).unwrap(), format!("\"{}\"", k.name()));
        if *k != WriteKind::FlushBarrier {
            assert_eq!(WriteKind::parse_request_kind(&i.to_string()), Some(*k));
        }
    }
}

#[test]
fn scope_hash_and_paths_match_csharp() {
    // SHA-256("test-scope") = 0b... (first 16 hex chars), computed independently.
    assert_eq!(scope_hash("abc"), "ba7816bf8f01cfea");
    assert_eq!(scope_hash(""), "e3b0c44298fc1c14");
    let d = broker_dir(std::path::Path::new("T"), "abc");
    assert_eq!(d, std::path::Path::new("T").join("SubtitleAdder").join("broker-ba7816bf8f01cfea"));
    #[cfg(windows)]
    assert_eq!(sa_sync::pipe_name("abc"), "SubtitleAdder.StateBroker.ba7816bf8f01cfea");
}

// ------------------------------------------------------------------------------------------------- framing

fn decode_all(chunks: &[&[u8]]) -> Vec<String> {
    let mut d = LineDecoder::new();
    let mut out = Vec::new();
    for c in chunks {
        d.push(c, &mut out).unwrap();
    }
    out.extend(d.finish_stream());
    out
}

#[test]
fn framing_accepts_lf_crlf_and_lone_cr() {
    assert_eq!(decode_all(&[b"a\nb\r\nc\rd"]), ["a", "b", "c", "d"]);
    // CR at a chunk end followed by LF in the next chunk is one terminator.
    assert_eq!(decode_all(&[b"a\r", b"\nb\n"]), ["a", "b"]);
    assert_eq!(decode_all(&[b"a\r", b"b\n"]), ["a", "b"]);
    // Lines split across reads, blank lines kept for the caller to skip.
    assert_eq!(decode_all(&[b"{\"x\"", b":1}\n\n  \n"]), ["{\"x\":1}", "", "  "]);
    assert!(is_blank("  \t") && is_blank("") && !is_blank(" x "));
    // BOM at the start of the stream (also split) is skipped; invalid UTF-8 is replaced.
    assert_eq!(decode_all(&[&[0xEF], &[0xBB, 0xBF, b'a', b'\n']]), ["a"]);
    assert_eq!(decode_all(&[&[0xEF, 0xBB, 0xBF, b'b', b'\n', 0xEF, 0xBB, 0xBF, b'\n']]), ["b", "\u{FEFF}"]);
    assert_eq!(decode_all(&[&[b'x', 0xFF, b'\n']]), ["x\u{FFFD}"]);
    // Multi-byte characters split across reads.
    let s = "şule\n".as_bytes();
    assert_eq!(decode_all(&[&s[..1], &s[1..]]), ["şule"]);
    assert_eq!(encode_line("x"), b"x\n");
}

#[test]
fn framing_handles_large_lines_and_caps_runaway_ones() {
    let big = "a".repeat(700_000);
    let bytes = encode_line(&big);
    let chunks: Vec<&[u8]> = bytes.chunks(4096).collect();
    assert_eq!(decode_all(&chunks), [big]);
    assert!(MAX_LINE_BYTES >= 64 * 1024 * 1024);
    let mut d = LineDecoder::with_limit(10);
    let mut out = Vec::new();
    assert!(d.push(b"0123456789abc", &mut out).is_err());
}
