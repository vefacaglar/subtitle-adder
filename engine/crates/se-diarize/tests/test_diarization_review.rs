//! Port of `tests/test_diarization_review.py`: bounded acoustic post-checks without model downloads or
//! real inference. Python monkeypatches `cluster_centroids`; here `verify_short_turns_with` takes the
//! centroid provider, and `run_diarization` takes merge/verify hooks.

mod common;

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;

use common::*;
use se_core::{Result, Word};
use se_diarize::{
    DiarizationEngine, DiarizeHooks, EmbedFn, RawSegment, SegmentIndex, SliceCache, SpeakerSegment, assign_speakers,
    merge_similar_speakers, run_diarization, verify_short_turns, verify_short_turns_with,
};

fn dialogue(reply_speaker: i64) -> (Vec<Word>, Vec<SpeakerSegment>, Vec<f32>) {
    let mut words: Vec<Word> = ["We", "are", "all", "ready."]
        .iter()
        .enumerate()
        .map(|(i, t)| tw(t, i as f64 * 0.2, (i + 1) as f64 * 0.2))
        .collect();
    words.push(tw("Yes.", 1.0, 1.5));
    words.extend(
        ["Let", "us", "start", "now."]
            .iter()
            .enumerate()
            .map(|(i, t)| tw(t, 1.7 + i as f64 * 0.2, 1.9 + i as f64 * 0.2)),
    );
    let segments = vec![seg(0.0, 1.0, 0), seg(1.0, 1.6, reply_speaker), seg(1.6, 3.0, 0), seg(4.0, 7.0, 1)];
    (words, segments, vec![0.0; 8 * RATE])
}

fn fixed(centroids: &[(i64, Vec<f64>)]) -> BTreeMap<i64, Vec<f64>> {
    centroids.iter().cloned().collect()
}

#[test]
fn test_isolated_switch_is_repaired_only_when_voice_matches_neighbors() {
    for (voice, expected) in [(vec![1.0, 0.0], 0), (vec![0.0, 1.0], 1)] {
        let (words, segments, samples) = dialogue(1);
        let centroids = fixed(&[(0, vec![1.0, 0.0]), (1, vec![0.0, 1.0])]);
        let mut provider =
            |_: &[SpeakerSegment], _: &[f32], _: EmbedFn<'_>, _: &mut SliceCache| centroids.clone();
        let mut embed = |_: &[f32]| voice.clone();
        let result = verify_short_turns_with(&segments, &words, &samples, &mut embed, None, &mut provider);
        assert_eq!(SegmentIndex::new(&result).speaker_for(&words[4]), expected);
        assert_eq!(assign_speakers(&words, &result)[4], Some(expected));
    }
}

#[test]
fn test_uncertain_audio_never_relabels_a_reply() {
    let cases = [
        (fixed(&[(0, vec![1.0, 0.0, 0.0]), (1, vec![0.0, 1.0, 0.0])]), vec![0.1, 0.5, 0.86]),
        (
            fixed(&[(0, vec![1.0, 0.0, 0.0]), (1, vec![0.0, 1.0, 0.0]), (2, vec![0.0, 0.99, 0.1])]),
            vec![0.0, 1.0, 0.0],
        ),
        (fixed(&[(0, vec![1.0, 0.0, 0.0]), (1, vec![0.0, 1.0, 0.0])]), vec![0.0, 0.0, 0.0]),
    ];
    for (centroids, voice) in cases {
        let (words, segments, samples) = dialogue(0);
        let mut provider =
            |_: &[SpeakerSegment], _: &[f32], _: EmbedFn<'_>, _: &mut SliceCache| centroids.clone();
        let mut embed = |_: &[f32]| voice.clone();
        assert_eq!(verify_short_turns_with(&segments, &words, &samples, &mut embed, None, &mut provider), segments);
    }
}

/// The thresholds themselves (the Python cases above stop at candidate selection): an isolated switch
/// whose voice is uncertain keeps its label.
#[test]
fn test_uncertain_voice_on_a_real_candidate_keeps_the_label() {
    let cases = [
        (fixed(&[(0, vec![1.0, 0.0, 0.0]), (1, vec![0.0, 1.0, 0.0])]), vec![0.6, 0.1, 0.79]), // cos 0.6 < 0.65
        (
            fixed(&[(0, vec![1.0, 0.0, 0.0]), (1, vec![0.0, 1.0, 0.0]), (2, vec![0.99, 0.1, 0.0])]),
            vec![1.0, 0.0, 0.0],
        ), // lead < 0.10
        (fixed(&[(0, vec![1.0, 0.0, 0.0]), (1, vec![0.0, 1.0, 0.0])]), vec![0.0, 0.0, 0.0]), // zero vector
    ];
    for (centroids, voice) in cases {
        let (words, segments, samples) = dialogue(1);
        let mut provider =
            |_: &[SpeakerSegment], _: &[f32], _: EmbedFn<'_>, _: &mut SliceCache| centroids.clone();
        let mut embed = |_: &[f32]| voice.clone();
        assert_eq!(verify_short_turns_with(&segments, &words, &samples, &mut embed, None, &mut provider), segments);
    }
}

#[test]
fn test_overlapping_or_mixed_speaker_replies_are_not_flattened() {
    for mixed in [false, true] {
        let (mut words, mut segments, samples) = dialogue(0);
        if mixed {
            words.splice(4..5, [tw("Yes", 1.0, 1.25), tw("indeed.", 1.25, 1.5)]);
            segments.splice(1..2, [seg(1.0, 1.25, 0), seg(1.25, 1.6, 1)]);
        } else {
            segments.push(seg(1.1, 1.4, 1));
        }
        let mut provider = |_: &[SpeakerSegment], _: &[f32], _: EmbedFn<'_>, _: &mut SliceCache| -> BTreeMap<i64, Vec<f64>> {
            panic!("No candidate: acoustic inference should not run")
        };
        let mut embed = |_: &[f32]| -> Vec<f64> { panic!("No candidate: acoustic inference should not run") };
        assert_eq!(verify_short_turns_with(&segments, &words, &samples, &mut embed, None, &mut provider), segments);
    }
}

#[test]
fn test_no_candidates_skips_reference_embedding_work() {
    let (mut words, segments, samples) = dialogue(1);
    words[4] = tw("Yes.", 1.0, 1.1);
    let mut provider = |_: &[SpeakerSegment], _: &[f32], _: EmbedFn<'_>, _: &mut SliceCache| -> BTreeMap<i64, Vec<f64>> {
        panic!("No candidate: acoustic inference should not run")
    };
    let mut embed = |_: &[f32]| -> Vec<f64> { panic!("No candidate: acoustic inference should not run") };
    assert_eq!(verify_short_turns_with(&segments, &words, &samples, &mut embed, None, &mut provider), segments);
    // The public entry point does no work either.
    assert_eq!(verify_short_turns(&segments, &words, &samples, &mut embed, None), segments);
}

#[test]
fn test_review_budget_covers_beginning_and_end_of_long_dialogue() {
    let words: Vec<Word> = (0..100).map(|i| tw("Yes.", i as f64, i as f64 + 0.4)).collect();
    let segments = vec![seg(0.0, 100.0, 0), seg(100.0, 103.0, 1)];
    let samples: Vec<f32> = (0..104 * RATE).map(|i| i as f32).collect();
    let centroids = fixed(&[(0, vec![1.0, 0.0]), (1, vec![0.0, 1.0])]);
    let mut provider = |_: &[SpeakerSegment], _: &[f32], _: EmbedFn<'_>, _: &mut SliceCache| centroids.clone();
    let mut checked: Vec<f64> = Vec::new();
    let mut embed = |chunk: &[f32]| {
        checked.push(chunk[0] as f64 / RATE as f64);
        vec![1.0, 0.0]
    };
    assert_eq!(verify_short_turns_with(&segments, &words, &samples, &mut embed, None, &mut provider), segments);
    assert_eq!(checked.len(), se_diarize::VERIFY_MAX_TURNS);
    assert_eq!(checked[0], 0.0);
    assert!(*checked.last().unwrap() > 98.0);
}

#[test]
fn test_review_reuses_voice_references_from_merge_pass() {
    let samples = blocks(&[1.0, 2.0, 1.0], 2);
    let segments = vec![seg(0.0, 2.0, 0), seg(2.0, 4.0, 1), seg(4.0, 6.0, 0)];
    let mut words: Vec<Word> = ["We", "are", "all", "ready."]
        .iter()
        .enumerate()
        .map(|(i, t)| tw(t, i as f64 * 0.3, (i + 1) as f64 * 0.3))
        .collect();
    words.push(tw("Yes.", 2.1, 2.6));
    words.extend(
        ["Let", "us", "start", "now."]
            .iter()
            .enumerate()
            .map(|(i, t)| tw(t, 2.8 + i as f64 * 0.2, 3.0 + i as f64 * 0.2)),
    );
    let calls = Cell::new(0usize);
    let mut embed = |chunk: &[f32]| {
        calls.set(calls.get() + 1);
        if median(chunk) == 1.0 { vec![1.0, 0.0] } else { vec![0.0, 1.0] }
    };
    let mut cache = SliceCache::new();
    let merged = merge_similar_speakers(&segments, &mut embed, &samples, 0.65, Some(&mut cache));
    assert_eq!(calls.get(), 3);
    assert_eq!(verify_short_turns(&merged, &words, &samples, &mut embed, Some(&mut cache)), segments);
    assert_eq!(calls.get(), 4);
}

struct Engine {
    segments: Vec<RawSegment>,
}

impl DiarizationEngine for Engine {
    fn process(&mut self, _samples: &[f32], callback: &mut dyn FnMut(i32, i32)) -> Result<Vec<RawSegment>> {
        callback(1, 1);
        Ok(self.segments.clone())
    }
}

#[test]
fn test_diarizer_checks_after_clustering_and_completes_progress_after_review() {
    for num_speakers in [None, Some(2u32)] {
        let progress: RefCell<Vec<f64>> = RefCell::default();
        let caches: RefCell<Vec<usize>> = RefCell::default();
        let verified = Cell::new(false);
        let mut engine = Engine {
            segments: vec![RawSegment { start: 0.0, end: 4.0, speaker: 0 }, RawSegment { start: 4.0, end: 8.0, speaker: 1 }],
        };
        let samples = vec![0.0f32; 8 * RATE];
        let words = [tw("Hello.", 0.0, 1.0)];
        let mut merge = |raw: &[SpeakerSegment], _: EmbedFn<'_>, _: &[f32], cache: &mut SliceCache| {
            caches.borrow_mut().push(cache as *mut SliceCache as usize);
            raw.to_vec()
        };
        let mut verify = |raw: &[SpeakerSegment], _: &[Word], _: &[f32], _: EmbedFn<'_>, cache: &mut SliceCache| {
            if num_speakers.is_none() {
                assert_eq!(caches.borrow()[0], cache as *mut SliceCache as usize);
            } else {
                assert!(caches.borrow().is_empty(), "explicit speaker count still skips automatic merging");
            }
            assert_eq!(*progress.borrow(), vec![0.85, 0.9]);
            verified.set(true);
            raw.to_vec()
        };
        let mut on_progress = |f: f64| progress.borrow_mut().push(f);
        let mut embed = |_: &[f32]| -> Vec<f64> { panic!("hooks replace every embedding user") };
        let result = run_diarization(
            &mut engine,
            &samples,
            num_speakers,
            Some(&mut on_progress),
            Some(&words),
            &mut embed,
            Some(DiarizeHooks { merge: &mut merge, verify: &mut verify }),
        )
        .unwrap();
        assert_eq!(result, vec![seg(0.0, 4.0, 0), seg(4.0, 8.0, 1)]);
        assert!(verified.get());
        assert_eq!(*progress.borrow(), vec![0.85, 0.9, 1.0]);
    }
}

#[test]
fn test_progress_is_strictly_increasing_and_ends_at_one() {
    struct Chatty;
    impl DiarizationEngine for Chatty {
        fn process(&mut self, _: &[f32], callback: &mut dyn FnMut(i32, i32)) -> Result<Vec<RawSegment>> {
            for (done, total) in [(0, 0), (0, 4), (1, 4), (1, 4), (2, 4), (5, 4), (4, 4)] {
                callback(done, total);
            }
            Ok(Vec::new())
        }
    }
    let mut progress = Vec::new();
    let mut on_progress = |f: f64| progress.push(f);
    let mut embed = |_: &[f32]| -> Vec<f64> { panic!("no segments, no embeddings") };
    let result =
        run_diarization(&mut Chatty, &[0.0; 100], None, Some(&mut on_progress), None, &mut embed, None).unwrap();
    assert!(result.is_empty());
    assert_eq!(progress, vec![0.0, 0.2125, 0.425, 0.85, 0.9, 1.0]);
}
