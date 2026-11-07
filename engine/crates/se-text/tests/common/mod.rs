#![allow(dead_code)]

use se_core::Word;

/// `conftest.make_words(text, step)`: word i = `Word(i*step, i*step + step*0.8, token)`.
pub fn make_words_step(text: &str, step: f64) -> Vec<Word> {
    text.split_whitespace()
        .enumerate()
        .map(|(i, w)| {
            let start = i as f64 * step;
            Word::new(start, start + step * 0.8, w)
        })
        .collect()
}

/// `conftest.make_words(text)` with the default step 0.4.
pub fn make_words(text: &str) -> Vec<Word> {
    make_words_step(text, 0.4)
}

pub fn texts(sentences: &[se_core::Sentence]) -> Vec<&str> {
    sentences.iter().map(|s| s.text.as_str()).collect()
}
