// SPDX-License-Identifier: GPL-3.0-only
//! Change journal (spec 90 §9.2, contract §0.3): record shape, append-only writer, and
//! reverse replay of every `journal\*.jsonl` file on uninstall.
//!
//! Record conventions used by the generic applier (other packages may journal kinds this
//! applier does not understand; those are left to the package's own `RestoreHook`):
//!
//! | kind | target | previous / new |
//! | --- | --- | --- |
//! | `registry` | `HKLM\<key>\<value name>` | value states; `{"type":"key"}` in `new` with `previous` absent = "we created this key" (target is then the key path) |
//! | `file` | absolute path | `previous` absent = we created it (delete); `sz` = path of a backup copy to put back; `binary_b64` = small file content |
//! | `firewall` | rule name | `previous` absent = we created it (remove by name) |
//! | `driver` | `inf:<original INF name>` | `new` = `sz` published name `oemNN.inf` we added (only that one is deleted, after its original name is cross-checked); `previous` = `absent` or `multi_sz` published names of same-named packages already in the store (never deleted) |
//! | `driver` | `device-hwid:<hardware id>` / `installer:<id>` | `previous` absent; root devnode we created / upstream installer we ran (kept) |
//! | `account` | local user name | `previous` absent = we created it (deleted only with "remove user data"; otherwise deferred and kept in the live journal) |
//!
//! When an intent and its `done` differ (a change whose result is only known afterwards,
//! such as the published name of a driver package), the `done` record is authoritative.

use crate::error::SetupError;
use crate::timefmt::parse_rfc3339_ns;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const RECORD_VERSION: u32 = 1;

/// Value state of a journaled target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum ValueState {
    Absent,
    /// A registry key exists (no value): used for "we created this key".
    Key,
    Dword(u32),
    Qword(u64),
    Sz(String),
    ExpandSz(String),
    MultiSz(Vec<String>),
    BinaryB64(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordKind {
    Registry,
    ServiceConfig,
    File,
    Firewall,
    Driver,
    Account,
    DeviceSecurity,
    /// Optional Windows feature (spec 90 §2.4); not handled by the generic applier.
    Feature,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordStatus {
    Intent,
    Done,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalRecord {
    pub v: u32,
    pub seq: u64,
    pub time: String,
    pub actor: String,
    pub package: String,
    pub kind: RecordKind,
    pub target: String,
    pub previous: ValueState,
    pub new: ValueState,
    pub status: RecordStatus,
}

/// Result of parsing one journal file.
#[derive(Debug, Default)]
pub struct ParsedJournal {
    pub records: Vec<JournalRecord>,
    /// Lines that did not parse (for example a torn last line after a crash).
    pub bad_lines: usize,
}

/// Parse JSON Lines; blank lines are ignored, unparsable lines counted and skipped.
pub fn parse_journal(text: &str) -> ParsedJournal {
    let mut out = ParsedJournal::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<JournalRecord>(line) {
            Ok(r) if r.v == RECORD_VERSION => out.records.push(r),
            _ => out.bad_lines += 1,
        }
    }
    out
}

/// One change as seen by replay: the `intent` record plus whether its `done` exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub record: JournalRecord,
    pub done: bool,
    pub file: PathBuf,
}

/// Pair intents with their `done` records (same package + seq) and order the changes
/// newest first. A `done` without an `intent` is treated as a complete change; when both
/// exist, the `done` record's content wins.
///
/// Inside one package the order is strictly by `seq` (descending): wall-clock time can
/// step backwards (NTP, RTC reset), `seq` cannot. Time only interleaves packages (k-way
/// merge of the per-package lists, timestamps parsed so different RFC 3339 forms compare
/// correctly).
pub fn changes_newest_first(files: &[(PathBuf, ParsedJournal)]) -> Vec<Change> {
    let mut by_key: HashMap<(String, u64), Change> = HashMap::new();
    for (file, parsed) in files {
        for r in &parsed.records {
            let key = (r.package.clone(), r.seq);
            match by_key.get_mut(&key) {
                Some(c) => {
                    if r.status == RecordStatus::Done {
                        c.done = true;
                        c.record = r.clone();
                    }
                }
                None => {
                    by_key.insert(
                        key,
                        Change { record: r.clone(), done: r.status == RecordStatus::Done, file: file.clone() },
                    );
                }
            }
        }
    }
    let mut per_pkg: HashMap<String, Vec<Change>> = HashMap::new();
    for c in by_key.into_values() {
        per_pkg.entry(c.record.package.clone()).or_default().push(c);
    }
    // Each list sorted by seq ascending, so `pop()` yields that package's newest change.
    let mut lists: Vec<(String, Vec<Change>)> = per_pkg
        .into_iter()
        .map(|(p, mut v)| {
            v.sort_by_key(|c| c.record.seq);
            (p, v)
        })
        .collect();
    lists.sort_by(|a, b| a.0.cmp(&b.0));
    // Unparsable times sort as oldest (None < Some), then by raw text.
    let time_key = |c: &Change| (parse_rfc3339_ns(&c.record.time), c.record.time.clone());
    let mut out = Vec::new();
    loop {
        // Newest head wins; on a tie the later package name (deterministic).
        let mut best: Option<(usize, (Option<i128>, String))> = None;
        for (i, (_, v)) in lists.iter().enumerate() {
            let Some(head) = v.last() else { continue };
            let k = time_key(head);
            if best.as_ref().is_none_or(|(_, bk)| k >= *bk) {
                best = Some((i, k));
            }
        }
        let Some((b, _)) = best else { break };
        if let Some(c) = lists[b].1.pop() {
            out.push(c);
        }
    }
    out
}

/// Applies one restore. Implemented by the Windows applier and by the fakes.
pub trait RestoreApplier: Send + Sync {
    /// Put `rec.previous` back. `Err(SetupError::Unsupported)` / `Err(SetupError::Skipped)`
    /// count as "skipped", not as failures.
    fn restore(&self, rec: &JournalRecord) -> Result<(), SetupError>;
    /// Live state of the target, when the applier can read it. Used for intents without
    /// `done` ("maybe applied", spec 90 §9.2). `None` = unknown.
    fn live(&self, _rec: &JournalRecord) -> Option<ValueState> {
        None
    }
    /// True when a restore scheduled something for the next reboot (locked file, driver).
    fn reboot_pending(&self) -> bool {
        false
    }
}

#[derive(Debug, Clone, Default)]
pub struct ReplayOptions {
    /// Packages whose own `RestoreHook` already replayed their file.
    pub skip_packages: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReplayItem {
    pub package: String,
    pub seq: u64,
    pub kind: RecordKind,
    pub target: String,
    pub reason: String,
}

/// A change the applier left in place on purpose (`SetupError::Deferred`): it stays in
/// the live journal for a later run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeferredChange {
    pub file: PathBuf,
    pub record: JournalRecord,
    pub done: bool,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ReplayReport {
    pub restored: usize,
    /// Intents without `done` whose live state equals `previous` (never applied).
    pub not_applied: usize,
    pub skipped: Vec<ReplayItem>,
    /// Intent without done whose live state is neither previous nor new: left alone.
    pub conflicts: Vec<ReplayItem>,
    pub failed: Vec<ReplayItem>,
    /// Kept on purpose for a later run; [`retire_keeping`] carries them into a fresh file.
    pub deferred: Vec<DeferredChange>,
    pub bad_lines: usize,
    /// Files whose every change was restored, skipped or deferred. Retire them with
    /// [`retire_keeping`] so the deferred records survive.
    pub completed_files: Vec<PathBuf>,
    pub reboot_required: bool,
}

impl ReplayReport {
    pub fn is_clean(&self) -> bool {
        self.failed.is_empty() && self.conflicts.is_empty()
    }
}

/// Read every `*.jsonl` file in `dir` (missing dir = empty journal).
pub fn read_dir_journals(dir: &Path) -> Result<Vec<(PathBuf, ParsedJournal)>, SetupError> {
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(SetupError::io(dir, e)),
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("jsonl"))
        .collect();
    paths.sort();
    for p in paths {
        let text = std::fs::read_to_string(&p).map_err(|e| SetupError::io(&p, e))?;
        out.push((p, parse_journal(&text)));
    }
    Ok(out)
}

/// Contract entry point: replay every journal file in `dir`, newest change first.
pub fn replay_all(dir: &Path, apply: &dyn RestoreApplier) -> Result<ReplayReport, SetupError> {
    replay_dir(dir, apply, &ReplayOptions::default())
}

pub fn replay_dir(dir: &Path, apply: &dyn RestoreApplier, opts: &ReplayOptions) -> Result<ReplayReport, SetupError> {
    let files = read_dir_journals(dir)?;
    Ok(replay_parsed(&files, apply, opts))
}

/// Pure replay over parsed files (unit-tested without a file system).
pub fn replay_parsed(
    files: &[(PathBuf, ParsedJournal)],
    apply: &dyn RestoreApplier,
    opts: &ReplayOptions,
) -> ReplayReport {
    let mut rep = ReplayReport { bad_lines: files.iter().map(|(_, p)| p.bad_lines).sum(), ..Default::default() };
    let mut file_failed: BTreeSet<PathBuf> = BTreeSet::new();
    for ch in changes_newest_first(files) {
        let r = &ch.record;
        let item = |reason: String| ReplayItem {
            package: r.package.clone(),
            seq: r.seq,
            kind: r.kind,
            target: r.target.clone(),
            reason,
        };
        if opts.skip_packages.contains(&r.package) {
            continue;
        }
        if r.previous == r.new {
            continue;
        }
        if !ch.done {
            match apply.live(r) {
                Some(live) if live == r.previous => {
                    rep.not_applied += 1;
                    continue;
                }
                Some(live) if live != r.new => {
                    rep.conflicts.push(item(format!("live state {live:?} is neither previous nor new")));
                    file_failed.insert(ch.file.clone());
                    continue;
                }
                _ => {}
            }
        }
        match apply.restore(r) {
            Ok(()) => rep.restored += 1,
            Err(e @ (SetupError::Unsupported(_) | SetupError::Skipped(_))) => rep.skipped.push(item(e.to_string())),
            Err(e @ SetupError::Deferred(_)) => rep.deferred.push(DeferredChange {
                file: ch.file.clone(),
                record: r.clone(),
                done: ch.done,
                reason: e.to_string(),
            }),
            Err(e) => {
                log::warn!("restore of {}#{} failed: {e}", r.package, r.seq);
                rep.failed.push(item(format!("{}: {e}", e.code())));
                file_failed.insert(ch.file.clone());
            }
        }
    }
    rep.completed_files = files
        .iter()
        .map(|(p, _)| p.clone())
        .filter(|p| !file_failed.contains(p))
        .filter(|p| {
            // Files of skipped packages are retired by their own hook, not here.
            let pkg = files.iter().find(|(q, _)| q == p).and_then(|(_, j)| j.records.first()).map(|r| &r.package);
            pkg.is_none_or(|pk| !opts.skip_packages.contains(pk))
        })
        .collect();
    rep.reboot_required = apply.reboot_pending();
    rep
}

// ---------------------------------------------------------------- writer

/// Appends records for one package (`journal\<package>.jsonl`).
pub trait RestoreLog: Send + Sync {
    fn append(&self, rec: &JournalRecord) -> Result<(), SetupError>;
    fn next_seq(&self) -> u64;
}

/// File-backed writer: append + `sync_data` per record (the intent is on disk before the
/// change is made). Sequence numbers continue from the existing file.
pub struct FileJournal {
    path: PathBuf,
    seq: Mutex<u64>,
}

impl FileJournal {
    pub fn open(dir: &Path, package: &str) -> Result<Self, SetupError> {
        std::fs::create_dir_all(dir).map_err(|e| SetupError::io(dir, e))?;
        let path = dir.join(format!("{package}.jsonl"));
        let last = match std::fs::read_to_string(&path) {
            Ok(t) => parse_journal(&t).records.iter().map(|r| r.seq).max().unwrap_or(0),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
            Err(e) => return Err(SetupError::io(&path, e)),
        };
        Ok(Self { path, seq: Mutex::new(last) })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl RestoreLog for FileJournal {
    fn append(&self, rec: &JournalRecord) -> Result<(), SetupError> {
        let mut line = serde_json::to_string(rec).map_err(|e| SetupError::Journal(e.to_string()))?;
        line.push('\n');
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| SetupError::io(&self.path, e))?;
        f.write_all(line.as_bytes()).map_err(|e| SetupError::io(&self.path, e))?;
        f.sync_data().map_err(|e| SetupError::io(&self.path, e))
    }

    fn next_seq(&self) -> u64 {
        let mut s = self.seq.lock().unwrap_or_else(|p| p.into_inner());
        *s += 1;
        *s
    }
}

/// Journal one change: write `intent`, run `change`, write `done`.
#[allow(clippy::too_many_arguments)]
pub fn journaled<T>(
    log: &dyn RestoreLog,
    time: String,
    actor: &str,
    package: &str,
    kind: RecordKind,
    target: &str,
    previous: ValueState,
    new: ValueState,
    change: impl FnOnce() -> Result<T, SetupError>,
) -> Result<T, SetupError> {
    let mut rec = JournalRecord {
        v: RECORD_VERSION,
        seq: log.next_seq(),
        time,
        actor: actor.into(),
        package: package.into(),
        kind,
        target: target.into(),
        previous,
        new,
        status: RecordStatus::Intent,
    };
    log.append(&rec)?;
    let out = change()?;
    rec.status = RecordStatus::Done;
    log.append(&rec)?;
    Ok(out)
}

/// Like [`journaled`], for a change whose resulting state is only known afterwards: the
/// `done` record carries the `new` state returned by `change` (the intent carries
/// `new_intent`). Replay uses the `done` content when it exists.
#[allow(clippy::too_many_arguments)]
pub fn journaled_outcome<T>(
    log: &dyn RestoreLog,
    time: String,
    actor: &str,
    package: &str,
    kind: RecordKind,
    target: &str,
    previous: ValueState,
    new_intent: ValueState,
    change: impl FnOnce() -> Result<(T, ValueState), SetupError>,
) -> Result<T, SetupError> {
    let mut rec = JournalRecord {
        v: RECORD_VERSION,
        seq: log.next_seq(),
        time,
        actor: actor.into(),
        package: package.into(),
        kind,
        target: target.into(),
        previous,
        new: new_intent,
        status: RecordStatus::Intent,
    };
    log.append(&rec)?;
    let (out, new) = change()?;
    rec.status = RecordStatus::Done;
    rec.new = new;
    log.append(&rec)?;
    Ok(out)
}

/// Rename a fully replayed file to `<name>.replayed` so a second uninstall run does not
/// restore it again (idempotence). An existing `.replayed` file is replaced.
pub fn retire(path: &Path) -> Result<(), SetupError> {
    let mut to = path.as_os_str().to_owned();
    to.push(".replayed");
    std::fs::rename(path, &to).map_err(|e| SetupError::io(path, e))
}

/// Retire `path` (see [`retire`]) but carry `keep` (the deferred changes of that file)
/// over into a fresh `path`, intent + done as they were, so a later run still sees them.
/// The `.replayed` copy is written before the live file is replaced, so a crash in
/// between loses nothing (at worst a deferred record is replayed again; replays are
/// idempotent).
pub fn retire_keeping(path: &Path, keep: &[DeferredChange]) -> Result<(), SetupError> {
    let keep: Vec<&DeferredChange> = keep.iter().filter(|d| d.file == path).collect();
    if keep.is_empty() {
        return retire(path);
    }
    let mut recs = keep;
    recs.sort_by_key(|d| d.record.seq);
    let mut body = String::new();
    for d in recs {
        let mut r = d.record.clone();
        r.status = RecordStatus::Intent;
        body.push_str(&serde_json::to_string(&r).map_err(|e| SetupError::Journal(e.to_string()))?);
        body.push('\n');
        if d.done {
            r.status = RecordStatus::Done;
            body.push_str(&serde_json::to_string(&r).map_err(|e| SetupError::Journal(e.to_string()))?);
            body.push('\n');
        }
    }
    let tmp = path.with_extension("jsonl.tmp");
    {
        let mut f = std::fs::File::create(&tmp).map_err(|e| SetupError::io(&tmp, e))?;
        f.write_all(body.as_bytes()).map_err(|e| SetupError::io(&tmp, e))?;
        f.sync_all().map_err(|e| SetupError::io(&tmp, e))?;
    }
    let mut replayed = path.as_os_str().to_owned();
    replayed.push(".replayed");
    std::fs::copy(path, &replayed).map_err(|e| SetupError::io(path, e))?;
    std::fs::rename(&tmp, path).map_err(|e| SetupError::io(path, e))
}

/// Compaction (spec 90 §9.2): drop records whose change is a no-op (previous == new)
/// and duplicate `done` lines, write `<file>.tmp`, keep the old file as `<file>.bak`,
/// then move the new file into place.
pub fn compact(path: &Path) -> Result<usize, SetupError> {
    let text = std::fs::read_to_string(path).map_err(|e| SetupError::io(path, e))?;
    let parsed = parse_journal(&text);
    let mut seen = BTreeSet::new();
    let mut kept = Vec::new();
    for r in parsed.records {
        if r.previous == r.new {
            continue;
        }
        if seen.insert((r.seq, r.status == RecordStatus::Done)) {
            kept.push(r);
        }
    }
    let mut body = String::new();
    for r in &kept {
        body.push_str(&serde_json::to_string(r).map_err(|e| SetupError::Journal(e.to_string()))?);
        body.push('\n');
    }
    let tmp = path.with_extension("jsonl.tmp");
    let bak = path.with_extension("jsonl.bak");
    {
        let mut f = std::fs::File::create(&tmp).map_err(|e| SetupError::io(&tmp, e))?;
        f.write_all(body.as_bytes()).map_err(|e| SetupError::io(&tmp, e))?;
        f.sync_all().map_err(|e| SetupError::io(&tmp, e))?;
    }
    std::fs::copy(path, &bak).map_err(|e| SetupError::io(&bak, e))?;
    // std::fs::rename = MoveFileExW(MOVEFILE_REPLACE_EXISTING) on Windows.
    std::fs::rename(&tmp, path).map_err(|e| SetupError::io(path, e))?;
    Ok(kept.len())
}

/// Every target a journal records (used so coexistence does not report our own changes
/// as foreign residue). Registry targets are upper-cased for case-insensitive matching.
pub fn own_targets(files: &[(PathBuf, ParsedJournal)]) -> BTreeSet<String> {
    files.iter().flat_map(|(_, j)| j.records.iter()).map(|r| r.target.to_ascii_uppercase()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn rec(pkg: &str, seq: u64, time: &str, target: &str, prev: ValueState, new: ValueState, st: RecordStatus) -> JournalRecord {
        JournalRecord {
            v: 1,
            seq,
            time: time.into(),
            actor: "service".into(),
            package: pkg.into(),
            kind: RecordKind::Registry,
            target: target.into(),
            previous: prev,
            new,
            status: st,
        }
    }

    #[derive(Default)]
    struct Recorder {
        order: Mutex<Vec<String>>,
        live: HashMap<String, ValueState>,
        fail: Option<String>,
    }
    impl RestoreApplier for Recorder {
        fn restore(&self, r: &JournalRecord) -> Result<(), SetupError> {
            if self.fail.as_deref() == Some(r.target.as_str()) {
                return Err(SetupError::Other("boom".into()));
            }
            if r.kind == RecordKind::ServiceConfig {
                return Err(SetupError::Unsupported("service_config".into()));
            }
            self.order.lock().unwrap().push(format!("{}#{}={:?}", r.target, r.seq, r.previous));
            Ok(())
        }
        fn live(&self, r: &JournalRecord) -> Option<ValueState> {
            self.live.get(&r.target).cloned()
        }
    }

    #[test]
    fn contract_example_record_parses() {
        let line = r#"{"v":1,"seq":12,"time":"2026-11-12T10:00:00Z","actor":"service","package":"session","kind":"registry","target":"HKLM\\SYSTEM\\CurrentControlSet\\Control\\Terminal Server\\fDenyTSConnections","previous":{"type":"dword","value":1},"new":{"type":"dword","value":0},"status":"intent"}"#;
        let p = parse_journal(line);
        assert_eq!(p.bad_lines, 0);
        assert_eq!(p.records[0].previous, ValueState::Dword(1));
        assert_eq!(serde_json::to_string(&ValueState::Absent).unwrap(), r#"{"type":"absent"}"#);
        assert_eq!(serde_json::to_string(&p.records[0]).unwrap(), line);
    }

    #[test]
    fn torn_line_is_counted_not_fatal() {
        let good = serde_json::to_string(&rec("a", 1, "t1", "x", ValueState::Absent, ValueState::Dword(1), RecordStatus::Intent)).unwrap();
        let p = parse_journal(&format!("{good}\n\n{{\"v\":1,\"seq\":2,"));
        assert_eq!(p.records.len(), 1);
        assert_eq!(p.bad_lines, 1);
    }

    #[test]
    fn replay_is_reverse_and_ends_at_first_previous() {
        use RecordStatus::*;
        use ValueState::*;
        let j = ParsedJournal {
            records: vec![
                rec("session", 1, "2026-11-12T10:00:00Z", "K\\v", Dword(1), Dword(0), Intent),
                rec("session", 1, "2026-11-12T10:00:00Z", "K\\v", Dword(1), Dword(0), Done),
                rec("session", 2, "2026-11-12T10:00:01Z", "K\\v", Dword(0), Dword(5), Intent),
                rec("session", 2, "2026-11-12T10:00:01Z", "K\\v", Dword(0), Dword(5), Done),
            ],
            bad_lines: 0,
        };
        let j2 = ParsedJournal {
            records: vec![
                rec("termwrap", 1, "2026-11-12T09:00:00Z", "T\\dll", Sz("a".into()), Sz("b".into()), Intent),
                rec("termwrap", 1, "2026-11-12T09:00:00Z", "T\\dll", Sz("a".into()), Sz("b".into()), Done),
            ],
            bad_lines: 0,
        };
        let a = Recorder::default();
        let rep = replay_parsed(&[("s.jsonl".into(), j), ("t.jsonl".into(), j2)], &a, &ReplayOptions::default());
        assert_eq!(rep.restored, 3);
        let order = a.order.lock().unwrap().clone();
        assert_eq!(order, vec!["K\\v#2=Dword(0)", "K\\v#1=Dword(1)", "T\\dll#1=Sz(\"a\")"]);
        assert_eq!(rep.completed_files.len(), 2);
        assert!(rep.is_clean());
    }

    #[test]
    fn backwards_clock_step_keeps_seq_order_within_a_package() {
        use RecordStatus::*;
        use ValueState::*;
        // seq 1 at 10:00:05, then the clock steps back: seq 2 at 10:00:01.
        let j = ParsedJournal {
            records: vec![
                rec("session", 1, "2026-11-12T10:00:05Z", "K\\v", Dword(1), Dword(0), Done),
                rec("session", 2, "2026-11-12T10:00:01Z", "K\\v", Dword(0), Dword(5), Done),
            ],
            bad_lines: 0,
        };
        // Another package, fractional seconds, between the two in real time.
        let j2 = ParsedJournal {
            records: vec![rec("stream", 1, "2026-11-12T10:00:03.250Z", "S\\v", Absent, Dword(1), Done)],
            bad_lines: 0,
        };
        let a = Recorder::default();
        let rep = replay_parsed(&[("s.jsonl".into(), j), ("t.jsonl".into(), j2)], &a, &ReplayOptions::default());
        assert_eq!(rep.restored, 3);
        let order = a.order.lock().unwrap().clone();
        let pos = |t: &str| order.iter().position(|o| o.starts_with(t)).unwrap();
        assert!(pos("K\\v#2") < pos("K\\v#1"), "seq 2 restored before seq 1: {order:?}");
        assert_eq!(order.last().unwrap(), "K\\v#1=Dword(1)", "replay ends at the first previous");
        // Mixed RFC 3339 forms within one second sort by real time across packages.
        let ch = changes_newest_first(&[
            ("a".into(), ParsedJournal { records: vec![rec("a", 1, "2026-11-12T10:00:00.900Z", "x", Absent, Dword(1), Done)], bad_lines: 0 }),
            ("b".into(), ParsedJournal { records: vec![rec("b", 1, "2026-11-12T10:00:01Z", "y", Absent, Dword(1), Done)], bad_lines: 0 }),
        ]);
        assert_eq!(ch[0].record.package, "b");
    }

    #[test]
    fn deferred_records_survive_retire() {
        use ValueState::*;
        struct DeferAccounts;
        impl RestoreApplier for DeferAccounts {
            fn restore(&self, r: &JournalRecord) -> Result<(), SetupError> {
                if r.kind == RecordKind::Account { Err(SetupError::Deferred("kept".into())) } else { Ok(()) }
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let j = FileJournal::open(dir.path(), "session").unwrap();
        journaled(&j, "2026-11-12T10:00:00Z".into(), "service", "session", RecordKind::Account, "seat1", Absent, Sz("created".into()), || Ok(())).unwrap();
        journaled(&j, "2026-11-12T10:00:01Z".into(), "service", "session", RecordKind::Registry, "HKLM\\K\\v", Absent, Dword(1), || Ok(())).unwrap();
        let rep = replay_dir(dir.path(), &DeferAccounts, &ReplayOptions::default()).unwrap();
        assert_eq!(rep.restored, 1);
        assert_eq!(rep.deferred.len(), 1);
        assert_eq!(rep.completed_files.len(), 1);
        retire_keeping(&rep.completed_files[0], &rep.deferred).unwrap();
        let left = read_dir_journals(dir.path()).unwrap();
        assert_eq!(left.len(), 1, "live file kept for the deferred record");
        let ch = changes_newest_first(&left);
        assert_eq!(ch.len(), 1);
        assert_eq!(ch[0].record.target, "seat1");
        assert!(ch[0].done);
        assert!(j.path().with_extension("jsonl.replayed").exists());
        // seq continues after the carried-over record
        assert_eq!(FileJournal::open(dir.path(), "session").unwrap().next_seq(), 2);
    }

    #[test]
    fn done_record_content_wins_over_intent() {
        use RecordStatus::*;
        use ValueState::*;
        let mut intent = rec("setup", 1, "t", "inf:x.inf", Absent, Sz("vdd".into()), Intent);
        intent.kind = RecordKind::Driver;
        let mut done = intent.clone();
        done.status = Done;
        done.new = Sz("oem7.inf".into());
        let ch = changes_newest_first(&[("s".into(), ParsedJournal { records: vec![intent, done], bad_lines: 0 })]);
        assert_eq!(ch[0].record.new, Sz("oem7.inf".into()));
        assert!(ch[0].done);
    }

    #[test]
    fn intent_without_done_compares_live_state() {
        use RecordStatus::*;
        use ValueState::*;
        let j = ParsedJournal {
            records: vec![
                rec("p", 1, "t1", "never", Dword(1), Dword(0), Intent),
                rec("p", 2, "t2", "applied", Dword(1), Dword(0), Intent),
                rec("p", 3, "t3", "foreign", Dword(1), Dword(0), Intent),
                rec("p", 4, "t4", "unknown", Dword(1), Dword(0), Intent),
            ],
            bad_lines: 0,
        };
        let mut a = Recorder::default();
        a.live.insert("never".into(), Dword(1));
        a.live.insert("applied".into(), Dword(0));
        a.live.insert("foreign".into(), Dword(7));
        let rep = replay_parsed(&[("p.jsonl".into(), j)], &a, &ReplayOptions::default());
        assert_eq!(rep.not_applied, 1);
        assert_eq!(rep.conflicts.len(), 1);
        assert_eq!(rep.conflicts[0].target, "foreign");
        assert_eq!(rep.restored, 2); // applied + unknown (best effort)
        assert!(rep.completed_files.is_empty(), "conflict keeps the file");
    }

    #[test]
    fn skip_packages_unsupported_and_failures() {
        use RecordStatus::*;
        use ValueState::*;
        let mut svc = rec("pads", 1, "t1", "svc", Absent, Dword(1), Done);
        svc.kind = RecordKind::ServiceConfig;
        let files = vec![
            ("pads.jsonl".into(), ParsedJournal { records: vec![svc], bad_lines: 0 }),
            ("hooked.jsonl".into(), ParsedJournal { records: vec![rec("hooked", 1, "t1", "h", Absent, Dword(1), Done)], bad_lines: 0 }),
            ("bad.jsonl".into(), ParsedJournal { records: vec![rec("bad", 1, "t1", "boom", Absent, Dword(1), Done)], bad_lines: 2 }),
        ];
        let a = Recorder { fail: Some("boom".into()), ..Default::default() };
        let opts = ReplayOptions { skip_packages: ["hooked".to_string()].into() };
        let rep = replay_parsed(&files, &a, &opts);
        assert_eq!(rep.restored, 0);
        assert_eq!(rep.skipped.len(), 1);
        assert_eq!(rep.failed.len(), 1);
        assert_eq!(rep.bad_lines, 2);
        assert_eq!(rep.completed_files, vec![PathBuf::from("pads.jsonl")]);
    }

    #[test]
    fn file_journal_round_trip_compact_and_retire() {
        let dir = tempfile::tempdir().unwrap();
        let j = FileJournal::open(dir.path(), "setup").unwrap();
        let out = journaled(&j, "t".into(), "installer", "setup", RecordKind::Registry, "K\\v", ValueState::Absent, ValueState::Dword(7), || Ok(42)).unwrap();
        assert_eq!(out, 42);
        let failed: Result<(), _> = journaled(&j, "t".into(), "installer", "setup", RecordKind::Registry, "K\\w", ValueState::Absent, ValueState::Dword(1), || Err(SetupError::Other("x".into())));
        assert!(failed.is_err());
        let j2 = FileJournal::open(dir.path(), "setup").unwrap();
        assert_eq!(j2.next_seq(), 3, "seq continues after reopen");
        let files = read_dir_journals(dir.path()).unwrap();
        assert_eq!(files[0].1.records.len(), 3);
        let ch = changes_newest_first(&files);
        assert_eq!(ch.len(), 2);
        assert!(ch.iter().any(|c| c.record.target == "K\\w" && !c.done));
        assert!(own_targets(&files).contains("K\\V"));
        assert_eq!(compact(j.path()).unwrap(), 3);
        assert!(j.path().with_extension("jsonl.bak").exists());
        retire(j.path()).unwrap();
        assert!(read_dir_journals(dir.path()).unwrap().is_empty());
        assert!(read_dir_journals(&dir.path().join("missing")).unwrap().is_empty());
    }
}
