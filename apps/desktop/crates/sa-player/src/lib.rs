//! `sa-player`: the video player of both front ends (ARCHITECTURE.md §7).
//!
//! * Pure, always compiled: [`MediaPlayerBackend`] contract, [`VideoFrame`] (RGBA8 buffers or GPU-ready YUV planes
//!   handed to the UI), [`PlaybackTimeline`] and [`PlayerSessionGate`] (win-media §4; shared by both platforms).
//! * Feature `ffmpeg` (default): [`ffmpeg_backend::FfmpegPlayer`] — demux/decode with `ffmpeg-next` 8.1 (hardware
//!   decoding through D3D11VA on Windows and VideoToolbox on macOS, software otherwise), video handed over either as
//!   the decoder's YUV planes (the UI converts them to RGB on the GPU; [`MediaPlayerBackend::set_planar_output`]) or
//!   converted to RGBA by swscale, audio resampled by swresample to the output format and played through `cpal`
//!   (WASAPI / CoreAudio); the audio clock is the master clock (video frames are shown when their pts is due, late
//!   frames dropped). The backend also lists the file's audio and subtitle streams ([`MediaTracks`]), switches the
//!   audio track while playing or paused, and reads embedded text subtitle tracks
//!   (`ffmpeg_backend::subtitles::extract_text_subtitles`). View models depend on this crate **without** the feature.
//!
//! This replaces both IMFMediaEngine (Windows) and AVPlayer (macOS) with one decoder. Windows feeds the stream through
//! `sa_media::win::BufferedVideoStream` (custom AVIO context), so the encrypted SSD chunk cache keeps working.

use std::io::{Read, Seek};
use std::path::PathBuf;
use std::sync::Arc;

pub mod color;
pub mod gate;
pub mod soft;
pub mod timeline;

#[cfg(feature = "ffmpeg")]
pub mod ffmpeg_backend;

pub use gate::{PlayerEventToken, PlayerSessionGate, SourceId};
pub use timeline::PlaybackTimeline;

/// A decoded video frame. `width` × `height` is the size as shown (sample aspect ratio and rotation applied).
///
/// RGBA output (the default): `rgba` holds RGBA8, tightly packed (`stride == width * 4`) unless stated, and `planar`
/// is `None`. Planar output ([`MediaPlayerBackend::set_planar_output`]): `planar` holds the decoder's YUV planes
/// (zero-copy for software decoding, one copy out of the GPU for hardware decoding) and `rgba` is empty; the UI
/// uploads the planes as textures and converts them on the GPU ([`PlanarImage`] says how). Pixel formats without a
/// planar description (RGB, palette, packed YUV, big-endian...) are converted to RGBA by swscale and handed over as a
/// one-plane [`PlaneLayout::Packed`] image, so the planar path covers every frame.
///
/// An interlaced planar frame is handed over twice, once per field (`second_field` false, then true half a frame
/// later): the UI deinterlaces it by showing the field due at that time ([`PlanarImage::shown_field_is_bottom`]).
#[derive(Clone)]
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    /// Presentation time (s).
    pub pts: f64,
    pub rgba: Arc<Vec<u8>>,
    pub planar: Option<Arc<PlanarImage>>,
    /// The second of the two fields of an interlaced frame (always false for progressive frames).
    pub second_field: bool,
}

impl VideoFrame {
    /// An RGBA frame.
    pub fn from_rgba(width: u32, height: u32, stride: u32, pts: f64, rgba: Arc<Vec<u8>>) -> Self {
        Self { width, height, stride, pts, rgba, planar: None, second_field: false }
    }
}

impl std::fmt::Debug for VideoFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoFrame")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("pts", &self.pts)
            .field("planar", &self.planar.as_ref().map(|p| (p.layout, p.bits)))
            .field("second_field", &self.second_field)
            .finish()
    }
}

/// How the planes of a [`PlanarImage`] are laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaneLayout {
    /// Three planes Y, U, V (`yuv420p`, `yuv411p`, `yuva444p`, ... at 8–16 bits little-endian; an alpha plane is
    /// ignored).
    Planar,
    /// Two planes: Y and interleaved UV (`nv12`, `p010`, `p016`; what hardware decoders produce), or VU when
    /// [`PlanarImage::swap_uv`] (`nv21`).
    SemiPlanar,
    /// One luma plane, no chroma (`gray`, `gray10le`, ...).
    Gray,
    /// One plane of packed RGBA8, already R'G'B' (swscale's conversion of every other pixel format); alpha is ignored.
    Packed,
}

/// The transfer characteristic of a frame's signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transfer {
    /// BT.709 / BT.601 / sRGB / unspecified: shown as is.
    Sdr,
    /// SMPTE ST 2084 (HDR10).
    Pq,
    /// ARIB STD-B67 (hybrid log-gamma).
    Hlg,
}

/// The colour primaries of a frame (anything other than these is shown as BT.709).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Primaries {
    Bt709,
    Bt2020,
    /// SMPTE EG 432-1 (P3 with a D65 white point).
    DisplayP3,
}

/// Colour description of a [`PlanarImage`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorInfo {
    pub matrix: YuvMatrix,
    pub full_range: bool,
    pub transfer: Transfer,
    pub primaries: Primaries,
    /// The content peak HDR is tone-mapped from (nits; see [`color::hdr_peak_nits`]).
    pub peak_nits: f32,
}

impl Default for ColorInfo {
    fn default() -> Self {
        Self { matrix: YuvMatrix::Bt709, full_range: false, transfer: Transfer::Sdr, primaries: Primaries::Bt709, peak_nits: color::DEFAULT_PEAK_NITS }
    }
}

/// The YUV → RGB matrix of a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YuvMatrix {
    Bt601,
    Bt709,
    Bt2020,
}

impl YuvMatrix {
    /// `(kr, kb)` luma coefficients.
    pub fn coefficients(self) -> (f32, f32) {
        match self {
            YuvMatrix::Bt601 => (0.299, 0.114),
            YuvMatrix::Bt709 => (0.2126, 0.0722),
            YuvMatrix::Bt2020 => (0.2627, 0.0593),
        }
    }
}

/// One plane: `rows` rows `stride` bytes apart; the first `row_bytes` of every row are `width` samples (one byte per
/// sample component, or two for high bit depths; semi-planar chroma has two components per sample).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaneInfo {
    pub width: u32,
    pub rows: u32,
    pub stride: usize,
    pub row_bytes: usize,
}

/// GPU-ready planes of one decoded frame (kept alive by the decoder frame they point into).
pub struct PlanarImage {
    pub layout: PlaneLayout,
    /// Bits per sample (8 = one byte per sample; 9..=16 = two bytes per sample, little-endian).
    pub bits: u8,
    /// Two-byte samples hold the value in their high bits (`p010`), not their low bits (`yuv420p10le`).
    pub msb_aligned: bool,
    /// Chroma subsampling as log2 (`(1, 1)` = 4:2:0, `(1, 0)` = 4:2:2, `(0, 0)` = 4:4:4, `(2, 0)` = 4:1:1).
    pub chroma_shift: (u8, u8),
    /// Semi-planar chroma is interleaved V, U (`nv21`).
    pub swap_uv: bool,
    /// Decoded (luma) size before the sample aspect ratio and rotation.
    pub coded_width: u32,
    pub coded_height: u32,
    pub matrix: YuvMatrix,
    pub full_range: bool,
    pub transfer: Transfer,
    pub primaries: Primaries,
    /// HDR content peak in nits (what PQ is tone-mapped from).
    pub peak_nits: f32,
    /// Clockwise quarter turns to show the picture upright.
    pub quarter_turns: u8,
    /// The two fields of the frame were captured at different times (deinterlace when showing it).
    pub interlaced: bool,
    /// The top field (even rows, counting from 0) is the earlier one.
    pub top_field_first: bool,
    planes: [Option<(*const u8, PlaneInfo)>; 3],
    _owner: Box<dyn Send + Sync>,
}

// SAFETY: the plane pointers point into memory owned by `_owner` (a reference-counted decoder frame that nothing
// writes to while it is referenced) and are only read.
unsafe impl Send for PlanarImage {}
unsafe impl Sync for PlanarImage {}

/// What a [`PlanarImage`] describes, apart from its planes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanarFormat {
    pub layout: PlaneLayout,
    pub bits: u8,
    pub msb_aligned: bool,
    pub chroma_shift: (u8, u8),
    pub swap_uv: bool,
}

impl PlanarFormat {
    /// Number of planes (3, 2 or 1).
    pub fn plane_count(&self) -> usize {
        match self.layout {
            PlaneLayout::Planar => 3,
            PlaneLayout::SemiPlanar => 2,
            PlaneLayout::Gray | PlaneLayout::Packed => 1,
        }
    }

    /// Sample components per texel of plane `i` (packed RGBA: 4; interleaved chroma: 2).
    pub fn components(&self, i: usize) -> usize {
        match (self.layout, i) {
            (PlaneLayout::Packed, _) => 4,
            (PlaneLayout::SemiPlanar, 1) => 2,
            _ => 1,
        }
    }
}

/// How the fields of an interlaced frame are ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Interlace {
    pub top_field_first: bool,
}

impl PlanarImage {
    /// # Safety
    /// Every plane pointer must stay valid and unchanged for `(rows - 1) * stride + row_bytes` bytes as long as
    /// `owner` lives.
    pub unsafe fn new(
        format: PlanarFormat,
        coded: (u32, u32),
        color: ColorInfo,
        quarter_turns: u8,
        interlace: Option<Interlace>,
        planes: [Option<(*const u8, PlaneInfo)>; 3],
        owner: Box<dyn Send + Sync>,
    ) -> Self {
        Self {
            layout: format.layout,
            bits: format.bits,
            msb_aligned: format.msb_aligned,
            chroma_shift: format.chroma_shift,
            swap_uv: format.swap_uv,
            coded_width: coded.0,
            coded_height: coded.1,
            matrix: color.matrix,
            full_range: color.full_range,
            transfer: color.transfer,
            primaries: color.primaries,
            peak_nits: color.peak_nits,
            quarter_turns,
            interlaced: interlace.is_some(),
            top_field_first: interlace.is_none_or(|i| i.top_field_first),
            planes,
            _owner: owner,
        }
    }

    pub fn format(&self) -> PlanarFormat {
        PlanarFormat { layout: self.layout, bits: self.bits, msb_aligned: self.msb_aligned, chroma_shift: self.chroma_shift, swap_uv: self.swap_uv }
    }

    /// Bytes per sample component (1 or 2).
    pub fn sample_bytes(&self) -> usize {
        if self.bits > 8 { 2 } else { 1 }
    }

    /// Number of planes (3, 2 or 1).
    pub fn plane_count(&self) -> usize {
        self.format().plane_count()
    }

    /// Which field a deinterlacer shows for the first (`second_field` false) or second hand-over of this frame:
    /// true = the bottom field (odd rows).
    pub fn shown_field_is_bottom(&self, second_field: bool) -> bool {
        self.top_field_first == second_field
    }

    /// Plane `i`: its bytes (`(rows - 1) * stride + row_bytes`) and geometry.
    pub fn plane(&self, i: usize) -> Option<(&[u8], PlaneInfo)> {
        let (ptr, info) = (*self.planes.get(i)?)?;
        let len = (info.rows as usize).saturating_sub(1) * info.stride + info.row_bytes;
        // SAFETY: guaranteed by `new`'s contract.
        Some((unsafe { std::slice::from_raw_parts(ptr, len) }, info))
    }
}

/// A `Read + Seek` source (Windows `BufferedVideoStream`).
pub trait ReadSeek: Read + Seek + Send {}
impl<T: Read + Seek + Send> ReadSeek for T {}

/// What to play.
pub enum MediaSource {
    File(PathBuf),
    /// Custom AVIO over a reader; `path` is kept for diagnostics and format probing by extension.
    Reader { path: PathBuf, open: Box<dyn FnOnce() -> std::io::Result<Box<dyn ReadSeek>> + Send> },
}

/// Monotonic load counter value (`PlayerSessionGate::begin_load`).
pub type LoadGeneration = i32;

/// An audio stream of the loaded source ([`MediaPlayerBackend::tracks`]).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AudioTrack {
    /// The container's stream index (what [`MediaPlayerBackend::select_audio_track`] takes).
    pub stream_index: usize,
    /// The container's language tag as written (`eng`, `tur`, `en`, ...); `None` when missing or `und`.
    pub language: Option<String>,
    pub title: Option<String>,
    /// FFmpeg's codec name (`aac`, `ac3`, `opus`, ...).
    pub codec: String,
    pub channels: u16,
    /// FFmpeg's description of the channel layout (`stereo`, `5.1(side)`), when the stream has one.
    pub channel_layout: Option<String>,
    pub sample_rate: u32,
    /// The container marks it as the default track.
    pub default: bool,
}

/// How a subtitle stream is coded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SubtitleKind {
    /// Text events (subrip, ass/ssa, mov_text, webvtt, text): can be loaded into the subtitle list.
    Text,
    /// Pictures (hdmv_pgs_subtitle, dvd_subtitle, dvb_subtitle, xsub) and anything else that is not plain text.
    Image,
    /// A text codec this FFmpeg build has no decoder for (arib_caption, hdmv_text_subtitle, teletext without
    /// libzvbi): listed, cannot be read.
    Unsupported,
}

/// A subtitle stream of the loaded source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubtitleTrack {
    pub stream_index: usize,
    pub language: Option<String>,
    pub title: Option<String>,
    /// FFmpeg's codec name (`subrip`, `ass`, `mov_text`, `hdmv_pgs_subtitle`, ...).
    pub codec: String,
    pub kind: SubtitleKind,
    pub default: bool,
    pub forced: bool,
}

impl SubtitleTrack {
    pub fn is_text(&self) -> bool {
        self.kind == SubtitleKind::Text
    }
}

/// The audio and subtitle streams of the loaded source and the audio stream being played.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MediaTracks {
    pub audio: Vec<AudioTrack>,
    pub subtitles: Vec<SubtitleTrack>,
    /// Stream index of the audio track being decoded; `None` without audio.
    pub active_audio: Option<usize>,
}

impl MediaTracks {
    pub fn active_audio_track(&self) -> Option<&AudioTrack> {
        let i = self.active_audio?;
        self.audio.iter().find(|t| t.stream_index == i)
    }
}

/// Why an embedded subtitle track could not be read (`ffmpeg_backend::subtitles::extract_text_subtitles`). Pure, so
/// the view models can word it without the FFmpeg feature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtractError {
    Cancelled,
    /// The stream is no text subtitle track this build can decode.
    NotText,
    /// The track holds more events or text than the importer takes ([`ExtractLimits`]).
    TooLarge,
    /// The file, the stream or its decoder could not be opened, or reading failed: FFmpeg's (or the OS's) message,
    /// shown as details like a playback error's; may be empty.
    Failed(String),
}

impl std::fmt::Display for ExtractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExtractError::Cancelled => f.write_str("cancelled"),
            ExtractError::NotText => f.write_str("not a text subtitle track"),
            ExtractError::TooLarge => f.write_str("subtitle track too large"),
            ExtractError::Failed(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for ExtractError {}

/// Bounds of one embedded subtitle read: past either, the read stops with [`ExtractError::TooLarge`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtractLimits {
    pub max_events: usize,
    /// Sum of the event texts in bytes.
    pub max_text_bytes: usize,
}

impl Default for ExtractLimits {
    /// 100 000 events / 16 MiB of text: far beyond any real film or series episode (a two-hour film has ~2 000).
    fn default() -> Self {
        Self { max_events: 100_000, max_text_bytes: 16 << 20 }
    }
}

/// Events posted by a backend (gated by generation before they reach the UI).
#[derive(Debug, Clone, PartialEq)]
pub enum PlayerEvent {
    Opened { generation: LoadGeneration, duration: f64, natural_size: Option<(u32, u32)> },
    /// The source could not be opened, or stopped being readable while playing (then instead of `Ended`). Playback
    /// stops; `play()` does nothing until the next load.
    Failed { generation: LoadGeneration, message: String },
    SizeChanged { generation: LoadGeneration, width: u32, height: u32 },
    /// A new frame can be fetched with `latest_frame` (coalesced: at most one pending).
    FrameReady { generation: LoadGeneration },
    Ended { generation: LoadGeneration },
    /// Reports only real changes (alternating). An auto-play load reports `playing: true` right after `Opened`; a
    /// failure or the end reports `playing: false`.
    PlayingChanged { generation: LoadGeneration, playing: bool },
    /// [`MediaPlayerBackend::tracks`] changed: posted once after `Opened` and after every audio track switch (also
    /// one that failed, which leaves the previous track active).
    TracksChanged { generation: LoadGeneration },
    /// The audio stream asked for could not be played (its decoder did not open); the previous one keeps playing.
    AudioTrackFailed { generation: LoadGeneration, stream_index: usize, message: String },
}

/// Why the current load failed (`MediaPlayerBackend::failure_kind`). The macOS player model uses it for its
/// engine `preview` fallback decision (mac-viewmodels §7.1 `can_try_preview`: only unsupported formats are retried).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FailureKind {
    /// Unknown container, invalid data, no stream, or no decoder for a track: converting may help.
    UnsupportedFormat,
    /// The source could not be read (missing file, no permission, I/O or cache stream error).
    Io,
    Other,
}

/// The player contract (UI-thread owned; decoding, audio and teardown run on their own threads).
pub trait MediaPlayerBackend {
    /// `None` empties the player.
    fn load(&mut self, source: Option<MediaSource>, auto_play: bool) -> LoadGeneration;
    fn play(&mut self);
    fn pause(&mut self);
    fn seek(&mut self, seconds: f64);
    /// 0…1 (clamped; non-finite is ignored). Volume and mute belong to the player and survive loads.
    fn set_volume(&mut self, volume: f64);
    fn set_muted(&mut self, muted: bool);
    fn position(&self) -> f64;
    fn duration(&self) -> f64;
    fn is_playing(&self) -> bool;
    fn natural_size(&self) -> Option<(u32, u32)>;
    /// Windows buffer bar (from the buffered stream); 0 when unknown.
    fn buffered_ratio(&self) -> f64;
    /// The newest decoded frame not yet taken.
    fn latest_frame(&mut self) -> Option<VideoFrame>;
    /// `Send` sink; the app re-posts to the Slint event loop.
    fn set_event_sink(&mut self, sink: Box<dyn Fn(PlayerEvent) + Send + Sync>);
    /// Stops threads; teardown off the UI thread.
    fn dispose(&mut self);
    /// Why the current load failed (after `PlayerEvent::Failed`); `None` while it did not fail.
    fn failure_kind(&self) -> Option<FailureKind> {
        None
    }
    /// Hand frames over as YUV planes ([`VideoFrame::planar`]) instead of RGBA, for a UI that converts them on the
    /// GPU. Takes effect with the next decoded frame. Off by default; backends without planar output ignore it.
    fn set_planar_output(&mut self, _planar: bool) {}
    /// Decoder and presentation counters of the current source as one line (diagnostics); `None` when unknown.
    fn diagnostics(&self) -> Option<String> {
        None
    }
    /// The audio and subtitle streams of the current source (empty until it opened). No default body on purpose: a
    /// wrapper backend that forgot to forward it would silently hide the track menu.
    fn tracks(&self) -> MediaTracks;
    /// Plays the audio stream `stream_index` from the current position (while playing or paused), keeping the
    /// position; answered with `TracksChanged` (or `AudioTrackFailed` and `TracksChanged`). Choosing the stream that
    /// is already active (or asked for last) does nothing. Backends without track selection implement it as a no-op.
    fn select_audio_track(&mut self, stream_index: usize);
}

/// What the native stack reports (startup diagnostics and the build proof).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeStackInfo {
    pub ffmpeg_avformat_version: String,
    /// `avcodec_license()` of the libraries this process loaded: `LGPL version 3 or later` for the build the app
    /// ships (the BtbN LGPL build is configured with `--enable-version3`, so the LGPLv3 terms apply, not 2.1's);
    /// the engine keeps its own GPL build (`GPL version 3 or later`) in its own folder.
    pub ffmpeg_license: String,
    pub audio_host: String,
    pub audio_output_device: Option<String>,
}
