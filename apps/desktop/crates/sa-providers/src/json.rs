//! Pretty JSON writers matching the two existing apps' encoders (diffs with their files stay minimal):
//!
//! * [`Style::CSharp`]: System.Text.Json `WriteIndented` on Windows — 2 spaces, `"key": value`, CRLF, `[]`/`{}` for
//!   empties, default encoder escaping (every non-ASCII char and `<>&'"+\`` as `\uXXXX`, uppercase hex). Keys in
//!   insertion order.
//! * [`Style::Apple`]: Foundation `JSONEncoder` with `.prettyPrinted, .sortedKeys, .withoutEscapingSlashes` —
//!   2 spaces, `"key" : value`, LF, keys sorted, raw UTF-8, `/` unescaped, empties as `[\n\n<indent>]`.

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Style {
    CSharp,
    Apple,
}

pub(crate) fn pretty(value: &Value, style: Style) -> String {
    let mut out = String::new();
    write_value(&mut out, value, style, 0);
    out
}

fn newline(style: Style) -> &'static str {
    match style {
        Style::CSharp => "\r\n",
        Style::Apple => "\n",
    }
}

fn indent(out: &mut String, level: usize) {
    for _ in 0..level {
        out.push_str("  ");
    }
}

fn write_value(out: &mut String, value: &Value, style: Style, level: usize) {
    let nl = newline(style);
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::String(s) => write_string(out, s, style),
        Value::Array(items) => {
            if items.is_empty() {
                match style {
                    Style::CSharp => out.push_str("[]"),
                    Style::Apple => {
                        out.push('[');
                        out.push_str(nl);
                        out.push_str(nl);
                        indent(out, level);
                        out.push(']');
                    }
                }
                return;
            }
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(nl);
                indent(out, level + 1);
                write_value(out, item, style, level + 1);
            }
            out.push_str(nl);
            indent(out, level);
            out.push(']');
        }
        Value::Object(map) => {
            if map.is_empty() {
                match style {
                    Style::CSharp => out.push_str("{}"),
                    Style::Apple => {
                        out.push('{');
                        out.push_str(nl);
                        out.push_str(nl);
                        indent(out, level);
                        out.push('}');
                    }
                }
                return;
            }
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            if style == Style::Apple {
                entries.sort_by(|a, b| a.0.cmp(b.0));
            }
            out.push('{');
            for (i, (k, v)) in entries.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(nl);
                indent(out, level + 1);
                write_string(out, k, style);
                out.push_str(match style {
                    Style::CSharp => ": ",
                    Style::Apple => " : ",
                });
                write_value(out, v, style, level + 1);
            }
            out.push_str(nl);
            indent(out, level);
            out.push('}');
        }
    }
}

fn write_string(out: &mut String, s: &str, style: Style) {
    out.push('"');
    for c in s.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\\' => out.push_str("\\\\"),
            '"' if style == Style::Apple => out.push_str("\\\""),
            c if (c as u32) < 0x20 => push_u(out, c as u32),
            c if style == Style::CSharp && (!c.is_ascii() || matches!(c, '"' | '<' | '>' | '&' | '\'' | '+' | '`')) => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    push_u(out, *unit as u32);
                }
            }
            c if c == '\u{7f}' && style == Style::CSharp => push_u(out, 0x7f),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn push_u(out: &mut String, unit: u32) {
    out.push_str(&format!("\\u{unit:04X}"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn csharp_shape() {
        let v = json!({"activeProviderId": null, "providers": [{"name": "Yerel Ağ <x>", "discoveredModels": []}]});
        let text = pretty(&v, Style::CSharp);
        assert_eq!(
            text,
            "{\r\n  \"activeProviderId\": null,\r\n  \"providers\": [\r\n    {\r\n      \"name\": \"Yerel A\\u011F \\u003Cx\\u003E\",\r\n      \"discoveredModels\": []\r\n    }\r\n  ]\r\n}"
        );
        assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), v);
    }

    #[test]
    fn apple_shape() {
        let v = json!({"providers": [], "activeProviderId": null, "b": {"z": "https://a/b", "a": "İkinci \"q\""}});
        let text = pretty(&v, Style::Apple);
        assert_eq!(
            text,
            "{\n  \"activeProviderId\" : null,\n  \"b\" : {\n    \"a\" : \"İkinci \\\"q\\\"\",\n    \"z\" : \"https://a/b\"\n  },\n  \"providers\" : [\n\n  ]\n}"
        );
        assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), v);
    }

    #[test]
    fn emoji_is_escaped_as_surrogates_in_csharp() {
        assert_eq!(pretty(&json!("😀"), Style::CSharp), "\"\\uD83D\\uDE00\"");
        assert_eq!(pretty(&json!("a\u{1}"), Style::Apple), "\"a\\u0001\"");
    }
}
