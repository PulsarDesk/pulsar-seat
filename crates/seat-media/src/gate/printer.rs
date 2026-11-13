// SPDX-License-Identifier: GPL-3.0-only
//! `printer_acl` gate (spec 70 §5.2(5), optional): the print queues of a seat-owned USB
//! printer get a printer security descriptor that lets only the owning seats' logon SIDs
//! print (SYSTEM keeps full access: the spooler opens the USB device from session 0, which
//! is correct).
//!
//! - Mapping devnode → queues: the USB printing-support interface of the printer's USB
//!   function names its port (`USB001`, ...); every local queue whose port list has that
//!   port belongs to the device ([`PrinterIo::queues_for_device`]).
//! - Lifecycle: logon SIDs change at every logon, so `apply` (run at service start, every
//!   seat logon / logoff and hot-plug) rewrites the DACL from the current logons; stale
//!   SIDs of a crashed run go at the next start. Every queue's previous DACL is journaled
//!   (`gate-printer-acl.json`) before the first write and restored when the printer becomes
//!   shared again, on `revert_all`, and for queues whose device is gone and no longer owned.
//! - Limit: per-seat **use**, not per-seat visibility; other seats may still see the queue
//!   in lists (HYPOTHESIS, spec 70 §5.2(5)). Same-account seats cannot be separated by user
//!   SID; we use logon SIDs, which differ per logon.
//!
//! **Untested on hardware.** HYPOTHESIS: the spooler checks the print-use right against
//! the caller's token including its logon SID group.

use super::{ChangeSource, GateCore, io_err, is_shared, plan_mask, valid_sid};
use crate::journal::{Journal, JournalLocation};
use seat_core::backend::gate::SeatLogon;
use seat_core::backend::{
    Availability, BackendError, BackendResult, DeviceGate, ErrorKind, GateBackendKind, GateCaps, GatePlan, GateReport,
    HotplugSink, IsolationStatus,
};
use seat_core::device::{Device, DeviceKind};
use seat_core::ids::SeatMask;
use seat_core::pnp::PnpTree;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;

/// `PRINTER_ALL_ACCESS` (winspool.h).
pub const PRINTER_ALL_ACCESS: u32 = 0x000F_000C;
/// `JOB_ALL_ACCESS` (winspool.h), inherited to jobs.
pub const JOB_ALL_ACCESS: u32 = 0x000F_0030;
/// `PRINTER_ACCESS_USE | READ_CONTROL`.
pub const PRINTER_USE: u32 = 0x0002_0008;
/// `READ_CONTROL | WRITE_DAC | WRITE_OWNER`: lets Administrators repair the ACL even when
/// they do not bypass seat isolation.
pub const ACL_REPAIR: u32 = 0x000E_0000;

/// Print queues and their descriptors (spooler API on Windows).
pub trait PrinterIo: Send + Sync {
    /// Local queues printing to this devnode (USB printer function or its parent).
    fn queues_for_device(&self, instance_id: &str) -> BackendResult<Vec<String>>;
    /// DACL of a queue in SDDL (`Ok(None)` = the queue does not exist).
    fn get_dacl(&self, queue: &str) -> BackendResult<Option<String>>;
    fn set_dacl(&self, queue: &str, sddl: &str) -> BackendResult<()>;
}

/// Printer DACL for a seat-owned printer.
pub fn printer_sddl(mask: SeatMask, logons: &[SeatLogon], admins_bypass: bool) -> String {
    let mut sids: Vec<&str> =
        logons.iter().filter(|l| mask.contains(l.seat) && valid_sid(&l.logon_sid)).map(|l| l.logon_sid.as_str()).collect();
    sids.sort();
    sids.dedup();
    let mut s = format!("D:P(A;;{PRINTER_ALL_ACCESS:#x};;;SY)(A;OIIO;{JOB_ALL_ACCESS:#x};;;SY)");
    if admins_bypass {
        s.push_str(&format!("(A;;{PRINTER_ALL_ACCESS:#x};;;BA)(A;OIIO;{JOB_ALL_ACCESS:#x};;;BA)"));
    } else {
        s.push_str(&format!("(A;;{ACL_REPAIR:#x};;;BA)"));
    }
    // The submitter manages its own jobs.
    s.push_str(&format!("(A;OIIO;{JOB_ALL_ACCESS:#x};;;CO)"));
    for sid in sids {
        s.push_str(&format!("(A;;{PRINTER_USE:#x};;;{sid})"));
    }
    s
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrinterRecord {
    /// Device key of the printer the queue belongs to.
    pub key: String,
    pub original_dacl: String,
    #[serde(default)]
    pub applied_dacl: Option<String>,
}

pub struct PrinterAclGate {
    core: GateCore,
    io: Arc<dyn PrinterIo>,
    journal: Journal<PrinterRecord>,
    available: Availability,
}

impl PrinterAclGate {
    pub fn new(pnp: Arc<dyn PnpTree>, io: Arc<dyn PrinterIo>, changes: Arc<dyn ChangeSource>, journal: JournalLocation) -> Self {
        Self { core: GateCore::new(pnp, vec![DeviceKind::Printer], changes), io, journal: Journal::open(journal), available: Availability::Available }
    }

    pub fn with_availability(mut self, a: Availability) -> Self {
        self.available = a;
        self
    }

    fn put(&self, q: &str, r: PrinterRecord) -> BackendResult<()> {
        self.journal.put(q, r).map_err(|e| io_err("journal_write_failed", e))
    }

    fn write(&self, key: &str, queue: &str, sddl: &str) -> BackendResult<()> {
        let rec = self.journal.get(queue);
        if rec.as_ref().is_some_and(|r| r.applied_dacl.as_deref() == Some(sddl)) {
            return Ok(());
        }
        let mut rec = match rec {
            Some(r) => PrinterRecord { applied_dacl: None, key: key.to_string(), ..r },
            None => {
                let Some(orig) = self.io.get_dacl(queue)? else {
                    return Err(BackendError::new(ErrorKind::NotFound, "printer_not_found", queue.to_string()));
                };
                PrinterRecord { key: key.to_string(), original_dacl: orig, applied_dacl: None }
            }
        };
        self.put(queue, rec.clone())?;
        self.io.set_dacl(queue, sddl)?;
        rec.applied_dacl = Some(sddl.to_string());
        self.put(queue, rec)
    }

    fn restore(&self, queue: &str) -> BackendResult<()> {
        let Some(rec) = self.journal.get(queue) else { return Ok(()) };
        match self.io.get_dacl(queue)? {
            // Queue deleted: nothing to restore.
            None => {}
            Some(_) => self.io.set_dacl(queue, &rec.original_dacl)?,
        }
        self.journal.remove(queue).map_err(|e| io_err("journal_write_failed", e))
    }
}

impl DeviceGate for PrinterAclGate {
    fn kind(&self) -> GateBackendKind {
        GateBackendKind::PrinterAcl
    }

    fn caps(&self) -> GateCaps {
        GateCaps { kinds: vec![DeviceKind::Printer], revokes_handles: false, driver: None }
    }

    fn availability(&self) -> Availability {
        self.available.clone()
    }

    fn enumerate(&self) -> BackendResult<Vec<Device>> {
        self.core.enumerate()
    }

    fn apply(&self, plan: &GatePlan) -> BackendResult<GateReport> {
        if !self.available.is_available() {
            return Err(BackendError::new(ErrorKind::Unsupported, "gate_unavailable", "printer_acl is not available"));
        }
        let mut report = GateReport::default();
        let mut handled: HashSet<String> = HashSet::new();
        for g in self.core.rescan() {
            let d = &g.device;
            let mask = plan_mask(plan, &d.key, d.kind);
            let queues = match self.io.queues_for_device(&g.instance_id) {
                Ok(q) => q,
                Err(e) => {
                    report.failed.push((d.key.clone(), e));
                    continue;
                }
            };
            handled.extend(queues.iter().map(|q| q.to_ascii_lowercase()));
            if queues.is_empty() {
                // No queue installed for it (yet): nothing to gate.
                let st = if is_shared(mask) { IsolationStatus::Shared } else { IsolationStatus::NotSupported };
                report.applied.push((d.key.clone(), st));
                continue;
            }
            let mut err = None;
            for q in &queues {
                let r = if is_shared(mask) {
                    self.restore(q)
                } else {
                    self.write(d.key.as_str(), q, &printer_sddl(mask, &plan.logons, plan.admins_bypass))
                };
                if let Err(e) = r {
                    err.get_or_insert(e);
                }
            }
            match err {
                Some(e) => report.failed.push((d.key.clone(), e)),
                None => {
                    let st = if is_shared(mask) { IsolationStatus::Shared } else { IsolationStatus::Descriptor };
                    report.applied.push((d.key.clone(), st));
                }
            }
        }
        // Queues of printers that are not plugged in now: keep them closed while the plan
        // still gives the printer to seats (with today's logons, so stale SIDs go), restore
        // them otherwise.
        for (q, rec) in self.journal.all() {
            if handled.contains(&q.to_ascii_lowercase()) {
                continue;
            }
            let mask = seat_core::DeviceKey::new(rec.key.clone())
                .ok()
                .and_then(|k| plan.devices.iter().find(|(pk, _)| *pk == k).map(|(_, m)| *m))
                .unwrap_or(SeatMask::SHARED);
            let r = if is_shared(mask) {
                self.restore(&q)
            } else {
                self.write(&rec.key, &q, &printer_sddl(mask, &plan.logons, plan.admins_bypass))
            };
            if let Err(e) = r {
                log::warn!("printer_acl: queue {q}: {e}");
            }
        }
        Ok(report)
    }

    fn revert_all(&self) -> BackendResult<()> {
        let mut first = None;
        for (q, _) in self.journal.all() {
            if let Err(e) = self.restore(&q) {
                log::error!("printer_acl: restore of {q} failed: {e}");
                first.get_or_insert(e);
            }
        }
        first.map_or(Ok(()), Err)
    }

    fn set_hotplug_sink(&self, sink: HotplugSink) {
        self.core.set_hotplug_sink(sink);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gate::NoChanges;
    use crate::gate::security::tests::{logon, seat};
    use crate::inventory::{ROOT_DEVNODE, class};
    use seat_core::mock::MockPnpTree;
    use seat_core::pnp::PnpNode;
    use std::collections::HashMap;
    use std::sync::Mutex;

    const PRN: &str = r"USBPRINT\TESTPRINTER\7&1";

    fn tree() -> MockPnpTree {
        let t = MockPnpTree::default();
        t.add(PnpNode { instance_id: ROOT_DEVNODE.into(), enumerator: "HTREE".into(), present: true, ..Default::default() });
        t.add(PnpNode {
            instance_id: r"USB\ROOT_HUB30\4".into(),
            parent: Some(ROOT_DEVNODE.into()),
            hardware_ids: vec![r"USB\ROOT_HUB30".into()],
            location_paths: vec!["PCIROOT(0)#USBROOT(0)".into()],
            enumerator: "USB".into(),
            present: true,
            ..Default::default()
        });
        t.add(PnpNode {
            instance_id: r"USB\VID_1234&PID_0001\SN".into(),
            parent: Some(r"USB\ROOT_HUB30\4".into()),
            location_paths: vec!["PCIROOT(0)#USBROOT(0)#USB(3)".into()],
            enumerator: "USB".into(),
            present: true,
            ..Default::default()
        });
        t.add(PnpNode {
            instance_id: PRN.into(),
            parent: Some(r"USB\VID_1234&PID_0001\SN".into()),
            class_guid: Some(class::PRINTER.into()),
            enumerator: "USBPRINT".into(),
            present: true,
            ..Default::default()
        });
        t
    }

    #[derive(Default)]
    struct FakeSpooler {
        dacl: Mutex<HashMap<String, String>>,
    }

    impl PrinterIo for FakeSpooler {
        fn queues_for_device(&self, id: &str) -> BackendResult<Vec<String>> {
            Ok(if id == PRN { vec!["Laser".into(), "Laser (kopya 1)".into()] } else { vec![] })
        }
        fn get_dacl(&self, q: &str) -> BackendResult<Option<String>> {
            Ok(self.dacl.lock().unwrap().get(q).cloned())
        }
        fn set_dacl(&self, q: &str, s: &str) -> BackendResult<()> {
            self.dacl.lock().unwrap().insert(q.into(), s.into());
            Ok(())
        }
    }

    const ORIG: &str = "D:(A;;0x20008;;;WD)";

    #[test]
    fn owned_printer_rewritten_per_logon_and_restored() {
        let io = Arc::new(FakeSpooler::default());
        for q in ["Laser", "Laser (kopya 1)"] {
            io.dacl.lock().unwrap().insert(q.into(), ORIG.into());
        }
        let g = PrinterAclGate::new(Arc::new(tree()), io.clone(), Arc::new(NoChanges), JournalLocation::Memory);
        let key = g.enumerate().unwrap()[0].key.clone();
        let plan = GatePlan {
            devices: vec![(key.clone(), SeatMask::from_seats([seat(2)]))],
            logons: vec![logon(2, 3, "S-1-5-5-0-10"), logon(3, 4, "S-1-5-5-0-11")],
            admins_bypass: true,
            class_defaults: vec![],
        };
        let r = g.apply(&plan).unwrap();
        assert_eq!(r.applied, vec![(key.clone(), IsolationStatus::Descriptor)]);
        let d = io.dacl.lock().unwrap().get("Laser").cloned().unwrap();
        assert!(d.contains("(A;;0x20008;;;S-1-5-5-0-10)"), "{d}");
        assert!(!d.contains("S-1-5-5-0-11"));
        // New logon of seat 2: the old SID goes.
        let next = GatePlan { logons: vec![logon(2, 7, "S-1-5-5-0-12")], ..plan.clone() };
        g.apply(&next).unwrap();
        let d = io.dacl.lock().unwrap().get("Laser (kopya 1)").cloned().unwrap();
        assert!(d.contains("S-1-5-5-0-12") && !d.contains("S-1-5-5-0-10"), "{d}");
        // Shared again: originals back, journal empty.
        let shared = GatePlan { devices: vec![(key, SeatMask::SHARED)], ..next };
        g.apply(&shared).unwrap();
        assert_eq!(io.dacl.lock().unwrap().get("Laser").map(String::as_str), Some(ORIG));
        assert!(g.journal.is_empty());
    }

    #[test]
    fn unplugged_owned_printer_stays_closed_then_revert_all() {
        let io = Arc::new(FakeSpooler::default());
        io.dacl.lock().unwrap().insert("Old".into(), ORIG.into());
        let g = PrinterAclGate::new(Arc::new(tree()), io.clone(), Arc::new(NoChanges), JournalLocation::Memory);
        g.put("Old", PrinterRecord { key: "prn;gone".into(), original_dacl: ORIG.into(), applied_dacl: Some("D:P(A;;GA;;;SY)".into()) })
            .unwrap();
        let k = seat_core::DeviceKey::new("prn;gone").unwrap();
        let plan = GatePlan { devices: vec![(k, SeatMask::from_seats([seat(2)]))], logons: vec![], admins_bypass: false, ..Default::default() };
        g.apply(&plan).unwrap();
        let d = io.dacl.lock().unwrap().get("Old").cloned().unwrap();
        assert!(d.contains("(A;;0xe0000;;;BA)"), "{d}");
        g.revert_all().unwrap();
        assert_eq!(io.dacl.lock().unwrap().get("Old").map(String::as_str), Some(ORIG));
        // No record and no plan entry: never touched.
        g.apply(&GatePlan::default()).unwrap();
        assert!(g.journal.is_empty());
    }

    #[test]
    fn sddl_shape() {
        let s = printer_sddl(SeatMask::from_seats([seat(2)]), &[logon(2, 1, "S-1-5-5-0-9)(A;;GA;;;WD")], true);
        assert!(!s.contains("WD"), "invalid SIDs never reach the SDDL: {s}");
        assert!(s.starts_with("D:P(A;;0xf000c;;;SY)"));
    }
}
