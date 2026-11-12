//! Bitmap subtitle tracks shown over the video: PGS (written by `common::sup`), VobSub, DVB and XSUB (FFmpeg's
//! encoders, transcoded from a PGS track) decoded in step with playback, with exact pixels, positions and timings
//! where the format keeps them, the clearing events, seeks, track switches, turning the track off, restarts that keep
//! the position, the end of playback and a track whose decoder rejects everything.
//!
//! Every clip is generated once into the fixtures folder under a name that carries a hash of its recipe (the `.sup`
//! bytes and the FFmpeg arguments), so a changed generator never reuses an old file. Waits poll for the state the test
//! expects (with a generous timeout) instead of sleeping a fixed time: the full suite runs on a loaded machine.

#![cfg(feature = "ffmpeg")]

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::sup::{self, Composition, DisplaySet, Object, State};
use common::{Recorder, fixture_file, hashed_clip, is_ended, is_opened, wait_until};
use sa_player::ffmpeg_backend::{AudioTap, FfmpegPlayer, PlayerOptions};
use sa_player::{MediaPlayerBackend, MediaSource, PlayerEvent, SubtitleKind, SubtitlePicture};

const T: Duration = Duration::from_secs(20);
const PGS: usize = 2;
const VOBSUB: usize = 3;
const DVB: usize = 4;
const SRT: usize = 5;

/// The palette: 1 opaque white, 2 opaque red, 3 black at half alpha (Y, Cr, Cb in BT.601 limited range: the canvases
/// here are at most 576 lines high, so FFmpeg converts with BT.601).
const PALETTE: [sup::PaletteEntry; 3] = [(1, 235, 128, 128, 255), (2, 81, 240, 90, 255), (3, 16, 128, 128, 128)];
/// The same entries after a palette update: 1 opaque black, 2 opaque white, 3 unchanged.
const SWAPPED: [sup::PaletteEntry; 3] = [(1, 16, 128, 128, 255), (2, 235, 128, 128, 255), (3, 16, 128, 128, 128)];
const WHITE: [u8; 4] = [255, 255, 255, 255];
const BLACK: [u8; 4] = [0, 0, 0, 255];

/// The PGS track: cleared at 0, two objects at 0.5 (40 × 10 at (100, 200), left half white, right half red; 16 × 4
/// half-black at (20, 20)), one white 30 × 6 at (50, 100) from 1.5, cleared at 2.5, a red 8 × 2 at (10, 10) from 3.2,
/// cleared at 3.6.
fn pgs_track() -> Vec<u8> {
    let canvas = (320, 240);
    let mut out = Vec::new();
    sup::display_set(&mut out, 0.0, canvas, &[], &PALETTE);
    let big = Object { x: 100, y: 200, rows: vec![[vec![1u8; 20], vec![2u8; 20]].concat(); 10] };
    sup::display_set(&mut out, 0.5, canvas, &[big, Object::filled(20, 20, 16, 4, 3)], &PALETTE);
    sup::display_set(&mut out, 1.5, canvas, &[Object::filled(50, 100, 30, 6, 1)], &PALETTE);
    sup::display_set(&mut out, 2.5, canvas, &[], &PALETTE);
    sup::display_set(&mut out, 3.2, canvas, &[Object::filled(10, 10, 8, 2, 2)], &PALETTE);
    sup::display_set(&mut out, 3.6, canvas, &[], &PALETTE);
    out
}

fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// 4 s of 320 × 240 video at 30 fps (a keyframe every second) and a tone; subtitle 2 = the PGS track (`eng`), 3 =
/// VobSub (`tur`), 4 = DVB (`deu`), both from it (`-fix_sub_duration`: each event ends at the next), 5 = SubRip.
fn picture_mkv() -> Option<PathBuf> {
    let sup_path = path_str(&fixture_file("pictures", "sup", &pgs_track()));
    let srt = path_str(&fixture_file("pictures", "srt", b"1\n00:00:01,000 --> 00:00:02,000\nText\n"));
    hashed_clip(
        "pictures",
        "mkv",
        &[
            "-f", "lavfi", "-i", "testsrc2=size=320x240:rate=30",
            "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000",
            "-fix_sub_duration", "-i", &sup_path, "-i", &srt,
            "-t", "4", "-map", "0:v", "-map", "1:a", "-map", "2:s", "-map", "2:s", "-map", "2:s", "-map", "3:s",
            "-c:v", "libx264", "-preset", "ultrafast", "-g", "30", "-pix_fmt", "yuv420p", "-c:a", "aac",
            "-c:s:0", "copy", "-c:s:1", "dvdsub", "-c:s:2", "dvbsub", "-c:s:3", "srt",
            "-metadata:s:s:0", "language=eng", "-metadata:s:s:1", "language=tur", "-metadata:s:s:2", "language=deu",
        ],
    )
}

/// The object of more than 64 KB of RLE: a 300 × 250 checkerboard of white and red (one RLE byte per pixel).
fn large_object() -> Object {
    Object::checkerboard(0, 0, 300, 250, 1, 2)
}

/// A PGS track that uses what the plain writer does not, on a 640 × 480 canvas:
/// * 0.0: an epoch start that clears;
/// * 0.5 (PCS; its other segments at 0.6): two objects in two windows, object 0 the large checkerboard at (10, 10)
///   (several ODS segments), object 1 a half-black 40 × 20 at (400, 400);
/// * 1.5: a palette-only update (normal state, `palette_update_flag`, a new PDS version, no objects): colours 1 and
///   2 swap to black and white, the same objects stay where they are;
/// * 2.5: a normal set that shows object 1 again, decoded at 0.5, at (100, 300) (no ODS, no PDS);
/// * 3.0: an acquisition point that sends object 1 again and shows it at (200, 100);
/// * 3.5: an epoch start that clears.
fn pgs_sets_track() -> Vec<u8> {
    let canvas = (640, 480);
    let big = large_object();
    let small = Object::filled(0, 0, 40, 20, 3);
    let mut out = Vec::new();
    sup::display_set(&mut out, 0.0, canvas, &[], &PALETTE);
    let both = || {
        vec![Composition { object: 0, window: 0, x: 10, y: 10 }, Composition { object: 1, window: 1, x: 400, y: 400 }]
    };
    let windows = || Some(vec![(0, 10, 10, 300, 250), (1, 400, 400, 40, 20)]);
    let set = |pts, state, compositions, windows, palette, objects| DisplaySet {
        pts,
        segments_pts: None,
        canvas,
        composition_number: 0,
        state,
        palette_update: false,
        palette_id: 0,
        compositions,
        windows,
        palette,
        objects,
    };
    sup::write(
        &mut out,
        &DisplaySet { segments_pts: Some(0.6), ..set(0.5, State::EpochStart, both(), windows(), Some((0, 0, &PALETTE[..])), vec![(0, 0, &big), (1, 0, &small)]) },
    );
    sup::write(&mut out, &DisplaySet { palette_update: true, composition_number: 1, ..set(1.5, State::Normal, both(), None, Some((0, 1, &SWAPPED[..])), vec![]) });
    sup::write(
        &mut out,
        &DisplaySet {
            composition_number: 2,
            ..set(2.5, State::Normal, vec![Composition { object: 1, window: 0, x: 100, y: 300 }], Some(vec![(0, 100, 300, 40, 20)]), None, vec![])
        },
    );
    sup::write(
        &mut out,
        &DisplaySet {
            composition_number: 3,
            ..set(
                3.0,
                State::AcquisitionPoint,
                vec![Composition { object: 1, window: 0, x: 200, y: 100 }],
                Some(vec![(0, 200, 100, 40, 20)]),
                Some((0, 1, &SWAPPED[..])),
                vec![(1, 0, &small)],
            )
        },
    );
    sup::display_set(&mut out, 3.5, canvas, &[], &PALETTE);
    out
}

/// 4 s of video with the PGS track of [`pgs_sets_track`] (stream 1).
fn pgs_sets_mkv() -> Option<PathBuf> {
    let sup_path = path_str(&fixture_file("pgs-sets", "sup", &pgs_sets_track()));
    hashed_clip(
        "pgs-sets",
        "mkv",
        &[
            "-f", "lavfi", "-i", "testsrc2=size=320x240:rate=30", "-i", &sup_path, "-t", "4", "-map", "0:v", "-map", "1:s",
            "-c:v", "libx264", "-preset", "ultrafast", "-g", "30", "-pix_fmt", "yuv420p", "-c:s", "copy",
        ],
    )
}

/// The PGS track transcoded to XSUB, in AVI (XSUB's container), stream 1.
fn xsub_avi() -> Option<PathBuf> {
    let sup_path = path_str(&fixture_file("pictures", "sup", &pgs_track()));
    hashed_clip(
        "xsub",
        "avi",
        &[
            "-f", "lavfi", "-i", "testsrc2=size=320x240:rate=30", "-fix_sub_duration", "-i", &sup_path, "-t", "4",
            "-map", "0:v", "-map", "1:s", "-c:v", "mpeg4", "-g", "30", "-c:s", "xsub",
        ],
    )
}

/// A VobSub subpicture too large for one PES packet, in MPEG-PS (`.vob`): the 280 × 200 white / red checkerboard at
/// (20, 20) from 0.5 to 2.0. The muxer splits it over packets of which only the first carries a PTS.
fn vobsub_ps() -> Option<PathBuf> {
    let mut out = Vec::new();
    let canvas = (320, 240);
    sup::display_set(&mut out, 0.0, canvas, &[], &PALETTE);
    sup::display_set(&mut out, 0.5, canvas, &[Object::checkerboard(20, 20, 280, 200, 1, 2)], &PALETTE);
    sup::display_set(&mut out, 2.0, canvas, &[], &PALETTE);
    let sup_path = path_str(&fixture_file("vobsub-ps", "sup", &out));
    hashed_clip(
        "vobsub-ps",
        "vob",
        &[
            "-f", "lavfi", "-i", "testsrc2=size=320x240:rate=25", "-fix_sub_duration", "-i", &sup_path, "-t", "4",
            "-map", "0:v", "-map", "1:s", "-c:v", "mpeg2video", "-g", "25", "-c:s", "dvdsub", "-f", "vob",
        ],
    )
}

/// A DVB track whose display definition segment (FFmpeg's encoder always writes one) is turned into a stuffing
/// segment of the same length, over 1280 × 720 video: the decoder sees no display definition, so the canvas is DVB's
/// default 720 × 576 (where the PGS source placed its object: white 40 × 10 at (100, 200) from 0.5 to 1.5), not the
/// video's size. Stream 1.
fn dvb_without_display_definition() -> Option<PathBuf> {
    let mut out = Vec::new();
    let canvas = (720, 576);
    sup::display_set(&mut out, 0.0, canvas, &[], &PALETTE);
    sup::display_set(&mut out, 0.5, canvas, &[Object::filled(100, 200, 40, 10, 1)], &PALETTE);
    sup::display_set(&mut out, 1.5, canvas, &[], &PALETTE);
    let sup_path = path_str(&fixture_file("dvb-pal", "sup", &out));
    let with_dds = hashed_clip(
        "dvb-pal",
        "mkv",
        &[
            "-f", "lavfi", "-i", "testsrc2=size=1280x720:rate=25", "-fix_sub_duration", "-i", &sup_path, "-t", "3",
            "-map", "0:v", "-map", "1:s", "-c:v", "libx264", "-preset", "ultrafast", "-g", "25", "-pix_fmt", "yuv420p",
            "-c:s", "dvbsub",
        ],
    )?;
    let mut bytes = std::fs::read(&with_dds).expect("clip");
    // `0f 14 <page 2> 00 05`: sync, DDS, page 1, length 5. Segment type 0xFF is stuffing (EN 300 743), skipped.
    let mut patched = 0;
    let pattern = [0x0f, 0x14, 0x00, 0x01, 0x00, 0x05];
    let mut i = 0;
    while i + pattern.len() <= bytes.len() {
        if bytes[i..i + pattern.len()] == pattern {
            bytes[i + 1] = 0xff;
            patched += 1;
            i += pattern.len();
        } else {
            i += 1;
        }
    }
    assert!(patched >= 2, "the encoder's display definition segments were found ({patched})");
    Some(fixture_file("dvb-no-dds", "mkv", &bytes))
}

/// The DVB track of [`picture_mkv`] with every byte of every packet at risk of corruption (FFmpeg's `noise`
/// bitstream filter): the decoder rejects every packet. Stream 1.
fn corrupt_dvb() -> Option<PathBuf> {
    let source = path_str(&picture_mkv()?);
    hashed_clip(
        "dvb-corrupt",
        "mkv",
        &["-i", &source, "-map", "0:v", "-map", "0:4", "-c", "copy", "-bsf:s", "noise=amount=2"],
    )
}

macro_rules! or_skip {
    ($e:expr) => {
        match $e {
            Some(p) => p,
            None => {
                eprintln!("skipped: no ffmpeg CLI or encoder to generate the test clip");
                return;
            }
        }
    };
}

struct Rig {
    p: FfmpegPlayer,
    rec: Recorder,
    g: i32,
}

/// Opens the main clip paused (see [`open_clip`]).
fn open() -> Option<Rig> {
    Some(open_clip(&picture_mkv()?))
}

/// Opens `path` paused, with a virtual audio device (the audio clock is master as with a real one).
fn open_clip(path: &Path) -> Rig {
    let rec = Recorder::default();
    let mut p = FfmpegPlayer::with_options(PlayerOptions { audio_output: false, ..PlayerOptions::default() });
    p.use_audio_tap(Some(AudioTap::new(48_000, 2)));
    rec.attach(&mut p);
    let g = p.load(Some(MediaSource::File(path.to_path_buf())), false);
    assert!(rec.wait_for(0, T, is_opened(g)).is_some(), "{:?}", rec.snapshot());
    assert!(rec.wait_for(0, T, |e| matches!(e, PlayerEvent::TracksChanged { .. })).is_some());
    Rig { p, rec, g }
}

impl Rig {
    /// Asks for `stream` and waits for the `TracksChanged` that answers it.
    fn select(&mut self, stream: Option<usize>) -> Vec<PlayerEvent> {
        let from = self.rec.len();
        self.p.select_image_subtitle_track(stream);
        assert!(self.rec.wait_for(from, T, |e| matches!(e, PlayerEvent::TracksChanged { .. })).is_some(), "no answer to the switch");
        self.rec.snapshot()[from..].to_vec()
    }

    /// Seeks (paused) and waits until the frame there is on screen.
    fn seek(&mut self, t: f64) {
        self.p.seek(t);
        let p = &mut self.p;
        let mut last = None;
        let shown = wait_until(T, || {
            if let Some(f) = p.latest_frame() {
                last = Some(f.pts);
            }
            last.is_some_and(|pts| (pts - t).abs() < 0.05)
        });
        assert!(shown, "no frame at {t} (last {last:?})");
    }

    /// Seeks and waits for the picture that started at `start` (to the ms).
    fn seek_to_picture(&mut self, t: f64, start: f64) -> Arc<SubtitlePicture> {
        self.seek(t);
        self.wait_picture(|p| (p.start - start).abs() < 1e-3).unwrap_or_else(|| panic!("no picture from {start} at {t}: {:?}", self.picture()))
    }

    /// Seeks and waits until nothing is shown (the frame there is on screen first, and the demuxer read past `t`).
    fn seek_to_nothing(&mut self, t: f64) {
        self.seek(t);
        assert!(wait_until(T, || self.picture().is_none()), "a picture at {t}: {:?}", self.picture());
    }

    fn wait_picture(&self, f: impl Fn(&SubtitlePicture) -> bool) -> Option<Arc<SubtitlePicture>> {
        let deadline = Instant::now() + T;
        while Instant::now() < deadline {
            if let Some(p) = self.picture().filter(|p| f(p)) {
                return Some(p);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        None
    }

    fn picture(&self) -> Option<Arc<SubtitlePicture>> {
        self.p.subtitle_picture()
    }

    fn picture_changes(&self, from: usize) -> usize {
        self.rec.snapshot()[from..].iter().filter(|e| matches!(e, PlayerEvent::SubtitlePictureChanged { generation } if *generation == self.g)).count()
    }

    /// Waits until no event came for 300 ms.
    fn settle(&self) {
        let deadline = Instant::now() + T;
        let mut n = self.rec.len();
        let mut quiet = Instant::now();
        while Instant::now() < deadline && quiet.elapsed() < Duration::from_millis(300) {
            std::thread::sleep(Duration::from_millis(10));
            let now = self.rec.len();
            if now != n {
                n = now;
                quiet = Instant::now();
            }
        }
    }
}

fn rect_at(p: &SubtitlePicture, x: u32, y: u32) -> &sa_player::SubtitleBitmap {
    p.rects.iter().find(|r| r.x == x && r.y == y).unwrap_or_else(|| panic!("no rect at {x},{y}: {:?}", p.rects))
}

/// Canvas pixel `(x, y)` of the picture: the rectangle covering it, else transparent.
fn canvas_pixel(p: &SubtitlePicture, x: u32, y: u32) -> [u8; 4] {
    p.rects.iter().find(|r| x >= r.x && y >= r.y && x < r.x + r.width && y < r.y + r.height).map_or([0; 4], |r| r.pixel(x - r.x, y - r.y))
}

fn close(a: [u8; 4], b: [u8; 4], tolerance: u8) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= tolerance)
}

fn is_red(p: [u8; 4]) -> bool {
    p[0] >= 250 && p[1] <= 5 && p[2] <= 5 && p[3] == 255
}

#[test]
fn pgs_runs_are_written_the_way_the_decoder_reads_them() {
    let mut out = Vec::new();
    sup::rle_line(&[5, 0, 0, 0, 7, 7, 7, 7], &mut out);
    assert_eq!(out, vec![5, 0, 3, 0, 0x84, 7, 0, 0]);
    let mut out = Vec::new();
    sup::rle_line(&[0; 100], &mut out);
    assert_eq!(out, vec![0, 0x40, 100, 0, 0]);
    let mut out = Vec::new();
    sup::rle_line(&[9; 300], &mut out);
    assert_eq!(out, vec![0, 0xC1, 44, 9, 0, 0]);
    // 75 000 bytes of RLE: two ODS segments.
    assert_eq!(sup::object_segment_count(&large_object()), 2);
    assert_eq!(sup::object_segment_count(&Object::filled(0, 0, 8, 2, 1)), 1);
}

#[test]
#[should_panic(expected = "at most 65535 bytes")]
fn a_pgs_segment_that_does_not_fit_its_length_field_fails_loudly() {
    // A PDS of 14 000 entries (70 002 bytes) cannot be written: no silently wrapped length.
    let huge: Vec<sup::PaletteEntry> = (0..14_000).map(|i| ((i % 256) as u8, 16, 128, 128, 255)).collect();
    let mut out = Vec::new();
    sup::write(
        &mut out,
        &DisplaySet {
            pts: 0.0,
            segments_pts: None,
            canvas: (320, 240),
            composition_number: 0,
            state: State::EpochStart,
            palette_update: false,
            palette_id: 0,
            compositions: vec![],
            windows: None,
            palette: Some((0, 0, &huge)),
            objects: vec![],
        },
    );
}

#[test]
fn picture_tracks_are_listed_as_images_and_the_text_one_as_text() {
    let r = or_skip!(open());
    let t = r.p.tracks();
    let kinds: Vec<(usize, &str, SubtitleKind)> = t.subtitles.iter().map(|s| (s.stream_index, s.codec.as_str(), s.kind)).collect();
    assert_eq!(
        kinds,
        vec![
            (PGS, "hdmv_pgs_subtitle", SubtitleKind::Image),
            (VOBSUB, "dvd_subtitle", SubtitleKind::Image),
            (DVB, "dvb_subtitle", SubtitleKind::Image),
            (SRT, "subrip", SubtitleKind::Text)
        ]
    );
    assert_eq!(t.active_image_subtitle, None, "nothing is shown until asked for");
    assert!(r.picture().is_none());
}

#[test]
fn pgs_pictures_have_their_exact_pixels_positions_and_times_and_the_empty_set_clears_them() {
    let mut r = or_skip!(open());
    let events = r.select(Some(PGS));
    assert!(!events.iter().any(|e| matches!(e, PlayerEvent::SubtitleTrackFailed { .. })), "{events:?}");
    assert_eq!(r.p.tracks().active_image_subtitle, Some(PGS));
    let p = r.seek_to_picture(0.7, 0.5);
    assert_eq!((p.stream_index, p.canvas, p.end), (PGS, (320, 240), None), "PGS events end with the next display set");
    assert_eq!(p.video_size, Some((320, 240)), "the video's coded size comes along");
    assert_eq!(p.rects.len(), 2, "{:?}", p.rects);
    let big = rect_at(&p, 100, 200);
    assert_eq!((big.width, big.height, big.rgba.len()), (40, 10, 40 * 10 * 4));
    // Straight alpha, RGBA byte order: Y 235 / 16 with neutral chroma are exact white and black.
    assert_eq!(big.pixel(0, 0), WHITE);
    assert_eq!(big.pixel(19, 9), WHITE);
    let red = big.pixel(20, 0);
    assert!(is_red(red), "red, not blue: {red:?}");
    assert_eq!(big.pixel(39, 9), red);
    let small = rect_at(&p, 20, 20);
    assert_eq!((small.width, small.height), (16, 4));
    assert_eq!(small.pixel(0, 0), [0, 0, 0, 128], "half alpha is not premultiplied away");
    assert_eq!(p.bounds(), (20, 20, 120, 190));

    let q = r.seek_to_picture(1.7, 1.5);
    assert_ne!(q.id, p.id);
    assert_eq!(q.rects.len(), 1);
    let one = rect_at(&q, 50, 100);
    assert_eq!((one.width, one.height, one.pixel(29, 5)), (30, 6, WHITE));

    r.seek_to_nothing(2.7);
    let last = r.seek_to_picture(3.3, 3.2);
    assert_eq!((last.rects[0].x, last.rects[0].y, last.rects[0].width, last.rects[0].height), (10, 10, 8, 2));
    let s = r.p.stats();
    assert_eq!((s.picture_failures, s.picture_overflow), (0, 0));
    assert!(s.picture_events >= 4 && s.pictures_shown >= 3, "{s:?}");
    let diag = r.p.diagnostics().unwrap();
    assert!(
        diag.contains("image_subtitle=2") && diag.contains("picture_size=8x2") && diag.contains("picture_canvas=320x240") && diag.contains("picture_overflow=0"),
        "{diag}"
    );
}

#[test]
fn pgs_display_sets_split_objects_palette_updates_reused_objects_and_acquisition_points() {
    let mut r = open_clip(&or_skip!(pgs_sets_mkv()));
    r.select(Some(1));
    // Shown from its composition (0.5), not from the END segment that completed it (0.6).
    let p = r.seek_to_picture(0.55, 0.5);
    assert_eq!(p.canvas, (640, 480));
    assert_eq!(p.rects.len(), 2, "{:?}", p.rects);
    // The object split over two ODS segments, pixel for pixel.
    let board = rect_at(&p, 10, 10);
    assert_eq!((board.width, board.height), (300, 250));
    assert!(is_red(board.pixel(1, 0)), "{:?}", board.pixel(1, 0));
    for (x, y) in [(0, 0), (1, 0), (0, 1), (299, 249), (298, 249), (150, 125), (151, 125), (17, 200)] {
        let want = if (x + y) % 2 == 0 { WHITE } else { board.pixel(1, 0) };
        assert_eq!(board.pixel(x, y), want, "({x}, {y})");
    }
    // The second object, in its own window.
    let small = rect_at(&p, 400, 400);
    assert_eq!((small.width, small.height, small.pixel(39, 19)), (40, 20, [0, 0, 0, 128]));

    // The sets after it build on what the decoder kept from 0.5 (a palette, objects): played, not seeked into (a
    // seek past 1.0 starts reading at the keyframe there, after the objects; such a set then shows nothing until the
    // next acquisition point or epoch start, as in other players).
    let from = r.rec.len();
    r.p.play();
    let mut pictures: Vec<Arc<SubtitlePicture>> = Vec::new();
    let deadline = Instant::now() + T;
    while !r.p.is_ended() && Instant::now() < deadline {
        if let Some(q) = r.picture()
            && pictures.last().is_none_or(|l| l.id != q.id)
        {
            pictures.push(q);
        }
        let _ = r.p.latest_frame();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(r.rec.wait_for(from, T, is_ended(r.g)).is_some());
    let starts: Vec<f64> = pictures.iter().map(|q| (q.start * 10.0).round() / 10.0).collect();
    assert_eq!(starts, vec![0.5, 1.5, 2.5, 3.0], "{pictures:?}");

    // Palette-only update: the same objects in the same places, colours 1 and 2 now black and white.
    let q = &pictures[1];
    assert_eq!(q.rects.len(), 2, "{:?}", q.rects);
    let board = rect_at(q, 10, 10);
    assert_eq!((board.width, board.height, board.pixel(0, 0), board.pixel(1, 0), board.pixel(299, 249)), (300, 250, BLACK, WHITE, BLACK));
    assert_eq!(rect_at(q, 400, 400).pixel(0, 0), [0, 0, 0, 128]);
    // A normal set shows object 1 again (decoded at 0.5) somewhere else.
    let n = &pictures[2];
    assert_eq!(n.rects.len(), 1, "{:?}", n.rects);
    let moved = rect_at(n, 100, 300);
    assert_eq!((moved.width, moved.height, moved.pixel(0, 0)), (40, 20, [0, 0, 0, 128]));
    // An acquisition point that sends it again, so it shows after a seek too.
    let a = &pictures[3];
    assert_eq!(a.rects.len(), 1, "{:?}", a.rects);
    assert_eq!((rect_at(a, 200, 100).width, rect_at(a, 200, 100).pixel(5, 5)), (40, [0, 0, 0, 128]));
    assert!(r.picture().is_none(), "cleared at 3.5");
    let a = r.seek_to_picture(3.1, 3.0);
    assert_eq!((a.rects.len(), a.rects[0].x, a.rects[0].y), (1, 200, 100));
    assert_eq!(r.p.stats().picture_failures, 0);
}

#[test]
fn playing_shows_each_picture_when_it_is_due_never_early_and_the_video_plays_on_to_the_end() {
    let mut r = or_skip!(open());
    r.select(Some(PGS));
    r.seek_to_nothing(0.0);
    let from = r.rec.len();
    r.p.play();
    // (position seen right after the change, the shown picture's start)
    let mut seen: Vec<(f64, Option<f64>)> = Vec::new();
    let deadline = Instant::now() + T;
    while !r.p.is_ended() && Instant::now() < deadline {
        let now = r.picture().map(|p| p.start);
        if seen.last().map(|s| s.1) != Some(now) {
            seen.push((r.p.position(), now));
        }
        let _ = r.p.latest_frame();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(r.rec.wait_for(from, T, is_ended(r.g)).is_some(), "the video plays to its end");
    let starts: Vec<Option<f64>> = seen.iter().map(|s| s.1.map(|t| (t * 10.0).round() / 10.0)).collect();
    assert_eq!(starts, vec![None, Some(0.5), Some(1.5), None, Some(3.2), None], "{seen:?}");
    // Each change came with the frame due then: never before it (one 30 fps frame of slack for the clock reading),
    // and not so late that it would show (generous: this test polls on a busy machine; the timeline's unit tests
    // check the rule itself).
    let due = [0.0, 0.5, 1.5, 2.5, 3.2, 3.6];
    for ((at, _), want) in seen.iter().zip(due).skip(1) {
        assert!(*at >= want - 0.034, "changed at {at}, before it was due at {want}: {seen:?}");
        assert!(*at < want + 0.4, "changed at {at}, due at {want}: {seen:?}");
    }
    assert!(r.picture_changes(from) >= 5);
    let s = r.p.stats();
    assert!(s.presented >= 100, "the video kept playing: {s:?}");
}

#[test]
fn a_seek_never_shows_a_picture_of_the_position_before_it() {
    let mut r = or_skip!(open());
    r.select(Some(PGS));
    let before = r.seek_to_picture(0.7, 0.5).id;
    // Forward into the cleared stretch and back into the second picture: the first never comes back. Until the
    // frame of the new position is on screen the old frame and its picture stay (paused); from that frame on only
    // pictures of the new position show.
    for (t, want) in [(2.7, None), (1.7, Some(1.5)), (0.2, None), (3.3, Some(3.2))] {
        r.seek(t);
        let deadline = Instant::now() + Duration::from_millis(600);
        while Instant::now() < deadline {
            let p = r.picture();
            assert!(p.as_ref().is_none_or(|p| p.id != before), "a picture from before the seek to {t}");
            if let Some(p) = &p {
                assert!(p.start <= t + 1e-6, "a picture ahead of {t}: {}", p.start);
            }
            let _ = r.p.latest_frame();
            std::thread::sleep(Duration::from_millis(2));
        }
        match want {
            Some(start) => assert!(r.wait_picture(|p| (p.start - start).abs() < 1e-3).is_some(), "after the seek to {t}: {:?}", r.picture()),
            None => assert!(wait_until(T, || r.picture().is_none()), "after the seek to {t}: {:?}", r.picture()),
        }
    }
}

#[test]
fn vobsub_pictures_keep_their_place_and_their_own_ends() {
    let mut r = or_skip!(open());
    r.select(Some(VOBSUB));
    assert_eq!(r.p.tracks().active_image_subtitle, Some(VOBSUB));
    let p = r.seek_to_picture(0.7, 0.5);
    assert_eq!((p.stream_index, p.canvas), (VOBSUB, (320, 240)), "the size from the track's idx header");
    assert!(p.end.is_some_and(|e| (e - 1.5).abs() < 0.02), "VobSub events carry their end: {:?}", p.end);
    // One subpicture holds both objects (cropped to them); four colours with 4-bit alpha survive the DVD palette.
    assert_eq!(p.bounds(), (20, 20, 120, 190), "{:?}", p.rects);
    assert!(close(canvas_pixel(&p, 105, 205), WHITE, 24), "{:?}", canvas_pixel(&p, 105, 205));
    let red = canvas_pixel(&p, 130, 205);
    assert!(red[0] > 180 && red[1] < 80 && red[2] < 80 && red[3] == 255, "{red:?}");
    let shade = canvas_pixel(&p, 25, 22);
    assert!(shade[0] < 40 && shade[1] < 40 && shade[2] < 40 && (100..=160).contains(&shade[3]), "{shade:?}");
    assert_eq!(canvas_pixel(&p, 60, 100), [0, 0, 0, 0], "between the objects");
    let q = r.seek_to_picture(1.7, 1.5);
    assert_eq!(q.bounds(), (50, 100, 30, 6));
    r.seek_to_nothing(2.7);
}

#[test]
fn a_vobsub_subpicture_split_over_packets_without_timestamps_is_put_back_together() {
    let path = or_skip!(vobsub_ps());
    // In the file the subpicture is spread over many PES packets (private stream 1, 00 00 01 BD), and FFmpeg's
    // MPEG-PS demuxer with its dvdsub parser hands it over whole: one packet, with the PTS.
    let bytes = std::fs::read(&path).expect("vob");
    let pes = bytes.windows(4).filter(|w| *w == [0, 0, 1, 0xBD]).count();
    assert!(pes >= 5, "{pes} PES packets");
    let mut ictx = ffmpeg_next::format::input(&path).expect("vob");
    let sub = ictx.streams().find(|s| s.parameters().medium() == ffmpeg_next::media::Type::Subtitle).expect("a subtitle stream").index();
    let packets: Vec<bool> = ictx.packets().filter(|(s, _)| s.index() == sub).map(|(_, p)| p.pts().is_some()).collect();
    assert_eq!(packets, vec![true], "reassembled by the parser");
    drop(ictx);

    // Played: exact place and time (0.5 after the file's start), the end from the subpicture's stop command.
    let mut r = open_clip(&path);
    assert!(r.p.tracks().subtitles.iter().any(|t| t.stream_index == sub && t.kind == SubtitleKind::Image));
    r.select(Some(sub));
    // Played from the start (seeks in this MPEG-PS file land on a keyframe found by a timestamp search, which may
    // be past the subtitle packet).
    r.p.play();
    let p = r.wait_picture(|p| (p.start - 0.5).abs() < 1e-3).unwrap_or_else(|| panic!("no picture: {:?}", r.rec.snapshot()));
    r.p.pause();
    assert!(p.end.is_some_and(|e| (e - 2.0).abs() < 0.02), "{:?}", p.end);
    // No idx header in MPEG-PS: the canvas is the video's coded size.
    assert_eq!(p.canvas, (320, 240));
    assert_eq!(p.bounds(), (20, 20, 280, 200), "{:?}", p.rects);
    // The whole checkerboard came through: alternating, visible pixels to its last row (without the palette the
    // DVD decoder picks its own colours, so only the pattern is compared).
    let (a, b) = (canvas_pixel(&p, 20, 20), canvas_pixel(&p, 21, 20));
    assert!(a[3] > 0 && b[3] > 0 && a != b, "{a:?} {b:?}");
    for (x, y) in [(22, 20), (21, 21), (299, 219), (298, 219), (160, 120), (161, 120)] {
        let want = if (x + y) % 2 == 0 { a } else { b };
        assert_eq!(canvas_pixel(&p, x, y), want, "({x}, {y})");
    }
    assert_eq!(r.p.stats().picture_failures, 0);
    drop(r);

    // A demuxer without that parser: the same packet in 8 pieces, only the first with a timestamp. Every piece
    // reaches the decoder, which puts the subpicture back together; the event keeps the first piece's time.
    let whole = sa_player::ffmpeg_backend::decode_picture_stream(&path, sub, 1).expect("decoded");
    let pieces = sa_player::ffmpeg_backend::decode_picture_stream(&path, sub, 8).expect("decoded");
    assert_eq!(whole.len(), 1, "{whole:?}");
    assert_eq!(pieces.len(), 1, "{pieces:?}");
    let ((s1, e1, p1), (s2, e2, p2)) = (&whole[0], &pieces[0]);
    assert!((s1 - 0.5).abs() < 1e-3 && (s2 - s1).abs() < 1e-9 && e1 == e2, "{s1} {s2} {e1:?} {e2:?}");
    let (p1, p2) = (p1.as_ref().unwrap(), p2.as_ref().unwrap());
    assert_eq!((p1.bounds(), p1.canvas), (p2.bounds(), p2.canvas));
    assert_eq!(p1.rects.iter().map(|r| r.rgba.clone()).collect::<Vec<_>>(), p2.rects.iter().map(|r| r.rgba.clone()).collect::<Vec<_>>());
}

#[test]
fn dvb_pictures_are_shown_too() {
    let mut r = or_skip!(open());
    r.select(Some(DVB));
    let p = r.seek_to_picture(0.7, 0.5);
    assert_eq!(p.stream_index, DVB);
    // FFmpeg's encoder writes a display definition of the source canvas: pictures keep their exact place on it.
    assert_eq!(p.canvas, (320, 240));
    assert_eq!(p.bounds(), (20, 20, 120, 190), "{:?}", p.rects);
    r.seek_to_nothing(2.7);
}

#[test]
fn a_dvb_track_without_a_display_definition_is_placed_on_dvbs_default_display() {
    let mut r = open_clip(&or_skip!(dvb_without_display_definition()));
    r.select(Some(1));
    let p = r.seek_to_picture(0.7, 0.5);
    assert_eq!(p.canvas, (720, 576), "not the 1280 × 720 video");
    assert_eq!(p.video_size, Some((1280, 720)));
    assert_eq!(p.bounds(), (100, 200, 40, 10), "{:?}", p.rects);
    assert_eq!(canvas_pixel(&p, 120, 205)[3], 255);
    r.seek_to_nothing(1.7);
}

#[test]
fn xsub_events_are_timed_by_their_own_time_codes() {
    let path = or_skip!(xsub_avi());
    // The premise: AVI numbers the subtitle chunks (0, 1, 2 in the stream's 1/30 time base), so only the time code
    // in each packet says when it is shown (FFmpeg's AVI muxer also puts them all at the start of the file).
    let mut ictx = ffmpeg_next::format::input(&path).expect("avi");
    let pts: Vec<Option<i64>> = ictx.packets().filter(|(s, _)| s.index() == 1).map(|(_, p)| p.pts()).collect();
    assert_eq!(pts, vec![Some(0), Some(1), Some(2)]);
    drop(ictx);
    let mut r = open_clip(&path);
    let sub = r.p.tracks().subtitles.iter().find(|t| t.codec == "xsub").map(|t| (t.stream_index, t.kind));
    assert_eq!(sub, Some((1, SubtitleKind::Image)));
    r.select(Some(1));
    r.seek_to_nothing(0.0);
    let from = r.rec.len();
    r.p.play();
    let mut seen: Vec<(f64, Option<Arc<SubtitlePicture>>)> = Vec::new();
    let deadline = Instant::now() + T;
    while !r.p.is_ended() && Instant::now() < deadline {
        let now = r.picture();
        if seen.last().is_none_or(|l| l.1.as_ref().map(|p| p.id) != now.as_ref().map(|p| p.id)) {
            seen.push((r.p.position(), now));
        }
        let _ = r.p.latest_frame();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(r.rec.wait_for(from, T, is_ended(r.g)).is_some());
    let shown: Vec<_> = seen.iter().filter_map(|(at, p)| p.as_ref().map(|p| (*at, p.clone()))).collect();
    // FFmpeg's XSUB encoder keeps the first rectangle of an event.
    let want = [(0.5, 1.5, (100, 200, 40, 10)), (1.5, 2.5, (50, 100, 30, 6)), (3.2, 3.6, (10, 10, 8, 2))];
    assert_eq!(shown.len(), want.len(), "{seen:?}");
    for ((at, p), (start, end, bounds)) in shown.iter().zip(want) {
        assert!((p.start - start).abs() < 1e-3 && p.end.is_some_and(|e| (e - end).abs() < 1e-3), "{start}..{end}: {:?}..{:?}", p.start, p.end);
        assert_eq!(p.bounds(), bounds, "{:?}", p.rects);
        assert!(*at >= start - 0.034 && *at < start + 0.4, "shown at {at}, due at {start}");
    }
    // Each one went at its own end: nothing between 2.5 and 3.2 and after 3.6.
    let gaps: Vec<f64> = seen.iter().filter(|(_, p)| p.is_none()).map(|(at, _)| *at).collect();
    assert!(gaps.iter().any(|at| (2.5..3.2).contains(at)) && gaps.iter().any(|at| *at >= 3.6), "{gaps:?}");
    assert_eq!(r.p.stats().picture_failures, 0);
}

#[test]
fn a_track_whose_decoder_rejects_every_packet_is_turned_off_and_reported() {
    let mut r = open_clip(&or_skip!(corrupt_dvb()));
    let from = r.rec.len();
    // It opens (the decoder does), and is given up once packet after packet is rejected.
    r.select(Some(1));
    r.p.play();
    let failed = r.rec.wait_for(from, T, |e| matches!(e, PlayerEvent::SubtitleTrackFailed { stream_index: 1, .. }));
    let Some(PlayerEvent::SubtitleTrackFailed { message, generation, .. }) = failed else { panic!("no failure: {:?}", r.rec.snapshot()) };
    assert_eq!(generation, r.g);
    assert!(!message.is_empty());
    assert!(wait_until(T, || r.p.tracks().active_image_subtitle.is_none()), "turned off");
    assert!(r.picture().is_none());
    assert!(r.p.stats().picture_failures >= 4, "{:?}", r.p.stats());
    let presented = r.p.stats().presented;
    assert!(wait_until(T, || r.p.stats().presented > presented + 10 || r.p.is_ended()), "the video plays on");
}

#[test]
fn an_open_picture_is_taken_off_when_playback_ends() {
    // The sets clip cut at 3.4: the acquisition point's picture (3.0, no end of its own) is never cleared by the
    // track, and the clock stops at the end, so only the end of playback can take it off.
    let sets = or_skip!(pgs_sets_mkv());
    let cut = or_skip!(hashed_clip("pgs-sets-cut", "mkv", &["-i", &path_str(&sets), "-t", "3.4", "-map", "0", "-c", "copy"]));
    let mut r = open_clip(&cut);
    r.select(Some(1));
    r.seek_to_picture(3.1, 3.0);
    let from = r.rec.len();
    r.p.play();
    assert!(r.rec.wait_for(from, T, is_ended(r.g)).is_some());
    assert!(wait_until(T, || r.picture().is_none()), "the open picture stayed after the end: {:?}", r.picture());
    assert!(r.picture_changes(from) >= 1);
}

#[test]
fn another_speed_restarts_decoding_in_place_and_keeps_the_picture_on_screen() {
    let mut r = or_skip!(open());
    r.select(Some(PGS));
    let p = r.seek_to_picture(0.7, 0.5);
    r.settle();
    let restarts = r.p.stats().decoded;
    // Another speed restarts decoding at the position (the queued audio was resampled for the old one): the
    // caption on screen must not blink off while the stream is read again.
    r.p.set_rate(1.5);
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        assert_eq!(r.picture().map(|q| q.start), Some(p.start), "the caption went away during the restart");
        let _ = r.p.latest_frame();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(r.p.stats().decoded > restarts, "decoding did restart");
    assert!((r.p.position() - 0.7).abs() < 0.05, "in place: {}", r.p.position());
    // A seek still takes it off.
    r.seek_to_nothing(2.7);
}

#[test]
fn switching_tracks_and_turning_them_off_replaces_the_picture_and_a_text_track_is_refused() {
    let mut r = or_skip!(open());
    r.select(Some(PGS));
    r.seek_to_picture(0.7, 0.5);
    // Paused: the switch restarts decoding at the position and the new track's picture there shows.
    r.select(Some(VOBSUB));
    assert!(r.wait_picture(|p| p.stream_index == VOBSUB).is_some(), "{:?}", r.picture());
    assert_eq!(r.p.tracks().active_image_subtitle, Some(VOBSUB));
    assert!((r.p.position() - 0.7).abs() < 0.05, "the position is kept: {}", r.p.position());
    // Asking for the track shown does nothing: no switch is answered.
    r.settle();
    let n = r.rec.len();
    r.p.select_image_subtitle_track(Some(VOBSUB));
    r.settle();
    let after = &r.rec.snapshot()[n..];
    assert!(!after.iter().any(|e| matches!(e, PlayerEvent::TracksChanged { .. } | PlayerEvent::SubtitleTrackFailed { .. })), "{after:?}");
    assert_eq!(r.picture().map(|p| p.stream_index), Some(VOBSUB));
    // A text track is not a picture track: refused, the shown one stays.
    let events = r.select(Some(SRT));
    assert!(events.iter().any(|e| matches!(e, PlayerEvent::SubtitleTrackFailed { stream_index, .. } if *stream_index == SRT)), "{events:?}");
    assert_eq!(r.p.tracks().active_image_subtitle, Some(VOBSUB));
    // Off.
    let from = r.rec.len();
    r.select(None);
    assert_eq!(r.p.tracks().active_image_subtitle, None);
    assert!(r.picture().is_none());
    assert!(r.picture_changes(from) >= 1, "the picture going is announced");
    r.seek(1.7);
    r.settle();
    assert!(r.picture().is_none(), "nothing is decoded while off");
    // And back on while playing: the video goes on.
    r.select(Some(PGS));
    r.p.play();
    assert!(r.wait_picture(|p| p.stream_index == PGS && (p.start - 1.5).abs() < 1e-6).is_some());
    let presented = r.p.stats().presented;
    assert!(wait_until(T, || r.p.stats().presented > presented + 10), "frames keep coming");
}
