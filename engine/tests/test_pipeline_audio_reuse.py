"""`pipeline.extract_video_audio` reuses a WAV only when it was extracted from the very same video.

The WAV is named `<stem>-<hash of the resolved video path>.wav`, so videos that share a stem never share a WAV; the
identity marker `<wav name>.source.json` keeps a stale WAV of the same path (a replaced or re-encoded video, a WAV from
an older engine) from being reused.
"""
import json
import os
import re
from pathlib import Path

import pytest
from conftest import make_words

from subtitle_engine import diarize, pipeline
from subtitle_engine.audio import AudioExtractionError
from subtitle_engine.models import Transcript


class FakeExtractor:
    """Stands in for `audio.extract_audio`: writes a WAV-sized file that names the video it came from."""

    def __init__(self):
        self.calls: list[Path] = []
        self.targets: list[Path] = []

    def __call__(self, video, out_wav, on_progress=None):
        self.calls.append(Path(video))
        self.targets.append(Path(out_wav))
        on_progress and on_progress(0.5)
        out_wav.write_bytes(b"\0" * 64 + f"audio-of:{video}".encode())
        on_progress and on_progress(1.0)
        return out_wav


@pytest.fixture
def extractor(monkeypatch):
    fake = FakeExtractor()
    monkeypatch.setattr(pipeline, "extract_audio", fake)
    return fake


def make_video(folder: Path, name: str = "Clip.mp4", content: bytes = b"video-bytes") -> Path:
    folder.mkdir(parents=True, exist_ok=True)
    path = folder / name
    path.write_bytes(content)
    return path


def extract(video: Path, out_dir: Path) -> tuple[Path, list[tuple[str, float | None]]]:
    stages: list[tuple[str, float | None]] = []
    wav = pipeline.extract_video_audio(video, out_dir, on_stage=lambda stage, fraction: stages.append((stage, fraction)))
    return wav, stages


def marker_of(wav: Path) -> dict:
    return json.loads(wav.with_name(wav.name + ".source.json").read_text(encoding="utf-8"))


def test_real_extraction_writes_identity_marker(tone_video, tmp_path):
    out_dir = tmp_path / "out"
    wav, stages = extract(tone_video, out_dir)

    info = tone_video.stat()
    marker = marker_of(wav)
    assert wav == pipeline.wav_path_for(tone_video, out_dir)
    assert re.fullmatch(r"tone-[0-9a-f]{8}\.wav", wav.name)
    assert marker["source"] == str(tone_video.resolve())
    assert marker["size"] == info.st_size
    assert marker["mtime_ns"] == info.st_mtime_ns
    assert marker["wav_size"] == wav.stat().st_size
    assert stages[0] == ("extracting_audio", None) and stages[-1] == ("extracting_audio", 1.0)
    assert sorted(p.name for p in out_dir.iterdir()) == sorted([wav.name, wav.name + ".source.json"])  # no temp leftovers


def test_matching_marker_reuses_wav_with_single_stage(tmp_path, extractor):
    video = make_video(tmp_path / "in")
    out_dir = tmp_path / "out"
    first, first_stages = extract(video, out_dir)
    second, second_stages = extract(video, out_dir)

    assert first == second == pipeline.wav_path_for(video, out_dir)
    assert first_stages[0] == ("extracting_audio", None)
    assert second_stages == [("extracting_audio", 1.0)]
    assert extractor.calls == [video]


def test_same_stem_in_another_folder_gets_its_own_wav_and_both_stay_reusable(tmp_path, extractor):
    video_a = make_video(tmp_path / "S01", "Episode 1.mkv", b"video-A")
    video_b = make_video(tmp_path / "S02", "Episode 1.mkv", b"video-B-is-longer")
    out_dir = tmp_path / "out"

    wav_a, _ = extract(video_a, out_dir)
    wav_b, stages_b = extract(video_b, out_dir)

    assert wav_a != wav_b
    assert wav_a.name.startswith("Episode 1-") and wav_b.name.startswith("Episode 1-")
    assert f"audio-of:{video_a}".encode() in wav_a.read_bytes()
    assert f"audio-of:{video_b}".encode() in wav_b.read_bytes()
    assert stages_b[0] == ("extracting_audio", None)
    assert marker_of(wav_a)["source"] == str(video_a.resolve())
    assert marker_of(wav_b)["source"] == str(video_b.resolve())

    # Extracting B did not disturb A: it is reused, not extracted again.
    wav_a_again, stages_a_again = extract(video_a, out_dir)
    assert wav_a_again == wav_a
    assert stages_a_again == [("extracting_audio", 1.0)]
    assert extractor.calls == [video_a, video_b]


def test_same_stem_different_extension_in_one_folder_gets_its_own_wav(tmp_path, extractor):
    mkv = make_video(tmp_path / "in", "Clip.mkv", b"video-mkv")
    mp4 = make_video(tmp_path / "in", "Clip.mp4", b"video-mp4-longer")
    out_dir = tmp_path / "out"

    wav_mkv, _ = extract(mkv, out_dir)
    wav_mp4, _ = extract(mp4, out_dir)

    assert wav_mkv != wav_mp4
    assert f"audio-of:{mkv}".encode() in wav_mkv.read_bytes()
    assert f"audio-of:{mp4}".encode() in wav_mp4.read_bytes()


def test_wav_name_hash_follows_the_resolved_path(tmp_path):
    video = make_video(tmp_path / "in", "Clip.mp4")
    out_dir = tmp_path / "out"
    alias = tmp_path / "in" / ".." / "in" / "Clip.mp4"  # the same file through a different spelling
    other = make_video(tmp_path / "other", "Clip.mp4")

    assert pipeline.wav_path_for(alias, out_dir) == pipeline.wav_path_for(video, out_dir)
    assert pipeline.wav_path_for(other, out_dir) != pipeline.wav_path_for(video, out_dir)


def test_long_stem_keeps_the_wav_and_its_companions_inside_the_path_budget(tmp_path):
    out_dir = tmp_path / "out"
    video = tmp_path / "in" / ("x" * 215 + ".mp4")
    wav = pipeline.wav_path_for(video, out_dir)

    assert len(str(wav)) <= pipeline.MAX_SAFE_PATH_CHARS
    longest_companion = out_dir / f"{wav.stem}.{4_194_304}.tmp.wav"
    marker_tmp = wav.with_name(f"{wav.name}.source.json.4194304.tmp")
    assert max(len(str(longest_companion)), len(str(marker_tmp))) < 260
    assert re.search(r"-[0-9a-f]{8}\.wav$", wav.name)


def test_same_size_and_mtime_in_another_folder_is_still_a_different_source(tmp_path, extractor):
    video_a = make_video(tmp_path / "A", content=b"same-size-A")
    video_b = make_video(tmp_path / "B", content=b"same-size-B")
    info = video_a.stat()
    os.utime(video_b, ns=(info.st_atime_ns, info.st_mtime_ns))
    out_dir = tmp_path / "out"

    wav_a, _ = extract(video_a, out_dir)
    wav_b, _ = extract(video_b, out_dir)

    assert wav_a != wav_b
    assert extractor.calls == [video_a, video_b]
    assert f"audio-of:{video_b}".encode() in wav_b.read_bytes()
    assert f"audio-of:{video_a}".encode() in wav_a.read_bytes()


def test_changed_size_re_extracts(tmp_path, extractor):
    video = make_video(tmp_path / "in")
    out_dir = tmp_path / "out"
    extract(video, out_dir)

    info = video.stat()
    video.write_bytes(b"re-encoded video with a different size")
    os.utime(video, ns=(info.st_atime_ns, info.st_mtime_ns))  # only the size differs
    wav, stages = extract(video, out_dir)

    assert stages[0] == ("extracting_audio", None)
    assert extractor.calls == [video, video]
    assert marker_of(wav)["size"] == video.stat().st_size


def test_changed_mtime_re_extracts(tmp_path, extractor):
    video = make_video(tmp_path / "in")
    out_dir = tmp_path / "out"
    extract(video, out_dir)

    info = video.stat()
    os.utime(video, ns=(info.st_atime_ns, info.st_mtime_ns + 5_000_000_000))  # same size, newer mtime
    wav, stages = extract(video, out_dir)

    assert stages[0] == ("extracting_audio", None)
    assert extractor.calls == [video, video]
    assert marker_of(wav)["mtime_ns"] == video.stat().st_mtime_ns


def test_wav_without_marker_is_re_extracted(tmp_path, extractor):
    video = make_video(tmp_path / "in")
    out_dir = tmp_path / "out"
    out_dir.mkdir()
    stale = pipeline.wav_path_for(video, out_dir)
    stale.write_bytes(b"\0" * 200)

    wav, stages = extract(video, out_dir)

    assert wav == stale
    assert stages[0] == ("extracting_audio", None)
    assert extractor.calls == [video]
    assert f"audio-of:{video}".encode() in wav.read_bytes()
    assert marker_of(wav)["source"] == str(video.resolve())


def test_wav_left_by_the_older_stem_only_naming_is_ignored_and_untouched(tmp_path, extractor):
    video = make_video(tmp_path / "in")
    out_dir = tmp_path / "out"
    out_dir.mkdir()
    legacy = out_dir / "Clip.wav"
    legacy.write_bytes(b"\0" * 200)

    wav, stages = extract(video, out_dir)

    assert wav != legacy
    assert stages[0] == ("extracting_audio", None)
    assert legacy.read_bytes() == b"\0" * 200


@pytest.mark.parametrize("garbage", ["not json", "[]", '{"source": "x"}', ""])
def test_unreadable_or_incomplete_marker_is_re_extracted(tmp_path, extractor, garbage):
    video = make_video(tmp_path / "in")
    out_dir = tmp_path / "out"
    wav, _ = extract(video, out_dir)
    wav.with_name(wav.name + ".source.json").write_text(garbage, encoding="utf-8")

    _, stages = extract(video, out_dir)

    assert stages[0] == ("extracting_audio", None)
    assert extractor.calls == [video, video]


def test_tiny_wav_is_not_reused_even_with_matching_marker(tmp_path, extractor):
    video = make_video(tmp_path / "in")
    out_dir = tmp_path / "out"
    wav, _ = extract(video, out_dir)
    wav.write_bytes(b"\0" * 44)  # header only

    _, stages = extract(video, out_dir)

    assert stages[0] == ("extracting_audio", None)
    assert extractor.calls == [video, video]


def test_wav_replaced_behind_the_markers_back_is_not_reused(tmp_path, extractor):
    video = make_video(tmp_path / "in")
    out_dir = tmp_path / "out"
    wav, _ = extract(video, out_dir)
    wav.write_bytes(b"\0" * 500)  # e.g. another process wrote a different video's audio meanwhile

    _, stages = extract(video, out_dir)

    assert stages[0] == ("extracting_audio", None)
    assert extractor.calls == [video, video]


def test_unwritable_marker_does_not_crash_and_wav_is_not_reused(tmp_path, extractor):
    video = make_video(tmp_path / "in")
    out_dir = tmp_path / "out"
    out_dir.mkdir()
    expected = pipeline.wav_path_for(video, out_dir)
    (out_dir / (expected.name + ".source.json")).mkdir()  # a directory squats on the marker path: it cannot be written

    wav, stages = extract(video, out_dir)
    assert wav == expected and wav.is_file()
    assert stages[0] == ("extracting_audio", None)
    assert not list(out_dir.glob("*.tmp*"))

    _, stages_again = extract(video, out_dir)
    assert stages_again[0] == ("extracting_audio", None)
    assert extractor.calls == [video, video]


def test_failed_extraction_cleans_temp_file_and_keeps_previous_wav_and_marker(tmp_path, extractor, monkeypatch):
    video = make_video(tmp_path / "in", content=b"video-v1")
    out_dir = tmp_path / "out"
    wav, _ = extract(video, out_dir)
    before = wav.read_bytes()
    old_marker = marker_of(wav)
    video.write_bytes(b"video-v2-is-longer")  # same path, changed content: it must be extracted again

    def failing(video, out_wav, on_progress=None):
        out_wav.write_bytes(b"\0" * 100)  # partial output
        raise RuntimeError("decode failed")

    monkeypatch.setattr(pipeline, "extract_audio", failing)
    with pytest.raises(RuntimeError, match="decode failed"):
        extract(video, out_dir)

    assert not list(out_dir.glob("*.tmp*"))
    assert wav.read_bytes() == before
    assert marker_of(wav) == old_marker


def test_locked_target_keeps_old_wav_and_marker_and_returns_own_audio(tmp_path, extractor, monkeypatch):
    """If the WAV cannot be replaced (e.g. Windows file still open), the old WAV is not returned and not touched."""
    video = make_video(tmp_path / "in", content=b"video-v1")
    out_dir = tmp_path / "out"
    wav, _ = extract(video, out_dir)
    old_bytes, old_marker = wav.read_bytes(), marker_of(wav)
    video.write_bytes(b"video-v2-is-longer")

    real_replace = Path.replace

    def locked_replace(self, target):
        if self.name.endswith(".tmp.wav"):
            raise PermissionError("target is in use")
        return real_replace(self, target)

    monkeypatch.setattr(Path, "replace", locked_replace)
    result, _ = extract(video, out_dir)

    assert result != wav and result.name.endswith(".tmp.wav")
    assert b"audio-of:" in result.read_bytes()
    assert wav.read_bytes() == old_bytes
    assert marker_of(wav) == old_marker  # the marker of the WAV that stayed is not deleted


def test_locked_target_already_produced_by_another_process_for_this_video_is_reused(tmp_path, extractor, monkeypatch):
    """Rename fails because a concurrent extraction of this very video just put its WAV (and marker) in place."""
    video = make_video(tmp_path / "in")
    out_dir = tmp_path / "out"
    out_dir.mkdir()
    wav = pipeline.wav_path_for(video, out_dir)
    real_replace = Path.replace

    def raced_replace(self, target):
        if self.name.endswith(".tmp.wav"):
            body = b"\0" * 64 + b"written-by-the-other-process"
            wav.write_bytes(body)
            pipeline._write_source_marker(wav, pipeline._source_identity(video), len(body))
            raise PermissionError("target is in use")
        return real_replace(self, target)

    monkeypatch.setattr(Path, "replace", raced_replace)
    result, _ = extract(video, out_dir)

    assert result == wav
    assert b"written-by-the-other-process" in wav.read_bytes()
    assert not list(out_dir.glob("*.tmp.wav"))  # our own temp file was dropped


def test_missing_video_still_reports_extraction_error_and_never_reuses_a_wav(tmp_path):
    out_dir = tmp_path / "out"
    out_dir.mkdir()
    missing = tmp_path / "yok.mp4"
    pipeline.wav_path_for(missing, out_dir).write_bytes(b"\0" * 200)  # no marker, and no video to compare against

    with pytest.raises(AudioExtractionError, match="yok.mp4"):
        extract(missing, out_dir)
    assert not list(out_dir.glob("*.tmp*"))


class RecordingTranscriber:
    def __init__(self):
        self.audio_bytes: list[bytes] = []

    def transcribe(self, audio, language=None, on_progress=None):
        self.audio_bytes.append(Path(audio).read_bytes())
        return Transcript(language="tr", duration=2.0, words=make_words("Merhaba dünya."))


def test_run_after_a_failed_run_of_another_video_with_the_same_stem_uses_its_own_audio(tmp_path, extractor):
    video_a = make_video(tmp_path / "A", "Clip.mp4", b"video-A")
    video_b = make_video(tmp_path / "B", "Clip.mkv", b"video-B-is-longer")
    out_dir = tmp_path / "out"

    class Boom:
        def transcribe(self, audio, language=None, on_progress=None):
            raise RuntimeError("cancelled")

    with pytest.raises(RuntimeError):
        pipeline.run(video_a, Boom, out_dir)
    assert pipeline.wav_path_for(video_a, out_dir).is_file()  # the failed run leaves A's WAV behind

    recorder = RecordingTranscriber()
    stages: list[tuple[str, float | None]] = []
    pipeline.run(video_b, lambda: recorder, out_dir, on_stage=lambda s, f: stages.append((s, f)))

    assert extractor.calls == [video_a, video_b]
    assert f"audio-of:{video_b}".encode() in recorder.audio_bytes[0]
    assert stages[0] == ("extracting_audio", None)
    assert pipeline.wav_path_for(video_a, out_dir).is_file()  # and B's run did not replace it


def test_same_stem_extraction_between_audio_step_and_transcription_does_not_change_the_audio(tmp_path, extractor):
    """The Windows playlist pre-extracts the next item into the shared folder while the current one loads its model."""
    video_a = make_video(tmp_path / "S01", "Episode 1.mkv", b"video-A")
    video_b = make_video(tmp_path / "S02", "Episode 1.mkv", b"video-B-is-longer")
    out_dir = tmp_path / "out"
    recorder = RecordingTranscriber()

    def factory():  # runs after run()'s audio step and before its transcription
        pipeline.extract_video_audio(video_b, out_dir)
        return recorder

    result = pipeline.run(video_a, factory, out_dir)

    assert extractor.calls == [video_a, video_b]
    assert f"audio-of:{video_a}".encode() in recorder.audio_bytes[0]
    assert f"audio-of:{video_b}".encode() not in recorder.audio_bytes[0]
    assert result.audio == pipeline.wav_path_for(video_a, out_dir)


def test_same_stem_extraction_during_transcription_does_not_change_the_diarized_audio(tmp_path, extractor, monkeypatch):
    video_a = make_video(tmp_path / "S01", "Episode 1.mkv", b"video-A")
    video_b = make_video(tmp_path / "S02", "Episode 1.mkv", b"video-B-is-longer")
    out_dir = tmp_path / "out"
    monkeypatch.setattr(pipeline.models, "needs_download", lambda model_id: False)

    class RacingTranscriber:
        def transcribe(self, audio, language=None, on_progress=None):
            pipeline.extract_video_audio(video_b, out_dir)
            return Transcript(language="tr", duration=2.0, words=make_words("Merhaba dünya."))

    diarized: list[bytes] = []

    class RecordingDiarizer:
        def diarize(self, audio, num_speakers=None, on_progress=None):
            diarized.append(Path(audio).read_bytes())
            return [diarize.SpeakerSegment(0.0, 2.0, 0)]

    pipeline.run(video_a, RacingTranscriber, out_dir, diarize=True, diarizer_factory=RecordingDiarizer)

    assert f"audio-of:{video_a}".encode() in diarized[0]


def test_wav_removed_while_loading_the_model_is_re_extracted_atomically_with_a_marker(tmp_path, extractor):
    video = make_video(tmp_path / "in")
    out_dir = tmp_path / "out"
    recorder = RecordingTranscriber()
    stages: list[tuple[str, float | None]] = []

    def factory():
        pipeline.wav_path_for(video, out_dir).unlink()  # an external cleaner removes the temp WAV
        return recorder

    result = pipeline.run(video, factory, out_dir, on_stage=lambda s, f: stages.append((s, f)))

    assert extractor.calls == [video, video]
    assert all(target.name.endswith(".tmp.wav") for target in extractor.targets)  # never written in place
    assert result.audio == pipeline.wav_path_for(video, out_dir)
    assert f"audio-of:{video}".encode() in recorder.audio_bytes[0]
    assert marker_of(result.audio)["source"] == str(video.resolve())
    assert not list(out_dir.glob("*.tmp*"))
    loading = stages.index(("loading_model", None))
    assert loading < stages.index(("extracting_audio", None), loading) < stages.index(("transcribing", 0.0))


def test_wav_removed_during_transcription_is_re_extracted_before_diarization(tmp_path, extractor, monkeypatch):
    video = make_video(tmp_path / "in")
    out_dir = tmp_path / "out"
    monkeypatch.setattr(pipeline.models, "needs_download", lambda model_id: False)

    class CleanedTranscriber:
        def transcribe(self, audio, language=None, on_progress=None):
            Path(audio).unlink()  # an external cleaner removes the temp WAV mid-run
            return Transcript(language="tr", duration=2.0, words=make_words("Merhaba dünya."))

    diarized: list[Path] = []

    class RecordingDiarizer:
        def diarize(self, audio, num_speakers=None, on_progress=None):
            diarized.append(Path(audio))
            assert Path(audio).is_file()
            return [diarize.SpeakerSegment(0.0, 2.0, 0)]

    result = pipeline.run(video, CleanedTranscriber, out_dir, diarize=True, diarizer_factory=RecordingDiarizer)

    assert extractor.calls == [video, video]
    assert all(target.name.endswith(".tmp.wav") for target in extractor.targets)
    assert diarized == [pipeline.wav_path_for(video, out_dir)] == [result.audio]
    assert marker_of(result.audio)["source"] == str(video.resolve())
    assert not list(out_dir.glob("*.tmp*"))


def test_subtitle_write_failure_falls_back_to_a_pid_named_file(tmp_path, extractor):
    video = make_video(tmp_path / "in", "Clip.mp4")
    out_dir = tmp_path / "out"
    (out_dir / "Clip.srt").mkdir(parents=True)  # a directory squats on the target: writing it raises OSError

    result = pipeline.run(video, RecordingTranscriber, out_dir, formats=("srt", "vtt"))

    fallback = out_dir / f"Clip.{os.getpid()}.srt"
    assert result.outputs == [fallback, out_dir / "Clip.vtt"]
    assert "Merhaba" in fallback.read_text(encoding="utf-8")
    assert (out_dir / "Clip.vtt").is_file() and (out_dir / "Clip.srt").is_dir()
