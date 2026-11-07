//! Text side of the engine: sentence segmentation (`sentences.py`) and subtitle formats (`subtitles.py`).
//!
//! Spec: `spec/sentences.md`. All lengths/indices are Unicode code points; rounding is ties-to-even.

pub mod sentences;
pub mod subtitles;
pub mod unicode;
mod unicode_tables;

pub use sentences::{SegmenterConfig, segment};
pub use subtitles::{render, write};
