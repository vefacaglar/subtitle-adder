//! ONNX Runtime shared library (load-dynamic). One onnxruntime per process: the one shipped with sherpa-onnx.
//!
//! Search order: `ORT_DYLIB_PATH` (a file), else next to the executable, else `$SUBTITLE_ENGINE_SHERPA_DIR`
//! (dev: `engine/.venv/Lib/site-packages/sherpa_onnx/lib`); see [`se_core::fsutil::find_native_library`].

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use ort::environment::Environment;
use ort::session::Session;
use ort::session::builder::SessionBuilder;
use se_core::{EngineError, Result};

/// Platform file name of the onnxruntime library (`onnxruntime.dll`, `libonnxruntime.dylib`, `libonnxruntime.so`).
pub const ORT_LIBRARY_NAME: &str = if cfg!(windows) {
    "onnxruntime.dll"
} else if cfg!(target_os = "macos") {
    "libonnxruntime.dylib"
} else {
    "libonnxruntime.so"
};

/// The onnxruntime library to load, or `None` if none is found.
pub fn locate() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("ORT_DYLIB_PATH") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    se_core::fsutil::find_native_library(ORT_LIBRARY_NAME)
}

static ENV: OnceLock<std::result::Result<Environment, String>> = OnceLock::new();

/// Initializes `ort` with the located library once per process (idempotent).
pub fn init() -> Result<()> {
    environment().map(|_| ())
}

/// The process-wide ONNX Runtime environment.
pub fn environment() -> Result<&'static Environment> {
    let env = ENV.get_or_init(|| {
        let path = locate().ok_or_else(|| format!("{ORT_LIBRARY_NAME} not found"))?;
        let builder = ort::init_from(&path).map_err(|e| e.to_string())?;
        builder.with_name("subtitle-engine").build().map_err(|e| e.to_string())
    });
    env.as_ref().map_err(|e| EngineError::runtime(e.clone()))
}

/// Converts an `ort` error into the engine error (`RuntimeError`-like text).
pub fn ort_err<E: std::fmt::Display>(e: E) -> EngineError {
    EngineError::runtime(e.to_string())
}

/// A session builder on the shared environment.
pub fn builder() -> Result<SessionBuilder> {
    Session::builder(environment()?).map_err(ort_err)
}

/// A session with ORT defaults (what onnx-asr gets with `providers=["CPUExecutionProvider"]` and no options).
pub fn default_session(model: &Path) -> Result<Session> {
    builder()?.commit_from_file(model).map_err(ort_err)
}
