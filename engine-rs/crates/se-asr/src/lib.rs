//! Speech-to-text (port of `transcribe.py` + the parts of faster-whisper and onnx-asr the engine uses;
//! spec: `spec/transcribe.md`).
//!
//! * [`whisper`]: `FasterWhisperTranscriber` reimplemented on CTranslate2 (`ct2rs` sys API), with [`fw`]
//!   (`WhisperModel.transcribe`), [`mel`] (feature extractor), [`tokenizer`], [`fw_vad`] (silero VAD v6):
//!   temperature fallback, word timestamps, >180 s chunking with checkpoint.
//! * [`onnx`]: `OnnxAsrTranscriber` (Parakeet TDT v3, Canary 1B v2, GigaAM v3) on ONNX Runtime (`ort`,
//!   load-dynamic) with onnx-asr's silero VAD ([`onnx_asr`]).
//! * [`chunks`]: long-WAV chunking that never cuts speech, and the ASR checkpoint files.
//! * [`ort_runtime`]: locating and initializing the onnxruntime shared library (shared with sherpa-onnx).

use std::path::Path;

use se_core::{ProgressFn, Result, Transcript, WordsFn};

pub mod chunks;
pub mod fw;
pub mod fw_vad;
pub mod hf;
pub mod mel;
pub mod onnx;
pub mod onnx_asr;
pub mod ort_runtime;
pub mod tokenizer;
pub mod wave;
pub mod whisper;

pub use onnx::OnnxAsrTranscriber;
pub use whisper::{Device, FasterWhisperTranscriber};

/// A speech recognizer (Python `Transcriber` protocol).
pub trait Transcriber {
    /// Transcribe a 16 kHz mono WAV.
    ///
    /// * `language`: `None` = auto-detect; `Some("")` is passed through as Python does (`-l ""`).
    /// * `on_progress`: fractions in `0..=1` while transcribing.
    /// * `on_words`: a full snapshot of all words so far plus the language label (`"?"` until known).
    fn transcribe(
        &mut self,
        audio: &Path,
        language: Option<&str>,
        on_progress: Option<ProgressFn<'_>>,
        on_words: Option<WordsFn<'_>>,
    ) -> Result<Transcript>;

    /// Whether `on_words` is used (Python checks the signature; test fakes return `false` so the pipeline
    /// does not install its partial-event handler). Real transcribers return `true`.
    fn supports_on_words(&self) -> bool {
        true
    }
}

impl Transcriber for FasterWhisperTranscriber {
    fn transcribe(
        &mut self,
        audio: &Path,
        language: Option<&str>,
        on_progress: Option<ProgressFn<'_>>,
        on_words: Option<WordsFn<'_>>,
    ) -> Result<Transcript> {
        self.run(audio, language, on_progress, on_words)
    }
}

/// The backend `create_transcriber` picks for a model id: `Some((onnx_model, quantization))` for the catalog's
/// onnx models, `None` for Whisper (catalog ids and names outside the catalog).
pub fn onnx_backend_for(model_id: &str) -> Option<(&str, Option<&'static str>)> {
    let info = se_models::find(model_id)?;
    (info.backend == se_models::Backend::Onnx).then(|| (info.onnx_model.unwrap_or(model_id), info.quantization))
}

/// `create_transcriber` with injected constructors (tests).
pub fn create_transcriber_with(
    model_id: &str,
    whisper: &mut dyn FnMut(&str) -> Result<Box<dyn Transcriber>>,
    onnx: &mut dyn FnMut(&str, Option<&str>) -> Result<Box<dyn Transcriber>>,
) -> Result<Box<dyn Transcriber>> {
    match onnx_backend_for(model_id) {
        Some((name, quantization)) => onnx(name, quantization),
        None => whisper(model_id),
    }
}

/// `create_transcriber(model_id)`: catalog `onnx` backend → [`OnnxAsrTranscriber`] with
/// `info.onnx_model.unwrap_or(model_id)` and `info.quantization`; anything else (catalog Whisper ids and
/// names outside the catalog such as `tiny`, `turbo`) → [`FasterWhisperTranscriber`] (device auto).
/// Does not check `kind` (the pipeline did).
pub fn create_transcriber(model_id: &str) -> Result<Box<dyn Transcriber>> {
    create_transcriber_with(
        model_id,
        &mut |id| Ok(Box::new(FasterWhisperTranscriber::new(id, Device::Auto, None)?) as Box<dyn Transcriber>),
        &mut |name, q| Ok(Box::new(OnnxAsrTranscriber::new(name, q)?) as Box<dyn Transcriber>),
    )
}

/// `words_from_tokens(tokens, timestamps, segment_start, segment_end)` (onnx-asr token → word assembly);
/// re-exported for tests.
pub use onnx::words_from_tokens;
