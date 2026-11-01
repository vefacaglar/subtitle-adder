# State

## RESUME POINT

2026-11-01: macOS packaging is ready. `scripts/package-macos.sh` → `build/Subtitle Adder.app` (~194 MB, engine as a PyInstaller onedir in `Contents/Resources/subtitle-engine/`), ~87 MB dmg with `MAKE_DMG=1`. The app finds the engine in the bundle without uv/the repo; "Open With" from Finder / dropping on the Dock goes to the open window.

Done:
- Engine: audio extraction (PyAV), STT (faster-whisper; Parakeet v3, Canary 1B v2, GigaAM v3 via onnx-asr), sentence segmentation, SRT/VTT/JSON, model catalog / download progress / deletion, local translation (MADLAD-400 default, NLLB optional).
- macOS: drag-and-drop, language and described model picker, progress, video preview + subtitle overlay, editing (text, time, merge, split, delete, undo), translation, export, Settings (models, disk, license).
- Speaker diarization: engine `--diarize` (sherpa-onnx, local), sentence `speaker`, "- " dash at speaker changes; macOS speaker column/menu, overlay dashes (parity with engine), on by default. Not visually checked by an agent (no screen recording permission).
- Packaging: ad-hoc signing by default; `SIGN_IDENTITY` (Developer ID + hardened runtime) and `NOTARY_PROFILE` (notarization) exist in the script but have never been run with a real identity. Hardened runtime was simulated with ad-hoc and worked.

- Localization: Turkish (default) + English UI with an immediate switch in Settings (General / Appearance / Models tabs); engine messages via `SUBTITLE_ENGINE_LANG` / `--lang`.
- CI (`.github/workflows/ci.yml`): engine tests on Ubuntu + macOS, macOS app build/tests on macos-26. Release (`release.yml`): a `v*` tag builds the ad-hoc signed dmg into a draft GitHub release; v0.1.0 tagged 2026-11-01.

Known issues:
- Diarization: the 0.65 merge threshold was tuned on synthetic TTS dialogues; overlapping speech is not handled; very short real interjections may be snapped into the neighbour.
- huggingface_hub ≥2.4 shared blob store (`hub/blobs/`): `delete-model` now cleans orphaned files there with hf's locked sweeper (private API, with a FileLock fallback if it is missing). If another repo's `blobs/` folder is unreadable, `scan_cache_dir` fails so the message is misleading ("not downloaded") but nothing is deleted.
- The package is arm64 only. The icon appears only in the packaged .app (not with `swift run`).
- Opening a file cancels the running job and discards edits without asking (`SubtitleViewModel.openVideo`).
- `disable-library-validation` is probably unnecessary with a Developer ID; should be tried without it using a real identity.

Next candidates:
- Developer ID signing + notarization (needs an Apple Developer account)
- Burning/muxing subtitles into the video, opening an existing SRT
- Quality/time measurement on a long/music real video; mlx-whisper backend on Mac
- README
- Windows app (WinUI 3 / C#) with the same engine protocol
