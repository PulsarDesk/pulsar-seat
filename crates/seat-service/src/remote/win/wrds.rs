// SPDX-License-Identifier: GPL-3.0-only
//! Windows side of `crate::remote::wrds`: the provider pipe server and the real
//! [`WrdsPort`]. Added 2026-11-17 (package remote-idd-hdr).
//!
//! - Pipe `\\.\pipe\PulsarSeat.Wrds`, one instance at a time (one provider per TermService),
//!   created with `FILE_FLAG_FIRST_PIPE_INSTANCE`, local clients only, DACL
//!   `seat_proto::wrds::pipe_sddl()` (SYSTEM + the TermService service SID).
//! - A client is served only when its process is the one running TermService
//!   (`GetNamedPipeClientProcessId` against the SCM's process id of `TermService`).
//! - The first frame must be a `hello` of our major version; then frames move both ways
//!   until the pipe breaks ([`Hub::provider_gone`]).
//! - Registration (`seat_session::wrds`) is written only by the real service in session 0;
//!   `--console` dev runs never register anything.
//!
//! HYPOTHESIS (nothing ran): TermService (NETWORK SERVICE with its service SID) can open
//! the pipe with this DACL; `WTSLogoffSession` ends a provider session like an RDP one.

use crate::remote::wrds::{CONNECT_TIMEOUT, Hub, WrdsConnect, WrdsFailure, WrdsPort, WrdsSession};
use crate::win::security::{SecurityDescriptor, wide};
use seat_proto::framing::{read_frame, write_frame};
use seat_proto::remote::{Secret, StopAction};
use seat_proto::wrds::{self, PIPE_NAME, ProviderMsg};
use seat_session::remote::manager::CancelToken;
use seat_session::wrds::{self as reg, WrdsStatus};
use std::io;
use std::os::windows::io::FromRawHandle;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_PIPE_CONNECTED, GetLastError, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
    PIPE_TYPE_BYTE, PIPE_WAIT, PeekNamedPipe,
};

const POLL: Duration = Duration::from_millis(50);

pub struct WinWrds {
    hub: Arc<Hub>,
    install_dir: PathBuf,
    /// Seat accounts of the last registration (for the state check).
    users: Mutex<Vec<String>>,
}

impl WinWrds {
    /// Build the port and start the pipe server thread (session 0 service only).
    pub fn start(install_dir: PathBuf) -> Arc<Self> {
        let me = Arc::new(Self { hub: Arc::new(Hub::new()), install_dir, users: Mutex::new(Vec::new()) });
        if crate::win::console_runner::in_session_zero() {
            let hub = me.hub.clone();
            let r = std::thread::Builder::new().name("wrds-pipe".into()).spawn(move || serve_forever(hub));
            if let Err(e) = r {
                log::error!("protocol provider pipe not started: {e}");
            }
        }
        me
    }

    fn dll(&self) -> PathBuf {
        self.install_dir.join(reg::DLL_NAME)
    }

    fn dll_ok(&self) -> Result<(), String> {
        let bytes = std::fs::read(self.dll()).map_err(|e| format!("{}: {e}", self.dll().display()))?;
        reg::check_dll(&bytes, crate::win::platform::native_machine()).map_err(|e| e.to_string())
    }

    fn sids(users: &[String]) -> Vec<String> {
        use seat_session::wts::Wts;
        let wts = seat_session::win::wts::WindowsWts;
        users.iter().filter(|u| !u.is_empty()).filter_map(|u| wts.account_sid(".", u)).collect()
    }
}

impl WrdsPort for WinWrds {
    fn status(&self) -> WrdsStatus {
        use seat_session::win::registry::WinRegistry;
        let users = self.users.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let dll = self.dll().display().to_string();
        let registration = reg::state(&WinRegistry, &dll, &Self::sids(&users)).unwrap_or(reg::Registration::Partial);
        WrdsStatus::evaluate(
            registration,
            self.hub.connected(),
            self.hub.listening(),
            reg::disabled_by_recovery(&WinRegistry),
            self.dll_ok().is_ok(),
        )
    }

    fn connect(&self, req: &WrdsConnect, password: &Secret, cancel: &CancelToken) -> Result<WrdsSession, WrdsFailure> {
        self.hub.connect(req, password, cancel, CONNECT_TIMEOUT)
    }

    fn end(&self, s: &WrdsSession, action: StopAction) -> Result<(), String> {
        use seat_session::wts::Wts;
        let wts = seat_session::win::wts::WindowsWts;
        let r = match action {
            StopAction::LogOff => wts.logoff(s.session_id, false),
            StopAction::Disconnect => wts.disconnect(s.session_id),
        };
        self.hub.disconnect(s.conn_id);
        r.map_err(|e| e.to_string())
    }

    fn take_lost(&self) -> Vec<(u64, String)> {
        self.hub.take_lost()
    }

    fn sync_registration(&self, want: bool, seat_users: &[String]) -> Result<WrdsStatus, String> {
        use seat_session::win::registry::WinRegistry;
        if !crate::win::console_runner::in_session_zero() {
            return Ok(self.status());
        }
        *self.users.lock().unwrap_or_else(|e| e.into_inner()) = seat_users.to_vec();
        let dll = self.dll().display().to_string();
        let ok = self.dll_ok();
        let r = reg::sync(&WinRegistry, want && ok.is_ok(), &dll, &Self::sids(seat_users)).map_err(|e| e.to_string());
        if want && let Err(e) = ok {
            return Err(e);
        }
        r.map(|_| self.status())
    }
}

struct Pipe(HANDLE);
impl Drop for Pipe {
    fn drop(&mut self) {
        // SAFETY: we own the handle.
        unsafe { CloseHandle(self.0) };
    }
}

fn instance(sd: &SecurityDescriptor) -> io::Result<Pipe> {
    let name = wide(PIPE_NAME);
    let sa = sd.attributes();
    // SAFETY: valid name and attributes for the duration of the call.
    let h = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            wrds::MAX_FRAME as u32,
            wrds::MAX_FRAME as u32,
            0,
            &sa,
        )
    };
    if h == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    Ok(Pipe(h))
}

fn serve_forever(hub: Arc<Hub>) {
    let sd = match SecurityDescriptor::from_sddl(&wrds::pipe_sddl()) {
        Ok(sd) => sd,
        Err(e) => {
            log::error!("protocol provider pipe security: {e}");
            return;
        }
    };
    loop {
        let pipe = match instance(&sd) {
            Ok(p) => p,
            Err(e) => {
                log::warn!("protocol provider pipe: {e}");
                std::thread::sleep(Duration::from_secs(5));
                continue;
            }
        };
        // SAFETY: fresh instance.
        let ok = unsafe { ConnectNamedPipe(pipe.0, std::ptr::null_mut()) };
        // SAFETY: trivially safe.
        if ok == 0 && unsafe { GetLastError() } != ERROR_PIPE_CONNECTED {
            continue;
        }
        let mut client = 0u32;
        // SAFETY: valid handle, out pointer.
        let got = unsafe { GetNamedPipeClientProcessId(pipe.0, &mut client) } != 0;
        match seat_compat::seatapi::service_pid("TermService") {
            Ok(ts) if got && ts == client => {}
            other => {
                log::warn!("protocol provider pipe: client {client} is not TermService ({other:?}); dropped");
                continue;
            }
        }
        if let Err(e) = session(&hub, pipe) {
            log::info!("protocol provider pipe session ended: {e}");
        }
        hub.provider_gone();
    }
}

fn session(hub: &Hub, pipe: Pipe) -> Result<(), String> {
    let h = pipe.0;
    // The File owns the handle from here (closed on drop); `pipe` must not close it again.
    std::mem::forget(pipe);
    // SAFETY: we own h.
    let mut f = unsafe { std::fs::File::from_raw_handle(h as _) };
    let first = read_frame(&mut f).map_err(|e| e.to_string())?.ok_or("provider closed the pipe")?;
    let hello: ProviderMsg = wrds::decode(&first).map_err(|e| e.to_string())?;
    let welcome = hub.provider_hello(&hello).ok_or("not a hello of our protocol version")?;
    write_frame(&mut f, &wrds::encode(&welcome).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    if let Some((pid, v)) = hub.provider() {
        log::info!("protocol provider {v} connected (TermService pid {pid})");
    }
    loop {
        for m in hub.take_outgoing() {
            write_frame(&mut f, &wrds::encode(&m).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        }
        let mut avail = 0u32;
        // SAFETY: valid handle, out pointer.
        if unsafe { PeekNamedPipe(h, std::ptr::null_mut(), 0, std::ptr::null_mut(), &mut avail, std::ptr::null_mut()) } == 0 {
            return Err(io::Error::last_os_error().to_string());
        }
        if avail >= 4 {
            let payload = read_frame(&mut f).map_err(|e| e.to_string())?.ok_or("provider closed the pipe")?;
            let msg: ProviderMsg = wrds::decode(&payload).map_err(|e| e.to_string())?;
            hub.on_provider(msg);
            continue;
        }
        hub.wait_outgoing(POLL);
    }
}
