// SPDX-License-Identifier: GPL-3.0-only
//! Real Windows implementations of the system traits. Nothing here runs in unit tests:
//! tests use `crate::fake`. All of it is compiled on the dev box (ARM64) and checked for
//! x86_64; none of it was executed against real drivers (see README "Untested").

pub mod channel;
pub mod devtree;
pub mod fsacl;
pub mod process;
pub mod services;
pub mod swdevice;
pub mod usbip;
pub mod vigem;

use windows_sys::core::GUID;

pub use channel::WinChannelFactory;
pub use devtree::{CmDeviceTree, DeviceArrivalWatch};
pub use process::{current_process_identity, WinProcessProbe};
pub use services::{file_version, service_facts, ServiceFacts, WinUserServices};
pub use swdevice::WinSwDevice;
pub use usbip::{WinConsoleSession, WinPeerAuth, WinUsbipTool};
pub use vigem::WinVigemDriver;

/// NUL-terminated UTF-16.
pub(crate) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Double-NUL-terminated UTF-16 list (REG_MULTI_SZ / `pszz…`).
pub(crate) fn multi_sz(items: &[String]) -> Vec<u16> {
    let mut v = Vec::new();
    for i in items {
        v.extend(i.encode_utf16());
        v.push(0);
    }
    v.push(0);
    v
}

/// Splits a double-NUL-terminated UTF-16 buffer.
pub(crate) fn split_multi_sz(buf: &[u16]) -> Vec<String> {
    buf.split(|&c| c == 0).filter(|s| !s.is_empty()).map(String::from_utf16_lossy).collect()
}

pub(crate) fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

pub(crate) fn guid((d1, d2, d3, d4): (u32, u16, u16, [u8; 8])) -> GUID {
    GUID { data1: d1, data2: d2, data3: d3, data4: d4 }
}

pub(crate) fn last_error() -> u32 {
    // SAFETY: trivial FFI.
    unsafe { windows_sys::Win32::Foundation::GetLastError() }
}

/// Owned kernel handle, closed on drop.
pub(crate) struct Handle(pub windows_sys::Win32::Foundation::HANDLE);

// SAFETY: kernel handles may be used from any thread.
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}

impl Handle {
    pub fn valid(&self) -> bool {
        !self.0.is_null() && self.0 != windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        if self.valid() {
            // SAFETY: we own the handle.
            unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0) };
        }
    }
}

/// Security descriptor parsed from SDDL, freed with `LocalFree`.
pub(crate) struct Sd(pub *mut core::ffi::c_void);

impl Sd {
    pub fn from_sddl(sddl: &str) -> Result<Self, crate::PadError> {
        use windows_sys::Win32::Security::Authorization::{
            ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
        };
        let w = wide(sddl);
        let mut sd: *mut core::ffi::c_void = std::ptr::null_mut();
        // SAFETY: valid NUL-terminated string, out pointer valid.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(w.as_ptr(), SDDL_REVISION_1, &mut sd, std::ptr::null_mut())
        };
        if ok == 0 {
            return Err(crate::PadError::os("ConvertStringSecurityDescriptorToSecurityDescriptorW", last_error()));
        }
        Ok(Sd(sd))
    }
}

impl Drop for Sd {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
            unsafe { windows_sys::Win32::Foundation::LocalFree(self.0) };
        }
    }
}

/// `QueryPerformanceCounter`.
pub fn qpc() -> u64 {
    let mut v = 0i64;
    // SAFETY: out pointer valid.
    unsafe { windows_sys::Win32::System::Performance::QueryPerformanceCounter(&mut v) };
    v as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_helpers() {
        assert_eq!(wide("ab"), vec![97, 98, 0]);
        let m = multi_sz(&["a".into(), "bc".into()]);
        assert_eq!(m, vec![97, 0, 98, 99, 0, 0]);
        assert_eq!(split_multi_sz(&m), vec!["a".to_string(), "bc".to_string()]);
        assert_eq!(from_wide(&[104, 105, 0, 120]), "hi");
    }

    /// Pure SDDL parse (no system state touched).
    #[test]
    fn sddl_parses() {
        assert!(Sd::from_sddl(&crate::ids::pad_sddl("S-1-5-21-1-2-3-1001")).is_ok());
        assert!(Sd::from_sddl("not sddl").is_err());
    }
}
