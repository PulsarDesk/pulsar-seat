// SPDX-License-Identifier: GPL-3.0-only
//! The seat's own cursor and key state for processes of a kiosk seat (parity 2026-11-17,
//! extended by the parity review; `seat_shim_core::kiosk_cursor`, layout v2).
//!
//! - `GetCursorPos`, `GetPhysicalCursorPos`, `GetCursorInfo` (its `ptScreenPos`) and
//!   `GetMessagePos` answer with the position the seat's kiosk shell publishes in
//!   `Local\PulsarSeat.KioskCursor.<NN>`, so a popup menu an app opens at "the cursor"
//!   (`TrackPopupMenu(Ex)` with `GetCursorPos`), tooltips and drag code follow the seat's
//!   mouse instead of the console's one system cursor. The section holds physical pixels;
//!   the logical functions convert them for threads that are not per-monitor DPI aware
//!   (`PhysicalToLogicalPointForPerMonitorDPI` on a window of this thread), like Windows'
//!   own DPI virtualisation does. `GetPhysicalCursorPos` gets the raw value.
//! - `SetCursorPos` / `SetPhysicalCursorPos` and `ClipCursor` move / confine the **seat**
//!   cursor: the request is written into the section and the shell's thread is woken; the
//!   console's system cursor is not touched. `GetClipCursor` reports the seat's clip.
//! - `GetAsyncKeyState`, `GetKeyState` and `GetKeyboardState` answer from the seat's own
//!   keyboard / mouse-button state the shell publishes, not the console's.
//!
//! The real call runs first for every getter; its answer stands whenever the real call
//! failed, the section is missing, being written, or the shell has gone (alive bit clear).
//! HYPOTHESIS: never run on hardware (DPI conversion with a DPI-unaware window, cross-
//! process `PostThreadMessageW` to the shell, games that warp the cursor every frame).

use super::hook::{Hook, guarded, install as hook};
use seat_shim_core::decide::Role;
use seat_shim_core::kiosk_cursor::{self as kc, SECTION_SIZE, SHELL_SIZE, SeatCursor, read_stable, section_name};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use windows_sys::Win32::Foundation::{HWND, LPARAM, POINT, RECT, TRUE};
use windows_sys::Win32::UI::WindowsAndMessaging::CURSORINFO;
use windows_sys::core::BOOL;

static CURSOR_POS: Hook = Hook::new();
static PHYSICAL_POS: Hook = Hook::new();
static CURSOR_INFO: Hook = Hook::new();
static MESSAGE_POS: Hook = Hook::new();
static SET_POS: Hook = Hook::new();
static SET_PHYSICAL_POS: Hook = Hook::new();
static CLIP: Hook = Hook::new();
static GET_CLIP: Hook = Hook::new();
static ASYNC_KEY: Hook = Hook::new();
static KEY_STATE: Hook = Hook::new();
static KEYBOARD_STATE: Hook = Hook::new();

/// Mapped view of the seat's section (0 = not open yet).
static VIEW: AtomicUsize = AtomicUsize::new(0);
/// The view is writable (requests possible).
static WRITABLE: AtomicBool = AtomicBool::new(false);
/// Tick of the last failed open (retried at most once a second).
static LAST_TRY: AtomicU64 = AtomicU64::new(0);

type PosFn = unsafe extern "system" fn(*mut POINT) -> BOOL;
type InfoFn = unsafe extern "system" fn(*mut CURSORINFO) -> BOOL;
type MsgPosFn = unsafe extern "system" fn() -> u32;
type SetPosFn = unsafe extern "system" fn(i32, i32) -> BOOL;
type ClipFn = unsafe extern "system" fn(*const RECT) -> BOOL;
type GetClipFn = unsafe extern "system" fn(*mut RECT) -> BOOL;
type KeyFn = unsafe extern "system" fn(i32) -> i16;
type KbdFn = unsafe extern "system" fn(*mut u8) -> BOOL;

/// Seat number when this is a kiosk process with the seat features on.
pub(super) fn seat() -> Option<u32> {
    let (role, plan, _, _) = super::live()?;
    match role {
        Role::Kiosk { seat, .. } if plan.cursor_pos => Some(seat),
        _ => None,
    }
}

fn view(seat: u32) -> Option<*mut u8> {
    use windows_sys::Win32::System::Memory::{FILE_MAP_READ, FILE_MAP_WRITE, MapViewOfFile, OpenFileMappingW};
    use windows_sys::Win32::System::SystemInformation::GetTickCount64;
    let v = VIEW.load(Ordering::Acquire);
    if v != 0 {
        return Some(v as *mut u8);
    }
    // SAFETY: plain call.
    let now = unsafe { GetTickCount64() };
    let last = LAST_TRY.load(Ordering::Relaxed);
    if last != 0 && now.saturating_sub(last) < 1000 {
        return None;
    }
    LAST_TRY.store(now.max(1), Ordering::Relaxed);
    let name: Vec<u16> = section_name(seat).encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: NUL-terminated name; view of a section the shell sized.
    unsafe {
        let mut access = FILE_MAP_READ | FILE_MAP_WRITE;
        let mut h = OpenFileMappingW(access, 0, name.as_ptr());
        if h.is_null() {
            access = FILE_MAP_READ;
            h = OpenFileMappingW(access, 0, name.as_ptr());
        }
        if h.is_null() {
            return None;
        }
        let p = MapViewOfFile(h, access, 0, 0, SECTION_SIZE);
        // The view keeps the section alive.
        windows_sys::Win32::Foundation::CloseHandle(h);
        if p.Value.is_null() {
            return None;
        }
        match VIEW.compare_exchange(0, p.Value as usize, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => {
                WRITABLE.store(access & FILE_MAP_WRITE != 0, Ordering::Release);
                Some(p.Value as *mut u8)
            }
            Err(other) => {
                windows_sys::Win32::System::Memory::UnmapViewOfFile(p);
                Some(other as *mut u8)
            }
        }
    }
}

/// The seat's published state, if this process is a kiosk process and the shell is live.
fn seat_state() -> Option<SeatCursor> {
    guarded(|| {
        let p = view(seat()?)?;
        read_stable(|| {
            let mut b = [0u8; SHELL_SIZE];
            for (i, x) in b.iter_mut().enumerate() {
                // SAFETY: the view is SECTION_SIZE >= SHELL_SIZE bytes; volatile because
                // the shell writes it from another process.
                *x = unsafe { std::ptr::read_volatile(p.add(i)) };
            }
            b
        })
    })
    .flatten()
}

/// The calling thread wants logical (DPI-virtualised) coordinates.
fn thread_wants_logical() -> bool {
    use windows_sys::Win32::UI::HiDpi::{GetAwarenessFromDpiAwarenessContext, GetThreadDpiAwarenessContext};
    // SAFETY: plain calls on the current thread.
    let a = unsafe { GetAwarenessFromDpiAwarenessContext(GetThreadDpiAwarenessContext()) };
    kc::wants_logical(a)
}

/// A window of the calling thread (for the per-monitor DPI conversion), else one of this
/// process under the point.
fn conversion_window(at: POINT) -> HWND {
    use windows_sys::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
    use windows_sys::Win32::UI::WindowsAndMessaging::{EnumThreadWindows, GetWindowThreadProcessId, WindowFromPhysicalPoint};
    unsafe extern "system" fn first(h: HWND, out: LPARAM) -> BOOL {
        // SAFETY: `out` points at the caller's HWND slot.
        unsafe { *(out as *mut HWND) = h };
        0
    }
    let mut found: HWND = std::ptr::null_mut();
    // SAFETY: callback writes one HWND into `found`, which outlives the call.
    unsafe { EnumThreadWindows(GetCurrentThreadId(), Some(first), &mut found as *mut HWND as LPARAM) };
    if !found.is_null() {
        return found;
    }
    // SAFETY: plain queries.
    unsafe {
        let h = WindowFromPhysicalPoint(at);
        let mut pid = 0u32;
        if !h.is_null() && GetWindowThreadProcessId(h, &mut pid) != 0 && pid == GetCurrentProcessId() {
            return h;
        }
    }
    std::ptr::null_mut()
}

/// Physical seat point in the caller's coordinate space.
fn for_caller(x: i32, y: i32) -> POINT {
    use windows_sys::Win32::UI::HiDpi::PhysicalToLogicalPointForPerMonitorDPI;
    let mut pt = POINT { x, y };
    if thread_wants_logical() {
        let h = conversion_window(pt);
        if !h.is_null() {
            let mut q = pt;
            // SAFETY: valid window and point.
            if unsafe { PhysicalToLogicalPointForPerMonitorDPI(h, &mut q) } != 0 {
                pt = q;
            }
        }
    }
    pt
}

/// Caller's point (logical for DPI-unaware threads) in physical pixels.
fn from_caller(x: i32, y: i32, physical: bool) -> (i32, i32) {
    use windows_sys::Win32::UI::HiDpi::LogicalToPhysicalPointForPerMonitorDPI;
    let mut pt = POINT { x, y };
    if !physical && thread_wants_logical() {
        let h = conversion_window(pt);
        if !h.is_null() {
            let mut q = pt;
            // SAFETY: valid window and point.
            if unsafe { LogicalToPhysicalPointForPerMonitorDPI(h, &mut q) } != 0 {
                pt = q;
            }
        }
    }
    (pt.x, pt.y)
}

fn u32_at(p: *mut u8, at: usize) -> &'static AtomicU32 {
    // SAFETY: 4-byte aligned offset inside the page-aligned SECTION_SIZE view, which stays
    // mapped for the life of the process.
    unsafe { &*(p.add(at) as *const AtomicU32) }
}

/// Writes one request under the section's request lock and wakes the shell. `false` when
/// this process has no writable view (the real call then runs).
fn request(write: impl FnOnce(*mut u8)) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::PostThreadMessageW;
    guarded(|| {
        let p = view(seat()?)?;
        if !WRITABLE.load(Ordering::Acquire) {
            return None;
        }
        let lock = u32_at(p, kc::REQ_LOCK);
        let mut spins = 0u32;
        while lock.compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed).is_err() {
            spins += 1;
            if spins > 10_000 {
                break;
            }
            std::hint::spin_loop();
        }
        write(p);
        lock.store(0, Ordering::Release);
        let tid = u32_at(p, kc::SHELL_THREAD).load(Ordering::Acquire);
        if tid != 0 {
            // SAFETY: plain post; a gone thread just fails.
            unsafe { PostThreadMessageW(tid, kc::WAKE_MESSAGE, 0, 0) };
        }
        Some(())
    })
    .flatten()
    .is_some()
}

unsafe extern "system" fn get_cursor_pos(pt: *mut POINT) -> BOOL {
    // SAFETY: trampoline has GetCursorPos's signature.
    let r = unsafe {
        let f: PosFn = std::mem::transmute(CURSOR_POS.trampoline());
        f(pt)
    };
    // Only when the real call succeeded: it validated the caller's pointer.
    if r != 0
        && !pt.is_null()
        && let Some(s) = seat_state()
    {
        // SAFETY: caller's POINT, accepted by the real call.
        unsafe { *pt = for_caller(s.x, s.y) };
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
    if r != 0
        && !pt.is_null()
        && let Some(s) = seat_state()
    {
        // SAFETY: as above (physical pixels as published).
        unsafe { *pt = POINT { x: s.x, y: s.y } };
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
        && let Some(s) = seat_state()
    {
        // SAFETY: the real call filled the caller's struct.
        unsafe { (*ci).ptScreenPos = for_caller(s.x, s.y) };
    }
    r
}

unsafe extern "system" fn get_message_pos() -> u32 {
    // SAFETY: trampoline has GetMessagePos's signature.
    let r = unsafe {
        let f: MsgPosFn = std::mem::transmute(MESSAGE_POS.trampoline());
        f()
    };
    match seat_state() {
        // Posted messages carry the real cursor in MSG.pt; the seat's is what the app
        // means (MAKELONG of the signed 16-bit coordinates, caller's space).
        Some(s) => {
            let p = for_caller(s.x, s.y);
            u32::from(p.x as i16 as u16) | (u32::from(p.y as i16 as u16) << 16)
        }
        None => r,
    }
}

fn set_pos(x: i32, y: i32, physical: bool) -> bool {
    let (px, py) = from_caller(x, y, physical);
    request(|p| {
        u32_at(p, kc::REQ_SET_X).store(px as u32, Ordering::Relaxed);
        u32_at(p, kc::REQ_SET_Y).store(py as u32, Ordering::Relaxed);
        u32_at(p, kc::REQ_SET_SEQ).fetch_add(1, Ordering::Release);
    })
}

unsafe extern "system" fn set_cursor_pos(x: i32, y: i32) -> BOOL {
    if seat_state().is_some() && set_pos(x, y, false) {
        return TRUE;
    }
    // SAFETY: trampoline has SetCursorPos's signature.
    unsafe {
        let f: SetPosFn = std::mem::transmute(SET_POS.trampoline());
        f(x, y)
    }
}

unsafe extern "system" fn set_physical_cursor_pos(x: i32, y: i32) -> BOOL {
    if seat_state().is_some() && set_pos(x, y, true) {
        return TRUE;
    }
    // SAFETY: trampoline has SetPhysicalCursorPos's signature.
    unsafe {
        let f: SetPosFn = std::mem::transmute(SET_PHYSICAL_POS.trampoline());
        f(x, y)
    }
}

unsafe extern "system" fn clip_cursor(rc: *const RECT) -> BOOL {
    if seat_state().is_some() {
        // SAFETY: guarded read of the caller's rectangle (null = free the cursor).
        let r = guarded(|| if rc.is_null() { None } else { Some(unsafe { *rc }) });
        if let Some(r) = r {
            let (l, t) = r.map_or((0, 0), |r| from_caller(r.left, r.top, false));
            let (rr, b) = r.map_or((0, 0), |r| from_caller(r.right, r.bottom, false));
            let on = r.is_some();
            if request(|p| {
                u32_at(p, kc::REQ_CLIP_ON).store(u32::from(on), Ordering::Relaxed);
                for (i, v) in [l, t, rr, b].into_iter().enumerate() {
                    u32_at(p, kc::REQ_CLIP_RECT + 4 * i).store(v as u32, Ordering::Relaxed);
                }
                u32_at(p, kc::REQ_CLIP_SEQ).fetch_add(1, Ordering::Release);
            }) {
                return TRUE;
            }
        }
    }
    // SAFETY: trampoline has ClipCursor's signature.
    unsafe {
        let f: ClipFn = std::mem::transmute(CLIP.trampoline());
        f(rc)
    }
}

unsafe extern "system" fn get_clip_cursor(rc: *mut RECT) -> BOOL {
    // SAFETY: trampoline has GetClipCursor's signature.
    let r = unsafe {
        let f: GetClipFn = std::mem::transmute(GET_CLIP.trampoline());
        f(rc)
    };
    if r != 0
        && !rc.is_null()
        && let Some(SeatCursor { clip: Some((l, t, rr, b)), .. }) = seat_state()
    {
        let a = for_caller(l, t);
        let z = for_caller(rr, b);
        // SAFETY: the real call filled the caller's rectangle.
        unsafe { *rc = RECT { left: a.x, top: a.y, right: z.x, bottom: z.y } };
    }
    r
}

unsafe extern "system" fn get_async_key_state(vk: i32) -> i16 {
    // SAFETY: trampoline has GetAsyncKeyState's signature.
    let r = unsafe {
        let f: KeyFn = std::mem::transmute(ASYNC_KEY.trampoline());
        f(vk)
    };
    match seat_state() {
        Some(s) => kc::async_key_state(&s.keys, vk),
        None => r,
    }
}

unsafe extern "system" fn get_key_state(vk: i32) -> i16 {
    // SAFETY: trampoline has GetKeyState's signature.
    let r = unsafe {
        let f: KeyFn = std::mem::transmute(KEY_STATE.trampoline());
        f(vk)
    };
    match seat_state() {
        Some(s) => kc::key_state(&s.keys, vk),
        None => r,
    }
}

unsafe extern "system" fn get_keyboard_state(buf: *mut u8) -> BOOL {
    // SAFETY: trampoline has GetKeyboardState's signature.
    let r = unsafe {
        let f: KbdFn = std::mem::transmute(KEYBOARD_STATE.trampoline());
        f(buf)
    };
    if r != 0
        && !buf.is_null()
        && let Some(s) = seat_state()
    {
        // SAFETY: the real call wrote 256 bytes into the caller's buffer.
        unsafe { std::ptr::copy_nonoverlapping(s.keys.as_ptr(), buf, 256) };
    }
    r
}

/// # Safety
/// Init path only.
pub unsafe fn install() -> bool {
    // SAFETY: detours match the targets' signatures.
    unsafe {
        let hooks: [(&'static Hook, &'static str, *const ()); 11] = [
            (&CURSOR_POS, "GetCursorPos", get_cursor_pos as *const ()),
            (&PHYSICAL_POS, "GetPhysicalCursorPos", get_physical_cursor_pos as *const ()),
            (&CURSOR_INFO, "GetCursorInfo", get_cursor_info as *const ()),
            (&MESSAGE_POS, "GetMessagePos", get_message_pos as *const ()),
            (&SET_POS, "SetCursorPos", set_cursor_pos as *const ()),
            (&SET_PHYSICAL_POS, "SetPhysicalCursorPos", set_physical_cursor_pos as *const ()),
            (&CLIP, "ClipCursor", clip_cursor as *const ()),
            (&GET_CLIP, "GetClipCursor", get_clip_cursor as *const ()),
            (&ASYNC_KEY, "GetAsyncKeyState", get_async_key_state as *const ()),
            (&KEY_STATE, "GetKeyState", get_key_state as *const ()),
            (&KEYBOARD_STATE, "GetKeyboardState", get_keyboard_state as *const ()),
        ];
        let mut ok = true;
        for (slot, name, detour) in hooks {
            ok &= hook(slot, "user32.dll", name, detour);
        }
        ok
    }
}
