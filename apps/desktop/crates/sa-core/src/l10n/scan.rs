//! Catalog-completeness scanner (win-core §12 `CatalogCompletenessTests`, UNIFIED.md §3.18).
//!
//! Finds every catalog key used in Rust and Slint sources so a test can assert that each one has `tr` **and** `en`
//! in a single catalog of the platform chain. Recognized forms (string literals only; comment lines are skipped):
//!
//! * Rust app strings: `l("…")`, `l!("…", …)`, `k("…")`, `LocalizedMessage::key("…")`, `localize(lang, "…"` and
//!   `localize_in(chain, lang, "…"`.
//! * Rust engine strings (EngineKit chain): `engine_string("…"`, `engine_string!("…"`.
//! * Slint: `Loc.t("…")`, `Loc.t1("…", …)`, `Loc.t2("…", …)`.

use std::sync::OnceLock;

use regex::Regex;

/// Which chain a scanned key must resolve in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyScope {
    /// App strings (`L()`): the platform chain.
    App,
    /// Engine-client strings: the EngineKit catalog.
    Engine,
}

/// A key found in a source file.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ScannedKey {
    pub key: String,
    pub scope: KeyScope,
    pub line: usize,
}

const LIT: &str = r#""((?:[^"\\]|\\.)*)""#;

fn rust_app() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(&format!(
            r#"(?:\b(?:l|k)!?\(\s*|LocalizedMessage::key\(\s*|\blocalize\([^,()]*,\s*|\blocalize_in\([^,()]*,[^,()]*,\s*){LIT}"#
        ))
        .expect("valid regex")
    })
}

fn rust_engine() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(&format!(r#"\bengine_string!?\(\s*{LIT}"#)).expect("valid regex"))
}

fn slint_app() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(&format!(r#"\bLoc\.t[12]?\(\s*{LIT}"#)).expect("valid regex"))
}

/// Scans Rust source text.
pub fn scan_rust(source: &str) -> Vec<ScannedKey> {
    scan(source, &[(rust_app(), KeyScope::App), (rust_engine(), KeyScope::Engine)])
}

/// Scans Slint source text.
pub fn scan_slint(source: &str) -> Vec<ScannedKey> {
    scan(source, &[(slint_app(), KeyScope::App)])
}

fn scan(source: &str, patterns: &[(&Regex, KeyScope)]) -> Vec<ScannedKey> {
    let mut out = Vec::new();
    for (index, line) in source.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with('*') {
            continue;
        }
        for (re, scope) in patterns {
            for cap in re.captures_iter(line) {
                if let Some(m) = cap.get(1) {
                    out.push(ScannedKey { key: unescape(m.as_str()), scope: *scope, line: index + 1 });
                }
            }
        }
    }
    out
}

/// Unescapes Rust/Slint string literal escapes (`\"`, `\\`, `\n`, `\t`, `\r`, `\u{…}`).
pub fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('0') => out.push('\0'),
            Some('u') => {
                if chars.peek() == Some(&'{') {
                    chars.next();
                    let hex: String = chars.by_ref().take_while(|c| *c != '}').collect();
                    if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                        out.push(ch);
                    }
                } else {
                    out.push('u');
                }
            }
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}
