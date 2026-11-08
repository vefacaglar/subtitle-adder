# Frozen C# outputs for `tests/csharp_compat.rs`

These files are what the C# Windows app's own code returned for the inputs `tests/csharp_compat.rs` generates. They
were captured once, from the C# sources at commit **2f4bc96** (`apps/windows/src/SubtitleAdder.Core`,
`apps/windows/src/EngineKit`, with the String Catalogs of `apps/macos/Sources/{SubtitleAdderApp,EngineKit}/Resources`
and `apps/windows/src/SubtitleAdder.Core/Strings/Windows.xcstrings` embedded), on Windows 11 with .NET 10 and the
real `tr-TR` / `en-US` cultures. The C# app and the fixture tool that drove it (`tools/fixtures/CoreCompat`, a console
program compiling those sources read-only) have since been removed, so these files cannot be regenerated.

How they were produced: the previous version of `csharp_compat.rs` ran CoreCompat for each input set
(`SA_DOTNET_FIXTURES=1 cargo test -p sa-core --test csharp_compat -- --test-threads=1`, all 14 tests passing against
the live C# code) and saved the tool's stdout unchanged. Two captures gave identical files apart from the temp
folder and the log timestamp noted below. Every `.jsonl` line is one JSON array, as the tool printed it (C# doubles
as `"R"` strings, decoded text as UTF-16 code units in hex).

| File | C# code | Input |
| --- | --- | --- |
| `labels.jsonl` | `ModelLabel`, `ModelRow`, `SubtitleLanguage`, theme/background/display titles, `AppearanceSettings` percent texts, undo titles, catalog totals (tr and en) | the C# `ModelCatalog.FallbackModels` (plain, downloaded, downloaded with 1234 MB) and `custom-x` |
| `sizes.jsonl` | `ModelLabel.SizeText` (tr, en) | MB −5…20000, 99900…100100, 999900…1000100 |
| `bytes.jsonl` | `SubtitleViewModel.FormatByteSize` under tr-TR and en-US | listed values + `Lcg(42)` |
| `times.jsonl` | `SubtitleTime.Short` | listed values + `Lcg(7)` |
| `hex.jsonl`, `style.jsonl` | `HexColor.Parse/ParseRgb/Format`, `OverlayStyle` | listed strings / doubles |
| `parse.jsonl`, `decode-subtitles.jsonl` | `SubtitleParser.ParseFile`, `SubtitleParser.DecodeBytes` | `subtitle_inputs()`; FNV-1a hashes of the files in `subtitle-inputs.jsonl` |
| `atomic-read.jsonl`, `decode-encodings.jsonl` | `AtomicFileStore.ReadAllTextShared`, `SubtitleParser.DecodeBytes` | the encoding files; hashes in `encoding-inputs.jsonl` |
| `redact.jsonl` | `DiagnosticLog.Redact` (output only, in input order) | listed strings |
| `paths.jsonl` | `IsSubtitleFile`, `FindSidecarSubtitle`, `FindCompanionVideo` | listed paths; the C# temp media folder is replaced by `<BASE>` |
| `settings-read.jsonl` | `JsonFileSettingsStore` typed reads of every key | the settings.json literal in the test |
| `settings-ops.jsonl` | `InMemorySettingsStore` typed writes, reads and `Changed` count | listed ops |
| `settings-csharp-written.json` | the file `JsonFileSettingsStore` wrote (`settings-write`) | fixed values in the tool |
| `settings-rust-written.json` | the file the Rust port wrote on top of the C# one at capture time | the writes in the test |
| `settings-rust-written.csharp-read.jsonl` | C# typed reads of `settings-rust-written.json` | — |
| `undo.jsonl` | `UndoStack` state after every op | `Lcg(2026)` script of 4000 ops |
| `scrub/` | the seeded log folder after `DiagnosticLog.Initialize()` (startup scrub) and one `Write("t", "x C:\a\b.mp4")` | the seed files in the test; the last line of `SubtitleAdder-diagnostics.log` carries the capture's timestamp and thread number, so only its shape is compared |

`.gitattributes` keeps every file byte for byte (`-text`): the logs use CRLF and the settings files are compared
exactly.

## What the frozen files can no longer verify

* Rust → C# reads beyond the frozen file: the C# store read `settings-rust-written.json` once. The test pins the
  Rust writer to exactly those bytes; a deliberate change to the Rust settings format can no longer be checked
  against the C# reader.
* New inputs: anything added to the test inputs (or a new fallback-catalog model) has no C# output; the tests fail
  on such a change instead of comparing nothing, and new cases need hand-checked expectations in the unit tests.
* The startup scrub ran against the C# code once; the time stamp and thread id of the appended C# line are frozen.
