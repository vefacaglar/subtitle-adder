//! Ports of `EditingTests`, `UndoStackTests`, `SpeakerEditingTests` (C# + Swift) and the selection part of
//! `SelectedTranslationTests`.

mod common;

use std::collections::HashSet;

use common::*;
use sa_core::l10n::{CatalogChain, UiLanguage};
use sa_viewmodels::SubtitleViewModel;
use sa_viewmodels::types::TimingEdge;

fn loaded(env: &Env, lines: &[(f64, f64, &str)]) -> SubtitleViewModel {
    let (vm, _c) = env.vm();
    let mut srt = String::new();
    for (i, (a, b, t)) in lines.iter().enumerate() {
        let f = |x: f64| {
            let ms = (x * 1000.0).round() as i64;
            format!("{:02}:{:02}:{:02},{:03}", ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000)
        };
        srt.push_str(&format!("{}\n{} --> {}\n{}\n\n", i + 1, f(*a), f(*b), t));
    }
    let p = env.touch("x/lines.srt", &srt);
    assert!(vm.load_subtitle_file(&p));
    vm
}

fn win() -> (LanguageScope, Env) {
    (LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr), Env::win())
}
fn mac() -> (LanguageScope, Env) {
    (LanguageScope::new(CatalogChain::Mac, UiLanguage::Tr), Env::mac())
}

const THREE: [(f64, f64, &str); 3] = [(0.0, 1.0, "Bir iki"), (1.0, 2.0, "Üç dört"), (2.0, 3.0, "Beş altı")];

#[test]
fn draft_is_not_undoable_until_committed() {
    let (_l, env) = win();
    let vm = loaded(&env, &THREE);
    let uid = vm.sentences()[0].uid;
    vm.begin_editing(uid);
    vm.set_draft(uid, "Yeni");
    assert!(!vm.can_undo());
    assert_eq!(vm.sentences()[0].text, "Bir iki");
    assert_eq!(vm.draft_text(uid), "Yeni");
    vm.commit_draft();
    assert_eq!(vm.sentences()[0].text, "Yeni");
    assert!(vm.can_undo());
}

#[test]
fn draft_newlines_become_spaces() {
    let (_l, env) = win();
    let vm = loaded(&env, &THREE);
    let uid = vm.sentences()[0].uid;
    vm.set_draft(uid, "a\r\nb\nc\rd");
    vm.commit_draft();
    assert_eq!(vm.sentences()[0].text, "a b c d");
}

#[test]
fn updating_another_line_commits_the_previous_draft() {
    let (_l, env) = win();
    let vm = loaded(&env, &THREE);
    let (a, b) = (vm.sentences()[0].uid, vm.sentences()[1].uid);
    vm.set_draft(a, "A");
    vm.set_draft(b, "B");
    assert_eq!(vm.sentences()[0].text, "A");
    assert_eq!(vm.sentences()[1].text, "Üç dört");
    vm.commit_draft();
    assert_eq!(vm.sentences()[1].text, "B");
}

#[test]
fn undo_with_uncommitted_draft_reverts_the_typing() {
    let (_l, env) = win();
    let vm = loaded(&env, &THREE);
    let uid = vm.sentences()[0].uid;
    vm.set_draft(uid, "Yazıyorum");
    vm.undo();
    assert_eq!(vm.sentences()[0].text, "Bir iki");
    assert_eq!(vm.draft_text(uid), "Bir iki");
}

#[test]
fn structural_command_with_draft_undoes_as_one_group() {
    let (_l, env) = win();
    let vm = loaded(&env, &THREE);
    let uid = vm.sentences()[0].uid;
    vm.set_selection(Some(uid));
    vm.set_draft(uid, "Taslak metin");
    vm.merge_selected_with_next();
    assert_eq!(vm.sentences().len(), 2);
    vm.undo();
    assert_eq!(vm.sentences().len(), 3);
    assert_eq!(vm.sentences()[0].text, "Bir iki", "draft and merge undone together");
    assert!(!vm.can_undo());
}

#[test]
fn delete_selects_next_line_and_undo_restores() {
    let (_l, env) = win();
    let vm = loaded(&env, &THREE);
    let lines = vm.sentences();
    vm.set_selection(Some(lines[1].uid));
    vm.delete_selected();
    assert_eq!(texts(&vm.sentences()), vec!["Bir iki", "Beş altı"]);
    assert_eq!(vm.selection(), Some(lines[2].uid));
    assert_eq!(vm.undo_action_key().as_deref(), Some("Satırı Sil"));
    vm.undo();
    assert_eq!(vm.sentences().len(), 3);
}

#[test]
fn set_selected_time_and_split_availability() {
    let (_l, env) = win();
    let vm = loaded(&env, &THREE);
    let uid = vm.sentences()[0].uid;
    assert_eq!(vm.selected_edge_text(TimingEdge::Start), sa_core::text::SUBTITLE_TIME_PLACEHOLDER);
    vm.set_selection(Some(uid));
    vm.set_edge_to_now(TimingEdge::Start, 0.5);
    assert_eq!(vm.selected_edge_text(TimingEdge::Start), "00:00.50");
    assert!(vm.can_split_selected(0.75));
    assert!(!vm.can_split_selected(5.0));
}

#[test]
fn no_op_edits_leave_no_undo_record() {
    let (_l, env) = win();
    let vm = loaded(&env, &THREE);
    let uid = vm.sentences()[0].uid;
    vm.set_text(uid, "Bir iki");
    vm.set_draft(uid, "Bir iki");
    vm.commit_draft();
    assert!(!vm.can_undo());
}

#[test]
fn new_edit_clears_redo() {
    let (_l, env) = win();
    let vm = loaded(&env, &THREE);
    let uid = vm.sentences()[0].uid;
    vm.set_text(uid, "A");
    vm.undo();
    assert!(vm.can_redo());
    vm.set_text(uid, "B");
    assert!(!vm.can_redo());
}

#[test]
fn undo_titles_are_localized() {
    let (l, env) = win();
    let vm = loaded(&env, &THREE);
    l.set(UiLanguage::En);
    assert_eq!(vm.undo_title(), "Undo");
    let uid = vm.sentences()[0].uid;
    vm.set_selection(Some(uid));
    vm.delete_selected();
    assert!(vm.undo_title().starts_with("Undo "), "{}", vm.undo_title());
    l.set(UiLanguage::Tr);
    assert_eq!(vm.undo_title(), "Satırı Sil İşlemini Geri Al");
}

#[test]
fn delete_removes_every_selected_line_in_one_undo_step() {
    for env in [Env::win(), Env::mac()] {
        let _l = LanguageScope::new(env.policy.catalog_chain, UiLanguage::Tr);
        let vm = loaded(&env, &[(0.0, 1.0, "a b"), (1.0, 2.0, "c d"), (2.0, 3.0, "e f"), (3.0, 4.0, "g h"), (4.0, 5.0, "i j")]);
        let l = vm.sentences();
        let sel: HashSet<_> = [l[1].uid, l[3].uid].into_iter().collect();
        vm.set_selected_lines(sel, Some(l[1].uid));
        vm.delete_selected();
        assert_eq!(texts(&vm.sentences()), vec!["a b", "e f", "i j"]);
        assert_eq!(ids(&vm.sentences()), vec![1, 2, 3], "ids renumbered");
        assert_eq!(vm.undo_action_key().as_deref(), Some("Satırları Sil"));
        assert!(vm.selection().is_some(), "next line selected");
        vm.undo();
        assert_eq!(vm.sentences().len(), 5);
        assert!(!vm.can_undo(), "one undo step");
    }
}

#[test]
fn split_merge_and_timing_are_unavailable_for_several_selected_lines_on_mac() {
    let (_l, env) = mac();
    let vm = loaded(&env, &THREE);
    let l = vm.sentences();
    vm.set_selected_lines([l[0].uid, l[1].uid].into_iter().collect(), Some(l[0].uid));
    assert!(!vm.can_split_selected(0.5));
    assert!(!vm.can_merge_selected());
    assert!(!vm.can_edit_timing());
    vm.set_selected_lines([l[0].uid].into_iter().collect(), Some(l[0].uid));
    assert!(vm.can_merge_selected());
    assert!(vm.can_edit_timing());
}

#[test]
fn windows_split_merge_act_on_the_active_line_of_a_multi_selection() {
    let (_l, env) = win();
    let vm = loaded(&env, &THREE);
    let l = vm.sentences();
    vm.set_selected_lines([l[0].uid, l[1].uid].into_iter().collect(), Some(l[0].uid));
    assert!(vm.can_merge_selected());
    assert!(vm.can_edit_timing());
}

#[test]
fn setting_the_active_line_selects_only_it_and_falls_back_in_document_order() {
    let (_l, env) = mac();
    let vm = loaded(&env, &THREE);
    let l = vm.sentences();
    vm.set_selection(Some(l[2].uid));
    assert_eq!(vm.selected_uids(), [l[2].uid].into_iter().collect());
    vm.set_selected_lines([l[0].uid, l[1].uid].into_iter().collect(), None);
    assert_eq!(vm.selection(), Some(l[0].uid), "first in document order");
}

#[test]
fn list_selection_makes_the_last_added_line_active() {
    let (_l, env) = mac();
    let vm = loaded(&env, &THREE);
    let l = vm.sentences();
    vm.update_selection([l[0].uid].into_iter().collect());
    assert_eq!(vm.selection(), Some(l[0].uid));
    vm.update_selection([l[0].uid, l[2].uid].into_iter().collect());
    assert_eq!(vm.selection(), Some(l[2].uid));
}

#[test]
fn selecting_a_row_keeps_a_multi_selection_it_belongs_to() {
    let (_l, env) = mac();
    let vm = loaded(&env, &THREE);
    let l = vm.sentences();
    vm.set_selected_lines([l[0].uid, l[1].uid].into_iter().collect(), Some(l[0].uid));
    vm.select_row(l[1].uid);
    assert_eq!(vm.selected_uids().len(), 2);
    assert_eq!(vm.selection(), Some(l[1].uid));
    vm.select_row(l[2].uid);
    assert_eq!(vm.selected_uids(), [l[2].uid].into_iter().collect());
}

#[test]
fn change_speaker_undo_and_redo() {
    let (_l, env) = mac();
    let vm = loaded(&env, &THREE);
    let uid = vm.sentences()[0].uid;
    vm.set_speaker(uid, Some(1));
    assert_eq!(vm.sentences()[0].speaker, Some(1));
    vm.undo();
    assert_eq!(vm.sentences()[0].speaker, None);
    vm.redo();
    assert_eq!(vm.sentences()[0].speaker, Some(1));
}

#[test]
fn new_speaker_gets_unused_index_and_undoes() {
    let (_l, env) = mac();
    let vm = loaded(&env, &THREE);
    let l = vm.sentences();
    vm.set_speaker(l[0].uid, Some(0));
    vm.set_speaker(l[1].uid, Some(1));
    vm.assign_new_speaker(l[2].uid);
    assert_eq!(vm.sentences()[2].speaker, Some(2));
    vm.undo();
    assert_eq!(vm.sentences()[2].speaker, None);
    vm.set_speaker(l[1].uid, Some(0));
    vm.assign_new_speaker(l[2].uid);
    assert_eq!(vm.speakers(), vec![0, 1]);
}

#[test]
fn merge_keeps_first_speaker_and_undo_restores() {
    let (_l, env) = mac();
    let vm = loaded(&env, &THREE);
    let l = vm.sentences();
    vm.set_speaker(l[0].uid, Some(0));
    vm.set_speaker(l[1].uid, Some(1));
    vm.set_selection(Some(l[0].uid));
    vm.merge_selected_with_next();
    assert_eq!(vm.sentences()[0].speaker, Some(0));
    vm.undo();
    assert_eq!(vm.sentences()[1].speaker, Some(1));
}

#[test]
fn split_keeps_speaker_on_both_halves_and_undo_restores() {
    let (_l, env) = mac();
    let vm = loaded(&env, &THREE);
    let l = vm.sentences();
    vm.set_speaker(l[0].uid, Some(0));
    vm.set_speaker(l[1].uid, Some(1));
    vm.set_selection(Some(l[0].uid));
    vm.split_selected(0.5);
    let sp: Vec<Option<i64>> = vm.sentences().iter().map(|x| x.speaker).collect();
    assert_eq!(sp[..3], [Some(0), Some(0), Some(1)]);
    vm.undo();
    assert_eq!(vm.sentences().len(), 3);
}

#[test]
fn remove_speaker_undoes() {
    let (_l, env) = mac();
    let vm = loaded(&env, &THREE);
    let l = vm.sentences();
    vm.set_speaker(l[0].uid, Some(0));
    vm.set_speaker(l[1].uid, Some(0));
    vm.set_speaker(l[1].uid, None);
    let sp: Vec<Option<i64>> = vm.sentences().iter().take(2).map(|x| x.speaker).collect();
    assert_eq!(sp, vec![Some(0), None]);
    vm.undo();
    assert_eq!(vm.sentences()[1].speaker, Some(0));
}

#[test]
fn nudge_moves_edges_and_is_one_undo_step() {
    let (_l, env) = win();
    let vm = loaded(&env, &THREE);
    let uid = vm.sentences()[2].uid;
    vm.set_selection(Some(uid));
    vm.nudge(TimingEdge::End, 0.1);
    assert!((vm.sentences()[2].end - 3.1).abs() < 1e-9, "{}", vm.sentences()[2].end);
    vm.undo();
    assert!((vm.sentences()[2].end - 3.0).abs() < 1e-9);
    // The end of a line never passes the next line's start.
    vm.set_selection(Some(vm.sentences()[1].uid));
    vm.nudge(TimingEdge::End, 0.1);
    assert!(vm.sentences()[1].end <= vm.sentences()[2].start + 1e-9);
}

#[test]
fn subtitle_time_format() {
    use sa_viewmodels::subtitle::subtitle_time_short;
    assert_eq!(subtitle_time_short(65.25), "01:05.25");
    assert_eq!(subtitle_time_short(0.0), "00:00.00");
    assert_eq!(subtitle_time_short(0.5), "00:00.50");
}
