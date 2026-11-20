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
//!
//! Per-seat accounts (round 4 local-gaps; `seat_session::host_account` module doc):
//!
//! - [`ensure_group`] at service start: the local group `GROUP_NAME` (created with
//!   `GROUP_COMMENT`; a group of that name Pulsar did not create is refused, and so are
//!   per-seat accounts for that run). The control pipe admits the group.
//! - [`ensure_account`] runs the steps above for any host account name with its own LSA
//!   secret and, when the group is known, also makes the account a member of it (the
//!   group is the one local group besides `Users` a host account may be in).
//!   [`ensure_seat`] = the account of one seat (`seat_account_name`), called the first
//!   time that seat's host starts in a service run.
//! - [`prune`] at service start: accounts of seats that are not configured any more go
//!   ([`remove_seat_account`]: rights, profile, account, logon-screen entry, secret).
//! - [`remove_all`] (uninstall): the shared account, every seat account and the group.

use seat_core::{SeatNumber, Secret};
use seat_session::host_account::{
    ACCOUNT_COMMENT, ACCOUNT_FULL_NAME, ACCOUNT_NAME, DENY_RIGHTS, GRANT_RIGHTS, GROUP_COMMENT, GROUP_NAME, PASSWORD_SECRET,
    is_sid_string, password_from_random, seat_account_name, seat_account_seats, seat_password_secret,
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
    /// Store (`Some`) or delete (`None`) the password in the LSA private data `secret`.
    fn store_password(&self, secret: &str, password: Option<&Secret>) -> Result<(), String>;
    fn delete(&self, name: &str) -> Result<(), String>;
    fn random32(&self) -> Result<zeroize::Zeroizing<[u8; 32]>, String>;
    /// The host accounts' local group.
    fn group_lookup(&self, name: &str) -> Result<GroupLookup, String> {
        let _ = name;
        Ok(GroupLookup::Missing)
    }
    fn create_group(&self, name: &str, comment: &str) -> Result<(), String> {
        let _ = (name, comment);
        Err("not supported".into())
    }
    /// Member of the local group `group` (already a member = Ok).
    fn add_to_group(&self, group: &str, sid: &str) -> Result<(), String> {
        let _ = (group, sid);
        Err("not supported".into())
    }
    fn delete_group(&self, name: &str) -> Result<(), String> {
        let _ = name;
        Ok(())
    }
    /// Delete the user profile of `sid` (folder + registry); a missing profile is Ok.
    fn delete_profile(&self, sid: &str) -> Result<(), String> {
        let _ = sid;
        Ok(())
    }
    /// Enable or disable the account (`UF_ACCOUNTDISABLE`), keeping its other flags.
    fn set_enabled(&self, name: &str, enabled: bool) -> Result<(), String> {
        let _ = (name, enabled);
        Err("not supported".into())
    }
}

/// What the host group name resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupLookup {
    Missing,
    /// The local group Pulsar created (comment `GROUP_COMMENT`), with its SID.
    Ours(String),
    /// A local group or account of that name Pulsar did not create: never used or deleted.
    Foreign,
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

/// The account's local groups (direct and through domain groups) hold nothing but `Users`
/// (and the host group `host_group`, when known). `Err` = refused, with the reason; a
/// failed lookup refuses too.
pub fn check_groups(ops: &dyn AccountOps, name: &str, host_group: Option<&str>) -> Result<(), String> {
    let groups = ops.local_groups(name).map_err(|e| format!("{name}: group membership not readable, not used: {e}"))?;
    if groups.iter().any(|g| g.eq_ignore_ascii_case(ADMINISTRATORS_SID)) {
        return Err(format!("{name} is in Administrators; not used (remove it from the group)"));
    }
    let extra: Vec<&str> = groups
        .iter()
        .map(String::as_str)
        .filter(|g| !g.eq_ignore_ascii_case(USERS_SID) && !host_group.is_some_and(|h| g.eq_ignore_ascii_case(h)))
        .collect();
    if !extra.is_empty() {
        return Err(format!("{name} is in local groups other than Users ({}); not used (remove it from them)", extra.join(", ")));
    }
    Ok(())
}

/// Create or maintain the shared account (see module doc), without the group. `Err` = the
/// hosts use the SYSTEM Low token for this run.
#[cfg(test)]
pub fn ensure(ops: &dyn AccountOps) -> Result<HostAccount, String> {
    ensure_account(ops, ACCOUNT_NAME, PASSWORD_SECRET, None)
}

/// The host group (created when missing). `Err` = no per-seat accounts in this run.
pub fn ensure_group(ops: &dyn AccountOps) -> Result<String, String> {
    match ops.group_lookup(GROUP_NAME)? {
        GroupLookup::Ours(sid) if is_sid_string(&sid) => Ok(sid),
        GroupLookup::Ours(sid) => Err(format!("{GROUP_NAME}: unexpected SID {sid:?}")),
        GroupLookup::Foreign => Err(format!("{GROUP_NAME} exists but Pulsar did not create it; per-seat host accounts are off (rename or remove it)")),
        GroupLookup::Missing => {
            ops.create_group(GROUP_NAME, GROUP_COMMENT.get())?;
            match ops.group_lookup(GROUP_NAME)? {
                GroupLookup::Ours(sid) if is_sid_string(&sid) => Ok(sid),
                other => Err(format!("{GROUP_NAME} created but not found ({other:?})")),
            }
        }
    }
}

/// The account of `seat`'s host (created or maintained, password rotated; member of the
/// host group `group_sid`).
pub fn ensure_seat(ops: &dyn AccountOps, seat: SeatNumber, group_sid: &str) -> Result<HostAccount, String> {
    ensure_account(ops, &seat_account_name(seat), &seat_password_secret(seat), Some(group_sid))
}

/// Create or maintain the host account `name` whose password lives in the LSA private data
/// `secret`; with `group_sid` also a member of the host group (see module doc).
pub fn ensure_account(ops: &dyn AccountOps, name: &str, secret: &str, group_sid: Option<&str>) -> Result<HostAccount, String> {
    ensure_account_as(ops, name, secret, group_sid, &HOST_PROFILE)
}

/// What kind of account [`ensure_account_as`] maintains.
#[derive(Debug, Clone, Copy)]
pub struct AccountProfile {
    pub full_name: &'static seat_core::i18n::Text,
    pub comment: &'static seat_core::i18n::Text,
    pub grant: &'static [&'static str],
    pub deny: &'static [&'static str],
}

/// The seat hosts' accounts (batch logon only).
pub const HOST_PROFILE: AccountProfile = AccountProfile { full_name: &ACCOUNT_FULL_NAME, comment: &ACCOUNT_COMMENT, grant: GRANT_RIGHTS, deny: DENY_RIGHTS };

/// The kiosk seats' accounts (`seat_session::kiosk_account`: interactive logon only).
pub const KIOSK_PROFILE: AccountProfile = AccountProfile {
    full_name: &seat_session::kiosk_account::KIOSK_FULL_NAME,
    comment: &seat_session::kiosk_account::KIOSK_COMMENT,
    grant: seat_session::kiosk_account::KIOSK_GRANT_RIGHTS,
    deny: seat_session::kiosk_account::KIOSK_DENY_RIGHTS,
};

/// The Pulsar kiosk account of `seat` (`PulsarKiosk<NN>`, created or maintained, password
/// rotated, `Users` only, no host group). The password goes to its own LSA secret.
pub fn ensure_kiosk(ops: &dyn AccountOps, seat: SeatNumber) -> Result<HostAccount, String> {
    use seat_session::kiosk_account::{kiosk_account_name, kiosk_password_secret};
    ensure_account_as(ops, &kiosk_account_name(seat), &kiosk_password_secret(seat), None, &KIOSK_PROFILE)
}

/// The console account of the host console preset (`seat_session::console_account`:
/// interactive logon only, never batch / RDP / network / service).
pub const CONSOLE_PROFILE: AccountProfile = AccountProfile {
    full_name: &seat_session::console_account::CONSOLE_FULL_NAME,
    comment: &seat_session::console_account::CONSOLE_COMMENT,
    grant: seat_session::console_account::CONSOLE_GRANT_RIGHTS,
    deny: seat_session::console_account::CONSOLE_DENY_RIGHTS,
};

/// Pulsar's console account (`PulsarSeatConsole`, created or maintained, password rotated,
/// `Users` only). Returns the new password too: the caller hands the same value to the
/// Winlogon auto-logon secret, so the two never differ (seat-service `host_console`).
pub fn ensure_console(ops: &dyn AccountOps) -> Result<(HostAccount, Secret), String> {
    use seat_session::console_account::{CONSOLE_ACCOUNT_NAME, CONSOLE_PASSWORD_SECRET};
    ensure_account_keep(ops, CONSOLE_ACCOUNT_NAME, CONSOLE_PASSWORD_SECRET, None, &CONSOLE_PROFILE)
}

/// Remove Pulsar's console account with its profile and stored password (preset off,
/// uninstall). A name Pulsar did not create is left alone. Returns the errors.
pub fn remove_console_account(ops: &dyn AccountOps) -> Vec<String> {
    use seat_session::console_account::{CONSOLE_ACCOUNT_NAME, CONSOLE_PASSWORD_SECRET};
    remove_named_with(ops, CONSOLE_ACCOUNT_NAME, CONSOLE_PASSWORD_SECRET, true)
}

/// [`ensure_account`] for any [`AccountProfile`].
pub fn ensure_account_as(ops: &dyn AccountOps, name: &str, secret: &str, group_sid: Option<&str>, profile: &AccountProfile) -> Result<HostAccount, String> {
    ensure_account_keep(ops, name, secret, group_sid, profile).map(|(a, _)| a)
}

/// [`ensure_account_as`] that also returns the password it set (and stored in `secret`).
fn ensure_account_keep(ops: &dyn AccountOps, name: &str, secret: &str, group_sid: Option<&str>, profile: &AccountProfile) -> Result<(HostAccount, Secret), String> {
    let bytes = ops.random32()?;
    let password = Secret::new(password_from_random(&bytes).to_string());
    drop(bytes);
    let (sid, created) = match ops.lookup(name)? {
        Lookup::Other => return Err(format!("{name} exists but is not a local user; not used")),
        Lookup::Foreign => return Err(format!("{name} is a local user Pulsar did not create; not used (rename or remove it)")),
        Lookup::User(sid) => {
            // Refused before anything is touched.
            check_groups(ops, name, group_sid)?;
            (sid, false)
        }
        Lookup::Missing => {
            ops.create(name, &password, profile.full_name.get(), profile.comment.get())?;
            match ops.lookup(name) {
                Ok(Lookup::User(sid)) => (sid, true),
                other => {
                    forget_password(ops, name, secret);
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
            check_groups(ops, name, group_sid)?;
        }
        ops.set_flags(name)?;
        ops.ensure_users_member(&sid)?;
        if group_sid.is_some() {
            // Without it the host could not open the control pipe (one ACE for the group).
            ops.add_to_group(GROUP_NAME, &sid).map_err(|e| format!("{name}: not added to {GROUP_NAME}: {e}"))?;
        }
        ops.add_rights(&sid, profile.grant)?;
        ops.add_rights(&sid, profile.deny)?;
        if let Err(e) = ops.hide_from_logon(name, true) {
            // Cosmetic only: a host account cannot log on interactively anyway, and a
            // kiosk account's password is random (nobody can type it).
            crate::log::warn(&format!("{name}: not hidden from the logon screen: {e}"));
        }
        if !created {
            // Last SAM step: a failure above leaves the old password, which still matches
            // the stored secret.
            ops.set_password(name, &password)?;
            sam_changed = true;
        }
        ops.store_password(secret, Some(&password))
    };
    if let Err(e) = steps() {
        if sam_changed {
            forget_password(ops, name, secret);
        }
        return Err(e);
    }
    Ok((HostAccount { name: name.into(), sid }, password))
}

/// Console copy of the uninstall steps (English default, Turkish locale; `seat_core::i18n`).
/// `{0}` = account or group name, `{1}` = the error.
mod copy {
    use seat_core::i18n::Text;
    pub const RIGHTS: Text = Text::new("{0}: the account's sign-in rights could not be removed: {1}", "{0} hesabının oturum hakları kaldırılamadı: {1}");
    pub const DELETE: Text = Text::new("{0}: the account could not be deleted: {1}", "{0} hesabı silinemedi: {1}");
    pub const NOT_USER: Text = Text::new("{0} is not a user account; left untouched", "{0} bir kullanıcı hesabı değil; dokunulmadı");
    pub const FOREIGN: Text = Text::new("{0}: Pulsar did not create this account; left untouched", "{0} hesabını Pulsar oluşturmamış; dokunulmadı");
    pub const LOOKUP: Text = Text::new("{0}: the account could not be looked up: {1}", "{0} hesabı aranamadı: {1}");
    pub const HIDE: Text = Text::new("{0}: the sign-in screen entry could not be deleted: {1}", "{0} oturum açma ekranı kaydı silinemedi: {1}");
    pub const SECRET: Text = Text::new("{0}: the password (LSA) could not be deleted: {1}", "{0} parolası (LSA) silinemedi: {1}");
    pub const PROFILE: Text = Text::new("{0}: the account's user profile could not be deleted: {1}", "{0} hesabının kullanıcı profili silinemedi: {1}");
    pub const GROUP_FOREIGN: Text = Text::new("{0}: Pulsar did not create this group; left untouched", "{0} grubunu Pulsar oluşturmamış; dokunulmadı");
    pub const GROUP_LOOKUP: Text = Text::new("{0}: the group could not be looked up: {1}", "{0} grubu aranamadı: {1}");
    #[cfg(test)]
    pub const ALL: &[(&str, &Text)] =
        seat_core::text_table![RIGHTS, DELETE, NOT_USER, FOREIGN, LOOKUP, HIDE, SECRET, PROFILE, GROUP_FOREIGN, GROUP_LOOKUP];
}

/// The SAM password changed but the secret could not follow: delete the stale secret.
fn forget_password(ops: &dyn AccountOps, name: &str, secret: &str) {
    if let Err(e) = ops.store_password(secret, None) {
        crate::log::warn(&format!("{name}: stale stored password not deleted: {e}"));
    }
}

/// Remove the account (uninstall). Returns the errors (localized, for the console output).
pub fn remove(ops: &dyn AccountOps) -> Vec<String> {
    let name = ACCOUNT_NAME;
    let mut errors = Vec::new();
    let mut foreign = false;
    match ops.lookup(name) {
        Ok(Lookup::User(sid)) => {
            if let Err(e) = ops.remove_all_rights(&sid) {
                errors.push(copy::RIGHTS.fmt(&[&name, &e]));
            }
            if let Err(e) = ops.delete(name) {
                errors.push(copy::DELETE.fmt(&[&name, &e]));
            }
        }
        Ok(Lookup::Missing) => {}
        Ok(Lookup::Other) => errors.push(copy::NOT_USER.fmt(&[&name])),
        Ok(Lookup::Foreign) => {
            // Somebody else's account with our name: leave it (and its logon-screen entry).
            crate::log::warn(&format!("{name}: a local user Pulsar did not create; left untouched"));
            errors.push(copy::FOREIGN.fmt(&[&name]));
            foreign = true;
        }
        Err(e) => errors.push(copy::LOOKUP.fmt(&[&name, &e])),
    }
    if !foreign && let Err(e) = ops.hide_from_logon(name, false) {
        errors.push(copy::HIDE.fmt(&[&name, &e]));
    }
    if let Err(e) = ops.store_password(PASSWORD_SECRET, None) {
        errors.push(copy::SECRET.fmt(&[&name, &e]));
    }
    errors
}

/// Remove `seat`'s host account (seat no longer configured, uninstall): rights, profile,
/// account, logon-screen entry, secret. A name Pulsar did not create is left alone.
/// Returns the errors (localized).
pub fn remove_seat_account(ops: &dyn AccountOps, seat: SeatNumber) -> Vec<String> {
    remove_named(ops, &seat_account_name(seat), &seat_password_secret(seat))
}

/// Remove `seat`'s Pulsar kiosk account (seat no longer a kiosk seat, uninstall), with its
/// profile. Returns the errors (localized).
pub fn remove_kiosk_account(ops: &dyn AccountOps, seat: SeatNumber) -> Vec<String> {
    use seat_session::kiosk_account::{kiosk_account_name, kiosk_password_secret};
    remove_named(ops, &kiosk_account_name(seat), &kiosk_password_secret(seat))
}

/// Uninstall: remove the kiosk accounts of seats not in `keep` (their profiles too only
/// when `with_profiles`, the uninstall's "remove user data" choice). Returns the errors.
pub fn prune_kiosk(ops: &dyn AccountOps, keep: &[SeatNumber], with_profiles: bool) -> Vec<String> {
    use seat_session::kiosk_account::{kiosk_account_name, kiosk_account_seats, kiosk_password_secret};
    let mut errors = Vec::new();
    for seat in kiosk_account_seats().filter(|s| !keep.contains(s)) {
        match ops.lookup(&kiosk_account_name(seat)) {
            Ok(Lookup::User(_)) => errors.extend(remove_named_with(ops, &kiosk_account_name(seat), &kiosk_password_secret(seat), with_profiles)),
            Ok(_) => {}
            Err(e) => errors.push(format!("{}: {e}", kiosk_account_name(seat))),
        }
    }
    errors
}

/// Service start (parity review 2026-11-17): the kiosk accounts of seats that are no longer
/// kiosk seats are only **disabled** (`UF_ACCOUNTDISABLE`) and lose their stored password;
/// the account and its profile (saves, browser profiles) stay, so turning the seat's kiosk
/// shell off for a while never destroys its data. [`ensure_kiosk`] enables the account
/// again with a new password. Deleting happens only at uninstall ([`remove_all`]) or on the
/// explicit [`remove_kiosk_user`]. Returns the errors.
pub fn disable_unused_kiosk(ops: &dyn AccountOps, keep: &[SeatNumber]) -> Vec<String> {
    use seat_session::kiosk_account::{kiosk_account_name, kiosk_account_seats, kiosk_password_secret};
    let mut errors = Vec::new();
    for seat in kiosk_account_seats().filter(|s| !keep.contains(s)) {
        let name = kiosk_account_name(seat);
        match ops.lookup(&name) {
            Ok(Lookup::User(_)) => {
                if let Err(e) = ops.set_enabled(&name, false) {
                    errors.push(format!("{name}: disable: {e}"));
                }
                if let Err(e) = ops.store_password(&kiosk_password_secret(seat), None) {
                    errors.push(copy::SECRET.fmt(&[&name, &e]));
                }
            }
            Ok(_) => {}
            Err(e) => errors.push(format!("{name}: {e}")),
        }
    }
    errors
}

/// The manager's explicit "remove this seat's Windows user" (admin): the kiosk account of
/// `seat`, its profile and its stored password. Refused while the seat is a kiosk seat
/// (`in_use`). Returns the errors (empty = done or nothing to remove).
pub fn remove_kiosk_user(ops: &dyn AccountOps, seat: SeatNumber, in_use: bool) -> Result<Vec<String>, String> {
    if in_use {
        return Err("the seat still runs a kiosk shell as this user".into());
    }
    Ok(remove_kiosk_account(ops, seat))
}

fn remove_named(ops: &dyn AccountOps, name: &str, secret: &str) -> Vec<String> {
    remove_named_with(ops, name, secret, true)
}

fn remove_named_with(ops: &dyn AccountOps, name: &str, secret: &str, with_profile: bool) -> Vec<String> {
    let name = name.to_string();
    let mut errors = Vec::new();
    match ops.lookup(&name) {
        Ok(Lookup::User(sid)) => {
            if let Err(e) = ops.remove_all_rights(&sid) {
                errors.push(copy::RIGHTS.fmt(&[&name, &e]));
            }
            if with_profile && let Err(e) = ops.delete_profile(&sid) {
                errors.push(copy::PROFILE.fmt(&[&name, &e]));
            }
            if let Err(e) = ops.delete(&name) {
                errors.push(copy::DELETE.fmt(&[&name, &e]));
            }
            if let Err(e) = ops.hide_from_logon(&name, false) {
                errors.push(copy::HIDE.fmt(&[&name, &e]));
            }
        }
        Ok(Lookup::Missing) => {}
        Ok(Lookup::Foreign | Lookup::Other) => {
            crate::log::warn(&format!("{name}: not an account Pulsar created; left untouched"));
            return errors;
        }
        Err(e) => {
            errors.push(copy::LOOKUP.fmt(&[&name, &e]));
            return errors;
        }
    }
    if let Err(e) = ops.store_password(secret, None) {
        errors.push(copy::SECRET.fmt(&[&name, &e]));
    }
    errors
}

/// Service start: remove the accounts of seats that are not in `keep` (seat removed from
/// the config, or a seat that no longer runs a host). Returns the errors.
pub fn prune(ops: &dyn AccountOps, keep: &[SeatNumber]) -> Vec<String> {
    let mut errors = Vec::new();
    for seat in seat_account_seats().filter(|s| !keep.contains(s)) {
        // Cheap check first: most seat numbers never had an account.
        match ops.lookup(&seat_account_name(seat)) {
            Ok(Lookup::User(_)) => errors.extend(remove_seat_account(ops, seat)),
            Ok(_) => {}
            Err(e) => errors.push(format!("{}: {e}", seat_account_name(seat))),
        }
    }
    errors
}

/// Uninstall: the shared account, every seat account, Pulsar's kiosk accounts (their
/// profiles only with `remove_user_data`) and the host group. Returns the errors
/// (localized).
pub fn remove_all(ops: &dyn AccountOps, remove_user_data: bool) -> Vec<String> {
    let mut errors = remove(ops);
    for seat in seat_account_seats() {
        if let Ok(Lookup::User(_)) = ops.lookup(&seat_account_name(seat)) {
            errors.extend(remove_seat_account(ops, seat));
        }
    }
    errors.extend(prune_kiosk(ops, &[], remove_user_data));
    // The host console preset's account (its Winlogon values are restored before this, by
    // the preset's own journal; a leftover account is only ever ours by its comment).
    if let Ok(Lookup::User(_)) = ops.lookup(seat_session::console_account::CONSOLE_ACCOUNT_NAME) {
        errors.extend(remove_console_account(ops));
    }
    match ops.group_lookup(GROUP_NAME) {
        Ok(GroupLookup::Ours(_)) => {
            if let Err(e) = ops.delete_group(GROUP_NAME) {
                errors.push(format!("{GROUP_NAME} grubu silinemedi: {e}"));
            }
        }
        Ok(GroupLookup::Missing) => {}
        Ok(GroupLookup::Foreign) => errors.push(copy::GROUP_FOREIGN.fmt(&[&GROUP_NAME])),
        Err(e) => errors.push(copy::GROUP_LOOKUP.fmt(&[&GROUP_NAME, &e])),
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
        /// Secret names written / deleted.
        secrets: Mutex<Vec<String>>,
        /// Answers of `group_lookup` (last one repeats).
        group: Mutex<Vec<GroupLookup>>,
        /// Per-name lookups (`lookup` falls back to the `lookup` queue when absent).
        named: Mutex<std::collections::HashMap<String, Lookup>>,
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
        fn lookup(&self, n: &str) -> Result<Lookup, String> {
            if let Some(l) = self.named.lock().unwrap().get(n).cloned() {
                self.calls.lock().unwrap().push(format!("lookup {n}"));
                return Ok(l);
            }
            self.call("lookup")?;
            let mut l = self.lookup.lock().unwrap();
            Ok(if l.len() > 1 { l.remove(0) } else { l[0].clone() })
        }
        fn create(&self, n: &str, p: &Secret, full: &str, _c: &str) -> Result<(), String> {
            assert!(
                seat_session::host_account::is_host_account_name(n)
                    || seat_session::kiosk_account::is_kiosk_account_name(n)
                    || seat_session::console_account::is_console_account_name(n),
                "{n}"
            );
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
        fn store_password(&self, secret: &str, p: Option<&Secret>) -> Result<(), String> {
            self.secrets.lock().unwrap().push(format!("{}{secret}", if p.is_some() { "+" } else { "-" }));
            *self.stored.lock().unwrap() = p.map(|s| s.expose().to_string());
            self.call(if p.is_some() { "store" } else { "unstore" })
        }
        fn delete(&self, _n: &str) -> Result<(), String> {
            self.call("delete")
        }
        fn group_lookup(&self, _n: &str) -> Result<GroupLookup, String> {
            self.call("group_lookup")?;
            let mut g = self.group.lock().unwrap();
            Ok(if g.len() > 1 { g.remove(0) } else { g.first().cloned().unwrap_or(GroupLookup::Missing) })
        }
        fn create_group(&self, n: &str, c: &str) -> Result<(), String> {
            assert_eq!(n, GROUP_NAME);
            assert!(GROUP_COMMENT.matches(c));
            self.call("create_group")
        }
        fn add_to_group(&self, g: &str, _s: &str) -> Result<(), String> {
            assert_eq!(g, GROUP_NAME);
            self.call("add_to_group")
        }
        fn delete_group(&self, _n: &str) -> Result<(), String> {
            self.call("delete_group")
        }
        fn delete_profile(&self, _s: &str) -> Result<(), String> {
            self.call("delete_profile")
        }
        fn set_enabled(&self, _n: &str, enabled: bool) -> Result<(), String> {
            self.call(if enabled { "enable" } else { "disable" })
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

    const GROUP_SID: &str = "S-1-5-21-1-2-3-1100";

    fn s(n: u8) -> SeatNumber {
        SeatNumber::new(n).unwrap()
    }

    #[test]
    fn copy_table_is_complete() {
        let p = seat_core::i18n::check_table(copy::ALL);
        assert!(p.is_empty(), "{p:?}");
    }

    #[test]
    fn group_is_created_once_and_a_foreign_one_is_refused() {
        let m = Mock { group: Mutex::new(vec![GroupLookup::Missing, GroupLookup::Ours(GROUP_SID.into())]), ..Default::default() };
        assert_eq!(ensure_group(&m).unwrap(), GROUP_SID);
        assert_eq!(m.calls(), ["group_lookup", "create_group", "group_lookup"]);
        let m = Mock { group: Mutex::new(vec![GroupLookup::Ours(GROUP_SID.into())]), ..Default::default() };
        assert_eq!(ensure_group(&m).unwrap(), GROUP_SID);
        assert_eq!(m.calls(), ["group_lookup"]);
        let m = Mock { group: Mutex::new(vec![GroupLookup::Foreign]), ..Default::default() };
        assert!(ensure_group(&m).unwrap_err().contains("did not create"));
        assert_eq!(m.calls(), ["group_lookup"], "never created over a foreign name");
    }

    #[test]
    fn seat_account_has_its_own_secret_and_joins_the_group() {
        let m = Mock { lookup: Mutex::new(vec![Lookup::Missing, Lookup::User(SID.into())]), groups: vec![USERS_SID.into(), GROUP_SID.into()], ..Default::default() };
        let a = ensure_seat(&m, s(3), GROUP_SID).unwrap();
        assert_eq!(a.name, "PulsarSeatHost03");
        let calls = m.calls();
        assert!(calls.contains(&"add_to_group".to_string()), "{calls:?}");
        assert_eq!(*m.secrets.lock().unwrap(), ["+L$PulsarSeat.HostAccount.03"]);
        // The host group is allowed next to Users; any other group still refuses.
        let m = Mock { lookup: Mutex::new(vec![Lookup::User(SID.into())]), groups: vec![USERS_SID.into(), GROUP_SID.into()], ..Default::default() };
        assert!(ensure_seat(&m, s(3), GROUP_SID).is_ok());
        let m = Mock { lookup: Mutex::new(vec![Lookup::User(SID.into())]), groups: vec![GROUP_SID.into(), "S-1-5-32-555".into()], ..Default::default() };
        assert!(ensure_seat(&m, s(3), GROUP_SID).is_err());
        // Without the group (shared account path), the group SID is not allowed.
        let m = Mock { lookup: Mutex::new(vec![Lookup::User(SID.into())]), groups: vec![GROUP_SID.into()], ..Default::default() };
        assert!(ensure(&m).is_err());
    }

    #[test]
    fn a_failed_group_join_drops_the_new_secret() {
        let m = Mock { lookup: Mutex::new(vec![Lookup::User(SID.into())]), fail: Some("add_to_group"), ..Default::default() };
        assert!(ensure_seat(&m, s(2), GROUP_SID).unwrap_err().contains(GROUP_NAME));
        assert!(!m.calls().contains(&"set_password".to_string()), "the old password still matches the stored secret");
    }

    #[test]
    fn prune_removes_only_unconfigured_seat_accounts_pulsar_created() {
        let m = Mock { lookup: Mutex::new(vec![Lookup::Missing]), ..Default::default() };
        m.named.lock().unwrap().insert("PulsarSeatHost02".into(), Lookup::User(SID.into()));
        m.named.lock().unwrap().insert("PulsarSeatHost05".into(), Lookup::User("S-1-5-21-1-2-3-1055".into()));
        m.named.lock().unwrap().insert("PulsarSeatHost07".into(), Lookup::Foreign);
        assert!(prune(&m, &[s(1), s(2)]).is_empty());
        let calls = m.calls();
        assert!(!calls.iter().any(|c| c == "lookup PulsarSeatHost02"), "kept seats are not even looked up");
        let removed: Vec<&String> = calls.iter().filter(|c| *c == "delete").collect();
        assert_eq!(removed.len(), 1, "only seat 5 is ours and unconfigured: {calls:?}");
        assert!(calls.contains(&"delete_profile".to_string()));
        assert_eq!(*m.secrets.lock().unwrap(), ["-L$PulsarSeat.HostAccount.05"]);
    }

    #[test]
    fn remove_all_takes_seat_accounts_and_the_group() {
        let m = Mock { lookup: Mutex::new(vec![Lookup::Missing]), group: Mutex::new(vec![GroupLookup::Ours(GROUP_SID.into())]), ..Default::default() };
        m.named.lock().unwrap().insert("PulsarSeatHost04".into(), Lookup::User(SID.into()));
        assert!(remove_all(&m, true).is_empty());
        let calls = m.calls();
        assert_eq!(calls.iter().filter(|c| *c == "delete").count(), 1);
        assert_eq!(calls.last().unwrap(), "delete_group");
        let m = Mock { lookup: Mutex::new(vec![Lookup::Missing]), group: Mutex::new(vec![GroupLookup::Foreign]), ..Default::default() };
        let errs = remove_all(&m, true);
        assert!(!m.calls().contains(&"delete_group".to_string()));
        assert_eq!(errs.len(), 1);
        // Kiosk accounts go at uninstall too, profile included.
        let m = Mock { lookup: Mutex::new(vec![Lookup::Missing]), group: Mutex::new(vec![GroupLookup::Missing]), ..Default::default() };
        m.named.lock().unwrap().insert("PulsarKiosk03".into(), Lookup::User(SID.into()));
        assert!(remove_all(&m, true).is_empty());
        assert!(m.calls().contains(&"delete_profile".to_string()));
        assert_eq!(*m.secrets.lock().unwrap().last().unwrap(), "-L$PulsarSeat.KioskAccount.03");
    }

    #[test]
    fn kiosk_accounts_are_interactive_only_and_pruned() {
        let m = Mock { lookup: Mutex::new(vec![Lookup::Missing, Lookup::User(SID.into())]), groups: vec![USERS_SID.into()], ..Default::default() };
        let a = ensure_kiosk(&m, s(4)).unwrap();
        assert_eq!(a.name, "PulsarKiosk04");
        let calls = m.calls();
        assert!(calls.contains(&"rights:SeInteractiveLogonRight".to_string()), "{calls:?}");
        assert!(calls.iter().any(|c| c.starts_with("rights:SeDenyRemoteInteractiveLogonRight") && c.contains("SeDenyBatchLogonRight")), "{calls:?}");
        assert!(!calls.contains(&"add_to_group".to_string()), "never in the host group");
        assert!(calls.contains(&"hide".to_string()));
        assert_eq!(*m.secrets.lock().unwrap(), ["+L$PulsarSeat.KioskAccount.04"]);
        // An administrator or a foreign user of that name is refused untouched.
        let m = Mock { lookup: Mutex::new(vec![Lookup::User(SID.into())]), groups: vec![ADMINISTRATORS_SID.into()], ..Default::default() };
        assert!(ensure_kiosk(&m, s(4)).is_err());
        assert!(!m.calls().contains(&"set_password".to_string()));
        let m = Mock { lookup: Mutex::new(vec![Lookup::Foreign]), ..Default::default() };
        assert!(ensure_kiosk(&m, s(4)).is_err());
        // Prune: only unkept seats, only ours.
        let m = Mock { lookup: Mutex::new(vec![Lookup::Missing]), ..Default::default() };
        m.named.lock().unwrap().insert("PulsarKiosk02".into(), Lookup::User(SID.into()));
        m.named.lock().unwrap().insert("PulsarKiosk06".into(), Lookup::User("S-1-5-21-1-2-3-1066".into()));
        m.named.lock().unwrap().insert("PulsarKiosk07".into(), Lookup::Foreign);
        assert!(prune_kiosk(&m, &[s(2)], true).is_empty());
        assert_eq!(m.calls().iter().filter(|c| *c == "delete").count(), 1);
        assert_eq!(*m.secrets.lock().unwrap(), ["-L$PulsarSeat.KioskAccount.06"]);
    }

    /// Service start never deletes a kiosk user or its profile: the unused account is only
    /// disabled and its password forgotten (parity review 2026-11-17).
    #[test]
    fn service_start_disables_unused_kiosk_users_and_keeps_their_profiles() {
        let m = Mock { lookup: Mutex::new(vec![Lookup::Missing]), ..Default::default() };
        m.named.lock().unwrap().insert("PulsarKiosk02".into(), Lookup::User(SID.into()));
        m.named.lock().unwrap().insert("PulsarKiosk06".into(), Lookup::User("S-1-5-21-1-2-3-1066".into()));
        m.named.lock().unwrap().insert("PulsarKiosk07".into(), Lookup::Foreign);
        assert!(disable_unused_kiosk(&m, &[s(2)]).is_empty());
        let calls = m.calls();
        assert_eq!(calls.iter().filter(|c| *c == "disable").count(), 1, "{calls:?}");
        assert!(!calls.iter().any(|c| c == "delete" || c == "delete_profile"), "{calls:?}");
        assert_eq!(*m.secrets.lock().unwrap(), ["-L$PulsarSeat.KioskAccount.06"]);
        // Explicit removal: refused while in use, else account + profile + secret.
        let m = Mock { lookup: Mutex::new(vec![Lookup::User(SID.into())]), ..Default::default() };
        assert!(remove_kiosk_user(&m, s(3), true).is_err());
        assert!(m.calls().is_empty());
        assert!(remove_kiosk_user(&m, s(3), false).unwrap().is_empty());
        let calls = m.calls();
        assert!(calls.contains(&"delete_profile".to_string()) && calls.contains(&"delete".to_string()), "{calls:?}");
        // Uninstall keeping user data: the account goes, the profile stays.
        let m = Mock { lookup: Mutex::new(vec![Lookup::Missing]), group: Mutex::new(vec![GroupLookup::Missing]), ..Default::default() };
        m.named.lock().unwrap().insert("PulsarKiosk03".into(), Lookup::User(SID.into()));
        assert!(remove_all(&m, false).is_empty());
        let calls = m.calls();
        assert!(calls.contains(&"delete".to_string()) && !calls.contains(&"delete_profile".to_string()), "{calls:?}");
    }

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
        assert_eq!(errs[0], copy::FOREIGN.fmt(&[&ACCOUNT_NAME]));
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
        assert!(errs[0].contains("could not be deleted") || errs[0].contains("silinemedi"), "{}", errs[0]);
        let m = Mock { lookup: Mutex::new(vec![Lookup::Missing]), ..Default::default() };
        assert!(remove(&m).is_empty());
        assert_eq!(m.calls(), ["lookup", "unhide", "unstore"]);
    }
}
