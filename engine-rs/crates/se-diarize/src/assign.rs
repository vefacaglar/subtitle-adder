//! Word → speaker assignment (`assign_speakers`, `_SegmentIndex`, `_smooth`, `_snap`, `_renumber`;
//! spec diarize.md §3, §8).
//!
//! All arithmetic is `f64` in the same order as the Python reference, with the same `<` / `<=` operators:
//! several tests sit right on binary rounding boundaries (`2.2 - 1.4 > 0.8`).

use std::cmp::Ordering;

use se_core::Word;

use crate::{SNAP_MAX_SECONDS, SNAP_MAX_WORDS, SpeakerSegment};

/// `SENTENCE_FINAL`: punctuation that ends a sentence (`…` is U+2026).
pub const SENTENCE_FINAL: [char; 4] = ['.', '?', '!', '…'];

/// `text.rstrip(_CLOSERS)`: only the seven closing quote/bracket characters, never whitespace.
pub(crate) fn strip_closers(text: &str) -> &str {
    text.trim_end_matches(|c| se_text::sentences::CLOSERS.contains(&c))
}

/// `_final(word)`: does the word end a sentence (`.`, `?`, `!`, `…`, possibly followed by closers)?
pub(crate) fn is_final(word: &Word) -> bool {
    strip_closers(&word.text).ends_with(|c| SENTENCE_FINAL.contains(&c))
}

/// Does the word end a question (`rstrip(_CLOSERS).endswith("?")`)?
pub(crate) fn is_question(word: &Word) -> bool {
    strip_closers(&word.text).ends_with('?')
}

/// Python `bisect_left(a, x)`.
pub(crate) fn bisect_left(a: &[f64], x: f64) -> usize {
    a.partition_point(|&v| v < x)
}

/// Python `bisect_right(a, x, 0, hi)`.
pub(crate) fn bisect_right(a: &[f64], x: f64, hi: usize) -> usize {
    a[..hi].partition_point(|&v| v <= x)
}

/// Python `sorted(..., key=lambda s: (s.start, s.end))` comparator (stable sort with float keys).
pub(crate) fn cmp_start_end(a: &SpeakerSegment, b: &SpeakerSegment) -> Ordering {
    cmp_f64(a.start, b.start).then_with(|| cmp_f64(a.end, b.end))
}

/// Total-enough float ordering for sorting (NaN compares equal, as Python's `<` would be false both ways).
pub(crate) fn cmp_f64(a: f64, b: f64) -> Ordering {
    a.partial_cmp(&b).unwrap_or(Ordering::Equal)
}

/// `_distance(point, segment)`.
pub(crate) fn distance(point: f64, segment: &SpeakerSegment) -> f64 {
    if point < segment.start {
        return segment.start - point;
    }
    py_max(point - segment.end, 0.0)
}

/// `assign_speakers(words, segments)`: each word gets the speaker with the largest time overlap (nearest
/// segment if none); lone words between two longer runs of one speaker join them unless they are a complete
/// short reply; a mid-sentence change snaps to the nearest sentence end (≤ 2 words / 0.8 s); renumbered by
/// first appearance. Every element is `None` only when there are no segments.
pub fn assign_speakers(words: &[Word], segments: &[SpeakerSegment]) -> Vec<Option<i64>> {
    if segments.is_empty() {
        return vec![None; words.len()];
    }
    let index = SegmentIndex::new(segments);
    let raw: Vec<Option<i64>> = words.iter().map(|w| Some(index.speaker_for(w))).collect();
    renumber(&snap(words, &smooth(words, &raw)))
}

/// `_SegmentIndex`: sorted segment starts + running max ends for O(log n) overlap lookups.
pub struct SegmentIndex {
    /// Segments stably sorted by `(start, end)`.
    pub segments: Vec<SpeakerSegment>,
    pub starts: Vec<f64>,
    /// `max_end[i]`: the largest end among `segments[..=i]`.
    pub max_end: Vec<f64>,
    /// `arg_end[i]`: index of the (earliest) segment holding `max_end[i]`.
    pub arg_end: Vec<usize>,
}

impl SegmentIndex {
    pub fn new(segments: &[SpeakerSegment]) -> Self {
        let mut sorted = segments.to_vec();
        sorted.sort_by(cmp_start_end);
        let starts = sorted.iter().map(|s| s.start).collect();
        let mut max_end = Vec::with_capacity(sorted.len());
        let mut arg_end = Vec::with_capacity(sorted.len());
        let mut best: Option<usize> = None;
        for (i, seg) in sorted.iter().enumerate() {
            match best {
                Some(b) if !(seg.end > sorted[b].end) => {}
                _ => best = Some(i),
            }
            let b = best.unwrap_or(i);
            max_end.push(sorted[b].end);
            arg_end.push(b);
        }
        Self { segments: sorted, starts, max_end, arg_end }
    }

    /// `(lo, hi)`: the index range of segments that may overlap `[start, end]`.
    pub(crate) fn window(&self, start: f64, end: f64) -> (usize, usize) {
        let hi = bisect_left(&self.starts, end);
        let lo = bisect_right(&self.max_end, start, hi);
        (lo, hi)
    }

    /// `speaker_for(word)`: largest overlap (ties: smallest speaker id), else the nearest segment (ties:
    /// earlier start, then the first candidate). Requires at least one segment.
    pub fn speaker_for(&self, word: &Word) -> i64 {
        self.speaker_for_times(word.start_f(), word.end_f())
    }

    pub(crate) fn speaker_for_times(&self, w_start: f64, w_end: f64) -> i64 {
        let (lo, hi) = self.window(w_start, w_end);
        let mut overlap: Vec<(i64, f64)> = Vec::new();
        for seg in &self.segments[lo..hi] {
            let shared = py_min(w_end, seg.end) - py_max(w_start, seg.start);
            if shared > 0.0 {
                match overlap.iter_mut().find(|(spk, _)| *spk == seg.speaker) {
                    Some((_, total)) => *total += shared,
                    None => overlap.push((seg.speaker, shared)),
                }
            }
        }
        if !overlap.is_empty() {
            // max by (overlap, -speaker); the first maximal item wins (keys are unique anyway).
            let mut best = overlap[0];
            for &(spk, total) in &overlap[1..] {
                if total > best.1 || (total == best.1 && -spk > -best.0) {
                    best = (spk, total);
                }
            }
            return best.0;
        }
        let middle = (w_start + w_end) / 2.0;
        let mut candidates: Vec<&SpeakerSegment> = Vec::with_capacity(2);
        if hi > 0 {
            candidates.push(&self.segments[self.arg_end[hi - 1]]);
        }
        if hi < self.segments.len() {
            candidates.push(&self.segments[hi]);
        }
        let mut best = candidates[0];
        let mut best_key = (distance(middle, best), best.start);
        for &c in &candidates[1..] {
            let key = (distance(middle, c), c.start);
            if key.0 < best_key.0 || (key.0 == best_key.0 && key.1 < best_key.1) {
                best = c;
                best_key = key;
            }
        }
        best.speaker
    }
}

/// Python `min(a, b)` for floats: returns `a` unless `b < a`.
pub(crate) fn py_min(a: f64, b: f64) -> f64 {
    if b < a { b } else { a }
}

/// Python `max(a, b)` for floats: returns `a` unless `b > a`.
pub(crate) fn py_max(a: f64, b: f64) -> f64 {
    if b > a { b } else { a }
}

/// `_runs(speakers)`: `(start, end exclusive)` ranges of equal consecutive speakers.
pub(crate) fn runs(speakers: &[Option<i64>]) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = Vec::new();
    for (i, speaker) in speakers.iter().enumerate() {
        match out.last_mut() {
            Some(last) if speakers[last.0] == *speaker => last.1 = i + 1,
            _ => out.push((i, i + 1)),
        }
    }
    out
}

/// `_smooth(words, speakers)`: joins a single-word (or brief 2-word mid-sentence) island to its
/// surrounding speaker; decided on the original assignment (no cascading).
pub(crate) fn smooth(words: &[Word], speakers: &[Option<i64>]) -> Vec<Option<i64>> {
    let mut result = speakers.to_vec();
    let runs = runs(speakers);
    if runs.len() < 3 {
        return result;
    }
    for k in 1..runs.len() - 1 {
        let (start, end) = runs[k];
        let before = runs[k - 1];
        let after = runs[k + 1];
        let island_len = end - start;
        if !(island_len == 1 || island_len == 2) || speakers[before.0].is_none() {
            continue;
        }
        if speakers[before.0] != speakers[after.0] {
            continue;
        }
        let min_surround = if island_len == 1 { 2 } else { 3 };
        if before.1 - before.0 < min_surround || after.1 - after.0 < min_surround {
            continue;
        }
        if is_final(&words[start - 1]) || (start..end).any(|idx| is_final(&words[idx])) {
            continue;
        }
        if words[end - 1].end_f() - words[start].start_f() > 0.9 {
            continue;
        }
        if words[start].start_f() - words[start - 1].end_f() > 0.25
            || words[end].start_f() - words[end - 1].end_f() > 0.25
        {
            continue;
        }
        for slot in &mut result[start..end] {
            *slot = speakers[before.0];
        }
    }
    result
}

/// `_snap(words, speakers)`: moves a mid-sentence speaker change to the nearest sentence end when that
/// touches at most `SNAP_MAX_WORDS` words and `SNAP_MAX_SECONDS` of speech (works on and cascades through
/// its own result).
pub(crate) fn snap(words: &[Word], speakers: &[Option<i64>]) -> Vec<Option<i64>> {
    let mut result = speakers.to_vec();
    let n = words.len();
    let mut i = 1usize;
    while i < n {
        let before = result[i - 1];
        let after = result[i];
        if before.is_none() || after.is_none() || before == after || is_final(&words[i - 1]) {
            i += 1;
            continue;
        }
        if words[i].start_f() - words[i - 1].end_f() > 0.25 {
            i += 1;
            continue;
        }
        let mut forward: Option<usize> = None;
        for j in i..(i + SNAP_MAX_WORDS).min(n) {
            if result[j] != after {
                break;
            }
            if j > i && words[j].start_f() - words[j - 1].end_f() > 0.25 {
                break;
            }
            if is_final(&words[j]) {
                if words[j].end_f() - words[i].start_f() <= SNAP_MAX_SECONDS {
                    forward = Some(j);
                }
                break;
            }
        }
        let mut backward: Option<usize> = None;
        // range(i - 2, max(i - 2 - SNAP_MAX_WORDS, -1), -1)
        let first = i as i64 - 2;
        let stop = (i as i64 - 2 - SNAP_MAX_WORDS as i64).max(-1);
        let mut m = first;
        while m > stop {
            let mu = m as usize;
            if result[mu + 1] != before {
                break;
            }
            if words[mu + 2].start_f() - words[mu + 1].end_f() > 0.25 {
                break;
            }
            if is_final(&words[mu]) {
                if words[i - 1].end_f() - words[mu + 1].start_f() <= SNAP_MAX_SECONDS {
                    backward = Some(mu + 1);
                }
                break;
            }
            m -= 1;
        }
        let fwd_cost = forward.map(|f| (f - i + 1, words[f].end_f() - words[i].start_f()));
        let bwd_cost = backward.map(|b| (i - b, words[i - 1].end_f() - words[b].start_f()));
        match (forward, fwd_cost) {
            (Some(f), Some(fc)) if bwd_cost.is_none_or(|bc| tuple_le(fc, bc)) => {
                for slot in &mut result[i..=f] {
                    *slot = before;
                }
                i = f + 1;
            }
            _ => {
                if let Some(b) = backward {
                    for slot in &mut result[b..i] {
                        *slot = after;
                    }
                }
                i += 1;
            }
        }
    }
    result
}

/// Python tuple `<=` on `(int, float)`.
fn tuple_le(a: (usize, f64), b: (usize, f64)) -> bool {
    if a.0 != b.0 {
        return a.0 < b.0;
    }
    a.1 <= b.1
}

/// `_renumber(speakers)`: non-`None` ids become 0, 1, 2… by first appearance.
pub(crate) fn renumber(speakers: &[Option<i64>]) -> Vec<Option<i64>> {
    let mut order: Vec<i64> = Vec::new();
    speakers
        .iter()
        .map(|s| {
            s.map(|spk| match order.iter().position(|&o| o == spk) {
                Some(p) => p as i64,
                None => {
                    order.push(spk);
                    (order.len() - 1) as i64
                }
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(text: &str, start: f64, end: f64) -> Word {
        Word::new(start, end, text)
    }

    #[test]
    fn final_and_question_strip_only_closers() {
        assert!(is_final(&w("dersin?”", 0.0, 1.0)));
        assert!(is_final(&w("bitti…", 0.0, 1.0)));
        assert!(!is_final(&w("bitti. ", 0.0, 1.0)));
        assert!(!is_final(&w("", 0.0, 1.0)));
        assert!(is_question(&w("ne?)", 0.0, 1.0)));
        assert!(!is_question(&w("ne.", 0.0, 1.0)));
    }

    #[test]
    fn runs_and_renumber() {
        assert_eq!(runs(&[Some(1), Some(1), None, None, Some(2)]), vec![(0, 2), (2, 4), (4, 5)]);
        assert_eq!(renumber(&[Some(5), None, Some(3), Some(5)]), vec![Some(0), None, Some(1), Some(0)]);
        assert!(runs(&[]).is_empty());
    }

    #[test]
    fn empty_words_do_not_underflow() {
        assert!(smooth(&[], &[]).is_empty());
        assert!(snap(&[], &[]).is_empty());
        assert!(assign_speakers(&[], &[SpeakerSegment { start: 0.0, end: 1.0, speaker: 0 }]).is_empty());
    }

    #[test]
    fn backward_loop_ranges_match_python() {
        // range(i-2, max(i-4, -1), -1): i=1 → [], i=2 → [0], i=3 → [1, 0], i=5 → [3, 2]
        let ranges = |i: i64| {
            let mut out = Vec::new();
            let mut m = i - 2;
            let stop = (i - 2 - SNAP_MAX_WORDS as i64).max(-1);
            while m > stop {
                out.push(m);
                m -= 1;
            }
            out
        };
        assert_eq!(ranges(1), Vec::<i64>::new());
        assert_eq!(ranges(2), vec![0]);
        assert_eq!(ranges(3), vec![1, 0]);
        assert_eq!(ranges(5), vec![3, 2]);
    }

    #[test]
    fn equal_overlap_prefers_smallest_speaker_and_equal_distance_prefers_earlier_start() {
        let segs = [
            SpeakerSegment { start: 0.0, end: 1.0, speaker: 3 },
            SpeakerSegment { start: 1.0, end: 2.0, speaker: 1 },
        ];
        let index = SegmentIndex::new(&segs);
        assert_eq!(index.speaker_for(&w("x", 0.5, 1.5)), 1);
        let segs = [
            SpeakerSegment { start: 0.0, end: 1.0, speaker: 3 },
            SpeakerSegment { start: 3.0, end: 4.0, speaker: 1 },
        ];
        let index = SegmentIndex::new(&segs);
        assert_eq!(index.speaker_for(&w("x", 1.5, 2.5)), 3);
    }
}
