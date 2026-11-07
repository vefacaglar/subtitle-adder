//! Port of `engine/tests/test_sentences.py`.

mod common;

use common::{make_words, make_words_step, texts};
use se_core::Word;
use se_text::sentences::{SegmenterConfig, fit, segment};

fn cfg(pause_gap: f64, max_chars: i64, max_duration: f64) -> SegmenterConfig {
    SegmenterConfig { pause_gap, max_chars, max_duration }
}

fn seg(words: &[Word]) -> Vec<se_core::Sentence> {
    segment(words, &SegmenterConfig::default(), None).unwrap()
}

#[test]
fn test_splits_on_sentence_end_punctuation() {
    let result = seg(&make_words("Merhaba dünya. Nasılsın? İyiyim!"));
    assert_eq!(texts(&result), ["Merhaba dünya.", "Nasılsın?", "İyiyim!"]);
    assert_eq!(result.iter().map(|s| s.id).collect::<Vec<_>>(), [1, 2, 3]);
}

#[test]
fn test_splits_on_long_pause() {
    let words = make_words("bir iki üç dört");
    let mut shifted = words[..2].to_vec();
    shifted.extend(words[2..].iter().map(|w| Word::new(w.start_f() + 3.0, w.end_f() + 3.0, w.text.clone())));
    assert_eq!(texts(&seg(&shifted)), ["bir iki", "üç dört"]);
}

#[test]
fn test_long_sentence_prefers_comma_break() {
    let text = "Bugün sabah erkenden kalktım, kahvaltı yaptım ve sonra uzun bir yürüyüşe çıktım.";
    let result = segment(&make_words(text), &cfg(1.2, 50, 100.0), None).unwrap();
    assert_eq!(result[0].text, "Bugün sabah erkenden kalktım,");
    assert!(result.iter().all(|s| s.text.chars().count() <= 50));
    assert_eq!(texts(&result).join(" "), text);
    assert_eq!(texts(&result), ["Bugün sabah erkenden kalktım,", "kahvaltı yaptım ve sonra uzun bir yürüyüşe çıktım."]);
}

#[test]
fn test_long_sentence_splits_on_duration() {
    let result = segment(&make_words_step("a b c d e f g h i j", 0.5), &cfg(1.2, 1000, 2.0), None).unwrap();
    assert!(result.len() > 1);
    assert!(result.iter().all(|s| s.end_f() - s.start_f() <= 2.0));
    let spans: Vec<_> = result.iter().map(|s| (s.text.as_str(), s.start_f(), s.end_f())).collect();
    assert_eq!(spans, [("a b c d", 0.0, 1.9), ("e f g h", 2.0, 3.9), ("i j", 4.0, 4.9)]);
}

#[test]
fn test_times_come_from_words() {
    let words = make_words("Selam millet.");
    let sentence = &seg(&words)[0];
    assert_eq!((sentence.start, sentence.end), (words[0].start, words[words.len() - 1].end));
}

#[test]
fn test_empty_input() {
    assert!(seg(&[]).is_empty());
}

#[test]
fn test_very_long_unpunctuated_sentence_does_not_recurse() {
    let words = make_words_step(&vec!["kelime"; 20_000].join(" "), 0.1);
    let result = segment(&words, &cfg(100.0, 40, 100.0), None).unwrap();
    assert!(result.len() > 1000);
    assert_eq!(result.iter().map(|s| s.words.len()).sum::<usize>(), 20_000);
}

#[test]
fn test_sentence_end_before_closing_quote_or_bracket() {
    let result = seg(&make_words("Dedi ki \"Evet.\" Sonra (gitti!) Bitti"));
    assert_eq!(texts(&result), ["Dedi ki \"Evet.\"", "Sonra (gitti!)", "Bitti"]);
}

#[test]
fn test_soft_break_before_closing_quote() {
    let text = "Bugün sabah \"erkenden kalktım,\" kahvaltı yaptım ve sonra uzun bir yürüyüşe çıktım.";
    let result = segment(&make_words(text), &cfg(1.2, 50, 100.0), None).unwrap();
    assert_eq!(result[0].text, "Bugün sabah \"erkenden kalktım,\"");
}

#[test]
fn test_realigns_trailing_starters_and_mid_sentence_pauses() {
    let words = [
        (10.0, 10.4, "Bon"),
        (10.5, 10.8, "Je"),
        (12.8, 13.2, "suis"),
        (13.3, 13.9, "désolée"),
        (14.1, 14.3, "Tu"),
        (14.4, 14.7, "en"),
        (17.2, 17.5, "penses"),
        (17.6, 17.8, "quoi"),
        (17.9, 18.1, "alors"),
        (18.2, 18.3, "de"),
        (18.4, 18.6, "celui"),
        (18.6, 18.7, "-ci"),
        (18.9, 19.1, "Tu"),
        (22.6, 22.8, "es"),
        (26.3, 26.8, "ridicule"),
        (27.0, 27.1, "T"),
        (27.1, 27.3, "'es"),
        (28.9, 29.4, "moche"),
    ]
    .map(|(s, e, t)| Word::new(s, e, t));
    let result = segment(&words, &cfg(1.2, 84, 10.0), None).unwrap();
    assert_eq!(
        texts(&result),
        ["Bon", "Je suis désolée", "Tu en penses quoi alors de celui-ci", "Tu es ridicule", "T'es moche"]
    );
}

// --- fit(): where a too-long, comma-less line is cut ---------------------------------------------------------

const FIT: SegmenterConfig = SegmenterConfig { pause_gap: 100.0, max_chars: 40, max_duration: 100.0 };

fn fit_lines_with(text: &str, config: &SegmenterConfig) -> Vec<String> {
    let lines: Vec<String> = fit(&make_words(text), config)
        .iter()
        .map(|part| part.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" "))
        .collect();
    // However it is cut, nothing is lost or reordered and every line is within the limits.
    assert_eq!(lines.join(" "), text);
    assert!(lines.iter().all(|l| l.chars().count() as i64 <= config.max_chars));
    lines
}

fn fit_lines(text: &str) -> Vec<String> {
    fit_lines_with(text, &FIT)
}

fn word_counts(lines: &[String]) -> Vec<usize> {
    lines.iter().map(|l| l.split_whitespace().count()).collect()
}

#[test]
fn test_fit_cuts_before_a_capitalized_word_when_there_is_no_comma() {
    let lines = fit_lines("ben dün akşam eve geç geldim çünkü Ahmet ile konuştum ve sonra uyudum");
    assert_eq!(lines, ["ben dün akşam eve geç geldim çünkü", "Ahmet ile konuştum ve sonra uyudum"]);
}

#[test]
fn test_fit_takes_the_last_capitalized_word_in_the_window() {
    let lines = fit_lines("ben dün Ali ile geç gittik Ece ile eve döndük ve çok yorulduk");
    assert_eq!(lines[0], "ben dün Ali ile geç gittik");
    assert!(lines[1].starts_with("Ece "));
}

#[test]
fn test_fit_ignores_a_capitalized_word_in_the_first_two_positions() {
    let lines = fit_lines_with(
        "bugün Ahmet eve gelmedi çünkü yağmur yağıyordu ve hava çok soğuktu",
        &cfg(100.0, 30, 100.0),
    );
    assert_eq!(lines[0], "bugün Ahmet eve gelmedi çünkü");
}

#[test]
fn test_fit_ignores_a_capitalized_word_before_the_start_of_the_window() {
    // 20 one-letter words fit, so the window opens at index 6.
    assert_eq!(word_counts(&fit_lines("a b C d e f g h i j k l m n o p q r s t u v w")), [20, 3]);
    assert_eq!(word_counts(&fit_lines("a b c d e f G h i j k l m n o p q r s t u v w")), [6, 17]);
    assert_eq!(word_counts(&fit_lines("a b c d e Ff g h i j k l m n o p q r s t u v w")), [20, 3]);
}

#[test]
fn test_fit_without_any_clause_start_cuts_at_the_hard_limit() {
    let text = format!("{}kelime", "kelime ".repeat(11));
    let lines = fit_lines(&text);
    assert_eq!(lines[0], "kelime kelime kelime kelime kelime");
    assert_eq!(lines.len(), 3);
}

#[test]
fn test_fit_prefers_a_comma_in_the_second_half_over_a_capitalized_word() {
    // The comma ends word 12 (>= 20 // 2); the capitalized word at index 16 would cut later.
    assert_eq!(word_counts(&fit_lines("a b c d e f g h i j k l, m n o p Q r s t u v w")), [12, 11]);
}

#[test]
fn test_fit_falls_back_to_a_capitalized_word_when_the_comma_is_too_early() {
    // The comma ends word 4 (< 10), so it is not worth a short first line.
    assert_eq!(word_counts(&fit_lines("a b c, d e f g h i j k l m n o p Q r s t u v w")), [16, 7]);
}

#[test]
fn test_fit_leaves_a_short_sentence_alone() {
    let words = make_words("Kısa bir cümle");
    assert_eq!(fit(&words, &FIT), vec![words]);
}

// --- extra goldens verified against the Python engine (spec/sentences.md) --------------------------------

#[test]
fn test_short_word_protection_and_long_gaps() {
    let w = |s, e, t| Word::new(s, e, t);
    assert_eq!(texts(&seg(&[w(0.0, 0.3, "Je"), w(4.0, 4.3, "suis")])), ["Je suis"]);
    assert_eq!(texts(&seg(&[w(0.0, 0.3, "Je"), w(4.3, 4.6, "Suis")])), ["Je Suis"]);
    assert_eq!(
        texts(&seg(&[w(0.0, 0.3, "ok"), w(0.4, 0.6, "Je"), w(3.0, 3.3, "suis"), w(3.4, 3.6, "là.")])),
        ["ok", "Je suis là."]
    );
    assert_eq!(texts(&seg(&[w(0.0, 0.3, "Hi"), w(3.0, 3.3, "there"), w(10.0, 10.3, "ok")])), ["Hi there", "ok"]);
}

#[test]
fn test_whitespace_words_are_dropped_and_unstripped_text_is_kept() {
    let w = |s, e, t| Word::new(s, e, t);
    assert_eq!(texts(&seg(&[w(0.0, 0.3, "Evet. "), w(0.4, 0.6, "iyi")])), ["Evet.  iyi"]);
    assert_eq!(texts(&seg(&[w(0.0, 0.3, " "), w(0.4, 0.6, "iyi")])), ["iyi"]);
    assert!(seg(&[w(0.0, 0.3, " "), w(0.4, 0.6, "  ")]).is_empty());
}

#[test]
fn test_single_overlong_word_is_its_own_line() {
    let long = "x".repeat(100);
    let result = segment(&[Word::new(0.0, 1.0, long.clone()), Word::new(1.0, 2.0, "y")], &cfg(1.2, 10, 6.0), None)
        .unwrap();
    assert_eq!(texts(&result), [long.as_str(), "y"]);
}

#[test]
fn test_clitics_are_not_merged_across_speakers() {
    let result = segment(&make_words("c 'est bien. oui"), &SegmenterConfig::default(), Some(&[Some(0), Some(1), Some(1), Some(1)]))
        .unwrap();
    let got: Vec<_> = result.iter().map(|s| (s.text.as_str(), s.speaker, s.id)).collect();
    assert_eq!(got, [("c", Some(0), 1), ("'est bien.", Some(1), 2), ("oui", Some(1), 3)]);
}
