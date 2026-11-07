//! The engine error type. `Display` is exactly the text Python's `str(exc)` would give, because the CLI
//! writes it into `{"event": "error", "message": ...}` or `hata: ...`.

use std::fmt;
use std::io;
use std::path::Path;

/// Which Python exception class the error corresponds to (for tests and for callers that branch on it,
/// e.g. "retry on RuntimeError"). It never changes the message text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// `models.ModelError` (localized).
    Model,
    /// `ValueError` (localized engine validation errors, e.g. `speakers_min`).
    Value,
    /// `audio.AudioExtractionError` (localized).
    AudioExtraction,
    /// `OSError` and subclasses (`[Errno N] ...` / `[WinError N] ...`).
    Os,
    /// `json.JSONDecodeError`.
    Json,
    /// `KeyError` (message is the `repr` of the key, e.g. `'id'`).
    Key,
    /// `TypeError` / `AttributeError` raised by Python on bad input shapes.
    Type,
    /// `UnicodeDecodeError` (stdin is not UTF-8).
    Unicode,
    /// `RuntimeError` from a native library (CTranslate2, ONNX Runtime, FFmpeg, sherpa-onnx).
    Runtime,
    /// A network/HTTP failure (urllib `URLError`/`HTTPError`).
    Network,
    /// Anything else.
    Other,
}

/// An engine error: a kind plus the final user-visible message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineError {
    pub kind: ErrorKind,
    pub message: String,
}

/// `Result` with [`EngineError`].
pub type Result<T, E = EngineError> = std::result::Result<T, E>;

impl EngineError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into() }
    }
    /// `ModelError(message)`.
    pub fn model(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Model, message)
    }
    /// `ValueError(message)`.
    pub fn value(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Value, message)
    }
    /// `AudioExtractionError(message)`.
    pub fn audio(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::AudioExtraction, message)
    }
    /// `RuntimeError(message)`.
    pub fn runtime(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Runtime, message)
    }
    /// `TypeError` / `AttributeError` with Python's text.
    pub fn type_error(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Type, message)
    }
    /// `KeyError(key)`: the message is `repr(key)` (`'start'`).
    pub fn key(key: &str) -> Self {
        Self::new(ErrorKind::Key, crate::pyfmt::repr_str(key))
    }
    /// An `OSError` from the C runtime (`open()` & co.): `[Errno 2] No such file or directory: 'x'`.
    pub fn os(err: &io::Error, filename: Option<&Path>) -> Self {
        Self::new(ErrorKind::Os, crate::pyfmt::oserror_text(err, filename, None))
    }
    /// An `OSError` from a Win32 call (`os.replace`, `os.unlink`, ...): `[WinError 5] Access is denied: 'x'`
    /// on Windows, same as [`EngineError::os`] elsewhere.
    pub fn win(err: &io::Error, filename: Option<&Path>) -> Self {
        Self::new(ErrorKind::Os, crate::pyfmt::winerror_text(err, filename, None))
    }

    pub fn is(&self, kind: ErrorKind) -> bool {
        self.kind == kind
    }
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for EngineError {}

/// An `io::Error` without a file name: Python `str(OSError(errno, strerror))` = `[Errno N] text`.
impl From<io::Error> for EngineError {
    fn from(err: io::Error) -> Self {
        EngineError::os(&err, None)
    }
}

impl From<crate::json::JsonDecodeError> for EngineError {
    fn from(err: crate::json::JsonDecodeError) -> Self {
        EngineError::new(ErrorKind::Json, err.to_string())
    }
}
