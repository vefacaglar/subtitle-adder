//! The player's audio / subtitle track menu (VLC-style; an addition of the Rust app, `PARITY.md`) and the state of
//! reading an embedded text subtitle track into the subtitle list.
//!
//! The menu lists every audio stream (the one playing marked) and every subtitle stream of the file. Choosing an audio
//! entry switches the player's audio track. Choosing a text subtitle entry reads that track out of the file
//! ([`EmbeddedSubtitleReads`] tracks the one read in flight) into a JSON sentence file that the app imports exactly
//! like a subtitle file the user picked (same view-model call, same confirmation and replacement rules), unless the
//! session changed while the read ran ([`SessionMark`]). Image-based tracks are listed but disabled until bitmap
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
}

impl TrackChoice {
    pub fn id(self) -> String {
        match self {
            TrackChoice::Audio(i) => format!("audio:{i}"),
            TrackChoice::Subtitle(i) => format!("subtitle:{i}"),
        }
    }

    pub fn parse(id: &str) -> Option<Self> {
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
    /// list holds now.
    pub fn build(tracks: &MediaTracks, reading: Option<(usize, f64)>, loaded: Option<usize>) -> Self {
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

/// The display name of a container language tag (ISO 639-2 `eng` / `ger` / `deu`, ISO 639-1 `en`, BCP 47 `en-US`):
/// the app's language names where it has one, else the tag in capitals; `None` for a blank or undetermined tag.
pub fn language_name(tag: &str) -> Option<String> {
    let tag = tag.trim();
    let base = tag.split(['-', '_']).next().unwrap_or("").to_ascii_lowercase();
    if base.is_empty() || base == "und" || base == "zxx" || base == "mis" || base == "mul" {
        return None;
    }
    let code = match base.as_str() {
        "tr" | "tur" => "tr",
        "en" | "eng" => "en",
        "de" | "deu" | "ger" => "de",
        "fr" | "fra" | "fre" => "fr",
        "es" | "spa" => "es",
        "pt" | "por" => "pt",
        "ru" | "rus" => "ru",
        "ja" | "jpn" => "ja",
        "zh" | "zho" | "chi" => "zh",
        _ => return Some(tag.to_uppercase()),
    };
    SubtitleLanguage::from_code(code).map(|l| l.short_name())
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
    let reason = move || match &e {
        ExtractError::Cancelled => String::new(),
        ExtractError::NotText => sa_core::l("Bu parça metin altyazısı değil"),
        ExtractError::TooLarge => sa_core::l("Altyazı parçası çok büyük"),
        ExtractError::Failed(d) => details_text(d),
    };
    match error {
        ExtractError::Cancelled => None,
        _ => Some(LocalizedMessage::new(format!("embedded-subtitle-failed:{error}"), move || {
            sa_core::l!("Videodaki altyazı okunamadı: %@", reason().as_str())
        })),
    }
}

/// The folder the embedded tracks are written to, under the app's temporary work folder (cleaned at launch, and of
/// this session's files on exit).
pub fn embedded_subtitle_dir(work_dir: &Path) -> PathBuf {
    work_dir.join("embedded-subtitles")
}

/// File names stay well under the 255-character limit of NTFS / APFS (the stem is cut on a character boundary).
const MAX_STEM_CHARS: usize = 120;

/// Where the text of embedded track `stream` of `video` is written for the importer, in
/// [`embedded_subtitle_dir`]: named after the video so the import looks like `<video>-<hash>.<stream>.json`.
pub fn embedded_subtitle_path(work_dir: &Path, video: &Path, stream: usize) -> PathBuf {
    let stem = video.file_stem().map(|s| s.to_string_lossy().into_owned()).filter(|s| !s.is_empty()).unwrap_or_else(|| "video".into());
    let stem: String = stem.chars().take(MAX_STEM_CHARS).collect();
    // A short hash of the full path keeps two videos with the same name apart.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in video.to_string_lossy().as_bytes() {
        h = (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    embedded_subtitle_dir(work_dir).join(format!("{stem}-{:08x}.{stream}.json", h as u32))
}

/// The foreground session's lines, translations and job as they were when a read started
/// ([`crate::subtitle::SubtitleViewModel::session_mark`]). A read of a big Matroska file can take a minute; when the
/// mark differs at its end (the user edited, a job started or finished), the result is not imported silently, since
/// importing replaces the lines, clears undo and stops a running job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionMark {
    pub content: u64,
    pub job: uuid::Uuid,
    pub working: bool,
}

/// `Videodaki altyazı yüklenmedi: …` (the session changed while the track was read; choosing it again imports it).
pub fn embedded_read_outdated() -> LocalizedMessage {
    LocalizedMessage::new("embedded-subtitle-outdated", || {
        sa_core::l("Videodaki altyazı yüklenmedi: okunurken altyazı değişti. Parçayı yeniden seç.")
    })
}

/// A read that [`EmbeddedSubtitleReads::start`] began: its id and its cancellation token.
#[derive(Debug, Clone)]
pub struct ReadTicket {
    pub id: u64,
    pub token: CancellationToken,
}

/// A read that ended while it was still the current one ([`EmbeddedSubtitleReads::finish`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinishedRead {
    pub video: PathBuf,
    pub stream: usize,
    /// The session when the read started.
    pub mark: SessionMark,
}

impl FinishedRead {
    /// Whether the result may be imported now: the session is as it was when the read started.
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
}

/// The embedded subtitle track being read (at most one: a new choice or another video cancels the previous read).
#[derive(Debug, Default)]
pub struct EmbeddedSubtitleReads {
    next: u64,
    current: Option<Read>,
}

impl EmbeddedSubtitleReads {
    /// Starts reading `stream` of `video`; `None` when exactly that read is already running. Any other read is
    /// cancelled.
    pub fn start(&mut self, video: &Path, stream: usize, mark: SessionMark) -> Option<ReadTicket> {
        if self.current.as_ref().is_some_and(|r| r.video == video && r.stream == stream) {
            return None;
        }
        self.cancel();
        self.next += 1;
        let token = CancellationToken::new();
        self.current = Some(Read { id: self.next, video: video.to_path_buf(), stream, token: token.clone(), fraction: 0.0, mark });
        Some(ReadTicket { id: self.next, token })
    }

    /// Cancels the read in flight (another video was opened, the window closes).
    pub fn cancel(&mut self) {
        if let Some(r) = self.current.take() {
            r.token.cancel();
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
            Some(r) if r.id == id => self.current.take().map(|r| FinishedRead { video: r.video, stream: r.stream, mark: r.mark }),
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
