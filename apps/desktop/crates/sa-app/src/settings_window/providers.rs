//! Provider editor state (pure): the Windows inline add/edit form (win-ui-hub-settings §7.4–§7.5, with the FIX-W
//! "pristine" rule of UNIFIED.md §3.13), the macOS `ProviderDraft` (mac-viewmodels §6.5) and the key cache states
//! (mac-viewmodels §6.3).

use sa_enginekit::external_llm::ExternalLlmServer;
use sa_enginekit::presets::TranslationProviderPreset;
use sa_providers::TranslationProviderProfile;
use uuid::Uuid;
use zeroize::Zeroizing;

/// A provider key as the UI knows it (never shown in clear except in the Windows edit form's password box).
#[derive(Clone, PartialEq, Eq)]
pub enum ProviderKey {
    Loading,
    Absent,
    /// Trimmed key.
    Present(Zeroizing<String>),
    Unreadable,
}

impl std::fmt::Debug for ProviderKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ProviderKey::Loading => "Loading",
            ProviderKey::Absent => "Absent",
            ProviderKey::Present(_) => "Present([redacted])",
            ProviderKey::Unreadable => "Unreadable",
        })
    }
}

impl ProviderKey {
    pub fn from_read(read: sa_providers::ProviderKeyRead) -> Self {
        match read {
            sa_providers::ProviderKeyRead::Found(k) => {
                let t = k.expose().trim();
                if t.is_empty() { ProviderKey::Absent } else { ProviderKey::Present(Zeroizing::new(t.to_owned())) }
            }
            sa_providers::ProviderKeyRead::Absent => ProviderKey::Absent,
            sa_providers::ProviderKeyRead::Unavailable => ProviderKey::Unreadable,
        }
    }

    /// `Present` key text, else "".
    pub fn value(&self) -> &str {
        match self {
            ProviderKey::Present(k) => k.as_str(),
            _ => "",
        }
    }

    pub fn is_settled(&self) -> bool {
        matches!(self, ProviderKey::Absent | ProviderKey::Present(_))
    }

    /// Row caption: `Anahtar okunuyor…` / `Anahtar yok` / mask / `Anahtar okunamadı`.
    pub fn caption(&self) -> String {
        match self {
            ProviderKey::Loading => sa_core::l("Anahtar okunuyor…"),
            ProviderKey::Absent => sa_core::l("Anahtar yok"),
            ProviderKey::Present(k) => sa_providers::mask_key(k).unwrap_or_else(|| sa_core::l("Anahtar yok")),
            ProviderKey::Unreadable => sa_core::l("Anahtar okunamadı"),
        }
    }
}

/// Index of a preset in `TranslationProviderPreset::ALL` (case-insensitive id, unknown → 0 like the C# combo).
pub fn preset_index(id: &str) -> usize {
    TranslationProviderPreset::ALL.iter().position(|p| p.id.eq_ignore_ascii_case(id.trim())).unwrap_or(0)
}

pub fn preset_at(index: usize) -> &'static TranslationProviderPreset {
    &TranslationProviderPreset::ALL[index.min(TranslationProviderPreset::ALL.len() - 1)]
}

/// The preset names in combo order (localized now).
pub fn preset_names() -> Vec<String> {
    TranslationProviderPreset::ALL.iter().map(|p| p.name(None)).collect()
}

// ---------------------------------------------------------------------------------------------------------------
// Windows inline form
// ---------------------------------------------------------------------------------------------------------------

/// The Windows add/edit form card (`TranslationProvidersControl`).
#[derive(Debug, Clone, PartialEq)]
pub struct WinProviderForm {
    pub editing: Option<Uuid>,
    pub preset_index: usize,
    pub name: String,
    pub url: String,
    pub key: Zeroizing<String>,
    pub model: String,
    pub model_items: Vec<String>,
    pub async_translation: bool,
    /// Nothing typed since the preset defaults were applied (FIX-W: only then may a language switch re-apply them).
    pub pristine: bool,
    pub testing: bool,
}

impl Default for WinProviderForm {
    fn default() -> Self {
        let mut f = Self {
            editing: None,
            preset_index: 0,
            name: String::new(),
            url: String::new(),
            key: Zeroizing::new(String::new()),
            model: String::new(),
            model_items: Vec::new(),
            async_translation: true,
            pristine: true,
            testing: false,
        };
        f.apply_preset_defaults();
        f
    }
}

impl WinProviderForm {
    pub fn preset(&self) -> &'static TranslationProviderPreset {
        preset_at(self.preset_index)
    }

    /// Name = preset name, URL = preset URL, model items = suggestions, model = first suggestion (key untouched).
    pub fn apply_preset_defaults(&mut self) {
        let p = self.preset();
        self.name = p.name(None);
        self.url = p.base_url.to_owned();
        self.model_items = p.suggested_models.iter().map(|s| (*s).to_owned()).collect();
        self.model = p.default_model().to_owned();
        self.pristine = true;
    }

    /// The user picked a preset (also while editing: overwrites name/URL/model, keeps the key and the edited
    /// profile's preset id on save).
    pub fn select_preset(&mut self, index: usize) {
        self.preset_index = index.min(TranslationProviderPreset::ALL.len() - 1);
        self.apply_preset_defaults();
    }

    /// Language switch / reload: re-apply the defaults only to a pristine new-provider draft.
    pub fn relocalize(&mut self) {
        if self.editing.is_none() && self.pristine {
            self.apply_preset_defaults();
        }
    }

    /// `ResetEditForm`. A running draft test keeps its button disabled until it answers (C#).
    pub fn reset(&mut self) {
        self.editing = None;
        self.key = Zeroizing::new(String::new());
        self.async_translation = true;
        self.apply_preset_defaults();
    }

    /// `BeginEdit(profile)`; `key` = the decrypted key for the password box.
    pub fn begin_edit(&mut self, profile: &TranslationProviderProfile, key: &str) {
        self.editing = Some(profile.id);
        self.preset_index = preset_index(&profile.preset_id);
        self.name = profile.name.clone();
        self.url = profile.base_url.clone();
        self.key = Zeroizing::new(key.to_owned());
        self.model_items = profile.available_models(false);
        self.model = profile.model.clone();
        self.async_translation = profile.async_translation;
        self.pristine = false;
    }

    pub fn header(&self) -> String {
        if self.editing.is_some() { sa_core::l("API Sağlayıcısını Düzenle") } else { sa_core::l("API Sağlayıcısı Ekle") }
    }

    pub fn save_caption(&self) -> String {
        if self.editing.is_some() { sa_core::l("Güncelle") } else { sa_core::l("Kaydet ve Etkinleştir") }
    }

    /// The test answered: the model list is replaced; the chosen model is the (trimmed) model sent with the test if
    /// it was not blank, else the first listed one (C# `TestDraftButton_Click`).
    pub fn apply_test_models(&mut self, models: &[String], tested_model: &str) {
        self.model_items = models.to_vec();
        let tested = tested_model.trim();
        self.model = if tested.is_empty() { models.first().cloned().unwrap_or_default() } else { tested.to_owned() };
    }
}

// ---------------------------------------------------------------------------------------------------------------
// macOS draft
// ---------------------------------------------------------------------------------------------------------------

/// `ProviderDraft` of the macOS editor sheet; nothing is stored before Save.
#[derive(Debug, Clone, PartialEq)]
pub struct MacProviderDraft {
    pub id: Uuid,
    pub is_new: bool,
    preset_id: String,
    pub name: String,
    base_url: String,
    api_key: Zeroizing<String>,
    pub key_edited: bool,
    pub stored_key: ProviderKey,
    pub model: String,
    async_translation: bool,
    async_translation_chosen: bool,
    pub discovered_models: Vec<String>,
}

impl MacProviderDraft {
    /// A new draft of the first preset (or `preset`).
    pub fn new(preset: &TranslationProviderPreset) -> Self {
        Self {
            id: Uuid::new_v4(),
            is_new: true,
            preset_id: preset.id.to_owned(),
            name: preset.name(None),
            base_url: preset.base_url.to_owned(),
            api_key: Zeroizing::new(String::new()),
            key_edited: false,
            stored_key: ProviderKey::Absent,
            model: preset.default_model().to_owned(),
            async_translation: preset.translates_in_background_by_default(),
            async_translation_chosen: false,
            discovered_models: Vec::new(),
        }
    }

    pub fn editing(profile: &TranslationProviderProfile, key: ProviderKey) -> Self {
        Self {
            id: profile.id,
            is_new: false,
            preset_id: profile.preset_id.clone(),
            name: profile.name.clone(),
            base_url: profile.base_url.clone(),
            api_key: Zeroizing::new(String::new()),
            key_edited: false,
            stored_key: key,
            model: profile.model.clone(),
            async_translation: profile.async_translation,
            async_translation_chosen: false,
            discovered_models: profile.discovered_models.clone(),
        }
    }

    pub fn preset(&self) -> &'static TranslationProviderPreset {
        TranslationProviderPreset::preset(Some(&self.preset_id))
    }
    pub fn preset_id(&self) -> &str {
        &self.preset_id
    }
    pub fn base_url(&self) -> &str {
        &self.base_url
    }
    /// A changed (trimmed) address forgets the models found for the old one.
    pub fn set_base_url(&mut self, v: &str) {
        if self.base_url.trim() != v.trim() {
            self.discovered_models.clear();
        }
        self.base_url = v.to_owned();
    }
    pub fn api_key(&self) -> &str {
        &self.api_key
    }
    pub fn set_api_key(&mut self, v: &str) {
        if *self.api_key != v {
            self.key_edited = true;
        }
        self.api_key = Zeroizing::new(v.to_owned());
    }
    pub fn async_translation(&self) -> bool {
        self.async_translation
    }
    pub fn set_async_translation(&mut self, v: bool) {
        if self.async_translation != v {
            self.async_translation_chosen = true;
        }
        self.async_translation = v;
    }
    pub fn set_stored_key(&mut self, k: ProviderKey) {
        self.stored_key = k;
    }

    pub fn can_remove_stored_key(&self) -> bool {
        !self.is_new && !self.key_edited && matches!(self.stored_key, ProviderKey::Present(_) | ProviderKey::Unreadable)
    }
    pub fn removes_stored_key(&self) -> bool {
        !self.is_new && self.key_edited && self.api_key.trim().is_empty() && self.stored_key != ProviderKey::Absent
    }
    pub fn remove_stored_key(&mut self) {
        self.api_key = Zeroizing::new(String::new());
        self.key_edited = true;
    }
    /// Raw field text when it is to be written, `None` to keep the stored key.
    pub fn key_to_save(&self) -> Option<&str> {
        (self.is_new || self.key_edited).then_some(self.api_key.as_str())
    }
    /// The key a test sends.
    pub fn effective_key(&self) -> &str {
        if self.is_new || self.key_edited { &self.api_key } else { self.stored_key.value() }
    }

    /// Picking a provider type refills name, address and model; a new draft whose async switch was never touched
    /// follows the preset default. The key is untouched.
    pub fn apply_preset(&mut self, preset: &TranslationProviderPreset) {
        self.preset_id = preset.id.to_owned();
        self.name = preset.name(None);
        self.base_url = preset.base_url.to_owned();
        self.model = preset.default_model().to_owned();
        self.discovered_models.clear();
        if self.is_new && !self.async_translation_chosen {
            self.async_translation = preset.translates_in_background_by_default();
        }
    }

    pub fn profile(&self) -> TranslationProviderProfile {
        TranslationProviderProfile {
            id: self.id,
            name: self.name.clone(),
            preset_id: self.preset_id.clone(),
            base_url: self.base_url.clone(),
            model: self.model.clone(),
            async_translation: self.async_translation,
            discovered_models: self.discovered_models.clone(),
        }
    }

    pub fn available_models(&self) -> Vec<String> {
        self.profile().available_models(true)
    }

    pub fn has_usable_address(&self) -> bool {
        ExternalLlmServer::normalize_base_url(&self.base_url).is_some()
    }

    /// The secure field's placeholder.
    pub fn key_prompt(&self) -> String {
        if !self.is_new {
            match &self.stored_key {
                ProviderKey::Present(k) => {
                    return sa_core::l!("Kayıtlı anahtar korunur: %@", sa_providers::mask_key(k).unwrap_or_default());
                }
                ProviderKey::Loading => return sa_core::l("Anahtar okunuyor…"),
                ProviderKey::Unreadable => return sa_core::l("Anahtar okunamadı; boş bırakırsan korunur"),
                ProviderKey::Absent => {}
            }
        }
        sa_core::l("sk-... (Yerel ağ için boş bırakılabilir)")
    }

    /// The note under the key field (existing providers only).
    pub fn stored_key_note(&self) -> Option<String> {
        if self.is_new {
            return None;
        }
        if self.removes_stored_key() {
            return Some(sa_core::l("Kayıtlı anahtar kaydedilince silinecek."));
        }
        match self.stored_key {
            ProviderKey::Present(_) if !self.key_edited => Some(sa_core::l("Alan boş bırakılırsa kayıtlı anahtar korunur.")),
            ProviderKey::Unreadable if !self.key_edited => {
                Some(sa_core::l("API anahtarı okunamadı. Alan boş bırakılırsa anahtar silinmez."))
            }
            _ => None,
        }
    }

    /// The address field's placeholder: the preset URL or a verbatim example.
    pub fn url_placeholder(&self) -> String {
        let url = self.preset().base_url;
        if url.is_empty() { "https://api.example.com/v1".into() } else { url.into() }
    }
}

/// A connection-test notice under a macOS provider row or in the sheet footer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub text: String,
    pub is_error: bool,
}
