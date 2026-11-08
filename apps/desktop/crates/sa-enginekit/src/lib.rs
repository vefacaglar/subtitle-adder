//! `sa-enginekit`: the client side of the `subtitle-engine` JSON-lines protocol (CLAUDE.md "Engine protocol"),
//! shared by both front ends (UNIFIED.md §1.1 `enginekit`).
//!
//! Wire data types live in `sa_core::engine_types` and are re-exported here.

pub mod client;
pub mod command;
pub mod download_phase;
pub mod editing;
pub mod events;
pub mod external_llm;
pub mod launch;
pub mod line_splitter;
pub mod media_types;
pub mod presets;
pub mod process;
pub mod working_dir;

pub use client::{EngineClient, EventStream, ProcessEngineClient, TranscribeOptions};
pub use command::EngineCommand;
pub use download_phase::{DownloadKind, DownloadPhaseTracker};
pub use editing::{DialogueDash, SentenceListExt};
pub use events::{DecodeError, EngineEvent, EngineResult, decode};
pub use launch::{EngineLaunchSettings, EngineLaunchState, ExternalLlm};
pub use line_splitter::LineSplitter;
pub use presets::TranslationProviderPreset;
pub use process::{ChildHandle, ProcessControl};
pub use sa_core::engine_types::{EngineError, EngineModel, EngineStage, Sentence, SubtitleFormat, Word};
