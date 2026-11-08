//! UI language, the process-wide language state and the persisted language choice (win-core §4.1, §4.5–4.7;
//! mac-enginekit §5.1–5.2; mac-viewmodels §2.2).

use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;

use crate::settings::SettingsStore;
use crate::subscription::{Subscribers, Subscription};

/// The two UI languages. `code()` is also the `SUBTITLE_ENGINE_LANG` value and the catalog language key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum UiLanguage {
    #[default]
    Tr,
    En,
}

impl UiLanguage {
    pub const ALL: [UiLanguage; 2] = [UiLanguage::Tr, UiLanguage::En];

    /// `"tr"` / `"en"`.
    pub const fn code(self) -> &'static str {
        match self {
            UiLanguage::Tr => "tr",
            UiLanguage::En => "en",
        }
    }

    /// Trim + ASCII lowercase; `"tr"`/`"en"` match, anything else is `None` (`"EN"` → En, `"de"` → None).
    pub fn parse(code: &str) -> Option<UiLanguage> {
        match code.trim().to_ascii_lowercase().as_str() {
            "tr" => Some(UiLanguage::Tr),
            "en" => Some(UiLanguage::En),
            _ => None,
        }
    }

    /// The "System" choice: the first preferred BCP-47 tag whose primary subtag (before the first `-` or `_`) is
    /// `tr` or `en`; `En` when none matches (also for an empty list).
    ///
    /// Sources: Windows `GetUserPreferredUILanguages` / `GlobalizationPreferences.Languages`; macOS the global-domain
    /// `AppleLanguages` (never the per-app override). Both are read by `sa-platform`.
    pub fn system<S: AsRef<str>>(preferred: &[S]) -> UiLanguage {
        for tag in preferred {
            let tag = tag.as_ref();
            let primary = tag.split(['-', '_']).next().unwrap_or("");
            if let Some(lang) = UiLanguage::parse(primary) {
                return lang;
            }
        }
        UiLanguage::En
    }

    /// Decimal separator of the language's culture (`%f`, `%@` of floats, byte sizes): `,` for tr, `.` for en.
    pub const fn decimal_separator(self) -> char {
        match self {
            UiLanguage::Tr => ',',
            UiLanguage::En => '.',
        }
    }
}

/// The process-wide current UI language (default `Tr`). Thread-safe; the setter notifies subscribers on the
/// setter's thread, outside the lock, and only when the value changes.
pub struct LanguageState {
    inner: Mutex<(UiLanguage, u64)>,
    subscribers: Subscribers<UiLanguage>,
}

impl LanguageState {
    /// A fresh state (tests). The app uses [`LanguageState::shared`].
    pub fn new(initial: UiLanguage) -> Arc<Self> {
        Arc::new(Self { inner: Mutex::new((initial, 0)), subscribers: Subscribers::new() })
    }

    /// The process-wide singleton, initially `Tr`.
    pub fn shared() -> &'static Arc<LanguageState> {
        static SHARED: OnceLock<Arc<LanguageState>> = OnceLock::new();
        SHARED.get_or_init(|| LanguageState::new(UiLanguage::Tr))
    }

    /// The current language.
    pub fn language(&self) -> UiLanguage {
        self.inner.lock().0
    }

    /// Monotonic counter bumped on every effective change (the Slint `Loc.revision` mirrors it).
    pub fn revision(&self) -> u64 {
        self.inner.lock().1
    }

    /// Sets the language. Equal value → no notification.
    pub fn set_language(&self, language: UiLanguage) {
        {
            let mut g = self.inner.lock();
            if g.0 == language {
                return;
            }
            g.0 = language;
            g.1 += 1;
        }
        self.subscribers.notify(&language);
    }

    /// Subscribes to language changes (`language_changed(new)`). The handler runs on the setter's thread; UI code
    /// re-posts to the event loop.
    pub fn subscribe(&self, handler: impl Fn(&UiLanguage) + Send + Sync + 'static) -> Subscription {
        self.subscribers.subscribe(handler)
    }
}

/// The language picker value persisted under `uiLanguage` (`system` / `tr` / `en`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum LanguageChoice {
    #[default]
    System,
    Turkish,
    English,
}

impl LanguageChoice {
    /// Picker order.
    pub const ALL: [LanguageChoice; 3] = [LanguageChoice::System, LanguageChoice::Turkish, LanguageChoice::English];

    pub const fn raw(self) -> &'static str {
        match self {
            LanguageChoice::System => "system",
            LanguageChoice::Turkish => "tr",
            LanguageChoice::English => "en",
        }
    }

    /// Exact, case-sensitive.
    pub fn parse(raw: Option<&str>) -> Option<LanguageChoice> {
        match raw? {
            "system" => Some(LanguageChoice::System),
            "tr" => Some(LanguageChoice::Turkish),
            "en" => Some(LanguageChoice::English),
            _ => None,
        }
    }

    /// Picker label: `Türkçe` / `English` are never localized; System is `L("Sistem")`.
    pub fn title(self) -> String {
        match self {
            LanguageChoice::Turkish => "Türkçe".into(),
            LanguageChoice::English => "English".into(),
            LanguageChoice::System => super::l("Sistem"),
        }
    }

    pub fn resolved<S: AsRef<str>>(self, preferred: &[S]) -> UiLanguage {
        match self {
            LanguageChoice::Turkish => UiLanguage::Tr,
            LanguageChoice::English => UiLanguage::En,
            LanguageChoice::System => UiLanguage::system(preferred),
        }
    }
}

/// Persisted language choice that drives [`LanguageState`] (win-core §4.6, mac-viewmodels §2.2).
///
/// Platform side effects (`SetProcessPreferredUILanguages` / per-app `AppleLanguages`, macOS `needs_relaunch`) are
/// done by the caller through `PlatformShell::apply_process_ui_language`.
pub struct AppLanguage {
    settings: Arc<dyn SettingsStore>,
    system_languages: Box<dyn Fn() -> Vec<String> + Send + Sync>,
    state: Arc<LanguageState>,
    choice: Mutex<LanguageChoice>,
    system_preferred: Mutex<Vec<String>>,
    launch_language: UiLanguage,
    subscribers: Subscribers<AppLanguageChange>,
}

/// What changed on an [`AppLanguage`] (C# property names `Choice` / `Effective`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppLanguageChange {
    Choice,
    Effective,
}

impl AppLanguage {
    /// Settings key.
    pub const DEFAULTS_KEY: &'static str = "uiLanguage";

    /// Reads `uiLanguage` (unknown → System), snapshots the system list and applies the effective language to `state`.
    pub fn new(
        settings: Arc<dyn SettingsStore>,
        system_languages: impl Fn() -> Vec<String> + Send + Sync + 'static,
        state: Arc<LanguageState>,
    ) -> Self {
        let system_preferred = system_languages();
        let choice =
            LanguageChoice::parse(settings.get_string(Self::DEFAULTS_KEY).as_deref()).unwrap_or(LanguageChoice::System);
        let effective = choice.resolved(&system_preferred);
        state.set_language(effective);
        Self {
            settings,
            system_languages: Box::new(system_languages),
            state,
            choice: Mutex::new(choice),
            system_preferred: Mutex::new(system_preferred),
            launch_language: effective,
            subscribers: Subscribers::new(),
        }
    }

    pub fn choice(&self) -> LanguageChoice {
        *self.choice.lock()
    }

    /// Equal value → nothing. Otherwise store, notify `Choice`, persist, then [`apply`](Self::apply).
    pub fn set_choice(&self, choice: LanguageChoice) {
        {
            let mut g = self.choice.lock();
            if *g == choice {
                return;
            }
            *g = choice;
        }
        self.subscribers.notify(&AppLanguageChange::Choice);
        self.settings.set_string(Self::DEFAULTS_KEY, choice.raw());
        self.apply();
    }

    /// `choice.resolved(system_preferred)`.
    pub fn effective(&self) -> UiLanguage {
        let choice = self.choice();
        choice.resolved(&self.system_preferred.lock())
    }

    /// The language the process started with (macOS `needs_relaunch` hint compares against it).
    pub fn launch_language(&self) -> UiLanguage {
        self.launch_language
    }

    /// macOS: system menus/panels follow the language only after a relaunch.
    pub fn needs_relaunch(&self) -> bool {
        self.effective() != self.launch_language
    }

    /// Re-reads the system list, sets the language state and always notifies `Effective`.
    pub fn apply(&self) {
        *self.system_preferred.lock() = (self.system_languages)();
        self.state.set_language(self.effective());
        self.subscribers.notify(&AppLanguageChange::Effective);
    }

    /// Re-reads the system language list and applies it.
    pub fn refresh_system_languages(&self) {
        self.apply();
    }

    pub fn subscribe(&self, handler: impl Fn(&AppLanguageChange) + Send + Sync + 'static) -> Subscription {
        self.subscribers.subscribe(handler)
    }
}
