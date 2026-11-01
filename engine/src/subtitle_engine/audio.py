"""Step 1: extracts audio from a video -> 16 kHz mono 16-bit WAV (the format Whisper expects).

PyAV ships the ffmpeg libraries inside its wheel; no system ffmpeg is needed.
"""
import contextlib
import wave
from pathlib import Path

import av

from .i18n import t

SAMPLE_RATE = 16_000
GAP_TOLERANCE = SAMPLE_RATE // 20  # drifts under 50 ms are not padded
MAX_MIDSTREAM_GAP_SAMPLES = 60 * SAMPLE_RATE  # ignore bogus PTS spikes > 60s in damaged AVI/WMV files
MAX_INITIAL_DELAY_SAMPLES = 600 * SAMPLE_RATE  # allow up to 10 min initial audio delay
MAX_CONSECUTIVE_DEMUX_ERRORS = 512


def _write_silence(wav, samples: int) -> None:
    block = 16_000
    while samples > 0:
        n = min(block, samples)
        wav.writeframes(bytes(n * 2))
        samples -= n


class AudioExtractionError(RuntimeError):
    pass


def _iter_audio_frames(container, stream):
    """Demuxes and decodes audio frames while skipping corrupt packets (common in long WMV/ASF/AVI/MKV files).

    Raises the underlying `av.FFmpegError` only if no valid audio frames could be decoded at all.
    """
    decoded_any = False
    first_error: av.FFmpegError | None = None
    demux_errors = 0
    decode_errors = 0
    demuxer = iter(container.demux(stream))
    while True:
        try:
            packet = next(demuxer)
        except StopIteration:
            break
        except av.FFmpegError as exc:
            if first_error is None:
                first_error = exc
            demux_errors += 1
            if demux_errors >= MAX_CONSECUTIVE_DEMUX_ERRORS:
                break
            continue

        demux_errors = 0
        # Skip mid-stream corrupt or empty non-flush packets in ASF/AVI containers.
        if getattr(packet, "is_corrupt", False):
            continue

        try:
            frames = packet.decode()
        except av.FFmpegError as exc:
            if first_error is None:
                first_error = exc
            decode_errors += 1
            if decode_errors >= 2:
                with contextlib.suppress(Exception):
                    stream.codec_context.flush_buffers()
            continue

        decode_errors = 0
        for frame in frames:
            decoded_any = True
            yield frame

    if not decoded_any and first_error is not None:
        raise first_error


def _extract_stream_to_wav(container, stream, out_wav: Path) -> int:
    """Decodes `stream` into `out_wav` and returns the number of decoded frames."""
    raw_start = container.start_time
    origin = (raw_start / av.time_base) if (raw_start is not None and abs(raw_start) < 10**15) else 0.0
    cursor = 0.0  # input time covered so far, in exact 16 kHz samples (float avoids per-frame rounding drift)
    skip_samples = 0
    frame_count = 0
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
                max_pad = MAX_INITIAL_DELAY_SAMPLES if frame_count == 0 else MAX_MIDSTREAM_GAP_SAMPLES
                if GAP_TOLERANCE < diff <= max_pad:
                    pad = round(diff)
                    _write_silence(wav, pad)
                    cursor += pad
                elif GAP_TOLERANCE < -diff <= MAX_MIDSTREAM_GAP_SAMPLES:
                    drop = round(-diff)
                    skip_samples += drop
                    cursor -= drop
                elif abs(diff) > max_pad and frame_count > 0:
                    # Bogus timestamp spike in a damaged AVI/WMV container: resync cursor to avoid
                    # writing gigabytes of silence or dropping the remainder of a multi-hour video.
                    cursor = target
            frame_count += 1
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
    return frame_count


def extract_audio(video: Path, out_wav: Path) -> Path:
    if not video.is_file():
        raise AudioExtractionError(t("video_not_found", video=video))
    out_wav.parent.mkdir(parents=True, exist_ok=True)
    try:
        with av.open(str(video)) as container:
            if not container.streams.audio:
                raise AudioExtractionError(t("no_audio_track"))
            best = container.streams.best("audio") or container.streams.audio[0]
            candidates = [best, *[s for s in container.streams.audio if s != best]]
            last_err: av.FFmpegError | None = None
            for idx, stream in enumerate(candidates):
                try:
                    if idx > 0:
                        container.seek(0)
                    count = _extract_stream_to_wav(container, stream, out_wav)
                    if count > 0 or idx == len(candidates) - 1:
                        break
                except av.FFmpegError as exc:
                    last_err = exc
                    if idx == len(candidates) - 1:
                        raise
            else:
                if last_err is not None:
                    raise last_err
    except av.FFmpegError as exc:
        raise AudioExtractionError(t("audio_read_failed", error=exc)) from exc
    return out_wav
