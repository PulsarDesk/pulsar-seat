// SPDX-License-Identifier: GPL-3.0-only
//! Real Windows implementations of the crate's system traits (`windows` crate).
//!
//! None of these run in the unit tests (they use `fake`). What each one needs:
//!
//! | Type | Trait | Needs |
//! | --- | --- | --- |
//! | [`os::WinConsole`], [`os::os_build`] | `ConsoleSession` | nothing |
//! | [`dxgi::DxgiAdapters`] | `AdapterEnumerator` | nothing (DXGI 1.6 + CfgMgr) |
//! | [`devices::WinSessionProbe`] | `SessionDisplayProbe` | nothing (SetupAPI read-only) |
//! | [`deviceio::WinDeviceIo`] | `DeviceIo` | SYSTEM (device ACL of the own driver) |
//! | [`vdd::WinVddPipe`], [`vdd::WinVddSettingsFile`] | `VddPipe`, `VddSettingsFile` | Administrators / SYSTEM for the file |
//! | [`vdd::WinVddMonitors`], [`vdd::WinModeSetter`] | `VddMonitors`, `ModeSetter` | must run **in the console session** (display config is per session) |
//! | [`journal::FileRestoreLog`] | `RestoreLog` | SYSTEM / Administrators; `%ProgramData%\PulsarSplit` must be SYSTEM + Administrators only (created that way if absent, refused otherwise) |
//! | [`secure::TrustedDir`] | (helper) | folder owner + DACL + reparse checks for the two above |

pub mod deviceio;
pub mod devices;
pub mod dxgi;
pub mod journal;
pub mod os;
pub mod secure;
pub mod vdd;

use windows::core::{Error, PCWSTR};

/// Win32 error code of a `windows` crate error (HRESULT_FROM_WIN32 unwrapped).
pub(crate) fn win32_of(e: &Error) -> u32 {
    let hr = e.code().0 as u32;
    if hr & 0xFFFF_0000 == 0x8007_0000 {
        hr & 0xFFFF
    } else {
        hr
    }
}

/// NUL-terminated UTF-16 copy of a string.
pub(crate) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub(crate) fn pcwstr(v: &[u16]) -> PCWSTR {
    PCWSTR(v.as_ptr())
}

/// UTF-16 buffer up to the first NUL.
pub(crate) fn from_wide(buf: &[u16]) -> String {
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..n])
}

#[cfg(test)]
mod tests {
    //! Read-only smoke test of the real implementations. `#[ignore]`: run by hand with
    //! `cargo test -p seat-display remote -- --ignored`. It changes nothing on the system (no
    //! pipe commands, no file writes, no mode changes).
    use super::*;
    use crate::remote::adapters::AdapterEnumerator;
    use crate::remote::backend::pulsar_idd::DeviceIo;
    use crate::remote::backend::rdp::SessionDisplayProbe;
    use crate::remote::backend::vdd::{ConsoleSession, VddMonitors};

    #[test]
    #[ignore]
    fn read_only_queries() {
        let (build, server) = os::os_version();
        assert!(build > 10_000, "build {build}");
        println!("build {build} server {server}");
        println!("console session {:?}", os::WinConsole.console_session_id());
        for a in dxgi::DxgiAdapters.adapters().unwrap() {
            println!("adapter {:?}", a);
        }
        println!("luid map {:?}", dxgi::luid_to_instance());
        println!(
            "display devices by session {:?}",
            devices::display_devices_by_session().unwrap()
        );
        println!(
            "session 1 has display: {:?}",
            devices::WinSessionProbe.session_has_display(1)
        );
        println!("vdd monitors {:?}", vdd::WinVddMonitors.monitors());
        println!(
            "own driver sessions {:?}",
            deviceio::WinDeviceIo.sessions(&crate::remote::ioctl::PULSAR_IDD_INTERFACE)
        );
        println!(
            "vdd settings path {}",
            vdd::WinVddSettingsFile::discover().path.display()
        );
    }
}
