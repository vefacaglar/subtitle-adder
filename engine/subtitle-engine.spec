# PyInstaller (onedir) tanımı: `scripts/package-macos.sh` çalıştırır.
# Elle: cd engine && uv run --group build pyinstaller subtitle-engine.spec
from PyInstaller.utils.hooks import collect_all, collect_submodules

datas, binaries, hiddenimports = [], [], []
for pkg in ("ctranslate2", "onnxruntime", "av", "tokenizers", "sentencepiece",
            "onnx_asr", "faster_whisper", "huggingface_hub", "hf_xet"):
    d, b, h = collect_all(pkg)
    datas += d
    binaries += b
    hiddenimports += h
# cli.py bu modülleri tembel içe aktarır.
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
