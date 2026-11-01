"""Hattın tamamı: video -> ses -> transkript -> cümleler -> altyazı dosyaları."""
import gc
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path

from . import diarize as diarization
from . import models, subtitles
from .audio import extract_audio
from .diarize import Diarizer
from .models import Sentence
from .sentences import SegmenterConfig, segment
from .transcribe import Transcriber

DiarizerFactory = Callable[[], "Diarizer"]

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
    model: str | None = None,
    formats: tuple[str, ...] = ("srt",),
    config: SegmenterConfig = SegmenterConfig(),
    on_stage: StageFn = lambda stage, fraction: None,
    diarize: bool = False,
    num_speakers: int | None = None,
    diarizer_factory: DiarizerFactory | None = None,
) -> PipelineResult:
    if num_speakers is not None and num_speakers < 1:
        raise ValueError("Konuşmacı sayısı en az 1 olmalı.")
    info = models.find(model) if model else None
    if info is not None and info.kind != "transcription":
        raise ValueError(f"{model} bir konuşma tanıma modeli değil; 'models' komutu geçerli modelleri listeler.")
    if language and info is not None and info.languages is not None and language not in info.languages:
        raise ValueError(models.support_error(info, language))

    on_stage("extracting_audio", None)
    audio = extract_audio(video, out_dir / f"{video.stem}.wav")

    # Katalogdaki model önbellekte değilse indirmeyi burada yaparız; böylece transkript
    # başlamadan önce ilerleme raporlanır. Katalog dışı adları faster-whisper indirir.
    if model and models.needs_download(model):
        on_stage("downloading_model", 0.0)
        models.ensure_model(model, on_progress=lambda fraction: on_stage("downloading_model", fraction))

    if diarize and models.needs_download(diarization.MODEL_ID):
        on_stage("downloading_model", 0.0)
        models.ensure_model(
            diarization.MODEL_ID, on_progress=lambda fraction: on_stage("downloading_model", fraction)
        )

    on_stage("loading_model", None)
    transcriber = transcriber_factory()

    on_stage("transcribing", 0.0)
    transcript = transcriber.transcribe(
        audio, language=language, on_progress=lambda f: on_stage("transcribing", f)
    )

    speakers = None
    if diarize:
        del transcriber  # large Whisper models keep gigabytes resident; free them before diarizing
        gc.collect()
        on_stage("diarizing", 0.0)
        segments = (diarizer_factory or diarization.create_diarizer)().diarize(
            audio, num_speakers=num_speakers, on_progress=lambda f: on_stage("diarizing", f)
        )
        speakers = diarization.assign_speakers(transcript.words, segments)

    on_stage("segmenting", None)
    sentences = segment(transcript.words, config, speakers)
    outputs = [subtitles.write(sentences, out_dir / f"{video.stem}.{fmt}", fmt) for fmt in formats]
    return PipelineResult(audio, transcript.language, transcript.duration, sentences, outputs)
