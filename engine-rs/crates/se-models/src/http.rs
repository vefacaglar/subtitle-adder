//! Shared blocking HTTP helpers (ureq 3, rustls) for model, CUDA and llama.cpp downloads.

use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

use sha2::{Digest, Sha256};

use se_core::{EngineError, ErrorKind, Result};

/// Chunk size of streamed downloads (`_CHUNK_SIZE`).
pub const CHUNK_SIZE: usize = 256 * 1024;
/// Per socket operation timeout of runtime downloads (`urlopen(..., timeout=60)`).
pub const SOCKET_TIMEOUT: Duration = Duration::from_secs(60);

/// Opens a URL for reading (`urllib.request.urlopen`); injectable for tests.
pub trait Opener {
    /// The response body of a successful `GET url` (redirects followed). The error is the text the caller
    /// embeds into its message.
    fn open(&self, url: &str) -> std::result::Result<Box<dyn Read>, String>;
}

/// The production [`Opener`] (ureq, 60 s socket timeouts, redirects followed, system proxy honoured).
pub struct UreqOpener;

pub(crate) fn user_agent() -> String {
    format!("subtitle-engine/{}", env!("CARGO_PKG_VERSION"))
}

/// The engine's ureq agent: rustls, redirects followed, HTTP errors as `Err`, no automatic decompression.
/// The engine's ureq configuration: rustls with the operating system's certificate verifier (system root store,
/// like the apps and Python's truststore; also works behind TLS-inspecting proxies and antivirus).
pub fn config_builder() -> ureq::config::ConfigBuilder<ureq::typestate::AgentScope> {
    ureq::Agent::config_builder().tls_config(
        ureq::tls::TlsConfig::builder().root_certs(ureq::tls::RootCerts::PlatformVerifier).build(),
    )
}

pub(crate) fn agent(timeout: Option<Duration>) -> ureq::Agent {
    config_builder()
        .user_agent(user_agent())
        .accept_encoding("identity")
        .timeout_connect(timeout)
        .timeout_recv_response(timeout)
        .timeout_recv_body(None)
        .timeout_per_read(timeout)
        .build()
        .new_agent()
}

/// A ureq error as display text (status errors read like urllib's `HTTP Error 404: Not Found`).
pub(crate) fn error_text(err: &ureq::Error) -> String {
    match err {
        ureq::Error::StatusCode(code) => {
            let reason = ureq::http::StatusCode::from_u16(*code).ok().and_then(|s| s.canonical_reason()).unwrap_or("");
            format!("HTTP Error {code}: {reason}")
        }
        other => other.to_string(),
    }
}

impl Opener for UreqOpener {
    fn open(&self, url: &str) -> std::result::Result<Box<dyn Read>, String> {
        let response = agent(Some(SOCKET_TIMEOUT)).get(url).call().map_err(|e| error_text(&e))?;
        Ok(Box::new(response.into_body().into_reader()))
    }
}

/// Streams the body of `url` (from `opener`) into `dest` in 256 KiB chunks, calling `on_bytes(total_so_far)`
/// after every chunk; returns the lowercase hex SHA-256 of the body. Errors are `EngineError`s of kind
/// `Network` (open/read) or `Os` (write) whose text is what Python would put into `{exc}`.
pub fn download_to_file_with(opener: &dyn Opener, url: &str, dest: &Path, on_bytes: &mut dyn FnMut(u64)) -> Result<String> {
    let mut reader = opener.open(url).map_err(|e| EngineError::new(ErrorKind::Network, e))?;
    let mut out = File::create(dest).map_err(|e| EngineError::os(&e, Some(dest)))?;
    let mut digest = Sha256::new();
    let mut buf = vec![0u8; CHUNK_SIZE];
    let mut total = 0u64;
    loop {
        let n = read_full(&mut *reader, &mut buf).map_err(|e| EngineError::new(ErrorKind::Network, e.to_string()))?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n]).map_err(|e| EngineError::os(&e, None))?;
        digest.update(&buf[..n]);
        total += n as u64;
        on_bytes(total);
    }
    out.flush().map_err(|e| EngineError::os(&e, None))?;
    Ok(hex::encode(digest.finalize()))
}

/// Reads up to `buf.len()` bytes (like `resp.read(n)`, which fills the buffer unless the body ends).
pub(crate) fn read_full(reader: &mut dyn Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

/// Streams `url` to `dest` (following redirects, 60 s socket timeout), calling `on_bytes(total_so_far)` after
/// every 256 KiB chunk, and returns the lowercase hex SHA-256 of the body.
pub fn download_to_file(url: &str, dest: &Path, on_bytes: &mut dyn FnMut(u64)) -> Result<String> {
    download_to_file_with(&UreqOpener, url, dest, on_bytes)
}

/// The throttled byte reporter of `ensure_cuda` / `ensure_llama`: `fraction = clamp(cur/total, 0, 0.99)`,
/// published only when it grew and at least `interval` passed since the last publication (the first always
/// passes).
pub struct RuntimeReporter<'a, 'b> {
    on_progress: Option<&'a mut (dyn FnMut(f64) + 'b)>,
    total: u64,
    last_fraction: f64,
    last_report: Option<std::time::Instant>,
    interval: Duration,
}

impl<'a, 'b> RuntimeReporter<'a, 'b> {
    pub fn new(on_progress: Option<&'a mut (dyn FnMut(f64) + 'b)>, total: u64, interval: Duration) -> Self {
        RuntimeReporter { on_progress, total, last_fraction: 0.0, last_report: None, interval }
    }

    pub fn report(&mut self, current: u64) {
        let Some(cb) = self.on_progress.as_mut() else { return };
        if self.total == 0 {
            return;
        }
        let fraction = (current as f64 / self.total as f64).clamp(0.0, 0.99);
        let now = std::time::Instant::now();
        let due = self.last_report.is_none_or(|t| now.duration_since(t) >= self.interval);
        if fraction > self.last_fraction && due {
            self.last_fraction = fraction;
            self.last_report = Some(now);
            cb(fraction);
        }
    }

    /// The final `on_progress(1.0)`.
    pub fn finish(&mut self) {
        if let Some(cb) = self.on_progress.as_mut() {
            cb(1.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Bytes(Vec<u8>);
    impl Opener for Bytes {
        fn open(&self, _url: &str) -> std::result::Result<Box<dyn Read>, String> {
            Ok(Box::new(std::io::Cursor::new(self.0.clone())))
        }
    }

    #[test]
    fn download_hashes_and_counts() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("x.bin");
        let data = vec![7u8; CHUNK_SIZE + 10];
        let mut seen = Vec::new();
        let sha = download_to_file_with(&Bytes(data.clone()), "u", &dest, &mut |n| seen.push(n)).unwrap();
        assert_eq!(seen, vec![CHUNK_SIZE as u64, CHUNK_SIZE as u64 + 10]);
        assert_eq!(sha, hex::encode(Sha256::digest(&data)));
        assert_eq!(std::fs::read(&dest).unwrap(), data);
    }
}
