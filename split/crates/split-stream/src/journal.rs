// SPDX-License-Identifier: GPL-3.0-only
//! Restore records (contracts §0.3, spec 90 §9.2) for system changes this package makes
//! (firewall rules). JSON Lines in `%ProgramData%\PulsarSplit\journal\stream.jsonl`; an
//! `intent` record is flushed to disk before the change, a `done` record after it.
//!
//! The journal drives SYSTEM-level undo (firewall rules), so it must not be writable by
//! standard users (spec 90 §9.2): the data root and `journal\` are created through
//! [`SecureFs::ensure_dir`] with [`JOURNAL_DIR_SDDL`] (owner SYSTEM, protected DACL), and
//! an existing journal file that is a reparse point, hard-linked or foreign-owned is
//! refused. [`crate::firewall::restore_all`] still validates every record it replays.

use crate::seatdir::{SecureFs, SEAT_DIR_SDDL};
use crate::{Clock, StreamError};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub const PACKAGE: &str = "stream";

/// SYSTEM + Administrators only, owner SYSTEM (same as the seat folders).
pub const JOURNAL_DIR_SDDL: &str = SEAT_DIR_SDDL;

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

impl RestoreRecord {
    /// `seq`, `time` and `status` are filled in by the log.
    pub fn new(kind: &str, target: &str, previous: serde_json::Value, new: serde_json::Value) -> Self {
        Self {
            v: 1,
            seq: 0,
            time: String::new(),
            actor: "service".into(),
            package: PACKAGE.into(),
            kind: kind.into(),
            target: target.into(),
            previous,
            new,
            status: String::new(),
        }
    }
}

pub trait RestoreLog: Send + Sync {
    /// Append an `intent` record (durable before returning); returns its `seq`.
    fn intent(&self, r: &RestoreRecord) -> Result<u64, StreamError>;
    fn done(&self, seq: u64, r: &RestoreRecord) -> Result<(), StreamError>;
    /// One record per `seq` (the latest status), newest first.
    fn replay_reverse(&self) -> Result<Vec<RestoreRecord>, StreamError>;
}

/// Collapse a record stream to one record per seq and reverse it.
pub fn collapse_reverse(mut all: Vec<RestoreRecord>) -> Vec<RestoreRecord> {
    let mut out: Vec<RestoreRecord> = Vec::new();
    all.sort_by_key(|r| r.seq);
    for r in all {
        match out.last_mut() {
            Some(last) if last.seq == r.seq => *last = r,
            _ => out.push(r),
        }
    }
    out.reverse();
    out
}

/// `YYYY-MM-DDTHH:MM:SSZ` from Unix milliseconds.
pub fn iso8601(ms: u64) -> String {
    let secs = ms / 1000;
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
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
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, (rem / 60) % 60, rem % 60)
}

/// File-backed journal. `clock` must return Unix milliseconds.
pub struct JsonlJournal<C: Clock> {
    path: PathBuf,
    clock: C,
    lock: Mutex<u64>,
}

impl<C: Clock> JsonlJournal<C> {
    /// `path` is `<data_root>\journal\stream.jsonl`. Creates (or checks) the data root and
    /// `journal\` with [`JOURNAL_DIR_SDDL`], top down, and checks the file before reading it.
    pub fn open(fs: Arc<dyn SecureFs>, path: PathBuf, clock: C) -> Result<Self, StreamError> {
        let dir = path.parent().ok_or_else(|| StreamError::invalid("journal path", "no parent"))?;
        let root = dir.parent().ok_or_else(|| StreamError::invalid("journal path", "no data root"))?;
        fs.ensure_dir(root, JOURNAL_DIR_SDDL)?;
        fs.ensure_dir(dir, JOURNAL_DIR_SDDL)?;
        fs.check_file(&path)?;
        let last = Self::read_all(&path)?.iter().map(|r| r.seq).max().unwrap_or(0);
        // Terminate a torn last line so the next record starts on its own line.
        if let Ok(bytes) = std::fs::read(&path) {
            if bytes.last().is_some_and(|b| *b != b'\n') {
                let mut f = std::fs::OpenOptions::new()
                    .append(true)
                    .open(&path)
                    .map_err(|e| StreamError::io("open journal", e))?;
                f.write_all(b"\n").map_err(|e| StreamError::io("write journal", e))?;
            }
        }
        Ok(Self { path, clock, lock: Mutex::new(last) })
    }

    fn read_all(path: &PathBuf) -> Result<Vec<RestoreRecord>, StreamError> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(StreamError::io("read journal", e)),
        };
        // A torn last line (crash mid-write) is skipped, not fatal.
        Ok(text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect())
    }

    fn append(&self, r: &RestoreRecord) -> Result<(), StreamError> {
        // The directory was secured in `open`; nobody but SYSTEM / Administrators can have
        // created or swapped the file since.
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| StreamError::io("open journal", e))?;
        let mut line = serde_json::to_vec(r).map_err(|e| StreamError::io("encode journal", e))?;
        line.push(b'\n');
        f.write_all(&line).map_err(|e| StreamError::io("write journal", e))?;
        f.sync_data().map_err(|e| StreamError::io("flush journal", e))
    }
}

impl<C: Clock> RestoreLog for JsonlJournal<C> {
    fn intent(&self, r: &RestoreRecord) -> Result<u64, StreamError> {
        let mut seq = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        *seq += 1;
        let rec = RestoreRecord { seq: *seq, time: iso8601(self.clock.now_ms()), status: "intent".into(), ..r.clone() };
        self.append(&rec)?;
        Ok(*seq)
    }

    fn done(&self, seq: u64, r: &RestoreRecord) -> Result<(), StreamError> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let rec = RestoreRecord { seq, time: iso8601(self.clock.now_ms()), status: "done".into(), ..r.clone() };
        self.append(&rec)
    }

    fn replay_reverse(&self) -> Result<Vec<RestoreRecord>, StreamError> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        Ok(collapse_reverse(Self::read_all(&self.path)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeClock, MemFs, Planted};

    /// Directories on disk (temp dir), ACL calls recorded only.
    #[derive(Default)]
    struct DiskDirs(MemFs);

    impl SecureFs for DiskDirs {
        fn ensure_dir(&self, p: &std::path::Path, sddl: &str) -> Result<(), StreamError> {
            self.0.ensure_dir(p, sddl)?;
            std::fs::create_dir_all(p).map_err(|e| StreamError::io("mkdir", e))
        }
        fn check_file(&self, p: &std::path::Path) -> Result<(), StreamError> {
            self.0.check_file(p)
        }
        fn write_atomic(&self, p: &std::path::Path, b: &[u8]) -> Result<(), StreamError> {
            self.0.write_atomic(p, b)
        }
        fn rotate_log(&self, p: &std::path::Path, k: usize) -> Result<(), StreamError> {
            self.0.rotate_log(p, k)
        }
        fn remove_dir_all(&self, p: &std::path::Path) -> Result<(), StreamError> {
            self.0.remove_dir_all(p)
        }
        fn read(&self, p: &std::path::Path) -> Result<Option<Vec<u8>>, StreamError> {
            self.0.read(p)
        }
        fn remove_file(&self, p: &std::path::Path) -> Result<(), StreamError> {
            self.0.remove_file(p)
        }
    }

    #[test]
    fn iso_dates() {
        assert_eq!(iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601(1_794_477_600_000), "2026-11-12T10:00:00Z");
        assert_eq!(iso8601(951_782_400_000), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn jsonl_intent_done_replay_and_torn_line() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("PulsarSplit").join("journal").join("stream.jsonl");
        let fs = Arc::new(DiskDirs::default());
        let j = JsonlJournal::open(fs.clone(), p.clone(), FakeClock::new(1_794_477_600_000)).unwrap();
        assert_eq!(fs.0.dir_sddl(p.parent().unwrap()).as_deref(), Some(JOURNAL_DIR_SDDL));
        assert_eq!(fs.0.ensured(), [dir.path().join("PulsarSplit"), p.parent().unwrap().to_path_buf()]);
        let r = RestoreRecord::new("firewall", "a", serde_json::json!({"type":"absent"}), serde_json::json!({}));
        let s1 = j.intent(&r).unwrap();
        j.done(s1, &r).unwrap();
        let s2 = j.intent(&RestoreRecord { target: "b".into(), ..r.clone() }).unwrap();
        assert_eq!((s1, s2), (1, 2));
        std::fs::OpenOptions::new().append(true).open(&p).unwrap().write_all(b"{\"v\":1,\"se").unwrap();
        let j = JsonlJournal::open(fs, p.clone(), FakeClock::new(0)).unwrap();
        let back = j.replay_reverse().unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].target, "b");
        assert_eq!(back[0].status, "intent", "intent without done is still replayed");
        assert_eq!(back[1].status, "done");
        assert_eq!(back[1].time, "2026-11-12T10:00:00Z");
        assert_eq!(back[1].package, "stream");
        assert_eq!(j.intent(&r).unwrap(), 3, "seq continues after reopen");
    }

    #[test]
    fn open_refuses_planted_dir_or_file() {
        let p = PathBuf::from(r"C:\pd\journal\stream.jsonl");
        let fs = Arc::new(MemFs::default());
        fs.plant(p.parent().unwrap(), Planted::Dir { owner: "S-1-5-21-1-2-3-1001".into() });
        assert!(JsonlJournal::open(fs, p.clone(), FakeClock::new(0)).is_err());
        let fs = Arc::new(MemFs::default());
        fs.plant(&p, Planted::File { owner: "S-1-5-21-1-2-3-1001".into() });
        assert!(JsonlJournal::open(fs, p.clone(), FakeClock::new(0)).is_err());
        let fs = Arc::new(MemFs::default());
        fs.plant(&p, Planted::Reparse);
        assert!(JsonlJournal::open(fs, p, FakeClock::new(0)).is_err());
    }
}
