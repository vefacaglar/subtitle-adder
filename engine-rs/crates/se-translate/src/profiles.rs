//! Per-model prompt/sampling profiles (port of `translation_profiles.py`, spec translate-local.md §9).
//!
//! Model-specific translation conventions, independent of the serving backend. Names cover common Ollama
//! tags and HF/GGUF paths; unknown models keep the generic subtitle prompt.

use se_core::pyfmt::float_repr;
use se_core::{Map, Value};

use crate::llm::ChatMessage;

/// `LANGUAGE_NAMES` (English names used in prompts).
pub const LANGUAGE_NAMES: &[(&str, &str)] = &[
    ("en", "English"),
    ("tr", "Turkish"),
    ("pt", "Portuguese"),
    ("es", "Spanish"),
    ("ru", "Russian"),
    ("de", "German"),
    ("fr", "French"),
    ("ja", "Japanese"),
    ("zh", "Chinese"),
];

/// `_CHINESE_NAMES` (HY-MT Chinese prompt names).
pub const CHINESE_NAMES: &[(&str, &str)] = &[
    ("en", "英语"),
    ("tr", "土耳其语"),
    ("pt", "葡萄牙语"),
    ("es", "西班牙语"),
    ("ru", "俄语"),
    ("de", "德语"),
    ("fr", "法语"),
    ("ja", "日语"),
    ("zh", "中文"),
];

/// `TranslationProfile` (frozen dataclass; field order matters for [`TranslationProfile::py_repr`], which is
/// part of the checkpoint hash).
#[derive(Clone, Debug, PartialEq)]
pub struct TranslationProfile {
    pub name: String,
    pub single_user: bool,
    pub disable_thinking: bool,
    pub soft_no_think: bool,
    pub temperature: f64,
    pub top_p: f64,
    pub top_k: Option<i64>,
    pub repetition_penalty: Option<f64>,
}

impl Default for TranslationProfile {
    fn default() -> Self {
        Self {
            name: "generic".to_string(),
            single_user: false,
            disable_thinking: false,
            soft_no_think: false,
            temperature: 0.2,
            top_p: 0.9,
            top_k: None,
            repetition_penalty: None,
        }
    }
}

impl TranslationProfile {
    fn named(name: &str) -> Self {
        Self { name: name.to_string(), ..Self::default() }
    }

    /// HY-MT's documented translation sampling.
    fn hy_mt(name: &str) -> Self {
        Self {
            single_user: true,
            temperature: 0.7,
            top_p: 0.6,
            top_k: Some(20),
            repetition_penalty: Some(1.05),
            ..Self::named(name)
        }
    }

    /// Python `repr(profile)` (= `str(profile)`), e.g. `TranslationProfile(name='gemma4', single_user=False, ...)`.
    pub fn py_repr(&self) -> String {
        fn py_bool(b: bool) -> &'static str {
            if b { "True" } else { "False" }
        }
        format!(
            "TranslationProfile(name={}, single_user={}, disable_thinking={}, soft_no_think={}, temperature={}, top_p={}, top_k={}, repetition_penalty={})",
            se_core::pyfmt::repr_str(&self.name),
            py_bool(self.single_user),
            py_bool(self.disable_thinking),
            py_bool(self.soft_no_think),
            float_repr(self.temperature),
            float_repr(self.top_p),
            self.top_k.map_or_else(|| "None".to_string(), |k| k.to_string()),
            self.repetition_penalty.map_or_else(|| "None".to_string(), float_repr),
        )
    }

    /// `chat_messages(system, user)`.
    pub fn chat_messages(&self, system: &str, user: &str) -> Vec<ChatMessage> {
        let mut user = user.to_string();
        if self.soft_no_think {
            user.push_str("\n/no_think");
        }
        if self.single_user {
            return vec![ChatMessage { role: "user".to_string(), content: format!("{system}\n\n{user}") }];
        }
        vec![
            ChatMessage { role: "system".to_string(), content: system.to_string() },
            ChatMessage { role: "user".to_string(), content: user },
        ]
    }

    /// `generation_options(ollama=...)` as a JSON object (key order as in Python).
    pub fn generation_options(&self, ollama: bool) -> Value {
        let mut sampling = Map::new().with("temperature", self.temperature).with("top_p", self.top_p);
        // A model name does not identify an OpenAI-compatible serving backend: keep its sampler fields
        // portable and add the extensions only for Ollama.
        if ollama {
            if let Some(k) = self.top_k {
                sampling.insert("top_k", Value::Int(k));
            }
            if let Some(p) = self.repetition_penalty {
                sampling.insert("repeat_penalty", p);
            }
            sampling.insert("num_predict", Value::Int(1536));
            let mut options = Map::new().with("options", sampling);
            if self.disable_thinking {
                options.insert("think", false);
            }
            return Value::Object(options);
        }
        sampling.insert("max_tokens", Value::Int(1536));
        if self.disable_thinking {
            sampling.insert("chat_template_kwargs", Map::new().with("enable_thinking", false));
        }
        Value::Object(sampling)
    }
}

/// Python `re` `\s` (str pattern): Unicode whitespace as `str.isspace()`.
fn is_re_space(c: char) -> bool {
    se_core::pyfmt::isspace(c)
}

/// `(?=[{set}\s-]|$)` at byte offset `at` of `name` (`$` also matches before a final `\n`, which is `\s`).
fn lookahead_ok(name: &str, at: usize, set: &str) -> bool {
    match name[at..].chars().next() {
        None => true,
        Some(c) => set.contains(c) || is_re_space(c) || c == '-',
    }
}

/// `re.search(r"{word}[-_\s]?(?:{tails})(?=[{set}\s-]|$)", name)`, backtracking over the optional separator
/// and over `tails` in order.
fn search_family(name: &str, word: &str, tails: &[&str], set: &str) -> bool {
    let mut from = 0;
    while let Some(pos) = name[from..].find(word) {
        let after = from + pos + word.len();
        let mut starts = Vec::with_capacity(2);
        if let Some(c) = name[after..].chars().next() {
            if c == '-' || c == '_' || is_re_space(c) {
                starts.push(after + c.len_utf8());
            }
        }
        starts.push(after);
        for &s in &starts {
            for tail in tails {
                if name[s..].starts_with(tail) && lookahead_ok(name, s + tail.len(), set) {
                    return true;
                }
            }
        }
        from += pos + word.chars().next().map_or(1, char::len_utf8);
    }
    false
}

/// `profile_for_model(model)` (first match wins).
pub fn profile_for_model(model: &str) -> TranslationProfile {
    let name = crate::pyunicode::casefold(model);
    let compact: String = name.chars().filter(|&c| !(c == '-' || c == '_' || is_re_space(c))).collect();
    if compact.contains("translategemma") {
        return TranslationProfile { single_user: true, ..TranslationProfile::named("translategemma") };
    }
    if compact.contains("hymt1.5") {
        return TranslationProfile::hy_mt("hy-mt1.5");
    }
    if compact.contains("hunyuanmt7b") && !name.contains("chimera") {
        return TranslationProfile::hy_mt("hunyuan-mt");
    }
    if search_family(&name, "gemma", &["4"], ":/._") {
        return TranslationProfile { disable_thinking: true, ..TranslationProfile::named("gemma4") };
    }
    if search_family(&name, "gemma", &["3n", "3", "2"], ":/._") {
        return TranslationProfile { single_user: true, ..TranslationProfile::named("gemma-legacy") };
    }
    if search_family(&name, "qwen", &["3.5"], ":/_") {
        return TranslationProfile { disable_thinking: true, ..TranslationProfile::named("qwen3.5") };
    }
    if search_family(&name, "qwen", &["3"], ":/_")
        && !["thinking", "instruct", "next", "vl"].iter().any(|v| name.contains(v))
    {
        return TranslationProfile {
            disable_thinking: true,
            soft_no_think: true,
            ..TranslationProfile::named("qwen3")
        };
    }
    TranslationProfile::default()
}

/// `LANGUAGE_NAMES[code]` / `_CHINESE_NAMES[code]`. Python raises KeyError for an unknown code, which is
/// unreachable after `check_request`; the code itself is used instead.
fn table_get<'a>(table: &'a [(&'a str, &'a str)], code: &'a str) -> &'a str {
    table.iter().find(|(c, _)| *c == code).map_or(code, |(_, name)| *name)
}

/// `build_hy_mt_prompt(lines, target, source, context, legacy)`: HY-MT's translation-only template and
/// 1.5's optional reference context.
pub fn build_hy_mt_prompt(lines: &[String], target: &str, source: Option<&str>, context: Option<&[String]>, legacy: bool) -> String {
    let numbered = lines
        .iter()
        .enumerate()
        .map(|(i, text)| format!("[{}] {text}", i + 1))
        .collect::<Vec<_>>()
        .join("\n");
    if let Some(context) = context.filter(|c| !c.is_empty()) {
        let reference = context.join("\n");
        return format!(
            "{reference}\n参考上面的信息，把下面的文本翻译成{}，注意不需要翻译上文，也不要额外解释：\n{numbered}",
            table_get(CHINESE_NAMES, target)
        );
    }
    if source == Some("zh") || target == "zh" {
        if legacy {
            return format!("把下面的文本翻译成{}，不要额外解释。\n\n{numbered}", table_get(CHINESE_NAMES, target));
        }
        return format!(
            "将以下文本翻译为{}，注意只需要输出翻译后的结果，不要额外解释：\n\n{numbered}",
            table_get(CHINESE_NAMES, target)
        );
    }
    format!(
        "Translate the following segment into {}, without additional explanation.\n\n{numbered}",
        table_get(LANGUAGE_NAMES, target)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_family_backtracks_over_separator_and_alternatives() {
        assert!(search_family("gemma-4", "gemma", &["4"], ":/._"));
        assert!(search_family("gemma4", "gemma", &["4"], ":/._"));
        assert!(!search_family("gemma40", "gemma", &["4"], ":/._"));
        assert!(search_family("x gemma 3n:e4b", "gemma", &["3n", "3", "2"], ":/._"));
        assert!(search_family("gemmagemma3", "gemma", &["3"], ":/._"));
        assert!(!search_family("qwen3.5", "qwen", &["3"], ":/_"));
        assert!(search_family("qwen3\n", "qwen", &["3"], ":/_"));
        assert!(search_family("qwen3", "qwen", &["3"], ":/_"));
    }
}
