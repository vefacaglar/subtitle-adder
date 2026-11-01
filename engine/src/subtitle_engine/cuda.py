"""NVIDIA GPU support for CTranslate2 (faster-whisper).

CTranslate2 loads cuBLAS/cuDNN lazily: a machine with an NVIDIA GPU but without the CUDA 12 runtime
libraries reports a CUDA device and then fails in the middle of transcription ("cublas64_12.dll is not
found"). The optional `cuda` dependency group installs those libraries as NVIDIA's pip wheels
(`nvidia/<lib>/bin` on Windows, `nvidia/<lib>/lib` on Linux); this module makes them loadable and
checks that they actually load before the GPU is chosen.
"""
import ctypes
import os
import sys
import sysconfig
from functools import cache
from pathlib import Path

_WINDOWS_LIBS = ("cublas64_12.dll", "cublasLt64_12.dll", "cudnn64_9.dll")
_LINUX_LIBS = ("libcublasLt.so.12", "libcublas.so.12", "libcudnn.so.9")


def _nvidia_dirs() -> list[Path]:
    """Library folders of the installed nvidia-* wheels (empty without the `cuda` group)."""
    sub = "bin" if sys.platform == "win32" else "lib"
    roots = {Path(sysconfig.get_paths()["purelib"]), Path(sysconfig.get_paths()["platlib"])}
    roots.update(Path(p) for p in sys.path if p.endswith("site-packages"))
    dirs: list[Path] = []
    for root in roots:
        nvidia = root / "nvidia"
        if nvidia.is_dir():
            dirs.extend(d / sub for d in sorted(nvidia.iterdir()) if (d / sub).is_dir())
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
            os.add_dll_directory(str(d))
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


@cache
def cuda_usable() -> bool:
    """Whether faster-whisper can run on the GPU: a CUDA device exists and cuBLAS/cuDNN load."""
    import ctranslate2

    try:
        if ctranslate2.get_cuda_device_count() == 0:
            return False
    except RuntimeError:
        return False
    if sys.platform == "darwin":
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


def is_cuda_library_error(exc: BaseException) -> bool:
    """A CTranslate2 error caused by missing/broken CUDA libraries or an unsupported GPU."""
    text = str(exc).lower()
    return any(key in text for key in ("cublas", "cudnn", "cuda", "no kernel image"))
