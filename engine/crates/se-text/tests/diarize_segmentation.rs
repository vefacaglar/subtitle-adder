//! Segmentation and speaker-dash parts of `engine/tests/test_diarize.py` (the `assign_speakers`,
//! `normalize`, pipeline and CLI parts are ported in se-diarize / se-cli).

mod common;

use common::{make_words, texts};
use se_core::json::parse;
use se_core::types::sentences_from_value;
use se_core::{ErrorKind, Sentence, Word};
use se_text::sentences::{SegmenterConfig, segment};
use se_text::subtitles::{render, speaker_dashes};

fn s(id: i64, text: &str, speaker: Option<i64>) -> Sentence {
    let mut sentence = Sentence::new(id, id as f64, id as f64 + 0.9, text);
    sentence.speaker = speaker;
    sentence
}

#[test]
fn test_sentence_splits_at_speaker_change() {
    let words = make_words("evet tamam olur peki sen nasılsın");
    let speakers = [0, 0, 0, 1, 1, 1].map(Some);
    let result = segment(&words, &SegmenterConfig::default(), Some(&speakers)).unwrap();
    let got: Vec<_> = result.iter().map(|x| (x.text.as_str(), x.speaker)).collect();
    assert_eq!(got, [("evet tamam olur", Some(0)), ("peki sen nasılsın", Some(1))]);
    assert_eq!(result.iter().map(|x| x.id).collect::<Vec<_>>(), [1, 2]);
}

#[test]
fn test_no_speakers_keeps_old_behaviour() {
    let result = segment(&make_words("Merhaba dünya. Nasılsın?"), &SegmenterConfig::default(), None).unwrap();
    assert_eq!(result.iter().map(|x| x.speaker).collect::<Vec<_>>(), [None, None]);
}

#[test]
#[serial_test::serial]
fn test_speaker_list_length_mismatch_is_rejected() {
    se_core::i18n::set_language(Some("en"));
    let err = segment(&make_words("a b"), &SegmenterConfig::default(), Some(&[Some(0)])).unwrap_err();
    assert!(err.is(ErrorKind::Value));
    assert_eq!(err.message, "The speaker list must have the same length as the word list.");
    se_core::i18n::set_language(None);
    // Empty words are checked before the length.
    assert!(segment(&[], &SegmenterConfig::default(), Some(&[Some(0)])).unwrap().is_empty());
}

#[test]
fn test_dash_rule_two_speakers() {
    let sentences = [s(1, "Selam.", Some(0)), s(2, "Nasılsın?", Some(0)), s(3, "İyiyim.", Some(1)), s(4, "Sen?", Some(0))];
    assert_eq!(speaker_dashes(&sentences), [true, false, true, true]);
    let srt = render(&sentences, "srt").unwrap();
    for needle in ["\n- Selam.\n", "\nNasılsın?\n", "\n- İyiyim.\n", "\n- Sen?\n"] {
        assert!(srt.contains(needle), "{needle:?} in {srt:?}");
    }
    let vtt = render(&sentences, "vtt").unwrap();
    for needle in ["\n- Selam.\n", "\nNasılsın?\n", "\n- Sen?\n"] {
        assert!(vtt.contains(needle), "{needle:?} in {vtt:?}");
    }
}

#[test]
fn test_no_dashes_for_single_or_null_speaker() {
    for speakers in [[Some(0), Some(0), Some(0)], [None, None, None]] {
        let sentences: Vec<_> =
            speakers.iter().enumerate().map(|(i, &sp)| s(i as i64 + 1, &format!("Cümle {}.", i + 1), sp)).collect();
        assert!(!speaker_dashes(&sentences).iter().any(|&d| d));
        assert!(!render(&sentences, "srt").unwrap().contains("- "));
    }
}

#[test]
fn test_json_keeps_speaker_field() {
    let data = parse(&render(&[s(1, "A.", Some(1)), s(2, "B.", None)], "json").unwrap()).unwrap();
    let back = sentences_from_value(&data).unwrap();
    assert_eq!(back.iter().map(|x| x.speaker).collect::<Vec<_>>(), [Some(1), None]);
    let minimal = Sentence::from_value(&parse(r#"{"id": 1, "start": 0, "end": 1, "text": "x"}"#).unwrap()).unwrap();
    assert_eq!(minimal.speaker, None);
}

#[test]
fn test_render_reads_speaker_from_stdin_payload() {
    // The library half of `test_render_command_reads_speaker_from_stdin` (the CLI half is in se-cli).
    let payload = r#"[{"id": 1, "start": 0.0, "end": 1.0, "text": "Selam.", "words": [], "speaker": 0}, {"id": 2, "start": 1.0, "end": 2.0, "text": "Merhaba.", "words": [], "speaker": 1}]"#;
    let out = render(&sentences_from_value(&parse(payload).unwrap()).unwrap(), "srt").unwrap();
    assert!(out.contains("- Selam.") && out.contains("- Merhaba."));
}

fn tw(text: &str, start: f64, end: f64) -> Word {
    Word::new(start, end, text)
}

fn dersin_words() -> Vec<Word> {
    vec![
        tw("Bu", 10.70, 10.95), tw("harika", 10.95, 11.4), tw("bir", 11.4, 11.6), tw("haber,", 11.6, 12.1),
        tw("ikinci", 12.1, 12.6), tw("bölüm", 12.6, 13.0), tw("ne", 13.0, 13.2), tw("zaman", 13.2, 13.6),
        tw("hazır", 13.6, 13.9), tw("olur", 13.9, 14.22), tw("dersin?", 14.22, 14.80),
        tw("Bence", 15.08, 15.4), tw("yaklaşık", 15.4, 15.9), tw("iki", 15.9, 16.1), tw("hafta.", 16.1, 16.5),
    ]
}

#[test]
fn test_forward_snap_returns_sentence_final_word_to_previous_speaker() {
    // `assign_speakers` gives [0]*11 + [1]*4 (tested in se-diarize); the segmentation half uses it directly.
    let speakers: Vec<Option<i64>> = [vec![Some(0); 11], vec![Some(1); 4]].concat();
    let result = segment(&dersin_words(), &SegmenterConfig::default(), Some(&speakers)).unwrap();
    let first = &result[0];
    assert_eq!((first.text.split_whitespace().last().unwrap(), first.speaker), ("dersin?", Some(0)));
    assert_eq!(texts(&result), ["Bu harika bir haber, ikinci bölüm ne zaman hazır olur dersin?", "Bence yaklaşık iki hafta."]);
}

#[test]
fn test_dash_carries_previous_speaker_across_null_sentences() {
    let sentences = [s(1, "A.", Some(0)), s(2, "B.", None), s(3, "C.", Some(0)), s(4, "D.", Some(1))];
    assert_eq!(speaker_dashes(&sentences), [true, false, false, true]);
}

#[test]
fn test_dash_not_doubled_when_text_already_has_one() {
    let sentences = [s(1, "- Selam.", Some(0)), s(2, "– Merhaba.", Some(1)), s(3, "-Kısa.", Some(0))];
    let srt = render(&sentences, "srt").unwrap();
    assert!(!srt.contains("- - ") && !srt.contains("- –") && srt.contains("\n- Selam.\n"));
    assert!(srt.contains("\n– Merhaba.\n") && srt.contains("\n- -Kısa.\n"));
}
