//! `read_wav`: Python `wave` + numpy parity on the formats the engine sees.

use std::path::Path;

use se_diarize::read_wav;

fn write(path: &Path, rate: u32, channels: u16, bits: u16, float: bool, samples: &[f32]) {
    let spec = hound::WavSpec {
        channels,
        sample_rate: rate,
        bits_per_sample: bits,
        sample_format: if float { hound::SampleFormat::Float } else { hound::SampleFormat::Int },
    };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    for &s in samples {
        if float {
            w.write_sample(s).unwrap();
        } else if bits == 8 {
            w.write_sample((s * 127.0) as i8).unwrap();
        } else if bits == 16 {
            w.write_sample((s * 32768.0).clamp(-32768.0, 32767.0) as i16).unwrap();
        } else {
            w.write_sample((s * 8_388_608.0) as i32).unwrap();
        }
    }
    w.finalize().unwrap();
}

#[test]
#[serial_test::serial]
fn test_reads_16k_mono_pcm_as_float32() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("a.wav");
    write(&p, 16000, 1, 16, false, &[0.0, 0.5, -1.0, 32767.0 / 32768.0]);
    assert_eq!(read_wav(&p).unwrap(), vec![0.0, 0.5, -1.0, 32767.0 / 32768.0]);
}

#[test]
#[serial_test::serial]
fn test_other_pcm_formats_are_rejected_with_the_localized_message() {
    let dir = tempfile::tempdir().unwrap();
    for (rate, channels, bits) in [(44100, 1, 16), (16000, 2, 16), (16000, 1, 8)] {
        let p = dir.path().join(format!("{rate}-{channels}-{bits}.wav"));
        write(&p, rate, channels, bits, false, &[0.0, 0.1]);
        let err = read_wav(&p).unwrap_err();
        assert_eq!(err.kind, se_core::ErrorKind::Model);
        assert_eq!(err.message, se_core::i18n::t("diarize_wav_format", &[]));
    }
}

#[test]
#[serial_test::serial]
fn test_python_wave_errors() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("f.wav");
    // hound writes WAVE_FORMAT_EXTENSIBLE for float and > 16 bits; Python 3.11 `wave` rejects it.
    write(&p, 16000, 1, 32, true, &[0.0]);
    assert_eq!(read_wav(&p).unwrap_err().message, "unknown format: 65534");
    let mut bytes = b"RIFF\x24\x00\x00\x00WAVEfmt \x10\x00\x00\x00".to_vec();
    bytes.extend_from_slice(&[3, 0, 1, 0, 0x80, 0x3e, 0, 0, 0, 0xfa, 0, 0, 4, 0, 32, 0]);
    bytes.extend_from_slice(b"data\x00\x00\x00\x00");
    std::fs::write(&p, &bytes).unwrap();
    assert_eq!(read_wav(&p).unwrap_err().message, "unknown format: 3");

    let p = dir.path().join("junk.wav");
    std::fs::write(&p, b"JUNKJUNKJUNK").unwrap();
    assert_eq!(read_wav(&p).unwrap_err().message, "file does not start with RIFF id");

    let p = dir.path().join("notwave.wav");
    std::fs::write(&p, b"RIFF\x04\x00\x00\x00AVI ").unwrap();
    assert_eq!(read_wav(&p).unwrap_err().message, "not a WAVE file");

    let p = dir.path().join("nodata.wav");
    let mut bytes = b"RIFF\x1c\x00\x00\x00WAVEfmt \x10\x00\x00\x00".to_vec();
    bytes.extend_from_slice(&[1, 0, 1, 0, 0x80, 0x3e, 0, 0, 0, 0x7d, 0, 0, 2, 0, 16, 0]);
    std::fs::write(&p, &bytes).unwrap();
    assert_eq!(read_wav(&p).unwrap_err().message, "fmt chunk and/or data chunk missing");

    let missing = dir.path().join("missing.wav");
    let err = read_wav(&missing).unwrap_err();
    assert!(err.message.starts_with("[Errno 2] No such file or directory: "), "{}", err.message);
}

#[test]
#[serial_test::serial]
fn test_extra_chunks_and_truncated_data_are_tolerated() {
    // LIST chunk (odd size → pad byte) before data; data chunk claims more bytes than the file holds.
    let mut bytes = b"RIFF\x00\x00\x00\x00WAVEfmt \x10\x00\x00\x00".to_vec();
    bytes.extend_from_slice(&[1, 0, 1, 0, 0x80, 0x3e, 0, 0, 0, 0x7d, 0, 0, 2, 0, 16, 0]);
    bytes.extend_from_slice(b"LIST\x03\x00\x00\x00abc\x00");
    bytes.extend_from_slice(b"data\x10\x00\x00\x00");
    bytes.extend_from_slice(&[0x00, 0x40, 0x00, 0xc0]); // 0.5, -0.5
    let riff = (bytes.len() - 8) as u32;
    bytes[4..8].copy_from_slice(&riff.to_le_bytes());
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("x.wav");
    std::fs::write(&p, &bytes).unwrap();
    assert_eq!(read_wav(&p).unwrap(), vec![0.5, -0.5]);
}
