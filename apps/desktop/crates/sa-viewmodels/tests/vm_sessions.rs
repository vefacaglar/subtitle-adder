//! Ports of `SessionTests` / `SessionEdgeCaseTests` (Swift) and the Windows single-video queue test
//! `TranslatingVideoA_SwitchingToVideoB_QueuesVideoBSequential_…`.

mod common;

use std::cell::RefCell;
use std::rc::Rc;

use common::*;
use sa_core::l10n::{CatalogChain, UiLanguage};
use sa_core::settings::{SettingsStore, keys};
use sa_viewmodels::SubtitleViewModel;
use sa_viewmodels::types::{JobStatus, PlaylistItemState};

fn mac() -> (LanguageScope, Env) {
    let l = LanguageScope::new(CatalogChain::Mac, UiLanguage::Tr);
    let env = Env::mac();
    env.settings.set_bool(keys::AUTO_SAVE_SUBTITLE, false);
    (l, env)
}

fn open(env: &Env, vm: &SubtitleViewModel, name: &str) -> std::path::PathBuf {
    let p = env.path(name);
    if !p.exists() {
        env.touch(name, "v");
    }
    vm.open_video(Some(p.clone()));
    p
}

#[test]
fn switching_videos_keeps_the_job_running_and_restores_its_live_state_on_return() {
    let (_l, env) = mac();
    let (vm, _c) = env.vm();
    open(&env, &vm, "a.mp4");
    vm.generate();
    let a = env.next(CallKind::Transcribe);
    a.send(sa_enginekit::events::EngineEvent::Partial { language: "tr".into(), sentences: vec![s(1, 0.0, 1.0, "Canlı a")] });
    env.eventually("live line", || texts(&vm.sentences()) == vec!["Canlı a"]);
    open(&env, &vm, "b.mp4");
    assert!(vm.sentences().is_empty());
    assert_eq!(vm.status(), JobStatus::Idle);
    open(&env, &vm, "a.mp4");
    assert_eq!(texts(&vm.sentences()), vec!["Canlı a"]);
    assert!(vm.is_working());
    open(&env, &vm, "b.mp4");
    a.result("tr", "", vec![s(1, 0.0, 1.0, "Son a")]);
    env.eventually("a done", || !vm.has_active_work());
    assert!(vm.sentences().is_empty(), "A's result lands only in A");
    open(&env, &vm, "a.mp4");
    assert_eq!(texts(&vm.sentences()), vec!["Son a"]);
    assert_eq!(vm.status(), JobStatus::Finished);
}

#[test]
fn a_job_from_one_video_never_touches_the_toolbar_or_the_other_videos_display() {
    let (_l, env) = mac();
    env.settings.set_string(keys::SELECTED_LANGUAGE, "tr");
    env.settings.set_string(keys::TRANSLATION_TARGET, "es");
    let (vm, _c) = env.vm();
    open(&env, &vm, "a.mp4");
    vm.generate_and_translate();
    let t = env.next(CallKind::Transcribe);
    vm.set_translation_target("en");
    open(&env, &vm, "b.mp4");
    vm.set_translation_target("de");
    t.result("tr", "", vec![s(1, 0.0, 1.0, "Bir.")]);
    let tr = env.next(CallKind::Translate);
    assert_eq!(tr.record.to.as_deref(), Some("es"), "translated as requested at request time");
    tr.translation("es", vec![s(1, 0.0, 1.0, "Uno.")]);
    tr.finish();
    env.eventually("a done", || !vm.has_active_work());
    assert_eq!(vm.translation_target(), "de", "toolbar untouched");
    assert!(!vm.has_translations(), "the other video untouched");
    open(&env, &vm, "a.mp4");
    assert!(vm.has_translations());
}

#[test]
fn jobs_wait_in_fifo_order_and_one_new_request_per_video_replaces_the_old_one() {
    let (_l, env) = mac();
    let (vm, _c) = env.vm();
    open(&env, &vm, "a.mp4");
    vm.generate();
    let a = env.next(CallKind::Transcribe);
    open(&env, &vm, "b.mp4");
    vm.generate();
    assert_eq!(vm.status_text(), "Sırada");
    assert_eq!(vm.queue_position(), Some(1));
    open(&env, &vm, "c.mp4");
    vm.generate();
    assert_eq!(vm.queue_position(), Some(2));
    assert_eq!(vm.queued_job_count(), 2);
    open(&env, &vm, "b.mp4");
    vm.set_language("en");
    vm.generate();
    assert_eq!(vm.queue_position(), Some(1), "replaced in place");
    assert_eq!(vm.queued_job_count(), 2);
    assert_eq!(env.engine.calls_of(CallKind::Transcribe).len(), 1, "only a started");
    a.finish();
    let b = env.next(CallKind::Transcribe);
    assert!(b.record.video.as_ref().unwrap().ends_with("b.mp4"));
    assert_eq!(b.record.options.as_ref().unwrap().language.as_deref(), Some("en"), "carries the newer toolbar");
    b.finish();
    let c = env.next(CallKind::Transcribe);
    assert!(c.record.video.as_ref().unwrap().ends_with("c.mp4"));
    c.finish();
}

#[test]
fn stop_on_a_queued_video_removes_only_it_and_stop_on_the_running_one_lets_the_queue_go_on() {
    let (_l, env) = mac();
    let (vm, _c) = env.vm();
    open(&env, &vm, "a.mp4");
    vm.generate();
    let a = env.next(CallKind::Transcribe);
    open(&env, &vm, "b.mp4");
    vm.generate();
    open(&env, &vm, "c.mp4");
    vm.generate();
    open(&env, &vm, "b.mp4");
    vm.cancel();
    assert_eq!(vm.status(), JobStatus::Idle);
    assert_eq!(vm.queued_job_count(), 1);
    assert!(!a.is_cancelled());
    open(&env, &vm, "a.mp4");
    vm.cancel();
    env.eventually("a cancelled", || a.is_cancelled());
    let c = env.next(CallKind::Transcribe);
    assert!(c.record.video.as_ref().unwrap().ends_with("c.mp4"));
    c.finish();
}

#[test]
fn clear_queue_drops_the_waiting_jobs_and_leaves_the_running_one() {
    let (_l, env) = mac();
    let (vm, _c) = env.vm();
    open(&env, &vm, "a.mp4");
    vm.generate();
    let a = env.next(CallKind::Transcribe);
    open(&env, &vm, "b.mp4");
    vm.generate();
    open(&env, &vm, "c.mp4");
    vm.generate();
    assert_eq!(vm.queued_job_count(), 2);
    vm.clear_queue();
    assert_eq!(vm.queued_job_count(), 0);
    open(&env, &vm, "b.mp4");
    assert_eq!(vm.status(), JobStatus::Idle);
    a.result("tr", "", vec![s(1, 0.0, 1.0, "A")]);
    env.eventually("a finishes", || !vm.has_active_work());
    open(&env, &vm, "a.mp4");
    assert_eq!(vm.status(), JobStatus::Finished);
}

#[test]
fn the_auto_save_of_a_background_job_writes_its_own_videos_file_and_reports_its_url() {
    let (_l, env) = mac();
    env.settings.set_bool(keys::AUTO_SAVE_SUBTITLE, true);
    env.settings.set_string(keys::SELECTED_LANGUAGE, "tr");
    let (vm, _c) = env.vm();
    let saved = Rc::new(RefCell::new(Vec::new()));
    let s2 = saved.clone();
    vm.on_subtitle_saved(Box::new(move |p| s2.borrow_mut().push(p.to_path_buf())));
    let a = open(&env, &vm, "a.mp4");
    vm.generate_and_translate();
    let t = env.next(CallKind::Transcribe);
    let b = open(&env, &vm, "b.mp4");
    t.result("tr", "", vec![s(1, 0.0, 1.0, "Bir.")]);
    let tr = env.next(CallKind::Translate);
    tr.translation("en", vec![s(1, 0.0, 1.0, "One.")]);
    tr.finish();
    let a_srt = a.with_extension("srt");
    env.eventually("a.srt", || a_srt.exists() && !vm.has_active_work());
    assert_eq!(*saved.borrow(), vec![a_srt.clone()]);
    assert!(!b.with_extension("srt").exists());
    assert!(vm.sentences().is_empty(), "on-screen B untouched");
    open(&env, &vm, "a.mp4");
    assert_eq!(vm.status(), JobStatus::Finished);
    assert!(!vm.has_unsaved_edits());
}

#[test]
fn single_jobs_show_as_rows_with_their_state_and_a_click_opens_their_session() {
    let (_l, env) = mac();
    let (vm, _c) = env.vm();
    assert_eq!(vm.playlist_summary_text(), "Listede henüz video yok.");
    open(&env, &vm, "a.mp4");
    vm.generate();
    let a = env.next(CallKind::Transcribe);
    open(&env, &vm, "b.mp4");
    vm.generate();
    let rows = vm.list_rows();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].state, PlaylistItemState::Working);
    assert_eq!(rows[1].state, PlaylistItemState::Pending);
    assert_eq!(rows[1].status_text, "Sırada");
    assert!(rows.iter().all(|r| r.can_stop));
    assert!(rows.iter().all(|r| r.origin == sa_viewmodels::types::ListRowOrigin::Job));
    assert_eq!(vm.playlist_summary_text(), "0 / 2 tamamlandı");
    vm.set_workspace(sa_viewmodels::types::Workspace::Playlist);
    vm.open_list_row(&rows[0]);
    assert_eq!(vm.workspace(), sa_viewmodels::types::Workspace::Editor);
    assert!(vm.video().unwrap().ends_with("a.mp4"));
    vm.cancel_job_of(&rows[1].video_path);
    assert_eq!(vm.list_rows().len(), 1);
    a.finish();
}

#[test]
fn sessions_with_content_are_kept_and_empty_idle_ones_dropped() {
    let (_l, env) = mac();
    let (vm, _c) = env.vm();
    open(&env, &vm, "a.mp4");
    // An empty idle session is dropped when it leaves the screen; one with typed-in lines is kept.
    open(&env, &vm, "b.mp4");
    let sub = env.touch("other.srt", "1\n00:00:01,000 --> 00:00:02,000\nYazıldı\n\n");
    assert!(vm.load_subtitle_file(&sub));
    vm.set_text(vm.sentences()[0].uid, "Değişti");
    open(&env, &vm, "c.mp4");
    open(&env, &vm, "b.mp4");
    assert_eq!(texts(&vm.sentences()), vec!["Değişti"], "typed-in lines kept");
}

#[test]
fn the_queue_shows_a_position_and_the_editor_of_a_waiting_video_says_so() {
    let (_l, env) = mac();
    let (vm, _c) = env.vm();
    open(&env, &vm, "a.mp4");
    vm.generate();
    let a = env.next(CallKind::Transcribe);
    assert_eq!(vm.queue_position(), None);
    open(&env, &vm, "b.mp4");
    vm.generate();
    assert_eq!(vm.queue_position(), Some(1));
    assert!(vm.is_working(), "a queued video is working");
    open(&env, &vm, "c.mp4");
    vm.generate();
    assert_eq!(vm.queue_position(), Some(2));
    a.finish();
    env.eventually("b runs", || env.engine.has_pending(CallKind::Transcribe));
    open(&env, &vm, "b.mp4");
    assert_eq!(vm.queue_position(), None);
    open(&env, &vm, "c.mp4");
    assert_eq!(vm.queue_position(), Some(1));
    vm.clear_queue();
    env.next(CallKind::Transcribe).finish();
}

#[test]
fn requesting_the_running_video_again_replaces_it_in_place_at_the_head() {
    let (_l, env) = mac();
    let (vm, _c) = env.vm();
    open(&env, &vm, "a.mp4");
    vm.generate();
    let a = env.next(CallKind::Transcribe);
    open(&env, &vm, "b.mp4");
    vm.generate();
    open(&env, &vm, "a.mp4");
    vm.set_language("en");
    vm.generate();
    env.eventually("old cancelled", || a.is_cancelled());
    let a2 = env.next(CallKind::Transcribe);
    assert!(a2.record.video.as_ref().unwrap().ends_with("a.mp4"));
    assert_eq!(a2.record.options.as_ref().unwrap().language.as_deref(), Some("en"));
    assert_eq!(vm.queued_job_count(), 1, "B still waits");
    a2.finish();
    env.next(CallKind::Transcribe).finish();
}

#[test]
fn a_model_stays_protected_until_its_job_ends_whatever_else_is_stopped() {
    let (_l, env) = mac();
    env.settings.set_string(keys::SELECTED_MODEL, "base");
    env.settings.set_bool(keys::DIARIZE_SPEAKERS, false);
    let (vm, catalog) = env.vm();
    open(&env, &vm, "a.mp4");
    vm.generate();
    let a = env.next(CallKind::Transcribe);
    open(&env, &vm, "b.mp4");
    vm.generate();
    vm.cancel();
    assert_eq!(catalog.active_models().into_iter().collect::<Vec<_>>(), vec!["base".to_owned()]);
    a.finish();
    env.eventually("released", || catalog.active_models().is_empty());
}

#[test]
fn windows_switching_to_b_queues_b_and_switching_back_shows_live_progress() {
    let _l = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    env.settings.set_bool(keys::AUTO_SAVE_SUBTITLE, false);
    env.settings.set_string(keys::SELECTED_LANGUAGE, "tr");
    let (vm, _c) = env.vm();
    let a = open(&env, &vm, "A.mp4");
    vm.generate_and_translate();
    let t = env.next(CallKind::Transcribe);
    t.result("tr", "", vec![s(1, 0.0, 1.0, "Bir."), s(2, 1.0, 2.0, "İki.")]);
    let tr = env.next(CallKind::Translate);
    tr.stage(sa_core::engine_types::EngineStage::Translating, Some(0.45));
    env.eventually("progress", || vm.status_fraction() == Some(0.45));
    let b = open(&env, &vm, "B.mp4");
    vm.generate();
    assert_eq!(vm.status_text(), "Sırada");
    let row = vm.playlist().into_iter().find(|p| p.video_path == b).expect("B row");
    assert_eq!(row.state, PlaylistItemState::Pending);
    vm.open_video(Some(a.clone()));
    assert_eq!(vm.status_fraction(), Some(0.45), "background job keeps running and shows live progress");
    tr.translation("en", vec![s(1, 0.0, 1.0, "One."), s(2, 1.0, 2.0, "Two.")]);
    tr.finish();
    let tb = env.next(CallKind::Transcribe);
    assert_eq!(tb.record.video.as_deref(), Some(b.as_path()), "B starts automatically after A");
    vm.open_video(Some(b.clone()));
    tb.result("tr", "", vec![s(1, 0.0, 1.0, "Bee.")]);
    env.eventually("B finished", || vm.status() == JobStatus::Finished);
    assert_eq!(texts(&vm.sentences()), vec!["Bee."]);
}

#[test]
fn windows_single_jobs_appear_as_playlist_rows() {
    let _l = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    let (vm, _c) = env.vm();
    let a = open(&env, &vm, "a.mp4");
    vm.generate();
    let call = env.next(CallKind::Transcribe);
    env.eventually("row", || vm.playlist().iter().any(|p| p.video_path == a && p.state == PlaylistItemState::Working));
    call.result("tr", "", vec![s(1, 0.0, 1.0, "x")]);
    env.eventually("row completed", || vm.playlist().iter().any(|p| p.video_path == a && p.state == PlaylistItemState::Completed));
}
