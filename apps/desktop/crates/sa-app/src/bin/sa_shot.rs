//! Renders a window of the compiled UI tree to PNG with Slint's software renderer (no GPU, no visible window), for
//! look checks of both styles on any machine (ARCHITECTURE.md §10).
//!
//! `sa-shot --out shots/win-fluent.png [--window main|settings|symbols] [--workspace editor|platform|playlist]
//!          [--width N --height N] [--scale F] [--dark] [--lang tr|en] [--explorer on|off] [--scene <name>] [--hub <scene>]`
//!
//! Build the macOS look on Windows with `SA_UI_PLATFORM=mac cargo run -p sa-app --features all-catalogs --bin sa-shot`.
//! `--window symbols` (mac tree off macOS only: `preview.slint`'s `SymbolGallery`) draws every SF Symbol stand-in.

use std::rc::Rc;

use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter};
use slint::{ComponentHandle, PhysicalSize, Rgb8Pixel};

use sa_app::ui;
use sa_core::l10n::{LanguageState, UiLanguage};

struct ShotPlatform {
    window: Rc<MinimalSoftwareWindow>,
}

impl Platform for ShotPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }
}

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let out = arg(&args, "--out").unwrap_or_else(|| format!("shots/{}-{}.png", sa_app::UI_TREE, sa_app::UI_STYLE));
    let which = arg(&args, "--window").unwrap_or_else(|| "main".into());
    let scale: f32 = arg(&args, "--scale").and_then(|v| v.parse().ok()).unwrap_or(1.0);
    let default = if which == "settings" {
        if sa_app::UI_TREE == "mac" { (560, 620) } else { (860, 760) }
    } else if which == "symbols" {
        (GALLERY_COLUMNS * 196 + 16, sa_app::symbols::svg::SYMBOLS.len().div_ceil(GALLERY_COLUMNS as usize) as u32 * 44 + 16)
    } else if sa_app::UI_TREE == "mac" {
        (1180, 720)
    } else {
        (1240, 840)
    };
    let width: u32 = arg(&args, "--width").and_then(|v| v.parse().ok()).unwrap_or(default.0);
    let height: u32 = arg(&args, "--height").and_then(|v| v.parse().ok()).unwrap_or(default.1);
    let dark = args.iter().any(|a| a == "--dark");
    let lang = arg(&args, "--lang").and_then(|l| UiLanguage::parse(&l)).unwrap_or(UiLanguage::Tr);

    sa_app::select_catalog_chain();
    LanguageState::shared().set_language(lang);

    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(ShotPlatform { window: window.clone() })).expect("platform");

    let (pw, ph) = ((width as f32 * scale) as u32, (height as f32 * scale) as u32);
    window.dispatch_event(slint::platform::WindowEvent::ScaleFactorChanged { scale_factor: scale });

    let keep: Box<dyn std::any::Any> = if which == "symbols" {
        symbol_gallery(dark)
    } else if which == "settings" {
        // ui-settings: the real Settings window over in-memory fakes (`--page`, `--scene`; settings_window::shot).
        let page = arg(&args, "--page").unwrap_or_else(|| "general".into());
        let shot = sa_app::settings_window::shot::open(&page, arg(&args, "--scene").as_deref(), lang.code(), dark);
        let w = shot.host.window().expect("settings window");
        apply_scheme(&w.global::<ui::Palette>(), dark);
        Box::new((w, shot))
    } else {
        let w = ui::MainWindow::new().expect("main window");
        sa_app::loc::install(&w);
        sa_app::symbols::install(&w);
        apply_scheme(&w.global::<ui::Palette>(), dark);
        let main = w.global::<ui::MainBridge>();
        main.set_status_text(sa_core::l("Hazır").into());
        main.set_playlist_count(0);
        if let Some(e) = arg(&args, "--explorer") {
            main.set_show_file_explorer(e != "off");
        }
        match arg(&args, "--workspace").as_deref() {
            Some("platform") => main.set_workspace(ui::Workspace::Platform),
            Some("playlist") => main.set_workspace(ui::Workspace::Playlist),
            _ => {}
        }
        // `--scene <name>`: representative main-window content without any view model (main_window::demo).
        if let Some(scene) = arg(&args, "--scene") {
            sa_app::main_window::demo::fill(&w, &scene, sa_app::UI_TREE == "mac");
        }
        // `--hub <scene>`: representative hub content (profile, cards, flyout, engagement bar; hub::demo).
        if let Some(scene) = arg(&args, "--hub") {
            sa_app::hub::demo::fill(&w, &scene);
        }
        main.set_window_width(width as f32);
        w.show().expect("show");
        Box::new(w)
    };

    window.set_size(PhysicalSize::new(pw, ph));
    // `--click x,y` (logical px): a left click there before the shot, e.g. to open a menu or flyout.
    if let Some((x, y)) = arg(&args, "--click").and_then(|v| {
        let (x, y) = v.split_once(',')?;
        Some((x.trim().parse::<f32>().ok()?, y.trim().parse::<f32>().ok()?))
    }) {
        render_to(&window, pw, ph, &out);
        let position = slint::LogicalPosition::new(x, y);
        let button = slint::platform::PointerEventButton::Left;
        window.dispatch_event(slint::platform::WindowEvent::PointerMoved { position });
        window.dispatch_event(slint::platform::WindowEvent::PointerPressed { position, button });
        window.dispatch_event(slint::platform::WindowEvent::PointerReleased { position, button });
    }
    render_to(&window, pw, ph, &out);

    // `--relang en`: switch the UI language, bump `Loc.revision` like the app does, render again (`<out>-relang.png`).
    // Proves that every `Loc.t` binding re-evaluates without recreating the window.
    if let Some(next) = arg(&args, "--relang").and_then(|l| UiLanguage::parse(&l)) {
        LanguageState::shared().set_language(next);
        let rev = LanguageState::shared().revision() as i32;
        if let Some(w) = keep.downcast_ref::<ui::MainWindow>() {
            w.global::<ui::Loc>().set_revision(rev);
            w.global::<ui::MainBridge>().set_status_text(sa_core::l("Hazır").into());
        } else if let Some((w, shot)) = keep.downcast_ref::<(ui::SettingsWindow, sa_app::settings_window::shot::Shot)>() {
            w.global::<ui::Loc>().set_revision(rev);
            shot.pump();
        }
        render_to(&window, pw, ph, &out.replace(".png", "-relang.png"));
    }
    drop(keep);
}

fn render_to(window: &MinimalSoftwareWindow, pw: u32, ph: u32, out: &str) {
    let mut buffer = vec![Rgb8Pixel { r: 0, g: 0, b: 0 }; (pw * ph) as usize];
    // Two passes: the first lays out and may schedule animations; the second paints the settled frame.
    for _ in 0..2 {
        slint::platform::update_timers_and_animations();
        window.request_redraw();
        window.draw_if_needed(|renderer| {
            renderer.render(&mut buffer, pw as usize);
        });
    }
    let bytes: Vec<u8> = buffer.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
    if let Some(parent) = std::path::Path::new(out).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    image::save_buffer(out, &bytes, pw, ph, image::ColorType::Rgb8).expect("write png");
    println!("{out} ({pw}x{ph}, {} / {})", sa_app::UI_TREE, sa_app::UI_STYLE);
}

const GALLERY_COLUMNS: u32 = 6;

/// Every vector stand-in at 24, 16 and 12 pt in the label tint (the macOS rasterizer draws the real SF Symbols).
#[cfg(all(sa_ui = "mac", not(target_os = "macos")))]
fn symbol_gallery(dark: bool) -> Box<dyn std::any::Any> {
    let w = ui::SymbolGallery::new().expect("symbol gallery");
    sa_app::symbols::install(&w);
    apply_scheme(&w.global::<ui::Palette>(), dark);
    let names: Vec<slint::SharedString> = sa_app::symbols::svg::SYMBOLS.iter().map(|(n, _)| (*n).into()).collect();
    w.set_names(Rc::new(slint::VecModel::from(names)).into());
    w.set_columns(GALLERY_COLUMNS as i32);
    w.show().expect("show");
    Box::new(w)
}

#[cfg(not(all(sa_ui = "mac", not(target_os = "macos"))))]
fn symbol_gallery(_dark: bool) -> Box<dyn std::any::Any> {
    eprintln!("--window symbols needs the mac preview tree: SA_UI_PLATFORM=mac on a non-mac host");
    std::process::exit(2);
}

fn apply_scheme(palette: &ui::Palette, dark: bool) {
    palette.set_color_scheme(if dark { slint::language::ColorScheme::Dark } else { slint::language::ColorScheme::Light });
}
