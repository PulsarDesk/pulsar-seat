// SPDX-License-Identifier: GPL-3.0-only
//! Safe directories for files the SYSTEM service writes under `%ProgramData%`.
//!
//! `%ProgramData%`'s default ACL lets every user create subfolders, so a seat user could
//! pre-create `PulsarSplit\pads` (or plant a junction in it) and redirect a SYSTEM write
//! or rename. [`secure_dir`] therefore:
//! - refuses any existing path component (data root, `pads`) that is a reparse point or
//!   is owned by anyone but SYSTEM or Administrators;
//! - creates a missing directory with a protected DACL (SYSTEM + Administrators full,
//!   no user ACE), and re-applies that DACL to an existing `pads` directory.
//!
//! The data root's own ACL belongs to setup (contracts §0 data root); here it is only
//! checked, never changed.

use std::os::windows::fs::MetadataExt;
use std::path::Path;

use windows_sys::Win32::Security::Authorization::{GetNamedSecurityInfoW, SetNamedSecurityInfoW, SE_FILE_OBJECT};
use windows_sys::Win32::Security::{
    GetSecurityDescriptorDacl, IsWellKnownSid, WinBuiltinAdministratorsSid, WinLocalSystemSid, ACL,
    DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSID,
    SECURITY_ATTRIBUTES,
};
use windows_sys::Win32::Storage::FileSystem::{CreateDirectoryW, FILE_ATTRIBUTE_REPARSE_POINT};

use super::{last_error, wide, Sd};
use crate::error::PadError;

/// SYSTEM + Administrators full control, inherited by files and subfolders, protected.
pub const DIR_SDDL: &str = "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)";

fn deny(path: &Path, why: &str) -> PadError {
    PadError::Invalid(format!("unsafe directory {}: {why}", path.display()))
}

/// Existing `path` must be a plain directory owned by SYSTEM or Administrators.
pub fn check_existing(path: &Path) -> Result<(), PadError> {
    let md = std::fs::symlink_metadata(path)?;
    if md.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(deny(path, "is a reparse point (junction or link)"));
    }
    if !md.is_dir() {
        return Err(deny(path, "is not a directory"));
    }
    let w = wide(&path.to_string_lossy());
    let mut owner: PSID = std::ptr::null_mut();
    let mut sd: *mut core::ffi::c_void = std::ptr::null_mut();
    // SAFETY: valid path and out pointers; `sd` is freed by `Sd`'s drop.
    let rc = unsafe {
        GetNamedSecurityInfoW(
            w.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut sd,
        )
    };
    let _sd = Sd(sd);
    if rc != 0 {
        return Err(PadError::os("GetNamedSecurityInfoW", rc));
    }
    // SAFETY: owner points into `sd`, alive until `_sd` drops.
    let ok = unsafe { IsWellKnownSid(owner, WinLocalSystemSid) != 0 || IsWellKnownSid(owner, WinBuiltinAdministratorsSid) != 0 };
    if !ok {
        return Err(deny(path, "owner is not SYSTEM or Administrators"));
    }
    Ok(())
}

fn create_protected(path: &Path) -> Result<(), PadError> {
    let sd = Sd::from_sddl(DIR_SDDL)?;
    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.0,
        bInheritHandle: 0,
    };
    let w = wide(&path.to_string_lossy());
    // SAFETY: valid path and attributes.
    if unsafe { CreateDirectoryW(w.as_ptr(), &sa) } == 0 {
        let e = last_error();
        // ERROR_ALREADY_EXISTS: someone raced us; the caller re-checks it.
        if e != windows_sys::Win32::Foundation::ERROR_ALREADY_EXISTS {
            return Err(PadError::os("CreateDirectoryW", e));
        }
    }
    Ok(())
}

fn apply_protected_dacl(path: &Path) -> Result<(), PadError> {
    let sd = Sd::from_sddl(DIR_SDDL)?;
    let mut present = 0;
    let mut defaulted = 0;
    let mut dacl: *mut ACL = std::ptr::null_mut();
    // SAFETY: valid descriptor and out pointers.
    if unsafe { GetSecurityDescriptorDacl(sd.0, &mut present, &mut dacl, &mut defaulted) } == 0 || present == 0 {
        return Err(PadError::os("GetSecurityDescriptorDacl", last_error()));
    }
    let w = wide(&path.to_string_lossy());
    // SAFETY: valid path; dacl points into `sd`.
    let rc = unsafe {
        SetNamedSecurityInfoW(
            w.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            dacl,
            std::ptr::null(),
        )
    };
    if rc != 0 {
        return Err(PadError::os("SetNamedSecurityInfoW", rc));
    }
    Ok(())
}

/// Makes `root\leaf` safe for SYSTEM writes: `root` must exist and pass
/// [`check_existing`]; `leaf` is created protected or checked and re-protected.
pub fn secure_dir(root: &Path, leaf: &Path) -> Result<(), PadError> {
    check_existing(root)?;
    if std::fs::symlink_metadata(leaf).is_err() {
        create_protected(leaf)?;
    }
    check_existing(leaf)?;
    apply_protected_dacl(leaf)
}

/// A temporary directory owned by the calling user's own SID (the token user), never by
/// Administrators. A plain `tempfile::tempdir()` is owned by BUILTIN\Administrators when
/// the process runs with an elevated admin token (default owner of such tokens), as on the
/// GitHub runners (first CI runs, 2026-11-25): the "user-owned" premise of the tests below
/// would not hold there.
#[cfg(test)]
pub(crate) fn user_owned_tempdir() -> tempfile::TempDir {
    use windows_sys::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    let d = tempfile::tempdir().unwrap();
    let mut token = std::ptr::null_mut();
    // SAFETY: pseudo handle of this process, valid out pointer.
    assert!(unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } != 0, "OpenProcessToken");
    let token = super::Handle(token);
    // TOKEN_USER plus the SID it points to; u64 elements keep the buffer aligned.
    let mut buf = vec![0u64; 64];
    let mut len = 0u32;
    // SAFETY: valid token and a buffer of the size passed.
    let ok = unsafe { GetTokenInformation(token.0, TokenUser, buf.as_mut_ptr().cast(), (buf.len() * 8) as u32, &mut len) };
    assert!(ok != 0, "GetTokenInformation(TokenUser): {}", last_error());
    // SAFETY: the call filled a TOKEN_USER at the start of `buf`.
    let user = unsafe { &*(buf.as_ptr() as *const TOKEN_USER) };
    let w = wide(&d.path().to_string_lossy());
    // SAFETY: valid path; the SID points into `buf`. Setting the owner to one's own user
    // SID needs no privilege.
    let rc = unsafe {
        SetNamedSecurityInfoW(
            w.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            user.User.Sid,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    assert_eq!(rc, 0, "SetNamedSecurityInfoW(owner = token user)");
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory the test user owns is refused (no system state is changed).
    #[test]
    fn user_owned_dir_refused() {
        let d = user_owned_tempdir();
        assert!(matches!(check_existing(d.path()), Err(PadError::Invalid(_))));
        assert!(secure_dir(d.path(), &d.path().join("pads")).is_err());
        assert!(!d.path().join("pads").exists(), "nothing created under an unsafe root");
        assert!(check_existing(&d.path().join("missing")).is_err());
    }

    #[test]
    fn sddl_parses() {
        assert!(Sd::from_sddl(DIR_SDDL).is_ok());
    }
}
