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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
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
use windows_sys::Win32::System::Threading::{CreateEventW, ResetEvent};

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

/// Reads kept in flight on the interrupt-IN pipe, so a 500/1000 Hz device does not lose
/// reports in the gap between one completed read and the next submit.
const READS_IN_FLIGHT: usize = 4;

/// Transfer length of one read: exactly one packet. A buffer larger than
/// `wMaxPacketSize` makes WinUSB wait for more packets when a report fills a whole packet
/// (it is not a short packet), so a read held ~8 reports of an 8-byte mouse and the seat
/// pointer moved in bursts (gaming PC 2026-11-25, input-trace `usb_read` 10-15/s).
fn transfer_len(max_packet: usize) -> usize {
    max_packet.max(1)
}

/// One submitted read: buffer and OVERLAPPED boxed so they stay put while pending.
struct Slot {
    buf: Vec<u8>,
    ov: Box<OVERLAPPED>,
    pending: bool,
}

// SAFETY: the event handle in the OVERLAPPED is a kernel handle usable from any thread.
unsafe impl Send for Slot {}

/// The reads in flight, oldest first (WinUSB completes a pipe's reads in order).
#[derive(Default)]
struct Ring {
    slots: Vec<Slot>,
    next: usize,
}

/// One open interface.
pub struct WinUsbDevice {
    file: usize,
    usb: usize,
    pipe: u8,
    packet: usize,
    descriptor: Option<Vec<u8>>,
    cancelled: AtomicBool,
    ring: Mutex<Ring>,
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
        let mut dev = Self {
            file: h as usize,
            usb: usb as usize,
            pipe: 0,
            packet: 64,
            descriptor: None,
            cancelled: AtomicBool::new(false),
            ring: Mutex::new(Ring::default()),
        };
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
                dev.packet = transfer_len(usize::from(pipe.MaximumPacketSize));
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

    /// One packet per completed transfer, with [`READS_IN_FLIGHT`] reads queued. Called
    /// from the one reader thread; [`Self::cancel`] does not take the ring lock.
    fn read(&self) -> BackendResult<Vec<u8>> {
        let mut ring = self.ring.lock().unwrap_or_else(|e| e.into_inner());
        if ring.slots.is_empty() {
            for _ in 0..READS_IN_FLIGHT {
                // SAFETY: manual-reset event, unnamed; closed in `Drop`.
                let ev: HANDLE = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
                if ev.is_null() {
                    return Err(last_error("create_event_failed", "CreateEventW"));
                }
                ring.slots.push(Slot { buf: vec![0u8; self.packet], ov: Box::new(OVERLAPPED { hEvent: ev, ..Default::default() }), pending: false });
            }
            for i in 0..ring.slots.len() {
                self.submit(&mut ring.slots[i])?;
            }
        }
        loop {
            if self.cancelled.load(Ordering::SeqCst) {
                return Err(cancelled());
            }
            let i = ring.next;
            let n = ring.slots.len();
            let slot = &mut ring.slots[i];
            if !slot.pending {
                self.submit(slot)?;
            }
            let mut got = 0u32;
            // SAFETY: the slot's OVERLAPPED of a submitted read; waits for completion or abort.
            let done = unsafe { WinUsb_GetOverlappedResult(self.handle(), &*slot.ov, &mut got, 1) } != 0;
            // SAFETY: no arguments.
            let err = if done { 0 } else { unsafe { GetLastError() } };
            slot.pending = false;
            ring.next = (i + 1) % n;
            if !done {
                if err == ERROR_OPERATION_ABORTED || self.cancelled.load(Ordering::SeqCst) {
                    return Err(cancelled());
                }
                return Err(BackendError::new(ErrorKind::Os, "winusb_read_failed", format!("WinUsb_ReadPipe: {err}")).with_win32(err));
            }
            let slot = &mut ring.slots[i];
            let report = slot.buf[..(got as usize).min(slot.buf.len())].to_vec();
            // Queue this slot again before returning, so a read stays in flight while the
            // caller decodes. A submit failure surfaces on the slot's next turn.
            if !self.cancelled.load(Ordering::SeqCst) {
                let _ = self.submit(slot);
            }
            if report.is_empty() {
                continue; // zero-length packet
            }
            return Ok(report);
        }
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        // SAFETY: initialised handle; aborts every pending transfer of the pipe.
        unsafe { WinUsb_AbortPipe(self.handle(), self.pipe) };
    }
}

impl WinUsbDevice {
    /// Submit one read into `slot` (its buffer and OVERLAPPED stay put until collected).
    fn submit(&self, slot: &mut Slot) -> BackendResult<()> {
        // SAFETY: event handle of this slot.
        unsafe { ResetEvent(slot.ov.hEvent) };
        let ev = slot.ov.hEvent;
        *slot.ov = OVERLAPPED { hEvent: ev, ..Default::default() };
        // SAFETY: the boxed buffer and OVERLAPPED outlive the transfer: they are only freed
        // in `Drop`, after the pipe is aborted and every pending read collected.
        let ok = unsafe { WinUsb_ReadPipe(self.handle(), self.pipe, slot.buf.as_mut_ptr(), slot.buf.len() as u32, std::ptr::null_mut(), &*slot.ov) };
        // SAFETY: no arguments.
        if ok != 0 || unsafe { GetLastError() } == ERROR_IO_PENDING {
            slot.pending = true;
            return Ok(());
        }
        // SAFETY: no arguments.
        let err = unsafe { GetLastError() };
        if err == ERROR_OPERATION_ABORTED || self.cancelled.load(Ordering::SeqCst) {
            return Err(cancelled());
        }
        Err(BackendError::new(ErrorKind::Os, "winusb_read_failed", format!("WinUsb_ReadPipe: {err}")).with_win32(err))
    }
}

impl Drop for WinUsbDevice {
    fn drop(&mut self) {
        let ring = self.ring.get_mut().unwrap_or_else(|e| e.into_inner());
        // SAFETY: handles owned by this object, freed once. Pending reads are aborted and
        // collected before their buffers and events go away.
        unsafe {
            if ring.slots.iter().any(|s| s.pending) {
                WinUsb_AbortPipe(self.usb as WINUSB_INTERFACE_HANDLE, self.pipe);
            }
            for s in ring.slots.iter_mut() {
                if s.pending {
                    let mut got = 0u32;
                    WinUsb_GetOverlappedResult(self.usb as WINUSB_INTERFACE_HANDLE, &*s.ov, &mut got, 1);
                    s.pending = false;
                }
                CloseHandle(s.ov.hEvent);
            }
            WinUsb_Free(self.usb as WINUSB_INTERFACE_HANDLE);
            CloseHandle(self.file as HANDLE);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One read is exactly one packet: a 64-byte buffer on an 8-byte pipe held ~8 mouse
    /// reports per transfer (seat pointer in bursts, gaming PC 2026-11-25).
    #[test]
    fn transfer_is_one_packet() {
        assert_eq!(transfer_len(8), 8);
        assert_eq!(transfer_len(64), 64);
        assert_eq!(transfer_len(0), 1);
    }

    /// Read-only: an unknown node has no WinUSB interface of ours.
    #[test]
    fn unknown_node_is_not_found() {
        match WinUsbSource.open(r"USB\VID_0000&PID_0000\NOPE") {
            Err(e) => assert!(matches!(e.kind, ErrorKind::NotFound | ErrorKind::Os), "{e}"),
            Ok(_) => panic!("unexpected WinUSB interface"),
        }
    }
}
