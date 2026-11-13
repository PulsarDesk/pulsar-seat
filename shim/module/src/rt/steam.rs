// SPDX-License-Identifier: GPL-3.0-only
//! Steam in a seat (spec 50 §6.2). Preferred: the service starts Steam as the seat user with
//! `seat_shim_core::steam_launch_args`. This hook only covers `steam.exe` started from the
//! seat shell without the option: `GetCommandLineW/A` return the command line with the
//! per-seat `-master_ipc_name_override`, and the matching variable is set in-process.

use super::hook::Hook;
use seat_shim_core::decide::Role;
use seat_shim_core::steam::{IPC_ENV, ipc_name, steam_cmdline_patch};
use std::sync::atomic::{AtomicUsize, Ordering};
use windows_sys::Win32::Globalization::{CP_ACP, WideCharToMultiByte};
use windows_sys::Win32::System::Environment::{GetCommandLineW, SetEnvironmentVariableW};

static CMD_W: Hook = Hook::new();
static CMD_A: Hook = Hook::new();
static PATCHED_W: AtomicUsize = AtomicUsize::new(0);
static PATCHED_A: AtomicUsize = AtomicUsize::new(0);

type CmdWFn = unsafe extern "system" fn() -> *const u16;
type CmdAFn = unsafe extern "system" fn() -> *const u8;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn seat_id() -> Option<String> {
    match super::live()? {
        (Role::Seat { seat_id, .. }, plan, _) if plan.steam_cmdline || plan.hkcu_redirect => Some(seat_id),
        _ => None,
    }
}

fn active() -> bool {
    super::live().is_some_and(|(r, p, _)| p.steam_cmdline && matches!(r, Role::Seat { .. }))
}

unsafe extern "system" fn cmd_w() -> *const u16 {
    let p = PATCHED_W.load(Ordering::Acquire);
    if p != 0 && active() {
        return p as *const u16;
    }
    // SAFETY: trampoline has GetCommandLineW's signature.
    unsafe {
        let f: CmdWFn = std::mem::transmute(CMD_W.trampoline());
        f()
    }
}

unsafe extern "system" fn cmd_a() -> *const u8 {
    let p = PATCHED_A.load(Ordering::Acquire);
    if p != 0 && active() {
        return p as *const u8;
    }
    // SAFETY: trampoline has GetCommandLineA's signature.
    unsafe {
        let f: CmdAFn = std::mem::transmute(CMD_A.trampoline());
        f()
    }
}

fn to_ansi(w: &[u16]) -> Vec<u8> {
    // SAFETY: size query, then conversion into a buffer of that size; `w` is NUL-terminated.
    unsafe {
        let n = WideCharToMultiByte(CP_ACP, 0, w.as_ptr(), -1, std::ptr::null_mut(), 0, std::ptr::null(), std::ptr::null_mut());
        if n <= 0 {
            return vec![0];
        }
        let mut out = vec![0u8; n as usize];
        WideCharToMultiByte(CP_ACP, 0, w.as_ptr(), -1, out.as_mut_ptr(), n, std::ptr::null(), std::ptr::null_mut());
        out
    }
}

/// Builds the patched command lines (leaked: they must stay valid for the process) and
/// hooks both getters.
///
/// # Safety
/// Init path only.
pub unsafe fn install() -> bool {
    let Some(seat) = seat_id() else {
        // Not a seat right now: the command line was already fixed at start, nothing to do.
        return false;
    };
    // SAFETY: returns the process command line (static buffer).
    let orig = unsafe {
        let p = GetCommandLineW();
        let mut n = 0;
        while *p.add(n) != 0 {
            n += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(p, n))
    };
    let Some(patched) = steam_cmdline_patch(&orig, &seat) else { return true };
    let w: &'static [u16] = Box::leak(wide(&patched).into_boxed_slice());
    let a: &'static [u8] = Box::leak(to_ansi(w).into_boxed_slice());
    PATCHED_W.store(w.as_ptr() as usize, Ordering::Release);
    PATCHED_A.store(a.as_ptr() as usize, Ordering::Release);
    super::log::redirect("steam_cmdline", "ipc override added");
    // kernelbase first: kernel32's exports are stubs into it, and the UCRT binds to
    // kernelbase through API sets, so a kernel32-only hook misses most callers.
    // HYPOTHESIS: on the AppHelp path the UCRT may cache the command line before
    // `NotifyShims`; the launcher path (or the service's launch arguments) covers that.
    const K: &[&str] = &["kernelbase.dll", "kernel32.dll"];
    // SAFETY: detours match the targets' signatures.
    unsafe {
        super::hook::install_first(&CMD_W, K, "GetCommandLineW", cmd_w as *const ())
            & super::hook::install_first(&CMD_A, K, "GetCommandLineA", cmd_a as *const ())
    }
}

/// `steam_master_ipc_name_override=<per-seat name>` in this process (inherited by its
/// children, e.g. the web helper).
pub fn set_env_if_seat() {
    if let Some(seat) = seat_id() {
        let n = wide(IPC_ENV);
        let v = wide(&ipc_name(&seat));
        // SAFETY: valid NUL-terminated strings.
        unsafe { SetEnvironmentVariableW(n.as_ptr(), v.as_ptr()) };
    }
}
