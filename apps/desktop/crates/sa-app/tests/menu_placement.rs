//! Where menu flyouts land in the Windows main window, measured on rendered pixels (software renderer, no display):
//! a flyout's top-left is where its caller placed it, the whole menu stays inside the window (moved left/up only as
//! far as an edge needs) and out of the app-drawn caption band (48 DIP, where clicks drag the window), a menu taller
//! than the window scrolls, a submenu with no room on the right opens to the left, and the player's track menu opens
//! above the whole transport pill.
//!
//! Each test knows which menu it measures: the bridge callbacks record what the click requested (`menu-requested`,
//! `row-context`, `translation-model-clicked`) and the test asserts it, so a control moved away from the clicked
//! point fails instead of measuring another menu. The anchor is measured too: hovering a button repaints exactly its
//! rectangle. The coordinates are those of the Windows tree's layout.
#![cfg(sa_ui = "win")]

use std::cell::RefCell;
use std::rc::Rc;

use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, PointerEventButton, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, Model, ModelRc, PhysicalSize, Rgb8Pixel, VecModel};

use sa_app::main_window::controller::menu_entry;
use sa_app::main_window::menus::{self, RowMenuState};
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
/// `Theme.title-bar-height`: the caption band, plus the 8 px margin menus keep to it.
const CAPTION: i32 = 48;
const MARGIN: i32 = 8;

#[derive(Debug, Clone, Copy, PartialEq)]
struct Rect {
    left: i32,
    top: i32,
    /// Exclusive.
    right: i32,
    /// Exclusive.
    bottom: i32,
}

impl Rect {
    fn width(&self) -> i32 {
        self.right - self.left
    }
    fn height(&self) -> i32 {
        self.bottom - self.top
    }
}

/// Fills the entries for a menu id (`None` keeps the scene's entries).
type Entries = Box<dyn Fn(&str) -> Option<Vec<MenuEntry>>>;

struct Shot {
    window: Rc<MinimalSoftwareWindow>,
    ui: MainWindow,
    width: u32,
    height: u32,
    /// What the clicks asked the bridge for: menu ids, `row-context:<i>`, `translation-model`.
    requested: Rc<RefCell<Vec<String>>>,
    activated: Rc<RefCell<Vec<String>>>,
}

impl Shot {
    /// The real main window at `width` × `height` with the demo `scene` (explorer on, light theme); `entries` answers
    /// the menu requests like the controller would.
    fn new(scene: &str, width: u32, height: u32, entries: impl Fn(&str) -> Option<Vec<MenuEntry>> + 'static) -> Shot {
        let window = software_window();
        let ui = MainWindow::new().expect("main window");
        sa_app::loc::install(&ui);
        ui.global::<ui::Palette>().set_color_scheme(slint::language::ColorScheme::Light);
        let main = ui.global::<MainBridge>();
        main.set_show_file_explorer(true);
        sa_app::main_window::demo::fill(&ui, scene, false);
        main.set_window_width(width as f32);
        main.set_window_height(height as f32);
        let requested = Rc::new(RefCell::new(Vec::new()));
        let activated = Rc::new(RefCell::new(Vec::new()));
        let entries: Rc<Entries> = Rc::new(Box::new(entries));
        let fill = {
            let weak = ui.as_weak();
            let requested = requested.clone();
            let entries = entries.clone();
            move |id: String| {
                if let (Some(ui), Some(list)) = (weak.upgrade(), entries(&id)) {
                    ui.global::<MainBridge>().set_menu_entries(ModelRc::new(VecModel::from(list)));
                }
                requested.borrow_mut().push(id);
            }
        };
        let f = fill.clone();
        main.on_menu_requested(move |id| f(id.to_string()));
        let f = fill.clone();
        main.on_row_context(move |i| f(format!("row-context:{i}")));
        let r = requested.clone();
        main.on_translation_model_clicked(move || r.borrow_mut().push("translation-model".into()));
        let a = activated.clone();
        main.on_menu_activated(move |id| a.borrow_mut().push(id.to_string()));
        ui.show().expect("show");
        window.set_size(PhysicalSize::new(width, height));
        Shot { window, ui, width, height, requested, activated }
    }

    #[track_caller]
    fn assert_requested(&self, id: &str) {
        assert_eq!(self.requested.borrow().last().map(String::as_str), Some(id), "the click asked for another menu");
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

    fn pixel(&self, frame: &[Rgb8Pixel], x: i32, y: i32) -> Rgb8Pixel {
        frame[(y as u32 * self.width + x as u32) as usize]
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

    fn scroll(&self, x: f32, y: f32, delta_y: f32) {
        self.window.dispatch_event(WindowEvent::PointerScrolled { position: LogicalPosition::new(x, y), delta_x: 0.0, delta_y });
    }

    /// The rectangle of the control under (x, y): hovering repaints its fill. Only rows within 19 px of the point
    /// count, so a tooltip that a slow render lets appear (36 px below the control's top) stays out.
    fn control_at(&self, x: f32, y: f32) -> Rect {
        self.hover(-10.0, -10.0);
        let idle = self.render();
        self.hover(x, y);
        let hovered = self.render();
        let band = (y as i32 - 19)..=(y as i32 + 19);
        bounds(self.width, self.height, &idle, &hovered, |p| band.contains(&p.1), |_| true).expect("the control repaints on hover")
    }

    /// The flyout that `open` shows: the pixels it turned into the flyout surface, grown by the 1 px border. The
    /// pointer starts away from everything (an open popup takes the hover from the control under it).
    fn menu_after(&self, open: impl FnOnce(&Shot)) -> Rect {
        self.hover(-10.0, -10.0);
        let before = self.render();
        open(self);
        self.surface_change(&before, "a flyout opened")
    }

    /// The flyout surface that appeared since `before`.
    fn surface_change(&self, before: &[Rgb8Pixel], what: &str) -> Rect {
        let after = self.render();
        if let Ok(dir) = std::env::var("MENU_DUMP") {
            for (name, b) in [("before", before), ("after", &after[..])] {
                let bytes: Vec<u8> = b.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
                let thread = std::thread::current().name().unwrap_or("t").replace("::", "-");
                let path = format!("{dir}/{thread}-{}x{}-{what}-{name}.png", self.width, self.height).replace(' ', "_");
                image::save_buffer(path, &bytes, self.width, self.height, image::ColorType::Rgb8).unwrap();
            }
        }
        let inner = bounds(self.width, self.height, before, &after, |_| true, |p| p == FLYOUT).expect(what);
        Rect { left: inner.left - 1, top: inner.top - 1, right: inner.right + 1, bottom: inner.bottom + 1 }
    }

    /// Inside the window and below the caption band (with the menus' margin). A menu cut by an edge shows its
    /// surface up to the window's last pixel, which puts the measured border 1 px outside the window.
    #[track_caller]
    fn assert_reachable(&self, menu: Rect) {
        assert!(
            menu.left >= 0 && menu.right <= self.width as i32 && menu.bottom <= self.height as i32,
            "the menu {menu:?} is not inside the {}×{} window",
            self.width,
            self.height
        );
        assert!(menu.top >= CAPTION + MARGIN - 1, "the menu {menu:?} reaches into the caption band (0..{CAPTION})");
    }
}

impl Drop for Shot {
    fn drop(&mut self) {
        let _ = self.ui.hide();
    }
}

fn bounds(
    width: u32,
    height: u32,
    before: &[Rgb8Pixel],
    after: &[Rgb8Pixel],
    place: impl Fn((i32, i32)) -> bool,
    color: impl Fn(Rgb8Pixel) -> bool,
) -> Option<Rect> {
    let mut r: Option<Rect> = None;
    for y in 0..height as i32 {
        for x in 0..width as i32 {
            let i = (y as u32 * width + x as u32) as usize;
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

/// The controller's menus for the ids these tests click.
fn app_menus(id: &str) -> Option<Vec<MenuEntry>> {
    let entries = match id {
        "explorer-quick" => return Some(quick_entries()),
        "speakers" => menus::speakers_menu(true, 3),
        _ => return None,
    };
    Some(entries.into_iter().map(menu_entry).collect())
}

/// The line row menu with `speakers` speakers (Windows order: retranslate, speakers, split / merge / delete).
fn row_entries(row: usize, speakers: i64) -> Vec<MenuEntry> {
    let st = RowMenuState { can_retranslate: true, can_split: true, can_merge: true, can_delete: true, shows_speakers: true, is_working: false };
    let ids: Vec<i64> = (0..speakers).collect();
    let items = if speakers > 0 { menus::speaker_items(row, &ids, Some(0), false, false) } else { Vec::new() };
    menus::row_menu(row, st, items, false).into_iter().map(menu_entry).collect()
}

fn rows_answer(speakers: i64) -> impl Fn(&str) -> Option<Vec<MenuEntry>> {
    move |id| id.strip_prefix("row-context:").and_then(|r| r.parse().ok()).map(|r| row_entries(r, speakers))
}

/// Drawn height of a Windows menu: 32 px rows, 28 px section captions, 9 px separators, 4 px padding at both ends.
fn menu_height(entries: &[MenuEntry]) -> i32 {
    8 + entries.iter().filter(|e| e.level == 0).map(|e| if e.separator { 9 } else if e.header { 28 } else { 32 }).sum::<i32>()
}

/// The quick-locations button (explorer top bar): its menu opens below it, flush with its left edge (240 px wide).
fn check_quick_locations(width: u32, height: u32) {
    let s = Shot::new("editor", width, height, app_menus);
    let button = s.control_at(80.0, 162.0);
    let menu = s.menu_after(|s| s.click(80.0, 162.0, PointerEventButton::Left));
    s.assert_requested("explorer-quick");
    near(menu.left, button.left, "menu left (button left)");
    near(menu.top, button.bottom + 4, "menu top (4 px under the button)");
    near(menu.width(), 240, "menu width (min-menu-width)");
    near(menu.height(), menu_height(&quick_entries()), "menu height (all rows)");
    s.assert_reachable(menu);
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
        let s = Shot::new("editor", width, height, app_menus);
        let button = s.control_at(x, 64.0);
        let menu = s.menu_after(|s| s.click(x, 64.0, PointerEventButton::Left));
        s.assert_requested("speakers");
        near(menu.left, button.left, "menu left (button left)");
        near(menu.top, button.bottom + 4, "menu top");
        s.assert_reachable(menu);
    }
}

fn row_menu(s: &Shot, row: usize, speakers: i64, x: f32, y: f32) -> Rect {
    // A right click on a line row outside its text fields (the time column, a row's bottom padding).
    let menu = s.menu_after(|s| s.click(x, y, PointerEventButton::Right));
    s.assert_requested(&format!("row-context:{row}"));
    // 240 px wide (the line editor's min-menu-width).
    near(menu.width(), 240, "menu width");
    let full = menu_height(&row_entries(row, speakers));
    let room = s.height as i32 - CAPTION - MARGIN;
    near(menu.height(), full.min(room), "menu height (all rows, or the room under the caption band)");
    s.assert_reachable(menu);
    menu
}

#[test]
fn a_row_context_menu_near_the_bottom_edge_moves_up_into_the_window() {
    // Row 4 (index 3) at the default size, row 2 (index 1) at the minimum size; two speakers: 291 px.
    for (width, height, row, x, y) in [(1240, 840, 3, 490.0, 775.0), (760, 580, 1, 480.0, 515.0)] {
        let s = Shot::new("editor", width, height, rows_answer(2));
        let menu = row_menu(&s, row, 2, x, y);
        near(menu.left, x as i32, "menu left (pointer)");
        near(menu.bottom, height as i32, "menu bottom (window edge)");
    }
}

#[test]
fn a_row_context_menu_near_the_right_edge_moves_left_into_the_window() {
    // In a row's 4 px bottom padding, under its translation field (the fields and the scroll bar take the clicks
    // further right).
    for (width, height, row, x, y) in [(1240, 840, 2, 1200.0, 736.0), (760, 580, 0, 720.0, 495.0)] {
        let s = Shot::new("editor", width, height, rows_answer(0));
        let menu = row_menu(&s, row, 0, x, y);
        near(menu.right, width as i32, "menu right (window edge)");
        // At the minimum size the menu does not fit under the pointer either.
        near(menu.top, (y as i32).min(height as i32 - menu.height()), "menu top (pointer, or up to the bottom edge)");
    }
}

#[test]
fn a_row_context_menu_with_room_opens_at_the_pointer() {
    let s = Shot::new("editor", 1240, 840, rows_answer(0));
    let menu = row_menu(&s, 1, 0, 480.0, 640.0);
    near(menu.left, 480, "menu left (pointer)");
    near(menu.top, 640, "menu top (pointer)");
}

#[test]
fn a_row_menu_taller_than_the_window_scrolls_below_the_caption_band_and_every_row_can_be_clicked() {
    // 14 speakers: 675 px of rows, more than the 580 px window holds.
    let (width, height, row, x, y) = (760, 580, 1, 480.0, 515.0);
    let entries = row_entries(row, 14);
    assert!(menu_height(&entries) > height as i32, "the premise: the menu is taller than the window");
    let s = Shot::new("editor", width, height, rows_answer(14));
    let menu = row_menu(&s, row, 14, x, y);
    near(menu.top, CAPTION + MARGIN, "menu top (just under the caption band)");
    near(menu.bottom, height as i32, "menu bottom (window edge)");
    let mid = (menu.left + menu.width() / 2) as f32;
    // The first row (retranslate) is clickable at the top.
    s.click(mid, (menu.top + 4 + 16) as f32, PointerEventButton::Left);
    assert_eq!(s.activated.borrow().last().map(String::as_str), Some(entries[0].id.as_str()), "the first row is under the pointer");
    // Open again, scroll to the end: the last row (Sil) is clickable at the bottom.
    let again = s.menu_after(|s| s.click(x, y, PointerEventButton::Right));
    assert_eq!(again, menu, "the menu opens at the same place");
    s.scroll(mid, (menu.top + menu.height() / 2) as f32, -10000.0);
    s.render();
    s.click(mid, (menu.bottom - 4 - 16) as f32, PointerEventButton::Left);
    let last = entries.iter().rev().find(|e| !e.separator && e.level == 0).unwrap();
    assert_eq!(s.activated.borrow().last().map(String::as_str), Some(last.id.as_str()), "the last row is reachable by scrolling");
}

#[test]
fn a_submenu_opens_beside_its_row_and_to_the_left_when_the_window_has_no_room_on_the_right() {
    // The speakers drop-down's "Konuşmacı sayısı" row (second row) opens the count submenu (six entries). At the
    // minimum size the menu ends 124 px before the window's right edge: no room for the submenu there.
    for (width, height, x, room_on_right) in [(1240, 840, 680.0, true), (760, 580, 440.0, false)] {
        let s = Shot::new("editor", width, height, app_menus);
        let menu = s.menu_after(|s| s.click(x, 64.0, PointerEventButton::Left));
        s.assert_requested("speakers");
        let row_y = menu.top + 4 + 32 + 16;
        let pointer = (menu.right - 30, row_y);
        let open = s.render();
        s.hover(pointer.0 as f32, pointer.1 as f32);
        let sub = s.surface_change(&open, "a submenu opened");
        near(sub.top, menu.top + 4 + 32, "submenu top (its row)");
        near(sub.height(), 8 + 6 * 32, "submenu height (six counts)");
        if room_on_right {
            assert!((sub.left - (menu.right - 6)).abs() <= 2, "submenu {sub:?} not at the menu's trailing edge {}", menu.right);
        } else {
            assert!(menu.right - 6 + sub.width() > width as i32, "the premise: no room on the right of {menu:?}");
            assert!((sub.right - (menu.left + 6)).abs() <= 2, "submenu {sub:?} not at the menu's leading edge {}", menu.left);
        }
        assert!(!(sub.left..sub.right).contains(&pointer.0), "the submenu {sub:?} lies under the pointer at {pointer:?}");
        assert!(sub.left >= 0 && sub.right <= width as i32, "the submenu {sub:?} leaves the window");
    }
}

#[test]
fn the_translation_model_flyout_opens_under_its_button_shifted_left_to_the_window_edge() {
    for (width, height) in [(1240, 840), (760, 580)] {
        // The demo picker scene opens the flyout once laid out; close it, then open it as the controller does
        // after the button asked for it.
        let s = Shot::new("picker-local", width, height, |_| None);
        s.render();
        let main = s.ui.global::<MainBridge>();
        main.set_picker_close_epoch(main.get_picker_close_epoch() + 1);
        let x = (width - 100) as f32;
        let button = s.control_at(x, 64.0);
        let menu = s.menu_after(|s| {
            s.click(x, 64.0, PointerEventButton::Left);
            s.assert_requested("translation-model");
            let main = s.ui.global::<MainBridge>();
            main.set_picker_open_epoch(main.get_picker_open_epoch() + 1);
        });
        // 372 px wide (340 + padding); the button sits too far right for that, so the flyout meets the right edge.
        near(menu.width(), 372, "flyout width");
        near(menu.left, button.left.min(width as i32 - 372), "flyout left (button left, or shifted to the edge)");
        near(menu.top, button.bottom + 4, "flyout top (4 px under the button)");
        s.assert_reachable(menu);
    }
}

/// From the tracks button's top up to the transport pill's top: the pill's padding-top 10, its upper row 18 (elapsed
/// time, scrubber, total time), spacing 6 and the button's 4 px inside its 34 px row (TransportHud in player.slint).
const BUTTON_IN_PILL: i32 = 38;

fn check_track_menu(scene: &str) {
    for (width, height, x, y) in [(1240, 840, 626.0, 455.0), (760, 580, 386.0, 310.0)] {
        // The demo track scenes fill the menu (two sections) and show the button.
        let s = Shot::new(scene, width, height, |_| None);
        let button = s.control_at(x, y);
        s.hover(-10.0, -10.0);
        let closed = s.render();
        let pill = button.top - BUTTON_IN_PILL;
        let menu = s.menu_after(|s| s.click(x, y, PointerEventButton::Left));
        s.assert_requested("tracks");
        near(menu.left, button.left, "menu left (button left)");
        near(menu.width(), 380, "menu width (min-menu-width)");
        near(menu.bottom, pill - 8, "menu bottom (8 px above the pill)");
        // The pill's upper row (elapsed time, scrubber, total time) stays uncovered: every pixel from the pill's top
        // to the button row is unchanged.
        s.hover(-10.0, -10.0);
        let open = s.render();
        for py in pill..button.top {
            for px in menu.left..menu.right {
                assert_eq!(s.pixel(&closed, px, py), s.pixel(&open, px, py), "the menu {menu:?} covers the pill's scrubber row at ({px}, {py})");
            }
        }
        s.assert_reachable(menu);
    }
}

#[test]
fn the_player_track_menu_opens_above_the_transport_pill() {
    check_track_menu("tracks");
}

#[test]
fn the_open_track_menu_stays_above_the_pill_when_its_entries_change() {
    // The controller replaces the entries while the menu is open (a track read finishes, the player switches):
    // the menu is placed from its new height, its bottom stays 8 px above the pill.
    let (x, y) = (626.0, 455.0);
    let s = Shot::new("tracks", 1240, 840, |_| None);
    let button = s.control_at(x, y);
    let pill = button.top - BUTTON_IN_PILL;
    s.hover(-10.0, -10.0);
    let closed = s.render();
    s.click(x, y, PointerEventButton::Left);
    s.render();
    let main = s.ui.global::<MainBridge>();
    let rows = main.get_menu_entries();
    let fewer: Vec<MenuEntry> = (0..4).filter_map(|i| rows.row_data(i)).collect();
    main.set_menu_entries(ModelRc::new(VecModel::from(fewer.clone())));
    s.hover(-10.0, -10.0);
    let open = s.render();
    let inner = bounds(s.width, s.height, &closed, &open, |_| true, |p| p == FLYOUT).expect("the menu is open");
    let menu = Rect { left: inner.left - 1, top: inner.top - 1, right: inner.right + 1, bottom: inner.bottom + 1 };
    near(menu.height(), menu_height(&fewer), "menu height (the four entries left)");
    near(menu.bottom, pill - 8, "menu bottom (8 px above the pill)");
}

#[test]
fn a_long_track_menu_scrolls_above_the_pill_and_below_the_caption_band() {
    // 3 audio and 54 subtitle tracks: far taller than the space above the pill.
    check_track_menu("tracks-many");
    let s = Shot::new("tracks-many", 1240, 840, |_| None);
    let rows = s.ui.global::<MainBridge>().get_menu_entries();
    let entries: Vec<MenuEntry> = (0..rows.row_count()).filter_map(|i| rows.row_data(i)).collect();
    assert!(menu_height(&entries) > 840, "the premise: the menu is taller than the window");
}
