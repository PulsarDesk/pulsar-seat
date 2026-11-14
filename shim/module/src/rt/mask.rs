// SPDX-License-Identifier: GPL-3.0-only
//! Remote-session masking, per-application opt-in (`Shim\MaskApps`), seat sessions only
//! (spec 50 §8.2): "which session is on the console?" answers the caller's own session and
//! `GetSystemMetrics(SM_REMOTESESSION)` answers 0. Display-device enumeration is not
//! changed (our IddCx monitor is not the RDP adapter). Anti-cheat software may treat this
//! as tampering; the manager warns about it.

use super::hook::Hook;
use windows_sys::Win32::UI::WindowsAndMessaging::SM_REMOTESESSION;

static RTL_CONSOLE: Hook = Hook::new();
static WTS_CONSOLE: Hook = Hook::new();
static METRICS: Hook = Hook::new();

type ConsoleFn = unsafe extern "system" fn() -> u32;
type MetricsFn = unsafe extern "system" fn(i32) -> i32;

/// `KUSER_SHARED_DATA` is mapped read-only at this address in every x86 / x64 process;
/// `ActiveConsoleId` is at offset 0x2D8 (documented layout, ntddk.h).
const USER_SHARED_DATA: usize = 0x7FFE_0000;
const ACTIVE_CONSOLE_ID: usize = 0x2D8;

/// The real active console session (unmasked), for our own decisions. Read from
/// `KUSER_SHARED_DATA` directly: no export (hooked or not) is called, so this can never
/// re-enter our own masking hooks (review fix 2026-11-12).
pub fn real_console_session() -> u32 {
    // SAFETY: the shared user page is always mapped and readable; volatile because the
    // kernel updates it.
    unsafe { std::ptr::read_volatile((USER_SHARED_DATA + ACTIVE_CONSOLE_ID) as *const u32) }
}

fn masking() -> Option<u32> {
    let (_, plan, _, _) = super::live()?;
    if !plan.mask_remote {
        return None;
    }
    super::state().map(|s| s.facts.session_id)
}

unsafe extern "system" fn rtl_console() -> u32 {
    if let Some(own) = masking() {
        return own;
    }
    // SAFETY: trampoline has RtlGetActiveConsoleId's signature.
    unsafe {
        let f: ConsoleFn = std::mem::transmute(RTL_CONSOLE.trampoline());
        f()
    }
}

unsafe extern "system" fn wts_console() -> u32 {
    if let Some(own) = masking() {
        return own;
    }
    // SAFETY: trampoline has WTSGetActiveConsoleSessionId's signature.
    unsafe {
        let f: ConsoleFn = std::mem::transmute(WTS_CONSOLE.trampoline());
        f()
    }
}

unsafe extern "system" fn metrics(index: i32) -> i32 {
    if index == SM_REMOTESESSION && masking().is_some() {
        return 0;
    }
    // SAFETY: trampoline has GetSystemMetrics's signature.
    unsafe {
        let f: MetricsFn = std::mem::transmute(METRICS.trampoline());
        f(index)
    }
}

/// # Safety
/// Init path only.
pub unsafe fn install() -> bool {
    // SAFETY: detours match the targets' signatures. `RtlGetActiveConsoleId` is optional
    // (absent on older builds).
    unsafe {
        let wts = super::hook::install_first(
            &WTS_CONSOLE,
            &["kernelbase.dll", "kernel32.dll"],
            "WTSGetActiveConsoleSessionId",
            wts_console as *const (),
        );
        let met = super::hook::install(&METRICS, "user32.dll", "GetSystemMetrics", metrics as *const ());
        if super::hook::export("ntdll.dll", "RtlGetActiveConsoleId").is_some() {
            super::hook::install(&RTL_CONSOLE, "ntdll.dll", "RtlGetActiveConsoleId", rtl_console as *const ());
        }
        wts && met
    }
}
