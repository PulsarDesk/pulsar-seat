// SPDX-License-Identifier: GPL-3.0-only
//! Named global events between the service (session 0) and helpers in other sessions
//! (the desktop keeper). The service creates the event with a DACL that lets SYSTEM and
//! interactive users signal it; the helper only opens and sets it. **Untested on hardware.**

use std::os::windows::io::OwnedHandle;
use std::time::Duration;

use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError, HLOCAL, LocalFree, WAIT_OBJECT_0};
use windows_sys::Win32::Security::Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows_sys::Win32::System::Threading::{CreateEventW, OpenEventW, SetEvent, WaitForSingleObject};

use super::{owned, raw, wide};
use crate::backends::desktops::{ArmedEvent, EventWaiter};
use crate::error::SysError;

/// SYSTEM full; interactive users `SYNCHRONIZE | EVENT_MODIFY_STATE`.
pub const EVENT_SDDL: &str = "D:(A;;GA;;;SY)(A;;0x100002;;;IU)";
const EVENT_MODIFY_STATE: u32 = 0x0002;

struct Armed(OwnedHandle);

impl ArmedEvent for Armed {
    fn wait(&self, timeout: Duration) -> bool {
        let ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
        // SAFETY: valid event handle.
        unsafe { WaitForSingleObject(raw(&self.0), ms) == WAIT_OBJECT_0 }
    }
}

/// Real event waiter (service side).
#[derive(Debug, Clone, Copy, Default)]
pub struct WindowsEvents;

impl EventWaiter for WindowsEvents {
    fn arm(&self, name: &str) -> Result<Box<dyn ArmedEvent>, SysError> {
        let sddl = wide(EVENT_SDDL);
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: valid SDDL; sd freed below.
        if unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), SDDL_REVISION_1, &mut sd, std::ptr::null_mut()) } == 0 {
            return Err(SysError::last_win32("ConvertStringSecurityDescriptorToSecurityDescriptorW"));
        }
        let sa = SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: sd, bInheritHandle: 0 };
        let n = wide(name);
        // SAFETY: manual-reset, non-signalled event.
        let h = unsafe { CreateEventW(&sa, 1, 0, n.as_ptr()) };
        // SAFETY: no preconditions; read right after CreateEventW.
        let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        // SAFETY: allocated by the conversion above.
        unsafe { LocalFree(sd as HLOCAL) };
        let h = owned(h, "CreateEventW")?;
        if existed {
            // Someone else created it first (squatting: their DACL, maybe pre-signalled).
            // Our SD was not applied, so never trust it.
            return Err(SysError::Win32 { api: "CreateEventW (name already exists)", code: ERROR_ALREADY_EXISTS });
        }
        Ok(Box::new(Armed(h)))
    }
}

/// Helper side: open the event the service created and set it.
pub fn signal(name: &str) -> Result<(), SysError> {
    let n = wide(name);
    // SAFETY: plain open.
    let h = owned(unsafe { OpenEventW(EVENT_MODIFY_STATE, 0, n.as_ptr()) }, "OpenEventW")?;
    // SAFETY: valid handle with EVENT_MODIFY_STATE.
    if unsafe { SetEvent(raw(&h)) } == 0 {
        return Err(SysError::last_win32("SetEvent"));
    }
    Ok(())
}

/// Kiosk shell stop request (service side): opens the shell's own event in the console
/// session (`Session\<id>\PulsarSeat.KioskStop.NN`, created by the shell with the console
/// user's default DACL, which grants SYSTEM full access) and sets it. **Untested on
/// hardware** (the `Session\<id>\` prefix from session 0 is a **HYPOTHESIS** to verify).
#[derive(Debug, Clone, Copy, Default)]
pub struct WindowsShellStop;

impl crate::backends::desktops::ShellStopper for WindowsShellStop {
    fn request_stop(&self, session_id: u32, seat: seat_core::SeatNumber) -> bool {
        signal(&crate::kiosk::session_stop_event(session_id, seat)).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_event_round_trip() {
        // Session-local name: no Global\ privilege needed for the test.
        let name = format!(r"Local\PulsarSeat.Test.{}", std::process::id());
        let a = WindowsEvents.arm(&name).unwrap();
        assert!(!a.wait(Duration::from_millis(1)));
        signal(&name).unwrap();
        assert!(a.wait(Duration::from_millis(100)));
        // A name that already exists (squatted, maybe pre-signalled) is refused.
        assert!(WindowsEvents.arm(&name).is_err());
        drop(a);
        assert!(WindowsEvents.arm(&name).is_ok());
    }

    #[test]
    fn shell_stop_without_a_shell_is_refused() {
        use crate::backends::desktops::ShellStopper;
        // No shell event exists for seat 16 in session 0: nothing to set.
        assert!(!WindowsShellStop.request_stop(0, seat_core::SeatNumber::new(16).unwrap()));
    }
}
