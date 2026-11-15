// SPDX-License-Identifier: GPL-3.0-only
//! Control-pipe client of the seat host and the session agent.
//!
//! Two connections per process: one for request / response ([`Conn::call`]) and one that
//! only reads pushed events after `subscribe` ([`Conn::next_event`]). A synchronous pipe
//! handle serialises its reads and writes, so a blocked event read on the request
//! connection would stall every request; two connections avoid that.
//!
//! Pipe security (same as the manager and `seat_compat::seatapi`): the pipe is opened at
//! **identification** level (`SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION`), and the
//! client only talks to a pipe whose server process is the running `PulsarSeat` service
//! (`GetNamedPipeServerProcessId` vs `QueryServiceStatusEx`). A local user who creates the
//! pipe name first while the service is down learns nothing (the host would otherwise send
//! its one-time token and receive the seat password).

use seat_core::SeatNumber;
use seat_core::Secret;
use seat_proto::messages::{ClientKind, Hello, RequestBody, ResponseBody, Topic};
use seat_proto::{EventFrame, PROTOCOL_VERSION, ProtoError, Request, ServerFrame, read_message, write_message};
use std::io::{Read, Write};

#[derive(Debug, thiserror::Error)]
pub enum PipeError {
    #[error("pipe not reachable: {0}")]
    Connect(std::io::Error),
    #[error("pipe server is not the Pulsar Seat service: {0}")]
    NotService(String),
    #[error("pipe I/O: {0}")]
    Frame(#[from] seat_proto::FrameError),
    #[error("service closed the connection")]
    Closed,
    #[error("service error: {0}")]
    Service(ProtoError),
    #[error("unexpected response")]
    Unexpected,
}

/// One connection over any byte stream (the pipe in production, buffers in tests).
pub struct Conn<S: Read + Write> {
    stream: S,
    next_id: u64,
}

impl<S: Read + Write> Conn<S> {
    pub fn over(stream: S) -> Self {
        Self { stream, next_id: 1 }
    }

    /// Send a request and wait for its response; events and other ids in between are
    /// skipped (this connection is not subscribed, so there should be none).
    pub fn call(&mut self, body: RequestBody) -> Result<ResponseBody, PipeError> {
        let id = self.next_id;
        self.next_id += 1;
        write_message(&mut self.stream, &Request::new(id, body))?;
        loop {
            match read_message::<_, ServerFrame>(&mut self.stream)?.ok_or(PipeError::Closed)? {
                ServerFrame::Response(r) if r.id == id => return r.result.map_err(PipeError::Service),
                _ => continue,
            }
        }
    }

    /// `hello` (must be first). `seat` + `token` for a seat host.
    pub fn hello(&mut self, client: ClientKind, seat: Option<SeatNumber>, token: Option<Secret>) -> Result<(), PipeError> {
        let h = Hello { client, client_version: env!("CARGO_PKG_VERSION").into(), protocol_version: PROTOCOL_VERSION, seat, token };
        match self.call(RequestBody::Hello(h))? {
            ResponseBody::Hello(_) => Ok(()),
            _ => Err(PipeError::Unexpected),
        }
    }

    pub fn subscribe(&mut self, topics: Vec<Topic>) -> Result<(), PipeError> {
        match self.call(RequestBody::Subscribe { topics })? {
            ResponseBody::Ack => Ok(()),
            _ => Err(PipeError::Unexpected),
        }
    }

    /// Next pushed event (blocking). `Ok(None)`: the service closed the pipe.
    pub fn next_event(&mut self) -> Result<Option<EventFrame>, PipeError> {
        loop {
            match read_message::<_, ServerFrame>(&mut self.stream)? {
                None => return Ok(None),
                Some(ServerFrame::Event(e)) => return Ok(Some(e)),
                Some(ServerFrame::Response(_)) => continue,
            }
        }
    }
}

/// Open the production pipe and check that its server is the running service.
#[cfg(windows)]
pub fn open() -> Result<Conn<std::fs::File>, PipeError> {
    use std::os::windows::fs::OpenOptionsExt;
    // std adds SECURITY_SQOS_PRESENT when QoS flags are set.
    const SECURITY_IDENTIFICATION: u32 = 0x0001_0000;
    let open = || {
        std::fs::OpenOptions::new()
            // No GENERIC_WRITE: the pipe DACL denies FILE_CREATE_PIPE_INSTANCE to users.
            .access_mode(seat_proto::PIPE_CLIENT_ACCESS)
            .security_qos_flags(SECURITY_IDENTIFICATION)
            .open(seat_proto::PIPE_NAME)
    };
    let mut tries = 0;
    let f = loop {
        match open() {
            Ok(f) => break f,
            // The service listens on one free instance at a time and creates the next right
            // after a connect: a client arriving in between gets ERROR_PIPE_BUSY. Documented
            // client handling: WaitNamedPipe, then open again (VM test 2026-11-15: the
            // console listener, started together with the kiosk shell, failed on it).
            Err(e) if is_pipe_busy(&e) && tries < PIPE_BUSY_RETRIES => {
                tries += 1;
                let name: Vec<u16> = seat_proto::PIPE_NAME.encode_utf16().chain(std::iter::once(0)).collect();
                // SAFETY: NUL-terminated name; waits at most PIPE_BUSY_WAIT_MS.
                unsafe { windows_sys::Win32::System::Pipes::WaitNamedPipeW(name.as_ptr(), PIPE_BUSY_WAIT_MS) };
            }
            Err(e) => return Err(PipeError::Connect(e)),
        }
    };
    verify_server(&f)?;
    Ok(Conn::over(f))
}

/// Opens tried again after `ERROR_PIPE_BUSY`, each after a `WaitNamedPipeW`.
pub const PIPE_BUSY_RETRIES: u32 = 10;
/// Longest `WaitNamedPipeW` per retry.
pub const PIPE_BUSY_WAIT_MS: u32 = 1_000;

/// The service PID from [`seat_core::consts::SERVICE_PID_ENV`] (`None`: absent or not a
/// non-zero number, then the SCM is asked).
pub fn expected_service_pid(v: Option<&str>) -> Option<u32> {
    v.and_then(|s| s.trim().parse::<u32>().ok()).filter(|p| *p != 0)
}

/// `ERROR_PIPE_BUSY` (231): every instance of the pipe is connected right now.
pub fn is_pipe_busy(e: &std::io::Error) -> bool {
    e.raw_os_error() == Some(231)
}

#[cfg(windows)]
fn verify_server(f: &std::fs::File) -> Result<(), PipeError> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId;
    let mut server = 0u32;
    // SAFETY: valid pipe handle, out parameter.
    if unsafe { GetNamedPipeServerProcessId(f.as_raw_handle() as _, &mut server) } == 0 {
        return Err(PipeError::NotService(format!("GetNamedPipeServerProcessId: {}", std::io::Error::last_os_error())));
    }
    // Launched by the service: it gave its PID (the SCM may be closed to a contained host).
    let service = match expected_service_pid(std::env::var(seat_core::consts::SERVICE_PID_ENV).ok().as_deref()) {
        Some(pid) => pid,
        None => service_pid(seat_core::consts::SERVICE_NAME).map_err(PipeError::NotService)?,
    };
    if server != service {
        return Err(PipeError::NotService(format!("server pid {server}, service pid {service}")));
    }
    Ok(())
}

/// PID of a running service. Adapted from `seat_compat::seatapi::service_pid` (same
/// repository; not a dependency of the host).
#[cfg(windows)]
fn service_pid(name: &str) -> Result<u32, String> {
    use windows_sys::Win32::System::Services::{
        CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceStatusEx, SC_MANAGER_CONNECT, SC_STATUS_PROCESS_INFO,
        SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_STATUS_PROCESS,
    };
    let w: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: plain SCM calls; handles closed below.
    unsafe {
        let scm = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT);
        if scm.is_null() {
            return Err(format!("OpenSCManagerW: {}", std::io::Error::last_os_error()));
        }
        let svc = OpenServiceW(scm, w.as_ptr(), SERVICE_QUERY_STATUS);
        if svc.is_null() {
            let e = std::io::Error::last_os_error();
            CloseServiceHandle(scm);
            return Err(format!("OpenServiceW({name}): {e}"));
        }
        let mut st: SERVICE_STATUS_PROCESS = std::mem::zeroed();
        let mut need = 0u32;
        let ok = QueryServiceStatusEx(
            svc,
            SC_STATUS_PROCESS_INFO,
            &mut st as *mut _ as *mut u8,
            std::mem::size_of::<SERVICE_STATUS_PROCESS>() as u32,
            &mut need,
        );
        let e = std::io::Error::last_os_error();
        CloseServiceHandle(svc);
        CloseServiceHandle(scm);
        if ok == 0 {
            return Err(format!("QueryServiceStatusEx: {e}"));
        }
        if st.dwCurrentState != SERVICE_RUNNING || st.dwProcessId == 0 {
            return Err(format!("{name} is not running"));
        }
        Ok(st.dwProcessId)
    }
}

#[cfg(test)]
pub mod testing {
    //! In-memory duplex: scripted server frames in, client requests out.
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    pub struct Duplex {
        pub input: Arc<Mutex<std::io::Cursor<Vec<u8>>>>,
        pub output: Arc<Mutex<Vec<u8>>>,
    }

    impl Duplex {
        pub fn with_frames(frames: &[ServerFrame]) -> Self {
            let mut buf = Vec::new();
            for f in frames {
                write_message(&mut buf, f).unwrap();
            }
            Self { input: Arc::new(Mutex::new(std::io::Cursor::new(buf))), output: Arc::default() }
        }
        /// Requests the client wrote, decoded.
        pub fn requests(&self) -> Vec<Request> {
            let out = self.output.lock().unwrap().clone();
            let mut c = std::io::Cursor::new(out);
            let mut v = Vec::new();
            while let Ok(Some(r)) = read_message::<_, Request>(&mut c) {
                v.push(r);
            }
            v
        }
    }

    impl Read for Duplex {
        fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
            self.input.lock().unwrap().read(b)
        }
    }
    impl Write for Duplex {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.output.lock().unwrap().write(b)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn service_pid_from_the_environment() {
        assert_eq!(super::expected_service_pid(Some("4242")), Some(4242));
        assert_eq!(super::expected_service_pid(Some(" 7 ")), Some(7));
        assert_eq!(super::expected_service_pid(Some("0")), None);
        assert_eq!(super::expected_service_pid(Some("x")), None);
        assert_eq!(super::expected_service_pid(None), None);
    }

    #[test]
    fn pipe_busy_is_retried_and_nothing_else() {
        assert!(super::is_pipe_busy(&std::io::Error::from_raw_os_error(231)));
        assert!(!super::is_pipe_busy(&std::io::Error::from_raw_os_error(5)));
        assert!(!super::is_pipe_busy(&std::io::Error::from_raw_os_error(2)));
    }

    use super::testing::Duplex;
    use super::*;
    use seat_proto::Response;
    use seat_proto::messages::{CallerInfo, HelloReply};

    fn hello_reply(id: u64) -> ServerFrame {
        ServerFrame::Response(Response::new(
            id,
            Ok(ResponseBody::Hello(HelloReply {
                service_version: "0.1.0".into(),
                protocol_version: PROTOCOL_VERSION,
                min_protocol_version: 1,
                caller: CallerInfo { client: ClientKind::SeatHost, session_id: 1, seat: None, is_admin: false, restricted: false },
            })),
        ))
    }

    /// The real framing: hello carries seat + token; stray frames are skipped.
    #[test]
    fn hello_then_call_skips_strays() {
        let stray = ServerFrame::Response(Response::new(99, Ok(ResponseBody::Ack)));
        let d = Duplex::with_frames(&[hello_reply(1), stray, ServerFrame::Response(Response::new(2, Ok(ResponseBody::Ack)))]);
        let mut c = Conn::over(d.clone());
        c.hello(ClientKind::SeatHost, SeatNumber::new(2), Some(Secret::new("tok"))).unwrap();
        assert!(matches!(c.call(RequestBody::GetStatus).unwrap(), ResponseBody::Ack));
        let reqs = d.requests();
        assert_eq!(reqs.len(), 2);
        match &reqs[0].body {
            RequestBody::Hello(h) => {
                assert_eq!(h.client, ClientKind::SeatHost);
                assert_eq!(h.seat, SeatNumber::new(2));
                assert_eq!(h.token.as_ref().map(|t| t.expose().to_string()), Some("tok".into()));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn service_error_and_close() {
        let e = ProtoError::new(seat_proto::ErrorCode::AccessDenied, "no");
        let d = Duplex::with_frames(&[ServerFrame::Response(Response::new(1, Err(e)))]);
        let mut c = Conn::over(d);
        assert!(matches!(c.call(RequestBody::GetStatus), Err(PipeError::Service(_))));
        assert!(matches!(c.call(RequestBody::GetStatus), Err(PipeError::Closed)));
    }

    #[test]
    fn events_after_subscribe() {
        let ev = ServerFrame::Event(EventFrame {
            seq: 5,
            at_ms: 0,
            event: seat_proto::Event::HostCommand { seat: SeatNumber::new(2).unwrap(), command: seat_proto::HostCommand::Exit },
        });
        let d = Duplex::with_frames(&[ServerFrame::Response(Response::new(1, Ok(ResponseBody::Ack))), ev]);
        let mut c = Conn::over(d);
        c.subscribe(vec![Topic::Host]).unwrap();
        assert_eq!(c.next_event().unwrap().unwrap().seq, 5);
        assert!(c.next_event().unwrap().is_none());
    }
}
