//! Diarization post-processing (spec diarize.md §4-§7): speech compaction, centroids, auto-mode merge,
//! noise filter and the acoustic review of short turns.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};

use se_core::Word;
use se_core::pyfmt::round_ndigits;

use crate::assign::{SegmentIndex, cmp_f64, cmp_start_end, is_final, is_question, py_max, py_min};
use crate::{
    CENTROID_MAX_SECONDS, CENTROID_MAX_SEGMENTS, CENTROID_SEGMENT_SECONDS, COMPACT_MAX_RATIO,
    COMPACT_MIN_DURATION, MIN_SPEAKER_SECONDS, SAMPLE_RATE, SPEECH_MERGE_GAP, SPEECH_PAD_SECONDS,
    SPEECH_SEPARATOR_SECONDS, SpeakerSegment, VERIFY_MAX_TURNS, VERIFY_MIN_SIMILARITY,
};

/// Cache of unit embedding vectors keyed by `(lo, hi)` sample indices, shared by merge and review.
pub type SliceCache = HashMap<(usize, usize), Vec<f64>>;

/// An embedding function: samples → embedding (CAM++: 192 floats; empty when sherpa returns nothing).
pub type EmbedFn<'a> = &'a mut dyn FnMut(&[f32]) -> Vec<f64>;

/// A centroid provider (`cluster_centroids` signature without the optional cache); the seam the Python
/// tests monkeypatch inside `verify_short_turns`.
pub type CentroidFn<'a> =
    &'a mut dyn FnMut(&[SpeakerSegment], &[f32], EmbedFn<'_>, &mut SliceCache) -> BTreeMap<i64, Vec<f64>>;

const RATE: f64 = SAMPLE_RATE as f64;

/// `_SpeechSpan`: a kept speech interval in original and compacted time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpeechSpan {
    pub orig_start: f64,
    pub orig_end: f64,
    pub comp_start: f64,
    pub comp_end: f64,
}

/// `round(x, 3)` (correctly rounded, ties to even on the exact binary value).
pub(crate) fn round3(x: f64) -> f64 {
    round_ndigits(x, 3)
}

/// Python `int(x)` for a float: truncation toward zero.
fn py_int(x: f64) -> i64 {
    x.trunc() as i64
}

/// Python `sum(...)` of floats: sequential left-to-right.
fn seq_sum(values: impl Iterator<Item = f64>) -> f64 {
    let mut total = 0.0;
    for v in values {
        total += v;
    }
    total
}

type PowFn = unsafe extern "C" fn(f64, f64) -> f64;

#[cfg(not(windows))]
unsafe extern "C" {
    /// The C library's `pow` (what CPython's `float ** float` calls).
    #[link_name = "pow"]
    fn libm_pow(x: f64, y: f64) -> f64;
}

/// The `pow` CPython uses: on Windows the one exported by the shared `ucrtbase.dll` (this crate links the
/// static CRT, whose `pow` rounds differently on some inputs), elsewhere the C library's.
fn crt_pow() -> PowFn {
    #[cfg(windows)]
    {
        static POW: std::sync::OnceLock<Option<PowFn>> = std::sync::OnceLock::new();
        let found = *POW.get_or_init(|| {
            // SAFETY: ucrtbase.dll is the system C runtime; `pow` has the C signature double(double, double).
            unsafe {
                let lib = libloading::Library::new("ucrtbase.dll").ok()?;
                let f: PowFn = *lib.get::<PowFn>(b"pow\0").ok()?;
                std::mem::forget(lib); // keep it loaded for the process lifetime
                Some(f)
            }
        });
        if let Some(f) = found {
            return f;
        }
        unsafe extern "C" fn fallback(x: f64, y: f64) -> f64 {
            x.powf(y)
        }
        fallback
    }
    #[cfg(not(windows))]
    {
        std::hint::black_box(libm_pow as PowFn)
    }
}

/// Python `x ** 0.5` for a non-negative float: the C runtime's `pow(x, 0.5)`, which is NOT always the
/// correctly rounded `sqrt` (the Windows UCRT differs by 1 ulp for ~0.05% of inputs). Rust's `powf`
/// would be folded into `sqrt` by LLVM, so the call goes through an opaque function pointer.
pub(crate) fn py_pow_half(x: f64) -> f64 {
    if x == 0.0 || x.is_nan() || x.is_infinite() {
        return x.powf(0.5);
    }
    // SAFETY: `pow` is a pure C function on two doubles.
    unsafe { crt_pow()(x, 0.5) }
}

/// `_unit(vector)`: `x / norm` for each element when `norm > 0` (`norm = sum(x*x) ** 0.5`), else unchanged.
pub(crate) fn unit(vector: &[f64]) -> Vec<f64> {
    let norm = py_pow_half(seq_sum(vector.iter().map(|x| x * x)));
    if norm > 0.0 { vector.iter().map(|x| x / norm).collect() } else { vector.to_vec() }
}

/// `_cosine(a, b)`: dot product of the unit vectors (`zip` truncates to the shorter one).
pub(crate) fn cosine(a: &[f64], b: &[f64]) -> f64 {
    let ua = unit(a);
    let ub = unit(b);
    seq_sum(ua.iter().zip(ub.iter()).map(|(x, y)| x * y))
}

/// Merges `(lo, hi)` into the interval list (`lo - last.hi <= SPEECH_MERGE_GAP` extends the last one).
fn push_interval(intervals: &mut Vec<(f64, f64)>, lo: f64, hi: f64) {
    if let Some(last) = intervals.last_mut()
        && lo - last.1 <= SPEECH_MERGE_GAP
    {
        last.1 = py_max(last.1, hi);
        return;
    }
    intervals.push((lo, hi));
}

/// numpy's float32 pairwise summation (`FLOAT_pairwise_sum`, block size 128, 8 accumulators).
fn pairwise_sum_f32(a: &[f32]) -> f32 {
    let n = a.len();
    if n < 8 {
        let mut res = 0.0f32;
        for &x in a {
            res += x;
        }
        res
    } else if n <= 128 {
        let mut r = [0.0f32; 8];
        r.copy_from_slice(&a[..8]);
        let mut i = 8;
        while i < n - (n % 8) {
            for j in 0..8 {
                r[j] += a[i + j];
            }
            i += 8;
        }
        let mut res = ((r[0] + r[1]) + (r[2] + r[3])) + ((r[4] + r[5]) + (r[6] + r[7]));
        while i < n {
            res += a[i];
            i += 1;
        }
        res
    } else {
        let mut n2 = n / 2;
        n2 -= n2 % 8;
        pairwise_sum_f32(&a[..n2]) + pairwise_sum_f32(&a[n2..])
    }
}

/// `np.sqrt(np.mean(frames * frames, axis=1))` over 100 ms frames, in float32.
fn frame_rms(samples: &[f32], frame_len: usize, n_frames: usize) -> Vec<f32> {
    let mut squares = vec![0.0f32; frame_len];
    (0..n_frames)
        .map(|k| {
            let frame = &samples[k * frame_len..(k + 1) * frame_len];
            for (dst, &x) in squares.iter_mut().zip(frame) {
                *dst = x * x;
            }
            let mean = pairwise_sum_f32(&squares) / frame_len as f32;
            mean.sqrt()
        })
        .collect()
}

/// `float(np.percentile(values, q))` with numpy's default "linear" method on a float32 array.
fn percentile_f32(values: &[f32], q: f64) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = sorted.len();
    let virtual_index = (n - 1) as f64 * (q / 100.0);
    let prev = virtual_index.floor();
    let prev_i = (prev as usize).min(n - 1);
    let next_i = (prev_i + 1).min(n - 1);
    let gamma = (virtual_index - prev) as f32;
    let a = sorted[prev_i];
    let b = sorted[next_i];
    let diff = b - a;
    let value = if gamma >= 0.5 { b - diff * (1.0 - gamma) } else { a + diff * gamma };
    value as f64
}

/// `compact_speech_samples(samples, words)`: when non-speech exceeds 15% of a ≥10 s recording, only the
/// speech intervals (ASR words, else RMS energy windows) joined by short separators; `None` spans = unchanged
/// (the samples are then borrowed, not copied).
pub fn compact_speech_samples<'a>(
    samples: &'a [f32],
    words: Option<&[Word]>,
) -> (Cow<'a, [f32]>, Option<Vec<SpeechSpan>>) {
    let unchanged = || (Cow::Borrowed(samples), None);
    let duration = samples.len() as f64 / RATE;
    if duration < COMPACT_MIN_DURATION {
        return unchanged();
    }

    let mut intervals: Vec<(f64, f64)> = Vec::new();
    match words {
        Some(words) if !words.is_empty() => {
            for w in words {
                let lo = py_max(0.0, w.start_f() - SPEECH_PAD_SECONDS);
                let hi = py_min(duration, w.end_f() + SPEECH_PAD_SECONDS);
                if hi <= lo {
                    continue;
                }
                push_interval(&mut intervals, lo, hi);
            }
        }
        _ => {
            let frame_len = (SAMPLE_RATE / 10) as usize;
            let n_frames = samples.len() / frame_len;
            if n_frames > 0 {
                let rms = frame_rms(samples, frame_len, n_frames);
                let floor = percentile_f32(&rms, 15.0);
                let thresh = py_max(0.002, py_min(0.015, floor * 2.5));
                // NEP 50: `float32 >= python float` compares in float32.
                let thresh32 = thresh as f32;
                for (idx, &val) in rms.iter().enumerate() {
                    if val >= thresh32 {
                        let lo = py_max(0.0, idx as f64 * 0.1 - SPEECH_PAD_SECONDS);
                        let hi = py_min(duration, (idx + 1) as f64 * 0.1 + SPEECH_PAD_SECONDS);
                        push_interval(&mut intervals, lo, hi);
                    }
                }
            }
        }
    }

    if intervals.is_empty() {
        return unchanged();
    }
    let speech_seconds = seq_sum(intervals.iter().map(|(lo, hi)| hi - lo));
    if speech_seconds >= duration * COMPACT_MAX_RATIO || speech_seconds < 1.0 {
        return unchanged();
    }

    let sep_len = py_int(SPEECH_SEPARATOR_SECONDS * RATE) as usize;
    let mut out: Vec<f32> = Vec::new();
    let mut spans: Vec<SpeechSpan> = Vec::new();
    let mut cursor = 0.0f64;
    for &(orig_lo, orig_hi) in &intervals {
        let s_idx = ((orig_lo * RATE).round_ties_even() as i64).max(0);
        let e_idx = ((orig_hi * RATE).round_ties_even() as i64).min(samples.len() as i64);
        if e_idx <= s_idx {
            continue;
        }
        if !spans.is_empty() {
            out.extend(std::iter::repeat_n(0.0f32, sep_len));
            cursor += sep_len as f64 / RATE;
        }
        let chunk = &samples[s_idx as usize..e_idx as usize];
        let comp_lo = cursor;
        let comp_hi = cursor + chunk.len() as f64 / RATE;
        spans.push(SpeechSpan {
            orig_start: s_idx as f64 / RATE,
            orig_end: e_idx as f64 / RATE,
            comp_start: comp_lo,
            comp_end: comp_hi,
        });
        out.extend_from_slice(chunk);
        cursor = comp_hi;
    }
    if spans.is_empty() {
        return unchanged();
    }
    (Cow::Owned(out), Some(spans))
}

/// `unmap_compacted_segments(segments, spans)`: back to the original timeline.
pub fn unmap_compacted_segments(segments: &[SpeakerSegment], spans: &[SpeechSpan]) -> Vec<SpeakerSegment> {
    let mut mapped = Vec::new();
    for seg in segments {
        for span in spans {
            if span.comp_end <= seg.start {
                continue;
            }
            if span.comp_start >= seg.end {
                break;
            }
            let ov_s = py_max(seg.start, span.comp_start);
            let ov_e = py_min(seg.end, span.comp_end);
            if ov_e - ov_s >= 0.04 {
                let orig_s = round3(span.orig_start + (ov_s - span.comp_start));
                let orig_e = round3(span.orig_start + (ov_e - span.comp_start));
                if orig_e > orig_s {
                    mapped.push(SpeakerSegment { start: orig_s, end: orig_e, speaker: seg.speaker });
                }
            }
        }
    }
    mapped
}

/// `cluster_centroids(segments, samples, embed, slice_cache)`: unit centroid per speaker from a bounded
/// sample (longest segments, at most 10 / 30 s, each cropped to its middle 6 s). Audio where another
/// speaker overlaps is cut out of each reference piece; speakers without a usable piece get no entry.
///
/// Embedding order: speakers by first appearance in `segments`, within a speaker by descending clean-piece
/// length (stable).
pub fn cluster_centroids(
    segments: &[SpeakerSegment],
    samples: &[f32],
    embed: EmbedFn<'_>,
    slice_cache: Option<&mut SliceCache>,
) -> BTreeMap<i64, Vec<f64>> {
    let mut local = SliceCache::new();
    let cache = slice_cache.unwrap_or(&mut local);
    let index = SegmentIndex::new(segments);
    let mut by_speaker: Vec<(i64, Vec<SpeakerSegment>)> = Vec::new();
    for s in segments {
        let (lo, hi) = index.window(s.start, s.end);
        let mut cursor = s.start;
        let slot = match by_speaker.iter().position(|(spk, _)| *spk == s.speaker) {
            Some(p) => p,
            None => {
                by_speaker.push((s.speaker, Vec::new()));
                by_speaker.len() - 1
            }
        };
        let clean = &mut by_speaker[slot].1;
        for other in &index.segments[lo..hi] {
            if other.speaker == s.speaker || other.end <= cursor {
                continue;
            }
            if other.start > cursor {
                clean.push(SpeakerSegment { start: cursor, end: py_min(other.start, s.end), speaker: s.speaker });
            }
            cursor = py_max(cursor, other.end);
            if cursor >= s.end {
                break;
            }
        }
        if cursor < s.end {
            clean.push(SpeakerSegment { start: cursor, end: s.end, speaker: s.speaker });
        }
    }

    let n_samples = samples.len() as i64;
    let min_samples = py_int(0.3 * RATE);
    let mut centroids = BTreeMap::new();
    for (speaker, mut group) in by_speaker {
        let mut total = 0.0f64;
        let mut vectors: Vec<Vec<f64>> = Vec::new();
        let mut weights: Vec<f64> = Vec::new();
        group.sort_by(|a, b| cmp_f64(b.end - b.start, a.end - a.start));
        for s in group.iter().take(CENTROID_MAX_SEGMENTS) {
            let length = py_min(py_min(s.end - s.start, CENTROID_SEGMENT_SECONDS), CENTROID_MAX_SECONDS - total);
            if length < 0.3 {
                continue;
            }
            let middle = (s.start + s.end) / 2.0;
            let lo = py_int((middle - length / 2.0) * RATE).max(0);
            let hi = n_samples.min(lo + py_int(length * RATE));
            if hi - lo < min_samples {
                continue;
            }
            let key = (lo as usize, hi as usize);
            let unit_vec = match cache.get(&key) {
                Some(v) => v.clone(),
                None => {
                    let v = unit(&embed(&samples[key.0..key.1]));
                    cache.insert(key, v.clone());
                    v
                }
            };
            vectors.push(unit_vec);
            weights.push(length);
            total += length;
        }
        if let Some(first) = vectors.first() {
            let dim = first.len();
            let summed: Vec<f64> = (0..dim)
                .map(|i| seq_sum(weights.iter().zip(&vectors).map(|(w, v)| w * v.get(i).copied().unwrap_or(0.0))))
                .collect();
            centroids.insert(speaker, unit(&summed));
        }
    }
    centroids
}

/// `_simultaneous_speakers(segments)`: speaker pairs `(min, max)` that coexist for at least 100 ms.
pub(crate) fn simultaneous_speakers(segments: &[SpeakerSegment]) -> HashSet<(i64, i64)> {
    let mut ordered = segments.to_vec();
    ordered.sort_by(|a, b| cmp_f64(a.start, b.start));
    let mut active: Vec<SpeakerSegment> = Vec::new();
    let mut pairs = HashSet::new();
    for current in ordered {
        active.retain(|s| s.end > current.start);
        for other in &active {
            if other.speaker != current.speaker && py_min(other.end, current.end) - current.start >= 0.1 {
                pairs.insert((other.speaker.min(current.speaker), other.speaker.max(current.speaker)));
            }
        }
        active.push(current);
    }
    pairs
}

fn distinct_speakers(segments: &[SpeakerSegment]) -> usize {
    segments.iter().map(|s| s.speaker).collect::<HashSet<_>>().len()
}

/// `merge_similar_speakers(segments, embed, samples, threshold, slice_cache)`: merges clusters with centroid
/// cosine ≥ `threshold` (never two that overlap ≥ 100 ms), repeating until stable. Labels are not renumbered.
pub fn merge_similar_speakers(
    segments: &[SpeakerSegment],
    embed: EmbedFn<'_>,
    samples: &[f32],
    threshold: f64,
    slice_cache: Option<&mut SliceCache>,
) -> Vec<SpeakerSegment> {
    let mut segments = segments.to_vec();
    if distinct_speakers(&segments) < 2 {
        return segments;
    }
    let mut local = SliceCache::new();
    let cache = slice_cache.unwrap_or(&mut local);
    let mut centroids = cluster_centroids(&segments, samples, &mut *embed, Some(&mut *cache));
    while centroids.len() > 1 {
        let keys: Vec<i64> = centroids.keys().copied().collect();
        let simultaneous = simultaneous_speakers(&segments);
        // Python `max((cos, a, b))`: ties on cosine go to the larger a, then the larger b.
        let mut best: Option<(f64, i64, i64)> = None;
        for (i, &a) in keys.iter().enumerate() {
            for &b in &keys[i + 1..] {
                if simultaneous.contains(&(a, b)) {
                    continue;
                }
                let cand = (cosine(&centroids[&a], &centroids[&b]), a, b);
                let better = match best {
                    None => true,
                    Some(cur) => {
                        cand.0 > cur.0 || (cand.0 == cur.0 && (cand.1 > cur.1 || (cand.1 == cur.1 && cand.2 > cur.2)))
                    }
                };
                if better {
                    best = Some(cand);
                }
            }
        }
        let Some((similarity, keep, drop)) = best else { break };
        if similarity < threshold {
            break;
        }
        for s in &mut segments {
            if s.speaker == drop {
                s.speaker = keep;
            }
        }
        centroids = cluster_centroids(&segments, samples, &mut *embed, Some(&mut *cache));
    }
    segments
}

/// `_short_reply_speakers(words, segments)`: speakers of a short, complete ASR reply (1-3 words,
/// 0.25-1.8 s) substantially covered (≥ 75 %) by that one diarized speaker.
pub(crate) fn short_reply_speakers(words: &[Word], segments: &[SpeakerSegment]) -> HashSet<i64> {
    let mut protected = HashSet::new();
    if words.is_empty() || segments.is_empty() {
        return protected;
    }
    let index = SegmentIndex::new(segments);
    let mut start = 0usize;
    for (end, word) in words.iter().enumerate() {
        if !is_final(word) {
            continue;
        }
        let phrase = &words[start..=end];
        start = end + 1;
        let span = phrase[phrase.len() - 1].end_f() - phrase[0].start_f();
        if !((1..=3).contains(&phrase.len()) && 0.25 <= span && span <= 1.8) {
            continue;
        }
        let speaker = index.speaker_for(&phrase[0]);
        if phrase.iter().any(|w| index.speaker_for(w) != speaker) {
            continue;
        }
        let speech = seq_sum(phrase.iter().map(|w| py_max(0.0, w.end_f() - w.start_f())));
        let mut covered = 0.0f64;
        for w in phrase {
            let (ws, we) = (w.start_f(), w.end_f());
            let (lo, hi) = index.window(ws, we);
            let by_speaker = seq_sum(
                index.segments[lo..hi]
                    .iter()
                    .filter(|s| s.speaker == speaker)
                    .map(|s| py_max(0.0, py_min(we, s.end) - py_max(ws, s.start))),
            );
            covered += py_min(py_max(0.0, we - ws), by_speaker);
        }
        if speech >= 0.25 && covered >= speech * 0.75 {
            protected.insert(speaker);
        }
    }
    protected
}

/// `normalize(segments, drop_noise, words)`: sort, drop speakers under 1 s unless a short complete reply
/// supports them (only with `drop_noise`), renumber by first appearance.
pub fn normalize(segments: &[SpeakerSegment], drop_noise: bool, words: Option<&[Word]>) -> Vec<SpeakerSegment> {
    let mut ordered = segments.to_vec();
    ordered.sort_by(cmp_start_end);
    if drop_noise {
        let mut totals: Vec<(i64, f64)> = Vec::new();
        for s in &ordered {
            match totals.iter_mut().find(|(spk, _)| *spk == s.speaker) {
                Some((_, t)) => *t += s.end - s.start,
                None => totals.push((s.speaker, 0.0 + (s.end - s.start))),
            }
        }
        if totals.len() > 1 {
            let mut keep: HashSet<i64> =
                totals.iter().filter(|(_, v)| *v >= MIN_SPEAKER_SECONDS).map(|(k, _)| *k).collect();
            keep.extend(short_reply_speakers(words.unwrap_or(&[]), &ordered));
            if !keep.is_empty() {
                ordered.retain(|s| keep.contains(&s.speaker));
            }
        }
    }
    let mut order: Vec<i64> = Vec::new();
    ordered
        .iter()
        .map(|s| {
            let id = match order.iter().position(|&o| o == s.speaker) {
                Some(p) => p,
                None => {
                    order.push(s.speaker);
                    order.len() - 1
                }
            };
            SpeakerSegment { start: s.start, end: s.end, speaker: id as i64 }
        })
        .collect()
}

/// `verify_short_turns(segments, words, samples, embed, slice_cache)`: bounded acoustic check of short
/// replies and isolated A→B→A switches against the speaker centroids (≤ 32 candidates; cosine ≥ 0.65 and a
/// lead ≥ 0.06 after a question, else ≥ 0.10).
pub fn verify_short_turns(
    segments: &[SpeakerSegment],
    words: &[Word],
    samples: &[f32],
    embed: EmbedFn<'_>,
    slice_cache: Option<&mut SliceCache>,
) -> Vec<SpeakerSegment> {
    let mut centroids = |segs: &[SpeakerSegment], samples: &[f32], embed: EmbedFn<'_>, cache: &mut SliceCache| {
        cluster_centroids(segs, samples, embed, Some(cache))
    };
    verify_short_turns_with(segments, words, samples, embed, slice_cache, &mut centroids)
}

/// A review candidate: word range, whether the previous sentence was a question, and the surrounding
/// speaker of an isolated A→B→A switch.
#[derive(Clone, Copy)]
struct Candidate {
    w_lo: usize,
    w_hi: usize,
    prev_question: bool,
    surrounding: Option<i64>,
}

/// [`verify_short_turns`] with an injectable centroid provider (test seam for Python's monkeypatched
/// `cluster_centroids`). The provider is called at most once, and only when there are candidates.
pub fn verify_short_turns_with(
    segments: &[SpeakerSegment],
    words: &[Word],
    samples: &[f32],
    embed: EmbedFn<'_>,
    slice_cache: Option<&mut SliceCache>,
    centroid_fn: CentroidFn<'_>,
) -> Vec<SpeakerSegment> {
    if segments.is_empty() || words.is_empty() || samples.len() < SAMPLE_RATE as usize {
        return segments.to_vec();
    }
    if distinct_speakers(segments) < 2 {
        return segments.to_vec();
    }

    let index = SegmentIndex::new(segments);
    let word_speakers: Vec<i64> = words.iter().map(|w| index.speaker_for(w)).collect();
    let n = words.len();

    let mut sentences: Vec<(usize, usize)> = Vec::new();
    let mut start_idx = 0usize;
    for (i, w) in words.iter().enumerate() {
        if is_final(w) || i == n - 1 {
            sentences.push((start_idx, i + 1));
            start_idx = i + 1;
        }
    }

    let mut candidates: Vec<Candidate> = Vec::new();
    for (s_idx, &(w_lo, w_hi)) in sentences.iter().enumerate() {
        if w_hi - w_lo > 3 {
            continue;
        }
        let t_start = words[w_lo].start_f();
        let t_end = words[w_hi - 1].end_f();
        let dur = t_end - t_start;
        if !(0.35 <= dur && dur <= 1.8) {
            continue;
        }
        let prev_question = s_idx > 0 && is_question(&words[sentences[s_idx - 1].1 - 1]);
        let prev_gap = if w_lo > 0 { t_start - words[w_lo - 1].end_f() } else { 1.0 };
        let cur = word_speakers[w_lo];
        if word_speakers[w_lo..w_hi].iter().any(|&spk| spk != cur) {
            continue;
        }
        let prev_speaker = if w_lo > 0 { Some(word_speakers[w_lo - 1]) } else { None };
        let next_speaker = if w_hi < n { Some(word_speakers[w_hi]) } else { None };
        let next_gap = if w_hi < n { words[w_hi].start_f() - t_end } else { 1.0 };
        if prev_gap < 0.0 || next_gap < 0.0 {
            continue;
        }
        let (lo, hi) = index.window(t_start, t_end);
        if index.segments[lo..hi].iter().any(|s| s.speaker != cur && s.end > t_start) {
            continue;
        }
        let island = prev_speaker.is_some()
            && prev_speaker == next_speaker
            && Some(cur) != prev_speaker
            && py_max(prev_gap, next_gap) <= 0.25;
        let glued_reply =
            (Some(cur) == prev_speaker || Some(cur) == next_speaker) && (prev_question || prev_gap >= 0.25);
        if !island && !glued_reply {
            continue;
        }
        candidates.push(Candidate { w_lo, w_hi, prev_question, surrounding: if island { prev_speaker } else { None } });
    }

    if candidates.is_empty() {
        return segments.to_vec();
    }
    if candidates.len() > VERIFY_MAX_TURNS {
        let len = candidates.len();
        candidates = (0..VERIFY_MAX_TURNS).map(|i| candidates[i * (len - 1) / (VERIFY_MAX_TURNS - 1)]).collect();
    }
    let mut local = SliceCache::new();
    let cache = slice_cache.unwrap_or(&mut local);
    let centroids = centroid_fn(segments, samples, &mut *embed, cache);
    if centroids.len() < 2 {
        return segments.to_vec();
    }

    let n_samples = samples.len() as i64;
    let min_samples = py_int(0.35 * RATE);
    let mut overrides: Vec<SpeakerSegment> = Vec::new();
    for c in &candidates {
        let t_start = words[c.w_lo].start_f();
        let t_end = words[c.w_hi - 1].end_f();
        let cur = word_speakers[c.w_lo];
        if !centroids.contains_key(&cur) {
            continue;
        }
        let lo_samp = py_int((t_start - 0.05) * RATE).max(0);
        let hi_samp = n_samples.min(py_int((t_end + 0.05) * RATE));
        if hi_samp - lo_samp < min_samples {
            continue;
        }
        let vec = unit(&embed(&samples[lo_samp as usize..hi_samp as usize]));
        let mut ranked: Vec<(f64, i64)> = centroids.iter().map(|(&spk, cen)| (cosine(&vec, cen), spk)).collect();
        // sorted(..., reverse=True) on (cosine, speaker) tuples.
        ranked.sort_by(|a, b| cmp_f64(b.0, a.0).then_with(|| b.1.cmp(&a.1)));
        let (best_sim, best_spk) = ranked[0];
        let margin = if c.prev_question { 0.06 } else { 0.10 };
        if best_spk != cur
            && best_sim >= VERIFY_MIN_SIMILARITY
            && best_sim - ranked[1].0 >= margin
            && c.surrounding.is_none_or(|s| best_spk == s)
        {
            overrides.push(SpeakerSegment { start: round3(t_start), end: round3(t_end), speaker: best_spk });
        }
    }

    if overrides.is_empty() {
        return segments.to_vec();
    }
    let mut updated = segments.to_vec();
    for ov in overrides {
        let mut next_list = Vec::with_capacity(updated.len() + 2);
        for seg in &updated {
            if seg.end <= ov.start || seg.start >= ov.end {
                next_list.push(*seg);
            } else {
                if seg.start < ov.start {
                    next_list.push(SpeakerSegment { start: seg.start, end: ov.start, speaker: seg.speaker });
                }
                if seg.end > ov.end {
                    next_list.push(SpeakerSegment { start: ov.end, end: seg.end, speaker: seg.speaker });
                }
            }
        }
        next_list.push(ov);
        next_list.sort_by(cmp_start_end);
        updated = next_list;
    }
    updated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round3_ties_to_even_on_exact_value() {
        assert_eq!(round3(0.0625), 0.062);
        assert_eq!(round3(1.0625), 1.062);
        assert_eq!(round3(0.1875), 0.188);
        assert_eq!(round3(2.0005), 2.001); // 2.0005 is slightly above the tie in binary (Python: 2.001)
        assert_eq!(round3(1.2345678), 1.235);
    }

    #[test]
    fn unit_and_cosine_edge_cases() {
        assert_eq!(unit(&[0.0, 0.0]), vec![0.0, 0.0]);
        assert!(unit(&[]).is_empty());
        assert_eq!(cosine(&[0.0, 0.0, 0.0], &[1.0, 0.0, 0.0]), 0.0);
        assert!((cosine(&[1.0, 0.0], &[1.0, 0.0, 5.0]) - 1.0 / 26f64.sqrt()).abs() < 1e-15); // zip truncates
        assert_eq!(cosine(&[3.0, 4.0], &[3.0, 4.0]), 1.0);
    }

    #[cfg(windows)]
    #[test]
    fn pow_half_matches_cpython_on_windows() {
        // CPython 3.11 (UCRT) `x ** 0.5` vs the correctly rounded `math.sqrt(x)`: they differ here.
        for (x, pow, sqrt) in [
            (0x3FEFB9C2139CA6F8u64, 0x3FEFDCCDAE9DC076u64, 0x3FEFDCCDAE9DC075u64),
            (0x3FE7174C4F414E7C, 0x3FEB2ED5323B8D2E, 0x3FEB2ED5323B8D2F),
            (0x3FDB662EC7E4EC9A, 0x3FE4F00BC2CFF6A0, 0x3FE4F00BC2CFF6A1),
        ] {
            let x = f64::from_bits(x);
            assert_eq!(py_pow_half(x).to_bits(), pow);
            assert_eq!(x.sqrt().to_bits(), sqrt);
        }
        assert_eq!(py_pow_half(0.0), 0.0);
        assert_eq!(py_pow_half(4.0), 2.0);
    }

    #[test]
    fn pairwise_sum_matches_simple_sum_for_small_inputs() {
        assert_eq!(pairwise_sum_f32(&[1.0, 2.0, 3.0]), 6.0);
        let v: Vec<f32> = (0..1600).map(|i| i as f32).collect();
        assert_eq!(pairwise_sum_f32(&v), 1599.0 * 1600.0 / 2.0);
    }

    #[test]
    fn percentile_linear() {
        assert_eq!(percentile_f32(&[1.0, 2.0, 3.0, 4.0, 5.0], 50.0), 3.0);
        assert!((percentile_f32(&[0.0, 10.0], 15.0) - 1.5).abs() < 1e-6);
        assert_eq!(percentile_f32(&[7.0], 15.0), 7.0);
    }

    #[test]
    fn centroid_cap_counts_skipped_pieces() {
        // 10 tiny pieces (skipped: < 0.3 s) used up the [:10] budget before the long one.
        let mut segs: Vec<SpeakerSegment> =
            (0..10).map(|i| SpeakerSegment { start: i as f64, end: i as f64 + 0.2, speaker: 0 }).collect();
        segs.push(SpeakerSegment { start: 20.0, end: 20.1, speaker: 0 });
        let samples = vec![0.0f32; 30 * 16000];
        let mut calls = 0;
        let mut embed = |_: &[f32]| {
            calls += 1;
            vec![1.0]
        };
        assert!(cluster_centroids(&segs, &samples, &mut embed, None).is_empty());
        assert_eq!(calls, 0);
        segs.push(SpeakerSegment { start: 25.0, end: 27.0, speaker: 0 });
        let mut calls = 0;
        let mut embed = |_: &[f32]| {
            calls += 1;
            vec![1.0]
        };
        // The 2 s piece sorts first, so it is inside the first ten.
        assert_eq!(cluster_centroids(&segs, &samples, &mut embed, None).len(), 1);
        assert_eq!(calls, 1);
    }

    #[test]
    fn merge_tie_break_prefers_larger_ids() {
        // Three identical voices, no overlaps: the first merge is (1, 2) → 2 becomes 1, then (0, 1).
        let segs = [
            SpeakerSegment { start: 0.0, end: 2.0, speaker: 0 },
            SpeakerSegment { start: 2.0, end: 4.0, speaker: 1 },
            SpeakerSegment { start: 4.0, end: 6.0, speaker: 2 },
        ];
        let samples = vec![1.0f32; 6 * 16000];
        let mut embed = |_: &[f32]| vec![1.0, 0.0];
        let merged = merge_similar_speakers(&segs, &mut embed, &samples, 0.65, None);
        assert!(merged.iter().all(|s| s.speaker == 0));
        // With an overlap between 0 and 2 (and thus 0 and 1 after the first merge) only (1, 2) merges.
        let segs = [
            SpeakerSegment { start: 0.0, end: 2.0, speaker: 0 },
            SpeakerSegment { start: 3.0, end: 5.0, speaker: 1 },
            SpeakerSegment { start: 1.0, end: 3.0, speaker: 2 },
        ];
        let merged = merge_similar_speakers(&segs, &mut embed, &samples, 0.65, None);
        assert_eq!(merged.iter().map(|s| s.speaker).collect::<Vec<_>>(), vec![0, 1, 1]);
    }
}
