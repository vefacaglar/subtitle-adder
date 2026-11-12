//! Bitmap subtitle tracks shown over the video: PGS (written by `common::sup`), VobSub, DVB and XSUB (FFmpeg's
//! encoders, transcoded from a PGS track) decoded in step with playback, with exact pixels, positions and timings
//! where the format keeps them, the clearing events, seeks, track switches, turning the track off, restarts that keep
//! the position (also racing a seek), the end of playback, a queue of more events than fit on screen, joining a PGS
//! epoch in the middle and a track whose decoder rejects everything.
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
use sa_player::{MediaPlayerBackend, MediaSource, PictureTrackFailure, PlayerEvent, SubtitleKind, SubtitlePicture};

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

/// The DVB track of [`picture_mkv`] (stream 1) with the first byte of every packet changed from the segment sync
/// byte 0x0f to 0xff: dvbsubdec rejects a packet that does not start with it (`AVERROR_INVALIDDATA`), so every packet
/// is rejected. Each packet starts with the display definition segment FFmpeg's encoder always writes
/// (`0f 14 00 01 00 05`), which is where the byte is found.
fn corrupt_dvb() -> Option<PathBuf> {
    let source = path_str(&picture_mkv()?);
    let copy = hashed_clip("dvb-only", "mkv", &["-i", &source, "-map", "0:v", "-map", "0:4", "-c", "copy"])?;
    let mut bytes = std::fs::read(&copy).expect("clip");
    let pattern = [0x0f, 0x14, 0x00, 0x01, 0x00, 0x05];
    let mut patched = 0;
    let mut i = 0;
    while i + pattern.len() <= bytes.len() {
        if bytes[i..i + pattern.len()] == pattern {
            bytes[i] = 0xff;
            patched += 1;
            i += pattern.len();
        } else {
            i += 1;
        }
    }
    assert!(patched >= 4, "the packets' display definition segments were found ({patched})");
    Some(fixture_file("dvb-corrupt", "mkv", &bytes))
}

/// The number of pictures of [`many_xsub_avi`]: more than the 64 the queue used to hold.
const MANY: usize = 90;

/// [`MANY`] XSUB events in AVI (stream 1), one every 0.1 s from 0.0, each a white 4 × 4 square at x = 4 i (each
/// replaced by the next: `-fix_sub_duration` ends it there), over 10 s of video with a keyframe every second.
/// FFmpeg's AVI muxer puts all subtitle chunks at the front of the file: the demuxer reads every event at once
/// and does not deliver them again after a seek.
fn many_xsub_avi() -> Option<PathBuf> {
    let canvas = (400, 240);
    let mut out = Vec::new();
    for i in 0..MANY {
        sup::display_set(&mut out, i as f64 * 0.1, canvas, &[Object::filled(4 * i as u16, 100, 4, 4, 1)], &PALETTE);
    }
    sup::display_set(&mut out, MANY as f64 * 0.1, canvas, &[], &PALETTE);
    let sup_path = path_str(&fixture_file("many-xsub", "sup", &out));
    hashed_clip(
        "many-xsub",
        "avi",
        &[
            "-f", "lavfi", "-i", "testsrc2=size=400x240:rate=30", "-fix_sub_duration", "-i", &sup_path, "-t", "10",
            "-map", "0:v", "-map", "1:s", "-c:v", "mpeg4", "-g", "30", "-c:s", "xsub",
        ],
    )
}

/// A PGS track (stream 1), cleared at 0.0, whose epoch starts at 0.2 with object 0 (white 40 × 20) and the palette; normal sets at
/// 1.2, 1.4 … 2.2 show object 0 again at x = 10, 20 … 60 (no object, no palette: they build on the epoch start);
/// an acquisition point at 3.0 sends it again at (200, 100); an epoch start clears at 3.5. 4 s of video with a
/// keyframe every second: selecting the track or seeking at 1.0 … 2.9 joins the epoch in the middle.
fn pgs_epoch_mkv() -> Option<PathBuf> {
    let canvas = (320, 240);
    let object = Object::filled(0, 0, 40, 20, 1);
    let set = |pts, state, x, y, palette, objects, n| DisplaySet {
        pts,
        segments_pts: None,
        canvas,
        composition_number: n,
        state,
        palette_update: false,
        palette_id: 0,
        compositions: vec![Composition { object: 0, window: 0, x, y }],
        windows: Some(vec![(0, x, y, 40, 20)]),
        palette,
        objects,
    };
    let mut out = Vec::new();
    // FFmpeg starts each input at 0: a first set at 0.0 keeps the times as written.
    sup::display_set(&mut out, 0.0, canvas, &[], &PALETTE);
    sup::write(&mut out, &set(0.2, State::EpochStart, 10, 10, Some((0, 0, &PALETTE[..])), vec![(0, 0, &object)], 0));
    for i in 0..6u16 {
        sup::write(&mut out, &set(1.2 + f64::from(i) * 0.2, State::Normal, 10 * (i + 1), 50, None, vec![], i + 1));
    }
    sup::write(&mut out, &set(3.0, State::AcquisitionPoint, 200, 100, Some((0, 1, &PALETTE[..])), vec![(0, 1, &object)], 7));
    sup::display_set(&mut out, 3.5, canvas, &[], &PALETTE);
    let sup_path = path_str(&fixture_file("pgs-epoch", "sup", &out));
    hashed_clip(
        "pgs-epoch",
        "mkv",
        &[
            "-f", "lavfi", "-i", "testsrc2=size=320x240:rate=30", "-i", &sup_path, "-t", "4", "-map", "0:v", "-map", "1:s",
            "-c:v", "libx264", "-preset", "ultrafast", "-g", "30", "-pix_fmt", "yuv420p", "-c:s", "copy",
        ],
    )
}

/// 12 s of video (a keyframe every second) and a tone (another speed restarts decoding only with audio) with a PGS
/// track (stream 1): cleared at 0.0, a white 40 × 10 at (100, 200)
/// from 1.5, cleared at 9.0, far beyond the demuxer's read-ahead from the picture's start.
fn pgs_sparse_mkv() -> Option<PathBuf> {
    let canvas = (320, 240);
    let mut out = Vec::new();
    sup::display_set(&mut out, 0.0, canvas, &[], &PALETTE);
    sup::display_set(&mut out, 1.5, canvas, &[Object::filled(100, 200, 40, 10, 1)], &PALETTE);
    sup::display_set(&mut out, 9.0, canvas, &[], &PALETTE);
    let sup_path = path_str(&fixture_file("pgs-sparse", "sup", &out));
    hashed_clip(
        "pgs-sparse",
        "mkv",
        &[
            "-f", "lavfi", "-i", "testsrc2=size=320x240:rate=30", "-i", &sup_path,
            "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000", "-t", "12", "-map", "0:v", "-map", "1:s", "-map", "2:a",
            "-c:v", "libx264", "-preset", "ultrafast", "-g", "30", "-pix_fmt", "yuv420p", "-c:s", "copy", "-c:a", "aac",
        ],
    )
}

/// 12 s of video and a tone with one PGS epoch (stream 1): its start at 0.2 sends object 0 (white 40 × 20) and the palette;
/// normal sets at 1.0, 2.0 … 11.0 show it again at x = 10 · i, y 50 (no object, no palette).
fn pgs_long_epoch_mkv() -> Option<PathBuf> {
    let canvas = (320, 240);
    let object = Object::filled(0, 0, 40, 20, 1);
    let set = |pts, state, x, palette, objects, n| DisplaySet {
        pts,
        segments_pts: None,
        canvas,
        composition_number: n,
        state,
        palette_update: false,
        palette_id: 0,
        compositions: vec![Composition { object: 0, window: 0, x, y: 50 }],
        windows: Some(vec![(0, x, 50, 40, 20)]),
        palette,
        objects,
    };
    let mut out = Vec::new();
    sup::display_set(&mut out, 0.0, canvas, &[], &PALETTE);
    sup::write(&mut out, &set(0.2, State::EpochStart, 0, Some((0, 0, &PALETTE[..])), vec![(0, 0, &object)], 0));
    for i in 1..=11u16 {
        sup::write(&mut out, &set(f64::from(i), State::Normal, 10 * i, None, vec![], i));
    }
    let sup_path = path_str(&fixture_file("pgs-long-epoch", "sup", &out));
    hashed_clip(
        "pgs-long-epoch",
        "mkv",
        &[
            "-f", "lavfi", "-i", "testsrc2=size=320x240:rate=30", "-i", &sup_path,
            "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000", "-t", "12", "-map", "0:v", "-map", "1:s", "-map", "2:a",
            "-c:v", "libx264", "-preset", "ultrafast", "-g", "30", "-pix_fmt", "yuv420p", "-c:s", "copy", "-c:a", "aac",
        ],
    )
}

/// A PGS track (stream 1) as Blu-ray discs write them, over 5 s of video: an epoch start at 0.5 shows object 0
/// (white 40 × 10 at (20, 20)); a normal set with no composition clears at 1.5; a normal set at 2.5 sends object
/// 0 again as version 1 (red 60 × 10) with a palette update and shows it at (40, 40); a normal set clears at 3.5.
fn pgs_bd_mkv() -> Option<PathBuf> {
    let canvas = (320, 240);
    let first = Object::filled(0, 0, 40, 10, 1);
    let second = Object::filled(0, 0, 60, 10, 2);
    let set = |pts, state, compositions, windows, palette, objects, n| DisplaySet {
        pts,
        segments_pts: None,
        canvas,
        composition_number: n,
        state,
        palette_update: false,
        palette_id: 0,
        compositions,
        windows,
        palette,
        objects,
    };
    let mut out = Vec::new();
    sup::display_set(&mut out, 0.0, canvas, &[], &PALETTE);
    sup::write(
        &mut out,
        &set(0.5, State::EpochStart, vec![Composition { object: 0, window: 0, x: 20, y: 20 }], Some(vec![(0, 20, 20, 40, 10)]), Some((0, 0, &PALETTE[..])), vec![(0, 0, &first)], 1),
    );
    sup::write(&mut out, &set(1.5, State::Normal, vec![], Some(vec![(0, 20, 20, 40, 10)]), None, vec![], 2));
    sup::write(
        &mut out,
        &set(2.5, State::Normal, vec![Composition { object: 0, window: 0, x: 40, y: 40 }], Some(vec![(0, 40, 40, 60, 10)]), Some((0, 1, &PALETTE[..])), vec![(0, 1, &second)], 3),
    );
    sup::write(&mut out, &set(3.5, State::Normal, vec![], Some(vec![(0, 40, 40, 60, 10)]), None, vec![], 4));
    let sup_path = path_str(&fixture_file("pgs-bd", "sup", &out));
    hashed_clip(
        "pgs-bd",
        "mkv",
        &[
            "-f", "lavfi", "-i", "testsrc2=size=320x240:rate=30", "-i", &sup_path, "-t", "5", "-map", "0:v", "-map", "1:s",
            "-c:v", "libx264", "-preset", "ultrafast", "-g", "30", "-pix_fmt", "yuv420p", "-c:s", "copy",
        ],
    )
}

/// The number of pictures of [`big_xsub_avi`].
const BIG: usize = 120;

/// The start of picture `i` of [`big_xsub_avi`].
fn big_start(i: usize) -> f64 {
    1.0 + 0.24 * i as f64
}

/// [`BIG`] XSUB events in AVI (stream 1) over 31 s of 640 × 480 video: picture `i` is a white (400 + i) × 400 block at
/// (20, 40) from [`big_start`] for 0.12 s. Decoded, they hold 77 MB of RGBA, more than the timeline's 64 MiB bound,
/// and FFmpeg's AVI muxer puts every one of their chunks with the first seconds of video.
fn big_xsub_avi() -> Option<PathBuf> {
    let canvas = (640, 480);
    let mut out = Vec::new();
    sup::display_set(&mut out, 0.0, canvas, &[], &PALETTE);
    for i in 0..BIG {
        sup::display_set(&mut out, big_start(i), canvas, &[Object::filled(20, 40, 400 + i, 400, 1)], &PALETTE);
        sup::display_set(&mut out, big_start(i) + 0.12, canvas, &[], &PALETTE);
    }
    let sup_path = path_str(&fixture_file("big-xsub", "sup", &out));
    hashed_clip(
        "big-xsub",
        "avi",
        &[
            "-f", "lavfi", "-i", "testsrc2=size=640x480:rate=30", "-fix_sub_duration", "-i", &sup_path, "-t", "31",
            "-map", "0:v", "-map", "1:s", "-c:v", "mpeg4", "-g", "30", "-c:s", "xsub",
        ],
    )
}

/// The DVB track of [`picture_mkv`] (stream 1) whose packets from the second on (from 1.5: dvbsub writes each
/// set after the first as a clearing packet and a showing one) are damaged like [`corrupt_dvb`]'s: only the set at
/// 0.5 decodes. A cluster per keyframe, so that a seek to 1.6 reads from 1.0 on.
fn late_corrupt_dvb() -> Option<PathBuf> {
    let source = path_str(&picture_mkv()?);
    let copy = hashed_clip("dvb-only-clustered", "mkv", &["-i", &source, "-map", "0:v", "-map", "0:4", "-c", "copy", "-cluster_time_limit", "900"])?;
    let mut bytes = std::fs::read(&copy).expect("clip");
    let pattern = [0x0f, 0x14, 0x00, 0x01, 0x00, 0x05];
    let at: Vec<usize> = (0..bytes.len().saturating_sub(pattern.len())).filter(|&i| bytes[i..i + pattern.len()] == pattern).collect();
    assert_eq!(at.len(), 6, "one display definition per packet");
    for &i in &at[1..] {
        bytes[i] = 0xff;
    }
    Some(fixture_file("dvb-late-corrupt", "mkv", &bytes))
}

/// [`many_xsub_avi`] with the time codes of four packets (those of 8.0 to 8.3 s) damaged: the other 86 are fine.
fn xsub_with_four_damaged_chunks() -> Option<PathBuf> {
    let mut bytes = std::fs::read(many_xsub_avi()?).expect("clip");
    let mut patched = 0;
    for i in 0..bytes.len().saturating_sub(27) {
        if bytes[i] == b'[' && bytes[i + 13] == b'-' && bytes[i + 26] == b']' && bytes[i + 1..i + 9] == *b"00:00:08" {
            if bytes[i + 10..i + 12] < b"40"[..] {
                bytes[i] = b'x';
                patched += 1;
            }
        }
    }
    assert_eq!(patched, 4, "the time codes of 8.0, 8.1, 8.2 and 8.3");
    Some(fixture_file("xsub-four-damaged", "avi", &bytes))
}

/// 6 s of video and a tone with one PGS epoch (stream 1): its start at 0.5 sends object 0 version 0 (white 40 × 10)
/// and shows it at (20, 20); a normal set at 1.1 shows it again at (60, 20); a normal set at 2.0 sends object 0
/// version 1 (red 40 × 10) and shows it at (100, 20); cleared at 3.0.
fn pgs_redefined_object_mkv() -> Option<PathBuf> {
    let canvas = (320, 240);
    let white = Object::filled(0, 0, 40, 10, 1);
    let red = Object::filled(0, 0, 40, 10, 2);
    let set = |pts, state, x, palette, objects, n| DisplaySet {
        pts,
        segments_pts: None,
        canvas,
        composition_number: n,
        state,
        palette_update: false,
        palette_id: 0,
        compositions: vec![Composition { object: 0, window: 0, x, y: 20 }],
        windows: Some(vec![(0, x, 20, 40, 10)]),
        palette,
        objects,
    };
    let mut out = Vec::new();
    sup::display_set(&mut out, 0.0, canvas, &[], &PALETTE);
    sup::write(&mut out, &set(0.5, State::EpochStart, 20, Some((0, 0, &PALETTE[..])), vec![(0, 0, &white)], 1));
    sup::write(&mut out, &set(1.1, State::Normal, 60, None, vec![], 2));
    sup::write(&mut out, &set(2.0, State::Normal, 100, None, vec![(0, 1, &red)], 3));
    sup::display_set(&mut out, 3.0, canvas, &[], &PALETTE);
    let sup_path = path_str(&fixture_file("pgs-redefined", "sup", &out));
    hashed_clip(
        "pgs-redefined",
        "mkv",
        &[
            "-f", "lavfi", "-i", "testsrc2=size=320x240:rate=30", "-i", &sup_path,
            "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000", "-t", "6", "-map", "0:v", "-map", "1:s", "-map", "2:a",
            "-c:v", "libx264", "-preset", "ultrafast", "-g", "30", "-pix_fmt", "yuv420p", "-c:s", "copy", "-c:a", "aac",
        ],
    )
}

/// [`many_xsub_avi`] with the `[` of every packet's time code changed: no packet says when it is shown.
fn xsub_without_time_codes() -> Option<PathBuf> {
    let mut bytes = std::fs::read(many_xsub_avi()?).expect("clip");
    let mut patched = 0;
    for i in 0..bytes.len().saturating_sub(27) {
        if bytes[i] == b'[' && bytes[i + 13] == b'-' && bytes[i + 26] == b']' {
            bytes[i] = b'x';
            patched += 1;
        }
    }
    assert_eq!(patched, MANY, "every packet's time code");
    Some(fixture_file("xsub-no-time-code", "avi", &bytes))
}

/// A PGS track (stream 1) on a 3840 × 2160 canvas whose epoch starts at 0.5, 1.0, 1.5 and 2.0 each show two
/// 3000 × 1500 objects: 9 M pixels an event, more than the player keeps. 3 s of video; nothing else decodes (a clearing
/// set would count as a decoded event and keep the track).
fn pgs_too_large_mkv() -> Option<PathBuf> {
    let canvas = (3840, 2160);
    let mut out = Vec::new();
    for i in 1..=4 {
        sup::display_set(&mut out, 0.5 * f64::from(i), canvas, &[Object::filled(0, 0, 3000, 1500, 1), Object::filled(0, 1600, 3000, 1500, 2)], &PALETTE);
    }
    let sup_path = path_str(&fixture_file("pgs-too-large", "sup", &out));
    hashed_clip(
        "pgs-too-large",
        "mkv",
        &[
            "-f", "lavfi", "-i", "testsrc2=size=320x240:rate=30", "-i", &sup_path, "-t", "3", "-map", "0:v", "-map", "1:s",
            "-c:v", "libx264", "-preset", "ultrafast", "-g", "30", "-pix_fmt", "yuv420p", "-c:s", "copy",
        ],
    )
}

/// [`picture_mkv`]'s video, turned a quarter round by its display matrix, with its PGS track (stream 1).
fn rotated_pgs_mkv() -> Option<PathBuf> {
    let source = path_str(&picture_mkv()?);
    hashed_clip("pictures-rotated", "mkv", &["-display_rotation:v:0", "90", "-i", &source, "-map", "0:v", "-map", "0:2", "-c", "copy"])
}

/// 6 s of video at one frame a second (each a keyframe) with a PGS track (stream 1): a white 40 × 10 at (100, 200)
/// from 0.5, cleared at 1.7; a red 8 × 2 at (10, 10) from 2.3, cleared at 3.4: every change falls between frames.
fn one_fps_mkv() -> Option<PathBuf> {
    let canvas = (320, 240);
    let mut out = Vec::new();
    sup::display_set(&mut out, 0.0, canvas, &[], &PALETTE);
    sup::display_set(&mut out, 0.5, canvas, &[Object::filled(100, 200, 40, 10, 1)], &PALETTE);
    sup::display_set(&mut out, 1.7, canvas, &[], &PALETTE);
    sup::display_set(&mut out, 2.3, canvas, &[Object::filled(10, 10, 8, 2, 2)], &PALETTE);
    sup::display_set(&mut out, 3.4, canvas, &[], &PALETTE);
    let sup_path = path_str(&fixture_file("one-fps", "sup", &out));
    hashed_clip(
        "one-fps",
        "mkv",
        &[
            "-f", "lavfi", "-i", "testsrc2=size=320x240:rate=1", "-i", &sup_path, "-t", "6", "-map", "0:v", "-map", "1:s",
            "-c:v", "libx264", "-preset", "ultrafast", "-g", "1", "-pix_fmt", "yuv420p", "-c:s", "copy",
        ],
    )
}

/// [`picture_mkv`] with a second audio track (a lower tone): video 0, audio 1 and 2, the PGS track 3.
fn two_audio_mkv() -> Option<PathBuf> {
    let source = path_str(&picture_mkv()?);
    hashed_clip(
        "pictures-two-audio",
        "mkv",
        &[
            "-i", &source, "-f", "lavfi", "-i", "sine=frequency=220:sample_rate=48000", "-t", "4",
            "-map", "0:v", "-map", "0:a", "-map", "1:a", "-map", "0:2", "-c", "copy", "-c:a:1", "aac",
        ],
    )
}

macro_rules! or_skip {
    ($e:expr) => {{
        common::announce_ffmpeg();
        match $e {
            Some(p) => p,
            None => {
                common::skipped(&format!("{} line {}", file!(), line!()));
                return;
            }
        }
    }};
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
    let sets = or_skip!(pgs_sets_mkv());
    // The premise, checked: Matroska keeps a display set in one block at its PCS time (FFmpeg's muxer merges the
    // segments, `pgs_frame_merge`), so in MKV the set written with its other segments at 0.6 is a single packet at
    // 0.5. The start from the PCS of a set whose segments arrive in packets of their own (.sup, Blu-ray .m2ts) is
    // proved on a raw .sup by `pictures::tests::a_display_set_split_over_packets_starts_at_its_composition`.
    let mut ictx = ffmpeg_next::format::input(&sets).expect("mkv");
    let tb = ictx.stream(1).expect("pgs").time_base();
    let times: Vec<f64> = ictx.packets().filter(|(s, _)| s.index() == 1).filter_map(|(_, p)| p.pts()).map(|t| t as f64 * f64::from(tb)).collect();
    drop(ictx);
    assert!(times.iter().any(|t| (t - 0.5).abs() < 0.002) && !times.iter().any(|t| (t - 0.6).abs() < 0.002), "{times:?}");
    let mut r = open_clip(&sets);
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
    let Some(PlayerEvent::SubtitleTrackFailed { reason, generation, .. }) = failed else { panic!("no failure: {:?}", r.rec.snapshot()) };
    assert_eq!(generation, r.g);
    assert!(matches!(&reason, PictureTrackFailure::Ffmpeg(text) if !text.is_empty()), "{reason:?}");
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
fn another_speed_restarts_decoding_in_place_and_keeps_the_very_picture_on_screen() {
    let mut r = or_skip!(open());
    r.select(Some(PGS));
    // At 2.2 the picture from 1.5 is on screen, and the restart reads again from the keyframe at 2.0: after the
    // event's packet, which the demuxer does not deliver again.
    let p = r.seek_to_picture(2.2, 1.5);
    r.settle();
    let restarts = r.p.stats().decoded;
    let from = r.rec.len();
    // Another speed restarts decoding at the position (the queued audio was resampled for the old one): the
    // caption on screen must not blink off, nor be replaced, while the stream is read again.
    r.p.set_rate(1.5);
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        assert_eq!(r.picture().map(|q| q.id), Some(p.id), "the caption went away or changed during the restart");
        let _ = r.p.latest_frame();
        std::thread::sleep(Duration::from_millis(2));
    }
    r.settle();
    assert!(r.p.stats().decoded > restarts, "decoding did restart");
    assert!((r.p.position() - 2.2).abs() < 0.05, "in place: {}", r.p.position());
    assert_eq!(r.picture_changes(from), 0, "no change was announced");
    // The same where the restart reads the event again (0.7: after the keyframe at 0.0, the event from 0.5).
    let q = r.seek_to_picture(0.7, 0.5);
    r.settle();
    let from = r.rec.len();
    r.p.set_rate(1.0);
    r.settle();
    assert_eq!(r.picture().map(|x| x.id), Some(q.id), "the event decoded again is the one on screen");
    assert_eq!(r.picture_changes(from), 0);
    // A seek still takes it off.
    r.seek_to_nothing(2.7);
}

/// Watches the picture for `wait` after the frame at `t` is on screen: every picture shown must be due at the
/// position by its own times (`due`: start and end of the clip's events), never one of before the seek.
fn watch_after_seek(r: &mut Rig, t: f64, wait: Duration, due: &[(f64, f64)]) {
    let p = &mut r.p;
    let mut last = None;
    assert!(
        wait_until(T, || {
            if let Some(f) = p.latest_frame() {
                last = Some(f.pts);
            }
            last.is_some_and(|pts| (pts - t).abs() < 0.05)
        }),
        "no frame at {t} (last {last:?})"
    );
    // The picture follows the presented frames (and the clock only while paused or between sparse frames): it is
    // judged against the newest frame seen before and after reading it, with one frame of slack each way.
    let deadline = Instant::now() + wait;
    let mut shown = last.unwrap_or(t);
    while Instant::now() < deadline {
        let before = shown;
        let q = r.picture();
        if let Some(f) = r.p.latest_frame() {
            shown = f.pts;
        }
        if let Some(q) = q {
            let due_at = |at: f64| due.iter().any(|&(s, e)| (q.start - s).abs() < 1e-3 && at >= s - 0.04 && at < e + 0.04);
            assert!(due_at(before) || due_at(shown), "picture from {} shown with the frames at {before}..{shown} after the seek to {t}", q.start);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Plays `r` from where it stands until playback ends (or `T` passes) and records every picture change with the pts
/// of the newest frame presented when it was seen.
fn play_and_record(r: &mut Rig) -> Vec<(f64, Option<Arc<SubtitlePicture>>)> {
    let from = r.rec.len();
    r.p.play();
    let mut seen: Vec<(f64, Option<Arc<SubtitlePicture>>)> = Vec::new();
    let mut at = r.p.position();
    let deadline = Instant::now() + T;
    while !r.p.is_ended() && Instant::now() < deadline {
        if let Some(f) = r.p.latest_frame() {
            at = f.pts;
        }
        let now = r.picture();
        if seen.last().is_none_or(|l| l.1.as_ref().map(|p| p.id) != now.as_ref().map(|p| p.id)) {
            seen.push((at, now));
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(r.rec.wait_for(from, T, is_ended(r.g)).is_some(), "played to the end");
    seen
}

/// The starts (to 0.01 s) of the pictures in a recording, `None` for nothing on screen.
fn starts(seen: &[(f64, Option<Arc<SubtitlePicture>>)]) -> Vec<Option<f64>> {
    seen.iter().map(|(_, p)| p.as_ref().map(|p| (p.start * 100.0).round() / 100.0)).collect()
}

/// The windows of the main clip's PGS events.
const PGS_DUE: [(f64, f64); 3] = [(0.5, 1.5), (1.5, 2.5), (3.2, 3.6)];

#[test]
fn a_restart_right_after_a_seek_never_brings_back_the_picture_of_before_it() {
    let mut r = or_skip!(open());
    r.select(Some(PGS));
    // Paused, on the picture from 0.5 (nothing delivered is known past it until the demuxer read on).
    r.seek_to_picture(0.7, 0.5);
    r.settle();
    // A seek into the cleared stretch, and another speed before its frame is on screen.
    r.p.seek(2.7);
    r.p.set_rate(1.5);
    watch_after_seek(&mut r, 2.7, Duration::from_millis(800), &PGS_DUE);
    assert!(r.picture().is_none(), "{:?}", r.picture());
    // Several restarts in a row while paused.
    r.seek_to_picture(1.7, 1.5);
    r.p.seek(2.9);
    r.p.set_rate(1.0);
    r.p.set_rate(1.25);
    watch_after_seek(&mut r, 2.9, Duration::from_millis(800), &PGS_DUE);
    assert!(r.picture().is_none(), "{:?}", r.picture());
    // Playing: a seek and another speed at once; the next picture (3.2) still comes when it is due.
    r.seek_to_picture(1.6, 1.5);
    r.p.play();
    r.p.seek(2.6);
    r.p.set_rate(1.0);
    watch_after_seek(&mut r, 2.6, Duration::from_millis(1200), &PGS_DUE);
}

#[test]
fn an_audio_switch_right_after_a_seek_never_brings_back_the_picture_of_before_it() {
    let mut r = open_clip(&or_skip!(two_audio_mkv()));
    r.select(Some(3));
    r.seek_to_picture(0.7, 0.5);
    r.settle();
    r.p.seek(2.7);
    r.p.select_audio_track(2);
    watch_after_seek(&mut r, 2.7, Duration::from_millis(800), &PGS_DUE);
    assert!(r.picture().is_none(), "{:?}", r.picture());
    assert!(wait_until(T, || r.p.tracks().active_audio == Some(2)), "switched");
    // Back, from a picture, to a time that has one of its own.
    r.seek_to_picture(0.7, 0.5);
    r.p.seek(1.7);
    r.p.select_audio_track(1);
    watch_after_seek(&mut r, 1.7, Duration::from_millis(800), &PGS_DUE);
    assert!(r.wait_picture(|p| (p.start - 1.5).abs() < 1e-3).is_some(), "{:?}", r.picture());
}

#[test]
fn the_software_fall_back_of_a_failing_hardware_decoder_right_after_a_seek_shows_nothing_of_before_it() {
    let path = or_skip!(picture_mkv());
    let rec = Recorder::default();
    let mut p = FfmpegPlayer::with_options(PlayerOptions { audio_output: false, ..PlayerOptions::default() });
    p.use_audio_tap(Some(AudioTap::new(48_000, 2)));
    p.simulate_hardware_failure(true);
    rec.attach(&mut p);
    let g = p.load(Some(MediaSource::File(path)), false);
    // The fall-back restarts decoding when the hardware decoder fails its first packets: select and seek at once,
    // so that it races them (whether it lands before or after the seek is up to the machine).
    p.select_image_subtitle_track(Some(PGS));
    assert!(rec.wait_for(0, T, is_opened(g)).is_some(), "{:?}", rec.snapshot());
    p.seek(0.7);
    p.seek(2.7);
    let mut r = Rig { p, rec, g };
    watch_after_seek(&mut r, 2.7, Duration::from_millis(800), &PGS_DUE);
    assert!(r.picture().is_none(), "{:?}", r.picture());
    let q = r.seek_to_picture(1.7, 1.5);
    assert_eq!(q.stream_index, PGS);
}

#[test]
fn a_queue_of_more_events_than_used_to_fit_keeps_them_all_across_seeks() {
    let path = or_skip!(many_xsub_avi());
    // The premise: AVI interleaves the subtitle chunks by their number (0, 1, 2 … in a 1/30 time base), so all of
    // them sit with the video of the first three seconds, though the events run to 8.9 s.
    let mut ictx = ffmpeg_next::format::input(&path).expect("avi");
    let mut seen = 0;
    let mut late_video_before_last = 0;
    for (s, packet) in ictx.packets() {
        if s.index() == 1 {
            seen += 1;
        } else if seen < MANY && packet.pts().is_some_and(|t| t >= 105) {
            late_video_before_last += 1;
        }
    }
    drop(ictx);
    assert_eq!((seen, late_video_before_last), (MANY, 0), "no video after 3.5 s comes before the last subtitle chunk");
    let mut r = open_clip(&path);
    r.select(Some(1));
    r.seek_to_picture(0.0, 0.0);
    // Played past the 64th event: the later ones were kept.
    r.p.set_rate(2.0);
    r.p.play();
    let late = r.wait_picture(|p| p.start > 7.0).unwrap_or_else(|| panic!("no picture after the 64th: {:?}", r.picture()));
    r.p.pause();
    let x = (late.start * 10.0).round() as u32 * 4;
    assert_eq!(late.bounds(), (x, 100, 4, 4), "the event of its own time");
    // Seeks back and forward: the demuxer does not read these events again, the queue still has them.
    for (t, start) in [(2.05, 2.0), (6.55, 6.5), (0.35, 0.3), (8.85, 8.8)] {
        let p = r.seek_to_picture(t, start);
        let x = (start * 10.0).round() as u32 * 4;
        assert_eq!(p.bounds(), (x, 100, 4, 4), "at {t}");
    }
    r.seek_to_nothing(9.5);
    let s = r.p.stats();
    assert_eq!((s.picture_failures, s.picture_overflow), (0, 0), "{s:?}");
}

#[test]
fn joining_a_pgs_epoch_in_the_middle_never_gives_the_track_up() {
    let mut r = open_clip(&or_skip!(pgs_epoch_mkv()));
    // Selected at 1.3: the restart reads from the keyframe at 1.0, so the six normal sets come without the epoch
    // start they build on (no object, no palette in the decoder).
    r.seek(1.3);
    r.select(Some(1));
    r.settle();
    let joined = r.p.stats();
    // The premise: the sets were delivered and decoded, without their epoch (FFmpeg 8.1's pgssubdec rejects none
    // of them; were it to, they would not count towards giving up either, being normal sets).
    assert!(joined.picture_events >= 1, "the normal sets reached the decoder: {joined:?}");
    assert!(r.picture().is_none(), "decoded without their object they show nothing: {:?}", r.picture());
    assert_eq!(r.p.tracks().active_image_subtitle, Some(1), "not given up");
    // The same by a seek, several times (each seek starts the give-up count again).
    for t in [1.1, 1.5, 2.1, 1.3] {
        r.seek(t);
        r.settle();
        assert_eq!(r.p.tracks().active_image_subtitle, Some(1), "not given up after the seek to {t}");
    }
    let from = r.rec.len();
    r.p.play();
    // The acquisition point brings the picture back.
    let a = r.wait_picture(|p| (p.start - 3.0).abs() < 1e-3).unwrap_or_else(|| panic!("{:?}", r.rec.snapshot()));
    assert_eq!(a.bounds(), (200, 100, 40, 20));
    assert!(!r.rec.snapshot()[from..].iter().any(|e| matches!(e, PlayerEvent::SubtitleTrackFailed { .. })));
    // From the start, the whole epoch is decoded: every normal set shows the object where it says.
    let mut r = open_clip(&or_skip!(pgs_epoch_mkv()));
    r.select(Some(1));
    r.seek_to_picture(0.3, 0.2);
    let seen = play_and_record(&mut r);
    let shown: Vec<_> = seen.iter().filter_map(|(_, p)| p.clone()).collect();
    let want: Vec<f64> = [0.2, 1.2, 1.4, 1.6, 1.8, 2.0, 2.2, 3.0].to_vec();
    assert_eq!(shown.iter().map(|p| (p.start * 10.0).round() / 10.0).collect::<Vec<_>>(), want, "{seen:?}");
    for (i, p) in shown[1..7].iter().enumerate() {
        assert_eq!(p.bounds(), (10 * (i as u32 + 1), 50, 40, 20));
    }
    assert_eq!(r.p.stats().picture_failures, 0);
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
    // Asking for the track shown changes nothing, but is answered all the same (the asker learns the outcome), with
    // no restart and no failure.
    r.settle();
    let n = r.rec.len();
    let decoded = r.p.stats().decoded;
    r.p.select_image_subtitle_track(Some(VOBSUB));
    assert!(r.rec.wait_for(n, T, |e| matches!(e, PlayerEvent::TracksChanged { .. })).is_some());
    r.settle();
    let after = &r.rec.snapshot()[n..];
    assert!(!after.iter().any(|e| matches!(e, PlayerEvent::SubtitleTrackFailed { .. } | PlayerEvent::SubtitlePictureChanged { .. })), "{after:?}");
    assert_eq!(r.p.stats().decoded, decoded, "no restart");
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

/// Holds the next seek between moving the bitmap subtitle timeline to its serial and moving the clock until the
/// presenter has asked for a picture with a time read before the seek (paused, it asks with the clock every 10 ms):
/// the race the serial check guards against, forced. The returned flag says it happened.
#[cfg(debug_assertions)]
fn hold_the_next_seek_for_a_stale_ask(r: &Rig) -> Arc<std::sync::atomic::AtomicBool> {
    use std::sync::atomic::{AtomicBool, Ordering};
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let tx = std::sync::Mutex::new(tx);
    r.p.set_picture_ask_hook(Some(Box::new(move |of, serial| {
        if of < serial {
            let _ = tx.lock().unwrap().send(());
        }
    })));
    let seen = Arc::new(AtomicBool::new(false));
    let flag = seen.clone();
    let rx = std::sync::Mutex::new(rx);
    r.p.set_seek_gate(Some(Box::new(move || {
        let rx = rx.lock().unwrap();
        while rx.try_recv().is_ok() {}
        if rx.recv_timeout(Duration::from_secs(5)).is_ok() {
            flag.store(true, Ordering::SeqCst);
        }
    })));
    seen
}

#[cfg(debug_assertions)]
#[test]
fn a_clock_reading_from_before_a_seek_never_pins_the_new_position() {
    use std::sync::atomic::Ordering;
    let mut r = or_skip!(open());
    r.select(Some(PGS));
    // Paused at 2.7 (cleared at 2.5), the queue knowing the whole clip.
    r.seek_to_nothing(2.7);
    r.settle();
    // The seek to 1.7 waits, between moving the timeline and moving the clock, until the presenter has read the
    // clock, still at 2.7, and asked with it.
    let raced = hold_the_next_seek_for_a_stale_ask(&r);
    r.p.seek(1.7);
    r.p.set_seek_gate(None);
    r.p.set_picture_ask_hook(None);
    assert!(raced.load(Ordering::SeqCst), "the presenter asked with a time of before the seek inside the window");
    watch_after_seek(&mut r, 1.7, Duration::from_millis(300), &PGS_DUE);
    // Read at 2.7 for the new serial, the time would stay there: nothing would show until playback passed 2.7.
    assert!(r.wait_picture(|p| (p.start - 1.5).abs() < 1e-3).is_some(), "{:?}", r.picture());
    // The same with the queue cleared (back before what it knows).
    r.seek_to_nothing(2.7);
    let raced = hold_the_next_seek_for_a_stale_ask(&r);
    r.p.seek(0.7);
    r.p.set_seek_gate(None);
    r.p.set_picture_ask_hook(None);
    assert!(raced.load(Ordering::SeqCst));
    assert!(r.wait_picture(|p| (p.start - 0.5).abs() < 1e-3).is_some(), "{:?}", r.picture());
}

#[test]
fn every_caption_of_a_front_loaded_xsub_stream_larger_than_the_byte_bound_shows_before_and_after_any_seek() {
    let path = or_skip!(big_xsub_avi());
    // The premise: every subtitle chunk comes before the video of 5 s, though the events run to 29.7 s.
    let mut ictx = ffmpeg_next::format::input(&path).expect("avi");
    let mut subs = 0;
    let mut late_video_first = 0;
    for (s, packet) in ictx.packets() {
        if s.index() == 1 {
            subs += 1;
        } else if subs < BIG && packet.pts().is_some_and(|t| t >= 150) {
            late_video_first += 1;
        }
    }
    drop(ictx);
    assert_eq!((subs, late_video_first), (BIG, 0));
    // Which picture by its start (xsubenc rounds a width up to even, so the width only checks it).
    let width = |p: &SubtitlePicture| {
        let i = ((p.start - 1.0) / 0.24).round() as usize;
        assert!((p.start - big_start(i)).abs() < 2e-3 && p.bounds().2 as usize == (400 + i).next_multiple_of(2), "{p:?}");
        i
    };
    let mut r = open_clip(&path);
    r.select(Some(1));
    // Into the end region and played to the end: the last captions, decoded from the store.
    r.seek_to_nothing(big_start(116) - 0.05);
    let seen = play_and_record(&mut r);
    let late: Vec<usize> = seen.iter().filter_map(|(_, p)| p.as_ref().map(|p| width(p))).collect();
    assert_eq!(late, vec![116, 117, 118, 119], "{:?}", starts(&seen));
    // Back to before the first event, then to it, past the last start, into the middle, and back to the start.
    r.seek_to_nothing(0.5);
    for (t, i) in [(1.05, Some(0)), (29.9, None), (15.0, Some(58)), (big_start(3) + 0.05, Some(3)), (29.6, Some(119)), (1.05, Some(0))] {
        match i {
            Some(i) => {
                let p = r.seek_to_picture(t, big_start(i));
                assert_eq!(width(&p), i, "at {t}");
            }
            None => r.seek_to_nothing(t),
        }
    }
    // Played through the middle: every caption on the way, none standing in for another. Counted by the player
    // (`pictures_shown`), not by this thread's polling, which a loaded machine starves: the pictures it does see only
    // have to come in order.
    r.seek_to_nothing(big_start(40) - 0.1);
    let shown_before = r.p.stats().pictures_shown;
    r.p.play();
    let mut widths = Vec::new();
    let deadline = Instant::now() + T;
    while Instant::now() < deadline && r.p.position() < big_start(52) {
        if let Some(p) = r.picture()
            && widths.last() != Some(&width(&p))
        {
            widths.push(width(&p));
        }
        let _ = r.p.latest_frame();
        std::thread::sleep(Duration::from_millis(2));
    }
    r.p.pause();
    r.settle();
    let at = r.p.position();
    let due = |t: f64| (40..BIG).filter(|&i| big_start(i) <= t).count() as u64;
    let shown = r.p.stats().pictures_shown - shown_before;
    assert!((due(at - 0.05)..=due(at + 0.05)).contains(&shown) && shown >= 12, "{shown} pictures shown up to {at}");
    assert!(widths.windows(2).all(|w| w[1] > w[0]) && widths.iter().all(|w| (40..=52).contains(w)), "{widths:?}");
    let s = r.p.stats();
    assert_eq!((s.picture_failures, s.picture_overflow, s.picture_store_overflow), (0, 0, 0), "{s:?}");
}

#[test]
fn a_restart_in_place_past_what_the_queue_knows_carries_the_very_picture_over() {
    let mut r = open_clip(&or_skip!(pgs_sparse_mkv()));
    r.select(Some(1));
    // On the picture from 1.5; its clearing set at 9.0 lies far beyond the read-ahead, so the queue knows the screen
    // only up to 1.5.
    let p = r.seek_to_picture(1.6, 1.5);
    r.settle();
    let from = r.rec.len();
    let decoded = r.p.stats().decoded;
    r.p.set_rate(1.5);
    let deadline = Instant::now() + Duration::from_secs(1);
    while Instant::now() < deadline {
        assert_eq!(r.picture().map(|q| q.id), Some(p.id), "the caption went away or changed during the restart");
        let _ = r.p.latest_frame();
        std::thread::sleep(Duration::from_millis(2));
    }
    r.settle();
    assert!(r.p.stats().decoded > decoded, "decoding did restart");
    assert!(r.p.stats().pictures_carried >= 1, "the carry-over ran: {:?}", r.p.stats());
    assert_eq!(r.picture_changes(from), 0);
    // A seek past its end, and another speed before the seek's first frame: the carried picture does not come back.
    r.p.seek(10.0);
    r.p.set_rate(1.0);
    watch_after_seek(&mut r, 10.0, Duration::from_millis(500), &[]);
    // Back on the picture, carried over another restart, then played (fast): the same picture until 9.0, then
    // nothing.
    let p = r.seek_to_picture(1.6, 1.5);
    r.p.set_rate(4.0);
    let seen = play_and_record(&mut r);
    assert_eq!(starts(&seen), vec![Some(1.5), None], "{seen:?}");
    assert_eq!(seen[0].1.as_ref().map(|q| q.id), Some(p.id));
    // Judged by the newest frame this thread had taken when it saw the change (the picture changes as the frame at
    // 9.0 is published, which this thread may take a few frames later at 4x): never before its time, and gone by the
    // end. The exact moment is covered at 1x by `playing_shows_each_picture_when_it_is_due...`.
    assert!((9.0 - 0.25..12.0).contains(&seen[1].0), "went at {}", seen[1].0);
}

#[test]
fn a_restart_in_place_keeps_the_pgs_epoch_so_later_normal_sets_still_show() {
    let mut r = open_clip(&or_skip!(pgs_long_epoch_mkv()));
    r.select(Some(1));
    // Played into the epoch, then another speed at 1.3: the restart reads from the keyframe at 1.0, after the epoch
    // start at 0.2, which it does not read again.
    r.seek_to_picture(0.3, 0.2);
    r.p.play();
    assert!(r.wait_picture(|p| (p.start - 1.0).abs() < 1e-3).is_some());
    r.p.pause();
    r.settle();
    let decoded = r.p.stats().decoded;
    r.p.set_rate(1.5);
    r.settle();
    assert!(r.p.stats().decoded > decoded, "decoding did restart");
    // The sets after the read-ahead (6.0 on) still find their object. (Settled by a mutation: with FFmpeg 8.1 this
    // also holds when the restart flushes the decoder, since pgssubdec keeps its objects over a flush; what the
    // player keeps, its knowledge of the epoch, is checked by
    // `pictures::tests::a_restart_in_place_keeps_the_pgs_epoch_and_a_seek_forgets_it`.)
    r.p.set_rate(4.0);
    r.p.play();
    let p = r.wait_picture(|p| (p.start - 8.0).abs() < 1e-3).unwrap_or_else(|| panic!("no set at 8.0: {:?}", r.picture()));
    assert_eq!(p.bounds(), (80, 50, 40, 20));
}

#[test]
fn blu_ray_style_clears_and_object_updates_in_one_epoch() {
    let path = or_skip!(pgs_bd_mkv());
    let mut r = open_clip(&path);
    r.select(Some(1));
    r.seek_to_nothing(0.0);
    let seen = play_and_record(&mut r);
    assert_eq!(starts(&seen), vec![None, Some(0.5), None, Some(2.5), None], "{seen:?}");
    let shown: Vec<_> = seen.iter().filter_map(|(_, p)| p.clone()).collect();
    assert_eq!(shown[0].bounds(), (20, 20, 40, 10));
    assert_eq!(shown[1].bounds(), (40, 40, 60, 10), "the object's new version");
    assert!(is_red(canvas_pixel(&shown[1], 45, 45)));
    // Seeks back inside the epoch after playing it: the complete decodes stay, whatever the re-read sets decode to.
    let again = r.seek_to_picture(2.7, 2.5);
    assert_eq!(again.id, shown[1].id);
    let again = r.seek_to_picture(0.7, 0.5);
    assert_eq!(again.id, shown[0].id);
    r.seek_to_nothing(1.7);
    // Cold, straight into the second show: its set sends its object and palette, so it shows on its own.
    let mut r = open_clip(&path);
    r.seek(2.7);
    r.select(Some(1));
    let p = r.wait_picture(|p| (p.start - 2.5).abs() < 1e-3).unwrap_or_else(|| panic!("{:?}", r.picture()));
    assert_eq!(p.bounds(), (40, 40, 60, 10));
}

#[test]
fn a_track_that_decoded_is_given_up_only_after_a_seek_into_its_damaged_part() {
    let path = or_skip!(late_corrupt_dvb());
    // Played through: it decoded its first events, so the damaged rest never gives it up.
    let mut r = open_clip(&path);
    r.select(Some(1));
    r.seek_to_picture(0.7, 0.5);
    let from = r.rec.len();
    play_and_record(&mut r);
    assert!(!r.rec.snapshot()[from..].iter().any(|e| matches!(e, PlayerEvent::SubtitleTrackFailed { .. })));
    assert_eq!(r.p.tracks().active_image_subtitle, Some(1));
    assert!(r.p.stats().picture_failures >= 4, "{:?}", r.p.stats());
    // A seek into the damaged part starts the count again: nothing decodes there, and the track goes.
    let from = r.rec.len();
    r.p.seek(1.6);
    let failed = r.rec.wait_for(from, T, |e| matches!(e, PlayerEvent::SubtitleTrackFailed { stream_index: 1, .. }));
    assert!(matches!(failed, Some(PlayerEvent::SubtitleTrackFailed { reason: PictureTrackFailure::Ffmpeg(_), .. })), "{failed:?} {:?} {:?}", r.p.stats(), &r.rec.snapshot()[from..]);
    assert!(wait_until(T, || r.p.tracks().active_image_subtitle.is_none()));
}

#[test]
fn xsub_packets_without_time_codes_give_the_track_up_with_that_reason() {
    let mut r = open_clip(&or_skip!(xsub_without_time_codes()));
    let from = r.rec.len();
    r.select(Some(1));
    let failed = r.rec.wait_for(from, T, |e| matches!(e, PlayerEvent::SubtitleTrackFailed { stream_index: 1, .. }));
    assert!(matches!(failed, Some(PlayerEvent::SubtitleTrackFailed { reason: PictureTrackFailure::NoTimeCode, .. })), "{failed:?}");
    assert!(wait_until(T, || r.p.tracks().active_image_subtitle.is_none()));
}

#[test]
fn pictures_too_large_to_keep_give_the_track_up_with_that_reason() {
    let mut r = open_clip(&or_skip!(pgs_too_large_mkv()));
    let from = r.rec.len();
    r.select(Some(1));
    r.p.play();
    let failed = r.rec.wait_for(from, T, |e| matches!(e, PlayerEvent::SubtitleTrackFailed { stream_index: 1, .. }));
    assert!(matches!(failed, Some(PlayerEvent::SubtitleTrackFailed { reason: PictureTrackFailure::TooLarge, .. })), "{failed:?}");
    assert!(r.picture().is_none());
}

#[test]
fn pictures_over_a_quarter_turned_video_carry_its_turned_size() {
    let path = or_skip!(rotated_pgs_mkv());
    let mut r = open_clip(&path);
    r.select(Some(1));
    let p = r.seek_to_picture(0.7, 0.5);
    assert_eq!(p.canvas, (320, 240), "the track's own canvas");
    assert_eq!(p.video_size, Some((240, 320)), "the video as shown, a quarter turned");
}

#[test]
fn with_one_frame_a_second_captions_still_start_and_end_on_time() {
    let mut r = open_clip(&or_skip!(one_fps_mkv()));
    r.select(Some(1));
    r.seek_to_nothing(0.0);
    let from = r.rec.len();
    r.p.play();
    let mut seen: Vec<(f64, Option<f64>)> = Vec::new();
    let deadline = Instant::now() + T;
    while !r.p.is_ended() && Instant::now() < deadline && r.p.position() < 4.0 {
        let now = r.picture().map(|p| p.start);
        if seen.last().map(|s| s.1) != Some(now) {
            seen.push((r.p.position(), now));
        }
        let _ = r.p.latest_frame();
        std::thread::sleep(Duration::from_millis(2));
    }
    r.p.pause();
    let starts: Vec<Option<f64>> = seen.iter().map(|s| s.1.map(|t| (t * 10.0).round() / 10.0)).collect();
    assert_eq!(starts, vec![None, Some(0.5), None, Some(2.3), None], "{seen:?}");
    // Each change within a few polls of its time, not at the next frame (up to a second later).
    for ((at, _), due) in seen.iter().zip([0.0, 0.5, 1.7, 2.3, 3.4]).skip(1) {
        assert!(*at >= due - 0.02 && *at < due + 0.15, "changed at {at}, due at {due}: {seen:?}");
    }
    assert!(r.picture_changes(from) >= 4);
}

#[test]
fn a_few_damaged_xsub_chunks_anywhere_never_turn_the_track_off() {
    // The scan and the demuxer both read the four damaged chunks at once, far from the position: they are turned
    // away when stored and never count towards giving up.
    let mut r = open_clip(&or_skip!(xsub_with_four_damaged_chunks()));
    let from = r.rec.len();
    r.select(Some(1));
    let p = r.seek_to_picture(1.05, 1.0);
    assert_eq!(p.bounds().0, 40);
    r.settle();
    assert!(!r.rec.snapshot()[from..].iter().any(|e| matches!(e, PlayerEvent::SubtitleTrackFailed { .. })), "{:?}", r.rec.snapshot());
    assert_eq!(r.p.tracks().active_image_subtitle, Some(1));
    assert!(r.p.stats().picture_failures >= 4, "the damaged chunks are counted: {:?}", r.p.stats());
    // Selected again, and played over the damaged part: still on, the captions after it shown.
    r.select(None);
    r.select(Some(1));
    r.seek_to_picture(7.85, 7.8);
    r.p.play();
    assert!(r.wait_picture(|p| (p.start - 8.4).abs() < 1e-3).is_some(), "{:?}", r.picture());
    r.p.pause();
    assert_eq!(r.p.tracks().active_image_subtitle, Some(1));
}

#[test]
fn an_xsub_track_chosen_far_past_its_front_loaded_chunks_shows_the_caption_there() {
    // The demuxer read every chunk at the start of the file long ago; only the scan delivers them now.
    let mut r = open_clip(&or_skip!(big_xsub_avi()));
    r.seek_to_nothing(0.5);
    r.seek(20.0);
    r.settle();
    r.select(Some(1));
    let p = r.wait_picture(|p| (p.start - big_start(79)).abs() < 2e-3).unwrap_or_else(|| panic!("{:?} {:?}", r.picture(), r.p.stats()));
    assert_eq!(p.bounds().2 as usize, (400 + 79usize).next_multiple_of(2));
}

#[test]
fn a_restart_in_place_never_decodes_a_set_again_against_objects_redefined_after_it() {
    let mut r = open_clip(&or_skip!(pgs_redefined_object_mkv()));
    r.select(Some(1));
    // Played into the epoch to the normal set at 1.1 (object 0, version 0: white), paused there with the read-ahead
    // past the set at 2.0, which redefined object 0 (version 1: red) in the decoder.
    r.seek_to_picture(0.6, 0.5);
    r.p.play();
    let p = r.wait_picture(|p| (p.start - 1.1).abs() < 1e-3).unwrap_or_else(|| panic!("{:?}", r.picture()));
    r.p.pause();
    r.settle();
    assert_eq!(p.bounds(), (60, 20, 40, 10));
    assert!(!is_red(canvas_pixel(&p, 70, 25)), "white, its own version");
    // Another speed: decoding restarts in place from the keyframe at 1.0 and reads the set at 1.1 again. Fed again
    // to the decoder (which holds version 1 now) it would come out red and replace the queued one.
    let from = r.rec.len();
    let decoded = r.p.stats().decoded;
    r.p.set_rate(1.5);
    r.settle();
    assert!(r.p.stats().decoded > decoded, "decoding did restart");
    let q = r.picture().expect("still on screen");
    assert_eq!(q.id, p.id, "the same picture");
    assert!(!is_red(canvas_pixel(&q, 70, 25)));
    assert_eq!(r.picture_changes(from), 0);
    // Played on, the decoder (kept) shows the new version at 2.0.
    r.p.play();
    let n = r.wait_picture(|p| (p.start - 2.0).abs() < 1e-3).unwrap_or_else(|| panic!("{:?}", r.picture()));
    assert_eq!(n.bounds(), (100, 20, 40, 10));
    assert!(is_red(canvas_pixel(&n, 110, 25)));
}
