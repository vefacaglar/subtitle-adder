<p align="center">
  <img src="apps/desktop/crates/sa-platform/windows/AppIcon.png" width="64" height="64" alt="Subtitle Adder icon">
</p>

<h1 align="center">Subtitle Adder</h1>

<p align="center">
  A desktop app that adds subtitles to video automatically, running entirely on your own computer.
</p>

<p align="center">
  <a href="https://github.com/vefacaglar/subtitle-adder/releases/latest">Download</a> ·
  <a href="#features">Features</a> ·
  <a href="#models">Models</a> ·
  <a href="#building-from-source">Building</a> ·
  <a href="LICENSE">MIT license</a>
</p>

---

Drop a video in, pick a language, and Subtitle Adder transcribes the speech, splits it into readable sentences, and
gives you an editable subtitle track that plays over the video. It can tell speakers apart, translate the result, and
export SRT, VTT or JSON.

Speech recognition, diarization and translation all run on your machine. No audio ever leaves it.

## Features

- **On-device speech recognition** with word-level timestamps: Whisper (a faster-whisper reimplementation on
  CTranslate2) and NVIDIA Parakeet, Canary and GigaAM models on ONNX Runtime.
- **Readable subtitles**: sentences are cut at punctuation and pauses, and long ones are split at a natural point
  instead of mid-phrase.
- **Speaker diarization** (optional): every sentence gets a speaker, a subtitle never mixes two speakers, and a
  speaker change is marked with a dash in SRT and VTT.
- **Translation** with local models (MADLAD-400, NLLB-200, and Qwen / Gemma through llama.cpp) between Turkish,
  English, Portuguese, Spanish, Russian, German, French, Japanese and Chinese.
- **Optional external translator**: point the app at an OpenAI-compatible server on your network (LM Studio, Ollama,
  llama-server) or at a cloud API you choose. This is opt-in, and only subtitle text is sent.
- **Subtitle editor**: edit text and timing, split and merge lines, select several lines and retranslate just those.
  Lines a translation could not recover are flagged for review.
- **Built-in player**: plays the video with the subtitles over it, with audio and subtitle track selection, including
  bitmap subtitle tracks (PGS, VobSub, DVB, XSUB).
- **Import and export**: open existing SRT, VTT or JSON subtitles; export SRT, VTT or JSON.
- **Batch playlists**: queue several videos and let them run one after another.
- **File explorer and library**: browse folders, search them (subfolders included), and see which videos already have
  subtitles.
- **Strict offline mode**: once the models you need are downloaded, nothing is fetched and nothing is sent.
- **Turkish and English** user interface.

## Download

Get the latest build from the [Releases page](https://github.com/vefacaglar/subtitle-adder/releases/latest).

| Platform | File | Notes |
|---|---|---|
| Windows (x64) | `SubtitleAdder-<version>-win-x64.zip` | Extract anywhere and run `Subtitle Adder\SubtitleAdder.exe`. The build is unsigned, so SmartScreen may warn on first run: choose **More info**, then **Run anyway**. |
| macOS (Apple Silicon, macOS 15+) | `Subtitle-Adder-<version>-macos-arm64.dmg` or `.zip` | Drag **Subtitle Adder** to Applications. If the build is not notarized, right-click the app and choose **Open** the first time. |

Models are not bundled. Each one is downloaded the first time you use it and kept in the Hugging Face cache
(`~/.cache/huggingface/`).

## Privacy

- Audio is extracted, transcribed and diarized locally. There is no cloud speech-to-text.
- The network is used only to download a model you chose and the runtime it needs (llama.cpp, CUDA libraries).
- Subtitle **text** leaves the machine only if you configure an external translation server or API provider.
- An API key is never sent over plain `http://` to a host outside your local network.

## Models

All models are optional downloads; sizes are approximate.

| Kind | Models | License |
|---|---|---|
| Speech recognition | Whisper `base`, `small`, `medium`, `large-v3-turbo`, `large-v3` (all languages) | MIT |
| Speech recognition | Parakeet TDT 0.6B v3, Canary 1B v2 (25 European languages) | CC-BY-4.0 |
| Speech recognition | GigaAM v3 (Russian only) | MIT |
| Speaker diarization | pyannote segmentation 3.0 + 3D-Speaker CAM++ (~29 MB) | MIT AND Apache-2.0 |
| Translation | MADLAD-400 3B (default, ~2.8 GB) | Apache-2.0 |
| Translation | Qwen 3.5 4B / 9B, Gemma 4 E4B / 12B (GGUF, run by llama.cpp; 2.6 – 6.8 GB) | Apache-2.0 |
| Translation | NLLB-200 1.3B (~1.3 GB) | CC-BY-NC-4.0, **no commercial use** |

Turkish speech recognition is available in the Whisper models. NLLB is downloaded only when you ask for it and is
marked as non-commercial in the app.

## How it works

```
apps/macos   (SwiftUI)        ─┐
apps/desktop (Rust + Slint)   ─┼─ subprocess + JSON line events ─▶  engine/ (Rust)
                               ┘
```

The user interface is native on each platform and holds no business logic. The work is done by a shared Rust engine,
`subtitle-engine`, that the apps start as a subprocess and follow through one JSON event per line:

1. **Audio extraction**: FFmpeg turns the video into 16 kHz mono audio.
2. **Speech-to-text**: the chosen model produces a transcript with word timestamps.
3. **Speaker diarization** (optional): each word is assigned a speaker.
4. **Sentence segmentation**: words become subtitle-sized sentences.
5. **Subtitles**: SRT, VTT or JSON is rendered.

The engine is also a command-line tool:

```bash
subtitle-engine transcribe video.mp4 --model small --language tr --out-dir output --format srt vtt json
```

```bash
subtitle-engine transcribe interview.mp4 --model small --out-dir output --diarize
```

```bash
subtitle-engine models
```

The full protocol (commands, events, checkpoints, environment variables) is documented in [CLAUDE.md](CLAUDE.md).

## Repository layout

| Path | What it is |
|---|---|
| `engine/` | The Rust engine (Cargo workspace): audio, speech recognition, diarization, segmentation, translation, model catalog, CLI. |
| `apps/desktop/` | The Rust + Slint desktop app. This is the Windows app; it also builds for macOS. |
| `apps/macos/` | The native SwiftUI macOS app and its engine client. |
| `scripts/` | Packaging and version scripts. |
| `.github/workflows/` | CI, and the manually started build-and-release workflow. |

## Building from source

Requirements: Rust stable (edition 2024). macOS builds also need Swift 6 on macOS 15 or later and Homebrew's FFmpeg.
Windows builds need MSVC, CMake, Ninja, libclang (`LIBCLANG_PATH`) and nuget, plus a short `CARGO_TARGET_DIR`
because long paths break `cl.exe`.

### Engine

```bash
engine/scripts/fetch-runtime.sh
```

```bash
cd engine && cargo test --workspace -j 2
```

```bash
cd engine && cargo build --release -p se-cli && scripts/stage-runtime.sh
```

On Windows use the `.ps1` twins of the two scripts. The staged engine ends up in `engine/dist`.

### Windows app

```bash
powershell -File apps/desktop/scripts/run.ps1
```

This builds the engine and the app when their sources changed, stages `apps/desktop/dist` and starts the app. To
build the distributable package:

```bash
pwsh scripts/package-desktop-windows.ps1 -Zip
```

### macOS app

```bash
cd apps/macos && swift build && swift test
```

```bash
cd apps/macos && swift run SubtitleAdderApp
```

```bash
scripts/package-macos.sh
```

The package script produces `build/Subtitle Adder.app`, ad-hoc signed by default. `SIGN_IDENTITY`, `NOTARY_PROFILE`
and `MAKE_DMG=1` turn on Developer ID signing, notarization and a `.dmg`.

### Tests for the desktop app

```bash
cd apps/desktop && cargo test --workspace -j 4
```

## Versioning

The project follows Semantic Versioning. `VERSION` at the repository root is the single source of truth, and
`scripts/bump-version.sh` (or `.ps1`) updates it together with the two Cargo workspaces.

## License

Subtitle Adder is released under the [MIT license](LICENSE).

The models keep their own licenses, listed in the table above. The Windows package ships LGPL FFmpeg libraries next
to the app and a GPL FFmpeg build inside the engine folder, which the engine uses to make preview copies.
