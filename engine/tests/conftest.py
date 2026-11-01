import math
import os
import wave
from pathlib import Path

import av
import numpy as np
import pytest

from subtitle_engine import cuda, i18n
from subtitle_engine.models import Word


@pytest.fixture(autouse=True)
def _default_language(monkeypatch):
    """Tests do not depend on the developer's SUBTITLE_ENGINE_LANG or on a language pinned by an earlier test."""
    monkeypatch.delenv(i18n.ENV_VAR, raising=False)
    i18n.set_language(None)
    yield
    i18n.set_language(None)


@pytest.fixture(autouse=True)
def _no_implicit_cuda_download(monkeypatch):
    """Unit tests must not trigger a real CUDA wheel download unless testing `cuda` explicitly."""
    monkeypatch.setattr(cuda, "needs_cuda_download", lambda: False)


def make_words(text: str, step: float = 0.4) -> list[Word]:
    return [Word(start=i * step, end=i * step + step * 0.8, text=w) for i, w in enumerate(text.split())]


@pytest.fixture
def tone_video(tmp_path: Path) -> Path:
    """Generates a small mp4 with 2 s of 44.1 kHz stereo sine audio using PyAV."""
    path = tmp_path / "tone.mp4"
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
        chunk = 1024
        for i in range(0, len(samples), chunk):
            block = np.stack([samples[i : i + chunk]] * 2)
            frame = av.AudioFrame.from_ndarray(block, format="fltp", layout="stereo")
            frame.sample_rate = rate
            frame.pts = i
            out.mux(astream.encode(frame))
        out.mux(vstream.encode())
        out.mux(astream.encode())
    return path


def read_wav(path: Path) -> tuple[int, int, float]:
    with wave.open(str(path)) as w:
        return w.getframerate(), w.getnchannels(), w.getnframes() / w.getframerate()


def symlinks_supported(directory: Path) -> bool:
    """Can symlinks be created in `directory`? Windows without Developer Mode / admin cannot."""
    probe = directory / "_symlink_probe"
    try:
        probe.symlink_to("target")
    except (OSError, NotImplementedError):
        return False
    probe.unlink()
    return True


def require_symlinks(directory: Path) -> None:
    """Skips the calling test when symlinks cannot be created (never weakens it into a copy)."""
    directory.mkdir(parents=True, exist_ok=True)
    if not symlinks_supported(directory):
        pytest.skip("symlinks are not available on this machine (Windows without Developer Mode)")


@pytest.fixture(autouse=True)
def _restore_hub_offline_state(monkeypatch):
    """`models.go_offline()` flips process-wide Hub flags; keep every test independent of the ones before it."""
    import huggingface_hub.constants as hub_constants

    for name in ("HF_HUB_OFFLINE", "HF_HUB_DISABLE_TELEMETRY", "SUBTITLE_ENGINE_OFFLINE"):
        monkeypatch.delenv(name, raising=False)
    monkeypatch.setattr(hub_constants, "HF_HUB_OFFLINE", False)
    monkeypatch.setattr(hub_constants, "HF_HUB_DISABLE_TELEMETRY", hub_constants.HF_HUB_DISABLE_TELEMETRY)
