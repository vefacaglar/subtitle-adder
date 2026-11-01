"""Step 2: turns audio into text with word timestamps.

Backends implement the `Transcriber` protocol; adding a new one (mlx-whisper, ...) leaves the
pipeline and the apps unchanged.
"""
import contextlib
import hashlib
import json
import re
import sys
import tempfile
import wave
from collections.abc import Callable, Iterator, Sequence
from pathlib import Path
from typing import Protocol

import numpy as np

from . import models
from .models import Transcript, Word

ProgressFn = Callable[[float], None]
WordsFn = Callable[[list[Word], str], None]

# In onnx-asr tokens a word start comes with a '▁' marker; the library turns it into a space.
WORD_MARKERS = (" ", "\u2581")
_SPECIAL_TOKEN = re.compile(r"^<[^<>]+>$")

# Silero's 30 ms padding can clip the first syllable. 50 ms padding and a 200 ms silence
# threshold preserve word starts and reduce needless splits mid-sentence.
VAD_OPTIONS = {"speech_pad_ms": 50, "min_silence_duration_ms": 200}

# Faster-Whisper Silero VAD defaults wait 2000 ms of silence and allow 30 s chunks, which glues
# rapid multi-speaker dialogue (e.g. fast Brazilian Portuguese) into huge windows where middle
# phrases get skipped. Tighter turn splitting (250 ms silence, <=14 s chunks, 0.35 threshold)
# preserves fast interjections and overlapping turns.
WHISPER_VAD_OPTIONS = {
    "threshold": 0.35,
    "neg_threshold": 0.20,
    "min_speech_duration_ms": 120,
    "max_speech_duration_s": 14.0,
    "min_silence_duration_ms": 250,
    "speech_pad_ms": 200,
}

# Long audio files (> 3 minutes) are decoded in ~3-minute windows split ONLY at a genuine silence pause
# (extending up to 5 minutes if a speaker is in the middle of a continuous monologue), so speech is
# NEVER cut in the middle of a sentence or word.
TRANSCRIBE_CHUNK_SECONDS = 180.0
MAX_CHUNK_EXTENSION_SECONDS = 120.0
CHUNK_SEARCH_TAIL_SECONDS = 45.0


def _find_silence_pause_cut(buf: np.ndarray, rate: int, tail_frames: int) -> tuple[int, bool]:
    """Finds the midpoint of the quietest 350 ms pause inside the last `tail_frames` samples of `buf`.

    Returns `(cut_index, is_clean_silence)`. If `is_clean_silence` is False, the caller can read ahead
    further so a continuous monologue is never split mid-sentence.
    """
    pause_frames = max(int(0.35 * rate), 1)  # 350 ms natural sentence pause
    step_frames = max(int(0.05 * rate), 1)   # 50 ms stride
    if buf.size <= pause_frames * 2:
        return buf.size, True

    search_start = max(buf.size - tail_frames, pause_frames)
    tail = buf[search_start:].astype(np.float32) / 32768.0
    if tail.size < pause_frames:
        return buf.size, True

    # Compute sliding 350 ms RMS energy with 50 ms step using cumulative sum of squares (O(N)).
    sq = np.square(tail)
    csum = np.concatenate(([0.0], np.cumsum(sq, dtype=np.float64)))
    starts = np.arange(0, tail.size - pause_frames + 1, step_frames)
    if starts.size == 0:
        return buf.size, True
    window_means = (csum[starts + pause_frames] - csum[starts]) / pause_frames
    min_idx = int(np.argmin(window_means))
    min_rms = float(np.sqrt(max(window_means[min_idx], 0.0)))
    median_rms = float(np.sqrt(max(float(np.median(window_means)), 0.0)))

    # A pause is clean silence if its RMS is < 0.008 (~ -42 dBFS) or < 25% of the median speech level.
    is_clean = min_rms <= 0.008 or (median_rms > 0 and min_rms <= median_rms * 0.25)
    best_offset = int(starts[min_idx])
    cut = search_start + best_offset + (pause_frames // 2)
    return cut, is_clean


def _iter_wav_chunks(
    audio: Path, chunk_seconds: float | None = None
) -> Iterator[tuple[int, float, np.ndarray, float]]:
    """Yields `(chunk_index, offset_seconds, float32_samples, total_duration)` windows from a 16-bit mono WAV file.

    Never splits in the middle of speech: searches for a 350 ms silence pause and extends the window if someone
    is still speaking across the nominal boundary.
    """
    effective_chunk = chunk_seconds if chunk_seconds is not None else TRANSCRIBE_CHUNK_SECONDS
    with wave.open(str(audio), "rb") as wav:
        rate = wav.getframerate()
        total_frames = wav.getnframes()
        total_duration = total_frames / rate if rate > 0 else 0.0
        target_frames = int(effective_chunk * rate)
        max_extra_frames = int(MAX_CHUNK_EXTENSION_SECONDS * rate)
        step_extend_frames = max(int(30.0 * rate), target_frames // 4)
        tail_frames = min(int(CHUNK_SEARCH_TAIL_SECONDS * rate), max(target_frames // 2, 1))

        cursor_frames = 0
        chunk_idx = 0
        carry = np.empty(0, dtype=np.int16)

        while cursor_frames < total_frames or carry.size > 0:
            needed = max(target_frames - carry.size, 0)
            raw = wav.readframes(needed) if needed > 0 else b""
            new_samples = np.frombuffer(raw, dtype="<i2") if raw else np.empty(0, dtype=np.int16)
            buf = np.concatenate((carry, new_samples)) if carry.size > 0 else new_samples
            if buf.size == 0:
                break

            # If the speaker is in the middle of a sentence at the boundary, keep reading until a pause!
            extended = 0
            while wav.tell() < total_frames and extended < max_extra_frames:
                _, is_clean = _find_silence_pause_cut(buf, rate, tail_frames)
                if is_clean:
                    break
                more_raw = wav.readframes(step_extend_frames)
                if not more_raw:
                    break
                more_samples = np.frombuffer(more_raw, dtype="<i2")
                buf = np.concatenate((buf, more_samples))
                extended += more_samples.size

            at_eof = (wav.tell() >= total_frames) and (buf.size <= target_frames + extended)
            if at_eof:
                cut = buf.size
            else:
                cut, _ = _find_silence_pause_cut(buf, rate, max(tail_frames, extended))

            chunk_i16 = buf[:cut]
            carry = buf[cut:]
            offset_sec = cursor_frames / rate
            cursor_frames += chunk_i16.size
            yield chunk_idx, offset_sec, chunk_i16.astype(np.float32) / 32768.0, total_duration
            chunk_idx += 1


def _can_stream_wav_chunks(audio: Path, min_duration: float | None = None) -> bool:
    threshold = min_duration if min_duration is not None else TRANSCRIBE_CHUNK_SECONDS
    if not audio.is_file():
        return False
    try:
        with wave.open(str(audio), "rb") as wav:
            if wav.getnchannels() != 1 or wav.getsampwidth() != 2 or wav.getframerate() <= 0:
                return False
            return (wav.getnframes() / wav.getframerate()) > threshold
    except Exception:
        return False


def _audio_fingerprint(audio: Path, size: int, blocks: int = 8, block_bytes: int = 16_384) -> str:
    """Hash of the file size plus a few evenly spaced blocks (first and last included). The extracted WAV of two
    different videos with the same name and duration has the same size, so the size alone can't tell them apart."""
    digest = hashlib.sha256(str(size).encode("ascii"))
    with audio.open("rb") as handle:
        for i in range(blocks):
            handle.seek(max(0, (size - block_bytes) * i // max(1, blocks - 1)))
            digest.update(handle.read(block_bytes))
    return digest.hexdigest()


def _transcribe_checkpoint_path(audio: Path, model_size: str, language: str | None) -> Path | None:
    try:
        stat = audio.stat()
        key = f"{audio.name}|{_audio_fingerprint(audio, stat.st_size)}|{model_size}|{language or ''}"
        digest = hashlib.sha256(key.encode("utf-8")).hexdigest()[:20]
        folder = Path(tempfile.gettempdir()) / "SubtitleAdder" / "transcriptions"
        folder.mkdir(parents=True, exist_ok=True)
        return folder / f"asr-{digest}.json"
    except Exception:
        return None


def _load_transcribe_checkpoint(path: Path | None) -> tuple[str | None, dict[int, list[Word]]]:
    if path is None or not path.is_file():
        return None, {}
    try:
        raw = json.loads(path.read_text(encoding="utf-8"))
        lang = raw.get("language")
        chunks: dict[int, list[Word]] = {}
        for k, word_list in (raw.get("chunks") or {}).items():
            chunks[int(k)] = [Word(start=float(w["start"]), end=float(w["end"]), text=str(w["text"])) for w in word_list]
        return lang, chunks
    except Exception:
        return None, {}


def _save_transcribe_checkpoint(path: Path | None, language: str | None, chunks: dict[int, list[Word]]) -> None:
    if path is None:
        return
    with contextlib.suppress(Exception):
        payload = {
            "language": language,
            "chunks": {
                str(k): [{"start": w.start, "end": w.end, "text": w.text} for w in v]
                for k, v in chunks.items()
            },
        }
        tmp = path.with_suffix(".json.tmp")
        tmp.write_text(json.dumps(payload, ensure_ascii=False), encoding="utf-8")
        tmp.replace(path)


def _delete_transcribe_checkpoint(path: Path | None) -> None:
    if path is None:
        return
    with contextlib.suppress(Exception):
        path.unlink(missing_ok=True)
    with contextlib.suppress(Exception):
        path.with_suffix(".json.tmp").unlink(missing_ok=True)


class Transcriber(Protocol):
    def transcribe(
        self,
        audio: Path,
        language: str | None = None,
        on_progress: ProgressFn | None = None,
        on_words: WordsFn | None = None,
    ) -> Transcript: ...


class FasterWhisperTranscriber:
    """faster-whisper (CTranslate2). CPU int8 on Mac; on Windows/Linux the NVIDIA GPU when the CUDA libraries load
    (`cuda` dependency group), otherwise CPU. The model is downloaded on first use."""

    def __init__(self, model_size: str = "small", device: str = "auto", compute_type: str | None = None):
        from . import cuda

        if device == "auto":
            device = "cuda" if cuda.cuda_usable() else "cpu"
        elif device == "cuda":
            cuda.register_libraries()
        self._model_size = model_size
        self._compute_type = compute_type
        self._load(device)

    def _load(self, device: str) -> None:
        from faster_whisper import WhisperModel

        # Models are stored as float16; on CPU they get expanded to float32 and slow down. int8 on CPU, float16 on CUDA.
        compute_type = self._compute_type or ("float16" if device == "cuda" else "int8")
        self._device = device
        # A cached catalog model loads from disk only: otherwise faster-whisper asks the Hub for a newer revision on every load.
        offline = models.is_downloaded(self._model_size)
        self._model = WhisperModel(
            self._model_size, device=device, compute_type=compute_type, local_files_only=offline
        )

    def transcribe(
        self,
        audio: Path,
        language: str | None = None,
        on_progress: ProgressFn | None = None,
        on_words: WordsFn | None = None,
    ) -> Transcript:
        from . import cuda

        # Progress never goes backwards, also across the CPU retry below.
        reported = 0.0

        def progress(fraction: float) -> None:
            nonlocal reported
            if on_progress is not None and fraction >= reported:
                reported = fraction
                on_progress(fraction)

        try:
            return self._transcribe(audio, language, progress, on_words)
        except RuntimeError as exc:
            # The GPU path usually fails on the first batch (a CUDA library is missing or the GPU is unsupported),
            # but a CUDA error (e.g. out of memory) can also come mid-run. Either way the run restarts on the CPU;
            # progress stays at the last reported value until the retry passes it.
            if self._device != "cuda" or not cuda.is_cuda_library_error(exc):
                raise
            print(f"CUDA unavailable, falling back to CPU: {exc}", file=sys.stderr)
            self._load("cpu")
            return self._transcribe(audio, language, progress, on_words)

    def _transcribe(
        self,
        audio: Path,
        language: str | None,
        on_progress: ProgressFn | None,
        on_words: WordsFn | None = None,
    ) -> Transcript:
        if _can_stream_wav_chunks(audio):
            return self._transcribe_chunked(audio, language, on_progress, on_words)

        segments, info = self._model.transcribe(
            str(audio),
            language=language,
            word_timestamps=True,
            vad_filter=True,
            vad_parameters=WHISPER_VAD_OPTIONS,
            condition_on_previous_text=False,
            no_speech_threshold=0.8,
            log_prob_threshold=-1.2,
            hallucination_silence_threshold=1.5,
        )
        words: list[Word] = []
        detected_language = info.language or language or "?"
        for seg in segments:
            seg_words = [
                Word(start=round(w.start, 3), end=round(w.end, 3), text=w.word.strip())
                for w in seg.words or []
                if w.word.strip()
            ]
            if seg_words:
                words.extend(seg_words)
                if on_words is not None:
                    on_words(list(words), detected_language)
            if on_progress and info.duration:
                on_progress(min(seg.end / info.duration, 1.0))
        return Transcript(language=info.language, duration=info.duration, words=words)

    def _transcribe_chunked(
        self,
        audio: Path,
        language: str | None,
        on_progress: ProgressFn | None,
        on_words: WordsFn | None = None,
    ) -> Transcript:
        checkpoint_path = _transcribe_checkpoint_path(audio, self._model_size, language)
        cached_lang, cached_chunks = _load_transcribe_checkpoint(checkpoint_path)

        words: list[Word] = []
        detected_language: str | None = language or cached_lang
        total_duration = 0.0

        for chunk_idx, offset_sec, chunk_samples, wav_duration in _iter_wav_chunks(audio):
            total_duration = wav_duration
            if chunk_idx in cached_chunks:
                chunk_words = cached_chunks[chunk_idx]
                words.extend(chunk_words)
                if on_words is not None and words:
                    on_words(list(words), detected_language or "?")
                if on_progress and total_duration > 0:
                    chunk_end = offset_sec + (chunk_samples.size / 16_000.0)
                    on_progress(min(chunk_end / total_duration, 1.0))
                continue

            segments, info = self._model.transcribe(
                chunk_samples,
                language=detected_language,
                word_timestamps=True,
                vad_filter=True,
                vad_parameters=WHISPER_VAD_OPTIONS,
                condition_on_previous_text=False,
                no_speech_threshold=0.8,
                log_prob_threshold=-1.2,
                hallucination_silence_threshold=1.5,
            )
            if not detected_language and getattr(info, "language", None):
                detected_language = info.language
            lang_label = detected_language or getattr(info, "language", None) or "?"

            chunk_words: list[Word] = []
            for seg in segments:
                seg_words = [
                    Word(
                        start=round(offset_sec + w.start, 3),
                        end=round(offset_sec + w.end, 3),
                        text=w.word.strip(),
                    )
                    for w in seg.words or []
                    if w.word.strip()
                ]
                if seg_words:
                    chunk_words.extend(seg_words)
                    words.extend(seg_words)
                    if on_words is not None:
                        on_words(list(words), lang_label)
                if on_progress and total_duration > 0:
                    on_progress(min((offset_sec + seg.end) / total_duration, 1.0))

            cached_chunks[chunk_idx] = chunk_words
            _save_transcribe_checkpoint(checkpoint_path, detected_language, cached_chunks)

            if on_progress and total_duration > 0:
                chunk_end = offset_sec + (chunk_samples.size / 16_000.0)
                on_progress(min(chunk_end / total_duration, 1.0))

        return Transcript(language=detected_language or "?", duration=total_duration, words=words)


class OnnxAsrTranscriber:
    """onnx-asr (onnxruntime) backend: Parakeet, Canary and GigaAM.

    Splits long audio with silero VAD, takes token timestamps for each segment and builds
    words from the tokens. Canary produces no token timestamps, so the segment
    duration is shared out according to token lengths.
    """

    def __init__(
        self,
        model_name: str,
        quantization: str | None = "int8",
        providers: Sequence[str] | None = ("CPUExecutionProvider",),
    ):
        import onnx_asr

        self._canary = model_name.startswith(("canary", "nemo-canary"))
        self._default_language = "ru" if model_name.startswith("gigaam") else None
        # CoreML splits the int8 graph into pieces, costing speed and quality; CPU int8
        # is fast and stable on both platforms. Anyone wanting a GPU can pass providers.
        provider_list = list(providers) if providers else None
        self._model = onnx_asr.load_model(model_name, quantization=quantization, providers=provider_list)
        self._vad = onnx_asr.load_vad("silero", providers=provider_list)

    def transcribe(
        self,
        audio: Path,
        language: str | None = None,
        on_progress: ProgressFn | None = None,
        on_words: WordsFn | None = None,
    ) -> Transcript:
        duration = _wav_duration(audio)
        language = language or self._default_language
        options: dict[str, str] = {}
        if language is not None:
            options["language"] = language
            if self._canary:  # Canary needs the target language separately; same language, not translation.
                options["target_language"] = language

        adapter = self._model.with_vad(self._vad, **VAD_OPTIONS).with_timestamps()
        words: list[Word] = []
        lang_label = language or "?"
        for segment in adapter.recognize(audio, **options):
            seg_words = words_from_tokens(segment.tokens, segment.timestamps, segment.start, segment.end)
            if seg_words:
                words.extend(seg_words)
                if on_words is not None:
                    on_words(list(words), lang_label)
            if on_progress is not None and duration > 0:
                on_progress(min(segment.end / duration, 1.0))
        return Transcript(language=lang_label, duration=duration, words=words)


def words_from_tokens(
    tokens: Sequence[str] | None,
    timestamps: Sequence[float] | None,
    segment_start: float,
    segment_end: float,
) -> list[Word]:
    """Builds words from segment tokens; converts times to absolute seconds.

    A token starting with a space or '▁' begins a new word; the others (punctuation included)
    are appended to the previous word. For models without timestamps (Canary) token durations
    are spread over the segment duration in proportion to character length.
    """
    if not tokens:
        return []
    times = (
        list(timestamps)
        if timestamps is not None
        else _estimated_timestamps(tokens, max(segment_end - segment_start, 0.0))
    )

    groups: list[list[tuple[str, float]]] = []
    current: list[tuple[str, float]] = []
    for token, at in zip(tokens, times):
        stripped = token.strip()
        if stripped and _SPECIAL_TOKEN.match(stripped):  # special tokens such as <blk>, <|en|>
            continue
        starts_word = token.startswith(WORD_MARKERS)
        content = token.lstrip(" \u2581")
        if starts_word and current:
            groups.append(current)
            current = []
        if content:
            current.append((content, at))
    if current:
        groups.append(current)

    result: list[Word] = []
    for index, group in enumerate(groups):
        start = _clamp(segment_start + group[0][1], segment_start, segment_end)
        next_start = segment_end
        if index + 1 < len(groups):
            next_start = _clamp(segment_start + groups[index + 1][0][1], segment_start, segment_end)
        end = max(next_start, start)
        result.append(Word(start=round(start, 3), end=round(end, 3), text="".join(t for t, _ in group)))
    return result


def _estimated_timestamps(tokens: Sequence[str], duration: float) -> list[float]:
    """Places tokens proportionally over the duration for models that produce no timestamps."""
    weights = [max(len(token.strip()), 1) for token in tokens]
    total = sum(weights)
    result: list[float] = []
    consumed = 0
    for weight in weights:
        result.append(duration * consumed / total)
        consumed += weight
    return result


def _clamp(value: float, low: float, high: float) -> float:
    return max(low, min(value, high))


def _wav_duration(audio: Path) -> float:
    with wave.open(str(audio)) as wav:
        return wav.getnframes() / wav.getframerate()


def create_transcriber(model_id: str) -> Transcriber:
    """Builds the right backend for a catalog id."""
    info = models.find(model_id)
    if info is not None and info.backend == "onnx":
        return OnnxAsrTranscriber(info.onnx_model or model_id, info.quantization)
    return FasterWhisperTranscriber(model_id)
