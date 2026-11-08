//! Platform process control (UNIFIED.md §5.9, §3.7 "Engine termination").
//!
//! * Windows: every engine run gets its own Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`; termination is
//!   `TerminateJobObject` (the engine and every descendant such as `llama-server`), and closing the job handle (the
//!   [`ChildHandle`] is dropped, or the app crashes) kills whatever is left. Engines start with `CREATE_NO_WINDOW`.
//! * Unix (macOS): `kill(pid, SIGTERM)` only, never SIGKILL (the engine stops `llama-server` itself on SIGTERM), and
//!   never for a pid that was already reaped: [`ChildHandle::wait_exit`] waits without reaping (`waitid(WNOWAIT)`)
//!   and reaps under the same lock the terminator takes.

use std::process::{Child, Command};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;

/// A spawned engine shared by the reader, the waiter and the canceller.
pub struct ChildHandle {
    pub pid: u32,
    child: Mutex<Option<Child>>,
    reaped: AtomicBool,
    status: Mutex<Option<i32>>,
    #[cfg(windows)]
    job: Mutex<Option<windows_impl::Job>>,
}

impl ChildHandle {
    pub fn new(child: Child) -> Arc<Self> {
        Arc::new(Self {
            pid: child.id(),
            child: Mutex::new(Some(child)),
            reaped: AtomicBool::new(false),
            status: Mutex::new(None),
            #[cfg(windows)]
            job: Mutex::new(None),
        })
    }

    /// True once the process was reaped (never signal a reaped pid).
    pub fn is_reaped(&self) -> bool {
        self.reaped.load(Ordering::Acquire)
    }

    #[doc(hidden)]
    pub fn child(&self) -> &Mutex<Option<Child>> {
        &self.child
    }

    #[doc(hidden)]
    pub fn mark_reaped(&self) {
        self.reaped.store(true, Ordering::Release);
    }

    /// The exit status once known: the exit code, or on Unix the signal number when the process was killed by a
    /// signal (Foundation's `terminationStatus`).
    pub fn exit_status(&self) -> Option<i32> {
        *self.status.lock()
    }

    /// Blocks until the process exited and returns its status (see [`exit_status`](Self::exit_status)). Safe to call
    /// from several threads and before or after the exit; never holds the lock the terminator needs while blocking.
    pub fn wait_exit(&self) -> i32 {
        if let Some(s) = *self.status.lock() {
            return s;
        }
        self.block_until_exited();
        let mut guard = self.child.lock();
        if let Some(s) = *self.status.lock() {
            return s;
        }
        let status = match guard.as_mut().map(|c| c.wait()) {
            Some(Ok(st)) => status_code(st),
            _ => -1,
        };
        self.mark_reaped();
        *self.status.lock() = Some(status);
        drop(guard);
        status
    }

    /// Waits at most `timeout` for the exit; `Some(status)` when the process exited (and was reaped).
    pub fn wait_exit_timeout(&self, timeout: std::time::Duration) -> Option<i32> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if let Some(s) = *self.status.lock() {
                return Some(s);
            }
            if self.has_exited() {
                return Some(self.wait_exit());
            }
            if std::time::Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// Non-blocking: true once the process exited (it may not be reaped yet).
    pub fn has_exited(&self) -> bool {
        if self.status.lock().is_some() {
            return true;
        }
        self.exited_now()
    }

    #[cfg(unix)]
    fn exited_now(&self) -> bool {
        // SAFETY: zeroed siginfo_t is a valid out-parameter; WNOWAIT keeps the child waitable.
        unsafe {
            let mut info: libc::siginfo_t = std::mem::zeroed();
            let rc = libc::waitid(libc::P_PID, self.pid as libc::id_t, &mut info, libc::WEXITED | libc::WNOWAIT | libc::WNOHANG);
            rc == 0 && info.si_pid() != 0
        }
    }

    #[cfg(windows)]
    fn exited_now(&self) -> bool {
        use std::os::windows::io::AsRawHandle;
        let guard = self.child.lock();
        let Some(c) = guard.as_ref() else { return true };
        // SAFETY: the handle is owned by the `Child` we hold the lock on.
        unsafe {
            use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
            use windows::Win32::System::Threading::WaitForSingleObject;
            WaitForSingleObject(HANDLE(c.as_raw_handle()), 0) == WAIT_OBJECT_0
        }
    }

    #[cfg(not(any(unix, windows)))]
    fn exited_now(&self) -> bool {
        true
    }

    /// Runs `f` under the child lock unless the process was reaped (used by terminators).
    #[doc(hidden)]
    pub fn with_live_child(&self, f: impl FnOnce(&mut Child)) {
        let mut guard = self.child.lock();
        if self.is_reaped() {
            return;
        }
        if let Some(c) = guard.as_mut() {
            f(c);
        }
    }

    #[cfg(unix)]
    fn block_until_exited(&self) {
        // waitid(WNOWAIT) leaves the child waitable, so the pid stays reserved until `wait()` below reaps it under
        // the lock the terminator takes.
        loop {
            // SAFETY: zeroed siginfo_t is a valid out-parameter; the pid is our own child.
            let rc = unsafe {
                let mut info: libc::siginfo_t = std::mem::zeroed();
                libc::waitid(libc::P_PID, self.pid as libc::id_t, &mut info, libc::WEXITED | libc::WNOWAIT)
            };
            if rc == 0 {
                return;
            }
            if std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                return;
            }
        }
    }

    #[cfg(windows)]
    fn block_until_exited(&self) {
        use std::os::windows::io::AsRawHandle;
        let raw = self.child.lock().as_ref().map(|c| c.as_raw_handle() as isize);
        if let Some(raw) = raw {
            // SAFETY: the process handle stays open as long as `self.child` holds the `Child`, which is only taken
            // after this wait.
            unsafe {
                use windows::Win32::Foundation::HANDLE;
                use windows::Win32::System::Threading::{INFINITE, WaitForSingleObject};
                WaitForSingleObject(HANDLE(raw as *mut core::ffi::c_void), INFINITE);
            }
        }
    }

    #[cfg(not(any(unix, windows)))]
    fn block_until_exited(&self) {}
}

#[cfg(unix)]
fn status_code(status: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status.code().or_else(|| status.signal()).unwrap_or(-1)
}

#[cfg(not(unix))]
fn status_code(status: std::process::ExitStatus) -> i32 {
    status.code().unwrap_or(-1)
}

impl std::fmt::Debug for ChildHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChildHandle").field("pid", &self.pid).field("reaped", &self.is_reaped()).finish()
    }
}

/// How engine processes are configured and terminated.
pub trait ProcessControl: Send + Sync {
    /// Before spawn: Windows `CREATE_NO_WINDOW`; macOS nothing special.
    fn configure(&self, command: &mut Command);
    /// After spawn: Windows assigns the child to a Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`.
    fn adopt(&self, child: &ChildHandle);
    /// Windows `TerminateJobObject` (whole tree); macOS `kill(pid, SIGTERM)` only if not reaped.
    fn terminate(&self, child: &ChildHandle);
}

/// The platform implementation.
pub fn platform_process_control() -> Arc<dyn ProcessControl> {
    #[cfg(windows)]
    {
        Arc::new(windows_impl::JobObjectControl::new())
    }
    #[cfg(not(windows))]
    {
        Arc::new(unix_impl::SigtermControl)
    }
}

#[cfg(windows)]
pub use windows_impl::JobObjectControl;

#[cfg(windows)]
mod windows_impl {
    use super::*;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectExtendedLimitInformation, SetInformationJobObject, TerminateJobObject,
    };

    /// An owned job handle (closing it kills the remaining tree).
    pub(super) struct Job(isize);

    // SAFETY: a kernel handle value may be used from any thread.
    unsafe impl Send for Job {}

    impl Job {
        fn handle(&self) -> HANDLE {
            HANDLE(self.0 as *mut core::ffi::c_void)
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            // SAFETY: we own the handle and close it once.
            unsafe {
                let _ = CloseHandle(self.handle());
            }
        }
    }

    /// One Job Object per engine run, so `TerminateJobObject` kills exactly that engine's tree.
    pub struct JobObjectControl {
        _private: (),
    }

    impl JobObjectControl {
        pub fn new() -> Self {
            Self { _private: () }
        }
    }

    impl Default for JobObjectControl {
        fn default() -> Self {
            Self::new()
        }
    }

    impl ProcessControl for JobObjectControl {
        fn configure(&self, command: &mut Command) {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }

        fn adopt(&self, child: &ChildHandle) {
            use std::os::windows::io::AsRawHandle;
            // SAFETY: plain Win32 calls on handles we own; the info struct outlives the call.
            unsafe {
                let Ok(job) = CreateJobObjectW(None, windows::core::PCWSTR::null()) else { return };
                let job = Job(job.0 as isize);
                let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let _ = SetInformationJobObject(
                    job.handle(),
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const core::ffi::c_void,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                let raw = child.child.lock().as_ref().map(|c| c.as_raw_handle() as isize);
                if let Some(raw) = raw
                    && AssignProcessToJobObject(job.handle(), HANDLE(raw as *mut core::ffi::c_void)).is_ok()
                {
                    *child.job.lock() = Some(job);
                }
            }
        }

        fn terminate(&self, child: &ChildHandle) {
            if let Some(job) = child.job.lock().as_ref() {
                // SAFETY: the job handle is owned by the child handle and still open.
                unsafe {
                    let _ = TerminateJobObject(job.handle(), 1);
                }
                return;
            }
            // Not in a job (assignment failed): kill the engine itself.
            child.with_live_child(|c| {
                let _ = c.kill();
            });
        }
    }
}

#[cfg(not(windows))]
mod unix_impl {
    use super::*;

    /// SIGTERM, never SIGKILL.
    pub struct SigtermControl;

    impl ProcessControl for SigtermControl {
        fn configure(&self, _command: &mut Command) {}
        fn adopt(&self, _child: &ChildHandle) {}
        fn terminate(&self, child: &ChildHandle) {
            child.with_live_child(|c| {
                // SAFETY: plain FFI call with a pid we spawned and have not reaped (checked under the reap lock).
                unsafe {
                    libc::kill(c.id() as libc::pid_t, libc::SIGTERM);
                }
            });
        }
    }
}
