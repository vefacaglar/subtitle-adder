# PyInstaller (onedir) spec: run by `scripts/package-macos.sh`.
# Platform-neutral: collect_all picks up the .dylib/.so/.dll files; the EXE gets its .exe suffix on Windows.
# Manually: cd engine && uv run --group build pyinstaller subtitle-engine.spec
from PyInstaller.utils.hooks import collect_all, collect_submodules

datas, binaries, hiddenimports = [], [], []
for pkg in ("ctranslate2", "onnxruntime", "av", "tokenizers", "sentencepiece",
            "onnx_asr", "sherpa_onnx", "faster_whisper", "huggingface_hub", "hf_xet"):
    d, b, h = collect_all(pkg)
    datas += d
    binaries += b
    hiddenimports += h
# cli.py imports these modules lazily.
hiddenimports += collect_submodules("subtitle_engine")

a = Analysis(
    ["packaging/entry.py"],
    pathex=["src"],
    binaries=binaries,
    datas=datas,
    hiddenimports=hiddenimports,
    excludes=["pytest", "tkinter", "matplotlib", "PyInstaller"],
)
pyz = PYZ(a.pure)
exe = EXE(
    pyz, a.scripts, [],
    exclude_binaries=True,
    name="subtitle-engine",
    console=True,
    strip=False,
    upx=False,
)
coll = COLLECT(exe, a.binaries, a.datas, name="subtitle-engine", strip=False, upx=False)
