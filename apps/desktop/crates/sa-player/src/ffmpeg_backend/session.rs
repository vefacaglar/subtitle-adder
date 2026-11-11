//! One loaded source: the demux thread (opens the input, then reads packets), the video decode thread (hardware or
//! software decode, then either the planes for the GPU or an RGBA conversion), the presenter thread (shows frames when the master clock reaches them, drops late ones, detects
//! the end), the audio decode thread (decode + resample into the ring) and the audio output thread (cpal).
//!
//! Seeks bump `serial`; every queued packet, frame and audio chunk carries the serial it was produced for, and
//! consumers drop anything older, so a seek never shows or plays data of the previous position.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ffmpeg_next::codec;
use ffmpeg_next::ffi;
use ffmpeg_next::frame;
use ffmpeg_next::{Packet, Rational};
use parking_lot::{Condvar, Mutex, RwLock};

use super::audio::{self, AudioChunk, AudioRing, DeviceFormat, Resampler};
use super::hw::{self, Download, OwnedFrame};
use super::io::{self, OpenFailure, OpenedInput};
use super::priority;
use super::sync::{Clock, Notify};
use super::video::{self, Converter, HdrLevels, StreamLayout};
use crate::{FailureKind, LoadGeneration, MediaSource, PlayerEvent, PlayerEventToken, PlayerSessionGate, SourceId, VideoFrame};

pub(crate) type Sink = Arc<dyn Fn(PlayerEvent) + Send + Sync>;

/// State shared by every session of one player.
pub(crate) struct PlayerShared {
    /// f64 bits, 0..=1.
    pub volume: AtomicU64,
    pub muted: AtomicBool,
    pub sink: RwLock<Option<Sink>>,
    pub gate: PlayerSessionGate,
    /// Hand frames over as YUV planes (the UI converts them on the GPU) instead of RGBA.
    pub planar: AtomicBool,
    /// Decode in hardware when the stream allows it.
    pub hardware: AtomicBool,
    /// Tests: every hardware decoder fails as on a GPU without support for the codec (`hw` module docs).
    pub simulate_hw_failure: AtomicBool,
}

impl PlayerShared {
    pub fn new(hardware: bool) -> Self {
        Self {
            volume: AtomicU64::new(1.0f64.to_bits()),
            muted: AtomicBool::new(false),
            sink: RwLock::new(None),
            gate: PlayerSessionGate::new(),
            planar: AtomicBool::new(false),
            hardware: AtomicBool::new(hardware),
            simulate_hw_failure: AtomicBool::new(false),
        }
    }
}

/// Playback counters (diagnostics and the dropped-frame measurement).
#[derive(Debug, Default)]
pub(crate) struct Stats {
    pub decoded: AtomicU64,
    pub presented: AtomicU64,
    pub dropped_late: AtomicU64,
    pub dropped_decoder: AtomicU64,
    /// Frames that came out of the hardware decoder.
    pub hardware: AtomicU64,
    /// Frames handed over as YUV planes.
    pub planar: AtomicU64,
}

#[derive(Debug, Default, Clone)]
pub(crate) struct Info {
    pub opened: bool,
    pub failed: bool,
    pub duration: f64,
    pub natural: Option<(u32, u32)>,
    pub has_video: bool,
    pub has_audio_output: bool,
    pub failure: Option<FailureKind>,
}

const VIDEO_QUEUE_PACKETS: usize = 90;
const AUDIO_QUEUE_PACKETS: usize = 150;
const MAX_QUEUED_BYTES: usize = 48 * 1024 * 1024;
/// Decoded frames queued for the presenter (an interlaced frame is queued as two fields: twice the items).
const FRAME_QUEUE: usize = 4;
const AUDIO_RING_SECONDS: f64 = 0.3;
/// Waits up to this long before a due frame use a precise sleep instead of the condvar.
const PRECISE_WAIT: f64 = 0.02;
/// How long a seek in flight may hold back the seeks requested after it (`Control::request_seek`).
const MAX_SEEK_DEFERRAL: Duration = Duration::from_millis(1500);

struct PacketItem {
    serial: u64,
    /// `None` = end of stream.
    packet: Option<Packet>,
}

#[derive(Default)]
struct PqState {
    items: VecDeque<PacketItem>,
    bytes: usize,
}

/// A packet queue between the demuxer and one decoder.
#[derive(Default)]
pub(crate) struct PacketQueue {
    s: Mutex<PqState>,
    cv: Condvar,
}

impl PacketQueue {
    fn push(&self, item: PacketItem) {
        let mut s = self.s.lock();
        s.bytes += item.packet.as_ref().map_or(0, |p| p.size());
        s.items.push_back(item);
        self.cv.notify_one();
    }

    fn pop(&self, timeout: Duration) -> Option<PacketItem> {
        let mut s = self.s.lock();
        if s.items.is_empty() {
            self.cv.wait_for(&mut s, timeout);
        }
        let item = s.items.pop_front()?;
        s.bytes -= item.packet.as_ref().map_or(0, |p| p.size());
        Some(item)
    }

    fn flush(&self) {
        let mut s = self.s.lock();
        s.items.clear();
        s.bytes = 0;
        self.cv.notify_all();
    }

    fn wake(&self) {
        self.cv.notify_all();
    }

    fn levels(&self) -> (usize, usize) {
        let s = self.s.lock();
        (s.items.len(), s.bytes)
    }
}

pub(crate) struct DecodedFrame {
    serial: u64,
    pts: f64,
    duration: f64,
    frame: VideoFrame,
}

enum FrameItem {
    Frame(DecodedFrame),
    Eof { serial: u64 },
}

impl FrameItem {
    fn serial(&self) -> u64 {
        match self {
            FrameItem::Frame(f) => f.serial,
            FrameItem::Eof { serial } => *serial,
        }
    }
}

#[derive(Default)]
struct FrameQueue {
    items: Mutex<VecDeque<FrameItem>>,
    space: Condvar,
}

/// Everything the threads of one session share.
pub(crate) struct Control {
    pub player: Arc<PlayerShared>,
    pub generation: LoadGeneration,
    pub source_id: SourceId,
    token: PlayerEventToken,
    stop: Arc<AtomicBool>,
    pub serial: AtomicU64,
    pub playing: AtomicBool,
    /// The `playing` value the last `PlayingChanged` reported (events only report real changes).
    announced: Mutex<bool>,
    pub ended: AtomicBool,
    speed_bits: AtomicU64,
    seek_req: Mutex<Option<(f64, u64)>>,
    seek_target: Mutex<(u64, f64)>,
    force_show: AtomicBool,
    /// The newest seek requested while the previous one had no frame on screen yet (see `request_seek`).
    deferred_seek: Mutex<Option<f64>>,
    /// When the seek in flight was started.
    seek_started: Mutex<Instant>,
    pub clock: Clock,
    pub info: Mutex<Info>,
    start_time: Mutex<f64>,
    vq: PacketQueue,
    aq: PacketQueue,
    space: Notify,
    frames: FrameQueue,
    pub wake: Notify,
    pub audio_ring: AudioRing,
    pub audio_wake: Notify,
    /// No working output stream right now (none at all, or lost; the output thread may reopen one).
    pub audio_failed: AtomicBool,
    /// The current output stream reported a fatal error; the output thread opens a new one.
    pub audio_lost: AtomicBool,
    /// `DeviceFormat::pack` of the current output stream (0 before the first one).
    pub device_format: AtomicU64,
    pub audio_drained: AtomicBool,
    pub(crate) audio_active: AtomicBool,
    video_eof: AtomicBool,
    /// End of the last presented frame (media seconds).
    last_frame_end: Mutex<f64>,
    latest: Mutex<(Option<VideoFrame>, bool)>,
    pub stats: Stats,
}

impl Control {
    pub fn new(player: Arc<PlayerShared>, generation: LoadGeneration, source_id: SourceId, playing: bool, speed: f64) -> Arc<Self> {
        let token = player.gate.create_bound_token(generation, Some(source_id));
        let ctl = Arc::new(Self {
            player,
            generation,
            source_id,
            token,
            stop: Arc::new(AtomicBool::new(false)),
            serial: AtomicU64::new(1),
            playing: AtomicBool::new(playing),
            announced: Mutex::new(false),
            ended: AtomicBool::new(false),
            speed_bits: AtomicU64::new(speed.to_bits()),
            seek_req: Mutex::new(None),
            seek_target: Mutex::new((1, 0.0)),
            force_show: AtomicBool::new(true),
            deferred_seek: Mutex::new(None),
            seek_started: Mutex::new(Instant::now()),
            clock: Clock::new(0.0),
            info: Mutex::new(Info::default()),
            start_time: Mutex::new(0.0),
            vq: PacketQueue::default(),
            aq: PacketQueue::default(),
            space: Notify::default(),
            frames: FrameQueue::default(),
            wake: Notify::default(),
            audio_ring: AudioRing::default(),
            audio_wake: Notify::default(),
            audio_failed: AtomicBool::new(false),
            audio_lost: AtomicBool::new(false),
            device_format: AtomicU64::new(0),
            audio_drained: AtomicBool::new(false),
            audio_active: AtomicBool::new(false),
            video_eof: AtomicBool::new(false),
            last_frame_end: Mutex::new(0.0),
            latest: Mutex::new((None, false)),
            stats: Stats::default(),
        });
        ctl.clock.set_speed(speed);
        ctl.clock.set_playing(playing);
        ctl
    }

    pub fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    pub fn speed(&self) -> f64 {
        f64::from_bits(self.speed_bits.load(Ordering::Relaxed))
    }

    /// Stops every thread of the session (they exit on their own; the demux thread joins the others).
    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::SeqCst);
        self.wake_all();
    }

    fn wake_all(&self) {
        self.space.notify();
        self.wake.notify();
        self.audio_wake.notify();
        self.vq.wake();
        self.aq.wake();
        self.frames.space.notify_all();
        self.audio_ring.space.notify_all();
    }

    pub(crate) fn emit(&self, event: PlayerEvent) {
        if self.stopped() || !self.player.gate.should_accept_event(self.token, Some(self.source_id)) {
            return;
        }
        let sink = self.player.sink.read().clone();
        if let Some(sink) = sink {
            sink(event);
        }
    }

    /// Duration-clamped position (the newest requested target while a seek waits, see `request_seek`).
    pub fn position(&self) -> f64 {
        if let Some(t) = *self.deferred_seek.lock() {
            return t;
        }
        let p = self.clock.now();
        let d = self.info.lock().duration;
        let p = if p.is_finite() { p.max(0.0) } else { 0.0 };
        if d > 0.0 { p.min(d) } else { p }
    }

    pub fn set_playing(&self, playing: bool) {
        // A failed source cannot play (Media Foundation and AVPlayer ignore play on a failed item).
        if playing && self.info.lock().failed {
            return;
        }
        if playing && self.ended.load(Ordering::Acquire) {
            self.request_seek(0.0);
        }
        self.playing.store(playing, Ordering::Release);
        self.clock.set_playing(playing);
        self.wake_all();
        self.announce_playing();
    }

    /// Posts `PlayingChanged` when `playing` differs from what was last reported (read and posted under one lock, so
    /// the last report always matches the state). An auto-play load is reported as playing once the source opened
    /// (Media Foundation and AVPlayer start an auto-play item when it is ready); an explicit `play()` before that is
    /// reported at once (AVPlayer's "waiting to play").
    pub(crate) fn announce_playing(&self) {
        let mut announced = self.announced.lock();
        let playing = self.playing.load(Ordering::Acquire);
        if *announced != playing {
            *announced = playing;
            self.emit(PlayerEvent::PlayingChanged { generation: self.generation, playing });
        }
    }

    pub fn set_speed(&self, speed: f64) {
        let old = self.speed();
        if (old - speed).abs() < 1e-9 {
            return;
        }
        self.speed_bits.store(speed.to_bits(), Ordering::Relaxed);
        self.clock.set_speed(speed);
        if self.audio_active.load(Ordering::Acquire) && self.info.lock().opened {
            // Queued audio was resampled for the old speed: restart from the current position.
            let p = self.position();
            self.request_seek(p);
        }
    }

    /// Records a seek: everything queued becomes stale, the clock stands at the target until its frame is shown.
    pub fn request_seek(&self, seconds: f64) {
        let target = self.clamp_target(seconds);
        {
            // A seek whose frame is not on screen yet finishes first, and only the newest of the seeks requested
            // meanwhile runs after it (`run_deferred_seek`): a stream of seeks (dragging the slider, a held arrow
            // key) then shows a frame for every seek the decoder completes, instead of cancelling each one before
            // it shows anything (a frozen picture for the whole drag). A seek stuck for longer gives way.
            let mut deferred = self.deferred_seek.lock();
            let (has_video, opened) = {
                let i = self.info.lock();
                (i.has_video, i.opened)
            };
            if has_video
                && opened
                && self.force_show.load(Ordering::Acquire)
                && self.seek_started.lock().elapsed() < MAX_SEEK_DEFERRAL
            {
                *deferred = Some(target);
                return;
            }
            *deferred = None;
            *self.seek_started.lock() = Instant::now();
        }
        self.start_seek(target);
    }

    fn clamp_target(&self, seconds: f64) -> f64 {
        let duration = self.info.lock().duration;
        if !seconds.is_finite() {
            0.0
        } else if duration > 0.0 {
            seconds.clamp(0.0, duration)
        } else {
            seconds.max(0.0)
        }
    }

    /// Restarts decoding from the keyframe at or before `at` (the video decoder was replaced and starts without
    /// one), bypassing the seek deferral: a deferred seek would leave the new decoder with the packets after the
    /// keyframe the failed decoder consumed. A seek the user requested meanwhile (deferred) is the target instead.
    fn restart_decoding(&self, at: f64) {
        let target = {
            let mut deferred = self.deferred_seek.lock();
            let t = deferred.take().unwrap_or(at);
            *self.seek_started.lock() = Instant::now();
            t
        };
        self.start_seek(self.clamp_target(target));
    }

    /// Starts a seek to `target` (clamped): a new serial, flushed queues, the clock at the target, the demuxer asked.
    fn start_seek(&self, target: f64) {
        let serial = self.serial.fetch_add(1, Ordering::AcqRel) + 1;
        *self.seek_target.lock() = (serial, target);
        // Flush before the demuxer can see the request (it is published last): flushing afterwards could drop the
        // first packets it already queued for the new serial (the keyframe the target is decoded from). Packets of
        // the old serial queued after this flush are dropped by the decoders.
        self.frames.items.lock().clear();
        self.audio_ring.flush();
        self.vq.flush();
        self.aq.flush();
        self.ended.store(false, Ordering::Release);
        self.audio_drained.store(false, Ordering::Release);
        self.video_eof.store(false, Ordering::Release);
        let (has_video, opened) = {
            let i = self.info.lock();
            (i.has_video, i.opened)
        };
        let audio = self.audio_active.load(Ordering::Acquire) && !self.audio_failed.load(Ordering::Acquire);
        self.clock.set_audio_master(audio);
        // Before the source opened it is not known yet whether there is video: hold and show the first frame
        // (the demuxer releases both for audio-only sources once they open).
        let show = has_video || !opened;
        self.clock.jump(target, show);
        self.force_show.store(show, Ordering::Release);
        *self.last_frame_end.lock() = target;
        // Last: everything above is in place before the demuxer acts on the request.
        *self.seek_req.lock() = Some((target, serial));
        self.wake_all();
    }

    /// After the frame (or the end) of a seek is shown: runs the seek requested meanwhile, if any.
    fn run_deferred_seek(&self) {
        let next = self.deferred_seek.lock().take();
        if let Some(t) = next {
            self.request_seek(t);
        }
    }

    fn seek_target_for(&self, serial: u64) -> f64 {
        let (s, t) = *self.seek_target.lock();
        if s == serial { t } else { 0.0 }
    }

    fn start_time(&self) -> f64 {
        *self.start_time.lock()
    }

    pub fn take_latest(&self) -> Option<VideoFrame> {
        let mut l = self.latest.lock();
        l.1 = false;
        l.0.take()
    }

    fn publish(&self, f: DecodedFrame) {
        let size = (f.frame.width, f.frame.height);
        *self.last_frame_end.lock() = f.pts + f.duration;
        let changed = {
            let mut i = self.info.lock();
            if i.natural != Some(size) {
                i.natural = Some(size);
                true
            } else {
                false
            }
        };
        if changed {
            self.emit(PlayerEvent::SizeChanged { generation: self.generation, width: size.0, height: size.1 });
        }
        let announce = {
            let mut l = self.latest.lock();
            l.0 = Some(f.frame);
            !std::mem::replace(&mut l.1, true)
        };
        self.stats.presented.fetch_add(1, Ordering::Relaxed);
        if announce {
            self.emit(PlayerEvent::FrameReady { generation: self.generation });
        }
    }
}

/// Starts the session's demux thread, which opens `source` and spawns the rest.
pub(crate) fn spawn(ctl: Arc<Control>, source: MediaSource, audio_output: bool) -> std::io::Result<JoinHandle<()>> {
    std::thread::Builder::new().name("player-demux".into()).spawn(move || demux_main(ctl, source, audio_output))
}

/// The video stream's decoder and what it was opened from (the software fallback reopens it).
pub(crate) struct VideoTrack {
    dec: codec::decoder::Video,
    /// The decoder was set up for hardware decoding.
    hw: bool,
    /// It is a hardware-only alternative decoder (`hw::HwDecoder::hardware_only`).
    hw_only: bool,
    params: codec::Parameters,
    tb: Rational,
    layout: StreamLayout,
    frame_dur: f64,
    hdr: HdrLevels,
}

struct Streams {
    video: Option<(usize, VideoTrack)>,
    audio: Option<(usize, codec::decoder::Audio, Rational)>,
}

/// Frame threads for software decoding (FFmpeg's automatic count is cores + 1; more than 8 only costs memory).
fn software_threads() -> usize {
    std::thread::available_parallelism().map_or(0, |n| (n.get() + 1).min(8))
}

/// Opens the video decoder: in hardware when `hardware` and the stream suit it (single-threaded; the GPU does the
/// work), else in software with frame threads. Returns the decoder, whether it was set up for hardware and whether it
/// is a hardware-only alternative decoder.
///
/// The hardware decoder is the codec's default decoder when that has a hardware config for the platform, else another
/// (native) decoder of the same codec that has one: FFmpeg's default AV1 decoder is `libdav1d`, which has none, so AV1
/// decodes through the native `av1` decoder (hardware only). The software decoder is always the default one, so a
/// fall-back from hardware never lands on a decoder that cannot decode without the GPU.
fn open_video_decoder(params: &codec::Parameters, tb: Rational, hardware: bool) -> Result<(codec::decoder::Video, bool, bool), ffmpeg_next::Error> {
    if hardware && let Some(choice) = unsafe { hw::decoder_for(params.as_ptr()) } {
        let mut cctx = codec::context::Context::from_parameters(params.clone())?;
        let attached = unsafe {
            (*cctx.as_mut_ptr()).pkt_timebase = tb.into();
            hw::attach(cctx.as_mut_ptr())
        };
        if attached {
            cctx.set_threading(codec::threading::Config { kind: codec::threading::Type::Frame, count: 1 });
            let codec = unsafe { ffmpeg_next::Codec::wrap(choice.codec) };
            match cctx.decoder().open_as(codec).and_then(|o| o.video()) {
                Ok(dec) => return Ok((dec, true, choice.hardware_only)),
                Err(e) => eprintln!("[player] hardware decoder did not open ({e}); decoding in software"),
            }
        }
    }
    let mut cctx = codec::context::Context::from_parameters(params.clone())?;
    unsafe { (*cctx.as_mut_ptr()).pkt_timebase = tb.into() };
    cctx.set_threading(codec::threading::Config { kind: codec::threading::Type::Frame, count: software_threads() });
    Ok((cctx.decoder().video()?, false, false))
}

fn open_streams(input: &mut OpenedInput, hardware: bool) -> Result<Streams, OpenFailure> {
    let ictx = input.input();
    let vbest = ictx.streams().best(ffmpeg_next::media::Type::Video).map(|s| s.index());
    // Skip cover art ("attached pictures"): it is a single still, not a video track.
    let video_index = vbest.filter(|&i| {
        let s = ictx.stream(i).expect("index from best()");
        unsafe { (*s.as_ptr()).disposition & ffi::AV_DISPOSITION_ATTACHED_PIC == 0 }
    });
    let audio_index = ictx.streams().best(ffmpeg_next::media::Type::Audio).map(|s| s.index());
    if video_index.is_none() && audio_index.is_none() {
        return Err(OpenFailure::new("no audio or video stream", FailureKind::UnsupportedFormat));
    }
    let mut out = Streams { video: None, audio: None };
    if let Some(i) = video_index {
        let s = ictx.stream(i).expect("video stream");
        let tb = s.time_base();
        let fps = {
            let r = s.avg_frame_rate();
            let r = if r.numerator() > 0 && r.denominator() > 0 { r } else { s.rate() };
            if r.numerator() > 0 && r.denominator() > 0 { r.denominator() as f64 / r.numerator() as f64 } else { 1.0 / 25.0 }
        };
        let rotation = unsafe { video::stream_rotation(s.as_ptr()) };
        let unsupported = |e: ffmpeg_next::Error| OpenFailure::new(e.to_string(), io::classify(e));
        let params = s.parameters().clone();
        // A video track without a decoder fails the load (the macOS player then tries the engine preview), as
        // Media Foundation and AVFoundation do.
        let (dec, hw, hw_only) = open_video_decoder(&params, tb, hardware).map_err(|e| match e {
            ffmpeg_next::Error::Other { .. } | ffmpeg_next::Error::Unknown => {
                OpenFailure::new(ffmpeg_next::Error::DecoderNotFound.to_string(), FailureKind::UnsupportedFormat)
            }
            e => unsupported(e),
        })?;
        let layout = unsafe {
            let st = s.as_ptr();
            let c = (*st).sample_aspect_ratio;
            let p = (*(*st).codecpar).sample_aspect_ratio;
            StreamLayout { rotation, container_sar: (c.num, c.den), codec_sar: (p.num, p.den) }
        };
        let hdr = unsafe { HdrLevels::of_stream(s.as_ptr()) };
        out.video = Some((i, VideoTrack { dec, hw, hw_only, params, tb, layout, frame_dur: fps.clamp(1.0 / 240.0, 1.0), hdr }));
    }
    if let Some(i) = audio_index {
        let s = ictx.stream(i).expect("audio stream");
        let tb = s.time_base();
        let opened = codec::context::Context::from_parameters(s.parameters()).and_then(|mut c| {
            unsafe { (*c.as_mut_ptr()).pkt_timebase = tb.into() };
            c.decoder().audio()
        });
        match opened {
            Ok(dec) => out.audio = Some((i, dec, tb)),
            Err(e) => eprintln!("[player] audio decoder unavailable: {e}"),
        }
    }
    Ok(out)
}

fn demux_main(ctl: Arc<Control>, source: MediaSource, audio_output: bool) {
    let _ = ffmpeg_next::init();
    let mut input = match io::open_input(source, ctl.stop.clone()) {
        Ok(i) => i,
        Err(failure) => {
            fail(&ctl, failure);
            return;
        }
    };
    if ctl.stopped() {
        return;
    }
    let streams = match open_streams(&mut input, ctl.player.hardware.load(Ordering::Relaxed)) {
        Ok(s) => s,
        Err(failure) => {
            fail(&ctl, failure);
            return;
        }
    };
    let (start_time, duration) = unsafe {
        let p = input.input().as_ptr();
        let st = if (*p).start_time == ffi::AV_NOPTS_VALUE { 0.0 } else { (*p).start_time as f64 / ffi::AV_TIME_BASE as f64 };
        let mut d = if (*p).duration > 0 && (*p).duration != ffi::AV_NOPTS_VALUE { (*p).duration as f64 / ffi::AV_TIME_BASE as f64 } else { 0.0 };
        if d <= 0.0 {
            for s in input.input().streams() {
                let sd = s.duration();
                if sd > 0 && sd != ffi::AV_NOPTS_VALUE {
                    d = d.max(sd as f64 * f64::from(s.time_base()));
                }
            }
        }
        (st, d)
    };
    *ctl.start_time.lock() = start_time;

    // Audio output first: the clock mode depends on it.
    let mut handles: Vec<JoinHandle<()>> = Vec::new();
    let mut device: Option<DeviceFormat> = None;
    let mut audio_decoder = None;
    if let Some((index, dec, tb)) = streams.audio {
        if audio_output {
            let (tx, rx) = crossbeam_channel::bounded(1);
            let c = ctl.clone();
            match std::thread::Builder::new().name("player-audio-out".into()).spawn(move || audio::output_thread(c, tx)) {
                Ok(h) => {
                    match rx.recv_timeout(Duration::from_secs(3)) {
                        Ok(Ok(fmt)) => {
                            device = Some(fmt);
                            audio_decoder = Some((index, dec, tb));
                        }
                        Ok(Err(e)) => eprintln!("[player] no audio output: {e}"),
                        Err(_) => eprintln!("[player] audio output did not start"),
                    }
                    handles.push(h);
                }
                Err(e) => eprintln!("[player] audio thread: {e}"),
            }
        }
    }
    let audio_index = audio_decoder.as_ref().map(|a| a.0);
    let has_video = streams.video.is_some();
    ctl.audio_active.store(device.is_some(), Ordering::Release);
    {
        let mut i = ctl.info.lock();
        i.opened = true;
        i.duration = duration;
        i.has_video = has_video;
        i.has_audio_output = device.is_some();
        i.natural = streams.video.as_ref().map(|(_, v)| v.layout.geometry(v.dec.width(), v.dec.height(), None).display_size());
    }
    // A seek requested before the source opened is clamped now.
    let pending = *ctl.seek_req.lock();
    if let Some((t, _)) = pending {
        if duration > 0.0 && t > duration {
            ctl.request_seek(duration);
        }
    }
    ctl.clock.set_ready(device.is_some(), has_video);
    if !has_video {
        ctl.force_show.store(false, Ordering::Release);
        ctl.clock.release();
    }
    let video_index = streams.video.as_ref().map(|v| v.0);
    if let Some((_, track)) = streams.video {
        let c = ctl.clone();
        if let Ok(h) = std::thread::Builder::new().name("player-video".into()).spawn(move || video_main(c, track)) {
            handles.push(h);
        }
    }
    if let (Some((_, dec, tb)), Some(_)) = (audio_decoder, device) {
        let c = ctl.clone();
        if let Ok(h) = std::thread::Builder::new().name("player-audio".into()).spawn(move || audio_main(c, dec, tb)) {
            handles.push(h);
        }
    }
    {
        let c = ctl.clone();
        if let Ok(h) = std::thread::Builder::new().name("player-present".into()).spawn(move || present_main(c)) {
            handles.push(h);
        }
    }

    let natural = ctl.info.lock().natural;
    ctl.player.gate.notify_source_open_completed(ctl.token, true, None);
    if ctl.player.gate.should_accept_media_opened(ctl.token, Some(ctl.source_id), true) {
        ctl.emit(PlayerEvent::Opened { generation: ctl.generation, duration, natural_size: natural });
        ctl.announce_playing();
    }

    demux_loop(&ctl, &mut input, video_index, audio_index);

    ctl.shutdown();
    for h in handles {
        let _ = h.join();
    }
    drop(input);
}

/// Reports a failed open. The message is trimmed and may be empty (the view models substitute their localized
/// "unknown error" text, as both reference apps do).
fn fail(ctl: &Control, failure: OpenFailure) {
    if ctl.stopped() {
        return;
    }
    {
        let mut i = ctl.info.lock();
        i.failed = true;
        i.failure = Some(failure.kind);
    }
    let message = failure.message.trim().to_owned();
    ctl.player.gate.notify_source_open_completed(ctl.token, false, Some(message.clone()));
    stop_after_failure(ctl);
    if ctl.player.gate.should_accept_media_failed(ctl.token, Some(ctl.source_id), true) {
        ctl.emit(PlayerEvent::Failed { generation: ctl.generation, message });
    }
}

/// A failure stops playback (an auto-play load that fails reports `playing: false`, as the platform players do).
fn stop_after_failure(ctl: &Control) {
    if ctl.playing.swap(false, Ordering::AcqRel) {
        ctl.clock.set_playing(false);
        ctl.wake_all();
    }
    ctl.announce_playing();
}

/// The source stopped being readable while playing (file removed, drive unplugged, cache stream error): reported
/// like Media Foundation's MediaFailed / AVPlayer's failed-to-play-to-end. The last frame stays on screen.
fn fail_playback(ctl: &Control, failure: OpenFailure) {
    if ctl.stopped() {
        return;
    }
    {
        let mut i = ctl.info.lock();
        if i.failed {
            return;
        }
        i.failed = true;
        i.failure = Some(failure.kind);
    }
    let message = failure.message.trim().to_owned();
    ctl.player.gate.notify_source_state_changed(ctl.token, false, true);
    stop_after_failure(ctl);
    if ctl.player.gate.should_accept_media_failed(ctl.token, Some(ctl.source_id), true) {
        ctl.emit(PlayerEvent::Failed { generation: ctl.generation, message });
    }
}

/// The I/O error the demuxer's byte stream recorded, if any (`AVIOContext::error`; demuxers often report a failed
/// read as plain end of file, which is why ffplay checks it too).
fn io_error(input: &mut OpenedInput) -> Option<std::ffi::c_int> {
    unsafe {
        let pb = (*input.input().as_ptr()).pb;
        if pb.is_null() {
            return None;
        }
        let e = (*pb).error;
        (e < 0 && e != ffi::AVERROR_EOF && e != ffi::AVERROR_EXIT).then_some(e)
    }
}

fn demux_loop(ctl: &Control, input: &mut OpenedInput, video: Option<usize>, audio: Option<usize>) {
    let mut serial = 1u64;
    let mut eof = false;
    let start = ctl.start_time();
    loop {
        if ctl.stopped() {
            return;
        }
        let req = ctl.seek_req.lock().take();
        if let Some((target, s)) = req {
            let ts = ((target + start) * ffi::AV_TIME_BASE as f64) as i64;
            let ictx = input.input();
            if ictx.seek(ts, ..ts).is_err() && ictx.seek(ts, ..).is_err() {
                eprintln!("[player] seek to {target:.3}s failed");
            }
            serial = s;
            eof = false;
            ctl.vq.flush();
            ctl.aq.flush();
        }
        if eof {
            ctl.space.wait(Duration::from_millis(20));
            continue;
        }
        let (vlen, vbytes) = ctl.vq.levels();
        let (alen, abytes) = ctl.aq.levels();
        let video_enough = video.is_none() || vlen >= VIDEO_QUEUE_PACKETS;
        let audio_enough = audio.is_none() || alen >= AUDIO_QUEUE_PACKETS;
        if vbytes + abytes > MAX_QUEUED_BYTES || (video_enough && audio_enough) {
            ctl.space.wait(Duration::from_millis(10));
            continue;
        }
        let mut packet = Packet::empty();
        match packet.read(input.input()) {
            Ok(()) => {
                let idx = packet.stream();
                if Some(idx) == video {
                    ctl.vq.push(PacketItem { serial, packet: Some(packet) });
                } else if Some(idx) == audio {
                    ctl.aq.push(PacketItem { serial, packet: Some(packet) });
                }
            }
            Err(ffmpeg_next::Error::Exit) => return,
            Err(ffmpeg_next::Error::Other { errno }) if errno == ffi::EAGAIN => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(_) if ctl.stopped() => return,
            Err(e) => {
                if let Some(code) = io_error(input) {
                    eprintln!("[player] read error: {}", io::error_text(code));
                    fail_playback(ctl, OpenFailure::new(io::error_text(code), FailureKind::Io));
                } else if !matches!(e, ffmpeg_next::Error::Eof) {
                    // A demuxer error with intact I/O (damaged tail of a file): end there, as the platform players do.
                    eprintln!("[player] read error: {e}");
                }
                eof = true;
                ctl.vq.push(PacketItem { serial, packet: None });
                ctl.aq.push(PacketItem { serial, packet: None });
            }
        }
    }
}

fn seconds(ts: i64, tb: Rational, start: f64) -> f64 {
    ts as f64 * f64::from(tb) - start
}

fn video_main(ctl: Arc<Control>, track: VideoTrack) {
    let _priority = priority::raise(priority::Role::VideoDecode);
    let VideoTrack { mut dec, hw: mut hw_decoder, hw_only, params, tb, layout, frame_dur, hdr } = track;
    let codec_id = unsafe { (*params.as_ptr()).codec_id };
    let simulate_failure = ctl.player.simulate_hw_failure.load(Ordering::Relaxed) || hw::simulate_failure_from_env();
    let mut out = FrameOutput { conv: Converter::new(), download: Download::default(), hdr, fields: FieldPacer::default() };
    let start = ctl.start_time();
    let mut serial = 0u64;
    let mut target = 0.0;
    let mut emitted = false;
    // The newest frame decoded before the seek target (shown when the target lies past the last frame). Swapped
    // with the receive frame instead of copied: `receive_frame` unrefs its target first, so this is zero-copy.
    let mut last_skipped = frame::Video::empty();
    let mut has_skipped = false;
    let mut next_pts = 0.0;
    let mut f = frame::Video::empty();
    let mut hw_health = HwHealth::new(hw_only, frame_dur);
    loop {
        if ctl.stopped() {
            return;
        }
        let Some(item) = ctl.vq.pop(Duration::from_millis(20)) else { continue };
        ctl.space.notify();
        let current = ctl.serial.load(Ordering::Acquire);
        if item.serial != current {
            continue;
        }
        if item.serial != serial {
            dec.flush();
            serial = item.serial;
            target = ctl.seek_target_for(serial);
            emitted = false;
            has_skipped = false;
            next_pts = target;
            out.fields.reset();
            hw_health.flushed();
        }
        let at_eof = item.packet.is_none();
        let sent = match &item.packet {
            // Simulated failure: the packet is consumed and fails, as on a GPU that cannot decode the codec.
            Some(_) if hw_decoder && simulate_failure => Err(ffmpeg_next::Error::Other { errno: ffi::ENOSYS }),
            Some(p) => dec.send_packet(p),
            None => dec.send_eof(),
        };
        if hw_decoder && !at_eof {
            hw_health.packet();
        }
        let mut fall_back = false;
        if let Err(e) = sent {
            if !matches!(e, ffmpeg_next::Error::Eof) && !at_eof {
                if hw_decoder && (hw_health.failed(&e) || hw_health.stalled()) {
                    fall_back = true;
                } else {
                    // Damaged packet: keep going with the next one.
                    continue;
                }
            }
        }
        while !fall_back {
            match dec.receive_frame(&mut f) {
                Ok(()) => {
                    ctl.stats.decoded.fetch_add(1, Ordering::Relaxed);
                    let raw = unsafe { f.as_ptr() };
                    if hw_decoder {
                        if unsafe { hw::is_hw_format((*raw).format) } {
                            ctl.stats.hardware.fetch_add(1, Ordering::Relaxed);
                            hw_health.frame();
                        } else {
                            // The decoder could not use the GPU for this stream after all (it is single-threaded
                            // then): reopen it in software with frame threads.
                            fall_back = true;
                            break;
                        }
                    }
                    let ts = unsafe { (*raw).best_effort_timestamp };
                    let pts = if ts != ffi::AV_NOPTS_VALUE { seconds(ts, tb, start) } else { next_pts };
                    let dur = unsafe {
                        let d = (*raw).duration;
                        if d > 0 { (d as f64 * f64::from(tb)).clamp(1.0 / 240.0, 1.0) } else { frame_dur }
                    };
                    next_pts = pts + dur;
                    if pts + dur * 0.5 < target {
                        std::mem::swap(&mut f, &mut last_skipped);
                        has_skipped = true;
                        continue;
                    }
                    if emitted && ctl.clock.is_running() && pts + dur < ctl.clock.now() - 0.02 {
                        // Hopelessly late: don't spend a download or conversion on it. The next frame's fields are
                        // paced from its own duration (the gap to the frame before spans the dropped one).
                        ctl.stats.dropped_decoder.fetch_add(1, Ordering::Relaxed);
                        out.fields.reset();
                        continue;
                    }
                    match push_frame(&ctl, &mut out, raw, layout, serial, pts, dur) {
                        Pushed::Queued => {
                            emitted = true;
                            has_skipped = false;
                        }
                        Pushed::Skipped => {
                            if ctl.stopped() {
                                return;
                            }
                            if ctl.serial.load(Ordering::Acquire) != serial {
                                break;
                            }
                        }
                        Pushed::DownloadFailed => {
                            fall_back = true;
                            break;
                        }
                    }
                }
                Err(ffmpeg_next::Error::Eof) => {
                    if !emitted {
                        // Seek past the last frame: show the last one there is.
                        if std::mem::take(&mut has_skipped) {
                            let raw = unsafe { last_skipped.as_ptr() };
                            let ts = unsafe { (*raw).best_effort_timestamp };
                            let pts = if ts != ffi::AV_NOPTS_VALUE { seconds(ts, tb, start) } else { target };
                            if push_frame(&ctl, &mut out, raw, layout, serial, pts, frame_dur) == Pushed::Queued {
                                emitted = true;
                            }
                        }
                    }
                    ctl.frames.items.lock().push_back(FrameItem::Eof { serial });
                    ctl.wake.notify();
                    break;
                }
                Err(e) => {
                    // EAGAIN (needs input) or a decode error. A hardware decoder that keeps failing (or fails before
                    // its first frame: the GPU cannot decode this profile, and a hardware-only decoder such as AV1's
                    // native one cannot decode in software at all) is replaced by the software decoder.
                    if hw_decoder && hw_health.failed(&e) {
                        fall_back = true;
                    }
                    break;
                }
            }
        }
        if hw_decoder && !fall_back && hw_health.stalled() {
            eprintln!("[player] hardware decoder produced no frame from {} packets", hw_health.packets_without_frame);
            fall_back = true;
        }
        if fall_back {
            eprintln!("[player] hardware decoding failed for this stream; decoding in software");
            if hw_only && !simulate_failure {
                // This GPU cannot decode the codec: later loads go straight to the software decoder.
                hw::remember_hardware_only_failure(codec_id);
            }
            let Ok((sw, _, _)) = open_video_decoder(&params, tb, false) else {
                eprintln!("[player] software video decoder unavailable");
                return;
            };
            dec = sw;
            hw_decoder = false;
            hw_health = HwHealth::new(false, frame_dur);
            f = frame::Video::empty();
            last_skipped = frame::Video::empty();
            has_skipped = false;
            out.fields.reset();
            // Decode again from the keyframe at or before where playback stands: the new decoder starts without one,
            // and the packets up to here went to the failed decoder.
            let at = if emitted { ctl.clock.now() } else { target };
            ctl.restart_decoding(at);
        }
    }
}

/// Health of a hardware decoder. It is given up after `HW_ERRORS_IN_A_ROW` errors without a frame in between (a
/// damaged packet, such as the first ones of a TS capture that starts in the middle of a GOP, does not end hardware
/// decoding), at its first error before any frame when it is a hardware-only alternative decoder (it cannot decode
/// in software at all: the GPU cannot decode the codec), and when it takes `stall_packets` packets since the last
/// flush or frame without handing over a frame (a decoder that only ever asks for more input).
#[derive(Debug)]
struct HwHealth {
    hardware_only: bool,
    frames: u64,
    errors_in_a_row: u32,
    packets_without_frame: u32,
    stall_packets: u32,
}

const HW_ERRORS_IN_A_ROW: u32 = 3;
/// A hardware decoder stalls after this many packets without a frame, or 2 s of stream if that is more.
const HW_STALL_PACKETS: u32 = 60;

impl HwHealth {
    fn new(hardware_only: bool, frame_dur: f64) -> Self {
        let two_seconds = if frame_dur > 0.0 { (2.0 / frame_dur).ceil() as u32 } else { 0 };
        Self { hardware_only, frames: 0, errors_in_a_row: 0, packets_without_frame: 0, stall_packets: HW_STALL_PACKETS.max(two_seconds) }
    }

    fn frame(&mut self) {
        self.frames += 1;
        self.errors_in_a_row = 0;
        self.packets_without_frame = 0;
    }

    /// A packet was sent to the decoder.
    fn packet(&mut self) {
        self.packets_without_frame = self.packets_without_frame.saturating_add(1);
    }

    /// The decoder was flushed (a seek): its stall count starts again.
    fn flushed(&mut self) {
        self.packets_without_frame = 0;
    }

    fn stalled(&self) -> bool {
        self.packets_without_frame >= self.stall_packets
    }

    /// Records `e` (ignoring "needs more input" / end of stream); true when hardware decoding should be given up.
    fn failed(&mut self, e: &ffmpeg_next::Error) -> bool {
        match e {
            ffmpeg_next::Error::Eof => false,
            ffmpeg_next::Error::Other { errno } if *errno == ffi::EAGAIN => false,
            _ => {
                self.errors_in_a_row += 1;
                (self.hardware_only && self.frames == 0) || self.errors_in_a_row >= HW_ERRORS_IN_A_ROW
            }
        }
    }
}

/// Paces the second field of interlaced frames: half the distance to the previous interlaced frame (what the stream
/// really runs at), or half the frame's duration when there is no previous frame to measure against.
#[derive(Debug, Default)]
struct FieldPacer {
    last_pts: Option<f64>,
}

impl FieldPacer {
    /// The field period of an interlaced frame at `pts` lasting `dur`.
    fn half(&mut self, pts: f64, dur: f64) -> f64 {
        let gap = self.last_pts.map(|p| pts - p).filter(|g| *g > 1.0 / 240.0 && *g < 0.25 && *g < dur * 2.5);
        self.last_pts = Some(pts);
        gap.unwrap_or(dur) * 0.5
    }

    /// The next frame does not follow the last one measured (a seek, a dropped frame, a progressive frame, a new
    /// decoder).
    fn reset(&mut self) {
        self.last_pts = None;
    }
}

/// Per-thread conversion state of the video decode thread.
struct FrameOutput {
    conv: Converter,
    download: Download,
    /// The stream's HDR light levels (from its side data, updated by frames that carry them).
    hdr: HdrLevels,
    fields: FieldPacer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pushed {
    Queued,
    /// Not queued: stop, seek, or a conversion failure.
    Skipped,
    /// A hardware frame could not be downloaded (the decoder falls back to software).
    DownloadFailed,
}

fn empty_rgba() -> Arc<Vec<u8>> {
    static EMPTY: std::sync::OnceLock<Arc<Vec<u8>>> = std::sync::OnceLock::new();
    EMPTY.get_or_init(|| Arc::new(Vec::new())).clone()
}

/// Downloads (hardware frames), describes the planes (planar output) or converts to RGBA, and queues a frame; waits
/// while the queue is full.
fn push_frame(ctl: &Control, out: &mut FrameOutput, raw: *const ffi::AVFrame, layout: StreamLayout, serial: u64, pts: f64, dur: f64) -> Pushed {
    let geo = unsafe {
        let r = (*raw).sample_aspect_ratio;
        layout.geometry((*raw).width.max(1) as u32, (*raw).height.max(1) as u32, Some((r.num, r.den)))
    };
    let mut downloaded: Option<Arc<OwnedFrame>> = None;
    let mut src = raw;
    if unsafe { hw::is_hw_format((*raw).format) } {
        let Some(d) = (unsafe { out.download.download(raw) }) else { return Pushed::DownloadFailed };
        src = d.0;
        downloaded = Some(d);
    }
    unsafe { out.hdr.update(raw) };
    let mut frame = None;
    if ctl.player.planar.load(Ordering::Relaxed) {
        let img = if video::planar_format(unsafe { (*src).format }).is_some() {
            let owner: Option<Box<dyn Send + Sync>> = match downloaded {
                Some(d) => Some(Box::new(d)),
                None => unsafe { OwnedFrame::reference(raw) }.map(|r| Box::new(r) as Box<dyn Send + Sync>),
            };
            owner.and_then(|owner| unsafe { video::planar_image(owner, src, raw, geo, &out.hdr) })
        } else {
            // Any other pixel format: swscale's RGBA at the coded size, scaled and rotated on the GPU.
            unsafe { out.conv.convert_packed(src) }.and_then(|(w, h, rgba)| unsafe { video::packed_image(rgba, (w, h), raw, geo, &out.hdr) })
        };
        if let Some(img) = img {
            let (w, h) = geo.display_size();
            ctl.stats.planar.fetch_add(1, Ordering::Relaxed);
            frame = Some(VideoFrame { width: w, height: h, stride: 0, pts, rgba: empty_rgba(), planar: Some(Arc::new(img)), second_field: false });
        }
    }
    let frame = match frame {
        Some(f) => f,
        None => {
            let Some((w, h, rgba)) = (unsafe { out.conv.convert(src, geo) }) else { return Pushed::Skipped };
            VideoFrame::from_rgba(w, h, w * 4, pts, rgba)
        }
    };
    // An interlaced frame is shown field by field (the UI deinterlaces): its second field half a frame later
    // (`FieldPacer`). The queue holds as many frames either way: twice the items for fields.
    let interlaced = frame.planar.as_ref().is_some_and(|p| p.interlaced);
    let capacity = if interlaced { FRAME_QUEUE * 2 } else { FRAME_QUEUE };
    let mut items = Vec::with_capacity(2);
    if interlaced {
        let half = out.fields.half(pts, dur);
        let mut second = frame.clone();
        second.second_field = true;
        second.pts = pts + half;
        items.push(DecodedFrame { serial, pts, duration: half, frame });
        items.push(DecodedFrame { serial, pts: pts + half, duration: (dur - half).max(half), frame: second });
    } else {
        out.fields.reset();
        items.push(DecodedFrame { serial, pts, duration: dur, frame });
    }
    for item in items {
        let mut q = ctl.frames.items.lock();
        loop {
            if ctl.stopped() || ctl.serial.load(Ordering::Acquire) != serial {
                return Pushed::Skipped;
            }
            if q.len() < capacity {
                break;
            }
            ctl.frames.space.wait_for(&mut q, Duration::from_millis(20));
        }
        q.push_back(FrameItem::Frame(item));
        drop(q);
        ctl.wake.notify();
    }
    Pushed::Queued
}

fn present_main(ctl: Arc<Control>) {
    let _priority = priority::raise(priority::Role::Presenter);
    loop {
        if ctl.stopped() {
            return;
        }
        let serial = ctl.serial.load(Ordering::Acquire);
        let mut q = ctl.frames.items.lock();
        while q.front().is_some_and(|i| i.serial() != serial) {
            q.pop_front();
            ctl.frames.space.notify_all();
        }
        if ctl.force_show.load(Ordering::Acquire) {
            match q.front() {
                Some(FrameItem::Frame(_)) => {
                    let Some(FrameItem::Frame(f)) = q.pop_front() else { unreachable!() };
                    ctl.frames.space.notify_all();
                    drop(q);
                    if ctl.serial.load(Ordering::Acquire) == serial && ctl.force_show.swap(false, Ordering::AcqRel) {
                        ctl.publish(f);
                        ctl.clock.release();
                        ctl.run_deferred_seek();
                    }
                }
                Some(FrameItem::Eof { .. }) => {
                    q.pop_front();
                    drop(q);
                    if ctl.serial.load(Ordering::Acquire) == serial && ctl.force_show.swap(false, Ordering::AcqRel) {
                        ctl.video_eof.store(true, Ordering::Release);
                        ctl.clock.release();
                        ctl.run_deferred_seek();
                    }
                }
                None => {
                    drop(q);
                    ctl.wake.wait(Duration::from_millis(10));
                }
            }
            continue;
        }
        match q.front() {
            None => {
                drop(q);
                check_end(&ctl);
                ctl.wake.wait(Duration::from_millis(10));
            }
            Some(FrameItem::Eof { .. }) => {
                q.pop_front();
                drop(q);
                ctl.video_eof.store(true, Ordering::Release);
            }
            Some(FrameItem::Frame(f)) => {
                if !ctl.clock.is_running() {
                    drop(q);
                    ctl.wake.wait(Duration::from_millis(10));
                    continue;
                }
                let now = ctl.clock.now();
                let delay = f.pts - now;
                if delay > 0.001 {
                    drop(q);
                    let wait = delay / ctl.speed().max(0.1);
                    if wait <= PRECISE_WAIT {
                        // Condvar timeouts follow the coarse system tick (15.6 ms on Windows), which would show
                        // 60 fps frames a frame late or drop them; `thread::sleep` uses a high-resolution timer.
                        std::thread::sleep(Duration::from_secs_f64(wait));
                    } else {
                        ctl.wake.wait(Duration::from_secs_f64((wait - PRECISE_WAIT).min(0.01)));
                    }
                    continue;
                }
                // Due. If the next frame is due as well this one is late: skip it.
                let next_due = matches!(q.get(1), Some(FrameItem::Frame(n)) if n.pts <= now);
                let Some(FrameItem::Frame(f)) = q.pop_front() else { unreachable!() };
                ctl.frames.space.notify_all();
                drop(q);
                if next_due {
                    ctl.stats.dropped_late.fetch_add(1, Ordering::Relaxed);
                } else if ctl.serial.load(Ordering::Acquire) == serial {
                    ctl.publish(f);
                }
            }
        }
    }
}

/// Ends playback when every stream ran out and the clock passed the last frame.
fn check_end(ctl: &Control) {
    if !ctl.playing.load(Ordering::Acquire) || ctl.ended.load(Ordering::Acquire) || !ctl.clock.is_running() {
        return;
    }
    let (has_video, duration) = {
        let i = ctl.info.lock();
        (i.has_video, i.duration)
    };
    if has_video && !ctl.video_eof.load(Ordering::Acquire) {
        return;
    }
    let audio_done = !ctl.audio_active.load(Ordering::Acquire)
        || ctl.audio_failed.load(Ordering::Acquire)
        || ctl.audio_drained.load(Ordering::Acquire);
    if !audio_done {
        return;
    }
    let now = ctl.clock.now();
    let end = if has_video { *ctl.last_frame_end.lock() } else { now };
    let end = if duration > 0.0 { end.min(duration) } else { end };
    if now + 0.001 < end {
        return;
    }
    if ctl.ended.swap(true, Ordering::AcqRel) {
        return;
    }
    let final_pos = if duration > 0.0 { duration } else { now.max(end) };
    ctl.playing.store(false, Ordering::Release);
    ctl.clock.set_playing(false);
    ctl.clock.jump(final_pos, false);
    ctl.audio_wake.notify();
    ctl.announce_playing();
    ctl.emit(PlayerEvent::Ended { generation: ctl.generation });
}

fn audio_main(ctl: Arc<Control>, mut dec: codec::decoder::Audio, tb: Rational) {
    let _priority = priority::raise(priority::Role::AudioDecode);
    let start = ctl.start_time();
    let mut serial = 0u64;
    let mut target = 0.0;
    let mut next_pts = 0.0;
    let mut resampler: Option<Resampler> = None;
    let mut f = frame::Audio::empty();
    loop {
        if ctl.stopped() {
            return;
        }
        let Some(item) = ctl.aq.pop(Duration::from_millis(20)) else { continue };
        ctl.space.notify();
        let current = ctl.serial.load(Ordering::Acquire);
        if item.serial != current {
            continue;
        }
        if item.serial != serial {
            dec.flush();
            serial = item.serial;
            target = ctl.seek_target_for(serial);
            next_pts = target;
            resampler = None;
        }
        let at_eof = item.packet.is_none();
        let sent = match &item.packet {
            Some(p) => dec.send_packet(p),
            None => dec.send_eof(),
        };
        if sent.is_err() && !at_eof {
            continue;
        }
        loop {
            match dec.receive_frame(&mut f) {
                Ok(()) => {
                    let raw = unsafe { f.as_ptr() };
                    let ts = unsafe { (*raw).best_effort_timestamp };
                    let mut pts = if ts != ffi::AV_NOPTS_VALUE { seconds(ts, tb, start) } else { next_pts };
                    // The output format of the current stream (it changes when the device is replaced; the resampler
                    // follows, and chunks made for the old format are dropped by the callback).
                    let Some(device) = DeviceFormat::unpack(ctl.device_format.load(Ordering::Acquire)) else { continue };
                    let channels = device.channels as usize;
                    let max_frames = (device.rate as f64 * AUDIO_RING_SECONDS) as usize;
                    let speed = ctl.speed().clamp(0.25, 4.0);
                    let out_rate = ((device.rate as f64 / speed).round() as i32).max(8000);
                    resampler = unsafe { Resampler::for_frame(resampler.take(), raw, out_rate, channels as i32) };
                    let Some(rs) = resampler.as_mut() else { continue };
                    let mut samples = unsafe { rs.convert(raw) };
                    let fd = 1.0 / rs.out_rate as f64;
                    let n = samples.len() / channels;
                    if n == 0 {
                        continue;
                    }
                    next_pts = pts + n as f64 * fd;
                    if next_pts <= target {
                        continue;
                    }
                    if pts < target {
                        let k = (((target - pts) / fd) as usize).min(n);
                        samples.drain(..k * channels);
                        pts += k as f64 * fd;
                    }
                    let frames = samples.len() / channels;
                    let mut ring = ctl.audio_ring.state.lock();
                    loop {
                        if ctl.stopped() || ctl.serial.load(Ordering::Acquire) != serial || ctl.audio_failed.load(Ordering::Acquire) {
                            break;
                        }
                        if ring.frames < max_frames {
                            ring.chunks.push_back(AudioChunk { serial, pts, frame_dur: fd, samples, channels: device.channels, pos: 0 });
                            ring.frames += frames;
                            break;
                        }
                        ctl.audio_ring.space.wait_for(&mut ring, Duration::from_millis(20));
                    }
                }
                Err(ffmpeg_next::Error::Eof) => {
                    let mut ring = ctl.audio_ring.state.lock();
                    if ctl.serial.load(Ordering::Acquire) == serial {
                        ring.eof_serial = Some(serial);
                    }
                    break;
                }
                Err(_) => break,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hardware_decoder_is_given_up_after_repeated_errors_or_a_hardware_only_one_at_its_first() {
        let bad = ffmpeg_next::Error::InvalidData;
        let nosys = ffmpeg_next::Error::Other { errno: ffi::ENOSYS };
        let again = ffmpeg_next::Error::Other { errno: ffi::EAGAIN };
        let mut h = HwHealth::new(true, 1.0 / 30.0);
        assert!(!h.failed(&again) && !h.failed(&ffmpeg_next::Error::Eof), "needing input is not an error");
        assert!(h.failed(&nosys), "a hardware-only decoder failing before any frame: the GPU cannot decode this codec");
        // The default decoder: damaged packets before the first frame (a capture starting mid-GOP) are survived.
        let mut h = HwHealth::new(false, 1.0 / 30.0);
        assert!(!h.failed(&bad) && !h.failed(&bad));
        assert!(h.failed(&bad), "three errors in a row");
        let mut h = HwHealth::new(true, 1.0 / 30.0);
        h.frame();
        assert!(!h.failed(&bad) && !h.failed(&bad), "one damaged packet is not the end of hardware decoding");
        h.frame();
        assert!(!h.failed(&bad) && !h.failed(&bad));
        assert!(h.failed(&bad), "three errors in a row");
    }

    #[test]
    fn a_hardware_decoder_that_never_outputs_stalls_after_60_packets_or_two_seconds() {
        let mut h = HwHealth::new(false, 1.0 / 25.0);
        for _ in 0..59 {
            h.packet();
        }
        assert!(!h.stalled());
        h.packet();
        assert!(h.stalled());
        h.frame();
        assert!(!h.stalled(), "a frame resets the count");
        for _ in 0..59 {
            h.packet();
        }
        h.flushed();
        h.packet();
        assert!(!h.stalled(), "so does a flush (seek)");
        // 120 fps: two seconds are 240 packets.
        let mut h = HwHealth::new(false, 1.0 / 120.0);
        (0..239).for_each(|_| h.packet());
        assert!(!h.stalled());
        h.packet();
        assert!(h.stalled());
    }

    #[test]
    fn second_fields_are_paced_from_the_frame_before_unless_it_does_not_precede_this_one() {
        let mut p = FieldPacer::default();
        let dur = 0.04;
        assert!((p.half(1.0, dur) - 0.02).abs() < 1e-9, "no previous frame: half the duration");
        assert!((p.half(1.04, dur) - 0.02).abs() < 1e-9);
        // The stream really runs faster than its frame durations say: the measured gap wins.
        assert!((p.half(1.073, dur) - 0.0165).abs() < 1e-9);
        // A dropped frame (gap of two frames) or a seek: the caller resets, and the frame's own duration paces it.
        p.reset();
        assert!((p.half(1.153, dur) - 0.02).abs() < 1e-9);
        p.reset();
        assert!((p.half(1.233, dur) - 0.02).abs() < 1e-9);
        // Without the reset a gap of two frames would put the second field on the next frame's first field.
        assert!((p.half(1.313, dur) - 0.04).abs() < 1e-9);
        // A backward jump or a gap beyond 2.5 frames is never measured.
        assert!((p.half(0.5, dur) - 0.02).abs() < 1e-9);
        assert!((p.half(0.62, dur) - 0.02).abs() < 1e-9);
    }
}
