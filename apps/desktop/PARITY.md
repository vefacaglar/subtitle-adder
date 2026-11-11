# PARITY.md — Rust desktop app vs the C# (Windows) and Swift (macOS) apps

Status as of 2026-11-08, branch `rust-engine`. The specs are in the coordinator's scratchpad (`spec-desktop/
ARCHITECTURE.md`, `API.md`, `UNIFIED.md`). UNIFIED.md decides wherever the two existing apps differ.

**Legend.** **done**: behaves like the reference app (or like the UNIFIED.md decision); **differs**: works, but
visibly or behaviorally different from the reference; **missing**: not available in the Rust app yet.
"W" = compared with the former C# WinUI app (removed from the repo; its behavior is pinned by frozen C# outputs in
`crates/*/tests/fixtures/csharp`, C# sources as of commit 2f4bc96), "M" = compared with the SwiftUI app.

## How this was checked

* Final gate (2026-11-08, after the C# app was removed): `cargo build --workspace --release` and
  `cargo test --workspace -j 4 --no-fail-fast` on Windows (78 test binaries: 1422 passed, 0 failed, 8 ignored),
  including the integration tests `crates/sa-app/tests/app_shell.rs` (the production composition on a temp data
  folder) and `app::win_parts` (temp cleanup). The C# behavior comparisons read frozen C# outputs and run by default
  on Windows (none needs .NET any more); only the real-engine tests (`ENGINE_E2E=1`) are opt-in; the area agents ran
  them. Two parts are bound to the Windows account that recorded
  the C# data and print `SKIPPED` on any other account (CI runners included): the DPAPI remembered-session restore
  in `sa-profiles`' `win_fixtures` (recognized by the SHA-256 of the user SID in `recording-account.json`; the rest
  of each dump is still compared) and `sa-sync`'s .NET default-scope check.
* **Windows end to end**, real app (`dist\SubtitleAdder.exe`, restaged by `scripts/stage.ps1` for the final gate, with
  the LGPL FFmpeg DLLs next to the app) on a copy of data written by the C#
  app's own classes (frozen in `crates/sa-app/tests/fixtures/csharp/e2e-data`, restored by `scripts/e2e-data.ps1`), with `SUBTITLE_ADDER_DATA_DIR`
  pointing at the copy. Driven through UI Automation (`scripts/e2e-windows.ps1`): start, Platform tab logged out,
  log in with the C# profile, history, log out, open a Windows-SAPI Turkish TTS clip (file dialog and command line),
  transcribe (whisper `base`), translate (`madlad-3b`), edit a line, undo, export SRT through the save dialog, add an
  API-key provider, restart, keys still decrypt. Screenshots: `shots/e2e/*.png` (app window only, PrintWindow). The final
  rerun (no .NET on PATH) passed every step and check (no plaintext key, profile reader `win_profile_dump`, which
  prints the C# reader's lines: logged-out use left the history at `views=2`); all 13
  screenshots were looked at and compared with the C# layout (e.g. the card's `+ Listeye ekle` keeps C#'s plus glyph
  *and* the `+` in the catalog string).
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
`11-settings.png`, `12-provider-added.png`, `13-settings-after-restart.png`; cupertino (re-rendered for the final gate): `shots/mac/main-empty.png` (no view model: empty pickers),
`main-empty-toolbar.png` (`--scene empty`: the same window with the toolbar and sidebar filled),
`main-editor.png`, `main-editor-dark-en.png`, `hub-logged-out.png`, `hub-feed.png`, `settings-general.png`,
`settings-translation.png`, `symbols-light.png`, `symbols-dark.png` (`--window symbols`: every SF Symbol stand-in). Compared with mac-ui: unified 52 pt toolbar row with traffic lights, translucent
sidebar with "Filmler", centered segmented Player / Platform / Çeviri listesi, editor table (#, time, `Konuşmacı N`,
text + translation), status bar with the SRT/VTT/JSON segmented control and the Dil pop-up, preferences window with
icon tabs; the SF Symbols are drawn by the SVG stand-ins off macOS (`shots/mac/symbols-*.png`); the visible difference left is the unmeasured settings window size.

## Feature table

### Process, windows, chrome

| Feature | Windows (W) | macOS (M) | Evidence |
|---|---|---|---|
| Startup order (crash log, command line, single instance, settings, associations, language, models, VMs, window, external files) | done (`app::run`, `sa_platform::win::startup`) | done in code (SIGPIPE/activation, appearance, language, temp cleanup, models, coalescer, explorer, hub) | E2E `06-editor-video` (file from the command line) |
| Single instance: launch with files forwards to the first instance (byte-exact mutex/pipe) | done | n/a (Launch Services) | `sa-platform` tests |
| Closing the main window quits; shutdown order (size saved, player disposed, broker + settings flushed) | done (`mainWindowWidth/Height` written on close, verified in the E2E copy) | done in code: app stays alive with the menu bar | E2E settings.json after close |
| Multiple main windows (⌘N, shared VM/explorer/hub) | n/a | done in code (`MainWindowHooks::new_window` → `AppShell::open_window`); Dock reopen: done in code (`rapp` handler: unhide, then one main-queue turn later deminiaturize, else `AppShell::reopen_main_window`; a miniaturized window never counts as visible; rule `mac_rules::reopen_action` tested); a Finder open / Dock drop with no window left opens a main window for the files (`AppShell::open_external_files`; close → reopen → open tested through the real close path in `tests/app_shell.rs`) | the AppKit side (`handle_reopen`, `unhide:` timing) is unverified without a Mac |
| Custom title bar, caption buttons, Snap Layouts, saved/centered size | done (`MainChrome`, `WindowChrome`) | n/a | `shots/e2e/01-start.png` |
| Mica backdrop | done: main + Settings windows (`DWMWA_SYSTEMBACKDROP_TYPE = DWMSBT_MAINWINDOW`, 21H2 attribute 1029; the frame is extended from the bottom edge only, `win::chrome::mica_margins`, because a sheet of glass made DWM draw a second set of caption buttons under the Slint ones; the Slint background turns transparent only while DWM draws Mica and the window's own renderer has per-pixel alpha, `main_window::chrome::mica_renderer`, so the software renderer, also as winit's fallback, keeps the solid background) | n/a | live run: with the bottom-only extension DWM draws no caption glyphs of its own (with `-1` it drew a full set, `shots/picker/live-mica-video.png`); `DWMWA_SYSTEMBACKDROP_TYPE = 2` read back. Not yet seen on screen: the Slint title bar over Mica after this change and Mica's look itself (transparency effects off on the dev machine; the display was off during the last check, so the GPU window did not paint) |
| Unified toolbar title bar, traffic lights | n/a | done in code (`winit_attributes` through `BackendSelector`) | `shots/mac/main-empty.png` (stand-in traffic lights) |
| Real NSMenu menu bar (App/File/Edit/View/Window/Help, live relabel) | n/a | done in code (Slint `MenuBar`); standard app-menu roles via AppKit: done in code | — |
| Theme `appTheme` live (System/Light/Dark) | done (main window + Settings) | done in code (+ `NSApp.appearance`) | `shots/e2e/*` (light from the data) |
| UI font `appFontFamily` | done (startup only, like C#) | done in code (live) | — |
| Icons | done (Segoe Fluent Icons) | done in code: SF Symbols via AppKit (`NSImage(systemSymbolName:)`, template + `colorize`, shown at the symbol's own bounds like `Image(systemName:)`, `Symbols.image-width/height`); menu bar icons are made template images when menu tracking begins (`appkit::keep_menu_icons_template`), so AppKit recolors them for highlight and disabled items; off macOS and as fallback the bundled SVG stand-ins. Coverage: a source scan for symbol names plus exhaustive tests of the Rust symbol tables (`quick_symbol`, `empty_symbol`) | `shots/mac/symbols-light.png`, `symbols-dark.png`, `shots/mac/*`; the AppKit bitmaps' baseline and the menu icon look are unverified without a Mac |
| Accent color from the system | done (UISettings) | done in code (NSColor) | — |
| File associations (HKCU self-registration, Settings card) | done (skipped when `fileAssociationsDisabled`); the C#-era 22 media extensions (`sa_core::media_types::CSHARP_MEDIA_EXTENSIONS`) and the 4 subtitle extensions get the C# tree byte for byte. **Differs** on purpose for the 81 other accepted extensions (`OPEN_WITH_ONLY_MEDIA_EXTENSIONS`): only an `Applications\SubtitleAdder.exe\SupportedTypes` value and a `Capabilities\FileAssociations` value are written (best effort, after the C# tree), and their `Software\Classes\.<ext>` keys are never created, read or deleted. The C#-style registration would write an empty per-user `.<ext>` default and take the default handler of `.vob`, `.mka`, … away from a machine-wide player; and measured on Windows 11 (VLC installed machine-wide, no user choice), even a per-user `.<ext>\OpenWithProgids` entry alone turns the default into the "choose an app" prompt, while the two values keep VLC as the default and list the app as a recommended "Open with" handler. Both values live in trees every version deletes on unregister, so the tree left behind equals C#'s | n/a (Info.plist) | `sa-platform` C# compare (C# tree plus exactly the additions, also when upgrading over the C#-written tree; unregistered tree unchanged), `associations::csharp_era_extensions_are_fully_associated_and_the_rest_are_open_with_only`, `associations::open_with_only_extensions_never_touch_their_classes_keys` |
| Crash log `%TEMP%\SubtitleAdder-crash.log` | done | n/a | — |
| Temp cleanup `%TEMP%\SubtitleAdder` (broker files, other instances, 2 h rule) | done (`app::win_parts::cleanup_temp_directory`, new test) | done (`working_dir::clean_up` at launch / quit) | unit test |

### Main window, editor, player

| Feature | Windows (W) | macOS (M) | Evidence |
|---|---|---|---|
| Toolbar (Video seç, Altyazı aç, language/model/speakers/target/translation-model pickers, Çevir…, Otomatik kaydet, profile, settings, Altyazı üret, Komple çevir) and responsive collapse | done; the left tools keep their natural widths and what does not fit is clipped at the edge (WinUI StackPanel), e.g. `Çevir…` at the default 1240 px with lines; **differs**: when the `Otomatik kaydet` CheckBox holds keyboard focus (after a command-line open), Slint's fluent focus border is drawn over the end of its label (`kaydet` touches the border) instead of around it as in WinUI | done | `shots/e2e/01-start.png`, `07-translated.png`; `shots/main/*`; `shots/picker/before-win-editor.png` vs `win-editor.png` |
| Toolbar translation-model picker (flyout / popover with providers) | done (`main_window::translation_picker`, `ui/win/main/translation_picker.slint`: provider combo that activates the provider, search, 240 px list with the selected model, empty text, 36 px status with the test message, Yenile (ignored while any connection test runs, C# `IsTesting`) / manage → Settings `general`; the flyout focuses the provider combo, which has the ComboBox keys; every close, light dismiss included, calls `Close()` and cancels a running refresh request) | done (`ui/mac/main/translation_picker.slint`: provider pop-up that only browses, focused search with ↑/↓/Return/Esc, check mark + highlight, disabled rows and block reason when offline/unusable, `Modeller yenilenemedi: …`, Yenile only for a provider, manage → Settings `translation`; also from the `»` overflow item); test flags and notices are shared app-wide (`vm.testingProviders` / `vm.providerNotices`): one `ProvidersModel` for every window's picker, which the Settings providers page also reports its tests to (`SettingsDeps::activity`); a finished refresh moves the highlight to the current row | `tests/translation_picker.rs` (both trees), `sa-viewmodels/tests/providers.rs`; `shots/picker/*` |
| Workspaces Player / Platform / Çeviri listesi (count badge) | done | done | E2E shots |
| Empty state, drop target | done | done | `shots/e2e/01-start.png` |
| Editor rows, speaker chips, translation sub-line, stale marker, needs-review marker | done | done | `07-translated.png`, `08-edited.png` |
| Speaker separation default on (`diarizeSpeakers`, absent = true on both; C# `GetBool ?? true`, Swift `as? Bool ?? true`); one diarized speaker still shows the column (`K1` W / `Konuşmacı 1` M, as C# `Speakers.Count > 0` and Swift `!speakers.isEmpty`; the dash rule needs two) | done | done | `vm_settings::defaults_match_macos`, `main_window::a_single_diarized_speaker_still_shows_the_speaker_column` |
| Edit text, Enter commits, undo/redo with C# button titles (`Metni Değiştir İşlemini Geri Al`) | done | done in code (Edit-menu titles) | `08-edited.png`, `09-undone.png` |
| Split / merge / delete / timing ±0.1 s / set to now | done | done | `sa-app` tests |
| Keyboard shortcuts (Space, arrows, M, F, F11, E, Esc, Ctrl+…/⌘…) | done | **differs**: non-Latin keyboard layouts do not fall back to the QWERTY position for M/F/E (Slint passes no hardware key code) | `sa-app` tests |
| Wheel volume on the video | done | **differs**: mouse wheel treated as precise (no notch/momentum info from Slint) | — |
| Player: FFmpeg 8.1 + cpal, first frame, seek, volume, end, every accepted extension | done (replaces Media Foundation; ARCHITECTURE §7) | done in code (replaces AVFoundation) | `sa-player` tests, E2E |
| Accepted media extensions (open dialog, drag and drop, explorer / media index, companion video of a subtitle, stream MIME type) | **differs** on purpose: one list, `sa_core::media_types` (59 video, 44 audio), instead of C#'s and Swift's 14 + 8, so the app opens what VLC commonly opens (`.vob`, `.ogv`, `.mxf`, `.rmvb`, `.mka`, `.ac3`, `.aiff`, …); the former 22 keep their order and come first in each group, the additions follow (companion search: video, then audio, as before, so `X.vob` now wins over `X.wma`). The open dialog's media filter is named `Video ve ses dosyaları` / `Video and audio files` instead of C#'s `*.mp4; *.mov; …` pattern list. The C#-era mime-type fixture still matches; the additions get their own types (`win::tests::every_accepted_extension_has_a_mime_type`) | **differs** the same way; the SwiftUI app's `MediaFileTypes` carries the same list (edited mechanically, not built) | `sa_core::media_types` tests, `sa-player` `every_media_extension_the_apps_accept_opens` (8 added containers) |
| Encrypted SSD chunk cache + buffer bar (`BufferedVideoStream` + VideoCache, nothing cached logged out) | done (`app::win_parts::CachedPlayer`) | n/a | — |
| Rename/delete a playing file | done (share delete) | done | `sa-player` test |
| Subtitle overlay (compose, style, margins) | done | done | `07-translated.png` |
| Transport HUD, display picker, theater, side-by-side, hide panel, full screen | done | done | `shots/main/*` |
| macOS preview fallback (engine `preview`) | n/a | done in code (`PreviewMaker` → engine `preview`), only when FFmpeg fails | — |
| Hardware decoding + GPU YUV→RGB presentation | done (WIP `76dec69`: D3D11VA + OpenGL `BeforeRendering` YUV shader & borrowed texture in `main_window::video_gl`, software fallback; real-time frame-drop/seek/fullscreen measurements pending) | done in code (WIP `76dec69`: VideoToolbox in `sa_player::ffmpeg_backend::hw` + OpenGL YUV shader; unverified on macOS) | `sa-player` planar/hw tests, `sa-app` `main_window` tests |
| HDR tone mapping, deinterlacing, audio-only fallback | **missing** (MF did these) | **missing** (AVFoundation did these) | — |

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
| Logged-out playback records nothing | done (profile reader `win_profile_dump`, C# line format: history unchanged after logged-out opens) | done in code | E2E `dump` check |
| Login with a C# profile: history, views, likes, ratings, PNG thumbnails, lists | done | n/a (LibraryKit format, `sa-profiles::mac` tests) | `03-hub-history.png`, `04-hub-favorites.png` |
| Remember-me opt-in, logout wins, other profiles' events ignored | done (`sa-profiles` tests) | done in code (encrypted `profile-remember-keys.json`, no Keychain) | tests |
| Lists, add-to-list menu, delete, rename (macOS) | done | done; **differs**: list counts include files missing on disk | `shots/hub-*` |
| Hub card like-button pink tint, middle-truncated folder line | n/a | **differs** (no tint; end truncation) | `shots/hub-mac-feed.png` |
| Profile popover closes on a second click of the toolbar button | n/a | unverified | — |
| Login while a video is open: the open video is adopted for the engagement bar (likes, stars, lists; zero counts when the profile has no record of it) without recording a view or progress (C# `OpenVideoProvider` → `AdoptOpenVideoForNewSession`; Swift `playerVideo` + `recordingGeneration`) | done | done | `sa-viewmodels/tests/hub.rs` `login_while_a_video_is_open_…` (fake store now returns no stats for unknown videos, like both real stores) |
| Explorer sidebar: quick locations, transient listing of an opened video's folder, index search only logged in, no `explorerLastFolder` | done | done | `06-editor-video.png` |
| Explorer startup: the launch folder (Videos / Movies) is listed and indexed once at startup, no crawl, whether or not the sidebar is shown and logged in or not (C# `FileExplorerPane.OnLoaded` → `RefreshAsync`: WinUI raises `Loaded` for the Collapsed pane, checked with a WinUI 1.8 probe; Swift `ContentView.task { explorer.start() }`) | done | done | `sa-viewmodels/tests/explorer.rs` `the_launch_folder_is_indexed_at_startup_…` |

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

* Every crate type-checks for macOS: `scripts/check-macos.sh` = `cargo check --target aarch64-apple-darwin
  --workspace --all-targets` (ring's build script overridden, ffmpeg-sys-next bindgen against stand-in libc headers;
  ARCHITECTURE §11). Nothing was linked or run for macOS; the CI job `desktop (macos-26)` is the first real build.
  Objective-C message signatures (`msg_send!`) are only checked at runtime.
* The whole macOS branch of `app::run`: `MacShell::prepare_process`, `BackendSelector` with the winit attributes,
  `UserDefaultsSettingsStore`, `MacProfileStore` (kqueue watcher, flock, fsync), `MacMediaIndex` with the
  CoreFoundation lister, `MacProviderStore` + `KeyFileProtector` (0600), the Apple Event open-documents handler,
  `run_event_loop_until_quit` (app alive without windows), ⌘N multi-window.
* Menu bar (Slint `MenuBar` → NSMenu), standard app-menu roles, Edit-menu undo titles, ⌘ shortcuts.
* SF Symbols rasterizer output (size/baseline of the AppKit bitmaps, menu icon look), Dock reopen, NSAlert sheets, NSColorPanel, NSSavePanel/NSOpenPanel via rfd.
* FFmpeg + CoreAudio playback, the preview fallback, AV1/WMV decoders in the macOS FFmpeg build (Homebrew).
* Window frame restore (`mainWindowFrame`), native full screen tracking, traffic-light placement, toolbar drag and
  `AppleActionOnDoubleClick`.
* Foundation-specific text rules: `localizedCaseInsensitiveCompare` sorts, JSONEncoder key order, number formats,
  ISO-8601 date forms, grapheme-cluster cutting of the diagnostic log.
* Thumbnail generation through AVAssetImageGenerator (384×216 JPEG).
* `scripts/package-desktop-macos.sh` (signing, notarization, dylib rewrites).

## Other known gaps

* FFmpeg build licensing (Windows): `stage.ps1` and `package-desktop-windows.ps1` put the BtbN **LGPL** shared build
  (`engine\scripts\fetch-runtime.ps1 -Only ffmpeg-lgpl` → `engine\.runtime\ffmpeg-lgpl`, same autobuild as
  the engine's GPL SDK) next to `SubtitleAdder.exe`; the engine folder keeps its GPL DLLs (libx264 for `preview`).
  The app still compiles against the GPL SDK headers/import libraries (identical API and DLL names). Both scripts
  run `SubtitleAdder.exe --probe` on a PATH without FFmpeg and require `ffmpeg license: LGPL…`. Next to both DLL
  sets the build's own `LICENSE.txt` ships as `FFmpeg-LICENSE.txt` with an `FFmpeg-NOTICE.txt` (what the DLLs are,
  where their source and build scripts are; `engine\scripts\ffmpeg-notice.ps1`). The BtbN builds use
  `--enable-version3`, so the app's DLLs are LGPL **v3** or later. **Open:** the LGPL asset (80.4 MB) is not
  downloaded yet, so its SHA-256 is not pinned (`@LGPL_SHA256@` in `fetch-runtime.ps1`): the package script stops
  with a clear error, `stage.ps1` falls back to the GPL DLLs with a warning for local runs, and playback with the LGPL
  DLLs is not verified. macOS: `FFMPEG_LGPL_DIR` names the LGPL dylibs the player bundles (FFMPEG_DIR stays the
  engine's GPL build and the build headers); without it the package bundles FFMPEG_DIR's and warns. Its license files
  ship in `Contents/Resources/FFmpeg` (the engine's next to its dylibs).
* `rustfmt`/`clippy` are not installed in this toolchain; nothing was linted.
* `scripts/check-macos.sh` type-checks the macOS code from Windows/Linux (a local tool; CI builds and tests on a real
  macOS runner).
