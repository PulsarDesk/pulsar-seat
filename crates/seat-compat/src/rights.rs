// SPDX-License-Identifier: GPL-3.0-only
//! "Deny log on locally" per seat (spec/remote 30 §10; compat-tweaks 2026-11-17).
//!
//! A seat that signs in through Remote Desktop (a remote seat, or a local seat on the
//! `rds_loopback` / `termsrv_wrapper` session methods) uses a `RemoteInteractive` logon.
//! With the seat option `deny_local_logon` the service gives the seat's account the
//! documented `SeDenyInteractiveLogonRight` (`LsaAddAccountRights`), so the same account
//! cannot also sign in at the console or on a physical seat while the seat runs.
//!
//! Journaled ([`Change::AccountRight`]) **only when the account did not already have the
//! right**: an operator's own policy is never recorded and never removed. Disable, reset
//! and uninstall remove what Pulsar Seat added (`LsaRemoveAccountRights`). Group Policy
//! may overwrite local rights at the next refresh on domain PCs (documented limit).
//! HYPOTHESIS (not run on hardware): Windows refuses a console sign-in of the account at
//! once (no reboot), and the RDP sign-in of the seat keeps working.

use crate::journal::{Change, Journal, JournalError, RevertOutcome};
use crate::ops::SystemOps;

/// The documented user right (`ntsecapi.h` `SE_DENY_INTERACTIVE_LOGON_NAME`).
pub const DENY_INTERACTIVE: &str = "SeDenyInteractiveLogonRight";
/// Journal owner.
pub const OWNER: &str = "logon_right:deny_local";

/// What [`sync`] did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SyncReport {
    pub added: usize,
    pub unchanged: usize,
    pub removed: usize,
    pub failed: Vec<(String, String)>,
}

/// Make the deny right match `sids` (the accounts of seats with the option): add it
/// where missing (journal first), remove it from accounts we added it to that are no
/// longer listed. Accounts that already had the right are left alone.
pub fn sync(j: &Journal, ops: &dyn SystemOps, sids: &[String]) -> Result<SyncReport, JournalError> {
    let mut rep = SyncReport::default();
    let wanted: Vec<String> = sids.iter().filter(|s| crate::ops::is_sid_string(s)).map(|s| s.to_ascii_uppercase()).collect();
    let keep: Vec<String> =
        wanted.iter().map(|s| Change::AccountRight { sid: s.clone(), right: DENY_INTERACTIVE.into() }.target()).collect();
    let r = j.revert_owner_except(ops, OWNER, &keep)?;
    rep.removed = r.done.len();
    rep.failed.extend(r.failed.into_iter().map(|(seq, e)| (format!("entry {seq}"), e)));
    let ours: Vec<String> = j.active_for(OWNER)?.into_iter().map(|e| e.change.target()).collect();
    for sid in wanted {
        let change = Change::AccountRight { sid: sid.clone(), right: DENY_INTERACTIVE.into() };
        let has = match ops.account_rights(&sid) {
            Ok(r) => r.iter().any(|x| x == DENY_INTERACTIVE),
            Err(e) => {
                rep.failed.push((sid, e.to_string()));
                continue;
            }
        };
        if has || ours.contains(&change.target()) {
            rep.unchanged += 1;
            continue;
        }
        let e = j.record(OWNER, change)?;
        match ops.account_right(&sid, DENY_INTERACTIVE, true) {
            Ok(()) => rep.added += 1,
            Err(err) => {
                j.close(e.seq, RevertOutcome::NotApplied)?;
                rep.failed.push((sid, err.to_string()));
            }
        }
    }
    Ok(rep)
}

/// Remove every right we added (disable, reset).
pub fn revert_all(j: &Journal, ops: &dyn SystemOps) -> Result<crate::journal::RevertReport, JournalError> {
    j.revert_owner(ops, OWNER, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::MemoryOps;

    const A: &str = "S-1-5-21-1-2-3-1001";
    const B: &str = "S-1-5-21-1-2-3-1002";

    #[test]
    fn adds_only_missing_rights_and_removes_only_ours() {
        let ops = MemoryOps::new();
        let j = Journal::memory();
        // B already has the right through the operator's own policy.
        ops.account_right(B, DENY_INTERACTIVE, true).unwrap();
        let r = sync(&j, &ops, &[A.into(), B.into()]).unwrap();
        assert_eq!((r.added, r.unchanged), (1, 1), "{r:?}");
        assert!(ops.account_rights(A).unwrap().contains(&DENY_INTERACTIVE.to_string()));
        let again = sync(&j, &ops, &[A.into(), B.into()]).unwrap();
        assert_eq!((again.added, again.unchanged), (0, 2));
        // Option off everywhere: our right goes, the operator's stays.
        let off = sync(&j, &ops, &[]).unwrap();
        assert_eq!(off.removed, 1);
        assert!(ops.account_rights(A).unwrap().is_empty());
        assert!(ops.account_rights(B).unwrap().contains(&DENY_INTERACTIVE.to_string()));
    }

    #[test]
    fn uninstall_reverts_and_refuses_foreign_rights() {
        let ops = MemoryOps::new();
        let j = Journal::memory();
        sync(&j, &ops, &[A.into(), "not-a-sid".into()]).unwrap();
        // A tampered entry for another right is refused, never removed.
        ops.account_right(B, "SeDebugPrivilege", true).unwrap();
        j.record(OWNER, Change::AccountRight { sid: B.into(), right: "SeDebugPrivilege".into() }).unwrap();
        let rep = j.revert_all(&ops, false).unwrap();
        assert!(rep.done.iter().any(|(_, o)| *o == RevertOutcome::Refused));
        assert!(ops.account_rights(A).unwrap().is_empty());
        assert!(ops.account_rights(B).unwrap().contains(&"SeDebugPrivilege".to_string()));
    }

    #[test]
    fn failed_write_is_not_left_active() {
        let ops = MemoryOps::new();
        let j = Journal::memory();
        *ops.fail_next_write.lock().unwrap() = Some("denied".into());
        let r = sync(&j, &ops, &[A.into()]).unwrap();
        assert_eq!(r.failed.len(), 1);
        assert!(j.active().unwrap().is_empty());
    }
}
