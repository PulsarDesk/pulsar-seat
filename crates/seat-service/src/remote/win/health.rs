// SPDX-License-Identifier: GPL-3.0-only
//! Shim health pipe `\\.\pipe\PulsarSplit.ShimHealth` (contracts §4.4, OQ 27), served by
//! `pulsar-seat-service` since merge step 6 (name kept: the `shim/` workspace writes to it).
//!
//! The isolation module in each hooked process writes one frame (u32 LE length + JSON
//! `ShimHealth`, ≤ 4 KiB) and closes. DACL: SYSTEM full, Authenticated Users only
//! `FILE_WRITE_DATA | SYNCHRONIZE` (they cannot read anything back). Local clients only.
//! Rate limit: at most [`MAX_CONNECTIONS`] connections are served at once, and each one
//! gets [`READ_DEADLINE`] to deliver its frame: a watchdog cancels the blocking read with
//! `CancelSynchronousIo`, so clients that connect and never write cannot hold the slots.
//! The engine keeps at most `MAX_HEALTH` entries. Health data is informational only (any
//! local user can write a frame).

use crate::remote::engine::Engine;

use crate::win::security::{SecurityDescriptor, wide};
use std::io::{self, Read};
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Duration;
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_PIPE_CONNECTED, GetLastError, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_INBOUND};
use windows_sys::Win32::System::IO::CancelSynchronousIo;
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
    PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};

pub const HEALTH_SDDL: &str = "D:P(A;;GA;;;SY)(A;;0x00100002;;;AU)";
pub const MAX_CONNECTIONS: usize = 8;
/// Time a client has to deliver its whole frame.
pub const READ_DEADLINE: Duration = Duration::from_secs(2);
/// Re-cancel period after the deadline (a cancel between two reads hits nothing).
const RECANCEL: Duration = Duration::from_millis(200);

/// Stops [`HealthServer::run`]: sets the flag, then releases a blocking `ConnectNamedPipe`
/// by connecting once.
pub struct Shutdown {
    name: String,
    flag: Arc<AtomicBool>,
}

impl Shutdown {
    pub fn trigger(&self) {
        self.flag.store(true, Ordering::SeqCst);
        // Retry briefly: between two instances there may be no listening instance.
        for _ in 0..20 {
            if std::fs::OpenOptions::new().write(true).open(&self.name).is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

struct OwnedPipe(HANDLE);
unsafe impl Send for OwnedPipe {}

pub struct HealthServer {
    sd: SecurityDescriptor,
    flag: Arc<AtomicBool>,
}

impl HealthServer {
    pub fn new() -> io::Result<Self> {
        Ok(Self { sd: SecurityDescriptor::from_sddl(HEALTH_SDDL)?, flag: Arc::default() })
    }

    pub fn shutdown_handle(&self) -> Shutdown {
        Shutdown { name: seat_shim_core::HEALTH_PIPE.to_string(), flag: self.flag.clone() }
    }

    fn instance(&self, first: bool) -> io::Result<HANDLE> {
        let name = wide(seat_shim_core::HEALTH_PIPE);
        let sa = self.sd.attributes();
        let mut mode = PIPE_ACCESS_INBOUND;
        if first {
            mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
        }
        // SAFETY: valid name and attributes for the duration of the call.
        let h = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                mode,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                0,
                seat_shim_core::health::MAX_HEALTH_FRAME as u32,
                0,
                &sa,
            )
        };
        if h == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(h)
    }

    /// Serve until shutdown.
    pub fn run(&self, engine: Engine) -> io::Result<()> {
        let busy = Arc::new(AtomicUsize::new(0));
        let mut first = true;
        while !self.flag.load(Ordering::SeqCst) {
            let h = self.instance(first)?;
            first = false;
            // SAFETY: fresh instance.
            let ok = unsafe { ConnectNamedPipe(h, std::ptr::null_mut()) };
            // SAFETY: trivially safe.
            if (ok == 0 && unsafe { GetLastError() } != ERROR_PIPE_CONNECTED) || self.flag.load(Ordering::SeqCst) {
                // SAFETY: we own h.
                unsafe { CloseHandle(h) };
                continue;
            }
            if busy.load(Ordering::SeqCst) >= MAX_CONNECTIONS {
                // SAFETY: we own h.
                unsafe {
                    DisconnectNamedPipe(h);
                    CloseHandle(h);
                }
                continue;
            }
            busy.fetch_add(1, Ordering::SeqCst);
            let pipe = OwnedPipe(h);
            let (busy, engine) = (busy.clone(), engine.clone());
            let (done_tx, done_rx) = mpsc::channel::<()>();
            let worker = std::thread::spawn(move || {
                serve(pipe, &engine);
                let _ = done_tx.send(());
            });
            // Deadline watchdog; also owns the slot until the worker has really finished.
            std::thread::spawn(move || {
                with_deadline(&done_rx, READ_DEADLINE, RECANCEL, || {
                    // SAFETY: the JoinHandle keeps the thread handle valid; cancelling I/O
                    // of a thread that has none pending is harmless.
                    unsafe { CancelSynchronousIo(worker.as_raw_handle() as HANDLE) };
                });
                let _ = worker.join();
                busy.fetch_sub(1, Ordering::SeqCst);
            });
        }
        Ok(())
    }
}

/// Waits for `done`; after `deadline` calls `cancel` every `again` until done (or the
/// sender is gone).
fn with_deadline(done: &mpsc::Receiver<()>, deadline: Duration, again: Duration, mut cancel: impl FnMut()) {
    let mut wait = deadline;
    loop {
        match done.recv_timeout(wait) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                cancel();
                wait = again;
            }
        }
    }
}

fn serve(pipe: OwnedPipe, engine: &Engine) {
    let h = pipe.0;
    // SAFETY: we own the handle; File closes it.
    let mut f = unsafe { std::fs::File::from_raw_handle(h as _) };
    let mut buf = Vec::with_capacity(512);
    let max = seat_shim_core::health::MAX_HEALTH_FRAME + 4;
    let mut chunk = [0u8; 512];
    while buf.len() < max {
        match f.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n.min(max - buf.len())]),
        }
    }
    match seat_shim_core::decode_health(&buf) {
        Ok(hl) => engine.record_health(hl),
        Err(e) => crate::log::debug(&format!("bad shim health frame: {e}")),
    }
    // SAFETY: handle still owned by `f`.
    unsafe { DisconnectNamedPipe(h) };
    drop(f);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;

    #[test]
    fn deadline_cancels_until_done() {
        let (tx, rx) = mpsc::channel::<()>();
        let n = Arc::new(AtomicU32::new(0));
        let n2 = n.clone();
        let t = std::thread::spawn(move || {
            with_deadline(&rx, Duration::from_millis(20), Duration::from_millis(5), || {
                if n2.fetch_add(1, Ordering::SeqCst) == 2 {
                    let _ = tx.send(());
                }
            })
        });
        t.join().unwrap();
        assert_eq!(n.load(Ordering::SeqCst), 3, "cancelled until the worker reported done");
        // Finished in time: no cancel at all.
        let (tx, rx) = mpsc::channel::<()>();
        tx.send(()).unwrap();
        let mut cancels = 0;
        with_deadline(&rx, Duration::from_millis(20), Duration::from_millis(5), || cancels += 1);
        assert_eq!(cancels, 0);
    }

    #[test]
    fn blocked_pipe_read_is_cancelled_by_the_watchdog() {
        // Anonymous pipe whose writer never writes: the read blocks until cancelled.
        let (mut reader, _writer) = std::io::pipe().unwrap();
        let (tx, rx) = mpsc::channel::<()>();
        let worker = std::thread::spawn(move || {
            let mut b = [0u8; 8];
            let r = reader.read(&mut b);
            let _ = tx.send(());
            r.is_err()
        });
        let h = worker.as_raw_handle() as HANDLE;
        let start = std::time::Instant::now();
        with_deadline(&rx, Duration::from_millis(100), Duration::from_millis(20), || {
            // SAFETY: worker's JoinHandle is alive.
            unsafe { CancelSynchronousIo(h) };
        });
        assert!(worker.join().unwrap(), "read ended with an error (cancelled)");
        assert!(start.elapsed() < Duration::from_secs(5));
    }
}
