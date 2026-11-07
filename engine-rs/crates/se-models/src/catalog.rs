//! The model catalog (`models.KATALOG`, verbatim, UI order) and lookups.

/// `Backend` literal of `models.py`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Whisper,
    Onnx,
    Ctranslate2,
    Sherpa,
    Llama,
}

impl Backend {
    /// The protocol string (`"whisper"`, `"onnx"`, `"ctranslate2"`, `"sherpa"`, `"llama"`).
    pub fn as_str(self) -> &'static str {
        match self {
            Backend::Whisper => "whisper",
            Backend::Onnx => "onnx",
            Backend::Ctranslate2 => "ctranslate2",
            Backend::Sherpa => "sherpa",
            Backend::Llama => "llama",
        }
    }
}

/// `ModelKind` literal of `models.py`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelKind {
    Transcription,
    Translation,
    Diarization,
}

impl ModelKind {
    /// The protocol string (`"transcription"`, `"translation"`, `"diarization"`).
    pub fn as_str(self) -> &'static str {
        match self {
            ModelKind::Transcription => "transcription",
            ModelKind::Translation => "translation",
            ModelKind::Diarization => "diarization",
        }
    }
}

/// A catalog entry (`ModelInfo` frozen dataclass; same fields and defaults).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelInfo {
    pub id: &'static str,
    /// Main HF repo id.
    pub repo: &'static str,
    /// Approximate download size shown in the UI (fixed estimate).
    pub size_mb: i64,
    /// 1 (slowest) - 5 (fastest), relative.
    pub speed: i64,
    /// 1 (lowest) - 5 (highest), relative.
    pub accuracy: i64,
    pub backend: Backend,
    /// `None`: all languages (Whisper, diarization).
    pub languages: Option<&'static [&'static str]>,
    /// onnx-asr model name (only `Backend::Onnx`).
    pub onnx_model: Option<&'static str>,
    /// onnx-asr quantization value.
    pub quantization: Option<&'static str>,
    /// File patterns (fnmatch) / names to download within `repo`.
    pub files: &'static [&'static str],
    pub kind: ModelKind,
    /// SPDX short name / expression from the model card.
    pub license: &'static str,
    /// Extra repos: `(repo, files)`.
    pub extra: &'static [(&'static str, &'static [&'static str])],
    /// Repos hold other models too: measure/delete only the listed files.
    pub owns_files_only: bool,
    /// Real download sizes aligned with [`ModelInfo::parts`], used as fallback estimates.
    pub part_bytes: &'static [u64],
}

impl ModelInfo {
    /// `_parts(info)`: `[(repo, files), *extra]`.
    pub fn parts(&self) -> Vec<(&'static str, &'static [&'static str])> {
        let mut parts = vec![(self.repo, self.files)];
        parts.extend_from_slice(self.extra);
        parts
    }
}

/// faster-whisper's download patterns (`ALLOW_PATTERNS`).
pub const ALLOW_PATTERNS: &[&str] = &["config.json", "preprocessor_config.json", "model.bin", "tokenizer.json", "vocabulary.*"];
/// CTranslate2 translation model files.
pub const TRANSLATION_FILES: &[&str] = &["config.json", "model.bin", "shared_vocabulary.json", "tokenizer.json"];
/// MADLAD additionally needs its SentencePiece model.
pub const MADLAD_FILES: &[&str] = &["config.json", "model.bin", "shared_vocabulary.json", "tokenizer.json", "spiece.model"];
/// Languages of every translation model.
pub const TRANSLATION_LANGUAGES: &[&str] = &["tr", "en", "pt", "es", "ru", "de", "fr", "ja", "zh"];
/// Shared silero VAD of the onnx models.
pub const VAD_REPO: &str = "istupakov/silero-vad-onnx";
pub const VAD_FILE: &str = "silero_vad.onnx";
pub const DIARIZATION_SEGMENTATION_REPO: &str = "csukuangfj/sherpa-onnx-pyannote-segmentation-3-0";
pub const DIARIZATION_SEGMENTATION_FILE: &str = "model.int8.onnx";
pub const DIARIZATION_EMBEDDING_REPO: &str = "csukuangfj/speaker-embedding-models";
pub const DIARIZATION_EMBEDDING_FILE: &str = "3dspeaker_speech_campplus_sv_zh_en_16k-common_advanced.onnx";
/// European languages supported by Parakeet TDT v3 and Canary 1B v2 (this order).
pub const EUROPEAN_LANGUAGES: &[&str] = &[
    "bg", "hr", "cs", "da", "nl", "en", "et", "fi", "fr", "de", "el", "hu", "it", "lv", "lt", "mt", "pl", "pt", "ro",
    "sk", "sl", "es", "sv", "ru", "uk",
];
/// Progress callbacks at most ~10 Hz.
pub const PROGRESS_INTERVAL_SECS: f64 = 0.1;

const fn whisper(id: &'static str, repo: &'static str, size_mb: i64, speed: i64, accuracy: i64) -> ModelInfo {
    ModelInfo {
        id,
        repo,
        size_mb,
        speed,
        accuracy,
        backend: Backend::Whisper,
        languages: None,
        onnx_model: None,
        quantization: None,
        files: ALLOW_PATTERNS,
        kind: ModelKind::Transcription,
        license: "MIT",
        extra: &[],
        owns_files_only: false,
        part_bytes: &[],
    }
}

const fn translation(id: &'static str, repo: &'static str, size_mb: i64, speed: i64, accuracy: i64) -> ModelInfo {
    ModelInfo {
        backend: Backend::Ctranslate2,
        languages: Some(TRANSLATION_LANGUAGES),
        files: TRANSLATION_FILES,
        kind: ModelKind::Translation,
        license: "Apache-2.0",
        ..whisper(id, repo, size_mb, speed, accuracy)
    }
}

const fn gguf(
    id: &'static str,
    repo: &'static str,
    size_mb: i64,
    speed: i64,
    accuracy: i64,
    files: &'static [&'static str],
    part_bytes: &'static [u64],
) -> ModelInfo {
    ModelInfo { backend: Backend::Llama, files, owns_files_only: true, part_bytes, ..translation(id, repo, size_mb, speed, accuracy) }
}

/// `KATALOG` (order is the UI / `models` output order).
pub static KATALOG: &[ModelInfo] = &[
    whisper("base", "Systran/faster-whisper-base", 145, 5, 2),
    ModelInfo {
        backend: Backend::Onnx,
        languages: Some(&["ru"]),
        onnx_model: Some("gigaam-v3-e2e-rnnt"),
        quantization: Some("int8"),
        files: &[
            "config.json",
            "v3_e2e_rnnt_vocab.txt",
            "v3_e2e_rnnt_encoder.int8.onnx",
            "v3_e2e_rnnt_decoder.int8.onnx",
            "v3_e2e_rnnt_joint.int8.onnx",
        ],
        ..whisper("gigaam-v3", "istupakov/gigaam-v3-onnx", 216, 5, 4)
    },
    whisper("small", "Systran/faster-whisper-small", 465, 4, 3),
    ModelInfo {
        backend: Backend::Onnx,
        languages: Some(EUROPEAN_LANGUAGES),
        onnx_model: Some("nemo-parakeet-tdt-0.6b-v3"),
        quantization: Some("int8"),
        license: "CC-BY-4.0",
        files: &["config.json", "vocab.txt", "encoder-model.int8.onnx", "decoder_joint-model.int8.onnx"],
        ..whisper("parakeet-v3", "istupakov/parakeet-tdt-0.6b-v3-onnx", 640, 5, 4)
    },
    ModelInfo {
        backend: Backend::Onnx,
        languages: Some(EUROPEAN_LANGUAGES),
        onnx_model: Some("nemo-canary-1b-v2"),
        quantization: Some("int8"),
        license: "CC-BY-4.0",
        files: &["config.json", "vocab.txt", "encoder-model.int8.onnx", "decoder-model.int8.onnx"],
        ..whisper("canary-1b-v2", "istupakov/canary-1b-v2-onnx", 983, 2, 4)
    },
    whisper("medium", "Systran/faster-whisper-medium", 1500, 3, 4),
    whisper("large-v3-turbo", "mobiuslabsgmbh/faster-whisper-large-v3-turbo", 1600, 2, 4),
    whisper("large-v3", "Systran/faster-whisper-large-v3", 3000, 1, 5),
    ModelInfo { license: "CC-BY-NC-4.0", ..translation("nllb-1.3b", "OpenNMT/nllb-200-distilled-1.3B-ct2-int8", 1336, 3, 4) },
    gguf("qwen3.5-4b", "unsloth/Qwen3.5-4B-GGUF", 2614, 3, 4, &["Qwen3.5-4B-Q4_K_M.gguf"], &[2_740_937_888]),
    ModelInfo { files: MADLAD_FILES, ..translation("madlad-3b", "Nextcloud-AI/madlad400-3b-mt-ct2-int8", 2839, 2, 5) },
    gguf("gemma-4-e4b", "unsloth/gemma-4-E4B-it-GGUF", 4747, 2, 5, &["gemma-4-E4B-it-Q4_K_M.gguf"], &[4_977_171_584]),
    gguf("qwen3.5-9b", "unsloth/Qwen3.5-9B-GGUF", 5417, 2, 5, &["Qwen3.5-9B-Q4_K_M.gguf"], &[5_680_522_464]),
    gguf("gemma-4-12b", "unsloth/gemma-4-12b-it-GGUF", 6792, 1, 5, &["gemma-4-12b-it-Q4_K_M.gguf"], &[7_121_861_440]),
    ModelInfo {
        backend: Backend::Sherpa,
        files: &[DIARIZATION_SEGMENTATION_FILE],
        kind: ModelKind::Diarization,
        license: "MIT AND Apache-2.0",
        extra: &[(DIARIZATION_EMBEDDING_REPO, &[DIARIZATION_EMBEDDING_FILE])],
        owns_files_only: true,
        part_bytes: &[1_540_506, 28_281_164],
        ..whisper("speaker-diarization", DIARIZATION_SEGMENTATION_REPO, 29, 4, 3)
    },
];

/// `models.find(id)`: the catalog entry, or `None` for names outside the catalog (`tiny`, `turbo`).
pub fn find(model_id: &str) -> Option<&'static ModelInfo> {
    KATALOG.iter().find(|m| m.id == model_id)
}

/// `support_error(info, language)`: `only_language` when the model has exactly one language, else
/// `unsupported_language` with the comma-joined list.
pub fn support_error(info: &ModelInfo, language: &str) -> String {
    let name = se_core::i18n::display_name(info.id);
    let languages = info.languages.unwrap_or(&[]);
    if languages.len() == 1 {
        se_core::t!("only_language", name = name, language = se_core::i18n::language_name(languages[0]))
    } else {
        se_core::t!("unsupported_language", name = name, language = language, supported = languages.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_catalog_order_and_fields() {
        let ids: Vec<&str> = KATALOG.iter().map(|m| m.id).collect();
        assert_eq!(
            ids,
            [
                "base", "gigaam-v3", "small", "parakeet-v3", "canary-1b-v2", "medium", "large-v3-turbo", "large-v3",
                "nllb-1.3b", "qwen3.5-4b", "madlad-3b", "gemma-4-e4b", "qwen3.5-9b", "gemma-4-12b", "speaker-diarization"
            ]
        );
        assert_eq!(find("gigaam-v3").unwrap().languages, Some(&["ru"][..]));
        assert_eq!(find("nllb-1.3b").unwrap().license, "CC-BY-NC-4.0");
        assert_eq!(find("madlad-3b").unwrap().files.len(), 5);
        assert_eq!(find("qwen3.5-4b").unwrap().backend, Backend::Llama);
        assert_eq!(find("speaker-diarization").unwrap().parts().len(), 2);
        assert!(find("tiny").is_none());
    }

    #[test]
    #[serial_test::serial]
    fn test_default_is_turkish_support_error() {
        // SAFETY: serial test.
        unsafe { std::env::remove_var(se_core::i18n::ENV_VAR) };
        se_core::i18n::set_language(None);
        assert_eq!(support_error(find("gigaam-v3").unwrap(), "tr"), "GigaAM yalnız Rusça destekler.");
        assert_eq!(
            support_error(find("madlad-3b").unwrap(), "xx"),
            "MADLAD xx dilini desteklemiyor. Desteklenen diller: tr, en, pt, es, ru, de, fr, ja, zh."
        );
    }
}
