// SPDX-License-Identifier: GPL-3.0-only
//! Real ViGEmBus connection: device interface lookup, overlapped DeviceIoControl.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_Device_Interface_ListW, CM_Get_Device_Interface_List_SizeW, CM_Get_Device_Interface_PropertyW,
    CM_GET_DEVICE_INTERFACE_LIST_PRESENT, CR_SUCCESS,
};
use windows_sys::Win32::Devices::Properties::{DEVPKEY_Device_InstanceId, DEVPROPTYPE};
use windows_sys::Win32::Foundation::{ERROR_IO_PENDING, ERROR_OPERATION_ABORTED, GENERIC_READ, GENERIC_WRITE, WAIT_OBJECT_0};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_OVERLAPPED, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::IO::{CancelIoEx, DeviceIoControl, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

use super::{from_wide, guid, last_error, services, split_multi_sz, wide, Handle};
use crate::error::PadError;
use crate::vigem::{proto, BusProbe, VigemConnection, VigemDriver};

pub struct WinVigemDriver;

fn interface_paths() -> Result<Vec<String>, PadError> {
    let g = guid(proto::BUS_INTERFACE_GUID);
    let mut len = 0u32;
    // SAFETY: valid GUID and out pointer.
    let r = unsafe { CM_Get_Device_Interface_List_SizeW(&mut len, &g, std::ptr::null(), CM_GET_DEVICE_INTERFACE_LIST_PRESENT) };
    if r != CR_SUCCESS {
        return Err(PadError::os("CM_Get_Device_Interface_List_SizeW", r));
    }
    let mut buf = vec![0u16; len as usize + 1];
    // SAFETY: buffer of `len` characters.
    let r = unsafe {
        CM_Get_Device_Interface_ListW(&g, std::ptr::null(), buf.as_mut_ptr(), buf.len() as u32, CM_GET_DEVICE_INTERFACE_LIST_PRESENT)
    };
    if r != CR_SUCCESS {
        return Err(PadError::os("CM_Get_Device_Interface_ListW", r));
    }
    Ok(split_multi_sz(&buf))
}

fn interface_instance(path: &str) -> Option<String> {
    let w = wide(path);
    let mut t: DEVPROPTYPE = 0;
    let mut buf = vec![0u16; 400];
    let mut size = (buf.len() * 2) as u32;
    // SAFETY: valid buffers.
    let r = unsafe {
        CM_Get_Device_Interface_PropertyW(w.as_ptr(), &DEVPKEY_Device_InstanceId, &mut t, buf.as_mut_ptr() as *mut u8, &mut size, 0)
    };
    (r == CR_SUCCESS).then(|| from_wide(&buf))
}

impl VigemDriver for WinVigemDriver {
    fn probe(&self) -> BusProbe {
        let f = services::service_facts("ViGEmBus");
        let version = f.image_path.as_deref().and_then(services::file_version);
        let present = f.exists && interface_paths().map(|p| !p.is_empty()).unwrap_or(false);
        BusProbe { present: present || f.exists, running: f.running && present, version }
    }

    fn connect(&self) -> Result<Arc<dyn VigemConnection>, PadError> {
        let paths = interface_paths()?;
        let path = paths.first().ok_or_else(|| PadError::DriverMissing("ViGEmBus interface not present".into()))?;
        let w = wide(path);
        // SAFETY: valid path; handle owned by `Handle`.
        let h = Handle(unsafe {
            CreateFileW(
                w.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OVERLAPPED,
                std::ptr::null_mut(),
            )
        });
        if !h.valid() {
            return Err(PadError::os("CreateFileW(ViGEmBus)", last_error()));
        }
        Ok(Arc::new(WinVigemConn { h, cancelled: AtomicBool::new(false), bus: interface_instance(path) }))
    }
}

struct WinVigemConn {
    h: Handle,
    cancelled: AtomicBool,
    bus: Option<String>,
}

fn event() -> Result<Handle, PadError> {
    // SAFETY: anonymous manual-reset event.
    let e = Handle(unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) });
    if !e.valid() {
        return Err(PadError::os("CreateEventW", last_error()));
    }
    Ok(e)
}

fn map_err(func: &'static str, code: u32) -> PadError {
    // ERROR_DEV_NOT_EXIST, ERROR_DEVICE_NOT_CONNECTED, ERROR_FILE_NOT_FOUND
    if matches!(code, 55 | 1167 | 2) {
        PadError::DeviceLost(format!("{func}: {code:#x}"))
    } else {
        PadError::os(func, code)
    }
}

impl WinVigemConn {
    fn start(&self, code: u32, input: &[u8], output: &mut [u8], ov: &mut OVERLAPPED) -> Result<Option<u32>, PadError> {
        let mut n = 0u32;
        // SAFETY: buffers outlive the operation (caller waits for completion before returning).
        let ok = unsafe {
            DeviceIoControl(
                self.h.0,
                code,
                input.as_ptr() as *const _,
                input.len() as u32,
                output.as_mut_ptr() as *mut _,
                output.len() as u32,
                &mut n,
                ov,
            )
        };
        if ok != 0 {
            return Ok(Some(n));
        }
        let e = last_error();
        if e == ERROR_IO_PENDING {
            Ok(None)
        } else {
            Err(map_err("DeviceIoControl", e))
        }
    }

    fn finish(&self, ov: &OVERLAPPED) -> Result<u32, PadError> {
        let mut n = 0u32;
        // SAFETY: the operation was started with `ov`; wait = TRUE.
        if unsafe { GetOverlappedResult(self.h.0, ov, &mut n, 1) } == 0 {
            return Err(map_err("GetOverlappedResult", last_error()));
        }
        Ok(n)
    }
}

impl VigemConnection for WinVigemConn {
    fn ioctl(&self, code: u32, input: &[u8], output: &mut [u8]) -> Result<usize, PadError> {
        let ev = event()?;
        let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
        ov.hEvent = ev.0;
        match self.start(code, input, output, &mut ov)? {
            Some(n) => Ok(n as usize),
            None => Ok(self.finish(&ov)? as usize),
        }
    }

    fn ioctl_wait(&self, code: u32, input: &[u8], output: &mut [u8]) -> Result<Option<usize>, PadError> {
        if self.cancelled.load(Ordering::SeqCst) {
            return Ok(None);
        }
        let ev = event()?;
        let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
        ov.hEvent = ev.0;
        if let Some(n) = self.start(code, input, output, &mut ov)? {
            return Ok(Some(n as usize));
        }
        loop {
            // SAFETY: valid event.
            if unsafe { WaitForSingleObject(ev.0, 100) } == WAIT_OBJECT_0 {
                return Ok(Some(self.finish(&ov)? as usize));
            }
            if self.cancelled.load(Ordering::SeqCst) {
                // SAFETY: cancels exactly this operation; then wait for it to finish so the
                // buffers are no longer used by the driver.
                unsafe { CancelIoEx(self.h.0, &ov) };
                return match self.finish(&ov) {
                    Ok(n) => Ok(Some(n as usize)),
                    Err(PadError::Os { code, .. }) if code == ERROR_OPERATION_ABORTED => Ok(None),
                    Err(_) => Ok(None),
                };
            }
        }
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    fn bus_instance_id(&self) -> Option<String> {
        self.bus.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read-only probe; on a box without ViGEmBus it reports not present.
    #[test]
    fn probe_is_read_only() {
        let p = WinVigemDriver.probe();
        if !p.present {
            assert!(WinVigemDriver.connect().is_err());
        }
    }
}
