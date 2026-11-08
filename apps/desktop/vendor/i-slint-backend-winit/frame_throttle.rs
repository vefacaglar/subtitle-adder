// Copyright © SixtyFPS GmbH <info@slint.dev>
// SPDX-License-Identifier: GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0

use std::rc::{Rc, Weak};

use i_slint_core::timers::{Timer, TimerMode};

#[cfg(target_vendor = "apple")]
mod apple_display_link;

use crate::winitwindowadapter::WinitWindowAdapter;

pub fn create_frame_throttle(
    window_adapter: Weak<WinitWindowAdapter>,
    _winit_window: &winit::window::Window,
    _is_wayland: bool,
) -> Box<dyn FrameThrottle> {
    if _is_wayland {
        WinitBasedFrameThrottle::create()
    } else {
        #[cfg(target_vendor = "apple")]
        if let Some(throttle) =
            apple_display_link::try_create(window_adapter.clone(), _winit_window)
        {
            return throttle;
        }
        TimerBasedFrameThrottle::create(window_adapter)
    }
}

pub trait FrameThrottle {
    fn request_throttled_redraw(&self, winit_window: &winit::window::Window);
}

struct TimerBasedFrameThrottle {
    window_adapter: Weak<WinitWindowAdapter>,
    timer: Rc<Timer>,
}

impl TimerBasedFrameThrottle {
    fn create(window_adapter: Weak<WinitWindowAdapter>) -> Box<dyn FrameThrottle> {
        Box::new(Self { window_adapter, timer: Rc::new(Timer::default()) })
    }
}

// Subtitle Adder patch (apps/desktop/Cargo.toml `[patch.crates-io]`): the upstream timer requested a redraw on every
// tick before checking `pending_redraw`, so the tick after a frame was drawn always queued one more, identical frame
// (two renders per request), and its interval was `1000 / refresh` whole milliseconds (3 ms at 280 Hz, faster than
// the display). Here an idle window draws at once, the timer only paces the frames that follow (one per refresh
// interval, in microseconds), and a tick without a pending redraw stops the timer without drawing.
impl FrameThrottle for TimerBasedFrameThrottle {
    fn request_throttled_redraw(&self, winit_window: &winit::window::Window) {
        if self.timer.running() {
            // The next tick draws it.
            return;
        }
        let refresh_interval_millihertz = winit_window
            .current_monitor()
            .and_then(|monitor| monitor.refresh_rate_millihertz())
            .filter(|mhz| *mhz >= 1000)
            .unwrap_or(60000) as u64;
        // Nothing drawn for at least one interval: draw now, without waiting for a tick.
        winit_window.request_redraw();
        let window_adapter = self.window_adapter.clone();
        let interval = std::time::Duration::from_micros(1_000_000_000 / refresh_interval_millihertz);
        let timer = Rc::downgrade(&self.timer);
        self.timer.start(TimerMode::Repeated, interval, move || {
            let Some(timer) = timer.upgrade() else { return };
            let keep_running = window_adapter.upgrade().is_some_and(|adapter| adapter.pending_redraw());
            if keep_running {
                // Requested again since the last frame (an animation, a property change): one frame per interval.
                redraw_now(&window_adapter);
            } else {
                timer.stop();
            }
        });
    }
}

fn redraw_now(window_adapter: &Weak<WinitWindowAdapter>) {
    let Some(winit_window) = window_adapter.upgrade().and_then(|adapter| adapter.winit_window())
    else {
        return;
    };
    winit_window.request_redraw();
}

struct WinitBasedFrameThrottle;

impl WinitBasedFrameThrottle {
    fn create() -> Box<dyn FrameThrottle> {
        Box::new(Self)
    }
}

impl FrameThrottle for WinitBasedFrameThrottle {
    fn request_throttled_redraw(&self, winit_window: &winit::window::Window) {
        winit_window.request_redraw();
    }
}
