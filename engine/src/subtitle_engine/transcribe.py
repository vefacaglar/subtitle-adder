"""Adım 2: sesi kelime zaman damgalı metne çevirir.

Backend'ler `Transcriber` protokolünü uygular; yenisi (mlx-whisper, bulut API) eklenince
pipeline ve uygulamalar değişmez.
"""
import re
import wave
from collections.abc import Callable, Sequence
from pathlib import Path
from typing import Protocol

from . import models
from .models import Transcript, Word

ProgressFn = Callable[[float], None]

# onnx-asr token'larında kelime başı '▁' işaretiyle gelir; kütüphane bunu boşluğa çevirir.
WORD_MARKERS = (" ", "\u2581")
_SPECIAL_TOKEN = re.compile(r"^<[^<>]+>$")

# Silero'nun 30 ms dolgusu ilk heceyi kırpabiliyor. 50 ms dolgu ve 200 ms sessizlik
# eşiği kelime başlarını korur, cümle ortasında gereksiz bölünmeyi azaltır.
VAD_OPTIONS = {"speech_pad_ms": 50, "min_silence_duration_ms": 200}


class Transcriber(Protocol):
    def transcribe(
        self, audio: Path, language: str | None = None, on_progress: ProgressFn | None = None
    ) -> Transcript: ...


class FasterWhisperTranscriber:
    """faster-whisper (CTranslate2). Mac'te CPU int8, Windows'ta CUDA varsa GPU. Model ilk kullanımda indirilir."""

    def __init__(self, model_size: str = "small", device: str = "auto", compute_type: str | None = None):
        import ctranslate2
        from faster_whisper import WhisperModel

        if compute_type is None:
            # Modeller float16 kayıtlı; CPU'da float32'ye açılıp yavaşlıyor. CPU'da int8, CUDA'da float16.
            on_cuda = device == "cuda" or (device == "auto" and ctranslate2.get_cuda_device_count() > 0)
            compute_type = "float16" if on_cuda else "int8"
        self._model = WhisperModel(model_size, device=device, compute_type=compute_type)

    def transcribe(
        self, audio: Path, language: str | None = None, on_progress: ProgressFn | None = None
    ) -> Transcript:
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
    """onnx-asr (onnxruntime) backend'i: Parakeet, Canary ve GigaAM.

    Uzun sesi silero VAD ile böler, her segment için token zaman damgalarını alır ve
    token'lardan kelimeleri kurar. Canary token zaman damgası üretmediği için segment
    süresi token uzunluklarına göre paylaştırılır.
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
        # CoreML int8 grafiği parçalara bölüp hız ve kalite kaybı yaratıyor; CPU int8
        # her iki platformda da hızlı ve kararlı. GPU isteyen providers ile geçebilir.
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
            if self._canary:  # Canary hedef dili ayrıca ister; çeviri değil, aynı dil.
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
    """Segment token'larından kelimeleri kurar; zamanları mutlak saniyeye çevirir.

    Boşluk ya da '▁' ile başlayan token yeni kelime başlatır, diğerleri (noktalama dahil)
    önceki kelimeye eklenir. Zaman damgası olmayan modellerde (Canary) token süreleri
    segment süresine karakter uzunluğu oranında yayılır.
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
        if stripped and _SPECIAL_TOKEN.match(stripped):  # <blk>, <|en|> gibi özel token'lar
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
    """Zaman damgası üretmeyen modeller için token'ları süreye oransal yerleştirir."""
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
    """Katalog kimliğine göre doğru backend'i kurar."""
    info = models.find(model_id)
    if info is not None and info.backend == "onnx":
        return OnnxAsrTranscriber(info.onnx_model or model_id, info.quantization)
    return FasterWhisperTranscriber(model_id)
