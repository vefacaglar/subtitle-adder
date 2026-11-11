//! The player's track menu model (`sa_viewmodels::media_tracks`): labels, the active marks, disabled image-based
//! entries, when the control shows at all, and the embedded-subtitle read bookkeeping.

mod common;

use std::path::Path;

use sa_core::l10n::{CatalogChain, UiLanguage};
use sa_core::platform::PlatformShell;
use sa_player::{AudioTrack, ExtractError, MediaTracks, SubtitleKind, SubtitleTrack};
use sa_viewmodels::media_tracks::{
    EmbeddedSubtitleReads, IMAGE_SUMMARY_ID, MAX_LISTED_IMAGE_TRACKS, SessionMark, TrackChoice, TrackMenu, audio_label, audio_track_failure,
    channels_text, codec_title, embedded_read_failure, embedded_read_outdated, embedded_subtitle_dir, embedded_subtitle_path, has_choices,
    language_name,
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
    let menu = TrackMenu::build(&sample(), None, None);
    let labels: Vec<&str> = menu.audio.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(labels, vec!["English (AAC, stereo)", "English — Commentary (AC-3, 5.1)", "Track 3 (Opus, mono)"]);
    let labels: Vec<&str> = menu.subtitles.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(labels, vec!["Turkish (SRT)", "English — SDH (ASS, forced)", "Japanese (PGS)"]);
    lang.set(UiLanguage::Tr);
    let menu = TrackMenu::build(&sample(), None, None);
    assert_eq!(menu.audio[1].label, "İngilizce — Commentary (AC-3, 5.1)");
    assert_eq!(menu.audio[2].label, "Parça 3 (Opus, mono)");
    assert_eq!(menu.subtitles[1].label, "İngilizce — SDH (ASS, zorunlu)");
    // The same file on the macOS chain.
    drop(lang);
    let _mac = LanguageScope::new(CatalogChain::Mac, UiLanguage::En);
    assert_eq!(TrackMenu::build(&sample(), None, None).subtitles[0].label, "Turkish (SRT)");
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
    let menu = TrackMenu::build(&sample(), None, Some(5));
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
    let menu = TrackMenu::build(&sample(), Some((4, 0.427)), None);
    assert!(!menu.subtitles[0].enabled);
    assert_eq!(menu.subtitles[0].hint, "Reading… 42%");
    assert!(menu.subtitles[1].enabled, "another track can be chosen (it replaces the read)");
    lang.set(UiLanguage::Tr);
    assert_eq!(TrackMenu::build(&sample(), Some((4, 0.427)), None).subtitles[0].hint, "Okunuyor… %42");
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
    assert!(TrackMenu::build(&two_audio, None, None).subtitles.is_empty());
}

fn mark(content: u64) -> SessionMark {
    SessionMark { content, job: uuid::Uuid::nil(), working: false }
}

#[test]
fn one_read_at_a_time_and_a_cancelled_or_replaced_read_is_dropped() {
    let mut reads = EmbeddedSubtitleReads::default();
    let a = reads.start(Path::new("/v.mkv"), 4, mark(1)).expect("started");
    assert!(reads.start(Path::new("/v.mkv"), 4, mark(1)).is_none(), "the same read is not started twice");
    assert!(reads.progress(a.id, 0.5));
    assert_eq!(reads.reading(), Some((4, 0.5)));
    // Another track replaces it: the first is cancelled and its result is ignored.
    let b = reads.start(Path::new("/v.mkv"), 5, mark(2)).expect("started");
    assert!(a.token.is_cancelled() && !b.token.is_cancelled());
    assert!(!reads.progress(a.id, 0.9));
    assert_eq!(reads.finish(a.id), None);
    let done = reads.finish(b.id).expect("current");
    assert_eq!((done.video.as_path(), done.stream, done.mark), (Path::new("/v.mkv"), 5, mark(2)));
    assert!(done.still_applies(mark(2)) && !done.still_applies(mark(3)));
    assert!(!reads.is_reading());
    // Opening another video cancels the read in flight.
    let c = reads.start(Path::new("/v.mkv"), 4, mark(1)).expect("started");
    reads.cancel();
    assert!(c.token.is_cancelled());
    assert_eq!(reads.finish(c.id), None);
}

#[test]
fn many_image_tracks_collapse_into_one_disabled_row_and_unsupported_text_tracks_are_disabled() {
    let lang = LanguageScope::new(CatalogChain::Windows, UiLanguage::En);
    let mut tracks = sample();
    tracks.subtitles.push(sub(7, Some("ara"), None, "arib_caption", SubtitleKind::Unsupported, false));
    let menu = TrackMenu::build(&tracks, None, None);
    let unsupported = menu.subtitles.last().unwrap();
    assert!(!unsupported.enabled);
    assert_eq!(unsupported.hint, "Not supported");
    // Up to MAX_LISTED_IMAGE_TRACKS image tracks are listed one by one.
    for i in 0..MAX_LISTED_IMAGE_TRACKS - 1 {
        tracks.subtitles.push(sub(10 + i, Some("deu"), None, "hdmv_pgs_subtitle", SubtitleKind::Image, false));
    }
    assert_eq!(TrackMenu::build(&tracks, None, None).subtitles.len(), 4 + MAX_LISTED_IMAGE_TRACKS - 1);
    // A Blu-ray remux: 30 PGS tracks become one row; the text tracks stay.
    for i in 0..30 {
        tracks.subtitles.push(sub(20 + i, Some("fra"), None, "hdmv_pgs_subtitle", SubtitleKind::Image, false));
    }
    let images = tracks.subtitles.iter().filter(|t| t.kind == SubtitleKind::Image).count();
    let menu = TrackMenu::build(&tracks, None, None);
    assert_eq!(menu.subtitles.len(), 4, "{:?}", menu.subtitles);
    let summary = menu.subtitles.last().unwrap();
    assert_eq!((summary.id.as_str(), summary.enabled, summary.active), (IMAGE_SUMMARY_ID, false, false));
    assert_eq!(summary.label, format!("Image-based subtitle tracks: {images}"));
    assert_eq!(summary.hint, "Not supported");
    assert_eq!(TrackChoice::parse(&summary.id), None);
    lang.set(UiLanguage::Tr);
    let menu = TrackMenu::build(&tracks, None, None);
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
}

#[test]
fn the_file_name_stays_short_for_a_long_video_name() {
    let work = Path::new("/tmp/SubtitleAdder");
    let long = format!("/films/{}.mkv", "ü".repeat(400));
    let path = embedded_subtitle_path(work, Path::new(&long), 12);
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    assert!(name.chars().count() < 255 && name.ends_with(".12.json"), "{name}");
    assert_eq!(path.parent().unwrap(), embedded_subtitle_dir(work));
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

#[test]
fn an_extracted_track_is_imported_through_the_subtitle_file_path() {
    let _lang = LanguageScope::new(CatalogChain::Windows, UiLanguage::Tr);
    let env = Env::win();
    let (vm, _catalog) = env.vm();
    let video = env.touch("movie.mkv", "not really a video");
    vm.open_video(Some(video.clone()));
    env.pump();
    let path = embedded_subtitle_path(&env.shell.engine_work_dir(), &video, 3);
    assert!(path.starts_with(env.shell.engine_work_dir()));
    assert_eq!(path.extension().and_then(|e| e.to_str()), Some("json"));
    assert_ne!(path, embedded_subtitle_path(&env.shell.engine_work_dir(), Path::new("/other/movie.mkv"), 3), "same name, other folder");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    // What `extract_text_subtitles` writes: the app's JSON sentence list (literal braces survive).
    let doc = r#"[{"id":1,"start":0.5,"end":1.25,"text":"Merhaba {dünya}","words":[]}]"#;
    std::fs::write(&path, doc).unwrap();
    assert!(vm.load_subtitle_file(&path));
    env.pump();
    let lines = vm.sentences();
    assert_eq!(lines.len(), 1);
    assert_eq!((lines[0].start, lines[0].end, lines[0].text.as_str()), (0.5, 1.25, "Merhaba {dünya}"));
    assert_eq!(vm.loaded_subtitle(), Some(path));
    assert_eq!(vm.video(), Some(video), "the video stays open");
}
