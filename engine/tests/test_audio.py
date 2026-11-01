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


def test_48khz_aac_does_not_accumulate_rounding_drift(tmp_path):
    """48 kHz 1024-sample AAC frames (341.333... samples at 16 kHz) must not drift over long durations."""
    import av
    import numpy as np

    path = tmp_path / "long_48k.mp4"
    rate = 48_000
    duration_sec = 180  # 3 minutes (> 3x the old 51.2s false-silence threshold)
    with av.open(str(path), "w") as out:
        a = out.add_stream("aac", rate=rate, layout="mono")
        total_samples = rate * duration_sec
        for pts in range(0, total_samples, 1024):
            f = av.AudioFrame.from_ndarray(np.zeros((1, 1024), dtype=np.float32), format="fltp", layout="mono")
            f.sample_rate = rate
            f.pts = pts
            out.mux(a.encode(f))
        out.mux(a.encode())

    expected_sec = ((rate * duration_sec + 1023) // 1024) * 1024 / rate
    _, _, wav_sec = read_wav(extract_audio(path, tmp_path / "out.wav"))
    assert abs(wav_sec - expected_sec) < 0.05



def test_corrupt_packet_mid_stream_is_skipped_without_failing_or_drifting(tone_video, tmp_path, monkeypatch):
    """Long WMV/ASF files often contain isolated corrupt packets that raise InvalidDataError in packet.decode()."""
    import av

    real_open = av.open

    class CorruptingPacket:
        def __init__(self, inner, fail):
            self._inner = inner
            self._fail = fail

        def decode(self):
            if self._fail:
                raise av.error.InvalidDataError(
                    1094995529, "Invalid data found when processing input", "avcodec_send_packet()"
                )
            return self._inner.decode()

    class WrappingContainer:
        def __init__(self, inner):
            self._inner = inner

        def __enter__(self):
            self._inner.__enter__()
            return self

        def __exit__(self, *args):
            return self._inner.__exit__(*args)

        @property
        def streams(self):
            return self._inner.streams

        @property
        def start_time(self):
            return self._inner.start_time

        def demux(self, stream):
            for idx, pkt in enumerate(self._inner.demux(stream)):
                yield CorruptingPacket(pkt, fail=(idx in (5, 6, 7)))

    monkeypatch.setattr(av, "open", lambda *a, **kw: WrappingContainer(real_open(*a, **kw)))
    wav = extract_audio(tone_video, tmp_path / "recovered.wav")
    rate, channels, seconds = read_wav(wav)
    assert (rate, channels) == (16_000, 1)
    assert abs(seconds - 2) < 0.15
