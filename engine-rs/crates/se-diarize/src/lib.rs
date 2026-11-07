//! Speaker diarization (port of `diarize.py`; spec: `spec/diarize.md`).
//!
//! sherpa-onnx `OfflineSpeakerDiarization` (pyannote segmentation-3.0 + CAM++ embeddings) through the
//! sherpa-onnx C API shared library loaded with `libloading` ([`ffi`]), plus all post-processing:
//! speech compaction, centroid merge, noise filter, acoustic review of short turns, word assignment.

pub mod assign;
pub mod ffi;
pub mod postprocess;
pub mod sherpa;

pub use assign::{SENTENCE_FINAL, SegmentIndex, assign_speakers};
pub use postprocess::{
    CentroidFn, EmbedFn, SliceCache, SpeechSpan, cluster_centroids, compact_speech_samples, merge_similar_speakers,
    normalize, unmap_compacted_segments, verify_short_turns, verify_short_turns_with,
};
pub use sherpa::{
    DiarizationEngine, DiarizeHooks, Embedder, EngineBuilder, RawSegment, SherpaDiarizer, WavReader, create_diarizer,
    model_files, read_wav, run_diarization,
};

use std::path::Path;

use se_core::{ProgressFn, Result, Word};

/// Catalog id of the diarization model.
pub const MODEL_ID: &str = "speaker-diarization";
pub const SAMPLE_RATE: u32 = 16_000;
/// Clustering threshold of the automatic speaker count.
pub const CLUSTER_THRESHOLD: f64 = 0.5;
/// Centroid cosine similarity at which auto mode merges clusters.
pub const MERGE_SIMILARITY: f64 = 0.65;
pub const CENTROID_MAX_SEGMENTS: usize = 10;
pub const CENTROID_MAX_SECONDS: f64 = 30.0;
/// A longer segment is cropped to its middle part.
pub const CENTROID_SEGMENT_SECONDS: f64 = 6.0;
/// Speakers with less speech are dropped as noise (unless a short complete reply supports them).
pub const MIN_SPEAKER_SECONDS: f64 = 1.0;
pub const VERIFY_MAX_TURNS: usize = 32;
pub const VERIFY_MIN_SIMILARITY: f64 = 0.65;
pub const SPEECH_PAD_SECONDS: f64 = 0.25;
pub const SPEECH_MERGE_GAP: f64 = 1.5;
pub const SPEECH_SEPARATOR_SECONDS: f64 = 0.25;
pub const COMPACT_MIN_DURATION: f64 = 10.0;
pub const COMPACT_MAX_RATIO: f64 = 0.85;
pub const SNAP_MAX_WORDS: usize = 2;
pub const SNAP_MAX_SECONDS: f64 = 0.8;

/// A diarized speaker turn (frozen dataclass; `==` compares all three fields). Speaker ids are 0-based.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpeakerSegment {
    pub start: f64,
    pub end: f64,
    pub speaker: i64,
}

/// A speaker diarizer (Python `Diarizer` protocol). The pipeline always passes the ASR words; fakes may
/// ignore them.
pub trait Diarizer {
    /// `diarize(audio, num_speakers, on_progress, words)`: segments sorted by start, speakers renumbered by
    /// first appearance. `num_speakers = None` → automatic count. Progress fractions are monotonic and reach
    /// 1.0 only after the acoustic review.
    fn diarize(
        &mut self,
        audio: &Path,
        num_speakers: Option<u32>,
        on_progress: Option<ProgressFn<'_>>,
        words: Option<&[Word]>,
    ) -> Result<Vec<SpeakerSegment>>;
}
