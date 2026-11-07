//! Strict offline mode and the Hugging Face network lock (`models.offline_requested` / `models.go_offline`).
//!
//! * Strict offline mode (`--offline` or `SUBTITLE_ENGINE_OFFLINE=1`): nothing may be downloaded; a missing
//!   model gives a localized error before any work.
//! * The network lock ([`go_offline`]): once the models a command needs are on disk, every Hugging Face
//!   request is refused without I/O (`HF_HUB_OFFLINE=1`, `HF_HUB_DISABLE_TELEMETRY=1` are exported so child
//!   processes inherit them). HF fetches in `se-models` must call [`hub_offline`] and fail with
//!   [`hub_offline_message`] when it is true.

use std::sync::atomic::{AtomicBool, Ordering};

/// Strict offline environment variable.
pub const OFFLINE_ENV_VAR: &str = "SUBTITLE_ENGINE_OFFLINE";

static NETWORK_LOCKED: AtomicBool = AtomicBool::new(false);

fn env_in(name: &str, values: &[&str]) -> bool {
    match std::env::var(name) {
        Ok(v) => {
            let v = crate::pyfmt::strip(&v).to_lowercase();
            values.contains(&v.as_str())
        }
        Err(_) => false,
    }
}

/// `models.offline_requested()`: `SUBTITLE_ENGINE_OFFLINE` (strip + lower) is `1`, `true`, `yes` or `on`.
pub fn offline_requested() -> bool {
    env_in(OFFLINE_ENV_VAR, &["1", "true", "yes", "on"])
}

/// Sets `SUBTITLE_ENGINE_OFFLINE=1` in the process environment (the CLI's `--offline`; inherited by children).
///
/// Must be called before other threads start (environment mutation is not thread-safe).
pub fn request_offline() {
    // SAFETY: called from the CLI main thread before any worker thread is spawned (and from serial tests).
    unsafe { std::env::set_var(OFFLINE_ENV_VAR, "1") };
}

/// `models.go_offline()`: lock the network for Hugging Face and export `HF_HUB_OFFLINE=1`,
/// `HF_HUB_DISABLE_TELEMETRY=1`. Idempotent.
pub fn go_offline() {
    NETWORK_LOCKED.store(true, Ordering::SeqCst);
    // SAFETY: see `request_offline`.
    unsafe {
        std::env::set_var("HF_HUB_OFFLINE", "1");
        std::env::set_var("HF_HUB_DISABLE_TELEMETRY", "1");
    }
}

/// Is Hugging Face offline: [`go_offline`] was called, or `HF_HUB_OFFLINE` / `TRANSFORMERS_OFFLINE` is set to a
/// true value (`1`, `ON`, `YES`, `TRUE`, case-insensitive) in the environment, as huggingface_hub reads it.
pub fn hub_offline() -> bool {
    NETWORK_LOCKED.load(Ordering::SeqCst)
        || env_in("HF_HUB_OFFLINE", &["1", "on", "yes", "true"])
        || env_in("TRANSFORMERS_OFFLINE", &["1", "on", "yes", "true"])
}

/// Only the in-process lock (without the environment).
pub fn network_locked() -> bool {
    NETWORK_LOCKED.load(Ordering::SeqCst)
}

/// huggingface_hub's `OfflineModeIsEnabled` text for a refused request.
pub fn hub_offline_message(url: &str) -> String {
    format!(
        "Cannot reach {url}: offline mode is enabled. To disable it, please unset the `HF_HUB_OFFLINE` environment variable."
    )
}

/// Test helper (conftest `_restore_hub_offline_state`): unlock and remove `HF_HUB_OFFLINE`,
/// `HF_HUB_DISABLE_TELEMETRY` and `SUBTITLE_ENGINE_OFFLINE` from the environment.
pub fn reset_for_tests() {
    NETWORK_LOCKED.store(false, Ordering::SeqCst);
    // SAFETY: tests touching the environment run serially.
    unsafe {
        std::env::remove_var("HF_HUB_OFFLINE");
        std::env::remove_var("HF_HUB_DISABLE_TELEMETRY");
        std::env::remove_var(OFFLINE_ENV_VAR);
    }
}
