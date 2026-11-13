// SPDX-License-Identifier: GPL-3.0-only
//! Restore records (spec 90 §9.2, contracts.md §0.3): JSON Lines in
//! `%ProgramData%\PulsarSplit\journal\shim.jsonl`. `intent` is written (flushed,
//! write-through) before a change, `done` after it. [`restore_plan`] replays only this
//! package's records, newest first, and restores each target to the oldest `previous`.

use crate::error::ShimError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Mutex;

pub const PACKAGE: &str = "shim";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordKind {
    Registry,
    File,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordStatus {
    Intent,
    Done,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RestoreRecord {
    pub v: u32,
    pub seq: u64,
    pub time: String,
    pub actor: String,
    pub package: String,
    pub kind: RecordKind,
    pub target: String,
    /// `{"type":"absent"}`, a registry value (`{"type":"dword","value":1}` …) or, for files,
    /// `{"type":"backup","value":"<path of the saved original>"}`.
    pub previous: Value,
    pub new: Value,
    pub status: RecordStatus,
}

pub fn absent() -> Value {
    serde_json::json!({"type": "absent"})
}
pub fn backup(path: &str) -> Value {
    serde_json::json!({"type": "backup", "value": path})
}
pub fn file_sha256(hex: &str) -> Value {
    serde_json::json!({"type": "file", "sha256": hex})
}

/// `YYYY-MM-DDTHH:MM:SSZ` for a Unix time (civil-from-days, proleptic Gregorian).
pub fn rfc3339_utc(unix_secs: u64) -> String {
    let days = (unix_secs / 86_400) as i64;
    let rem = unix_secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// Current time for journal records.
pub fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    rfc3339_utc(secs)
}

pub trait RestoreLog: Send + Sync {
    fn append(&self, r: &RestoreRecord) -> Result<(), ShimError>;
    fn read_all(&self) -> Result<Vec<RestoreRecord>, ShimError>;
}

/// Writes `intent`, runs `change`, writes `done`. Returns the change's result.
pub struct Journal<'a> {
    log: &'a dyn RestoreLog,
    actor: &'a str,
    clock: &'a dyn Fn() -> String,
    seq: Mutex<u64>,
}

impl<'a> Journal<'a> {
    pub fn new(log: &'a dyn RestoreLog, actor: &'a str, clock: &'a dyn Fn() -> String) -> Result<Self, ShimError> {
        let next = log.read_all()?.iter().map(|r| r.seq).max().map_or(1, |m| m + 1);
        Ok(Self { log, actor, clock, seq: Mutex::new(next) })
    }

    pub fn record<T>(
        &self,
        kind: RecordKind,
        target: &str,
        previous: Value,
        new: Value,
        change: impl FnOnce() -> Result<T, ShimError>,
    ) -> Result<T, ShimError> {
        let seq = {
            let mut s = self.seq.lock().unwrap_or_else(|e| e.into_inner());
            let v = *s;
            *s += 1;
            v
        };
        let mut rec = RestoreRecord {
            v: 1,
            seq,
            time: (self.clock)(),
            actor: self.actor.to_string(),
            package: PACKAGE.to_string(),
            kind,
            target: target.to_string(),
            previous,
            new,
            status: RecordStatus::Intent,
        };
        self.log.append(&rec)?;
        let out = change()?;
        rec.status = RecordStatus::Done;
        rec.time = (self.clock)();
        self.log.append(&rec)?;
        Ok(out)
    }
}

/// One restore step: put `target` back to `previous`.
#[derive(Debug, Clone, PartialEq)]
pub struct RestoreStep {
    pub kind: RecordKind,
    pub target: String,
    pub previous: Value,
}

/// Newest change first; each target appears once with its oldest `previous`.
/// `intent` without `done` is included (the change may have half-happened).
pub fn restore_plan(records: &[RestoreRecord]) -> Vec<RestoreStep> {
    let mut intents: Vec<&RestoreRecord> =
        records.iter().filter(|r| r.package == PACKAGE && r.status == RecordStatus::Intent).collect();
    intents.sort_by_key(|r| r.seq);
    // Per target (case-insensitive): the oldest intent gives `previous`, the newest gives
    // the position in the replay order.
    let mut per_target: Vec<(u64, RestoreStep)> = Vec::new();
    for r in intents {
        let same = |s: &RestoreStep| s.kind == r.kind && s.target.eq_ignore_ascii_case(&r.target);
        match per_target.iter_mut().find(|(_, s)| same(s)) {
            Some(entry) => entry.0 = r.seq,
            None => per_target
                .push((r.seq, RestoreStep { kind: r.kind, target: r.target.clone(), previous: r.previous.clone() })),
        }
    }
    per_target.sort_by_key(|e| std::cmp::Reverse(e.0));
    per_target.into_iter().map(|(_, s)| s).collect()
}

/// Applies restore steps (real impl in the service / `win`; tests use a recorder).
pub trait RestoreApplier {
    fn apply(&self, step: &RestoreStep) -> Result<(), ShimError>;
}

/// `restore_all` for this package: replays [`restore_plan`]; continues past failures and
/// returns the first error after trying every step.
pub fn restore_all(log: &dyn RestoreLog, applier: &dyn RestoreApplier) -> Result<usize, ShimError> {
    let plan = restore_plan(&log.read_all()?);
    let mut first_err = None;
    let mut ok = 0;
    for s in &plan {
        match applier.apply(s) {
            Ok(()) => ok += 1,
            Err(e) => {
                log::warn!("restore of {} failed: {e}", s.target);
                first_err.get_or_insert(e);
            }
        }
    }
    match first_err {
        Some(e) => Err(e),
        None => Ok(ok),
    }
}

/// JSON Lines file, one record per line, flushed with `sync_data` after each append.
pub struct JsonlFileLog {
    pub path: std::path::PathBuf,
}

impl RestoreLog for JsonlFileLog {
    fn append(&self, r: &RestoreRecord) -> Result<(), ShimError> {
        use std::io::Write;
        if let Some(p) = self.path.parent() {
            std::fs::create_dir_all(p)?;
        }
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&self.path)?;
        let mut line = serde_json::to_vec(r).map_err(|e| ShimError::Decode(e.to_string()))?;
        line.push(b'\n');
        f.write_all(&line)?;
        f.sync_data()?;
        Ok(())
    }

    fn read_all(&self) -> Result<Vec<RestoreRecord>, ShimError> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        // A torn last line (crash during append) is skipped.
        Ok(text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::MemoryLog;
    use std::cell::RefCell;

    fn clock() -> String {
        "2026-11-12T10:00:00Z".into()
    }

    #[test]
    fn rfc3339() {
        assert_eq!(rfc3339_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339_utc(1_794_477_600), "2026-11-12T10:00:00Z");
    }

    #[test]
    fn intent_then_done() {
        let log = MemoryLog::default();
        let c = clock;
        let j = Journal::new(&log, "service", &c).unwrap();
        j.record(RecordKind::File, r"C:\g\d3d9.dll", absent(), file_sha256("ab"), || Ok(())).unwrap();
        let r = log.read_all().unwrap();
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].status, RecordStatus::Intent);
        assert_eq!(r[1].status, RecordStatus::Done);
        assert_eq!(r[0].seq, r[1].seq);
        // Failing change leaves only the intent.
        let e = j.record(RecordKind::File, "x", absent(), absent(), || -> Result<(), ShimError> {
            Err(ShimError::System("boom".into()))
        });
        assert!(e.is_err());
        let r = log.read_all().unwrap();
        assert_eq!(r.len(), 3);
        assert_eq!(r[2].seq, 2);
        // A new journal continues the sequence.
        let j2 = Journal::new(&log, "service", &c).unwrap();
        j2.record(RecordKind::Registry, "k", absent(), absent(), || Ok(())).unwrap();
        assert_eq!(log.read_all().unwrap().last().unwrap().seq, 3);
    }

    #[test]
    fn plan_reverse_oldest_previous() {
        let log = MemoryLog::default();
        let c = clock;
        let j = Journal::new(&log, "service", &c).unwrap();
        j.record(RecordKind::File, "A", backup("A.orig"), absent(), || Ok(())).unwrap();
        j.record(RecordKind::File, "B", absent(), absent(), || Ok(())).unwrap();
        j.record(RecordKind::File, "a", absent(), absent(), || Ok(())).unwrap(); // same target, newer
        let dword = serde_json::json!({"type": "dword", "value": 1});
        let _ = j.record(RecordKind::Registry, "R", dword, absent(), || -> Result<(), ShimError> {
            Err(ShimError::System("x".into()))
        });
        let plan = restore_plan(&log.read_all().unwrap());
        let names: Vec<&str> = plan.iter().map(|s| s.target.as_str()).collect();
        assert_eq!(names, vec!["R", "A", "B"]);
        assert_eq!(plan[1].previous, backup("A.orig"));
    }

    struct Rec(RefCell<Vec<String>>);
    impl RestoreApplier for Rec {
        fn apply(&self, s: &RestoreStep) -> Result<(), ShimError> {
            self.0.borrow_mut().push(s.target.clone());
            if s.target == "bad" { Err(ShimError::System("no".into())) } else { Ok(()) }
        }
    }

    #[test]
    fn restore_all_tries_everything() {
        let log = MemoryLog::default();
        let c = clock;
        let j = Journal::new(&log, "service", &c).unwrap();
        for t in ["one", "bad", "two"] {
            j.record(RecordKind::File, t, absent(), absent(), || Ok(())).unwrap();
        }
        let a = Rec(RefCell::new(vec![]));
        assert!(restore_all(&log, &a).is_err());
        assert_eq!(*a.0.borrow(), vec!["two", "bad", "one"]);
    }

    #[test]
    fn jsonl_file_round_trip_and_torn_line() {
        let dir = std::env::temp_dir().join(format!("split-shim-journal-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let log = JsonlFileLog { path: dir.join("shim.jsonl") };
        assert!(log.read_all().unwrap().is_empty());
        let c = clock;
        let j = Journal::new(&log, "service", &c).unwrap();
        j.record(RecordKind::File, "x", absent(), absent(), || Ok(())).unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(&log.path)
            .and_then(|mut f| std::io::Write::write_all(&mut f, b"{\"v\":1,\"se"))
            .unwrap();
        assert_eq!(log.read_all().unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
