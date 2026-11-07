//! Shared core of the Rust subtitle engine (port of the Python `subtitle_engine` package).
//!
//! Everything here is pure Rust and has no native dependencies:
//!
//! * [`i18n`]: the full Turkish/English message table and [`i18n::t`] (Python `i18n.py`).
//! * [`json`]: a JSON [`json::Value`] that keeps the Python int/float distinction, a parser with Python
//!   `json.loads` error texts and a writer byte-compatible with `json.dumps` (both `ensure_ascii` modes,
//!   `indent`), plus the Python float `repr`.
//! * [`pyfmt`]: Python formatting helpers (`repr(float)`, `round()`, `str()`/`repr()` of values,
//!   `OSError` texts) and Python string predicates (`str.isspace`, `str.strip`).
//! * [`types`]: [`types::Word`], [`types::Sentence`], [`types::Transcript`] and [`types::Num`]
//!   (`models.py` dataclasses, including the raw-JSON-tolerant `Sentence.from_dict`).
//! * [`events`]: the stdout JSON event lines of the engine protocol (version 1).
//! * [`error`]: [`error::EngineError`], whose `Display` equals Python `str(exc)`.
//! * [`offline`]: strict offline mode and the process-wide network lock (`models.go_offline`).
//! * [`paths`]: Python `Path.resolve()`/`stem`/`str(path)` parity, the WAV naming helpers of `pipeline.py`.
//! * [`fsutil`]: atomic write / replace helpers, `tempfile.gettempdir()` parity, the process id.

pub mod error;
pub mod events;
pub mod fsutil;
pub mod i18n;
pub mod json;
pub mod offline;
pub mod paths;
pub mod pyfmt;
pub mod types;

pub use error::{EngineError, ErrorKind, Result};
pub use json::{Map, Value};
pub use types::{Num, Sentence, Transcript, Word};

/// Progress callback: a fraction in `0..=1` (Python `ProgressFn = Callable[[float], None]`).
pub type ProgressFn<'a> = &'a mut dyn FnMut(f64);

/// Stage callback of the pipeline / CLI: `(stage id, fraction or None)`.
///
/// Stage ids: `extracting_audio`, `downloading_model`, `loading_model`, `transcribing`, `diarizing`,
/// `segmenting`, `translating` (see [`events::stages`]).
pub type StageFn<'a> = &'a mut dyn FnMut(&str, Option<f64>);

/// Words callback of a transcriber: `(all words recognized so far, language label)`.
/// Always receives a full snapshot, never a delta. The label is `"?"` until the language is known.
pub type WordsFn<'a> = &'a mut dyn FnMut(&[Word], &str);

/// Partial-snapshot callback of the pipeline: `(sentences segmented from all words so far, language)`.
pub type PartialFn<'a> = &'a mut dyn FnMut(&[Sentence], &str);

/// Incremental translation callback: the growing prefix of translated sentences after every batch.
pub type BatchFn<'a> = &'a mut dyn FnMut(&[Sentence]);
