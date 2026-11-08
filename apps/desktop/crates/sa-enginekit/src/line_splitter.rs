//! stdout chunk → lines (mac-enginekit §3.6). Linear in the line length (a 16 MiB line in 256-byte chunks must take
//! under 5 s); decodes only whole lines so multi-byte UTF-8 split across chunks survives.

/// Incremental line splitter.
#[derive(Debug, Default)]
pub struct LineSplitter {
    buffer: Vec<u8>,
    /// Leading bytes of `buffer` already known to contain no `\n`.
    scanned: usize,
}

impl LineSplitter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends `chunk` and returns every completed line (one trailing `\r` removed, lossy UTF-8; empty lines as `""`).
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buffer.extend_from_slice(chunk);
        let mut lines = Vec::new();
        let mut line_start = 0;
        let mut pos = self.scanned;
        while let Some(offset) = self.buffer[pos..].iter().position(|&b| b == b'\n') {
            let newline = pos + offset;
            let mut end = newline;
            if end > line_start && self.buffer[end - 1] == b'\r' {
                end -= 1;
            }
            lines.push(String::from_utf8_lossy(&self.buffer[line_start..end]).into_owned());
            line_start = newline + 1;
            pos = line_start;
        }
        if line_start > 0 {
            self.buffer.drain(..line_start);
        }
        self.scanned = self.buffer.len();
        lines
    }

    /// The unterminated tail as one line (if any); resets the buffer.
    pub fn rest(&mut self) -> Vec<String> {
        self.scanned = 0;
        if self.buffer.is_empty() {
            return Vec::new();
        }
        let tail = std::mem::take(&mut self.buffer);
        vec![String::from_utf8_lossy(&tail).into_owned()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_lines_across_chunks() {
        let mut s = LineSplitter::new();
        assert_eq!(s.feed(b"one\ntw"), vec!["one"]);
        assert_eq!(s.feed(b"o\r\nthree"), vec!["two"]);
        assert_eq!(s.feed(b"\n\nfour\n"), vec!["three", "", "four"]);
        assert!(s.rest().is_empty());
    }

    #[test]
    fn a_carriage_return_waits_for_its_newline() {
        let mut s = LineSplitter::new();
        assert!(s.feed(b"x\r").is_empty());
        assert_eq!(s.feed(b"\n"), vec!["x"]);
    }

    #[test]
    fn multi_byte_characters_split_across_chunks_survive() {
        let text = "Altyazı – şöyle ğ\n";
        let mut s = LineSplitter::new();
        let mut lines = Vec::new();
        for b in text.as_bytes() {
            lines.extend(s.feed(std::slice::from_ref(b)));
        }
        assert_eq!(lines, vec!["Altyazı – şöyle ğ"]);
    }

    #[test]
    fn rest_returns_the_unterminated_tail() {
        let mut s = LineSplitter::new();
        assert!(s.feed(b"partial").is_empty());
        assert_eq!(s.rest(), vec!["partial"]);
        assert!(s.rest().is_empty());
    }

    #[test]
    fn a_huge_single_line_is_cut_in_linear_time() {
        let started = std::time::Instant::now();
        let mut s = LineSplitter::new();
        let chunk = [b'a'; 256];
        for _ in 0..65536 {
            assert!(s.feed(&chunk).is_empty());
        }
        let lines = s.feed(b"\nnext\n");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].len(), 256 * 65536);
        assert_eq!(lines[1], "next");
        assert!(started.elapsed() < std::time::Duration::from_secs(5), "{:?}", started.elapsed());
    }
}
