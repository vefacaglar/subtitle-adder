//! Engine data types (`models.py`: `Word`, `Transcript`, `Sentence`).
//!
//! Times are [`Num`] so a JSON integer read from stdin (`"start": 0`, as Swift's `JSONEncoder` writes whole
//! doubles) is written back as `0`, and a float as Python `repr` (`0.0`). Engine-produced values are floats.
//! Arithmetic code uses [`Num::f`] / [`Word::start_f`] etc.
//!
//! Deliberate, documented strictness compared to Python's untyped `Sentence.from_dict`: `id` must be a JSON
//! integer, `start`/`end` JSON numbers (bools count as 0/1), `text` a string, `speaker` null or an integer
//! (an integral float is accepted as that integer), and `needs_review` is reduced to its truthiness.
//! Python accepted other types and failed later (or echoed them in `render -f json`).

use std::cmp::Ordering;
use std::fmt;

use crate::error::{EngineError, Result};
use crate::json::{Map, Value};
use crate::pyfmt;

/// A JSON number that remembers whether it was an integer (Python `int` vs `float`).
#[derive(Clone, Copy, Debug)]
pub enum Num {
    Int(i64),
    Float(f64),
}

impl Num {
    /// The value as `f64` (what Python arithmetic would use).
    pub fn f(self) -> f64 {
        match self {
            Num::Int(i) => i as f64,
            Num::Float(f) => f,
        }
    }

    pub fn is_int(self) -> bool {
        matches!(self, Num::Int(_))
    }

    /// Python `str()` / `repr()`: `0`, `0.0`, `1e-05`.
    pub fn py_str(self) -> String {
        match self {
            Num::Int(i) => i.to_string(),
            Num::Float(f) => pyfmt::float_repr(f),
        }
    }

    pub fn to_value(self) -> Value {
        match self {
            Num::Int(i) => Value::Int(i),
            Num::Float(f) => Value::Float(f),
        }
    }

    /// A JSON number (bools are Python ints 0/1). `None` for anything else.
    pub fn from_value(v: &Value) -> Option<Num> {
        match v {
            Value::Int(i) => Some(Num::Int(*i)),
            Value::BigInt(s) => s.parse::<f64>().ok().map(Num::Float),
            Value::Float(f) => Some(Num::Float(*f)),
            Value::Bool(b) => Some(Num::Int(*b as i64)),
            _ => None,
        }
    }
}

impl Default for Num {
    fn default() -> Self {
        Num::Float(0.0)
    }
}

impl From<f64> for Num {
    fn from(f: f64) -> Self {
        Num::Float(f)
    }
}

impl From<i64> for Num {
    fn from(i: i64) -> Self {
        Num::Int(i)
    }
}

/// Numeric equality like Python (`1 == 1.0`).
impl PartialEq for Num {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Num::Int(a), Num::Int(b)) => a == b,
            _ => self.f() == other.f(),
        }
    }
}

impl PartialOrd for Num {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        match (self, other) {
            (Num::Int(a), Num::Int(b)) => a.partial_cmp(b),
            _ => self.f().partial_cmp(&other.f()),
        }
    }
}

impl fmt::Display for Num {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.py_str())
    }
}

/// A recognized word with its time span in seconds (frozen dataclass `Word(start, end, text)`).
#[derive(Clone, Debug, PartialEq)]
pub struct Word {
    pub start: Num,
    pub end: Num,
    pub text: String,
}

impl Word {
    /// A word with float times (what every transcriber produces).
    pub fn new(start: f64, end: f64, text: impl Into<String>) -> Self {
        Self { start: Num::Float(start), end: Num::Float(end), text: text.into() }
    }

    pub fn start_f(&self) -> f64 {
        self.start.f()
    }

    pub fn end_f(&self) -> f64 {
        self.end.f()
    }

    /// `asdict(word)`: `{"start", "end", "text"}` in that order.
    pub fn to_value(&self) -> Value {
        Map::new().with("start", self.start.to_value()).with("end", self.end.to_value()).with("text", self.text.as_str()).into()
    }

    /// `Word(**w)` with Python's error texts:
    /// * not an object → `subtitle_engine.models.Word() argument after ** must be a mapping, not int`;
    /// * extra key → `Word.__init__() got an unexpected keyword argument 'x'` (first extra key);
    /// * missing keys → `Word.__init__() missing 1 required positional argument: 'text'` /
    ///   `missing 2 required positional arguments: 'end' and 'text'` /
    ///   `missing 3 required positional arguments: 'start', 'end', and 'text'`.
    pub fn from_value(v: &Value) -> Result<Word> {
        let Some(obj) = v.as_object() else {
            return Err(EngineError::type_error(format!(
                "subtitle_engine.models.Word() argument after ** must be a mapping, not {}",
                v.py_type_name()
            )));
        };
        for key in obj.keys() {
            if !matches!(key.as_str(), "start" | "end" | "text") {
                return Err(EngineError::type_error(format!(
                    "Word.__init__() got an unexpected keyword argument {}",
                    pyfmt::repr_str(key)
                )));
            }
        }
        let missing: Vec<&str> = ["start", "end", "text"].into_iter().filter(|k| !obj.contains_key(k)).collect();
        if !missing.is_empty() {
            let quoted: Vec<String> = missing.iter().map(|k| format!("'{k}'")).collect();
            let list = match quoted.len() {
                1 => quoted[0].clone(),
                2 => format!("{} and {}", quoted[0], quoted[1]),
                _ => format!("{}, {}, and {}", quoted[0], quoted[1], quoted[2]),
            };
            let noun = if missing.len() == 1 { "argument" } else { "arguments" };
            return Err(EngineError::type_error(format!(
                "Word.__init__() missing {} required positional {noun}: {list}",
                missing.len()
            )));
        }
        let start = num_field(obj, "start")?;
        let end = num_field(obj, "end")?;
        let text = str_field(obj, "text")?;
        Ok(Word { start, end, text })
    }
}

fn num_field(obj: &Map, key: &str) -> Result<Num> {
    let v = obj.get(key).ok_or_else(|| EngineError::key(key))?;
    Num::from_value(v).ok_or_else(|| {
        EngineError::type_error(format!("type {} doesn't define __round__ method", v.py_type_name()))
    })
}

fn str_field(obj: &Map, key: &str) -> Result<String> {
    let v = obj.get(key).ok_or_else(|| EngineError::key(key))?;
    match v {
        Value::Str(s) => Ok(s.clone()),
        other => Err(EngineError::type_error(format!(
            "{key} must be a string, not {}",
            other.py_type_name()
        ))),
    }
}

/// The result of speech recognition (`Transcript(language, duration, words)`).
#[derive(Clone, Debug, PartialEq)]
pub struct Transcript {
    pub language: String,
    pub duration: f64,
    pub words: Vec<Word>,
}

/// A subtitle line (`Sentence` dataclass).
#[derive(Clone, Debug, PartialEq)]
pub struct Sentence {
    pub id: i64,
    pub start: Num,
    pub end: Num,
    pub text: String,
    pub words: Vec<Word>,
    /// 0-based speaker; `None` when diarization was off.
    pub speaker: Option<i64>,
    /// The translation could not be recovered; the source text was preserved. Written only when true.
    pub needs_review: bool,
}

impl Sentence {
    /// A sentence with float times, no words, no speaker.
    pub fn new(id: i64, start: f64, end: f64, text: impl Into<String>) -> Self {
        Self {
            id,
            start: Num::Float(start),
            end: Num::Float(end),
            text: text.into(),
            words: Vec::new(),
            speaker: None,
            needs_review: false,
        }
    }

    pub fn start_f(&self) -> f64 {
        self.start.f()
    }

    pub fn end_f(&self) -> f64 {
        self.end.f()
    }

    /// `Sentence.to_dict()`: keys `id, start, end, text, words, speaker` and `needs_review` only when true.
    pub fn to_value(&self) -> Value {
        let mut m = Map::new()
            .with("id", self.id)
            .with("start", self.start.to_value())
            .with("end", self.end.to_value())
            .with("text", self.text.as_str())
            .with("words", Value::Array(self.words.iter().map(Word::to_value).collect()))
            .with("speaker", self.speaker);
        if self.needs_review {
            m.insert("needs_review", true);
        }
        m.into()
    }

    /// `Sentence.from_dict(data)` with Python's evaluation order and error texts:
    /// 1. not an object → `'<type>' object has no attribute 'get'`;
    /// 2. `words` (default `[]`) first: `null` → `'NoneType' object is not iterable`, a number →
    ///    `'int' object is not iterable`, a string iterates its characters and an object its keys (each a
    ///    `str` → the `Word()` mapping error), then each element through [`Word::from_value`];
    /// 3. `id`, `start`, `end`, `text` in that order (missing → `KeyError` text `'start'`);
    /// 4. `speaker` (missing/null → `None`), `needs_review` (truthiness, default false).
    pub fn from_value(data: &Value) -> Result<Sentence> {
        let Some(obj) = data.as_object() else {
            return Err(EngineError::type_error(format!(
                "'{}' object has no attribute 'get'",
                data.py_type_name()
            )));
        };
        let words = match obj.get("words") {
            None => Vec::new(),
            Some(v) => py_iter(v)?.iter().map(Word::from_value).collect::<Result<Vec<_>>>()?,
        };
        let id_value = obj.get("id").ok_or_else(|| EngineError::key("id"))?;
        let start = num_field(obj, "start")?;
        let end = num_field(obj, "end")?;
        let text = str_field(obj, "text")?;
        let id = match id_value {
            Value::Int(i) => *i,
            Value::Bool(b) => *b as i64,
            Value::Float(f) if f.fract() == 0.0 && f.is_finite() => *f as i64,
            other => {
                return Err(EngineError::type_error(format!(
                    "sentence id must be an integer, not {}",
                    other.py_type_name()
                )));
            }
        };
        let speaker = match obj.get("speaker") {
            None | Some(Value::Null) => None,
            Some(Value::Int(i)) => Some(*i),
            Some(Value::Bool(b)) => Some(*b as i64),
            Some(Value::Float(f)) if f.fract() == 0.0 && f.is_finite() => Some(*f as i64),
            Some(other) => {
                return Err(EngineError::type_error(format!(
                    "speaker must be an integer or null, not {}",
                    other.py_type_name()
                )));
            }
        };
        let needs_review = obj.get("needs_review").map(Value::is_truthy).unwrap_or(false);
        Ok(Sentence { id, start, end, text, words, speaker, needs_review })
    }
}

/// Python `iter(value)` turned into a list of values, with Python's `TypeError` text for non-iterables.
/// A string yields one `Str` per character, an object yields its keys.
pub fn py_iter(v: &Value) -> Result<Vec<Value>> {
    match v {
        Value::Array(a) => Ok(a.clone()),
        Value::Object(o) => Ok(o.keys().map(|k| Value::Str(k.clone())).collect()),
        Value::Str(s) => Ok(s.chars().map(|c| Value::Str(c.to_string())).collect()),
        other => Err(EngineError::type_error(format!("'{}' object is not iterable", other.py_type_name()))),
    }
}

/// `[Sentence.from_dict(d) for d in json.load(stdin)]` (used by `render` and `translate`).
pub fn sentences_from_value(v: &Value) -> Result<Vec<Sentence>> {
    py_iter(v)?.iter().map(Sentence::from_value).collect()
}

/// `[s.to_dict() for s in sentences]`.
pub fn sentences_to_value(sentences: &[Sentence]) -> Value {
    Value::Array(sentences.iter().map(Sentence::to_value).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::{dumps, parse};

    fn err(src: &str) -> String {
        sentences_from_value(&parse(src).unwrap()).unwrap_err().to_string()
    }

    #[test]
    fn test_sentence_roundtrip_keeps_number_types() {
        let src = r#"[{"id": 1, "start": 0, "end": 1.5, "text": "Düzenlendi.", "words": [{"start": 0, "end": 1.0, "text": "a"}], "needs_review": true}]"#;
        let s = sentences_from_value(&parse(src).unwrap()).unwrap();
        assert_eq!(
            dumps(&sentences_to_value(&s), false),
            r#"[{"id": 1, "start": 0, "end": 1.5, "text": "Düzenlendi.", "words": [{"start": 0, "end": 1.0, "text": "a"}], "speaker": null, "needs_review": true}]"#
        );
    }

    #[test]
    fn test_from_dict_error_texts_match_python() {
        assert_eq!(err("5"), "'int' object is not iterable");
        assert_eq!(err("\"ab\""), "'str' object has no attribute 'get'");
        assert_eq!(err("[1]"), "'int' object has no attribute 'get'");
        assert_eq!(err(r#"[{"id":1}]"#), "'start'");
        assert_eq!(err(r#"[{"words":[{"x":1}]}]"#), "Word.__init__() got an unexpected keyword argument 'x'");
        assert_eq!(
            err(r#"[{"id":1,"start":0,"end":1,"text":"a","words":[{"start":0,"end":1}]}]"#),
            "Word.__init__() missing 1 required positional argument: 'text'"
        );
        assert_eq!(
            err(r#"[{"words":[{}]}]"#),
            "Word.__init__() missing 3 required positional arguments: 'start', 'end', and 'text'"
        );
        assert_eq!(
            err(r#"[{"id":1,"start":0,"end":1,"text":"a","words":[3]}]"#),
            "subtitle_engine.models.Word() argument after ** must be a mapping, not int"
        );
        assert_eq!(err(r#"[{"id":1,"start":0,"end":1,"text":"a","words":null}]"#), "'NoneType' object is not iterable");
        assert!(sentences_from_value(&parse("{}").unwrap()).unwrap().is_empty());
        assert!(sentences_from_value(&parse("[]").unwrap()).unwrap().is_empty());
    }

    #[test]
    fn test_needs_review_omitted_when_false() {
        let s = Sentence::new(3, 0.5, 1.0, "x");
        assert_eq!(
            dumps(&s.to_value(), false),
            r#"{"id": 3, "start": 0.5, "end": 1.0, "text": "x", "words": [], "speaker": null}"#
        );
    }
}
