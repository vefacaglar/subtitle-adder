//! Port of `tests/test_audio.py` (+ extra behavior checks from spec `audio.md` §10).

use std::path::Path;

use se_audio::audio::{extract_audio, extract_audio_with_failing_packets};
use se_audio::testgen;
use se_audio::wav::wav_info;
use se_core::ErrorKind;

fn read_wav(path: &Path) -> (u32, u16, f64) {
    let info = wav_info(path).unwrap();
    (info.sample_rate, info.channels, info.frames as f64 / info.sample_rate as f64)
}

fn tone_video(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("tone.mp4");
    testgen::tone_video(&path).unwrap();
    path
}

#[test]
fn test_extracts_mono_16k_wav() {
    let tmp = tempfile::tempdir().unwrap();
    let video = tone_video(tmp.path());
    let wav = extract_audio(&video, &tmp.path().join("out.wav"), None).unwrap();
    let (rate, channels, seconds) = read_wav(&wav);
    assert_eq!((rate, channels), (16_000, 1));
    assert!((seconds - 2.0).abs() < 0.15, "{seconds}");
}

/// Golden values measured with the Python engine (PyAV 18.1 / FFmpeg 8.1.2) on the same fixture:
/// 32322 samples and 87 progress callbacks (first 0.011609977324263039, ending 0.9868480725623568,
/// 0.9984580498866198, 1.0). The sample count may differ by a frame between FFmpeg builds.
#[test]
fn test_tone_video_progress_and_sample_count_match_python() {
    let tmp = tempfile::tempdir().unwrap();
    let video = tone_video(tmp.path());
    let mut fractions = Vec::new();
    let mut cb = |f: f64| fractions.push(f);
    let wav = extract_audio(&video, &tmp.path().join("out.wav"), Some(&mut cb)).unwrap();
    let frames = wav_info(&wav).unwrap().frames as i64;
    assert!((frames - 32322).abs() <= 1024 / 2, "{frames}");
    assert_eq!(std::fs::metadata(&wav).unwrap().len(), 44 + 2 * frames as u64);
    assert_eq!(fractions.len(), 87, "{fractions:?}");
    assert_eq!(fractions[0], 0.011609977324263039);
    assert_eq!(&fractions[84..], &[0.9868480725623568, 0.9984580498866198, 1.0]);
    for w in fractions.windows(2) {
        assert!(w[1] - w[0] >= 0.01 || w[1] >= 1.0);
    }
}

#[test]
fn test_missing_video() {
    let tmp = tempfile::tempdir().unwrap();
    let err = extract_audio(&tmp.path().join("yok.mp4"), &tmp.path().join("out.wav"), None).unwrap_err();
    assert_eq!(err.kind, ErrorKind::AudioExtraction);
    assert!(err.to_string().contains("yok.mp4"));
    assert!(!tmp.path().join("out.wav").exists());
}

#[test]
fn test_late_audio_start_is_padded_to_container_timeline() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("late.mp4");
    testgen::late_audio_mp4(&path).unwrap();
    let (_, _, seconds) = read_wav(&extract_audio(&path, &tmp.path().join("out.wav"), None).unwrap());
    assert!((seconds - 4.0).abs() < 0.15, "{seconds}");
}

#[test]
fn test_48khz_aac_does_not_accumulate_rounding_drift() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("long_48k.mp4");
    let (rate, duration_sec) = (48_000i64, 180i64);
    testgen::aac_48k_mp4(&path, duration_sec).unwrap();
    let expected = ((rate * duration_sec + 1023) / 1024 * 1024) as f64 / rate as f64;
    let (_, _, wav_sec) = read_wav(&extract_audio(&path, &tmp.path().join("out.wav"), None).unwrap());
    assert!((wav_sec - expected).abs() < 0.05, "{wav_sec} vs {expected}");
}

#[test]
fn test_corrupt_packet_mid_stream_is_skipped_without_failing_or_drifting() {
    let tmp = tempfile::tempdir().unwrap();
    let video = tone_video(tmp.path());
    let wav = extract_audio_with_failing_packets(&video, &tmp.path().join("recovered.wav"), None, &[5, 6, 7]).unwrap();
    let (rate, channels, seconds) = read_wav(&wav);
    assert_eq!((rate, channels), (16_000, 1));
    assert!((seconds - 2.0).abs() < 0.15, "{seconds}");
}

#[test]
fn test_wmv_avi_mov_mkv_mp4_containers_with_corrupt_midstream_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let cases = [
        ("sample.wmv", "wmav2"),
        ("sample.avi", "libmp3lame"),
        ("sample.mov", "aac"),
        ("sample.mkv", "aac"),
        ("sample.mp4", "aac"),
    ];
    for (filename, codec) in cases {
        let path = tmp.path().join(filename);
        testgen::corrupt_tone_container(&path, codec).unwrap();
        let wav = extract_audio(&path, &tmp.path().join(format!("{filename}.wav")), None)
            .unwrap_or_else(|e| panic!("{filename}: {e}"));
        let (rate, channels, seconds) = read_wav(&wav);
        assert_eq!((rate, channels), (16_000, 1), "{filename}");
        assert!(seconds > 2.5, "{filename}: {seconds}");
    }
}

#[test]
fn test_unreadable_file_reports_the_ffmpeg_error_wrapped() {
    let tmp = tempfile::tempdir().unwrap();
    // A file that is not media at all: FFmpeg's error is wrapped in `audio_read_failed`.
    let bad = tmp.path().join("bad.mp4");
    std::fs::write(&bad, b"not a video at all").unwrap();
    let err = extract_audio(&bad, &tmp.path().join("bad.wav"), None).unwrap_err();
    assert_eq!(err.kind, ErrorKind::AudioExtraction);
    let msg = err.to_string();
    assert!(msg.starts_with("Ses okunamadı: [Errno 1094995529] Invalid data found when processing input: '"), "{msg}");
    assert!(msg.ends_with("bad.mp4'"), "{msg}");
}

#[test]
fn test_no_audio_track_is_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let video = tmp.path().join("silent.mkv");
    testgen::video_only(&video).unwrap();
    let err = extract_audio(&video, &tmp.path().join("out.wav"), None).unwrap_err();
    assert_eq!(err.kind, ErrorKind::AudioExtraction);
    assert_eq!(err.to_string(), "Videoda ses izi yok.");
}

/// A 16 kHz mono s16 source puts the resampler in passthrough mode: the PCM is copied verbatim.
#[test]
fn test_16k_mono_s16_input_is_passed_through_unchanged() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("in.wav");
    let samples: Vec<i16> = (0..16_000 * 3).map(|i| ((i * 37) % 20_000 - 10_000) as i16).collect();
    testgen::pcm_wav(&src, 16_000, 1, &samples).unwrap();
    let mut fractions = Vec::new();
    let mut cb = |f: f64| fractions.push(f);
    let wav = extract_audio(&src, &tmp.path().join("out.wav"), Some(&mut cb)).unwrap();
    let (_, out) = se_audio::wav::read_i16(&wav).unwrap();
    assert_eq!(out, samples);
    assert_eq!(*fractions.last().unwrap(), 1.0);
}

/// 44.1 kHz stereo PCM is downmixed and resampled; the length follows the input duration.
#[test]
fn test_stereo_pcm_is_resampled_to_16k_mono() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("in.wav");
    let samples = vec![1000i16; 44_100 * 2 * 2];
    testgen::pcm_wav(&src, 44_100, 2, &samples).unwrap();
    let wav = extract_audio(&src, &tmp.path().join("out.wav"), None).unwrap();
    let (rate, channels, seconds) = read_wav(&wav);
    assert_eq!((rate, channels), (16_000, 1));
    assert!((seconds - 2.0).abs() < 0.01, "{seconds}");
}
