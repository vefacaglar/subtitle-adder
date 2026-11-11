//! Native open/save dialogs and the alert fallback: on Windows `IFileOpenDialog`/`IFileSaveDialog` set up like the
//! WinUI pickers ([`crate::win::file_dialog`]: Videos as the *default* folder, so the last-used folder wins, as with
//! `SuggestedStartLocation`; win-ui-main §18); on macOS `rfd` (NSOpenPanel/NSSavePanel attached to the key window,
//! mac-ui §6.3).
//!
//! Parenting: the app registers the window that owns dialogs with [`set_dialog_parent`] (main window on Windows, the
//! key main window on macOS, updated when another window becomes key). The parent lives in a thread-local of the UI
//! thread, where every dialog is started.
//!
//! Alerts: [`set_alert_presenter`] installs the app's own presenter (Windows: the ContentDialog-style overlay drawn
//! in Slint). Without one, alerts fall back to an `rfd` message dialog (a sheet on macOS when parented).

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle, WindowHandle,
};
use sa_core::engine_types::SubtitleFormat;
use sa_core::future::LocalBoxFuture;
use sa_core::platform::{Alert, AlertButtonRole, AlertResponse, FileFilter};

/// The media extensions of the Windows pickers (`MainWindow.VideoTypes`, widened) and of the macOS video picker
/// (`MediaFileTypes.mediaExtensions`): [`sa_core::media_types::MEDIA_EXTENSIONS`].
pub const MEDIA_EXTENSIONS: &[&str] = &sa_core::media_types::MEDIA_EXTENSIONS;

/// `SubtitleParser.Extensions` (both apps).
pub const SUBTITLE_EXTENSIONS: [&str; 5] = ["srt", "vtt", "ass", "ssa", "json"];

/// Which platform's picker rules to apply (Windows rules on Windows, macOS rules elsewhere; tests pick explicitly).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogFlavor {
    Windows,
    Mac,
}

impl DialogFlavor {
    pub const fn native() -> Self {
        if cfg!(windows) { DialogFlavor::Windows } else { DialogFlavor::Mac }
    }
}

/// Extensions per filter (without dots), native flavor.
pub fn filter_extensions(filter: &FileFilter) -> Vec<&'static str> {
    filter_extensions_for(filter, DialogFlavor::native())
}

/// Extensions per filter (without dots). macOS' subtitle picker also accepts plain text (`txt`, mac-ui §6.3).
pub fn filter_extensions_for(filter: &FileFilter, flavor: DialogFlavor) -> Vec<&'static str> {
    match filter {
        FileFilter::Videos => MEDIA_EXTENSIONS.to_vec(),
        FileFilter::Subtitles => {
            let mut v = SUBTITLE_EXTENSIONS.to_vec();
            if flavor == DialogFlavor::Mac {
                v.push("txt");
            }
            v
        }
        FileFilter::Subtitle(f) => vec![f.raw()],
        FileFilter::Any => Vec::new(),
    }
}

/// The filter's display name: WinRT pickers show the pattern list, so the dialog shows the same (`*.srt; *.vtt`);
/// a one-format export shows the upper-cased format (`SRT`, win-ui-main §18.4). The media filter's ~100 patterns
/// would make the type box unusable, so it shows a localized label instead
/// ([`sa_core::media_types::media_filter_label`]); the pattern list still filters.
pub fn filter_name(filter: &FileFilter, flavor: DialogFlavor) -> String {
    match filter {
        FileFilter::Subtitle(f) => f.raw().to_uppercase(),
        FileFilter::Videos => sa_core::media_types::media_filter_label(),
        other => filter_extensions_for(other, flavor).iter().map(|e| format!("*.{e}")).collect::<Vec<_>>().join("; "),
    }
}

/// `Path.HasExtension` (.NET): the file name has a `.` followed by at least one character.
pub fn has_extension(path: &Path) -> bool {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    match name.rfind('.') {
        Some(i) => i + 1 < name.len(),
        None => false,
    }
}

/// Windows export rule: append `.<raw>` when the chosen path has no extension; macOS keeps the exact name.
pub fn finish_save_path(path: PathBuf, format: SubtitleFormat, flavor: DialogFlavor) -> PathBuf {
    if flavor == DialogFlavor::Windows && !has_extension(&path) {
        let mut s = path.into_os_string();
        s.push(".");
        s.push(format.raw());
        PathBuf::from(s)
    } else {
        path
    }
}

/// Suggested name in the save dialog: Windows drops the extension (`Path.GetFileNameWithoutExtension`, the dialog
/// adds the default one); macOS keeps the exact `<stem>[.<lang>].<fmt>` name.
pub fn suggested_save_name(name: &str, flavor: DialogFlavor) -> String {
    match flavor {
        DialogFlavor::Mac => name.to_owned(),
        DialogFlavor::Windows => {
            // .NET `Path.GetFileName`: after the last separator, never inside the root (`C:film.srt` → `film.srt`).
            let rest = match name.as_bytes() {
                [d, b':', ..] if d.is_ascii_alphabetic() => &name[2..],
                _ => name,
            };
            let file = rest.rsplit(['\\', '/']).next().unwrap_or(rest);
            match file.rfind('.') {
                Some(i) => file[..i].to_owned(),
                None => file.to_owned(),
            }
        }
    }
}

// ---- parent window ----------------------------------------------------------------------------------------------

/// Raw handles of the window that owns dialogs (copied out of a `HasWindowHandle`; valid while that window lives).
#[derive(Clone, Copy)]
struct ParentHandles {
    window: RawWindowHandle,
    display: RawDisplayHandle,
}

impl HasWindowHandle for ParentHandles {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        // SAFETY: the app clears the parent before the window is destroyed (`clear_dialog_parent`).
        Ok(unsafe { WindowHandle::borrow_raw(self.window) })
    }
}

impl HasDisplayHandle for ParentHandles {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        // SAFETY: see above.
        Ok(unsafe { DisplayHandle::borrow_raw(self.display) })
    }
}

thread_local! {
    static PARENT: RefCell<Option<ParentHandles>> = const { RefCell::new(None) };
    static PRESENTER: RefCell<Option<std::rc::Rc<AlertPresenter>>> = const { RefCell::new(None) };
}

/// Makes `window` the owner of the following dialogs (UI thread). Call again when the key window changes (macOS).
pub fn set_dialog_parent<W: HasWindowHandle + HasDisplayHandle + ?Sized>(window: &W) {
    let handles = match (window.window_handle(), window.display_handle()) {
        (Ok(w), Ok(d)) => Some(ParentHandles { window: w.as_raw(), display: d.as_raw() }),
        _ => None,
    };
    PARENT.with(|p| *p.borrow_mut() = handles);
}

/// Forgets the owner (before the window is destroyed).
pub fn clear_dialog_parent() {
    PARENT.with(|p| *p.borrow_mut() = None);
}

fn parent() -> Option<ParentHandles> {
    PARENT.with(|p| *p.borrow())
}

/// The `NSView*` of the window that owns dialogs (macOS sheets).
#[cfg(target_os = "macos")]
pub(crate) fn parent_ns_view() -> Option<*mut std::ffi::c_void> {
    match parent()?.window {
        RawWindowHandle::AppKit(h) => Some(h.ns_view.as_ptr()),
        _ => None,
    }
}

// ---- file dialogs -----------------------------------------------------------------------------------------------

/// The owner `HWND` of the dialogs (0 = none).
#[cfg(windows)]
fn parent_hwnd() -> isize {
    match parent().map(|p| p.window) {
        Some(RawWindowHandle::Win32(h)) => h.hwnd.get(),
        _ => 0,
    }
}

#[cfg(windows)]
fn windows_open_request(filter: &FileFilter, start_dir: Option<PathBuf>, multiple: bool) -> crate::win::file_dialog::DialogRequest {
    let exts = filter_extensions_for(filter, DialogFlavor::Windows);
    crate::win::file_dialog::DialogRequest {
        filter: (!exts.is_empty())
            .then(|| (filter_name(filter, DialogFlavor::Windows), exts.iter().map(|e| (*e).to_owned()).collect())),
        multiple,
        file_name: None,
        default_extension: None,
        start_dir,
        owner: parent_hwnd(),
    }
}

#[cfg_attr(windows, allow(dead_code))]
fn open_dialog(filter: &FileFilter, start_dir: Option<PathBuf>) -> rfd::AsyncFileDialog {
    let flavor = DialogFlavor::native();
    let mut d = rfd::AsyncFileDialog::new();
    let exts = filter_extensions_for(filter, flavor);
    if !exts.is_empty() {
        d = d.add_filter(filter_name(filter, flavor), &exts);
    }
    if let Some(dir) = start_dir.filter(|d| d.is_dir()) {
        d = d.set_directory(dir);
    }
    if let Some(p) = parent() {
        d = d.set_parent(&p);
    }
    d
}

/// One file.
pub async fn pick_open_file(filter: FileFilter, start_dir: Option<PathBuf>) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        crate::win::file_dialog::open(windows_open_request(&filter, start_dir, false)).await.into_iter().next()
    }
    #[cfg(not(windows))]
    {
        open_dialog(&filter, start_dir).pick_file().await.map(|h| h.path().to_path_buf())
    }
}

/// Several files.
pub async fn pick_open_files(filter: FileFilter, start_dir: Option<PathBuf>) -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        crate::win::file_dialog::open(windows_open_request(&filter, start_dir, true)).await
    }
    #[cfg(not(windows))]
    {
        open_dialog(&filter, start_dir)
            .pick_files()
            .await
            .map(|v| v.into_iter().map(|h| h.path().to_path_buf()).collect())
            .unwrap_or_default()
    }
}

/// Save dialog; Windows appends the extension when missing and starts in the Videos library, macOS keeps the exact
/// name (no type filter, so NSSavePanel never appends `.txt`).
pub async fn pick_save_file(suggested_name: &str, format: SubtitleFormat) -> Option<PathBuf> {
    pick_save_file_in(suggested_name, format, None).await
}

/// [`pick_save_file`] with a start folder (Windows: the Videos known folder).
pub async fn pick_save_file_in(suggested_name: &str, format: SubtitleFormat, start_dir: Option<PathBuf>) -> Option<PathBuf> {
    let flavor = DialogFlavor::native();
    #[cfg(windows)]
    {
        // C# FileSavePicker: SuggestedFileName = name without extension, DefaultFileExtension = ".<raw>", one
        // FileTypeChoices entry "<RAW>" → [".<raw>"], SuggestedStartLocation = VideosLibrary.
        let request = crate::win::file_dialog::DialogRequest {
            filter: Some((filter_name(&FileFilter::Subtitle(format), flavor), vec![format.raw().to_owned()])),
            multiple: false,
            file_name: Some(suggested_save_name(suggested_name, flavor)),
            default_extension: Some(format.raw().to_owned()),
            start_dir,
            owner: parent_hwnd(),
        };
        let path = crate::win::file_dialog::save(request).await?;
        Some(finish_save_path(path, format, flavor))
    }
    #[cfg(not(windows))]
    {
        // macOS: no type filter, so NSSavePanel keeps the exact `<stem>[.<lang>].<fmt>` name (never appends `.txt`).
        let mut d = rfd::AsyncFileDialog::new().set_file_name(suggested_save_name(suggested_name, flavor));
        if let Some(dir) = start_dir.filter(|d| d.is_dir()) {
            d = d.set_directory(dir);
        }
        if let Some(p) = parent() {
            d = d.set_parent(&p);
        }
        let path = d.save_file().await?.path().to_path_buf();
        Some(finish_save_path(path, format, flavor))
    }
}

// ---- alerts -----------------------------------------------------------------------------------------------------

/// The app's alert presenter (UI thread).
pub type AlertPresenter = dyn Fn(Alert) -> LocalBoxFuture<'static, AlertResponse>;

/// Installs the app's presenter for [`sa_core::platform::PlatformShell::confirm`] (UI thread). Windows draws a
/// ContentDialog-style overlay in Slint; macOS may leave it unset (rfd sheet).
pub fn set_alert_presenter(presenter: Box<AlertPresenter>) {
    PRESENTER.with(|p| *p.borrow_mut() = Some(std::rc::Rc::from(presenter)));
}

pub fn clear_alert_presenter() {
    PRESENTER.with(|p| *p.borrow_mut() = None);
}

/// The installed presenter, if any (UI thread).
pub fn presenter() -> Option<std::rc::Rc<AlertPresenter>> {
    PRESENTER.with(|p| p.borrow().clone())
}

/// Maps the button choice of an rfd dialog back to the index in `alert.buttons`.
pub fn response_index(alert: &Alert, result: &rfd::MessageDialogResult) -> AlertResponse {
    let by_title = |t: &str| alert.buttons.iter().position(|b| b.title == t);
    let by_role = |r: AlertButtonRole| alert.buttons.iter().position(|b| b.role == r);
    match result {
        rfd::MessageDialogResult::Custom(t) => by_title(t),
        rfd::MessageDialogResult::Ok | rfd::MessageDialogResult::Yes => (!alert.buttons.is_empty()).then_some(0),
        rfd::MessageDialogResult::No => (alert.buttons.len() > 1).then_some(1),
        rfd::MessageDialogResult::Cancel => by_role(AlertButtonRole::Cancel),
    }
}

/// The rfd buttons for an alert (default first; at most three buttons are shown).
pub fn message_buttons(alert: &Alert) -> rfd::MessageButtons {
    let t: Vec<String> = alert.buttons.iter().map(|b| b.title.clone()).collect();
    match t.len() {
        0 => rfd::MessageButtons::Ok,
        1 => rfd::MessageButtons::OkCustom(t[0].clone()),
        2 => rfd::MessageButtons::OkCancelCustom(t[0].clone(), t[1].clone()),
        _ => rfd::MessageButtons::YesNoCancelCustom(t[0].clone(), t[1].clone(), t[2].clone()),
    }
}

/// `PlatformShell::confirm`: the installed presenter, else an rfd message dialog.
pub fn confirm(alert: Alert) -> LocalBoxFuture<'static, AlertResponse> {
    if let Some(p) = presenter() {
        return p(alert);
    }
    Box::pin(async move {
        let destructive = alert.buttons.iter().any(|b| b.role == AlertButtonRole::Destructive);
        let mut d = rfd::AsyncMessageDialog::new()
            .set_title(alert.title.clone())
            .set_description(alert.message.clone())
            .set_level(if destructive { rfd::MessageLevel::Warning } else { rfd::MessageLevel::Info })
            .set_buttons(message_buttons(&alert));
        if let Some(p) = parent() {
            d = d.set_parent(&p);
        }
        let result = d.show().await;
        response_index(&alert, &result)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sa_core::platform::AlertButton;

    #[test]
    fn filters_follow_each_platform() {
        assert_eq!(filter_extensions_for(&FileFilter::Videos, DialogFlavor::Windows), sa_core::media_types::MEDIA_EXTENSIONS);
        assert_eq!(filter_extensions_for(&FileFilter::Videos, DialogFlavor::Mac), sa_core::media_types::MEDIA_EXTENSIONS);
        assert_eq!(filter_extensions_for(&FileFilter::Videos, DialogFlavor::Windows)[..4], ["mp4", "mov", "m4v", "mkv"]);
        assert!(filter_extensions_for(&FileFilter::Videos, DialogFlavor::Windows).contains(&"vob"));
        let label = filter_name(&FileFilter::Videos, DialogFlavor::Windows);
        assert!(!label.is_empty() && !label.contains("*."), "{label}");
        assert_eq!(filter_extensions_for(&FileFilter::Subtitles, DialogFlavor::Windows), ["srt", "vtt", "ass", "ssa", "json"]);
        assert_eq!(filter_extensions_for(&FileFilter::Subtitles, DialogFlavor::Mac), ["srt", "vtt", "ass", "ssa", "json", "txt"]);
        assert_eq!(filter_extensions_for(&FileFilter::Subtitle(SubtitleFormat::Vtt), DialogFlavor::Mac), ["vtt"]);
        assert!(filter_extensions_for(&FileFilter::Any, DialogFlavor::Mac).is_empty());
        assert_eq!(filter_name(&FileFilter::Subtitle(SubtitleFormat::Srt), DialogFlavor::Windows), "SRT");
        assert_eq!(filter_name(&FileFilter::Subtitles, DialogFlavor::Windows), "*.srt; *.vtt; *.ass; *.ssa; *.json");
    }

    #[test]
    fn media_extensions_match_the_core_list_as_a_set() {
        let mut a: Vec<_> = MEDIA_EXTENSIONS.to_vec();
        let mut b: Vec<_> = sa_core::subtitle_import::COMPANION_VIDEO_EXTENSIONS.to_vec();
        a.sort();
        b.sort();
        assert_eq!(a, b);
        assert_eq!(MEDIA_EXTENSIONS.to_vec(), sa_core::subtitle_import::MAC_MEDIA_EXTENSIONS.to_vec());
        assert_eq!(MEDIA_EXTENSIONS, sa_core::media_types::MEDIA_EXTENSIONS);
    }

    #[test]
    fn windows_save_appends_the_extension_only_when_missing() {
        let w = DialogFlavor::Windows;
        assert_eq!(finish_save_path(PathBuf::from(r"C:\v\film"), SubtitleFormat::Srt, w), PathBuf::from(r"C:\v\film.srt"));
        assert_eq!(finish_save_path(PathBuf::from(r"C:\v\film.tr"), SubtitleFormat::Srt, w), PathBuf::from(r"C:\v\film.tr"));
        assert_eq!(finish_save_path(PathBuf::from(r"C:\v\film."), SubtitleFormat::Vtt, w), PathBuf::from(r"C:\v\film..vtt"));
        assert_eq!(finish_save_path(PathBuf::from("/v/film"), SubtitleFormat::Json, DialogFlavor::Mac), PathBuf::from("/v/film"));
    }

    #[test]
    fn suggested_names() {
        assert_eq!(suggested_save_name("film.tr.srt", DialogFlavor::Windows), "film.tr");
        assert_eq!(suggested_save_name("film", DialogFlavor::Windows), "film");
        assert_eq!(suggested_save_name(".srt", DialogFlavor::Windows), "");
        assert_eq!(suggested_save_name("C:", DialogFlavor::Windows), "");
        assert_eq!(suggested_save_name("C:film.tr.srt", DialogFlavor::Windows), "film.tr");
        assert_eq!(suggested_save_name(r"C:\v\film.srt", DialogFlavor::Windows), "film");
        assert_eq!(suggested_save_name("film.tr.srt", DialogFlavor::Mac), "film.tr.srt");
    }

    #[test]
    fn rfd_results_map_back_to_button_indexes() {
        let alert = Alert {
            title: "t".into(),
            message: "m".into(),
            buttons: vec![
                AlertButton { title: "Sil".into(), role: AlertButtonRole::Destructive },
                AlertButton { title: "Vazgeç".into(), role: AlertButtonRole::Cancel },
            ],
        };
        assert_eq!(response_index(&alert, &rfd::MessageDialogResult::Custom("Vazgeç".into())), Some(1));
        assert_eq!(response_index(&alert, &rfd::MessageDialogResult::Custom("Sil".into())), Some(0));
        assert_eq!(response_index(&alert, &rfd::MessageDialogResult::Ok), Some(0));
        assert_eq!(response_index(&alert, &rfd::MessageDialogResult::Cancel), Some(1));
        let one = Alert { buttons: vec![alert.buttons[0].clone()], ..alert.clone() };
        assert_eq!(response_index(&one, &rfd::MessageDialogResult::Cancel), None);
        assert!(matches!(message_buttons(&alert), rfd::MessageButtons::OkCancelCustom(a, b) if a == "Sil" && b == "Vazgeç"));
    }
}
