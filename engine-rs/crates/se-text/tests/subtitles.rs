//! Port of `engine/tests/test_subtitles.py` (plus a few verified extras from spec/sentences.md).

use se_core::json::parse;
use se_core::types::sentences_from_value;
use se_core::{Num, Sentence, Word};
use se_text::subtitles::{render, write};

fn sample() -> Vec<Sentence> {
    let mut first = Sentence::new(1, 0.0, 1.5, "Merhaba.");
    first.start = Num::Int(0);
    first.words = vec![Word { start: Num::Int(0), end: Num::Float(1.5), text: "Merhaba.".into() }];
    vec![first, Sentence::new(2, 3661.007, 3662.5, "Görüşürüz.")]
}

#[test]
fn test_srt() {
    assert_eq!(
        render(&sample(), "srt").unwrap(),
        "1\n00:00:00,000 --> 00:00:01,500\nMerhaba.\n\n2\n01:01:01,007 --> 01:01:02,500\nGörüşürüz.\n"
    );
}

#[test]
fn test_vtt() {
    assert!(render(&sample(), "vtt").unwrap().starts_with("WEBVTT\n\n00:00:00.000 --> 00:00:01.500\nMerhaba.\n"));
}

#[test]
fn test_json_roundtrip_keeps_unicode() {
    let text = render(&sample(), "json").unwrap();
    assert!(text.contains("Görüşürüz."));
    let back = sentences_from_value(&parse(&text).unwrap()).unwrap();
    assert_eq!(back, sample());
    // Int-ness survives the round trip (`"start": 0`, not `0.0`).
    assert!(back[0].start.is_int() && back[0].words[0].start.is_int());
}

#[test]
fn test_json_layout_matches_python() {
    let text = r#"[{"id": 1, "start": 0, "end": 1e-05, "text": "a", "words": [{"start": 1e16, "end": 0.1, "text": "x"}], "speaker": null, "needs_review": true}, {"id": 2, "start": 1.0, "end": 2.5, "text": "b"}]"#;
    let sentences = sentences_from_value(&parse(text).unwrap()).unwrap();
    let expected = "[\n  {\n    \"id\": 1,\n    \"start\": 0,\n    \"end\": 1e-05,\n    \"text\": \"a\",\n    \"words\": [\n      {\n        \"start\": 1e+16,\n        \"end\": 0.1,\n        \"text\": \"x\"\n      }\n    ],\n    \"speaker\": null,\n    \"needs_review\": true\n  },\n  {\n    \"id\": 2,\n    \"start\": 1.0,\n    \"end\": 2.5,\n    \"text\": \"b\",\n    \"words\": [],\n    \"speaker\": null\n  }\n]";
    assert_eq!(render(&sentences, "json").unwrap(), expected);
}

#[test]
fn test_empty_renders() {
    assert_eq!(render(&[], "srt").unwrap(), "");
    assert_eq!(render(&[], "vtt").unwrap(), "WEBVTT\n\n");
    assert_eq!(render(&[], "json").unwrap(), "[]");
}

#[test]
#[serial_test::serial]
fn test_unknown_format_is_localized() {
    se_core::i18n::set_language(Some("en"));
    let err = render(&sample(), "ass").unwrap_err();
    assert_eq!(err.message, "Unknown format: ass");
    assert!(err.is(se_core::ErrorKind::Value));
    se_core::i18n::set_language(Some("tr"));
    assert_eq!(render(&sample(), "ass").unwrap_err().message, "Bilinmeyen format: ass");
    se_core::i18n::set_language(None);
}

#[test]
fn test_nan_start_is_an_error() {
    let mut s = sample();
    s[0].start = Num::Float(f64::NAN);
    assert_eq!(render(&s, "srt").unwrap_err().message, "cannot convert float NaN to integer");
    // JSON passes it through like Python (`NaN`).
    assert!(render(&s, "json").unwrap().contains("\"start\": NaN"));
}

#[test]
fn test_write_creates_parents_and_uses_lf() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a").join("b").join("out.srt");
    let returned = write(&sample(), &path, "srt").unwrap();
    assert_eq!(returned, path);
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(bytes, render(&sample(), "srt").unwrap().as_bytes());
    assert!(!bytes.contains(&b'\r'));
    // Existing file is truncated.
    write(&sample()[..1], &path, "vtt").unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), render(&sample()[..1], "vtt").unwrap());
}

#[test]
fn test_write_leaves_an_empty_file_when_render_fails() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.srt");
    let mut s = sample();
    s[1].end = Num::Float(f64::INFINITY);
    assert_eq!(write(&s, &path, "srt").unwrap_err().message, "cannot convert float infinity to integer");
    assert_eq!(std::fs::read(&path).unwrap(), b"");
}

#[test]
fn test_write_into_a_file_parent_is_an_os_error() {
    let dir = tempfile::tempdir().unwrap();
    let blocker = dir.path().join("file");
    std::fs::write(&blocker, b"x").unwrap();
    let err = write(&sample(), &blocker.join("out.srt"), "srt").unwrap_err();
    assert!(err.is(se_core::ErrorKind::Os), "{err}");
    // Python: FileExistsError `[WinError 183] <system text>: '<dir>\\file'` (Path.mkdir on the file).
    #[cfg(windows)]
    assert!(err.message.starts_with("[WinError 183] ") && err.message.ends_with("\\\\file'"), "{err}");
}
