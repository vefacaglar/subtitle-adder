//! Helpers shared by the ported diarization tests.
#![allow(dead_code)]

use se_core::Word;
use se_diarize::SpeakerSegment;

pub const RATE: usize = 16_000;

/// `conftest.make_words(text, step=0.4)`: word i at `[i*step, i*step + step*0.8]`.
pub fn make_words(text: &str) -> Vec<Word> {
    make_words_step(text, 0.4)
}

pub fn make_words_step(text: &str, step: f64) -> Vec<Word> {
    text.split_whitespace()
        .enumerate()
        .map(|(i, w)| Word::new(i as f64 * step, i as f64 * step + step * 0.8, w))
        .collect()
}

pub fn tw(text: &str, start: f64, end: f64) -> Word {
    Word::new(start, end, text)
}

pub fn seg(start: f64, end: f64, speaker: i64) -> SpeakerSegment {
    SpeakerSegment { start, end, speaker }
}

pub fn speakers(segments: &[SpeakerSegment]) -> Vec<i64> {
    segments.iter().map(|s| s.speaker).collect()
}

/// `np.concatenate([np.full(n * RATE, v, float32) for v in values])` with `seconds` per block.
pub fn blocks(values: &[f32], seconds: usize) -> Vec<f32> {
    values.iter().flat_map(|&v| std::iter::repeat_n(v, seconds * RATE)).collect()
}

/// `np.median` of a float32 array (mean of the two middle values for an even length, in float32).
pub fn median(chunk: &[f32]) -> f32 {
    let mut v = chunk.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = v.len();
    if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2.0 }
}

pub fn some(v: &[i64]) -> Vec<Option<i64>> {
    v.iter().map(|&x| Some(x)).collect()
}
