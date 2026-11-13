// SPDX-License-Identifier: GPL-3.0-only
//! Real Windows implementations of the system traits. Nothing here runs in unit tests;
//! every function needs a live system (most of them SYSTEM in session 0).

// Edition, RDS probe, registry and WMI are the shared ones in `crate::win` (merge step 7).
pub mod accounts;
pub mod files;
pub mod helper;
pub mod launch;
pub mod process;
pub mod wts;

use crate::remote::SessionError;
use std::os::windows::io::{FromRawHandle, OwnedHandle};
use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};

/// NUL-terminated UTF-16.
pub(crate) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// NUL-terminated UTF-16 that is wiped on drop (passwords).
pub(crate) fn wide_secret(s: &str) -> zeroize::Zeroizing<Vec<u16>> {
    zeroize::Zeroizing::new(wide(s))
}

/// Reads a NUL-terminated UTF-16 string.
///
/// # Safety
/// `p` is null or points to a NUL-terminated UTF-16 string.
pub(crate) unsafe fn from_wide_ptr(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut n = 0usize;
    // SAFETY: caller guarantees NUL termination.
    unsafe {
        while *p.add(n) != 0 {
            n += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(p, n))
    }
}

/// Takes ownership of a raw handle returned by an API (null / INVALID = error).
pub(crate) fn owned(h: HANDLE, api: &'static str) -> Result<OwnedHandle, SessionError> {
    if h.is_null() || h == INVALID_HANDLE_VALUE {
        return Err(SessionError::last_win32(api));
    }
    // SAFETY: the API just returned this handle and nobody else owns it.
    Ok(unsafe { OwnedHandle::from_raw_handle(h as _) })
}

pub(crate) fn raw(h: &OwnedHandle) -> HANDLE {
    std::os::windows::io::AsRawHandle::as_raw_handle(h) as HANDLE
}

/// Maps a `NET_API_STATUS` / Win32 return code.
pub(crate) fn check(rc: u32, api: &'static str) -> Result<(), SessionError> {
    if rc == 0 { Ok(()) } else { Err(SessionError::Win32 { api, code: rc }) }
}
