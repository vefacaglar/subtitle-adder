//! Diagnostic log tests: ports of DiagnosticLogTests.swift, WatchHistoryPrivacyTests.DiagnosticLog_RedactsPaths and
//! the win-core §6 queue/rotation rules.

use std::path::Path;
use std::sync::Arc;

use regex::Regex;
use sa_core::diagnostics::{DiagnosticFormat, DiagnosticLog, DiagnosticSink, MemoryDiagnostics};

fn no_secrets() -> Box<dyn Fn() -> Vec<String> + Send + Sync> {
    Box::new(Vec::new)
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

#[test]
fn sanitize_removes_secrets_and_bearer_tokens() {
    let secrets = vec!["  sk-secret-123  ".to_owned(), "".to_owned(), "   ".to_owned()];
    let text = DiagnosticLog::sanitize("key sk-secret-123 and Authorization: Bearer abc.def,ghi BEARER x", &secrets);
    assert_eq!(text, "key [redacted] and Authorization: Bearer [redacted],ghi BEARER [redacted]");
    assert_eq!(DiagnosticLog::sanitize("nothing here", &secrets), "nothing here", "blank secrets never erase text");
}

#[test]
fn mac_messages_lose_newlines_and_are_cut() {
    assert_eq!(DiagnosticLog::sanitize_mac("second\nline  with\r\nbreaks", &[]), "second line  with breaks");
    assert_eq!(DiagnosticLog::sanitize_mac("a\u{2028}b\u{85}c", &[]), "a b c");
    let long = "x".repeat(2500);
    assert_eq!(DiagnosticLog::sanitize_mac(&long, &[]).chars().count(), DiagnosticLog::MAC_MAX_MESSAGE_CHARS);
}

#[test]
fn diagnostic_log_redacts_paths() {
    for (message, expected) in [
        (r"MediaFailed path='C:\Videos\Secret\a.mp4': boom", "MediaFailed path='<path>': boom"),
        ("open D:/Films/Private/clip.mkv failed", "open <path>"),
        (r"share \\nas\media\x.mp4", "share <path>"),
        ("uri file:///C:/x/y.mp4", "uri <path>"),
        ("jobId=3 models=[small]", "jobId=3 models=[small]"),
    ] {
        assert_eq!(DiagnosticLog::redact_paths(message), expected);
    }
}

#[test]
fn mac_lines_are_iso_utc_and_scrubbed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Support").join("diagnostics.log");
    let log = DiagnosticLog::new(path.clone(), DiagnosticFormat::Mac, Box::new(|| vec!["sk-live-1".to_owned()]));
    log.write("translation", "batch failed sk-live-1\nretrying");
    log.write_redacting("engine", "extra sk-other", &["sk-other".to_owned()]);
    log.flush();
    let text = read(&path);
    let line = Regex::new(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z \[translation\] batch failed \[redacted\] retrying\n").unwrap();
    assert!(line.is_match(&text), "{text}");
    assert!(text.contains("[engine] extra [redacted]\n"));
    assert!(!text.contains("sk-"));
}

#[test]
fn windows_lines_use_the_round_trip_time_thread_id_and_crlf() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("SubtitleAdder-diagnostics.log");
    let log = DiagnosticLog::new(path.clone(), DiagnosticFormat::Windows, Box::new(|| vec!["sk-win".to_owned()]));
    log.write("Job", r"StartJob jobId=1 video='C:\Users\x\film.mp4' key=sk-win");
    log.flush();
    let text = read(&path);
    let line = Regex::new(
        r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{7}[+-]\d{2}:\d{2} \[T\d+\] \[Job\] StartJob jobId=1 video='<path>' key=\[redacted\]\r\n$",
    )
    .unwrap();
    assert!(line.is_match(&text), "{text:?}");
}

#[test]
fn windows_consumer_scrubs_existing_logs_first() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("SubtitleAdder-diagnostics.log");
    std::fs::write(&path, "old [Player] MediaFailed path='C:\\Secret\\a.mp4': x\n").unwrap();
    std::fs::write(dir.path().join("SubtitleAdder-diagnostics.log.old"), "older D:/v/b.mkv\n").unwrap();
    std::fs::write(dir.path().join("SubtitleAdder-crash.log"), "crash at C:\\Users\\me\\c.mp4\n").unwrap();
    std::fs::write(dir.path().join("SubtitleAdder-player.log"), "legacy").unwrap();
    let log = DiagnosticLog::new(path.clone(), DiagnosticFormat::Windows, no_secrets());
    log.write("Job", "Finished jobId=2");
    log.flush();
    let text = read(&path);
    assert!(text.starts_with("old [Player] MediaFailed path='<path>': x\n"), "{text}");
    assert!(text.contains("[Job] Finished jobId=2"));
    assert_eq!(read(&dir.path().join("SubtitleAdder-diagnostics.log.old")), "older <path>\n");
    assert_eq!(read(&log.crash_log_path()), "crash at <path>\n");
    assert!(!log.legacy_player_log_path().exists());
    assert!(log.append_crash("panic: boom\n"));
    assert!(read(&log.crash_log_path()).ends_with("panic: boom\n"));
}

#[test]
fn rotates_past_the_threshold_and_keeps_writing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("diagnostics.log");
    let log = DiagnosticLog::with_max_bytes(path.clone(), DiagnosticFormat::Mac, 100, no_secrets());
    std::fs::write(&path, "x".repeat(150)).unwrap();
    log.write("a", "after rotation");
    log.flush();
    assert_eq!(read(&log.rotated_path()), "x".repeat(150));
    assert!(read(&path).contains("[a] after rotation"));
    assert_eq!(log.rotated_path(), dir.path().join("diagnostics.log.old"));
}

#[test]
fn a_burst_never_blocks_and_keeps_at_most_the_queue_capacity() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("diagnostics.log");
    let log = DiagnosticLog::new(path.clone(), DiagnosticFormat::Mac, no_secrets());
    let writers: Vec<_> = (0..4)
        .map(|t| {
            let log = log.clone();
            std::thread::spawn(move || {
                for i in 0..1000 {
                    log.write("burst", &format!("t{t} line {i}"));
                }
            })
        })
        .collect();
    for w in writers {
        w.join().unwrap();
    }
    log.flush();
    let count = read(&path).lines().count();
    assert!(count > 0 && count <= 4000, "{count}");
    assert!(read(&path).lines().all(|l| l.contains("[burst] t")));
}

#[test]
fn an_unwritable_location_drops_lines_silently() {
    let dir = tempfile::tempdir().unwrap();
    let blocker = dir.path().join("file");
    std::fs::write(&blocker, "x").unwrap();
    let log = DiagnosticLog::new(blocker.join("diagnostics.log"), DiagnosticFormat::Mac, no_secrets());
    log.write("a", "b");
    log.flush(); // returns, nothing written
    assert_eq!(read(&blocker), "x");
}

#[test]
fn engine_translation_lines_are_extracted_and_copied() {
    let dir = tempfile::tempdir().unwrap();
    let stderr = dir.path().join("subtitle-engine-1.log");
    let mut content = String::from("noise\r[translation] {\"event\": \"batch_start\"}\n [translation] leading space\n");
    for i in 0..600 {
        content.push_str(&format!("[translation] {{\"event\": \"request_waiting\", \"n\": {i}}}\n"));
    }
    std::fs::write(&stderr, content).unwrap();
    let lines = DiagnosticLog::translation_lines(&stderr);
    assert_eq!(lines.len(), DiagnosticLog::MAX_ENGINE_LINES);
    assert!(lines[0].contains("\"n\": 100"));
    assert!(lines.last().unwrap().contains("\"n\": 599"));
    assert!(DiagnosticLog::translation_lines(&dir.path().join("missing.log")).is_empty());
    let short = dir.path().join("short.log");
    std::fs::write(&short, "progress 10%\r[translation] {\"event\": \"batch_start\"}\r\n").unwrap();
    assert_eq!(DiagnosticLog::translation_lines(&short), ["[translation] {\"event\": \"batch_start\"}"]);

    let path = dir.path().join("diagnostics.log");
    let log = DiagnosticLog::new(path.clone(), DiagnosticFormat::Mac, no_secrets());
    log.write_engine_lines(&[], &[]);
    log.write_engine_lines(&["[translation] {\"key\": \"sk-run\"}".to_owned()], &["sk-run".to_owned()]);
    log.flush();
    let text = read(&path);
    assert!(text.contains("[engine] [translation] {\"key\": \"[redacted]\"}\n"), "{text}");
    assert_eq!(text.lines().count(), 1);
}

#[test]
fn default_paths_and_memory_sink() {
    let app = Path::new("/Users/x/Library/Application Support/SubtitleAdder");
    assert_eq!(DiagnosticLog::default_path(DiagnosticFormat::Mac, app), app.join("diagnostics.log"));
    assert_eq!(
        DiagnosticLog::default_path(DiagnosticFormat::Windows, app),
        std::env::temp_dir().join("SubtitleAdder-diagnostics.log")
    );
    let sink = MemoryDiagnostics::new();
    let dynamic: Arc<dyn DiagnosticSink> = sink.clone();
    dynamic.write("Job", "x");
    assert_eq!(sink.lines(), [("Job".to_owned(), "x".to_owned())]);
}
