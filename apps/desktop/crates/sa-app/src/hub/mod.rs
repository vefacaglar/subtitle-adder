//! The hub area (ARCHITECTURE.md §6.5, owner ui-hub): the Platform workspace, the profile button/flyout (Windows) or
//! popover (macOS), the player's engagement bar and the add-to-list menus, all driven by
//! `sa_viewmodels::hub::VideoPlatformModel` through `HubBridge`. (The file explorer sidebar is wired by the main
//! window controller, which owns `ExplorerBridge`.)
//!
//! Rule 6 (UNIFIED.md §4): the bridge never holds a profile's cards, lists, counts or pictures while logged out; on
//! every session change (logout, switch, delete) the card/list models and the decoded pictures are dropped in the
//! same UI turn, before the store answers, and decodes that finish for an older session are discarded.

pub mod cards;
pub mod demo;
pub mod menus;
pub mod thumbs;

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::sync::Arc;

use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use sa_core::dispatch::UiDispatcher;
use sa_core::l;
use sa_core::l10n::LanguageState;
use sa_core::platform::{Alert, AlertButton, AlertButtonRole, PlatformShell};
use sa_core::secrets::SecretString;
use sa_core::subscription::Subscription;
use sa_profiles::VideoFeedFilter;
use sa_viewmodels::hub::{HubChange, VideoPlatformModel};
use sa_viewmodels::uiref::UiKey;

use crate::ui::{HubBridge, HubCardData, HubListData, MainBridge, MainWindow, MenuEntry, Workspace};

pub use cards::HubFlavor;
use thumbs::{CardImageCache, DecodeWorker};

/// What the presenter needs besides the view model.
#[derive(Clone)]
pub struct HubServices {
    pub ui: Arc<dyn UiDispatcher>,
    /// Confirmations and failure alerts (macOS NSAlert sheets), "Finder'da göster". `None` in tests.
    pub shell: Option<Arc<dyn PlatformShell>>,
    pub flavor: HubFlavor,
}

/// The target of the menu that is open.
#[derive(Debug, Clone, PartialEq)]
enum MenuTarget {
    None,
    AddToList(Option<PathBuf>),
    Card(PathBuf),
    List(String),
}

#[derive(Default)]
struct Pending {
    /// Lists before a create (a new one appearing clears the box).
    create: Option<usize>,
    /// (list id, requested name) of a rename under way.
    rename: Option<(String, String)>,
    delete_profile: bool,
}

struct State {
    /// The session the bridge shows (`None` logged out): a different one clears everything first.
    session: Option<String>,
    /// (filter, applied query) of the feed on screen: a change scrolls back to the top.
    feed_key: Option<(VideoFeedFilter, String)>,
    applied_query: String,
    was_authenticating: bool,
    was_logged_in: bool,
    pending: Pending,
    menu: MenuTarget,
    /// Path of every rendered card row (actions resolve the index here).
    paths: Vec<PathBuf>,
    active: bool,
    alert_open: bool,
}

struct Inner {
    window: slint::Weak<MainWindow>,
    vm: Rc<VideoPlatformModel>,
    svc: HubServices,
    cards: Rc<VecModel<HubCardData>>,
    lists: Rc<VecModel<HubListData>>,
    menu: Rc<VecModel<MenuEntry>>,
    card_names: Rc<VecModel<SharedString>>,
    flyout_names: Rc<VecModel<SharedString>>,
    st: RefCell<State>,
    images: RefCell<CardImageCache>,
    worker: RefCell<Option<DecodeWorker>>,
    key: Cell<Option<UiKey<Inner>>>,
    play: RefCell<Option<Rc<dyn Fn(PathBuf)>>>,
    open_platform: RefCell<Option<Rc<dyn Fn()>>>,
    subs: RefCell<Vec<Subscription>>,
}

/// Binds one main window's `HubBridge` to the hub view model.
pub struct HubPresenter {
    inner: Rc<Inner>,
}

impl Drop for HubPresenter {
    fn drop(&mut self) {
        if let Some(k) = self.inner.key.get() {
            k.unregister();
        }
    }
}

fn set_if<T: PartialEq>(get: impl Fn() -> T, set: impl Fn(T), v: T) {
    if get() != v {
        set(v);
    }
}

fn shared_list(model: &VecModel<SharedString>, values: Vec<String>) {
    let same = model.row_count() == values.len() && values.iter().enumerate().all(|(i, v)| model.row_data(i).is_some_and(|o| o == v.as_str()));
    if !same {
        model.set_vec(values.into_iter().map(SharedString::from).collect::<Vec<_>>());
    }
}

impl HubPresenter {
    /// Wires the window's hub surfaces to `vm` and pushes the current state.
    pub fn attach(window: &MainWindow, vm: Rc<VideoPlatformModel>, svc: HubServices) -> Self {
        let inner = Rc::new(Inner {
            window: window.as_weak(),
            vm,
            svc,
            cards: Rc::new(VecModel::default()),
            lists: Rc::new(VecModel::default()),
            menu: Rc::new(VecModel::default()),
            card_names: Rc::new(VecModel::default()),
            flyout_names: Rc::new(VecModel::default()),
            st: RefCell::new(State {
                session: None,
                feed_key: None,
                applied_query: String::new(),
                was_authenticating: false,
                was_logged_in: false,
                pending: Pending::default(),
                menu: MenuTarget::None,
                paths: Vec::new(),
                active: false,
                alert_open: false,
            }),
            images: RefCell::new(CardImageCache::default()),
            worker: RefCell::new(None),
            key: Cell::new(None),
            play: RefCell::new(None),
            open_platform: RefCell::new(None),
            subs: RefCell::new(Vec::new()),
        });
        let key = UiKey::register(&inner);
        inner.key.set(Some(key));
        {
            let ui = inner.svc.ui.clone();
            *inner.worker.borrow_mut() = Some(DecodeWorker::spawn(move |generation, path_key, decoded| {
                key.post(&ui, move |inner| inner.picture_decoded(generation, path_key, decoded));
            }));
        }
        let hub = window.global::<HubBridge>();
        hub.set_cards(ModelRc::from(inner.cards.clone()));
        hub.set_lists(ModelRc::from(inner.lists.clone()));
        hub.set_menu_entries(ModelRc::from(inner.menu.clone()));
        hub.set_card_usernames(ModelRc::from(inner.card_names.clone()));
        hub.set_flyout_usernames(ModelRc::from(inner.flyout_names.clone()));
        Inner::bind_callbacks(&inner, &hub);
        {
            let weak = Rc::downgrade(&inner);
            inner.vm.on_change(Box::new(move |change| {
                if let Some(inner) = weak.upgrade() {
                    inner.changed(change);
                }
            }));
        }
        {
            let ui = inner.svc.ui.clone();
            let sub = LanguageState::shared().subscribe(move |_| {
                key.post(&ui, |inner| {
                    // Texts built by the view model (last-watched dates) are rebuilt by a refresh.
                    inner.vm.refresh();
                    inner.sync();
                });
            });
            inner.subs.borrow_mut().push(sub);
        }
        inner.sync();
        HubPresenter { inner }
    }

    /// A card's "Oynat" / thumbnail / title (Windows `VideoSelected`, macOS `play(url)`).
    pub fn on_play(&self, f: impl Fn(PathBuf) + 'static) {
        *self.inner.play.borrow_mut() = Some(Rc::new(f));
    }

    /// "Video Platformu" in the profile menu. Default: selects the Platform workspace through `MainBridge`.
    pub fn on_open_platform(&self, f: impl Fn() + 'static) {
        *self.inner.open_platform.borrow_mut() = Some(Rc::new(f));
    }

    /// The Platform tab became visible / hidden (refreshes the feed when it appears).
    pub fn set_active(&self, active: bool) {
        self.inner.st.borrow_mut().active = active;
        self.inner.vm.set_hub_active(active);
        self.inner.sync();
    }

    /// Re-reads the view model into the bridge.
    pub fn sync(&self) {
        self.inner.sync();
    }

    pub fn view_model(&self) -> &Rc<VideoPlatformModel> {
        &self.inner.vm
    }

    /// Decoded pictures held right now (tests: rule 6).
    pub fn cached_picture_count(&self) -> usize {
        self.inner.images.borrow().len()
    }

    /// The rows the bridge shows (tests).
    pub fn card_rows(&self) -> Vec<HubCardData> {
        self.inner.cards.iter().collect()
    }

    pub fn list_rows(&self) -> Vec<HubListData> {
        self.inner.lists.iter().collect()
    }

    pub fn menu_rows(&self) -> Vec<MenuEntry> {
        self.inner.menu.iter().collect()
    }

    /// Builds the entries of a menu as the UI would request it (tests and demo scenes).
    pub fn request_menu(&self, id: &str) {
        self.inner.menu_requested(id);
    }

    pub fn activate_menu(&self, id: &str) {
        self.inner.menu_activated(id);
    }
}

impl Inner {
    fn key(&self) -> UiKey<Inner> {
        self.key.get().expect("registered")
    }

    fn bind_callbacks(this: &Rc<Self>, hub: &HubBridge) {
        macro_rules! cb {
            ($method:ident, |$inner:ident $(, $arg:ident : $t:ty)*| $body:expr) => {{
                let weak: Weak<Inner> = Rc::downgrade(this);
                hub.$method(move |$($arg: $t),*| {
                    if let Some($inner) = weak.upgrade() {
                        $body
                    }
                });
            }};
        }
        cb!(on_sign_in_requested, |inner| inner.sync());
        cb!(on_profile_opened, |inner| {
            if inner.svc.flavor == HubFlavor::Mac {
                inner.vm.clear_auth_error();
            }
            inner.sync();
        });
        cb!(on_profile_closed, |inner| {
            if inner.svc.flavor == HubFlavor::Mac {
                inner.vm.clear_auth_error();
            }
        });
        cb!(on_login, |inner, user: SharedString, pw: SharedString, remember: bool| {
            inner.vm.login(&user, SecretString::new(pw.as_str()), remember);
            inner.sync();
        });
        cb!(on_register, |inner, user: SharedString, pw: SharedString, remember: bool| {
            inner.vm.register(&user, SecretString::new(pw.as_str()), remember);
            inner.sync();
        });
        cb!(on_logout, |inner| {
            inner.vm.logout();
            inner.bump_auth_reset();
            inner.sync();
        });
        cb!(on_delete_profile, |inner, pw: SharedString| {
            let Some(user) = inner.vm.current_username() else { return };
            inner.st.borrow_mut().pending.delete_profile = true;
            inner.vm.delete_profile(&user, SecretString::new(pw.as_str()));
            inner.sync();
        });
        cb!(on_clear_auth_error, |inner| inner.vm.clear_auth_error());
        cb!(on_remember_toggled, |inner, v: bool| inner.vm.set_remember_me(v));
        cb!(on_open_platform, |inner| inner.open_platform());
        cb!(on_search_submitted, |inner, q: SharedString| {
            inner.vm.search(&q);
            inner.sync();
        });
        cb!(on_search_edited, |inner, q: SharedString| {
            // Emptying the box restores the full feed at once; typing waits for Enter / "Ara".
            if q.trim().is_empty() && !inner.vm.search_query().is_empty() {
                inner.vm.search("");
                inner.sync();
            }
        });
        cb!(on_clear_search, |inner| {
            inner.vm.search("");
            inner.sync();
        });
        cb!(on_filter_selected, |inner, i: i32| {
            if let Some(f) = cards::filter_from_index(i) {
                inner.vm.set_filter(f);
            }
            inner.sync();
        });
        cb!(on_list_selected, |inner, id: SharedString| {
            inner.vm.set_filter(VideoFeedFilter::CustomList(Some(id.to_string())));
            inner.sync();
        });
        cb!(on_list_delete, |inner, id: SharedString| inner.delete_list(id.to_string()));
        cb!(on_list_create, |inner, name: SharedString| inner.create_list(name.to_string()));
        cb!(on_list_rename, |inner, id: SharedString, name: SharedString| {
            inner.st.borrow_mut().pending.rename = Some((id.to_string(), name.trim().to_owned()));
            inner.set_rename_error("");
            inner.vm.rename_list(&id, &name);
        });
        cb!(on_clear_list_error, |inner| {
            inner.set_list_error("");
            inner.set_rename_error("");
        });
        cb!(on_clear_history, |inner| inner.clear_history());
        cb!(on_load_more, |inner| inner.vm.load_more());
        cb!(on_card_play, |inner, i: i32| {
            if let Some(p) = inner.card_path(i) {
                inner.play(p);
            }
        });
        cb!(on_card_like, |inner, i: i32| {
            if let Some(p) = inner.card_path(i) {
                inner.vm.toggle_like_for(&p);
            }
        });
        cb!(on_card_rate, |inner, i: i32, n: i32| {
            if let Some(p) = inner.card_path(i) {
                inner.vm.set_rating_for(&p, n.clamp(0, 5) as u8);
            }
        });
        cb!(on_engagement_like, |inner| {
            let liked = inner.vm.engagement().is_liked;
            inner.vm.set_liked(!liked);
        });
        cb!(on_engagement_rate, |inner, n: i32| inner.vm.set_rating(n.clamp(0, 5) as u8));
        cb!(on_menu_requested, |inner, id: SharedString| inner.menu_requested(&id));
        cb!(on_menu_activated, |inner, id: SharedString| inner.menu_activated(&id));
    }

    fn card_path(&self, index: i32) -> Option<PathBuf> {
        usize::try_from(index).ok().and_then(|i| self.st.borrow().paths.get(i).cloned())
    }

    fn play(&self, path: PathBuf) {
        let f = self.play.borrow().clone();
        if let Some(f) = f {
            f(path);
        }
    }

    fn open_platform(&self) {
        let f = self.open_platform.borrow().clone();
        match f {
            Some(f) => f(),
            None => {
                if let Some(w) = self.window.upgrade() {
                    w.global::<MainBridge>().invoke_workspace_selected(Workspace::Platform);
                }
            }
        }
    }

    fn bump_auth_reset(&self) {
        if let Some(w) = self.window.upgrade() {
            let hub = w.global::<HubBridge>();
            hub.set_auth_reset_epoch(hub.get_auth_reset_epoch().wrapping_add(1));
        }
    }

    fn set_list_error(&self, text: &str) {
        if let Some(w) = self.window.upgrade() {
            w.global::<HubBridge>().set_list_error(text.into());
        }
    }

    fn set_rename_error(&self, text: &str) {
        if let Some(w) = self.window.upgrade() {
            w.global::<HubBridge>().set_rename_error(text.into());
        }
    }

    fn changed(self: &Rc<Self>, change: HubChange) {
        match change {
            HubChange::Thumbnail => self.request_pictures(),
            HubChange::Error => {
                self.route_action_error();
                self.sync();
            }
            _ => self.sync(),
        }
    }

    /// An action failure: shown in the list form that caused it (macOS), else as the `İşlem tamamlanamadı` alert
    /// (macOS); Windows shows none (its actions fail silently, like the C# app).
    fn route_action_error(self: &Rc<Self>) {
        let Some(err) = self.vm.action_error() else { return };
        let (create, rename) = {
            let st = self.st.borrow();
            (st.pending.create.is_some(), st.pending.rename.clone())
        };
        if rename.is_some() {
            self.st.borrow_mut().pending.rename = None;
            self.set_rename_error(&err.message());
            self.vm.clear_action_error();
            return;
        }
        if create {
            self.st.borrow_mut().pending.create = None;
            if self.svc.flavor == HubFlavor::Mac {
                self.set_list_error(&err.message());
            }
            self.vm.clear_action_error();
            return;
        }
        if self.svc.flavor == HubFlavor::Windows {
            self.vm.clear_action_error();
            return;
        }
        if std::mem::replace(&mut self.st.borrow_mut().alert_open, true) {
            return;
        }
        let alert = Alert {
            title: l!("İşlem tamamlanamadı"),
            message: err.message(),
            buttons: vec![AlertButton { title: l!("Tamam"), role: AlertButtonRole::Default }],
        };
        let weak = Rc::downgrade(self);
        self.confirm(alert, move |_| {
            if let Some(inner) = weak.upgrade() {
                inner.st.borrow_mut().alert_open = false;
                inner.vm.clear_action_error();
            }
        });
    }

    /// Shows an alert through the shell (macOS sheet); without a shell the answer is "dismissed".
    fn confirm(&self, alert: Alert, done: impl FnOnce(Option<usize>) + 'static) {
        let Some(shell) = self.svc.shell.clone() else {
            done(None);
            return;
        };
        let fut = shell.confirm(alert);
        if slint::spawn_local(async move { done(fut.await) }).is_err() {
            // No event loop (headless tests): nothing was shown, nothing is done.
        }
    }

    fn clear_history(self: &Rc<Self>) {
        match self.svc.flavor {
            HubFlavor::Windows => self.vm.clear_watch_history(),
            HubFlavor::Mac => {
                let alert = Alert {
                    title: l!("İzleme geçmişi temizlensin mi?"),
                    message: l!("Favoriler, puanlar ve listeler korunur; görüntülenme sayıları ve kaldığın yerler silinir."),
                    buttons: vec![
                        AlertButton { title: l!("Geçmişi temizle"), role: AlertButtonRole::Destructive },
                        AlertButton { title: l!("Vazgeç"), role: AlertButtonRole::Cancel },
                    ],
                };
                let weak = Rc::downgrade(self);
                self.confirm(alert, move |r| {
                    if r == Some(0)
                        && let Some(inner) = weak.upgrade()
                    {
                        inner.vm.clear_watch_history();
                    }
                });
            }
        }
    }

    fn delete_list(self: &Rc<Self>, id: String) {
        match self.svc.flavor {
            // Windows: no confirmation; the filter falls back to Keşfet.
            HubFlavor::Windows => {
                self.vm.delete_list(&id);
                self.sync();
            }
            HubFlavor::Mac => {
                let name = self.vm.lists().into_iter().find(|l| l.id.eq_ignore_ascii_case(&id)).map(|l| l.name).unwrap_or_default();
                let alert = Alert {
                    title: l!("“%@” listesi silinsin mi?", name.as_str()),
                    message: l!("Listedeki videolar silinmez; yalnızca liste kaldırılır."),
                    buttons: vec![
                        AlertButton { title: l!("Sil"), role: AlertButtonRole::Destructive },
                        AlertButton { title: l!("Vazgeç"), role: AlertButtonRole::Cancel },
                    ],
                };
                let weak = Rc::downgrade(self);
                self.confirm(alert, move |r| {
                    if r == Some(0)
                        && let Some(inner) = weak.upgrade()
                    {
                        inner.vm.delete_list(&id);
                        inner.sync();
                    }
                });
            }
        }
    }

    fn create_list(&self, name: String) {
        let trimmed = name.trim();
        if trimmed.is_empty() && self.svc.flavor == HubFlavor::Windows {
            return; // Windows: an empty name does nothing.
        }
        if self.st.borrow().pending.create.is_some() {
            return; // a double press while one is under way
        }
        self.set_list_error("");
        let before = self.vm.lists().len();
        self.st.borrow_mut().pending.create = Some(before);
        if trimmed.is_empty() {
            // macOS: the store reports `Lütfen bir liste adı girin.` under the row.
            self.st.borrow_mut().pending.create = None;
            self.set_list_error(&sa_profiles::ProfileError::EmptyListName.message());
            return;
        }
        self.vm.create_list(Some(trimmed));
    }

    fn menu_requested(&self, id: &str) {
        let (target, entries) = if let Some(rest) = id.strip_prefix("add:") {
            let path = if rest == "player" {
                self.vm.engagement().visible.then(|| self.vm.player_video()).flatten()
            } else {
                rest.parse::<i32>().ok().and_then(|i| self.card_path(i))
            };
            let lists = self.vm.lists();
            let entries = menus::add_to_list(&lists, self.vm.is_logged_in(), path.is_some(), |list_id| {
                path.as_ref().is_some_and(|p| self.vm.is_in_list(list_id, p))
            });
            (MenuTarget::AddToList(path), entries)
        } else if let Some(rest) = id.strip_prefix("card:") {
            match rest.parse::<i32>().ok().and_then(|i| self.card_path(i)) {
                Some(p) => (MenuTarget::Card(p), menus::card_context()),
                None => (MenuTarget::None, Vec::new()),
            }
        } else if let Some(rest) = id.strip_prefix("list:") {
            (MenuTarget::List(rest.to_owned()), menus::list_context())
        } else {
            (MenuTarget::None, Vec::new())
        };
        self.st.borrow_mut().menu = target;
        self.menu.set_vec(entries);
    }

    fn menu_activated(&self, id: &str) {
        let target = std::mem::replace(&mut self.st.borrow_mut().menu, MenuTarget::None);
        match target {
            MenuTarget::AddToList(Some(path)) => {
                if id == menus::ADD_NEW {
                    self.vm.create_list_and_add(&path);
                } else if let Some(list) = id.strip_prefix(menus::ADD_LIST_PREFIX) {
                    self.vm.toggle_in_list(list, &path);
                }
            }
            MenuTarget::Card(path) => {
                if id == menus::CARD_PLAY {
                    self.play(path);
                } else if id == menus::CARD_REVEAL
                    && let Some(shell) = &self.svc.shell
                {
                    shell.reveal_in_file_manager(&path);
                }
            }
            MenuTarget::List(list_id) => {
                if id == menus::LIST_RENAME {
                    self.open_rename(&list_id);
                } else if id == menus::LIST_DELETE
                    && let Some(me) = self.key().get()
                {
                    me.delete_list(list_id);
                }
            }
            MenuTarget::AddToList(None) | MenuTarget::None => {}
        }
    }

    fn open_rename(&self, list_id: &str) {
        let Some(list) = self.vm.lists().into_iter().find(|l| l.id.eq_ignore_ascii_case(list_id)) else { return };
        self.set_list_error("");
        self.set_rename_error("");
        if let Some(w) = self.window.upgrade() {
            let hub = w.global::<HubBridge>();
            hub.set_rename_list_id(list.id.as_str().into());
            hub.set_rename_initial(list.name.as_str().into());
            hub.set_rename_open_epoch(hub.get_rename_open_epoch().wrapping_add(1));
        }
    }

    // ----- pictures

    fn picture_width(&self) -> u32 {
        let scale = self.window.upgrade().map(|w| w.window().scale_factor()).unwrap_or(1.0).max(1.0);
        (192.0 * scale).round() as u32
    }

    fn request_pictures(&self) {
        if !self.vm.is_logged_in() {
            return;
        }
        let width = self.picture_width();
        let worker = self.worker.borrow();
        let Some(worker) = worker.as_ref() else { return };
        let mut images = self.images.borrow_mut();
        let paths = self.st.borrow().paths.clone();
        let mut updated = Vec::new();
        for (i, p) in paths.iter().enumerate() {
            let k = cards::path_key(p);
            if let Some(img) = images.get(&k) {
                updated.push((i, img.clone()));
                continue;
            }
            if images.is_in_flight(&k) {
                continue;
            }
            if let Some(bytes) = self.vm.thumbnail(p) {
                images.request(worker, &k, bytes, width);
            }
        }
        drop(images);
        for (i, img) in updated {
            self.set_row_picture(i, &img);
        }
    }

    fn set_row_picture(&self, index: usize, image: &slint::Image) {
        if let Some(mut row) = self.cards.row_data(index)
            && (!row.has_thumbnail || row.thumbnail != *image)
        {
            row.has_thumbnail = true;
            row.thumbnail = image.clone();
            self.cards.set_row_data(index, row);
        }
    }

    fn picture_decoded(&self, generation: u64, path_key: String, decoded: Option<thumbs::Decoded>) {
        // Rule 6: a decode for an older session (or after logout) is dropped.
        if !self.vm.is_logged_in() {
            return;
        }
        let Some(image) = self.images.borrow_mut().finish(generation, path_key.clone(), decoded) else { return };
        let paths = self.st.borrow().paths.clone();
        for (i, p) in paths.iter().enumerate() {
            if cards::path_key(p) == path_key {
                self.set_row_picture(i, &image);
            }
        }
    }

    // ----- sync

    /// Pushes the view model into the bridge (diffed).
    fn sync(&self) {
        let vm = &self.vm;
        let logged_in = vm.is_logged_in();
        let session = if logged_in { vm.current_username().map(|u| u.to_lowercase()) } else { None };

        // Rule 6: another session (logout, switch, delete) → forget every row and picture first.
        let session_changed = {
            let mut st = self.st.borrow_mut();
            let changed = st.session != session;
            if changed {
                let previous = std::mem::replace(&mut st.session, session.clone());
                st.paths.clear();
                st.feed_key = None;
                st.menu = MenuTarget::None;
                drop(st);
                if previous.is_some() {
                    self.images.borrow_mut().reset();
                }
                self.cards.set_vec(Vec::new());
                self.lists.set_vec(Vec::new());
                self.menu.set_vec(Vec::new());
            }
            changed
        };

        let Some(w) = self.window.upgrade() else { return };
        let hub = w.global::<HubBridge>();
        let flavor = self.svc.flavor;

        // Session and profile.
        set_if(|| hub.get_is_logged_in(), |v| hub.set_is_logged_in(v), logged_in);
        let user = if logged_in { vm.current_username().unwrap_or_default() } else { String::new() };
        set_if(|| hub.get_current_username().to_string(), |v| hub.set_current_username(v.into()), user);
        set_if(|| hub.get_profile_title().to_string(), |v| hub.set_profile_title(v.into()), vm.profile_title());
        set_if(|| hub.get_summary_text().to_string(), |v| hub.set_summary_text(v.into()), vm.summary_text());
        let names = vm.registered_usernames();
        shared_list(&self.card_names, names.iter().take(flavor.card_chip_count()).cloned().collect());
        shared_list(&self.flyout_names, names.iter().take(4).cloned().collect());
        set_if(|| hub.get_last_username().to_string(), |v| hub.set_last_username(v.into()), vm.last_username().unwrap_or_default());
        set_if(|| hub.get_remember_me(), |v| hub.set_remember_me(v), vm.remember_me());
        let auth_error = vm.last_error().map(|e| e.message()).unwrap_or_default();
        set_if(|| hub.get_auth_error().to_string(), |v| hub.set_auth_error(v.into()), auth_error.clone());
        let authenticating = vm.is_authenticating();
        set_if(|| hub.get_is_authenticating(), |v| hub.set_is_authenticating(v), authenticating);
        let counts = vm.counts();
        set_if(|| hub.get_watched_count(), |v| hub.set_watched_count(v), counts.watched.clamp(0, i32::MAX as i64) as i32);
        set_if(|| hub.get_liked_count(), |v| hub.set_liked_count(v), counts.liked.clamp(0, i32::MAX as i64) as i32);
        set_if(|| hub.get_rated_count(), |v| hub.set_rated_count(v), counts.rated.clamp(0, i32::MAX as i64) as i32);

        // Authentication outcomes.
        {
            let mut st = self.st.borrow_mut();
            let finished = st.was_authenticating && !authenticating;
            let became_logged_in = !st.was_logged_in && logged_in;
            st.was_authenticating = authenticating;
            st.was_logged_in = logged_in;
            let deleting = finished && std::mem::take(&mut st.pending.delete_profile);
            drop(st);
            if finished || became_logged_in {
                if deleting {
                    if logged_in {
                        hub.set_delete_failed_epoch(hub.get_delete_failed_epoch().wrapping_add(1));
                    } else {
                        hub.set_popover_close_epoch(hub.get_popover_close_epoch().wrapping_add(1));
                        hub.set_auth_reset_epoch(hub.get_auth_reset_epoch().wrapping_add(1));
                    }
                } else if logged_in && (became_logged_in || auth_error.is_empty()) {
                    hub.set_auth_reset_epoch(hub.get_auth_reset_epoch().wrapping_add(1));
                    if flavor == HubFlavor::Mac && finished {
                        hub.set_popover_close_epoch(hub.get_popover_close_epoch().wrapping_add(1));
                    }
                }
            }
        }

        // Filters and search.
        let filter = vm.filter();
        set_if(|| hub.get_filter(), |v| hub.set_filter(v), cards::filter_index(&filter));
        set_if(|| hub.get_search_enabled(), |v| hub.set_search_enabled(v), vm.is_search_enabled());
        let query = vm.search_query();
        {
            let mut st = self.st.borrow_mut();
            // FIX-W: the box is only cleared when the applied query became empty.
            if !st.applied_query.is_empty() && query.is_empty() {
                hub.set_search_reset_epoch(hub.get_search_reset_epoch().wrapping_add(1));
            }
            st.applied_query = query.clone();
        }
        set_if(|| hub.get_search_query().to_string(), |v| hub.set_search_query(v.into()), query.clone());

        // Lists.
        let lists = vm.lists();
        let list_rows = cards::list_rows(&lists, &filter);
        let same_lists = self.lists.row_count() == list_rows.len()
            && list_rows.iter().enumerate().all(|(i, r)| self.lists.row_data(i).is_some_and(|o| o == *r));
        if !same_lists {
            self.lists.set_vec(list_rows);
        }
        {
            let mut st = self.st.borrow_mut();
            if let Some(before) = st.pending.create
                && lists.len() > before
            {
                st.pending.create = None;
                hub.set_new_list_reset_epoch(hub.get_new_list_reset_epoch().wrapping_add(1));
            } else if st.pending.create.is_some() && !vm.is_refreshing() {
                // Windows: a duplicate name is silently refused.
                st.pending.create = None;
            }
            if let Some((id, name)) = st.pending.rename.clone()
                && vm.action_error().is_none()
                && lists.iter().any(|l| l.id.eq_ignore_ascii_case(&id) && l.name == name)
            {
                st.pending.rename = None;
                hub.set_rename_done_epoch(hub.get_rename_done_epoch().wrapping_add(1));
            }
        }

        // Content.
        let shows_auth = vm.shows_auth_card();
        set_if(|| hub.get_shows_auth_card(), |v| hub.set_shows_auth_card(v), shows_auth);
        let feed = if logged_in { vm.cards() } else { Vec::new() };
        let loading = logged_in && feed.is_empty() && (vm.is_refreshing() || !vm.has_loaded_feed());
        set_if(|| hub.get_shows_loading(), |v| hub.set_shows_loading(v), loading);
        let can_clear = logged_in
            && filter == VideoFeedFilter::History
            && match flavor {
                HubFlavor::Windows => counts.watched > 0,
                HubFlavor::Mac => !feed.is_empty(),
            };
        set_if(|| hub.get_can_clear_history(), |v| hub.set_can_clear_history(v), can_clear);
        let (title, hint) = vm.empty_state();
        set_if(|| hub.get_empty_title().to_string(), |v| hub.set_empty_title(v.into()), title);
        set_if(|| hub.get_empty_hint().to_string(), |v| hub.set_empty_hint(v.into()), hint);
        set_if(
            || hub.get_empty_symbol().to_string(),
            |v| hub.set_empty_symbol(v.into()),
            cards::empty_symbol(&filter, !query.trim().is_empty()).to_owned(),
        );
        let rows: Vec<HubCardData> = {
            let images = self.images.borrow();
            feed.iter().map(|c| cards::card_row(c, flavor, images.get(&cards::path_key(&c.path)))).collect()
        };
        self.st.borrow_mut().paths = feed.iter().map(|c| c.path.clone()).collect();
        cards::apply_rows(&self.cards, rows);
        {
            let key = (filter.clone(), query.clone());
            let mut st = self.st.borrow_mut();
            if st.feed_key.as_ref() != Some(&key) {
                let first = st.feed_key.is_none();
                st.feed_key = Some(key);
                if !first || session_changed {
                    hub.set_feed_epoch(hub.get_feed_epoch().wrapping_add(1));
                }
            }
        }

        // Engagement bar.
        let eng = vm.engagement();
        let visible = logged_in && eng.visible;
        // (`MainBridge.engagement-visible`, the player's layout input, is kept by the main window controller.)
        set_if(|| hub.get_engagement_visible(), |v| hub.set_engagement_visible(v), visible);
        set_if(|| hub.get_engagement_views(), |v| hub.set_engagement_views(v), if visible { eng.view_count.clamp(0, i32::MAX as i64) as i32 } else { 0 });
        set_if(|| hub.get_engagement_liked(), |v| hub.set_engagement_liked(v), visible && eng.is_liked);
        set_if(|| hub.get_engagement_rating(), |v| hub.set_engagement_rating(v), if visible { eng.rating.clamp(0, 5) as i32 } else { 0 });

        drop(hub);
        self.request_pictures();
    }
}
