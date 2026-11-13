// SPDX-License-Identifier: GPL-3.0-only
//! Pure security-descriptor rules (allow-list, fail closed). The Windows side
//! ([`crate::win::files`]) only reads the owner and the DACL into [`SecurityInfo`]; every
//! decision is made here so it can be tested over synthetic ACE lists.
//!
//! Wrapper files: TermService (svchost, NetworkService) loads the wrapper DLL and reads its
//! config, so nobody but SYSTEM, Administrators and TrustedInstaller may be able to
//! replace them. That covers: the file itself, its directory (no planting, no rename) and
//! every ancestor up to the volume root (no rename of an ancestor + recreate of the path).
//!
//! Journal: only SYSTEM and Administrators (we create it with a protected DACL).

/// `S-1-5-18` LocalSystem.
pub const SID_SYSTEM: &str = "S-1-5-18";
/// `S-1-5-32-544` BUILTIN\Administrators.
pub const SID_ADMINS: &str = "S-1-5-32-544";
/// `NT SERVICE\TrustedInstaller`.
pub const SID_TRUSTED_INSTALLER: &str = "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464";

/// Principals allowed to have write-type rights on a wrapper file or its directories.
pub const WRAPPER_TRUSTED: [&str; 3] = [SID_SYSTEM, SID_ADMINS, SID_TRUSTED_INSTALLER];
/// Principals allowed on the journal directory and file (owner and every allow ACE).
pub const JOURNAL_TRUSTED: [&str; 2] = [SID_SYSTEM, SID_ADMINS];

/// SDDL of the journal directory: protected (no inheritance from `%ProgramData%`), full
/// control for SYSTEM and Administrators only, inherited by the journal file.
pub const JOURNAL_DIR_SDDL: &str = "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)";

pub const ACCESS_ALLOWED_ACE_TYPE: u8 = 0x0;
pub const ACCESS_DENIED_ACE_TYPE: u8 = 0x1;
pub const ACCESS_DENIED_OBJECT_ACE_TYPE: u8 = 0x6;
pub const ACCESS_DENIED_CALLBACK_ACE_TYPE: u8 = 0xA;
pub const ACCESS_DENIED_CALLBACK_OBJECT_ACE_TYPE: u8 = 0xC;
pub const INHERIT_ONLY_ACE: u8 = 0x08;

pub const DELETE: u32 = 0x0001_0000;
pub const WRITE_DAC: u32 = 0x0004_0000;
pub const WRITE_OWNER: u32 = 0x0008_0000;
pub const GENERIC_ALL: u32 = 0x1000_0000;
pub const GENERIC_WRITE: u32 = 0x4000_0000;
pub const MAXIMUM_ALLOWED: u32 = 0x0200_0000;
/// File: write data. Directory: `FILE_ADD_FILE`.
pub const FILE_WRITE_DATA: u32 = 0x0002;
/// File: append data. Directory: `FILE_ADD_SUBDIRECTORY`.
pub const FILE_APPEND_DATA: u32 = 0x0004;
pub const FILE_ADD_FILE: u32 = FILE_WRITE_DATA;
pub const FILE_ADD_SUBDIRECTORY: u32 = FILE_APPEND_DATA;
pub const FILE_DELETE_CHILD: u32 = 0x0040;

/// One ACE as read from a DACL. `sid` is `None` when the SID could not be converted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ace {
    pub ace_type: u8,
    pub flags: u8,
    pub mask: u32,
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

/// Where an object sits relative to the wrapper file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The DLL or the config file.
    File,
    /// The directory that holds the wrapper: no planting next to it, no rename, no
    /// delete/replace of children.
    WrapperDir,
    /// Any further ancestor up to the volume root: no rename of itself, no rename or
    /// delete of its children. Creating new entries is fine (`C:\` allows that).
    Ancestor,
}

/// Rights that let a principal change or replace the object, given its role.
pub fn tamper_mask(role: Role) -> u32 {
    let common = WRITE_DAC | WRITE_OWNER | GENERIC_WRITE | GENERIC_ALL | MAXIMUM_ALLOWED | DELETE;
    match role {
        Role::File => common | FILE_WRITE_DATA | FILE_APPEND_DATA,
        Role::WrapperDir => common | FILE_DELETE_CHILD | FILE_ADD_FILE | FILE_ADD_SUBDIRECTORY,
        Role::Ancestor => common | FILE_DELETE_CHILD,
    }
}

fn is_deny(t: u8) -> bool {
    matches!(
        t,
        ACCESS_DENIED_ACE_TYPE
            | ACCESS_DENIED_OBJECT_ACE_TYPE
            | ACCESS_DENIED_CALLBACK_ACE_TYPE
            | ACCESS_DENIED_CALLBACK_OBJECT_ACE_TYPE
    )
}

fn trusted(sid: Option<&str>, allow: &[&str]) -> bool {
    sid.is_some_and(|s| allow.iter().any(|a| a.eq_ignore_ascii_case(s)))
}

/// Pure: why a wrapper file / directory is tamperable by someone outside
/// [`WRAPPER_TRUSTED`], or `None` if it is not. Deny ACEs are ignored on purpose (fail
/// closed: a false "insecure" only blocks enabling). Inherit-only ACEs do not apply to the
/// object itself and are skipped (children are covered: planting new children in the
/// wrapper directory is itself refused, and existing children are checked on their own).
pub fn wrapper_tamper_reason(info: &SecurityInfo, role: Role) -> Option<String> {
    if !trusted(info.owner.as_deref(), &WRAPPER_TRUSTED) {
        return Some(format!("owner {} is not SYSTEM, Administrators or TrustedInstaller", show(&info.owner)));
    }
    let Some(dacl) = &info.dacl else {
        return Some("NULL DACL (everyone has full control)".into());
    };
    let bad = tamper_mask(role);
    for ace in dacl {
        if is_deny(ace.ace_type) || ace.flags & INHERIT_ONLY_ACE != 0 {
            continue;
        }
        if ace.ace_type != ACCESS_ALLOWED_ACE_TYPE {
            return Some(format!("allow ACE of type {:#x} is not understood", ace.ace_type));
        }
        if ace.mask & bad != 0 && !trusted(ace.sid.as_deref(), &WRAPPER_TRUSTED) {
            return Some(format!("{} has rights {:#x}", show(&ace.sid), ace.mask & bad));
        }
    }
    None
}

/// Pure: why the journal directory / file must not be trusted, or `None`. Owner must be
/// SYSTEM or Administrators and every non-deny ACE (inherit-only included) must be an
/// allow ACE for one of them.
pub fn journal_untrusted_reason(info: &SecurityInfo) -> Option<String> {
    if !trusted(info.owner.as_deref(), &JOURNAL_TRUSTED) {
        return Some(format!("owner {} is not SYSTEM or Administrators", show(&info.owner)));
    }
    let Some(dacl) = &info.dacl else {
        return Some("NULL DACL".into());
    };
    for ace in dacl {
        if is_deny(ace.ace_type) {
            continue;
        }
        if ace.ace_type != ACCESS_ALLOWED_ACE_TYPE || !trusted(ace.sid.as_deref(), &JOURNAL_TRUSTED) {
            return Some(format!("ACE type {:#x} for {} is not allowed", ace.ace_type, show(&ace.sid)));
        }
    }
    None
}

fn show(sid: &Option<String>) -> &str {
    sid.as_deref().unwrap_or("<unreadable SID>")
}

#[cfg(test)]
mod tests {
    use super::*;

    const USER: &str = "S-1-5-21-1111111111-2222222222-3333333333-1001";
    const AUTH_USERS: &str = "S-1-5-11";
    const USERS: &str = "S-1-5-32-545";
    const CREATOR_OWNER: &str = "S-1-3-0";
    /// Standard "Modify" (includes DELETE, add file, add subdir; not FILE_DELETE_CHILD).
    const MODIFY: u32 = 0x0013_01BF;
    const READ_EXECUTE: u32 = 0x0012_00A9;
    const FULL: u32 = 0x001F_01FF;

    fn info(owner: &str, dacl: Vec<Ace>) -> SecurityInfo {
        SecurityInfo { owner: Some(owner.into()), dacl: Some(dacl) }
    }

    fn program_files_like() -> Vec<Ace> {
        vec![
            Ace::allow(SID_TRUSTED_INSTALLER, FULL),
            Ace::allow(SID_SYSTEM, FULL),
            Ace::allow(SID_ADMINS, FULL),
            Ace::allow(USERS, READ_EXECUTE),
            Ace { ace_type: ACCESS_ALLOWED_ACE_TYPE, flags: 0x0B, mask: GENERIC_ALL, sid: Some(CREATOR_OWNER.into()) },
        ]
    }

    #[test]
    fn admin_only_tree_passes_every_role() {
        let i = info(SID_TRUSTED_INSTALLER, program_files_like());
        for role in [Role::File, Role::WrapperDir, Role::Ancestor] {
            assert_eq!(wrapper_tamper_reason(&i, role), None, "{role:?}");
        }
    }

    #[test]
    fn specific_user_sid_is_tamper() {
        // e.g. a wrapper unpacked under a user profile: the user's own SID has full control
        let mut d = program_files_like();
        d.push(Ace::allow(USER, FULL));
        assert!(wrapper_tamper_reason(&info(SID_ADMINS, d), Role::File).is_some());
    }

    #[test]
    fn non_admin_owner_is_tamper() {
        // the owner has implicit WRITE_DAC even with an admin-only DACL
        let r = wrapper_tamper_reason(&info(USER, program_files_like()), Role::File).unwrap();
        assert!(r.contains("owner"), "{r}");
        let none = SecurityInfo { owner: None, dacl: Some(program_files_like()) };
        assert!(wrapper_tamper_reason(&none, Role::Ancestor).is_some());
    }

    #[test]
    fn null_dacl_and_unknown_allow_types_are_tamper() {
        let i = SecurityInfo { owner: Some(SID_SYSTEM.into()), dacl: None };
        assert!(wrapper_tamper_reason(&i, Role::File).is_some());
        let mut d = program_files_like();
        d.push(Ace { ace_type: 0x9, flags: 0, mask: READ_EXECUTE, sid: Some(USERS.into()) }); // callback allow
        assert!(wrapper_tamper_reason(&info(SID_SYSTEM, d.clone()), Role::File).is_some());
        d.pop();
        d.push(Ace { ace_type: 0x5, flags: 0, mask: READ_EXECUTE, sid: Some(USERS.into()) }); // object allow
        assert!(wrapper_tamper_reason(&info(SID_SYSTEM, d), Role::File).is_some());
    }

    #[test]
    fn deny_and_inherit_only_aces_are_ignored() {
        let mut d = program_files_like();
        d.push(Ace { ace_type: ACCESS_DENIED_ACE_TYPE, flags: 0, mask: FULL, sid: Some(USER.into()) });
        d.push(Ace { ace_type: ACCESS_ALLOWED_ACE_TYPE, flags: INHERIT_ONLY_ACE | 0x3, mask: MODIFY, sid: Some(AUTH_USERS.into()) });
        assert_eq!(wrapper_tamper_reason(&info(SID_SYSTEM, d), Role::Ancestor), None);
    }

    #[test]
    fn inherited_modify_allows_rename_of_directories() {
        // C:\Tools\RDP Wrapper inheriting "Authenticated Users: Modify" from a folder under C:\
        let mut d = program_files_like();
        d.push(Ace { ace_type: ACCESS_ALLOWED_ACE_TYPE, flags: 0x10, mask: MODIFY, sid: Some(AUTH_USERS.into()) });
        let i = info(SID_ADMINS, d);
        assert!(wrapper_tamper_reason(&i, Role::WrapperDir).is_some());
        assert!(wrapper_tamper_reason(&i, Role::Ancestor).is_some(), "DELETE on an ancestor = rename");
        assert!(wrapper_tamper_reason(&i, Role::File).is_some());
    }

    #[test]
    fn adding_entries_is_tamper_only_in_the_wrapper_dir() {
        // C:\ style: Authenticated Users may create folders (append data / add subdir)
        let mut d = program_files_like();
        d.push(Ace::allow(AUTH_USERS, FILE_ADD_SUBDIRECTORY));
        d.push(Ace::allow(USERS, FILE_ADD_FILE));
        let i = info(SID_SYSTEM, d);
        assert_eq!(wrapper_tamper_reason(&i, Role::Ancestor), None);
        assert!(wrapper_tamper_reason(&i, Role::WrapperDir).is_some(), "DLL planting next to the wrapper");
        // FILE_DELETE_CHILD on an ancestor lets a user rename the child directory
        let mut d = program_files_like();
        d.push(Ace::allow(USERS, FILE_DELETE_CHILD));
        assert!(wrapper_tamper_reason(&info(SID_SYSTEM, d), Role::Ancestor).is_some());
    }

    #[test]
    fn read_execute_for_users_is_fine() {
        let d = vec![Ace::allow(SID_SYSTEM, FULL), Ace::allow(USERS, READ_EXECUTE), Ace::allow(USER, READ_EXECUTE)];
        for role in [Role::File, Role::WrapperDir, Role::Ancestor] {
            assert_eq!(wrapper_tamper_reason(&info(SID_SYSTEM, d.clone()), role), None);
        }
    }

    #[test]
    fn journal_rules() {
        let good = info(SID_SYSTEM, vec![
            Ace { ace_type: ACCESS_ALLOWED_ACE_TYPE, flags: 0x3, mask: FULL, sid: Some(SID_SYSTEM.into()) },
            Ace { ace_type: ACCESS_ALLOWED_ACE_TYPE, flags: 0x3, mask: FULL, sid: Some(SID_ADMINS.into()) },
        ]);
        assert_eq!(journal_untrusted_reason(&good), None);
        assert!(journal_untrusted_reason(&info(USER, good.dacl.clone().unwrap())).is_some());
        assert!(journal_untrusted_reason(&info(SID_TRUSTED_INSTALLER, good.dacl.clone().unwrap())).is_some());
        // %ProgramData% inherited "Users: create files" -> refuse, even a read-only ACE
        let mut d = good.dacl.clone().unwrap();
        d.push(Ace::allow(USERS, READ_EXECUTE));
        assert!(journal_untrusted_reason(&info(SID_ADMINS, d)).is_some());
        let mut d = good.dacl.clone().unwrap();
        d.push(Ace { ace_type: ACCESS_ALLOWED_ACE_TYPE, flags: INHERIT_ONLY_ACE, mask: FULL, sid: Some(CREATOR_OWNER.into()) });
        assert!(journal_untrusted_reason(&info(SID_ADMINS, d)).is_some());
        assert!(journal_untrusted_reason(&SecurityInfo { owner: Some(SID_SYSTEM.into()), dacl: None }).is_some());
        let mut d = good.dacl.unwrap();
        d.push(Ace { ace_type: ACCESS_DENIED_ACE_TYPE, flags: 0, mask: FULL, sid: Some(USERS.into()) });
        assert_eq!(journal_untrusted_reason(&info(SID_SYSTEM, d)), None);
    }
}
