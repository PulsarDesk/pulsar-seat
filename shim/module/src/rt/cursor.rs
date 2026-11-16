// SPDX-License-Identifier: GPL-3.0-only
//! The seat's own cursor for processes of a kiosk seat (parity 2026-11-17;
//! `seat_shim_core::kiosk_cursor`). `GetCursorPos`, `GetPhysicalCursorPos`,
//! `GetCursorInfo` (its `ptScreenPos`) and `GetMessagePos` answer with the position the
//! seat's kiosk shell publishes in `Local\PulsarSeat.KioskCursor.<NN>`, so a popup menu an
//! app opens at "the cursor" (`TrackPopupMenu(Ex)` with `GetCursorPos`), tooltips and
//! drag code follow the seat's mouse instead of the console's one system cursor.
//!
//! The real call runs first; its answer stands whenever the section is missing, being
//! written, or the shell has gone (alive bit clear). `SetCursorPos` is not hooked (a seat
//! app moving "the" cursor still moves the console's system cursor; known limit).
//! HYPOTHESIS: never run on hardware.

use super::hook::{Hook, guarded, install as hook};
use seat_shim_core::decide::Role;
use seat_shim_core::kiosk_cursor::{SECTION_SIZE, read_stable, section_name};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use windows_sys::Win32::Foundation::{POINT, TRUE};
use windows_sys::Win32::UI::WindowsAndMessaging::CURSORINFO;
use windows_sys::core::BOOL;

static CURSOR_POS: Hook = Hook::new();
static PHYSICAL_POS: Hook = Hook::new();
static CURSOR_INFO: Hook = Hook::new();
static MESSAGE_POS: Hook = Hook::new();

/// Mapped view of the seat's section (0 = not open yet).
static VIEW: AtomicUsize = AtomicUsize::new(0);
/// Tick of the last failed open (retried at most once a second).
static LAST_TRY: AtomicU64 = AtomicU64::new(0);

type PosFn = unsafe extern "system" fn(*mut POINT) -> BOOL;
type InfoFn = unsafe extern "system" fn(*mut CURSORINFO) -> BOOL;
type MsgPosFn = unsafe extern "system" fn() -> u32;

fn seat() -> Option<u32> {
    let (role, plan, _, _) = super::live()?;
    match role {
        Role::Kiosk { seat, .. } if plan.cursor_pos => Some(seat),
        _ => None,
    }
}

fn view(seat: u32) -> Option<*const u8> {
    use windows_sys::Win32::System::Memory::{FILE_MAP_READ, MapViewOfFile, OpenFileMappingW};
    use windows_sys::Win32::System::SystemInformation::GetTickCount64;
    let v = VIEW.load(Ordering::Acquire);
    if v != 0 {
        return Some(v as *const u8);
    }
    // SAFETY: plain call.
    let now = unsafe { GetTickCount64() };
    let last = LAST_TRY.load(Ordering::Relaxed);
    if last != 0 && now.saturating_sub(last) < 1000 {
        return None;
    }
    LAST_TRY.store(now.max(1), Ordering::Relaxed);
    let name: Vec<u16> = section_name(seat).encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: NUL-terminated name; read-only view of a section the shell sized.
    unsafe {
        let h = OpenFileMappingW(FILE_MAP_READ, 0, name.as_ptr());
        if h.is_null() {
            return None;
        }
        let p = MapViewOfFile(h, FILE_MAP_READ, 0, 0, SECTION_SIZE);
        // The view keeps the section alive.
        windows_sys::Win32::Foundation::CloseHandle(h);
        if p.Value.is_null() {
            return None;
        }
        match VIEW.compare_exchange(0, p.Value as usize, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => Some(p.Value as *const u8),
            Err(other) => {
                windows_sys::Win32::System::Memory::UnmapViewOfFile(p);
                Some(other as *const u8)
            }
        }
    }
}

/// The seat cursor, if this process is a kiosk process and the shell publishes one.
fn seat_cursor() -> Option<(i32, i32)> {
    guarded(|| {
        let p = view(seat()?)?;
        read_stable(|| {
            let mut b = [0u8; SECTION_SIZE];
            for (i, x) in b.iter_mut().enumerate() {
                // SAFETY: the view is SECTION_SIZE bytes; volatile because the shell
                // writes it from another process.
                *x = unsafe { std::ptr::read_volatile(p.add(i)) };
            }
            b
        })
    })
    .flatten()
}

unsafe extern "system" fn get_cursor_pos(pt: *mut POINT) -> BOOL {
    // SAFETY: trampoline has GetCursorPos's signature.
    let r = unsafe {
        let f: PosFn = std::mem::transmute(CURSOR_POS.trampoline());
        f(pt)
    };
    if !pt.is_null()
        && let Some((x, y)) = seat_cursor()
    {
        // SAFETY: caller's POINT (the real call accepted it).
        unsafe { *pt = POINT { x, y } };
        return TRUE;
    }
    r
}

unsafe extern "system" fn get_physical_cursor_pos(pt: *mut POINT) -> BOOL {
    // SAFETY: trampoline has GetPhysicalCursorPos's signature.
    let r = unsafe {
        let f: PosFn = std::mem::transmute(PHYSICAL_POS.trampoline());
        f(pt)
    };
    if !pt.is_null()
        && let Some((x, y)) = seat_cursor()
    {
        // SAFETY: as above.
        unsafe { *pt = POINT { x, y } };
        return TRUE;
    }
    r
}

unsafe extern "system" fn get_cursor_info(ci: *mut CURSORINFO) -> BOOL {
    // SAFETY: trampoline has GetCursorInfo's signature.
    let r = unsafe {
        let f: InfoFn = std::mem::transmute(CURSOR_INFO.trampoline());
        f(ci)
    };
    if r != 0
        && !ci.is_null()
        && let Some((x, y)) = seat_cursor()
    {
        // SAFETY: the real call filled the caller's struct.
        unsafe { (*ci).ptScreenPos = POINT { x, y } };
    }
    r
}

unsafe extern "system" fn get_message_pos() -> u32 {
    // SAFETY: trampoline has GetMessagePos's signature.
    let r = unsafe {
        let f: MsgPosFn = std::mem::transmute(MESSAGE_POS.trampoline());
        f()
    };
    match seat_cursor() {
        // Posted messages carry the real cursor in MSG.pt; the seat's is what the app
        // means (MAKELONG of the signed 16-bit coordinates).
        Some((x, y)) => u32::from(x as i16 as u16) | (u32::from(y as i16 as u16) << 16),
        None => r,
    }
}

/// # Safety
/// Init path only.
pub unsafe fn install() -> bool {
    // SAFETY: detours match the targets' signatures.
    unsafe {
        let a = hook(&CURSOR_POS, "user32.dll", "GetCursorPos", get_cursor_pos as *const ());
        let b = hook(&PHYSICAL_POS, "user32.dll", "GetPhysicalCursorPos", get_physical_cursor_pos as *const ());
        let c = hook(&CURSOR_INFO, "user32.dll", "GetCursorInfo", get_cursor_info as *const ());
        let d = hook(&MESSAGE_POS, "user32.dll", "GetMessagePos", get_message_pos as *const ());
        a && b && c && d
    }
}
