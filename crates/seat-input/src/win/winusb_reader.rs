// SPDX-License-Identifier: GPL-3.0-only
//! Reader of a captured USB keyboard / mouse interface over WinUSB (osms G2,
//! [`crate::usb_capture::UsbReportSource`]).
//!
//! Written from Microsoft's WinUSB function documentation: the interface is found by our
//! interface GUID on the captured devnode (`CM_Get_Device_Interface_ListW` with the
//! instance id), opened with `CreateFileW(FILE_FLAG_OVERLAPPED)` + `WinUsb_Initialize`,
//! its first interrupt-IN pipe is found with `WinUsb_QueryInterfaceSettings` /
//! `WinUsb_QueryPipe`, the HID report descriptor is read with a standard
//! `GET_DESCRIPTOR` (type 0x22, interface recipient, HID 1.11 §7.1.1), and reports are
//! read with overlapped `WinUsb_ReadPipe`. `WinUsb_AbortPipe` ends a pending read.
//! Same code on x64 and ARM64 (windows-sys types, no hand-sized structs).
//!
//! **Untested on hardware (HYPOTHESIS):** every call here; in particular that the device
//! answers `GET_DESCRIPTOR(0x22)` on the WinUSB control pipe (else the boot layout is
//! used) and that report protocol stays on after the swap (HID devices start in report
//! protocol; nothing here sends `SET_PROTOCOL`).

use super::{cr_error, guid, last_error, wide};
use crate::platform::cancelled;
use crate::usb_capture::{INTERFACE_GUID, UsbReportReader, UsbReportSource};
use seat_core::backend::{BackendError, BackendResult, ErrorKind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_GET_DEVICE_INTERFACE_LIST_PRESENT, CM_Get_Device_Interface_List_SizeW, CM_Get_Device_Interface_ListW, CR_BUFFER_SMALL, CR_SUCCESS,
};
use windows_sys::Win32::Devices::Usb::{
    USB_INTERFACE_DESCRIPTOR, UsbdPipeTypeInterrupt, WINUSB_INTERFACE_HANDLE, WINUSB_PIPE_INFORMATION, WINUSB_SETUP_PACKET, WinUsb_AbortPipe,
    WinUsb_ControlTransfer, WinUsb_Free, WinUsb_GetOverlappedResult, WinUsb_Initialize, WinUsb_QueryInterfaceSettings, WinUsb_QueryPipe, WinUsb_ReadPipe,
};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_IO_PENDING, ERROR_OPERATION_ABORTED, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_OVERLAPPED, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
use windows_sys::Win32::System::IO::OVERLAPPED;
use windows_sys::Win32::System::Threading::CreateEventW;

/// Largest report descriptor we ask for.
const MAX_DESCRIPTOR: u16 = 4096;

/// Present WinUSB interface paths of our GUID on `node`.
fn interface_paths(node: &str) -> BackendResult<Vec<String>> {
    let g = guid(INTERFACE_GUID).ok_or_else(|| BackendError::new(ErrorKind::Internal, "bad_guid", INTERFACE_GUID))?;
    let id = wide(node);
    for _ in 0..8 {
        let mut len = 0u32;
        // SAFETY: valid GUID, NUL-terminated instance id, out pointer.
        let cr = unsafe { CM_Get_Device_Interface_List_SizeW(&mut len, &g, id.as_ptr(), CM_GET_DEVICE_INTERFACE_LIST_PRESENT) };
        if cr != CR_SUCCESS {
            return Err(cr_error(cr, "CM_Get_Device_Interface_List_SizeW"));
        }
        let mut buf = vec![0u16; len.max(1) as usize];
        // SAFETY: buffer of `len` units.
        let cr = unsafe { CM_Get_Device_Interface_ListW(&g, id.as_ptr(), buf.as_mut_ptr(), len.max(1), CM_GET_DEVICE_INTERFACE_LIST_PRESENT) };
        if cr == CR_BUFFER_SMALL {
            continue;
        }
        if cr != CR_SUCCESS {
            return Err(cr_error(cr, "CM_Get_Device_Interface_ListW"));
        }
        return Ok(buf.split(|c| *c == 0).filter(|s| !s.is_empty()).map(String::from_utf16_lossy).collect());
    }
    Err(BackendError::new(ErrorKind::Os, "interface_list_unstable", "device interface list kept changing"))
}

/// Opens captured nodes.
pub struct WinUsbSource;

impl UsbReportSource for WinUsbSource {
    fn open(&self, node: &str) -> BackendResult<Arc<dyn UsbReportReader>> {
        let path = interface_paths(node)?
            .into_iter()
            .next()
            .ok_or_else(|| BackendError::new(ErrorKind::NotFound, "winusb_interface_absent", format!("{node} has no WinUSB interface yet")))?;
        Ok(Arc::new(WinUsbDevice::open(&path)?))
    }
}

/// One open interface.
pub struct WinUsbDevice {
    file: usize,
    usb: usize,
    pipe: u8,
    packet: usize,
    descriptor: Option<Vec<u8>>,
    cancelled: AtomicBool,
}

// SAFETY: the handles are kernel / WinUSB handles usable from any thread; WinUSB allows a
// read on one thread and `WinUsb_AbortPipe` on another.
unsafe impl Send for WinUsbDevice {}
// SAFETY: as above; no interior state besides the atomic.
unsafe impl Sync for WinUsbDevice {}

impl WinUsbDevice {
    fn handle(&self) -> WINUSB_INTERFACE_HANDLE {
        self.usb as WINUSB_INTERFACE_HANDLE
    }

    fn open(path: &str) -> BackendResult<Self> {
        let p = wide(path);
        // SAFETY: NUL-terminated path; no security attributes, no template.
        let h = unsafe {
            CreateFileW(
                p.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OVERLAPPED,
                std::ptr::null_mut(),
            )
        };
        if h == INVALID_HANDLE_VALUE {
            return Err(last_error("winusb_open_failed", "CreateFileW(WinUSB interface)"));
        }
        let mut usb: WINUSB_INTERFACE_HANDLE = std::ptr::null_mut();
        // SAFETY: overlapped file handle from above.
        if unsafe { WinUsb_Initialize(h, &mut usb) } == 0 {
            let e = last_error("winusb_init_failed", "WinUsb_Initialize");
            // SAFETY: handle from above.
            unsafe { CloseHandle(h) };
            return Err(e);
        }
        let mut dev = Self { file: h as usize, usb: usb as usize, pipe: 0, packet: 64, descriptor: None, cancelled: AtomicBool::new(false) };
        let mut desc = USB_INTERFACE_DESCRIPTOR::default();
        // SAFETY: initialised handle, sized out struct.
        if unsafe { WinUsb_QueryInterfaceSettings(dev.handle(), 0, &mut desc) } == 0 {
            return Err(last_error("winusb_query_failed", "WinUsb_QueryInterfaceSettings"));
        }
        for i in 0..desc.bNumEndpoints {
            let mut pipe = WINUSB_PIPE_INFORMATION::default();
            // SAFETY: as above.
            if unsafe { WinUsb_QueryPipe(dev.handle(), 0, i, &mut pipe) } == 0 {
                continue;
            }
            if pipe.PipeType == UsbdPipeTypeInterrupt && pipe.PipeId & 0x80 != 0 {
                dev.pipe = pipe.PipeId;
                dev.packet = usize::from(pipe.MaximumPacketSize).max(8);
                break;
            }
        }
        if dev.pipe == 0 {
            return Err(BackendError::new(ErrorKind::Unsupported, "no_interrupt_in_pipe", format!("{path} has no interrupt-IN endpoint")));
        }
        dev.descriptor = dev.read_descriptor(desc.bInterfaceNumber);
        Ok(dev)
    }

    /// `GET_DESCRIPTOR(HID report)` to the interface; `None` when the device refuses.
    fn read_descriptor(&self, interface: u8) -> Option<Vec<u8>> {
        let setup = WINUSB_SETUP_PACKET {
            // Device-to-host, standard, interface recipient.
            RequestType: 0x81,
            Request: 0x06,
            Value: 0x2200,
            Index: u16::from(interface),
            Length: MAX_DESCRIPTOR,
        };
        let mut buf = vec![0u8; usize::from(MAX_DESCRIPTOR)];
        let mut got = 0u32;
        // SAFETY: buffer of `Length` bytes; synchronous control transfer.
        let ok = unsafe { WinUsb_ControlTransfer(self.handle(), setup, buf.as_mut_ptr(), buf.len() as u32, &mut got, std::ptr::null()) };
        if ok == 0 || got == 0 {
            log::debug!("WinUSB: report descriptor not returned (Win32 {})", unsafe { GetLastError() });
            return None;
        }
        buf.truncate(got as usize);
        Some(buf)
    }
}

impl UsbReportReader for WinUsbDevice {
    fn report_descriptor(&self) -> Option<Vec<u8>> {
        self.descriptor.clone()
    }

    fn read(&self) -> BackendResult<Vec<u8>> {
        loop {
            if self.cancelled.load(Ordering::SeqCst) {
                return Err(cancelled());
            }
            let mut buf = vec![0u8; self.packet.max(64)];
            // SAFETY: manual-reset event, unnamed.
            let ev: HANDLE = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
            if ev.is_null() {
                return Err(last_error("create_event_failed", "CreateEventW"));
            }
            let ov = OVERLAPPED { hEvent: ev, ..Default::default() };
            let mut got = 0u32;
            // SAFETY: buffer and OVERLAPPED live until the result is collected below.
            let ok = unsafe { WinUsb_ReadPipe(self.handle(), self.pipe, buf.as_mut_ptr(), buf.len() as u32, std::ptr::null_mut(), &ov) };
            // SAFETY: no arguments.
            let started = ok != 0 || unsafe { GetLastError() } == ERROR_IO_PENDING;
            let done = started
                // SAFETY: same handle / OVERLAPPED; waits for completion or abort.
                && unsafe { WinUsb_GetOverlappedResult(self.handle(), &ov, &mut got, 1) } != 0;
            // SAFETY: no arguments (read before CloseHandle can change it).
            let err = if done { 0 } else { unsafe { GetLastError() } };
            // SAFETY: event created above.
            unsafe { CloseHandle(ev) };
            if !done {
                if err == ERROR_OPERATION_ABORTED || self.cancelled.load(Ordering::SeqCst) {
                    return Err(cancelled());
                }
                return Err(BackendError::new(ErrorKind::Os, "winusb_read_failed", format!("WinUsb_ReadPipe: {err}")).with_win32(err));
            }
            if got == 0 {
                continue; // zero-length packet
            }
            buf.truncate(got as usize);
            return Ok(buf);
        }
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        // SAFETY: initialised handle; aborts every pending transfer of the pipe.
        unsafe { WinUsb_AbortPipe(self.handle(), self.pipe) };
    }
}

impl Drop for WinUsbDevice {
    fn drop(&mut self) {
        // SAFETY: handles owned by this object, freed once.
        unsafe {
            WinUsb_Free(self.handle());
            CloseHandle(self.file as HANDLE);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read-only: an unknown node has no WinUSB interface of ours.
    #[test]
    fn unknown_node_is_not_found() {
        match WinUsbSource.open(r"USB\VID_0000&PID_0000\NOPE") {
            Err(e) => assert!(matches!(e.kind, ErrorKind::NotFound | ErrorKind::Os), "{e}"),
            Ok(_) => panic!("unexpected WinUSB interface"),
        }
    }
}
