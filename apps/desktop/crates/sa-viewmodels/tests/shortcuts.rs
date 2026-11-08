//! Ports of the shortcut / click / scroll part of `PlayerControlsTests` (Swift) and the Windows key routing of
//! win-ui-main §15.

use sa_viewmodels::shortcuts::*;

fn mac_key(code: u16, text: &str) -> KeyInput {
    KeyInput { key_code: Some(code), text: text.into(), modifiers: Modifiers::default(), is_repeat: false }
}

fn win_key(text: &str) -> KeyInput {
    KeyInput { key_code: None, text: text.into(), modifiers: Modifiers::default(), is_repeat: false }
}

fn with(mut k: KeyInput, f: impl FnOnce(&mut Modifiers)) -> KeyInput {
    f(&mut k.modifiers);
    k
}

const F: ShortcutFocus =
    ShortcutFocus { is_editing_text: false, is_on_button: false, is_in_list: false, is_full_screen: false, panel_hidden: false, is_theater: false };

#[test]
fn keys_map_to_player_actions() {
    use PlayerShortcut::*;
    assert_eq!(resolve_mac(&mac_key(49, " "), &F), Some(TogglePlayback));
    assert_eq!(resolve_mac(&mac_key(123, ""), &F), Some(Seek(-5.0)));
    assert_eq!(resolve_mac(&mac_key(124, ""), &F), Some(Seek(5.0)));
    assert_eq!(resolve_mac(&with(mac_key(123, ""), |m| m.shift = true), &F), Some(Seek(-10.0)));
    assert_eq!(resolve_mac(&mac_key(126, ""), &F), Some(AdjustVolume(0.05)));
    assert_eq!(resolve_mac(&mac_key(125, ""), &F), Some(AdjustVolume(-0.05)));
    assert_eq!(resolve_mac(&mac_key(46, "m"), &F), Some(ToggleMute));
    assert_eq!(resolve_mac(&mac_key(3, "f"), &F), Some(ToggleFullScreen));
    assert_eq!(resolve_mac(&mac_key(14, "e"), &F), Some(TogglePanel));
}

#[test]
fn a_key_that_types_punctuation_or_a_marked_latin_letter_is_not_a_shortcut_whatever_its_position() {
    assert_eq!(resolve_mac(&mac_key(46, ","), &F), None);
    assert_eq!(resolve_mac(&mac_key(46, ";"), &F), None);
    assert_eq!(resolve_mac(&mac_key(3, "ğ"), &F), None);
}

#[test]
fn letters_of_other_scripts_still_fall_back_to_their_key_position() {
    assert_eq!(resolve_mac(&mac_key(46, "ь"), &F), Some(PlayerShortcut::ToggleMute));
    assert_eq!(resolve_mac(&with(mac_key(46, "Ь"), |m| m.shift = true), &F), Some(PlayerShortcut::ToggleMute));
    assert_eq!(resolve_mac(&mac_key(3, "а"), &F), Some(PlayerShortcut::ToggleFullScreen));
    assert_eq!(resolve_mac(&mac_key(14, "у"), &F), Some(PlayerShortcut::TogglePanel));
}

#[test]
fn shortcuts_ignore_chords_and_typing() {
    for f in [|m: &mut Modifiers| m.command = true, |m: &mut Modifiers| m.control = true, |m: &mut Modifiers| m.alt = true] {
        assert_eq!(resolve_mac(&with(mac_key(46, "m"), f), &F), None);
        assert_eq!(resolve_mac(&with(mac_key(49, " "), f), &F), None);
    }
    let editing = ShortcutFocus { is_editing_text: true, ..F };
    assert_eq!(resolve_mac(&mac_key(49, " "), &editing), None);
    assert_eq!(resolve_windows(&win_key(" "), &editing), None);
}

#[test]
fn space_belongs_to_a_focused_button() {
    let b = ShortcutFocus { is_on_button: true, ..F };
    assert_eq!(resolve_mac(&mac_key(49, " "), &b), None);
    assert_eq!(resolve_mac(&mac_key(124, ""), &b), Some(PlayerShortcut::Seek(5.0)));
    assert_eq!(resolve_windows(&win_key(" "), &b), None);
    assert_eq!(resolve_windows(&win_key(keys::RIGHT), &b), Some(PlayerShortcut::Seek(5.0)));
}

#[test]
fn up_and_down_go_to_the_list_unless_it_is_out_of_the_way() {
    let list = ShortcutFocus { is_in_list: true, ..F };
    assert_eq!(resolve_mac(&mac_key(126, ""), &list), None);
    assert_eq!(resolve_mac(&mac_key(126, ""), &ShortcutFocus { panel_hidden: true, ..list }), Some(PlayerShortcut::AdjustVolume(0.05)));
    assert_eq!(resolve_mac(&mac_key(125, ""), &ShortcutFocus { is_full_screen: true, ..list }), Some(PlayerShortcut::AdjustVolume(-0.05)));
    assert_eq!(resolve_windows(&win_key(keys::UP), &list), None);
    assert_eq!(resolve_windows(&win_key(keys::UP), &ShortcutFocus { panel_hidden: true, ..list }), Some(PlayerShortcut::AdjustVolume(0.05)));
}

#[test]
fn named_keys_stand_in_for_hardware_codes_on_mac() {
    let k = KeyInput { key_code: None, text: keys::SPACE.into(), modifiers: Modifiers::default(), is_repeat: false };
    assert_eq!(resolve_mac(&k, &F), Some(PlayerShortcut::TogglePlayback));
    let k = KeyInput { key_code: None, text: "m".into(), modifiers: Modifiers::default(), is_repeat: false };
    assert_eq!(resolve_mac(&k, &F), Some(PlayerShortcut::ToggleMute));
}

#[test]
fn windows_routing() {
    use WindowsKeyAction as A;
    assert_eq!(resolve_windows_key(&win_key(keys::F11), &ShortcutFocus { is_editing_text: true, ..F }), Some(A::Player(PlayerShortcut::ToggleFullScreen)));
    assert_eq!(resolve_windows_key(&win_key(keys::ESCAPE), &F), None, "Esc only while full screen or theater");
    assert_eq!(
        resolve_windows_key(&win_key(keys::ESCAPE), &ShortcutFocus { is_theater: true, ..F }),
        Some(A::Player(PlayerShortcut::LeaveFullScreenOrTheater))
    );
    assert_eq!(resolve_windows_key(&with(win_key(keys::LEFT), |m| m.shift = true), &F), Some(A::Player(PlayerShortcut::Seek(-10.0))));
    assert_eq!(resolve_windows_key(&with(win_key(keys::RIGHT), |m| m.control = true), &F), Some(A::Player(PlayerShortcut::Seek(10.0))));
    assert_eq!(resolve_windows_key(&win_key("M"), &F), Some(A::Player(PlayerShortcut::ToggleMute)));
    assert_eq!(resolve_windows_key(&win_key("e"), &F), Some(A::Player(PlayerShortcut::TogglePanel)));
    let ctrl = |t: &str| with(win_key(t), |m| m.control = true);
    let editing = ShortcutFocus { is_editing_text: true, ..F };
    assert_eq!(resolve_windows_key(&ctrl("z"), &editing), Some(A::Undo));
    assert_eq!(resolve_windows_key(&ctrl("y"), &F), Some(A::Redo));
    assert_eq!(resolve_windows_key(&with(ctrl("Z"), |m| m.shift = true), &F), Some(A::Redo));
    assert_eq!(resolve_windows_key(&ctrl("o"), &F), Some(A::PickVideo));
    assert_eq!(resolve_windows_key(&ctrl("b"), &F), Some(A::ToggleExplorer));
    assert_eq!(resolve_windows_key(&ctrl(","), &F), Some(A::OpenSettings));
    assert_eq!(resolve_windows_key(&ctrl(keys::RETURN), &F), Some(A::Generate));
    assert_eq!(resolve_windows_key(&with(ctrl("z"), |m| m.alt = true), &F), None, "AltGr is not a shortcut");
    assert_eq!(resolve_windows_list_key(&win_key(keys::DELETE), &F), Some(A::DeleteSelectedLines));
    assert_eq!(resolve_windows_list_key(&win_key(keys::DELETE), &editing), None);
}

#[test]
fn holding_a_key_repeats_only_seeking_and_volume() {
    assert!(PlayerShortcut::Seek(5.0).repeats());
    assert!(PlayerShortcut::AdjustVolume(0.05).repeats());
    assert!(!PlayerShortcut::ToggleMute.repeats());
    assert!(!PlayerShortcut::TogglePlayback.repeats());
}

#[derive(Default)]
struct Target {
    shown: bool,
    full: bool,
    log: Vec<String>,
}

impl ShortcutTarget for Target {
    fn is_player_shown(&self) -> bool {
        self.shown
    }
    fn is_full_screen(&self) -> bool {
        self.full
    }
    fn toggle_playback(&mut self) {
        self.log.push("play".into());
    }
    fn seek_by(&mut self, s: f64) {
        self.log.push(format!("seek {s}"));
    }
    fn adjust_volume(&mut self, d: f64) {
        self.log.push(format!("vol {d}"));
    }
    fn toggle_mute(&mut self) {
        self.log.push("mute".into());
    }
    fn toggle_full_screen(&mut self) {
        self.log.push("full".into());
    }
    fn toggle_theater(&mut self) {
        self.log.push("theater".into());
    }
    fn toggle_subtitle_panel(&mut self) {
        self.log.push("panel".into());
    }
    fn leave_full_screen_or_theater(&mut self) {
        self.log.push("leave".into());
    }
}

#[test]
fn shortcuts_act_only_while_a_player_is_on_screen() {
    let mut t = Target::default();
    assert!(!perform(PlayerShortcut::TogglePlayback, false, &mut t));
    assert!(t.log.is_empty());
}

#[test]
fn toggle_keys_ignore_auto_repeat_and_full_screen_keeps_the_panel_setting() {
    let mut t = Target { shown: true, ..Target::default() };
    assert!(perform(PlayerShortcut::ToggleMute, true, &mut t), "a swallowed repeat is consumed");
    assert!(t.log.is_empty());
    assert!(perform(PlayerShortcut::Seek(5.0), true, &mut t));
    assert_eq!(t.log, vec!["seek 5"]);
    t.full = true;
    assert!(perform(PlayerShortcut::TogglePanel, false, &mut t));
    assert_eq!(t.log, vec!["seek 5"], "E in full screen does not change the panel setting");
}

#[test]
fn video_clicks() {
    assert_eq!(video_click_actions(1), vec![PlayerShortcut::TogglePlayback]);
    assert_eq!(video_click_actions(2), vec![PlayerShortcut::TogglePlayback, PlayerShortcut::ToggleFullScreen]);
    assert!(video_click_actions(3).is_empty());
}

#[test]
fn volume_moves_in_whole_five_percent_steps_and_counts_from_silence_while_muted() {
    assert_eq!(adjusted_volume(0.7, false, 0.05), (0.75, false));
    assert_eq!(adjusted_volume(0.98, false, 0.05), (1.0, false));
    assert_eq!(adjusted_volume(0.8, true, 0.05), (0.05, false), "muted counts from silence and unmutes above zero");
    assert_eq!(adjusted_volume(0.0, true, -0.05), (0.0, true));
}

#[test]
fn scroll_notches_accumulate_precise_deltas_and_reset_on_direction_change() {
    let mut s = ScrollNotches::default();
    assert_eq!(s.feed(3.0, false, false), 1);
    assert_eq!(s.feed(-0.5, false, false), -1);
    assert_eq!(s.feed(4.0, true, false), 0);
    assert_eq!(s.feed(7.0, true, false), 1);
    assert_eq!(s.feed(-3.0, true, false), 0, "sign change resets the remainder");
    assert_eq!(s.feed(-8.0, true, false), -1);
    assert_eq!(s.feed(50.0, true, true), 0, "momentum ignored");
    assert_eq!(s.feed(f64::NAN, true, false), 0);
    assert_eq!(s.feed(25.0, true, false), 2);
}
