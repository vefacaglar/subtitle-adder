"""Hattın tamamı: video -> ses -> transkript -> cümleler -> altyazı dosyaları."""
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path

from . import subtitles
from .audio import extract_audio
from .models import Sentence
from .sentences import SegmenterConfig, segment
from .transcribe import Transcriber

# (aşama, 0..1 arası ilerleme ya da None)
StageFn = Callable[[str, float | None], None]


@dataclass
class PipelineResult:
    audio: Path
    language: str
    duration: float
    sentences: list[Sentence]
    outputs: list[Path]


def run(
    video: Path,
    transcriber_factory: Callable[[], Transcriber],
    out_dir: Path,
    language: str | None = None,
    formats: tuple[str, ...] = ("srt",),
    config: SegmenterConfig = SegmenterConfig(),
    on_stage: StageFn = lambda stage, fraction: None,
) -> PipelineResult:
    on_stage("extracting_audio", None)
    audio = extract_audio(video, out_dir / f"{video.stem}.wav")

    on_stage("loading_model", None)
    transcriber = transcriber_factory()

    on_stage("transcribing", 0.0)
    transcript = transcriber.transcribe(
        audio, language=language, on_progress=lambda f: on_stage("transcribing", f)
    )

    on_stage("segmenting", None)
    sentences = segment(transcript.words, config)
    outputs = [subtitles.write(sentences, out_dir / f"{video.stem}.{fmt}", fmt) for fmt in formats]
    return PipelineResult(audio, transcript.language, transcript.duration, sentences, outputs)
