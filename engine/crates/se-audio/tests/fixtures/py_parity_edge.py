"""Edge-case parity driver for the se-audio tests (same manifest format as py_parity.py).

Builds inputs the Python test suite does not cover (timestamp gaps/overlaps/spikes, late origins, passthrough
input, mid-stream format changes, several audio streams, broken files, odd preview sources), runs the Python
engine's extract_audio / make_preview on them and writes manifest.json for the Rust side.

Usage: python py_parity_edge.py <work dir>   (run with the Python engine's venv: engine/.venv)
"""
import json
import math
from fractions import Fraction
import sys
from pathlib import Path

import av
import numpy as np

from subtitle_engine.audio import extract_audio
from subtitle_engine.preview import make_preview


def tone(n, rate, freq=440.0, start=0):
    t = (np.arange(n) + start) / rate
    return (0.3 * np.sin(2 * math.pi * freq * t)).astype(np.float32)


def s16_frame(samples, rate, pts, channels=1):
    data = (samples * 32767).astype(np.int16)
    if channels == 2:
        data = np.stack([data, data], axis=1).reshape(1, -1)
    else:
        data = data.reshape(1, -1)
    f = av.AudioFrame.from_ndarray(data, format="s16", layout="mono" if channels == 1 else "stereo")
    f.sample_rate = rate
    f.pts = pts
    return f


def pcm_with_timeline(path, rate, pts_list, block=1600, codec="pcm_s16le", video=False):
    """Audio frames of `block` samples at the given pts (in 1/rate units); optional one-frame video at 0."""
    with av.open(str(path), "w") as out:
        v = None
        if video:
            v = out.add_stream("mpeg4", rate=1)
            v.width = v.height = 32
            v.pix_fmt = "yuv420p"
        a = out.add_stream(codec, rate=rate, layout="mono")
        a.time_base = Fraction(1, rate)
        if v is not None:
            for i in range(2):
                fr = av.VideoFrame.from_ndarray(np.zeros((32, 32, 3), dtype=np.uint8), format="rgb24")
                fr.pts = i
                out.mux(v.encode(fr))
        for k, pts in enumerate(pts_list):
            out.mux(a.encode(s16_frame(tone(block, rate, start=k * block), rate, pts)))
        out.mux(a.encode())
        if v is not None:
            out.mux(v.encode())


def encoded_audio(path, codec, rate, channels, seconds, fmt="fltp", container_format=None, bit_rate=None):
    layout = "mono" if channels == 1 else ("stereo" if channels == 2 else "5.1")
    with av.open(str(path), "w", format=container_format) as out:
        a = out.add_stream(codec, rate=rate, layout=layout)
        if bit_rate:
            a.bit_rate = bit_rate
        cc = a.codec_context
        fmt = cc.format.name if cc.format is not None else fmt
        n = 1152 if codec in ("mp2", "libmp3lame") else 1024
        total = rate * seconds
        for i in range(0, total, n):
            x = tone(min(n, total - i), rate, start=i)
            planar = fmt.endswith("p")
            base = fmt.rstrip("p")
            if base == "s16":
                xx = (x * 32767).astype(np.int16)
            elif base == "s32":
                xx = (x * 2**31 * 0.99).astype(np.int32)
            else:
                xx = x
            if planar:
                arr = np.stack([xx] * channels)
            else:
                arr = np.stack([xx] * channels, axis=1).reshape(1, -1)
            f = av.AudioFrame.from_ndarray(arr, format=fmt, layout=layout)
            f.sample_rate = rate
            f.pts = i
            out.mux(a.encode(f))
        out.mux(a.encode())


def two_audio_streams(path):
    with av.open(str(path), "w") as out:
        a1 = out.add_stream("aac", rate=22_050, layout="mono")
        a2 = out.add_stream("aac", rate=44_100, layout="stereo")
        for i in range(0, 22_050 * 2, 1024):
            f = av.AudioFrame.from_ndarray(np.stack([tone(1024, 22_050, 300, i)]), format="fltp", layout="mono")
            f.sample_rate = 22_050
            f.pts = i
            out.mux(a1.encode(f))
        for i in range(0, 44_100 * 3, 1024):
            x = tone(1024, 44_100, 500, i)
            f = av.AudioFrame.from_ndarray(np.stack([x, x]), format="fltp", layout="stereo")
            f.sample_rate = 44_100
            f.pts = i
            out.mux(a2.encode(f))
        out.mux(a1.encode())
        out.mux(a2.encode())


def video_frames(stream, count, w, h, fmt="rgb24"):
    for i in range(count):
        img = np.full((h, w, 3), (i * 9) % 256, dtype=np.uint8)
        img[: h // 2, : w // 3] = 200
        fr = av.VideoFrame.from_ndarray(img, format="rgb24")
        if fmt != "rgb24":
            fr = fr.reformat(format=fmt)
        fr.pts = i
        yield fr


def av_source(path, vcodec, w, h, pix_fmt, acodec=None, arate=48_000, alayout="stereo", options=None, frames=15,
              afmt="fltp", container_format=None):
    with av.open(str(path), "w", format=container_format) as out:
        v = None
        if vcodec:
            v = out.add_stream(vcodec, rate=10, options=options or {})
            v.width, v.height = w, h
            v.pix_fmt = pix_fmt
        a = None
        if acodec:
            a = out.add_stream(acodec, rate=arate, layout=alayout)
        if v is not None:
            for fr in video_frames(v, frames, w, h):
                out.mux(v.encode(fr))
        if a is not None:
            ch = {"mono": 1, "stereo": 2, "5.1": 6}[alayout]
            fmt = a.codec_context.format.name
            n = a.codec_context.frame_size or 1024
            total = arate * frames // 10
            for i in range(0, total, n):
                x = tone(min(n, total - i), arate, start=i)
                if fmt.rstrip("p") == "s16":
                    x = (x * 32767).astype(np.int16)
                arr = np.stack([x] * ch) if fmt.endswith("p") else np.stack([x] * ch, axis=1).reshape(1, -1)
                f = av.AudioFrame.from_ndarray(arr, format=fmt, layout=alayout)
                f.sample_rate = arate
                f.pts = i
                out.mux(a.encode(f))
            out.mux(a.encode())
        if v is not None:
            out.mux(v.encode())


def mp3_with_cover(path):
    with av.open(str(path), "w", format="mp3") as out:
        a = out.add_stream("libmp3lame", rate=44_100, layout="stereo")
        pic = out.add_stream("mjpeg")
        pic.width = pic.height = 32
        pic.pix_fmt = "yuvj420p"
        pic.disposition = av.stream.Disposition.attached_pic
        img = av.VideoFrame.from_ndarray(np.full((32, 32, 3), 99, dtype=np.uint8), format="rgb24")
        img = img.reformat(format="yuvj420p")
        img.pts = 0
        for p in pic.encode(img):
            out.mux(p)
        for p in pic.encode():
            out.mux(p)
        for i in range(0, 44_100 * 2, 1152):
            x = tone(1152, 44_100, start=i)
            f = av.AudioFrame.from_ndarray(np.stack([x, x]), format="fltp", layout="stereo")
            f.sample_rate = 44_100
            f.pts = i
            out.mux(a.encode(f))
        out.mux(a.encode())


def main():
    work = Path(sys.argv[1])
    work.mkdir(parents=True, exist_ok=True)
    inputs = []

    def add(name, builder, *args, **kwargs):
        try:
            builder(work / name, *args, **kwargs)
            inputs.append(work / name)
        except Exception as exc:  # noqa: BLE001  (a missing encoder just skips the case)
            print(f"skip {name}: {exc!r}", file=sys.stderr)

    r = 16_000
    # Gap (0.5 s) and overlap (1200 samples) in a 16 kHz mono s16 stream (passthrough resampler); the muxer
    # needs monotonic timestamps, so the overlap starts after the previous frame's start.
    add("gap.mkv", pcm_with_timeline, r, [0, 1600, 3200, 3200 + 1600 + 8000, 16000, 16400, 18000])
    # A small drift under the tolerance, a bogus forward spike (> 60 s) and resync.
    add("spike.mkv", pcm_with_timeline, r, [0, 1600, 3200 + 700, 6400, 6400 + 1600 * 1000, 6400 + 1601600, 6400 + 1603200])
    # 22.05 kHz pcm (resampled) with a gap of 1234.5 samples at 16 kHz.
    add("gap22.mkv", pcm_with_timeline, 22_050, [0, 2205, 4410, 6615 + 1701, 8820 + 1701], 2205)
    # Audio that starts 700 s after the video (beyond the 10 min initial pad).
    add("late700.mkv", pcm_with_timeline, r, [700 * r, 700 * r + 1600], 1600, "pcm_s16le", True)
    # Audio that starts 3 s after the video (padded).
    add("late3.mkv", pcm_with_timeline, r, [3 * r, 3 * r + 1600, 3 * r + 3200], 1600, "pcm_s16le", True)
    add("pcm16k.wav", encoded_audio, "pcm_s16le", 16_000, 1, 2, "s16")
    add("pcm48k.wav", encoded_audio, "pcm_s16le", 48_000, 2, 2, "s16")
    add("flac.flac", encoded_audio, "flac", 48_000, 2, 2, "s16")
    add("opus.ogg", encoded_audio, "libopus", 48_000, 2, 2, "flt")
    add("mp2.ts", encoded_audio, "mp2", 48_000, 2, 2, "s16", "mpegts")
    add("ac3.ac3", encoded_audio, "ac3", 48_000, 6, 2, "fltp")
    add("aac.aac", encoded_audio, "aac", 44_100, 2, 2, "fltp", "adts")
    add("two.mkv", two_audio_streams)
    add("cover.mp3", mp3_with_cover)

    # Raw mp3 whose sample rate changes mid-stream: the resampler raises its ValueError.
    def rate_change(path):
        a, b = work / "_a.mp3", work / "_b.mp3"
        encoded_audio(a, "libmp3lame", 44_100, 1, 1, "fltp")
        encoded_audio(b, "libmp3lame", 22_050, 1, 1, "fltp")
        path.write_bytes(a.read_bytes() + b.read_bytes())

    add("ratechange.mp3", rate_change)

    def truncated(path):
        encoded_audio(work / "_t.mp4", "aac", 44_100, 2, 3, "fltp")
        raw = (work / "_t.mp4").read_bytes()
        path.write_bytes(raw[: len(raw) * 2 // 3])

    add("truncated.mp4", truncated)

    def text(path):
        path.write_text("not a video\n" * 50, encoding="utf-8")

    add("text.mp4", text)
    add("empty.mkv", lambda p: p.write_bytes(b""))
    (work / "dir.mp4").mkdir(exist_ok=True)
    inputs.append(work / "dir.mp4")
    inputs.append(work / "missing.mp4")

    previews = []

    def addp(name, builder, *args, **kwargs):
        try:
            builder(work / name, *args, **kwargs)
            previews.append(work / name)
        except Exception as exc:  # noqa: BLE001
            print(f"skip {name}: {exc!r}", file=sys.stderr)

    addp("v_only.mp4", av_source, "libx264", 64, 48, "yuv420p", options={"bf": "0"})
    addp("big.mkv", av_source, "mpeg4", 2000, 1000, "yuv420p", "pcm_s16le", 44_100, "mono", frames=5)
    addp("odd.mkv", av_source, "mpeg4", 66, 46, "yuv420p", "flac", 44_100, "stereo")
    addp("yuv444.mkv", av_source, "libx264", 64, 64, "yuv444p", "aac", 48_000, "stereo", {"bf": "0"})
    addp("opus.mkv", av_source, "libx264", 64, 64, "yuv420p", "libopus", 48_000, "stereo", {"bf": "0"})
    addp("aac51.mkv", av_source, None, 0, 0, None, "aac", 48_000, "5.1")
    addp("a_only.ogg", av_source, None, 0, 0, None, "libopus", 48_000, "mono", container_format="ogg")
    previews.append(work / "cover.mp3")
    addp("tone_b.mp4", av_source, "libx264", 64, 64, "yuv420p", "aac", 44_100, "stereo")
    previews.append(work / "text.mp4")
    previews.append(work / "empty.mkv")
    previews.append(work / "missing.mkv")
    previews.append(work / "dir.mp4")

    manifest = {"extract": [], "preview": []}
    for video in inputs:
        progress = []
        wav = work / (video.name + ".py.wav")
        try:
            extract_audio(video, wav, on_progress=progress.append)
            error = None
        except Exception as exc:  # noqa: BLE001
            error = str(exc)
        manifest["extract"].append({"video": str(video), "wav": str(wav), "progress": progress, "error": error})
    for source in previews:
        out = work / (source.name + ".py.mp4")
        try:
            make_preview(source, out)
            error = None
        except Exception as exc:  # noqa: BLE001
            error = str(exc)
        manifest["preview"].append({"source": str(source), "output": str(out), "error": error})
    (work / "manifest.json").write_text(json.dumps(manifest, indent=1), encoding="utf-8")


if __name__ == "__main__":
    main()
