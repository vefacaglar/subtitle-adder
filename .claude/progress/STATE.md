# State

## RESUME POINT

2026-11-01: macOS packaging is ready. `scripts/package-macos.sh` → `build/Subtitle Adder.app` (~194 MB, engine as a PyInstaller onedir in `Contents/Resources/subtitle-engine/`), ~87 MB dmg with `MAKE_DMG=1`. The app finds the engine in the bundle without uv/the repo; "Open With" from Finder / dropping on the Dock goes to the open window.

Done:
- Engine: audio extraction (PyAV), STT (faster-whisper; Parakeet v3, Canary 1B v2, GigaAM v3 via onnx-asr), sentence segmentation, SRT/VTT/JSON, model catalog / download progress / deletion, local translation (MADLAD-400 default, NLLB optional).
- macOS: drag-and-drop, language and described model picker, progress, video preview + subtitle overlay, editing (text, time, merge, split, delete, undo), translation, export, Settings (models, disk, license).
- Packaging: ad-hoc signing by default; `SIGN_IDENTITY` (Developer ID + hardened runtime) and `NOTARY_PROFILE` (notarization) exist in the script but have never been run with a real identity. Hardened runtime was simulated with ad-hoc and worked.

Known issues:
- huggingface_hub ≥2.4 shared blob store (`hub/blobs/`): `delete-model` now cleans orphaned files there with hf's locked sweeper (private API, with a FileLock fallback if it is missing). If another repo's `blobs/` folder is unreadable, `scan_cache_dir` fails so the message is misleading ("not downloaded") but nothing is deleted.
- The package is arm64 only. The icon appears only in the packaged .app (not with `swift run`).
- Opening a file cancels the running job and discards edits without asking (`SubtitleViewModel.openVideo`).
- `disable-library-validation` is probably unnecessary with a Developer ID; should be tried without it using a real identity.

Next candidates:
- (In progress) Speaker diarization: engine `--diarize`, sentence `speaker`, "- " at speaker changes in subtitles; then the macOS UI
- (Decided, after diarization) Multi-language UI: Turkish (default) + English, switchable in Settings; the engine's user-facing error messages must be translated too. The "UI strings are Turkish" rule in CLAUDE.md will be updated accordingly
- Developer ID signing + notarization (needs an Apple Developer account)
- Burning/muxing subtitles into the video, opening an existing SRT
- Quality/time measurement on a long/music real video; mlx-whisper backend on Mac
- README
- Windows app (WinUI 3 / C#) with the same engine protocol
