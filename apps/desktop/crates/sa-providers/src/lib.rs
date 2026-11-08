//! `sa-providers`: translation providers and their API keys (win-providers, mac-librarykit §13–§14,
//! UNIFIED.md §3.13, §5.2).
//!
//! * [`TranslationProviderProfile`] + two file formats behind [`ProviderStore`]: [`win::WinProviderManager`]
//!   (C# shape, CRLF, case-insensitive read, LAN settings bridge, broker `providers` topic) and
//!   [`mac::MacProviderStore`] (sorted keys, uppercase UUIDs, legacy LAN migration).
//! * API keys are **never** in `translation-providers.json` any more: [`secrets::EncryptedSecretStore`] keeps them
//!   AES-256-GCM encrypted in `translation-provider-keys.json`, the data key protected by DPAPI (Windows,
//!   `translation-provider-keys.dpapi`) or a 0600 key file (macOS, `translation-provider-keys.key`). No Keychain.
//!   Windows migrates a C# plaintext `apiKey` into the sidecar on load, then rewrites the JSON with `"apiKey": ""`.

use sa_enginekit::presets::TranslationProviderPreset;
use sa_enginekit::launch::ExternalLlm;
use sa_core::subscription::Subscription;
use uuid::Uuid;

mod fileio;
mod json;
pub mod secrets;
pub mod selection;

#[cfg(windows)]
pub mod win;

/// Compiled on macOS, and in this crate's tests on every platform (the store is portable Rust).
#[cfg(any(target_os = "macos", test))]
pub mod mac;

/// A saved provider (both formats; the Windows file's `apiKey` field is read for migration only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslationProviderProfile {
    pub id: Uuid,
    pub name: String,
    pub preset_id: String,
    pub base_url: String,
    pub model: String,
    pub async_translation: bool,
    pub discovered_models: Vec<String>,
}

impl TranslationProviderProfile {
    /// A new profile from a preset (name in the current language, preset default model, async per
    /// `PlatformPolicy::new_provider_async_default`).
    pub fn from_preset(preset: &TranslationProviderPreset, async_translation: bool) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: preset.name(None),
            preset_id: preset.id.to_owned(),
            base_url: preset.base_url.to_owned(),
            model: preset.default_model().to_owned(),
            async_translation,
            discovered_models: Vec::new(),
        }
    }

    pub fn preset(&self) -> &'static TranslationProviderPreset {
        TranslationProviderPreset::preset(Some(&self.preset_id))
    }

    pub fn is_lan(&self) -> bool {
        self.preset().id == TranslationProviderPreset::LAN_ID
    }

    /// Platform order differs (win-providers §1.2 vs mac-librarykit §14.1); `model_first` = macOS order.
    ///
    /// * Windows (`model_first = false`): discovered models, then the preset suggestions (only when nothing was
    ///   discovered or the preset is not `lan`), then the chosen model inserted at index 0 if it is not listed yet.
    /// * macOS (`model_first = true`): the chosen model, then discovered, then the suggestions under the same rule.
    ///
    /// Every entry is trimmed, blanks are dropped and case-insensitive duplicates keep their first spelling.
    ///
    /// The `lan` test is on the stored id as is (C# `string.Equals(PresetId, "lan", OrdinalIgnoreCase)`, Swift
    /// `preset.id != "lan"`; neither trims). Identity folds per platform (.NET `OrdinalIgnoreCase`, Swift
    /// `lowercased()`).
    pub fn available_models(&self, model_first: bool) -> Vec<String> {
        let fold: fn(&str) -> String = if model_first { fold_case_mac } else { fold_case };
        let mut seen = std::collections::HashSet::new();
        let mut result: Vec<String> = Vec::new();
        let mut add = |m: &str, result: &mut Vec<String>| {
            let t = m.trim();
            if !t.is_empty() && seen.insert(fold(t)) {
                result.push(t.to_owned());
            }
        };
        if model_first {
            add(&self.model, &mut result);
        }
        for m in &self.discovered_models {
            add(m, &mut result);
        }
        if self.discovered_models.is_empty() || !self.preset_id.eq_ignore_ascii_case(TranslationProviderPreset::LAN_ID) {
            for m in self.preset().suggested_models {
                add(m, &mut result);
            }
        }
        if !model_first {
            let t = self.model.trim();
            if !t.is_empty() && seen.insert(fold(t)) {
                result.insert(0, t.to_owned());
            }
        }
        result
    }

    /// The secret-store account: `translation-provider.<UUID upper-case with dashes>`.
    pub fn secret_account(id: Uuid) -> String {
        format!("translation-provider.{}", id.hyphenated().to_string().to_uppercase())
    }
}

/// .NET `OrdinalIgnoreCase` folding (Windows): the simple one-to-one uppercase mapping per character, except that a
/// non-ASCII character never folds into ASCII (`ı` U+0131 and `ſ` U+017F stay themselves, as in .NET's ordinal
/// casing table; verified against the C# code by `tests/csharp_behavior.rs`).
pub(crate) fn fold_case(s: &str) -> String {
    s.chars()
        .map(|c| {
            let mut up = c.to_uppercase();
            match (up.next(), up.next()) {
                (Some(u), None) if c.is_ascii() || !u.is_ascii() => u,
                _ => c,
            }
        })
        .collect()
}

/// Swift `lowercased()` folding (macOS identity of model names).
pub(crate) fn fold_case_mac(s: &str) -> String {
    s.to_lowercase()
}

/// System.Text.Json's default `MaxDepth` (64): a document nested deeper fails to parse in the C# app.
#[cfg(windows)]
pub(crate) const DOTNET_MAX_JSON_DEPTH: usize = 64;

/// Whether `text` stays within `max` nested arrays/objects (strings skipped). Cheap pre-check so a document the C#
/// parser rejects for its depth is rejected here too (serde_json allows 128).
#[cfg(windows)]
pub(crate) fn json_depth_within(text: &str, max: usize) -> bool {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for b in text.bytes() {
        if in_string {
            match (escaped, b) {
                (true, _) => escaped = false,
                (false, b'\\') => escaped = true,
                (false, b'"') => in_string = false,
                _ => {}
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'[' | b'{' => {
                depth += 1;
                if depth > max {
                    return false;
                }
            }
            b']' | b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    true
}

/// `"••••"`, `"••••" + last4`, `first3 + "••••" + last4`; `None` for a blank key (UI shows `Anahtar yok`).
pub fn mask_key(key: &str) -> Option<String> {
    let t = key.trim();
    let chars: Vec<char> = t.chars().collect();
    let n = chars.len();
    if n == 0 {
        return None;
    }
    let bullets = "\u{2022}\u{2022}\u{2022}\u{2022}";
    let last4: String = chars[n.saturating_sub(4)..].iter().collect();
    Some(if n <= 4 {
        bullets.to_owned()
    } else if n <= 8 {
        format!("{bullets}{last4}")
    } else {
        let first3: String = chars[..3].iter().collect();
        format!("{first3}{bullets}{last4}")
    })
}

/// Reading a provider key.
#[derive(Clone, PartialEq, Eq)]
pub enum ProviderKeyRead {
    /// Raw (untrimmed) key.
    Found(sa_core::secrets::SecretString),
    Absent,
    /// Never treated as absent: an empty field keeps the stored key.
    Unavailable,
}

impl std::fmt::Debug for ProviderKeyRead {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ProviderKeyRead::Found(_) => "Found([redacted])",
            ProviderKeyRead::Absent => "Absent",
            ProviderKeyRead::Unavailable => "Unavailable",
        })
    }
}

/// Active provider mapped for the engine launch state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveExternalLlm {
    pub llm: ExternalLlm,
    pub async_translation: bool,
}

/// Load problems surfaced in Settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProviderLoadProblem {
    Unreadable,
    Damaged,
}

/// What changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProviderChange {
    Providers,
    Active,
    Keys,
    /// Reloaded from another instance.
    External,
}

/// The provider store contract shared by both formats. UI thread; key I/O on background lanes.
pub trait ProviderStore: Send + Sync {
    fn providers(&self) -> Vec<TranslationProviderProfile>;
    fn active_provider_id(&self) -> Option<Uuid>;
    fn active_provider(&self) -> Option<TranslationProviderProfile> {
        let id = self.active_provider_id()?;
        self.providers().into_iter().find(|p| p.id == id)
    }
    fn load_problem(&self) -> Option<ProviderLoadProblem>;
    /// Adds (or replaces the same id), optionally storing a key and activating it.
    fn add(&self, profile: TranslationProviderProfile, key: Option<&str>, set_active: bool) -> TranslationProviderProfile;
    /// `key = None` keeps the stored key; `Some("")` removes it. Unknown id → false.
    fn update(&self, profile: TranslationProviderProfile, key: Option<&str>) -> bool;
    /// Always removes the provider's stored key.
    fn remove(&self, id: Uuid);
    fn set_active(&self, id: Option<Uuid>);
    fn update_model(&self, id: Uuid, model: &str);
    /// Only after a successful connection test.
    fn set_discovered_models(&self, id: Uuid, models: &[String]);
    /// Replaces (blank removes) the key of an existing provider. False for an unknown id or when the secret store
    /// refused the write (the UI then shows `API anahtarı kaydedilemedi. Yeniden girip tekrar dene.`).
    fn set_key(&self, id: Uuid, key: &str) -> bool;
    /// Blocking key read (background lane).
    fn read_key(&self, id: Uuid) -> ProviderKeyRead;
    /// Active provider for `EngineLaunchState` (key read from the secret store).
    fn active_external_llm(&self) -> Option<ActiveExternalLlm>;
    fn subscribe(&self, handler: Box<dyn Fn(ProviderChange) + Send + Sync>) -> Subscription;
}
