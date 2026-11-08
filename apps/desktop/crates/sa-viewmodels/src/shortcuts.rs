//! Player keyboard shortcuts (UNIFIED.md §3.3): the shared `PlayerShortcut` enum and action performer; per-platform
//! resolution (Windows focus-kind routing win-ui-main §15; macOS `PlayerShortcut.resolve` with hardware key codes
//! and layout fallback mac-ui §5.2).

/// Seek step (s).
pub const SEEK_STEP: f64 = 5.0;
/// Shift / Ctrl seek step (s).
pub const LARGE_SEEK_STEP: f64 = 10.0;
/// Volume step.
pub const VOLUME_STEP: f64 = 0.05;

/// A player action.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PlayerShortcut {
    TogglePlayback,
    Seek(f64),
    AdjustVolume(f64),
    ToggleMute,
    ToggleTheater,
    ToggleFullScreen,
    TogglePanel,
    LeaveFullScreenOrTheater,
}

impl PlayerShortcut {
    /// Held keys repeat only seeking and volume (macOS `repeats()`).
    pub fn repeats(self) -> bool {
        matches!(self, PlayerShortcut::Seek(_) | PlayerShortcut::AdjustVolume(_))
    }
}

/// Keyboard focus context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ShortcutFocus {
    pub is_editing_text: bool,
    /// Windows: button/toggle/checkbox/combo box/combo item/list item/list (Space belongs to them).
    /// macOS: a focused button.
    pub is_on_button: bool,
    /// Windows: a list item, combo box or combo item (Up/Down navigate it). macOS: the line list.
    pub is_in_list: bool,
    pub is_full_screen: bool,
    pub panel_hidden: bool,
    /// Windows: theater mode is on (Esc leaves it).
    pub is_theater: bool,
}

/// Modifier set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modifiers {
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
    /// ⌘ (macOS) / Win key (ignored on Windows).
    pub command: bool,
}

/// A key as the UI layer reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyInput {
    /// macOS hardware key code when available.
    pub key_code: Option<u16>,
    /// Characters ignoring modifiers (Slint `KeyEvent.text`).
    pub text: String,
    pub modifiers: Modifiers,
    pub is_repeat: bool,
}

/// Named keys (Slint `Key` special characters).
pub mod keys {
    pub const SPACE: &str = " ";
    pub const LEFT: &str = "\u{F702}";
    pub const RIGHT: &str = "\u{F703}";
    pub const UP: &str = "\u{F700}";
    pub const DOWN: &str = "\u{F701}";
    pub const ESCAPE: &str = "\u{1b}";
    pub const RETURN: &str = "\n";
    pub const DELETE: &str = "\u{7f}";
    pub const BACKSPACE: &str = "\u{8}";
    pub const F11: &str = "\u{F70E}";
}

/// macOS virtual key codes (kVK_*).
pub mod mac_codes {
    pub const SPACE: u16 = 49;
    pub const LEFT: u16 = 123;
    pub const RIGHT: u16 = 124;
    pub const DOWN: u16 = 125;
    pub const UP: u16 = 126;
    pub const ANSI_M: u16 = 46;
    pub const ANSI_F: u16 = 3;
    pub const ANSI_E: u16 = 14;
}

/// Every action the Windows root key handler takes (win-ui-main §15).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WindowsKeyAction {
    Player(PlayerShortcut),
    Undo,
    Redo,
    PickVideo,
    ToggleExplorer,
    /// `FinishEditing()` then `Generate()` if allowed.
    Generate,
    OpenSettings,
    /// Line list: `FinishEditing(); DeleteSelected()`.
    DeleteSelectedLines,
}

/// Windows routing of the root (tunneling) handler; `None` = not consumed.
pub fn resolve_windows_key(key: &KeyInput, focus: &ShortcutFocus) -> Option<WindowsKeyAction> {
    use WindowsKeyAction as A;
    let t = key.text.as_str();
    let m = key.modifiers;
    // 1. F11 anywhere.
    if t == keys::F11 {
        return Some(A::Player(PlayerShortcut::ToggleFullScreen));
    }
    // 2. Escape outside a text box while full screen or theater.
    if t == keys::ESCAPE && !focus.is_editing_text && (focus.is_full_screen || focus.is_theater) {
        return Some(A::Player(PlayerShortcut::LeaveFullScreenOrTheater));
    }
    let lower = t.to_lowercase();
    // 3. No Ctrl/Alt, outside text boxes.
    if !focus.is_editing_text && !m.control && !m.alt {
        let step = if m.shift { LARGE_SEEK_STEP } else { SEEK_STEP };
        match t {
            keys::SPACE if !focus.is_on_button => return Some(A::Player(PlayerShortcut::TogglePlayback)),
            keys::LEFT => return Some(A::Player(PlayerShortcut::Seek(-step))),
            keys::RIGHT => return Some(A::Player(PlayerShortcut::Seek(step))),
            keys::UP | keys::DOWN if !focus.is_in_list || focus.panel_hidden || focus.is_full_screen => {
                let d = if t == keys::UP { VOLUME_STEP } else { -VOLUME_STEP };
                return Some(A::Player(PlayerShortcut::AdjustVolume(d)));
            }
            _ => {}
        }
        match lower.as_str() {
            "m" => return Some(A::Player(PlayerShortcut::ToggleMute)),
            "f" => return Some(A::Player(PlayerShortcut::ToggleFullScreen)),
            "e" => return Some(A::Player(PlayerShortcut::TogglePanel)),
            _ => {}
        }
    }
    // 4. Ctrl without Alt (AltGr = Ctrl+Alt is ignored).
    if m.control && !m.alt {
        match lower.as_str() {
            "z" if !m.shift => return Some(A::Undo),
            "z" | "y" => return Some(A::Redo),
            "o" => return Some(A::PickVideo),
            "b" => return Some(A::ToggleExplorer),
            "," => return Some(A::OpenSettings),
            _ => {}
        }
        match t {
            keys::RETURN | "\r" => return Some(A::Generate),
            keys::LEFT if !focus.is_editing_text => return Some(A::Player(PlayerShortcut::Seek(-LARGE_SEEK_STEP))),
            keys::RIGHT if !focus.is_editing_text => return Some(A::Player(PlayerShortcut::Seek(LARGE_SEEK_STEP))),
            keys::UP if !focus.is_editing_text => return Some(A::Player(PlayerShortcut::AdjustVolume(VOLUME_STEP))),
            keys::DOWN if !focus.is_editing_text => return Some(A::Player(PlayerShortcut::AdjustVolume(-VOLUME_STEP))),
            _ => {}
        }
    }
    None
}

/// Line-list handler on Windows (§15 step 5), after the root did not consume the key.
pub fn resolve_windows_list_key(key: &KeyInput, focus: &ShortcutFocus) -> Option<WindowsKeyAction> {
    if focus.is_editing_text {
        return None;
    }
    match key.text.as_str() {
        keys::SPACE => Some(WindowsKeyAction::Player(PlayerShortcut::TogglePlayback)),
        keys::DELETE => Some(WindowsKeyAction::DeleteSelectedLines),
        _ => None,
    }
}

/// Windows routing (win-ui-main §15), player actions only.
pub fn resolve_windows(key: &KeyInput, focus: &ShortcutFocus) -> Option<PlayerShortcut> {
    match resolve_windows_key(key, focus)? {
        WindowsKeyAction::Player(p) => Some(p),
        _ => None,
    }
}

fn is_latin_letter(c: char) -> bool {
    let v = c as u32;
    matches!(v,
        0x41..=0x24F | 0x250..=0x2AF | 0x300..=0x36F | 0x1E00..=0x1EFF | 0x2C60..=0x2C7F | 0xA720..=0xA7FF
        | 0xFB00..=0xFB06 | 0xFF21..=0xFF3A | 0xFF41..=0xFF5A)
}

fn mac_letter(text: &str, key_code: Option<u16>) -> Option<char> {
    let typed = text.to_lowercase();
    if !typed.is_empty() {
        let mut chars = typed.chars();
        let first = chars.next()?;
        let rest: Vec<char> = chars.collect();
        // One grapheme: a base char followed only by combining marks.
        let single = rest.iter().all(|c| matches!(*c as u32, 0x300..=0x36F));
        if !single {
            return None;
        }
        if first.is_ascii() {
            return first.is_ascii_alphabetic().then_some(first);
        }
        if !first.is_alphabetic() || (is_latin_letter(first) && rest.iter().all(|c| is_latin_letter(*c))) {
            return None;
        }
    }
    match key_code? {
        mac_codes::ANSI_M => Some('m'),
        mac_codes::ANSI_F => Some('f'),
        mac_codes::ANSI_E => Some('e'),
        _ => None,
    }
}

/// macOS resolution (mac-ui §5.2). Without a hardware key code, Slint's named keys stand in for the code.
pub fn resolve_mac(key: &KeyInput, focus: &ShortcutFocus) -> Option<PlayerShortcut> {
    let m = key.modifiers;
    if focus.is_editing_text || m.control || m.alt || m.command {
        return None;
    }
    let code = key.key_code.or(match key.text.as_str() {
        keys::SPACE => Some(mac_codes::SPACE),
        keys::LEFT => Some(mac_codes::LEFT),
        keys::RIGHT => Some(mac_codes::RIGHT),
        keys::UP => Some(mac_codes::UP),
        keys::DOWN => Some(mac_codes::DOWN),
        _ => None,
    });
    let step = if m.shift { LARGE_SEEK_STEP } else { SEEK_STEP };
    match code {
        Some(mac_codes::SPACE) => return (!focus.is_on_button).then_some(PlayerShortcut::TogglePlayback),
        Some(mac_codes::LEFT) => return Some(PlayerShortcut::Seek(-step)),
        Some(mac_codes::RIGHT) => return Some(PlayerShortcut::Seek(step)),
        Some(c @ (mac_codes::UP | mac_codes::DOWN)) => {
            if focus.is_in_list && !focus.panel_hidden && !focus.is_full_screen {
                return None;
            }
            return Some(PlayerShortcut::AdjustVolume(if c == mac_codes::UP { VOLUME_STEP } else { -VOLUME_STEP }));
        }
        _ => {}
    }
    match mac_letter(&key.text, key.key_code)? {
        'm' => Some(PlayerShortcut::ToggleMute),
        'f' => Some(PlayerShortcut::ToggleFullScreen),
        'e' => Some(PlayerShortcut::TogglePanel),
        _ => None,
    }
}

/// `VideoClick.actions(click_count)`: 1 → toggle; 2 → toggle back + full screen.
pub fn video_click_actions(click_count: u32) -> Vec<PlayerShortcut> {
    match click_count {
        1 => vec![PlayerShortcut::TogglePlayback],
        2 => vec![PlayerShortcut::TogglePlayback, PlayerShortcut::ToggleFullScreen],
        _ => Vec::new(),
    }
}

/// What the action performer needs from the player and window.
pub trait ShortcutTarget {
    fn is_player_shown(&self) -> bool;
    fn is_full_screen(&self) -> bool;
    fn toggle_playback(&mut self);
    fn seek_by(&mut self, seconds: f64);
    fn adjust_volume(&mut self, step: f64);
    fn toggle_mute(&mut self);
    fn toggle_full_screen(&mut self);
    fn toggle_theater(&mut self);
    fn toggle_subtitle_panel(&mut self);
    fn leave_full_screen_or_theater(&mut self);
}

/// `PlayerShortcutActions.perform` (shared): `false` = not consumed.
pub fn perform(shortcut: PlayerShortcut, is_repeat: bool, target: &mut dyn ShortcutTarget) -> bool {
    if !target.is_player_shown() {
        return false;
    }
    if is_repeat && !shortcut.repeats() {
        return true;
    }
    match shortcut {
        PlayerShortcut::TogglePlayback => target.toggle_playback(),
        PlayerShortcut::Seek(s) => target.seek_by(s),
        PlayerShortcut::AdjustVolume(d) => target.adjust_volume(d),
        PlayerShortcut::ToggleMute => target.toggle_mute(),
        PlayerShortcut::ToggleFullScreen => target.toggle_full_screen(),
        PlayerShortcut::ToggleTheater => target.toggle_theater(),
        PlayerShortcut::TogglePanel => {
            if !target.is_full_screen() {
                target.toggle_subtitle_panel();
            }
        }
        PlayerShortcut::LeaveFullScreenOrTheater => target.leave_full_screen_or_theater(),
    }
    true
}

/// `adjust_volume(step)`: `base = muted ? 0 : volume`; next = round(clamp(base + step) * 100) / 100; returns
/// `(volume, muted)`.
pub fn adjusted_volume(volume: f64, muted: bool, step: f64) -> (f64, bool) {
    let base = if muted { 0.0 } else { volume };
    let next = ((base + step).clamp(0.0, 1.0) * 100.0).round() / 100.0;
    (next, if next > 0.0 { false } else { muted })
}

/// Mouse wheel on the video: Windows ±0.05 per event; macOS `ScrollNotches` accumulation (momentum ignored).
#[derive(Debug, Clone, Default)]
pub struct ScrollNotches {
    accumulated: f64,
}

impl ScrollNotches {
    pub const PRECISE_STEP: f64 = 10.0;

    /// Returns notches (positive = up = louder). `momentum` events are ignored.
    pub fn feed(&mut self, delta_y: f64, precise: bool, momentum: bool) -> i32 {
        if momentum || !delta_y.is_finite() || delta_y == 0.0 {
            return 0;
        }
        if !precise {
            self.accumulated = 0.0;
            return if delta_y > 0.0 { 1 } else { -1 };
        }
        if self.accumulated * delta_y < 0.0 {
            self.accumulated = 0.0;
        }
        self.accumulated += delta_y;
        let n = (self.accumulated / Self::PRECISE_STEP).trunc();
        self.accumulated -= n * Self::PRECISE_STEP;
        n as i32
    }

    pub fn reset(&mut self) {
        self.accumulated = 0.0;
    }
}
