"""Parity driver for the se-audio tests: builds the Python test suite's fixtures with PyAV, runs the Python
engine's extract_audio / make_preview on them, and writes a manifest the Rust test compares against.

Usage: python py_parity.py <work dir>   (run with the Python engine's venv: engine/.venv)
"""
import json
import math
import sys
from pathlib import Path

import av
import numpy as np

from subtitle_engine.audio import extract_audio
from subtitle_engine.pipeline import extract_video_audio
from subtitle_engine.preview import make_preview


def tone_video(path):
    rate, seconds = 44_100, 2
    with av.open(str(path), "w") as out:
        vstream = out.add_stream("libx264", rate=10)
        vstream.width = vstream.height = 64
        vstream.pix_fmt = "yuv420p"
        astream = out.add_stream("aac", rate=rate, layout="stereo")
        for i in range(seconds * 10):
            frame = av.VideoFrame.from_ndarray(np.zeros((64, 64, 3), dtype=np.uint8), format="rgb24")
            frame.pts = i
            out.mux(vstream.encode(frame))
        t = np.arange(rate * seconds) / rate
        samples = (np.sin(2 * math.pi * 440 * t) * 0.3).astype(np.float32)
        for i in range(0, len(samples), 1024):
            block = np.stack([samples[i : i + 1024]] * 2)
            frame = av.AudioFrame.from_ndarray(block, format="fltp", layout="stereo")
            frame.sample_rate = rate
            frame.pts = i
            out.mux(astream.encode(frame))
        out.mux(vstream.encode())
        out.mux(astream.encode())


def corrupt_container(path, codec):
    with av.open(str(path), "w") as out:
        a = out.add_stream(codec, rate=44_100, layout="stereo")
        a.bit_rate = 128_000
        t = np.arange(1024, dtype=np.float32) / 44_100
        tone = np.vstack([0.3 * np.sin(2 * np.pi * 440 * t), 0.3 * np.cos(2 * np.pi * 440 * t)])
        for pts in range(0, 44_100 * 4, 1024):
            f = av.AudioFrame.from_ndarray(tone, format="fltp", layout="stereo")
            f.sample_rate = 44_100
            f.pts = pts
            out.mux(a.encode(f))
        out.mux(a.encode())
    raw = bytearray(path.read_bytes())
    off = len(raw) // 3
    raw[off : off + 64] = b"\xff" * 64
    path.write_bytes(raw)


def preview_source(source, extension, video_codec, audio_codec, video_options):
    with av.open(str(source), "w") as container:
        video = container.add_stream(video_codec, rate=10, options=video_options)
        video.width = video.height = 64
        video.pix_fmt = "yuv420p"
        audio = container.add_stream(audio_codec, rate=48_000, layout="mono")
        if audio_codec == "wmav2":
            audio.bit_rate = 64_000
        damaged = False
        for i in range(20):
            frame = av.VideoFrame.from_ndarray(np.full((64, 64, 3), i, dtype=np.uint8), format="rgb24")
            frame.pts = i
            container.mux(video.encode(frame))
            samples = av.AudioFrame.from_ndarray(np.zeros((1, 4_800), dtype=np.int16), format="s16", layout="mono")
            samples.sample_rate = 48_000
            samples.pts = 24_000 + i * 4_800
            for packet in audio.encode(samples):
                if extension == "wmv" and i >= 10 and not damaged:
                    packet.update(b"\xff" * packet.size)
                    damaged = True
                container.mux(packet)
        container.mux(video.encode())
        container.mux(audio.encode())


def main():
    work = Path(sys.argv[1])
    work.mkdir(parents=True, exist_ok=True)
    manifest = {"extract": [], "preview": []}
    inputs = []
    tone_video(work / "tone.mp4")
    inputs.append(work / "tone.mp4")
    for name, codec in (("c.wmv", "wmav2"), ("c.avi", "mp3"), ("c.mov", "aac"), ("c.mkv", "aac"), ("c.mp4", "aac")):
        corrupt_container(work / name, codec)
        inputs.append(work / name)
    previews = (
        ("p1.mkv", "mkv", "libx264", "aac", {"bf": "0"}),
        ("p2.mkv", "mkv", "libx264", "aac", {"bf": "2"}),
        ("p3.mkv", "mkv", "mpeg4", "pcm_s16le", {}),
        ("p4.wmv", "wmv", "wmv2", "wmav2", {}),
    )
    for name, ext, vc, ac, opts in previews:
        preview_source(work / name, ext, vc, ac, opts)
        inputs.append(work / name)
    for video in inputs:
        progress = []
        wav = work / (video.name + ".py.wav")
        try:
            extract_audio(video, wav, on_progress=progress.append)
            error = None
        except Exception as exc:  # noqa: BLE001
            error = str(exc)
        manifest["extract"].append({"video": str(video), "wav": str(wav), "progress": progress, "error": error})
    for name, *_ in previews:
        out = work / (name + ".py.mp4")
        try:
            make_preview(work / name, out)
            error = None
        except Exception as exc:  # noqa: BLE001
            error = str(exc)
        manifest["preview"].append({"source": str(work / name), "output": str(out), "error": error})
    stages = []
    wav = extract_video_audio(work / "tone.mp4", work / "pyout", lambda s, f: stages.append([s, f]))
    marker = wav.with_name(wav.name + ".source.json")
    manifest["reuse"] = {"wav": str(wav), "marker": marker.read_text(encoding="utf-8"), "stages": stages}
    (work / "manifest.json").write_text(json.dumps(manifest, indent=1), encoding="utf-8")


if __name__ == "__main__":
    main()
