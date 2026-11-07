//! 16 kHz mono 16-bit PCM WAV files: the writer used by [`crate::audio`] (byte-identical to Python's `wave`
//! module) and a reader for the engine's own WAV files (used by `se-asr` and `se-diarize`).

use std::fs::File;
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

use se_core::{EngineError, ErrorKind, Result};

/// Format of a WAV file (`wave` module view).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WavInfo {
    pub sample_rate: u32,
    pub channels: u16,
    pub bits_per_sample: u16,
    pub frames: u64,
}

fn wave_error(message: &str) -> EngineError {
    EngineError::new(ErrorKind::Other, message)
}

/// The canonical 44-byte header Python's `wave` writes for 1 channel, 2 bytes, `rate` Hz.
pub fn header(data_len: u32, channels: u16, sample_width: u16, rate: u32) -> [u8; 44] {
    let mut h = [0u8; 44];
    let block_align = channels * sample_width;
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&(36u32.wrapping_add(data_len)).to_le_bytes());
    h[8..12].copy_from_slice(b"WAVE");
    h[12..16].copy_from_slice(b"fmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&1u16.to_le_bytes());
    h[22..24].copy_from_slice(&channels.to_le_bytes());
    h[24..28].copy_from_slice(&rate.to_le_bytes());
    h[28..32].copy_from_slice(&(rate * block_align as u32).to_le_bytes());
    h[32..34].copy_from_slice(&block_align.to_le_bytes());
    h[34..36].copy_from_slice(&(sample_width * 8).to_le_bytes());
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&data_len.to_le_bytes());
    h
}

/// Python `wave.open(path, "wb")` with 1 channel, 2-byte samples, 16 kHz. The header is (re)written at
/// [`WavWriter::close`]; the final bytes equal Python's, which patches the header after every chunk.
pub struct WavWriter {
    out: BufWriter<File>,
    data_len: u64,
    rate: u32,
    closed: bool,
}

/// The largest data length whose RIFF size (`36 + len`) still fits `struct.pack("<L")`.
const MAX_DATA_LEN: u64 = u32::MAX as u64 - 36;

impl WavWriter {
    /// Create/truncate `path` (errors are Python `open()` OSErrors with the file name).
    pub fn create(path: &Path, rate: u32) -> Result<Self> {
        let file = File::create(path).map_err(|e| EngineError::os(&e, Some(path)))?;
        let mut out = BufWriter::with_capacity(1 << 16, file);
        out.write_all(&header(0, 1, 2, rate)).map_err(|e| EngineError::os(&e, None))?;
        Ok(WavWriter { out, data_len: 0, rate, closed: false })
    }

    /// `writeframes(pcm)`. Fails like Python (`argument out of range`) once the data passes 4 GiB.
    pub fn write(&mut self, pcm: &[u8]) -> Result<()> {
        if pcm.is_empty() {
            return Ok(());
        }
        self.out.write_all(pcm).map_err(|e| EngineError::os(&e, None))?;
        self.data_len += pcm.len() as u64;
        if self.data_len > MAX_DATA_LEN {
            return Err(EngineError::new(ErrorKind::Other, "argument out of range"));
        }
        Ok(())
    }

    /// `n` zero samples (`_write_silence`).
    pub fn write_silence(&mut self, samples: u64) -> Result<()> {
        const BLOCK: [u8; 32_000] = [0u8; 32_000];
        let mut left = samples;
        while left > 0 {
            let n = left.min(16_000);
            self.write(&BLOCK[..(n * 2) as usize])?;
            left -= n;
        }
        Ok(())
    }

    pub fn data_len(&self) -> u64 {
        self.data_len
    }

    /// Patch the header and close. A data length over the RIFF limit is never written wrapped.
    pub fn close(&mut self) -> Result<()> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        let res = (|| -> io::Result<()> {
            self.out.flush()?;
            if self.data_len <= MAX_DATA_LEN {
                let h = header(self.data_len as u32, 1, 2, self.rate);
                let f = self.out.get_mut();
                f.seek(SeekFrom::Start(0))?;
                f.write_all(&h)?;
                f.seek(SeekFrom::End(0))?;
            }
            Ok(())
        })();
        res.map_err(|e| EngineError::os(&e, None))?;
        if self.data_len > MAX_DATA_LEN {
            return Err(EngineError::new(ErrorKind::Other, "argument out of range"));
        }
        Ok(())
    }
}

/// Parsed header of a RIFF/WAVE file plus where its data starts and how long it is.
struct Parsed {
    info: WavInfo,
    data_start: u64,
    data_len: u64,
}

fn read_exact_or_eof(f: &mut File, buf: &mut [u8]) -> Result<bool> {
    let mut read = 0;
    while read < buf.len() {
        match f.read(&mut buf[read..]) {
            Ok(0) => return Ok(false),
            Ok(n) => read += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(EngineError::os(&e, None)),
        }
    }
    Ok(true)
}

/// Python `wave.Wave_read` header parsing (PCM and WAVE_FORMAT_EXTENSIBLE PCM).
fn parse(path: &Path) -> Result<(File, Parsed)> {
    let mut f = File::open(path).map_err(|e| EngineError::os(&e, Some(path)))?;
    let file_len = f.metadata().map(|m| m.len()).unwrap_or(u64::MAX);
    let mut riff = [0u8; 12];
    if !read_exact_or_eof(&mut f, &mut riff[..8])? {
        return Err(EngineError::new(ErrorKind::Other, ""));
    }
    if &riff[0..4] != b"RIFF" {
        return Err(wave_error("file does not start with RIFF id"));
    }
    if !read_exact_or_eof(&mut f, &mut riff[8..12])? || &riff[8..12] != b"WAVE" {
        return Err(wave_error("not a WAVE file"));
    }
    let mut fmt: Option<(u16, u32, u16)> = None;
    let mut pos = 12u64;
    loop {
        let mut head = [0u8; 8];
        if !read_exact_or_eof(&mut f, &mut head)? {
            break;
        }
        pos += 8;
        let id = [head[0], head[1], head[2], head[3]];
        let size = u32::from_le_bytes([head[4], head[5], head[6], head[7]]) as u64;
        if &id == b"fmt " {
            let mut body = vec![0u8; size.min(64) as usize];
            if !read_exact_or_eof(&mut f, &mut body)? || body.len() < 14 {
                return Err(EngineError::new(ErrorKind::Other, ""));
            }
            let tag = u16::from_le_bytes([body[0], body[1]]);
            let channels = u16::from_le_bytes([body[2], body[3]]);
            let rate = u32::from_le_bytes([body[4], body[5], body[6], body[7]]);
            let bits = if body.len() >= 16 { u16::from_le_bytes([body[14], body[15]]) } else { 0 };
            let mut tag_ok = tag == 1;
            if tag == 0xFFFE && body.len() >= 26 {
                tag_ok = u16::from_le_bytes([body[24], body[25]]) == 1;
            }
            if !tag_ok {
                return Err(wave_error(&format!("unknown format: {tag}")));
            }
            if channels == 0 {
                return Err(wave_error("bad # of channels"));
            }
            fmt = Some((channels, rate, bits));
        } else if &id == b"data" {
            let Some((channels, rate, bits)) = fmt else {
                return Err(wave_error("data chunk before fmt chunk"));
            };
            let data_len = size.min(file_len.saturating_sub(pos));
            let width = bits.div_ceil(8).max(1) as u64;
            let frames = data_len / (channels as u64 * width);
            let info = WavInfo { sample_rate: rate, channels, bits_per_sample: bits, frames };
            return Ok((f, Parsed { info, data_start: pos, data_len }));
        }
        let skip = size + (size & 1);
        pos += skip;
        f.seek(SeekFrom::Start(pos)).map_err(|e| EngineError::os(&e, None))?;
    }
    Err(wave_error("fmt chunk and/or data chunk missing"))
}

/// Header of a WAV file. Errors carry Python `wave`-like texts (best effort).
pub fn wav_info(path: &Path) -> Result<WavInfo> {
    parse(path).map(|(_, p)| p.info)
}

fn read_data(path: &Path) -> Result<(WavInfo, Vec<u8>)> {
    let (mut f, p) = parse(path)?;
    f.seek(SeekFrom::Start(p.data_start)).map_err(|e| EngineError::os(&e, None))?;
    let width = p.info.bits_per_sample.div_ceil(8).max(1) as u64;
    let usable = p.info.frames * p.info.channels as u64 * width;
    let mut data = Vec::with_capacity(usable as usize);
    f.take(p.data_len.min(usable)).read_to_end(&mut data).map_err(|e| EngineError::os(&e, None))?;
    Ok((p.info, data))
}

/// All samples as `i16` (any channel count interleaved as stored).
pub fn read_i16(path: &Path) -> Result<(WavInfo, Vec<i16>)> {
    let (info, data) = read_data(path)?;
    if info.bits_per_sample != 16 {
        return Err(wave_error(&format!("unsupported sample width: {}", info.bits_per_sample)));
    }
    let samples = data.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
    Ok((info, samples))
}

/// All samples as `f32` in `[-1, 1)` (`int16 / 32768`), like `np.frombuffer(...).astype(float32) / 32768`.
pub fn read_f32(path: &Path) -> Result<(WavInfo, Vec<f32>)> {
    let (info, samples) = read_i16(path)?;
    Ok((info, samples.into_iter().map(|s| s as f32 / 32768.0).collect()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_matches_python_wave_for_empty_and_small_files() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.wav");
        let mut w = WavWriter::create(&p, 16_000).unwrap();
        w.close().unwrap();
        let bytes = std::fs::read(&p).unwrap();
        assert_eq!(bytes.len(), 44);
        assert_eq!(&bytes[..], &header(0, 1, 2, 16_000)[..]);
        // Python: wave.open(p, "wb"); setnchannels(1); setsampwidth(2); setframerate(16000); writeframes(b"\1\0\2\0")
        let mut w = WavWriter::create(&p, 16_000).unwrap();
        w.write(&[1, 0, 2, 0]).unwrap();
        w.write_silence(3).unwrap();
        w.close().unwrap();
        let bytes = std::fs::read(&p).unwrap();
        let expected: Vec<u8> = [
            b"RIFF".as_slice(),
            &46u32.to_le_bytes(),
            b"WAVEfmt ",
            &16u32.to_le_bytes(),
            &[1, 0, 1, 0],
            &16000u32.to_le_bytes(),
            &32000u32.to_le_bytes(),
            &[2, 0, 16, 0],
            b"data",
            &10u32.to_le_bytes(),
            &[1, 0, 2, 0, 0, 0, 0, 0, 0, 0],
        ]
        .concat();
        assert_eq!(bytes, expected);
        let (info, samples) = read_i16(&p).unwrap();
        assert_eq!(info, WavInfo { sample_rate: 16_000, channels: 1, bits_per_sample: 16, frames: 5 });
        assert_eq!(samples, vec![1, 2, 0, 0, 0]);
        let (_, f) = read_f32(&p).unwrap();
        assert_eq!(f[0], 1.0 / 32768.0);
    }

    #[test]
    fn reader_rejects_non_wav() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.wav");
        std::fs::write(&p, b"RIFX0000WAVE").unwrap();
        assert_eq!(wav_info(&p).unwrap_err().to_string(), "file does not start with RIFF id");
    }
}
