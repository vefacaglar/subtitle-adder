//! .NET / Win32 compatibility helpers for the Windows profile store (byte- and behavior-compatible with the C# app):
//! `ToUpperInvariant`, `OrdinalIgnoreCase` maps, `CurrentCultureIgnoreCase` compare/contains (NLS with
//! `NORM_LINGUISTIC_CASING | NORM_IGNORECASE`, the flags .NET passes), `AtomicFileStore`, `Environment.UserName` /
//! `MachineName`, DPAPI (CurrentUser) and the cross-process named mutex.

use std::cmp::Ordering;
use std::path::Path;
use std::sync::LazyLock;

use indexmap::IndexMap;
use parking_lot::RwLock;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree, WAIT_ABANDONED, WAIT_OBJECT_0};
use windows::Win32::Globalization::{
    CSTR_EQUAL, CSTR_LESS_THAN, CompareStringEx, FIND_FROMSTART, FindNLSStringEx, NORM_IGNORECASE,
    NORM_LINGUISTIC_CASING,
};
use windows::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};
use windows::core::PCWSTR;

use super::upper_table::UPPER_INVARIANT;

// ---------------------------------------------------------------------------------------------------------------
// Case mapping

/// .NET `char.ToUpperInvariant` for one code point.
pub fn upper_char(c: char) -> char {
    if c.is_ascii() {
        return c.to_ascii_uppercase();
    }
    let cp = c as u32;
    match UPPER_INVARIANT.binary_search_by_key(&cp, |&(s, _)| s) {
        Ok(i) => char::from_u32(UPPER_INVARIANT[i].1).unwrap_or(c),
        Err(_) => c,
    }
}

/// .NET `string.ToUpperInvariant()` (simple per-code-point mapping; `ß` and `ı` stay, `i` → `I`).
pub fn upper_invariant(s: &str) -> String {
    if s.is_ascii() {
        return s.to_ascii_uppercase();
    }
    s.chars().map(upper_char).collect()
}

/// `StringComparison.OrdinalIgnoreCase` equality.
pub fn eq_ci(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.eq_ignore_ascii_case(b) || upper_invariant(a) == upper_invariant(b)
}

/// `StringComparer.OrdinalIgnoreCase` ordering (upper-cased UTF-16 code units).
pub fn cmp_ci(a: &str, b: &str) -> Ordering {
    let (a, b) = (upper_invariant(a), upper_invariant(b));
    a.encode_utf16().cmp(b.encode_utf16())
}

/// `string.StartsWith(prefix, OrdinalIgnoreCase)`.
pub fn starts_with_ci(s: &str, prefix: &str) -> bool {
    let (s, p) = (upper_invariant(s), upper_invariant(prefix));
    s.starts_with(&p)
}

/// `Option<string>` OrdinalIgnoreCase equality (`null == null`).
pub fn opt_eq_ci(a: Option<&str>, b: Option<&str>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => eq_ci(a, b),
        _ => false,
    }
}

/// An insertion-ordered dictionary with `StringComparer.OrdinalIgnoreCase` keys. Like a .NET `Dictionary`, setting
/// an existing key keeps the originally inserted key spelling.
#[derive(Clone, Debug)]
pub struct CiMap<V> {
    inner: IndexMap<String, (String, V)>,
}

impl<V> Default for CiMap<V> {
    fn default() -> Self {
        Self { inner: IndexMap::new() }
    }
}

impl<V> CiMap<V> {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn len(&self) -> usize {
        self.inner.len()
    }
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
    pub fn clear(&mut self) {
        self.inner.clear();
    }
    pub fn contains_key(&self, key: &str) -> bool {
        self.inner.contains_key(&upper_invariant(key))
    }
    pub fn get(&self, key: &str) -> Option<&V> {
        self.inner.get(&upper_invariant(key)).map(|(_, v)| v)
    }
    pub fn get_mut(&mut self, key: &str) -> Option<&mut V> {
        self.inner.get_mut(&upper_invariant(key)).map(|(_, v)| v)
    }
    /// `dict[key] = value`.
    pub fn insert(&mut self, key: &str, value: V) {
        let k = upper_invariant(key);
        match self.inner.get_mut(&k) {
            Some(slot) => slot.1 = value,
            None => {
                self.inner.insert(k, (key.to_owned(), value));
            }
        }
    }
    /// `dict.TryAdd`.
    pub fn try_add(&mut self, key: &str, value: V) -> bool {
        let k = upper_invariant(key);
        if self.inner.contains_key(&k) {
            return false;
        }
        self.inner.insert(k, (key.to_owned(), value));
        true
    }
    pub fn remove(&mut self, key: &str) -> Option<V> {
        self.inner.shift_remove(&upper_invariant(key)).map(|(_, v)| v)
    }
    /// The stored key spelling and value.
    pub fn get_key_value(&self, key: &str) -> Option<(&str, &V)> {
        self.inner.get(&upper_invariant(key)).map(|(k, v)| (k.as_str(), v))
    }
    pub fn iter(&self) -> impl Iterator<Item = (&str, &V)> {
        self.inner.values().map(|(k, v)| (k.as_str(), v))
    }
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&str, &mut V)> {
        self.inner.values_mut().map(|(k, v)| (k.as_str(), v))
    }
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.inner.values().map(|(k, _)| k.as_str())
    }
    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.inner.values().map(|(_, v)| v)
    }
}

/// A `HashSet<string>(StringComparer.OrdinalIgnoreCase)`.
#[derive(Clone, Debug, Default)]
pub struct CiSet {
    inner: indexmap::IndexSet<String>,
}

impl CiSet {
    pub fn new() -> Self {
        Self::default()
    }
    /// True when newly added.
    pub fn insert(&mut self, s: &str) -> bool {
        self.inner.insert(upper_invariant(s))
    }
    pub fn contains(&self, s: &str) -> bool {
        self.inner.contains(&upper_invariant(s))
    }
    pub fn remove(&mut self, s: &str) -> bool {
        self.inner.shift_remove(&upper_invariant(s))
    }
    pub fn clear(&mut self) {
        self.inner.clear();
    }
    pub fn len(&self) -> usize {
        self.inner.len()
    }
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Culture-sensitive comparisons (StringComparison.CurrentCultureIgnoreCase)

static CULTURE_OVERRIDE: LazyLock<RwLock<Option<Vec<u16>>>> = LazyLock::new(|| RwLock::new(None));

/// Overrides the culture used by the `CurrentCultureIgnoreCase` helpers (`None` = the user's default locale).
/// Tests use it to pin `tr-TR` regardless of the machine.
pub fn set_culture_override(locale: Option<&str>) {
    *CULTURE_OVERRIDE.write() = locale.map(|l| l.encode_utf16().chain(std::iter::once(0)).collect());
}

fn with_locale<R>(f: impl FnOnce(PCWSTR) -> R) -> R {
    let guard = CULTURE_OVERRIDE.read();
    match guard.as_ref() {
        Some(name) => f(PCWSTR(name.as_ptr())),
        None => f(PCWSTR::null()),
    }
}

/// `string.Compare(a, b, StringComparison.CurrentCultureIgnoreCase)` as an ordering.
pub fn culture_cmp_ci(a: &str, b: &str) -> Ordering {
    let (wa, wb): (Vec<u16>, Vec<u16>) = (a.encode_utf16().collect(), b.encode_utf16().collect());
    let r = with_locale(|loc| unsafe {
        CompareStringEx(loc, NORM_LINGUISTIC_CASING | NORM_IGNORECASE, &wa, &wb, None, None, None)
    });
    if r == CSTR_LESS_THAN {
        Ordering::Less
    } else if r == CSTR_EQUAL {
        Ordering::Equal
    } else if r.0 == 0 {
        // API failure: fall back to OrdinalIgnoreCase.
        cmp_ci(a, b)
    } else {
        Ordering::Greater
    }
}

/// `string.Equals(a, b, StringComparison.CurrentCultureIgnoreCase)`.
pub fn culture_eq_ci(a: &str, b: &str) -> bool {
    culture_cmp_ci(a, b) == Ordering::Equal
}

/// `haystack.Contains(needle, StringComparison.CurrentCultureIgnoreCase)`.
pub fn culture_contains_ci(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    if haystack.is_empty() {
        return false;
    }
    let (wh, wn): (Vec<u16>, Vec<u16>) = (haystack.encode_utf16().collect(), needle.encode_utf16().collect());
    let flags = FIND_FROMSTART | NORM_LINGUISTIC_CASING.0 | NORM_IGNORECASE.0;
    let idx = with_locale(|loc| unsafe {
        FindNLSStringEx(loc, flags, &wh, &wn, None, None, None, windows::Win32::Foundation::LPARAM(0))
    });
    idx >= 0
}

/// `haystack.Contains(needle, StringComparison.OrdinalIgnoreCase)`.
pub fn contains_ci(haystack: &str, needle: &str) -> bool {
    upper_invariant(haystack).contains(&upper_invariant(needle))
}

// ---------------------------------------------------------------------------------------------------------------
// AtomicFileStore (C# Settings.cs)

/// Decodes bytes like `StreamReader(UTF8, detectEncodingFromByteOrderMarks: true)`.
pub fn decode_with_bom(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    if bytes.len() >= 4 && bytes[..4] == [0xFF, 0xFE, 0, 0] {
        return decode_utf32(&bytes[4..], true);
    }
    if bytes.len() >= 4 && bytes[..4] == [0, 0, 0xFE, 0xFF] {
        return decode_utf32(&bytes[4..], false);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return decode_utf16(rest, true);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return decode_utf16(rest, false);
    }
    String::from_utf8_lossy(bytes).into_owned()
}

fn decode_utf16(b: &[u8], le: bool) -> String {
    let units = b.chunks_exact(2).map(|c| if le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) });
    char::decode_utf16(units).map(|r| r.unwrap_or('\u{FFFD}')).collect()
}

fn decode_utf32(b: &[u8], le: bool) -> String {
    b.chunks_exact(4)
        .map(|c| {
            let v = if le { u32::from_le_bytes([c[0], c[1], c[2], c[3]]) } else { u32::from_be_bytes([c[0], c[1], c[2], c[3]]) };
            char::from_u32(v).unwrap_or('\u{FFFD}')
        })
        .collect()
}

/// `AtomicFileStore.ReadAllTextShared`: `None` for a blank path, a missing file or after `max_attempts` failed reads
/// (sleeping `15 * attempt` ms between them).
pub fn read_all_text_shared(path: &Path, max_attempts: u32) -> Option<String> {
    if path.as_os_str().is_empty() {
        return None;
    }
    for attempt in 1..=max_attempts.max(1) {
        match std::fs::metadata(path) {
            Ok(m) if m.is_file() => {}
            Ok(_) => return None,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(_) => {}
        }
        // std opens with FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE.
        match std::fs::read(path) {
            Ok(bytes) => return Some(decode_with_bom(&bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(_) => {
                if attempt == max_attempts.max(1) {
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(15 * u64::from(attempt)));
            }
        }
    }
    None
}

/// `AtomicFileStore.WriteAllTextAtomic`: writes `"{path}.{pid}.{guid}.tmp"` (UTF-8, no BOM) and replaces `path` with
/// `MoveFileExW(MOVEFILE_REPLACE_EXISTING | MOVEFILE_COPY_ALLOWED)`; the temp file is always removed.
pub fn write_all_text_atomic(path: &Path, content: &str, max_attempts: u32) -> bool {
    write_all_bytes_atomic(path, content.as_bytes(), max_attempts)
}

pub fn write_all_bytes_atomic(path: &Path, content: &[u8], max_attempts: u32) -> bool {
    use windows::Win32::Storage::FileSystem::{MOVEFILE_COPY_ALLOWED, MOVEFILE_REPLACE_EXISTING, MoveFileExW};
    if path.as_os_str().is_empty() {
        return false;
    }
    let tmp = {
        let mut s = path.as_os_str().to_owned();
        s.push(format!(".{}.{}.tmp", std::process::id(), sa_core::text::guid_n()));
        std::path::PathBuf::from(s)
    };
    let result = (|| {
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() && std::fs::create_dir_all(dir).is_err() {
                return false;
            }
        }
        if std::fs::write(&tmp, content).is_err() {
            return false;
        }
        let wtmp = wide(tmp.as_os_str());
        let wdst = wide(path.as_os_str());
        for attempt in 1..=max_attempts.max(1) {
            let ok = unsafe {
                MoveFileExW(PCWSTR(wtmp.as_ptr()), PCWSTR(wdst.as_ptr()), MOVEFILE_REPLACE_EXISTING | MOVEFILE_COPY_ALLOWED)
            }
            .is_ok();
            if ok {
                return true;
            }
            if attempt == max_attempts.max(1) {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(15 * u64::from(attempt)));
        }
        false
    })();
    if tmp.exists() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// NUL-terminated UTF-16.
pub fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    s.encode_wide().chain(std::iter::once(0)).collect()
}

// ---------------------------------------------------------------------------------------------------------------
// Environment.UserName / Environment.MachineName

/// .NET `Environment.UserName` on Windows: `GetUserNameExW(NameSamCompatible)` without the `DOMAIN\` part
/// (`GetUserNameW` if that fails).
pub fn environment_user_name() -> String {
    use windows::Win32::Security::Authentication::Identity::{GetUserNameExW, NameSamCompatible};
    use windows::Win32::System::WindowsProgramming::GetUserNameW;
    let mut size: u32 = 0;
    unsafe {
        let _ = GetUserNameExW(NameSamCompatible, None, &mut size);
    }
    if size > 0 {
        let mut buf = vec![0u16; size as usize + 1];
        let mut len = buf.len() as u32;
        let ok = unsafe { GetUserNameExW(NameSamCompatible, Some(windows::core::PWSTR(buf.as_mut_ptr())), &mut len) };
        if ok {
            let name = String::from_utf16_lossy(&buf[..len as usize]);
            return match name.find('\\') {
                Some(i) => name[i + 1..].to_owned(),
                None => name,
            };
        }
    }
    let mut buf = vec![0u16; 257];
    let mut len = buf.len() as u32;
    if unsafe { GetUserNameW(Some(windows::core::PWSTR(buf.as_mut_ptr())), &mut len) }.is_ok() && len > 0 {
        // The count includes the terminating NUL.
        return String::from_utf16_lossy(&buf[..(len as usize).saturating_sub(1)]);
    }
    String::new()
}

/// .NET `Environment.MachineName` (`GetComputerNameW`, as returned).
pub fn environment_machine_name() -> String {
    use windows::Win32::System::WindowsProgramming::GetComputerNameW;
    let mut buf = vec![0u16; 64];
    let mut len = buf.len() as u32;
    if unsafe { GetComputerNameW(Some(windows::core::PWSTR(buf.as_mut_ptr())), &mut len) }.is_ok() {
        return String::from_utf16_lossy(&buf[..len as usize]);
    }
    String::new()
}

// ---------------------------------------------------------------------------------------------------------------
// DPAPI (C# WindowsDataProtection, current-user scope, CRYPTPROTECT_UI_FORBIDDEN)

pub fn dpapi_protect(data: &[u8], entropy: &[u8]) -> Option<Vec<u8>> {
    dpapi_run(data, entropy, true)
}

pub fn dpapi_unprotect(data: &[u8], entropy: &[u8]) -> Option<Vec<u8>> {
    dpapi_run(data, entropy, false)
}

fn dpapi_run(data: &[u8], entropy: &[u8], protect: bool) -> Option<Vec<u8>> {
    let mut input = data.to_vec();
    let mut ent = entropy.to_vec();
    let in_blob = CRYPT_INTEGER_BLOB { cbData: input.len() as u32, pbData: input.as_mut_ptr() };
    let ent_blob = CRYPT_INTEGER_BLOB { cbData: ent.len() as u32, pbData: ent.as_mut_ptr() };
    let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
    let ok = unsafe {
        if protect {
            CryptProtectData(&in_blob, PCWSTR::null(), Some(&ent_blob), None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out)
        } else {
            CryptUnprotectData(&in_blob, None, Some(&ent_blob), None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out)
        }
    }
    .is_ok();
    zeroize::Zeroize::zeroize(&mut input);
    if !ok || out.pbData.is_null() {
        return None;
    }
    let result = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec() };
    unsafe {
        if !protect {
            std::ptr::write_bytes(out.pbData, 0, out.cbData as usize);
        }
        let _ = LocalFree(Some(HLOCAL(out.pbData as *mut core::ffi::c_void)));
    }
    Some(result)
}

// ---------------------------------------------------------------------------------------------------------------
// Cross-process mutex (C# CrossProcessMutexScope)

/// Holds `Local\SubtitleAdder.Profiles.<h16>` for its lifetime when it could be acquired within 2 s (abandoned counts
/// as acquired). Creation failure or timeout proceeds without the lock.
pub struct CrossProcessMutex {
    handle: Option<HANDLE>,
    acquired: bool,
}

impl CrossProcessMutex {
    pub fn acquire(name: &str, timeout_ms: u32) -> Self {
        let wname: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        let handle = unsafe { CreateMutexW(None, false, PCWSTR(wname.as_ptr())) }.ok();
        let acquired = match handle {
            Some(h) => {
                let r = unsafe { WaitForSingleObject(h, timeout_ms) };
                r == WAIT_OBJECT_0 || r == WAIT_ABANDONED
            }
            None => false,
        };
        Self { handle, acquired }
    }

    pub fn is_acquired(&self) -> bool {
        self.acquired
    }
}

impl Drop for CrossProcessMutex {
    fn drop(&mut self) {
        if let Some(h) = self.handle.take() {
            unsafe {
                if self.acquired {
                    let _ = ReleaseMutex(h);
                }
                let _ = CloseHandle(h);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upper_invariant_uses_the_dotnet_simple_mapping() {
        assert_eq!(upper_invariant("istanbul"), "ISTANBUL");
        assert_eq!(upper_invariant("ß"), "ß");
        assert_eq!(upper_invariant("ı"), "ı"); // .NET 10 leaves dotless i unmapped
        assert_eq!(upper_invariant("çğöşü"), "ÇĞÖŞÜ");
        assert_eq!(upper_invariant("ǆ"), "Ǆ");
        assert!(eq_ci("C:\\Videos\\Şarkı.mp4", "c:\\videos\\şARKı.MP4"));
        assert!(!eq_ci("i", "İ"));
    }

    #[test]
    fn ci_map_keeps_first_spelling_and_order() {
        let mut m = CiMap::new();
        m.insert("Alice", 1);
        m.insert("bob", 2);
        m.insert("ALICE", 3);
        assert_eq!(m.get("alice"), Some(&3));
        assert_eq!(m.keys().collect::<Vec<_>>(), vec!["Alice", "bob"]);
    }

    #[test]
    fn turkish_culture_folds_dotted_i() {
        set_culture_override(Some("tr-TR"));
        assert!(culture_eq_ci("daha sonra izle", "Daha Sonra İzle"));
        assert!(culture_contains_ci("Sezon_2_Bolum_05.mp4", "bolum"));
        set_culture_override(None);
    }

    #[test]
    fn bom_detection() {
        assert_eq!(decode_with_bom(b"\xEF\xBB\xBF{}"), "{}");
        assert_eq!(decode_with_bom(&[0xFF, 0xFE, b'{', 0, b'}', 0]), "{}");
    }
}
