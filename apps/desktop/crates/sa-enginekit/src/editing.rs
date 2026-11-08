//! Sentence-list editing: pure functions on `Vec<Sentence>` (win-enginekit §6 = mac-enginekit §13).
//! All mutators return "changed"; an out-of-range index changes nothing and returns false.

use std::collections::{HashMap, HashSet, VecDeque};

use sa_core::engine_types::{Sentence, Word};
use uuid::Uuid;

/// Shortest line (s).
pub const MINIMUM_DURATION: f64 = 0.1;
/// Adjustment button step (s).
pub const NUDGE_STEP: f64 = 0.1;
/// Largest start difference of a near match in [`SentenceListExt::merged_with_snapshot`] (s).
pub const SNAPSHOT_NEAR_MATCH_SECONDS: f64 = 1.5;

/// Millisecond rounding, half away from zero.
pub fn rounded(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

/// Number of user-perceived characters, as each platform's app counted them for the proportional split:
/// * macOS: extended grapheme clusters of current Unicode (Swift's `String.count`, which applies the Indic conjunct
///   rule GB9c);
/// * Windows: .NET's `StringInfo.LengthInTextElements`, the same clusters **without** GB9c, so a virama does not join
///   the next consonant (`"नमस्ते"` is 4 there, 3 on macOS). Checked against frozen C# outputs by `tests/csharp_compat.rs`.
pub fn grapheme_len(text: &str) -> usize {
    let clusters = unicode_segmentation::UnicodeSegmentation::graphemes(text, true);
    if cfg!(target_os = "macos") { clusters.count() } else { clusters.map(clusters_without_conjunct_rule).sum() }
}

/// How many clusters one extended grapheme cluster is without GB9c: every character after an `InCB=Linker` (virama)
/// that is not itself an extending character starts a new one.
fn clusters_without_conjunct_rule(cluster: &str) -> usize {
    let mut count = 1;
    let mut after_linker = false;
    for (i, c) in cluster.chars().enumerate() {
        if i > 0 && after_linker && !extends_cluster(c) {
            count += 1;
            after_linker = false;
        }
        if is_incb_linker(c) {
            after_linker = true;
        }
    }
    count
}

/// `Indic_Conjunct_Break=Linker` (Unicode 17, the version of the `unicode-segmentation` tables).
fn is_incb_linker(c: char) -> bool {
    matches!(
        c,
        '\u{94D}' | '\u{9CD}' | '\u{ACD}' | '\u{B4D}' | '\u{C4D}' | '\u{D4D}' | '\u{1039}' | '\u{17D2}' | '\u{1A60}'
            | '\u{1B44}' | '\u{1BAB}' | '\u{A9C0}' | '\u{AAF6}' | '\u{10A3F}' | '\u{11133}' | '\u{113D0}' | '\u{1193E}'
            | '\u{11A47}' | '\u{11A99}' | '\u{11F42}'
    )
}

/// Characters that never start a cluster after a virama: marks, ZWNJ/ZWJ, variation selectors, tags, emoji modifiers.
fn extends_cluster(c: char) -> bool {
    let cp = c as u32;
    unicode_normalization::char::is_combining_mark(c)
        || matches!(cp, 0x200C | 0x200D | 0xFE00..=0xFE0F | 0xE0100..=0xE01EF | 0xE0020..=0xE007F | 0x1F3FB..=0x1F3FF)
}

fn join_trimmed<'a>(parts: impl IntoIterator<Item = &'a str>) -> String {
    parts.into_iter().map(str::trim).filter(|t| !t.is_empty()).collect::<Vec<_>>().join(" ")
}

/// `clamped(value, current, bounds)`: the rounded target inside the bounds, or `None` when nothing would change or the
/// clamp would move against the requested direction (direction uses the unrounded value).
fn clamped(value: f64, current: f64, lower: f64, upper: f64) -> Option<f64> {
    let target = rounded(value).max(lower).min(upper);
    if target == current {
        return None;
    }
    if !((value - current) * (target - current) > 0.0) {
        return None;
    }
    Some(target)
}

/// Editing operations on a sentence list.
pub trait SentenceListExt {
    /// First `i` with `start <= t < end`.
    fn active_index(&self, t: f64) -> Option<usize>;
    fn index_of_uid(&self, uid: Uuid) -> Option<usize>;
    fn set_start(&mut self, value: f64, i: usize) -> bool;
    fn set_end(&mut self, value: f64, i: usize) -> bool;
    fn nudge_start(&mut self, i: usize, delta: f64) -> bool;
    fn nudge_end(&mut self, i: usize, delta: f64) -> bool;
    /// macOS rule (mac-enginekit §13): a changed text also clears `needs_review` (a hand edit resolves the marker).
    fn set_text(&mut self, text: &str, i: usize) -> bool;
    /// Windows rule (win-enginekit §6, §13.10): a changed text keeps `needs_review`; the view model clears the
    /// translation marker itself.
    fn set_text_keeping_review(&mut self, text: &str, i: usize) -> bool;
    fn set_speaker(&mut self, speaker: Option<i64>, i: usize) -> bool;
    /// Joins `i` and `i + 1` (keeps `i`'s uid/id/speaker, resets `needs_review`), renumbers.
    fn merge_with_next(&mut self, i: usize) -> bool;
    fn delete_sentence(&mut self, i: usize) -> bool;
    /// Splits at the playhead (word midpoints, else token halves by grapheme length); see win-enginekit §6.
    fn split_sentence(&mut self, i: usize, time: f64) -> bool;
    fn can_split(&self, i: usize, time: f64) -> bool;
    /// `id = i + 1` where it differs; uids untouched.
    fn renumber(&mut self);
    /// Distinct non-null speakers, ascending.
    fn speakers(&self) -> Vec<i64>;
    /// Max speaker + 1, or 0.
    fn next_new_speaker(&self) -> i64;
    /// The render-time dialogue dash rule (needs ≥ 2 distinct non-null speakers).
    fn dialogue_dashes(&self) -> Vec<bool>;
    fn starts_dialogue_turn(&self, i: usize) -> bool;
    /// `DialogueDash::apply(starts_dialogue_turn(i), text)`.
    fn display_text(&self, i: usize) -> String;
    /// A live `partial`/`result` snapshot replacing the list, keeping the uids of unchanged lines
    /// (mac-enginekit §13 `merged_with_snapshot` = win-viewmodels §10.1 `MergeSentencesPreservingUid`; the
    /// millisecond key uses banker's rounding, UNIFIED.md §3.7).
    fn merged_with_snapshot(&self, snapshot: &[Sentence]) -> Vec<Sentence>;
}

impl SentenceListExt for Vec<Sentence> {
    fn active_index(&self, t: f64) -> Option<usize> {
        self.iter().position(|s| s.start <= t && t < s.end)
    }

    fn index_of_uid(&self, uid: Uuid) -> Option<usize> {
        self.iter().position(|s| s.uid == uid)
    }

    fn set_start(&mut self, value: f64, i: usize) -> bool {
        let Some(s) = self.get(i) else { return false };
        let lower = if i > 0 { self[i - 1].end } else { 0.0 };
        let upper = s.end - MINIMUM_DURATION;
        if upper < lower {
            return false;
        }
        match clamped(value, s.start, lower, upper) {
            Some(target) => {
                self[i].start = target;
                true
            }
            None => false,
        }
    }

    fn set_end(&mut self, value: f64, i: usize) -> bool {
        let Some(s) = self.get(i) else { return false };
        let lower = s.start + MINIMUM_DURATION;
        let upper = self.get(i + 1).map_or(f64::INFINITY, |n| n.start);
        if upper < lower {
            return false;
        }
        match clamped(value, s.end, lower, upper) {
            Some(target) => {
                self[i].end = target;
                true
            }
            None => false,
        }
    }

    fn nudge_start(&mut self, i: usize, delta: f64) -> bool {
        let Some(s) = self.get(i) else { return false };
        let value = s.start + delta;
        self.set_start(value, i)
    }

    fn nudge_end(&mut self, i: usize, delta: f64) -> bool {
        let Some(s) = self.get(i) else { return false };
        let value = s.end + delta;
        self.set_end(value, i)
    }

    fn set_text(&mut self, text: &str, i: usize) -> bool {
        let Some(s) = self.get_mut(i) else { return false };
        if s.text == text {
            return false;
        }
        s.text = text.to_owned();
        s.needs_review = false;
        true
    }

    fn set_text_keeping_review(&mut self, text: &str, i: usize) -> bool {
        let Some(s) = self.get_mut(i) else { return false };
        if s.text == text {
            return false;
        }
        s.text = text.to_owned();
        true
    }

    fn set_speaker(&mut self, speaker: Option<i64>, i: usize) -> bool {
        let Some(s) = self.get_mut(i) else { return false };
        if s.speaker == speaker {
            return false;
        }
        s.speaker = speaker;
        true
    }

    fn merge_with_next(&mut self, i: usize) -> bool {
        if i + 1 >= self.len() {
            return false;
        }
        let next = self.remove(i + 1);
        let first = &mut self[i];
        first.text = join_trimmed([first.text.as_str(), next.text.as_str()]);
        first.end = next.end;
        first.words.extend(next.words);
        first.needs_review = false;
        self.renumber();
        true
    }

    fn delete_sentence(&mut self, i: usize) -> bool {
        if i >= self.len() {
            return false;
        }
        self.remove(i);
        self.renumber();
        true
    }

    fn split_sentence(&mut self, i: usize, time: f64) -> bool {
        let Some(plan) = split_plan(self, i, time) else { return false };
        let original = &mut self[i];
        let second = Sentence {
            id: original.id + 1,
            uid: Uuid::new_v4(),
            start: plan.boundary,
            end: original.end,
            text: plan.right_text,
            words: plan.right_words,
            speaker: original.speaker,
            needs_review: false,
        };
        original.end = plan.boundary;
        original.text = plan.left_text;
        original.words = plan.left_words;
        self.insert(i + 1, second);
        self.renumber();
        true
    }

    fn can_split(&self, i: usize, time: f64) -> bool {
        let Some(s) = self.get(i) else { return false };
        let p = rounded(time);
        if !(s.start < p && p < s.end) || s.end - s.start < 2.0 * MINIMUM_DURATION {
            return false;
        }
        s.words.len() >= 2 || s.text.split_whitespace().count() >= 2
    }

    fn renumber(&mut self) {
        for (i, s) in self.iter_mut().enumerate() {
            let id = i as i64 + 1;
            if s.id != id {
                s.id = id;
            }
        }
    }

    fn speakers(&self) -> Vec<i64> {
        let mut v: Vec<i64> = self.iter().filter_map(|s| s.speaker).collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    fn next_new_speaker(&self) -> i64 {
        self.speakers().last().map_or(0, |m| m + 1)
    }

    fn dialogue_dashes(&self) -> Vec<bool> {
        if self.speakers().len() < 2 {
            return vec![false; self.len()];
        }
        let mut prev: Option<i64> = None;
        self.iter()
            .map(|s| {
                let dash = s.speaker.is_some() && s.speaker != prev;
                if s.speaker.is_some() {
                    prev = s.speaker;
                }
                dash
            })
            .collect()
    }

    fn starts_dialogue_turn(&self, i: usize) -> bool {
        i < self.len() && self.dialogue_dashes()[i]
    }

    fn display_text(&self, i: usize) -> String {
        DialogueDash::apply(self.starts_dialogue_turn(i), &self[i].text)
    }

    fn merged_with_snapshot(&self, snapshot: &[Sentence]) -> Vec<Sentence> {
        if self.is_empty() {
            return snapshot.to_vec();
        }
        fn ms(v: f64) -> i64 {
            (v * 1000.0).round_ties_even() as i64
        }
        type Key<'a> = (i64, i64, &'a str);
        let mut queues: HashMap<Key<'_>, VecDeque<Uuid>> = HashMap::new();
        for s in self {
            queues.entry((ms(s.start), ms(s.end), s.text.as_str())).or_default().push_back(s.uid);
        }
        let mut used: HashSet<Uuid> = HashSet::new();
        let mut assigned: Vec<Option<Uuid>> = vec![None; snapshot.len()];
        // Pass 1: exact matches (identical duplicates each take their own uid, in order).
        for (n, incoming) in snapshot.iter().enumerate() {
            if let Some(q) = queues.get_mut(&(ms(incoming.start), ms(incoming.end), incoming.text.as_str())) {
                while let Some(uid) = q.pop_front() {
                    if used.insert(uid) {
                        assigned[n] = Some(uid);
                        break;
                    }
                }
            }
        }
        // Pass 2: near matches (same text, start within 1.5 s), first unused existing line in list order.
        for (n, incoming) in snapshot.iter().enumerate() {
            if assigned[n].is_some() {
                continue;
            }
            if let Some(old) = self.iter().find(|old| {
                !used.contains(&old.uid)
                    && old.text == incoming.text
                    && (old.start - incoming.start).abs() <= SNAPSHOT_NEAR_MATCH_SECONDS
            }) {
                used.insert(old.uid);
                assigned[n] = Some(old.uid);
            }
        }
        snapshot
            .iter()
            .zip(assigned)
            .map(|(s, uid)| {
                let mut out = s.clone();
                if let Some(uid) = uid {
                    out.uid = uid;
                }
                out
            })
            .collect()
    }
}

struct SplitPlan {
    boundary: f64,
    left_text: String,
    right_text: String,
    left_words: Vec<Word>,
    right_words: Vec<Word>,
}

fn split_plan(list: &[Sentence], i: usize, time: f64) -> Option<SplitPlan> {
    let s = list.get(i)?;
    let playhead = rounded(time);
    if !(s.start < playhead && playhead < s.end) || s.end - s.start < 2.0 * MINIMUM_DURATION {
        return None;
    }
    let lower = s.start + MINIMUM_DURATION;
    let upper = s.end - MINIMUM_DURATION;
    let (left_text, right_text, left_words, right_words, raw);
    if s.words.len() >= 2 {
        let n = s.words.len();
        let left_count = s.words.iter().take_while(|w| w.midpoint() < playhead).count();
        let split = left_count.clamp(1, n - 1);
        let (l, r) = s.words.split_at(split);
        left_text = join_trimmed(l.iter().map(|w| w.text.as_str()));
        right_text = join_trimmed(r.iter().map(|w| w.text.as_str()));
        raw = (l[l.len() - 1].end + r[0].start) / 2.0;
        left_words = l.to_vec();
        right_words = r.to_vec();
    } else {
        let tokens: Vec<&str> = s.text.split_whitespace().collect();
        if tokens.len() < 2 {
            return None;
        }
        let middle = tokens.len() / 2;
        left_text = tokens[..middle].join(" ");
        right_text = tokens[middle..].join(" ");
        let (ll, rl) = (grapheme_len(&left_text) as f64, grapheme_len(&right_text) as f64);
        raw = s.start + (s.end - s.start) * ll / (ll + rl).max(1.0);
        let cut = rounded(raw);
        left_words = s.words.iter().filter(|w| w.midpoint() < cut).cloned().collect();
        right_words = s.words.iter().filter(|w| w.midpoint() >= cut).cloned().collect();
    }
    if left_text.is_empty() || right_text.is_empty() {
        return None;
    }
    let boundary = rounded(raw.max(lower).min(upper));
    Some(SplitPlan { boundary, left_text, right_text, left_words, right_words })
}

/// Engine-identical dash check (`- ` or `– ` prefix).
pub struct DialogueDash;

impl DialogueDash {
    pub fn has_dash(text: &str) -> bool {
        text.starts_with("- ") || text.starts_with("\u{2013} ")
    }
    pub fn apply(dash: bool, text: &str) -> String {
        if dash && !Self::has_dash(text) { format!("- {text}") } else { text.to_owned() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grapheme_length_counts_user_characters() {
        assert_eq!(grapheme_len("üç dört"), 7);
        assert_eq!(grapheme_len("e\u{301}"), 1);
        assert_eq!(grapheme_len("👍🏽"), 1);
        assert_eq!(grapheme_len("👨‍👩‍👧"), 1);
        assert_eq!(grapheme_len("🇹🇷🇺🇸"), 2);
        assert_eq!(grapheme_len(""), 0);
        // Hangul jamo sequences, CR LF and Prepend sequences are one cluster each.
        assert_eq!(grapheme_len("\u{1100}\u{1161}\u{11A8}"), 1);
        assert_eq!(grapheme_len("a\r\nb"), 3);
        assert_eq!(grapheme_len("\u{600}a"), 1);
        assert_eq!(grapheme_len("\u{D4E}\u{D15}"), 1);
        // Indic conjuncts: .NET (Windows) breaks after a virama, Swift (macOS) does not. The Windows values are what
        // the C# `StringInfo` returns.
        let conjuncts = ["– नमस्ते", "नमस्ते", "क्\u{200D}ष", "ക്ഷ", "ক্ষ"];
        let extended: Vec<usize> =
            conjuncts.iter().map(|t| unicode_segmentation::UnicodeSegmentation::graphemes(*t, true).count()).collect();
        assert_eq!(extended, [5, 3, 1, 1, 1]);
        let counts: Vec<usize> = conjuncts.iter().map(|t| grapheme_len(t)).collect();
        if cfg!(target_os = "macos") {
            assert_eq!(counts, [5, 3, 1, 1, 1]);
        } else {
            assert_eq!(counts, [6, 4, 2, 2, 2]);
        }
    }

    #[test]
    fn rounding_matches_swift_half_away_from_zero() {
        assert_eq!(rounded(1.2345), 1.235);
        assert_eq!(rounded(-1.2345), -1.235);
    }
}
