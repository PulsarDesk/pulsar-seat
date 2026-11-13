// SPDX-License-Identifier: GPL-3.0-only
//! Restore journal (adapted from pulsar-split `split-termwrap`): JSON Lines in
//! `%ProgramData%\Pulsar Seat\journal\<package>.jsonl` (`session.jsonl` for the RDS rows,
//! `termwrap.jsonl` for the experimental wrapper module). `intent` is flushed before each
//! change and `done` after it. Replay walks the records in reverse and puts back
//! `previous` only where the value still equals `new` (nobody changed it since), so a
//! replay is idempotent and never overwrites a later change made by someone else.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::error::SysError;


#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum RegValue {
    Absent,
    Dword(u32),
    Sz(String),
    ExpandSz(String),
    MultiSz(Vec<String>),
    BinaryB64(String),
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
    pub kind: String,
    /// `HKLM\<key path>\<value name>`
    pub target: String,
    pub previous: RegValue,
    pub new: RegValue,
    pub status: RecordStatus,
}

/// `HKLM\<path>\<value>` for a registry value.
pub fn registry_target(path: &str, value: &str) -> String {
    format!(r"HKLM\{path}\{value}")
}

/// Inverse of [`registry_target`]: `(key path, value name)`.
pub fn split_registry_target(target: &str) -> Option<(&str, &str)> {
    let rest = target.strip_prefix(r"HKLM\")?;
    let (path, value) = rest.rsplit_once('\\')?;
    (!path.is_empty() && !value.is_empty()).then_some((path, value))
}

/// One step of a replay: put `previous` back if the value still equals `new`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayStep {
    pub target: String,
    pub previous: RegValue,
    pub new: RegValue,
}

/// Pure: replay plan, newest change first. Every `intent` counts (a crash may have cut
/// the `done`); the guard at apply time sorts out whether the change happened.
pub fn replay_plan(records: &[RestoreRecord], package: &str) -> Vec<ReplayStep> {
    let mut own: Vec<&RestoreRecord> =
        records.iter().filter(|r| r.package == package && r.status == RecordStatus::Intent).collect();
    own.sort_by_key(|r| r.seq);
    own.into_iter()
        .rev()
        .map(|r| ReplayStep { target: r.target.clone(), previous: r.previous.clone(), new: r.new.clone() })
        .collect()
}

/// What to do with one replay step given the value found now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepAction {
    Restore,
    AlreadyRestored,
    /// Someone changed the value after us: leave it.
    Conflict,
}

pub fn step_action(step: &ReplayStep, current: &RegValue) -> StepAction {
    if *current == step.new && step.new != step.previous {
        StepAction::Restore
    } else if *current == step.previous {
        StepAction::AlreadyRestored
    } else {
        StepAction::Conflict
    }
}

/// Wall-clock time source for journal timestamps (Unix milliseconds).
pub trait WallClock: Send + Sync {
    fn now_ms(&self) -> u64;
}

/// Real wall clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemWallClock;

impl WallClock for SystemWallClock {
    fn now_ms(&self) -> u64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
    }
}

/// In-memory journal (tests and the service's tests).
#[derive(Default)]
pub struct MemoryRestoreLog {
    pub records: Mutex<Vec<RestoreRecord>>,
}

impl RestoreLog for MemoryRestoreLog {
    fn append(&self, rec: &RestoreRecord) -> Result<(), SysError> {
        self.records.lock().unwrap_or_else(|p| p.into_inner()).push(rec.clone());
        Ok(())
    }
    fn read_all(&self) -> Result<Vec<RestoreRecord>, SysError> {
        Ok(self.records.lock().unwrap_or_else(|p| p.into_inner()).clone())
    }
    fn clear(&self) -> Result<(), SysError> {
        self.records.lock().unwrap_or_else(|p| p.into_inner()).clear();
        Ok(())
    }
}

/// Append-only journal of one package.
pub trait RestoreLog: Send + Sync {
    fn append(&self, rec: &RestoreRecord) -> Result<(), SysError>;
    /// All readable records in file order; a torn last line is skipped.
    fn read_all(&self) -> Result<Vec<RestoreRecord>, SysError>;
    /// Called after a complete, successful replay.
    fn clear(&self) -> Result<(), SysError>;
}

/// JSON Lines file. Each append is opened write-through (Windows) and `sync_data`-ed.
///
/// The service replays this file as SYSTEM, so on Windows it is trusted only when nobody
/// but SYSTEM / Administrators can have written it (`%ProgramData%` lets Users create
/// files and folders): the journal directory is created with the protected DACL
/// [`crate::acl::JOURNAL_DIR_SDDL`], and before every read and append the owner and DACL of
/// the directory and of the open file handle are checked
/// ([`crate::acl::journal_untrusted_reason`]). Anything else fails closed.
pub struct JsonlRestoreLog {
    path: PathBuf,
    lock: Mutex<()>,
    check_acl: bool,
}

impl JsonlRestoreLog {
    /// Journal with the ACL check (on Windows).
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into(), lock: Mutex::new(()), check_acl: cfg!(windows) }
    }

    /// Journal without any ACL check. Only for tests and tools that run as the file's
    /// owner; never for the service.
    pub fn without_acl_check(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into(), lock: Mutex::new(()), check_acl: false }
    }

    /// `<data_root>\journal\<package>.jsonl`, data root normally `%ProgramData%\Pulsar Seat`.
    pub fn in_data_root(data_root: &Path, package: &str) -> Self {
        Self::new(data_root.join(seat_core::consts::JOURNAL_DIR).join(format!("{package}.jsonl")))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Creates the journal directory (protected DACL on Windows) and verifies it, its
    /// parent (the data root) included: neither may be a reparse point and the data root
    /// must be owned by SYSTEM / Administrators (a standard user may pre-create folders in
    /// `%ProgramData%`; such a folder is refused, fail closed).
    fn ensure_dir(&self) -> Result<(), SysError> {
        let Some(dir) = self.path.parent().filter(|d| !d.as_os_str().is_empty()) else {
            return Ok(());
        };
        if !self.check_acl {
            return fs::create_dir_all(dir).map_err(|e| io_err("create journal dir", e));
        }
        if let Some(parent) = dir.parent().filter(|d| !d.as_os_str().is_empty()) {
            fs::create_dir_all(parent).map_err(|e| io_err("create data root", e))?;
            secure::verify_root(parent)?;
        }
        secure::create_dir(dir)?;
        secure::verify_dir(dir)
    }

    /// Re-checks the tree before a read or delete (no creation).
    fn verify_tree(&self) -> Result<(), SysError> {
        if !self.check_acl {
            return Ok(());
        }
        let Some(dir) = self.path.parent().filter(|d| !d.as_os_str().is_empty()) else {
            return Ok(());
        };
        if let Some(parent) = dir.parent().filter(|d| !d.as_os_str().is_empty()) {
            secure::verify_root(parent)?;
        }
        secure::verify_dir(dir)
    }

    fn verify_open(&self, f: &fs::File) -> Result<(), SysError> {
        if !self.check_acl {
            return Ok(());
        }
        self.verify_tree()?;
        secure::verify_file(f, &self.path)
    }
}

fn io_err(what: &str, e: std::io::Error) -> SysError {
    SysError::Journal(format!("{what}: {e}"))
}

#[cfg(windows)]
mod secure {
    use std::path::Path;

    use crate::acl::{JOURNAL_DIR_SDDL, journal_untrusted_reason, owner_untrusted_reason, same_final_path};
    use crate::error::SysError;
    use crate::win::files::{create_dir_with_sddl, final_path_of, is_reparse_point, open_trusted_append, security_of_handle, security_of_path};

    fn refuse(what: &Path, why: String) -> SysError {
        SysError::Journal(format!("{} is not trusted (fail closed): {why}", what.display()))
    }

    fn no_reparse(p: &Path) -> Result<(), SysError> {
        match is_reparse_point(p) {
            Ok(false) => Ok(()),
            Ok(true) => Err(refuse(p, "reparse point (junction / symlink)".into())),
            Err(e) => Err(refuse(p, e)),
        }
    }

    pub fn create_dir(dir: &Path) -> Result<(), SysError> {
        create_dir_with_sddl(dir, JOURNAL_DIR_SDDL).map_err(|e| SysError::Journal(format!("create journal dir: {e}")))
    }

    /// Data root: not a reparse point, owned by SYSTEM / Administrators.
    pub fn verify_root(root: &Path) -> Result<(), SysError> {
        no_reparse(root)?;
        let info = security_of_path(root).map_err(|e| refuse(root, e))?;
        owner_untrusted_reason(&info).map_or(Ok(()), |why| Err(refuse(root, why)))
    }

    pub fn verify_dir(dir: &Path) -> Result<(), SysError> {
        no_reparse(dir)?;
        let info = security_of_path(dir).map_err(|e| refuse(dir, e))?;
        journal_untrusted_reason(&info).map_or(Ok(()), |why| Err(refuse(dir, why)))
    }

    /// The opened handle is the expected file (no redirection) with the journal DACL.
    pub fn verify_file(f: &std::fs::File, path: &Path) -> Result<(), SysError> {
        let fin = final_path_of(f).map_err(|e| refuse(path, e))?;
        let expected = std::path::absolute(path).map_err(|e| refuse(path, e.to_string()))?;
        if !same_final_path(&fin, &expected.to_string_lossy()) {
            return Err(refuse(path, format!("opened {fin}")));
        }
        let info = security_of_handle(f).map_err(|e| refuse(path, e))?;
        journal_untrusted_reason(&info).map_or(Ok(()), |why| Err(refuse(path, why)))
    }

    pub fn open_append(path: &Path) -> Result<std::fs::File, SysError> {
        open_trusted_append(path).map_err(|e| refuse(path, e))
    }
}

#[cfg(not(windows))]
mod secure {
    use std::path::Path;

    use crate::error::SysError;

    pub fn create_dir(dir: &Path) -> Result<(), SysError> {
        std::fs::create_dir_all(dir).map_err(|e| SysError::Journal(format!("create journal dir: {e}")))
    }
    pub fn verify_root(_root: &Path) -> Result<(), SysError> {
        Ok(())
    }
    pub fn verify_dir(_dir: &Path) -> Result<(), SysError> {
        Ok(())
    }
    pub fn verify_file(_f: &std::fs::File, _path: &Path) -> Result<(), SysError> {
        Ok(())
    }
    pub fn open_append(path: &Path) -> Result<std::fs::File, SysError> {
        std::fs::OpenOptions::new().create(true).append(true).read(true).open(path).map_err(|e| SysError::Journal(format!("open journal: {e}")))
    }
}

impl RestoreLog for JsonlRestoreLog {
    fn append(&self, rec: &RestoreRecord) -> Result<(), SysError> {
        let _g = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        self.ensure_dir()?;
        let mut line = serde_json::to_string(rec).map_err(|e| SysError::Journal(e.to_string()))?;
        line.push('\n');
        let mut f = if self.check_acl {
            secure::open_append(&self.path)?
        } else {
            OpenOptions::new().create(true).append(true).read(true).open(&self.path).map_err(|e| io_err("open journal", e))?
        };
        self.verify_open(&f)?;
        f.write_all(line.as_bytes()).map_err(|e| io_err("write journal", e))?;
        f.sync_data().map_err(|e| io_err("flush journal", e))
    }

    fn read_all(&self) -> Result<Vec<RestoreRecord>, SysError> {
        let _g = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let mut f = match fs::File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io_err("read journal", e)),
        };
        self.verify_open(&f)?;
        let mut text = String::new();
        f.read_to_string(&mut text).map_err(|e| io_err("read journal", e))?;
        Ok(text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| match serde_json::from_str(l) {
                Ok(r) => Some(r),
                Err(e) => {
                    log::warn!("journal: skipping unreadable line: {e}");
                    None
                }
            })
            .collect())
    }

    fn clear(&self) -> Result<(), SysError> {
        let _g = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        // A junction planted in the tree would turn this into a delete elsewhere.
        match self.verify_tree() {
            Ok(()) => {}
            Err(_) if !self.path.exists() => return Ok(()),
            Err(e) => return Err(e),
        }
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(io_err("clear journal", e)),
        }
    }
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding (for `binary_b64` journal values).
pub fn b64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(B64[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

pub fn b64_decode(s: &str) -> Option<Vec<u8>> {
    let s = s.trim().trim_end_matches('=');
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        let v = B64.iter().position(|&x| x == c)? as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// RFC 3339 UTC timestamp (`2026-11-12T10:00:00Z`) from Unix milliseconds.
pub fn rfc3339_utc(ms: u64) -> String {
    let secs = ms / 1000;
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, (rem / 60) % 60, rem % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PACKAGE: &str = "test";
    const JOURNAL_FILE: &str = "test.jsonl";

    fn rec(seq: u64, target: &str, prev: RegValue, new: RegValue, status: RecordStatus) -> RestoreRecord {
        RestoreRecord {
            v: 1,
            seq,
            time: rfc3339_utc(0),
            actor: "service".into(),
            package: PACKAGE.into(),
            kind: "registry".into(),
            target: target.into(),
            previous: prev,
            new,
            status,
        }
    }

    #[test]
    fn record_shape_matches_contract() {
        let r = rec(12, r"HKLM\A\B", RegValue::Dword(1), RegValue::Dword(0), RecordStatus::Intent);
        let j = serde_json::to_string(&r).unwrap();
        assert!(j.contains(r#""previous":{"type":"dword","value":1}"#), "{j}");
        assert!(j.contains(r#""status":"intent""#));
        let a = serde_json::to_string(&RegValue::Absent).unwrap();
        assert_eq!(a, r#"{"type":"absent"}"#);
        let x = serde_json::to_string(&RegValue::ExpandSz("%x%".into())).unwrap();
        assert_eq!(x, r#"{"type":"expand_sz","value":"%x%"}"#);
    }

    #[test]
    fn targets_roundtrip() {
        let t = registry_target(r"SYSTEM\CurrentControlSet\Services\TermService\Parameters", "ServiceDll");
        assert_eq!(t, r"HKLM\SYSTEM\CurrentControlSet\Services\TermService\Parameters\ServiceDll");
        assert_eq!(
            split_registry_target(&t),
            Some((r"SYSTEM\CurrentControlSet\Services\TermService\Parameters", "ServiceDll"))
        );
        assert_eq!(split_registry_target(r"HKCU\A\B"), None);
        assert_eq!(split_registry_target(r"HKLM\A"), None);
    }

    #[test]
    fn plan_is_reverse_intents_only() {
        let recs = vec![
            rec(1, "HKLM\\A\\x", RegValue::Absent, RegValue::Dword(1), RecordStatus::Intent),
            rec(2, "HKLM\\A\\x", RegValue::Absent, RegValue::Dword(1), RecordStatus::Done),
            rec(3, "HKLM\\A\\y", RegValue::Dword(0), RegValue::Dword(1), RecordStatus::Intent),
        ];
        let p = replay_plan(&recs, PACKAGE);
        assert!(replay_plan(&recs, "other").is_empty());
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].target, "HKLM\\A\\y");
        assert_eq!(p[1].target, "HKLM\\A\\x");
    }

    #[test]
    fn step_guard() {
        let s = ReplayStep { target: "t".into(), previous: RegValue::Dword(0), new: RegValue::Dword(1) };
        assert_eq!(step_action(&s, &RegValue::Dword(1)), StepAction::Restore);
        assert_eq!(step_action(&s, &RegValue::Dword(0)), StepAction::AlreadyRestored);
        assert_eq!(step_action(&s, &RegValue::Dword(7)), StepAction::Conflict);
    }

    #[test]
    fn jsonl_file_roundtrip_and_torn_line() {
        let dir = tempfile::tempdir().unwrap();
        let log = JsonlRestoreLog::without_acl_check(dir.path().join("journal").join(JOURNAL_FILE));
        assert!(log.read_all().unwrap().is_empty());
        let r1 = rec(1, "HKLM\\A\\x", RegValue::Absent, RegValue::Dword(1), RecordStatus::Intent);
        log.append(&r1).unwrap();
        // simulate a crash mid-write
        let mut f = OpenOptions::new().append(true).open(log.path()).unwrap();
        f.write_all(b"{\"v\":1,\"seq\":2,\"tim").unwrap();
        drop(f);
        assert_eq!(log.read_all().unwrap(), vec![r1]);
        log.clear().unwrap();
        assert!(log.read_all().unwrap().is_empty());
        log.clear().unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn user_writable_journal_is_refused() {
        // A journal a non-admin could have written (here: owned by this non-SYSTEM test
        // process, DACL inherited from the user's temp dir) must never be replayed.
        let dir = tempfile::tempdir().unwrap();
        let open = JsonlRestoreLog::without_acl_check(dir.path().join("journal").join(JOURNAL_FILE));
        let r1 = rec(1, r"HKLM\A\x", RegValue::Absent, RegValue::Dword(1), RecordStatus::Intent);
        open.append(&r1).unwrap();
        let checked = JsonlRestoreLog::in_data_root(dir.path(), PACKAGE);
        let e = checked.read_all().unwrap_err();
        assert_eq!(e.code(), "journal");
        assert!(e.to_string().contains("not trusted"), "{e}");
        assert!(checked.append(&r1).is_err());
        // missing file = empty journal, no error
        let none = JsonlRestoreLog::in_data_root(&dir.path().join("nothing"), PACKAGE);
        assert!(none.read_all().unwrap().is_empty());
    }

    #[test]
    fn base64() {
        for (raw, enc) in [(&b""[..], ""), (b"f", "Zg=="), (b"fo", "Zm8="), (b"foo", "Zm9v"), (b"foobar", "Zm9vYmFy")] {
            assert_eq!(b64_encode(raw), enc);
            assert_eq!(b64_decode(enc).unwrap(), raw);
        }
        assert_eq!(b64_decode("Zm9v!"), None);
    }

    #[test]
    fn timestamps() {
        assert_eq!(rfc3339_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339_utc(1_794_477_600_000), "2026-11-12T10:00:00Z");
        assert_eq!(rfc3339_utc(951_782_400_000), "2000-02-29T00:00:00Z");
    }
}
