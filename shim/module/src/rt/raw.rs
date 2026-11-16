// SPDX-License-Identifier: GPL-3.0-only
//! Raw-input data filter for processes of a local kiosk seat (round 4, kiosk-isolation;
//! `seat_shim_core::kiosk`). Every kiosk seat shares the console session, so Windows
//! delivers the raw input of every keyboard, mouse and HID pad of the PC to whichever
//! window registered for it. `GetRawInputData` / `GetRawInputBuffer` here drop the records
//! of devices that the kiosk map gives to another seat (or that no kiosk seat owns):
//!
//! - `GetRawInputData` with a header or input buffer: a denied device's record makes the
//!   call fail (`(UINT)-1`, `ERROR_INVALID_HANDLE`), as for a stale `HRAWINPUT`;
//! - `GetRawInputBuffer` (64-bit processes only): denied records are removed from the
//!   buffer and the count shrinks. In 32-bit processes the call is passed through: under
//!   WOW64 the buffer layout is reported to differ from the 32-bit `RAWINPUT` structure
//!   (HYPOTHESIS, not checked here), and a wrong guess would corrupt the app's input
//!   (known limit).
//!
//! Records without a device (`hDevice` null: injected input) always pass. HYPOTHESIS:
//! never run on hardware.

use super::device::{Check, decide};
use super::hid::raw_device_name;
use super::hook::{Hook, guarded, install as hook};
use seat_shim_core::decide::Access;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Mutex;
use windows_sys::Win32::Foundation::{ERROR_INVALID_HANDLE, HANDLE, SetLastError};
use windows_sys::Win32::UI::Input::{HRAWINPUT, RAWINPUTHEADER};

static RAW_DATA: Hook = Hook::new();
#[cfg(target_pointer_width = "64")]
static RAW_BUFFER: Hook = Hook::new();

type RawDataFn = unsafe extern "system" fn(HRAWINPUT, u32, *mut c_void, *mut u32, u32) -> u32;
#[cfg(target_pointer_width = "64")]
type RawBufferFn = unsafe extern "system" fn(*mut c_void, *mut u32, u32) -> u32;

/// Device handle -> interface name. Raw-input handles stay valid while the device is
/// present; the cache is dropped when it grows (handles of removed devices).
static NAMES: Mutex<Option<HashMap<usize, Option<String>>>> = Mutex::new(None);

fn name_of(h: usize) -> Option<String> {
    if let Some(n) = NAMES.lock().ok().and_then(|g| g.as_ref().and_then(|m| m.get(&h).cloned())) {
        return n;
    }
    let n = raw_device_name(h as HANDLE);
    if let Ok(mut g) = NAMES.lock() {
        let m = g.get_or_insert_with(HashMap::new);
        if m.len() > 256 {
            m.clear();
        }
        m.insert(h, n.clone());
    }
    n
}

/// Whether the record of this device is dropped for the current role.
fn denied(device: usize) -> bool {
    if device == 0 {
        return false;
    }
    guarded(|| name_of(device).and_then(|p| decide(&p, Check::RawData))).flatten() == Some(Access::Deny)
}

fn active() -> bool {
    super::live().is_some_and(|(_, p, _, _)| p.raw_input_data)
}

unsafe extern "system" fn get_raw_input_data(h: HRAWINPUT, cmd: u32, data: *mut c_void, size: *mut u32, header: u32) -> u32 {
    // SAFETY: trampoline has GetRawInputData's signature.
    let r = unsafe {
        let f: RawDataFn = std::mem::transmute(RAW_DATA.trampoline());
        f(h, cmd, data, size, header)
    };
    if data.is_null() || r == u32::MAX || (r as usize) < size_of::<RAWINPUTHEADER>() || !active() {
        return r;
    }
    // SAFETY: on success with a buffer, both RID_HEADER and RID_INPUT start with a
    // RAWINPUTHEADER of `r >= sizeof` bytes; the caller's buffer may be unaligned.
    let device = unsafe { std::ptr::read_unaligned(data as *const RAWINPUTHEADER) }.hDevice as usize;
    if denied(device) {
        // SAFETY: plain Win32 call.
        unsafe { SetLastError(ERROR_INVALID_HANDLE) };
        return u32::MAX;
    }
    r
}

#[cfg(target_pointer_width = "64")]
unsafe extern "system" fn get_raw_input_buffer(data: *mut c_void, size: *mut u32, header: u32) -> u32 {
    // SAFETY: trampoline has GetRawInputBuffer's signature.
    let n = unsafe {
        let f: RawBufferFn = std::mem::transmute(RAW_BUFFER.trampoline());
        f(data, size, header)
    };
    if data.is_null() || size.is_null() || n == 0 || n == u32::MAX || header as usize != size_of::<RAWINPUTHEADER>() || !active()
    {
        return n;
    }
    // SAFETY: the caller's buffer holds `*size` bytes (its size on input; Windows writes at
    // most that many) and now `n` records.
    let buf = unsafe { std::slice::from_raw_parts_mut(data as *mut u8, *size as usize) };
    match crate::filter::compact_raw_records(buf, n as usize, crate::filter::RawLayout::NATIVE, |d| !denied(d)) {
        Some(kept) => kept as u32,
        None => n,
    }
}

static RAW_REGISTER: Hook = Hook::new();
type RegisterFn = unsafe extern "system" fn(*const windows_sys::Win32::UI::Input::RAWINPUTDEVICE, u32, u32) -> windows_sys::core::BOOL;

/// Kiosk role only (parity 2026-11-17): registrations get `RIDEV_INPUTSINK`
/// (`seat_shim_core::kiosk_cursor::kiosk_raw_flags`), so a game of a kiosk seat, which is
/// never the foreground window, still gets its seat's raw input. Seat-1 apps are unchanged.
unsafe extern "system" fn register_raw_input_devices(
    devs: *const windows_sys::Win32::UI::Input::RAWINPUTDEVICE,
    n: u32,
    size: u32,
) -> windows_sys::core::BOOL {
    use windows_sys::Win32::UI::Input::RAWINPUTDEVICE;
    // SAFETY: trampoline has RegisterRawInputDevices's signature.
    let f: RegisterFn = unsafe { std::mem::transmute(RAW_REGISTER.trampoline()) };
    let kiosk = super::live().is_some_and(|(role, p, _, _)| p.raw_input_data && matches!(role, seat_shim_core::decide::Role::Kiosk { .. }));
    if !kiosk || devs.is_null() || n == 0 || n > 64 || size as usize != size_of::<RAWINPUTDEVICE>() {
        // SAFETY: the caller's arguments, unchanged.
        return unsafe { f(devs, n, size) };
    }
    // SAFETY: the caller passed `n` records of this size.
    let mut copy: Vec<RAWINPUTDEVICE> = unsafe { std::slice::from_raw_parts(devs, n as usize) }.to_vec();
    for d in copy.iter_mut() {
        d.dwFlags = seat_shim_core::kiosk_cursor::kiosk_raw_flags(d.dwFlags, !d.hwndTarget.is_null());
    }
    // SAFETY: our copy, same count and size.
    let r = unsafe { f(copy.as_ptr(), n, size) };
    if r == 0 {
        // Refused with the added flag (e.g. a window of another thread): the original call.
        // SAFETY: the caller's arguments, unchanged.
        return unsafe { f(devs, n, size) };
    }
    r
}

/// # Safety
/// Init path only.
pub unsafe fn install() -> bool {
    // SAFETY: detours match the targets' signatures.
    unsafe {
        // Best effort: the background registration is not needed for the data filter.
        let _ = hook(&RAW_REGISTER, "user32.dll", "RegisterRawInputDevices", register_raw_input_devices as *const ());
        let data = hook(&RAW_DATA, "user32.dll", "GetRawInputData", get_raw_input_data as *const ());
        #[cfg(target_pointer_width = "64")]
        let data = data & hook(&RAW_BUFFER, "user32.dll", "GetRawInputBuffer", get_raw_input_buffer as *const ());
        data
    }
}
