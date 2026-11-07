//! CLI part of `tests/test_i18n.py`.

use std::collections::BTreeMap;
use std::ffi::OsString;

use serial_test::serial;

use se_cli::{Io, Real, Services, main_with};
use se_core::i18n;

struct Guard;

impl Guard {
    fn new() -> Self {
        // SAFETY: #[serial] tests.
        unsafe { std::env::remove_var(i18n::ENV_VAR) };
        i18n::set_language(None);
        se_core::offline::reset_for_tests();
        Guard
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        unsafe { std::env::remove_var(i18n::ENV_VAR) };
        i18n::set_language(None);
        se_core::offline::reset_for_tests();
    }
}

fn set_env(value: &str) {
    unsafe { std::env::set_var(i18n::ENV_VAR, value) };
}

/// Runs the CLI in-process; returns (exit code, stdout, stderr).
fn run(services: &dyn Services, args: &[&str]) -> (i32, String, String) {
    let argv: Vec<OsString> = args.iter().map(OsString::from).collect();
    let mut stdin: &[u8] = b"";
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = {
        let mut io = Io { stdin: &mut stdin, stdout: &mut stdout, stderr: &mut stderr };
        main_with(&argv, services, &mut io)
    };
    (code, String::from_utf8(stdout).unwrap(), String::from_utf8(stderr).unwrap())
}

fn message(stdout: &str) -> String {
    let v = se_core::json::parse(stdout.trim_end()).unwrap();
    v.get("message").unwrap().as_str().unwrap().to_string()
}

#[test]
#[serial]
fn test_cli_error_event_follows_env() {
    let _g = Guard::new();
    set_env("en");
    let (code, out, _) = run(&Real, &["delete-model", "nope", "--json"]);
    assert_eq!(code, 1);
    assert_eq!(out, "{\"event\": \"error\", \"message\": \"Unknown model: nope. The 'models' command lists the valid ids.\"}\n");
}

#[test]
#[serial]
fn test_cli_lang_flag_overrides_env() {
    let _g = Guard::new();
    set_env("en");
    let (code, out, _) = run(&Real, &["--lang", "tr", "delete-model", "nope", "--json"]);
    assert_eq!(code, 1);
    assert!(message(&out).contains("Bilinmeyen model: nope"));
}

#[test]
#[serial]
fn test_cli_lang_flag_invalid_value_means_turkish() {
    let _g = Guard::new();
    let (code, out, _) = run(&Real, &["--lang", "xx", "delete-model", "nope", "--json"]);
    assert_eq!(code, 1);
    assert!(message(&out).contains("Bilinmeyen model"));
}

#[test]
#[serial]
fn test_cli_lang_flag_does_not_leak_between_calls() {
    let _g = Guard::new();
    run(&Real, &["--lang", "en", "delete-model", "nope", "--json"]);
    let (_, out, _) = run(&Real, &["delete-model", "nope", "--json"]);
    assert!(message(&out).contains("Bilinmeyen model"));
}

#[test]
#[serial]
fn test_speakers_without_diarize_error_english() {
    let _g = Guard::new();
    let (code, out, _) = run(&Real, &["--lang", "en", "transcribe", "x.mp4", "--speakers", "2", "--json"]);
    assert_eq!(code, 1);
    assert_eq!(message(&out), "--speakers can only be used with --diarize.");
}

struct NothingCached;

impl Services for NothingCached {
    fn cached_disk_mb(&self) -> BTreeMap<String, i64> {
        BTreeMap::new()
    }
    fn is_downloaded(&self, _model_id: &str) -> bool {
        false
    }
    fn disk_mb(&self, _model_id: &str, _sizes: &BTreeMap<String, i64>) -> Option<i64> {
        None
    }
}

#[test]
#[serial]
fn test_models_table_headers_english() {
    let _g = Guard::new();
    let (code, out, _) = run(&NothingCached, &["--lang", "en", "models"]);
    assert_eq!(code, 0);
    let header = out.lines().next().unwrap();
    assert!(header.contains("accuracy") && header.contains("cache") && !header.contains("doğruluk"));
}

#[test]
#[serial]
fn test_cli_lang_flag_after_subcommand() {
    let _g = Guard::new();
    let (code, out, _) = run(&Real, &["delete-model", "nope", "--lang", "en", "--json"]);
    assert_eq!(code, 1);
    assert!(message(&out).contains("Unknown model: nope"));
}

#[test]
#[serial]
fn test_subcommand_lang_wins_over_global() {
    let _g = Guard::new();
    let (code, out, _) = run(&Real, &["--lang", "tr", "delete-model", "nope", "--lang", "en", "--json"]);
    assert_eq!(code, 1);
    assert!(message(&out).contains("Unknown model: nope"));
    let (code, out, _) = run(&Real, &["--lang", "en", "delete-model", "nope", "--json"]);
    assert_eq!(code, 1);
    assert!(message(&out).contains("Unknown model: nope"));
}
