//! Behavior parity with the C# app's EngineKit (Windows): the same inputs go through the real C# code (fixture tool
//! `apps/desktop/tools/fixtures/EngineKitCompat`, which compiles the C# EngineKit sources read-only) and through the
//! Rust port, and the outputs must be identical:
//! * thousands of generated sentence-editing sequences (timing clamps, nudges, merge, delete, split by words and by
//!   grapheme-proportional text, dialogue dashes, speakers), including the uids each final line kept and the exact
//!   stdin JSON the engine receives;
//! * `DownloadPhaseTracker` stage sequences;
//! * `EngineEvent.Decode` on hand-written lines, sentence fields included (the only intended difference: an unknown
//!   event type is an error in C# and skipped in Rust, UNIFIED.md §3.7).
//!
//! Opt-in: `SA_DOTNET_FIXTURES=1 cargo test -p sa-enginekit --test csharp_compat` (needs the .NET 10 SDK; the tool is
//! built on first use with `dotnet build -c Release`). Without the variable every test returns early.
#![cfg(windows)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use sa_core::engine_types::{EngineStage, Sentence};
use sa_enginekit::download_phase::{DownloadKind, DownloadPhaseTracker};
use sa_enginekit::editing::{SentenceListExt, grapheme_len};
use sa_enginekit::events::{EngineEvent, decode};
use serde_json::{Value, json};
use uuid::Uuid;

fn enabled() -> bool {
    let on = std::env::var("SA_DOTNET_FIXTURES").is_ok_and(|v| v == "1");
    if !on {
        eprintln!("skipped: set SA_DOTNET_FIXTURES=1 to run the C# compatibility tests");
    }
    on
}

fn tool_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/fixtures/EngineKitCompat")
}

fn tool_exe() -> PathBuf {
    // Tests run in parallel; only one may build the tool.
    static BUILD: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = BUILD.lock().unwrap_or_else(|e| e.into_inner());
    let exe = tool_dir().join("bin/Release/net10.0/EngineKitCompat.exe");
    let program = tool_dir().join("Program.cs");
    let stale = match (std::fs::metadata(&exe).and_then(|m| m.modified()), std::fs::metadata(&program).and_then(|m| m.modified())) {
        (Ok(built), Ok(source)) => built < source,
        _ => true,
    };
    if stale {
        let status = Command::new("dotnet")
            .args(["build", "-c", "Release", "-nologo", "-v", "q"])
            .current_dir(tool_dir())
            .status()
            .expect("dotnet is on PATH");
        assert!(status.success(), "building the EngineKitCompat fixture tool failed");
    }
    exe
}

/// Writes `input` to a temp file, runs `EngineKitCompat <command> <file>` and returns its stdout lines.
fn csharp(command: &str, input: &str) -> Vec<String> {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("input.txt");
    std::fs::write(&file, input).unwrap();
    let out = Command::new(tool_exe()).arg(command).arg(&file).output().expect("EngineKitCompat runs");
    assert!(
        out.status.success(),
        "EngineKitCompat {command} failed: {}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("UTF-8 output").lines().map(str::to_owned).collect()
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

#[test]
fn sentence_editing_matches_the_csharp_engine_kit() {
    if !enabled() {
        return;
    }
    let mut rng = Rng(0x5EED_0001);
    let mut cases = Vec::new();
    for _ in 0..8000 {
        let sentences = sentence_list(&mut rng);
        let ops = ops(&mut rng, sentences.len());
        cases.push(json!({"sentences": sentences, "ops": ops}));
    }
    // Hand-picked edge cases on top of the generated ones.
    cases.push(json!({"sentences": [{"id": 1, "start": 0, "end": 4, "text": "Bir iki üç dört", "words": []}],
        "ops": [{"op": "split", "i": 0, "t": 3.5}]}));
    cases.push(json!({"sentences": [{"id": 1, "start": 0, "end": 3, "text": "👨‍👩‍👧 a\u{301}bc", "words": [{"start": 0.2, "end": 0.4, "text": "x"}]}],
        "ops": [{"op": "split", "i": 0, "t": 1.0}]}));
    cases.push(json!({"sentences": [{"id": 1, "start": 1.2345, "end": 5.2, "text": "a"}, {"id": 2, "start": 5.05, "end": 6, "text": "b"}],
        "ops": [{"op": "nudge_start", "i": 1, "d": -0.1}, {"op": "nudge_start", "i": 1, "d": 0.1}, {"op": "set_start", "i": 0, "v": 1.2345}]}));
    let input = serde_json::to_string(&cases).unwrap();
    let lines = csharp("edit", &input);
    assert_eq!(lines.len(), cases.len());
    let mut splits = 0;
    for (n, (case, line)) in cases.iter().zip(&lines).enumerate() {
        let expected: Value = serde_json::from_str(line).unwrap();
        let actual = rust_edit(case);
        let context = format!("case {n}: {case}");
        assert_eq!(expected["results"], actual["results"], "{context}");
        assert_eq!(expected["uids"], actual["uids"], "{context}");
        assert_same_payload(expected["final"].as_str().unwrap(), actual["final"].as_str().unwrap(), &context);
        splits += case["ops"].as_array().unwrap().iter().zip(expected["results"].as_array().unwrap())
            .filter(|(op, r)| op["op"] == "split" && **r == json!(true))
            .count();
    }
    // The generator must actually exercise the split paths.
    assert!(splits > 200, "only {splits} successful splits");
}

#[test]
fn download_phase_tracking_matches_the_csharp_engine_kit() {
    if !enabled() {
        return;
    }
    let mut rng = Rng(0x5EED_0002);
    let stages = ["downloading_model", "downloading_model", "downloading_model", "loading_model", "extracting_audio", "transcribing", "x"];
    let fractions = [Value::Null, json!(0.0), json!(0.5), json!(0.98), json!(0.99), json!(1.0), json!(0.12)];
    let mut cases = Vec::new();
    for _ in 0..1000 {
        let events: Vec<Value> = (0..rng.below(12))
            .map(|_| json!([stages[rng.below(stages.len() as u64) as usize], fractions[rng.below(fractions.len() as u64) as usize]]))
            .collect();
        cases.push(json!({"init": [rng.chance(50), rng.chance(50), rng.chance(50)], "events": events}));
    }
    let lines = csharp("tracker", &serde_json::to_string(&cases).unwrap());
    assert_eq!(lines.len(), cases.len());
    for (case, line) in cases.iter().zip(&lines) {
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
        assert_eq!(serde_json::from_str::<Value>(line).unwrap(), Value::Array(labels), "{case}");
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

const SENTENCE: &str = r#"{"id": 1, "start": 0, "end": 1.5, "text": "Şöyle <b>&</b>", "words": [{"start": 0, "end": 1.5, "text": "Şöyle"}], "speaker": 1}"#;

fn event_lines() -> Vec<String> {
    let mut lines: Vec<String> = [
        r#"{"event": "stage", "stage": "transcribing", "fraction": 0.5}"#,
        r#"{"event": "stage", "stage": "yeni_asama", "fraction": null}"#,
        r#"{"event": "stage", "stage": "segmenting"}"#,
        r#"{"event": "stage", "stage": "diarizing", "fraction": 1}"#,
        r#"{"event": "stage"}"#,
        r#"{"event": "stage", "stage": null}"#,
        r#"{"event": "stage", "stage": 3}"#,
        r#"{"event": "stage", "stage": "x", "fraction": "0.5"}"#,
        r#"{"event": "stage", "stage": "x", "fraction": true}"#,
        r#"{"event": "error", "message": "olmadı"}"#,
        r#"{"event": "error"}"#,
        r#"{"event": "error", "message": null}"#,
        r#"{"event": "error", "message": 7}"#,
        r#"{"event": "partial", "language": "?", "sentences": []}"#,
        r#"{"event": "partial", "language": "tr"}"#,
        r#"{"event": "partial", "sentences": []}"#,
        r#"{"event": "partial", "language": "tr", "sentences": null}"#,
        r#"{"event": "partial", "language": "tr", "sentences": {}}"#,
        r#"{"event": "translation", "language": "es", "sentences": [{"id": 1, "start": 0, "end": 1, "text": "Hola.", "words": []}]}"#,
        r#"{"event": "translation", "language": null, "sentences": []}"#,
        r#"{"event": "result", "language": "?", "duration": 0, "audio": "C:\\a b.wav", "outputs": [], "sentences": []}"#,
        r#"{"event": "result", "language": "tr", "duration": 3.5, "audio": "a", "outputs": ["a.srt", "a.vtt"], "sentences": [], "extra": {}}"#,
        r#"{"event": "result", "language": "tr", "duration": 1, "audio": "a", "outputs": []}"#,
        r#"{"event": "result", "language": "tr", "duration": 1, "audio": "a", "sentences": []}"#,
        r#"{"event": "result", "language": "tr", "audio": "a", "outputs": [], "sentences": []}"#,
        r#"{"event": "result", "language": "tr", "duration": 1, "audio": "a", "outputs": [1], "sentences": []}"#,
        r#"{"event": "result", "language": "tr", "duration": "1", "audio": "a", "outputs": [], "sentences": []}"#,
        r#"{"stage": "transcribing"}"#,
        "[1]",
        "42",
        r#"{"event": 7}"#,
        "not JSON",
        "",
        r#"{"event": "stage", "stage": "transcribing", "fraction": 0.5} trailing"#,
        r#"{"event": "models", "models": []}"#,
        r#"{"event": "heartbeat"}"#,
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    // Sentence fields inside events.
    for variant in [
        SENTENCE.to_owned(),
        SENTENCE.replace(r#""speaker": 1"#, r#""speaker": null"#),
        SENTENCE.replace(r#", "speaker": 1"#, ""),
        SENTENCE.replace(r#""speaker": 1"#, r#""speaker": "1""#),
        SENTENCE.replace(r#""speaker": 1"#, r#""speaker": 1.5"#),
        SENTENCE.replace(r#""speaker": 1"#, r#""needs_review": true"#),
        SENTENCE.replace(r#""speaker": 1"#, r#""needs_review": "true""#),
        SENTENCE.replace(r#""speaker": 1"#, r#""needs_review": 1"#),
        SENTENCE.replace(r#""speaker": 1"#, r#""needs_review": null"#),
        SENTENCE.replace(r#""speaker": 1"#, r#""uid": "x""#),
        SENTENCE.replace(r#""words": [{"start": 0, "end": 1.5, "text": "Şöyle"}]"#, r#""words": null"#),
        SENTENCE.replace(r#", "words": [{"start": 0, "end": 1.5, "text": "Şöyle"}]"#, ""),
        SENTENCE.replace(r#"{"start": 0, "end": 1.5, "text": "Şöyle"}"#, r#"{"start": 0, "text": "Şöyle"}"#),
        SENTENCE.replace(r#""id": 1"#, r#""id": 1.0"#),
        SENTENCE.replace(r#""id": 1"#, r#""id": 2147483648"#),
        SENTENCE.replace(r#""id": 1, "#, ""),
        SENTENCE.replace(r#""text": "Şöyle <b>&</b>""#, r#""text": null"#),
        SENTENCE.replace(r#""start": 0, "end": 1.5, "text": "Şöyle <b>"#, r#""start": 0.1, "end": 1E-7, "text": "Şöyle <b>"#),
    ] {
        lines.push(format!(r#"{{"event": "partial", "language": "tr", "sentences": [{variant}]}}"#));
    }
    lines
}

#[test]
fn event_decoding_matches_the_csharp_engine_kit() {
    if !enabled() {
        return;
    }
    let lines = event_lines();
    let expected = csharp("decode", &(lines.join("\n") + "\n"));
    assert_eq!(expected.len(), lines.len());
    let mut mismatches = Vec::new();
    for (line, expected) in lines.iter().zip(&expected) {
        let expected: Value = serde_json::from_str(expected).unwrap();
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
    if !enabled() {
        return;
    }
    let mut rng = Rng(0x5EED_0003);
    let pieces = [
        "a", "ı", "İ", "e\u{301}", "\u{301}", "👍", "\u{1F3FD}", "\u{200D}", "\u{200C}", "👩", "🇹", "🇷", "\u{FE0F}", "\u{1100}",
        "\u{1161}", "\u{11A8}", "가", "न", "म", "स", "त", "\u{94D}", "\u{947}", "ক", "\u{9CD}", "ষ", "ക", "\u{D4D}", "ഷ",
        "\u{D4E}", "\u{600}", "\r", "\n", " ", "\t", "က", "\u{1039}", "ក", "\u{17D2}", "\u{E0067}", "😀",
    ];
    let mut texts: Vec<String> = Vec::new();
    for _ in 0..3000 {
        let n = rng.below(8);
        texts.push((0..n).map(|_| rng.pick(&pieces)).collect());
    }
    let input: String = texts.iter().map(|t| serde_json::to_string(t).unwrap() + "\n").collect();
    let lines = csharp("graphemes", &input);
    assert_eq!(lines.len(), texts.len());
    for (t, line) in texts.iter().zip(&lines) {
        assert_eq!(grapheme_len(t).to_string(), *line, "{t:?} ({:?})", t.chars().map(|c| c as u32).collect::<Vec<_>>());
    }
}

// ---------------------------------------------------------------------------------------------------------------
// The engine client: the same scripted fake engine driven by the C# `EngineClient` and by `ProcessEngineClient`.
// ---------------------------------------------------------------------------------------------------------------

mod common;

use sa_core::cancel::CancellationToken;
use sa_core::engine_types::{EngineError, SubtitleFormat};
use sa_core::l10n::UiLanguage;
use sa_enginekit::client::{EngineClient, TranscribeOptions};

const MODELS_OK: &str = r#"{"event": "models", "models": [{"id": "base", "kind": null, "backend": null, "languages": null, "size_mb": 145, "disk_mb": null, "speed": 5, "accuracy": 2, "license": null, "downloaded": true, "extra": [1]}, {"id": "gigaam-v3", "kind": "transcription", "backend": "onnx", "languages": ["ru"], "size_mb": 900, "disk_mb": 901, "speed": 4, "accuracy": 4, "license": "MIT", "downloaded": false}, {"id": "nllb-1.3b", "kind": "translation", "backend": "ctranslate2", "languages": ["tr", "es"], "size_mb": 1336, "speed": 3, "accuracy": 4, "license": "CC-BY-NC-4.0", "downloaded": false}]}"#;
const RESULT: &str = r#"{"event": "result", "language": "tr", "duration": 3.5, "audio": "a.wav", "outputs": ["a.srt"], "sentences": [{"id": 1, "start": 0, "end": 1.25, "text": "Merhaba <i>&</i> dünya", "words": [{"start": 0, "end": 1.25, "text": "Merhaba"}], "speaker": 0}]}"#;
/// Stderr with blank lines, CR LF, a lone trailing CR pair, non-ASCII text and a last line without a newline.
const STDERR_NOISE: &str =
    "err line 1\nerr line 2\nerrraw \\r\\n\\r\\nline 3 \\r\\r\\n\nerr \nerr line 4 ünlü\nerr line 5\nerrraw line 6 without newline";

fn stage_line(stage: &str, fraction: &str) -> String {
    format!(r#"out {{"event": "stage", "stage": "{stage}", "fraction": {fraction}}}"#)
}

/// `(op, extra fields, script)` for every client case; each runs in both UI languages.
fn client_cases() -> Vec<(&'static str, Value, String)> {
    let sentences = json!([
        {"id": 1, "start": 0, "end": 1.5, "text": "Şöyle \"tırnak\" \\ <b>", "words": [{"start": 0, "end": 1.5, "text": "Şöyle"}], "speaker": 1},
        {"id": 2, "start": 1.5, "end": 2, "text": "İyi.", "words": [], "needs_review": true}
    ]);
    let with_sentences = || json!({"sentences": sentences.clone()});
    let small = || json!({"id": "small"});
    let m = |extra: &str| format!("out {MODELS_OK}\n{extra}");
    let models_with = |row: &str| format!(r#"out {{"event": "models", "models": [{row}]}}"#);
    let row = |fields: &str| format!(r#"{{"id": "x", "size_mb": 1, "speed": 1, "accuracy": 1, "downloaded": false{fields}}}"#);
    let deleted = |id: &str, freed: &str| format!(r#"out {{"event": "deleted", "id": "{id}", "freed_mb": {freed}}}"#);
    let raw = |json: &str| json.replace('\\', "\\\\");
    let error = |message: &str| format!(r#"out {{"event": "error", "message": "{message}"}}"#);
    let translation = r#"out {"event": "translation", "language": "es", "sentences": [{"id": 1, "start": 0, "end": 1.5, "text": "Hola.", "words": [], "speaker": 1, "needs_review": true}]}
out {"event": "translation", "language": "es", "sentences": [{"id": 1, "start": 0, "end": 1.5, "text": "Hola.", "words": []}, {"id": 2, "start": 1.5, "end": 2, "text": "Bien.", "words": [], "needs_review": false}]}"#;

    let cases: Vec<(&'static str, Value, String)> = vec![
        ("models", json!({}), m("")),
        ("models", json!({}), format!("outraw {}\\r\\r\\n\\r\\n\\n", raw(MODELS_OK))),
        ("models", json!({}), m("out\nout   ")),
        ("models", json!({}), m("outraw \\r\\r\\n")),
        ("models", json!({}), m(r#"out {"event": "stage", "stage": "x"}"#)),
        ("models", json!({}), m("exit 2")),
        ("models", json!({}), format!("{STDERR_NOISE}\nexit 3")),
        ("models", json!({}), "out".to_owned()),
        ("models", json!({}), "out not json".to_owned()),
        ("models", json!({}), error("boom")),
        ("models", json!({}), format!("{STDERR_NOISE}\n{}\nexit 4", error("Model yok"))),
        ("models", json!({}), r#"out {"event": "error", "message": 7}"#.to_owned()),
        ("models", json!({}), r#"out {"event": "models", "models": {}}"#.to_owned()),
        ("models", json!({}), r#"out {"event": "models"}"#.to_owned()),
        ("models", json!({}), "out [1, 2]".to_owned()),
        ("models", json!({}), models_with(&row(""))),
        ("models", json!({}), models_with(&row(r#", "size_mb": 145.0"#))),
        ("models", json!({}), models_with(&row(r#", "speed": 2147483648"#))),
        ("models", json!({}), models_with(&row(r#", "disk_mb": -5, "license": "MIT AND Apache-2.0""#))),
        ("models", json!({}), models_with(r#"{"id": "x", "speed": 1, "accuracy": 1, "downloaded": false}"#)),
        ("models", json!({}), models_with(&row(r#", "downloaded": "yes""#))),
        ("models", json!({}), models_with(&row(r#", "languages": "tr""#))),
        ("models", json!({}), models_with("7")),
        ("delete", small(), deleted("small", "464")),
        ("delete", small(), deleted("small", "0")),
        ("delete", small(), deleted("small", "-3")),
        ("delete", small(), deleted("small", "464.0")),
        ("delete", small(), deleted("small", "1e3")),
        ("delete", small(), deleted("small", "2147483648")),
        ("delete", small(), deleted("small", "\"464\"")),
        ("delete", small(), deleted("small", "null")),
        ("delete", small(), deleted("base", "464")),
        ("delete", small(), r#"out {"event": "deleted", "id": "small"}"#.to_owned()),
        ("delete", small(), format!("{}\nexit 1", deleted("small", "464"))),
        ("delete", small(), format!("{STDERR_NOISE}\n{}\nexit 1", error("Model indirilmemiş"))),
        ("delete", small(), format!("{STDERR_NOISE}\nexit 9")),
        ("render", with_sentences(), "cat".to_owned()),
        ("render", with_sentences(), "drain\noutraw 1\\r\\n00:00:00,000 --> 00:00:01,500\\r\\n- Şöyle\\r\\n\\r\\n".to_owned()),
        ("render", with_sentences(), format!("drain\n{}", error("render error event"))),
        ("render", with_sentences(), format!("drain\nout partial output\n{STDERR_NOISE}\nexit 2")),
        ("render", with_sentences(), "exit 4".to_owned()),
        (
            "transcribe",
            json!({}),
            format!(
                "{}\nerr noise\n{}\nout {{\"event\": \"stage\", \"stage\": \"yeni\"}}\nout {{\"event\": \"partial\", \"language\": \"?\", \"sentences\": []}}\nout {RESULT}",
                stage_line("extracting_audio", "null"),
                stage_line("transcribing", "0.5")
            ),
        ),
        (
            "transcribe",
            json!({}),
            format!("outraw {{\"event\": \"stage\", \"stage\": \"segmenting\"}}\\r\\n\\n\\r\\n\nout\noutraw {}", raw(RESULT)),
        ),
        (
            "transcribe",
            json!({}),
            format!("{}\n{}\n{}", error("Video bulunamadı"), stage_line("segmenting", "null"), error("ikinci")),
        ),
        ("transcribe", json!({}), format!("out {RESULT}\n{}\nexit 3", error("geç hata"))),
        ("transcribe", json!({}), format!("{}\n{STDERR_NOISE}\nexit 5", stage_line("loading_model", "null"))),
        ("transcribe", json!({}), format!("{}\nexit 1", stage_line("loading_model", "null"))),
        ("transcribe", json!({}), format!("out {RESULT}\nexit 0")),
        (
            "translate",
            with_sentences(),
            format!("drain\n{}\n{}\n{translation}", stage_line("downloading_model", "0.12"), stage_line("translating", "null")),
        ),
        ("translate", with_sentences(), format!("drain\n{}\nexit 1", error("NLLB kaynak dil ister."))),
        ("translate", with_sentences(), format!("{STDERR_NOISE}\nexit 6")),
    ];
    let mut both = Vec::new();
    for (op, extra, script) in cases {
        both.push((op, extra.clone(), script.clone()));
        let mut en = extra;
        en["lang"] = json!("en");
        both.push((op, en, script));
    }
    both
}

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
    if !enabled() {
        return;
    }
    let cases = client_cases();
    let lang = |extra: &Value| if extra["lang"] == "en" { UiLanguage::En } else { UiLanguage::Tr };
    let fakes: Vec<common::Fake> = cases.iter().map(|(_, extra, script)| common::Fake::new(script, lang(extra))).collect();
    let input: Vec<Value> = cases
        .iter()
        .zip(&fakes)
        .map(|((op, extra, _), fake)| {
            let mut case = extra.clone();
            case["op"] = json!(op);
            case["lang"] = json!(if lang(extra) == UiLanguage::En { "en" } else { "tr" });
            case["script"] = json!(fake.path("script.txt"));
            case
        })
        .collect();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("cases.json");
    std::fs::write(&file, serde_json::to_string(&input).unwrap()).unwrap();
    let out = Command::new(tool_exe()).arg("client").arg(common::FAKE_ENGINE).arg(&file).output().expect("EngineKitCompat runs");
    assert!(out.status.success(), "EngineKitCompat client failed: {}", String::from_utf8_lossy(&out.stderr));
    let expected: Vec<String> = String::from_utf8(out.stdout).unwrap().lines().map(str::to_owned).collect();
    assert_eq!(expected.len(), cases.len());
    let mut mismatches = Vec::new();
    for (((op, extra, script), fake), expected) in cases.iter().zip(&fakes).zip(&expected) {
        let expected = normalize_outcome(serde_json::from_str(expected).unwrap());
        let actual = normalize_outcome(rust_client_outcome(op, extra, fake));
        if expected != actual {
            mismatches.push(format!("{op} {extra}\n{script}\n  C#:   {expected}\n  Rust: {actual}"));
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
