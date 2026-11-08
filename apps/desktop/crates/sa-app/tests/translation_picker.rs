//! The toolbar translation-model picker through the production composition ([`sa_app::app::AppShell`]) on a temp data
//! folder: the button opens the flyout/popover with the on-device models, the provider combo, search, choosing a
//! model (it closes) and the platform rules (Windows: choosing a provider in the combo activates it).

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter};
use slint::{ComponentHandle, Model};

use sa_app::app::{AppOptions, AppShell};
use sa_app::ui::MainBridge;
use sa_core::dispatch::{QueueDispatcher, UiDispatcher};
use sa_player::{LoadGeneration, MediaPlayerBackend, MediaSource, PlayerEvent, VideoFrame};
use sa_providers::TranslationProviderProfile;

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
}

fn ids(main: &MainBridge) -> Vec<String> {
    main.get_picker_rows().iter().map(|r| r.id.to_string()).collect()
}

#[test]
fn the_toolbar_picker_lists_chooses_and_closes() {
    install_platform();
    let dir = tempfile::tempdir().unwrap();
    let ui = QueueDispatcher::new();
    let dispatcher: Arc<dyn UiDispatcher> = ui.clone();
    let app = AppShell::build(AppOptions {
        data_dir: Some(dir.path().join("SubtitleAdder")),
        engine: None,
        ui: Some(dispatcher),
        player: Some(Box::new(|| Box::new(NullPlayer(0)) as Box<dyn MediaPlayerBackend>)),
        native_chrome: false,
        startup_side_effects: false,
        last_window_quits: true,
    });
    let w = app.open_window().expect("main window");
    app.show_window(&w).unwrap();
    let main = w.window.global::<MainBridge>();
    let pump = || {
        let _ = ui.run_until(Duration::from_millis(150), || false);
    };

    // The button asks Rust to open; Rust fills the state and bumps the open epoch.
    let opened = main.get_picker_open_epoch();
    main.invoke_translation_model_clicked();
    assert_eq!(main.get_picker_open_epoch(), opened + 1, "the flyout/popover is shown");
    let local: Vec<String> = app.catalog.translation_models().iter().map(|m| m.id.clone()).collect();
    assert_eq!(ids(&main), local, "on-device models first");
    assert_eq!(main.get_picker_provider_index(), 0);
    assert_eq!(main.get_picker_providers().row_count(), 1);
    assert_eq!(main.get_picker_providers().row_data(0).unwrap().title.as_str(), sa_core::l("Yerel modeller (Cihaz üzerinde)"));
    let current = app.vm.translation_model();
    assert!(main.get_picker_rows().iter().any(|r| r.current && r.id.as_str() == current), "the current model is marked");
    assert!(!main.get_picker_can_refresh(), "nothing to refresh for on-device models");

    // Search (case-insensitive contains) and the empty text.
    main.invoke_picker_search_edited("zzz-no-model".into());
    assert_eq!(main.get_picker_rows().row_count(), 0);
    assert_eq!(main.get_picker_empty_text().as_str(), sa_core::l("Aramayla eşleşen model yok."));
    main.invoke_picker_search_edited("".into());
    assert_eq!(ids(&main), local);

    // Choosing an on-device model sets `translationModel` and closes.
    let other = local.iter().find(|id| **id != current).cloned().expect("a second model");
    let closed = main.get_picker_close_epoch();
    main.invoke_picker_row_clicked(other.clone().into());
    assert_eq!(main.get_picker_close_epoch(), closed + 1, "a chosen model closes it");
    assert_eq!(app.vm.translation_model(), other);

    // A saved provider: its models (discovered, trimmed, sorted) once it is browsed.
    let profile = TranslationProviderProfile {
        id: uuid::Uuid::new_v4(),
        name: "LAN".into(),
        preset_id: "lan".into(),
        base_url: "http://10.0.0.2:1234".into(),
        model: "m-a".into(),
        async_translation: false,
        discovered_models: vec!["m-b".into(), " m-a ".into()],
    };
    let added = app.providers.add(profile, None, false);
    pump();
    main.invoke_translation_model_clicked();
    assert_eq!(main.get_picker_providers().row_count(), 2);
    let reset = main.get_picker_search_reset();
    main.invoke_picker_provider_selected(1);
    assert_eq!(main.get_picker_provider_index(), 1);
    assert_eq!(ids(&main), vec!["m-a", "m-b"]);
    assert_eq!(main.get_picker_search_reset(), reset + 1, "a provider switch clears the search");
    assert!(main.get_picker_show_refresh());
    if sa_app::UI_TREE == "win" {
        // C# `SelectProvider`: the combo makes the provider active at once.
        assert_eq!(app.providers.active_provider_id(), Some(added.id));
        assert!(main.get_picker_rows().iter().any(|r| r.current && r.id.as_str() == "m-a"));
    } else {
        assert_eq!(app.providers.active_provider_id(), None, "macOS: browsing changes nothing");
    }
    let closed = main.get_picker_close_epoch();
    main.invoke_picker_row_clicked("m-b".into());
    assert_eq!(main.get_picker_close_epoch(), closed + 1);
    assert_eq!(app.providers.active_provider_id(), Some(added.id), "a provider's model activates it");
    assert_eq!(app.providers.providers()[0].model, "m-b");
    pump();
    assert!(app.vm.translation_model_name().contains("m-b"), "{}", app.vm.translation_model_name());

    // Back to on-device: the provider is deactivated.
    main.invoke_translation_model_clicked();
    assert_eq!(main.get_picker_provider_index(), 1, "opens on the active provider");
    main.invoke_picker_provider_selected(0);
    main.invoke_picker_row_clicked(current.clone().into());
    assert_eq!(app.providers.active_provider_id(), None);
    assert_eq!(app.vm.translation_model(), current);

    app.shutdown();
}
