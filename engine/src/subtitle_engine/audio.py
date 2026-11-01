"""Step 1: extracts audio from a video -> 16 kHz mono 16-bit WAV (the format Whisper expects).

PyAV ships the ffmpeg libraries inside its wheel; no system ffmpeg is needed.
"""
import wave
from pathlib import Path

import av

from .i18n import t

SAMPLE_RATE = 16_000
GAP_TOLERANCE = SAMPLE_RATE // 20  # drifts under 50 ms are not padded
MAX_CONSECUTIVE_PACKET_ERRORS = 64


def _write_silence(wav, samples: int) -> None:
    block = 16_000
    while samples > 0:
        n = min(block, samples)
        wav.writeframes(bytes(n * 2))
        samples -= n


class AudioExtractionError(RuntimeError):
    pass


def _iter_audio_frames(container, stream):
    """Demuxes and decodes audio frames while skipping isolated corrupt packets (common in long WMV/ASF/MKV files).

    Raises the underlying `av.FFmpegError` only if no valid audio frames could be decoded at all.
    """
    decoded_any = False
    first_error: av.FFmpegError | None = None
    consecutive_errors = 0
    demuxer = iter(container.demux(stream))
    while True:
        try:
            packet = next(demuxer)
        except StopIteration:
            break
        except av.FFmpegError as exc:
            if first_error is None:
                first_error = exc
            consecutive_errors += 1
            if consecutive_errors >= MAX_CONSECUTIVE_PACKET_ERRORS:
                break
            continue

        try:
            frames = packet.decode()
        except av.FFmpegError as exc:
            if first_error is None:
                first_error = exc
            consecutive_errors += 1
            continue

        consecutive_errors = 0
        for frame in frames:
            decoded_any = True
            yield frame

    if not decoded_any and first_error is not None:
        raise first_error


def extract_audio(video: Path, out_wav: Path) -> Path:
    if not video.is_file():
        raise AudioExtractionError(t("video_not_found", video=video))
    out_wav.parent.mkdir(parents=True, exist_ok=True)
    try:
        with av.open(str(video)) as container:
            if not container.streams.audio:
                raise AudioExtractionError(t("no_audio_track"))
            stream = container.streams.best("audio") or container.streams.audio[0]
            origin = (container.start_time or 0) / av.time_base
            cursor = 0.0  # input time covered so far, in exact 16 kHz samples (float avoids per-frame rounding drift)
            skip_samples = 0
            resampler = av.AudioResampler(format="s16", layout="mono", rate=SAMPLE_RATE)
            with wave.open(str(out_wav), "wb") as wav:
                wav.setnchannels(1)
                wav.setsampwidth(2)
                wav.setframerate(SAMPLE_RATE)
                for frame in _iter_audio_frames(container, stream):
                    # If the audio stream starts late, has gaps, or overlaps, pad or trim so the WAV
                    # timeline stays aligned with the container timeline (no subtitle drift).
                    if frame.time is not None:
                        target = (frame.time - origin) * SAMPLE_RATE
                        diff = target - cursor
                        if diff > GAP_TOLERANCE:
                            pad = round(diff)
                            _write_silence(wav, pad)
                            cursor += pad
                        elif -diff > GAP_TOLERANCE:
                            drop = round(-diff)
                            skip_samples += drop
                            cursor -= drop
                    cursor += frame.samples * SAMPLE_RATE / frame.sample_rate
                    for chunk in resampler.resample(frame):
                        pcm = chunk.to_ndarray().tobytes()
                        if skip_samples > 0:
                            drop_bytes = min(len(pcm), skip_samples * 2)
                            pcm = pcm[drop_bytes:]
                            skip_samples -= drop_bytes // 2
                        if pcm:
                            wav.writeframes(pcm)
                for chunk in resampler.resample(None):
                    pcm = chunk.to_ndarray().tobytes()
                    if skip_samples > 0:
                        drop_bytes = min(len(pcm), skip_samples * 2)
                        pcm = pcm[drop_bytes:]
                        skip_samples -= drop_bytes // 2
                    if pcm:
                        wav.writeframes(pcm)
    except av.FFmpegError as exc:
        raise AudioExtractionError(t("audio_read_failed", error=exc)) from exc
    return out_wav
