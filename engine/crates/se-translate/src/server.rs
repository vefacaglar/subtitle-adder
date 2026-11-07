//! The local `llama-server` process (spec translate-llm.md §10): start, `/health` wait, GPU→CPU fallback,
//! termination.
//!
//! The server holds gigabytes of (V)RAM, so it must not outlive the engine even when the engine does not get to
//! run [`LlamaServer::stop`] (the macOS app sends SIGTERM, a terminal sends SIGINT, the Windows app terminates
//! the process): see [`install_process_guards`] and the `guard` module.

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
    /// The signal-cleanup registration (unix), released once the process is reaped.
    guard: guard::Registration,
}

impl LlamaServer {
    /// `Popen.poll()`: the exit status once the process has exited (reaped). The signal-cleanup registration
    /// is released then, so a reused pid is never killed.
    fn poll(&mut self) -> Option<std::io::Result<std::process::ExitStatus>> {
        let status = match self.child.try_wait() {
            Ok(None) => return None,
            Ok(Some(status)) => Ok(status),
            Err(e) => Err(e),
        };
        self.guard.release();
        Some(status)
    }

    /// Has the process exited?
    pub fn exited(&mut self) -> bool {
        self.poll().is_some()
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
        if self.poll().is_none() {
            // Windows: TerminateProcess; POSIX: SIGKILL (std has no SIGTERM; documented deviation).
            let _ = self.child.kill();
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                if self.poll().is_some() {
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
    let guard = guard::adopt(&child);
    let mut server = LlamaServer { child, stderr_file, port, guard };

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

/// Process-wide guards the engine binary installs once at startup, before any child process is spawned:
///
/// * unix: SIGTERM, SIGINT and SIGHUP (only those still at their default disposition, as Python leaves an
///   ignored SIGINT ignored) kill every running `llama-server` and then terminate the engine with the same
///   signal, so the exit status is what Python gives (SIGTERM/SIGHUP: killed by the signal; SIGINT: an uncaught
///   `KeyboardInterrupt`, which Python ≥3.8 turns into death by SIGINT).
/// * Windows: the engine's standard handles are made non-inheritable, so `llama-server` (spawned with
///   `bInheritHandles=TRUE` by std) does not hold the app's stdout/stderr pipes open after the engine is gone.
///   Children that use `Stdio::inherit` are unaffected: std duplicates the handle as inheritable for them. The
///   server itself is placed in a kill-on-close Job Object when it is started ([`start_server`]), which covers
///   Ctrl-C, console close and `TerminateProcess` alike.
pub fn install_process_guards() {
    guard::install();
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

/// Keeps `llama-server` from outliving the engine (see [`install_process_guards`]).
mod guard {
    use std::process::Child;

    #[cfg(unix)]
    mod imp {
        use std::process::Child;
        use std::sync::atomic::{AtomicI32, Ordering};

        /// Running servers' pids (0 = free slot). The signal handler only reads these atomics and calls
        /// async-signal-safe functions (`kill`, `signal`, `raise`).
        const SLOTS: usize = 8;
        static PIDS: [AtomicI32; SLOTS] = [const { AtomicI32::new(0) }; SLOTS];

        pub struct Registration(Option<usize>);

        impl Registration {
            pub fn release(&mut self) {
                if let Some(slot) = self.0.take() {
                    PIDS[slot].store(0, Ordering::SeqCst);
                }
            }
        }

        pub fn adopt(child: &Child) -> Registration {
            let Ok(pid) = i32::try_from(child.id()) else { return Registration(None) };
            for (slot, cell) in PIDS.iter().enumerate() {
                if cell.compare_exchange(0, pid, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
                    return Registration(Some(slot));
                }
            }
            Registration(None)
        }

        extern "C" fn on_signal(sig: libc::c_int) {
            for cell in &PIDS {
                let pid = cell.load(Ordering::SeqCst);
                if pid > 0 {
                    // SAFETY: async-signal-safe; the pid belongs to a child that has not been reaped yet.
                    unsafe { libc::kill(pid, libc::SIGKILL) };
                }
            }
            // Die by the same signal: restore the default action and re-raise (delivered once this handler
            // returns, because the signal is blocked while it runs).
            // SAFETY: both calls are async-signal-safe.
            unsafe {
                libc::signal(sig, libc::SIG_DFL);
                libc::raise(sig);
            }
        }

        pub fn install() {
            for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
                // SAFETY: plain sigaction calls with zero-initialized, fully set structures.
                unsafe {
                    let mut old: libc::sigaction = std::mem::zeroed();
                    if libc::sigaction(sig, std::ptr::null(), &mut old) != 0 || old.sa_sigaction != libc::SIG_DFL {
                        continue;
                    }
                    let mut action: libc::sigaction = std::mem::zeroed();
                    action.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
                    libc::sigemptyset(&mut action.sa_mask);
                    action.sa_flags = libc::SA_RESTART;
                    libc::sigaction(sig, &action, std::ptr::null_mut());
                }
            }
        }
    }

    #[cfg(windows)]
    mod imp {
        use std::os::windows::io::AsRawHandle;
        use std::process::Child;
        use std::sync::OnceLock;

        use windows_sys::Win32::Foundation::{
            CloseHandle, HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation,
        };
        use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE};
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject,
        };

        pub struct Registration;

        impl Registration {
            pub fn release(&mut self) {}
        }

        /// A new Job Object whose processes are killed when its last handle closes, or `None`.
        pub fn kill_on_close_job() -> Option<HANDLE> {
            // SAFETY: plain Win32 calls; the info structure is zero-initialized and fully sized.
            unsafe {
                let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if job.is_null() {
                    return None;
                }
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let ok = SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                if ok == 0 {
                    CloseHandle(job);
                    return None;
                }
                Some(job)
            }
        }

        /// The engine's job, created on first use. Its handle is never closed: the kernel closes it when the
        /// engine exits in any way (normal exit, Ctrl-C, console close, `TerminateProcess`), killing the servers.
        fn engine_job() -> Option<HANDLE> {
            static JOB: OnceLock<usize> = OnceLock::new();
            let job = *JOB.get_or_init(|| kill_on_close_job().map_or(0, |h| h as usize));
            (job != 0).then_some(job as HANDLE)
        }

        pub fn assign(job: HANDLE, child: &Child) -> bool {
            // SAFETY: both handles are valid for the duration of the call.
            unsafe { AssignProcessToJobObject(job, child.as_raw_handle() as HANDLE) != 0 }
        }

        pub fn adopt(child: &Child) -> Registration {
            let assigned = engine_job().is_some_and(|job| assign(job, child));
            if !assigned {
                eprintln!(
                    "llama-server could not be added to the engine's job object: {}",
                    std::io::Error::last_os_error()
                );
            }
            Registration
        }

        /// Clears `HANDLE_FLAG_INHERIT` on the engine's standard handles (see `install_process_guards`).
        pub fn install() {
            for id in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
                // SAFETY: GetStdHandle has no preconditions; SetHandleInformation only changes the flag of a
                // valid handle (errors, e.g. for a console pseudo-handle, are ignored).
                unsafe {
                    let handle = GetStdHandle(id);
                    if !handle.is_null() && handle != INVALID_HANDLE_VALUE {
                        SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0);
                    }
                }
            }
        }
    }

    #[cfg(not(any(unix, windows)))]
    mod imp {
        use std::process::Child;

        pub struct Registration;

        impl Registration {
            pub fn release(&mut self) {}
        }

        pub fn adopt(_child: &Child) -> Registration {
            Registration
        }

        pub fn install() {}
    }

    pub use imp::Registration;
    #[cfg(all(test, windows))]
    pub(super) use imp::{assign, kill_on_close_job};

    impl Drop for Registration {
        fn drop(&mut self) {
            self.release();
        }
    }

    /// Registers a freshly spawned server (unix: for the signal handler; Windows: in the engine's job).
    pub fn adopt(child: &Child) -> Registration {
        imp::adopt(child)
    }

    pub fn install() {
        imp::install();
    }
}

#[cfg(all(test, windows))]
mod tests {
    use std::os::windows::io::AsRawHandle;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use windows_sys::Win32::Foundation::{
        CloseHandle, GetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation,
    };
    use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE};

    use super::guard;

    fn long_running_child() -> std::process::Child {
        use std::os::windows::process::CommandExt;
        Command::new("ping")
            .args(["-n", "60", "127.0.0.1"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(0x0800_0000)
            .spawn()
            .expect("spawn ping")
    }

    #[test]
    fn job_kills_its_process_when_the_last_handle_closes() {
        let job = guard::kill_on_close_job().expect("job object");
        let mut child = long_running_child();
        assert!(guard::assign(job, &child), "{}", std::io::Error::last_os_error());
        assert!(child.try_wait().unwrap().is_none());
        // What the kernel does with the engine's job handle when the engine dies.
        unsafe { CloseHandle(job) };
        let deadline = Instant::now() + Duration::from_secs(5);
        while child.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "child survived the job close");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn spawned_server_is_adopted_into_the_engine_job() {
        let mut child = long_running_child();
        let _registration = guard::adopt(&child);
        let mut in_job = 0;
        // SAFETY: valid process handle; a null job handle asks about the calling process's job.
        let ok = unsafe {
            windows_sys::Win32::System::JobObjects::IsProcessInJob(
                child.as_raw_handle() as HANDLE,
                std::ptr::null_mut(),
                &mut in_job,
            )
        };
        let _ = child.kill();
        let _ = child.wait();
        assert!(ok != 0 && in_job != 0);
    }

    #[test]
    fn std_handles_stop_being_inheritable() {
        // SAFETY: GetStdHandle has no preconditions.
        let handle = unsafe { GetStdHandle(STD_ERROR_HANDLE) };
        if handle.is_null() {
            return;
        }
        // Start from an inheritable handle, as for a pipe handed down by the app.
        // SAFETY: changes only the inherit flag of a valid handle.
        if unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) } == 0 {
            return; // a console pseudo-handle: nothing to clear
        }
        guard::install();
        let mut flags = 0u32;
        // SAFETY: valid handle and out pointer.
        assert!(unsafe { GetHandleInformation(handle, &mut flags) } != 0);
        assert_eq!(flags & HANDLE_FLAG_INHERIT, 0);
        // A child with inherited stdio still works (std duplicates the handle as inheritable).
        let status = Command::new("cmd").args(["/c", "exit", "0"]).stderr(Stdio::inherit()).status().unwrap();
        assert!(status.success());
    }
}
