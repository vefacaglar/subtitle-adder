//! Apple/printf-style formatting as used by the catalogs (win-core §4.4; the same results Foundation's
//! `String(format:locale:)` gives for every specifier the catalogs contain).

use super::UiLanguage;

/// A formatting argument.
#[derive(Debug, Clone, PartialEq)]
pub enum Arg {
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    /// A missing value (`null` in C#): formats as `""`.
    Null,
}

impl From<&str> for Arg {
    fn from(v: &str) -> Self {
        Arg::Str(v.to_owned())
    }
}
impl From<String> for Arg {
    fn from(v: String) -> Self {
        Arg::Str(v)
    }
}
impl From<&String> for Arg {
    fn from(v: &String) -> Self {
        Arg::Str(v.clone())
    }
}
impl From<std::borrow::Cow<'_, str>> for Arg {
    fn from(v: std::borrow::Cow<'_, str>) -> Self {
        Arg::Str(v.into_owned())
    }
}
macro_rules! int_arg {
    ($($t:ty),*) => {$(impl From<$t> for Arg { fn from(v: $t) -> Self { Arg::Int(v as i64) } })*};
}
int_arg!(i8, i16, i32, i64, u8, u16, u32, isize);
impl From<u64> for Arg {
    fn from(v: u64) -> Self {
        Arg::Int(i64::try_from(v).unwrap_or(i64::MAX))
    }
}
impl From<usize> for Arg {
    fn from(v: usize) -> Self {
        Arg::Int(i64::try_from(v).unwrap_or(i64::MAX))
    }
}
impl From<f64> for Arg {
    fn from(v: f64) -> Self {
        Arg::Float(v)
    }
}
impl From<f32> for Arg {
    fn from(v: f32) -> Self {
        Arg::Float(v as f64)
    }
}
impl From<bool> for Arg {
    fn from(v: bool) -> Self {
        Arg::Bool(v)
    }
}
impl<T: Into<Arg>> From<Option<T>> for Arg {
    fn from(v: Option<T>) -> Self {
        v.map_or(Arg::Null, Into::into)
    }
}

const FLAGS: &[char] = &['-', '+', ' ', '#', '0'];
const LENGTH_MODIFIERS: &[char] = &['h', 'l', 'q', 'L', 'z', 't', 'j'];

/// Formats `format` with `args` using the decimal separator of `language` (`,` for tr, `.` for en).
///
/// Rules (win-core §4.4): `%%` → `%`; optional positional `n$`; flags, width, `.precision` and length modifiers are
/// skipped (precision only matters for `%f`); a trailing incomplete specifier is copied verbatim; the argument
/// counter advances for every conversion, including unknown ones (which are copied verbatim); a missing argument
/// formats as `""`. No digit grouping is ever applied.
pub fn printf(format: &str, language: UiLanguage, args: &[Arg]) -> String {
    let chars: Vec<char> = format.chars().collect();
    let mut out = String::with_capacity(format.len() + 16);
    let mut next = 0usize;
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        if c != '%' {
            out.push(c);
            i += 1;
            continue;
        }
        let spec_start = i;
        i += 1;
        if i < chars.len() && chars[i] == '%' {
            out.push('%');
            i += 1;
            continue;
        }
        // Optional positional index.
        let mut position: Option<i64> = None;
        let digits_start = i;
        while i < chars.len() && chars[i].is_ascii_digit() {
            i += 1;
        }
        if i > digits_start && i < chars.len() && chars[i] == '$' {
            let n: String = chars[digits_start..i].iter().collect();
            position = Some(n.parse::<i64>().unwrap_or(i64::MAX));
            i += 1;
        } else {
            i = digits_start;
        }
        // Flags, width, precision.
        while i < chars.len() && FLAGS.contains(&chars[i]) {
            i += 1;
        }
        while i < chars.len() && chars[i].is_ascii_digit() {
            i += 1;
        }
        let mut precision: Option<usize> = None;
        if i < chars.len() && chars[i] == '.' {
            i += 1;
            let p_start = i;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            let p: String = chars[p_start..i].iter().collect();
            precision = Some(if p.is_empty() { 0 } else { p.parse::<usize>().unwrap_or(usize::MAX).min(1000) });
        }
        while i < chars.len() && LENGTH_MODIFIERS.contains(&chars[i]) {
            i += 1;
        }
        if i >= chars.len() {
            out.extend(&chars[spec_start..]);
            break;
        }
        let conversion = chars[i];
        i += 1;
        let index: i64 = match position {
            Some(p) => p - 1,
            None => {
                let n = next as i64;
                next += 1;
                n
            }
        };
        let arg = if index >= 0 && (index as usize) < args.len() { &args[index as usize] } else { &Arg::Null };
        match conversion {
            '@' | 's' => out.push_str(&to_display_string(arg, language)),
            'd' | 'i' | 'u' => {
                if let Some(v) = to_i64(arg) {
                    out.push_str(&v.to_string());
                }
            }
            'f' | 'F' => {
                if let Some(v) = to_f64(arg) {
                    out.push_str(&format_fixed(v, precision.unwrap_or(6), language));
                }
            }
            _ => out.extend(&chars[spec_start..i]),
        }
    }
    out
}

/// `%@` rendering: strings as-is, integers in decimal, floats in shortest round-trip form with the culture's decimal
/// separator, booleans `True`/`False`, null `""`.
fn to_display_string(arg: &Arg, language: UiLanguage) -> String {
    match arg {
        Arg::Str(s) => s.clone(),
        Arg::Int(v) => v.to_string(),
        Arg::Float(v) => shortest_float(*v, language),
        Arg::Bool(b) => if *b { "True".into() } else { "False".into() },
        Arg::Null => String::new(),
    }
}

fn shortest_float(v: f64, language: UiLanguage) -> String {
    if v.is_nan() {
        return "NaN".into();
    }
    if v.is_infinite() {
        return if v > 0.0 { "∞".into() } else { "-∞".into() };
    }
    let s = format!("{v}");
    if language.decimal_separator() == '.' { s } else { s.replace('.', &language.decimal_separator().to_string()) }
}

/// `Convert.ToInt64` semantics: floats round half to even, numeric strings parse, booleans 1/0, null → None.
fn to_i64(arg: &Arg) -> Option<i64> {
    match arg {
        Arg::Int(v) => Some(*v),
        Arg::Float(v) => {
            if v.is_finite() {
                Some(v.round_ties_even().clamp(i64::MIN as f64, i64::MAX as f64) as i64)
            } else {
                None
            }
        }
        Arg::Bool(b) => Some(i64::from(*b)),
        Arg::Str(s) => {
            let t = s.trim();
            t.parse::<i64>().ok().or_else(|| t.parse::<f64>().ok().filter(|f| f.is_finite()).map(|f| f.round_ties_even() as i64))
        }
        Arg::Null => None,
    }
}

fn to_f64(arg: &Arg) -> Option<f64> {
    match arg {
        Arg::Int(v) => Some(*v as f64),
        Arg::Float(v) => Some(*v),
        Arg::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Arg::Str(s) => s.trim().parse::<f64>().ok(),
        Arg::Null => None,
    }
}

/// Fixed-point with `precision` decimals, rounding the exact binary value half **away from zero** (.NET), with the
/// culture's decimal separator.
fn format_fixed(v: f64, precision: usize, language: UiLanguage) -> String {
    if !v.is_finite() {
        return shortest_float(v, language);
    }
    // Rust rounds exact ties to even; detect an exact tie from the exact decimal expansion and round away instead.
    let mut s = format!("{v:.precision$}");
    let exact = format!("{:.1100}", v.abs());
    if let Some(dot) = exact.find('.') {
        let frac = exact[dot + 1..].trim_end_matches('0');
        if frac.len() == precision + 1 && frac.ends_with('5') {
            // Exact tie: the magnitude must round up (away from zero).
            // The digits up to `precision`, taken from the exact expansion, then +1 in the last place.
            let int_part = &exact[..dot];
            let kept = &frac[..precision];
            let truncated = if precision == 0 { int_part.to_owned() } else { format!("{int_part}.{kept}") };
            let bumped = increment_last_digit(&truncated);
            s = if v < 0.0 { format!("-{bumped}") } else { bumped };
        }
    }
    if language.decimal_separator() != '.' {
        s = s.replace('.', &language.decimal_separator().to_string());
    }
    s
}

/// Adds one unit in the last decimal place of a non-negative fixed-point decimal string.
fn increment_last_digit(s: &str) -> String {
    let mut bytes: Vec<u8> = s.as_bytes().to_vec();
    let mut i = bytes.len();
    loop {
        if i == 0 {
            bytes.insert(0, b'1');
            break;
        }
        i -= 1;
        match bytes[i] {
            b'.' => continue,
            b'9' => bytes[i] = b'0',
            d => {
                bytes[i] = d + 1;
                break;
            }
        }
    }
    String::from_utf8(bytes).unwrap_or_default()
}
