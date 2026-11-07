//! `Ct2Translator`: CTranslate2 translation of MADLAD-400 3B (SentencePiece, `<2xx>` prefix, no source) and
//! NLLB-200 1.3B (HF tokenizer, source/target language tokens). Spec translate-local.md §5.

use std::path::{Path, PathBuf};

use se_core::pyfmt::strip;
use se_core::{BatchFn, EngineError, ErrorKind, ProgressFn, Result, Sentence};
use se_models::ModelInfo;

use crate::{BEAM_SIZE, BATCH_SIZE, Device, EOS_TOKEN, MAX_DECODING_LENGTH, Translator, check_request, nllb_code};

/// Tokenizer seam (`_HfTokenizer` / `_SentencePieceTokenizer`; tests use a whitespace fake).
pub trait TextTokenizer {
    fn encode(&self, text: &str) -> Result<Vec<String>>;
    fn decode(&self, tokens: &[String]) -> Result<String>;
}

/// The per-call options the translator passes to [`Ct2Backend::translate_batch`] (Python keyword arguments;
/// every other CTranslate2 option is its Python default).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BatchOptions {
    pub beam_size: usize,
    pub max_decoding_length: usize,
    pub max_batch_size: usize,
}

/// Translation backend seam (`ctranslate2.Translator.translate_batch`; tests use a fake).
pub trait Ct2Backend {
    /// One result per source; each result is the (possibly empty) list of hypotheses (token lists).
    /// `target_prefix` holds one prefix per source when given.
    fn translate_batch(
        &mut self,
        sources: &[Vec<String>],
        target_prefix: Option<&[Vec<String>]>,
        options: &BatchOptions,
    ) -> Result<Vec<Vec<Vec<String>>>>;
}

/// Reloads the backend on a device (`_load_ct2_translator(directory, device, compute_type)`); used for the
/// CUDA → CPU fallback.
pub type BackendLoader = Box<dyn FnMut(Device) -> Result<Box<dyn Ct2Backend>>>;

/// The CTranslate2 translator.
pub struct Ct2Translator {
    info: &'static ModelInfo,
    device: Device,
    compute_type: String,
    tokenizer: Box<dyn TextTokenizer>,
    backend: Box<dyn Ct2Backend>,
    loader: Option<BackendLoader>,
    /// Lines per `translate_batch` call ([`BATCH_SIZE`]; tests lower it, as Python monkeypatches it).
    pub batch_size: usize,
}

fn lookup(model_id: &str) -> Result<&'static ModelInfo> {
    match se_models::find(model_id) {
        Some(info) if info.kind == se_models::ModelKind::Translation => Ok(info),
        _ => Err(EngineError::model(se_core::t!("unknown_translation_model_short", model = model_id))),
    }
}

impl Ct2Translator {
    /// Loads the model from the HF cache (`models::model_dir`) on `device` (auto: CUDA if usable) with
    /// `compute_type` (default `int8`), retrying on CPU after a CUDA library error.
    pub fn new(model_id: &str, device: Device, compute_type: &str) -> Result<Self> {
        let info = lookup(model_id)?;
        let device = device.resolve();
        let directory = se_models::cache::model_dir(model_id)?;
        let tokenizer = load_tokenizer(&directory, model_id)?;
        let compute = compute_type.to_string();
        let dir = directory.clone();
        let mut loader: BackendLoader = Box::new(move |device| {
            Ok(Box::new(RealCt2::load(&dir, device, &compute)?) as Box<dyn Ct2Backend>)
        });
        let backend = loader(device)?;
        Ok(Self {
            info,
            device,
            compute_type: compute_type.to_string(),
            tokenizer,
            backend,
            loader: Some(loader),
            batch_size: BATCH_SIZE,
        })
    }

    /// Test constructor with injected tokenizer and backend (device CPU, no reload).
    pub fn with_parts(model_id: &str, tokenizer: Box<dyn TextTokenizer>, backend: Box<dyn Ct2Backend>) -> Result<Self> {
        let info = lookup(model_id)?;
        Ok(Self {
            info,
            device: Device::Cpu,
            compute_type: "int8".to_string(),
            tokenizer,
            backend,
            loader: None,
            batch_size: BATCH_SIZE,
        })
    }

    /// Test constructor with an injected tokenizer, an already resolved `device` and a backend loader
    /// (exercises the CUDA → CPU fallback).
    pub fn with_loader(
        model_id: &str,
        tokenizer: Box<dyn TextTokenizer>,
        device: Device,
        mut loader: BackendLoader,
    ) -> Result<Self> {
        let info = lookup(model_id)?;
        let backend = loader(device)?;
        Ok(Self {
            info,
            device,
            compute_type: "int8".to_string(),
            tokenizer,
            backend,
            loader: Some(loader),
            batch_size: BATCH_SIZE,
        })
    }

    /// The device currently in use (`Cpu` after a CUDA fallback).
    pub fn device(&self) -> Device {
        self.device
    }

    /// The compute type passed at construction.
    pub fn compute_type(&self) -> &str {
        &self.compute_type
    }

    fn is_nllb(&self) -> bool {
        self.info.id == "nllb-1.3b"
    }

    /// `_target_prefix(target)`: NLLB decoder prefix (FLORES code); MADLAD none.
    fn target_prefix(&self, target: &str) -> Option<String> {
        if self.is_nllb() {
            // check_request guarantees the code exists; Python would raise KeyError otherwise.
            Some(nllb_code(target).unwrap_or(target).to_string())
        } else {
            None
        }
    }

    /// `_source_tokens(text, source, target)`.
    fn source_tokens(&self, text: &str, source: Option<&str>, target: &str) -> Result<Vec<String>> {
        if self.is_nllb() {
            let code = source
                .and_then(nllb_code)
                .ok_or_else(|| EngineError::key(source.unwrap_or("None")))?;
            let mut tokens = vec![code.to_string()];
            tokens.extend(self.tokenizer.encode(text)?);
            tokens.push(EOS_TOKEN.to_string());
            Ok(tokens)
        } else {
            let mut tokens = self.tokenizer.encode(&format!("<2{target}> {text}"))?;
            tokens.push(EOS_TOKEN.to_string());
            Ok(tokens)
        }
    }

    /// `_decode(hypothesis, target)`: decoded, stripped, target tag removed.
    fn decode(&self, hypothesis: &[String], target: &str) -> Result<String> {
        let decoded = self.tokenizer.decode(hypothesis)?;
        let text = strip(&decoded);
        let tag = self.target_prefix(target).unwrap_or_else(|| format!("<2{target}>"));
        let text = match text.strip_prefix(tag.as_str()) {
            Some(rest) => rest.trim_start_matches([' ', ':', ',']),
            None => text,
        };
        Ok(text.to_string())
    }

    /// The batch call with the CUDA → CPU fallback (only on a native `RuntimeError` naming a CUDA library).
    fn translate_with_fallback(
        &mut self,
        sources: &[Vec<String>],
        target_prefix: Option<&[Vec<String>]>,
        options: &BatchOptions,
    ) -> Result<Vec<Vec<Vec<String>>>> {
        match self.backend.translate_batch(sources, target_prefix, options) {
            Ok(results) => Ok(results),
            Err(exc) => {
                if self.device != Device::Cuda
                    || !exc.is(ErrorKind::Runtime)
                    || !se_models::cuda::is_cuda_library_error(&exc.message)
                {
                    return Err(exc);
                }
                let Some(loader) = self.loader.as_mut() else { return Err(exc) };
                eprintln!("CUDA unavailable, falling back to CPU: {exc}");
                self.device = Device::Cpu;
                self.backend = loader(Device::Cpu)?;
                self.backend.translate_batch(sources, target_prefix, options)
            }
        }
    }
}

impl Translator for Ct2Translator {
    fn translate(
        &mut self,
        sentences: &[Sentence],
        target: &str,
        source: Option<&str>,
        mut on_progress: Option<ProgressFn<'_>>,
        mut on_batch: Option<BatchFn<'_>>,
    ) -> Result<Vec<Sentence>> {
        check_request(self.info.id, target, source)?;
        // Empty lines are not sent to the model; they come back with their text, times and ids preserved.
        let mut result: Vec<Sentence> = sentences.iter().map(fresh_copy).collect();
        let pending: Vec<(usize, &Sentence)> =
            sentences.iter().enumerate().filter(|(_, s)| !strip(&s.text).is_empty()).collect();
        if pending.is_empty() {
            if let Some(cb) = on_progress.as_mut() {
                cb(1.0);
            }
            return Ok(result);
        }

        let prefix = self.target_prefix(target);
        let batch_size = self.batch_size.max(1);
        let mut processed = 0usize;
        for batch in pending.chunks(batch_size) {
            let sources = batch
                .iter()
                .map(|(_, s)| self.source_tokens(strip(&s.text), source, target))
                .collect::<Result<Vec<_>>>()?;
            let target_prefix: Option<Vec<Vec<String>>> =
                prefix.as_ref().map(|p| batch.iter().map(|_| vec![p.clone()]).collect());
            let options = BatchOptions { beam_size: BEAM_SIZE, max_decoding_length: MAX_DECODING_LENGTH, max_batch_size: batch_size };
            let results = self.translate_with_fallback(&sources, target_prefix.as_deref(), &options)?;
            if results.len() != batch.len() {
                return Err(EngineError::model(se_core::t!("translation_invalid", id = batch[0].1.id)));
            }
            for ((index, sentence), hypotheses) in batch.iter().zip(results) {
                let empty = Vec::new();
                let hypothesis = hypotheses.first().unwrap_or(&empty);
                let mut text = self.decode(hypothesis, target)?;
                if text.is_empty() {
                    let retry_source = vec![self.source_tokens(strip(&sentence.text), source, target)?];
                    let retry_prefix: Option<Vec<Vec<String>>> = prefix.as_ref().map(|p| vec![vec![p.clone()]]);
                    let retry_options = BatchOptions { max_batch_size: 1, ..options };
                    let retry = self.backend.translate_batch(&retry_source, retry_prefix.as_deref(), &retry_options)?;
                    if let Some(hypothesis) = retry.first().and_then(|r| r.first()) {
                        text = self.decode(hypothesis, target)?;
                    }
                }
                let needs_review = text.is_empty();
                result[*index] = Sentence {
                    id: sentence.id,
                    start: sentence.start,
                    end: sentence.end,
                    text: if needs_review { sentence.text.clone() } else { text },
                    words: Vec::new(),
                    speaker: sentence.speaker,
                    needs_review,
                };
            }
            processed += batch.len();
            if let Some(cb) = on_batch.as_mut() {
                let last_index = batch[batch.len() - 1].0;
                cb(&result[..=last_index]);
            }
            if let Some(cb) = on_progress.as_mut() {
                cb((processed as f64 / pending.len() as f64).min(1.0));
            }
        }
        Ok(result)
    }
}

/// `Sentence(id, start, end, text=s.text, words=[], speaker=s.speaker)` (needs_review false).
pub(crate) fn fresh_copy(s: &Sentence) -> Sentence {
    Sentence {
        id: s.id,
        start: s.start,
        end: s.end,
        text: s.text.clone(),
        words: Vec::new(),
        speaker: s.speaker,
        needs_review: false,
    }
}

// --- Tokenizers ---------------------------------------------------------------------------------------

/// `_load_tokenizer(directory, model_id)`: MADLAD → `spiece.model`, anything else → `tokenizer.json`.
pub fn load_tokenizer(directory: &Path, model_id: &str) -> Result<Box<dyn TextTokenizer>> {
    if model_id == "madlad-3b" {
        Ok(Box::new(SentencePieceTokenizer::open(&directory.join("spiece.model"))?))
    } else {
        Ok(Box::new(HfTokenizer::open(&directory.join("tokenizer.json"))?))
    }
}

/// `_SentencePieceTokenizer` (MADLAD): `encode(text, out_type=str)`, decode by ids (unknown → `unk_id`).
pub struct SentencePieceTokenizer {
    processor: sentencepiece::SentencePieceProcessor,
}

impl SentencePieceTokenizer {
    pub fn open(path: &Path) -> Result<Self> {
        // Read the bytes in Rust: the C++ loader's file name handling differs per platform. The messages are
        // the ones Python's `SentencePieceProcessor(model_file=...)` raises (RuntimeError, `str(exc)`):
        // `NOT_FOUND: "<path>": <strerror> Error #<errno>` and `INTERNAL: could not parse ModelProto from <path>`.
        let shown = se_core::paths::py_path_str(path);
        let data = std::fs::read(path).map_err(|e| {
            let (errno, text) = match e.kind() {
                std::io::ErrorKind::PermissionDenied => (13, "Permission denied"),
                std::io::ErrorKind::IsADirectory => (13, "Permission denied"),
                _ => (2, "No such file or directory"),
            };
            EngineError::runtime(format!("NOT_FOUND: \"{shown}\": {text} Error #{errno}"))
        })?;
        let processor = sentencepiece::SentencePieceProcessor::from_serialized_proto(&data)
            .map_err(|_| EngineError::runtime(format!("INTERNAL: could not parse ModelProto from {shown}")))?;
        Ok(Self { processor })
    }
}

impl TextTokenizer for SentencePieceTokenizer {
    fn encode(&self, text: &str) -> Result<Vec<String>> {
        if text.is_empty() {
            return Ok(Vec::new());
        }
        let pieces = self.processor.encode(text).map_err(|e| EngineError::runtime(e.to_string()))?;
        Ok(pieces.into_iter().map(|p| p.piece).collect())
    }

    fn decode(&self, tokens: &[String]) -> Result<String> {
        let unk = self.processor.unk_id();
        let ids: Vec<u32> = tokens
            .iter()
            .map(|t| self.processor.piece_to_id(t).ok().flatten().unwrap_or(unk))
            .collect();
        self.processor.decode_piece_ids(&ids).map_err(|e| EngineError::runtime(e.to_string()))
    }
}

/// `_HfTokenizer` (NLLB `tokenizer.json`): no special tokens on encode; unknown tokens dropped and special
/// tokens skipped on decode.
pub struct HfTokenizer {
    tokenizer: tokenizers::Tokenizer,
}

impl HfTokenizer {
    pub fn open(path: &Path) -> Result<Self> {
        let tokenizer = tokenizers::Tokenizer::from_file(path).map_err(|e| EngineError::new(ErrorKind::Other, e.to_string()))?;
        Ok(Self { tokenizer })
    }
}

impl TextTokenizer for HfTokenizer {
    fn encode(&self, text: &str) -> Result<Vec<String>> {
        let encoding = self.tokenizer.encode(text, false).map_err(|e| EngineError::new(ErrorKind::Other, e.to_string()))?;
        Ok(encoding.get_tokens().to_vec())
    }

    fn decode(&self, tokens: &[String]) -> Result<String> {
        let ids: Vec<u32> = tokens.iter().filter_map(|t| self.tokenizer.token_to_id(t)).collect();
        self.tokenizer.decode(&ids, true).map_err(|e| EngineError::new(ErrorKind::Other, e.to_string()))
    }
}

// --- Real CTranslate2 backend -----------------------------------------------------------------------------

/// `ctranslate2.Translator(path, device, compute_type)` through `ct2rs::sys` (all other options default:
/// device index 0, one replica, automatic intra threads).
pub struct RealCt2 {
    translator: ct2rs::sys::Translator,
}

fn compute_type_of(name: &str) -> Result<ct2rs::sys::ComputeType> {
    use ct2rs::sys::ComputeType as C;
    Ok(match name {
        "default" => C::DEFAULT,
        "auto" => C::AUTO,
        "float32" => C::FLOAT32,
        "int8" => C::INT8,
        "int8_float32" => C::INT8_FLOAT32,
        "int8_float16" => C::INT8_FLOAT16,
        "int8_bfloat16" => C::INT8_BFLOAT16,
        "int16" => C::INT16,
        "float16" => C::FLOAT16,
        "bfloat16" => C::BFLOAT16,
        other => return Err(EngineError::value(format!("Unsupported compute type: {other}"))),
    })
}

impl RealCt2 {
    pub fn load(path: &Path, device: Device, compute_type: &str) -> Result<Self> {
        let config = ct2rs::sys::Config {
            device: match device {
                Device::Cuda => ct2rs::sys::Device::CUDA,
                _ => ct2rs::sys::Device::CPU,
            },
            compute_type: compute_type_of(compute_type)?,
            ..Default::default()
        };
        let translator = ct2rs::sys::Translator::new(path, &config).map_err(|e| EngineError::runtime(error_text(&e)))?;
        Ok(Self { translator })
    }

    /// The model directory a [`RealCt2`] would load for `model_id` (HF snapshot holding `model.bin`).
    pub fn model_dir(model_id: &str) -> Result<PathBuf> {
        se_models::cache::model_dir(model_id)
    }
}

fn error_text(e: &anyhow::Error) -> String {
    e.to_string()
}

impl Ct2Backend for RealCt2 {
    fn translate_batch(
        &mut self,
        sources: &[Vec<String>],
        target_prefix: Option<&[Vec<String>]>,
        options: &BatchOptions,
    ) -> Result<Vec<Vec<Vec<String>>>> {
        let opts = ct2rs::sys::TranslationOptions::<String, String> {
            beam_size: options.beam_size,
            patience: 1.0,
            length_penalty: 1.0,
            coverage_penalty: 0.0,
            repetition_penalty: 1.0,
            no_repeat_ngram_size: 0,
            disable_unk: false,
            suppress_sequences: Vec::new(),
            prefix_bias_beta: 0.0,
            end_token: Vec::new(),
            return_end_token: false,
            max_input_length: 1024,
            max_decoding_length: options.max_decoding_length,
            min_decoding_length: 1,
            sampling_topk: 1,
            sampling_topp: 1.0,
            sampling_temperature: 1.0,
            use_vmap: false,
            num_hypotheses: 1,
            return_scores: false,
            return_attention: false,
            return_logits_vocab: false,
            return_alternatives: false,
            min_alternative_expansion_prob: 0.0,
            replace_unknowns: false,
            max_batch_size: options.max_batch_size,
            batch_type: ct2rs::sys::BatchType::Examples,
        };
        let results = match target_prefix {
            Some(prefix) => self.translator.translate_batch_with_target_prefix(sources, prefix, &opts, None),
            None => self.translator.translate_batch(sources, &opts, None),
        }
        .map_err(|e| EngineError::runtime(error_text(&e)))?;
        Ok(results.into_iter().map(|r| r.hypotheses).collect())
    }
}
