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


@dataclass(frozen=True)
class SpeakerSegment:
    start: float
    end: float
    speaker: int


class Diarizer(Protocol):
    def diarize(
        self, audio: Path, num_speakers: int | None = None, on_progress: ProgressFn | None = None
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
        self._threads = num_threads or max(1, min(8, os.cpu_count() or 1))
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
            raise ModelError("Konuşmacı ayrıştırma modeli yüklenemedi; dosyalar bozuk olabilir.")
        return sherpa.OfflineSpeakerDiarization(config)

    def diarize(
        self, audio: Path, num_speakers: int | None = None, on_progress: ProgressFn | None = None
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
                report(min(processed / total, 1.0))
            return 0  # 0 = keep going

        result = engine.process(samples, callback=callback).sort_by_start_time()
        raw = [SpeakerSegment(round(s.start, 3), round(s.end, 3), int(s.speaker)) for s in result]
        if not num_speakers:
            raw = merge_similar_speakers(raw, lambda chunk: self._embed(chunk), samples)
        report(1.0)
        return normalize(raw, drop_noise=not num_speakers)


    def _embed(self, samples) -> list[float]:
        if self._extractor is None:
            self._extractor = self._sherpa.SpeakerEmbeddingExtractor(self._embedding)
        stream = self._extractor.create_stream()
        stream.accept_waveform(sample_rate=SAMPLE_RATE, waveform=samples)
        stream.input_finished()
        return list(self._extractor.compute(stream))


def _unit(vector: Sequence[float]) -> list[float]:
    norm = sum(x * x for x in vector) ** 0.5
    return [x / norm for x in vector] if norm > 0 else list(vector)


def _cosine(a: Sequence[float], b: Sequence[float]) -> float:
    return sum(x * y for x, y in zip(_unit(a), _unit(b)))


def cluster_centroids(
    segments: Sequence[SpeakerSegment], samples, embed: Callable[[object], Sequence[float]]
) -> dict[int, list[float]]:
    """Centroid embedding per speaker from a bounded sample: its longest segments, at most
    CENTROID_MAX_SEGMENTS and CENTROID_MAX_SECONDS of audio, each cropped to its middle part."""
    by_speaker: dict[int, list[SpeakerSegment]] = {}
    for s in segments:
        by_speaker.setdefault(s.speaker, []).append(s)
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
            vector = embed(samples[lo : lo + int(length * SAMPLE_RATE)])
            vectors.append(_unit(vector))
            weights.append(length)
            total += length
        if vectors:
            dim = len(vectors[0])
            centroids[speaker] = _unit(
                [sum(w * v[i] for w, v in zip(weights, vectors)) for i in range(dim)]
            )
    return centroids


def merge_similar_speakers(
    segments: list[SpeakerSegment],
    embed_samples: Callable[[object], Sequence[float]],
    samples,
    threshold: float = MERGE_SIMILARITY,
) -> list[SpeakerSegment]:
    """Merges clusters whose centroids are cosine-similar, repeating until stable.

    Agglomerative clustering over a long recording tends to split one person into several
    clusters; the centroids come from a bounded sample, so the extra cost does not grow with length.
    """
    if len({s.speaker for s in segments}) < 2:
        return segments
    centroids = cluster_centroids(segments, samples, embed_samples)
    while len(centroids) > 1:
        keys = sorted(centroids)
        best = max(
            ((_cosine(centroids[a], centroids[b]), a, b) for i, a in enumerate(keys) for b in keys[i + 1 :]),
        )
        if best[0] < threshold:
            break
        _, keep, drop = best
        segments = [SpeakerSegment(s.start, s.end, keep if s.speaker == drop else s.speaker) for s in segments]
        centroids = cluster_centroids(segments, samples, embed_samples)
    return segments


def _cached_path(repo: str, filename: str) -> str:
    path = models.huggingface_hub.try_to_load_from_cache(repo, filename)
    if not isinstance(path, str):
        raise ModelError("Konuşmacı ayrıştırma modeli indirilmemiş; önce indirilmesi gerekiyor.")
    return path


def read_wav(path: Path):
    """Reads a 16 kHz mono 16-bit WAV as a float32 array in [-1, 1]."""
    import numpy as np

    with wave.open(str(path)) as wav:
        if wav.getframerate() != SAMPLE_RATE or wav.getnchannels() != 1 or wav.getsampwidth() != 2:
            raise ModelError("Konuşmacı ayrıştırma 16 kHz mono 16-bit WAV ses bekler.")
        frames = wav.readframes(wav.getnframes())
    return np.frombuffer(frames, dtype=np.int16).astype(np.float32) / 32768.0


def normalize(segments: Sequence[SpeakerSegment], drop_noise: bool = True) -> list[SpeakerSegment]:
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
    2. `_smooth`: a lone word between two longer runs of one speaker joins them, unless it is a
       complete short reply (it, or the word before it, ends a sentence).
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
    """Joins a single-word island to its surrounding speaker (A A B A A -> A A A A A).

    Decided on the original assignment (no cascading) and only when both neighbouring runs have at
    least two words, so alternating one-word turns survive. A real one-word reply is kept: an island
    that ends a sentence, or that follows a word ending one ("Hazır mısın? | Evet. | Başlayalım.").
    """
    result = list(speakers)
    runs = _runs(speakers)
    for k in range(1, len(runs) - 1):
        (start, end), before, after = runs[k], runs[k - 1], runs[k + 1]
        if end - start != 1 or speakers[before[0]] is None:
            continue
        if speakers[before[0]] != speakers[after[0]]:
            continue
        if before[1] - before[0] < 2 or after[1] - after[0] < 2:
            continue
        if _final(words[start]) or _final(words[start - 1]):
            continue
        result[start] = speakers[before[0]]
    return result


def _snap(words: Sequence[Word], speakers: list[int | None]) -> list[int | None]:
    """Moves a mid-sentence speaker change to the nearest sentence end when that touches at most
    SNAP_MAX_WORDS words and SNAP_MAX_SECONDS of speech.

    Forward: the new speaker's first words finish the previous speaker's sentence ("... hazır olur |
    dersin?") -> they go back to the previous speaker. Backward: the previous speaker's last words
    after a sentence end start the new speaker's sentence -> they go to the new speaker. Pauses are
    not used (Whisper glues word timestamps). A change right after sentence-final punctuation is
    a real turn and is never touched.
    """
    result = list(speakers)
    n = len(words)
    i = 1
    while i < n:
        before, after = result[i - 1], result[i]
        if before is None or after is None or before == after or _final(words[i - 1]):
            i += 1
            continue
        forward = None
        for j in range(i, min(i + SNAP_MAX_WORDS, n)):
            if result[j] != after:
                break
            if _final(words[j]):
                if words[j].end - words[i].start <= SNAP_MAX_SECONDS:
                    forward = j
                break
        backward = None
        for m in range(i - 2, max(i - 2 - SNAP_MAX_WORDS, -1), -1):
            if result[m + 1] != before:
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
