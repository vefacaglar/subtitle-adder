//! The production composition ([`sa_app::app::AppShell`]) over a temp data folder: real stores (Windows profiles.json
//! v3 + profiles.db, settings.json, media index, providers with the DPAPI sidecar), the real view models and a real
//! main window on the software renderer. No engine and no audio device are used.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use slint::ComponentHandle;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter};

use sa_app::app::{AppOptions, AppShell, AppWindow};
use sa_app::ui::{HubBridge, MainBridge, Workspace};
use sa_core::dispatch::{QueueDispatcher, UiDispatcher};
use sa_player::{LoadGeneration, MediaPlayerBackend, MediaSource, PlayerEvent, VideoFrame};

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

/// A silent player (no FFmpeg device work in this test).
struct NullPlayer(i32);

impl MediaPlayerBackend for NullPlayer {
    fn load(&mut self, _source: Option<MediaSource>, _auto_play: bool) -> LoadGeneration {
        self.0 += 1;
        self.0
    }
    fn play(&mut self) {}
    fn pause(&mut self) {}
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
        false
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
    fn set_event_sink(&mut self, _sink: Box<dyn Fn(PlayerEvent) + Send + Sync>) {}
    fn dispose(&mut self) {}
    fn tracks(&self) -> sa_player::MediaTracks {
        sa_player::MediaTracks::default()
    }
    fn select_audio_track(&mut self, _stream_index: usize) {}
    fn select_image_subtitle_track(&mut self, _stream_index: Option<usize>) {}
    fn subtitle_picture(&self) -> Option<std::sync::Arc<sa_player::SubtitlePicture>> {
        None
    }
}

#[test]
fn the_production_composition_runs_on_a_temp_data_folder_and_shows_the_sign_in_card_logged_out() {
    install_platform();
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("SubtitleAdder");
    let ui = QueueDispatcher::new();
    let dispatcher: Arc<dyn UiDispatcher> = ui.clone();
    let app = AppShell::build(AppOptions {
        data_dir: Some(data.clone()),
        engine: None,
        ui: Some(dispatcher),
        player: Some(Box::new(|| Box::new(NullPlayer(0)) as Box<dyn MediaPlayerBackend>)),
        native_chrome: false,
        startup_side_effects: false,
        // macOS lifetime on every target: closing the last window keeps the app (and this test) running.
        last_window_quits: false,
    });
    assert_eq!(app.data_dir, data);
    let w = app.open_window().expect("main window");
    app.show_window(&w).unwrap();
    // Background store starts (profiles header read, explorer, hub) report back through the queue.
    app.hub.start();
    let ok = ui.run_until(Duration::from_secs(20), || w.window.global::<HubBridge>().get_shows_auth_card());
    assert!(ok, "logged out: the hub shows the sign-in card");

    // Every hub tab is the sign-in card while logged out (rule 6).
    w.window.global::<MainBridge>().invoke_workspace_selected(Workspace::Platform);
    for filter in 0..5 {
        w.window.global::<HubBridge>().invoke_filter_selected(filter);
        let _ = ui.run_until(Duration::from_millis(200), || false);
        assert!(w.window.global::<HubBridge>().get_shows_auth_card(), "tab {filter}");
        assert_eq!(w.window.global::<HubBridge>().get_cards().row_count(), 0, "tab {filter}: no cards");
    }

    // Register + login through the bridge, then logout: the feed is gone again and nothing stays on screen.
    w.window.global::<HubBridge>().invoke_register("E2E".into(), "e2e-pass-1".into(), false);
    let ok = ui.run_until(Duration::from_secs(30), || w.window.global::<HubBridge>().get_is_logged_in());
    assert!(ok, "registered and logged in");
    w.window.global::<HubBridge>().invoke_logout();
    let ok = ui.run_until(Duration::from_secs(10), || !w.window.global::<HubBridge>().get_is_logged_in());
    assert!(ok, "logged out");
    assert!(w.window.global::<HubBridge>().get_shows_auth_card());
    assert_eq!(w.window.global::<HubBridge>().get_cards().row_count(), 0);

    // Dock reopen (the macOS `rapp` handler posts this when no window is left): an open window is reused, and with
    // every window closed a new main window opens, sharing the view models.
    app.reopen_main_window();
    assert_eq!(app.windows.borrow().len(), 1, "an open window is shown, not duplicated");
    assert!(Rc::ptr_eq(&app.windows.borrow()[0], &w));
    close_through_the_title_bar(&w);
    assert!(app.windows.borrow().is_empty(), "the closed window left the list");
    assert!(!w.window.window().is_visible());
    app.reopen_main_window();
    assert_eq!(app.windows.borrow().len(), 1, "a new main window after the last one closed");
    let reopened = app.windows.borrow()[0].clone();
    assert!(!Rc::ptr_eq(&reopened, &w));
    assert!(reopened.window.window().is_visible());

    // Finder / a drop on the Dock icon with no window left opens a main window for the files (SwiftUI
    // `handlesExternalEvents`), instead of loading them into a view model nobody sees.
    close_through_the_title_bar(&reopened);
    assert!(app.windows.borrow().is_empty());
    app.open_external_files(vec![dir.path().join("missing.mp4")]);
    assert_eq!(app.windows.borrow().len(), 1, "an external open with no window opens one");
    assert!(app.windows.borrow()[0].window.window().is_visible());
    assert!(!Rc::ptr_eq(&app.windows.borrow()[0], &reopened));
    app.open_external_files(vec![dir.path().join("missing.mp4")]);
    assert_eq!(app.windows.borrow().len(), 1, "with a window open, the files go to it");

    app.shutdown();
    // The C#-compatible files exist in the injected folder only.
    assert!(data.join("profiles.json").is_file());
    #[cfg(windows)]
    {
        assert!(data.join("profiles.db").is_file());
        assert!(data.join("settings.json").is_file() || !data.join("settings.json").exists());
    }
}

/// The window's close button: the real `on_close_requested` → `window_closing` path.
fn close_through_the_title_bar(w: &AppWindow) {
    w.window.window().dispatch_event(slint::platform::WindowEvent::CloseRequested);
}

use slint::Model as _;
