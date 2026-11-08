# Frozen C# `StateSyncBroker` transcripts

Golden data recorded from the C# app's real `StateSyncBroker` (`apps/windows/src/SubtitleAdder.Core/StateSyncBroker.cs`
at commit `2f4bc96`), so `tests/csharp_golden.rs` can check wire, file and parsing compatibility without .NET. The C#
app and the `tools/fixtures/BrokerInterop` driver that produced this data have been removed. This data cannot be
re-recorded; treat it as read-only.

## How it was produced

`BrokerInterop` was a .NET 10 console app. It compiled the real `StateSyncBroker.cs` against an in-memory stand-in for
`SqliteProfileStore` (synchronous commit, `processed_requests` idempotency) and was driven over stdin/stdout by a
one-off Rust recorder test. The recorder sent `CS-*` commands and printed `CS-*` lines. It connected the Rust
`NamedPipeBus`, raw pipe clients and a raw spy pipe server (`sa_sync::win_testing::{RawClient, RawServer}`) to the C#
broker and saved every byte on the pipe. Inputs were deterministic: fixed event and request ids, timestamps and DB paths
(`C:\sa-sync-golden\*.db`, never opened), and the values defined in `csharp_golden.rs` (`rich_event`, `rich_request`,
`resend_requests`, ...). The only values left random are the ones STJ generates itself: `eventId`, `requestId` and
`timestampUtc` when they are missing from an input. The tests mask these. The test scopes (`golden-*-<uuid>`) are
random too, but they appear only as file-name hashes.

Wire lines are stored as JSON strings holding the exact UTF-8 text on the pipe, terminator included (C# writes
`line + "\n"`). Arbitrary byte inputs are stored as base64.

| File | Content |
| --- | --- |
| `stj-roundtrip.json` | STJ re-serialization (`JsonSerializer.Serialize(Deserialize(x))`, the broker's `JsonOptions`) of the Rust-written rich event, the rich request, the minimal request and five acks. |
| `stj-parity.json` | 154 edge-case lines (events, `W:` requests, `A:` acks). Each holds what STJ made of the input (`csharp`, `null` = dropped), the Rust output at recording time (`rust`), and what STJ made of that Rust output (`csharp_of_rust`). |
| `write-kind-try-parse.json` | `Enum.TryParse<WriteKind>(ignoreCase)` for 26 inputs, and the 16 C# ordinals by name. |
| `ordinal-ignore-case.json` | `string.Equals(a, b, OrdinalIgnoreCase)` for 12 tricky pairs. |
| `stream-reader-lines.json` | `StreamReader(UTF8, detectEncodingFromByteOrderMarks: false, 4096).ReadLine` lines plus `IsNullOrWhiteSpace` for 26 byte streams (BOMs, invalid UTF-8, CR/LF at the 4096-byte buffer edge, ...). |
| `default-scope.json` | SHA-256 of .NET's `$"{UserDomainName}\{UserName}"` and of the recording account's user SID. Only hashes are kept, so no account names end up in the repository. |
| `csharp-host-transcript.json` | The C# broker hosting. It holds its engine lock and slot file names (scope hash as `{hash}`), sizes and the Win32 error another opener got (32 = sharing violation). It also holds the pipe's SDDL with the token SIDs as `{owner}`/`{user}`, and the bytes the C# host wrote to a Rust client for each step: the relayed Rust event, the C# client's event, `A:` acks for a committed, duplicate and unknown-kind `W:`, the post-commit broadcast, and 24 resent `W:` lines with their acks. Each step also keeps the C# stdout (`CS-RECV`/`CS-EXEC`, the order the C# store executed requests in). |
| `csharp-client-transcript.json` | The C# broker as a client. `spy` holds what it sent to a raw spy host while the test held the engine lock: nothing on connect, then its own event line. It also holds how it read a Rust-written event line and its exit (`closed=true`). `rust_host` covers the C# client on the real Rust host: the Rust pipe SDDL it accepted, the lines the Rust host relayed, the `W:` lines in the C# broker's own format (`"W:" + Serialize(request)`, sent by `rawwrite`) and the acks as C# read them. |

## What the tests no longer verify

These checks needed a live C# process. They are now covered only by the frozen observations above:

- that the C# host counts the Rust slot file and session (`count=2 clients=2`), and that the C# client counts the Rust
  host's slot. The Rust slot and lock files still use the C# names and open flags (`FileShare.None`, `DeleteOnClose`).
- .NET's `CurrentUserOnly` owner check accepting the Rust pipe. The tests only check that the Rust descriptor still
  equals the frozen one C# accepted.
- the C# engine's own takeover timing (lock polling) when a Rust host exits.
- parsing and acceptance of any input not in the frozen lists: new edge cases cannot be added without C#.
- the default scope on any account other than the recording one (the test skips there).
