# PARITY.md — Rust desktop app vs the C# (Windows) and Swift (macOS) apps

Status as of 2026-11-08, branch `rust-engine`. The specs are in the coordinator's scratchpad (`spec-desktop/
ARCHITECTURE.md`, `API.md`, `UNIFIED.md`). UNIFIED.md decides wherever the two existing apps differ.

**Legend.** **done**: behaves like the reference app (or like the UNIFIED.md decision); **differs**: works, but
visibly or behaviorally different from the reference; **missing**: not available in the Rust app yet.
"W" = compared with the C# WinUI app, "M" = compared with the SwiftUI app.

## How this was checked

* `cargo build --workspace --release` and `cargo test --workspace -j 3` on Windows (1391 passed, 0 failed, 8 ignored), including the
  integration tests `crates/sa-app/tests/app_shell.rs` (the production composition on a temp data folder) and
  `app::win_parts` (temp cleanup). The C# behavior comparisons (`SA_DOTNET_FIXTURES=1`) and real-engine tests
  (`ENGINE_E2E=1`) are opt-in; the area agents ran them.
* **Windows end to end**, real app (`dist\SubtitleAdder.exe` and the debug build) on a copy of data written by the C#
  app's own classes (`tools/fixtures/E2EData`, driven by `scripts/e2e-data.ps1`), with `SUBTITLE_ADDER_DATA_DIR`
  pointing at the copy. Driven through UI Automation (`scripts/e2e-windows.ps1`): start, Platform tab logged out,
  log in with the C# profile, history, log out, open a Windows-SAPI Turkish TTS clip (file dialog and command line),
  transcribe (whisper `base`), translate (`madlad-3b`), edit a line, undo, export SRT through the save dialog, add an
  API-key provider, restart, keys still decrypt. Screenshots: `shots/e2e/*.png` (app window only, PrintWindow).
* **macOS look** on Windows: the cupertino tree rendered with `sa-shot` (software renderer, `SA_UI_PLATFORM=mac`):
  `shots/mac/*.png`; area agents' renders in `shots/main`, `shots/settings`, `shots/hub-mac-*.png`.
* Nothing ran on a Mac (see "Unverified on macOS").
* `shots/` is gitignored: `scripts/e2e-windows.ps1 -Root <scratch>` recreates `shots/e2e`-style captures, and
  `SA_UI_PLATFORM=mac cargo run -p sa-app --features all-catalogs --bin sa-shot -- --out shots/mac/<name>.png
  [--scene editor | --workspace platform --hub logged-out|feed | --window settings --page general|translation]`
  recreates the cupertino ones.

Screenshots (paths relative to `apps/desktop`): `shots/e2e/01-start.png` (empty main window),
`02-hub-logged-out.png`, `03-hub-history.png`, `04-hub-favorites.png`, `05-hub-after-logout.png`,
`06-editor-video.png`, `07-translated.png`, `08-edited.png`, `09-undone.png`, `10-export-dialog.png`,
`11-settings.png`, `12-provider-added.png`, `13-settings-after-restart.png`; cupertino: `shots/mac/main-empty.png`,
`main-editor.png`, `main-editor-dark-en.png`, `hub-logged-out.png`, `hub-feed.png`, `settings-general.png`,
`settings-translation.png`. Compared with mac-ui: unified 52 pt toolbar row with traffic lights, translucent
sidebar with "Filmler", centered segmented Player / Platform / Çeviri listesi, editor table (#, time, `Konuşmacı N`,
text + translation), status bar with the SRT/VTT/JSON segmented control and the Dil pop-up, preferences window with
icon tabs; the visible differences are the placeholder SF Symbols and the unmeasured settings window size.

## Feature table

### Process, windows, chrome

| Feature | Windows (W) | macOS (M) | Evidence |
|---|---|---|---|
| Startup order (crash log, command line, single instance, settings, associations, language, models, VMs, window, external files) | done (`app::run`, `sa_platform::win::startup`) | done in code (SIGPIPE/activation, appearance, language, temp cleanup, models, coalescer, explorer, hub) | E2E `06-editor-video` (file from the command line) |
| Single instance: launch with files forwards to the first instance (byte-exact mutex/pipe) | done | n/a (Launch Services) | `sa-platform` tests |
| Closing the main window quits; shutdown order (size saved, player disposed, broker + settings flushed) | done (`mainWindowWidth/Height` written on close, verified in the E2E copy) | done in code: app stays alive with the menu bar | E2E settings.json after close |
| Multiple main windows (⌘N, shared VM/explorer/hub) | n/a | done in code (`MainWindowHooks::new_window` → `AppShell::open_window`); Dock reopen with no window: **missing** (`OpenDocumentsHandler::set_reopen_handler` not wired) | — |
| Custom title bar, caption buttons, Snap Layouts, saved/centered size | done (`MainChrome`, `WindowChrome`) | n/a | `shots/e2e/01-start.png` |
| Mica backdrop | **missing** (`WindowChrome::set_mica` exists, not enabled) | n/a | — |
| Unified toolbar title bar, traffic lights | n/a | done in code (`winit_attributes` through `BackendSelector`) | `shots/mac/main-empty.png` (stand-in traffic lights) |
| Real NSMenu menu bar (App/File/Edit/View/Window/Help, live relabel) | n/a | done in code (Slint `MenuBar`); standard app-menu roles via AppKit: done in code | — |
| Theme `appTheme` live (System/Light/Dark) | done (main window + Settings) | done in code (+ `NSApp.appearance`) | `shots/e2e/*` (light from the data) |
| UI font `appFontFamily` | done (startup only, like C#) | done in code (live) | — |
| Icons | done (Segoe Fluent Icons) | **differs**: SF Symbols are placeholder rings until the AppKit rasterizer is wired | `shots/mac/*` |
| Accent color from the system | done (UISettings) | done in code (NSColor) | — |
| File associations (HKCU self-registration, Settings card) | done (skipped when `fileAssociationsDisabled`) | n/a (Info.plist) | `sa-platform` C# compare |
| Crash log `%TEMP%\SubtitleAdder-crash.log` | done | n/a | — |
| Temp cleanup `%TEMP%\SubtitleAdder` (broker files, other instances, 2 h rule) | done (`app::win_parts::cleanup_temp_directory`, new test) | done (`working_dir::clean_up` at launch / quit) | unit test |

### Main window, editor, player

| Feature | Windows (W) | macOS (M) | Evidence |
|---|---|---|---|
| Toolbar (Video seç, Altyazı aç, language/model/speakers/target/translation-model pickers, Çevir…, Otomatik kaydet, profile, settings, Altyazı üret, Komple çevir) and responsive collapse | done | done | `shots/e2e/01-start.png`, `07-translated.png`; `shots/main/*` |
| Toolbar translation-model picker (flyout / popover with providers) | **missing**: the button opens Settings on the providers page (`general` / `translation`) | **missing** (same fallback) | — |
| Workspaces Player / Platform / Çeviri listesi (count badge) | done | done | E2E shots |
| Empty state, drop target | done | done | `shots/e2e/01-start.png` |
| Editor rows, speaker chips, translation sub-line, stale marker, needs-review marker | done | done | `07-translated.png`, `08-edited.png` |
| Edit text, Enter commits, undo/redo with C# button titles (`Metni Değiştir İşlemini Geri Al`) | done | done in code (Edit-menu titles) | `08-edited.png`, `09-undone.png` |
| Split / merge / delete / timing ±0.1 s / set to now | done | done | `sa-app` tests |
| Keyboard shortcuts (Space, arrows, M, F, F11, E, Esc, Ctrl+…/⌘…) | done | **differs**: non-Latin keyboard layouts do not fall back to the QWERTY position for M/F/E (Slint passes no hardware key code) | `sa-app` tests |
| Wheel volume on the video | done | **differs**: mouse wheel treated as precise (no notch/momentum info from Slint) | — |
| Player: FFmpeg 8.1 + cpal, first frame, seek, volume, end, every accepted extension | done (replaces Media Foundation; ARCHITECTURE §7) | done in code (replaces AVFoundation) | `sa-player` tests, E2E |
| Encrypted SSD chunk cache + buffer bar (`BufferedVideoStream` + VideoCache, nothing cached logged out) | done (`app::win_parts::CachedPlayer`) | n/a | — |
| Rename/delete a playing file | done (share delete) | done | `sa-player` test |
| Subtitle overlay (compose, style, margins) | done | done | `07-translated.png` |
| Transport HUD, display picker, theater, side-by-side, hide panel, full screen | done | done | `shots/main/*` |
| macOS preview fallback (engine `preview`) | n/a | done in code (`PreviewMaker` → engine `preview`), only when FFmpeg fails | — |
| HDR tone mapping, deinterlacing, hardware decoding, audio-only fallback | **missing** (MF did these) | **missing** (AVFoundation did these) | — |

### Jobs, translation, export

| Feature | Windows (W) | macOS (M) | Evidence |
|---|---|---|---|
| Transcribe through `subtitle-engine` (JSON lines, stages, partials) | done (whisper `base` on the TTS clip) | done in code | `07-translated.png` |
| Translate (madlad-3b), incremental events, gate, queue | done | done in code | `07-translated.png` |
| Komple çevir (generate + translate) | done | **differs**: when ⌘O needs the confirmation sheet first, the run after the pick is lost (no continuation API) | E2E |
| Auto-save translation next to the video (`Otomatik kaydet`) | done (`altyazi-testi.srt` written, SRT badge in the explorer) | done | `07-translated.png` |
| Export (save dialog, one type, extension appended, starts in the last folder) | done | done in code (rfd NSSavePanel) | `10-export-dialog.png`, export file |
| Playlist (3-stage pipeline, list rows) | done | done | `shots/main/*playlist*` |
| External LLM / API providers for translation | done | done in code | Settings shots |
| Offline mode, "go online and retry" | done | done | `sa-viewmodels` tests |

### Video platform hub, explorer, profiles (privacy rule 6)

| Feature | Windows (W) | macOS (M) | Evidence |
|---|---|---|---|
| Logged out: sign-in card on every tab, search disabled, no feed/index query, nothing in memory | done (checked on all 5 tabs in the real app) | done in code (FIX-M per UNIFIED §3.9) | `02-hub-logged-out.png`, `05-hub-after-logout.png`, `tests/app_shell.rs` |
| Logged-out playback records nothing | done (C# reader: history unchanged after logged-out opens) | done in code | E2E `dump` check |
| Login with a C# profile: history, views, likes, ratings, PNG thumbnails, lists | done | n/a (LibraryKit format, `sa-profiles::mac` tests) | `03-hub-history.png`, `04-hub-favorites.png` |
| Remember-me opt-in, logout wins, other profiles' events ignored | done (`sa-profiles` tests) | done in code (encrypted `profile-remember-keys.json`, no Keychain) | tests |
| Lists, add-to-list menu, delete, rename (macOS) | done | done; **differs**: list counts include files missing on disk | `shots/hub-*` |
| Hub card like-button pink tint, middle-truncated folder line | n/a | **differs** (no tint; end truncation) | `shots/hub-mac-feed.png` |
| Profile popover closes on a second click of the toolbar button | n/a | unverified | — |
| Explorer sidebar: quick locations, transient listing of an opened video's folder, index search only logged in, no `explorerLastFolder` | done | done | `06-editor-video.png` |

### Settings, providers, keys

| Feature | Windows (W) | macOS (M) | Evidence |
|---|---|---|---|
| Settings window (W: Genel/Görünüm/Modeller, resizable, saved size; M: 4 preference tabs, per-tab height) | done | done in code; **differs**: window height/NSPopUp look not measured against the real app | `11-settings.png`, `shots/settings/*`, `shots/mac/settings-*` |
| Legacy LAN section bridged to the `lan` provider | done | n/a | `11-settings.png` |
| Providers: add / edit / delete / activate / test (key never over plain http to a public host) | done | done in code | `12-provider-added.png` |
| API keys encrypted on disk (AES-256-GCM sidecar, DPAPI data key) | done: C# plaintext `apiKey` moved into `translation-provider-keys.json` on first load; new keys written encrypted; no plaintext key anywhere under the data or temp folder; both keys decrypt after a restart | done in code (0600 key file, no Keychain); keys from the Swift app's memory/Keychain must be re-entered once | `13-settings-after-restart.png`, E2E scan |
| Models page (catalog, sizes, delete with confirmation) | done | done | `shots/settings/*models*` |
| Video cache card, file-association card | done | n/a | `11-settings.png` |
| Appearance (theme, UI font, overlay style; macOS NSColorPanel) | done | done in code | `shots/settings/*appearance*` |

### Data, sync, localization

| Feature | Windows (W) | macOS (M) | Evidence |
|---|---|---|---|
| `%LOCALAPPDATA%\SubtitleAdder`: profiles.json v3 + profiles.db, settings.json, translation-providers.json, media-index.json, VideoCache | done, read and written in place (E2E on C#-written data, C# reads the result back) | n/a | E2E, fixture tests |
| `~/Library/Application Support/SubtitleAdder` (LibraryKit v2, CFPreferences `app.subtitleadder.mac`) | n/a | done in code; checked against a second implementation only (C# writer of the v2 format) | `sa-profiles::mac` tests |
| `SUBTITLE_ADDER_DATA_DIR` override (tests/tools; separate broker scope) | done | done (also switches settings to a JSON file there) | E2E |
| Cross-instance broker (C#-compatible named pipe), settings topic reload | done (`BusSettingsHook`) | n/a (in-process bus + file watcher) | `sa-sync` tests |
| Every Turkish + English string from the existing catalogs, live language switch | done | done | catalog tests, `*-relang.png` |

## Unverified on macOS (nothing was compiled for, or run on, a Mac)

* `sa-app`, `sa-player` (cpal/CoreAudio), `sa-platform` (objc2) and anything depending on `ring` could not be
  cross-checked here (ARCHITECTURE §11); only the pure crates passed `cargo check --target aarch64-apple-darwin`.
  The new CI job `desktop (macos-26)` is the first real build.
* The whole macOS branch of `app::run`: `MacShell::prepare_process`, `BackendSelector` with the winit attributes,
  `UserDefaultsSettingsStore`, `MacProfileStore` (kqueue watcher, flock, fsync), `MacMediaIndex` with the
  CoreFoundation lister, `MacProviderStore` + `KeyFileProtector` (0600), the Apple Event open-documents handler,
  `run_event_loop_until_quit` (app alive without windows), ⌘N multi-window.
* Menu bar (Slint `MenuBar` → NSMenu), standard app-menu roles, Edit-menu undo titles, ⌘ shortcuts.
* SF Symbols rasterizer (placeholders are drawn today), NSAlert sheets, NSColorPanel, NSSavePanel/NSOpenPanel via rfd.
* FFmpeg + CoreAudio playback, the preview fallback, AV1/WMV decoders in the macOS FFmpeg build (Homebrew).
* Window frame restore (`mainWindowFrame`), native full screen tracking, traffic-light placement, toolbar drag and
  `AppleActionOnDoubleClick`.
* Foundation-specific text rules: `localizedCaseInsensitiveCompare` sorts, JSONEncoder key order, number formats,
  ISO-8601 date forms, grapheme-cluster cutting of the diagnostic log.
* Thumbnail generation through AVAssetImageGenerator (384×216 JPEG).
* `scripts/package-desktop-macos.sh` (signing, notarization, dylib rewrites).

## Other known gaps

* FFmpeg build licensing: the staged DLLs come from the BtbN **GPL** shared build (same as the engine); ARCHITECTURE
  §11 asks for LGPL builds in a release.
* No toolbar translation-model picker (see above); the area agent's `TranslationModelPickerModel` exists in
  `sa-viewmodels` without a UI.
* C# `OpenVideoProvider` (adopting the open video after a login for likes/ratings without recording progress) is not
  wired.
* `rustfmt`/`clippy` are not installed in this toolchain; nothing was linted.
