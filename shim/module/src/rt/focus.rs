// SPDX-License-Identifier: GPL-3.0-only
//! Controller-focus filter in the seat shell (spec 50 §5.2): a publication of one of the
//! shell's controller-focus WNF states whose target PID is in another session is dropped.
//! Installed on builds where the names and payload were verified (`wnf_known`), or, since
//! the parity review (2026-11-17), when the runtime check passes in this process
//! ([`runtime_verified`]: every candidate state can be queried and its payload reads as
//! "target PID first"). Off switch: `Shim\WnfRuntimeCheck` = 0.
//! HYPOTHESIS: never run on hardware; which candidate is which focus kind is unknown.

use super::hook::Hook;
use seat_shim_core::decide::{WNF_FOCUS_CANDIDATES, filter_focus, focus_target_pid};
use std::ffi::c_void;
use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;

static PUBLISH: Hook = Hook::new();

/// The module runs in the game-input service (session 0, opt-in) with only this hook.
static SERVICE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// `(tick, seat sessions)` of the last seat-map read in the service (refreshed every 2 s).
static SEAT_SESSIONS: std::sync::Mutex<(u64, Vec<u32>)> = std::sync::Mutex::new((0, Vec::new()));

fn seat_sessions() -> Vec<u32> {
    use seat_shim_core::seatmap::SeatMapReader;
    use windows_sys::Win32::System::SystemInformation::GetTickCount64;
    // SAFETY: plain call.
    let now = unsafe { GetTickCount64() };
    let mut g = SEAT_SESSIONS.lock().unwrap_or_else(|e| e.into_inner());
    if g.0 == 0 || now.saturating_sub(g.0) >= 2000 {
        g.1 = seat_shim_core::win::RegSeatMap.read_all().unwrap_or_default().iter().map(|e| e.session_id).collect();
        g.0 = now.max(1);
    }
    g.1.clone()
}

/// Service mode (game-input service): drop publications that move focus to a seat session.
fn service_drop(payload: &[u8]) -> Option<(u32, bool)> {
    let pid = focus_target_pid(payload)?;
    let mut sess = 0u32;
    // SAFETY: out parameter.
    if unsafe { ProcessIdToSessionId(pid, &mut sess) } == 0 {
        return None;
    }
    let console = Some(seat_shim_core::win::active_console_session()).filter(|c| *c != u32::MAX);
    Some((pid, seat_shim_core::decide::filter_focus_service(sess, console, &seat_sessions())))
}

/// Game-input service mode (opt-in, `Shim\GameInputFocus` = 1): only the focus hook.
///
/// # Safety
/// Init path only.
pub unsafe fn install_service() -> bool {
    SERVICE.store(true, std::sync::atomic::Ordering::Release);
    // SAFETY: as `install`.
    unsafe { install() }
}
type PublishFn = unsafe extern "system" fn(u64, *const c_void, *const c_void, u32, *const c_void) -> i32;

unsafe extern "system" fn publish(name: u64, type_id: *const c_void, buf: *const c_void, len: u32, scope: *const c_void) -> i32 {
    if WNF_FOCUS_CANDIDATES.contains(&name) && !buf.is_null() && len >= 4 && SERVICE.load(std::sync::atomic::Ordering::Acquire) {
        // SAFETY: the caller passes `len` readable bytes.
        let payload = unsafe { std::slice::from_raw_parts(buf as *const u8, len as usize) };
        if let Some((pid, drop)) = super::hook::guarded(|| service_drop(payload)).flatten() {
            super::log::focus(name, pid, drop);
            if drop {
                return 0;
            }
        }
    } else if WNF_FOCUS_CANDIDATES.contains(&name) && !buf.is_null() && len >= 4 {
        if let Some((role, plan, _, _)) = super::live() {
            if plan.focus_filter {
                // SAFETY: the caller passes `len` readable bytes.
                let payload = unsafe { std::slice::from_raw_parts(buf as *const u8, len as usize) };
                if let Some(pid) = focus_target_pid(payload) {
                    let mut sess = 0u32;
                    // SAFETY: out parameter.
                    let ok = unsafe { ProcessIdToSessionId(pid, &mut sess) } != 0;
                    let own = super::state().map_or(0, |s| s.facts.session_id);
                    let drop = ok && filter_focus(&role, sess, own);
                    super::log::focus(name, pid, drop);
                    if drop {
                        return 0; // STATUS_SUCCESS: the caller sees a normal publication.
                    }
                }
            }
        }
    }
    // SAFETY: trampoline has RtlPublishWnfStateData's signature.
    unsafe {
        let f: PublishFn = std::mem::transmute(PUBLISH.trampoline());
        f(name, type_id, buf, len, scope)
    }
}

/// `NtQueryWnfStateData` (ntdll; public interop signature): state name by pointer, type
/// id, explicit scope, change stamp out, buffer, size in / out.
type QueryFn = unsafe extern "system" fn(*const u64, *const c_void, *const c_void, *mut u32, *mut c_void, *mut u32) -> i32;

/// The runtime check of the focus WNF states (`seat_shim_core::decide::wnf_runtime_plausible`).
pub fn runtime_verified() -> bool {
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    let ntdll: Vec<u16> = "ntdll.dll".encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: ntdll is always loaded; the export name is NUL-terminated.
    let query: Option<QueryFn> = unsafe {
        let m = GetModuleHandleW(ntdll.as_ptr());
        if m.is_null() {
            return false;
        }
        GetProcAddress(m, c"NtQueryWnfStateData".as_ptr().cast()).map(|f| std::mem::transmute(f))
    };
    let Some(query) = query else { return false };
    let samples: Vec<(u64, Option<Vec<u8>>)> = WNF_FOCUS_CANDIDATES
        .iter()
        .map(|&name| {
            let mut buf = vec![0u8; 4096];
            let mut size = buf.len() as u32;
            let mut stamp = 0u32;
            // SAFETY: valid pointers for the call's duration; size = the buffer's length.
            let st = unsafe { query(&name, std::ptr::null(), std::ptr::null(), &mut stamp, buf.as_mut_ptr().cast(), &mut size) };
            if st < 0 {
                return (name, None);
            }
            buf.truncate((size as usize).min(4096));
            (name, Some(buf))
        })
        .collect();
    let alive = |pid: u32| {
        let mut s = 0u32;
        // SAFETY: out parameter.
        unsafe { ProcessIdToSessionId(pid, &mut s) != 0 }
    };
    seat_shim_core::decide::wnf_runtime_plausible(&samples, &alive)
}

/// # Safety
/// Init path only.
pub unsafe fn install() -> bool {
    // SAFETY: detour matches `RtlPublishWnfStateData` (WNF_STATE_NAME is 8 bytes by value).
    unsafe { super::hook::install(&PUBLISH, "ntdll.dll", "RtlPublishWnfStateData", publish as *const ()) }
}
