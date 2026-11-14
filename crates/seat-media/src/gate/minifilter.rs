// SPDX-License-Identifier: GPL-3.0-only
//! `storage_minifilter` gate: client of our own Filter Manager minifilter
//! (`drivers/seat-storage-filter`). **Exception**: a minifilter is outside the rules.md
//! driver list and needs the maintainer's explicit OK, an allocated altitude and a signed
//! build (spec 70 §5.2(4), §5.3). Until then the port does not exist and the gate reports
//! [`Availability::DriverNotSigned`]; nothing here loads or installs a driver.
//!
//! The wire format is [`proto`], mirrored by `drivers/seat-storage-filter/inc/seat_storage_gate.h`
//! (keep both in step; a new major gets a new port name).

use super::{ChangeSource, GateCore, VolumeIo, is_shared, plan_mask};
use crate::keys::volume_guid;
use seat_core::backend::{
    Availability, BackendError, BackendResult, DeviceGate, DriverId, ErrorKind, GateBackendKind, GateCaps, GatePlan,
    GateReport, HotplugSink, IsolationStatus,
};
use seat_core::device::{Device, DeviceKind};
use seat_core::pnp::PnpTree;
use std::sync::{Arc, Mutex};

/// Wire protocol of the communication port (little endian, packed, no padding).
pub mod proto {
    /// `'PSSG'` read as a little-endian u32.
    pub const MAGIC: u32 = u32::from_le_bytes(*b"PSSG");
    pub const MINOR: u16 = 0;
    pub const HEADER_LEN: usize = 16;
    pub const VOLUME_GUID_CHARS: usize = 40;
    pub const MAX_SESSIONS: usize = 64;
    pub const MAX_VOLUMES: usize = 256;

    pub const CMD_QUERY_VERSION: u32 = 1;
    pub const CMD_BEGIN_PLAN: u32 = 2;
    pub const CMD_SET_SESSION: u32 = 3;
    pub const CMD_SET_VOLUME: u32 = 4;
    pub const CMD_COMMIT_PLAN: u32 = 5;
    pub const CMD_PASS_ALL: u32 = 6;

    pub const FLAG_ADMINS_BYPASS: u32 = 1;

    fn header(cmd: u32, payload: usize) -> Vec<u8> {
        let mut v = Vec::with_capacity(HEADER_LEN + payload);
        v.extend_from_slice(&MAGIC.to_le_bytes());
        v.extend_from_slice(&seat_core::consts::DRIVER_INTERFACE_MAJOR.to_le_bytes());
        v.extend_from_slice(&MINOR.to_le_bytes());
        v.extend_from_slice(&cmd.to_le_bytes());
        v.extend_from_slice(&((HEADER_LEN + payload) as u32).to_le_bytes());
        v
    }

    pub fn query_version() -> Vec<u8> {
        header(CMD_QUERY_VERSION, 0)
    }
    pub fn begin_plan(admins_bypass: bool) -> Vec<u8> {
        let mut v = header(CMD_BEGIN_PLAN, 4);
        v.extend_from_slice(&(if admins_bypass { FLAG_ADMINS_BYPASS } else { 0 }).to_le_bytes());
        v
    }
    pub fn set_session(session_id: u32, seat_bit: u32) -> Vec<u8> {
        let mut v = header(CMD_SET_SESSION, 8);
        v.extend_from_slice(&session_id.to_le_bytes());
        v.extend_from_slice(&seat_bit.to_le_bytes());
        v
    }
    /// `guid` = `{xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx}` (38 chars), NUL padded to 40.
    /// `None` for anything else: the driver rejects a malformed field with
    /// `STATUS_INVALID_PARAMETER` (gate.c `PssgNormalizeGuid`). Hex is sent lower case.
    pub fn set_volume(mask: u32, guid: &str) -> Option<Vec<u8>> {
        if !is_volume_guid(guid) {
            return None;
        }
        let w: Vec<u16> = guid.to_ascii_lowercase().encode_utf16().collect();
        if w.len() >= VOLUME_GUID_CHARS {
            return None;
        }
        let mut v = header(CMD_SET_VOLUME, 4 + VOLUME_GUID_CHARS * 2);
        v.extend_from_slice(&mask.to_le_bytes());
        for i in 0..VOLUME_GUID_CHARS {
            v.extend_from_slice(&w.get(i).copied().unwrap_or(0).to_le_bytes());
        }
        Some(v)
    }
    /// `{8-4-4-4-12}` hex digits, braces included (38 characters).
    pub fn is_volume_guid(s: &str) -> bool {
        let b = s.as_bytes();
        b.len() == 38
            && b.iter().enumerate().all(|(i, &c)| match i {
                0 => c == b'{',
                37 => c == b'}',
                9 | 14 | 19 | 24 => c == b'-',
                _ => c.is_ascii_hexdigit(),
            })
    }
    pub fn commit_plan() -> Vec<u8> {
        header(CMD_COMMIT_PLAN, 0)
    }
    pub fn pass_all() -> Vec<u8> {
        header(CMD_PASS_ALL, 0)
    }

    /// `(major, minor, build)` from an 8-byte version reply.
    pub fn parse_version(r: &[u8]) -> Option<(u16, u16, u32)> {
        if r.len() < 8 {
            return None;
        }
        Some((u16::from_le_bytes([r[0], r[1]]), u16::from_le_bytes([r[2], r[3]]), u32::from_le_bytes([r[4], r[5], r[6], r[7]])))
    }

    /// NTSTATUS from a 4-byte status reply.
    pub fn parse_status(r: &[u8]) -> Option<i32> {
        (r.len() >= 4).then(|| i32::from_le_bytes([r[0], r[1], r[2], r[3]]))
    }
}

/// Last version reply read from the port in this process (`(major, minor, build)`), for the
/// service's driver status while the gate holds the port's only connection.
pub(crate) static LAST_VERSION: Mutex<Option<(u16, u16, u32)>> = Mutex::new(None);

/// An open connection to the port.
pub trait FilterConn: Send {
    /// Send one message, return the reply bytes (at most `reply_len`).
    fn request(&mut self, msg: &[u8], reply_len: usize) -> BackendResult<Vec<u8>>;
}

/// Opens the port (`FilterConnectCommunicationPort`).
pub trait FilterPort: Send + Sync {
    fn connect(&self) -> BackendResult<Box<dyn FilterConn>>;
}

pub struct MinifilterGate {
    core: GateCore,
    port: Arc<dyn FilterPort>,
    volumes: Arc<dyn VolumeIo>,
    conn: Mutex<Option<Box<dyn FilterConn>>>,
}

fn not_signed() -> BackendError {
    BackendError::new(ErrorKind::Driver, "driver_not_signed", "storage minifilter is not installed (needs signing and the maintainer's OK)")
}

impl MinifilterGate {
    pub fn new(pnp: Arc<dyn PnpTree>, port: Arc<dyn FilterPort>, volumes: Arc<dyn VolumeIo>, changes: Arc<dyn ChangeSource>) -> Self {
        Self { core: GateCore::new(pnp, vec![DeviceKind::Storage], changes), port, volumes, conn: Mutex::new(None) }
    }

    /// Run `f` on a (re)connected port; one reconnect on failure.
    fn with_conn<T>(&self, f: impl Fn(&mut dyn FilterConn) -> BackendResult<T>) -> BackendResult<T> {
        let mut g = super::lock(&self.conn);
        for attempt in 0..2 {
            if g.is_none() {
                let mut c = self.port.connect().map_err(|_| not_signed())?;
                let v = c.request(&proto::query_version(), 8)?;
                let parsed = proto::parse_version(&v);
                if let Some(p) = parsed {
                    *LAST_VERSION.lock().unwrap_or_else(|e| e.into_inner()) = Some(p);
                }
                match parsed {
                    Some((major, _, _)) if major == seat_core::consts::DRIVER_INTERFACE_MAJOR => {}
                    Some((major, _, _)) => {
                        return Err(BackendError::new(ErrorKind::Driver, "driver_version_mismatch", format!("minifilter major {major}")));
                    }
                    None => return Err(BackendError::new(ErrorKind::Driver, "driver_bad_reply", "version reply too short")),
                }
                *g = Some(c);
            }
            let c = g.as_mut().expect("connected above");
            match f(c.as_mut()) {
                Ok(t) => return Ok(t),
                Err(e) if attempt == 0 && e.kind == ErrorKind::Os => {
                    *g = None;
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
        Err(not_signed())
    }

    fn send(c: &mut dyn FilterConn, msg: &[u8]) -> BackendResult<()> {
        let r = c.request(msg, 4)?;
        match proto::parse_status(&r) {
            Some(0) => Ok(()),
            Some(st) => Err(BackendError::new(ErrorKind::Driver, "driver_refused", format!("NTSTATUS {st:#x}")).with_win32(st as u32)),
            None => Err(BackendError::new(ErrorKind::Driver, "driver_bad_reply", "status reply too short")),
        }
    }
}

impl DeviceGate for MinifilterGate {
    fn kind(&self) -> GateBackendKind {
        GateBackendKind::StorageMinifilter
    }

    fn caps(&self) -> GateCaps {
        GateCaps { kinds: vec![DeviceKind::Storage], revokes_handles: false, driver: Some(DriverId::StorageMinifilter) }
    }

    fn availability(&self) -> Availability {
        match self.with_conn(|_| Ok(())) {
            Ok(()) => Availability::Available,
            Err(e) if e.reason == "driver_version_mismatch" => {
                let found = e.message.rsplit(' ').next().and_then(|n| n.parse().ok()).unwrap_or(0);
                Availability::DriverVersionMismatch { driver: DriverId::StorageMinifilter, found }
            }
            Err(_) => Availability::DriverNotSigned { driver: DriverId::StorageMinifilter },
        }
    }

    fn enumerate(&self) -> BackendResult<Vec<Device>> {
        self.core.enumerate()
    }

    fn apply(&self, plan: &GatePlan) -> BackendResult<GateReport> {
        let mut report = GateReport::default();
        let mut vols: Vec<(u32, String)> = Vec::new();
        let full = || BackendError::new(ErrorKind::Policy, "minifilter_plan_full", format!("more than {} gated volumes", proto::MAX_VOLUMES));
        for g in self.core.rescan() {
            let mask = plan_mask(plan, &g.device.key, DeviceKind::Storage);
            if is_shared(mask) {
                continue;
            }
            match self.volumes.volumes_of(&g.instance_id) {
                Ok(vs) => {
                    let guids: Vec<String> = vs.iter().filter_map(|v| volume_guid(&v.name)).collect();
                    // A disk goes into the plan whole or not at all; only what is sent is
                    // reported as isolated.
                    if vols.len() + guids.len() > proto::MAX_VOLUMES {
                        log::warn!("storage_minifilter: plan full, {} not gated", g.device.key);
                        report.failed.push((g.device.key.clone(), full()));
                        continue;
                    }
                    vols.extend(guids.into_iter().map(|guid| (mask.0, guid)));
                    report.applied.push((g.device.key.clone(), IsolationStatus::Minifilter));
                }
                Err(e) => report.failed.push((g.device.key.clone(), e)),
            }
        }
        if plan.logons.len() > proto::MAX_SESSIONS {
            // Sessions past the limit would be refused on their own volumes; say so on
            // every gated disk they own instead of failing silently.
            log::warn!("storage_minifilter: {} logons, only {} fit the plan", plan.logons.len(), proto::MAX_SESSIONS);
            let dropped: Vec<_> = plan.logons[proto::MAX_SESSIONS..].iter().map(|l| l.seat).collect();
            let mut keep = Vec::new();
            for (k, st) in std::mem::take(&mut report.applied) {
                let mask = plan_mask(plan, &k, DeviceKind::Storage);
                if dropped.iter().any(|s| mask.contains(*s)) {
                    report.failed.push((
                        k,
                        BackendError::new(ErrorKind::Policy, "minifilter_plan_full", format!("more than {} seat logons", proto::MAX_SESSIONS)),
                    ));
                } else {
                    keep.push((k, st));
                }
            }
            report.applied = keep;
        }
        let sessions: Vec<(u32, u32)> =
            plan.logons.iter().take(proto::MAX_SESSIONS).map(|l| (l.session_id, l.seat.mask().0)).collect();
        self.with_conn(|c| {
            Self::send(c, &proto::begin_plan(plan.admins_bypass))?;
            for (sid, bit) in &sessions {
                Self::send(c, &proto::set_session(*sid, *bit))?;
            }
            for (mask, guid) in &vols {
                match proto::set_volume(*mask, guid) {
                    Some(m) => Self::send(c, &m)?,
                    // Not a volume GUID: the driver would reject it, so the volume stays
                    // ungated. Never silent.
                    None => log::warn!("storage gate: volume id {guid:?} is not a volume GUID; volume not gated"),
                }
            }
            Self::send(c, &proto::commit_plan())
        })?;
        Ok(report)
    }

    fn revert_all(&self) -> BackendResult<()> {
        match self.with_conn(|c| Self::send(c, &proto::pass_all())) {
            Err(e) if e.reason == "driver_not_signed" => Ok(()),
            r => r,
        }
    }

    fn set_hotplug_sink(&self, sink: HotplugSink) {
        self.core.set_hotplug_sink(sink);
    }
}

/// Port that never exists (non-Windows builds).
pub struct NoPort;
impl FilterPort for NoPort {
    fn connect(&self) -> BackendResult<Box<dyn FilterConn>> {
        Err(not_signed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gate::NoChanges;
    use crate::gate::security::tests::{FakeSys, logon, seat};
    use crate::inventory::tests::sample_tree;
    use seat_core::ids::SeatMask;

    struct FakePort {
        major: u16,
        log: Arc<Mutex<Vec<u32>>>,
    }
    struct FakeConn {
        major: u16,
        log: Arc<Mutex<Vec<u32>>>,
    }
    impl FilterConn for FakeConn {
        fn request(&mut self, msg: &[u8], _: usize) -> BackendResult<Vec<u8>> {
            assert_eq!(&msg[0..4], b"PSSG");
            let len = u32::from_le_bytes(msg[12..16].try_into().unwrap()) as usize;
            assert_eq!(len, msg.len());
            let cmd = u32::from_le_bytes(msg[8..12].try_into().unwrap());
            self.log.lock().unwrap().push(cmd);
            if cmd == proto::CMD_QUERY_VERSION {
                let mut r = self.major.to_le_bytes().to_vec();
                r.extend_from_slice(&0u16.to_le_bytes());
                r.extend_from_slice(&7u32.to_le_bytes());
                return Ok(r);
            }
            Ok(0i32.to_le_bytes().to_vec())
        }
    }
    impl FilterPort for FakePort {
        fn connect(&self) -> BackendResult<Box<dyn FilterConn>> {
            Ok(Box::new(FakeConn { major: self.major, log: self.log.clone() }))
        }
    }

    #[test]
    fn unsigned_driver_reports_not_signed() {
        let sys = Arc::new(FakeSys::default());
        let g = MinifilterGate::new(Arc::new(sample_tree()), Arc::new(NoPort), sys, Arc::new(NoChanges));
        assert_eq!(g.availability(), Availability::DriverNotSigned { driver: DriverId::StorageMinifilter });
        assert_eq!(g.apply(&GatePlan::default()).unwrap_err().reason, "driver_not_signed");
        g.revert_all().unwrap();
    }

    #[test]
    fn plan_is_sent_in_order() {
        let sys = Arc::new(FakeSys::default());
        let log = Arc::new(Mutex::new(Vec::new()));
        let port = Arc::new(FakePort { major: seat_core::consts::DRIVER_INTERFACE_MAJOR, log: log.clone() });
        let g = MinifilterGate::new(Arc::new(sample_tree()), port, sys, Arc::new(NoChanges));
        assert!(g.availability().is_available());
        let key = g.enumerate().unwrap()[0].key.clone();
        let plan = GatePlan { devices: vec![(key.clone(), SeatMask::from_seats([seat(2)]))], logons: vec![logon(2, 3, "S-1-5-5-0-1")], admins_bypass: true, class_defaults: vec![], printer_hosts: vec![] };
        let r = g.apply(&plan).unwrap();
        assert_eq!(r.applied, vec![(key, IsolationStatus::Minifilter)]);
        assert_eq!(*log.lock().unwrap(), vec![1, 2, 3, 4, 5]);
        let mismatch = MinifilterGate::new(
            Arc::new(sample_tree()),
            Arc::new(FakePort { major: 9, log: log.clone() }),
            Arc::new(FakeSys::default()),
            Arc::new(NoChanges),
        );
        assert_eq!(mismatch.availability(), Availability::DriverVersionMismatch { driver: DriverId::StorageMinifilter, found: 9 });
    }

    /// More volumes than fit: the overflowing disk is failed, not reported as isolated.
    #[test]
    fn plan_overflow_is_reported() {
        struct ManyVols;
        impl VolumeIo for ManyVols {
            fn volumes_of(&self, _: &str) -> BackendResult<Vec<crate::gate::VolumeRef>> {
                Ok((0..proto::MAX_VOLUMES + 1)
                    .map(|i| crate::gate::VolumeRef {
                        instance_id: format!(r"STORAGE\VOLUME\{i}"),
                        name: format!(r"\\?\Volume{{{i:08x}-0000-0000-0000-000000000000}}\"),
                        letters: vec![],
                    })
                    .collect())
            }
            fn dismount(&self, _: &str) -> BackendResult<crate::gate::Invalidate> {
                Ok(crate::gate::Invalidate::Done)
            }
            fn remove_global_letter(&self, _: char) -> BackendResult<()> {
                Ok(())
            }
            fn add_global_letter(&self, _: char, _: &str) -> BackendResult<()> {
                Ok(())
            }
            fn letter_in_use(&self, _: char) -> bool {
                false
            }
        }
        let log = Arc::new(Mutex::new(Vec::new()));
        let port = Arc::new(FakePort { major: seat_core::consts::DRIVER_INTERFACE_MAJOR, log: log.clone() });
        let g = MinifilterGate::new(Arc::new(sample_tree()), port, Arc::new(ManyVols), Arc::new(NoChanges));
        let key = g.enumerate().unwrap()[0].key.clone();
        let plan = GatePlan { devices: vec![(key.clone(), SeatMask::from_seats([seat(2)]))], logons: vec![], admins_bypass: true, class_defaults: vec![], printer_hosts: vec![] };
        let r = g.apply(&plan).unwrap();
        assert!(r.applied.is_empty());
        assert_eq!(r.failed[0].1.reason, "minifilter_plan_full");
        assert!(!log.lock().unwrap().contains(&proto::CMD_SET_VOLUME));
    }

    #[test]
    fn message_layout() {
        let m = proto::set_volume(2, "{01234567-89ab-cdef-0123-456789abcdef}").unwrap();
        assert_eq!(m.len(), 16 + 4 + 80);
        assert_eq!(u16::from_le_bytes([m[20], m[21]]), u16::from(b'{'));
        assert!(proto::set_volume(1, &"x".repeat(40)).is_none());
        // Malformed GUIDs never reach the driver; upper case is sent lower case.
        assert!(proto::set_volume(1, "{01234567-89ab-cdef-0123-456789abcde}").is_none());
        assert!(proto::set_volume(1, "{01234567-89ab-cdef-0123-456789abcdeg}").is_none());
        assert!(proto::set_volume(1, "01234567-89ab-cdef-0123-456789abcdef00").is_none());
        let up = proto::set_volume(2, "{01234567-89AB-CDEF-0123-456789ABCDEF}").unwrap();
        assert_eq!(up, m);
        assert_eq!(proto::begin_plan(true)[16], 1);
    }
}
