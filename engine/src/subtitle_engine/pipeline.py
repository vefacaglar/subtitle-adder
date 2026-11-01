"""The whole pipeline: video -> audio -> transcript -> sentences -> subtitle files."""
import gc
import inspect
import time
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path

from . import cuda, diarize as diarization
from . import models, subtitles
from .audio import extract_audio
from .diarize import Diarizer
from .i18n import t
from .models import Sentence, Word
from .sentences import SegmenterConfig, segment
from .transcribe import Transcriber

DiarizerFactory = Callable[[], "Diarizer"]

# (stage, progress in 0..1 or None)
StageFn = Callable[[str, float | None], None]
PartialFn = Callable[[list[Sentence], str], None]


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
    on_partial: PartialFn | None = None,
) -> PipelineResult:
    if num_speakers is not None and num_speakers < 1:
        raise ValueError(t("speakers_min"))
    info = models.find(model) if model else None
    if info is not None and info.kind != "transcription":
        raise ValueError(t("not_transcription_model", model=model))
    if language and info is not None and info.languages is not None and language not in info.languages:
        raise ValueError(models.support_error(info, language))

    # Strict offline mode: a missing model is reported before any work starts.
    if model:
        models.require_download(model)
    if diarize:
        models.require_download(diarization.MODEL_ID)

    on_stage("extracting_audio", None)
    extract_kwargs: dict = {}
    if "on_progress" in inspect.signature(extract_audio).parameters:
        extract_kwargs["on_progress"] = lambda f: on_stage("extracting_audio", f)
    audio = extract_audio(video, out_dir / f"{video.stem}.wav", **extract_kwargs)

    # If a catalog model is not cached we download it here, so progress is reported
    # before transcription starts. Names outside the catalog are downloaded by faster-whisper.
    if model and models.needs_download(model):
        on_stage("downloading_model", 0.0)
        models.ensure_model(model, on_progress=lambda fraction: on_stage("downloading_model", fraction))

    if diarize and models.needs_download(diarization.MODEL_ID):
        on_stage("downloading_model", 0.0)
        models.ensure_model(
            diarization.MODEL_ID, on_progress=lambda fraction: on_stage("downloading_model", fraction)
        )

    if (info is None or info.backend == "whisper") and cuda.needs_cuda_download():
        on_stage("downloading_model", 0.0)
        cuda.ensure_cuda(on_progress=lambda fraction: on_stage("downloading_model", fraction))

    # Everything this run needs is on disk now; names outside the catalog may still be fetched by faster-whisper.
    if info is not None or models.offline_requested():
        models.go_offline()

    on_stage("loading_model", None)
    transcriber = transcriber_factory()

    on_stage("transcribing", 0.0)
    transcribe_kwargs: dict = {
        "language": language,
        "on_progress": lambda f: on_stage("transcribing", f),
    }
    if on_partial is not None and "on_words" in inspect.signature(transcriber.transcribe).parameters:
        last_emit = 0.0

        def handle_words(words: list[Word], detected_lang: str) -> None:
            nonlocal last_emit
            now = time.monotonic()
            if now - last_emit < 0.5:
                return
            last_emit = now
            partial_sentences = segment(words, config)
            if partial_sentences:
                on_partial(partial_sentences, detected_lang)

        transcribe_kwargs["on_words"] = handle_words

    transcript = transcriber.transcribe(audio, **transcribe_kwargs)

    speakers = None
    if diarize:
        del transcriber  # large Whisper models keep gigabytes resident; free them before diarizing
        gc.collect()
        on_stage("diarizing", 0.0)
        diarizer = (diarizer_factory or diarization.create_diarizer)()
        diarize_kwargs: dict = {
            "num_speakers": num_speakers,
            "on_progress": lambda f: on_stage("diarizing", f),
        }
        if "words" in inspect.signature(diarizer.diarize).parameters:
            diarize_kwargs["words"] = transcript.words
        segments = diarizer.diarize(audio, **diarize_kwargs)
        speakers = diarization.assign_speakers(transcript.words, segments)

    on_stage("segmenting", None)
    sentences = segment(transcript.words, config, speakers)
    outputs = [subtitles.write(sentences, out_dir / f"{video.stem}.{fmt}", fmt) for fmt in formats]
    return PipelineResult(audio, transcript.language, transcript.duration, sentences, outputs)
