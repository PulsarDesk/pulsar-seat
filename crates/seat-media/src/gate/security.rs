// SPDX-License-Identifier: GPL-3.0-only
//! `device_security` gate (spec 70 §5.2(1)): a protected device security descriptor plus the
//! secure-open characteristic on every devnode a seat owns — disk **and** volume devnodes
//! for storage — allowing SYSTEM, Administrators (configurable) and the logon SIDs of the
//! owning seats' sessions.
//!
//! Change → invalidate: after a descriptor change the devnode is restarted (storage: the
//! volumes are locked and dismounted first) so the new descriptor is in force and handles
//! opened under the old one are gone. A refused lock / query-remove is reported in
//! `GateReport::vetoed` and never forced (no data loss); the descriptor stays written and
//! the invalidation stays pending in the journal (`pending_invalidate`), retried and
//! reported again on every apply until it succeeds.
//!
//! `revert_all` restores only records whose devnode this gate classifies now (or a volume
//! of such a disk); the journal itself is trusted only when SYSTEM / Administrators own it
//! (`crate::journal`), and restored descriptors must parse as SDDL (`win::setupdi`).
//!
//! Class-level default descriptors (spec 70 §5.2(2), `GatePlan::class_defaults`, off by
//! default) run after the per-device pass: [`super::classdefault`].
//!
//! **HYPOTHESIS (untested on hardware):** the new descriptor takes effect only when the
//! device stack is rebuilt, hence the restart after every change; and a descriptor on the
//! volume devnode gates file opens through that volume (spec 70 §6 q1).

use super::{
    ChangeSource, GateCore, Invalidate, SecurityIo, VolumeIo, device_sddl, io_err, is_shared, plan_mask,
};
use crate::journal::{Journal, JournalLocation};
use seat_core::backend::{
    Availability, BackendError, BackendResult, DeviceGate, ErrorKind, GateBackendKind, GateCaps, GatePlan, GateReport,
    HotplugSink, IsolationStatus,
};
use seat_core::device::{Device, DeviceKind};
use seat_core::pnp::PnpTree;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// `FILE_DEVICE_SECURE_OPEN` (wdm.h device characteristic): the descriptor also applies to
/// opens of names *below* the device object (`\Device\X\anything`).
pub const FILE_DEVICE_SECURE_OPEN: u32 = 0x0000_0100;

/// Kinds this gate lists (hubs only for inheritance; printers, readers and NICs for the
/// isolation-status row).
pub const LISTED_KINDS: [DeviceKind; 8] = [
    DeviceKind::UsbHub,
    DeviceKind::Storage,
    DeviceKind::Webcam,
    DeviceKind::SerialPort,
    DeviceKind::SmartCardReader,
    DeviceKind::Printer,
    DeviceKind::Network,
    DeviceKind::Other,
];

/// Undo record of one devnode (written before the change).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecurityRecord {
    pub key: String,
    pub original_sddl: Option<String>,
    pub original_characteristics: Option<u32>,
    /// Descriptor known to be written (both setters succeeded). `None` while a write is in
    /// flight or after it failed, so the next apply writes again.
    #[serde(default)]
    pub applied_sddl: Option<String>,
    /// Disk / device devnode only: a descriptor change whose handle invalidation (dismount,
    /// restart) has not succeeded yet. Retried on every apply (spec 70 §5.2(1), §5.5).
    #[serde(default)]
    pub pending_invalidate: bool,
}

pub struct DeviceSecurityGate {
    core: GateCore,
    io: Arc<dyn SecurityIo>,
    volumes: Arc<dyn VolumeIo>,
    journal: Journal<SecurityRecord>,
    class: super::classdefault::ClassDefaults,
    available: Availability,
}

/// The gate's journal as seen by the class-default pass.
struct Gated<'a>(&'a Journal<SecurityRecord>);

impl super::classdefault::GatedRecords for Gated<'_> {
    fn is_gated(&self, instance_id: &str) -> bool {
        self.0.get(instance_id).is_some()
    }
    fn set_original(&self, instance_id: &str, original: Option<String>) -> BackendResult<()> {
        match self.0.get(instance_id) {
            Some(r) => self
                .0
                .put(instance_id, SecurityRecord { original_sddl: original, ..r })
                .map_err(|e| io_err("journal_write_failed", e)),
            None => Ok(()),
        }
    }
}

impl DeviceSecurityGate {
    pub fn new(
        pnp: Arc<dyn PnpTree>,
        io: Arc<dyn SecurityIo>,
        volumes: Arc<dyn VolumeIo>,
        changes: Arc<dyn ChangeSource>,
        journal: JournalLocation,
    ) -> Self {
        Self {
            core: GateCore::new(pnp, LISTED_KINDS.to_vec(), changes),
            io,
            volumes,
            class: super::classdefault::ClassDefaults::new(&journal),
            journal: Journal::open(journal),
            available: Availability::Available,
        }
    }

    /// Override availability (non-Windows builds report `unavailable`).
    pub fn with_availability(mut self, a: Availability) -> Self {
        self.available = a;
        self
    }

    fn restore(&self, instance_id: &str, rec: &SecurityRecord) -> BackendResult<()> {
        self.io.set_sddl(instance_id, rec.original_sddl.as_deref())?;
        self.io.set_characteristics(instance_id, rec.original_characteristics)?;
        self.journal.remove(instance_id).map_err(|e| io_err("journal_write_failed", e))
    }

    fn put(&self, instance_id: &str, rec: SecurityRecord) -> BackendResult<()> {
        self.journal.put(instance_id, rec).map_err(|e| io_err("journal_write_failed", e))
    }

    /// Write `sddl` on one devnode. Returns whether anything changed.
    ///
    /// The record (originals) is journaled first with `applied_sddl: None`; only after both
    /// setters succeed is `applied_sddl` set, so a failed write is retried on the next apply.
    fn write(&self, key: &str, instance_id: &str, sddl: &str) -> BackendResult<bool> {
        let rec = self.journal.get(instance_id);
        if rec.as_ref().is_some_and(|r| r.applied_sddl.as_deref() == Some(sddl)) {
            return Ok(false);
        }
        let mut rec = match rec {
            Some(r) => SecurityRecord { applied_sddl: None, ..r },
            None => SecurityRecord {
                key: key.to_string(),
                original_sddl: self.io.get_sddl(instance_id)?,
                original_characteristics: self.io.get_characteristics(instance_id)?,
                applied_sddl: None,
                pending_invalidate: false,
            },
        };
        self.put(instance_id, rec.clone())?;
        self.io.set_sddl(instance_id, Some(sddl))?;
        let chars = rec.original_characteristics.unwrap_or(0) | FILE_DEVICE_SECURE_OPEN;
        self.io.set_characteristics(instance_id, Some(chars))?;
        rec.applied_sddl = Some(sddl.to_string());
        self.put(instance_id, rec)?;
        Ok(true)
    }

    fn set_pending(&self, instance_id: &str, pending: bool) -> BackendResult<()> {
        match self.journal.get(instance_id) {
            Some(r) if r.pending_invalidate != pending => {
                self.put(instance_id, SecurityRecord { pending_invalidate: pending, ..r })
            }
            _ => Ok(()),
        }
    }

    /// Dismount the volumes, then restart the devnode. A refusal is never forced.
    fn invalidate(&self, instance_id: &str, vols: &[super::VolumeRef]) -> BackendResult<Invalidate> {
        // Dismount first so file handles go without a restart veto from the volume.
        for v in vols {
            if let Invalidate::Vetoed(why) = self.volumes.dismount(&v.name)? {
                log::warn!("device_security: dismount of {} refused: {why}", v.name);
                // The descriptor is written; the restart would be vetoed too.
                return Ok(Invalidate::Vetoed(why));
            }
        }
        let r = self.io.restart(instance_id)?;
        if let Invalidate::Vetoed(why) = &r {
            log::warn!("device_security: restart of {instance_id} vetoed: {why}");
        }
        Ok(r)
    }

    /// Instance ids whose records may be restored: devnodes this gate classifies now (never
    /// internal disks, `inventory::classify`) and the volumes of classified storage. A
    /// record naming anything else is never acted on.
    fn restorable_ids(&self) -> std::collections::HashSet<String> {
        let mut ids = std::collections::HashSet::new();
        for g in self.core.rescan() {
            if g.device.kind == DeviceKind::UsbHub {
                continue;
            }
            if g.device.kind == DeviceKind::Storage {
                if let Ok(vs) = self.volumes.volumes_of(&g.instance_id) {
                    ids.extend(vs.into_iter().map(|v| v.instance_id.to_ascii_uppercase()));
                }
            }
            ids.insert(g.instance_id.to_ascii_uppercase());
        }
        ids
    }

    /// Restore one devnode to its original descriptor. Returns whether anything changed.
    fn unwrite(&self, instance_id: &str) -> BackendResult<bool> {
        match self.journal.get(instance_id) {
            Some(rec) => self.restore(instance_id, &rec).map(|_| true),
            None => Ok(false),
        }
    }

    fn apply_device(&self, d: &Device, instance_id: &str, plan: &GatePlan, report: &mut GateReport) -> BackendResult<()> {
        let mask = plan_mask(plan, &d.key, d.kind);
        let shared = is_shared(mask);
        let mut targets = vec![instance_id.to_string()];
        let vols = if d.kind == DeviceKind::Storage { self.volumes.volumes_of(instance_id)? } else { Vec::new() };
        targets.extend(vols.iter().map(|v| v.instance_id.clone()));

        let mut changed = false;
        if shared {
            for t in &targets {
                changed |= self.unwrite(t)?;
            }
        } else {
            let (sddl, _) = device_sddl(d.kind, mask, &plan.logons, plan.admins_bypass);
            for t in &targets {
                changed |= self.write(d.key.as_str(), t, &sddl)?;
            }
        }
        // A narrowed descriptor must also revoke handles opened under the old one; a veto
        // stays pending (journaled on the devnode's record) until an apply gets through.
        // Widening (back to shared) has nothing to revoke; its restart is best effort.
        let pending = !shared && self.journal.get(instance_id).is_some_and(|r| r.pending_invalidate);
        if (changed || pending) && d.present {
            if !shared {
                self.set_pending(instance_id, true)?;
            }
            match self.invalidate(instance_id, &vols)? {
                Invalidate::Done => {
                    if !shared {
                        self.set_pending(instance_id, false)?;
                    }
                }
                Invalidate::Vetoed(_) => report.vetoed.push(d.key.clone()),
            }
        }
        report.applied.push((d.key.clone(), status(d.kind, shared)));
        Ok(())
    }
}

fn status(kind: DeviceKind, shared: bool) -> IsolationStatus {
    if shared { IsolationStatus::Shared } else if kind == DeviceKind::Network { IsolationStatus::Network } else { IsolationStatus::Descriptor }
}

impl DeviceGate for DeviceSecurityGate {
    fn kind(&self) -> GateBackendKind {
        GateBackendKind::DeviceSecurity
    }

    fn caps(&self) -> GateCaps {
        GateCaps {
            kinds: vec![DeviceKind::Storage, DeviceKind::Webcam, DeviceKind::SerialPort, DeviceKind::Other],
            revokes_handles: true,
            driver: None,
        }
    }

    fn availability(&self) -> Availability {
        self.available.clone()
    }

    fn enumerate(&self) -> BackendResult<Vec<Device>> {
        self.core.enumerate()
    }

    fn apply(&self, plan: &GatePlan) -> BackendResult<GateReport> {
        if !self.available.is_available() {
            return Err(BackendError::new(ErrorKind::Unsupported, "gate_unavailable", "device_security is not available"));
        }
        let mut report = GateReport::default();
        let nodes = self.core.rescan();
        for g in &nodes {
            let d = &g.device;
            let mask = plan_mask(plan, &d.key, d.kind);
            match d.kind {
                DeviceKind::UsbHub => continue,
                DeviceKind::Network => {
                    report.applied.push((d.key.clone(), IsolationStatus::Network));
                    continue;
                }
                // Readers: the smart-card service opens them from session 0 (no device ACL);
                // the RDP redirect and the module's WinSCard filter give each seat its own
                // (parity review 2026-11-17). Printers are opened by the spooler from
                // session 0 (§5.2(5)).
                DeviceKind::SmartCardReader => {
                    let st = if is_shared(mask) { IsolationStatus::Shared } else { IsolationStatus::SeatFilter };
                    report.applied.push((d.key.clone(), st));
                    continue;
                }
                DeviceKind::Printer => {
                    let st = if is_shared(mask) { IsolationStatus::Shared } else { IsolationStatus::NotSupported };
                    report.applied.push((d.key.clone(), st));
                    continue;
                }
                _ => {}
            }
            if d.warnings.contains(&seat_core::device::DeviceWarning::KeyCollision) && !is_shared(mask) {
                report.failed.push((
                    d.key.clone(),
                    BackendError::new(ErrorKind::Policy, "key_collision", "two devices share this key; not gated"),
                ));
                continue;
            }
            if let Err(e) = self.apply_device(d, &g.instance_id, plan, &mut report) {
                report.failed.push((d.key.clone(), e));
            }
        }
        // Records of devnodes that are gone (unplugged) stay: the descriptor lives in the
        // devnode's registry and applies again when it returns; the next apply after the
        // re-arrival rewrites or restores it. Volume records are kept with their disk.
        if !plan.class_defaults.is_empty() || !self.class.is_empty() {
            let (errors, opened) = self.class.apply(
                self.io.as_ref(),
                self.core.pnp().as_ref(),
                &Gated(&self.journal),
                &plan.class_defaults,
                plan.admins_bypass,
            );
            for (what, e) in errors {
                log::error!("device_security: class default {what}: {e}");
                if let Ok(k) = seat_core::DeviceKey::new(format!("cls;{what}")) {
                    report.failed.push((k, e));
                }
            }
            // A removable devnode this gate classifies that started under the closed class
            // default: restart it so the open descriptor applies (best effort). Internal
            // devnodes are never restarted.
            let ours: std::collections::HashSet<String> = nodes.iter().map(|g| g.instance_id.to_ascii_uppercase()).collect();
            for id in opened.iter().filter(|id| ours.contains(&id.to_ascii_uppercase())) {
                if let Err(e) = self.io.restart(id) {
                    log::warn!("device_security: restart of {id} after opening it: {e}");
                }
            }
        }
        Ok(report)
    }

    fn revert_all(&self) -> BackendResult<()> {
        let mut first_err = None;
        let records = self.journal.all();
        let allowed = if records.is_empty() { Default::default() } else { self.restorable_ids() };
        for (instance_id, rec) in records {
            if !allowed.contains(&instance_id.to_ascii_uppercase()) {
                // Unplugged now, or not a devnode this gate handles: keep the record (the
                // next apply after re-arrival restores it) and never touch anything else.
                log::warn!("device_security: record for {instance_id} kept: not a gated devnode now");
                continue;
            }
            if let Err(e) = self.restore(&instance_id, &rec) {
                log::error!("device_security: restore of {instance_id} failed: {e}");
                first_err.get_or_insert(e);
            } else if let Err(e) = self.io.restart(&instance_id) {
                // Restoring widens access; a failed restart only delays it to the next start.
                log::warn!("device_security: restart after restore of {instance_id}: {e}");
            }
        }
        if let Err(e) = self.class.revert_all(self.io.as_ref(), self.core.pnp().as_ref(), &Gated(&self.journal)) {
            log::error!("device_security: class defaults revert failed: {e}");
            first_err.get_or_insert(e);
        }
        first_err.map_or(Ok(()), Err)
    }

    fn set_hotplug_sink(&self, sink: HotplugSink) {
        self.core.set_hotplug_sink(sink);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::gate::{NoChanges, VolumeRef};
    use crate::inventory::tests::sample_tree;
    use seat_core::backend::gate::SeatLogon;
    use seat_core::ids::{SeatMask, SeatNumber};
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Default)]
    pub(crate) struct FakeSys {
        pub sddl: Mutex<HashMap<String, String>>,
        pub chars: Mutex<HashMap<String, u32>>,
        pub restarts: Mutex<Vec<String>>,
        pub dismounts: Mutex<Vec<String>>,
        pub veto_dismount: Mutex<bool>,
        pub fail_set: Mutex<bool>,
        pub fail_remove_letter: Mutex<bool>,
        pub letters: Mutex<HashMap<char, String>>,
        pub log: Mutex<Vec<String>>,
        pub class_sddl: Mutex<HashMap<String, String>>,
    }

    impl SecurityIo for FakeSys {
        fn get_sddl(&self, id: &str) -> BackendResult<Option<String>> {
            Ok(self.sddl.lock().unwrap().get(id).cloned())
        }
        fn set_sddl(&self, id: &str, s: Option<&str>) -> BackendResult<()> {
            if *self.fail_set.lock().unwrap() {
                return Err(BackendError::new(ErrorKind::Os, "setupapi_failed", "transient"));
            }
            let mut m = self.sddl.lock().unwrap();
            match s {
                Some(s) => m.insert(id.into(), s.into()),
                None => m.remove(id),
            };
            Ok(())
        }
        fn get_characteristics(&self, id: &str) -> BackendResult<Option<u32>> {
            Ok(self.chars.lock().unwrap().get(id).copied())
        }
        fn set_characteristics(&self, id: &str, v: Option<u32>) -> BackendResult<()> {
            let mut m = self.chars.lock().unwrap();
            match v {
                Some(v) => m.insert(id.into(), v),
                None => m.remove(id),
            };
            Ok(())
        }
        fn restart(&self, id: &str) -> BackendResult<Invalidate> {
            self.restarts.lock().unwrap().push(id.into());
            Ok(Invalidate::Done)
        }
        fn get_class_sddl(&self, guid: &str) -> BackendResult<Option<String>> {
            Ok(self.class_sddl.lock().unwrap().get(guid).cloned())
        }
        fn set_class_sddl(&self, guid: &str, s: Option<&str>) -> BackendResult<()> {
            let mut m = self.class_sddl.lock().unwrap();
            match s {
                Some(s) => m.insert(guid.into(), s.into()),
                None => m.remove(guid),
            };
            Ok(())
        }
    }

    pub(crate) const DISK: &str = r"USBSTOR\DISK&VEN_X\SN1&0";
    pub(crate) const VOL: &str = r"STORAGE\VOLUME\{V1}";
    pub(crate) const VOLNAME: &str = r"\\?\Volume{00000000-0000-0000-0000-000000000001}\";

    impl VolumeIo for FakeSys {
        fn volumes_of(&self, disk: &str) -> BackendResult<Vec<VolumeRef>> {
            if disk != DISK {
                return Ok(vec![]);
            }
            let letters = self.letters.lock().unwrap().iter().filter(|(_, v)| v.as_str() == VOLNAME).map(|(l, _)| *l).collect();
            Ok(vec![VolumeRef { instance_id: VOL.into(), name: VOLNAME.into(), letters }])
        }
        fn dismount(&self, v: &str) -> BackendResult<Invalidate> {
            self.dismounts.lock().unwrap().push(v.into());
            if *self.veto_dismount.lock().unwrap() { Ok(Invalidate::Vetoed("open files".into())) } else { Ok(Invalidate::Done) }
        }
        fn remove_global_letter(&self, l: char) -> BackendResult<()> {
            if *self.fail_remove_letter.lock().unwrap() {
                return Err(BackendError::new(ErrorKind::Os, "mountmgr_failed", "transient"));
            }
            self.letters.lock().unwrap().remove(&l);
            self.log.lock().unwrap().push(format!("del {l}"));
            Ok(())
        }
        fn add_global_letter(&self, l: char, v: &str) -> BackendResult<()> {
            self.letters.lock().unwrap().insert(l, v.into());
            self.log.lock().unwrap().push(format!("add {l}"));
            Ok(())
        }
        fn letter_in_use(&self, l: char) -> bool {
            self.letters.lock().unwrap().contains_key(&l)
        }
    }

    pub(crate) fn seat(n: u8) -> SeatNumber {
        SeatNumber::new(n).unwrap()
    }

    pub(crate) fn logon(n: u8, session: u32, logon: &str) -> SeatLogon {
        SeatLogon { seat: seat(n), session_id: session, logon_sid: logon.into(), user_sid: format!("S-1-5-21-9-{n}") }
    }

    fn gate(sys: &Arc<FakeSys>) -> DeviceSecurityGate {
        DeviceSecurityGate::new(Arc::new(sample_tree()), sys.clone(), sys.clone(), Arc::new(NoChanges), JournalLocation::Memory)
    }

    fn disk_key(g: &DeviceSecurityGate) -> seat_core::DeviceKey {
        g.enumerate().unwrap().into_iter().find(|d| d.kind == DeviceKind::Storage).unwrap().key
    }

    #[test]
    fn owned_disk_gets_descriptor_on_disk_and_volume_then_restores() {
        let sys = Arc::new(FakeSys::default());
        sys.sddl.lock().unwrap().insert(DISK.into(), "D:(A;;GA;;;WD)".into());
        let g = gate(&sys);
        let key = disk_key(&g);
        let plan = GatePlan {
            devices: vec![(key.clone(), SeatMask::from_seats([seat(2)]))],
            logons: vec![logon(2, 3, "S-1-5-5-0-77")],
            admins_bypass: true,
            class_defaults: vec![],
            printer_hosts: vec![],
        };
        let r = g.apply(&plan).unwrap();
        assert!(r.applied.contains(&(key.clone(), IsolationStatus::Descriptor)));
        let want = "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGWGX;;;S-1-5-5-0-77)";
        assert_eq!(sys.sddl.lock().unwrap().get(DISK).map(String::as_str), Some(want));
        assert_eq!(sys.sddl.lock().unwrap().get(VOL).map(String::as_str), Some(want));
        assert_eq!(sys.chars.lock().unwrap().get(DISK), Some(&FILE_DEVICE_SECURE_OPEN));
        assert_eq!(sys.dismounts.lock().unwrap().as_slice(), [VOLNAME]);
        assert_eq!(sys.restarts.lock().unwrap().as_slice(), [DISK]);
        // Idempotent: same plan, no new restart.
        g.apply(&plan).unwrap();
        assert_eq!(sys.restarts.lock().unwrap().len(), 1);
        // Logoff: only SYSTEM / admins remain, devnode restarted again.
        let mut off = plan.clone();
        off.logons.clear();
        g.apply(&off).unwrap();
        assert_eq!(sys.sddl.lock().unwrap().get(DISK).map(String::as_str), Some("D:P(A;;GA;;;SY)(A;;GA;;;BA)"));
        assert_eq!(sys.restarts.lock().unwrap().len(), 2);
        // Back to shared → original values restored, volume descriptor removed.
        let shared = GatePlan { devices: vec![(key.clone(), SeatMask::SHARED)], ..off };
        let r = g.apply(&shared).unwrap();
        assert!(r.applied.contains(&(key, IsolationStatus::Shared)));
        assert_eq!(sys.sddl.lock().unwrap().get(DISK).map(String::as_str), Some("D:(A;;GA;;;WD)"));
        assert!(!sys.sddl.lock().unwrap().contains_key(VOL));
        assert!(!sys.chars.lock().unwrap().contains_key(DISK));
        assert!(g.journal.is_empty());
    }

    #[test]
    fn veto_is_reported_not_forced() {
        let sys = Arc::new(FakeSys::default());
        *sys.veto_dismount.lock().unwrap() = true;
        let g = gate(&sys);
        let key = disk_key(&g);
        let plan = GatePlan { devices: vec![(key.clone(), SeatMask::from_seats([seat(3)]))], logons: vec![], admins_bypass: false, class_defaults: vec![], printer_hosts: vec![] };
        let r = g.apply(&plan).unwrap();
        assert_eq!(r.vetoed, vec![key.clone()]);
        assert!(sys.restarts.lock().unwrap().is_empty());
        assert_eq!(sys.sddl.lock().unwrap().get(DISK).map(String::as_str), Some("D:P(A;;GA;;;SY)"));
        // Same plan, still vetoed: reported again, dismount retried.
        let r = g.apply(&plan).unwrap();
        assert_eq!(r.vetoed, vec![key.clone()]);
        assert_eq!(sys.dismounts.lock().unwrap().len(), 2);
        // Files closed: the next apply invalidates although the descriptor is unchanged.
        *sys.veto_dismount.lock().unwrap() = false;
        let r = g.apply(&plan).unwrap();
        assert!(r.vetoed.is_empty());
        assert_eq!(sys.restarts.lock().unwrap().as_slice(), [DISK]);
        assert!(!g.journal.get(DISK).unwrap().pending_invalidate);
        // And then it is idempotent again.
        g.apply(&plan).unwrap();
        assert_eq!(sys.restarts.lock().unwrap().len(), 1);
    }

    #[test]
    fn failed_write_is_retried() {
        let sys = Arc::new(FakeSys::default());
        *sys.fail_set.lock().unwrap() = true;
        let g = gate(&sys);
        let key = disk_key(&g);
        let plan = GatePlan { devices: vec![(key.clone(), SeatMask::from_seats([seat(3)]))], logons: vec![], admins_bypass: false, class_defaults: vec![], printer_hosts: vec![] };
        let r = g.apply(&plan).unwrap();
        assert!(r.failed.iter().any(|(k, _)| *k == key));
        assert_eq!(g.journal.get(DISK).unwrap().applied_sddl, None);
        *sys.fail_set.lock().unwrap() = false;
        let r = g.apply(&plan).unwrap();
        assert!(r.failed.is_empty());
        assert_eq!(sys.sddl.lock().unwrap().get(DISK).map(String::as_str), Some("D:P(A;;GA;;;SY)"));
        assert_eq!(sys.restarts.lock().unwrap().as_slice(), [DISK]);
    }

    #[test]
    fn revert_skips_records_of_ungated_devnodes() {
        let sys = Arc::new(FakeSys::default());
        let g = gate(&sys);
        let internal = r"SCSI\DISK&VEN_NVME\5&1&0";
        g.journal
            .put(
                internal,
                SecurityRecord {
                    key: "stor;x".into(),
                    original_sddl: Some("D:(A;;GA;;;WD)".into()),
                    original_characteristics: None,
                    applied_sddl: Some("D:P(A;;GA;;;SY)".into()),
                    pending_invalidate: false,
                },
            )
            .unwrap();
        g.revert_all().unwrap();
        assert!(!sys.sddl.lock().unwrap().contains_key(internal));
        assert!(g.journal.get(internal).is_some(), "kept, never applied");
    }

    #[test]
    fn revert_all_restores_everything() {
        let sys = Arc::new(FakeSys::default());
        let g = gate(&sys);
        let devices: Vec<_> = g.enumerate().unwrap().into_iter().map(|d| (d.key, SeatMask::from_seats([seat(2)]))).collect();
        let r = g.apply(&GatePlan { devices, logons: vec![], admins_bypass: true, class_defaults: vec![], printer_hosts: vec![] }).unwrap();
        assert!(r.applied.iter().any(|(_, s)| *s == IsolationStatus::Network));
        assert!(!sys.sddl.lock().unwrap().is_empty());
        g.revert_all().unwrap();
        assert!(sys.sddl.lock().unwrap().is_empty());
        assert!(sys.chars.lock().unwrap().is_empty());
        assert!(g.journal.is_empty());
    }

    const PORT: &str = r"FTDIBUS\VID_0403+A\0000";

    #[test]
    fn class_defaults_open_members_close_class_and_restore() {
        use crate::gate::classdefault::OPEN_SDDL;
        use crate::inventory::class;
        use seat_core::backend::gate::DeviceClassTarget;
        let sys = Arc::new(FakeSys::default());
        sys.class_sddl.lock().unwrap().insert(class::PORTS.into(), "D:(A;;GA;;;WD)".into());
        let g = gate(&sys);
        let key = g.enumerate().unwrap().into_iter().find(|d| d.kind == DeviceKind::SerialPort).unwrap().key;
        // Shared USB serial port: not gated by the per-device pass.
        let plan = GatePlan {
            devices: vec![(key.clone(), SeatMask::SHARED)],
            logons: vec![],
            admins_bypass: true,
            class_defaults: vec![DeviceClassTarget::Ports],
            printer_hosts: vec![],
        };
        let r = g.apply(&plan).unwrap();
        assert!(r.failed.is_empty(), "{:?}", r.failed);
        assert_eq!(sys.sddl.lock().unwrap().get(PORT).map(String::as_str), Some(OPEN_SDDL), "shared port stays open");
        assert_eq!(sys.class_sddl.lock().unwrap().get(class::PORTS).map(String::as_str), Some("D:P(A;;GA;;;SY)(A;;GA;;;BA)"));
        // The removable devnode this gate classifies is restarted so the open value applies.
        assert_eq!(sys.restarts.lock().unwrap().as_slice(), [PORT]);
        // Idempotent.
        g.apply(&plan).unwrap();
        assert_eq!(sys.restarts.lock().unwrap().len(), 1);
        // The port goes to seat 2.
        let owned = GatePlan { devices: vec![(key.clone(), SeatMask::from_seats([seat(2)]))], ..plan.clone() };
        g.apply(&owned).unwrap();
        assert_eq!(sys.sddl.lock().unwrap().get(PORT).map(String::as_str), Some("D:P(A;;GA;;;SY)(A;;GA;;;BA)"));
        // Feature off: class restored first, and the port's record now holds the true
        // original (none).
        let off = GatePlan { class_defaults: vec![], ..owned.clone() };
        g.apply(&off).unwrap();
        assert_eq!(sys.class_sddl.lock().unwrap().get(class::PORTS).map(String::as_str), Some("D:(A;;GA;;;WD)"));
        assert_eq!(g.journal.get(PORT).unwrap().original_sddl, None);
        assert!(g.class.is_empty());
        g.revert_all().unwrap();
        assert!(sys.sddl.lock().unwrap().is_empty());
    }

    #[test]
    fn class_defaults_refuse_disk_and_volume_classes() {
        use crate::inventory::class;
        use seat_core::backend::gate::DeviceClassTarget;
        let sys = Arc::new(FakeSys::default());
        let g = gate(&sys);
        let internal = r"SCSI\DISK&VEN_NVME\1&0";
        let plan = GatePlan {
            class_defaults: vec![DeviceClassTarget::DiskDrive, DeviceClassTarget::Volume],
            admins_bypass: true,
            ..Default::default()
        };
        let r = g.apply(&plan).unwrap();
        let codes: Vec<&str> = r.failed.iter().map(|(_, e)| e.reason.as_str()).collect();
        assert_eq!(codes, ["class_not_supported", "class_not_supported"]);
        // No class value and no devnode value written: the system disk keeps the stack's
        // own (admin-only raw) access.
        assert!(sys.class_sddl.lock().unwrap().is_empty());
        assert!(!sys.sddl.lock().unwrap().contains_key(internal));
        assert!(!sys.class_sddl.lock().unwrap().contains_key(class::DISK_DRIVE));
        assert!(g.class.is_empty());
    }

    #[test]
    fn class_defaults_release_records_of_refused_classes() {
        use crate::gate::classdefault::{ClassRecord, OPEN_SDDL, OpenRecord};
        use crate::inventory::class;
        use seat_core::backend::gate::DeviceClassTarget;
        // An older build closed the disk class and opened the internal disk.
        let sys = Arc::new(FakeSys::default());
        let internal = r"SCSI\DISK&VEN_NVME\1&0";
        sys.class_sddl.lock().unwrap().insert(class::DISK_DRIVE.into(), "D:P(A;;GA;;;SY)".into());
        sys.sddl.lock().unwrap().insert(internal.into(), OPEN_SDDL.into());
        let g = gate(&sys);
        let (c, o) = g.class.journals();
        c.put(class::DISK_DRIVE, ClassRecord { original_sddl: None, applied_sddl: Some("D:P(A;;GA;;;SY)".into()) }).unwrap();
        o.put(internal, OpenRecord { class_guid: class::DISK_DRIVE.into(), original_sddl: None, applied: true }).unwrap();
        // Still ticked: released anyway.
        g.apply(&GatePlan { class_defaults: vec![DeviceClassTarget::DiskDrive], ..Default::default() }).unwrap();
        assert!(sys.class_sddl.lock().unwrap().is_empty());
        assert!(!sys.sddl.lock().unwrap().contains_key(internal));
        assert!(g.class.is_empty());
    }

    #[test]
    fn class_defaults_leave_a_third_party_value_alone() {
        use crate::gate::classdefault::OPEN_SDDL;
        use seat_core::backend::gate::DeviceClassTarget;
        let sys = Arc::new(FakeSys::default());
        let g = gate(&sys);
        let plan = GatePlan { class_defaults: vec![DeviceClassTarget::Ports], ..Default::default() };
        g.apply(&plan).unwrap();
        assert_eq!(sys.sddl.lock().unwrap().get(PORT).map(String::as_str), Some(OPEN_SDDL));
        // Someone else writes an explicit value afterwards.
        let theirs = "D:P(A;;GA;;;SY)(A;;GR;;;WD)";
        sys.sddl.lock().unwrap().insert(PORT.into(), theirs.into());
        g.apply(&plan).unwrap();
        assert_eq!(sys.sddl.lock().unwrap().get(PORT).map(String::as_str), Some(theirs), "not overwritten");
        g.revert_all().unwrap();
        assert_eq!(sys.sddl.lock().unwrap().get(PORT).map(String::as_str), Some(theirs), "not restored over");
    }

    #[test]
    fn open_descriptors_never_give_disk_or_volume_to_users() {
        use crate::gate::classdefault::open_sddl;
        use seat_core::backend::gate::DeviceClassTarget;
        for t in [DeviceClassTarget::DiskDrive, DeviceClassTarget::Volume] {
            if let Some(s) = open_sddl(t) {
                for sid in ["WD", "AU", "BU", "IU"] {
                    for ace in s.split(')').filter(|a| a.ends_with(&format!(";;;{sid}"))) {
                        assert!(!ace.contains("GW") && !ace.contains("GR") && !ace.contains("GA"), "{t:?}: {ace}");
                    }
                }
            }
        }
    }

    #[test]
    fn raw_open_classes_are_supported_and_disk_paths_never() {
        use super::super::classdefault::{OPEN_SDDL, open_sddl, supported};
        use seat_core::backend::gate::DeviceClassTarget as T;
        for t in [T::Cdrom, T::Ports, T::Modem, T::MultiportSerial, T::Floppy, T::TapeDrive, T::MediumChanger] {
            assert!(supported(t), "{t:?}");
            assert_eq!(open_sddl(t), Some(OPEN_SDDL));
            assert_eq!(t.class_guid().len(), 36);
        }
        for t in [T::DiskDrive, T::Volume] {
            assert!(!supported(t));
        }
    }

    #[test]
    fn class_defaults_revert_all_restores_class() {
        use crate::inventory::class;
        use seat_core::backend::gate::DeviceClassTarget;
        let sys = Arc::new(FakeSys::default());
        let g = gate(&sys);
        let plan = GatePlan { class_defaults: vec![DeviceClassTarget::Ports], admins_bypass: false, ..Default::default() };
        g.apply(&plan).unwrap();
        assert_eq!(sys.class_sddl.lock().unwrap().get(class::PORTS).map(String::as_str), Some("D:P(A;;GA;;;SY)"));
        let (c, o) = g.class.journals();
        assert!(!c.is_empty() && !o.is_empty());
        g.revert_all().unwrap();
        assert!(sys.class_sddl.lock().unwrap().is_empty());
        assert!(sys.sddl.lock().unwrap().is_empty());
        assert!(g.class.is_empty());
    }
}
