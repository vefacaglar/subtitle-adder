//! Ports of `PlaylistTests` / the playlist part of `TranslationGateTests` (Swift) and the Windows playlist tests
//! (`PlaylistProcessesMultipleVideos…`, `PipelinedPlaylistOverlaps…`, `PlaylistEnforcesSingleConcurrentTranslationLimit`).

mod common;

use std::sync::atomic::Ordering;

use common::*;
use sa_core::l10n::{CatalogChain, UiLanguage};
use sa_core::settings::{SettingsStore, keys};
use sa_enginekit::events::EngineEvent;
use sa_viewmodels::SubtitleViewModel;
use sa_viewmodels::types::{JobStatus, PlaylistItemState, Workspace};

fn mac() -> (LanguageScope, Env) {
    let l = LanguageScope::new(CatalogChain::Mac, UiLanguage::Tr);
    (l, Env::mac())
}
fn win() -> (LanguageScope, Env) {
    let l = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    (l, Env::win())
}

fn states(vm: &SubtitleViewModel) -> Vec<PlaylistItemState> {
    vm.playlist().iter().map(|p| p.state).collect()
}

/// Answers every transcription with one line and every translation with `T:<text>`.
fn auto_engine(env: &Env) {
    env.engine.set_script(|r| match r.kind {
        CallKind::Transcribe => {
            let name = r.video.as_ref().unwrap().file_stem().unwrap().to_string_lossy().into_owned();
            Some(vec![Ok(EngineEvent::Result(sa_enginekit::events::EngineResult {
                language: r.options.as_ref().and_then(|o| o.language.clone()).unwrap_or_else(|| "en".into()),
                duration: 1.0,
                audio: String::new(),
                outputs: vec![],
                sentences: vec![s(1, 0.0, 1.0, &format!("Metin {name}"))],
            }))])
        }
        CallKind::Translate => Some(vec![Ok(EngineEvent::Translation {
            language: r.to.clone().unwrap(),
            sentences: r.sentences.iter().map(|x| s(x.id, x.start, x.end, &format!("T:{}", x.text))).collect(),
        })]),
        CallKind::Extract => None,
    });
}

#[test]
fn processes_videos_with_per_file_languages_and_saves_to_each_folder() {
    let (_l, env) = mac();
    env.settings.set_bool(keys::AUTO_SAVE_SUBTITLE, false);
    env.settings.set_string(keys::TRANSLATION_TARGET, "tr");
    auto_engine(&env);
    let (vm, _c) = env.vm();
    let a = env.touch("one/a.mp4", "v");
    let b = env.touch("two/b.mp4", "v");
    vm.add_playlist_videos(vec![a.clone(), b.clone()]);
    let items = vm.playlist();
    vm.set_playlist_item_language(items[0].id, "pt");
    vm.set_playlist_item_language(items[1].id, "ja");
    let langs: Vec<String> = vm.playlist().iter().map(|p| p.source_language.clone()).collect();
    assert_eq!(langs, vec!["pt", "ja"]);
    assert!(vm.can_start_playlist());
    vm.start_playlist();
    assert!(vm.has_active_work());
    assert!(!vm.is_working(), "the editor is not working");
    env.eventually("completed", || states(&vm) == vec![PlaylistItemState::Completed; 2]);
    assert_eq!(vm.playlist_summary_text(), "2 / 2 tamamlandı");
    assert!(a.with_extension("srt").exists() && b.with_extension("srt").exists(), "saves with auto-save off");
    let tr = env.engine.calls_of(CallKind::Translate);
    let froms: Vec<Option<String>> = tr.iter().map(|c| c.from.clone()).collect();
    assert_eq!(froms, vec![Some("pt".into()), Some("ja".into())]);
    env.eventually("idle", || !vm.has_active_work());
}

#[test]
fn windows_playlist_always_saves_and_completes() {
    let (_l, env) = win();
    env.settings.set_bool(keys::AUTO_SAVE_SUBTITLE, false);
    env.settings.set_string(keys::TRANSLATION_TARGET, "tr");
    auto_engine(&env);
    let (vm, _c) = env.vm();
    let a = env.touch("one/a.mp4", "v");
    let b = env.touch("two/b.mp4", "v");
    vm.add_playlist_videos_with_language(vec![a.clone(), b.clone()], "en");
    vm.start_playlist();
    env.eventually("completed", || states(&vm) == vec![PlaylistItemState::Completed; 2]);
    assert_eq!(vm.playlist_summary_text(), "2 / 2 tamamlandı");
    assert_eq!(std::fs::read_to_string(a.with_extension("srt")).unwrap(), "T:Metin a");
    assert!(b.with_extension("srt").exists());
}

#[test]
fn a_sidecar_skips_transcription() {
    let (_l, env) = mac();
    env.settings.set_string(keys::TRANSLATION_TARGET, "tr");
    env.settings.set_bool(keys::AUTO_SAVE_SUBTITLE, true);
    auto_engine(&env);
    let (vm, _c) = env.vm();
    let v = env.touch("f/clip.mp4", "v");
    env.touch("f/clip.srt", "1\n00:00:01,000 --> 00:00:02,000\nHello\n\n");
    vm.add_playlist_videos_with_language(vec![v.clone()], "en");
    vm.start_playlist();
    env.eventually("completed", || states(&vm) == vec![PlaylistItemState::Completed]);
    let kinds: Vec<CallKind> = env.engine.calls().iter().map(|c| c.kind).collect();
    assert_eq!(kinds, vec![CallKind::Translate]);
    assert_eq!(std::fs::read_to_string(v.with_extension("srt")).unwrap(), "T:Hello");
}

#[test]
fn a_failed_item_is_retried_by_the_next_run_not_again_by_this_one() {
    let (_l, env) = mac();
    env.settings.set_string(keys::TRANSLATION_TARGET, "tr");
    env.engine.set_script(|r| match r.kind {
        CallKind::Transcribe if r.video.as_ref().unwrap().ends_with("bad.mp4") => {
            Some(vec![Err(sa_core::engine_types::EngineError::Failed("boom".into()))])
        }
        CallKind::Transcribe => Some(vec![Ok(EngineEvent::Result(sa_enginekit::events::EngineResult {
            language: "en".into(),
            duration: 1.0,
            audio: String::new(),
            outputs: vec![],
            sentences: vec![s(1, 0.0, 1.0, "ok")],
        }))]),
        CallKind::Translate => Some(vec![Ok(EngineEvent::Translation { language: "tr".into(), sentences: vec![s(1, 0.0, 1.0, "tamam")] })]),
        _ => None,
    });
    let (vm, _c) = env.vm();
    let bad = env.touch("x/bad.mp4", "v");
    let good = env.touch("x/good.mp4", "v");
    vm.add_playlist_videos(vec![bad, good]);
    vm.start_playlist();
    env.eventually("done", || !vm.is_playlist_running());
    assert_eq!(states(&vm), vec![PlaylistItemState::Failed, PlaylistItemState::Completed]);
    assert_eq!(vm.playlist()[0].status_text(), "boom");
    assert_eq!(vm.playlist_summary_text(), "1 / 2 tamamlandı");
    assert_eq!(env.engine.calls_of(CallKind::Transcribe).len(), 2, "not retried by this run");
    assert!(vm.can_start_playlist(), "a failed item can run again");
}

#[test]
fn cancel_puts_the_current_item_back_to_queued_and_leaves_the_toolbar_alone() {
    let (_l, env) = mac();
    env.settings.set_string(keys::TRANSLATION_TARGET, "tr");
    let (vm, _c) = env.vm();
    let a = env.touch("x/a.mp4", "v");
    let b = env.touch("x/b.mp4", "v");
    vm.add_playlist_videos_with_language(vec![a, b], "en");
    vm.set_workspace(Workspace::Playlist);
    vm.start_playlist();
    let t = env.next(CallKind::Transcribe);
    vm.cancel();
    env.eventually("stopped", || !vm.is_playlist_running());
    env.eventually("killed", || t.is_cancelled());
    assert_eq!(states(&vm), vec![PlaylistItemState::Pending; 2]);
    assert!(vm.playlist().iter().all(|p| p.status_text() == "Sırada"), "{:?}", vm.playlist().iter().map(|p| p.status_text()).collect::<Vec<_>>());
    assert_eq!(env.settings.get_string(keys::SELECTED_LANGUAGE), None, "selectedLanguage stays untouched");
}

#[test]
fn a_missing_model_blocks_the_run_while_offline() {
    let (_l, env) = mac();
    env.settings.set_bool(keys::OFFLINE_MODE, true);
    env.settings.set_string(keys::TRANSLATION_TARGET, "tr");
    let mut models = downloaded_models();
    models.iter_mut().find(|m| m.id == "large-v3").unwrap().downloaded = false;
    *env.engine.models.lock() = models;
    auto_engine(&env);
    let (vm, _c) = env.vm();
    let a = env.touch("x/a.mp4", "v");
    vm.add_playlist_videos_with_language(vec![a], "en");
    vm.start_playlist();
    let n = vm.playlist_notice().expect("notice");
    assert_eq!(n.message.text(), "En doğru indirilmemiş. Çevrimdışı mod açıkken model indirilemez.");
    assert!(n.has_retry);
    assert_eq!(vm.status(), JobStatus::Idle, "video status idle");
    assert!(!vm.is_playlist_running());
    vm.go_online_and_retry_playlist();
    assert!(!vm.offline_mode());
    env.eventually("completed", || states(&vm) == vec![PlaylistItemState::Completed]);
}

#[test]
fn adding_skips_waiting_duplicates_but_allows_finished_ones() {
    let (_l, env) = mac();
    env.settings.set_string(keys::SELECTED_LANGUAGE, "de");
    let (vm, _c) = env.vm();
    let a = env.touch("x/a.mp4", "v");
    vm.add_playlist_videos(vec![a.clone()]);
    vm.add_playlist_videos(vec![a.clone()]);
    assert_eq!(vm.playlist().len(), 1);
    assert_eq!(vm.playlist()[0].source_language, "de", "default language = toolbar");
    vm.add_playlist_videos_with_language(vec![env.touch("x/b.mp4", "v")], "xx");
    assert_eq!(vm.playlist()[1].source_language, "", "unknown code → automatic");
}

#[test]
fn several_videos_or_any_video_on_the_playlist_go_to_the_playlist() {
    use sa_viewmodels::subtitle::{OpenPlan, plan_open};
    let (_l, env) = mac();
    let a = env.touch("x/a.mp4", "v");
    let b = env.touch("x/b.mkv", "v");
    let srt = env.touch("x/a.srt", "");
    assert_eq!(plan_open(&[a.clone(), b.clone()], false), OpenPlan::Playlist(vec![a.clone(), b.clone()]));
    assert_eq!(plan_open(&[a.clone()], true), OpenPlan::Playlist(vec![a.clone()]));
    assert_eq!(plan_open(&[a.clone(), srt.clone()], false), OpenPlan::Video { video: a.clone(), subtitle: Some(srt.clone()) });
    assert_eq!(plan_open(&[srt.clone()], false), OpenPlan::Subtitle { subtitle: srt, companion_video: Some(a.clone()) });
    assert_eq!(plan_open(&[env.path("x")], false), OpenPlan::Nothing, "folders ignored");
    let (vm, _c) = env.vm();
    vm.open_files(vec![a.clone(), b.clone()]);
    assert_eq!(vm.playlist().len(), 2);
}

#[test]
fn each_item_has_its_own_folder_even_with_the_same_file_name() {
    let (_l, env) = mac();
    let (vm, _c) = env.vm();
    let a = env.path("a/clip.mp4");
    let alias = env.path("b/../a/clip.mp4");
    let other = env.path("b/clip.mp4");
    assert_eq!(vm.playlist_out_directory(&a), vm.playlist_out_directory(&alias));
    assert_ne!(vm.playlist_out_directory(&a), vm.playlist_out_directory(&other));
    assert_eq!(vm.playlist_out_directory(&a).parent().unwrap(), env.shell.root.join("work").join("playlist"));
}

#[test]
fn extraction_recognition_and_translation_overlap_across_videos() {
    let (_l, env) = win();
    env.settings.set_string(keys::TRANSLATION_TARGET, "tr");
    env.engine.auto_complete_extract.store(false, Ordering::SeqCst);
    let (vm, _c) = env.vm();
    let videos: Vec<_> = (1..=3).map(|i| env.touch(&format!("p/{i}.mp4"), "v")).collect();
    vm.add_playlist_videos_with_language(videos.clone(), "en");
    vm.start_playlist();
    let e1 = env.next(CallKind::Extract);
    assert!(!env.engine.has_pending(CallKind::Extract), "exactly one extraction at a time");
    e1.result("?", "", vec![]);
    let t1 = env.next(CallKind::Transcribe);
    let e2 = env.next(CallKind::Extract);
    e2.result("?", "", vec![]);
    env.eventually("audio ready row", || {
        vm.playlist().get(1).is_some_and(|p| p.status_text() == "Ses hazır · Sırada" && p.fraction == Some(1.0) && p.is_audio_ready())
    });
    assert!(!env.engine.has_pending(CallKind::Extract), "lookahead of one");
    t1.result("en", "", vec![s(1, 0.0, 1.0, "one")]);
    let tr1 = env.next(CallKind::Translate);
    let t2 = env.next(CallKind::Transcribe);
    tr1.translation("tr", vec![s(1, 0.0, 1.0, "bir")]);
    tr1.finish();
    env.eventually("item 1 completed", || vm.playlist()[0].state == PlaylistItemState::Completed);
    assert_eq!(vm.playlist()[1].state, PlaylistItemState::Working, "translation of 1 finishes while 2 transcribes");
    let e3 = env.next(CallKind::Extract);
    e3.result("?", "", vec![]);
    t2.result("en", "", vec![s(1, 0.0, 1.0, "two")]);
    let tr2 = env.next(CallKind::Translate);
    let t3 = env.next(CallKind::Transcribe);
    t3.result("en", "", vec![s(1, 0.0, 1.0, "three")]);
    tr2.translation("tr", vec![s(1, 0.0, 1.0, "iki")]);
    tr2.finish();
    let tr3 = env.next(CallKind::Translate);
    tr3.translation("tr", vec![s(1, 0.0, 1.0, "üç")]);
    tr3.finish();
    env.eventually("all completed", || states(&vm) == vec![PlaylistItemState::Completed; 3]);
    assert_eq!(vm.playlist_summary_text(), "3 / 3 tamamlandı");
    assert_eq!(env.engine.peak_extract.load(Ordering::SeqCst), 1);
    assert_eq!(env.engine.peak_transcribe.load(Ordering::SeqCst), 1);
    assert_eq!(env.engine.peak_translate.load(Ordering::SeqCst), 1, "single concurrent translation");
}

#[test]
fn a_local_model_on_a_small_mac_translates_video_one_before_video_two_is_recognized() {
    let (_l, env) = mac();
    env.settings.set_string(keys::TRANSLATION_TARGET, "tr");
    env.shell.memory.store(8 * 1024 * 1024 * 1024, Ordering::SeqCst);
    let (vm, _c) = env.vm();
    vm.set_physical_memory(8 * 1024 * 1024 * 1024);
    let videos: Vec<_> = (1..=2).map(|i| env.touch(&format!("p/{i}.mp4"), "v")).collect();
    vm.add_playlist_videos_with_language(videos, "en");
    vm.start_playlist();
    let t1 = env.next(CallKind::Transcribe);
    t1.result("en", "", vec![s(1, 0.0, 1.0, "one")]);
    let tr1 = env.next(CallKind::Translate);
    env.pump();
    assert!(!env.engine.has_pending(CallKind::Transcribe), "no overlap below 24 GiB");
    tr1.translation("tr", vec![s(1, 0.0, 1.0, "bir")]);
    tr1.finish();
    let t2 = env.next(CallKind::Transcribe);
    t2.result("en", "", vec![s(1, 0.0, 1.0, "two")]);
    let tr2 = env.next(CallKind::Translate);
    tr2.translation("tr", vec![s(1, 0.0, 1.0, "iki")]);
    tr2.finish();
    env.eventually("completed", || states(&vm) == vec![PlaylistItemState::Completed; 2]);
}

#[test]
fn the_overlap_threshold_is_a_24_gib_mac() {
    assert_eq!(sa_viewmodels::PlatformPolicy::mac().playlist_overlap_requires_memory, Some(24 * 1024 * 1024 * 1024));
    assert_eq!(sa_viewmodels::PlatformPolicy::windows().playlist_overlap_requires_memory, None);
}

#[test]
fn a_playlist_run_never_takes_over_the_editor_the_toolbar_or_the_players_video() {
    let (_l, env) = mac();
    env.settings.set_string(keys::TRANSLATION_TARGET, "tr");
    let (vm, _c) = env.vm();
    let own = env.touch("own.mp4", "v");
    vm.open_video(Some(own.clone()));
    let x = env.touch("p/x.mp4", "v");
    vm.add_playlist_videos_with_language(vec![x.clone()], "en");
    vm.start_playlist();
    let t = env.next(CallKind::Transcribe);
    t.send(EngineEvent::Partial { language: "en".into(), sentences: vec![s(1, 0.0, 1.0, "Canlı x")] });
    env.pump();
    assert_eq!(vm.video(), Some(own.clone()), "editor untouched");
    vm.open_video(Some(x.clone()));
    env.eventually("live x", || texts(&vm.sentences()) == vec!["Canlı x"]);
    t.result("en", "", vec![s(1, 0.0, 1.0, "Metin x")]);
    let tr = env.next(CallKind::Translate);
    tr.translation("tr", vec![s(1, 0.0, 1.0, "Çeviri x")]);
    tr.finish();
    env.eventually("done", || !vm.is_playlist_running());
    assert_eq!(texts(&vm.sentences()), vec!["Metin x"]);
    assert_eq!(vm.status(), JobStatus::Finished);
}

#[test]
fn a_subtitle_is_refused_for_a_video_the_run_is_recognizing() {
    let (_l, env) = mac();
    env.settings.set_string(keys::TRANSLATION_TARGET, "tr");
    let (vm, _c) = env.vm();
    let x = env.touch("p/x.mp4", "v");
    vm.add_playlist_videos_with_language(vec![x.clone()], "en");
    vm.start_playlist();
    let t = env.next(CallKind::Transcribe);
    vm.open_video(Some(x.clone()));
    let sub = env.touch("p/other.srt", "1\n00:00:01,000 --> 00:00:02,000\nX\n\n");
    assert!(!vm.load_subtitle_file(&sub));
    assert!(vm.open_refusal().is_some());
    assert!(vm.pending_open().is_none());
    t.result("en", "", vec![s(1, 0.0, 1.0, "Metin")]);
    let tr = env.next(CallKind::Translate);
    tr.translation("tr", vec![s(1, 0.0, 1.0, "Çeviri")]);
    tr.finish();
    env.eventually("done", || !vm.is_playlist_running());
    assert_eq!(vm.playlist()[0].state, PlaylistItemState::Completed);
    assert_eq!(vm.saved_subtitle_name().as_deref(), Some("x.srt"));
}

#[test]
fn clear_and_remove_playlist_items() {
    let (_l, env) = win();
    let (vm, _c) = env.vm();
    let a = env.touch("x/a.mp4", "v");
    let b = env.touch("x/b.mp4", "v");
    vm.add_playlist_videos(vec![a, b]);
    let id = vm.playlist()[0].id;
    vm.remove_playlist_item(id);
    assert_eq!(vm.playlist().len(), 1);
    vm.clear_playlist();
    assert!(vm.playlist().is_empty());
    assert!(!vm.can_start_playlist());
}
