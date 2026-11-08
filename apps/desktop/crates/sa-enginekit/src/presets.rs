//! Translation provider presets (mac-enginekit §8 = win-providers §1.1). One table for both platforms.

use sa_core::l10n::{UiLanguage, engine_string};

/// A compiled-in provider preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TranslationProviderPreset {
    pub id: &'static str,
    /// Brand name, or an EngineKit catalog key when `localizes_name`.
    pub raw_name: &'static str,
    /// `""` for `custom`.
    pub base_url: &'static str,
    pub requires_api_key: bool,
    pub suggested_models: &'static [&'static str],
    pub localizes_name: bool,
}

impl TranslationProviderPreset {
    pub const CUSTOM_ID: &'static str = "custom";
    pub const LAN_ID: &'static str = "lan";
    pub const OPENCODE_GO_BASE_URL: &'static str = "https://opencode.ai/inference/go/openai/v1";

    /// Display order; `custom` is last (the fallback).
    pub const ALL: [TranslationProviderPreset; 8] = [
        Self::p("opencode-go", "OpenCode Go", "https://opencode.ai/inference/go/openai/v1", true,
            &["glm-5.3-flash", "kimi-k3.1-pro", "minimax-m3.1-flash", "deepseek-v4.1-flash", "deepseek-v4-pro"], false),
        Self::p("openrouter", "OpenRouter", "https://openrouter.ai/api/v1", true,
            &["google/gemma-3-27b-it", "deepseek/deepseek-chat-v3-0324", "qwen/qwen-2.5-72b-instruct", "openai/gpt-4o-mini"], false),
        Self::p("openai", "OpenAI", "https://api.openai.com/v1", true, &["gpt-4o-mini", "gpt-4.1-mini", "gpt-4.1-nano"], false),
        Self::p("deepseek", "DeepSeek", "https://api.deepseek.com/v1", true, &["deepseek-chat"], false),
        Self::p("groq", "Groq", "https://api.groq.com/openai/v1", true, &["llama-3.3-70b-versatile", "gemma2-9b-it"], false),
        Self::p("gemini", "Google Gemini", "https://generativelanguage.googleapis.com/v1beta/openai", true,
            &["gemini-2.5-flash", "gemini-2.5-pro"], false),
        Self::p("lan", "Yerel Ağ (Ollama / LM Studio)", "http://127.0.0.1:11434", false, &["gemma4:e4b", "qwen3.5:4b"], true),
        Self::p("custom", "Özel (OpenAI Uyumlu)", "", true, &[], true),
    ];

    #[allow(clippy::too_many_arguments)]
    const fn p(
        id: &'static str,
        raw_name: &'static str,
        base_url: &'static str,
        requires_api_key: bool,
        suggested_models: &'static [&'static str],
        localizes_name: bool,
    ) -> Self {
        Self { id, raw_name, base_url, requires_api_key, suggested_models, localizes_name }
    }

    /// Case-insensitive id match, falling back to `custom` (None, `""`, unknown).
    pub fn preset(id: Option<&str>) -> &'static TranslationProviderPreset {
        id.and_then(|id| Self::ALL.iter().find(|p| p.id.eq_ignore_ascii_case(id))).unwrap_or(&Self::ALL[Self::ALL.len() - 1])
    }

    /// Localized through the EngineKit catalog when `localizes_name`.
    pub fn name(&self, language: Option<UiLanguage>) -> String {
        if self.localizes_name { engine_string(self.raw_name, language, &[]) } else { self.raw_name.to_owned() }
    }

    /// First suggested model or `""`.
    pub fn default_model(&self) -> &'static str {
        self.suggested_models.first().copied().unwrap_or("")
    }

    /// Only a LAN server translates in the background by default (macOS rule; Windows uses `true` for every new
    /// provider through `PlatformPolicy::new_provider_async_default`).
    pub fn translates_in_background_by_default(&self) -> bool {
        self.id == Self::LAN_ID
    }
}
