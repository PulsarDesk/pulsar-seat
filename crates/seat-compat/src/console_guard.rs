// SPDX-License-Identifier: GPL-3.0-only
//! Console lock guard and seat power policies (OSMS-style local seat parity, gap G4,
//! 2026-11-18).
//!
//! Local seats are shown by presenter windows on the console session's desktop. When that
//! console locks (Win+L, Ctrl+Alt+Del > Lock, the screen saver's lock, the machine
//! inactivity limit) Windows switches to the secure desktop and every seat's monitor shows
//! the lock screen. The seat sessions keep running, but nobody can see them. This module
//! keeps the console from locking with documented **per-user** policy values, written in
//! the user's own hive (`HKEY_USERS\<sid>`), each journaled first
//! ([`Change::UserPolicy`]) with its previous data and undone through the journal:
//!
//! | Policy | Value |
//! | --- | --- |
//! | [`UserPolicy::DisableLockWorkstation`] | `Software\Microsoft\Windows\CurrentVersion\Policies\System` `DisableLockWorkstation` = 1 (DWORD) |
//! | [`UserPolicy::ScreenSaveActive`] | `Software\Policies\Microsoft\Windows\Control Panel\Desktop` `ScreenSaveActive` = "0" (REG_SZ) |
//! | [`UserPolicy::NoClose`] | `Software\Microsoft\Windows\CurrentVersion\Policies\Explorer` `NoClose` = 1 (DWORD) |
//!
//! `DisableLockWorkstation` is a user policy: the same value under HKLM is not honoured
//! (OpenStreamMultiSeat notes this too), so nothing is written to HKLM here. The machine
//! inactivity limit (`InactivityTimeoutSecs`) is the separate, optional machine tweak
//! `crate::tweaks::TweakId::InactivityLockOff`.
//!
//! Journal owners (each synced to a wanted list; what the list no longer names is undone):
//!
//! - [`OWNER_HOST`]: `dedicated_host` only. Lock and screen saver off for the account signed
//!   in at the console (the host account). Permanent while the topology and the option
//!   stay on; undone when either is turned off, on disable, reset and uninstall.
//! - [`OWNER_SEAT_ONE`]: `seat_one_console` only, opt-in. Lock off for seat 1's user while
//!   at least one other seat runs (engage on the first seat start, release on the last
//!   seat stop, on service stop, and at service start for an entry a crash left behind).
//! - [`OWNER_NO_CLOSE`]: per-seat opt-in: `NoClose` (no shut down / restart / sleep in
//!   Start, Ctrl+Alt+Del and Alt+F4) for the seat's account.
//!
//! "Shut down the system" (`SeShutdownPrivilege`) for `BUILTIN\Users` is the machine-wide
//! opt-in [`sync_users_shutdown`]: removed with `LsaRemoveAccountRights` only while
//! `BUILTIN\Administrators` still holds it, journaled ([`Change::AccountRightRemoved`]) and
//! given back on disable, reset and uninstall.
//!
//! A value the user (or their own policy) already had is never recorded and never undone.
//! An undo finds the value changed by someone else since: left alone (`changed_since`).
//!
//! HYPOTHESIS (nothing ran on hardware): `DisableLockWorkstation` also blocks Win+L for an
//! auto-logon account; the per-user `ScreenSaveActive` policy stops the screen saver for
//! the host account without a sign-out; `NoClose` is honoured by a running Explorer (else
//! at the next sign-in); a removed privilege applies from each user's next sign-in. The
//! UAC secure desktop still blanks the presenters (only `PromptOnSecureDesktop` = 0 avoids
//! that, a security trade-off this module never writes).

use crate::journal::{Change, Journal, JournalError, RevertOutcome, RevertReport};
use crate::ops::{Hive, OpsError, RegData, SystemOps};
use serde::{Deserialize, Serialize};

/// `HKCU\...\Policies\System` (relative to the user's hive root).
pub const USER_POLICIES_SYSTEM: &str = r"Software\Microsoft\Windows\CurrentVersion\Policies\System";
/// `HKCU\Software\Policies\...\Control Panel\Desktop` (the Group Policy screen-saver key).
pub const USER_DESKTOP_POLICY: &str = r"Software\Policies\Microsoft\Windows\Control Panel\Desktop";
/// `HKCU\...\Policies\Explorer`.
pub const USER_EXPLORER_POLICY: &str = r"Software\Microsoft\Windows\CurrentVersion\Policies\Explorer";

/// Journal owner of the host account's guard (`dedicated_host`).
pub const OWNER_HOST: &str = "console_guard:host";
/// Journal owner of seat 1's opt-in guard (`seat_one_console`).
pub const OWNER_SEAT_ONE: &str = "console_guard:seat_one";
/// Journal owner of the per-seat `NoClose` option.
pub const OWNER_NO_CLOSE: &str = "seat_policy:no_close";
/// Journal owner of the `BUILTIN\Users` shutdown-right removal.
pub const OWNER_USERS_SHUTDOWN: &str = "seat_policy:users_shutdown";

/// `BUILTIN\Users`.
pub const USERS_SID: &str = "S-1-5-32-545";
/// `BUILTIN\Administrators`.
pub const ADMINS_SID: &str = "S-1-5-32-544";
/// The documented privilege name (`SE_SHUTDOWN_NAME`).
pub const SHUTDOWN_RIGHT: &str = "SeShutdownPrivilege";

/// The fixed per-user policy values this module may write (the journal can name nothing
/// else, so a tampered entry cannot reach another value).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserPolicy {
    DisableLockWorkstation,
    ScreenSaveActive,
    NoClose,
}

/// What the host account gets.
pub const HOST_POLICIES: &[UserPolicy] = &[UserPolicy::DisableLockWorkstation, UserPolicy::ScreenSaveActive];
/// What seat 1's user gets while the opt-in guard is engaged.
pub const SEAT_ONE_POLICIES: &[UserPolicy] = &[UserPolicy::DisableLockWorkstation];
/// What a seat with `no_power_menu` gets.
pub const NO_CLOSE_POLICIES: &[UserPolicy] = &[UserPolicy::NoClose];

impl UserPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DisableLockWorkstation => "disable_lock_workstation",
            Self::ScreenSaveActive => "screen_save_active",
            Self::NoClose => "no_close",
        }
    }

    /// Key relative to the user's hive root.
    pub fn key(self) -> &'static str {
        match self {
            Self::DisableLockWorkstation => USER_POLICIES_SYSTEM,
            Self::ScreenSaveActive => USER_DESKTOP_POLICY,
            Self::NoClose => USER_EXPLORER_POLICY,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::DisableLockWorkstation => "DisableLockWorkstation",
            Self::ScreenSaveActive => "ScreenSaveActive",
            Self::NoClose => "NoClose",
        }
    }

    /// The data we write.
    pub fn data(self) -> RegData {
        match self {
            Self::DisableLockWorkstation | Self::NoClose => RegData::Dword(1),
            Self::ScreenSaveActive => RegData::Sz("0".into()),
        }
    }
}

fn under(root: &str, key: &str) -> String {
    format!("{root}\\{key}")
}

/// Keys of `rel` (relative to the user root) that do not exist yet, deepest first. Keys
/// with fewer than three parts (`Software\Policies`) are never listed.
fn missing_keys(ops: &dyn SystemOps, root: &str, rel: &str) -> Result<Vec<String>, OpsError> {
    let mut out = Vec::new();
    let mut k = rel.trim_matches('\\').to_string();
    while out.len() < 8 && k.split('\\').count() >= 3 && !ops.reg_key_exists(Hive::Users, &under(root, &k))? {
        out.push(k.clone());
        match k.rfind('\\') {
            Some(i) => k.truncate(i),
            None => break,
        }
    }
    Ok(out)
}

/// Remove the keys our write created (relative, deepest first) while they are empty. Only
/// the policy's own key and its ancestors are ever touched.
fn remove_created(ops: &dyn SystemOps, root: &str, p: UserPolicy, created: &[String]) {
    let own = p.key().to_ascii_lowercase();
    for c in created {
        let c = c.trim_matches('\\');
        let l = c.to_ascii_lowercase();
        if !(own == l || own.starts_with(&format!("{l}\\"))) || c.split('\\').count() < 3 {
            continue;
        }
        if matches!(ops.reg_key_exists(Hive::Users, &under(root, c)), Ok(false)) {
            continue;
        }
        if !matches!(ops.reg_delete_empty_key(Hive::Users, &under(root, c)), Ok(true)) {
            break;
        }
    }
}

/// What a sync did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SyncReport {
    pub written: usize,
    pub unchanged: usize,
    pub reverted: usize,
    /// (who, error).
    pub failed: Vec<(String, String)>,
    /// SIDs without a profile (nothing written; retried at the next sync).
    pub no_profile: Vec<String>,
}

impl SyncReport {
    /// One log line; empty when nothing happened.
    pub fn summary(&self) -> String {
        if self.written + self.reverted == 0 && self.failed.is_empty() && self.no_profile.is_empty() {
            return String::new();
        }
        let mut s = format!("{} written, {} undone", self.written, self.reverted);
        if !self.no_profile.is_empty() {
            s.push_str(&format!(", no profile: {}", self.no_profile.join(" ")));
        }
        s
    }
}

/// Make `owner`'s per-user policy values match `wanted` ((SID, policies)): entries of
/// `owner` no longer wanted are undone, wanted values written (journal first). A value
/// already holding the wanted data is left unrecorded (it is the user's own).
pub fn sync_owner(j: &Journal, ops: &dyn SystemOps, owner: &str, wanted: &[(String, Vec<UserPolicy>)]) -> Result<SyncReport, JournalError> {
    let mut rep = SyncReport::default();
    let mut want: Vec<(String, UserPolicy)> = Vec::new();
    for (sid, ps) in wanted {
        if !crate::ops::is_sid_string(sid) {
            rep.failed.push((sid.clone(), "not a SID".into()));
            continue;
        }
        for p in ps {
            let k = (sid.to_ascii_uppercase(), *p);
            if !want.contains(&k) {
                want.push(k);
            }
        }
    }
    let target = |sid: &str, p: UserPolicy| {
        Change::UserPolicy { sid: sid.to_string(), policy: p, previous: None, written: p.data(), created_keys: Vec::new() }.target()
    };
    let keep: Vec<String> = want.iter().map(|(s, p)| target(s, *p)).collect();
    let r = j.revert_owner_except(ops, owner, &keep)?;
    rep.reverted = r.done.iter().filter(|(_, o)| *o == RevertOutcome::Restored).count();
    rep.failed.extend(r.failed.into_iter().map(|(seq, e)| (format!("entry {seq}"), e)));
    let mut sids: Vec<String> = want.iter().map(|(s, _)| s.clone()).collect();
    sids.sort();
    sids.dedup();
    for sid in sids {
        let ps: Vec<UserPolicy> = want.iter().filter(|(s, _)| *s == sid).map(|(_, p)| *p).collect();
        let mut todo: Vec<(UserPolicy, Option<RegData>, Vec<String>)> = Vec::new();
        let mut failed = Vec::new();
        let found = ops.with_user_hive(&sid, &mut |root| {
            for p in &ps {
                let cur = match ops.reg_read(Hive::Users, &under(root, p.key()), p.name()) {
                    Ok(c) => c,
                    Err(e) => {
                        failed.push((format!("{sid} {}", p.as_str()), e.to_string()));
                        continue;
                    }
                };
                if cur.as_ref() == Some(&p.data()) {
                    // Ours (journaled earlier) or the user's own value: nothing to write.
                    rep.unchanged += 1;
                    continue;
                }
                let created = missing_keys(ops, root, p.key())?;
                todo.push((*p, cur, created));
            }
            Ok(())
        });
        let found = match found {
            Ok(f) => f,
            Err(e) => {
                rep.failed.push((sid.clone(), e.to_string()));
                continue;
            }
        };
        rep.failed.extend(failed);
        if !found {
            rep.no_profile.push(sid.clone());
            continue;
        }
        for (p, previous, created_keys) in todo {
            let e = j.record(owner, Change::UserPolicy { sid: sid.clone(), policy: p, previous, written: p.data(), created_keys })?;
            let mut res = Ok(());
            let wrote = ops.with_user_hive(&sid, &mut |root| {
                res = ops.reg_write(Hive::Users, &under(root, p.key()), p.name(), &p.data());
                Ok(())
            });
            match (wrote, res) {
                (Ok(true), Ok(())) => rep.written += 1,
                (Ok(_), Err(err)) | (Err(err), _) => {
                    j.close(e.seq, RevertOutcome::NotApplied)?;
                    rep.failed.push((format!("{sid} {}", p.as_str()), err.to_string()));
                }
                (Ok(false), Ok(())) => {
                    j.close(e.seq, RevertOutcome::NotApplied)?;
                    rep.no_profile.push(sid.clone());
                }
            }
        }
    }
    Ok(rep)
}

/// Undo one value (journal revert). Called by `journal::revert_one`.
pub fn revert_value(
    ops: &dyn SystemOps,
    sid: &str,
    p: UserPolicy,
    previous: Option<&RegData>,
    written: &RegData,
    created_keys: &[String],
    force: bool,
) -> Result<RevertOutcome, OpsError> {
    if !crate::ops::is_sid_string(sid) || *written != p.data() {
        return Ok(RevertOutcome::Refused);
    }
    let mut outcome = RevertOutcome::Closed;
    let found = ops.with_user_hive(sid, &mut |root| {
        let key = under(root, p.key());
        let cur = ops.reg_read(Hive::Users, &key, p.name())?;
        outcome = if cur.as_ref() == previous {
            if previous.is_none() {
                remove_created(ops, root, p, created_keys);
            }
            RevertOutcome::AlreadyRestored
        } else if cur.as_ref() != Some(written) && !force {
            RevertOutcome::ChangedSince
        } else {
            match previous {
                Some(v) => ops.reg_write(Hive::Users, &key, p.name(), v)?,
                None => {
                    ops.reg_delete(Hive::Users, &key, p.name())?;
                    remove_created(ops, root, p, created_keys);
                }
            }
            RevertOutcome::Restored
        };
        Ok(())
    })?;
    // Profile deleted: nothing left to undo.
    Ok(if found { outcome } else { RevertOutcome::Closed })
}

/// Host account guard (`dedicated_host`): `Some(sid)` = the account signed in at the
/// console; `None` = turn it off (topology / option off, product disabled).
pub fn sync_host(j: &Journal, ops: &dyn SystemOps, sid: Option<&str>) -> Result<SyncReport, JournalError> {
    let wanted: Vec<(String, Vec<UserPolicy>)> = sid.map(|s| vec![(s.to_string(), HOST_POLICIES.to_vec())]).unwrap_or_default();
    sync_owner(j, ops, OWNER_HOST, &wanted)
}

/// Seat 1's opt-in guard: `Some(sid)` engages it for seat 1's user, `None` releases it.
/// Idempotent both ways.
pub fn sync_seat_one(j: &Journal, ops: &dyn SystemOps, sid: Option<&str>) -> Result<SyncReport, JournalError> {
    let wanted: Vec<(String, Vec<UserPolicy>)> = sid.map(|s| vec![(s.to_string(), SEAT_ONE_POLICIES.to_vec())]).unwrap_or_default();
    sync_owner(j, ops, OWNER_SEAT_ONE, &wanted)
}

/// `NoClose` for exactly these accounts.
pub fn sync_no_close(j: &Journal, ops: &dyn SystemOps, sids: &[String]) -> Result<SyncReport, JournalError> {
    let wanted: Vec<(String, Vec<UserPolicy>)> = sids.iter().map(|s| (s.clone(), NO_CLOSE_POLICIES.to_vec())).collect();
    sync_owner(j, ops, OWNER_NO_CLOSE, &wanted)
}

/// Undo every per-user value of this module (product disabled).
pub fn revert_all(j: &Journal, ops: &dyn SystemOps) -> Result<RevertReport, JournalError> {
    let mut rep = RevertReport::default();
    for o in [OWNER_SEAT_ONE, OWNER_HOST, OWNER_NO_CLOSE, OWNER_USERS_SHUTDOWN] {
        let r = j.revert_owner(ops, o, false)?;
        rep.done.extend(r.done);
        rep.failed.extend(r.failed);
    }
    Ok(rep)
}

/// SIDs that hold a seat-one guard entry right now (the service's crash check).
pub fn seat_one_engaged(j: &Journal) -> Result<bool, JournalError> {
    Ok(!j.active_for(OWNER_SEAT_ONE)?.is_empty())
}

// ---- BUILTIN\Users shutdown right -------------------------------------------------------

/// What [`sync_users_shutdown`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownOutcome {
    /// Nothing to do (already in the wanted state, or the right was never ours to touch).
    Unchanged,
    /// Removed from `BUILTIN\Users` (journaled).
    Removed,
    /// Given back to `BUILTIN\Users`.
    Restored,
}

/// `off = true`: remove `SeShutdownPrivilege` from `BUILTIN\Users`, journaled, only while
/// `BUILTIN\Administrators` still holds it (the console administrator must always be able
/// to shut down). `off = false`: give back what we removed.
pub fn sync_users_shutdown(j: &Journal, ops: &dyn SystemOps, off: bool) -> Result<ShutdownOutcome, JournalError> {
    if !off {
        let r = j.revert_owner(ops, OWNER_USERS_SHUTDOWN, false)?;
        if let Some((_, e)) = r.failed.first() {
            return Err(JournalError::Ops(OpsError::new("users_shutdown", e.clone())));
        }
        return Ok(if r.done.iter().any(|(_, o)| *o == RevertOutcome::Restored) {
            ShutdownOutcome::Restored
        } else {
            ShutdownOutcome::Unchanged
        });
    }
    let change = Change::AccountRightRemoved { sid: USERS_SID.into(), right: SHUTDOWN_RIGHT.into() };
    if j.active_for(OWNER_USERS_SHUTDOWN)?.iter().any(|e| e.change.target() == change.target()) {
        // Ours already; remove again if someone gave it back (the entry still restores it).
        if ops.account_rights(USERS_SID)?.iter().any(|r| r == SHUTDOWN_RIGHT) {
            ops.account_right(USERS_SID, SHUTDOWN_RIGHT, false)?;
            return Ok(ShutdownOutcome::Removed);
        }
        return Ok(ShutdownOutcome::Unchanged);
    }
    if !ops.account_rights(USERS_SID)?.iter().any(|r| r == SHUTDOWN_RIGHT) {
        return Ok(ShutdownOutcome::Unchanged);
    }
    if !ops.account_rights(ADMINS_SID)?.iter().any(|r| r == SHUTDOWN_RIGHT) {
        return Err(JournalError::Ops(OpsError::new(
            "users_shutdown",
            "BUILTIN\\Administrators does not hold SeShutdownPrivilege; refusing to remove it from BUILTIN\\Users",
        )));
    }
    let e = j.record(OWNER_USERS_SHUTDOWN, change)?;
    if let Err(err) = ops.account_right(USERS_SID, SHUTDOWN_RIGHT, false) {
        j.close(e.seq, RevertOutcome::NotApplied)?;
        return Err(err.into());
    }
    Ok(ShutdownOutcome::Removed)
}

/// Undo of [`Change::AccountRightRemoved`]: only `SeShutdownPrivilege` of `BUILTIN\Users`.
pub fn revert_removed_right(ops: &dyn SystemOps, sid: &str, right: &str) -> Result<RevertOutcome, OpsError> {
    if sid != USERS_SID || right != SHUTDOWN_RIGHT {
        return Ok(RevertOutcome::Refused);
    }
    if ops.account_rights(sid)?.iter().any(|r| r == right) {
        return Ok(RevertOutcome::AlreadyRestored);
    }
    ops.account_right(sid, right, true)?;
    Ok(RevertOutcome::Restored)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::MemoryOps;

    const HOST: &str = "S-1-5-21-1-2-3-1001";
    const SEAT1: &str = "S-1-5-21-1-2-3-1002";

    fn get(ops: &MemoryOps, sid: &str, p: UserPolicy) -> Option<RegData> {
        ops.get(Hive::Users, &under(sid, p.key()), p.name())
    }

    #[test]
    fn host_guard_writes_both_values_and_uninstall_restores() {
        let ops = MemoryOps::new();
        let j = Journal::memory();
        ops.add_key(Hive::Users, HOST);
        // The user had their own screen-saver policy "1".
        ops.set(Hive::Users, &under(HOST, USER_DESKTOP_POLICY), "ScreenSaveActive", RegData::Sz("1".into()));
        let r = sync_host(&j, &ops, Some(HOST)).unwrap();
        assert_eq!((r.written, r.unchanged), (2, 0), "{r:?}");
        assert_eq!(get(&ops, HOST, UserPolicy::DisableLockWorkstation), Some(RegData::Dword(1)));
        assert_eq!(get(&ops, HOST, UserPolicy::ScreenSaveActive), Some(RegData::Sz("0".into())));
        // Idempotent.
        let again = sync_host(&j, &ops, Some(HOST)).unwrap();
        assert_eq!((again.written, again.unchanged), (0, 2));
        assert_eq!(j.active_for(OWNER_HOST).unwrap().len(), 2);
        // Uninstall: previous values back, created keys gone.
        let rep = j.revert_all(&ops, false).unwrap();
        assert!(rep.is_clean(), "{rep:?}");
        assert_eq!(get(&ops, HOST, UserPolicy::DisableLockWorkstation), None);
        assert_eq!(get(&ops, HOST, UserPolicy::ScreenSaveActive), Some(RegData::Sz("1".into())));
        assert!(!ops.reg_key_exists(Hive::Users, &under(HOST, USER_POLICIES_SYSTEM)).unwrap());
        assert!(j.active().unwrap().is_empty());
    }

    #[test]
    fn host_guard_off_and_account_change_undo_the_old_account() {
        let ops = MemoryOps::new();
        let j = Journal::memory();
        ops.add_key(Hive::Users, HOST);
        ops.add_key(Hive::Users, SEAT1);
        sync_host(&j, &ops, Some(HOST)).unwrap();
        // Another account now owns the console: the first one is undone.
        let r = sync_host(&j, &ops, Some(SEAT1)).unwrap();
        assert_eq!((r.written, r.reverted), (2, 2), "{r:?}");
        assert_eq!(get(&ops, HOST, UserPolicy::DisableLockWorkstation), None);
        assert_eq!(get(&ops, SEAT1, UserPolicy::DisableLockWorkstation), Some(RegData::Dword(1)));
        let off = sync_host(&j, &ops, None).unwrap();
        assert_eq!(off.reverted, 2);
        assert!(j.active().unwrap().is_empty());
    }

    #[test]
    fn users_own_value_is_never_recorded_or_undone() {
        let ops = MemoryOps::new();
        let j = Journal::memory();
        ops.set(Hive::Users, &under(SEAT1, USER_POLICIES_SYSTEM), "DisableLockWorkstation", RegData::Dword(1));
        let r = sync_seat_one(&j, &ops, Some(SEAT1)).unwrap();
        assert_eq!((r.written, r.unchanged), (0, 1));
        assert!(j.active().unwrap().is_empty());
        sync_seat_one(&j, &ops, None).unwrap();
        assert_eq!(get(&ops, SEAT1, UserPolicy::DisableLockWorkstation), Some(RegData::Dword(1)));
    }

    #[test]
    fn seat_one_engage_release_and_crash_recovery() {
        let ops = MemoryOps::new();
        ops.add_key(Hive::Users, SEAT1);
        let j = Journal::memory();
        ops.set(Hive::Users, &under(SEAT1, USER_POLICIES_SYSTEM), "DisableLockWorkstation", RegData::Dword(0));
        sync_seat_one(&j, &ops, Some(SEAT1)).unwrap();
        assert!(seat_one_engaged(&j).unwrap());
        assert_eq!(get(&ops, SEAT1, UserPolicy::DisableLockWorkstation), Some(RegData::Dword(1)));
        // "Crash": a new service run finds the entry and releases it.
        let r = sync_seat_one(&j, &ops, None).unwrap();
        assert_eq!(r.reverted, 1);
        assert!(!seat_one_engaged(&j).unwrap());
        assert_eq!(get(&ops, SEAT1, UserPolicy::DisableLockWorkstation), Some(RegData::Dword(0)));
    }

    #[test]
    fn changed_since_is_left_alone() {
        let ops = MemoryOps::new();
        ops.add_key(Hive::Users, SEAT1);
        let j = Journal::memory();
        sync_seat_one(&j, &ops, Some(SEAT1)).unwrap();
        ops.set(Hive::Users, &under(SEAT1, USER_POLICIES_SYSTEM), "DisableLockWorkstation", RegData::Dword(5));
        let rep = j.revert_owner(&ops, OWNER_SEAT_ONE, false).unwrap();
        assert_eq!(rep.done[0].1, RevertOutcome::ChangedSince);
        assert_eq!(get(&ops, SEAT1, UserPolicy::DisableLockWorkstation), Some(RegData::Dword(5)));
    }

    #[test]
    fn missing_profile_and_bad_sid() {
        let ops = MemoryOps::new();
        let j = Journal::memory();
        let r = sync_no_close(&j, &ops, &["S-1-5-21-9-9-9-1003".into(), r"..\x".into()]).unwrap();
        assert_eq!(r.no_profile, vec!["S-1-5-21-9-9-9-1003".to_string()]);
        assert_eq!(r.failed.len(), 1);
        assert!(j.active().unwrap().is_empty());
        assert!(r.summary().contains("no profile"));
    }

    #[test]
    fn no_close_follows_the_list() {
        let ops = MemoryOps::new();
        ops.add_key(Hive::Users, HOST);
        ops.add_key(Hive::Users, SEAT1);
        let j = Journal::memory();
        sync_no_close(&j, &ops, &[HOST.into(), SEAT1.into()]).unwrap();
        assert_eq!(get(&ops, SEAT1, UserPolicy::NoClose), Some(RegData::Dword(1)));
        let r = sync_no_close(&j, &ops, &[HOST.into()]).unwrap();
        assert_eq!(r.reverted, 1);
        assert_eq!(get(&ops, SEAT1, UserPolicy::NoClose), None);
        assert_eq!(get(&ops, HOST, UserPolicy::NoClose), Some(RegData::Dword(1)));
        let rep = revert_all(&j, &ops).unwrap();
        assert!(rep.is_clean());
        assert_eq!(get(&ops, HOST, UserPolicy::NoClose), None);
    }

    #[test]
    fn failed_write_is_not_left_active() {
        let ops = MemoryOps::new();
        ops.add_key(Hive::Users, HOST);
        let j = Journal::memory();
        *ops.fail_next_write.lock().unwrap() = Some("denied".into());
        let r = sync_seat_one(&j, &ops, Some(HOST)).unwrap();
        assert_eq!(r.failed.len(), 1, "{r:?}");
        assert!(j.active().unwrap().is_empty());
    }

    #[test]
    fn tampered_entry_is_refused() {
        let ops = MemoryOps::new();
        ops.add_key(Hive::Users, HOST);
        let j = Journal::memory();
        j.record(
            OWNER_HOST,
            Change::UserPolicy {
                sid: HOST.into(),
                policy: UserPolicy::NoClose,
                previous: None,
                written: RegData::Sz("x".into()),
                created_keys: Vec::new(),
            },
        )
        .unwrap();
        let rep = j.revert_all(&ops, false).unwrap();
        assert_eq!(rep.done[0].1, RevertOutcome::Refused);
    }

    #[test]
    fn users_shutdown_right_removed_only_with_admins_and_given_back() {
        let ops = MemoryOps::new();
        let j = Journal::memory();
        ops.account_right(USERS_SID, SHUTDOWN_RIGHT, true).unwrap();
        // Administrators lack it: refused, nothing changed.
        assert!(sync_users_shutdown(&j, &ops, true).is_err());
        assert!(ops.account_rights(USERS_SID).unwrap().contains(&SHUTDOWN_RIGHT.to_string()));
        assert!(j.active().unwrap().is_empty());
        ops.account_right(ADMINS_SID, SHUTDOWN_RIGHT, true).unwrap();
        assert_eq!(sync_users_shutdown(&j, &ops, true).unwrap(), ShutdownOutcome::Removed);
        assert!(ops.account_rights(USERS_SID).unwrap().is_empty());
        assert_eq!(sync_users_shutdown(&j, &ops, true).unwrap(), ShutdownOutcome::Unchanged);
        // Uninstall gives it back; Administrators keep theirs.
        let rep = j.revert_all(&ops, false).unwrap();
        assert!(rep.is_clean(), "{rep:?}");
        assert!(ops.account_rights(USERS_SID).unwrap().contains(&SHUTDOWN_RIGHT.to_string()));
        assert!(ops.account_rights(ADMINS_SID).unwrap().contains(&SHUTDOWN_RIGHT.to_string()));
    }

    #[test]
    fn users_shutdown_off_restores_and_foreign_entries_are_refused() {
        let ops = MemoryOps::new();
        let j = Journal::memory();
        ops.account_right(USERS_SID, SHUTDOWN_RIGHT, true).unwrap();
        ops.account_right(ADMINS_SID, SHUTDOWN_RIGHT, true).unwrap();
        sync_users_shutdown(&j, &ops, true).unwrap();
        assert_eq!(sync_users_shutdown(&j, &ops, false).unwrap(), ShutdownOutcome::Restored);
        assert_eq!(sync_users_shutdown(&j, &ops, false).unwrap(), ShutdownOutcome::Unchanged);
        // Users never had it (Server default): nothing recorded.
        let ops2 = MemoryOps::new();
        assert_eq!(sync_users_shutdown(&j, &ops2, true).unwrap(), ShutdownOutcome::Unchanged);
        // A tampered entry naming another right is never re-granted.
        j.record(OWNER_USERS_SHUTDOWN, Change::AccountRightRemoved { sid: HOST.into(), right: "SeDebugPrivilege".into() }).unwrap();
        let rep = j.revert_all(&ops2, false).unwrap();
        assert!(rep.done.iter().any(|(_, o)| *o == RevertOutcome::Refused));
        assert!(ops2.account_rights(HOST).unwrap().is_empty());
    }
}
