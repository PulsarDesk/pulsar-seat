// SPDX-License-Identifier: GPL-3.0-only
//! Crash guard, module side (spec 50 §12.2): a vectored exception handler that notices a
//! fatal exception raised inside this module's own code and reports `fault` once over the
//! health pipe. The service counts faults per target (`split_shim_core::FaultCounter`) and
//! writes the target to `Shim\DisabledTargets`. The handler never handles the exception.
//!
//! The handler runs in exception context, possibly while this thread holds the heap lock or
//! one of our locks (review fix 2026-11-12). So it takes no lock and allocates nothing:
//! the frames (one per role) and the NT pipe name are built at init ([`prepare`]), the role
//! is an atomic kept current by `live()`, and the pipe is opened with the real
//! `NtCreateFile` (our hook's trampoline when hooked) and written with `NtWriteFile`.

use split_shim_core::decide::{ProcessFacts, Role};
use std::ffi::c_void;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use windows_sys::Win32::Foundation::{
    EXCEPTION_ACCESS_VIOLATION, EXCEPTION_ILLEGAL_INSTRUCTION, EXCEPTION_INT_DIVIDE_BY_ZERO, EXCEPTION_STACK_OVERFLOW,
    HANDLE,
};
use windows_sys::Win32::System::Diagnostics::Debug::{AddVectoredExceptionHandler, EXCEPTION_POINTERS};

static BASE: AtomicUsize = AtomicUsize::new(0);
static SIZE: AtomicUsize = AtomicUsize::new(0);
static REPORTED: AtomicBool = AtomicBool::new(false);
static ROLE: AtomicU8 = AtomicU8::new(0);
const EXCEPTION_CONTINUE_SEARCH: i32 = 0;

const ROLES: [&str; 3] = ["none", "console", "seat"];

fn role_index(r: &Role) -> u8 {
    match r {
        Role::None => 0,
        Role::Console => 1,
        Role::Seat { .. } => 2,
    }
}

/// Kept current by `live()`; read by the handler.
pub fn set_role(r: &Role) {
    ROLE.store(role_index(r), Ordering::Relaxed);
}

struct Prepared {
    frames: [Vec<u8>; 3],
    /// `\??\pipe\PulsarSplit.ShimHealth`, UTF-16, no NUL.
    name: Vec<u16>,
    nt_create: usize,
    nt_write: usize,
    nt_close: usize,
}

static PREPARED: OnceLock<Prepared> = OnceLock::new();

/// Builds everything the handler needs. Called once from init, before [`install`].
pub fn prepare(facts: &ProcessFacts, role: &Role) {
    set_role(role);
    let nt_path = split_shim_core::HEALTH_PIPE.replacen(r"\\.\", r"\??\", 1);
    let ex = |n| super::hook::export("ntdll.dll", n);
    let (Some(nt_create), Some(nt_write), Some(nt_close)) = (ex("NtCreateFile"), ex("NtWriteFile"), ex("NtClose")) else {
        return;
    };
    let _ = PREPARED.set(Prepared {
        frames: ROLES.map(|r| super::health::frame(facts, r, true, Some("fault"))),
        name: nt_path.encode_utf16().collect(),
        nt_create,
        nt_write,
        nt_close,
    });
}

/// `SizeOfImage` from our own PE header (same offset in PE32 and PE32+).
fn image_size(base: usize) -> usize {
    // SAFETY: `base` is the start of our mapped image; the headers are always mapped.
    unsafe {
        let lfanew = std::ptr::read_unaligned((base + 0x3C) as *const u32) as usize;
        std::ptr::read_unaligned((base + lfanew + 24 + 56) as *const u32) as usize
    }
}

#[repr(C)]
struct Sqos {
    length: u32,
    impersonation_level: u32,
    context_tracking_mode: u8,
    effective_only: u8,
}

type NtWriteFileFn =
    unsafe extern "system" fn(HANDLE, HANDLE, *const c_void, *const c_void, *mut [usize; 2], *const u8, u32, *const i64, *const u32) -> i32;
type NtCloseFn = unsafe extern "system" fn(HANDLE) -> i32;

/// Lock-free, allocation-free: stack data and the prepared buffers only.
fn report_fault() {
    use super::hid::{NtCreateFileFn, ObjectAttributes, UnicodeString};
    let Some(p) = PREPARED.get() else { return };
    // Through our hook's trampoline when hooked (never the detour), else the export.
    let create = super::hid::nt_create_trampoline().unwrap_or(p.nt_create);
    let frame = &p.frames[(ROLE.load(Ordering::Relaxed) as usize).min(2)];
    let name = UnicodeString {
        length: (p.name.len() * 2) as u16,
        maximum_length: (p.name.len() * 2) as u16,
        buffer: p.name.as_ptr(),
    };
    // SecurityIdentification, static tracking: a pipe server can identify, never
    // impersonate, this (possibly elevated) client.
    let sqos = Sqos { length: std::mem::size_of::<Sqos>() as u32, impersonation_level: 1, context_tracking_mode: 0, effective_only: 0 };
    let oa = ObjectAttributes {
        length: std::mem::size_of::<ObjectAttributes>() as u32,
        root_directory: std::ptr::null_mut(),
        object_name: &name,
        attributes: 0x40, // OBJ_CASE_INSENSITIVE
        security_descriptor: std::ptr::null(),
        security_quality_of_service: (&sqos as *const Sqos).cast(),
    };
    const FILE_WRITE_DATA: u32 = 0x0002;
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const FILE_SHARE_READ_WRITE: u32 = 0x3;
    const FILE_OPEN: u32 = 1;
    const FILE_NON_DIRECTORY_FILE: u32 = 0x40;
    const FILE_SYNCHRONOUS_IO_NONALERT: u32 = 0x20;
    let mut h: HANDLE = std::ptr::null_mut();
    let mut iosb = [0usize; 2];
    // SAFETY: resolved ntdll exports with their documented signatures; every pointer is to
    // stack data or a prepared buffer that lives for the process.
    unsafe {
        let create: NtCreateFileFn = std::mem::transmute(create);
        let st = create(
            &mut h,
            FILE_WRITE_DATA | SYNCHRONIZE,
            &oa,
            iosb.as_mut_ptr().cast(),
            std::ptr::null(),
            0,
            FILE_SHARE_READ_WRITE,
            FILE_OPEN,
            FILE_NON_DIRECTORY_FILE | FILE_SYNCHRONOUS_IO_NONALERT,
            std::ptr::null(),
            0,
        );
        if st < 0 || h.is_null() {
            return;
        }
        let write: NtWriteFileFn = std::mem::transmute(p.nt_write);
        let close: NtCloseFn = std::mem::transmute(p.nt_close);
        write(h, std::ptr::null_mut(), std::ptr::null(), std::ptr::null(), &mut iosb, frame.as_ptr(), frame.len() as u32, std::ptr::null(), std::ptr::null());
        close(h);
    }
}

unsafe extern "system" fn handler(ep: *mut EXCEPTION_POINTERS) -> i32 {
    // SAFETY: the OS passes valid exception pointers.
    let (code, addr) = unsafe {
        let rec = (*ep).ExceptionRecord;
        if rec.is_null() {
            return EXCEPTION_CONTINUE_SEARCH;
        }
        ((*rec).ExceptionCode, (*rec).ExceptionAddress as usize)
    };
    let fatal = [EXCEPTION_ACCESS_VIOLATION, EXCEPTION_ILLEGAL_INSTRUCTION, EXCEPTION_STACK_OVERFLOW, EXCEPTION_INT_DIVIDE_BY_ZERO]
        .contains(&code);
    let base = BASE.load(Ordering::Relaxed);
    let ours = base != 0 && addr >= base && addr < base + SIZE.load(Ordering::Relaxed);
    if fatal && ours && code != EXCEPTION_STACK_OVERFLOW && !REPORTED.swap(true, Ordering::SeqCst) {
        report_fault();
    }
    EXCEPTION_CONTINUE_SEARCH
}

pub fn install() {
    let base = super::module_handle() as usize;
    if base == 0 {
        return;
    }
    BASE.store(base, Ordering::Relaxed);
    SIZE.store(image_size(base), Ordering::Relaxed);
    // SAFETY: registers a handler that lives as long as the (pinned) module.
    unsafe {
        AddVectoredExceptionHandler(1, Some(handler));
    }
}
