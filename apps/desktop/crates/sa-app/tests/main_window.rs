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

use sa_app::main_window::{MainWindowController, MainWindowDeps, MainWindowHooks, SubtitleReader};
use sa_app::ui::{self, AppBridge, MainBridge, MainWindow, MenuBridge};
use sa_core::dispatch::UiDispatcher;
use sa_core::l10n::{CatalogChain, UiLanguage};
use sa_core::models::ModelCatalog;
use sa_core::platform::PlatformShell;
use sa_core::settings::SettingsStore;
use sa_player::{AudioTrack, LoadGeneration, MediaPlayerBackend, MediaSource, MediaTracks, PlayerEvent, SubtitleKind, SubtitleTrack, VideoFrame};
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
    tracks: MediaTracks,
    selected: Vec<usize>,
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
    fn tracks(&self) -> MediaTracks {
        self.0.lock().tracks.clone()
    }
    fn select_audio_track(&mut self, stream_index: usize) {
        self.0.lock().selected.push(stream_index);
    }
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
    rig_with(mac, None)
}

fn rig_with(mac: bool, subtitle_reader: Option<SubtitleReader>) -> Rig {
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
        subtitle_reader,
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
fn the_software_renderer_never_gets_a_transparent_mica_background() {
    // The renderer that draws the window decides, not SLINT_BACKEND: the software renderer has no per-pixel alpha.
    let r = rig(false);
    r.window.show().unwrap();
    assert!(!sa_app::main_window::chrome::mica_renderer(r.window.window(), None));
    assert!(!sa_app::main_window::chrome::mica_renderer(r.window.window(), None), "asking twice gives the same answer");
    // A window whose notifier is already installed passes what installing it said (asking again would replace it).
    assert!(sa_app::main_window::chrome::mica_renderer(r.window.window(), Some(true)));
}

#[test]
fn without_a_gpu_renderer_the_video_surface_stays_off() {
    // The testing backend has no rendering notifier: the controller keeps the RGBA path.
    let r = rig(false);
    let surface = sa_app::main_window::video_gl::VideoSurface::for_main_window(&r.window);
    assert!(!surface.installed());
    assert!(!surface.available());
    assert_eq!(surface.uploads(), 0);
}

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
fn windows_e_still_toggles_the_panel_setting_in_full_screen() {
    // MainWindow.ToggleSubtitlePanel() has no full-screen guard (unlike macOS); the layout stays player-only.
    let r = rig(false);
    r.open_video("a.mp4");
    assert!(r.key("f", false, false));
    assert!(r.controller.is_full_screen());
    assert!(r.key("e", false, false));
    r.pump();
    assert!(r.vm.hide_subtitle_panel());
    assert_eq!(r.main().get_layout_mode(), 0);
    assert!(r.key("\u{1b}", false, false));
    assert!(!r.controller.is_full_screen());
    assert_eq!(r.main().get_layout_mode(), 0, "the panel stays hidden after leaving full screen");
}

#[test]
fn windows_space_in_the_focused_list_goes_to_the_list_handler() {
    let r = rig(false);
    r.open_video("a.mp4");
    r.load_three_lines();
    r.main().invoke_list_focus_changed(true);
    let before = r.controller.player().auto_play_when_ready();
    // Root_PreviewKeyDown skips Space on a ListView(Item); Lines_PreviewKeyDown toggles playback.
    assert!(!r.key(" ", false, false));
    assert!(r.main().invoke_list_key("space".into(), false));
    assert_ne!(r.controller.player().auto_play_when_ready(), before);
    r.main().invoke_list_focus_changed(false);
    assert!(r.key(" ", false, false));
    assert_eq!(r.controller.player().auto_play_when_ready(), before);
}

#[test]
fn windows_zero_wheel_delta_keeps_the_volume() {
    let r = rig(false);
    r.open_video("a.mp4");
    let before = r.controller.player().volume();
    r.main().invoke_video_wheel(0.0, false);
    r.main().invoke_volume_wheel(0.0);
    assert_eq!(r.controller.player().volume(), before);
    r.main().invoke_video_wheel(-120.0, false);
    assert!((r.controller.player().volume() - (before - 0.05)).abs() < 1e-9);
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
fn several_dropped_videos_go_to_the_queue() {
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
}

#[test]
fn one_dropped_video_opens_with_its_subtitle_on_the_player_tab() {
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
fn a_single_diarized_speaker_still_shows_the_speaker_column() {
    // C# `ShowsSpeakers => Speakers.Count > 0` with `CompactSpeakerTitle` "K1"; Swift `showsSpeakers =
    // !speakers.isEmpty` with "Konuşmacı 1": one speaker is enough (more can be added), only the dash rule needs two.
    for mac in [false, true] {
        let r = rig(mac);
        let json = r.env.touch(
            "one-speaker.json",
            r#"[{"id":1,"start":0,"end":2,"text":"Bir.","speaker":0,"words":[]},{"id":2,"start":2,"end":4,"text":"İki.","speaker":0,"words":[]}]"#,
        );
        assert!(r.vm.load_subtitle_file(&json));
        r.pump();
        assert!(r.main().get_shows_speakers(), "mac={mac}");
        let expected = if mac { "Konuşmacı 1" } else { "K1" };
        assert!(r.rows().iter().all(|row| row.has_speaker && row.speaker_text == expected), "mac={mac}");
    }
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

// ------------------------------------------------------------------------------------------------ row sync cost

fn many_lines(r: &Rig, name: &str, n: usize) {
    let mut body = String::new();
    for i in 0..n {
        let (a, b) = (i * 2, i * 2 + 2);
        body.push_str(&format!("{}\n00:{:02}:{:02},000 --> 00:{:02}:{:02},000\nSatır {i}.\n\n", i + 1, a / 60, a % 60, b / 60, b % 60));
    }
    let p = r.env.touch(name, &body);
    assert!(r.vm.load_subtitle_file(&p));
    r.pump();
    assert_eq!(r.rows().len(), n);
}

#[test]
fn an_active_line_change_rewrites_only_the_two_rows_whose_mark_flips() {
    let r = rig(false);
    many_lines(&r, "long.srt", 40);
    let uids: Vec<_> = r.vm.sentences().iter().map(|s| s.uid).collect();
    assert_eq!(r.controller.player().active_id(), Some(uids[0]), "the line at 0 s plays first");
    assert!(r.rows()[0].is_active);
    let before = r.controller.row_sync_counters();
    // Selecting a line seeks the player to its start: the selection rebuilds the rows once, the active-line change
    // that follows flips two rows and rebuilds nothing.
    r.main().invoke_row_pressed(5, false, false);
    r.pump();
    assert_eq!(r.controller.player().active_id(), Some(uids[5]));
    let after = r.controller.row_sync_counters();
    assert_eq!(after.full_syncs - before.full_syncs, 1, "only the selection change rebuilds");
    assert_eq!(after.active_row_updates - before.active_row_updates, 2, "old and new active rows only");
    let fast = r.rows();
    assert!(fast[5].is_active && !fast[0].is_active);
    assert_eq!(fast.iter().filter(|row| row.is_active).count(), 1);
    // Identical to what a full rebuild produces.
    r.controller.refresh_all(false);
    assert_eq!(r.rows(), fast);
}

#[test]
fn rebuilding_the_rows_costs_the_same_view_model_reads_for_any_line_count() {
    let r = rig(false);
    many_lines(&r, "short.srt", 5);
    let start = r.vm.read_count();
    r.controller.refresh_all(false);
    let small = r.vm.read_count() - start;
    many_lines(&r, "long.srt", 300);
    let start = r.vm.read_count();
    r.controller.refresh_all(false);
    let large = r.vm.read_count() - start;
    assert_eq!(
        small, large,
        "view-model reads across the whole of refresh_all (5 lines: {small}, 300 lines: {large}) must not grow with the line count"
    );
}

// ------------------------------------------------------------------------------------------------ active-row mark

/// Shift+Right: the player seeks 10 s forward (needs a loaded video).
const RIGHT: &str = "\u{F703}";

/// Exactly the playing line is marked, and nothing else (no stale mark).
fn assert_mark(r: &Rig, what: &str) -> Option<uuid::Uuid> {
    let active = r.controller.player().active_id();
    let marked: Vec<String> = r.rows().iter().filter(|row| row.is_active).map(|row| row.uid.to_string()).collect();
    let expected: Vec<String> = active.iter().map(|u| u.to_string()).collect();
    assert_eq!(marked, expected, "{what}");
    active
}

#[test]
fn the_active_mark_follows_inserts_deletes_undo_and_video_switches() {
    let r = rig(false);
    r.open_video("a.mp4");
    many_lines(&r, "a.srt", 40);
    r.main().invoke_row_pressed(3, false, false);
    r.pump();
    assert_eq!(assert_mark(&r, "selected line plays"), Some(r.vm.sentences()[3].uid));
    assert!(r.key(RIGHT, true, false));
    r.pump();
    assert_eq!(assert_mark(&r, "seeked ahead"), Some(r.vm.sentences()[8].uid));
    // Delete the selected line (before the playing one).
    r.main().invoke_row_pressed(3, false, false);
    assert!(r.key(RIGHT, true, false));
    r.pump();
    assert!(assert_mark(&r, "line 3 selected, a later line plays").is_some());
    assert!(r.main().invoke_list_key("delete".into(), false));
    r.pump();
    assert_eq!(r.rows().len(), 39);
    assert_mark(&r, "after deleting a line before the playing one");
    assert!(r.key(RIGHT, true, false));
    r.pump();
    assert!(assert_mark(&r, "seeked after the delete").is_some());
    // Insert one (split a line before the playing one).
    r.main().invoke_row_pressed(1, false, false);
    r.pump();
    let start = r.vm.sentences()[1].start;
    r.vm.split_selected(start + 1.0);
    r.pump();
    assert_eq!(r.rows().len(), 40);
    assert_mark(&r, "after inserting a line before the playing one");
    assert!(r.key(RIGHT, true, false));
    r.pump();
    assert!(assert_mark(&r, "seeked after the insert").is_some());
    // Undo both, redo one.
    assert!(r.key("z", false, true));
    r.pump();
    assert_mark(&r, "undo the insert");
    assert!(r.key("z", false, true));
    r.pump();
    assert_eq!(r.rows().len(), 40);
    assert_mark(&r, "undo the delete");
    assert!(r.key("y", false, true));
    r.pump();
    assert_mark(&r, "redo the delete");
    // A video switch reloads the player (and its lines).
    r.open_video("b.mp4");
    assert_mark(&r, "after a video switch");
    assert!(r.key(RIGHT, true, false));
    r.pump();
    assert_mark(&r, "seeked in the second video");
}

#[test]
fn the_active_mark_follows_live_partial_snapshots_that_replace_the_rows() {
    use sa_core::engine_types::Sentence;
    use sa_enginekit::events::EngineEvent;
    let r = rig(false);
    r.open_video("talk.mp4");
    r.vm.generate();
    let call = r.env.next(common::CallKind::Transcribe);
    let lines = |n: usize, word: &str| -> Vec<Sentence> {
        (0..n).map(|i| Sentence::new(i as i64 + 1, i as f64 * 2.0, i as f64 * 2.0 + 2.0, &format!("{word} {i}."))).collect()
    };
    call.send(EngineEvent::Partial { language: "tr".into(), sentences: lines(3, "Bir") });
    r.env.eventually("first snapshot", || r.rows().len() == 3);
    assert_eq!(assert_mark(&r, "first snapshot"), Some(r.vm.sentences()[0].uid));
    // A later snapshot replaces the rows (new text, more lines) while line 0 plays.
    call.send(EngineEvent::Partial { language: "tr".into(), sentences: lines(6, "İki") });
    r.env.eventually("second snapshot", || r.rows().len() == 6);
    assert!(assert_mark(&r, "second snapshot").is_some());
    assert!(r.key(RIGHT, true, false));
    r.pump();
    assert_eq!(assert_mark(&r, "seeked into the snapshot"), Some(r.vm.sentences()[5].uid));
    // A snapshot that no longer reaches the playing time: no line is marked.
    call.send(EngineEvent::Partial { language: "tr".into(), sentences: lines(2, "Üç") });
    r.env.eventually("shorter snapshot", || r.rows().len() == 2);
    assert_eq!(assert_mark(&r, "shorter snapshot"), None);
    call.result("tr", "", lines(6, "Son"));
    r.env.eventually("result", || r.rows().len() == 6 && r.rows()[0].text.starts_with("Son"));
    assert_eq!(assert_mark(&r, "final result"), Some(r.vm.sentences()[5].uid));
}

// ------------------------------------------------------------------------------------------------ player tracks

fn sample_tracks() -> MediaTracks {
    let audio = |i: usize, lang: &str, title: Option<&str>, ch: u16| AudioTrack {
        stream_index: i,
        language: Some(lang.into()),
        title: title.map(Into::into),
        codec: "aac".into(),
        channels: ch,
        ..Default::default()
    };
    let sub = |i: usize, codec: &str, kind: SubtitleKind| SubtitleTrack {
        stream_index: i,
        language: Some("tur".into()),
        title: None,
        codec: codec.into(),
        kind,
        default: false,
        forced: false,
    };
    MediaTracks {
        audio: vec![audio(1, "eng", None, 2), audio(2, "tur", Some("Yorum"), 1)],
        subtitles: vec![sub(3, "subrip", SubtitleKind::Text), sub(4, "hdmv_pgs_subtitle", SubtitleKind::Image)],
        active_audio: Some(1),
    }
}

/// A reader that answers every text track with one line after reporting half way; records what it was asked.
fn fake_reader(asked: Arc<Mutex<Vec<(PathBuf, usize)>>>) -> SubtitleReader {
    Arc::new(move |path: &std::path::Path, stream: usize, _cancel: &sa_core::cancel::CancellationToken, progress: &mut dyn FnMut(f64)| {
        asked.lock().push((path.to_path_buf(), stream));
        progress(0.5);
        Ok(embedded_doc(stream))
    })
}

/// What `extract_text_subtitles` hands over for `stream`: one line as the app's JSON sentence list.
fn embedded_doc(stream: usize) -> sa_player::ffmpeg_backend::ExtractedSubtitle {
    let text = format!(r#"[{{"id":1,"start":1.0,"end":2.5,"text":"Gömülü parça {stream}","words":[]}}]"#);
    sa_player::ffmpeg_backend::ExtractedSubtitle { text, extension: "json", events: 1 }
}

/// A reader that blocks until the test sends on the returned channel, then answers like [`fake_reader`].
fn gated_reader() -> (SubtitleReader, crossbeam_channel::Sender<()>) {
    let (tx, rx) = crossbeam_channel::bounded::<()>(0);
    let rx = Arc::new(Mutex::new(rx));
    let reader: SubtitleReader = Arc::new(move |_p: &std::path::Path, stream: usize, _c: &sa_core::cancel::CancellationToken, _f: &mut dyn FnMut(f64)| {
        let _ = rx.lock().recv_timeout(std::time::Duration::from_secs(5));
        Ok(embedded_doc(stream))
    });
    (reader, tx)
}

impl Rig {
    /// Opens `rel` and lets the fake backend report it opened with `tracks`.
    fn open_with_tracks(&self, rel: &str, tracks: MediaTracks) -> PathBuf {
        let video = self.open_video(rel);
        let (sink, generation) = {
            let mut s = self.backend.lock();
            s.tracks = tracks;
            (s.sink.clone().expect("sink"), s.generation)
        };
        sink(PlayerEvent::Opened { generation, duration: 10.0, natural_size: Some((320, 240)) });
        sink(PlayerEvent::TracksChanged { generation });
        self.pump();
        video
    }
    fn menu(&self) -> Vec<ui::MenuEntry> {
        let m = self.main().get_menu_entries();
        (0..m.row_count()).filter_map(|i| m.row_data(i)).collect()
    }
}

#[test]
fn the_track_menu_lists_both_sections_and_switches_the_audio_track() {
    let r = rig(false);
    assert!(!r.main().get_tracks_available());
    r.open_with_tracks("film.mkv", sample_tracks());
    assert!(r.main().get_tracks_available());
    r.main().invoke_menu_requested("tracks".into());
    let menu = r.menu();
    let titles: Vec<String> = menu.iter().map(|e| e.title.to_string()).collect();
    assert_eq!(titles, vec!["Ses", "İngilizce (AAC, stereo)", "Türkçe — Yorum (AAC, mono)", "", "Videodaki altyazılar", "Türkçe (SRT)", "Türkçe (PGS)"]);
    assert!(menu[0].header && menu[4].header && menu[3].separator);
    assert!(menu[1].checked && menu[1].radio && !menu[2].checked);
    assert!(menu[5].enabled && !menu[6].enabled);
    assert_eq!(menu[6].hint, "Görüntü tabanlı, desteklenmiyor");
    r.main().invoke_menu_activated("audio:2".into());
    r.pump();
    assert_eq!(r.backend.lock().selected, vec![2]);
    // The menu shown follows the switch the player reports.
    r.main().invoke_tracks_menu_open(true);
    r.backend.lock().tracks.active_audio = Some(2);
    let (sink, generation) = {
        let s = r.backend.lock();
        (s.sink.clone().unwrap(), s.generation)
    };
    sink(PlayerEvent::TracksChanged { generation });
    r.pump();
    let menu = r.menu();
    assert!(!menu[1].checked && menu[2].checked, "the open menu marks the new track");
    r.main().invoke_tracks_menu_open(false);
    // A failed switch is reported in the error bar.
    sink(PlayerEvent::AudioTrackFailed { generation, stream_index: 2, message: "Decoder not found".into() });
    r.pump();
    assert!(r.main().get_error_open());
    assert_eq!(r.main().get_error_text(), "Ses parçası açılamadı: Decoder not found");
    // Another video without a choice hides the control.
    r.open_with_tracks("plain.mp4", MediaTracks { audio: vec![sample_tracks().audio[0].clone()], subtitles: vec![], active_audio: Some(1) });
    assert!(!r.main().get_tracks_available());
}

#[test]
fn choosing_an_embedded_text_track_imports_it_like_a_subtitle_file() {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let r = rig_with(false, Some(fake_reader(asked.clone())));
    let video = r.open_with_tracks("film.mkv", sample_tracks());
    r.main().invoke_menu_activated("subtitle:4".into());
    r.pump();
    assert!(asked.lock().is_empty(), "image tracks are not read");
    r.main().invoke_menu_activated("subtitle:3".into());
    r.env.eventually("the track's lines", || r.vm.sentences().len() == 1);
    assert_eq!(asked.lock().clone(), vec![(video.clone(), 3)]);
    let line = &r.vm.sentences()[0];
    assert_eq!((line.start, line.end, line.text.as_str()), (1.0, 2.5, "Gömülü parça 3"));
    assert_eq!(r.vm.video(), Some(video.clone()), "the video stays open");
    let loaded = r.vm.loaded_subtitle().expect("imported from a file");
    assert!(loaded.starts_with(r.env.shell.engine_work_dir()), "{}", loaded.display());
    assert!(!r.main().get_tracks_busy());
    r.main().invoke_menu_requested("tracks".into());
    let menu = r.menu();
    let sub = menu.iter().find(|e| e.id == "subtitle:3").unwrap();
    assert!(sub.checked, "the track in the list is checked");
    assert!(!sub.radio);
    // Choosing the track that is already in the list does nothing (edits stay).
    r.vm.set_text(r.vm.sentences()[0].uid, "Düzeltildi");
    assert!(r.vm.has_unsaved_edits());
    r.main().invoke_menu_activated("subtitle:3".into());
    r.pump();
    assert_eq!(asked.lock().len(), 1, "not read again");
    assert!(!r.main().get_tracks_busy());
    assert_eq!(r.vm.sentences()[0].text, "Düzeltildi");
    assert!(r.vm.pending_open().is_none());
    // The file is deleted with the session's other scratch files on exit.
    assert!(loaded.exists());
    r.vm.shutdown();
    assert!(!loaded.exists(), "{}", loaded.display());
}

/// M1: a read that ends after the user edited, or after a job started, does not replace the lines silently.
#[test]
fn a_read_that_ends_after_the_session_changed_is_not_imported_silently() {
    let (reader, release) = gated_reader();
    let r = rig_with(false, Some(reader));
    r.open_with_tracks("film.mkv", sample_tracks());
    r.load_three_lines();
    r.main().invoke_menu_activated("subtitle:3".into());
    r.pump();
    assert!(r.main().get_tracks_busy());
    // Edited during the read.
    r.vm.set_text(r.vm.sentences()[0].uid, "Okurken düzeltildi");
    release.send(()).unwrap();
    r.env.eventually("the read ended", || {
        r.pump();
        !r.main().get_tracks_busy()
    });
    assert_eq!(r.vm.sentences().len(), 3, "the lines were kept");
    assert_eq!(r.vm.sentences()[0].text, "Okurken düzeltildi");
    assert!(r.main().get_error_open());
    assert_eq!(r.main().get_error_text(), "Videodaki altyazı yüklenmedi: okunurken altyazı değişti. Parçayı yeniden seç.");
    // Chosen again with nothing changing meanwhile, it is imported.
    r.main().invoke_menu_activated("subtitle:3".into());
    r.pump();
    release.send(()).unwrap();
    r.env.eventually("imported", || r.vm.sentences().len() == 1);
    assert_eq!(r.vm.sentences()[0].text, "Gömülü parça 3");
}

#[test]
fn a_read_that_ends_after_a_job_started_is_not_imported_silently() {
    let (reader, release) = gated_reader();
    let r = rig_with(false, Some(reader));
    r.open_with_tracks("film.mkv", sample_tracks());
    r.main().invoke_menu_activated("subtitle:3".into());
    r.pump();
    r.vm.generate();
    let _call = r.env.next(common::CallKind::Transcribe);
    assert!(r.vm.is_working());
    release.send(()).unwrap();
    r.env.eventually("the read ended", || {
        r.pump();
        !r.main().get_tracks_busy()
    });
    assert!(r.vm.is_working(), "the job was not stopped");
    assert!(r.vm.sentences().is_empty());
    assert!(r.main().get_error_open());
}

/// L2: the track menu refills the shared menu entries only while they are its own, and another video closes it.
#[test]
fn track_changes_never_overwrite_another_menu() {
    let r = rig(false);
    r.open_with_tracks("film.mkv", sample_tracks());
    r.main().invoke_menu_requested("tracks".into());
    r.main().invoke_tracks_menu_open(true);
    // Another menu opens while the track menu's close was never reported.
    r.main().invoke_menu_requested("speakers".into());
    let speakers = r.menu();
    let (sink, generation) = {
        let s = r.backend.lock();
        (s.sink.clone().unwrap(), s.generation)
    };
    sink(PlayerEvent::TracksChanged { generation });
    r.pump();
    assert_eq!(r.menu(), speakers, "the speakers menu stays");
    // A new video (the button goes away with its popup) resets the open flag: its tracks do not fill any menu.
    r.main().invoke_menu_requested("tracks".into());
    r.open_with_tracks("other.mkv", sample_tracks());
    r.main().invoke_menu_requested("speakers".into());
    let speakers = r.menu();
    let generation = r.backend.lock().generation;
    sink(PlayerEvent::TracksChanged { generation });
    r.pump();
    assert_eq!(r.menu(), speakers);
}

/// L10: on macOS a track failure is a dismissable notice; the session's status stays.
#[test]
fn on_macos_a_track_failure_is_a_notice_and_keeps_the_status() {
    let r = rig(true);
    r.open_with_tracks("film.mkv", sample_tracks());
    let status = r.main().get_status_text();
    let (sink, generation) = {
        let s = r.backend.lock();
        (s.sink.clone().unwrap(), s.generation)
    };
    sink(PlayerEvent::AudioTrackFailed { generation, stream_index: 2, message: String::new() });
    r.pump();
    assert_eq!(r.main().get_playback_notice(), "Ses parçası açılamadı: Bilinmeyen hata");
    assert_eq!(r.main().get_status_text(), status);
    r.main().invoke_dismiss_playback_notice();
    r.pump();
    assert_eq!(r.main().get_playback_notice(), "");
}

#[test]
fn on_macos_an_embedded_track_asks_first_when_edits_would_be_lost() {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let r = rig_with(true, Some(fake_reader(asked.clone())));
    r.open_with_tracks("film.mkv", sample_tracks());
    r.load_three_lines();
    r.vm.set_text(r.vm.sentences()[0].uid, "Değişti");
    r.main().invoke_menu_activated("subtitle:3".into());
    r.env.eventually("the confirmation", || r.vm.pending_open().is_some());
    assert!(r.vm.pending_open().unwrap().loses_edits);
    assert_eq!(r.vm.sentences()[0].text, "Değişti", "nothing replaced yet");
    r.vm.confirm_pending_open();
    r.pump();
    assert_eq!(r.vm.sentences().len(), 1);
    assert_eq!(r.vm.sentences()[0].text, "Gömülü parça 3");
}

#[test]
fn opening_another_video_drops_a_track_still_being_read() {
    let (tx, rx) = crossbeam_channel::bounded::<()>(0);
    let rx = Arc::new(Mutex::new(rx));
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let seen = cancelled.clone();
    let reader: SubtitleReader = Arc::new(move |_p: &std::path::Path, _s: usize, cancel: &sa_core::cancel::CancellationToken, _progress: &mut dyn FnMut(f64)| {
        // Blocks until the test lets it go, then reports how it ended.
        let _ = rx.lock().recv_timeout(std::time::Duration::from_secs(5));
        seen.store(cancel.is_cancelled(), std::sync::atomic::Ordering::SeqCst);
        Err(sa_player::ffmpeg_backend::ExtractError::Cancelled)
    });
    let r = rig_with(false, Some(reader));
    r.open_with_tracks("film.mkv", sample_tracks());
    r.main().invoke_menu_activated("subtitle:3".into());
    r.pump();
    assert!(r.main().get_tracks_busy());
    r.open_video("other.mp4");
    assert!(!r.main().get_tracks_busy(), "the read is dropped with the video");
    let _ = tx.send(());
    r.env.eventually("the reader saw the cancel", || cancelled.load(std::sync::atomic::Ordering::SeqCst));
    r.pump();
    assert!(r.vm.sentences().is_empty());
    assert!(!r.main().get_error_open());
}
