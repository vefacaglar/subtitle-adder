//! Ports of `ViewModelJobTests` (C#) and the macOS job/offline foundation tests.

mod common;

use std::sync::atomic::Ordering;

use common::*;
use sa_core::engine_types::{EngineError, EngineStage};
use sa_core::l10n::{CatalogChain, UiLanguage};
use sa_core::settings::{SettingsStore, keys};
use sa_viewmodels::types::{JobPurpose, JobStatus, JobStatusKind, stage_label};

fn open_video(env: &Env, vm: &sa_viewmodels::SubtitleViewModel, name: &str) -> std::path::PathBuf {
    let p = env.touch(name, "video");
    vm.open_video(Some(p.clone()));
    env.pump();
    p
}

#[test]
fn generate_runs_transcription_and_finishes() {
    let _l = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    env.settings.set_string(keys::SELECTED_LANGUAGE, "tr");
    env.settings.set_string(keys::SELECTED_MODEL, "small");
    env.settings.set_bool(keys::DIARIZE_SPEAKERS, true);
    env.settings.set_int(keys::SPEAKER_COUNT, 3);
    let (vm, catalog) = env.vm();
    let video = open_video(&env, &vm, "clip.mp4");
    assert!(vm.can_generate());
    let loads_before = env.engine.models_calls.load(Ordering::SeqCst);
    vm.generate();
    assert_eq!(vm.status_text(), "Başlıyor…");
    assert!(vm.is_working());
    assert!(!vm.can_generate());
    assert!(!vm.can_translate());
    let active = catalog.active_models();
    assert!(active.contains("small") && active.contains("speaker-diarization") && active.len() == 2, "{active:?}");
    let call = env.next(CallKind::Transcribe);
    assert_eq!(call.record.video.as_deref(), Some(video.as_path()));
    assert_eq!(call.record.out_dir.as_deref(), Some(env.shell.root.join("work").as_path()));
    let o = call.record.options.clone().expect("options");
    assert_eq!(o.language.as_deref(), Some("tr"));
    assert_eq!(o.model, "small");
    assert!(o.diarize);
    assert_eq!(o.speakers, Some(3));
    call.stage(EngineStage::Transcribing, Some(0.5));
    env.eventually("progress", || vm.status_fraction() == Some(0.5));
    assert_eq!(vm.status_text(), "Konuşma metne çevriliyor… 50%");
    let wav = env.touch("work/clip.wav", "RIFF");
    call.result("tr", &wav.to_string_lossy(), vec![s(1, 0.0, 1.0, "Merhaba."), s(2, 1.0, 2.0, "Nasılsın?")]);
    env.eventually("finished", || vm.status() == JobStatus::Finished);
    assert_eq!(vm.status_text(), "2 satır · dil: tr");
    assert_eq!(vm.detected_language().as_deref(), Some("tr"));
    assert!(wav.exists(), "Windows keeps the WAV until exit");
    env.eventually("catalog reloaded once", || env.engine.models_calls.load(Ordering::SeqCst) == loads_before + 1);
    assert!(catalog.active_models().is_empty());
    assert!(vm.can_translate());
    vm.shutdown();
    assert!(!wav.exists(), "shutdown deletes the session temp files");
}

#[test]
fn mac_deletes_the_wav_as_soon_as_the_result_arrives() {
    let _l = LanguageScope::new(CatalogChain::Mac, UiLanguage::Tr);
    let env = Env::mac();
    let (vm, _c) = env.vm();
    open_video(&env, &vm, "clip.mp4");
    vm.generate();
    let call = env.next(CallKind::Transcribe);
    let wav = env.touch("work/clip.wav", "RIFF");
    call.result("tr", &wav.to_string_lossy(), vec![s(1, 0.0, 1.0, "Merhaba.")]);
    env.eventually("finished", || vm.status() == JobStatus::Finished);
    env.eventually("wav deleted", || !wav.exists());
    assert_eq!(vm.status_text(), "1 satır · dil: tr");
}

#[test]
fn automatic_language_uses_detected_language_as_translation_source() {
    let _l = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    env.settings.set_bool(keys::DIARIZE_SPEAKERS, false);
    env.settings.set_string(keys::TRANSLATION_TARGET, "en");
    let (vm, catalog) = env.vm();
    open_video(&env, &vm, "a.mp4");
    vm.generate();
    let call = env.next(CallKind::Transcribe);
    assert_eq!(call.record.options.as_ref().unwrap().language, None);
    assert!(!call.record.options.as_ref().unwrap().diarize);
    let active = catalog.active_models();
    assert_eq!(active.into_iter().collect::<Vec<_>>(), vec!["large-v3".to_owned()]);
    call.result("en", "", vec![s(1, 0.0, 1.0, "Hello.")]);
    env.eventually("finished", || vm.status() == JobStatus::Finished);
    assert_eq!(vm.translation_source().as_deref(), Some("en"));
    assert_eq!(vm.translation_target(), "tr");
}

#[test]
fn download_labels_tell_transcription_and_diarization_apart() {
    let _l = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    let mut models = sa_core::models::fallback_models();
    for m in &mut models {
        m.downloaded = false;
    }
    *env.engine.models.lock() = models;
    env.settings.set_bool(keys::DIARIZE_SPEAKERS, true);
    let (vm, _c) = env.vm();
    open_video(&env, &vm, "a.mp4");
    vm.generate();
    let call = env.next(CallKind::Transcribe);
    call.stage(EngineStage::DownloadingModel, Some(0.25));
    env.eventually("download 25", || vm.status_text() == "Model indiriliyor… %25");
    call.stage(EngineStage::DownloadingModel, Some(1.0));
    call.stage(EngineStage::DownloadingModel, Some(0.0));
    env.eventually("diarization download", || vm.status_text() == "Konuşmacı ayırma modeli indiriliyor… %0");
    vm.cancel();
}

#[test]
fn engine_error_fails_the_job() {
    let _l = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    let (vm, catalog) = env.vm();
    open_video(&env, &vm, "a.mp4");
    let loads = env.engine.models_calls.load(Ordering::SeqCst);
    vm.generate();
    let call = env.next(CallKind::Transcribe);
    call.fail(EngineError::Failed("boom".into()));
    env.eventually("failed", || vm.status().kind() == JobStatusKind::Failed);
    assert_eq!(vm.status_text(), "boom");
    assert!(catalog.active_models().is_empty());
    env.pump();
    assert_eq!(env.engine.models_calls.load(Ordering::SeqCst), loads, "no catalog reload");
}

#[test]
fn cancel_stops_the_stream_and_ignores_late_events() {
    let _l = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    let (vm, catalog) = env.vm();
    open_video(&env, &vm, "a.mp4");
    vm.generate();
    let call = env.next(CallKind::Transcribe);
    vm.cancel();
    assert_eq!(vm.status(), JobStatus::Idle);
    assert!(catalog.active_models().is_empty());
    env.eventually("stream cancelled", || call.is_cancelled());
    call.result("tr", "", vec![s(1, 0.0, 1.0, "late")]);
    env.pump();
    assert!(vm.sentences().is_empty());
    assert_eq!(vm.status(), JobStatus::Idle);
}

#[test]
fn new_job_makes_the_old_one_stale() {
    let _l = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    let (vm, _c) = env.vm();
    open_video(&env, &vm, "a.mp4");
    vm.generate();
    let old = env.next(CallKind::Transcribe);
    vm.cancel();
    vm.generate();
    let new = env.next(CallKind::Transcribe);
    old.result("tr", "", vec![s(1, 0.0, 1.0, "old")]);
    env.pump();
    assert!(vm.sentences().is_empty());
    assert!(vm.is_working());
    new.result("tr", "", vec![s(1, 0.0, 1.0, "new")]);
    env.eventually("new result", || texts(&vm.sentences()) == vec!["new"]);
}

#[test]
fn open_video_resets_everything() {
    let _l = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    let (vm, _c) = env.vm();
    let a = open_video(&env, &vm, "a.mp4");
    vm.generate();
    let call = env.next(CallKind::Transcribe);
    call.result("tr", "", vec![s(1, 0.0, 1.0, "Bir."), s(2, 1.0, 2.0, "İki.")]);
    env.eventually("finished", || vm.status() == JobStatus::Finished);
    let uid = vm.sentences()[0].uid;
    vm.set_selection(Some(uid));
    vm.set_text(uid, "Değişti.");
    assert!(vm.can_undo());
    vm.open_video(Some(a.clone()));
    assert_eq!(vm.sentences().len(), 2, "same path is a no-op");
    open_video(&env, &vm, "b.mp4");
    assert!(vm.sentences().is_empty());
    assert!(vm.selection().is_none());
    assert!(vm.detected_language().is_none());
    assert!(!vm.can_undo());
    assert_eq!(vm.status(), JobStatus::Idle);
    assert_eq!(vm.window_title(), "b.mp4");
}

#[test]
fn generate_without_engine_fails() {
    let l = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let mut env = Env::win();
    env.with_engine = false;
    let (vm, _c) = env.vm();
    open_video(&env, &vm, "a.mp4");
    vm.generate();
    assert_eq!(vm.status().kind(), JobStatusKind::Failed);
    let tr = vm.status_text();
    l.set(UiLanguage::En);
    let en = vm.status_text();
    assert_ne!(tr, en, "re-localizes after a language switch");
    assert!(!en.is_empty());
}

#[test]
fn idle_hint_depends_on_video_and_lines() {
    let _l = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    let (vm, _c) = env.vm();
    assert_eq!(vm.status_text(), "Hazır");
    open_video(&env, &vm, "a.mp4");
    assert_eq!(vm.status_text(), "Altyazı çıkarmak için sağ üstteki “Altyazı üret”e bas (Ctrl+Enter)");
}

#[test]
fn idle_hint_mac_uses_the_command_key() {
    let _l = LanguageScope::new(CatalogChain::Mac, UiLanguage::Tr);
    let env = Env::mac();
    let (vm, _c) = env.vm();
    open_video(&env, &vm, "a.mp4");
    assert_eq!(vm.status_text(), "Altyazı çıkarmak için sağ üstteki “Altyazı üret”e bas (⌘↩)");
}

#[test]
fn language_switch_re_raises_all_properties() {
    let l = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    let (vm, _c) = env.vm();
    let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let s2 = seen.clone();
    vm.on_change(Box::new(move |c| s2.borrow_mut().push(c)));
    l.set(UiLanguage::En);
    env.eventually("All raised", || seen.borrow().contains(&sa_viewmodels::types::SubtitleChange::All));
    assert_eq!(vm.status_text(), "Ready");
}

#[test]
fn stage_labels() {
    let l = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    use EngineStage::*;
    use JobPurpose::*;
    let t = |st, f, p| stage_label(st, f, p).text();
    assert_eq!(t(ExtractingAudio, None, Transcription), "Ses ayrılıyor…");
    assert_eq!(t(ExtractingAudio, Some(0.5), Transcription), "Ses ayrılıyor… 50%");
    assert_eq!(t(DownloadingModel, Some(1.4), Translation), "Çeviri modeli indiriliyor… %100");
    assert_eq!(t(DownloadingModel, None, Transcription), "Model indiriliyor…");
    assert_eq!(t(LoadingModel, None, Translation), "Çeviri modeli yükleniyor…");
    assert_eq!(t(LoadingModel, None, Transcription), "Model yükleniyor…");
    assert_eq!(t(Diarizing, Some(0.425), Diarization), "Konuşmacılar ayrılıyor… %43");
    assert_eq!(t(Segmenting, None, Transcription), "Cümleler çıkarılıyor…");
    assert_eq!(t(Translating, Some(0.42), Translation), "Çevriliyor… %42");
    assert_eq!(t(Unknown, None, Transcription), "Çalışıyor…");
    assert_eq!(stage_label(Translating, Some(0.5), Translation), stage_label(Translating, Some(0.5), Translation));
    assert_ne!(stage_label(Translating, Some(0.5), Translation), stage_label(Translating, Some(0.6), Translation));
    l.set(UiLanguage::En);
    assert_eq!(t(Translating, Some(0.42), Translation), "Translating… 42%");
}

#[test]
fn mac_stage_percents_round_and_clamp() {
    let _l = LanguageScope::new(CatalogChain::Mac, UiLanguage::Tr);
    assert_eq!(sa_viewmodels::types::percent(0.456), 46);
    assert_eq!(sa_viewmodels::types::percent(0.999), 100);
    assert_eq!(sa_viewmodels::types::percent(1.7), 100);
    assert_eq!(stage_label(EngineStage::ExtractingAudio, Some(0.5), JobPurpose::Transcription).text(), "Ses ayrılıyor… %50");
}

#[test]
fn offline_missing_model_blocks_generate_and_going_online_retries() {
    let l = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    env.settings.set_bool(keys::OFFLINE_MODE, true);
    env.settings.set_bool(keys::DIARIZE_SPEAKERS, false);
    let (vm, _c) = env.vm();
    assert!(vm.offline_mode());
    open_video(&env, &vm, "a.mp4");
    vm.generate();
    assert_eq!(vm.status().kind(), JobStatusKind::Failed);
    assert!(vm.can_go_online_and_retry());
    assert!(!env.engine.has_pending(CallKind::Transcribe));
    l.set(UiLanguage::En);
    assert!(vm.status_text().contains("offline mode"), "{}", vm.status_text());
    vm.go_online_and_retry();
    assert!(!vm.offline_mode());
    assert_eq!(env.settings.get_bool(keys::OFFLINE_MODE), Some(false));
    let call = env.next(CallKind::Transcribe);
    call.result("tr", "", vec![s(1, 0.0, 1.0, "x")]);
    env.eventually("finished", || vm.status() == JobStatus::Finished);
}

#[test]
fn missing_diarization_model_counts_only_when_diarizing() {
    let _l = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    let mut models = downloaded_models();
    models.iter_mut().find(|m| m.id == "speaker-diarization").unwrap().downloaded = false;
    *env.engine.models.lock() = models;
    env.settings.set_bool(keys::OFFLINE_MODE, true);
    env.settings.set_bool(keys::DIARIZE_SPEAKERS, false);
    let (vm, _c) = env.vm();
    open_video(&env, &vm, "a.mp4");
    vm.generate();
    let call = env.next(CallKind::Transcribe);
    call.result("tr", "", vec![s(1, 0.0, 1.0, "x")]);
    env.eventually("finished", || vm.status() == JobStatus::Finished);
    vm.set_diarize(true);
    vm.generate();
    assert_eq!(vm.status().kind(), JobStatusKind::Failed);
    assert!(vm.status_text().contains("indirilmemiş"), "{}", vm.status_text());
}

#[test]
fn missing_translation_model_blocks_translate() {
    let _l = LanguageScope::new(CatalogChain::Mac, UiLanguage::Tr);
    let env = Env::mac();
    let mut models = downloaded_models();
    models.iter_mut().find(|m| m.id == "madlad-3b").unwrap().downloaded = false;
    *env.engine.models.lock() = models;
    env.settings.set_bool(keys::OFFLINE_MODE, true);
    env.settings.set_string(keys::SELECTED_LANGUAGE, "tr");
    let (vm, _c) = env.vm();
    let sub = env.touch("Film.srt", "1\n00:00:01,000 --> 00:00:02,000\nMerhaba\n\n");
    assert!(vm.load_subtitle_file(&sub));
    vm.translate();
    assert_eq!(vm.status_text(), "MADLAD-400 3B indirilmemiş. Çevrimdışı mod açıkken model indirilemez.");
    assert!(vm.can_go_online_and_retry());
    // A later status clears the retry.
    vm.set_offline_mode(false);
    vm.translate();
    let call = env.next(CallKind::Translate);
    assert!(!vm.can_go_online_and_retry());
    call.translation("en", vec![s(1, 1.0, 2.0, "Hello")]);
    call.finish();
    env.eventually("finished", || vm.status() == JobStatus::Finished);
}

#[test]
fn other_failures_offer_no_retry() {
    let _l = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    let (vm, _c) = env.vm();
    open_video(&env, &vm, "a.mp4");
    vm.generate();
    env.next(CallKind::Transcribe).fail(EngineError::Failed("x".into()));
    env.eventually("failed", || vm.status().kind() == JobStatusKind::Failed);
    assert!(!vm.can_go_online_and_retry());
}

#[test]
fn nothing_is_blocked_when_online_downloaded_unknown_or_not_yet_loaded() {
    let _l = LanguageScope::new(CatalogChain::Mac, UiLanguage::Tr);
    let env = Env::mac();
    *env.engine.models.lock() = downloaded_models();
    env.settings.set_bool(keys::OFFLINE_MODE, true);
    env.settings.set_string(keys::SELECTED_MODEL, "custom-unknown");
    let (vm, _c) = env.vm();
    // An unknown id is reconciled away only when the language filter excludes it; the catalog keeps unknown
    // models out of the offline check.
    open_video(&env, &vm, "a.mp4");
    vm.generate();
    let call = env.next(CallKind::Transcribe);
    call.finish();
}

#[test]
fn quitting_is_held_off_by_a_background_job_even_when_the_video_on_screen_is_idle() {
    let _l = LanguageScope::new(CatalogChain::Mac, UiLanguage::Tr);
    let env = Env::mac();
    let (vm, _c) = env.vm();
    open_video(&env, &vm, "a.mp4");
    vm.generate();
    let call = env.next(CallKind::Transcribe);
    open_video(&env, &vm, "b.mp4");
    assert!(vm.has_active_work());
    assert!(!vm.is_working());
    call.result("tr", "", vec![s(1, 0.0, 1.0, "A")]);
    env.eventually("done", || !vm.has_active_work());
}
