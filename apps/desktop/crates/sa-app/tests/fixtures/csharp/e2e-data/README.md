# C#-written end-to-end data (frozen)

The Windows data folder the end-to-end run (`scripts/e2e-windows.ps1`) starts from, written by the C# app's own
classes, plus what the C# reader printed for it. Frozen once from the C# app at commit `2f4bc96` (`apps/windows`,
since removed) so neither the e2e scripts nor the Rust tests need .NET.

| File | What it is |
| --- | --- |
| `data/profiles.json`, `data/profiles.db` | `LocalProfileManager`: profile `Emre` (password `e2e-parola-1`), history, a like, ratings, two PNG thumbnails, the list `İzlenecekler` |
| `data/settings.json` | `JsonFileSettingsStore`: file associations off, Turkish UI/source, `base` + `madlad-3b` → `en`, explorer hidden, light theme |
| `data/media-index.json` | `MediaLibraryIndex.ScanDirectoryAsync` of the clip folder |
| `data/translation-providers.json` | `TranslationProviderManager.AddProvider`: one custom provider `C# anahtarlı sağlayıcı` on `https://127.0.0.1:9/v1` with the plaintext key `sk-e2e-csharp-0123456789abcdef` (never contacted) |
| `csharp-dump.txt` | the C# reader's output for `data/` (`E2EData dump <data> Emre e2e-parola-1`, CRLF as printed) |
| `media.sha256` | SHA-256 of the clips and thumbnails the C# tool was given (the profile rows are keyed by the clips' content fingerprints, the thumbnails are stored verbatim) |

`.gitattributes` keeps every byte as written (no line-ending conversion).

## How it was produced

On Windows 11 with the .NET 10 SDK, from `apps/desktop` at the commit above:

1. `scripts/e2e-data.ps1 -Root C:\SubtitleAdderE2E\template` as it was then: it generated the clips (Turkish SAPI TTS
   clip `altyazi-testi.mp4`, ffmpeg test patterns `belgesel-uzay.mp4` and `yaz-tatili.mp4`, PNG thumbnails of the last
   two, ffmpeg from `engine/.runtime/ffmpeg`) and ran the C# tool `tools/fixtures/E2EData` (`make <data> <media>`), which
   compiled the `apps/windows` `SubtitleAdder.Core` and `EngineKit` sources.
2. `E2EData dump C:\SubtitleAdderE2E\template\data Emre e2e-parola-1 > csharp-dump.txt`.
3. `data/*`, `csharp-dump.txt` and the hashes of `media/` were copied here.

Nothing in the data is random except what the C# classes generate themselves (salt, provider id, timestamps of
2026-11-08); every path in it is under `C:\SubtitleAdderE2E\template\media`. Two runs of the clip generation on the
same machine gave identical files.

## Who uses it

* `scripts/e2e-data.ps1` regenerates the clips, checks them against `media.sha256` (a warning when an other ffmpeg
  build or TTS voice changes them), sets their modification times back to the ones in `media-index.json` and copies
  `data/` next to them. `-Root C:\SubtitleAdderE2E\template` (`e2e-windows.ps1 -Root C:\SubtitleAdderE2E`) reproduces
  the original folder exactly.
* `scripts/e2e-windows.ps1` runs the app on a copy, then reads the copy back with the Rust profile reader
  (`crates/sa-profiles/examples/win_profile_dump.rs`, same line format as `csharp-dump.txt`).
* `tests/csharp_e2e_data.rs` reads this folder through the Rust stores: the Rust reader must print `csharp-dump.txt`
  byte for byte (line endings aside), the profile, thumbnails (hashes), list, settings, media index and provider read
  back as written, and data the Rust stores rewrite (login/logout cycle, provider key moved into the DPAPI sidecar)
  keeps the C# file shapes.

## No longer verifiable without .NET

* That the C# app reads back what the Rust app wrote during the e2e run (the former final check ran the C# reader on
  the run's data folder; it is now the Rust reader, whose C# compatibility rests on the frozen fixtures of
  `sa-profiles`, `sa-providers`, `sa-media` and `sa-core`).
* New C# data for changed inputs (other clips, another profile): this folder can only be read, not regenerated.
