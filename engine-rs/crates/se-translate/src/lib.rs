//! Subtitle translation (port of `translate.py` and `translation_profiles.py`; specs:
//! `spec/translate-local.md`, `spec/translate-llm.md`).
//!
//! * [`ct2`]: CTranslate2 translators (MADLAD-400 3B, NLLB-200 1.3B).
//! * [`llm`]: the prompt-driven translator for the `llama` models (local `llama-server`) and for the opt-in
//!   external OpenAI-compatible server (`SUBTITLE_ENGINE_EXTERNAL_LLM_URL`).
//! * [`external`]: external server configuration and safety rules (local-network rule, https for keys,
//!   strict offline, `.local` resolution).
//! * [`prompts`], [`validate`], [`profiles`], [`checkpoint`], [`diag`]: prompt texts, output validators,
//!   per-model profiles, `chunk-<hash>.json` checkpoints and `[translation]` stderr diagnostics.

pub mod checkpoint;
pub mod ct2;
pub mod diag;
pub mod external;
pub mod http;
pub mod lan;
pub mod llm;
pub mod profiles;
pub mod prompts;
pub mod pyunicode;
pub mod pyurl;
pub mod server;
pub mod validate;

pub use ct2::{BatchOptions, Ct2Backend, Ct2Translator, TextTokenizer};
pub use external::{
    check_external_llm, external_llm_api_key, external_llm_model, external_llm_url, normalize_external_chat_url,
};
pub use llm::{ChatMessage, LlamaTranslator, LlmOptions};

use se_core::{BatchFn, EngineError, ProgressFn, Result, Sentence};
use se_models::{Backend, ModelInfo, ModelKind};

/// Lines per CTranslate2 batch.
pub const BATCH_SIZE: usize = 16;
/// Lines per LLM request (tests use 1 or 2 through [`LlmOptions`]).
pub const LLM_BATCH_SIZE: usize = 6;
/// Context lines before/after a batch.
pub const LLM_CONTEXT_LINES: usize = 2;
pub const BEAM_SIZE: usize = 4;
pub const MAX_DECODING_LENGTH: usize = 256;
pub const EOS_TOKEN: &str = "</s>";
/// MADLAD target prefix `<2xx>`.
pub const MADLAD_PREFIX: &str = "<2{}>";

/// `NLLB_LANGUAGE_CODES` (FLORES-200 codes of the supported languages).
pub const NLLB_LANGUAGE_CODES: &[(&str, &str)] = &[
    ("tr", "tur_Latn"),
    ("en", "eng_Latn"),
    ("pt", "por_Latn"),
    ("es", "spa_Latn"),
    ("ru", "rus_Cyrl"),
    ("de", "deu_Latn"),
    ("fr", "fra_Latn"),
    ("ja", "jpn_Jpan"),
    ("zh", "zho_Hans"),
];

/// A subtitle translator (Python `Translator` protocol).
pub trait Translator {
    /// Translate every sentence: `id`/`start`/`end`/`speaker` kept, `text` translated, `words` emptied.
    /// `on_batch(prefix)` after every batch (before `on_progress`), `on_progress(f)` with `0 < f <= 1`
    /// (exactly 1.0 when there is nothing to translate).
    fn translate(
        &mut self,
        sentences: &[Sentence],
        target: &str,
        source: Option<&str>,
        on_progress: Option<ProgressFn<'_>>,
        on_batch: Option<BatchFn<'_>>,
    ) -> Result<Vec<Sentence>>;

    /// Release resources (stop `llama-server`); called by the CLI in a `finally`. Must be idempotent.
    fn close(&mut self) {}
}

/// `check_request(model_id, target, source)`: catalog/language validation with localized `ModelError`s
/// (`unknown_translation_model`, `support_error`, `nllb_source_required`, `cannot_translate_from`,
/// `nllb_language_undefined`). The NLLB source checks are skipped when an external LLM URL is set.
pub fn check_request(model_id: &str, target: &str, source: Option<&str>) -> Result<&'static ModelInfo> {
    let info = translation_info(model_id)?;
    let supported: &[&str] = info.languages.unwrap_or(&[]);
    if !supported.contains(&target) {
        return Err(EngineError::model(se_models::support_error(info, target)));
    }
    if info.id == "nllb-1.3b" && env_external_llm_url().is_none() {
        let source = match source {
            Some(s) if !s.is_empty() => s,
            _ => return Err(EngineError::model(se_core::t!("nllb_source_required"))),
        };
        if !supported.contains(&source) {
            let name = se_core::i18n::display_name(info.id);
            return Err(EngineError::model(se_core::t!(
                "cannot_translate_from",
                name = name,
                source = source,
                supported = supported.join(", ")
            )));
        }
        if nllb_code(source).is_none() || nllb_code(target).is_none() {
            return Err(EngineError::model(se_core::t!("nllb_language_undefined", target = target)));
        }
    }
    // MADLAD uses no source language tag; --from is ignored even if given.
    Ok(info)
}

/// Catalog lookup restricted to translation models (`unknown_translation_model` otherwise).
fn translation_info(model_id: &str) -> Result<&'static ModelInfo> {
    match se_models::find(model_id) {
        Some(info) if info.kind == ModelKind::Translation => Ok(info),
        _ => Err(EngineError::model(se_core::t!("unknown_translation_model", model = model_id))),
    }
}

/// `NLLB_LANGUAGE_CODES[code]`.
pub fn nllb_code(code: &str) -> Option<&'static str> {
    NLLB_LANGUAGE_CODES.iter().find(|(c, _)| *c == code).map(|(_, flores)| *flores)
}

/// `external_llm_url()` as `check_request` / `create_translator` read it: the stripped
/// `SUBTITLE_ENGINE_EXTERNAL_LLM_URL`, `None` when unset or blank (the same rule as
/// [`external::external_llm_url`], read here directly so the local path has no dependency on the LLM module).
fn env_external_llm_url() -> Option<String> {
    let raw = std::env::var_os(external::EXTERNAL_LLM_URL_ENV_VAR)?;
    let raw = raw.to_string_lossy();
    let value = se_core::pyfmt::strip(&raw);
    if value.is_empty() { None } else { Some(value.to_string()) }
}

/// Inference device of the local translators (shared with `se-asr`).
pub use se_models::cuda::Device;

/// `create_translator(model_id, device, compute_type)`: [`LlamaTranslator`] for `llama` models or whenever
/// an external LLM URL is set, else [`Ct2Translator`]. Unknown / non-translation ids →
/// `unknown_translation_model`.
pub fn create_translator(model_id: &str, device: Device, compute_type: &str) -> Result<Box<dyn Translator>> {
    let info = translation_info(model_id)?;
    if info.backend == Backend::Llama || env_external_llm_url().is_some() {
        let options = LlmOptions { device, ..LlmOptions::default() };
        return Ok(Box::new(LlamaTranslator::new(model_id, options)?));
    }
    Ok(Box::new(Ct2Translator::new(model_id, device, compute_type)?))
}
