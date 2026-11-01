"""Tests for automatic CUDA runtime discovery and wheel extraction."""
import hashlib
import io
import zipfile
from pathlib import Path

from subtitle_engine import cuda, models, translate
from subtitle_engine.models import Sentence


def _make_fake_wheel(subdir: str, lib_folder: str, filename: str, payload: bytes = b"dll-bytes") -> tuple[bytes, str]:
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w") as zf:
        zf.writestr(f"nvidia/{subdir}/{lib_folder}/{filename}", payload)
        zf.writestr(f"nvidia/{subdir}/include/header.h", b"// ignored")
    raw = buf.getvalue()
    return raw, hashlib.sha256(raw).hexdigest()


def test_cuda_cache_dir_respects_env_override(monkeypatch, tmp_path):
    custom = tmp_path / "custom-cuda"
    monkeypatch.setenv(cuda.CUDA_DIR_ENV_VAR, str(custom))
    assert cuda.cuda_cache_dir() == custom


def test_ensure_cuda_downloads_and_extracts_wheel_binaries(monkeypatch, tmp_path):
    cache_dir = tmp_path / "cuda-cache"
    monkeypatch.setenv(cuda.CUDA_DIR_ENV_VAR, str(cache_dir))
    monkeypatch.setattr(cuda, "sys", type("FakeSys", (), {"platform": "win32", "path": []})())

    raw_whl, sha = _make_fake_wheel("cublas", "bin", "cublas64_12.dll")
    fake_wheel = cuda._CudaWheel(subdir="cublas", url="https://example.invalid/cublas.whl", sha256=sha, size=len(raw_whl))
    monkeypatch.setitem(cuda._CUDA_WHEELS, "win32", (fake_wheel,))
    monkeypatch.setattr(cuda, "needs_cuda_download", lambda: True)
    monkeypatch.setattr(cuda, "register_libraries", type("FakeCacheFn", (), {"__call__": lambda self: None, "cache_clear": lambda self: None})())
    monkeypatch.setattr(cuda, "cuda_usable", type("FakeUsableFn", (), {"__call__": lambda self: True, "cache_clear": lambda self: None})())
    monkeypatch.setattr(cuda.urllib.request, "urlopen", lambda url, timeout=60: io.BytesIO(raw_whl))

    progress: list[float] = []
    assert cuda.ensure_cuda(on_progress=progress.append) is True
    assert (cache_dir / "nvidia" / "cublas" / "bin" / "cublas64_12.dll").read_bytes() == b"dll-bytes"
    assert not (cache_dir / "nvidia" / "cublas" / "include" / "header.h").exists()
    assert progress[-1] == 1.0


def test_ensure_cuda_returns_false_on_network_error(monkeypatch, tmp_path):
    monkeypatch.setenv(cuda.CUDA_DIR_ENV_VAR, str(tmp_path / "cuda-cache"))
    monkeypatch.setattr(cuda, "needs_cuda_download", lambda: True)

    def fail_open(url, timeout=60):
        raise OSError("network unreachable")

    monkeypatch.setattr(cuda.urllib.request, "urlopen", fail_open)
    assert cuda.ensure_cuda() is False


def test_needs_cuda_download_is_false_in_strict_offline_mode(monkeypatch):
    monkeypatch.setenv(models.OFFLINE_ENV_VAR, "1")
    monkeypatch.setattr(cuda, "has_nvidia_gpu", lambda: True)
    monkeypatch.setattr(cuda, "cuda_usable", lambda: False)
    assert cuda.needs_cuda_download() is False


def test_translator_falls_back_to_cpu_on_cuda_library_error(monkeypatch):
    loads: list[str] = []

    class BrokenThenWorkingCt2:
        def __init__(self, device: str):
            self.device = device

        def translate_batch(self, source, **kwargs):
            if self.device == "cuda":
                raise RuntimeError("Library cublas64_12.dll is not found")
            from types import SimpleNamespace
            return [SimpleNamespace(hypotheses=[["hola"]]) for _ in source]

    monkeypatch.setattr(cuda, "cuda_usable", lambda: True)
    monkeypatch.setattr(models, "model_dir", lambda mid: Path("/cache") / mid)
    monkeypatch.setattr(
        translate,
        "_load_tokenizer",
        lambda directory, model_id: type("Tok", (), {"encode": lambda self, t: t.split(), "decode": lambda self, t: " ".join(t)})(),
    )
    monkeypatch.setattr(
        translate,
        "_load_ct2_translator",
        lambda path, device, compute_type: (loads.append(device), BrokenThenWorkingCt2(device))[1],
    )

    tr = translate.Ct2Translator("madlad-3b")
    out = tr.translate([Sentence(id=1, start=0.0, end=1.0, text="Merhaba.")], "es")

    assert loads == ["cuda", "cpu"]
    assert out[0].text == "hola"
