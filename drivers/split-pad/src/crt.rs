// SPDX-License-Identifier: GPL-3.0-only
//! `memcpy`, `memmove`, `memset` and `memcmp` for the CRT-free driver DLL (see build.rs).
//! Byte loops through volatile accesses, so LLVM cannot turn a loop back into a call to
//! the function itself. The driver moves at most a few hundred bytes per call (HID
//! reports, descriptors), so speed does not matter here.

use core::ffi::{c_int, c_void};
use core::ptr::{read_volatile, write_volatile};

/// # Safety
/// C `memcpy` contract: `n` bytes valid at both pointers, no overlap.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn memcpy(dest: *mut c_void, src: *const c_void, n: usize) -> *mut c_void {
    let (d, s) = (dest.cast::<u8>(), src.cast::<u8>());
    let mut i = 0;
    while i < n {
        // SAFETY: caller guarantees both ranges.
        unsafe { write_volatile(d.add(i), read_volatile(s.add(i))) };
        i += 1;
    }
    dest
}

/// # Safety
/// C `memmove` contract: `n` bytes valid at both pointers; they may overlap.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn memmove(dest: *mut c_void, src: *const c_void, n: usize) -> *mut c_void {
    let (d, s) = (dest.cast::<u8>(), src.cast::<u8>());
    if (d as usize) <= (s as usize) {
        let mut i = 0;
        while i < n {
            // SAFETY: caller guarantees both ranges; forward copy is safe for dest <= src.
            unsafe { write_volatile(d.add(i), read_volatile(s.add(i))) };
            i += 1;
        }
    } else {
        let mut i = n;
        while i > 0 {
            i -= 1;
            // SAFETY: as above; backward copy is safe for dest > src.
            unsafe { write_volatile(d.add(i), read_volatile(s.add(i))) };
        }
    }
    dest
}

/// # Safety
/// C `memset` contract: `n` bytes writable at `dest`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn memset(dest: *mut c_void, c: c_int, n: usize) -> *mut c_void {
    let d = dest.cast::<u8>();
    let mut i = 0;
    while i < n {
        // SAFETY: caller guarantees the range.
        unsafe { write_volatile(d.add(i), c as u8) };
        i += 1;
    }
    dest
}

/// # Safety
/// C `memcmp` contract: `n` bytes readable at both pointers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn memcmp(a: *const c_void, b: *const c_void, n: usize) -> c_int {
    let (a, b) = (a.cast::<u8>(), b.cast::<u8>());
    let mut i = 0;
    while i < n {
        // SAFETY: caller guarantees both ranges.
        let (x, y) = unsafe { (read_volatile(a.add(i)), read_volatile(b.add(i))) };
        if x != y {
            return i32::from(x) - i32::from(y);
        }
        i += 1;
    }
    0
}