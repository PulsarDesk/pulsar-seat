// SPDX-License-Identifier: GPL-3.0-only
//! Control-pipe client: one connection per request, `hello` first (architecture §5).
//! Adapted from pulsar-split (`pipe_client.rs`).
//!
//! Before anything is written to the pipe, the client checks the pipe server and fails
//! closed (`AccessDenied`, reason `pipe_server_untrusted`) on any doubt:
//! - `GetNamedPipeServerSessionId` must report session 0 (read from the pipe itself, no
//!   process handle needed, so a squatter cannot make the check fail open);
//! - `GetNamedPipeServerProcessId` must equal the PID the SCM reports for our service
//!   (`QueryServiceStatusEx`; `SERVICE_QUERY_STATUS` is granted to interactive users).
//!
//! A user process that grabbed the pipe name first therefore never receives a request (so
//! never a seat password). The service itself creates the first instance with
//! `FILE_FLAG_FIRST_PIPE_INSTANCE`, so this is defence in depth. Untested on hardware.
//!
//! The pipe is opened with `SECURITY_IDENTIFICATION` QoS: the server can identify the
//! manager's token (all the service needs: `ImpersonateNamedPipeClient` +
//! `OpenThreadToken(TOKEN_QUERY)` + `CheckTokenMembership` work at that level) but cannot
//! act as the manager.

use seat_proto::{
    ClientKind, ErrorCode, EventFrame, Hello, HelloReply, PIPE_NAME, PROTOCOL_VERSION, ProtoError, Request,
    RequestBody, Response, ResponseBody, ServerFrame, Topic,
};
use std::fs::{File, OpenOptions};
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

const ERROR_PIPE_BUSY: i32 = 231;
/// `SECURITY_IDENTIFICATION` (= `SecurityIdentification << 16`); std adds
/// `SECURITY_SQOS_PRESENT` itself when the flags are non-zero.
#[cfg(windows)]
const SECURITY_IDENTIFICATION: u32 = 0x0001_0000;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn unavailable(msg: impl Into<String>) -> ProtoError {
    ProtoError::new(ErrorCode::ServiceUnavailable, msg)
}

fn open_options() -> OpenOptions {
    let mut o = OpenOptions::new();
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // No GENERIC_WRITE: the pipe DACL denies FILE_CREATE_PIPE_INSTANCE to users.
        o.access_mode(seat_proto::PIPE_CLIENT_ACCESS);
        o.security_qos_flags(SECURITY_IDENTIFICATION);
    }
    #[cfg(not(windows))]
    o.read(true).write(true);
    o
}

fn open() -> Result<File, ProtoError> {
    for _ in 0..20 {
        match open_options().open(PIPE_NAME) {
            Ok(f) => {
                verify_server(&f)?;
                return Ok(f);
            }
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                return Err(ProtoError::new(ErrorCode::AccessDenied, "pipe access denied"));
            }
            Err(e) => return Err(unavailable(format!("service not reachable: {e}"))),
        }
    }
    Err(unavailable("service pipe busy"))
}

fn untrusted(msg: impl Into<String>) -> ProtoError {
    ProtoError::new(ErrorCode::AccessDenied, msg).with_reason("pipe_server_untrusted")
}

/// The pipe server must be our LocalSystem service: session 0 and the SCM's service PID.
/// Fails closed on every error.
#[cfg(windows)]
fn verify_server(pipe: &File) -> Result<(), ProtoError> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::Pipes::{GetNamedPipeServerProcessId, GetNamedPipeServerSessionId};

    let h = pipe.as_raw_handle();
    let mut session = u32::MAX;
    // SAFETY: `h` is a valid, open pipe handle owned by `pipe`; `session` outlives the call.
    if unsafe { GetNamedPipeServerSessionId(h, &mut session) } == 0 {
        return Err(untrusted(format!("cannot read pipe server session: {}", io::Error::last_os_error())));
    }
    if session != 0 {
        return Err(untrusted(format!("pipe server runs in session {session}, not a service")));
    }
    let mut pid = 0u32;
    // SAFETY: as above.
    if unsafe { GetNamedPipeServerProcessId(h, &mut pid) } == 0 {
        return Err(untrusted(format!("cannot identify pipe server: {}", io::Error::last_os_error())));
    }
    let service_pid = service_pid().map_err(|e| untrusted(format!("cannot query service PID: {e}")))?;
    if service_pid == 0 || pid != service_pid {
        return Err(untrusted(format!("pipe server PID {pid} is not the service (PID {service_pid})")));
    }
    Ok(())
}

/// PID of the running seat service as the SCM reports it (0 when it is not running).
#[cfg(windows)]
fn service_pid() -> io::Result<u32> {
    use windows_sys::Win32::System::Services::{
        CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceStatusEx, SC_MANAGER_CONNECT,
        SC_STATUS_PROCESS_INFO, SERVICE_QUERY_STATUS, SERVICE_STATUS_PROCESS,
    };

    let name: Vec<u16> = seat_core::consts::SERVICE_NAME.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: null machine / database = local SCM, active database.
    let scm = unsafe { OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT) };
    if scm.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `scm` is open; `name` is NUL-terminated and outlives the call.
    let svc = unsafe { OpenServiceW(scm, name.as_ptr(), SERVICE_QUERY_STATUS) };
    if svc.is_null() {
        let e = io::Error::last_os_error();
        // SAFETY: opened above.
        unsafe { CloseServiceHandle(scm) };
        return Err(e);
    }
    // SAFETY: zero is a valid bit pattern for this plain C struct.
    let mut st: SERVICE_STATUS_PROCESS = unsafe { std::mem::zeroed() };
    let mut needed = 0u32;
    // SAFETY: the buffer is a SERVICE_STATUS_PROCESS of exactly the size passed.
    let ok = unsafe {
        QueryServiceStatusEx(
            svc,
            SC_STATUS_PROCESS_INFO,
            (&mut st as *mut SERVICE_STATUS_PROCESS).cast(),
            std::mem::size_of::<SERVICE_STATUS_PROCESS>() as u32,
            &mut needed,
        )
    };
    let r = if ok == 0 { Err(io::Error::last_os_error()) } else { Ok(st.dwProcessId) };
    // SAFETY: both handles opened above.
    unsafe {
        CloseServiceHandle(svc);
        CloseServiceHandle(scm);
    }
    r
}

#[cfg(not(windows))]
fn verify_server(_pipe: &File) -> Result<(), ProtoError> {
    Ok(())
}

fn send(pipe: &mut File, body: RequestBody) -> Result<u64, ProtoError> {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    seat_proto::write_message(pipe, &Request::new(id, body)).map_err(|e| unavailable(e.to_string()))?;
    Ok(id)
}

fn read_frame(pipe: &mut File) -> Result<ServerFrame, ProtoError> {
    seat_proto::read_message(pipe)
        .map_err(|e| unavailable(e.to_string()))?
        .ok_or_else(|| unavailable("service closed the connection"))
}

/// What one raw frame on the subscription connection turned out to be.
#[derive(Debug)]
enum SubFrame {
    Frame(Box<ServerFrame>),
    /// A complete frame whose JSON we do not understand (newer service, unknown event).
    /// The length prefix was consumed, so the stream is still in sync.
    Undecodable(String),
    /// EOF or an I/O / framing error: the connection is gone.
    Closed,
}

fn read_sub_frame(r: &mut impl io::Read) -> SubFrame {
    match seat_proto::read_frame(r) {
        Ok(Some(payload)) => match serde_json::from_slice::<ServerFrame>(&payload) {
            Ok(f) => SubFrame::Frame(Box::new(f)),
            Err(e) => SubFrame::Undecodable(e.to_string()),
        },
        Ok(None) | Err(_) => SubFrame::Closed,
    }
}

/// Wait for the response to `id`, skipping event frames.
fn read_response(pipe: &mut File, id: u64) -> Result<ResponseBody, ProtoError> {
    loop {
        let ServerFrame::Response(resp) = read_frame(pipe)? else { continue };
        let resp: Response = resp;
        if resp.version != PROTOCOL_VERSION {
            return Err(ProtoError::new(ErrorCode::VersionMismatch, format!("service speaks {}", resp.version)));
        }
        if resp.id != id && resp.id != 0 {
            return Err(ProtoError::new(ErrorCode::Internal, "response id mismatch"));
        }
        return resp.result;
    }
}

fn roundtrip(pipe: &mut File, body: RequestBody) -> Result<ResponseBody, ProtoError> {
    let id = send(pipe, body)?;
    read_response(pipe, id)
}

fn hello_body() -> RequestBody {
    RequestBody::Hello(Hello {
        client: ClientKind::Manager,
        client_version: env!("CARGO_PKG_VERSION").into(),
        protocol_version: PROTOCOL_VERSION,
        seat: None,
        token: None,
    })
}

fn connect() -> Result<(File, HelloReply), ProtoError> {
    let mut pipe = open()?;
    match roundtrip(&mut pipe, hello_body())? {
        ResponseBody::Hello(reply) => Ok((pipe, reply)),
        other => Err(ProtoError::new(ErrorCode::Internal, format!("unexpected hello reply: {other:?}"))),
    }
}

/// `hello` only: who the service thinks we are (admin, restricted, seat).
pub fn hello() -> Result<HelloReply, ProtoError> {
    connect().map(|(_, reply)| reply)
}

/// One request on its own connection.
pub fn call(body: RequestBody) -> Result<ResponseBody, ProtoError> {
    if matches!(body, RequestBody::Hello(_) | RequestBody::Subscribe { .. }) {
        return Err(ProtoError::new(ErrorCode::BadRequest, "hello / subscribe are handled by the client"));
    }
    let (mut pipe, _) = connect()?;
    roundtrip(&mut pipe, body)
}

/// Keep one subscription connection; call `on_event` for every pushed event and
/// `on_link(true/false)` when the connection comes up or drops. Returns only when
/// `keep_going` says so (checked between reconnects).
pub fn subscribe_loop(
    topics: Vec<Topic>,
    keep_going: impl Fn() -> bool,
    on_link: impl Fn(bool),
    on_event: impl Fn(EventFrame),
) {
    let mut backoff = Duration::from_millis(500);
    while keep_going() {
        // A connection that came up and later dropped resets the back-off.
        if subscribe_once(&topics, &on_link, &on_event).is_ok() {
            backoff = Duration::from_millis(500);
        }
        on_link(false);
        std::thread::sleep(backoff);
        backoff = (backoff * 2).min(Duration::from_secs(5));
    }
}

fn subscribe_once(topics: &[Topic], on_link: &impl Fn(bool), on_event: &impl Fn(EventFrame)) -> Result<(), ProtoError> {
    let (mut pipe, _) = connect()?;
    roundtrip(&mut pipe, RequestBody::Subscribe { topics: topics.to_vec() })?;
    on_link(true);
    // Once linked, a dropped connection counts as a normal end (Ok): the caller retries fast.
    // A frame we cannot decode is skipped, not treated as a disconnect.
    loop {
        match read_sub_frame(&mut pipe) {
            SubFrame::Frame(f) => {
                if let ServerFrame::Event(ev) = *f {
                    on_event(ev);
                }
            }
            SubFrame::Undecodable(e) => eprintln!("pulsar-seat-manager: skipped undecodable event frame: {e}"),
            SubFrame::Closed => return Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_and_subscribe_are_not_forwarded() {
        let e = call(RequestBody::Subscribe { topics: vec![] }).unwrap_err();
        assert_eq!(e.code, ErrorCode::BadRequest);
        let e = call(hello_body()).unwrap_err();
        assert_eq!(e.code, ErrorCode::BadRequest);
    }

    #[test]
    fn undecodable_frame_is_skipped_and_stream_stays_in_sync() {
        let mut buf = Vec::new();
        seat_proto::write_frame(&mut buf, br#"{"kind":"from_the_future","x":1}"#).unwrap();
        seat_proto::write_frame(&mut buf, b"not json").unwrap();
        let mut r = std::io::Cursor::new(buf);
        assert!(matches!(read_sub_frame(&mut r), SubFrame::Undecodable(_)));
        assert!(matches!(read_sub_frame(&mut r), SubFrame::Undecodable(_)));
        assert!(matches!(read_sub_frame(&mut r), SubFrame::Closed));
    }

    #[test]
    fn truncated_frame_closes() {
        let mut buf = 10u32.to_le_bytes().to_vec();
        buf.extend_from_slice(b"abc");
        assert!(matches!(read_sub_frame(&mut std::io::Cursor::new(buf)), SubFrame::Closed));
    }

    /// From Pulsar Split's manager (merge step 9): the protocol 3 requests that carry a
    /// secret go through the same fail-closed server check as every other request, and their
    /// secret never shows in a debug print (the manager logs request errors).
    #[test]
    fn secret_requests_fail_closed_and_never_print_the_secret() {
        let seat = seat_core::SeatNumber::new(2).unwrap();
        let s = |v: &str| seat_core::Secret::new(v);
        let bodies = [
            RequestBody::SetSeatPassword { seat, password: Some(s("hunter2-x")) },
            RequestBody::SubmitPin { seat, pin: s("hunter2-x"), client_name: "tv".into(), request_id: None },
            RequestBody::SetSeatConnectPassword { seat, password: s("hunter2-x") },
            RequestBody::AnswerRelayAuth { seat, password: s("hunter2-x"), totp: Some(s("hunter2-x")) },
        ];
        for b in &bodies {
            assert!(!format!("{b:?}").contains("hunter2-x"), "{b:?}");
        }
        let e = untrusted("session 2");
        assert_eq!(e.code, ErrorCode::AccessDenied);
        assert_eq!(e.reason.as_deref(), Some("pipe_server_untrusted"));
    }

    #[test]
    fn hello_identifies_the_manager() {
        let RequestBody::Hello(h) = hello_body() else { panic!() };
        assert_eq!(h.client, ClientKind::Manager);
        assert_eq!(h.protocol_version, PROTOCOL_VERSION);
        assert!(h.token.is_none());
    }
}
