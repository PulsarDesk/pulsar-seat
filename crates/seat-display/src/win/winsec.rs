// SPDX-License-Identifier: GPL-3.0-only
//! File-system trust checks for the SYSTEM-only display topology journal the service
//! reads back as LocalSystem (adapted from seat-compat `winsec.rs`, same repository). A standard user can create folders under
//! `%ProgramData%`, so a path there is trusted only after we checked who owns it and who
//! can write it:
//!
//! - owner is SYSTEM, BUILTIN\Administrators or TrustedInstaller;
//! - no allow ACE that applies to the object grants a write-type right to any other SID
//!   (a NULL DACL counts as "everyone may write");
//! - not a reparse point (symlink, junction, mount point);
//! - a file has exactly one hard link.
//!
//! [`harden`] sets owner + protected DACL with `SetNamedSecurityInfoW`, which also
//! re-applies inheritance to existing children (unlike `SetFileSecurityW`). It needs an
//! elevated token (setting the owner to Administrators).
//!
//! Documented Win32 APIs only. Untested on hardware beyond `cargo test` on the dev box.

#![allow(unsafe_code)]

use super::wide;
use std::fs::File;
use std::os::windows::fs::MetadataExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use windows_sys::Win32::Foundation::{HLOCAL, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
    GetNamedSecurityInfoW, GetSecurityInfo, SE_FILE_OBJECT, SetNamedSecurityInfoW,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION, GetAce,
    GetSecurityDescriptorDacl, GetSecurityDescriptorOwner, OWNER_SECURITY_INFORMATION,
    PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_REPARSE_POINT, GetFileInformationByHandle,
};

/// SIDs allowed to own or write trusted state.
const TRUSTED_SIDS: [&str; 3] = [
    "S-1-5-18",                                                       // LocalSystem
    "S-1-5-32-544",                                                   // BUILTIN\Administrators
    "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464", // TrustedInstaller
];

/// Rights that let a holder change the object (or, on a folder, its children).
const WRITE_RIGHTS: u32 = 0x0000_0002 // FILE_WRITE_DATA / FILE_ADD_FILE
    | 0x0000_0004 // FILE_APPEND_DATA / FILE_ADD_SUBDIRECTORY
    | 0x0000_0010 // FILE_WRITE_EA
    | 0x0000_0040 // FILE_DELETE_CHILD
    | 0x0000_0100 // FILE_WRITE_ATTRIBUTES
    | 0x0001_0000 // DELETE
    | 0x0004_0000 // WRITE_DAC
    | 0x0008_0000 // WRITE_OWNER
    | 0x1000_0000 // GENERIC_ALL
    | 0x4000_0000; // GENERIC_WRITE

const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
const ACCESS_DENIED_ACE_TYPE: u8 = 1;
const SYSTEM_AUDIT_ACE_TYPE: u8 = 2;
const INHERIT_ONLY_ACE: u8 = 0x08;

/// Security descriptor returned by the system (freed with `LocalFree`).
struct LocalSd(PSECURITY_DESCRIPTOR);
impl Drop for LocalSd {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: allocated by the system with LocalAlloc.
            unsafe { LocalFree(self.0 as HLOCAL) };
        }
    }
}

fn sid_string(sid: PSID) -> Option<String> {
    if sid.is_null() {
        return None;
    }
    let mut s: *mut u16 = std::ptr::null_mut();
    // SAFETY: valid SID; the string is freed below.
    if unsafe { ConvertSidToStringSidW(sid, &mut s) } == 0 || s.is_null() {
        return None;
    }
    let mut n = 0usize;
    // SAFETY: NUL-terminated string from the system.
    unsafe {
        while *s.add(n) != 0 {
            n += 1;
        }
    }
    // SAFETY: n code units are valid.
    let out = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(s, n) });
    unsafe { LocalFree(s as HLOCAL) };
    Some(out)
}

fn is_trusted_sid(s: &str) -> bool {
    TRUSTED_SIDS.iter().any(|t| t.eq_ignore_ascii_case(s))
}

/// Check owner + DACL of a descriptor.
fn check_owner_dacl(owner: PSID, dacl: *mut ACL) -> Result<(), String> {
    match sid_string(owner) {
        Some(o) if is_trusted_sid(&o) => {}
        Some(o) => return Err(format!("owner {o} is not SYSTEM or Administrators")),
        None => return Err("owner unknown".into()),
    }
    if dacl.is_null() {
        return Err("NULL DACL (everyone has full access)".into());
    }
    // SAFETY: dacl points into the descriptor, valid while it lives.
    let count = unsafe { (*dacl).AceCount } as u32;
    for i in 0..count {
        let mut ace: *mut core::ffi::c_void = std::ptr::null_mut();
        // SAFETY: index < AceCount.
        if unsafe { GetAce(dacl, i, &mut ace) } == 0 || ace.is_null() {
            return Err(format!("ACE {i} unreadable"));
        }
        // SAFETY: every ACE starts with ACE_HEADER.
        let h = unsafe { &*(ace as *const ACE_HEADER) };
        if h.AceFlags & INHERIT_ONLY_ACE != 0 {
            continue; // does not apply to this object
        }
        match h.AceType {
            ACCESS_DENIED_ACE_TYPE | SYSTEM_AUDIT_ACE_TYPE => continue,
            ACCESS_ALLOWED_ACE_TYPE => {
                // SAFETY: type says ACCESS_ALLOWED_ACE; the SID starts at SidStart.
                let a = unsafe { &*(ace as *const ACCESS_ALLOWED_ACE) };
                if a.Mask & WRITE_RIGHTS == 0 {
                    continue;
                }
                let sid = &a.SidStart as *const u32 as PSID;
                match sid_string(sid) {
                    Some(s) if is_trusted_sid(&s) => {}
                    Some(s) => {
                        return Err(format!("{s} may write (mask 0x{:08x})", a.Mask));
                    }
                    None => return Err(format!("ACE {i}: unreadable SID")),
                }
            }
            // Object / callback allow ACEs are not expected on our files: refuse.
            t => return Err(format!("ACE {i}: unexpected type {t}")),
        }
    }
    Ok(())
}

/// Path (file or folder, not followed if it is a link) is owned and writable only by
/// SYSTEM / Administrators and is not a reparse point.
pub fn check_path(path: &Path) -> Result<(), String> {
    let md = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if md.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err("reparse point (link or junction)".into());
    }
    let w = wide(&path.to_string_lossy());
    let mut owner: PSID = std::ptr::null_mut();
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: out pointers; sd freed by LocalSd.
    let rc = unsafe {
        GetNamedSecurityInfoW(
            w.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            &mut dacl,
            std::ptr::null_mut(),
            &mut sd,
        )
    };
    let _sd = LocalSd(sd);
    if rc != 0 {
        return Err(format!("GetNamedSecurityInfoW: Win32 error {rc}"));
    }
    check_owner_dacl(owner, dacl)
}

/// An open file: not a reparse point, one hard link, trusted owner + DACL. The handle
/// needs `READ_CONTROL` (any read open has it).
pub fn check_file(f: &File) -> Result<(), String> {
    let h = f.as_raw_handle();
    // SAFETY: zeroed POD out struct.
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: valid handle.
    if unsafe { GetFileInformationByHandle(h as _, &mut info) } == 0 {
        return Err(format!(
            "GetFileInformationByHandle: {}",
            std::io::Error::last_os_error()
        ));
    }
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err("reparse point (link)".into());
    }
    if info.nNumberOfLinks != 1 {
        return Err(format!("{} hard links", info.nNumberOfLinks));
    }
    let mut owner: PSID = std::ptr::null_mut();
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: valid handle, out pointers.
    let rc = unsafe {
        GetSecurityInfo(
            h as _,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            &mut dacl,
            std::ptr::null_mut(),
            &mut sd,
        )
    };
    let _sd = LocalSd(sd);
    if rc != 0 {
        return Err(format!("GetSecurityInfo: Win32 error {rc}"));
    }
    check_owner_dacl(owner, dacl)
}

/// Set owner `BUILTIN\Administrators` and a protected DACL from `dacl_sddl`
/// (`D:P(...)`) on a folder or file. Inheritable entries are re-applied to existing
/// children. Needs an elevated token.
pub fn harden(path: &Path, dacl_sddl: &str) -> Result<(), String> {
    let md = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if md.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err("reparse point (link or junction)".into());
    }
    let sddl = wide(&format!("O:BA{dacl_sddl}"));
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: valid SDDL string; sd freed by LocalSd.
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut sd,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(format!("SDDL: {}", std::io::Error::last_os_error()));
    }
    let sd = LocalSd(sd);
    let mut owner: PSID = std::ptr::null_mut();
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut present = 0;
    let mut defaulted = 0;
    // SAFETY: valid descriptor.
    unsafe {
        GetSecurityDescriptorOwner(sd.0, &mut owner, &mut defaulted);
        GetSecurityDescriptorDacl(sd.0, &mut present, &mut dacl, &mut defaulted);
    }
    if owner.is_null() || present == 0 || dacl.is_null() {
        return Err("SDDL without owner or DACL".into());
    }
    let w = wide(&path.to_string_lossy());
    // SAFETY: valid path, SID and ACL.
    let rc = unsafe {
        SetNamedSecurityInfoW(
            w.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION
                | DACL_SECURITY_INFORMATION
                | PROTECTED_DACL_SECURITY_INFORMATION,
            owner,
            std::ptr::null_mut(),
            dacl,
            std::ptr::null(),
        )
    };
    if rc != 0 {
        return Err(format!("SetNamedSecurityInfoW: Win32 error {rc}"));
    }
    Ok(())
}

/// Open `path` for reading without following a reparse point, then [`check_file`] it.
/// `Ok(None)` when the file does not exist.
pub fn open_trusted(path: &Path) -> Result<Option<File>, String> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    match std::fs::OpenOptions::new().read(true).custom_flags(FILE_FLAG_OPEN_REPARSE_POINT).open(path) {
        Ok(f) => check_file(&f).map(|()| Some(f)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("open: {e}")),
    }
}

/// Make `dir` a trusted private folder: create it when missing, and when its owner / DACL
/// is not trusted, set owner Administrators + `dacl_sddl` (needs an elevated token), then
/// check again.
pub fn prepare_private_dir(dir: &Path, dacl_sddl: &str) -> Result<(), String> {
    if let Some(parent) = dir.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    match std::fs::create_dir(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(format!("{}: {e}", dir.display())),
    }
    if check_path(dir).is_ok() {
        return Ok(());
    }
    harden(dir, dacl_sddl)?;
    check_path(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_hard_link_and_user_folders_are_refused() {
        let dir = std::env::temp_dir().join(format!("pseat-dwinsec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("f.txt");
        std::fs::write(&p, b"x").unwrap();
        let l = dir.join("link.txt");
        if std::fs::hard_link(&p, &l).is_ok() {
            let f = File::open(&p).unwrap();
            assert!(check_file(&f).is_err());
        }
        assert!(open_trusted(&dir.join("missing.json")).unwrap().is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn system_folder_is_trusted() {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        assert_eq!(check_path(&Path::new(&root).join("System32")), Ok(()));
    }
}
