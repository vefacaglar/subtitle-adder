//! Python `int()` / `float()` text parsing for argparse `type=int` / `type=float` (spec cli.md §2.3):
//! surrounding whitespace, sign, single underscores between digits, Unicode decimal digits; `float` also
//! `nan`, `inf`, `infinity` (case-insensitive, signed) and exponents.

/// First code point of every run of ten Unicode `Nd` digits (CPython 3.11 / Unicode 14 `unicodedata`).
const DECIMAL_ZEROS: [u32; 66] = [
    0x30, 0x660, 0x6F0, 0x7C0, 0x966, 0x9E6, 0xA66, 0xAE6, 0xB66, 0xBE6, 0xC66, 0xCE6, 0xD66, 0xDE6, 0xE50, 0xED0,
    0xF20, 0x1040, 0x1090, 0x17E0, 0x1810, 0x1946, 0x19D0, 0x1A80, 0x1A90, 0x1B50, 0x1BB0, 0x1C40, 0x1C50, 0xA620,
    0xA8D0, 0xA900, 0xA9D0, 0xA9F0, 0xAA50, 0xABF0, 0xFF10, 0x104A0, 0x10D30, 0x11066, 0x110F0, 0x11136, 0x111D0,
    0x112F0, 0x11450, 0x114D0, 0x11650, 0x116C0, 0x11730, 0x118E0, 0x11950, 0x11C50, 0x11D50, 0x11DA0, 0x16A60,
    0x16AC0, 0x16B50, 0x1D7CE, 0x1D7D8, 0x1D7E2, 0x1D7EC, 0x1D7F6, 0x1E140, 0x1E2F0, 0x1E950, 0x1FBF0,
];

/// The decimal value of a Unicode `Nd` character (`unicodedata.decimal`).
fn decimal_value(c: char) -> Option<u32> {
    let cp = c as u32;
    DECIMAL_ZEROS.iter().find(|&&zero| cp >= zero && cp < zero + 10).map(|zero| cp - zero)
}

/// Strip Python whitespace and map Unicode decimal digits to ASCII (CPython
/// `_PyUnicode_TransformDecimalAndSpaceToASCII`); other characters stay as they are.
fn to_ascii(text: &str) -> String {
    se_core::pyfmt::strip(text)
        .chars()
        .map(|c| match decimal_value(c) {
            Some(d) => char::from_digit(d, 10).unwrap(),
            None => c,
        })
        .collect()
}

/// Digits with single underscores only between digits; returns the digits without underscores.
fn digits_with_underscores(s: &str) -> Option<String> {
    if s.is_empty() || s.starts_with('_') || s.ends_with('_') || s.contains("__") {
        return None;
    }
    if !s.chars().all(|c| c.is_ascii_digit() || c == '_') {
        return None;
    }
    Some(s.chars().filter(|&c| c != '_').collect())
}

/// Python `int(text)`; `None` → argparse reports `invalid int value: '<text>'`.
///
/// Values beyond `i64` saturate (Python ints are unbounded; such counts are meaningless anyway).
pub fn py_int(text: &str) -> Option<i64> {
    let s = to_ascii(text);
    let (negative, body) = match s.as_bytes().first() {
        Some(b'+') => (false, &s[1..]),
        Some(b'-') => (true, &s[1..]),
        _ => (false, &s[..]),
    };
    let digits = digits_with_underscores(body)?;
    let mut value: i128 = 0;
    for b in digits.bytes() {
        value = (value * 10 + i128::from(b - b'0')).min(i128::from(i64::MAX) + 1);
    }
    let value = if negative { -value } else { value };
    Some(value.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64)
}

/// Python `float(text)`; `None` → argparse reports `invalid float value: '<text>'`.
pub fn py_float(text: &str) -> Option<f64> {
    let s = to_ascii(text);
    let (negative, body) = match s.as_bytes().first() {
        Some(b'+') => (false, &s[1..]),
        Some(b'-') => (true, &s[1..]),
        _ => (false, &s[..]),
    };
    let lower = body.to_ascii_lowercase();
    let magnitude = match lower.as_str() {
        "nan" => Some(f64::NAN),
        "inf" | "infinity" => Some(f64::INFINITY),
        _ => None,
    };
    if let Some(m) = magnitude {
        return Some(if negative { -m } else { m });
    }
    // mantissa [exponent]
    let (mantissa, exponent) = match lower.find('e') {
        Some(i) => (&lower[..i], Some(&lower[i + 1..])),
        None => (&lower[..], None),
    };
    let (int_part, frac_part) = match mantissa.find('.') {
        Some(i) => (&mantissa[..i], Some(&mantissa[i + 1..])),
        None => (mantissa, None),
    };
    let int_digits = if int_part.is_empty() { String::new() } else { digits_with_underscores(int_part)? };
    let frac_digits = match frac_part {
        None => String::new(),
        Some("") => String::new(),
        Some(f) => digits_with_underscores(f)?,
    };
    if int_digits.is_empty() && frac_digits.is_empty() {
        return None;
    }
    let mut normalized = String::new();
    if negative {
        normalized.push('-');
    }
    normalized.push_str(if int_digits.is_empty() { "0" } else { &int_digits });
    normalized.push('.');
    normalized.push_str(if frac_digits.is_empty() { "0" } else { &frac_digits });
    if let Some(exp) = exponent {
        let (sign, digits) = match exp.as_bytes().first() {
            Some(b'+') => ("", &exp[1..]),
            Some(b'-') => ("-", &exp[1..]),
            _ => ("", exp),
        };
        let digits = digits_with_underscores(digits)?;
        normalized.push('e');
        normalized.push_str(sign);
        normalized.push_str(&digits);
    }
    normalized.parse::<f64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ints() {
        assert_eq!(py_int(" 1_000 "), Some(1000));
        assert_eq!(py_int("+7"), Some(7));
        assert_eq!(py_int("-7"), Some(-7));
        assert_eq!(py_int("010"), Some(10));
        assert_eq!(py_int("\u{661}\u{662}"), Some(12));
        for bad in ["abc", "1__0", "_1", "1_", "1.0", "", " ", "+", "0x10", "+ 1", "2.5"] {
            assert_eq!(py_int(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn floats() {
        assert_eq!(py_float("6"), Some(6.0));
        assert_eq!(py_float(".5"), Some(0.5));
        assert_eq!(py_float("5."), Some(5.0));
        assert_eq!(py_float("1_0.2_5e1"), Some(102.5));
        assert_eq!(py_float("-Infinity"), Some(f64::NEG_INFINITY));
        assert!(py_float("nan").unwrap().is_nan());
        assert_eq!(py_float(" 1E3 "), Some(1000.0));
        assert_eq!(py_float("1e-5"), Some(1e-5));
        for bad in ["x", "1e", "_1", ".", "e5", "1.5.2", "1_.5", "1._5", "infinit", "--1", "1e+"] {
            assert_eq!(py_float(bad), None, "{bad:?}");
        }
    }
}
