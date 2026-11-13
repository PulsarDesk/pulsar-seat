// SPDX-License-Identifier: GPL-3.0-only
//! Control named-pipe server.
//!
//! - DACL from SDDL (default [`crate::security::PIPE_SDDL`]: SYSTEM + Administrators).
//! - `PIPE_REJECT_REMOTE_CLIENTS`: local callers only.
//! - The first instance is created with `FILE_FLAG_FIRST_PIPE_INSTANCE`, so the service
//!   refuses to start if somebody else already owns the pipe name (name squatting).
//! - Byte mode; framing from `split_proto::framing`. One thread per connection.

use crate::security::{SecurityDescriptor, wide};
use split_proto::{ErrorCode, ProtoError, Request, Response};
use std::fs::File;
use std::io;
use std::os::windows::io::FromRawHandle;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_PIPE_CONNECTED, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, FlushFileBuffers, PIPE_ACCESS_DUPLEX};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeClientProcessId, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};

pub type Handler = Arc<dyn Fn(Request) -> Response + Send + Sync>;

const BUF: u32 = 64 * 1024;

pub struct PipeServer {
    name: String,
    sd: SecurityDescriptor,
    shutdown: Arc<AtomicBool>,
}

/// Raw handle that may move to the connection thread (we own it exclusively).
struct OwnedPipe(HANDLE);
unsafe impl Send for OwnedPipe {}

#[derive(Clone)]
pub struct Shutdown {
    name: String,
    flag: Arc<AtomicBool>,
}

impl Shutdown {
    pub fn new(name: &str, flag: Arc<AtomicBool>) -> Self {
        Self { name: name.to_string(), flag }
    }

    /// Ask `run` to return. A blocking `ConnectNamedPipe` is released by connecting to it.
    pub fn trigger(&self) {
        self.flag.store(true, Ordering::SeqCst);
        // Retry briefly: between two instances there may be no listening instance.
        for _ in 0..20 {
            if std::fs::OpenOptions::new().write(true).open(&self.name).is_ok() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }
}

impl PipeServer {
    pub fn new(name: &str, sddl: &str) -> io::Result<Self> {
        Ok(Self { name: name.to_string(), sd: SecurityDescriptor::from_sddl(sddl)?, shutdown: Arc::default() })
    }

    pub fn shutdown_handle(&self) -> Shutdown {
        Shutdown { name: self.name.clone(), flag: self.shutdown.clone() }
    }

    fn create_instance(&self, first: bool) -> io::Result<HANDLE> {
        let name = wide(&self.name);
        let sa = self.sd.attributes();
        let mut open_mode = PIPE_ACCESS_DUPLEX;
        if first {
            open_mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
        }
        // SAFETY: valid name and security attributes that outlive the call.
        let h = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                open_mode,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
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

    /// Serve until [`Shutdown::trigger`]. Errors only if the pipe cannot be created.
    pub fn run(&self, handler: Handler) -> io::Result<()> {
        let mut first = true;
        while !self.shutdown.load(Ordering::SeqCst) {
            let h = self.create_instance(first)?;
            first = false;
            // SAFETY: h is a fresh pipe instance.
            let ok = unsafe { ConnectNamedPipe(h, std::ptr::null_mut()) };
            // SAFETY: trivially safe.
            if ok == 0 && unsafe { GetLastError() } != ERROR_PIPE_CONNECTED {
                crate::log::warn(&format!("ConnectNamedPipe failed: {}", io::Error::last_os_error()));
                // SAFETY: we own h.
                unsafe { CloseHandle(h) };
                continue;
            }
            if self.shutdown.load(Ordering::SeqCst) {
                // SAFETY: we own h.
                unsafe { CloseHandle(h) };
                break;
            }
            let pipe = OwnedPipe(h);
            let handler = handler.clone();
            std::thread::spawn(move || serve(pipe, handler));
        }
        Ok(())
    }
}

fn client_pid(h: HANDLE) -> u32 {
    let mut pid = 0u32;
    // SAFETY: connected server end of a pipe.
    unsafe { GetNamedPipeClientProcessId(h, &mut pid) };
    pid
}

fn serve(pipe: OwnedPipe, handler: Handler) {
    let h = pipe.0;
    let pid = client_pid(h);
    crate::log::debug(&format!("control client connected (pid {pid})"));
    // SAFETY: we own the handle; File closes it on drop.
    let mut file = unsafe { File::from_raw_handle(h as _) };
    loop {
        match split_proto::read_message::<_, Request>(&mut file) {
            Ok(Some(req)) => {
                let resp = handler(req);
                if let Err(e) = split_proto::write_message(&mut file, &resp) {
                    crate::log::warn(&format!("pipe write failed (pid {pid}): {e}"));
                    break;
                }
            }
            Ok(None) => break,
            Err(e) => {
                crate::log::warn(&format!("bad frame from pid {pid}: {e}"));
                let resp = Response::new(0, Err(ProtoError::new(ErrorCode::BadRequest, e.to_string())));
                let _ = split_proto::write_message(&mut file, &resp);
                break;
            }
        }
    }
    // SAFETY: handle still owned by `file`.
    unsafe {
        FlushFileBuffers(h);
        DisconnectNamedPipe(h);
    }
    drop(file);
}

#[cfg(test)]
mod tests {
    use super::*;
    use split_proto::{RequestBody, ResponseBody};

    /// End-to-end over a real pipe. The test process is the creator, so the DACL also
    /// grants the current user (like `--allow-current-user`).
    #[test]
    fn request_response_over_real_pipe() {
        let name = format!(r"\\.\pipe\PulsarSplit.Test.{}", uuid::Uuid::new_v4());
        let sid = crate::security::current_user_sid().unwrap();
        let sddl = format!("{}(A;;GA;;;{sid})", crate::security::PIPE_SDDL);
        let server = PipeServer::new(&name, &sddl).unwrap();
        let stop = server.shutdown_handle();
        let handler: Handler = Arc::new(|req: Request| {
            Response::new(req.id, Ok(ResponseBody::Hello { service_version: "t".into(), protocol_version: 1 }))
        });
        let t = std::thread::spawn(move || server.run(handler).unwrap());

        let mut client = None;
        for _ in 0..100 {
            match std::fs::OpenOptions::new().read(true).write(true).open(&name) {
                Ok(f) => {
                    client = Some(f);
                    break;
                }
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(20)),
            }
        }
        let mut c = client.expect("connect");
        for id in 1..=3 {
            split_proto::write_message(&mut c, &Request::new(id, RequestBody::Hello)).unwrap();
            let resp: Response = split_proto::read_message(&mut c).unwrap().unwrap();
            assert_eq!(resp.id, id);
            assert!(resp.result.is_ok());
        }
        // Garbage frame gets a BadRequest and the connection closes.
        split_proto::write_frame(&mut c, b"{nope").unwrap();
        let resp: Response = split_proto::read_message(&mut c).unwrap().unwrap();
        assert_eq!(resp.result.unwrap_err().code, ErrorCode::BadRequest);
        drop(c);

        // A second server on the same name must fail (first-instance flag).
        let squat = PipeServer::new(&name, &sddl).unwrap();
        assert!(squat.create_instance(true).is_err());

        stop.trigger();
        t.join().unwrap();
    }
}
