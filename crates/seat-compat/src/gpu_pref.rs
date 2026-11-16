// SPDX-License-Identifier: GPL-3.0-only
//! Per-seat GPU preference (spec 40 §6.4; added 2026-11-17): the per-app graphics
//! preference that Windows Settings > System > Display > Graphics writes, applied by the
//! service in the **seat user's** hive.
//!
//! Documented storage (Microsoft Learn, "Graphics performance preference" / DirectX
//! `UserGpuPreferences`): key `HKCU\Software\Microsoft\DirectX\UserGpuPreferences`, one
//! `REG_SZ` per program, value name = the program's full path, data = `key=value;` pairs
//! of which `GpuPreference=<n>;` picks the GPU (0 = let Windows decide, 1 = power saving,
//! 2 = high performance). Other pairs Windows keeps there (for example
//! `SwapEffectUpgradeEnable=1;`) are preserved.
//!
//! This is the GPU enumeration control Pulsar Seat offers: a preference the DXGI runtime
//! honours when an app picks its adapter, not isolation (every seat still sees every GPU;
//! no filtering, no kernel component). "The seat's GPU" is translated into 1 or 2 by
//! asking Windows which adapter it ranks as minimum power / high performance
//! (`IDXGIFactory6::EnumAdapterByGpuPreference`, the service's platform layer); a third
//! GPU cannot be named by a documented value, so nothing is written for it.
//!
//! Every write is journaled first ([`Change::UserGpuPreference`]) with the previous data,
//! and undone through `HKEY_USERS\<sid>` (the profile's `NTUSER.DAT` is loaded when the
//! user is not logged on) on disable, reset and uninstall. HYPOTHESIS (nothing ran on
//! hardware): DXGI reads the value at app start, so a changed preference applies to apps
//! started afterwards.

use crate::journal::{Change, Journal, JournalError, RevertOutcome};
use crate::ops::{Hive, OpsError, RegData, SystemOps};
use seat_core::config::{GpuPreference, GpuPreferenceMode};

/// Key under the user's hive root.
pub const USER_GPU_PREFERENCES: &str = r"Software\Microsoft\DirectX\UserGpuPreferences";
/// `GpuPreference` values.
pub const PREF_POWER_SAVING: u32 = 1;
pub const PREF_HIGH_PERFORMANCE: u32 = 2;
/// Journal owner prefix (one owner per user SID).
pub const OWNER_PREFIX: &str = "gpu_preference:";

/// Journal owner of user `sid`'s values.
pub fn owner(sid: &str) -> String {
    format!("{OWNER_PREFIX}{}", sid.to_ascii_uppercase())
}

/// `GpuPreference` in a `key=value;` string, if present.
pub fn preference_of(data: &str) -> Option<u32> {
    data.split(';').filter_map(|p| p.split_once('=')).find(|(k, _)| k.trim().eq_ignore_ascii_case("GpuPreference")).and_then(|(_, v)| v.trim().parse().ok())
}

/// `existing` with `GpuPreference` set to `pref` (other pairs kept, in order; ours first
/// when it was absent). Always ends in `;`.
pub fn merge_value(existing: Option<&str>, pref: u32) -> String {
    let mut out = format!("GpuPreference={pref};");
    for p in existing.unwrap_or("").split(';').map(str::trim).filter(|p| !p.is_empty()) {
        if p.split_once('=').is_some_and(|(k, _)| k.trim().eq_ignore_ascii_case("GpuPreference")) {
            continue;
        }
        out.push_str(p);
        out.push(';');
    }
    out
}

/// The `GpuPreference` number a seat's setting means on this PC.
///
/// `seat_gpu` = adapter LUID of the seat's monitors; `high` / `low` = the adapters
/// Windows ranks as high performance / minimum power. `Err(reason)` when `SeatGpu` cannot
/// be expressed (reason for the log / manager): `seat_gpu_unknown`, `single_gpu` (both
/// ranks are the same adapter: nothing to choose), `seat_gpu_not_ranked` (a third GPU).
pub fn resolve(mode: GpuPreferenceMode, seat_gpu: Option<u64>, high: Option<u64>, low: Option<u64>) -> Result<Option<u32>, &'static str> {
    match mode {
        GpuPreferenceMode::Off => Ok(None),
        GpuPreferenceMode::HighPerformance => Ok(Some(PREF_HIGH_PERFORMANCE)),
        GpuPreferenceMode::PowerSaving => Ok(Some(PREF_POWER_SAVING)),
        GpuPreferenceMode::SeatGpu => {
            let seat = seat_gpu.filter(|&l| l != 0).ok_or("seat_gpu_unknown")?;
            if high.is_some() && high == low {
                return Err("single_gpu");
            }
            if Some(seat) == high {
                Ok(Some(PREF_HIGH_PERFORMANCE))
            } else if Some(seat) == low {
                Ok(Some(PREF_POWER_SAVING))
            } else {
                Err("seat_gpu_not_ranked")
            }
        }
    }
}

/// What [`sync_user`] did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SyncReport {
    pub written: usize,
    pub unchanged: usize,
    pub reverted: usize,
    /// (program, error) of values that could not be written.
    pub failed: Vec<(String, String)>,
    /// The user's profile does not exist (nothing written).
    pub no_profile: bool,
}

/// Make user `sid`'s values match `apps` + `pref`: values of programs no longer listed
/// (or all, with `pref` `None`) are reverted, the listed ones written. Journal first.
pub fn sync_user(j: &Journal, ops: &dyn SystemOps, sid: &str, apps: &[String], pref: Option<u32>) -> Result<SyncReport, JournalError> {
    let mut rep = SyncReport::default();
    if !crate::ops::is_sid_string(sid) {
        return Err(JournalError::Ops(OpsError::new("gpu_preference", format!("not a SID: {sid}"))));
    }
    let own = owner(sid);
    // Revert what is no longer wanted (newest first, one target at a time).
    let keep = |app: &str| pref.is_some() && apps.iter().any(|a| a.eq_ignore_ascii_case(app));
    let mut stale: Vec<_> = j.active_for(&own)?.into_iter().filter(|e| matches!(&e.change, Change::UserGpuPreference { app, .. } if !keep(app))).collect();
    stale.reverse();
    for e in stale {
        if let Change::UserGpuPreference { sid, app, previous, written } = &e.change {
            match revert_value(ops, sid, app, previous.as_deref(), written, false) {
                Ok(o) => {
                    j.close(e.seq, o)?;
                    rep.reverted += 1;
                }
                Err(err) => rep.failed.push((app.clone(), err.to_string())),
            }
        }
    }
    let Some(pref) = pref else { return Ok(rep) };
    let mut failed = Vec::new();
    let found = ops.with_user_hive(sid, &mut |root| {
        let key = format!("{root}\\{USER_GPU_PREFERENCES}");
        for app in apps {
            let cur = match ops.reg_read(Hive::Users, &key, app) {
                Ok(v) => v.and_then(|d| d.as_str().map(str::to_string)),
                Err(e) => {
                    failed.push((app.clone(), e.to_string()));
                    continue;
                }
            };
            let want = merge_value(cur.as_deref(), pref);
            if cur.as_deref() == Some(want.as_str()) {
                rep.unchanged += 1;
                continue;
            }
            let change = Change::UserGpuPreference { sid: sid.to_string(), app: app.clone(), previous: cur.clone(), written: want.clone() };
            let entry = match j.record(&own, change) {
                Ok(e) => e,
                Err(e) => {
                    failed.push((app.clone(), e.to_string()));
                    continue;
                }
            };
            match ops.reg_write(Hive::Users, &key, app, &RegData::Sz(want)) {
                Ok(()) => rep.written += 1,
                Err(e) => {
                    let _ = j.close(entry.seq, RevertOutcome::NotApplied);
                    failed.push((app.clone(), e.to_string()));
                }
            }
        }
        Ok(())
    })?;
    rep.failed.extend(failed);
    rep.no_profile = !found;
    Ok(rep)
}

/// Undo one value in user `sid`'s hive: restore `previous` (or delete the value) when it
/// still holds `written` (or `force`). The profile being gone closes the entry.
pub fn revert_value(ops: &dyn SystemOps, sid: &str, app: &str, previous: Option<&str>, written: &str, force: bool) -> Result<RevertOutcome, OpsError> {
    let mut outcome = RevertOutcome::Closed;
    let found = ops.with_user_hive(sid, &mut |root| {
        let key = format!("{root}\\{USER_GPU_PREFERENCES}");
        let cur = ops.reg_read(Hive::Users, &key, app)?.and_then(|d| d.as_str().map(str::to_string));
        outcome = if cur.as_deref() == previous {
            RevertOutcome::AlreadyRestored
        } else if cur.as_deref() != Some(written) && !force {
            RevertOutcome::ChangedSince
        } else {
            match previous {
                Some(p) => ops.reg_write(Hive::Users, &key, app, &RegData::Sz(p.to_string()))?,
                None => ops.reg_delete(Hive::Users, &key, app)?,
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

/// Seat config shortcut: the programs and the mode, `apps` empty = nothing to do.
pub fn wanted(p: &GpuPreference) -> bool {
    p.mode != GpuPreferenceMode::Off && !p.apps.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::MemoryOps;

    const SID: &str = "S-1-5-21-1-2-3-1001";

    #[test]
    fn value_strings() {
        assert_eq!(merge_value(None, 2), "GpuPreference=2;");
        assert_eq!(merge_value(Some("SwapEffectUpgradeEnable=1;GpuPreference=1;"), 2), "GpuPreference=2;SwapEffectUpgradeEnable=1;");
        assert_eq!(merge_value(Some("garbage"), 1), "GpuPreference=1;garbage;");
        assert_eq!(preference_of("SwapEffectUpgradeEnable=1;GpuPreference=2;"), Some(2));
        assert_eq!(preference_of("x=1;"), None);
    }

    #[test]
    fn seat_gpu_resolution() {
        use GpuPreferenceMode::*;
        assert_eq!(resolve(Off, Some(5), Some(5), Some(6)), Ok(None));
        assert_eq!(resolve(HighPerformance, None, None, None), Ok(Some(2)));
        assert_eq!(resolve(PowerSaving, None, None, None), Ok(Some(1)));
        assert_eq!(resolve(SeatGpu, Some(5), Some(5), Some(6)), Ok(Some(2)), "discrete GPU drives the seat");
        assert_eq!(resolve(SeatGpu, Some(6), Some(5), Some(6)), Ok(Some(1)), "integrated GPU drives the seat");
        assert_eq!(resolve(SeatGpu, Some(7), Some(5), Some(6)), Err("seat_gpu_not_ranked"));
        assert_eq!(resolve(SeatGpu, Some(5), Some(5), Some(5)), Err("single_gpu"));
        assert_eq!(resolve(SeatGpu, None, Some(5), Some(6)), Err("seat_gpu_unknown"));
    }

    #[test]
    fn sync_writes_journals_and_reverts() {
        let ops = MemoryOps::new();
        ops.add_key(Hive::Users, SID);
        let key = format!("{SID}\\{USER_GPU_PREFERENCES}");
        // A value the user set before (with another pair Windows keeps).
        ops.set(Hive::Users, &key, r"C:\b.exe", RegData::Sz("SwapEffectUpgradeEnable=1;".into()));
        let j = Journal::memory();
        let apps = vec![r"C:\a.exe".to_string(), r"C:\b.exe".to_string()];
        let rep = sync_user(&j, &ops, SID, &apps, Some(2)).unwrap();
        assert_eq!((rep.written, rep.unchanged, rep.reverted), (2, 0, 0));
        assert_eq!(ops.get(Hive::Users, &key, r"C:\a.exe"), Some(RegData::Sz("GpuPreference=2;".into())));
        assert_eq!(ops.get(Hive::Users, &key, r"C:\b.exe"), Some(RegData::Sz("GpuPreference=2;SwapEffectUpgradeEnable=1;".into())));
        // Same again: nothing new.
        assert_eq!(sync_user(&j, &ops, SID, &apps, Some(2)).unwrap().unchanged, 2);
        // a.exe leaves the list: its value is deleted (it did not exist before).
        let rep = sync_user(&j, &ops, SID, &apps[1..], Some(2)).unwrap();
        assert_eq!(rep.reverted, 1);
        assert_eq!(ops.get(Hive::Users, &key, r"C:\a.exe"), None);
        // Mode changes 2 -> 1: the first previous value is kept for the final undo.
        sync_user(&j, &ops, SID, &apps[1..], Some(1)).unwrap();
        assert_eq!(ops.get(Hive::Users, &key, r"C:\b.exe"), Some(RegData::Sz("GpuPreference=1;SwapEffectUpgradeEnable=1;".into())));
        let (n, failed) = revert_all(&j, &ops).unwrap();
        assert!(n >= 1 && failed.is_empty(), "{n} {failed:?}");
        assert_eq!(ops.get(Hive::Users, &key, r"C:\b.exe"), Some(RegData::Sz("SwapEffectUpgradeEnable=1;".into())), "user's own value back");
        assert!(j.active().unwrap().is_empty());
    }

    #[test]
    fn user_changes_win_and_missing_profiles_close() {
        let ops = MemoryOps::new();
        ops.add_key(Hive::Users, SID);
        let key = format!("{SID}\\{USER_GPU_PREFERENCES}");
        let j = Journal::memory();
        sync_user(&j, &ops, SID, &[r"C:\a.exe".into()], Some(2)).unwrap();
        // The user picked another GPU in Settings afterwards: left alone.
        ops.set(Hive::Users, &key, r"C:\a.exe", RegData::Sz("GpuPreference=1;".into()));
        sync_user(&j, &ops, SID, &[], None).unwrap();
        assert_eq!(ops.get(Hive::Users, &key, r"C:\a.exe"), Some(RegData::Sz("GpuPreference=1;".into())));
        // No profile: nothing written, reported.
        let other = "S-1-5-21-9-9-9-1002";
        let rep = sync_user(&j, &ops, other, &[r"C:\a.exe".into()], Some(2)).unwrap();
        assert!(rep.no_profile && rep.written == 0);
        assert_eq!(revert_value(&ops, other, r"C:\a.exe", None, "GpuPreference=2;", false).unwrap(), RevertOutcome::Closed);
        assert!(sync_user(&j, &ops, "not-a-sid", &[], None).is_err());
        assert!(wanted(&GpuPreference { mode: GpuPreferenceMode::SeatGpu, apps: vec!["x".into()] }));
        assert!(!wanted(&GpuPreference::default()));
    }
}
