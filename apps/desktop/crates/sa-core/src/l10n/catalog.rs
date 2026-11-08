//! Apple `.xcstrings` string catalogs (win-core §4.2, mac-enginekit §5.3).
//!
//! The catalogs are embedded at build time from the existing app repositories (never copied):
//! the macOS app catalog and the EngineKit catalog are the source of truth for shared strings, the Windows catalog
//! overrides a few of them on Windows. Strings the Rust app adds (UNIFIED.md §8) live in the two `Desktop*` overlay
//! catalogs of this crate, because the existing apps' folders must not be modified.

use std::collections::HashMap;
use std::sync::OnceLock;

/// A parsed `.xcstrings` catalog: `key → (language code → value)`.
///
/// Only `strings.<key>.localizations.<lang>.stringUnit.value` nodes of the right JSON types are kept; everything else
/// (`state`, `extractionState`, `comment`, `variations`, `substitutions`) is ignored. A key without any valid unit is
/// absent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StringCatalog {
    table: HashMap<String, HashMap<String, String>>,
    order: Vec<String>,
}

impl StringCatalog {
    /// An empty catalog.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Parses catalog JSON. Invalid JSON, a non-object root or a missing `strings` object give an empty catalog
    /// (never panics).
    pub fn parse(json: &str) -> Self {
        let Ok(root) = serde_json::from_str::<serde_json::Value>(json) else {
            return Self::empty();
        };
        let Some(strings) = root.get("strings").and_then(|s| s.as_object()) else {
            return Self::empty();
        };
        let mut table = HashMap::with_capacity(strings.len());
        let mut order = Vec::with_capacity(strings.len());
        for (key, entry) in strings {
            let Some(locs) = entry.get("localizations").and_then(|l| l.as_object()) else {
                continue;
            };
            let mut units = HashMap::new();
            for (lang, loc) in locs {
                if let Some(value) = loc
                    .as_object()
                    .and_then(|o| o.get("stringUnit"))
                    .and_then(|u| u.as_object())
                    .and_then(|u| u.get("value"))
                    .and_then(|v| v.as_str())
                {
                    units.insert(lang.clone(), value.to_owned());
                }
            }
            if !units.is_empty() {
                order.push(key.clone());
                table.insert(key.clone(), units);
            }
        }
        Self { table, order }
    }

    /// Exact key, exact language code.
    pub fn lookup(&self, key: &str, language: &str) -> Option<&str> {
        self.table.get(key).and_then(|m| m.get(language)).map(String::as_str)
    }

    /// Whether the key has a unit for `language`.
    pub fn has(&self, key: &str, language: &str) -> bool {
        self.lookup(key, language).is_some()
    }

    /// Every key with at least one unit, in file order.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.order.iter().map(String::as_str)
    }

    /// Number of keys with at least one unit.
    pub fn len(&self) -> usize {
        self.order.len()
    }

    /// True when the catalog has no key.
    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// Whether the key exists (in any language).
    pub fn contains_key(&self, key: &str) -> bool {
        self.table.contains_key(key)
    }
}

/// The embedded catalogs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CatalogId {
    /// `apps/macos/Sources/SubtitleAdderApp/Resources/Localizable.xcstrings` (always embedded: both chains use it).
    App,
    /// `apps/macos/Sources/EngineKit/Resources/Localizable.xcstrings` (always embedded).
    EngineKit,
    /// `apps/windows/src/SubtitleAdder.Core/Strings/Windows.xcstrings` (Windows builds, or feature `all-catalogs`).
    Windows,
    /// `crates/sa-core/catalogs/DesktopWindows.xcstrings`: strings the Rust app adds on Windows.
    DesktopWindows,
    /// `crates/sa-core/catalogs/DesktopMac.xcstrings`: strings the Rust app adds on macOS.
    DesktopMac,
}

macro_rules! embedded {
    ($cell:ident, $path:expr) => {{
        static $cell: OnceLock<StringCatalog> = OnceLock::new();
        $cell.get_or_init(|| StringCatalog::parse(include_str!($path)))
    }};
}

/// Returns an embedded catalog, parsed once. A catalog not embedded in this build (see [`CatalogId`]) is empty.
pub fn catalog(id: CatalogId) -> &'static StringCatalog {
    static EMPTY: OnceLock<StringCatalog> = OnceLock::new();
    match id {
        CatalogId::App => embedded!(
            APP,
            concat!(env!("CARGO_MANIFEST_DIR"), "/../../../macos/Sources/SubtitleAdderApp/Resources/Localizable.xcstrings")
        ),
        CatalogId::EngineKit => embedded!(
            ENGINE_KIT,
            concat!(env!("CARGO_MANIFEST_DIR"), "/../../../macos/Sources/EngineKit/Resources/Localizable.xcstrings")
        ),
        #[cfg(any(windows, feature = "all-catalogs"))]
        CatalogId::Windows => embedded!(
            WINDOWS,
            concat!(env!("CARGO_MANIFEST_DIR"), "/../../../windows/src/SubtitleAdder.Core/Strings/Windows.xcstrings")
        ),
        #[cfg(any(windows, feature = "all-catalogs"))]
        CatalogId::DesktopWindows => embedded!(
            DESKTOP_WINDOWS,
            concat!(env!("CARGO_MANIFEST_DIR"), "/catalogs/DesktopWindows.xcstrings")
        ),
        #[cfg(any(not(windows), feature = "all-catalogs"))]
        CatalogId::DesktopMac => embedded!(
            DESKTOP_MAC,
            concat!(env!("CARGO_MANIFEST_DIR"), "/catalogs/DesktopMac.xcstrings")
        ),
        #[allow(unreachable_patterns)]
        _ => EMPTY.get_or_init(StringCatalog::empty),
    }
}

/// Whether `id` is embedded in this build.
pub fn is_embedded(id: CatalogId) -> bool {
    match id {
        CatalogId::App | CatalogId::EngineKit => true,
        CatalogId::Windows | CatalogId::DesktopWindows => cfg!(any(windows, feature = "all-catalogs")),
        CatalogId::DesktopMac => cfg!(any(not(windows), feature = "all-catalogs")),
    }
}

/// The lookup chain for app strings (`L()`), per platform (UNIFIED.md §3.18, §6 `catalog_chain`).
///
/// * Windows: `[DesktopWindows, Windows, App, EngineKit]` (the Windows catalog wins over the App catalog, e.g. tr
///   `Ses ayrılıyor… 42%`).
/// * macOS: `[DesktopMac, App, EngineKit]` (e.g. tr `Ses ayrılıyor… %42`).
///
/// The `Desktop*` overlays come first but only hold keys no other catalog of the chain has (enforced by a test), so
/// they never change an existing string. EngineKit is the last fallback on both (win-enginekit §8.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CatalogChain {
    Windows,
    Mac,
    /// Engine-client strings (`engine_string`): `[EngineKit]` only, on both platforms.
    Engine,
}

impl CatalogChain {
    /// The chain of the platform this binary was built for.
    pub const fn native() -> Self {
        if cfg!(windows) { CatalogChain::Windows } else { CatalogChain::Mac }
    }

    /// The catalogs in lookup order.
    pub fn ids(self) -> &'static [CatalogId] {
        match self {
            CatalogChain::Windows => &[CatalogId::DesktopWindows, CatalogId::Windows, CatalogId::App, CatalogId::EngineKit],
            CatalogChain::Mac => &[CatalogId::DesktopMac, CatalogId::App, CatalogId::EngineKit],
            CatalogChain::Engine => &[CatalogId::EngineKit],
        }
    }

    /// The parsed catalogs in lookup order.
    pub fn catalogs(self) -> Vec<&'static StringCatalog> {
        self.ids().iter().map(|id| catalog(*id)).collect()
    }

    /// First value for `key` in `language` along the chain.
    pub fn lookup(self, key: &str, language: &str) -> Option<&'static str> {
        self.ids().iter().find_map(|id| catalog(*id).lookup(key, language))
    }

    /// Whether a single catalog of the chain has both `tr` and `en` for `key` (catalog completeness rule).
    pub fn has_both_languages(self, key: &str) -> bool {
        self.ids().iter().any(|id| {
            let c = catalog(*id);
            c.has(key, "tr") && c.has(key, "en")
        })
    }
}
