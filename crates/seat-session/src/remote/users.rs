// SPDX-License-Identifier: GPL-3.0-only
//! Seat user checks (spec 30 §4, spec 20 §6.2): Net* APIs only, never the SAM hive.

use crate::remote::SessionError;
use serde::{Deserialize, Serialize};
use seat_core::remote::validate::UserFacts;
use seat_core::Secret;

/// Well-known SID of the builtin "Remote Desktop Users" group (name is localised).
pub const REMOTE_DESKTOP_USERS_SID: &str = "S-1-5-32-555";
/// Well-known SID of the builtin "Administrators" group.
pub const ADMINISTRATORS_SID: &str = "S-1-5-32-544";
/// Comment put on accounts we create.
pub const SEAT_USER_COMMENT: &str = "Pulsar Split seat user";
/// Longest password we pass on (also the helper frame limit is bytes, see `helper_proto`).
pub const PASSWORD_MAX_CHARS: usize = 127;

pub trait Accounts: Send + Sync {
    fn facts(&self, user: &str) -> Result<UserFacts, SessionError>;
    /// `"S-1-5-21-…"`.
    fn sid(&self, user: &str) -> Result<String, SessionError>;
    /// `NetUserAdd` level 1, normal user (not admin), password expiry left at the default.
    fn create(&self, user: &str, password: &Secret, comment: &str) -> Result<(), SessionError>;
    /// `NetUserSetInfo` level 1003.
    fn set_password(&self, user: &str, password: &Secret) -> Result<(), SessionError>;
    /// `NetLocalGroupGetMembers`; the group name is resolved from S-1-5-32-555.
    fn in_remote_desktop_users(&self, user: &str) -> Result<bool, SessionError>;
    /// `NetLocalGroupAddMembers`; never the SAM hive.
    fn add_to_remote_desktop_users(&self, user: &str) -> Result<(), SessionError>;
    /// Member of Administrators (warn only).
    fn is_admin(&self, user: &str) -> Result<bool, SessionError>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserCheck {
    pub sid: String,
    /// We added the user to Remote Desktop Users just now.
    pub added_to_rdu: bool,
    /// Seat users need not be admins; reported as a warning.
    pub is_admin: bool,
}

/// Account facts + RDU membership (spec 30 §14 reason codes `account`, `password_expired`,
/// `rdu_membership`). Profile creation is a separate step ([`Profiles`]).
pub fn ensure_seat_user<A: Accounts + ?Sized>(a: &A, user: &str) -> Result<UserCheck, SessionError> {
    seat_core::remote::validate::user_name(user).map_err(|e| SessionError::Account(e.message))?;
    if user.is_empty() {
        return Err(SessionError::Account("seat has no user".into()));
    }
    let f = a.facts(user)?;
    if !f.exists {
        return Err(SessionError::Account("user does not exist".into()));
    }
    if !f.enabled {
        return Err(SessionError::Account("user account is disabled".into()));
    }
    if f.locked_out {
        return Err(SessionError::Account("user account is locked out".into()));
    }
    if f.password_expired {
        return Err(SessionError::PasswordExpired);
    }
    let sid = a.sid(user)?;
    let mut added = false;
    let member = a.in_remote_desktop_users(user).map_err(|e| SessionError::RduMembership(e.to_string()))?;
    if !member {
        a.add_to_remote_desktop_users(user).map_err(|e| SessionError::RduMembership(e.to_string()))?;
        added = true;
        log::info!("added seat user to Remote Desktop Users");
    }
    let is_admin = a.is_admin(user).unwrap_or(false);
    if is_admin {
        log::warn!("a seat user is a member of Administrators");
    }
    Ok(UserCheck { sid, added_to_rdu: added, is_admin })
}

/// Password rules we enforce before a password reaches Windows (non-blank, no NUL, bounded).
pub fn check_password(password: &Secret) -> Result<(), SessionError> {
    let p = &password.0;
    if p.is_empty() {
        return Err(SessionError::Account("seat users must have a password".into()));
    }
    if p.contains('\0') || p.chars().count() > PASSWORD_MAX_CHARS {
        return Err(SessionError::Account("password contains NUL or is too long".into()));
    }
    Ok(())
}

/// Profile creation: `LogonUserW` + `LoadUserProfileW` inside the service (no helper).
pub trait Profiles: Send + Sync {
    fn ensure_profile(&self, user: &str, password: &Secret) -> Result<(), SessionError>;
}

/// `ERROR_LOGON_TYPE_NOT_GRANTED`: the password was accepted, the logon type is denied.
pub const LOGON_TYPE_NOT_GRANTED: u32 = 1385;

/// Credential test order (spec/remote 10 §4.4, rdp-listener 2026-11-17): an **interactive**
/// logon against the local machine first, as the spec says. Windows checks the password
/// before the logon right, so `ERROR_LOGON_TYPE_NOT_GRANTED` already proves the password;
/// seat users of RDP seats may be denied local logon on purpose (spec/local 30 §10), so
/// only then a network logon confirms the account (it also catches a password that
/// expired between the two calls). Every other result of the interactive logon is final.
pub fn credential_test_order(
    interactive: impl FnOnce() -> Result<(), u32>,
    network: impl FnOnce() -> Result<(), u32>,
) -> Result<(), u32> {
    match interactive() {
        Err(LOGON_TYPE_NOT_GRANTED) => network(),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_test_is_interactive_first() {
        let mut net = 0;
        assert_eq!(credential_test_order(|| Ok(()), || { net += 1; Ok(()) }), Ok(()));
        assert_eq!(net, 0, "no network logon after an interactive success");
        assert_eq!(credential_test_order(|| Err(1326), || Ok(())), Err(1326), "wrong password is final");
        assert_eq!(credential_test_order(|| Err(1331), || Ok(())), Err(1331), "disabled is final");
        assert_eq!(credential_test_order(|| Err(LOGON_TYPE_NOT_GRANTED), || Ok(())), Ok(()), "right denied: network confirms");
        assert_eq!(credential_test_order(|| Err(LOGON_TYPE_NOT_GRANTED), || Err(1330)), Err(1330));
    }
    use crate::remote::fake::FakeAccounts;

    #[test]
    fn ensure_adds_to_rdu_once() {
        let a = FakeAccounts::default();
        a.add_user("seat1", "S-1-5-21-1-2-3-1001", "pw");
        let c = ensure_seat_user(&a, "seat1").unwrap();
        assert!(c.added_to_rdu);
        assert_eq!(c.sid, "S-1-5-21-1-2-3-1001");
        assert!(!c.is_admin);
        let c = ensure_seat_user(&a, "SEAT1").unwrap();
        assert!(!c.added_to_rdu);
    }

    #[test]
    fn reason_codes() {
        let a = FakeAccounts::default();
        assert_eq!(ensure_seat_user(&a, "nobody").unwrap_err().code(), "account");
        assert_eq!(ensure_seat_user(&a, "").unwrap_err().code(), "account");
        assert_eq!(ensure_seat_user(&a, "bad/name").unwrap_err().code(), "account");
        a.add_user("u1", "S-1-5-21-1", "pw");
        a.with_facts("u1", |f| f.enabled = false);
        assert_eq!(ensure_seat_user(&a, "u1").unwrap_err().code(), "account");
        a.with_facts("u1", |f| {
            f.enabled = true;
            f.locked_out = true
        });
        assert_eq!(ensure_seat_user(&a, "u1").unwrap_err().code(), "account");
        a.with_facts("u1", |f| {
            f.locked_out = false;
            f.password_expired = true
        });
        assert_eq!(ensure_seat_user(&a, "u1").unwrap_err().code(), "password_expired");
        a.with_facts("u1", |f| f.password_expired = false);
        a.fail_rdu_add(true);
        assert_eq!(ensure_seat_user(&a, "u1").unwrap_err().code(), "rdu_membership");
    }

    #[test]
    fn admin_is_reported() {
        let a = FakeAccounts::default();
        a.add_user("boss", "S-1-5-21-9", "pw");
        a.set_admin("boss", true);
        assert!(ensure_seat_user(&a, "boss").unwrap().is_admin);
    }

    #[test]
    fn password_rules() {
        assert!(check_password(&Secret("x".into())).is_ok());
        assert_eq!(check_password(&Secret(String::new())).unwrap_err().code(), "account");
        assert!(check_password(&Secret("a\0b".into())).is_err());
        assert!(check_password(&Secret("p".repeat(128))).is_err());
    }
}
