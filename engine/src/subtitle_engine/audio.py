"""Step 1: extracts audio from a video -> 16 kHz mono 16-bit WAV (the format Whisper expects).

PyAV ships the ffmpeg libraries inside its wheel; no system ffmpeg is needed.
"""
import wave
from pathlib import Path

import av

SAMPLE_RATE = 16_000
GAP_TOLERANCE = SAMPLE_RATE // 20  # drifts under 50 ms are not padded


def _write_silence(wav, samples: int) -> None:
    block = 16_000
    while samples > 0:
        n = min(block, samples)
        wav.writeframes(bytes(n * 2))
        samples -= n


class AudioExtractionError(RuntimeError):
    pass


def extract_audio(video: Path, out_wav: Path) -> Path:
    if not video.is_file():
        raise AudioExtractionError(f"Video bulunamadı: {video}")
    out_wav.parent.mkdir(parents=True, exist_ok=True)
    try:
        with av.open(str(video)) as container:
            if not container.streams.audio:
                raise AudioExtractionError("Videoda ses izi yok.")
            stream = container.streams.best("audio") or container.streams.audio[0]
            origin = (container.start_time or 0) / av.time_base
            cursor = 0  # input time covered so far, in 16 kHz samples
            resampler = av.AudioResampler(format="s16", layout="mono", rate=SAMPLE_RATE)
            with wave.open(str(out_wav), "wb") as wav:
                wav.setnchannels(1)
                wav.setsampwidth(2)
                wav.setframerate(SAMPLE_RATE)
                for frame in container.decode(stream):
                    # If the audio stream starts late or has gaps, pad with silence so the WAV
                    # timeline stays aligned with the container timeline (no subtitle drift).
                    if frame.time is not None:
                        target = round((frame.time - origin) * SAMPLE_RATE)
                        if target - cursor > GAP_TOLERANCE:
                            _write_silence(wav, target - cursor)
                            cursor = target
                    cursor += round(frame.samples * SAMPLE_RATE / frame.sample_rate)
                    for chunk in resampler.resample(frame):
                        wav.writeframes(chunk.to_ndarray().tobytes())
                for chunk in resampler.resample(None):
                    wav.writeframes(chunk.to_ndarray().tobytes())
    except av.FFmpegError as exc:
        raise AudioExtractionError(f"Ses okunamadı: {exc}") from exc
    return out_wav
