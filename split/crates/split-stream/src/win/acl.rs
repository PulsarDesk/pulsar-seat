// SPDX-License-Identifier: GPL-3.0-only
//! Read-only ACL inspection: "can anyone but SYSTEM / Administrators / TrustedInstaller
//! change this object?" Used before launching the server binary as SYSTEM.

use super::os_err;
use crate::StreamError;
use std::ffi::c_void;
use std::ptr;
use windows_sys::Win32::Foundation::{LocalFree, HANDLE, HLOCAL};
use windows_sys::Win32::Security::Authorization::{ConvertSidToStringSidW, GetSecurityInfo, SE_FILE_OBJECT};
use windows_sys::Win32::Security::{
    GetAce, ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR, PSID,
};

pub const SID_SYSTEM: &str = "S-1-5-18";
pub const SID_ADMINS: &str = "S-1-5-32-544";
pub const SID_TRUSTED_INSTALLER: &str = "S-1-5-80-956008885-3418522649-1831038044-1851658575-3002747228";

const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
const INHERIT_ONLY_ACE: u8 = 0x08;
/// Rights that let a holder replace, append to, delete or re-ACL a file or a folder's
/// entries (FILE_WRITE_DATA/ADD_FILE, FILE_APPEND_DATA/ADD_SUBDIRECTORY, FILE_WRITE_EA,
/// FILE_DELETE_CHILD, FILE_WRITE_ATTRIBUTES, DELETE, WRITE_DAC, WRITE_OWNER, MAXIMUM_ALLOWED,
/// GENERIC_ALL, GENERIC_WRITE).
pub const WRITE_LIKE: u32 =
    0x2 | 0x4 | 0x10 | 0x40 | 0x100 | 0x1_0000 | 0x4_0000 | 0x8_0000 | 0x0200_0000 | 0x1000_0000 | 0x4000_0000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ace {
    pub allow: bool,
    pub inherit_only: bool,
    pub mask: u32,
    pub sid: String,
}

fn trusted(sid: &str) -> bool {
    sid == SID_SYSTEM || sid == SID_ADMINS || sid == SID_TRUSTED_INSTALLER
}

/// Pure check: owner trusted, and no effective allow ACE grants a write-like right to
/// anyone else. Inherit-only ACEs (e.g. CREATOR OWNER on Program Files) do not apply to
/// the object itself and are ignored.
pub fn check_admin_only(owner: &str, aces: &[Ace]) -> Result<(), String> {
    if !trusted(owner) {
        return Err(format!("owner {owner} is not SYSTEM/Administrators/TrustedInstaller"));
    }
    for a in aces {
        if a.allow && !a.inherit_only && a.mask & WRITE_LIKE != 0 && !trusted(&a.sid) {
            return Err(format!("{} may modify it (mask 0x{:x})", a.sid, a.mask));
        }
    }
    Ok(())
}

pub(crate) fn sid_string(sid: PSID) -> Result<String, StreamError> {
    let mut w: *mut u16 = ptr::null_mut();
    // SAFETY: valid SID; the API allocates `w`, freed below.
    if unsafe { ConvertSidToStringSidW(sid, &mut w) } == 0 {
        return Err(os_err("ConvertSidToStringSidW"));
    }
    // SAFETY: NUL-terminated string from the API.
    let s = unsafe {
        let n = (0..).take_while(|&i| *w.add(i) != 0).count();
        String::from_utf16_lossy(std::slice::from_raw_parts(w, n))
    };
    // SAFETY: allocated by ConvertSidToStringSidW.
    unsafe { LocalFree(w as HLOCAL) };
    Ok(s)
}

/// Owner SID and DACL entries of an open file / directory (handle needs READ_CONTROL).
pub(crate) fn owner_and_aces(h: HANDLE) -> Result<(String, Vec<Ace>), StreamError> {
    let mut owner: PSID = ptr::null_mut();
    let mut dacl: *mut ACL = ptr::null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: valid handle; out pointers point into `sd`, freed below.
    let rc = unsafe {
        GetSecurityInfo(
            h,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            ptr::null_mut(),
            &mut dacl,
            ptr::null_mut(),
            &mut sd,
        )
    };
    if rc != 0 {
        return Err(StreamError::io("GetSecurityInfo", std::io::Error::from_raw_os_error(rc as i32)));
    }
    let res = (|| {
        let owner = if owner.is_null() { String::new() } else { sid_string(owner)? };
        let mut aces = Vec::new();
        if dacl.is_null() {
            // NULL DACL = everyone full access.
            aces.push(Ace { allow: true, inherit_only: false, mask: u32::MAX, sid: "S-1-1-0".into() });
            return Ok((owner, aces));
        }
        // SAFETY: valid ACL inside `sd`.
        let count = unsafe { (*dacl).AceCount };
        for i in 0..u32::from(count) {
            let mut p: *mut c_void = ptr::null_mut();
            // SAFETY: index < AceCount.
            if unsafe { GetAce(dacl, i, &mut p) } == 0 {
                return Err(os_err("GetAce"));
            }
            // SAFETY: every ACE starts with an ACE_HEADER.
            let hdr = unsafe { &*(p as *const ACE_HEADER) };
            if hdr.AceType != ACCESS_ALLOWED_ACE_TYPE {
                // Deny ACEs only remove rights; object/callback ACEs are not used on files.
                continue;
            }
            // SAFETY: ACCESS_ALLOWED_ACE layout for this type; SID starts at SidStart.
            let ace = unsafe { &*(p as *const ACCESS_ALLOWED_ACE) };
            let sid = sid_string(ptr::addr_of!(ace.SidStart) as PSID)?;
            aces.push(Ace { allow: true, inherit_only: hdr.AceFlags & INHERIT_ONLY_ACE != 0, mask: ace.Mask, sid });
        }
        Ok((owner, aces))
    })();
    // SAFETY: allocated by GetSecurityInfo.
    unsafe { LocalFree(sd as HLOCAL) };
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ace(sid: &str, mask: u32, inherit_only: bool) -> Ace {
        Ace { allow: true, inherit_only, mask, sid: sid.into() }
    }

    #[test]
    fn program_files_like_acl_passes_user_write_fails() {
        const READ_EXEC: u32 = 0x1200a9;
        const MODIFY: u32 = 0x1301bf;
        let pf = vec![
            ace(SID_TRUSTED_INSTALLER, 0x1f01ff, false),
            ace(SID_SYSTEM, MODIFY, false),
            ace(SID_ADMINS, MODIFY, false),
            ace("S-1-5-32-545", READ_EXEC, false),
            ace("S-1-3-0", 0x1000_0000, true), // CREATOR OWNER, inherit only
        ];
        assert!(check_admin_only(SID_TRUSTED_INSTALLER, &pf).is_ok());
        let mut bad = pf.clone();
        bad.push(ace("S-1-5-32-545", 0x2, false));
        assert!(check_admin_only(SID_ADMINS, &bad).is_err());
        assert!(check_admin_only("S-1-5-21-1-2-3-1001", &pf).is_err(), "user-owned folder");
    }
}
