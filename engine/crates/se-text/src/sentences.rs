//! Sentence segmentation (port of `sentences.py`).
//!
//! Sentences by punctuation/silence; a long sentence is split at a comma in the second half of the window,
//! otherwise just before a capitalized word (not among the first two words, nor in the first third of the
//! window), otherwise at the `max_chars`/`max_duration` limit. With speakers, every speaker change splits.
//!
//! Every length and index is in Unicode code points (Python `str`), and the character predicates are
//! CPython 3.11's (`str.isalpha` / `isalnum` / `isupper` / `islower`, Unicode 14.0; see [`crate::unicode`]).

use se_core::{EngineError, Num, Result, Sentence, Word, pyfmt};

use crate::unicode::{py_isalnum, py_isalpha, py_islower, py_isupper};

/// `SENTENCE_END`: words ending (after closers) with one of these end a sentence (`…` is U+2026).
pub const SENTENCE_END: [char; 4] = ['.', '!', '?', '…'];
/// `SOFT_BREAK`: comma-like characters where a long sentence may be split.
pub const SOFT_BREAK: [char; 3] = [',', ';', ':'];
/// `_CLOSERS`: trailing characters ignored when looking for the final punctuation (shared with diarize).
pub const CLOSERS: [char; 7] = ['"', '\'', ')', ']', '»', '”', '’'];

/// Clitic/hyphen marks that glue a sub-word token to its neighbour (`'`, `’`, `-`).
const CLITIC_MARKS: [char; 3] = ['\'', '’', '-'];
/// Longest gap (s) across which clitic tokens are merged.
const CLITIC_MAX_GAP: f64 = 1.0;
/// Longest gap (s) across which the realignment passes move or merge words.
const REALIGN_MAX_GAP: f64 = 6.0;

/// `SegmenterConfig` (defaults `pause_gap=1.2`, `max_chars=84`, `max_duration=6.0`).
///
/// `max_chars` is an `i64` because the CLI accepts any Python int (`--max-chars -5`).
#[derive(Clone, Debug, PartialEq)]
pub struct SegmenterConfig {
    pub pause_gap: f64,
    pub max_chars: i64,
    pub max_duration: f64,
}

impl Default for SegmenterConfig {
    fn default() -> Self {
        Self { pause_gap: 1.2, max_chars: 84, max_duration: 6.0 }
    }
}

/// `_core(text)`: `text` with trailing [`CLOSERS`] removed (whitespace kept).
pub fn core(text: &str) -> &str {
    text.trim_end_matches(|c| CLOSERS.contains(&c))
}

/// Does `text` end a sentence (`_core(text).endswith(SENTENCE_END)`)? Shared with `se-diarize` (`_final`).
pub fn ends_sentence_text(text: &str) -> bool {
    core(text).ends_with(|c| SENTENCE_END.contains(&c))
}

/// `_first_alpha(text)`: the first character for which Python `isalpha()` holds.
fn first_alpha(text: &str) -> Option<char> {
    text.chars().find(|&c| py_isalpha(c))
}

/// `_starts_upper(word)`.
fn starts_upper(word: &Word) -> bool {
    first_alpha(&word.text).is_some_and(py_isupper)
}

/// `_starts_lower(word)`.
fn starts_lower(word: &Word) -> bool {
    first_alpha(&word.text).is_some_and(py_islower)
}

/// `_ends_sentence(words)`.
fn ends_sentence(words: &[Word]) -> bool {
    words.last().is_some_and(|w| ends_sentence_text(&w.text))
}

/// `_ends_break(word)`: ends (after closers) with sentence-end or soft-break punctuation.
fn ends_break(word: &Word) -> bool {
    core(&word.text).ends_with(|c| SENTENCE_END.contains(&c) || SOFT_BREAK.contains(&c))
}

/// Python `max(a, b)`: `a` unless `b` is strictly greater (keeps the original [`Num`] variant).
fn py_max(a: Num, b: Num) -> Num {
    if b > a { b } else { a }
}

/// `_merge_clitic_words(words)`: merges contraction/hyphen sub-tokens (`c` + `'est`, `T` + `'es`,
/// `celui` + `-ci`) into one word; whitespace-only words are dropped. Kept words keep their original
/// (unstripped) text; a merged word appends the stripped token.
fn merge_clitic_words(words: &[Word]) -> Vec<Word> {
    let mut merged: Vec<Word> = Vec::with_capacity(words.len());
    for w in words {
        let txt = pyfmt::strip(&w.text);
        let Some(first) = txt.chars().next() else { continue };
        let glue = merged.last().is_some_and(|prev| {
            if !(w.start_f() - prev.end_f() <= CLITIC_MAX_GAP) {
                return false;
            }
            let leading = CLITIC_MARKS.contains(&first) && txt.chars().skip(1).any(py_isalnum);
            let trailing = match prev.text.chars().next_back() {
                Some(last) if CLITIC_MARKS.contains(&last) => {
                    prev.text[..prev.text.len() - last.len_utf8()].chars().any(py_isalnum)
                }
                _ => false,
            };
            leading || trailing
        });
        if glue {
            let prev = merged.last_mut().expect("glue implies a previous word");
            prev.end = py_max(prev.end, w.end);
            prev.text.push_str(txt);
        } else {
            merged.push(w.clone());
        }
    }
    merged
}

/// `segment(words, config, speakers)`: subtitle lines with ids `1..`. If `speakers` is parallel to the
/// words, it also splits at every speaker change (a line never contains two speakers) and sets
/// `Sentence.speaker`. Errors: `speaker_list_length` (localized `ValueError`) when the lengths differ.
///
/// Sentence `start`/`end` are copied from the first/last word ([`se_core::Num`] type preserved).
pub fn segment(words: &[Word], config: &SegmenterConfig, speakers: Option<&[Option<i64>]>) -> Result<Vec<Sentence>> {
    let mut out = Vec::new();
    for (run, speaker) in speaker_runs(words, speakers)? {
        for sentence in split_sentences(&run, config.pause_gap) {
            for line in fit(&sentence, config) {
                let id = out.len() as i64 + 1;
                out.push(join(line, id, speaker));
            }
        }
    }
    Ok(out)
}

/// `_join(words, id, speaker)`.
fn join(words: Vec<Word>, id: i64, speaker: Option<i64>) -> Sentence {
    let first = words.first().expect("a subtitle line has at least one word");
    let last = words.last().expect("a subtitle line has at least one word");
    let (start, end) = (first.start, last.end);
    let text = words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
    Sentence { id, start, end, text, words, speaker, needs_review: false }
}

/// `speaker_runs(words, speakers)`: consecutive words of the same speaker; without speakers one run
/// with speaker `None`; empty words → empty list. Errors like [`segment`].
pub fn speaker_runs(words: &[Word], speakers: Option<&[Option<i64>]>) -> Result<Vec<(Vec<Word>, Option<i64>)>> {
    if words.is_empty() {
        return Ok(Vec::new());
    }
    let Some(speakers) = speakers else {
        return Ok(vec![(words.to_vec(), None)]);
    };
    if speakers.len() != words.len() {
        return Err(EngineError::value(se_core::t!("speaker_list_length")));
    }
    let mut runs: Vec<(Vec<Word>, Option<i64>)> = Vec::new();
    for (word, &speaker) in words.iter().zip(speakers) {
        match runs.last_mut() {
            Some((run, sp)) if *sp == speaker => run.push(word.clone()),
            _ => runs.push((vec![word.clone()], speaker)),
        }
    }
    Ok(runs)
}

/// `split_sentences(words, pause_gap)`: closes a sentence at a word ending with punctuation or followed
/// by a pause longer than `pause_gap`, after clitic merging and the two realignment passes.
pub fn split_sentences(words: &[Word], pause_gap: f64) -> Vec<Vec<Word>> {
    let words = merge_clitic_words(words);
    let short_word_limit = f64::max(pause_gap * 3.5, 4.2);
    let mut result: Vec<Vec<Word>> = Vec::new();
    let mut current: Vec<Word> = Vec::new();
    for (i, word) in words.iter().enumerate() {
        current.push(word.clone());
        let nxt = words.get(i + 1);
        let gap = nxt.map_or(0.0, |n| n.start_f() - word.end_f());
        let mut long_pause = nxt.is_some() && gap >= pause_gap;
        let c = core(&word.text);
        let ends = c.ends_with(|ch| SENTENCE_END.contains(&ch));
        // Avoid isolating a single unpunctuated short word ("Je", "Tu", "es") unless the silence is very long.
        if long_pause && current.len() == 1 && pyfmt::char_len(pyfmt::strip(c)) <= 3 && !ends && gap < short_word_limit
        {
            long_pause = false;
        }
        if ends || long_pause || nxt.is_none() {
            result.push(std::mem::take(&mut current));
        }
    }
    realign_sentences(result)
}

/// `_realign_sentences(sentences)`: pass 1 moves a trailing capitalized 1-2 word starter onto a lowercase
/// continuation; pass 2 merges short unpunctuated fragments into a following lowercase continuation.
fn realign_sentences(sentences: Vec<Vec<Word>>) -> Vec<Vec<Word>> {
    if sentences.len() <= 1 {
        return sentences;
    }
    let mut aligned: Vec<Vec<Word>> = sentences.into_iter().filter(|s| !s.is_empty()).collect();

    // Pass 1: detach trailing uppercase sentence starters before a lowercase continuation. Iteration i sees
    // aligned[i] after iteration i-1 may have prepended words to it (in-place mutation, like Python).
    for i in 0..aligned.len().saturating_sub(1) {
        let (curr, nxt) = (&aligned[i], &aligned[i + 1]);
        if curr.is_empty() || nxt.is_empty() || ends_sentence(curr) || !starts_lower(&nxt[0]) {
            continue;
        }
        if nxt[0].start_f() - curr[curr.len() - 1].end_f() > REALIGN_MAX_GAP {
            continue;
        }
        // for k in range(len(curr) - 1, max(0, len(curr) - 3), -1)
        let n = curr.len() as isize;
        let lowest = isize::max(0, n - 3);
        let mut split_at = None;
        let mut k = n - 1;
        while k > lowest {
            let ku = k as usize;
            if ku >= 1 && starts_upper(&curr[ku]) && !curr[ku..].iter().any(ends_break) {
                split_at = Some(ku);
                break;
            }
            k -= 1;
        }
        if let Some(k) = split_at {
            let mut moved = aligned[i].split_off(k);
            moved.append(&mut aligned[i + 1]);
            aligned[i + 1] = moved;
        }
    }

    // Pass 2: merge short unpunctuated starter/middle fragments into a lowercase continuation.
    let mut merged: Vec<Vec<Word>> = Vec::new();
    let mut idx = 0;
    while idx < aligned.len() {
        let mut curr = std::mem::take(&mut aligned[idx]);
        while idx + 1 < aligned.len() {
            let nxt = &aligned[idx + 1];
            if curr.is_empty() || nxt.is_empty() || ends_sentence(&curr) || !starts_lower(&nxt[0]) {
                break;
            }
            if nxt[0].start_f() - curr[curr.len() - 1].end_f() > REALIGN_MAX_GAP {
                break;
            }
            let should_merge = curr.len() == 1
                || (curr.len() <= 3 && starts_upper(&curr[0]))
                || (nxt.len() == 1 && !starts_upper(&nxt[0]));
            if !should_merge {
                break;
            }
            curr.append(&mut aligned[idx + 1]);
            idx += 1;
        }
        if !curr.is_empty() {
            merged.push(curr);
        }
        idx += 1;
    }
    merged
}

/// `fit(words, config)`: splits one sentence into lines within `max_chars` / `max_duration`
/// (iterative; must handle 20 000 words quickly).
pub fn fit(words: &[Word], config: &SegmenterConfig) -> Vec<Vec<Word>> {
    // prefix[i] = total code point length of words[..i], so `_too_long` on any range is O(1).
    let mut prefix = Vec::with_capacity(words.len() + 1);
    prefix.push(0i64);
    for w in words {
        let last = prefix[prefix.len() - 1];
        prefix.push(last + pyfmt::char_len(&w.text) as i64);
    }
    // `_too_long(words[a..b])` for a non-empty range.
    let too_long = |a: usize, b: usize| -> bool {
        let chars = prefix[b] - prefix[a] + (b - a) as i64 - 1;
        chars > config.max_chars || words[b - 1].end_f() - words[a].start_f() > config.max_duration
    };
    let mut result = Vec::new();
    let n = words.len();
    let mut start = 0;
    while n - start > 1 && too_long(start, n) {
        let len = n - start;
        let mut cut = 1;
        while cut < len && !too_long(start, start + cut + 1) {
            cut += 1;
        }
        // Last comma/semicolon/colon in the window (cut after it), if in the window's second half.
        let soft = (0..cut - 1).rev().find(|&i| core(&words[start + i].text).ends_with(|c| SOFT_BREAK.contains(&c)));
        match soft.map(|i| i + 1) {
            Some(s) if s >= cut / 2 => cut = s,
            _ => {
                // Otherwise before the last capitalized word at index >= 2 and >= cut // 3.
                if let Some(u) = (usize::max(2, cut / 3)..cut).rev().find(|&i| starts_upper(&words[start + i])) {
                    cut = u;
                }
            }
        }
        result.push(words[start..start + cut].to_vec());
        start += cut;
    }
    result.push(words[start..].to_vec());
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_strips_only_closers() {
        assert_eq!(core("\"Evet.\""), "\"Evet.");
        assert_eq!(core("(gitti!)"), "(gitti!");
        assert_eq!(core("kalktım,\""), "kalktım,");
        assert_eq!(core("Evet. "), "Evet. ");
        assert!(ends_sentence_text("«Evet…»"));
        assert!(!ends_sentence_text("Evet. "));
    }

    #[test]
    fn first_alpha_uses_python_isalpha() {
        assert_eq!(first_alpha("(gitti!)"), Some('g'));
        assert_eq!(first_alpha("'es"), Some('e'));
        assert_eq!(first_alpha("123"), None);
        // U+0903 DEVANAGARI SIGN VISARGA is Alphabetic (Mc) but not Python isalpha.
        assert_eq!(first_alpha("\u{0903}a"), Some('a'));
        assert!(starts_upper(&Word::new(0.0, 1.0, "İyi")));
        assert!(starts_lower(&Word::new(0.0, 1.0, "ıslak")));
        // Titlecase U+01C5 is neither upper nor lower.
        let dz = Word::new(0.0, 1.0, "\u{01C5}x");
        assert!(!starts_upper(&dz) && !starts_lower(&dz));
    }

    #[test]
    fn clitic_merge_matches_python() {
        let words = [
            Word::new(0.0, 0.1, "c'"),
            Word::new(0.1, 0.2, "est"),
            Word::new(0.2, 0.3, "'"),
            Word::new(0.3, 0.4, "-"),
            Word::new(0.4, 0.5, "x-"),
            Word::new(0.5, 0.6, " y "),
            Word::new(2.0, 2.1, "'s"),
        ];
        let texts: Vec<String> = merge_clitic_words(&words).into_iter().map(|w| w.text).collect();
        assert_eq!(texts, ["c'est", "'", "-", "x-y", "'s"]);
        let overlap = merge_clitic_words(&[Word::new(0.0, 2.0, "celui"), Word::new(1.5, 1.8, "-ci")]);
        assert_eq!(overlap, vec![Word::new(0.0, 2.0, "celui-ci")]);
    }

    #[test]
    fn fit_on_empty_returns_one_empty_line() {
        assert_eq!(fit(&[], &SegmenterConfig::default()), vec![Vec::<Word>::new()]);
    }
}
