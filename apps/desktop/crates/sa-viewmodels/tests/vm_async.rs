//! Ports of `ExternalLLMTests` (Swift), `DiarizationTranslationTests` and
//! `ExternalAsyncModeIsOnlyActiveWithExternalLlm…` (C#): background pre-translation through an external server.

mod common;

use common::*;
use sa_core::engine_types::{EngineStage, Sentence};
use sa_core::l10n::UiLanguage;
use sa_core::settings::{SettingsStore, keys};
use sa_enginekit::events::EngineEvent;
use sa_providers::ProviderStore;
use sa_viewmodels::SubtitleViewModel;
use sa_viewmodels::types::JobStatus;

fn lines(n: i64) -> Vec<Sentence> {
    (1..=n).map(|i| s(i, i as f64, i as f64 + 1.0, &format!("Satır numarası {i}."))).collect()
}

fn answer(call: &LiveCall, lang: &str) {
    let out = call.record.sentences.iter().map(|x| s(x.id, x.start, x.end, &format!("Line {}", x.id))).collect();
    call.translation(lang, out);
    call.finish();
}

fn setup(policy: sa_viewmodels::PlatformPolicy, async_on: bool) -> (LanguageScope, Env, SubtitleViewModel) {
    let l = LanguageScope::new(policy.catalog_chain, UiLanguage::Tr);
    let env = Env::new(policy);
    env.settings.set_string(keys::SELECTED_LANGUAGE, "tr");
    env.settings.set_bool(keys::AUTO_SAVE_SUBTITLE, false);
    env.settings.set_bool(keys::DIARIZE_SPEAKERS, false);
    env.providers.add(provider("lan", "LAN", "http://10.0.0.2:1234", "gemma", async_on), None, true);
    let (vm, _c) = env.vm();
    (l, env, vm)
}

#[test]
fn external_async_mode_is_only_active_with_an_external_llm() {
    let (_l, env, vm) = setup(sa_viewmodels::PlatformPolicy::windows(), true);
    assert!(vm.is_external_llm_active());
    assert!(vm.is_external_async_active());
    let id = env.providers.active_provider_id().unwrap();
    env.providers.set_active(None);
    env.eventually("deactivated", || !vm.is_external_llm_active());
    assert!(!vm.is_external_async_active());
    env.providers.set_active(Some(id));
    env.eventually("active", || vm.is_external_async_active());
}

#[test]
fn async_mode_translates_finished_lines_while_transcribing_then_only_the_rest() {
    for policy in [sa_viewmodels::PlatformPolicy::windows(), sa_viewmodels::PlatformPolicy::mac()] {
        let (_l, env, vm) = setup(policy, true);
        vm.open_video(Some(env.touch("a.mp4", "v")));
        vm.generate_and_translate();
        let t = env.next(CallKind::Transcribe);
        t.send(EngineEvent::Partial { language: "tr".into(), sentences: lines(14) });
        let first = env.next(CallKind::Translate);
        assert_eq!(ids(&first.record.sentences), (1..=14).collect::<Vec<_>>(), "12 stable lines plus 2 lines of context");
        env.eventually("async flag", || vm.is_async_translating());
        answer(&first, "en");
        env.eventually("12 applied", || vm.translations().len() == 12);
        let held: Vec<i64> = vm.sentences().iter().filter(|x| vm.translation_for(x.uid).is_none()).map(|x| x.id).collect();
        assert_eq!(held, vec![13, 14], "the two unstable lines are context only, never stored");
        t.result("tr", "", lines(14));
        let rest = env.next(CallKind::Translate);
        assert_eq!(ids(&rest.record.sentences), vec![11, 12, 13, 14], "only the rest, with two lines of context");
        answer(&rest, "en");
        env.eventually("finished", || vm.status() == JobStatus::Finished && !vm.is_working());
        assert_eq!(vm.translations().len(), 14);
        assert!(!vm.is_async_translating());
        assert_eq!(env.engine.calls_of(CallKind::Translate).len(), 2, "no extra pass: everything is reusable");
    }
}

#[test]
fn pending_text_translates_while_diarization_is_still_running() {
    for n in [1i64, 8, 14] {
        let (_l, env, vm) = setup(sa_viewmodels::PlatformPolicy::windows(), true);
        vm.set_diarize(true);
        vm.open_video(Some(env.touch("a.mp4", "v")));
        vm.generate_and_translate();
        let t = env.next(CallKind::Transcribe);
        t.send(EngineEvent::Partial { language: "tr".into(), sentences: lines(n) });
        env.pump();
        if n < 12 {
            assert!(!env.engine.has_pending(CallKind::Translate), "{n}: nothing before diarizing");
        } else {
            let b = env.next(CallKind::Translate);
            assert_eq!(b.record.sentences.len(), 14, "12 stable lines plus context");
            answer(&b, "en");
            env.eventually("applied", || vm.translations().len() == 12);
        }
        t.stage(EngineStage::Diarizing, None);
        let drain = env.next(CallKind::Translate);
        let requested: Vec<i64> = ids(&drain.record.sentences);
        assert!(requested.contains(&n), "{n}: the tail drains while diarizing: {requested:?}");
        answer(&drain, "en");
        env.eventually("all translated", || vm.translations().len() == n as usize);
        t.result("tr", "", lines(n));
        env.eventually("finished", || vm.status() == JobStatus::Finished && !vm.is_working());
        assert_eq!(vm.translations().len(), n as usize);
    }
}

#[test]
fn without_async_mode_nothing_is_translated_before_the_result() {
    let (_l, env, vm) = setup(sa_viewmodels::PlatformPolicy::mac(), false);
    vm.open_video(Some(env.touch("a.mp4", "v")));
    vm.generate_and_translate();
    let t = env.next(CallKind::Transcribe);
    t.send(EngineEvent::Partial { language: "tr".into(), sentences: lines(14) });
    env.pump();
    assert!(!env.engine.has_pending(CallKind::Translate));
    t.result("tr", "", lines(14));
    let all = env.next(CallKind::Translate);
    assert_eq!(all.record.sentences.len(), 14);
    answer(&all, "en");
    env.eventually("finished", || vm.status() == JobStatus::Finished);
}

#[test]
fn empty_lines_dont_count_towards_the_batch() {
    let (_l, env, vm) = setup(sa_viewmodels::PlatformPolicy::mac(), true);
    vm.open_video(Some(env.touch("a.mp4", "v")));
    vm.generate_and_translate();
    let t = env.next(CallKind::Transcribe);
    let mut ls = lines(12);
    for x in ls.iter_mut().take(3) {
        x.text = "  ".into();
    }
    t.send(EngineEvent::Partial { language: "tr".into(), sentences: ls });
    env.pump();
    assert!(!env.engine.has_pending(CallKind::Translate), "7 non-empty stable lines are not enough");
    t.finish();
}

#[test]
fn stopping_cancels_the_background_translation() {
    let (_l, env, vm) = setup(sa_viewmodels::PlatformPolicy::mac(), true);
    vm.open_video(Some(env.touch("a.mp4", "v")));
    vm.generate_and_translate();
    let t = env.next(CallKind::Transcribe);
    t.send(EngineEvent::Partial { language: "tr".into(), sentences: lines(14) });
    let b = env.next(CallKind::Translate);
    vm.cancel();
    env.eventually("both cancelled", || b.is_cancelled() && t.is_cancelled());
    env.eventually("idle", || vm.status() == JobStatus::Idle);
    assert!(!vm.is_async_translating());
}

#[test]
fn background_translation_never_lands_on_a_line_edited_since_the_batch_was_sent() {
    let (_l, env, vm) = setup(sa_viewmodels::PlatformPolicy::mac(), true);
    vm.open_video(Some(env.touch("a.mp4", "v")));
    vm.generate_and_translate();
    let t = env.next(CallKind::Transcribe);
    t.send(EngineEvent::Partial { language: "tr".into(), sentences: lines(14) });
    let b = env.next(CallKind::Translate);
    env.eventually("lines", || vm.sentences().len() == 14);
    let edited = vm.sentences()[0].uid;
    vm.set_text(edited, "Elle değişti.");
    answer(&b, "en");
    env.eventually("applied", || vm.translations().len() >= 11);
    assert!(vm.translation_for(edited).is_none_or(|e| e.source_text == "Elle değişti."), "never a stale translation on an edited line");
    t.finish();
}

#[test]
fn a_failed_batch_stops_background_translation_for_the_rest_of_the_run() {
    let (_l, env, vm) = setup(sa_viewmodels::PlatformPolicy::mac(), true);
    vm.open_video(Some(env.touch("a.mp4", "v")));
    vm.generate_and_translate();
    let t = env.next(CallKind::Transcribe);
    t.send(EngineEvent::Partial { language: "tr".into(), sentences: lines(14) });
    let b = env.next(CallKind::Translate);
    b.fail(sa_core::engine_types::EngineError::Failed("down".into()));
    env.eventually("flag off", || !vm.is_async_translating());
    t.send(EngineEvent::Partial { language: "tr".into(), sentences: lines(30) });
    env.pump();
    assert!(!env.engine.has_pending(CallKind::Translate), "no more background batches");
    let diag: Vec<String> = env.diagnostics.lines().into_iter().map(|(_, m)| m).collect();
    assert!(diag.iter().any(|m| m.contains("background batch of 12 lines failed")), "{diag:?}");
    assert!(!diag.iter().any(|m| m.contains("Satır numarası")), "no line text logged");
    t.result("tr", "", lines(14));
    let all = env.next(CallKind::Translate);
    assert_eq!(all.record.sentences.len(), 14, "the closing pass translates everything");
    answer(&all, "en");
    env.eventually("finished", || vm.status() == JobStatus::Finished);
}

#[test]
fn a_background_job_snapshots_its_settings_at_request_time() {
    let (_l, env, vm) = setup(sa_viewmodels::PlatformPolicy::windows(), true);
    vm.set_translation_target("es");
    vm.open_video(Some(env.touch("a.mp4", "v")));
    vm.generate_and_translate();
    let t = env.next(CallKind::Transcribe);
    vm.set_translation_target("de");
    t.send(EngineEvent::Partial { language: "tr".into(), sentences: lines(14) });
    let b = env.next(CallKind::Translate);
    assert_eq!(b.record.to.as_deref(), Some("es"), "UNIFIED.md §3.7 FIX-W: request-time snapshot");
    b.finish();
    t.finish();
}
