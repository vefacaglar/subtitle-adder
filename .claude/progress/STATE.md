# State

## RESUME POINT

2026-11-01 (macOS parity with the Windows features): the macOS app now has the Windows-side features, built in five commits on top of the engine (`abe39c2` EngineKit, `b5c1390` app core, `0061777` files, `09badf5` external LLM, `fe93e7b` playlist). Engine protocol and CLAUDE.md are updated to HEAD (partial/incremental translation events, llama backend, new env vars, checkpoints).
- EngineKit: `partial` event (unknown event types are skipped), launch environment (offline flag, external LLM URL/model) via `EngineLaunchState`, external LLM helpers (address normalizing, model list parsing, connection test), `SubtitleParser` (SRT/VTT/ASS/SSA/JSON import, sidecar lookup), `WorkingDirectory` (`<temp>/SubtitleAdder`).
- App core: languages de/fr/ja/zh; Qwen 3.5 / Gemma 4 translation models in the toolbar menu, Settings and Models tab (NLLB keeps its non-commercial marking); offline mode (Settings > Gizlilik, on by default, blocked-job message with "Çevrimdışı modu kapat ve indir"); display/export language sync; live partial snapshots merged into the line list keeping line identities; editor fields disabled while a job runs; working folder cleaned at launch and quit (not while working).
- Files: open an existing subtitle (button, drop, Finder), sidecar auto-load next to a video, "Altyazıyı kaydet", auto-save of translations, "Komple çevir" (transcribe + translate + save).
- External LAN LLM: Settings section (toggle, address, connection test, model field/list, async toggle), toolbar label, async translation inside "Komple çevir" (finished lines translate while transcription continues). `NSLocalNetworkUsageDescription` added to `scripts/macos/Info.plist.in`.
- Playlist: Düzenleyici / Çalma listesi tabs, sequential batch run with per-item language, async external-LLM mode (translation of one item overlaps the next transcription), failed items retried only by the next run.
- Layout (Windows e4816ad): the player overlay (top right, next to the display picker) has three switches: subtitles to the right/bottom (`sideBySideLayout` in UserDefaults; HSplitView player min 260 / editor min 320, VSplitView 180 / 160), theater mode (hides the window toolbar via `.toolbar(.hidden, for: .windowToolbar)`; not persisted) and native window full screen (`WindowChrome`: button state follows `NSWindow.didEnter/ExitFullScreenNotification`, so ⌃⌘F, the green button and Esc agree). Esc (when no text is being edited) leaves full screen first, then theater mode. "Altyazı üret" (⌘↩) moved from the toolbar button to a File-menu command so it works in theater mode; theater and layout toggles are also in the View menu. The edit bar drops the Split/Merge/Delete titles under 720 pt and the Başlangıç/Bitiş captions under 540 pt.
- Translation progress: explicit `isAsyncTranslating` / `isTranslationJobRunning` flags (no label parsing); `isWorking` covers async translation; `translationBadgeText` drives the editor-header badge; fractions are capped at 0.99 everywhere (foreground reuse, background batches, playlist rows).
Deviations from the Windows app / spec, and open items:
- Save naming is `<stem>.<fmt>` for the original and for a translation (no language suffix, overwrites, like Windows); Export… still suggests `<name>[.<lang>].<fmt>`. A saved translation therefore becomes the next sidecar: "Komple çevir" with a video open always re-transcribes, but the playlist (like Windows) uses a sidecar as it is.
- Live partial merge matches lines by exact (start, end, text) then by same text within 1.5 s; the line number is not used any more (Windows 573d933), so a refined last line gets a new identity each snapshot.
- Translation: a partial request carries two neighbours per side as context and only requested ids are applied (no positional fallback); a translation that repeats a source of 4+ characters is retranslated; the async pump holds back the last two lines and waits for 10 ready lines (Windows 8b09cf4, 21e184a).
- Async translation only runs inside "Komple çevir" and the playlist, not plain "Altyazı üret". The playlist does not pre-translate during transcription (Windows does); an item starts translating after its own transcription. Async errors are not shown during the background pass; the closing pass reports them.
- The playlist keeps toolbar language/model/target unpersisted and restores them at the end; `isWorking` also covers a playlist run.
- No `.local` mDNS resolution or subnet scan in the app's connection test (the engine does that itself when translating).
- Not done: extracting_audio percent in the status label.
- Unplanned fix: `EngineClient.exitStatus()` used `Process.waitUntilExit()` and `swift test` hung in 25-50% of runs once more fake-engine tests existed; replaced with a `terminationHandler` + `ExitSignal`. The root cause of the hang is unproven.
- Not verified: the GUI was only launch-checked for 6 s (Settings > Genel height 800, playlist rows, segmented switcher, drop zones, display picker segment disabling are unseen; likewise the three layouts, the overlay panel's narrow variants, theater mode hiding the toolbar and the HSplitView/VSplitView start ratio, since screencapture has no permission here); `ENGINE_E2E=1 swift test`; a real LAN server; the Local Network prompt for the engine child process in the packaged .app.
- A single video dropped on the editor tab with no run active still opens in the editor (not the playlist).

2026-11-01 (Windows): native Windows app in `apps/windows` (WinUI 3, unpackaged, self-contained Windows App SDK; .NET 10). Layers: `EngineKit` (.NET port of the macOS EngineKit), `SubtitleAdder.Core` (UI-free view models, settings in `%LOCALAPPDATA%\SubtitleAdder\settings.json`), `SubtitleAdder` (WinUI). Feature parity with macOS was checked by running the app (UI Automation + screenshots); translation was not run (no translation model downloaded on the dev machine). `scripts/package-windows.ps1` → `build\Subtitle Adder\` (+ `-Zip`), engine as PyInstaller onedir next to `SubtitleAdder.exe`; CI/release have Windows jobs (not yet run on GitHub). Engine on Windows: no-symlink HF cache layout handled; NVIDIA GPU via the optional `cuda` dependency group (`uv sync --group cuda`) with CPU fallback; no network once models are cached; strict offline mode (`SUBTITLE_ENGINE_OFFLINE` / `--offline`), on by default in the Windows app (Settings > General > Privacy; the error bar offers to go online and download).

Windows known gaps / next:
- Localization: the user wants key-based `en.json` / `tr.json` files with translation codes in the code, instead of Turkish source strings as keys (both apps currently use the macOS String Catalogs with Turkish keys). Not started.
- Packaged Windows build is CPU only (the cuda group is not bundled); unsigned zip (SmartScreen warns).
- dotnet publish drops SubtitleAdder.pri / *.xbf for unpackaged WinUI; the packaging script copies them from the build output.


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
