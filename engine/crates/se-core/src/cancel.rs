//! Process-wide cancellation: set once the engine's stdout is gone (the app quit or closed the pipe), so long
//! loops (translation batches, model and runtime downloads) stop promptly instead of paying for cloud tokens or
//! downloading gigabytes whose result nobody can read.
//!
//! The CLI requests it on the first failed stdout write; the loops call [`check`] between units of work. The
//! error [`check`] returns carries the text of that write error, so even if it surfaced it would read the same
//! as the error the CLI already reports.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::{EngineError, ErrorKind, Result};

static CANCELLED: AtomicBool = AtomicBool::new(false);
static REASON: Mutex<String> = Mutex::new(String::new());

/// Requests cancellation; `reason` is the text of the error that caused it. The first reason is kept.
pub fn request(reason: impl Into<String>) {
    let mut guard = REASON.lock().unwrap_or_else(|p| p.into_inner());
    if !CANCELLED.load(Ordering::SeqCst) {
        *guard = reason.into();
        CANCELLED.store(true, Ordering::SeqCst);
    }
}

/// Has cancellation been requested?
pub fn requested() -> bool {
    CANCELLED.load(Ordering::SeqCst)
}

/// `Err` (kind `Os`, the stored reason) once cancellation was requested.
pub fn check() -> Result<()> {
    if !requested() {
        return Ok(());
    }
    let reason = REASON.lock().unwrap_or_else(|p| p.into_inner()).clone();
    Err(EngineError::new(ErrorKind::Os, reason))
}

/// Clears the flag (each CLI invocation starts uncancelled; tests run many in one process).
pub fn reset() {
    let mut guard = REASON.lock().unwrap_or_else(|p| p.into_inner());
    CANCELLED.store(false, Ordering::SeqCst);
    guard.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[serial_test::serial(cancel)]
    fn first_reason_wins_and_reset_clears() {
        reset();
        assert!(check().is_ok());
        request("[Errno 32] Broken pipe");
        request("second");
        let err = check().unwrap_err();
        assert_eq!(err.kind, ErrorKind::Os);
        assert_eq!(err.message, "[Errno 32] Broken pipe");
        reset();
        assert!(!requested());
        assert!(check().is_ok());
    }
}
