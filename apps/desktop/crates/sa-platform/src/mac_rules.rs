//! Pure macOS rules used by [`crate::mac`], compiled (and tested) on every platform.

use std::path::{Path, PathBuf};

use sa_core::platform::{AlertButtonRole, QuickLocation, QuickLocationKind};

/// Swift `ExplorerPaths.volumeRoot(of:)`: `/Volumes/<name>` for a path below a mounted-volume folder, `None` for
/// everything else (including `/Volumes` itself).
pub fn volume_root(path: &str) -> Option<String> {
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    (parts.len() >= 2 && parts[0] == "Volumes").then(|| format!("/Volumes/{}", parts[1]))
}

/// LibraryKit `MediaPath.normalized`: `URL(fileURLWithPath:).standardizedFileURL.path` in Unicode NFC
/// (`precomposedStringWithCanonicalMapping`): an absolute, standardized path without a trailing slash (`/` stays);
/// `""` for an empty path.
pub fn normalized(path: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    if path.is_empty() {
        return String::new();
    }
    let mut out: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            p => out.push(p),
        }
    }
    let joined = if out.is_empty() { "/".to_owned() } else { format!("/{}", out.join("/")) };
    joined.nfc().collect()
}

/// A mounted volume as `NSFileManager.mountedVolumeURLs` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeEntry {
    pub path: PathBuf,
    /// `volumeName ?? volumeLocalizedName ?? lastPathComponent`.
    pub name: String,
}

/// LibraryKit `QuickLocations.current` (mac-librarykit §16): Movies, Downloads, Desktop that exist as folders, then
/// the visible mounted volumes except the startup disk; de-duplicated by the lowercased normalized path.
pub fn build_quick_locations(
    movies: Option<PathBuf>,
    downloads: Option<PathBuf>,
    desktop: Option<PathBuf>,
    volumes: &[VolumeEntry],
    is_dir: &dyn Fn(&Path) -> bool,
) -> Vec<QuickLocation> {
    let mut out = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    let mut add = |kind: QuickLocationKind, title: String, path: &Path, out: &mut Vec<QuickLocation>| {
        let n = normalized(&path.to_string_lossy());
        let key = n.to_lowercase();
        if !seen.contains(&key) {
            seen.push(key);
            out.push(QuickLocation { title, path: PathBuf::from(n), kind });
        }
    };
    for (kind, key, path) in [
        (QuickLocationKind::Videos, "Filmler", movies),
        (QuickLocationKind::Downloads, "İndirilenler", downloads),
        (QuickLocationKind::Desktop, "Masaüstü", desktop),
    ] {
        if let Some(p) = path.filter(|p| is_dir(p)) {
            add(kind, sa_core::l(key), &p, &mut out);
        }
    }
    for v in volumes {
        if normalized(&v.path.to_string_lossy()) == "/" {
            continue;
        }
        add(QuickLocationKind::Volume, v.name.clone(), &v.path, &mut out);
    }
    out
}

/// Storage check of the hub (mac `FileExplorerModel.mountAvailability`): paths outside `/Volumes/<name>` are always
/// available; otherwise the volume must be mounted (or be the startup disk's link).
pub fn storage_available(path: &str, is_mounted: &dyn Fn(&str) -> bool) -> bool {
    if path.trim().is_empty() {
        return false;
    }
    match volume_root(path) {
        None => true,
        Some(root) => is_mounted(&root),
    }
}

/// `appTheme` → `NSApp.appearance` name (`None` = follow the system), mac-ui §14.1.
pub fn appearance_name(theme: Option<&str>) -> Option<&'static str> {
    match theme {
        Some("light") => Some("NSAppearanceNameAqua"),
        Some("dark") => Some("NSAppearanceNameDarkAqua"),
        _ => None,
    }
}

/// What a double click on the title bar / unified toolbar does (`AppleActionOnDoubleClick`, older
/// `AppleMiniaturizeOnDoubleClick`; default Maximize = zoom).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleBarDoubleClick {
    Zoom,
    Minimize,
    None,
}

pub fn title_bar_double_click(action: Option<&str>, miniaturize: Option<bool>) -> TitleBarDoubleClick {
    match action {
        Some(a) if a.eq_ignore_ascii_case("Minimize") => TitleBarDoubleClick::Minimize,
        Some(a) if a.eq_ignore_ascii_case("None") => TitleBarDoubleClick::None,
        Some(a) if a.eq_ignore_ascii_case("Maximize") || a.eq_ignore_ascii_case("Fill") => TitleBarDoubleClick::Zoom,
        _ if miniaturize == Some(true) => TitleBarDoubleClick::Minimize,
        _ => TitleBarDoubleClick::Zoom,
    }
}

/// `MediaFileTypes.isAudio` (no picture to show).
pub fn is_audio(path: &Path) -> bool {
    const AUDIO: [&str; 8] = ["mp3", "wav", "m4a", "aac", "flac", "ogg", "opus", "wma"];
    path.extension().map(|e| e.to_string_lossy().to_lowercase()).is_some_and(|e| AUDIO.contains(&e.as_str()))
}

/// Thumbnail time: a tenth of the way in, at most 30 s (0 for an unknown duration).
pub fn thumbnail_seconds(duration: f64) -> f64 {
    if duration.is_finite() && duration > 0.0 { (duration * 0.1).min(30.0) } else { 0.0 }
}

/// Points → pixels for a rasterized symbol (rounded up, at least 1).
pub fn pixel_extent(points: f64, scale: f64) -> usize {
    ((points * scale).ceil() as usize).max(1)
}

/// `NSAlertFirstButtonReturn` (1000): the first added button.
pub const ALERT_FIRST_BUTTON_RETURN: isize = 1000;

/// An `NSModalResponse` of an alert with `count` buttons → the button index (`None` for anything else).
pub fn alert_button_index(response: isize, count: usize) -> Option<usize> {
    let i = response.checked_sub(ALERT_FIRST_BUTTON_RETURN)?;
    (i >= 0 && (i as usize) < count).then_some(i as usize)
}

/// The key equivalent an alert button needs beyond AppKit's defaults (the first button already answers Return):
/// Esc for the cancel role, whatever its title (AppKit only maps Esc to a button titled "Cancel").
pub fn alert_key_equivalent(role: AlertButtonRole) -> Option<&'static str> {
    match role {
        AlertButtonRole::Cancel => Some("\u{1b}"),
        AlertButtonRole::Default | AlertButtonRole::Destructive => None,
    }
}

/// An sRGB color (0..1 components) as `#RRGGBB` (the overlay text color setting, mac-ui §11.3; the panel shows no
/// opacity, so alpha is ignored).
pub fn hex_from_components(rgb: [f64; 3]) -> String {
    let c = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02X}{:02X}{:02X}", c(rgb[0]), c(rgb[1]), c(rgb[2]))
}

/// One `NSApp.windows` entry as the Dock reopen handling sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReopenWindow {
    /// `isVisible` (on screen; a closed window is not). A miniaturized window is never counted as visible, whatever
    /// AppKit reports here.
    pub visible: bool,
    /// `isMiniaturized` (in the Dock).
    pub miniaturized: bool,
    /// An `NSPanel` (color panel, alerts): AppKit hides panels when the app deactivates, so they never count as a
    /// window the user could return to.
    pub panel: bool,
}

/// What a Dock-icon click (`'aevt','rapp'`) does once a hidden app is unhidden.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReopenAction {
    /// A window is visible: nothing (activation already brought the app to the front).
    Nothing,
    /// No window is visible but this one (index into the list) is in the Dock: deminiaturize it.
    Deminiaturize(usize),
    /// No window left: open a new main window.
    OpenWindow,
}

/// AppKit's default reopen handling as a SwiftUI `WindowGroup` app gets it (mac-ui §2.3): with a visible window
/// nothing happens; otherwise the first miniaturized window comes back; with none left a new main window opens.
pub fn reopen_action(windows: &[ReopenWindow]) -> ReopenAction {
    let regular = || windows.iter().enumerate().filter(|(_, w)| !w.panel);
    if regular().any(|(_, w)| w.visible && !w.miniaturized) {
        return ReopenAction::Nothing;
    }
    match regular().find(|(_, w)| w.miniaturized) {
        Some((i, _)) => ReopenAction::Deminiaturize(i),
        None => ReopenAction::OpenWindow,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_roots() {
        assert_eq!(volume_root("/Volumes/Media/Films/a.mp4").as_deref(), Some("/Volumes/Media"));
        assert_eq!(volume_root("/Volumes/Media").as_deref(), Some("/Volumes/Media"));
        assert_eq!(volume_root("/Volumes"), None);
        assert_eq!(volume_root("/Users/me/Movies/a.mp4"), None);
    }

    #[test]
    fn paths_normalize() {
        assert_eq!(normalized("/Users/me/Movies/"), "/Users/me/Movies");
        assert_eq!(normalized("/Users/me/./x/../Movies"), "/Users/me/Movies");
        assert_eq!(normalized("/"), "/");
        assert_eq!(normalized(""), "");
        // Decomposed names (as some volumes report them) compare equal to the precomposed form.
        assert_eq!(normalized("/Volumes/Mu\u{308}zik/"), "/Volumes/M\u{fc}zik");
    }

    #[test]
    fn quick_locations_follow_librarykit() {
        sa_core::l10n::LanguageState::shared().set_language(sa_core::l10n::UiLanguage::Tr);
        let vols = vec![
            VolumeEntry { path: PathBuf::from("/"), name: "Macintosh HD".into() },
            VolumeEntry { path: PathBuf::from("/Volumes/Media"), name: "Media".into() },
            VolumeEntry { path: PathBuf::from("/Volumes/media/"), name: "dup".into() },
        ];
        let q = build_quick_locations(
            Some(PathBuf::from("/Users/me/Movies")),
            Some(PathBuf::from("/Users/me/Downloads")),
            Some(PathBuf::from("/Users/me/Desktop")),
            &vols,
            &|p| !p.ends_with("Downloads"),
        );
        let rows: Vec<_> = q.iter().map(|q| (q.title.clone(), q.path.to_string_lossy().into_owned(), q.kind)).collect();
        assert_eq!(
            rows,
            vec![
                (sa_core::l("Filmler"), "/Users/me/Movies".to_owned(), QuickLocationKind::Videos),
                (sa_core::l("Masaüstü"), "/Users/me/Desktop".to_owned(), QuickLocationKind::Desktop),
                ("Media".to_owned(), "/Volumes/Media".to_owned(), QuickLocationKind::Volume),
            ]
        );
    }

    #[test]
    fn storage_needs_a_mounted_volume_only_below_volumes() {
        let mounted = |r: &str| r == "/Volumes/Media";
        assert!(storage_available("/Users/me/a.mp4", &mounted));
        assert!(storage_available("/Volumes/Media/a.mp4", &mounted));
        assert!(!storage_available("/Volumes/Gone/a.mp4", &mounted));
        assert!(!storage_available("", &mounted));
    }

    #[test]
    fn theme_and_double_click_mapping() {
        assert_eq!(appearance_name(Some("light")), Some("NSAppearanceNameAqua"));
        assert_eq!(appearance_name(Some("dark")), Some("NSAppearanceNameDarkAqua"));
        assert_eq!(appearance_name(Some("system")), None);
        assert_eq!(appearance_name(Some("zzz")), None);
        assert_eq!(appearance_name(None), None);
        assert_eq!(title_bar_double_click(None, None), TitleBarDoubleClick::Zoom);
        assert_eq!(title_bar_double_click(Some("Minimize"), None), TitleBarDoubleClick::Minimize);
        assert_eq!(title_bar_double_click(Some("None"), Some(true)), TitleBarDoubleClick::None);
        assert_eq!(title_bar_double_click(None, Some(true)), TitleBarDoubleClick::Minimize);
    }

    #[test]
    fn alert_responses_and_keys() {
        assert_eq!(alert_button_index(1000, 2), Some(0));
        assert_eq!(alert_button_index(1001, 2), Some(1));
        assert_eq!(alert_button_index(1002, 2), None);
        assert_eq!(alert_button_index(-1000, 2), None);
        assert_eq!(alert_button_index(0, 2), None);
        assert_eq!(alert_key_equivalent(AlertButtonRole::Cancel), Some("\u{1b}"));
        assert_eq!(alert_key_equivalent(AlertButtonRole::Destructive), None);
        assert_eq!(hex_from_components([1.0, 0.5, 0.0]), "#FF8000");
        assert_eq!(hex_from_components([-1.0, 2.0, 0.2]), "#00FF33");
    }

    #[test]
    fn thumbnail_rules() {
        assert!(is_audio(Path::new("/a/b.MP3")));
        assert!(!is_audio(Path::new("/a/b.mp4")));
        assert_eq!(thumbnail_seconds(100.0), 10.0);
        assert_eq!(thumbnail_seconds(1000.0), 30.0);
        assert_eq!(thumbnail_seconds(f64::NAN), 0.0);
        assert_eq!(pixel_extent(13.2, 2.0), 27);
        assert_eq!(pixel_extent(0.0, 2.0), 1);
    }

    #[test]
    fn dock_reopen_opens_a_window_only_when_none_is_left() {
        let visible = ReopenWindow { visible: true, ..Default::default() };
        let mini = ReopenWindow { miniaturized: true, ..Default::default() };
        let closed = ReopenWindow::default();
        let panel = ReopenWindow { visible: true, panel: true, ..Default::default() };
        assert_eq!(reopen_action(&[]), ReopenAction::OpenWindow);
        assert_eq!(reopen_action(&[closed, closed]), ReopenAction::OpenWindow);
        assert_eq!(reopen_action(&[panel]), ReopenAction::OpenWindow);
        assert_eq!(reopen_action(&[closed, visible]), ReopenAction::Nothing);
        assert_eq!(reopen_action(&[mini, visible]), ReopenAction::Nothing);
        assert_eq!(reopen_action(&[closed, panel, mini, mini]), ReopenAction::Deminiaturize(2));
        let mini_panel = ReopenWindow { miniaturized: true, panel: true, ..Default::default() };
        assert_eq!(reopen_action(&[mini_panel]), ReopenAction::OpenWindow);
        // A window in the Dock that AppKit still reports as visible is not on screen: it comes back.
        let mini_visible = ReopenWindow { visible: true, miniaturized: true, ..Default::default() };
        assert_eq!(reopen_action(&[mini_visible]), ReopenAction::Deminiaturize(0));
        assert_eq!(reopen_action(&[closed, mini_visible, visible]), ReopenAction::Nothing);
    }
}
