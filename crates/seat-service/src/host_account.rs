// SPDX-License-Identifier: GPL-3.0-only
//! The seat hosts' local account (`seat_session::host_account`, architecture §3): create /
//! maintain it at service start, remove it at uninstall. System access goes through
//! [`AccountOps`] (Windows: `win::host_account::WinAccountOps`), so the order and the
//! refusals are tested here with a mock. **Untested on hardware.**
//!
//! [`ensure`] at every service start:
//!
//! 1. look the name up; a name that is not a local user, or a local user Pulsar did not
//!    create (its comment is not `ACCOUNT_COMMENT`, [`Lookup::Foreign`]), is refused
//!    (nothing touched; [`remove`] leaves it in place too);
//! 2. a new account is created with a fresh random password (`password_from_random`,
//!    32 bytes from the system RNG); an existing one is refused, untouched, when it is in
//!    any local group but `Users`, directly or through a domain group
//!    ([`AccountOps::local_groups`], [`check_groups`]): `Administrators`, but also
//!    `Backup Operators`, `Remote Desktop Users`, `Hyper-V Administrators`, … whose SIDs
//!    would stay enabled in the host token and grant their DACL access. A failed group
//!    lookup refuses too (fail closed). The new account is checked the same way;
//! 3. flags (password never expires, cannot be changed by the user, enabled), membership in
//!    `Users` (needed to load images from Program Files / System32), logon rights
//!    (`GRANT_RIGHTS` / `DENY_RIGHTS`), hidden from the logon screen;
//! 4. an existing account's password is rotated only now, as the last SAM step, and goes
//!    straight to LSA private data (`PASSWORD_SECRET`); the service reads it from there for
//!    every host logon, it is not kept in memory. When a step fails after the SAM password
//!    changed (new account, or rotation), the stored secret is deleted, so nothing reads a
//!    dead password later; the hosts use the SYSTEM Low token for this run.
//!
//! [`remove`] (uninstall): rights first (they are keyed by SID and would stay behind once
//! the account is gone), then the account, the logon-screen entry and the secret. Every
//! step runs even when an earlier one failed; the errors are returned.

use seat_core::Secret;
use seat_session::host_account::{
    ACCOUNT_COMMENT, ACCOUNT_FULL_NAME, ACCOUNT_NAME, DENY_RIGHTS, GRANT_RIGHTS, is_sid_string, password_from_random,
};

/// What a name resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    Missing,
    /// A local user account with this SID, created by Pulsar (its comment is
    /// `ACCOUNT_COMMENT`).
    User(String),
    /// A local user with this name that Pulsar did not create (another comment): never
    /// taken over, never deleted.
    Foreign,
    /// Something else (group, alias, domain account).
    Other,
}

pub trait AccountOps {
    fn lookup(&self, name: &str) -> Result<Lookup, String>;
    fn create(&self, name: &str, password: &Secret, full_name: &str, comment: &str) -> Result<(), String>;
    fn set_password(&self, name: &str, password: &Secret) -> Result<(), String>;
    /// Password never expires, user cannot change it, account enabled, normal account.
    fn set_flags(&self, name: &str) -> Result<(), String>;
    /// SIDs of every local group `name` is in, including through domain groups. `Err` when
    /// the membership cannot be read (the caller then refuses the account).
    fn local_groups(&self, name: &str) -> Result<Vec<String>, String>;
    /// Member of `BUILTIN\Users` (already a member = Ok).
    fn ensure_users_member(&self, sid: &str) -> Result<(), String>;
    fn add_rights(&self, sid: &str, rights: &[&str]) -> Result<(), String>;
    fn remove_all_rights(&self, sid: &str) -> Result<(), String>;
    fn hide_from_logon(&self, name: &str, hide: bool) -> Result<(), String>;
    fn store_password(&self, password: Option<&Secret>) -> Result<(), String>;
    fn delete(&self, name: &str) -> Result<(), String>;
    fn random32(&self) -> Result<zeroize::Zeroizing<[u8; 32]>, String>;
}

/// The account as the service uses it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostAccount {
    pub name: String,
    pub sid: String,
}

/// `BUILTIN\Users`: the only local group the account may be in.
pub const USERS_SID: &str = "S-1-5-32-545";
/// `BUILTIN\Administrators`.
pub const ADMINISTRATORS_SID: &str = "S-1-5-32-544";

/// The account's local groups (direct and through domain groups) hold nothing but `Users`.
/// `Err` = refused, with the reason; a failed lookup refuses too.
pub fn check_groups(ops: &dyn AccountOps, name: &str) -> Result<(), String> {
    let groups = ops.local_groups(name).map_err(|e| format!("{name}: group membership not readable, not used: {e}"))?;
    if groups.iter().any(|g| g.eq_ignore_ascii_case(ADMINISTRATORS_SID)) {
        return Err(format!("{name} is in Administrators; not used (remove it from the group)"));
    }
    let extra: Vec<&str> = groups.iter().map(String::as_str).filter(|g| !g.eq_ignore_ascii_case(USERS_SID)).collect();
    if !extra.is_empty() {
        return Err(format!("{name} is in local groups other than Users ({}); not used (remove it from them)", extra.join(", ")));
    }
    Ok(())
}

/// Create or maintain the account (see module doc). `Err` = the hosts use the SYSTEM Low
/// token for this run.
pub fn ensure(ops: &dyn AccountOps) -> Result<HostAccount, String> {
    let name = ACCOUNT_NAME;
    let bytes = ops.random32()?;
    let password = Secret::new(password_from_random(&bytes).to_string());
    drop(bytes);
    let (sid, created) = match ops.lookup(name)? {
        Lookup::Other => return Err(format!("{name} exists but is not a local user; not used")),
        Lookup::Foreign => return Err(format!("{name} is a local user Pulsar did not create; not used (rename or remove it)")),
        Lookup::User(sid) => {
            // Refused before anything is touched.
            check_groups(ops, name)?;
            (sid, false)
        }
        Lookup::Missing => {
            ops.create(name, &password, ACCOUNT_FULL_NAME, ACCOUNT_COMMENT)?;
            match ops.lookup(name) {
                Ok(Lookup::User(sid)) => (sid, true),
                other => {
                    forget_password(ops, name);
                    return Err(format!("{name} created but not found ({other:?})"));
                }
            }
        }
    };
    // From here on the SAM password may differ from the stored secret (always for a new
    // account; after the rotation for an existing one).
    let mut sam_changed = created;
    let mut steps = || {
        if !is_sid_string(&sid) {
            return Err(format!("{name}: unexpected SID {sid:?}"));
        }
        if created {
            check_groups(ops, name)?;
        }
        ops.set_flags(name)?;
        ops.ensure_users_member(&sid)?;
        ops.add_rights(&sid, GRANT_RIGHTS)?;
        ops.add_rights(&sid, DENY_RIGHTS)?;
        if let Err(e) = ops.hide_from_logon(name, true) {
            // Cosmetic only: the account cannot log on interactively anyway.
            crate::log::warn(&format!("{name}: not hidden from the logon screen: {e}"));
        }
        if !created {
            // Last SAM step: a failure above leaves the old password, which still matches
            // the stored secret.
            ops.set_password(name, &password)?;
            sam_changed = true;
        }
        ops.store_password(Some(&password))
    };
    if let Err(e) = steps() {
        if sam_changed {
            forget_password(ops, name);
        }
        return Err(e);
    }
    Ok(HostAccount { name: name.into(), sid })
}

/// The SAM password changed but the secret could not follow: delete the stale secret.
fn forget_password(ops: &dyn AccountOps, name: &str) {
    if let Err(e) = ops.store_password(None) {
        crate::log::warn(&format!("{name}: stale stored password not deleted: {e}"));
    }
}

/// Remove the account (uninstall). Returns the errors (Turkish, for the console output).
pub fn remove(ops: &dyn AccountOps) -> Vec<String> {
    let name = ACCOUNT_NAME;
    let mut errors = Vec::new();
    let mut foreign = false;
    match ops.lookup(name) {
        Ok(Lookup::User(sid)) => {
            if let Err(e) = ops.remove_all_rights(&sid) {
                errors.push(format!("{name} hesabının oturum hakları kaldırılamadı: {e}"));
            }
            if let Err(e) = ops.delete(name) {
                errors.push(format!("{name} hesabı silinemedi: {e}"));
            }
        }
        Ok(Lookup::Missing) => {}
        Ok(Lookup::Other) => errors.push(format!("{name} bir kullanıcı hesabı değil; dokunulmadı")),
        Ok(Lookup::Foreign) => {
            // Somebody else's account with our name: leave it (and its logon-screen entry).
            crate::log::warn(&format!("{name}: a local user Pulsar did not create; left untouched"));
            errors.push(format!("{name} hesabını Pulsar oluşturmamış; dokunulmadı"));
            foreign = true;
        }
        Err(e) => errors.push(format!("{name} hesabı aranamadı: {e}")),
    }
    if !foreign && let Err(e) = ops.hide_from_logon(name, false) {
        errors.push(format!("{name} oturum açma ekranı kaydı silinemedi: {e}"));
    }
    if let Err(e) = ops.store_password(None) {
        errors.push(format!("{name} parolası (LSA) silinemedi: {e}"));
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Mock {
        lookup: Mutex<Vec<Lookup>>,
        /// SIDs `local_groups` returns.
        groups: Vec<String>,
        /// `local_groups` fails.
        groups_fail: bool,
        fail: Option<&'static str>,
        calls: Mutex<Vec<String>>,
        stored: Mutex<Option<String>>,
        passwords: Mutex<Vec<String>>,
    }

    impl Mock {
        fn call(&self, c: &str) -> Result<(), String> {
            self.calls.lock().unwrap().push(c.to_string());
            if self.fail == Some(c) { Err(format!("{c} failed")) } else { Ok(()) }
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl AccountOps for Mock {
        fn lookup(&self, _n: &str) -> Result<Lookup, String> {
            self.call("lookup")?;
            let mut l = self.lookup.lock().unwrap();
            Ok(if l.len() > 1 { l.remove(0) } else { l[0].clone() })
        }
        fn create(&self, n: &str, p: &Secret, full: &str, _c: &str) -> Result<(), String> {
            assert_eq!(n, ACCOUNT_NAME);
            assert!(!full.is_empty());
            self.passwords.lock().unwrap().push(p.expose().to_string());
            self.call("create")
        }
        fn set_password(&self, _n: &str, p: &Secret) -> Result<(), String> {
            self.passwords.lock().unwrap().push(p.expose().to_string());
            self.call("set_password")
        }
        fn set_flags(&self, _n: &str) -> Result<(), String> {
            self.call("set_flags")
        }
        fn local_groups(&self, _n: &str) -> Result<Vec<String>, String> {
            self.call("groups")?;
            if self.groups_fail { Err("NetUserGetLocalGroups failed".into()) } else { Ok(self.groups.clone()) }
        }
        fn ensure_users_member(&self, _s: &str) -> Result<(), String> {
            self.call("users")
        }
        fn add_rights(&self, _s: &str, r: &[&str]) -> Result<(), String> {
            self.call(&format!("rights:{}", r.join(",")))
        }
        fn remove_all_rights(&self, _s: &str) -> Result<(), String> {
            self.call("remove_rights")
        }
        fn hide_from_logon(&self, _n: &str, hide: bool) -> Result<(), String> {
            self.call(if hide { "hide" } else { "unhide" })
        }
        fn store_password(&self, p: Option<&Secret>) -> Result<(), String> {
            *self.stored.lock().unwrap() = p.map(|s| s.expose().to_string());
            self.call(if p.is_some() { "store" } else { "unstore" })
        }
        fn delete(&self, _n: &str) -> Result<(), String> {
            self.call("delete")
        }
        fn random32(&self) -> Result<zeroize::Zeroizing<[u8; 32]>, String> {
            let mut b = [0u8; 32];
            for (i, x) in b.iter_mut().enumerate() {
                *x = (i as u8).wrapping_mul(37).wrapping_add(11);
            }
            Ok(zeroize::Zeroizing::new(b))
        }
    }

    const SID: &str = "S-1-5-21-1-2-3-1050";

    #[test]
    fn creates_a_missing_account_and_stores_its_password() {
        let m = Mock { lookup: Mutex::new(vec![Lookup::Missing, Lookup::User(SID.into())]), ..Default::default() };
        let a = ensure(&m).unwrap();
        assert_eq!(a, HostAccount { name: ACCOUNT_NAME.into(), sid: SID.into() });
        let calls = m.calls();
        assert_eq!(
            calls,
            [
                "lookup",
                "create",
                "lookup",
                "groups",
                "set_flags",
                "users",
                "rights:SeBatchLogonRight",
                "rights:SeDenyInteractiveLogonRight,SeDenyRemoteInteractiveLogonRight,SeDenyNetworkLogonRight,SeDenyServiceLogonRight",
                "hide",
                "store"
            ]
        );
        let pw = m.passwords.lock().unwrap()[0].clone();
        assert_eq!(m.stored.lock().unwrap().as_deref(), Some(pw.as_str()));
        assert_eq!(pw.len(), 32);
    }

    #[test]
    fn existing_account_gets_a_new_password_last() {
        let m = Mock { lookup: Mutex::new(vec![Lookup::User(SID.into())]), groups: vec![USERS_SID.into()], ..Default::default() };
        ensure(&m).unwrap();
        let calls = m.calls();
        assert_eq!(&calls[..2], ["lookup", "groups"]);
        assert_eq!(&calls[calls.len() - 2..], ["set_password", "store"]);
        let pw = m.passwords.lock().unwrap()[0].clone();
        assert_eq!(m.stored.lock().unwrap().as_deref(), Some(pw.as_str()));
    }

    #[test]
    fn admin_other_groups_or_foreign_names_are_refused_untouched() {
        for groups in [vec![ADMINISTRATORS_SID.to_string()], vec![USERS_SID.into(), "S-1-5-32-551".into()], vec!["S-1-5-32-555".into()]] {
            let m = Mock { lookup: Mutex::new(vec![Lookup::User(SID.into())]), groups: groups.clone(), ..Default::default() };
            assert!(ensure(&m).is_err(), "{groups:?}");
            assert_eq!(m.calls(), ["lookup", "groups"], "{groups:?}");
        }
        let m = Mock { lookup: Mutex::new(vec![Lookup::Other]), ..Default::default() };
        assert!(ensure(&m).is_err());
        assert_eq!(m.calls(), ["lookup"]);
    }

    #[test]
    fn a_user_pulsar_did_not_create_is_never_taken_over_or_deleted() {
        let m = Mock { lookup: Mutex::new(vec![Lookup::Foreign]), groups: vec![USERS_SID.into()], ..Default::default() };
        assert!(ensure(&m).unwrap_err().contains("did not create"));
        assert_eq!(m.calls(), ["lookup"]);
        let m = Mock { lookup: Mutex::new(vec![Lookup::Foreign]), ..Default::default() };
        let errs = remove(&m);
        assert_eq!(m.calls(), ["lookup", "unstore"], "no rights / delete / unhide");
        assert_eq!(errs.len(), 1);
        assert!(errs[0].contains("dokunulmadı"));
    }

    #[test]
    fn unreadable_groups_fail_closed() {
        let m = Mock { lookup: Mutex::new(vec![Lookup::User(SID.into())]), groups_fail: true, ..Default::default() };
        let e = ensure(&m).unwrap_err();
        assert!(e.contains("not readable"), "{e}");
        assert_eq!(m.calls(), ["lookup", "groups"]);
    }

    #[test]
    fn a_new_account_in_extra_groups_is_refused_and_its_secret_dropped() {
        let m = Mock {
            lookup: Mutex::new(vec![Lookup::Missing, Lookup::User(SID.into())]),
            groups: vec![USERS_SID.into(), "S-1-5-32-555".into()],
            ..Default::default()
        };
        assert!(ensure(&m).is_err());
        assert_eq!(m.calls(), ["lookup", "create", "lookup", "groups", "unstore"]);
    }

    #[test]
    fn a_failed_rights_step_keeps_the_old_password_and_secret() {
        let m = Mock { lookup: Mutex::new(vec![Lookup::User(SID.into())]), fail: Some("rights:SeBatchLogonRight"), ..Default::default() };
        assert!(ensure(&m).is_err());
        let calls = m.calls();
        assert!(!calls.contains(&"set_password".to_string()), "{calls:?}");
        assert!(!calls.contains(&"store".to_string()) && !calls.contains(&"unstore".to_string()), "{calls:?}");
    }

    #[test]
    fn a_failed_store_after_the_rotation_deletes_the_stale_secret() {
        let m = Mock { lookup: Mutex::new(vec![Lookup::User(SID.into())]), fail: Some("store"), ..Default::default() };
        assert!(ensure(&m).is_err());
        let calls = m.calls();
        assert_eq!(&calls[calls.len() - 3..], ["set_password", "store", "unstore"]);
        assert!(m.stored.lock().unwrap().is_none());
        // A failed rotation itself leaves the old password: the secret stays.
        let m = Mock { lookup: Mutex::new(vec![Lookup::User(SID.into())]), fail: Some("set_password"), ..Default::default() };
        assert!(ensure(&m).is_err());
        assert!(!m.calls().contains(&"unstore".to_string()));
    }

    #[test]
    fn hide_failure_is_not_fatal() {
        let m = Mock { lookup: Mutex::new(vec![Lookup::User(SID.into())]), fail: Some("hide"), ..Default::default() };
        assert!(ensure(&m).is_ok());
    }

    #[test]
    fn remove_takes_rights_before_the_account_and_runs_every_step() {
        let m = Mock { lookup: Mutex::new(vec![Lookup::User(SID.into())]), fail: Some("delete"), ..Default::default() };
        let errs = remove(&m);
        assert_eq!(m.calls(), ["lookup", "remove_rights", "delete", "unhide", "unstore"]);
        assert_eq!(errs.len(), 1);
        assert!(errs[0].contains("silinemedi"));
        let m = Mock { lookup: Mutex::new(vec![Lookup::Missing]), ..Default::default() };
        assert!(remove(&m).is_empty());
        assert_eq!(m.calls(), ["lookup", "unhide", "unstore"]);
    }
}
