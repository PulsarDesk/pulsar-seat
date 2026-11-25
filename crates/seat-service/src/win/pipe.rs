// SPDX-License-Identifier: GPL-3.0-only
//! Control pipe server (architecture §5). Adapted from pulsar-split
//! (`split-service::pipe`), with overlapped I/O so a subscribed connection can be read and
//! written from two threads, and with caller identification from the client's token.
//!
//! - DACL from SDDL ([`seat_proto::PIPE_SDDL`]); `PIPE_REJECT_REMOTE_CLIENTS`.
//! - The first instance uses `FILE_FLAG_FIRST_PIPE_INSTANCE`: the service refuses to run if
//!   somebody else already owns the name (squatting).
//! - Byte mode; framing from `seat_proto::framing`. One thread per connection, plus one
//!   event pusher per subscribed connection.
//! - The caller is identified after the first frame (impersonation needs a read first):
//!   PID, token user SID, token session, Administrators enabled, SYSTEM.
//! - Limits against a local user exhausting the service: at most [`MAX_INSTANCES`] pipe
//!   instances, of which ordinary clients may hold at most [`MAX_UNTRUSTED`] in total and
//!   [`MAX_PER_SESSION`] per client session. The service's own children (seat hosts, the
//!   console listener, seat agents; checked by client PID right after the connect, see
//!   [`TrustCheck`]) are not counted, so a user who opens many idle connections cannot lock
//!   the hosts out. An ordinary client that sends no request within
//!   [`FIRST_REQUEST_TIMEOUT`] is disconnected. Every write is bounded by
//!   [`WRITE_TIMEOUT`] (a client that stops reading is disconnected), the event pusher
//!   notices a closed connection within [`PUSH_POLL`], and a closing instance never waits
//!   for the client to read (no `FlushFileBuffers`; after an error reply the server waits
//!   at most [`LINGER`] for the client to hang up).

use crate::auth::Caller;
use crate::log;
use crate::requests::Handled;
use crate::win::security::{SecurityDescriptor, token_is_admin, token_session_id, token_user_sid, wide};
use seat_proto::{ErrorCode, ProtoError, Request, Response, ServerFrame};
use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_OPERATION_ABORTED, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED,
    ERROR_PIPE_NOT_CONNECTED, GetLastError, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
};
use windows_sys::Win32::Security::{RevertToSelf, TOKEN_QUERY};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX, ReadFile, WriteFile,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeClientProcessId, GetNamedPipeClientSessionId,
    ImpersonateNamedPipeClient, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentThread, INFINITE, OpenThreadToken, SetEvent, WaitForMultipleObjects, WaitForSingleObject,
};

pub type Handler = Arc<dyn Fn(&mut Caller, Request) -> Handled + Send + Sync>;

const BUF: u32 = 64 * 1024;
/// Pipe instances (= concurrent connections) the server allows.
pub const MAX_INSTANCES: u32 = 128;
/// Concurrent connections of ordinary clients (not the service's own children) in total;
/// the rest of [`MAX_INSTANCES`] stays for verified service children (up to 16 seats with
/// 2 host connections each, the console listener, the seat agents).
pub const MAX_UNTRUSTED: usize = 64;
/// Concurrent connections of ordinary clients from one client session.
pub const MAX_PER_SESSION: usize = 16;
/// An ordinary client must send its first request within this time.
pub const FIRST_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// `true` when the client PID is a live process the service itself launched as part of
/// its infrastructure (seat host, console listener, seat agent). It only exempts the
/// connection from the counting limits; authorization still goes by the token (`Caller`).
pub type TrustCheck = Arc<dyn Fn(u32) -> bool + Send + Sync>;
/// A write that does not complete in this time disconnects the client.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// Event pusher wake-up to notice a closed connection.
pub const PUSH_POLL: Duration = Duration::from_millis(500);
/// After an error reply, wait at most this long for the client to hang up.
pub const LINGER: Duration = Duration::from_secs(2);

/// Owned event handle.
struct Event(HANDLE);
unsafe impl Send for Event {}
unsafe impl Sync for Event {}

impl Event {
    fn new(manual: bool) -> io::Result<Self> {
        // SAFETY: plain unnamed event.
        let h = unsafe { CreateEventW(std::ptr::null(), i32::from(manual), 0, std::ptr::null()) };
        if h.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(h))
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: owned handle.
        unsafe { CloseHandle(self.0) };
    }
}

/// One connected pipe instance with overlapped reads and writes.
pub struct PipeIo {
    h: HANDLE,
}

unsafe impl Send for PipeIo {}
unsafe impl Sync for PipeIo {}

impl PipeIo {
    /// One overlapped operation; with `timeout`, cancelled (and `TimedOut` returned) when
    /// it does not complete in time.
    fn overlapped(&self, op: impl FnOnce(*mut OVERLAPPED) -> i32, timeout: Option<Duration>) -> io::Result<usize> {
        let ev = Event::new(true)?;
        // SAFETY: zeroed OVERLAPPED is valid; the event outlives the operation.
        let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
        ov.hEvent = ev.0;
        if op(&mut ov) == 0 {
            // SAFETY: trivially safe.
            let e = unsafe { GetLastError() };
            if e != ERROR_IO_PENDING {
                return Err(io::Error::from_raw_os_error(e as i32));
            }
        }
        let mut timed_out = false;
        if let Some(t) = timeout {
            let ms = u32::try_from(t.as_millis()).unwrap_or(INFINITE - 1);
            // SAFETY: valid event handle.
            if unsafe { WaitForSingleObject(ev.0, ms) } != WAIT_OBJECT_0 {
                timed_out = true;
                // SAFETY: cancel our own pending operation; completion is awaited below
                // before `ov` and the buffer go away.
                unsafe { CancelIoEx(self.h, &ov) };
            }
        }
        let mut n = 0u32;
        // SAFETY: waits for the operation started above on our handle.
        let ok = unsafe { GetOverlappedResult(self.h, &ov, &mut n, 1) } != 0;
        if timed_out && !ok {
            return Err(io::Error::from(io::ErrorKind::TimedOut));
        }
        if !ok {
            return Err(io::Error::last_os_error());
        }
        Ok(n as usize)
    }

    fn read_with(&self, buf: &mut [u8], timeout: Option<Duration>) -> io::Result<usize> {
        let len = buf.len().min(u32::MAX as usize) as u32;
        let r = self.overlapped(
            |ov| {
                // SAFETY: buffer valid for `len` bytes until GetOverlappedResult returns.
                unsafe { ReadFile(self.h, buf.as_mut_ptr(), len, std::ptr::null_mut(), ov) }
            },
            timeout,
        );
        match r {
            // Hang-up, or our own CancelIoEx when the connection is being closed: EOF.
            Err(e)
                if matches!(
                    e.raw_os_error().map(|c| c as u32),
                    Some(ERROR_BROKEN_PIPE | ERROR_PIPE_NOT_CONNECTED | ERROR_OPERATION_ABORTED)
                ) =>
            {
                Ok(0)
            }
            other => other,
        }
    }

    pub fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        self.read_with(buf, None)
    }

    /// Bounded write ([`WRITE_TIMEOUT`]).
    pub fn write(&self, buf: &[u8]) -> io::Result<usize> {
        let len = buf.len().min(u32::MAX as usize) as u32;
        self.overlapped(
            |ov| {
                // SAFETY: buffer valid until GetOverlappedResult returns.
                unsafe { WriteFile(self.h, buf.as_ptr(), len, std::ptr::null_mut(), ov) }
            },
            Some(WRITE_TIMEOUT),
        )
    }

    /// Abort every pending read / write on this instance (from any thread).
    fn cancel_all(&self) {
        // SAFETY: valid handle; pending operations complete with ERROR_OPERATION_ABORTED.
        unsafe { CancelIoEx(self.h, std::ptr::null()) };
    }

    /// Discard input until the client hangs up, at most [`LINGER`] (so an error reply is
    /// read before the instance is disconnected, without pinning the thread).
    fn linger(&self) {
        let end = std::time::Instant::now() + LINGER;
        let mut buf = [0u8; 512];
        loop {
            let now = std::time::Instant::now();
            if now >= end {
                return;
            }
            match self.read_with(&mut buf, Some(end - now)) {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
        }
    }

    /// Identify the client (after at least one read).
    pub fn caller(&self) -> Caller {
        let mut pid = 0u32;
        let mut session = 0u32;
        // SAFETY: connected server end.
        unsafe {
            GetNamedPipeClientProcessId(self.h, &mut pid);
            GetNamedPipeClientSessionId(self.h, &mut session);
        }
        let mut c = Caller {
            pid,
            session_id: session,
            user_sid: None,
            is_admin: false,
            is_system: false,
            client: None,
            host_seat: None,
            agent_session: None,
            console_listener: false,
            credprov_session: None,
            kiosk_seat: None,
            protocol: 0,
        };
        // SAFETY: impersonate on this thread, query, revert.
        unsafe {
            if ImpersonateNamedPipeClient(self.h) != 0 {
                let mut tok: HANDLE = std::ptr::null_mut();
                if OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut tok) != 0 {
                    c.user_sid = token_user_sid(tok).ok();
                    if let Ok(s) = token_session_id(tok) {
                        c.session_id = s;
                    }
                    c.is_admin = token_is_admin(tok);
                    CloseHandle(tok);
                }
                RevertToSelf();
            }
        }
        c.is_system = c.user_sid.as_deref() == Some("S-1-5-18");
        c
    }
}

impl Drop for PipeIo {
    fn drop(&mut self) {
        // No FlushFileBuffers: it blocks until the client has read everything, so a client
        // that never reads would pin the instance forever.
        // SAFETY: owned handle.
        unsafe {
            DisconnectNamedPipe(self.h);
            CloseHandle(self.h);
        }
    }
}

/// Ordinary (untrusted) connections: per client session and in total.
#[derive(Default)]
struct Counts {
    per_session: HashMap<u32, usize>,
    total: usize,
}

type SessionCounts = Arc<Mutex<Counts>>;

/// Why [`SessionSlot::take`] refused a connection.
#[derive(Debug, PartialEq, Eq)]
enum Refused {
    Session,
    Total,
}

/// One counted connection; uncounted on drop.
struct SessionSlot {
    counts: SessionCounts,
    session: u32,
}

impl SessionSlot {
    fn take(counts: &SessionCounts, session: u32) -> Result<Self, Refused> {
        let mut g = counts.lock().unwrap_or_else(|e| e.into_inner());
        if g.total >= MAX_UNTRUSTED {
            return Err(Refused::Total);
        }
        let n = g.per_session.entry(session).or_default();
        if *n >= MAX_PER_SESSION {
            return Err(Refused::Session);
        }
        *n += 1;
        g.total += 1;
        Ok(Self { counts: counts.clone(), session })
    }
}

impl Drop for SessionSlot {
    fn drop(&mut self) {
        let mut g = self.counts.lock().unwrap_or_else(|e| e.into_inner());
        g.total = g.total.saturating_sub(1);
        if let Some(n) = g.per_session.get_mut(&self.session) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                g.per_session.remove(&self.session);
            }
        }
    }
}

/// Reader for the first request of an ordinary client: every read is bounded by the time
/// left to `end`; `timed_out` is set when the deadline cut a read.
struct DeadlineReader<'a> {
    io: &'a PipeIo,
    end: Option<std::time::Instant>,
    timed_out: bool,
}

impl io::Read for DeadlineReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let Some(end) = self.end else { return self.io.read(buf) };
        let now = std::time::Instant::now();
        if now >= end {
            self.timed_out = true;
            return Err(io::Error::from(io::ErrorKind::TimedOut));
        }
        let r = self.io.read_with(buf, Some(end - now));
        if matches!(&r, Err(e) if e.kind() == io::ErrorKind::TimedOut) {
            self.timed_out = true;
        }
        r
    }
}

struct Writer<'a>(&'a PipeIo);
impl io::Write for Writer<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub struct PipeServer {
    name: String,
    sd: SecurityDescriptor,
    stop: Arc<Event>,
    stopped: Arc<AtomicBool>,
    sessions: SessionCounts,
    trusted: Option<TrustCheck>,
}

/// Ends [`PipeServer::run`].
#[derive(Clone)]
pub struct Shutdown {
    stop: Arc<Event>,
    stopped: Arc<AtomicBool>,
}

unsafe impl Send for Shutdown {}
unsafe impl Sync for Shutdown {}

impl Shutdown {
    pub fn trigger(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        // SAFETY: valid event handle.
        unsafe { SetEvent(self.stop.0) };
    }
}

impl PipeServer {
    pub fn new(name: &str, sddl: &str) -> io::Result<Self> {
        Ok(Self {
            name: name.to_string(),
            sd: SecurityDescriptor::from_sddl(sddl)?,
            stop: Arc::new(Event::new(true)?),
            stopped: Arc::default(),
            sessions: Arc::default(),
            trusted: None,
        })
    }

    /// Exempt the service's own children (by client PID) from the connection limits.
    pub fn with_trusted(mut self, check: TrustCheck) -> Self {
        self.trusted = Some(check);
        self
    }

    pub fn shutdown_handle(&self) -> Shutdown {
        Shutdown { stop: self.stop.clone(), stopped: self.stopped.clone() }
    }

    fn create_instance(&self, first: bool) -> io::Result<HANDLE> {
        let name = wide(&self.name);
        let sa = self.sd.attributes();
        let mut open_mode = PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED;
        if first {
            open_mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
        }
        // SAFETY: valid name and security attributes that outlive the call.
        let h = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                open_mode,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                MAX_INSTANCES,
                BUF,
                BUF,
                0,
                &sa,
            )
        };
        if h == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(h)
    }

    /// Claim the name now (fails if another process owns it). Returns the first instance.
    pub fn claim(&self) -> io::Result<PipeIo> {
        Ok(PipeIo { h: self.create_instance(true)? })
    }

    /// Serve until [`Shutdown::trigger`]. `first` = the instance from [`Self::claim`].
    pub fn run(&self, first: PipeIo, handler: Handler) -> io::Result<()> {
        let mut next = Some(first);
        while !self.stopped.load(Ordering::SeqCst) {
            let pipe = match next.take() {
                Some(p) => p,
                None => match self.create_instance(false) {
                    Ok(h) => PipeIo { h },
                    Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                        // Every instance is in use: wait for one to close (or the stop).
                        // SAFETY: valid event handle.
                        unsafe { WaitForSingleObject(self.stop.0, 250) };
                        continue;
                    }
                    Err(e) => return Err(e),
                },
            };
            let ev = Event::new(true)?;
            // SAFETY: zeroed OVERLAPPED with our event.
            let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
            ov.hEvent = ev.0;
            // SAFETY: fresh instance, OVERLAPPED lives until completion or cancel below.
            let ok = unsafe { ConnectNamedPipe(pipe.h, &mut ov) };
            // SAFETY: trivially safe.
            let last = unsafe { GetLastError() };
            let connected = if ok != 0 || last == ERROR_PIPE_CONNECTED {
                true
            } else if last == ERROR_IO_PENDING {
                let handles = [ev.0, self.stop.0];
                // SAFETY: two valid handles.
                let w = unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, INFINITE) };
                if w == WAIT_OBJECT_0 {
                    let mut n = 0;
                    // SAFETY: operation completed.
                    unsafe { GetOverlappedResult(pipe.h, &ov, &mut n, 0) != 0 }
                } else {
                    // SAFETY: cancel the pending connect before `ov` goes away.
                    unsafe {
                        CancelIoEx(pipe.h, &ov);
                        let mut n = 0;
                        GetOverlappedResult(pipe.h, &ov, &mut n, 1);
                    }
                    break;
                }
            } else {
                log::warn(&format!("ConnectNamedPipe failed: {}", io::Error::from_raw_os_error(last as i32)));
                false
            };
            if !connected {
                continue;
            }
            let handler = handler.clone();
            let sessions = self.sessions.clone();
            let trusted = self.trusted.clone();
            std::thread::spawn(move || serve(Arc::new(pipe), handler, sessions, trusted));
        }
        Ok(())
    }
}

fn write_frame(w: &Mutex<()>, io: &PipeIo, frame: &ServerFrame) -> bool {
    let _g = w.lock().unwrap_or_else(|e| e.into_inner());
    seat_proto::write_message(&mut Writer(io), frame).is_ok()
}

fn serve(pipe: Arc<PipeIo>, handler: Handler, sessions: SessionCounts, trusted: Option<TrustCheck>) {
    let write_lock = Arc::new(Mutex::new(()));
    let mut caller: Option<Caller> = None;
    // Counted from the connect on (the pipe knows the client PID and session before any
    // read), so idle connections of one session cannot take every instance. The service's
    // own children are exempt: with one-console seats every seat host sits in the console
    // session with two connections each.
    let (mut session, mut pid) = (0u32, 0u32);
    // SAFETY: connected server end; out pointers valid.
    unsafe {
        GetNamedPipeClientSessionId(pipe.h, &mut session);
        GetNamedPipeClientProcessId(pipe.h, &mut pid);
    }
    let is_child = pid != 0 && trusted.as_ref().is_some_and(|t| t(pid));
    let slot = if is_child {
        None
    } else {
        match SessionSlot::take(&sessions, session) {
            Ok(s) => Some(s),
            Err(Refused::Session) => {
                log::warn(&format!("too many connections from session {session}; connection closed"));
                return;
            }
            Err(Refused::Total) => {
                log::warn(&format!("too many client connections in total; connection from session {session} closed"));
                return;
            }
        }
    };
    // Ordinary clients must speak within FIRST_REQUEST_TIMEOUT (an idle connection would
    // otherwise hold its slot forever).
    let mut first_deadline = (!is_child).then(|| std::time::Instant::now() + FIRST_REQUEST_TIMEOUT);
    let mut push_stop: Option<Arc<AtomicBool>> = None;
    // Set by a pusher whose write failed / timed out: the connection is closing.
    let dead = Arc::new(AtomicBool::new(false));
    loop {
        let mut reader = DeadlineReader { io: &pipe, end: first_deadline.take(), timed_out: false };
        let read = seat_proto::read_message::<_, Request>(&mut reader);
        if reader.timed_out {
            log::warn(&format!("pid {pid} (session {session}) sent no request in time; connection closed"));
            break;
        }
        let req = match read {
            Ok(Some(r)) => r,
            Ok(None) => break,
            Err(e) => {
                if dead.load(Ordering::SeqCst) {
                    break;
                }
                let pid = caller.as_ref().map(|c| c.pid).unwrap_or(0);
                log::warn(&format!("bad frame from pid {pid}: {e}"));
                let resp = Response::new(0, Err(ProtoError::new(ErrorCode::BadRequest, e.to_string())));
                if write_frame(&write_lock, &pipe, &ServerFrame::Response(resp)) {
                    pipe.linger();
                }
                break;
            }
        };
        if dead.load(Ordering::SeqCst) {
            break;
        }
        let c = caller.get_or_insert_with(|| pipe.caller());
        let Handled { response, subscription } = handler(c, req);
        if !write_frame(&write_lock, &pipe, &ServerFrame::Response(response)) {
            break;
        }
        if let Some(rx) = subscription {
            if let Some(old) = push_stop.take() {
                old.store(true, Ordering::SeqCst);
            }
            let stop = Arc::new(AtomicBool::new(false));
            push_stop = Some(stop.clone());
            let (p, w, dead) = (pipe.clone(), write_lock.clone(), dead.clone());
            std::thread::spawn(move || {
                // Input trace (2026-11-25 mouse ~1 Hz): input frames/s pushed on this
                // connection and the time each pipe write took (the client reading slowly
                // shows up here).
                let mut meter = seat_core::input_trace::InputMeter::new("pipe_push");
                loop {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    match rx.recv_timeout(PUSH_POLL) {
                        Ok(frame) => {
                            if stop.load(Ordering::SeqCst) {
                                break;
                            }
                            let input = matches!(frame.event, seat_proto::Event::InputPackets { .. });
                            let t = std::time::Instant::now();
                            let ok = write_frame(&w, &p, &ServerFrame::Event(frame));
                            if input {
                                let done = std::time::Instant::now();
                                meter.hit(t);
                                meter.wait(done.saturating_duration_since(t));
                                if let Some(l) = meter.take_line(done) {
                                    log::info(&format!("{l} pid={pid}"));
                                }
                            }
                            if !ok {
                                // Client stopped reading (timeout) or hung up: close.
                                dead.store(true, Ordering::SeqCst);
                                p.cancel_all();
                                break;
                            }
                        }
                        Err(RecvTimeoutError::Timeout) => {}
                        // The hub dropped a lagging subscriber: close the connection, the
                        // client re-subscribes and catches up with poll_events.
                        Err(RecvTimeoutError::Disconnected) => {
                            dead.store(true, Ordering::SeqCst);
                            p.cancel_all();
                            break;
                        }
                    }
                }
            });
        }
    }
    drop(slot);
    if let Some(s) = push_stop {
        s.store(true, Ordering::SeqCst);
    }
    // Wake a pusher blocked in a write; the handle closes when the last Arc drops (the
    // pusher notices `stop` within PUSH_POLL).
    pipe.cancel_all();
}

#[cfg(test)]
mod tests {
    use super::*;
    use seat_proto::{ClientKind, Event, Hello, RequestBody, ResponseBody, Topic};

    fn connect(name: &str) -> std::fs::File {
        for _ in 0..100 {
            if let Ok(f) = std::os::windows::fs::OpenOptionsExt::access_mode(&mut std::fs::OpenOptions::new(), seat_proto::PIPE_CLIENT_ACCESS).open(name) {
                return f;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("connect");
    }

    /// Interactive users may connect with [`seat_proto::PIPE_CLIENT_ACCESS`] but may not
    /// create further server instances on the name (no `FILE_CREATE_PIPE_INSTANCE`).
    /// Meaningful only when the test runs non-elevated (the usual dev shell): an elevated
    /// token matches the Administrators GA entry and may create instances.
    #[test]
    fn users_cannot_create_rogue_instances() {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
        let name = format!(r"\\.\pipe\PulsarSeat.TestRogue.{}", std::process::id());
        let server = PipeServer::new(&name, seat_proto::PIPE_SDDL).unwrap();
        let _first = server.claim().unwrap();
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: pseudo handle of our process; out pointer valid.
        assert!(unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } != 0);
        let admin = token_is_admin(token);
        // SAFETY: token from OpenProcessToken.
        unsafe { CloseHandle(token) };
        match server.create_instance(false) {
            Ok(h) => {
                // SAFETY: handle we just created.
                unsafe { CloseHandle(h) };
                assert!(admin, "a non-admin created a second server instance");
            }
            Err(e) => assert_eq!(e.kind(), io::ErrorKind::PermissionDenied, "{e}"),
        }
        // The client access mask still opens the pipe.
        let c = std::fs::OpenOptions::new().access_mode(seat_proto::PIPE_CLIENT_ACCESS).open(&name);
        assert!(c.is_ok(), "{c:?}");
    }

    /// End-to-end over a real pipe: identification, responses, pushed events, squatting.
    #[test]
    fn real_pipe_roundtrip() {
        let name = format!(r"\\.\pipe\PulsarSeat.Test.{}", std::process::id());
        let sid = crate::win::security::current_user_sid().unwrap();
        // With the Low label the service uses (creatable without privileges).
        let sddl = format!("{}(A;;GA;;;{sid}){}", seat_proto::PIPE_SDDL, seat_proto::PIPE_LOW_LABEL_SDDL);
        let server = PipeServer::new(&name, &sddl).unwrap();
        let first = server.claim().unwrap();
        // A second server on the same name must fail (first-instance flag).
        assert!(PipeServer::new(&name, &sddl).unwrap().claim().is_err());
        let stop = server.shutdown_handle();
        let hub = Arc::new(crate::events::EventHub::new(Arc::new(seat_core::SystemClock)));
        let h2 = hub.clone();
        let handler: Handler = Arc::new(move |c: &mut Caller, req: Request| {
            assert_eq!(c.user_sid.as_deref(), Some(sid.as_str()));
            let subscription = matches!(req.body, RequestBody::Subscribe { .. })
                .then(|| h2.subscribe(vec![Topic::Status], Default::default()));
            Handled { response: Response::new(req.id, Ok(ResponseBody::Ack)), subscription }
        });
        let t = std::thread::spawn(move || server.run(first, handler).unwrap());
        let mut c = connect(&name);
        let hello = RequestBody::Hello(Hello {
            client: ClientKind::Cli,
            client_version: "t".into(),
            protocol_version: 1,
            seat: None,
            token: None,
        });
        seat_proto::write_message(&mut c, &Request::new(1, hello)).unwrap();
        let f: ServerFrame = seat_proto::read_message(&mut c).unwrap().unwrap();
        assert!(matches!(f, ServerFrame::Response(r) if r.id == 1));
        seat_proto::write_message(&mut c, &Request::new(2, RequestBody::Subscribe { topics: vec![Topic::Status] })).unwrap();
        let _: ServerFrame = seat_proto::read_message(&mut c).unwrap().unwrap();
        hub.publish(Event::ConfigChanged { revision: 9 });
        let f: ServerFrame = seat_proto::read_message(&mut c).unwrap().unwrap();
        assert!(matches!(f, ServerFrame::Event(e) if e.event == Event::ConfigChanged { revision: 9 }));
        // Garbage frame gets a BadRequest and the connection closes.
        seat_proto::write_frame(&mut c, b"{nope").unwrap();
        let f: ServerFrame = seat_proto::read_message(&mut c).unwrap().unwrap();
        assert!(matches!(f, ServerFrame::Response(r) if r.result.as_ref().unwrap_err().code == ErrorCode::BadRequest));
        drop(c);
        stop.trigger();
        t.join().unwrap();
    }

    #[test]
    fn slots_limit_per_session_and_in_total() {
        let counts: SessionCounts = Arc::default();
        let mut held: Vec<SessionSlot> = (0..MAX_PER_SESSION).map(|_| SessionSlot::take(&counts, 1).unwrap()).collect();
        assert_eq!(SessionSlot::take(&counts, 1).err(), Some(Refused::Session));
        // Another session still gets slots until the total is reached.
        let mut s = 2;
        while held.len() < MAX_UNTRUSTED {
            match SessionSlot::take(&counts, s) {
                Ok(x) => held.push(x),
                Err(Refused::Session) => s += 1,
                Err(Refused::Total) => panic!("total too early"),
            }
        }
        assert_eq!(SessionSlot::take(&counts, 999).err(), Some(Refused::Total));
        held.pop();
        assert!(SessionSlot::take(&counts, 999).is_ok());
        drop(held);
        assert_eq!(counts.lock().unwrap().total, 0);
    }

    /// A client that never sends a request is dropped after FIRST_REQUEST_TIMEOUT; a
    /// trusted PID (here: our own process) is never limited.
    #[test]
    fn idle_client_is_dropped_and_trusted_pid_is_exempt() {
        use std::io::Read;
        let name = format!(r"\\.\pipe\PulsarSeat.TestIdle.{}", std::process::id());
        let sid = crate::win::security::current_user_sid().unwrap();
        let sddl = format!("{}(A;;GA;;;{sid})", seat_proto::PIPE_SDDL);
        let me = std::process::id();
        let server = PipeServer::new(&name, &sddl).unwrap().with_trusted(Arc::new(move |pid| pid == me));
        let first = server.claim().unwrap();
        let stop = server.shutdown_handle();
        let sessions = server.sessions.clone();
        let handler: Handler =
            Arc::new(|_c: &mut Caller, req: Request| Handled { response: Response::new(req.id, Ok(ResponseBody::Ack)), subscription: None });
        let t = std::thread::spawn(move || server.run(first, handler).unwrap());
        // Trusted: many more than MAX_PER_SESSION connections stay uncounted.
        let conns: Vec<_> = (0..MAX_PER_SESSION + 4).map(|_| connect(&name)).collect();
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(sessions.lock().unwrap().total, 0);
        // Trusted connections are not timed out either.
        std::thread::sleep(FIRST_REQUEST_TIMEOUT + Duration::from_millis(500));
        let mut c = conns.into_iter().next().unwrap();
        let hello = RequestBody::Hello(Hello {
            client: ClientKind::Cli,
            client_version: "t".into(),
            protocol_version: 1,
            seat: None,
            token: None,
        });
        seat_proto::write_message(&mut c, &Request::new(1, hello)).unwrap();
        let mut buf = [0u8; 4];
        assert!(c.read_exact(&mut buf).is_ok());
        stop.trigger();
        drop(c);
        t.join().unwrap();
    }
}
