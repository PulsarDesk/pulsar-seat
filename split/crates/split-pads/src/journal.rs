// SPDX-License-Identifier: GPL-3.0-only
//! Restore records (contracts §0.3, spec 90 §9.2): JSON Lines in
//! `%ProgramData%\PulsarSplit\journal\pads.jsonl`, `intent` before a change, `done` after.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::error::PadError;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RestoreRecord {
    pub v: u32,
    pub seq: u64,
    pub time: String,
    pub actor: String,
    pub package: String,
    pub kind: String,
    pub target: String,
    pub previous: serde_json::Value,
    pub new: serde_json::Value,
    pub status: String,
}

pub trait RestoreLog: Send + Sync {
    fn append(&self, rec: &RestoreRecord) -> Result<(), PadError>;
    fn records(&self) -> Result<Vec<RestoreRecord>, PadError>;
    fn next_seq(&self) -> u64 {
        self.records().map(|r| r.iter().map(|x| x.seq).max().unwrap_or(0) + 1).unwrap_or(1)
    }
}

/// File-backed log, each line flushed to disk before `append` returns.
pub struct FileRestoreLog {
    path: PathBuf,
    lock: Mutex<()>,
}

impl FileRestoreLog {
    pub fn under(data_root: &std::path::Path) -> Self {
        FileRestoreLog { path: data_root.join("journal").join("pads.jsonl"), lock: Mutex::new(()) }
    }
}

impl RestoreLog for FileRestoreLog {
    fn append(&self, rec: &RestoreRecord) -> Result<(), PadError> {
        let _g = self.lock.lock().map_err(|_| PadError::Invalid("journal lock poisoned".into()))?;
        if let Some(d) = self.path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&self.path)?;
        let mut line = serde_json::to_vec(rec)?;
        line.push(b'\n');
        f.write_all(&line)?;
        f.sync_data()?;
        Ok(())
    }

    fn records(&self) -> Result<Vec<RestoreRecord>, PadError> {
        let body = match std::fs::read_to_string(&self.path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        Ok(body.lines().filter(|l| !l.trim().is_empty()).filter_map(|l| serde_json::from_str(l).ok()).collect())
    }
}

/// In-memory log.
#[derive(Default)]
pub struct VecRestoreLog(pub Mutex<Vec<RestoreRecord>>);

impl RestoreLog for VecRestoreLog {
    fn append(&self, rec: &RestoreRecord) -> Result<(), PadError> {
        self.0.lock().unwrap().push(rec.clone());
        Ok(())
    }
    fn records(&self) -> Result<Vec<RestoreRecord>, PadError> {
        Ok(self.0.lock().unwrap().clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(seq: u64, status: &str) -> RestoreRecord {
        RestoreRecord {
            v: 1,
            seq,
            time: "2026-11-12T10:00:00Z".into(),
            actor: "service".into(),
            package: "pads".into(),
            kind: "service_config".into(),
            target: "BluetoothUserService_1a2b3".into(),
            previous: serde_json::json!({"type":"dword","value":3}),
            new: serde_json::json!({"type":"dword","value":4}),
            status: status.into(),
        }
    }

    #[test]
    fn file_log_appends() {
        let d = tempfile::tempdir().unwrap();
        let l = FileRestoreLog::under(d.path());
        assert_eq!(l.next_seq(), 1);
        l.append(&rec(1, "intent")).unwrap();
        l.append(&rec(2, "done")).unwrap();
        let r = l.records().unwrap();
        assert_eq!(r.len(), 2);
        assert_eq!(r[1].status, "done");
        assert_eq!(l.next_seq(), 3);
        let j = serde_json::to_string(&r[0]).unwrap();
        assert!(j.starts_with(r#"{"v":1,"seq":1,"time":"#), "{j}");
    }
}
