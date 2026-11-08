//! .NET-compatible text helpers shared by the Windows stores and the view models.

use crate::l10n::UiLanguage;

#[path = "text_upper_table.rs"]
mod upper_table;

/// .NET `ToUpperInvariant`: Unicode **simple** uppercase mapping per code point (`ß` stays, `ı` stays, `i` → `I`,
/// `ſ` → `S`). Used for OrdinalIgnoreCase comparisons, mutex names, list-item HMAC input and queue keys
/// (win-profiles §3, win-media §0.2). Rust's `to_uppercase` (full mapping) differs and must not be used for these.
/// The table is the exact .NET 10 output (`tests/data/win-media-upper-invariant.txt`).
pub fn upper_invariant(s: &str) -> String {
    if s.is_ascii() {
        return s.to_ascii_uppercase();
    }
    s.chars().map(upper_invariant_char).collect()
}

/// [`upper_invariant`] of one code point.
pub fn upper_invariant_char(c: char) -> char {
    if c.is_ascii() {
        return c.to_ascii_uppercase();
    }
    let cp = c as u32;
    match upper_table::UPPER_INVARIANT.binary_search_by_key(&cp, |&(from, _)| from) {
        Ok(i) => char::from_u32(upper_table::UPPER_INVARIANT[i].1).unwrap_or(c),
        Err(_) => c,
    }
}

/// `StringComparer.OrdinalIgnoreCase` equality.
pub fn eq_ordinal_ignore_case(a: &str, b: &str) -> bool {
    upper_invariant(a) == upper_invariant(b)
}

/// OrdinalIgnoreCase ordering: `upper_invariant` strings compared by UTF-16 code units.
pub fn cmp_ordinal_ignore_case(a: &str, b: &str) -> std::cmp::Ordering {
    let (a, b) = (upper_invariant(a), upper_invariant(b));
    a.encode_utf16().cmp(b.encode_utf16())
}

/// GUID "N" format: 32 lowercase hex characters.
pub fn guid_n() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// `SubtitleTime.Short(s)` = `MM:SS.ss` (`65.25` → `01:05.25`): C# `$"{(int)s / 60:00}:{s % 60:00.00}"` with the
/// invariant culture (minutes truncated toward zero, `%` the float remainder, seconds rounded half away from zero).
pub fn subtitle_time_short(seconds: f64) -> String {
    let whole = if seconds.is_nan() { 0 } else { seconds.clamp(i32::MIN as f64, i32::MAX as f64) as i32 };
    let minutes = whole / 60;
    let rem = if seconds.is_finite() { seconds % 60.0 } else { f64::NAN };
    format!("{}:{}", dotnet_fixed(minutes as f64, 0, 2), dotnet_fixed(rem, 2, 2))
}

/// Placeholder when no line is selected.
pub const SUBTITLE_TIME_PLACEHOLDER: &str = "––:––.––";

/// Windows `FormatByteSize` (win-viewmodels §4.2): `<= 0` → `0 MB`; below 1 MiB `0.##` MB; below 1 GiB `0.#` MB;
/// else `0.##` GB. The decimal separator follows the UI language (tr `,`, en `.`); no digit grouping.
pub fn format_byte_size_windows(bytes: i64, language: UiLanguage) -> String {
    if bytes <= 0 {
        return "0 MB".into();
    }
    let mb = bytes as f64 / (1024.0 * 1024.0);
    let (value, decimals, unit) = if mb < 1024.0 {
        (mb, if mb < 1.0 { 2 } else { 1 }, "MB")
    } else {
        (mb / 1024.0, 2, "GB")
    };
    format!("{} {unit}", format_optional_decimals(value, decimals, decimal_separator(language)))
}

/// The decimal separator of a UI language (`,` Turkish, `.` English).
pub fn decimal_separator(language: UiLanguage) -> char {
    match language {
        UiLanguage::Tr => ',',
        UiLanguage::En => '.',
    }
}

/// .NET custom format `0.#…#` (`decimals` optional digits): rounded half away from zero, trailing zeros and a
/// trailing separator dropped.
pub fn format_optional_decimals(value: f64, decimals: u32, separator: char) -> String {
    let text = dotnet_fixed(value, decimals, 1);
    if !text.contains('.') {
        return text;
    }
    let trimmed = text.trim_end_matches('0');
    let trimmed = trimmed.strip_suffix('.').unwrap_or(trimmed);
    trimmed.replacen('.', &separator.to_string(), 1)
}

/// Fixed-point text with `decimals` digits after `.` and at least `min_int_digits` integer digits, like a .NET
/// custom numeric format (`00.00`, `0.0`) in the invariant culture: the double is first taken to 15 significant
/// digits (.NET's custom-format precision), then rounded half away from zero on those decimal digits, so
/// `0.125 → 0.13` and `2.675 → 2.68` regardless of the binary value. The sign follows .NET Core 3.0+: every negative
/// value keeps its `-`, also `-0.0` and values that round to zero (`-0.004` → `-0.00`; checked against the C# app by
/// `tests/csharp_compat.rs`).
pub fn dotnet_fixed(value: f64, decimals: u32, min_int_digits: usize) -> String {
    if !value.is_finite() {
        return if value.is_nan() { "NaN".into() } else if value > 0.0 { "∞".into() } else { "-∞".into() };
    }
    let decimals = decimals as usize;
    // 15 significant digits: "d.dddddddddddddde<exp>".
    let sci = format!("{:.14e}", value.abs());
    let (mantissa, exponent) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exponent: i64 = exponent.parse().unwrap_or(0);
    let mut digits: Vec<u8> = mantissa.bytes().filter(u8::is_ascii_digit).map(|b| b - b'0').collect();
    let is_zero = digits.iter().all(|d| *d == 0);
    // `point` = number of digits before the decimal point (may be ≤ 0 or > 15).
    let mut point = if is_zero { 1 } else { exponent + 1 };
    // Keep `point + decimals` digits, rounding on the first dropped one (≥ 5 rounds away from zero).
    let keep = point + decimals as i64;
    if keep < 0 {
        digits.clear();
    } else {
        let keep = keep as usize;
        let round_up = digits.get(keep).is_some_and(|d| *d >= 5);
        digits.truncate(keep);
        digits.resize(keep, 0);
        if round_up {
            let mut i = keep;
            loop {
                if i == 0 {
                    digits.insert(0, 1);
                    point += 1;
                    break;
                }
                i -= 1;
                if digits[i] == 9 {
                    digits[i] = 0;
                } else {
                    digits[i] += 1;
                    break;
                }
            }
        }
    }
    // Lay the digits out around the point.
    let digit_at = |pos: i64| -> u8 {
        let index = pos;
        if index < 0 { 0 } else { digits.get(index as usize).copied().unwrap_or(0) }
    };
    let mut int_text: String = (0..point.max(0)).map(|p| char::from(b'0' + digit_at(p))).collect();
    let trimmed = int_text.trim_start_matches('0').len();
    int_text = int_text[int_text.len() - trimmed..].to_owned();
    while int_text.len() < min_int_digits.max(if decimals == 0 { 1 } else { 0 }) {
        int_text.insert(0, '0');
    }
    let frac_text: String = (0..decimals as i64).map(|i| char::from(b'0' + digit_at(point + i))).collect();
    let negative = value.is_sign_negative();
    let mut out = String::new();
    if negative {
        out.push('-');
    }
    out.push_str(&int_text);
    if decimals > 0 {
        out.push('.');
        out.push_str(&frac_text);
    }
    out
}

/// .NET `Math.Round(x, MidpointRounding.AwayFromZero)` for non-negative and negative values.
pub fn round_half_away_from_zero(x: f64) -> f64 {
    x.round()
}

/// .NET `String.Trim()` / Swift `trimmingCharacters(in: .whitespacesAndNewlines)`: Unicode white space.
pub fn trim_dotnet(s: &str) -> &str {
    s.trim_matches(char::is_whitespace)
}
