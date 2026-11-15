// SPDX-License-Identifier: GPL-3.0-only
//! Raw input reader for the seat host (spec 30 §6.1.1 point 6, spec 50 §4.1.4).
//!
//! A message-only window on its own thread registers keyboards, mice and the forwarded
//! HID collections with `RIDEV_INPUTSINK | RIDEV_DEVNOTIFY` (input arrives even though the
//! window never has focus; arrival / removal arrive as `WM_INPUT_DEVICE_CHANGE`). Each
//! `WM_INPUT` is mapped from its device handle to the device-interface path
//! (`RIDI_DEVICENAME`) and kept only if the path is in the seat's unit set.
//!
//! - `no_legacy` adds `RIDEV_NOLEGACY` for keyboards and mice: the host process then gets no
//!   legacy key / mouse messages at all (its presenter windows must not act on console
//!   input). Under `HostTopology::DedicatedHost` this completes user-mode isolation; under
//!   `SeatOneConsole` the console's own windows still get every key (`input_not_isolated`).
//! - Only one window per process may own a usage registration; the presenter must not
//!   register raw input itself.
//! - Injected input (`hDevice == NULL`) is ignored.
//!
//! **Untested on hardware** (needs an interactive session with several keyboards).

use super::hid::HidDevice;
use super::wide;
use crate::convert::RawUnitEvent;
use crate::units::normalize_interface_path;
use seat_core::backend::{BackendError, BackendResult, ErrorKind};
use std::collections::{HashMap, HashSet};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use windows_sys::Win32::Foundation::{GetLastError, HANDLE, HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::{
    GetRawInputData, GetRawInputDeviceInfoW, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER, RID_DEVICE_INFO,
    RID_INPUT, RIDEV_DEVNOTIFY, RIDEV_INPUTSINK, RIDEV_NOLEGACY, RIDEV_REMOVE, RIDI_DEVICEINFO, RIDI_DEVICENAME,
    RIDI_PREPARSEDDATA, RIM_TYPEHID, RIM_TYPEKEYBOARD, RIM_TYPEMOUSE, RegisterRawInputDevices,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GIDC_ARRIVAL, GIDC_REMOVAL, GWLP_USERDATA,
    GetMessageW, GetWindowLongPtrW, HWND_MESSAGE, MSG, PostMessageW, PostQuitMessage, RegisterClassExW,
    SetWindowLongPtrW, WM_CLOSE, WM_DESTROY, WM_INPUT, WM_INPUT_DEVICE_CHANGE, WNDCLASSEXW,
};

/// HID collections a seat host forwards.
pub const FORWARDED_HID_USAGES: [(u16, u16); 4] = [(0x0C, 0x01), (0x01, 0x80), (0x0D, 0x04), (0x0D, 0x02)];

/// What the reader reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RawNotice {
    /// An event of the unit with this (normalized) interface path.
    Event { unit: String, event: RawUnitEvent },
    Arrived { unit: String },
    Removed { unit: String },
}

pub type RawCallback = Box<dyn FnMut(RawNotice) + Send>;

#[derive(Debug, Clone)]
pub struct RawReaderConfig {
    /// Normalized interface paths to keep; `None` = every device (console activity
    /// listener, identify by touch).
    pub units: Option<HashSet<String>>,
    pub keyboard_mouse: bool,
    pub hid_usages: Vec<(u16, u16)>,
    pub no_legacy: bool,
}

impl RawReaderConfig {
    /// Seat host: only the seat's units, legacy messages off.
    pub fn seat(interfaces: &[String]) -> Self {
        Self {
            units: Some(interfaces.iter().map(|p| normalize_interface_path(p)).collect()),
            keyboard_mouse: true,
            hid_usages: FORWARDED_HID_USAGES.to_vec(),
            no_legacy: true,
        }
    }
    /// Activity listener: every device, nothing suppressed.
    pub fn listener() -> Self {
        Self { units: None, keyboard_mouse: true, hid_usages: FORWARDED_HID_USAGES.to_vec(), no_legacy: false }
    }
}

type UnitFilter = Arc<Mutex<Option<HashSet<String>>>>;

struct DevInfo {
    path: String,
    hid: Option<HidDevice>,
}

struct State {
    filter: UnitFilter,
    devices: HashMap<usize, Option<DevInfo>>,
    cb: RawCallback,
    buf: Vec<u64>,
}

/// Running reader. Dropping it stops the thread and removes the registrations.
pub struct RawReader {
    hwnd: usize,
    filter: UnitFilter,
    thread: Option<JoinHandle<()>>,
}

impl RawReader {
    pub fn start(cfg: RawReaderConfig, cb: RawCallback) -> BackendResult<Self> {
        let filter: UnitFilter = Arc::new(Mutex::new(cfg.units.clone()));
        let (tx, rx) = mpsc::channel::<BackendResult<usize>>();
        let f2 = filter.clone();
        let thread = std::thread::Builder::new()
            .name("seat-raw-input".into())
            .spawn(move || run(cfg, f2, cb, tx))
            .map_err(|e| BackendError::new(ErrorKind::Internal, "thread_spawn", e.to_string()))?;
        match rx.recv() {
            Ok(Ok(hwnd)) => Ok(Self { hwnd, filter, thread: Some(thread) }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => Err(BackendError::new(ErrorKind::Internal, "raw_input_thread", "raw input thread exited")),
        }
    }

    /// Replace the seat's unit set (after `host_get_assignment` changed).
    pub fn set_units(&self, interfaces: &[String]) {
        let set = interfaces.iter().map(|p| normalize_interface_path(p)).collect();
        *self.filter.lock().unwrap_or_else(|e| e.into_inner()) = Some(set);
        // Cached handles keep their old decision only for their path; re-decide lazily.
        // SAFETY: posting to our own window; harmless if it is already gone.
        unsafe { PostMessageW(self.hwnd as HWND, WM_APP_RESET, 0, 0) };
    }

    pub fn stop(&mut self) {
        if let Some(t) = self.thread.take() {
            // SAFETY: as above.
            unsafe { PostMessageW(self.hwnd as HWND, WM_CLOSE, 0, 0) };
            let _ = t.join();
        }
    }
}

impl Drop for RawReader {
    fn drop(&mut self) {
        self.stop();
    }
}

const WM_APP_RESET: u32 = 0x8000 + 0x51;

fn os_err(reason: &str, what: &str) -> BackendError {
    // SAFETY: no arguments.
    let e = unsafe { GetLastError() };
    BackendError::new(ErrorKind::Os, reason, format!("{what}: Win32 error {e}")).with_win32(e)
}

fn run(cfg: RawReaderConfig, filter: UnitFilter, cb: RawCallback, tx: mpsc::Sender<BackendResult<usize>>) {
    let class = wide("PulsarSeatRawInput");
    // SAFETY: null = this module.
    let hinst = unsafe { GetModuleHandleW(std::ptr::null()) };
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wndproc),
        hInstance: hinst,
        lpszClassName: class.as_ptr(),
        ..Default::default()
    };
    // SAFETY: valid class description; "already exists" (1410) is fine for a second reader.
    if unsafe { RegisterClassExW(&wc) } == 0 && unsafe { GetLastError() } != 1410 {
        let _ = tx.send(Err(os_err("register_class_failed", "RegisterClassExW")));
        return;
    }
    // SAFETY: message-only window of our class.
    let hwnd = unsafe {
        CreateWindowExW(0, class.as_ptr(), std::ptr::null(), 0, 0, 0, 0, 0, HWND_MESSAGE, std::ptr::null_mut(), hinst, std::ptr::null())
    };
    if hwnd.is_null() {
        let _ = tx.send(Err(os_err("create_window_failed", "CreateWindowExW")));
        return;
    }
    let state = Box::into_raw(Box::new(State { filter, devices: HashMap::new(), cb, buf: vec![0u64; 64] }));
    // SAFETY: our window; the pointer lives until WM_DESTROY clears it below.
    unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize) };

    let mut regs = Vec::new();
    let base = RIDEV_INPUTSINK | RIDEV_DEVNOTIFY;
    if cfg.keyboard_mouse {
        let km = if cfg.no_legacy { base | RIDEV_NOLEGACY } else { base };
        regs.push(RAWINPUTDEVICE { usUsagePage: 0x01, usUsage: 0x06, dwFlags: km, hwndTarget: hwnd });
        regs.push(RAWINPUTDEVICE { usUsagePage: 0x01, usUsage: 0x02, dwFlags: km, hwndTarget: hwnd });
    }
    for (page, usage) in &cfg.hid_usages {
        regs.push(RAWINPUTDEVICE { usUsagePage: *page, usUsage: *usage, dwFlags: base, hwndTarget: hwnd });
    }
    // SAFETY: array of `regs.len()` registrations for our window.
    let ok = unsafe { RegisterRawInputDevices(regs.as_ptr(), regs.len() as u32, std::mem::size_of::<RAWINPUTDEVICE>() as u32) };
    if ok == 0 {
        let e = os_err("raw_input_register_failed", "RegisterRawInputDevices");
        // SAFETY: our window.
        unsafe { DestroyWindow(hwnd) };
        let _ = tx.send(Err(e));
        return;
    }
    let _ = tx.send(Ok(hwnd as usize));

    let mut msg = MSG::default();
    // SAFETY: standard message loop on this thread's queue.
    while unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) } > 0 {
        // SAFETY: message from GetMessageW.
        unsafe { DispatchMessageW(&msg) };
    }
    let remove: Vec<RAWINPUTDEVICE> = regs
        .iter()
        .map(|r| RAWINPUTDEVICE { usUsagePage: r.usUsagePage, usUsage: r.usUsage, dwFlags: RIDEV_REMOVE, hwndTarget: std::ptr::null_mut() })
        .collect();
    // SAFETY: removal entries (null target as documented for RIDEV_REMOVE).
    unsafe { RegisterRawInputDevices(remove.as_ptr(), remove.len() as u32, std::mem::size_of::<RAWINPUTDEVICE>() as u32) };
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: GWLP_USERDATA holds our State (or 0 before / after its lifetime).
    let st = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut State;
    match msg {
        WM_INPUT if !st.is_null() => {
            // SAFETY: State is only touched from this window's thread.
            let s = unsafe { &mut *st };
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| on_input(s, lparam as HRAWINPUT)));
            // Cleanup of the raw input buffer (documented requirement).
            // SAFETY: forwarding the original message.
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_INPUT_DEVICE_CHANGE if !st.is_null() => {
            // SAFETY: as above.
            let s = unsafe { &mut *st };
            let h = lparam as usize;
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match wparam as u32 {
                GIDC_ARRIVAL => {
                    s.devices.remove(&h);
                    if let Some(Some(d)) = lookup(s, h).map(|d| d.map(|d| d.path.clone())) {
                        (s.cb)(RawNotice::Arrived { unit: d });
                    }
                }
                GIDC_REMOVAL => {
                    if let Some(Some(d)) = s.devices.remove(&h) {
                        (s.cb)(RawNotice::Removed { unit: d.path });
                    }
                }
                _ => {}
            }));
            0
        }
        WM_APP_RESET if !st.is_null() => {
            // SAFETY: as above.
            unsafe { &mut *st }.devices.clear();
            0
        }
        WM_CLOSE => {
            // SAFETY: our window.
            unsafe { DestroyWindow(hwnd) };
            0
        }
        WM_DESTROY => {
            if !st.is_null() {
                // SAFETY: clear first so no message sees a dangling pointer, then free.
                unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
                drop(unsafe { Box::from_raw(st) });
            }
            // SAFETY: ends this thread's loop.
            unsafe { PostQuitMessage(0) };
            0
        }
        // SAFETY: default handling.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn device_name(h: HANDLE) -> Option<String> {
    let mut chars = 0u32;
    // SAFETY: size query.
    unsafe { GetRawInputDeviceInfoW(h, RIDI_DEVICENAME, std::ptr::null_mut(), &mut chars) };
    if chars == 0 || chars > 4096 {
        return None;
    }
    let mut buf = vec![0u16; chars as usize];
    // SAFETY: buffer of `chars` UTF-16 units.
    let n = unsafe { GetRawInputDeviceInfoW(h, RIDI_DEVICENAME, buf.as_mut_ptr().cast(), &mut chars) };
    if n == u32::MAX || n == 0 {
        return None;
    }
    let end = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..end]))
}

fn device_type(h: HANDLE) -> Option<u32> {
    let mut info = RID_DEVICE_INFO { cbSize: std::mem::size_of::<RID_DEVICE_INFO>() as u32, ..Default::default() };
    let mut size = info.cbSize;
    // SAFETY: out struct of `size` bytes.
    let n = unsafe { GetRawInputDeviceInfoW(h, RIDI_DEVICEINFO, (&mut info as *mut RID_DEVICE_INFO).cast(), &mut size) };
    (n != u32::MAX && n != 0).then_some(info.dwType)
}

fn preparsed(h: HANDLE) -> Option<Vec<u8>> {
    let mut size = 0u32;
    // SAFETY: size query.
    unsafe { GetRawInputDeviceInfoW(h, RIDI_PREPARSEDDATA, std::ptr::null_mut(), &mut size) };
    if size == 0 || size > 1 << 20 {
        return None;
    }
    let mut buf = vec![0u8; size as usize];
    // SAFETY: buffer of `size` bytes.
    let n = unsafe { GetRawInputDeviceInfoW(h, RIDI_PREPARSEDDATA, buf.as_mut_ptr().cast(), &mut size) };
    (n != u32::MAX && n != 0).then_some(buf)
}

fn lookup(s: &mut State, h: usize) -> Option<Option<&DevInfo>> {
    if !s.devices.contains_key(&h) {
        let info = device_name(h as HANDLE).and_then(|name| {
            let path = normalize_interface_path(&name);
            let keep = s.filter.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_none_or(|set| set.contains(&path));
            if !keep {
                return None;
            }
            let hid = if device_type(h as HANDLE) == Some(RIM_TYPEHID) {
                preparsed(h as HANDLE).and_then(|b| HidDevice::new(&b))
            } else {
                None
            };
            Some(DevInfo { path, hid })
        });
        s.devices.insert(h, info);
    }
    s.devices.get(&h).map(|d| d.as_ref())
}

fn on_input(s: &mut State, hri: HRAWINPUT) {
    let hdr = std::mem::size_of::<RAWINPUTHEADER>() as u32;
    let mut size = 0u32;
    // SAFETY: size query for this WM_INPUT.
    unsafe { GetRawInputData(hri, RID_INPUT, std::ptr::null_mut(), &mut size, hdr) };
    if size == 0 || size > 1 << 20 {
        return;
    }
    let words = (size as usize).div_ceil(8);
    if s.buf.len() < words {
        s.buf.resize(words, 0);
    }
    // SAFETY: buffer of at least `size` bytes, 8-byte aligned (RAWINPUT alignment).
    let n = unsafe { GetRawInputData(hri, RID_INPUT, s.buf.as_mut_ptr().cast(), &mut size, hdr) };
    if n == u32::MAX || (n as usize) < std::mem::size_of::<RAWINPUTHEADER>() {
        return;
    }
    // SAFETY: the buffer now holds one RAWINPUT of `n` bytes.
    let ri = unsafe { &*(s.buf.as_ptr() as *const RAWINPUT) };
    let h = ri.header.hDevice as usize;
    let base = s.buf.as_ptr() as usize;
    if h == 0 {
        return; // injected input
    }
    let kind = ri.header.dwType;
    let mut events = Vec::new();
    let unit;
    {
        let Some(Some(dev)) = lookup(s, h) else { return };
        unit = dev.path.clone();
        match kind {
            RIM_TYPEKEYBOARD => {
                // SAFETY: dwType says keyboard.
                let k = unsafe { ri.data.keyboard };
                let e0 = k.Flags & 2 != 0;
                // Skip overrun markers and the fake shifts some keys emit with E0.
                if k.MakeCode != 0xFF && !(e0 && (k.MakeCode == 0x2A || k.MakeCode == 0x36)) {
                    events.push(RawUnitEvent::Key { sc: k.MakeCode, flags: k.Flags & 7 });
                }
            }
            RIM_TYPEMOUSE => {
                // SAFETY: dwType says mouse; the button union is read as its documented pair.
                let m = unsafe { ri.data.mouse };
                let b = unsafe { m.Anonymous.Anonymous };
                events.push(RawUnitEvent::Mouse {
                    flags: m.usFlags,
                    buttons: b.usButtonFlags,
                    data: b.usButtonData,
                    x: m.lLastX,
                    y: m.lLastY,
                });
            }
            RIM_TYPEHID => {
                if let Some(hid) = &dev.hid {
                    // SAFETY: dwType says HID; the reports follow `RAWHID` in the buffer.
                    let rh = unsafe { &ri.data.hid };
                    let per = rh.dwSizeHid as usize;
                    let count = rh.dwCount as usize;
                    let start = (std::ptr::addr_of!(rh.bRawData) as usize) - base;
                    let avail = (n as usize).saturating_sub(start);
                    if per > 0 && per.saturating_mul(count) <= avail {
                        // SAFETY: range checked against the bytes GetRawInputData wrote.
                        let bytes = unsafe { std::slice::from_raw_parts(std::ptr::addr_of!(rh.bRawData).cast::<u8>(), per * count) };
                        for r in bytes.chunks_exact(per) {
                            if let Some(e) = hid.parse(r) {
                                events.push(e);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    for event in events {
        (s.cb)(RawNotice::Event { unit: unit.clone(), event });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seat_config_normalizes_paths() {
        let c = RawReaderConfig::seat(&["\\??\\HID#X#{Y}".into()]);
        assert!(c.units.unwrap().contains("\\\\?\\hid#x#{y}"));
        assert!(RawReaderConfig::listener().units.is_none());
    }

    /// Starts and stops a real reader (no input needed): window, registration, teardown.
    #[test]
    fn reader_starts_and_stops() {
        let mut r = match RawReader::start(RawReaderConfig::seat(&[]), Box::new(|_| {})) {
            Ok(r) => r,
            // Non-interactive test runners (services, CI without a desktop) cannot register.
            Err(e) if e.reason == "raw_input_register_failed" || e.reason == "create_window_failed" => return,
            Err(e) => panic!("{e}"),
        };
        r.set_units(&["\\\\?\\hid#a".into()]);
        r.stop();
    }
}
