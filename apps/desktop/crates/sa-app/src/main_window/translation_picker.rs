//! The toolbar translation-model picker of one main window (Windows flyout, win-ui-hub-settings §8 / C#
//! `TranslationModelPicker`; macOS popover, mac-ui §6.6 / SwiftUI `TranslationModelPickerButton`).
//!
//! [`TranslationModelPickerModel`] (sa-viewmodels) decides everything; this presenter maps it onto the
//! `MainBridge.picker-*` properties and turns the flyout's/popover's callbacks into model calls:
//!
//! * open: the toolbar button (or the macOS overflow item) → `MainWindowHooks::translation_model_clicked` →
//!   [`TranslationPickerPresenter::open`] → `picker.open()`, push, bump `picker-open-epoch` (the tree shows it);
//! * a chosen model (click; macOS Return) and "API sağlayıcılarını yönet…" close it (`picker-close-epoch`), the latter
//!   opening Settings on the providers page (`general` W, `translation` M);
//! * a finished "Yenile" and, on macOS, store changes (`providersChanged`) re-push the state.
//!
//! A light dismiss (click outside) is not reported by Slint; the next `open()` abandons a refresh still under way, so
//! nothing of it is shown afterwards.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use uuid::Uuid;

use sa_core::dispatch::UiDispatcher;
use sa_core::models::ModelCatalog;
use sa_viewmodels::providers_ui::{PickerHost, ProvidersChange, ProvidersModel, TranslationModelPickerModel};
use sa_viewmodels::uiref::UiKey;
use sa_viewmodels::{AppServices, SubtitleViewModel};

use super::rows::fill_if_changed;
use crate::ui::{MainBridge, MainWindow, PickerItem, PickerModelRow};

/// The editor view model as the picker's host (`translationModel`).
pub struct VmPickerHost(pub Rc<SubtitleViewModel>);

impl PickerHost for VmPickerHost {
    fn translation_model(&self) -> String {
        self.0.translation_model()
    }
    fn set_translation_model(&self, id: &str) {
        self.0.set_translation_model(id);
    }
}

/// What the presenter is built from (the app shell composes it per window).
pub struct PickerDeps {
    pub services: AppServices,
    pub catalog: Rc<ModelCatalog>,
    pub vm: Rc<SubtitleViewModel>,
    /// Saved-provider tests ("Yenile"): the platform's tester behind it (Windows C# messages, macOS notices).
    pub providers: Rc<ProvidersModel>,
    /// "API sağlayıcılarını yönet…": opens Settings on the providers page.
    pub manage_providers: Rc<dyn Fn()>,
}

pub struct TranslationPickerPresenter {
    window: slint::Weak<MainWindow>,
    mac: bool,
    picker: Rc<TranslationModelPickerModel>,
    _providers: Rc<ProvidersModel>,
    manage: Rc<dyn Fn()>,
    ui: Arc<dyn UiDispatcher>,
    provider_items: Rc<VecModel<PickerItem>>,
    rows: Rc<VecModel<PickerModelRow>>,
    provider_ids: RefCell<Vec<Option<Uuid>>>,
    key: Cell<Option<UiKey<TranslationPickerPresenter>>>,
}

impl Drop for TranslationPickerPresenter {
    fn drop(&mut self) {
        if let Some(k) = self.key.get() {
            k.unregister();
        }
    }
}

fn bump(v: i32) -> i32 {
    v.wrapping_add(1)
}

impl TranslationPickerPresenter {
    pub fn attach(window: &MainWindow, deps: PickerDeps) -> Rc<Self> {
        let mac = deps.services.policy.catalog_chain == sa_core::l10n::CatalogChain::Mac;
        let ui = deps.services.ui.clone();
        let picker = Rc::new(TranslationModelPickerModel::new(
            deps.services,
            deps.catalog,
            deps.providers.clone(),
            Rc::new(VmPickerHost(deps.vm)),
        ));
        let this = Rc::new(Self {
            window: window.as_weak(),
            mac,
            picker,
            _providers: deps.providers.clone(),
            manage: deps.manage_providers,
            ui,
            provider_items: Rc::new(VecModel::default()),
            rows: Rc::new(VecModel::default()),
            provider_ids: RefCell::new(Vec::new()),
            key: Cell::new(None),
        });
        this.key.set(Some(UiKey::register(&this)));
        let main = window.global::<MainBridge>();
        main.set_picker_providers(ModelRc::from(this.provider_items.clone()));
        main.set_picker_rows(ModelRc::from(this.rows.clone()));
        this.wire(window);
        {
            let weak = Rc::downgrade(&this);
            this.picker.set_on_change(Box::new(move || {
                if let Some(p) = weak.upgrade() {
                    p.sync();
                }
            }));
        }
        {
            // macOS `.onChange(of: providerStore.providers.map(\.id))` → `providersChanged()`; test flags feed
            // `canRefresh`. The Windows flyout lists what `LoadModels` read and does not follow the store.
            let weak = Rc::downgrade(&this);
            deps.providers.on_change(Box::new(move |change| {
                if let Some(p) = weak.upgrade()
                    && p.mac
                {
                    if change == ProvidersChange::Providers {
                        p.picker.providers_changed();
                    }
                    p.sync();
                }
            }));
        }
        {
            let key = this.key.get().expect("registered");
            let ui = this.ui.clone();
            sa_core::l10n::LanguageState::shared()
                .subscribe(move |_| {
                    key.post(&ui, |p| p.sync());
                })
                .detach();
        }
        this
    }

    /// The model (tests, the app shell).
    pub fn picker(&self) -> &Rc<TranslationModelPickerModel> {
        &self.picker
    }

    fn wire(self: &Rc<Self>, window: &MainWindow) {
        let main = window.global::<MainBridge>();
        macro_rules! on {
            ($setter:ident, |$p:ident $(, $arg:ident)*| $body:expr) => {{
                let weak = Rc::downgrade(self);
                main.$setter(move |$($arg),*| {
                    if let Some($p) = weak.upgrade() {
                        $body
                    }
                });
            }};
        }
        on!(on_picker_provider_selected, |p, index| p.provider_selected(index));
        on!(on_picker_search_edited, |p, text| {
            p.picker.set_search_text(text.as_str());
            p.sync();
        });
        on!(on_picker_row_clicked, |p, id| {
            if p.picker.select(id.as_str()) {
                p.dismiss();
            }
        });
        on!(on_picker_move_highlight, |p, step| {
            p.picker.move_highlight(step as i64);
            p.sync();
            p.scroll_to_highlight();
        });
        on!(on_picker_choose_highlighted, |p| {
            if p.picker.choose_highlighted() {
                p.dismiss();
            }
        });
        on!(on_picker_refresh, |p| {
            p.picker.refresh();
            p.sync();
        });
        on!(on_picker_manage_providers, |p| {
            p.dismiss();
            (p.manage)();
        });
        on!(on_picker_closed, |p| p.picker.close());
    }

    /// The flyout/popover opens (C# `Flyout.Opening` → `Open`; SwiftUI `model.open()` + `isPresented = true`).
    pub fn open(&self) {
        self.picker.open();
        self.sync();
        if let Some(w) = self.window.upgrade() {
            let main = w.global::<MainBridge>();
            main.set_picker_search_reset(bump(main.get_picker_search_reset()));
            main.set_picker_open_epoch(bump(main.get_picker_open_epoch()));
        }
        self.scroll_to_highlight();
    }

    /// Closes it from Rust (`ModelChosen`, `SettingsRequested`) and abandons a refresh under way.
    fn dismiss(&self) {
        self.picker.close();
        if let Some(w) = self.window.upgrade() {
            let main = w.global::<MainBridge>();
            main.set_picker_close_epoch(bump(main.get_picker_close_epoch()));
        }
    }

    fn provider_selected(&self, index: i32) {
        let id = match usize::try_from(index).ok().and_then(|i| self.provider_ids.borrow().get(i).copied()) {
            Some(id) => id,
            None => return,
        };
        let before = self.picker.browsed_provider();
        if !self.picker.browse(id) {
            // A provider removed meanwhile: nothing changes (C#), macOS re-reads the list.
            self.sync();
            return;
        }
        if before != id
            && let Some(w) = self.window.upgrade()
        {
            let main = w.global::<MainBridge>();
            main.set_picker_search_reset(bump(main.get_picker_search_reset()));
        }
        self.sync();
        self.scroll_to_highlight();
    }

    /// Pushes everything the flyout/popover shows (diffed models).
    pub fn sync(&self) {
        let Some(w) = self.window.upgrade() else { return };
        let main = w.global::<MainBridge>();
        let choices = self.picker.provider_choices();
        let items: Vec<PickerItem> =
            choices.iter().map(|c| PickerItem { title: SharedString::from(c.name.as_str()), enabled: true }).collect();
        fill_if_changed(&self.provider_items, items, false);
        *self.provider_ids.borrow_mut() = choices.iter().map(|c| c.id).collect();
        main.set_picker_provider_index(self.picker.provider_index());

        let highlighted = if self.mac { self.picker.highlighted_id() } else { None };
        let rows: Vec<PickerModelRow> = self
            .picker
            .picker_rows()
            .into_iter()
            .map(|r| PickerModelRow {
                highlighted: highlighted.as_deref() == Some(r.id.as_str()),
                id: r.id.into(),
                title: r.title.into(),
                current: r.is_current,
                enabled: r.is_enabled,
            })
            .collect();
        fill_if_changed(&self.rows, rows, false);
        main.set_picker_empty_text(self.picker.empty_text().unwrap_or_default().into());
        main.set_picker_status(self.picker.status_text().unwrap_or_default().into());
        main.set_picker_show_refresh(!self.mac || self.picker.has_browsed_profile());
        main.set_picker_can_refresh(self.picker.can_refresh());
    }

    /// macOS `proxy.scrollTo(highlightedID)`.
    fn scroll_to_highlight(&self) {
        if !self.mac {
            return;
        }
        let Some(w) = self.window.upgrade() else { return };
        let main = w.global::<MainBridge>();
        let Some(h) = self.picker.highlighted_id() else { return };
        let Some(index) = self.picker.picker_rows().iter().position(|r| r.id == h) else { return };
        main.set_picker_scroll_row(index as i32);
        main.set_picker_scroll_epoch(bump(main.get_picker_scroll_epoch()));
    }
}
