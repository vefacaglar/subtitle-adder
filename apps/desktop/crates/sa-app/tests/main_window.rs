//! Main-window wiring tests: a real `MainWindow` (software renderer, no display) attached to a real
//! `SubtitleViewModel` over the view-model test fakes, checked through the bridge globals. Covers the UI-level
//! behaviors of win-ui-main §6–§17 and mac-ui §3–§7 that live in `sa-app` (rows, selection, keys, layout, drops,
//! menus, error bar, macOS menu state). The compiled UI tree does not matter: both trees export the same bridges.

#[path = "../../sa-viewmodels/tests/common/mod.rs"]
mod common;

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use parking_lot::Mutex;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter};
use slint::{ComponentHandle, Model};

use sa_app::main_window::{MainWindowController, MainWindowDeps, MainWindowHooks};
use sa_app::ui::{self, AppBridge, MainBridge, MainWindow, MenuBridge};
use sa_core::dispatch::UiDispatcher;
use sa_core::l10n::{CatalogChain, UiLanguage};
use sa_core::models::ModelCatalog;
use sa_core::settings::SettingsStore;
use sa_player::{LoadGeneration, MediaPlayerBackend, MediaSource, PlayerEvent, VideoFrame};
use sa_viewmodels::SubtitleViewModel;
use sa_viewmodels::types::Workspace;

use common::{Env, LanguageScope};

// ------------------------------------------------------------------------------------------------ harness

struct TestPlatform(Rc<MinimalSoftwareWindow>);

impl Platform for TestPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

fn install_platform() {
    // Slint's platform is per thread; every test thread installs its own once.
    thread_local!(static DONE: Cell<bool> = const { Cell::new(false) });
    DONE.with(|d| {
        if !d.get() {
            let _ = slint::platform::set_platform(Box::new(TestPlatform(MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer))));
            d.set(true);
        }
    });
}

#[derive(Default)]
struct BackendState {
    generation: i32,
    loads: Vec<Option<PathBuf>>,
    sink: Option<Arc<dyn Fn(PlayerEvent) + Send + Sync>>,
    playing: bool,
}

struct FakeBackend(Arc<Mutex<BackendState>>);

impl MediaPlayerBackend for FakeBackend {
    fn load(&mut self, source: Option<MediaSource>, _auto_play: bool) -> LoadGeneration {
        let mut s = self.0.lock();
        s.generation += 1;
        s.loads.push(source.map(|m| match m {
            MediaSource::File(p) => p,
            MediaSource::Reader { path, .. } => path,
        }));
        s.generation
    }
    fn play(&mut self) {
        self.0.lock().playing = true;
    }
    fn pause(&mut self) {
        self.0.lock().playing = false;
    }
    fn seek(&mut self, _seconds: f64) {}
    fn set_volume(&mut self, _volume: f64) {}
    fn set_muted(&mut self, _muted: bool) {}
    fn position(&self) -> f64 {
        0.0
    }
    fn duration(&self) -> f64 {
        0.0
    }
    fn is_playing(&self) -> bool {
        self.0.lock().playing
    }
    fn natural_size(&self) -> Option<(u32, u32)> {
        None
    }
    fn buffered_ratio(&self) -> f64 {
        0.0
    }
    fn latest_frame(&mut self) -> Option<VideoFrame> {
        None
    }
    fn set_event_sink(&mut self, sink: Box<dyn Fn(PlayerEvent) + Send + Sync>) {
        self.0.lock().sink = Some(Arc::from(sink));
    }
    fn dispose(&mut self) {}
}

struct Rig {
    env: Env,
    _lang: LanguageScope,
    vm: Rc<SubtitleViewModel>,
    _catalog: Rc<ModelCatalog>,
    window: MainWindow,
    controller: Rc<MainWindowController>,
    backend: Arc<Mutex<BackendState>>,
    full_screen_calls: Rc<RefCell<Vec<bool>>>,
    settings_opened: Rc<Cell<usize>>,
}

fn rig(mac: bool) -> Rig {
    install_platform();
    let lang = LanguageScope::new(if mac { CatalogChain::Mac } else { CatalogChain::Windows }, UiLanguage::Tr);
    let env = if mac { Env::mac() } else { Env::win() };
    let (vm, catalog) = env.vm();
    let vm = Rc::new(vm);
    let window = MainWindow::new().expect("main window");
    sa_app::loc::install(&window);
    let backend = Arc::new(Mutex::new(BackendState::default()));
    let full_screen_calls = Rc::new(RefCell::new(Vec::new()));
    let settings_opened = Rc::new(Cell::new(0));
    let fs = full_screen_calls.clone();
    let so = settings_opened.clone();
    let ui: Arc<dyn UiDispatcher> = env.ui.clone();
    let deps = MainWindowDeps {
        vm: vm.clone(),
        explorer: None,
        hub: None,
        settings: env.settings.clone(),
        appearance: None,
        shell: env.shell.clone(),
        ui,
        player: Box::new(FakeBackend(backend.clone())),
        source_factory: None,
        preview: None,
        hooks: MainWindowHooks {
            set_full_screen: Some(Rc::new(move |on| fs.borrow_mut().push(on))),
            open_settings: Some(Rc::new(move || so.set(so.get() + 1))),
            ..Default::default()
        },
    };
    let controller = MainWindowController::attach(&window, deps);
    env.pump();
    Rig { env, _lang: lang, vm, _catalog: catalog, window, controller, backend, full_screen_calls, settings_opened }
}

impl Rig {
    fn main(&self) -> MainBridge<'_> {
        self.window.global::<MainBridge>()
    }
    fn pump(&self) {
        self.env.pump();
    }
    fn srt(&self, name: &str, lines: &[(&str, &str, &str)]) -> PathBuf {
        let mut body = String::new();
        for (i, (a, b, t)) in lines.iter().enumerate() {
            body.push_str(&format!("{}\n{a} --> {b}\n{t}\n\n", i + 1));
        }
        self.env.touch(name, &body)
    }
    fn load_three_lines(&self) -> PathBuf {
        let p = self.srt(
            "clip.srt",
            &[("00:00:00,000", "00:00:02,000", "Bir."), ("00:00:02,000", "00:00:04,000", "İki."), ("00:00:04,000", "00:00:06,000", "Üç.")],
        );
        assert!(self.vm.load_subtitle_file(&p));
        self.pump();
        p
    }
    fn key(&self, text: &str, shift: bool, control: bool) -> bool {
        self.main().invoke_key_pressed(text.into(), shift, control, false, false, false)
    }
    fn rows(&self) -> Vec<ui::SentenceRowData> {
        let m = self.main().get_rows();
        (0..m.row_count()).filter_map(|i| m.row_data(i)).collect()
    }
    fn open_video(&self, rel: &str) -> PathBuf {
        let p = self.env.touch(rel, "not really a video");
        self.vm.open_video(Some(p.clone()));
        self.pump();
        p
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.controller.detach();
        self.vm.shutdown();
    }
}

// ------------------------------------------------------------------------------------------------ tests

#[test]
fn attaching_pushes_the_initial_state() {
    let r = rig(false);
    let main = r.main();
    assert_eq!(r.window.global::<AppBridge>().get_window_title(), "Subtitle Adder");
    assert_eq!(r.window.global::<AppBridge>().get_title_file(), "");
    assert_eq!(main.get_status_text(), "Hazır");
    assert_eq!(main.get_language_items().row_count(), 10);
    assert_eq!(main.get_target_items().row_count(), 9);
    assert_eq!(main.get_workspace(), ui::Workspace::Editor);
    assert!(!main.get_has_video() && !main.get_has_sentences());
    assert!(main.get_show_file_explorer(), "showFileExplorer defaults to true");
    assert_eq!(main.get_layout_mode(), 2, "stacked by default");
    assert_eq!(main.get_total_text(), "--:--");
}

#[test]
fn a_loaded_subtitle_fills_the_rows_with_display_ready_text() {
    let r = rig(false);
    r.load_three_lines();
    let rows = r.rows();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].number, "1");
    assert_eq!(rows[0].time_text, "00:00.00 → 00:02.00");
    assert_eq!(rows[1].text, "İki.");
    assert_eq!(rows[1].accessible_label, "2. İki.");
    assert!(!rows[0].has_translation);
    assert!(r.main().get_has_sentences());
    assert!(!r.main().get_shows_speakers());
}

#[test]
fn editing_one_line_keeps_the_other_rows_in_place() {
    let r = rig(false);
    r.load_three_lines();
    let before = r.rows();
    let uid = r.vm.sentences()[1].uid;
    r.vm.set_text(uid, "Değişti.");
    r.pump();
    let after = r.rows();
    assert_eq!(after.len(), 3);
    assert_eq!(after[1].text, "Değişti.");
    assert_eq!(after[0].uid, before[0].uid);
    assert_eq!(after[2].uid, before[2].uid);
}

#[test]
fn clicks_select_one_ctrl_toggles_and_shift_extends() {
    let r = rig(false);
    r.load_three_lines();
    let uids: Vec<_> = r.vm.sentences().iter().map(|s| s.uid).collect();
    r.main().invoke_row_pressed(0, false, false);
    r.pump();
    assert_eq!(r.vm.selection(), Some(uids[0]));
    r.main().invoke_row_pressed(2, false, true);
    r.pump();
    assert_eq!(r.vm.selected_uids().len(), 3);
    r.main().invoke_row_pressed(1, true, false);
    r.pump();
    assert!(!r.vm.selected_uids().contains(&uids[1]));
    assert!(r.rows()[0].is_selected && !r.rows()[1].is_selected);
    // Arrow keys in the list move the selection.
    r.main().invoke_row_pressed(0, false, false);
    assert!(r.main().invoke_list_key("down".into(), false));
    r.pump();
    assert_eq!(r.vm.selection(), Some(uids[1]));
}

#[test]
fn delete_in_the_list_removes_the_selected_lines() {
    let r = rig(false);
    r.load_three_lines();
    r.main().invoke_row_pressed(0, false, false);
    r.main().invoke_row_pressed(1, true, false);
    r.pump();
    assert!(r.main().invoke_list_key("delete".into(), false));
    r.pump();
    // FIX-W (UNIFIED §3.5): every selected line is deleted.
    assert_eq!(r.vm.sentence_count(), 1);
    assert_eq!(r.rows().len(), 1);
    assert!(r.main().get_can_undo());
    assert!(r.key("z", false, true), "Ctrl+Z undoes");
    r.pump();
    assert_eq!(r.vm.sentence_count(), 3);
}

#[test]
fn windows_keys_toggle_explorer_settings_and_full_screen() {
    let r = rig(false);
    assert!(r.key("b", false, true));
    r.pump();
    assert_eq!(r.env.settings.get_bool("showFileExplorer"), Some(false));
    assert!(!r.main().get_show_file_explorer());
    assert!(r.key(",", false, true));
    assert_eq!(r.settings_opened.get(), 1);
    // F11 toggles full screen at any focus; Escape leaves it.
    assert!(r.key("\u{F70E}", false, false));
    assert!(r.controller.is_full_screen());
    assert!(r.window.global::<AppBridge>().get_is_full_screen());
    assert_eq!(r.main().get_layout_mode(), 0, "full screen shows the player only");
    assert!(r.key("\u{1b}", false, false));
    assert!(!r.controller.is_full_screen());
    assert_eq!(*r.full_screen_calls.borrow(), vec![true, false]);
    // Escape with nothing to leave is not consumed.
    assert!(!r.key("\u{1b}", false, false));
}

#[test]
fn player_keys_need_a_video_on_screen_and_e_toggles_the_panel() {
    let r = rig(false);
    r.open_video("a.mp4");
    assert!(r.main().get_has_video());
    assert_eq!(r.window.global::<AppBridge>().get_window_title(), "a.mp4");
    assert_eq!(r.window.global::<AppBridge>().get_title_file(), "— a.mp4");
    assert_eq!(r.backend.lock().loads.last().cloned().flatten().map(|p| p.file_name().unwrap().to_owned()), Some("a.mp4".into()));
    assert!(r.key("e", false, false));
    r.pump();
    assert!(r.vm.hide_subtitle_panel());
    assert_eq!(r.main().get_layout_mode(), 0);
    assert!(r.key("E", true, false));
    r.pump();
    assert!(!r.vm.hide_subtitle_panel());
    // While a text field is edited the letters are typing.
    r.main().invoke_editing_changed(true);
    assert!(!r.key("e", false, false));
    r.main().invoke_editing_changed(false);
    // Space toggles playback (before the media opened it flips auto play).
    assert!(r.key(" ", false, false));
    assert!(r.controller.player().auto_play_when_ready() != r.vm.auto_play_video());
}

#[test]
fn side_by_side_and_hidden_panel_drive_the_layout_mode() {
    let r = rig(false);
    r.open_video("a.mp4");
    let epoch = r.main().get_layout_epoch();
    r.main().invoke_toggle_side_by_side();
    r.pump();
    assert!(r.vm.side_by_side_layout());
    assert_eq!(r.main().get_layout_mode(), 1);
    assert!(r.main().get_layout_epoch() > epoch, "re-applying resets the splitter");
    r.main().invoke_toggle_subtitle_panel();
    r.pump();
    assert_eq!(r.main().get_layout_mode(), 0);
    r.main().invoke_toggle_theater();
    r.pump();
    assert!(r.window.global::<AppBridge>().get_is_theater_mode());
    assert!(r.key("\u{1b}", false, false), "Esc leaves theater mode");
    r.pump();
    assert!(!r.vm.is_theater_mode());
}

#[test]
fn several_dropped_videos_go_to_the_queue_and_one_opens_with_its_subtitle() {
    let r = rig(false);
    let a = r.env.touch("v/a.mp4", "x");
    let b = r.env.touch("v/b.mkv", "x");
    r.controller.handle_drop(vec![a.clone(), b.clone()]);
    r.pump();
    assert_eq!(r.vm.workspace(), Workspace::Playlist);
    assert_eq!(r.main().get_workspace(), ui::Workspace::Playlist);
    assert_eq!(r.main().get_queue_rows().row_count(), 2);
    assert_eq!(r.main().get_playlist_count(), 2);
    assert!(r.main().get_has_playlist_items());

    let r = rig(false);
    let v = r.env.touch("w/c.mp4", "x");
    let s = r.srt("w/c.srt", &[("00:00:00,000", "00:00:01,000", "Merhaba.")]);
    r.vm.set_workspace(Workspace::Platform);
    r.pump();
    r.controller.handle_drop(vec![s, v.clone()]);
    r.pump();
    assert_eq!(r.vm.video().as_deref(), Some(v.as_path()));
    assert_eq!(r.vm.sentence_count(), 1);
    // FIX-W: a single dropped video switches to the Player tab.
    assert_eq!(r.vm.workspace(), Workspace::Editor);
}

#[test]
fn menus_are_built_on_opening_and_their_ids_act() {
    let r = rig(false);
    r.main().invoke_menu_requested("speakers".into());
    let entries = r.main().get_menu_entries();
    assert!(entries.row_count() >= 8);
    let first = entries.row_data(0).unwrap();
    assert_eq!(first.title, "Konuşmacıları ayır");
    let diarize = r.vm.diarize();
    r.main().invoke_menu_activated("diarize".into());
    r.pump();
    assert_eq!(r.vm.diarize(), !diarize);
    r.main().invoke_menu_activated("speakers:3".into());
    r.pump();
    assert_eq!(r.vm.speaker_count(), 3);

    r.main().invoke_menu_requested("model".into());
    let entries = r.main().get_menu_entries();
    let ids: Vec<String> = (0..entries.row_count()).filter_map(|i| entries.row_data(i)).map(|e| e.id.to_string()).filter(|i| !i.is_empty()).collect();
    assert!(!ids.is_empty());
    let other = ids.iter().find(|i| **i != format!("model:{}", r.vm.model())).cloned().unwrap();
    r.main().invoke_menu_activated(other.clone().into());
    r.pump();
    assert_eq!(format!("model:{}", r.vm.model()), other);
    assert_eq!(r.main().get_model_title(), sa_core::models::ModelLabel::name(&r.vm.model()));
}

#[test]
fn row_menu_speaker_items_set_the_line_speaker() {
    let r = rig(false);
    r.load_three_lines();
    r.main().invoke_menu_requested("row:1".into());
    let entries = r.main().get_menu_entries();
    let ids: Vec<String> = (0..entries.row_count()).filter_map(|i| entries.row_data(i)).map(|e| e.id.to_string()).collect();
    assert_eq!(ids.first().map(String::as_str), Some("row-retranslate:1"));
    assert!(ids.contains(&"row-delete:1".to_owned()));
    r.main().invoke_menu_activated("spk-new:1".into());
    r.pump();
    assert!(r.vm.sentences()[1].speaker.is_some());
    assert!(r.main().get_shows_speakers());
    assert_eq!(r.rows()[1].speaker_text, "K1");
    r.main().invoke_menu_activated("spk:1:none".into());
    r.pump();
    assert!(r.vm.sentences()[1].speaker.is_none());
}

#[test]
fn a_playback_failure_opens_the_error_bar_until_it_is_closed() {
    let r = rig(false);
    r.open_video("broken.mp4");
    let (sink, generation) = {
        let s = r.backend.lock();
        (s.sink.clone().expect("sink"), s.generation)
    };
    sink(PlayerEvent::Failed { generation, message: "MF_E_UNSUPPORTED (0xC00D36C4)".into() });
    r.pump();
    assert!(r.main().get_error_open());
    assert_eq!(r.main().get_error_text(), "Video oynatılamadı: MF_E_UNSUPPORTED (0xC00D36C4)");
    assert!(!r.main().get_go_online_visible());
    r.main().invoke_error_closed();
    assert!(!r.main().get_error_open());
    // A stale load's failure is ignored.
    sink(PlayerEvent::Failed { generation: generation - 1, message: "old".into() });
    r.pump();
    assert!(!r.main().get_error_open());
}

#[test]
fn display_picker_falls_back_to_the_original_without_translations() {
    let r = rig(false);
    r.load_three_lines();
    r.main().invoke_display_mode_selected(1);
    r.pump();
    assert_eq!(r.main().get_display_mode(), 0);
    assert!(!r.main().get_has_translations());
}

#[test]
fn the_window_width_reaches_the_responsive_rules() {
    let r = rig(false);
    r.controller.window_resized(900.0, 700.0);
    assert_eq!(r.main().get_window_width(), 900.0);
}

// ------------------------------------------------------------------------------------------------ macOS

#[test]
fn mac_menu_state_follows_the_workspace_and_the_video() {
    let r = rig(true);
    let m = r.window.global::<MenuBridge>();
    assert!(m.get_can_open_video());
    assert!(!m.get_can_generate(), "no video");
    r.open_video("a.mp4");
    let m = r.window.global::<MenuBridge>();
    assert!(m.get_can_generate());
    assert!(m.get_can_toggle_panel());
    r.vm.set_workspace(Workspace::Playlist);
    r.pump();
    let m = r.window.global::<MenuBridge>();
    assert!(!m.get_can_open_video());
    assert!(!m.get_can_generate());
    assert!(!r.main().get_can_pick_video());
    assert_eq!(m.get_undo_title(), "Geri Al");
}

#[test]
fn mac_keys_use_command_and_esc_leaves_theater_before_anything_else() {
    let r = rig(true);
    r.open_video("a.mp4");
    r.vm.set_theater_mode(true);
    r.pump();
    // Slint `control` = ⌘ on macOS: ⌘, opens Settings.
    assert!(r.key(",", false, true));
    assert_eq!(r.settings_opened.get(), 1);
    assert!(r.key("\u{1b}", false, false));
    r.pump();
    assert!(!r.vm.is_theater_mode());
    assert!(!r.key("\u{1b}", false, false), "nothing left to leave: passed on");
    // ⌃⌘S toggles the sidebar.
    let before = r.main().get_show_file_explorer();
    assert!(r.main().invoke_key_pressed("s".into(), false, true, false, true, false));
    r.pump();
    assert_ne!(r.main().get_show_file_explorer(), before);
    // Plain letters are player keys; a chord with ⌘ is not.
    assert!(r.key("e", false, false));
    r.pump();
    assert!(r.vm.hide_subtitle_panel());
    // ⌘M belongs to the Window menu (minimize), never to the player.
    assert!(!r.key("m", false, true));
}

#[test]
fn mac_a_subtitle_without_video_shows_the_line_panel_alone() {
    let r = rig(true);
    r.load_three_lines();
    assert!(!r.main().get_has_video());
    assert_eq!(r.main().get_layout_mode(), 3);
    assert_eq!(r.rows()[0].speaker_text, "Konuşmacı yok");
}

#[test]
fn mac_full_screen_hides_the_panel_without_changing_the_setting() {
    let r = rig(true);
    r.open_video("a.mp4");
    assert!(r.key("f", false, false));
    assert!(r.controller.is_full_screen());
    assert_eq!(r.main().get_layout_mode(), 0);
    assert!(!r.vm.hide_subtitle_panel());
    // E does nothing in full screen (the setting stays).
    assert!(r.key("e", false, false));
    r.pump();
    assert!(!r.vm.hide_subtitle_panel());
    assert!(r.key("\u{1b}", false, false));
    assert!(!r.controller.is_full_screen());
    assert_eq!(r.main().get_layout_mode(), 2);
}
