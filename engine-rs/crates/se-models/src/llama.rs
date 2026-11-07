//! llama.cpp runtime (`llama-server`): cache location, download with SHA-256 check (port of `llama.py`,
//! spec models.md §6), and launching the server with its `/health` wait (`LlamaTranslator._start_server` /
//! `close`, spec §6.3) for `se-translate`.

use std::fs;
use std::io::{self, Read, Seek, SeekFrom};
use std::net::TcpListener;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use se_core::{EngineError, ProgressFn, Result};

use crate::http::{Opener, RuntimeReporter, UreqOpener, download_to_file_with};

/// Override of the runtime folder.
pub const LLAMA_DIR_ENV_VAR: &str = "SUBTITLE_ENGINE_LLAMA_DIR";
/// Pinned llama.cpp release.
pub const RELEASE_TAG: &str = "b12032";
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

/// A pinned release archive (`_LlamaArchive`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LlamaArchive {
    pub filename: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub size: u64,
}

pub static WINDOWS_ARCHIVE: LlamaArchive = LlamaArchive {
    filename: "llama-b12032-bin-win-vulkan-x64.zip",
    url: "https://github.com/ggml-org/llama.cpp/releases/download/b12032/llama-b12032-bin-win-vulkan-x64.zip",
    sha256: "19a8bbf269fa683ae7bc04f886c9c2a93e0655c2606f37d2b60069bf9e8bcc32",
    size: 37_523_450,
};
pub static MACOS_ARCHIVE: LlamaArchive = LlamaArchive {
    filename: "llama-b12032-bin-macos-arm64.tar.gz",
    url: "https://github.com/ggml-org/llama.cpp/releases/download/b12032/llama-b12032-bin-macos-arm64.tar.gz",
    sha256: "00468fe833eb000b78a748122f5921b6c7b07dc82331a8d0c68196c69cb26382",
    size: 16_513_842,
};
pub static LINUX_ARCHIVE: LlamaArchive = LlamaArchive {
    filename: "llama-b12032-bin-ubuntu-vulkan-x64.tar.gz",
    url: "https://github.com/ggml-org/llama.cpp/releases/download/b12032/llama-b12032-bin-ubuntu-vulkan-x64.tar.gz",
    sha256: "f75e7107f9450034c5467492c41e9ed91816e2d815fc4b802c6bd58de7d5b126",
    size: 35_669_614,
};

/// `_LLAMA_ARCHIVES.get(platform)`.
pub fn archive_for(platform: &str) -> Option<&'static LlamaArchive> {
    match platform {
        "win32" => Some(&WINDOWS_ARCHIVE),
        "darwin" => Some(&MACOS_ARCHIVE),
        "linux" => Some(&LINUX_ARCHIVE),
        _ => None,
    }
}

/// `llama_cache_dir()`: `$SUBTITLE_ENGINE_LLAMA_DIR`, else Windows `%LOCALAPPDATA%\SubtitleAdder\llama`, else
/// `$XDG_CACHE_HOME/subtitle-adder/llama` (`~/.cache` fallback, macOS too).
pub fn llama_cache_dir() -> PathBuf {
    if let Some(dir) = std::env::var(LLAMA_DIR_ENV_VAR).ok().map(|v| se_core::pyfmt::strip(&v).to_string()).filter(|v| !v.is_empty()) {
        return PathBuf::from(dir);
    }
    crate::cuda::user_cache_base().join(if cfg!(windows) { "SubtitleAdder" } else { "subtitle-adder" }).join("llama")
}

/// `_server_binary_name()`.
pub fn server_binary_name() -> &'static str {
    if cfg!(windows) { "llama-server.exe" } else { "llama-server" }
}

/// The candidate locations of the server below `root`, in search order.
pub fn server_candidates(root: &Path) -> Vec<PathBuf> {
    let exe = server_binary_name();
    let tagged = format!("llama-{RELEASE_TAG}");
    vec![
        root.join(exe),
        root.join("bin").join(exe),
        root.join(&tagged).join(exe),
        root.join(&tagged).join("bin").join(exe),
        root.join("build").join("bin").join(exe),
    ]
}

/// [`llama_server_path`] below an explicit root.
pub fn llama_server_path_in(root: &Path) -> Option<PathBuf> {
    if !root.is_dir() {
        return None;
    }
    server_candidates(root).into_iter().find(|c| c.is_file())
}

/// `llama_server_path()`: the first existing `llama-server[.exe]` among the known layouts, or `None`.
pub fn llama_server_path() -> Option<PathBuf> {
    llama_server_path_in(&llama_cache_dir())
}

/// `llama_usable() = llama_server_path().is_some()`.
pub fn llama_usable() -> bool {
    llama_server_path().is_some()
}

/// `require_llama_download()`: false when usable; strict offline → localized `ValueError`
/// `offline_missing_llama`; else whether this platform has an archive.
pub fn require_llama_download() -> Result<bool> {
    if llama_usable() {
        return Ok(false);
    }
    if se_core::offline::offline_requested() {
        return Err(EngineError::value(se_core::t!("offline_missing_llama")));
    }
    Ok(archive_for(crate::cuda::platform()).is_some())
}

/// `needs_llama_download()`: not strict offline, platform supported and not usable.
pub fn needs_llama_download() -> bool {
    !se_core::offline::offline_requested() && archive_for(crate::cuda::platform()).is_some() && !llama_usable()
}

/// `ensure_llama(on_progress)`: downloads, verifies and extracts the pinned archive; errors are localized
/// `ModelError`s (`offline_missing_llama`, `llama_unavailable`).
pub fn ensure_llama(on_progress: Option<ProgressFn<'_>>) -> Result<PathBuf> {
    let platform = crate::cuda::platform();
    ensure_llama_with(&UreqOpener, &llama_cache_dir(), platform, archive_for(platform), on_progress, PROGRESS_INTERVAL)
}

fn unavailable(error: impl std::fmt::Display) -> EngineError {
    EngineError::model(se_core::t!("llama_unavailable", error = error))
}

/// [`ensure_llama`] with injectable opener, root, platform and archive.
pub fn ensure_llama_with(
    opener: &dyn Opener,
    root: &Path,
    platform: &str,
    archive: Option<&LlamaArchive>,
    on_progress: Option<ProgressFn<'_>>,
    interval: Duration,
) -> Result<PathBuf> {
    if let Some(existing) = llama_server_path_in(root) {
        return Ok(existing);
    }
    if se_core::offline::offline_requested() {
        return Err(EngineError::model(se_core::t!("offline_missing_llama")));
    }
    let archive = archive.ok_or_else(|| unavailable(format!("unsupported platform {platform}")))?;
    let mut reporter = RuntimeReporter::new(on_progress, archive.size, interval);
    let tmp = root.join(format!(".{}.tmp", archive.filename));
    let result = (|| -> std::result::Result<(), String> {
        fs::create_dir_all(root).map_err(|e| EngineError::win(&e, Some(root)).message)?;
        let digest = download_to_file_with(opener, archive.url, &tmp, &mut |n| reporter.report(n)).map_err(|e| e.message)?;
        if digest.to_lowercase() != archive.sha256.to_lowercase() {
            return Err(format!("SHA-256 mismatch for {}", archive.filename));
        }
        extract_archive(&tmp, root).map_err(|e| e.to_string())
    })();
    let _ = fs::remove_file(&tmp);
    result.map_err(unavailable)?;
    let server = llama_server_path_in(root).ok_or_else(|| unavailable("llama-server binary not found in archive"))?;
    reporter.finish();
    Ok(server)
}

/// `zipfile.is_zipfile(path)`: an end-of-central-directory record is present (content, not extension).
pub fn is_zipfile(path: &Path) -> bool {
    let Ok(mut f) = fs::File::open(path) else { return false };
    let Ok(len) = f.seek(SeekFrom::End(0)) else { return false };
    let window = len.min(22 + 65_535);
    if window < 22 || f.seek(SeekFrom::Start(len - window)).is_err() {
        return false;
    }
    let mut buf = vec![0u8; window as usize];
    if f.read_exact(&mut buf).is_err() {
        return false;
    }
    buf.windows(4).rev().any(|w| w == b"PK\x05\x06")
}

/// `_extract_archive(path, root)`: zip (detected by content) or tar.gz with tarfile's "data" filter; then the
/// server binary gets its exec bits on POSIX.
pub fn extract_archive(path: &Path, root: &Path) -> io::Result<()> {
    if is_zipfile(path) {
        let mut zip = zip::ZipArchive::new(fs::File::open(path)?).map_err(io::Error::other)?;
        for i in 0..zip.len() {
            let mut member = zip.by_index(i).map_err(io::Error::other)?;
            // Python's zipfile strips absolute and `..` components.
            let parts = safe_zip_parts(member.name());
            if parts.is_empty() {
                continue;
            }
            let target = parts.iter().fold(root.to_path_buf(), |acc, p| acc.join(p));
            if member.is_dir() {
                fs::create_dir_all(&target)?;
                continue;
            }
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut out = fs::File::create(&target)?;
            io::copy(&mut member, &mut out)?;
            #[cfg(unix)]
            if let Some(mode) = member.unix_mode() {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&target, fs::Permissions::from_mode(mode & 0o777));
            }
        }
    } else {
        extract_tar_gz_data_filter(path, root)?;
    }
    #[cfg(unix)]
    if let Some(server) = llama_server_path_in(root) {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&server)?.permissions().mode();
        fs::set_permissions(&server, fs::Permissions::from_mode(mode | 0o111))?;
    }
    Ok(())
}

fn safe_zip_parts(name: &str) -> Vec<String> {
    name.split(['/', '\\'])
        .enumerate()
        .filter(|(i, p)| !p.is_empty() && *p != "." && *p != ".." && !(*i == 0 && p.ends_with(':')))
        .map(|(_, p)| p.replace(':', "_"))
        .collect()
}

/// `tarfile.extractall(root, filter="data")`: refuses absolute names, `..` escapes, links leaving `root` and
/// device/fifo members; strips setuid/setgid/sticky and group/other write bits.
fn extract_tar_gz_data_filter(path: &Path, root: &Path) -> io::Result<()> {
    let gz = flate2::read::GzDecoder::new(fs::File::open(path)?);
    let mut archive = tar::Archive::new(gz);
    let filter_error = |msg: String| io::Error::new(io::ErrorKind::InvalidData, msg);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let name = entry.path()?.into_owned();
        if name.is_absolute() || name.has_root() {
            return Err(filter_error(format!("member {:?} has an absolute path", name)));
        }
        let mut rel = PathBuf::new();
        for c in name.components() {
            match c {
                Component::Normal(p) => rel.push(p),
                Component::CurDir => {}
                _ => return Err(filter_error(format!("member {:?} would be extracted outside the destination", name))),
            }
        }
        let kind = entry.header().entry_type();
        if kind.is_block_special() || kind.is_character_special() || kind.is_fifo() {
            return Err(filter_error(format!("member {:?} is a special file", name)));
        }
        if kind.is_symlink() || kind.is_hard_link() {
            let link = entry.link_name()?.map(|l| l.into_owned()).unwrap_or_default();
            let base = if kind.is_symlink() { rel.parent().map(Path::to_path_buf).unwrap_or_default() } else { PathBuf::new() };
            let mut depth: i64 = base.components().count() as i64;
            if link.is_absolute() || link.has_root() {
                return Err(filter_error(format!("link {:?} is absolute", name)));
            }
            for c in link.components() {
                match c {
                    Component::ParentDir => depth -= 1,
                    Component::Normal(_) => depth += 1,
                    _ => {}
                }
                if depth < 0 {
                    return Err(filter_error(format!("link {:?} would point outside the destination", name)));
                }
            }
        }
        let target = root.join(&rel);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        entry.set_preserve_permissions(false);
        entry.unpack(&target)?;
        #[cfg(unix)]
        if kind.is_file() {
            use std::os::unix::fs::PermissionsExt;
            let mode = entry.header().mode().unwrap_or(0o644);
            let mode = (mode & 0o777) & !0o022 | 0o600;
            let _ = fs::set_permissions(&target, fs::Permissions::from_mode(mode));
        }
    }
    Ok(())
}

// --- Launching llama-server --------------------------------------------------------------------------

/// `_free_local_port()`: binds `127.0.0.1:0`, returns the port and closes the socket.
pub fn free_local_port() -> io::Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    Ok(listener.local_addr()?.port())
}

/// The `llama-server` arguments of `_start_server` (after the program).
pub fn server_args(model: &Path, port: u16, gpu_layers: u32) -> Vec<std::ffi::OsString> {
    let mut args: Vec<std::ffi::OsString> = vec!["-m".into(), model.as_os_str().to_owned()];
    for a in [
        "--host".to_string(),
        "127.0.0.1".to_string(),
        "--port".to_string(),
        port.to_string(),
        "-ngl".to_string(),
        gpu_layers.to_string(),
        "-fa".to_string(),
        "on".to_string(),
        "-np".to_string(),
        "1".to_string(),
        "-c".to_string(),
        "4096".to_string(),
        "--reasoning-budget".to_string(),
        "0".to_string(),
    ] {
        args.push(a.into());
    }
    args
}

/// Seconds `_start_server` waits for `/health`.
pub const HEALTH_DEADLINE: Duration = Duration::from_secs(90);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(1);
const HEALTH_RETRY_SLEEP: Duration = Duration::from_millis(150);

/// Outcome of one `/health` probe.
#[derive(Debug, PartialEq, Eq)]
pub enum Health {
    /// HTTP 200.
    Ready,
    /// A non-200 response below 400 (loop again without sleeping).
    NotReady,
    /// Connection error, timeout or HTTP >= 400 (sleep 0.15 s).
    Failed,
}

/// `GET http://127.0.0.1:{port}/health` with a 1 s timeout and no proxy (Python's urllib would honour
/// `HTTP_PROXY` here; a loopback request must not go through a proxy — deliberate improvement).
pub fn probe_health(port: u16) -> Health {
    let agent = crate::http::config_builder()
        .proxy(None)
        .timeout_global(Some(HEALTH_TIMEOUT))
        .http_status_as_error(false)
        .max_redirects(0)
        .max_redirects_will_error(false)
        .build()
        .new_agent();
    match agent.get(&format!("http://127.0.0.1:{port}/health")).call() {
        Ok(r) if r.status().as_u16() == 200 => Health::Ready,
        Ok(r) if r.status().as_u16() < 400 => Health::NotReady,
        _ => Health::Failed,
    }
}

/// Why waiting for the server failed.
#[derive(Debug, PartialEq, Eq)]
pub enum WaitError {
    /// The process exited before becoming healthy (`returncode`).
    Exited(Option<i64>),
    /// `deadline` passed.
    TimedOut,
}

/// The health loop of `_start_server`: until `deadline`, first `exited()` (→ `Exited`), then `probe()`.
pub fn wait_until_healthy(
    deadline: Duration,
    exited: &mut dyn FnMut() -> Option<Option<i64>>,
    probe: &mut dyn FnMut() -> Health,
) -> std::result::Result<(), WaitError> {
    let end = Instant::now() + deadline;
    while Instant::now() < end {
        if let Some(code) = exited() {
            return Err(WaitError::Exited(code));
        }
        match probe() {
            Health::Ready => return Ok(()),
            Health::NotReady => {}
            Health::Failed => std::thread::sleep(HEALTH_RETRY_SLEEP),
        }
    }
    Err(WaitError::TimedOut)
}

/// A running `llama-server` child (killed by [`LlamaServer::close`] and on drop).
pub struct LlamaServer {
    child: Option<Child>,
    stderr: Option<fs::File>,
    port: u16,
}

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

impl LlamaServer {
    /// `_start_server(gpu_layers)` without the GPU retry: spawns `server -m model ...` (cwd = the server's
    /// folder, stdout discarded, stderr to an anonymous temp file, no console window on Windows) and waits
    /// up to 90 s for `/health`. Errors are `llama_unavailable` with the stderr tail (last 400 characters),
    /// `exit code N`, the spawn error or `timed out waiting for llama-server /health`.
    pub fn start(server: &Path, model: &Path, gpu_layers: u32) -> std::result::Result<LlamaServer, StartError> {
        let port = free_local_port().map_err(|e| StartError::Failed(unavailable(EngineError::win(&e, None).message)))?;
        let stderr = tempfile::tempfile().map_err(|e| StartError::Failed(unavailable(EngineError::os(&e, None).message)))?;
        let mut cmd = Command::new(server);
        cmd.args(server_args(model, port, gpu_layers))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(stderr.try_clone().map_err(|e| StartError::Failed(unavailable(e)))?);
        if let Some(dir) = server.parent().filter(|d| !d.as_os_str().is_empty()) {
            cmd.current_dir(dir);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let child = cmd.spawn().map_err(|e| StartError::Failed(unavailable(spawn_error_text(&e, server))))?;
        let mut srv = LlamaServer { child: Some(child), stderr: Some(stderr), port };
        let outcome = {
            let child = srv.child.as_mut().expect("child");
            let mut exited = || match child.try_wait() {
                Ok(Some(status)) => Some(Some(returncode(&status))),
                Ok(None) => None,
                Err(_) => None,
            };
            wait_until_healthy(HEALTH_DEADLINE, &mut exited, &mut || probe_health(port))
        };
        match outcome {
            Ok(()) => Ok(srv),
            Err(WaitError::Exited(code)) => {
                let tail = srv.stderr_tail();
                srv.close();
                let error = if tail.is_empty() {
                    format!("exit code {}", code.unwrap_or(-1))
                } else {
                    tail
                };
                Err(StartError::Exited(error))
            }
            Err(WaitError::TimedOut) => {
                srv.close();
                Err(StartError::Failed(unavailable("timed out waiting for llama-server /health")))
            }
        }
    }

    /// `_start_server(gpu_layers)` with the GPU-init retry: an early exit with `gpu_layers > 0` and
    /// `retry_on_cpu` (device `auto`) prints `llama-server GPU init failed, retrying on CPU...` on stderr
    /// and starts again with `-ngl 0`.
    pub fn launch(server: &Path, model: &Path, gpu_layers: u32, retry_on_cpu: bool) -> Result<LlamaServer> {
        match Self::start(server, model, gpu_layers) {
            Ok(s) => Ok(s),
            Err(StartError::Exited(_)) if gpu_layers > 0 && retry_on_cpu => {
                eprintln!("llama-server GPU init failed, retrying on CPU...");
                Self::launch(server, model, 0, retry_on_cpu)
            }
            Err(StartError::Exited(tail)) => Err(unavailable(tail)),
            Err(StartError::Failed(e)) => Err(e),
        }
    }

    /// The server's port.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// `http://127.0.0.1:{port}/v1/chat/completions`.
    pub fn chat_url(&self) -> String {
        format!("http://127.0.0.1:{}/v1/chat/completions", self.port)
    }

    /// The process is still running (`poll() is None`).
    pub fn is_running(&mut self) -> bool {
        match self.child.as_mut() {
            Some(c) => matches!(c.try_wait(), Ok(None)),
            None => false,
        }
    }

    fn stderr_tail(&mut self) -> String {
        let Some(f) = self.stderr.as_mut() else { return String::new() };
        let mut bytes = Vec::new();
        if f.seek(SeekFrom::Start(0)).is_err() || f.read_to_end(&mut bytes).is_err() {
            return String::new();
        }
        let text = String::from_utf8_lossy(&bytes);
        let text = se_core::pyfmt::strip(&se_core::pyfmt::universal_newlines(&text)).to_string();
        let n = text.chars().count();
        text.chars().skip(n.saturating_sub(400)).collect()
    }

    /// `close()`: terminate (wait 3 s), then kill (wait 2 s); idempotent, errors suppressed.
    pub fn close(&mut self) {
        let child = self.child.take();
        self.stderr = None;
        if let Some(mut child) = child {
            if matches!(child.try_wait(), Ok(None)) {
                terminate(&mut child);
                if !wait_for(&mut child, Duration::from_secs(3)) {
                    let _ = child.kill();
                    wait_for(&mut child, Duration::from_secs(2));
                }
            }
        }
    }
}

impl Drop for LlamaServer {
    fn drop(&mut self) {
        self.close();
    }
}

/// A failed start: the early exit text (for the GPU retry decision) or a final error.
#[derive(Debug)]
pub enum StartError {
    /// The process exited early; the text is the stderr tail or `exit code N`.
    Exited(String),
    Failed(EngineError),
}

fn terminate(child: &mut Child) {
    #[cfg(unix)]
    {
        unsafe extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        // SAFETY: plain syscall on our own child's pid.
        unsafe { kill(child.id() as i32, 15) };
    }
    #[cfg(not(unix))]
    {
        // Windows `terminate()` is TerminateProcess, which is what `kill()` does.
        let _ = child.kill();
    }
}

fn wait_for(child: &mut Child, timeout: Duration) -> bool {
    let end = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) if Instant::now() < end => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => return false,
            Err(_) => return true,
        }
    }
}

/// Python `Popen.returncode`: the exit code, or `-signal` on POSIX.
fn returncode(status: &std::process::ExitStatus) -> i64 {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return -i64::from(sig);
        }
    }
    #[cfg(windows)]
    {
        // GetExitCodeProcess returns a DWORD; Python shows it unsigned (an access violation is 3221225477).
        status.code().map(|c| c as u32 as i64).unwrap_or(-1)
    }
    #[cfg(not(windows))]
    {
        status.code().map(i64::from).unwrap_or(-1)
    }
}

/// `str(OSError)` of a failed `Popen` (Windows: `[WinError 2] ...` without a file name; POSIX:
/// `[Errno 2] No such file or directory: 'path'`).
fn spawn_error_text(err: &io::Error, server: &Path) -> String {
    if cfg!(windows) { EngineError::win(err, None).message } else { EngineError::os(err, Some(server)).message }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn returncode_is_unsigned_on_windows_like_python() {
        let status = Command::new("cmd").args(["/c", "exit", "-1073741819"]).status().unwrap();
        assert_eq!(returncode(&status), 3_221_225_477);
        let status = Command::new("cmd").args(["/c", "exit", "3"]).status().unwrap();
        assert_eq!(returncode(&status), 3);
    }

    #[cfg(unix)]
    #[test]
    fn returncode_is_minus_signal_on_posix() {
        let status = Command::new("sh").args(["-c", "kill -9 $$"]).status().unwrap();
        assert_eq!(returncode(&status), -9);
    }
}
