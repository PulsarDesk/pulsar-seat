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
    HIDP_BUTTON_CAPS, HIDP_CAPS, HIDP_LINK_COLLECTION_NODE, HIDP_STATUS_SUCCESS, HIDP_VALUE_CAPS, HidD_FreePreparsedData,
    HidD_GetPreparsedData, HidP_GetButtonCaps, HidP_GetCaps, HidP_GetLinkCollectionNodes, HidP_GetSpecificValueCaps,
    HidP_GetUsageValue, HidP_GetUsages, HidP_GetValueCaps, HidP_Input, PHIDP_PREPARSED_DATA,
};
#[cfg(feature = "local-pads")]
use windows_sys::Win32::Devices::HumanInterfaceDevice::{HidP_InitializeReportForID, HidP_Output, HidP_SetUsageValue, HidP_SetUsages};
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
    /// `HIDP_CAPS.OutputReportByteLength` (report id included; 0 = no output report).
    pub output_len: u16,
    contacts: Vec<Contact>,
    /// Pen collections (0x0D/0x02): the optional pen usages per contact link.
    pen: Vec<PenCaps>,
}

/// Optional pen usages of one contact link (HID digitizer page): tilt X 0x3D, tilt Y 0x3E,
/// twist 0x41 (rotation), azimuth 0x3F (fallback for rotation).
#[derive(Debug, Clone, Default)]
struct PenCaps {
    link: u16,
    tilt_x: Option<Range>,
    tilt_y: Option<Range>,
    twist: Option<Range>,
    azimuth: Option<Range>,
}

/// One input value usage of a collection (`HIDP_VALUE_CAPS`, ranges expanded).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValueCap {
    pub page: u16,
    pub usage: u16,
    pub link: u16,
    pub min: i32,
    pub max: i32,
    pub bits: u16,
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
    pub(crate) fn from_handle(h: HANDLE) -> Option<Self> {
        let mut p: PHIDP_PREPARSED_DATA = 0;
        // SAFETY: valid handle, out pointer.
        if !unsafe { HidD_GetPreparsedData(h, &mut p) } || p == 0 {
            return None;
        }
        Self::with(Preparsed::Hid(p))
    }

    fn with(pp: Preparsed) -> Option<Self> {
        let mut dev = Self { pp, page: 0, usage: 0, input_len: 0, output_len: 0, contacts: Vec::new(), pen: Vec::new() };
        let mut caps = HIDP_CAPS::default();
        // SAFETY: valid preparsed data and out struct.
        if unsafe { HidP_GetCaps(dev.ptr(), &mut caps) } != HIDP_STATUS_SUCCESS {
            return None;
        }
        dev.page = caps.UsagePage;
        dev.usage = caps.Usage;
        dev.input_len = caps.InputReportByteLength;
        dev.output_len = caps.OutputReportByteLength;
        if dev.page == 0x0D {
            dev.contacts = dev.find_contacts(caps.NumberLinkCollectionNodes);
            if dev.usage == 0x02 {
                dev.pen = dev
                    .contacts
                    .iter()
                    .map(|c| PenCaps {
                        link: c.link,
                        tilt_x: dev.range(0x0D, c.link, 0x3D),
                        tilt_y: dev.range(0x0D, c.link, 0x3E),
                        twist: dev.range(0x0D, c.link, 0x41),
                        azimuth: dev.range(0x0D, c.link, 0x3F),
                    })
                    .collect();
            }
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

}

/// HID PID force feedback (`seat_pads::ff`): only with the `local-pads` feature, whose
/// `pad_io` is the one caller (`seat-pads` is an optional dependency, so a build of
/// `seat-host` alone must not name it).
#[cfg(feature = "local-pads")]
impl HidDevice {
    /// Output report id and logical maximum of a PID (page 0x0F) value usage.
    fn pid_value(&self, usage: u16) -> Option<(u8, i32)> {
        let mut caps = vec![HIDP_VALUE_CAPS::default(); 128];
        let mut n = caps.len() as u16;
        // SAFETY: array of `n` caps.
        if unsafe { HidP_GetValueCaps(HidP_Output, caps.as_mut_ptr(), &mut n, self.ptr()) } != HIDP_STATUS_SUCCESS {
            return None;
        }
        caps.iter().take(usize::from(n)).find_map(|c| {
            // SAFETY: the union member is chosen by `IsRange`, as documented.
            let (lo, hi) = unsafe {
                if c.IsRange { (c.Anonymous.Range.UsageMin, c.Anonymous.Range.UsageMax) } else { (c.Anonymous.NotRange.Usage, c.Anonymous.NotRange.Usage) }
            };
            (c.UsagePage == seat_pads::ff::pid::PAGE && (lo..=hi).contains(&usage)).then_some((c.ReportID, c.LogicalMax))
        })
    }

    /// Output report id of a PID (page 0x0F) selector / button usage.
    fn pid_button(&self, usage: u16) -> Option<u8> {
        let mut caps = vec![HIDP_BUTTON_CAPS::default(); 128];
        let mut n = caps.len() as u16;
        // SAFETY: array of `n` caps.
        if unsafe { HidP_GetButtonCaps(HidP_Output, caps.as_mut_ptr(), &mut n, self.ptr()) } != HIDP_STATUS_SUCCESS {
            return None;
        }
        caps.iter().take(usize::from(n)).find_map(|c| {
            // SAFETY: the union member is chosen by `IsRange`, as documented.
            let (lo, hi) = unsafe {
                if c.IsRange { (c.Anonymous.Range.UsageMin, c.Anonymous.Range.UsageMax) } else { (c.Anonymous.NotRange.Usage, c.Anonymous.NotRange.Usage) }
            };
            (c.UsagePage == seat_pads::ff::pid::PAGE && (lo..=hi).contains(&usage)).then_some(c.ReportID)
        })
    }

    /// The collection has HID PID force feedback (a periodic magnitude and an effect
    /// operation it can be started with).
    pub fn has_pid(&self) -> bool {
        use seat_pads::ff::pid;
        self.output_len > 0 && self.pid_value(pid::MAGNITUDE).is_some() && self.pid_button(pid::OP_EFFECT_START).is_some()
    }

    /// Output reports for one rumble command on a PID device (`seat_pads::ff`): enable
    /// actuators (first time), set effect (sine, block 1, infinite), set periodic
    /// (magnitude, 20 ms), effect operation (start / stop). Only usages the descriptor
    /// declares are written. HYPOTHESIS: never run on a PID device.
    pub fn pid_rumble_reports(&self, strong: u16, weak: u16, first: bool) -> Vec<Vec<u8>> {
        use seat_pads::ff::{pid, pid_magnitude};
        let len = usize::from(self.output_len);
        let new_report = |id: u8| -> Option<Vec<u8>> {
            let mut r = vec![0u8; len];
            // SAFETY: buffer of the collection's output report length.
            let st = unsafe { HidP_InitializeReportForID(HidP_Output, id, self.ptr(), r.as_mut_ptr(), len as u32) };
            (st == HIDP_STATUS_SUCCESS).then_some(r)
        };
        let set_value = |r: &mut Vec<u8>, usage: u16, v: u32| {
            // SAFETY: report of the right length; a usage not in this report is refused.
            unsafe { HidP_SetUsageValue(HidP_Output, pid::PAGE, 0, usage, v, self.ptr(), r.as_mut_ptr(), len as u32) };
        };
        let set_usage = |r: &mut Vec<u8>, usage: u16| {
            let mut u = [usage];
            let mut n = 1u32;
            // SAFETY: as above.
            unsafe { HidP_SetUsages(HidP_Output, pid::PAGE, 0, u.as_mut_ptr(), &mut n, self.ptr(), r.as_mut_ptr(), len as u32) };
        };
        let mut out = Vec::new();
        if first && let Some(mut r) = self.pid_button(pid::DC_ENABLE_ACTUATORS).and_then(new_report) {
            set_usage(&mut r, pid::DC_ENABLE_ACTUATORS);
            out.push(r);
        }
        let on = strong.max(weak) > 0;
        if on {
            if let Some(mut r) = self.pid_value(pid::DURATION).and_then(|(id, _)| new_report(id)) {
                set_value(&mut r, pid::EFFECT_BLOCK_INDEX, 1);
                set_usage(&mut r, pid::ET_SINE);
                set_value(&mut r, pid::DURATION, pid::INFINITE);
                if let Some((_, max)) = self.pid_value(pid::GAIN) {
                    set_value(&mut r, pid::GAIN, max.max(0) as u32);
                }
                out.push(r);
            }
            if let Some((id, max)) = self.pid_value(pid::MAGNITUDE)
                && let Some(mut r) = new_report(id)
            {
                set_value(&mut r, pid::EFFECT_BLOCK_INDEX, 1);
                set_value(&mut r, pid::MAGNITUDE, pid_magnitude(strong, weak, max));
                set_value(&mut r, pid::PERIOD, pid::RUMBLE_PERIOD_MS);
                out.push(r);
            }
        }
        let op = if on { pid::OP_EFFECT_START } else { pid::OP_EFFECT_STOP };
        if let Some(mut r) = self.pid_button(op).and_then(new_report) {
            set_value(&mut r, pid::EFFECT_BLOCK_INDEX, 1);
            set_usage(&mut r, op);
            set_value(&mut r, pid::LOOP_COUNT, 1);
            out.push(r);
        }
        out
    }
}

impl HidDevice {
    /// Every input value usage (ranges expanded, at most 64), for game pads
    /// (`seat_pads::generic` layouts).
    pub fn value_caps(&self) -> Vec<ValueCap> {
        let mut caps = vec![HIDP_VALUE_CAPS::default(); 64];
        let mut n = caps.len() as u16;
        // SAFETY: array of `n` caps.
        if unsafe { HidP_GetValueCaps(HidP_Input, caps.as_mut_ptr(), &mut n, self.ptr()) } != HIDP_STATUS_SUCCESS {
            return Vec::new();
        }
        let mut out = Vec::new();
        for c in caps.iter().take(usize::from(n)) {
            // SAFETY: the union member is chosen by `IsRange`, as documented.
            let (lo, hi) = unsafe {
                if c.IsRange { (c.Anonymous.Range.UsageMin, c.Anonymous.Range.UsageMax) } else { (c.Anonymous.NotRange.Usage, c.Anonymous.NotRange.Usage) }
            };
            for usage in lo..=hi.min(lo.saturating_add(63)) {
                if out.len() >= 64 {
                    break;
                }
                out.push(ValueCap { page: c.UsagePage, usage, link: c.LinkCollection, min: c.LogicalMin, max: c.LogicalMax, bits: c.BitSize });
            }
        }
        out
    }

    /// Highest usage of `page` among the input buttons (button page 0x09: the button
    /// count); 0 when there is none.
    pub fn button_max(&self, page: u16) -> u16 {
        let mut caps = vec![HIDP_BUTTON_CAPS::default(); 32];
        let mut n = caps.len() as u16;
        // SAFETY: array of `n` caps.
        if unsafe { HidP_GetButtonCaps(HidP_Input, caps.as_mut_ptr(), &mut n, self.ptr()) } != HIDP_STATUS_SUCCESS {
            return 0;
        }
        caps.iter()
            .take(usize::from(n))
            .filter(|c| c.UsagePage == page)
            // SAFETY: the union member is chosen by `IsRange`, as documented.
            .map(|c| unsafe { if c.IsRange { c.Anonymous.Range.UsageMax } else { c.Anonymous.NotRange.Usage } })
            .max()
            .unwrap_or(0)
    }

    /// Pressed usages of `page` in one report (all link collections).
    pub fn pressed(&self, page: u16, report: &[u8]) -> Vec<u16> {
        let mut rep = report.to_vec();
        self.usages(page, 0, &mut rep)
    }

    /// Logical value of one value usage in one report (sign-extended for signed ranges).
    pub fn logical(&self, cap: &ValueCap, report: &[u8]) -> Option<i32> {
        let v = self.value(cap.page, cap.link, cap.usage, report)?;
        let r = Range { min: i64::from(cap.min), max: i64::from(cap.max), bits: cap.bits };
        i32::try_from(Self::signed(v, r)).ok()
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
            let mut contact = RawContact {
                id,
                tip,
                in_range: buttons.contains(&0x32) || tip,
                x: normalize(Self::signed(xv, c.x), c.x.min, c.x.max),
                y: normalize(Self::signed(yv, c.y), c.y.min, c.y.max),
                pressure,
                ..RawContact::default()
            };
            if pen {
                // Barrel 0x44, eraser 0x45, invert 0x3C, secondary barrel 0x5A.
                contact.barrel = buttons.contains(&0x44) || buttons.contains(&0x5A);
                contact.eraser = buttons.contains(&0x45);
                contact.inverted = buttons.contains(&0x3C);
                if let Some(pc) = self.pen.iter().find(|p| p.link == c.link) {
                    let read = |r: Option<Range>, usage: u16| r.and_then(|r| self.value(0x0D, c.link, usage, rep).map(|v| (Self::signed(v, r), r)));
                    if let Some((v, r)) = read(pc.tilt_x, 0x3D) {
                        contact.tilt_x = Some(tilt_degrees(v, r));
                    }
                    if let Some((v, r)) = read(pc.tilt_y, 0x3E) {
                        contact.tilt_y = Some(tilt_degrees(v, r));
                    }
                    contact.rotation = read(pc.twist, 0x41).or_else(|| read(pc.azimuth, 0x3F)).map(|(v, r)| rotation_degrees(v, r));
                }
            }
            contacts.push(contact);
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

/// Tilt value -> degrees -90..=90. The usage is specified in degrees (unit exponent
/// usually 0 or -2); we map the logical range linearly onto -90..=90, which matches pens
/// that report -90..90 (or -9000..9000) and is a sane fallback otherwise (HYPOTHESIS).
fn tilt_degrees(v: i64, r: Range) -> i16 {
    let span = r.max - r.min;
    if span <= 0 {
        return 0;
    }
    (((v.clamp(r.min, r.max) - r.min) * 180 / span) - 90) as i16
}

/// Twist / azimuth value -> degrees 0..=359 over the logical range (one full turn).
fn rotation_degrees(v: i64, r: Range) -> u16 {
    let span = r.max - r.min + 1;
    if span <= 0 {
        return 0;
    }
    (((v.clamp(r.min, r.max) - r.min) * 360 / span) % 360) as u16
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
