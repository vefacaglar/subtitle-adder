//! Ports of `MediaLibraryIndexTests` (explorer VM part, C#) and `FileExplorerModelTests` (Swift), with the rule-6
//! decisions of UNIFIED.md §3.10 / §4 item 8.

mod common;

use std::time::Duration;

use common::*;
use sa_core::l10n::{CatalogChain, UiLanguage};
use sa_core::platform::{QuickLocation, QuickLocationKind};
use sa_core::settings::{SettingsStore, keys};
use sa_viewmodels::explorer::{FileExplorerModel, fold, size_text};
use sa_viewmodels::policy::ByteSizeFormat;

fn names(m: &FileExplorerModel) -> Vec<String> {
    m.rows().into_iter().map(|r| r.name).collect()
}

fn idle(env: &Env, m: &FileExplorerModel) {
    env.eventually("scan done", || !m.is_scanning());
    env.ui.run_until(Duration::from_millis(50), || false);
}

fn tree(env: &Env) {
    env.touch("lib/Movies/Alpha.mp4", "aaaa");
    env.touch("lib/Movies/Beta.mkv", "bb");
    env.touch("lib/Movies/Beta.srt", "");
    env.touch("lib/Movies/notes.txt", "");
    env.touch("lib/Movies/Series/Ep1.mp4", "e");
    env.touch("lib/Other/Gamma.mp4", "g");
}

#[test]
fn navigation_back_up_and_search() {
    let _l = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    tree(&env);
    let m = FileExplorerModel::new(env.services());
    m.navigate(&env.path("lib/Movies"));
    idle(&env, &m);
    assert_eq!(names(&m), vec!["Series", "Alpha.mp4", "Beta.mkv"]);
    let beta = m.rows().into_iter().find(|r| r.name == "Beta.mkv").unwrap();
    assert_eq!(beta.sidecar_badge.as_deref(), Some("SRT"));
    m.navigate(&env.path("lib/Movies/Series"));
    idle(&env, &m);
    assert_eq!(names(&m), vec!["Ep1.mp4"]);
    assert!(m.can_go_back());
    m.go_up();
    idle(&env, &m);
    assert_eq!(m.current_folder(), Some(env.path("lib/Movies")));
    m.go_back();
    idle(&env, &m);
    assert_eq!(m.current_folder(), Some(env.path("lib/Movies/Series")));
    m.go_back();
    idle(&env, &m);
    assert_eq!(m.current_folder(), Some(env.path("lib/Movies")));
    m.set_search("alp", false);
    env.pump();
    assert_eq!(names(&m), vec!["Alpha.mp4"]);
    assert_eq!(m.status_text(), "1 video · SSD indeksinde 3 dosya");
}

#[test]
fn logged_out_search_filters_the_current_folder_only() {
    for policy in [sa_viewmodels::PlatformPolicy::windows(), sa_viewmodels::PlatformPolicy::mac()] {
        let _l = LanguageScope::new(policy.catalog_chain, UiLanguage::Tr);
        let env = Env::new(policy);
        tree(&env);
        use sa_media::MediaIndex;
        env.index.scan(&env.path("lib/Other"), &sa_core::cancel::CancellationToken::new());
        let m = FileExplorerModel::new(env.services());
        m.navigate(&env.path("lib/Movies"));
        idle(&env, &m);
        m.set_search("gamma", false);
        env.ui.run_until(Duration::from_millis(400), || false);
        assert!(names(&m).is_empty(), "no index-wide results logged out");
        assert_eq!(env.index.search_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        m.set_search("gamma", true);
        env.eventually("index hit", || names(&m) == vec!["Gamma.mp4"]);
        assert!(m.rows()[0].is_index_result);
        m.set_logged_in(false);
        assert!(names(&m).is_empty(), "hits dropped at once on logout");
    }
}

#[test]
fn the_last_folder_is_never_stored() {
    let _l = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    tree(&env);
    env.settings.set_string(keys::EXPLORER_LAST_FOLDER, "C:\\secret");
    let m = FileExplorerModel::new(env.services());
    assert_eq!(env.settings.get_string(keys::EXPLORER_LAST_FOLDER), None, "removed at startup");
    m.navigate(&env.path("lib/Movies"));
    idle(&env, &m);
    assert_eq!(env.settings.get_string(keys::EXPLORER_LAST_FOLDER), None, "never written");
}

#[test]
fn starts_in_the_videos_library_and_the_first_video_moves_a_fresh_explorer() {
    let _l = LanguageScope::new(CatalogChain::Mac, UiLanguage::Tr);
    let env = Env::mac();
    tree(&env);
    *env.shell.quick.lock() = vec![
        QuickLocation { title: "Filmler".into(), path: env.path("lib/Movies"), kind: QuickLocationKind::Videos },
        QuickLocation { title: "Masaüstü".into(), path: env.path("lib"), kind: QuickLocationKind::Desktop },
    ];
    let m = FileExplorerModel::new(env.services());
    m.start();
    env.eventually("initial folder", || m.current_folder() == Some(env.path("lib/Movies")));
    idle(&env, &m);
    assert_eq!(m.current_folder_name(), "Filmler");
    assert_eq!(m.quick_locations().len(), 2);
    m.sync_to_video(Some(&env.path("lib/Other/Gamma.mp4")));
    env.eventually("moved", || m.current_folder() == Some(env.path("lib/Other")));
    idle(&env, &m);
    assert!(m.rows().iter().any(|r| r.name == "Gamma.mp4" && r.is_active));
    m.sync_to_video(Some(&env.path("lib/Movies/Alpha.mp4")));
    idle(&env, &m);
    assert_eq!(m.current_folder(), Some(env.path("lib/Other")), "later videos do not move it");
    assert_eq!(m.status_text(), "1 medya · indekste 3 dosya");
}

#[test]
fn sync_to_video_and_saving_update_the_sidecar_status() {
    let _l = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    tree(&env);
    let m = FileExplorerModel::new(env.services());
    m.navigate(&env.path("lib/Movies"));
    idle(&env, &m);
    assert!(m.rows().iter().find(|r| r.name == "Alpha.mp4").unwrap().sidecar_badge.is_none());
    let srt = env.touch("lib/Movies/Alpha.srt", "1\n");
    m.subtitle_saved(&srt);
    env.eventually("badge", || m.rows().iter().find(|r| r.name == "Alpha.mp4").is_some_and(|r| r.sidecar_badge.as_deref() == Some("SRT")));
}

#[test]
fn sidebar_visibility_defaults_on_and_persists() {
    let _l = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    let m = FileExplorerModel::new(env.services());
    assert!(m.is_sidebar_visible());
    m.toggle_sidebar();
    assert_eq!(env.settings.get_bool(keys::SHOW_FILE_EXPLORER), Some(false));
    let m2 = FileExplorerModel::new(env.services());
    assert!(!m2.is_sidebar_visible());
}

#[test]
fn a_missing_folder_reports_not_found() {
    let _l = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    let m = FileExplorerModel::new(env.services());
    m.navigate(&env.path("nope"));
    idle(&env, &m);
    assert_eq!(m.availability(), sa_viewmodels::explorer::FolderAvailability::NotFound);
    assert!(m.rows().is_empty());
}

#[test]
fn mac_search_folds_case_diacritics_and_dotless_i() {
    assert_eq!(fold("İSTANBUL"), fold("istanbul"));
    assert_eq!(fold("ılık"), "ilik");
    assert_eq!(fold("Café"), "cafe");
    assert_eq!(fold("Ｆｉｌｍ"), "film");
    let _l = LanguageScope::new(CatalogChain::Mac, UiLanguage::Tr);
    let env = Env::mac();
    env.touch("d/Işık Çağı.mp4", "x");
    env.touch("d/other.mp4", "x");
    let m = FileExplorerModel::new(env.services());
    m.navigate(&env.path("d"));
    idle(&env, &m);
    m.set_search("isik caği", false);
    env.ui.run_until(Duration::from_millis(400), || false);
    assert_eq!(names(&m), vec!["Işık Çağı.mp4"]);
}

#[test]
fn size_texts_per_platform() {
    let l = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    assert_eq!(size_text(0, ByteSizeFormat::WindowsBinary), "0 MB");
    assert_eq!(size_text(1_572_864, ByteSizeFormat::WindowsBinary), "1,5 MB");
    assert_eq!(size_text(524_288, ByteSizeFormat::WindowsBinary), "0,5 MB");
    assert_eq!(size_text(2_147_483_648, ByteSizeFormat::WindowsBinary), "2 GB");
    l.set(UiLanguage::En);
    assert_eq!(size_text(1_572_864, ByteSizeFormat::WindowsBinary), "1.5 MB");
    assert_eq!(size_text(500, ByteSizeFormat::AppleDecimal), "500 bytes");
    assert_eq!(size_text(2_500_000, ByteSizeFormat::AppleDecimal), "2.5 MB");
}
