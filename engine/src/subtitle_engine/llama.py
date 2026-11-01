"""Local llama.cpp runtime management for GGUF translation models (Gemma 4, Qwen 3.5).

Downloads and caches the official prebuilt `llama-server` binary (with Vulkan GPU acceleration on
Windows/Linux and Metal GPU acceleration on macOS) into the user cache on first use:
- Windows: `%LOCALAPPDATA%\\SubtitleAdder\\llama`
- macOS / Linux: `~/.cache/subtitle-adder/llama` (or `$XDG_CACHE_HOME/subtitle-adder/llama`)
Can be overridden via `SUBTITLE_ENGINE_LLAMA_DIR`.
"""
import hashlib
import os
import shutil
import stat
import sys
import tarfile
import time
import urllib.request
import zipfile
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path

from .i18n import t
from .models import ModelError

LLAMA_DIR_ENV_VAR = "SUBTITLE_ENGINE_LLAMA_DIR"
RELEASE_TAG = "b12032"
_PROGRESS_INTERVAL = 0.1
_CHUNK_SIZE = 256 * 1024


@dataclass(frozen=True)
class _LlamaArchive:
    filename: str
    url: str
    sha256: str
    size: int


_LLAMA_ARCHIVES: dict[str, _LlamaArchive] = {
    "win32": _LlamaArchive(
        filename=f"llama-{RELEASE_TAG}-bin-win-vulkan-x64.zip",
        url=f"https://github.com/ggml-org/llama.cpp/releases/download/{RELEASE_TAG}/llama-{RELEASE_TAG}-bin-win-vulkan-x64.zip",
        sha256="19a8bbf269fa683ae7bc04f886c9c2a93e0655c2606f37d2b60069bf9e8bcc32",
        size=37_523_450,
    ),
    "darwin": _LlamaArchive(
        filename=f"llama-{RELEASE_TAG}-bin-macos-arm64.tar.gz",
        url=f"https://github.com/ggml-org/llama.cpp/releases/download/{RELEASE_TAG}/llama-{RELEASE_TAG}-bin-macos-arm64.tar.gz",
        sha256="00468fe833eb000b78a748122f5921b6c7b07dc82331a8d0c68196c69cb26382",
        size=16_513_842,
    ),
    "linux": _LlamaArchive(
        filename=f"llama-{RELEASE_TAG}-bin-ubuntu-vulkan-x64.tar.gz",
        url=f"https://github.com/ggml-org/llama.cpp/releases/download/{RELEASE_TAG}/llama-{RELEASE_TAG}-bin-ubuntu-vulkan-x64.tar.gz",
        sha256="f75e7107f9450034c5467492c41e9ed91816e2d815fc4b802c6bd58de7d5b126",
        size=35_669_614,
    ),
}


def llama_cache_dir() -> Path:
    """User-writable cache directory where the llama.cpp runtime is stored."""
    if override := os.environ.get(LLAMA_DIR_ENV_VAR, "").strip():
        return Path(override)
    if sys.platform == "win32":
        base = Path(os.environ.get("LOCALAPPDATA") or (Path.home() / "AppData" / "Local"))
        return base / "SubtitleAdder" / "llama"
    base = Path(os.environ.get("XDG_CACHE_HOME") or (Path.home() / ".cache"))
    return base / "subtitle-adder" / "llama"


def _server_binary_name() -> str:
    return "llama-server.exe" if sys.platform == "win32" else "llama-server"


def llama_server_path() -> Path | None:
    """Returns the path to the cached `llama-server` executable if installed, else None."""
    root = llama_cache_dir()
    if not root.is_dir():
        return None
    exe = _server_binary_name()
    candidates = (
        root / exe,
        root / "bin" / exe,
        root / f"llama-{RELEASE_TAG}" / exe,
        root / f"llama-{RELEASE_TAG}" / "bin" / exe,
        root / "build" / "bin" / exe,
    )
    for candidate in candidates:
        if candidate.is_file():
            return candidate
    return None


def llama_usable() -> bool:
    """Whether `llama-server` is already present in the local cache."""
    return llama_server_path() is not None


def require_llama_download() -> bool:
    """Like `needs_llama_download`, but raises a localized error if missing in strict offline mode."""
    from . import models

    if llama_usable():
        return False
    if models.offline_requested():
        raise ValueError(t("offline_missing_llama"))
    return sys.platform in _LLAMA_ARCHIVES


def needs_llama_download() -> bool:
    """True when `llama-server` is not yet cached and offline mode is not active."""
    from . import models

    if models.offline_requested():
        return False
    return sys.platform in _LLAMA_ARCHIVES and not llama_usable()


def _extract_archive(archive_path: Path, cache_root: Path) -> None:
    if zipfile.is_zipfile(archive_path):
        with zipfile.ZipFile(archive_path) as zf:
            zf.extractall(cache_root)
    else:
        with tarfile.open(archive_path, "r:gz") as tf:
            tf.extractall(cache_root, filter="data")

    if sys.platform != "win32":
        server = llama_server_path()
        if server is not None:
            mode = server.stat().st_mode
            server.chmod(mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)


def ensure_llama(on_progress: Callable[[float], None] | None = None) -> Path:
    """Downloads and extracts `llama-server` into `llama_cache_dir()` if not already present."""
    existing = llama_server_path()
    if existing is not None:
        return existing

    from . import models

    if models.offline_requested():
        raise ModelError(t("offline_missing_llama"))

    archive = _LLAMA_ARCHIVES.get(sys.platform)
    if archive is None:
        raise ModelError(t("llama_unavailable", error=f"unsupported platform {sys.platform}"))

    cache_root = llama_cache_dir()
    total_bytes = archive.size
    downloaded_total = 0
    last_fraction = 0.0
    last_report = 0.0

    def report(current_bytes: int) -> None:
        nonlocal last_fraction, last_report
        if on_progress is None or total_bytes <= 0:
            return
        fraction = min(max(current_bytes / total_bytes, 0.0), 0.99)
        now = time.monotonic()
        if fraction > last_fraction and now - last_report >= _PROGRESS_INTERVAL:
            last_fraction = fraction
            last_report = now
            on_progress(fraction)

    tmp_archive = cache_root / f".{archive.filename}.tmp"
    try:
        cache_root.mkdir(parents=True, exist_ok=True)
        digest = hashlib.sha256()
        with urllib.request.urlopen(archive.url, timeout=60) as resp, open(tmp_archive, "wb") as out:
            while chunk := resp.read(_CHUNK_SIZE):
                out.write(chunk)
                digest.update(chunk)
                downloaded_total += len(chunk)
                report(downloaded_total)
        if digest.hexdigest().lower() != archive.sha256.lower():
            raise OSError(f"SHA-256 mismatch for {archive.filename}")
        _extract_archive(tmp_archive, cache_root)
    except Exception as exc:
        raise ModelError(t("llama_unavailable", error=str(exc))) from exc
    finally:
        tmp_archive.unlink(missing_ok=True)

    server = llama_server_path()
    if server is None:
        raise ModelError(t("llama_unavailable", error="llama-server binary not found in archive"))
    if on_progress is not None:
        on_progress(1.0)
    return server
