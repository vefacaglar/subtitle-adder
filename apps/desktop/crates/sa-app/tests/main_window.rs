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
use sa_core::engine_types::Sentence;
use sa_core::l10n::{CatalogChain, UiLanguage};
use sa_core::models::ModelCatalog;
use sa_core::settings::SettingsStore;
use sa_player::{
    AudioTrack, LoadGeneration, MediaPlayerBackend, MediaSource, MediaTracks, PictureTrackFailure, PlayerEvent, SubtitleBitmap, SubtitleKind,
    SubtitlePicture, SubtitleTrack, VideoFrame,
};
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
    /// Every `select_image_subtitle_track` call.
    images: Vec<Option<usize>>,
    picture: Option<Arc<SubtitlePicture>>,
    position: f64,
}

struct FakeBackend(Arc<Mutex<BackendState>>);

impl MediaPlayerBackend for FakeBackend {
    fn load(&mut self, source: Option<MediaSource>, _auto_play: bool) -> LoadGeneration {
        let mut s = self.0.lock();
        s.generation += 1;
        // A new load shows no picture (as the real player).
        s.picture = None;
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
    fn seek(&mut self, seconds: f64) {
        self.0.lock().position = seconds;
    }
    fn set_volume(&mut self, _volume: f64) {}
    fn set_muted(&mut self, _muted: bool) {}
    fn position(&self) -> f64 {
        self.0.lock().position
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
    /// Shows an image track as the real player does: the active stream changes (a picture track only) and
    /// `TracksChanged` answers.
    fn select_image_subtitle_track(&mut self, stream_index: Option<usize>) {
        let (sink, generation) = {
            let mut s = self.0.lock();
            s.images.push(stream_index);
            if stream_index.is_none_or(|i| s.tracks.subtitles.iter().any(|t| t.stream_index == i && t.is_image())) {
                s.tracks.active_image_subtitle = stream_index;
            }
            (s.sink.clone(), s.generation)
        };
        if let Some(sink) = sink {
            sink(PlayerEvent::TracksChanged { generation });
        }
    }
    fn subtitle_picture(&self) -> Option<Arc<SubtitlePicture>> {
        self.0.lock().picture.clone()
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
    // The PGS track is Japanese: nothing in the UI language, so it is not shown for the user either.
    let pgs = SubtitleTrack { language: Some("jpn".into()), ..sub(4, "hdmv_pgs_subtitle", SubtitleKind::Image) };
    MediaTracks {
        audio: vec![audio(1, "eng", None, 2), audio(2, "tur", Some("Yorum"), 1)],
        subtitles: vec![sub(3, "subrip", SubtitleKind::Text), pgs],
        active_audio: Some(1),
        active_image_subtitle: None,
    }
}

/// [`sample_tracks`] with the text track in English and not the default: nothing is chosen for a Turkish UI.
fn manual_tracks() -> MediaTracks {
    let mut t = sample_tracks();
    t.subtitles[0].language = Some("eng".into());
    t
}

/// A reader that answers every text track with one line after reporting half way; records what it was asked.
fn fake_reader(asked: Arc<Mutex<Vec<(PathBuf, usize)>>>) -> SubtitleReader {
    Arc::new(
        move |path: &std::path::Path, stream: usize, _cancel: &sa_core::cancel::CancellationToken, progress: &mut dyn FnMut(f64), _snapshot: Option<&mut dyn FnMut(Vec<Sentence>)>| {
            asked.lock().push((path.to_path_buf(), stream));
            progress(0.5);
            Ok(embedded_doc(stream))
        },
    )
}

/// What `stream_text_subtitles` hands over for `stream`: one line.
fn embedded_doc(stream: usize) -> sa_player::ffmpeg_backend::ExtractedSubtitle {
    sa_player::ffmpeg_backend::ExtractedSubtitle { sentences: vec![Sentence::new(1, 1.0, 2.5, format!("Gömülü parça {stream}"))], events: 1 }
}

/// A reader that blocks until the test sends on the returned channel, then answers like [`fake_reader`].
fn gated_reader() -> (SubtitleReader, crossbeam_channel::Sender<()>) {
    let (tx, rx) = crossbeam_channel::bounded::<()>(0);
    let rx = Arc::new(Mutex::new(rx));
    let reader: SubtitleReader = Arc::new(
        move |_p: &std::path::Path, stream: usize, _c: &sa_core::cancel::CancellationToken, _f: &mut dyn FnMut(f64), _s: Option<&mut dyn FnMut(Vec<Sentence>)>| {
            let _ = rx.lock().recv_timeout(std::time::Duration::from_secs(5));
            Ok(embedded_doc(stream))
        },
    );
    (reader, tx)
}

/// What a [`scripted_reader`] does next.
enum Step {
    Snapshot(Vec<Sentence>),
    Finish(Vec<Sentence>),
    Fail(sa_player::ffmpeg_backend::ExtractError),
}

/// A reader driven by the test: each [`Step`] sent is a snapshot or the result. Records what it was asked and whether
/// it saw its cancel.
struct Script {
    reader: SubtitleReader,
    steps: crossbeam_channel::Sender<Step>,
    asked: Arc<Mutex<Vec<(PathBuf, usize)>>>,
    /// Per read: whether it was handed a snapshot callback (a progressive read).
    progressive: Arc<Mutex<Vec<bool>>>,
    cancelled: Arc<std::sync::atomic::AtomicUsize>,
}

fn scripted_reader() -> Script {
    let (tx, rx) = crossbeam_channel::unbounded::<Step>();
    let asked = Arc::new(Mutex::new(Vec::new()));
    let cancelled = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let progressive = Arc::new(Mutex::new(Vec::new()));
    let (a, c, p) = (asked.clone(), cancelled.clone(), progressive.clone());
    let reader: SubtitleReader = Arc::new(
        move |path: &std::path::Path, stream: usize, cancel: &sa_core::cancel::CancellationToken, _f: &mut dyn FnMut(f64), mut snapshot: Option<&mut dyn FnMut(Vec<Sentence>)>| {
            a.lock().push((path.to_path_buf(), stream));
            p.lock().push(snapshot.is_some());
            loop {
                if cancel.is_cancelled() {
                    c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    return Err(sa_player::ffmpeg_backend::ExtractError::Cancelled);
                }
                match rx.recv_timeout(std::time::Duration::from_millis(10)) {
                    // A read that does not fill the list as it goes gets no snapshot callback (none is built).
                    Ok(Step::Snapshot(lines)) => {
                        if let Some(snapshot) = snapshot.as_mut() {
                            snapshot(lines);
                        }
                    }
                    Ok(Step::Fail(e)) => return Err(e),
                    Ok(Step::Finish(lines)) => {
                        let events = lines.len();
                        return Ok(sa_player::ffmpeg_backend::ExtractedSubtitle { sentences: lines, events });
                    }
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                    Err(_) => return Err(sa_player::ffmpeg_backend::ExtractError::Cancelled),
                }
            }
        },
    );
    Script { reader, steps: tx, asked, progressive, cancelled }
}

/// `(start, end, text)` lines as the reader hands them over (ids from 1).
fn track(lines: &[(f64, f64, &str)]) -> Vec<Sentence> {
    lines.iter().enumerate().map(|(i, (a, b, t))| Sentence::new(i as i64 + 1, *a, *b, *t)).collect()
}

/// The two tracks of the user's films: SubRip `eng` (3) and `tur` (5), and a PGS one (4).
fn film_tracks() -> MediaTracks {
    let mut t = sample_tracks();
    t.subtitles[0].language = Some("eng".into());
    t.subtitles.push(SubtitleTrack { stream_index: 5, language: Some("tur".into()), title: None, codec: "subrip".into(), kind: SubtitleKind::Text, default: false, forced: false });
    t
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
    /// The file names next to `video`.
    fn folder_of(&self, video: &std::path::Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(video.parent().unwrap()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }
    fn menu_ids(&self) -> Vec<String> {
        self.main().invoke_menu_requested("tracks".into());
        self.menu().iter().map(|e| e.id.to_string()).filter(|i| i.starts_with("subtitle")).collect()
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
    r.open_with_tracks("film.mkv", manual_tracks());
    assert!(r.main().get_tracks_available());
    r.main().invoke_menu_requested("tracks".into());
    let menu = r.menu();
    let titles: Vec<String> = menu.iter().map(|e| e.title.to_string()).collect();
    assert_eq!(titles, vec!["Ses", "İngilizce (AAC, stereo)", "Türkçe — Yorum (AAC, mono)", "", "Videodaki altyazılar", "Kapalı", "İngilizce (SRT)", "Japonca (PGS)"]);
    assert!(menu[0].header && menu[4].header && menu[3].separator);
    assert!(menu[1].checked && menu[1].radio && !menu[2].checked);
    assert!(menu[5].checked, "\"Off\" holds while nothing from the video is loaded");
    assert!(menu[6].enabled && menu[7].enabled, "the PGS track can be chosen");
    assert!(menu[7].hint.is_empty() && !menu[7].checked);
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
    r.open_with_tracks("plain.mp4", MediaTracks { audio: vec![sample_tracks().audio[0].clone()], active_audio: Some(1), ..MediaTracks::default() });
    assert!(!r.main().get_tracks_available());
}

#[test]
fn choosing_an_embedded_text_track_loads_it_without_a_file_and_marks_it_in_the_menu() {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let r = rig_with(false, Some(fake_reader(asked.clone())));
    let video = r.open_with_tracks("films/film.mkv", manual_tracks());
    assert!(asked.lock().is_empty(), "an English track that is not the default is not chosen for a Turkish UI");
    r.main().invoke_menu_activated("subtitle:4".into());
    r.pump();
    assert!(asked.lock().is_empty(), "image tracks are not read");
    assert_eq!(r.backend.lock().images, vec![Some(4)], "the player shows it");
    r.main().invoke_menu_activated("subtitle:3".into());
    r.env.eventually("the track's lines", || r.vm.sentences().len() == 1);
    assert_eq!(asked.lock().clone(), vec![(video.clone(), 3)]);
    assert_eq!(r.backend.lock().images, vec![Some(4), None], "a text track takes the image track off the video");
    let line = &r.vm.sentences()[0];
    assert_eq!((line.start, line.end, line.text.as_str()), (1.0, 2.5, "Gömülü parça 3"));
    assert_eq!(r.vm.video(), Some(video.clone()), "the video stays open");
    assert_eq!(r.vm.loaded_subtitle(), None, "no file behind the lines");
    assert!(!r.vm.has_unsaved_edits());
    assert!(!r.main().get_tracks_busy());
    r.main().invoke_menu_requested("tracks".into());
    let menu = r.menu();
    let sub = menu.iter().find(|e| e.id == "subtitle:3").unwrap();
    assert!(sub.checked, "the track in the list is checked");
    assert!(!sub.radio);
    let off = menu.iter().find(|e| e.id == "subtitle:off").expect("Off while the lines are the track's");
    assert_eq!(off.title, "Kapalı");
    // Choosing the track that is already in the list does nothing (edits stay).
    r.vm.set_text(r.vm.sentences()[0].uid, "Düzeltildi");
    assert!(r.vm.has_unsaved_edits());
    r.main().invoke_menu_requested("tracks".into());
    let off = r.menu().into_iter().find(|e| e.id == "subtitle:off").expect("Off");
    assert!(!off.enabled && !off.checked, "Off cannot clear edited lines");
    r.main().invoke_menu_activated("subtitle:3".into());
    r.pump();
    assert_eq!(asked.lock().len(), 1, "not read again");
    assert!(!r.main().get_tracks_busy());
    assert_eq!(r.vm.sentences()[0].text, "Düzeltildi");
    assert!(r.vm.pending_open().is_none());
    assert_eq!(r.folder_of(&video), vec!["film.mkv"]);
}

fn pgs_picture(id: u64) -> Arc<SubtitlePicture> {
    let rect = |x, y, w: u32, h: u32| SubtitleBitmap { x, y, width: w, height: h, rgba: Arc::new(vec![255; (w * h * 4) as usize]) };
    Arc::new(SubtitlePicture { id, stream_index: 4, canvas: (1920, 1080), video_size: Some((1920, 1080)), start: 1.0, end: None, rects: vec![rect(480, 900, 960, 108), rect(0, 0, 192, 54)] })
}

impl Rig {
    fn picture_rects(&self) -> Vec<ui::PictureSubtitleRect> {
        let m = self.main().get_picture_subtitles();
        (0..m.row_count()).filter_map(|i| m.row_data(i)).collect()
    }
    /// The player reports `picture` on screen (`None`: none).
    fn show_picture(&self, picture: Option<Arc<SubtitlePicture>>) {
        let (sink, generation) = {
            let mut s = self.backend.lock();
            s.picture = picture;
            (s.sink.clone().unwrap(), s.generation)
        };
        sink(PlayerEvent::SubtitlePictureChanged { generation });
        self.pump();
    }
}

#[test]
fn an_image_track_is_drawn_over_the_video_from_the_players_pictures_and_cleared() {
    let r = rig(false);
    r.open_with_tracks("film.mkv", manual_tracks());
    r.main().invoke_menu_activated("subtitle:4".into());
    r.pump();
    assert_eq!(r.backend.lock().images, vec![Some(4)]);
    r.main().invoke_menu_requested("tracks".into());
    let menu = r.menu();
    assert!(menu.iter().find(|e| e.id == "subtitle:4").unwrap().checked, "the image track shown is checked");
    assert!(!menu.iter().find(|e| e.id == "subtitle:off").unwrap().checked);
    assert!(r.picture_rects().is_empty());
    r.show_picture(Some(pgs_picture(1)));
    let rects = r.picture_rects();
    assert_eq!(rects.len(), 2);
    assert_eq!((rects[0].x, rects[0].y, rects[0].width, rects[0].height), (480.0, 900.0, 960.0, 108.0));
    assert_eq!((rects[0].canvas_width, rects[0].canvas_height, rects[0].cover), (1920.0, 1080.0, false), "the video's own size stretches");
    assert_eq!((rects[0].image.size().width, rects[0].image.size().height), (960, 108));
    assert_eq!((rects[1].x, rects[1].y, rects[1].width), (0.0, 0.0, 192.0));
    // The overlay asks the window where each goes, for its own size (here a 960 × 540 surface, 16:9 video).
    let b = r.main().invoke_place_picture(rects[0].clone(), 960.0, 540.0, 16.0 / 9.0);
    assert_eq!((b.x, b.y, b.width, b.height), (240.0, 450.0, 480.0, 54.0));
    // A cropped encode (1920 × 800) with a 1920 × 1080 PGS canvas: the canvas keeps its shape over the video.
    let cropped = Arc::new(SubtitlePicture { video_size: Some((1920, 800)), id: 9, ..(*pgs_picture(9)).clone() });
    r.show_picture(Some(cropped));
    let rects = r.picture_rects();
    assert!(rects[0].cover);
    let b = r.main().invoke_place_picture(rects[0].clone(), 1600.0, 900.0, 2.4);
    assert!((b.y - 750.0).abs() < 0.01 && (b.height - 90.0).abs() < 0.01, "{:?}", (b.x, b.y, b.width, b.height));
    // The text overlay style is the user's text lines'; pictures are not styled, and the line overlay stays as is.
    assert_eq!(r.main().get_overlay_text(), "");
    // Off hides it (the player is told; its next picture event empties the overlay).
    r.main().invoke_menu_activated("subtitle:off".into());
    r.pump();
    assert_eq!(r.backend.lock().images, vec![Some(4), None]);
    r.main().invoke_menu_requested("tracks".into());
    assert!(r.menu().iter().find(|e| e.id == "subtitle:off").unwrap().checked);
    // A stale load's picture event is ignored; another video clears what is shown.
    r.open_with_tracks("other.mkv", manual_tracks());
    assert!(r.picture_rects().is_empty());
    assert!(r.vm.sentences().is_empty() && r.vm.video().is_some());
}

#[test]
fn the_picture_overlay_follows_the_players_events() {
    let r = rig(false);
    r.open_with_tracks("film.mkv", manual_tracks());
    r.main().invoke_menu_activated("subtitle:4".into());
    r.pump();
    r.show_picture(Some(pgs_picture(1)));
    assert_eq!(r.picture_rects().len(), 2);
    r.show_picture(Some(pgs_picture(2)));
    assert_eq!(r.picture_rects().len(), 2);
    r.show_picture(None);
    assert!(r.picture_rects().is_empty(), "a clearing event empties the overlay");
    // A failure to show a track is reported in the error bar.
    let (sink, generation) = {
        let s = r.backend.lock();
        (s.sink.clone().unwrap(), s.generation)
    };
    sink(PlayerEvent::SubtitleTrackFailed { generation, stream_index: 4, reason: PictureTrackFailure::Ffmpeg("Decoder not found".into()) });
    r.pump();
    assert!(r.main().get_error_open());
    assert_eq!(r.main().get_error_text(), "Altyazı parçası gösterilemedi: Decoder not found");
    // The player's own reasons are worded in the UI language.
    sink(PlayerEvent::SubtitleTrackFailed { generation, stream_index: 4, reason: PictureTrackFailure::TooLarge });
    r.pump();
    assert_eq!(r.main().get_error_text(), "Altyazı parçası gösterilemedi: Altyazı resimleri gösterilemeyecek kadar büyük");
}

#[test]
fn an_image_track_chosen_for_the_user_that_cannot_be_shown_goes_without_a_notice() {
    let r = rig(false);
    open_images_only(&r, "a/film.mkv");
    let (sink, generation) = {
        let s = r.backend.lock();
        (s.sink.clone().unwrap(), s.generation)
    };
    // Given up by the player (it turned the track off itself): nobody asked for it, so nothing is reported.
    sink(PlayerEvent::SubtitleTrackFailed { generation, stream_index: 4, reason: PictureTrackFailure::NoTimeCode });
    r.pump();
    assert!(!r.main().get_error_open(), "{}", r.main().get_error_text());
    // Chosen by the user afterwards, the same failure is reported.
    r.main().invoke_menu_activated("subtitle:4".into());
    r.pump();
    sink(PlayerEvent::SubtitleTrackFailed { generation, stream_index: 4, reason: PictureTrackFailure::NoTimeCode });
    r.pump();
    assert!(r.main().get_error_open());
    assert_eq!(r.main().get_error_text(), "Altyazı parçası gösterilemedi: XSUB paketlerinde zaman kodu yok");
}

#[test]
fn choosing_an_image_track_never_touches_the_users_lines_or_the_disk_and_drops_unedited_track_lines() {
    let script = scripted_reader();
    let r = rig_with(false, Some(script.reader.clone()));
    // The user's own lines (from a subtitle file they opened) and an image track: the lines stay exactly as they are.
    let video = r.open_with_tracks("own/film.mkv", manual_tracks());
    r.load_three_lines();
    r.vm.set_text(r.vm.sentences()[0].uid, "Benim düzeltmem");
    let before: Vec<(f64, f64, String)> = r.vm.sentences().iter().map(|s| (s.start, s.end, s.text.clone())).collect();
    let folder = r.folder_of(&video);
    r.main().invoke_menu_activated("subtitle:4".into());
    r.pump();
    assert_eq!(r.backend.lock().images, vec![Some(4)]);
    let after: Vec<(f64, f64, String)> = r.vm.sentences().iter().map(|s| (s.start, s.end, s.text.clone())).collect();
    assert_eq!(after, before, "the user's lines are left exactly as they were");
    assert!(r.vm.has_unsaved_edits(), "and still the user's unsaved work");
    assert_eq!(r.folder_of(&video), folder, "nothing was written next to the video");
    assert!(script.asked.lock().is_empty(), "nothing was read");
    // A text track the video brought, unedited (it was loaded for the user): choosing the image track drops it like
    // choosing another track does, and writes nothing.
    r.backend.lock().images.clear();
    let video = r.open_with_tracks("auto/film.mkv", film_tracks());
    r.env.eventually("the Turkish track is read", || script.asked.lock().len() == 1);
    script.steps.send(Step::Finish(track(&[(0.5, 1.5, "Bir"), (2.0, 3.0, "İki")]))).unwrap();
    r.env.eventually("loaded", || r.vm.sentences().len() == 2);
    r.pump();
    assert!(r.vm.embedded_lines().is_some_and(|e| e.pristine));
    r.main().invoke_menu_activated("subtitle:4".into());
    r.pump();
    assert_eq!(r.backend.lock().images, vec![Some(4)]);
    assert!(r.vm.sentences().is_empty() && r.vm.embedded_lines().is_none(), "the track's own lines went");
    assert!(!r.vm.has_unsaved_edits());
    assert_eq!(r.folder_of(&video), vec!["film.mkv"]);
    r.main().invoke_menu_requested("tracks".into());
    let menu = r.menu();
    assert!(menu.iter().find(|e| e.id == "subtitle:4").unwrap().checked);
    assert!(menu.iter().filter(|e| e.id.starts_with("subtitle:") && e.id != "subtitle:4").all(|e| !e.checked));
    // Choosing the text track again reads it and takes the image track off the video.
    r.main().invoke_menu_activated("subtitle:5".into());
    r.env.eventually("read again", || script.asked.lock().len() == 2);
    assert_eq!(r.backend.lock().images, vec![Some(4), None]);
    // Edited track lines are the user's: an image track leaves them as they are.
    script.steps.send(Step::Finish(track(&[(0.5, 1.5, "Bir")]))).unwrap();
    r.env.eventually("loaded again", || r.vm.sentences().len() == 1);
    r.pump();
    r.vm.set_text(r.vm.sentences()[0].uid, "Düzeltildi");
    r.main().invoke_menu_activated("subtitle:4".into());
    r.pump();
    assert_eq!(r.vm.sentences().len(), 1);
    assert_eq!(r.vm.sentences()[0].text, "Düzeltildi");
    assert_eq!(r.folder_of(&video), vec!["film.mkv"]);
}

/// Only image tracks, the UI language's shown for the user: the session's own lines take its place.
fn open_images_only(r: &Rig, name: &str) {
    let mut images = sample_tracks();
    images.subtitles.retain(|t| t.kind == SubtitleKind::Image);
    images.subtitles[0].language = Some("tur".into());
    r.open_with_tracks(name, images);
    r.pump();
    assert_eq!(r.backend.lock().images, vec![Some(4)], "shown for the user");
}

#[test]
fn an_image_track_chosen_for_the_user_goes_when_the_session_gets_lines_of_its_own() {
    use sa_core::engine_types::Sentence;
    use sa_enginekit::events::EngineEvent;
    // An imported subtitle file.
    let r = rig(false);
    open_images_only(&r, "a/film.mkv");
    r.load_three_lines();
    assert_eq!(r.backend.lock().images, vec![Some(4), None], "turned off, not overlapping the imported lines");
    r.main().invoke_menu_requested("tracks".into());
    assert!(r.menu().iter().filter(|e| e.id.starts_with("subtitle:")).all(|e| !e.checked || e.id == "subtitle:off"));
    // A transcription's first live lines.
    r.backend.lock().images.clear();
    open_images_only(&r, "b/film.mkv");
    r.vm.generate();
    let call = r.env.next(common::CallKind::Transcribe);
    assert_eq!(r.backend.lock().images, vec![Some(4)], "a job without lines yet changes nothing");
    call.send(EngineEvent::Partial { language: "tr".into(), sentences: vec![Sentence::new(1, 0.0, 2.0, "Bir.")] });
    r.env.eventually("the first partial", || {
        r.pump();
        r.rows().len() == 1
    });
    assert_eq!(r.backend.lock().images, vec![Some(4), None]);
    r.vm.cancel();
}

#[test]
fn an_image_track_the_user_chose_stays_when_lines_come() {
    let r = rig(false);
    open_images_only(&r, "film.mkv");
    // Chosen from the menu (again): it is the user's choice now.
    r.main().invoke_menu_requested("tracks".into());
    r.main().invoke_menu_activated("subtitle:4".into());
    r.pump();
    assert_eq!(r.backend.lock().images, vec![Some(4), Some(4)]);
    r.load_three_lines();
    assert_eq!(r.backend.lock().images, vec![Some(4), Some(4)], "both shown, as the user asked");
    assert_eq!(r.vm.sentences().len(), 3);
}

/// M1: a read that ends after the user edited, or after a job started, does not replace the lines silently.
#[test]
fn a_read_that_ends_after_the_session_changed_is_not_imported_silently() {
    let (reader, release) = gated_reader();
    let r = rig_with(false, Some(reader));
    r.open_with_tracks("film.mkv", manual_tracks());
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

/// M1: generating subtitles supersedes a read filling the list: it stops silently (the user changed nothing).
#[test]
fn generating_subtitles_while_a_track_is_read_stops_the_read_silently() {
    let (reader, release) = gated_reader();
    let r = rig_with(false, Some(reader));
    r.open_with_tracks("film.mkv", manual_tracks());
    r.main().invoke_menu_activated("subtitle:3".into());
    r.pump();
    assert!(r.main().get_tracks_busy());
    assert!(r.vm.can_generate(), "generating stays available while the track is read");
    r.vm.generate();
    let _call = r.env.next(common::CallKind::Transcribe);
    assert!(r.vm.is_working());
    r.pump();
    assert!(!r.main().get_tracks_busy(), "the read stopped when the job started");
    release.send(()).unwrap();
    for _ in 0..10 {
        r.pump();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(r.vm.is_working(), "the job was not stopped");
    assert!(r.vm.sentences().is_empty());
    assert!(!r.main().get_error_open(), "a job start is not reported as the subtitles changing");
    assert_eq!(r.vm.embedded_read_progress(), None);
}

/// L2: the track menu refills the shared menu entries only while they are its own, and another video closes it.
#[test]
fn track_changes_never_overwrite_another_menu() {
    let r = rig(false);
    r.open_with_tracks("film.mkv", manual_tracks());
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
    r.open_with_tracks("other.mkv", manual_tracks());
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
    r.open_with_tracks("film.mkv", manual_tracks());
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
    r.open_with_tracks("film.mkv", manual_tracks());
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
    let reader: SubtitleReader = Arc::new(
        move |_p: &std::path::Path, _s: usize, cancel: &sa_core::cancel::CancellationToken, _progress: &mut dyn FnMut(f64), _snap: Option<&mut dyn FnMut(Vec<Sentence>)>| {
            // Blocks until the test lets it go, then reports how it ended.
            let _ = rx.lock().recv_timeout(std::time::Duration::from_secs(5));
            seen.store(cancel.is_cancelled(), std::sync::atomic::Ordering::SeqCst);
            Err(sa_player::ffmpeg_backend::ExtractError::Cancelled)
        },
    );
    let r = rig_with(false, Some(reader));
    r.open_with_tracks("film.mkv", manual_tracks());
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

// ------------------------------------------------------------------------------------------------ embedded tracks on open

fn auto_opens_the_ui_languages_track_and_fills_the_list_as_it_reads(mac: bool) {
    let script = scripted_reader();
    let r = rig_with(mac, Some(script.reader.clone()));
    assert!(r.vm.auto_save_subtitle(), "auto-save is on");
    let video = r.open_with_tracks("films/film.mkv", film_tracks());
    r.env.eventually("the Turkish track is read", || script.asked.lock().len() == 1);
    assert_eq!(script.asked.lock()[0], (video.clone(), 5));
    assert!(r.main().get_tracks_busy());
    // The first lines arrive while the rest is still being read.
    script.steps.send(Step::Snapshot(track(&[(0.5, 1.5, "Bir"), (2.0, 3.0, "İki")]))).unwrap();
    r.env.eventually("the first snapshot", || r.rows().len() == 2);
    assert!(r.main().get_tracks_busy(), "still reading");
    let first_row = r.rows()[0].uid.clone();
    // Playback ahead of what was read shows nothing ...
    assert!(r.key(RIGHT, true, false));
    r.pump();
    let at = r.backend.lock().position;
    assert!(at > 3.0, "seeked past the lines read so far: {at}");
    assert_eq!(r.main().get_overlay_text(), "");
    // ... until the read gets there.
    script.steps.send(Step::Snapshot(track(&[(0.5, 1.5, "Bir"), (2.0, 3.0, "İki"), (3.5, at + 5.0, "Üç")]))).unwrap();
    r.env.eventually("the second snapshot", || r.rows().len() == 3);
    assert_eq!(r.main().get_overlay_text(), "Üç");
    assert_eq!(r.rows()[0].uid, first_row, "rows keep their identity: the list grows in place");
    let all = track(&[(0.5, 1.5, "Bir"), (2.0, 3.0, "İki"), (3.5, at + 5.0, "Üç"), (at + 6.0, at + 7.0, "Dört")]);
    script.steps.send(Step::Finish(all.clone())).unwrap();
    r.env.eventually("the read ended", || !r.main().get_tracks_busy() && r.rows().len() == 4);
    let got: Vec<(f64, f64, String)> = r.vm.sentences().into_iter().map(|l| (l.start, l.end, l.text)).collect();
    assert_eq!(got, all.into_iter().map(|l| (l.start, l.end, l.text)).collect::<Vec<_>>());
    // Not the user's work: clean, no file behind it, nothing written next to the video.
    assert!(!r.vm.has_unsaved_edits());
    assert_eq!(r.vm.loaded_subtitle(), None);
    assert!(r.vm.embedded_lines().is_some_and(|e| e.stream == 5 && e.pristine && e.complete));
    let ids = r.menu_ids();
    assert_eq!(ids, vec!["subtitle:off", "subtitle:3", "subtitle:4", "subtitle:5"]);
    let menu = r.menu();
    assert!(menu.iter().find(|e| e.id == "subtitle:5").unwrap().checked, "the track loaded for the user is checked");
    // Another video and back, then the exit: nothing was ever saved.
    r.open_video("films/other.mkv");
    r.open_video("films/film.mkv");
    r.vm.shutdown();
    r.pump();
    assert_eq!(r.folder_of(&video), vec!["film.mkv", "other.mkv"]);
}

#[test]
fn a_video_opens_with_its_turkish_track_filled_in_as_it_is_read() {
    auto_opens_the_ui_languages_track_and_fills_the_list_as_it_reads(false);
}

#[test]
fn on_macos_a_video_opens_with_its_turkish_track_filled_in_as_it_is_read() {
    auto_opens_the_ui_languages_track_and_fills_the_list_as_it_reads(true);
}

#[test]
fn an_english_ui_gets_the_english_track_and_off_clears_it() {
    let script = scripted_reader();
    let r = rig_with(false, Some(script.reader.clone()));
    r._lang.set(UiLanguage::En);
    r.open_with_tracks("film.mkv", film_tracks());
    r.env.eventually("the English track is read", || script.asked.lock().len() == 1);
    assert_eq!(script.asked.lock()[0].1, 3);
    script.steps.send(Step::Finish(track(&[(0.5, 1.5, "One")]))).unwrap();
    r.env.eventually("loaded", || r.rows().len() == 1);
    r.pump();
    r.main().invoke_menu_requested("tracks".into());
    assert_eq!(r.menu().iter().find(|e| e.id == "subtitle:off").map(|e| e.title.to_string()).as_deref(), Some("Off"));
    r.main().invoke_menu_activated("subtitle:off".into());
    r.pump();
    assert!(r.rows().is_empty() && r.vm.sentences().is_empty());
    assert!(r.vm.embedded_lines().is_none());
    r.main().invoke_menu_requested("tracks".into());
    let off = r.menu().into_iter().find(|e| e.id == "subtitle:off").expect("Off");
    assert!(off.checked && off.enabled, "Off is checked while nothing from the video is loaded");
    assert_eq!(script.asked.lock().len(), 1, "not read again");
    // Remembered for the video in this run: opening it again chooses nothing.
    r.open_video("other.mkv");
    r.open_with_tracks("film.mkv", film_tracks());
    for _ in 0..5 {
        r.pump();
    }
    assert_eq!(script.asked.lock().len(), 1, "Off holds when the video opens again");
    assert!(r.vm.sentences().is_empty());
}

#[test]
fn nothing_is_chosen_over_lines_a_sidecar_or_a_job_and_an_image_track_only_without_a_text_one() {
    let script = scripted_reader();
    let r = rig_with(false, Some(script.reader.clone()));
    // A subtitle next to the video.
    r.env.touch("withsrt/film.srt", "1\n00:00:00,500 --> 00:00:01,250\nYandaki\n");
    r.open_with_tracks("withsrt/film.mkv", film_tracks());
    r.env.eventually("the sidecar", || r.vm.sentences().len() == 1);
    for _ in 0..5 {
        r.pump();
    }
    assert!(script.asked.lock().is_empty(), "the sidecar wins");
    // Lines the user loaded before the tracks were known.
    r.open_video("lines/film.mkv");
    r.load_three_lines();
    let (sink, generation) = {
        let mut s = r.backend.lock();
        s.tracks = film_tracks();
        (s.sink.clone().unwrap(), s.generation)
    };
    sink(PlayerEvent::Opened { generation, duration: 10.0, natural_size: Some((320, 240)) });
    sink(PlayerEvent::TracksChanged { generation });
    r.pump();
    assert!(script.asked.lock().is_empty(), "the user's lines stay");
    // A job running.
    r.open_video("job/film.mkv");
    r.vm.generate();
    let _call = r.env.next(common::CallKind::Transcribe);
    let (sink, generation) = {
        let mut s = r.backend.lock();
        s.tracks = film_tracks();
        (s.sink.clone().unwrap(), s.generation)
    };
    sink(PlayerEvent::TracksChanged { generation });
    r.pump();
    assert!(script.asked.lock().is_empty(), "the job is not disturbed");
    r.vm.cancel();
    assert!(r.backend.lock().images.is_empty(), "no image track is shown over lines, a sidecar or a job either");
    // Only image-based tracks in the UI language: one is shown, never read.
    let mut images = sample_tracks();
    images.subtitles.retain(|t| t.kind == SubtitleKind::Image);
    images.subtitles[0].language = Some("tur".into());
    r.open_with_tracks("images/film.mkv", images);
    r.pump();
    assert!(script.asked.lock().is_empty(), "image tracks are never read");
    assert_eq!(r.backend.lock().images, vec![Some(4)], "the UI language's image track is shown");
    assert!(r.vm.sentences().is_empty());
}

#[test]
fn an_edit_while_the_track_is_read_keeps_the_users_lines_and_stops_the_read() {
    let script = scripted_reader();
    let r = rig_with(false, Some(script.reader.clone()));
    r.open_with_tracks("film.mkv", film_tracks());
    r.env.eventually("reading", || script.asked.lock().len() == 1);
    script.steps.send(Step::Snapshot(track(&[(0.5, 1.5, "Bir"), (2.0, 3.0, "İki")]))).unwrap();
    r.env.eventually("the first snapshot", || r.rows().len() == 2);
    r.vm.set_text(r.vm.sentences()[0].uid, "Benim");
    script.steps.send(Step::Snapshot(track(&[(0.5, 1.5, "Bir"), (2.0, 3.0, "İki"), (4.0, 5.0, "Üç")]))).unwrap();
    r.env.eventually("the read stopped", || script.cancelled.load(std::sync::atomic::Ordering::SeqCst) == 1);
    r.pump();
    assert_eq!(r.vm.sentences().len(), 2, "no line was added after the edit");
    assert_eq!(r.vm.sentences()[0].text, "Benim");
    assert!(!r.main().get_tracks_busy());
    assert!(r.main().get_error_open());
    assert_eq!(r.main().get_error_text(), "Videodaki altyazının yalnızca bir kısmı yüklendi: okunurken altyazı değişti. Parçayı yeniden seç.");
    assert!(r.vm.has_unsaved_edits(), "the edited lines are the user's");
}

#[test]
fn another_video_cancels_the_read_and_the_first_one_is_read_again_when_it_comes_back() {
    let script = scripted_reader();
    let r = rig_with(false, Some(script.reader.clone()));
    r.open_with_tracks("film.mkv", film_tracks());
    r.env.eventually("reading", || script.asked.lock().len() == 1);
    script.steps.send(Step::Snapshot(track(&[(0.5, 1.5, "Bir")]))).unwrap();
    r.env.eventually("the first snapshot", || r.rows().len() == 1);
    r.open_with_tracks("other.mkv", manual_tracks());
    assert!(!r.main().get_tracks_busy(), "the read is dropped with the video");
    r.env.eventually("the reader saw the cancel", || script.cancelled.load(std::sync::atomic::Ordering::SeqCst) == 1);
    assert!(r.vm.sentences().is_empty());
    // Back to the first video: its half-read lines were dropped, so it is read again from the start.
    r.open_with_tracks("film.mkv", film_tracks());
    r.env.eventually("read again", || script.asked.lock().len() == 2);
    assert!(r.vm.sentences().is_empty());
    // Opening the same video again does not start a second read.
    r.vm.open_video(r.vm.video());
    let (sink, generation) = {
        let s = r.backend.lock();
        (s.sink.clone().unwrap(), s.generation)
    };
    sink(PlayerEvent::TracksChanged { generation });
    r.pump();
    assert_eq!(script.asked.lock().len(), 2);
    script.steps.send(Step::Finish(track(&[(0.5, 1.5, "Bir"), (2.0, 3.0, "İki")]))).unwrap();
    r.env.eventually("loaded", || r.rows().len() == 2);
}

fn cancels(script: &Script) -> usize {
    script.cancelled.load(std::sync::atomic::Ordering::SeqCst)
}

/// Lets the UI thread run a few times (posts from worker threads included).
fn settle(r: &Rig) {
    for _ in 0..10 {
        r.pump();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// H1 + M1: a batch run started while the video's track is being read into its list stops the read silently, drops
/// the lines read so far and transcribes the video; while the read ran, saving waited and the status said why.
#[test]
fn a_batch_run_stops_a_read_in_flight_silently_and_transcribes_the_video() {
    let script = scripted_reader();
    let r = rig_with(false, Some(script.reader.clone()));
    r.env.settings.set_string(sa_core::settings::keys::TRANSLATION_TARGET, "tr");
    let video = r.open_with_tracks("h1/film.mkv", film_tracks());
    r.env.eventually("reading", || script.asked.lock().len() == 1);
    script.steps.send(Step::Snapshot(track(&[(0.5, 1.5, "Bir"), (2.0, 3.0, "İki")]))).unwrap();
    r.env.eventually("the first snapshot", || r.rows().len() == 2);
    assert!(r.vm.embedded_read_progress().is_some());
    assert!(!r.vm.can_save_subtitle() && !r.vm.can_translate() && !r.vm.can_export());
    assert!(r.main().get_status_text().contains("Okunuyor… %"), "{}", r.main().get_status_text());
    r.vm.add_playlist_videos_with_language(vec![video.clone()], "en");
    r.vm.start_playlist();
    r.pump();
    assert!(!r.main().get_tracks_busy(), "the read stopped when the run started");
    r.env.eventually("the reader saw the cancel", || cancels(&script) == 1);
    let call = r.env.next(common::CallKind::Transcribe);
    assert_eq!(call.record.video.as_deref(), Some(video.as_path()));
    settle(&r);
    assert!(r.vm.sentences().iter().all(|l| l.text != "Bir" && l.text != "İki"), "the track's lines went");
    assert!(!r.main().get_error_open(), "nothing to report: the user changed nothing");
    assert_eq!(r.vm.embedded_read_progress(), None);
    assert_eq!(r.folder_of(&video), vec!["film.mkv"]);
}

/// Closing the window (and quitting) while a track is read, with a snapshot posted and not yet applied: nothing is
/// written, nothing asks about unsaved changes.
fn closing_while_a_track_is_read_writes_nothing(mac: bool) {
    let script = scripted_reader();
    let r = rig_with(mac, Some(script.reader.clone()));
    let video = r.open_with_tracks("close/film.mkv", film_tracks());
    r.env.eventually("reading", || script.asked.lock().len() == 1);
    script.steps.send(Step::Snapshot(track(&[(0.5, 1.5, "Bir")]))).unwrap();
    r.env.eventually("the first snapshot", || r.rows().len() == 1);
    // Another snapshot is taken by the reader and posted, but the UI thread does not get to it before the close.
    script.steps.send(Step::Snapshot(track(&[(0.5, 1.5, "Bir"), (2.0, 3.0, "İki")]))).unwrap();
    while !script.steps.is_empty() {
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    std::thread::sleep(std::time::Duration::from_millis(30));
    r.controller.detach();
    settle(&r);
    r.env.eventually("the reader saw the cancel", || cancels(&script) == 1);
    assert!(!r.vm.has_unsaved_edits(), "no unsaved-changes question on close");
    assert!(r.vm.pending_open().is_none());
    assert_eq!(r.vm.embedded_read_progress(), None);
    // Nothing asked the engine for a render (every write renders first): the marker is the first.
    let done = Rc::new(Cell::new(false));
    let d = done.clone();
    r.vm.render_export(Box::new(move |_| d.set(true)));
    r.env.eventually("the marker render", || done.get());
    assert_eq!(r.env.engine.renders.lock().len(), 1);
    r.vm.shutdown();
    settle(&r);
    assert_eq!(r.env.engine.renders.lock().len(), 1, "nothing rendered on the way out either");
    assert_eq!(r.folder_of(&video), vec!["film.mkv"]);
}

#[test]
fn closing_while_a_track_is_read_writes_nothing_and_asks_nothing() {
    closing_while_a_track_is_read_writes_nothing(false);
}

#[test]
fn on_macos_closing_while_a_track_is_read_writes_nothing_and_asks_nothing() {
    closing_while_a_track_is_read_writes_nothing(true);
}

/// L1: "Off" with text being typed into a line commits it and keeps the lines (they are the user's now).
#[test]
fn off_with_a_draft_being_typed_keeps_the_lines_with_the_draft() {
    let script = scripted_reader();
    let r = rig_with(false, Some(script.reader.clone()));
    r.open_with_tracks("l1/film.mkv", film_tracks());
    r.env.eventually("reading", || script.asked.lock().len() == 1);
    script.steps.send(Step::Finish(track(&[(0.5, 1.5, "Bir"), (2.0, 3.0, "İki")]))).unwrap();
    r.env.eventually("loaded", || r.rows().len() == 2 && !r.main().get_tracks_busy());
    let uid = r.vm.sentences()[0].uid;
    r.vm.select_row(uid);
    r.vm.begin_editing(uid);
    r.vm.set_draft(uid, "Yazıyorum");
    r.main().invoke_menu_activated("subtitle:off".into());
    r.pump();
    assert_eq!(r.vm.sentences().len(), 2, "the lines stay");
    assert_eq!(r.vm.sentences()[0].text, "Yazıyorum", "the draft was committed");
    assert!(r.vm.has_unsaved_edits());
}

/// L1: switching to another track with a draft being typed commits it first; the other track then replaces the
/// user's lines at the end of its read like opening a subtitle file (Windows: right away), never as it reads.
#[test]
fn switching_tracks_with_a_draft_being_typed_commits_it_and_reads_the_other_track_whole() {
    let script = scripted_reader();
    let r = rig_with(false, Some(script.reader.clone()));
    r.open_with_tracks("l1b/film.mkv", film_tracks());
    r.env.eventually("reading", || script.asked.lock().len() == 1);
    script.steps.send(Step::Finish(track(&[(0.5, 1.5, "Bir"), (2.0, 3.0, "İki")]))).unwrap();
    r.env.eventually("loaded", || r.rows().len() == 2 && !r.main().get_tracks_busy());
    let uid = r.vm.sentences()[0].uid;
    r.vm.select_row(uid);
    r.vm.begin_editing(uid);
    r.vm.set_draft(uid, "Yazıyorum");
    r.main().invoke_menu_activated("subtitle:3".into());
    r.env.eventually("the English track is read", || script.asked.lock().len() == 2);
    assert_eq!(r.vm.sentences()[0].text, "Yazıyorum", "committed before the choice");
    assert_eq!(script.progressive.lock().clone(), vec![true, false], "the user's lines are not filled in as it reads");
    script.steps.send(Step::Snapshot(track(&[(0.5, 1.5, "One")]))).unwrap();
    settle(&r);
    assert_eq!(r.vm.sentences()[0].text, "Yazıyorum");
    script.steps.send(Step::Finish(track(&[(0.5, 1.5, "One"), (2.0, 3.0, "Two")]))).unwrap();
    r.env.eventually("replaced at the end", || r.vm.sentences().first().is_some_and(|l| l.text == "One"));
    assert!(r.vm.embedded_lines().is_some_and(|e| e.stream == 3 && e.complete));
}

/// L2: a read that fails part-way. Chosen for the user: its lines go, quietly. Chosen by the user: they stay and
/// the user is told only part of the track was loaded.
#[test]
fn a_read_that_fails_part_way_drops_automatic_lines_and_reports_a_chosen_one() {
    let script = scripted_reader();
    let r = rig_with(false, Some(script.reader.clone()));
    r.open_with_tracks("l2/film.mkv", film_tracks());
    r.env.eventually("reading", || script.asked.lock().len() == 1);
    script.steps.send(Step::Snapshot(track(&[(0.5, 1.5, "Bir"), (2.0, 3.0, "İki")]))).unwrap();
    r.env.eventually("the first snapshot", || r.rows().len() == 2);
    script.steps.send(Step::Fail(sa_player::ffmpeg_backend::ExtractError::Failed("Disk okunamadı".into()))).unwrap();
    r.env.eventually("the read ended", || {
        r.pump();
        !r.main().get_tracks_busy()
    });
    assert!(r.vm.sentences().is_empty(), "half of a track chosen for the user is dropped");
    assert!(r.vm.embedded_lines().is_none());
    assert!(!r.main().get_error_open());
    assert_eq!(r.vm.embedded_read_progress(), None);
    // The user chooses it now.
    r.main().invoke_menu_activated("subtitle:5".into());
    r.env.eventually("read again", || script.asked.lock().len() == 2);
    script.steps.send(Step::Snapshot(track(&[(0.5, 1.5, "Bir"), (2.0, 3.0, "İki")]))).unwrap();
    r.env.eventually("the first snapshot", || r.rows().len() == 2);
    script.steps.send(Step::Fail(sa_player::ffmpeg_backend::ExtractError::Failed("Disk okunamadı".into()))).unwrap();
    r.env.eventually("the read ended", || {
        r.pump();
        !r.main().get_tracks_busy()
    });
    assert_eq!(r.vm.sentences().len(), 2, "the user's choice keeps what was read");
    assert!(r.main().get_error_open());
    assert_eq!(r.main().get_error_text(), "Videodaki altyazının yalnızca bir kısmı yüklendi: Disk okunamadı");
    assert!(r.vm.embedded_lines().is_some_and(|e| e.pristine && !e.complete));
    assert!(!r.vm.has_unsaved_edits());
}

/// M2: the track chosen again after the user edited an early-stopped read: the rest is added after their lines,
/// as one undo step.
#[test]
fn choosing_the_track_again_after_an_edit_stopped_its_read_adds_the_rest() {
    let script = scripted_reader();
    let r = rig_with(false, Some(script.reader.clone()));
    r.open_with_tracks("m2/film.mkv", film_tracks());
    r.env.eventually("reading", || script.asked.lock().len() == 1);
    script.steps.send(Step::Snapshot(track(&[(0.5, 1.5, "Bir"), (2.0, 3.0, "İki")]))).unwrap();
    r.env.eventually("the first snapshot", || r.rows().len() == 2);
    r.vm.set_text(r.vm.sentences()[1].uid, "Benim");
    script.steps.send(Step::Snapshot(track(&[(0.5, 1.5, "Bir"), (2.0, 3.0, "İki"), (4.0, 5.0, "Üç")]))).unwrap();
    r.env.eventually("the read stopped", || cancels(&script) == 1);
    r.pump();
    r.main().invoke_error_closed();
    let all = track(&[(0.5, 1.5, "Bir"), (2.0, 3.0, "İki"), (4.0, 5.0, "Üç"), (6.0, 7.0, "Dört")]);
    r.main().invoke_menu_activated("subtitle:5".into());
    r.env.eventually("read again", || script.asked.lock().len() == 2);
    assert_eq!(script.progressive.lock().clone(), vec![true, false]);
    script.steps.send(Step::Finish(all)).unwrap();
    r.env.eventually("the rest", || r.vm.sentences().len() == 4);
    let texts: Vec<String> = r.vm.sentences().into_iter().map(|l| l.text).collect();
    assert_eq!(texts, vec!["Bir", "Benim", "Üç", "Dört"]);
    assert!(!r.main().get_error_open());
    // Complete now: chosen again, nothing is read.
    r.main().invoke_menu_activated("subtitle:5".into());
    r.pump();
    assert_eq!(script.asked.lock().len(), 2);
    r.vm.undo();
    r.pump();
    let texts: Vec<String> = r.vm.sentences().into_iter().map(|l| l.text).collect();
    assert_eq!(texts, vec!["Bir", "Benim"]);
}

/// L2: a batch run of other videos leaves the read of the video on screen alone.
#[test]
fn a_batch_run_of_other_videos_does_not_stop_the_read_of_the_video_on_screen() {
    let script = scripted_reader();
    let r = rig_with(false, Some(script.reader.clone()));
    r.env.settings.set_string(sa_core::settings::keys::TRANSLATION_TARGET, "tr");
    r.open_with_tracks("l2/film.mkv", film_tracks());
    r.env.eventually("reading", || script.asked.lock().len() == 1);
    script.steps.send(Step::Snapshot(track(&[(0.5, 1.5, "Bir")]))).unwrap();
    r.env.eventually("the first snapshot", || r.rows().len() == 1);
    let other = r.env.touch("l2/other.mp4", "v");
    r.vm.add_playlist_videos_with_language(vec![other], "en");
    r.vm.start_playlist();
    let _call = r.env.next(common::CallKind::Transcribe);
    r.pump();
    assert!(r.main().get_tracks_busy(), "still reading");
    assert_eq!(cancels(&script), 0);
    script.steps.send(Step::Snapshot(track(&[(0.5, 1.5, "Bir"), (2.0, 3.0, "İki")]))).unwrap();
    r.env.eventually("the second snapshot", || r.rows().len() == 2);
    script.steps.send(Step::Finish(track(&[(0.5, 1.5, "Bir"), (2.0, 3.0, "İki"), (4.0, 5.0, "Üç")]))).unwrap();
    r.env.eventually("complete", || r.rows().len() == 3 && !r.main().get_tracks_busy());
    assert!(r.vm.embedded_lines().is_some_and(|e| e.pristine && e.complete));
}

/// L7b: another track chosen while one is read, failing before its first lines: the first one's half goes too (it
/// is not left looking like the whole track), and the failure is reported.
#[test]
fn a_track_that_fails_before_its_first_lines_takes_the_half_read_one_with_it() {
    let script = scripted_reader();
    let r = rig_with(false, Some(script.reader.clone()));
    r.open_with_tracks("l7/film.mkv", film_tracks());
    r.env.eventually("reading", || script.asked.lock().len() == 1);
    script.steps.send(Step::Snapshot(track(&[(0.5, 1.5, "Bir"), (2.0, 3.0, "İki")]))).unwrap();
    r.env.eventually("the first snapshot", || r.rows().len() == 2);
    r.main().invoke_menu_activated("subtitle:3".into());
    r.env.eventually("the other track is read", || script.asked.lock().len() == 2);
    r.env.eventually("the first read saw the cancel", || cancels(&script) == 1);
    assert_eq!(script.progressive.lock().clone(), vec![true, true]);
    script.steps.send(Step::Fail(sa_player::ffmpeg_backend::ExtractError::Failed("Bozuk paket".into()))).unwrap();
    r.env.eventually("the read ended", || {
        r.pump();
        !r.main().get_tracks_busy()
    });
    assert!(r.vm.sentences().is_empty(), "no half track left behind");
    assert!(r.vm.embedded_lines().is_none());
    assert!(r.main().get_error_open());
    assert_eq!(r.main().get_error_text(), "Videodaki altyazı okunamadı: Bozuk paket");
    settle(&r);
    assert_eq!(script.asked.lock().len(), 2, "nothing is read again by itself");
}

/// L7b: typing stops a read (the snapshot finds the user's work), and when the typing is taken back the lines are
/// the track's again: the read goes on to the end.
#[test]
fn a_read_stopped_by_typing_goes_on_when_the_typing_is_taken_back() {
    let script = scripted_reader();
    let r = rig_with(false, Some(script.reader.clone()));
    r.open_with_tracks("l7b/film.mkv", film_tracks());
    r.env.eventually("reading", || script.asked.lock().len() == 1);
    script.steps.send(Step::Snapshot(track(&[(0.5, 1.5, "Bir"), (2.0, 3.0, "İki")]))).unwrap();
    r.env.eventually("the first snapshot", || r.rows().len() == 2);
    let first = r.rows()[0].uid.clone();
    let uid = r.vm.sentences()[0].uid;
    r.vm.select_row(uid);
    r.vm.begin_editing(uid);
    r.vm.set_draft(uid, "Yazıyorum");
    script.steps.send(Step::Snapshot(track(&[(0.5, 1.5, "Bir"), (2.0, 3.0, "İki"), (4.0, 5.0, "Üç")]))).unwrap();
    r.env.eventually("the read stopped", || cancels(&script) == 1);
    r.pump();
    assert_eq!(r.vm.sentences().len(), 2);
    // The typing is taken back (the editor's text is the line's again, as Esc leaves it).
    r.vm.set_draft(uid, "Bir");
    r.env.eventually("read on", || script.asked.lock().len() == 2);
    assert_eq!(script.progressive.lock().clone(), vec![true, true]);
    script.steps.send(Step::Snapshot(track(&[(0.5, 1.5, "Bir"), (2.0, 3.0, "İki"), (4.0, 5.0, "Üç")]))).unwrap();
    r.env.eventually("the rest comes in", || r.rows().len() == 3);
    assert_eq!(r.rows()[0].uid, first, "the lines read before keep their rows");
    script.steps.send(Step::Finish(track(&[(0.5, 1.5, "Bir"), (2.0, 3.0, "İki"), (4.0, 5.0, "Üç"), (6.0, 7.0, "Dört")]))).unwrap();
    r.env.eventually("complete", || r.rows().len() == 4 && !r.main().get_tracks_busy());
    assert!(r.vm.embedded_lines().is_some_and(|e| e.pristine && e.complete));
    assert!(!r.vm.has_unsaved_edits());
}
