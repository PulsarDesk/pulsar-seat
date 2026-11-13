// SPDX-License-Identifier: GPL-3.0-only
//! Class-level default descriptors (spec 70 §5.2(2), "race-free default"), run by the
//! `device_security` gate when `GatePlan::class_defaults` names classes.
//!
//! What it does, in this order, for every chosen setup class:
//!
//! 1. every present devnode of the class that the gate does **not** gate itself (internal
//!    COM ports, built-in optical drives, shared USB devices) gets an explicit open
//!    descriptor ([`open_sddl`]), journaled with its previous value
//!    (`gate-class-open.json`). Without it such a device would start closed after its next
//!    restart;
//! 2. the class gets a closed default descriptor ([`closed_class_sddl`]: SYSTEM, plus
//!    Administrators when `admins_bypass`), journaled with the previous class value
//!    (`gate-class-defaults.json`). New devices of the class then start closed to normal
//!    users until the gate opens them to the owning seat: no window between arrival and the
//!    service reacting.
//!
//! When a class leaves the list (feature off, admin unticks it) and on `revert_all`, the
//! class value is restored **first**, then the open descriptors. A devnode that the gate
//! started to gate after step 1 hands the true original back to the gate's own record, so
//! nothing ends up with our open descriptor after an uninstall.
//!
//! Limits (documented, spec 70 §5.2(2)):
//! - a class descriptor applies when a device object is created, so an internal device that
//!   appears *after* the feature is on (a mounted VHD, a hot-plugged SATA disk) is closed to
//!   normal users until its next start after the service wrote its open descriptor (replug,
//!   reboot). No internal devnode is ever restarted by this code;
//! - system change, off by default; the manager must only offer it after the maintainer's
//!   sign-off. Supported classes: CD-ROM and Ports ([`DeviceClassTarget::DEFAULT`]).
//! - **disk and volume classes are refused** ([`open_sddl`] is `None` for them, `apply`
//!   reports `class_not_supported` and never closes them). Their members include the system
//!   disk and the system volume. On stock Windows a normal user cannot open
//!   `\\.\PhysicalDriveN` or `\\.\C:` for raw read / write (spec 70 §5.2(1)); a fixed
//!   world-writable explicit descriptor would hand that raw access to every user (local
//!   privilege escalation), and a fixed narrow one could break file access on a
//!   secure-open volume. They can only come back once the code copies each devnode's
//!   *current effective* device-object descriptor (read with `READ_CONTROL`) into the
//!   explicit value, and that is checked on hardware.
//!
//! **HYPOTHESIS (untested on hardware):** [`OPEN_SDDL`] has the shape of the kernel's
//! public "open" default for named device objects (SYSTEM and Administrators all, Everyone
//! read / write / execute, restricted code read / execute), which is what CD-ROM and serial
//! device objects get without an explicit descriptor. It is **not**
//! `SDDL_DEVOBJ_SYS_ALL_ADM_RWX_WORLD_RW_RES_R` (that one gives Administrators only RWX and
//! Everyone no execute).

use crate::inventory::class_members;
use crate::journal::{Journal, JournalLocation};
use seat_core::backend::gate::DeviceClassTarget;
use seat_core::backend::{BackendError, BackendResult, ErrorKind};
use seat_core::pnp::PnpTree;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Explicit descriptor for CD-ROM / Ports devnodes of a closed class that stay open to
/// everybody: SYSTEM and Administrators all, Everyone read / write / execute, restricted
/// code read / execute. Never used for disk or volume devnodes ([`open_sddl`]).
pub const OPEN_SDDL: &str = "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGWGX;;;WD)(A;;GRGX;;;RC)";

/// The explicit open descriptor for ungated members of a closed class, or `None` when the
/// class must not be closed at all (disk and volume: see the module docs).
pub const fn open_sddl(target: DeviceClassTarget) -> Option<&'static str> {
    match target {
        DeviceClassTarget::Cdrom | DeviceClassTarget::Ports => Some(OPEN_SDDL),
        DeviceClassTarget::DiskDrive | DeviceClassTarget::Volume => None,
    }
}

/// The class may get a closed class default (it has a safe open descriptor).
pub const fn supported(target: DeviceClassTarget) -> bool {
    open_sddl(target).is_some()
}

/// The closed class default.
pub fn closed_class_sddl(admins_bypass: bool) -> String {
    if admins_bypass { "D:P(A;;GA;;;SY)(A;;GA;;;BA)".into() } else { "D:P(A;;GA;;;SY)".into() }
}

const ALL: [DeviceClassTarget; 4] =
    [DeviceClassTarget::DiskDrive, DeviceClassTarget::Cdrom, DeviceClassTarget::Volume, DeviceClassTarget::Ports];

fn target_of(class_guid: &str) -> Option<DeviceClassTarget> {
    ALL.into_iter().find(|t| t.class_guid().eq_ignore_ascii_case(class_guid))
}

/// Undo record of one class value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClassRecord {
    pub original_sddl: Option<String>,
    /// `None` while the write is in flight / failed.
    #[serde(default)]
    pub applied_sddl: Option<String>,
}

/// Undo record of one devnode given an open descriptor ([`open_sddl`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenRecord {
    pub class_guid: String,
    pub original_sddl: Option<String>,
    #[serde(default)]
    pub applied: bool,
}

/// Journals beside the gate's own one.
pub fn sibling(loc: &JournalLocation, file: &str) -> JournalLocation {
    match loc {
        JournalLocation::File(p) => JournalLocation::File(p.with_file_name(file)),
        JournalLocation::Memory => JournalLocation::Memory,
    }
}

pub struct ClassDefaults {
    classes: Journal<ClassRecord>,
    open: Journal<OpenRecord>,
}

/// What the gate's own journal knows about a devnode (so ownership hand-over works).
pub trait GatedRecords {
    /// The devnode carries a descriptor of the gate (seat-owned).
    fn is_gated(&self, instance_id: &str) -> bool;
    /// Replace the "original" the gate restores later (the open descriptor was ours).
    fn set_original(&self, instance_id: &str, original: Option<String>) -> BackendResult<()>;
}

fn jerr(e: std::io::Error) -> BackendError {
    BackendError::new(ErrorKind::Os, "journal_write_failed", e.to_string())
}

impl ClassDefaults {
    pub fn new(gate_journal: &JournalLocation) -> Self {
        Self {
            classes: Journal::open(sibling(gate_journal, "gate-class-defaults.json")),
            open: Journal::open(sibling(gate_journal, "gate-class-open.json")),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.classes.is_empty() && self.open.is_empty()
    }

    /// Bring the classes in line with `wanted`. Errors are collected per class, the rest
    /// goes on. Also returns the devnodes that got an open descriptor just now (the gate restarts
    /// the removable ones it classifies, so a device that started under the closed class
    /// default opens; internal devnodes are never restarted).
    pub fn apply(
        &self,
        io: &dyn super::SecurityIo,
        tree: &dyn PnpTree,
        gated: &dyn GatedRecords,
        wanted: &[DeviceClassTarget],
        admins_bypass: bool,
    ) -> (Vec<(String, BackendError)>, Vec<String>) {
        let mut errors = Vec::new();
        let mut opened = Vec::new();
        let wanted: HashSet<DeviceClassTarget> = wanted.iter().copied().collect();
        // A class we may keep closed: wanted and supported. Records of refused classes (disk,
        // volume, written by an older build) are released like an unticked class.
        let keep = |t: DeviceClassTarget| wanted.contains(&t) && supported(t);
        // Leave first (restores never depend on writes).
        for (guid, _) in self.classes.all() {
            if target_of(&guid).is_none_or(|t| !keep(t)) {
                if let Err(e) = self.release_class(io, tree, gated, &guid) {
                    errors.push((guid, e));
                }
            }
        }
        for (id, rec) in self.open.all() {
            if target_of(&rec.class_guid).is_none_or(|t| !keep(t)) && self.classes.get(&rec.class_guid).is_none() {
                if let Err(e) = self.release_open(io, tree, gated, &id, &rec) {
                    errors.push((id, e));
                }
            }
        }
        let closed = closed_class_sddl(admins_bypass);
        let mut order: Vec<DeviceClassTarget> = wanted.into_iter().collect();
        order.sort();
        for t in order {
            let guid = t.class_guid();
            let Some(open_sddl) = open_sddl(t) else {
                // Disk / volume: never closed, never given a fixed open descriptor.
                errors.push((
                    guid.to_string(),
                    BackendError::new(
                        ErrorKind::Policy,
                        "class_not_supported",
                        "disk and volume classes are never closed (system disk / volume)",
                    ),
                ));
                continue;
            };
            if let Err(e) = self.open_internal(io, tree, gated, guid, open_sddl, &mut opened) {
                // Never close a class whose internal devices could not all be opened.
                errors.push((guid.to_string(), e));
                continue;
            }
            if let Err(e) = self.close_class(io, guid, &closed) {
                errors.push((guid.to_string(), e));
            }
        }
        (errors, opened)
    }

    fn open_internal(
        &self,
        io: &dyn super::SecurityIo,
        tree: &dyn PnpTree,
        gated: &dyn GatedRecords,
        guid: &str,
        open_sddl: &str,
        opened: &mut Vec<String>,
    ) -> BackendResult<()> {
        for id in class_members(tree, guid) {
            if gated.is_gated(&id) {
                continue;
            }
            let rec = self.open.get(&id);
            if rec.as_ref().is_some_and(|r| r.applied) {
                // Applied before. Still ours: nothing to do. Changed since (an explicit write
                // by someone else): left alone, and `release_open` will not restore over it
                // either (it only restores while the devnode still holds our value).
                let now = io.get_sddl(&id)?;
                if now.as_deref() != Some(open_sddl) {
                    log::warn!("class defaults: {id} has a descriptor written by someone else; left alone");
                }
                continue;
            }
            let mut rec = match rec {
                Some(r) => OpenRecord { applied: false, ..r },
                None => OpenRecord { class_guid: guid.to_string(), original_sddl: io.get_sddl(&id)?, applied: false },
            };
            if rec.original_sddl.as_deref() == Some(open_sddl) {
                rec.original_sddl = None;
            }
            self.open.put(&id, rec.clone()).map_err(jerr)?;
            io.set_sddl(&id, Some(open_sddl))?;
            rec.applied = true;
            self.open.put(&id, rec).map_err(jerr)?;
            opened.push(id);
        }
        Ok(())
    }

    fn close_class(&self, io: &dyn super::SecurityIo, guid: &str, closed: &str) -> BackendResult<()> {
        let rec = self.classes.get(guid);
        if rec.as_ref().is_some_and(|r| r.applied_sddl.as_deref() == Some(closed)) {
            return Ok(());
        }
        let mut rec = match rec {
            Some(r) => ClassRecord { applied_sddl: None, ..r },
            None => ClassRecord { original_sddl: io.get_class_sddl(guid)?, applied_sddl: None },
        };
        self.classes.put(guid, rec.clone()).map_err(jerr)?;
        io.set_class_sddl(guid, Some(closed))?;
        rec.applied_sddl = Some(closed.to_string());
        self.classes.put(guid, rec).map_err(jerr)
    }

    fn release_class(&self, io: &dyn super::SecurityIo, tree: &dyn PnpTree, gated: &dyn GatedRecords, guid: &str) -> BackendResult<()> {
        if let Some(rec) = self.classes.get(guid) {
            if target_of(guid).is_none() {
                // Not a class we ever close: never write to it from a record.
                log::warn!("class defaults: record for unknown class {guid} dropped");
            } else {
                io.set_class_sddl(guid, rec.original_sddl.as_deref())?;
            }
            self.classes.remove(guid).map_err(jerr)?;
        }
        for (id, rec) in self.open.all() {
            if rec.class_guid.eq_ignore_ascii_case(guid) {
                self.release_open(io, tree, gated, &id, &rec)?;
            }
        }
        Ok(())
    }

    fn release_open(
        &self,
        io: &dyn super::SecurityIo,
        tree: &dyn PnpTree,
        gated: &dyn GatedRecords,
        id: &str,
        rec: &OpenRecord,
    ) -> BackendResult<()> {
        if gated.is_gated(id) {
            // The gate wrote a seat descriptor over ours: give it the true original.
            gated.set_original(id, rec.original_sddl.clone())?;
        } else if rec.applied {
            // Only devnodes of that class, present now; anything else keeps its record.
            let present = class_members(tree, &rec.class_guid).iter().any(|m| m.eq_ignore_ascii_case(id));
            if !present {
                log::warn!("class defaults: open record for {id} kept: not present now");
                return Ok(());
            }
            if io.get_sddl(id)?.as_deref() == Some(OPEN_SDDL) {
                io.set_sddl(id, rec.original_sddl.as_deref())?;
            }
        }
        self.open.remove(id).map_err(jerr)
    }

    /// Undo everything (disable / uninstall): classes first, then devnodes.
    pub fn revert_all(&self, io: &dyn super::SecurityIo, tree: &dyn PnpTree, gated: &dyn GatedRecords) -> BackendResult<()> {
        let mut first = None;
        for (guid, _) in self.classes.all() {
            if let Err(e) = self.release_class(io, tree, gated, &guid) {
                first.get_or_insert(e);
            }
        }
        for (id, rec) in self.open.all() {
            if let Err(e) = self.release_open(io, tree, gated, &id, &rec) {
                first.get_or_insert(e);
            }
        }
        first.map_or(Ok(()), Err)
    }

    #[cfg(test)]
    pub(crate) fn journals(&self) -> (&Journal<ClassRecord>, &Journal<OpenRecord>) {
        (&self.classes, &self.open)
    }
}
