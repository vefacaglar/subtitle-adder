# Frozen C# provider behavior and data

Golden data for `tests/csharp_behavior.rs` and `tests/csharp_compat.rs`. It was produced on 2026-11-08 (Windows,
.NET 10.0.203) by two C# fixture tools that compiled the C# app's sources read-only at commit **2f4bc96**
(`apps/windows` was unchanged from that commit when this was captured). The tools and the C# app have since been
removed; this data cannot be regenerated, only kept.

## `ProvidersCompat` (behavior, `csharp_behavior.rs`)

The tool compiled `apps/windows/src/SubtitleAdder.Core/*.cs` and `apps/windows/src/EngineKit/*.cs` (with the macOS
`Localizable.xcstrings` and `Windows.xcstrings` as embedded resources) and patched one line so that
`TranslationProviderManager.Shared` lived in a temp folder with a private broker (never the user's real data). UI
language started as Turkish.

| Files | What |
| --- | --- |
| `<name>.in.jsonl` | The exact input the test generates (and the tool was given). |
| `<name>.out.jsonl` | The tool's stdout for it: one JSON array per line. |

* `pure`: `ProvidersCompat pure <file>`, one `[op, args...]` per line (`endpoints`, `chat`, `opencode`, `cloud`,
  `normalize`, `merge`, `parse`, `mask`, `available`, `picker`, `error`, `preset`); output `[line number, result]`.
* `load`: `ProvidersCompat load <dir>` over a folder holding every case of `load_cases()`; the input file lists that
  folder as `[file name, hex bytes]` lines (the tool loaded the files in ordinal name order, which is this order).
  Output `[name, active index | null, [[id | "*", name, presetId, baseUrl, apiKey, model, async, models]...]]` or
  `[name, "crash", exception type]` (`*` = a fresh random id the file did not contain).
* `scenario-*`: `ProvidersCompat scenario <file>` against a real `TranslationProviderManager` + `SubtitleViewModel`
  with a scripted `HttpMessageHandler` (no network); one state line per printing op. `editing`, `lan-bridge`,
  `lan-startup`, `lan-startup-inactive`, `test-tr`, `test-en`, `test-addresses`, `default-names-en` are the
  `run_scenario` calls of the test in order.

Nondeterminism in the frozen output, all neutralized by the test: the result of a `file` op is the C# file text with the
random ids of added providers (the test masks every UUID and scrubs `apiKey` values before comparing); elapsed times in
connection-test messages were already written as `· N ms ·` by the tool; the opencode session header is written as
`<session>`. A second capture run differed only in those random UUIDs.

## `ProvidersFixture` (data, `csharp_compat.rs`)

The tool serialized and deserialized `translation-providers.json` with the C# app's record shape and
`JsonSerializerOptions` (`WriteIndented`, camelCase, case-insensitive; .NET writes CRLF on Windows).

* `csharp-translation-providers.json`: `ProvidersFixture write` (UTF-8 without BOM, as `AtomicFileStore` writes it),
  fixed ids and a plaintext key. Also case `01-csharp` of `load`.
* `rust-rewritten.json`: the file the Rust app rewrote from it (key scrubbed); `rust-rewritten.csharp-read.txt` is
  `ProvidersFixture read` of it (`active=<id>`, then `id|name|presetId|baseUrl|apiKey|model|async|models` per provider).
* `rust-groq.json`: the file the Rust app wrote after adding a Groq provider (its id is the random one of the capture
  run); `rust-groq.csharp-read.txt` is `ProvidersFixture read` of it. The test compares a new Rust-written file with it
  after swapping in the new id.

How it was captured: the tests' helpers ran the built tools (`dotnet build -c Release`) on every generated input and
wrote the files. The tests assert that every `.in.jsonl` still equals the generated input and that the Rust-written files
equal the frozen ones byte for byte, so the frozen C# reads still apply.

`.gitattributes` keeps every file byte for byte (no line-ending conversion; the C#-written JSON has CRLF).
