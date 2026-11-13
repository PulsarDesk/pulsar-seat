// SPDX-License-Identifier: GPL-3.0-only
//! Detour helper over `retour::RawDetour` (BSD-2-Clause).
//!
//! Targets in a module that is not loaded yet (review fix 2026-11-12):
//! - launcher path (`PulsarSplitInit`, not under the loader lock): the module is loaded
//!   from System32 (`LoadLibraryExW(.., LOAD_LIBRARY_SEARCH_SYSTEM32)`) and hooked now;
//! - AppHelp / DllMain fallback paths: the hook is registered as *pending* and installed
//!   from an `LdrRegisterDllNotification` callback when the module loads. A pending hook
//!   counts as installed for its group (it is in place before any code of that module runs).
//!
//! Patching runs with the other threads suspended ([`super::threads`]).

use std::cell::Cell;
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use windows_sys::Win32::Foundation::HMODULE;
use windows_sys::Win32::System::LibraryLoader::{
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_PIN, GetModuleHandleExW, GetModuleHandleW,
    GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};

/// Trampoline of one hooked function (0 = not hooked) and whether it waits for its module.
pub struct Hook {
    tramp: AtomicUsize,
    pending: AtomicBool,
}

impl Hook {
    pub const fn new() -> Self {
        Self { tramp: AtomicUsize::new(0), pending: AtomicBool::new(false) }
    }
    pub fn trampoline(&self) -> usize {
        self.tramp.load(Ordering::Acquire)
    }
    pub fn is_on(&self) -> bool {
        self.trampoline() != 0
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Set on the launcher path only: missing target modules may be loaded.
static LOAD_ALLOWED: AtomicBool = AtomicBool::new(false);
/// Set while init runs before the target's first instruction (launcher path), where no
/// other thread runs target code; patching still freezes the loader's worker threads.
pub fn allow_loading(on: bool) {
    LOAD_ALLOWED.store(on, Ordering::Release);
}

/// Address of an export of an already-loaded module (forwarders are resolved).
pub fn export(module: &str, name: &str) -> Option<usize> {
    let m = wide(module);
    // SAFETY: NUL-terminated string; the module is not loaded by this call.
    let h = unsafe { GetModuleHandleW(m.as_ptr()) };
    export_of(h, name)
}

fn export_of(h: HMODULE, name: &str) -> Option<usize> {
    if h.is_null() {
        return None;
    }
    let n: Vec<u8> = name.bytes().chain(std::iter::once(0)).collect();
    // SAFETY: valid module handle, NUL-terminated name.
    unsafe { GetProcAddress(h, n.as_ptr()).map(|f| f as usize) }
}

fn load_system(module: &str) -> HMODULE {
    let m = wide(module);
    // SAFETY: System32-only load of a Windows DLL, not under the loader lock (launcher path).
    unsafe { LoadLibraryExW(m.as_ptr(), std::ptr::null_mut(), LOAD_LIBRARY_SEARCH_SYSTEM32) }
}

/// Creates and enables the detour at `target`.
///
/// # Safety
/// `detour` must have exactly the target's signature and calling convention.
unsafe fn patch(slot: &'static Hook, target: usize, name: &str, detour: *const (), freeze: bool) -> Result<(), String> {
    // Allocation (trampoline, relocation) happens here, before any thread is suspended.
    // SAFETY: target is a function entry from an export table; detour matches it.
    let d = unsafe { retour::RawDetour::new(target as *const (), detour) }.map_err(|e| format!("{name}: {e}"))?;
    // Published before the jump is written, so a call racing with the patch finds it.
    slot.tramp.store(d.trampoline() as *const () as usize, Ordering::Release);
    // SAFETY: patches the target prologue with the other threads suspended.
    let r = super::threads::with_others_frozen(target, freeze, || unsafe { d.enable() });
    match r {
        Some(Ok(())) => {
            slot.pending.store(false, Ordering::Release);
            // The detour must live as long as the process (the module is pinned).
            std::mem::forget(d);
            Ok(())
        }
        Some(Err(e)) => {
            slot.tramp.store(0, Ordering::Release);
            Err(format!("{name}: {e}"))
        }
        None => {
            slot.tramp.store(0, Ordering::Release);
            Err(format!("{name}: a thread kept executing the prologue"))
        }
    }
}

/// Hooks the first of `modules` that exports `name` with `detour`.
///
/// # Safety
/// `detour` must have exactly the target's signature and calling convention.
pub unsafe fn install_first(slot: &'static Hook, modules: &[&'static str], name: &'static str, detour: *const ()) -> bool {
    if slot.is_on() || slot.pending.load(Ordering::Acquire) {
        return true;
    }
    // Already loaded: hook now.
    for m in modules {
        if let Some(t) = export(m, name) {
            // SAFETY: forwarded from the caller.
            return match unsafe { patch(slot, t, name, detour, true) } {
                Ok(()) => true,
                Err(e) => {
                    super::log::error("hook_enable_failed", &e);
                    false
                }
            };
        }
    }
    let first = modules[0];
    if LOAD_ALLOWED.load(Ordering::Acquire) {
        if let Some(t) = modules.iter().find_map(|m| export_of(load_system(m), name)) {
            // SAFETY: forwarded from the caller.
            return match unsafe { patch(slot, t, name, detour, true) } {
                Ok(()) => true,
                Err(e) => {
                    super::log::error("hook_enable_failed", &e);
                    false
                }
            };
        }
        super::log::error("hook_target_missing", name);
        return false;
    }
    defer(slot, first, name, detour)
}

/// Hooks `module!name` with `detour`.
///
/// # Safety
/// `detour` must have exactly the target's signature and calling convention.
pub unsafe fn install(slot: &'static Hook, module: &'static str, name: &'static str, detour: *const ()) -> bool {
    // SAFETY: forwarded from the caller.
    unsafe { install_first(slot, &[module], name, detour) }
}

// ---------------------------------------------------------------- deferred hooks

struct Pending {
    slot: &'static Hook,
    module: &'static str,
    name: &'static str,
    detour: usize,
}

static PENDING: Mutex<Vec<Pending>> = Mutex::new(Vec::new());
static NOTIFY_COOKIE: AtomicUsize = AtomicUsize::new(0);
/// Deferred hooks that could not be installed when their module loaded.
pub static DEFERRED_FAILED: AtomicUsize = AtomicUsize::new(0);

#[repr(C)]
struct UnicodeString {
    length: u16,
    maximum_length: u16,
    buffer: *const u16,
}

/// `LDR_DLL_LOADED_NOTIFICATION_DATA` (same layout for loaded / unloaded).
#[repr(C)]
struct DllNotificationData {
    flags: u32,
    full_dll_name: *const UnicodeString,
    base_dll_name: *const UnicodeString,
    dll_base: *mut c_void,
    size_of_image: u32,
}

const LDR_DLL_NOTIFICATION_REASON_LOADED: u32 = 1;

type NotifyFn = unsafe extern "system" fn(u32, *const DllNotificationData, *mut c_void);
type RegisterFn = unsafe extern "system" fn(u32, NotifyFn, *mut c_void, *mut *mut c_void) -> i32;

fn eq_name(u: &UnicodeString, name: &str) -> bool {
    if u.buffer.is_null() {
        return false;
    }
    // SAFETY: the loader passes a valid counted string.
    let s = unsafe { std::slice::from_raw_parts(u.buffer, u.length as usize / 2) };
    s.len() == name.len() && s.iter().zip(name.bytes()).all(|(a, b)| *a < 128 && (*a as u8).eq_ignore_ascii_case(&b))
}

/// Runs under the loader lock: no loading, no event log, only patching of the module that
/// is being mapped (nobody runs its code yet, so no thread is frozen).
unsafe extern "system" fn on_dll(reason: u32, data: *const DllNotificationData, _ctx: *mut c_void) {
    if reason != LDR_DLL_NOTIFICATION_REASON_LOADED || data.is_null() {
        return;
    }
    // SAFETY: loader-provided data.
    let (base, name) = unsafe { ((*data).dll_base, (*data).base_dll_name) };
    if name.is_null() {
        return;
    }
    let mut list = match PENDING.lock() {
        Ok(l) => l,
        Err(e) => e.into_inner(),
    };
    list.retain(|p| {
        // SAFETY: loader-provided string.
        if !eq_name(unsafe { &*name }, p.module) {
            return true;
        }
        let ok = export_of(base as HMODULE, p.name).is_some_and(|t| {
            // SAFETY: the detour was registered with the target's signature.
            unsafe { patch(p.slot, t, p.name, p.detour as *const (), false) }.is_ok()
        });
        if !ok {
            p.slot.pending.store(false, Ordering::Release);
            DEFERRED_FAILED.fetch_add(1, Ordering::Relaxed);
            super::log::etw_error("hook_deferred_failed", p.name);
        }
        false
    });
}

fn ensure_notification() -> bool {
    if NOTIFY_COOKIE.load(Ordering::Acquire) != 0 {
        return true;
    }
    let Some(f) = export("ntdll.dll", "LdrRegisterDllNotification") else { return false };
    // SAFETY: documented signature (winternl.h: LdrRegisterDllNotification).
    let register: RegisterFn = unsafe { std::mem::transmute(f) };
    let mut cookie: *mut c_void = std::ptr::null_mut();
    // SAFETY: callback lives as long as the (pinned) module.
    let st = unsafe { register(0, on_dll, std::ptr::null_mut(), &mut cookie) };
    if st < 0 || cookie.is_null() {
        return false;
    }
    NOTIFY_COOKIE.store(cookie as usize, Ordering::Release);
    true
}

fn defer(slot: &'static Hook, module: &'static str, name: &'static str, detour: *const ()) -> bool {
    if !ensure_notification() {
        super::log::error("hook_target_missing", name);
        return false;
    }
    slot.pending.store(true, Ordering::Release);
    PENDING.lock().unwrap_or_else(|e| e.into_inner()).push(Pending { slot, module, name, detour: detour as usize });
    // The module may have loaded before the entry was queued (no lock is held across the
    // loader call below, so the notification callback can never wait on us).
    if let Some(t) = export(module, name) {
        let mut list = PENDING.lock().unwrap_or_else(|e| e.into_inner());
        let before = list.len();
        list.retain(|p| !std::ptr::eq(p.slot, slot));
        let ours = list.len() != before;
        drop(list);
        if ours {
            // SAFETY: forwarded from the caller.
            return match unsafe { patch(slot, t, name, detour, true) } {
                Ok(()) => true,
                Err(e) => {
                    slot.pending.store(false, Ordering::Release);
                    super::log::error("hook_enable_failed", &e);
                    false
                }
            };
        }
    }
    true
}

/// Keeps the module mapped forever: detours point into it.
pub fn pin_module() {
    let mut h = std::ptr::null_mut();
    // SAFETY: FROM_ADDRESS takes any address inside this module.
    unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_PIN,
            pin_module as *const () as *const u16,
            &mut h,
        );
    }
}

thread_local! {
    static INSIDE: Cell<bool> = const { Cell::new(false) };
}

/// Runs `f` unless this thread is already inside one of our decisions (a decision's own
/// API calls must not be filtered again). Returns `None` when re-entered.
pub fn guarded<R>(f: impl FnOnce() -> R) -> Option<R> {
    let entered = INSIDE.try_with(|c| !c.replace(true)).unwrap_or(false);
    if !entered {
        return None;
    }
    let r = f();
    let _ = INSIDE.try_with(|c| c.set(false));
    Some(r)
}
