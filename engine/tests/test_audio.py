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


def test_late_audio_start_is_padded_to_container_timeline(tmp_path):
    import av
    import numpy as np

    path = tmp_path / "late.mp4"
    rate, offset = 44_100, 2
    with av.open(str(path), "w") as out:
        v = out.add_stream("libx264", rate=10)
        v.width = v.height = 64
        v.pix_fmt = "yuv420p"
        a = out.add_stream("aac", rate=rate, layout="mono")
        for i in range(40):  # 4 s video
            f = av.VideoFrame.from_ndarray(np.zeros((64, 64, 3), dtype=np.uint8), format="rgb24")
            f.pts = i
            out.mux(v.encode(f))
        for i in range(0, rate * 2, 1024):  # audio starts at second 2 and lasts 2 s
            f = av.AudioFrame.from_ndarray(np.full((1, 1024), 0.2, dtype=np.float32), format="fltp", layout="mono")
            f.sample_rate = rate
            f.pts = offset * rate + i
            out.mux(a.encode(f))
        out.mux(v.encode())
        out.mux(a.encode())
    _, _, seconds = read_wav(extract_audio(path, tmp_path / "out.wav"))
    assert abs(seconds - 4) < 0.15
