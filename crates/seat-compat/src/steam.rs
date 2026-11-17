// SPDX-License-Identifier: GPL-3.0-only
//! Steam on seats (spec/remote 50 §6.2, spec/local 80 §3.2 / §11.1; compat-tweaks
//! 2026-11-17): the legacy proxy-DLL report.
//!
//! Older "multibox" add-ons put a proxy `version.dll` next to `steam.exe`, which the
//! Steam client then loads instead of the system DLL. The reference product deletes such a
//! file at service start. Pulsar Seat never deletes files in the Steam folder by itself:
//! the service **reports** a found file (pre-flight check `steam_proxy_dll`, warning, with
//! the path) and the administrator may move it into Pulsar Seat's quarantine folder from
//! the manager ([`quarantine`]). The move is journaled ([`Change::QuarantinedFile`]) and
//! can be undone ([`restore`]); uninstall moves it back too (it is the operator's file).
//!
//! Steam folders: `InstallPath` under `HKLM\SOFTWARE\WOW6432Node\Valve\Steam` and
//! `HKLM\SOFTWARE\Valve\Steam` (written by Steam's installer). Detection = the file is
//! present; no content check (the reference does none either). HYPOTHESIS: current Steam
//! clients do not ship a `version.dll` of their own in that folder.

use crate::journal::{Change, Journal, JournalError, RevertOutcome};
use crate::ops::{Hive, OpsError, SystemOps};

/// Proxy DLL names looked for next to `steam.exe`.
pub const PROXY_DLLS: [&str; 1] = ["version.dll"];
/// Registry keys with Steam's `InstallPath`.
pub const STEAM_KEYS: [&str; 2] = [r"SOFTWARE\WOW6432Node\Valve\Steam", r"SOFTWARE\Valve\Steam"];
/// Quarantine sub-folder of the data folder.
pub const QUARANTINE_DIR: &str = "quarantine";
/// Journal owner.
pub const OWNER: &str = "steam:proxy_dll";

/// Steam install folders on this PC (deduplicated, without trailing `\`).
pub fn steam_dirs(ops: &dyn SystemOps) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for k in STEAM_KEYS {
        if let Ok(Some(d)) = ops.reg_read(Hive::Hklm, k, "InstallPath")
            && let Some(p) = d.as_str()
        {
            let p = p.trim().trim_end_matches(['\\', '/']).replace('/', "\\");
            if p.len() > 3 && !out.iter().any(|o| o.eq_ignore_ascii_case(&p)) {
                out.push(p);
            }
        }
    }
    out
}

/// Proxy DLLs present in the Steam folders (full paths).
pub fn proxy_dlls(ops: &dyn SystemOps) -> Vec<String> {
    let mut out = Vec::new();
    for d in steam_dirs(ops) {
        for n in PROXY_DLLS {
            let p = format!("{d}\\{n}");
            if ops.file_exists(&p).unwrap_or(false) {
                out.push(p);
            }
        }
    }
    out
}

/// Journal entries may only name a proxy DLL and a file in our quarantine folder.
pub fn is_quarantine_pair(original: &str, quarantined: &str) -> bool {
    let o = original.to_ascii_lowercase();
    let q = quarantined.to_ascii_lowercase();
    PROXY_DLLS.iter().any(|n| o.ends_with(&format!("\\{n}")) && q.ends_with(&format!("-{n}")))
        && q.contains(&format!("\\pulsar seat\\{QUARANTINE_DIR}\\"))
        && !o.contains("..")
        && !q.contains("..")
}

#[derive(Debug, thiserror::Error)]
pub enum SteamError {
    #[error("not_a_proxy_dll")]
    NotAProxyDll,
    #[error("not_quarantined")]
    NotQuarantined,
    #[error(transparent)]
    Journal(#[from] JournalError),
    #[error(transparent)]
    Ops(#[from] OpsError),
}

/// Move a reported proxy DLL into `<data_dir>\quarantine\<stamp>-version.dll`. `path`
/// must be one [`proxy_dlls`] reports right now (never an arbitrary file).
pub fn quarantine(j: &Journal, ops: &dyn SystemOps, path: &str, data_dir: &str, stamp: u64) -> Result<String, SteamError> {
    let found = proxy_dlls(ops);
    let Some(orig) = found.iter().find(|p| p.eq_ignore_ascii_case(path.trim())) else { return Err(SteamError::NotAProxyDll) };
    let name = orig.rsplit('\\').next().unwrap_or("version.dll");
    let dest = format!("{}\\{QUARANTINE_DIR}\\{stamp}-{name}", data_dir.trim_end_matches('\\'));
    if !is_quarantine_pair(orig, &dest) {
        return Err(SteamError::NotAProxyDll);
    }
    let e = j.record(OWNER, Change::QuarantinedFile { original: orig.clone(), quarantined: dest.clone() })?;
    if let Err(err) = ops.file_move(orig, &dest) {
        j.close(e.seq, RevertOutcome::NotApplied)?;
        return Err(err.into());
    }
    Ok(dest)
}

/// Move a quarantined file back to `original`.
pub fn restore(j: &Journal, ops: &dyn SystemOps, original: &str) -> Result<RevertOutcome, SteamError> {
    let target = Change::QuarantinedFile { original: original.trim().to_string(), quarantined: String::new() }.target();
    let keep: Vec<String> =
        j.active_for(OWNER)?.into_iter().map(|e| e.change.target()).filter(|t| *t != target).collect();
    if keep.len() == j.active_for(OWNER)?.len() {
        return Err(SteamError::NotQuarantined);
    }
    let r = j.revert_owner_except(ops, OWNER, &keep)?;
    if let Some((_, e)) = r.failed.first() {
        return Err(SteamError::Ops(OpsError::new("restore", e.clone())));
    }
    Ok(r.done.first().map(|(_, o)| *o).unwrap_or(RevertOutcome::Closed))
}

/// Files in quarantine now: (original, quarantined).
pub fn quarantined(j: &Journal) -> Result<Vec<(String, String)>, JournalError> {
    Ok(j.active_for(OWNER)?
        .into_iter()
        .filter_map(|e| match e.change {
            Change::QuarantinedFile { original, quarantined } => Some((original, quarantined)),
            _ => None,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::{MemoryOps, RegData};

    const DATA: &str = r"C:\ProgramData\Pulsar Seat";

    fn steam(ops: &MemoryOps) {
        ops.set(Hive::Hklm, STEAM_KEYS[0], "InstallPath", RegData::Sz(r"C:\Program Files (x86)\Steam".into()));
        ops.set(Hive::Hklm, STEAM_KEYS[1], "InstallPath", RegData::Sz(r"c:\program files (x86)\steam\".into()));
    }

    #[test]
    fn reports_proxy_dll_only_when_present() {
        let ops = MemoryOps::new();
        assert!(proxy_dlls(&ops).is_empty());
        steam(&ops);
        assert_eq!(steam_dirs(&ops).len(), 1);
        assert!(proxy_dlls(&ops).is_empty());
        ops.files.lock().unwrap().insert(r"c:\program files (x86)\steam\version.dll".into());
        assert_eq!(proxy_dlls(&ops), vec![r"C:\Program Files (x86)\Steam\version.dll".to_string()]);
    }

    #[test]
    fn quarantine_and_restore_roundtrip() {
        let ops = MemoryOps::new();
        let j = Journal::memory();
        steam(&ops);
        let dll = r"C:\Program Files (x86)\Steam\version.dll";
        ops.files.lock().unwrap().insert(dll.to_ascii_lowercase());
        // Only a reported file may be moved.
        assert!(matches!(quarantine(&j, &ops, r"C:\Windows\System32\version.dll", DATA, 1), Err(SteamError::NotAProxyDll)));
        let dest = quarantine(&j, &ops, dll, DATA, 42).unwrap();
        assert_eq!(dest, format!(r"{DATA}\quarantine\42-version.dll"));
        assert!(proxy_dlls(&ops).is_empty());
        assert_eq!(quarantined(&j).unwrap().len(), 1);
        assert_eq!(restore(&j, &ops, dll).unwrap(), RevertOutcome::Restored);
        assert_eq!(proxy_dlls(&ops).len(), 1);
        assert!(matches!(restore(&j, &ops, dll), Err(SteamError::NotQuarantined)));
    }

    #[test]
    fn uninstall_moves_back_unless_replaced() {
        let ops = MemoryOps::new();
        let j = Journal::memory();
        steam(&ops);
        let dll = r"C:\Program Files (x86)\Steam\version.dll";
        ops.files.lock().unwrap().insert(dll.to_ascii_lowercase());
        quarantine(&j, &ops, dll, DATA, 7).unwrap();
        // Steam put a file of the same name back meanwhile: ours stays in quarantine.
        ops.files.lock().unwrap().insert(dll.to_ascii_lowercase());
        let rep = j.revert_all(&ops, false).unwrap();
        assert!(rep.done.iter().any(|(_, o)| *o == RevertOutcome::ChangedSince));
        assert!(!is_quarantine_pair(dll, r"C:\Windows\x-version.dll"));
        assert!(!is_quarantine_pair(r"C:\Steam\steam.exe", &format!(r"{DATA}\quarantine\1-steam.exe")));
    }
}
