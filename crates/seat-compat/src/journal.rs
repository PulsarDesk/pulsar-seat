// SPDX-License-Identifier: GPL-3.0-only
//! Undo journal of every system change Pulsar Seat makes (architecture §6,
//! spec 90 §11 items 7 and 11): `%ProgramData%\Pulsar Seat\journal\changes.jsonl`.
//!
//! Format: JSON Lines, append-only. Each line is a [`Record`]: `apply` (a change with its
//! previous value, written **before** the change so a crash never loses an undo) or
//! `revert` (the change with that `seq` is no longer active, with the outcome).
//!
//! Trust (the service reads this file back as LocalSystem and acts on it):
//! - [`Journal::in_data_dir`] sets owner Administrators + the protected
//!   `PRIVATE_DIR_SDDL` on `journal\` (and `DATA_DIR_SDDL` on the data dir) when elevated,
//!   then refuses to work in a folder that is not owned and writable only by SYSTEM /
//!   Administrators or that is a link / junction;
//! - an existing `changes.jsonl` that is a link, has more than one hard link, or has an
//!   untrusted owner / DACL is renamed to `changes.jsonl.untrusted-<ms>` (quarantine) and
//!   the journal starts empty;
//! - every later open re-checks the open handle (no time-of-check gap on the file).
//!
//! Crash safety:
//! - a torn last line (crash mid-write) is ignored on read and cut off before the next
//!   append, so it never ends up in the middle of the file;
//! - re-applying a change on a target writes the new `apply` (carrying the original
//!   previous value) **before** closing the old entry as `superseded`. If a crash leaves
//!   both active, the newest one counts and the older is closed on the next write / revert.
//!
//! Revert rules:
//! - newest first;
//! - a registry value / power setting is restored only while it still holds the value we
//!   wrote; if someone changed it since, we leave it alone and report `changed_since`
//!   (never clobber a user's later choice) unless `force` is set;
//! - a registry target outside the tweak allowlist ([`crate::tweaks::is_tweak_target`])
//!   is never written (`refused`), whatever the journal says;
//! - a driver package is deleted only while the store still holds **our** package under
//!   that `oemNN.inf` name (original INF name and hash match), never with `/force`;
//! - an entry whose target already holds the previous value counts as reverted;
//! - a failed revert stays active and is retried on the next `revert_*` call.

use crate::ops::{ExclusionKind, Hive, OpsError, PowerSetting, RegData, SystemOps};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// File name inside `journal\`.
pub const JOURNAL_FILE: &str = "changes.jsonl";

/// One system change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Change {
    /// A registry value. `previous: None` = absent before; `written: None` = we deleted it.
    Registry {
        hive: Hive,
        key: String,
        name: String,
        previous: Option<RegData>,
        written: Option<RegData>,
        /// Keys the write created (the key and the ancestors that were missing, deepest
        /// first). The undo of a value that was absent before removes them again when they
        /// are empty (VM round 2, 2026-11-17: uninstall left empty policy keys behind).
        /// Additive: older records have none.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        created_keys: Vec<String>,
    },
    /// A power setting (AC index) on a scheme.
    PowerAc {
        setting: PowerSetting,
        previous: u32,
        written: u32,
    },
    /// A unicast address we added to an interface.
    IpAddress {
        interface_luid: u64,
        address: Ipv4Addr,
        prefix_len: u8,
    },
    /// A driver package we published into the driver store (`oemNN.inf`).
    DriverPackage {
        published: String,
        original: String,
        /// SHA-256 of the published INF at stage time (`None` in old entries).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        inf_sha256: Option<String>,
    },
    /// The session agent set the per-user palm check in this user's hive (undo value
    /// kept in that hive under `tweaks::USER_UNDO_KEY`). Journaled by the service
    /// **before** it asks the agent, so uninstall / reset can undo it through
    /// `HKEY_USERS\<sid>` even when the user is not logged on.
    UserPalmCheck { sid: String },
    /// A per-app GPU preference value we wrote in user `sid`'s hive
    /// (`Software\Microsoft\DirectX\UserGpuPreferences`, value name = the program path,
    /// spec 40 §6.4; added 2026-11-17). Undone through `HKEY_USERS\<sid>` (hive loaded
    /// from `NTUSER.DAT` when the user is not logged on), see [`crate::gpu_pref`].
    UserGpuPreference {
        sid: String,
        app: String,
        previous: Option<String>,
        written: String,
    },
    /// A System Restore checkpoint we created (informational; never reverted).
    RestorePoint { sequence: i64, description: String },
    /// A Microsoft Defender exclusion we added (it was not on the list before; added
    /// 2026-11-17, `crate::defender`). Undone with `MSFT_MpPreference.Remove`.
    DefenderExclusion { list: ExclusionKind, value: String },
    /// A logon right we added to account `sid` (it did not have it before; added
    /// 2026-11-17, `crate::rights`). Only `SeDenyInteractiveLogonRight` is ever undone.
    AccountRight { sid: String, right: String },
    /// An environment variable we wrote in user `sid`'s `Environment` key (added
    /// 2026-11-17, `crate::user_env`). Undone through `HKEY_USERS\<sid>`.
    UserEnvironment {
        sid: String,
        name: String,
        previous: Option<RegData>,
        written: RegData,
    },
    /// Game Bar's "open with the controller button" we turned off in user `sid`'s hive
    /// (`Software\Microsoft\GameBar\UseNexusForGameBarEnabled`, OSMS parity G5,
    /// 2026-11-18, `crate::guide_guard`). Undone through `HKEY_USERS\<sid>`.
    UserGameBarGuide {
        sid: String,
        previous: Option<RegData>,
        written: RegData,
    },
    /// A file we moved out of the way on the administrator's request (the legacy Steam
    /// proxy DLL, added 2026-11-17, `crate::steam`). Undone by moving it back while its
    /// place is free.
    QuarantinedFile { original: String, quarantined: String },
    /// A certificate we added to a machine store on the installer's opt-in (it was not in
    /// that store before; added 2026-11-17, `crate::certs`). Undone by deleting it from
    /// that store again.
    Certificate { store: crate::certs::CertStore, thumbprint: String },
    /// An optional Windows feature we turned on at the installer's opt-in (it was off
    /// before; added 2026-11-17, `crate::features`). Informational: uninstall keeps the
    /// feature (other software may rely on it by then).
    OptionalFeature { name: String },
}

impl Change {
    /// Stable target identity: two active entries on one target keep the **first**
    /// previous value (re-applying a tweak must not overwrite the original state).
    pub fn target(&self) -> String {
        match self {
            Self::Registry {
                hive, key, name, ..
            } => {
                format!(
                    "reg:{hive:?}:{}:{}",
                    key.to_ascii_lowercase(),
                    name.to_ascii_lowercase()
                )
            }
            Self::PowerAc { setting, .. } => {
                format!(
                    "power:{}:{}:{}",
                    setting.scheme, setting.subgroup, setting.setting
                )
            }
            Self::IpAddress {
                interface_luid,
                address,
                ..
            } => format!("ip:{interface_luid}:{address}"),
            Self::DriverPackage { published, .. } => {
                format!("drv:{}", published.to_ascii_lowercase())
            }
            Self::UserPalmCheck { sid } => format!("userpalm:{}", sid.to_ascii_uppercase()),
            Self::UserGpuPreference { sid, app, .. } => {
                format!("usergpu:{}:{}", sid.to_ascii_uppercase(), app.to_ascii_lowercase())
            }
            Self::RestorePoint { sequence, .. } => format!("rp:{sequence}"),
            Self::DefenderExclusion { list: kind, value } => {
                format!("defender:{kind:?}:{}", value.trim_end_matches('\\').to_ascii_lowercase())
            }
            Self::AccountRight { sid, right } => format!("right:{}:{right}", sid.to_ascii_uppercase()),
            Self::UserEnvironment { sid, name, .. } => {
                format!("userenv:{}:{}", sid.to_ascii_uppercase(), name.to_ascii_lowercase())
            }
            Self::UserGameBarGuide { sid, .. } => format!("usergamebar:{}", sid.to_ascii_uppercase()),
            Self::QuarantinedFile { original, .. } => format!("quarantine:{}", original.to_ascii_lowercase()),
            Self::Certificate { store, thumbprint } => format!("cert:{store:?}:{}", thumbprint.to_ascii_uppercase()),
            Self::OptionalFeature { name } => format!("feature:{}", name.to_ascii_lowercase()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub seq: u64,
    pub at_ms: u64,
    /// Who made it: `tweak:<id>`, `install`, `driver:<name>`, `ip:seat<N>`, …
    pub owner: String,
    pub change: Change,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RevertOutcome {
    Restored,
    /// Target already held the previous value.
    AlreadyRestored,
    /// Someone changed the value after us (or the driver store now holds another
    /// package under that name); left alone.
    ChangedSince,
    /// Informational entry (restore point) closed, or the target is gone.
    Closed,
    /// The change was never made (the write after the journal line failed).
    NotApplied,
    /// A newer entry on the same target took over (it carries the original previous value).
    Superseded,
    /// The target is outside what Pulsar Seat ever changes (tampered entry): untouched.
    Refused,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Record {
    Apply(Entry),
    Revert {
        seq: u64,
        at_ms: u64,
        outcome: RevertOutcome,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum JournalError {
    #[error("journal I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("journal line {line} is not valid: {message}")]
    Corrupt { line: usize, message: String },
    #[error("journal location is not trusted: {0}")]
    Untrusted(String),
    #[error(transparent)]
    Ops(#[from] OpsError),
}

/// Result of a revert pass.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevertReport {
    pub done: Vec<(u64, RevertOutcome)>,
    /// Entries that could not be reverted now (kept active).
    pub failed: Vec<(u64, String)>,
}

impl RevertReport {
    pub fn is_clean(&self) -> bool {
        self.failed.is_empty()
            && !self
                .done
                .iter()
                .any(|(_, o)| matches!(o, RevertOutcome::ChangedSince | RevertOutcome::Refused))
    }
}

/// Check applied to every open journal file handle.
type FileCheck = fn(&File) -> Result<(), String>;

/// Where records go. File-backed in production, memory-backed in tests.
enum Backing {
    File {
        path: PathBuf,
        check: Option<FileCheck>,
    },
    Memory(Mutex<Vec<String>>),
}

pub struct Journal {
    backing: Backing,
    lock: Mutex<()>,
    /// Where an untrusted journal file was moved when this journal was opened.
    quarantined: Option<PathBuf>,
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The production file check: trusted owner / DACL, no link, one hard link.
#[cfg(windows)]
fn default_check() -> Option<FileCheck> {
    Some(crate::winsec::check_file)
}
#[cfg(not(windows))]
fn default_check() -> Option<FileCheck> {
    None
}

/// Effective state of a record list.
struct State {
    /// Active entries, oldest first, at most one per target (the newest).
    active: Vec<Entry>,
    /// Older entries on a target that has a newer active entry (crash between the new
    /// `apply` and the `superseded` close): to be closed as superseded.
    stale: Vec<Entry>,
    max_seq: u64,
}

fn state_of(recs: Vec<Record>) -> State {
    let mut act: Vec<Entry> = Vec::new();
    let mut max_seq = 0;
    for r in recs {
        match r {
            Record::Apply(e) => {
                max_seq = max_seq.max(e.seq);
                act.push(e)
            }
            Record::Revert { seq, .. } => act.retain(|e| e.seq != seq),
        }
    }
    let mut active: Vec<Entry> = Vec::new();
    let mut stale = Vec::new();
    // Newest wins per target.
    for e in act.into_iter().rev() {
        let t = e.change.target();
        if active.iter().any(|a| a.change.target() == t) {
            stale.push(e);
        } else {
            active.push(e);
        }
    }
    active.reverse();
    stale.reverse();
    State {
        active,
        stale,
        max_seq,
    }
}

impl Journal {
    /// Journal at `<data_dir>\journal\changes.jsonl`: folders created if missing,
    /// hardened when elevated, and checked (see module docs). An untrusted existing file
    /// is quarantined; an untrusted folder is an error.
    pub fn in_data_dir(data_dir: &Path) -> Result<Self, JournalError> {
        let dir = data_dir.join(seat_core::consts::JOURNAL_DIR);
        fs::create_dir_all(&dir)?;
        #[cfg(windows)]
        {
            use seat_core::consts::{DATA_DIR_SDDL, PRIVATE_DIR_SDDL};
            if crate::win::is_elevated() {
                crate::winsec::harden(data_dir, DATA_DIR_SDDL).map_err(JournalError::Untrusted)?;
                crate::winsec::harden(&dir, PRIVATE_DIR_SDDL).map_err(JournalError::Untrusted)?;
            }
            for d in [data_dir, dir.as_path()] {
                crate::winsec::check_path(d)
                    .map_err(|e| JournalError::Untrusted(format!("{}: {e}", d.display())))?;
            }
        }
        Self::open_checked(dir.join(JOURNAL_FILE), default_check())
    }

    /// Journal at `path` with the production file check (the folder is the caller's
    /// responsibility; prefer [`Self::in_data_dir`]).
    pub fn at(path: PathBuf) -> Result<Self, JournalError> {
        Self::open_checked(path, default_check())
    }

    /// Journal at `path` **without** any trust check. Tests and `--console` dev runs on a
    /// private data dir only; never for the service's real journal.
    pub fn at_unchecked(path: PathBuf) -> Self {
        Self {
            backing: Backing::File { path, check: None },
            lock: Mutex::new(()),
            quarantined: None,
        }
    }

    fn open_checked(path: PathBuf, check: Option<FileCheck>) -> Result<Self, JournalError> {
        let mut quarantined = None;
        if let Some(c) = check
            && fs::symlink_metadata(&path).is_ok()
        {
            let verdict = match open_no_follow(&path, false, false) {
                Ok(f) => c(&f),
                Err(e) => Err(e.to_string()),
            };
            if verdict.is_err() {
                let q = path.with_file_name(format!("{JOURNAL_FILE}.untrusted-{}", now_ms()));
                // Renaming a link renames the link itself, never its target.
                fs::rename(&path, &q)?;
                quarantined = Some(q);
            }
        }
        let j = Self {
            backing: Backing::File { path, check },
            lock: Mutex::new(()),
            quarantined,
        };
        {
            let _g = j.lock.lock().unwrap();
            j.repair_tail()?;
        }
        Ok(j)
    }

    pub fn memory() -> Self {
        Self {
            backing: Backing::Memory(Mutex::new(Vec::new())),
            lock: Mutex::new(()),
            quarantined: None,
        }
    }

    /// Where an untrusted journal file was moved on open (report it to the admin).
    pub fn quarantined(&self) -> Option<&Path> {
        self.quarantined.as_deref()
    }

    /// Open the journal file (checked). `Ok(None)` when it does not exist and `create`
    /// is false. `create` implies write access.
    fn open_file(&self, create: bool, write: bool) -> Result<Option<File>, JournalError> {
        let Backing::File { path, check } = &self.backing else {
            return Ok(None);
        };
        let f = match open_no_follow(path, create, write || create) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !create => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        if let Some(c) = check {
            c(&f).map_err(|e| JournalError::Untrusted(format!("{}: {e}", path.display())))?;
        }
        Ok(Some(f))
    }

    /// Cut a torn last line (no trailing newline) so the next append starts a fresh line.
    /// Caller holds `lock`.
    fn repair_tail(&self) -> Result<(), JournalError> {
        let Some(mut f) = self.open_file(false, true)? else {
            return Ok(());
        };
        let mut buf = Vec::new();
        f.read_to_end(&mut buf)?;
        if buf.is_empty() || buf.last() == Some(&b'\n') {
            return Ok(());
        }
        let keep = buf.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
        f.set_len(keep as u64)?;
        f.sync_data()?;
        Ok(())
    }

    fn lines(&self) -> Result<Vec<String>, JournalError> {
        match &self.backing {
            Backing::Memory(v) => Ok(v.lock().unwrap().clone()),
            Backing::File { .. } => {
                let Some(mut f) = self.open_file(false, false)? else {
                    return Ok(Vec::new());
                };
                let mut buf = Vec::new();
                f.read_to_end(&mut buf)?;
                let text = String::from_utf8_lossy(&buf);
                Ok(text.lines().map(str::to_string).collect())
            }
        }
    }

    /// Caller holds `lock`.
    fn append_record(&self, r: &Record) -> Result<(), JournalError> {
        let line = serde_json::to_string(r).expect("records serialize");
        match &self.backing {
            Backing::Memory(v) => v.lock().unwrap().push(line),
            Backing::File { .. } => {
                self.repair_tail()?;
                let mut f = self.open_file(true, true)?.expect("created");
                f.seek(SeekFrom::End(0))?;
                let mut b = line.into_bytes();
                b.push(b'\n');
                f.write_all(&b)?;
                f.sync_data()?;
            }
        }
        Ok(())
    }

    /// Every record in order. A torn last line (crash mid-write) is ignored; corruption
    /// anywhere else is an error.
    pub fn records(&self) -> Result<Vec<Record>, JournalError> {
        let lines = self.lines()?;
        let n = lines.len();
        let mut out = Vec::new();
        for (i, l) in lines.iter().enumerate() {
            if l.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Record>(l) {
                Ok(r) => out.push(r),
                Err(_) if i + 1 == n => break,
                Err(e) => {
                    return Err(JournalError::Corrupt {
                        line: i + 1,
                        message: e.to_string(),
                    });
                }
            }
        }
        Ok(out)
    }

    fn state(&self) -> Result<State, JournalError> {
        Ok(state_of(self.records()?))
    }

    /// Changes that are still in effect, oldest first (one per target: the newest).
    pub fn active(&self) -> Result<Vec<Entry>, JournalError> {
        Ok(self.state()?.active)
    }

    pub fn active_for(&self, owner: &str) -> Result<Vec<Entry>, JournalError> {
        Ok(self
            .active()?
            .into_iter()
            .filter(|e| e.owner == owner)
            .collect())
    }

    /// Append an `apply` record. If the target already has an active entry, the new one
    /// inherits its previous value (the undo always restores the state from before
    /// Pulsar Seat's **first** change) and the old one is then closed as `superseded`.
    /// The new line is written first, so a crash in between never loses the undo.
    pub fn record(&self, owner: &str, change: Change) -> Result<Entry, JournalError> {
        let _g = self.lock.lock().unwrap();
        let st = self.state()?;
        let t = change.target();
        let mut change = change;
        // Oldest first: the stale entries are older than the active one.
        let mut old: Vec<&Entry> = st
            .stale
            .iter()
            .chain(st.active.iter())
            .filter(|e| e.change.target() == t)
            .collect();
        old.sort_by_key(|e| e.seq);
        if let Some(first) = old.first() {
            inherit_previous(&mut change, &first.change);
        }
        let e = Entry {
            seq: st.max_seq + 1,
            at_ms: now_ms(),
            owner: owner.to_string(),
            change,
        };
        self.append_record(&Record::Apply(e.clone()))?;
        for o in old {
            self.append_record(&Record::Revert {
                seq: o.seq,
                at_ms: now_ms(),
                outcome: RevertOutcome::Superseded,
            })?;
        }
        Ok(e)
    }

    pub fn close(&self, seq: u64, outcome: RevertOutcome) -> Result<(), JournalError> {
        let _g = self.lock.lock().unwrap();
        self.append_record(&Record::Revert {
            seq,
            at_ms: now_ms(),
            outcome,
        })
    }

    /// Close entries left behind by a crash inside [`Self::record`].
    fn close_stale(&self) -> Result<(), JournalError> {
        for e in self.state()?.stale {
            self.close(e.seq, RevertOutcome::Superseded)?;
        }
        Ok(())
    }

    /// Revert every active change (newest first). Used by `uninstall` and `reset`.
    pub fn revert_all(
        &self,
        ops: &dyn SystemOps,
        force: bool,
    ) -> Result<RevertReport, JournalError> {
        self.close_stale()?;
        let mut act = self.active()?;
        act.reverse();
        self.revert_entries(ops, &act, force)
    }

    /// Revert the active changes of `owner` whose target is not in `keep` (the targets a
    /// sync still wants). Added 2026-11-17.
    pub fn revert_owner_except(
        &self,
        ops: &dyn SystemOps,
        owner: &str,
        keep: &[String],
    ) -> Result<RevertReport, JournalError> {
        self.close_stale()?;
        let mut act: Vec<Entry> = self
            .active_for(owner)?
            .into_iter()
            .filter(|e| !keep.contains(&e.change.target()))
            .collect();
        act.reverse();
        self.revert_entries(ops, &act, false)
    }

    /// Revert the active changes of one owner.
    pub fn revert_owner(
        &self,
        ops: &dyn SystemOps,
        owner: &str,
        force: bool,
    ) -> Result<RevertReport, JournalError> {
        self.close_stale()?;
        let mut act = self.active_for(owner)?;
        act.reverse();
        self.revert_entries(ops, &act, force)
    }

    fn revert_entries(
        &self,
        ops: &dyn SystemOps,
        entries: &[Entry],
        force: bool,
    ) -> Result<RevertReport, JournalError> {
        let mut rep = RevertReport::default();
        // Keys any record of this journal created (VM round 2 run 3, 2026-11-17): with
        // every option off one by one, `exclude_driver_updates` created
        // `Policies\...\WindowsUpdate` and was undone first while `windows_update` values
        // still lived there; the later undo of those values found the key already there
        // at their write, so the empty key stayed behind.
        let created = self.created_keys_all()?;
        for e in entries {
            match revert_one(ops, &e.change, force, &created) {
                Ok(o) => {
                    self.close(e.seq, o)?;
                    rep.done.push((e.seq, o));
                }
                Err(err) => rep.failed.push((e.seq, err.to_string())),
            }
        }
        Ok(rep)
    }

    /// Every registry key a record of this journal created, with its hive (any state).
    fn created_keys_all(&self) -> Result<Vec<(Hive, String)>, JournalError> {
        let mut out: Vec<(Hive, String)> = Vec::new();
        for r in self.records()? {
            if let Record::Apply(Entry { change: Change::Registry { hive, created_keys, .. }, .. }) = r {
                for k in created_keys {
                    if !out.iter().any(|(h, x)| *h == hive && x.eq_ignore_ascii_case(&k)) {
                        out.push((hive, k));
                    }
                }
            }
        }
        Ok(out)
    }
}

/// Open without following a final link (the check then sees the link itself).
fn open_no_follow(path: &Path, create: bool, write: bool) -> std::io::Result<File> {
    let mut o = OpenOptions::new();
    o.read(true).write(write).create(create);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        o.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    o.open(path)
}

fn inherit_previous(new: &mut Change, old: &Change) {
    match (new, old) {
        (Change::Registry { previous, created_keys, .. }, Change::Registry { previous: p, created_keys: c, .. }) => {
            *previous = p.clone();
            // The first write created the keys; a later one found them.
            if created_keys.is_empty() {
                *created_keys = c.clone();
            }
        }
        (Change::PowerAc { previous, .. }, Change::PowerAc { previous: p, .. }) => *previous = *p,
        (
            Change::UserGpuPreference { previous, .. },
            Change::UserGpuPreference { previous: p, .. },
        ) => *previous = p.clone(),
        (
            Change::UserEnvironment { previous, .. },
            Change::UserEnvironment { previous: p, .. },
        ) => *previous = p.clone(),
        (Change::UserGameBarGuide { previous, .. }, Change::UserGameBarGuide { previous: p, .. }) => *previous = p.clone(),
        _ => {}
    }
}

fn revert_one(
    ops: &dyn SystemOps,
    c: &Change,
    force: bool,
    journal_created: &[(Hive, String)],
) -> Result<RevertOutcome, OpsError> {
    match c {
        Change::Registry {
            hive,
            key,
            name,
            previous,
            written,
            created_keys,
        } => {
            // Defence in depth: only values a tweak writes are ever restored.
            if !crate::tweaks::is_tweak_target(*hive, key, name) {
                return Ok(RevertOutcome::Refused);
            }
            let created = created_with_journal(*hive, created_keys, journal_created);
            let cur = ops.reg_read(*hive, key, name)?;
            if &cur == previous {
                if previous.is_none() {
                    remove_created_keys(ops, *hive, key, &created);
                }
                return Ok(RevertOutcome::AlreadyRestored);
            }
            if &cur != written && !force {
                return Ok(RevertOutcome::ChangedSince);
            }
            match previous {
                Some(p) => ops.reg_write(*hive, key, name, p)?,
                None => {
                    ops.reg_delete(*hive, key, name)?;
                    remove_created_keys(ops, *hive, key, &created);
                }
            }
            Ok(RevertOutcome::Restored)
        }
        Change::PowerAc {
            setting,
            previous,
            written,
        } => {
            let cur = ops.power_read_ac(setting)?;
            if cur == Some(*previous) {
                return Ok(RevertOutcome::AlreadyRestored);
            }
            if cur.is_none() {
                // The scheme or setting is gone (plan deleted): nothing to restore.
                return Ok(RevertOutcome::Closed);
            }
            if cur != Some(*written) && !force {
                return Ok(RevertOutcome::ChangedSince);
            }
            ops.power_write_ac(setting, *previous)?;
            Ok(RevertOutcome::Restored)
        }
        Change::IpAddress {
            interface_luid,
            address,
            ..
        } => {
            let present = ops
                .ip_list()?
                .iter()
                .any(|a| a.interface_luid == *interface_luid && a.address == *address);
            if !present {
                return Ok(RevertOutcome::AlreadyRestored);
            }
            ops.ip_remove(*interface_luid, *address)?;
            Ok(RevertOutcome::Restored)
        }
        Change::DriverPackage {
            published,
            original,
            inf_sha256,
        } => {
            // oemNN numbers are reused: delete only while the store still holds our
            // package under that name. `force` never overrides this.
            let Some(info) = ops.driver_info(published)? else {
                return Ok(RevertOutcome::AlreadyRestored);
            };
            let same_name = info.original_name.eq_ignore_ascii_case(original);
            let same_hash = match (inf_sha256, &info.inf_sha256) {
                (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
                (Some(_), None) => false,
                (None, _) => true, // old entry without a hash: name check only
            };
            if !(same_name && same_hash) {
                return Ok(RevertOutcome::ChangedSince);
            }
            ops.driver_delete(published)?;
            Ok(RevertOutcome::Restored)
        }
        Change::UserPalmCheck { sid } => {
            let mut restored = false;
            let found = ops.with_user_hive(sid, &mut |root| {
                restored = crate::tweaks::palm_check_revert_in(ops, Hive::Users, root)?;
                Ok(())
            })?;
            Ok(match (found, restored) {
                (false, _) => RevertOutcome::Closed, // profile deleted
                (true, true) => RevertOutcome::Restored,
                (true, false) => RevertOutcome::AlreadyRestored,
            })
        }
        Change::UserGpuPreference {
            sid,
            app,
            previous,
            written,
        } => crate::gpu_pref::revert_value(ops, sid, app, previous.as_deref(), written, force),
        Change::RestorePoint { .. } => Ok(RevertOutcome::Closed),
        Change::DefenderExclusion { list: kind, value } => {
            let Some(list) = ops.defender_exclusions()? else {
                // Defender is gone (another antivirus took over): nothing to undo.
                return Ok(RevertOutcome::Closed);
            };
            if !list.contains(*kind, value) {
                return Ok(RevertOutcome::AlreadyRestored);
            }
            ops.defender_exclusion(*kind, value, false)?;
            Ok(RevertOutcome::Restored)
        }
        Change::AccountRight { sid, right } => {
            if !crate::ops::is_sid_string(sid) || right != crate::rights::DENY_INTERACTIVE {
                return Ok(RevertOutcome::Refused);
            }
            if !ops.account_rights(sid)?.iter().any(|r| r == right) {
                return Ok(RevertOutcome::AlreadyRestored);
            }
            ops.account_right(sid, right, false)?;
            Ok(RevertOutcome::Restored)
        }
        Change::UserEnvironment {
            sid,
            name,
            previous,
            written,
        } => crate::user_env::revert_value(ops, sid, name, previous.as_ref(), written, force),
        Change::UserGameBarGuide { sid, previous, written } => crate::guide_guard::revert_value(ops, sid, previous.as_ref(), written, force),
        Change::QuarantinedFile { original, quarantined } => {
            if !crate::steam::is_quarantine_pair(original, quarantined) {
                return Ok(RevertOutcome::Refused);
            }
            if !ops.file_exists(quarantined)? {
                return Ok(RevertOutcome::Closed);
            }
            if ops.file_exists(original)? {
                return Ok(RevertOutcome::ChangedSince);
            }
            ops.file_move(quarantined, original)?;
            Ok(RevertOutcome::Restored)
        }
        Change::Certificate { store, thumbprint } => {
            if !crate::certs::is_thumbprint(thumbprint) {
                return Ok(RevertOutcome::Refused);
            }
            if !ops.cert_thumbprints(*store)?.iter().any(|t| t.eq_ignore_ascii_case(thumbprint)) {
                return Ok(RevertOutcome::AlreadyRestored);
            }
            ops.cert_remove(*store, thumbprint)?;
            Ok(RevertOutcome::Restored)
        }
        // Kept on purpose (see `crate::features`).
        Change::OptionalFeature { .. } => Ok(RevertOutcome::Closed),
    }

}

/// `key` and its ancestors that do not exist yet, deepest first (at most 8 levels; a key
/// with fewer than three path parts, like `SOFTWARE\Policies`, is never listed).
fn missing_keys(ops: &dyn SystemOps, hive: Hive, key: &str) -> Result<Vec<String>, OpsError> {
    let mut out = Vec::new();
    let mut k = key.trim_matches('\\').to_string();
    while out.len() < 8 && k.split('\\').count() >= 3 && !ops.reg_key_exists(hive, &k)? {
        out.push(k.clone());
        match k.rfind('\\') {
            Some(i) => k.truncate(i),
            None => break,
        }
    }
    Ok(out)
}

/// After the undo of a value that was absent before: remove the keys its write created,
/// deepest first, each only when it is empty. Only `key` itself and its ancestors are ever
/// touched (a record cannot name another key), and errors are ignored (an empty key left
/// behind is harmless).
fn remove_created_keys(ops: &dyn SystemOps, hive: Hive, key: &str, created: &[String]) {
    let own = key.trim_matches('\\').to_ascii_lowercase();
    for c in created {
        let c = c.trim_matches('\\');
        let l = c.to_ascii_lowercase();
        let ancestor_or_self = own == l || own.starts_with(&format!("{l}\\"));
        if !ancestor_or_self || c.split('\\').count() < 3 {
            continue;
        }
        // Already gone (an earlier undo removed it): its ancestors may still be empty.
        if matches!(ops.reg_key_exists(hive, c), Ok(false)) {
            continue;
        }
        if !matches!(ops.reg_delete_empty_key(hive, c), Ok(true)) {
            // Not empty (other values or subkeys): its ancestors are not empty either.
            break;
        }
    }
}

/// The keys a record created plus those other records of the journal created in the same
/// hive, deepest first, without duplicates. `remove_created_keys` still keeps only the
/// record's own key and its ancestors, and deletes only empty keys.
fn created_with_journal(hive: Hive, own: &[String], journal: &[(Hive, String)]) -> Vec<String> {
    let mut all: Vec<String> = own.to_vec();
    for (h, k) in journal {
        if *h == hive && !all.iter().any(|x| x.eq_ignore_ascii_case(k)) {
            all.push(k.clone());
        }
    }
    all.sort_by_key(|k| std::cmp::Reverse(k.trim_matches('\\').split('\\').count()));
    all
}

/// Write a registry value through the journal: records the previous value first, then
/// writes. Returns `false` when the value already had the wanted data (nothing changed,
/// nothing recorded).
pub fn set_registry(
    j: &Journal,
    ops: &dyn SystemOps,
    owner: &str,
    hive: Hive,
    key: &str,
    name: &str,
    data: Option<&RegData>,
) -> Result<bool, JournalError> {
    let previous = ops.reg_read(hive, key, name)?;
    if previous.as_ref() == data {
        return Ok(false);
    }
    let created_keys = if data.is_some() { missing_keys(ops, hive, key)? } else { Vec::new() };
    let e = j.record(
        owner,
        Change::Registry {
            hive,
            key: key.to_string(),
            name: name.to_string(),
            previous,
            written: data.cloned(),
            created_keys,
        },
    )?;
    let res = match data {
        Some(d) => ops.reg_write(hive, key, name, d),
        None => ops.reg_delete(hive, key, name),
    };
    if let Err(err) = res {
        // The journal says `written`, the system did not take it. Make the journal match
        // the system again: nothing changed -> close; something else is there now ->
        // record that as what we wrote, keeping the original previous value.
        let cur = ops.reg_read(hive, key, name)?;
        if let Change::Registry { previous: orig, .. } = &e.change {
            if &cur == orig {
                j.close(e.seq, RevertOutcome::NotApplied)?;
            } else {
                j.record(
                    owner,
                    Change::Registry {
                        hive,
                        key: key.to_string(),
                        name: name.to_string(),
                        previous: orig.clone(),
                        written: cur,
                        created_keys: Vec::new(),
                    },
                )?;
            }
        }
        return Err(err.into());
    }
    Ok(true)
}

/// Write a power setting through the journal (same contract as [`set_registry`]).
/// `Ok(false)` also when the setting does not exist on this PC.
pub fn set_power(
    j: &Journal,
    ops: &dyn SystemOps,
    owner: &str,
    s: &PowerSetting,
    value: u32,
) -> Result<bool, JournalError> {
    let Some(previous) = ops.power_read_ac(s)? else {
        return Ok(false);
    };
    if previous == value {
        return Ok(false);
    }
    let e = j.record(
        owner,
        Change::PowerAc {
            setting: s.clone(),
            previous,
            written: value,
        },
    )?;
    if let Err(err) = ops.power_write_ac(s, value) {
        let cur = ops.power_read_ac(s)?;
        if let Change::PowerAc { previous: orig, .. } = &e.change {
            match cur {
                Some(c) if c != *orig => {
                    j.record(
                        owner,
                        Change::PowerAc {
                            setting: s.clone(),
                            previous: *orig,
                            written: c,
                        },
                    )?;
                }
                _ => j.close(e.seq, RevertOutcome::NotApplied)?,
            }
        }
        return Err(err.into());
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::{DriverStoreInfo, MemoryOps};

    /// A tweak target (the allowlist only lets these through).
    const K: &str = r"SYSTEM\CurrentControlSet\Control\CrashControl";
    const V: &str = "CrashDumpEnabled";

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "pseat-journal-{tag}-{}-{}",
            std::process::id(),
            now_ms()
        ));
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// VM round 2 (2026-11-17): the undo removes the policy keys the writes created (in
    /// reverse order, each only when empty) and keeps keys that were there before.
    #[test]
    fn undo_removes_the_keys_it_created() {
        const WU: &str = r"SOFTWARE\Policies\Microsoft\Windows\WindowsUpdate";
        const AU: &str = r"SOFTWARE\Policies\Microsoft\Windows\WindowsUpdate\AU";
        let ops = MemoryOps::new();
        ops.add_key(Hive::Hklm, r"SOFTWARE\Policies\Microsoft\Windows");
        let j = Journal::memory();
        set_registry(&j, &ops, "t", Hive::Hklm, WU, "DeferFeatureUpdates", Some(&RegData::Dword(1))).unwrap();
        set_registry(&j, &ops, "t", Hive::Hklm, WU, "DeferFeatureUpdatesPeriodInDays", Some(&RegData::Dword(9))).unwrap();
        set_registry(&j, &ops, "t", Hive::Hklm, AU, "NoAutoRebootWithLoggedOnUsers", Some(&RegData::Dword(1))).unwrap();
        let recs: Vec<Vec<String>> = j
            .active()
            .unwrap()
            .into_iter()
            .map(|e| match e.change {
                Change::Registry { created_keys, .. } => created_keys,
                _ => panic!(),
            })
            .collect();
        assert_eq!(recs, vec![vec![WU.to_string()], vec![], vec![AU.to_string()]]);
        // A pre-existing key outside ours with a value stays whatever happens.
        ops.set(Hive::Hklm, r"SOFTWARE\Policies\Microsoft\Windows\System", "X", RegData::Dword(1));
        let r = j.revert_all(&ops, false).unwrap();
        assert!(r.is_clean());
        assert!(!ops.reg_key_exists(Hive::Hklm, AU).unwrap());
        assert!(!ops.reg_key_exists(Hive::Hklm, WU).unwrap());
        assert!(ops.reg_key_exists(Hive::Hklm, r"SOFTWARE\Policies\Microsoft\Windows").unwrap());
        // A key that existed (empty) before the write is kept.
        let j2 = Journal::memory();
        ops.add_key(Hive::Hklm, WU);
        set_registry(&j2, &ops, "t", Hive::Hklm, WU, "DeferQualityUpdates", Some(&RegData::Dword(1))).unwrap();
        j2.revert_all(&ops, false).unwrap();
        assert!(ops.reg_key_exists(Hive::Hklm, WU).unwrap());
        // Old records without the field still parse.
        let old = r#"{"kind":"registry","hive":"hklm","key":"A\\B\\C","name":"n","previous":null,"written":null}"#;
        assert!(matches!(serde_json::from_str::<Change>(old).unwrap(), Change::Registry { ref created_keys, .. } if created_keys.is_empty()));
    }

    /// VM round 2 run 3 (2026-11-17): owners undone one by one. The owner whose write
    /// created the key is undone first, while another owner's values still live there;
    /// the last undo in that key removes it (the journal remembers who created it).
    #[test]
    fn undo_per_owner_removes_a_key_another_owner_created() {
        const WU: &str = r"SOFTWARE\Policies\Microsoft\Windows\WindowsUpdate";
        const AU: &str = r"SOFTWARE\Policies\Microsoft\Windows\WindowsUpdate\AU";
        let ops = MemoryOps::new();
        ops.add_key(Hive::Hklm, r"SOFTWARE\Policies\Microsoft\Windows");
        let j = Journal::memory();
        set_registry(&j, &ops, "drivers", Hive::Hklm, WU, "ExcludeWUDriversInQualityUpdate", Some(&RegData::Dword(1))).unwrap();
        set_registry(&j, &ops, "wu", Hive::Hklm, WU, "DeferFeatureUpdates", Some(&RegData::Dword(1))).unwrap();
        set_registry(&j, &ops, "wu", Hive::Hklm, AU, "NoAutoRebootWithLoggedOnUsers", Some(&RegData::Dword(1))).unwrap();
        assert!(j.revert_owner(&ops, "drivers", false).unwrap().is_clean());
        assert!(ops.reg_key_exists(Hive::Hklm, WU).unwrap(), "still holds the other owner's values");
        assert!(j.revert_owner(&ops, "wu", false).unwrap().is_clean());
        assert!(!ops.reg_key_exists(Hive::Hklm, AU).unwrap());
        assert!(!ops.reg_key_exists(Hive::Hklm, WU).unwrap());
        assert!(ops.reg_key_exists(Hive::Hklm, r"SOFTWARE\Policies\Microsoft\Windows").unwrap());
    }

    #[test]
    fn set_and_revert_registry() {
        let ops = MemoryOps::new();
        let j = Journal::memory();
        assert!(set_registry(&j, &ops, "t", Hive::Hklm, K, V, Some(&RegData::Dword(1))).unwrap());
        // Same value again: no new record.
        assert!(!set_registry(&j, &ops, "t", Hive::Hklm, K, V, Some(&RegData::Dword(1))).unwrap());
        assert_eq!(j.active().unwrap().len(), 1);
        let r = j.revert_all(&ops, false).unwrap();
        assert_eq!(r.done, vec![(1, RevertOutcome::Restored)]);
        assert_eq!(ops.get(Hive::Hklm, K, V), None);
        assert!(j.active().unwrap().is_empty());
    }

    #[test]
    fn keeps_original_previous_on_reapply() {
        let ops = MemoryOps::new();
        ops.set(Hive::Hklm, K, V, RegData::Dword(7));
        let j = Journal::memory();
        set_registry(&j, &ops, "t", Hive::Hklm, K, V, Some(&RegData::Dword(1))).unwrap();
        set_registry(&j, &ops, "t", Hive::Hklm, K, V, Some(&RegData::Dword(2))).unwrap();
        let act = j.active().unwrap();
        assert_eq!(act.len(), 1);
        assert!(matches!(
            &act[0].change,
            Change::Registry {
                previous: Some(RegData::Dword(7)),
                written: Some(RegData::Dword(2)),
                ..
            }
        ));
        // New apply is written before the superseded close.
        let recs = j.records().unwrap();
        assert!(matches!(recs[1], Record::Apply(ref e) if e.seq == 2));
        assert!(matches!(
            recs[2],
            Record::Revert {
                seq: 1,
                outcome: RevertOutcome::Superseded,
                ..
            }
        ));
        let r = j.revert_all(&ops, false).unwrap();
        assert_eq!(r.done[0].1, RevertOutcome::Restored);
        assert_eq!(ops.get(Hive::Hklm, K, V), Some(RegData::Dword(7)));
    }

    #[test]
    fn crash_between_apply_and_superseded_keeps_undo() {
        let ops = MemoryOps::new();
        ops.set(Hive::Hklm, K, V, RegData::Dword(7));
        let j = Journal::memory();
        set_registry(&j, &ops, "t", Hive::Hklm, K, V, Some(&RegData::Dword(1))).unwrap();
        // Simulate the crash: second apply written, superseded close missing.
        {
            let _g = j.lock.lock().unwrap();
            j.append_record(&Record::Apply(Entry {
                seq: 2,
                at_ms: 0,
                owner: "t".into(),
                change: Change::Registry {
                    hive: Hive::Hklm,
                    key: K.into(),
                    name: V.into(),
                    previous: Some(RegData::Dword(7)),
                    written: Some(RegData::Dword(2)),
                    created_keys: Vec::new(),
                },
            }))
            .unwrap();
        }
        ops.set(Hive::Hklm, K, V, RegData::Dword(2));
        let act = j.active().unwrap();
        assert_eq!(act.len(), 1);
        assert_eq!(act[0].seq, 2);
        let r = j.revert_all(&ops, false).unwrap();
        assert!(r.is_clean(), "{r:?}");
        assert_eq!(ops.get(Hive::Hklm, K, V), Some(RegData::Dword(7)));
        assert!(state_of(j.records().unwrap()).stale.is_empty());
    }

    #[test]
    fn user_change_is_not_clobbered_but_force_restores() {
        let ops = MemoryOps::new();
        ops.set(Hive::Hklm, K, V, RegData::Dword(7));
        let j = Journal::memory();
        set_registry(&j, &ops, "t", Hive::Hklm, K, V, Some(&RegData::Dword(1))).unwrap();
        ops.set(Hive::Hklm, K, V, RegData::Dword(9));
        let mut act = j.active().unwrap();
        act.reverse();
        let r = j.revert_entries(&ops, &act, true).unwrap();
        assert_eq!(r.done[0].1, RevertOutcome::Restored);
        assert_eq!(ops.get(Hive::Hklm, K, V), Some(RegData::Dword(7)));
    }

    #[test]
    fn tampered_registry_target_is_refused() {
        let ops = MemoryOps::new();
        let j = Journal::memory();
        let evil = r"SYSTEM\CurrentControlSet\Services\Evil";
        j.record(
            "t",
            Change::Registry {
                hive: Hive::Hklm,
                key: evil.into(),
                name: "ImagePath".into(),
                previous: Some(RegData::Sz(r"C:\evil.exe".into())),
                written: None,
                created_keys: Vec::new(),
            },
        )
        .unwrap();
        let r = j.revert_all(&ops, true).unwrap();
        assert_eq!(r.done[0].1, RevertOutcome::Refused);
        assert!(!r.is_clean());
        assert_eq!(ops.get(Hive::Hklm, evil, "ImagePath"), None);
    }

    #[test]
    fn failed_write_closes_entry() {
        let ops = MemoryOps::new();
        let j = Journal::memory();
        *ops.fail_next_write.lock().unwrap() = Some("denied".into());
        assert!(set_registry(&j, &ops, "t", Hive::Hklm, K, V, Some(&RegData::Dword(1))).is_err());
        assert!(j.active().unwrap().is_empty());
    }

    #[test]
    fn file_journal_survives_and_repairs_torn_tail() {
        let dir = temp_dir("torn");
        let p = dir.join(JOURNAL_FILE);
        let j = Journal::at_unchecked(p.clone());
        let ops = MemoryOps::new();
        set_registry(&j, &ops, "t", Hive::Hklm, K, V, Some(&RegData::Dword(1))).unwrap();
        let mut f = OpenOptions::new().append(true).open(&p).unwrap();
        f.write_all(b"{\"op\":\"apply\",\"seq\":").unwrap();
        drop(f);
        // Read tolerates the torn tail...
        let j2 = Journal::at_unchecked(p.clone());
        assert_eq!(j2.active().unwrap().len(), 1);
        // ...and the next append does not glue onto it.
        set_registry(
            &j2,
            &ops,
            "t",
            Hive::Hklm,
            r"SYSTEM\CurrentControlSet\Control\Session Manager\Power",
            "HiberbootEnabled",
            Some(&RegData::Dword(0)),
        )
        .unwrap();
        set_registry(&j2, &ops, "t", Hive::Hklm, K, V, Some(&RegData::Dword(3))).unwrap();
        assert_eq!(j2.active().unwrap().len(), 2);
        let text = fs::read_to_string(&p).unwrap();
        assert!(text.ends_with('\n'));
        assert!(!text.contains("\"seq\":{"));
        let r = j2.revert_all(&ops, false).unwrap();
        assert!(r.is_clean(), "{r:?}");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn untrusted_file_is_quarantined() {
        let dir = temp_dir("quarantine");
        let p = dir.join(JOURNAL_FILE);
        let planted = Record::Apply(Entry {
            seq: 1,
            at_ms: 0,
            owner: "x".into(),
            change: Change::DriverPackage {
                published: "oem1.inf".into(),
                original: "gpu.inf".into(),
                inf_sha256: None,
            },
        });
        fs::write(
            &p,
            format!("{}\n", serde_json::to_string(&planted).unwrap()),
        )
        .unwrap();
        fn reject(_: &File) -> Result<(), String> {
            Err("owner S-1-5-21-x is not SYSTEM or Administrators".into())
        }
        let j = Journal::open_checked(p.clone(), Some(reject)).unwrap();
        let q = j.quarantined().unwrap().to_path_buf();
        assert!(q.exists());
        assert!(!p.exists());
        // Every later open is checked too: a re-planted file is refused, not read.
        fs::write(&p, b"").unwrap();
        assert!(matches!(j.active(), Err(JournalError::Untrusted(_))));
        fs::remove_dir_all(&dir).ok();
    }

    #[cfg(windows)]
    #[test]
    fn production_check_quarantines_hard_link() {
        if crate::win::is_elevated() {
            return; // owner would be Administrators; the hard-link rule still applies
        }
        let dir = temp_dir("hardlink");
        let p = dir.join(JOURNAL_FILE);
        fs::write(&p, b"").unwrap();
        let _ = fs::hard_link(&p, dir.join("other"));
        let j = Journal::at(p.clone()).unwrap();
        assert!(j.quarantined().is_some());
        fs::remove_dir_all(&dir).ok();
    }

    #[cfg(windows)]
    #[test]
    fn in_data_dir_refuses_user_owned_folder() {
        if crate::win::is_elevated() {
            return; // elevated runs harden the folder instead
        }
        let dir = temp_dir("datadir");
        assert!(matches!(
            Journal::in_data_dir(&dir),
            Err(JournalError::Untrusted(_))
        ));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn power_and_ip_and_driver_revert() {
        let ops = MemoryOps::new();
        let j = Journal::memory();
        let s = PowerSetting {
            scheme: "s".into(),
            subgroup: "g".into(),
            setting: "x".into(),
        };
        ops.power.lock().unwrap().insert(s.clone(), 600);
        assert!(set_power(&j, &ops, "p", &s, 0).unwrap());
        let missing = PowerSetting {
            setting: "none".into(),
            ..s.clone()
        };
        assert!(!set_power(&j, &ops, "p", &missing, 0).unwrap());
        ops.ip_add(5, Ipv4Addr::new(192, 168, 1, 50), 24).unwrap();
        j.record(
            "ip",
            Change::IpAddress {
                interface_luid: 5,
                address: Ipv4Addr::new(192, 168, 1, 50),
                prefix_len: 24,
            },
        )
        .unwrap();
        ops.driver_store.lock().unwrap().insert(
            "oem42.inf".into(),
            DriverStoreInfo {
                original_name: "x.inf".into(),
                inf_sha256: Some("aa".into()),
            },
        );
        j.record(
            "drv",
            Change::DriverPackage {
                published: "oem42.inf".into(),
                original: "x.inf".into(),
                inf_sha256: Some("AA".into()),
            },
        )
        .unwrap();
        let r = j.revert_all(&ops, false).unwrap();
        assert!(r.is_clean(), "{r:?}");
        assert_eq!(ops.power_read_ac(&s).unwrap(), Some(600));
        assert!(ops.ip_list().unwrap().is_empty());
        assert_eq!(
            *ops.deleted_drivers.lock().unwrap(),
            vec!["oem42.inf".to_string()]
        );
    }

    #[test]
    fn reused_oem_name_is_not_deleted() {
        let ops = MemoryOps::new();
        let j = Journal::memory();
        // Our package was removed by hand; a GPU package now has the same oemNN name.
        ops.driver_store.lock().unwrap().insert(
            "oem7.inf".into(),
            DriverStoreInfo {
                original_name: "gpu.inf".into(),
                inf_sha256: Some("bb".into()),
            },
        );
        j.record(
            "drv",
            Change::DriverPackage {
                published: "oem7.inf".into(),
                original: "seat-idd.inf".into(),
                inf_sha256: Some("aa".into()),
            },
        )
        .unwrap();
        // Same name, other content: also kept.
        ops.driver_store.lock().unwrap().insert(
            "oem8.inf".into(),
            DriverStoreInfo {
                original_name: "seat-idd.inf".into(),
                inf_sha256: Some("cc".into()),
            },
        );
        j.record(
            "drv",
            Change::DriverPackage {
                published: "oem8.inf".into(),
                original: "seat-idd.inf".into(),
                inf_sha256: Some("aa".into()),
            },
        )
        .unwrap();
        // Gone from the store: closed, nothing deleted.
        j.record(
            "drv",
            Change::DriverPackage {
                published: "oem9.inf".into(),
                original: "seat-idd.inf".into(),
                inf_sha256: None,
            },
        )
        .unwrap();
        let r = j.revert_all(&ops, true).unwrap();
        assert!(ops.deleted_drivers.lock().unwrap().is_empty());
        let outcomes: Vec<_> = r.done.iter().map(|(_, o)| *o).collect();
        assert_eq!(
            outcomes,
            vec![
                RevertOutcome::AlreadyRestored,
                RevertOutcome::ChangedSince,
                RevertOutcome::ChangedSince
            ]
        );
    }

    #[test]
    fn old_driver_entry_without_hash_still_parses() {
        let l = r#"{"op":"apply","seq":1,"at_ms":0,"owner":"d","change":{"kind":"driver_package","published":"oem1.inf","original":"a.inf"}}"#;
        let r: Record = serde_json::from_str(l).unwrap();
        assert!(matches!(
            r,
            Record::Apply(Entry {
                change: Change::DriverPackage {
                    inf_sha256: None,
                    ..
                },
                ..
            })
        ));
    }
}
