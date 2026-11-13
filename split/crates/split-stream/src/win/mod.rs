// SPDX-License-Identifier: GPL-3.0-only
//! Real Windows implementations of this crate's system traits.
//!
//! None of these is exercised by the unit tests (they would change the live system or need
//! SYSTEM privileges); they are compiled for aarch64 and x86_64 and reviewed against the
//! Microsoft Learn documentation of each API. See the crate README for what is untested.

pub mod acl;
pub mod connector;
pub mod firewall;
pub mod fs;
pub mod host_pipe;
pub mod launch;
pub mod pipe;
pub mod ports;
pub mod random;

pub use connector::WinConnector;
pub use firewall::ComFirewall;
pub use fs::WinSecureFs;
pub use host_pipe::WinHostPipes;
pub use launch::SessionLauncher;
pub use pipe::{connect, PipeClient};
pub use ports::IpHelperProbe;
pub use random::SystemRandom;

use crate::StreamError;
use std::io;
use std::ptr;
use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, HANDLE, HLOCAL, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};

pub(crate) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub(crate) fn wide_os(s: &std::ffi::OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    s.encode_wide().chain(std::iter::once(0)).collect()
}

pub(crate) fn os_err(context: &str) -> StreamError {
    StreamError::io(context, io::Error::last_os_error())
}

/// Owned kernel handle.
pub struct OwnedHandle(pub(crate) HANDLE);

// Kernel handles may be used from any thread.
unsafe impl Send for OwnedHandle {}
unsafe impl Sync for OwnedHandle {}

impl OwnedHandle {
    pub(crate) fn new(h: HANDLE) -> Option<Self> {
        if h.is_null() || h == INVALID_HANDLE_VALUE { None } else { Some(Self(h)) }
    }
    pub fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: we own the handle.
        unsafe { CloseHandle(self.0) };
    }
}

/// Self-relative security descriptor from SDDL (freed with `LocalFree`).
pub struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

unsafe impl Send for SecurityDescriptor {}
unsafe impl Sync for SecurityDescriptor {}

impl SecurityDescriptor {
    pub fn from_sddl(sddl: &str) -> Result<Self, StreamError> {
        let w = wide(sddl);
        let mut sd: PSECURITY_DESCRIPTOR = ptr::null_mut();
        // SAFETY: NUL-terminated input, valid out pointer.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(w.as_ptr(), SDDL_REVISION_1, &mut sd, ptr::null_mut())
        };
        if ok == 0 {
            return Err(os_err("parse SDDL"));
        }
        Ok(Self(sd))
    }

    pub fn as_ptr(&self) -> PSECURITY_DESCRIPTOR {
        self.0
    }

    pub fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.0,
            bInheritHandle: 0,
        }
    }
}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
        unsafe { LocalFree(self.0 as HLOCAL) };
    }
}
