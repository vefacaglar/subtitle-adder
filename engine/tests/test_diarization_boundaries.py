"""Regression cases for real short replies and overlapping speaker clusters."""
import numpy as np
import pytest

from subtitle_engine import diarize
from subtitle_engine.diarize import SpeakerSegment as Seg
from subtitle_engine.models import Word


def test_short_complete_reply_survives_noise_filter():
    segments = [Seg(0, 2, 7), Seg(2, 2.5, 9), Seg(2.5, 5, 7)]
    words = [Word(0, 1, "Ready?"), Word(2, 2.5, "Yes."), Word(3, 4, "Good.")]
    normalized = diarize.normalize(segments, words=words)
    assert [s.speaker for s in normalized] == [0, 1, 0]
    assert diarize.assign_speakers(words, normalized) == [0, 1, 0]


def test_two_word_reply_survives_noise_filter():
    segments = [Seg(0, 2, 0), Seg(2, 2.6, 1), Seg(2.6, 5, 0)]
    words = [Word(0, 1, "Ready?"), Word(2, 2.3, "Of"), Word(2.3, 2.6, "course."), Word(3, 4, "Good.")]
    assert [s.speaker for s in diarize.normalize(segments, words=words)] == [0, 1, 0]


def test_short_noise_without_matching_words_is_still_dropped():
    segments = [Seg(0, 2, 0), Seg(2, 2.3, 1), Seg(2.3, 5, 0)]
    words = [Word(0, 1, "Ready?"), Word(3, 4, "Yes.")]
    assert [s.speaker for s in diarize.normalize(segments, words=words)] == [0, 0]


def test_brief_fragment_cannot_claim_a_whole_asr_reply():
    segments = [Seg(0, 2, 0), Seg(2, 2.1, 1), Seg(2.6, 5, 0)]
    words = [Word(0, 1, "Ready?"), Word(2, 2.5, "Yes."), Word(3, 4, "Good.")]
    assert [s.speaker for s in diarize.normalize(segments, words=words)] == [0, 0]


def test_voice_references_exclude_other_speakers_audio():
    rate = diarize.SAMPLE_RATE
    samples = np.concatenate([np.full(rate, value, dtype=np.float32) for value in (1, 9, 2)])
    segments = [Seg(0, 2, 0), Seg(1, 3, 1)]
    used = []
    def embed(chunk):
        assert not np.any(chunk == 9), "Overlapping speech contaminated a voice reference"
        used.append(float(chunk[0]))
        return [1, 0] if chunk[0] == 1 else [0, 1]
    assert diarize.cluster_centroids(segments, samples, embed) == {0: [1, 0], 1: [0, 1]}
    assert used == [1, 2]


def test_simultaneous_speakers_are_not_merged_even_when_embeddings_match():
    segments = [Seg(0, 2, 0), Seg(1, 3, 1)]
    samples = np.ones(3 * diarize.SAMPLE_RATE, dtype=np.float32)
    assert diarize.merge_similar_speakers(segments, lambda _: [1, 0], samples) == segments


def test_overlap_constraint_survives_intermediate_cluster_merge():
    segments = [Seg(0, 2, 0), Seg(3, 5, 1), Seg(1, 3, 2)]
    samples = np.ones(5 * diarize.SAMPLE_RATE, dtype=np.float32)
    result = diarize.merge_similar_speakers(segments, lambda _: [1, 0], samples)
    assert result[0].speaker != result[2].speaker
    assert len({s.speaker for s in result}) == 2


def test_fully_overlapping_voices_have_no_clean_reference():
    segments = [Seg(0, 2, 0), Seg(0, 2, 1)]
    samples = np.ones(2 * diarize.SAMPLE_RATE, dtype=np.float32)
    def unexpected(_):
        pytest.fail("No isolated speech should mean no embedding inference")
    assert diarize.cluster_centroids(segments, samples, unexpected) == {}
    assert diarize.merge_similar_speakers(segments, unexpected, samples) == segments


@pytest.mark.parametrize("gap,duration", [(1.0, 0.3), (0.0, 1.2)])
def test_unpunctuated_single_word_turn_is_not_smoothed_across_pause_or_long_word(gap, duration):
    start = 0.6 + gap
    end = start + duration
    words = [Word(0, 0.3, "a"), Word(0.3, 0.6, "b"), Word(start, end, "maybe"),
             Word(end + gap, end + gap + 0.3, "c"), Word(end + gap + 0.3, end + gap + 0.6, "d")]
    segments = [Seg(0, 0.6, 0), Seg(start, end, 1), Seg(end + gap, end + gap + 0.6, 0)]
    assert diarize.assign_speakers(words, segments) == [0, 0, 1, 0, 0]


def test_punctuation_repair_does_not_move_reply_across_pause():
    words = [Word(0, 0.4, "are"), Word(0.4, 0.8, "you"), Word(1.5, 1.9, "Yes.")]
    assert diarize.assign_speakers(words, [Seg(0, 0.8, 0), Seg(1.5, 1.9, 1)]) == [0, 0, 1]


def test_sentence_tail_repair_cannot_jump_over_an_internal_pause():
    words = [Word(0, 0.2, "a"), Word(0.2, 0.3, "b"), Word(0.8, 0.9, "c.")]
    assert diarize.assign_speakers(words, [Seg(0, 0.2, 0), Seg(0.2, 0.9, 1)]) == [0, 1, 1]
