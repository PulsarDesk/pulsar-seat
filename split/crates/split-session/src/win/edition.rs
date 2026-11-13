// SPDX-License-Identifier: GPL-3.0-only
//! Real edition probe: registry values + `termsrv.dll` VS_FIXEDFILEINFO + RDSH role (WMI).

use super::wide;
use crate::SessionError;
use crate::edition::{EditionInfo, EditionProbe, FileVersion, edition_from_registry};
use crate::rds::Registry;
use std::sync::Arc;
use windows_sys::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VS_FIXEDFILEINFO, VerQueryValueW};
use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;

pub struct WindowsEditionProbe {
    reg: Arc<dyn Registry>,
}

impl WindowsEditionProbe {
    pub fn new(reg: Arc<dyn Registry>) -> Self {
        Self { reg }
    }
}

pub(crate) fn system32() -> Option<String> {
    let mut buf = [0u16; 260];
    // SAFETY: buffer length passed.
    let n = unsafe { GetSystemDirectoryW(buf.as_mut_ptr(), buf.len() as u32) } as usize;
    (n > 0 && n < buf.len()).then(|| String::from_utf16_lossy(&buf[..n]))
}

/// Fixed file version of a binary (never the string resource, spec 30 §2.2).
pub fn file_version(path: &str) -> Option<FileVersion> {
    let p = wide(path);
    let mut h = 0u32;
    // SAFETY: valid NUL-terminated path.
    let size = unsafe { GetFileVersionInfoSizeW(p.as_ptr(), &mut h) };
    if size == 0 {
        return None;
    }
    let mut data = vec![0u8; size as usize];
    // SAFETY: buffer of `size` bytes.
    if unsafe { GetFileVersionInfoW(p.as_ptr(), 0, size, data.as_mut_ptr().cast()) } == 0 {
        return None;
    }
    let root = wide("\\");
    let mut ptr: *mut core::ffi::c_void = std::ptr::null_mut();
    let mut len = 0u32;
    // SAFETY: `data` holds the version block; the returned pointer points into it.
    if unsafe { VerQueryValueW(data.as_ptr().cast(), root.as_ptr(), &mut ptr, &mut len) } == 0
        || ptr.is_null()
        || (len as usize) < std::mem::size_of::<VS_FIXEDFILEINFO>()
    {
        return None;
    }
    // SAFETY: checked size; read unaligned to be safe.
    let fi: VS_FIXEDFILEINFO = unsafe { std::ptr::read_unaligned(ptr.cast()) };
    if fi.dwSignature != 0xFEEF_04BD {
        return None;
    }
    Some(FileVersion::from_ms_ls(fi.dwFileVersionMS, fi.dwFileVersionLS))
}

impl EditionProbe for WindowsEditionProbe {
    fn edition(&self) -> Result<EditionInfo, SessionError> {
        let tv = system32().and_then(|d| file_version(&format!(r"{d}\termsrv.dll")));
        edition_from_registry(self.reg.as_ref(), tv)
    }

    /// `Win32_TerminalServiceSetting.TerminalServerMode` = 1 means the RD Session Host
    /// (application server) role is installed.
    fn rdsh_role_installed(&self) -> Result<bool, SessionError> {
        let w = super::wmi::Wmi::connect(r"root\cimv2\TerminalServices")?;
        let row = w.query_first("SELECT TerminalServerMode FROM Win32_TerminalServiceSetting", &["TerminalServerMode"])?;
        Ok(row.and_then(|r| r[0].as_u32()) == Some(1))
    }
}
