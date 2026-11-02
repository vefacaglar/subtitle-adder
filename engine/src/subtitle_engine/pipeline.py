"""The whole pipeline: video -> audio -> transcript -> sentences -> subtitle files."""
import contextlib
import gc
import hashlib
import inspect
import json
import os
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

MAX_SAFE_PATH_CHARS = 210
# `<stem>-<SOURCE_HASH_CHARS hex chars of sha256(resolved video path)>.wav`: the hash keeps two videos that share a
# file stem (`S01/Episode 1.mkv`, `S02/Episode 1.mkv`) from ever sharing a WAV in one `--out-dir`.
SOURCE_HASH_CHARS = 8


def _source_hash(video: Path) -> str:
    try:
        resolved = str(video.resolve())
    except (OSError, RuntimeError):
        resolved = os.path.abspath(video)
    return hashlib.sha256(os.path.normcase(resolved).encode("utf-8", errors="replace")).hexdigest()[:SOURCE_HASH_CHARS]


def _safe_out_stem(video: Path, out_dir: Path) -> str:
    """Keeps output paths in `out_dir` safely below the Windows 260-char MAX_PATH limit.

    A video with a 215-char filename on `D:\\` is under 260 chars in its source folder, but
    `C:\\Users\\User\\AppData\\Local\\Temp\\SubtitleAdder\\<215-char-stem>.wav` exceeds 260 chars
    and causes Windows/Python to raise `[Errno 2] No such file or directory`. The budget includes the
    `-<hash>` part of the WAV name, so the WAV (the longest name; its marker and temp files stay inside the
    260 - `MAX_SAFE_PATH_CHARS` slack) and the subtitle outputs share one stem.
    """
    stem = video.stem.rstrip(" .") or "video"
    wav_extra = 1 + SOURCE_HASH_CHARS
    candidate = out_dir / f"{stem}{'-' * wav_extra}.wav"
    if len(str(candidate)) <= MAX_SAFE_PATH_CHARS:
        return stem
    digest = hashlib.sha256(stem.encode("utf-8", errors="replace")).hexdigest()[:10]
    allowed = max(24, MAX_SAFE_PATH_CHARS - len(str(out_dir)) - 20 - wav_extra)
    prefix = stem[:allowed].rstrip(" .")
    return f"{prefix}_{digest}"


def wav_path_for(video: Path, out_dir: Path) -> Path:
    """The WAV `extract_video_audio` writes for `video`: `<safe_stem>-<hash of the resolved video path>.wav`."""
    return out_dir / f"{_safe_out_stem(video, out_dir)}-{_source_hash(video)}.wav"


@dataclass
class PipelineResult:
    audio: Path
    language: str
    duration: float
    sentences: list[Sentence]
    outputs: list[Path]


SOURCE_MARKER_SUFFIX = ".source.json"
SOURCE_MARKER_VERSION = 1


def _source_marker_path(wav_path: Path) -> Path:
    return wav_path.with_name(wav_path.name + SOURCE_MARKER_SUFFIX)


def _source_identity(video: Path) -> dict | None:
    """Identity of the source video (resolved path, size, mtime_ns), or None if it cannot be determined."""
    try:
        try:
            resolved = str(video.resolve())
        except (OSError, RuntimeError):
            resolved = os.path.abspath(video)
        info = video.stat()
        return {"source": resolved, "size": info.st_size, "mtime_ns": info.st_mtime_ns}
    except (OSError, ValueError):
        return None


def _wav_matches_source(wav_path: Path, identity: dict | None) -> bool:
    """True when `wav_path` was extracted from exactly the video described by `identity`.

    The WAV name carries a hash of the video's path, but a WAV left in `out_dir` by an earlier version of the same
    video, by an engine without a marker, or (a hash collision) by another video must never be reused.
    """
    if identity is None:
        return False
    try:
        if not wav_path.is_file():
            return False
        wav_size = wav_path.stat().st_size
        if wav_size <= 44:
            return False
        marker = json.loads(_source_marker_path(wav_path).read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return False
    if not isinstance(marker, dict) or marker.get("version") != SOURCE_MARKER_VERSION:
        return False
    # `wav_size` ties the marker to this exact WAV (a torn write by a concurrent extraction of another video
    # would leave a marker whose WAV has a different size).
    return marker.get("wav_size") == wav_size and all(marker.get(key) == value for key, value in identity.items())


def _write_source_marker(wav_path: Path, identity: dict | None, wav_size: int) -> None:
    """Atomically writes the identity marker next to `wav_path`; on failure the WAV is simply not reused later."""
    marker_path = _source_marker_path(wav_path)
    tmp_marker = marker_path.with_name(f"{marker_path.name}.{os.getpid()}.tmp")
    try:
        if identity is None:
            return
        payload = {"version": SOURCE_MARKER_VERSION, **identity, "wav_size": wav_size}
        tmp_marker.write_text(json.dumps(payload), encoding="utf-8")
        tmp_marker.replace(marker_path)
    except (OSError, ValueError, TypeError):
        with contextlib.suppress(OSError):
            tmp_marker.unlink(missing_ok=True)
        with contextlib.suppress(OSError):
            marker_path.unlink(missing_ok=True)


def extract_video_audio(
    video: Path,
    out_dir: Path,
    on_stage: StageFn = lambda stage, fraction: None,
) -> Path:
    """Extracts 16 kHz mono WAV audio for `video` into `out_dir`, reusing a WAV that came from the same video.

    The WAV is `<safe_stem>-<sha256(resolved video path)[:8]>.wav`, so videos that share a stem (different
    folders or extensions) never share a WAV, even while another extraction runs in the same `out_dir`. It is
    written to a PID-scoped temporary file first and atomically renamed into place so a concurrent or subsequent
    `transcribe` process never reads a partially written WAV file.

    An identity marker `<wav name>.source.json` (resolved source path, size and `st_mtime_ns` of the video, plus
    the WAV size) is written next to it after a successful extraction. An existing WAV is reused only when the
    marker matches the current video; a missing or mismatched marker (the same path with changed content, a
    re-encoded video, a WAV left by an older engine) means the audio is extracted again and both files are
    overwritten.
    """
    out_dir.mkdir(parents=True, exist_ok=True)
    wav_path = wav_path_for(video, out_dir)
    # Taken before extraction: a video modified while it is decoded then fails the check next time.
    identity = _source_identity(video)
    if _wav_matches_source(wav_path, identity):
        on_stage("extracting_audio", 1.0)
        return wav_path

    on_stage("extracting_audio", None)
    extract_kwargs: dict = {}
    if "on_progress" in inspect.signature(extract_audio).parameters:
        extract_kwargs["on_progress"] = lambda f: on_stage("extracting_audio", f)
    tmp_path = out_dir / f"{wav_path.stem}.{os.getpid()}.tmp.wav"
    try:
        extracted = extract_audio(video, tmp_path, **extract_kwargs)
        if tmp_path.is_file():
            try:
                wav_size = tmp_path.stat().st_size
            except OSError:
                wav_size = -1  # never matches a real WAV, so the result is simply not reused
            try:
                tmp_path.replace(wav_path)
            except OSError:
                # The target is locked (e.g. still open on Windows) or another process just produced it.
                # Its marker is untouched, so this check tells the two cases apart.
                if _wav_matches_source(wav_path, identity):
                    with contextlib.suppress(OSError):
                        tmp_path.unlink(missing_ok=True)
                    return wav_path
                # Keep the old WAV and marker as they are and use our own audio.
                return tmp_path
            # The marker of the replaced WAV (same path, so same video) names an older size/mtime or WAV size
            # and cannot match this audio; the new marker replaces it atomically.
            _write_source_marker(wav_path, identity, wav_size)
            return wav_path
        return extracted
    except BaseException:
        with contextlib.suppress(OSError):
            tmp_path.unlink(missing_ok=True)
        raise


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

    out_dir.mkdir(parents=True, exist_ok=True)
    safe_stem = _safe_out_stem(video, out_dir)
    audio = extract_video_audio(video, out_dir, on_stage=on_stage)

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

    # Self-heal if an external cleaner removed the temp WAV while models were downloading/loading.
    if isinstance(audio, Path) and not audio.is_file() and video.is_file():
        audio = extract_video_audio(video, out_dir, on_stage=on_stage)

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
        # Flush the complete ASR text before the potentially long speaker pass. Streaming ASR callbacks
        # can be throttled (or unsupported), so the UI otherwise waits until diarization for the last lines.
        if on_partial is not None:
            on_partial(segment(transcript.words, config), transcript.language)
        # Model construction, WAV loading and speech compaction have no measurable percentage yet.
        on_stage("diarizing", None)
        del transcriber  # large Whisper models keep gigabytes resident; free them before diarizing
        gc.collect()
        # Self-heal if an external cleaner removed the temp WAV during a long transcription run.
        if isinstance(audio, Path) and not audio.is_file() and video.is_file():
            audio = extract_video_audio(video, out_dir, on_stage=on_stage)
            on_stage("diarizing", None)
        diarizer = (diarizer_factory or diarization.create_diarizer)()
        diarize_kwargs: dict = {
            "num_speakers": num_speakers,
            "on_progress": lambda f: on_stage("diarizing", f if f > 0 else None),
        }
        if "words" in inspect.signature(diarizer.diarize).parameters:
            diarize_kwargs["words"] = transcript.words
        segments = diarizer.diarize(audio, **diarize_kwargs)
        speakers = diarization.assign_speakers(transcript.words, segments)

    on_stage("segmenting", None)
    sentences = segment(transcript.words, config, speakers)
    out_dir.mkdir(parents=True, exist_ok=True)
    outputs: list[Path] = []
    for fmt in formats:
        target_out = out_dir / f"{safe_stem}.{fmt}"
        try:
            outputs.append(subtitles.write(sentences, target_out, fmt))
        except OSError:
            fallback_out = out_dir / f"{safe_stem}.{os.getpid()}.{fmt}"
            outputs.append(subtitles.write(sentences, fallback_out, fmt))
    return PipelineResult(audio, transcript.language, transcript.duration, sentences, outputs)
