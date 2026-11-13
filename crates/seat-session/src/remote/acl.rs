// SPDX-License-Identifier: GPL-3.0-only
//! Trust rules for the files the SYSTEM service reads back (run state, restore journal).
//! `%ProgramData%` lets every user create files and folders, so a file there is trusted
//! only when nobody but SYSTEM / Administrators can have written it. Same rules as
//! `split-termwrap`'s journal (ported, not shared, to keep the crates independent).
//!
//! Pure: the Windows side (`win::files`) only reads owner + DACL into [`SecurityInfo`].

use crate::remote::SessionError;
use std::path::Path;

/// `S-1-5-18` LocalSystem.
pub const SID_SYSTEM: &str = "S-1-5-18";
/// `S-1-5-32-544` BUILTIN\Administrators.
pub const SID_ADMINS: &str = "S-1-5-32-544";
/// Principals allowed as owner and in every allow ACE.
pub const TRUSTED: [&str; 2] = [SID_SYSTEM, SID_ADMINS];

/// Protected DACL (no inheritance from `%ProgramData%`): full control for SYSTEM and
/// Administrators only, inherited by the files inside.
pub const PRIVATE_DIR_SDDL: &str = "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)";

pub const ACCESS_ALLOWED_ACE_TYPE: u8 = 0x0;
const DENY_TYPES: [u8; 4] = [0x1, 0x6, 0xA, 0xC];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ace {
    pub ace_type: u8,
    pub flags: u8,
    pub mask: u32,
    /// `None` when the SID could not be converted.
    pub sid: Option<String>,
}

impl Ace {
    pub fn allow(sid: &str, mask: u32) -> Self {
        Self { ace_type: ACCESS_ALLOWED_ACE_TYPE, flags: 0, mask, sid: Some(sid.into()) }
    }
}

/// Owner + DACL of one object. `dacl: None` is a NULL DACL (everyone full control).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecurityInfo {
    pub owner: Option<String>,
    pub dacl: Option<Vec<Ace>>,
}

fn trusted(sid: Option<&str>) -> bool {
    sid.is_some_and(|s| TRUSTED.iter().any(|a| a.eq_ignore_ascii_case(s)))
}

/// Why the object must not be trusted, or `None`. Owner must be SYSTEM or Administrators
/// and every non-deny ACE (inherit-only included) an allow ACE for one of them.
pub fn untrusted_reason(info: &SecurityInfo) -> Option<String> {
    let show = |s: &Option<String>| s.clone().unwrap_or_else(|| "<unreadable SID>".into());
    if !trusted(info.owner.as_deref()) {
        return Some(format!("owner {} is not SYSTEM or Administrators", show(&info.owner)));
    }
    let Some(dacl) = &info.dacl else {
        return Some("NULL DACL".into());
    };
    for ace in dacl {
        if DENY_TYPES.contains(&ace.ace_type) {
            continue;
        }
        if ace.ace_type != ACCESS_ALLOWED_ACE_TYPE || !trusted(ace.sid.as_deref()) {
            return Some(format!("ACE type {:#x} for {} is not allowed", ace.ace_type, show(&ace.sid)));
        }
    }
    None
}

/// `PermissionDenied` I/O error naming the refused object (fail closed).
pub fn refused(what: &Path, why: &str) -> SessionError {
    SessionError::Io(std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        format!("{} is not trusted (fail closed): {why}", what.display()),
    ))
}

/// Creates `dir` with [`PRIVATE_DIR_SDDL`] (parents normally) and checks it. On other
/// platforms a plain `create_dir_all`.
pub fn ensure_private_dir(dir: &Path) -> Result<(), SessionError> {
    #[cfg(windows)]
    {
        if let Some(parent) = dir.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        crate::remote::win::files::create_dir_with_sddl(dir, PRIVATE_DIR_SDDL).map_err(|e| refused(dir, &e))?;
        verify_dir(dir)
    }
    #[cfg(not(windows))]
    {
        std::fs::create_dir_all(dir)?;
        Ok(())
    }
}

/// Owner + DACL check of an existing directory (no-op off Windows).
pub fn verify_dir(dir: &Path) -> Result<(), SessionError> {
    #[cfg(windows)]
    {
        let info = crate::remote::win::files::security_of_path(dir).map_err(|e| refused(dir, &e))?;
        untrusted_reason(&info).map_or(Ok(()), |why| Err(refused(dir, &why)))
    }
    #[cfg(not(windows))]
    {
        let _ = dir;
        Ok(())
    }
}

/// Owner + DACL check of an open file (needs READ_CONTROL; no-op off Windows). Checking
/// the handle we read from leaves no window between check and read.
pub fn verify_file(f: &std::fs::File, path: &Path) -> Result<(), SessionError> {
    #[cfg(windows)]
    {
        let info = crate::remote::win::files::security_of_handle(f).map_err(|e| refused(path, &e))?;
        untrusted_reason(&info).map_or(Ok(()), |why| Err(refused(path, &why)))
    }
    #[cfg(not(windows))]
    {
        let _ = (f, path);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const USER: &str = "S-1-5-21-1111111111-2222222222-3333333333-1001";
    const USERS: &str = "S-1-5-32-545";
    const FULL: u32 = 0x001F_01FF;

    fn info(owner: &str, dacl: Vec<Ace>) -> SecurityInfo {
        SecurityInfo { owner: Some(owner.into()), dacl: Some(dacl) }
    }

    #[test]
    fn private_dir_passes() {
        let d = vec![Ace::allow(SID_SYSTEM, FULL), Ace::allow(SID_ADMINS, FULL)];
        assert_eq!(untrusted_reason(&info(SID_SYSTEM, d.clone())), None);
        assert_eq!(untrusted_reason(&info(SID_ADMINS, d)), None);
    }

    #[test]
    fn user_owner_or_user_ace_or_null_dacl_fails() {
        let d = vec![Ace::allow(SID_SYSTEM, FULL)];
        assert!(untrusted_reason(&info(USER, d.clone())).is_some());
        let mut d2 = d.clone();
        d2.push(Ace::allow(USERS, 0x0012_00A9));
        assert!(untrusted_reason(&info(SID_SYSTEM, d2)).is_some(), "ProgramData-like inherited DACL");
        assert!(untrusted_reason(&SecurityInfo { owner: Some(SID_SYSTEM.into()), dacl: None }).is_some());
        assert!(untrusted_reason(&SecurityInfo { owner: None, dacl: Some(d) }).is_some());
        let deny = Ace { ace_type: 0x1, flags: 0, mask: FULL, sid: Some(USER.into()) };
        assert_eq!(untrusted_reason(&info(SID_SYSTEM, vec![Ace::allow(SID_SYSTEM, FULL), deny])), None);
    }
}
