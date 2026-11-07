//! Subtitle formats (port of `subtitles.py`): SRT, VTT and JSON, with the speaker dash rule.

use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use se_core::json::{DumpOptions, dumps_with};
use se_core::types::sentences_to_value;
use se_core::{EngineError, ErrorKind, Num, Result, Sentence};

/// `FORMATS`, in CLI choice order.
pub const FORMATS: [&str; 3] = ["srt", "vtt", "json"];

const MS_PER_HOUR: u32 = 3_600_000;

/// `timestamp(seconds, sep)`: `HH:MM:SS<sep>mmm` with `ms = round_half_even(seconds * 1000)` and
/// floor-division semantics for negative values; hours are not limited to two digits. `sep` is `,` (SRT)
/// or `.` (VTT).
///
/// Panics on NaN/infinity (Python raises); [`render`] uses [`try_timestamp`], which returns the error.
pub fn timestamp(seconds: f64, sep: char) -> String {
    match try_timestamp(seconds, sep) {
        Ok(s) => s,
        Err(e) => panic!("{e}"),
    }
}

/// [`timestamp`] with Python's errors for non-finite input: NaN → `ValueError("cannot convert float NaN to
/// integer")`, ±inf → `OverflowError("cannot convert float infinity to integer")`.
///
/// Any finite value is exact like Python's arbitrary-precision ints (`1e300` seconds prints every hour digit).
pub fn try_timestamp(seconds: f64, sep: char) -> Result<String> {
    let ms = (seconds * 1000.0).round_ties_even();
    if ms.is_nan() {
        return Err(EngineError::value("cannot convert float NaN to integer"));
    }
    if ms.is_infinite() {
        return Err(EngineError::new(ErrorKind::Other, "cannot convert float infinity to integer"));
    }
    // h, rest = divmod(ms, 3_600_000) with Python floor semantics; rest is always 0..3_600_000.
    let (hours, rest) = if ms.abs() < 9.0e15 {
        let ms = ms as i64;
        (ms.div_euclid(MS_PER_HOUR as i64).to_string(), ms.rem_euclid(MS_PER_HOUR as i64) as u32)
    } else {
        big_hours(ms)
    };
    Ok(format_hms(hours, rest, sep))
}

/// [`try_timestamp`] for a [`Num`] time as read from JSON: an integer is exact like Python's
/// `round(int * 1000)` (no float rounding for values beyond 2^53), a float goes through [`try_timestamp`].
pub fn try_timestamp_num(seconds: Num, sep: char) -> Result<String> {
    match seconds {
        Num::Float(f) => try_timestamp(f, sep),
        Num::Int(i) => {
            let ms = i as i128 * 1000;
            let hours = ms.div_euclid(MS_PER_HOUR as i128).to_string();
            let rest = ms.rem_euclid(MS_PER_HOUR as i128) as u32;
            Ok(format_hms(hours, rest, sep))
        }
    }
}

/// `f"{h:02}:{m:02}:{s:02}{sep}{ms:03}"` from the hours (decimal text) and the remainder of `divmod`.
fn format_hms(hours: String, rest: u32, sep: char) -> String {
    let (m, rest) = (rest / 60_000, rest % 60_000);
    let (s, ms) = (rest / 1000, rest % 1000);
    // `{h:02}`: the sign counts toward the width, so a negative hour is never zero-padded further.
    let hours = if hours.len() < 2 { format!("0{hours}") } else { hours };
    format!("{hours}:{m:02}:{s:02}{sep}{ms:03}")
}

/// `divmod(ms, 3_600_000)` for an integral `ms` too large for exact `i64` arithmetic: the hours as a
/// decimal string (floor division) and the non-negative remainder.
fn big_hours(ms: f64) -> (String, u32) {
    let negative = ms < 0.0;
    // |ms| = mantissa * 2^exp exactly (|ms| >= 2^53 here, so exp >= 0).
    let bits = ms.abs().to_bits();
    let exp = ((bits >> 52) & 0x7ff) as i64 - 1075;
    let mantissa = (bits & ((1u64 << 52) - 1)) | (1u64 << 52);
    debug_assert!(exp >= 0);
    let mut limbs: Vec<u32> = vec![mantissa as u32, (mantissa >> 32) as u32];
    shift_left(&mut limbs, exp as u32);
    let mut rem = div_small(&mut limbs, MS_PER_HOUR);
    if negative && rem != 0 {
        add_one(&mut limbs);
        rem = MS_PER_HOUR - rem;
    }
    let mut digits = to_decimal(limbs);
    if negative {
        digits.insert(0, '-');
    }
    (digits, rem)
}

fn shift_left(limbs: &mut Vec<u32>, bits: u32) {
    let (words, bits) = ((bits / 32) as usize, bits % 32);
    if bits > 0 {
        let mut carry = 0u32;
        for limb in limbs.iter_mut() {
            let next = *limb >> (32 - bits);
            *limb = (*limb << bits) | carry;
            carry = next;
        }
        if carry != 0 {
            limbs.push(carry);
        }
    }
    limbs.splice(0..0, std::iter::repeat_n(0, words));
}

/// Divides in place, returns the remainder.
fn div_small(limbs: &mut [u32], divisor: u32) -> u32 {
    let mut rem = 0u64;
    for limb in limbs.iter_mut().rev() {
        let cur = (rem << 32) | *limb as u64;
        *limb = (cur / divisor as u64) as u32;
        rem = cur % divisor as u64;
    }
    rem as u32
}

fn add_one(limbs: &mut Vec<u32>) {
    for limb in limbs.iter_mut() {
        let (v, overflow) = limb.overflowing_add(1);
        *limb = v;
        if !overflow {
            return;
        }
    }
    limbs.push(1);
}

fn to_decimal(mut limbs: Vec<u32>) -> String {
    let mut chunks = Vec::new();
    while limbs.iter().any(|&l| l != 0) {
        chunks.push(div_small(&mut limbs, 1_000_000_000));
    }
    let Some((first, rest)) = chunks.split_last() else { return "0".into() };
    let mut out = first.to_string();
    for chunk in rest.iter().rev() {
        out.push_str(&format!("{chunk:09}"));
    }
    out
}

/// `speaker_dashes(sentences)`: per sentence, whether SRT/VTT prefix `- ` (dash rule: at least two distinct
/// non-null speakers; a sentence whose speaker differs from the previous known speaker; `None` speakers are
/// skipped and do not reset it; no dash if the text already starts with `- ` or `– `).
pub fn speaker_dashes(sentences: &[Sentence]) -> Vec<bool> {
    let mut known = sentences.iter().filter_map(|s| s.speaker);
    let distinct = match known.next() {
        Some(first) => known.any(|sp| sp != first),
        None => false,
    };
    if !distinct {
        return vec![false; sentences.len()];
    }
    let mut previous: Option<i64> = None; // last known speaker; sentences without one do not reset it
    sentences
        .iter()
        .map(|s| {
            let dash = s.speaker.is_some() && s.speaker != previous;
            if s.speaker.is_some() {
                previous = s.speaker;
            }
            dash
        })
        .collect()
}

/// `_has_dash(text)`: the (possibly user-edited) text already starts with a dialogue dash.
fn has_dash(text: &str) -> bool {
    text.starts_with("- ") || text.starts_with("\u{2013} ")
}

/// `_texts(sentences)`: the cue texts with the speaker dash applied.
fn texts(sentences: &[Sentence]) -> Vec<String> {
    sentences
        .iter()
        .zip(speaker_dashes(sentences))
        .map(|(s, dash)| if dash && !has_dash(&s.text) { format!("- {}", s.text) } else { s.text.clone() })
        .collect()
}

/// `render(sentences, fmt)`: `srt` / `vtt` (cues joined by `\n`, ending with one `\n`; empty → `""` /
/// `"WEBVTT\n\n"`) or `json` (`json.dumps(..., ensure_ascii=False, indent=2)`, no trailing newline, `[]` when
/// empty). Unknown `fmt` → localized `ValueError` `unknown_format`.
pub fn render(sentences: &[Sentence], fmt: &str) -> Result<String> {
    match fmt {
        "srt" => {
            let mut cues = Vec::with_capacity(sentences.len());
            for (i, (s, text)) in sentences.iter().zip(texts(sentences)).enumerate() {
                let start = try_timestamp_num(s.start, ',')?;
                let end = try_timestamp_num(s.end, ',')?;
                cues.push(format!("{}\n{start} --> {end}\n{text}\n", i + 1));
            }
            Ok(cues.join("\n"))
        }
        "vtt" => {
            let mut cues = Vec::with_capacity(sentences.len());
            for (s, text) in sentences.iter().zip(texts(sentences)) {
                let start = try_timestamp_num(s.start, '.')?;
                let end = try_timestamp_num(s.end, '.')?;
                cues.push(format!("{start} --> {end}\n{text}\n"));
            }
            Ok(format!("WEBVTT\n\n{}", cues.join("\n")))
        }
        "json" => Ok(dumps_with(
            &sentences_to_value(sentences),
            &DumpOptions { ensure_ascii: false, indent: Some(2), ..DumpOptions::default() },
        )),
        _ => Err(EngineError::value(se_core::t!("unknown_format", format = fmt))),
    }
}

/// `Path.mkdir(parents=True, exist_ok=True)`.
fn mkdir_parents(dir: &Path) -> Result<()> {
    let dir = if dir.as_os_str().is_empty() { Path::new(".") } else { dir };
    match fs::create_dir(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            match dir.parent() {
                Some(parent) if parent != dir => mkdir_parents(parent)?,
                _ => return Err(EngineError::win(&e, Some(dir))),
            }
            match fs::create_dir(dir) {
                Ok(()) => Ok(()),
                Err(_) if dir.is_dir() => Ok(()),
                Err(e) => Err(EngineError::win(&e, Some(dir))),
            }
        }
        Err(_) if dir.is_dir() => Ok(()),
        Err(e) => Err(EngineError::win(&e, Some(dir))),
    }
}

/// `write(sentences, path, fmt)`: creates parent folders, writes UTF-8 with LF, returns `path`.
/// I/O errors are `ErrorKind::Os` with Python's `[Errno N] ...: '<path>'` text (the pipeline falls back to a
/// pid-named file on any `Os` error).
///
/// Like Python, the file is created/truncated before rendering, so a render error leaves an empty file.
pub fn write(sentences: &[Sentence], path: &Path, fmt: &str) -> Result<PathBuf> {
    if let Some(parent) = path.parent() {
        mkdir_parents(parent)?;
    }
    let mut file = fs::File::create(path).map_err(|e| EngineError::os(&e, Some(path)))?;
    let text = render(sentences, fmt)?;
    file.write_all(text.as_bytes()).map_err(|e| EngineError::os(&e, None))?;
    file.flush().map_err(|e| EngineError::os(&e, None))?;
    Ok(path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_matches_python() {
        let cases = [
            (0.0005, "00:00:00,000"),
            (0.0015, "00:00:00,002"),
            (0.0025, "00:00:00,002"),
            (1.0005, "00:00:01,000"),
            (3599.9996, "01:00:00,000"),
            (359999.9995, "100:00:00,000"),
            (1e7, "2777:46:40,000"),
            (-0.0004, "00:00:00,000"),
            (-0.4, "-1:59:59,600"),
            (-1.5, "-1:59:58,500"),
            (3661.007, "01:01:01,007"),
            (-36000.0, "-10:00:00,000"),
        ];
        for (seconds, expected) in cases {
            assert_eq!(timestamp(seconds, ','), expected, "{seconds}");
        }
        assert_eq!(timestamp(1.5, '.'), "00:00:01.500");
    }

    #[test]
    fn huge_timestamps_are_exact() {
        // Expected values printed by the Python engine's `timestamp`.
        assert_eq!(timestamp(1e20, ','), "27777777777777775:26:51,392");
        assert_eq!(timestamp(-1e20, ','), "-27777777777777776:33:08,608");
        assert_eq!(timestamp(1e16, ','), "2777777777777:46:40,000");
        assert_eq!(timestamp(-9.1e12, ','), "-2527777778:13:20,000");
        assert_eq!(timestamp(9.007199254740992e12, ','), "2501999792:59:00,992");
        let huge = timestamp(1e300, ',');
        assert!(huge.starts_with("2777777777777777778227125213295712328368524017310165106376375047503104164515940966"));
        assert!(huge.ends_with("8425182074699756184:14:37,568"));
        assert_eq!(huge.len(), 307);
    }

    #[test]
    fn integer_timestamps_are_exact() {
        // Expected values printed by the Python engine's `timestamp` with int seconds.
        let cases = [
            (9_007_199_254_740_993, "2501999792983:36:33,000"),
            (-9_007_199_254_740_993, "-2501999792984:23:27,000"),
            (i64::MAX, "2562047788015215:30:07,000"),
            (i64::MIN, "-2562047788015216:29:52,000"),
            (-1, "-1:59:59,000"),
            (0, "00:00:00,000"),
            (1, "00:00:01,000"),
        ];
        for (seconds, expected) in cases {
            assert_eq!(try_timestamp_num(Num::Int(seconds), ',').unwrap(), expected, "{seconds}");
        }
        assert_eq!(try_timestamp_num(Num::Float(1.5), '.').unwrap(), "00:00:01.500");
    }

    #[test]
    fn non_finite_timestamps_are_errors() {
        assert_eq!(try_timestamp(f64::NAN, ',').unwrap_err().message, "cannot convert float NaN to integer");
        assert_eq!(
            try_timestamp(f64::NEG_INFINITY, ',').unwrap_err().message,
            "cannot convert float infinity to integer"
        );
    }
}
