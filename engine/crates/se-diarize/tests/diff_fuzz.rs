//! Differential check against the Python reference on randomized inputs.
//!
//! The reference JSON comes from the scratch generator `gen.py` (run with the engine venv's Python); set
//! `SE_DIARIZE_FUZZ=<path to ref.json>` to run. Without the variable the test returns early. Floats are
//! exchanged as IEEE-754 bit patterns, so the comparison is exact.

use std::collections::HashMap;

use se_core::Word;
use se_diarize::{
    SpeakerSegment, assign_speakers, cluster_centroids, compact_speech_samples, merge_similar_speakers, normalize,
    unmap_compacted_segments, verify_short_turns,
};
use serde_json::Value;

fn f(v: &Value) -> f64 {
    f64::from_bits(v.as_i64().expect("bits") as u64)
}

fn segs(v: &Value) -> Vec<SpeakerSegment> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|s| SpeakerSegment { start: f(&s[0]), end: f(&s[1]), speaker: s[2].as_i64().unwrap() })
        .collect()
}

fn embed(x: &[f32]) -> Vec<f64> {
    let n = x.len();
    if n == 0 {
        return Vec::new();
    }
    vec![x[0] as f64, x[n / 3] as f64, x[2 * n / 3] as f64, x[n - 1] as f64, 0.25, n as f64 / 16000.0 * 0.01]
}

fn check_segs(case: usize, what: &str, got: &[SpeakerSegment], want: &[SpeakerSegment]) {
    assert_eq!(got, want, "case {case}: {what}");
    for (g, w) in got.iter().zip(want) {
        assert_eq!(g.start.to_bits(), w.start.to_bits(), "case {case}: {what} bits");
        assert_eq!(g.end.to_bits(), w.end.to_bits(), "case {case}: {what} bits");
    }
}

#[test]
fn test_postprocess_matches_python_reference() {
    let Ok(path) = std::env::var("SE_DIARIZE_FUZZ") else { return };
    let cases: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert!(!cases.is_empty());
    for (ci, case) in cases.iter().enumerate() {
        let samples: Vec<f32> =
            case["samples"].as_array().unwrap().iter().map(|v| v.as_i64().unwrap() as i16 as f32 / 32768.0).collect();
        let segments = segs(&case["segments"]);
        let words: Vec<Word> = case["words"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| Word::new(f(&w[0]), f(&w[1]), w[2].as_str().unwrap()))
            .collect();

        let want: Vec<Option<i64>> = case["assign"].as_array().unwrap().iter().map(|v| v.as_i64()).collect();
        assert_eq!(assign_speakers(&words, &segments), want, "case {ci}: assign");

        check_segs(ci, "normalize drop", &normalize(&segments, true, Some(&words)), &segs(&case["normalize_True"]));
        check_segs(ci, "normalize keep", &normalize(&segments, false, Some(&words)), &segs(&case["normalize_False"]));

        let mut calls: Vec<usize> = Vec::new();
        let mut emb = |x: &[f32]| {
            calls.push(x.len());
            embed(x)
        };
        let merged = merge_similar_speakers(&segments, &mut emb, &samples, 0.65, None);
        check_segs(ci, "merge", &merged, &segs(&case["merge"]));
        let want_calls: Vec<usize> =
            case["merge_calls"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap() as usize).collect();
        assert_eq!(calls, want_calls, "case {ci}: merge embed calls");

        let mut emb = embed;
        let centroids = cluster_centroids(&segments, &samples, &mut emb, None);
        let want_c: HashMap<i64, Vec<u64>> = case["centroids"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.parse().unwrap(), v.as_array().unwrap().iter().map(|x| x.as_i64().unwrap() as u64).collect()))
            .collect();
        let got_c: HashMap<i64, Vec<u64>> =
            centroids.iter().map(|(k, v)| (*k, v.iter().map(|x| x.to_bits()).collect())).collect();
        assert_eq!(got_c, want_c, "case {ci}: centroids");

        let norm = normalize(&merged, true, Some(&words));
        let mut emb = embed;
        check_segs(ci, "verify", &verify_short_turns(&norm, &words, &samples, &mut emb, None), &segs(&case["verify"]));
        let mut emb = embed;
        check_segs(
            ci,
            "verify raw",
            &verify_short_turns(&segments, &words, &samples, &mut emb, None),
            &segs(&case["verify_raw"]),
        );

        for (key, use_words) in [("compact_w", true), ("compact_n", false)] {
            let (comp, spans) = compact_speech_samples(&samples, if use_words { Some(&words) } else { None });
            let want = &case[key];
            if want.is_null() {
                assert!(spans.is_none(), "case {ci}: {key} should be unchanged");
                continue;
            }
            let spans = spans.unwrap_or_else(|| panic!("case {ci}: {key} should compact"));
            assert_eq!(comp.len() as u64, want["len"].as_u64().unwrap(), "case {ci}: {key} len");
            let hash: u64 = comp.iter().map(|x| x.to_bits() as u64).sum();
            assert_eq!(hash, want["hash"].as_u64().unwrap(), "case {ci}: {key} hash");
            let want_spans: Vec<[u64; 4]> = want["spans"]
                .as_array()
                .unwrap()
                .iter()
                .map(|s| std::array::from_fn(|i| s[i].as_i64().unwrap() as u64))
                .collect();
            let got_spans: Vec<[u64; 4]> = spans
                .iter()
                .map(|s| {
                    [s.orig_start.to_bits(), s.orig_end.to_bits(), s.comp_start.to_bits(), s.comp_end.to_bits()]
                })
                .collect();
            assert_eq!(got_spans, want_spans, "case {ci}: {key} spans");
            let raw = segs(&want["raw"]);
            check_segs(ci, key, &unmap_compacted_segments(&raw, &spans), &segs(&want["unmap"]));
        }
    }
    eprintln!("{} cases matched", cases.len());
}
