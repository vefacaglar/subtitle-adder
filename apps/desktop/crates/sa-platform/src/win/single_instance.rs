//! Single instance (win-ui-main §2; C# `SingleInstanceCoordinator`): named mutex
//! `Local\SubtitleAdder.SingleInstance.<user>` + pipe `\\.\pipe\SubtitleAdder.SingleInstance.<user>`. A launch with
//! files forwards them to the owner (1200 ms) and exits; a plain launch is an independent process; only the mutex owner
//! runs the pipe server. Names and the wire protocol are byte-exact so a C# instance and the Rust app interoperate.
//!
//! Wire: the client writes each path followed by `\r\n`, UTF-8 without BOM, then closes. The server reads to EOF,
//! keeps lines (trimmed, `"` trimmed) that name existing files as full paths and calls the sink even when the list is
//! empty (the app then brings its window to the front).

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use sa_core::platform::{ExternalOpenSource, InstanceRole};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, ERROR_BROKEN_PIPE, ERROR_FILE_NOT_FOUND, ERROR_IO_PENDING, ERROR_PIPE_BUSY,
    ERROR_PIPE_CONNECTED, GENERIC_WRITE, GetLastError, HANDLE, HLOCAL, INVALID_HANDLE_VALUE, LocalFree, WAIT_ABANDONED,
    WAIT_OBJECT_0,
};
use windows::Win32::Security::Authorization::{ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
use windows::Win32::Security::{GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_OVERLAPPED, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_NONE, OPEN_EXISTING, PIPE_ACCESS_INBOUND, ReadFile,
    WriteFile,
};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES,
    WaitNamedPipeW,
};
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, GetCurrentProcess, OpenProcessToken, ReleaseMutex, SetEvent, WaitForMultipleObjects,
    WaitForSingleObject,
};
use windows::core::PWSTR;

use super::wide::Wide;

/// Client connect timeout (`NamedPipeClientStream.Connect(1200)`).
pub const FORWARD_TIMEOUT: Duration = Duration::from_millis(1200);

/// C# `SanitizeToken`: every UTF-16 unit that is not a letter or decimal digit (`char.IsLetterOrDigit`: general
/// categories L* and Nd) becomes `_` (a non-BMP character is two units, so two `_`); blank → `default`.
pub fn sanitize_token(raw: &str) -> String {
    static LETTER_OR_DIGIT: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"^[\p{L}\p{Nd}]$").expect("category regex"));
    if raw.trim().is_empty() {
        return "default".into();
    }
    let mut out = String::with_capacity(raw.len());
    let mut buf = [0u8; 4];
    for c in raw.chars() {
        if (c as u32) > 0xFFFF {
            out.push_str("__");
        } else if LETTER_OR_DIGIT.is_match(c.encode_utf8(&mut buf)) {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    out
}

/// Mutex and pipe names for a (raw) user name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceNames {
    pub mutex: String,
    pub pipe: String,
}

impl InstanceNames {
    pub fn for_user(user: &str) -> Self {
        let safe = sanitize_token(user);
        Self { mutex: format!("Local\\SubtitleAdder.SingleInstance.{safe}"), pipe: format!("SubtitleAdder.SingleInstance.{safe}") }
    }
    pub fn pipe_path(&self) -> String {
        format!(r"\\.\pipe\{}", self.pipe)
    }
}

/// .NET `StreamReader.ReadLine` over the whole payload: `\r\n`, `\n` or `\r` end a line; a BOM is skipped; invalid
/// UTF-8 becomes U+FFFD. Each line is trimmed (whitespace, then `"`); empty lines are dropped.
pub fn parse_lines(bytes: &[u8]) -> Vec<String> {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    let text = String::from_utf8_lossy(bytes);
    let mut out = Vec::new();
    let mut rest: &str = &text;
    while !rest.is_empty() {
        let (line, next) = match rest.find(['\r', '\n']) {
            Some(i) => {
                let skip = if rest[i..].starts_with("\r\n") { 2 } else { 1 };
                (&rest[..i], &rest[i + skip..])
            }
            None => (rest, ""),
        };
        let t = line.trim().trim_matches('"');
        if !t.is_empty() {
            out.push(t.to_owned());
        }
        rest = next;
    }
    out
}

/// Keeps entries that name existing files (directories dropped), as full paths; order and duplicates kept.
pub fn existing_full_paths<S: AsRef<str>>(entries: &[S]) -> Vec<PathBuf> {
    entries
        .iter()
        .map(|e| e.as_ref())
        .filter(|e| !e.is_empty() && std::path::Path::new(e).is_file())
        .map(|e| PathBuf::from(super::folders::full_path(e).unwrap_or_else(|| e.to_owned())))
        .collect()
}

/// Step 1.1.2: `args[1..]` trimmed (whitespace, then `"`), existing files only, full paths.
pub fn command_line_paths(args: impl IntoIterator<Item = OsString>) -> Vec<PathBuf> {
    let entries: Vec<String> =
        args.into_iter().skip(1).map(|a| a.to_string_lossy().trim().trim_matches('"').to_owned()).collect();
    existing_full_paths(&entries)
}

/// The payload a client writes (`WriteLine` per path).
pub fn encode_paths(paths: &[PathBuf]) -> Vec<u8> {
    let mut s = String::new();
    for p in paths {
        s.push_str(&p.to_string_lossy());
        s.push_str("\r\n");
    }
    s.into_bytes()
}

#[derive(Clone, Copy)]
struct SendHandle(HANDLE);
// SAFETY: kernel handles may be used from any thread.
unsafe impl Send for SendHandle {}
unsafe impl Sync for SendHandle {}

type Sink = Box<dyn FnMut(Vec<PathBuf>) + Send>;

#[derive(Default)]
struct SinkState {
    sink: Option<Sink>,
    waiting: Vec<Vec<PathBuf>>,
}

fn deliver(state: &Mutex<SinkState>, paths: Vec<PathBuf>) {
    let mut st = state.lock();
    match st.sink.as_mut() {
        Some(sink) => sink(paths),
        None => st.waiting.push(paths),
    }
}

struct Server {
    cancel: SendHandle,
    thread: Option<std::thread::JoinHandle<()>>,
}

pub struct SingleInstanceCoordinator {
    names: InstanceNames,
    mutex: Option<SendHandle>,
    server: Option<Server>,
    state: Arc<Mutex<SinkState>>,
    disposed: bool,
}

impl SingleInstanceCoordinator {
    /// Names for the current Windows user (`Environment.UserName`).
    pub fn new() -> Self {
        Self::with_names(InstanceNames::for_user(&super::folders::user_name()))
    }

    pub fn with_names(names: InstanceNames) -> Self {
        Self { names, mutex: None, server: None, state: Arc::default(), disposed: false }
    }

    pub fn names(&self) -> &InstanceNames {
        &self.names
    }

    pub fn mutex_name(user: &str) -> String {
        InstanceNames::for_user(user).mutex
    }

    /// Whether this process owns the mutex (and runs the pipe server).
    pub fn is_owner(&self) -> bool {
        self.mutex.is_some()
    }

    /// Sends `paths` to the instance listening on `names` (1200 ms connect timeout); `false` on any error.
    pub fn forward(names: &InstanceNames, paths: &[PathBuf]) -> bool {
        let path = Wide::new(&names.pipe_path());
        let deadline = Instant::now() + FORWARD_TIMEOUT;
        let handle = loop {
            let r = unsafe {
                CreateFileW(path.pcwstr(), GENERIC_WRITE.0, FILE_SHARE_NONE, None, OPEN_EXISTING, FILE_FLAGS_AND_ATTRIBUTES(0), None)
            };
            match r {
                Ok(h) => break h,
                Err(_) => {
                    let err = unsafe { GetLastError() };
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return false;
                    }
                    if err == ERROR_PIPE_BUSY {
                        unsafe {
                            let _ = WaitNamedPipeW(path.pcwstr(), left.as_millis().max(1) as u32);
                        }
                    } else if err == ERROR_FILE_NOT_FOUND {
                        // No server instance yet (it is between connections): retry until the timeout, as .NET does.
                        std::thread::sleep(Duration::from_millis(10).min(left));
                    } else {
                        return false;
                    }
                }
            }
        };
        let data = encode_paths(paths);
        let mut written = 0u32;
        let ok = unsafe { WriteFile(handle, Some(&data), Some(&mut written), None) }.is_ok() && written as usize == data.len();
        unsafe {
            let _ = CloseHandle(handle);
        }
        ok
    }

    fn start_server(&mut self) {
        let Ok(cancel) = (unsafe { CreateEventW(None, true, false, None) }) else { return };
        let cancel = SendHandle(cancel);
        let names = self.names.clone();
        let state = self.state.clone();
        let thread = std::thread::Builder::new()
            .name("sa-single-instance".into())
            .spawn(move || server_loop(&names, cancel, &state))
            .ok();
        self.server = Some(Server { cancel, thread });
    }

    /// Cancels the server loop, releases and closes the mutex (win-ui-main §2.4). Idempotent.
    pub fn dispose(&mut self) {
        if std::mem::replace(&mut self.disposed, true) {
            return;
        }
        if let Some(mut s) = self.server.take() {
            unsafe {
                let _ = SetEvent(s.cancel.0);
            }
            if let Some(t) = s.thread.take() {
                let _ = t.join();
            }
            unsafe {
                let _ = CloseHandle(s.cancel.0);
            }
        }
        if let Some(m) = self.mutex.take() {
            unsafe {
                // Fails when called from another thread than the owner; the handle close releases it then.
                let _ = ReleaseMutex(m.0);
                let _ = CloseHandle(m.0);
            }
        }
    }
}

impl Default for SingleInstanceCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for SingleInstanceCoordinator {
    fn drop(&mut self) {
        self.dispose();
    }
}

impl ExternalOpenSource for SingleInstanceCoordinator {
    fn acquire_or_forward(&mut self, initial_paths: &[PathBuf]) -> InstanceRole {
        let name = Wide::new(&self.names.mutex);
        let handle = match unsafe { CreateMutexW(None, true, name.pcwstr()) } {
            Ok(h) => h,
            // A restricted sandbox: continue as an independent instance.
            Err(_) => return InstanceRole::Primary,
        };
        let created_new = unsafe { GetLastError() } != ERROR_ALREADY_EXISTS;
        let owns = created_new || {
            let w = unsafe { WaitForSingleObject(handle, 0) };
            w == WAIT_OBJECT_0 || w == WAIT_ABANDONED
        };
        if !owns {
            unsafe {
                let _ = CloseHandle(handle);
            }
            if initial_paths.is_empty() {
                return InstanceRole::Primary;
            }
            if Self::forward(&self.names, initial_paths) {
                return InstanceRole::Exit;
            }
            return InstanceRole::Primary;
        }
        self.mutex = Some(SendHandle(handle));
        self.start_server();
        InstanceRole::Primary
    }

    fn set_sink(&mut self, mut sink: Box<dyn FnMut(Vec<PathBuf>) + Send>) {
        let mut st = self.state.lock();
        for batch in std::mem::take(&mut st.waiting) {
            sink(batch);
        }
        st.sink = Some(sink);
    }
}

// ---- server -----------------------------------------------------------------------------------------------------

/// `D:P(A;;GA;;;<current user SID>)`: only this user may connect (.NET `PipeOptions.CurrentUserOnly`).
fn current_user_security() -> Option<(SECURITY_ATTRIBUTES, PSECURITY_DESCRIPTOR)> {
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).ok()?;
        let mut len = 0u32;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut len);
        let mut buf = vec![0u8; len as usize];
        let ok = GetTokenInformation(token, TokenUser, Some(buf.as_mut_ptr().cast()), len, &mut len).is_ok();
        let _ = CloseHandle(token);
        if !ok {
            return None;
        }
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut sid_str = PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut sid_str).ok()?;
        let sid = sid_str.to_string().ok();
        let _ = LocalFree(Some(HLOCAL(sid_str.0.cast())));
        let sddl = Wide::new(&format!("D:P(A;;GA;;;{})", sid?));
        let mut sd = PSECURITY_DESCRIPTOR::default();
        ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.pcwstr(), SDDL_REVISION_1, &mut sd, None).ok()?;
        let sa = SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: sd.0, bInheritHandle: false.into() };
        Some((sa, sd))
    }
}

enum Wait {
    Done,
    Cancelled,
}

/// Waits for an overlapped operation or the cancel event.
fn wait_io(pipe: HANDLE, ov: &OVERLAPPED, cancel: HANDLE) -> Wait {
    let w = unsafe { WaitForMultipleObjects(&[ov.hEvent, cancel], false, u32::MAX) };
    if w == WAIT_OBJECT_0 {
        Wait::Done
    } else {
        unsafe {
            let _ = CancelIoEx(pipe, Some(ov));
            let mut n = 0;
            let _ = GetOverlappedResult(pipe, ov, &mut n, true);
        }
        Wait::Cancelled
    }
}

fn is_cancelled(cancel: HANDLE) -> bool {
    unsafe { WaitForSingleObject(cancel, 0) == WAIT_OBJECT_0 }
}

/// One connection: `Ok(Some(bytes))`, `Ok(None)` when cancelled, `Err` on an IO error.
fn serve_once(pipe: HANDLE, cancel: HANDLE) -> Result<Option<Vec<u8>>, ()> {
    let event = unsafe { CreateEventW(None, true, false, None) }.map_err(|_| ())?;
    let result = (|| {
        let mut ov = OVERLAPPED { hEvent: event, ..Default::default() };
        match unsafe { ConnectNamedPipe(pipe, Some(&mut ov)) } {
            Ok(()) => {}
            Err(_) => {
                let e = unsafe { GetLastError() };
                if e == ERROR_IO_PENDING {
                    if let Wait::Cancelled = wait_io(pipe, &ov, cancel) {
                        return Ok(None);
                    }
                    let mut n = 0;
                    unsafe { GetOverlappedResult(pipe, &ov, &mut n, false) }.map_err(|_| ())?;
                } else if e != ERROR_PIPE_CONNECTED {
                    return Err(());
                }
            }
        }
        let mut data = Vec::new();
        let mut buf = vec![0u8; 4096];
        loop {
            let mut ov = OVERLAPPED { hEvent: event, ..Default::default() };
            let mut n = 0u32;
            let r = unsafe { ReadFile(pipe, Some(&mut buf), Some(&mut n), Some(&mut ov)) };
            if r.is_err() {
                let e = unsafe { GetLastError() };
                if e == ERROR_BROKEN_PIPE {
                    break;
                }
                if e != ERROR_IO_PENDING {
                    return Err(());
                }
                if let Wait::Cancelled = wait_io(pipe, &ov, cancel) {
                    return Ok(None);
                }
                if unsafe { GetOverlappedResult(pipe, &ov, &mut n, false) }.is_err() {
                    if unsafe { GetLastError() } == ERROR_BROKEN_PIPE {
                        break;
                    }
                    return Err(());
                }
            } else {
                // Completed synchronously; the count is also in the overlapped result.
                let _ = unsafe { GetOverlappedResult(pipe, &ov, &mut n, false) };
            }
            if n == 0 {
                // A zero-byte message on a byte pipe: keep reading until the client closes.
                continue;
            }
            data.extend_from_slice(&buf[..n as usize]);
        }
        Ok(Some(data))
    })();
    unsafe {
        let _ = CloseHandle(event);
    }
    result
}

fn server_loop(names: &InstanceNames, cancel: SendHandle, state: &Mutex<SinkState>) {
    let cancel = cancel.0;
    let path = Wide::new(&names.pipe_path());
    let security = current_user_security();
    while !is_cancelled(cancel) {
        let sa = security.as_ref().map(|(sa, _)| sa as *const SECURITY_ATTRIBUTES);
        let pipe = unsafe {
            CreateNamedPipeW(
                path.pcwstr(),
                PIPE_ACCESS_INBOUND | FILE_FLAG_OVERLAPPED,
                PIPE_TYPE_BYTE | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                0,
                0,
                0,
                sa,
            )
        };
        let outcome = if pipe == INVALID_HANDLE_VALUE || pipe.is_invalid() { Err(()) } else { serve_once(pipe, cancel) };
        if !(pipe == INVALID_HANDLE_VALUE || pipe.is_invalid()) {
            unsafe {
                let _ = DisconnectNamedPipe(pipe);
                let _ = CloseHandle(pipe);
            }
        }
        match outcome {
            Ok(Some(bytes)) => deliver(state, existing_full_paths(&parse_lines(&bytes))),
            Ok(None) => break,
            Err(()) => {
                // Non-cancel error: wait 200 ms (or the cancel) and loop.
                if unsafe { WaitForSingleObject(cancel, 200) } == WAIT_OBJECT_0 {
                    break;
                }
            }
        }
    }
    if let Some((_, sd)) = security {
        unsafe {
            let _ = LocalFree(Some(HLOCAL(sd.0)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_names() -> InstanceNames {
        let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        InstanceNames::for_user(&format!("sa test {}-{n}", std::process::id()))
    }

    #[test]
    fn names_are_byte_exact_with_the_csharp_app() {
        assert_eq!(sanitize_token("Vefa Çağlar"), "Vefa_Çağlar");
        assert_eq!(sanitize_token("first.last@corp"), "first_last_corp");
        assert_eq!(sanitize_token("   "), "default");
        assert_eq!(sanitize_token(""), "default");
        assert_eq!(sanitize_token("x²y Ⅻ ٣"), "x_y___٣");
        assert_eq!(sanitize_token("हिन्दी"), "ह_न_द_");
        assert_eq!(sanitize_token("a😀b"), "a__b");
        let n = InstanceNames::for_user("User");
        assert_eq!(n.mutex, r"Local\SubtitleAdder.SingleInstance.User");
        assert_eq!(n.pipe, "SubtitleAdder.SingleInstance.User");
        assert_eq!(n.pipe_path(), r"\\.\pipe\SubtitleAdder.SingleInstance.User");
        assert_eq!(SingleInstanceCoordinator::mutex_name("a-b"), r"Local\SubtitleAdder.SingleInstance.a_b");
    }

    #[test]
    fn pipe_lines_parse_like_stream_reader() {
        let bytes = b"\xEF\xBB\xBF  \"C:\\a b.mp4\"  \r\nC:\\c.srt\n\r\n\rD:\\x\rlast";
        assert_eq!(parse_lines(bytes), vec![r"C:\a b.mp4", r"C:\c.srt", r"D:\x", "last"]);
        assert!(parse_lines(b"").is_empty());
        assert_eq!(encode_paths(&[PathBuf::from(r"C:\a.mp4"), PathBuf::from(r"C:\ü.srt")]), "C:\\a.mp4\r\nC:\\ü.srt\r\n".as_bytes());
    }

    #[test]
    fn command_line_keeps_existing_files_only_as_full_paths() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("film.mp4");
        std::fs::write(&f, b"x").unwrap();
        let args = vec![
            OsString::from("SubtitleAdder.exe"),
            OsString::from(format!("  \"{}\" ", f.display())),
            OsString::from(dir.path()),
            OsString::from(r"C:\missing\x.mp4"),
            OsString::from(f.as_os_str()),
        ];
        assert_eq!(command_line_paths(args), vec![f.clone(), f]);
    }

    #[test]
    fn second_launch_with_files_forwards_to_the_owner_and_plain_launch_runs_independently() {
        let names = test_names();
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.mp4");
        let b = dir.path().join("b.srt");
        std::fs::write(&a, b"x").unwrap();
        std::fs::write(&b, b"x").unwrap();

        let mut owner = SingleInstanceCoordinator::with_names(names.clone());
        assert_eq!(owner.acquire_or_forward(&[]), InstanceRole::Primary);
        assert!(owner.is_owner());
        let (tx, rx) = std::sync::mpsc::channel();
        owner.set_sink(Box::new(move |p| {
            let _ = tx.send(p);
        }));

        // The mutex is held by another thread here, as in another process.
        let names2 = names.clone();
        let a2 = a.clone();
        let missing = dir.path().join("missing.mp4");
        let roles = std::thread::spawn(move || {
            let mut plain = SingleInstanceCoordinator::with_names(names2.clone());
            let plain_role = plain.acquire_or_forward(&[]);
            let plain_owner = plain.is_owner();
            let mut second = SingleInstanceCoordinator::with_names(names2);
            let role = second.acquire_or_forward(&[a2, missing]);
            (plain_role, plain_owner, role, second.is_owner())
        })
        .join()
        .unwrap();
        assert_eq!(roles, (InstanceRole::Primary, false, InstanceRole::Exit, false));
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), vec![a.clone()]);

        // A raw client (the C# wire form) with an empty payload still calls the sink.
        assert!(SingleInstanceCoordinator::forward(&names, &[]));
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), Vec::<PathBuf>::new());
        assert!(SingleInstanceCoordinator::forward(&names, &[b.clone(), a.clone()]));
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), vec![b, a.clone()]);

        owner.dispose();
        // After dispose nobody listens: forwarding fails within the timeout and the next launch becomes the owner.
        let t0 = Instant::now();
        assert!(!SingleInstanceCoordinator::forward(&names, &[a]));
        assert!(t0.elapsed() < Duration::from_secs(3));
        let names3 = names.clone();
        let owner_again = std::thread::spawn(move || {
            let mut c = SingleInstanceCoordinator::with_names(names3);
            let r = c.acquire_or_forward(&[]);
            (r, c.is_owner())
        })
        .join()
        .unwrap();
        assert_eq!(owner_again, (InstanceRole::Primary, true));
    }

    #[test]
    fn paths_received_before_the_sink_is_set_are_replayed() {
        let names = test_names();
        let mut owner = SingleInstanceCoordinator::with_names(names.clone());
        assert_eq!(owner.acquire_or_forward(&[]), InstanceRole::Primary);
        assert!(SingleInstanceCoordinator::forward(&names, &[]));
        std::thread::sleep(Duration::from_millis(300));
        let got = Arc::new(Mutex::new(Vec::new()));
        let g = got.clone();
        owner.set_sink(Box::new(move |p| g.lock().push(p)));
        assert_eq!(got.lock().len(), 1);
        owner.dispose();
    }
}
