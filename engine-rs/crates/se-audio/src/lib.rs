//! Media side of the engine (spec: `spec/audio.md`):
//!
//! * [`audio`]: decode any video/audio container with FFmpeg into a 16 kHz mono 16-bit WAV (`audio.py`).
//! * [`reuse`]: WAV reuse across runs: identity marker, atomic temp + rename extraction
//!   (`pipeline.extract_video_audio` and helpers).
//! * [`preview`]: a local MP4 playback copy (stream copy or H.264/AAC transcode, `preview.py`).
//! * [`wav`]: writing (Python `wave` byte parity) and reading the engine's own WAV files.
//! * [`testgen`]: media fixtures and probes for tests (the PyAV code of the Python test suite).

pub mod audio;
pub mod ff;
pub mod preview;
pub mod reuse;
#[doc(hidden)]
pub mod testgen;
pub mod wav;

pub use audio::{SAMPLE_RATE, extract_audio};
pub use preview::{PreviewSource, make_preview};
pub use reuse::{AudioBackend, FfmpegBackend, SourceIdentity, extract_video_audio, extract_video_audio_with};
