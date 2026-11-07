//! Python 3.11 (Unicode 14.0) character classes and string helpers the LLM validators and the reply parser rely
//! on (spec translate-llm.md §13.2, §13.3). Tables are generated from CPython by `tools/gen_pyunicode.py`.
//!
//! Rust's `char::is_alphanumeric` / regex `\w` differ from Python's `str.isalnum` / `re` `\w` (combining marks,
//! `Nl`, Unicode versions), and std has no full case folding, so everything here comes from CPython's database.

#[allow(clippy::all)]
mod tables {
    include!("pyunicode_tables.rs");
}

use se_core::pyfmt;

fn in_ranges(table: &[(u32, u32)], c: char) -> bool {
    let cp = c as u32;
    table
        .binary_search_by(|&(lo, hi)| {
            if hi < cp {
                std::cmp::Ordering::Less
            } else if lo > cp {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

fn mapped(table: &'static [(u32, &'static str)], c: char) -> Option<&'static str> {
    let cp = c as u32;
    table.binary_search_by_key(&cp, |&(k, _)| k).ok().map(|i| table[i].1)
}

/// `str.isalnum()` for one character.
pub fn isalnum(c: char) -> bool {
    in_ranges(tables::ALNUM, c)
}

/// Python `re` `\w` (str pattern): `isalnum() or '_'`.
pub fn is_word(c: char) -> bool {
    c == '_' || isalnum(c)
}

/// Python `re` `[^\W\d_]`: alphanumeric but not a decimal digit (and not `_`).
pub fn is_letter_class(c: char) -> bool {
    in_ranges(tables::ALNUM_NOT_DECIMAL, c)
}

/// `str.isdecimal()` / `re` `\d` for one character: its decimal value.
pub fn decimal_value(c: char) -> Option<u32> {
    let cp = c as u32;
    let t = tables::DECIMAL;
    let i = t.partition_point(|&(_, hi, _)| hi < cp);
    match t.get(i) {
        Some(&(lo, hi, v)) if lo <= cp && cp <= hi => Some(v + (cp - lo)),
        _ => None,
    }
}

/// `str.isdecimal()` for one character.
pub fn isdecimal(c: char) -> bool {
    decimal_value(c).is_some()
}

/// `str.isupper()` for one character.
pub fn isupper(c: char) -> bool {
    in_ranges(tables::UPPER, c)
}

/// `str.islower()` for one character.
pub fn islower(c: char) -> bool {
    in_ranges(tables::LOWER, c)
}

/// `c.lower()` (full lowercase mapping of one character).
pub fn push_lower(out: &mut String, c: char) {
    match mapped(tables::LOWER_MAP, c) {
        Some(s) => out.push_str(s),
        None => out.push(c),
    }
}

/// `c.casefold()` (full case folding of one character).
pub fn push_casefold(out: &mut String, c: char) {
    match mapped(tables::CASEFOLD_MAP, c) {
        Some(s) => out.push_str(s),
        None => out.push(c),
    }
}

/// `s.casefold()`.
pub fn casefold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        push_casefold(&mut out, c);
    }
    out
}

/// `"".join(ch.lower() for ch in text if ch.isalnum())`.
pub fn alnum_lower(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars().filter(|&c| isalnum(c)) {
        push_lower(&mut out, c);
    }
    out
}

/// `"".join(ch.casefold() for ch in text if ch.isalnum())`.
pub fn alnum_casefold(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars().filter(|&c| isalnum(c)) {
        push_casefold(&mut out, c);
    }
    out
}

/// `str.isdigit()` on a whole string (non-empty, every char Numeric_Type Digit/Decimal).
pub fn str_isdigit(s: &str) -> bool {
    !s.is_empty() && s.chars().all(isdigit)
}

/// `str.isdigit()` for one character (Numeric_Type Decimal or Digit).
pub fn isdigit(c: char) -> bool {
    in_ranges(tables::DIGIT, c)
}

/// `re.findall(r"[^\W\d_]+", text)`.
pub fn findall_letter_runs(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    for (i, c) in s.char_indices() {
        if is_letter_class(c) {
            if start.is_none() {
                start = Some(i);
            }
        } else if let Some(st) = start.take() {
            out.push(&s[st..i]);
        }
    }
    if let Some(st) = start {
        out.push(&s[st..]);
    }
    out
}

/// `str.splitlines()` (no keepends): boundaries `\n`, `\r\n`, `\r`, `\x0b`, `\x0c`, `\x1c`, `\x1d`, `\x1e`,
/// `\x85`, U+2028, U+2029; no trailing empty element.
pub fn splitlines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut iter = s.char_indices().peekable();
    while let Some((i, c)) = iter.next() {
        let boundary = matches!(
            c,
            '\n' | '\r' | '\u{0b}' | '\u{0c}' | '\u{1c}' | '\u{1d}' | '\u{1e}' | '\u{85}' | '\u{2028}' | '\u{2029}'
        );
        if boundary {
            out.push(&s[start..i]);
            let mut next = i + c.len_utf8();
            if c == '\r' {
                if let Some(&(j, '\n')) = iter.peek() {
                    iter.next();
                    next = j + 1;
                }
            }
            start = next;
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// `" ".join(s.split())` (Python whitespace).
pub fn collapse_whitespace(s: &str) -> String {
    s.split(pyfmt::isspace).filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" ")
}

/// Python `s.rstrip(chars)`.
pub fn rstrip_chars<'a>(s: &'a str, chars: &str) -> &'a str {
    s.trim_end_matches(|c| chars.contains(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes_match_python() {
        assert!(isalnum('ş') && isalnum('٣') && isalnum('²') && !isalnum('_') && !isalnum('\u{301}'));
        assert!(is_letter_class('ş') && !is_letter_class('3') && is_letter_class('²') && !is_letter_class('_'));
        assert_eq!(decimal_value('３'), Some(3));
        assert_eq!(decimal_value('٧'), Some(7));
        assert_eq!(decimal_value('²'), None);
        assert_eq!(casefold("Straße ΣΑΣ"), "strasse σασ");
        assert_eq!(alnum_lower("İ!"), "i\u{307}");
        assert!(isupper('A') && !isupper('a') && islower('a'));
        assert!(str_isdigit("2026") && str_isdigit("²") && !str_isdigit(""));
    }

    #[test]
    fn splitlines_matches_python() {
        assert_eq!(splitlines("a\r\nb\rc\nd\u{2028}e\x1cf\n"), vec!["a", "b", "c", "d", "e", "f"]);
        assert_eq!(splitlines("\n\n"), vec!["", ""]);
        assert!(splitlines("").is_empty());
        assert_eq!(findall_letter_runs("Hi, you2 _x é"), vec!["Hi", "you", "x", "é"]);
        assert_eq!(collapse_whitespace("  a \n b\x1c c "), "a b c");
    }
}
