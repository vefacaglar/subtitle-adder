//! Python formatting and string helpers needed for byte-compatible output.
//!
//! * [`float_repr`]: `repr(float)` / `str(float)` (shortest round-trip digits, Python notation rules).
//! * [`round_ndigits`] / [`round_half_even`]: Python `round(x, n)` and `int(round(x))`.
//! * [`repr_str`]: Python `repr(str)` (used in `OSError`/`KeyError` texts).
//! * [`isspace`], [`strip`], [`lstrip`], [`rstrip`]: Python `str.isspace` / `str.strip()` semantics.
//! * [`oserror_text`], [`winerror_text`]: Python `str(OSError)` texts.

use std::io;
use std::path::Path;

/// Python `repr(x)` for a float (identical to `str(x)` since Python 3).
///
/// Shortest round-trip digits; fixed notation when `1e-4 <= |x| < 1e16` (always with a fractional part,
/// e.g. `2.0`), otherwise `d[.ddd]e±XX` with at least two exponent digits. `nan` → `nan`,
/// `inf` → `inf`, `-inf` → `-inf` (use [`json_float`] for the JSON spellings).
pub fn float_repr(x: f64) -> String {
    if x.is_nan() {
        return "nan".to_string();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf".to_string() } else { "-inf".to_string() };
    }
    // Rust's `{:e}` prints the shortest round-trip digits in scientific form ("-1.2345e-7", "0e0"), but when two
    // shortest candidates are equally close it may pick the other one than CPython's dtoa (which takes the
    // correctly rounded, ties-to-even one). So only take the digit COUNT from it and format exactly with that
    // many significant digits (exact formatting rounds half to even; the closest n-digit decimal always
    // round-trips when some n-digit decimal does).
    let shortest = format!("{:e}", x);
    let (sm, _) = shortest.split_once('e').expect("LowerExp always has an exponent");
    let n = sm.chars().filter(|c| c.is_ascii_digit()).count().max(1);
    let s = format!("{:.*e}", n - 1, x);
    let (mantissa, exp) = s.split_once('e').expect("LowerExp always has an exponent");
    let exp: i32 = exp.parse().expect("valid exponent");
    let (neg, mantissa) = match mantissa.strip_prefix('-') {
        Some(m) => (true, m),
        None => (false, mantissa),
    };
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let digits = if digits.chars().all(|c| c == '0') { "0".to_string() } else { digits };
    let ndigits = digits.len() as i32;
    // value = 0.d1d2... * 10^decpt
    let decpt = exp + 1;
    let mut out = String::with_capacity(24);
    if neg {
        out.push('-');
    }
    if decpt > -4 && decpt <= 16 {
        if decpt <= 0 {
            out.push_str("0.");
            for _ in 0..(-decpt) {
                out.push('0');
            }
            out.push_str(&digits);
        } else if decpt >= ndigits {
            out.push_str(&digits);
            for _ in 0..(decpt - ndigits) {
                out.push('0');
            }
            out.push_str(".0");
        } else {
            out.push_str(&digits[..decpt as usize]);
            out.push('.');
            out.push_str(&digits[decpt as usize..]);
        }
    } else {
        out.push_str(&digits[..1]);
        if ndigits > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        let e = decpt - 1;
        out.push('e');
        out.push(if e < 0 { '-' } else { '+' });
        let ea = e.unsigned_abs();
        if ea < 10 {
            out.push('0');
        }
        out.push_str(&ea.to_string());
    }
    out
}

/// A float as `json.dumps` writes it (`allow_nan=True`): `NaN`, `Infinity`, `-Infinity`, else [`float_repr`].
pub fn json_float(x: f64) -> String {
    if x.is_nan() {
        "NaN".to_string()
    } else if x.is_infinite() {
        if x > 0.0 { "Infinity".to_string() } else { "-Infinity".to_string() }
    } else {
        float_repr(x)
    }
}

/// Python `round(x)` / `int(round(x))`: round half to even. Returns an `f64` holding an integer value.
pub fn round_half_even(x: f64) -> f64 {
    x.round_ties_even()
}

/// Python `round(x, ndigits)` for `ndigits >= 0`: correctly rounded on the exact binary value of `x`,
/// ties to even (`round(0.0625, 3) == 0.062`, `round(2.675, 2) == 2.67`).
///
/// Non-finite values are returned unchanged (Python does the same).
pub fn round_ndigits(x: f64, ndigits: u32) -> f64 {
    if !x.is_finite() {
        return x;
    }
    // Rust's fixed-precision formatting is exact (Dragon4/Grisu exact mode) with ties to even, which is
    // what CPython's `_Py_dg_dtoa(mode=3)` does; parsing back is correctly rounded like `strtod`.
    let s = format!("{:.*}", ndigits as usize, x);
    let v: f64 = s.parse().expect("formatted float parses");
    if v == 0.0 { 0.0f64.copysign(x) } else { v }
}

/// Python `str.isspace()` for one character: Unicode `White_Space` plus U+001C..U+001F.
pub fn isspace(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Python `s.strip()` (no argument).
pub fn strip(s: &str) -> &str {
    s.trim_matches(isspace)
}

/// Python `s.lstrip()` (no argument).
pub fn lstrip(s: &str) -> &str {
    s.trim_start_matches(isspace)
}

/// Python `s.rstrip()` (no argument).
pub fn rstrip(s: &str) -> &str {
    s.trim_end_matches(isspace)
}

/// Number of Unicode code points (Python `len(str)`).
pub fn char_len(s: &str) -> usize {
    s.chars().count()
}

/// Python `s[:n]` on code points.
pub fn char_prefix(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// Universal-newline translation of text read from stdin (`\r\n` and lone `\r` become `\n`), as Python's
/// text stdin does before `json.load` sees the text (matters for error positions).
pub fn universal_newlines(s: &str) -> String {
    if !s.contains('\r') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            out.push('\n');
        } else {
            out.push(c);
        }
    }
    out
}

/// Best-effort Python `str.isprintable()` for one character (used by [`repr_str`]).
fn is_printable(c: char) -> bool {
    let u = c as u32;
    if u < 0x20 || (0x7f..=0xa0).contains(&u) {
        return false;
    }
    if c == ' ' {
        return true;
    }
    !matches!(u,
        0xad | 0x1680 | 0x2000..=0x200f | 0x2028..=0x202f | 0x205f..=0x2064 | 0x2066..=0x206f
        | 0x3000 | 0xfeff | 0xfff9..=0xfffb | 0xe000..=0xf8ff | 0xf0000..=0x10ffff)
}

/// Python `repr(s)` for a string: single quotes unless the text contains `'` and no `"`; escapes `\\`,
/// the quote, `\t \n \r`, and non-printable characters as `\xhh` / `\uhhhh` / `\Uhhhhhhhh`.
pub fn repr_str(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if is_printable(c) => out.push(c),
            c => {
                let u = c as u32;
                if u <= 0xff {
                    out.push_str(&format!("\\x{:02x}", u));
                } else if u <= 0xffff {
                    out.push_str(&format!("\\u{:04x}", u));
                } else {
                    out.push_str(&format!("\\U{:08x}", u));
                }
            }
        }
    }
    out.push(quote);
    out
}

/// errno number and C runtime `strerror` text Python would report for an OS error.
///
/// On Windows, Win32 error codes are mapped to errno like CPython's `winerror_to_errno` (common cases),
/// and the texts are the MSVC CRT ones. On POSIX the raw errno and the libc text (from `io::Error`) are used.
pub fn errno_of(err: &io::Error) -> (i32, String) {
    #[cfg(windows)]
    {
        let code = err.raw_os_error();
        let errno = match code {
            Some(2) | Some(3) | Some(15) | Some(18) | Some(53) | Some(161) | Some(206) => 2,
            Some(5) | Some(19) | Some(32) | Some(33) | Some(65) | Some(82) | Some(1920) => 13,
            Some(80) | Some(183) => 17,
            Some(145) => 41,
            Some(112) => 28,
            Some(267) => 20,
            Some(8) | Some(14) => 12,
            Some(4) => 24,
            Some(6) => 9,
            Some(_) => 22,
            None => match err.kind() {
                io::ErrorKind::NotFound => 2,
                io::ErrorKind::PermissionDenied => 13,
                io::ErrorKind::AlreadyExists => 17,
                io::ErrorKind::IsADirectory => 21,
                io::ErrorKind::NotADirectory => 20,
                io::ErrorKind::DirectoryNotEmpty => 41,
                _ => 22,
            },
        };
        let text = match errno {
            2 => "No such file or directory",
            9 => "Bad file descriptor",
            12 => "Not enough space",
            13 => "Permission denied",
            17 => "File exists",
            20 => "Not a directory",
            21 => "Is a directory",
            24 => "Too many open files",
            28 => "No space left on device",
            41 => "Directory not empty",
            _ => "Invalid argument",
        };
        (errno, text.to_string())
    }
    #[cfg(not(windows))]
    {
        let errno = err.raw_os_error().unwrap_or(0);
        (errno, os_message(err))
    }
}

/// The OS message of an `io::Error` without Rust's ` (os error N)` suffix.
fn os_message(err: &io::Error) -> String {
    let text = err.to_string();
    match text.rfind(" (os error ") {
        Some(i) if text.ends_with(')') => text[..i].to_string(),
        _ => text,
    }
}

fn with_filenames(mut head: String, filename: Option<&Path>, filename2: Option<&Path>) -> String {
    if let Some(f) = filename {
        head.push_str(": ");
        head.push_str(&repr_str(&crate::paths::py_path_str(f)));
        if let Some(f2) = filename2 {
            head.push_str(" -> ");
            head.push_str(&repr_str(&crate::paths::py_path_str(f2)));
        }
    }
    head
}

/// Python `str(OSError)` for an error raised through the C runtime (e.g. `open()`):
/// `[Errno 2] No such file or directory: 'C:\\x\\y.txt'` (the file name is `repr`-quoted).
pub fn oserror_text(err: &io::Error, filename: Option<&Path>, filename2: Option<&Path>) -> String {
    if err.raw_os_error().is_none() && !matches!(err.kind(), io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied | io::ErrorKind::AlreadyExists) {
        return err.to_string();
    }
    let (errno, text) = errno_of(err);
    with_filenames(format!("[Errno {errno}] {text}"), filename, filename2)
}

/// Python `str(OSError)` for an error raised by a Win32 call (`os.replace`, `os.unlink`, `os.mkdir`, `os.stat`
/// on Windows): `[WinError 5] Access is denied: 'C:\\x'` (FormatMessage text with trailing dots/whitespace
/// removed). On POSIX this is the same as [`oserror_text`].
pub fn winerror_text(err: &io::Error, filename: Option<&Path>, filename2: Option<&Path>) -> String {
    #[cfg(windows)]
    {
        if let Some(code) = err.raw_os_error() {
            let msg = os_message(err);
            let msg = msg.trim_end_matches(|c: char| c <= ' ' || c == '.').to_string();
            return with_filenames(format!("[WinError {code}] {msg}"), filename, filename2);
        }
        oserror_text(err, filename, filename2)
    }
    #[cfg(not(windows))]
    {
        oserror_text(err, filename, filename2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_float_repr_basic_cases() {
        let cases: &[(f64, &str)] = &[
            (0.0, "0.0"),
            (-0.0, "-0.0"),
            (1.0, "1.0"),
            (2.0, "2.0"),
            (0.42, "0.42"),
            (1e-05, "1e-05"),
            (1e16, "1e+16"),
            (1.5e-07, "1.5e-07"),
            (123456789.123, "123456789.123"),
            (0.0001, "0.0001"),
            (1e15, "1000000000000000.0"),
            (1234567890123456.7, "1234567890123456.8"),
            (0.32000000000000006, "0.32000000000000006"),
            (1e22, "1e+22"),
            (1.7976931348623157e308, "1.7976931348623157e+308"),
            (5e-324, "5e-324"),
            (-2.5e-5, "-2.5e-05"),
            (100.0, "100.0"),
        ];
        for (x, want) in cases {
            assert_eq!(float_repr(*x), *want, "repr({x:?})");
        }
        assert_eq!(json_float(f64::NAN), "NaN");
        assert_eq!(json_float(f64::INFINITY), "Infinity");
        assert_eq!(json_float(f64::NEG_INFINITY), "-Infinity");
    }

    #[test]
    fn test_round_matches_python() {
        assert_eq!(round_ndigits(0.0625, 3), 0.062);
        assert_eq!(round_ndigits(1.0625, 3), 1.062);
        assert_eq!(round_ndigits(0.1875, 3), 0.188);
        assert_eq!(round_ndigits(2.675, 2), 2.67);
        assert_eq!(round_ndigits(0.125, 2), 0.12);
        assert_eq!(round_ndigits(0.375, 2), 0.38);
        assert_eq!(round_ndigits(-0.0004, 3), -0.0);
        assert!(round_ndigits(-0.0004, 3).is_sign_negative());
        assert_eq!(round_half_even(0.5), 0.0);
        assert_eq!(round_half_even(1.5), 2.0);
        assert_eq!(round_half_even(2.5), 2.0);
        assert_eq!(round_half_even(-0.5), -0.0);
    }

    #[test]
    fn test_strip_and_isspace() {
        assert_eq!(strip(" \u{1c}EN\u{1f}\u{a0} "), "EN");
        assert!(isspace('\u{1d}'));
        assert!(!isspace('\u{200b}'));
        assert_eq!(char_prefix("dünya", 2), "dü");
        assert_eq!(char_len("dünya"), 5);
    }

    #[test]
    fn test_repr_str() {
        assert_eq!(repr_str("id"), "'id'");
        assert_eq!(repr_str("C:\\a\\b"), "'C:\\\\a\\\\b'");
        assert_eq!(repr_str("it's"), "\"it's\"");
        assert_eq!(repr_str("a'b\"c"), "'a\\'b\"c'");
        assert_eq!(repr_str("x\ny\u{7}"), "'x\\ny\\x07'");
        assert_eq!(repr_str("dünya"), "'dünya'");
    }

    #[test]
    fn test_universal_newlines() {
        assert_eq!(universal_newlines("a\r\nb\rc\n"), "a\nb\nc\n");
    }
}
