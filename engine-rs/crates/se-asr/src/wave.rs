//! A WAV reader with Python 3.11 `wave` module semantics (`wave.open(path, "rb")`): the same chunk walk, the same
//! frame count (`data` chunk size // frame size), short reads at a truncated file and the same error texts
//! (`file does not start with RIFF id`, `unknown format: 3`, an empty message for `EOFError`, ...).

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use se_core::{EngineError, ErrorKind, Result};

const WAVE_FORMAT_PCM: u16 = 1;

/// An open WAV file positioned in its `data` chunk.
pub struct WaveReader {
    file: BufReader<File>,
    nchannels: u16,
    sampwidth: u16,
    framerate: u32,
    nframes: u64,
    /// Bytes of the data chunk (as declared).
    data_size: u64,
    /// Bytes of the data chunk read so far.
    data_read: u64,
    /// Frames read so far (`tell()`).
    soundpos: u64,
}

fn wave_error(message: &str) -> EngineError {
    EngineError::new(ErrorKind::Other, message)
}

/// Python `EOFError()` (empty `str()`).
fn eof_error() -> EngineError {
    EngineError::new(ErrorKind::Other, "")
}

/// Reads up to `n` bytes (fewer at EOF), like Python `file.read(n)`.
fn read_up_to(file: &mut impl Read, n: usize) -> std::io::Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(n.min(1 << 20));
    file.take(n as u64).read_to_end(&mut buf)?;
    Ok(buf)
}

/// `chunk.Chunk(file, bigendian=0)` header: name and size; `None` on `EOFError`.
fn read_chunk_header(file: &mut impl Read) -> std::io::Result<Option<([u8; 4], u64)>> {
    let name = read_up_to(file, 4)?;
    if name.len() < 4 {
        return Ok(None);
    }
    let size = read_up_to(file, 4)?;
    if size.len() < 4 {
        return Ok(None);
    }
    let size = u32::from_le_bytes([size[0], size[1], size[2], size[3]]) as u64;
    Ok(Some(([name[0], name[1], name[2], name[3]], size)))
}

impl WaveReader {
    /// `wave.open(str(path), "rb")`.
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).map_err(|e| EngineError::os(&e, Some(path)))?;
        let mut file = BufReader::new(file);
        let io = |e: std::io::Error| EngineError::from(e);

        let Some((riff, _riff_size)) = read_chunk_header(&mut file).map_err(io)? else {
            return Err(eof_error());
        };
        if &riff != b"RIFF" {
            return Err(wave_error("file does not start with RIFF id"));
        }
        // The outer chunk's read(4) is bounded by its size; for a sane file this is just the next 4 bytes.
        let wave = read_up_to(&mut file, 4).map_err(io)?;
        if wave != b"WAVE" {
            return Err(wave_error("not a WAVE file"));
        }

        let mut fmt: Option<(u16, u32, u16)> = None;
        loop {
            let Some((name, size)) = read_chunk_header(&mut file).map_err(io)? else {
                break;
            };
            if &name == b"fmt " {
                let head = read_up_to(&mut file, 14.min(size as usize)).map_err(io)?;
                if head.len() < 14 {
                    return Err(eof_error());
                }
                let tag = u16::from_le_bytes([head[0], head[1]]);
                let nchannels = u16::from_le_bytes([head[2], head[3]]);
                let framerate = u32::from_le_bytes([head[4], head[5], head[6], head[7]]);
                let mut consumed = 14u64;
                if tag != WAVE_FORMAT_PCM {
                    return Err(wave_error(&format!("unknown format: {tag}")));
                }
                let width = read_up_to(&mut file, 2.min(size.saturating_sub(14) as usize)).map_err(io)?;
                if width.len() < 2 {
                    return Err(eof_error());
                }
                consumed += 2;
                let bits = u16::from_le_bytes([width[0], width[1]]) as u32;
                let sampwidth = ((bits + 7) / 8) as u16;
                if sampwidth == 0 {
                    return Err(wave_error("bad sample width"));
                }
                if nchannels == 0 {
                    return Err(wave_error("bad # of channels"));
                }
                fmt = Some((nchannels, framerate, sampwidth));
                // chunk.skip(): the rest of the chunk plus the pad byte of an odd size.
                let mut rest = size - consumed.min(size);
                if size & 1 == 1 {
                    rest += 1;
                }
                file.seek(SeekFrom::Current(rest as i64)).map_err(io)?;
                continue;
            }
            if &name == b"data" {
                let Some((nchannels, framerate, sampwidth)) = fmt else {
                    return Err(wave_error("data chunk before fmt chunk"));
                };
                let framesize = nchannels as u64 * sampwidth as u64;
                return Ok(Self {
                    file,
                    nchannels,
                    sampwidth,
                    framerate,
                    nframes: size / framesize,
                    data_size: size,
                    data_read: 0,
                    soundpos: 0,
                });
            }
            let mut rest = size;
            if size & 1 == 1 {
                rest += 1;
            }
            file.seek(SeekFrom::Current(rest as i64)).map_err(io)?;
        }
        Err(wave_error("fmt chunk and/or data chunk missing"))
    }

    pub fn nchannels(&self) -> u16 {
        self.nchannels
    }

    pub fn sampwidth(&self) -> u16 {
        self.sampwidth
    }

    pub fn framerate(&self) -> u32 {
        self.framerate
    }

    pub fn nframes(&self) -> u64 {
        self.nframes
    }

    /// Frames read so far (`tell()`).
    pub fn tell(&self) -> u64 {
        self.soundpos
    }

    fn framesize(&self) -> u64 {
        self.nchannels as u64 * self.sampwidth as u64
    }

    /// `readframes(n)`: raw little-endian bytes of up to `n` frames (fewer at the end of the data chunk or the file).
    pub fn readframes(&mut self, n: u64) -> Result<Vec<u8>> {
        let want = n.saturating_mul(self.framesize());
        let left = self.data_size - self.data_read;
        let want = want.min(left);
        let data = read_up_to(&mut self.file, want as usize)?;
        self.data_read += data.len() as u64;
        self.soundpos += data.len() as u64 / self.framesize();
        Ok(data)
    }

    /// `readframes(n)` decoded as little-endian i16 samples (sample width 2).
    pub fn read_i16(&mut self, n: u64) -> Result<Vec<i16>> {
        let raw = self.readframes(n)?;
        Ok(raw.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect())
    }
}

/// `_wav_duration(audio)`: `nframes / framerate` (`division by zero` for a zero rate).
pub fn wav_duration(path: &Path) -> Result<f64> {
    let wav = WaveReader::open(path)?;
    if wav.framerate() == 0 {
        return Err(EngineError::new(ErrorKind::Other, "division by zero"));
    }
    Ok(wav.nframes() as f64 / wav.framerate() as f64)
}

/// Writes a 16-bit PCM WAV (Python `wave` writer layout); used by tests.
pub fn write_pcm16(path: &Path, rate: u32, channels: u16, samples: &[i16]) -> std::io::Result<()> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * channels as u32 * 2).to_le_bytes());
    out.extend_from_slice(&(channels * 2).to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    std::fs::write(path, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_header_and_frames() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.wav");
        write_pcm16(&path, 16000, 1, &[1, -2, 3]).unwrap();
        let mut wav = WaveReader::open(&path).unwrap();
        assert_eq!((wav.nchannels(), wav.sampwidth(), wav.framerate(), wav.nframes()), (1, 2, 16000, 3));
        assert_eq!(wav.read_i16(2).unwrap(), vec![1, -2]);
        assert_eq!(wav.tell(), 2);
        assert_eq!(wav.read_i16(5).unwrap(), vec![3]);
        assert_eq!(wav.read_i16(5).unwrap(), Vec::<i16>::new());
    }

    #[test]
    fn errors_match_python_wave() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("b.wav");
        std::fs::write(&path, b"RIFX....WAVE").unwrap();
        assert_eq!(WaveReader::open(&path).err().unwrap().message, "file does not start with RIFF id");
        std::fs::write(&path, b"RIF").unwrap();
        assert_eq!(WaveReader::open(&path).err().unwrap().message, "");
        let mut float = Vec::new();
        float.extend_from_slice(b"RIFF\x00\x00\x00\x00WAVEfmt \x10\x00\x00\x00\x03\x00\x01\x00");
        float.extend_from_slice(&[0u8; 12]);
        std::fs::write(&path, &float).unwrap();
        assert_eq!(WaveReader::open(&path).err().unwrap().message, "unknown format: 3");
        std::fs::write(&path, b"RIFF\x00\x00\x00\x00WAVE").unwrap();
        assert_eq!(WaveReader::open(&path).err().unwrap().message, "fmt chunk and/or data chunk missing");
        std::fs::write(&path, b"RIFF\x00\x00\x00\x00WAVEdata\x00\x00\x00\x00").unwrap();
        assert_eq!(WaveReader::open(&path).err().unwrap().message, "data chunk before fmt chunk");
    }
}
