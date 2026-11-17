// SPDX-License-Identifier: GPL-3.0-only
//! Per-seat environment variables (spec/remote 50 §9.2; compat-tweaks 2026-11-17).
//!
//! The service writes the seat's variables into the seat user's own environment key
//! (`HKEY_USERS\<sid>\Environment`, the documented per-user environment that Explorer and
//! every program started from it read), journaled first ([`Change::UserEnvironment`]) with
//! the previous data, and undone through `HKEY_USERS\<sid>` (the profile's `NTUSER.DAT`
//! is loaded when the user is not logged on) on disable, reset and uninstall. Values that
//! contain `%` are written as `REG_EXPAND_SZ` so `%USERPROFILE%`-style references expand.
//!
//! The agent then broadcasts `WM_SETTINGCHANGE("Environment")` in the seat session so a
//! running shell reloads it; programs the service starts in the seat get the variables
//! directly. HYPOTHESIS (nothing ran on hardware): Explorer re-reads the key on that
//! broadcast, as it does for changes made in System Properties.

use crate::journal::{Change, Journal, JournalError, RevertOutcome};
use crate::ops::{Hive, OpsError, RegData, SystemOps};
use seat_core::config::EnvVar;

/// Key under the user's hive root.
pub const USER_ENVIRONMENT: &str = "Environment";
/// Journal owner prefix (one owner per user SID).
pub const OWNER_PREFIX: &str = "environment:";

pub fn owner(sid: &str) -> String {
    format!("{OWNER_PREFIX}{}", sid.to_ascii_uppercase())
}

/// The registry data for a value.
pub fn data_for(value: &str) -> RegData {
    if value.contains('%') { RegData::ExpandSz(value.to_string()) } else { RegData::Sz(value.to_string()) }
}

/// What [`sync_user`] did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SyncReport {
    pub written: usize,
    pub unchanged: usize,
    pub reverted: usize,
    pub failed: Vec<(String, String)>,
    pub no_profile: bool,
}

/// Make user `sid`'s values match `vars`: our values of names no longer listed are
/// reverted, the listed ones written (journal first). Invalid names are skipped (the
/// config validation refuses them already).
pub fn sync_user(j: &Journal, ops: &dyn SystemOps, sid: &str, vars: &[EnvVar]) -> Result<SyncReport, JournalError> {
    let mut rep = SyncReport::default();
    if !crate::ops::is_sid_string(sid) {
        return Err(JournalError::Ops(OpsError::new("environment", format!("not a SID: {sid}"))));
    }
    let own = owner(sid);
    let vars: Vec<&EnvVar> =
        vars.iter().filter(|v| seat_core::config::env_name_ok(&v.name) && seat_core::config::env_value_ok(&v.value)).collect();
    let keep = |name: &str| vars.iter().any(|v| v.name.eq_ignore_ascii_case(name));
    let mut stale: Vec<_> = j
        .active_for(&own)?
        .into_iter()
        .filter(|e| matches!(&e.change, Change::UserEnvironment { name, .. } if !keep(name)))
        .collect();
    stale.reverse();
    for e in stale {
        if let Change::UserEnvironment { sid, name, previous, written } = &e.change {
            match revert_value(ops, sid, name, previous.as_ref(), written, false) {
                Ok(o) => {
                    j.close(e.seq, o)?;
                    rep.reverted += 1;
                }
                Err(err) => rep.failed.push((name.clone(), err.to_string())),
            }
        }
    }
    if vars.is_empty() {
        return Ok(rep);
    }
    let mut todo: Vec<(String, Option<RegData>, RegData)> = Vec::new();
    let mut failed = Vec::new();
    let found = ops.with_user_hive(sid, &mut |root| {
        let key = format!("{root}\\{USER_ENVIRONMENT}");
        for v in &vars {
            let want = data_for(&v.value);
            match ops.reg_read(Hive::Users, &key, &v.name) {
                Ok(cur) if cur.as_ref() == Some(&want) => rep.unchanged += 1,
                Ok(cur) => todo.push((v.name.clone(), cur, want)),
                Err(e) => failed.push((v.name.clone(), e.to_string())),
            }
        }
        Ok(())
    })?;
    if !found {
        rep.no_profile = true;
        return Ok(rep);
    }
    for (name, previous, written) in todo {
        let e = j.record(&own, Change::UserEnvironment { sid: sid.to_string(), name: name.clone(), previous, written: written.clone() })?;
        let mut res = Ok(());
        ops.with_user_hive(sid, &mut |root| {
            res = ops.reg_write(Hive::Users, &format!("{root}\\{USER_ENVIRONMENT}"), &name, &written);
            Ok(())
        })?;
        match res {
            Ok(()) => rep.written += 1,
            Err(err) => {
                j.close(e.seq, RevertOutcome::NotApplied)?;
                failed.push((name, err.to_string()));
            }
        }
    }
    rep.failed.extend(failed);
    Ok(rep)
}

/// Undo one value (journal revert, [`sync_user`]).
pub fn revert_value(
    ops: &dyn SystemOps,
    sid: &str,
    name: &str,
    previous: Option<&RegData>,
    written: &RegData,
    force: bool,
) -> Result<RevertOutcome, OpsError> {
    if !seat_core::config::env_name_ok(name) {
        return Ok(RevertOutcome::Refused);
    }
    let mut outcome = RevertOutcome::Closed;
    let found = ops.with_user_hive(sid, &mut |root| {
        let key = format!("{root}\\{USER_ENVIRONMENT}");
        let cur = ops.reg_read(Hive::Users, &key, name)?;
        outcome = if cur.as_ref() == previous {
            RevertOutcome::AlreadyRestored
        } else if cur.as_ref() != Some(written) && !force {
            RevertOutcome::ChangedSince
        } else {
            match previous {
                Some(p) => ops.reg_write(Hive::Users, &key, name, p)?,
                None => ops.reg_delete(Hive::Users, &key, name)?,
            }
            RevertOutcome::Restored
        };
        Ok(())
    })?;
    Ok(if found { outcome } else { RevertOutcome::Closed })
}

/// Revert every user's values (disable, reset). Returns (reverted, failures).
pub fn revert_all(j: &Journal, ops: &dyn SystemOps) -> Result<(usize, Vec<String>), JournalError> {
    let mut owners: Vec<String> = j.active()?.into_iter().map(|e| e.owner).filter(|o| o.starts_with(OWNER_PREFIX)).collect();
    owners.sort();
    owners.dedup();
    let (mut n, mut failed) = (0, Vec::new());
    for o in owners {
        let rep = j.revert_owner(ops, &o, false)?;
        n += rep.done.len();
        failed.extend(rep.failed.into_iter().map(|(_, e)| e));
    }
    Ok((n, failed))
}

/// The environment of a process the service starts in the seat (valid entries only).
pub fn launch_env(vars: &[EnvVar]) -> Vec<(String, String)> {
    vars.iter()
        .filter(|v| seat_core::config::env_name_ok(&v.name) && seat_core::config::env_value_ok(&v.value))
        .map(|v| (v.name.clone(), v.value.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::MemoryOps;

    const SID: &str = "S-1-5-21-1-2-3-1001";

    fn var(n: &str, v: &str) -> EnvVar {
        EnvVar { name: n.into(), value: v.into() }
    }

    #[test]
    fn sync_writes_reverts_and_uninstall_restores() {
        let ops = MemoryOps::new();
        let j = Journal::memory();
        let key = format!("{SID}\\{USER_ENVIRONMENT}");
        ops.set(Hive::Users, &key, "GAME_PROFILE", RegData::Sz("old".into()));
        let r = sync_user(&j, &ops, SID, &[var("GAME_PROFILE", "seat2"), var("CACHE", "%USERPROFILE%\\c"), var("Path", "x")]).unwrap();
        assert_eq!((r.written, r.unchanged), (2, 0), "{r:?}");
        assert_eq!(ops.get(Hive::Users, &key, "CACHE"), Some(RegData::ExpandSz("%USERPROFILE%\\c".into())));
        assert_eq!(ops.get(Hive::Users, &key, "Path"), None);
        let again = sync_user(&j, &ops, SID, &[var("GAME_PROFILE", "seat2"), var("CACHE", "%USERPROFILE%\\c")]).unwrap();
        assert_eq!((again.written, again.unchanged), (0, 2));
        // CACHE dropped from the config: reverted to absent.
        let r = sync_user(&j, &ops, SID, &[var("GAME_PROFILE", "seat2")]).unwrap();
        assert_eq!(r.reverted, 1);
        assert_eq!(ops.get(Hive::Users, &key, "CACHE"), None);
        // Uninstall restores the original value.
        let rep = j.revert_all(&ops, false).unwrap();
        assert!(rep.is_clean(), "{rep:?}");
        assert_eq!(ops.get(Hive::Users, &key, "GAME_PROFILE"), Some(RegData::Sz("old".into())));
        assert!(j.active().unwrap().is_empty());
    }

    #[test]
    fn missing_profile_and_bad_sid() {
        let ops = MemoryOps::new();
        let j = Journal::memory();
        assert!(sync_user(&j, &ops, r"..\x", &[var("A", "1")]).is_err());
        let r = sync_user(&j, &ops, "S-1-5-21-9-9-9-1002", &[var("A", "1")]).unwrap();
        assert!(r.no_profile);
        assert_eq!(launch_env(&[var("A", "1"), var("TEMP", "x")]), vec![("A".to_string(), "1".to_string())]);
    }

    #[test]
    fn revert_all_reverts_only_environment_owners() {
        let ops = MemoryOps::new();
        let j = Journal::memory();
        ops.add_key(Hive::Users, SID);
        sync_user(&j, &ops, SID, &[var("A", "1")]).unwrap();
        let (n, failed) = revert_all(&j, &ops).unwrap();
        assert_eq!((n, failed.len()), (1, 0));
        assert_eq!(ops.get(Hive::Users, &format!("{SID}\\Environment"), "A"), None);
    }
}
