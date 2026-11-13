// SPDX-License-Identifier: GPL-3.0-only
//! Process liveness by (pid, creation time), so a reused pid is never mistaken for the
//! creator of a pad.

use windows_sys::Win32::Foundation::FILETIME;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, GetExitCodeProcess, GetProcessTimes, OpenProcess,
    PROCESS_QUERY_LIMITED_INFORMATION,
};

use super::Handle;
use crate::owners::ProcessProbe;

const STILL_ACTIVE: u32 = 0x103;

fn creation_time(h: windows_sys::Win32::Foundation::HANDLE) -> Option<u64> {
    let mut c = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    let mut e = c;
    let mut k = c;
    let mut u = c;
    // SAFETY: valid handle and out pointers.
    let ok = unsafe { GetProcessTimes(h, &mut c, &mut e, &mut k, &mut u) };
    (ok != 0).then_some(((c.dwHighDateTime as u64) << 32) | c.dwLowDateTime as u64)
}

/// (pid, creation FILETIME) of this process.
pub fn current_process_identity() -> (u32, u64) {
    // SAFETY: pseudo handle, never closed.
    let (pid, h) = unsafe { (GetCurrentProcessId(), GetCurrentProcess()) };
    (pid, creation_time(h).unwrap_or(0))
}

pub struct WinProcessProbe;

impl ProcessProbe for WinProcessProbe {
    fn alive(&self, pid: u32, start_time: u64) -> bool {
        // SAFETY: plain FFI; the handle is owned by `Handle`.
        let h = Handle(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) });
        if !h.valid() {
            return false;
        }
        let mut code = 0u32;
        // SAFETY: valid handle.
        if unsafe { GetExitCodeProcess(h.0, &mut code) } == 0 || code != STILL_ACTIVE {
            return false;
        }
        creation_time(h.0) == Some(start_time)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reads only this test process (no system change).
    #[test]
    fn self_is_alive() {
        let (pid, t) = current_process_identity();
        assert!(t > 0);
        assert!(WinProcessProbe.alive(pid, t));
        assert!(!WinProcessProbe.alive(pid, t + 1));
    }
}
