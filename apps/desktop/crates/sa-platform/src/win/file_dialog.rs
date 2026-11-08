//! The Windows open/save dialogs (win-ui-main §18) on `IFileOpenDialog` / `IFileSaveDialog` directly, set up the way
//! the WinUI app's pickers (`Microsoft.Windows.Storage.Pickers`) set them up:
//!
//! * `SuggestedStartLocation = VideosLibrary` is `IFileDialog::SetDefaultFolder` with the Videos folder
//!   (`FOLDERID_Videos`, win-ui-main §18): like the WinUI pickers, the dialog starts where the user picked last time
//!   (the shell remembers it per app) and only falls back to Videos on first use. rfd only offers `SetFolder`, which
//!   would force the Videos folder on every open.
//! * open: the type filter of the extensions, single or multiple selection, file system items only;
//! * save: the suggested name without extension (`SetFileName`), the default extension (`SetDefaultExtension`) and
//!   one type choice `"SRT" → *.srt`; the caller appends the extension when the chosen name has none.
//!
//! Each dialog runs modally (owned by the dialog parent window) on its own STA thread, like rfd's async dialogs, so
//! the UI thread's event loop keeps running; the future resolves on the UI thread's executor.

use std::path::PathBuf;
use std::sync::Arc;
use std::task::{Poll, Waker};

use parking_lot::Mutex;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    CoUninitialize,
};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{
    FOLDERID_Videos, FOLDERID_VideosLibrary, FOS_ALLOWMULTISELECT, FOS_FORCEFILESYSTEM, FileOpenDialog, FileSaveDialog, IFileDialog,
    IFileOpenDialog, IFileSaveDialog, IShellItem, KF_FLAG_DEFAULT, SHCreateItemFromParsingName, SHGetKnownFolderItem, SIGDN_FILESYSPATH,
};
use windows::core::Interface;

use super::wide::Wide;

/// What one dialog shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DialogRequest {
    /// `(display name, extensions without dots)`; `None` = no type filter.
    pub filter: Option<(String, Vec<String>)>,
    pub multiple: bool,
    /// Save dialogs: the suggested file name and the default extension (without dot).
    pub file_name: Option<String>,
    pub default_extension: Option<String>,
    /// The default folder (`None` = the Videos known folder).
    pub start_dir: Option<PathBuf>,
    /// The owner window (`HWND` as an integer, 0 = none).
    pub owner: isize,
}

/// `*.mp4;*.mov` for `IFileDialog::SetFileTypes`.
pub fn filter_spec(extensions: &[String]) -> String {
    extensions.iter().map(|e| format!("*.{e}")).collect::<Vec<_>>().join(";")
}

/// The Videos folder item (`FOLDERID_Videos`), else the Videos library.
unsafe fn videos_start_item() -> Option<IShellItem> {
    unsafe {
        SHGetKnownFolderItem::<IShellItem>(&FOLDERID_Videos, KF_FLAG_DEFAULT, None)
            .or_else(|_| SHGetKnownFolderItem::<IShellItem>(&FOLDERID_VideosLibrary, KF_FLAG_DEFAULT, None))
            .ok()
    }
}

/// The item of `start_dir` when it is an existing folder, else [`videos_start_item`].
unsafe fn start_item(start_dir: Option<&std::path::Path>) -> Option<IShellItem> {
    unsafe {
        if let Some(dir) = start_dir.filter(|d| d.is_dir())
            && let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(Wide::path(dir).pcwstr(), None)
        {
            return Some(item);
        }
        videos_start_item()
    }
}

unsafe fn item_path(item: &IShellItem) -> Option<PathBuf> {
    unsafe {
        let p = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let s = super::wide::from_pwstr(p);
        CoTaskMemFree(Some(p.0 as *const _));
        s.filter(|s| !s.is_empty()).map(PathBuf::from)
    }
}

/// Common setup (options, type filter, default folder, name, default extension), then the modal `Show` on the owner
/// window; an error (including the user's cancel, `ERROR_CANCELLED`) means no result.
unsafe fn configure(dialog: &IFileDialog, request: &DialogRequest) -> windows::core::Result<()> {
    unsafe {
        let mut options = dialog.GetOptions()? | FOS_FORCEFILESYSTEM;
        if request.multiple {
            options |= FOS_ALLOWMULTISELECT;
        }
        dialog.SetOptions(options)?;
        // Kept alive until Show returns (the dialog copies the strings).
        let filter = request.filter.as_ref().map(|(name, exts)| (Wide::new(name), Wide::new(&filter_spec(exts))));
        if let Some((name, spec)) = &filter {
            dialog.SetFileTypes(&[COMDLG_FILTERSPEC { pszName: name.pcwstr(), pszSpec: spec.pcwstr() }])?;
            dialog.SetFileTypeIndex(1)?;
        }
        if let Some(folder) = start_item(request.start_dir.as_deref()) {
            let _ = dialog.SetDefaultFolder(&folder);
        }
        if let Some(name) = &request.file_name {
            dialog.SetFileName(Wide::new(name).pcwstr())?;
        }
        if let Some(ext) = &request.default_extension {
            dialog.SetDefaultExtension(Wide::new(ext).pcwstr())?;
        }
        let owner = (request.owner != 0).then_some(HWND(request.owner as *mut _));
        dialog.Show(owner)
    }
}

/// Runs an open dialog on this thread (COM initialized by the caller). Empty on cancel or error.
unsafe fn run_open(request: &DialogRequest) -> Vec<PathBuf> {
    unsafe {
        let Ok(dialog) = CoCreateInstance::<_, IFileOpenDialog>(&FileOpenDialog, None, CLSCTX_INPROC_SERVER) else {
            return Vec::new();
        };
        if configure(&dialog.cast::<IFileDialog>().expect("IFileOpenDialog is an IFileDialog"), request).is_err() {
            return Vec::new();
        }
        let Ok(items) = dialog.GetResults() else { return Vec::new() };
        let count = items.GetCount().unwrap_or(0);
        (0..count).filter_map(|i| items.GetItemAt(i).ok()).filter_map(|item| item_path(&item)).collect()
    }
}

unsafe fn run_save(request: &DialogRequest) -> Option<PathBuf> {
    unsafe {
        let dialog = CoCreateInstance::<_, IFileSaveDialog>(&FileSaveDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        configure(&dialog.cast::<IFileDialog>().ok()?, request).ok()?;
        item_path(&dialog.GetResult().ok()?)
    }
}

/// A value produced on another thread, awaited on the UI thread.
struct Shared<T> {
    value: Option<T>,
    done: bool,
    waker: Option<Waker>,
}

fn on_dialog_thread<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> impl std::future::Future<Output = Option<T>> {
    let shared = Arc::new(Mutex::new(Shared { value: None, done: false, waker: None }));
    let s = shared.clone();
    let spawned = std::thread::Builder::new().name("sa-file-dialog".into()).spawn(move || {
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
        let value = f();
        if hr.is_ok() {
            unsafe { CoUninitialize() };
        }
        let mut st = s.lock();
        st.value = Some(value);
        st.done = true;
        if let Some(w) = st.waker.take() {
            w.wake();
        }
    });
    if spawned.is_err() {
        shared.lock().done = true;
    }
    std::future::poll_fn(move |cx| {
        let mut st = shared.lock();
        if st.done {
            Poll::Ready(st.value.take())
        } else {
            st.waker = Some(cx.waker().clone());
            Poll::Pending
        }
    })
}

/// Open dialog (one or several files); empty on cancel.
pub async fn open(request: DialogRequest) -> Vec<PathBuf> {
    on_dialog_thread(move || unsafe { run_open(&request) }).await.unwrap_or_default()
}

/// Save dialog; `None` on cancel.
pub async fn save(request: DialogRequest) -> Option<PathBuf> {
    on_dialog_thread(move || unsafe { run_save(&request) }).await.flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_specs_join_wildcards() {
        assert_eq!(filter_spec(&["mp4".into(), "mov".into()]), "*.mp4;*.mov");
        assert_eq!(filter_spec(&["srt".into()]), "*.srt");
    }

    #[test]
    fn the_start_folder_resolves_to_a_file_system_folder() {
        unsafe {
            let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
            let item = videos_start_item().expect("Videos folder or library");
            assert_eq!(item_path(&item), super::super::folders::videos_folder());
            let dir = tempfile::tempdir().unwrap();
            assert_eq!(start_item(Some(dir.path())).and_then(|i| item_path(&i)), Some(dir.path().to_path_buf()));
            let missing = dir.path().join("missing");
            assert_eq!(start_item(Some(&missing)).and_then(|i| item_path(&i)), super::super::folders::videos_folder());
            drop(item);
            if hr.is_ok() {
                CoUninitialize();
            }
        }
    }

    #[test]
    fn a_dialog_thread_hands_its_value_back() {
        let v = futures_lite_block_on(on_dialog_thread(|| 42));
        assert_eq!(v, Some(42));
    }

    /// A tiny executor for the test (the app awaits on the Slint event loop).
    fn futures_lite_block_on<F: std::future::Future>(f: F) -> F::Output {
        struct ThreadWaker(std::thread::Thread);
        impl std::task::Wake for ThreadWaker {
            fn wake(self: Arc<Self>) {
                self.0.unpark();
            }
        }
        let waker = Waker::from(Arc::new(ThreadWaker(std::thread::current())));
        let mut cx = std::task::Context::from_waker(&waker);
        let mut f = std::pin::pin!(f);
        loop {
            if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
                return v;
            }
            std::thread::park_timeout(std::time::Duration::from_millis(50));
        }
    }
}

#[cfg(test)]
mod smoke {
    use super::*;
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetClassNameW, GetWindowThreadProcessId, PostMessageW, WM_CLOSE};
    use windows::core::BOOL;

    unsafe extern "system" fn find_dialog(hwnd: HWND, out: LPARAM) -> BOOL {
        unsafe {
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            let mut class = [0u16; 64];
            let n = GetClassNameW(hwnd, &mut class);
            if pid == std::process::id() && String::from_utf16_lossy(&class[..n as usize]) == "#32770" {
                *(out.0 as *mut isize) = hwnd.0 as isize;
                return false.into();
            }
            true.into()
        }
    }

    /// Opens a real dialog on its thread and closes it from here (`--ignored`: it shows a window).
    #[test]
    #[ignore]
    fn a_real_open_dialog_shows_and_cancels() {
        let request = DialogRequest {
            filter: Some(("*.mp4; *.mkv".into(), vec!["mp4".into(), "mkv".into()])),
            multiple: true,
            ..Default::default()
        };
        let closer = std::thread::spawn(|| {
            for _ in 0..100 {
                std::thread::sleep(std::time::Duration::from_millis(100));
                let mut found: isize = 0;
                unsafe {
                    let _ = EnumWindows(Some(find_dialog), LPARAM(&mut found as *mut isize as isize));
                    if found != 0 {
                        std::thread::sleep(std::time::Duration::from_millis(500));
                        let _ = PostMessageW(Some(HWND(found as *mut _)), WM_CLOSE, WPARAM(0), LPARAM(0));
                        return true;
                    }
                }
            }
            false
        });
        let picked = tests_block_on(open(request));
        assert!(closer.join().unwrap(), "the dialog window appeared");
        assert!(picked.is_empty());
    }

    fn tests_block_on<F: std::future::Future>(f: F) -> F::Output {
        struct ThreadWaker(std::thread::Thread);
        impl std::task::Wake for ThreadWaker {
            fn wake(self: Arc<Self>) {
                self.0.unpark();
            }
        }
        let waker = Waker::from(Arc::new(ThreadWaker(std::thread::current())));
        let mut cx = std::task::Context::from_waker(&waker);
        let mut f = std::pin::pin!(f);
        loop {
            if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
                return v;
            }
            std::thread::park_timeout(std::time::Duration::from_millis(50));
        }
    }
}
