//! Slint glue of the Settings window: one window per process, the model's [`View`] pushed into `SettingsBridge`
//! (lists replaced only when their content changed, so focus and scroll positions survive), callbacks forwarded to
//! the model. Refreshes are coalesced: view-model changes go through `SettingsRefreshSubscription` (job progress
//! never rebuilds the controls), model changes through one queued push.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use sa_core::appearance::ThemeChoice;
use sa_core::settings::keys;
use sa_viewmodels::settings_refresh::SettingsRefreshSubscription;
use sa_viewmodels::uiref::UiKey;
use uuid::Uuid;

use super::deps::SettingsDeps;
use super::model::{Flavor, ModelRowView, ProviderRowView, SettingsModel, View};
use crate::ui;

/// Minimum / default size of the Windows Settings window (DIP).
pub const WIN_MIN: (f32, f32) = (640.0, 520.0);
pub const WIN_DEFAULT: (f32, f32) = (860.0, 760.0);
/// macOS: fixed width, per-tab height.
pub const MAC_WIDTH: f32 = 560.0;

/// Owns the (single) Settings window of the process.
pub struct SettingsWindowHost {
    deps: SettingsDeps,
    flavor: Flavor,
    open: RefCell<Option<OpenWindow>>,
    key: Cell<Option<UiKey<SettingsWindowHost>>>,
    theme: Cell<Option<ThemeChoice>>,
}

impl Drop for SettingsWindowHost {
    fn drop(&mut self) {
        if let Some(k) = self.key.get() {
            k.unregister();
        }
    }
}

struct OpenWindow {
    window: ui::SettingsWindow,
    model: Rc<SettingsModel>,
    refresh: Rc<SettingsRefreshSubscription>,
    _alive: Rc<Cell<bool>>,
    /// The native chrome (Windows tree on a real window only).
    #[cfg(all(windows, sa_ui = "win"))]
    chrome: Option<Rc<sa_platform::win::chrome::WindowChrome>>,
    /// The open NSColorPanel session of the color well (real Mac only).
    #[cfg(target_os = "macos")]
    color_panel: Rc<RefCell<Option<sa_platform::mac::color_panel::ColorPanelSession>>>,
}

impl SettingsWindowHost {
    pub fn new(deps: SettingsDeps) -> Rc<Self> {
        Self::with_flavor(deps, Flavor::native())
    }

    /// Like [`new`](Self::new) for the other layout (tests, previews).
    pub fn with_flavor(deps: SettingsDeps, flavor: Flavor) -> Rc<Self> {
        let host = Rc::new(Self { deps, flavor, open: RefCell::new(None), key: Cell::new(None), theme: Cell::new(None) });
        host.key.set(Some(UiKey::register(&host)));
        host
    }

    fn ui_key(&self) -> UiKey<SettingsWindowHost> {
        self.key.get().expect("registered")
    }

    pub fn is_open(&self) -> bool {
        self.open.borrow().is_some()
    }

    /// The open window (tests / sa-shot).
    pub fn window(&self) -> Option<ui::SettingsWindow> {
        self.open.borrow().as_ref().map(|o| o.window.clone_strong())
    }

    /// The Windows chrome is attached to the open window (always `false` elsewhere).
    pub fn has_native_chrome(&self) -> bool {
        #[cfg(all(windows, sa_ui = "win"))]
        return self.open.borrow().as_ref().is_some_and(|o| o.chrome.is_some());
        #[cfg(not(all(windows, sa_ui = "win")))]
        return false;
    }

    pub fn model(&self) -> Option<Rc<SettingsModel>> {
        self.open.borrow().as_ref().map(|o| o.model.clone())
    }

    /// `App.ShowSettings(page)`: creates the window if missing, navigates to `page` (Windows `general|appearance|
    /// models`, macOS `settingsTab` raw values; `None` keeps / reads the stored tab) and brings it to front.
    pub fn show(self: &Rc<Self>, page: Option<&str>) -> Result<(), slint::PlatformError> {
        if self.open.borrow().is_none() {
            let opened = self.create()?;
            *self.open.borrow_mut() = Some(opened);
            let (window, model) = {
                let o = self.open.borrow();
                let o = o.as_ref().unwrap();
                (o.window.clone_strong(), o.model.clone())
            };
            model.show_page(page);
            self.restore_size(&window);
            window.show()?;
            // winit creates the native window once the event loop runs: native setup waits for it.
            self.when_native_window(60);
            model.window_loaded();
        } else if let Some(o) = self.open.borrow().as_ref() {
            if page.is_some() {
                o.model.show_page(page);
            }
            o.window.show()?;
            o.window.window().set_minimized(false);
        }
        self.push_now();
        Ok(())
    }

    /// Closing the main window closes Settings too.
    pub fn close(&self) {
        let Some(o) = self.open.borrow_mut().take() else { return };
        #[cfg(all(windows, sa_ui = "win"))]
        match &o.chrome {
            Some(c) => super::chrome::save_size(c, self.deps.settings.as_ref()),
            None => self.save_size(&o.window),
        }
        #[cfg(not(all(windows, sa_ui = "win")))]
        self.save_size(&o.window);
        o.refresh.dispose();
        o.model.close();
        #[cfg(target_os = "macos")]
        if o.color_panel.borrow_mut().take().is_some() {
            sa_platform::mac::color_panel::close();
        }
        self.theme.set(None);
        let _ = o.window.hide();
    }

    fn create(self: &Rc<Self>) -> Result<OpenWindow, slint::PlatformError> {
        let window = ui::SettingsWindow::new()?;
        crate::loc::install(&window);
        crate::symbols::install(&window);
        crate::loc::follow_language_changes(window.as_weak());
        let model = SettingsModel::new(self.deps.clone(), self.flavor);
        let alive = Rc::new(Cell::new(true));

        // Model → view: one queued push per burst of changes.
        let pending = Rc::new(Cell::new(false));
        {
            let weak = Rc::downgrade(self);
            let pending = pending.clone();
            model.set_on_changed(move || {
                if pending.replace(true) {
                    return;
                }
                let weak = weak.clone();
                let pending = pending.clone();
                let _ = slint::Timer::single_shot(std::time::Duration::ZERO, move || {
                    pending.set(false);
                    if let Some(host) = weak.upgrade() {
                        host.push_now();
                    }
                });
            });
        }

        // View-model → refresh (coalesced, filtered: progress events are ignored). The refresh closure must be
        // `Send`, so it reaches the host through a UI-thread key.
        let host_key = self.ui_key();
        let refresh = {
            let ui = self.deps.ui.clone();
            Rc::new(SettingsRefreshSubscription::new(
                move |job| ui.post(job),
                move |_refill| {
                    if let Some(host) = host_key.get() {
                        host.push_now();
                    }
                },
            ))
        };
        {
            let refresh = Rc::downgrade(&refresh);
            let alive = alive.clone();
            self.deps.vm.subscribe_changes(Box::new(move |change| {
                if alive.get()
                    && let Some(r) = refresh.upgrade()
                {
                    r.on_view_model_change(change);
                }
            }));
        }

        self.bind(&window, &model);
        #[cfg(target_os = "macos")]
        let color_panel = {
            let session = Rc::new(RefCell::new(None));
            window.global::<ui::SettingsBridge>().set_native_color_panel(true);
            let m = Rc::downgrade(&model);
            let weak = window.as_weak();
            let slot = Rc::downgrade(&session);
            window.global::<ui::SettingsBridge>().on_overlay_color_panel_requested(move || {
                let (Some(w), Some(slot)) = (weak.upgrade(), slot.upgrade()) else { return };
                let c = w.global::<ui::SettingsBridge>().get_overlay_color();
                let initial = [c.red() as f64 / 255.0, c.green() as f64 / 255.0, c.blue() as f64 / 255.0];
                let m = m.clone();
                // `HexColor.hex(from:)`: components clamped, ×255 rounded.
                let to_u8 = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
                *slot.borrow_mut() = Some(sa_platform::mac::color_panel::show(
                    initial,
                    Box::new(move |rgb| {
                        if let Some(m) = m.upgrade() {
                            m.set_overlay_color(to_u8(rgb[0]), to_u8(rgb[1]), to_u8(rgb[2]));
                        }
                    }),
                ));
            });
            session
        };
        {
            let weak = Rc::downgrade(self);
            window.window().on_close_requested(move || {
                if let Some(host) = weak.upgrade() {
                    host.close();
                }
                slint::CloseRequestResponse::HideWindow
            });
        }
        Ok(OpenWindow {
            window,
            model,
            refresh,
            _alive: alive,
            #[cfg(all(windows, sa_ui = "win"))]
            chrome: None,
            #[cfg(target_os = "macos")]
            color_panel,
        })
    }

    // MARK: native window setup

    /// Runs the native setup once winit has created the window (retries every 16 ms, `tries` times; a software
    /// renderer window never gets one, so nothing happens there).
    fn when_native_window(self: &Rc<Self>, tries: u32) {
        let Some(window) = self.window() else { return };
        if !self.setup_native(&window) && tries > 0 {
            let weak = Rc::downgrade(self);
            slint::Timer::single_shot(std::time::Duration::from_millis(16), move || {
                if let Some(h) = weak.upgrade()
                    && h.is_open()
                {
                    h.when_native_window(tries - 1);
                }
            });
        }
    }

    /// `false` while the native window does not exist yet.
    fn setup_native(self: &Rc<Self>, window: &ui::SettingsWindow) -> bool {
        use slint::winit_030::WinitWindowAccessor;
        if !window.window().has_winit_window() {
            return false;
        }
        let host = Rc::downgrade(self);
        // The OS theme changed: `appTheme = system` re-resolves.
        let on_theme: Box<dyn Fn()> = Box::new(move || {
            if let Some(h) = host.upgrade() {
                h.theme.set(None);
                h.push_now();
            }
        });
        #[cfg(all(windows, sa_ui = "win"))]
        {
            let chrome = super::chrome::attach(window, self.deps.settings.as_ref(), on_theme);
            if let Some(o) = self.open.borrow_mut().as_mut() {
                o.chrome = chrome;
            }
        }
        #[cfg(not(all(windows, sa_ui = "win")))]
        {
            window.window().on_winit_window_event(move |_, event| {
                if matches!(event, slint::winit_030::winit::event::WindowEvent::ThemeChanged(_)) {
                    on_theme();
                }
                slint::winit_030::EventResult::Propagate
            });
        }
        #[cfg(target_os = "macos")]
        if self.flavor == Flavor::Mac {
            // Preferences window: not resizable, close button only (mac-ui §3.5).
            use slint::winit_030::winit::window::WindowButtons;
            let _ = window.window().with_winit_window(|w| {
                w.set_resizable(false);
                w.set_enabled_buttons(WindowButtons::CLOSE);
            });
        }
        self.theme.set(None);
        self.push_now();
        true
    }

    // MARK: window size (Windows: persisted physical size, written on close when restored)

    fn restore_size(&self, window: &ui::SettingsWindow) {
        let w = window.window();
        match self.flavor {
            Flavor::Windows => {
                let scale = w.scale_factor();
                let s = &self.deps.settings;
                let (min_w, min_h) = ((WIN_MIN.0 * scale) as i64, (WIN_MIN.1 * scale) as i64);
                match (s.get_int(keys::SETTINGS_WINDOW_WIDTH), s.get_int(keys::SETTINGS_WINDOW_HEIGHT)) {
                    (Some(pw), Some(ph)) if pw > 0 && ph > 0 => {
                        w.set_size(slint::PhysicalSize::new(pw.max(min_w) as u32, ph.max(min_h) as u32));
                    }
                    _ => w.set_size(slint::LogicalSize::new(WIN_DEFAULT.0, WIN_DEFAULT.1)),
                }
            }
            Flavor::Mac => {}
        }
    }

    fn save_size(&self, window: &ui::SettingsWindow) {
        if self.flavor != Flavor::Windows {
            return;
        }
        let w = window.window();
        if w.is_maximized() || w.is_minimized() || w.is_fullscreen() {
            return;
        }
        let size = w.size();
        if size.width > 0 && size.height > 0 {
            self.deps.settings.set_int(keys::SETTINGS_WINDOW_WIDTH, size.width as i64);
            self.deps.settings.set_int(keys::SETTINGS_WINDOW_HEIGHT, size.height as i64);
        }
    }

    // MARK: push

    /// Pushes the model's view into the bridge now.
    pub fn push_now(&self) {
        let o = self.open.borrow();
        let Some(o) = o.as_ref() else { return };
        let view = o.model.view();
        let theme = self.deps.appearance.theme();
        if self.theme.get() != Some(theme) {
            self.theme.set(Some(theme));
            apply_theme_to(&o.window, theme);
            #[cfg(all(windows, sa_ui = "win"))]
            if let Some(c) = &o.chrome {
                super::chrome::set_dark(c, &o.window);
            }
        }
        push(&o.window, &view, self.flavor);
    }

    // MARK: callbacks

    fn bind(self: &Rc<Self>, window: &ui::SettingsWindow, model: &Rc<SettingsModel>) {
        let b = window.global::<ui::SettingsBridge>();
        let m = Rc::downgrade(model);
        macro_rules! on {
            ($setter:ident, |$m:ident $(, $arg:ident)*| $body:expr) => {{
                let w = m.clone();
                b.$setter(move |$($arg),*| {
                    if let Some($m) = w.upgrade() {
                        $body;
                    }
                });
            }};
        }
        on!(on_page_selected, |m, page| m.select_page(page));
        // window chrome
        {
            let weak = window.as_weak();
            b.on_minimize_requested(move || {
                if let Some(w) = weak.upgrade() {
                    w.window().set_minimized(true);
                }
            });
            let weak = window.as_weak();
            b.on_maximize_toggle_requested(move || {
                if let Some(w) = weak.upgrade() {
                    let max = !w.window().is_maximized();
                    w.window().set_maximized(max);
                    w.global::<ui::SettingsBridge>().set_is_maximized(max);
                }
            });
            let weak = window.as_weak();
            b.on_title_double_clicked(move || {
                if let Some(w) = weak.upgrade() {
                    let max = !w.window().is_maximized();
                    w.window().set_maximized(max);
                    w.global::<ui::SettingsBridge>().set_is_maximized(max);
                }
            });
            let host = Rc::downgrade(self);
            b.on_close_requested(move || {
                if let Some(h) = host.upgrade() {
                    h.close();
                }
            });
            let weak = window.as_weak();
            b.on_drag_requested(move || {
                if let Some(w) = weak.upgrade() {
                    drag_window(&w);
                }
            });
        }
        // General
        on!(on_ui_language_selected, |m, i| m.select_ui_language(i));
        on!(on_speech_language_selected, |m, i| m.select_speech_language(i));
        on!(on_model_selected, |m, i| m.select_model(i));
        on!(on_translation_model_selected, |m, i| m.select_translation_model(i));
        on!(on_diarize_toggled, |m, v| m.set_diarize(v));
        on!(on_speaker_count_selected, |m, i| m.select_speaker_count(i));
        on!(on_auto_play_toggled, |m, v| m.set_auto_play(v));
        on!(on_offline_toggled, |m, v| m.set_offline(v));
        // LAN
        on!(on_lan_toggled, |m, v| m.set_lan_enabled(v));
        on!(on_lan_url_edited, |m, t| m.set_lan_url(&t));
        on!(on_lan_test, |m| m.lan_test());
        on!(on_lan_test_closed, |m| m.close_lan_test());
        on!(on_lan_model_committed, |m, t| m.commit_lan_model(&t));
        on!(on_lan_refresh_models, |m| m.lan_refresh_models());
        on!(on_lan_async_toggled, |m, v| m.set_lan_async(v));
        // cache + associations
        on!(on_cache_gb_edited, |m, t| m.edit_cache_gb(&t));
        on!(on_clear_cache, |m| m.clear_cache());
        on!(on_associations_toggle, |m| m.toggle_associations());
        on!(on_default_apps, |m| m.open_default_apps());
        on!(on_associations_bar_closed, |m| m.close_associations_bar());
        // providers
        on!(on_provider_activate, |m, id| m.activate_provider(parse_id(&id)));
        on!(on_provider_test, |m, id| if let Some(id) = parse_id(&id) { m.test_provider(id) });
        on!(on_provider_edit, |m, id| if let Some(id) = parse_id(&id) { m.edit_provider(id) });
        on!(on_provider_delete, |m, id| if let Some(id) = parse_id(&id) { m.delete_provider(id) });
        on!(on_provider_model_committed, |m, id, model| if let Some(id) = parse_id(&id) {
            m.commit_provider_model(id, &model)
        });
        on!(on_provider_refresh_models, |m, id| if let Some(id) = parse_id(&id) { m.test_provider(id) });
        on!(on_provider_status_closed, |m| m.close_provider_status());
        on!(on_provider_add, |m| m.add_provider());
        // form
        on!(on_form_preset_selected, |m, i| m.form_select_preset(i));
        on!(on_form_name_edited, |m, t| m.form_set_name(&t));
        on!(on_form_url_edited, |m, t| m.form_set_url(&t));
        on!(on_form_key_edited, |m, t| m.form_set_key(&t));
        on!(on_form_remove_key, |m| m.form_remove_stored_key());
        on!(on_form_model_edited, |m, t| m.form_set_model(&t));
        on!(on_form_async_toggled, |m, v| m.form_set_async(v));
        on!(on_form_test, |m| m.form_test());
        on!(on_form_save, |m| m.form_save());
        on!(on_form_cancel, |m| m.form_cancel());
        // appearance
        on!(on_theme_selected, |m, i| m.select_theme(i));
        on!(on_ui_font_selected, |m, i| m.select_ui_font(i));
        on!(on_overlay_font_selected, |m, i| m.select_overlay_font(i));
        on!(on_overlay_size_changed, |m, v| m.set_overlay_size(v as f64));
        on!(on_overlay_color_picked, |m, c| m.set_overlay_color(c.red(), c.green(), c.blue()));
        on!(on_overlay_color_hex_entered, |m, t| m.set_overlay_color_hex(&t));
        on!(on_overlay_background_selected, |m, i| m.select_overlay_background(i));
        on!(on_overlay_opacity_changed, |m, v| m.set_overlay_opacity(v as f64));
        on!(on_overlay_reset, |m| m.reset_overlay());
        // models
        on!(on_model_delete, |m, id| m.request_model_delete(&id));
        on!(on_models_notice_closed, |m| m.close_models_notice());
        on!(on_confirm_accepted, |m| m.confirm_accept());
        on!(on_confirm_dismissed, |m| m.confirm_dismiss());
    }
}

fn parse_id(s: &str) -> Option<Uuid> {
    if s.is_empty() { None } else { Uuid::parse_str(s).ok() }
}

/// `appTheme`: System leaves the OS value (`unknown`), Light/Dark force the scheme.
pub fn apply_theme_to(window: &ui::SettingsWindow, theme: ThemeChoice) {
    // System is resolved here (not left `Unknown`): the std widgets and the area's own tokens must agree.
    let dark = match theme {
        ThemeChoice::System => system_uses_dark(),
        ThemeChoice::Light => false,
        ThemeChoice::Dark => true,
    };
    let scheme = if dark { slint::language::ColorScheme::Dark } else { slint::language::ColorScheme::Light };
    window.global::<ui::Palette>().set_color_scheme(scheme);
}

/// The OS app theme (`appTheme = system`): UISettings on Windows, the app's effective appearance on macOS.
pub fn system_uses_dark() -> bool {
    #[cfg(windows)]
    return sa_platform::win::accent::system_uses_dark_theme().unwrap_or(false);
    #[cfg(target_os = "macos")]
    return sa_platform::mac::appkit::is_dark_appearance();
    #[cfg(not(any(windows, target_os = "macos")))]
    return false;
}

/// Starts a system move of the window (title bar drag) where the backend supports it.
fn drag_window(window: &ui::SettingsWindow) {
    use slint::winit_030::WinitWindowAccessor;
    let _ = window.window().with_winit_window(|w| {
        let _ = w.drag_window();
    });
}

fn s(v: &str) -> SharedString {
    SharedString::from(v)
}

fn strings(v: &[String]) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(v.iter().map(|x| s(x)).collect::<Vec<_>>()))
}

fn same_strings(model: &ModelRc<SharedString>, v: &[String]) -> bool {
    model.row_count() == v.len() && model.iter().zip(v).all(|(a, b)| a.as_str() == b)
}

fn model_row(r: &ModelRowView) -> ui::SettingsModelRow {
    ui::SettingsModelRow {
        id: s(&r.id),
        name: s(&r.name),
        is_active: r.is_active,
        language_summary: s(&r.language_summary),
        storage_status: s(&r.storage_status),
        rating_text: s(&r.rating_text),
        license_text: s(&r.license_text),
        is_non_commercial: r.is_non_commercial,
        error_text: s(&r.error_text),
        is_deleting: r.is_deleting,
        shows_delete: r.shows_delete,
        can_delete: r.can_delete,
        delete_help: s(&r.delete_help),
    }
}

fn provider_row(r: &ProviderRowView) -> ui::SettingsProviderRow {
    ui::SettingsProviderRow {
        id: s(&r.id),
        name: s(&r.name),
        preset_name: s(&r.preset_name),
        shows_preset_tag: r.shows_preset_tag,
        summary: s(&r.summary),
        is_active: r.is_active,
        is_testing: r.is_testing,
        model: s(&r.model),
        models: strings(&r.models),
        notice_text: s(&r.notice_text),
        notice_error: r.notice_error,
    }
}

/// Replaces a struct model only when its rows differ (keyed update: unchanged rows keep focus).
fn set_rows<T: Clone + PartialEq + 'static>(current: ModelRc<T>, rows: Vec<T>, set: impl FnOnce(ModelRc<T>)) {
    if let Some(vec) = current.as_any().downcast_ref::<VecModel<T>>() {
        if vec.row_count() == rows.len() {
            for (i, r) in rows.into_iter().enumerate() {
                if vec.row_data(i).as_ref() != Some(&r) {
                    vec.set_row_data(i, r);
                }
            }
            return;
        }
        vec.set_vec(rows);
        return;
    }
    set(ModelRc::new(VecModel::from(rows)));
}

/// Pushes `view` into the bridge (unchanged values are not re-set).
pub fn push(window: &ui::SettingsWindow, view: &View, flavor: Flavor) {
    let b = window.global::<ui::SettingsBridge>();
    macro_rules! set {
        ($get:ident, $set:ident, $v:expr) => {{
            let v = $v;
            if b.$get() != v {
                b.$set(v);
            }
        }};
    }
    macro_rules! set_list {
        ($get:ident, $set:ident, $v:expr) => {{
            if !same_strings(&b.$get(), $v) {
                b.$set(strings($v));
            }
        }};
    }
    set!(get_draws_title_bar, set_draws_title_bar, cfg!(sa_ui = "mac") && !cfg!(target_os = "macos"));
    set!(get_page, set_page, view.page);
    set!(get_scroll_epoch, set_scroll_epoch, view.scroll_epoch);
    // General
    set_list!(get_ui_language_items, set_ui_language_items, &view.ui_language_items);
    set!(get_ui_language_index, set_ui_language_index, view.ui_language_index);
    set!(get_needs_relaunch, set_needs_relaunch, view.needs_relaunch);
    set_list!(get_speech_language_items, set_speech_language_items, &view.speech_language_items);
    set!(get_speech_language_index, set_speech_language_index, view.speech_language_index);
    set_list!(get_model_items, set_model_items, &view.model_items);
    set!(get_model_index, set_model_index, view.model_index);
    set!(get_model_enabled, set_model_enabled, view.model_enabled);
    set_list!(get_translation_model_items, set_translation_model_items, &view.translation_model_items);
    set!(get_translation_model_index, set_translation_model_index, view.translation_model_index);
    set!(get_translation_model_separator, set_translation_model_separator, view.translation_model_separator);
    set!(get_translation_model_enabled, set_translation_model_enabled, view.translation_model_enabled);
    set!(get_diarize, set_diarize, view.diarize);
    set_list!(get_speaker_count_items, set_speaker_count_items, &view.speaker_count_items);
    set!(get_speaker_count_index, set_speaker_count_index, view.speaker_count_index);
    set!(get_auto_play, set_auto_play, view.auto_play);
    set!(get_offline, set_offline, view.offline);
    set!(get_offline_footer, set_offline_footer, s(&view.offline_footer));
    // LAN
    set!(get_lan_enabled, set_lan_enabled, view.lan_enabled);
    set!(get_lan_url, set_lan_url, s(&view.lan_url));
    set!(get_lan_busy, set_lan_busy, view.lan_busy);
    set!(get_lan_test_open, set_lan_test_open, view.lan_test_open);
    set!(get_lan_test_success, set_lan_test_success, view.lan_test_success);
    set!(get_lan_test_text, set_lan_test_text, s(&view.lan_test_text));
    set_list!(get_lan_models, set_lan_models, &view.lan_models);
    set!(get_lan_model, set_lan_model, s(&view.lan_model));
    set!(get_lan_async, set_lan_async, view.lan_async);
    // cache + associations
    set!(get_cache_usage_text, set_cache_usage_text, s(&view.cache_usage_text));
    set!(get_cache_gb, set_cache_gb, view.cache_gb as i32);
    set!(get_clear_cache_enabled, set_clear_cache_enabled, view.clear_cache_enabled);
    set!(get_associations_registered, set_associations_registered, view.associations_registered);
    set!(get_associations_bar_open, set_associations_bar_open, view.associations_bar_open);
    set!(get_associations_bar_severity, set_associations_bar_severity, view.associations_bar_severity);
    set!(get_associations_bar_text, set_associations_bar_text, s(&view.associations_bar_text));
    // providers
    set!(get_local_provider_active, set_local_provider_active, view.local_provider_active);
    set_rows(b.get_provider_rows(), view.provider_rows.iter().map(provider_row).collect(), |m| b.set_provider_rows(m));
    set!(get_cloud_callout, set_cloud_callout, s(&view.cloud_callout));
    set!(get_provider_status_open, set_provider_status_open, view.provider_status_open);
    set!(get_provider_status_success, set_provider_status_success, view.provider_status_success);
    set!(get_provider_status_text, set_provider_status_text, s(&view.provider_status_text));
    // form
    let f = &view.form;
    set!(get_form_open, set_form_open, f.open);
    set!(get_form_is_new, set_form_is_new, f.is_new);
    set_list!(get_form_preset_items, set_form_preset_items, &f.preset_items);
    set!(get_form_preset_index, set_form_preset_index, f.preset_index);
    set!(get_form_name, set_form_name, s(&f.name));
    set!(get_form_url, set_form_url, s(&f.url));
    set!(get_form_url_placeholder, set_form_url_placeholder, s(&f.url_placeholder));
    set!(get_form_is_lan, set_form_is_lan, f.is_lan);
    set!(get_form_key, set_form_key, s(&f.key));
    set!(get_form_key_placeholder, set_form_key_placeholder, s(&f.key_placeholder));
    set!(get_form_key_note, set_form_key_note, s(&f.key_note));
    set!(get_form_can_remove_key, set_form_can_remove_key, f.can_remove_key);
    set!(get_form_model, set_form_model, s(&f.model));
    set_list!(get_form_model_items, set_form_model_items, &f.model_items);
    set!(get_form_async, set_form_async, f.async_translation);
    set!(get_form_testing, set_form_testing, f.testing);
    set!(get_form_can_test, set_form_can_test, f.can_test);
    set!(get_form_can_save, set_form_can_save, f.can_save);
    set!(get_form_notice_text, set_form_notice_text, s(&f.notice_text));
    set!(get_form_notice_error, set_form_notice_error, f.notice_error);
    set!(get_form_key_not_stored, set_form_key_not_stored, f.key_not_stored);
    // appearance
    set!(get_theme_index, set_theme_index, view.theme_index);
    let fonts: Vec<ui::SettingsChoice> =
        view.font_items.iter().map(|(t, sep)| ui::SettingsChoice { title: s(t), separator_before: *sep && flavor == Flavor::Mac }).collect();
    {
        let cur = b.get_font_items();
        if cur.row_count() != fonts.len() || cur.iter().zip(&fonts).any(|(a, b)| &a != b) {
            b.set_font_items(ModelRc::new(VecModel::from(fonts)));
        }
    }
    let titles: Vec<String> = view.font_items.iter().map(|(t, _)| t.clone()).collect();
    set_list!(get_font_titles, set_font_titles, &titles);
    set!(get_ui_font_index, set_ui_font_index, view.ui_font_index);
    set!(get_overlay_font_index, set_overlay_font_index, view.overlay_font_index);
    set!(get_overlay_font_family, set_overlay_font_family, s(&view.overlay_font_family));
    set!(get_overlay_size, set_overlay_size, view.overlay_size as f32);
    set!(get_overlay_size_text, set_overlay_size_text, s(&view.overlay_size_text));
    let (r, g, bl) = view.overlay_color;
    set!(get_overlay_color, set_overlay_color, slint::Color::from_rgb_u8(r, g, bl));
    set!(get_overlay_color_hex, set_overlay_color_hex, s(&view.overlay_color_hex));
    set!(get_overlay_background_index, set_overlay_background_index, view.overlay_background_index);
    set!(get_overlay_opacity, set_overlay_opacity, view.overlay_opacity as f32);
    set!(get_overlay_opacity_text, set_overlay_opacity_text, s(&view.overlay_opacity_text));
    // models
    set_rows(b.get_transcription_rows(), view.transcription_rows.iter().map(model_row).collect(), |m| b.set_transcription_rows(m));
    set_rows(b.get_diarization_rows(), view.diarization_rows.iter().map(model_row).collect(), |m| b.set_diarization_rows(m));
    set_rows(b.get_translation_rows(), view.translation_rows.iter().map(model_row).collect(), |m| b.set_translation_rows(m));
    set!(get_models_loading, set_models_loading, view.models_loading);
    set!(get_models_load_error, set_models_load_error, s(&view.models_load_error));
    set!(get_models_total_text, set_models_total_text, s(&view.models_total_text));
    set!(get_models_notice_open, set_models_notice_open, view.models_notice_open);
    set!(get_models_notice_text, set_models_notice_text, s(&view.models_notice_text));
    // confirmation
    let c = view.confirm.as_ref();
    set!(get_confirm_open, set_confirm_open, c.is_some());
    set!(get_confirm_title, set_confirm_title, s(c.map(|c| c.title.as_str()).unwrap_or("")));
    set!(get_confirm_message, set_confirm_message, s(c.map(|c| c.message.as_str()).unwrap_or("")));
    set!(get_confirm_primary, set_confirm_primary, s(c.map(|c| c.primary.as_str()).unwrap_or("")));
}
