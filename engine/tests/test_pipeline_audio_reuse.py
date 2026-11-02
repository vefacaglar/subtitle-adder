"""`pipeline.extract_video_audio` reuses a WAV only when it was extracted from the very same video.

The WAV name depends only on the video's file stem, so the identity marker `<stem>.wav.source.json` is what keeps
a stale WAV (another video with the same stem, a replaced video, a WAV from an older engine) from being reused.
"""
import json
import os
from pathlib import Path

import pytest
from conftest import make_words

from subtitle_engine import pipeline
from subtitle_engine.audio import AudioExtractionError
from subtitle_engine.models import Transcript


class FakeExtractor:
    """Stands in for `audio.extract_audio`: writes a WAV-sized file that names the video it came from."""

    def __init__(self):
        self.calls: list[Path] = []

    def __call__(self, video, out_wav, on_progress=None):
        self.calls.append(Path(video))
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
    assert wav == out_dir / "tone.wav"
    assert marker["source"] == str(tone_video.resolve())
    assert marker["size"] == info.st_size
    assert marker["mtime_ns"] == info.st_mtime_ns
    assert marker["wav_size"] == wav.stat().st_size
    assert stages[0] == ("extracting_audio", None) and stages[-1] == ("extracting_audio", 1.0)
    assert sorted(p.name for p in out_dir.iterdir()) == ["tone.wav", "tone.wav.source.json"]  # no temp leftovers


def test_matching_marker_reuses_wav_with_single_stage(tmp_path, extractor):
    video = make_video(tmp_path / "in")
    out_dir = tmp_path / "out"
    first, first_stages = extract(video, out_dir)
    second, second_stages = extract(video, out_dir)

    assert first == second == out_dir / "Clip.wav"
    assert first_stages[0] == ("extracting_audio", None)
    assert second_stages == [("extracting_audio", 1.0)]
    assert extractor.calls == [video]


def test_same_stem_in_another_folder_is_extracted_again(tmp_path, extractor):
    video_a = make_video(tmp_path / "A", "Clip.mp4", b"video-A")
    video_b = make_video(tmp_path / "B", "Clip.mkv", b"video-B-is-longer")
    out_dir = tmp_path / "out"

    wav_a, _ = extract(video_a, out_dir)
    assert f"audio-of:{video_a}".encode() in wav_a.read_bytes()

    wav_b, stages_b = extract(video_b, out_dir)
    assert wav_b == wav_a == out_dir / "Clip.wav"
    assert f"audio-of:{video_b}".encode() in wav_b.read_bytes()
    assert stages_b[0] == ("extracting_audio", None)
    assert extractor.calls == [video_a, video_b]
    assert marker_of(wav_b)["source"] == str(video_b.resolve())

    # The marker follows the latest extraction: A is no longer served from B's audio.
    wav_a_again, stages_a_again = extract(video_a, out_dir)
    assert f"audio-of:{video_a}".encode() in wav_a_again.read_bytes()
    assert stages_a_again[0] == ("extracting_audio", None)
    assert extractor.calls == [video_a, video_b, video_a]


def test_same_size_and_mtime_in_another_folder_is_still_a_different_source(tmp_path, extractor):
    video_a = make_video(tmp_path / "A", content=b"same-size-A")
    video_b = make_video(tmp_path / "B", content=b"same-size-B")
    info = video_a.stat()
    os.utime(video_b, ns=(info.st_atime_ns, info.st_mtime_ns))
    out_dir = tmp_path / "out"

    extract(video_a, out_dir)
    wav, _ = extract(video_b, out_dir)

    assert extractor.calls == [video_a, video_b]
    assert f"audio-of:{video_b}".encode() in wav.read_bytes()


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


def test_wav_without_marker_from_older_engine_is_re_extracted(tmp_path, extractor):
    video = make_video(tmp_path / "in")
    out_dir = tmp_path / "out"
    out_dir.mkdir()
    stale = out_dir / "Clip.wav"
    stale.write_bytes(b"\0" * 200)

    wav, stages = extract(video, out_dir)

    assert wav == stale
    assert stages[0] == ("extracting_audio", None)
    assert extractor.calls == [video]
    assert f"audio-of:{video}".encode() in wav.read_bytes()
    assert marker_of(wav)["source"] == str(video.resolve())


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
    (out_dir / "Clip.wav.source.json").mkdir()  # a directory squats on the marker path: it cannot be written

    wav, stages = extract(video, out_dir)
    assert wav == out_dir / "Clip.wav" and wav.is_file()
    assert stages[0] == ("extracting_audio", None)
    assert not list(out_dir.glob("*.tmp*"))

    _, stages_again = extract(video, out_dir)
    assert stages_again[0] == ("extracting_audio", None)
    assert extractor.calls == [video, video]


def test_failed_extraction_cleans_temp_file_and_keeps_previous_wav_and_marker(tmp_path, extractor, monkeypatch):
    video_a = make_video(tmp_path / "A", content=b"video-A")
    video_b = make_video(tmp_path / "B", content=b"video-B-is-longer")
    out_dir = tmp_path / "out"
    wav, _ = extract(video_a, out_dir)
    before = wav.read_bytes()

    def failing(video, out_wav, on_progress=None):
        out_wav.write_bytes(b"\0" * 100)  # partial output
        raise RuntimeError("decode failed")

    monkeypatch.setattr(pipeline, "extract_audio", failing)
    with pytest.raises(RuntimeError, match="decode failed"):
        extract(video_b, out_dir)

    assert not list(out_dir.glob("*.tmp*"))
    assert wav.read_bytes() == before
    assert marker_of(wav)["source"] == str(video_a.resolve())


def test_locked_target_keeps_stale_wav_and_returns_own_audio(tmp_path, extractor, monkeypatch):
    """If the WAV cannot be replaced (e.g. Windows file still open), the other video's WAV must not be returned."""
    video_a = make_video(tmp_path / "A", content=b"video-A")
    video_b = make_video(tmp_path / "B", content=b"video-B-is-longer")
    out_dir = tmp_path / "out"
    wav_a, _ = extract(video_a, out_dir)

    real_replace = Path.replace

    def locked_replace(self, target):
        if self.name.endswith(".tmp.wav"):
            raise PermissionError("target is in use")
        return real_replace(self, target)

    monkeypatch.setattr(Path, "replace", locked_replace)
    result, _ = extract(video_b, out_dir)

    assert result != wav_a
    assert f"audio-of:{video_b}".encode() in result.read_bytes()
    assert f"audio-of:{video_a}".encode() in wav_a.read_bytes()


def test_missing_video_still_reports_extraction_error_and_never_reuses_a_wav(tmp_path):
    out_dir = tmp_path / "out"
    out_dir.mkdir()
    (out_dir / "yok.wav").write_bytes(b"\0" * 200)  # no marker, and no video to compare against

    with pytest.raises(AudioExtractionError, match="yok.mp4"):
        extract(tmp_path / "yok.mp4", out_dir)
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
    assert (out_dir / "Clip.wav").is_file()  # the failed run leaves A's WAV behind

    recorder = RecordingTranscriber()
    stages: list[tuple[str, float | None]] = []
    pipeline.run(video_b, lambda: recorder, out_dir, on_stage=lambda s, f: stages.append((s, f)))

    assert extractor.calls == [video_a, video_b]
    assert f"audio-of:{video_b}".encode() in recorder.audio_bytes[0]
    assert stages[0] == ("extracting_audio", None)
