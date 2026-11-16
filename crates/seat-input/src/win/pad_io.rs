// SPDX-License-Identifier: GPL-3.0-only
//! Physical pads for [`crate::local_pads::LocalPadHub`]: the pad's HID collection opened by
//! the service (session 0) with shared read / write access, overlapped reads with a
//! timeout, output reports written with `WriteFile`.
//!
//! The collection's preparsed data (`HidD_GetPreparsedData`) decodes generic HID pads
//! (Xbox HID collections, generic joysticks): [`PhysicalPadIo::hid_layout`] lists its value
//! usages, [`PhysicalPadIo::hid_sample`] reads buttons (page 0x09), consumer usages and
//! every value with `HidP_*`. Output reports shorter than the collection's
//! `OutputReportByteLength` are zero-padded (`WriteFile` on a HID collection needs exactly
//! that length).
//!
//! HYPOTHESIS (untested on hardware): a shared open from session 0 does not disturb the
//! seat's own readers of the pad (games open HID pads shared too); `WriteFile` of the output
//! report layouts (USB and the Bluetooth reports with CRC, `seat_pads::bt`) reaches the pad;
//! padding to the longest output report is accepted by the Bluetooth HID stack.

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
use crate::win::hid::{HidDevice, ValueCap};
use seat_pads::generic::{HidLayout, HidSample, HidValue, PAGE_BUTTON, PAGE_CONSUMER};

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
        let owned = Arc::new(Owned(h as usize));
        // The preparsed data stays readable after the open (read-only queries).
        let dev = HidDevice::from_handle(h);
        let output_len = dev.as_ref().map_or(0, |d| usize::from(d.output_len));
        let caps = dev.as_ref().map(HidDevice::value_caps).unwrap_or_default();
        Ok(Box::new(WinPadIo { h: owned, ev: event()?, dev, caps, output_len }))
    }
}

struct WinPadIo {
    h: Arc<Owned>,
    ev: Owned,
    dev: Option<HidDevice>,
    /// Input value usages (generic decoding), read once.
    caps: Vec<ValueCap>,
    output_len: usize,
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
        Arc::new(WinPadWriter { h: self.h.clone(), output_len: self.output_len })
    }

    fn hid_layout(&self) -> Option<HidLayout> {
        let dev = self.dev.as_ref()?;
        if self.caps.is_empty() {
            return None;
        }
        Some(HidLayout { values: self.caps.iter().map(|c| (c.page, c.usage)).collect(), button_count: dev.button_max(PAGE_BUTTON) })
    }

    fn hid_sample(&self, report: &[u8]) -> Option<HidSample> {
        let dev = self.dev.as_ref()?;
        let values: Vec<HidValue> = self
            .caps
            .iter()
            .filter_map(|c| dev.logical(c, report).map(|value| HidValue { page: c.page, usage: c.usage, value, min: c.min, max: c.max }))
            .collect();
        let buttons = dev.pressed(PAGE_BUTTON, report);
        if values.is_empty() && buttons.is_empty() {
            // Another report id (vendor / feature data): nothing of ours in it.
            return None;
        }
        Some(HidSample { buttons, consumer: dev.pressed(PAGE_CONSUMER, report), values })
    }
}

struct WinPadWriter {
    h: Arc<Owned>,
    /// `OutputReportByteLength` of the collection (0 = unknown: written as given).
    output_len: usize,
}

// SAFETY: see WinPadIo; each write has its own event and OVERLAPPED.
unsafe impl Send for WinPadWriter {}
// SAFETY: as above.
unsafe impl Sync for WinPadWriter {}

impl PhysicalPadWriter for WinPadWriter {
    fn write(&self, report: &[u8]) -> Result<(), String> {
        let padded;
        let report = if self.output_len > report.len() {
            let mut v = report.to_vec();
            v.resize(self.output_len, 0);
            padded = v;
            &padded[..]
        } else {
            report
        };
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
