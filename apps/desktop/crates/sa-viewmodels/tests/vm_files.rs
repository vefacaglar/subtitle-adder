//! Ports of `SubtitleFilesTests`, `OpenConfirmationTests` / `OpeningAndLayoutTests` (Swift) and the Windows
//! sidecar/companion tests.

mod common;

use common::*;
use sa_core::appearance::SubtitleDisplay;
use sa_core::engine_types::SubtitleFormat;
use sa_core::l10n::{CatalogChain, UiLanguage};
use sa_core::settings::{SettingsStore, keys};
use sa_viewmodels::SubtitleViewModel;
use sa_viewmodels::types::{ExportLanguage, JobStatus, JobStatusKind, Workspace};

const SRT: &str = "1\n00:00:01,000 --> 00:00:02,000\nBir\n\n2\n00:00:03,000 --> 00:00:04,000\nİki\n\n";

fn mac() -> (LanguageScope, Env) {
    let l = LanguageScope::new(CatalogChain::Mac, UiLanguage::Tr);
    (l, Env::mac())
}

fn translate_all(env: &Env, vm: &SubtitleViewModel, lang: &str) {
    vm.translate();
    let call = env.next(CallKind::Translate);
    let out = call.record.sentences.iter().map(|x| s(x.id, x.start, x.end, &format!("{lang}:{}", x.text))).collect();
    call.translation(lang, out);
    call.finish();
    env.eventually("translated", || vm.status() == JobStatus::Finished && !vm.is_working());
}

#[test]
fn import_keeps_the_video_and_resets_translations_and_display() {
    let (_l, env) = mac();
    env.settings.set_bool(keys::AUTO_SAVE_SUBTITLE, false);
    env.settings.set_string(keys::SELECTED_LANGUAGE, "tr");
    let (vm, _c) = env.vm();
    let v = env.touch("Film.mp4", "v");
    vm.open_video(Some(v.clone()));
    let sub = env.touch("other.srt", SRT);
    assert!(vm.load_subtitle_file(&sub));
    translate_all(&env, &vm, "en");
    assert_eq!(vm.display_mode(), SubtitleDisplay::Translation);
    let sub2 = env.touch("third.srt", SRT);
    assert!(vm.load_subtitle_file(&sub2));
    assert_eq!(vm.video(), Some(v), "the video stays");
    assert!(!vm.has_translations());
    assert_eq!(vm.effective_display_mode(), SubtitleDisplay::Original);
    assert_eq!(vm.loaded_subtitle(), Some(sub2));
}

#[test]
fn import_without_valid_lines_fails_and_keeps_the_current_lines() {
    let (_l, env) = mac();
    let (vm, _c) = env.vm();
    let sub = env.touch("ok.srt", SRT);
    assert!(vm.load_subtitle_file(&sub));
    let bad = env.touch("bad.srt", "nothing here");
    assert!(!vm.load_subtitle_file(&bad));
    assert_eq!(vm.status_text(), "Altyazı dosyasında geçerli satır bulunamadı.");
    assert_eq!(vm.sentences().len(), 2);
}

#[test]
fn imported_file_cancels_a_running_job() {
    let (_l, env) = mac();
    let (vm, _c) = env.vm();
    vm.open_video(Some(env.touch("a.mp4", "v")));
    vm.generate();
    let call = env.next(CallKind::Transcribe);
    let sub = env.touch("x.srt", SRT);
    assert!(vm.load_subtitle_file(&sub));
    env.eventually("cancelled", || call.is_cancelled());
    assert!(!vm.is_working());
    assert_eq!(vm.sentences().len(), 2);
}

#[test]
fn opening_a_video_loads_its_sidecar_only() {
    let (_l, env) = mac();
    let (vm, _c) = env.vm();
    env.touch("m/clip.tr.srt", SRT);
    let v = env.touch("m/clip.mp4", "v");
    vm.open_video(Some(v));
    env.pump();
    assert!(vm.sentences().is_empty(), "`clip.tr.srt` alone is not a sidecar");
    env.touch("m/film.srt", SRT);
    vm.open_video(Some(env.touch("m/film.mkv", "v")));
    env.eventually("sidecar", || vm.sentences().len() == 2);
}

#[test]
fn drop_routes_subtitles_and_videos_by_extension() {
    let (_l, env) = mac();
    let (vm, _c) = env.vm();
    let v = env.touch("d/Film.mkv", "v");
    let sub = env.touch("d/Film.tr.srt", SRT);
    vm.set_workspace(Workspace::Platform);
    vm.open_files(vec![sub.clone()]);
    env.pump();
    assert_eq!(vm.video(), Some(v.clone()), "companion video found");
    assert_eq!(vm.sentences().len(), 2);
    assert_eq!(vm.loaded_subtitle(), Some(sub));
    assert_eq!(vm.workspace(), Workspace::Editor, "the platform tab switches to the editor");
}

#[test]
fn windows_single_dropped_video_switches_to_the_player_tab() {
    let _l = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    let (vm, _c) = env.vm();
    vm.set_workspace(Workspace::Platform);
    let v = env.touch("a.mp4", "v");
    vm.open_files(vec![v.clone()]);
    assert_eq!(vm.workspace(), Workspace::Editor, "UNIFIED.md §3.3 FIX-W");
    assert_eq!(vm.video(), Some(v));
}

#[test]
fn saved_files_never_carry_a_language_suffix_but_export_suggestions_do() {
    let (_l, env) = mac();
    env.settings.set_bool(keys::AUTO_SAVE_SUBTITLE, false);
    env.settings.set_string(keys::SELECTED_LANGUAGE, "tr");
    let (vm, _c) = env.vm();
    let v = env.touch("Film.mp4", "v");
    vm.open_video(Some(v.clone()));
    assert!(vm.load_subtitle_file(&env.touch("x.srt", SRT)));
    vm.set_translation_target("es");
    translate_all(&env, &vm, "es");
    assert_eq!(vm.video_subtitle_path(), Some(v.with_extension("srt")), "saved under the video's name, no language");
    vm.set_export_format(SubtitleFormat::Vtt);
    assert_eq!(vm.export_file_name(), "Film.es.vtt");
}

#[test]
fn save_writes_under_the_videos_name_without_bom_and_overwrites() {
    let (_l, env) = mac();
    env.settings.set_bool(keys::AUTO_SAVE_SUBTITLE, false);
    let (vm, _c) = env.vm();
    let v = env.touch("Film.mp4", "v");
    vm.open_video(Some(v.clone()));
    vm.generate();
    env.next(CallKind::Transcribe).result("tr", "", vec![s(1, 0.0, 1.0, "Bir"), s(2, 1.0, 2.0, "İki")]);
    env.eventually("done", || vm.status() == JobStatus::Finished);
    let out = v.with_extension("srt");
    std::fs::write(&out, "old").unwrap();
    vm.save_subtitle_next_to_video();
    env.eventually("saved", || std::fs::read_to_string(&out).unwrap() == "Bir|İki");
    let bytes = std::fs::read(&out).unwrap();
    assert!(!bytes.starts_with(&[0xEF, 0xBB, 0xBF]), "no BOM");
    env.eventually("status", || vm.status_text().ends_with(" · kaydedildi: Film.srt"));
    assert!(!vm.has_unsaved_edits());
}

#[test]
fn save_without_video_uses_the_loaded_subtitle_name_and_is_unavailable_without_a_place() {
    let (_l, env) = mac();
    env.settings.set_bool(keys::AUTO_SAVE_SUBTITLE, false);
    let (vm, _c) = env.vm();
    assert!(!vm.can_save_subtitle());
    assert_eq!(vm.export_file_name(), "subtitles.srt");
    let sub = env.touch("Dizi.srt", SRT);
    assert!(vm.load_subtitle_file(&sub));
    assert!(vm.can_save_subtitle());
    assert_eq!(vm.export_file_name(), "Dizi.srt");
}

#[test]
fn save_failure_shows_a_localized_message() {
    let (_l, env) = mac();
    let (vm, _c) = env.vm();
    let sub = env.touch("gone/Dizi.srt", SRT);
    assert!(vm.load_subtitle_file(&sub));
    std::fs::remove_dir_all(env.path("gone")).unwrap();
    vm.save_subtitle_next_to_video();
    env.eventually("failed", || vm.status().kind() == JobStatusKind::Failed);
    assert!(vm.status_text().starts_with("Dosya kaydedilemedi:"), "{}", vm.status_text());
    vm.export_finished(None);
    assert_eq!(vm.status(), JobStatus::Finished, "a successful export clears a save failure");
}

#[test]
fn translate_auto_saves_the_translation_next_to_the_video() {
    let (_l, env) = mac();
    env.settings.set_string(keys::SELECTED_LANGUAGE, "tr");
    let (vm, _c) = env.vm();
    let v = env.touch("Film.mp4", "v");
    vm.open_video(Some(v.clone()));
    vm.generate();
    env.next(CallKind::Transcribe).result("tr", "", vec![s(1, 0.0, 1.0, "Bir")]);
    env.eventually("done", || vm.status() == JobStatus::Finished);
    assert!(!v.with_extension("srt").exists(), "generate alone never auto-saves");
    vm.set_export_language(ExportLanguage::Original);
    translate_all(&env, &vm, "en");
    let out = v.with_extension("srt");
    env.eventually("auto-saved", || out.exists());
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "en:Bir", "the translation even when the export shows the original");
    env.eventually("status", || vm.status_text() == "1 satır · dil: tr · çeviri: en · kaydedildi: Film.srt");
    assert!(!env.path("Film.en.srt").exists());
}

#[test]
fn auto_save_off_writes_nothing_and_a_missing_folder_does_nothing() {
    let (_l, env) = mac();
    env.settings.set_bool(keys::AUTO_SAVE_SUBTITLE, false);
    env.settings.set_string(keys::SELECTED_LANGUAGE, "tr");
    let (vm, _c) = env.vm();
    let sub = env.touch("Film.srt", SRT);
    assert!(vm.load_subtitle_file(&sub));
    translate_all(&env, &vm, "en");
    assert_eq!(std::fs::read_to_string(&sub).unwrap(), SRT);
}

#[test]
fn full_translate_without_a_video_only_translates_the_imported_lines() {
    let (_l, env) = mac();
    env.settings.set_string(keys::SELECTED_LANGUAGE, "tr");
    env.settings.set_bool(keys::AUTO_SAVE_SUBTITLE, false);
    let (vm, _c) = env.vm();
    assert!(vm.load_subtitle_file(&env.touch("x.srt", SRT)));
    assert!(vm.can_generate_and_translate());
    vm.generate_and_translate();
    let call = env.next(CallKind::Translate);
    assert!(env.engine.calls_of(CallKind::Transcribe).is_empty());
    call.finish();
}

#[test]
fn full_translate_with_a_video_transcribes_again_even_when_a_sidecar_is_loaded() {
    let (_l, env) = mac();
    env.settings.set_string(keys::SELECTED_LANGUAGE, "tr");
    let (vm, _c) = env.vm();
    env.touch("Film.srt", SRT);
    vm.open_video(Some(env.touch("Film.mp4", "v")));
    env.eventually("sidecar", || vm.sentences().len() == 2);
    vm.generate_and_translate();
    let t = env.next(CallKind::Transcribe);
    assert!(!vm.can_generate_and_translate(), "follows the working state");
    t.finish();
}

#[test]
fn full_translate_checked_offline_includes_the_translation_model() {
    let (_l, env) = mac();
    env.settings.set_bool(keys::OFFLINE_MODE, true);
    let mut models = downloaded_models();
    models.iter_mut().find(|m| m.id == "madlad-3b").unwrap().downloaded = false;
    *env.engine.models.lock() = models;
    let (vm, _c) = env.vm();
    vm.open_video(Some(env.touch("a.mp4", "v")));
    vm.generate_and_translate();
    assert_eq!(vm.status_text(), "MADLAD-400 3B indirilmemiş. Çevrimdışı mod açıkken model indirilemez.");
    assert!(env.engine.calls_of(CallKind::Transcribe).is_empty());
}

#[test]
fn opening_another_file_asks_first_when_work_would_be_lost_on_mac() {
    let (_l, env) = mac();
    let (vm, _c) = env.vm();
    assert!(vm.load_subtitle_file(&env.touch("x.srt", SRT)));
    vm.set_text(vm.sentences()[0].uid, "Değişti");
    assert!(vm.has_unsaved_edits());
    let v = env.touch("b.mp4", "v");
    vm.request_open_video(v.clone());
    let p = vm.pending_open().expect("asks");
    assert!(p.loses_edits);
    assert!(!p.stops_job);
    assert_eq!(p.message(), "Kaydedilmemiş değişiklikler kaybolacak. Devam edilsin mi?");
    assert_eq!(sa_viewmodels::subtitle::PendingOpen::title(), "Başka bir dosya açılsın mı?");
    assert_eq!(vm.video(), None, "nothing happened yet");
    vm.cancel_pending_open();
    assert!(vm.pending_open().is_none());
    assert_eq!(vm.sentences()[0].text, "Değişti");
    vm.request_open_video(v.clone());
    vm.confirm_pending_open();
    assert_eq!(vm.video(), Some(v));
}

#[test]
fn windows_opens_without_asking() {
    let _l = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    let (vm, _c) = env.vm();
    assert!(vm.load_subtitle_file(&env.touch("x.srt", SRT)));
    vm.set_text(vm.sentences()[0].uid, "Değişti");
    let v = env.touch("b.mp4", "v");
    vm.request_open_video(v.clone());
    assert!(vm.pending_open().is_none());
    assert_eq!(vm.video(), Some(v));
}

#[test]
fn open_external_files_keeps_only_existing_regular_files() {
    let (_l, env) = mac();
    let (vm, _c) = env.vm();
    vm.open_external_files(vec![env.path("missing.mp4"), env.path("")]);
    assert_eq!(vm.video(), None);
    let a = env.touch("e/a.mp4", "v");
    let b = env.touch("e/b.mp4", "v");
    vm.open_external_files(vec![a.clone(), b.clone()]);
    assert_eq!(vm.playlist().len(), 2, ">1 videos → playlist");
    vm.set_workspace(Workspace::Platform);
    vm.open_external_files(vec![a.clone()]);
    env.pump();
    assert_eq!(vm.workspace(), Workspace::Editor);
    assert_eq!(vm.video(), Some(a));
}

#[test]
fn json_import_review_markers_follow_the_platform() {
    let json = r#"[{"id":5,"start":0,"end":1,"text":"Bir","words":[],"needs_review":true},{"id":9,"start":1,"end":2,"text":"İki","words":[]}]"#;
    for env in [Env::mac(), Env::win()] {
        let _l = LanguageScope::new(env.policy.catalog_chain, UiLanguage::Tr);
        let (vm, _c) = env.vm();
        let p = env.touch("x.json", json);
        assert!(vm.load_subtitle_file(&p));
        let uid = vm.sentences()[0].uid;
        assert_eq!(vm.needs_review(uid), env.policy.import_json_review_entries);
        if env.policy.import_json_review_entries {
            assert_eq!(ids(&vm.sentences()), vec![1, 2], "macOS renumbers imported JSON ids");
            vm.set_text(uid, "Bir!");
            assert!(!vm.needs_review(uid), "editing the line clears the imported flag");
            vm.undo();
            assert!(vm.needs_review(uid), "undo restores it");
        }
    }
}
