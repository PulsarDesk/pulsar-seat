// SPDX-License-Identifier: GPL-3.0-only
//! Control-pipe client: one connection per request.
//!
//! Server identity check (pipe squatting): the service is demand-start, so the pipe name is
//! often free and any local user could create it first. Before any request is sent the
//! client therefore checks that the server end is our service:
//!
//! 1. the pipe is opened with `SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION`, so a
//!    squatter can at most identify, never impersonate, the elevated manager;
//! 2. `GetNamedPipeServerProcessId` -> `ProcessIdToSessionId` must be session 0;
//! 3. the server process token user must be `S-1-5-18` (LocalSystem);
//! 4. the pipe object's owner SID (`GetSecurityInfo`) must be SYSTEM or Administrators.
//!
//! A request that carries a [`seat_proto::remote::Secret`] (seat password, pairing PIN) is only
//! sent when every check passes. Other requests are refused too in release builds; debug
//! builds let them through so the `--console` development mode (elevated admin, not SYSTEM,
//! not session 0) stays usable for non-secret calls.

use seat_proto::remote::legacy::{PIPE_NAME, Request, RequestBody, Response, ResponseBody};
use seat_proto::remote::{ErrorCode, PROTOCOL_VERSION, ProtoError};
use std::fs::{File, OpenOptions};
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

const ERROR_PIPE_BUSY: i32 = 231;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn unavailable(msg: impl Into<String>) -> ProtoError {
    ProtoError::new(ErrorCode::ServiceUnavailable, msg)
}

/// True for requests that carry a secret (password / PIN) to the service, or whose answer
/// is one (one-time password of a Pulsar seat).
pub fn carries_secret(body: &RequestBody) -> bool {
    matches!(
        body,
        RequestBody::SetSeatPassword { .. }
            | RequestBody::SubmitPin { .. }
            | RequestBody::SetSeatConnectPassword { .. }
            | RequestBody::AnswerRelayAuth { .. }
            | RequestBody::GetSeatOtp { .. }
            | RequestBody::RotateSeatOtp { .. }
    )
}

fn open() -> Result<File, ProtoError> {
    for _ in 0..20 {
        let mut opts = OpenOptions::new();
        opts.read(true).write(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // std adds SECURITY_SQOS_PRESENT when security_qos_flags is set.
            opts.security_qos_flags(win::SECURITY_IDENTIFICATION);
        }
        match opts.open(PIPE_NAME) {
            Ok(f) => return Ok(f),
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                return Err(unavailable("access denied: the manager must run as administrator"));
            }
            Err(e) => return Err(unavailable(format!("service not reachable: {e}"))),
        }
    }
    Err(unavailable("service pipe busy"))
}

/// Decide whether a request may go to a server whose identity check gave `verified`.
fn may_send(body: &RequestBody, verified: &Result<(), String>) -> Result<(), ProtoError> {
    match verified {
        Ok(()) => Ok(()),
        Err(why) if carries_secret(body) || !cfg!(debug_assertions) => Err(ProtoError::new(
            ErrorCode::ServiceUnavailable,
            format!("pipe server is not the Pulsar Split service: {why}"),
        )
        .with_reason("untrusted_server")),
        Err(_) => Ok(()),
    }
}

pub fn call(body: RequestBody) -> Result<ResponseBody, ProtoError> {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let mut pipe = open()?;
    let verified = verify_server(&pipe);
    may_send(&body, &verified)?;
    seat_proto::remote::write_message(&mut pipe, &Request::new(id, body)).map_err(|e| unavailable(e.to_string()))?;
    let resp: Response = seat_proto::remote::read_message(&mut pipe)
        .map_err(|e| unavailable(e.to_string()))?
        .ok_or_else(|| unavailable("service closed the connection"))?;
    if resp.version != PROTOCOL_VERSION {
        return Err(ProtoError::new(ErrorCode::VersionMismatch, format!("service speaks {}", resp.version)));
    }
    if resp.id != id && resp.id != 0 {
        return Err(ProtoError::new(ErrorCode::Internal, "response id mismatch"));
    }
    resp.result
}

#[cfg(windows)]
fn verify_server(pipe: &File) -> Result<(), String> {
    use std::os::windows::io::AsRawHandle;
    win::verify(pipe.as_raw_handle())
}

#[cfg(not(windows))]
fn verify_server(_pipe: &File) -> Result<(), String> {
    Err("unsupported platform".into())
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;
    use std::ptr;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
    use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_KERNEL_OBJECT};
    use windows_sys::Win32::Security::{
        GetTokenInformation, IsWellKnownSid, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, TOKEN_QUERY,
        TOKEN_USER, TokenUser, WinBuiltinAdministratorsSid, WinLocalSystemSid,
    };
    use windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId;
    use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    use windows_sys::Win32::System::Threading::{OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION};

    pub use windows_sys::Win32::Storage::FileSystem::SECURITY_IDENTIFICATION;

    struct Handle(HANDLE);
    impl Drop for Handle {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: we own the handle.
                unsafe { CloseHandle(self.0) };
            }
        }
    }

    fn last() -> String {
        std::io::Error::last_os_error().to_string()
    }

    pub fn verify(pipe: *mut c_void) -> Result<(), String> {
        let pipe = pipe as HANDLE;
        let mut pid = 0u32;
        // SAFETY: valid pipe handle, valid out pointer.
        if unsafe { GetNamedPipeServerProcessId(pipe, &mut pid) } == 0 {
            return Err(format!("GetNamedPipeServerProcessId: {}", last()));
        }
        let mut session = u32::MAX;
        // SAFETY: valid out pointer.
        if unsafe { ProcessIdToSessionId(pid, &mut session) } == 0 {
            return Err(format!("ProcessIdToSessionId({pid}): {}", last()));
        }
        if session != 0 {
            return Err(format!("server pid {pid} runs in session {session}, not 0"));
        }
        server_is_system(pid)?;
        owner_is_trusted(pipe)
    }

    fn server_is_system(pid: u32) -> Result<(), String> {
        // SAFETY: plain call; result checked.
        let proc = Handle(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) });
        if proc.0.is_null() {
            return Err(format!("OpenProcess({pid}): {}", last()));
        }
        let mut tok: HANDLE = ptr::null_mut();
        // SAFETY: valid process handle and out pointer.
        if unsafe { OpenProcessToken(proc.0, TOKEN_QUERY, &mut tok) } == 0 {
            return Err(format!("OpenProcessToken: {}", last()));
        }
        let tok = Handle(tok);
        // TOKEN_USER + SID fits easily; u64 buffer keeps the alignment.
        let mut buf = [0u64; 64];
        let mut len = 0u32;
        // SAFETY: buffer valid for its size in bytes.
        let ok = unsafe {
            GetTokenInformation(tok.0, TokenUser, buf.as_mut_ptr().cast(), size_of_val(&buf) as u32, &mut len)
        };
        if ok == 0 {
            return Err(format!("GetTokenInformation: {}", last()));
        }
        // SAFETY: GetTokenInformation(TokenUser) filled a TOKEN_USER at the start of buf.
        let user = unsafe { &*(buf.as_ptr() as *const TOKEN_USER) };
        // SAFETY: SID pointer points into buf.
        if unsafe { IsWellKnownSid(user.User.Sid, WinLocalSystemSid) } == 0 {
            return Err(format!("server pid {pid} does not run as LocalSystem"));
        }
        Ok(())
    }

    fn owner_is_trusted(pipe: HANDLE) -> Result<(), String> {
        let mut owner: PSID = ptr::null_mut();
        let mut sd: PSECURITY_DESCRIPTOR = ptr::null_mut();
        // SAFETY: valid handle; out pointers valid; sd freed below.
        let rc = unsafe {
            GetSecurityInfo(
                pipe,
                SE_KERNEL_OBJECT,
                OWNER_SECURITY_INFORMATION,
                &mut owner,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                &mut sd,
            )
        };
        if rc != 0 {
            return Err(format!("GetSecurityInfo: error {rc}"));
        }
        // SAFETY: owner points into sd, which is still alive.
        let trusted = unsafe {
            IsWellKnownSid(owner, WinLocalSystemSid) != 0 || IsWellKnownSid(owner, WinBuiltinAdministratorsSid) != 0
        };
        // SAFETY: sd was allocated by GetSecurityInfo.
        unsafe { LocalFree(sd) };
        if trusted { Ok(()) } else { Err("pipe owner is neither SYSTEM nor Administrators".into()) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use seat_proto::remote::Secret;

    #[test]
    fn secrets_never_go_to_an_unverified_server() {
        let bad: Result<(), String> = Err("session 2".into());
        let pw = RequestBody::SetSeatPassword { name: "a".into(), password: Secret("x".into()) };
        let pin = RequestBody::SubmitPin { name: "a".into(), pin: Secret("1234".into()), client_name: "tv".into(), request_id: None };
        let cpw = RequestBody::SetSeatConnectPassword { name: "a".into(), password: Secret("x".into()) };
        let relay = RequestBody::AnswerRelayAuth { name: "a".into(), password: Secret("x".into()), totp: None };
        let otp = RequestBody::GetSeatOtp { name: "a".into() };
        for b in [&pw, &pin, &cpw, &relay, &otp] {
            let e = may_send(b, &bad).unwrap_err();
            assert_eq!(e.reason.as_deref(), Some("untrusted_server"));
            assert!(may_send(b, &Ok(())).is_ok());
        }
        // Non-secret requests: allowed only in debug builds (console dev mode).
        assert_eq!(may_send(&RequestBody::GetStatus, &bad).is_ok(), cfg!(debug_assertions));
    }
}
