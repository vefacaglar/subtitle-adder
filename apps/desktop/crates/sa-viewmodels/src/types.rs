//! Core view-model value types (win-viewmodels §2, mac-viewmodels §3.1, §4.1, §5.1).

use std::collections::HashSet;
use std::path::PathBuf;

use sa_core::engine_types::{EngineStage, SubtitleFormat};
use sa_core::l10n::LocalizedMessage;
use uuid::Uuid;

/// Job status. Equality: variant + message id + fraction.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum JobStatus {
    #[default]
    Idle,
    Working { label: LocalizedMessage, fraction: Option<f64> },
    Finished,
    Failed { message: LocalizedMessage },
}

impl JobStatus {
    pub fn kind(&self) -> JobStatusKind {
        match self {
            JobStatus::Idle => JobStatusKind::Idle,
            JobStatus::Working { .. } => JobStatusKind::Working,
            JobStatus::Finished => JobStatusKind::Finished,
            JobStatus::Failed { .. } => JobStatusKind::Failed,
        }
    }
    pub fn is_working(&self) -> bool {
        matches!(self, JobStatus::Working { .. })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JobStatusKind {
    Idle,
    Working,
    Finished,
    Failed,
}

/// Names used verbatim inside stage label ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JobPurpose {
    Transcription,
    Translation,
    Diarization,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ExportLanguage {
    #[default]
    Original,
    Translated,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextDraft {
    pub uid: Uuid,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimingEdge {
    Start,
    End,
}

/// Workspace tabs (Player / Platform / Çeviri listesi).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Workspace {
    #[default]
    Editor,
    Platform,
    Playlist,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlaylistItemState {
    Pending,
    Working,
    Completed,
    Failed,
}

impl PlaylistItem {
    pub fn file_name(&self) -> String {
        self.video_path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }
    pub fn folder_path(&self) -> String {
        self.video_path.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()
    }
    pub fn status_text(&self) -> String {
        self.status_message.text()
    }
    pub fn is_editable(&self) -> bool {
        self.state != PlaylistItemState::Working
    }
    pub fn is_audio_ready(&self) -> bool {
        self.state == PlaylistItemState::Pending && self.fraction.is_some()
    }
}

/// A playlist row (in memory only; the playlist is replaced as a whole on every change).
#[derive(Debug, Clone, PartialEq)]
pub struct PlaylistItem {
    pub id: Uuid,
    pub video_path: PathBuf,
    /// `""` = automatic.
    pub source_language: String,
    pub state: PlaylistItemState,
    pub status_message: LocalizedMessage,
    pub fraction: Option<f64>,
}

/// What a job does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobKind {
    Generate { then_translate: bool },
    Translate { reuse_existing: bool, only: Option<HashSet<Uuid>> },
}

/// Toolbar snapshot taken at request time; a job never reads the toolbar again (UNIFIED.md §3.7 FIX-W).
#[derive(Debug, Clone, PartialEq)]
pub struct JobSettings {
    pub language: String,
    pub model: String,
    pub diarize: bool,
    pub speaker_count: i64,
    pub translation_target: String,
    pub translation_model: String,
    pub auto_save: bool,
    pub export_format: SubtitleFormat,
}

impl JobPurpose {
    /// The name used inside stage label ids (`Transcription`, `Translation`, `Diarization`).
    pub const fn id_name(self) -> &'static str {
        match self {
            JobPurpose::Transcription => "Transcription",
            JobPurpose::Translation => "Translation",
            JobPurpose::Diarization => "Diarization",
        }
    }
}

/// `round_half_away_from_zero(clamp(v, 0, 1) * 100)`.
pub fn percent(v: f64) -> i64 {
    let v = if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
    (v * 100.0).round() as i64
}

/// Shortest round-trip formatting of a fraction inside message ids (`0.5`, `1`, `0.425`), `-` when none.
pub fn fraction_id(fraction: Option<f64>) -> String {
    match fraction {
        Some(f) => format!("{f}"),
        None => "-".to_owned(),
    }
}

/// The localized text of a stage label (win-viewmodels §5 = mac-viewmodels §3.6).
pub fn stage_label_text(stage: EngineStage, fraction: Option<f64>, purpose: JobPurpose) -> String {
    use sa_core::l;
    match stage {
        EngineStage::ExtractingAudio => match fraction {
            Some(f) => l!("Ses ayrılıyor… %lld%%", percent(f)),
            None => l!("Ses ayrılıyor…"),
        },
        EngineStage::DownloadingModel => {
            let prefix = match purpose {
                JobPurpose::Translation => l!("Çeviri modeli indiriliyor…"),
                JobPurpose::Diarization => l!("Konuşmacı ayırma modeli indiriliyor…"),
                JobPurpose::Transcription => l!("Model indiriliyor…"),
            };
            match fraction {
                Some(f) => format!("{prefix} {}", l!("%lld%%", percent(f))),
                None => prefix,
            }
        }
        EngineStage::LoadingModel => match purpose {
            JobPurpose::Translation => l!("Çeviri modeli yükleniyor…"),
            _ => l!("Model yükleniyor…"),
        },
        EngineStage::Transcribing => match fraction {
            Some(f) => l!("Konuşma metne çevriliyor… %lld%%", percent(f)),
            None => l!("Konuşma metne çevriliyor…"),
        },
        EngineStage::Diarizing => match fraction {
            Some(f) => l!("Konuşmacılar ayrılıyor… %lld%%", percent(f)),
            None => l!("Konuşmacılar ayrılıyor…"),
        },
        EngineStage::Segmenting => l!("Cümleler çıkarılıyor…"),
        EngineStage::Translating => match fraction {
            Some(f) => l!("Çevriliyor… %lld%%", percent(f)),
            None => l!("Çevriliyor…"),
        },
        EngineStage::Unknown => l!("Çalışıyor…"),
    }
}

/// Stage label id `"stage:{Stage}:{fraction}:{Purpose}"` and its localized text (win-viewmodels §5).
pub fn stage_label(stage: EngineStage, fraction: Option<f64>, purpose: JobPurpose) -> LocalizedMessage {
    let id = format!("stage:{}:{}:{}", stage.id_name(), fraction_id(fraction), purpose.id_name());
    LocalizedMessage::new(id, move || stage_label_text(stage, fraction, purpose))
}

/// `true` for translation labels: `key:Çeviri hazırlanıyor…` or an id ending in `:Translation`.
pub fn is_translation_label(message: &LocalizedMessage) -> bool {
    message.id() == "key:Çeviri hazırlanıyor…" || message.id().ends_with(":Translation")
}

/// Where a list row comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ListRowOrigin {
    Playlist(Uuid),
    Job,
}

/// A row of the translation-list tab (macOS `ListRow`; Windows uses playlist items only).
#[derive(Debug, Clone, PartialEq)]
pub struct ListRow {
    /// Playlist item id, or a stable id derived from the session for single jobs.
    pub id: Uuid,
    pub origin: ListRowOrigin,
    pub video_path: PathBuf,
    pub title: String,
    pub status_message: LocalizedMessage,
    pub status_text: String,
    pub state: PlaylistItemState,
    pub fraction: Option<f64>,
    pub source_language: String,
    pub is_audio_ready: bool,
    pub can_stop: bool,
}

/// Change notifications of [`crate::subtitle::SubtitleViewModel`] (diffed into Slint properties by the views).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SubtitleChange {
    Status,
    Video,
    Sentences,
    Translations,
    Selection,
    Drafts,
    Settings,
    Models,
    Playlist,
    Undo,
    Workspace,
    Layout,
    Providers,
    Notice,
    /// Everything (language switch).
    All,
}
