// SPDX-License-Identifier: GPL-3.0-only
//! Smart-card readers per seat (parity review 2026-11-17; `seat_shim_core::readers`).
//! `SCardListReadersW/A` drop the readers this process's seat does not own;
//! `SCardConnectW/A` to one fails with `SCARD_E_UNKNOWN_READER`. Rules come from the
//! `Shim\ReaderRules` value (re-read at most every 2 s). HYPOTHESIS: never run on
//! hardware.

use super::hook::{Hook, guarded, install as hook};
use seat_shim_core::readers::{ReaderRule, SCARD_E_UNKNOWN_READER, filter_multi, reader_visible};
use std::ffi::c_void;
use std::sync::Mutex;

static LIST_W: Hook = Hook::new();
static LIST_A: Hook = Hook::new();
static CONNECT_W: Hook = Hook::new();
static CONNECT_A: Hook = Hook::new();

/// `SCARD_AUTOALLOCATE`: the buffer argument is a pointer to a pointer WinSCard fills.
const AUTOALLOCATE: u32 = u32::MAX;

static RULES: Mutex<(u64, Vec<ReaderRule>)> = Mutex::new((0, Vec::new()));

fn rules() -> Vec<ReaderRule> {
    use windows_sys::Win32::System::SystemInformation::GetTickCount64;
    // SAFETY: plain call.
    let now = unsafe { GetTickCount64() };
    let mut g = RULES.lock().unwrap_or_else(|e| e.into_inner());
    if g.0 == 0 || now.saturating_sub(g.0) >= 2000 {
        g.1 = seat_shim_core::win::read_reader_rules();
        g.0 = now.max(1);
    }
    g.1.clone()
}

/// May this process see the reader?
fn visible(name: &str) -> bool {
    let Some((role, plan, _, _)) = super::live() else { return true };
    if !plan.scard_filter {
        return true;
    }
    let session = super::state().map_or(0, |s| s.facts.session_id);
    reader_visible(name, &role, session, &rules())
}

type ListW = unsafe extern "system" fn(usize, *const u16, *mut u16, *mut u32) -> i32;
type ListA = unsafe extern "system" fn(usize, *const u8, *mut u8, *mut u32) -> i32;
type ConnectW = unsafe extern "system" fn(usize, *const u16, u32, u32, *mut usize, *mut u32) -> i32;
type ConnectA = unsafe extern "system" fn(usize, *const u8, u32, u32, *mut usize, *mut u32) -> i32;

/// Filters the returned list in place (the caller's buffer or the auto-allocated one).
///
/// # Safety
/// `buf` / `len` as returned by a successful real call.
unsafe fn filter<T: Copy + PartialEq + Default>(buf: *mut c_void, len: *mut u32, as_str: &dyn Fn(&[T]) -> String) {
    if buf.is_null() || len.is_null() {
        return;
    }
    let _ = guarded(|| {
        // SAFETY: the real call returned `*len` elements at `buf` (or a pointer to them).
        unsafe {
            let auto = *len == AUTOALLOCATE;
            let data = if auto { *(buf as *mut *mut T) } else { buf as *mut T };
            let n = *len as usize;
            if data.is_null() || n == 0 || n == AUTOALLOCATE as usize {
                return;
            }
            let s = std::slice::from_raw_parts_mut(data, n);
            let kept = filter_multi(s, &|item: &[T]| visible(&as_str(item)));
            *len = kept as u32;
        }
    });
}

unsafe extern "system" fn list_w(ctx: usize, groups: *const u16, readers: *mut u16, len: *mut u32) -> i32 {
    // SAFETY: trampoline has SCardListReadersW's signature.
    let r = unsafe {
        let f: ListW = std::mem::transmute(LIST_W.trampoline());
        f(ctx, groups, readers, len)
    };
    if r == 0 {
        // SAFETY: successful call: buffer and length are valid.
        unsafe { filter::<u16>(readers.cast(), len, &|s: &[u16]| String::from_utf16_lossy(s)) };
    }
    r
}

unsafe extern "system" fn list_a(ctx: usize, groups: *const u8, readers: *mut u8, len: *mut u32) -> i32 {
    // SAFETY: trampoline has SCardListReadersA's signature.
    let r = unsafe {
        let f: ListA = std::mem::transmute(LIST_A.trampoline());
        f(ctx, groups, readers, len)
    };
    if r == 0 {
        // SAFETY: as above.
        unsafe { filter::<u8>(readers.cast(), len, &|s: &[u8]| String::from_utf8_lossy(s).into_owned()) };
    }
    r
}

/// The reader name of a connect call (NUL-terminated).
fn name_w(p: *const u16) -> Option<String> {
    guarded(|| {
        if p.is_null() {
            return None;
        }
        // SAFETY: NUL-terminated string from the caller.
        unsafe {
            let n = (0..4096).position(|i| *p.add(i) == 0)?;
            Some(String::from_utf16_lossy(std::slice::from_raw_parts(p, n)))
        }
    })
    .flatten()
}

fn name_a(p: *const u8) -> Option<String> {
    guarded(|| {
        if p.is_null() {
            return None;
        }
        // SAFETY: as above.
        unsafe {
            let n = (0..4096).position(|i| *p.add(i) == 0)?;
            Some(String::from_utf8_lossy(std::slice::from_raw_parts(p, n)).into_owned())
        }
    })
    .flatten()
}

unsafe extern "system" fn connect_w(ctx: usize, reader: *const u16, share: u32, proto: u32, card: *mut usize, active: *mut u32) -> i32 {
    if name_w(reader).is_some_and(|n| !visible(&n)) {
        return SCARD_E_UNKNOWN_READER;
    }
    // SAFETY: trampoline has SCardConnectW's signature.
    unsafe {
        let f: ConnectW = std::mem::transmute(CONNECT_W.trampoline());
        f(ctx, reader, share, proto, card, active)
    }
}

unsafe extern "system" fn connect_a(ctx: usize, reader: *const u8, share: u32, proto: u32, card: *mut usize, active: *mut u32) -> i32 {
    if name_a(reader).is_some_and(|n| !visible(&n)) {
        return SCARD_E_UNKNOWN_READER;
    }
    // SAFETY: trampoline has SCardConnectA's signature.
    unsafe {
        let f: ConnectA = std::mem::transmute(CONNECT_A.trampoline());
        f(ctx, reader, share, proto, card, active)
    }
}

/// # Safety
/// Init path only.
pub unsafe fn install() -> bool {
    // SAFETY: detours match the targets' signatures (WinSCard handles are pointer-sized).
    unsafe {
        let a = hook(&LIST_W, "winscard.dll", "SCardListReadersW", list_w as *const ());
        let b = hook(&LIST_A, "winscard.dll", "SCardListReadersA", list_a as *const ());
        let c = hook(&CONNECT_W, "winscard.dll", "SCardConnectW", connect_w as *const ());
        let d = hook(&CONNECT_A, "winscard.dll", "SCardConnectA", connect_a as *const ());
        a && b && c && d
    }
}
