"""Speaker diarization: who speaks when?

Fully local: sherpa-onnx (Apache-2.0, onnxruntime) running pyannote segmentation-3.0 (MIT) and the
3D-Speaker CAM++ embedding model (Apache-2.0). No pyannote.audio/torch; audio never leaves the
machine. The network is only used to download the model files
(`models.ensure_model("speaker-diarization")`).
"""
import os
import wave
from bisect import bisect_left, bisect_right
from collections.abc import Callable, Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Protocol

from . import models
from .i18n import t
from .models import ModelError, Word

ProgressFn = Callable[[float], None]

MODEL_ID = "speaker-diarization"
SAMPLE_RATE = 16000
# FastClustering threshold: a larger value finds fewer speakers (sherpa default is 0.5).
CLUSTER_THRESHOLD = 0.5
# Post-clustering merge (auto mode only): clusters whose centroid embeddings are at least this
# cosine-similar are the same person. Tuned on long audio, where clustering over-splits a speaker.
MERGE_SIMILARITY = 0.65
# Bounded sample per cluster for its centroid: longest segments first.
CENTROID_MAX_SEGMENTS = 10
CENTROID_MAX_SECONDS = 30.0
CENTROID_SEGMENT_SECONDS = 6.0  # a longer segment is cropped to its middle part
# With automatic counting, a "speaker" with less speech than this is treated as noise and dropped.
MIN_SPEAKER_SECONDS = 1.0
# Conservative post-check: a bounded number of short turns, using the existing voice model.
VERIFY_MAX_TURNS = 32
VERIFY_MIN_SIMILARITY = 0.65
# Speech compaction: only active speech intervals (plus padding) are fed to segmentation.
SPEECH_PAD_SECONDS = 0.25
SPEECH_MERGE_GAP = 1.5
SPEECH_SEPARATOR_SECONDS = 0.25
COMPACT_MIN_DURATION = 10.0
COMPACT_MAX_RATIO = 0.85


@dataclass(frozen=True)
class SpeakerSegment:
    start: float
    end: float
    speaker: int


@dataclass(frozen=True)
class _SpeechSpan:
    orig_start: float
    orig_end: float
    comp_start: float
    comp_end: float


class Diarizer(Protocol):
    def diarize(
        self,
        audio: Path,
        num_speakers: int | None = None,
        on_progress: ProgressFn | None = None,
        words: Sequence[Word] | None = None,
    ) -> list[SpeakerSegment]: ...


class SherpaDiarizer:
    """sherpa-onnx `OfflineSpeakerDiarization`; model files are read from the HF cache."""

    def __init__(self, num_threads: int | None = None, threshold: float = CLUSTER_THRESHOLD):
        import sherpa_onnx

        info = models.find(MODEL_ID)
        assert info is not None
        (embedding_repo, (embedding_file,)) = info.extra[0]
        segmentation = _cached_path(info.repo, info.files[0])
        embedding = _cached_path(embedding_repo, embedding_file)
        self._sherpa = sherpa_onnx
        self._threads = num_threads or max(1, os.cpu_count() or 1)
        self._threshold = threshold
        self._segmentation = sherpa_onnx.OfflineSpeakerSegmentationModelConfig(
            pyannote=sherpa_onnx.OfflineSpeakerSegmentationPyannoteModelConfig(model=segmentation),
            num_threads=self._threads,
        )
        self._embedding = sherpa_onnx.SpeakerEmbeddingExtractorConfig(
            model=embedding, num_threads=self._threads
        )
        self._extractor = None
        self._default = None  # built lazily; an explicit speaker count needs its own instance

    def _build(self, num_speakers: int | None):
        sherpa = self._sherpa
        config = sherpa.OfflineSpeakerDiarizationConfig(
            segmentation=self._segmentation,
            embedding=self._embedding,
            clustering=sherpa.FastClusteringConfig(
                num_clusters=num_speakers if num_speakers else -1, threshold=self._threshold
            ),
        )
        if not config.validate():
            raise ModelError(t("diarize_load_failed"))
        return sherpa.OfflineSpeakerDiarization(config)

    def diarize(
        self,
        audio: Path,
        num_speakers: int | None = None,
        on_progress: ProgressFn | None = None,
        words: Sequence[Word] | None = None,
    ) -> list[SpeakerSegment]:
        samples = read_wav(audio)
        if num_speakers:
            engine = self._build(num_speakers)
        else:
            if self._default is None:
                self._default = self._build(None)
            engine = self._default

        last = [-1.0]

        def report(fraction: float) -> None:
            if on_progress is not None and fraction > last[0]:
                last[0] = fraction
                on_progress(fraction)

        def callback(processed: int, total: int) -> int:
            if total > 0:
                report(0.85 * min(processed / total, 1.0))
            return 0  # 0 = keep going

        compact_samples, spans = compact_speech_samples(samples, words)
        result = engine.process(compact_samples, callback=callback).sort_by_start_time()
        raw = [SpeakerSegment(round(s.start, 3), round(s.end, 3), int(s.speaker)) for s in result]
        if spans is not None:
            raw = unmap_compacted_segments(raw, spans)
        slice_cache: dict[tuple[int, int], list[float]] = {}
        if not num_speakers:
            raw = merge_similar_speakers(raw, lambda chunk: self._embed(chunk), samples,
                                         _slice_cache=slice_cache)
        raw = normalize(raw, drop_noise=not num_speakers, words=words)
        report(0.9)
        if words and len({s.speaker for s in raw}) >= 2:
            raw = verify_short_turns(raw, words, samples, lambda chunk: self._embed(chunk),
                                     _slice_cache=slice_cache)
        report(1.0)
        return raw

    def _embed(self, samples) -> list[float]:
        if self._extractor is None:
            self._extractor = self._sherpa.SpeakerEmbeddingExtractor(self._embedding)
        stream = self._extractor.create_stream()
        stream.accept_waveform(sample_rate=SAMPLE_RATE, waveform=samples)
        stream.input_finished()
        return list(self._extractor.compute(stream))


def compact_speech_samples(
    samples, words: Sequence[Word] | None = None
) -> tuple[object, list[_SpeechSpan] | None]:
    """Extracts only active speech intervals when non-speech exceeds 15% of a >=10s recording.

    Uses ASR word timestamps when available (falling back to fast RMS energy windows otherwise),
    so silence, music intros/outros and SFX scenes are skipped during segmentation and clustering.
    """
    import numpy as np

    if not isinstance(samples, np.ndarray):
        return samples, None
    duration = len(samples) / SAMPLE_RATE
    if duration < COMPACT_MIN_DURATION:
        return samples, None

    intervals: list[tuple[float, float]] = []
    if words:
        for w in words:
            lo = max(0.0, w.start - SPEECH_PAD_SECONDS)
            hi = min(duration, w.end + SPEECH_PAD_SECONDS)
            if hi <= lo:
                continue
            if intervals and lo - intervals[-1][1] <= SPEECH_MERGE_GAP:
                intervals[-1] = (intervals[-1][0], max(intervals[-1][1], hi))
            else:
                intervals.append((lo, hi))
    else:
        frame_len = SAMPLE_RATE // 10  # 100 ms frames
        n_frames = len(samples) // frame_len
        if n_frames > 0:
            frames = samples[: n_frames * frame_len].reshape(n_frames, frame_len)
            rms = np.sqrt(np.mean(frames * frames, axis=1))
            floor = float(np.percentile(rms, 15))
            thresh = max(0.002, min(0.015, floor * 2.5))
            for idx, val in enumerate(rms):
                if val >= thresh:
                    lo = max(0.0, idx * 0.1 - SPEECH_PAD_SECONDS)
                    hi = min(duration, (idx + 1) * 0.1 + SPEECH_PAD_SECONDS)
                    if intervals and lo - intervals[-1][1] <= SPEECH_MERGE_GAP:
                        intervals[-1] = (intervals[-1][0], max(intervals[-1][1], hi))
                    else:
                        intervals.append((lo, hi))

    if not intervals:
        return samples, None
    speech_seconds = sum(hi - lo for lo, hi in intervals)
    if speech_seconds >= duration * COMPACT_MAX_RATIO or speech_seconds < 1.0:
        return samples, None

    sep = np.zeros(int(SPEECH_SEPARATOR_SECONDS * SAMPLE_RATE), dtype=np.float32)
    chunks: list[np.ndarray] = []
    spans: list[_SpeechSpan] = []
    cursor = 0.0
    for orig_lo, orig_hi in intervals:
        s_idx = max(0, int(round(orig_lo * SAMPLE_RATE)))
        e_idx = min(len(samples), int(round(orig_hi * SAMPLE_RATE)))
        if e_idx <= s_idx:
            continue
        if chunks:
            chunks.append(sep)
            cursor += len(sep) / SAMPLE_RATE
        chunk = samples[s_idx:e_idx]
        comp_lo = cursor
        comp_hi = cursor + len(chunk) / SAMPLE_RATE
        spans.append(_SpeechSpan(s_idx / SAMPLE_RATE, e_idx / SAMPLE_RATE, comp_lo, comp_hi))
        chunks.append(chunk)
        cursor = comp_hi

    if not chunks:
        return samples, None
    return np.concatenate(chunks), spans


def unmap_compacted_segments(
    segments: Sequence[SpeakerSegment], spans: Sequence[_SpeechSpan]
) -> list[SpeakerSegment]:
    """Maps segments from the compacted speech buffer back to the original audio timeline."""
    mapped: list[SpeakerSegment] = []
    for seg in segments:
        for span in spans:
            if span.comp_end <= seg.start:
                continue
            if span.comp_start >= seg.end:
                break
            ov_s = max(seg.start, span.comp_start)
            ov_e = min(seg.end, span.comp_end)
            if ov_e - ov_s >= 0.04:
                orig_s = round(span.orig_start + (ov_s - span.comp_start), 3)
                orig_e = round(span.orig_start + (ov_e - span.comp_start), 3)
                if orig_e > orig_s:
                    mapped.append(SpeakerSegment(orig_s, orig_e, seg.speaker))
    return mapped


def _unit(vector: Sequence[float]) -> list[float]:
    norm = sum(x * x for x in vector) ** 0.5
    return [x / norm for x in vector] if norm > 0 else list(vector)


def _cosine(a: Sequence[float], b: Sequence[float]) -> float:
    return sum(x * y for x, y in zip(_unit(a), _unit(b)))


def cluster_centroids(
    segments: Sequence[SpeakerSegment],
    samples,
    embed: Callable[[object], Sequence[float]],
    _slice_cache: dict[tuple[int, int], list[float]] | None = None,
) -> dict[int, list[float]]:
    """Centroid embedding per speaker from a bounded sample: its longest segments, at most
    CENTROID_MAX_SEGMENTS and CENTROID_MAX_SECONDS of audio, each cropped to its middle part."""
    by_speaker: dict[int, list[SpeakerSegment]] = {}
    index = _SegmentIndex(segments)
    for s in segments:
        # A mixed-voice slice is not a reliable reference for either speaker. Subtract
        # other speakers while retaining original segment boundaries for cache reuse.
        hi = bisect_left(index.starts, s.end)
        lo = bisect_right(index.max_end, s.start, 0, hi)
        cursor = s.start
        clean = by_speaker.setdefault(s.speaker, [])
        for other in index.segments[lo:hi]:
            if other.speaker == s.speaker or other.end <= cursor:
                continue
            if other.start > cursor:
                clean.append(SpeakerSegment(cursor, min(other.start, s.end), s.speaker))
            cursor = max(cursor, other.end)
            if cursor >= s.end:
                break
        if cursor < s.end:
            clean.append(SpeakerSegment(cursor, s.end, s.speaker))
    centroids: dict[int, list[float]] = {}
    for speaker, group in by_speaker.items():
        total = 0.0
        vectors: list[list[float]] = []
        weights: list[float] = []
        for s in sorted(group, key=lambda s: s.end - s.start, reverse=True)[:CENTROID_MAX_SEGMENTS]:
            length = min(s.end - s.start, CENTROID_SEGMENT_SECONDS, CENTROID_MAX_SECONDS - total)
            if length < 0.3:
                continue
            middle = (s.start + s.end) / 2
            lo = max(int((middle - length / 2) * SAMPLE_RATE), 0)
            hi = min(len(samples), lo + int(length * SAMPLE_RATE))
            if hi - lo < int(0.3 * SAMPLE_RATE):
                continue
            if _slice_cache is not None and (lo, hi) in _slice_cache:
                unit_vec = _slice_cache[(lo, hi)]
            else:
                unit_vec = _unit(embed(samples[lo:hi]))
                if _slice_cache is not None:
                    _slice_cache[(lo, hi)] = unit_vec
            vectors.append(unit_vec)
            weights.append(length)
            total += length
        if vectors:
            dim = len(vectors[0])
            centroids[speaker] = _unit(
                [sum(w * v[i] for w, v in zip(weights, vectors)) for i in range(dim)]
            )
    return centroids


def _simultaneous_speakers(segments: Sequence[SpeakerSegment]) -> set[tuple[int, int]]:
    """Do not merge voices that coexist for >=100 ms; recomputed after each cluster merge."""
    active: list[SpeakerSegment] = []
    pairs: set[tuple[int, int]] = set()
    for current in sorted(segments, key=lambda s: s.start):
        active = [s for s in active if s.end > current.start]
        for other in active:
            if other.speaker != current.speaker and min(other.end, current.end) - current.start >= 0.1:
                pairs.add(tuple(sorted((other.speaker, current.speaker))))
        active.append(current)
    return pairs


def merge_similar_speakers(
    segments: list[SpeakerSegment],
    embed_samples: Callable[[object], Sequence[float]],
    samples,
    threshold: float = MERGE_SIMILARITY,
    *,
    _slice_cache: dict[tuple[int, int], list[float]] | None = None,
) -> list[SpeakerSegment]:
    """Merges clusters whose centroids are cosine-similar, repeating until stable.

    Agglomerative clustering over a long recording tends to split one person into several
    clusters; the centroids come from a bounded sample and reuse cached segment embeddings,
    so the extra cost does not grow with length or merge count.
    """
    if len({s.speaker for s in segments}) < 2:
        return segments
    slice_cache = _slice_cache if _slice_cache is not None else {}
    centroids = cluster_centroids(segments, samples, embed_samples, _slice_cache=slice_cache)
    while len(centroids) > 1:
        keys = sorted(centroids)
        simultaneous = _simultaneous_speakers(segments)
        best = max(
            ((_cosine(centroids[a], centroids[b]), a, b) for i, a in enumerate(keys) for b in keys[i + 1 :]
             if (a, b) not in simultaneous),
            default=None,
        )
        if best is None or best[0] < threshold:
            break
        _, keep, drop = best
        segments = [SpeakerSegment(s.start, s.end, keep if s.speaker == drop else s.speaker) for s in segments]
        centroids = cluster_centroids(segments, samples, embed_samples, _slice_cache=slice_cache)
    return segments


def verify_short_turns(
    segments: list[SpeakerSegment],
    words: Sequence[Word],
    samples,
    embed_samples: Callable[[object], Sequence[float]],
    *,
    _slice_cache: dict[tuple[int, int], list[float]] | None = None,
) -> list[SpeakerSegment]:
    """Checks short replies and isolated speaker switches against existing voice centroids.

    Only unambiguous acoustic matches can change a label. Mixed-speaker/overlapping turns are
    left alone, and at most VERIFY_MAX_TURNS candidates spread across the recording are checked.
    The merge pass's cached reference embeddings can be reused; no second full diarization run.
    """
    if not segments or not words or len(samples) < SAMPLE_RATE:
        return segments
    if len({s.speaker for s in segments}) < 2:
        return segments

    index = _SegmentIndex(segments)
    word_speakers = [index.speaker_for(w) for w in words]

    # Group words into punctuation-bounded sentences.
    sentences: list[tuple[int, int]] = []
    start_idx = 0
    for i, w in enumerate(words):
        if _final(w) or i == len(words) - 1:
            sentences.append((start_idx, i + 1))
            start_idx = i + 1

    candidates: list[tuple[int, int, bool, int | None]] = []
    for s_idx, (w_lo, w_hi) in enumerate(sentences):
        count = w_hi - w_lo
        if count > 3:
            continue
        t_start = words[w_lo].start
        t_end = words[w_hi - 1].end
        dur = t_end - t_start
        if not (0.35 <= dur <= 1.8):
            continue
        prev_question = s_idx > 0 and words[sentences[s_idx - 1][1] - 1].text.rstrip(_CLOSERS).endswith("?")
        prev_gap = (t_start - words[w_lo - 1].end) if w_lo > 0 else 1.0
        cur_speaker = word_speakers[w_lo]
        if any(spk != cur_speaker for spk in word_speakers[w_lo:w_hi]):
            continue
        prev_speaker = word_speakers[w_lo - 1] if w_lo > 0 else None
        next_speaker = word_speakers[w_hi] if w_hi < len(words) else None
        next_gap = words[w_hi].start - t_end if w_hi < len(words) else 1.0
        if prev_gap < 0 or next_gap < 0:
            continue
        # Do not flatten simultaneous voices into one speaker, even if one has more overlap.
        hi = bisect_left(index.starts, t_end)
        lo = bisect_right(index.max_end, t_start, 0, hi)
        if any(s.speaker != cur_speaker and s.end > t_start for s in index.segments[lo:hi]):
            continue
        island = (prev_speaker is not None and prev_speaker == next_speaker
                  and cur_speaker != prev_speaker and max(prev_gap, next_gap) <= 0.25)
        glued_reply = (cur_speaker in (prev_speaker, next_speaker)
                       and (prev_question or prev_gap >= 0.25))
        if not island and not glued_reply:
            continue
        candidates.append((w_lo, w_hi, prev_question, prev_speaker if island else None))

    if not candidates:
        return segments
    if len(candidates) > VERIFY_MAX_TURNS:
        candidates = [candidates[i * (len(candidates) - 1) // (VERIFY_MAX_TURNS - 1)]
                      for i in range(VERIFY_MAX_TURNS)]
    slice_cache = _slice_cache if _slice_cache is not None else {}
    centroids = cluster_centroids(segments, samples, embed_samples, _slice_cache=slice_cache)
    if len(centroids) < 2:
        return segments

    overrides: list[SpeakerSegment] = []
    for w_lo, w_hi, prev_question, surrounding_speaker in candidates:
        t_start, t_end = words[w_lo].start, words[w_hi - 1].end
        cur_speaker = word_speakers[w_lo]
        if cur_speaker not in centroids:
            continue

        lo_samp = max(0, int((t_start - 0.05) * SAMPLE_RATE))
        hi_samp = min(len(samples), int((t_end + 0.05) * SAMPLE_RATE))
        if hi_samp - lo_samp < int(0.35 * SAMPLE_RATE):
            continue
        vec = _unit(embed_samples(samples[lo_samp:hi_samp]))
        ranked = sorted(((_cosine(vec, c), spk) for spk, c in centroids.items()), reverse=True)
        best_sim, best_spk = ranked[0]
        margin = 0.06 if prev_question else 0.10
        if (best_spk != cur_speaker and best_sim >= VERIFY_MIN_SIMILARITY
                and best_sim - ranked[1][0] >= margin
                and (surrounding_speaker is None or best_spk == surrounding_speaker)):
            overrides.append(SpeakerSegment(round(t_start, 3), round(t_end, 3), best_spk))

    if not overrides:
        return segments

    # Splice overridden short-turn intervals into the segment list.
    updated = list(segments)
    for ov in overrides:
        next_list: list[SpeakerSegment] = []
        for seg in updated:
            if seg.end <= ov.start or seg.start >= ov.end:
                next_list.append(seg)
            else:
                if seg.start < ov.start:
                    next_list.append(SpeakerSegment(seg.start, ov.start, seg.speaker))
                if seg.end > ov.end:
                    next_list.append(SpeakerSegment(ov.end, seg.end, seg.speaker))
        next_list.append(ov)
        updated = sorted(next_list, key=lambda s: (s.start, s.end))
    return updated


def _cached_path(repo: str, filename: str) -> str:
    path = models.huggingface_hub.try_to_load_from_cache(repo, filename)
    if not isinstance(path, str):
        raise ModelError(t("diarize_not_downloaded"))
    return path


def read_wav(path: Path):
    """Reads a 16 kHz mono 16-bit WAV as a float32 array in [-1, 1]."""
    import numpy as np

    with wave.open(str(path)) as wav:
        if wav.getframerate() != SAMPLE_RATE or wav.getnchannels() != 1 or wav.getsampwidth() != 2:
            raise ModelError(t("diarize_wav_format"))
        frames = wav.readframes(wav.getnframes())
    return np.frombuffer(frames, dtype=np.int16).astype(np.float32) / 32768.0


def _short_reply_speakers(words: Sequence[Word], segments: Sequence[SpeakerSegment]) -> set[int]:
    """Protect a short, complete ASR reply substantially covered by one diarized speaker."""
    if not words or not segments:
        return set()
    index = _SegmentIndex(segments)
    protected: set[int] = set()
    start = 0
    for end, word in enumerate(words):
        if not _final(word):
            continue
        phrase = words[start:end + 1]
        start = end + 1
        if not (1 <= len(phrase) <= 3 and 0.25 <= phrase[-1].end - phrase[0].start <= 1.8):
            continue
        speaker = index.speaker_for(phrase[0])
        if any(index.speaker_for(w) != speaker for w in phrase):
            continue
        speech = sum(max(0, w.end - w.start) for w in phrase)
        covered = 0.0
        for w in phrase:
            hi = bisect_left(index.starts, w.end)
            lo = bisect_right(index.max_end, w.start, 0, hi)
            covered += min(max(0, w.end - w.start), sum(
                max(0, min(w.end, s.end) - max(w.start, s.start))
                for s in index.segments[lo:hi] if s.speaker == speaker))
        if speech >= 0.25 and covered >= speech * 0.75:
            protected.add(speaker)
    return protected


def normalize(segments: Sequence[SpeakerSegment], drop_noise: bool = True,
              *, words: Sequence[Word] | None = None) -> list[SpeakerSegment]:
    """Sorts by time, drops speakers with very little speech and renumbers by first appearance.

    The first person to speak becomes 0. With `drop_noise` off (explicit speaker count) nobody is dropped.
    """
    ordered = sorted(segments, key=lambda s: (s.start, s.end))
    if drop_noise:
        totals: dict[int, float] = {}
        for s in ordered:
            totals[s.speaker] = totals.get(s.speaker, 0.0) + (s.end - s.start)
        if len(totals) > 1:
            keep = {k for k, v in totals.items() if v >= MIN_SPEAKER_SECONDS}
            keep.update(_short_reply_speakers(words or [], ordered))
            if keep:  # if everyone is short, leave them alone
                ordered = [s for s in ordered if s.speaker in keep]
    order: dict[int, int] = {}
    for s in ordered:
        order.setdefault(s.speaker, len(order))
    return [SpeakerSegment(s.start, s.end, order[s.speaker]) for s in ordered]


SENTENCE_FINAL = (".", "?", "!", "…")
_CLOSERS = "\"')]»”’"
SNAP_MAX_WORDS = 2
SNAP_MAX_SECONDS = 0.8


def _final(word: Word) -> bool:
    """Does the word end a sentence (., ?, !, … possibly followed by closing quotes/brackets)?"""
    return word.text.rstrip(_CLOSERS).endswith(SENTENCE_FINAL)


def assign_speakers(words: Sequence[Word], segments: Sequence[SpeakerSegment]) -> list[int | None]:
    """Gives each word a speaker, then repairs timing noise around turn changes.

    1. Largest total time overlap per speaker; no overlap -> nearest segment (None without segments).
    2. `_smooth`: a lone word (or unpunctuated 2-word jitter) between longer runs of one speaker
       joins them, unless it is a complete short reply (it, or the word before it, ends a sentence).
    3. `_snap`: a speaker change in the middle of a sentence moves to the nearest sentence end
       when only one or two words are involved.
    Speakers are renumbered by first appearance in the words. Runs in O((words + segments) log).
    """
    if not segments:
        return [None] * len(words)
    index = _SegmentIndex(segments)
    raw = [index.speaker_for(word) for word in words]
    return _renumber(_snap(words, _smooth(words, raw)))


class _SegmentIndex:
    """Segments sorted by start with prefix maxima of the end, for overlap queries in O(log n + k)."""

    def __init__(self, segments: Sequence[SpeakerSegment]):
        self.segments = sorted(segments, key=lambda s: (s.start, s.end))
        self.starts = [s.start for s in self.segments]
        self.max_end: list[float] = []
        self.arg_end: list[int] = []
        best = -1
        for i, seg in enumerate(self.segments):
            if best < 0 or seg.end > self.segments[best].end:
                best = i
            self.max_end.append(self.segments[best].end)
            self.arg_end.append(best)

    def speaker_for(self, word: Word) -> int:
        hi = bisect_left(self.starts, word.end)  # segments before hi start before the word ends
        lo = bisect_right(self.max_end, word.start, 0, hi)  # earlier ones all end by the word start
        overlap: dict[int, float] = {}
        for seg in self.segments[lo:hi]:
            shared = min(word.end, seg.end) - max(word.start, seg.start)
            if shared > 0:
                overlap[seg.speaker] = overlap.get(seg.speaker, 0.0) + shared
        if overlap:
            return max(overlap.items(), key=lambda kv: (kv[1], -kv[0]))[0]
        middle = (word.start + word.end) / 2
        candidates = []
        if hi > 0:
            candidates.append(self.segments[self.arg_end[hi - 1]])  # latest-ending segment before the word
        if hi < len(self.segments):
            candidates.append(self.segments[hi])  # first segment after it
        return min(candidates, key=lambda s: (_distance(middle, s), s.start)).speaker


def _runs(speakers: Sequence[int | None]) -> list[tuple[int, int]]:
    """(start, end exclusive) index ranges of equal consecutive speakers."""
    runs: list[tuple[int, int]] = []
    for i, speaker in enumerate(speakers):
        if runs and speakers[runs[-1][0]] == speaker:
            runs[-1] = (runs[-1][0], i + 1)
        else:
            runs.append((i, i + 1))
    return runs


def _smooth(words: Sequence[Word], speakers: list[int | None]) -> list[int | None]:
    """Joins a single-word (or brief 2-word mid-sentence) island to its surrounding speaker.

    Decided on the original assignment (no cascading) and only when both neighbouring runs have at
    least two words (three for a 2-word island), so alternating turns survive. A real short reply is
    kept: an island that ends a sentence, or that follows a word ending one
    ("Hazır mısın? | Evet. | Başlayalım." -- Turkish example).
    """
    result = list(speakers)
    runs = _runs(speakers)
    for k in range(1, len(runs) - 1):
        (start, end), before, after = runs[k], runs[k - 1], runs[k + 1]
        island_len = end - start
        if island_len not in (1, 2) or speakers[before[0]] is None:
            continue
        if speakers[before[0]] != speakers[after[0]]:
            continue
        min_surround = 2 if island_len == 1 else 3
        if before[1] - before[0] < min_surround or after[1] - after[0] < min_surround:
            continue
        if _final(words[start - 1]) or any(_final(words[ idx]) for idx in range(start, end)):
            continue
        if words[end - 1].end - words[start].start > 0.9:
            continue
        if words[start].start - words[start - 1].end > 0.25 or words[end].start - words[end - 1].end > 0.25:
            continue
        for idx in range(start, end):
            result[idx] = speakers[before[0]]
    return result


def _snap(words: Sequence[Word], speakers: list[int | None]) -> list[int | None]:
    """Moves a mid-sentence speaker change to the nearest sentence end when that touches at most
    SNAP_MAX_WORDS words and SNAP_MAX_SECONDS of speech.

    Forward: the new speaker's first words finish the previous speaker's sentence ("... hazır olur |
    dersin?", Turkish example) -> they go back to the previous speaker. Backward: the previous speaker's last words
    after a sentence end start the new speaker's sentence -> they go to the new speaker. A visible
    pause or a change right after sentence-final punctuation is preserved as a possible real turn.
    """
    result = list(speakers)
    n = len(words)
    i = 1
    while i < n:
        before, after = result[i - 1], result[i]
        if before is None or after is None or before == after or _final(words[i - 1]):
            i += 1
            continue
        if words[i].start - words[i - 1].end > 0.25:
            i += 1
            continue
        forward = None
        for j in range(i, min(i + SNAP_MAX_WORDS, n)):
            if result[j] != after:
                break
            if j > i and words[j].start - words[j - 1].end > 0.25:
                break
            if _final(words[j]):
                if words[j].end - words[i].start <= SNAP_MAX_SECONDS:
                    forward = j
                break
        backward = None
        for m in range(i - 2, max(i - 2 - SNAP_MAX_WORDS, -1), -1):
            if result[m + 1] != before:
                break
            if words[m + 2].start - words[m + 1].end > 0.25:
                break
            if _final(words[m]):
                if words[i - 1].end - words[m + 1].start <= SNAP_MAX_SECONDS:
                    backward = m + 1
                break
        fwd_cost = None if forward is None else (forward - i + 1, words[forward].end - words[i].start)
        bwd_cost = None if backward is None else (i - backward, words[i - 1].end - words[backward].start)
        if fwd_cost is not None and (bwd_cost is None or fwd_cost <= bwd_cost):
            for k in range(i, forward + 1):
                result[k] = before
            i = forward + 1
        elif bwd_cost is not None:
            for k in range(backward, i):
                result[k] = after
            i += 1
        else:
            i += 1
    return result


def _renumber(speakers: list[int | None]) -> list[int | None]:
    order: dict[int, int] = {}
    for speaker in speakers:
        if speaker is not None:
            order.setdefault(speaker, len(order))
    return [None if speaker is None else order[speaker] for speaker in speakers]


def _distance(point: float, segment: SpeakerSegment) -> float:
    if point < segment.start:
        return segment.start - point
    return max(point - segment.end, 0.0)


def create_diarizer() -> Diarizer:
    return SherpaDiarizer()
