//! Overlapped named-pipe primitives (no async runtime): cancellable reads/writes/connects waited on Win32 events,
//! and [`Connection`] = one pipe end with a bounded outbound queue drained by a writer thread (F5: a stalled peer
//! never blocks a publisher; on overflow that peer is disconnected and reconnects by itself).

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use crossbeam_channel::{Sender, TrySendError, bounded};
use parking_lot::Mutex;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_BROKEN_PIPE, ERROR_FILE_NOT_FOUND, ERROR_IO_PENDING, ERROR_NO_DATA, ERROR_OPERATION_ABORTED,
    ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED, GENERIC_READ, GENERIC_WRITE, HANDLE,
    WAIT_OBJECT_0,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_OVERLAPPED, FILE_SHARE_MODE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX, ReadFile,
    SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT, WriteFile,
};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT, WaitNamedPipeW,
};
use windows::Win32::System::Threading::{CreateEventW, ResetEvent, SetEvent, WaitForMultipleObjects};
use windows::core::PCWSTR;

use super::security::ServerSecurity;

/// Per-connection outbound queue (F5).
pub(crate) const CONNECTION_QUEUE: usize = 1024;

fn os_err(e: windows::core::Error) -> io::Error {
    io::Error::from_raw_os_error((e.code().0 & 0xFFFF) as i32)
}

fn is(e: &windows::core::Error, code: windows::Win32::Foundation::WIN32_ERROR) -> bool {
    e.code() == code.to_hresult()
}

/// A manual-reset Win32 event.
pub(crate) struct Event(HANDLE);

unsafe impl Send for Event {}
unsafe impl Sync for Event {}

impl Event {
    pub(crate) fn new() -> io::Result<Self> {
        let h = unsafe { CreateEventW(None, true, false, PCWSTR::null()) }.map_err(os_err)?;
        Ok(Self(h))
    }
    pub(crate) fn set(&self) {
        unsafe {
            let _ = SetEvent(self.0);
        }
    }
    pub(crate) fn reset(&self) {
        unsafe {
            let _ = ResetEvent(self.0);
        }
    }
    pub(crate) fn handle(&self) -> HANDLE {
        self.0
    }
    /// Waits up to `timeout`; true when signaled.
    pub(crate) fn wait(&self, timeout: Duration) -> bool {
        let r = unsafe { WaitForMultipleObjects(&[self.0], false, timeout.as_millis().min(u32::MAX as u128 - 1) as u32) };
        r == WAIT_OBJECT_0
    }
    pub(crate) fn is_set(&self) -> bool {
        self.wait(Duration::ZERO)
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// An overlapped pipe handle (client or server end). Reads and writes may run concurrently on two threads.
pub(crate) struct Pipe {
    h: HANDLE,
    read_ev: Event,
    write_ev: Event,
}

unsafe impl Send for Pipe {}
unsafe impl Sync for Pipe {}

impl Drop for Pipe {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.h);
        }
    }
}

const INFINITE: u32 = u32::MAX;

impl Pipe {
    fn from_handle(h: HANDLE) -> io::Result<Self> {
        Ok(Self { h, read_ev: Event::new()?, write_ev: Event::new()? })
    }

    pub(crate) fn raw(&self) -> HANDLE {
        self.h
    }

    /// Aborts every pending operation on the handle (any thread).
    pub(crate) fn cancel_io(&self) {
        unsafe {
            let _ = CancelIoEx(self.h, None);
        }
    }

    /// Runs one overlapped operation to completion or until one of `cancels` is signaled (then it is cancelled and
    /// awaited, and `Interrupted` is returned).
    fn run(
        &self,
        ev: &Event,
        cancels: &[&Event],
        timeout: Option<Duration>,
        start: impl FnOnce(*mut OVERLAPPED) -> windows::core::Result<bool>,
    ) -> io::Result<u32> {
        ev.reset();
        let mut ov = OVERLAPPED { hEvent: ev.handle(), ..Default::default() };
        let mut n = 0u32;
        match start(&mut ov) {
            // Completed synchronously; the OVERLAPPED holds the result (and the event is set).
            Ok(true) => {
                unsafe { GetOverlappedResult(self.h, &ov, &mut n, true) }.map_err(os_err)?;
                Ok(n)
            }
            // Completed without an I/O request (ERROR_PIPE_CONNECTED): nothing to wait for — waiting on the event
            // here would hang forever.
            Ok(false) => Ok(0),
            Err(e) if is(&e, ERROR_IO_PENDING) => {
                let mut handles = vec![ev.handle()];
                handles.extend(cancels.iter().map(|c| c.handle()));
                let ms = timeout.map_or(INFINITE, |t| t.as_millis().min(u128::from(INFINITE - 1)) as u32);
                let r = unsafe { WaitForMultipleObjects(&handles, false, ms) };
                if r == WAIT_OBJECT_0 {
                    unsafe { GetOverlappedResult(self.h, &ov, &mut n, false) }.map_err(os_err)?;
                    Ok(n)
                } else {
                    unsafe {
                        let _ = CancelIoEx(self.h, Some(&ov));
                    }
                    // The OVERLAPPED and buffer must outlive the operation: wait for it to finish.
                    let done = unsafe { GetOverlappedResult(self.h, &ov, &mut n, true) };
                    match done {
                        Ok(()) => Ok(n),
                        Err(_) => Err(io::Error::from(io::ErrorKind::Interrupted)),
                    }
                }
            }
            Err(e) => Err(os_err(e)),
        }
    }

    /// Reads some bytes; `Ok(0)` = end of stream (peer closed).
    pub(crate) fn read(&self, buf: &mut [u8], cancels: &[&Event], timeout: Option<Duration>) -> io::Result<usize> {
        let h = self.h;
        let ptr = buf.as_mut_ptr();
        let len = buf.len();
        let r = self.run(&self.read_ev, cancels, timeout, |ov| unsafe {
            ReadFile(h, Some(std::slice::from_raw_parts_mut(ptr, len)), None, Some(ov)).map(|()| true)
        });
        match r {
            Ok(n) => Ok(n as usize),
            Err(e) => match e.raw_os_error().map(|c| c as u32) {
                Some(c) if c == ERROR_BROKEN_PIPE.0 || c == ERROR_PIPE_NOT_CONNECTED.0 || c == ERROR_NO_DATA.0 => Ok(0),
                Some(c) if c == ERROR_OPERATION_ABORTED.0 => Err(io::Error::from(io::ErrorKind::Interrupted)),
                _ => Err(e),
            },
        }
    }

    /// Writes all of `data`.
    pub(crate) fn write_all(&self, mut data: &[u8], cancels: &[&Event]) -> io::Result<()> {
        let h = self.h;
        while !data.is_empty() {
            let chunk = &data[..data.len().min(1 << 20)];
            let n = self.run(&self.write_ev, cancels, None, |ov| unsafe { WriteFile(h, Some(chunk), None, Some(ov)).map(|()| true) })?;
            if n == 0 {
                return Err(io::Error::from(io::ErrorKind::WriteZero));
            }
            data = &data[n as usize..];
        }
        Ok(())
    }

    /// Server: waits for a client (`ConnectNamedPipe`).
    pub(crate) fn accept(&self, cancels: &[&Event]) -> io::Result<()> {
        let h = self.h;
        // A client may connect between CreateNamedPipeW and ConnectNamedPipe: ERROR_PIPE_CONNECTED, no I/O pending.
        let r = self.run(&self.read_ev, cancels, None, |ov| match unsafe { ConnectNamedPipe(h, Some(ov)) } {
            Err(e) if is(&e, ERROR_PIPE_CONNECTED) => Ok(false),
            Ok(()) => Ok(false),
            Err(e) => Err(e),
        });
        r.map(|_| ())
    }
}

/// UTF-16, NUL-terminated `\\.\pipe\<name>`.
pub(crate) fn pipe_path(name: &str) -> Vec<u16> {
    format!(r"\\.\pipe\{name}").encode_utf16().chain(Some(0)).collect()
}

/// Creates one server instance (byte mode, duplex, overlapped, unlimited instances, remote clients rejected, F6 DACL).
pub(crate) fn create_server_instance(path: &[u16], security: &ServerSecurity) -> io::Result<Pipe> {
    let h = unsafe {
        CreateNamedPipeW(
            PCWSTR(path.as_ptr()),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            0,
            0,
            0,
            Some(security.attributes()),
        )
    };
    if h.is_invalid() {
        return Err(io::Error::last_os_error());
    }
    Pipe::from_handle(h)
}

/// Client: opens the pipe, retrying "not found"/"busy" for up to `timeout` (.NET `ConnectAsync(150)`).
pub(crate) fn connect_client(path: &[u16], timeout: Duration, cancel: &Event) -> io::Result<Pipe> {
    let deadline = Instant::now() + timeout;
    loop {
        let r = unsafe {
            CreateFileW(
                PCWSTR(path.as_ptr()),
                GENERIC_READ.0 | GENERIC_WRITE.0,
                FILE_SHARE_MODE(0),
                None,
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                None,
            )
        };
        let now = Instant::now();
        match r {
            Ok(h) => return Pipe::from_handle(h),
            Err(e) if is(&e, ERROR_PIPE_BUSY) => {
                if now >= deadline {
                    return Err(io::Error::from(io::ErrorKind::TimedOut));
                }
                let ms = (deadline - now).as_millis().clamp(1, 1000) as u32;
                let _ = unsafe { WaitNamedPipeW(PCWSTR(path.as_ptr()), ms) };
            }
            Err(e) if is(&e, ERROR_FILE_NOT_FOUND) => {
                if now >= deadline {
                    return Err(io::Error::from(io::ErrorKind::TimedOut));
                }
                if cancel.wait(Duration::from_millis(5)) {
                    return Err(io::Error::from(io::ErrorKind::Interrupted));
                }
            }
            Err(e) => return Err(os_err(e)),
        }
        if cancel.is_set() {
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }
    }
}

/// One connected pipe end with an outbound queue drained by its own writer thread.
pub(crate) struct Connection {
    pipe: Arc<Pipe>,
    closed: AtomicBool,
    close_ev: Arc<Event>,
    /// Set when the writer thread has exited (queue drained, closed or failed).
    writer_done: Arc<Event>,
    tx: Mutex<Option<Sender<Arc<[u8]>>>>,
}

impl Connection {
    pub(crate) fn start(pipe: Arc<Pipe>) -> io::Result<Arc<Self>> {
        let close_ev = Arc::new(Event::new()?);
        let writer_done = Arc::new(Event::new()?);
        let (tx, rx) = bounded::<Arc<[u8]>>(CONNECTION_QUEUE);
        let conn = Arc::new(Self {
            pipe: pipe.clone(),
            closed: AtomicBool::new(false),
            close_ev: close_ev.clone(),
            writer_done: writer_done.clone(),
            tx: Mutex::new(Some(tx)),
        });
        let weak: Weak<Connection> = Arc::downgrade(&conn);
        std::thread::Builder::new()
            .name("sa-sync-writer".into())
            .spawn(move || {
                while let Ok(bytes) = rx.recv() {
                    if close_ev.is_set() {
                        break;
                    }
                    if pipe.write_all(&bytes, &[&close_ev]).is_err() {
                        if let Some(c) = weak.upgrade() {
                            c.close();
                        }
                        break;
                    }
                }
                writer_done.set();
            })
            .map_err(io::Error::other)?;
        Ok(conn)
    }

    pub(crate) fn is_open(&self) -> bool {
        !self.closed.load(Ordering::SeqCst)
    }

    /// Queues `bytes`; false when closed (or the queue overflowed — the peer is then disconnected).
    pub(crate) fn try_send(&self, bytes: Arc<[u8]>) -> bool {
        if !self.is_open() {
            return false;
        }
        let r = match self.tx.lock().as_ref() {
            Some(tx) => tx.try_send(bytes),
            None => return false,
        };
        match r {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                self.close();
                false
            }
            Err(TrySendError::Disconnected(_)) => false,
        }
    }

    /// Reads some bytes; `Ok(0)` at end of stream; `Interrupted` when closed or `extra` is signaled.
    pub(crate) fn read(&self, buf: &mut [u8], extra: &Event) -> io::Result<usize> {
        self.read_timeout(buf, extra, None)
    }

    /// [`Self::read`] that gives up (`Interrupted`) after `timeout`.
    pub(crate) fn read_timeout(&self, buf: &mut [u8], extra: &Event, timeout: Option<Duration>) -> io::Result<usize> {
        if !self.is_open() {
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }
        self.pipe.read(buf, &[&self.close_ev, extra], timeout)
    }

    /// Stops accepting lines, lets the writer put every line already queued into the pipe (at most `timeout`), then
    /// [`Self::close`]s. The C# broker writes synchronously, so a line published right before its dispose is in the
    /// pipe when dispose returns; this keeps that guarantee for the queued writer (F5), bounded for a stalled peer.
    pub(crate) fn close_after_drain(&self, timeout: Duration) {
        if self.is_open() {
            // Dropping the sender ends the writer's loop once the queue is empty.
            self.tx.lock().take();
            self.writer_done.wait(timeout);
        }
        self.close();
    }

    /// Idempotent: stops the writer, aborts pending I/O; the handle closes when the last user lets go.
    pub(crate) fn close(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        self.close_ev.set();
        self.tx.lock().take();
        self.pipe.cancel_io();
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.close();
    }
}
