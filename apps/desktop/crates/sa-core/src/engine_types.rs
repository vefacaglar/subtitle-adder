//! Engine wire data model shared by every crate (mac-enginekit §1, win-enginekit §4).
//!
//! These types live in `sa-core` (not `sa-enginekit`) because the subtitle importer, the model catalog and the
//! localized error messages of this crate need them; `sa-enginekit` re-exports them.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

use crate::l10n::{UiLanguage, engine_string};

/// One recognized word.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Word {
    #[serde(serialize_with = "integral_f64")]
    pub start: f64,
    #[serde(serialize_with = "integral_f64")]
    pub end: f64,
    pub text: String,
}

impl Word {
    pub fn new(start: f64, end: f64, text: impl Into<String>) -> Self {
        Self { start, end, text: text.into() }
    }

    /// `(start + end) / 2` (used by splitting).
    pub fn midpoint(&self) -> f64 {
        (self.start + self.end) / 2.0
    }
}

impl Eq for Word {}
impl std::hash::Hash for Word {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.start.to_bits().hash(state);
        self.end.to_bits().hash(state);
        self.text.hash(state);
    }
}

/// One subtitle line.
///
/// JSON (compact, key order `id, start, end, text, words`, then `speaker` only when `Some`, then
/// `"needs_review":true` only when true; `uid` is never serialized). Decoding: `words` absent/null → `[]`;
/// `speaker` absent/null → `None` (a non-integer value fails); `needs_review` is true only for JSON `true` and never
/// fails. Equality and hash exclude `uid`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sentence {
    pub id: i64,
    /// View identity and selection key; a fresh random value on every decode.
    #[serde(skip, default = "Uuid::new_v4")]
    pub uid: Uuid,
    #[serde(serialize_with = "integral_f64")]
    pub start: f64,
    #[serde(serialize_with = "integral_f64")]
    pub end: f64,
    pub text: String,
    #[serde(default, deserialize_with = "null_as_empty")]
    pub words: Vec<Word>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<i64>,
    #[serde(default, deserialize_with = "lenient_true", skip_serializing_if = "is_false")]
    pub needs_review: bool,
}

impl Sentence {
    /// `uid = new_v4`, `words = []`, `speaker = None`, `needs_review = false`.
    pub fn new(id: i64, start: f64, end: f64, text: impl Into<String>) -> Self {
        Self { id, uid: Uuid::new_v4(), start, end, text: text.into(), words: Vec::new(), speaker: None, needs_review: false }
    }

    /// Encodes a list as the engine's stdin JSON array (compact).
    pub fn encode_list(sentences: &[Sentence]) -> Result<String, serde_json::Error> {
        serde_json::to_string(sentences)
    }

    /// Decodes a JSON array of sentences (fresh uids).
    pub fn decode_list(json: &str) -> Result<Vec<Sentence>, serde_json::Error> {
        serde_json::from_str(json)
    }
}

impl PartialEq for Sentence {
    fn eq(&self, o: &Self) -> bool {
        self.id == o.id
            && self.start == o.start
            && self.end == o.end
            && self.text == o.text
            && self.words == o.words
            && self.speaker == o.speaker
            && self.needs_review == o.needs_review
    }
}
impl Eq for Sentence {}
impl std::hash::Hash for Sentence {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.id.hash(state);
        self.start.to_bits().hash(state);
        self.end.to_bits().hash(state);
        self.text.hash(state);
        self.words.hash(state);
        self.speaker.hash(state);
        self.needs_review.hash(state);
    }
}

/// Writes a finite double like .NET's `System.Text.Json` does: integral values without a fraction (`0`, `1`, not
/// `0.0`), anything else in the shortest round-trip form (win-enginekit §4.2, test `SentenceJsonUsesUtf8Unescaped`).
/// Non-finite values are an error, never `null`.
fn integral_f64<S: Serializer>(v: &f64, s: S) -> Result<S::Ok, S::Error> {
    if !v.is_finite() {
        return Err(serde::ser::Error::custom("non-finite number in a sentence"));
    }
    if v.fract() == 0.0 && v.abs() < 1e15 { s.serialize_i64(*v as i64) } else { s.serialize_f64(*v) }
}

fn is_false(v: &bool) -> bool {
    !*v
}

fn lenient_true<'de, D: Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(v == serde_json::Value::Bool(true))
}

fn null_as_empty<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Word>, D::Error> {
    Ok(Option::<Vec<Word>>::deserialize(d)?.unwrap_or_default())
}

/// Subtitle output formats, in picker order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SubtitleFormat {
    #[default]
    Srt,
    Vtt,
    Json,
}

impl SubtitleFormat {
    pub const ALL: [SubtitleFormat; 3] = [SubtitleFormat::Srt, SubtitleFormat::Vtt, SubtitleFormat::Json];

    /// `"srt"` / `"vtt"` / `"json"`.
    pub const fn raw(self) -> &'static str {
        match self {
            SubtitleFormat::Srt => "srt",
            SubtitleFormat::Vtt => "vtt",
            SubtitleFormat::Json => "json",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.raw() == raw)
    }
}

/// One row of the `models` event.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EngineModel {
    pub id: String,
    /// Absent or JSON `null` → `"transcription"` (C# init setter, Swift `decodeIfPresent ?? default`).
    #[serde(default = "default_kind", deserialize_with = "kind_or_default")]
    pub kind: String,
    /// Absent or JSON `null` → `"whisper"`.
    #[serde(default = "default_backend", deserialize_with = "backend_or_default")]
    pub backend: String,
    /// `None` (or JSON null) = every language.
    #[serde(default)]
    pub languages: Option<Vec<String>>,
    pub size_mb: i64,
    #[serde(default)]
    pub disk_mb: Option<i64>,
    pub speed: i64,
    pub accuracy: i64,
    #[serde(default)]
    pub license: Option<String>,
    pub downloaded: bool,
}

fn default_kind() -> String {
    "transcription".into()
}
fn default_backend() -> String {
    "whisper".into()
}
fn kind_or_default<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(Option::<String>::deserialize(d)?.unwrap_or_else(default_kind))
}
fn backend_or_default<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(Option::<String>::deserialize(d)?.unwrap_or_else(default_backend))
}

impl EngineModel {
    /// Test/fallback constructor with the decode defaults (kind transcription, backend whisper, no languages, no
    /// disk size, no license).
    pub fn new(id: impl Into<String>, size_mb: i64, speed: i64, accuracy: i64, downloaded: bool) -> Self {
        Self {
            id: id.into(),
            kind: default_kind(),
            backend: default_backend(),
            languages: None,
            size_mb,
            disk_mb: None,
            speed,
            accuracy,
            license: None,
            downloaded,
        }
    }

    pub fn is_transcription(&self) -> bool {
        self.kind == "transcription"
    }
    pub fn is_translation(&self) -> bool {
        self.kind == "translation"
    }
    pub fn is_diarization(&self) -> bool {
        self.kind == "diarization"
    }
    pub fn is_llama(&self) -> bool {
        self.backend == "llama"
    }
    /// `license.to_uppercase().contains("NC")`.
    pub fn has_non_commercial_license(&self) -> bool {
        self.license.as_deref().is_some_and(|l| l.to_uppercase().contains("NC"))
    }
    /// Whisper models detect the spoken language.
    pub fn detects_language(&self) -> bool {
        self.backend == "whisper"
    }
    /// Exact, case-sensitive language support check (`None` = all).
    pub fn supports(&self, code: &str) -> bool {
        self.languages.as_ref().is_none_or(|l| l.iter().any(|c| c == code))
    }
}

/// Engine stages of the `stage` event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EngineStage {
    ExtractingAudio,
    DownloadingModel,
    LoadingModel,
    Transcribing,
    Diarizing,
    Segmenting,
    Translating,
    /// A stage name this client does not know (not an error).
    Unknown,
}

impl EngineStage {
    pub fn parse(raw: &str) -> Self {
        match raw {
            "extracting_audio" => Self::ExtractingAudio,
            "downloading_model" => Self::DownloadingModel,
            "loading_model" => Self::LoadingModel,
            "transcribing" => Self::Transcribing,
            "diarizing" => Self::Diarizing,
            "segmenting" => Self::Segmenting,
            "translating" => Self::Translating,
            _ => Self::Unknown,
        }
    }

    /// The C# enum name used inside `stage:` message ids (`ExtractingAudio`, …, `Unknown`).
    pub const fn id_name(self) -> &'static str {
        match self {
            Self::ExtractingAudio => "ExtractingAudio",
            Self::DownloadingModel => "DownloadingModel",
            Self::LoadingModel => "LoadingModel",
            Self::Transcribing => "Transcribing",
            Self::Diarizing => "Diarizing",
            Self::Segmenting => "Segmenting",
            Self::Translating => "Translating",
            Self::Unknown => "Unknown",
        }
    }
}

/// Engine client errors. `Display` is the user-facing (localized) text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineError {
    /// No engine binary was found; localized at each display.
    NotFound,
    /// Any other failure; the message is shown verbatim.
    Failed(String),
    /// The caller cancelled the operation (never shown to the user).
    Cancelled,
}

impl EngineError {
    /// Stable kind name used in `LocalizedMessage` ids.
    pub const fn kind_name(&self) -> &'static str {
        match self {
            EngineError::NotFound => "notFound",
            EngineError::Failed(_) => "failed",
            EngineError::Cancelled => "cancelled",
        }
    }

    /// The localized not-found text in an explicit language.
    pub fn not_found_text(language: Option<UiLanguage>) -> String {
        engine_string("Altyazı motoru bulunamadı (engine/dist derlendi mi, ya da SUBTITLE_ENGINE ayarlı mı?).", language, &[])
    }
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EngineError::NotFound => f.write_str(&Self::not_found_text(None)),
            EngineError::Failed(m) => f.write_str(m),
            EngineError::Cancelled => f.write_str("cancelled"),
        }
    }
}

impl std::error::Error for EngineError {}
