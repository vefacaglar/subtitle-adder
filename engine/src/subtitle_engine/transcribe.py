"""Step 2: turns audio into text with word timestamps.

Backends implement the `Transcriber` protocol; adding a new one (mlx-whisper, ...) leaves the
pipeline and the apps unchanged.
"""
import re
import sys
import wave
from collections.abc import Callable, Sequence
from pathlib import Path
from typing import Protocol

from . import models
from .models import Transcript, Word

ProgressFn = Callable[[float], None]

# In onnx-asr tokens a word start comes with a '▁' marker; the library turns it into a space.
WORD_MARKERS = (" ", "\u2581")
_SPECIAL_TOKEN = re.compile(r"^<[^<>]+>$")

# Silero's 30 ms padding can clip the first syllable. 50 ms padding and a 200 ms silence
# threshold preserve word starts and reduce needless splits mid-sentence.
VAD_OPTIONS = {"speech_pad_ms": 50, "min_silence_duration_ms": 200}


class Transcriber(Protocol):
    def transcribe(
        self, audio: Path, language: str | None = None, on_progress: ProgressFn | None = None
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
        self, audio: Path, language: str | None = None, on_progress: ProgressFn | None = None
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
            return self._transcribe(audio, language, progress)
        except RuntimeError as exc:
            # The GPU path usually fails on the first batch (a CUDA library is missing or the GPU is unsupported),
            # but a CUDA error (e.g. out of memory) can also come mid-run. Either way the run restarts on the CPU;
            # progress stays at the last reported value until the retry passes it.
            if self._device != "cuda" or not cuda.is_cuda_library_error(exc):
                raise
            print(f"CUDA unavailable, falling back to CPU: {exc}", file=sys.stderr)
            self._load("cpu")
            return self._transcribe(audio, language, progress)

    def _transcribe(self, audio: Path, language: str | None, on_progress: ProgressFn | None) -> Transcript:
        segments, info = self._model.transcribe(
            str(audio), language=language, word_timestamps=True, vad_filter=True
        )
        words: list[Word] = []
        for seg in segments:
            words.extend(
                Word(start=round(w.start, 3), end=round(w.end, 3), text=w.word.strip())
                for w in seg.words or []
                if w.word.strip()
            )
            if on_progress and info.duration:
                on_progress(min(seg.end / info.duration, 1.0))
        return Transcript(language=info.language, duration=info.duration, words=words)


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
        self, audio: Path, language: str | None = None, on_progress: ProgressFn | None = None
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
        for segment in adapter.recognize(audio, **options):
            words.extend(words_from_tokens(segment.tokens, segment.timestamps, segment.start, segment.end))
            if on_progress is not None and duration > 0:
                on_progress(min(segment.end / duration, 1.0))
        return Transcript(language=language or "?", duration=duration, words=words)


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
