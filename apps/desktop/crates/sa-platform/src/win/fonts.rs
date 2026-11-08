//! Installed font families from the DirectWrite system collection (C# `SystemFonts`, win-ui-main §20.2).

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
        if names.is_empty() { FALLBACK.iter().map(|s| (*s).to_owned()).collect() } else { sort_names(names) }
    })
}

/// Case-insensitive sort and de-dup (first spelling wins, like a `SortedSet` with an ignore-case comparer); empty
/// names and vertical fonts (`@…`) are dropped.
pub fn sort_names(names: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for n in names {
        if n.is_empty() || n.starts_with('@') {
            continue;
        }
        let key = n.to_lowercase();
        match out.binary_search_by(|e| e.to_lowercase().cmp(&key)) {
            Ok(_) => {}
            Err(i) => out.insert(i, n),
        }
    }
    out
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
    fn the_system_collection_lists_segoe() {
        let f = families();
        assert!(f.iter().any(|n| n == "Segoe UI"), "{f:?}");
        assert!(f.windows(2).all(|w| w[0].to_lowercase() < w[1].to_lowercase()));
    }
}
