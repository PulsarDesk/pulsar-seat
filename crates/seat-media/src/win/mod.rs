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

pub(crate) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Split a REG_MULTI_SZ-style UTF-16 buffer.
pub(crate) fn split_multi_sz(buf: &[u16]) -> Vec<String> {
    buf.split(|&c| c == 0).take_while(|s| !s.is_empty()).map(String::from_utf16_lossy).collect()
}

/// UTF-16 up to the first NUL.
pub(crate) fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

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
