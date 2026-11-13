// SPDX-License-Identifier: GPL-3.0-only
//! Owner + DACL reads for the gate journals, a protected-directory create and an SDDL
//! syntax check. Adapted from `seat-session::win::files` (same project; seat-media does not
//! depend on seat-session). Every trust decision is made by the pure
//! [`crate::journal::untrusted_reason`].

use crate::journal::{Ace, SecurityInfo};
use std::path::Path;
use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, HANDLE, HLOCAL, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, GetNamedSecurityInfoW,
    GetSecurityInfo, SDDL_REVISION_1, SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    ACE_HEADER, ACL, DACL_SECURITY_INFORMATION, GetAce, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
    SECURITY_ATTRIBUTES,
};
use windows_sys::Win32::Storage::FileSystem::CreateDirectoryW;

use super::{last_error, wide};

fn wide_path(p: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    p.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
}

/// ACE types whose SID follows `ACE_HEADER` + `Mask` directly (allowed, denied, callback).
const SIMPLE_SID_ACE_TYPES: [u8; 4] = [0x0, 0x1, 0x9, 0xA];
const SEC_INFO: u32 = OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;

struct FreeLocal(*mut core::ffi::c_void);
impl Drop for FreeLocal {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: allocated with LocalAlloc by the API that returned it.
            unsafe { LocalFree(self.0 as HLOCAL) };
        }
    }
}

fn sid_string(sid: *mut core::ffi::c_void) -> Option<String> {
    if sid.is_null() {
        return None;
    }
    let mut out: *mut u16 = std::ptr::null_mut();
    // SAFETY: sid points to a SID inside a live security descriptor.
    if unsafe { ConvertSidToStringSidW(sid, &mut out) } == 0 || out.is_null() {
        return None;
    }
    let _free = FreeLocal(out.cast());
    let mut len = 0;
    // SAFETY: out is NUL-terminated.
    unsafe {
        while *out.add(len) != 0 {
            len += 1;
        }
        Some(String::from_utf16_lossy(std::slice::from_raw_parts(out, len)))
    }
}

fn parse_security(owner: *mut core::ffi::c_void, dacl: *mut ACL) -> SecurityInfo {
    let owner = sid_string(owner);
    if dacl.is_null() {
        return SecurityInfo { owner, dacl: None };
    }
    // SAFETY: dacl points into the caller's live security descriptor.
    let count = unsafe { (*dacl).AceCount };
    let mut aces = Vec::with_capacity(usize::from(count));
    for i in 0..u32::from(count) {
        let mut ace: *mut core::ffi::c_void = std::ptr::null_mut();
        // SAFETY: i < AceCount.
        if unsafe { GetAce(dacl, i, &mut ace) } == 0 || ace.is_null() {
            // Unreadable ACE: an allow ACE of unknown type (fail closed).
            aces.push(Ace { ace_type: 0xFF, flags: 0, mask: u32::MAX, sid: None });
            continue;
        }
        // SAFETY: every ACE starts with ACE_HEADER, followed by a 32-bit mask.
        let header = unsafe { std::ptr::read_unaligned(ace as *const ACE_HEADER) };
        // SAFETY: as above; the mask is at offset 4.
        let mask = unsafe { std::ptr::read_unaligned((ace as *const u8).add(4) as *const u32) };
        let sid = if SIMPLE_SID_ACE_TYPES.contains(&header.AceType) && header.AceSize >= 16 {
            // SAFETY: for these types the SID starts at offset 8.
            sid_string(unsafe { (ace as *mut u8).add(8) }.cast())
        } else {
            None
        };
        aces.push(Ace { ace_type: header.AceType, flags: header.AceFlags, mask, sid });
    }
    SecurityInfo { owner, dacl: Some(aces) }
}

/// Owner + DACL of a named file or directory.
pub fn security_of_path(path: &Path) -> Result<SecurityInfo, String> {
    let p = wide_path(path);
    let mut owner: *mut core::ffi::c_void = std::ptr::null_mut();
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: out pointers valid; sd freed below.
    let rc = unsafe {
        GetNamedSecurityInfoW(p.as_ptr(), SE_FILE_OBJECT, SEC_INFO, &mut owner, std::ptr::null_mut(), &mut dacl, std::ptr::null_mut(), &mut sd)
    };
    let _free = FreeLocal(sd);
    if rc != 0 {
        return Err(format!("GetNamedSecurityInfoW({}) failed: {rc}", path.display()));
    }
    Ok(parse_security(owner, dacl))
}

/// Owner + DACL of an open handle (needs READ_CONTROL, which a read open includes).
/// Checking the handle we read from leaves no window between check and read.
pub fn security_of_handle(file: &std::fs::File) -> Result<SecurityInfo, String> {
    use std::os::windows::io::AsRawHandle;
    let handle = file.as_raw_handle() as HANDLE;
    let mut owner: *mut core::ffi::c_void = std::ptr::null_mut();
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: handle is open; out pointers valid; sd freed below.
    let rc = unsafe {
        GetSecurityInfo(handle, SE_FILE_OBJECT, SEC_INFO, &mut owner, std::ptr::null_mut(), &mut dacl, std::ptr::null_mut(), &mut sd)
    };
    let _free = FreeLocal(sd);
    if rc != 0 {
        return Err(format!("GetSecurityInfo failed: {rc}"));
    }
    Ok(parse_security(owner, dacl))
}

/// Create one directory with the descriptor given as SDDL. An existing directory is left
/// as is (the caller verifies it).
pub fn create_dir_with_sddl(path: &Path, sddl: &str) -> Result<(), String> {
    let w = wide(sddl);
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: w is NUL-terminated; sd receives a LocalAlloc'd descriptor.
    if unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(w.as_ptr(), SDDL_REVISION_1, &mut sd, std::ptr::null_mut()) } == 0 {
        return Err(format!("bad SDDL: {}", last_error()));
    }
    let _free = FreeLocal(sd);
    let sa = SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: sd, bInheritHandle: 0 };
    let p = wide_path(path);
    // SAFETY: p is NUL-terminated; sa and sd live for the call.
    if unsafe { CreateDirectoryW(p.as_ptr(), &sa) } == 0 {
        let e = last_error();
        if e != ERROR_ALREADY_EXISTS {
            return Err(format!("CreateDirectoryW({}) failed: {e}", path.display()));
        }
    }
    Ok(())
}

/// The string parses as a security descriptor.
pub fn sddl_is_valid(sddl: &str) -> bool {
    if sddl.is_empty() || sddl.len() > 4096 || sddl.contains('\0') {
        return false;
    }
    let w = wide(sddl);
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: w is NUL-terminated; sd freed below.
    let ok = unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(w.as_ptr(), SDDL_REVISION_1, &mut sd, std::ptr::null_mut()) } != 0;
    let _free = FreeLocal(sd);
    ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sddl_check() {
        assert!(sddl_is_valid("D:P(A;;GA;;;SY)(A;;GA;;;BA)"));
        assert!(!sddl_is_valid("D:P(A;;GA;;;SY"));
        assert!(!sddl_is_valid(""));
    }

    #[test]
    fn temp_dir_is_readable() {
        let info = security_of_path(&std::env::temp_dir()).unwrap();
        assert!(info.owner.is_some());
    }
}
