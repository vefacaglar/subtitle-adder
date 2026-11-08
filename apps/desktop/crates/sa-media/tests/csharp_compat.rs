//! Data compatibility with the C# app (Windows), checked against golden fixtures frozen from the real C#
//! `MediaLibraryIndex` / `EncryptedVideoCacheStore` / `BufferedVideoStream` (see `tests/fixtures/csharp/README.md`):
//! the C#-written `media-index.json` files and encrypted `VideoCache` chunks are read by the Rust port, and what the
//! Rust port writes must equal the C# bytes (index files) or match the C# files' structure and round-trip (chunks,
//! whose nonces are random).
#![cfg(windows)]

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sa_core::cancel::CancellationToken;
use sa_core::secrets::SecretKey32;
use sa_media::win::{BufferedVideoStream, VaultKeyProvider, VideoCacheStore, WinMediaIndex};
use sa_media::{MediaIndex, MediaIndexDocument, ScanResult};
use serde_json::Value;
use sha2::{Digest, Sha256};

// ---- fixtures -------------------------------------------------------------------------------------------------

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/csharp").join(name)
}

fn fixture_text(name: &str) -> String {
    std::fs::read_to_string(fixture(name)).unwrap_or_else(|e| panic!("fixture {name}: {e}"))
}

fn fixture_json(name: &str) -> Value {
    serde_json::from_str(&fixture_text(name)).unwrap_or_else(|e| panic!("fixture {name}: {e}"))
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array().unwrap().iter().map(|s| s.as_str().unwrap().to_owned()).collect()
}

/// `[{query, lines}]` of a frozen `index-dump` series.
fn dumps(v: &Value) -> Vec<(String, Vec<String>)> {
    v.as_array().unwrap().iter().map(|d| (d["query"].as_str().unwrap().to_owned(), strings(&d["lines"]))).collect()
}

/// The folder that replaces `{ROOT}`; it must need no JSON escaping, like the freeze-time folder.
fn root_of(dir: &Path) -> String {
    let root = dir.to_str().unwrap().to_owned();
    assert!(
        root.chars().all(|c| c.is_ascii_alphanumeric() || "\\:._-".contains(c)),
        "the temp folder {root} must be plain ASCII for the frozen fixtures"
    );
    root
}

/// Search also matches a file's directory path, so a query found inside `{ROOT}` matches every file there. The
/// frozen C# results hold the freeze-time folder's answers (`matched` lists the queries it contained); the test's
/// own folder must answer the same, or the comparison would test the folder name instead of the index.
fn assert_root_matches(root: &str, queries: &[&str], matched: &[&str]) {
    for q in queries.iter().filter(|q| !q.trim().is_empty()) {
        assert_eq!(
            sa_media::text::contains_ordinal_ignore_case(root, q.trim()),
            matched.contains(q),
            "the folder {root} must {}contain the search query {q:?}, as the freeze-time folder did",
            if matched.contains(q) { "" } else { "not " }
        );
    }
}

fn with_root(lines: &[String], root: &str) -> Vec<String> {
    lines.iter().map(|l| l.replace("{ROOT}", root)).collect()
}

fn with_root_json(text: &str, root: &str) -> String {
    text.replace("{ROOT}", &root.replace('\\', "\\\\"))
}

// ---- helpers --------------------------------------------------------------------------------------------------

fn dotnet_bool(b: bool) -> &'static str {
    if b { "True" } else { "False" }
}

/// The Rust index state in the format of the C# tool's `index-dump`: `total|n`, then per folder `dir|path`,
/// `sub|path` and `file|fullPath|name|dir|size|lastWriteTicks|hasSidecar|ext|indexedTicks`, then the search order.
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

fn ticks_to_system_time(ticks: i64) -> std::time::SystemTime {
    let rel = ticks - sa_media::dates::UNIX_EPOCH_TICKS;
    std::time::UNIX_EPOCH + std::time::Duration::from_nanos(u64::try_from(rel).unwrap() * 100)
}

fn stamp(path: &Path, ticks: i64) {
    let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    f.set_times(std::fs::FileTimes::new().set_modified(ticks_to_system_time(ticks))).unwrap();
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

// ---- media index ----------------------------------------------------------------------------------------------

/// `ScriptDefaultTicks` of the C# tool: every scripted file (and every file the index-session test adds) gets this
/// modification time unless the script names one.
const SCRIPT_DEFAULT_TICKS: i64 = 638_000_000_000_000_000;

/// The time the C# `index-write` gave every file of its tree: 2026-11-01T08:12:44.1234567Z.
const INDEX_WRITE_TICKS: i64 = 639_291_175_641_234_567;

/// The tree of the C# `index-write`, rebuilt under `dir\tree` (same names, sizes and times).
fn build_index_write_tree(dir: &Path) -> PathBuf {
    let tree = dir.join("tree");
    std::fs::create_dir_all(tree.join("Season 1").join("Extras")).unwrap();
    std::fs::create_dir_all(tree.join("Vid\u{e9}o \u{15f}ark\u{131}")).unwrap();
    std::fs::write(tree.join("intro.mp4"), vec![0u8; 2048]).unwrap();
    std::fs::write(tree.join("intro.srt"), "1\n00:00:01,000 --> 00:00:02,000\nx\n").unwrap();
    std::fs::write(tree.join("a+b'(c)\u{e9}.mkv"), vec![0u8; 10]).unwrap();
    std::fs::write(tree.join("Season 1").join("s01e01.mkv"), vec![0u8; 4096]).unwrap();
    std::fs::write(tree.join("Season 1").join("s01e01.vtt"), "WEBVTT\n").unwrap();
    std::fs::write(tree.join("Season 1").join("Extras").join("behind.mov"), vec![0u8; 7]).unwrap();
    std::fs::write(tree.join("Vid\u{e9}o \u{15f}ark\u{131}").join("I\u{15f}\u{131}k.mp3"), vec![0u8; 3]).unwrap();
    for f in [
        tree.join("intro.mp4"),
        tree.join("intro.srt"),
        tree.join("a+b'(c)\u{e9}.mkv"),
        tree.join("Season 1").join("s01e01.mkv"),
        tree.join("Season 1").join("s01e01.vtt"),
        tree.join("Season 1").join("Extras").join("behind.mov"),
        tree.join("Vid\u{e9}o \u{15f}ark\u{131}").join("I\u{15f}\u{131}k.mp3"),
    ] {
        stamp(&f, INDEX_WRITE_TICKS);
    }
    tree
}

#[test]
fn a_csharp_media_index_reads_identically_in_rust_and_a_rust_index_matches_the_csharp_view_of_it() {
    assert_eq!(sa_media::dates::format_dotnet(INDEX_WRITE_TICKS), "2026-11-01T08:12:44.1234567+00:00");
    let tmp = tempfile::tempdir().unwrap();
    let root = root_of(tmp.path());
    let tree = build_index_write_tree(tmp.path());
    let index_file = tmp.path().join("media-index.json");
    let frozen = fixture_json("index-session.dumps.json");

    // C# wrote: a scan and a depth-2 crawl of the tree, with sidecars, non-ASCII and JSON-escaped names.
    let written = with_root_json(&fixture_text("index-session.media-index.json"), &root);
    assert!(written.contains("\\u00E9") && written.contains("\\u002B"), "the C# file escapes non-ASCII and '+'");
    std::fs::write(&index_file, &written).unwrap();
    let csharp_written = dumps(&frozen["csharp_written"]);
    assert_eq!(csharp_written.len(), 4);
    let all_queries: Vec<&str> = ["csharp_written", "rust_written"]
        .iter()
        .flat_map(|k| frozen[*k].as_array().unwrap().iter().map(|d| d["query"].as_str().unwrap()))
        .collect();
    assert_root_matches(&root, &all_queries, &[]);
    for (query, lines) in &csharp_written {
        let csharp_view = with_root(lines, &root);
        assert_eq!(csharp_view[0], "total|5");
        assert_eq!(csharp_view.iter().filter(|l| l.starts_with("dir|")).count(), 4);
        let rust = WinMediaIndex::new(Some(index_file.clone()), None);
        assert_eq!(rust_dump(&rust, query), csharp_view, "Rust reads the C# file like C# does (query {query:?})");
    }
    assert_eq!(std::fs::read_to_string(&index_file).unwrap(), written, "reading never rewrites the C# file");

    // Rust writes: the listing changes (a new episode, a removed sidecar, a deleted folder), so the file is rewritten.
    let season = tree.join("Season 1");
    std::fs::write(season.join("s01e02.mp4"), vec![1u8; 333]).unwrap();
    std::fs::write(season.join("s01e02.vtt"), b"WEBVTT\n").unwrap();
    std::fs::write(season.join("s01e03.mp4"), vec![2u8; 44]).unwrap();
    std::fs::write(season.join("s01e03.tr.srt"), b"1\n").unwrap();
    for name in ["s01e02.mp4", "s01e02.vtt", "s01e03.mp4", "s01e03.tr.srt"] {
        stamp(&season.join(name), SCRIPT_DEFAULT_TICKS);
    }
    std::fs::remove_file(tree.join("intro.srt")).unwrap();
    std::fs::remove_dir_all(season.join("Extras")).unwrap();
    let rust = WinMediaIndex::new(Some(index_file.clone()), None);
    assert!(matches!(rust.scan_directory(tree.to_str().unwrap(), &tok()), ScanResult::Listed(_)));
    let crawled = rust.crawl(&tree, 2, 64, &tok(), &|_| {});
    assert_eq!(crawled, 3, "the deleted folder is no longer listed by its parent");
    let extras = season.join("Extras");
    assert_eq!(rust.scan_directory(extras.to_str().unwrap(), &tok()), ScanResult::Missing, "dropped when scanned");
    let rewritten = std::fs::read_to_string(&index_file).unwrap();
    assert_ne!(rewritten, written);
    let doc = MediaIndexDocument::parse(&rewritten).unwrap();
    assert_eq!(doc.version, 2, "the Rust file is the current C# version, so C# never discards it as legacy");

    // What C# read from the Rust-written file at freeze time: Rust must still hold that state, and write a file
    // that reads back into it.
    let rust_written = dumps(&frozen["rust_written"]);
    assert_eq!(rust_written.len(), 3);
    let reloaded = WinMediaIndex::new(Some(index_file.clone()), None);
    for (query, lines) in &rust_written {
        let csharp_view = with_root(lines, &root);
        assert_eq!(rust_dump(&rust, query), csharp_view, "Rust holds what C# read from the Rust file (query {query:?})");
        assert_eq!(rust_dump(&reloaded, query), csharp_view, "the Rust file reads back the same (query {query:?})");
    }
    let view = rust_dump(&reloaded, "");
    assert!(view.iter().any(|l| l.contains("\\s01e02.mp4|s01e02.mp4|") && l.contains("|True|.vtt|")), "the new episode");
    assert!(view.iter().any(|l| l.contains("\\s01e03.mp4|s01e03.mp4|") && l.contains("|False|null|")), "Windows has no language-suffixed sidecars");
    assert!(view.iter().any(|l| l.contains("|intro.mp4|") && l.contains("|False|null|")), "the removed sidecar");
    assert!(!view.iter().any(|l| l.contains("Extras")), "the deleted folder is gone");
}

#[test]
fn csharp_path_rules_match() {
    let cwd = std::env::current_dir().unwrap().to_str().unwrap().to_owned();
    let lines = strings(&fixture_json("probe.json"));
    assert_eq!(lines.len(), 7);
    for line in lines {
        let parts: Vec<&str> = line.split('|').collect();
        assert_eq!(parts.len(), 3, "{line}");
        assert_eq!(sa_media::win::paths::path_root(parts[0]), parts[1], "GetPathRoot({})", parts[0]);
        if parts[2] == "{CWD}" {
            // `C:` is drive-relative: it resolves to the current directory only while that is on drive C.
            if cwd.get(..2).is_some_and(|d| d.eq_ignore_ascii_case("C:")) {
                assert_eq!(WinMediaIndex::normalize_directory_path(parts[0]), cwd, "NormalizeDirectoryPath({})", parts[0]);
            }
            continue;
        }
        assert_eq!(WinMediaIndex::normalize_directory_path(parts[0]), parts[2], "NormalizeDirectoryPath({})", parts[0]);
    }
}

/// The Rust side of the C# tool's `script` command (one `op|arg|…` per line, against `dir\tree` and
/// `dir\media-index.json`):
///   reset                      empty tree, no index file, new index
///   mkdir|rel  write|rel|len[|ticks]  delete|rel  stamp|rel|ticks   (files get SCRIPT_DEFAULT_TICKS unless given)
///   scan|rel  refresh|rel  transient|rel  crawl|rel|depth  reload
///   dump|query                 total|n, then search|fullPath of search(query, 500)
///   dirs                       dir|path per indexed directory
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

/// Search ties (the same name with the same time in several folders, e.g. copies) keep the C# dictionary order,
/// including the reuse of freed slots after a rescan; OrdinalIgnoreCase treats `ſ` as its own letter; escaping and
/// line breaks of the file match System.Text.Json: the printed state equals the frozen C# output line for line and
/// the final `media-index.json` equals the C# file byte for byte.
#[test]
fn scripted_index_sessions_match_csharp_line_for_line_and_byte_for_byte() {
    // The freeze-time folder (`%TEMP%\.tmp??????\work`) contained `S` (`Users`) and `i` (its random part) and none of
    // the other queries; the prefix gives this folder an `i` and an `s` whatever the random part is.
    let tmp = tempfile::Builder::new().prefix("sa-script-i-").tempdir().unwrap();
    let work = tmp.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let root = root_of(&work);
    let script = fixture_text("script-session.script.txt");
    let queries: Vec<&str> = script.lines().filter_map(|l| l.trim().strip_prefix("dump|")).collect();
    assert_eq!(queries.len(), 7);
    assert_root_matches(&root, &queries, &["S", "i"]);
    let csharp_out = with_root(&fixture_text("script-session.out.txt").lines().map(str::to_owned).collect::<Vec<_>>(), &root);
    let csharp_file = with_root_json(&fixture_text("script-session.media-index.json"), &root);

    let rust_out = run_rust_script(&work, &script);
    let rust_file = std::fs::read(work.join("media-index.json")).unwrap();
    assert_eq!(rust_out.len(), csharp_out.len(), "C#:\n{}\nRust:\n{}", csharp_out.join("\n"), rust_out.join("\n"));
    for (i, (r, c)) in rust_out.iter().zip(&csharp_out).enumerate() {
        assert_eq!(r, c, "line {i}; C#:\n{}\nRust:\n{}", csharp_out.join("\n"), rust_out.join("\n"));
    }
    assert_eq!(String::from_utf8(rust_file).unwrap(), csharp_file, "the Rust media-index.json is byte-identical to the C# one");
    assert!(csharp_out.iter().any(|l| l.ends_with("klip.webm")), "the crawl reached depth 2");
    assert!(!csharp_out.iter().any(|l| l.contains(".hidden")));
}

/// A hand-written index with the odd entries a file may hold (paths needing normalization, case-duplicate folders,
/// unsorted lists, blank paths, roots that are not drives, an unknown drive, offsets, a newer version) loads into the
/// same state and the same search order in Rust as in C#.
#[test]
fn odd_index_files_load_like_csharp() {
    let tmp = tempfile::tempdir().unwrap();
    let index_file = tmp.path().join("media-index.json");
    let before = std::fs::read(fixture("odd-index.media-index.json")).unwrap();
    std::fs::write(&index_file, &before).unwrap();
    // The index holds the drive-relative directory `\rooted\dir`. C# resolved it against the drive of the freeze
    // run's current directory (C:); Rust resolves it against the test's current drive (D: on CI runners). On another
    // drive the frozen drive is replaced by the current one, and that directory's block (its `dir|` line and the
    // lines up to the next `dir|`/`search|`) is compared on its own, because the directory order follows the
    // resolved path; the other blocks must still come in the C# order.
    let cwd = std::env::current_dir().unwrap().to_str().unwrap().to_owned();
    let drive = cwd.get(..2).filter(|d| d.ends_with(':')).expect("the tests run from a drive-letter path").to_uppercase();
    let rooted = format!("dir|{drive}\\rooted\\dir");
    // (the lines without the rooted block, the rooted block)
    let split_rooted = |lines: &[String]| -> (Vec<String>, Vec<String>) {
        let mut rest = Vec::new();
        let mut block = Vec::new();
        let mut inside = false;
        for l in lines {
            if l.starts_with("dir|") || l.starts_with("search|") {
                inside = *l == rooted;
            }
            if inside { block.push(l.clone()) } else { rest.push(l.clone()) }
        }
        (rest, block)
    };
    let frozen = dumps(&fixture_json("odd-index.dumps.json"));
    assert_eq!(frozen.len(), 5);
    for (query, csharp_view) in &frozen {
        let rust = WinMediaIndex::new(Some(index_file.clone()), None);
        let rust_view = rust_dump(&rust, query);
        if drive == "C:" {
            assert_eq!(&rust_view, csharp_view, "query {query:?}");
            continue;
        }
        let csharp_view: Vec<String> =
            csharp_view.iter().map(|l| if l == "dir|C:\\rooted\\dir" { rooted.clone() } else { l.clone() }).collect();
        let (rust_rest, rust_block) = split_rooted(&rust_view);
        let (csharp_rest, csharp_block) = split_rooted(&csharp_view);
        assert!(!csharp_block.is_empty(), "query {query:?}: the drive-relative entry");
        assert_eq!(rust_block, csharp_block, "query {query:?}: the drive-relative directory");
        assert_eq!(rust_rest, csharp_rest, "query {query:?}");
    }
    assert_eq!(std::fs::read(&index_file).unwrap(), before, "a current-version file is never rewritten on load");
}

// ---- video cache ----------------------------------------------------------------------------------------------

/// The cache identity (input of every chunk file name and key) equals the C# app's for non-ASCII paths: Turkish
/// letters, `ſ` (upper-cased to `S` by ICU `ToUpperInvariant`), `ß`, title-case digraphs, Greek, mixed separators.
#[test]
fn video_identity_matches_csharp_for_non_ascii_paths() {
    let cases = fixture_json("identity.json");
    assert_eq!(cases.as_array().unwrap().len(), 10);
    for case in cases.as_array().unwrap() {
        let path = case["path"].as_str().unwrap();
        let (len, chunk) = (case["length"].as_i64().unwrap(), case["chunk_size"].as_i64().unwrap() as i32);
        assert_eq!(VideoCacheStore::compute_video_identity(Some(path), len, chunk), case["identity"].as_str().unwrap(), "{path}");
    }
}

fn keys_for(user: &str, key: [u8; 32]) -> VaultKeyProvider {
    let user = user.to_owned();
    Arc::new(move || Some((user.clone(), SecretKey32::new(key))))
}

/// Plays `raw` (identity from `path`) through a Rust stream into `store` (synchronous SSD writes, no read-ahead).
fn play_into(store: &Arc<VideoCacheStore>, raw: &[u8], path: &str, chunk: i32) -> BufferedVideoStream {
    let mut stream = BufferedVideoStream::from_stream(
        Box::new(std::io::Cursor::new(raw.to_vec())),
        false,
        chunk,
        2,
        0,
        Some(Arc::clone(store)),
        None,
        Some(path.to_owned()),
    )
    .unwrap();
    let mut all = Vec::new();
    std::io::Read::read_to_end(&mut stream, &mut all).unwrap();
    assert_eq!(all, raw);
    stream
}

fn vcache_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
    files.sort();
    files
}

#[test]
fn csharp_cache_chunks_are_read_by_rust_and_rust_chunks_match_the_csharp_layout() {
    let meta = fixture_json("cache/cache.json");
    let user = meta["user"].as_str().unwrap();
    let key = unhex32(meta["vault_key_hex"].as_str().unwrap());
    let video_path = meta["video_path"].as_str().unwrap();
    let identity = meta["identity"].as_str().unwrap();
    let chunk = meta["chunk_size"].as_i64().unwrap() as i32;
    let length = meta["length"].as_i64().unwrap();
    let names: HashSet<String> = strings(&meta["files"]).into_iter().collect();
    assert_eq!(user, "alice");
    let raw = pattern(length as usize, meta["pattern_seed"].as_u64().unwrap() as u32);
    assert_eq!(raw.len(), 3 * 65536 + 1234);
    assert_eq!(VideoCacheStore::compute_video_identity(Some(video_path), length, chunk), identity, "identity");

    // The C# chunks, copied into a fresh cache folder.
    let tmp = tempfile::tempdir().unwrap();
    let v3 = tmp.path().join("VideoCache").join(VideoCacheStore::LAYOUT_FOLDER);
    std::fs::create_dir_all(&v3).unwrap();
    for name in &names {
        std::fs::copy(fixture("cache/v3").join(name), v3.join(name)).unwrap();
    }
    let store = VideoCacheStore::new(tmp.path().join("VideoCache"), keys_for(user, key), Some(1 << 30));
    let total = (length - 1) / i64::from(chunk) + 1;
    assert_eq!(total, 4);
    assert_eq!(store.cached_chunk_indices(identity, total, chunk, length), (0..total).collect::<HashSet<_>>());
    for i in 0..total {
        let start = (i * i64::from(chunk)) as usize;
        let expected = &raw[start..raw.len().min(start + chunk as usize)];
        let data = store.try_read_chunk(identity, chunk, i, expected.len() as i32).expect("Rust decrypts the C# chunk");
        assert_eq!(data, expected, "chunk {i}");
    }

    // Reopening the video in Rust is served entirely from the C# cache: zero reads of the source.
    {
        let stream = play_into(&store, &raw, video_path, chunk);
        assert_eq!(stream.underlying_read_count(), 0);
        assert_eq!(stream.encrypted_cache_read_count(), total as i32);
    }

    // Rust writes the same video with the same profile key into an empty cache: the same file names (the keyed
    // tags) and sizes as the C# files, the same header (magic and padded chunk size), a different random nonce, and
    // the chunks decrypt again in a new store.
    let tmp2 = tempfile::tempdir().unwrap();
    {
        let writer = VideoCacheStore::new(tmp2.path().join("VideoCache"), keys_for(user, key), Some(1 << 30));
        let stream = play_into(&writer, &raw, video_path, chunk);
        assert_eq!(stream.encrypted_cache_read_count(), 0);
    }
    let rust_files = vcache_files(&tmp2.path().join("VideoCache").join(VideoCacheStore::LAYOUT_FOLDER));
    let rust_names: HashSet<String> = rust_files.iter().map(|p| p.file_name().unwrap().to_str().unwrap().to_owned()).collect();
    assert_eq!(rust_names, names, "Rust names the chunks like C#");
    for path in &rust_files {
        let name = path.file_name().unwrap().to_str().unwrap();
        let rust_bytes = std::fs::read(path).unwrap();
        let csharp_bytes = std::fs::read(fixture("cache/v3").join(name)).unwrap();
        assert_eq!(rust_bytes.len(), csharp_bytes.len(), "{name}");
        assert_eq!(rust_bytes.len() as i64, VideoCacheStore::HEADER_SIZE + i64::from(chunk));
        assert_eq!(&rust_bytes[..12], &csharp_bytes[..12], "{name}: magic and padded chunk size");
        assert_eq!(&rust_bytes[..8], b"SAVC0003");
        assert_ne!(&rust_bytes[12..24], &csharp_bytes[12..24], "{name}: a fresh nonce");
        assert_eq!(std::fs::metadata(path).unwrap().modified().unwrap(), sa_media::win::cache::fixed_file_time());
    }
    let reader = VideoCacheStore::new(tmp2.path().join("VideoCache"), keys_for(user, key), Some(1 << 30));
    assert_eq!(reader.cached_chunk_indices(identity, total, chunk, length), (0..total).collect::<HashSet<_>>());
    for i in 0..total {
        let start = (i * i64::from(chunk)) as usize;
        let expected = &raw[start..raw.len().min(start + chunk as usize)];
        assert_eq!(reader.try_read_chunk(identity, chunk, i, expected.len() as i32).as_deref(), Some(expected), "chunk {i}");
    }

    // Rust plays another video into the folder holding the C# chunks; a new store lists and decrypts every chunk.
    let video2 = tmp.path().join("second video.bin");
    let raw2 = pattern(2 * 65536 + 77, 5);
    let video2 = video2.to_str().unwrap();
    drop(play_into(&store, &raw2, video2, chunk));
    let len2 = raw2.len() as i64;
    let identity2 = VideoCacheStore::compute_video_identity(Some(video2), len2, chunk);
    let reader = VideoCacheStore::new(tmp.path().join("VideoCache"), keys_for(user, key), Some(1 << 30));
    assert_eq!(reader.cached_chunk_indices(&identity2, 3, chunk, len2), (0..3).collect::<HashSet<_>>());
    for (i, c) in raw2.chunks(chunk as usize).enumerate() {
        let data = reader.try_read_chunk(&identity2, chunk, i as i64, c.len() as i32).expect("the Rust chunk reads back");
        assert_eq!(hex(&Sha256::digest(&data)), hex(&Sha256::digest(c)), "chunk {i}");
    }

    // Every file of the shared flat folder is padded to the chunk size; the Rust-written ones carry the fixed
    // 2001-01-01 time (git does not keep the C# files' times; they had it at freeze time).
    let files = vcache_files(&v3);
    assert_eq!(files.len(), 4 + 3);
    for path in files {
        let meta = std::fs::metadata(&path).unwrap();
        assert_eq!(meta.len(), 40 + chunk as u64);
        if !names.contains(path.file_name().unwrap().to_str().unwrap()) {
            assert_eq!(meta.modified().unwrap(), sa_media::win::cache::fixed_file_time());
        }
    }
}

// ---- stream and formatting behavior ---------------------------------------------------------------------------

/// The Rust side of the C# tool's `stream-trace`: plays `dir\video.bin` (pattern seed 19) through a
/// `BufferedVideoStream` over an in-memory video (synchronous SSD writes, no read-ahead) with a `VideoCacheStore`
/// whose profile logs out (`o`) and in (`l`). Ops (comma-separated): `sN` seek to N, `eN` seek to length-N, `cN` seek
/// by N, `rN` read N bytes (prints `r|count|` and the first 16 hex of the SHA-256), `q` (prints the stream state and
/// the buffered-ratio bits at 0, 0.25, 0.5, 0.99, NaN and 2), `x` dispose and reopen, `o` log out, `l` log in, `f`
/// (prints `f|count|used` of the cache folder).
fn run_rust_stream_trace(dir: &Path, length: usize, chunk: i32, max_cached: i32, ops: &str) -> Vec<String> {
    use std::sync::atomic::{AtomicBool, Ordering};
    let raw = pattern(length, 19);
    let video = dir.join("video.bin");
    std::fs::write(&video, &raw).unwrap();
    let logged_in = Arc::new(AtomicBool::new(true));
    let flag = Arc::clone(&logged_in);
    let keys: VaultKeyProvider =
        Arc::new(move || flag.load(Ordering::SeqCst).then(|| ("alice".to_owned(), SecretKey32::new([7u8; 32]))));
    let cache_dir = dir.join("VideoCache");
    let store = VideoCacheStore::new(cache_dir.clone(), keys, Some(1 << 30));
    let open = || {
        BufferedVideoStream::from_stream(
            Box::new(std::io::Cursor::new(raw.clone())),
            false,
            chunk,
            max_cached,
            0,
            Some(Arc::clone(&store)),
            None,
            Some(video.to_string_lossy().into_owned()),
        )
        .unwrap()
    };
    fn count_vcache(dir: &Path) -> usize {
        std::fs::read_dir(dir).map_or(0, |entries| {
            entries
                .flatten()
                .map(|e| {
                    let p = e.path();
                    if p.is_dir() {
                        count_vcache(&p)
                    } else {
                        usize::from(p.extension().is_some_and(|x| x.eq_ignore_ascii_case("vcache")))
                    }
                })
                .sum()
        })
    }
    let mut stream = open();
    let mut out = Vec::new();
    for op in ops.split(',') {
        let n: i64 = if op.len() > 1 { op[1..].parse().unwrap() } else { 0 };
        match &op[..1] {
            "s" => drop(stream.seek_shared(std::io::SeekFrom::Start(n as u64))),
            "e" => drop(stream.seek_shared(std::io::SeekFrom::End(-n))),
            "c" => drop(stream.seek_shared(std::io::SeekFrom::Current(n))),
            "r" => {
                let mut buf = vec![0u8; n as usize];
                let got = stream.read_shared(&mut buf).unwrap();
                out.push(format!("r|{got}|{}", &hex(&Sha256::digest(&buf[..got]))[..16]));
            }
            "q" => {
                let bits: Vec<String> = [0.0, 0.25, 0.5, 0.99, f64::NAN, 2.0]
                    .iter()
                    .map(|p| (stream.buffered_ratio(*p).to_bits() as i64).to_string())
                    .collect();
                out.push(format!(
                    "q|{}|{}|{}|{}|{}|{}",
                    stream.position(),
                    stream.underlying_read_count(),
                    stream.underlying_bytes_read(),
                    stream.encrypted_cache_read_count(),
                    stream.cached_chunk_count(),
                    bits.join("|")
                ));
            }
            "x" => drop(std::mem::replace(&mut stream, open())),
            "o" => {
                logged_in.store(false, Ordering::SeqCst);
                store.end_stale_sessions();
            }
            "l" => logged_in.store(true, Ordering::SeqCst),
            "f" => out.push(format!("f|{}|{}", count_vcache(&cache_dir), store.total_used_bytes())),
            other => panic!("unknown stream op {other}"),
        }
    }
    out
}

/// RAM window eviction, SSD hits after a reopen, short reads at the end, seeks past the end, the buffer-bar ratio (bit
/// for bit, NaN and out-of-range playback ratios included), and a logout in the middle of playback (the open stream's
/// session ends: no more SSD reads or writes, even after the same profile logs in again; a reopened stream uses the
/// cache again) behave exactly like the C# `BufferedVideoStream` + `EncryptedVideoCacheStore`: the Rust trace equals
/// the frozen C# trace. Scenario 2 has a chunk size below the 64 KiB floor, a two-chunk RAM window, seeks past the end
/// and current-relative seeks; scenario 3 a video of exactly one chunk and empty reads.
#[test]
fn buffered_stream_traces_match_csharp() {
    let scenarios = fixture_json("stream-traces.json");
    assert_eq!(scenarios.as_array().unwrap().len(), 3);
    for s in scenarios.as_array().unwrap() {
        let length = s["length"].as_u64().unwrap() as usize;
        let chunk = s["chunk_size"].as_i64().unwrap() as i32;
        let max_cached = s["max_cached_chunks"].as_i64().unwrap() as i32;
        let ops = s["ops"].as_str().unwrap();
        let expected = strings(&s["lines"]);
        let tmp = tempfile::tempdir().unwrap();
        let actual = run_rust_stream_trace(tmp.path(), length, chunk, max_cached, ops);
        assert_eq!(actual, expected, "ops {ops}");
    }
}

/// `FormatFileSize`, `GetMimeType` and `SidecarBadge` give the C# text for the same inputs.
#[test]
fn size_mime_and_badge_texts_match_csharp() {
    let texts = fixture_json("texts.json");
    let sizes = texts["format_file_size"].as_array().unwrap();
    assert!(sizes.len() > 1200);
    for pair in sizes {
        let bytes = pair[0].as_i64().unwrap();
        assert_eq!(sa_media::format_file_size(bytes), pair[1].as_str().unwrap(), "format_file_size({bytes})");
    }

    let mimes = texts["mime_type"].as_array().unwrap();
    assert_eq!(mimes.len(), 13);
    for pair in mimes {
        let path = pair[0].as_str().unwrap();
        assert_eq!(sa_media::win::mime_type(Some(path)), pair[1].as_str().unwrap(), "mime_type({path:?})");
    }

    let badges = texts["sidecar_badge"].as_array().unwrap();
    assert_eq!(badges.len(), 11);
    for pair in badges {
        let ext = pair[0].as_str().map(str::to_owned);
        let f = sa_media::IndexedMediaFile {
            full_path: "C:\\a.mp4".into(),
            file_name: "a.mp4".into(),
            directory_path: "C:\\".into(),
            size_bytes: 1,
            last_write_time_utc: 0,
            has_sidecar_subtitle: true,
            sidecar_extension: ext.clone(),
            indexed_at_utc: 0,
        };
        assert_eq!(f.sidecar_badge().as_deref(), pair[1].as_str(), "sidecar_badge({ext:?})");
    }
}
