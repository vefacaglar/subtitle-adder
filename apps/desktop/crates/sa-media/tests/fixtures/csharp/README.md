# Frozen C# media fixtures

Golden outputs of the C# Windows app's own `MediaLibraryIndex`, `EncryptedVideoCacheStore`, `BufferedVideoStream`,
`LocalProfileManager` and `IndexedMediaFile`, read by `crates/sa-media/tests/csharp_compat.rs`. The C# app has been
removed; these files are what is left of it for the media area, and they must not be regenerated.

## How they were produced

The fixture tool `apps/desktop/tools/fixtures/MediaCompat` (since deleted) compiled the C# sources of
`apps/windows/src/SubtitleAdder.Core` and `apps/windows/src/EngineKit` read-only, as of commit **2f4bc96** (the C#
sources were unchanged from there up to the deletion), with .NET 10 SDK 10.0.203 on Windows 11, ICU globalization on.
A one-off Rust test ran the tool once per scenario (the same scenarios the old opt-in `SA_DOTNET_FIXTURES=1` tests
ran live) and wrote its stdout and files here:

| File | Tool command | Contents |
| --- | --- | --- |
| `index-session.media-index.json` | `index-write <dir>` | the C#-written index of a scan + depth-2 crawl of a small tree (all files stamped 2026-11-01T08:12:44.1234567Z) |
| `index-session.dumps.json` | `index-dump <index> <query>` | `csharp_written`: C#'s view of that file per query; `rust_written`: C#'s view, at freeze time, of the file the Rust port rewrote after the test's tree changes (new files stamped tick 638000000000000000) |
| `script-session.script.txt` | (input) | the media-index script (ops documented in `csharp_compat.rs`) |
| `script-session.out.txt`, `script-session.media-index.json` | `script <dir> <script>` | C#'s printed state and its final index file bytes |
| `odd-index.media-index.json` | (input) | a hand-written index with odd entries (base folder `C:\SaMediaFixture\odd`, which need not exist) |
| `odd-index.dumps.json` | `index-dump <index> <query>` | C#'s view of it per query |
| `identity.json` | `identity <path> <len> <cs>` | `ComputeVideoIdentity` for non-ASCII paths |
| `probe.json` | `probe` | one line per path: the path, `GetPathRoot` and `NormalizeDirectoryPath`, separated by a vertical bar |
| `texts.json` | `format-size`, `mime`, `badge` | `FormatFileSize`, `GetMimeType`, `SidecarBadge` (`null` = no badge / null sidecar) per input |
| `stream-traces.json` | `stream-trace <dir> <len> <cs> <maxCached> <ops>` | the printed trace of three playback scenarios |
| `cache/cache.json`, `cache/v3/*.vcache` | `cache-write <dir>` | profile `alice` (password `pass123`, fresh random vault key, recorded as `vault_key_hex`), the video `video.bin` (`Pattern(3*65536+1234)`, seed 19) played through the C# stream into the C# cache: its path, identity and the four encrypted chunk files |

Placeholders: `{ROOT}` stands for the temporary folder that held `tree\` and `media-index.json` (the tests substitute
their own temp folder, JSON-escaped inside the index files; it must be plain ASCII, which `%TEMP%` normally is).
Search also matches a file's folder path, so the frozen search results depend on which queries the freeze-time folder
contained: for the script session `S` and `i` (none of the others), for the index session none. The tests assert their
own folder answers the same (the script test names its folder `sa-script-i-*` for that).
`{CWD}` in `probe.json` is the process's current directory (`NormalizeDirectoryPath("C:")` resolves the drive-relative
path against it). The cache video path is a real temp path from the freeze run; nothing reads that file again, the
identity is only an input of the chunk names and keys.

The index files are CRLF (System.Text.Json indented output on Windows) and are compared byte for byte, hence the
`-text` `.gitattributes` here. Git does not keep modification times, so the 2001-01-01 fixed time of the C# chunk files
was verified once at freeze time and is now checked only on chunks the Rust port writes.
