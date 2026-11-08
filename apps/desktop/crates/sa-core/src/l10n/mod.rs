//! Localization: catalogs embedded from the existing apps, `L()` lookup chains per platform, Apple printf
//! formatting, the process-wide UI language and live switching (win-core §4, mac-enginekit §5, UNIFIED.md §3.18).
//!
//! Keys are the **Turkish source strings**. A missing translation falls back to the key, so Turkish always exists and
//! an unknown key in English shows Turkish. With no arguments a format is returned unformatted (`"%lld%%"` stays
//! `"%lld%%"`).
//!
//! Slint side: every visible string is `Loc.t("key")` (or `Loc.t1/t2` with string arguments) on the shared `Loc`
//! global, whose functions read `Loc.revision`; Rust bumps the revision on the UI thread after
//! [`LanguageState::set_language`], so every binding re-evaluates immediately (no restart). See ARCHITECTURE.md §6.

mod catalog;
mod language;
mod message;
mod printf;
pub mod scan;

use std::sync::atomic::{AtomicU8, Ordering};

pub use catalog::{CatalogChain, CatalogId, StringCatalog, catalog, is_embedded};
pub use language::{AppLanguage, AppLanguageChange, LanguageChoice, LanguageState, UiLanguage};
pub use message::LocalizedMessage;
pub use printf::{Arg, printf};

static ACTIVE_CHAIN: AtomicU8 = AtomicU8::new(0); // 0 = native, 1 = Windows, 2 = Mac

/// The chain `L()` uses: the native one unless [`set_active_chain`] overrode it.
pub fn active_chain() -> CatalogChain {
    match ACTIVE_CHAIN.load(Ordering::Relaxed) {
        1 => CatalogChain::Windows,
        2 => CatalogChain::Mac,
        _ => CatalogChain::native(),
    }
}

/// Overrides the app chain for the whole process (only the cross-platform screenshot tool and tests; needs the
/// `all-catalogs` feature for the non-native chain to have content). `CatalogChain::Engine` resets to native.
pub fn set_active_chain(chain: CatalogChain) {
    let v = match chain {
        CatalogChain::Windows => 1,
        CatalogChain::Mac => 2,
        CatalogChain::Engine => 0,
    };
    ACTIVE_CHAIN.store(v, Ordering::Relaxed);
}

/// `localize(language, key, chain, args)`: the first catalog of `chain` with a value for `(key, language)`, else the
/// key; formatted with [`printf`] only when `args` is non-empty.
pub fn localize_in(chain: CatalogChain, language: UiLanguage, key: &str, args: &[Arg]) -> String {
    let format = chain.lookup(key, language.code()).unwrap_or(key);
    if args.is_empty() { format.to_owned() } else { printf(format, language, args) }
}

/// App string in an explicit language (active chain).
pub fn localize(language: UiLanguage, key: &str, args: &[Arg]) -> String {
    localize_in(active_chain(), language, key, args)
}

/// `L(key)`: app string in the current UI language ([`LanguageState::shared`]) at call time.
pub fn l(key: &str) -> String {
    localize(LanguageState::shared().language(), key, &[])
}

/// `L(key, args…)`: see also the [`l!`](crate::l) macro.
pub fn l_args(key: &str, args: &[Arg]) -> String {
    localize(LanguageState::shared().language(), key, args)
}

/// `K(key)`: identity marker for keys localized later (undo action names, provider raw names). Scanned by the
/// completeness test like `l`.
pub const fn k(key: &str) -> &str {
    key
}

/// Engine-client strings (`EngineStrings::get`): `[EngineKit]` chain only; `language` defaults to the shared state.
pub fn engine_string(key: &str, language: Option<UiLanguage>, args: &[Arg]) -> String {
    let lang = language.unwrap_or_else(|| LanguageState::shared().language());
    localize_in(CatalogChain::Engine, lang, key, args)
}

/// `l!("key")` = [`l`]; `l!("key", a, b)` = [`l_args`] with each argument converted through `Into<Arg>`.
#[macro_export]
macro_rules! l {
    ($key:expr $(,)?) => {
        $crate::l10n::l($key)
    };
    ($key:expr, $($arg:expr),+ $(,)?) => {
        $crate::l10n::l_args($key, &[$($crate::l10n::Arg::from($arg)),+])
    };
}

/// `engine_string!("key", lang_option, a, b)`.
#[macro_export]
macro_rules! engine_string {
    ($key:expr, $lang:expr $(,)?) => {
        $crate::l10n::engine_string($key, $lang, &[])
    };
    ($key:expr, $lang:expr, $($arg:expr),+ $(,)?) => {
        $crate::l10n::engine_string($key, $lang, &[$($crate::l10n::Arg::from($arg)),+])
    };
}
