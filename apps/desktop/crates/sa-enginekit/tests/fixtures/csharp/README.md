# Frozen C# EngineKit outputs

Golden data for `tests/csharp_compat.rs`. Each file holds what the C# Windows app's own EngineKit
(`apps/windows/src/EngineKit/*.cs` at commit `2f4bc96`, compiled together with the macOS
`EngineKit/Resources/Localizable.xcstrings` string catalog for the localized client messages) returned for a
fixed set of inputs. The C# app has since been removed; these files are the only record of its behavior, so
they are not regenerated. Never edit them by hand.

How they were produced (2026-11-08, .NET 10, real cultures, `InvariantGlobalization=false`): the former fixture
tool `apps/desktop/tools/fixtures/EngineKitCompat` (a console program linking those C# sources read-only) was fed
the inputs that `tests/csharp_compat.rs` generated at commit `a39f020` (SplitMix64 generator, fixed seeds
`0x5EED_0001`..`0x5EED_0003`; hand-written lines and scripts), and every stdout line was stored verbatim.

| File | Rows | Input | C# output (`csharp`, the tool's raw stdout line) |
| --- | --- | --- | --- |
| `edit.jsonl.gz` | 8003 | `case`: sentence list + edit operations (8000 generated from seed `0x5EED_0001`, then 3 hand-picked) | `SentenceEditing` results per operation, the final list encoded by `EngineJson` (engine stdin payload) and where each final uid came from |
| `tracker.jsonl` | 1000 | `case`: `DownloadPhaseTracker` init flags + `(stage, fraction)` events (seed `0x5EED_0002`) | the label `Update` returned per event |
| `decode.jsonl` | 54 | `line`: one engine stdout line | `EngineEvent.Decode` result (or `unknown` / `invalid`) |
| `graphemes.jsonl` | 3000 | `text` (seed `0x5EED_0003`) | `StringInfo.LengthInTextElements` (a number) |
| `client.jsonl` | 102 | `op`, `lang`, `extra` (sentences / model id) and the `sa-fake-engine` `script` | the real C# `EngineClient` outcome (value, or `EngineException` kind + message) and stream events, run against `sa-fake-engine` with `Offline = true` |

`edit.jsonl.gz` is gzip (level 9, mtime 0) of a 13 MB JSON-lines file. Its `case` values are still regenerated
by the test from the same seed, because serde_json's default float parsing is not always correctly rounded;
the frozen `case` text must equal the regenerated case's serialization byte for byte, which proves the generator
did not drift.
