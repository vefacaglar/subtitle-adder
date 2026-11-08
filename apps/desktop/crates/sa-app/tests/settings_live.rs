//! Opens the real Settings window (winit backend, native chrome on Windows) over in-memory fakes, checks that the
//! saved size is written on close, and quits. Needs a desktop session: `cargo test -p sa-app --test settings_live
//! -- --ignored`.

use std::sync::Arc;
use std::time::Duration;

use sa_app::settings_window::SettingsWindowHost;
use sa_app::settings_window::testing::fake_deps;
use sa_core::dispatch::UiDispatcher;
use sa_core::settings::SettingsStore;

#[test]
#[ignore = "opens a real window"]
fn settings_window_opens_with_native_chrome_and_saves_its_size() {
    sa_app::select_catalog_chain();
    let ui: Arc<dyn UiDispatcher> = sa_app::dispatcher::SlintDispatcher::new();
    let fakes = fake_deps(ui, vec!["Arial".into()], "tr");
    let settings = fakes.settings.clone();
    let host = SettingsWindowHost::new(fakes.deps.clone());
    host.show(Some("general")).expect("show");
    let h = host.clone();
    slint::Timer::single_shot(Duration::from_millis(1500), move || {
        h.show(Some("models")).expect("navigate");
        if cfg!(windows) {
            assert!(h.has_native_chrome(), "chrome attached once winit created the window");
        }
        // `SA_LIVE_HOLD_MS`: keep the window open longer (look checks); its outer rect is printed for a capture.
        let hold = std::env::var("SA_LIVE_HOLD_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(800);
        if let Some(w) = h.window() {
            use slint::ComponentHandle;
            let (p, s) = (w.window().position(), w.window().size());
            println!("SA_LIVE_RECT {} {} {} {}", p.x, p.y, s.width, s.height);
        }
        let h = h.clone();
        slint::Timer::single_shot(Duration::from_millis(hold), move || {
            h.close();
            let _ = slint::quit_event_loop();
        });
    });
    slint::run_event_loop().expect("event loop");
    assert!(!host.is_open());
    if cfg!(windows) {
        assert!(settings.get_int("settingsWindowWidth").is_some_and(|w| w > 0), "size saved on close");
    }
}
