// SPDX-License-Identifier: GPL-3.0-only
//! Per-seat desktop objects for the desktops backend's per-seat mode (runs inside the
//! console session, in `pulsar-seat-host --desktop-keeper`). A desktop lives as long as a
//! handle to it is open, so the keeper holds it until its job is closed.
//! **Untested on hardware.**

use windows_sys::Win32::Foundation::{HLOCAL, LocalFree};
use windows_sys::Win32::Security::Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows_sys::Win32::System::StationsAndDesktops::{CloseDesktop, CreateDesktopW, DESKTOP_SWITCHDESKTOP, HDESK, OpenDesktopW, SwitchDesktop};

use super::wide;
use crate::error::SysError;

/// `GENERIC_ALL` for SYSTEM and the desktop's owner (the creating user).
pub const DESKTOP_SDDL: &str = "D:P(A;;GA;;;SY)(A;;GA;;;OW)";

/// An open desktop; closed on drop.
pub struct SeatDesktop(HDESK);

// SAFETY: a desktop handle may be used and closed from any thread of the process.
unsafe impl Send for SeatDesktop {}

impl Drop for SeatDesktop {
    fn drop(&mut self) {
        // SAFETY: handle from CreateDesktopW / OpenDesktopW.
        unsafe { CloseDesktop(self.0) };
    }
}

impl SeatDesktop {
    /// `CreateDesktopW(name)` in the caller's window station with [`DESKTOP_SDDL`].
    pub fn create(name: &str) -> Result<Self, SysError> {
        let sddl = wide(DESKTOP_SDDL);
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: valid SDDL; freed below.
        if unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), SDDL_REVISION_1, &mut sd, std::ptr::null_mut()) } == 0 {
            return Err(SysError::last_win32("ConvertStringSecurityDescriptorToSecurityDescriptorW"));
        }
        let sa = SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: sd, bInheritHandle: 0 };
        let n = wide(name);
        const GENERIC_ALL: u32 = 0x1000_0000;
        // SAFETY: valid name and attributes.
        let h = unsafe { CreateDesktopW(n.as_ptr(), std::ptr::null(), std::ptr::null(), 0, GENERIC_ALL, &sa) };
        // SAFETY: allocated above.
        unsafe { LocalFree(sd as HLOCAL) };
        if h.is_null() {
            return Err(SysError::last_win32("CreateDesktopW"));
        }
        Ok(Self(h))
    }

    /// Makes this desktop the input desktop (shown on every monitor).
    pub fn switch_to(&self) -> Result<(), SysError> {
        // SAFETY: valid handle.
        if unsafe { SwitchDesktop(self.0) } == 0 {
            return Err(SysError::last_win32("SwitchDesktop"));
        }
        Ok(())
    }
}

/// Switches back to `Default` (seat 1's desktop).
pub fn switch_to_default() -> Result<(), SysError> {
    let n = wide("Default");
    // SAFETY: plain open; closed below.
    let h = unsafe { OpenDesktopW(n.as_ptr(), 0, 0, DESKTOP_SWITCHDESKTOP) };
    if h.is_null() {
        return Err(SysError::last_win32("OpenDesktopW"));
    }
    // SAFETY: valid handle with DESKTOP_SWITCHDESKTOP.
    let ok = unsafe { SwitchDesktop(h) } != 0;
    let e = SysError::last_win32("SwitchDesktop");
    // SAFETY: opened above.
    unsafe { CloseDesktop(h) };
    if ok { Ok(()) } else { Err(e) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_and_close_private_desktop() {
        // Creating (not switching to) a desktop in our own window station changes
        // nothing visible; it vanishes when the handle closes.
        let d = SeatDesktop::create(&format!("PulsarSeat.Test.{}", std::process::id()));
        if let Ok(d) = d {
            drop(d);
        }
    }
}
