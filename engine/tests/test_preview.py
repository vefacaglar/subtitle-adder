import sys

import av
import numpy as np
import pytest

from subtitle_engine.cli import main
from subtitle_engine.preview import make_preview


@pytest.mark.parametrize(
    "extension,video_codec,audio_codec,video_options",
    [
        ("mkv", "libx264", "aac", {"bf": "0"}),
        ("mkv", "libx264", "aac", {"bf": "2"}),
        ("mkv", "mpeg4", "pcm_s16le", {}),
        ("wmv", "wmv2", "wmav2", {}),
    ],
)
def test_previews_as_seekable_mp4(tmp_path, monkeypatch, extension, video_codec, audio_codec, video_options):
    source = tmp_path / "Dış disk" / f"Örnek video.{extension}"
    source.parent.mkdir()
    output = tmp_path / "preview.mp4"
    with av.open(str(source), "w") as container:
        video = container.add_stream(video_codec, rate=10, options=video_options)
        video.width = video.height = 64
        video.pix_fmt = "yuv420p"
        audio = container.add_stream(audio_codec, rate=48_000, layout="mono")
        if audio_codec == "wmav2":
            audio.bit_rate = 64_000
        damaged_packet = False
        for i in range(20):
            frame = av.VideoFrame.from_ndarray(np.full((64, 64, 3), i, dtype=np.uint8), format="rgb24")
            frame.pts = i
            container.mux(video.encode(frame))
            # Audio intentionally starts half a second after the video.
            samples = av.AudioFrame.from_ndarray(np.zeros((1, 4_800), dtype=np.int16), format="s16", layout="mono")
            samples.sample_rate = 48_000
            samples.pts = 24_000 + i * 4_800
            for packet in audio.encode(samples):
                if extension == "wmv" and i >= 10 and not damaged_packet:
                    packet.update(b"\xff" * packet.size)
                    damaged_packet = True
                container.mux(packet)
        container.mux(video.encode())
        container.mux(audio.encode())

    source_bytes = source.read_bytes()
    source.chmod(0o444)
    if extension == "wmv":
        with source.open("r", encoding="utf-8") as selected:
            monkeypatch.setattr(sys, "stdin", selected)
            assert main(["preview", "-", "--output", str(output), "--json"]) == 0
            assert not selected.closed
    else:
        assert main(["preview", str(source), "--output", str(output)]) == 0
    assert source.read_bytes() == source_bytes
    with av.open(str(output)) as result:
        video, audio = result.streams.video[0], result.streams.audio[0]
        assert video.codec_context.name == "h264"
        assert audio.codec_context.name == "aac"
        assert float(audio.start_time * audio.time_base) == pytest.approx(0.5, abs=0.06)
        assert result.duration / av.time_base == pytest.approx(2.5, abs=0.08)
        assert next(result.decode(video)).time == pytest.approx(0, abs=0.06)
        result.seek(1_000_000)
        frames = list(result.decode(video))
        assert frames[-1].time == pytest.approx(1.9, abs=0.06)


def test_preview_cannot_overwrite_source(tone_video):
    before = tone_video.read_bytes()
    with pytest.raises(FileExistsError):
        make_preview(tone_video, tone_video)
    assert tone_video.read_bytes() == before
