//! Port of `tests/test_offline.py`.

mod common;

use serial_test::serial;

use common::*;
use se_cli::Services;
use se_core::offline;

#[test]
#[serial]
fn test_go_offline_blocks_hub_requests() {
    let _g = Guard::new();
    offline::go_offline();
    assert!(offline::hub_offline());
    assert_eq!(std::env::var("HF_HUB_OFFLINE").as_deref(), Ok("1"));
    assert_eq!(std::env::var("HF_HUB_DISABLE_TELEMETRY").as_deref(), Ok("1"));
}

#[test]
#[serial]
fn test_local_commands_lock_the_network_out() {
    let _g = Guard::new();
    let (code, _, _) = run(&Fake::default(), &["models", "--json"]);
    assert_eq!(code, 0);
    assert!(offline::hub_offline());
}

#[test]
#[serial]
fn test_transcribe_does_not_lock_the_network_before_its_downloads() {
    let _g = Guard::new();
    // Validation fails before any download; the network lock must still be open.
    let (code, _, _) = run(&Fake::default(), &["transcribe", "x.mp4", "--speakers", "2", "--json"]);
    assert_eq!(code, 1);
    assert!(!offline::network_locked());
}

fn strict_offline(flag: &str) {
    let g = Guard::new();
    let tmp = g.path();
    set_env("SUBTITLE_ENGINE_LANG", "en");
    let fake = Fake { needs: None, downloaded: Box::new(|_| false), ..Fake::default() };
    fake.audio.mode.set(ExtractMode::Forbidden); // "must not start work"; ensure_fractions None: no download
    let video = se_core::paths::py_path_str(&tmp.join("video.mp4"));
    let out = se_core::paths::py_path_str(&tmp);
    let mut args = vec!["transcribe", video.as_str(), "-m", "base", "-o", out.as_str(), "--json"];
    if flag == "env" {
        set_env(offline::OFFLINE_ENV_VAR, "1");
    } else {
        args.insert(0, "--offline");
    }
    let (code, stdout, _) = run(&fake, &args);
    assert_eq!(code, 1);
    let event = events(&stdout).pop().unwrap();
    assert_eq!(ev_str(&event, "event"), Some("error"));
    assert!(ev_str(&event, "message").unwrap().contains("offline mode"), "{stdout}");
}

#[test]
#[serial]
fn test_strict_offline_reports_missing_model_before_any_work_env() {
    strict_offline("env");
}

#[test]
#[serial]
fn test_strict_offline_reports_missing_model_before_any_work_cli() {
    strict_offline("cli");
}

#[test]
#[serial]
fn test_strict_offline_message_is_turkish_by_default() {
    let _g = Guard::new();
    set_env(offline::OFFLINE_ENV_VAR, "1");
    let fake = Fake { needs: None, downloaded: Box::new(|_| false), ..Fake::default() };
    let err = fake.require_download("base").unwrap_err();
    assert!(err.to_string().contains("Çevrimdışı mod"), "{err}");
}

#[test]
#[serial]
fn test_cached_models_need_no_network_and_downloads_stay_allowed() {
    let _g = Guard::new();
    let cached = Fake { needs: None, downloaded: Box::new(|_| true), ..Fake::default() };
    assert!(!cached.require_download("base").unwrap());
    let missing = Fake { needs: None, downloaded: Box::new(|_| false), ..Fake::default() };
    assert!(missing.require_download("base").unwrap());
    assert!(!offline::hub_offline());
}
