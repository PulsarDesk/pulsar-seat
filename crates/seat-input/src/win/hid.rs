// SPDX-License-Identifier: GPL-3.0-only
//! HID input reports of the collections a seat host (raw input) or the service
//! (`kmdf_filter`, [`WinHidSource`]) forwards (spec 50 §4.1.3, §4.1.4, §4.1.10): consumer
//! control (0x0C/0x01), system control (0x01/0x80), touch screen (0x0D/0x04), pen
//! (0x0D/0x02) and precision touchpad (0x0D/0x05). Parsed with the documented `HidP_*`
//! routines over the collection's preparsed data (raw input `RIDI_PREPARSEDDATA`, or
//! `HidD_GetPreparsedData` on an open collection).
//!
//! **Untested on hardware**: digitizer report layouts vary; only the documented usages
//! (X 0x01/0x30, Y 0x01/0x31, tip 0x0D/0x42, in range 0x0D/0x32, contact id 0x0D/0x51,
//! tip pressure 0x0D/0x30, finger 0x0D/0x22, stylus 0x0D/0x20) are read.

use crate::convert::{RawContact, RawUnitEvent, normalize, sign_extend};
use crate::platform::{HidReader, HidSource, cancelled, hidden_by_other_software};
use seat_core::backend::{BackendError, BackendResult, ErrorKind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use windows_sys::Win32::Devices::HumanInterfaceDevice::{
    HIDP_CAPS, HIDP_LINK_COLLECTION_NODE, HIDP_STATUS_SUCCESS, HIDP_VALUE_CAPS, HidD_FreePreparsedData, HidD_GetPreparsedData,
    HidP_GetCaps, HidP_GetLinkCollectionNodes, HidP_GetSpecificValueCaps, HidP_GetUsageValue, HidP_GetUsages, HidP_Input,
    PHIDP_PREPARSED_DATA,
};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_IO_PENDING, ERROR_OPERATION_ABORTED, ERROR_SHARING_VIOLATION, GENERIC_READ,
    GetLastError, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_OVERLAPPED, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, ReadFile,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Threading::CreateEventW;

#[derive(Debug, Clone, Copy, Default)]
struct Range {
    min: i64,
    max: i64,
    /// Field size (`HIDP_VALUE_CAPS.BitSize`), for sign extension.
    bits: u16,
}

/// Preparsed data: our own aligned copy (raw input) or the HID class driver's buffer.
enum Preparsed {
    Owned(Vec<u64>),
    Hid(isize),
}

impl Drop for Preparsed {
    fn drop(&mut self) {
        if let Self::Hid(p) = self {
            // SAFETY: returned by HidD_GetPreparsedData, freed once.
            let _ = unsafe { HidD_FreePreparsedData(*p as PHIDP_PREPARSED_DATA) };
        }
    }
}

#[derive(Debug, Clone)]
struct Contact {
    link: u16,
    x: Range,
    y: Range,
    pressure: Option<Range>,
}

/// One HID collection's parser. Owns a copy of the preparsed data.
pub struct HidDevice {
    pp: Preparsed,
    pub page: u16,
    pub usage: u16,
    /// `HIDP_CAPS.InputReportByteLength` (report id included).
    pub input_len: u16,
    contacts: Vec<Contact>,
}

// SAFETY: the preparsed data is read-only after creation; HidP_* calls on it only read.
unsafe impl Send for HidDevice {}
// SAFETY: as above.
unsafe impl Sync for HidDevice {}

impl HidDevice {
    /// From the raw `RIDI_PREPARSEDDATA` bytes.
    pub fn new(preparsed: &[u8]) -> Option<Self> {
        let mut pp = vec![0u64; preparsed.len().div_ceil(8)];
        // SAFETY: `pp` has at least `preparsed.len()` bytes.
        unsafe { std::ptr::copy_nonoverlapping(preparsed.as_ptr(), pp.as_mut_ptr().cast::<u8>(), preparsed.len()) };
        Self::with(Preparsed::Owned(pp))
    }

    /// From an open collection handle (`HidD_GetPreparsedData`).
    fn from_handle(h: HANDLE) -> Option<Self> {
        let mut p: PHIDP_PREPARSED_DATA = 0;
        // SAFETY: valid handle, out pointer.
        if !unsafe { HidD_GetPreparsedData(h, &mut p) } || p == 0 {
            return None;
        }
        Self::with(Preparsed::Hid(p))
    }

    fn with(pp: Preparsed) -> Option<Self> {
        let mut dev = Self { pp, page: 0, usage: 0, input_len: 0, contacts: Vec::new() };
        let mut caps = HIDP_CAPS::default();
        // SAFETY: valid preparsed data and out struct.
        if unsafe { HidP_GetCaps(dev.ptr(), &mut caps) } != HIDP_STATUS_SUCCESS {
            return None;
        }
        dev.page = caps.UsagePage;
        dev.usage = caps.Usage;
        dev.input_len = caps.InputReportByteLength;
        if dev.page == 0x0D {
            dev.contacts = dev.find_contacts(caps.NumberLinkCollectionNodes);
        }
        Some(dev)
    }

    fn ptr(&self) -> PHIDP_PREPARSED_DATA {
        match &self.pp {
            Preparsed::Owned(v) => v.as_ptr() as PHIDP_PREPARSED_DATA,
            Preparsed::Hid(p) => *p as PHIDP_PREPARSED_DATA,
        }
    }

    fn range(&self, page: u16, link: u16, usage: u16) -> Option<Range> {
        let mut vc = [HIDP_VALUE_CAPS::default(); 4];
        let mut n = vc.len() as u16;
        // SAFETY: array of `n` caps.
        let st = unsafe { HidP_GetSpecificValueCaps(HidP_Input, page, link, usage, vc.as_mut_ptr(), &mut n, self.ptr()) };
        if st != HIDP_STATUS_SUCCESS || n == 0 {
            return None;
        }
        let c = vc[0];
        Some(Range { min: i64::from(c.LogicalMin), max: i64::from(c.LogicalMax), bits: c.BitSize })
    }

    fn find_contacts(&self, nodes: u16) -> Vec<Contact> {
        let mut list = vec![HIDP_LINK_COLLECTION_NODE::default(); usize::from(nodes.max(1))];
        let mut n = list.len() as u32;
        // SAFETY: array of `n` nodes.
        let st = unsafe { HidP_GetLinkCollectionNodes(list.as_mut_ptr(), &mut n, self.ptr()) };
        let mut links: Vec<u16> = if st == HIDP_STATUS_SUCCESS {
            list.iter()
                .take(n as usize)
                .enumerate()
                .filter(|(_, l)| l.LinkUsagePage == 0x0D && matches!(l.LinkUsage, 0x20 | 0x22))
                .map(|(i, _)| i as u16)
                .collect()
        } else {
            Vec::new()
        };
        if links.is_empty() {
            links.push(0);
        }
        links
            .into_iter()
            .filter_map(|link| {
                Some(Contact {
                    link,
                    x: self.range(0x01, link, 0x30)?,
                    y: self.range(0x01, link, 0x31)?,
                    pressure: self.range(0x0D, link, 0x30),
                })
            })
            .collect()
    }

    fn usages(&self, page: u16, link: u16, report: &mut [u8]) -> Vec<u16> {
        let mut buf = [0u16; 32];
        let mut n = buf.len() as u32;
        // SAFETY: usage buffer of `n` entries, report buffer of its length.
        let st = unsafe {
            HidP_GetUsages(HidP_Input, page, link, buf.as_mut_ptr(), &mut n, self.ptr(), report.as_mut_ptr(), report.len() as u32)
        };
        if st != HIDP_STATUS_SUCCESS {
            return Vec::new();
        }
        buf[..n as usize].to_vec()
    }

    fn value(&self, page: u16, link: u16, usage: u16, report: &[u8]) -> Option<u32> {
        let mut v = 0u32;
        // SAFETY: report buffer of its length; out value.
        let st = unsafe { HidP_GetUsageValue(HidP_Input, page, link, usage, &mut v, self.ptr(), report.as_ptr(), report.len() as u32) };
        (st == HIDP_STATUS_SUCCESS).then_some(v)
    }

    /// Sign-extend a logical value by its field size when the range is signed.
    fn signed(v: u32, r: Range) -> i64 {
        if r.min < 0 { sign_extend(v, r.bits) } else { i64::from(v) }
    }

    fn contacts(&self, rep: &mut [u8], pen: bool) -> Vec<RawContact> {
        let mut contacts = Vec::new();
        for c in &self.contacts {
            let Some(xv) = self.value(0x01, c.link, 0x30, rep) else { continue };
            let Some(yv) = self.value(0x01, c.link, 0x31, rep) else { continue };
            let buttons = self.usages(0x0D, c.link, rep);
            let id = self.value(0x0D, c.link, 0x51, rep).unwrap_or(u32::from(c.link));
            let pressure = match (pen, c.pressure, self.value(0x0D, c.link, 0x30, rep)) {
                (true, Some(r), Some(v)) => (u32::from(normalize(Self::signed(v, r), r.min, r.max)) * 1024 / 65535) as u16,
                _ => 0,
            };
            let tip = buttons.contains(&0x42);
            contacts.push(RawContact {
                id,
                tip,
                in_range: buttons.contains(&0x32) || tip,
                x: normalize(Self::signed(xv, c.x), c.x.min, c.x.max),
                y: normalize(Self::signed(yv, c.y), c.y.min, c.y.max),
                pressure,
            });
        }
        contacts
    }

    /// One input report → event (`None` for collections we do not forward).
    pub fn parse(&self, report: &[u8]) -> Option<RawUnitEvent> {
        let mut rep = report.to_vec();
        match (self.page, self.usage) {
            (0x0C, _) => Some(RawUnitEvent::Consumer { usages: self.usages(0x0C, 0, &mut rep) }),
            (0x01, 0x80) => Some(RawUnitEvent::System { usages: self.usages(0x01, 0, &mut rep) }),
            (0x0D, 0x04) | (0x0D, 0x02) => {
                let pen = self.usage == 0x02;
                Some(RawUnitEvent::Digitizer { contacts: self.contacts(&mut rep, pen), pen })
            }
            (0x0D, 0x05) => {
                let contacts = self.contacts(&mut rep, false);
                // Pad button: Button page (0x09) usage 1 in the top-level collection.
                let button = self.usages(0x09, 0, &mut rep).contains(&1);
                Some(RawUnitEvent::Touchpad { contacts, button })
            }
            _ => None,
        }
    }
}

fn hid_error(reason: &str, what: &str) -> BackendError {
    // SAFETY: no arguments.
    let e = unsafe { GetLastError() };
    BackendError::new(ErrorKind::Os, reason, format!("{what}: Win32 error {e}")).with_win32(e)
}

/// [`HidSource`] over `CreateFileW` on the collection's interface path (the service runs
/// in session 0, which our filter always allows).
pub struct WinHidSource;

impl HidSource for WinHidSource {
    fn open(&self, interface_path: &str) -> BackendResult<Arc<dyn HidReader>> {
        let path = super::wide(interface_path);
        // SAFETY: NUL-terminated path; shared so other readers of the collection keep working.
        let h = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                std::ptr::null_mut(),
            )
        };
        if h == INVALID_HANDLE_VALUE {
            // SAFETY: no arguments.
            let e = unsafe { GetLastError() };
            if e == ERROR_SHARING_VIOLATION || e == ERROR_ACCESS_DENIED {
                return Err(hidden_by_other_software(interface_path).with_win32(e));
            }
            return Err(BackendError::new(ErrorKind::Os, "hid_open_failed", format!("open {interface_path}: Win32 error {e}")).with_win32(e));
        }
        let Some(dev) = HidDevice::from_handle(h) else {
            // SAFETY: our handle.
            unsafe { CloseHandle(h) };
            return Err(BackendError::new(ErrorKind::Os, "hid_preparsed_failed", format!("no preparsed data for {interface_path}")));
        };
        Ok(Arc::new(WinHidReader { h: h as usize, dev, cancelled: AtomicBool::new(false) }))
    }
}

/// One open collection; overlapped reads so [`HidReader::cancel`] can stop a pending one.
pub struct WinHidReader {
    h: usize,
    dev: HidDevice,
    cancelled: AtomicBool,
}

impl WinHidReader {
    fn handle(&self) -> HANDLE {
        self.h as HANDLE
    }
}

impl Drop for WinHidReader {
    fn drop(&mut self) {
        // SAFETY: handle owned by this object.
        unsafe { CloseHandle(self.handle()) };
    }
}

impl HidReader for WinHidReader {
    fn read(&self) -> BackendResult<Option<RawUnitEvent>> {
        if self.cancelled.load(Ordering::SeqCst) {
            return Err(cancelled());
        }
        let mut buf = vec![0u8; usize::from(self.dev.input_len.max(1))];
        // SAFETY: manual-reset event, unnamed.
        let ev = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if ev.is_null() {
            return Err(hid_error("create_event_failed", "CreateEventW"));
        }
        let mut ov = OVERLAPPED { hEvent: ev, ..Default::default() };
        let mut got = 0u32;
        // SAFETY: buffer and OVERLAPPED stay alive until GetOverlappedResult returns; the
        // byte count comes from GetOverlappedResult only (overlapped handle).
        let ok = unsafe { ReadFile(self.handle(), buf.as_mut_ptr(), buf.len() as u32, std::ptr::null_mut(), &mut ov) };
        // SAFETY: no arguments.
        let started = ok != 0 || unsafe { GetLastError() } == ERROR_IO_PENDING;
        let result = if started {
            // SAFETY: same handle and OVERLAPPED; wait for completion.
            if unsafe { GetOverlappedResult(self.handle(), &ov, &mut got, 1) } != 0 {
                Ok(())
            } else {
                Err(read_error())
            }
        } else {
            Err(read_error())
        };
        // SAFETY: event created above.
        unsafe { CloseHandle(ev) };
        result?;
        buf.truncate(got as usize);
        if buf.is_empty() {
            return Ok(None);
        }
        Ok(self.dev.parse(&buf))
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        // SAFETY: cancels every pending I/O of this handle.
        unsafe { CancelIoEx(self.handle(), std::ptr::null()) };
    }
}

fn read_error() -> BackendError {
    // SAFETY: no arguments.
    let e = unsafe { GetLastError() };
    if e == ERROR_OPERATION_ABORTED {
        cancelled()
    } else {
        BackendError::new(ErrorKind::Os, "hid_read_failed", format!("ReadFile: Win32 error {e}")).with_win32(e)
    }
}
