// SPDX-License-Identifier: GPL-3.0-only
//! OS version and console session.

use windows::Wdk::System::SystemServices::RtlGetVersion;
use windows::Win32::System::RemoteDesktop::WTSGetActiveConsoleSessionId;
use windows::Win32::System::SystemInformation::{OSVERSIONINFOEXW, OSVERSIONINFOW};

use crate::remote::backend::vdd::ConsoleSession;

/// `VER_NT_WORKSTATION`.
const VER_NT_WORKSTATION: u8 = 1;

/// (build number, is Windows Server) from `RtlGetVersion` (not subject to manifest lies).
pub fn os_version() -> (u32, bool) {
    let mut v = OSVERSIONINFOEXW {
        dwOSVersionInfoSize: size_of::<OSVERSIONINFOEXW>() as u32,
        ..Default::default()
    };
    // SAFETY: OSVERSIONINFOEXW starts with OSVERSIONINFOW and its size field says which
    // one is passed, as documented for RtlGetVersion.
    let st = unsafe { RtlGetVersion(&mut v as *mut OSVERSIONINFOEXW as *mut OSVERSIONINFOW) };
    if st.is_err() {
        return (0, false);
    }
    (v.dwBuildNumber, v.wProductType != VER_NT_WORKSTATION)
}

pub fn os_build() -> u32 {
    os_version().0
}

/// `WTSGetActiveConsoleSessionId`.
#[derive(Debug, Default, Clone, Copy)]
pub struct WinConsole;

impl ConsoleSession for WinConsole {
    fn console_session_id(&self) -> Option<u32> {
        // SAFETY: no arguments; returns 0xFFFFFFFF when no session is attached.
        let id = unsafe { WTSGetActiveConsoleSessionId() };
        (id != u32::MAX).then_some(id)
    }
}
