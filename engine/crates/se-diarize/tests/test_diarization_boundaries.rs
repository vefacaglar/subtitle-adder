//! Port of `tests/test_diarization_boundaries.py`: real short replies and overlapping speaker clusters.

mod common;

use common::*;
use se_diarize::{assign_speakers, cluster_centroids, merge_similar_speakers, normalize};

#[test]
fn test_short_complete_reply_survives_noise_filter() {
    let segments = [seg(0.0, 2.0, 7), seg(2.0, 2.5, 9), seg(2.5, 5.0, 7)];
    let words = [tw("Ready?", 0.0, 1.0), tw("Yes.", 2.0, 2.5), tw("Good.", 3.0, 4.0)];
    let normalized = normalize(&segments, true, Some(&words));
    assert_eq!(speakers(&normalized), vec![0, 1, 0]);
    assert_eq!(assign_speakers(&words, &normalized), some(&[0, 1, 0]));
}

#[test]
fn test_two_word_reply_survives_noise_filter() {
    let segments = [seg(0.0, 2.0, 0), seg(2.0, 2.6, 1), seg(2.6, 5.0, 0)];
    let words = [tw("Ready?", 0.0, 1.0), tw("Of", 2.0, 2.3), tw("course.", 2.3, 2.6), tw("Good.", 3.0, 4.0)];
    assert_eq!(speakers(&normalize(&segments, true, Some(&words))), vec![0, 1, 0]);
}

#[test]
fn test_short_noise_without_matching_words_is_still_dropped() {
    let segments = [seg(0.0, 2.0, 0), seg(2.0, 2.3, 1), seg(2.3, 5.0, 0)];
    let words = [tw("Ready?", 0.0, 1.0), tw("Yes.", 3.0, 4.0)];
    assert_eq!(speakers(&normalize(&segments, true, Some(&words))), vec![0, 0]);
}

#[test]
fn test_brief_fragment_cannot_claim_a_whole_asr_reply() {
    let segments = [seg(0.0, 2.0, 0), seg(2.0, 2.1, 1), seg(2.6, 5.0, 0)];
    let words = [tw("Ready?", 0.0, 1.0), tw("Yes.", 2.0, 2.5), tw("Good.", 3.0, 4.0)];
    assert_eq!(speakers(&normalize(&segments, true, Some(&words))), vec![0, 0]);
}

#[test]
fn test_voice_references_exclude_other_speakers_audio() {
    let samples = blocks(&[1.0, 9.0, 2.0], 1);
    let segments = [seg(0.0, 2.0, 0), seg(1.0, 3.0, 1)];
    let mut used = Vec::new();
    let mut embed = |chunk: &[f32]| {
        assert!(!chunk.contains(&9.0), "Overlapping speech contaminated a voice reference");
        used.push(chunk[0]);
        if chunk[0] == 1.0 { vec![1.0, 0.0] } else { vec![0.0, 1.0] }
    };
    let centroids = cluster_centroids(&segments, &samples, &mut embed, None);
    assert_eq!(centroids.into_iter().collect::<Vec<_>>(), vec![(0, vec![1.0, 0.0]), (1, vec![0.0, 1.0])]);
    assert_eq!(used, vec![1.0, 2.0]);
}

#[test]
fn test_simultaneous_speakers_are_not_merged_even_when_embeddings_match() {
    let segments = [seg(0.0, 2.0, 0), seg(1.0, 3.0, 1)];
    let samples = vec![1.0f32; 3 * RATE];
    let mut embed = |_: &[f32]| vec![1.0, 0.0];
    assert_eq!(merge_similar_speakers(&segments, &mut embed, &samples, 0.65, None), segments.to_vec());
}

#[test]
fn test_overlap_constraint_survives_intermediate_cluster_merge() {
    let segments = [seg(0.0, 2.0, 0), seg(3.0, 5.0, 1), seg(1.0, 3.0, 2)];
    let samples = vec![1.0f32; 5 * RATE];
    let mut embed = |_: &[f32]| vec![1.0, 0.0];
    let result = merge_similar_speakers(&segments, &mut embed, &samples, 0.65, None);
    assert_ne!(result[0].speaker, result[2].speaker);
    let distinct: std::collections::HashSet<i64> = result.iter().map(|s| s.speaker).collect();
    assert_eq!(distinct.len(), 2);
}

#[test]
fn test_fully_overlapping_voices_have_no_clean_reference() {
    let segments = [seg(0.0, 2.0, 0), seg(0.0, 2.0, 1)];
    let samples = vec![1.0f32; 2 * RATE];
    let mut unexpected = |_: &[f32]| -> Vec<f64> { panic!("No isolated speech should mean no embedding inference") };
    assert!(cluster_centroids(&segments, &samples, &mut unexpected, None).is_empty());
    assert_eq!(merge_similar_speakers(&segments, &mut unexpected, &samples, 0.65, None), segments.to_vec());
}

#[test]
fn test_unpunctuated_single_word_turn_is_not_smoothed_across_pause_or_long_word() {
    for (gap, duration) in [(1.0, 0.3), (0.0, 1.2)] {
        let start = 0.6 + gap;
        let end = start + duration;
        let words = [
            tw("a", 0.0, 0.3),
            tw("b", 0.3, 0.6),
            tw("maybe", start, end),
            tw("c", end + gap, end + gap + 0.3),
            tw("d", end + gap + 0.3, end + gap + 0.6),
        ];
        let segments = [seg(0.0, 0.6, 0), seg(start, end, 1), seg(end + gap, end + gap + 0.6, 0)];
        assert_eq!(assign_speakers(&words, &segments), some(&[0, 0, 1, 0, 0]), "gap {gap}, duration {duration}");
    }
}

#[test]
fn test_punctuation_repair_does_not_move_reply_across_pause() {
    let words = [tw("are", 0.0, 0.4), tw("you", 0.4, 0.8), tw("Yes.", 1.5, 1.9)];
    assert_eq!(assign_speakers(&words, &[seg(0.0, 0.8, 0), seg(1.5, 1.9, 1)]), some(&[0, 0, 1]));
}

#[test]
fn test_sentence_tail_repair_cannot_jump_over_an_internal_pause() {
    let words = [tw("a", 0.0, 0.2), tw("b", 0.2, 0.3), tw("c.", 0.8, 0.9)];
    assert_eq!(assign_speakers(&words, &[seg(0.0, 0.2, 0), seg(0.2, 0.9, 1)]), some(&[0, 1, 1]));
}
