//! Ports of `ViewModelTranslationTests`, `TranslationContextTests`, `SelectedTranslationTests` and the macOS
//! `TranslationProgressTests` / `ExternalLLMTests` subset that runs against the fake engine.

mod common;

use std::collections::HashSet;

use common::*;
use sa_core::appearance::SubtitleDisplay;
use sa_core::engine_types::{EngineStage, Sentence, SubtitleFormat};
use sa_core::l10n::{CatalogChain, UiLanguage};
use sa_core::settings::{SettingsStore, keys};
use sa_viewmodels::SubtitleViewModel;
use sa_viewmodels::types::{ExportLanguage, JobStatus, JobStatusKind};

/// Opens `name` and runs a transcription that returns `lines` in `lang`.
fn transcribed(env: &Env, vm: &SubtitleViewModel, name: &str, lang: &str, lines: &[&str]) -> std::path::PathBuf {
    let p = env.touch(name, "video");
    vm.open_video(Some(p.clone()));
    vm.generate();
    let call = env.next(CallKind::Transcribe);
    let sentences: Vec<Sentence> = lines.iter().enumerate().map(|(i, t)| s(i as i64 + 1, i as f64, i as f64 + 1.0, t)).collect();
    call.result(lang, "", sentences);
    env.eventually("transcribed", || vm.status() == JobStatus::Finished);
    p
}

fn translated(lines: &[(i64, &str)]) -> Vec<Sentence> {
    lines.iter().map(|(id, t)| s(*id, *id as f64 - 1.0, *id as f64, t)).collect()
}

fn win_env() -> (LanguageScope, Env) {
    let l = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    env.settings.set_bool(keys::AUTO_SAVE_SUBTITLE, false);
    (l, env)
}

fn mac_env() -> (LanguageScope, Env) {
    let l = LanguageScope::new(CatalogChain::Mac, UiLanguage::Tr);
    let env = Env::mac();
    env.settings.set_bool(keys::AUTO_SAVE_SUBTITLE, false);
    (l, env)
}

#[test]
fn translation_is_mapped_by_uid() {
    let (_l, env) = win_env();
    env.settings.set_string(keys::SELECTED_LANGUAGE, "tr");
    env.settings.set_string(keys::TRANSLATION_MODEL, "nllb-1.3b");
    let (vm, _c) = env.vm();
    transcribed(&env, &vm, "a.mp4", "tr", &["Bir.", "İki."]);
    vm.translate();
    let call = env.next(CallKind::Translate);
    assert_eq!(call.record.to.as_deref(), Some("en"));
    assert_eq!(call.record.from.as_deref(), Some("tr"));
    assert_eq!(call.record.model.as_deref(), Some("nllb-1.3b"));
    call.translation("en", translated(&[(1, "One."), (2, "T2")]));
    call.finish();
    env.eventually("finished", || vm.status() == JobStatus::Finished);
    let s2 = vm.sentences()[1].clone();
    assert_eq!(vm.translation_for(s2.uid).map(|e| e.text), Some("T2".into()));
    assert_eq!(vm.translation_language().as_deref(), Some("en"));
    assert_eq!(vm.translation_target(), "en");
    assert!(!vm.is_translation_stale(s2.uid));
}

#[test]
fn editing_the_original_marks_translation_stale_and_undo_clears_it() {
    let (_l, env) = win_env();
    env.settings.set_string(keys::SELECTED_LANGUAGE, "tr");
    let (vm, _c) = env.vm();
    transcribed(&env, &vm, "a.mp4", "tr", &["Bir.", "İki."]);
    vm.translate();
    let call = env.next(CallKind::Translate);
    call.translation("en", translated(&[(1, "One."), (2, "Two.")]));
    call.finish();
    env.eventually("finished", || vm.status() == JobStatus::Finished);
    let uid = vm.sentences()[0].uid;
    vm.set_text(uid, "Birinci.");
    assert!(vm.is_translation_stale(uid));
    vm.undo();
    assert!(!vm.is_translation_stale(uid));
}

#[test]
fn merge_and_split_clear_affected_translations_and_undo_restores() {
    let (_l, env) = win_env();
    env.settings.set_string(keys::SELECTED_LANGUAGE, "tr");
    let (vm, _c) = env.vm();
    transcribed(&env, &vm, "a.mp4", "tr", &["Bir.", "İki.", "Üç dört."]);
    vm.translate();
    let call = env.next(CallKind::Translate);
    call.translation("en", translated(&[(1, "One."), (2, "Two."), (3, "Three four.")]));
    call.finish();
    env.eventually("finished", || vm.status() == JobStatus::Finished);
    let lines = vm.sentences();
    vm.set_selection(Some(lines[0].uid));
    vm.merge_selected_with_next();
    assert_eq!(vm.sentences().len(), 2);
    assert!(vm.translation_for(vm.sentences()[0].uid).is_none(), "merged line loses its translation");
    assert!(vm.translation_for(vm.sentences()[1].uid).is_some());
    vm.undo();
    assert_eq!(vm.sentences().len(), 3);
    assert_eq!(vm.translation_for(lines[0].uid).map(|e| e.text), Some("One.".into()));
    assert_eq!(vm.translation_for(lines[1].uid).map(|e| e.text), Some("Two.".into()));
    vm.redo();
    assert!(vm.translation_for(vm.sentences()[0].uid).is_none(), "redo clears the merged translation again");
    vm.undo();
    vm.set_selection(Some(lines[2].uid));
    vm.split_selected(2.5);
    assert_eq!(vm.sentences().len(), 4);
    assert!(vm.translation_for(vm.sentences()[2].uid).is_none());
    vm.undo();
    assert_eq!(vm.translation_for(lines[2].uid).map(|e| e.text), Some("Three four.".into()));
}

#[test]
fn undoing_an_edit_made_before_translating_keeps_the_translation() {
    let (_l, env) = win_env();
    env.settings.set_string(keys::SELECTED_LANGUAGE, "tr");
    let (vm, _c) = env.vm();
    transcribed(&env, &vm, "a.mp4", "tr", &["Bir.", "İki.", "Üç."]);
    let uid = vm.sentences()[0].uid;
    vm.set_text(uid, "Birr.");
    vm.translate();
    let call = env.next(CallKind::Translate);
    call.translation("en", translated(&[(1, "T1"), (2, "T2"), (3, "T3")]));
    call.finish();
    env.eventually("finished", || vm.status() == JobStatus::Finished);
    vm.undo();
    let tr: Vec<Option<String>> = vm.sentences().iter().map(|x| vm.translation_for(x.uid).map(|e| e.text)).collect();
    assert_eq!(tr, vec![Some("T1".into()), Some("T2".into()), Some("T3".into())]);
    vm.redo();
    let tr: Vec<Option<String>> = vm.sentences().iter().map(|x| vm.translation_for(x.uid).map(|e| e.text)).collect();
    assert_eq!(tr, vec![Some("T1".into()), Some("T2".into()), Some("T3".into())]);
}

#[test]
fn translation_draft_commits_as_one_undoable_edit() {
    let (_l, env) = win_env();
    env.settings.set_string(keys::SELECTED_LANGUAGE, "tr");
    let (vm, _c) = env.vm();
    transcribed(&env, &vm, "a.mp4", "tr", &["Bir.", "İki."]);
    vm.translate();
    let call = env.next(CallKind::Translate);
    call.translation("en", translated(&[(1, "One."), (2, "Two.")]));
    call.finish();
    env.eventually("finished", || vm.status() == JobStatus::Finished);
    let uid = vm.sentences()[0].uid;
    let undo_before = vm.can_undo();
    vm.begin_translation_editing(uid);
    vm.set_translation_draft(uid, "First\nline");
    assert_eq!(vm.translation_for(uid).map(|e| e.text), Some("One.".into()), "not committed while drafting");
    assert_eq!(vm.translation_draft_text(uid), "First\nline");
    assert_eq!(vm.can_undo(), undo_before);
    vm.commit_translation_draft();
    assert_eq!(vm.translation_for(uid).map(|e| e.text), Some("First line".into()));
    assert_eq!(vm.undo_action_key().as_deref(), Some("Çeviriyi Değiştir"));
    vm.undo();
    assert_eq!(vm.translation_for(uid).map(|e| e.text), Some("One.".into()));
    vm.redo();
    assert_eq!(vm.translation_for(uid).map(|e| e.text), Some("First line".into()));
}

#[test]
fn export_uses_translation_text_with_original_times() {
    let (_l, env) = win_env();
    env.settings.set_string(keys::SELECTED_LANGUAGE, "tr");
    let (vm, _c) = env.vm();
    transcribed(&env, &vm, "clip.mp4", "tr", &["Bir.", "İki."]);
    vm.set_translation_target("es");
    vm.translate();
    let call = env.next(CallKind::Translate);
    call.translation("es", translated(&[(1, "Uno."), (2, "")]));
    call.finish();
    env.eventually("finished", || vm.status() == JobStatus::Finished);
    vm.set_export_format(SubtitleFormat::Vtt);
    vm.set_export_language(ExportLanguage::Translated);
    assert_eq!(vm.export_file_name(), "clip.es.vtt");
    let target = env.path("out/clip.es.vtt");
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    vm.export_to(target.clone());
    env.eventually("written", || target.exists());
    let (sent, fmt) = env.engine.renders.lock().last().cloned().unwrap();
    assert_eq!(fmt, SubtitleFormat::Vtt);
    assert_eq!(texts(&sent), vec!["Uno.", "İki."], "an empty translation falls back to the original");
    assert!(sent.iter().all(|x| x.words.is_empty()));
    assert_eq!((sent[0].start, sent[0].end), (0.0, 1.0));
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "Uno.|İki.");
    vm.set_export_language(ExportLanguage::Original);
    assert_eq!(vm.export_file_name(), "clip.vtt");
}

#[test]
fn render_error_is_shown_in_status() {
    let (_l, env) = win_env();
    let (vm, _c) = env.vm();
    transcribed(&env, &vm, "clip.mp4", "tr", &["Bir."]);
    *env.engine.render_result.lock() = Some(Err(sa_core::engine_types::EngineError::Failed("render boom".into())));
    vm.export_to(env.path("x.srt"));
    env.eventually("failed", || vm.status().kind() == JobStatusKind::Failed);
    assert!(vm.status_text().contains("render boom"), "{}", vm.status_text());
}

#[test]
fn same_source_and_target_is_rejected() {
    let (_l, env) = win_env();
    env.settings.set_string(keys::SELECTED_LANGUAGE, "tr");
    let (vm, _c) = env.vm();
    transcribed(&env, &vm, "a.mp4", "tr", &["Bir."]);
    env.settings.set_string(keys::TRANSLATION_TARGET, "tr");
    // Force the field (the setter itself does not reconcile).
    vm.set_translation_target("tr");
    vm.translate();
    assert_eq!(vm.status_text(), "Kaynak ve hedef dil aynı olamaz.");
    assert!(!env.engine.has_pending(CallKind::Translate));
    assert!(env.engine.calls_of(CallKind::Translate).is_empty());
}

#[test]
fn nllb_needs_source_language() {
    let (_l, env) = win_env();
    env.settings.set_string(keys::TRANSLATION_MODEL, "nllb-1.3b");
    let (vm, _c) = env.vm();
    let sub = env.touch("a.srt", "1\n00:00:01,000 --> 00:00:02,000\nMerhaba\n\n");
    assert!(vm.load_subtitle_file(&sub));
    vm.translate();
    assert_eq!(vm.status_text(), "NLLB için kaynak dil gerekli. Araç çubuğundan konuşma dilini seç.");
    assert!(env.engine.calls_of(CallKind::Translate).is_empty());
}

#[test]
fn translation_job_marks_model_active_and_can_be_cancelled() {
    let (_l, env) = win_env();
    env.settings.set_string(keys::SELECTED_LANGUAGE, "tr");
    let (vm, catalog) = env.vm();
    transcribed(&env, &vm, "a.mp4", "tr", &["Bir."]);
    vm.translate();
    assert_eq!(vm.status_text(), "Çeviri hazırlanıyor…");
    let call = env.next(CallKind::Translate);
    assert_eq!(catalog.active_models().into_iter().collect::<Vec<_>>(), vec!["madlad-3b".to_owned()]);
    call.stage(EngineStage::DownloadingModel, Some(0.5));
    env.eventually("download label", || vm.status_text() == "Çeviri modeli indiriliyor… %50");
    vm.cancel();
    assert_eq!(vm.status(), JobStatus::Idle);
    assert!(!vm.has_translations());
    env.eventually("cancelled", || call.is_cancelled());
}

#[test]
fn new_transcription_clears_translations() {
    let (_l, env) = win_env();
    env.settings.set_string(keys::SELECTED_LANGUAGE, "tr");
    let (vm, _c) = env.vm();
    transcribed(&env, &vm, "a.mp4", "tr", &["Bir."]);
    vm.translate();
    let call = env.next(CallKind::Translate);
    call.translation("en", translated(&[(1, "One.")]));
    call.finish();
    env.eventually("finished", || vm.status() == JobStatus::Finished);
    assert!(vm.has_translations());
    vm.generate();
    assert!(!vm.has_translations());
    env.next(CallKind::Transcribe).finish();
}

#[test]
fn translation_switches_display_mode_and_syncs_with_export_language() {
    let (_l, env) = win_env();
    env.settings.set_string(keys::SELECTED_LANGUAGE, "tr");
    let (vm, _c) = env.vm();
    transcribed(&env, &vm, "a.mp4", "tr", &["Bir.", "İki."]);
    assert_eq!(vm.display_mode(), SubtitleDisplay::Original);
    vm.translate();
    let call = env.next(CallKind::Translate);
    call.translation("en", translated(&[(1, "One.")]));
    env.eventually("first batch", || vm.has_translations());
    assert_eq!(vm.display_mode(), SubtitleDisplay::Translation);
    assert_eq!(vm.export_language(), ExportLanguage::Translated);
    vm.set_display_mode(SubtitleDisplay::Original);
    call.translation("en", translated(&[(1, "One."), (2, "Two.")]));
    call.finish();
    env.eventually("finished", || vm.status() == JobStatus::Finished);
    assert_eq!(vm.display_mode(), SubtitleDisplay::Original, "a later batch keeps the user's Original");
    assert_eq!(vm.export_language(), ExportLanguage::Original);
    vm.set_export_language(ExportLanguage::Translated);
    assert_eq!(vm.display_mode(), SubtitleDisplay::Translation);
}

#[test]
fn save_subtitle_to_video_folder_writes_next_to_video_with_same_name() {
    let (_l, env) = win_env();
    let (vm, _c) = env.vm();
    let video = transcribed(&env, &vm, "v/My Video.mp4", "tr", &["T1", "T2"]);
    assert!(vm.can_save_subtitle());
    vm.save_subtitle_next_to_video();
    let out = video.with_file_name("My Video.srt");
    env.eventually("saved", || out.exists());
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "T1|T2");
    env.eventually("status", || vm.status_text().contains("kaydedildi: My Video.srt"));
}

#[test]
fn open_video_auto_loads_sidecar_srt_and_allows_incremental_translation() {
    let (_l, env) = win_env();
    env.settings.set_string(keys::SELECTED_LANGUAGE, "tr");
    let (vm, _c) = env.vm();
    env.touch("m/Film.srt", "1\n00:00:01,250 --> 00:00:03,500\n<i>Merhaba</i>\n\n2\n00:00:04,000 --> 00:00:05,000\nDünya\n\n");
    let video = env.touch("m/Film.mp4", "v");
    vm.open_video(Some(video));
    env.eventually("sidecar loaded", || vm.sentences().len() == 2);
    let first = vm.sentences()[0].clone();
    assert_eq!(first.text, "Merhaba");
    assert_eq!((first.start, first.end), (1.25, 3.5));
    vm.translate();
    let call = env.next(CallKind::Translate);
    call.translation("en", translated(&[(1, "Hello")]));
    env.eventually("live batch", || vm.has_translations());
    assert_eq!(vm.display_mode(), SubtitleDisplay::Translation);
    call.translation("en", translated(&[(1, "Hello"), (2, "World")]));
    call.finish();
    env.eventually("finished", || vm.status() == JobStatus::Finished);
}

#[test]
fn generate_and_translate_chains_transcription_translation_and_auto_save() {
    let (_l, env) = win_env();
    env.settings.set_bool(keys::AUTO_SAVE_SUBTITLE, true);
    env.settings.set_string(keys::SELECTED_LANGUAGE, "en");
    env.settings.set_string(keys::TRANSLATION_TARGET, "tr");
    let (vm, _c) = env.vm();
    let video = env.touch("Episode.mp4", "v");
    vm.open_video(Some(video.clone()));
    vm.generate_and_translate();
    let t = env.next(CallKind::Transcribe);
    t.result("en", "", vec![s(1, 0.0, 1.0, "Hello."), s(2, 1.0, 2.0, "How are you?")]);
    let tr = env.next(CallKind::Translate);
    tr.translation("tr", translated(&[(1, "Merhaba."), (2, "Nasılsın?")]));
    tr.finish();
    let out = video.with_file_name("Episode.srt");
    env.eventually("auto-saved", || out.exists() && vm.status() == JobStatus::Finished);
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "Merhaba.|Nasılsın?");
}

#[test]
fn with_translation_context_includes_two_neighbours() {
    use sa_viewmodels::subtitle::session::with_translation_context;
    let all: Vec<Sentence> = (1..=8).map(|i| s(i, i as f64, i as f64 + 1.0, &format!("L{i}"))).collect();
    let pick = |id_list: &[i64]| -> Vec<i64> {
        let req: Vec<Sentence> = all.iter().filter(|x| id_list.contains(&x.id)).cloned().collect();
        ids(&with_translation_context(&all, &req))
    };
    assert_eq!(pick(&[4]), vec![2, 3, 4, 5, 6]);
    assert_eq!(pick(&[1]), vec![1, 2, 3]);
    assert_eq!(pick(&[8]), vec![6, 7, 8]);
    assert_eq!(pick(&[2, 8]), vec![1, 2, 3, 4, 6, 7, 8]);
    assert_eq!(pick(&[3, 4]), vec![1, 2, 3, 4, 5, 6]);
    assert!(pick(&[]).is_empty());
    assert_eq!(pick(&[1, 2, 3, 4, 5, 6, 7, 8]), vec![1, 2, 3, 4, 5, 6, 7, 8]);
}

#[test]
fn async_chunk_includes_neighbouring_dialogue_without_renumbering() {
    use sa_viewmodels::subtitle::session::with_translation_context;
    let all: Vec<Sentence> = (10..=20).map(|i| s(i, i as f64, i as f64 + 1.0, "x")).collect();
    let req: Vec<Sentence> = all.iter().filter(|x| (14..=15).contains(&x.id)).cloned().collect();
    assert_eq!(ids(&with_translation_context(&all, &req)), vec![12, 13, 14, 15, 16, 17]);
}

#[test]
fn selected_retry_keeps_other_translations_and_snapshots_selection() {
    let (_l, env) = win_env();
    env.settings.set_string(keys::SELECTED_LANGUAGE, "tr");
    let (vm, _c) = env.vm();
    let lines: Vec<String> = (1..=10).map(|i| format!("Satır {i}")).collect();
    let refs: Vec<&str> = lines.iter().map(|x| x.as_str()).collect();
    transcribed(&env, &vm, "a.mp4", "tr", &refs);
    vm.translate();
    let call = env.next(CallKind::Translate);
    call.translation("en", (1..=10).map(|i| s(i, 0.0, 1.0, &format!("Old {i}"))).collect());
    call.finish();
    env.eventually("finished", || vm.status() == JobStatus::Finished);
    let all = vm.sentences();
    let sel: HashSet<_> = [all[1].uid, all[8].uid].into_iter().collect();
    vm.set_selected_lines(sel, Some(all[1].uid));
    assert!(vm.can_translate_selected());
    vm.translate_selected();
    let call = env.next(CallKind::Translate);
    assert_eq!(ids(&call.record.sentences), vec![1, 2, 3, 4, 7, 8, 9, 10]);
    vm.set_selected_lines([all[0].uid].into_iter().collect(), Some(all[0].uid));
    call.translation("en", (1..=10).map(|i| s(i, 0.0, 1.0, &format!("New {i}"))).collect());
    call.finish();
    env.eventually("finished", || vm.status() == JobStatus::Finished);
    let got: Vec<String> = all.iter().map(|x| vm.translation_for(x.uid).map(|e| e.text).unwrap_or_default()).collect();
    let mut want: Vec<String> = (1..=10).map(|i| format!("Old {i}")).collect();
    want[1] = "New 2".into();
    want[8] = "New 9".into();
    assert_eq!(got, want, "only 2 and 9 change");
}

#[test]
fn removed_and_unknown_lines_cannot_remain_selected_or_start_a_job() {
    let (_l, env) = win_env();
    let (vm, _c) = env.vm();
    transcribed(&env, &vm, "a.mp4", "tr", &["Bir.", "İki."]);
    let unknown = uuid::Uuid::new_v4();
    vm.set_selected_lines([unknown].into_iter().collect(), Some(unknown));
    assert!(vm.selected_uids().is_empty());
    assert!(vm.selection().is_none());
    assert!(!vm.can_translate_selected());
    let uid = vm.sentences()[0].uid;
    vm.set_selected_lines([uid].into_iter().collect(), Some(uid));
    vm.delete_selected();
    assert!(!vm.selected_uids().contains(&uid));
}

#[test]
fn failed_retry_keeps_previous_text_and_marks_it_until_edited() {
    let (_l, env) = win_env();
    env.settings.set_string(keys::SELECTED_LANGUAGE, "tr");
    let (vm, _c) = env.vm();
    transcribed(&env, &vm, "a.mp4", "tr", &["Bir.", "İki."]);
    vm.translate();
    let call = env.next(CallKind::Translate);
    call.translation("en", translated(&[(1, "One."), (2, "Two.")]));
    call.finish();
    env.eventually("finished", || vm.status() == JobStatus::Finished);
    let uid = vm.sentences()[0].uid;
    vm.set_selected_lines([uid].into_iter().collect(), Some(uid));
    vm.translate_selected();
    let call = env.next(CallKind::Translate);
    let mut flagged = s(1, 0.0, 1.0, "Bir.");
    flagged.needs_review = true;
    call.translation("en", vec![flagged]);
    call.finish();
    env.eventually("finished", || vm.status() == JobStatus::Finished);
    let e = vm.translation_for(uid).unwrap();
    assert_eq!(e.text, "One.", "the earlier good text is kept");
    assert!(e.needs_review);
    assert!(vm.needs_review(uid));
    vm.set_translation_text(uid, "First.");
    assert!(!vm.needs_review(uid), "a hand edit clears the marker");
}

#[test]
fn imported_partial_subtitle_can_be_repaired_and_saved_without_video() {
    let (_l, env) = win_env();
    env.settings.set_bool(keys::AUTO_SAVE_SUBTITLE, true);
    env.settings.set_string(keys::SELECTED_LANGUAGE, "en");
    env.settings.set_string(keys::TRANSLATION_TARGET, "tr");
    let (vm, _c) = env.vm();
    let sub = env.touch("Dizi.srt", "1\n00:00:01,000 --> 00:00:02,000\nOne\n\n2\n00:00:03,000 --> 00:00:04,000\nTwo\n\n");
    assert!(vm.load_subtitle_file(&sub));
    let before: Vec<(uuid::Uuid, f64)> = vm.sentences().iter().map(|x| (x.uid, x.start)).collect();
    let uid = before[1].0;
    vm.set_selected_lines([uid].into_iter().collect(), Some(uid));
    vm.translate_selected();
    let call = env.next(CallKind::Translate);
    call.translation("tr", vec![s(2, 3.0, 4.0, "İki")]);
    call.finish();
    env.eventually("saved", || vm.status() == JobStatus::Finished && vm.saved_subtitle_name().is_some());
    let after: Vec<(uuid::Uuid, f64)> = vm.sentences().iter().map(|x| (x.uid, x.start)).collect();
    assert_eq!(before, after, "uids and times kept");
    assert_eq!(std::fs::read_to_string(&sub).unwrap(), "One|İki", "auto-save overwrites the SRT itself");
}

#[test]
fn a_fresh_translation_is_preparing_without_a_fraction_and_the_flag_ends_with_the_job() {
    let (_l, env) = mac_env();
    env.settings.set_string(keys::SELECTED_LANGUAGE, "tr");
    let (vm, _c) = env.vm();
    transcribed(&env, &vm, "a.mp4", "tr", &["Bir."]);
    vm.translate();
    assert_eq!(vm.status_text(), "Çeviri hazırlanıyor…");
    assert_eq!(vm.status_fraction(), None);
    assert!(vm.is_translation_job_running());
    assert_eq!(vm.translation_badge_text(), "Çeviri hazırlanıyor…");
    let call = env.next(CallKind::Translate);
    call.fail(sa_core::engine_types::EngineError::Failed("nope".into()));
    env.eventually("failed", || vm.status().kind() == JobStatusKind::Failed);
    assert!(!vm.is_translation_job_running());
}

#[test]
fn a_reusing_translation_starts_where_the_background_left_off() {
    let (_l, env) = mac_env();
    env.settings.set_string(keys::SELECTED_LANGUAGE, "tr");
    let (vm, _c) = env.vm();
    transcribed(&env, &vm, "a.mp4", "tr", &["Bir uzun satır.", "İki uzun satır.", "Üç uzun satır.", "Dört uzun satır."]);
    vm.translate();
    let call = env.next(CallKind::Translate);
    call.translation("en", translated(&[(1, "One"), (2, "Two")]));
    env.eventually("partial", || vm.translations().len() == 2);
    vm.cancel();
    env.eventually("idle", || vm.status() == JobStatus::Idle);
    vm.translate_with(true, None);
    assert_eq!(vm.status_fraction(), Some(0.5));
    assert_eq!(vm.status_text(), "Çevriliyor… %50");
    assert_eq!(vm.translation_badge_text(), vm.status_text());
    let call = env.next(CallKind::Translate);
    call.stage(EngineStage::Translating, Some(0.5));
    env.eventually("0.75", || vm.status_fraction() == Some(0.75));
    call.translation("en", translated(&[(3, "Three"), (4, "Four")]));
    call.finish();
    env.eventually("finished", || vm.status() == JobStatus::Finished);
    assert_eq!(vm.translations().len(), 4);
}

#[test]
fn translation_progress_formula() {
    use sa_viewmodels::subtitle::session::{translation_progress, translation_progress_resumed};
    assert_eq!(translation_progress(0, 10), None);
    assert_eq!(translation_progress(3, 0), None);
    assert_eq!(translation_progress(5, 10), Some(0.5));
    assert_eq!(translation_progress(10, 10), Some(0.99));
    assert_eq!(translation_progress(12, 10), Some(0.99));
    assert_eq!(translation_progress_resumed(2, 2, 0.0, 4), 0.5);
    assert_eq!(translation_progress_resumed(2, 2, 0.5, 4), 0.75);
    assert_eq!(translation_progress_resumed(2, 2, 1.0, 4), 0.99);
    assert_eq!(translation_progress_resumed(2, 2, 7.0, 4), 0.99);
    assert_eq!(translation_progress_resumed(2, 2, -1.0, 4), 0.5);
    assert_eq!(translation_progress_resumed(0, 4, 0.25, 4), 0.25);
    assert_eq!(translation_progress_resumed(0, 4, 1.0, 4), 0.99);
}

#[test]
fn reuse_only_translates_missing_outdated_and_wrong_language_lines() {
    let (_l, env) = mac_env();
    env.settings.set_string(keys::SELECTED_LANGUAGE, "tr");
    let (vm, _c) = env.vm();
    transcribed(&env, &vm, "a.mp4", "tr", &["Bir.", "İki.", "Üç.", "Dört."]);
    vm.translate();
    let call = env.next(CallKind::Translate);
    call.translation("en", translated(&[(1, "One."), (2, "Two."), (3, "Three.")]));
    call.finish();
    env.eventually("finished", || vm.status() == JobStatus::Finished);
    let uid3 = vm.sentences()[2].uid;
    vm.set_text(uid3, "Üç!");
    let uid2 = vm.sentences()[1].uid;
    vm.set_translation_text(uid2, "İki.");
    vm.translate_with(true, None);
    let call = env.next(CallKind::Translate);
    let req: Vec<String> = texts(&call.record.sentences);
    assert!(req.contains(&"Üç!".to_owned()) && req.contains(&"Dört.".to_owned()), "{req:?}");
    call.finish();
}

#[test]
fn reuse_with_everything_translated_only_saves() {
    let (_l, env) = mac_env();
    env.settings.set_bool(keys::AUTO_SAVE_SUBTITLE, true);
    env.settings.set_string(keys::SELECTED_LANGUAGE, "tr");
    let (vm, _c) = env.vm();
    let video = transcribed(&env, &vm, "Film.mp4", "tr", &["Bir uzun.", "İki uzun."]);
    vm.translate();
    let call = env.next(CallKind::Translate);
    call.translation("en", translated(&[(1, "One long."), (2, "Two long.")]));
    call.finish();
    let out = video.with_file_name("Film.srt");
    env.eventually("saved", || out.exists());
    std::fs::remove_file(&out).unwrap();
    let n = env.engine.calls_of(CallKind::Translate).len();
    vm.translate_with(true, None);
    env.eventually("saved again", || out.exists());
    assert_eq!(env.engine.calls_of(CallKind::Translate).len(), n, "engine not called");
}

#[test]
fn display_mode_falls_back_to_original_without_translations() {
    let (_l, env) = mac_env();
    env.settings.set_string(keys::SUBTITLE_DISPLAY_MODE, "translation");
    let (vm, _c) = env.vm();
    assert_eq!(vm.display_mode(), SubtitleDisplay::Translation);
    assert_eq!(vm.effective_display_mode(), SubtitleDisplay::Original);
}

/// `line_rows` (one read for the whole list) gives exactly what the per-line getters give.
#[test]
fn line_rows_match_the_per_line_getters() {
    for mac in [false, true] {
        let (_l, env) = if mac { mac_env() } else { win_env() };
        env.settings.set_string(keys::SELECTED_LANGUAGE, "tr");
        let (vm, _c) = env.vm();
        transcribed(&env, &vm, "a.mp4", "tr", &["Bir.", "İki.", "Üç.", "Dört."]);
        vm.translate();
        let call = env.next(CallKind::Translate);
        call.translation("en", translated(&[(1, "One."), (2, "Two."), (3, "Three.")]));
        call.finish();
        env.eventually("finished", || vm.status() == JobStatus::Finished);
        let uids: Vec<_> = vm.sentences().iter().map(|x| x.uid).collect();
        // Line 2: a failed retry marks it for review.
        vm.set_selected_lines([uids[1]].into_iter().collect(), Some(uids[1]));
        vm.translate_selected();
        let call = env.next(CallKind::Translate);
        let mut flagged = s(2, 1.0, 2.0, "İki.");
        flagged.needs_review = true;
        call.translation("en", vec![flagged]);
        call.finish();
        env.eventually("finished", || vm.status() == JobStatus::Finished);
        // Line 1 stale, line 3 has an open draft, line 2 an open translation draft, lines 1 and 4 selected.
        vm.set_text(uids[0], "Birinci.");
        vm.begin_translation_editing(uids[1]);
        vm.set_translation_draft(uids[1], "Second (draft)");
        vm.begin_editing(uids[2]);
        vm.set_draft(uids[2], "Üç (taslak)");
        vm.set_selected_lines([uids[0], uids[3]].into_iter().collect(), Some(uids[3]));
        let rows = vm.line_rows();
        let selected = vm.selected_uids();
        let expected: Vec<_> = vm
            .sentences()
            .iter()
            .map(|x| {
                let has_tr = vm.translation_for(x.uid).is_some();
                sa_viewmodels::subtitle::LineRow {
                    uid: x.uid,
                    id: x.id,
                    start: x.start,
                    end: x.end,
                    speaker: x.speaker,
                    text: vm.draft_text(x.uid),
                    translation: has_tr.then(|| vm.translation_draft_text(x.uid)),
                    is_stale: has_tr && vm.is_translation_stale(x.uid),
                    needs_review: vm.needs_review(x.uid),
                    is_selected: selected.contains(&x.uid),
                }
            })
            .collect();
        assert_eq!(rows, expected, "mac={mac}");
        assert!(rows[0].is_stale && rows[1].needs_review && rows[3].translation.is_none(), "mac={mac}: the fixture covers every flag");
        let reads = vm.read_count();
        vm.line_rows();
        assert_eq!(vm.read_count() - reads, 1, "one state read for the whole list");
        // `overlay_text` (composed in place) equals `OverlayText::compose` over copies of the same state, in every
        // display mode, with and without dialogue speakers.
        for speakers in [false, true] {
            if speakers {
                for (i, uid) in uids.iter().enumerate() {
                    vm.set_speaker(*uid, Some((i % 2) as i64));
                }
            }
            for mode in [SubtitleDisplay::Original, SubtitleDisplay::Translation, SubtitleDisplay::Both] {
                vm.set_display_mode(mode);
                let display = vm.effective_display_mode();
                for uid in uids.iter().copied().chain([uuid::Uuid::new_v4()]) {
                    let expected = sa_core::appearance::OverlayText::compose(&vm.sentences(), &vm.translations(), display, uid);
                    assert_eq!(vm.overlay_text(uid), expected, "mac={mac} speakers={speakers} mode={mode:?}");
                }
            }
        }
    }
}
