//! Apple `JSONEncoder` compatible writer (mac-librarykit §2.7): `.sortedKeys`, `.withoutEscapingSlashes`, raw
//! UTF-8, integers as integers, and the `.prettyPrinted` layout (2-space indent, `"key" : value`, empty containers
//! as `[\n\n<indent>]` / `{\n\n<indent>}`).
//!
//! Key order: Foundation's `JSONWriter` sorts `.sortedKeys` with `compare(_:options: [.numeric, .caseInsensitive,
//! .forcedOrdering])`, not by bytes, so `rememberedUsername` comes before `rememberMe` (see [`apple_key_cmp`]).

use serde_json::Value;

/// Compact, sorted keys (the vault plaintext).
pub fn compact(value: &Value) -> String {
    let mut out = String::new();
    write(value, &mut out, None, 0);
    out
}

/// Pretty, sorted keys (`profiles.json`).
pub fn pretty(value: &Value) -> String {
    let mut out = String::new();
    write(value, &mut out, Some(2), 0);
    out
}

/// A JSON number for a double: non-finite values become 0 (never written by the store anyway).
pub fn number(x: f64) -> Value {
    serde_json::Number::from_f64(if x.is_finite() { x } else { 0.0 }).map(Value::Number).unwrap_or(Value::from(0))
}

fn write(value: &Value, out: &mut String, indent: Option<usize>, depth: usize) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => write_number(n, out),
        Value::String(s) => write_string(s, out),
        Value::Array(items) => {
            out.push('[');
            match indent {
                None => {
                    for (k, item) in items.iter().enumerate() {
                        if k > 0 {
                            out.push(',');
                        }
                        write(item, out, None, depth + 1);
                    }
                }
                Some(step) => {
                    if items.is_empty() {
                        out.push_str("\n\n");
                        pad(out, step * depth);
                    } else {
                        for (k, item) in items.iter().enumerate() {
                            out.push_str(if k > 0 { ",\n" } else { "\n" });
                            pad(out, step * (depth + 1));
                            write(item, out, indent, depth + 1);
                        }
                        out.push('\n');
                        pad(out, step * depth);
                    }
                }
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_by(|a, b| apple_key_cmp(a.0, b.0));
            out.push('{');
            match indent {
                None => {
                    for (k, (key, item)) in entries.iter().enumerate() {
                        if k > 0 {
                            out.push(',');
                        }
                        write_string(key, out);
                        out.push(':');
                        write(item, out, None, depth + 1);
                    }
                }
                Some(step) => {
                    if entries.is_empty() {
                        out.push_str("\n\n");
                        pad(out, step * depth);
                    } else {
                        for (k, (key, item)) in entries.iter().enumerate() {
                            out.push_str(if k > 0 { ",\n" } else { "\n" });
                            pad(out, step * (depth + 1));
                            write_string(key, out);
                            out.push_str(" : ");
                            write(item, out, indent, depth + 1);
                        }
                        out.push('\n');
                        pad(out, step * depth);
                    }
                }
            }
            out.push('}');
        }
    }
}

/// `String.compare(_:options: [.numeric, .caseInsensitive, .forcedOrdering])` as `JSONWriter` uses it for
/// `.sortedKeys`: runs of ASCII digits compare by numeric value, everything else by lowercased scalars; when the
/// two keys are equal under those rules, the literal scalars decide (`.forcedOrdering`).
pub fn apple_key_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let x: Vec<char> = a.chars().collect();
    let y: Vec<char> = b.chars().collect();
    let (mut i, mut j) = (0, 0);
    while i < x.len() && j < y.len() {
        if x[i].is_ascii_digit() && y[j].is_ascii_digit() {
            let (si, sj) = (i, j);
            while i < x.len() && x[i].is_ascii_digit() {
                i += 1;
            }
            while j < y.len() && y[j].is_ascii_digit() {
                j += 1;
            }
            let da: String = x[si..i].iter().skip_while(|c| **c == '0').collect();
            let db: String = y[sj..j].iter().skip_while(|c| **c == '0').collect();
            match da.len().cmp(&db.len()).then_with(|| da.cmp(&db)) {
                Ordering::Equal => continue,
                other => return other,
            }
        }
        let ca: String = x[i].to_lowercase().collect();
        let cb: String = y[j].to_lowercase().collect();
        match ca.cmp(&cb) {
            Ordering::Equal => {
                i += 1;
                j += 1;
            }
            other => return other,
        }
    }
    match (x.len() - i).cmp(&(y.len() - j)) {
        Ordering::Equal => a.cmp(b),
        other => other,
    }
}

fn pad(out: &mut String, n: usize) {
    for _ in 0..n {
        out.push(' ');
    }
}

fn write_number(n: &serde_json::Number, out: &mut String) {
    if let Some(i) = n.as_i64() {
        out.push_str(&i.to_string());
    } else if let Some(u) = n.as_u64() {
        out.push_str(&u.to_string());
    } else {
        let x = n.as_f64().unwrap_or(0.0);
        // Rust's `Display` for f64 is the shortest round-trip form and writes integral values without a fraction.
        out.push_str(&format!("{x}"));
    }
}

fn write_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keys_sort_like_foundation_numeric_case_insensitive() {
        let v = json!({"version": 2, "rememberMe": true, "rememberedUsername": "a", "profiles": [], "lastUsername": "a"});
        assert_eq!(
            compact(&v),
            r#"{"lastUsername":"a","profiles":[],"rememberedUsername":"a","rememberMe":true,"version":2}"#
        );
        let mut keys = vec!["fp:10:a", "fp:9:a", "b", "B", "a", "fp:09:a"];
        keys.sort_by(|a, b| apple_key_cmp(a, b));
        assert_eq!(keys, ["a", "B", "b", "fp:09:a", "fp:9:a", "fp:10:a"]);
    }

    #[test]
    fn writes_the_apple_layout() {
        let v = json!({"b": [1, 2.5, 42.0], "a": "x/y ş", "e": [], "o": {}, "n": {"z": true, "c": null}});
        assert_eq!(compact(&v), r#"{"a":"x/y ş","b":[1,2.5,42],"e":[],"n":{"c":null,"z":true},"o":{}}"#);
        assert_eq!(
            pretty(&v),
            "{\n  \"a\" : \"x/y ş\",\n  \"b\" : [\n    1,\n    2.5,\n    42\n  ],\n  \"e\" : [\n\n  ],\n  \"n\" : {\n    \"c\" : null,\n    \"z\" : true\n  },\n  \"o\" : {\n\n  }\n}"
        );
        assert_eq!(compact(&json!("a\"\\\n\u{1}")), r#""a\"\\\n\u0001""#);
    }
}
