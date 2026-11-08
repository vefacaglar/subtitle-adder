//! HKCU self-registration (win-ui-main §3; C# `FileAssociationRegistrar`, registry layout byte-exact, no elevation).
//! Registered on every launch unless `fileAssociationsDisabled` is true, and only when the binary is
//! `SubtitleAdder.exe` ([`should_register_on_launch`]).

use std::path::{Path, PathBuf};

use sa_core::platform::FileAssociations;
use windows::Win32::Foundation::{ERROR_SUCCESS, WIN32_ERROR};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, REG_VALUE_TYPE, RegCloseKey, RegCreateKeyExW,
    RegDeleteTreeW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};
use windows::Win32::UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify};

use super::wide::Wide;

pub const MEDIA_PROG_ID: &str = "SubtitleAdder.Media";
pub const SUBTITLE_PROG_ID: &str = "SubtitleAdder.Subtitle";
pub const APP_REGISTERED_NAME: &str = "Subtitle Adder";
pub const CAPABILITIES_PATH: &str = r"Software\SubtitleAdder\Capabilities";
pub const APPLICATION_DESCRIPTION: &str = "Subtitle Adder \u{2014} Media Player & AI Subtitle Studio";
pub const EXE_NAME: &str = "SubtitleAdder.exe";

pub const MEDIA_EXTENSIONS: [&str; 22] = [
    ".mp4", ".mov", ".m4v", ".mkv", ".avi", ".webm", ".wmv", ".mpg", ".mpeg", ".ts", ".mts", ".m2ts", ".3gp", ".flv",
    ".mp3", ".wav", ".m4a", ".aac", ".flac", ".ogg", ".opus", ".wma",
];
pub const SUBTITLE_EXTENSIONS: [&str; 4] = [".srt", ".vtt", ".ass", ".ssa"];

/// Step 1.1.6: the running exe is `SubtitleAdder.exe` (case-insensitive) and `fileAssociationsDisabled` is not true.
pub fn should_register_on_launch(exe: &Path, file_associations_disabled: Option<bool>) -> bool {
    exe.file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(EXE_NAME)) && file_associations_disabled != Some(true)
}

/// True for instances that must never take over the user's file associations: a test/tool instance pointed at
/// another data folder (`SUBTITLE_ADDER_DATA_DIR`), or an exe running straight from a cargo target folder
/// (`<target>\{debug,release}\SubtitleAdder.exe`, recognizable by the sibling `.fingerprint` folder), whose path
/// changes with every target dir. Staged (`apps\desktop\dist`) and packaged builds still register, like the C# app.
pub fn is_scratch_instance(exe: &Path, data_dir_override: bool) -> bool {
    data_dir_override || exe.parent().is_some_and(|dir| dir.join(".fingerprint").is_dir())
}

/// `<app dir>\Assets\AppIcon.ico` (the window icon and the registered icon).
pub fn icon_path() -> Option<PathBuf> {
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    Some(dir.join("Assets").join("AppIcon.ico"))
}

pub struct FileAssociationRegistrar {
    /// Prefix of every key path under HKCU (`""` in production; a scratch subtree in tests).
    root: String,
    exe: Option<PathBuf>,
    icon: Option<String>,
    notify_shell: bool,
}

impl FileAssociationRegistrar {
    pub fn new() -> Self {
        Self { root: String::new(), exe: None, icon: None, notify_shell: true }
    }

    /// Writes below `HKCU\<root>` with a fixed exe and icon and no shell notification (tests).
    pub fn with_root(root: &str, exe: PathBuf, icon: Option<String>) -> Self {
        Self { root: format!("{}\\", root.trim_end_matches('\\')), exe: Some(exe), icon, notify_shell: false }
    }

    /// Registers on a worker thread when [`should_register_on_launch`]; every error is swallowed.
    pub fn register_on_launch_in_background(file_associations_disabled: Option<bool>) {
        let Ok(exe) = std::env::current_exe() else { return };
        let data_dir_override = std::env::var_os(sa_core::platform::DATA_DIR_ENV).is_some_and(|v| !v.is_empty());
        if should_register_on_launch(&exe, file_associations_disabled) && !is_scratch_instance(&exe, data_dir_override) {
            let _ = std::thread::Builder::new().name("sa-file-associations".into()).spawn(|| {
                let _ = FileAssociationRegistrar::new().register();
            });
        }
    }

    fn key(&self, path: &str) -> String {
        format!("{}{path}", self.root)
    }

    fn exe(&self) -> PathBuf {
        self.exe.clone().or_else(|| std::env::current_exe().ok()).unwrap_or_else(|| PathBuf::from(EXE_NAME))
    }

    fn icon(&self, exe: &Path) -> String {
        if let Some(i) = &self.icon {
            return i.clone();
        }
        match icon_path() {
            Some(p) if p.is_file() => p.to_string_lossy().into_owned(),
            _ => format!("{},0", exe.display()),
        }
    }

    /// `"\"{exe}\" \"%1\""`.
    pub fn command(exe: &Path) -> String {
        format!("\"{}\" \"%1\"", exe.display())
    }

    fn try_register(&self) -> Result<(), WIN32_ERROR> {
        let exe = self.exe();
        let icon = self.icon(&exe);
        let command = Self::command(&exe);
        self.register_prog_id(MEDIA_PROG_ID, "Subtitle Adder Media File", &icon, &command)?;
        self.register_prog_id(SUBTITLE_PROG_ID, "Subtitle Adder Subtitle File", &icon, &command)?;

        let app = self.key(r"Software\Classes\Applications\SubtitleAdder.exe");
        {
            let k = Key::create(&app)?;
            k.set(Some("FriendlyAppName"), APP_REGISTERED_NAME)?;
            Key::create(&format!(r"{app}\DefaultIcon"))?.set(None, &icon)?;
            Key::create(&format!(r"{app}\shell\open\command"))?.set(None, &command)?;
            let types = Key::create(&format!(r"{app}\SupportedTypes"))?;
            for ext in MEDIA_EXTENSIONS.iter().chain(SUBTITLE_EXTENSIONS.iter()) {
                types.set(Some(ext), "")?;
            }
        }
        {
            let cap = Key::create(&self.key(CAPABILITIES_PATH))?;
            cap.set(Some("ApplicationName"), APP_REGISTERED_NAME)?;
            cap.set(Some("ApplicationDescription"), APPLICATION_DESCRIPTION)?;
            cap.set(Some("ApplicationIcon"), &icon)?;
            let assoc = Key::create(&self.key(&format!(r"{CAPABILITIES_PATH}\FileAssociations")))?;
            for ext in MEDIA_EXTENSIONS {
                assoc.set(Some(ext), MEDIA_PROG_ID)?;
            }
            for ext in SUBTITLE_EXTENSIONS {
                assoc.set(Some(ext), SUBTITLE_PROG_ID)?;
            }
        }
        Key::create(&self.key(r"Software\RegisteredApplications"))?.set(Some(APP_REGISTERED_NAME), CAPABILITIES_PATH)?;
        for ext in MEDIA_EXTENSIONS {
            self.register_extension(ext, MEDIA_PROG_ID)?;
        }
        for ext in SUBTITLE_EXTENSIONS {
            self.register_extension(ext, SUBTITLE_PROG_ID)?;
        }
        self.notify();
        Ok(())
    }

    fn register_prog_id(&self, prog_id: &str, friendly: &str, icon: &str, command: &str) -> Result<(), WIN32_ERROR> {
        let base = self.key(&format!(r"Software\Classes\{prog_id}"));
        let k = Key::create(&base)?;
        k.set(None, friendly)?;
        k.set(Some("FriendlyTypeName"), friendly)?;
        Key::create(&format!(r"{base}\DefaultIcon"))?.set(None, icon)?;
        Key::create(&format!(r"{base}\shell\open"))?.set(Some("FriendlyAppName"), APP_REGISTERED_NAME)?;
        Key::create(&format!(r"{base}\shell\open\command"))?.set(None, command)?;
        Ok(())
    }

    fn register_extension(&self, ext: &str, prog_id: &str) -> Result<(), WIN32_ERROR> {
        let path = self.key(&format!(r"Software\Classes\{ext}"));
        let k = Key::create(&path)?;
        // Never steals an existing default.
        if k.get(None).is_none_or(|v| v.trim().is_empty()) {
            k.set(None, prog_id)?;
        }
        Key::create(&format!(r"{path}\OpenWithProgids"))?.set(Some(prog_id), "")?;
        Ok(())
    }

    fn unregister_extension(&self, ext: &str, prog_id: &str) {
        let path = self.key(&format!(r"Software\Classes\{ext}"));
        let Some(k) = Key::open(&path, true) else { return };
        if k.get(None).is_some_and(|v| v.eq_ignore_ascii_case(prog_id)) {
            k.delete_value(None);
        }
        if let Some(open_with) = Key::open(&format!(r"{path}\OpenWithProgids"), true) {
            open_with.delete_value(Some(prog_id));
        }
    }

    fn notify(&self) {
        if self.notify_shell {
            unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None) };
        }
    }
}

impl Default for FileAssociationRegistrar {
    fn default() -> Self {
        Self::new()
    }
}

impl FileAssociations for FileAssociationRegistrar {
    fn register(&self) -> bool {
        self.try_register().is_ok()
    }

    fn unregister(&self) -> bool {
        for ext in MEDIA_EXTENSIONS {
            self.unregister_extension(ext, MEDIA_PROG_ID);
        }
        for ext in SUBTITLE_EXTENSIONS {
            self.unregister_extension(ext, SUBTITLE_PROG_ID);
        }
        if let Some(reg_apps) = Key::open(&self.key(r"Software\RegisteredApplications"), true) {
            reg_apps.delete_value(Some(APP_REGISTERED_NAME));
        }
        let mut ok = true;
        for tree in [
            r"Software\SubtitleAdder".to_owned(),
            r"Software\Classes\Applications\SubtitleAdder.exe".to_owned(),
            format!(r"Software\Classes\{MEDIA_PROG_ID}"),
            format!(r"Software\Classes\{SUBTITLE_PROG_ID}"),
        ] {
            ok &= delete_tree(&self.key(&tree));
        }
        self.notify();
        ok
    }

    fn is_registered(&self) -> bool {
        Key::open(&self.key(&format!(r"Software\Classes\{MEDIA_PROG_ID}\shell\open\command")), false)
            .and_then(|k| k.get(None))
            .is_some_and(|v| !v.trim().is_empty())
    }

    fn open_default_apps_settings(&self) {
        super::process::open_default_apps_settings();
    }
}

// ---- minimal HKCU wrapper ---------------------------------------------------------------------------------------

pub(crate) struct Key(HKEY);

impl Key {
    pub(crate) fn create(path: &str) -> Result<Key, WIN32_ERROR> {
        let w = Wide::new(path);
        let mut h = HKEY::default();
        let r = unsafe {
            RegCreateKeyExW(HKEY_CURRENT_USER, w.pcwstr(), None, None, REG_OPTION_NON_VOLATILE, KEY_READ | KEY_WRITE, None, &mut h, None)
        };
        if r == ERROR_SUCCESS { Ok(Key(h)) } else { Err(r) }
    }

    pub(crate) fn open(path: &str, write: bool) -> Option<Key> {
        let w = Wide::new(path);
        let mut h = HKEY::default();
        let sam = if write { KEY_READ | KEY_WRITE } else { KEY_READ };
        (unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, w.pcwstr(), None, sam, &mut h) } == ERROR_SUCCESS).then_some(Key(h))
    }

    pub(crate) fn set(&self, name: Option<&str>, value: &str) -> Result<(), WIN32_ERROR> {
        let n = name.map(Wide::new);
        let data: Vec<u8> = value.encode_utf16().chain(std::iter::once(0)).flat_map(u16::to_le_bytes).collect();
        let r = unsafe {
            match &n {
                Some(n) => RegSetValueExW(self.0, n.pcwstr(), None, REG_SZ, Some(&data)),
                None => RegSetValueExW(self.0, windows::core::PCWSTR::null(), None, REG_SZ, Some(&data)),
            }
        };
        if r == ERROR_SUCCESS { Ok(()) } else { Err(r) }
    }

    /// A string value (`REG_SZ`/`REG_EXPAND_SZ` read as text); `None` when absent or of another type.
    pub(crate) fn get(&self, name: Option<&str>) -> Option<String> {
        let n = name.map(Wide::new);
        let pn = n.as_ref().map(|w| w.pcwstr()).unwrap_or(windows::core::PCWSTR::null());
        let mut ty = REG_VALUE_TYPE(0);
        let mut len = 0u32;
        unsafe {
            if RegQueryValueExW(self.0, pn, None, Some(&mut ty), None, Some(&mut len)) != ERROR_SUCCESS {
                return None;
            }
            if ty.0 != 1 && ty.0 != 2 {
                return None;
            }
            let mut buf = vec![0u8; len as usize + 2];
            let mut len2 = buf.len() as u32;
            if RegQueryValueExW(self.0, pn, None, Some(&mut ty), Some(buf.as_mut_ptr()), Some(&mut len2)) != ERROR_SUCCESS {
                return None;
            }
            let units: Vec<u16> = buf[..len2 as usize].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            Some(super::wide::from_buf(&units))
        }
    }

    pub(crate) fn delete_value(&self, name: Option<&str>) {
        let n = name.map(Wide::new);
        unsafe {
            let _ = RegDeleteValueW(self.0, n.as_ref().map(|w| w.pcwstr()).unwrap_or(windows::core::PCWSTR::null()));
        }
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

/// Deletes `HKCU\<path>` with everything below it; a missing key is fine.
pub(crate) fn delete_tree(path: &str) -> bool {
    let w = Wide::new(path);
    let r = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, w.pcwstr()) };
    r == ERROR_SUCCESS || r.0 == 2
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(String);
    impl Drop for Scratch {
        fn drop(&mut self) {
            delete_tree(&self.0);
        }
    }

    fn scratch() -> Scratch {
        Scratch(format!(r"Software\SubtitleAdderDesktopTests-{}-{}", std::process::id(), rand_suffix()))
    }

    fn rand_suffix() -> u128 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    }

    #[test]
    fn launch_rule_needs_the_exact_exe_name_and_no_opt_out() {
        assert!(should_register_on_launch(Path::new(r"C:\a\SubtitleAdder.exe"), None));
        assert!(should_register_on_launch(Path::new(r"C:\a\subtitleadder.EXE"), Some(false)));
        assert!(!should_register_on_launch(Path::new(r"C:\a\SubtitleAdder.exe"), Some(true)));
        assert!(!should_register_on_launch(Path::new(r"C:\a\sa-app.exe"), None));
    }

    #[test]
    fn test_instances_and_cargo_target_builds_never_register() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("SubtitleAdder.exe");
        assert!(!is_scratch_instance(&exe, false), "a staged or packaged exe registers");
        assert!(is_scratch_instance(&exe, true), "SUBTITLE_ADDER_DATA_DIR marks a test instance");
        std::fs::create_dir(dir.path().join(".fingerprint")).unwrap();
        assert!(is_scratch_instance(&exe, false), "an exe inside a cargo target folder");
    }

    #[test]
    fn registry_layout_is_byte_exact_and_unregister_cleans_up() {
        let s = scratch();
        let root = &s.0;
        let exe = PathBuf::from(r"C:\Program Files\Subtitle Adder\SubtitleAdder.exe");
        // An extension that already has another default keeps it.
        Key::create(&format!(r"{root}\Software\Classes\.mkv")).unwrap().set(None, "VLC.mkv").unwrap();
        let r = FileAssociationRegistrar::with_root(root, exe.clone(), None);
        assert!(!r.is_registered());
        assert!(r.register());
        assert!(r.is_registered());
        let get = |p: &str, n: Option<&str>| Key::open(&format!(r"{root}\{p}"), false).and_then(|k| k.get(n));
        let command = r#""C:\Program Files\Subtitle Adder\SubtitleAdder.exe" "%1""#;
        let icon = r"C:\Program Files\Subtitle Adder\SubtitleAdder.exe,0";
        assert_eq!(get(r"Software\Classes\SubtitleAdder.Media", None).as_deref(), Some("Subtitle Adder Media File"));
        assert_eq!(get(r"Software\Classes\SubtitleAdder.Media", Some("FriendlyTypeName")).as_deref(), Some("Subtitle Adder Media File"));
        assert_eq!(get(r"Software\Classes\SubtitleAdder.Media\DefaultIcon", None).as_deref(), Some(icon));
        assert_eq!(get(r"Software\Classes\SubtitleAdder.Media\shell\open", Some("FriendlyAppName")).as_deref(), Some("Subtitle Adder"));
        assert_eq!(get(r"Software\Classes\SubtitleAdder.Media\shell\open\command", None).as_deref(), Some(command));
        assert_eq!(get(r"Software\Classes\SubtitleAdder.Subtitle", None).as_deref(), Some("Subtitle Adder Subtitle File"));
        assert_eq!(get(r"Software\Classes\Applications\SubtitleAdder.exe", Some("FriendlyAppName")).as_deref(), Some("Subtitle Adder"));
        assert_eq!(get(r"Software\Classes\Applications\SubtitleAdder.exe\shell\open\command", None).as_deref(), Some(command));
        assert_eq!(get(r"Software\Classes\Applications\SubtitleAdder.exe\SupportedTypes", Some(".wma")).as_deref(), Some(""));
        assert_eq!(get(r"Software\Classes\Applications\SubtitleAdder.exe\SupportedTypes", Some(".ssa")).as_deref(), Some(""));
        assert_eq!(get(CAPABILITIES_PATH, Some("ApplicationDescription")).as_deref(), Some("Subtitle Adder — Media Player & AI Subtitle Studio"));
        assert_eq!(get(CAPABILITIES_PATH, Some("ApplicationIcon")).as_deref(), Some(icon));
        assert_eq!(get(&format!(r"{CAPABILITIES_PATH}\FileAssociations"), Some(".mp4")).as_deref(), Some("SubtitleAdder.Media"));
        assert_eq!(get(&format!(r"{CAPABILITIES_PATH}\FileAssociations"), Some(".vtt")).as_deref(), Some("SubtitleAdder.Subtitle"));
        assert_eq!(get(r"Software\RegisteredApplications", Some("Subtitle Adder")).as_deref(), Some(CAPABILITIES_PATH));
        assert_eq!(get(r"Software\Classes\.mp4", None).as_deref(), Some("SubtitleAdder.Media"));
        assert_eq!(get(r"Software\Classes\.mkv", None).as_deref(), Some("VLC.mkv"));
        assert_eq!(get(r"Software\Classes\.mkv\OpenWithProgids", Some("SubtitleAdder.Media")).as_deref(), Some(""));
        assert_eq!(get(r"Software\Classes\.srt\OpenWithProgids", Some("SubtitleAdder.Subtitle")).as_deref(), Some(""));
        // Idempotent.
        assert!(r.register());

        assert!(r.unregister());
        assert!(!r.is_registered());
        assert_eq!(get(r"Software\Classes\.mp4", None), None);
        assert_eq!(get(r"Software\Classes\.mkv", None).as_deref(), Some("VLC.mkv"));
        assert_eq!(get(r"Software\Classes\.mp4\OpenWithProgids", Some("SubtitleAdder.Media")), None);
        assert_eq!(get(r"Software\RegisteredApplications", Some("Subtitle Adder")), None);
        assert!(Key::open(&format!(r"{root}\Software\SubtitleAdder"), false).is_none());
        assert!(Key::open(&format!(r"{root}\Software\Classes\Applications\SubtitleAdder.exe"), false).is_none());
        assert!(Key::open(&format!(r"{root}\Software\Classes\SubtitleAdder.Media"), false).is_none());
        // Unregistering twice is fine.
        assert!(r.unregister());
    }
}
