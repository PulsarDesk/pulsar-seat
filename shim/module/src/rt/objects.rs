// SPDX-License-Identifier: GPL-3.0-only
//! Named-object isolation per seat (compat-tweaks 2026-11-17; `seat_shim_core::objects`).
//! The `Create*` / `Open*` calls for mutexes, events, semaphores and file mappings get the
//! per-seat name when the program has rules (`Shim\ObjectRules`) and the process runs in
//! a seat or kiosk role. `W` entry points of `kernelbase.dll` (fallback `kernel32.dll`)
//! form the group; the `A` entry points are hooked best effort (a rewritten name is never
//! rewritten twice, so an `A` that calls its `W` is harmless). HYPOTHESIS: never run on
//! hardware.

use super::hook::{Hook, guarded, install_first};
use seat_shim_core::decide::object_scope;
use seat_shim_core::objects::{rewrite, rules_for};
use std::ffi::c_void;

type H = *mut c_void;
const K: &[&str] = &["kernelbase.dll", "kernel32.dll"];

/// Longest name we read from the caller (object names are limited to `MAX_PATH`-ish
/// lengths by convention; the object manager allows 32 767 characters).
const MAX_NAME: usize = 32_767;

fn new_name(name: &str) -> Option<String> {
    let (role, plan, _, _) = super::live()?;
    if !plan.object_names {
        return None;
    }
    let st = super::state()?;
    let rules = rules_for(st.facts.exe_name(), &st.cfg.object_rules);
    let scope = object_scope(&role)?;
    let out = rewrite(name, &rules, &scope)?;
    super::log::redirect("object_name", &out);
    Some(out)
}

/// Per-seat wide name for `p` (NUL-terminated), or `None` to pass the original.
fn per_seat_w(p: *const u16) -> Option<Vec<u16>> {
    if p.is_null() {
        return None;
    }
    let name = guarded(|| {
        // SAFETY: NUL-terminated string from the caller, read up to MAX_NAME.
        unsafe {
            let n = (0..MAX_NAME).position(|i| *p.add(i) == 0)?;
            Some(String::from_utf16_lossy(std::slice::from_raw_parts(p, n)))
        }
    })
    .flatten()?;
    new_name(&name).map(|s| s.encode_utf16().chain(std::iter::once(0)).collect())
}

/// Per-seat ANSI name: the caller's bytes plus the ASCII suffix (never re-encoded).
fn per_seat_a(p: *const u8) -> Option<Vec<u8>> {
    if p.is_null() {
        return None;
    }
    let bytes = guarded(|| {
        // SAFETY: as above.
        unsafe {
            let n = (0..MAX_NAME).position(|i| *p.add(i) == 0)?;
            Some(std::slice::from_raw_parts(p, n).to_vec())
        }
    })
    .flatten()?;
    let name = String::from_utf8_lossy(&bytes);
    let out = new_name(&name)?;
    // `rewrite` only appends ASCII; keep the original bytes as they are.
    let tail = out.strip_prefix(name.as_ref())?.as_bytes().to_vec();
    let mut v = bytes;
    v.extend_from_slice(&tail);
    v.push(0);
    Some(v)
}

macro_rules! name_w {
    ($p:expr) => {{
        let alt = per_seat_w($p);
        (alt.as_ref().map_or($p, |v| v.as_ptr()), alt)
    }};
}
macro_rules! name_a {
    ($p:expr) => {{
        let alt = per_seat_a($p);
        (alt.as_ref().map_or($p, |v| v.as_ptr()), alt)
    }};
}

static CREATE_MUTEX_W: Hook = Hook::new();
static CREATE_MUTEX_EX_W: Hook = Hook::new();
static OPEN_MUTEX_W: Hook = Hook::new();
static CREATE_EVENT_W: Hook = Hook::new();
static CREATE_EVENT_EX_W: Hook = Hook::new();
static OPEN_EVENT_W: Hook = Hook::new();
static CREATE_SEMAPHORE_EX_W: Hook = Hook::new();
static OPEN_SEMAPHORE_W: Hook = Hook::new();
static CREATE_MAPPING_W: Hook = Hook::new();
static OPEN_MAPPING_W: Hook = Hook::new();
static CREATE_MUTEX_A: Hook = Hook::new();
static OPEN_MUTEX_A: Hook = Hook::new();
static CREATE_EVENT_A: Hook = Hook::new();
static OPEN_EVENT_A: Hook = Hook::new();
static CREATE_MAPPING_A: Hook = Hook::new();
static OPEN_MAPPING_A: Hook = Hook::new();

type CreateMutexW = unsafe extern "system" fn(*const c_void, i32, *const u16) -> H;
type CreateExW = unsafe extern "system" fn(*const c_void, *const u16, u32, u32) -> H;
type OpenW = unsafe extern "system" fn(u32, i32, *const u16) -> H;
type CreateEventW = unsafe extern "system" fn(*const c_void, i32, i32, *const u16) -> H;
type CreateSemaphoreExW = unsafe extern "system" fn(*const c_void, i32, i32, *const u16, u32, u32) -> H;
type CreateMappingW = unsafe extern "system" fn(H, *const c_void, u32, u32, u32, *const u16) -> H;
type CreateMutexA = unsafe extern "system" fn(*const c_void, i32, *const u8) -> H;
type OpenA = unsafe extern "system" fn(u32, i32, *const u8) -> H;
type CreateEventA = unsafe extern "system" fn(*const c_void, i32, i32, *const u8) -> H;
type CreateMappingA = unsafe extern "system" fn(H, *const c_void, u32, u32, u32, *const u8) -> H;

unsafe extern "system" fn create_mutex_w(sa: *const c_void, own: i32, name: *const u16) -> H {
    let (n, _keep) = name_w!(name);
    // SAFETY: trampoline has CreateMutexW's signature.
    unsafe { std::mem::transmute::<usize, CreateMutexW>(CREATE_MUTEX_W.trampoline())(sa, own, n) }
}
unsafe extern "system" fn create_mutex_ex_w(sa: *const c_void, name: *const u16, flags: u32, access: u32) -> H {
    let (n, _keep) = name_w!(name);
    // SAFETY: trampoline has CreateMutexExW's signature.
    unsafe { std::mem::transmute::<usize, CreateExW>(CREATE_MUTEX_EX_W.trampoline())(sa, n, flags, access) }
}
unsafe extern "system" fn open_mutex_w(access: u32, inherit: i32, name: *const u16) -> H {
    let (n, _keep) = name_w!(name);
    // SAFETY: trampoline has OpenMutexW's signature.
    unsafe { std::mem::transmute::<usize, OpenW>(OPEN_MUTEX_W.trampoline())(access, inherit, n) }
}
unsafe extern "system" fn create_event_w(sa: *const c_void, manual: i32, initial: i32, name: *const u16) -> H {
    let (n, _keep) = name_w!(name);
    // SAFETY: trampoline has CreateEventW's signature.
    unsafe { std::mem::transmute::<usize, CreateEventW>(CREATE_EVENT_W.trampoline())(sa, manual, initial, n) }
}
unsafe extern "system" fn create_event_ex_w(sa: *const c_void, name: *const u16, flags: u32, access: u32) -> H {
    let (n, _keep) = name_w!(name);
    // SAFETY: trampoline has CreateEventExW's signature.
    unsafe { std::mem::transmute::<usize, CreateExW>(CREATE_EVENT_EX_W.trampoline())(sa, n, flags, access) }
}
unsafe extern "system" fn open_event_w(access: u32, inherit: i32, name: *const u16) -> H {
    let (n, _keep) = name_w!(name);
    // SAFETY: trampoline has OpenEventW's signature.
    unsafe { std::mem::transmute::<usize, OpenW>(OPEN_EVENT_W.trampoline())(access, inherit, n) }
}
unsafe extern "system" fn create_semaphore_ex_w(sa: *const c_void, init: i32, max: i32, name: *const u16, flags: u32, access: u32) -> H {
    let (n, _keep) = name_w!(name);
    // SAFETY: trampoline has CreateSemaphoreExW's signature.
    unsafe { std::mem::transmute::<usize, CreateSemaphoreExW>(CREATE_SEMAPHORE_EX_W.trampoline())(sa, init, max, n, flags, access) }
}
unsafe extern "system" fn open_semaphore_w(access: u32, inherit: i32, name: *const u16) -> H {
    let (n, _keep) = name_w!(name);
    // SAFETY: trampoline has OpenSemaphoreW's signature.
    unsafe { std::mem::transmute::<usize, OpenW>(OPEN_SEMAPHORE_W.trampoline())(access, inherit, n) }
}
unsafe extern "system" fn create_mapping_w(f: H, sa: *const c_void, protect: u32, hi: u32, lo: u32, name: *const u16) -> H {
    let (n, _keep) = name_w!(name);
    // SAFETY: trampoline has CreateFileMappingW's signature.
    unsafe { std::mem::transmute::<usize, CreateMappingW>(CREATE_MAPPING_W.trampoline())(f, sa, protect, hi, lo, n) }
}
unsafe extern "system" fn open_mapping_w(access: u32, inherit: i32, name: *const u16) -> H {
    let (n, _keep) = name_w!(name);
    // SAFETY: trampoline has OpenFileMappingW's signature.
    unsafe { std::mem::transmute::<usize, OpenW>(OPEN_MAPPING_W.trampoline())(access, inherit, n) }
}
unsafe extern "system" fn create_mutex_a(sa: *const c_void, own: i32, name: *const u8) -> H {
    let (n, _keep) = name_a!(name);
    // SAFETY: trampoline has CreateMutexA's signature.
    unsafe { std::mem::transmute::<usize, CreateMutexA>(CREATE_MUTEX_A.trampoline())(sa, own, n) }
}
unsafe extern "system" fn open_mutex_a(access: u32, inherit: i32, name: *const u8) -> H {
    let (n, _keep) = name_a!(name);
    // SAFETY: trampoline has OpenMutexA's signature.
    unsafe { std::mem::transmute::<usize, OpenA>(OPEN_MUTEX_A.trampoline())(access, inherit, n) }
}
unsafe extern "system" fn create_event_a(sa: *const c_void, manual: i32, initial: i32, name: *const u8) -> H {
    let (n, _keep) = name_a!(name);
    // SAFETY: trampoline has CreateEventA's signature.
    unsafe { std::mem::transmute::<usize, CreateEventA>(CREATE_EVENT_A.trampoline())(sa, manual, initial, n) }
}
unsafe extern "system" fn open_event_a(access: u32, inherit: i32, name: *const u8) -> H {
    let (n, _keep) = name_a!(name);
    // SAFETY: trampoline has OpenEventA's signature.
    unsafe { std::mem::transmute::<usize, OpenA>(OPEN_EVENT_A.trampoline())(access, inherit, n) }
}
unsafe extern "system" fn create_mapping_a(f: H, sa: *const c_void, protect: u32, hi: u32, lo: u32, name: *const u8) -> H {
    let (n, _keep) = name_a!(name);
    // SAFETY: trampoline has CreateFileMappingA's signature.
    unsafe { std::mem::transmute::<usize, CreateMappingA>(CREATE_MAPPING_A.trampoline())(f, sa, protect, hi, lo, n) }
}
unsafe extern "system" fn open_mapping_a(access: u32, inherit: i32, name: *const u8) -> H {
    let (n, _keep) = name_a!(name);
    // SAFETY: trampoline has OpenFileMappingA's signature.
    unsafe { std::mem::transmute::<usize, OpenA>(OPEN_MAPPING_A.trampoline())(access, inherit, n) }
}

/// # Safety
/// Init path only.
pub unsafe fn install() -> bool {
    // SAFETY: every detour has its target's signature and calling convention.
    unsafe {
        let w = [
            install_first(&CREATE_MUTEX_W, K, "CreateMutexW", create_mutex_w as *const ()),
            install_first(&CREATE_MUTEX_EX_W, K, "CreateMutexExW", create_mutex_ex_w as *const ()),
            install_first(&OPEN_MUTEX_W, K, "OpenMutexW", open_mutex_w as *const ()),
            install_first(&CREATE_EVENT_W, K, "CreateEventW", create_event_w as *const ()),
            install_first(&CREATE_EVENT_EX_W, K, "CreateEventExW", create_event_ex_w as *const ()),
            install_first(&OPEN_EVENT_W, K, "OpenEventW", open_event_w as *const ()),
            install_first(&CREATE_SEMAPHORE_EX_W, K, "CreateSemaphoreExW", create_semaphore_ex_w as *const ()),
            install_first(&OPEN_SEMAPHORE_W, K, "OpenSemaphoreW", open_semaphore_w as *const ()),
            install_first(&CREATE_MAPPING_W, K, "CreateFileMappingW", create_mapping_w as *const ()),
            install_first(&OPEN_MAPPING_W, K, "OpenFileMappingW", open_mapping_w as *const ()),
        ];
        // Best effort: the ANSI forms.
        install_first(&CREATE_MUTEX_A, K, "CreateMutexA", create_mutex_a as *const ());
        install_first(&OPEN_MUTEX_A, K, "OpenMutexA", open_mutex_a as *const ());
        install_first(&CREATE_EVENT_A, K, "CreateEventA", create_event_a as *const ());
        install_first(&OPEN_EVENT_A, K, "OpenEventA", open_event_a as *const ());
        install_first(&CREATE_MAPPING_A, K, "CreateFileMappingA", create_mapping_a as *const ());
        install_first(&OPEN_MAPPING_A, K, "OpenFileMappingA", open_mapping_a as *const ());
        w.iter().all(|x| *x)
    }
}
