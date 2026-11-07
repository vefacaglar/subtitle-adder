//! `FasterWhisperTranscriber` on CTranslate2 (spec transcribe.md §2-§4).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use se_core::{ErrorKind, EngineError, ProgressFn, Result, Transcript, Word, WordsFn};

use crate::chunks::{self, ChunkConfig, WavChunks};
use crate::fw::{AudioInput, FwSegment, TranscribeOptions, WhisperBackend, WhisperModel};
use crate::fw_vad::PyF;

/// `WHISPER_VAD_OPTIONS` (faster-whisper `VadOptions` overrides).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WhisperVadOptions {
    pub threshold: f32,
    pub neg_threshold: f32,
    pub min_speech_duration_ms: u32,
    pub max_speech_duration_s: f32,
    pub min_silence_duration_ms: u32,
    pub speech_pad_ms: u32,
}

/// The engine's VAD settings: threshold 0.35, neg 0.20, min speech 120 ms, max speech 14 s,
/// min silence 250 ms, pad 200 ms. (Thresholds are compared in f32 as NumPy 2 does.)
pub const WHISPER_VAD_OPTIONS: WhisperVadOptions = WhisperVadOptions {
    threshold: 0.35,
    neg_threshold: 0.20,
    min_speech_duration_ms: 120,
    max_speech_duration_s: 14.0,
    min_silence_duration_ms: 250,
    speech_pad_ms: 200,
};

/// Decoder options the engine passes to `WhisperModel.transcribe` (both paths). Python floats: compared in f64.
pub const NO_SPEECH_THRESHOLD: f64 = 0.8;
pub const LOG_PROB_THRESHOLD: f64 = -1.2;
pub const HALLUCINATION_SILENCE_THRESHOLD: f64 = 1.5;

/// The full `model.transcribe(...)` keyword set of the engine.
pub fn engine_options() -> TranscribeOptions {
    TranscribeOptions {
        word_timestamps: true,
        vad_filter: true,
        vad_parameters: WHISPER_VAD_OPTIONS,
        condition_on_previous_text: false,
        no_speech_threshold: NO_SPEECH_THRESHOLD,
        log_prob_threshold: LOG_PROB_THRESHOLD,
        hallucination_silence_threshold: HALLUCINATION_SILENCE_THRESHOLD,
    }
}

/// Inference device (`device="auto"`: CUDA if `cuda_usable()`, else CPU).
pub use se_models::cuda::Device;

/// Number of CUDA devices CTranslate2 sees (`ctranslate2.get_cuda_device_count()`); 0 on CPU-only builds.
/// Install it into `se-models` with `se_models::cuda::set_gpu_probe(se_asr::whisper::cuda_device_count)`.
pub fn cuda_device_count() -> usize {
    ct2rs::sys::get_device_count(ct2rs::Device::CUDA).max(0) as usize
}

/// faster-whisper's `_MODELS` (order matters for the error text).
pub const MODEL_REPOS: [(&str, &str); 19] = [
    ("tiny.en", "Systran/faster-whisper-tiny.en"),
    ("tiny", "Systran/faster-whisper-tiny"),
    ("base.en", "Systran/faster-whisper-base.en"),
    ("base", "Systran/faster-whisper-base"),
    ("small.en", "Systran/faster-whisper-small.en"),
    ("small", "Systran/faster-whisper-small"),
    ("medium.en", "Systran/faster-whisper-medium.en"),
    ("medium", "Systran/faster-whisper-medium"),
    ("large-v1", "Systran/faster-whisper-large-v1"),
    ("large-v2", "Systran/faster-whisper-large-v2"),
    ("large-v3", "Systran/faster-whisper-large-v3"),
    ("large", "Systran/faster-whisper-large-v3"),
    ("distil-large-v2", "Systran/faster-distil-whisper-large-v2"),
    ("distil-medium.en", "Systran/faster-distil-whisper-medium.en"),
    ("distil-small.en", "Systran/faster-distil-whisper-small.en"),
    ("distil-large-v3", "Systran/faster-distil-whisper-large-v3"),
    ("distil-large-v3.5", "distil-whisper/distil-large-v3.5-ct2"),
    ("large-v3-turbo", "mobiuslabsgmbh/faster-whisper-large-v3-turbo"),
    ("turbo", "mobiuslabsgmbh/faster-whisper-large-v3-turbo"),
];

/// `download_model(size_or_id)` repo id: anything with a `/` is a repo id, else the `_MODELS` alias.
pub fn model_repo(name: &str) -> Result<String> {
    if name.contains('/') {
        return Ok(name.to_string());
    }
    MODEL_REPOS.iter().find(|(k, _)| *k == name).map(|(_, r)| r.to_string()).ok_or_else(|| {
        EngineError::value(format!(
            "Invalid model size '{name}', expected one of: {}",
            MODEL_REPOS.iter().map(|(k, _)| *k).collect::<Vec<_>>().join(", ")
        ))
    })
}

/// Resolves a model name to a local folder: an existing directory, else the cached HF snapshot.
pub fn resolve_model_dir_with(model_size: &str, local_files_only: bool) -> Result<PathBuf> {
    let p = Path::new(model_size);
    if p.is_dir() {
        return Ok(p.to_path_buf());
    }
    let repo = model_repo(model_size)?;
    crate::hf::snapshot(&repo, &crate::hf::WHISPER_ALLOW_PATTERNS, local_files_only)
}

/// `WhisperModel(size, device, compute_type, local_files_only)` factory (a seam for tests).
pub type ModelFactory = Box<dyn Fn(&str, Device, &str, bool) -> Result<Box<dyn WhisperBackend>>>;

/// Everything the Python tests monkeypatch around the transcriber.
pub struct WhisperHooks {
    pub cuda_usable: Box<dyn Fn() -> bool>,
    pub register_libraries: Box<dyn Fn()>,
    pub is_downloaded: Box<dyn Fn(&str) -> bool>,
    pub model_factory: ModelFactory,
}

impl Default for WhisperHooks {
    fn default() -> Self {
        Self {
            cuda_usable: Box::new(se_models::cuda::cuda_usable),
            register_libraries: Box::new(se_models::cuda::register_libraries),
            is_downloaded: Box::new(se_models::cache::is_downloaded),
            model_factory: Box::new(|size, device, compute_type, local_files_only| {
                let dir = resolve_model_dir_with(size, local_files_only)?;
                Ok(Box::new(WhisperModel::load(&dir, device, compute_type)?) as Box<dyn WhisperBackend>)
            }),
        }
    }
}

/// faster-whisper port. CPU int8 on Mac; CUDA float16 on Windows/Linux when the CUDA libraries load (with
/// a CPU fallback on CUDA library errors), otherwise CPU int8.
pub struct FasterWhisperTranscriber {
    model_size: String,
    device: Device,
    compute_type: Option<String>,
    hooks: WhisperHooks,
    model: Option<Box<dyn WhisperBackend>>,
    /// Chunking parameters (tests shrink them).
    pub chunks: ChunkConfig,
    /// Root of the ASR checkpoint folder (`<temp>/SubtitleAdder/transcriptions` by default).
    pub checkpoint_root: Option<PathBuf>,
}

/// Monotonic progress (`reported` shared across the CPU retry).
struct Progress<'p> {
    cb: Option<ProgressFn<'p>>,
    reported: f64,
}

impl Progress<'_> {
    fn report(&mut self, fraction: f64) {
        if let Some(cb) = self.cb.as_mut() {
            if fraction >= self.reported {
                self.reported = fraction;
                cb(fraction);
            }
        }
    }
}

fn words_of(seg: &FwSegment, offset: f64) -> Vec<Word> {
    seg.words
        .iter()
        .filter_map(|w| {
            let text = se_core::pyfmt::strip(&w.word);
            if text.is_empty() {
                return None;
            }
            let start = PyF { v: offset + w.start.v, np: w.start.np }.round(3).v;
            let end = PyF { v: offset + w.end.v, np: w.end.np }.round(3).v;
            Some(Word::new(start, end, text))
        })
        .collect()
}

impl FasterWhisperTranscriber {
    /// `FasterWhisperTranscriber(model_size, device, compute_type)`: resolves the model locally (catalog
    /// models are already downloaded by the pipeline; unknown names map to faster-whisper's repo naming) and
    /// loads it. Errors: CTranslate2 load errors as `ErrorKind::Runtime`.
    pub fn new(model_size: &str, device: Device, compute_type: Option<&str>) -> Result<Self> {
        Self::with_hooks(model_size, device, compute_type, WhisperHooks::default())
    }

    /// [`FasterWhisperTranscriber::new`] with injected hooks (tests).
    pub fn with_hooks(model_size: &str, device: Device, compute_type: Option<&str>, hooks: WhisperHooks) -> Result<Self> {
        let device = match device {
            Device::Auto => {
                if (hooks.cuda_usable)() { Device::Cuda } else { Device::Cpu }
            }
            Device::Cuda => {
                (hooks.register_libraries)();
                Device::Cuda
            }
            Device::Cpu => Device::Cpu,
        };
        let mut me = Self {
            model_size: model_size.to_string(),
            device,
            compute_type: compute_type.map(str::to_string),
            hooks,
            model: None,
            chunks: ChunkConfig::default(),
            checkpoint_root: None,
        };
        me.load(device)?;
        Ok(me)
    }

    fn load(&mut self, device: Device) -> Result<()> {
        let compute_type = self
            .compute_type
            .clone()
            .unwrap_or_else(|| if device == Device::Cuda { "float16".into() } else { "int8".into() });
        self.device = device;
        self.model = None;
        let offline = (self.hooks.is_downloaded)(&self.model_size);
        self.model = Some((self.hooks.model_factory)(&self.model_size, device, &compute_type, offline)?);
        Ok(())
    }

    /// The device the model is loaded on (after `auto` resolution and a CPU fallback).
    pub fn device(&self) -> Device {
        self.device
    }

    /// Resolve a model name to a local CTranslate2 model folder (catalog id, faster-whisper alias such as
    /// `tiny`/`turbo`, `Org/repo`, or a local directory).
    pub fn resolve_model_dir(model_size: &str) -> Result<PathBuf> {
        resolve_model_dir_with(model_size, se_models::cache::is_downloaded(model_size))
    }

    fn transcribe_once(
        &mut self,
        audio: &Path,
        language: Option<&str>,
        progress: &mut Progress<'_>,
        on_words: &mut Option<WordsFn<'_>>,
    ) -> Result<Transcript> {
        if chunks::can_stream_wav_chunks(audio, &self.chunks, None) {
            return self.transcribe_chunked(audio, language, progress, on_words);
        }
        let options = engine_options();
        let model = self.model.as_mut().expect("model loaded");
        let (info, segments) = model.transcribe(AudioInput::Path(audio), language, &options)?;
        let detected = if !info.language.is_empty() {
            info.language.clone()
        } else {
            language.filter(|l| !l.is_empty()).unwrap_or("?").to_string()
        };
        let mut words: Vec<Word> = Vec::new();
        for seg in segments {
            let seg = seg?;
            let seg_words = words_of(&seg, 0.0);
            if !seg_words.is_empty() {
                words.extend(seg_words);
                if let Some(cb) = on_words.as_mut() {
                    cb(&words, &detected);
                }
            }
            if info.duration != 0.0 {
                progress.report((seg.end.v / info.duration).min(1.0));
            }
        }
        Ok(Transcript { language: info.language, duration: info.duration, words })
    }

    fn transcribe_chunked(
        &mut self,
        audio: &Path,
        language: Option<&str>,
        progress: &mut Progress<'_>,
        on_words: &mut Option<WordsFn<'_>>,
    ) -> Result<Transcript> {
        let root = self.checkpoint_root.clone().unwrap_or_else(chunks::default_checkpoint_root);
        let ckpt = chunks::checkpoint_path(&root, audio, &self.model_size, language);
        let (cached_lang, mut cached): (Option<String>, BTreeMap<i64, Vec<Word>>) =
            chunks::load_checkpoint(ckpt.as_deref());
        let options = engine_options();
        let mut words: Vec<Word> = Vec::new();
        let mut detected: Option<String> =
            language.filter(|l| !l.is_empty()).map(str::to_string).or(cached_lang.filter(|l| !l.is_empty()));
        let mut total_duration = 0.0;
        let reader = WavChunks::open(audio, &self.chunks)?;
        let wav_duration = reader.total_duration();
        let model = self.model.as_mut().expect("model loaded");
        for chunk in reader {
            let chunk = chunk?;
            total_duration = wav_duration;
            let idx = chunk.index as i64;
            let chunk_end = chunk.offset_seconds + chunk.samples.len() as f64 / 16_000.0;
            if let Some(cw) = cached.get(&idx) {
                words.extend(cw.iter().cloned());
                if let Some(cb) = on_words.as_mut() {
                    if !words.is_empty() {
                        cb(&words, detected.as_deref().unwrap_or("?"));
                    }
                }
                if total_duration > 0.0 {
                    progress.report((chunk_end / total_duration).min(1.0));
                }
                continue;
            }
            let samples = chunk.samples_f32();
            // `language or cached_lang`: an empty `-l ""` is falsy, so it auto-detects here.
            let (info, segments) = model.transcribe(AudioInput::Samples(&samples), detected.as_deref(), &options)?;
            if detected.is_none() && !info.language.is_empty() {
                detected = Some(info.language.clone());
            }
            let label = detected
                .clone()
                .or_else(|| (!info.language.is_empty()).then(|| info.language.clone()))
                .unwrap_or_else(|| "?".to_string());
            let mut chunk_words = Vec::new();
            for seg in segments {
                let seg = seg?;
                let seg_words = words_of(&seg, chunk.offset_seconds);
                if !seg_words.is_empty() {
                    chunk_words.extend(seg_words.iter().cloned());
                    words.extend(seg_words);
                    if let Some(cb) = on_words.as_mut() {
                        cb(&words, &label);
                    }
                }
                if total_duration > 0.0 {
                    progress.report(((chunk.offset_seconds + seg.end.v) / total_duration).min(1.0));
                }
            }
            cached.insert(idx, chunk_words);
            chunks::save_checkpoint(ckpt.as_deref(), detected.as_deref(), &cached);
            if total_duration > 0.0 {
                progress.report((chunk_end / total_duration).min(1.0));
            }
        }
        Ok(Transcript {
            language: detected.filter(|d| !d.is_empty()).unwrap_or_else(|| "?".to_string()),
            duration: total_duration,
            words,
        })
    }
}

impl FasterWhisperTranscriber {
    /// `FasterWhisperTranscriber.transcribe` (with the CUDA → CPU retry).
    pub fn run(
        &mut self,
        audio: &Path,
        language: Option<&str>,
        on_progress: Option<ProgressFn<'_>>,
        on_words: Option<WordsFn<'_>>,
    ) -> Result<Transcript> {
        let mut progress = Progress { cb: on_progress, reported: 0.0 };
        let mut on_words = on_words;
        match self.transcribe_once(audio, language, &mut progress, &mut on_words) {
            Err(e)
                if self.device == Device::Cuda
                    && e.is(ErrorKind::Runtime)
                    && se_models::cuda::is_cuda_library_error(&e.message) =>
            {
                eprintln!("CUDA unavailable, falling back to CPU: {e}");
                self.load(Device::Cpu)?;
                self.transcribe_once(audio, language, &mut progress, &mut on_words)
            }
            other => other,
        }
    }
}
