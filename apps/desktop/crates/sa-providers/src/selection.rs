//! Toolbar translation-model selection (win-providers §7, mac-ui §6.6 / mac-viewmodels §9).

use uuid::Uuid;

use crate::{fold_case, fold_case_mac};

/// One row of the picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslationModelChoice {
    pub id: String,
    pub title: String,
}

/// Models offered for a provider (`None` = on-device catalog models titled `ModelLabel::menu_title`).
/// Provider: discovered models if any else `available_models`, + its model; trimmed, ci-distinct, ci-sorted.
/// Never mixes in the LAN settings or another provider's models. Uses this platform's ordering rules.
pub fn provider_models(provider: &crate::TranslationProviderProfile) -> Vec<TranslationModelChoice> {
    provider_models_for(provider, cfg!(target_os = "macos"))
}

/// [`provider_models`] with an explicit platform: macOS (`mac = true`) builds `available_models` model-first and sorts
/// by lowercase (`caseInsensitiveCompare`); Windows sorts by .NET `OrdinalIgnoreCase` (uppercase, UTF-16 order).
/// Both sorts are stable, so ties keep their first-seen order.
pub fn provider_models_for(provider: &crate::TranslationProviderProfile, mac: bool) -> Vec<TranslationModelChoice> {
    let mut source: Vec<String> = if provider.discovered_models.is_empty() {
        provider.available_models(mac)
    } else {
        provider.discovered_models.clone()
    };
    source.push(provider.model.clone());
    let fold: fn(&str) -> String = if mac { fold_case_mac } else { fold_case };
    let mut seen = std::collections::HashSet::new();
    let mut ids: Vec<String> = source
        .iter()
        .map(|m| m.trim())
        .filter(|m| !m.is_empty() && seen.insert(fold(m)))
        .map(str::to_owned)
        .collect();
    if mac {
        ids.sort_by_key(|a| a.to_lowercase());
    } else {
        ids.sort_by(|a, b| fold_case(a).encode_utf16().cmp(fold_case(b).encode_utf16()));
    }
    ids.into_iter().map(|id| TranslationModelChoice { title: id.clone(), id }).collect()
}

/// A stale picker must not switch to on-device: an unknown `Some(id)` → false.
pub fn can_select_provider(store: &dyn crate::ProviderStore, provider_id: Option<Uuid>) -> bool {
    match provider_id {
        None => true,
        Some(id) => store.providers().iter().any(|p| p.id == id),
    }
}

/// `model_id` (exact) is offered for that provider; `None` (on-device) checks `on_device_ids`. A removed provider
/// offers nothing.
pub fn can_select_model(
    store: &dyn crate::ProviderStore,
    provider_id: Option<Uuid>,
    model_id: &str,
    on_device_ids: &[&str],
) -> bool {
    match provider_id {
        None => on_device_ids.contains(&model_id),
        Some(id) => store
            .providers()
            .iter()
            .find(|p| p.id == id)
            .is_some_and(|p| provider_models(p).iter().any(|c| c.id == model_id)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TranslationProviderProfile;

    fn profile(preset: &str, model: &str, discovered: &[&str]) -> TranslationProviderProfile {
        TranslationProviderProfile {
            id: Uuid::new_v4(),
            name: "x".into(),
            preset_id: preset.into(),
            base_url: "https://x.test/v1".into(),
            model: model.into(),
            async_translation: true,
            discovered_models: discovered.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    fn ids(v: Vec<TranslationModelChoice>) -> Vec<String> {
        v.into_iter().map(|c| c.id).collect()
    }

    #[test]
    fn discovered_trimmed_distinct_sorted() {
        let p = profile("openrouter", "cloud-a", &["cloud-b", "cloud-a", " cloud-b "]);
        assert_eq!(ids(provider_models_for(&p, false)), vec!["cloud-a", "cloud-b"]);
        assert_eq!(ids(provider_models_for(&p, true)), vec!["cloud-a", "cloud-b"]);
    }

    #[test]
    fn without_discovered_uses_available_models() {
        let p = profile("openai", "zeta", &[]);
        assert_eq!(ids(provider_models_for(&p, false)), vec!["gpt-4.1-mini", "gpt-4.1-nano", "gpt-4o-mini", "zeta"]);
        let lan = profile("lan", "", &[]);
        assert_eq!(ids(provider_models_for(&lan, true)), vec!["gemma4:e4b", "qwen3.5:4b"]);
        let custom = profile("custom", "", &[]);
        assert!(provider_models_for(&custom, false).is_empty());
    }

    #[test]
    fn titles_are_the_ids() {
        let p = profile("custom", "m", &[]);
        assert_eq!(provider_models_for(&p, false), vec![TranslationModelChoice { id: "m".into(), title: "m".into() }]);
    }
}
