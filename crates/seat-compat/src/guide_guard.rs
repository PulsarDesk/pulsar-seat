// SPDX-License-Identifier: GPL-3.0-only
//! Guide-button guard (OSMS parity G5, 2026-11-18).
//!
//! The Xbox Guide button of any pad reaches the Xbox Game Bar of **every** signed-in
//! session: a player in one seat would open the Game Bar on the console and in the other
//! seats. While seats with scoped pads run, the service turns off Game Bar's "open Xbox Game
//! Bar using this button on a controller" for the signed-in seat users and the console user
//! (`HKEY_USERS\<sid>\Software\Microsoft\GameBar\UseNexusForGameBarEnabled` = 0), and sets
//! the machine-wide `ControllerToVKMapping\Enabled` = 0 (Windows' controller-to-keyboard
//! navigation mapping reads pads across sessions).
//!
//! Every write is journaled **first** in `%ProgramData%\Pulsar Seat\journal\changes.jsonl`
//! ([`Change::UserGameBarGuide`] with the previous data; the machine value as a
//! [`Change::Registry`] under owner [`VK_OWNER`]). The service syncs to the wanted set
//! ("ref-counting" by set: a SID stays guarded while any running seat needs it), and
//! reverts what is no longer wanted on seat stop, on service stop, and at the next service
//! start after a crash or reboot (the first sync with nothing wanted). OSMS keeps the
//! previous value only in the user's own hive and has no start-up restore; this does both
//! from the journal. A user value is written only while that user's hive is loaded
//! (signed in); a revert loads `NTUSER.DAT` when the user is not signed in, like the other
//! per-user undo paths. A value someone changed after us is left alone (`changed_since`).
//!
//! HYPOTHESIS (nothing ran on hardware; behaviour reference: OSMS `GuideButtonGuard` /
//! `GamepadSessionIsolation`, MIT, no code copied): Game Bar reads the per-user value when
//! the button is pressed, so no sign-out is needed; `ControllerToVKMapping\Enabled` = 0
//! stops the cross-session navigation mapping.

use std::collections::BTreeSet;

use crate::journal::{Change, Journal, JournalError, RevertOutcome, set_registry};
use crate::ops::{Hive, OpsError, RegData, SystemOps};

/// Key under the user's hive root.
pub const GAME_BAR_KEY: &str = r"Software\Microsoft\GameBar";
pub const NEXUS_VALUE: &str = "UseNexusForGameBarEnabled";
/// Machine key / value of the controller-to-VK mapping.
pub const VK_KEY: &str = r"SOFTWARE\Microsoft\Input\Settings\ControllerProcessor\ControllerToVKMapping";
pub const VK_VALUE: &str = "Enabled";
/// Journal owner prefix of the per-user values (one owner per user SID).
pub const OWNER_PREFIX: &str = "guide_guard:";
/// Journal owner of the machine value.
pub const VK_OWNER: &str = "guide_guard_vk";

pub fn owner(sid: &str) -> String {
    format!("{OWNER_PREFIX}{}", sid.to_ascii_uppercase())
}

/// The machine value this module writes (allowed in the journal's revert allowlist).
pub fn is_machine_target(key: &str, name: &str) -> bool {
    key.trim_matches('\\').eq_ignore_ascii_case(VK_KEY) && name.eq_ignore_ascii_case(VK_VALUE)
}

/// What the service wants guarded now.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GuardWant {
    /// User SIDs (seat users and the console user) whose Game Bar Guide button is off.
    pub sids: Vec<String>,
    /// `ControllerToVKMapping\Enabled` = 0.
    pub vk_mapping_off: bool,
}

impl GuardWant {
    pub fn is_empty(&self) -> bool {
        self.sids.is_empty() && !self.vk_mapping_off
    }
}

/// What [`sync`] did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SyncReport {
    pub written: usize,
    pub unchanged: usize,
    pub reverted: usize,
    /// Wanted users whose hive is not loaded (not signed in): retried at the next sync.
    pub not_loaded: Vec<String>,
    pub failed: Vec<(String, String)>,
}

impl SyncReport {
    /// One log line (empty when nothing happened).
    pub fn summary(&self) -> String {
        if self.written + self.reverted == 0 && self.failed.is_empty() {
            return String::new();
        }
        format!("{} written, {} reverted, {} failed", self.written, self.reverted, self.failed.len())
    }
}

fn user_key(root: &str) -> String {
    format!("{root}\\{GAME_BAR_KEY}")
}

/// Make the guarded values match `want`: users no longer wanted get their value back,
/// wanted users get 0 (journal first), and the machine value follows `vk_mapping_off`.
pub fn sync(j: &Journal, ops: &dyn SystemOps, want: &GuardWant) -> Result<SyncReport, JournalError> {
    let mut rep = SyncReport::default();
    let wanted: BTreeSet<String> = want.sids.iter().filter(|s| crate::ops::is_sid_string(s)).map(|s| s.to_ascii_uppercase()).collect();
    // Users no longer wanted.
    let mut owners: Vec<String> = j.active()?.into_iter().map(|e| e.owner).filter(|o| o.starts_with(OWNER_PREFIX)).collect();
    owners.sort();
    owners.dedup();
    for o in owners {
        if wanted.contains(&o[OWNER_PREFIX.len()..]) {
            continue;
        }
        let r = j.revert_owner(ops, &o, false)?;
        rep.reverted += r.done.len();
        rep.failed.extend(r.failed.into_iter().map(|(_, e)| (o.clone(), e)));
    }
    // Wanted users.
    let zero = RegData::Dword(0);
    for sid in &wanted {
        let own = owner(sid);
        if !j.active_for(&own)?.is_empty() {
            // Ours already (a later change by the user is left alone).
            rep.unchanged += 1;
            continue;
        }
        // Loaded hives only: HKEY_USERS\<sid> exists while the user is signed in.
        if !ops.reg_key_exists(Hive::Users, sid).unwrap_or(false) {
            rep.not_loaded.push(sid.clone());
            continue;
        }
        let key = user_key(sid);
        let cur = match ops.reg_read(Hive::Users, &key, NEXUS_VALUE) {
            Ok(c) => c,
            Err(e) => {
                rep.failed.push((sid.clone(), e.to_string()));
                continue;
            }
        };
        if cur.as_ref() == Some(&zero) {
            rep.unchanged += 1; // already off by the user's own choice: not ours
            continue;
        }
        let e = j.record(&own, Change::UserGameBarGuide { sid: sid.clone(), previous: cur, written: zero.clone() })?;
        match ops.reg_write(Hive::Users, &key, NEXUS_VALUE, &zero) {
            Ok(()) => rep.written += 1,
            Err(err) => {
                j.close(e.seq, RevertOutcome::NotApplied)?;
                rep.failed.push((sid.clone(), err.to_string()));
            }
        }
    }
    // Machine value.
    if want.vk_mapping_off {
        match set_registry(j, ops, VK_OWNER, Hive::Hklm, VK_KEY, VK_VALUE, Some(&zero)) {
            Ok(true) => rep.written += 1,
            Ok(false) => rep.unchanged += 1,
            Err(e) => rep.failed.push((VK_OWNER.into(), e.to_string())),
        }
    } else if !j.active_for(VK_OWNER)?.is_empty() {
        let r = j.revert_owner(ops, VK_OWNER, false)?;
        rep.reverted += r.done.len();
        rep.failed.extend(r.failed.into_iter().map(|(_, e)| (VK_OWNER.to_string(), e)));
    }
    Ok(rep)
}

/// Undo every guarded value (service stop, product disabled).
pub fn revert_all(j: &Journal, ops: &dyn SystemOps) -> Result<SyncReport, JournalError> {
    sync(j, ops, &GuardWant::default())
}

/// Undo one user value (journal revert).
pub fn revert_value(ops: &dyn SystemOps, sid: &str, previous: Option<&RegData>, written: &RegData, force: bool) -> Result<RevertOutcome, OpsError> {
    let mut outcome = RevertOutcome::Closed;
    let found = ops.with_user_hive(sid, &mut |root| {
        let key = user_key(root);
        let cur = ops.reg_read(Hive::Users, &key, NEXUS_VALUE)?;
        outcome = if cur.as_ref() == previous {
            RevertOutcome::AlreadyRestored
        } else if cur.as_ref() != Some(written) && !force {
            RevertOutcome::ChangedSince
        } else {
            match previous {
                Some(p) => ops.reg_write(Hive::Users, &key, NEXUS_VALUE, p)?,
                None => ops.reg_delete(Hive::Users, &key, NEXUS_VALUE)?,
            }
            RevertOutcome::Restored
        };
        Ok(())
    })?;
    // Profile deleted: nothing left to undo.
    Ok(if found { outcome } else { RevertOutcome::Closed })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::MemoryOps;

    const A: &str = "S-1-5-21-1-2-3-1001";
    const B: &str = "S-1-5-21-1-2-3-1002";

    fn loaded(ops: &MemoryOps, sid: &str) {
        // A key under the user's root makes the hive "loaded" for MemoryOps.
        ops.set(Hive::Users, &format!(r"{sid}\Volatile Environment"), "x", RegData::Sz("1".into()));
    }

    fn nexus(ops: &MemoryOps, sid: &str) -> Option<RegData> {
        ops.get(Hive::Users, &user_key(sid), NEXUS_VALUE)
    }

    #[test]
    fn guard_writes_journals_and_restores() {
        let ops = MemoryOps::new();
        let j = Journal::memory();
        loaded(&ops, A);
        loaded(&ops, B);
        ops.set(Hive::Users, &user_key(A), NEXUS_VALUE, RegData::Dword(1));
        ops.set(Hive::Hklm, VK_KEY, VK_VALUE, RegData::Dword(1));
        let want = GuardWant { sids: vec![A.into(), B.into()], vk_mapping_off: true };
        let r = sync(&j, &ops, &want).unwrap();
        assert_eq!((r.written, r.failed.len()), (3, 0), "{r:?}");
        assert_eq!(nexus(&ops, A), Some(RegData::Dword(0)));
        assert_eq!(nexus(&ops, B), Some(RegData::Dword(0)));
        assert_eq!(ops.get(Hive::Hklm, VK_KEY, VK_VALUE), Some(RegData::Dword(0)));
        // Same want again: nothing written.
        let r = sync(&j, &ops, &want).unwrap();
        assert_eq!((r.written, r.unchanged), (0, 3));
        // Seat of B stopped: B back to absent, A and the machine value stay.
        let r = sync(&j, &ops, &GuardWant { sids: vec![A.into()], vk_mapping_off: true }).unwrap();
        assert_eq!(r.reverted, 1);
        assert_eq!(nexus(&ops, B), None);
        assert_eq!(nexus(&ops, A), Some(RegData::Dword(0)));
        // Service stop: everything back.
        let r = revert_all(&j, &ops).unwrap();
        assert!(r.failed.is_empty(), "{r:?}");
        assert_eq!(nexus(&ops, A), Some(RegData::Dword(1)));
        assert_eq!(ops.get(Hive::Hklm, VK_KEY, VK_VALUE), Some(RegData::Dword(1)));
        assert!(j.active().unwrap().is_empty());
    }

    #[test]
    fn crash_leftovers_are_reverted_by_the_first_sync_and_uninstall() {
        let ops = MemoryOps::new();
        let j = Journal::memory();
        loaded(&ops, A);
        sync(&j, &ops, &GuardWant { sids: vec![A.into()], vk_mapping_off: true }).unwrap();
        // "Crash": a new service start syncs with nothing wanted yet.
        let r = sync(&j, &ops, &GuardWant::default()).unwrap();
        assert_eq!(r.reverted, 2);
        assert_eq!(nexus(&ops, A), None);
        assert_eq!(ops.get(Hive::Hklm, VK_KEY, VK_VALUE), None);
        // The uninstall path (journal revert_all) also knows both change kinds.
        sync(&j, &ops, &GuardWant { sids: vec![A.into()], vk_mapping_off: true }).unwrap();
        let rep = j.revert_all(&ops, false).unwrap();
        assert!(rep.is_clean(), "{rep:?}");
        assert_eq!(nexus(&ops, A), None);
    }

    #[test]
    fn not_loaded_users_and_users_own_choices_are_left_alone() {
        let ops = MemoryOps::new();
        let j = Journal::memory();
        let r = sync(&j, &ops, &GuardWant { sids: vec![A.into(), "junk".into()], vk_mapping_off: false }).unwrap();
        assert_eq!(r.not_loaded, vec![A.to_string()], "hive not loaded: nothing written");
        assert!(j.active().unwrap().is_empty());
        // Already 0 by the user's choice: not journaled, so a revert never touches it.
        loaded(&ops, A);
        ops.set(Hive::Users, &user_key(A), NEXUS_VALUE, RegData::Dword(0));
        let r = sync(&j, &ops, &GuardWant { sids: vec![A.into()], vk_mapping_off: false }).unwrap();
        assert_eq!((r.written, r.unchanged), (0, 1));
        assert!(j.active().unwrap().is_empty());
        // The user turns it back on while guarded: our revert leaves the newer choice.
        ops.set(Hive::Users, &user_key(A), NEXUS_VALUE, RegData::Dword(1));
        sync(&j, &ops, &GuardWant { sids: vec![A.into()], vk_mapping_off: false }).unwrap();
        ops.set(Hive::Users, &user_key(A), NEXUS_VALUE, RegData::Dword(1));
        let r = revert_all(&j, &ops).unwrap();
        assert_eq!(r.reverted, 1);
        assert_eq!(nexus(&ops, A), Some(RegData::Dword(1)));
    }

    #[test]
    fn machine_value_is_in_the_revert_allowlist() {
        assert!(is_machine_target(VK_KEY, "enabled"));
        assert!(crate::tweaks::is_tweak_target(Hive::Hklm, VK_KEY, VK_VALUE));
        assert!(!crate::tweaks::is_tweak_target(Hive::Hklm, VK_KEY, "Other"));
        assert!(!crate::tweaks::is_tweak_target(Hive::Users, VK_KEY, VK_VALUE));
    }
}
