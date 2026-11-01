"""NVIDIA GPU support for CTranslate2 (faster-whisper and translation).

CTranslate2 loads cuBLAS/cuDNN lazily: a machine with an NVIDIA GPU but without the CUDA 12 runtime
libraries reports a CUDA device and then fails in the middle of transcription ("cublas64_12.dll is not
found"). In development the `cuda` dependency group installs those libraries as NVIDIA's pip wheels; in a
packaged build `ensure_cuda()` downloads and extracts the same wheel binaries into the user cache on first
use (`%LOCALAPPDATA%\\SubtitleAdder\\cuda` on Windows, `~/.cache/subtitle-adder/cuda` on Linux).
"""
import ctypes
import hashlib
import os
import shutil
import sys
import sysconfig
import time
import urllib.request
import zipfile
from collections.abc import Callable
from dataclasses import dataclass
from functools import cache
from pathlib import Path

_WINDOWS_LIBS = ("cublas64_12.dll", "cublasLt64_12.dll", "cudnn64_9.dll")
_LINUX_LIBS = ("libcublasLt.so.12", "libcublas.so.12", "libcudnn.so.9")

CUDA_DIR_ENV_VAR = "SUBTITLE_ENGINE_CUDA_DIR"
_PROGRESS_INTERVAL = 0.1
_CHUNK_SIZE = 256 * 1024


@dataclass(frozen=True)
class _CudaWheel:
    subdir: str  # folder under nvidia/ that marks this wheel as extracted (e.g. "cublas", "cuda_nvrtc", "cudnn")
    url: str
    sha256: str
    size: int


# Pinned to the exact wheel URLs and SHA-256 digests in engine/uv.lock.
_CUDA_WHEELS: dict[str, tuple[_CudaWheel, ...]] = {
    "win32": (
        _CudaWheel(
            subdir="cublas",
            url="https://files.pythonhosted.org/packages/20/e2/fc9a0e985249d873150276d5afb02e39a66817fedbf1a385724393e505ed/nvidia_cublas_cu12-12.9.2.10-py3-none-win_amd64.whl",
            sha256="623f43027d40d44ceadf0043f002bd25cf353e8f13ce90b9a87057019f560661",
            size=553_162_896,
        ),
        _CudaWheel(
            subdir="cuda_nvrtc",
            url="https://files.pythonhosted.org/packages/52/de/823919be3b9d0ccbf1f784035423c5f18f4267fb0123558d58b813c6ec86/nvidia_cuda_nvrtc_cu12-12.9.86-py3-none-win_amd64.whl",
            sha256="72972ebdcf504d69462d3bcd67e7b81edd25d0fb85a2c46d3ea351766636349",
            size=76_408_187,
        ),
        _CudaWheel(
            subdir="cudnn",
            url="https://files.pythonhosted.org/packages/aa/38/f856579877f7c1c5066e61182e7de7bc27bf35a78c8d1b0fa592e6985bc4/nvidia_cudnn_cu12-9.27.0.42-py3-none-win_amd64.whl",
            sha256="06e9b0026f3bad97d2b58666330fabec04fe1672f776661ecb0ce0029c27f142",
            size=743_068_852,
        ),
    ),
    "linux": (
        _CudaWheel(
            subdir="cublas",
            url="https://files.pythonhosted.org/packages/7b/ea/9b6c36aef90766eebb4763aded157039e797ae9789c9c216e960d1d3cd47/nvidia_cublas_cu12-12.9.3.13-py3-none-manylinux_2_27_x86_64.whl",
            sha256="d7d51055f161b317c7105ddaae90e71eda059026074ffef93cf7cde2886bfe72",
            size=581_217_458,
        ),
        _CudaWheel(
            subdir="cuda_nvrtc",
            url="https://files.pythonhosted.org/packages/b8/85/e4af82cc9202023862090bfca4ea827d533329e925c758f0cde964cb54b7/nvidia_cuda_nvrtc_cu12-12.9.86-py3-none-manylinux2010_x86_64.manylinux_2_12_x86_64.whl",
            sha256="210cf05005a447e29214e9ce50851e83fc5f4358df8b453155d5e1918094dcb4",
            size=89_568_129,
        ),
        _CudaWheel(
            subdir="cudnn",
            url="https://files.pythonhosted.org/packages/65/e4/c5a205d48ff00ed8b27882bb45d338d8138e976c76385f328a326bbfaeda/nvidia_cudnn_cu12-9.27.0.42-py3-none-manylinux_2_27_x86_64.whl",
            sha256="0a4aa3a7d2264256506c6857fc41fc0c499982f78d70195b1cfcc9055c1957cd",
            size=766_178_381,
        ),
    ),
}


def cuda_cache_dir() -> Path:
    """User-writable cache directory where downloaded CUDA runtime libraries are stored."""
    if override := os.environ.get(CUDA_DIR_ENV_VAR, "").strip():
        return Path(override)
    if sys.platform == "win32":
        base = Path(os.environ.get("LOCALAPPDATA") or (Path.home() / "AppData" / "Local"))
        return base / "SubtitleAdder" / "cuda"
    base = Path(os.environ.get("XDG_CACHE_HOME") or (Path.home() / ".cache"))
    return base / "subtitle-adder" / "cuda"


def _nvidia_dirs() -> list[Path]:
    """Library folders of the installed or cached nvidia-* wheels."""
    sub = "bin" if sys.platform == "win32" else "lib"
    roots: list[Path] = []
    if meipass := getattr(sys, "_MEIPASS", None):
        roots.append(Path(meipass))
    roots.extend([Path(sysconfig.get_paths()["purelib"]), Path(sysconfig.get_paths()["platlib"])])
    roots.extend(Path(p) for p in sys.path if p.endswith("site-packages"))
    roots.append(cuda_cache_dir())

    dirs: list[Path] = []
    seen: set[Path] = set()
    for root in roots:
        nvidia = root / "nvidia"
        if nvidia.is_dir():
            for d in sorted(nvidia.iterdir()):
                lib_dir = d / sub
                if lib_dir.is_dir() and lib_dir not in seen:
                    seen.add(lib_dir)
                    dirs.append(lib_dir)
    return dirs


@cache
def register_libraries() -> None:
    """Makes the wheel-installed CUDA libraries findable by CTranslate2 (once per process)."""
    dirs = _nvidia_dirs()
    if not dirs:
        return
    if sys.platform == "win32":
        # CTranslate2 loads the DLLs with the default search order, which includes PATH.
        os.environ["PATH"] = os.pathsep.join([*map(str, dirs), os.environ.get("PATH", "")])
        for d in dirs:
            try:
                os.add_dll_directory(str(d))
            except OSError:
                pass
    elif sys.platform == "linux":
        # LD_LIBRARY_PATH is read at process start only; preloading by full path makes later dlopen calls find them.
        for name in _LINUX_LIBS:
            for d in dirs:
                if (d / name).exists():
                    try:
                        ctypes.CDLL(str(d / name), mode=ctypes.RTLD_GLOBAL)
                    except OSError:
                        pass
                    break


def has_nvidia_gpu() -> bool:
    """Whether an NVIDIA GPU is visible to CTranslate2 on this machine."""
    if sys.platform == "darwin":
        return False
    import ctranslate2

    try:
        return ctranslate2.get_cuda_device_count() > 0
    except RuntimeError:
        return False


@cache
def cuda_usable() -> bool:
    """Whether faster-whisper can run on the GPU: a CUDA device exists and cuBLAS/cuDNN load."""
    if not has_nvidia_gpu():
        return False
    register_libraries()
    names = _WINDOWS_LIBS if sys.platform == "win32" else _LINUX_LIBS
    for name in names:
        try:
            if sys.platform == "win32":
                ctypes.WinDLL(name, winmode=0)  # winmode=0: standard search order including PATH
            else:
                ctypes.CDLL(name)
        except OSError:
            return False
    return True


def needs_cuda_download() -> bool:
    """True when an NVIDIA GPU is present on Windows/Linux but the CUDA runtime libraries are missing."""
    return sys.platform in _CUDA_WHEELS and has_nvidia_gpu() and not cuda_usable()


def _wheel_installed(cache_root: Path, wheel: _CudaWheel) -> bool:
    sub = "bin" if sys.platform == "win32" else "lib"
    lib_dir = cache_root / "nvidia" / wheel.subdir / sub
    return lib_dir.is_dir() and any(lib_dir.iterdir())


def _extract_wheel_libs(whl_path: Path, cache_root: Path) -> None:
    sub = "bin" if sys.platform == "win32" else "lib"
    with zipfile.ZipFile(whl_path) as zf:
        for member in zf.infolist():
            if member.is_dir():
                continue
            parts = Path(member.filename).parts
            # Match nvidia/<pkg>/bin/*.dll on Windows or nvidia/<pkg>/lib/*.so* on Linux.
            if len(parts) >= 4 and parts[0] == "nvidia" and parts[2] == sub:
                target = cache_root.joinpath(*parts)
                target.parent.mkdir(parents=True, exist_ok=True)
                with zf.open(member) as src, open(target, "wb") as dst:
                    shutil.copyfileobj(src, dst)


def ensure_cuda(on_progress: Callable[[float], None] | None = None) -> bool:
    """Downloads and extracts the CUDA runtime wheels into `cuda_cache_dir()` if an NVIDIA GPU is present
    and the libraries are not yet loadable. Returns whether CUDA is usable afterwards."""
    if not needs_cuda_download():
        return cuda_usable()

    wheels = _CUDA_WHEELS.get(sys.platform, ())
    cache_root = cuda_cache_dir()
    missing = [w for w in wheels if not _wheel_installed(cache_root, w)]
    if not missing:
        missing = list(wheels)

    total_bytes = sum(w.size for w in missing)
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

    try:
        cache_root.mkdir(parents=True, exist_ok=True)
        for wheel in missing:
            tmp_whl = cache_root / f".{wheel.subdir}.whl.tmp"
            try:
                digest = hashlib.sha256()
                with urllib.request.urlopen(wheel.url, timeout=60) as resp, open(tmp_whl, "wb") as out:
                    while chunk := resp.read(_CHUNK_SIZE):
                        out.write(chunk)
                        digest.update(chunk)
                        downloaded_total += len(chunk)
                        report(downloaded_total)
                if digest.hexdigest().lower() != wheel.sha256.lower():
                    raise OSError(f"SHA-256 mismatch for {wheel.subdir} CUDA wheel")
                _extract_wheel_libs(tmp_whl, cache_root)
            finally:
                tmp_whl.unlink(missing_ok=True)
    except Exception as exc:
        print(f"CUDA runtime download failed, falling back to CPU: {exc}", file=sys.stderr)
        return False

    register_libraries.cache_clear()
    cuda_usable.cache_clear()
    register_libraries()
    if on_progress is not None:
        on_progress(1.0)
    return cuda_usable()


def is_cuda_library_error(exc: BaseException) -> bool:
    """A CTranslate2 error caused by missing/broken CUDA libraries or an unsupported GPU."""
    text = str(exc).lower()
    return any(key in text for key in ("cublas", "cudnn", "cuda", "no kernel image"))
