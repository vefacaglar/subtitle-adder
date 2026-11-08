//! Hub wiring tests: a real `MainWindow` (software renderer, no display) with a `HubPresenter` over a real
//! `VideoPlatformModel` and the view-model fakes, checked through `HubBridge`. Covers the UI-level behaviors of
//! win-ui-hub-settings §3–§5 and mac-ui §7.5 / §9 that live in `sa-app`: rule 6 (sign-in card on every tab, no rows,
//! lists or pictures while logged out and none left after logout, late picture decodes dropped), the add-to-list
//! menu, paging, list creation, the search box reset and the authentication outcomes. Both flavors are exercised
//! on the compiled tree (the bridge is identical).

#[path = "../../sa-viewmodels/tests/common/mod.rs"]
mod common;

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter};
use slint::{ComponentHandle, Model};

use sa_app::hub::{HubFlavor, HubPresenter, HubServices};
use sa_app::ui::{HubBridge, MainWindow};
use sa_core::dispatch::UiDispatcher;
use sa_core::l10n::{CatalogChain, UiLanguage};
use sa_core::platform::PlatformShell;
use sa_profiles::{Ticks, UserVideoList, VideoUserStats};
use sa_viewmodels::hub::VideoPlatformModel;

use common::{Env, LanguageScope};

struct TestPlatform(Rc<MinimalSoftwareWindow>);

impl Platform for TestPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

fn install_platform() {
    thread_local!(static DONE: Cell<bool> = const { Cell::new(false) });
    DONE.with(|d| {
        if !d.get() {
            let _ = slint::platform::set_platform(Box::new(TestPlatform(MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer))));
            d.set(true);
        }
    });
}

struct Rig {
    env: Env,
    _lang: LanguageScope,
    vm: Rc<VideoPlatformModel>,
    window: MainWindow,
    hub: HubPresenter,
    played: Rc<std::cell::RefCell<Vec<PathBuf>>>,
}

impl Rig {
    fn bridge(&self) -> HubBridge<'_> {
        self.window.global::<HubBridge>()
    }
    fn eventually(&self, what: &str, mut cond: impl FnMut(&Rig) -> bool) {
        self.env.eventually(what, || cond(self));
    }
    fn settle(&self) {
        let vm = self.vm.clone();
        self.env.eventually("hub settled", move || !vm.is_refreshing() && !vm.is_authenticating());
        self.env.pump();
    }
    fn login(&self, user: &str, pw: &str) {
        self.bridge().invoke_login(user.into(), pw.into(), false);
        self.eventually("logged in", |r| r.bridge().get_is_logged_in());
        self.settle();
    }
}

fn rig(flavor: HubFlavor, user: Option<(&str, &str)>) -> Rig {
    install_platform();
    let mac = flavor == HubFlavor::Mac;
    let lang = LanguageScope::new(if mac { CatalogChain::Mac } else { CatalogChain::Windows }, UiLanguage::Tr);
    let mut env = if mac { Env::mac() } else { Env::win() };
    if let Some((u, p)) = user {
        env.profiles = common::FakeProfiles::with_user(u, p, false);
    }
    let window = MainWindow::new().expect("main window");
    sa_app::loc::install(&window);
    sa_app::symbols::install(&window);
    let vm = Rc::new(VideoPlatformModel::new(env.services()));
    let ui: Arc<dyn UiDispatcher> = env.ui.clone();
    let shell: Arc<dyn PlatformShell> = env.shell.clone();
    let hub = HubPresenter::attach(&window, vm.clone(), HubServices { ui, shell: Some(shell), flavor });
    let played = Rc::new(std::cell::RefCell::new(Vec::new()));
    {
        let played = played.clone();
        hub.on_play(move |p| played.borrow_mut().push(p));
    }
    vm.start();
    hub.set_active(true);
    let r = Rig { env, _lang: lang, vm, window, hub, played };
    r.settle();
    r
}

fn stats(path: &Path, views: i64, liked: bool, rating: i64) -> VideoUserStats {
    VideoUserStats {
        content_fingerprint: None,
        video_path: path.to_string_lossy().into_owned(),
        file_name: path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        directory_path: path.parent().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        view_count: views,
        is_liked: liked,
        rating,
        last_position_seconds: 30.0,
        duration_seconds: 120.0,
        last_watched_at: (views > 0).then(Ticks::now),
        updated_at: Ticks::now(),
        thumbnail: None,
    }
}

/// Adds a video with engagement to `user`'s profile (and the file on disk).
fn add_video(r: &Rig, user: &str, rel: &str, views: i64, liked: bool, rating: i64) -> PathBuf {
    let p = r.env.touch(rel, "video");
    r.env.profiles.st.lock().stats.entry(user.into()).or_default().insert(p.to_string_lossy().to_lowercase(), stats(&p, views, liked, rating));
    p
}

fn png(w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbaImage::from_pixel(w, h, image::Rgba([200, 40, 40, 255]));
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).expect("png");
    out.into_inner()
}

// ------------------------------------------------------------------------------------------------ rule 6

#[test]
fn logged_out_every_tab_shows_the_sign_in_card_and_nothing_is_searched() {
    for flavor in [HubFlavor::Windows, HubFlavor::Mac] {
        let r = rig(flavor, Some(("alice", "pw")));
        // A discovered (indexed) file exists on disk; logged out it must never reach the hub.
        r.env.touch("Videos/old.mp4", "x");
        for tab in 0..5 {
            r.bridge().invoke_filter_selected(tab);
            r.settle();
            let b = r.bridge();
            assert!(b.get_shows_auth_card(), "{flavor:?} tab {tab}: sign-in card");
            assert!(!b.get_is_logged_in());
            assert_eq!(b.get_cards().row_count(), 0, "{flavor:?} tab {tab}: no cards");
            assert_eq!(b.get_lists().row_count(), 0);
            assert!(!b.get_search_enabled(), "{flavor:?}: search disabled logged out");
            assert!(!b.get_engagement_visible());
            assert!(!b.get_can_clear_history());
        }
        r.bridge().invoke_search_submitted("old".into());
        r.settle();
        assert_eq!(r.bridge().get_search_query().as_str(), "", "a logged-out search is not applied");
        assert_eq!(r.env.index.search_calls.load(std::sync::atomic::Ordering::SeqCst), 0, "no index search logged out");
        assert_eq!(r.env.index.all_calls.load(std::sync::atomic::Ordering::SeqCst), 0, "no index listing logged out");
        // Counts are plain and the profile button asks to sign in.
        assert_eq!(r.bridge().get_profile_title().as_str(), "Profil / Giriş");
        assert_eq!(r.bridge().get_summary_text().as_str(), "İzleme geçmişi için giriş yapın");
        assert_eq!(r.bridge().get_watched_count(), 0);
    }
}

#[test]
fn logout_drops_every_row_list_and_picture_at_once() {
    let r = rig(HubFlavor::Windows, Some(("alice", "pw")));
    let a = add_video(&r, "alice", "Videos/a.mp4", 3, true, 4);
    add_video(&r, "alice", "Videos/b.mp4", 1, false, 0);
    r.env.profiles.st.lock().thumbs.entry("alice".into()).or_default().insert(a.to_string_lossy().to_lowercase(), png(64, 36));
    r.env.profiles.st.lock().lists.insert(
        "alice".into(),
        vec![UserVideoList { id: "l1".into(), name: "Daha Sonra İzle".into(), video_paths: vec![a.to_string_lossy().into_owned()], created_at: Ticks(0), updated_at: Ticks(0) }],
    );
    r.login("alice", "pw");
    r.eventually("two cards", |r| r.bridge().get_cards().row_count() == 2);
    assert!(!r.bridge().get_shows_auth_card());
    assert_eq!(r.bridge().get_lists().row_count(), 1);
    assert_eq!(r.bridge().get_lists().row_data(0).unwrap().title.as_str(), "Daha Sonra İzle (1)");
    r.eventually("picture decoded", |r| r.hub.cached_picture_count() == 1);
    let first = r.bridge().get_cards().row_data(0).unwrap();
    assert!(first.has_thumbnail, "the decoded picture reaches its card");
    assert_eq!(first.views_text.as_str(), "3 görüntüleme");
    assert_eq!(first.rating_badge.as_str(), "★ 4/5");
    assert_eq!(first.list_names.row_count(), 1);
    assert_eq!(r.bridge().get_current_username().as_str(), "alice");
    assert_eq!(r.bridge().get_watched_count(), 2);

    let reset = r.bridge().get_auth_reset_epoch();
    r.bridge().invoke_logout();
    // Same UI turn, before the store answered: nothing of alice is left.
    let b = r.bridge();
    assert!(!b.get_is_logged_in());
    assert!(b.get_shows_auth_card());
    assert_eq!(b.get_cards().row_count(), 0);
    assert_eq!(b.get_lists().row_count(), 0);
    assert_eq!(b.get_current_username().as_str(), "");
    assert_eq!(b.get_watched_count(), 0);
    assert!(!b.get_engagement_visible());
    assert_eq!(r.hub.cached_picture_count(), 0, "decoded pictures dropped");
    assert_ne!(b.get_auth_reset_epoch(), reset, "the password fields are cleared");
    r.settle();
    assert_eq!(r.bridge().get_cards().row_count(), 0, "still nothing after the store answered");
    assert!(!r.bridge().get_remember_me(), "remember-me is opt-in again after logout");
}

#[test]
fn a_picture_decoded_after_logout_is_dropped() {
    let r = rig(HubFlavor::Mac, Some(("alice", "pw")));
    let a = add_video(&r, "alice", "Movies/a.mov", 2, false, 0);
    r.env.profiles.st.lock().thumbs.entry("alice".into()).or_default().insert(a.to_string_lossy().to_lowercase(), png(32, 18));
    r.bridge().invoke_login("alice".into(), "pw".into(), false);
    // Pump only until the card shows (its decode is queued), then log out before the decode result is applied.
    r.eventually("card", |r| r.bridge().get_cards().row_count() == 1);
    r.bridge().invoke_logout();
    r.settle();
    std::thread::sleep(std::time::Duration::from_millis(50));
    r.env.pump();
    assert_eq!(r.hub.cached_picture_count(), 0);
    assert_eq!(r.bridge().get_cards().row_count(), 0);
}

#[test]
fn switching_profiles_never_shows_the_previous_profiles_cards() {
    let r = rig(HubFlavor::Windows, Some(("alice", "pw")));
    r.env.profiles.st.lock().users.push(("bob".into(), "pw2".into()));
    add_video(&r, "alice", "Videos/alice-only.mp4", 5, true, 5);
    r.login("alice", "pw");
    r.eventually("alice's card", |r| r.bridge().get_cards().row_count() == 1);
    r.bridge().invoke_logout();
    r.bridge().invoke_login("bob".into(), "pw2".into(), false);
    // While bob's feed loads, alice's card must not come back.
    assert_eq!(r.bridge().get_cards().row_count(), 0);
    r.eventually("bob logged in", |r| r.bridge().get_current_username().as_str() == "bob");
    r.settle();
    assert_eq!(r.bridge().get_cards().row_count(), 0, "bob has no videos");
    assert_eq!(r.bridge().get_liked_count(), 0);
}

// ------------------------------------------------------------------------------------------------ auth

#[test]
fn login_failure_shows_the_localized_error_and_success_clears_the_password() {
    let r = rig(HubFlavor::Windows, Some(("alice", "pw")));
    r.bridge().invoke_login("alice".into(), "wrong".into(), false);
    r.eventually("error", |r| !r.bridge().get_auth_error().is_empty());
    assert_eq!(r.bridge().get_auth_error().as_str(), "Kullanıcı adı veya şifre hatalı.");
    assert!(r.bridge().get_shows_auth_card());
    let reset = r.bridge().get_auth_reset_epoch();
    r.login("alice", "pw");
    assert_eq!(r.bridge().get_auth_error().as_str(), "");
    assert_ne!(r.bridge().get_auth_reset_epoch(), reset);
    assert_eq!(r.bridge().get_profile_title().as_str(), "alice");
    assert_eq!(r.bridge().get_summary_text().as_str(), "0 izlenen · 0 beğenilen");
}

#[test]
fn registered_names_are_cut_per_surface() {
    let r = rig(HubFlavor::Windows, None);
    {
        let mut st = r.env.profiles.st.lock();
        for n in ["a1", "a2", "a3", "a4", "a5", "a6"] {
            st.users.push((n.into(), "x".into()));
        }
    }
    r.vm.refresh();
    r.settle();
    assert_eq!(r.bridge().get_card_usernames().row_count(), 5, "Windows auth card: 5 chips");
    assert_eq!(r.bridge().get_flyout_usernames().row_count(), 4, "flyout: 4 chips");
    // One rig at a time: each holds the process-wide language lock.
    drop(r);
    let m = rig(HubFlavor::Mac, None);
    {
        let mut st = m.env.profiles.st.lock();
        for n in ["a1", "a2", "a3", "a4", "a5"] {
            st.users.push((n.into(), "x".into()));
        }
    }
    m.vm.refresh();
    m.settle();
    assert_eq!(m.bridge().get_card_usernames().row_count(), 4, "macOS: 4 pills");
}

// ------------------------------------------------------------------------------------------------ menus, lists, feed

#[test]
fn add_to_list_menu_lists_lists_and_quick_creates() {
    let r = rig(HubFlavor::Windows, Some(("alice", "pw")));
    // Logged out: one disabled item.
    r.hub.request_menu("add:0");
    let rows = r.hub.menu_rows();
    assert_eq!(rows.len(), 1);
    assert!(!rows[0].enabled);
    assert_eq!(rows[0].title.as_str(), "Listeye eklemek için giriş yapın");

    let a = add_video(&r, "alice", "Videos/a.mp4", 1, false, 0);
    r.login("alice", "pw");
    r.eventually("card", |r| r.bridge().get_cards().row_count() == 1);
    r.hub.request_menu("add:0");
    let rows = r.hub.menu_rows();
    assert_eq!(rows.len(), 1, "no lists: only the quick-create item");
    assert_eq!(rows[0].title.as_str(), "+ \"Daha Sonra İzle\" listesi oluştur ve ekle");
    assert_eq!(rows[0].glyph.as_str(), "\u{E710}");
    r.hub.activate_menu(rows[0].id.as_str());
    r.eventually("list created with the video", |r| {
        r.vm.lists().iter().any(|l| l.name == "Daha Sonra İzle" && l.video_paths.iter().any(|p| p.eq_ignore_ascii_case(&a.to_string_lossy())))
    });
    r.settle();
    r.hub.request_menu("add:0");
    let rows = r.hub.menu_rows();
    assert_eq!(rows.len(), 3, "list, separator, quick create");
    assert!(rows[0].checkable && rows[0].checked, "the video is in the list");
    assert_eq!(rows[0].title.as_str(), "Daha Sonra İzle (1)");
    assert!(rows[1].separator);
    assert_eq!(rows[2].title.as_str(), "+ Yeni liste oluştur ve ekle");
    // Toggling removes it again.
    r.hub.activate_menu(rows[0].id.as_str());
    r.eventually("removed from the list", |r| r.vm.lists().iter().all(|l| l.video_paths.is_empty()));
}

#[test]
fn windows_list_create_clears_the_box_and_selects_the_new_list() {
    let r = rig(HubFlavor::Windows, Some(("alice", "pw")));
    r.login("alice", "pw");
    let reset = r.bridge().get_new_list_reset_epoch();
    r.bridge().invoke_list_create("   ".into());
    r.settle();
    assert_eq!(r.vm.lists().len(), 0, "a blank name does nothing");
    r.bridge().invoke_list_create("Belgeseller".into());
    r.eventually("list pill", |r| r.bridge().get_lists().row_count() == 1);
    r.settle();
    assert_ne!(r.bridge().get_new_list_reset_epoch(), reset, "the box is cleared");
    assert_eq!(r.bridge().get_filter(), 5, "the new list is on show");
    assert!(r.bridge().get_lists().row_data(0).unwrap().selected);
    assert_eq!(r.bridge().get_empty_title().as_str(), "\"Belgeseller\" listesinde henüz video yok");
    // Windows: deleting the selected list has no confirmation and falls back to Keşfet.
    let id = r.bridge().get_lists().row_data(0).unwrap().id;
    r.bridge().invoke_list_delete(id);
    r.eventually("list gone", |r| r.bridge().get_lists().row_count() == 0);
    assert_eq!(r.bridge().get_filter(), 0);
}

#[test]
fn mac_blank_list_name_is_reported_under_the_row() {
    let r = rig(HubFlavor::Mac, Some(("alice", "pw")));
    r.login("alice", "pw");
    r.bridge().invoke_list_create("".into());
    r.settle();
    assert_eq!(r.bridge().get_list_error().as_str(), "Lütfen bir liste adı girin.");
    r.bridge().invoke_clear_list_error();
    assert_eq!(r.bridge().get_list_error().as_str(), "");
}

#[test]
fn windows_feed_pages_by_forty_cards() {
    let r = rig(HubFlavor::Windows, Some(("alice", "pw")));
    for i in 0..45 {
        add_video(&r, "alice", &format!("Videos/v{i:02}.mp4"), 1, false, 0);
    }
    r.login("alice", "pw");
    r.eventually("first page", |r| r.bridge().get_cards().row_count() == 40);
    r.bridge().invoke_load_more();
    r.eventually("second page", |r| r.bridge().get_cards().row_count() == 45);
    // Changing the filter starts over at 40 and scrolls back to the top.
    let epoch = r.bridge().get_feed_epoch();
    r.bridge().invoke_filter_selected(1);
    r.settle();
    assert_eq!(r.bridge().get_cards().row_count(), 40);
    assert_ne!(r.bridge().get_feed_epoch(), epoch);
}

#[test]
fn card_actions_like_rate_and_play() {
    let r = rig(HubFlavor::Windows, Some(("alice", "pw")));
    let a = add_video(&r, "alice", "Videos/a.mp4", 2, false, 0);
    r.login("alice", "pw");
    r.eventually("card", |r| r.bridge().get_cards().row_count() == 1);
    r.bridge().invoke_card_like(0);
    // Optimistic: the card flips at once.
    assert!(r.bridge().get_cards().row_data(0).unwrap().is_liked);
    assert_eq!(r.bridge().get_cards().row_data(0).unwrap().like_text.as_str(), "Favorilerde");
    r.bridge().invoke_card_rate(0, 3);
    assert_eq!(r.bridge().get_cards().row_data(0).unwrap().rating, 3);
    r.settle();
    let s = r.env.profiles.stats_of("alice", &a).expect("stats");
    assert!(s.is_liked);
    assert_eq!(s.rating, 3);
    r.bridge().invoke_card_play(0);
    assert_eq!(r.played.borrow().as_slice(), &[a.clone()]);
    // History tab with watched videos offers "Geçmişi temizle" (Windows: by the watched count).
    r.bridge().invoke_filter_selected(1);
    r.settle();
    assert!(r.bridge().get_can_clear_history());
    r.bridge().invoke_clear_history();
    r.settle();
    assert_eq!(r.bridge().get_watched_count(), 0);
    assert_eq!(r.bridge().get_liked_count(), 1, "favorites are kept");
}

#[test]
fn search_box_is_cleared_only_when_the_applied_query_becomes_empty() {
    let r = rig(HubFlavor::Windows, Some(("alice", "pw")));
    add_video(&r, "alice", "Videos/Sezon_2_Bolum_05.mp4", 1, false, 0);
    add_video(&r, "alice", "Videos/other.mp4", 1, false, 0);
    r.login("alice", "pw");
    let epoch = r.bridge().get_search_reset_epoch();
    r.bridge().invoke_search_submitted("sezon".into());
    r.settle();
    assert_eq!(r.bridge().get_search_query().as_str(), "sezon");
    assert_eq!(r.bridge().get_cards().row_count(), 1);
    // An unrelated refresh keeps whatever is typed (FIX-W).
    r.vm.refresh();
    r.settle();
    assert_eq!(r.bridge().get_search_reset_epoch(), epoch);
    // Typing (non-empty) does not search; emptying the box restores the feed.
    r.bridge().invoke_search_edited("sez".into());
    assert_eq!(r.bridge().get_search_query().as_str(), "sezon");
    r.bridge().invoke_search_edited("".into());
    r.settle();
    assert_eq!(r.bridge().get_search_query().as_str(), "");
    assert_eq!(r.bridge().get_cards().row_count(), 2);
    assert_ne!(r.bridge().get_search_reset_epoch(), epoch);
}

#[test]
fn engagement_bar_follows_the_player_video_and_its_menu_targets_it() {
    let r = rig(HubFlavor::Mac, Some(("alice", "pw")));
    let a = add_video(&r, "alice", "Movies/a.mov", 4, true, 2);
    r.vm.player_video_changed(Some(&a), false);
    r.settle();
    assert!(!r.bridge().get_engagement_visible(), "logged out: no bar");
    r.hub.request_menu("add:player");
    assert!(!r.hub.menu_rows()[0].enabled, "logged out: the menu asks to sign in");
    r.login("alice", "pw");
    r.eventually("bar", |r| r.bridge().get_engagement_visible());
    assert_eq!(r.bridge().get_engagement_views(), 4);
    assert!(r.bridge().get_engagement_liked());
    assert_eq!(r.bridge().get_engagement_rating(), 2);
    r.bridge().invoke_engagement_like();
    assert!(!r.bridge().get_engagement_liked(), "what the bar shows is what flips");
    r.bridge().invoke_engagement_rate(5);
    assert_eq!(r.bridge().get_engagement_rating(), 5);
    r.hub.request_menu("add:player");
    let rows = r.hub.menu_rows();
    assert!(rows[0].enabled);
    r.hub.activate_menu(rows[0].id.as_str());
    r.eventually("player video in the new list", |r| {
        r.vm.lists().iter().any(|l| l.video_paths.iter().any(|p| p.eq_ignore_ascii_case(&a.to_string_lossy())))
    });
    r.settle();
    r.bridge().invoke_logout();
    assert!(!r.bridge().get_engagement_visible());
    assert_eq!(r.bridge().get_engagement_views(), 0);
}

#[test]
fn mac_card_context_menu_plays_and_reveals() {
    let r = rig(HubFlavor::Mac, Some(("alice", "pw")));
    let a = add_video(&r, "alice", "Movies/a.mov", 1, false, 0);
    r.login("alice", "pw");
    r.eventually("card", |r| r.bridge().get_cards().row_count() == 1);
    r.hub.request_menu("card:0");
    let rows = r.hub.menu_rows();
    assert_eq!(rows.iter().map(|e| e.title.to_string()).collect::<Vec<_>>(), vec!["Oynat".to_owned(), "Finder'da göster".to_owned()]);
    r.hub.activate_menu(rows[1].id.as_str());
    assert_eq!(r.env.shell.revealed.lock().as_slice(), &[a.clone()]);
    r.hub.request_menu("card:0");
    r.hub.activate_menu("card-play");
    assert_eq!(r.played.borrow().as_slice(), &[a]);
    // The list context menu opens the rename popover with the list's name.
    r.bridge().invoke_list_create("Belgeseller".into());
    r.eventually("list", |r| r.bridge().get_lists().row_count() == 1);
    let id = r.bridge().get_lists().row_data(0).unwrap().id;
    r.hub.request_menu(&format!("list:{id}"));
    let open = r.bridge().get_rename_open_epoch();
    r.hub.activate_menu("list-rename");
    assert_ne!(r.bridge().get_rename_open_epoch(), open);
    assert_eq!(r.bridge().get_rename_initial().as_str(), "Belgeseller");
    let done = r.bridge().get_rename_done_epoch();
    r.bridge().invoke_list_rename(id.clone(), "Belgesel".into());
    r.eventually("renamed", |r| r.bridge().get_lists().row_data(0).is_some_and(|l| l.name.as_str() == "Belgesel"));
    r.settle();
    assert_ne!(r.bridge().get_rename_done_epoch(), done, "the popover closes after a rename");
}

#[test]
fn texts_follow_the_ui_language() {
    let r = rig(HubFlavor::Windows, Some(("alice", "pw")));
    add_video(&r, "alice", "Videos/a.mp4", 2, false, 0);
    r.login("alice", "pw");
    r.eventually("card", |r| r.bridge().get_cards().row_count() == 1);
    r._lang.set(UiLanguage::En);
    r.eventually("english summary", |r| r.bridge().get_summary_text().contains("watched"));
    r.settle();
    assert_eq!(r.bridge().get_cards().row_data(0).unwrap().views_text.as_str(), "2 views");
    assert_eq!(r.bridge().get_profile_title().as_str(), "alice");
}

// ------------------------------------------------------------------------------------------------ added UI tests

#[test]
fn logout_while_the_hub_is_hidden_still_clears_the_bridge() {
    // win-ui-hub-settings §5.6: the profile-change clearing runs even while the Platform tab is hidden, so returning
    // to it never shows the previous profile for a frame.
    let r = rig(HubFlavor::Windows, Some(("alice", "pw")));
    let a = add_video(&r, "alice", "Videos/a.mp4", 1, true, 0);
    r.env.profiles.st.lock().thumbs.entry("alice".into()).or_default().insert(a.to_string_lossy().to_lowercase(), png(32, 18));
    r.login("alice", "pw");
    r.eventually("card", |r| r.bridge().get_cards().row_count() == 1);
    r.eventually("picture", |r| r.hub.cached_picture_count() == 1);
    r.hub.set_active(false);
    r.bridge().invoke_logout();
    assert_eq!(r.bridge().get_cards().row_count(), 0);
    assert_eq!(r.bridge().get_lists().row_count(), 0);
    assert_eq!(r.hub.cached_picture_count(), 0);
    r.settle();
    r.hub.set_active(true);
    r.settle();
    assert!(r.bridge().get_shows_auth_card());
    assert_eq!(r.bridge().get_cards().row_count(), 0);
}

#[test]
fn an_unchanged_card_sequence_is_updated_in_place() {
    // win-ui-hub-settings §5.4 rebuild rule 1: same paths in the same order → rows are patched, the list is kept.
    let r = rig(HubFlavor::Windows, Some(("alice", "pw")));
    add_video(&r, "alice", "Videos/a.mp4", 2, false, 0);
    add_video(&r, "alice", "Videos/b.mp4", 1, false, 0);
    r.login("alice", "pw");
    r.eventually("two cards", |r| r.bridge().get_cards().row_count() == 2);
    let model = r.bridge().get_cards();
    let rows: Vec<_> = model.iter().collect();
    let mut liked = rows.clone();
    liked[1].is_liked = true;
    let vm = slint::VecModel::from(rows.clone());
    assert!(!sa_app::hub::cards::apply_rows(&vm, liked.clone()), "same sequence: patched in place");
    assert!(vm.row_data(1).unwrap().is_liked);
    let mut swapped = liked.clone();
    swapped.swap(0, 1);
    assert!(sa_app::hub::cards::apply_rows(&vm, swapped), "another order: the list is replaced");
    // Paths compare case-insensitively (Windows).
    let mut upper = vm.iter().collect::<Vec<_>>();
    upper[0].path = upper[0].path.to_uppercase().into();
    assert!(!sa_app::hub::cards::apply_rows(&vm, upper));
    // Through the presenter: a like keeps the bridge's model and its two rows.
    r.bridge().invoke_card_like(0);
    r.settle();
    assert_eq!(r.bridge().get_cards().row_count(), 2);
}

fn sample_card(badge: Option<&str>, views: i64, liked: bool, rating: i64, lists: &[&str]) -> sa_viewmodels::hub::VideoCard {
    let path = PathBuf::from(r"D:\Diziler\Sezon 2\Bolum_05.mp4");
    sa_viewmodels::hub::VideoCard {
        title: "Bolum_05".into(),
        file_name: "Bolum_05.mp4".into(),
        extension_label: "MP4".into(),
        folder: sa_viewmodels::hub::folder_line(r"D:\Diziler\Sezon 2"),
        view_count: views,
        is_liked: liked,
        rating,
        progress_ratio: 0.5,
        duration_seconds: 95.0,
        duration_text: "1:35".into(),
        last_watched: None,
        last_watched_text: None,
        list_ids: lists.iter().map(|l| l.to_lowercase()).collect(),
        list_names: lists.iter().map(|l| (*l).to_owned()).collect(),
        sidecar_badge: badge.map(str::to_owned),
        has_thumbnail: false,
        path,
    }
}

#[test]
fn card_rows_carry_each_faces_texts_in_both_languages() {
    use sa_app::hub::cards::card_row;
    let lang = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let c = sample_card(Some("SRT"), 3, true, 4, &["Belgeseller"]);
    let w = card_row(&c, HubFlavor::Windows, None);
    assert_eq!(w.views_text.as_str(), "3 görüntüleme");
    assert_eq!(w.watch_line.as_str(), "3 görüntüleme · Henüz izlenmedi");
    assert_eq!(w.subtitle_status.as_str(), "Altyazılı (SRT)");
    assert_eq!(w.like_text.as_str(), "Favorilerde");
    assert_eq!(w.rating_badge.as_str(), "★ 4/5");
    assert_eq!(w.list_names.row_count(), 1);
    assert!(w.has_progress && !w.has_thumbnail);
    let none = card_row(&sample_card(None, 0, false, 0, &[]), HubFlavor::Windows, None);
    assert_eq!(none.views_text.as_str(), "Henüz izlenmedi");
    assert_eq!(none.subtitle_status.as_str(), "Altyazı yok");
    assert_eq!(none.rating_badge.as_str(), "");
    assert_eq!(none.like_text.as_str(), "Favorilere ekle");
    lang.set(UiLanguage::En);
    let en = card_row(&c, HubFlavor::Windows, None);
    assert_eq!(en.views_text.as_str(), "3 views");
    assert_eq!(en.like_text.as_str(), "In Favorites");
    drop(lang);

    // macOS: the chip uses the one format key `Altyazılı (%@)` in both languages (theSubtitleChipUsesOneFormatKey…).
    let lang = LanguageScope::new(CatalogChain::Mac, UiLanguage::Tr);
    let m = card_row(&c, HubFlavor::Mac, None);
    assert_eq!(m.subtitle_chip.as_str(), "Altyazılı (SRT)");
    assert_eq!(m.watch_badges.as_str(), "3 görüntüleme");
    assert_eq!(m.view_count.as_str(), "3");
    assert_eq!(m.accessible_label.as_str(), "Bolum_05.mp4");
    let unwatched = card_row(&sample_card(None, 0, false, 0, &[]), HubFlavor::Mac, None);
    assert_eq!(unwatched.watch_badges.as_str(), "Henüz izlenmedi");
    assert_eq!(unwatched.subtitle_chip.as_str(), "Altyazı yok");
    lang.set(UiLanguage::En);
    let m = card_row(&c, HubFlavor::Mac, None);
    assert!(m.subtitle_chip.ends_with("(SRT)") && !m.subtitle_chip.starts_with("Altyazılı"), "{}", m.subtitle_chip);
    assert!(cards_audio_check());
}

fn cards_audio_check() -> bool {
    sa_app::hub::cards::is_audio(Path::new("a.MP3")) && !sa_app::hub::cards::is_audio(Path::new("a.mp4"))
}

#[test]
fn list_pills_and_filters_map_both_ways() {
    use sa_app::hub::cards::{empty_symbol, filter_from_index, filter_index, list_rows};
    use sa_profiles::VideoFeedFilter;
    for i in 0..5 {
        assert_eq!(filter_index(&filter_from_index(i).unwrap()), i);
    }
    assert!(filter_from_index(5).is_none(), "a custom list is selected by id, not by index");
    let lists = vec![
        UserVideoList { id: "A1".into(), name: "Daha Sonra İzle".into(), video_paths: vec!["x".into(), "y".into()], created_at: Ticks(0), updated_at: Ticks(0) },
        UserVideoList { id: "b2".into(), name: "Belgeseller".into(), video_paths: vec![], created_at: Ticks(0), updated_at: Ticks(0) },
    ];
    let rows = list_rows(&lists, &VideoFeedFilter::CustomList(Some("a1".into())));
    assert_eq!(rows[0].title.as_str(), "Daha Sonra İzle (2)");
    assert!(rows[0].selected, "ids compare case-insensitively");
    assert!(!rows[1].selected);
    assert_eq!(filter_index(&VideoFeedFilter::CustomList(Some("a1".into()))), 5);
    assert!(list_rows(&lists, &VideoFeedFilter::All).iter().all(|r| !r.selected));
    assert_eq!(empty_symbol(&VideoFeedFilter::History, false), "clock");
    assert_eq!(empty_symbol(&VideoFeedFilter::Liked, true), "magnifyingglass");
}

// ------------------------------------------------------------------------------------------------ parity checks

#[test]
fn submitting_the_same_search_again_runs_it_again() {
    // C# `ExecuteSearch` always calls `RefreshFeed()`, Swift `submitSearch()` always schedules a refresh: pressing
    // Enter / "Ara" again finds videos that appeared since the last run of the same query.
    for flavor in [HubFlavor::Windows, HubFlavor::Mac] {
        let r = rig(flavor, Some(("alice", "pw")));
        add_video(&r, "alice", "Videos/Sezon_1.mp4", 1, false, 0);
        r.login("alice", "pw");
        r.bridge().invoke_search_submitted("sezon".into());
        r.settle();
        assert_eq!(r.bridge().get_cards().row_count(), 1, "{flavor:?}");
        add_video(&r, "alice", "Videos/Sezon_2.mp4", 1, false, 0);
        r.bridge().invoke_search_submitted("sezon ".into());
        r.eventually("the repeated search sees the new video", |r| r.bridge().get_cards().row_count() == 2);
        drop(r);
    }
}

#[test]
fn logout_clears_a_typed_search_and_the_list_failures() {
    // Swift `forgetProfileState` empties the search field; nothing typed or reported for the closing profile stays on
    // screen (rule 6), even when no query was applied.
    let r = rig(HubFlavor::Mac, Some(("alice", "pw")));
    r.login("alice", "pw");
    r.bridge().invoke_list_create("".into());
    r.settle();
    assert_ne!(r.bridge().get_list_error().as_str(), "");
    let epoch = r.bridge().get_search_reset_epoch();
    r.bridge().invoke_logout();
    assert_ne!(r.bridge().get_search_reset_epoch(), epoch, "the search box is emptied at logout");
    assert_eq!(r.bridge().get_list_error().as_str(), "");
    assert_eq!(r.bridge().get_rename_error().as_str(), "");
    // Logging in does not clear a box (logged-out → logged-in needs no clearing).
    r.settle();
    let epoch = r.bridge().get_search_reset_epoch();
    r.login("alice", "pw");
    assert_eq!(r.bridge().get_search_reset_epoch(), epoch);
}
