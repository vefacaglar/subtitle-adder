//! Keyboard, wheel and drop input of the main window: Slint key events → `sa_viewmodels::shortcuts` inputs
//! (win-ui-main §15, mac-ui §3.2 / §5), file-drop batching (win-ui-main §17.1, mac-ui §13.1) and the drop / external
//! open split into videos and subtitles.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sa_viewmodels::shortcuts::{KeyInput, Modifiers, keys};

/// A key as `MainBridge.key-pressed` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlintKey {
    pub text: String,
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
    pub meta: bool,
    pub repeat: bool,
}

/// Slint `Key.F11`.
pub const F11: &str = keys::F11;

/// Maps a Slint key to the shared shortcut input.
///
/// * Slint's `control` is ⌘ on macOS and `meta` is ⌃ there (ARCHITECTURE.md §6.6), so the mac mapping swaps them.
/// * With Ctrl held some backends deliver ASCII control characters (`^Z` = U+001A); they are folded back to the
///   letter so `Ctrl+Z`, `Ctrl+Y`, `Ctrl+O`, `Ctrl+B` resolve.
pub fn to_key_input(k: &SlintKey, mac: bool) -> KeyInput {
    let modifiers = if mac {
        Modifiers { shift: k.shift, control: k.meta, alt: k.alt, command: k.control }
    } else {
        Modifiers { shift: k.shift, control: k.control, alt: k.alt, command: k.meta }
    };
    let mut text = k.text.clone();
    if (k.control || k.meta) && text.chars().count() == 1 {
        let c = text.chars().next().unwrap_or_default() as u32;
        if (0x01..=0x1a).contains(&c) && !matches!(c, 0x08 | 0x09 | 0x0a | 0x0d) {
            text = char::from_u32(c + 0x60).map(String::from).unwrap_or(text);
        }
    }
    KeyInput { key_code: None, text, modifiers, is_repeat: k.repeat }
}

/// macOS menu key equivalents the window handles itself while no NSMenu exists (preview builds) and that the Slint
/// MenuBar does not route on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacCommandKey {
    OpenVideo,
    Generate,
    Stop,
    ToggleSidebar,
    ToggleFullScreen,
    Settings,
    Undo,
    Redo,
}

/// ⌘O, ⌘↩, ⌘., ⌃⌘S, ⌃⌘F, ⌘,, ⌘Z, ⇧⌘Z (mac-ui §5.1).
pub fn mac_command_key(k: &KeyInput) -> Option<MacCommandKey> {
    let m = k.modifiers;
    if !m.command || m.alt {
        return None;
    }
    let t = k.text.to_lowercase();
    match (m.control, m.shift, t.as_str()) {
        (true, _, "s") => Some(MacCommandKey::ToggleSidebar),
        (true, _, "f") => Some(MacCommandKey::ToggleFullScreen),
        (false, false, "o") => Some(MacCommandKey::OpenVideo),
        (false, false, "\n" | "\r") => Some(MacCommandKey::Generate),
        (false, false, ".") => Some(MacCommandKey::Stop),
        (false, false, ",") => Some(MacCommandKey::Settings),
        (false, false, "z") => Some(MacCommandKey::Undo),
        (false, true, "z") => Some(MacCommandKey::Redo),
        _ => None,
    }
}

/// `true` for the Esc key without modifiers (macOS handles Esc before the player keys).
pub fn is_plain_escape(k: &KeyInput) -> bool {
    k.text == keys::ESCAPE && !k.modifiers.shift && !k.modifiers.control && !k.modifiers.alt && !k.modifiers.command
}

/// `SubtitleParser.Extensions`.
pub const SUBTITLE_EXTENSIONS: [&str; 5] = ["srt", "vtt", "ass", "ssa", "json"];

pub fn is_subtitle(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| SUBTITLE_EXTENSIONS.iter().any(|s| s.eq_ignore_ascii_case(e)))
}

/// Dropped files split into videos (every non-subtitle file, folders ignored) and the first subtitle.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DropSplit {
    pub videos: Vec<PathBuf>,
    pub subtitle: Option<PathBuf>,
}

pub fn split_drop(paths: &[PathBuf], is_dir: impl Fn(&Path) -> bool) -> DropSplit {
    let mut out = DropSplit::default();
    for p in paths {
        if is_dir(p) {
            continue;
        }
        if is_subtitle(p) {
            if out.subtitle.is_none() {
                out.subtitle = Some(p.clone());
            }
        } else {
            out.videos.push(p.clone());
        }
    }
    out
}

/// Coalesces winit's one-`DroppedFile`-per-file events into one batch: files that arrive within `window` of each
/// other belong to the same drop.
#[derive(Debug, Default)]
pub struct DropBatcher {
    pending: Vec<PathBuf>,
    last: Option<Instant>,
}

impl DropBatcher {
    pub const WINDOW: Duration = Duration::from_millis(80);

    pub fn push(&mut self, path: PathBuf, now: Instant) {
        self.pending.push(path);
        self.last = Some(now);
    }

    /// The finished batch once no file arrived for [`Self::WINDOW`].
    pub fn take_if_settled(&mut self, now: Instant) -> Option<Vec<PathBuf>> {
        match self.last {
            Some(t) if now.duration_since(t) >= Self::WINDOW && !self.pending.is_empty() => {
                self.last = None;
                Some(std::mem::take(&mut self.pending))
            }
            _ => None,
        }
    }

    pub fn is_pending(&self) -> bool {
        !self.pending.is_empty()
    }
}

/// Precise (trackpad) vs line (mouse wheel) delta of a wheel event on the video (mac `ScrollNotches` input).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Wheel {
    pub delta_y: f64,
    pub precise: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use sa_viewmodels::shortcuts::{PlayerShortcut, ShortcutFocus, WindowsKeyAction, resolve_mac, resolve_windows_key};

    fn key(text: &str) -> SlintKey {
        SlintKey { text: text.into(), shift: false, control: false, alt: false, meta: false, repeat: false }
    }

    #[test]
    fn windows_control_letters_resolve_even_as_control_characters() {
        let f = ShortcutFocus::default();
        let z = to_key_input(&SlintKey { control: true, ..key("\u{1a}") }, false);
        assert_eq!(resolve_windows_key(&z, &f), Some(WindowsKeyAction::Undo));
        let y = to_key_input(&SlintKey { control: true, ..key("y") }, false);
        assert_eq!(resolve_windows_key(&y, &f), Some(WindowsKeyAction::Redo));
        let sz = to_key_input(&SlintKey { control: true, shift: true, ..key("Z") }, false);
        assert_eq!(resolve_windows_key(&sz, &f), Some(WindowsKeyAction::Redo));
        let enter = to_key_input(&SlintKey { control: true, ..key("\n") }, false);
        assert_eq!(resolve_windows_key(&enter, &f), Some(WindowsKeyAction::Generate));
        let altgr = to_key_input(&SlintKey { control: true, alt: true, ..key("z") }, false);
        assert_eq!(resolve_windows_key(&altgr, &f), None);
    }

    #[test]
    fn windows_routing_table() {
        let f = ShortcutFocus::default();
        let key_in = |t: &str| to_key_input(&key(t), false);
        assert_eq!(resolve_windows_key(&key_in(" "), &f), Some(WindowsKeyAction::Player(PlayerShortcut::TogglePlayback)));
        assert_eq!(resolve_windows_key(&key_in(F11), &ShortcutFocus { is_editing_text: true, ..f }), Some(WindowsKeyAction::Player(PlayerShortcut::ToggleFullScreen)));
        assert_eq!(resolve_windows_key(&key_in("m"), &ShortcutFocus { is_editing_text: true, ..f }), None);
        let list = ShortcutFocus { is_in_list: true, ..f };
        assert_eq!(resolve_windows_key(&key_in(keys::UP), &list), None);
        assert_eq!(resolve_windows_key(&key_in(keys::UP), &ShortcutFocus { panel_hidden: true, ..list }), Some(WindowsKeyAction::Player(PlayerShortcut::AdjustVolume(0.05))));
        let esc = key_in(keys::ESCAPE);
        assert_eq!(resolve_windows_key(&esc, &f), None);
        assert_eq!(resolve_windows_key(&esc, &ShortcutFocus { is_theater: true, ..f }), Some(WindowsKeyAction::Player(PlayerShortcut::LeaveFullScreenOrTheater)));
    }

    #[test]
    fn mac_swaps_slint_control_and_meta() {
        // Slint `control` = ⌘ on macOS.
        let cmd_o = to_key_input(&SlintKey { control: true, ..key("o") }, true);
        assert!(cmd_o.modifiers.command && !cmd_o.modifiers.control);
        assert_eq!(mac_command_key(&cmd_o), Some(MacCommandKey::OpenVideo));
        let ctrl_cmd_s = to_key_input(&SlintKey { control: true, meta: true, ..key("s") }, true);
        assert_eq!(mac_command_key(&ctrl_cmd_s), Some(MacCommandKey::ToggleSidebar));
        let ctrl_cmd_f = to_key_input(&SlintKey { control: true, meta: true, ..key("f") }, true);
        assert_eq!(mac_command_key(&ctrl_cmd_f), Some(MacCommandKey::ToggleFullScreen));
        let redo = to_key_input(&SlintKey { control: true, shift: true, ..key("Z") }, true);
        assert_eq!(mac_command_key(&redo), Some(MacCommandKey::Redo));
        assert_eq!(mac_command_key(&to_key_input(&key("o"), true)), None);
        // A chord is never a player key.
        assert_eq!(resolve_mac(&cmd_o, &ShortcutFocus::default()), None);
        assert_eq!(resolve_mac(&to_key_input(&key("m"), true), &ShortcutFocus::default()), Some(PlayerShortcut::ToggleMute));
        assert!(is_plain_escape(&to_key_input(&key(keys::ESCAPE), true)));
        assert!(!is_plain_escape(&to_key_input(&SlintKey { shift: true, ..key(keys::ESCAPE) }, true)));
    }

    #[test]
    fn drops_split_videos_and_the_first_subtitle_and_skip_folders() {
        let paths: Vec<PathBuf> = ["a.mp4", "dir", "b.SRT", "c.vtt", "d.txt"].iter().map(PathBuf::from).collect();
        let s = split_drop(&paths, |p| p == Path::new("dir"));
        assert_eq!(s.videos, vec![PathBuf::from("a.mp4"), PathBuf::from("d.txt")]);
        assert_eq!(s.subtitle, Some(PathBuf::from("b.SRT")));
    }

    #[test]
    fn dropped_files_of_one_drop_arrive_as_one_batch() {
        let t0 = Instant::now();
        let mut b = DropBatcher::default();
        b.push("a.mp4".into(), t0);
        b.push("b.mp4".into(), t0 + Duration::from_millis(5));
        assert_eq!(b.take_if_settled(t0 + Duration::from_millis(30)), None);
        assert_eq!(b.take_if_settled(t0 + Duration::from_millis(100)), Some(vec!["a.mp4".into(), "b.mp4".into()]));
        assert!(!b.is_pending());
        assert_eq!(b.take_if_settled(t0 + Duration::from_millis(500)), None);
    }
}
