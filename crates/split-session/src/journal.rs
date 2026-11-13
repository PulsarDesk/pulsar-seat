// SPDX-License-Identifier: GPL-3.0-only
//! Restore journal (spec 90 §9.2, contracts §0.3): JSON Lines in
//! `%ProgramData%\PulsarSplit\journal\session.jsonl`, `intent` before a change and `done`
//! after, so `seat-setup` can replay it on uninstall.

use crate::SessionError;
use crate::rds::RegData;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

pub const PACKAGE: &str = "session";

/// `previous` / `new` in a record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum JournalValue {
    Absent,
    Dword(u32),
    Sz(String),
    ExpandSz(String),
    MultiSz(Vec<String>),
    BinaryB64(String),
}

impl From<Option<&RegData>> for JournalValue {
    fn from(d: Option<&RegData>) -> Self {
        match d {
            None => Self::Absent,
            Some(RegData::Dword(v)) => Self::Dword(*v),
            Some(RegData::Sz(s)) => Self::Sz(s.clone()),
            Some(RegData::ExpandSz(s)) => Self::ExpandSz(s.clone()),
            Some(RegData::MultiSz(v)) => Self::MultiSz(v.clone()),
        }
    }
}

impl JournalValue {
    /// Back to registry data; `Ok(None)` = absent. Binary is never written by this package.
    pub fn to_reg(&self) -> Result<Option<RegData>, SessionError> {
        Ok(match self {
            Self::Absent => None,
            Self::Dword(v) => Some(RegData::Dword(*v)),
            Self::Sz(s) => Some(RegData::Sz(s.clone())),
            Self::ExpandSz(s) => Some(RegData::ExpandSz(s.clone())),
            Self::MultiSz(v) => Some(RegData::MultiSz(v.clone())),
            Self::BinaryB64(_) => return Err(SessionError::InvalidArgument("binary journal value".into())),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordKind {
    Registry,
    ServiceConfig,
    File,
    Firewall,
    Driver,
    Account,
    DeviceSecurity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordStatus {
    Intent,
    Done,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreRecord {
    pub v: u32,
    pub seq: u64,
    pub time: String,
    pub actor: String,
    pub package: String,
    pub kind: RecordKind,
    /// For registry: `HKLM\<key>\<value>`; for accounts: the account name.
    pub target: String,
    pub previous: JournalValue,
    pub new: JournalValue,
    pub status: RecordStatus,
}

impl RestoreRecord {
    pub fn new(kind: RecordKind, target: String, previous: JournalValue, new: JournalValue, status: RecordStatus) -> Self {
        Self {
            v: 1,
            seq: 0,
            time: String::new(),
            actor: "service".into(),
            package: PACKAGE.into(),
            kind,
            target,
            previous,
            new,
            status,
        }
    }
}

/// Where restore records go. `append` fills `seq` and `time` and must be write-through.
pub trait RestoreLog: Send + Sync {
    fn append(&self, rec: RestoreRecord) -> Result<(), SessionError>;
    fn records(&self) -> Result<Vec<RestoreRecord>, SessionError>;
}

impl<T: RestoreLog + ?Sized> RestoreLog for std::sync::Arc<T> {
    fn append(&self, rec: RestoreRecord) -> Result<(), SessionError> {
        (**self).append(rec)
    }
    fn records(&self) -> Result<Vec<RestoreRecord>, SessionError> {
        (**self).records()
    }
}

/// Real journal file (platform-neutral std I/O; the path is under `%ProgramData%`).
///
/// The service and `seat-setup` replay it as SYSTEM / admin, so on Windows it is trusted
/// only when nobody but SYSTEM / Administrators can have written it: `journal\` is created
/// with [`crate::acl::PRIVATE_DIR_SDDL`] and the directory and the open file are checked
/// before every read and append. Anything else fails closed.
pub struct JsonlRestoreLog {
    path: PathBuf,
    lock: Mutex<()>,
    check_acl: bool,
}

impl JsonlRestoreLog {
    /// `data_dir` = `%ProgramData%\PulsarSplit`. ACL-checked on Windows.
    pub fn new(data_dir: &std::path::Path) -> Self {
        Self { path: data_dir.join("journal").join("session.jsonl"), lock: Mutex::new(()), check_acl: cfg!(windows) }
    }

    /// No ACL check. Tests and tools that run as the file's owner only; never the service.
    pub fn without_acl_check(data_dir: &std::path::Path) -> Self {
        Self { check_acl: false, ..Self::new(data_dir) }
    }

    fn verify(&self, f: &std::fs::File) -> Result<(), SessionError> {
        if !self.check_acl {
            return Ok(());
        }
        if let Some(dir) = self.path.parent() {
            crate::acl::verify_dir(dir)?;
        }
        crate::acl::verify_file(f, &self.path)
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl RestoreLog for JsonlRestoreLog {
    fn append(&self, mut rec: RestoreRecord) -> Result<(), SessionError> {
        let _g = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(dir) = self.path.parent() {
            if self.check_acl {
                crate::acl::ensure_private_dir(dir)?;
            } else {
                std::fs::create_dir_all(dir)?;
            }
        }
        rec.seq = self.records_unlocked()?.last().map_or(1, |r| r.seq + 1);
        rec.time = rfc3339_utc(std::time::SystemTime::now());
        let mut line = serde_json::to_string(&rec).map_err(|e| SessionError::InvalidArgument(e.to_string()))?;
        line.push('\n');
        // read: READ_CONTROL for the handle check.
        let mut f = std::fs::OpenOptions::new().create(true).append(true).read(true).open(&self.path)?;
        self.verify(&f)?;
        // After a torn write the file may not end in a newline; start a fresh line.
        let len = f.metadata()?.len();
        if len > 0 {
            let mut last = [0u8; 1];
            let mut r = std::fs::File::open(&self.path)?;
            std::io::Seek::seek(&mut r, std::io::SeekFrom::Start(len - 1))?;
            std::io::Read::read_exact(&mut r, &mut last)?;
            if last[0] != b'\n' {
                line.insert(0, '\n');
            }
        }
        f.write_all(line.as_bytes())?;
        f.sync_all()?;
        Ok(())
    }

    fn records(&self) -> Result<Vec<RestoreRecord>, SessionError> {
        let _g = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        self.records_unlocked()
    }
}

impl JsonlRestoreLog {
    fn records_unlocked(&self) -> Result<Vec<RestoreRecord>, SessionError> {
        let mut f = match std::fs::File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        self.verify(&f)?;
        let mut text = String::new();
        std::io::Read::read_to_string(&mut f, &mut text)?;
        // A torn last line (crash mid-write) is skipped, not fatal.
        Ok(text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect())
    }
}

/// `YYYY-MM-DDTHH:MM:SSZ` without a date crate.
pub fn rfc3339_utc(t: std::time::SystemTime) -> String {
    let secs = t.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Civil-from-days (Howard Hinnant's public-domain algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, (rem % 3600) / 60, rem % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn record_shape_matches_contract() {
        let r = RestoreRecord {
            seq: 12,
            time: "2026-11-12T10:00:00Z".into(),
            ..RestoreRecord::new(
                RecordKind::Registry,
                r"HKLM\SYSTEM\CurrentControlSet\Control\Terminal Server\fDenyTSConnections".into(),
                JournalValue::Dword(1),
                JournalValue::Dword(0),
                RecordStatus::Intent,
            )
        };
        let j = serde_json::to_string(&r).unwrap();
        assert_eq!(
            j,
            r#"{"v":1,"seq":12,"time":"2026-11-12T10:00:00Z","actor":"service","package":"session","kind":"registry","target":"HKLM\\SYSTEM\\CurrentControlSet\\Control\\Terminal Server\\fDenyTSConnections","previous":{"type":"dword","value":1},"new":{"type":"dword","value":0},"status":"intent"}"#
        );
        assert_eq!(serde_json::to_string(&JournalValue::Absent).unwrap(), r#"{"type":"absent"}"#);
    }

    #[test]
    fn rfc3339() {
        assert_eq!(rfc3339_utc(UNIX_EPOCH), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339_utc(UNIX_EPOCH + Duration::from_secs(1_794_477_600)), "2026-11-12T10:00:00Z");
        assert_eq!(rfc3339_utc(UNIX_EPOCH + Duration::from_secs(951_782_400)), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn jsonl_file_appends_with_sequence_and_skips_torn_lines() {
        let dir = std::env::temp_dir().join(format!("split-session-journal-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let log = JsonlRestoreLog::without_acl_check(&dir);
        let rec = || RestoreRecord::new(RecordKind::Account, "seat1".into(), JournalValue::Absent, JournalValue::Sz("created".into()), RecordStatus::Done);
        log.append(rec()).unwrap();
        log.append(rec()).unwrap();
        std::fs::OpenOptions::new().append(true).open(log.path()).unwrap().write_all(b"{\"v\":1,\"se").unwrap();
        let recs = log.records().unwrap();
        assert_eq!(recs.iter().map(|r| r.seq).collect::<Vec<_>>(), vec![1, 2]);
        log.append(rec()).unwrap();
        let recs = log.records().unwrap();
        assert_eq!(recs.iter().map(|r| r.seq).collect::<Vec<_>>(), vec![1, 2, 3]);
        #[cfg(windows)]
        {
            // The same file in a user-owned directory is refused by the checked journal.
            let checked = JsonlRestoreLog::new(&dir);
            assert!(checked.records().is_err());
            assert!(checked.append(rec()).is_err());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
