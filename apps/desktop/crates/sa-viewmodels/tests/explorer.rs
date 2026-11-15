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
fn search_also_finds_files_in_the_folders_below_without_indexing_them() {
    for policy in [sa_viewmodels::PlatformPolicy::windows(), sa_viewmodels::PlatformPolicy::mac()] {
        use sa_media::MediaIndex;
        use std::sync::atomic::Ordering::SeqCst;
        let _l = LanguageScope::new(policy.catalog_chain, UiLanguage::Tr);
        let env = Env::new(policy);
        tree(&env);
        env.touch("lib/Movies/Series/Season 2/Ep7.mkv", "s");
        let m = FileExplorerModel::new(env.services());
        m.navigate(&env.path("lib/Movies"));
        idle(&env, &m);
        let scans = env.index.scans.load(SeqCst);
        // Logged out: the folders below are listed live, nearest first.
        m.set_search("ep", false);
        env.eventually("files below the folder", || names(&m) == vec!["Ep1.mp4", "Ep7.mkv"]);
        idle(&env, &m);
        let rows = m.rows();
        assert!(rows.iter().all(|r| r.is_index_result), "marked as outside the current folder");
        assert_eq!(rows[1].path, env.path("lib/Movies/Series/Season 2/Ep7.mkv"));
        assert_eq!(env.index.search_calls.load(SeqCst), 0, "the index is not searched logged out");
        assert_eq!(env.index.scans.load(SeqCst), scans, "nothing is written to the index");
        assert!(env.index.cached_snapshot(&env.path("lib/Movies/Series")).is_none());
        let listed = env.index.transient_scans.lock().len();
        assert_eq!(listed, 3, "the folder, Series and Season 2");
        // A folder's name finds the files in it; typing on filters what was listed.
        m.set_search("season", false);
        env.pump();
        assert_eq!(names(&m), vec!["Ep7.mkv"]);
        // A sibling folder is not below the current one.
        m.set_search("gamma", false);
        env.pump();
        assert!(names(&m).is_empty());
        assert_eq!(env.index.transient_scans.lock().len(), listed, "listed once per search");
        // A subtitle saved next to a result shows its badge (that folder alone is listed again).
        m.set_search("ep7", false);
        env.pump();
        assert_eq!(m.rows()[0].sidecar_badge, None);
        m.subtitle_saved(&env.touch("lib/Movies/Series/Season 2/Ep7.srt", "1\n"));
        env.eventually("badge of a result below", || m.rows().first().is_some_and(|r| r.sidecar_badge.as_deref() == Some("SRT")));
        assert_eq!(env.index.transient_scans.lock().len(), listed + 1);
        assert!(env.index.cached_snapshot(&env.path("lib/Movies/Series/Season 2")).is_none());
        // The files of the current folder come first.
        env.touch("lib/Movies/Epilogue.mp4", "x");
        m.refresh();
        idle(&env, &m);
        m.set_search("ep", false);
        env.eventually("current folder first", || names(&m) == vec!["Epilogue.mp4", "Ep1.mp4", "Ep7.mkv"]);
        idle(&env, &m);
        // Ending the search shows the folder again; one level up, the search reaches every folder below.
        m.set_search("", false);
        env.pump();
        assert_eq!(names(&m), vec!["Series", "Alpha.mp4", "Beta.mkv", "Epilogue.mp4"]);
        m.go_up();
        idle(&env, &m);
        m.set_search("gamma", false);
        env.eventually("from the parent", || names(&m) == vec!["Gamma.mp4"]);
        idle(&env, &m);
        m.navigate(&env.path("lib/Other"));
        idle(&env, &m);
        assert!(m.search_query().is_empty());
        assert_eq!(names(&m), vec!["Gamma.mp4"], "the listing of the folder left behind is dropped");
    }
}

#[test]
fn logged_out_search_never_reaches_the_index() {
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
    env.profiles.st.lock().current = Some("ayse".into());
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
fn the_launch_folder_is_indexed_at_startup_even_with_the_sidebar_hidden_but_not_crawled() {
    // Both originals list (and index) the launch folder at startup whatever `showFileExplorer` says: C#
    // `FileExplorerPane.OnLoaded` → `RefreshAsync` (WinUI raises `Loaded` for the Collapsed pane too; checked with a
    // WinUI 1.8 probe) → `NavigateCoreAsync(initial, crawl: false)` → `ScanDirectoryAsync`; Swift
    // `ContentView.task { explorer.start() }` (the window's root, not the sidebar) → `runScan(crawl: false)`. Neither
    // crawls below it, and neither needs a profile (a file browser, UNIFIED §3.10).
    for policy in [sa_viewmodels::PlatformPolicy::windows(), sa_viewmodels::PlatformPolicy::mac()] {
        use sa_media::MediaIndex;
        let _l = LanguageScope::new(policy.catalog_chain, UiLanguage::Tr);
        let env = Env::new(policy);
        tree(&env);
        movies_quick(&env);
        env.settings.set_bool(keys::SHOW_FILE_EXPLORER, false);
        let m = FileExplorerModel::new(env.services());
        assert!(!m.is_sidebar_visible());
        assert_eq!(env.index.scans.load(std::sync::atomic::Ordering::SeqCst), 0, "the constructor touches nothing");
        m.start();
        env.eventually("initial folder", || m.current_folder() == Some(env.path("lib/Movies")));
        idle(&env, &m);
        assert_eq!(env.index.scans.load(std::sync::atomic::Ordering::SeqCst), 1, "exactly the launch folder");
        assert!(env.index.cached_snapshot(&env.path("lib/Movies")).is_some());
        assert!(env.index.cached_snapshot(&env.path("lib/Movies/Series")).is_none(), "no crawl at startup");
        assert!(env.index.cached_snapshot(&env.path("lib/Other")).is_none());
        assert!(env.index.transient_scans.lock().is_empty());
    }
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
    // Windows rows: `IndexedMediaFile.FormattedSize` (invariant, B/KB/MB/GB), whatever the UI language.
    let l = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    assert_eq!(size_text(0, ByteSizeFormat::WindowsBinary), "0 B");
    assert_eq!(size_text(1_572_864, ByteSizeFormat::WindowsBinary), "1.5 MB");
    assert_eq!(size_text(524_288, ByteSizeFormat::WindowsBinary), "512 KB");
    assert_eq!(size_text(500_000, ByteSizeFormat::WindowsBinary), "488.3 KB");
    assert_eq!(size_text(2_147_483_648, ByteSizeFormat::WindowsBinary), "2 GB");
    l.set(UiLanguage::En);
    assert_eq!(size_text(1_572_864, ByteSizeFormat::WindowsBinary), "1.5 MB");
    assert_eq!(size_text(500, ByteSizeFormat::AppleDecimal), "500 bytes");
    assert_eq!(size_text(2_500_000, ByteSizeFormat::AppleDecimal), "2.5 MB");
}

fn movies_quick(env: &Env) {
    *env.shell.quick.lock() = vec![QuickLocation { title: "Videolar".into(), path: env.path("lib/Movies"), kind: QuickLocationKind::Videos }];
}

#[test]
fn windows_an_opened_video_never_writes_its_folder_to_the_index() {
    // C# `SyncToVideoPathAsync` after 43ab997: a fresh explorer shows the video's folder transiently; later videos
    // only refresh already indexed folders, in memory.
    use sa_media::MediaIndex;
    let _l = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    tree(&env);
    let m = FileExplorerModel::new(env.services());
    m.sync_to_video(Some(&env.path("lib/Other/Gamma.mp4")));
    idle(&env, &m);
    assert_eq!(m.current_folder(), Some(env.path("lib/Other")));
    assert!(m.rows().iter().any(|r| r.name == "Gamma.mp4" && r.is_active));
    assert_eq!(*env.index.transient_scans.lock(), vec![env.path("lib/Other")]);
    assert!(env.index.cached_snapshot(&env.path("lib/Other")).is_none(), "not recorded in the index");
    assert_eq!(env.index.scans.load(std::sync::atomic::Ordering::SeqCst), 0);
    m.refresh();
    idle(&env, &m);
    assert!(env.index.cached_snapshot(&env.path("lib/Other")).is_none(), "a refresh keeps it transient");
    // Browsing persists; going back to the transient folder does not.
    m.navigate(&env.path("lib/Movies"));
    idle(&env, &m);
    assert!(env.index.cached_snapshot(&env.path("lib/Movies")).is_some());
    m.go_back();
    idle(&env, &m);
    assert_eq!(m.current_folder(), Some(env.path("lib/Other")));
    assert!(env.index.cached_snapshot(&env.path("lib/Other")).is_none());
    // A video from a folder that was never indexed: nothing is scanned; from an indexed one: memory only.
    env.touch("lib/Third/x.mp4", "x");
    let persisted = env.index.scans.load(std::sync::atomic::Ordering::SeqCst);
    m.sync_to_video(Some(&env.path("lib/Third/x.mp4")));
    idle(&env, &m);
    assert!(env.index.cached_snapshot(&env.path("lib/Third")).is_none());
    m.sync_to_video(Some(&env.path("lib/Movies/Alpha.mp4")));
    env.eventually("in-memory refresh", || env.index.memory_refreshes.lock().contains(&env.path("lib/Movies")));
    assert_eq!(env.index.scans.load(std::sync::atomic::Ordering::SeqCst), persisted, "the index file is never written");
    assert_eq!(m.current_folder(), Some(env.path("lib/Other")), "later videos do not move it");
}

#[test]
fn windows_the_launch_folder_is_not_left_for_the_first_video() {
    let _l = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    tree(&env);
    movies_quick(&env);
    let m = FileExplorerModel::new(env.services());
    m.start();
    env.eventually("initial folder", || m.current_folder() == Some(env.path("lib/Movies")));
    idle(&env, &m);
    m.sync_to_video(Some(&env.path("lib/Other/Gamma.mp4")));
    idle(&env, &m);
    assert_eq!(m.current_folder(), Some(env.path("lib/Movies")));
}

#[test]
fn mac_a_video_opened_without_a_profile_does_not_write_its_folder_to_the_index() {
    use sa_media::MediaIndex;
    let _l = LanguageScope::new(CatalogChain::Mac, UiLanguage::Tr);
    let env = Env::mac();
    tree(&env);
    movies_quick(&env);
    let m = FileExplorerModel::new(env.services());
    m.start();
    env.eventually("initial folder", || m.current_folder() == Some(env.path("lib/Movies")));
    idle(&env, &m);
    m.sync_to_video(Some(&env.path("lib/Other/Gamma.mp4")));
    idle(&env, &m);
    assert_eq!(m.current_folder(), Some(env.path("lib/Movies")));
    assert!(env.index.cached_snapshot(&env.path("lib/Other")).is_none());
    let srt = env.touch("lib/Other/Gamma.srt", "1\n");
    m.subtitle_saved(&srt);
    idle(&env, &m);
    assert!(env.index.cached_snapshot(&env.path("lib/Other")).is_none(), "a save elsewhere is not indexed logged out");
}

#[test]
fn a_session_change_forgets_the_browsed_folders() {
    for policy in [sa_viewmodels::PlatformPolicy::windows(), sa_viewmodels::PlatformPolicy::mac()] {
        let _l = LanguageScope::new(policy.catalog_chain, UiLanguage::Tr);
        let env = Env::new(policy);
        tree(&env);
        movies_quick(&env);
        env.profiles.st.lock().current = Some("ayse".into());
        let m = FileExplorerModel::new(env.services());
        m.start();
        env.eventually("initial folder", || m.current_folder() == Some(env.path("lib/Movies")));
        idle(&env, &m);
        m.navigate(&env.path("lib/Other"));
        idle(&env, &m);
        m.navigate(&env.path("lib/Movies/Series"));
        idle(&env, &m);
        m.set_search("ep", true);
        assert!(m.can_go_back());
        // The same profile again changes nothing.
        m.set_logged_in(true);
        assert!(m.can_go_back());
        env.profiles.st.lock().current = None;
        m.set_logged_in(false);
        env.eventually("back at the launch folder", || m.current_folder() == Some(env.path("lib/Movies")));
        assert!(!m.can_go_back());
        assert!(m.search_query().is_empty());
        // Logging in later starts no reset of its own.
        env.profiles.st.lock().current = Some("mehmet".into());
        m.navigate(&env.path("lib/Other"));
        idle(&env, &m);
        m.set_logged_in(true);
        idle(&env, &m);
        assert_eq!(m.current_folder(), Some(env.path("lib/Other")));
    }
}
