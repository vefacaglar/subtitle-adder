import pytest
from conftest import read_wav

from subtitle_engine.audio import AudioExtractionError, extract_audio


def test_extracts_mono_16k_wav(tone_video, tmp_path):
    wav = extract_audio(tone_video, tmp_path / "out.wav")
    rate, channels, seconds = read_wav(wav)
    assert (rate, channels) == (16_000, 1)
    assert abs(seconds - 2) < 0.15


def test_missing_video(tmp_path):
    with pytest.raises(AudioExtractionError):
        extract_audio(tmp_path / "yok.mp4", tmp_path / "out.wav")
