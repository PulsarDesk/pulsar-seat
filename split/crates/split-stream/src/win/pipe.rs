// SPDX-License-Identifier: GPL-3.0-only
//! Client end of the fork's seat pipe (spec 80 §8.4).
//!
//! The service connects with `SECURITY_IDENTIFICATION` (the server may identify but not
//! impersonate us beyond that) and then checks `GetNamedPipeServerProcessId` against the pid
//! it launched, so a squatter that created the pipe name first is refused (the fork also
//! uses `FILE_FLAG_FIRST_PIPE_INSTANCE` and a SYSTEM-only DACL).

use super::{os_err, wide, OwnedHandle};
use crate::control::frame::{write_frame, FrameBuf};
use crate::control::{pipe_name, PipeTransport};
use crate::StreamError;
use std::io::{self, Write};
use std::ptr;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{
    GetLastError, ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_PIPE_BUSY, GENERIC_READ, GENERIC_WRITE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, OPEN_EXISTING, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT,
};
use windows_sys::Win32::System::Pipes::{GetNamedPipeServerProcessId, PeekNamedPipe, WaitNamedPipeW};

/// A frame that started but made no progress for this long is a dead server.
pub const STALL_LIMIT: Duration = Duration::from_secs(5);

pub struct PipeClient {
    h: OwnedHandle,
    /// Bytes of a frame that has not fully arrived yet (kept across calls).
    buf: FrameBuf,
    last_progress: Instant,
}

struct Io<'a>(&'a OwnedHandle);

impl PipeClient {
    /// Transport over an already connected pipe handle (client or server end).
    pub(crate) fn from_handle(h: OwnedHandle) -> Self {
        Self { h, buf: FrameBuf::default(), last_progress: Instant::now() }
    }

    /// Bytes readable right now without blocking.
    fn available(&self) -> io::Result<u32> {
        let mut avail = 0u32;
        // SAFETY: only the "bytes available" out pointer is used.
        let ok = unsafe { PeekNamedPipe(self.h.raw(), ptr::null_mut(), 0, ptr::null_mut(), &mut avail, ptr::null_mut()) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(avail)
    }

    /// Read at most `max` bytes that `PeekNamedPipe` reported, so `ReadFile` never blocks.
    fn read_some(&mut self, max: usize) -> io::Result<usize> {
        let mut tmp = vec![0u8; max];
        let mut n = 0u32;
        let len = u32::try_from(max).unwrap_or(u32::MAX);
        // SAFETY: buffer valid for `len` bytes; the bytes are already in the pipe.
        if unsafe { ReadFile(self.h.raw(), tmp.as_mut_ptr(), len, &mut n, ptr::null_mut()) } == 0 {
            // Message-mode pipe: the rest of the message is read next time.
            // SAFETY: trivially safe.
            if unsafe { GetLastError() } != ERROR_MORE_DATA {
                return Err(io::Error::last_os_error());
            }
        }
        self.buf.push(&tmp[..n as usize]);
        Ok(n as usize)
    }
}

impl Write for Io<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut n = 0u32;
        let len = u32::try_from(buf.len()).unwrap_or(u32::MAX);
        // SAFETY: buffer valid for `len` bytes; synchronous handle.
        if unsafe { WriteFile(self.0.raw(), buf.as_ptr(), len, &mut n, ptr::null_mut()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(n as usize)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl PipeTransport for PipeClient {
    fn send(&mut self, frame: &[u8]) -> io::Result<()> {
        write_frame(&mut Io(&self.h), frame)
    }

    fn recv(&mut self) -> io::Result<Vec<u8>> {
        loop {
            if let Some(f) = self.recv_timeout(Duration::from_secs(1))? {
                return Ok(f);
            }
        }
    }

    /// Never blocks past `timeout`: polls `PeekNamedPipe` (10 ms) and reads only bytes that
    /// are already in the pipe, keeping a partial frame for the next call. A frame that
    /// stalls for [`STALL_LIMIT`] is an error (dead or wedged server).
    fn recv_timeout(&mut self, timeout: Duration) -> io::Result<Option<Vec<u8>>> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(f) = self.buf.take_frame()? {
                return Ok(Some(f));
            }
            let avail = self.available()? as usize;
            if avail > 0 {
                let want = self.buf.wanted().min(avail);
                if self.read_some(want)? > 0 {
                    self.last_progress = Instant::now();
                }
                continue;
            }
            if self.buf.is_partial() && self.last_progress.elapsed() >= STALL_LIMIT {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "seat pipe frame stalled"));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// Connect to the seat pipe and verify that its server is the process we launched.
pub fn connect(seat_id: &str, expected_pid: u32, timeout: Duration) -> Result<PipeClient, StreamError> {
    crate::validate_seat_id(seat_id)?;
    let name = wide(&pipe_name(seat_id));
    let deadline = Instant::now() + timeout;
    let h = loop {
        // SAFETY: NUL-terminated name; no security attributes; no template.
        let h = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                ptr::null(),
                OPEN_EXISTING,
                SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                ptr::null_mut(),
            )
        };
        if let Some(h) = OwnedHandle::new(h) {
            break h;
        }
        // SAFETY: trivially safe.
        let err = unsafe { GetLastError() };
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(StreamError::ControlPipe(format!("seat pipe not available (error {err})")));
        }
        match err {
            // SAFETY: NUL-terminated name.
            ERROR_PIPE_BUSY => unsafe {
                WaitNamedPipeW(name.as_ptr(), left.as_millis().min(u128::from(u32::MAX)) as u32);
            },
            ERROR_FILE_NOT_FOUND => std::thread::sleep(Duration::from_millis(100).min(left)),
            _ => return Err(os_err("open seat pipe")),
        }
    };
    let mut pid = 0u32;
    // SAFETY: valid pipe handle and out pointer.
    if unsafe { GetNamedPipeServerProcessId(h.raw(), &mut pid) } == 0 {
        return Err(os_err("GetNamedPipeServerProcessId"));
    }
    if pid != expected_pid {
        return Err(StreamError::ControlPipe(format!("seat pipe served by pid {pid}, expected {expected_pid}")));
    }
    Ok(PipeClient { h, buf: FrameBuf::default(), last_progress: Instant::now() })
}
