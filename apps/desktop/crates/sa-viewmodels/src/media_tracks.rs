//! The player's audio / subtitle track menu (VLC-style; an addition of the Rust app, `PARITY.md`) and the state of
//! reading an embedded text subtitle track into the subtitle list.
//!
//! The menu lists every audio stream (the one playing marked) and every subtitle stream of the file. Choosing an audio
//! entry switches the player's audio track. Choosing a text subtitle entry reads that track out of the file
//! ([`EmbeddedSubtitleReads`] tracks the one read in flight); when a video opens with no lines, one is chosen for the
//! user ([`auto_track`]). The lines reach the list while the track is read (growing snapshots) when nothing the user
//! made is in it, otherwise at the end through the same confirmation and replacement rules as a subtitle file the
//! user picked; either way not when the session changed while the read ran ([`SessionMark`]). Lines read from the
//! video are not the user's work until they change them: no unsaved-changes prompt, no automatic save
//! (`crate::subtitle::SubtitleViewModel::embedded_lines`). Image-based tracks are listed but disabled until bitmap
//! subtitles are supported (more than [`MAX_LISTED_IMAGE_TRACKS`] as one summary row).

use std::path::{Path, PathBuf};

use sa_core::cancel::CancellationToken;
use sa_core::l10n::LocalizedMessage;
use sa_core::models::SubtitleLanguage;
use sa_player::{AudioTrack, ExtractError, MediaTracks, SubtitleKind, SubtitleTrack};

/// One menu entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackItem {
    /// `audio:<stream>` / `subtitle:<stream>` ([`TrackChoice::parse`]).
    pub id: String,
    pub label: String,
    /// The audio track playing / the embedded track whose lines are in the list.
    pub active: bool,
    pub enabled: bool,
    /// Short trailing note (reading progress, why an entry is disabled); empty for none.
    pub hint: String,
}

/// More image-based subtitle tracks than this are shown as one summary row ([`IMAGE_SUMMARY_ID`]).
pub const MAX_LISTED_IMAGE_TRACKS: usize = 3;
/// The id of that summary row (disabled; no [`TrackChoice`]).
pub const IMAGE_SUMMARY_ID: &str = "subtitle-images";

/// The two sections of the menu.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TrackMenu {
    pub audio: Vec<TrackItem>,
    pub subtitles: Vec<TrackItem>,
}

/// What a chosen entry asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackChoice {
    Audio(usize),
    Subtitle(usize),
    /// No embedded track for this video: clears lines that are an unedited embedded track, and none is chosen for
    /// the user when the video opens again in this run of the app.
    SubtitleOff,
}

/// The id of the [`TrackChoice::SubtitleOff`] entry.
pub const SUBTITLE_OFF_ID: &str = "subtitle:off";

impl TrackChoice {
    pub fn id(self) -> String {
        match self {
            TrackChoice::Audio(i) => format!("audio:{i}"),
            TrackChoice::Subtitle(i) => format!("subtitle:{i}"),
            TrackChoice::SubtitleOff => SUBTITLE_OFF_ID.to_owned(),
        }
    }

    pub fn parse(id: &str) -> Option<Self> {
        if id == SUBTITLE_OFF_ID {
            return Some(TrackChoice::SubtitleOff);
        }
        let (head, rest) = id.split_once(':')?;
        let i = rest.parse().ok()?;
        match head {
            "audio" => Some(TrackChoice::Audio(i)),
            "subtitle" => Some(TrackChoice::Subtitle(i)),
            _ => None,
        }
    }
}

/// Whether the player shows the track control at all: there is a choice to make (more than one audio track, or a
/// subtitle track in the file).
pub fn has_choices(tracks: &MediaTracks) -> bool {
    tracks.audio.len() > 1 || !tracks.subtitles.is_empty()
}

impl TrackMenu {
    /// `reading`: the embedded track being read and its progress (0…1); `loaded`: the embedded track whose lines the
    /// list holds now, `loaded_pristine`: those lines are still exactly the track's. A file with a text track gets an
    /// "Off" entry first: checked while no embedded track is in the list (an imported or generated subtitle
    /// included: the video's own tracks are off then), enabled then (it keeps a track from being chosen for the
    /// user) and while the loaded lines are unedited (it clears them).
    pub fn build(tracks: &MediaTracks, reading: Option<(usize, f64)>, loaded: Option<usize>, loaded_pristine: bool) -> Self {
        let audio = tracks
            .audio
            .iter()
            .enumerate()
            .map(|(n, t)| TrackItem {
                id: TrackChoice::Audio(t.stream_index).id(),
                label: audio_label(t, n + 1),
                active: tracks.active_audio == Some(t.stream_index),
                enabled: true,
                hint: String::new(),
            })
            .collect();
        let images = tracks.subtitles.iter().filter(|t| t.kind == SubtitleKind::Image).count();
        let collapse_images = images > MAX_LISTED_IMAGE_TRACKS;
        let mut subtitles: Vec<TrackItem> = tracks
            .subtitles
            .iter()
            .enumerate()
            .filter(|(_, t)| !(collapse_images && t.kind == SubtitleKind::Image))
            .map(|(n, t)| {
                let read = reading.filter(|(s, _)| *s == t.stream_index);
                let (enabled, hint) = match (t.kind, read) {
                    (SubtitleKind::Image, _) => (false, sa_core::l("Görüntü tabanlı, desteklenmiyor")),
                    (SubtitleKind::Unsupported, _) => (false, sa_core::l("Desteklenmiyor")),
                    (SubtitleKind::Text, Some((_, f))) => (false, sa_core::l!("Okunuyor… %lld%%", (f.clamp(0.0, 1.0) * 100.0).floor() as i64)),
                    (SubtitleKind::Text, None) => (true, String::new()),
                };
                TrackItem {
                    id: TrackChoice::Subtitle(t.stream_index).id(),
                    label: subtitle_label(t, n + 1),
                    active: loaded == Some(t.stream_index),
                    enabled,
                    hint,
                }
            })
            .collect();
        if tracks.subtitles.iter().any(SubtitleTrack::is_text) {
            let off = TrackItem {
                id: SUBTITLE_OFF_ID.to_owned(),
                label: sa_core::l("Kapalı"),
                active: loaded.is_none() && reading.is_none(),
                enabled: loaded.is_none() || loaded_pristine,
                hint: String::new(),
            };
            subtitles.insert(0, off);
        }
        if collapse_images {
            // A Blu-ray remux carries dozens of PGS tracks: one disabled row says how many instead.
            subtitles.push(TrackItem {
                id: IMAGE_SUMMARY_ID.to_owned(),
                label: sa_core::l!("Görüntü tabanlı altyazı parçaları: %lld", images as i64),
                active: false,
                enabled: false,
                hint: sa_core::l("Desteklenmiyor"),
            });
        }
        Self { audio, subtitles }
    }
}

/// `English — Commentary (AAC, stereo)`: language, title (unless it only repeats the language), then codec and
/// channels; `Track 2 (AC-3, 5.1)` when the stream names neither.
pub fn audio_label(t: &AudioTrack, position: usize) -> String {
    let details = [codec_title(&t.codec), channels_text(t.channels)];
    label(t.language.as_deref(), t.title.as_deref(), position, &details)
}

/// `Türkçe — SDH (SRT, forced)`.
pub fn subtitle_label(t: &SubtitleTrack, position: usize) -> String {
    let forced = if t.forced { sa_core::l("zorunlu") } else { String::new() };
    label(t.language.as_deref(), t.title.as_deref(), position, &[codec_title(&t.codec), forced])
}

fn label(language: Option<&str>, title: Option<&str>, position: usize, details: &[String]) -> String {
    let language = language.and_then(language_name);
    let title = title.map(str::trim).filter(|t| !t.is_empty() && !language.as_deref().is_some_and(|l| l.eq_ignore_ascii_case(t)));
    let name = match (language, title) {
        (Some(l), Some(t)) => format!("{l} — {t}"),
        (Some(l), None) => l,
        (None, Some(t)) => t.to_owned(),
        (None, None) => sa_core::l!("Parça %lld", position as i64),
    };
    let details: Vec<&str> = details.iter().map(String::as_str).filter(|d| !d.is_empty()).collect();
    if details.is_empty() { name } else { format!("{name} ({})", details.join(", ")) }
}

/// What a container language tag (ISO 639-2 `eng` / `ger` / `deu`, ISO 639-1 `en`, BCP 47 `en-US`) says.
enum Tag {
    /// Blank or undetermined (`und`, `zxx`, `mis`, `mul`).
    None,
    /// A language the app has a name for, as its ISO 639-1 code.
    Known(&'static str),
    Other,
}

fn parse_tag(tag: &str) -> Tag {
    let base = tag.trim().split(['-', '_']).next().unwrap_or("").to_ascii_lowercase();
    match base.as_str() {
        "" | "und" | "zxx" | "mis" | "mul" => Tag::None,
        "tr" | "tur" => Tag::Known("tr"),
        "en" | "eng" => Tag::Known("en"),
        "de" | "deu" | "ger" => Tag::Known("de"),
        "fr" | "fra" | "fre" => Tag::Known("fr"),
        "es" | "spa" => Tag::Known("es"),
        "pt" | "por" => Tag::Known("pt"),
        "ru" | "rus" => Tag::Known("ru"),
        "ja" | "jpn" => Tag::Known("ja"),
        "zh" | "zho" | "chi" => Tag::Known("zh"),
        _ => Tag::Other,
    }
}

/// The ISO 639-1 code of a container language tag for the languages the app knows (`tur` → `tr`, `en-US` → `en`).
pub fn language_code(tag: &str) -> Option<&'static str> {
    match parse_tag(tag) {
        Tag::Known(code) => Some(code),
        Tag::None | Tag::Other => None,
    }
}

/// The display name of a container language tag: the app's language names where it has one, else the tag in
/// capitals; `None` for a blank or undetermined tag.
pub fn language_name(tag: &str) -> Option<String> {
    match parse_tag(tag) {
        Tag::None => None,
        Tag::Known(code) => SubtitleLanguage::from_code(code).map(|l| l.short_name()),
        Tag::Other => Some(tag.trim().to_uppercase()),
    }
}

/// The embedded track loaded for the user when a video opens with no lines (VLC-like): the first text track in
/// `ui_language` (`tr` / `en`, the UI's), a non-forced one before a forced one; otherwise the first text track with
/// the default disposition; otherwise none. Image-based and unsupported tracks are never chosen.
pub fn auto_track(tracks: &MediaTracks, ui_language: &str) -> Option<usize> {
    let text = || tracks.subtitles.iter().filter(|t| t.kind == SubtitleKind::Text);
    let in_language = |t: &&SubtitleTrack| t.language.as_deref().and_then(language_code) == Some(ui_language);
    text()
        .filter(in_language)
        .min_by_key(|t| t.forced)
        .or_else(|| text().find(|t| t.default))
        .map(|t| t.stream_index)
}

/// The usual name of a codec (`AAC`, `E-AC-3`, `SRT`, `PGS`); other FFmpeg names in capitals.
pub fn codec_title(codec: &str) -> String {
    let c = codec.to_ascii_lowercase();
    let known = match c.as_str() {
        "" => "",
        "aac" | "aac_latm" => "AAC",
        "ac3" => "AC-3",
        "eac3" => "E-AC-3",
        "truehd" => "TrueHD",
        "dts" => "DTS",
        "flac" => "FLAC",
        "opus" => "Opus",
        "vorbis" => "Vorbis",
        "mp3" | "mp3float" => "MP3",
        "mp2" => "MP2",
        "alac" => "ALAC",
        "subrip" | "srt" => "SRT",
        "ass" => "ASS",
        "ssa" => "SSA",
        "mov_text" => "TX3G",
        "webvtt" => "WebVTT",
        "text" => "Text",
        "hdmv_pgs_subtitle" => "PGS",
        "dvd_subtitle" => "VobSub",
        "dvb_subtitle" => "DVB",
        "xsub" => "XSUB",
        _ if c.starts_with("pcm_") => "PCM",
        _ => return codec.to_uppercase(),
    };
    known.to_owned()
}

/// `mono`, `stereo`, `5.1`, `7.1`, `3 channels`; empty when unknown.
pub fn channels_text(channels: u16) -> String {
    match channels {
        0 => String::new(),
        1 => sa_core::l("mono"),
        2 => sa_core::l("stereo"),
        6 => "5.1".to_owned(),
        8 => "7.1".to_owned(),
        n => sa_core::l!("%lld kanal", n as i64),
    }
}

/// FFmpeg's (or the OS's) details for a `%@`, as the playback error does: the text as is, `Bilinmeyen hata` when
/// there is none.
fn details_text(details: &str) -> String {
    let d = details.trim();
    if d.is_empty() { sa_core::l("Bilinmeyen hata") } else { d.to_owned() }
}

/// `Ses parçası açılamadı: %@` (the player kept the previous audio track).
pub fn audio_track_failure(details: &str) -> LocalizedMessage {
    let d = details.to_owned();
    LocalizedMessage::new(format!("audio-track-failed:{details}"), move || sa_core::l!("Ses parçası açılamadı: %@", details_text(&d).as_str()))
}

/// `Videodaki altyazı okunamadı: %@` with the reason in the UI language; `None` for a cancelled read (nothing to
/// report).
pub fn embedded_read_failure(error: &ExtractError) -> Option<LocalizedMessage> {
    let e = error.clone();
    let reason = move || failure_reason(&e);
    match error {
        ExtractError::Cancelled => None,
        _ => Some(LocalizedMessage::new(format!("embedded-subtitle-failed:{error}"), move || {
            sa_core::l!("Videodaki altyazı okunamadı: %@", reason().as_str())
        })),
    }
}

/// `Videodaki altyazının yalnızca bir kısmı yüklendi: %@`: a read that failed after some of its lines were shown
/// (they stay); `None` for a cancelled read.
pub fn embedded_read_failure_partial(error: &ExtractError) -> Option<LocalizedMessage> {
    let e = error.clone();
    match error {
        ExtractError::Cancelled => None,
        _ => Some(LocalizedMessage::new(format!("embedded-subtitle-partial-failed:{error}"), move || {
            sa_core::l!("Videodaki altyazının yalnızca bir kısmı yüklendi: %@", failure_reason(&e).as_str())
        })),
    }
}

/// Why a read failed, in the UI language.
fn failure_reason(e: &ExtractError) -> String {
    match e {
        ExtractError::Cancelled => String::new(),
        ExtractError::NotText => sa_core::l("Bu parça metin altyazısı değil"),
        ExtractError::TooLarge => sa_core::l("Altyazı parçası çok büyük"),
        ExtractError::Failed(d) => details_text(d),
    }
}

/// `Videodaki altyazı okunamadı: Parçada satır yok` (a track that holds no text event).
pub fn embedded_read_empty() -> LocalizedMessage {
    LocalizedMessage::new("embedded-subtitle-empty", || sa_core::l!("Videodaki altyazı okunamadı: %@", sa_core::l("Parçada satır yok").as_str()))
}

/// The foreground session's lines, translations and job as they were when a read started, or as the read's last
/// snapshot left them ([`crate::subtitle::SubtitleViewModel::session_mark`]). A read of a big Matroska file can take a
/// minute; when the mark differs at a snapshot or at its end (the user edited, a job started or finished), the lines
/// are not replaced behind the user's back, since that replaces their work, clears undo and stops a running job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionMark {
    pub content: u64,
    pub job: uuid::Uuid,
    pub working: bool,
}

/// Whether an embedded track may be chosen for the user now that the video opened
/// ([`crate::subtitle::SubtitleViewModel::embedded_auto_choice`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoChoice {
    /// The session is empty and idle: choose one.
    Allowed,
    /// A subtitle file next to the video is still being looked for (it wins when there is one): ask again later.
    Wait,
    /// The session has lines, a loaded file, a job or another flow going on: leave it alone.
    Refused,
}

/// What became of a snapshot (or the complete lines) of a progressive read
/// ([`crate::subtitle::SubtitleViewModel::apply_embedded_snapshot`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotOutcome {
    /// In the list; the session is now this.
    Applied(SessionMark),
    /// The user changed the lines read so far (they are theirs now): the read stops, the rest is not added.
    Edited,
    /// Something else replaced them (a file, a job, another video): the read stops silently.
    Superseded,
}

/// `Videodaki altyazı yüklenmedi: …` (the session changed while the track was read; choosing it again imports it).
pub fn embedded_read_outdated() -> LocalizedMessage {
    LocalizedMessage::new("embedded-subtitle-outdated", || {
        sa_core::l("Videodaki altyazı yüklenmedi: okunurken altyazı değişti. Parçayı yeniden seç.")
    })
}

/// `Videodaki altyazının yalnızca bir kısmı yüklendi: …` (the user changed lines that were still coming in; the
/// rest was not added).
pub fn embedded_read_partial() -> LocalizedMessage {
    LocalizedMessage::new("embedded-subtitle-partial", || {
        sa_core::l("Videodaki altyazının yalnızca bir kısmı yüklendi: okunurken altyazı değişti. Parçayı yeniden seç.")
    })
}

/// A read that [`EmbeddedSubtitleReads::start`] began: its id and its cancellation token.
#[derive(Debug, Clone)]
pub struct ReadTicket {
    pub id: u64,
    pub token: CancellationToken,
}

/// How a read was asked for and how its lines reach the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadMode {
    /// Chosen for the user when the video opened ([`auto_track`]): its failures are not shown.
    pub automatic: bool,
    /// Snapshots go into the list while it reads (nothing the user made is in it); otherwise the complete track is
    /// applied at the end through the open-subtitle rules.
    pub progressive: bool,
    /// The list holds an earlier read of this track that stopped early and was edited since: only the lines past
    /// where it stopped are added at the end (`SubtitleViewModel::append_embedded_rest`).
    pub resume: bool,
}

impl ReadMode {
    /// A track chosen for the user on open (always into an empty list).
    pub const AUTOMATIC: Self = Self { automatic: true, progressive: true, resume: false };
}

/// A read that ended or was cancelled while it was still the current one ([`EmbeddedSubtitleReads::finish`],
/// [`EmbeddedSubtitleReads::cancel`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinishedRead {
    /// The session's video (the session the lines are for).
    pub video: PathBuf,
    pub stream: usize,
    /// The session when the read started, or as its last applied snapshot left it.
    pub mark: SessionMark,
    pub mode: ReadMode,
    /// A snapshot of it went into the list.
    pub applied: bool,
}

impl FinishedRead {
    /// Whether the result may be imported now: the session is as it was when the read started (or as its last
    /// snapshot left it).
    pub fn still_applies(&self, now: SessionMark) -> bool {
        self.mark == now
    }
}

#[derive(Debug)]
struct Read {
    id: u64,
    video: PathBuf,
    stream: usize,
    token: CancellationToken,
    fraction: f64,
    mark: SessionMark,
    mode: ReadMode,
    applied: bool,
}

impl Read {
    fn finished(self) -> FinishedRead {
        FinishedRead { video: self.video, stream: self.stream, mark: self.mark, mode: self.mode, applied: self.applied }
    }
}

/// The embedded subtitle track being read (at most one: a new choice or another video cancels the previous read).
#[derive(Debug, Default)]
pub struct EmbeddedSubtitleReads {
    next: u64,
    current: Option<Read>,
}

impl EmbeddedSubtitleReads {
    /// Starts reading `stream` of `video`; `None` when exactly that read is already running for this session state
    /// (`mark`; a choice by the user makes an automatic one theirs). Any other read is cancelled.
    pub fn start(&mut self, video: &Path, stream: usize, mark: SessionMark, mode: ReadMode) -> Option<ReadTicket> {
        if let Some(r) = self.current.as_mut()
            && r.video == video
            && r.stream == stream
            && r.mark == mark
        {
            r.mode.automatic &= mode.automatic;
            return None;
        }
        self.cancel();
        self.next += 1;
        let token = CancellationToken::new();
        self.current = Some(Read {
            id: self.next,
            video: video.to_path_buf(),
            stream,
            token: token.clone(),
            fraction: 0.0,
            mark,
            mode,
            applied: false,
        });
        Some(ReadTicket { id: self.next, token })
    }

    /// Cancels the read in flight (another video was opened, the window closes, the user changed the lines it was
    /// filling in) and returns it.
    pub fn cancel(&mut self) -> Option<FinishedRead> {
        let r = self.current.take()?;
        r.token.cancel();
        Some(r.finished())
    }

    /// The current read when it is `id` (its snapshots may be applied).
    pub fn current(&self, id: u64) -> Option<FinishedRead> {
        self.current.as_ref().filter(|r| r.id == id).and_then(|_| self.active())
    }

    /// The read in flight, whichever it is.
    pub fn active(&self) -> Option<FinishedRead> {
        self.current.as_ref().map(|r| FinishedRead {
            video: r.video.clone(),
            stream: r.stream,
            mark: r.mark,
            mode: r.mode,
            applied: r.applied,
        })
    }

    /// Read `id`'s snapshot went into the list, which is now `mark`.
    pub fn applied(&mut self, id: u64, mark: SessionMark) {
        if let Some(r) = self.current.as_mut().filter(|r| r.id == id) {
            r.mark = mark;
            r.applied = true;
        }
    }

    /// Progress of read `id`; true when it is the current read (the menu should show it).
    pub fn progress(&mut self, id: u64, fraction: f64) -> bool {
        match &mut self.current {
            Some(r) if r.id == id => {
                r.fraction = fraction.clamp(0.0, 1.0);
                true
            }
            _ => false,
        }
    }

    /// Read `id` ended; returns it when it is still the current read (its result may be used, see
    /// [`FinishedRead::still_applies`]), `None` for a read that was cancelled or replaced meanwhile.
    pub fn finish(&mut self, id: u64) -> Option<FinishedRead> {
        match &self.current {
            Some(r) if r.id == id => self.current.take().map(Read::finished),
            _ => None,
        }
    }

    /// The stream being read and its progress.
    pub fn reading(&self) -> Option<(usize, f64)> {
        self.current.as_ref().map(|r| (r.stream, r.fraction))
    }

    pub fn is_reading(&self) -> bool {
        self.current.is_some()
    }
}
