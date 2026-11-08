//! Workspace layout state (win-ui-main §8 `apply_workspace_layout`, mac-ui §3.3 / §6 / §7.1 content selection).
//! Pure: the controller pushes the result into `MainBridge` / `AppBridge`.

/// Layout modes of `MainBridge.layout-mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LayoutMode {
    /// The player fills the work area (panel hidden or full screen).
    PlayerOnly = 0,
    /// Player | splitter | editor.
    SideBySide = 1,
    /// Player over splitter over editor.
    Stacked = 2,
    /// macOS: a subtitle file without a video shows the line panel alone.
    EditorOnly = 3,
}

/// One grid track (star weight + minimum) of the work area.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Track {
    pub star: f64,
    pub min: f64,
}

impl Track {
    const fn star(star: f64, min: f64) -> Self {
        Self { star, min }
    }
    const ZERO: Track = Track { star: 0.0, min: 0.0 };
}

/// The persisted and transient toggles (win-ui-main §8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LayoutFlags {
    /// `showFileExplorer` (default true).
    pub explorer_open: bool,
    /// `hideSubtitlePanel` (default false).
    pub editor_hidden: bool,
    /// `sideBySideLayout` (default false).
    pub side_by_side: bool,
    /// Not persisted.
    pub theater: bool,
    /// Not persisted.
    pub full_screen: bool,
}

/// Everything `apply_workspace_layout()` sets on Windows.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowsLayout {
    pub explorer_toggles_checked: bool,
    pub show_explorer: bool,
    pub explorer_column: f64,
    pub panel_button_checked: bool,
    pub side_by_side_checked: bool,
    pub side_by_side_enabled: bool,
    pub theater_checked: bool,
    pub full_screen_checked: bool,
    /// U+E73F while full screen, else U+E740.
    pub full_screen_glyph: char,
    pub command_bar_visible: bool,
    pub title_bar_row: f64,
    pub title_bar_visible: bool,
    pub status_bar_visible: bool,
    /// Horizontal margin of the content grid.
    pub content_margin: f64,
    pub player_radius: f64,
    pub mode: LayoutMode,
    pub rows: [Track; 3],
    pub cols: [Track; 3],
    /// (minimum, other minimum) of the splitter, `None` when hidden.
    pub splitter_minimums: Option<(f64, f64)>,
}

/// win-ui-main §8 `apply_workspace_layout()`.
pub fn windows_layout(f: LayoutFlags) -> WindowsLayout {
    let show_explorer = f.explorer_open && !f.full_screen;
    let (mode, rows, cols, splitter) = if f.editor_hidden || f.full_screen {
        (LayoutMode::PlayerOnly, [Track::star(1.0, 0.0), Track::ZERO, Track::ZERO], [Track::star(1.0, 0.0), Track::ZERO, Track::ZERO], None)
    } else if f.side_by_side {
        (
            LayoutMode::SideBySide,
            [Track::star(1.0, 0.0), Track::ZERO, Track::ZERO],
            [Track::star(2.0, 260.0), Track { star: 0.0, min: 12.0 }, Track::star(1.0, 320.0)],
            Some((260.0, 320.0)),
        )
    } else {
        (
            LayoutMode::Stacked,
            [Track::star(5.0, 180.0), Track { star: 0.0, min: 12.0 }, Track::star(4.0, 160.0)],
            [Track::star(1.0, 0.0), Track::ZERO, Track::ZERO],
            Some((180.0, 160.0)),
        )
    };
    WindowsLayout {
        explorer_toggles_checked: f.explorer_open,
        show_explorer,
        explorer_column: if show_explorer { 280.0 } else { 0.0 },
        panel_button_checked: !f.editor_hidden,
        side_by_side_checked: f.side_by_side,
        side_by_side_enabled: !f.editor_hidden && !f.full_screen,
        theater_checked: f.theater,
        full_screen_checked: f.full_screen,
        full_screen_glyph: if f.full_screen { '\u{E73F}' } else { '\u{E740}' },
        command_bar_visible: !(f.theater || f.full_screen),
        title_bar_row: if f.full_screen { 0.0 } else { 48.0 },
        title_bar_visible: !f.full_screen,
        status_bar_visible: !f.full_screen,
        content_margin: if f.full_screen { 0.0 } else { 12.0 },
        player_radius: if f.full_screen { 0.0 } else { 8.0 },
        mode,
        rows,
        cols,
        splitter_minimums: splitter,
    }
}

/// What the macOS editor workspace shows (mac-ui §7.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacEditorContent {
    DropZone,
    /// Only a subtitle file: the line panel alone, no player.
    EditorOnly,
    PlayerOnly,
    SideBySide,
    Stacked,
}

/// mac-ui §7.1: `shows_subtitle_panel = !(is_player_shown && (hide_panel || full_screen))`.
pub fn mac_editor_content(has_video: bool, has_sentences: bool, f: LayoutFlags) -> MacEditorContent {
    if !has_video && !has_sentences {
        MacEditorContent::DropZone
    } else if !has_video {
        MacEditorContent::EditorOnly
    } else if f.editor_hidden || f.full_screen {
        MacEditorContent::PlayerOnly
    } else if f.side_by_side {
        MacEditorContent::SideBySide
    } else {
        MacEditorContent::Stacked
    }
}

impl MacEditorContent {
    pub fn layout_mode(self) -> LayoutMode {
        match self {
            MacEditorContent::DropZone | MacEditorContent::Stacked => LayoutMode::Stacked,
            MacEditorContent::EditorOnly => LayoutMode::EditorOnly,
            MacEditorContent::PlayerOnly => LayoutMode::PlayerOnly,
            MacEditorContent::SideBySide => LayoutMode::SideBySide,
        }
    }
}

/// macOS window chrome rules (mac-ui §3.1, §3.3, §6).
pub mod mac {
    /// `sidebarMinWidth`.
    pub const SIDEBAR_MIN_WIDTH: f64 = 220.0;
    pub const SIDEBAR_IDEAL_WIDTH: f64 = 260.0;
    pub const SIDEBAR_MAX_WIDTH: f64 = 420.0;
    /// `detailMinWidth`.
    pub const DETAIL_MIN_WIDTH: f64 = 600.0;
    /// `minimumWindowWidth` = sidebar + detail, so SwiftUI-like folding never reports a user choice.
    pub const MIN_WINDOW_WIDTH: f64 = SIDEBAR_MIN_WIDTH + DETAIL_MIN_WIDTH;
    pub const MIN_WINDOW_HEIGHT: f64 = 600.0;
    pub const DEFAULT_WINDOW_SIZE: (f64, f64) = (1180.0, 720.0);

    /// §3.3: the sidebar is visible iff the stored choice is on and the window is not full screen.
    pub fn sidebar_visible(stored: bool, full_screen: bool) -> bool {
        stored && !full_screen
    }

    /// §3.3: a user change of the sidebar is stored, except while full screen (not a choice there).
    pub fn sidebar_change_is_stored(full_screen: bool) -> bool {
        !full_screen
    }

    /// §6: full screen with a player shown hides the workspace switcher, dividers and the status bar.
    pub fn player_fills_detail(is_player_shown: bool, full_screen: bool) -> bool {
        is_player_shown && full_screen
    }

    /// Clamps a dragged sidebar width.
    pub fn clamp_sidebar_width(w: f64) -> f64 {
        if w.is_finite() { w.clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH) } else { SIDEBAR_IDEAL_WIDTH }
    }
}

/// Windows window size restore (win-ui-main §4.1): stored physical size if both > 0 (raised to the minimum), else
/// the default, each clamped to the work area.
pub fn windows_initial_size(stored: Option<(i64, i64)>, scale: f64, work_area: (f64, f64)) -> (f64, f64) {
    let (min_w, min_h) = (760.0 * scale, 580.0 * scale);
    let (w, h) = match stored {
        Some((w, h)) if w > 0 && h > 0 => ((w as f64).max(min_w), (h as f64).max(min_h)),
        _ => (1240.0 * scale, 840.0 * scale),
    };
    (w.min(work_area.0), h.min(work_area.1))
}

/// win-ui-main §4.3: the size is stored only from the normal (restored) state.
pub fn windows_should_store_size(maximized: bool, minimized: bool, full_screen: bool) -> bool {
    !(maximized || minimized || full_screen)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flags(explorer: bool, hidden: bool, side: bool, theater: bool, full: bool) -> LayoutFlags {
        LayoutFlags { explorer_open: explorer, editor_hidden: hidden, side_by_side: side, theater, full_screen: full }
    }

    #[test]
    fn every_flag_combination_follows_the_windows_table() {
        for bits in 0..32u8 {
            let f = flags(bits & 1 != 0, bits & 2 != 0, bits & 4 != 0, bits & 8 != 0, bits & 16 != 0);
            let l = windows_layout(f);
            assert_eq!(l.show_explorer, f.explorer_open && !f.full_screen, "{f:?}");
            assert_eq!(l.explorer_column, if l.show_explorer { 280.0 } else { 0.0 });
            assert_eq!(l.explorer_toggles_checked, f.explorer_open);
            assert_eq!(l.panel_button_checked, !f.editor_hidden);
            assert_eq!(l.side_by_side_enabled, !f.editor_hidden && !f.full_screen);
            assert_eq!(l.command_bar_visible, !f.theater && !f.full_screen);
            assert_eq!(l.title_bar_row, if f.full_screen { 0.0 } else { 48.0 });
            assert_eq!(l.status_bar_visible, !f.full_screen);
            assert_eq!(l.content_margin, if f.full_screen { 0.0 } else { 12.0 });
            assert_eq!(l.player_radius, if f.full_screen { 0.0 } else { 8.0 });
            assert_eq!(l.full_screen_glyph, if f.full_screen { '\u{E73F}' } else { '\u{E740}' });
            let expected = if f.editor_hidden || f.full_screen {
                LayoutMode::PlayerOnly
            } else if f.side_by_side {
                LayoutMode::SideBySide
            } else {
                LayoutMode::Stacked
            };
            assert_eq!(l.mode, expected, "{f:?}");
        }
    }

    #[test]
    fn tracks_and_splitter_minimums_match_the_spec() {
        let stacked = windows_layout(flags(true, false, false, false, false));
        assert_eq!(stacked.rows, [Track::star(5.0, 180.0), Track { star: 0.0, min: 12.0 }, Track::star(4.0, 160.0)]);
        assert_eq!(stacked.splitter_minimums, Some((180.0, 160.0)));
        let side = windows_layout(flags(true, false, true, false, false));
        assert_eq!(side.cols, [Track::star(2.0, 260.0), Track { star: 0.0, min: 12.0 }, Track::star(1.0, 320.0)]);
        assert_eq!(side.splitter_minimums, Some((260.0, 320.0)));
        let only = windows_layout(flags(true, true, true, false, false));
        assert_eq!(only.splitter_minimums, None);
        assert_eq!(only.cols[0], Track::star(1.0, 0.0));
    }

    #[test]
    fn mac_content_follows_video_lines_and_panel() {
        let f = LayoutFlags::default();
        assert_eq!(mac_editor_content(false, false, f), MacEditorContent::DropZone);
        assert_eq!(mac_editor_content(false, true, f), MacEditorContent::EditorOnly);
        assert_eq!(mac_editor_content(true, false, f), MacEditorContent::Stacked);
        assert_eq!(mac_editor_content(true, true, LayoutFlags { side_by_side: true, ..f }), MacEditorContent::SideBySide);
        assert_eq!(mac_editor_content(true, true, LayoutFlags { editor_hidden: true, ..f }), MacEditorContent::PlayerOnly);
        // Full screen hides the panel without touching the stored setting.
        assert_eq!(mac_editor_content(true, true, LayoutFlags { full_screen: true, ..f }), MacEditorContent::PlayerOnly);
        // A subtitle without a video never hides the panel (no player is shown).
        assert_eq!(mac_editor_content(false, true, LayoutFlags { editor_hidden: true, full_screen: true, ..f }), MacEditorContent::EditorOnly);
    }

    #[test]
    fn the_window_is_never_narrower_than_the_sidebar_and_the_detail_need_together() {
        assert!(mac::MIN_WINDOW_WIDTH >= mac::SIDEBAR_MIN_WIDTH + mac::DETAIL_MIN_WIDTH);
        assert_eq!(mac::MIN_WINDOW_WIDTH, 820.0);
        assert!(!mac::sidebar_visible(true, true));
        assert!(mac::sidebar_visible(true, false));
        assert!(!mac::sidebar_change_is_stored(true));
        assert_eq!(mac::clamp_sidebar_width(100.0), 220.0);
        assert_eq!(mac::clamp_sidebar_width(900.0), 420.0);
        assert_eq!(mac::clamp_sidebar_width(f64::NAN), 260.0);
    }

    #[test]
    fn windows_size_restore_uses_stored_size_raised_to_the_minimum_and_clamped() {
        assert_eq!(windows_initial_size(None, 1.0, (4000.0, 4000.0)), (1240.0, 840.0));
        assert_eq!(windows_initial_size(Some((0, 900)), 1.5, (4000.0, 4000.0)), (1860.0, 1260.0));
        assert_eq!(windows_initial_size(Some((500, 400)), 1.0, (4000.0, 4000.0)), (760.0, 580.0));
        assert_eq!(windows_initial_size(Some((3000, 2000)), 1.0, (1920.0, 1040.0)), (1920.0, 1040.0));
        assert!(windows_should_store_size(false, false, false));
        assert!(!windows_should_store_size(true, false, false));
        assert!(!windows_should_store_size(false, false, true));
    }
}
