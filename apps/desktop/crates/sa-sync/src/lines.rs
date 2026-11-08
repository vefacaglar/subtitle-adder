//! Line framing of the broker pipe (win-sync §4.1): .NET `StreamReader.ReadLine` semantics.
//!
//! A line ends at `\n`, `\r\n` or a lone `\r`; the terminator is stripped. Bytes are decoded as UTF-8 with invalid
//! sequences replaced (U+FFFD, like `StreamReader`), a UTF-8 BOM at the very start of the stream is skipped. Lines of
//! hundreds of KB are normal (thumbnails travel inline); a line above [`MAX_LINE_BYTES`] is a protocol violation and
//! the connection is dropped. Writers always terminate with `"\n"`.

/// Upper bound for one line (64 MiB); larger → the peer is disconnected.
pub const MAX_LINE_BYTES: usize = 64 * 1024 * 1024;

/// Errors of [`LineDecoder::push`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FramingError {
    /// The current line exceeds [`MAX_LINE_BYTES`].
    LineTooLong,
}

/// Incremental decoder: feed raw bytes, receive complete lines.
#[derive(Debug)]
pub struct LineDecoder {
    buf: Vec<u8>,
    /// A `\r` ended the previous line; a `\n` at the start of the next chunk belongs to it.
    pending_cr: bool,
    at_stream_start: bool,
    max: usize,
}

impl Default for LineDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl LineDecoder {
    pub fn new() -> Self {
        Self::with_limit(MAX_LINE_BYTES)
    }

    /// A decoder with a custom line limit (tests).
    pub fn with_limit(max: usize) -> Self {
        Self { buf: Vec::new(), pending_cr: false, at_stream_start: true, max }
    }

    /// Appends `data` and pushes every completed line into `out`.
    pub fn push(&mut self, data: &[u8], out: &mut Vec<String>) -> Result<(), FramingError> {
        if !self.at_stream_start {
            return self.push_inner(data, out);
        }
        // Skip a UTF-8 preamble (possibly split across reads). Until decided, `buf` only holds BOM candidates.
        const BOM: [u8; 3] = [0xEF, 0xBB, 0xBF];
        let take = (3 - self.buf.len()).min(data.len());
        self.buf.extend_from_slice(&data[..take]);
        let rest = &data[take..];
        if self.buf.len() < 3 && BOM.starts_with(&self.buf) {
            return Ok(());
        }
        self.at_stream_start = false;
        let head = std::mem::take(&mut self.buf);
        if head.starts_with(&BOM) {
            self.push_inner(rest, out)
        } else {
            let mut combined = head;
            combined.extend_from_slice(rest);
            self.push_inner(&combined, out)
        }
    }

    fn push_inner(&mut self, mut data: &[u8], out: &mut Vec<String>) -> Result<(), FramingError> {
        if self.pending_cr {
            self.pending_cr = false;
            if let Some(b'\n') = data.first() {
                data = &data[1..];
            }
        }
        let mut start = 0;
        let mut i = 0;
        while i < data.len() {
            match data[i] {
                b'\n' => {
                    self.finish(&data[start..i], out);
                    i += 1;
                    start = i;
                }
                b'\r' => {
                    self.finish(&data[start..i], out);
                    i += 1;
                    if i < data.len() {
                        if data[i] == b'\n' {
                            i += 1;
                        }
                    } else {
                        self.pending_cr = true;
                    }
                    start = i;
                }
                _ => i += 1,
            }
        }
        self.buf.extend_from_slice(&data[start..]);
        if self.buf.len() > self.max {
            self.buf.clear();
            return Err(FramingError::LineTooLong);
        }
        Ok(())
    }

    /// End of stream: a trailing unterminated line is returned like `ReadLine` does.
    pub fn finish_stream(&mut self) -> Option<String> {
        if self.buf.is_empty() {
            return None;
        }
        let line = String::from_utf8_lossy(&self.buf).into_owned();
        self.buf.clear();
        Some(line)
    }

    fn finish(&mut self, tail: &[u8], out: &mut Vec<String>) {
        if self.buf.is_empty() {
            out.push(String::from_utf8_lossy(tail).into_owned());
        } else {
            self.buf.extend_from_slice(tail);
            out.push(String::from_utf8_lossy(&self.buf).into_owned());
            self.buf.clear();
        }
    }
}

/// `string.IsNullOrWhiteSpace`: blank lines are skipped by every reader.
pub fn is_blank(line: &str) -> bool {
    line.trim().is_empty()
}

/// Encodes `line + "\n"` as UTF-8 (no BOM).
pub fn encode_line(line: &str) -> Vec<u8> {
    let mut v = Vec::with_capacity(line.len() + 1);
    v.extend_from_slice(line.as_bytes());
    v.push(b'\n');
    v
}
