// SPDX-License-Identifier: GPL-3.0-only
//! Physical pads for [`crate::local_pads::LocalPadHub`]: the pad's HID collection opened by
//! the service (session 0) with shared read / write access, overlapped reads with a
//! timeout, output reports written with `WriteFile`.
//!
//! HYPOTHESIS (untested on hardware): a shared open from session 0 does not disturb the
//! seat's own readers of the pad (games open HID pads shared too), and `WriteFile` of the
//! USB output report layout reaches a DualSense / DS4 on USB. Bluetooth output needs a CRC
//! and is not written (see `LocalFeedbackRouter`).

use std::sync::Arc;

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_IO_PENDING, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
    WAIT_TIMEOUT,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_OVERLAPPED, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, ReadFile, WriteFile,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

use crate::local_pads::{PhysicalPadIo, PhysicalPadSource, PhysicalPadWriter};

/// Largest input report read (DualSense Bluetooth 0x31 is 78 bytes).
const READ_BUFFER: usize = 128;
/// A feedback write that does not finish in this time is cancelled.
const WRITE_TIMEOUT_MS: u32 = 200;

/// Owned handle (file or event), closed on drop.
struct Owned(usize);

impl Owned {
    fn raw(&self) -> HANDLE {
        self.0 as HANDLE
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: handle owned by this object.
        unsafe { CloseHandle(self.raw()) };
    }
}

fn last_error(what: &str) -> String {
    // SAFETY: no arguments.
    format!("{what}: Win32 error {}", unsafe { GetLastError() })
}

fn event() -> Result<Owned, String> {
    // SAFETY: unnamed manual-reset event.
    let e = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
    if e.is_null() { Err(last_error("CreateEventW")) } else { Ok(Owned(e as usize)) }
}

/// [`PhysicalPadSource`] over `CreateFileW` on the pad's HID interface path.
pub struct WinPadSource;

impl PhysicalPadSource for WinPadSource {
    fn open(&self, interface_path: &str) -> Result<Box<dyn PhysicalPadIo>, String> {
        let path = super::wide(interface_path);
        // SAFETY: NUL-terminated path; shared so the seat's own readers keep working.
        let h = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                std::ptr::null_mut(),
            )
        };
        if h == INVALID_HANDLE_VALUE {
            return Err(last_error(&format!("open {interface_path}")));
        }
        Ok(Box::new(WinPadIo { h: Arc::new(Owned(h as usize)), ev: event()? }))
    }
}

struct WinPadIo {
    h: Arc<Owned>,
    ev: Owned,
}

// SAFETY: the handles are plain kernel handles; every call passes its own OVERLAPPED.
unsafe impl Send for WinPadIo {}

impl PhysicalPadIo for WinPadIo {
    fn read(&mut self, timeout_ms: u32) -> Result<Option<Vec<u8>>, String> {
        let mut buf = vec![0u8; READ_BUFFER];
        let mut ov = OVERLAPPED { hEvent: self.ev.raw(), ..Default::default() };
        let mut got = 0u32;
        // SAFETY: buffer and OVERLAPPED live until the I/O is finished or cancelled below.
        let ok = unsafe { ReadFile(self.h.raw(), buf.as_mut_ptr(), buf.len() as u32, std::ptr::null_mut(), &mut ov) };
        // SAFETY: no arguments.
        if ok == 0 && unsafe { GetLastError() } != ERROR_IO_PENDING {
            return Err(last_error("ReadFile"));
        }
        // SAFETY: our event.
        let w = unsafe { WaitForSingleObject(self.ev.raw(), timeout_ms) };
        if w == WAIT_TIMEOUT {
            // SAFETY: cancel this read, then wait for it so the buffer is no longer used.
            unsafe {
                CancelIoEx(self.h.raw(), &ov);
                GetOverlappedResult(self.h.raw(), &ov, &mut got, 1);
            }
            return Ok(None);
        }
        if w != WAIT_OBJECT_0 {
            return Err(last_error("WaitForSingleObject"));
        }
        // SAFETY: the I/O has completed.
        if unsafe { GetOverlappedResult(self.h.raw(), &ov, &mut got, 0) } == 0 {
            return Err(last_error("GetOverlappedResult"));
        }
        buf.truncate(got as usize);
        Ok((!buf.is_empty()).then_some(buf))
    }

    fn writer(&self) -> Arc<dyn PhysicalPadWriter> {
        Arc::new(WinPadWriter { h: self.h.clone() })
    }
}

struct WinPadWriter {
    h: Arc<Owned>,
}

// SAFETY: see WinPadIo; each write has its own event and OVERLAPPED.
unsafe impl Send for WinPadWriter {}
// SAFETY: as above.
unsafe impl Sync for WinPadWriter {}

impl PhysicalPadWriter for WinPadWriter {
    fn write(&self, report: &[u8]) -> Result<(), String> {
        let ev = event()?;
        let mut ov = OVERLAPPED { hEvent: ev.raw(), ..Default::default() };
        let mut done = 0u32;
        // SAFETY: report and OVERLAPPED live until the write finished or was cancelled.
        let ok = unsafe { WriteFile(self.h.raw(), report.as_ptr(), report.len() as u32, std::ptr::null_mut(), &mut ov) };
        // SAFETY: no arguments.
        if ok == 0 && unsafe { GetLastError() } != ERROR_IO_PENDING {
            return Err(last_error("WriteFile"));
        }
        // SAFETY: our event.
        if unsafe { WaitForSingleObject(ev.raw(), WRITE_TIMEOUT_MS) } != WAIT_OBJECT_0 {
            // SAFETY: cancel and wait, so `report` is no longer used.
            unsafe {
                CancelIoEx(self.h.raw(), &ov);
                GetOverlappedResult(self.h.raw(), &ov, &mut done, 1);
            }
            return Err("WriteFile: timed out".into());
        }
        // SAFETY: completed.
        if unsafe { GetOverlappedResult(self.h.raw(), &ov, &mut done, 0) } == 0 {
            return Err(last_error("GetOverlappedResult"));
        }
        Ok(())
    }
}
