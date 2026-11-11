//! The Settings window model (UI thread, no Slint): state, commands and the display snapshot ([`View`]) of both
//! platform layouts (win-ui-hub-settings §6–§7, mac-ui §3.5 + §11, UNIFIED.md §3.13–§3.15).
//!
//! Background work (connection tests, key reads, video-cache scans, font enumeration) runs on worker threads and
//! comes back through [`UiKey`] posts, re-checking an epoch / id guard before it is applied.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use sa_core::appearance::{OverlayBackground, ThemeChoice};
use sa_core::cancel::CancellationToken;
use sa_core::l10n::{LanguageChoice, LanguageState};
use sa_core::models::{ModelLabel, ModelRow, SubtitleLanguage};
use sa_core::settings::keys;
use sa_core::subscription::Subscription;
use sa_enginekit::external_llm::ExternalLlmServer;
use sa_providers::TranslationProviderProfile;
use sa_viewmodels::uiref::UiKey;
use uuid::Uuid;

use super::deps::{LanSettings, SettingsDeps, TestOutcome};
use super::providers::{MacProviderDraft, Notice, ProviderKey, WinProviderForm, preset_at, preset_names};

/// Video cache quota bounds (`videoCacheMaxSizeGb`).
pub const CACHE_MIN_GB: i64 = 1;
pub const CACHE_MAX_GB: i64 = 500;

/// Speaker-count choices (`Otomatik`, 2…6).
pub const SPEAKER_COUNTS: [i64; 6] = [0, 2, 3, 4, 5, 6];

/// Which layout the model serves (behavior follows the compiled UI tree).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavor {
    /// NavigationView: 0 Genel, 1 Görünüm, 2 Modeller.
    Windows,
    /// Preferences tabs: 0 Genel, 1 Çeviri, 2 Görünüm, 3 Modeller.
    Mac,
}

impl Flavor {
    pub const fn native() -> Self {
        if cfg!(sa_ui = "mac") { Flavor::Mac } else { Flavor::Windows }
    }
    pub fn models_page(self) -> i32 {
        match self {
            Flavor::Windows => 2,
            Flavor::Mac => 3,
        }
    }
    /// `App.ShowSettings(page)` names (Windows) / `settingsTab` raw values (macOS) → page index.
    pub fn page_index(self, name: &str) -> i32 {
        match (self, name) {
            (Flavor::Windows, "appearance") => 1,
            (Flavor::Windows, "models") => 2,
            (Flavor::Mac, "translation") => 1,
            (Flavor::Mac, "appearance") => 2,
            (Flavor::Mac, "models") => 3,
            _ => 0,
        }
    }
    pub fn page_name(self, index: i32) -> &'static str {
        match (self, index) {
            (Flavor::Windows, 1) => "appearance",
            (Flavor::Windows, 2) => "models",
            (Flavor::Mac, 1) => "translation",
            (Flavor::Mac, 2) => "appearance",
            (Flavor::Mac, 3) => "models",
            _ => "general",
        }
    }
    /// Page title (Windows) / window title (macOS).
    pub fn page_title(self, index: i32) -> String {
        sa_core::l(match self.page_name(index) {
            "translation" => "Çeviri",
            "appearance" => "Görünüm",
            "models" => "Modeller",
            _ => "Genel",
        })
    }
}

/// What a confirmation dialog will do when accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingConfirm {
    DeleteModel(String),
    DeleteProvider(Uuid),
}

/// What a file-association worker did: `true` = it succeeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AssocOutcome {
    Registered(bool),
    Unregistered(bool),
}

/// Which card action a file-association worker runs for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AssocJob {
    /// The register / unregister switch (shows the info bar).
    Toggle,
    /// "Varsayılan uygulamalar…" (registers only when needed, then opens the Windows page).
    DefaultApps,
}

/// The open confirmation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confirm {
    pub action: PendingConfirm,
    pub title: String,
    pub message: String,
    pub primary: String,
}

#[derive(Default)]
struct State {
    page: i32,
    /// A page was shown once (the window opened); re-selecting the shown page does nothing from then on.
    entered: bool,
    scroll_epoch: i32,
    closed: bool,
    // fonts
    fonts: Vec<String>,
    // Windows LAN
    lan: LanSettings,
    lan_busy: bool,
    lan_epoch: u64,
    lan_test: Option<Notice>,
    // Windows video cache + associations
    cache_used: Option<i64>,
    cache_busy: bool,
    assoc_bar: Option<(i32, String)>,
    /// A registration / unregistration runs on a worker.
    assoc_busy: bool,
    /// The registry state read after this window's last action (`None` until one finished: read live).
    assoc_registered: Option<bool>,
    // providers
    keys: HashMap<Uuid, ProviderKey>,
    key_epochs: HashMap<Uuid, u64>,
    notices: HashMap<Uuid, Notice>,
    testing: HashSet<Uuid>,
    test_ids: HashMap<Uuid, u64>,
    next_test: u64,
    provider_status: Option<Notice>,
    win_form: WinProviderForm,
    mac_draft: Option<MacProviderDraft>,
    mac_testing: bool,
    mac_test_epoch: u64,
    mac_notice: Option<Notice>,
    mac_key_not_stored: bool,
    // models
    freed_mb: Option<i64>,
    confirm: Option<Confirm>,
}

/// One saved provider as displayed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProviderRowView {
    pub id: String,
    pub name: String,
    pub preset_name: String,
    pub shows_preset_tag: bool,
    pub summary: String,
    pub is_active: bool,
    pub is_testing: bool,
    pub model: String,
    pub models: Vec<String>,
    pub notice_text: String,
    pub notice_error: bool,
}

/// One model row as displayed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ModelRowView {
    pub id: String,
    pub name: String,
    pub is_active: bool,
    pub language_summary: String,
    pub storage_status: String,
    pub rating_text: String,
    pub license_text: String,
    pub is_non_commercial: bool,
    pub error_text: String,
    pub is_deleting: bool,
    pub shows_delete: bool,
    pub can_delete: bool,
    pub delete_help: String,
}

/// The provider form (Windows inline card / macOS sheet).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FormView {
    pub open: bool,
    pub is_new: bool,
    pub header: String,
    pub save_caption: String,
    pub preset_items: Vec<String>,
    pub preset_index: i32,
    pub name: String,
    pub url: String,
    pub url_placeholder: String,
    pub is_lan: bool,
    pub key: String,
    pub key_placeholder: String,
    pub key_note: String,
    pub can_remove_key: bool,
    pub model: String,
    pub model_items: Vec<String>,
    pub async_translation: bool,
    pub testing: bool,
    pub can_test: bool,
    pub can_save: bool,
    pub notice_text: String,
    pub notice_error: bool,
    pub key_not_stored: bool,
}

/// Everything both layouts display.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct View {
    pub page: i32,
    pub page_title: String,
    pub scroll_epoch: i32,
    // General
    pub ui_language_items: Vec<String>,
    pub ui_language_index: i32,
    pub needs_relaunch: bool,
    pub speech_language_items: Vec<String>,
    pub speech_language_index: i32,
    pub model_items: Vec<String>,
    pub model_index: i32,
    pub model_enabled: bool,
    pub translation_model_items: Vec<String>,
    pub translation_model_index: i32,
    pub translation_model_separator: i32,
    pub translation_model_enabled: bool,
    pub diarize: bool,
    pub speaker_count_items: Vec<String>,
    pub speaker_count_index: i32,
    pub auto_play: bool,
    pub offline: bool,
    pub offline_footer: String,
    // Windows LAN
    pub lan_enabled: bool,
    pub lan_url: String,
    pub lan_busy: bool,
    pub lan_test_open: bool,
    pub lan_test_success: bool,
    pub lan_test_text: String,
    pub lan_models: Vec<String>,
    pub lan_model: String,
    pub lan_async: bool,
    // Windows cache + associations
    pub cache_usage_text: String,
    pub cache_gb: i64,
    pub clear_cache_enabled: bool,
    pub associations_registered: bool,
    /// A registration / unregistration is running (the card ignores clicks meanwhile).
    pub associations_busy: bool,
    pub associations_bar_open: bool,
    pub associations_bar_severity: i32,
    pub associations_bar_text: String,
    // providers
    pub local_provider_active: bool,
    pub provider_rows: Vec<ProviderRowView>,
    pub cloud_callout: String,
    pub provider_status_open: bool,
    pub provider_status_success: bool,
    pub provider_status_text: String,
    pub form: FormView,
    // Appearance
    pub theme_index: i32,
    pub font_items: Vec<(String, bool)>,
    pub ui_font_index: i32,
    pub overlay_font_index: i32,
    pub overlay_font_family: String,
    pub overlay_size: f64,
    pub overlay_size_text: String,
    pub overlay_color: (u8, u8, u8),
    pub overlay_color_hex: String,
    pub overlay_background_index: i32,
    pub overlay_opacity: f64,
    pub overlay_opacity_text: String,
    // Models
    pub transcription_rows: Vec<ModelRowView>,
    pub diarization_rows: Vec<ModelRowView>,
    pub translation_rows: Vec<ModelRowView>,
    pub models_loading: bool,
    pub models_load_error: String,
    pub models_total_text: String,
    pub models_notice_open: bool,
    pub models_notice_text: String,
    // confirmation
    pub confirm: Option<Confirm>,
}

/// The Settings window model.
pub struct SettingsModel {
    deps: SettingsDeps,
    flavor: Flavor,
    state: RefCell<State>,
    key: Cell<Option<UiKey<SettingsModel>>>,
    /// The association state the last [`View`] showed (what a click on the card's switch means).
    assoc_shown: Cell<Option<bool>>,
    on_changed: RefCell<Option<Rc<dyn Fn()>>>,
    subs: RefCell<Vec<Subscription>>,
    cancel: CancellationToken,
}

impl Drop for SettingsModel {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(k) = self.key.get() {
            k.unregister();
        }
    }
}

fn index_of<T: PartialEq>(items: &[T], value: &T) -> i32 {
    items.iter().position(|v| v == value).map(|i| i as i32).unwrap_or(-1)
}

/// Whether text to this address leaves the local network (`!ExternalLLM(url).isOnLocalNetwork`).
pub fn sends_text_over_internet(base_url: &str) -> bool {
    let host = ExternalLlmServer::normalize_base_url(base_url).as_deref().and_then(ExternalLlmServer::host);
    !host.is_some_and(|h| ExternalLlmServer::is_local_network_host(&h))
}

impl SettingsModel {
    /// Must be called on the UI thread. Starts the font enumeration and (macOS) the provider key reads.
    pub fn new(deps: SettingsDeps, flavor: Flavor) -> Rc<Self> {
        let lan = if flavor == Flavor::Windows { Self::read_lan(&deps) } else { LanSettings::default() };
        let page = match flavor {
            Flavor::Mac => flavor.page_index(&deps.settings.get_string(keys::SETTINGS_TAB).unwrap_or_default()),
            Flavor::Windows => 0,
        };
        let model = Rc::new(Self {
            deps,
            flavor,
            state: RefCell::new(State { page, lan, ..Default::default() }),
            key: Cell::new(None),
            assoc_shown: Cell::new(None),
            on_changed: RefCell::new(None),
            subs: RefCell::new(Vec::new()),
            cancel: CancellationToken::new(),
        });
        let key = UiKey::register(&model);
        model.key.set(Some(key));
        model.subscribe_sources();
        model.load_fonts();
        let ids: Vec<Uuid> = model.deps.providers.providers().iter().map(|p| p.id).collect();
        model.load_keys(&ids);
        model
    }

    pub fn flavor(&self) -> Flavor {
        self.flavor
    }

    pub fn deps(&self) -> &SettingsDeps {
        &self.deps
    }

    /// The host's re-push hook (coalescing is the host's job).
    pub fn set_on_changed(&self, f: impl Fn() + 'static) {
        *self.on_changed.borrow_mut() = Some(Rc::new(f));
    }

    fn changed(&self) {
        let f = self.on_changed.borrow().clone();
        if let Some(f) = f {
            f();
        }
    }

    fn ui_key(&self) -> UiKey<SettingsModel> {
        self.key.get().expect("registered")
    }

    fn subscribe_sources(&self) {
        let mut subs = Vec::new();
        let ui = self.deps.ui.clone();
        let key = self.ui_key();
        // Providers (may arrive off the UI thread; another instance's reload too).
        {
            let ui = ui.clone();
            subs.push(self.deps.providers.subscribe(Box::new(move |_| {
                key.post(&ui, |m| m.on_providers_changed());
            })));
        }
        {
            let ui = ui.clone();
            subs.push(self.deps.catalog.subscribe(move |_| key.post(&ui, |m| m.changed())));
        }
        {
            let ui = ui.clone();
            subs.push(self.deps.appearance.subscribe(move |_| key.post(&ui, |m| m.changed())));
        }
        {
            let ui = ui.clone();
            subs.push(self.deps.language.subscribe(move |_| key.post(&ui, |m| m.on_language_changed())));
        }
        {
            let ui = ui.clone();
            subs.push(LanguageState::shared().subscribe(move |_| key.post(&ui, |m| m.on_language_changed())));
        }
        if let Some(activity) = self.mac_activity() {
            // A picker's test flags and notices show on the provider cards.
            let ui = ui.clone();
            subs.push(activity.subscribe(Box::new(move |change| {
                if change == sa_viewmodels::providers_ui::ProvidersChange::Tests {
                    key.post(&ui, |m| m.changed());
                }
            })));
        }
        if self.flavor == Flavor::Mac {
            // `settingsTab` is read live: another window storing it switches the tab.
            let ui = ui.clone();
            subs.push(self.deps.settings.subscribe(Box::new(move |k| {
                if k == keys::SETTINGS_TAB {
                    key.post(&ui, |m| m.on_settings_tab_changed());
                }
            })));
        }
        self.subs.borrow_mut().extend(subs);
    }

    /// The window closed: queued results are ignored from now on, running work is cancelled.
    pub fn close(&self) {
        self.state.borrow_mut().closed = true;
        self.cancel.cancel();
        self.subs.borrow_mut().clear();
    }

    pub fn is_closed(&self) -> bool {
        self.state.borrow().closed
    }

    fn on_language_changed(&self) {
        self.deps.catalog.refresh_localization();
        self.state.borrow_mut().win_form.relocalize();
        self.changed();
    }

    fn on_settings_tab_changed(&self) {
        let raw = self.deps.settings.get_string(keys::SETTINGS_TAB).unwrap_or_default();
        let page = self.flavor.page_index(&raw);
        if page != self.state.borrow().page {
            self.enter_page(page, false);
        }
    }

    fn on_providers_changed(&self) {
        if self.flavor == Flavor::Windows {
            self.apply_providers();
        }
        // New providers (another instance) need their key captions.
        let ids: Vec<Uuid> = {
            let st = self.state.borrow();
            self.deps.providers.providers().iter().map(|p| p.id).filter(|id| !st.keys.contains_key(id)).collect()
        };
        self.load_keys(&ids);
        self.changed();
    }

    // MARK: pages and window lifecycle

    pub fn page(&self) -> i32 {
        self.state.borrow().page
    }

    /// `App.ShowSettings(page)` / opening with `settingsTab`.
    pub fn show_page(&self, name: Option<&str>) {
        let page = match (self.flavor, name) {
            (_, Some(n)) => self.flavor.page_index(n),
            (Flavor::Mac, None) => self.flavor.page_index(&self.deps.settings.get_string(keys::SETTINGS_TAB).unwrap_or_default()),
            (Flavor::Windows, None) => self.state.borrow().page,
        };
        self.enter_page(page, true);
    }

    /// The user selected a navigation item / tab.
    pub fn select_page(&self, page: i32) {
        self.enter_page(page, true);
    }

    /// Shows `page`. Selecting the page that is already shown changes nothing (a NavigationView raises no
    /// SelectionChanged, a SwiftUI TabView keeps its tab): no reload, no scroll to the top.
    fn enter_page(&self, page: i32, persist: bool) {
        let max = if self.flavor == Flavor::Mac { 3 } else { 2 };
        let page = page.clamp(0, max);
        {
            let mut st = self.state.borrow_mut();
            if st.entered && st.page == page {
                return;
            }
            st.entered = true;
            st.page = page;
            st.scroll_epoch += 1;
        }
        if persist && self.flavor == Flavor::Mac {
            self.deps.settings.set_string(keys::SETTINGS_TAB, self.flavor.page_name(page));
        }
        if page == self.flavor.models_page() {
            self.deps.catalog.load();
        }
        if self.flavor == Flavor::Mac && page == 1 {
            // The local server's models may have changed since the last visit; clouds only on request.
            if let Some(p) = self.deps.providers.active_provider()
                && ExternalLlmServer::normalize_base_url(&p.base_url).is_some()
                && !sends_text_over_internet(&p.base_url)
            {
                self.test_provider(p.id);
            }
        }
        self.changed();
    }

    /// The window was shown (not constructed): compute the encrypted-cache usage off the UI thread.
    pub fn window_loaded(&self) {
        self.refresh_cache_usage();
    }

    // MARK: General

    pub fn select_ui_language(&self, index: i32) {
        if let Some(c) = LanguageChoice::ALL.get(index.max(0) as usize) {
            self.deps.language.set_choice(*c);
        }
        self.changed();
    }

    pub fn select_speech_language(&self, index: i32) {
        if let Some(l) = SubtitleLanguage::ALL.get(index.max(0) as usize) {
            self.deps.vm.set_language(l.code);
        }
        self.changed();
    }

    pub fn select_model(&self, index: i32) {
        if let Some(m) = self.deps.vm.available_models().get(index.max(0) as usize) {
            self.deps.vm.set_model(&m.id);
        }
        self.changed();
    }

    /// The default translation model ids in pop-up order (macOS: llama, separator, classic).
    fn translation_model_ids(&self) -> (Vec<String>, i32) {
        let c = &self.deps.catalog;
        match self.flavor {
            Flavor::Windows => (c.translation_models().into_iter().map(|m| m.id).collect(), -1),
            Flavor::Mac => {
                let llama: Vec<String> = c.llama_translation_models().into_iter().map(|m| m.id).collect();
                let classic: Vec<String> = c.classic_translation_models().into_iter().map(|m| m.id).collect();
                let sep = if !llama.is_empty() && !classic.is_empty() { llama.len() as i32 } else { -1 };
                (llama.into_iter().chain(classic).collect(), sep)
            }
        }
    }

    pub fn select_translation_model(&self, index: i32) {
        let (ids, _) = self.translation_model_ids();
        if let Some(id) = ids.get(index.max(0) as usize) {
            self.deps.vm.set_translation_model(id);
        }
        self.changed();
    }

    pub fn set_diarize(&self, v: bool) {
        self.deps.vm.set_diarize(v);
        self.changed();
    }

    pub fn select_speaker_count(&self, index: i32) {
        let v = SPEAKER_COUNTS.get(index.max(0) as usize).copied().unwrap_or(0);
        self.deps.vm.set_speaker_count(v);
        self.changed();
    }

    pub fn set_auto_play(&self, v: bool) {
        self.deps.vm.set_auto_play_video(v);
        self.changed();
    }

    pub fn set_offline(&self, v: bool) {
        self.deps.vm.set_offline_mode(v);
        self.changed();
    }

    // MARK: Windows LAN section (legacy `externalLlm*` keys bridged to the `lan` provider)

    fn read_lan(deps: &SettingsDeps) -> LanSettings {
        let s = &deps.settings;
        LanSettings {
            enabled: s.get_bool(keys::EXTERNAL_LLM_ENABLED).unwrap_or(false),
            url: s.get_string(keys::EXTERNAL_LLM_URL).unwrap_or_default(),
            model: s.get_string(keys::EXTERNAL_LLM_MODEL).unwrap_or_default(),
            async_translation: s.get_bool(keys::EXTERNAL_LLM_ASYNC).unwrap_or(false),
            models: Vec::new(),
        }
    }

    fn persist_lan(&self, lan: &LanSettings) {
        let s = &self.deps.settings;
        if s.get_bool(keys::EXTERNAL_LLM_ENABLED) != Some(lan.enabled) {
            s.set_bool(keys::EXTERNAL_LLM_ENABLED, lan.enabled);
        }
        if s.get_string(keys::EXTERNAL_LLM_URL).as_deref() != Some(lan.url.as_str()) {
            s.set_string(keys::EXTERNAL_LLM_URL, &lan.url);
        }
        if s.get_string(keys::EXTERNAL_LLM_MODEL).as_deref() != Some(lan.model.as_str()) {
            s.set_string(keys::EXTERNAL_LLM_MODEL, &lan.model);
        }
        if s.get_bool(keys::EXTERNAL_LLM_ASYNC) != Some(lan.async_translation) {
            s.set_bool(keys::EXTERNAL_LLM_ASYNC, lan.async_translation);
        }
    }

    fn sync_lan(&self) {
        let lan = self.state.borrow().lan.clone();
        self.persist_lan(&lan);
        if let Some(w) = &self.deps.win {
            w.sync_lan(&lan);
        }
    }

    /// `ApplyToViewModel`: the active provider is mirrored into the LAN fields (persisted).
    fn apply_providers(&self) {
        let Some(w) = &self.deps.win else { return };
        let mut lan = self.state.borrow().lan.clone();
        w.apply_providers(&mut lan);
        self.persist_lan(&lan);
        self.state.borrow_mut().lan = lan;
    }

    pub fn lan(&self) -> LanSettings {
        self.state.borrow().lan.clone()
    }

    /// C# `IsExternalLlmActive`: a cloud provider is active (`ActiveProviderName` set by `ApplyToViewModel`), or the
    /// LAN section is on with an address.
    fn is_lan_active(&self) -> bool {
        let cloud_active = self
            .deps
            .providers
            .active_provider()
            .is_some_and(|p| !p.preset_id.eq_ignore_ascii_case("lan") && !p.base_url.trim().is_empty());
        let st = self.state.borrow();
        cloud_active || (st.lan.enabled && !st.lan.url.trim().is_empty())
    }

    pub fn set_lan_enabled(&self, v: bool) {
        self.state.borrow_mut().lan.enabled = v;
        self.sync_lan();
        if self.is_lan_active() {
            self.run_lan_test(false);
        }
        self.changed();
    }

    /// Every text change (the provider write is coalesced by the store's atomic writes).
    pub fn set_lan_url(&self, text: &str) {
        if self.state.borrow().lan.url == text {
            return;
        }
        self.state.borrow_mut().lan.url = text.to_owned();
        self.sync_lan();
        self.changed();
    }

    /// Selection, Enter and focus loss of the editable combo. Stored as typed (the C# setter does not trim; the LAN
    /// provider sync trims).
    pub fn commit_lan_model(&self, text: &str) {
        if self.state.borrow().lan.model == text {
            return;
        }
        self.state.borrow_mut().lan.model = text.to_owned();
        self.sync_lan();
        self.changed();
    }

    pub fn set_lan_async(&self, v: bool) {
        self.state.borrow_mut().lan.async_translation = v;
        self.sync_lan();
        self.changed();
    }

    /// "Bağlantıyı test et" (with the InfoBar) / the refresh button (without).
    pub fn lan_test(&self) {
        self.run_lan_test(true);
    }

    pub fn lan_refresh_models(&self) {
        self.run_lan_test(false);
    }

    pub fn close_lan_test(&self) {
        self.state.borrow_mut().lan_test = None;
        self.changed();
    }

    fn run_lan_test(&self, show: bool) {
        let Some(w) = self.deps.win.clone() else { return };
        let url = {
            let mut st = self.state.borrow_mut();
            if st.lan_busy {
                return;
            }
            st.lan_busy = true;
            st.lan_epoch += 1;
            st.lan.url.clone()
        };
        self.changed();
        let epoch = self.state.borrow().lan_epoch;
        let cancel = self.cancel.child();
        let key = self.ui_key();
        let ui = self.deps.ui.clone();
        let _ = std::thread::Builder::new().name("sa-settings-lan".into()).spawn(move || {
            let outcome = w.test_lan(&url, &cancel);
            key.post(&ui, move |m| {
                if !m.is_closed() {
                    m.finish_lan_test(epoch, show, outcome);
                }
            });
        });
    }

    fn finish_lan_test(&self, epoch: u64, show: bool, outcome: Option<TestOutcome>) {
        {
            let mut st = self.state.borrow_mut();
            if st.lan_epoch != epoch {
                return;
            }
            st.lan_busy = false;
        }
        let Some(o) = outcome else {
            self.changed();
            return;
        };
        {
            let mut st = self.state.borrow_mut();
            st.lan.models = o.models.clone();
            if st.lan.model.trim().is_empty()
                && let Some(first) = o.models.first()
            {
                st.lan.model = first.clone();
            }
            if show {
                st.lan_test = Some(Notice { text: o.message.clone(), is_error: !o.success });
            }
        }
        if !o.models.is_empty()
            && let Some(w) = &self.deps.win
        {
            let model = self.state.borrow().lan.model.clone();
            w.store_lan_models(&o.models, Some(&model));
        }
        self.sync_lan();
        self.changed();
    }

    // MARK: Windows video cache and file associations

    fn cache_max_gb(&self) -> i64 {
        self.deps
            .settings
            .get_int(keys::VIDEO_CACHE_MAX_SIZE_GB)
            .unwrap_or(sa_core::settings::defaults::VIDEO_CACHE_MAX_SIZE_GB)
            .clamp(CACHE_MIN_GB, CACHE_MAX_GB)
    }

    /// NumberBox value: non-finite → revert to the stored value; else rounded, clamped, persisted.
    pub fn edit_cache_gb(&self, text: &str) {
        let parsed = text.trim().replace(',', ".").parse::<f64>().ok().filter(|v| v.is_finite());
        if let Some(v) = parsed {
            let gb = (sa_core::text::round_half_away_from_zero(v) as i64).clamp(CACHE_MIN_GB, CACHE_MAX_GB);
            if gb != self.cache_max_gb() || self.deps.settings.get_int(keys::VIDEO_CACHE_MAX_SIZE_GB).is_none() {
                self.deps.settings.set_int(keys::VIDEO_CACHE_MAX_SIZE_GB, gb);
            }
            if let Some(w) = &self.deps.win {
                w.set_cache_max_gb(gb);
            }
        }
        self.changed();
    }

    fn refresh_cache_usage(&self) {
        let Some(w) = self.deps.win.clone() else { return };
        {
            let mut st = self.state.borrow_mut();
            if st.closed || st.cache_busy {
                return;
            }
            st.cache_busy = true;
        }
        self.changed();
        self.run_cache_job(w, false);
    }

    pub fn clear_cache(&self) {
        let Some(w) = self.deps.win.clone() else { return };
        {
            let mut st = self.state.borrow_mut();
            if st.cache_busy || st.cache_used.unwrap_or(0) <= 0 {
                return;
            }
            st.cache_busy = true;
        }
        self.changed();
        self.run_cache_job(w, true);
    }

    fn run_cache_job(&self, w: std::sync::Arc<dyn super::deps::WinSettingsOps>, clear: bool) {
        let key = self.ui_key();
        let ui = self.deps.ui.clone();
        let _ = std::thread::Builder::new().name("sa-settings-cache".into()).spawn(move || {
            if clear {
                w.clear_cache();
            }
            let used = w.cache_used_bytes();
            key.post(&ui, move |m| m.finish_cache(used));
        });
    }

    fn finish_cache(&self, used: i64) {
        if self.is_closed() {
            return;
        }
        {
            let mut st = self.state.borrow_mut();
            st.cache_used = Some(used);
            st.cache_busy = false;
        }
        self.changed();
    }

    /// The registration state shown on the card: the outcome of the last action of this window, else a live read.
    fn associations_registered(&self) -> bool {
        if let Some(registered) = self.state.borrow().assoc_registered {
            return registered;
        }
        self.deps.win.as_ref().and_then(|w| w.associations()).is_some_and(|a| a.is_registered())
    }

    /// True while a registration / unregistration of this window runs (further clicks are ignored).
    pub fn associations_busy(&self) -> bool {
        self.state.borrow().assoc_busy
    }

    /// Marks the card busy and returns the associations, or `None` (nothing to do, or an action already runs).
    fn begin_associations_job(&self) -> Option<std::sync::Arc<dyn sa_core::platform::FileAssociations>> {
        let a = self.deps.win.as_ref().and_then(|w| w.associations())?;
        {
            let mut st = self.state.borrow_mut();
            if st.closed || st.assoc_busy {
                return None;
            }
            st.assoc_busy = true;
        }
        self.changed();
        Some(a)
    }

    /// The card's switch. The direction is what the card showed when it was clicked (not a later registry read, which
    /// a running launch registration could flip), and the worker does exactly that ([`Self::run_associations_job`]).
    pub fn toggle_associations(&self) {
        let shown = self.assoc_shown.get().unwrap_or_else(|| self.associations_registered());
        let Some(a) = self.begin_associations_job() else { return };
        self.run_associations_job(a, !shown, AssocJob::Toggle);
    }

    /// "Varsayılan uygulamalar…": registers first when needed (on a worker), then opens the Windows page. Both card
    /// buttons are disabled while an action runs (`associations_busy`); a click that still arrives is ignored.
    pub fn open_default_apps(&self) {
        let Some(a) = self.begin_associations_job() else { return };
        self.run_associations_job(a, true, AssocJob::DefaultApps);
    }

    /// Runs one association action on a worker; the result comes back to the UI thread ([`Self::finish_associations`]).
    ///
    /// The user's intent (`fileAssociationsDisabled = !register`) is persisted and flushed before the registry writes,
    /// inside the registrar's ordering section ([`sa_core::platform::FileAssociations::apply_user_choice`]), and reverted when the writes
    /// fail. So two windows cannot leave the setting out of order with the registry, a quit right after the click
    /// cannot lose the opt-out, and an instance started meanwhile already reads it.
    fn run_associations_job(&self, a: std::sync::Arc<dyn sa_core::platform::FileAssociations>, register: bool, job: AssocJob) {
        let key = self.ui_key();
        let ui = self.deps.ui.clone();
        let settings = self.deps.settings.clone();
        let worker = a.clone();
        let spawned = std::thread::Builder::new().name("sa-settings-associations".into()).spawn(move || {
            let previous = std::cell::Cell::new(None);
            let ok = worker.apply_user_choice(
                register,
                job == AssocJob::DefaultApps,
                &mut || {
                    previous.set(Some(settings.get_bool(keys::FILE_ASSOCIATIONS_DISABLED)));
                    settings.set_bool(keys::FILE_ASSOCIATIONS_DISABLED, !register);
                    settings.flush();
                },
                &mut |ok| {
                    if !ok {
                        match previous.get().flatten() {
                            Some(v) => settings.set_bool(keys::FILE_ASSOCIATIONS_DISABLED, v),
                            None => settings.remove(keys::FILE_ASSOCIATIONS_DISABLED),
                        }
                        settings.flush();
                    }
                },
            );
            let outcome = if register { AssocOutcome::Registered(ok) } else { AssocOutcome::Unregistered(ok) };
            let registered = worker.is_registered();
            key.post(&ui, move |m| m.finish_associations(worker, job, outcome, registered));
        });
        if spawned.is_err() {
            self.state.borrow_mut().assoc_busy = false;
            self.show_associations_outcome(if register { AssocOutcome::Registered(false) } else { AssocOutcome::Unregistered(false) });
        }
    }

    fn finish_associations(
        &self,
        a: std::sync::Arc<dyn sa_core::platform::FileAssociations>,
        job: AssocJob,
        outcome: AssocOutcome,
        registered: bool,
    ) {
        {
            let mut st = self.state.borrow_mut();
            st.assoc_busy = false;
            st.assoc_registered = Some(registered);
        }
        if self.is_closed() {
            return;
        }
        match job {
            AssocJob::Toggle => self.show_associations_outcome(outcome),
            AssocJob::DefaultApps => {
                a.open_default_apps_settings();
                self.changed();
            }
        }
    }

    fn show_associations_outcome(&self, outcome: AssocOutcome) {
        let bar = match outcome {
            AssocOutcome::Unregistered(true) => (0, sa_core::l("Dosya türü ilişkilendirmeleri kaldırıldı.")),
            AssocOutcome::Unregistered(false) => (2, sa_core::l("Dosya türü ilişkilendirmeleri kaldırılamadı.")),
            AssocOutcome::Registered(true) => (
                1,
                sa_core::l(
                    "Video ve altyazı dosya türleri Windows'a kaydedildi. “Varsayılan uygulamalar…”dan Subtitle Adder'ı varsayılan oynatıcı yapabilirsin.",
                ),
            ),
            AssocOutcome::Registered(false) => (2, sa_core::l("Dosya türleri Windows'a kaydedilemedi.")),
        };
        self.state.borrow_mut().assoc_bar = Some(bar);
        self.changed();
    }

    pub fn close_associations_bar(&self) {
        self.state.borrow_mut().assoc_bar = None;
        self.changed();
    }

    // MARK: provider keys (cache; reads off the UI thread)

    fn load_keys(&self, ids: &[Uuid]) {
        let existing: HashSet<Uuid> = self.deps.providers.providers().iter().map(|p| p.id).collect();
        let ids: Vec<Uuid> = ids.iter().copied().filter(|id| existing.contains(id)).collect();
        if ids.is_empty() {
            return;
        }
        let epochs: Vec<(Uuid, u64)> = {
            let mut st = self.state.borrow_mut();
            for id in &ids {
                st.keys.entry(*id).or_insert(ProviderKey::Loading);
            }
            ids.iter().map(|id| (*id, st.key_epochs.get(id).copied().unwrap_or(0))).collect()
        };
        let store = self.deps.providers.clone();
        let key = self.ui_key();
        let ui = self.deps.ui.clone();
        let _ = std::thread::Builder::new().name("sa-settings-keys".into()).spawn(move || {
            let reads: Vec<(Uuid, u64, ProviderKey)> =
                epochs.into_iter().map(|(id, e)| (id, e, ProviderKey::from_read(store.read_key(id)))).collect();
            key.post(&ui, move |m| m.finish_keys(reads));
        });
    }

    fn finish_keys(&self, reads: Vec<(Uuid, u64, ProviderKey)>) {
        if self.is_closed() {
            return;
        }
        let existing: HashSet<Uuid> = self.deps.providers.providers().iter().map(|p| p.id).collect();
        {
            let mut st = self.state.borrow_mut();
            for (id, epoch, value) in reads {
                if st.key_epochs.get(&id).copied().unwrap_or(0) != epoch || !existing.contains(&id) {
                    continue;
                }
                // Never throw away a known key because a later read failed.
                if value == ProviderKey::Unreadable && matches!(st.keys.get(&id), Some(ProviderKey::Present(_))) {
                    continue;
                }
                st.keys.insert(id, value.clone());
                if let Some(d) = st.mac_draft.as_mut()
                    && d.id == id
                    && !d.is_new
                    && !d.stored_key.is_settled()
                {
                    d.set_stored_key(value);
                }
            }
        }
        self.changed();
    }

    fn record_key_written(&self, id: Uuid, key: &str, stored: bool) {
        let refused = {
            let mut st = self.state.borrow_mut();
            *st.key_epochs.entry(id).or_insert(0) += 1;
            let t = key.trim();
            let v = if t.is_empty() {
                ProviderKey::Absent
            } else if stored {
                ProviderKey::Present(zeroize::Zeroizing::new(t.to_owned()))
            } else {
                ProviderKey::Loading
            };
            let refused = v == ProviderKey::Loading;
            st.keys.insert(id, v);
            refused
        };
        if refused {
            self.load_keys(&[id]);
        }
    }

    fn forget_key(&self, id: Uuid) {
        let mut st = self.state.borrow_mut();
        *st.key_epochs.entry(id).or_insert(0) += 1;
        st.keys.remove(&id);
    }

    fn forget_tests(&self, id: Uuid) {
        let was_testing = {
            let mut st = self.state.borrow_mut();
            st.test_ids.remove(&id);
            st.notices.remove(&id);
            st.testing.remove(&id)
        };
        if let Some(activity) = self.mac_activity() {
            if was_testing {
                activity.end_external_test(id, None);
            }
            activity.set_notice(id, None);
        }
    }

    /// macOS: the provider test state shared with the toolbar pickers ([`SettingsDeps::activity`]).
    fn mac_activity(&self) -> Option<&Rc<sa_viewmodels::providers_ui::ProvidersModel>> {
        if self.flavor == Flavor::Mac { self.deps.activity.as_ref() } else { None }
    }

    /// A provider card's notice: on macOS the shared one when the pickers share the state.
    fn provider_notice(&self, st: &State, id: Uuid) -> Option<Notice> {
        match self.mac_activity() {
            Some(activity) => activity.notice(id).map(|n| Notice { text: n.text(), is_error: n.is_error }),
            None => st.notices.get(&id).cloned(),
        }
    }

    fn shared_notice(notice: &Notice) -> sa_viewmodels::providers_ui::ExternalConnectionNotice {
        let message = sa_core::l10n::LocalizedMessage::verbatim(notice.text.clone());
        if notice.is_error {
            sa_viewmodels::providers_ui::ExternalConnectionNotice::error(message)
        } else {
            sa_viewmodels::providers_ui::ExternalConnectionNotice::success(message)
        }
    }

    /// The key part of a provider row's summary. Windows reads the manager's in-memory key slots synchronously like the
    /// C# `MaskedApiKey` (no loading state); macOS uses the background-filled key cache (`providerKeyCaption`).
    pub fn key_caption(&self, id: Uuid) -> String {
        if self.flavor == Flavor::Windows
            && let Some(w) = &self.deps.win
        {
            return match w.masked_key(id) {
                Ok(Some(mask)) => mask,
                Ok(None) => sa_core::l("Anahtar yok"),
                Err(()) => sa_core::l("Anahtar okunamadı"),
            };
        }
        self.state.borrow().keys.get(&id).cloned().unwrap_or(ProviderKey::Loading).caption()
    }

    // MARK: providers (both layouts)

    fn provider(&self, id: Uuid) -> Option<TranslationProviderProfile> {
        self.deps.providers.providers().into_iter().find(|p| p.id == id)
    }

    /// Radio / row click: `None` = on-device models.
    pub fn activate_provider(&self, id: Option<Uuid>) {
        if id.is_some_and(|id| self.provider(id).is_none()) {
            return;
        }
        self.deps.providers.set_active(id);
        if self.flavor == Flavor::Windows {
            self.apply_providers();
        }
        self.changed();
    }

    fn offline_blocks(&self, base_url: &str) -> bool {
        self.deps.vm.offline_mode() && sends_text_over_internet(base_url)
    }

    /// "Test et" on a saved provider (macOS also "Uzaktaki modelleri listele").
    pub fn test_provider(&self, id: Uuid) {
        let Some(p) = self.provider(id) else { return };
        if self.flavor == Flavor::Mac && self.offline_blocks(&p.base_url) {
            self.forget_tests(id);
            let notice = Notice { text: sa_core::l("Çevrimdışı mod açık: internetteki sağlayıcılara bağlanılmaz."), is_error: true };
            if let Some(activity) = self.mac_activity() {
                activity.set_notice(id, Some(Self::shared_notice(&notice)));
            }
            self.state.borrow_mut().notices.insert(id, notice);
            self.changed();
            return;
        }
        // macOS: one test per provider at a time, wherever it was started (a picker's "Yenile" included).
        if self.mac_activity().is_some_and(|a| a.is_testing(id)) {
            return;
        }
        let test_id = {
            let mut st = self.state.borrow_mut();
            if st.testing.contains(&id) {
                return;
            }
            st.next_test += 1;
            let t = st.next_test;
            st.test_ids.insert(id, t);
            st.testing.insert(id);
            t
        };
        if let Some(activity) = self.mac_activity() {
            activity.begin_external_test(id);
        }
        self.changed();
        let tester = self.deps.tester.clone();
        let cancel = self.cancel.child();
        let key = self.ui_key();
        let ui = self.deps.ui.clone();
        let tested_url = p.base_url.clone();
        let _ = std::thread::Builder::new().name("sa-settings-test".into()).spawn(move || {
            let outcome = tester.test_saved(id, &cancel);
            key.post(&ui, move |m| m.finish_provider_test(id, test_id, tested_url, outcome));
        });
    }

    fn finish_provider_test(&self, id: Uuid, test_id: u64, tested_url: String, outcome: Option<TestOutcome>) {
        if self.is_closed() {
            return;
        }
        {
            let mut st = self.state.borrow_mut();
            if st.test_ids.get(&id) != Some(&test_id) {
                return;
            }
            st.test_ids.remove(&id);
            st.testing.remove(&id);
        }
        let Some(o) = outcome else {
            if let Some(activity) = self.mac_activity() {
                activity.end_external_test(id, None);
            }
            self.changed();
            return;
        };
        let current = self.provider(id);
        let notice = Notice { text: o.message, is_error: !o.success };
        match self.flavor {
            // Windows: the status InfoBar under the cards.
            Flavor::Windows => {
                self.state.borrow_mut().provider_status = Some(notice);
                self.apply_providers();
            }
            // macOS: ignore an answer for a provider that is gone or whose address changed meanwhile.
            Flavor::Mac => {
                let valid = current.is_some_and(|p| p.base_url == tested_url);
                if let Some(activity) = self.mac_activity() {
                    activity.end_external_test(id, valid.then(|| Self::shared_notice(&notice)));
                }
                if valid {
                    self.state.borrow_mut().notices.insert(id, notice);
                }
            }
        }
        self.changed();
    }

    /// "Düzenle".
    pub fn edit_provider(&self, id: Uuid) {
        let Some(p) = self.provider(id) else { return };
        match self.flavor {
            Flavor::Windows => {
                let key = match self.deps.win.as_ref() {
                    Some(w) => w.plain_key(id),
                    None => self.state.borrow().keys.get(&id).map(|k| k.value().to_owned()).unwrap_or_default(),
                };
                self.state.borrow_mut().win_form.begin_edit(&p, &key);
            }
            Flavor::Mac => {
                let cached = self.state.borrow().keys.get(&id).cloned().unwrap_or(ProviderKey::Loading);
                let settled = cached.is_settled();
                {
                    let mut st = self.state.borrow_mut();
                    st.mac_draft = Some(MacProviderDraft::editing(&p, cached));
                    st.mac_notice = None;
                    st.mac_key_not_stored = false;
                    st.mac_testing = false;
                    st.mac_test_epoch += 1;
                }
                if !settled {
                    self.load_keys(&[id]);
                }
            }
        }
        self.changed();
    }

    /// "Sil": Windows deletes at once; macOS asks first.
    pub fn delete_provider(&self, id: Uuid) {
        let Some(p) = self.provider(id) else { return };
        match self.flavor {
            Flavor::Windows => self.remove_provider(id),
            Flavor::Mac => {
                self.state.borrow_mut().confirm = Some(Confirm {
                    action: PendingConfirm::DeleteProvider(id),
                    title: sa_core::l!("“%@” sağlayıcısı silinsin mi?", p.name.as_str()),
                    message: sa_core::l("Kayıtlı API anahtarı da silinir."),
                    primary: sa_core::l("Sil"),
                });
                self.changed();
            }
        }
    }

    fn remove_provider(&self, id: Uuid) {
        if self.flavor == Flavor::Windows && self.state.borrow().win_form.editing == Some(id) {
            self.state.borrow_mut().win_form.reset();
        }
        self.deps.providers.remove(id);
        self.forget_key(id);
        self.forget_tests(id);
        if self.flavor == Flavor::Windows {
            self.apply_providers();
        }
        self.changed();
    }

    /// A row's model field / combo: Windows ignores blank values and applies; macOS stores the trimmed value.
    pub fn commit_provider_model(&self, id: Uuid, model: &str) {
        let Some(p) = self.provider(id) else { return };
        let model = model.trim();
        match self.flavor {
            Flavor::Windows => {
                if model.is_empty() {
                    return;
                }
                self.deps.providers.update_model(id, model);
                self.apply_providers();
            }
            Flavor::Mac => {
                if model == p.model {
                    return;
                }
                self.deps.providers.update_model(id, model);
            }
        }
        self.changed();
    }

    pub fn close_provider_status(&self) {
        self.state.borrow_mut().provider_status = None;
        self.changed();
    }

    /// macOS "API sağlayıcısı ekle…".
    pub fn add_provider(&self) {
        {
            let mut st = self.state.borrow_mut();
            st.mac_draft = Some(MacProviderDraft::new(preset_at(0)));
            st.mac_notice = None;
            st.mac_key_not_stored = false;
            st.mac_testing = false;
            st.mac_test_epoch += 1;
        }
        self.changed();
    }

    // MARK: provider form

    pub fn form_select_preset(&self, index: i32) {
        let index = index.max(0) as usize;
        {
            let mut st = self.state.borrow_mut();
            match self.flavor {
                Flavor::Windows => st.win_form.select_preset(index),
                Flavor::Mac => {
                    if let Some(d) = st.mac_draft.as_mut() {
                        d.apply_preset(preset_at(index));
                    }
                    st.mac_notice = None;
                }
            }
        }
        self.changed();
    }

    pub fn form_set_name(&self, v: &str) {
        let mut st = self.state.borrow_mut();
        match self.flavor {
            Flavor::Windows => {
                if st.win_form.name != v {
                    st.win_form.name = v.to_owned();
                    st.win_form.pristine = false;
                }
            }
            Flavor::Mac => {
                if let Some(d) = st.mac_draft.as_mut() {
                    d.name = v.to_owned();
                }
            }
        }
        drop(st);
        self.changed();
    }

    pub fn form_set_url(&self, v: &str) {
        let mut st = self.state.borrow_mut();
        match self.flavor {
            Flavor::Windows => {
                if st.win_form.url != v {
                    st.win_form.url = v.to_owned();
                    st.win_form.pristine = false;
                }
            }
            Flavor::Mac => {
                if let Some(d) = st.mac_draft.as_mut() {
                    d.set_base_url(v);
                }
            }
        }
        drop(st);
        self.changed();
    }

    pub fn form_set_key(&self, v: &str) {
        let mut st = self.state.borrow_mut();
        match self.flavor {
            Flavor::Windows => {
                if *st.win_form.key != v {
                    st.win_form.key = zeroize::Zeroizing::new(v.to_owned());
                    st.win_form.pristine = false;
                }
            }
            Flavor::Mac => {
                if let Some(d) = st.mac_draft.as_mut() {
                    d.set_api_key(v);
                }
            }
        }
        drop(st);
        self.changed();
    }

    pub fn form_remove_stored_key(&self) {
        if let Some(d) = self.state.borrow_mut().mac_draft.as_mut()
            && d.can_remove_stored_key()
        {
            d.remove_stored_key();
        }
        self.changed();
    }

    pub fn form_set_model(&self, v: &str) {
        let mut st = self.state.borrow_mut();
        match self.flavor {
            Flavor::Windows => {
                if st.win_form.model != v {
                    st.win_form.model = v.to_owned();
                    st.win_form.pristine = false;
                }
            }
            Flavor::Mac => {
                if let Some(d) = st.mac_draft.as_mut() {
                    d.model = v.to_owned();
                }
            }
        }
        drop(st);
        self.changed();
    }

    pub fn form_set_async(&self, v: bool) {
        let mut st = self.state.borrow_mut();
        match self.flavor {
            Flavor::Windows => {
                if st.win_form.async_translation != v {
                    st.win_form.async_translation = v;
                    st.win_form.pristine = false;
                }
            }
            Flavor::Mac => {
                if let Some(d) = st.mac_draft.as_mut() {
                    d.set_async_translation(v);
                }
            }
        }
        drop(st);
        self.changed();
    }

    /// "Bağlantıyı test et" of the form: tests what is typed, stores nothing.
    pub fn form_test(&self) {
        match self.flavor {
            Flavor::Windows => {
                // C#: url, key and model are trimmed when the button is clicked; the answer's chosen model is this
                // snapshot (or the first listed model when it was blank).
                let (preset, url, key, model) = {
                    let mut st = self.state.borrow_mut();
                    if st.win_form.testing {
                        return;
                    }
                    st.win_form.testing = true;
                    let f = &st.win_form;
                    (
                        f.preset().id.to_owned(),
                        f.url.trim().to_owned(),
                        zeroize::Zeroizing::new(f.key.trim().to_owned()),
                        f.model.trim().to_owned(),
                    )
                };
                self.changed();
                let tester = self.deps.tester.clone();
                let cancel = self.cancel.child();
                let key_ui = self.ui_key();
                let ui = self.deps.ui.clone();
                let _ = std::thread::Builder::new().name("sa-settings-form-test".into()).spawn(move || {
                    let outcome = tester.test_draft(&preset, &url, &key, &model, &cancel);
                    key_ui.post(&ui, move |m| m.finish_win_form_test(&model, outcome));
                });
            }
            Flavor::Mac => {
                let snapshot = {
                    let mut st = self.state.borrow_mut();
                    let Some(d) = st.mac_draft.clone() else { return };
                    st.mac_test_epoch += 1;
                    st.mac_testing = true;
                    st.mac_notice = None;
                    d
                };
                let epoch = self.state.borrow().mac_test_epoch;
                if self.offline_blocks(snapshot.base_url()) {
                    let mut st = self.state.borrow_mut();
                    st.mac_testing = false;
                    st.mac_notice = Some(Notice {
                        text: sa_core::l("Çevrimdışı mod açık: internetteki sağlayıcılara bağlanılmaz."),
                        is_error: true,
                    });
                    drop(st);
                    self.changed();
                    return;
                }
                self.changed();
                let tester = self.deps.tester.clone();
                let cancel = self.cancel.child();
                let key_ui = self.ui_key();
                let ui = self.deps.ui.clone();
                let preset = snapshot.preset_id().to_owned();
                let url = snapshot.base_url().to_owned();
                let key = zeroize::Zeroizing::new(snapshot.effective_key().to_owned());
                let model = snapshot.model.clone();
                let _ = std::thread::Builder::new().name("sa-settings-draft-test".into()).spawn(move || {
                    let outcome = tester.test_draft(&preset, &url, &key, &model, &cancel);
                    key_ui.post(&ui, move |m| m.finish_mac_draft_test(epoch, url, key, outcome));
                });
            }
        }
    }

    fn finish_win_form_test(&self, tested_model: &str, outcome: Option<TestOutcome>) {
        if self.is_closed() {
            return;
        }
        {
            let mut st = self.state.borrow_mut();
            st.win_form.testing = false;
            if let Some(o) = &outcome {
                st.win_form.apply_test_models(&o.models, tested_model);
                st.provider_status = Some(Notice { text: o.message.clone(), is_error: !o.success });
            }
        }
        self.changed();
    }

    fn finish_mac_draft_test(&self, epoch: u64, url: String, key: zeroize::Zeroizing<String>, outcome: Option<TestOutcome>) {
        if self.is_closed() {
            return;
        }
        {
            let mut st = self.state.borrow_mut();
            if st.mac_test_epoch != epoch {
                return;
            }
            let Some(o) = outcome else { return };
            st.mac_testing = false;
            // Typing another address or key meanwhile makes the answer about something else.
            let Some(d) = st.mac_draft.as_mut() else { return };
            if d.base_url() != url || d.effective_key() != key.as_str() {
                drop(st);
                self.changed();
                return;
            }
            if !o.models.is_empty() {
                d.discovered_models = o.models.clone();
                if d.model.trim().is_empty() {
                    d.model = o.models[0].clone();
                }
            }
            st.mac_notice = Some(Notice { text: o.message, is_error: !o.success });
        }
        self.changed();
    }

    /// "Kaydet ve Etkinleştir" / "Güncelle".
    pub fn form_save(&self) {
        match self.flavor {
            Flavor::Windows => self.save_win_form(),
            Flavor::Mac => self.save_mac_draft(),
        }
        self.changed();
    }

    fn save_win_form(&self) {
        let form = self.state.borrow().win_form.clone();
        let name = form.name.trim().to_owned();
        let url = form.url.trim().to_owned();
        let key = form.key.trim().to_owned();
        let model = form.model.trim().to_owned();
        if url.is_empty() {
            self.state.borrow_mut().provider_status =
                Some(Notice { text: sa_core::l("Lütfen geçerli bir sunucu adresi girin."), is_error: true });
            return;
        }
        let preset = form.preset();
        // C#: while editing, `UpdateProvider` of a provider that is gone (another instance removed it) does nothing and
        // `SetActiveProvider(id)` of an unknown id selects the on-device models; nothing is added.
        let target = match form.editing {
            Some(id) => match self.provider(id) {
                Some(p) => Some(p),
                None => {
                    self.deps.providers.set_active(None);
                    self.apply_providers();
                    let mut st = self.state.borrow_mut();
                    st.win_form.reset();
                    st.provider_status = Some(Notice { text: sa_core::l("Sağlayıcı güncellendi ve etkinleştirildi."), is_error: false });
                    return;
                }
            },
            None => None,
        };
        let message = match target {
            Some(existing) => {
                let mut p = existing.clone();
                p.name = if name.is_empty() { existing.preset().name(None) } else { name };
                p.base_url = url;
                p.model = model;
                p.async_translation = form.async_translation;
                self.deps.providers.update(p, Some(&key));
                self.deps.providers.set_active(Some(existing.id));
                self.record_key_written(existing.id, &key, true);
                sa_core::l("Sağlayıcı güncellendi ve etkinleştirildi.")
            }
            None => {
                let mut p = TranslationProviderProfile::from_preset(preset, form.async_translation);
                if !name.is_empty() {
                    p.name = name;
                }
                p.base_url = url;
                if !model.is_empty() {
                    p.model = model;
                }
                let added = self.deps.providers.add(p, (!key.is_empty()).then_some(key.as_str()), true);
                self.record_key_written(added.id, &key, true);
                sa_core::l!("%@ kaydedildi ve etkinleştirildi.", added.name.as_str())
            }
        };
        self.apply_providers();
        let mut st = self.state.borrow_mut();
        st.win_form.reset();
        st.provider_status = Some(Notice { text: message, is_error: false });
    }

    fn save_mac_draft(&self) {
        let Some(draft) = self.state.borrow().mac_draft.clone() else { return };
        if !draft.has_usable_address() {
            return;
        }
        {
            let mut st = self.state.borrow_mut();
            st.mac_test_epoch += 1;
            st.mac_testing = false;
        }
        let stored = if draft.is_new {
            let added = self.deps.providers.add(draft.profile(), None, true);
            let key = draft.api_key().to_owned();
            let stored = if key.trim().is_empty() { true } else { self.deps.providers.set_key(added.id, key.trim()) };
            self.record_key_written(added.id, &key, stored);
            // The provider exists now; saving again (after a refused key) updates it.
            if let Some(d) = self.state.borrow_mut().mac_draft.as_mut() {
                d.is_new = false;
                d.key_edited = true;
            }
            stored
        } else {
            let ok = self.deps.providers.update(draft.profile(), None);
            self.forget_tests(draft.id);
            match (ok, draft.key_to_save()) {
                // `writeKey`: a blank key removes the stored one; a refused write (also a refused removal) keeps the
                // sheet open with "API anahtarı kaydedilemedi".
                (true, Some(key)) => {
                    let stored = self.deps.providers.set_key(draft.id, key.trim());
                    self.record_key_written(draft.id, key, stored);
                    stored
                }
                _ => true,
            }
        };
        let mut st = self.state.borrow_mut();
        st.mac_key_not_stored = !stored;
        if stored {
            st.mac_draft = None;
            st.mac_notice = None;
        }
    }

    /// "Vazgeç": Windows resets the form; macOS closes the sheet (cancelling a running test).
    pub fn form_cancel(&self) {
        {
            let mut st = self.state.borrow_mut();
            match self.flavor {
                Flavor::Windows => st.win_form.reset(),
                Flavor::Mac => {
                    st.mac_draft = None;
                    st.mac_test_epoch += 1;
                    st.mac_testing = false;
                    st.mac_notice = None;
                    st.mac_key_not_stored = false;
                }
            }
        }
        self.changed();
    }

    // MARK: Appearance

    fn load_fonts(&self) {
        let fonts = self.deps.font_families.clone();
        let key = self.ui_key();
        let ui = self.deps.ui.clone();
        let _ = std::thread::Builder::new().name("sa-settings-fonts".into()).spawn(move || {
            let list = fonts();
            key.post(&ui, move |m| {
                if !m.is_closed() {
                    m.state.borrow_mut().fonts = list;
                    m.changed();
                }
            });
        });
    }

    pub fn select_theme(&self, index: i32) {
        if let Some(t) = ThemeChoice::ALL.get(index.max(0) as usize) {
            self.deps.appearance.set_theme(*t);
        }
        self.changed();
    }

    fn font_at(&self, index: i32) -> String {
        if index <= 0 {
            return String::new();
        }
        self.state.borrow().fonts.get(index as usize - 1).cloned().unwrap_or_default()
    }

    pub fn select_ui_font(&self, index: i32) {
        let f = self.font_at(index);
        self.deps.appearance.set_ui_font_family(f.trim());
        self.changed();
    }

    pub fn select_overlay_font(&self, index: i32) {
        let f = self.font_at(index);
        self.deps.appearance.set_font_family(f.trim());
        self.changed();
    }

    pub fn set_overlay_size(&self, v: f64) {
        let v = if v.is_finite() { v.clamp(sa_core::appearance::OverlayStyle::MIN_SIZE, sa_core::appearance::OverlayStyle::MAX_SIZE) } else { sa_core::appearance::OverlayStyle::DEFAULT_SIZE };
        // Windows: the slider's StepFrequency 0.001 snaps the value; the SwiftUI slider is continuous.
        let v = if self.flavor == Flavor::Windows { (v * 1000.0).round() / 1000.0 } else { v };
        if (self.deps.appearance.size() - v).abs() > 1e-9 {
            self.deps.appearance.set_size(v);
        }
        self.changed();
    }

    pub fn set_overlay_color(&self, r: u8, g: u8, b: u8) {
        let hex = sa_core::appearance::HexColor::format_rgb(sa_core::appearance::RgbColor { r, g, b });
        if self.deps.appearance.text_hex() != hex {
            self.deps.appearance.set_text_hex(&hex);
        }
        self.changed();
    }

    /// A typed `#RRGGBB` (invalid text is ignored).
    pub fn set_overlay_color_hex(&self, text: &str) {
        let t = text.trim();
        let t = if t.starts_with('#') { t.to_owned() } else { format!("#{t}") };
        if let Some(c) = sa_core::appearance::HexColor::parse_rgb(&t) {
            self.set_overlay_color(c.r, c.g, c.b);
        } else {
            self.changed();
        }
    }

    pub fn select_overlay_background(&self, index: i32) {
        if let Some(b) = OverlayBackground::ALL.get(index.max(0) as usize) {
            self.deps.appearance.set_background(*b);
        }
        self.changed();
    }

    pub fn set_overlay_opacity(&self, v: f64) {
        let v = if v.is_finite() { v.clamp(0.0, 1.0) } else { sa_core::appearance::OverlayStyle::DEFAULT_OPACITY };
        // Windows: StepFrequency 0.01; macOS: continuous.
        let v = if self.flavor == Flavor::Windows { (v * 100.0).round() / 100.0 } else { v };
        if (self.deps.appearance.opacity() - v).abs() > 1e-9 {
            self.deps.appearance.set_opacity(v);
        }
        self.changed();
    }

    /// "Varsayılana döndür": the five overlay keys only (theme and interface font untouched).
    pub fn reset_overlay(&self) {
        self.deps.appearance.reset_overlay();
        self.changed();
    }

    // MARK: Models

    pub fn request_model_delete(&self, id: &str) {
        let Some(m) = self.deps.catalog.model(id) else { return };
        if !self.deps.catalog.can_delete(id) {
            return;
        }
        let row = ModelRow::new(&m, false, false, None);
        self.state.borrow_mut().confirm = Some(Confirm {
            action: PendingConfirm::DeleteModel(id.to_owned()),
            title: row.delete_confirm_title,
            message: row.delete_confirm_message,
            primary: sa_core::l("Sil"),
        });
        self.changed();
    }

    fn delete_model(&self, id: &str) {
        let key = self.ui_key();
        let ui = self.deps.ui.clone();
        self.deps.catalog.delete(
            id,
            Box::new(move |freed| {
                key.post(&ui, move |m| {
                    if m.is_closed() {
                        return;
                    }
                    if let Some(mb) = freed {
                        m.state.borrow_mut().freed_mb = Some(mb);
                    }
                    m.changed();
                });
            }),
        );
        self.changed();
    }

    pub fn close_models_notice(&self) {
        self.state.borrow_mut().freed_mb = None;
        self.deps.catalog.dismiss_delete_notice();
        self.changed();
    }

    // MARK: confirmation

    pub fn confirm_accept(&self) {
        let Some(c) = self.state.borrow_mut().confirm.take() else { return };
        match c.action {
            PendingConfirm::DeleteModel(id) => self.delete_model(&id),
            PendingConfirm::DeleteProvider(id) => self.remove_provider(id),
        }
        self.changed();
    }

    pub fn confirm_dismiss(&self) {
        self.state.borrow_mut().confirm = None;
        self.changed();
    }

    // MARK: snapshot

    pub fn view(&self) -> View {
        let d = &self.deps;
        let vm = &d.vm;
        let st = self.state.borrow();
        let mac = self.flavor == Flavor::Mac;

        let speech_codes: Vec<&str> = SubtitleLanguage::ALL.iter().map(|l| l.code).collect();
        let models = vm.available_models();
        let model_ids: Vec<String> = models.iter().map(|m| m.id.clone()).collect();
        let (tm_ids, tm_sep) = self.translation_model_ids();
        let speaker_index = SPEAKER_COUNTS.iter().position(|c| *c == vm.speaker_count()).unwrap_or(0) as i32;
        let cloud = vm.active_cloud_provider_name();

        // providers
        let providers = d.providers.providers();
        let active = d.providers.active_provider_id();
        let provider_rows = providers
            .iter()
            .map(|p| {
                let preset_name = p.preset().name(None);
                let notice = self.provider_notice(&st, p.id);
                let notice = notice.as_ref();
                ProviderRowView {
                    id: p.id.to_string(),
                    name: p.name.clone(),
                    shows_preset_tag: p.name.to_lowercase() != preset_name.to_lowercase(),
                    preset_name,
                    summary: format!("{} · {}", p.base_url, self.key_caption(p.id)),
                    is_active: active == Some(p.id),
                    is_testing: st.testing.contains(&p.id) || self.mac_activity().is_some_and(|a| a.is_testing(p.id)),
                    model: p.model.clone(),
                    models: p.available_models(mac),
                    notice_text: notice.map(|n| n.text.clone()).unwrap_or_default(),
                    notice_error: notice.is_some_and(|n| n.is_error),
                }
            })
            .collect();

        // appearance
        let a = &d.appearance;
        let style = a.style();
        let font_index = |family: &str| -> i32 {
            let f = family.trim();
            if f.is_empty() { 0 } else { st.fonts.iter().position(|x| x == f).map(|i| i as i32 + 1).unwrap_or(0) }
        };
        let mut font_items = vec![(sa_core::l("Sistem"), false)];
        font_items.extend(st.fonts.iter().enumerate().map(|(i, f)| (f.clone(), i == 0)));
        let color = style.text_color();

        // models
        let catalog = &d.catalog;
        let rows = |rows: Vec<ModelRow>| -> Vec<ModelRowView> {
            rows.into_iter()
                .map(|r| ModelRowView {
                    is_active: catalog.is_active(&r.id),
                    is_deleting: catalog.is_deleting(&r.id),
                    rating_text: if mac {
                        r.rating_text.clone().unwrap_or_default()
                    } else {
                        sa_core::l!(
                            "Hız %lld/5 · Doğruluk %lld/5",
                            r.model.speed.clamp(0, 5),
                            r.model.accuracy.clamp(0, 5)
                        )
                    },
                    id: r.id,
                    name: r.name,
                    language_summary: r.language_summary,
                    storage_status: r.storage_status,
                    license_text: r.license_text,
                    is_non_commercial: r.is_non_commercial,
                    error_text: r.error_text.unwrap_or_default(),
                    shows_delete: r.shows_delete_button,
                    can_delete: r.can_delete,
                    delete_help: r.delete_help,
                })
                .collect()
        };
        let translation_rows = if mac {
            let mut v = rows(catalog.translation_rows());
            // macOS lists llama first (catalog.translation_models order).
            let order: Vec<String> = catalog.llama_translation_models().into_iter().chain(catalog.classic_translation_models()).map(|m| m.id).collect();
            v.sort_by_key(|r| order.iter().position(|id| *id == r.id).unwrap_or(usize::MAX));
            v
        } else {
            rows(catalog.translation_rows())
        };
        let notice_text = if mac {
            catalog.delete_notice().map(|n| n.text())
        } else {
            st.freed_mb.map(|mb| sa_core::l!("%@ boşaltıldı.", ModelLabel::size(mb)))
        };

        // form
        let form = match self.flavor {
            Flavor::Windows => {
                let f = &st.win_form;
                FormView {
                    open: true,
                    is_new: f.editing.is_none(),
                    header: f.header(),
                    save_caption: f.save_caption(),
                    preset_items: preset_names(),
                    preset_index: f.preset_index as i32,
                    name: f.name.clone(),
                    url: f.url.clone(),
                    url_placeholder: "https://opencode.ai/inference/go/openai/v1".into(),
                    is_lan: f.preset().id == sa_enginekit::presets::TranslationProviderPreset::LAN_ID,
                    key: f.key.to_string(),
                    key_placeholder: sa_core::l("sk-... (Yerel ağ için boş bırakılabilir)"),
                    key_note: String::new(),
                    can_remove_key: false,
                    model: f.model.clone(),
                    model_items: f.model_items.clone(),
                    async_translation: f.async_translation,
                    testing: f.testing,
                    can_test: !f.testing,
                    can_save: true,
                    notice_text: String::new(),
                    notice_error: false,
                    key_not_stored: false,
                }
            }
            Flavor::Mac => match &st.mac_draft {
                None => FormView::default(),
                Some(dr) => FormView {
                    open: true,
                    is_new: dr.is_new,
                    header: if dr.is_new { sa_core::l("API Sağlayıcısı Ekle") } else { sa_core::l("API Sağlayıcısını Düzenle") },
                    save_caption: if dr.is_new { sa_core::l("Kaydet ve Etkinleştir") } else { sa_core::l("Güncelle") },
                    preset_items: preset_names(),
                    // `draft.preset` (unknown id → custom, the last preset), unlike the Windows combo (→ 0).
                    preset_index: super::providers::preset_index(dr.preset().id) as i32,
                    name: dr.name.clone(),
                    url: dr.base_url().to_owned(),
                    url_placeholder: dr.url_placeholder(),
                    is_lan: dr.preset().id == sa_enginekit::presets::TranslationProviderPreset::LAN_ID,
                    key: dr.api_key().to_owned(),
                    key_placeholder: dr.key_prompt(),
                    key_note: dr.stored_key_note().unwrap_or_default(),
                    can_remove_key: dr.can_remove_stored_key(),
                    model: dr.model.clone(),
                    model_items: dr.available_models(),
                    async_translation: dr.async_translation(),
                    testing: st.mac_testing,
                    can_test: !st.mac_testing && dr.has_usable_address(),
                    can_save: dr.has_usable_address(),
                    notice_text: st.mac_notice.as_ref().map(|n| n.text.clone()).unwrap_or_default(),
                    notice_error: st.mac_notice.as_ref().is_some_and(|n| n.is_error),
                    key_not_stored: st.mac_key_not_stored,
                },
            },
        };

        let cache_gb = self.cache_max_gb();
        let associations_registered = self.associations_registered();
        self.assoc_shown.set(Some(associations_registered));
        View {
            page: st.page,
            page_title: self.flavor.page_title(st.page),
            scroll_epoch: st.scroll_epoch,
            ui_language_items: LanguageChoice::ALL.iter().map(|c| c.title()).collect(),
            ui_language_index: index_of(&LanguageChoice::ALL, &d.language.choice()),
            needs_relaunch: d.language.needs_relaunch(),
            speech_language_items: SubtitleLanguage::ALL.iter().map(|l| l.title()).collect(),
            speech_language_index: index_of(&speech_codes, &vm.language().as_str()).max(0),
            model_items: models.iter().map(|m| ModelLabel::name(&m.id)).collect(),
            model_index: index_of(&model_ids, &vm.model()),
            model_enabled: !models.is_empty(),
            translation_model_items: tm_ids.iter().map(|id| ModelLabel::name(id)).collect(),
            translation_model_index: index_of(&tm_ids, &vm.translation_model()),
            translation_model_separator: tm_sep,
            translation_model_enabled: !tm_ids.is_empty(),
            diarize: vm.diarize(),
            speaker_count_items: SPEAKER_COUNTS
                .iter()
                .map(|c| if *c == 0 { sa_core::l("Otomatik") } else { c.to_string() })
                .collect(),
            speaker_count_index: speaker_index,
            auto_play: vm.auto_play_video(),
            offline: vm.offline_mode(),
            offline_footer: match &cloud {
                Some(name) => sa_core::l!(
                    "Açıkken internete hiç bağlanılmaz ve eksik modeller indirilmez; bulut çeviri sağlayıcısı (%@) da kullanılmaz. Kapalıyken çevrilecek metin bu sağlayıcıya gönderilir. Ses hiçbir durumda dışarı gönderilmez.",
                    name.as_str()
                ),
                None => sa_core::l(
                    "Açıkken internete hiç bağlanılmaz ve eksik modeller indirilmez. Ses ve metin hiçbir durumda dışarı gönderilmez.",
                ),
            },
            lan_enabled: st.lan.enabled,
            lan_url: st.lan.url.clone(),
            lan_busy: st.lan_busy,
            lan_test_open: st.lan_test.is_some(),
            lan_test_success: st.lan_test.as_ref().is_some_and(|n| !n.is_error),
            lan_test_text: st.lan_test.as_ref().map(|n| n.text.clone()).unwrap_or_default(),
            lan_models: st.lan.models.clone(),
            lan_model: st.lan.model.clone(),
            lan_async: st.lan.async_translation,
            cache_usage_text: match st.cache_used {
                Some(used) => sa_core::l!(
                    "Şifreli önbellek kullanımı: %@ / %lld GB",
                    sa_core::text::format_byte_size_windows(used, LanguageState::shared().language()),
                    cache_gb
                ),
                None => sa_core::l("Önbellek kullanımı hesaplanıyor…"),
            },
            cache_gb,
            clear_cache_enabled: !st.cache_busy && st.cache_used.unwrap_or(0) > 0,
            associations_registered,
            associations_busy: st.assoc_busy,
            associations_bar_open: st.assoc_bar.is_some(),
            associations_bar_severity: st.assoc_bar.as_ref().map(|b| b.0).unwrap_or(0),
            associations_bar_text: st.assoc_bar.as_ref().map(|b| b.1.clone()).unwrap_or_default(),
            local_provider_active: active.is_none(),
            provider_rows,
            cloud_callout: cloud
                .map(|n| sa_core::l!("%@ bir bulut hizmeti: çevrilecek metin ona gönderilir. Ses ve video gönderilmez.", n.as_str()))
                .unwrap_or_default(),
            provider_status_open: st.provider_status.is_some(),
            provider_status_success: st.provider_status.as_ref().is_some_and(|n| !n.is_error),
            provider_status_text: st.provider_status.as_ref().map(|n| n.text.clone()).unwrap_or_default(),
            form,
            theme_index: index_of(&ThemeChoice::ALL, &a.theme()).max(0),
            font_items,
            ui_font_index: font_index(&a.ui_font_family()),
            overlay_font_index: font_index(&style.font_family),
            overlay_font_family: style.font_family.clone(),
            overlay_size: style.size_fraction,
            overlay_size_text: a.size_text(),
            overlay_color: (color.r, color.g, color.b),
            overlay_color_hex: style.text_hex.to_uppercase(),
            overlay_background_index: index_of(&OverlayBackground::ALL, &style.background).max(0),
            overlay_opacity: style.background_opacity,
            overlay_opacity_text: a.opacity_text(),
            transcription_rows: rows(catalog.transcription_rows()),
            diarization_rows: rows(catalog.diarization_rows()),
            translation_rows,
            models_loading: catalog.is_loading(),
            models_load_error: catalog.load_error().map(|e| e.text()).unwrap_or_default(),
            models_total_text: catalog.total_disk_text(),
            models_notice_open: notice_text.is_some(),
            models_notice_text: notice_text.unwrap_or_default(),
            confirm: st.confirm.clone(),
        }
    }
}

