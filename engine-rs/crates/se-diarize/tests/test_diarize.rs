//! Port of the diarization part of `tests/test_diarize.py` (word assignment, smoothing, snapping,
//! normalize, merge, compaction, review). The pipeline/CLI/render/model-cache tests of that file are
//! ported with the se-cli, se-text and se-models crates.

mod common;

use std::cell::RefCell;
use std::path::Path;

use common::*;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use se_core::{Result, Word};
use se_diarize::{
    DiarizationEngine, Diarizer, Embedder, RawSegment, SegmentIndex, SherpaDiarizer, SpeakerSegment, assign_speakers,
    compact_speech_samples, merge_similar_speakers, normalize, unmap_compacted_segments, verify_short_turns,
};

#[test]
fn test_word_gets_speaker_with_largest_overlap() {
    let words = [tw("a", 0.0, 1.0), tw("b", 1.0, 2.0), tw("c", 2.0, 3.0)];
    let segments = [seg(0.0, 1.2, 0), seg(1.2, 3.0, 1)];
    assert_eq!(assign_speakers(&words, &segments), some(&[0, 1, 1]));
}

#[test]
fn test_word_without_overlap_uses_nearest_segment() {
    let words = [tw("x", 5.0, 5.4), tw("y", 9.0, 9.4)];
    let segments = [seg(0.0, 4.0, 0), seg(10.0, 12.0, 1)];
    assert_eq!(assign_speakers(&words, &segments), some(&[0, 1]));
}

#[test]
fn test_no_segments_means_no_speakers() {
    assert_eq!(assign_speakers(&make_words("a b c"), &[]), vec![None, None, None]);
}

#[test]
fn test_single_word_island_joins_surrounding_speaker() {
    let words = make_words("a b c d e");
    let segments = [seg(0.0, 0.8, 0), seg(0.8, 1.2, 1), seg(1.2, 3.0, 0)];
    assert_eq!(assign_speakers(&words, &segments), some(&[0, 0, 0, 0, 0]));
}

#[test]
fn test_normalize_renumbers_by_first_appearance_and_drops_noise() {
    let segments = [seg(5.0, 9.0, 3), seg(0.0, 4.0, 7), seg(4.0, 4.3, 5)];
    let result = normalize(&segments, true, None);
    assert_eq!(result.iter().map(|s| (s.start, s.speaker)).collect::<Vec<_>>(), vec![(0.0, 0), (5.0, 1)]);
    let kept = normalize(&segments, false, None);
    assert_eq!(speakers(&kept), vec![0, 1, 2]);
}

// --- smoothing, boundary snapping ---------------------------------------------------------------

#[test]
fn test_smoothing_keeps_a_complete_one_word_reply() {
    let words = [
        tw("Hazır", 0.0, 0.4),
        tw("mısın?", 0.4, 0.8),
        tw("Evet.", 0.8, 1.2),
        tw("Başlayalım", 1.2, 1.7),
        tw("hemen.", 1.7, 2.1),
    ];
    let segments = [seg(0.0, 0.85, 0), seg(0.85, 1.2, 1), seg(1.2, 2.2, 0)];
    assert_eq!(assign_speakers(&words, &segments), some(&[0, 0, 1, 0, 0]));
}

#[test]
fn test_smoothing_still_joins_a_noise_island_mid_sentence() {
    let words = make_words("bunu şimdi yapmak istiyorum çünkü");
    let segments = [seg(0.0, 0.9, 0), seg(0.9, 1.2, 1), seg(1.2, 2.4, 0)];
    assert_eq!(assign_speakers(&words, &segments), some(&[0; 5]));
}

#[test]
fn test_alternating_single_word_turns_do_not_cascade() {
    let words: Vec<Word> = ["A.", "B.", "C.", "D.", "E.", "F."]
        .iter()
        .enumerate()
        .map(|(i, t)| tw(t, i as f64 * 0.5, i as f64 * 0.5 + 0.4))
        .collect();
    let segments: Vec<SpeakerSegment> =
        (0..6).map(|i| seg(i as f64 * 0.5, i as f64 * 0.5 + 0.45, (i % 2) as i64)).collect();
    assert_eq!(assign_speakers(&words, &segments), some(&[0, 1, 0, 1, 0, 1]));
    let mid_sentence = make_words("a b c d e f");
    let segs: Vec<SpeakerSegment> =
        mid_sentence.iter().enumerate().map(|(i, w)| seg(w.start_f(), w.end_f(), (i % 2) as i64)).collect();
    assert_eq!(assign_speakers(&mid_sentence, &segs), some(&[0, 1, 0, 1, 0, 1]));
}

fn dersin_words() -> Vec<Word> {
    vec![
        tw("Bu", 10.70, 10.95),
        tw("harika", 10.95, 11.4),
        tw("bir", 11.4, 11.6),
        tw("haber,", 11.6, 12.1),
        tw("ikinci", 12.1, 12.6),
        tw("bölüm", 12.6, 13.0),
        tw("ne", 13.0, 13.2),
        tw("zaman", 13.2, 13.6),
        tw("hazır", 13.6, 13.9),
        tw("olur", 13.9, 14.22),
        tw("dersin?", 14.22, 14.80),
        tw("Bence", 15.08, 15.4),
        tw("yaklaşık", 15.4, 15.9),
        tw("iki", 15.9, 16.1),
        tw("hafta.", 16.1, 16.5),
    ]
}

#[test]
fn test_forward_snap_returns_sentence_final_word_to_previous_speaker() {
    let segments = [seg(10.6, 14.24, 0), seg(14.24, 17.0, 1)];
    let speakers = assign_speakers(&dersin_words(), &segments);
    assert_eq!(speakers[..11], some(&[0; 11])[..]);
    assert_eq!(speakers[11..], some(&[1; 4])[..]);
    // The segmented first sentence ends with "dersin?" (speaker 0): with these labels the first run of
    // speaker 0 ends exactly at the question mark.
    let first_run_end = speakers.iter().position(|s| *s != Some(0)).unwrap();
    assert_eq!(dersin_words()[first_run_end - 1].text, "dersin?");
}

#[test]
fn test_backward_snap_moves_start_of_next_sentence_to_new_speaker() {
    let words = [
        tw("Tamam.", 0.0, 0.5),
        tw("Peki", 0.8, 1.1),
        tw("bunu", 1.1, 1.4),
        tw("yapalım", 1.4, 1.9),
        tw("mı?", 1.9, 2.2),
    ];
    let segments = [seg(0.0, 1.45, 0), seg(1.45, 2.3, 1)];
    assert_eq!(assign_speakers(&words, &segments), some(&[0, 1, 1, 1, 1]));
}

#[test]
fn test_change_right_after_sentence_end_is_never_moved() {
    let words = [tw("Hazır", 0.0, 0.4), tw("mısın?", 0.4, 0.8), tw("Evet.", 0.8, 1.2), tw("Tamam.", 1.2, 1.6)];
    let segments = [seg(0.0, 0.8, 0), seg(0.8, 1.7, 1)];
    assert_eq!(assign_speakers(&words, &segments), some(&[0, 0, 1, 1]));
}

#[test]
fn test_snap_ignores_long_fragments() {
    let words = [
        tw("bir", 0.0, 0.5),
        tw("iki", 0.5, 1.0),
        tw("üç", 1.0, 1.5),
        tw("dört", 1.5, 2.0),
        tw("beş", 2.0, 3.0),
        tw("altı.", 3.0, 4.0),
    ];
    let segments = [seg(0.0, 1.5, 0), seg(1.5, 4.1, 1)];
    assert_eq!(assign_speakers(&words, &segments), some(&[0, 0, 0, 1, 1, 1]));
}

fn naive(word: &Word, segments: &[SpeakerSegment]) -> i64 {
    let mut overlap: Vec<(i64, f64)> = Vec::new();
    for g in segments {
        let shared = word.end_f().min(g.end) - word.start_f().max(g.start);
        if shared > 0.0 {
            match overlap.iter_mut().find(|(s, _)| *s == g.speaker) {
                Some((_, t)) => *t += shared,
                None => overlap.push((g.speaker, shared)),
            }
        }
    }
    if !overlap.is_empty() {
        let mut best = overlap[0];
        for &(s, t) in &overlap[1..] {
            if t > best.1 || (t == best.1 && s < best.0) {
                best = (s, t);
            }
        }
        return best.0;
    }
    let middle = (word.start_f() + word.end_f()) / 2.0;
    let dist = |g: &SpeakerSegment| if middle < g.start { g.start - middle } else { (middle - g.end).max(0.0) };
    let mut best = &segments[0];
    for g in &segments[1..] {
        let (a, b) = ((dist(g), g.start), (dist(best), best.start));
        if a.0 < b.0 || (a.0 == b.0 && a.1 < b.1) {
            best = g;
        }
    }
    best.speaker
}

#[test]
fn test_assign_speakers_matches_naive_overlap_on_many_segments() {
    for seed in 1..=5u64 {
        let mut rng = ChaCha8Rng::seed_from_u64(seed);
        let mut t = 0.0f64;
        let mut segments = Vec::new();
        for _ in 0..300 {
            let length: f64 = rng.random_range(0.5..3.0);
            segments.push(seg(t, t + length, rng.random_range(0..3)));
            t += length + [0.0, 0.0, 0.7][rng.random_range(0..3)];
        }
        // A few overlapping segments too (the index must handle non-monotonic ends).
        for _ in 0..30 {
            let start: f64 = rng.random_range(0.0..t);
            segments.push(seg(start, start + rng.random_range(0.1..5.0), rng.random_range(0..3)));
        }
        let words: Vec<Word> =
            (0..(t / 0.1) as usize).map(|i| tw(&format!("w{i}"), i as f64 * 0.1, i as f64 * 0.1 + 0.08)).collect();
        let index = SegmentIndex::new(&segments);
        for w in &words {
            assert_eq!(index.speaker_for(w), naive(w, &segments), "seed {seed}, word {}", w.text);
        }
    }
}

// --- post-clustering merge ----------------------------------------------------------------------

#[test]
fn test_merge_similar_speakers_merges_close_centroids_only() {
    let samples = blocks(&[1.0, 2.0, 3.0, 1.0], 2);
    let segments = [seg(0.0, 2.0, 0), seg(2.0, 4.0, 1), seg(4.0, 6.0, 2), seg(6.0, 8.0, 0)];
    let mut embed = |chunk: &[f32]| match chunk[0] {
        1.0 => vec![1.0, 0.0, 0.0],
        2.0 => vec![0.98, 0.2, 0.0],
        3.0 => vec![0.0, 0.0, 1.0],
        v => panic!("unexpected chunk value {v}"),
    };
    let merged = merge_similar_speakers(&segments, &mut embed, &samples, 0.65, None);
    assert_eq!(speakers(&merged), vec![0, 0, 2, 0]);
}

struct FakeEngine {
    segments: Vec<RawSegment>,
}

impl DiarizationEngine for FakeEngine {
    fn process(&mut self, _samples: &[f32], _callback: &mut dyn FnMut(i32, i32)) -> Result<Vec<RawSegment>> {
        Ok(self.segments.clone())
    }
}

struct NoEmbedder;

impl Embedder for NoEmbedder {
    fn embed(&mut self, _samples: &[f32]) -> Result<Vec<f64>> {
        panic!("no embedding expected")
    }
}

fn one_sample(_: &Path) -> Result<Vec<f32>> {
    Ok(vec![0.0])
}

#[test]
fn test_merge_pass_is_skipped_for_explicit_speaker_count() {
    use std::rc::Rc;
    let raw = vec![RawSegment { start: 0.0, end: 2.0, speaker: 0 }, RawSegment { start: 2.0, end: 4.0, speaker: 1 }];

    // Merge is skipped with an explicit count and runs once in auto mode.
    let mut merge_calls = 0;
    for num_speakers in [Some(2), None] {
        let mut engine = FakeEngine { segments: raw.clone() };
        let mut merge = |segs: &[SpeakerSegment], _: se_diarize::EmbedFn<'_>, _: &[f32], _: &mut se_diarize::SliceCache| {
            merge_calls += 1;
            segs.to_vec()
        };
        let mut verify = |segs: &[SpeakerSegment],
                          _: &[Word],
                          _: &[f32],
                          _: se_diarize::EmbedFn<'_>,
                          _: &mut se_diarize::SliceCache| segs.to_vec();
        let hooks = se_diarize::DiarizeHooks { merge: &mut merge, verify: &mut verify };
        let mut embed = |_: &[f32]| -> Vec<f64> { panic!("no embedding expected") };
        se_diarize::run_diarization(&mut engine, &[0.0], num_speakers, None, None, &mut embed, Some(hooks)).unwrap();
        let _ = &mut merge_calls;
    }
    assert_eq!(merge_calls, 1);

    // An explicit count builds a fresh engine every call; auto mode builds the default once.
    let builds: Rc<RefCell<Vec<Option<u32>>>> = Rc::default();
    let log = Rc::clone(&builds);
    let builder: se_diarize::EngineBuilder = Box::new(move |n| {
        log.borrow_mut().push(n);
        Ok(Box::new(FakeEngine {
            segments: vec![
                RawSegment { start: 0.0, end: 2.0, speaker: 0 },
                RawSegment { start: 2.0, end: 4.0, speaker: 1 },
            ],
        }) as Box<dyn DiarizationEngine>)
    });
    let mut d = SherpaDiarizer::with_parts(builder, Box::new(NoEmbedder), one_sample);
    let result = d.diarize(Path::new("x.wav"), Some(2), None, None).unwrap();
    assert_eq!(result, vec![seg(0.0, 2.0, 0), seg(2.0, 4.0, 1)]);
    d.diarize(Path::new("x.wav"), None, None, None).unwrap();
    d.diarize(Path::new("x.wav"), None, None, None).unwrap();
    d.diarize(Path::new("x.wav"), Some(2), None, None).unwrap();
    assert_eq!(*builds.borrow(), vec![Some(2), None, Some(2)]);
}

// --- speech compaction, embedding cache & short-turn verification --------------------------------

#[test]
fn test_compact_speech_samples_skips_silence_and_unmaps_timestamps() {
    let samples = vec![0.1f32; 30 * RATE];
    let words = [tw("Merhaba", 2.0, 3.0), tw("dünya.", 3.0, 4.0), tw("İkinci", 20.0, 21.0), tw("kısım.", 21.0, 22.0)];
    let (compacted, spans) = compact_speech_samples(&samples, Some(&words));
    let spans = spans.expect("compacted");
    assert_eq!(spans.len(), 2);
    assert!((compacted.len() as f64) < samples.len() as f64 * 0.25);

    let comp_segs = [seg(spans[0].comp_start, spans[0].comp_end, 0), seg(spans[1].comp_start, spans[1].comp_end, 1)];
    let unmapped = unmap_compacted_segments(&comp_segs, &spans);
    assert_eq!(unmapped, vec![seg(1.75, 4.25, 0), seg(19.75, 22.25, 1)]);
}

#[test]
fn test_compaction_skips_short_or_mostly_speech_audio() {
    let samples = vec![0.1f32; 9 * RATE];
    let words = [tw("a.", 1.0, 2.0)];
    assert!(compact_speech_samples(&samples, Some(&words)).1.is_none()); // < 10 s
    let samples = vec![0.1f32; 12 * RATE];
    let words: Vec<Word> = (0..12).map(|i| tw("x", i as f64, i as f64 + 0.9)).collect();
    assert!(compact_speech_samples(&samples, Some(&words)).1.is_none()); // speech >= 85 %
}

#[test]
fn test_compaction_energy_fallback_without_words() {
    // 20 s: silence with a loud 3 s burst at [5, 8) → one span around it.
    let mut samples = vec![0.0f32; 20 * RATE];
    for (i, s) in samples[5 * RATE..8 * RATE].iter_mut().enumerate() {
        *s = if i % 2 == 0 { 0.3 } else { -0.3 };
    }
    let (compacted, spans) = compact_speech_samples(&samples, None);
    let spans = spans.expect("compacted");
    assert_eq!(spans.len(), 1);
    assert_eq!((spans[0].orig_start, spans[0].orig_end), (4.75, 8.25));
    assert_eq!(compacted.len(), (3.5 * RATE as f64) as usize);
    // Empty words behave like no words.
    assert_eq!(compact_speech_samples(&samples, Some(&[])).1, Some(spans));
}

#[test]
fn test_merge_similar_speakers_caches_segment_embeddings() {
    let samples = blocks(&[1.0, 2.0, 3.0, 4.0], 2);
    let segments = [seg(0.0, 2.0, 0), seg(2.0, 4.0, 1), seg(4.0, 6.0, 2), seg(6.0, 8.0, 3)];
    let mut calls = Vec::new();
    let mut embed = |chunk: &[f32]| {
        calls.push(chunk[0]);
        match chunk[0] {
            1.0 => vec![1.0, 0.0, 0.0],
            2.0 => vec![0.99, 0.1, 0.0],
            3.0 => vec![0.98, 0.15, 0.0],
            4.0 => vec![0.0, 0.0, 1.0],
            v => panic!("unexpected chunk value {v}"),
        }
    };
    let merged = merge_similar_speakers(&segments, &mut embed, &samples, 0.65, None);
    assert_eq!(speakers(&merged), vec![0, 0, 0, 3]);
    assert_eq!(calls.len(), 4);
}

#[test]
fn test_verify_short_turns_recovers_swallowed_reply_after_question() {
    let mut samples = vec![1.0f32; 8 * RATE];
    for s in &mut samples[(1.1 * 16000.0) as usize..(1.8 * 16000.0) as usize] {
        *s = 2.0;
    }
    for s in &mut samples[(4.0 * 16000.0) as usize..(7.5 * 16000.0) as usize] {
        *s = 2.0;
    }
    let words = [
        tw("Hazır", 0.0, 0.5),
        tw("mısın?", 0.5, 1.0),
        tw("Evet.", 1.2, 1.7),
        tw("Tamam", 2.0, 2.8),
        tw("başlıyoruz.", 2.8, 3.8),
        tw("Harika", 4.2, 5.5),
        tw("oldu.", 5.5, 7.0),
    ];
    let raw_segments = [seg(0.0, 3.9, 0), seg(4.0, 7.2, 1)];
    let mut embed = |chunk: &[f32]| {
        let m = se_core::pyfmt::round_ndigits(median(chunk) as f64, 1);
        if m == 1.0 {
            vec![1.0, 0.0]
        } else if m == 2.0 {
            vec![0.0, 1.0]
        } else {
            panic!("KeyError: {m}")
        }
    };
    let refined = verify_short_turns(&raw_segments, &words, &samples, &mut embed, None);
    assert_eq!(assign_speakers(&words, &refined), some(&[0, 0, 1, 0, 0, 1, 1]));
}

struct HalfwayEngine;

impl DiarizationEngine for HalfwayEngine {
    fn process(&mut self, _samples: &[f32], callback: &mut dyn FnMut(i32, i32)) -> Result<Vec<RawSegment>> {
        callback(1, 2);
        Ok(vec![RawSegment { start: 0.0, end: 2.0, speaker: 0 }, RawSegment { start: 2.0, end: 4.0, speaker: 1 }])
    }
}

struct FailingEmbedder;

impl Embedder for FailingEmbedder {
    fn embed(&mut self, _samples: &[f32]) -> Result<Vec<f64>> {
        Err(se_core::EngineError::model("extractor failed"))
    }
}

fn four_seconds(_: &Path) -> Result<Vec<f32>> {
    Ok(vec![0.1; 4 * RATE])
}

#[test]
fn test_extractor_error_stops_progress_like_the_python_exception() {
    // Python: the extractor error is raised inside the merge pass, so 0.9 / 1.0 are never reported.
    let builder: se_diarize::EngineBuilder =
        Box::new(|_| Ok(Box::new(HalfwayEngine) as Box<dyn DiarizationEngine>));
    let mut d = SherpaDiarizer::with_parts(builder, Box::new(FailingEmbedder), four_seconds);
    let mut progress = Vec::new();
    let mut on_progress = |f: f64| progress.push(f);
    let err = d.diarize(Path::new("x.wav"), None, Some(&mut on_progress), None).unwrap_err();
    assert_eq!(err.message, "extractor failed");
    assert_eq!(progress, vec![0.425]);

    // An explicit count without words never embeds: the run completes.
    let mut progress = Vec::new();
    let mut on_progress = |f: f64| progress.push(f);
    d.diarize(Path::new("x.wav"), Some(2), Some(&mut on_progress), None).unwrap();
    assert_eq!(progress, vec![0.425, 0.9, 1.0]);
}
