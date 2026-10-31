"""Adım 1: videodan sesi ayırır -> 16 kHz mono 16-bit WAV (Whisper'ın beklediği biçim).

PyAV, ffmpeg kütüphanelerini wheel içinde getirir; sistemde ffmpeg kurulu olması gerekmez.
"""
import wave
from pathlib import Path

import av

SAMPLE_RATE = 16_000
GAP_TOLERANCE = SAMPLE_RATE // 20  # 50 ms'den küçük sapmalar doldurulmaz


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
            cursor = 0  # girişte şu ana kadar kapsanan süre, 16 kHz örnek cinsinden
            resampler = av.AudioResampler(format="s16", layout="mono", rate=SAMPLE_RATE)
            with wave.open(str(out_wav), "wb") as wav:
                wav.setnchannels(1)
                wav.setsampwidth(2)
                wav.setframerate(SAMPLE_RATE)
                for frame in container.decode(stream):
                    # Ses akışı geç başlıyorsa/boşluk varsa sessizlikle doldur: WAV zamanı
                    # kapsayıcı zaman çizelgesiyle hizalı kalsın (altyazı kayması olmasın).
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
