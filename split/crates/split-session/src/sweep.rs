// SPDX-License-Identifier: GPL-3.0-only
//! Start/stop sweeps limited to our own sessions and PIDs (spec 30 §9.2).

use crate::SessionError;
use crate::discover::{WtsSession, is_our_client_name};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnedSessionRecord {
    pub seat_id: String,
    pub client_name: String,
    pub user_sid: String,
    pub session_id: Option<u32>,
    pub helper_pid: u32,
    pub helper_creation_time: u64,
}

/// `%ProgramData%\PulsarSplit\run\sessions.json`, written atomically.
pub trait RunState: Send + Sync {
    fn load(&self) -> Vec<OwnedSessionRecord>;
    fn save(&self, recs: &[OwnedSessionRecord]) -> Result<(), SessionError>;
}

/// File-backed run state (std I/O; temp file + rename).
///
/// The SYSTEM service kills processes listed here, so on Windows the file is trusted only
/// when nobody but SYSTEM / Administrators can have written it: `run\` is created with the
/// protected DACL [`crate::acl::PRIVATE_DIR_SDDL`], and the directory and the open file are
/// checked before every load ([`crate::acl::untrusted_reason`]). An untrusted file is
/// ignored (nothing is swept); an untrusted directory makes `save` fail.
pub struct FileRunState {
    path: PathBuf,
    check_acl: bool,
}

impl FileRunState {
    /// `data_dir` = `%ProgramData%\PulsarSplit`. ACL-checked on Windows.
    pub fn new(data_dir: &Path) -> Self {
        Self { path: data_dir.join("run").join("sessions.json"), check_acl: cfg!(windows) }
    }

    /// No ACL check. Tests and tools that run as the file's owner only; never the service.
    pub fn without_acl_check(data_dir: &Path) -> Self {
        Self { check_acl: false, ..Self::new(data_dir) }
    }

    fn read_checked(&self) -> Result<Option<Vec<u8>>, SessionError> {
        let mut f = match std::fs::File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        if self.check_acl {
            if let Some(dir) = self.path.parent() {
                crate::acl::verify_dir(dir)?;
            }
            crate::acl::verify_file(&f, &self.path)?;
        }
        let mut b = Vec::new();
        std::io::Read::read_to_end(&mut f, &mut b)?;
        Ok(Some(b))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl RunState for FileRunState {
    fn load(&self) -> Vec<OwnedSessionRecord> {
        match self.read_checked() {
            Ok(Some(b)) => serde_json::from_slice(&b).unwrap_or_else(|e| {
                log::warn!("run state unreadable, ignoring: {e}");
                Vec::new()
            }),
            Ok(None) => Vec::new(),
            Err(e) => {
                log::warn!("run state ignored: {e}");
                Vec::new()
            }
        }
    }

    fn save(&self, recs: &[OwnedSessionRecord]) -> Result<(), SessionError> {
        let dir = self.path.parent().ok_or_else(|| SessionError::InvalidArgument("run state path".into()))?;
        if self.check_acl {
            crate::acl::ensure_private_dir(dir)?;
        } else {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        let data = serde_json::to_vec_pretty(recs).map_err(|e| SessionError::InvalidArgument(e.to_string()))?;
        {
            let mut f = std::fs::File::create(&tmp)?;
            std::io::Write::write_all(&mut f, &data)?;
            f.sync_all()?;
        }
        // std::fs::rename replaces the target atomically on NTFS (MoveFileExW REPLACE_EXISTING).
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

/// Live process check + termination by (pid, creation time).
pub trait ProcessTable: Send + Sync {
    /// Of the given pids, those that are alive, with their creation time.
    fn alive(&self, pids: &[u32]) -> Vec<(u32, u64)>;
    /// Ends the process only if its creation time still matches.
    fn kill(&self, pid: u32, creation_time: u64) -> Result<(), SessionError>;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SweepPlan {
    pub logoff: Vec<u32>,
    pub kill: Vec<(u32, u64)>,
}

/// Pure decision: which sessions to log off / which PIDs to end. A session is ours when its
/// user is a configured seat user and either its client name has our "PSS" form, or a
/// record names its session id with the same user SID (`live_sids`: session id -> SID of
/// its user, resolved by the caller). The second rule catches disconnected seat sessions
/// whose client name Windows has blanked (HYPOTHESIS, open question session-10). Only PIDs
/// whose (pid, creation_time) match a record. Never image-path matching, never the console,
/// never local sessions.
pub fn plan_sweep(
    live: &[WtsSession],
    seat_users: &[String],
    recs: &[OwnedSessionRecord],
    live_pids: &[(u32, u64)],
    live_sids: &[(u32, String)],
) -> SweepPlan {
    let mut plan = SweepPlan::default();
    for s in live {
        if s.is_system_or_listener() || !s.is_remote {
            continue;
        }
        if !seat_users.iter().any(|u| !u.is_empty() && u.eq_ignore_ascii_case(&s.user)) {
            continue;
        }
        let sid = live_sids.iter().find(|(id, _)| *id == s.id).map(|(_, sid)| sid.as_str());
        let recorded = recs.iter().any(|r| r.session_id == Some(s.id) && sid.is_some_and(|sid| !sid.is_empty() && sid.eq_ignore_ascii_case(&r.user_sid)));
        if !is_our_client_name(&s.client_name) && !recorded {
            continue;
        }
        if !plan.logoff.contains(&s.id) {
            plan.logoff.push(s.id);
        }
    }
    for r in recs {
        let key = (r.helper_pid, r.helper_creation_time);
        if r.helper_pid != 0 && live_pids.contains(&key) && !plan.kill.contains(&key) {
            plan.kill.push(key);
        }
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discover::{WtsState, session};

    fn rec(pid: u32, ct: u64) -> OwnedSessionRecord {
        OwnedSessionRecord {
            seat_id: "s".into(),
            client_name: "PSS00000001".into(),
            user_sid: "S-1-5-21-1".into(),
            session_id: Some(2),
            helper_pid: pid,
            helper_creation_time: ct,
        }
    }

    #[test]
    fn only_our_sessions_and_pids() {
        let mut console = session(1, "PSS00000001", "seat1", "127.0.0.1", WtsState::Active);
        console.winstation = "Console".into();
        let live = vec![
            session(0, "", "", "", WtsState::Disconnected),
            console,
            session(2, "PSS00000001", "seat1", "127.0.0.1", WtsState::Disconnected), // ours, stale
            session(3, "LAPTOP-OPS", "seat1", "10.0.0.9", WtsState::Disconnected),   // operator's own RDP, same user
            session(4, "PSS00000002", "admin", "127.0.0.1", WtsState::Disconnected), // not a seat user
            session(5, "PSS00000003", "SEAT2", "127.0.0.1", WtsState::Active),       // ours, active
            session(6, "LAPTOP-OPS", "admin", "10.0.0.9", WtsState::Active),         // foreign active
            session(65_537, "PSS00000004", "seat1", "127.0.0.1", WtsState::Listen),
        ];
        let users = vec!["seat1".to_string(), "seat2".to_string(), String::new()];
        let recs = vec![rec(100, 5), rec(200, 7), rec(300, 9)];
        // 100 alive with matching time, 200 alive but pid reused (other time), 300 dead.
        let live_pids = vec![(100, 5), (200, 8)];
        let plan = plan_sweep(&live, &users, &recs, &live_pids, &[]);
        assert_eq!(plan.logoff, vec![2, 5]);
        assert_eq!(plan.kill, vec![(100, 5)]);
    }

    #[test]
    fn foreign_disconnected_session_survives() {
        let live = vec![session(7, "DESKTOP-ABC", "seat1", "127.0.0.1", WtsState::Disconnected)];
        let plan = plan_sweep(&live, &["seat1".into()], &[], &[], &[]);
        assert!(plan.logoff.is_empty());
        assert!(plan.kill.is_empty());
    }

    #[test]
    fn no_seat_users_means_no_logoff() {
        let live = vec![session(2, "PSS00000001", "seat1", "127.0.0.1", WtsState::Disconnected)];
        assert!(plan_sweep(&live, &[], &[], &[], &[]).logoff.is_empty());
    }

    #[test]
    fn recorded_session_with_blank_client_name_is_swept_by_sid() {
        // Disconnected seat session whose client name and address Windows blanked.
        let blank = session(2, "", "seat1", "", WtsState::Disconnected);
        let other = session(3, "", "seat1", "", WtsState::Disconnected);
        let recs = vec![rec(0, 0)]; // session 2, S-1-5-21-1
        let sids = vec![(2, "s-1-5-21-1".to_string()), (3, "S-1-5-21-1".to_string())];
        let plan = plan_sweep(&[blank.clone(), other], &["seat1".into()], &recs, &[], &sids);
        assert_eq!(plan.logoff, vec![2], "only the recorded id");
        // Wrong SID or not a seat user: spared.
        assert!(plan_sweep(std::slice::from_ref(&blank), &["seat1".into()], &recs, &[], &[(2, "S-1-5-21-9".into())]).logoff.is_empty());
        assert!(plan_sweep(&[blank], &["seat2".into()], &recs, &[], &sids).logoff.is_empty());
    }

    #[test]
    fn file_run_state_round_trip() {
        let dir = std::env::temp_dir().join(format!("split-session-run-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let rs = FileRunState::without_acl_check(&dir);
        assert!(rs.load().is_empty());
        rs.save(&[rec(1, 2)]).unwrap();
        rs.save(&[rec(1, 2), rec(3, 4)]).unwrap();
        assert_eq!(rs.load(), vec![rec(1, 2), rec(3, 4)]);
        std::fs::write(rs.path(), b"{broken").unwrap();
        assert!(rs.load().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn planted_run_state_is_ignored() {
        // A file under a user-owned directory (as a standard user could plant under
        // %ProgramData%) must not make the service kill anything.
        let dir = std::env::temp_dir().join(format!("split-session-plant-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        FileRunState::without_acl_check(&dir).save(&[rec(1, 2)]).unwrap();
        let checked = FileRunState::new(&dir);
        assert!(checked.load().is_empty());
        assert!(checked.save(&[rec(3, 4)]).is_err(), "user-owned run dir is refused");
        assert_eq!(FileRunState::without_acl_check(&dir).load(), vec![rec(1, 2)]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
