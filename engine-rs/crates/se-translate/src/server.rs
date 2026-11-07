//! The local `llama-server` process (spec translate-llm.md §10): start, `/health` wait, GPU→CPU fallback,
//! termination.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use se_core::{EngineError, Result, pyfmt, t};

use crate::http::{HopSender, HttpRequest, UreqTransport};

/// `_free_local_port()`: bind `127.0.0.1:0`, read the port, close (a race is acceptable).
pub fn free_local_port() -> std::io::Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    Ok(listener.local_addr()?.port())
}

/// A running `llama-server`.
pub struct LlamaServer {
    child: Child,
    stderr_file: File,
    pub port: u16,
}

impl LlamaServer {
    /// Has the process exited?
    pub fn exited(&mut self) -> bool {
        !matches!(self.child.try_wait(), Ok(None))
    }

    fn stderr_tail(&mut self) -> String {
        let mut bytes = Vec::new();
        if self.stderr_file.seek(SeekFrom::Start(0)).is_ok() {
            let _ = self.stderr_file.read_to_end(&mut bytes);
        }
        // Python reads the text-mode temp file back with universal newlines (CRLF and CR become LF).
        let text = pyfmt::universal_newlines(&String::from_utf8_lossy(&bytes));
        let stripped = pyfmt::strip(&text);
        let n = stripped.chars().count();
        stripped.chars().skip(n.saturating_sub(400)).collect()
    }

    /// `close()`: terminate, wait 3 s, kill, wait 2 s; errors suppressed.
    pub fn stop(mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            // Windows: TerminateProcess; POSIX: SIGKILL (std has no SIGTERM; documented deviation).
            let _ = self.child.kill();
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                if !matches!(self.child.try_wait(), Ok(None)) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

/// `_start_server(gpu_layers)`: launches `llama-server` and waits up to 90 s for `/health` 200. When the process
/// exits during startup with GPU layers on an `auto` device, retries once per call chain on the CPU.
pub fn start_server(server_path: &Path, model_path: &Path, gpu_layers: u32, device_auto: bool) -> Result<LlamaServer> {
    let port = free_local_port().map_err(|e| EngineError::model(t!("llama_unavailable", error = e)))?;
    let mut cmd = Command::new(server_path);
    cmd.arg("-m")
        .arg(model_path)
        .args(["--host", "127.0.0.1", "--port", &port.to_string(), "-ngl", &gpu_layers.to_string()])
        .args(["-fa", "on", "-np", "1", "-c", "4096", "--reasoning-budget", "0"]);
    if let Some(parent) = server_path.parent() {
        cmd.current_dir(parent);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    // Never a pipe for stderr without a reader (Windows pipe buffers fill and deadlock llama-server).
    let stderr_file = tempfile::tempfile().map_err(|e| EngineError::model(t!("llama_unavailable", error = os_text(&e))))?;
    let stderr_for_child =
        stderr_file.try_clone().map_err(|e| EngineError::model(t!("llama_unavailable", error = os_text(&e))))?;
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::from(stderr_for_child));
    let child = cmd.spawn().map_err(|e| EngineError::model(t!("llama_unavailable", error = os_text(&e))))?;
    let mut server = LlamaServer { child, stderr_file, port };

    let health_url = format!("http://127.0.0.1:{port}/health");
    let transport = UreqTransport::new();
    let deadline = Instant::now() + Duration::from_secs(90);
    while Instant::now() < deadline {
        if server.exited() {
            let tail = server.stderr_tail();
            // Windows exit codes are DWORDs in Python (`3221225477`, not a negative i32).
            let code = server.child.try_wait().ok().flatten().and_then(|s| py_returncode(&s));
            server.stop();
            if gpu_layers > 0 && device_auto {
                eprintln!("llama-server GPU init failed, retrying on CPU...");
                return start_server(server_path, model_path, 0, device_auto);
            }
            let error = if tail.is_empty() {
                format!("exit code {}", code.map_or_else(|| "None".to_string(), |c| c.to_string()))
            } else {
                tail
            };
            return Err(EngineError::model(t!("llama_unavailable", error = error)));
        }
        let req = HttpRequest::new("GET", &health_url, Vec::new(), None);
        match transport.send(&req, Duration::from_secs(1)) {
            Ok(resp) if resp.status == 200 => return Ok(server),
            Ok(resp) if (200..300).contains(&resp.status) => {}
            _ => std::thread::sleep(Duration::from_millis(150)),
        }
    }
    server.stop();
    Err(EngineError::model(t!("llama_unavailable", error = "timed out waiting for llama-server /health")))
}

/// `Popen.returncode`: Windows exit codes are DWORDs (`3221225477`, not a negative i32); a POSIX process killed
/// by a signal is `-signal`.
fn py_returncode(status: &std::process::ExitStatus) -> Option<i64> {
    #[cfg(windows)]
    {
        status.code().map(|c| (c as u32) as i64)
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        status.code().map(i64::from).or_else(|| status.signal().map(|s| -i64::from(s)))
    }
    #[cfg(not(any(windows, unix)))]
    {
        status.code().map(i64::from)
    }
}

fn os_text(e: &std::io::Error) -> String {
    pyfmt::winerror_text(e, None, None)
}

/// The paths a local translator needs: the GGUF file (`models.model_file`) and `llama-server`
/// (`llama.llama_server_path()`, `offline_missing_llama` when missing).
pub fn locate(model_id: &str) -> Result<(PathBuf, PathBuf)> {
    let model_path = se_models::model_file(model_id)?;
    let server = se_models::llama::llama_server_path().ok_or_else(|| EngineError::model(t!("offline_missing_llama")))?;
    Ok((model_path, server))
}
