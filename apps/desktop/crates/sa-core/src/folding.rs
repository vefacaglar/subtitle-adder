//! `TextFolding::fold` of the macOS app (mac-librarykit §2.2): the one case-, diacritic- and width-insensitive
//! folding used by the macOS library search, list-name uniqueness (`sa-profiles::mac`) and the macOS media-index
//! search (`sa-media::mac`). All three must call this function so they agree on what is "the same".
//!
//! Swift: `text.folding([.caseInsensitive, .diacriticInsensitive, .widthInsensitive], locale: nil)` then every
//! `ı` (U+0131) → `i`. Locale-free recipe:
//! 1. NFD; 2. Halfwidth/Fullwidth Forms (U+FF00–U+FFEF) through their NFKC mapping; 3. full Unicode case folding;
//! 4. remove combining marks (strips the U+0307 that `İ` produces and every accent); 5. `ı` → `i`; 6. NFC.
//! Apple's exact table for exotic code points is not reproducible; the tested cases (Turkish, Latin accents,
//! ligatures, fullwidth Latin) agree.

use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

/// Folds `text` for search and identity comparisons (see the module docs).
pub fn fold(text: &str) -> String {
    // 1–2. Canonical decomposition, width folding (then decompose again: NFKC output is composed).
    let widened: String = text
        .nfd()
        .flat_map(|c| {
            if ('\u{FF00}'..='\u{FFEF}').contains(&c) {
                let mapped: Vec<char> = c.to_string().nfkc().collect();
                mapped.into_iter()
            } else {
                vec![c].into_iter()
            }
        })
        .collect();
    // 3. Full case folding, decomposed again so a folded precomposed letter loses its marks too.
    let mut folded = String::with_capacity(widened.len());
    for c in widened.nfd() {
        push_case_fold(c, &mut folded);
    }
    // 4–6. Strip marks, dotless i → i, recompose.
    folded.nfd().filter(|c| !is_combining_mark(*c)).map(|c| if c == '\u{131}' { 'i' } else { c }).nfc().collect()
}

/// Unicode full case folding (CaseFolding.txt statuses C + F) for one character: the special mappings that differ
/// from default lowercasing, else `char::to_lowercase`.
fn push_case_fold(c: char, out: &mut String) {
    let special: Option<&str> = match c {
        'ß' | '\u{1E9E}' => Some("ss"),
        'ſ' => Some("s"),
        'ς' => Some("σ"),
        'ϐ' => Some("β"),
        'ϑ' => Some("θ"),
        'ϕ' => Some("φ"),
        'ϖ' => Some("π"),
        'ϰ' => Some("κ"),
        'ϱ' => Some("ρ"),
        'ϵ' => Some("ε"),
        '\u{0345}' | '\u{1FBE}' => Some("ι"),
        'ẛ' => Some("ṡ"),
        'ŉ' => Some("ʼn"),
        'և' => Some("եւ"),
        'ﬀ' => Some("ff"),
        'ﬁ' => Some("fi"),
        'ﬂ' => Some("fl"),
        'ﬃ' => Some("ffi"),
        'ﬄ' => Some("ffl"),
        'ﬅ' | 'ﬆ' => Some("st"),
        'ﬓ' => Some("մն"),
        'ﬔ' => Some("մե"),
        'ﬕ' => Some("մի"),
        'ﬖ' => Some("վն"),
        'ﬗ' => Some("մխ"),
        _ => None,
    };
    match special {
        Some(s) => out.push_str(s),
        None => out.extend(c.to_lowercase()),
    }
}

#[cfg(test)]
mod tests {
    use super::fold;

    #[test]
    fn turkish_list_names_fold_together() {
        let a = fold("Daha Sonra İzle");
        for other in ["daha sonra izle", "Daha Sonra Izle", "daha sonra izlé", "DAHA SONRA İZLE"] {
            assert_eq!(fold(other), a, "{other}");
        }
        let k = fold("Kısa Filmler");
        for other in ["KISA FILMLER", "kisa filmler", "Kısa Fılmler", "KISA FİLMLER"] {
            assert_eq!(fold(other), k, "{other}");
        }
    }

    #[test]
    fn accents_ligatures_and_width() {
        assert_eq!(fold("Crème Brûlée"), "creme brulee");
        assert!(fold("Işık Şarkı").contains(&fold("isik")));
        assert!(fold("Işık Şarkı").contains(&fold("SARKI")));
        assert!(fold("Işık Şarkı").contains(&fold("ışık şarkı")));
        assert_eq!(fold("İstanbul"), fold("istanbul"));
        assert_eq!(fold("İSTANBUL"), fold("istanbul"));
        assert_eq!(fold("Ｆｕｌｌ"), "full");
        assert_eq!(fold("ﬁ"), "fi");
        assert_eq!(fold("Straße"), "strasse");
        // NFC and NFD inputs fold alike.
        assert_eq!(fold("e\u{301}"), fold("\u{e9}"));
    }
}
