// SPDX-License-Identifier: GPL-3.0-only
//! Handle broker (OSMS-parity G1, in-session capture): moves the capture handles the seat's
//! session agent offered (shared textures, mailbox section, frame event) from the agent
//! process into the seat host. The host is contained (own account, low integrity) and can
//! open neither the agent nor its objects; the service, as SYSTEM, opens both processes
//! with `PROCESS_DUP_HANDLE` and calls `DuplicateHandle` (Microsoft Learn,
//! "DuplicateHandle function"). Nothing else: no object is created or changed here, so
//! nothing needs a journal (the handles die with the processes).
//!
//! Access: textures keep the agent's access (the presenter opens them with
//! `OpenSharedResource1` and uses the keyed mutex), the mailbox becomes read-only
//! (`FILE_MAP_READ`), the event wait-only (`SYNCHRONIZE`).
//!
//! **Untested on hardware.**

use crate::platform::HandleAccess;
use seat_core::backend::{BackendError, BackendResult, ErrorKind};
use std::ptr;
use windows_sys::Win32::Foundation::{CloseHandle, DUPLICATE_CLOSE_SOURCE, DUPLICATE_SAME_ACCESS, DuplicateHandle, GetLastError, HANDLE};
use windows_sys::Win32::Storage::FileSystem::SYNCHRONIZE;
use windows_sys::Win32::System::Memory::FILE_MAP_READ;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcess, PROCESS_DUP_HANDLE};

/// Most handles one call moves (4 outputs x 3 textures + mailbox + event).
pub const MAX_HANDLES: usize = 16;

struct Proc(HANDLE);
impl Drop for Proc {
    fn drop(&mut self) {
        // SAFETY: a process handle this module opened.
        unsafe { CloseHandle(self.0) };
    }
}

fn os_err(what: &str) -> BackendError {
    // SAFETY: trivially safe.
    let e = unsafe { GetLastError() };
    BackendError::new(ErrorKind::Os, "handle_broker_failed", format!("{what} failed ({e})"))
}

fn open(pid: u32) -> BackendResult<Proc> {
    // SAFETY: plain open; closed by `Proc`.
    let h = unsafe { OpenProcess(PROCESS_DUP_HANDLE, 0, pid) };
    if h.is_null() {
        return Err(os_err("OpenProcess"));
    }
    Ok(Proc(h))
}

/// `(desired access, options)` for `DuplicateHandle`.
fn access(a: HandleAccess) -> (u32, u32) {
    match a {
        HandleAccess::Same => (0, DUPLICATE_SAME_ACCESS),
        HandleAccess::MapRead => (FILE_MAP_READ, 0),
        HandleAccess::Synchronize => (SYNCHRONIZE, 0),
    }
}

/// See [`crate::platform::Platform::duplicate_handles`].
pub fn duplicate(from_pid: u32, to_pid: u32, handles: &[(u64, HandleAccess)]) -> BackendResult<Vec<u64>> {
    if handles.len() > MAX_HANDLES {
        return Err(BackendError::new(ErrorKind::Internal, "too_many_handles", format!("{} handles", handles.len())));
    }
    let (from, to) = (open(from_pid)?, open(to_pid)?);
    let mut made: Vec<u64> = Vec::with_capacity(handles.len());
    for &(h, a) in handles {
        let (desired, options) = access(a);
        let mut out: HANDLE = ptr::null_mut();
        // SAFETY: process handles opened above; `h` is a value in the source process (an
        // invalid one only makes the call fail); out pointer valid.
        let ok = unsafe { DuplicateHandle(from.0, h as usize as HANDLE, to.0, &mut out, desired, 0, options) };
        if ok == 0 {
            let e = os_err("DuplicateHandle");
            // Undo: close what this call already put into the target.
            for v in &made {
                // SAFETY: closing a handle we created in the target process (source =
                // target process, DUPLICATE_CLOSE_SOURCE, no new handle made here).
                unsafe { DuplicateHandle(to.0, *v as usize as HANDLE, GetCurrentProcess(), ptr::null_mut(), 0, 0, DUPLICATE_CLOSE_SOURCE) };
            }
            return Err(e);
        }
        made.push(out as usize as u64);
    }
    Ok(made)
}
