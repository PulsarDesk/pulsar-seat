// SPDX-License-Identifier: GPL-3.0-only
//! `DeviceIo` over CreateFileW + overlapped DeviceIoControl, with per-IOCTL timeouts.
//!
//! Untested on hardware: the own driver is unbuilt and unsigned (drivers/split-idd).

use windows::Win32::Foundation::{
    CloseHandle, ERROR_IO_PENDING, GENERIC_READ, GENERIC_WRITE, HANDLE, WAIT_TIMEOUT,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_OVERLAPPED, FILE_SHARE_NONE, OPEN_EXISTING,
};
use windows::Win32::System::IO::{
    CancelIoEx, DeviceIoControl, GetOverlappedResult, GetOverlappedResultEx, OVERLAPPED,
};
use windows::Win32::System::Threading::{CreateEventW, INFINITE};
use windows::core::PCWSTR;

use crate::backend::pulsar_idd::{DeviceHandle, DeviceIo};
use crate::error::DisplayError;
use crate::ioctl::{self, Guid};

use super::{devices, pcwstr, wide, win32_of};

/// Timeout of `REQUEST_MODE` (spec 40 §4.4: no driver confirmation within 5 s).
pub const MODE_TIMEOUT_MS: u32 = 5000;
pub const DEFAULT_TIMEOUT_MS: u32 = 5000;

#[derive(Debug, Default, Clone, Copy)]
pub struct WinDeviceIo;

fn timeout_for(code: u32) -> u32 {
    match code {
        ioctl::IOCTL_WAIT_EVENT => INFINITE,
        ioctl::IOCTL_REQUEST_MODE => MODE_TIMEOUT_MS,
        _ => DEFAULT_TIMEOUT_MS,
    }
}

struct Event(HANDLE);
impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: event handle owned by this guard.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

impl DeviceIo for WinDeviceIo {
    fn open(&self, interface: &Guid, session_id: u32) -> Result<DeviceHandle, DisplayError> {
        let paths = devices::interface_paths(interface)?;
        let (path, _) = paths
            .into_iter()
            .find(|(_, s)| *s == Some(session_id))
            .ok_or_else(|| {
                DisplayError::DriverNotInstalled(format!(
                    "no Pulsar Split display device in session {session_id}"
                ))
            })?;
        let w = wide(&path);
        // SAFETY: NUL-terminated path; exclusive, overlapped handle.
        let h = unsafe {
            CreateFileW(
                pcwstr(&w),
                (GENERIC_READ | GENERIC_WRITE).0,
                FILE_SHARE_NONE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                None,
            )
        }
        .map_err(|e| ioctl::error_from_win32(win32_of(&e), "open display device"))?;
        Ok(DeviceHandle::new(h.0 as isize))
    }

    fn ioctl(
        &self,
        h: &DeviceHandle,
        code: u32,
        input: &[u8],
        out: &mut [u8],
    ) -> Result<usize, DisplayError> {
        // Held until the I/O has completed or been cancelled: `close` waits for it before
        // CloseHandle, so the raw value cannot be closed (and reused) under this call.
        let Some(_call) = h.enter() else {
            return Err(DisplayError::NoDisplay("device handle closed".into()));
        };
        let handle = HANDLE(h.raw() as *mut core::ffi::c_void);
        // SAFETY: manual-reset, unnamed event for the OVERLAPPED.
        let ev = Event(
            unsafe { CreateEventW(None, true, false, PCWSTR::null()) }
                .map_err(|e| DisplayError::Io(format!("CreateEventW: {}", win32_of(&e))))?,
        );
        let mut ov = OVERLAPPED {
            hEvent: ev.0,
            ..Default::default()
        };
        let mut n = 0u32;
        let inp = (!input.is_empty()).then_some(input.as_ptr() as *const core::ffi::c_void);
        let outp = (!out.is_empty()).then_some(out.as_mut_ptr() as *mut core::ffi::c_void);
        // SAFETY: buffers outlive the call: on pending I/O we wait for completion or cancel
        // and wait again before returning, so the kernel never writes after we return.
        let r = unsafe {
            DeviceIoControl(
                handle,
                code,
                inp,
                input.len() as u32,
                outp,
                out.len() as u32,
                Some(&mut n),
                Some(&mut ov),
            )
        };
        if let Err(e) = r {
            let w = win32_of(&e);
            if w != ERROR_IO_PENDING.0 {
                return Err(ioctl::error_from_win32(w, "DeviceIoControl"));
            }
            // SAFETY: same OVERLAPPED / handle as above.
            let waited =
                unsafe { GetOverlappedResultEx(handle, &ov, &mut n, timeout_for(code), false) };
            if let Err(e) = waited {
                let w = win32_of(&e);
                if w == WAIT_TIMEOUT.0 {
                    // SAFETY: cancel this request, then wait for it to finish.
                    unsafe {
                        let _ = CancelIoEx(handle, Some(&ov));
                        let _ = GetOverlappedResult(handle, &ov, &mut n, true);
                    }
                    return Err(DisplayError::Timeout(format!(
                        "ioctl 0x{code:08X} timed out"
                    )));
                }
                return Err(ioctl::error_from_win32(w, "DeviceIoControl"));
            }
        }
        Ok(n as usize)
    }

    fn close(&self, h: &DeviceHandle) {
        if h.mark_closed() {
            let handle = HANDLE(h.raw() as *mut core::ffi::c_void);
            // No new call can start (closed); cancel pending I/O (WAIT_EVENT) until every
            // in-flight call has returned, then close. The handle is still open during the
            // cancels, so CancelIoEx never hits a reused value.
            // SAFETY: handle from CreateFileW, not yet closed.
            h.drain(|| unsafe {
                let _ = CancelIoEx(handle, None);
            });
            // SAFETY: first and only close; no call is in flight.
            unsafe {
                let _ = CloseHandle(handle);
            }
        }
    }

    fn sessions(&self, interface: &Guid) -> Result<Vec<u32>, DisplayError> {
        let mut v: Vec<u32> = devices::interface_paths(interface)?
            .into_iter()
            .filter_map(|(_, s)| s)
            .collect();
        v.sort_unstable();
        v.dedup();
        Ok(v)
    }
}
