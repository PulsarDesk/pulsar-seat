// SPDX-License-Identifier: GPL-3.0-only
//! HKCU redirect fallback (spec 50 §6.1; contracts §4.3): only when two seats share one
//! Windows account (`SeatFlags::shared_account`, which seat validation forbids). Opens and
//! creates of `HKCU\Software\Valve\Steam…` go to
//! `HKCU\Software\PulsarSplit\SeatHkcu\<seat id>\Software\Valve\Steam…`.
//! Limitation: only paths given relative to the predefined HKCU handle are seen; a key
//! opened earlier (e.g. `HKCU\Software`) and then `Valve\Steam` below it is not redirected.

use super::hook::Hook;
use split_shim_core::decide::{Role, hkcu_redirect};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::System::Registry::{HKEY, HKEY_CURRENT_USER};

static OPEN_W: Hook = Hook::new();
static OPEN_A: Hook = Hook::new();
static CREATE_W: Hook = Hook::new();
static CREATE_A: Hook = Hook::new();

type OpenWFn = unsafe extern "system" fn(HKEY, *const u16, u32, u32, *mut HKEY) -> u32;
type OpenAFn = unsafe extern "system" fn(HKEY, *const u8, u32, u32, *mut HKEY) -> u32;
type CreateWFn = unsafe extern "system" fn(HKEY, *const u16, u32, *const u16, u32, u32, *const SECURITY_ATTRIBUTES, *mut HKEY, *mut u32) -> u32;
type CreateAFn = unsafe extern "system" fn(HKEY, *const u8, u32, *const u8, u32, u32, *const SECURITY_ATTRIBUTES, *mut HKEY, *mut u32) -> u32;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Redirected HKCU-relative path, if this call must be redirected.
fn target(hkey: HKEY, sub: Option<String>) -> Option<Vec<u16>> {
    if hkey != HKEY_CURRENT_USER {
        return None;
    }
    let sub = sub?;
    let (role, plan, map) = super::live()?;
    if !plan.hkcu_redirect {
        return None;
    }
    let Role::Seat { session_id, .. } = &role else { return None };
    let shared = map.iter().any(|e| e.session_id == *session_id && e.flags.shared_account);
    if !shared {
        return None;
    }
    let to = hkcu_redirect(&role, &sub)?;
    super::log::redirect("hkcu", "steam key");
    Some(wide(&to))
}

/// # Safety
/// `p` is null or NUL-terminated.
unsafe fn wstr(p: *const u16) -> Option<String> {
    if p.is_null() {
        return None;
    }
    let mut n = 0;
    // SAFETY: caller guarantees NUL termination.
    unsafe {
        while *p.add(n) != 0 && n < 4096 {
            n += 1;
        }
        Some(String::from_utf16_lossy(std::slice::from_raw_parts(p, n)))
    }
}

/// # Safety
/// `p` is null or NUL-terminated.
unsafe fn astr(p: *const u8) -> Option<String> {
    if p.is_null() {
        return None;
    }
    // SAFETY: caller guarantees NUL termination.
    Some(unsafe { std::ffi::CStr::from_ptr(p.cast()) }.to_string_lossy().into_owned())
}

unsafe extern "system" fn open_w(h: HKEY, sub: *const u16, opt: u32, sam: u32, out: *mut HKEY) -> u32 {
    // SAFETY: caller's arguments; trampoline matches RegOpenKeyExW.
    unsafe {
        let f: OpenWFn = std::mem::transmute(OPEN_W.trampoline());
        match target(h, wstr(sub)) {
            Some(to) => f(h, to.as_ptr(), opt, sam, out),
            None => f(h, sub, opt, sam, out),
        }
    }
}

unsafe extern "system" fn open_a(h: HKEY, sub: *const u8, opt: u32, sam: u32, out: *mut HKEY) -> u32 {
    // SAFETY: caller's arguments; trampolines match RegOpenKeyExA / RegOpenKeyExW.
    unsafe {
        if let Some(to) = target(h, astr(sub)) {
            let f: OpenWFn = std::mem::transmute(OPEN_W.trampoline());
            return f(h, to.as_ptr(), opt, sam, out);
        }
        let f: OpenAFn = std::mem::transmute(OPEN_A.trampoline());
        f(h, sub, opt, sam, out)
    }
}

unsafe extern "system" fn create_w(
    h: HKEY,
    sub: *const u16,
    res: u32,
    class: *const u16,
    opt: u32,
    sam: u32,
    sa: *const SECURITY_ATTRIBUTES,
    out: *mut HKEY,
    disp: *mut u32,
) -> u32 {
    // SAFETY: caller's arguments; trampoline matches RegCreateKeyExW.
    unsafe {
        let f: CreateWFn = std::mem::transmute(CREATE_W.trampoline());
        match target(h, wstr(sub)) {
            Some(to) => f(h, to.as_ptr(), res, class, opt, sam, sa, out, disp),
            None => f(h, sub, res, class, opt, sam, sa, out, disp),
        }
    }
}

unsafe extern "system" fn create_a(
    h: HKEY,
    sub: *const u8,
    res: u32,
    class: *const u8,
    opt: u32,
    sam: u32,
    sa: *const SECURITY_ATTRIBUTES,
    out: *mut HKEY,
    disp: *mut u32,
) -> u32 {
    // SAFETY: caller's arguments; trampolines match RegCreateKeyExA / RegCreateKeyExW.
    unsafe {
        if let Some(to) = target(h, astr(sub)) {
            let f: CreateWFn = std::mem::transmute(CREATE_W.trampoline());
            return f(h, to.as_ptr(), res, std::ptr::null(), opt, sam, sa, out, disp);
        }
        let f: CreateAFn = std::mem::transmute(CREATE_A.trampoline());
        f(h, sub, res, class, opt, sam, sa, out, disp)
    }
}

/// # Safety
/// Init path only.
pub unsafe fn install() -> bool {
    // kernelbase first (advapi32's exports forward there; API-set callers bind to it).
    const K: &[&str] = &["kernelbase.dll", "advapi32.dll"];
    // SAFETY: detours match the targets' signatures.
    unsafe {
        super::hook::install_first(&OPEN_W, K, "RegOpenKeyExW", open_w as *const ())
            & super::hook::install_first(&OPEN_A, K, "RegOpenKeyExA", open_a as *const ())
            & super::hook::install_first(&CREATE_W, K, "RegCreateKeyExW", create_w as *const ())
            & super::hook::install_first(&CREATE_A, K, "RegCreateKeyExA", create_a as *const ())
    }
}
