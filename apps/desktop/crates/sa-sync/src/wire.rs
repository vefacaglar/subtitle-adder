//! Broker wire types (win-sync §4–§5), byte-compatible in meaning with the C# `StateSyncBroker` (System.Text.Json,
//! `CamelCase` + `WhenWritingNull`):
//!
//! * keys camelCase in C# declaration order, `null` members omitted, `kind` of an event is a JSON **number** (unknown
//!   values are kept, [`EventKind::Other`]);
//! * `byte[]` members are standard padded base64 strings (validated on read like STJ does);
//! * `DateTimeOffset` members are ISO 8601 text: written with at most 7 trimmed fractional digits and `+00:00`
//!   ([`format_timestamp`]), read in any ISO 8601 form STJ accepts ([`parse_timestamp`]);
//! * integral `double` values are written without a fraction (`77`, like STJ), non-finite values make the line
//!   unserializable ([`event_line`] returns `None`, the C# serializer throws);
//! * C# `required` members missing from the JSON fail the whole line; present-but-`null` strings are accepted (read as
//!   `""`), like STJ without nullable enforcement.

use base64::Engine as _;

use chrono::{DateTime, Datelike, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// `StateSyncEventKind` — the wire integers MUST stay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventKind {
    ProfileLogin,
    ProfileLogout,
    ProfileDeleted,
    VideoWatched,
    ProgressUpdated,
    VideoLiked,
    VideoRated,
    HistoryCleared,
    ListChanged,
    ThumbnailCaptured,
    MediaIndexed,
    ProvidersUpdated,
    SettingsUpdated,
    /// Undefined values still reach subscribers.
    Other(i32),
}

impl EventKind {
    pub const fn to_wire(self) -> i32 {
        match self {
            EventKind::ProfileLogin => 0,
            EventKind::ProfileLogout => 1,
            EventKind::ProfileDeleted => 2,
            EventKind::VideoWatched => 3,
            EventKind::ProgressUpdated => 4,
            EventKind::VideoLiked => 5,
            EventKind::VideoRated => 6,
            EventKind::HistoryCleared => 7,
            EventKind::ListChanged => 8,
            EventKind::ThumbnailCaptured => 9,
            EventKind::MediaIndexed => 10,
            EventKind::ProvidersUpdated => 11,
            EventKind::SettingsUpdated => 12,
            EventKind::Other(v) => v,
        }
    }
    pub const fn from_wire(v: i32) -> Self {
        match v {
            0 => EventKind::ProfileLogin,
            1 => EventKind::ProfileLogout,
            2 => EventKind::ProfileDeleted,
            3 => EventKind::VideoWatched,
            4 => EventKind::ProgressUpdated,
            5 => EventKind::VideoLiked,
            6 => EventKind::VideoRated,
            7 => EventKind::HistoryCleared,
            8 => EventKind::ListChanged,
            9 => EventKind::ThumbnailCaptured,
            10 => EventKind::MediaIndexed,
            11 => EventKind::ProvidersUpdated,
            12 => EventKind::SettingsUpdated,
            other => EventKind::Other(other),
        }
    }
}

impl Serialize for EventKind {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_i32(self.to_wire())
    }
}

impl<'de> Deserialize<'de> for EventKind {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(EventKind::from_wire(i32::deserialize(d)?))
    }
}

/// An event (un-prefixed line). Field order = C# declaration order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StateSyncEvent {
    /// Dedupe key; `""` disables dedupe. Absent → a fresh id.
    #[serde(default = "sa_core::text::guid_n", deserialize_with = "null_as_empty")]
    pub event_id: String,
    /// Absent or `null` → `"profiles"`.
    #[serde(default = "default_topic", deserialize_with = "null_as_profiles")]
    pub topic: String,
    pub kind: EventKind,
    /// The publishing *component's* id (not the broker's).
    #[serde(deserialize_with = "null_as_empty")]
    pub sender_instance_id: String,
    /// Full path of the file the event is about.
    #[serde(deserialize_with = "null_as_empty")]
    pub scope_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", serialize_with = "ser_opt_f64")]
    pub position_seconds: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none", serialize_with = "ser_opt_f64")]
    pub duration_seconds: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view_count: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_liked: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rating: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub list_id: Option<String>,
    /// Plain thumbnail bytes, base64 (legacy producers; the pipe is user-only after F6).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_base64: Option<String>,
    /// Personal event data (watched path, position, thumbnail) sealed with a key derived from the sending profile's
    /// vault key (C# `SealedPayloadBase64`, added by the "no watch history without a logged-in profile" change). The
    /// broker relays it opaquely; only `sa-profiles` seals/opens it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sealed_payload_base64: Option<String>,
    /// `DateTimeOffset` text ([`format_timestamp`]); absent → now. Validated on read ([`parse_timestamp`]).
    #[serde(default = "now_text", deserialize_with = "de_timestamp")]
    pub timestamp_utc: String,
}

fn default_topic() -> String {
    "profiles".into()
}

fn now_text() -> String {
    format_timestamp(Utc::now())
}

impl StateSyncEvent {
    /// A new event with a fresh id and timestamp.
    pub fn new(topic: crate::Topic, kind: EventKind, sender_instance_id: &str, scope_path: &str) -> Self {
        Self {
            event_id: sa_core::text::guid_n(),
            topic: topic.wire().into(),
            kind,
            sender_instance_id: sender_instance_id.into(),
            scope_path: scope_path.into(),
            username: None,
            video_key: None,
            video_path: None,
            position_seconds: None,
            duration_seconds: None,
            view_count: None,
            is_liked: None,
            rating: None,
            list_id: None,
            payload_base64: None,
            sealed_payload_base64: None,
            timestamp_utc: now_text(),
        }
    }

    /// The parsed `timestampUtc` (UTC); `None` only for a value constructed in Rust with invalid text.
    pub fn timestamp(&self) -> Option<DateTime<Utc>> {
        parse_timestamp(&self.timestamp_utc).map(|t| t.with_timezone(&Utc))
    }

    /// The topic used for routing: C# `Topic ?? "profiles"`. A JSON `null` topic is already read as `"profiles"`; an
    /// empty or blank topic is routed as it is (only `*` subscriptions match it), like C#.
    pub fn routing_topic(&self) -> &str {
        &self.topic
    }

    /// True when every `double` member is finite (STJ throws on NaN/±Infinity, so such an event never crosses the
    /// pipe; local dispatch still happens).
    pub fn is_wire_serializable(&self) -> bool {
        self.position_seconds.is_none_or(f64::is_finite) && self.duration_seconds.is_none_or(f64::is_finite)
    }
}

/// `SqliteProfileStore.WriteKind` names in C# ordinal order (0..15; `PurgeLegacyVideoKeys`/`RetagOwner` were added by
/// the C# "no watch history without a logged-in profile" change, after win-sync was written). `FlushBarrier` is never
/// accepted from a request (F4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WriteKind {
    UpsertProfile,
    DeleteProfileCascade,
    UpsertVideo,
    DeleteVideo,
    ReplaceAllVideosAndProgress,
    UpsertWatchProgress,
    UpsertCustomList,
    DeleteCustomList,
    UpsertCustomListItem,
    DeleteCustomListItem,
    ReplaceCustomListItems,
    UpsertThumbnail,
    MigrateVideoKey,
    PurgeLegacyVideoKeys,
    RetagOwner,
    FlushBarrier,
}

impl WriteKind {
    /// Every kind in ordinal order.
    pub const ALL: [WriteKind; 16] = [
        WriteKind::UpsertProfile,
        WriteKind::DeleteProfileCascade,
        WriteKind::UpsertVideo,
        WriteKind::DeleteVideo,
        WriteKind::ReplaceAllVideosAndProgress,
        WriteKind::UpsertWatchProgress,
        WriteKind::UpsertCustomList,
        WriteKind::DeleteCustomList,
        WriteKind::UpsertCustomListItem,
        WriteKind::DeleteCustomListItem,
        WriteKind::ReplaceCustomListItems,
        WriteKind::UpsertThumbnail,
        WriteKind::MigrateVideoKey,
        WriteKind::PurgeLegacyVideoKeys,
        WriteKind::RetagOwner,
        WriteKind::FlushBarrier,
    ];

    /// The C# enum member name (the wire value producers write).
    pub const fn name(self) -> &'static str {
        match self {
            WriteKind::UpsertProfile => "UpsertProfile",
            WriteKind::DeleteProfileCascade => "DeleteProfileCascade",
            WriteKind::UpsertVideo => "UpsertVideo",
            WriteKind::DeleteVideo => "DeleteVideo",
            WriteKind::ReplaceAllVideosAndProgress => "ReplaceAllVideosAndProgress",
            WriteKind::UpsertWatchProgress => "UpsertWatchProgress",
            WriteKind::UpsertCustomList => "UpsertCustomList",
            WriteKind::DeleteCustomList => "DeleteCustomList",
            WriteKind::UpsertCustomListItem => "UpsertCustomListItem",
            WriteKind::DeleteCustomListItem => "DeleteCustomListItem",
            WriteKind::ReplaceCustomListItems => "ReplaceCustomListItems",
            WriteKind::UpsertThumbnail => "UpsertThumbnail",
            WriteKind::MigrateVideoKey => "MigrateVideoKey",
            WriteKind::PurgeLegacyVideoKeys => "PurgeLegacyVideoKeys",
            WriteKind::RetagOwner => "RetagOwner",
            WriteKind::FlushBarrier => "FlushBarrier",
        }
    }

    /// Parses a request's `kind` (win-sync §5.3 port): member names case-insensitively (surrounding whitespace
    /// ignored, like `Enum.TryParse`) and the decimal integers 0–14. `FlushBarrier`/15, other numbers, flag lists and
    /// anything else are rejected (F4).
    pub fn parse_request_kind(kind: &str) -> Option<WriteKind> {
        let k = kind.trim();
        if k.is_empty() {
            return None;
        }
        let parsed = if let Ok(n) = k.parse::<i64>() {
            usize::try_from(n).ok().and_then(|i| WriteKind::ALL.get(i).copied())
        } else {
            WriteKind::ALL.iter().copied().find(|w| w.name().eq_ignore_ascii_case(k))
        };
        parsed.filter(|w| *w != WriteKind::FlushBarrier)
    }

    /// The negative-ack text for an unparseable kind (developer text, never displayed; C# wording).
    pub fn unknown_kind_error(kind: &str) -> String {
        format!("Unknown WriteKind '{kind}'.")
    }
}

/// `{key, encryptedBlob, updatedTicks}` (C# positional record; missing members become `null`/0).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BatchRow {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// base64
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "de_opt_base64")]
    pub encrypted_blob: Option<String>,
    pub updated_ticks: i64,
}

/// `{listId, itemHash, ord, encryptedPathBlob}`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BatchListItem {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub list_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item_hash: Option<String>,
    pub ord: i32,
    /// base64
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "de_opt_base64")]
    pub encrypted_path_blob: Option<String>,
}

/// A `W:` line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StateSyncWriteRequest {
    /// Idempotency key (SQLite `processed_requests`). Absent → a fresh id; blank → assigned on submit.
    #[serde(default = "sa_core::text::guid_n", deserialize_with = "null_as_empty")]
    pub request_id: String,
    #[serde(deserialize_with = "null_as_empty")]
    pub sender_instance_id: String,
    #[serde(deserialize_with = "null_as_empty")]
    pub db_path: String,
    /// WriteKind name ([`WriteKind::parse_request_kind`]).
    #[serde(deserialize_with = "null_as_empty")]
    pub kind: String,
    #[serde(default, deserialize_with = "null_as_empty")]
    pub username: String,
    #[serde(default, deserialize_with = "null_as_empty")]
    pub key: String,
    #[serde(default, deserialize_with = "null_as_empty")]
    pub secondary_key: String,
    #[serde(default)]
    pub ord: i32,
    /// base64
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "de_opt_base64")]
    pub blob: Option<String>,
    /// base64
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "de_opt_base64")]
    pub secondary_blob: Option<String>,
    /// .NET UTC ticks (100 ns since 0001-01-01).
    #[serde(default)]
    pub updated_ticks: i64,
    #[serde(default = "minus_one")]
    pub view_count: i32,
    #[serde(default)]
    pub view_count_delta: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub salt_base64: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password_hash_base64: Option<String>,
    #[serde(default)]
    pub iterations: i32,
    #[serde(default = "min_date", deserialize_with = "de_timestamp")]
    pub created_at_utc: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_videos: Option<Vec<BatchRow>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_progress: Option<Vec<BatchRow>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_list_items: Option<Vec<BatchListItem>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_commit_events: Option<Vec<StateSyncEvent>>,
}

fn minus_one() -> i32 {
    -1
}

fn min_date() -> String {
    MIN_DATE_TEXT.into()
}

/// `default(DateTimeOffset)` as STJ writes it.
pub const MIN_DATE_TEXT: &str = "0001-01-01T00:00:00+00:00";

impl StateSyncWriteRequest {
    /// A request with the C# defaults (`viewCount` -1, `createdAtUtc` = `default(DateTimeOffset)`) and a fresh id.
    pub fn new(sender_instance_id: &str, db_path: &str, kind: WriteKind) -> Self {
        Self {
            request_id: sa_core::text::guid_n(),
            sender_instance_id: sender_instance_id.into(),
            db_path: db_path.into(),
            kind: kind.name().into(),
            username: String::new(),
            key: String::new(),
            secondary_key: String::new(),
            ord: 0,
            blob: None,
            secondary_blob: None,
            updated_ticks: 0,
            view_count: -1,
            view_count_delta: 0,
            salt_base64: None,
            password_hash_base64: None,
            iterations: 0,
            created_at_utc: min_date(),
            batch_videos: None,
            batch_progress: None,
            batch_list_items: None,
            post_commit_events: None,
        }
    }

    /// The parsed kind ([`WriteKind::parse_request_kind`]).
    pub fn write_kind(&self) -> Option<WriteKind> {
        WriteKind::parse_request_kind(&self.kind)
    }

    /// True when every embedded post-commit event is serializable (see [`StateSyncEvent::is_wire_serializable`]).
    pub fn is_wire_serializable(&self) -> bool {
        self.post_commit_events.as_ref().is_none_or(|v| v.iter().all(StateSyncEvent::is_wire_serializable))
    }
}

/// An `A:` line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StateSyncWriteAck {
    #[serde(deserialize_with = "null_as_empty")]
    pub request_id: String,
    #[serde(default)]
    pub committed: bool,
    #[serde(default)]
    pub is_duplicate: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl StateSyncWriteAck {
    pub fn new(request_id: &str, committed: bool, is_duplicate: bool, error: Option<String>) -> Self {
        Self { request_id: request_id.into(), committed, is_duplicate, error }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Line encoding / decoding

/// Line prefix of a write request.
pub const WRITE_PREFIX: &str = "W:";
/// Line prefix of a write ack.
pub const ACK_PREFIX: &str = "A:";

/// The JSON of an event line (no terminator); `None` when a `double` is non-finite (C# serialization throws).
pub fn event_line(event: &StateSyncEvent) -> Option<String> {
    if !event.is_wire_serializable() {
        return None;
    }
    serde_json::to_string(event).ok()
}

/// `W:` + JSON; `None` when not serializable.
pub fn write_request_line(request: &StateSyncWriteRequest) -> Option<String> {
    if !request.is_wire_serializable() {
        return None;
    }
    serde_json::to_string(request).ok().map(|j| format!("{WRITE_PREFIX}{j}"))
}

/// `A:` + JSON.
pub fn ack_line(ack: &StateSyncWriteAck) -> String {
    format!("{ACK_PREFIX}{}", serde_json::to_string(ack).unwrap_or_default())
}

/// Parses an event JSON (an un-prefixed line); any failure → `None` (the line is dropped).
pub fn parse_event(json: &str) -> Option<StateSyncEvent> {
    parse_stj(json)
}

/// Parses a write request JSON (the text after `W:`).
pub fn parse_write_request(json: &str) -> Option<StateSyncWriteRequest> {
    parse_stj(json)
}

/// Parses an ack JSON (the text after `A:`).
pub fn parse_ack(json: &str) -> Option<StateSyncWriteAck> {
    parse_stj(json)
}

/// Reads one wire JSON like STJ: a repeated member name takes the last value (serde would reject it), so the text is
/// read into a [`serde_json::Value`] first.
fn parse_stj<T: serde::de::DeserializeOwned>(json: &str) -> Option<T> {
    if nesting_depth(json) > STJ_MAX_DEPTH {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    serde_json::from_value(value).ok()
}

/// `JsonSerializerOptions.MaxDepth` default: deeper documents fail to read (serde_json's own limit is 128).
const STJ_MAX_DEPTH: usize = 64;

/// The deepest object/array nesting of `json` (strings skipped); only meaningful for text that then parses.
fn nesting_depth(json: &str) -> usize {
    let (mut depth, mut max, mut in_string, mut escaped) = (0usize, 0usize, false, false);
    for b in json.bytes() {
        if in_string {
            match b {
                _ if escaped => escaped = false,
                b'\\' => escaped = true,
                b'"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' | b'[' => {
                depth += 1;
                max = max.max(depth);
            }
            b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    max
}

// ---------------------------------------------------------------------------------------------------------------
// DateTimeOffset text

/// `DateTimeOffset` round-trip text as STJ writes a UTC value: `yyyy-MM-ddTHH:mm:ss[.fffffff]+00:00`, fraction in
/// 100 ns ticks with trailing zeros trimmed (dot dropped when zero).
pub fn format_timestamp(t: DateTime<Utc>) -> String {
    let base = t.format("%Y-%m-%dT%H:%M:%S").to_string();
    let ticks = t.timestamp_subsec_nanos().min(999_999_999) / 100;
    if ticks == 0 {
        format!("{base}+00:00")
    } else {
        let frac = format!("{ticks:07}");
        format!("{base}.{}+00:00", frac.trim_end_matches('0'))
    }
}

/// Parses ISO 8601 text exactly as STJ reads a `DateTimeOffset` (checked against .NET 10 by the C# interop corpus):
/// `yyyy-MM-dd` alone (local midnight), or `yyyy-MM-ddTHH:mm` with optional `:ss`, after the seconds an optional
/// `.` with 0–16 fraction digits (truncated to 7), then `Z`, `±hh`, `±hh:mm` (|offset| ≤ 14:00) or nothing (local
/// time). The UTC instant must lie within .NET's `DateTimeOffset` range (years 1–9999).
pub fn parse_timestamp(text: &str) -> Option<DateTime<FixedOffset>> {
    let s = text.as_bytes();
    let num = |from: usize, len: usize| -> Option<u32> {
        let part = s.get(from..from + len)?;
        if !part.iter().all(u8::is_ascii_digit) {
            return None;
        }
        std::str::from_utf8(part).ok()?.parse().ok()
    };
    let year = num(0, 4)? as i32;
    if s.get(4) != Some(&b'-') || s.get(7) != Some(&b'-') {
        return None;
    }
    let date = NaiveDate::from_ymd_opt(year, num(5, 2)?, num(8, 2)?)?;
    if s.len() == 10 {
        return local_offset(NaiveDateTime::new(date, NaiveTime::MIN));
    }
    if s[10] != b'T' || s.get(13) != Some(&b':') {
        return None;
    }
    let (h, m) = (num(11, 2)?, num(14, 2)?);
    let mut i = 16;
    let (mut sec, mut nanos) = (0, 0u32);
    if s.get(i) == Some(&b':') {
        sec = num(i + 1, 2)?;
        i += 3;
        if s.get(i) == Some(&b'.') {
            i += 1;
            let start = i;
            while i < s.len() && s[i].is_ascii_digit() {
                i += 1;
            }
            // STJ quirk: a bare `.` is accepted only when an offset follows (`…:33.Z`, never `…:33.`).
            if i - start > 16 || (i == start && i == s.len()) {
                return None;
            }
            let seven: String = text[start..i].chars().chain(std::iter::repeat('0')).take(7).collect();
            nanos = seven.parse::<u32>().ok()? * 100;
        }
    }
    let naive = NaiveDateTime::new(date, NaiveTime::from_hms_nano_opt(h, m, sec, nanos)?);
    let offset_secs = match &s[i..] {
        [] => return local_offset(naive),
        [b'Z'] => 0,
        [sign @ (b'+' | b'-'), rest @ ..] => {
            let (oh, om) = match rest {
                [_, _] => (num(i + 1, 2)?, 0),
                [_, _, b':', _, _] => (num(i + 1, 2)?, num(i + 4, 2)?),
                _ => return None,
            };
            if om > 59 || oh * 60 + om > 14 * 60 {
                return None;
            }
            let secs = (oh * 3600 + om * 60) as i32;
            if *sign == b'-' { -secs } else { secs }
        }
        _ => return None,
    };
    with_offset(naive, FixedOffset::east_opt(offset_secs)?)
}

/// `naive` at `offset`, rejected when the UTC instant leaves .NET's range (0001-01-01 … 9999-12-31T23:59:59.9999999).
fn with_offset(naive: NaiveDateTime, offset: FixedOffset) -> Option<DateTime<FixedOffset>> {
    let utc = naive.checked_sub_offset(offset)?;
    if !(1..=9999).contains(&utc.year()) {
        return None;
    }
    offset.from_local_datetime(&naive).single()
}

/// A time without an offset is local (STJ: `DateTimeKind.Local`); a time in a DST gap takes the offset in effect
/// just before it, like .NET does not reject such times.
fn local_offset(naive: NaiveDateTime) -> Option<DateTime<FixedOffset>> {
    let offset = match chrono::Local.offset_from_local_datetime(&naive) {
        chrono::LocalResult::Single(o) | chrono::LocalResult::Ambiguous(o, _) => o,
        chrono::LocalResult::None => chrono::Local.offset_from_utc_datetime(&(naive - chrono::TimeDelta::hours(3))),
    };
    with_offset(naive, offset)
}

/// .NET UTC ticks (100 ns since 0001-01-01T00:00:00Z) of a UTC time.
pub fn dotnet_ticks(t: DateTime<Utc>) -> i64 {
    const UNIX_EPOCH_TICKS: i64 = 621_355_968_000_000_000;
    UNIX_EPOCH_TICKS + t.timestamp() * 10_000_000 + i64::from(t.timestamp_subsec_nanos() / 100)
}

// ---------------------------------------------------------------------------------------------------------------
// serde helpers

fn null_as_empty<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(Option::<String>::deserialize(d)?.unwrap_or_default())
}

fn null_as_profiles<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(Option::<String>::deserialize(d)?.unwrap_or_else(default_topic))
}

fn de_timestamp<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    // `null` into a non-nullable DateTimeOffset fails in STJ.
    let s = String::deserialize(d)?;
    if parse_timestamp(&s).is_none() {
        return Err(serde::de::Error::custom("invalid DateTimeOffset"));
    }
    Ok(s)
}

fn de_opt_base64<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    match Option::<String>::deserialize(d)? {
        None => Ok(None),
        Some(s) => canonical_base64(&s).map(Some).ok_or_else(|| serde::de::Error::custom("invalid base64")),
    }
}

/// .NET's base64 decoder (used by STJ for `byte[]`) skips these anywhere, also between the padding characters.
fn is_base64_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\r' | '\n')
}

/// Validates base64 text like STJ reads a `byte[]` (standard alphabet, padded; spaces, tabs, CR and LF skipped
/// anywhere) and returns it canonically encoded, as C# would write the bytes back; `None` when STJ would fail.
pub fn canonical_base64(text: &str) -> Option<String> {
    let engine = &base64::engine::general_purpose::STANDARD;
    if !text.contains(is_base64_space) {
        // Already canonical whenever it decodes (padding and trailing bits are strict).
        return engine.decode(text).ok().map(|_| text.to_owned());
    }
    let compact: String = text.chars().filter(|c| !is_base64_space(*c)).collect();
    engine.decode(&compact).ok().map(|bytes| engine.encode(bytes))
}

/// STJ writes doubles in shortest round-trip form; integral values carry no fraction (`77`, not `77.0`).
fn ser_opt_f64<S: Serializer>(v: &Option<f64>, s: S) -> Result<S::Ok, S::Error> {
    match v {
        Some(x) if x.is_finite() && x.fract() == 0.0 && x.abs() < 1e15 && !(*x == 0.0 && x.is_sign_negative()) => {
            s.serialize_i64(*x as i64)
        }
        Some(x) => s.serialize_f64(*x),
        None => s.serialize_none(),
    }
}
