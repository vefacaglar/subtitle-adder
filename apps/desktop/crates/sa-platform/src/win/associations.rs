//! HKCU self-registration (win-ui-main §3; C# `FileAssociationRegistrar`, registry layout byte-exact, no elevation).
//! Registered on every launch unless `fileAssociationsDisabled` is true, and only when the binary is
//! `SubtitleAdder.exe` ([`should_register_on_launch`]). The launch registration and the Settings actions are ordered
//! by a process-wide [`RegistrationGate`]: one writer at a time, and the launch one never runs after the user acted.
//!
//! The C#-era media extensions ([`MEDIA_EXTENSIONS`]) and [`SUBTITLE_EXTENSIONS`] get the C# tree exactly. The other
//! accepted media extensions ([`OPEN_WITH_ONLY_MEDIA_EXTENSIONS`]) are a deliberate addition: only an
//! `Applications\SubtitleAdder.exe\SupportedTypes` value and a `Capabilities\FileAssociations` value, so the app is
//! listed as an "Open with" handler (and under Settings › Default apps) without ever becoming their default handler.
//! No `HKCU\Software\Classes\.<ext>` key is created, read or deleted for them: measured on Windows 11, a per-user
//! `.<ext>\OpenWithProgids` entry alone turns a machine-wide default nobody chose (HKLM `.dts` = `VLC.dts`) into the
//! "choose an app" prompt, while the two values leave the default alone.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;
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

/// The fully associated media extensions (without dots): the C#-era 22 of [`sa_core::media_types`], registered exactly
/// as the C# app did. [`FileAssociationRegistrar::register_extension`] also writes the per-user default
/// (`HKCU\Software\Classes\.<ext>\(Default)`) when that value is empty, and an HKCU default overrides the machine-wide
/// one (HKLM) wherever the user has made no explicit choice, so the rest of the accepted list (`.vob`, `.ogv`, `.mka`,
/// …) is not registered this way: it would take over their default handler on machines where VLC or another player
/// owns them system-wide. Those are only listed as "Open with" types, see [`OPEN_WITH_ONLY_MEDIA_EXTENSIONS`].
pub const MEDIA_EXTENSIONS: &[&str] = &sa_core::media_types::CSHARP_MEDIA_EXTENSIONS;
/// The other accepted media extensions (without dots): registered as `SupportedTypes` and
/// `Capabilities\FileAssociations` values only, which make the app a recommended "Open with" handler and keep the
/// existing default. Their `Software\Classes\.<ext>` keys are never touched; not even an `OpenWithProgids` entry is
/// written, because on Windows 11 that alone replaces a machine-wide default the user never chose with the
/// "choose an app" prompt.
pub const OPEN_WITH_ONLY_MEDIA_EXTENSIONS: &[&str] = &sa_core::media_types::OPEN_WITH_ONLY_MEDIA_EXTENSIONS;
pub const SUBTITLE_EXTENSIONS: [&str; 4] = [".srt", ".vtt", ".ass", ".ssa"];

/// [`MEDIA_EXTENSIONS`] with their dots (`.mp4`), in order.
fn dotted_media_extensions() -> impl Iterator<Item = String> {
    MEDIA_EXTENSIONS.iter().map(|e| format!(".{e}"))
}

/// [`OPEN_WITH_ONLY_MEDIA_EXTENSIONS`] with their dots (`.vob`), in order.
fn dotted_open_with_only_extensions() -> impl Iterator<Item = String> {
    OPEN_WITH_ONLY_MEDIA_EXTENSIONS.iter().map(|e| format!(".{e}"))
}

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

/// Orders every registration and unregistration of one registry root: one runs at a time, and the launch-time
/// registration gives way to the user. The launch worker runs only while no explicit (Settings) call has been made in
/// this process; once one has, a launch registration that had not started its writes yet does nothing, so an
/// unregister from Settings is never undone by it.
#[derive(Debug, Default)]
pub struct RegistrationGate {
    /// `true` once an explicit `register()` / `unregister()` ran (or started) in this process.
    user_acted: Mutex<bool>,
}

impl RegistrationGate {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// The gate of the real `HKCU` root, shared by every [`FileAssociationRegistrar::new`] in the process.
    pub fn process() -> Arc<Self> {
        static GATE: OnceLock<Arc<RegistrationGate>> = OnceLock::new();
        GATE.get_or_init(RegistrationGate::new).clone()
    }
}

pub struct FileAssociationRegistrar {
    /// Prefix of every key path under HKCU (`""` in production; a scratch subtree in tests).
    root: String,
    exe: Option<PathBuf>,
    icon: Option<String>,
    notify_shell: bool,
    gate: Arc<RegistrationGate>,
}

impl FileAssociationRegistrar {
    pub fn new() -> Self {
        Self { root: String::new(), exe: None, icon: None, notify_shell: true, gate: RegistrationGate::process() }
    }

    /// Writes below `HKCU\<root>` with a fixed exe and icon and no shell notification (tests). Gets its own
    /// [`RegistrationGate`]; registrars that stand for one process share one through [`Self::with_gate`].
    pub fn with_root(root: &str, exe: PathBuf, icon: Option<String>) -> Self {
        Self { root: format!("{}\\", root.trim_end_matches('\\')), exe: Some(exe), icon, notify_shell: false, gate: RegistrationGate::new() }
    }

    /// The same registrar ordered by `gate`.
    pub fn with_gate(mut self, gate: Arc<RegistrationGate>) -> Self {
        self.gate = gate;
        self
    }

    /// Registers on a worker thread when [`should_register_on_launch`]; every error is swallowed. Ordered with the
    /// Settings actions through the process gate (see [`Self::register_on_launch`]).
    pub fn register_on_launch_in_background(file_associations_disabled: Option<bool>) {
        let Ok(exe) = std::env::current_exe() else { return };
        let data_dir_override = std::env::var_os(sa_core::platform::DATA_DIR_ENV).is_some_and(|v| !v.is_empty());
        if should_register_on_launch(&exe, file_associations_disabled) && !is_scratch_instance(&exe, data_dir_override) {
            let _ = std::thread::Builder::new().name("sa-file-associations".into()).spawn(|| {
                let _ = FileAssociationRegistrar::new().register_on_launch();
            });
        }
    }

    /// The launch-time registration: waits for a running Settings action, and does nothing (returns `false`) once
    /// the user registered or unregistered in this process. Otherwise registers, holding the gate for the whole
    /// write, so a Settings action that comes in meanwhile runs after it.
    pub fn register_on_launch(&self) -> bool {
        let acted = self.gate.user_acted.lock();
        if *acted {
            return false;
        }
        self.try_register().is_ok()
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
            for ext in dotted_media_extensions().chain(SUBTITLE_EXTENSIONS.iter().map(|e| (*e).to_owned())) {
                types.set(Some(&ext), "")?;
            }
        }
        {
            let cap = Key::create(&self.key(CAPABILITIES_PATH))?;
            cap.set(Some("ApplicationName"), APP_REGISTERED_NAME)?;
            cap.set(Some("ApplicationDescription"), APPLICATION_DESCRIPTION)?;
            cap.set(Some("ApplicationIcon"), &icon)?;
            let assoc = Key::create(&self.key(&format!(r"{CAPABILITIES_PATH}\FileAssociations")))?;
            for ext in dotted_media_extensions() {
                assoc.set(Some(&ext), MEDIA_PROG_ID)?;
            }
            for ext in SUBTITLE_EXTENSIONS {
                assoc.set(Some(ext), SUBTITLE_PROG_ID)?;
            }
        }
        Key::create(&self.key(r"Software\RegisteredApplications"))?.set(Some(APP_REGISTERED_NAME), CAPABILITIES_PATH)?;
        for ext in dotted_media_extensions() {
            self.register_extension(&ext, MEDIA_PROG_ID)?;
        }
        for ext in SUBTITLE_EXTENSIONS {
            self.register_extension(ext, SUBTITLE_PROG_ID)?;
        }
        self.register_open_with_only_best_effort(&app);
        self.notify();
        Ok(())
    }

    /// The [`OPEN_WITH_ONLY_MEDIA_EXTENSIONS`] values (`SupportedTypes` and `Capabilities\FileAssociations` only; no
    /// `Software\Classes\.<ext>` key), after the strict C#-era tree is written. Best effort: an error on one of them
    /// skips only that value and never makes `register()` fail or skip the shell notification, so a machine that
    /// registered cleanly before these additions still does. `unregister()` needs nothing extra: both values live in
    /// trees it deletes.
    fn register_open_with_only_best_effort(&self, app: &str) {
        let types = Key::create(&format!(r"{app}\SupportedTypes")).ok();
        // Declarative only: lets Settings › Default apps offer the app for these types; sets no default.
        let assoc = Key::create(&self.key(&format!(r"{CAPABILITIES_PATH}\FileAssociations"))).ok();
        for ext in dotted_open_with_only_extensions() {
            if let Some(types) = &types {
                let _ = types.set(Some(&ext), "");
            }
            if let Some(assoc) = &assoc {
                let _ = assoc.set(Some(&ext), MEDIA_PROG_ID);
            }
        }
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
    /// An explicit registration (Settings): ordered after a running launch registration, and ends the launch one.
    fn register(&self) -> bool {
        let mut acted = self.gate.user_acted.lock();
        *acted = true;
        self.try_register().is_ok()
    }

    /// An explicit unregistration (Settings): ordered after a running launch registration, and a launch registration
    /// that has not started yet never runs afterwards.
    fn unregister(&self) -> bool {
        let mut acted = self.gate.user_acted.lock();
        *acted = true;
        self.unregister_locked()
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

impl FileAssociationRegistrar {
    /// [`FileAssociations::unregister`] without the gate (the caller holds it).
    fn unregister_locked(&self) -> bool {
        for ext in dotted_media_extensions() {
            self.unregister_extension(&ext, MEDIA_PROG_ID);
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
    fn csharp_era_extensions_are_fully_associated_and_the_rest_are_open_with_only() {
        use sa_core::media_types::{AUDIO_EXTENSIONS, CSHARP_AUDIO_COUNT, CSHARP_VIDEO_COUNT, MEDIA_EXTENSIONS as ACCEPTED, VIDEO_EXTENSIONS};
        assert_eq!(MEDIA_EXTENSIONS.len(), 22);
        assert_eq!(MEDIA_EXTENSIONS[..CSHARP_VIDEO_COUNT], VIDEO_EXTENSIONS[..CSHARP_VIDEO_COUNT]);
        assert_eq!(MEDIA_EXTENSIONS[CSHARP_VIDEO_COUNT..], AUDIO_EXTENSIONS[..CSHARP_AUDIO_COUNT]);
        assert!(MEDIA_EXTENSIONS.iter().all(|e| ACCEPTED.contains(e)));
        assert_eq!(dotted_media_extensions().next().as_deref(), Some(".mp4"));
        assert_eq!(dotted_media_extensions().last().as_deref(), Some(".wma"));
        // Every accepted extension is in exactly one of the two lists.
        assert_eq!(OPEN_WITH_ONLY_MEDIA_EXTENSIONS.len(), 81);
        assert_eq!(MEDIA_EXTENSIONS.len() + OPEN_WITH_ONLY_MEDIA_EXTENSIONS.len(), ACCEPTED.len());
        assert!(ACCEPTED.iter().all(|e| MEDIA_EXTENSIONS.contains(e) != OPEN_WITH_ONLY_MEDIA_EXTENSIONS.contains(e)));
        assert_eq!(dotted_open_with_only_extensions().next().as_deref(), Some(".vob"));
        assert!(dotted_open_with_only_extensions().all(|e| !SUBTITLE_EXTENSIONS.contains(&e.as_str())));
    }

    fn get_at(root: &str, p: &str, n: Option<&str>) -> Option<String> {
        Key::open(&format!(r"{root}\{p}"), false).and_then(|k| k.get(n))
    }

    /// `HKCU\<path>` and everything below it as sorted `key|` / `key|name|data` lines (relative to `path`); empty when
    /// the key does not exist. String values only (all the tests write).
    fn dump(path: &str) -> Vec<String> {
        use windows::Win32::Foundation::ERROR_NO_MORE_ITEMS;
        use windows::Win32::System::Registry::{RegEnumKeyExW, RegEnumValueW};
        use windows::core::PWSTR;
        fn walk(path: &str, rel: &str, out: &mut Vec<String>) {
            let Some(k) = Key::open(path, false) else { return };
            out.push(format!("{rel}|"));
            for i in 0.. {
                let mut name = [0u16; 512];
                let mut name_len = name.len() as u32;
                let r = unsafe { RegEnumValueW(k.0, i, Some(PWSTR(name.as_mut_ptr())), &mut name_len, None, None, None, None) };
                if r == ERROR_NO_MORE_ITEMS {
                    break;
                }
                assert_eq!(r, ERROR_SUCCESS);
                let n = String::from_utf16(&name[..name_len as usize]).unwrap();
                let data = k.get(if n.is_empty() { None } else { Some(&n) }).unwrap();
                out.push(format!("{rel}|{n}|{data}"));
            }
            for i in 0.. {
                let mut name = [0u16; 512];
                let mut len = name.len() as u32;
                let r = unsafe { RegEnumKeyExW(k.0, i, Some(PWSTR(name.as_mut_ptr())), &mut len, None, None, None, None) };
                if r == ERROR_NO_MORE_ITEMS {
                    break;
                }
                assert_eq!(r, ERROR_SUCCESS);
                let sub = String::from_utf16(&name[..len as usize]).unwrap();
                walk(&format!(r"{path}\{sub}"), &format!(r"{rel}\{sub}"), out);
            }
        }
        let mut out = Vec::new();
        walk(path, "", &mut out);
        out.sort();
        out
    }

    #[test]
    fn open_with_only_extensions_never_touch_their_classes_keys() {
        let s = scratch();
        let root = &s.0;
        let classes = |ext: &str| format!(r"{root}\Software\Classes\{ext}");
        let ogv = Key::create(&classes(".ogv")).unwrap();
        ogv.set(None, "VLC.ogv").unwrap();
        ogv.set(Some("Content Type"), "video/ogg").unwrap();
        drop(ogv);
        Key::create(&format!(r"{}\OpenWithProgids", classes(".ogv"))).unwrap().set(Some("VLC.ogv"), "").unwrap();
        Key::create(&format!(r"{}\ShellEx", classes(".ogv"))).unwrap();
        Key::create(&classes(".mxf")).unwrap();
        let seeded = [dump(&classes(".ogv")), dump(&classes(".mxf")), dump(&classes(".vob"))];
        assert_eq!(seeded[1], ["|"]);
        assert!(seeded[2].is_empty());
        let unchanged = || {
            assert_eq!([dump(&classes(".ogv")), dump(&classes(".mxf")), dump(&classes(".vob"))], seeded);
            assert!(Key::open(&classes(".vob"), false).is_none());
        };
        let types = r"Software\Classes\Applications\SubtitleAdder.exe\SupportedTypes";
        let assoc = format!(r"{CAPABILITIES_PATH}\FileAssociations");
        let r = FileAssociationRegistrar::with_root(root, PathBuf::from(r"C:\Program Files\Subtitle Adder\SubtitleAdder.exe"), None);
        for _ in 0..2 {
            assert!(r.register());
            unchanged();
            for ext in [".ogv", ".mxf", ".vob"] {
                assert_eq!(get_at(root, types, Some(ext)).as_deref(), Some(""), "{ext}");
                assert_eq!(get_at(root, &assoc, Some(ext)).as_deref(), Some(MEDIA_PROG_ID), "{ext}");
            }
        }
        for _ in 0..2 {
            assert!(r.unregister());
            unchanged();
            for ext in [".ogv", ".mxf", ".vob"] {
                assert_eq!(get_at(root, types, Some(ext)), None, "{ext}");
                assert_eq!(get_at(root, &assoc, Some(ext)), None, "{ext}");
            }
        }
    }

    /// A user's unregister (Settings) wins over the launch registration in every order: the launch one either ran
    /// first (and is then removed) or has not started its writes and never runs.
    #[test]
    fn launch_registration_never_undoes_a_settings_unregister() {
        let exe = PathBuf::from(r"C:\Program Files\Subtitle Adder\SubtitleAdder.exe");
        // The launch worker has not started when the user unregisters: it does nothing afterwards.
        let s = scratch();
        let gate = RegistrationGate::new();
        let settings = FileAssociationRegistrar::with_root(&s.0, exe.clone(), None).with_gate(gate.clone());
        let launch = FileAssociationRegistrar::with_root(&s.0, exe.clone(), None).with_gate(gate.clone());
        assert!(settings.unregister());
        assert!(!launch.register_on_launch(), "skipped after the user acted");
        assert!(!settings.is_registered());
        // An explicit register is still honored, and a late launch one stays a no-op that removes nothing.
        assert!(settings.register());
        assert!(!launch.register_on_launch());
        assert!(settings.is_registered());
        drop(s);

        // The launch registration ran first: the unregister that follows removes it.
        let s = scratch();
        let gate = RegistrationGate::new();
        let settings = FileAssociationRegistrar::with_root(&s.0, exe.clone(), None).with_gate(gate.clone());
        let launch = FileAssociationRegistrar::with_root(&s.0, exe.clone(), None).with_gate(gate.clone());
        assert!(launch.register_on_launch());
        assert!(settings.is_registered());
        assert!(settings.unregister());
        assert!(!settings.is_registered());
        drop(s);

        // Racing: whichever takes the gate first, the end state is unregistered.
        for _ in 0..3 {
            let s = scratch();
            let gate = RegistrationGate::new();
            let settings = FileAssociationRegistrar::with_root(&s.0, exe.clone(), None).with_gate(gate.clone());
            let launch = FileAssociationRegistrar::with_root(&s.0, exe.clone(), None).with_gate(gate.clone());
            let start = std::sync::Barrier::new(2);
            std::thread::scope(|scope| {
                scope.spawn(|| {
                    start.wait();
                    launch.register_on_launch();
                });
                start.wait();
                assert!(settings.unregister());
            });
            assert!(!settings.is_registered(), "a launch registration undid the unregister");
        }
    }

    /// While a launch registration writes, a Settings call waits for it (one writer per root at a time).
    #[test]
    fn settings_calls_wait_for_a_running_launch_registration() {
        let s = scratch();
        let gate = RegistrationGate::new();
        let exe = PathBuf::from(r"C:\Program Files\Subtitle Adder\SubtitleAdder.exe");
        let settings = FileAssociationRegistrar::with_root(&s.0, exe, None).with_gate(gate.clone());
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let held = gate.user_acted.lock();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let ok = settings.unregister();
                done_tx.send(ok).unwrap();
            });
            assert!(done_rx.recv_timeout(std::time::Duration::from_millis(200)).is_err(), "ran while the gate was held");
            drop(held);
            assert_eq!(done_rx.recv_timeout(std::time::Duration::from_secs(10)), Ok(true));
        });
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
        // Accepted additions: SupportedTypes and Capabilities only, no `Software\Classes\.<ext>` key at all (see
        // `OPEN_WITH_ONLY_MEDIA_EXTENSIONS`).
        for ext in [".vob", ".mka"] {
            assert!(Key::open(&format!(r"{root}\Software\Classes\{ext}"), false).is_none(), "{ext}");
            assert_eq!(get(r"Software\Classes\Applications\SubtitleAdder.exe\SupportedTypes", Some(ext)).as_deref(), Some(""));
            assert_eq!(get(&format!(r"{CAPABILITIES_PATH}\FileAssociations"), Some(ext)).as_deref(), Some("SubtitleAdder.Media"));
        }
        // Idempotent.
        assert!(r.register());
        assert!(Key::open(&format!(r"{root}\Software\Classes\.vob"), false).is_none());

        assert!(r.unregister());
        assert!(!r.is_registered());
        assert_eq!(get(r"Software\Classes\.mp4", None), None);
        assert_eq!(get(r"Software\Classes\.mkv", None).as_deref(), Some("VLC.mkv"));
        assert_eq!(get(r"Software\Classes\.mp4\OpenWithProgids", Some("SubtitleAdder.Media")), None);
        assert_eq!(get(r"Software\RegisteredApplications", Some("Subtitle Adder")), None);
        assert!(Key::open(&format!(r"{root}\Software\SubtitleAdder"), false).is_none());
        assert!(Key::open(&format!(r"{root}\Software\Classes\Applications\SubtitleAdder.exe"), false).is_none());
        assert!(Key::open(&format!(r"{root}\Software\Classes\SubtitleAdder.Media"), false).is_none());
        // The additions never had a key; the C#-era ones keep the C# app's (empty) keys.
        assert!(Key::open(&format!(r"{root}\Software\Classes\.vob"), false).is_none());
        assert!(Key::open(&format!(r"{root}\Software\Classes\.mka"), false).is_none());
        assert!(Key::open(&format!(r"{root}\Software\Classes\.mp4\OpenWithProgids"), false).is_some());
        // Unregistering twice is fine.
        assert!(r.unregister());
    }
}
