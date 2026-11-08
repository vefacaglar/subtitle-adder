//! Known folders, user/machine names, preferred UI languages, quick locations and drive readiness (Windows).

use std::path::{Path, PathBuf};

use sa_core::l10n::UiLanguage;
use sa_core::platform::{QuickLocation, QuickLocationKind};
use windows::Win32::Foundation::MAX_PATH;
use windows::Win32::Globalization::{GetUserPreferredUILanguages, MUI_LANGUAGE_NAME, SetProcessPreferredUILanguages};
use windows::Win32::Security::Authentication::Identity::{GetUserNameExW, NameSamCompatible};
use windows::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_DIRECTORY, GetDriveTypeW, GetFileAttributesW, GetFullPathNameW, GetLogicalDrives, GetVolumeInformationW,
    INVALID_FILE_ATTRIBUTES,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Diagnostics::Debug::{SEM_FAILCRITICALERRORS, SEM_NOOPENFILEERRORBOX, SetThreadErrorMode, THREAD_ERROR_MODE};
use windows::Win32::System::WindowsProgramming::GetComputerNameW;
use windows::Win32::UI::Shell::{FOLDERID_Desktop, FOLDERID_LocalAppData, FOLDERID_Profile, FOLDERID_Videos, KF_FLAG_DONT_VERIFY, SHGetKnownFolderPath};
use windows::core::{GUID, PWSTR};

use super::wide::{Wide, from_buf};

fn known_folder(id: &GUID) -> Option<PathBuf> {
    // .NET `Environment.GetFolderPath` (SpecialFolderOption.None) passes KF_FLAG_DONT_VERIFY.
    unsafe {
        let p = SHGetKnownFolderPath(id, KF_FLAG_DONT_VERIFY, None).ok()?;
        let s = super::wide::from_pwstr(p);
        CoTaskMemFree(Some(p.0 as *const _));
        s.filter(|s| !s.trim().is_empty()).map(PathBuf::from)
    }
}

pub fn local_app_data() -> PathBuf {
    known_folder(&FOLDERID_LocalAppData)
        .or_else(|| std::env::var_os("LOCALAPPDATA").map(PathBuf::from))
        .unwrap_or_else(std::env::temp_dir)
}

/// `SpecialFolder.MyVideos`.
pub fn videos_folder() -> Option<PathBuf> {
    known_folder(&FOLDERID_Videos)
}

/// `SpecialFolder.DesktopDirectory`.
pub fn desktop_folder() -> Option<PathBuf> {
    known_folder(&FOLDERID_Desktop)
}

/// `SpecialFolder.UserProfile`.
pub fn user_profile() -> Option<PathBuf> {
    known_folder(&FOLDERID_Profile).or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
}

/// .NET `Environment.UserName`: `GetUserNameExW(NameSamCompatible)` (`DOMAIN\user`) after the last `\`; falls back
/// to `%USERNAME%`.
pub fn user_name() -> String {
    let mut buf = vec![0u16; 256];
    let mut len = buf.len() as u32;
    let ok = unsafe { GetUserNameExW(NameSamCompatible, Some(PWSTR(buf.as_mut_ptr())), &mut len) };
    if !ok && len as usize > buf.len() {
        buf = vec![0u16; len as usize + 1];
        len = buf.len() as u32;
        if !unsafe { GetUserNameExW(NameSamCompatible, Some(PWSTR(buf.as_mut_ptr())), &mut len) } {
            return std::env::var("USERNAME").unwrap_or_default();
        }
    } else if !ok {
        return std::env::var("USERNAME").unwrap_or_default();
    }
    let full = from_buf(&buf);
    match full.rfind('\\') {
        Some(i) => full[i + 1..].to_owned(),
        None => full,
    }
}

/// .NET `Environment.MachineName` (`GetComputerNameW`, NetBIOS name).
pub fn machine_name() -> String {
    let mut buf = vec![0u16; 64];
    let mut len = buf.len() as u32;
    if unsafe { GetComputerNameW(Some(PWSTR(buf.as_mut_ptr())), &mut len) }.is_ok() {
        return from_buf(&buf[..len as usize]);
    }
    std::env::var("COMPUTERNAME").unwrap_or_default()
}

/// The user's preferred UI languages as BCP-47 names (`GetUserPreferredUILanguages(MUI_LANGUAGE_NAME)`).
pub fn preferred_ui_languages() -> Vec<String> {
    let mut count = 0u32;
    let mut len = 0u32;
    unsafe {
        if GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut count, None, &mut len).is_err() || len == 0 {
            return Vec::new();
        }
        let mut buf = vec![0u16; len as usize];
        if GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut count, Some(PWSTR(buf.as_mut_ptr())), &mut len).is_err() {
            return Vec::new();
        }
        parse_multi_sz(&buf)
    }
}

/// A double-NUL-terminated list.
pub fn parse_multi_sz(buf: &[u16]) -> Vec<String> {
    buf.split(|&c| c == 0).filter(|s| !s.is_empty()).map(String::from_utf16_lossy).collect()
}

/// The process language tag for system UI (win-ui-main §1.1.7): `en-US` for English, `tr-TR` otherwise.
pub fn process_language_tag(language: UiLanguage) -> &'static str {
    match language {
        UiLanguage::En => "en-US",
        UiLanguage::Tr => "tr-TR",
    }
}

/// `SetProcessPreferredUILanguages` (common dialogs created afterwards follow it); failures are ignored.
pub fn set_process_ui_language(language: UiLanguage) {
    let tag = process_language_tag(language);
    let list: Vec<u16> = tag.encode_utf16().chain([0, 0]).collect();
    let mut n = 0u32;
    unsafe {
        let _ = SetProcessPreferredUILanguages(MUI_LANGUAGE_NAME, windows::core::PCWSTR(list.as_ptr()), Some(&mut n));
    }
}

// ---- paths ------------------------------------------------------------------------------------------------------

/// .NET `Path.GetPathRoot` for full Windows paths: `C:\` (or `C:` for a bare drive), `\\server\share` for UNC,
/// `\\?\C:\` for extended paths; `""` for rootless paths.
pub fn path_root(path: &str) -> String {
    let p = path.replace('/', "\\");
    let b = p.as_bytes();
    if let Some(rest) = p.strip_prefix(r"\\?\").or_else(|| p.strip_prefix(r"\\.\")) {
        if let Some(unc) = rest.strip_prefix("UNC\\") {
            let root = unc_root(unc);
            return format!("{}UNC\\{root}", &p[..4]);
        }
        return format!("{}{}", &p[..4], drive_root(rest));
    }
    if b.len() >= 2 && b[0] == b'\\' && b[1] == b'\\' {
        return format!(r"\\{}", unc_root(&p[2..]));
    }
    if b.len() >= 1 && b[0] == b'\\' {
        return "\\".into();
    }
    drive_root(&p)
}

fn drive_root(p: &str) -> String {
    let b = p.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        if b.len() >= 3 && b[2] == b'\\' { p[..3].to_owned() } else { p[..2].to_owned() }
    } else {
        String::new()
    }
}

fn unc_root(rest: &str) -> String {
    let mut parts = rest.splitn(3, '\\');
    let server = parts.next().unwrap_or("");
    match parts.next() {
        Some(share) => format!("{server}\\{share}"),
        None => server.to_owned(),
    }
}

/// `Path.GetFullPath` through `GetFullPathNameW`.
pub fn full_path(path: &str) -> Option<String> {
    let w = Wide::new(path);
    unsafe {
        let n = GetFullPathNameW(w.pcwstr(), None, None);
        if n == 0 {
            return None;
        }
        let mut buf = vec![0u16; n as usize + 1];
        let m = GetFullPathNameW(w.pcwstr(), Some(&mut buf), None);
        if m == 0 || m as usize >= buf.len() {
            return None;
        }
        Some(from_buf(&buf))
    }
}

/// C# `MediaLibraryIndex.NormalizeDirectoryPath`: full path; a root stays as is (`C:\`), anything else loses
/// trailing separators.
pub fn normalize_directory_path(path: &str) -> String {
    if path.trim().is_empty() {
        return String::new();
    }
    let Some(full) = full_path(path.trim()) else { return path.trim().to_owned() };
    let root = path_root(&full);
    if !root.is_empty() && full.eq_ignore_ascii_case(&root) {
        return full;
    }
    full.trim_end_matches(['\\', '/']).to_owned()
}

/// Runs `f` with the critical-error dialogs off (an empty card reader never shows "insert a disk").
pub fn without_error_dialogs<T>(f: impl FnOnce() -> T) -> T {
    let mut old = THREAD_ERROR_MODE(0);
    let set = unsafe { SetThreadErrorMode(SEM_FAILCRITICALERRORS | SEM_NOOPENFILEERRORBOX, Some(&mut old)) }.is_ok();
    let r = f();
    if set {
        unsafe {
            let _ = SetThreadErrorMode(old, None);
        }
    }
    r
}

/// The directory exists (attributes only, no file is opened).
pub fn is_directory(path: &str) -> bool {
    let w = Wide::new(path);
    let attrs = unsafe { GetFileAttributesW(w.pcwstr()) };
    attrs != INVALID_FILE_ATTRIBUTES && attrs & FILE_ATTRIBUTE_DIRECTORY.0 != 0
}

/// Drive root ready / UNC share reachable (win-media §10 "drive-root readiness"): only the root is touched.
pub fn storage_available(path: &Path) -> bool {
    let s = path.to_string_lossy();
    if s.trim().is_empty() {
        return false;
    }
    let root = path_root(&s);
    if root.is_empty() || root == "\\" {
        return true;
    }
    without_error_dialogs(|| is_directory(&root))
}

// ---- quick locations --------------------------------------------------------------------------------------------

const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_FIXED: u32 = 3;
const DRIVE_REMOTE: u32 = 4;

/// One logical drive as `DriveInfo` sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriveEntry {
    /// `C:\`
    pub root: String,
    pub drive_type: u32,
    pub ready: bool,
    pub label: String,
}

pub fn logical_drives() -> Vec<DriveEntry> {
    let mask = unsafe { GetLogicalDrives() };
    let mut out = Vec::new();
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let root = format!("{}:\\", (b'A' + i as u8) as char);
        let w = Wide::new(&root);
        let drive_type = unsafe { GetDriveTypeW(w.pcwstr()) };
        let (ready, label) = without_error_dialogs(|| {
            let ready = is_directory(&root);
            let mut name = [0u16; MAX_PATH as usize + 1];
            let label = if ready && unsafe { GetVolumeInformationW(w.pcwstr(), Some(&mut name), None, None, None, None) }.is_ok() {
                from_buf(&name)
            } else {
                String::new()
            };
            (ready, label)
        });
        out.push(DriveEntry { root, drive_type, ready, label });
    }
    out
}

/// The drive row title: `"{label} (C:)"`, or `C:` without a label.
pub fn drive_title(root: &str, label: &str) -> String {
    let bare = root.trim_end_matches('\\');
    if label.trim().is_empty() { bare.to_owned() } else { format!("{label} ({bare})") }
}

/// C# `FileExplorerViewModel.GetRootLocations` from injected sources (testable).
pub fn build_quick_locations(
    videos: Option<PathBuf>,
    user_profile: Option<PathBuf>,
    desktop: Option<PathBuf>,
    drives: &[DriveEntry],
    dir_exists: &dyn Fn(&Path) -> bool,
    normalize: &dyn Fn(&str) -> String,
) -> Vec<QuickLocation> {
    let mut list = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    let mut add = |path: Option<PathBuf>, title: String, kind: QuickLocationKind, list: &mut Vec<QuickLocation>| {
        let Some(path) = path else { return };
        if path.as_os_str().is_empty() || !dir_exists(&path) {
            return;
        }
        let normalized = normalize(&path.to_string_lossy());
        let key = normalized.to_lowercase();
        if !seen.contains(&key) {
            seen.push(key);
            list.push(QuickLocation { title, path: PathBuf::from(normalized), kind });
        }
    };
    add(videos, sa_core::l("Videolar"), QuickLocationKind::Videos, &mut list);
    add(user_profile.map(|p| p.join("Downloads")), sa_core::l("İndirilenler"), QuickLocationKind::Downloads, &mut list);
    add(desktop, sa_core::l("Masaüstü"), QuickLocationKind::Desktop, &mut list);
    for d in drives {
        if !d.ready || !matches!(d.drive_type, DRIVE_FIXED | DRIVE_REMOVABLE | DRIVE_REMOTE) {
            continue;
        }
        let root = normalize(&d.root);
        let key = root.to_lowercase();
        if !seen.contains(&key) {
            seen.push(key);
            list.push(QuickLocation { title: drive_title(&root, &d.label), path: PathBuf::from(root), kind: QuickLocationKind::Drive });
        }
    }
    list
}

/// Videolar, İndirilenler (`%USERPROFILE%\Downloads`), Masaüstü, then ready fixed/removable/network drives.
pub fn quick_locations() -> Vec<QuickLocation> {
    build_quick_locations(
        videos_folder(),
        user_profile(),
        desktop_folder(),
        &logical_drives(),
        &|p| without_error_dialogs(|| is_directory(&p.to_string_lossy())),
        &normalize_directory_path,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_roots_match_dotnet() {
        assert_eq!(path_root(r"C:\Users\x\film.mp4"), r"C:\");
        assert_eq!(path_root("C:"), "C:");
        assert_eq!(path_root(r"\\nas\media\film.mp4"), r"\\nas\media");
        assert_eq!(path_root(r"\\nas"), r"\\nas");
        assert_eq!(path_root(r"\\?\C:\long\path"), r"\\?\C:\");
        assert_eq!(path_root(r"\\?\UNC\nas\share\x"), r"\\?\UNC\nas\share");
        assert_eq!(path_root("relative\\x"), "");
        assert_eq!(path_root("D:/x/y"), r"D:\");
    }

    #[test]
    fn directory_paths_normalize_like_the_media_index() {
        assert_eq!(normalize_directory_path(r"C:\Windows\"), r"C:\Windows");
        assert_eq!(normalize_directory_path(r"C:\"), r"C:\");
        assert_eq!(normalize_directory_path("  C:/Windows/System32//  "), r"C:\Windows\System32");
        assert_eq!(normalize_directory_path("   "), "");
    }

    #[test]
    fn drive_titles() {
        assert_eq!(drive_title(r"C:\", "Windows"), "Windows (C:)");
        assert_eq!(drive_title(r"D:\", ""), "D:");
    }

    #[test]
    fn quick_locations_follow_the_csharp_order_and_rules() {
        sa_core::l10n::LanguageState::shared().set_language(UiLanguage::Tr);
        let drives = vec![
            DriveEntry { root: r"C:\".into(), drive_type: DRIVE_FIXED, ready: true, label: "Windows".into() },
            DriveEntry { root: r"D:\".into(), drive_type: 5, ready: true, label: "DVD".into() },
            DriveEntry { root: r"E:\".into(), drive_type: DRIVE_REMOVABLE, ready: false, label: String::new() },
            DriveEntry { root: r"Z:\".into(), drive_type: DRIVE_REMOTE, ready: true, label: String::new() },
        ];
        let q = build_quick_locations(
            Some(PathBuf::from(r"C:\Users\u\Videos")),
            Some(PathBuf::from(r"C:\Users\u")),
            Some(PathBuf::from(r"C:\Users\u\Videos\")),
            &drives,
            &|p| !p.to_string_lossy().contains("Missing"),
            &|s| s.trim_end_matches('\\').to_owned() + if s.len() == 3 { "\\" } else { "" },
        );
        let rows: Vec<_> = q.iter().map(|q| (q.title.as_str(), q.path.to_string_lossy().into_owned(), q.kind)).collect();
        assert_eq!(
            rows,
            vec![
                ("Videolar", r"C:\Users\u\Videos".to_owned(), QuickLocationKind::Videos),
                ("İndirilenler", r"C:\Users\u\Downloads".to_owned(), QuickLocationKind::Downloads),
                ("Windows (C:)", r"C:\".to_owned(), QuickLocationKind::Drive),
                ("Z:", r"Z:\".to_owned(), QuickLocationKind::Drive),
            ]
        );
    }

    #[test]
    fn storage_checks_only_roots() {
        assert!(!storage_available(Path::new("")));
        assert!(storage_available(Path::new("relative.mp4")));
        assert!(storage_available(Path::new(r"C:\definitely\missing\file.mp4")));
    }

    #[test]
    fn real_folders_and_names_resolve() {
        assert!(local_app_data().is_absolute());
        assert!(!user_name().is_empty());
        assert!(!user_name().contains('\\'));
        assert!(!machine_name().is_empty());
        assert_eq!(process_language_tag(UiLanguage::En), "en-US");
        assert_eq!(process_language_tag(UiLanguage::Tr), "tr-TR");
        assert_eq!(parse_multi_sz(&[116, 114, 0, 101, 110, 0, 0]), vec!["tr".to_owned(), "en".to_owned()]);
        let _ = preferred_ui_languages();
        assert!(quick_locations().iter().any(|q| q.kind == QuickLocationKind::Drive));
    }
}
