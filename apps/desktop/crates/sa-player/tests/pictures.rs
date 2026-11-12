//! Bitmap subtitle tracks shown over the video: PGS (written by `common::sup`), VobSub and DVB (FFmpeg's encoders,
//! transcoded from the PGS track) decoded in step with playback, with exact pixels, positions and timings where the
//! format keeps them, the clearing events, seeks, track switches and turning the track off.

#![cfg(feature = "ffmpeg")]

mod common;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::sup::{self, Object};
use common::{Recorder, custom_clip, fixtures_dir, is_ended, is_opened, wait_until};
use sa_player::ffmpeg_backend::{AudioTap, FfmpegPlayer, PlayerOptions};
use sa_player::{MediaPlayerBackend, MediaSource, PlayerEvent, SubtitleKind, SubtitlePicture};

const T: Duration = Duration::from_secs(10);
const PGS: usize = 2;
const VOBSUB: usize = 3;
const DVB: usize = 4;
const SRT: usize = 5;

/// The palette: 1 opaque white, 2 opaque red, 3 black at half alpha (Y, Cr, Cb in BT.601 limited range: the canvas
/// is 240 lines high, so FFmpeg converts with BT.601).
const PALETTE: [sup::PaletteEntry; 3] = [(1, 235, 128, 128, 255), (2, 81, 240, 90, 255), (3, 16, 128, 128, 128)];

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

/// 4 s of 320 × 240 video at 30 fps (a keyframe every second) and a tone; subtitle 2 = the PGS track (`eng`), 3 =
/// VobSub (`tur`), 4 = DVB (`deu`), both from it (`-fix_sub_duration`: each event ends at the next), 5 = SubRip.
fn picture_mkv() -> Option<PathBuf> {
    let dir = fixtures_dir();
    let sup_path = dir.join("pictures-v1.sup");
    if !sup_path.exists() {
        std::fs::write(&sup_path, pgs_track()).expect("sup");
    }
    let srt = dir.join("pictures-v1.srt");
    if !srt.exists() {
        std::fs::write(&srt, "1\n00:00:01,000 --> 00:00:02,000\nText\n").expect("srt");
    }
    let (sup_path, srt) = (sup_path.to_string_lossy().into_owned(), srt.to_string_lossy().into_owned());
    custom_clip(
        "pictures-v1.mkv",
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

/// Opens the clip paused, with a virtual audio device (the audio clock is master as with a real one).
fn open() -> Option<Rig> {
    let path = picture_mkv()?;
    let rec = Recorder::default();
    let mut p = FfmpegPlayer::with_options(PlayerOptions { audio_output: false, ..PlayerOptions::default() });
    p.use_audio_tap(Some(AudioTap::new(48_000, 2)));
    rec.attach(&mut p);
    let g = p.load(Some(MediaSource::File(path)), false);
    assert!(rec.wait_for(0, T, is_opened(g)).is_some(), "{:?}", rec.snapshot());
    assert!(rec.wait_for(0, T, |e| matches!(e, PlayerEvent::TracksChanged { .. })).is_some());
    Some(Rig { p, rec, g })
}

impl Rig {
    /// Asks for `stream` and waits for the `TracksChanged` that answers it.
    fn select(&mut self, stream: Option<usize>) -> Vec<PlayerEvent> {
        let from = self.rec.len();
        self.p.select_image_subtitle_track(stream);
        assert!(self.rec.wait_for(from, T, |e| matches!(e, PlayerEvent::TracksChanged { .. })).is_some(), "no answer to the switch");
        self.rec.snapshot()[from..].to_vec()
    }

    /// Seeks (paused) and waits until the frame there is on screen and the picture due there settled (a picture
    /// decoded after the frame shows within moments).
    fn seek(&mut self, t: f64) {
        self.p.seek(t);
        let deadline = Instant::now() + T;
        loop {
            if let Some(f) = self.p.latest_frame()
                && (f.pts - t).abs() < 0.05
            {
                break;
            }
            assert!(Instant::now() < deadline, "no frame at {t}");
            std::thread::sleep(Duration::from_millis(2));
        }
        std::thread::sleep(Duration::from_millis(250));
    }

    fn picture(&self) -> Option<Arc<SubtitlePicture>> {
        self.p.subtitle_picture()
    }

    fn picture_changes(&self, from: usize) -> usize {
        self.rec.snapshot()[from..].iter().filter(|e| matches!(e, PlayerEvent::SubtitlePictureChanged { generation } if *generation == self.g)).count()
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
    r.seek(0.7);
    let p = r.picture().expect("the picture shown from 0.5");
    assert_eq!((p.stream_index, p.canvas, p.end), (PGS, (320, 240), None), "PGS events end with the next display set");
    assert!((p.start - 0.5).abs() < 1e-6, "{}", p.start);
    assert_eq!(p.rects.len(), 2, "{:?}", p.rects);
    let big = rect_at(&p, 100, 200);
    assert_eq!((big.width, big.height, big.rgba.len()), (40, 10, 40 * 10 * 4));
    // Straight alpha, RGBA byte order: Y 235 / 16 with neutral chroma are exact white and black.
    assert_eq!(big.pixel(0, 0), [255, 255, 255, 255]);
    assert_eq!(big.pixel(19, 9), [255, 255, 255, 255]);
    let red = big.pixel(20, 0);
    assert!(red[0] >= 250 && red[1] <= 5 && red[2] <= 5 && red[3] == 255, "red, not blue: {red:?}");
    assert_eq!(big.pixel(39, 9), red);
    let small = rect_at(&p, 20, 20);
    assert_eq!((small.width, small.height), (16, 4));
    assert_eq!(small.pixel(0, 0), [0, 0, 0, 128], "half alpha is not premultiplied away");
    assert_eq!(p.bounds(), (20, 20, 120, 190));

    r.seek(1.7);
    let q = r.picture().expect("the picture shown from 1.5");
    assert!((q.start - 1.5).abs() < 1e-6 && q.id != p.id);
    assert_eq!(q.rects.len(), 1);
    let one = rect_at(&q, 50, 100);
    assert_eq!((one.width, one.height, one.pixel(29, 5)), (30, 6, [255, 255, 255, 255]));

    r.seek(2.7);
    assert!(r.picture().is_none(), "the display set without objects at 2.5 cleared the screen");
    r.seek(3.3);
    let last = r.picture().expect("the picture shown from 3.2");
    assert_eq!((last.rects[0].x, last.rects[0].y, last.rects[0].width, last.rects[0].height), (10, 10, 8, 2));
    let s = r.p.stats();
    assert_eq!(s.picture_failures, 0);
    assert!(s.picture_events >= 4 && s.pictures_shown >= 3, "{s:?}");
    let diag = r.p.diagnostics().unwrap();
    assert!(diag.contains("image_subtitle=2") && diag.contains("picture_size=8x2") && diag.contains("picture_canvas=320x240"), "{diag}");
}

#[test]
fn playing_shows_each_picture_when_it_is_due_and_the_video_plays_on_to_the_end() {
    let mut r = or_skip!(open());
    r.select(Some(PGS));
    r.seek(0.0);
    assert!(r.picture().is_none(), "the first display set clears");
    let from = r.rec.len();
    r.p.play();
    // (position when it changed, the shown picture's start)
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
    // Each change came with the frame due then (one 30 fps frame, plus the polling of this test).
    let due = [0.0, 0.5, 1.5, 2.5, 3.2, 3.6];
    for ((at, _), want) in seen.iter().zip(due).skip(1) {
        assert!((at - want).abs() < 0.1, "changed at {at}, due at {want}: {seen:?}");
    }
    assert!(r.picture_changes(from) >= 5);
    let s = r.p.stats();
    assert!(s.presented >= 100, "the video kept playing: {s:?}");
}

#[test]
fn a_seek_never_shows_a_picture_of_the_position_before_it() {
    let mut r = or_skip!(open());
    r.select(Some(PGS));
    r.seek(0.7);
    let before = r.picture().expect("shown at 0.7").id;
    // Forward into the cleared stretch and back into the second picture: the first never comes back. Until the
    // frame of the new position is on screen the old frame and its picture stay (paused); from that frame on only
    // pictures of the new position show.
    for (t, want) in [(2.7, None), (1.7, Some(1.5)), (0.2, None), (3.3, Some(3.2))] {
        r.p.seek(t);
        assert!(wait_until(T, || r.p.latest_frame().is_some_and(|f| (f.pts - t).abs() < 0.05)), "no frame at {t}");
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
        assert_eq!(r.picture().map(|p| (p.start * 10.0).round() / 10.0), want, "after the seek to {t}");
    }
}

#[test]
fn vobsub_pictures_keep_their_place_and_their_own_ends() {
    let mut r = or_skip!(open());
    r.select(Some(VOBSUB));
    assert_eq!(r.p.tracks().active_image_subtitle, Some(VOBSUB));
    r.seek(0.7);
    let p = r.picture().expect("the VobSub picture shown from 0.5");
    assert_eq!((p.stream_index, p.canvas), (VOBSUB, (320, 240)), "the size from the track's idx header");
    assert!((p.start - 0.5).abs() < 1e-3, "{}", p.start);
    assert!(p.end.is_some_and(|e| (e - 1.5).abs() < 0.02), "VobSub events carry their end: {:?}", p.end);
    // One subpicture holds both objects (cropped to them); four colours with 4-bit alpha survive the DVD palette.
    assert_eq!(p.bounds(), (20, 20, 120, 190), "{:?}", p.rects);
    assert!(close(canvas_pixel(&p, 105, 205), [255, 255, 255, 255], 24), "{:?}", canvas_pixel(&p, 105, 205));
    let red = canvas_pixel(&p, 130, 205);
    assert!(red[0] > 180 && red[1] < 80 && red[2] < 80 && red[3] == 255, "{red:?}");
    let shade = canvas_pixel(&p, 25, 22);
    assert!(shade[0] < 40 && shade[1] < 40 && shade[2] < 40 && (100..=160).contains(&shade[3]), "{shade:?}");
    assert_eq!(canvas_pixel(&p, 60, 100), [0, 0, 0, 0], "between the objects");
    r.seek(1.7);
    let q = r.picture().expect("from 1.5");
    assert_eq!(q.bounds(), (50, 100, 30, 6));
    r.seek(2.7);
    assert!(r.picture().is_none(), "its end passed (no event of its own clears it)");
}

#[test]
fn dvb_pictures_are_shown_too() {
    let mut r = or_skip!(open());
    r.select(Some(DVB));
    r.seek(0.7);
    let p = r.picture().expect("the DVB picture shown from 0.5");
    assert_eq!(p.stream_index, DVB);
    assert!((p.start - 0.5).abs() < 1e-3, "{}", p.start);
    // DVB pictures are placed on their display (720 × 576 unless a display definition says otherwise): compare by
    // the share of the canvas, as the UI scales them.
    let (x, y, w, h) = p.bounds();
    let frac = |v: u32, of: u32| f64::from(v) / f64::from(of);
    assert!((frac(x, p.canvas.0) - 20.0 / 320.0).abs() < 0.01 && (frac(y, p.canvas.1) - 20.0 / 240.0).abs() < 0.01, "{:?} {:?}", p.bounds(), p.canvas);
    assert!((frac(w, p.canvas.0) - 120.0 / 320.0).abs() < 0.01 && (frac(h, p.canvas.1) - 190.0 / 240.0).abs() < 0.01, "{:?} {:?}", p.bounds(), p.canvas);
    r.seek(2.7);
    assert!(r.picture().is_none(), "the empty page cleared it");
}

#[test]
fn switching_tracks_and_turning_them_off_replaces_the_picture_and_a_text_track_is_refused() {
    let mut r = or_skip!(open());
    r.select(Some(PGS));
    r.seek(0.7);
    assert_eq!(r.picture().map(|p| p.stream_index), Some(PGS));
    // Paused: the switch restarts decoding at the position and the new track's picture there shows.
    r.select(Some(VOBSUB));
    assert!(wait_until(T, || r.picture().is_some_and(|p| p.stream_index == VOBSUB)), "{:?}", r.picture());
    assert_eq!(r.p.tracks().active_image_subtitle, Some(VOBSUB));
    assert!((r.p.position() - 0.7).abs() < 0.05, "the position is kept: {}", r.p.position());
    // Asking for the track shown does nothing.
    let n = r.rec.len();
    r.p.select_image_subtitle_track(Some(VOBSUB));
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(r.rec.len(), n, "{:?}", &r.rec.snapshot()[n..]);
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
    assert!(r.picture().is_none(), "nothing is decoded while off");
    // And back on while playing: the video goes on.
    r.select(Some(PGS));
    r.p.play();
    assert!(wait_until(T, || r.picture().is_some_and(|p| p.stream_index == PGS && (p.start - 1.5).abs() < 1e-6)));
    let presented = r.p.stats().presented;
    assert!(wait_until(T, || r.p.stats().presented > presented + 10), "frames keep coming");
}
