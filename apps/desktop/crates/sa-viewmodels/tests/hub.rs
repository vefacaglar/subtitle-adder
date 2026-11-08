//! Ports of `LocalProfileTests` (VM level, C#), `VideoPlatformTests` / `LoggedOutHistoryTests` /
//! `PlatformReviewFixTests` (Swift, with the rule-6 rewrites of mac-viewmodels §16).

mod common;

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::Duration;

use common::*;
use sa_core::l10n::{CatalogChain, UiLanguage};
use sa_core::secrets::SecretString;
use sa_profiles::{ProfileStore, Ticks, VideoFeedFilter};
use sa_viewmodels::hub::{HubChange, NameSequence, PlaybackReport, VideoPlatformModel, date_text, duration_text, folder_line};
use sa_viewmodels::policy::HubDateFormat;

fn settle(env: &Env, hub: &VideoPlatformModel) {
    for _ in 0..3 {
        hub.settle(Duration::from_secs(5));
        env.pump();
    }
    env.eventually("hub idle", || !hub.is_refreshing());
}

/// A media folder with indexed videos.
fn index_videos(env: &Env, names: &[&str]) -> Vec<PathBuf> {
    let paths: Vec<PathBuf> = names.iter().map(|n| env.touch(&format!("media/{n}"), "v")).collect();
    use sa_media::MediaIndex;
    env.index.scan(&env.path("media"), &sa_core::cancel::CancellationToken::new());
    paths
}

fn hub(env: &Env) -> VideoPlatformModel {
    let h = VideoPlatformModel::new(env.services());
    h.start();
    h.set_hub_active(true);
    settle(env, &h);
    h
}

const ALL_FILTERS: [VideoFeedFilter; 5] =
    [VideoFeedFilter::All, VideoFeedFilter::History, VideoFeedFilter::Liked, VideoFeedFilter::MostWatched, VideoFeedFilter::TopRated];

#[test]
fn logged_out_fresh_view_model_shows_no_indexed_videos_on_any_tab() {
    for policy in [sa_viewmodels::PlatformPolicy::windows(), sa_viewmodels::PlatformPolicy::mac()] {
        let _l = LanguageScope::new(policy.catalog_chain, UiLanguage::Tr);
        let env = Env::new(policy);
        index_videos(&env, &["a.mp4", "b.mp4"]);
        let h = hub(&env);
        assert!(!h.is_logged_in());
        for f in ALL_FILTERS {
            h.set_filter(f.clone());
            settle(&env, &h);
            assert!(h.shows_auth_card(), "{f:?}: sign-in card");
            assert!(h.cards().is_empty(), "{f:?}: no cards");
        }
        assert!(!h.is_search_enabled());
        h.search("a");
        settle(&env, &h);
        assert_eq!(h.search_query(), "", "search ignored logged out");
        assert_eq!(env.index.search_calls.load(Ordering::SeqCst), 0, "index never searched");
        assert_eq!(env.index.all_calls.load(Ordering::SeqCst), 0, "index never listed");
        assert_eq!(env.profiles.st.lock().feed_calls, 0, "no feed query");
        assert!(h.lists().is_empty());
        assert_eq!(h.counts(), sa_profiles::ProfileCounts::default());
        assert!(!h.engagement().visible);
        assert_eq!(h.summary_text(), "İzleme geçmişi için giriş yapın");
    }
}

#[test]
fn login_shows_discovered_videos_and_logout_forgets_everything_synchronously() {
    let _l = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    env.profiles.st.lock().users.push(("ayse".into(), "pw".into()));
    let vids = index_videos(&env, &["Film A.mp4", "Film B.mkv"]);
    let h = hub(&env);
    h.login("ayse", SecretString::new("pw"), false);
    settle(&env, &h);
    assert!(h.is_logged_in());
    assert_eq!(h.current_username().as_deref(), Some("ayse"));
    assert_eq!(h.cards().len(), 2, "Keşfet lists indexed videos when logged in");
    h.set_liked_for(&vids[0], true);
    settle(&env, &h);
    assert_eq!(h.counts().liked, 1);
    let changes = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let c2 = changes.clone();
    h.on_change(Box::new(move |c| c2.borrow_mut().push(c)));
    h.logout();
    // Forgotten before the store answered.
    assert!(!h.is_logged_in());
    assert!(h.cards().is_empty());
    assert!(h.lists().is_empty());
    assert_eq!(h.counts(), sa_profiles::ProfileCounts::default());
    assert!(changes.borrow().contains(&HubChange::Session));
    settle(&env, &h);
    assert!(!env.profiles.is_logged_in());
    assert!(h.cards().is_empty());
    // The profile's history reappears on login.
    h.login("ayse", SecretString::new("pw"), false);
    settle(&env, &h);
    assert_eq!(h.counts().liked, 1);
}

#[test]
fn filters_multi_token_search_and_engagement_actions_work() {
    let _l = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    env.profiles.st.lock().users.push(("u".into(), "pw".into()));
    let vids = index_videos(&env, &["Tatil Antalya 2024.mp4", "Tatil Bodrum.mp4", "Doğum günü.mp4"]);
    env.touch("media/Tatil Bodrum.tr.srt", "");
    let h = hub(&env);
    h.login("u", SecretString::new("pw"), false);
    settle(&env, &h);
    assert_eq!(h.cards().len(), 3);
    let bodrum = h.cards().into_iter().find(|c| c.file_name == "Tatil Bodrum.mp4").unwrap();
    assert_eq!(bodrum.sidecar_badge.as_deref(), Some("SRT"), "a `.tr.srt` counts as a subtitle");
    h.search("tatil 2024");
    settle(&env, &h);
    let names: Vec<String> = h.cards().into_iter().map(|c| c.file_name).collect();
    assert_eq!(names, vec!["Tatil Antalya 2024.mp4"]);
    h.search("");
    h.set_rating_for(&vids[1], 4);
    h.set_liked_for(&vids[2], true);
    settle(&env, &h);
    h.set_filter(VideoFeedFilter::Liked);
    settle(&env, &h);
    assert_eq!(h.cards().iter().map(|c| c.file_name.clone()).collect::<Vec<_>>(), vec!["Doğum günü.mp4"]);
    h.set_filter(VideoFeedFilter::TopRated);
    settle(&env, &h);
    let top = h.cards();
    assert_eq!(top.len(), 1);
    assert_eq!(top[0].rating, 4);
    assert_eq!(top[0].rating_badge_text(), "★ 4/5");
    h.logout();
    settle(&env, &h);
    h.set_rating_for(&vids[0], 5);
    settle(&env, &h);
    assert!(env.profiles.stats_of("u", &vids[0]).is_none_or(|s| s.rating == 0), "logout locks everything");
}

#[test]
fn custom_lists_and_clear_watch_history_preserve_favorites_and_ratings() {
    let _l = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    env.profiles.st.lock().users.push(("u".into(), "pw".into()));
    let vids = index_videos(&env, &["a.mp4", "b.mp4"]);
    let h = hub(&env);
    h.login("u", SecretString::new("pw"), false);
    settle(&env, &h);
    h.player_video_changed(Some(&vids[0]), true);
    settle(&env, &h);
    h.set_liked(true);
    h.set_rating(3);
    settle(&env, &h);
    let st = env.profiles.stats_of("u", &vids[0]).unwrap();
    assert_eq!(st.view_count, 1);
    assert!(st.is_liked);
    assert_eq!(st.rating, 3);
    h.create_list(None);
    settle(&env, &h);
    assert_eq!(h.lists().iter().map(|l| l.name.clone()).collect::<Vec<_>>(), vec!["Daha Sonra İzle"]);
    let id = h.lists()[0].id.clone();
    assert_eq!(h.filter(), VideoFeedFilter::CustomList(Some(id.clone())), "the new list is shown");
    h.toggle_in_list(&id, &vids[1]);
    settle(&env, &h);
    assert!(h.is_in_list(&id, &vids[1]));
    assert_eq!(h.cards().iter().map(|c| c.path.clone()).collect::<Vec<_>>(), vec![vids[1].clone()]);
    h.create_list(None);
    settle(&env, &h);
    assert!(h.lists().iter().any(|l| l.name == "Listem 2"), "{:?}", h.lists().iter().map(|l| l.name.clone()).collect::<Vec<_>>());
    h.clear_watch_history();
    settle(&env, &h);
    let st = env.profiles.stats_of("u", &vids[0]).unwrap();
    assert_eq!(st.view_count, 0);
    assert!(st.is_liked, "favorites kept");
    assert_eq!(st.rating, 3, "ratings kept");
    assert_eq!(h.lists().len(), 2, "lists kept");
    h.delete_list(&id);
    settle(&env, &h);
    assert_eq!(h.lists().len(), 1);
    let current = h.lists()[0].id.clone();
    assert_eq!(h.filter(), VideoFeedFilter::CustomList(Some(current.clone())), "the shown list stays");
    h.delete_list(&current);
    assert_eq!(h.filter(), VideoFeedFilter::All, "deleting the shown list falls back to Keşfet");
    settle(&env, &h);
    assert!(h.lists().is_empty());
}

#[test]
fn mac_quick_lists_use_a_name_sequence_skipping_taken_names() {
    let _l = LanguageScope::new(CatalogChain::Mac, UiLanguage::Tr);
    let env = Env::mac();
    env.profiles.st.lock().users.push(("u".into(), "pw".into()));
    let h = hub(&env);
    h.login("u", SecretString::new("pw"), false);
    settle(&env, &h);
    h.create_list(Some("Listem 2"));
    settle(&env, &h);
    h.create_list(None);
    settle(&env, &h);
    let names: Vec<String> = h.lists().into_iter().map(|l| l.name).collect();
    assert_eq!(names, vec!["Listem 2", "Listem 3"]);
}

#[test]
fn name_sequence_prefers_the_free_name_and_caps_at_500_tries() {
    let seq: Vec<String> = NameSequence::new(Some("Daha Sonra İzle"), "Listem", &[]).take(2).collect();
    assert_eq!(seq, vec!["Daha Sonra İzle", "Listem 1"]);
    let taken: Vec<String> = vec!["daha sonra izle".into()];
    let seq: Vec<String> = NameSequence::new(Some("Daha Sonra İzle"), "Listem", &taken).take(1).collect();
    assert_eq!(seq, vec!["Listem 2"]);
    assert_eq!(NameSequence::new(None, "Listem", &[]).count(), 500);
}

#[test]
fn a_watch_is_recorded_for_opened_videos_but_not_for_playlist_ones_nor_logged_out() {
    let _l = LanguageScope::new(CatalogChain::Mac, UiLanguage::Tr);
    let env = Env::mac();
    env.profiles.st.lock().users.push(("u".into(), "pw".into()));
    let vids = index_videos(&env, &["a.mp4", "b.mp4", "c.mp4"]);
    let h = hub(&env);
    h.player_video_changed(Some(&vids[2]), true);
    settle(&env, &h);
    assert_eq!(env.profiles.st.lock().logged_out_io, 0, "nothing recorded logged out");
    h.login("u", SecretString::new("pw"), false);
    settle(&env, &h);
    h.player_video_changed(Some(&vids[0]), true);
    settle(&env, &h);
    h.player_video_changed(Some(&vids[1]), false);
    settle(&env, &h);
    assert_eq!(env.profiles.stats_of("u", &vids[0]).map(|s| s.view_count), Some(1));
    assert!(env.profiles.stats_of("u", &vids[1]).is_none_or(|s| s.view_count == 0));
    assert!(env.profiles.stats_of("u", &vids[2]).is_none());
}

#[test]
fn progress_goes_to_the_closing_profile_at_logout_then_nothing_is_recorded() {
    let _l = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    env.profiles.st.lock().users.push(("u".into(), "pw".into()));
    let vids = index_videos(&env, &["a.mp4"]);
    let h = hub(&env);
    h.login("u", SecretString::new("pw"), false);
    settle(&env, &h);
    h.player_video_changed(Some(&vids[0]), true);
    h.report_playback(PlaybackReport::Opened { duration: 100.0 });
    h.report_playback(PlaybackReport::Progress { position: 10.0, duration: 100.0 });
    settle(&env, &h);
    assert_eq!(env.profiles.stats_of("u", &vids[0]).unwrap().last_position_seconds, 10.0);
    h.report_playback(PlaybackReport::Progress { position: 20.0, duration: 100.0 });
    h.logout();
    settle(&env, &h);
    assert_eq!(env.profiles.stats_of("u", &vids[0]).unwrap().last_position_seconds, 20.0, "closing profile got the position");
    let writes = env.profiles.st.lock().progress_writes;
    h.report_playback(PlaybackReport::Paused { position: 50.0 });
    h.report_playback(PlaybackReport::Progress { position: 60.0, duration: 100.0 });
    settle(&env, &h);
    assert_eq!(env.profiles.st.lock().progress_writes, writes, "nothing recorded after logout");
    assert_eq!(env.profiles.st.lock().logged_out_io, 0);
}

#[test]
fn thumbnails_are_generated_and_stored_only_when_logged_in() {
    let _l = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    env.profiles.st.lock().users.push(("u".into(), "pw".into()));
    let vids = index_videos(&env, &["a.mp4"]);
    let h = hub(&env);
    settle(&env, &h);
    assert!(env.thumbs.calls.lock().is_empty(), "never generated logged out");
    h.login("u", SecretString::new("pw"), false);
    settle(&env, &h);
    env.eventually("thumbnail", || h.thumbnail(&vids[0]).is_some());
    assert!(env.profiles.thumbnail(&vids[0]).is_some(), "saved encrypted for the profile");
    h.logout();
    assert!(h.thumbnail(&vids[0]).is_none(), "pictures forgotten at logout");
}

#[test]
fn windows_cards_page_in_steps_of_40() {
    let _l = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    env.profiles.st.lock().users.push(("u".into(), "pw".into()));
    let names: Vec<String> = (0..45).map(|i| format!("v{i:02}.mp4")).collect();
    let refs: Vec<&str> = names.iter().map(|x| x.as_str()).collect();
    index_videos(&env, &refs);
    let h = hub(&env);
    h.login("u", SecretString::new("pw"), false);
    settle(&env, &h);
    assert_eq!(h.total_cards(), 45);
    assert_eq!(h.cards().len(), 40);
    h.load_more();
    assert_eq!(h.cards().len(), 45);
}

#[test]
fn remember_me_is_opt_in_and_logout_resets_it() {
    let _l = LanguageScope::new(CatalogChain::Mac, UiLanguage::Tr);
    let env = Env::mac();
    env.profiles.st.lock().users.push(("u".into(), "pw".into()));
    let h = hub(&env);
    assert!(!h.remember_me(), "default off");
    h.login("u", SecretString::new("pw"), true);
    settle(&env, &h);
    assert!(h.remember_me());
    assert!(env.profiles.remember_me());
    h.logout();
    assert!(!h.remember_me());
    settle(&env, &h);
    assert!(!env.profiles.remember_me());
}

#[test]
fn wrong_password_reports_the_error() {
    let _l = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    env.profiles.st.lock().users.push(("u".into(), "pw".into()));
    let h = hub(&env);
    h.login("u", SecretString::new("nope"), false);
    settle(&env, &h);
    assert_eq!(h.last_error(), Some(sa_profiles::ProfileError::InvalidCredentials));
    assert_eq!(h.last_error().unwrap().message(), "Kullanıcı adı veya şifre hatalı.");
    assert!(!h.is_logged_in());
}

#[test]
fn external_changes_of_the_profile_refresh_the_feed() {
    let _l = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    env.profiles.st.lock().users.push(("u".into(), "pw".into()));
    let vids = index_videos(&env, &["a.mp4"]);
    let h = hub(&env);
    h.login("u", SecretString::new("pw"), false);
    settle(&env, &h);
    env.profiles.set_liked(&vids[0], true);
    env.profiles.emit(sa_profiles::ProfileChange::External);
    env.eventually("refreshed", || h.counts().liked == 1);
}

#[test]
fn engagement_bar_is_visible_only_logged_in_with_a_video_open() {
    let _l = LanguageScope::new(CatalogChain::Mac, UiLanguage::Tr);
    let env = Env::mac();
    env.profiles.st.lock().users.push(("u".into(), "pw".into()));
    let vids = index_videos(&env, &["a.mp4"]);
    let h = hub(&env);
    h.player_video_changed(Some(&vids[0]), true);
    settle(&env, &h);
    assert!(!h.engagement().visible);
    h.login("u", SecretString::new("pw"), false);
    settle(&env, &h);
    assert!(h.engagement().visible);
    h.player_video_changed(None, false);
    assert!(!h.engagement().visible);
}

#[test]
fn text_helpers() {
    let _l = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    assert_eq!(duration_text(65.9, HubDateFormat::DotNetGeneral), "01:05");
    assert_eq!(duration_text(3725.0, HubDateFormat::DotNetGeneral), "01:02:05");
    assert_eq!(duration_text(65.6, HubDateFormat::AppleAbbreviated), "1:06");
    assert_eq!(duration_text(0.0, HubDateFormat::AppleAbbreviated), "");
    let sep = std::path::MAIN_SEPARATOR;
    let dir = format!("{sep}media{sep}Tatil");
    assert_eq!(folder_line(&dir), format!("Tatil ({dir})"));
    assert_eq!(folder_line(""), "");
    let t = Ticks(Ticks::UNIX_EPOCH + 1_700_000_000 * 10_000_000);
    let tr = date_text(t, HubDateFormat::DotNetGeneral, UiLanguage::Tr);
    assert!(regex_like(&tr, "dd.dd.dddd dd:dd"), "{tr}");
    let en = date_text(t, HubDateFormat::DotNetGeneral, UiLanguage::En);
    assert!(en.ends_with(" AM") || en.ends_with(" PM"), "{en}");
}

/// `d` = digit, anything else literal.
fn regex_like(s: &str, pattern: &str) -> bool {
    s.chars().count() == pattern.chars().count()
        && s.chars().zip(pattern.chars()).all(|(c, p)| if p == 'd' { c.is_ascii_digit() } else { c == p })
}
