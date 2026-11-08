//! String rules of the two index backends.
//!
//! * Windows (C# `StringComparer.OrdinalIgnoreCase`, win-media §0.2–§0.3): [`upper_invariant`] (exact .NET
//!   `ToUpperInvariant` of the ICU-mode app, simple uppercase mapping, `ı` unmapped; the video-cache identity and
//!   the sidecar badge), [`upper_ordinal_char`] (the OrdinalIgnoreCase casing: the same, except that `ſ` U+017F is
//!   not mapped either), [`CiKey`], [`cmp_ordinal_ignore_case`].
//! * macOS (LibraryKit, mac-librarykit §2.2–§2.4): [`nfc`], [`lower_key`], [`fold`] (`TextFolding.fold`) and
//!   [`ordered_before`] (Foundation `compare(_, options: [.caseInsensitive, .numeric])`).
//!
//! `sa_core::text::upper_invariant` is the workspace-wide version of the .NET mapping; this crate keeps its own copy
//! (same rule, verified against the .NET 10 table in `tests/data/upper-invariant.txt`) so the media index and the
//! video-cache identity never depend on code outside this crate's tests.

use std::cmp::Ordering;
use std::hash::{Hash, Hasher};

use unicode_normalization::UnicodeNormalization;

// ---------------------------------------------------------------------------------------------------------------
// .NET invariant casing
// ---------------------------------------------------------------------------------------------------------------

/// The .NET 10 `ToUpperInvariant` mapping (`source:target` hex lines; unlisted code points map to themselves),
/// generated on Windows 11 / .NET 10.0.14. Embedded rather than derived from Rust's `char::to_uppercase`: Rust follows
/// newer Unicode versions (U+019B, U+A7CD, U+10D70… gained uppercase forms that .NET 10 does not map), and the
/// mapping is part of the C# cache identity.
const DOTNET_UPPER_TABLE: &str = include_str!("../data/upper-invariant.txt");

fn dotnet_upper_table() -> &'static [(u32, u32)] {
    static TABLE: std::sync::OnceLock<Vec<(u32, u32)>> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        let mut t: Vec<(u32, u32)> = DOTNET_UPPER_TABLE
            .lines()
            .filter(|l| !l.starts_with('#'))
            .filter_map(|l| {
                let (a, b) = l.split_once(':')?;
                Some((u32::from_str_radix(a.trim(), 16).ok()?, u32::from_str_radix(b.trim(), 16).ok()?))
            })
            .collect();
        t.sort_unstable();
        t
    })
}

/// .NET `char`/`Rune.ToUpperInvariant` for one code point: the Unicode **simple** uppercase mapping of .NET 10's
/// Unicode version, except that U+0131 `ı` stays unmapped (`ß` stays, `i` → `I`, never `İ`).
pub fn upper_invariant_char(c: char) -> char {
    if c.is_ascii() {
        return c.to_ascii_uppercase();
    }
    let cp = c as u32;
    let table = dotnet_upper_table();
    match table.binary_search_by_key(&cp, |(s, _)| *s) {
        Ok(i) => char::from_u32(table[i].1).unwrap_or(c),
        Err(_) => c,
    }
}

/// .NET `string.ToUpperInvariant()` (per code point; surrogate pairs are mapped as one code point, as .NET does).
pub fn upper_invariant(s: &str) -> String {
    s.chars().map(upper_invariant_char).collect()
}

/// The casing of .NET `StringComparison.OrdinalIgnoreCase` for one code point (ICU mode, the app's default on
/// Windows): [`upper_invariant_char`], except that U+017F `ſ` is not mapped (the ordinal casing table disables both
/// `ı` and `ſ`: `ToUpperInvariant("ſ")` is `"S"`, yet `ſ` and `S` are not OrdinalIgnoreCase-equal). Verified against
/// .NET 10 for every code point.
pub fn upper_ordinal_char(c: char) -> char {
    if c == '\u{017F}' { c } else { upper_invariant_char(c) }
}

/// A string under [`upper_ordinal_char`] (the OrdinalIgnoreCase identity).
pub fn upper_ordinal(s: &str) -> String {
    s.chars().map(upper_ordinal_char).collect()
}

/// `StringComparer.OrdinalIgnoreCase.Equals`.
pub fn eq_ordinal_ignore_case(a: &str, b: &str) -> bool {
    a.chars().map(upper_ordinal_char).eq(b.chars().map(upper_ordinal_char))
}

/// `StringComparer.OrdinalIgnoreCase.Compare`: both upper-cased, then compared as UTF-16 code-unit sequences.
pub fn cmp_ordinal_ignore_case(a: &str, b: &str) -> Ordering {
    let a = upper_ordinal(a);
    let b = upper_ordinal(b);
    a.encode_utf16().cmp(b.encode_utf16())
}

/// `string.StartsWith(needle, StringComparison.OrdinalIgnoreCase)`.
pub fn starts_with_ordinal_ignore_case(haystack: &str, needle: &str) -> bool {
    upper_ordinal(haystack).starts_with(&upper_ordinal(needle))
}

/// `string.Contains(needle, StringComparison.OrdinalIgnoreCase)`.
pub fn contains_ordinal_ignore_case(haystack: &str, needle: &str) -> bool {
    upper_ordinal(haystack).contains(&upper_ordinal(needle))
}

/// A dictionary key that compares and hashes OrdinalIgnoreCase but keeps the original spelling.
#[derive(Debug, Clone)]
pub struct CiKey {
    original: String,
    upper: String,
}

impl CiKey {
    pub fn new(s: &str) -> Self {
        Self { original: s.to_owned(), upper: upper_ordinal(s) }
    }
    pub fn as_str(&self) -> &str {
        &self.original
    }
}

impl PartialEq for CiKey {
    fn eq(&self, other: &Self) -> bool {
        self.upper == other.upper
    }
}

impl Eq for CiKey {}

impl Hash for CiKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.upper.hash(state);
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Apple string identity and folding
// ---------------------------------------------------------------------------------------------------------------

/// Unicode NFC (Swift `precomposedStringWithCanonicalMapping`).
pub fn nfc(s: &str) -> String {
    s.nfc().collect()
}

/// The case-insensitive identity of an Apple string: `nfc(to_lowercase(s))` (Swift `lowercased()` is the locale-free
/// full lowercase mapping; canonical equivalence is reproduced by the NFC).
pub fn lower_key(s: &str) -> String {
    nfc(&s.to_lowercase())
}

/// LibraryKit `TextFolding.fold`: case-, diacritic- and width-insensitive form (Foundation
/// `folding([.caseInsensitive, .diacriticInsensitive, .widthInsensitive], locale: nil)`), then `ı` → `i`. The
/// macOS index search uses the workspace's one folding, so it always agrees with the library search and list-name
/// uniqueness of `sa-profiles::mac` (in Swift all three call `TextFolding.fold`). `"Işık Şarkı"` matches `"isik"`,
/// `"SARKI"` and `"ışık şarkı"`, `"Crème Brûlée"` folds to `"creme brulee"`.
pub use sa_core::folding::fold;

/// Swift `String <`: lexicographic over the Unicode scalars of the NFC forms.
pub fn swift_lt(a: &str, b: &str) -> bool {
    a.nfc().lt(b.nfc())
}

/// [`swift_lt`] as an `Ordering`.
pub fn swift_cmp(a: &str, b: &str) -> Ordering {
    a.nfc().cmp(b.nfc())
}

#[derive(Debug)]
enum Run<'a> {
    Digits(&'a str),
    Text(&'a str),
}

fn runs(s: &str) -> Vec<Run<'_>> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut digits: Option<bool> = None;
    for (i, c) in s.char_indices() {
        let is_digit = c.is_ascii_digit();
        match digits {
            Some(d) if d == is_digit => {}
            Some(d) => {
                out.push(if d { Run::Digits(&s[start..i]) } else { Run::Text(&s[start..i]) });
                start = i;
                digits = Some(is_digit);
            }
            None => digits = Some(is_digit),
        }
    }
    if let Some(d) = digits {
        out.push(if d { Run::Digits(&s[start..]) } else { Run::Text(&s[start..]) });
    }
    out
}

fn cmp_digit_runs(a: &str, b: &str) -> Ordering {
    let a = a.trim_start_matches('0');
    let b = b.trim_start_matches('0');
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

fn case_folded_scalars(s: &str) -> impl Iterator<Item = char> + '_ {
    s.nfc().flat_map(char::to_lowercase)
}

/// Foundation `compare(_, options: [.caseInsensitive, .numeric])` without a locale: alternating ASCII digit and
/// non-digit runs, digit runs compared by value, other runs case-insensitively by scalar.
pub fn compare_numeric_case_insensitive(a: &str, b: &str) -> Ordering {
    let (ra, rb) = (runs(a), runs(b));
    for (x, y) in ra.iter().zip(rb.iter()) {
        let ord = match (x, y) {
            (Run::Digits(x), Run::Digits(y)) => cmp_digit_runs(x, y),
            (Run::Text(x), Run::Text(y)) => case_folded_scalars(x).cmp(case_folded_scalars(y)),
            (Run::Digits(x), Run::Text(y)) | (Run::Text(x), Run::Digits(y)) => {
                case_folded_scalars(x).next().cmp(&case_folded_scalars(y).next())
            }
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    ra.len().cmp(&rb.len())
}

/// `MediaPath.orderedBefore`: [`compare_numeric_case_insensitive`], ties broken by [`swift_lt`].
pub fn ordered_before(a: &str, b: &str) -> bool {
    match compare_numeric_case_insensitive(a, b) {
        Ordering::Less => true,
        Ordering::Greater => false,
        Ordering::Equal => swift_lt(a, b),
    }
}

/// `ordered_before` as an `Ordering` for `sort_by` (stable).
pub fn ordered_cmp(a: &str, b: &str) -> Ordering {
    compare_numeric_case_insensitive(a, b).then_with(|| swift_cmp(a, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upper_invariant_matches_dotnet_examples() {
        assert_eq!(upper_invariant("ıißſé"), "ıIßSÉ");
        assert_eq!(upper_invariant("i"), "I");
        assert_eq!(upper_invariant("\u{1F80}"), "\u{1F88}");
        assert_eq!(upper_invariant("c:\\videos\\episode_01.mp4"), "C:\\VIDEOS\\EPISODE_01.MP4");
    }

    #[test]
    fn ordinal_ignore_case_orders_by_utf16_units() {
        assert_eq!(cmp_ordinal_ignore_case("a", "B"), Ordering::Less);
        assert_eq!(cmp_ordinal_ignore_case("abc", "ABC"), Ordering::Equal);
        // U+FF21 (fullwidth A) sorts after a supplementary-plane character in UTF-8 order but before it in UTF-16.
        assert_eq!(cmp_ordinal_ignore_case("\u{FF21}", "\u{10400}"), Ordering::Greater);
        assert!(eq_ordinal_ignore_case("Straße", "STRASSE") == false);
        assert!(eq_ordinal_ignore_case("film.MP4", "FILM.mp4"));
        // .NET 10 (ICU): `ToUpperInvariant("ſ") == "S"`, but `ſ` is not OrdinalIgnoreCase-equal to `s`/`S`, and
        // `Compare("ſ", "T") > 0` (U+017F after U+0054).
        assert_eq!(upper_invariant("ſ"), "S");
        assert!(!eq_ordinal_ignore_case("ſon", "SON"));
        assert!(eq_ordinal_ignore_case("ſon", "ſON"));
        assert_eq!(cmp_ordinal_ignore_case("ſ", "T"), Ordering::Greater);
        assert!(!contains_ordinal_ignore_case("xſy", "S"));
        assert!(contains_ordinal_ignore_case("Işık", "IŞ"));
        assert!(!contains_ordinal_ignore_case("\u{FB00}", "ff"));
        assert_ne!(CiKey::new("ſ"), CiKey::new("s"));
    }

    #[test]
    fn fold_matches_library_kit_cases() {
        assert_eq!(fold("Daha Sonra İzle"), fold("daha sonra izle"));
        assert_eq!(fold("Daha Sonra Izle"), fold("daha sonra izlé"));
        assert_eq!(fold("Kısa Filmler"), fold("KISA FILMLER"));
        assert_eq!(fold("kisa filmler"), fold("Kısa Fılmler"));
        assert_eq!(fold("KISA FİLMLER"), fold("kisa filmler"));
        assert_eq!(fold("Crème Brûlée"), "creme brulee");
        assert!(fold("Işık Şarkı").contains(&fold("isik")));
        assert!(fold("Işık Şarkı").contains(&fold("SARKI")));
        assert!(fold("Işık Şarkı").contains(&fold("ışık şarkı")));
        assert!(fold("İstanbul").contains(&fold("istanbul")));
        assert!(fold("İstanbul").contains(&fold("İSTANBUL")));
        assert_eq!(fold("Ｆｉｌｍ"), "film");
        assert_eq!(fold("Straße"), "strasse");
        // The same function as the library search and list names (`sa_core::folding::fold`).
        assert_eq!(fold("Երևան"), sa_core::folding::fold("ԵՐԵՒԱՆ"));
    }

    #[test]
    fn numeric_ordering_matches_foundation() {
        let mut files = vec!["Episode 10.mp4", "episode 2.mp4", "Episode 1.mp4", "Zeta.mp4", "alpha.mp4"];
        files.sort_by(|a, b| ordered_cmp(a, b));
        assert_eq!(files, ["alpha.mp4", "Episode 1.mp4", "episode 2.mp4", "Episode 10.mp4", "Zeta.mp4"]);
        let mut folders = vec!["b10", "B9", "a"];
        folders.sort_by(|a, b| ordered_cmp(a, b));
        assert_eq!(folders, ["a", "B9", "b10"]);
        assert!(ordered_before("/Volumes/Movies/A", "/Volumes/Movies/b"));
        assert!(ordered_before("Film", "film") || ordered_before("film", "Film"));
        let mut names = vec!["Çalışma.mov", "link.mp4", "Film.mp4"];
        names.sort_by(|a, b| ordered_cmp(a, b));
        assert_eq!(names, ["Film.mp4", "link.mp4", "Çalışma.mov"], "non-ASCII letters sort after ASCII (no locale)");
    }
}
