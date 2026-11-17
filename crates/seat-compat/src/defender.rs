// SPDX-License-Identifier: GPL-3.0-only
//! Microsoft Defender exclusions for Pulsar Seat's own folders and processes
//! (compat-tweaks 2026-11-17; spec/local 20 §4.2 notes that the reference product adds one
//! at install; ours is a visible opt-in, `settings.tweaks.defender_exclusions`).
//!
//! Mechanism: the documented WMI class `MSFT_MpPreference` (namespace
//! `root/Microsoft/Windows/Defender`): its `ExclusionPath` / `ExclusionProcess` lists are
//! read, and the static methods `Add` / `Remove` change them (the service calls them
//! through `Invoke-CimMethod` in a non-interactive `powershell.exe`; `crate::win`).
//!
//! Only items that are **not** on a list yet are added, and each is journaled first
//! ([`Change::DefenderExclusion`]); turning the option off, disabling the product, reset
//! and uninstall remove exactly those. Items the operator listed are never touched.
//! When Defender is not the active antivirus (the class is missing) the option reports
//! `defender_unavailable` and changes nothing.
//!
//! Scope: the install folder, the data folder, and the three long-running executables
//! (service, seat host, agent), by full path. Never a whole drive, never a wildcard,
//! never a user-writable folder (the data folder is SYSTEM / Administrators only,
//! `seat_core::consts::DATA_DIR_SDDL`). HYPOTHESIS (not run on hardware): Tamper
//! Protection does not block exclusion changes made by LocalSystem through WMI (it blocks
//! some other settings); if it does, the option reports the error and the operator adds
//! the exclusions in Windows Security.

use crate::journal::{Change, Journal, JournalError, RevertOutcome};
use crate::ops::{ExclusionKind, SystemOps};

/// Journal owner.
pub const OWNER: &str = "tweak:defender_exclusions";

/// The items Pulsar Seat wants excluded on this PC.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OwnItems {
    pub paths: Vec<String>,
    pub processes: Vec<String>,
}

impl OwnItems {
    /// Our folders and executables under `install_dir` / `data_dir`.
    pub fn for_install(install_dir: &str, data_dir: &str) -> Self {
        let dir = install_dir.trim_end_matches('\\');
        let exes = [seat_core::consts::SERVICE_EXE, seat_core::consts::HOST_EXE, seat_core::consts::AGENT_EXE];
        Self {
            paths: [install_dir, data_dir].iter().map(|p| p.trim_end_matches('\\').to_string()).filter(|p| is_safe_path(p)).collect(),
            processes: exes.iter().map(|e| format!("{dir}\\{e}")).filter(|p| is_safe_path(p)).collect(),
        }
    }

    fn items(&self) -> Vec<(ExclusionKind, String)> {
        self.paths
            .iter()
            .map(|p| (ExclusionKind::Path, p.clone()))
            .chain(self.processes.iter().map(|p| (ExclusionKind::Process, p.clone())))
            .collect()
    }
}

/// An absolute local path at least two levels deep, no wildcard, no UNC, no drive root.
pub fn is_safe_path(p: &str) -> bool {
    let b = p.as_bytes();
    b.len() > 3
        && b[0].is_ascii_alphabetic()
        && b[1] == b':'
        && b[2] == b'\\'
        && !p.contains(['*', '?', '"', '\n', '\r'])
        && !p.contains("..")
        && p[3..].trim_end_matches('\\').contains(|c: char| c != '\\')
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub added: usize,
    pub present: usize,
    pub removed: usize,
    pub failed: Vec<(String, String)>,
}

#[derive(Debug, thiserror::Error)]
pub enum DefenderError {
    #[error("defender_unavailable")]
    Unavailable,
    #[error(transparent)]
    Journal(#[from] JournalError),
    #[error(transparent)]
    Ops(#[from] crate::ops::OpsError),
}

/// Make our exclusions match `want` (`None` = option off: remove ours).
pub fn sync(j: &Journal, ops: &dyn SystemOps, want: Option<&OwnItems>) -> Result<SyncReport, DefenderError> {
    let mut rep = SyncReport::default();
    let items = want.map(OwnItems::items).unwrap_or_default();
    let keep: Vec<String> =
        items.iter().map(|(k, v)| Change::DefenderExclusion { list: *k, value: v.clone() }.target()).collect();
    let r = j.revert_owner_except(ops, OWNER, &keep)?;
    rep.removed = r.done.len();
    rep.failed.extend(r.failed.into_iter().map(|(seq, e)| (format!("entry {seq}"), e)));
    if items.is_empty() {
        return Ok(rep);
    }
    let Some(cur) = ops.defender_exclusions()? else { return Err(DefenderError::Unavailable) };
    for (kind, value) in items {
        if cur.contains(kind, &value) {
            rep.present += 1;
            continue;
        }
        let e = j.record(OWNER, Change::DefenderExclusion { list: kind, value: value.clone() })?;
        match ops.defender_exclusion(kind, &value, true) {
            Ok(()) => rep.added += 1,
            Err(err) => {
                j.close(e.seq, RevertOutcome::NotApplied)?;
                rep.failed.push((value, err.to_string()));
            }
        }
    }
    Ok(rep)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::{DefenderExclusions, MemoryOps};

    const INSTALL: &str = r"C:\Program Files\Pulsar Seat";
    const DATA: &str = r"C:\ProgramData\Pulsar Seat";

    #[test]
    fn own_items_are_exact_paths() {
        let o = OwnItems::for_install(INSTALL, &format!("{DATA}\\"));
        assert_eq!(o.paths, vec![INSTALL.to_string(), DATA.to_string()]);
        assert_eq!(o.processes.len(), 3);
        assert!(o.processes.iter().all(|p| p.starts_with(INSTALL) && p.ends_with(".exe")));
        for bad in [r"C:\", "C:", r"\\server\share\x", r"C:\x\*", r"relative\x", r"C:\a\..\Windows"] {
            assert!(!is_safe_path(bad), "{bad}");
        }
        assert!(OwnItems::for_install(r"C:\", DATA).paths == vec![DATA.to_string()]);
    }

    #[test]
    fn adds_missing_keeps_operator_items_and_reverts() {
        let ops = MemoryOps::new();
        let j = Journal::memory();
        *ops.defender.lock().unwrap() =
            Some(DefenderExclusions { paths: vec![format!("{DATA}\\")], processes: vec![r"C:\Tools\x.exe".into()] });
        let o = OwnItems::for_install(INSTALL, DATA);
        let r = sync(&j, &ops, Some(&o)).unwrap();
        assert_eq!((r.added, r.present), (4, 1), "{r:?}");
        let again = sync(&j, &ops, Some(&o)).unwrap();
        assert_eq!((again.added, again.present), (0, 5));
        // Off: ours are removed, the operator's data-folder entry and tool stay.
        let off = sync(&j, &ops, None).unwrap();
        assert_eq!(off.removed, 4);
        let d = ops.defender.lock().unwrap().clone().unwrap();
        assert_eq!(d.paths, vec![format!("{DATA}\\")]);
        assert_eq!(d.processes, vec![r"C:\Tools\x.exe".to_string()]);
    }

    #[test]
    fn unavailable_defender_changes_nothing() {
        let ops = MemoryOps::new();
        let j = Journal::memory();
        let o = OwnItems::for_install(INSTALL, DATA);
        assert!(matches!(sync(&j, &ops, Some(&o)), Err(DefenderError::Unavailable)));
        assert!(j.active().unwrap().is_empty());
        // Uninstall after Defender disappeared: entries close without error.
        *ops.defender.lock().unwrap() = Some(DefenderExclusions::default());
        sync(&j, &ops, Some(&o)).unwrap();
        *ops.defender.lock().unwrap() = None;
        let rep = j.revert_all(&ops, false).unwrap();
        assert!(rep.is_clean());
        assert!(j.active().unwrap().is_empty());
    }
}
