//! The player's track menu model (`sa_viewmodels::media_tracks`): labels, the active marks, disabled image-based
//! entries, when the control shows at all, and the embedded-subtitle read bookkeeping.

mod common;

use std::path::Path;

use sa_core::l10n::{CatalogChain, UiLanguage};
use sa_core::settings::SettingsStore;
use sa_player::{AudioTrack, ExtractError, MediaTracks, SubtitleKind, SubtitleTrack};
use sa_viewmodels::media_tracks::{
    AutoChoice, EmbeddedSubtitleReads, IMAGE_SUMMARY_ID, MAX_LISTED_IMAGE_TRACKS, ReadMode, SUBTITLE_OFF_ID, SessionMark, SnapshotOutcome,
    TrackChoice, TrackMenu, audio_label, audio_track_failure, auto_track, channels_text, codec_title, embedded_read_empty, embedded_read_failure,
    embedded_read_outdated, embedded_read_partial, has_choices, language_code, language_name,
};

use common::{Env, LanguageScope};

fn audio(stream: usize, language: Option<&str>, title: Option<&str>, codec: &str, channels: u16) -> AudioTrack {
    AudioTrack {
        stream_index: stream,
        language: language.map(Into::into),
        title: title.map(Into::into),
        codec: codec.into(),
        channels,
        channel_layout: None,
        sample_rate: 48_000,
        default: false,
    }
}

fn sub(stream: usize, language: Option<&str>, title: Option<&str>, codec: &str, kind: SubtitleKind, forced: bool) -> SubtitleTrack {
    SubtitleTrack { stream_index: stream, language: language.map(Into::into), title: title.map(Into::into), codec: codec.into(), kind, default: false, forced }
}

fn sample() -> MediaTracks {
    MediaTracks {
        audio: vec![audio(1, Some("eng"), None, "aac", 2), audio(2, Some("eng"), Some("Commentary"), "ac3", 6), audio(3, None, None, "opus", 1)],
        subtitles: vec![
            sub(4, Some("tur"), None, "subrip", SubtitleKind::Text, false),
            sub(5, Some("en-US"), Some("SDH"), "ass", SubtitleKind::Text, true),
            sub(6, Some("jpn"), None, "hdmv_pgs_subtitle", SubtitleKind::Image, false),
        ],
        active_audio: Some(2),
    }
}

#[test]
fn labels_name_the_language_then_the_title_then_codec_and_channels() {
    let lang = LanguageScope::new(CatalogChain::Windows, UiLanguage::En);
    let menu = TrackMenu::build(&sample(), None, None, false);
    let labels: Vec<&str> = menu.audio.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(labels, vec!["English (AAC, stereo)", "English — Commentary (AC-3, 5.1)", "Track 3 (Opus, mono)"]);
    let labels: Vec<&str> = menu.subtitles.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(labels, vec!["Turkish (SRT)", "English — SDH (ASS, forced)", "Japanese (PGS)"]);
    lang.set(UiLanguage::Tr);
    let menu = TrackMenu::build(&sample(), None, None, false);
    assert_eq!(menu.audio[1].label, "İngilizce — Commentary (AC-3, 5.1)");
    assert_eq!(menu.audio[2].label, "Parça 3 (Opus, mono)");
    assert_eq!(menu.subtitles[1].label, "İngilizce — SDH (ASS, zorunlu)");
    // The same file on the macOS chain.
    drop(lang);
    let _mac = LanguageScope::new(CatalogChain::Mac, UiLanguage::En);
    assert_eq!(TrackMenu::build(&sample(), None, None, false).subtitles[0].label, "Turkish (SRT)");
}

#[test]
fn language_tags_codecs_and_channel_counts() {
    let _lang = LanguageScope::new(CatalogChain::Windows, UiLanguage::En);
    assert_eq!(language_name("ger").as_deref(), Some("German"));
    assert_eq!(language_name("deu").as_deref(), Some("German"));
    assert_eq!(language_name("fre").as_deref(), Some("French"));
    assert_eq!(language_name("pt-BR").as_deref(), Some(sa_core::models::SubtitleLanguage::from_code("pt").unwrap().short_name().as_str()));
    assert_eq!(language_name("ita").as_deref(), Some("ITA"), "a language the app has no name for shows its tag");
    assert_eq!(language_name("und"), None);
    assert_eq!(language_name("  "), None);
    assert_eq!(codec_title("eac3"), "E-AC-3");
    assert_eq!(codec_title("pcm_s16le"), "PCM");
    assert_eq!(codec_title("mov_text"), "TX3G");
    assert_eq!(codec_title("dvd_subtitle"), "VobSub");
    assert_eq!(codec_title("wmapro"), "WMAPRO");
    assert_eq!(channels_text(0), "");
    assert_eq!(channels_text(8), "7.1");
    assert_eq!(channels_text(3), "3 channels");
    // A title that only repeats the language is not shown twice.
    assert_eq!(audio_label(&audio(1, Some("eng"), Some("English"), "aac", 2), 1), "English (AAC, stereo)");
}

#[test]
fn the_playing_track_and_the_loaded_subtitle_are_marked_and_image_tracks_are_disabled() {
    let _lang = LanguageScope::new(CatalogChain::Windows, UiLanguage::En);
    let menu = TrackMenu::build(&sample(), None, Some(5), false);
    let active: Vec<bool> = menu.audio.iter().map(|i| i.active).collect();
    assert_eq!(active, vec![false, true, false]);
    assert!(menu.audio.iter().all(|i| i.enabled && i.hint.is_empty()));
    assert_eq!(menu.subtitles.iter().map(|i| i.active).collect::<Vec<_>>(), vec![false, true, false]);
    let image = &menu.subtitles[2];
    assert!(!image.enabled);
    assert_eq!(image.hint, "Image-based, not supported");
    assert!(menu.subtitles[0].enabled && menu.subtitles[1].enabled);
    // The ids parse back to what they stand for.
    assert_eq!(TrackChoice::parse(&menu.audio[0].id), Some(TrackChoice::Audio(1)));
    assert_eq!(TrackChoice::parse(&menu.subtitles[2].id), Some(TrackChoice::Subtitle(6)));
    assert_eq!(TrackChoice::parse("audio:x"), None);
    assert_eq!(TrackChoice::parse("model:base"), None);
}

#[test]
fn a_track_being_read_shows_its_progress_and_cannot_be_chosen_again() {
    let lang = LanguageScope::new(CatalogChain::Windows, UiLanguage::En);
    let menu = TrackMenu::build(&sample(), Some((4, 0.427)), None, false);
    assert!(!menu.subtitles[0].enabled);
    assert_eq!(menu.subtitles[0].hint, "Reading… 42%");
    assert!(menu.subtitles[1].enabled, "another track can be chosen (it replaces the read)");
    lang.set(UiLanguage::Tr);
    assert_eq!(TrackMenu::build(&sample(), Some((4, 0.427)), None, false).subtitles[0].hint, "Okunuyor… %42");
}

#[test]
fn the_control_shows_only_when_there_is_something_to_choose() {
    let none = MediaTracks::default();
    assert!(!has_choices(&none));
    let one_audio = MediaTracks { audio: vec![audio(1, None, None, "aac", 2)], subtitles: vec![], active_audio: Some(1) };
    assert!(!has_choices(&one_audio), "one audio track and no subtitles: nothing to choose");
    let one_audio_one_sub = MediaTracks { subtitles: vec![sub(2, None, None, "hdmv_pgs_subtitle", SubtitleKind::Image, false)], ..one_audio.clone() };
    assert!(has_choices(&one_audio_one_sub), "an image track is listed (disabled) too");
    let two_audio = MediaTracks { audio: vec![audio(1, None, None, "aac", 2), audio(2, None, None, "aac", 2)], subtitles: vec![], active_audio: Some(1) };
    assert!(has_choices(&two_audio));
    assert!(TrackMenu::build(&two_audio, None, None, false).subtitles.is_empty());
}

fn mark(content: u64) -> SessionMark {
    SessionMark { content, job: uuid::Uuid::nil(), working: false }
}

#[test]
fn one_read_at_a_time_and_a_cancelled_or_replaced_read_is_dropped() {
    let mut reads = EmbeddedSubtitleReads::default();
    let auto = ReadMode { automatic: true, progressive: true };
    let chosen = ReadMode { automatic: false, progressive: true };
    let a = reads.start(Path::new("/v.mkv"), 4, mark(1), auto).expect("started");
    assert!(reads.start(Path::new("/v.mkv"), 4, mark(1), chosen).is_none(), "the same read is not started twice");
    assert!(!reads.current(a.id).unwrap().mode.automatic, "choosing the track being read makes the read the user's");
    reads.applied(a.id, mark(7));
    let now = reads.current(a.id).unwrap();
    assert!(now.applied && now.mark == mark(7), "a snapshot moves the mark on");
    assert!(reads.progress(a.id, 0.5));
    assert_eq!(reads.reading(), Some((4, 0.5)));
    // Another track replaces it: the first is cancelled and its result is ignored.
    let b = reads.start(Path::new("/v.mkv"), 5, mark(2), chosen).expect("started");
    assert!(a.token.is_cancelled() && !b.token.is_cancelled());
    assert!(!reads.progress(a.id, 0.9));
    assert_eq!(reads.finish(a.id), None);
    let done = reads.finish(b.id).expect("current");
    assert_eq!((done.video.as_path(), done.stream, done.mark), (Path::new("/v.mkv"), 5, mark(2)));
    assert!(done.still_applies(mark(2)) && !done.still_applies(mark(3)));
    assert!(!reads.is_reading());
    // Opening another video cancels the read in flight (and says which one it was).
    let c = reads.start(Path::new("/v.mkv"), 4, mark(1), auto).expect("started");
    let cancelled = reads.cancel().expect("the read in flight");
    assert_eq!((cancelled.stream, cancelled.mode, cancelled.applied), (4, auto, false));
    assert!(c.token.is_cancelled());
    assert_eq!(reads.finish(c.id), None);
    assert!(reads.cancel().is_none());
    // The same track started again for a session that changed meanwhile is a new read.
    let d = reads.start(Path::new("/v.mkv"), 4, mark(1), auto).expect("started");
    let e = reads.start(Path::new("/v.mkv"), 4, mark(2), chosen).expect("restarted");
    assert!(d.token.is_cancelled() && !e.token.is_cancelled());
}

#[test]
fn many_image_tracks_collapse_into_one_disabled_row_and_unsupported_text_tracks_are_disabled() {
    let lang = LanguageScope::new(CatalogChain::Windows, UiLanguage::En);
    let mut tracks = sample();
    tracks.subtitles.push(sub(7, Some("ara"), None, "arib_caption", SubtitleKind::Unsupported, false));
    let menu = TrackMenu::build(&tracks, None, None, false);
    let unsupported = menu.subtitles.last().unwrap();
    assert!(!unsupported.enabled);
    assert_eq!(unsupported.hint, "Not supported");
    // Up to MAX_LISTED_IMAGE_TRACKS image tracks are listed one by one.
    for i in 0..MAX_LISTED_IMAGE_TRACKS - 1 {
        tracks.subtitles.push(sub(10 + i, Some("deu"), None, "hdmv_pgs_subtitle", SubtitleKind::Image, false));
    }
    assert_eq!(TrackMenu::build(&tracks, None, None, false).subtitles.len(), 4 + MAX_LISTED_IMAGE_TRACKS - 1);
    // A Blu-ray remux: 30 PGS tracks become one row; the text tracks stay.
    for i in 0..30 {
        tracks.subtitles.push(sub(20 + i, Some("fra"), None, "hdmv_pgs_subtitle", SubtitleKind::Image, false));
    }
    let images = tracks.subtitles.iter().filter(|t| t.kind == SubtitleKind::Image).count();
    let menu = TrackMenu::build(&tracks, None, None, false);
    assert_eq!(menu.subtitles.len(), 4, "{:?}", menu.subtitles);
    let summary = menu.subtitles.last().unwrap();
    assert_eq!((summary.id.as_str(), summary.enabled, summary.active), (IMAGE_SUMMARY_ID, false, false));
    assert_eq!(summary.label, format!("Image-based subtitle tracks: {images}"));
    assert_eq!(summary.hint, "Not supported");
    assert_eq!(TrackChoice::parse(&summary.id), None);
    lang.set(UiLanguage::Tr);
    let menu = TrackMenu::build(&tracks, None, None, false);
    assert_eq!(menu.subtitles.last().unwrap().label, format!("Görüntü tabanlı altyazı parçaları: {images}"));
}

#[test]
fn failures_are_worded_in_the_ui_language() {
    let lang = LanguageScope::new(CatalogChain::Windows, UiLanguage::En);
    assert_eq!(audio_track_failure("Decoder not found").text(), "The audio track could not be played: Decoder not found");
    assert_eq!(audio_track_failure("  ").text(), "The audio track could not be played: Unknown error");
    assert!(embedded_read_failure(&ExtractError::Cancelled).is_none());
    let text = |e: ExtractError| embedded_read_failure(&e).unwrap().text();
    assert_eq!(text(ExtractError::NotText), "The subtitles in the video could not be read: This track is not a text subtitle track");
    assert_eq!(text(ExtractError::TooLarge), "The subtitles in the video could not be read: The subtitle track is too large");
    assert_eq!(text(ExtractError::Failed(String::new())), "The subtitles in the video could not be read: Unknown error");
    let message = embedded_read_failure(&ExtractError::NotText).unwrap();
    lang.set(UiLanguage::Tr);
    assert_eq!(message.text(), "Videodaki altyazı okunamadı: Bu parça metin altyazısı değil", "re-worded on a language switch");
    assert_eq!(audio_track_failure("").text(), "Ses parçası açılamadı: Bilinmeyen hata");
    assert!(embedded_read_outdated().text().starts_with("Videodaki altyazı yüklenmedi"));
    assert_eq!(embedded_read_empty().text(), "Videodaki altyazı okunamadı: Parçada satır yok");
    assert!(embedded_read_partial().text().starts_with("Videodaki altyazının yalnızca bir kısmı yüklendi"));
    lang.set(UiLanguage::En);
    assert_eq!(embedded_read_empty().text(), "The subtitles in the video could not be read: The track has no lines");
    assert!(embedded_read_partial().text().starts_with("Only part of the subtitles from the video was loaded"));
}

/// The M1 rule: the mark changes when the user edits, when a job starts and when lines are loaded, so a read that
/// ended after any of these does not replace the lines silently.
#[test]
fn the_session_mark_follows_edits_and_loads() {
    let _lang = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    let (vm, _catalog) = env.vm();
    let video = env.touch("movie.mkv", "not really a video");
    vm.open_video(Some(video));
    env.pump();
    let empty = vm.session_mark();
    assert_eq!(vm.session_mark(), empty, "stable while nothing changes");
    let srt = env.touch("movie.srt", "1\n00:00:00,500 --> 00:00:01,250\nMerhaba\n\n2\n00:00:02,000 --> 00:00:03,000\nİkinci\n");
    assert!(vm.load_subtitle_file(&srt));
    env.pump();
    let loaded = vm.session_mark();
    assert_ne!(loaded, empty, "loading lines changes it");
    let uid = vm.sentences()[0].uid;
    vm.select_row(uid);
    vm.set_text(uid, "Merhaba dünya");
    env.pump();
    assert_ne!(vm.session_mark(), loaded, "an edit changes it");
}

// ------------------------------------------------------------------------------------------------ automatic choice

fn text_sub(stream: usize, language: Option<&str>, default: bool, forced: bool) -> SubtitleTrack {
    SubtitleTrack { default, ..sub(stream, language, None, "subrip", SubtitleKind::Text, forced) }
}

fn subs(subtitles: Vec<SubtitleTrack>) -> MediaTracks {
    MediaTracks { audio: vec![audio(1, Some("eng"), None, "aac", 6)], subtitles, active_audio: Some(1) }
}

#[test]
fn the_track_chosen_on_open_is_the_ui_languages_then_the_default_one() {
    // The user's films: SubRip `tur` + `eng` (stream order varies).
    let film = subs(vec![text_sub(2, Some("eng"), false, false), text_sub(3, Some("tur"), false, false)]);
    assert_eq!(auto_track(&film, "tr"), Some(3));
    assert_eq!(auto_track(&film, "en"), Some(2));
    // ISO 639-2/B and /T, ISO 639-1 and BCP 47 tags all count.
    assert_eq!(auto_track(&subs(vec![text_sub(4, Some("TR"), false, false)]), "tr"), Some(4));
    assert_eq!(auto_track(&subs(vec![text_sub(4, Some("en-GB"), false, false)]), "en"), Some(4));
    assert_eq!(language_code("ger"), Some("de"));
    assert_eq!(language_code("deu"), Some("de"));
    assert_eq!(language_code("tur"), Some("tr"));
    assert_eq!(language_code("und"), None);
    assert_eq!(language_code("ita"), None);
    // A forced track (only the foreign-language parts) comes after a full one, wherever it is.
    let forced_first = subs(vec![text_sub(2, Some("tur"), true, true), text_sub(3, Some("tur"), false, false), text_sub(4, Some("tur"), false, false)]);
    assert_eq!(auto_track(&forced_first, "tr"), Some(3));
    assert_eq!(auto_track(&subs(vec![text_sub(2, Some("tur"), false, true)]), "tr"), Some(2), "a forced one when it is all there is");
    // No track in the UI language: the default one, else none.
    let other = subs(vec![text_sub(2, Some("fre"), false, false), text_sub(3, Some("spa"), true, false), text_sub(4, None, true, false)]);
    assert_eq!(auto_track(&other, "tr"), Some(3));
    assert_eq!(auto_track(&subs(vec![text_sub(2, Some("fre"), false, false), text_sub(3, None, false, false)]), "en"), None);
    assert_eq!(auto_track(&subs(vec![]), "tr"), None);
    // Image-based and unsupported tracks are never chosen, even in the UI language and as the default.
    let images = subs(vec![
        SubtitleTrack { default: true, ..sub(2, Some("tur"), None, "hdmv_pgs_subtitle", SubtitleKind::Image, false) },
        SubtitleTrack { default: true, ..sub(3, Some("tur"), None, "arib_caption", SubtitleKind::Unsupported, false) },
    ]);
    assert_eq!(auto_track(&images, "tr"), None);
    let mixed = subs(vec![sub(2, Some("tur"), None, "hdmv_pgs_subtitle", SubtitleKind::Image, false), text_sub(3, Some("eng"), true, false)]);
    assert_eq!(auto_track(&mixed, "tr"), Some(3), "the text track with the default disposition");
}

#[test]
fn off_is_offered_first_while_the_lines_are_an_unedited_embedded_track() {
    let _lang = LanguageScope::new(CatalogChain::Windows, UiLanguage::En);
    let menu = TrackMenu::build(&sample(), None, Some(4), true);
    assert_eq!(menu.subtitles[0].id, SUBTITLE_OFF_ID);
    assert_eq!(menu.subtitles[0].label, "Off");
    assert!(menu.subtitles[0].enabled && !menu.subtitles[0].active);
    assert!(menu.subtitles[1].active, "the loaded track stays marked");
    assert_eq!(TrackChoice::parse(SUBTITLE_OFF_ID), Some(TrackChoice::SubtitleOff));
    assert_eq!(TrackChoice::SubtitleOff.id(), SUBTITLE_OFF_ID);
    assert!(TrackMenu::build(&sample(), None, Some(4), false).subtitles.iter().all(|i| i.id != SUBTITLE_OFF_ID));
}

fn lines(n: usize) -> Vec<sa_core::engine_types::Sentence> {
    (0..n).map(|i| sa_core::engine_types::Sentence::new(i as i64 + 1, i as f64 * 2.0, i as f64 * 2.0 + 1.5, format!("Satır {i}"))).collect()
}

/// A view model with `rel` open as the foreground video, its sidecar lookup over.
fn opened(env: &Env, rel: &str) -> (sa_viewmodels::SubtitleViewModel, std::rc::Rc<sa_core::models::ModelCatalog>, std::path::PathBuf) {
    let (vm, catalog) = env.vm();
    let video = env.touch(rel, "not really a video");
    vm.open_video(Some(video.clone()));
    env.eventually("the sidecar lookup", || vm.embedded_auto_choice(&video) != AutoChoice::Wait);
    (vm, catalog, video)
}

#[test]
fn a_track_is_chosen_only_for_an_empty_idle_session_once_no_sidecar_turned_up() {
    let _lang = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    let (vm, _catalog) = env.vm();
    // A subtitle next to the video wins: while it is looked for the choice waits, then it is refused.
    let video = env.touch("withsrt/film.mkv", "not really a video");
    env.touch("withsrt/film.srt", "1\n00:00:00,500 --> 00:00:01,250\nYandaki\n");
    vm.open_video(Some(video.clone()));
    assert_eq!(vm.embedded_auto_choice(&video), AutoChoice::Wait, "the sidecar is still being looked for");
    env.eventually("the sidecar", || vm.sentences().len() == 1);
    assert_eq!(vm.embedded_auto_choice(&video), AutoChoice::Refused);
    // No sidecar: allowed, but not for another video than the foreground's.
    let plain = env.touch("plain/film.mkv", "not really a video");
    vm.open_video(Some(plain.clone()));
    env.eventually("the sidecar lookup", || vm.embedded_auto_choice(&plain) != AutoChoice::Wait);
    assert_eq!(vm.embedded_auto_choice(&plain), AutoChoice::Allowed);
    assert_eq!(vm.embedded_auto_choice(&video), AutoChoice::Refused);
    assert!(vm.embedded_reads_progressively());
    // A running job: refused (and a read started now would not go in as it reads).
    vm.generate();
    let _call = env.next(common::CallKind::Transcribe);
    assert_eq!(vm.embedded_auto_choice(&plain), AutoChoice::Refused);
    assert!(!vm.embedded_reads_progressively());
    vm.cancel();
    env.eventually("cancelled", || !vm.is_working());
    // Lines the user imported: refused.
    let other = env.touch("other/film.mkv", "not really a video");
    vm.open_video(Some(other.clone()));
    env.eventually("the sidecar lookup", || vm.embedded_auto_choice(&other) != AutoChoice::Wait);
    let srt = env.touch("other/elsewhere.srt", "1\n00:00:00,500 --> 00:00:01,250\nBenim\n");
    assert!(vm.load_subtitle_file(&srt));
    assert_eq!(vm.embedded_auto_choice(&other), AutoChoice::Refused);
    assert!(!vm.embedded_reads_progressively(), "the user's lines are replaced only at the end, by the open-subtitle rules");
}

#[test]
fn embedded_lines_grow_snapshot_by_snapshot_without_ever_being_dirty_or_saved() {
    let _lang = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    assert!(env.settings.get_bool(sa_core::settings::keys::AUTO_SAVE_SUBTITLE).unwrap_or(true), "auto-save is on");
    let (vm, _catalog, video) = opened(&env, "films/film.mkv");
    let start = vm.session_mark();
    let SnapshotOutcome::Applied(m1) = vm.apply_embedded_snapshot(&video, 3, start, lines(2), false) else { panic!("applied") };
    assert_eq!(vm.sentences().len(), 2);
    let first = vm.sentences()[0].uid;
    vm.select_row(first);
    let SnapshotOutcome::Applied(m2) = vm.apply_embedded_snapshot(&video, 3, m1, lines(5), false) else { panic!("applied") };
    assert_eq!(vm.sentences().len(), 5);
    assert_eq!(vm.sentences()[0].uid, first, "a line keeps its identity across snapshots");
    assert_eq!(vm.selection(), Some(first), "and the selection stays");
    // A stale mark (a snapshot that raced another) is not applied.
    assert_eq!(vm.apply_embedded_snapshot(&video, 3, m1, lines(6), false), SnapshotOutcome::Edited);
    let SnapshotOutcome::Applied(_) = vm.apply_embedded_snapshot(&video, 3, m2, lines(6), true) else { panic!("applied") };
    assert_eq!(vm.sentences().len(), 6);
    let e = vm.embedded_lines().expect("embedded");
    assert_eq!((e.stream, e.pristine, e.complete), (3, true, true));
    assert!(!vm.has_unsaved_edits(), "not the user's work");
    assert_eq!(vm.loaded_subtitle(), None, "no file behind them");
    assert_eq!(vm.status(), sa_viewmodels::types::JobStatus::Finished);
    // Nothing is written next to the video, by any automatic path, up to and including the exit.
    vm.open_video(Some(env.touch("films/other.mkv", "not really a video")));
    env.pump();
    vm.open_video(Some(video.clone()));
    env.pump();
    vm.shutdown();
    env.pump();
    let mut names: Vec<String> = std::fs::read_dir(video.parent().unwrap()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    assert_eq!(names, vec!["film.mkv", "other.mkv"]);
}

#[test]
fn on_macos_embedded_lines_never_ask_before_another_video_opens_and_are_read_again_later() {
    let _lang = LanguageScope::new(CatalogChain::Mac, UiLanguage::Tr);
    let env = Env::mac();
    let (vm, _catalog, video) = opened(&env, "film.mkv");
    let SnapshotOutcome::Applied(_) = vm.apply_embedded_snapshot(&video, 3, vm.session_mark(), lines(3), true) else { panic!("applied") };
    vm.request_open_video(env.touch("next.mkv", "not really a video"));
    env.pump();
    assert!(vm.pending_open().is_none(), "no unsaved-changes question");
    assert_eq!(vm.video().unwrap().file_name().unwrap(), "next.mkv");
    // Evicted like a session that can be rebuilt: opened again, it is empty and gets its track read again.
    vm.request_open_video(video.clone());
    env.eventually("the sidecar lookup", || vm.embedded_auto_choice(&video) != AutoChoice::Wait);
    assert!(vm.sentences().is_empty());
    assert_eq!(vm.embedded_auto_choice(&video), AutoChoice::Allowed);
}

#[test]
fn an_edit_makes_embedded_lines_the_users_and_stops_further_snapshots() {
    let _lang = LanguageScope::new(CatalogChain::Mac, UiLanguage::Tr);
    let env = Env::mac();
    let (vm, _catalog, video) = opened(&env, "film.mkv");
    let SnapshotOutcome::Applied(mark) = vm.apply_embedded_snapshot(&video, 3, vm.session_mark(), lines(2), false) else { panic!("applied") };
    let uid = vm.sentences()[0].uid;
    vm.select_row(uid);
    vm.set_text(uid, "Düzelttim");
    env.pump();
    assert_eq!(vm.apply_embedded_snapshot(&video, 3, mark, lines(4), false), SnapshotOutcome::Edited);
    assert_eq!(vm.sentences().len(), 2);
    assert_eq!(vm.sentences()[0].text, "Düzelttim");
    let e = vm.embedded_lines().expect("still from the track");
    assert!(!e.pristine && !e.complete);
    assert!(!vm.clear_embedded_lines(), "\"Off\" does not throw edits away");
    assert!(vm.has_unsaved_edits());
    // Kept like any edited session: another video and back, and the edit is still there.
    vm.request_open_video(env.touch("next.mkv", "not really a video"));
    vm.request_open_video(video.clone());
    env.pump();
    assert_eq!(vm.sentences()[0].text, "Düzelttim");
    // Another file opened meanwhile supersedes a read silently.
    let srt = env.touch("other.srt", "1\n00:00:00,500 --> 00:00:01,250\nDosya\n");
    assert!(vm.load_subtitle_file(&srt));
    assert_eq!(vm.apply_embedded_snapshot(&video, 3, mark, lines(4), false), SnapshotOutcome::Superseded);
    assert_eq!(vm.apply_embedded_snapshot(&video, 3, vm.session_mark(), lines(4), false), SnapshotOutcome::Superseded);
    assert_eq!(vm.sentences().len(), 1, "a snapshot never replaces the user's lines");
}

#[test]
fn a_translation_or_a_save_makes_embedded_lines_the_users_and_auto_save_then_works_as_usual() {
    let _lang = LanguageScope::new(CatalogChain::Mac, UiLanguage::Tr);
    let env = Env::mac();
    env.settings.set_string(sa_core::settings::keys::SELECTED_LANGUAGE, "tr");
    let (vm, _catalog, video) = opened(&env, "Film.mkv");
    let SnapshotOutcome::Applied(_) = vm.apply_embedded_snapshot(&video, 3, vm.session_mark(), lines(1), true) else { panic!("applied") };
    assert!(vm.embedded_lines().unwrap().pristine);
    // The user asks for a translation: that is their work, and the translation is auto-saved as for any lines.
    vm.translate();
    let call = env.next(common::CallKind::Translate);
    let out = call.record.sentences.iter().map(|x| sa_core::engine_types::Sentence::new(x.id, x.start, x.end, format!("en:{}", x.text))).collect();
    call.translation("en", out);
    call.finish();
    let saved = video.with_extension("srt");
    env.eventually("auto-saved", || saved.exists());
    assert!(!vm.embedded_lines().unwrap().pristine);
    assert!(!vm.clear_embedded_lines());
}

#[test]
fn off_clears_unedited_embedded_lines_and_an_abandoned_read_leaves_nothing_behind() {
    let _lang = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    let (vm, _catalog, video) = opened(&env, "film.mkv");
    let SnapshotOutcome::Applied(_) = vm.apply_embedded_snapshot(&video, 3, vm.session_mark(), lines(3), true) else { panic!("applied") };
    assert!(vm.clear_embedded_lines());
    assert!(vm.sentences().is_empty() && vm.embedded_lines().is_none());
    assert_eq!(vm.status(), sa_viewmodels::types::JobStatus::Idle);
    assert!(!vm.clear_embedded_lines(), "nothing left to clear");
    // A read stopped half way by another video: its lines go, so the video opens empty again.
    let SnapshotOutcome::Applied(_) = vm.apply_embedded_snapshot(&video, 4, vm.session_mark(), lines(2), false) else { panic!("applied") };
    let next = env.touch("next.mkv", "not really a video");
    vm.open_video(Some(next));
    vm.abandon_embedded_read(&video, 4);
    vm.open_video(Some(video.clone()));
    env.eventually("the sidecar lookup", || vm.embedded_auto_choice(&video) != AutoChoice::Wait);
    assert!(vm.sentences().is_empty());
    assert_eq!(vm.embedded_auto_choice(&video), AutoChoice::Allowed);
    // A read that finished is not abandoned.
    let SnapshotOutcome::Applied(_) = vm.apply_embedded_snapshot(&video, 4, vm.session_mark(), lines(2), true) else { panic!("applied") };
    vm.abandon_embedded_read(&video, 4);
    assert_eq!(vm.sentences().len(), 2);
}

#[test]
fn the_complete_track_over_the_users_lines_follows_the_open_subtitle_rules() {
    let _lang = LanguageScope::new(CatalogChain::Mac, UiLanguage::Tr);
    let env = Env::mac();
    let (vm, _catalog, video) = opened(&env, "film.mkv");
    let srt = env.touch("mine.srt", "1\n00:00:00,500 --> 00:00:01,250\nBenim\n");
    assert!(vm.load_subtitle_file(&srt));
    // Unedited imported lines: replaced right away.
    vm.request_embedded_lines(&video, 3, lines(3));
    assert_eq!(vm.sentences().len(), 3);
    assert!(vm.embedded_lines().is_some_and(|e| e.pristine && e.complete));
    // Edited lines: macOS asks first.
    let uid = vm.sentences()[0].uid;
    vm.select_row(uid);
    vm.set_text(uid, "Değişti");
    env.pump();
    vm.request_embedded_lines(&video, 4, lines(5));
    assert!(vm.pending_open().is_some_and(|p| p.loses_edits));
    assert_eq!(vm.sentences().len(), 3);
    vm.confirm_pending_open();
    assert_eq!(vm.sentences().len(), 5);
    assert_eq!(vm.embedded_lines().map(|e| e.stream), Some(4));
    assert!(!vm.has_unsaved_edits());
}
