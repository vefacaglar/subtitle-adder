"""Adım 2: sesi kelime zaman damgalı metne çevirir.

Backend'ler `Transcriber` protokolünü uygular; yenisi (mlx-whisper, bulut API) eklenince
pipeline ve uygulamalar değişmez.
"""
from collections.abc import Callable
from pathlib import Path
from typing import Protocol

from .models import Transcript, Word

ProgressFn = Callable[[float], None]


class Transcriber(Protocol):
    def transcribe(
        self, audio: Path, language: str | None = None, on_progress: ProgressFn | None = None
    ) -> Transcript: ...


class FasterWhisperTranscriber:
    """faster-whisper (CTranslate2). Mac'te CPU int8, Windows'ta CUDA varsa GPU. Model ilk kullanımda indirilir."""

    def __init__(self, model_size: str = "small", device: str = "auto", compute_type: str = "default"):
        from faster_whisper import WhisperModel

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
