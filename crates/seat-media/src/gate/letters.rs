// SPDX-License-Identifier: GPL-3.0-only
//! `session_drive_letters` gate (spec 70 §5.2(3)): a volume owned by exactly one seat loses
//! its global drive letter; the seat agent in the owner's session defines the same letter in
//! that logon session's own DOS-device namespace (`DefineDosDevice` from a non-system
//! caller is per logon session). Shared volumes keep their global letter, and so do volumes
//! owned by seat 1 alone (the console session has no seat agent under the default
//! `seat_one_console` topology; `device_security` still refuses other seats).
//!
//! The service collects the agent work with [`SessionDriveLetterGate::take_agent_tasks`]
//! and sends each as `AgentTaskKind::DefineDriveLetter` / `RemoveDriveLetter` to the
//! session's agent.
//!
//! Remembered letters: removing the global `X:` mount point also makes the mount manager
//! forget it in `HKLM\SYSTEM\MountedDevices`; our journal keeps the letter so the same one
//! is used per session and restored globally on revert. The volume GUID entry is never
//! touched.
//!
//! Known gaps (spec 70 §5.2(3), untested on hardware): the arrival race (the mount manager
//! assigns a letter at arrival, before the service reacts; Explorer / AutoPlay of other
//! seats may see it briefly); UAC split tokens (a letter defined in the filtered logon
//! session is not visible to elevated apps of the same seat); the agent not running.

use super::{ChangeSource, GateCore, VolumeIo, is_shared, logon_of, plan_mask};
use crate::journal::{Journal, JournalLocation};
use seat_core::backend::{
    Availability, BackendError, BackendResult, DeviceGate, ErrorKind, GateBackendKind, GateCaps, GatePlan, GateReport,
    HotplugSink, IsolationStatus,
};
use seat_core::device::{Device, DeviceDetails, DeviceKind};
use seat_core::ids::SeatNumber;
use seat_core::pnp::PnpTree;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

/// Work for a seat agent (maps 1:1 to `seat_proto::AgentTaskKind`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LetterTask {
    pub seat: SeatNumber,
    pub session_id: u32,
    pub action: LetterAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LetterAction {
    /// `DefineDosDevice(DDD_RAW_TARGET_PATH, "<letter>:", raw_target_for_volume(volume))`.
    Define { letter: char, volume: String },
    Remove { letter: char },
}

/// Journal record per volume (`\\?\Volume{GUID}\`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LetterRecord {
    pub key: String,
    pub letter: char,
    /// Owner seat and the session that got the per-session letter (if any).
    pub seat: Option<SeatNumber>,
    pub session_id: Option<u32>,
    /// The `Define` task for `session_id` was queued (after the global letter was gone).
    #[serde(default)]
    pub defined: bool,
}

impl LetterRecord {
    fn same_owner(&self, other: &LetterRecord) -> bool {
        self.seat == other.seat && self.session_id == other.session_id
    }
}

pub struct SessionDriveLetterGate {
    core: GateCore,
    volumes: Arc<dyn VolumeIo>,
    journal: Journal<LetterRecord>,
    tasks: Mutex<Vec<LetterTask>>,
    available: Availability,
}

impl SessionDriveLetterGate {
    pub fn new(pnp: Arc<dyn PnpTree>, volumes: Arc<dyn VolumeIo>, changes: Arc<dyn ChangeSource>, journal: JournalLocation) -> Self {
        Self {
            core: GateCore::new(pnp, vec![DeviceKind::Storage], changes),
            volumes,
            journal: Journal::open(journal),
            tasks: Mutex::new(Vec::new()),
            available: Availability::Available,
        }
    }

    pub fn with_availability(mut self, a: Availability) -> Self {
        self.available = a;
        self
    }

    /// Agent work produced by the last `apply` / `revert_all` calls (drained).
    pub fn take_agent_tasks(&self) -> Vec<LetterTask> {
        std::mem::take(&mut *super::lock(&self.tasks))
    }

    /// Per-session letters each seat should have now (for an agent that (re)connects).
    pub fn letters_for_session(&self, session_id: u32) -> Vec<(char, String)> {
        self.journal
            .all()
            .into_iter()
            .filter(|(_, r)| r.session_id == Some(session_id))
            .map(|(v, r)| (r.letter, v))
            .collect()
    }

    fn push(&self, t: LetterTask) {
        super::lock(&self.tasks).push(t);
    }

    fn put(&self, vol: &str, r: LetterRecord) -> BackendResult<()> {
        self.journal.put(vol, r).map_err(|e| super::io_err("journal_write_failed", e))
    }

    /// Tell the previous session to drop the letter.
    fn retract(&self, r: &LetterRecord) {
        if let (Some(seat), Some(session_id)) = (r.seat, r.session_id) {
            self.push(LetterTask { seat, session_id, action: LetterAction::Remove { letter: r.letter } });
        }
    }

    /// Put the global letter back and forget the record.
    fn restore(&self, vol: &str, r: &LetterRecord, present: bool) -> BackendResult<()> {
        self.retract(r);
        if present {
            if self.volumes.letter_in_use(r.letter) {
                // Someone took the letter meanwhile; leave the volume without one rather
                // than steal it (Windows assigns a free letter at the next arrival).
                self.journal.remove(vol).map_err(|e| super::io_err("journal_write_failed", e))?;
                return Err(BackendError::new(ErrorKind::Os, "letter_in_use", format!("{}: is taken", r.letter)));
            }
            self.volumes.add_global_letter(r.letter, vol)?;
        }
        self.journal.remove(vol).map_err(|e| super::io_err("journal_write_failed", e))
    }

    fn apply_disk(&self, d: &Device, instance_id: &str, plan: &GatePlan, seen: &mut HashSet<String>) -> BackendResult<Option<IsolationStatus>> {
        let mask = plan_mask(plan, &d.key, d.kind);
        let owner = mask.single();
        // Seat 1 alone keeps the global letter: under the default `seat_one_console`
        // topology seat 1 is the console session, where no seat agent runs
        // (architecture.md §3), so a per-session letter would never be defined. Other
        // seats' access is still refused by the `device_security` descriptor.
        let console_only = owner == Some(SeatNumber::CONSOLE);
        let isolate = !is_shared(mask) && mask.count() <= 1 && !console_only;
        let vols = self.volumes.volumes_of(instance_id)?;
        let mut any = false;
        for v in &vols {
            seen.insert(v.name.clone());
            let rec = self.journal.get(&v.name);
            if !isolate {
                if let Some(r) = rec {
                    self.restore(&v.name, &r, true)?;
                }
                continue;
            }
            // Letter to use: remembered one, else the current global one.
            let letter = rec.as_ref().map(|r| r.letter).or_else(|| v.letters.first().copied());
            let Some(letter) = letter else { continue };
            let session = owner.and_then(|s| logon_of(plan, s)).map(|l| l.session_id);
            let mut new = LetterRecord { key: d.key.to_string(), letter, seat: owner, session_id: session, defined: false };
            match &rec {
                Some(r) if r.same_owner(&new) => new.defined = r.defined,
                // Owner or session changed: the old session drops its letter now.
                Some(r) => self.retract(r),
                None => {}
            }
            // Journal first, then the change.
            if rec.as_ref() != Some(&new) {
                self.put(&v.name, new.clone())?;
            }
            for l in &v.letters {
                self.volumes.remove_global_letter(*l)?;
            }
            // Only once the global letter is gone does the owner get its own; a failed
            // removal leaves `defined` false so the next apply queues it again.
            if !new.defined && new.session_id.is_some() {
                self.define(&new, &v.name);
                self.put(&v.name, LetterRecord { defined: true, ..new })?;
            }
            any = true;
        }
        Ok(any.then_some(IsolationStatus::SessionLetter))
    }

    fn define(&self, r: &LetterRecord, vol: &str) {
        if let (Some(seat), Some(session_id)) = (r.seat, r.session_id) {
            self.push(LetterTask { seat, session_id, action: LetterAction::Define { letter: r.letter, volume: vol.to_string() } });
        }
    }
}

impl DeviceGate for SessionDriveLetterGate {
    fn kind(&self) -> GateBackendKind {
        GateBackendKind::SessionDriveLetters
    }

    fn caps(&self) -> GateCaps {
        GateCaps { kinds: vec![DeviceKind::Storage], revokes_handles: false, driver: None }
    }

    fn availability(&self) -> Availability {
        self.available.clone()
    }

    fn enumerate(&self) -> BackendResult<Vec<Device>> {
        // Fill in volumes and letters for the storage records.
        let mut out = self.core.enumerate()?;
        let nodes = self.core.nodes();
        for d in &mut out {
            let Some(id) = nodes.iter().find(|g| g.device.key == d.key).map(|g| g.instance_id.clone()) else { continue };
            if let (DeviceDetails::Storage(s), Ok(vols)) = (&mut d.details, self.volumes.volumes_of(&id)) {
                s.volumes = vols.iter().map(|v| v.name.clone()).collect();
                s.letters = vols.iter().flat_map(|v| v.letters.iter().copied()).collect();
                for v in &vols {
                    if let Some(r) = self.journal.get(&v.name) {
                        if !s.letters.contains(&r.letter) {
                            s.letters.push(r.letter);
                        }
                    }
                }
            }
        }
        Ok(out)
    }

    fn apply(&self, plan: &GatePlan) -> BackendResult<GateReport> {
        if !self.available.is_available() {
            return Err(BackendError::new(ErrorKind::Unsupported, "gate_unavailable", "session_drive_letters is not available"));
        }
        let mut report = GateReport::default();
        let mut seen = HashSet::new();
        for g in self.core.rescan() {
            match self.apply_disk(&g.device, &g.instance_id, plan, &mut seen) {
                Ok(Some(st)) => report.applied.push((g.device.key.clone(), st)),
                Ok(None) => {}
                Err(e) => report.failed.push((g.device.key.clone(), e)),
            }
        }
        // Volumes not present now: the owner's per-session letter points nowhere; retract it
        // but keep the remembered letter for the re-arrival.
        for (vol, r) in self.journal.all() {
            if !seen.contains(&vol) && r.session_id.is_some() {
                self.retract(&r);
                self.put(&vol, LetterRecord { session_id: None, defined: false, ..r })?;
            }
        }
        Ok(report)
    }

    fn revert_all(&self) -> BackendResult<()> {
        let mut first = None;
        let present: HashSet<String> = self
            .core
            .rescan()
            .iter()
            .filter_map(|g| self.volumes.volumes_of(&g.instance_id).ok())
            .flatten()
            .map(|v| v.name)
            .collect();
        for (vol, r) in self.journal.all() {
            if let Err(e) = self.restore(&vol, &r, present.contains(&vol)) {
                log::error!("session_drive_letters: restore of {vol}: {e}");
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
    use crate::gate::security::tests::{FakeSys, VOLNAME, logon, seat};
    use crate::inventory::tests::sample_tree;
    use seat_core::ids::SeatMask;

    fn setup() -> (Arc<FakeSys>, SessionDriveLetterGate, seat_core::DeviceKey) {
        let sys = Arc::new(FakeSys::default());
        sys.letters.lock().unwrap().insert('E', VOLNAME.into());
        let g = SessionDriveLetterGate::new(Arc::new(sample_tree()), sys.clone(), Arc::new(NoChanges), JournalLocation::Memory);
        let key = g.enumerate().unwrap()[0].key.clone();
        (sys, g, key)
    }

    #[test]
    fn single_owner_gets_session_letter() {
        let (sys, g, key) = setup();
        let d = &g.enumerate().unwrap()[0];
        match &d.details {
            DeviceDetails::Storage(s) => assert_eq!(s.letters, vec!['E']),
            x => panic!("{x:?}"),
        }
        let plan = GatePlan { devices: vec![(key.clone(), SeatMask::from_seats([seat(2)]))], logons: vec![logon(2, 5, "S-1-5-5-0-1")], admins_bypass: true, class_defaults: vec![], printer_hosts: vec![] };
        let r = g.apply(&plan).unwrap();
        assert_eq!(r.applied, vec![(key.clone(), IsolationStatus::SessionLetter)]);
        assert!(!sys.letters.lock().unwrap().contains_key(&'E'));
        assert_eq!(
            g.take_agent_tasks(),
            vec![LetterTask { seat: seat(2), session_id: 5, action: LetterAction::Define { letter: 'E', volume: VOLNAME.into() } }]
        );
        assert_eq!(g.letters_for_session(5), vec![('E', VOLNAME.to_string())]);
        // Idempotent.
        g.apply(&plan).unwrap();
        assert!(g.take_agent_tasks().is_empty());
        // Owner logs off and back on in another session.
        let plan2 = GatePlan { logons: vec![logon(2, 9, "S-1-5-5-0-2")], ..plan.clone() };
        g.apply(&plan2).unwrap();
        let t = g.take_agent_tasks();
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].action, LetterAction::Remove { letter: 'E' });
        assert_eq!(t[1].session_id, 9);
        // Shared again → global letter back, agent told to drop its own.
        let shared = GatePlan { devices: vec![(key, SeatMask::SHARED)], ..plan2 };
        assert!(g.apply(&shared).unwrap().applied.is_empty());
        assert_eq!(sys.letters.lock().unwrap().get(&'E').map(String::as_str), Some(VOLNAME));
        assert_eq!(g.take_agent_tasks()[0].action, LetterAction::Remove { letter: 'E' });
    }

    #[test]
    fn failed_removal_defines_on_retry() {
        let (sys, g, key) = setup();
        let plan = GatePlan { devices: vec![(key.clone(), SeatMask::from_seats([seat(2)]))], logons: vec![logon(2, 5, "S-1-5-5-0-1")], admins_bypass: true, class_defaults: vec![], printer_hosts: vec![] };
        *sys.fail_remove_letter.lock().unwrap() = true;
        let r = g.apply(&plan).unwrap();
        assert_eq!(r.failed.len(), 1);
        assert!(g.take_agent_tasks().is_empty(), "no per-session letter while the global one exists");
        *sys.fail_remove_letter.lock().unwrap() = false;
        g.apply(&plan).unwrap();
        assert_eq!(
            g.take_agent_tasks(),
            vec![LetterTask { seat: seat(2), session_id: 5, action: LetterAction::Define { letter: 'E', volume: VOLNAME.into() } }]
        );
        g.apply(&plan).unwrap();
        assert!(g.take_agent_tasks().is_empty());
    }

    #[test]
    fn console_seat_alone_keeps_global_letter() {
        let (sys, g, key) = setup();
        let plan = GatePlan { devices: vec![(key, SeatMask::CONSOLE)], logons: vec![logon(1, 1, "S-1-5-5-0-1")], admins_bypass: true, class_defaults: vec![], printer_hosts: vec![] };
        assert!(g.apply(&plan).unwrap().applied.is_empty());
        assert_eq!(sys.letters.lock().unwrap().get(&'E').map(String::as_str), Some(VOLNAME));
        assert!(g.take_agent_tasks().is_empty());
        assert!(g.journal.is_empty());
    }

    #[test]
    fn revert_and_taken_letter() {
        let (sys, g, key) = setup();
        let plan = GatePlan { devices: vec![(key, SeatMask::from_seats([seat(3)]))], logons: vec![], admins_bypass: true, class_defaults: vec![], printer_hosts: vec![] };
        g.apply(&plan).unwrap();
        assert!(g.take_agent_tasks().is_empty(), "owner not logged on: no agent work");
        sys.letters.lock().unwrap().insert('E', r"\\?\Volume{other}\".into());
        let e = g.revert_all().unwrap_err();
        assert_eq!(e.reason, "letter_in_use");
        assert!(g.journal.is_empty());
    }
}
