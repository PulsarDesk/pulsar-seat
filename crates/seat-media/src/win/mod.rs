// SPDX-License-Identifier: GPL-3.0-only
//! Windows implementations. Written from the public CfgMgr32 / SetupAPI / mount manager /
//! Filter Manager / MMDevice / WASAPI documentation (Microsoft Learn). The parts that change
//! the system (descriptors, letters, consent, restarts) are **untested on hardware**: they
//! need SYSTEM rights and seat sessions, and the unit tests never call them.

pub mod acl;
pub mod consent;
pub mod fltport;
pub mod mmdevice;
pub mod notify;
pub mod pnp;
pub mod setupdi;
pub mod spooler;
pub mod wasapi;

/// UTF-16 helpers: one copy, shared with the remote kind's discovery (`seat_audio::cfgmgr`).
pub(crate) use seat_audio::cfgmgr::{from_wide, split_multi_sz, wide};

pub(crate) fn last_error() -> u32 {
    // SAFETY: trivially safe.
    unsafe { windows_sys::Win32::Foundation::GetLastError() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multi_sz() {
        let mut b: Vec<u16> = r"A\1".encode_utf16().collect();
        b.push(0);
        b.extend("B".encode_utf16());
        b.extend([0, 0]);
        assert_eq!(split_multi_sz(&b), vec![r"A\1", "B"]);
        assert_eq!(from_wide(&wide("xy")), "xy");
    }
}
