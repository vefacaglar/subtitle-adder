//! Where menu flyouts land in the Windows main window, measured on rendered pixels (software renderer, no display):
//! a flyout's top-left is where its caller placed it, and the whole menu stays inside the window (moved left/up
//! only as far as an edge needs). The anchor is measured too: hovering a button repaints exactly its rectangle.
//! The coordinates are those of the Windows tree's layout.
#![cfg(sa_ui = "win")]

use std::cell::RefCell;
use std::rc::Rc;

use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, PointerEventButton, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, ModelRc, PhysicalSize, Rgb8Pixel, VecModel};

use sa_app::ui::{self, MainBridge, MainWindow, MenuEntry};

struct TestPlatform(Rc<MinimalSoftwareWindow>);

impl Platform for TestPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

fn software_window() -> Rc<MinimalSoftwareWindow> {
    // Slint's platform is per thread; every test thread installs its own once.
    thread_local!(static WINDOW: RefCell<Option<Rc<MinimalSoftwareWindow>>> = const { RefCell::new(None) });
    WINDOW.with(|w| {
        w.borrow_mut()
            .get_or_insert_with(|| {
                let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
                slint::platform::set_platform(Box::new(TestPlatform(window.clone()))).expect("platform");
                window
            })
            .clone()
    })
}

/// `Theme.flyout-background` (light): the menu surface, opaque.
const FLYOUT: Rgb8Pixel = Rgb8Pixel { r: 0xf9, g: 0xf9, b: 0xf9 };

#[derive(Debug, Clone, Copy, PartialEq)]
struct Rect {
    left: i32,
    top: i32,
    /// Exclusive.
    right: i32,
    /// Exclusive.
    bottom: i32,
}

struct Shot {
    window: Rc<MinimalSoftwareWindow>,
    ui: MainWindow,
    width: u32,
    height: u32,
}

impl Shot {
    /// The real main window at `width` × `height` with the demo `scene` (explorer on, light theme).
    fn new(scene: &str, width: u32, height: u32) -> Shot {
        let window = software_window();
        let ui = MainWindow::new().expect("main window");
        sa_app::loc::install(&ui);
        ui.global::<ui::Palette>().set_color_scheme(slint::language::ColorScheme::Light);
        let main = ui.global::<MainBridge>();
        main.set_show_file_explorer(true);
        sa_app::main_window::demo::fill(&ui, scene, false);
        main.set_window_width(width as f32);
        ui.show().expect("show");
        window.set_size(PhysicalSize::new(width, height));
        Shot { window, ui, width, height }
    }

    fn entries(&self, entries: Vec<MenuEntry>) {
        self.ui.global::<MainBridge>().set_menu_entries(ModelRc::new(VecModel::from(entries)));
    }

    fn render(&self) -> Vec<Rgb8Pixel> {
        let mut buffer = vec![Rgb8Pixel { r: 0, g: 0, b: 0 }; (self.width * self.height) as usize];
        // Two passes: the first lays out and may schedule animations; the second paints the settled frame.
        for _ in 0..2 {
            slint::platform::update_timers_and_animations();
            self.window.request_redraw();
            self.window.draw_if_needed(|renderer| {
                renderer.render(&mut buffer, self.width as usize);
            });
        }
        buffer
    }

    fn hover(&self, x: f32, y: f32) {
        self.window.dispatch_event(WindowEvent::PointerMoved { position: LogicalPosition::new(x, y) });
    }

    fn click(&self, x: f32, y: f32, button: PointerEventButton) {
        let position = LogicalPosition::new(x, y);
        self.window.dispatch_event(WindowEvent::PointerMoved { position });
        self.window.dispatch_event(WindowEvent::PointerPressed { position, button });
        self.window.dispatch_event(WindowEvent::PointerReleased { position, button });
    }

    /// The rectangle of the control under (x, y): hovering repaints its fill. Only rows within 19 px of the point
    /// count, so a tooltip that a slow render lets appear (36 px below the control's top) stays out.
    fn control_at(&self, x: f32, y: f32) -> Rect {
        self.hover(-10.0, -10.0);
        let idle = self.render();
        self.hover(x, y);
        let hovered = self.render();
        let band = (y as i32 - 19)..=(y as i32 + 19);
        self.bounds(&idle, &hovered, |p| p.1 >= *band.start() && p.1 <= *band.end(), |_| true).expect("the control repaints on hover")
    }

    /// The flyout that `open` shows: the pixels it turned into the flyout surface, grown by the 1 px border.
    fn menu_after(&self, open: impl FnOnce(&Shot)) -> Rect {
        // The pointer away from everything: an open popup takes the hover from the control under it, so the frame
        // before must show that control unhovered too.
        self.hover(-10.0, -10.0);
        let before = self.render();
        open(self);
        let after = self.render();
        if let Ok(dir) = std::env::var("MENU_DUMP") {
            for (name, b) in [("before", &before), ("after", &after)] {
                let bytes: Vec<u8> = b.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
                let path = format!("{dir}/{}-{}x{}-{name}.png", std::thread::current().name().unwrap_or("t"), self.width, self.height);
                image::save_buffer(path, &bytes, self.width, self.height, image::ColorType::Rgb8).unwrap();
            }
        }
        let inner = self.bounds(&before, &after, |_| true, |p| p == FLYOUT).expect("a flyout opened");
        Rect { left: inner.left - 1, top: inner.top - 1, right: inner.right + 1, bottom: inner.bottom + 1 }
    }

    fn bounds(
        &self,
        before: &[Rgb8Pixel],
        after: &[Rgb8Pixel],
        place: impl Fn((i32, i32)) -> bool,
        color: impl Fn(Rgb8Pixel) -> bool,
    ) -> Option<Rect> {
        let mut r: Option<Rect> = None;
        for y in 0..self.height as i32 {
            for x in 0..self.width as i32 {
                let i = (y as u32 * self.width + x as u32) as usize;
                if before[i] == after[i] || !place((x, y)) || !color(after[i]) {
                    continue;
                }
                let b = r.get_or_insert(Rect { left: x, top: y, right: x + 1, bottom: y + 1 });
                b.left = b.left.min(x);
                b.top = b.top.min(y);
                b.right = b.right.max(x + 1);
                b.bottom = b.bottom.max(y + 1);
            }
        }
        r
    }

    #[track_caller]
    fn assert_inside(&self, menu: Rect) {
        // A menu cut by an edge shows its surface up to the window's last pixel, which puts the measured border 1 px
        // outside the window.
        assert!(
            menu.left >= 0 && menu.top >= 0 && menu.right <= self.width as i32 && menu.bottom <= self.height as i32,
            "the menu {menu:?} is not inside the {}×{} window",
            self.width,
            self.height
        );
    }
}

impl Drop for Shot {
    fn drop(&mut self) {
        let _ = self.ui.hide();
    }
}

#[track_caller]
fn near(actual: i32, expected: i32, what: &str) {
    assert!((actual - expected).abs() <= 1, "{what}: {actual}, expected {expected}");
}

fn item(id: &str, title: &str) -> MenuEntry {
    MenuEntry { id: id.into(), title: title.into(), enabled: true, ..Default::default() }
}

fn quick_entries() -> Vec<MenuEntry> {
    ["Masaüstü", "Belgeler", "İndirilenler", "Videolar", "Müzik", "Yerel Disk (C:)", "Yedek 2tb (D:)"]
        .iter()
        .enumerate()
        .map(|(i, t)| item(&format!("quick:{i}"), t))
        .collect()
}

/// The quick-locations button (explorer top bar): its menu opens below it, flush with its left edge (240 px wide).
fn check_quick_locations(width: u32, height: u32) {
    let s = Shot::new("editor", width, height);
    s.entries(quick_entries());
    let button = s.control_at(80.0, 162.0);
    let menu = s.menu_after(|s| s.click(80.0, 162.0, PointerEventButton::Left));
    near(menu.left, button.left, "menu left (button left)");
    near(menu.top, button.bottom + 4, "menu top (4 px under the button)");
    near(menu.right - menu.left, 240, "menu width (min-menu-width)");
    s.assert_inside(menu);
}

#[test]
fn the_quick_locations_menu_opens_at_its_button_inside_the_window() {
    check_quick_locations(1240, 840);
}

#[test]
fn the_quick_locations_menu_opens_at_its_button_at_the_minimum_window_size() {
    check_quick_locations(760, 580);
}

#[test]
fn a_toolbar_drop_down_opens_flush_with_its_button() {
    for (width, height, x) in [(1240, 840, 680.0), (760, 580, 440.0)] {
        // "Konuşmacılar" (speakers): a `DropDownButton`, menu at the button's left edge, 4 px under it.
        let s = Shot::new("editor", width, height);
        s.entries(vec![item("diarize", "Konuşmacıları ayır"), MenuEntry { separator: true, ..Default::default() }, item("speakers:2", "2 kişi")]);
        let button = s.control_at(x, 64.0);
        let menu = s.menu_after(|s| s.click(x, 64.0, PointerEventButton::Left));
        near(menu.left, button.left, "menu left (button left)");
        near(menu.top, button.bottom + 4, "menu top");
        s.assert_inside(menu);
    }
}

fn row_menu(s: &Shot, items: usize, x: f32, y: f32) -> Rect {
    s.entries((0..items).map(|i| item(&format!("row:{i}"), &format!("Komut {i}"))).collect());
    // A right click on a line row outside its text fields (the time column, the space after the translation).
    let menu = s.menu_after(|s| s.click(x, y, PointerEventButton::Right));
    // 240 px wide (the line editor's min-menu-width), 32 px per item plus 8 px padding.
    near(menu.right - menu.left, 240, "menu width");
    near(menu.bottom - menu.top, items as i32 * 32 + 8, "menu height");
    s.assert_inside(menu);
    menu
}

#[test]
fn a_row_context_menu_near_the_bottom_edge_moves_up_into_the_window() {
    for (width, height, x, y) in [(1240, 840, 490.0, 775.0), (760, 580, 480.0, 515.0)] {
        let s = Shot::new("editor", width, height);
        let menu = row_menu(&s, 8, x, y);
        near(menu.left, x as i32, "menu left (pointer)");
        near(menu.bottom, height as i32, "menu bottom (window edge)");
    }
}

#[test]
fn a_row_context_menu_near_the_right_edge_moves_left_into_the_window() {
    // In a row's 4 px bottom padding, under its translation field (the fields and the scroll bar take the clicks
    // further right).
    for (width, height, x, y) in [(1240, 840, 1200.0, 736.0), (760, 580, 720.0, 495.0)] {
        let s = Shot::new("editor", width, height);
        let menu = row_menu(&s, 3, x, y);
        near(menu.right, width as i32, "menu right (window edge)");
        // At the minimum size the menu does not fit under the pointer either.
        near(menu.top, (y as i32).min(height as i32 - (menu.bottom - menu.top)), "menu top (pointer, or up to the bottom edge)");
    }
}

#[test]
fn a_row_context_menu_with_room_opens_at_the_pointer() {
    let s = Shot::new("editor", 1240, 840);
    let menu = row_menu(&s, 3, 480.0, 640.0);
    near(menu.left, 480, "menu left (pointer)");
    near(menu.top, 640, "menu top (pointer)");
}

#[test]
fn the_translation_model_flyout_opens_under_its_button_shifted_left_to_the_window_edge() {
    for (width, height) in [(1240, 840), (760, 580)] {
        // The demo picker scene opens the flyout once laid out; close it to measure it opening.
        let s = Shot::new("picker-local", width, height);
        s.render();
        let main = s.ui.global::<MainBridge>();
        main.set_picker_close_epoch(main.get_picker_close_epoch() + 1);
        let menu = s.menu_after(|s| {
            let main = s.ui.global::<MainBridge>();
            main.set_picker_open_epoch(main.get_picker_open_epoch() + 1);
        });
        // 372 px wide (340 + padding); the button sits too far right for that, so the flyout meets the right edge.
        near(menu.right - menu.left, 372, "flyout width");
        near(menu.right, width as i32, "flyout right (window edge)");
        near(menu.top, 84, "flyout top (4 px under the toolbar button)");
        s.assert_inside(menu);
    }
}

#[test]
fn the_player_track_menu_opens_above_its_button_from_its_left_edge() {
    for (width, height, x, y) in [(1240, 840, 626.0, 455.0), (760, 580, 386.0, 310.0)] {
        // The demo "tracks" scene fills the menu (two sections) and shows the button.
        let s = Shot::new("tracks", width, height);
        let button = s.control_at(x, y);
        let menu = s.menu_after(|s| s.click(x, y, PointerEventButton::Left));
        near(menu.left, button.left, "menu left (button left)");
        near(menu.bottom, button.top - 12, "menu bottom (12 px above the button)");
        near(menu.right - menu.left, 380, "menu width (min-menu-width)");
        s.assert_inside(menu);
    }
}
