"""Adım 1: videodan sesi ayırır -> 16 kHz mono 16-bit WAV (Whisper'ın beklediği biçim).

PyAV, ffmpeg kütüphanelerini wheel içinde getirir; sistemde ffmpeg kurulu olması gerekmez.
"""
import wave
from pathlib import Path

import av

SAMPLE_RATE = 16_000


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
            stream = container.streams.audio[0]
            resampler = av.AudioResampler(format="s16", layout="mono", rate=SAMPLE_RATE)
            with wave.open(str(out_wav), "wb") as wav:
                wav.setnchannels(1)
                wav.setsampwidth(2)
                wav.setframerate(SAMPLE_RATE)
                for frame in container.decode(stream):
                    for chunk in resampler.resample(frame):
                        wav.writeframes(chunk.to_ndarray().tobytes())
                for chunk in resampler.resample(None):
                    wav.writeframes(chunk.to_ndarray().tobytes())
    except av.FFmpegError as exc:
        raise AudioExtractionError(f"Ses okunamadı: {exc}") from exc
    return out_wav
