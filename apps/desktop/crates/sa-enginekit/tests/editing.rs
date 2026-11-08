//! Sentence-list editing, dialogue dashes, snapshot merging, review flags and the sentence JSON wire format
//! (C# SentenceEditingTests / SpeakerTests / DialogueParityTests / TranslationReviewTests, Swift
//! SentenceEditingTests / SnapshotMergeTests / SpeakerTests / TranslationReviewTests).

use std::collections::HashSet;

use sa_core::engine_types::{Sentence, Word};
use sa_enginekit::editing::{DialogueDash, MINIMUM_DURATION, NUDGE_STEP, SentenceListExt};
use uuid::Uuid;

fn s(id: i64, start: f64, end: f64, text: &str) -> Sentence {
    Sentence::new(id, start, end, text)
}

fn with_words(mut sentence: Sentence, words: Vec<Word>) -> Sentence {
    sentence.words = words;
    sentence
}

fn spk(mut sentence: Sentence, speaker: Option<i64>) -> Sentence {
    sentence.speaker = speaker;
    sentence
}

fn sample() -> Vec<Sentence> {
    vec![
        with_words(s(1, 0.5, 2.0, "Merhaba."), vec![Word::new(0.5, 2.0, "Merhaba.")]),
        s(2, 2.5, 4.0, "Nasılsın?"),
        s(3, 4.0, 6.0, "İyiyim."),
    ]
}

fn ids(list: &[Sentence]) -> Vec<i64> {
    list.iter().map(|x| x.id).collect()
}

#[test]
fn active_index_finds_containing_sentence() {
    let l = sample();
    assert_eq!(l.active_index(0.2), None);
    assert_eq!(l.active_index(0.5), Some(0));
    assert_eq!(l.active_index(1.99), Some(0));
    assert_eq!(l.active_index(2.2), None);
    assert_eq!(l.active_index(4.0), Some(2));
    assert_eq!(l.active_index(6.0), None);
}

#[test]
fn index_of_uid_finds_sentence() {
    let l = sample();
    assert_eq!(l.index_of_uid(l[1].uid), Some(1));
    assert_eq!(l.index_of_uid(Uuid::new_v4()), None);
}

#[test]
fn nudge_moves_by_step_without_float_noise() {
    let mut l = sample();
    assert!(l.nudge_start(0, NUDGE_STEP));
    assert_eq!(l[0].start, 0.6);
    assert!(l.nudge_end(0, -NUDGE_STEP));
    assert_eq!(l[0].end, 1.9);
}

#[test]
fn start_clamps_to_zero_and_previous_end() {
    let mut l = sample();
    l.set_start(-3.0, 0);
    assert_eq!(l[0].start, 0.0);
    l.set_start(0.0, 1);
    assert_eq!(l[1].start, 2.0);
    assert!(!l.set_start(0.0, 1));
}

#[test]
fn start_keeps_minimum_duration() {
    let mut l = sample();
    l.set_start(10.0, 0);
    assert!((l[0].start - (2.0 - MINIMUM_DURATION)).abs() < 1e-9);
    assert!(l[0].start < l[0].end);
}

#[test]
fn end_clamps_to_next_start_and_minimum_duration() {
    let mut l = sample();
    l.set_end(9.0, 0);
    assert_eq!(l[0].end, 2.5);
    l.set_end(0.0, 1);
    assert!((l[1].end - (2.5 + MINIMUM_DURATION)).abs() < 1e-9);
    l.set_end(100.0, 2);
    assert_eq!(l[2].end, 100.0);
}

#[test]
fn invalid_index_is_ignored() {
    let mut l = sample();
    assert!(!l.nudge_start(5, 0.1));
    assert!(!l.nudge_end(5, 0.1));
    assert!(!l.set_start(1.0, 5));
    assert!(!l.set_end(1.0, 5));
    assert!(!l.set_text("x", 5));
    assert!(!l.set_speaker(Some(1), 5));
    assert!(!l.merge_with_next(2));
    assert!(!l.delete_sentence(9));
    assert!(!l.split_sentence(9, 1.0));
    assert!(!l.can_split(9, 1.0));
    assert_eq!(l, sample());
}

#[test]
fn merge_joins_text_times_and_words_then_renumbers() {
    let mut l = sample();
    let first = l[0].uid;
    l[0].needs_review = true;
    assert!(l.merge_with_next(0));
    assert_eq!(l.len(), 2);
    assert_eq!(l[0].text, "Merhaba. Nasılsın?");
    assert_eq!((l[0].start, l[0].end), (0.5, 4.0));
    assert_eq!(l[0].words.len(), 1);
    assert_eq!(l[0].uid, first);
    assert!(!l[0].needs_review);
    assert_eq!(ids(&l), [1, 2]);
    assert_eq!(l[1].text, "İyiyim.");
}

#[test]
fn merge_drops_blank_texts() {
    let mut l = vec![s(1, 0.0, 1.0, "  "), s(2, 1.0, 2.0, " İkinci ")];
    assert!(l.merge_with_next(0));
    assert_eq!(l[0].text, "İkinci");
}

#[test]
fn delete_removes_and_renumbers() {
    let mut l = sample();
    assert!(l.delete_sentence(0));
    assert_eq!(l.iter().map(|x| x.text.as_str()).collect::<Vec<_>>(), ["Nasılsın?", "İyiyim."]);
    assert_eq!(ids(&l), [1, 2]);
}

#[test]
fn set_text_reports_change() {
    let mut l = sample();
    assert!(!l.set_text("Merhaba.", 0));
    assert!(l.set_text("Selam.", 0));
    assert_eq!(l[0].text, "Selam.");
}

#[test]
fn a_text_edit_clears_the_review_flag_on_mac_and_keeps_it_on_windows() {
    let mut l = sample();
    l[0].needs_review = true;
    assert!(!l.set_text("Merhaba.", 0));
    assert!(l[0].needs_review, "the same text keeps it");
    assert!(l.set_text("Selam.", 0));
    assert!(!l[0].needs_review);
    l[1].needs_review = true;
    assert!(l.set_text_keeping_review("Naber?", 1));
    assert!(l[1].needs_review);
    assert!(!l.set_text_keeping_review("Naber?", 1));
}

#[test]
fn timing_and_speaker_edits_keep_the_review_flag() {
    let mut l = sample();
    l[2].needs_review = true;
    assert!(l.nudge_end(2, NUDGE_STEP));
    assert!(l.set_speaker(Some(1), 2));
    assert!(l[2].needs_review);
}

#[test]
fn nudge_never_moves_against_requested_direction() {
    let mut l = vec![s(1, 0.0, 5.20, "Önceki"), s(2, 5.05, 6.00, "Sonraki")];
    assert!(!l.nudge_start(1, -NUDGE_STEP));
    assert_eq!(l[1].start, 5.05);
    assert!(l.nudge_start(1, NUDGE_STEP));
    assert_eq!(l[1].start, 5.20);
}

#[test]
fn set_start_never_jumps_against_requested_direction() {
    let mut l = vec![s(1, 0.0, 5.20, "Önceki"), s(2, 5.05, 6.00, "Sonraki")];
    assert!(!l.set_start(5.0, 1));
    assert_eq!(l[1].start, 5.05);
    assert!(l.set_start(5.15, 1));
    assert_eq!(l[1].start, 5.20);
}

#[test]
fn nudge_end_does_not_move_backwards_when_sentence_is_too_short() {
    let mut l = vec![s(1, 2.0, 2.05, "Kısa")];
    assert!(!l.nudge_end(0, -NUDGE_STEP));
    assert_eq!(l[0].end, 2.05);
    assert!(l.nudge_end(0, NUDGE_STEP));
    assert_eq!(l[0].end, 2.15);
}

fn two_words() -> Sentence {
    with_words(s(5, 0.0, 4.0, "Merhaba dünya"), vec![Word::new(0.0, 1.0, "Merhaba"), Word::new(2.0, 4.0, "dünya")])
}

#[test]
fn split_uses_word_boundary_at_playhead() {
    let mut l = vec![two_words()];
    l[0].needs_review = true;
    l[0].speaker = Some(1);
    let first = l[0].uid;
    assert!(l.split_sentence(0, 1.5));
    assert_eq!(l.len(), 2);
    assert_eq!((l[0].text.as_str(), l[1].text.as_str()), ("Merhaba", "dünya"));
    assert_eq!((l[0].start, l[0].end, l[1].start, l[1].end), (0.0, 1.5, 1.5, 4.0));
    assert_eq!(l[0].uid, first);
    assert_ne!(l[1].uid, first);
    assert_eq!(ids(&l), [1, 2]);
    assert_eq!(l[0].words, vec![Word::new(0.0, 1.0, "Merhaba")]);
    assert_eq!(l[1].words, vec![Word::new(2.0, 4.0, "dünya")]);
    assert_eq!((l[0].speaker, l[1].speaker), (Some(1), Some(1)));
    assert!(l[0].needs_review && !l[1].needs_review);
}

#[test]
fn split_inside_first_word_still_uses_word_boundary() {
    let mut l = vec![two_words()];
    assert!(l.split_sentence(0, 0.2));
    assert_eq!(l[0].text, "Merhaba");
    assert_eq!(l[0].end, 1.5);
    assert_eq!(l[1].start, 1.5);
}

#[test]
fn split_without_words_uses_middle_whitespace_and_proportional_time() {
    let mut l = vec![s(1, 0.0, 4.0, "Bir iki üç dört")];
    assert!(l.split_sentence(0, 3.5));
    assert_eq!(l.len(), 2);
    assert_eq!((l[0].text.as_str(), l[1].text.as_str()), ("Bir iki", "üç dört"));
    assert_eq!((l[0].end, l[1].start), (2.0, 2.0));
    assert!(l[0].words.is_empty() && l[1].words.is_empty());
}

#[test]
fn split_rejects_invalid_requests_without_changing_list() {
    let mut l = sample();
    assert!(!l.split_sentence(0, 10.0));
    assert!(!l.split_sentence(9, 1.0));
    let mut single = vec![s(1, 0.0, 2.0, "Tek")];
    assert!(!single.split_sentence(0, 1.0));
    let mut short = vec![s(1, 0.0, 0.15, "Bir iki")];
    assert!(!short.split_sentence(0, 0.075));
    assert_eq!(l, sample());
    assert_eq!((single.len(), short.len()), (1, 1));
}

#[test]
fn can_split_matches_split_prerequisites() {
    let splittable = vec![s(1, 0.0, 4.0, "Bir iki")];
    assert!(splittable.can_split(0, 2.0));
    assert!(!splittable.can_split(0, 5.0));
    assert!(!vec![s(1, 0.0, 4.0, "Tek")].can_split(0, 2.0));
    assert!(vec![two_words()].can_split(0, 1.0));
    assert!(!vec![s(1, 0.0, 0.15, "Bir iki")].can_split(0, 0.07));
}

#[test]
fn rounding_matches_swift_half_away_from_zero() {
    let mut l = vec![s(1, 1.0, 3.0, "A")];
    assert!(l.set_start(1.2345, 0));
    assert_eq!(l[0].start, 1.235);
}

// MARK: speakers and dashes

fn dialogue(speakers: &[Option<i64>]) -> Vec<Sentence> {
    speakers.iter().enumerate().map(|(i, sp)| spk(s(i as i64 + 1, i as f64, i as f64 + 1.0, "x"), *sp)).collect()
}

#[test]
fn dash_applies_at_speaker_changes() {
    let mut l = dialogue(&[Some(0), Some(1), Some(1), Some(0)]);
    l[0].text = "Hello.".into();
    l[1].text = "How are you?".into();
    assert_eq!(l.dialogue_dashes(), [true, true, false, true]);
    assert_eq!(l.display_text(0), "- Hello.");
    assert_eq!(l.display_text(2), "x");
    assert!(l.starts_dialogue_turn(1));
}

#[test]
fn dash_never_applies_with_one_speaker_or_none() {
    let mut one = dialogue(&[Some(0), Some(0)]);
    one[1].text = "B".into();
    assert_eq!(one.dialogue_dashes(), [false, false]);
    assert_eq!(one.display_text(1), "B");
    assert_eq!(dialogue(&[None, None]).dialogue_dashes(), [false, false]);
    assert!(!dialogue(&[Some(0), Some(1)]).starts_dialogue_turn(99));
}

#[test]
fn dash_skips_sentences_without_speaker() {
    let l = dialogue(&[Some(0), None, Some(1)]);
    assert!(!l.starts_dialogue_turn(1));
    assert!(l.starts_dialogue_turn(2));
}

#[test]
fn matches_engine_speaker_dashes() {
    let cases: [(&[Option<i64>], &[bool]); 6] = [
        (&[Some(0), Some(0), Some(1), Some(0)], &[true, false, true, true]),
        (&[Some(0), Some(0), Some(0)], &[false, false, false]),
        (&[None, None, None], &[false, false, false]),
        (&[Some(0), None, Some(0), Some(1)], &[true, false, false, true]),
        (&[Some(0), None, Some(0)], &[false, false, false]),
        (&[], &[]),
    ];
    for (speakers, expected) in cases {
        assert_eq!(dialogue(speakers).dialogue_dashes(), expected, "{speakers:?}");
    }
}

#[test]
fn dash_is_not_doubled_on_already_dashed_text() {
    let mut l = dialogue(&[Some(0), Some(1), Some(0)]);
    l[0].text = "- Selam.".into();
    l[1].text = "– Merhaba.".into();
    l[2].text = "-Kısa.".into();
    assert_eq!(l.display_text(0), "- Selam.");
    assert_eq!(l.display_text(1), "– Merhaba.");
    assert_eq!(l.display_text(2), "- -Kısa.");
    assert_eq!(DialogueDash::apply(false, "Selam."), "Selam.");
    assert_eq!(DialogueDash::apply(true, "Selam."), "- Selam.");
    assert!(DialogueDash::has_dash("– x"));
    assert!(!DialogueDash::has_dash("-x"));
}

#[test]
fn set_speaker_changes_and_reports_no_op() {
    let mut l = dialogue(&[Some(0), Some(0)]);
    assert!(l.set_speaker(Some(1), 1));
    assert!(!l.set_speaker(Some(1), 1));
    assert!(!l.set_speaker(Some(1), 9));
    assert_eq!(l.next_new_speaker(), 2);
    assert_eq!(l.speakers(), [0, 1]);
    assert_eq!(dialogue(&[None]).next_new_speaker(), 0);
    assert!(l.set_speaker(None, 0));
    assert_eq!(l.speakers(), [1]);
}

#[test]
fn merge_keeps_first_speaker() {
    let mut l = dialogue(&[Some(0), Some(1), Some(1), Some(0)]);
    assert!(l.merge_with_next(0));
    assert_eq!(l[0].speaker, Some(0));
    assert_eq!(l.len(), 3);
}

#[test]
fn split_keeps_speaker_on_both_halves() {
    let mut l = vec![spk(s(1, 0.0, 4.0, "Bir iki"), Some(1))];
    assert!(l.split_sentence(0, 2.0));
    assert_eq!(l.iter().map(|x| x.speaker).collect::<Vec<_>>(), [Some(1), Some(1)]);
}

// MARK: snapshot merge

#[test]
fn first_snapshot_is_taken_as_is() {
    let snapshot = vec![s(1, 0.0, 1.0, "a")];
    let merged = Vec::<Sentence>::new().merged_with_snapshot(&snapshot);
    assert_eq!(merged.iter().map(|x| x.uid).collect::<Vec<_>>(), snapshot.iter().map(|x| x.uid).collect::<Vec<_>>());
}

#[test]
fn unchanged_lines_keep_their_identity() {
    let existing = vec![s(1, 0.0, 1.0, "a"), s(2, 1.0, 2.0, "b")];
    let snapshot = vec![s(1, 0.0, 1.0, "a"), s(2, 1.0, 2.0, "b"), s(3, 2.0, 3.0, "c")];
    let merged = existing.merged_with_snapshot(&snapshot);
    assert_eq!((merged[0].uid, merged[1].uid), (existing[0].uid, existing[1].uid));
    assert_eq!(merged[2].uid, snapshot[2].uid);
    assert_eq!(merged.iter().map(|x| x.text.as_str()).collect::<Vec<_>>(), ["a", "b", "c"]);
}

#[test]
fn refined_line_with_new_text_gets_a_new_identity() {
    let existing = vec![s(1, 0.0, 1.0, "a"), s(2, 1.0, 2.0, "ben")];
    let merged = existing.merged_with_snapshot(&[s(1, 0.0, 1.0, "a"), s(2, 1.0, 2.6, "ben geliyorum")]);
    assert_eq!(merged[0].uid, existing[0].uid);
    assert_ne!(merged[1].uid, existing[1].uid);
    assert_eq!((merged[1].text.as_str(), merged[1].end), ("ben geliyorum", 2.6));
}

#[test]
fn same_number_alone_never_matches() {
    let existing = vec![s(1, 0.0, 1.0, "a")];
    assert_ne!(existing.merged_with_snapshot(&[s(1, 5.0, 6.0, "b")])[0].uid, existing[0].uid);
}

#[test]
fn duplicate_identical_lines_each_keep_their_own_identity() {
    let existing = vec![s(1, 0.0, 1.0, "Evet."), s(2, 0.0, 1.0, "Evet."), s(3, 0.0, 1.0, "Evet.")];
    let snapshot = vec![s(1, 0.0, 1.0, "Evet."), s(2, 0.0, 1.0, "Evet."), s(3, 0.0, 1.0, "Evet.")];
    let uids = |l: &[Sentence]| l.iter().map(|x| x.uid).collect::<Vec<_>>();
    assert_eq!(uids(&existing.merged_with_snapshot(&snapshot)), uids(&existing));
    let mut more = snapshot.clone();
    more.push(s(4, 0.0, 1.0, "Evet."));
    let merged = existing.merged_with_snapshot(&more);
    assert_eq!(uids(&merged[..3]), uids(&existing));
    assert_eq!(merged.iter().map(|x| x.uid).collect::<HashSet<_>>().len(), 4);
}

#[test]
fn near_match_keeps_identity_for_the_same_text_within_one_and_a_half_seconds() {
    let existing = vec![s(1, 10.0, 12.0, "Merhaba.")];
    let near = existing.merged_with_snapshot(&[s(1, 11.5, 13.0, "Merhaba.")]);
    assert_eq!(near[0].uid, existing[0].uid);
    assert_eq!(near[0].start, 11.5);
    assert_ne!(existing.merged_with_snapshot(&[s(1, 11.6, 13.0, "Merhaba.")])[0].uid, existing[0].uid);
    assert_ne!(existing.merged_with_snapshot(&[s(1, 10.0, 12.0, "Merhaba!")])[0].uid, existing[0].uid);
}

#[test]
fn exact_matches_are_reserved_before_near_matches() {
    let existing = vec![s(1, 1.0, 2.0, "x")];
    let merged = existing.merged_with_snapshot(&[s(1, 0.5, 1.5, "x"), s(2, 1.0, 2.0, "x")]);
    assert_eq!(merged[1].uid, existing[0].uid);
    assert_ne!(merged[0].uid, existing[0].uid);
}

#[test]
fn ids_shifting_after_a_speaker_split_do_not_steal_identities() {
    let existing = vec![s(1, 0.0, 4.0, "Merhaba nasılsın."), s(2, 4.0, 6.0, "İyiyim.")];
    let merged = existing.merged_with_snapshot(&[s(1, 0.0, 2.0, "Merhaba"), s(2, 2.0, 4.0, "nasılsın."), s(3, 4.0, 6.0, "İyiyim.")]);
    assert_eq!(merged[2].uid, existing[1].uid);
    for i in 0..2 {
        assert!(merged[i].uid != existing[0].uid && merged[i].uid != existing[1].uid);
    }
}

#[test]
fn each_identity_is_used_once() {
    let existing = vec![s(1, 0.0, 1.0, "a")];
    let merged = existing.merged_with_snapshot(&[s(1, 0.0, 1.0, "a"), s(2, 0.0, 1.0, "a")]);
    assert_eq!(merged[0].uid, existing[0].uid);
    assert_ne!(merged[1].uid, existing[0].uid);
}

#[test]
fn time_match_wins_over_number_when_lines_shift() {
    let existing = vec![s(1, 0.0, 1.0, "a"), s(2, 1.0, 2.0, "b")];
    let merged = existing.merged_with_snapshot(&[s(1, -1.0, 0.0, "z"), s(2, 0.0, 1.0, "a"), s(3, 1.0, 2.0, "b")]);
    assert_eq!((merged[1].uid, merged[2].uid), (existing[0].uid, existing[1].uid));
    assert_ne!(merged[0].uid, existing[0].uid);
}

#[test]
fn content_speaker_and_review_flag_come_from_the_snapshot() {
    let existing = vec![spk(s(1, 0.0, 1.0, "a"), Some(1))];
    let mut incoming = spk(s(1, 0.0, 1.0, "a"), Some(0));
    incoming.needs_review = true;
    let merged = existing.merged_with_snapshot(&[incoming]);
    assert_eq!(merged[0].speaker, Some(0));
    assert!(merged[0].needs_review);
    assert_eq!(merged[0].uid, existing[0].uid);
}

#[test]
fn the_millisecond_key_uses_bankers_rounding() {
    // 1.0005 s is a tie that goes to the even millisecond (1000 ms), so it is an exact match of a 1.000 line and
    // takes its identity before the earlier near match can.
    assert_eq!((1.0005f64 * 1000.0).round_ties_even(), 1000.0);
    let existing = vec![s(1, 1.0, 2.0, "a")];
    let merged = existing.merged_with_snapshot(&[s(1, 0.9, 2.0, "a"), s(2, 1.0005, 2.0, "a")]);
    assert_eq!(merged[1].uid, existing[0].uid);
    assert_ne!(merged[0].uid, existing[0].uid);
}

// MARK: JSON wire format

#[test]
fn sentence_json_uses_utf8_unescaped_and_integral_numbers() {
    assert_eq!(Sentence::encode_list(&[s(1, 0.0, 1.0, "Şöyle")]).unwrap(), r#"[{"id":1,"start":0,"end":1,"text":"Şöyle","words":[]}]"#);
    let w = with_words(s(2, 0.5, 1.25, "<a & 'b'>"), vec![Word::new(0.5, 1.0, "x")]);
    assert_eq!(
        Sentence::encode_list(&[w]).unwrap(),
        r#"[{"id":2,"start":0.5,"end":1.25,"text":"<a & 'b'>","words":[{"start":0.5,"end":1,"text":"x"}]}]"#
    );
    assert!(Sentence::encode_list(&[s(1, f64::NAN, 1.0, "x")]).is_err());
}

#[test]
fn encodes_speaker_only_when_present() {
    let json = Sentence::encode_list(&[spk(s(1, 0.0, 1.0, "a"), Some(2))]).unwrap();
    assert!(json.contains("\"speaker\":2"));
    assert!(!Sentence::encode_list(&[s(1, 0.0, 1.0, "a")]).unwrap().contains("speaker"));
}

#[test]
fn decodes_sentence_with_speaker_without_or_null() {
    let one = Sentence::decode_list(r#"[{"id": 1, "start": 0, "end": 1, "text": "a", "speaker": 1}]"#).unwrap();
    assert_eq!(one[0].speaker, Some(1));
    let none = Sentence::decode_list(r#"[{"id": 1, "start": 0, "end": 1, "text": "a"}, {"id": 2, "start": 1, "end": 2, "text": "b", "speaker": null}]"#).unwrap();
    assert_eq!((none[0].speaker, none[1].speaker), (None, None));
    assert!(Sentence::decode_list(r#"[{"id": 1, "start": 0, "end": 1, "text": "a", "speaker": "x"}]"#).is_err());
}

#[test]
fn uid_is_stable_for_selection_and_stays_out_of_json() {
    let original = with_words(s(1, 0.0, 1.0, "Merhaba."), vec![Word::new(0.0, 1.0, "Merhaba.")]);
    let json = Sentence::encode_list(std::slice::from_ref(&original)).unwrap();
    assert!(!json.to_lowercase().contains("uid"));
    let decoded = Sentence::decode_list(&json).unwrap();
    assert_ne!(decoded[0].uid, original.uid);
    assert_eq!(decoded[0], original);
}

#[test]
fn words_default_to_empty_when_missing_and_required_fields_fail() {
    let d = Sentence::decode_list(r#"[{"id": 1, "start": 0, "end": 1, "text": "A"}, {"id": 2, "start": 1, "end": 2, "text": "B", "words": null}]"#).unwrap();
    assert!(d[0].words.is_empty() && d[1].words.is_empty());
    assert!(Sentence::decode_list(r#"[{"id": 1, "start": 0, "text": "A"}]"#).is_err());
    assert!(Sentence::decode_list(r#"[{"id": 1.5, "start": 0, "end": 1, "text": "A"}]"#).is_err());
    assert!(Sentence::decode_list(r#"[{"id": 1, "start": 0, "end": 1, "text": null}]"#).is_err());
}

#[test]
fn review_flag_survives_json_while_old_sentences_default_to_false() {
    let decoded = Sentence::decode_list(r#"[{"id": 1, "start": 0, "end": 1, "text": "a", "words": [], "needs_review": true}]"#).unwrap();
    assert!(decoded[0].needs_review);
    let json = Sentence::encode_list(&decoded).unwrap();
    assert!(json.contains("\"needs_review\":true"));
    assert!(Sentence::decode_list(&json).unwrap()[0].needs_review);
    let old = Sentence::decode_list(r#"[{"id": 1, "start": 0, "end": 1, "text": "a"}]"#).unwrap();
    assert!(!old[0].needs_review);
    assert!(!Sentence::encode_list(&old).unwrap().contains("needs_review"));
}

#[test]
fn only_a_json_true_sets_the_review_flag_and_a_malformed_one_never_fails() {
    for value in ["false", "1", "\"yes\"", "\"x\"", "null", "{}", "[true]"] {
        let json = format!(r#"[{{"id": 1, "start": 0, "end": 1, "text": "a", "needs_review": {value}}}]"#);
        let d = Sentence::decode_list(&json).unwrap_or_else(|e| panic!("{value}: {e}"));
        assert!(!d[0].needs_review, "{value}");
    }
}

#[test]
fn equality_and_hash_include_the_review_flag_but_not_the_uid() {
    use std::hash::{BuildHasher, RandomState};
    let a = s(1, 0.0, 1.0, "a");
    let mut b = a.clone();
    b.uid = Uuid::new_v4();
    let hasher = RandomState::new();
    assert_eq!(a, b);
    assert_eq!(hasher.hash_one(&a), hasher.hash_one(&b));
    b.needs_review = true;
    assert_ne!(a, b);
}
