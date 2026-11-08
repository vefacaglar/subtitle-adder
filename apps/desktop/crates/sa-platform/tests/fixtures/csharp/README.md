# C# golden data for sa-platform

Frozen output of the C# WinUI app's platform code at commit `2f4bc96`, read by `tests/csharp_compat.rs` instead of
running .NET. It was produced on Windows 11 (10.0.26300), .NET 10.0.7 (ICU globalization, user culture `tr-TR`), by
the former fixture tool `apps/desktop/tools/fixtures/PlatformCompat` before the C# app was removed. That tool was a
.NET 10 console app holding verbatim copies of `SingleInstanceCoordinator.cs` (client and server bodies, with the pipe
name as a parameter), `FileAssociationRegistrar.cs` (the exe and icon as parameters, no shell notification, HKCU
redirected to a scratch subtree with `RegOverridePredefKey`), `SystemFonts.cs`, `MediaLibraryIndex.NormalizeDirectoryPath`
and `LocalProfileManager.IsDriveRootReady`. A one-off Rust recorder test drove it, connected the Rust pipe server and
raw pipe clients and servers to it, and wrote these files. The data cannot be re-recorded. Treat it as read-only.

Placeholders keep machine-specific values out of the files. The tests substitute them:

| Placeholder | Meaning |
| --- | --- |
| `{dir}` | A fresh temp folder holding `files` and `directories` from `single-instance.json` |
| `{owner}` / `{user}` | The token owner SID / user SID in an SDDL string (the `O:` field is the owner; ACE trustees prefer `{user}`) |
| `{cwd}` / `{parent}` / `{drive}` | The test's working directory (the crate folder, as for the recording), its parent, its drive (`C:`) |
| `{missing}` | A drive letter that does not exist (`DriveInfo.IsReady` false) |
| `{tmp}` / `{tmp_root}` | A fresh temp folder that is never created as a file, and its drive root |

| File | What it is |
| --- | --- |
| `single-instance.json` | `names`: `SanitizeToken` and the mutex and pipe names for 21 user names. `csharp_client`: the bytes the .NET `CurrentUserOnly` client wrote for four argument lists, captured by a raw spy pipe server. `csharp_client_without_server`: the client's result with nobody listening. `csharp_server`: what the .NET server (`StreamReader(UTF8)` + `Trim().Trim('"')` + `File.Exists` + `GetFullPath`) made of 14 payloads: the four client payloads plus ten edge cases (BOM, CR/LF/CRLF, quotes, Unicode whitespace, invalid UTF-8, `.`/`..`, trailing dots, a 5000-character line, folders). Payloads are lists of text segments and `{"hex": ...}` raw bytes. `rust_server_sddl`: the Rust pipe's security descriptor that the .NET client accepted (it forwarded files to the Rust server in the same run). `csharp_server_sddl`: the .NET server pipe's descriptor. |
| `file-associations.json` | `Register` / `Unregister` / `IsRegistered` with and without an explicit icon, on a scratch HKCU subtree where `.mkv` already belongs to another app. The trees are in the tool's dump format: `key\|` per key and `key\|name\|kind\|data` per value, relative to the subtree, ordinal order. `registered_sizes` holds the raw byte size of each value as written by .NET (REG_SZ with its terminating NUL), read by the Rust dumper the tests use. That dumper's view of the C# tree equalled the C# dump. Registering twice left the same tree. |
| `fonts.json` | The `SystemFonts` rules on fixed names, not this machine's font list: `inputs` (95 names in enumeration order, with case duplicates, an empty name and vertical `@` names) put into `SortedSet<string>(StringComparer.CurrentCultureIgnoreCase)` with the same empty and `@` filter, under `en-US`, `tr-TR`, `de-DE`, `sv-SE`, `ja-JP` and the invariant culture (`""`). `compare` holds the sign of `CurrentCultureIgnoreCase.Compare` for each of 40 `pairs` per culture. `fallback` is the list used without DirectWrite. |
| `paths.json` | `Path.GetPathRoot`, `Path.HasExtension`, `Path.GetFileNameWithoutExtension`, `NormalizeDirectoryPath` and the drive-root readiness rule for 31 inputs. A `null` from .NET is stored as `""`. |

At recording time the live DirectWrite family list of the recording machine (293 families, `tr-TR` order) was
identical in C# and Rust. That list is not stored because it depends on the machine.

Observations from the recording that the tests do not assert:

- The .NET server also decodes a UTF-16 LE payload with a BOM (`StreamReader` detects BOMs) and accepted the file in
  it. The Rust server reads payloads as UTF-8 only and gets no file. No client sends UTF-16, so this payload is not
  part of `csharp_server`.
- `CurrentCultureIgnoreCase` keeps width (`Ａ`/`A`, `ｱ`/`ア`) and kana-type (`あ`/`ア`) differences. The Rust ICU
  collator at secondary strength treats them as equal. These three pairs are listed in the test's
  `KNOWN_FONT_ORDER_DIVERGENCES`, which also fails once they start to match.
