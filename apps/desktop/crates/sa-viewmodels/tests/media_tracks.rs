//! The player's track menu model (`sa_viewmodels::media_tracks`): labels, the active marks, selectable image-based
//! entries, when the control shows at all, the automatic choice, and the embedded-subtitle read bookkeeping.

mod common;

use std::path::Path;

use sa_core::l10n::{CatalogChain, UiLanguage};
use sa_core::settings::SettingsStore;
use sa_player::{AudioTrack, ExtractError, MediaTracks, SubtitleKind, SubtitleTrack};
use sa_viewmodels::media_tracks::{
    AutoChoice, AutoTrack, EmbeddedSubtitleReads, ReadMode, SUBTITLE_OFF_ID, SessionMark, SnapshotOutcome, TrackChoice, TrackMenu, audio_label,
    audio_track_failure, auto_choice, auto_image_track, auto_track, channels_text, codec_title, embedded_read_empty, embedded_read_failure,
    embedded_read_outdated, embedded_read_partial, has_choices, language_code, language_name, subtitle_track_failure,
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
        active_image_subtitle: None,
    }
}

#[test]
fn labels_name_the_language_then_the_title_then_codec_and_channels() {
    let lang = LanguageScope::new(CatalogChain::Windows, UiLanguage::En);
    let menu = TrackMenu::build(&sample(), None, None, false);
    let labels: Vec<&str> = menu.audio.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(labels, vec!["English (AAC, stereo)", "English — Commentary (AC-3, 5.1)", "Track 3 (Opus, mono)"]);
    let labels: Vec<&str> = menu.subtitles.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(labels, vec!["Off", "Turkish (SRT)", "English — SDH (ASS, forced)", "Japanese (PGS)"]);
    lang.set(UiLanguage::Tr);
    let menu = TrackMenu::build(&sample(), None, None, false);
    assert_eq!(menu.audio[1].label, "İngilizce — Commentary (AC-3, 5.1)");
    assert_eq!(menu.audio[2].label, "Parça 3 (Opus, mono)");
    assert_eq!(menu.subtitles[0].label, "Kapalı");
    assert_eq!(menu.subtitles[2].label, "İngilizce — SDH (ASS, zorunlu)");
    // The same file on the macOS chain.
    drop(lang);
    let _mac = LanguageScope::new(CatalogChain::Mac, UiLanguage::En);
    assert_eq!(TrackMenu::build(&sample(), None, None, false).subtitles[1].label, "Turkish (SRT)");
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
fn the_playing_track_and_the_loaded_subtitle_are_marked_and_image_tracks_can_be_chosen() {
    let _lang = LanguageScope::new(CatalogChain::Windows, UiLanguage::En);
    let menu = TrackMenu::build(&sample(), None, Some(5), false);
    let active: Vec<bool> = menu.audio.iter().map(|i| i.active).collect();
    assert_eq!(active, vec![false, true, false]);
    assert!(menu.audio.iter().all(|i| i.enabled && i.hint.is_empty()));
    // "Off" first, not checked: a track's lines are in the list.
    assert_eq!(menu.subtitles.iter().map(|i| i.active).collect::<Vec<_>>(), vec![false, false, true, false]);
    let image = &menu.subtitles[3];
    assert!(image.enabled && image.hint.is_empty() && !image.active);
    assert!(menu.subtitles[1].enabled && menu.subtitles[2].enabled);
    // The ids parse back to what they stand for.
    assert_eq!(TrackChoice::parse(&menu.audio[0].id), Some(TrackChoice::Audio(1)));
    assert_eq!(TrackChoice::parse(&menu.subtitles[3].id), Some(TrackChoice::Subtitle(6)));
    assert_eq!(TrackChoice::parse("audio:x"), None);
    assert_eq!(TrackChoice::parse("model:base"), None);
}

#[test]
fn a_track_being_read_shows_its_progress_and_cannot_be_chosen_again() {
    let lang = LanguageScope::new(CatalogChain::Windows, UiLanguage::En);
    let menu = TrackMenu::build(&sample(), Some((4, 0.427)), None, false);
    assert!(!menu.subtitles[0].active, "\"Off\" is not checked while a track is read");
    assert!(!menu.subtitles[1].enabled);
    assert_eq!(menu.subtitles[1].hint, "Reading… 42%");
    assert!(menu.subtitles[2].enabled, "another track can be chosen (it replaces the read)");
    lang.set(UiLanguage::Tr);
    assert_eq!(TrackMenu::build(&sample(), Some((4, 0.427)), None, false).subtitles[1].hint, "Okunuyor… %42");
}

#[test]
fn the_control_shows_only_when_there_is_something_to_choose() {
    let none = MediaTracks::default();
    assert!(!has_choices(&none));
    let one_audio = MediaTracks { audio: vec![audio(1, None, None, "aac", 2)], active_audio: Some(1), ..MediaTracks::default() };
    assert!(!has_choices(&one_audio), "one audio track and no subtitles: nothing to choose");
    let one_audio_one_sub = MediaTracks { subtitles: vec![sub(2, None, None, "hdmv_pgs_subtitle", SubtitleKind::Image, false)], ..one_audio.clone() };
    assert!(has_choices(&one_audio_one_sub), "an image track is a choice too");
    let two_audio = MediaTracks { audio: vec![audio(1, None, None, "aac", 2), audio(2, None, None, "aac", 2)], active_audio: Some(1), ..MediaTracks::default() };
    assert!(has_choices(&two_audio));
    assert!(TrackMenu::build(&two_audio, None, None, false).subtitles.is_empty());
}

fn mark(content: u64) -> SessionMark {
    SessionMark { content, job: uuid::Uuid::nil(), working: false }
}

#[test]
fn one_read_at_a_time_and_a_cancelled_or_replaced_read_is_dropped() {
    let mut reads = EmbeddedSubtitleReads::default();
    let auto = ReadMode::AUTOMATIC;
    let chosen = ReadMode { automatic: false, progressive: true, resume: false };
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
fn every_image_track_is_its_own_choice_however_many_and_unsupported_tracks_are_disabled() {
    let lang = LanguageScope::new(CatalogChain::Windows, UiLanguage::En);
    let mut tracks = sample();
    tracks.subtitles.push(sub(7, Some("ara"), None, "arib_caption", SubtitleKind::Unsupported, false));
    let menu = TrackMenu::build(&tracks, None, None, false);
    let unsupported = menu.subtitles.last().unwrap();
    assert!(!unsupported.enabled);
    assert_eq!(unsupported.hint, "Not supported");
    // A Blu-ray remux: 30 PGS tracks, each a row of its own (the menu scrolls), one of them shown.
    for i in 0..30 {
        tracks.subtitles.push(sub(20 + i, Some(["fra", "deu", "spa"][i % 3]), None, "hdmv_pgs_subtitle", SubtitleKind::Image, i == 0));
    }
    tracks.active_image_subtitle = Some(25);
    let menu = TrackMenu::build(&tracks, None, Some(4), true);
    assert_eq!(menu.subtitles.len(), 1 + 4 + 30, "Off, the four tracks of before and every PGS track");
    let pgs: Vec<_> = menu.subtitles.iter().filter(|i| i.label.contains("PGS")).collect();
    assert_eq!(pgs.len(), 31);
    assert!(pgs.iter().all(|i| i.enabled && i.hint.is_empty()));
    assert_eq!(pgs.iter().filter(|i| i.active).map(|i| i.id.as_str()).collect::<Vec<_>>(), vec!["subtitle:25"]);
    assert_eq!(menu.subtitles.iter().find(|i| i.id == "subtitle:20").unwrap().label, "French (PGS, forced)");
    assert_eq!(TrackChoice::parse("subtitle:25"), Some(TrackChoice::Subtitle(25)));
    // One radio group: the image track shown is the one marked, not the text track whose lines are in the list.
    assert_eq!(menu.subtitles.iter().filter(|i| i.active).count(), 1);
    assert!(!menu.subtitles[1].active);
    lang.set(UiLanguage::Tr);
    let menu = TrackMenu::build(&tracks, None, None, false);
    assert_eq!(menu.subtitles.iter().find(|i| i.id == "subtitle:21").unwrap().label, "Almanca (PGS)");
}

#[test]
fn an_image_track_shown_takes_the_mark_from_off_and_off_can_hide_it() {
    let _lang = LanguageScope::new(CatalogChain::Windows, UiLanguage::En);
    let mut tracks = sample();
    tracks.active_image_subtitle = Some(6);
    // Nothing in the list: the image track is checked, "Off" is not but can be chosen (it hides the picture).
    let menu = TrackMenu::build(&tracks, None, None, false);
    assert_eq!(menu.subtitles.iter().map(|i| (i.id.as_str(), i.active)).collect::<Vec<_>>(), vec![
        (SUBTITLE_OFF_ID, false),
        ("subtitle:4", false),
        ("subtitle:5", false),
        ("subtitle:6", true)
    ]);
    assert!(menu.subtitles[0].enabled);
    // Edited lines of a text track in the list: "Off" still hides the picture (it never clears the user's lines).
    let menu = TrackMenu::build(&tracks, None, Some(4), false);
    assert!(menu.subtitles[0].enabled && !menu.subtitles[0].active && menu.subtitles[3].active && !menu.subtitles[1].active);
    // A stream the player reports that is no image track of the list is not marked.
    tracks.active_image_subtitle = Some(4);
    let menu = TrackMenu::build(&tracks, None, None, false);
    assert!(menu.subtitles[0].active && menu.subtitles[1..].iter().all(|i| !i.active));
}

#[test]
fn failures_are_worded_in_the_ui_language() {
    let lang = LanguageScope::new(CatalogChain::Windows, UiLanguage::En);
    assert_eq!(audio_track_failure("Decoder not found").text(), "The audio track could not be played: Decoder not found");
    assert_eq!(audio_track_failure("  ").text(), "The audio track could not be played: Unknown error");
    assert_eq!(subtitle_track_failure("Decoder not found").text(), "The subtitle track could not be shown: Decoder not found");
    assert_eq!(subtitle_track_failure("").text(), "The subtitle track could not be shown: Unknown error");
    let shown = subtitle_track_failure("x");
    assert!(embedded_read_failure(&ExtractError::Cancelled).is_none());
    let text = |e: ExtractError| embedded_read_failure(&e).unwrap().text();
    assert_eq!(text(ExtractError::NotText), "The subtitles in the video could not be read: This track is not a text subtitle track");
    assert_eq!(text(ExtractError::TooLarge), "The subtitles in the video could not be read: The subtitle track is too large");
    assert_eq!(text(ExtractError::Failed(String::new())), "The subtitles in the video could not be read: Unknown error");
    let message = embedded_read_failure(&ExtractError::NotText).unwrap();
    lang.set(UiLanguage::Tr);
    assert_eq!(message.text(), "Videodaki altyazı okunamadı: Bu parça metin altyazısı değil", "re-worded on a language switch");
    assert_eq!(audio_track_failure("").text(), "Ses parçası açılamadı: Bilinmeyen hata");
    assert_eq!(shown.text(), "Altyazı parçası gösterilemedi: x");
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
    MediaTracks { audio: vec![audio(1, Some("eng"), None, "aac", 6)], subtitles, active_audio: Some(1), active_image_subtitle: None }
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
fn without_a_text_track_to_choose_the_same_rule_picks_an_image_track() {
    let pgs = |stream, language, default, forced| SubtitleTrack { default, ..sub(stream, language, None, "hdmv_pgs_subtitle", SubtitleKind::Image, forced) };
    // Text first, whenever the text rule finds one, even with an image track in the UI language.
    let mixed = subs(vec![pgs(2, Some("tur"), false, false), text_sub(3, Some("eng"), true, false)]);
    assert_eq!(auto_choice(&mixed, "tr"), Some(AutoTrack::Text(3)));
    // A Blu-ray remux (PGS only): the UI language's, non-forced first; else the default one; else none.
    let disc = subs(vec![pgs(2, Some("eng"), false, false), pgs(3, Some("tur"), false, true), pgs(4, Some("tur"), false, false), pgs(5, Some("fre"), true, false)]);
    assert_eq!(auto_image_track(&disc, "tr"), Some(4));
    assert_eq!(auto_choice(&disc, "tr"), Some(AutoTrack::Image(4)));
    assert_eq!(auto_choice(&disc, "en"), Some(AutoTrack::Image(2)));
    assert_eq!(auto_choice(&subs(vec![pgs(2, Some("tur"), false, true)]), "tr"), Some(AutoTrack::Image(2)), "a forced one when it is all there is");
    let other = subs(vec![pgs(2, Some("eng"), false, false), pgs(5, Some("fre"), true, false)]);
    assert_eq!(auto_choice(&other, "tr"), Some(AutoTrack::Image(5)));
    assert_eq!(auto_choice(&subs(vec![pgs(2, Some("eng"), false, false)]), "tr"), None);
    // A text track that the text rule does not pick (another language, not the default) does not stop the image rule.
    let foreign_text = subs(vec![text_sub(2, Some("eng"), false, false), pgs(3, Some("tur"), false, false)]);
    assert_eq!(auto_choice(&foreign_text, "tr"), Some(AutoTrack::Image(3)));
    // Unsupported tracks never.
    let teletext = subs(vec![SubtitleTrack { default: true, ..sub(2, Some("tur"), None, "dvb_teletext", SubtitleKind::Unsupported, false) }]);
    assert_eq!(auto_choice(&teletext, "tr"), None);
}

#[test]
fn off_comes_first_checked_while_nothing_is_loaded_and_enabled_while_it_can_clear() {
    let _lang = LanguageScope::new(CatalogChain::Windows, UiLanguage::En);
    // Nothing from the video in the list: "Off" is what holds (and choosing it keeps a track from being chosen).
    let menu = TrackMenu::build(&sample(), None, None, false);
    assert_eq!(menu.subtitles[0].id, SUBTITLE_OFF_ID);
    assert!(menu.subtitles[0].active && menu.subtitles[0].enabled);
    assert!(menu.subtitles[1..].iter().all(|i| !i.active));
    let menu = TrackMenu::build(&sample(), None, Some(4), true);
    assert_eq!(menu.subtitles[0].id, SUBTITLE_OFF_ID);
    assert_eq!(menu.subtitles[0].label, "Off");
    assert!(menu.subtitles[0].enabled && !menu.subtitles[0].active);
    assert!(menu.subtitles[1].active, "the loaded track stays marked");
    assert_eq!(TrackChoice::parse(SUBTITLE_OFF_ID), Some(TrackChoice::SubtitleOff));
    assert_eq!(TrackChoice::SubtitleOff.id(), SUBTITLE_OFF_ID);
    // Edited lines: "Off" cannot clear them.
    let edited = TrackMenu::build(&sample(), None, Some(4), false);
    assert!(!edited.subtitles[0].enabled && !edited.subtitles[0].active);
    // Only image tracks: "Off" is there too (the image tracks are choices now).
    let mut images = sample();
    images.subtitles.retain(|t| t.kind == SubtitleKind::Image);
    assert_eq!(TrackMenu::build(&images, None, None, false).subtitles[0].id, SUBTITLE_OFF_ID);
    // Only unsupported tracks: nothing to turn off.
    let mut unsupported = sample();
    unsupported.subtitles = vec![sub(9, None, None, "arib_caption", SubtitleKind::Unsupported, false)];
    assert!(TrackMenu::build(&unsupported, None, None, false).subtitles.iter().all(|i| i.id != SUBTITLE_OFF_ID));
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
    // The automatic write (what auto-save runs) refuses them; written once the user changed them.
    vm.save_automatically();
    assert_eq!(renders_up_to_a_marker(&env, &vm), 1, "the automatic write rendered nothing: only the marker");
    assert!(!video.with_extension("srt").exists(), "the automatic write refuses lines read from the video");
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
    assert_eq!(vm.embedded_auto_choice(&video), AutoChoice::Refused, "\"Off\" holds for the video in this run");
    // The user chooses a track again: a read stopped half way by another video; its lines go, so the video opens
    // empty again.
    vm.embedded_track_chosen();
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

/// The automatic-write guard (`Session::may_write_automatically`), driven through the write auto-save makes: it fails
/// if the guard in `save_subtitle(automatic)` is removed.
#[test]
fn the_automatic_write_skips_unedited_embedded_lines_and_writes_them_once_edited() {
    let _lang = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    let (vm, _catalog, video) = opened(&env, "guard/film.mkv");
    let SnapshotOutcome::Applied(_) = vm.apply_embedded_snapshot(&video, 3, vm.session_mark(), lines(2), true) else { panic!("applied") };
    let srt = video.with_extension("srt");
    vm.save_automatically();
    assert_eq!(renders_up_to_a_marker(&env, &vm), 1, "refused while the lines are the track's: only the marker rendered");
    assert!(!srt.exists());
    let uid = vm.sentences()[0].uid;
    vm.select_row(uid);
    vm.set_text(uid, "Benim");
    vm.save_automatically();
    env.eventually("written once edited", || srt.exists());
}

fn translate_everything(env: &Env) {
    env.engine.set_script(|r| match r.kind {
        common::CallKind::Transcribe => {
            let name = r.video.as_ref().unwrap().file_stem().unwrap().to_string_lossy().into_owned();
            Some(vec![Ok(sa_enginekit::events::EngineEvent::Result(sa_enginekit::events::EngineResult {
                language: "en".into(),
                duration: 1.0,
                audio: String::new(),
                outputs: vec![],
                sentences: vec![common::s(1, 0.0, 1.0, &format!("Metin {name}"))],
            }))])
        }
        common::CallKind::Translate => Some(vec![Ok(sa_enginekit::events::EngineEvent::Translation {
            language: r.to.clone().unwrap(),
            sentences: r.sentences.iter().map(|x| common::s(x.id, x.start, x.end, &format!("T:{}", x.text))).collect(),
        })]),
        common::CallKind::Extract => None,
    });
}

fn playlist_states(vm: &sa_viewmodels::SubtitleViewModel) -> Vec<sa_viewmodels::types::PlaylistItemState> {
    vm.playlist().iter().map(|p| p.state).collect()
}

/// H1: a batch run transcribes a video whose list holds an unedited embedded track (it is no lines to the run), and
/// nothing made from the track is written.
#[test]
fn a_batch_run_transcribes_a_video_holding_an_unedited_embedded_track() {
    let _lang = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    env.settings.set_string(sa_core::settings::keys::TRANSLATION_TARGET, "tr");
    translate_everything(&env);
    let (vm, _catalog, video) = opened(&env, "h1/film.mkv");
    let SnapshotOutcome::Applied(_) = vm.apply_embedded_snapshot(&video, 3, vm.session_mark(), lines(3), true) else { panic!("applied") };
    vm.add_playlist_videos_with_language(vec![video.clone()], "en");
    vm.start_playlist();
    env.eventually("completed", || playlist_states(&vm) == vec![sa_viewmodels::types::PlaylistItemState::Completed]);
    let transcribed: Vec<_> = env.engine.calls_of(common::CallKind::Transcribe).iter().map(|c| c.video.clone()).collect();
    assert_eq!(transcribed, vec![Some(video.clone())], "the video was transcribed");
    let translated: Vec<String> = env.engine.calls_of(common::CallKind::Translate).iter().flat_map(|c| c.sentences.iter().map(|x| x.text.clone())).collect();
    assert_eq!(translated, vec!["Metin film"], "the track's lines were never translated");
    assert_eq!(std::fs::read_to_string(video.with_extension("srt")).unwrap(), "T:Metin film");
}

/// H1: a read filling the list when the run starts stops (the view model no longer waits for it), its lines go, and
/// a late snapshot of it is not applied.
#[test]
fn a_batch_run_drops_the_lines_of_a_read_in_flight_and_ignores_its_late_snapshots() {
    let _lang = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    env.settings.set_string(sa_core::settings::keys::TRANSLATION_TARGET, "tr");
    translate_everything(&env);
    let (vm, _catalog, video) = opened(&env, "h1b/film.mkv");
    let SnapshotOutcome::Applied(mark) = vm.apply_embedded_snapshot(&video, 3, vm.session_mark(), lines(2), false) else { panic!("applied") };
    vm.set_embedded_read(&video, Some(0.3));
    assert_eq!(vm.embedded_read_progress(), Some(0.3));
    vm.add_playlist_videos_with_language(vec![video.clone()], "en");
    assert_eq!(vm.embedded_auto_choice(&video), AutoChoice::Refused, "nothing is chosen for a queued video");
    vm.start_playlist();
    assert_eq!(vm.embedded_read_progress(), None, "the run does not wait for the read");
    assert!(vm.sentences().iter().all(|l| !l.text.starts_with("Satır")), "the partial lines went");
    assert_eq!(vm.apply_embedded_snapshot(&video, 3, mark, lines(4), false), SnapshotOutcome::Superseded);
    assert_eq!(vm.apply_embedded_snapshot(&video, 3, vm.session_mark(), lines(4), false), SnapshotOutcome::Superseded);
    env.eventually("completed", || playlist_states(&vm) == vec![sa_viewmodels::types::PlaylistItemState::Completed]);
    assert_eq!(env.engine.calls_of(common::CallKind::Transcribe).len(), 1);
    assert_eq!(std::fs::read_to_string(video.with_extension("srt")).unwrap(), "T:Metin film");
}

/// M1: while a read fills the list, it is not complete: translating, saving and exporting wait (the status says why);
/// generating does not.
#[test]
fn while_a_read_fills_the_list_translating_saving_and_exporting_wait() {
    let _lang = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    env.settings.set_string(sa_core::settings::keys::SELECTED_LANGUAGE, "tr");
    let (vm, _catalog, video) = opened(&env, "m1/film.mkv");
    let SnapshotOutcome::Applied(_) = vm.apply_embedded_snapshot(&video, 3, vm.session_mark(), lines(3), false) else { panic!("applied") };
    vm.select_row(vm.sentences()[0].uid);
    let (translate, selected, export, save) = (vm.can_translate(), vm.can_translate_selected(), vm.can_export(), vm.can_save_subtitle());
    assert!(translate && selected && export && save, "available for the lines as they are");
    vm.set_embedded_read(&video, Some(0.42));
    assert!(!vm.can_translate() && !vm.can_translate_selected() && !vm.can_export() && !vm.can_save_subtitle());
    assert!(vm.status_text().ends_with("Okunuyor… %42"), "{}", vm.status_text());
    assert!(vm.can_generate(), "generating replaces the track's lines: available");
    vm.translate();
    vm.translate_selected();
    vm.save_subtitle_next_to_video();
    vm.export_to(video.with_extension("vtt"));
    // The read ends: all of it again.
    vm.set_embedded_read(&video, None);
    assert!(vm.can_translate() && vm.can_save_subtitle() && vm.can_export());
    assert!(!vm.status_text().contains("Okunuyor"));
    // Nothing of the above reached the engine: the first render is the marker, the first translation this one.
    assert_eq!(renders_up_to_a_marker(&env, &vm), 1);
    vm.translate();
    let _call = env.next(common::CallKind::Translate);
    assert_eq!(env.engine.calls_of(common::CallKind::Translate).len(), 1);
    let mut names: Vec<String> = std::fs::read_dir(video.parent().unwrap()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    assert_eq!(names, vec!["film.mkv"]);
}

/// M2: an early-stopped read the user edited (a split, a delete) gets only the rest of the track added after the
/// user's lines; one undo takes exactly those away.
#[test]
fn choosing_a_track_again_after_its_read_stopped_adds_only_the_rest() {
    let _lang = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    let (vm, _catalog, video) = opened(&env, "m2/film.mkv");
    let all = lines(10);
    let SnapshotOutcome::Applied(_) = vm.apply_embedded_snapshot(&video, 3, vm.session_mark(), all[..4].to_vec(), false) else { panic!("applied") };
    // The user splits line 1 (2.0–3.5) and deletes line 2 of what was read.
    let second = vm.sentences()[1].uid;
    vm.select_row(second);
    assert!(vm.can_split_selected(2.7));
    vm.split_selected(2.7);
    let third = vm.sentences().into_iter().find(|l| l.text == "Satır 2").unwrap().uid;
    vm.select_row(third);
    vm.delete_selected();
    env.pump();
    assert_eq!(vm.sentences().len(), 4);
    assert!(!vm.embedded_lines().unwrap().pristine);
    assert!(vm.embedded_resumes(3), "the same track resumes");
    assert!(!vm.embedded_resumes(4), "another track replaces");
    let before: Vec<(uuid::Uuid, String, f64)> = vm.sentences().into_iter().map(|l| (l.uid, l.text, l.start)).collect();
    assert_eq!(vm.append_embedded_rest(&video, 3, all.clone()), Some(6));
    let after = vm.sentences();
    assert_eq!(after.len(), 10);
    let kept: Vec<(uuid::Uuid, String, f64)> = after[..4].iter().map(|l| (l.uid, l.text.clone(), l.start)).collect();
    assert_eq!(kept, before, "the user's lines are untouched");
    let added: Vec<&str> = after[4..].iter().map(|l| l.text.as_str()).collect();
    assert_eq!(added, vec!["Satır 4", "Satır 5", "Satır 6", "Satır 7", "Satır 8", "Satır 9"]);
    let ids: Vec<i64> = after.iter().map(|l| l.id).collect();
    assert_eq!(ids, (1..=10).collect::<Vec<i64>>(), "numbered again: ids stay unique");
    let uids: std::collections::HashSet<uuid::Uuid> = after.iter().map(|l| l.uid).collect();
    assert_eq!(uids.len(), 10);
    assert!(vm.embedded_lines().unwrap().complete);
    assert!(!vm.embedded_resumes(3), "read to the end now");
    assert!(vm.has_unsaved_edits());
    // One undo step removes exactly the added lines.
    vm.undo();
    let undone: Vec<(uuid::Uuid, String, f64)> = vm.sentences().into_iter().map(|l| (l.uid, l.text, l.start)).collect();
    assert_eq!(undone, before);
    // Another file replaced the lines meanwhile: nothing is added.
    let srt = env.touch("m2/other.srt", "1\n00:00:00,500 --> 00:00:01,250\nDosya\n");
    assert!(vm.load_subtitle_file(&srt));
    assert_eq!(vm.append_embedded_rest(&video, 3, all), None);
    assert_eq!(vm.sentences().len(), 1);
}

/// M4 (Windows): opening the video again looks for a subtitle next to it again, and one that appeared replaces lines
/// read from the video that nobody changed.
#[test]
fn on_windows_a_subtitle_that_appeared_replaces_unedited_embedded_lines_when_the_video_opens_again() {
    let _lang = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    let (vm, _catalog, video) = opened(&env, "m4/film.mkv");
    let SnapshotOutcome::Applied(_) = vm.apply_embedded_snapshot(&video, 3, vm.session_mark(), lines(3), true) else { panic!("applied") };
    let other = env.touch("m4/other.mkv", "not really a video");
    vm.open_video(Some(other));
    env.pump();
    let srt = env.touch("m4/film.srt", "1\n00:00:00,500 --> 00:00:01,250\nYandaki\n");
    vm.open_video(Some(video.clone()));
    env.eventually("the sidecar", || vm.sentences().len() == 1 && vm.sentences()[0].text == "Yandaki");
    assert!(vm.embedded_lines().is_none());
    assert_eq!(vm.loaded_subtitle(), Some(srt));
    // Edited embedded lines stay the user's.
    let edited = env.touch("m4b/film.mkv", "not really a video");
    vm.open_video(Some(edited.clone()));
    env.eventually("the sidecar lookup", || vm.embedded_auto_choice(&edited) != AutoChoice::Wait);
    let SnapshotOutcome::Applied(_) = vm.apply_embedded_snapshot(&edited, 3, vm.session_mark(), lines(3), true) else { panic!("applied") };
    let uid = vm.sentences()[0].uid;
    vm.select_row(uid);
    vm.set_text(uid, "Benim");
    vm.open_video(Some(env.touch("m4b/other.mkv", "not really a video")));
    env.pump();
    env.touch("m4b/film.srt", "1\n00:00:00,500 --> 00:00:01,250\nYandaki\n");
    vm.open_video(Some(edited));
    env.eventually("the sidecar lookup", || !vm.sidecar_lookup_pending());
    assert_eq!(vm.sentences().len(), 3);
    assert_eq!(vm.sentences()[0].text, "Benim");
}

/// L1: text being typed into an embedded line is the user's work: no longer pristine, and "Off" commits it and keeps
/// the lines.
#[test]
fn a_typed_draft_makes_embedded_lines_the_users_and_off_commits_it() {
    let _lang = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    let (vm, _catalog, video) = opened(&env, "l1/film.mkv");
    let SnapshotOutcome::Applied(_) = vm.apply_embedded_snapshot(&video, 3, vm.session_mark(), lines(2), true) else { panic!("applied") };
    let uid = vm.sentences()[0].uid;
    vm.select_row(uid);
    vm.begin_editing(uid);
    assert!(vm.embedded_lines().unwrap().pristine, "an editor opened on a line changes nothing yet");
    vm.set_draft(uid, "Yazıyorum");
    assert!(!vm.embedded_lines().unwrap().pristine, "a typed draft counts as the user's work");
    assert!(!vm.has_unsaved_edits(), "not committed yet");
    assert!(!vm.clear_embedded_lines(), "\"Off\" commits the draft and keeps the lines");
    assert_eq!(vm.sentences().len(), 2);
    assert_eq!(vm.sentences()[0].text, "Yazıyorum");
    assert!(vm.has_unsaved_edits());
    // A draft typed back to the line's text is nothing.
    let other = vm.sentences()[1].uid;
    vm.begin_editing(other);
    vm.set_draft(other, "x");
    vm.set_draft(other, "Satır 1");
    let s = vm.sentences();
    assert_eq!(s[1].text, "Satır 1");
}

/// Renders the foreground once (as the macOS export panel does) and waits for it: the engine's render count then. A
/// refused write never asks for a render, so a count of 1 means only this one ran (renders start in order).
fn renders_up_to_a_marker(env: &Env, vm: &sa_viewmodels::SubtitleViewModel) -> usize {
    let done = std::rc::Rc::new(std::cell::Cell::new(false));
    let d = done.clone();
    vm.render_export(Box::new(move |_| d.set(true)));
    env.eventually("the marker render", || done.get());
    env.engine.renders.lock().len()
}

fn start_two_video_run(env: &Env, dir: &str) -> (sa_viewmodels::SubtitleViewModel, std::rc::Rc<sa_core::models::ModelCatalog>, std::path::PathBuf, std::path::PathBuf) {
    env.settings.set_string(sa_core::settings::keys::TRANSLATION_TARGET, "tr");
    let (vm, catalog) = env.vm();
    let a = env.touch(&format!("{dir}/a.mp4"), "v");
    let b = env.touch(&format!("{dir}/b.mp4"), "v");
    vm.add_playlist_videos_with_language(vec![a.clone(), b.clone()], "en");
    vm.start_playlist();
    (vm, catalog, a, b)
}

/// L1/L8: the stage-3 backstop. Lines that are a video's track nobody changed, turning up in a batch item between
/// its transcription and its translation (no flow of the app puts them there; the hook does), are not translated or
/// written, and the row fails, freeing the session.
#[test]
fn stage_three_refuses_unchanged_track_lines_and_fails_the_row() {
    let _lang = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    let (vm, _catalog, a, b) = start_two_video_run(&env, "backstop");
    let t1 = env.next(common::CallKind::Transcribe);
    t1.result("en", "", vec![common::s(1, 0.0, 1.0, "one")]);
    let tr1 = env.next(common::CallKind::Translate);
    let t2 = env.next(common::CallKind::Transcribe);
    t2.result("en", "", vec![common::s(1, 0.0, 1.0, "two")]);
    // Video b waits for a's translation (one at a time) before its own.
    env.eventually("b waits", || vm.playlist()[1].status_text().contains("Çeviri hazırlanıyor"));
    vm.put_track_lines_for_tests(&b, 3, lines(2));
    tr1.translation("tr", vec![common::s(1, 0.0, 1.0, "bir")]);
    tr1.finish();
    env.eventually("b failed", || vm.playlist()[1].state == sa_viewmodels::types::PlaylistItemState::Failed);
    assert_eq!(vm.playlist()[1].status_text(), "Videodan okunan altyazı değiştirilmeden yazılmaz.");
    env.eventually("the run ended", || !vm.is_playlist_running());
    assert_eq!(env.engine.calls_of(common::CallKind::Translate).len(), 1, "the track's lines were not translated");
    assert!(a.with_extension("srt").exists() && !b.with_extension("srt").exists());
    // The session is the user's again: its video can be generated.
    vm.open_video(Some(b));
    env.pump();
    assert!(vm.can_generate());
}

/// L1/L8: a video added to the list mid-run whose track was read before it was added: transcribed, not translated
/// from the track.
#[test]
fn a_video_added_mid_run_with_its_track_in_the_list_is_transcribed() {
    let _lang = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    env.settings.set_string(sa_core::settings::keys::TRANSLATION_TARGET, "tr");
    let (vm, _catalog) = env.vm();
    let a = env.touch("midrun/a.mp4", "v");
    vm.add_playlist_videos_with_language(vec![a.clone()], "en");
    vm.start_playlist();
    let t1 = env.next(common::CallKind::Transcribe);
    // While a is transcribed, the user opens c and its track loads; then c joins the list.
    let c = env.touch("midrun/c.mkv", "v");
    vm.open_video(Some(c.clone()));
    env.eventually("the sidecar lookup", || !vm.sidecar_lookup_pending());
    assert_eq!(vm.embedded_auto_choice(&c), AutoChoice::Allowed, "a run of other videos does not stop the choice");
    let SnapshotOutcome::Applied(_) = vm.apply_embedded_snapshot(&c, 3, vm.session_mark(), lines(3), true) else { panic!("applied") };
    vm.add_playlist_videos_with_language(vec![c.clone()], "en");
    t1.result("en", "", vec![common::s(1, 0.0, 1.0, "one")]);
    let tr1 = env.next(common::CallKind::Translate);
    let t2 = env.next(common::CallKind::Transcribe);
    assert_eq!(t2.record.video.as_deref(), Some(c.as_path()), "c is transcribed");
    t2.result("en", "", vec![common::s(1, 0.0, 1.0, "speech")]);
    tr1.translation("tr", vec![common::s(1, 0.0, 1.0, "bir")]);
    tr1.finish();
    let tr2 = env.next(common::CallKind::Translate);
    let texts: Vec<String> = tr2.record.sentences.iter().map(|l| l.text.clone()).collect();
    assert_eq!(texts, vec!["speech"], "the transcript is translated, not the track");
    tr2.translation("tr", vec![common::s(1, 0.0, 1.0, "konuşma")]);
    tr2.finish();
    env.eventually("completed", || playlist_states(&vm) == vec![sa_viewmodels::types::PlaylistItemState::Completed; 2]);
}

/// L1/L8: a track chosen by hand for a queued video after the run started (its complete lines replace the empty
/// list) is dropped when the run gets to it.
#[test]
fn a_track_chosen_by_hand_for_a_queued_video_is_dropped_when_the_run_gets_to_it() {
    let _lang = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    let (vm, _catalog, _a, b) = start_two_video_run(&env, "byhand");
    let t1 = env.next(common::CallKind::Transcribe);
    vm.open_video(Some(b.clone()));
    env.eventually("the sidecar lookup", || !vm.sidecar_lookup_pending());
    assert_eq!(vm.embedded_auto_choice(&b), AutoChoice::Refused, "queued: nothing chosen for the user");
    assert_eq!(vm.apply_embedded_snapshot(&b, 3, vm.session_mark(), lines(2), false), SnapshotOutcome::Superseded, "no progressive read");
    vm.request_embedded_lines(&b, 3, lines(3));
    assert_eq!(vm.sentences().len(), 3, "the user's choice loads it");
    t1.result("en", "", vec![common::s(1, 0.0, 1.0, "one")]);
    let tr1 = env.next(common::CallKind::Translate);
    let t2 = env.next(common::CallKind::Transcribe);
    assert_eq!(t2.record.video.as_deref(), Some(b.as_path()));
    t2.result("en", "", vec![common::s(1, 0.0, 1.0, "speech")]);
    tr1.translation("tr", vec![common::s(1, 0.0, 1.0, "bir")]);
    tr1.finish();
    let tr2 = env.next(common::CallKind::Translate);
    assert_eq!(tr2.record.sentences.iter().map(|l| l.text.clone()).collect::<Vec<_>>(), vec!["speech"]);
    tr2.translation("tr", vec![common::s(1, 0.0, 1.0, "konuşma")]);
    tr2.finish();
    env.eventually("completed", || playlist_states(&vm) == vec![sa_viewmodels::types::PlaylistItemState::Completed; 2]);
}

/// L2: a batch run of other videos leaves the foreground's read alone: its snapshots still go in and the session
/// mark does not move.
#[test]
fn a_batch_run_of_other_videos_leaves_the_foregrounds_read_alone() {
    let _lang = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    env.settings.set_string(sa_core::settings::keys::TRANSLATION_TARGET, "tr");
    let (vm, _catalog, video) = opened(&env, "l2/film.mkv");
    let SnapshotOutcome::Applied(mark) = vm.apply_embedded_snapshot(&video, 3, vm.session_mark(), lines(2), false) else { panic!("applied") };
    vm.set_embedded_read(&video, Some(0.2));
    let other = env.touch("l2/other.mp4", "v");
    vm.add_playlist_videos_with_language(vec![other.clone()], "en");
    vm.start_playlist();
    let _t = env.next(common::CallKind::Transcribe);
    assert!(vm.is_playlist_running());
    assert_eq!(vm.session_mark(), mark, "the run is not this session's business");
    assert!(!vm.embedded_video_in_batch(&video));
    assert!(vm.embedded_video_in_batch(&other));
    assert_eq!(vm.embedded_read_progress(), Some(0.2));
    assert!(matches!(vm.apply_embedded_snapshot(&video, 3, mark, lines(4), false), SnapshotOutcome::Applied(_)));
    assert_eq!(vm.sentences().len(), 4);
}

/// L3: once the user changes the lines a read is filling, they are theirs: saving works at once, while the read
/// is still running.
#[test]
fn edited_lines_can_be_saved_while_the_read_is_still_running() {
    let _lang = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    let (vm, _catalog, video) = opened(&env, "l3/film.mkv");
    let SnapshotOutcome::Applied(_) = vm.apply_embedded_snapshot(&video, 3, vm.session_mark(), lines(2), false) else { panic!("applied") };
    vm.set_embedded_read(&video, Some(0.3));
    assert!(!vm.can_save_subtitle());
    // Typing alone lifts the wait (Ctrl+S commits it and saves).
    let uid = vm.sentences()[0].uid;
    vm.select_row(uid);
    vm.begin_editing(uid);
    vm.set_draft(uid, "Benim");
    assert!(vm.can_save_subtitle() && vm.can_export() && vm.can_translate());
    assert!(!vm.status_text().contains("Okunuyor"), "{}", vm.status_text());
    vm.save_subtitle_next_to_video();
    let srt = video.with_extension("srt");
    env.eventually("saved", || srt.exists());
    assert_eq!(env.engine.renders.lock().last().map(|(l, _)| l[0].text.clone()).as_deref(), Some("Benim"));
}

/// L4: a batch run reaching a video whose track lines hold a typed draft commits it (the typing is the user's) and
/// then treats the lines as the user's; an automatic write never goes by a draft alone.
#[test]
fn a_batch_run_commits_a_draft_typed_into_the_track_lines() {
    let _lang = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    env.settings.set_string(sa_core::settings::keys::TRANSLATION_TARGET, "tr");
    let (vm, _catalog, video) = opened(&env, "l4/film.mkv");
    let SnapshotOutcome::Applied(_) = vm.apply_embedded_snapshot(&video, 3, vm.session_mark(), lines(2), true) else { panic!("applied") };
    let uid = vm.sentences()[0].uid;
    vm.select_row(uid);
    vm.begin_editing(uid);
    vm.set_draft(uid, "Yazdığım");
    // A draft alone does not make the track's own lines writable.
    vm.save_automatically();
    assert_eq!(renders_up_to_a_marker(&env, &vm), 1);
    vm.add_playlist_videos_with_language(vec![video.clone()], "en");
    vm.start_playlist();
    let tr = env.next(common::CallKind::Translate);
    let texts: Vec<String> = tr.record.sentences.iter().map(|l| l.text.clone()).collect();
    assert_eq!(texts, vec!["Yazdığım", "Satır 1"], "the committed draft is part of the user's lines");
    assert!(env.engine.calls_of(common::CallKind::Transcribe).is_empty());
    assert_eq!(vm.sentences()[0].text, "Yazdığım");
    tr.translation("tr", tr.record.sentences.iter().map(|x| common::s(x.id, x.start, x.end, &format!("T:{}", x.text))).collect());
    tr.finish();
    env.eventually("completed", || playlist_states(&vm) == vec![sa_viewmodels::types::PlaylistItemState::Completed]);
}

/// L5/L6: the rest of a track goes in by start time among the user's lines (a line they moved late stays theirs,
/// in time order), undo brings back the stopped state so the rest can be added again, and an emptied list gets the
/// whole track.
#[test]
fn the_rest_of_a_track_merges_by_time_and_its_undo_can_be_redone() {
    let _lang = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    let (vm, _catalog, video) = opened(&env, "l5/film.mkv");
    // Lines 0..9 start at 0, 2, 4, …; the stopped read had 0..3.
    let all = lines(10);
    let SnapshotOutcome::Applied(_) = vm.apply_embedded_snapshot(&video, 3, vm.session_mark(), all[..4].to_vec(), false) else { panic!("applied") };
    // The user moves the last line read (6.0) to 11.0–12.0, past where the read stopped.
    let moved = vm.sentences()[3].uid;
    vm.select_row(moved);
    vm.set_edge_to_now(sa_viewmodels::types::TimingEdge::End, 12.0);
    vm.set_edge_to_now(sa_viewmodels::types::TimingEdge::Start, 11.0);
    env.pump();
    assert_eq!(vm.sentences()[3].start, 11.0);
    assert!(vm.embedded_resumes(3));
    assert_eq!(vm.append_embedded_rest(&video, 3, all.clone()), Some(6));
    let texts: Vec<String> = vm.sentences().into_iter().map(|l| l.text).collect();
    assert_eq!(texts, vec!["Satır 0", "Satır 1", "Satır 2", "Satır 4", "Satır 5", "Satır 3", "Satır 6", "Satır 7", "Satır 8", "Satır 9"]);
    let starts: Vec<f64> = vm.sentences().into_iter().map(|l| l.start).collect();
    assert!(starts.windows(2).all(|w| w[0] <= w[1]), "in time order: {starts:?}");
    assert_eq!(vm.sentences().into_iter().map(|l| l.id).collect::<Vec<_>>(), (1..=10).collect::<Vec<i64>>());
    // Undo restores the stopped state: the rest can be added again.
    vm.undo();
    assert_eq!(vm.sentences().len(), 4);
    assert!(vm.embedded_resumes(3), "stopped early again");
    assert!(!vm.embedded_lines().unwrap().complete);
    vm.redo();
    assert_eq!(vm.sentences().len(), 10);
    assert!(vm.embedded_lines().unwrap().complete);
    vm.undo();
    vm.select_row(vm.sentences()[0].uid);
    vm.set_text(vm.sentences()[0].uid, "Değişti");
    assert_eq!(vm.append_embedded_rest(&video, 3, all.clone()), Some(6), "added again after an undo and another edit");
    // Every line deleted: the whole track.
    vm.undo();
    vm.undo();
    for l in vm.sentences() {
        vm.select_row(l.uid);
        vm.delete_selected();
    }
    assert!(vm.sentences().is_empty());
    assert!(vm.embedded_resumes(3));
    assert_eq!(vm.append_embedded_rest(&video, 3, all), Some(10));
    assert_eq!(vm.sentences().len(), 10);
}
