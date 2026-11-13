// SPDX-License-Identifier: GPL-3.0-only
//! Device gates (spec 70 §5.2): [`security::DeviceSecurityGate`],
//! [`letters::SessionDriveLetterGate`], [`camera::CameraConsentGate`],
//! [`minifilter::MinifilterGate`].
//!
//! Every gate is written against small I/O traits ([`SecurityIo`], [`VolumeIo`],
//! [`ConsentIo`], [`minifilter::FilterPort`]) so the decision logic is tested with in-memory
//! fakes; the Windows implementations live in `crate::win`. Every gate keeps an undo
//! [`crate::journal::Journal`] and is idempotent: the service re-runs `apply` at service
//! start, at every seat logon / logoff and on hot-plug.

pub mod camera;
pub mod classdefault;
pub mod letters;
pub mod minifilter;
pub mod printer;
pub mod security;

use crate::inventory::{self, GateNode};
use seat_core::backend::{BackendError, BackendResult, ErrorKind, HotplugSink};
use seat_core::device::{Device, DeviceKind};
use seat_core::ids::{DeviceKey, SeatMask, SeatNumber};
use seat_core::pnp::PnpTree;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

pub(crate) fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// "Every seat, including seats added later": exactly [`SeatMask::SHARED`], the same rule
/// as seat-core validation and assignment (and `PSSG_SHARED_MASK` in the minifilter).
/// A mask that lists all 16 seats one by one is *not* shared; it gates to those seats.
pub fn is_shared(mask: SeatMask) -> bool {
    mask.is_shared()
}

/// A logon SID as the plan gives it, accepted only in its strict textual form so nothing
/// else can reach an SDDL string.
pub fn valid_sid(s: &str) -> bool {
    let Some(rest) = s.strip_prefix("S-1-") else { return false };
    !rest.is_empty() && rest.len() < 180 && rest.split('-').all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

/// Security descriptor (SDDL) for a gated devnode (spec 70 §5.2(1)): protected DACL,
/// SYSTEM full, Administrators full when `admins_bypass`, read/write/execute for the
/// logon SID of every logged-on owning seat. Cameras also keep Local Service (the camera
/// frame server opens cameras from session 0 on behalf of apps; without it nobody, not
/// even the owner, could use the camera through Media Foundation, spec 70 §5.2(7)).
pub fn device_sddl(kind: DeviceKind, mask: SeatMask, logons: &[seat_core::backend::gate::SeatLogon], admins_bypass: bool) -> (String, Vec<String>) {
    let mut sids: Vec<String> = logons
        .iter()
        .filter(|l| mask.contains(l.seat) && valid_sid(&l.logon_sid))
        .map(|l| l.logon_sid.clone())
        .collect();
    sids.sort();
    sids.dedup();
    let mut s = String::from("D:P(A;;GA;;;SY)");
    if admins_bypass {
        s.push_str("(A;;GA;;;BA)");
    }
    if kind == DeviceKind::Webcam {
        s.push_str("(A;;GRGWGX;;;LS)");
    }
    for sid in &sids {
        s.push_str(&format!("(A;;GRGWGX;;;{sid})"));
    }
    (s, sids)
}

/// One volume on a disk (from the disk's removal relations + the mount manager).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeRef {
    /// Volume devnode (`STORAGE\Volume\...`).
    pub instance_id: String,
    /// `\\?\Volume{GUID}\`.
    pub name: String,
    /// Global drive letters now (upper case).
    pub letters: Vec<char>,
}

/// Result of invalidating open handles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invalidate {
    Done,
    /// A lock / query-remove was refused (open files); the admin must close them first.
    Vetoed(String),
}

/// Device security descriptor access (SetupAPI) and handle invalidation.
pub trait SecurityIo: Send + Sync {
    /// Current descriptor of the devnode in SDDL (`None` = no explicit descriptor).
    fn get_sddl(&self, instance_id: &str) -> BackendResult<Option<String>>;
    /// Write (or with `None` delete) the explicit descriptor.
    fn set_sddl(&self, instance_id: &str, sddl: Option<&str>) -> BackendResult<()>;
    fn get_characteristics(&self, instance_id: &str) -> BackendResult<Option<u32>>;
    fn set_characteristics(&self, instance_id: &str, value: Option<u32>) -> BackendResult<()>;
    /// Restart the devnode so the new descriptor applies and old handles go (query-remove
    /// honours vetoes).
    fn restart(&self, instance_id: &str) -> BackendResult<Invalidate>;
    /// Class-level default descriptor (`SPCRP_SECURITY_SDS`) of a setup class, SDDL
    /// (`None` = not set). `class_guid` lowercase, no braces.
    fn get_class_sddl(&self, class_guid: &str) -> BackendResult<Option<String>> {
        let _ = class_guid;
        Err(BackendError::unsupported("class descriptors"))
    }
    /// Write (or with `None` delete) the class-level default descriptor.
    fn set_class_sddl(&self, class_guid: &str, sddl: Option<&str>) -> BackendResult<()> {
        let _ = (class_guid, sddl);
        Err(BackendError::unsupported("class descriptors"))
    }
}

/// Volumes and global drive letters (mount manager).
pub trait VolumeIo: Send + Sync {
    fn volumes_of(&self, disk_instance_id: &str) -> BackendResult<Vec<VolumeRef>>;
    /// Lock + dismount (refuses when files are open).
    fn dismount(&self, volume_name: &str) -> BackendResult<Invalidate>;
    fn remove_global_letter(&self, letter: char) -> BackendResult<()>;
    fn add_global_letter(&self, letter: char, volume_name: &str) -> BackendResult<()>;
    fn letter_in_use(&self, letter: char) -> bool;
}

/// Per-user camera consent value (`Allow` / `Deny`, absent = Windows default).
pub trait ConsentIo: Send + Sync {
    fn get(&self, user_sid: &str) -> BackendResult<Option<String>>;
    fn set(&self, user_sid: &str, value: Option<&str>) -> BackendResult<()>;
    /// The user's registry hive is loaded (user logged on somewhere).
    fn hive_loaded(&self, user_sid: &str) -> bool;
}

/// Something that tells a listener "the device tree changed" (CfgMgr32 notifications on
/// Windows). The returned guard keeps the registration alive.
pub trait ChangeSource: Send + Sync {
    fn subscribe(&self, on_change: Arc<dyn Fn() + Send + Sync>) -> Option<Box<dyn std::any::Any + Send + Sync>>;
}

/// A source that never reports changes (tests, non-Windows).
pub struct NoChanges;
impl ChangeSource for NoChanges {
    fn subscribe(&self, _: Arc<dyn Fn() + Send + Sync>) -> Option<Box<dyn std::any::Any + Send + Sync>> {
        None
    }
}

/// Inventory cache + hot-plug fan-out shared by the gates.
pub struct GateCore {
    pnp: Arc<dyn PnpTree>,
    kinds: Vec<DeviceKind>,
    nodes: Mutex<Vec<GateNode>>,
    changes: Arc<dyn ChangeSource>,
    sink: Arc<Mutex<Option<HotplugSink>>>,
    last: Arc<Mutex<BTreeMap<DeviceKey, Device>>>,
    guard: Mutex<Option<Box<dyn std::any::Any + Send + Sync>>>,
}

impl GateCore {
    pub fn new(pnp: Arc<dyn PnpTree>, kinds: Vec<DeviceKind>, changes: Arc<dyn ChangeSource>) -> Self {
        Self {
            pnp,
            kinds,
            nodes: Mutex::new(Vec::new()),
            changes,
            sink: Arc::new(Mutex::new(None)),
            last: Arc::new(Mutex::new(BTreeMap::new())),
            guard: Mutex::new(None),
        }
    }

    pub fn pnp(&self) -> &Arc<dyn PnpTree> {
        &self.pnp
    }

    fn filtered(pnp: &dyn PnpTree, kinds: &[DeviceKind]) -> Vec<GateNode> {
        inventory::scan(pnp).into_iter().filter(|g| kinds.contains(&g.device.kind)).collect()
    }

    /// Re-read the tree and return the classified devnodes of this gate's kinds.
    pub fn rescan(&self) -> Vec<GateNode> {
        if let Err(e) = self.pnp.refresh() {
            log::warn!("gate: device tree refresh failed: {e}");
        }
        let v = Self::filtered(self.pnp.as_ref(), &self.kinds);
        *lock(&self.nodes) = v.clone();
        *lock(&self.last) = inventory::by_key(v.iter().map(|g| g.device.clone()));
        v
    }

    pub fn enumerate(&self) -> BackendResult<Vec<Device>> {
        Ok(self.rescan().into_iter().map(|g| g.device).collect())
    }

    /// Cached nodes (scans once if empty).
    pub fn nodes(&self) -> Vec<GateNode> {
        let cached = lock(&self.nodes).clone();
        if cached.is_empty() { self.rescan() } else { cached }
    }

    pub fn set_hotplug_sink(&self, sink: HotplugSink) {
        *lock(&self.sink) = Some(sink);
        let mut g = lock(&self.guard);
        if g.is_some() {
            return;
        }
        let (pnp, kinds, sink, last) = (self.pnp.clone(), self.kinds.clone(), self.sink.clone(), self.last.clone());
        let cb: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            let _ = pnp.refresh();
            let now = inventory::by_key(Self::filtered(pnp.as_ref(), &kinds).into_iter().map(|g| g.device));
            let events = {
                let mut prev = lock(&last);
                let ev = inventory::diff(&prev, &now);
                *prev = now;
                ev
            };
            let s = lock(&sink).clone();
            if let Some(s) = s {
                for e in events {
                    s(e);
                }
            }
        });
        *g = self.changes.subscribe(cb);
    }
}

/// Effective mask of a key in the plan; absent keys use the kind default (shared for every
/// kind a gate handles).
pub fn plan_mask(plan: &seat_core::backend::GatePlan, key: &DeviceKey, kind: DeviceKind) -> SeatMask {
    plan.devices.iter().find(|(k, _)| k == key).map(|(_, m)| *m).unwrap_or(kind.default_mask())
}

pub fn logon_of(plan: &seat_core::backend::GatePlan, seat: SeatNumber) -> Option<&seat_core::backend::gate::SeatLogon> {
    plan.logons.iter().find(|l| l.seat == seat)
}

pub(crate) fn io_err(reason: &str, e: std::io::Error) -> BackendError {
    BackendError::new(ErrorKind::Os, reason, e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use seat_core::backend::gate::SeatLogon;

    fn s(n: u8) -> SeatNumber {
        SeatNumber::new(n).unwrap()
    }

    #[test]
    fn sddl_building() {
        let logons = vec![
            SeatLogon { seat: s(2), session_id: 3, logon_sid: "S-1-5-5-0-111".into(), user_sid: "S-1-5-21-1-2-3-1001".into() },
            SeatLogon { seat: s(3), session_id: 4, logon_sid: "S-1-5-5-0-222".into(), user_sid: "S-1-5-21-1-2-3-1002".into() },
            SeatLogon { seat: s(4), session_id: 5, logon_sid: "S-1-5-5-0-1)(A;;GA;;;WD".into(), user_sid: "x".into() },
        ];
        let (sddl, sids) = device_sddl(DeviceKind::Storage, SeatMask::from_seats([s(2), s(4)]), &logons, true);
        assert_eq!(sddl, "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGWGX;;;S-1-5-5-0-111)");
        assert_eq!(sids, vec!["S-1-5-5-0-111".to_string()]);
        let (sddl, _) = device_sddl(DeviceKind::Webcam, SeatMask::SPARE, &logons, false);
        assert_eq!(sddl, "D:P(A;;GA;;;SY)(A;;GRGWGX;;;LS)");
        assert!(is_shared(SeatMask::SHARED));
        assert!(!is_shared(SeatMask::CONSOLE));
        assert!(!is_shared(SeatMask(0x0000_FFFF)));
        assert!(!is_shared(SeatMask(0xFFFE_FFFF)));
        // The minifilter header pins the same rule.
        let h = include_str!("../../../../drivers/seat-storage-filter/inc/seat_storage_gate.h");
        assert!(h.contains("#define PSSG_SHARED_MASK 0xFFFFFFFFu"), "PSSG_SHARED_MASK out of step");
        assert!(!h.contains("PSSG_SHARED_BITS"));
        assert_eq!(SeatMask::SHARED.0, 0xFFFF_FFFF);
        assert!(valid_sid("S-1-5-5-0-123"));
        assert!(!valid_sid("S-1-5-5-0-1)"));
    }
}
