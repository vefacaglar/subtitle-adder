//! Data compatibility with the C# app (Windows): `media-index.json` and the encrypted `VideoCache` chunks are written
//! by the real C# `MediaLibraryIndex` / `EncryptedVideoCacheStore` / `BufferedVideoStream` (fixture tool
//! `apps/desktop/tools/fixtures/MediaCompat`, which compiles the C# app's own sources read-only) and read by the Rust
//! port, and the other way round.
//!
//! Opt-in: `SA_DOTNET_FIXTURES=1 cargo test -p sa-media --test csharp_compat` (needs the .NET 10 SDK; the tool is built
//! on first use with `dotnet build -c Release`). Without the variable every test returns early.
#![cfg(windows)]

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use sa_core::cancel::CancellationToken;
use sa_core::secrets::SecretKey32;
use sa_media::win::{BufferedVideoStream, VaultKeyProvider, VideoCacheStore, WinMediaIndex};
use sa_media::{MediaIndex, MediaIndexDocument, ScanResult};
use sha2::{Digest, Sha256};

fn enabled() -> bool {
    let on = std::env::var("SA_DOTNET_FIXTURES").is_ok_and(|v| v == "1");
    if !on {
        eprintln!("skipped: set SA_DOTNET_FIXTURES=1 to run the C# compatibility tests");
    }
    on
}

fn tool_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/fixtures/MediaCompat")
}

fn tool_exe() -> PathBuf {
    let exe = tool_dir().join("bin/Release/net10.0/MediaCompat.exe");
    let built = std::fs::metadata(&exe).and_then(|m| m.modified());
    let stale = ["Program.cs", "MediaCompat.csproj"].iter().any(|source| {
        match (&built, std::fs::metadata(tool_dir().join(source)).and_then(|m| m.modified())) {
            (Ok(built), Ok(source)) => *built < source,
            _ => true,
        }
    });
    if stale {
        let status = Command::new("dotnet")
            .args(["build", "-c", "Release", "-nologo", "-v", "q"])
            .current_dir(tool_dir())
            .status()
            .expect("dotnet is on PATH");
        assert!(status.success(), "building the MediaCompat fixture tool failed");
    }
    exe
}

/// Runs the C# tool and returns its stdout lines (UTF-8).
fn csharp(args: &[&str]) -> Vec<String> {
    let out = Command::new(tool_exe()).args(args).output().expect("MediaCompat runs");
    assert!(
        out.status.success(),
        "MediaCompat {args:?} failed: {}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("UTF-8 output").lines().map(str::to_owned).collect()
}

fn dotnet_bool(b: bool) -> &'static str {
    if b { "True" } else { "False" }
}

/// The Rust index state in the format of the tool's `index-dump`.
fn rust_dump(index: &WinMediaIndex, query: &str) -> Vec<String> {
    let mut lines = vec![format!("total|{}", index.total_indexed_files())];
    for d in index.indexed_directories() {
        lines.push(format!("dir|{d}"));
        for s in index.cached_subdirectories(&d) {
            lines.push(format!("sub|{s}"));
        }
        for f in index.cached_directory(&d) {
            lines.push(format!(
                "file|{}|{}|{}|{}|{}|{}|{}|{}",
                f.full_path,
                f.file_name,
                f.directory_path,
                f.size_bytes,
                f.last_write_time_utc,
                dotnet_bool(f.has_sidecar_subtitle),
                f.sidecar_extension.as_deref().unwrap_or("null"),
                f.indexed_at_utc
            ));
        }
    }
    for f in index.search(query, 500) {
        lines.push(format!("search|{}", f.full_path));
    }
    lines
}

fn tok() -> CancellationToken {
    CancellationToken::new()
}

#[test]
fn a_csharp_media_index_reads_identically_in_rust_and_a_rust_index_reads_identically_in_csharp() {
    if !enabled() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_str().unwrap().to_owned();
    let index_file = tmp.path().join("media-index.json");
    let file_arg = index_file.to_str().unwrap().to_owned();

    // C# writes: a scan and a depth-2 crawl of a tree with sidecars, non-ASCII and JSON-escaped names.
    assert_eq!(csharp(&["index-write", &dir]), ["5|4"]);
    let written = std::fs::read_to_string(&index_file).unwrap();
    assert!(written.contains("\\u00E9") && written.contains("\\u002B"), "the C# file escapes non-ASCII and '+'");

    for query in ["", "s01", "vid\u{e9}o", "I\u{15f}IK"] {
        let csharp_view = csharp(&["index-dump", &file_arg, query]);
        assert_eq!(csharp_view[0], "total|5");
        assert_eq!(csharp_view.iter().filter(|l| l.starts_with("dir|")).count(), 4);
        let rust = WinMediaIndex::new(Some(index_file.clone()), None);
        assert_eq!(rust_dump(&rust, query), csharp_view, "Rust reads the C# file like C# does (query {query:?})");
    }
    assert_eq!(std::fs::read_to_string(&index_file).unwrap(), written, "reading never rewrites the C# file");

    // Rust writes: the listing changes (a new episode, a removed sidecar, a deleted folder), so the file is rewritten.
    let tree = tmp.path().join("tree");
    std::fs::write(tree.join("Season 1").join("s01e02.mp4"), vec![1u8; 333]).unwrap();
    std::fs::write(tree.join("Season 1").join("s01e02.vtt"), b"WEBVTT\n").unwrap();
    std::fs::write(tree.join("Season 1").join("s01e03.mp4"), vec![2u8; 44]).unwrap();
    std::fs::write(tree.join("Season 1").join("s01e03.tr.srt"), b"1\n").unwrap();
    std::fs::remove_file(tree.join("intro.srt")).unwrap();
    std::fs::remove_dir_all(tree.join("Season 1").join("Extras")).unwrap();
    {
        let rust = WinMediaIndex::new(Some(index_file.clone()), None);
        assert!(matches!(rust.scan_directory(tree.to_str().unwrap(), &tok()), ScanResult::Listed(_)));
        let crawled = rust.crawl(&tree, 2, 64, &tok(), &|_| {});
        assert_eq!(crawled, 3, "the deleted folder is no longer listed by its parent");
        let extras = tree.join("Season 1").join("Extras");
        assert_eq!(rust.scan_directory(extras.to_str().unwrap(), &tok()), ScanResult::Missing, "dropped when scanned");
        let rewritten = std::fs::read_to_string(&index_file).unwrap();
        assert_ne!(rewritten, written);
        let doc = MediaIndexDocument::parse(&rewritten).unwrap();
        assert_eq!(doc.version, 2, "the Rust file is the current C# version, so C# never discards it as legacy");
        for query in ["", "s01e0", "intro"] {
            let csharp_view = csharp(&["index-dump", &file_arg, query]);
            assert_eq!(csharp_view, rust_dump(&rust, query), "C# reads the Rust file like Rust holds it (query {query:?})");
        }
        let csharp_view = csharp(&["index-dump", &file_arg]);
        assert!(csharp_view.iter().any(|l| l.contains("\\s01e02.mp4|s01e02.mp4|") && l.contains("|True|.vtt|")), "the new episode");
        assert!(csharp_view.iter().any(|l| l.contains("\\s01e03.mp4|s01e03.mp4|") && l.contains("|False|null|")), "Windows has no language-suffixed sidecars");
        assert!(csharp_view.iter().any(|l| l.contains("|intro.mp4|") && l.contains("|False|null|")), "the removed sidecar");
        assert!(!csharp_view.iter().any(|l| l.contains("Extras")), "the deleted folder is gone for C# too");
    }
}

#[test]
fn csharp_path_rules_match() {
    if !enabled() {
        return;
    }
    for line in csharp(&["probe"]) {
        let parts: Vec<&str> = line.split('|').collect();
        assert_eq!(parts.len(), 3, "{line}");
        assert_eq!(sa_media::win::paths::path_root(parts[0]), parts[1], "GetPathRoot({})", parts[0]);
        assert_eq!(WinMediaIndex::normalize_directory_path(parts[0]), parts[2], "NormalizeDirectoryPath({})", parts[0]);
    }
}

fn pattern(len: usize, seed: u32) -> Vec<u8> {
    (0..len).map(|i| ((i as u32).wrapping_mul(73).wrapping_add(seed) & 0xFF) as u8).collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex32(text: &str) -> [u8; 32] {
    let mut key = [0u8; 32];
    for (i, k) in key.iter_mut().enumerate() {
        *k = u8::from_str_radix(&text[2 * i..2 * i + 2], 16).unwrap();
    }
    key
}

#[test]
fn csharp_cache_chunks_are_read_by_rust_and_rust_chunks_by_csharp() {
    if !enabled() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_str().unwrap().to_owned();

    // C# registers alice and plays a video (4 chunks of 64 KiB, the last one 1234 bytes) through its stream + cache.
    let out = csharp(&["cache-write", &dir]);
    let parts: Vec<&str> = out[0].split('|').collect();
    // user|vaultKeyHex|identity (itself "PATH|len:N|cs:N")|chunkSize|length
    let (user, key) = (parts[0].to_owned(), unhex32(parts[1]));
    let identity = parts[2..parts.len() - 2].join("|");
    let (chunk, length): (i32, i64) = (parts[parts.len() - 2].parse().unwrap(), parts[parts.len() - 1].parse().unwrap());
    assert_eq!(user, "alice");
    let video = tmp.path().join("video.bin");
    let raw = std::fs::read(&video).unwrap();
    assert_eq!(raw, pattern(3 * 65536 + 1234, 19));
    assert_eq!(VideoCacheStore::compute_video_identity(video.to_str(), length, chunk), identity, "identity");

    let provider_user = user.clone();
    let keys: VaultKeyProvider = Arc::new(move || Some((provider_user.clone(), SecretKey32::new(key))));
    let store = VideoCacheStore::new(tmp.path().join("VideoCache"), keys, Some(1 << 30));
    let total = (length - 1) / i64::from(chunk) + 1;
    assert_eq!(store.cached_chunk_indices(&identity, total, chunk, length), (0..total).collect::<HashSet<_>>());
    for i in 0..total {
        let start = (i * i64::from(chunk)) as usize;
        let expected = &raw[start..raw.len().min(start + chunk as usize)];
        let data = store.try_read_chunk(&identity, chunk, i, expected.len() as i32).expect("Rust decrypts the C# chunk");
        assert_eq!(data, expected, "chunk {i}");
    }

    // Reopening the video in Rust is served entirely from the C# cache: zero reads of the source.
    {
        let mut stream = BufferedVideoStream::open_file(&video, chunk, 2, 0, Some(Arc::clone(&store))).unwrap();
        let mut all = Vec::new();
        std::io::Read::read_to_end(&mut stream, &mut all).unwrap();
        assert_eq!(all, raw);
        assert_eq!(stream.underlying_read_count(), 0);
        assert_eq!(stream.encrypted_cache_read_count(), total as i32);
    }

    // Rust plays another video; C# (logged in again as alice) reads every chunk Rust wrote.
    let video2 = tmp.path().join("second video.bin");
    let raw2 = pattern(2 * 65536 + 77, 5);
    std::fs::write(&video2, &raw2).unwrap();
    {
        let source = std::fs::File::open(&video2).unwrap();
        let mut stream = BufferedVideoStream::from_stream(
            Box::new(source),
            false,
            chunk,
            2,
            0,
            Some(Arc::clone(&store)),
            None,
            Some(video2.to_string_lossy().into_owned()),
        )
        .unwrap();
        let mut all = Vec::new();
        std::io::Read::read_to_end(&mut stream, &mut all).unwrap();
        assert_eq!(all, raw2);
    }
    let len2 = raw2.len() as i64;
    let identity2 = VideoCacheStore::compute_video_identity(video2.to_str(), len2, chunk);
    let lines = csharp(&["cache-read", &dir, &identity2, &chunk.to_string(), &len2.to_string()]);
    let expected: Vec<String> = raw2
        .chunks(chunk as usize)
        .enumerate()
        .map(|(i, c)| format!("{i}|True|{}", hex(&Sha256::digest(c))))
        .collect();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines, expected, "C# lists and decrypts the Rust chunks");

    // Every file of the shared flat folder is padded to the chunk size and carries the fixed 2001-01-01 time.
    for entry in std::fs::read_dir(tmp.path().join("VideoCache").join("v3")).unwrap() {
        let meta = entry.unwrap().metadata().unwrap();
        assert_eq!(meta.len(), 40 + chunk as u64);
        assert_eq!(meta.modified().unwrap(), sa_media::win::cache::fixed_file_time());
    }
}

// ---- script-driven comparison ---------------------------------------------------------------------------------

/// `ScriptDefaultTicks` of the fixture: every written file gets this modification time unless the script names one.
const SCRIPT_DEFAULT_TICKS: i64 = 638_000_000_000_000_000;

fn ticks_to_system_time(ticks: i64) -> std::time::SystemTime {
    let rel = ticks - sa_media::dates::UNIX_EPOCH_TICKS;
    std::time::UNIX_EPOCH + std::time::Duration::from_nanos(u64::try_from(rel).unwrap() * 100)
}

fn stamp(path: &Path, ticks: i64) {
    let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    f.set_times(std::fs::FileTimes::new().set_modified(ticks_to_system_time(ticks))).unwrap();
}

/// The Rust side of the fixture's `script` command: same ops, same tree, same index file.
fn run_rust_script(dir: &Path, script: &str) -> Vec<String> {
    let tree = dir.join("tree");
    let index_file = dir.join("media-index.json");
    let p = |rel: &str| if rel == "." { tree.clone() } else { rel.split('/').fold(tree.clone(), |acc, part| acc.join(part)) };
    let s = |path: PathBuf| path.to_str().unwrap().to_owned();
    let mut index: Option<Arc<WinMediaIndex>> = None;
    let mut out = Vec::new();
    for line in script.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let a: Vec<&str> = line.split('|').collect();
        match a[0] {
            "reset" => {
                if tree.exists() {
                    std::fs::remove_dir_all(&tree).unwrap();
                }
                std::fs::create_dir_all(&tree).unwrap();
                let _ = std::fs::remove_file(&index_file);
                index = Some(WinMediaIndex::new(Some(index_file.clone()), None));
            }
            "mkdir" => std::fs::create_dir_all(p(a[1])).unwrap(),
            "write" => {
                std::fs::write(p(a[1]), vec![0u8; a[2].parse().unwrap()]).unwrap();
                stamp(&p(a[1]), a.get(3).map_or(SCRIPT_DEFAULT_TICKS, |t| t.parse().unwrap()));
            }
            "delete" => {
                if p(a[1]).is_dir() {
                    std::fs::remove_dir_all(p(a[1])).unwrap();
                } else {
                    std::fs::remove_file(p(a[1])).unwrap();
                }
            }
            "stamp" => stamp(&p(a[1]), a[2].parse().unwrap()),
            "scan" => drop(index.as_ref().unwrap().scan_directory(&s(p(a[1])), &tok())),
            "refresh" => drop(index.as_ref().unwrap().refresh_in_memory(&s(p(a[1])), &tok())),
            "transient" => drop(index.as_ref().unwrap().scan_transient(&s(p(a[1])), &tok())),
            "crawl" => drop(index.as_ref().unwrap().crawl_subtree(&s(p(a[1])), a[2].parse().unwrap(), 64, &tok(), &|_| {})),
            "reload" => index = Some(WinMediaIndex::new(Some(index_file.clone()), None)),
            "dump" => {
                let index = index.as_ref().unwrap();
                out.push(format!("total|{}", index.total_indexed_files()));
                for f in index.search_files(a.get(1).copied().unwrap_or(""), 500) {
                    out.push(format!("search|{}", f.full_path));
                }
            }
            "dirs" => out.extend(index.as_ref().unwrap().indexed_directories().into_iter().map(|d| format!("dir|{d}"))),
            op => panic!("unknown script op {op}"),
        }
    }
    out
}

/// Runs `script` with C#, then (from scratch, same paths) with Rust: the printed state and the final
/// `media-index.json` bytes must be identical.
fn compare_script(script: &str) -> Vec<String> {
    let tmp = tempfile::tempdir().unwrap();
    let script_file = tmp.path().join("script.txt");
    std::fs::write(&script_file, script).unwrap();
    let work = tmp.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let csharp_out = csharp(&["script", work.to_str().unwrap(), script_file.to_str().unwrap()]);
    let csharp_file = std::fs::read(work.join("media-index.json")).unwrap();
    let rust_out = run_rust_script(&work, script);
    let rust_file = std::fs::read(work.join("media-index.json")).unwrap();
    assert_eq!(rust_out.len(), csharp_out.len(), "C#:\n{}\nRust:\n{}", csharp_out.join("\n"), rust_out.join("\n"));
    for (i, (r, c)) in rust_out.iter().zip(&csharp_out).enumerate() {
        assert_eq!(r, c, "line {i}; C#:\n{}\nRust:\n{}", csharp_out.join("\n"), rust_out.join("\n"));
    }
    assert_eq!(
        String::from_utf8(rust_file).unwrap(),
        String::from_utf8(csharp_file).unwrap(),
        "the Rust media-index.json is byte-identical to the C# one"
    );
    csharp_out
}

/// Search ties (the same name with the same time in several folders, e.g. copies) keep the C# dictionary order,
/// including the reuse of freed slots after a rescan; OrdinalIgnoreCase treats `ſ` as its own letter; escaping and
/// line breaks of the file match System.Text.Json.
#[test]
fn scripted_index_sessions_match_csharp_line_for_line_and_byte_for_byte() {
    if !enabled() {
        return;
    }
    let out = compare_script(
        "reset
         mkdir|A
         mkdir|B
         mkdir|C
         mkdir|ſub
         mkdir|Vidéo şarkı/İç
         mkdir|.hidden
         write|A/video.mp4|10
         write|B/video.mp4|10
         write|C/VIDEO.mp4|10
         write|A/x.mp4|1
         write|A/a&b`'+(1) ;=.mkv|2
         write|A/Işık şarkı.mp3|2
         write|B/video.srt|1
         write|ſub/ſon.mp4|3
         write|ſub/sample.mp4|4
         write|ſub/t.mp4|4
         write|ſub/ſa.mp4|4
         write|Vidéo şarkı/İç/klip.webm|5|638500000000000001
         write|Vidéo şarkı/notes.txt|5
         scan|.
         crawl|.|2
         dump
         dump|video
         dump|ſ
         dump|S
         dump|ı
         dump|i
         dump|İÇ
         delete|A/x.mp4
         write|A/new.mp4|5
         write|C/video.vtt|5
         scan|A
         scan|C
         dump
         write|B/zz.mp4|1
         delete|B/video.srt
         scan|B
         dump
         dump|VIDEO
         refresh|A
         transient|ſub
         delete|C
         scan|C
         dump
         dirs
         reload
         dump
         dirs",
    );
    assert!(out.iter().any(|l| l.ends_with("klip.webm")), "the crawl reached depth 2");
    assert!(!out.iter().any(|l| l.contains(".hidden")));
}

/// The cache identity (input of every chunk file name and key) equals the C# app's for non-ASCII paths: Turkish
/// letters, `ſ` (upper-cased to `S` by ICU `ToUpperInvariant`), `ß`, title-case digraphs, Greek, mixed separators.
#[test]
fn video_identity_matches_csharp_for_non_ascii_paths() {
    if !enabled() {
        return;
    }
    let b = std::path::MAIN_SEPARATOR;
    let paths = [
        format!("C:{b}Videolar{b}ſon ışık İÇ Straße.mp4"),
        format!("c:/videolar/Şarkı ǅemal ǆ ǈ.MKV"),
        format!("C:{b}a{b}..{b}b{b}Ωmega ﬁlm ΐ.mp4{b}"),
        format!("{b}{b}server{b}share{b}Kelvin K Å µ ÿ.mp4"),
        format!("  C:/x/Ⓐ ⓐ 𐐨 𞤢.mp4  "),
    ];
    for path in paths {
        for (len, chunk) in [(1_234_567i64, 8 * 1024 * 1024), (0, 65_536)] {
            let expected = csharp(&["identity", &path, &len.to_string(), &chunk.to_string()]);
            assert_eq!(expected.len(), 1);
            assert_eq!(VideoCacheStore::compute_video_identity(Some(&path), len, chunk), expected[0], "{path}");
        }
    }
}

/// A hand-written index with the odd entries a file may hold (paths needing normalization, case-duplicate folders,
/// unsorted lists, blank paths, roots that are not drives, an unknown drive, offsets, a newer version) loads into the
/// same state and the same search order in Rust as in C#.
#[test]
fn odd_index_files_load_like_csharp() {
    if !enabled() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let b = std::path::MAIN_SEPARATOR;
    let base = tmp.path().to_str().unwrap().to_owned();
    let file = |dir: &str, name: &str, size: i64, time: &str| {
        serde_json::json!({
            "fullPath": format!("{dir}{b}{name}"), "fileName": name, "directoryPath": dir, "sizeBytes": size,
            "lastWriteTimeUtc": time, "hasSidecarSubtitle": size % 2 == 0, "sidecarExtension": if size % 2 == 0 { serde_json::json!(".srt") } else { serde_json::Value::Null },
            "indexedAtUtc": "0001-01-01T00:00:00+00:00", "extra": [1]
        })
    };
    let d1 = format!("{base}{b}Movies");
    let d1_upper = format!("{base}{b}MOVIES{b}");
    let unc = format!("{b}{b}nohost-sa-test{b}share{b}v");
    let slash_rooted = format!("{b}rooted{b}dir");
    let forward = "//nohost-sa-test/share/w".to_owned();
    let unknown_drive = format!("Q:{b}Films");
    let doc = serde_json::json!({
        "version": 3,
        "unknownTop": {"a": 1},
        "directories": [
            { "directoryPath": d1, "subdirectories": [format!("{d1}{b}b{b}"), "  ", format!("{d1}{b}A")],
              "mediaFiles": [file(&d1, "zeta.mp4", 3, "2026-11-01T10:00:00+03:00"), file(&d1, "Alpha.mkv", 4, "2026-11-01T07:00:00Z"),
                             file(&d1, "same.mp4", 5, "2026-11-01T07:00:00.1234567+00:00"),
                             serde_json::json!({"fullPath": "   ", "fileName": "blank.mp4"})] },
            { "directoryPath": d1_upper, "subdirectories": [], "mediaFiles": [file(&d1_upper.trim_end_matches(b), "same.mp4", 5, "2026-11-01T07:00:00.1234567+00:00")] },
            { "directoryPath": "   ", "mediaFiles": [file("X:", "never.mp4", 1, "2026-01-01T00:00:00Z")] },
            { "directoryPath": unc, "mediaFiles": [file(&unc, "unc.mp4", 7, "2026-11-01T07:00:00Z")] },
            { "directoryPath": slash_rooted, "mediaFiles": [file(&slash_rooted, "rooted.mp4", 7, "2026-11-01T07:00:00Z")] },
            { "directoryPath": forward, "mediaFiles": [file(&forward, "fwd.mp4", 7, "2026-11-01T07:00:00Z")] },
            { "directoryPath": unknown_drive, "mediaFiles": [file(&unknown_drive, "away.mp4", 7, "2026-11-01T07:00:00Z")] }
        ]
    });
    let index_file = tmp.path().join("media-index.json");
    std::fs::write(&index_file, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
    let before = std::fs::read(&index_file).unwrap();
    for query in ["", "same", "A", "movies", "mp4"] {
        let csharp_view = csharp(&["index-dump", index_file.to_str().unwrap(), query]);
        let rust = WinMediaIndex::new(Some(index_file.clone()), None);
        assert_eq!(rust_dump(&rust, query), csharp_view, "query {query:?}");
    }
    assert_eq!(std::fs::read(&index_file).unwrap(), before, "a current-version file is never rewritten on load");
}
