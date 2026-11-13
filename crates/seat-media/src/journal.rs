// SPDX-License-Identifier: GPL-3.0-only
//! Crash-safe undo records of the gates (every system change with its previous value,
//! including "absent"; architecture.md §6).
//!
//! Each gate keeps one JSON file under the service's private `run\` directory
//! (`gate-<kind>.json`). A record is written **before** the system change it describes, so a
//! crash between the two leaves at worst a record whose change never happened; restoring it
//! is harmless.
//!
//! Trust: the service replays these records as LocalSystem (device descriptors, registry
//! values in other users' hives), and `%ProgramData%` lets Users create folders and files.
//! So on Windows a journal is used only when its directory and file are owned by SYSTEM or
//! Administrators and every allow ACE is for one of them ([`untrusted_reason`], same rule
//! as `seat-session`'s restore journal). A missing directory is created with the protected
//! [`seat_core::consts::PRIVATE_DIR_SDDL`]. Anything else fails closed: the journal is
//! *refused* — nothing is read or restored and every write fails, so no gate changes the
//! system without a trusted undo record.

use serde::Serialize;
use serde::de::DeserializeOwned;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Where the records live. `Memory` is used by tests and when no directory is configured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JournalLocation {
    File(PathBuf),
    Memory,
}

/// Default location: `%ProgramData%\Pulsar Seat\run\<file>`.
pub fn default_location(file: &str) -> JournalLocation {
    match std::env::var_os("ProgramData") {
        Some(pd) => JournalLocation::File(
            Path::new(&pd).join(seat_core::consts::PRODUCT_DIR).join(seat_core::consts::RUN_DIR).join(file),
        ),
        None => JournalLocation::Memory,
    }
}

/// `S-1-5-18` LocalSystem.
pub const SID_SYSTEM: &str = "S-1-5-18";
/// `S-1-5-32-544` BUILTIN\Administrators.
pub const SID_ADMINS: &str = "S-1-5-32-544";
/// Principals allowed as owner and in every allow ACE of a journal directory / file.
pub const JOURNAL_TRUSTED: [&str; 2] = [SID_SYSTEM, SID_ADMINS];

pub const ACCESS_ALLOWED_ACE_TYPE: u8 = 0x0;
/// Denied, denied-object, denied-callback, denied-callback-object.
const DENY_ACE_TYPES: [u8; 4] = [0x1, 0x6, 0xA, 0xC];

/// One ACE as read from a DACL. `sid` is `None` when the SID could not be converted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ace {
    pub ace_type: u8,
    pub flags: u8,
    pub mask: u32,
    pub sid: Option<String>,
}

/// Owner and DACL of a file or directory (`dacl: None` = NULL DACL).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecurityInfo {
    pub owner: Option<String>,
    pub dacl: Option<Vec<Ace>>,
}

fn trusted(sid: Option<&str>) -> bool {
    sid.is_some_and(|s| JOURNAL_TRUSTED.iter().any(|a| a.eq_ignore_ascii_case(s)))
}

/// Pure: why a journal directory / file must not be trusted, or `None`. Owner must be
/// SYSTEM or Administrators and every non-deny ACE (inherit-only included) an allow ACE
/// for one of them.
pub fn untrusted_reason(info: &SecurityInfo) -> Option<String> {
    let show = |s: &Option<String>| s.clone().unwrap_or_else(|| "<unreadable SID>".into());
    if !trusted(info.owner.as_deref()) {
        return Some(format!("owner {} is not SYSTEM or Administrators", show(&info.owner)));
    }
    let Some(dacl) = &info.dacl else {
        return Some("NULL DACL".into());
    };
    for ace in dacl {
        if DENY_ACE_TYPES.contains(&ace.ace_type) {
            continue;
        }
        if ace.ace_type != ACCESS_ALLOWED_ACE_TYPE || !trusted(ace.sid.as_deref()) {
            return Some(format!("ACE type {:#x} for {} is not allowed", ace.ace_type, show(&ace.sid)));
        }
    }
    None
}

#[cfg(windows)]
mod secure {
    use super::untrusted_reason;
    use crate::win::acl::{create_dir_with_sddl, security_of_handle, security_of_path};
    use std::path::Path;

    fn refuse(what: &Path, why: String) -> String {
        format!("{} is not trusted: {why}", what.display())
    }

    /// Create the directory (protected DACL) when missing, then verify it.
    pub fn prepare_dir(dir: &Path) -> Result<(), String> {
        if !dir.exists() {
            if let Some(parent) = dir.parent().filter(|d| !d.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
            }
            create_dir_with_sddl(dir, seat_core::consts::PRIVATE_DIR_SDDL)?;
        }
        let info = security_of_path(dir).map_err(|e| refuse(dir, e))?;
        untrusted_reason(&info).map_or(Ok(()), |why| Err(refuse(dir, why)))
    }

    pub fn verify_file(f: &std::fs::File, path: &Path) -> Result<(), String> {
        let info = security_of_handle(f).map_err(|e| refuse(path, e))?;
        untrusted_reason(&info).map_or(Ok(()), |why| Err(refuse(path, why)))
    }
}

#[cfg(not(windows))]
mod secure {
    use std::path::Path;
    pub fn prepare_dir(dir: &Path) -> Result<(), String> {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())
    }
    pub fn verify_file(_: &std::fs::File, _: &Path) -> Result<(), String> {
        Ok(())
    }
}

fn denied(e: String) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::PermissionDenied, e)
}

/// A persisted map of undo records.
pub struct Journal<V> {
    location: JournalLocation,
    map: Mutex<BTreeMap<String, V>>,
    check_acl: bool,
    /// Set when the on-disk journal failed the trust check: nothing is restored, every
    /// write fails.
    refused: Option<String>,
}

impl<V: Serialize + DeserializeOwned + Clone> Journal<V> {
    /// Load existing records, with the ACL check on Windows (see the module docs). A
    /// missing file starts empty; an unparsable (but trusted) one is kept aside as
    /// `<file>.bad`.
    pub fn open(location: JournalLocation) -> Self {
        Self::open_with(location, cfg!(windows))
    }

    /// Without any ACL check. Only for tests and tools running as the file's owner; never
    /// for the service.
    pub fn open_unchecked(location: JournalLocation) -> Self {
        Self::open_with(location, false)
    }

    fn open_with(location: JournalLocation, check_acl: bool) -> Self {
        let mut j = Self { location, map: Mutex::new(BTreeMap::new()), check_acl, refused: None };
        if let JournalLocation::File(p) = j.location.clone() {
            match j.load(&p) {
                Ok(m) => *j.map.get_mut().unwrap_or_else(|e| e.into_inner()) = m,
                Err(why) => {
                    log::error!("gate journal {}: refused (fail closed): {why}", p.display());
                    j.refused = Some(why);
                }
            }
        }
        j
    }

    fn prepare_dir(&self, p: &Path) -> Result<(), String> {
        let Some(dir) = p.parent().filter(|d| !d.as_os_str().is_empty()) else { return Ok(()) };
        if self.check_acl { secure::prepare_dir(dir) } else { std::fs::create_dir_all(dir).map_err(|e| e.to_string()) }
    }

    fn verify_file(&self, f: &std::fs::File, p: &Path) -> Result<(), String> {
        if self.check_acl { secure::verify_file(f, p) } else { Ok(()) }
    }

    fn load(&self, p: &Path) -> Result<BTreeMap<String, V>, String> {
        self.prepare_dir(p)?;
        let mut f = match std::fs::File::open(p) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(e) => return Err(format!("read: {e}")),
        };
        self.verify_file(&f, p)?;
        let mut bytes = Vec::new();
        f.read_to_end(&mut bytes).map_err(|e| format!("read: {e}"))?;
        drop(f);
        match serde_json::from_slice(&bytes) {
            Ok(m) => Ok(m),
            Err(e) => {
                log::error!("gate journal {}: unreadable ({e}); kept as .bad", p.display());
                let _ = std::fs::rename(p, p.with_extension("json.bad"));
                Ok(BTreeMap::new())
            }
        }
    }

    /// Why the journal was refused (manager / diagnostics), if it was.
    pub fn untrusted(&self) -> Option<&str> {
        self.refused.as_deref()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, V>> {
        self.map.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn persist(&self, map: &BTreeMap<String, V>) -> std::io::Result<()> {
        if let Some(why) = &self.refused {
            return Err(denied(format!("journal_untrusted: {why}")));
        }
        let JournalLocation::File(p) = &self.location else { return Ok(()) };
        self.prepare_dir(p).map_err(denied)?;
        let tmp = p.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(map).map_err(std::io::Error::other)?;
        {
            // read: READ_CONTROL for the handle check.
            let mut f = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(true).open(&tmp)?;
            self.verify_file(&f, &tmp).map_err(denied)?;
            f.write_all(&bytes)?;
            f.sync_data()?;
        }
        std::fs::rename(&tmp, p)
    }

    pub fn get(&self, key: &str) -> Option<V> {
        self.lock().get(key).cloned()
    }

    pub fn all(&self) -> BTreeMap<String, V> {
        self.lock().clone()
    }

    /// Insert or replace a record and persist it. On a persist failure the in-memory map is
    /// left as it was, so memory never claims more than the disk.
    pub fn put(&self, key: &str, v: V) -> std::io::Result<()> {
        let mut m = self.lock();
        let mut next = m.clone();
        next.insert(key.to_string(), v);
        self.persist(&next)?;
        *m = next;
        Ok(())
    }

    pub fn remove(&self, key: &str) -> std::io::Result<()> {
        let mut m = self.lock();
        if m.contains_key(key) {
            let mut next = m.clone();
            next.remove(key);
            self.persist(&next)?;
            *m = next;
        }
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_on_disk() {
        let dir = std::env::temp_dir().join(format!("seat-media-journal-{}", std::process::id()));
        let p = dir.join("j.json");
        let _ = std::fs::remove_file(&p);
        {
            let j: Journal<u32> = Journal::open_unchecked(JournalLocation::File(p.clone()));
            j.put("a", 1).unwrap();
            j.put("b", 2).unwrap();
            j.remove("a").unwrap();
        }
        let j: Journal<u32> = Journal::open_unchecked(JournalLocation::File(p.clone()));
        assert_eq!(j.get("b"), Some(2));
        assert_eq!(j.get("a"), None);
        std::fs::write(&p, b"{not json").unwrap();
        let j: Journal<u32> = Journal::open_unchecked(JournalLocation::File(p.clone()));
        assert!(j.is_empty());
        assert!(p.with_extension("json.bad").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A journal in a folder a standard user created (the test's own temp dir: owner is
    /// the test user, DACL inherited from the profile) is never read nor written.
    #[cfg(windows)]
    #[test]
    fn user_owned_journal_is_refused() {
        let dir = std::env::temp_dir().join(format!("seat-media-journal-acl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("gate-device-security.json");
        std::fs::write(&p, br#"{"X\\0":{"original_sddl":"D:(A;;GA;;;WD)"}}"#).unwrap();
        let j: Journal<serde_json::Value> = Journal::open(JournalLocation::File(p.clone()));
        assert!(j.untrusted().is_some());
        assert!(j.all().is_empty());
        assert!(j.put("y", serde_json::Value::Null).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn trust_rule() {
        let ace = |sid: &str| Ace { ace_type: 0, flags: 0x3, mask: 0x1F_01FF, sid: Some(sid.into()) };
        let ok = SecurityInfo { owner: Some(SID_SYSTEM.into()), dacl: Some(vec![ace(SID_SYSTEM), ace(SID_ADMINS)]) };
        assert_eq!(untrusted_reason(&ok), None);
        let foreign_owner = SecurityInfo { owner: Some("S-1-5-21-1-2-3-1001".into()), ..ok.clone() };
        assert!(untrusted_reason(&foreign_owner).unwrap().contains("owner"));
        let mut users_ace = ok.clone();
        users_ace.dacl.as_mut().unwrap().push(ace("S-1-5-32-545"));
        assert!(untrusted_reason(&users_ace).is_some());
        let mut deny = ok.clone();
        deny.dacl.as_mut().unwrap().push(Ace { ace_type: 1, ..ace("S-1-1-0") });
        assert_eq!(untrusted_reason(&deny), None);
        assert!(untrusted_reason(&SecurityInfo { dacl: None, ..ok }).is_some());
    }
}
