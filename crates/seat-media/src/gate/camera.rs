// SPDX-License-Identifier: GPL-3.0-only
//! `camera_consent` gate (spec 70 §5.2(7)): a coarse "this seat has no camera" switch.
//!
//! Media Foundation opens cameras through the frame server in session 0, so a device
//! descriptor cannot tell seats apart there. For a seat that owns no camera while other
//! seats do, the per-user camera privacy setting of the seat's user is set to `Deny`; the
//! user's previous value (or "absent") is journaled and restored when the seat gets a
//! camera again, at revert, or at the next apply after the user's hive is loaded again.
//!
//! Limits (documented in the manager): per user, not per seat — when one account is signed
//! in at two seats with different answers the gate fails open (`Allow`) and reports
//! `same_account_conflict`; apps that open a camera directly (DirectShow / KS) may ignore
//! the setting (HYPOTHESIS); direct opens are gated by `device_security` instead.
//!
//! Registry location (per user, documented Windows privacy setting store):
//! `HKU\<user SID>\Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\webcam`,
//! value `Value` (`Allow` / `Deny`). Untested on hardware.

use super::{ChangeSource, ConsentIo, GateCore, is_shared, plan_mask, valid_sid};
use crate::journal::{Journal, JournalLocation};
use seat_core::backend::{
    Availability, BackendError, BackendResult, DeviceGate, ErrorKind, GateBackendKind, GateCaps, GatePlan, GateReport,
    HotplugSink, IsolationStatus,
};
use seat_core::device::{Device, DeviceKind};
use seat_core::ids::SeatNumber;
use seat_core::pnp::PnpTree;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;

pub const CONSENT_SUBKEY: &str = r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\webcam";
pub const CONSENT_VALUE: &str = "Value";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsentRecord {
    pub original: Option<String>,
}

pub struct CameraConsentGate {
    core: GateCore,
    io: Arc<dyn ConsentIo>,
    journal: Journal<ConsentRecord>,
    available: Availability,
}

impl CameraConsentGate {
    pub fn new(pnp: Arc<dyn PnpTree>, io: Arc<dyn ConsentIo>, changes: Arc<dyn ChangeSource>, journal: JournalLocation) -> Self {
        Self {
            core: GateCore::new(pnp, vec![DeviceKind::Webcam], changes),
            io,
            journal: Journal::open(journal),
            available: Availability::Available,
        }
    }

    pub fn with_availability(mut self, a: Availability) -> Self {
        self.available = a;
        self
    }

    fn deny(&self, user: &str) -> BackendResult<()> {
        if self.journal.get(user).is_none() {
            let original = self.io.get(user)?;
            self.journal.put(user, ConsentRecord { original }).map_err(|e| super::io_err("journal_write_failed", e))?;
        }
        self.io.set(user, Some("Deny"))
    }

    fn allow(&self, user: &str) -> BackendResult<()> {
        if let Some(r) = self.journal.get(user) {
            self.io.set(user, r.original.as_deref())?;
            self.journal.remove(user).map_err(|e| super::io_err("journal_write_failed", e))?;
        }
        Ok(())
    }
}

impl DeviceGate for CameraConsentGate {
    fn kind(&self) -> GateBackendKind {
        GateBackendKind::CameraConsent
    }

    fn caps(&self) -> GateCaps {
        GateCaps { kinds: vec![DeviceKind::Webcam], revokes_handles: false, driver: None }
    }

    fn availability(&self) -> Availability {
        self.available.clone()
    }

    fn enumerate(&self) -> BackendResult<Vec<Device>> {
        self.core.enumerate()
    }

    fn apply(&self, plan: &GatePlan) -> BackendResult<GateReport> {
        if !self.available.is_available() {
            return Err(BackendError::new(ErrorKind::Unsupported, "gate_unavailable", "camera_consent is not available"));
        }
        let mut report = GateReport::default();
        let cams: Vec<(seat_core::DeviceKey, seat_core::SeatMask)> = self
            .core
            .rescan()
            .into_iter()
            .filter(|g| g.device.present)
            .map(|g| {
                let m = plan_mask(plan, &g.device.key, DeviceKind::Webcam);
                (g.device.key, m)
            })
            .collect();
        for (k, m) in &cams {
            if !is_shared(*m) {
                report.applied.push((k.clone(), IsolationStatus::CameraConsent));
            }
        }
        let seat_has_camera = |s: SeatNumber| cams.is_empty() || cams.iter().any(|(_, m)| m.contains(s));

        // user SID -> decisions of its seats.
        let mut users: BTreeMap<String, Vec<bool>> = BTreeMap::new();
        for l in &plan.logons {
            if valid_sid(&l.user_sid) {
                users.entry(l.user_sid.clone()).or_default().push(!seat_has_camera(l.seat));
            }
        }
        for (user, denies) in &users {
            let all_deny = denies.iter().all(|d| *d);
            let conflict = !all_deny && denies.iter().any(|d| *d);
            let r = if all_deny { self.deny(user) } else { self.allow(user) };
            if conflict {
                if let Some((k, _)) = cams.first() {
                    report.failed.push((
                        k.clone(),
                        BackendError::new(ErrorKind::Policy, "same_account_conflict", "one account on seats with and without a camera"),
                    ));
                }
            }
            if let Err(e) = r {
                if let Some((k, _)) = cams.first() {
                    report.failed.push((k.clone(), e));
                }
            }
        }
        // Users no longer signed in anywhere: restore when their hive is loaded.
        for (user, _) in self.journal.all() {
            if !users.contains_key(&user) && self.io.hive_loaded(&user) {
                if let Err(e) = self.allow(&user) {
                    log::warn!("camera_consent: restore for a signed-out user failed: {e}");
                }
            }
        }
        Ok(report)
    }

    fn revert_all(&self) -> BackendResult<()> {
        let mut first = None;
        for (user, _) in self.journal.all() {
            if !self.io.hive_loaded(&user) {
                // Keep the record; the service retries at the user's next logon.
                first.get_or_insert(BackendError::new(ErrorKind::NotReady, "user_hive_not_loaded", "restore pending until the user signs in"));
                continue;
            }
            if let Err(e) = self.allow(&user) {
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
    use crate::inventory::tests::sample_tree;
    use seat_core::ids::SeatMask;
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeConsent(Mutex<HashMap<String, String>>);
    impl ConsentIo for FakeConsent {
        fn get(&self, u: &str) -> BackendResult<Option<String>> {
            Ok(self.0.lock().unwrap().get(u).cloned())
        }
        fn set(&self, u: &str, v: Option<&str>) -> BackendResult<()> {
            let mut m = self.0.lock().unwrap();
            match v {
                Some(v) => m.insert(u.into(), v.into()),
                None => m.remove(u),
            };
            Ok(())
        }
        fn hive_loaded(&self, _: &str) -> bool {
            true
        }
    }

    #[test]
    fn seat_without_camera_is_denied_and_restored() {
        let io = Arc::new(FakeConsent::default());
        io.0.lock().unwrap().insert("S-1-5-21-9-3".into(), "Allow".into());
        let g = CameraConsentGate::new(Arc::new(sample_tree()), io.clone(), Arc::new(NoChanges), JournalLocation::Memory);
        let cam = g.enumerate().unwrap()[0].key.clone();
        let plan = GatePlan {
            devices: vec![(cam.clone(), SeatMask::from_seats([seat(2)]))],
            logons: vec![logon(2, 4, "S-1-5-5-0-1"), logon(3, 5, "S-1-5-5-0-2")],
            admins_bypass: true,
            class_defaults: vec![],
            printer_hosts: vec![],
        };
        let r = g.apply(&plan).unwrap();
        assert_eq!(r.applied, vec![(cam.clone(), IsolationStatus::CameraConsent)]);
        assert_eq!(io.0.lock().unwrap().get("S-1-5-21-9-3").map(String::as_str), Some("Deny"));
        assert!(!io.0.lock().unwrap().contains_key("S-1-5-21-9-2"));
        // Camera shared again → original value back.
        let shared = GatePlan { devices: vec![(cam, SeatMask::SHARED)], ..plan.clone() };
        g.apply(&shared).unwrap();
        assert_eq!(io.0.lock().unwrap().get("S-1-5-21-9-3").map(String::as_str), Some("Allow"));
        // Same account on seats 2 and 3 → fail open + conflict.
        let mut same = plan.clone();
        same.logons[1].user_sid = same.logons[0].user_sid.clone();
        let r = g.apply(&same).unwrap();
        assert_eq!(r.failed[0].1.reason, "same_account_conflict");
        g.revert_all().unwrap();
        assert!(g.journal.is_empty());
    }
}
