//! Transport controls (win-ui-main §11 `PreviewTransportControls.synchronize`, mac-ui §7.4 AVKit-like inline bar):
//! the display values computed on every player tick. Pure.

use sa_player::PlaybackTimeline;

/// Inputs of one synchronization.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransportInput {
    pub position: f64,
    pub duration: f64,
    pub is_playing: bool,
    pub is_muted: bool,
    pub volume: f64,
    pub buffered_ratio: f64,
    /// The seek slider is being dragged (its ratio is the user's, not the player's).
    pub seek_scrubbing: bool,
    /// The ratio under the pointer while scrubbing.
    pub scrub_ratio: f64,
    pub volume_scrubbing: bool,
}

/// What the HUD shows.
#[derive(Debug, Clone, PartialEq)]
pub struct TransportView {
    pub interactive: bool,
    /// `None` while scrubbing (the slider keeps the user's ratio).
    pub seek_ratio: Option<f64>,
    pub buffered_ratio: f64,
    pub elapsed: String,
    pub total: String,
    /// macOS `-m:ss` (empty without a duration).
    pub remaining: String,
    pub play_glyph: char,
    /// Optical centering of the play triangle (Windows).
    pub play_glyph_margin_left: f64,
    pub play_enabled: bool,
    pub volume_glyph: char,
    /// macOS SF Symbol of the volume button.
    pub volume_symbol: &'static str,
    /// `None` while the volume slider is dragged.
    pub volume_ratio: Option<f64>,
}

fn finite_or(v: f64, d: f64) -> f64 {
    if v.is_finite() { v } else { d }
}

/// Windows volume glyph by effective volume (`muted ? 0 : volume`).
pub fn windows_volume_glyph(effective: f64) -> char {
    if effective <= 0.001 {
        '\u{E74F}'
    } else if effective < 0.33 {
        '\u{E992}'
    } else if effective < 0.66 {
        '\u{E993}'
    } else {
        '\u{E767}'
    }
}

/// macOS AVKit time text: `m:ss`, or `h:mm:ss` from one hour.
pub fn mac_time(seconds: f64) -> String {
    let total = finite_or(seconds, 0.0).max(0.0).floor().min(i64::MAX as f64) as i64;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

/// `synchronize(position, duration, is_playing, is_muted, volume, buffered)` (win-ui-main §11); the macOS bar uses
/// the same values with its own time texts.
pub fn synchronize(i: TransportInput, mac: bool) -> TransportView {
    let duration = finite_or(i.duration, 0.0).max(0.0);
    let position = PlaybackTimeline::clamp(i.position, duration);
    let volume = finite_or(i.volume, 1.0).clamp(0.0, 1.0);
    let interactive = duration > 0.0;
    let ratio = if duration > 0.0 { (position / duration).clamp(0.0, 1.0) } else { 0.0 };
    let buffered = if duration > 0.0 { ratio.max(finite_or(i.buffered_ratio, 0.0).clamp(0.0, 1.0)) } else { 0.0 };
    let scrub = finite_or(i.scrub_ratio, 0.0).clamp(0.0, 1.0);
    let shown_position = if i.seek_scrubbing { scrub * duration } else { position };
    let hours = duration >= 3600.0;
    let (elapsed, total, remaining) = if mac {
        let rem = if duration > 0.0 { format!("-{}", mac_time(duration.floor() - shown_position.floor())) } else { String::new() };
        (mac_time(shown_position), if duration > 0.0 { mac_time(duration) } else { "--:--".to_owned() }, rem)
    } else {
        (
            PlaybackTimeline::format_time(shown_position, hours),
            if duration > 0.0 { PlaybackTimeline::format_time(duration, hours) } else { "--:--".to_owned() },
            String::new(),
        )
    };
    let effective = if i.is_muted { 0.0 } else { volume };
    TransportView {
        interactive,
        seek_ratio: (!i.seek_scrubbing).then_some(ratio),
        buffered_ratio: buffered,
        elapsed,
        total,
        remaining,
        play_glyph: if i.is_playing { '\u{E769}' } else { '\u{E768}' },
        play_glyph_margin_left: if i.is_playing { 0.0 } else { 1.5 },
        play_enabled: duration > 0.0,
        volume_glyph: windows_volume_glyph(effective),
        volume_symbol: if effective <= 0.001 { "speaker.slash.fill" } else { "speaker.wave.2.fill" },
        volume_ratio: (!i.volume_scrubbing).then_some(effective),
    }
}

/// Windows: a buffered-ratio change below this is ignored by the scrubber (`set_buffered_ratio`).
pub const BUFFERED_EPSILON: f64 = 0.0005;

/// Seek target of a scrub (`seek_requested(ratio * duration)` only with a duration).
pub fn scrub_target(ratio: f64, duration: f64) -> Option<f64> {
    let d = finite_or(duration, 0.0);
    (d > 0.0).then(|| finite_or(ratio, 0.0).clamp(0.0, 1.0) * d)
}

/// Wheel over the volume panel / the player (Windows): ±0.05 from `muted ? 0 : volume`, clamped.
pub fn windows_wheel_volume(volume: f64, muted: bool, delta: f64) -> f64 {
    let base = if muted { 0.0 } else { volume };
    (base + if delta > 0.0 { 0.05 } else { -0.05 }).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(position: f64, duration: f64) -> TransportInput {
        TransportInput {
            position,
            duration,
            is_playing: false,
            is_muted: false,
            volume: 1.0,
            buffered_ratio: 0.0,
            seek_scrubbing: false,
            scrub_ratio: 0.0,
            volume_scrubbing: false,
        }
    }

    #[test]
    fn without_a_duration_the_bar_is_inert() {
        let v = synchronize(input(5.0, 0.0), false);
        assert!(!v.interactive && !v.play_enabled);
        assert_eq!(v.total, "--:--");
        // Without a duration the position is only clamped at zero.
        assert_eq!(v.elapsed, "00:05");
        assert_eq!(v.seek_ratio, Some(0.0));
        assert_eq!(v.buffered_ratio, 0.0);
        let v = synchronize(input(f64::NAN, f64::INFINITY), false);
        assert_eq!(v.elapsed, "00:00");
    }

    #[test]
    fn texts_ratios_and_hours_follow_the_duration() {
        let v = synchronize(TransportInput { buffered_ratio: 0.2, ..input(138.9, 600.0) }, false);
        assert_eq!(v.elapsed, "02:18");
        assert_eq!(v.total, "10:00");
        assert!((v.seek_ratio.unwrap() - 138.9 / 600.0).abs() < 1e-9);
        // Buffered never trails the playhead.
        assert!((v.buffered_ratio - 138.9 / 600.0).abs() < 1e-9);
        let v = synchronize(input(138.0, 3700.0), false);
        assert_eq!(v.elapsed, "00:02:18");
        assert_eq!(v.total, "01:01:40");
    }

    #[test]
    fn scrubbing_shows_the_pointer_time_and_keeps_the_slider() {
        let v = synchronize(TransportInput { seek_scrubbing: true, scrub_ratio: 0.5, ..input(10.0, 100.0) }, false);
        assert_eq!(v.seek_ratio, None);
        assert_eq!(v.elapsed, "00:50");
        assert_eq!(scrub_target(0.5, 100.0), Some(50.0));
        assert_eq!(scrub_target(0.5, 0.0), None);
    }

    #[test]
    fn play_and_volume_glyphs() {
        let v = synchronize(TransportInput { is_playing: true, ..input(1.0, 10.0) }, false);
        assert_eq!((v.play_glyph, v.play_glyph_margin_left), ('\u{E769}', 0.0));
        let v = synchronize(input(1.0, 10.0), false);
        assert_eq!((v.play_glyph, v.play_glyph_margin_left), ('\u{E768}', 1.5));
        assert_eq!(windows_volume_glyph(0.0), '\u{E74F}');
        assert_eq!(windows_volume_glyph(0.2), '\u{E992}');
        assert_eq!(windows_volume_glyph(0.5), '\u{E993}');
        assert_eq!(windows_volume_glyph(0.9), '\u{E767}');
        let v = synchronize(TransportInput { is_muted: true, volume: 0.8, ..input(1.0, 10.0) }, true);
        assert_eq!(v.volume_ratio, Some(0.0));
        assert_eq!(v.volume_symbol, "speaker.slash.fill");
        let v = synchronize(TransportInput { volume: f64::NAN, ..input(1.0, 10.0) }, false);
        assert_eq!(v.volume_ratio, Some(1.0));
    }

    #[test]
    fn mac_texts_are_avkit_like() {
        let v = synchronize(input(7.4, 38.2), true);
        assert_eq!(v.elapsed, "0:07");
        assert_eq!(v.remaining, "-0:31");
        assert_eq!(mac_time(3725.0), "1:02:05");
        assert_eq!(mac_time(-3.0), "0:00");
        assert_eq!(synchronize(input(0.0, 0.0), true).remaining, "");
    }

    #[test]
    fn windows_wheel_steps_from_silence_while_muted() {
        assert!((windows_wheel_volume(0.5, false, 1.0) - 0.55).abs() < 1e-9);
        assert!((windows_wheel_volume(0.5, true, 1.0) - 0.05).abs() < 1e-9);
        assert_eq!(windows_wheel_volume(0.02, false, -1.0), 0.0);
        assert_eq!(windows_wheel_volume(0.99, false, 3.0), 1.0);
    }
}
