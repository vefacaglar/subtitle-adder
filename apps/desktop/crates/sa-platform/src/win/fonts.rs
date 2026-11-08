//! Installed font families from the DirectWrite system collection (C# `SystemFonts`, win-ui-main §20.2).
//!
//! The C# list is a `SortedSet` with `StringComparer.CurrentCultureIgnoreCase`; .NET on Windows 10 1903+ compares
//! through ICU (`icu.dll`, collator of the user's locale at secondary strength), so the order here comes from the
//! same ICU collator ([`CultureCollator`]). Ordinal case-insensitive order is the fallback without `icu.dll`.

use std::cmp::Ordering;
use std::ffi::c_void;
use std::sync::OnceLock;

use windows::Win32::Graphics::DirectWrite::{DWRITE_FACTORY_TYPE_SHARED, DWriteCreateFactory, IDWriteFactory, IDWriteLocalizedStrings};
use windows::core::{BOOL, w};

/// Used when DirectWrite is unavailable or lists nothing.
pub const FALLBACK: [&str; 8] = ["Arial", "Calibri", "Consolas", "Segoe UI", "Segoe UI Variable", "Tahoma", "Times New Roman", "Verdana"];

/// Sorted, de-duplicated (case-insensitive) family names; enumerated once per process.
pub fn families() -> &'static [String] {
    static FAMILIES: OnceLock<Vec<String>> = OnceLock::new();
    FAMILIES.get_or_init(|| {
        let names = enumerate().unwrap_or_default();
        if names.is_empty() {
            return FALLBACK.iter().map(|s| (*s).to_owned()).collect();
        }
        match CultureCollator::current() {
            Some(c) => sort_names_by(names, |a, b| c.compare(a, b)),
            None => sort_names(names),
        }
    })
}

/// The WinUI system font (`XamlAutoFontFamily`, win-ui-main §1.3): Segoe UI Variable (its Text optical size, the
/// one for body text) on Windows 11, Segoe UI on Windows 10.
pub fn system_ui_font_family() -> &'static str {
    pick_system_ui_font(families())
}

/// [`system_ui_font_family`] over a given family list.
pub fn pick_system_ui_font(families: &[String]) -> &'static str {
    let has = |name: &str| families.iter().any(|f| f.eq_ignore_ascii_case(name));
    if has("Segoe UI Variable Text") {
        "Segoe UI Variable Text"
    } else if has("Segoe UI Variable") {
        "Segoe UI Variable"
    } else {
        "Segoe UI"
    }
}

/// The interface font for `appFontFamily` (win-ui-main §1.3, applied once at startup): the trimmed setting, or the
/// system font when it is empty or whitespace.
pub fn ui_font_family(app_font_family: Option<&str>) -> String {
    match app_font_family.map(sa_core::text::trim_dotnet).filter(|s| !s.is_empty()) {
        Some(f) => f.to_owned(),
        None => system_ui_font_family().to_owned(),
    }
}

/// Case-insensitive sort and de-dup (first spelling wins, like a `SortedSet` with an ignore-case comparer); empty
/// names and vertical fonts (`@…`) are dropped. Ordinal (lower-cased) order; see [`sort_names_by`].
pub fn sort_names(names: Vec<String>) -> Vec<String> {
    sort_names_by(names, |a, b| a.to_lowercase().cmp(&b.to_lowercase()))
}

/// `SortedSet<string>(comparer)` filled in enumeration order: an element the comparer finds equal to one already in
/// the set is dropped (the first spelling stays); empty names and vertical fonts (`@…`) are skipped.
pub fn sort_names_by(names: Vec<String>, mut compare: impl FnMut(&str, &str) -> Ordering) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for n in names {
        if n.is_empty() || n.starts_with('@') {
            continue;
        }
        match out.binary_search_by(|e| compare(e, &n)) {
            Ok(_) => {}
            Err(i) => out.insert(i, n),
        }
    }
    out
}

// ---- ICU collation (what .NET's CurrentCultureIgnoreCase uses on Windows) ------------------------------------------

type UcolOpen = unsafe extern "C" fn(locale: *const u8, status: *mut i32) -> *mut c_void;
type UcolSetAttribute = unsafe extern "C" fn(coll: *mut c_void, attr: i32, value: i32, status: *mut i32);
type UcolStrcoll = unsafe extern "C" fn(coll: *const c_void, a: *const u16, a_len: i32, b: *const u16, b_len: i32) -> i32;
type UcolClose = unsafe extern "C" fn(coll: *mut c_void);

const UCOL_STRENGTH: i32 = 5;
const UCOL_SECONDARY: i32 = 1;

/// An ICU collator of the user's locale (`GetUserDefaultLocaleName`, .NET `CultureInfo.CurrentCulture`) with
/// secondary strength (case ignored, accents not), from the system `icu.dll`. Single-threaded use.
pub struct CultureCollator {
    coll: *mut c_void,
    strcoll: UcolStrcoll,
    close: UcolClose,
}

impl CultureCollator {
    /// `None` when `icu.dll` (Windows 10 1903+) or the locale's collator is unavailable.
    pub fn current() -> Option<CultureCollator> {
        Self::for_locale(&user_locale_name())
    }

    /// A collator for a BCP-47 name (`tr-TR`).
    pub fn for_locale(bcp47: &str) -> Option<CultureCollator> {
        use windows::Win32::System::LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW};
        use windows::core::s;
        unsafe {
            let lib = LoadLibraryExW(w!("icu.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32).ok()?;
            let open: UcolOpen = std::mem::transmute(GetProcAddress(lib, s!("ucol_open"))?);
            let set_attribute: UcolSetAttribute = std::mem::transmute(GetProcAddress(lib, s!("ucol_setAttribute"))?);
            let strcoll: UcolStrcoll = std::mem::transmute(GetProcAddress(lib, s!("ucol_strcoll"))?);
            let close: UcolClose = std::mem::transmute(GetProcAddress(lib, s!("ucol_close"))?);
            let locale: Vec<u8> = bcp47.replace('-', "_").bytes().chain(std::iter::once(0)).collect();
            let mut status = 0i32;
            let coll = open(locale.as_ptr(), &mut status);
            // ICU errors are > 0 (warnings such as a fallback locale are < 0).
            if coll.is_null() || status > 0 {
                return None;
            }
            set_attribute(coll, UCOL_STRENGTH, UCOL_SECONDARY, &mut status);
            if status > 0 {
                close(coll);
                return None;
            }
            Some(CultureCollator { coll, strcoll, close })
        }
    }

    pub fn compare(&self, a: &str, b: &str) -> Ordering {
        let a: Vec<u16> = a.encode_utf16().collect();
        let b: Vec<u16> = b.encode_utf16().collect();
        match unsafe { (self.strcoll)(self.coll, a.as_ptr(), a.len() as i32, b.as_ptr(), b.len() as i32) } {
            r if r < 0 => Ordering::Less,
            0 => Ordering::Equal,
            _ => Ordering::Greater,
        }
    }
}

impl Drop for CultureCollator {
    fn drop(&mut self) {
        unsafe { (self.close)(self.coll) };
    }
}

/// `GetUserDefaultLocaleName` (`tr-TR`), `en-US` when it fails.
pub fn user_locale_name() -> String {
    use windows::Win32::Globalization::GetUserDefaultLocaleName;
    let mut buf = [0u16; 85];
    let n = unsafe { GetUserDefaultLocaleName(&mut buf) };
    if n > 1 { String::from_utf16_lossy(&buf[..n as usize - 1]) } else { "en-US".to_owned() }
}

fn enumerate() -> windows::core::Result<Vec<String>> {
    unsafe {
        let factory: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
        let mut collection = None;
        factory.GetSystemFontCollection(&mut collection, false)?;
        let Some(collection) = collection else { return Ok(Vec::new()) };
        let mut names = Vec::new();
        for i in 0..collection.GetFontFamilyCount() {
            let Ok(family) = collection.GetFontFamily(i) else { continue };
            let Ok(strings) = family.GetFamilyNames() else { continue };
            if let Some(name) = read_name(&strings) {
                names.push(name);
            }
        }
        Ok(names)
    }
}

/// The `en-us` name when present (what font-family lookups accept everywhere), else the first one.
unsafe fn read_name(strings: &IDWriteLocalizedStrings) -> Option<String> {
    unsafe {
        let mut index = 0u32;
        let mut exists = BOOL(0);
        strings.FindLocaleName(w!("en-us"), &mut index, &mut exists).ok()?;
        if !exists.as_bool() {
            index = 0;
        }
        let len = strings.GetStringLength(index).ok()?;
        let mut buf = vec![0u16; len as usize + 1];
        strings.GetString(index, &mut buf).ok()?;
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_sort_case_insensitively_without_vertical_fonts() {
        let v = sort_names(vec!["b".into(), "@MS Gothic".into(), "A".into(), "a".into(), "".into(), "C".into()]);
        assert_eq!(v, ["A", "b", "C"]);
    }

    #[test]
    fn culture_order_follows_icu_like_dotnet() {
        let Some(root) = CultureCollator::for_locale("en-US") else { return };
        let names = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        // ICU puts `_` before `-` (ordinal does the opposite) and compares case-insensitively.
        let v = sort_names_by(names(&["MingLiU-ExtB", "MingLiU_HKSCS", "mingliu", "MingLiU"]), |a, b| root.compare(a, b));
        assert_eq!(v, ["mingliu", "MingLiU_HKSCS", "MingLiU-ExtB"]);
        // Turkish: dotless ı sorts before i, and I is the capital of ı.
        if let Some(tr) = CultureCollator::for_locale("tr-TR") {
            assert_eq!(tr.compare("Ink", "ınk"), Ordering::Equal);
            assert_eq!(tr.compare("Ink", "ink"), Ordering::Less);
        }
        assert!(!user_locale_name().is_empty());
    }

    #[test]
    fn the_system_font_prefers_segoe_ui_variable() {
        let v = |n: &[&str]| n.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert_eq!(pick_system_ui_font(&v(&["Arial", "Segoe UI", "Segoe UI Variable Display", "Segoe UI Variable Text"])), "Segoe UI Variable Text");
        assert_eq!(pick_system_ui_font(&v(&["Segoe UI Variable"])), "Segoe UI Variable");
        assert_eq!(pick_system_ui_font(&v(&["Arial", "Segoe UI"])), "Segoe UI");
        assert_eq!(ui_font_family(Some("  Verdana  ")), "Verdana");
        assert_eq!(ui_font_family(Some("   ")), system_ui_font_family());
        assert_eq!(ui_font_family(None), system_ui_font_family());
    }

    #[test]
    fn the_system_collection_lists_segoe() {
        let f = families();
        assert!(f.iter().any(|n| n == "Segoe UI"), "{f:?}");
        match CultureCollator::current() {
            Some(c) => assert!(f.windows(2).all(|w| c.compare(&w[0], &w[1]) == Ordering::Less)),
            None => assert!(f.windows(2).all(|w| w[0].to_lowercase() < w[1].to_lowercase())),
        }
    }
}
