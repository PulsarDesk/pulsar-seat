// SPDX-License-Identifier: GPL-3.0-only
//! "Which seat am I on" API for apps (spec 80 §6, §11.5).
//!
//! The service answers `my_seat` / `seat_of_session` / `seat_of_process` on the control
//! pipe (access level `any`). This module holds:
//!
//! - the pure lookups the service uses ([`seat_of_session`]);
//! - a tiny blocking pipe [`Client`] (one connection per query, `hello` first, stray event
//!   frames skipped) that a helper DLL or a launcher can embed;
//! - the compatibility mapping for the three drop-in names (`GetMaxTerminalsCount`,
//!   `ProcessIdToTerminalId`, `SessionIdToTerminalId`). Their original signatures are not
//!   confirmed (spec 80 §12 q6), so only the Pulsar-native exports are built
//!   (`crates/seat-compat/apihelper`, x86 / x64 / ARM64 / ARM64EC); [`compat`] documents
//!   the convention (seat numbers from 1, 0 = not on a seat / unknown).
//!
//! Pipe security ([`connect`]): the client opens the pipe at **identification** level
//! (`SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION`), so a server can learn who called
//! but never act as the caller, and it talks only to a pipe whose server process is the
//! running `PulsarSeat` service (`GetNamedPipeServerProcessId` vs `QueryServiceStatusEx`).
//! A local user who creates the pipe name first while the service is down gets nothing.

use seat_core::{MAX_SEATS, SeatNumber};
use seat_proto::messages::{ClientKind, Hello, RequestBody, ResponseBody};
use seat_proto::{
    PIPE_NAME, PROTOCOL_VERSION, ProtoError, Request, ServerFrame, read_message, write_message,
};
use std::io::{Read, Write};

/// Seat of a session from the service's session map.
pub fn seat_of_session(map: &[(SeatNumber, u32)], session_id: u32) -> Option<SeatNumber> {
    map.iter().find(|(_, s)| *s == session_id).map(|(n, _)| *n)
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("Pulsar Seat service is not running or the pipe is not reachable: {0}")]
    Connect(std::io::Error),
    #[error("pipe I/O: {0}")]
    Frame(#[from] seat_proto::FrameError),
    #[error("service closed the connection")]
    Closed,
    #[error("service error: {0:?}")]
    Service(ProtoError),
    #[error("unexpected response")]
    Unexpected,
    #[error("the pipe server is not the Pulsar Seat service: {0}")]
    NotService(String),
}

/// One-shot client over any byte stream (the pipe in production, a buffer in tests).
pub struct Client<S: Read + Write> {
    stream: S,
    next_id: u64,
}

impl<S: Read + Write> Client<S> {
    pub fn over(stream: S) -> Self {
        Self { stream, next_id: 1 }
    }

    fn call(&mut self, body: RequestBody) -> Result<ResponseBody, ClientError> {
        let id = self.next_id;
        self.next_id += 1;
        write_message(&mut self.stream, &Request::new(id, body))?;
        loop {
            let f: ServerFrame = read_message(&mut self.stream)?.ok_or(ClientError::Closed)?;
            match f {
                ServerFrame::Response(r) if r.id == id => {
                    return r.result.map_err(ClientError::Service);
                }
                _ => continue, // stray events / other ids
            }
        }
    }

    pub fn hello(&mut self) -> Result<(), ClientError> {
        let h = Hello {
            client: ClientKind::AppApi,
            client_version: env!("CARGO_PKG_VERSION").into(),
            protocol_version: PROTOCOL_VERSION,
            seat: None,
            token: None,
        };
        match self.call(RequestBody::Hello(h))? {
            ResponseBody::Hello(_) => Ok(()),
            _ => Err(ClientError::Unexpected),
        }
    }

    fn seat(&mut self, body: RequestBody) -> Result<Option<SeatNumber>, ClientError> {
        match self.call(body)? {
            ResponseBody::Seat { seat, .. } => Ok(seat),
            _ => Err(ClientError::Unexpected),
        }
    }

    pub fn my_seat(&mut self) -> Result<Option<SeatNumber>, ClientError> {
        self.seat(RequestBody::MySeat)
    }
    pub fn seat_of_session(&mut self, session_id: u32) -> Result<Option<SeatNumber>, ClientError> {
        self.seat(RequestBody::SeatOfSession { session_id })
    }
    pub fn seat_of_process(&mut self, pid: u32) -> Result<Option<SeatNumber>, ClientError> {
        self.seat(RequestBody::SeatOfProcess { pid })
    }
}

/// Connect to the service pipe (identification level only, server verified as the
/// running service) and say hello.
#[cfg(windows)]
pub fn connect() -> Result<Client<std::fs::File>, ClientError> {
    use std::os::windows::fs::OpenOptionsExt;
    // std adds SECURITY_SQOS_PRESENT when qos flags are set.
    const SECURITY_IDENTIFICATION: u32 = 0x0001_0000;
    let f = std::fs::OpenOptions::new()
        // No GENERIC_WRITE: the pipe DACL denies FILE_CREATE_PIPE_INSTANCE to users.
        .access_mode(seat_proto::PIPE_CLIENT_ACCESS)
        .security_qos_flags(SECURITY_IDENTIFICATION)
        .open(PIPE_NAME)
        .map_err(ClientError::Connect)?;
    verify_server(&f)?;
    let mut c = Client::over(f);
    c.hello()?;
    Ok(c)
}

/// The pipe's server process is the running `PulsarSeat` service.
#[cfg(windows)]
#[allow(unsafe_code)]
fn verify_server(f: &std::fs::File) -> Result<(), ClientError> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId;
    let mut server = 0u32;
    // SAFETY: valid pipe handle, out parameter.
    if unsafe { GetNamedPipeServerProcessId(f.as_raw_handle() as _, &mut server) } == 0 {
        return Err(ClientError::NotService(format!(
            "GetNamedPipeServerProcessId: {}",
            std::io::Error::last_os_error()
        )));
    }
    let service = service_pid(seat_core::consts::SERVICE_NAME).map_err(ClientError::NotService)?;
    if server != service {
        return Err(ClientError::NotService(format!(
            "server pid {server}, service pid {service}"
        )));
    }
    Ok(())
}

/// PID of a running service (`0` / error when stopped).
#[cfg(windows)]
#[allow(unsafe_code)]
pub fn service_pid(name: &str) -> Result<u32, String> {
    use windows_sys::Win32::System::Services::{
        CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceStatusEx, SC_MANAGER_CONNECT,
        SC_STATUS_PROCESS_INFO, SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_STATUS_PROCESS,
    };
    let w = crate::win::wide(name);
    // SAFETY: plain SCM calls; handles closed below.
    unsafe {
        let scm = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT);
        if scm.is_null() {
            return Err(format!(
                "OpenSCManagerW: {}",
                std::io::Error::last_os_error()
            ));
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

/// Pipe name, for documentation of the app API.
pub const fn pipe_name() -> &'static str {
    PIPE_NAME
}

/// Drop-in naming convention for a future helper DLL (x86, x64, ARM64, ARM64EC).
pub mod compat {
    use super::*;

    /// `GetMaxTerminalsCount`: our seat limit.
    pub const fn max_terminals() -> u32 {
        MAX_SEATS as u32
    }
    /// `*ToTerminalId` result: seat number (1-based), 0 = not on a seat / service down.
    pub fn terminal_id(seat: Result<Option<SeatNumber>, ClientError>) -> u32 {
        match seat {
            Ok(Some(s)) => s.get() as u32,
            _ => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use seat_proto::Response;
    use seat_proto::messages::{CallerInfo, HelloReply};
    use std::io::Cursor;

    /// Fake duplex: reads come from a prepared buffer, writes are collected.
    struct Duplex {
        input: Cursor<Vec<u8>>,
        output: Vec<u8>,
    }
    impl Read for Duplex {
        fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
            self.input.read(b)
        }
    }
    impl Write for Duplex {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.output.extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn client_skips_stray_frames() {
        let mut buf = Vec::new();
        let hr = HelloReply {
            service_version: "0.1.0".into(),
            protocol_version: 1,
            min_protocol_version: 1,
            caller: CallerInfo {
                client: ClientKind::AppApi,
                session_id: 3,
                seat: None,
                is_admin: false,
                restricted: false,
            },
        };
        write_message(
            &mut buf,
            &ServerFrame::Response(Response::new(1, Ok(ResponseBody::Hello(hr)))),
        )
        .unwrap();
        write_message(
            &mut buf,
            &ServerFrame::Response(Response::new(99, Ok(ResponseBody::Ack))),
        )
        .unwrap();
        let seat2 = SeatNumber::new(2).unwrap();
        write_message(
            &mut buf,
            &ServerFrame::Response(Response::new(
                2,
                Ok(ResponseBody::Seat {
                    seat: Some(seat2),
                    session_id: Some(3),
                }),
            )),
        )
        .unwrap();
        let mut c = Client::over(Duplex {
            input: Cursor::new(buf),
            output: Vec::new(),
        });
        c.hello().unwrap();
        assert_eq!(c.my_seat().unwrap(), Some(seat2));
        assert!(matches!(c.my_seat(), Err(ClientError::Closed)));
        assert_eq!(compat::terminal_id(Ok(Some(seat2))), 2);
        assert_eq!(compat::terminal_id(Err(ClientError::Closed)), 0);
        assert_eq!(compat::max_terminals(), 16);
    }

    #[test]
    fn session_lookup() {
        let s3 = SeatNumber::new(3).unwrap();
        assert_eq!(seat_of_session(&[(s3, 7)], 7), Some(s3));
        assert_eq!(seat_of_session(&[(s3, 7)], 8), None);
    }

    #[cfg(windows)]
    #[test]
    fn service_pid_lookup() {
        // RpcSs always runs; a made-up name never resolves.
        assert!(service_pid("RpcSs").unwrap() > 0);
        assert!(service_pid("PulsarSeatNoSuchService").is_err());
    }
}
