//! Byte-for-byte parity with the Python engine on a random corpus (`tests/fixtures/gen_golden.py`):
//! `segment` on random word streams (closers, clitics, odd whitespace, Unicode case edge cases, int times,
//! random speakers and configs), then `render` to SRT / VTT / JSON.

use se_core::json::{Value, dumps, parse};
use se_core::types::sentences_to_value;
use se_core::{Num, Word};
use se_text::sentences::{SegmenterConfig, segment};
use se_text::subtitles::render;

#[test]
fn test_segment_and_render_match_python_corpus() {
    let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/golden_segment.json"))
        .expect("golden corpus");
    let cases = parse(&text).expect("valid corpus");
    let cases = cases.as_array().expect("array");
    assert!(cases.len() >= 200);
    for (n, case) in cases.iter().enumerate() {
        let words: Vec<Word> = case
            .get("words")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .map(|w| {
                let w = w.as_array().unwrap();
                Word {
                    start: Num::from_value(&w[0]).unwrap(),
                    end: Num::from_value(&w[1]).unwrap(),
                    text: w[2].as_str().unwrap().to_string(),
                }
            })
            .collect();
        let speakers: Option<Vec<Option<i64>>> = match case.get("speakers").unwrap() {
            Value::Null => None,
            v => Some(v.as_array().unwrap().iter().map(Value::as_i64).collect()),
        };
        let c = case.get("config").and_then(Value::as_array).unwrap();
        let config = SegmenterConfig {
            pause_gap: c[0].as_f64().unwrap(),
            max_chars: c[1].as_i64().unwrap(),
            max_duration: c[2].as_f64().unwrap(),
        };
        let out = segment(&words, &config, speakers.as_deref()).unwrap();
        assert_eq!(
            dumps(&sentences_to_value(&out), false),
            case.get("sentences").and_then(Value::as_str).unwrap(),
            "case {n}: sentences"
        );
        assert_eq!(render(&out, "srt").unwrap(), case.get("srt").and_then(Value::as_str).unwrap(), "case {n}: srt");
        if let Some(vtt) = case.get("vtt").and_then(Value::as_str) {
            assert_eq!(render(&out, "vtt").unwrap(), vtt, "case {n}: vtt");
        }
        if let Some(json) = case.get("json").and_then(Value::as_str) {
            assert_eq!(render(&out, "json").unwrap(), json, "case {n}: json");
        }
    }
}

/// Second, boundary-oriented corpus (`tests/fixtures/gen_fuzz.py`): threshold gaps, overlapping times, long
/// unpunctuated runs for `fit`, realignment starters, tiny/negative limits and non-finite times (SRT errors).
#[test]
fn test_segment_matches_python_boundary_corpus() {
    let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/golden_fuzz.json"))
        .expect("fuzz corpus");
    let cases = parse(&text).expect("valid corpus");
    let cases = cases.as_array().expect("array");
    assert!(cases.len() >= 200);
    for (n, case) in cases.iter().enumerate() {
        let words: Vec<Word> = case
            .get("words")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .map(|w| {
                let w = w.as_array().unwrap();
                Word {
                    start: Num::from_value(&w[0]).unwrap(),
                    end: Num::from_value(&w[1]).unwrap(),
                    text: w[2].as_str().unwrap().to_string(),
                }
            })
            .collect();
        let speakers: Option<Vec<Option<i64>>> = match case.get("speakers").unwrap() {
            Value::Null => None,
            v => Some(v.as_array().unwrap().iter().map(Value::as_i64).collect()),
        };
        let c = case.get("config").and_then(Value::as_array).unwrap();
        let config = SegmenterConfig {
            pause_gap: c[0].as_f64().unwrap(),
            max_chars: c[1].as_i64().unwrap(),
            max_duration: c[2].as_f64().unwrap(),
        };
        let out = segment(&words, &config, speakers.as_deref()).unwrap();
        assert_eq!(
            dumps(&sentences_to_value(&out), false),
            case.get("sentences").and_then(Value::as_str).unwrap(),
            "case {n}: sentences"
        );
        let srt = match render(&out, "srt") {
            Ok(s) => s,
            Err(e) => format!("ERROR: {}", e.message),
        };
        assert_eq!(srt, case.get("srt").and_then(Value::as_str).unwrap(), "case {n}: srt");
    }
}
