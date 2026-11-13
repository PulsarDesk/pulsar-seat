// SPDX-License-Identifier: GPL-3.0-only
//! Security descriptors from SDDL (pipe ACL, secrets registry key).

use std::ffi::c_void;
use std::io;
use std::ptr;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    DACL_SECURITY_INFORMATION, GetTokenInformation, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
    SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::System::Registry::RegSetKeySecurity;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// Control pipe DACL: protected, SYSTEM and built-in Administrators get full access,
/// nobody else (a non-elevated admin token has Administrators as deny-only, so the
/// manager must run elevated).
pub const PIPE_SDDL: &str = "D:P(A;;GA;;;SY)(A;;GA;;;BA)";

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Owned self-relative security descriptor allocated by `LocalAlloc`.
pub struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

// The descriptor is immutable after creation and only read by the kernel.
unsafe impl Send for SecurityDescriptor {}
unsafe impl Sync for SecurityDescriptor {}

impl SecurityDescriptor {
    pub fn from_sddl(sddl: &str) -> io::Result<Self> {
        let w = wide(sddl);
        let mut sd: PSECURITY_DESCRIPTOR = ptr::null_mut();
        // SAFETY: valid NUL-terminated string, out pointer is valid.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(w.as_ptr(), SDDL_REVISION_1, &mut sd, ptr::null_mut())
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(sd))
    }

    pub fn as_ptr(&self) -> PSECURITY_DESCRIPTOR {
        self.0
    }

    /// `SECURITY_ATTRIBUTES` borrowing this descriptor (no handle inheritance).
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

/// Replace a registry key's DACL (protected: no inherited entries).
pub fn set_key_dacl(key: &winreg::RegKey, sddl: &str) -> io::Result<()> {
    let sd = SecurityDescriptor::from_sddl(sddl)?;
    // SAFETY: key handle is open, sd is a valid descriptor.
    let rc = unsafe {
        RegSetKeySecurity(
            key.raw_handle(),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            sd.as_ptr(),
        )
    };
    if rc != 0 {
        return Err(io::Error::from_raw_os_error(rc as i32));
    }
    Ok(())
}

/// A registry key's DACL as SDDL (tests check the secrets keys with it).
#[cfg(test)]
pub fn key_dacl_sddl(key: &winreg::RegKey) -> io::Result<String> {
    use windows_sys::Win32::Security::Authorization::ConvertSecurityDescriptorToStringSecurityDescriptorW;
    use windows_sys::Win32::System::Registry::RegGetKeySecurity;
    let mut len = 0u32;
    // SAFETY: size query with a null buffer.
    unsafe { RegGetKeySecurity(key.raw_handle(), DACL_SECURITY_INFORMATION, ptr::null_mut(), &mut len) };
    let mut buf = vec![0u8; len as usize];
    // SAFETY: buffer of the size the system asked for.
    let rc = unsafe { RegGetKeySecurity(key.raw_handle(), DACL_SECURITY_INFORMATION, buf.as_mut_ptr() as PSECURITY_DESCRIPTOR, &mut len) };
    if rc != 0 {
        return Err(io::Error::from_raw_os_error(rc as i32));
    }
    let mut s: *mut u16 = ptr::null_mut();
    // SAFETY: valid self-relative descriptor in `buf`; out pointer valid.
    let ok = unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            buf.as_ptr() as PSECURITY_DESCRIPTOR,
            SDDL_REVISION_1,
            DACL_SECURITY_INFORMATION,
            &mut s,
            ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: NUL-terminated string allocated by the system, freed below.
    let out = unsafe {
        let mut n = 0;
        while *s.add(n) != 0 {
            n += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(s, n))
    };
    // SAFETY: allocated by the conversion above.
    unsafe { LocalFree(s as HLOCAL) };
    Ok(out)
}

/// String SID of the user running this process (for the `--allow-current-user` dev flag).
pub fn current_user_sid() -> io::Result<String> {
    let mut token: HANDLE = ptr::null_mut();
    // SAFETY: pseudo handle of our own process; out pointer valid.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let result = (|| {
        let mut len = 0u32;
        // SAFETY: size query with a null buffer.
        unsafe { GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut len) };
        let mut buf = vec![0u8; len as usize];
        // SAFETY: buffer of the size the system asked for.
        if unsafe { GetTokenInformation(token, TokenUser, buf.as_mut_ptr() as *mut c_void, len, &mut len) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: buffer holds a TOKEN_USER (alignment: Vec<u8> from the allocator is
        // at least pointer-aligned for this size in practice; read_unaligned to be safe).
        let tu: TOKEN_USER = unsafe { ptr::read_unaligned(buf.as_ptr() as *const TOKEN_USER) };
        let mut s: *mut u16 = ptr::null_mut();
        // SAFETY: SID pointer points into `buf`, which is alive.
        if unsafe { ConvertSidToStringSidW(tu.User.Sid, &mut s) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: NUL-terminated string allocated by the system.
        let out = unsafe {
            let mut n = 0;
            while *s.add(n) != 0 {
                n += 1;
            }
            let v = String::from_utf16_lossy(std::slice::from_raw_parts(s, n));
            LocalFree(s as HLOCAL);
            v
        };
        Ok(out)
    })();
    // SAFETY: token opened above.
    unsafe { CloseHandle(token) };
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipe_sddl_parses() {
        assert!(SecurityDescriptor::from_sddl(PIPE_SDDL).is_ok());
        assert!(SecurityDescriptor::from_sddl(crate::registry_store::SECRETS_SDDL).is_ok());
        assert!(SecurityDescriptor::from_sddl("garbage(").is_err());
    }

    #[test]
    fn current_sid_looks_like_a_sid() {
        assert!(current_user_sid().unwrap().starts_with("S-1-"));
    }
}
