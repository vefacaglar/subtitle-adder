//! Dependency injection for the CLI and the pipeline (spec cli.md §7.2): everything the Python tests
//! monkeypatch. Every method has the production behavior as its default, so a test implementation overrides
//! only what it fakes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use se_asr::Transcriber;
use se_audio::{AudioBackend, FfmpegBackend, PreviewSource};
use se_core::{ProgressFn, Result};
use se_diarize::Diarizer;
use se_translate::Translator;

/// Injectable engine services.
pub trait Services {
    /// `transcribe.create_transcriber(model_id)`: catalog `onnx` backend →
    /// [`Services::create_onnx_transcriber`]`(info.onnx_model or model_id, info.quantization)`, anything else →
    /// [`Services::create_whisper_transcriber`]`(model_id)` (the dispatch of `se_asr::create_transcriber`, kept
    /// here so tests can replace the two constructors like the Python tests patch the classes).
    fn create_transcriber(&self, model_id: &str) -> Result<Box<dyn Transcriber>> {
        match se_models::find(model_id) {
            Some(info) if info.backend == se_models::Backend::Onnx => {
                self.create_onnx_transcriber(info.onnx_model.unwrap_or(model_id), info.quantization)
            }
            _ => self.create_whisper_transcriber(model_id),
        }
    }

    /// `FasterWhisperTranscriber(model_id)` (device auto, default compute type).
    fn create_whisper_transcriber(&self, model_id: &str) -> Result<Box<dyn Transcriber>> {
        Ok(Box::new(se_asr::FasterWhisperTranscriber::new(model_id, se_asr::Device::Auto, None)?))
    }

    /// `OnnxAsrTranscriber(model_name, quantization)`.
    fn create_onnx_transcriber(&self, model_name: &str, quantization: Option<&str>) -> Result<Box<dyn Transcriber>> {
        Ok(Box::new(se_asr::OnnxAsrTranscriber::new(model_name, quantization)?))
    }

    /// `diarize.create_diarizer()`.
    fn create_diarizer(&self) -> Result<Box<dyn Diarizer>> {
        se_diarize::create_diarizer()
    }

    /// `translate.create_translator(model_id)` (device auto, compute type int8).
    fn create_translator(&self, model_id: &str) -> Result<Box<dyn Translator>> {
        se_translate::create_translator(model_id, se_translate::Device::Auto, "int8")
    }

    /// `models.is_downloaded`.
    fn is_downloaded(&self, model_id: &str) -> bool {
        se_models::is_downloaded(model_id)
    }

    /// `models.needs_download` (Python tests patch it separately from `is_downloaded`).
    fn needs_download(&self, model_id: &str) -> bool {
        se_models::find(model_id).is_some() && !self.is_downloaded(model_id)
    }

    /// `models.require_download` built on [`Services::needs_download`].
    fn require_download(&self, model_id: &str) -> Result<bool> {
        let missing = self.needs_download(model_id);
        if missing && se_core::offline::offline_requested() {
            return Err(se_core::EngineError::value(se_core::t!(
                "offline_missing_model",
                model = se_core::i18n::display_name(model_id)
            )));
        }
        Ok(missing)
    }

    /// `models.ensure_model`.
    fn ensure_model(&self, model_id: &str, on_progress: Option<ProgressFn<'_>>) -> Result<()> {
        se_models::ensure_model(model_id, on_progress).map(|_| ())
    }

    /// `models.cached_disk_mb()` (CLI `models`).
    fn cached_disk_mb(&self) -> BTreeMap<String, i64> {
        se_models::cached_disk_mb(None)
    }

    /// `models.disk_mb(id, sizes)`.
    fn disk_mb(&self, model_id: &str, sizes: &BTreeMap<String, i64>) -> Option<i64> {
        se_models::disk_mb(model_id, Some(sizes))
    }

    /// `models.delete_model(id)`.
    fn delete_model(&self, model_id: &str) -> Result<i64> {
        se_models::delete_model(model_id, None)
    }

    /// `cuda.needs_cuda_download()` (conftest stubs it to false).
    fn needs_cuda_download(&self) -> bool {
        se_models::cuda::needs_cuda_download()
    }

    /// `cuda.ensure_cuda(on_progress)`.
    fn ensure_cuda(&self, on_progress: Option<ProgressFn<'_>>) -> bool {
        se_models::cuda::ensure_cuda(on_progress)
    }

    /// `llama.require_llama_download()`.
    fn require_llama_download(&self) -> Result<bool> {
        se_models::llama::require_llama_download()
    }

    /// `llama.ensure_llama(on_progress)`.
    fn ensure_llama(&self, on_progress: Option<ProgressFn<'_>>) -> Result<PathBuf> {
        se_models::llama::ensure_llama(on_progress)
    }

    /// Audio extraction + rename (`pipeline.extract_audio`, `Path.replace`).
    fn audio(&self) -> &dyn AudioBackend {
        &FfmpegBackend
    }

    /// `preview.make_preview`.
    fn make_preview(&self, source: PreviewSource, output: &Path) -> Result<()> {
        se_audio::make_preview(source, output)
    }

    /// Monotonic clock for the partial-event throttle (0.5 s).
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// Production services.
#[derive(Clone, Copy, Debug, Default)]
pub struct Real;

impl Services for Real {}
