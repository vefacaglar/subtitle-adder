//! Translation output validators (spec translate-llm.md §4.4-§4.8, Python-Unicode semantics).

use std::collections::HashSet;

use se_core::pyfmt;

use crate::pyunicode;

/// `_REFERENCE_PAIR_RE.split(text)`: split at `=>`, `->`, `→` (U+2192) or `⇒` (U+21D2), leftmost first.
fn split_reference_pair(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut last = 0;
    let mut i = 0;
    let bytes = text.as_bytes();
    while i < text.len() {
        let rest = &text[i..];
        let len = if rest.starts_with("=>") || rest.starts_with("->") {
            2
        } else if rest.starts_with('→') || rest.starts_with('⇒') {
            3
        } else {
            0
        };
        if len > 0 {
            parts.push(&text[last..i]);
            i += len;
            last = i;
        } else {
            // advance one char
            let mut step = 1;
            while i + step < bytes.len() && !text.is_char_boundary(i + step) {
                step += 1;
            }
            i += step;
        }
    }
    parts.push(&text[last..]);
    parts
}

/// `_REFERENCE_PAIR_RE.search(text)`.
fn has_reference_arrow(text: &str) -> bool {
    text.contains("=>") || text.contains("->") || text.contains('→') || text.contains('⇒')
}

/// `_INLINE_MARKUP_RE.search(text)`: `(?<!\w)(\*{1,2}|_{1,2}|`)(?=\S)(.+?)(?<=\S)\1(?!\w)`.
pub fn has_inline_markup(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let not_word_at = |i: usize| i >= n || !pyunicode::is_word(chars[i]);
    for p in 0..n {
        if p > 0 && pyunicode::is_word(chars[p - 1]) {
            continue;
        }
        let c = chars[p];
        // Alternatives in order: \*{1,2} (greedy: ** then *), _{1,2} (__ then _), `
        let delims: Vec<usize> = match c {
            '*' | '_' => {
                if p + 1 < n && chars[p + 1] == c { vec![2, 1] } else { vec![1] }
            }
            '`' => vec![1],
            _ => continue,
        };
        for dlen in delims {
            let start = p + dlen;
            // (?=\S)
            if start >= n || pyfmt::isspace(chars[start]) {
                continue;
            }
            // (.+?) lazy: q is the end of group 2 (exclusive), q > start; no '\n' inside.
            let mut q = start + 1;
            while q <= n {
                if chars[q - 1] == '\n' {
                    break;
                }
                // (?<=\S) then \1 then (?!\w)
                if !pyfmt::isspace(chars[q - 1])
                    && q + dlen <= n
                    && chars[q..q + dlen].iter().all(|&d| d == c)
                    && not_word_at(q + dlen)
                {
                    return true;
                }
                q += 1;
            }
        }
    }
    false
}

/// `_has_unrequested_markup(source_text, translated_text)`.
pub fn has_unrequested_markup(source_text: &str, translated_text: &str) -> bool {
    has_inline_markup(translated_text) && !has_inline_markup(source_text)
}

/// `_clean_translation_pair(source_text, translated_text)`.
pub fn clean_translation_pair(source_text: &str, translated_text: &str) -> String {
    if has_reference_arrow(source_text) {
        return pyfmt::strip(translated_text).to_string();
    }
    let parts = split_reference_pair(translated_text);
    let source = pyunicode::alnum_casefold(source_text);
    if parts.len() == 2 && !source.is_empty() {
        for i in 0..2 {
            if pyunicode::alnum_casefold(parts[i]) == source && !pyfmt::strip(parts[1 - i]).is_empty() {
                return pyfmt::strip(parts[1 - i]).to_string();
            }
        }
    }
    pyfmt::strip(translated_text).to_string()
}

/// `_has_source_translation_pair(source_text, translated_text)`.
pub fn has_source_translation_pair(source_text: &str, translated_text: &str) -> bool {
    if has_reference_arrow(source_text) {
        return false;
    }
    let source = pyunicode::alnum_casefold(source_text);
    if source.is_empty() {
        return false;
    }
    let parts = split_reference_pair(translated_text);
    parts.len() > 1
        && parts.iter().enumerate().any(|(i, part)| {
            pyunicode::alnum_casefold(part) == source
                && parts.iter().enumerate().any(|(j, other)| j != i && !pyfmt::strip(other).is_empty())
        })
}

fn first_char(w: &str) -> char {
    w.chars().next().unwrap_or(' ')
}

/// `_looks_untranslated(source_text, translated_text, source, target)`.
pub fn looks_untranslated(source_text: &str, translated_text: &str, source: Option<&str>, target: Option<&str>) -> bool {
    if has_source_translation_pair(source_text, translated_text) {
        return true;
    }
    if let (Some(s), Some(t)) = (source, target) {
        if !s.is_empty() && !t.is_empty() && s == t {
            return false;
        }
    }
    let words = pyunicode::findall_letter_runs(source_text);
    if words.len() < 2 || words.iter().all(|w| pyunicode::isupper(first_char(w))) {
        return false;
    }
    let src_alnum = pyunicode::alnum_lower(source_text);
    let dst_alnum = pyunicode::alnum_lower(translated_text);
    if src_alnum.is_empty() || dst_alnum.is_empty() {
        return false;
    }
    if src_alnum.chars().count() >= 4 && src_alnum == dst_alnum && !pyunicode::str_isdigit(&src_alnum) {
        return true;
    }
    let src_words: HashSet<String> = words
        .iter()
        .filter(|w| w.chars().count() >= 3 && pyunicode::islower(first_char(w)))
        .map(|w| pyunicode::casefold(w))
        .collect();
    let dst_words: Vec<String> = pyunicode::findall_letter_runs(translated_text)
        .into_iter()
        .filter(|w| w.chars().count() >= 3 && pyunicode::islower(first_char(w)))
        .map(pyunicode::casefold)
        .collect();
    if src_words.len() >= 3 && dst_words.len() >= 3 {
        let overlap = dst_words.iter().filter(|w| src_words.contains(*w)).count();
        if overlap as f64 / dst_words.len() as f64 >= 0.75 {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = "eu não consigo abrir minha boca daquele jeito";
    const TARGET: &str = "ben o şekilde ağzımı açamıyorum";
    const ARROWS: [&str; 4] = ["=>", "->", "→", "⇒"];

    #[test]
    fn test_bilingual_pair_is_detected_even_when_target_text_dilutes_source_overlap() {
        for arrow in ARROWS {
            for reverse in [false, true] {
                let parts = if reverse { [TARGET, SOURCE] } else { [SOURCE, TARGET] };
                let output = parts.join(&format!(" {arrow} "));
                assert!(looks_untranslated(SOURCE, &output, Some("pt"), Some("tr")), "{output}");
            }
        }
    }

    #[test]
    fn test_pair_detection_ignores_case_quotes_and_punctuation() {
        assert!(has_source_translation_pair(SOURCE, &format!("\"{}.\" => {TARGET}", SOURCE.to_uppercase())));
    }

    #[test]
    fn test_unambiguous_pair_is_cleaned_in_either_order() {
        for arrow in ARROWS {
            for reverse in [false, true] {
                let parts = if reverse { [TARGET, SOURCE] } else { [SOURCE, TARGET] };
                assert_eq!(clean_translation_pair(SOURCE, &parts.join(&format!(" {arrow} "))), TARGET);
            }
        }
    }

    #[test]
    fn test_literal_or_ambiguous_arrows_are_not_cut() {
        assert_eq!(clean_translation_pair("A => B", "A => B"), "A => B");
        let ambiguous = format!("{SOURCE} => {TARGET} => extra");
        assert_eq!(clean_translation_pair(SOURCE, &ambiguous), ambiguous);
    }

    #[test]
    fn test_real_dialogue_and_literal_arrows_are_not_rejected() {
        for (source, output) in [
            (SOURCE, TARGET),
            ("The sign says A => B.", "Tabelada A => B yazıyor."),
            ("A -> B", "A → B"),
            ("Alice!", "Alice!"),
            ("2026", "2026"),
            ("Use the arrow symbol.", "=> simgesini kullan."),
        ] {
            assert!(!has_source_translation_pair(source, output), "{source:?}");
        }
    }

    #[test]
    fn test_shared_names_do_not_trigger_an_untranslated_false_positive() {
        assert!(!looks_untranslated("Alice, Bob and Charlie.", "Alice, Bob ve Charlie.", Some("en"), Some("tr")));
        assert!(!looks_untranslated("Where are you going?", "Where are you going?", Some("en"), Some("en")));
        assert!(looks_untranslated("Where are you going?", "Where are you going?", Some("en"), Some("tr")));
    }

    #[test]
    fn test_markup_detection() {
        let source = "Sua língua, véia, horrorosa.";
        for t in ["*Kafan* *kötü* *kadın* *kıskanç* *yapıyor*.", "**Merhaba.**", "_Merhaba._", "`Merhaba.`"] {
            assert!(has_unrequested_markup(source, t), "{t}");
        }
        for (s, t) in [
            ("*Hello.*", "*Merhaba.*"),
            ("Use user_name.", "user_name kullan."),
            ("Two times three.", "2 * 3"),
            ("A censored word.", "s*ktir"),
            ("Hello.", "Merhaba."),
        ] {
            assert!(!has_unrequested_markup(s, t), "{t}");
        }
    }
}
