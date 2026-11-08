//! Crash log (win-ui-main §1.4; C# `App.LogCrash`): an unhandled error is appended to `%TEMP%\SubtitleAdder-crash.log`
//! as `"{local time, .NET round-trip "O"}\n{redacted error with backtrace}\n\n"`. Paths are redacted (the message can
//! name the opened video); IO errors while logging are ignored.
//!
//! The hook is installed first thing at startup (win-ui-main §1.1 step 1), before the diagnostics log exists. Once
//! the app created its `DiagnosticLog`, [`set_diagnostic_log`] routes entries through `DiagnosticLog::append_crash`,
//! which holds the same lock as the log's startup scrub of old crash entries (C# `DiagnosticLog.AppendCrash`), so a
//! crash written while the scrub runs is never overwritten.

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;
use sa_core::diagnostics::DiagnosticLog;

static LOCK: Mutex<()> = Mutex::new(());
static LOG: OnceLock<Arc<DiagnosticLog>> = OnceLock::new();

/// Routes crash entries through `log` (its crash file, under its scrub lock). The first call wins.
pub fn set_diagnostic_log(log: Arc<DiagnosticLog>) {
    let _ = LOG.set(log);
}

/// `%TEMP%\SubtitleAdder-crash.log`.
pub fn crash_log_path() -> PathBuf {
    std::env::temp_dir().join("SubtitleAdder-crash.log")
}

/// .NET `DateTime.Now.ToString("O")`: `2026-11-07T14:03:11.1234567+03:00`.
pub fn round_trip_now() -> String {
    use chrono::Timelike;
    let now = chrono::Local::now();
    let ticks = (now.nanosecond() % 1_000_000_000) / 100;
    format!("{}.{ticks:07}{}", now.format("%Y-%m-%dT%H:%M:%S"), now.format("%:z"))
}

/// The entry text for `error` (already including any backtrace).
pub fn entry(timestamp: &str, error: &str) -> String {
    format!("{timestamp}\n{}\n\n", DiagnosticLog::redact_paths(error))
}

/// Appends one entry; `false` when it could not be written.
pub fn append(error: &str) -> bool {
    match LOG.get() {
        Some(log) => log.append_crash(&entry(&round_trip_now(), error)),
        None => append_to(&crash_log_path(), error),
    }
}

pub fn append_to(path: &std::path::Path, error: &str) -> bool {
    let text = entry(&round_trip_now(), error);
    let _g = LOCK.lock();
    std::fs::OpenOptions::new().create(true).append(true).open(path).and_then(|mut f| f.write_all(text.as_bytes())).is_ok()
}

/// Installs the panic hook (once; the previous hook still runs, so debug output keeps working).
pub fn install() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let thread = std::thread::current().name().unwrap_or("<unnamed>").to_owned();
            let payload = info
                .payload()
                .downcast_ref::<&str>()
                .map(|s| (*s).to_owned())
                .or_else(|| info.payload().downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "Box<dyn Any>".to_owned());
            let location = info.location().map(|l| format!(" at {}:{}:{}", l.file(), l.line(), l.column())).unwrap_or_default();
            let backtrace = std::backtrace::Backtrace::force_capture();
            let _ = append(&format!("panic in thread '{thread}'{location}: {payload}\n{backtrace}"));
            previous(info);
        }));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_use_the_dotnet_round_trip_form() {
        let t = round_trip_now();
        let re = regex_like(&t);
        assert!(re, "{t}");
    }

    fn regex_like(t: &str) -> bool {
        // yyyy-MM-ddTHH:mm:ss.fffffff+hh:mm
        let b = t.as_bytes();
        t.len() == 33 && b[4] == b'-' && b[10] == b'T' && b[19] == b'.' && (b[27] == b'+' || b[27] == b'-') && b[30] == b':'
    }

    #[test]
    fn entries_redact_paths_and_end_with_a_blank_line() {
        let e = entry("T", r"failed to open C:\Users\me\Secret Film.mp4");
        assert_eq!(e, "T\nfailed to open <path>\n\n");
    }

    #[test]
    fn append_writes_to_the_given_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("crash.log");
        assert!(append_to(&p, "boom"));
        assert!(append_to(&p, "again"));
        let text = std::fs::read_to_string(&p).unwrap();
        assert_eq!(text.matches("\n\n").count(), 2);
        assert!(text.contains("\nboom\n\n") && text.ends_with("\nagain\n\n"));
    }

    #[test]
    fn with_a_diagnostic_log_entries_go_to_its_crash_file() {
        let dir = tempfile::tempdir().unwrap();
        let log = DiagnosticLog::new(
            dir.path().join("SubtitleAdder-diagnostics.log"),
            sa_core::diagnostics::DiagnosticFormat::Windows,
            Box::new(Vec::new),
        );
        log.wait_for_startup_scrub();
        set_diagnostic_log(log.clone());
        assert!(append(r"panic: D:\Films\x.mkv missing"));
        let text = std::fs::read_to_string(log.crash_log_path()).unwrap();
        assert!(text.ends_with("\npanic: <path>\n\n"), "{text}");
    }
}
