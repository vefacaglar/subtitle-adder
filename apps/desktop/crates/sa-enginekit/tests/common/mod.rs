//! Test support (port of the C# `Fake` helpers and the Swift `fakeEngine`): every engine is the `sa-fake-engine`
//! test binary driven by a script file written to a temp folder.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use sa_core::diagnostics::DiagnosticSink;
use sa_core::engine_types::EngineError;
use sa_core::l10n::UiLanguage;
use sa_enginekit::events::EngineEvent;
use sa_enginekit::launch::EngineLaunchSettings;
use sa_enginekit::{EngineCommand, EventStream, ProcessEngineClient};

pub const FAKE_ENGINE: &str = env!("CARGO_BIN_EXE_sa-fake-engine");

/// Serializes tests that change process-wide state (`LanguageState`, `EngineLaunchState`).
pub static GLOBAL_STATE: Mutex<()> = Mutex::new(());

/// Records diagnostic lines.
#[derive(Default)]
pub struct RecordingSink {
    pub lines: Mutex<Vec<(String, String)>>,
}

impl DiagnosticSink for RecordingSink {
    fn write(&self, category: &str, message: &str) {
        self.lines.lock().push((category.to_owned(), message.to_owned()));
    }
}

/// A fake engine plus the temp folder holding its script and the files it writes.
pub struct Fake {
    pub dir: tempfile::TempDir,
    pub client: ProcessEngineClient,
    pub sink: Arc<RecordingSink>,
}

impl Fake {
    /// A client running `script` (LF line endings) with explicit language and launch settings (offline on, no
    /// external LLM) so tests never depend on the shared state.
    pub fn new(script: &str, language: UiLanguage) -> Fake {
        let dir = tempfile::tempdir().expect("temp dir");
        let script_path = dir.path().join("script.txt");
        std::fs::write(&script_path, script.replace("\r\n", "\n")).expect("script");
        let sink = Arc::new(RecordingSink::default());
        let command = EngineCommand::with_arguments(FAKE_ENGINE, vec![script_path.to_string_lossy().into_owned()]);
        let mut client =
            ProcessEngineClient::new(command, sa_enginekit::process::platform_process_control(), sink.clone() as Arc<dyn DiagnosticSink>);
        client.language = Some(language);
        client.launch_settings = Some(EngineLaunchSettings { offline: true, external_llm: None });
        Fake { dir, client, sink }
    }

    pub fn tr(script: &str) -> Fake {
        Self::new(script, UiLanguage::Tr)
    }

    pub fn en(script: &str) -> Fake {
        Self::new(script, UiLanguage::En)
    }

    /// A path inside the temp folder, in the script's spelling (forward slashes are fine on Windows).
    pub fn path(&self, name: &str) -> String {
        self.dir.path().join(name).to_string_lossy().into_owned()
    }

    pub fn out_dir(&self) -> PathBuf {
        self.dir.path().to_path_buf()
    }
}

/// Reads a file the fake engine wrote.
pub fn read(path: &str) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// Polls for a PID file (200 × 50 ms).
pub fn read_pid(path: &str) -> u32 {
    for _ in 0..200 {
        if let Ok(text) = std::fs::read_to_string(path)
            && let Ok(pid) = text.trim().parse()
        {
            return pid;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("no pid in {path}");
}

#[cfg(windows)]
pub fn is_alive(pid: u32) -> bool {
    use windows::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    // SAFETY: plain Win32 calls; the handle is closed before returning.
    unsafe {
        let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return false };
        let mut code = 0u32;
        let ok = GetExitCodeProcess(handle, &mut code).is_ok();
        let _ = CloseHandle(handle);
        ok && code == STILL_ACTIVE.0 as u32
    }
}

#[cfg(unix)]
pub fn is_alive(pid: u32) -> bool {
    // SAFETY: signal 0 only checks for existence.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

/// Polls until the process is gone (60 × 50 ms); true when it died.
pub fn wait_until_dead(pid: u32) -> bool {
    for _ in 0..60 {
        if !is_alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    !is_alive(pid)
}

/// Collects every event; the terminal error, if any, is returned separately.
pub fn collect(stream: EventStream) -> (Vec<EngineEvent>, Option<EngineError>) {
    let mut events = Vec::new();
    for item in stream {
        match item {
            Ok(e) => events.push(e),
            Err(e) => return (events, Some(e)),
        }
    }
    (events, None)
}

/// Drains a stream and returns its terminal error.
pub fn drain_error(stream: EventStream) -> EngineError {
    collect(stream).1.expect("the stream ends with an error")
}

/// A file path with forward slashes kept as typed (used for argv assertions).
pub fn p(s: &str) -> &Path {
    Path::new(s)
}
