//! Python-compatible JSON.
//!
//! [`Value`] keeps the distinction Python keeps: JSON integers stay integers (`0` → `0`), floats stay
//! floats (`0.0` → `0.0`, `1.50` → `1.5`, `1E3` → `1000.0`), object keys keep insertion order (a repeated
//! key keeps its first position and takes the last value, like a `dict`).
//!
//! * [`parse`] = `json.loads(s)` with the same error texts (`Expecting value: line 1 column 1 (char 0)`).
//! * [`dumps`] = `json.dumps(v, ensure_ascii=...)` with the default separators `", "` / `": "`.
//! * [`dumps_with`] = `json.dumps` with `indent`, custom separators and `sort_keys`.

use std::fmt;

use crate::pyfmt;

/// A JSON value with Python semantics.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    /// An integer that fits in `i64`.
    Int(i64),
    /// An integer outside the `i64` range, kept as its decimal text (Python ints are unbounded).
    BigInt(String),
    Float(f64),
    Str(String),
    Array(Vec<Value>),
    Object(Map),
}

/// An insertion-ordered JSON object (Python `dict`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Map {
    entries: Vec<(String, Value)>,
}

impl Map {
    pub fn new() -> Self {
        Self { entries: Vec::new() }
    }

    /// Inserts or replaces (keeping the original position, like `dict[key] = value`).
    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<Value>) {
        let key = key.into();
        let value = value.into();
        if let Some(slot) = self.entries.iter_mut().find(|(k, _)| *k == key) {
            slot.1 = value;
        } else {
            self.entries.push((key, value));
        }
    }

    /// Builder form of [`Map::insert`].
    pub fn with(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.insert(key, value);
        self
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        self.entries.iter_mut().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    pub fn remove(&mut self, key: &str) -> Option<Value> {
        let i = self.entries.iter().position(|(k, _)| k == key)?;
        Some(self.entries.remove(i).1)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &Value)> {
        self.entries.iter().map(|(k, v)| (k, v))
    }

    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.entries.iter().map(|(k, _)| k)
    }
}

impl FromIterator<(String, Value)> for Map {
    fn from_iter<I: IntoIterator<Item = (String, Value)>>(iter: I) -> Self {
        let mut m = Map::new();
        for (k, v) in iter {
            m.insert(k, v);
        }
        m
    }
}

impl Value {
    /// Python type name of the value (`dict`, `list`, `str`, `int`, `float`, `bool`, `NoneType`),
    /// used in Python-style `TypeError`/`AttributeError` texts.
    pub fn py_type_name(&self) -> &'static str {
        match self {
            Value::Null => "NoneType",
            Value::Bool(_) => "bool",
            Value::Int(_) | Value::BigInt(_) => "int",
            Value::Float(_) => "float",
            Value::Str(_) => "str",
            Value::Array(_) => "list",
            Value::Object(_) => "dict",
        }
    }

    /// Python truthiness (`bool(value)`).
    pub fn is_truthy(&self) -> bool {
        match self {
            Value::Null => false,
            Value::Bool(b) => *b,
            Value::Int(i) => *i != 0,
            Value::BigInt(_) => true,
            Value::Float(f) => *f != 0.0,
            Value::Str(s) => !s.is_empty(),
            Value::Array(a) => !a.is_empty(),
            Value::Object(o) => !o.is_empty(),
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// `Int` as `i64` (no conversion from floats or bools).
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }

    /// Any JSON number (int, big int, float) as `f64`; `None` for everything else (bools included).
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Int(i) => Some(*i as f64),
            Value::BigInt(s) => s.parse().ok(),
            Value::Float(f) => Some(*f),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&Vec<Value>> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<&Map> {
        match self {
            Value::Object(o) => Some(o),
            _ => None,
        }
    }

    /// `value.get(key)` for objects; `None` for other types or a missing key.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.as_object().and_then(|o| o.get(key))
    }

    /// Python `repr()` of the equivalent Python object (`None`, `True`, `1.0`, `'text'`, `[1, 'a']`,
    /// `{'k': 1}`).
    pub fn py_repr(&self) -> String {
        match self {
            Value::Null => "None".to_string(),
            Value::Bool(true) => "True".to_string(),
            Value::Bool(false) => "False".to_string(),
            Value::Int(i) => i.to_string(),
            Value::BigInt(s) => s.clone(),
            Value::Float(f) => pyfmt::float_repr(*f),
            Value::Str(s) => pyfmt::repr_str(s),
            Value::Array(a) => {
                let items: Vec<String> = a.iter().map(Value::py_repr).collect();
                format!("[{}]", items.join(", "))
            }
            Value::Object(o) => {
                let items: Vec<String> =
                    o.iter().map(|(k, v)| format!("{}: {}", pyfmt::repr_str(k), v.py_repr())).collect();
                format!("{{{}}}", items.join(", "))
            }
        }
    }

    /// Python `str()` of the equivalent Python object (a string is itself, everything else is `repr`).
    pub fn py_str(&self) -> String {
        match self {
            Value::Str(s) => s.clone(),
            other => other.py_repr(),
        }
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::Str(s.to_string())
    }
}
impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::Str(s)
    }
}
impl From<&String> for Value {
    fn from(s: &String) -> Self {
        Value::Str(s.clone())
    }
}
impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::Bool(b)
    }
}
impl From<f64> for Value {
    fn from(f: f64) -> Self {
        Value::Float(f)
    }
}
impl From<f32> for Value {
    fn from(f: f32) -> Self {
        Value::Float(f as f64)
    }
}
macro_rules! int_from {
    ($($t:ty),*) => {$(
        impl From<$t> for Value {
            fn from(i: $t) -> Self {
                match i64::try_from(i) {
                    Ok(v) => Value::Int(v),
                    Err(_) => Value::BigInt(i.to_string()),
                }
            }
        }
    )*};
}
int_from!(i8, i16, i32, i64, i128, u8, u16, u32, u64, u128, usize, isize);
impl From<Map> for Value {
    fn from(m: Map) -> Self {
        Value::Object(m)
    }
}
impl From<Vec<Value>> for Value {
    fn from(a: Vec<Value>) -> Self {
        Value::Array(a)
    }
}
impl<T: Into<Value>> From<Option<T>> for Value {
    fn from(o: Option<T>) -> Self {
        match o {
            Some(v) => v.into(),
            None => Value::Null,
        }
    }
}

// --------------------------------------------------------------------------------------------------
// Writer
// --------------------------------------------------------------------------------------------------

/// Options of [`dumps_with`] (the `json.dumps` keyword arguments that matter here).
#[derive(Clone, Debug)]
pub struct DumpOptions {
    /// `ensure_ascii`: escape every non-ASCII character (and DEL) as `\uXXXX` (UTF-16 pairs above U+FFFF).
    pub ensure_ascii: bool,
    /// `indent`: `None` = single line; `Some(n)` = newline + `n` spaces per level.
    pub indent: Option<usize>,
    /// Item separator; `None` = Python's default (`", "` without indent, `","` with indent).
    pub item_separator: Option<String>,
    /// Key separator (default `": "`).
    pub key_separator: String,
    /// `sort_keys` (sorts by code point, like Python's `sorted` on `str`).
    pub sort_keys: bool,
}

impl Default for DumpOptions {
    fn default() -> Self {
        Self { ensure_ascii: true, indent: None, item_separator: None, key_separator: ": ".to_string(), sort_keys: false }
    }
}

/// `json.dumps(value, ensure_ascii=ensure_ascii)`: one line, separators `", "` and `": "`.
///
/// Events use `ensure_ascii = false` (raw UTF-8); the WAV identity marker uses `true`.
pub fn dumps(value: &Value, ensure_ascii: bool) -> String {
    dumps_with(value, &DumpOptions { ensure_ascii, ..DumpOptions::default() })
}

/// `json.dumps(value, ensure_ascii=..., indent=..., separators=..., sort_keys=...)`.
pub fn dumps_with(value: &Value, opts: &DumpOptions) -> String {
    let item_sep = opts.item_separator.clone().unwrap_or_else(|| if opts.indent.is_some() { ",".into() } else { ", ".into() });
    let mut out = String::new();
    write_value(&mut out, value, opts, &item_sep, 0);
    out
}

/// `json.dumps(s)` of a single string (with quotes).
pub fn encode_str(s: &str, ensure_ascii: bool) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    write_str(&mut out, s, ensure_ascii);
    out
}

fn write_str(out: &mut String, s: &str, ensure_ascii: bool) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c if ensure_ascii && (c as u32) >= 0x7f => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{:04x}", unit));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn newline_indent(out: &mut String, indent: usize, level: usize) {
    out.push('\n');
    for _ in 0..indent * level {
        out.push(' ');
    }
}

fn write_value(out: &mut String, value: &Value, opts: &DumpOptions, item_sep: &str, level: usize) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Int(i) => out.push_str(&i.to_string()),
        Value::BigInt(s) => out.push_str(s),
        Value::Float(f) => out.push_str(&pyfmt::json_float(*f)),
        Value::Str(s) => write_str(out, s, opts.ensure_ascii),
        Value::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(item_sep);
                }
                if let Some(n) = opts.indent {
                    newline_indent(out, n, level + 1);
                }
                write_value(out, item, opts, item_sep, level + 1);
            }
            if let Some(n) = opts.indent {
                newline_indent(out, n, level);
            }
            out.push(']');
        }
        Value::Object(map) => {
            if map.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push('{');
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            if opts.sort_keys {
                entries.sort_by(|a, b| a.0.cmp(b.0));
            }
            for (i, (k, v)) in entries.into_iter().enumerate() {
                if i > 0 {
                    out.push_str(item_sep);
                }
                if let Some(n) = opts.indent {
                    newline_indent(out, n, level + 1);
                }
                write_str(out, k, opts.ensure_ascii);
                out.push_str(&opts.key_separator);
                write_value(out, v, opts, item_sep, level + 1);
            }
            if let Some(n) = opts.indent {
                newline_indent(out, n, level);
            }
            out.push('}');
        }
    }
}

// --------------------------------------------------------------------------------------------------
// Parser
// --------------------------------------------------------------------------------------------------

/// Python `json.JSONDecodeError`. `Display` = `"<msg>: line L column C (char P)"` (positions in code points).
#[derive(Clone, Debug, PartialEq)]
pub struct JsonDecodeError {
    pub msg: String,
    /// Code-point index into the document.
    pub pos: usize,
    pub lineno: usize,
    pub colno: usize,
}

impl fmt::Display for JsonDecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: line {} column {} (char {})", self.msg, self.lineno, self.colno, self.pos)
    }
}

impl std::error::Error for JsonDecodeError {}

/// Python `json.loads(s)` (`strict=True`, NaN/Infinity accepted, duplicate keys: last wins).
///
/// Lone UTF-16 surrogates from `\uD800`-style escapes cannot live in a Rust `String`; they become U+FFFD
/// (Python would keep them and fail later when writing).
pub fn parse(s: &str) -> Result<Value, JsonDecodeError> {
    let chars: Vec<char> = s.chars().collect();
    let mut p = Parser { s: &chars };
    if chars.first() == Some(&'\u{feff}') {
        return Err(p.err("Unexpected UTF-8 BOM (decode using utf-8-sig)", 0));
    }
    let start = p.skip_ws(0);
    let (value, end) = p.scan_once(start)?;
    let end = p.skip_ws(end);
    if end != chars.len() {
        return Err(p.err("Extra data", end));
    }
    Ok(value)
}

/// Strict UTF-8 decoding of raw stdin bytes, with a Python `UnicodeDecodeError`-like message
/// (`'utf-8' codec can't decode byte 0xff in position 0: invalid start byte`).
pub fn decode_utf8_strict(bytes: &[u8]) -> Result<&str, String> {
    match std::str::from_utf8(bytes) {
        Ok(s) => Ok(s),
        Err(e) => {
            let pos = e.valid_up_to();
            match e.error_len() {
                Some(n) => {
                    let b = bytes[pos];
                    let start_ok = matches!(b, 0xc2..=0xf4);
                    let reason = if start_ok { "invalid continuation byte" } else { "invalid start byte" };
                    let _ = n;
                    Err(format!("'utf-8' codec can't decode byte 0x{b:02x} in position {pos}: {reason}"))
                }
                None => {
                    let len = bytes.len() - pos;
                    if len == 1 {
                        Err(format!(
                            "'utf-8' codec can't decode byte 0x{:02x} in position {pos}: unexpected end of data",
                            bytes[pos]
                        ))
                    } else {
                        Err(format!(
                            "'utf-8' codec can't decode bytes in position {}-{}: unexpected end of data",
                            pos,
                            bytes.len() - 1
                        ))
                    }
                }
            }
        }
    }
}

struct Parser<'a> {
    s: &'a [char],
}

impl Parser<'_> {
    fn err(&self, msg: &str, pos: usize) -> JsonDecodeError {
        let before = &self.s[..pos.min(self.s.len())];
        let lineno = before.iter().filter(|c| **c == '\n').count() + 1;
        let colno = match before.iter().rposition(|c| *c == '\n') {
            Some(i) => pos - i,
            None => pos + 1,
        };
        JsonDecodeError { msg: msg.to_string(), pos, lineno, colno }
    }

    fn skip_ws(&self, mut i: usize) -> usize {
        while i < self.s.len() && matches!(self.s[i], ' ' | '\t' | '\n' | '\r') {
            i += 1;
        }
        i
    }

    fn starts_with(&self, i: usize, lit: &str) -> bool {
        let mut j = i;
        for c in lit.chars() {
            if j >= self.s.len() || self.s[j] != c {
                return false;
            }
            j += 1;
        }
        true
    }

    /// `scan_once`: one value at `i`; failure is "Expecting value" at `i` (Python's StopIteration).
    fn scan_once(&mut self, i: usize) -> Result<(Value, usize), JsonDecodeError> {
        if i >= self.s.len() {
            return Err(self.err("Expecting value", i));
        }
        match self.s[i] {
            '"' => {
                let (s, end) = self.scan_string(i + 1)?;
                Ok((Value::Str(s), end))
            }
            '{' => self.parse_object(i + 1),
            '[' => self.parse_array(i + 1),
            'n' if self.starts_with(i, "null") => Ok((Value::Null, i + 4)),
            't' if self.starts_with(i, "true") => Ok((Value::Bool(true), i + 4)),
            'f' if self.starts_with(i, "false") => Ok((Value::Bool(false), i + 5)),
            'N' if self.starts_with(i, "NaN") => Ok((Value::Float(f64::NAN), i + 3)),
            'I' if self.starts_with(i, "Infinity") => Ok((Value::Float(f64::INFINITY), i + 8)),
            '-' if self.starts_with(i, "-Infinity") => Ok((Value::Float(f64::NEG_INFINITY), i + 9)),
            _ => self.match_number(i),
        }
    }

    fn is_digit(&self, i: usize) -> bool {
        i < self.s.len() && self.s[i].is_ascii_digit()
    }

    fn match_number(&self, start: usize) -> Result<(Value, usize), JsonDecodeError> {
        let mut i = start;
        if i < self.s.len() && self.s[i] == '-' {
            i += 1;
        }
        if i >= self.s.len() {
            return Err(self.err("Expecting value", start));
        }
        match self.s[i] {
            '1'..='9' => {
                i += 1;
                while self.is_digit(i) {
                    i += 1;
                }
            }
            '0' => i += 1,
            _ => return Err(self.err("Expecting value", start)),
        }
        let mut is_float = false;
        if i < self.s.len() && self.s[i] == '.' && self.is_digit(i + 1) {
            is_float = true;
            i += 2;
            while self.is_digit(i) {
                i += 1;
            }
        }
        if i < self.s.len() && (self.s[i] == 'e' || self.s[i] == 'E') {
            let e_start = i;
            i += 1;
            if i < self.s.len() && (self.s[i] == '-' || self.s[i] == '+') {
                i += 1;
            }
            let digits_start = i;
            while self.is_digit(i) {
                i += 1;
            }
            if i > digits_start {
                is_float = true;
            } else {
                i = e_start;
            }
        }
        let text: String = self.s[start..i].iter().collect();
        if is_float {
            let f: f64 = text.parse().unwrap_or(f64::NAN);
            Ok((Value::Float(f), i))
        } else {
            match text.parse::<i64>() {
                Ok(v) => Ok((Value::Int(v), i)),
                Err(_) => Ok((Value::BigInt(text), i)),
            }
        }
    }

    /// Python's C `scanstring` (strict): `i` is just after the opening quote.
    fn scan_string(&self, i: usize) -> Result<(String, usize), JsonDecodeError> {
        let begin = i - 1;
        let mut out = String::new();
        let mut j = i;
        loop {
            if j >= self.s.len() {
                return Err(self.err("Unterminated string starting at", begin));
            }
            let c = self.s[j];
            if c == '"' {
                return Ok((out, j + 1));
            }
            if c == '\\' {
                j += 1;
                if j >= self.s.len() {
                    return Err(self.err("Unterminated string starting at", begin));
                }
                let e = self.s[j];
                match e {
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    '/' => out.push('/'),
                    'b' => out.push('\u{8}'),
                    'f' => out.push('\u{c}'),
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    'u' => {
                        let esc_pos = j;
                        let u = self.hex4(j + 1).ok_or_else(|| self.err("Invalid \\uXXXX escape", esc_pos))?;
                        j += 4;
                        if (0xd800..=0xdbff).contains(&u) && j + 7 < self.s.len() && self.s[j + 1] == '\\' && self.s[j + 2] == 'u' {
                            let u2 = self.hex4(j + 3).ok_or_else(|| self.err("Invalid \\uXXXX escape", j + 2))?;
                            if (0xdc00..=0xdfff).contains(&u2) {
                                let cp = 0x10000 + (((u - 0xd800) << 10) | (u2 - 0xdc00));
                                out.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
                                j += 6;
                            } else {
                                out.push('\u{fffd}');
                            }
                        } else {
                            out.push(char::from_u32(u).unwrap_or('\u{fffd}'));
                        }
                    }
                    _ => return Err(self.err("Invalid \\escape", j - 1)),
                }
                j += 1;
                continue;
            }
            if (c as u32) < 0x20 {
                return Err(self.err("Invalid control character at", j));
            }
            out.push(c);
            j += 1;
        }
    }

    fn hex4(&self, i: usize) -> Option<u32> {
        // Python requires at least one character after the four digits (`end >= len` is an error).
        if i + 4 >= self.s.len() {
            return None;
        }
        let mut v = 0u32;
        for k in 0..4 {
            v = v * 16 + self.s[i + k].to_digit(16)?;
        }
        Some(v)
    }

    fn parse_object(&mut self, i: usize) -> Result<(Value, usize), JsonDecodeError> {
        let mut map = Map::new();
        let mut idx = self.skip_ws(i);
        if idx < self.s.len() && self.s[idx] == '}' {
            return Ok((Value::Object(map), idx + 1));
        }
        loop {
            if idx >= self.s.len() || self.s[idx] != '"' {
                return Err(self.err("Expecting property name enclosed in double quotes", idx));
            }
            let (key, next) = self.scan_string(idx + 1)?;
            idx = self.skip_ws(next);
            if idx >= self.s.len() || self.s[idx] != ':' {
                return Err(self.err("Expecting ':' delimiter", idx));
            }
            idx = self.skip_ws(idx + 1);
            let (val, next) = self.scan_once(idx)?;
            map.insert(key, val);
            idx = self.skip_ws(next);
            if idx < self.s.len() && self.s[idx] == '}' {
                return Ok((Value::Object(map), idx + 1));
            }
            if idx >= self.s.len() || self.s[idx] != ',' {
                return Err(self.err("Expecting ',' delimiter", idx));
            }
            idx = self.skip_ws(idx + 1);
        }
    }

    fn parse_array(&mut self, i: usize) -> Result<(Value, usize), JsonDecodeError> {
        let mut items = Vec::new();
        let mut idx = self.skip_ws(i);
        if idx < self.s.len() && self.s[idx] == ']' {
            return Ok((Value::Array(items), idx + 1));
        }
        loop {
            let (val, next) = self.scan_once(idx)?;
            items.push(val);
            idx = self.skip_ws(next);
            if idx < self.s.len() && self.s[idx] == ']' {
                return Ok((Value::Array(items), idx + 1));
            }
            if idx >= self.s.len() || self.s[idx] != ',' {
                return Err(self.err("Expecting ',' delimiter", idx));
            }
            idx = self.skip_ws(idx + 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dumps_event_format() {
        let v: Value = Map::new().with("event", "deleted").with("id", "base").with("freed_mb", 123).into();
        assert_eq!(dumps(&v, false), r#"{"event": "deleted", "id": "base", "freed_mb": 123}"#);
        let v: Value = Map::new().with("event", "stage").with("stage", "x").with("fraction", Value::Null).into();
        assert_eq!(dumps(&v, false), r#"{"event": "stage", "stage": "x", "fraction": null}"#);
    }

    #[test]
    fn test_parse_keeps_int_float_distinction() {
        let v = parse(r#"{"a": 0, "b": 0.0, "c": 1.50, "d": 1E3, "e": -0, "f": 123456789012345678901234567890}"#).unwrap();
        assert_eq!(dumps(&v, false), r#"{"a": 0, "b": 0.0, "c": 1.5, "d": 1000.0, "e": 0, "f": 123456789012345678901234567890}"#);
    }

    #[test]
    fn test_parse_errors_match_python() {
        let e = |s: &str| parse(s).unwrap_err().to_string();
        assert_eq!(e(""), "Expecting value: line 1 column 1 (char 0)");
        assert_eq!(e("[1,]"), "Expecting value: line 1 column 4 (char 3)");
        assert_eq!(e("[1 2]"), "Expecting ',' delimiter: line 1 column 4 (char 3)");
        assert_eq!(e("{\"a\" 1}"), "Expecting ':' delimiter: line 1 column 6 (char 5)");
        assert_eq!(e("{\"a\":1,}"), "Expecting property name enclosed in double quotes: line 1 column 8 (char 7)");
        assert_eq!(e("[] x"), "Extra data: line 1 column 4 (char 3)");
        assert_eq!(e("\n\"abc"), "Unterminated string starting at: line 2 column 1 (char 1)");
        assert_eq!(e("\"a\nb\""), "Invalid control character at: line 1 column 3 (char 2)");
        assert_eq!(e("\u{feff}[]"), "Unexpected UTF-8 BOM (decode using utf-8-sig): line 1 column 1 (char 0)");
    }

    #[test]
    fn test_duplicate_keys_keep_first_position_last_value() {
        let v = parse(r#"{"a": 1, "b": 2, "a": 3}"#).unwrap();
        assert_eq!(dumps(&v, false), r#"{"a": 3, "b": 2}"#);
    }

    #[test]
    fn test_indent_format() {
        let v = parse(r#"[{"id": 1, "words": [], "o": {}}]"#).unwrap();
        let s = dumps_with(&v, &DumpOptions { ensure_ascii: false, indent: Some(2), ..Default::default() });
        assert_eq!(s, "[\n  {\n    \"id\": 1,\n    \"words\": [],\n    \"o\": {}\n  }\n]");
        assert_eq!(dumps_with(&Value::Array(vec![]), &DumpOptions { indent: Some(2), ..Default::default() }), "[]");
    }
}
