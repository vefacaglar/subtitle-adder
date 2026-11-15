//! The volume controls of the Windows transport pill take the pointer (software renderer window, no display): a press
//! on the speaker button toggles mute, a press or a drag on the slider sets the volume, and the wheel over either
//! (and between them) steps it. The wheel catcher must not lie over the two controls: it would take every press.
//!
//! The coordinates are those of the Windows tree's layout at 1240 × 840 with the demo `tracks` scene (the pill's lower
//! row, right of the play button).
#![cfg(sa_ui = "win")]

use std::cell::RefCell;
use std::rc::Rc;

use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, PointerEventButton, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, PhysicalSize, Rgb8Pixel};

use sa_app::ui::{self, MainBridge, MainWindow};

struct TestPlatform(Rc<MinimalSoftwareWindow>);

impl Platform for TestPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

const WIDTH: u32 = 1240;
const HEIGHT: u32 = 840;
/// The middle of the pill's lower row (tracks button, play circle, speaker button, volume slider).
const ROW_Y: f32 = 455.0;
/// From just right of the play circle to the pill's right edge.
const SCAN: std::ops::RangeInclusive<i32> = 790..=932;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Event {
    Mute,
    Scrub(bool, f32),
    Wheel(f32),
}

struct Pill {
    window: Rc<MinimalSoftwareWindow>,
    ui: MainWindow,
    events: Rc<RefCell<Vec<Event>>>,
}

impl Pill {
    fn new() -> Pill {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(TestPlatform(window.clone()))).expect("platform");
        let ui = MainWindow::new().expect("main window");
        sa_app::loc::install(&ui);
        ui.global::<ui::Palette>().set_color_scheme(slint::language::ColorScheme::Light);
        let main = ui.global::<MainBridge>();
        main.set_show_file_explorer(true);
        sa_app::main_window::demo::fill(&ui, "tracks", false);
        main.set_window_width(WIDTH as f32);
        main.set_window_height(HEIGHT as f32);
        let events = Rc::new(RefCell::new(Vec::new()));
        let e = events.clone();
        main.on_toggle_mute(move || e.borrow_mut().push(Event::Mute));
        let e = events.clone();
        main.on_volume_scrub(move |scrubbing, ratio| e.borrow_mut().push(Event::Scrub(scrubbing, ratio)));
        let e = events.clone();
        main.on_volume_wheel(move |delta| e.borrow_mut().push(Event::Wheel(delta)));
        ui.show().expect("show");
        window.set_size(PhysicalSize::new(WIDTH, HEIGHT));
        let pill = Pill { window, ui, events };
        // Lay out, and let the pill finish fading in (it takes no pointer while transparent).
        pill.render();
        std::thread::sleep(std::time::Duration::from_millis(300));
        pill.render();
        pill
    }

    fn render(&self) {
        let mut buffer = vec![Rgb8Pixel { r: 0, g: 0, b: 0 }; (WIDTH * HEIGHT) as usize];
        for _ in 0..2 {
            slint::platform::update_timers_and_animations();
            self.window.request_redraw();
            self.window.draw_if_needed(|renderer| {
                renderer.render(&mut buffer, WIDTH as usize);
            });
        }
    }

    fn at(x: i32) -> LogicalPosition {
        LogicalPosition::new(x as f32 + 0.5, ROW_Y)
    }

    fn moved(&self, x: i32) {
        self.window.dispatch_event(WindowEvent::PointerMoved { position: Self::at(x) });
    }

    fn press(&self, x: i32) {
        self.moved(x);
        self.window.dispatch_event(WindowEvent::PointerPressed { position: Self::at(x), button: PointerEventButton::Left });
    }

    fn release(&self, x: i32) {
        self.window.dispatch_event(WindowEvent::PointerReleased { position: Self::at(x), button: PointerEventButton::Left });
    }

    fn wheel(&self, x: i32, delta_y: f32) {
        self.moved(x);
        self.window.dispatch_event(WindowEvent::PointerScrolled { position: Self::at(x), delta_x: 0.0, delta_y });
    }

    /// What the bridge was asked since the last call.
    fn take(&self) -> Vec<Event> {
        std::mem::take(&mut *self.events.borrow_mut())
    }

    /// The columns of the row where a press toggles mute, and those where it starts a volume scrub (with the ratio).
    fn controls(&self) -> (Vec<i32>, Vec<(i32, f32)>) {
        let (mut mute, mut slider) = (Vec::new(), Vec::new());
        for x in SCAN {
            self.take();
            self.press(x);
            let pressed = self.take();
            self.release(x);
            match pressed.as_slice() {
                [] => {}
                [Event::Mute] => mute.push(x),
                [Event::Scrub(true, ratio)] => {
                    slider.push((x, *ratio));
                    assert_eq!(self.take(), [Event::Scrub(false, *ratio)], "the release at {x} ends the scrub");
                }
                other => panic!("a press at {x} asked for {other:?}"),
            }
        }
        (mute, slider)
    }
}

impl Drop for Pill {
    fn drop(&mut self) {
        let _ = self.ui.hide();
    }
}

fn contiguous(xs: &[i32]) -> bool {
    xs.windows(2).all(|w| w[1] == w[0] + 1)
}

#[test]
fn the_speaker_button_and_the_volume_slider_take_presses_and_the_wheel() {
    let pill = Pill::new();
    let (mute, slider) = pill.controls();

    // The speaker button: 26 px, toggles on press.
    assert!((25..=27).contains(&mute.len()) && contiguous(&mute), "a press on the speaker button toggles mute: {mute:?}");

    // The slider: 56 px right of it (6 px apart), the ratio follows the pointer from 0 to 1.
    let columns: Vec<i32> = slider.iter().map(|(x, _)| *x).collect();
    assert!((55..=57).contains(&columns.len()) && contiguous(&columns), "a press on the slider sets the volume: {slider:?}");
    assert!((5..=7).contains(&(columns[0] - mute[mute.len() - 1] - 1)), "the slider follows the button: {mute:?} {columns:?}");
    assert!(slider.windows(2).all(|w| w[1].1 > w[0].1), "the ratio grows to the right: {slider:?}");
    let (first, last) = (slider[0], slider[slider.len() - 1]);
    assert!(first.1 < 0.03 && last.1 > 0.97, "the ratio spans the slider: {first:?} … {last:?}");

    // A drag: the ratio follows the pointer (also past the slider's end) until the release.
    pill.press(first.0);
    pill.moved(last.0 + 40);
    pill.release(last.0 + 40);
    let drag = pill.take();
    assert_eq!(drag.first(), Some(&Event::Scrub(true, first.1)));
    assert_eq!(drag.last(), Some(&Event::Scrub(false, 1.0)), "{drag:?}");

    // The wheel steps the volume over the button, over the slider and between them, and nothing else happens.
    let gap = mute[mute.len() - 1] + 3;
    for x in [mute[mute.len() / 2], gap, columns[columns.len() / 2]] {
        pill.wheel(x, 120.0);
        pill.wheel(x, -120.0);
        match pill.take().as_slice() {
            [Event::Wheel(up), Event::Wheel(down)] => assert!(*up > 0.0 && *down < 0.0, "wheel deltas at {x}: {up} {down}"),
            other => panic!("the wheel at {x} asked for {other:?}"),
        }
    }
}
