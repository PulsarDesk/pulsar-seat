// SPDX-License-Identifier: GPL-3.0-only
//! Service-owned server end of the `pulsar-host` control pipe
//! (docs/split/agents/pulsar-host-backend.md §3.1).
//!
//! - Created **before** `pulsar-host` is launched, with `FILE_FLAG_FIRST_PIPE_INSTANCE`
//!   (fails if anyone already owns the name), `PIPE_REJECT_REMOTE_CLIENTS`, byte mode, one
//!   instance, DACL [`HOST_PIPE_SDDL`] (SYSTEM + Administrators).
//! - Listening uses `PIPE_NOWAIT` so [`HostPipe::accept`] never blocks the service tick;
//!   once the right client is in, the handle is switched to `PIPE_WAIT` and read with the
//!   same peek-then-read transport as the seat-pipe client ([`super::pipe::PipeClient`]).
//! - A client is accepted only if `GetNamedPipeClientProcessId` is the launched pid **and**
//!   that process's creation time equals `Launched::creation_time`. Anyone else (an elevated
//!   admin tool, a second process) is disconnected before a byte is read, so the
//!   Administrators ACE grants no control.
//!
//! Untested on a real host (needs the service as SYSTEM and a `pulsar-host` build).

use super::{os_err, wide, OwnedHandle, SecurityDescriptor};
use crate::backend::pulsar::control::pipe_name;
use crate::backend::pulsar::mic::mic_pipe_name;
use crate::backend::pulsar::{HostPipe, HostPipeListener, HOST_PIPE_SDDL};
use crate::control::PipeTransport;
use crate::supervise::Launched;
use crate::StreamError;
use std::ptr;
use windows_sys::Win32::Foundation::{
    GetLastError, ERROR_NO_DATA, ERROR_PIPE_CONNECTED, ERROR_PIPE_LISTENING, FILETIME, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeClientProcessId, SetNamedPipeHandleState,
    PIPE_NOWAIT, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

const BUF: u32 = 64 * 1024;

/// Real [`HostPipeListener`].
#[derive(Default)]
pub struct WinHostPipes;

struct WinHostPipe {
    /// `None` once handed out as a transport.
    h: Option<OwnedHandle>,
}

impl HostPipeListener for WinHostPipes {
    fn create(&self, seat_id: &str) -> Result<Box<dyn HostPipe>, StreamError> {
        crate::validate_seat_id(seat_id)?;
        create_at(&pipe_name(seat_id))
    }

    fn create_mic(&self, seat_id: &str) -> Result<Box<dyn HostPipe>, StreamError> {
        crate::validate_seat_id(seat_id)?;
        create_at(&mic_pipe_name(seat_id))
    }
}

/// One service-owned pipe instance at `name` (control or mic pipe, same rules).
fn create_at(name: &str) -> Result<Box<dyn HostPipe>, StreamError> {
    let sd = SecurityDescriptor::from_sddl(HOST_PIPE_SDDL)?;
    let sa = sd.attributes();
    let name = wide(name);
    // SAFETY: NUL-terminated name; security attributes outlive the call.
    let h = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_NOWAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            BUF,
            BUF,
            0,
            &sa,
        )
    };
    if h == INVALID_HANDLE_VALUE {
        return Err(os_err("create pulsar-host pipe"));
    }
    let h = OwnedHandle::new(h).ok_or_else(|| os_err("create pulsar-host pipe"))?;
    Ok(Box::new(WinHostPipe { h: Some(h) }))
}

/// Creation time (FILETIME as u64) of `pid`, if it can be opened.
fn creation_time(pid: u32) -> Option<u64> {
    // SAFETY: plain query access; handle owned below.
    let p = OwnedHandle::new(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) })?;
    let zero = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    let (mut c, mut e, mut k, mut u) = (zero, zero, zero, zero);
    // SAFETY: valid process handle and out pointers.
    if unsafe { GetProcessTimes(p.raw(), &mut c, &mut e, &mut k, &mut u) } == 0 {
        return None;
    }
    Some((u64::from(c.dwHighDateTime) << 32) | u64::from(c.dwLowDateTime))
}

/// What one non-blocking `ConnectNamedPipe` call means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectOutcome {
    Listening,
    Connected,
    ClosedEarly,
    Failed,
}

/// `ok` = return value, `err` = `GetLastError` (only meaningful when `ok == 0`).
fn connect_outcome(ok: i32, err: u32) -> ConnectOutcome {
    if ok != 0 {
        return ConnectOutcome::Listening;
    }
    match err {
        ERROR_PIPE_CONNECTED => ConnectOutcome::Connected,
        ERROR_PIPE_LISTENING => ConnectOutcome::Listening,
        ERROR_NO_DATA => ConnectOutcome::ClosedEarly,
        _ => ConnectOutcome::Failed,
    }
}

impl HostPipe for WinHostPipe {
    fn accept(&mut self, expected: &Launched) -> Result<Option<Box<dyn PipeTransport>>, StreamError> {
        let Some(h) = self.h.as_ref() else {
            return Err(StreamError::ControlPipe("pipe already handed out".into()));
        };
        // PIPE_NOWAIT: returns at once. Nonzero = "now listening" (first call after a
        // DisconnectNamedPipe), never "connected"; only ERROR_PIPE_CONNECTED means a client
        // is in. GetLastError is read only on a zero return.
        // SAFETY: valid pipe handle, no overlapped I/O.
        let ok = unsafe { ConnectNamedPipe(h.raw(), ptr::null_mut()) };
        // SAFETY: trivially safe.
        let err = if ok == 0 { unsafe { GetLastError() } } else { 0 };
        match connect_outcome(ok, err) {
            ConnectOutcome::Listening => return Ok(None),
            ConnectOutcome::ClosedEarly => {
                // A client connected and already closed: reset and keep listening.
                // SAFETY: valid pipe handle.
                unsafe { DisconnectNamedPipe(h.raw()) };
                return Ok(None);
            }
            ConnectOutcome::Failed => return Err(os_err("ConnectNamedPipe")),
            ConnectOutcome::Connected => {}
        }
        let mut pid = 0u32;
        // SAFETY: valid pipe handle and out pointer.
        let have_pid = unsafe { GetNamedPipeClientProcessId(h.raw(), &mut pid) } != 0;
        let genuine = have_pid && pid == expected.pid && creation_time(pid) == Some(expected.creation_time);
        if !genuine {
            log::warn!("pulsar-host pipe: refused client pid {pid} (expected {})", expected.pid);
            // SAFETY: valid pipe handle; drops the foreign client, the instance listens again.
            unsafe { DisconnectNamedPipe(h.raw()) };
            return Ok(None);
        }
        let mode = PIPE_READMODE_BYTE | PIPE_WAIT;
        // SAFETY: valid pipe handle; only the mode pointer is set.
        if unsafe { SetNamedPipeHandleState(h.raw(), &mode, ptr::null(), ptr::null()) } == 0 {
            return Err(os_err("SetNamedPipeHandleState"));
        }
        let h = self.h.take().expect("checked above");
        Ok(Some(Box::new(super::pipe::PipeClient::from_handle(h))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonzero_return_is_listening_not_connected() {
        // Sequence after a refused client: DisconnectNamedPipe, then the next tick's call
        // returns nonzero (listening), then the real host connects.
        assert_eq!(connect_outcome(1, 0), ConnectOutcome::Listening);
        assert_eq!(connect_outcome(1, ERROR_PIPE_CONNECTED), ConnectOutcome::Listening);
        assert_eq!(connect_outcome(0, ERROR_PIPE_LISTENING), ConnectOutcome::Listening);
        assert_eq!(connect_outcome(0, ERROR_PIPE_CONNECTED), ConnectOutcome::Connected);
        assert_eq!(connect_outcome(0, ERROR_NO_DATA), ConnectOutcome::ClosedEarly);
        assert_eq!(connect_outcome(0, 5), ConnectOutcome::Failed);
    }
}
