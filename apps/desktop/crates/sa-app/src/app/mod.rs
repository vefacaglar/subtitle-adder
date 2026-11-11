//! The app shell: composes every crate into one running app (win-ui-main §1, mac-ui §2; ARCHITECTURE.md §8).
//!
//! * [`AppShell::build`] creates the process-wide parts in the platform's launch order: diagnostics, shell, settings,
//!   language, broker, stores (profiles, media index, providers, Windows video cache), the engine client, the model
//!   catalog and the shared view models (subtitle editor, file explorer, hub) plus the Settings window host.
//! * [`AppShell::open_window`] creates one main window with its own player, controller and hub presenter (macOS
//!   File ▸ New Window opens more windows sharing the view models, mac-ui §3.1).
//! * [`run`] is the binary's entry: startup steps, the event loop, then the shutdown order (win-ui-main §1.5,
//!   mac-ui §2.1 step 10).
//!
//! Data root: the platform folder, or `SUBTITLE_ADDER_DATA_DIR` (tests and tools; never the user's data). With the
//! override the Windows broker uses its own channel scope, so a test instance never talks to a real one.

#[cfg(windows)]
mod win_parts;

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use slint::ComponentHandle;

use sa_core::appearance::{AppearanceSettings, ThemeChoice};
use sa_core::cancel::CancellationToken;
use sa_core::diagnostics::{DiagnosticFormat, DiagnosticLog, DiagnosticSink};
use sa_core::dispatch::UiDispatcher;
use sa_core::l10n::{AppLanguage, LanguageState};
use sa_core::models::ModelCatalog;
use sa_core::platform::{PlatformShell, ThumbnailGenerator};
use sa_core::settings::{SettingsStore, keys};
use sa_core::subscription::Subscription;
use sa_enginekit::client::{CatalogBackend, EngineClient, ProcessEngineClient};
use sa_enginekit::command::EngineCommand;
use sa_enginekit::launch::EngineLaunchState;
use sa_media::MediaIndex;
use sa_player::MediaPlayerBackend;
use sa_profiles::ProfileStore;
use sa_providers::ProviderStore;
use sa_sync::SyncBus;
use sa_viewmodels::explorer::FileExplorerModel;
use sa_viewmodels::hub::VideoPlatformModel;
use sa_viewmodels::uiref::UiKey;
use sa_viewmodels::{AppServices, PlatformPolicy, SubtitleViewModel};

use crate::hub::{HubFlavor, HubPresenter, HubServices};
use crate::main_window::translation_picker::{PickerDeps, TranslationPickerPresenter};
use crate::main_window::{MainWindowController, MainWindowDeps, MainWindowHooks, PreviewMaker};
use crate::settings_window::{SettingsDeps, SettingsWindowHost};
use crate::ui::{self, HubBridge, MainWindow};

/// Creates the player backend of one window.
pub type PlayerFactory = Box<dyn Fn() -> Box<dyn MediaPlayerBackend>>;

/// How the shell is composed. [`AppOptions::production`] is what the binary uses; tests replace the dispatcher (no
/// event loop), the player (no audio device) and point the data folder at a temp dir.
pub struct AppOptions {
    /// The app data folder; `None` = `PlatformShell::app_data_dir()` (honors `SUBTITLE_ADDER_DATA_DIR`).
    pub data_dir: Option<PathBuf>,
    /// The engine; `None` = `EngineCommand::locate_default()` (`SUBTITLE_ENGINE` → bundled → repo `engine/dist`).
    pub engine: Option<EngineCommand>,
    /// UI dispatcher; `None` = the Slint event loop ([`crate::dispatcher::SlintDispatcher`]).
    pub ui: Option<Arc<dyn UiDispatcher>>,
    /// The player of each window; `None` = FFmpeg (on Windows through the encrypted SSD chunk cache).
    pub player: Option<PlayerFactory>,
    /// Attach the native window chrome after the window exists (real windows only).
    pub native_chrome: bool,
    /// Windows: start the background startup work (temp cleanup, file-association self-registration, provider
    /// model refresh). Off in tests.
    pub startup_side_effects: bool,
    /// Closing the last main window quits (Windows, win-ui-main §1.5). Off on macOS, where the app keeps its menu
    /// bar and a Dock click or an opened file brings a main window back (mac-ui §2.3).
    pub last_window_quits: bool,
}

impl AppOptions {
    pub fn production() -> Self {
        Self {
            data_dir: None,
            engine: None,
            ui: None,
            player: None,
            native_chrome: true,
            startup_side_effects: true,
            last_window_quits: !cfg!(target_os = "macos"),
        }
    }
}

/// One open main window.
pub struct AppWindow {
    pub window: MainWindow,
    pub controller: Rc<MainWindowController>,
    pub hub: HubPresenter,
    /// The toolbar translation-model flyout (Windows) / popover (macOS).
    pub picker: Rc<TranslationPickerPresenter>,
}

/// The process-wide parts (shared by every main window and the Settings window).
pub struct AppShell {
    pub policy: Arc<PlatformPolicy>,
    pub ui: Arc<dyn UiDispatcher>,
    pub data_dir: PathBuf,
    pub diagnostics: Arc<DiagnosticLog>,
    pub shell: Arc<dyn PlatformShell>,
    pub settings: Arc<dyn SettingsStore>,
    pub language: Arc<AppLanguage>,
    pub appearance: Arc<AppearanceSettings>,
    pub bus: Arc<dyn SyncBus>,
    pub profiles: Arc<dyn ProfileStore>,
    pub media_index: Arc<dyn MediaIndex>,
    pub providers: Arc<dyn ProviderStore>,
    pub engine: Option<Arc<dyn EngineClient>>,
    pub services: AppServices,
    pub catalog: Rc<ModelCatalog>,
    pub vm: Rc<SubtitleViewModel>,
    pub explorer: Rc<FileExplorerModel>,
    pub hub: Rc<VideoPlatformModel>,
    pub settings_host: Rc<SettingsWindowHost>,
    pub windows: RefCell<Vec<Rc<AppWindow>>>,
    #[cfg(windows)]
    pub win: win_parts::WinParts,
    player: Option<PlayerFactory>,
    native_chrome: bool,
    last_window_quits: bool,
    /// The provider test state shared by every picker and the Settings window.
    picker_providers: Rc<sa_viewmodels::providers_ui::ProvidersModel>,
    key: Cell<Option<UiKey<AppShell>>>,
    subs: RefCell<Vec<Subscription>>,
    shut_down: Cell<bool>,
}

fn platform_policy() -> PlatformPolicy {
    // The policy follows the compiled UI tree, so the macOS tree rendered on Windows (sa-shot) behaves like macOS.
    if crate::UI_TREE == "mac" { PlatformPolicy::mac() } else { PlatformPolicy::windows() }
}

impl AppShell {
    /// Builds every process-wide part on the UI thread (after the Slint backend exists, so posts reach the loop).
    pub fn build(options: AppOptions) -> Rc<Self> {
        crate::select_catalog_chain();
        let policy = Arc::new(platform_policy());
        let ui: Arc<dyn UiDispatcher> = options.ui.clone().unwrap_or_else(|| crate::dispatcher::SlintDispatcher::new());

        // Diagnostics first: every later part may log (redacting the configured API keys).
        let format = if cfg!(target_os = "macos") { DiagnosticFormat::Mac } else { DiagnosticFormat::Windows };
        let probe_shell = sa_platform::platform_shell(Arc::new(sa_core::diagnostics::NullDiagnostics));
        let data_dir = options.data_dir.clone().unwrap_or_else(|| probe_shell.app_data_dir());
        let _ = std::fs::create_dir_all(&data_dir);
        let diagnostics = DiagnosticLog::new(
            DiagnosticLog::default_path(format, &data_dir),
            format,
            Box::new(|| EngineLaunchState::shared().secrets()),
        );
        #[cfg(windows)]
        sa_platform::win::crash::set_diagnostic_log(diagnostics.clone());
        let diag: Arc<dyn DiagnosticSink> = diagnostics.clone();
        let shell = sa_platform::platform_shell(diag.clone());

        // Broker (Windows: the C#-compatible named-pipe broker; macOS: in process).
        #[cfg(windows)]
        let pipe_bus = {
            let scope = match sa_core::platform::data_dir_override().or_else(|| options.data_dir.clone()) {
                // A separate data folder never shares a channel with the user's real instances.
                Some(dir) => Some(format!("{}|{}", sa_sync::default_scope(), dir.display())),
                None => None,
            };
            sa_sync::NamedPipeBus::new(scope.as_deref())
        };
        #[cfg(windows)]
        let bus: Arc<dyn SyncBus> = pipe_bus.clone();
        #[cfg(not(windows))]
        let bus: Arc<dyn SyncBus> = sa_sync::InProcessBus::shared().clone();

        // Settings (win-ui-main §1.1 step 5; macOS CFPreferences, or a JSON file under an overridden data folder).
        #[cfg(windows)]
        let json_settings = sa_core::settings::JsonFileSettingsStore::open(
            sa_core::settings::JsonFileSettingsStore::default_path(&data_dir),
            sa_core::settings::JsonFileSettingsStore::DEFAULT_SAVE_DELAY,
            Some(Arc::new(win_parts::BusSettingsHook { bus: bus.clone() })),
        );
        #[cfg(windows)]
        let settings: Arc<dyn SettingsStore> = json_settings.clone();
        #[cfg(not(windows))]
        let settings: Arc<dyn SettingsStore> = mac_settings(&data_dir, options.data_dir.is_some());

        // Language before any localized string is built (win-ui-main §1.1 step 7, mac-ui §2.1 step 4).
        let language = {
            let s = shell.clone();
            Arc::new(AppLanguage::new(settings.clone(), move || s.system_preferred_languages(), LanguageState::shared().clone()))
        };
        #[cfg(windows)]
        if options.startup_side_effects {
            sa_platform::win::startup::after_settings(settings.as_ref(), language.effective());
        }
        #[cfg(target_os = "macos")]
        if options.startup_side_effects {
            // mac-ui §2.1 step 5: nothing runs yet.
            sa_enginekit::working_dir::clean_up(&std::env::temp_dir());
        }
        let appearance = AppearanceSettings::new(settings.clone());

        // Stores.
        #[cfg(windows)]
        let win = win_parts::WinParts::build(&data_dir, &pipe_bus, bus.clone(), settings.as_ref(), json_settings);
        #[cfg(windows)]
        let (profiles, media_index, providers): (Arc<dyn ProfileStore>, Arc<dyn MediaIndex>, Arc<dyn ProviderStore>) =
            (win.profiles.clone(), win.media_index.clone(), win.providers.clone());
        #[cfg(not(windows))]
        let (profiles, media_index, providers) = mac_stores(&data_dir, settings.as_ref());

        // Engine (ARCHITECTURE.md §8.4).
        let engine: Option<Arc<dyn EngineClient>> = options.engine.clone().or_else(EngineCommand::locate_default).map(|command| {
            Arc::new(ProcessEngineClient::new(command, sa_enginekit::process::platform_process_control(), diag.clone()))
                as Arc<dyn EngineClient>
        });
        let catalog = Rc::new(ModelCatalog::new(
            engine.clone().map(|e| Arc::new(CatalogBackend(e)) as Arc<dyn sa_core::models::ModelCatalogBackend>),
            ui.clone(),
        ));
        let thumbnails: Arc<dyn ThumbnailGenerator> = sa_platform::thumbnail_generator();

        let services = AppServices {
            policy: policy.clone(),
            settings: settings.clone(),
            ui: ui.clone(),
            engine: engine.clone(),
            launch_state: EngineLaunchState::shared().clone(),
            profiles: profiles.clone(),
            media_index: media_index.clone(),
            providers: providers.clone(),
            shell: shell.clone(),
            bus: bus.clone(),
            diagnostics: diag.clone(),
            thumbnails,
        };
        let vm = Rc::new(SubtitleViewModel::new(services.clone(), catalog.clone()));
        let explorer = Rc::new(FileExplorerModel::new(services.clone()));
        let hub = Rc::new(VideoPlatformModel::new(services.clone()));
        {
            // mac-ui §2.1 step 9 (both platforms): a saved subtitle refreshes the sidebar badge.
            let explorer = Rc::downgrade(&explorer);
            vm.on_subtitle_saved(Box::new(move |path| {
                if let Some(e) = explorer.upgrade() {
                    e.subtitle_saved(path);
                }
            }));
        }

        // One provider test state for every window's translation picker and the Settings window (macOS
        // `vm.testingProviders` / `vm.providerNotices` on the shared view model). Saved-provider tests of the picker's
        // "Yenile": Windows = `WinProviderManager::test_draft` (C# messages, `IsTesting`); macOS = EngineKit's
        // `test_connection` with the SwiftUI notices.
        #[cfg(windows)]
        let picker_tester = win.picker_tester();
        #[cfg(not(windows))]
        let picker_tester: Arc<dyn sa_viewmodels::providers_ui::ProviderConnectionTester> =
            sa_viewmodels::providers_ui::EngineKitConnectionTester::new(Arc::new(sa_enginekit::external_llm::UreqFetch));
        let picker_providers = Rc::new(sa_viewmodels::providers_ui::ProvidersModel::with_tester(services.clone(), picker_tester));

        let settings_host = SettingsWindowHost::new(SettingsDeps {
            settings: settings.clone(),
            ui: ui.clone(),
            vm: vm.clone(),
            catalog: catalog.clone(),
            appearance: appearance.clone(),
            language: language.clone(),
            providers: providers.clone(),
            #[cfg(windows)]
            tester: win.tester(),
            #[cfg(not(windows))]
            tester: Arc::new(crate::settings_window::deps::ServerTester {
                providers: providers.clone(),
                fetch: Arc::new(sa_enginekit::external_llm::UreqFetch),
            }),
            #[cfg(windows)]
            win: Some(win.settings_ops()),
            #[cfg(not(windows))]
            win: None,
            font_families: crate::settings_window::deps::system_font_families(),
            activity: Some(picker_providers.clone()),
        });

        let app = Rc::new(AppShell {
            policy,
            ui,
            data_dir,
            diagnostics,
            shell,
            settings,
            language,
            appearance,
            bus,
            profiles,
            media_index,
            providers,
            engine,
            services,
            catalog,
            vm,
            explorer,
            hub,
            settings_host,
            windows: RefCell::new(Vec::new()),
            #[cfg(windows)]
            win,
            player: options.player,
            native_chrome: options.native_chrome,
            last_window_quits: options.last_window_quits,
            picker_providers,
            key: Cell::new(None),
            subs: RefCell::new(Vec::new()),
            shut_down: Cell::new(false),
        });
        app.key.set(Some(UiKey::register(&app)));
        app.follow_theme();
        app
    }

    fn ui_key(&self) -> UiKey<AppShell> {
        self.key.get().expect("registered")
    }

    /// Starts the background work of the first window (mac-ui §2.2; win-ui-main §1.1 steps 4, 11–12).
    pub fn start(self: &Rc<Self>, side_effects: bool) {
        self.catalog.load();
        self.explorer.start();
        self.hub.start();
        if !side_effects {
            return;
        }
        #[cfg(windows)]
        {
            win_parts::cleanup_temp_directory_in_background(self.shell.clone(), self.bus.clone());
            self.win.refresh_initial_external_models();
        }
        #[cfg(not(windows))]
        {
            // mac-ui §2.2: the active local provider's models are fetched 500 ms after the window appears.
            let providers = self.picker_providers.clone();
            slint::Timer::single_shot(Duration::from_millis(500), move || providers.refresh_active_provider_models_if_needed());
        }
    }

    /// Opens a main window (macOS ⌘N opens more, sharing the view models).
    pub fn open_window(self: &Rc<Self>) -> Result<Rc<AppWindow>, slint::PlatformError> {
        let window = MainWindow::new()?;
        crate::loc::install(&window);
        crate::symbols::install(&window);
        crate::loc::follow_language_changes(window.as_weak());
        apply_theme(&window, self.appearance.theme());

        let picker = self.translation_picker(&window);
        let mut hooks = self.hooks(&window);
        {
            let p = Rc::downgrade(&picker);
            hooks.translation_model_clicked = Some(Rc::new(move || {
                if let Some(p) = p.upgrade() {
                    p.open();
                }
            }));
        }
        let deps = MainWindowDeps {
            vm: self.vm.clone(),
            explorer: Some(self.explorer.clone()),
            hub: Some(self.hub.clone()),
            settings: self.settings.clone(),
            appearance: Some(self.appearance.clone()),
            shell: self.shell.clone(),
            ui: self.ui.clone(),
            player: self.new_player(),
            source_factory: None,
            preview: self.preview_maker(),
            subtitle_reader: None,
            hooks,
        };
        let controller = MainWindowController::attach(&window, deps);
        let hub = HubPresenter::attach(
            &window,
            self.hub.clone(),
            HubServices { ui: self.ui.clone(), shell: Some(self.shell.clone()), flavor: HubFlavor::native() },
        );
        {
            let c = Rc::downgrade(&controller);
            hub.on_play(move |path| {
                if let Some(c) = c.upgrade() {
                    c.play_from_hub(path);
                }
            });
        }
        let entry = Rc::new(AppWindow { window, controller, hub, picker });
        {
            // The window's close button / Alt+F4 / ⌘W.
            let key = self.ui_key();
            let weak = Rc::downgrade(&entry);
            entry.window.window().on_close_requested(move || {
                if let (Some(app), Some(w)) = (key.get(), weak.upgrade()) {
                    app.window_closing(&w);
                }
                slint::CloseRequestResponse::HideWindow
            });
        }
        self.windows.borrow_mut().push(entry.clone());
        Ok(entry)
    }

    /// Shows the window and, on a real window, attaches the native chrome once winit created it.
    pub fn show_window(self: &Rc<Self>, w: &Rc<AppWindow>) -> Result<(), slint::PlatformError> {
        w.window.show()?;
        if self.native_chrome {
            attach_chrome_when_ready(Rc::downgrade(w), 120);
        }
        Ok(())
    }

    fn new_player(&self) -> Box<dyn MediaPlayerBackend> {
        if let Some(f) = &self.player {
            return f();
        }
        #[cfg(windows)]
        return self.win.player();
        #[cfg(not(windows))]
        return Box::new(sa_player::ffmpeg_backend::FfmpegPlayer::new());
    }

    /// The window's translation-model picker over the app-wide `ProvidersModel` (the platform's connection tester;
    /// test flags and notices shared with the other windows and Settings); "API sağlayıcılarını yönet…" opens
    /// Settings on the providers page (Windows `general`, macOS `translation`).
    fn translation_picker(self: &Rc<Self>, window: &MainWindow) -> Rc<TranslationPickerPresenter> {
        let key = self.ui_key();
        let page = match self.policy.settings_manage_providers_page {
            sa_viewmodels::policy::SettingsPage::Translation => "translation",
            _ => "general",
        };
        let providers = self.picker_providers.clone();
        TranslationPickerPresenter::attach(
            window,
            PickerDeps {
                services: self.services.clone(),
                catalog: self.catalog.clone(),
                vm: self.vm.clone(),
                providers,
                manage_providers: Rc::new(move || {
                    if let Some(app) = key.get() {
                        let _ = app.settings_host.show(Some(page));
                    }
                }),
            },
        )
    }

    fn hooks(self: &Rc<Self>, window: &MainWindow) -> MainWindowHooks {
        let key = self.ui_key();
        let weak = window.as_weak();
        MainWindowHooks {
            open_settings: Some(Rc::new(move || {
                if let Some(app) = key.get() {
                    let _ = app.settings_host.show(None);
                }
            })),
            profile_clicked: Some(Rc::new(move || {
                if let Some(w) = weak.upgrade() {
                    let hub = w.global::<HubBridge>();
                    hub.set_popover_open_epoch(hub.get_popover_open_epoch() + 1);
                }
            })),
            new_window: if cfg!(target_os = "macos") {
                Some(Rc::new(move || {
                    if let Some(app) = key.get()
                        && let Ok(w) = app.open_window()
                    {
                        let _ = app.show_window(&w);
                    }
                }))
            } else {
                None
            },
            ..Default::default()
        }
    }

    /// macOS preview fallback: the engine's `preview` command into `<tmp>/SubtitleAdder/preview-<uuid>.mp4`.
    fn preview_maker(&self) -> Option<PreviewMaker> {
        if !cfg!(target_os = "macos") {
            return None;
        }
        let engine = self.engine.clone()?;
        Some(Arc::new(move |source: &Path| {
            let dir = sa_enginekit::working_dir::ensure_exists(&std::env::temp_dir());
            let out = dir.join(format!("preview-{}.mp4", uuid::Uuid::new_v4().hyphenated().to_string().to_uppercase()));
            engine.make_preview(source, &out, &CancellationToken::new()).map(|_| out).map_err(|e| e.to_string())
        }))
    }

    /// Re-applies `appTheme` to every window when the setting changes (live on both platforms).
    fn follow_theme(self: &Rc<Self>) {
        #[cfg(target_os = "macos")]
        sa_platform::mac::MacShell::apply_app_theme(Some(self.appearance.theme().raw()));
        let key = self.ui_key();
        let ui = self.ui.clone();
        let sub = self.settings.subscribe(Box::new(move |k| {
            if k == keys::APP_THEME {
                key.post(&ui, |app| app.apply_theme_everywhere());
            }
        }));
        self.subs.borrow_mut().push(sub);
    }

    fn apply_theme_everywhere(&self) {
        let theme = self.appearance.theme();
        #[cfg(target_os = "macos")]
        sa_platform::mac::MacShell::apply_app_theme(Some(theme.raw()));
        for w in self.windows.borrow().iter() {
            apply_theme(&w.window, theme);
        }
    }

    /// macOS Dock reopen with no window left (mac-ui §2.3): shows an open main window that is not on screen, or opens
    /// a new one like File ▸ New Window.
    pub fn reopen_main_window(self: &Rc<Self>) {
        let existing = self.windows.borrow().first().cloned();
        match existing {
            Some(w) => {
                let _ = w.window.show();
            }
            None => {
                if let Ok(w) = self.open_window() {
                    let _ = self.show_window(&w);
                }
            }
        }
    }

    /// Opens files from the command line, a second instance or Finder in the key (first) window. On macOS with no
    /// window left (Finder, a drop on the Dock icon) a new main window opens for them, like the SwiftUI
    /// `WindowGroup` with `handlesExternalEvents`.
    pub fn open_external_files(self: &Rc<Self>, paths: Vec<PathBuf>) {
        if self.windows.borrow().is_empty() && !self.last_window_quits {
            self.reopen_main_window();
        }
        let first = self.windows.borrow().first().cloned();
        match first {
            Some(w) => {
                let _ = w.window.show();
                w.controller.open_external_files(paths);
            }
            None => self.vm.open_external_files(paths),
        }
    }

    /// A main window is closing: its controller detaches (saves the Windows size, disposes the player).
    fn window_closing(&self, w: &Rc<AppWindow>) {
        w.controller.detach();
        self.windows.borrow_mut().retain(|x| !Rc::ptr_eq(x, w));
        if self.windows.borrow().is_empty() && self.last_window_quits {
            // Windows: closing the main window quits (win-ui-main §1.5); macOS keeps the menu bar alive.
            self.settings_host.close();
            let _ = slint::quit_event_loop();
        }
    }

    /// The shutdown order (win-ui-main §1.5 step 3, mac-ui §2.1 step 10). Idempotent.
    pub fn shutdown(&self) {
        if self.shut_down.replace(true) {
            return;
        }
        for w in self.windows.borrow_mut().drain(..) {
            w.controller.detach();
        }
        self.settings_host.close();
        self.hub.prepare_for_quit();
        let had_work = self.vm.has_active_work();
        self.vm.shutdown();
        self.subs.borrow_mut().clear();
        #[cfg(windows)]
        {
            let _ = had_work;
            self.win.shutdown(self.bus.as_ref());
        }
        #[cfg(not(windows))]
        {
            if !had_work {
                sa_enginekit::working_dir::clean_up(&std::env::temp_dir());
            }
            self.bus.dispose();
            self.settings.flush();
        }
        self.diagnostics.flush();
    }
}

/// `appTheme`: System follows the OS, Light/Dark force the scheme (the theme globals derive `dark` from it).
pub fn apply_theme(window: &MainWindow, theme: ThemeChoice) {
    let dark = match theme {
        ThemeChoice::System => crate::settings_window::host::system_uses_dark(),
        ThemeChoice::Light => false,
        ThemeChoice::Dark => true,
    };
    let scheme = if dark { slint::language::ColorScheme::Dark } else { slint::language::ColorScheme::Light };
    window.global::<ui::Palette>().set_color_scheme(scheme);
}

fn attach_chrome_when_ready(w: std::rc::Weak<AppWindow>, tries: u32) {
    use slint::winit_030::WinitWindowAccessor;
    let Some(entry) = w.upgrade() else { return };
    if entry.window.window().has_winit_window() {
        entry.controller.attach_native_chrome();
        return;
    }
    if tries > 0 {
        slint::Timer::single_shot(Duration::from_millis(16), move || attach_chrome_when_ready(w, tries - 1));
    }
}

#[cfg(not(windows))]
fn mac_settings(data_dir: &Path, overridden: bool) -> Arc<dyn SettingsStore> {
    // An overridden data folder (tests, tools) keeps its settings in a JSON file next to the data, never in the
    // user's preferences domain.
    if overridden || sa_core::platform::data_dir_override().is_some() {
        return sa_core::settings::JsonFileSettingsStore::open(
            sa_core::settings::JsonFileSettingsStore::default_path(data_dir),
            sa_core::settings::JsonFileSettingsStore::DEFAULT_SAVE_DELAY,
            None,
        );
    }
    #[cfg(target_os = "macos")]
    return sa_platform::mac::defaults::UserDefaultsSettingsStore::new(sa_platform::mac::MacShell::defaults_domain());
    #[cfg(not(target_os = "macos"))]
    return sa_core::settings::InMemorySettingsStore::new();
}

#[cfg(not(windows))]
fn mac_stores(data_dir: &Path, settings: &dyn SettingsStore) -> (Arc<dyn ProfileStore>, Arc<dyn MediaIndex>, Arc<dyn ProviderStore>) {
    #[cfg(target_os = "macos")]
    {
        // No Keychain: remember-me and provider keys live in the AES-GCM sidecars under a 0600 key file.
        let secrets: Arc<dyn sa_core::secrets::SecretStore> = sa_providers::secrets::EncryptedSecretStore::for_app_data(data_dir);
        let profiles = sa_profiles::mac::MacProfileStore::new(
            sa_profiles::mac::MacProfileStoreOptions {
                file: Some(data_dir.join("profiles.json")),
                thumbnails_folder: Some(data_dir.join("thumbnails")),
                // mac-ui §2.1 step 8: the file is read later by `platform.start()` on a background queue.
                reads_file_immediately: false,
                ..Default::default()
            },
            secrets.clone(),
        );
        let index = sa_media::mac::MacMediaIndex::new(Some(data_dir.join("media-index.json")), None);
        let providers = sa_providers::mac::MacProviderStore::new(sa_providers::mac::MacProviderStore::default_file(data_dir), secrets);
        providers.migrate_legacy_if_needed(settings);
        (profiles as Arc<dyn ProfileStore>, index as Arc<dyn MediaIndex>, providers as Arc<dyn ProviderStore>)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (data_dir, settings);
        unimplemented!("only Windows and macOS are supported")
    }
}

/// The binary's entry (`SubtitleAdder.exe` / `SubtitleAdderApp`).
pub fn run() -> Result<(), slint::PlatformError> {
    // Windows steps 1–3: crash log, command-line paths, single instance (forwarded = exit 0, no window).
    #[cfg(windows)]
    let primary = match sa_platform::win::startup::begin(std::env::args_os()) {
        sa_platform::win::startup::Startup::Exit => return Ok(()),
        sa_platform::win::startup::Startup::Primary(p) => p,
    };
    #[cfg(target_os = "macos")]
    sa_platform::mac::MacShell::prepare_process();

    // The backend exists before anything posts to the loop (the window attributes give macOS its unified title bar).
    let backend = slint::BackendSelector::new().with_winit_window_attributes_hook(crate::main_window::chrome::winit_attributes);
    backend.select()?;

    let app = AppShell::build(AppOptions::production());
    let first = app.open_window()?;
    app.show_window(&first)?;
    app.start(true);

    // External opens: Windows second instances (pipe), macOS Apple Events (coalesced). Delivered on the UI thread.
    #[cfg(windows)]
    let mut coordinator = {
        let mut c = primary.coordinator;
        install_external_sink(&mut c, &app);
        if !primary.initial_paths.is_empty() {
            app.open_external_files(primary.initial_paths);
        }
        c
    };
    #[cfg(target_os = "macos")]
    let _external = {
        use sa_core::platform::ExternalOpenSource;
        let mut source = sa_platform::mac::open_documents::OpenDocumentsHandler::install();
        let _ = source.acquire_or_forward(&[]);
        install_external_sink(&mut source, &app);
        // Menu bar icons behave like SwiftUI's template `systemImage` icons (highlight, disabled).
        sa_platform::mac::appkit::keep_menu_icons_template();
        // Dock-icon click with no window left opens one (mac-ui §2.3); unhide/deminiaturize happen in the handler.
        let key = app.ui_key();
        let ui = app.ui.clone();
        source.set_reopen_handler(Box::new(move || key.post(&ui, |app| app.reopen_main_window())));
        source
    };
    #[cfg(not(any(windows, target_os = "macos")))]
    let _external = {
        let mut source = sa_platform::external_open_source();
        let _ = source.acquire_or_forward(&[]);
        install_external_sink(source.as_mut(), &app);
        source
    };

    let result = if cfg!(target_os = "macos") { slint::run_event_loop_until_quit() } else { slint::run_event_loop() };

    app.shutdown();
    #[cfg(windows)]
    coordinator.dispose();
    result
}

fn install_external_sink(source: &mut dyn sa_core::platform::ExternalOpenSource, app: &Rc<AppShell>) {
    let key = app.ui_key();
    let ui = app.ui.clone();
    source.set_sink(Box::new(move |paths| {
        key.post(&ui, move |app| app.open_external_files(paths));
    }));
}
