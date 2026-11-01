"""Bounded acoustic post-checks without model downloads or real inference."""
import numpy as np
import pytest

from subtitle_engine import diarize
from subtitle_engine.diarize import SpeakerSegment as Seg
from subtitle_engine.models import Word


def dialogue(reply_speaker=1):
    words = [Word(i * 0.2, (i + 1) * 0.2, text)
             for i, text in enumerate(["We", "are", "all", "ready."])]
    words += [Word(1.0, 1.5, "Yes.")]
    words += [Word(1.7 + i * 0.2, 1.9 + i * 0.2, text)
              for i, text in enumerate(["Let", "us", "start", "now."])]
    segments = [Seg(0, 1, 0), Seg(1, 1.6, reply_speaker), Seg(1.6, 3, 0), Seg(4, 7, 1)]
    return words, segments, np.zeros(8 * diarize.SAMPLE_RATE, dtype=np.float32)


@pytest.mark.parametrize("voice,expected", [([1.0, 0.0], 0), ([0.0, 1.0], 1)])
def test_isolated_switch_is_repaired_only_when_voice_matches_neighbors(monkeypatch, voice, expected):
    words, segments, samples = dialogue()
    monkeypatch.setattr(diarize, "cluster_centroids", lambda *a, **k: {0: [1, 0], 1: [0, 1]})
    result = diarize.verify_short_turns(segments, words, samples, lambda _: voice)
    assert diarize._SegmentIndex(result).speaker_for(words[4]) == expected
    # The real short reply survives all subsequent word smoothing too.
    assert diarize.assign_speakers(words, result)[4] == expected


@pytest.mark.parametrize("centroids,voice", [
    ({0: [1, 0, 0], 1: [0, 1, 0]}, [0.1, 0.5, 0.86]),  # weak match to every known speaker
    ({0: [1, 0, 0], 1: [0, 1, 0], 2: [0, 0.99, 0.1]}, [0, 1, 0]),  # two plausible alternatives
    ({0: [1, 0, 0], 1: [0, 1, 0]}, [0, 0, 0]),  # unusable embedding
])
def test_uncertain_audio_never_relabels_a_reply(monkeypatch, centroids, voice):
    words, segments, samples = dialogue(reply_speaker=0)
    monkeypatch.setattr(diarize, "cluster_centroids", lambda *a, **k: centroids)
    assert diarize.verify_short_turns(segments, words, samples, lambda _: voice) == segments


@pytest.mark.parametrize("mixed", [False, True])
def test_overlapping_or_mixed_speaker_replies_are_not_flattened(monkeypatch, mixed):
    words, segments, samples = dialogue(reply_speaker=0)
    if mixed:
        words[4:5] = [Word(1, 1.25, "Yes"), Word(1.25, 1.5, "indeed.")]
        segments[1:2] = [Seg(1, 1.25, 0), Seg(1.25, 1.6, 1)]
    else:
        segments.append(Seg(1.1, 1.4, 1))
    def unexpected(*args, **kwargs):
        pytest.fail("No candidate: acoustic inference should not run")
    monkeypatch.setattr(diarize, "cluster_centroids", unexpected)
    assert diarize.verify_short_turns(segments, words, samples, unexpected) == segments


def test_no_candidates_skips_reference_embedding_work(monkeypatch):
    words, segments, samples = dialogue()
    words[4] = Word(1, 1.1, "Yes.")  # too short for a reliable check
    def unexpected(*args, **kwargs):
        pytest.fail("No candidate: acoustic inference should not run")
    monkeypatch.setattr(diarize, "cluster_centroids", unexpected)
    assert diarize.verify_short_turns(segments, words, samples, unexpected) == segments


def test_review_budget_covers_beginning_and_end_of_long_dialogue(monkeypatch):
    words = [Word(i, i + 0.4, "Yes.") for i in range(100)]
    segments = [Seg(0, 100, 0), Seg(100, 103, 1)]
    samples = np.arange(104 * diarize.SAMPLE_RATE, dtype=np.float32)
    monkeypatch.setattr(diarize, "cluster_centroids", lambda *a, **k: {0: [1, 0], 1: [0, 1]})
    checked = []
    def embed(chunk):
        checked.append(float(chunk[0]) / diarize.SAMPLE_RATE)
        return [1, 0]
    assert diarize.verify_short_turns(segments, words, samples, embed) == segments
    assert len(checked) == diarize.VERIFY_MAX_TURNS
    assert checked[0] == 0 and checked[-1] > 98


def test_review_reuses_voice_references_from_merge_pass():
    samples = np.concatenate([np.full(2 * diarize.SAMPLE_RATE, v, dtype=np.float32) for v in (1, 2, 1)])
    segments = [Seg(0, 2, 0), Seg(2, 4, 1), Seg(4, 6, 0)]
    words = [Word(i * 0.3, (i + 1) * 0.3, text)
             for i, text in enumerate(["We", "are", "all", "ready."])]
    words += [Word(2.1, 2.6, "Yes.")]
    words += [Word(2.8 + i * 0.2, 3 + i * 0.2, text)
              for i, text in enumerate(["Let", "us", "start", "now."])]
    calls, cache = [], {}
    def embed(chunk):
        calls.append(len(chunk))
        return [1, 0] if np.median(chunk) == 1 else [0, 1]
    merged = diarize.merge_similar_speakers(segments, embed, samples, _slice_cache=cache)
    assert len(calls) == 3
    assert diarize.verify_short_turns(merged, words, samples, embed, _slice_cache=cache) == segments
    assert len(calls) == 4  # one short candidate; none of the references are recalculated


@pytest.mark.parametrize("num_speakers", [None, 2])
def test_diarizer_checks_after_clustering_and_completes_progress_after_review(monkeypatch, num_speakers):
    progress, caches = [], []
    segments = [Seg(0, 4, 0), Seg(4, 8, 1)]
    class Engine:
        def process(self, samples, callback):
            callback(1, 1)
            return self
        def sort_by_start_time(self):
            return segments
    instance = diarize.SherpaDiarizer.__new__(diarize.SherpaDiarizer)
    instance._default = Engine()
    instance._build = lambda _: Engine()
    monkeypatch.setattr(diarize, "read_wav", lambda _: np.zeros(8 * diarize.SAMPLE_RATE, dtype=np.float32))
    def merge(raw, embed, samples, **kwargs):
        caches.append(kwargs["_slice_cache"])
        return raw
    def verify(raw, words, samples, embed, **kwargs):
        if num_speakers is None:
            assert kwargs["_slice_cache"] is caches[0]
        else:
            assert not caches  # explicit speaker count still skips automatic merging
        assert progress == [0.85, 0.9]
        return raw
    monkeypatch.setattr(diarize, "merge_similar_speakers", merge)
    monkeypatch.setattr(diarize, "verify_short_turns", verify)
    assert instance.diarize(None, num_speakers=num_speakers, on_progress=progress.append,
                            words=[Word(0, 1, "Hello.")]) == segments
    assert progress == [0.85, 0.9, 1.0]
