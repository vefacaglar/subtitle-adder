//! Behavior parity with the C# app's EngineKit (Windows). The real C# code was run once on a fixed set of inputs and
//! its outputs were frozen under `tests/fixtures/csharp/` (see the README there); the Rust port must reproduce them:
//! * thousands of generated sentence-editing sequences (timing clamps, nudges, merge, delete, split by words and by
//!   grapheme-proportional text, dialogue dashes, speakers), including the uids each final line kept and the exact
//!   stdin JSON the engine receives (byte for byte);
//! * `DownloadPhaseTracker` stage sequences;
//! * `EngineEvent.Decode` on hand-written lines, sentence fields included (the only intended difference: an unknown
//!   event type is an error in C# and skipped in Rust, UNIFIED.md §3.7);
//! * `StringInfo.LengthInTextElements` grapheme counts;
//! * the C# `EngineClient` driven by the scripted fake engine, against `ProcessEngineClient` on the same scripts.
#![cfg(windows)]

mod common;

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use sa_core::cancel::CancellationToken;
use sa_core::engine_types::{EngineError, EngineStage, Sentence, SubtitleFormat};
use sa_core::l10n::UiLanguage;
use sa_enginekit::client::{EngineClient, TranscribeOptions};
use sa_enginekit::download_phase::{DownloadKind, DownloadPhaseTracker};
use sa_enginekit::editing::{SentenceListExt, grapheme_len};
use sa_enginekit::events::{EngineEvent, decode};
use serde_json::{Value, json};
use uuid::Uuid;

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/csharp").join(name)
}

/// A fixture's text; `.gz` files are gunzipped.
fn fixture_text(name: &str) -> String {
    let bytes = std::fs::read(fixture_path(name)).unwrap_or_else(|e| panic!("fixture {name}: {e}"));
    if name.ends_with(".gz") {
        let mut text = String::new();
        flate2::read::GzDecoder::new(bytes.as_slice()).read_to_string(&mut text).expect("gzip fixture");
        text
    } else {
        String::from_utf8(bytes).expect("UTF-8 fixture")
    }
}

/// One JSON object per line. Only for fixtures whose inputs hold no floats that need exact parsing (see
/// `frozen_case_text`).
fn fixture(name: &str) -> Vec<Value> {
    fixture_text(name).lines().map(|line| serde_json::from_str(line).expect("fixture line is JSON")).collect()
}

/// The C# output line of a fixture row, parsed.
fn csharp_output(row: &Value) -> Value {
    serde_json::from_str(row["csharp"].as_str().expect("csharp line")).expect("C# output is JSON")
}

/// Runs one case through the Rust port in the fixture's output shape.
fn rust_edit(case: &Value) -> Value {
    let mut list: Vec<Sentence> = serde_json::from_value(case["sentences"].clone()).unwrap();
    let origin: HashMap<Uuid, usize> = list.iter().enumerate().map(|(i, s)| (s.uid, i)).collect();
    let mut results = Vec::new();
    for op in case["ops"].as_array().unwrap() {
        let i = op.get("i").and_then(Value::as_u64).unwrap_or(0) as usize;
        let f = |k: &str| op[k].as_f64().unwrap();
        let r = match op["op"].as_str().unwrap() {
            "set_start" => json!(list.set_start(f("v"), i)),
            "set_end" => json!(list.set_end(f("v"), i)),
            "nudge_start" => json!(list.nudge_start(i, f("d"))),
            "nudge_end" => json!(list.nudge_end(i, f("d"))),
            // The C# rule keeps the review flag (the Windows view model clears it).
            "set_text" => json!(list.set_text_keeping_review(op["text"].as_str().unwrap(), i)),
            "set_speaker" => json!(list.set_speaker(op["speaker"].as_i64(), i)),
            "merge" => json!(list.merge_with_next(i)),
            "delete" => json!(list.delete_sentence(i)),
            "split" => json!(list.split_sentence(i, f("t"))),
            "can_split" => json!(list.can_split(i, f("t"))),
            "active" => json!(list.active_index(f("t"))),
            "dashes" => json!(list.dialogue_dashes()),
            "display" => if i < list.len() { json!(list.display_text(i)) } else { Value::Null },
            "speakers" => json!(list.speakers()),
            "next_speaker" => json!(list.next_new_speaker()),
            other => panic!("unknown op {other}"),
        };
        results.push(r);
    }
    let mut fresh: HashMap<Uuid, usize> = HashMap::new();
    let uids: Vec<String> = list
        .iter()
        .map(|s| match origin.get(&s.uid) {
            Some(o) => format!("old{o}"),
            None => {
                let n = fresh.len();
                format!("new{}", *fresh.entry(s.uid).or_insert(n))
            }
        })
        .collect();
    json!({"results": results, "final": Sentence::encode_list(&list).unwrap(), "uids": uids})
}

/// The engine stdin JSON must be identical; the only tolerated spelling differences are .NET's `\uXXXX` escapes of
/// non-BMP characters and exponent notation (both decode to the same value).
fn assert_same_payload(csharp: &str, rust: &str, context: &str) {
    if csharp.contains("\\u") || csharp.contains('E') {
        let a: Value = serde_json::from_str(csharp).unwrap();
        let b: Value = serde_json::from_str(rust).unwrap();
        assert_eq!(a, b, "{context}");
    } else {
        assert_eq!(csharp, rust, "{context}");
    }
}

/// Small deterministic generator (no extra dev-dependency).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // SplitMix64.
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
    /// A time with at most 4 decimals in `[lo, hi)`.
    fn time(&mut self, lo: f64, hi: f64) -> f64 {
        let span = ((hi - lo) * 10_000.0).max(1.0) as u64;
        ((lo * 10_000.0).round() + self.below(span) as f64) / 10_000.0
    }
    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len() as u64) as usize]
    }
}

const WORDS: [&str; 22] = [
    "Merhaba", "dünya", "İyiyim.", "Nasılsın?", "ğüşıöç", "a", "Bir", "iki", "üç", "dört", "e\u{301}", "👍🏽", "👨‍👩‍👧",
    "🇹🇷", "\u{1100}\u{1161}\u{11A8}", "नमस्ते", "-", "–", "x,y", "\"quote\"", "back\\slash", "tab\there",
];

fn text(rng: &mut Rng) -> String {
    let n = rng.below(6);
    let mut out = String::new();
    if rng.chance(15) {
        out.push_str(rng.pick(&["- ", "– ", "-", "  "]));
    }
    for i in 0..n {
        if i > 0 {
            out.push_str(rng.pick(&[" ", " ", " ", "  ", "\t", "\n", "\u{3000}"]));
        }
        out.push_str(rng.pick(&WORDS));
    }
    if rng.chance(10) {
        out.push(' ');
    }
    out
}

fn sentence_list(rng: &mut Rng) -> Vec<Value> {
    let count = rng.below(6) + usize::from(rng.chance(90)) as u64;
    let mut t = rng.time(0.0, 2.0);
    let mut list = Vec::new();
    for id in 1..=count {
        let start = if rng.chance(10) { (t - rng.time(0.0, 0.5)).max(0.0) } else { t + rng.time(0.0, 0.8) };
        let end = start + if rng.chance(8) { rng.time(0.0, 0.25) } else { rng.time(0.05, 4.0) };
        let mut words = Vec::new();
        let word_count = if rng.chance(30) { 0 } else { rng.below(5) };
        let mut w = start;
        for _ in 0..word_count {
            let ws = w + rng.time(0.0, 0.3);
            let we = ws + rng.time(0.0, 0.9);
            words.push(json!({"start": ws, "end": we, "text": if rng.chance(10) { " " } else { rng.pick(&WORDS) }}));
            w = we;
        }
        let mut s = json!({"id": if rng.chance(10) { 99 } else { id }, "start": start, "end": end, "text": text(rng), "words": words});
        if rng.chance(60) {
            s["speaker"] = if rng.chance(15) { Value::Null } else { json!(rng.below(3)) };
        }
        if rng.chance(20) {
            s["needs_review"] = json!(true);
        }
        list.push(s);
        t = end;
    }
    list
}

fn ops(rng: &mut Rng, len: usize) -> Vec<Value> {
    let n = rng.below(14) + 1;
    let index = |rng: &mut Rng| rng.below(len as u64 + 2);
    let delta = |rng: &mut Rng| [0.1, -0.1, 0.05, -0.05, 1.0, -1.0, 0.0][rng.below(7) as usize];
    (0..n)
        .map(|_| match rng.below(15) {
            0 => json!({"op": "set_start", "i": index(rng), "v": rng.time(-1.0, 25.0)}),
            1 => json!({"op": "set_end", "i": index(rng), "v": rng.time(-1.0, 25.0)}),
            2 => json!({"op": "nudge_start", "i": index(rng), "d": delta(rng)}),
            3 => json!({"op": "nudge_end", "i": index(rng), "d": delta(rng)}),
            4 => json!({"op": "set_text", "i": index(rng), "text": text(rng)}),
            5 => json!({"op": "set_speaker", "i": index(rng), "speaker": if rng.chance(30) { Value::Null } else { json!(rng.below(4)) }}),
            6 => json!({"op": "merge", "i": index(rng)}),
            7 => json!({"op": "delete", "i": index(rng)}),
            8 | 9 => json!({"op": "split", "i": index(rng), "t": rng.time(0.0, 25.0)}),
            10 => json!({"op": "can_split", "i": index(rng), "t": rng.time(0.0, 25.0)}),
            11 => json!({"op": "active", "t": rng.time(0.0, 25.0)}),
            12 => json!({"op": "dashes"}),
            13 => json!({"op": "display", "i": index(rng)}),
            _ => json!({"op": if rng.chance(50) { "speakers" } else { "next_speaker" }}),
        })
        .collect()
}

/// The 8000 generated cases plus the hand-picked edge cases, exactly as they were handed to the C# code.
fn edit_cases() -> Vec<Value> {
    let mut rng = Rng(0x5EED_0001);
    let mut cases = Vec::new();
    for _ in 0..8000 {
        let sentences = sentence_list(&mut rng);
        let ops = ops(&mut rng, sentences.len());
        cases.push(json!({"sentences": sentences, "ops": ops}));
    }
    cases.push(json!({"sentences": [{"id": 1, "start": 0, "end": 4, "text": "Bir iki üç dört", "words": []}],
        "ops": [{"op": "split", "i": 0, "t": 3.5}]}));
    cases.push(json!({"sentences": [{"id": 1, "start": 0, "end": 3, "text": "👨‍👩‍👧 a\u{301}bc", "words": [{"start": 0.2, "end": 0.4, "text": "x"}]}],
        "ops": [{"op": "split", "i": 0, "t": 1.0}]}));
    cases.push(json!({"sentences": [{"id": 1, "start": 1.2345, "end": 5.2, "text": "a"}, {"id": 2, "start": 5.05, "end": 6, "text": "b"}],
        "ops": [{"op": "nudge_start", "i": 1, "d": -0.1}, {"op": "nudge_start", "i": 1, "d": 0.1}, {"op": "set_start", "i": 0, "v": 1.2345}]}));
    cases
}

/// The frozen `case` of an `edit.jsonl.gz` line as written (`{"case":<case>,"csharp":"..."}`). Read as text, not
/// parsed: serde_json's default float parsing is not always correctly rounded, so the Rust side runs the generated
/// in-memory values (what the C# code parsed) and the frozen text only proves the generator did not drift.
fn frozen_case_text(line: &str) -> &str {
    let rest = line.strip_prefix(r#"{"case":"#).expect("edit fixture line starts with the case");
    &rest[..rest.rfind(r#","csharp":"#).expect("edit fixture line has the C# output")]
}

#[test]
fn sentence_editing_matches_the_csharp_engine_kit() {
    let lines = fixture_text("edit.jsonl.gz");
    let lines: Vec<&str> = lines.lines().collect();
    let cases = edit_cases();
    assert_eq!(lines.len(), cases.len());
    let mut splits = 0;
    for (n, (case, line)) in cases.iter().zip(&lines).enumerate() {
        assert_eq!(frozen_case_text(line), serde_json::to_string(case).unwrap(), "case {n}: the generator drifted");
        let row: Value = serde_json::from_str(line).unwrap();
        let expected = csharp_output(&row);
        let actual = rust_edit(case);
        let context = format!("case {n}: {case}");
        assert_eq!(expected["results"], actual["results"], "{context}");
        assert_eq!(expected["uids"], actual["uids"], "{context}");
        assert_same_payload(expected["final"].as_str().unwrap(), actual["final"].as_str().unwrap(), &context);
        splits += case["ops"]
            .as_array()
            .unwrap()
            .iter()
            .zip(expected["results"].as_array().unwrap())
            .filter(|(op, r)| op["op"] == "split" && **r == json!(true))
            .count();
    }
    // The cases must actually exercise the split paths.
    assert!(splits > 200, "only {splits} successful splits");
}

#[test]
fn download_phase_tracking_matches_the_csharp_engine_kit() {
    let rows = fixture("tracker.jsonl");
    assert_eq!(rows.len(), 1000);
    for row in &rows {
        let case = &row["case"];
        let init = &case["init"];
        let mut tracker =
            DownloadPhaseTracker::new(init[0].as_bool().unwrap(), init[1].as_bool().unwrap(), init[2].as_bool().unwrap());
        let labels: Vec<Value> = case["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| match tracker.update(EngineStage::parse(e[0].as_str().unwrap()), e[1].as_f64()) {
                Some(DownloadKind::Transcription) => json!("Transcription"),
                Some(DownloadKind::Diarization) => json!("Diarization"),
                None => Value::Null,
            })
            .collect();
        assert_eq!(csharp_output(row), Value::Array(labels), "{case}");
    }
}

fn rust_decode(line: &str) -> Value {
    let encode = |s: &[Sentence]| Sentence::encode_list(s).unwrap();
    match decode(line) {
        Err(_) => json!({"invalid": true}),
        Ok(None) => json!({"unknown": true}),
        Ok(Some(EngineEvent::Stage { stage, fraction })) => json!({"stage": stage_raw(stage), "fraction": fraction}),
        Ok(Some(EngineEvent::Partial { language, sentences })) => json!({"partial": language, "sentences": encode(&sentences)}),
        Ok(Some(EngineEvent::Translation { language, sentences })) => {
            json!({"translation": language, "sentences": encode(&sentences)})
        }
        Ok(Some(EngineEvent::Result(r))) => json!({"result": r.language, "duration": r.duration, "audio": r.audio,
            "outputs": r.outputs, "sentences": encode(&r.sentences)}),
        Ok(Some(EngineEvent::Error { message })) => json!({"error": message}),
    }
}

fn stage_raw(stage: EngineStage) -> &'static str {
    match stage {
        EngineStage::ExtractingAudio => "extracting_audio",
        EngineStage::DownloadingModel => "downloading_model",
        EngineStage::LoadingModel => "loading_model",
        EngineStage::Transcribing => "transcribing",
        EngineStage::Diarizing => "diarizing",
        EngineStage::Segmenting => "segmenting",
        EngineStage::Translating => "translating",
        EngineStage::Unknown => "unknown",
    }
}

#[test]
fn event_decoding_matches_the_csharp_engine_kit() {
    let rows = fixture("decode.jsonl");
    assert_eq!(rows.len(), 54);
    let mut mismatches = Vec::new();
    for row in &rows {
        let line = row["line"].as_str().unwrap();
        let expected = csharp_output(row);
        let actual = rust_decode(line);
        // Intended difference (UNIFIED.md §3.7 FIX-W): C# rejects unknown event types; Rust skips them.
        if expected == json!({"unknown": true}) {
            assert_eq!(actual, json!({"unknown": true}), "{line}");
            continue;
        }
        if KNOWN_SA_CORE_DIFFERENCES.iter().any(|known| line.contains(known)) {
            continue;
        }
        let same_sentences = match (expected.get("sentences"), actual.get("sentences")) {
            (Some(Value::String(a)), Some(Value::String(b))) => {
                serde_json::from_str::<Value>(a).unwrap() == serde_json::from_str::<Value>(b).unwrap()
            }
            (a, b) => a == b,
        };
        let strip = |mut v: Value| {
            if let Value::Object(o) = &mut v {
                o.remove("sentences");
            }
            numbers_as_f64(v)
        };
        if !same_sentences || strip(expected.clone()) != strip(actual.clone()) {
            mismatches.push(format!("{line}\n  C#:   {expected}\n  Rust: {actual}"));
        }
    }
    assert!(mismatches.is_empty(), "{} mismatches:\n{}", mismatches.len(), mismatches.join("\n"));
}

/// Wire decoding that lives in `sa_core::engine_types` and differs from C# for inputs the engine never writes:
/// C# reads a word without `end` as `end: 0` (record struct defaults) where sa-core (and Swift) reject it, and C#
/// ids are `int` (sa-core and Swift: 64-bit).
const KNOWN_SA_CORE_DIFFERENCES: [&str; 2] = [r#"{"start": 0, "text": "Şöyle"}"#, r#""id": 2147483648"#];

/// .NET writes the double 1 as `1`; compare numbers by value.
fn numbers_as_f64(v: Value) -> Value {
    match v {
        Value::Number(n) => json!(n.as_f64()),
        Value::Array(a) => Value::Array(a.into_iter().map(numbers_as_f64).collect()),
        Value::Object(o) => Value::Object(o.into_iter().map(|(k, v)| (k, numbers_as_f64(v))).collect()),
        other => other,
    }
}

#[test]
fn grapheme_counts_match_dotnet_string_info() {
    let rows = fixture("graphemes.jsonl");
    assert_eq!(rows.len(), 3000);
    for row in &rows {
        let t = row["text"].as_str().unwrap();
        let expected = row["csharp"].as_u64().unwrap() as usize;
        assert_eq!(grapheme_len(t), expected, "{t:?} ({:?})", t.chars().map(|c| c as u32).collect::<Vec<_>>());
    }
}

// ---------------------------------------------------------------------------------------------------------------
// The engine client: the same scripted fake engine driven by the C# `EngineClient` (frozen) and by
// `ProcessEngineClient`.
// ---------------------------------------------------------------------------------------------------------------

fn rust_events(stream: sa_enginekit::EventStream) -> (Vec<Value>, Option<EngineError>) {
    let (events, error) = common::collect(stream);
    let encode = |s: &[Sentence]| Sentence::encode_list(s).unwrap();
    let events = events
        .iter()
        .map(|e| match e {
            EngineEvent::Stage { stage, fraction } => json!({"stage": stage_raw(*stage), "fraction": fraction}),
            EngineEvent::Partial { language, sentences } => json!({"partial": language, "sentences": encode(sentences)}),
            EngineEvent::Translation { language, sentences } => json!({"translation": language, "sentences": encode(sentences)}),
            EngineEvent::Result(r) => json!({"result": r.language, "duration": r.duration, "audio": r.audio,
                "outputs": r.outputs, "sentences": encode(&r.sentences)}),
            EngineEvent::Error { message } => json!({"error": message}),
        })
        .collect();
    (events, error)
}

fn rust_client_outcome(op: &str, extra: &Value, fake: &common::Fake) -> Value {
    let cancel = CancellationToken::new();
    let client = &fake.client;
    let sentences = || -> Vec<Sentence> { serde_json::from_value(extra["sentences"].clone()).unwrap() };
    let one_shot = |r: Result<Value, EngineError>| match r {
        Ok(v) => (Some(v), None, Vec::new()),
        Err(e) => (None, Some(e), Vec::new()),
    };
    let stream = |(events, error): (Vec<Value>, Option<EngineError>)| (error.is_none().then(|| json!(true)), error, events);
    let (ok, error, events) = match op {
        "models" => one_shot(client.models(&cancel).map(|m| serde_json::to_value(m).unwrap())),
        "delete" => one_shot(client.delete_model(extra["id"].as_str().unwrap(), &cancel).map(|mb| json!(mb))),
        "render" => one_shot(client.render(&sentences(), SubtitleFormat::Srt, &cancel).map(|t| json!(t))),
        "transcribe" => {
            stream(rust_events(client.transcribe(Path::new("v.mp4"), Path::new("out"), &TranscribeOptions::default(), &cancel)))
        }
        "translate" => stream(rust_events(client.translate(&sentences(), "es", Some("tr"), "madlad-3b", &cancel))),
        other => panic!("unknown op {other}"),
    };
    let mut line = serde_json::Map::new();
    if let Some(ok) = ok {
        line.insert("ok".into(), ok);
    }
    if let Some(e) = error {
        let (kind, message) = match e {
            EngineError::Failed(m) => ("Failed", m),
            EngineError::NotFound => ("NotFound", EngineError::not_found_text(None)),
            EngineError::Cancelled => ("other", "cancelled".to_owned()),
        };
        line.insert("error".into(), json!({"kind": kind, "message": message}));
    }
    line.insert("events".into(), Value::Array(events));
    Value::Object(line)
}

#[test]
fn engine_client_outcomes_match_the_csharp_engine_kit() {
    let rows = fixture("client.jsonl");
    // 51 scripts, each in both UI languages.
    assert_eq!(rows.len(), 102);
    let mut mismatches = Vec::new();
    for row in &rows {
        let op = row["op"].as_str().unwrap();
        let extra = &row["extra"];
        let script = row["script"].as_str().unwrap();
        let language = if row["lang"] == "en" { UiLanguage::En } else { UiLanguage::Tr };
        let fake = common::Fake::new(script, language);
        let expected = normalize_outcome(csharp_output(row));
        let actual = normalize_outcome(rust_client_outcome(op, extra, &fake));
        if expected != actual {
            mismatches.push(format!("{op} {} {extra}\n{script}\n  C#:   {expected}\n  Rust: {actual}", row["lang"]));
        }
    }
    assert!(mismatches.is_empty(), "{} mismatches:\n{}", mismatches.len(), mismatches.join("\n\n"));
}

/// Numbers compared by value; sentence payloads compared as JSON values (same keys and numbers).
fn normalize_outcome(v: Value) -> Value {
    fn sentences_as_values(v: Value) -> Value {
        match v {
            Value::Object(o) => Value::Object(
                o.into_iter()
                    .map(|(k, v)| match (k.as_str(), v) {
                        ("sentences", Value::String(s)) => (k, serde_json::from_str(&s).unwrap()),
                        (_, v) => (k, sentences_as_values(v)),
                    })
                    .collect(),
            ),
            Value::Array(a) => Value::Array(a.into_iter().map(sentences_as_values).collect()),
            other => other,
        }
    }
    numbers_as_f64(sentences_as_values(v))
}
