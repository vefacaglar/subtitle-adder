# Frozen C# player behavior

Golden data for `tests/csharp_compat.rs`. It was produced on 2026-11-08 (Windows, .NET 10.0.203) by the C# fixture
tool `apps/desktop/tools/fixtures/PlayerCompat`, which compiled the C# app's own
`apps/windows/src/SubtitleAdder.Core/PlaybackTimeline.cs` (`PlaybackTimeline` and `PlayerSessionGate`) read-only at
commit **2f4bc96** (`apps/windows` was unchanged from that commit when this was captured). The tool and the C# app have
since been removed; this data cannot be regenerated, only kept.

| Files | What |
| --- | --- |
| `<name>.script` | The operation script handed to the tool, exactly as `csharp_compat.rs` generates it (one op per line). |
| `<name>.out.jsonl` | The tool's stdout for that script: one JSON array per op, the op's result and the full state after it. Doubles are IEEE-754 bit patterns (decimal `i64`). |

* `timeline-<seed>`: 4000 ops each on `PlaybackTimeline` (`t-reset`, `t-seek`, `t-observe`, `fmt`, `clamp`); seeds
  `9e3779b97f4a7c15`, `2a`, `76adf1`, `deadbeef` (hex) of the test's xorshift64* generator. Times are milliseconds after
  a fixed epoch.
* `gate-<seed>`: 5000 ops each on `PlayerSessionGate`; seeds `1`, `123456789abcdef0`, `18697`, `c0ffee00` (hex).
* `gate-late-event`: the scripted case of the C# test
  `LocalProfileTests.PlayerSessionGate_RejectsLateEventFromPreviousSourceAfterUIThreadSwap`.

Op grammar (from the tool): generation operands are a number, `L` (LoadGeneration), `L-1` or `A` (ActiveGeneration);
source operands are `0` (null) or an id naming one shared object per id. Gate state = load generation, active
generation, active source id (0 = null), opened, failed, error.

How it was captured: the test's `csharp()` helper ran the built tool (`dotnet build -c Release`) on every generated
script and wrote both files; a second run of the tool on the same scripts gave byte-identical output (the C# code is
deterministic for a fixed script). The test asserts that every `.script` still equals the generated script, so a
changed generator fails instead of comparing against output for other operations.

`.gitattributes` keeps every file byte for byte (no line-ending conversion).
