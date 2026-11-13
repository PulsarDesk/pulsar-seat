// SPDX-License-Identifier: GPL-3.0-only
//! PulsarPad backend: our own UMDF 2 HID minidriver (`drivers/split-pad`) on a software
//! device (`SwDeviceCreate`, method A of spec 60 §3.6) with the §3.7 properties.
//!
//! **Never enabled by default** and never tested: the driver is unbuilt and unsigned
//! (plan.md, 2026-11-12). It is used only when the operator sets the preferred backend to
//! `pulsar_pad` *and* the driver package is staged. Everything here is exercised against
//! fakes only.

use std::sync::Arc;
use std::time::Duration;

use crate::error::PadError;
use crate::ids;
use crate::layout;
use crate::types::{
    BackendAvailability, BackendId, FeedbackSink, PadBackend, PadCreate, PadId, PadKind, PadState, VirtualPad,
};

pub const CREATE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwDeviceSpec {
    /// `PulsarSplitPad`.
    pub enumerator: String,
    /// Device-id part (spec 60 §3.6), used as the software-device instance id.
    pub instance: String,
    pub hardware_ids: Vec<String>,
    pub description: String,
    pub session_id: u32,
    pub channel: String,
    /// Seat GUID in Windows memory order.
    pub seat_guid: [u8; 16],
    pub slot: u32,
    pub kind: u32,
    pub protocol: u32,
    /// Device security descriptor (SYSTEM, Administrators, seat user).
    pub sddl: String,
    /// Per-pad MAC (`PKEY_PulsarPad_Mac`).
    pub mac: [u8; 6],
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DriverProbe {
    /// Our INF is staged in the driver store (setup class key present).
    pub staged: bool,
    pub version: Option<String>,
}

pub trait SwDeviceHandle: Send {
    fn instance_id(&self) -> String;
    /// Closes the handle; with the default lifetime the device node goes away.
    fn close(self: Box<Self>);
}

pub trait SwDeviceApi: Send + Sync {
    fn driver_probe(&self) -> DriverProbe;
    /// Creates the device and waits for the completion callback.
    fn create(&self, spec: &SwDeviceSpec, timeout: Duration) -> Result<Box<dyn SwDeviceHandle>, PadError>;
}

/// Builds the software-device spec for a pad (pure).
pub fn device_spec(req: &PadCreate, host_salt: &[u8; 16]) -> Result<SwDeviceSpec, PadError> {
    if req.kind == PadKind::X360 {
        return Err(PadError::Unsupported("PulsarPad has no Xbox 360 kind; use xone".into()));
    }
    let id = req.pad_id()?;
    if req.slot >= crate::manager::MAX_SLOTS {
        return Err(PadError::Limit { slot: req.slot, limit: crate::manager::MAX_SLOTS });
    }
    let channel = if req.channel.is_empty() { ids::channel_base(&id) } else { req.channel.clone() };
    if channel.len() > layout::CHANNEL_MAX_LEN || !channel.starts_with(layout::CHANNEL_PREFIX) {
        return Err(PadError::Invalid(format!("bad channel name {channel}")));
    }
    debug_assert!(channel.len() <= layout::CHANNEL_MAX_LEN);
    Ok(SwDeviceSpec {
        enumerator: layout::SW_ENUMERATOR.to_string(),
        instance: ids::device_id_part(req.kind, &id),
        hardware_ids: ids::hardware_ids(req.kind),
        description: format!("Pulsar Split pad {id}"),
        session_id: req.session_id,
        channel,
        seat_guid: ids::seat_guid_bytes(&id.seat32)?,
        slot: req.slot as u32,
        kind: req.kind.kind_value(),
        protocol: layout::PROTOCOL,
        sddl: ids::pad_sddl(&req.owner_sid),
        mac: ids::mac_for(&id.seat32, id.slot, host_salt),
    })
}

pub struct PulsarPadBackend {
    api: Arc<dyn SwDeviceApi>,
    host_salt: [u8; 16],
}

impl PulsarPadBackend {
    pub fn new(api: Arc<dyn SwDeviceApi>, host_salt: [u8; 16]) -> Self {
        PulsarPadBackend { api, host_salt }
    }
}

impl PadBackend for PulsarPadBackend {
    fn id(&self) -> BackendId {
        BackendId::PulsarPad
    }

    fn probe(&self) -> BackendAvailability {
        let p = self.api.driver_probe();
        BackendAvailability {
            id: BackendId::PulsarPad,
            installed: p.staged,
            version: p.version,
            signed: false,
            kinds: vec![PadKind::XOne, PadKind::Ds4, PadKind::Ds5, PadKind::Ds5Edge],
            reason: Some(if p.staged {
                "own UMDF pad driver: unsigned and untested; used only when selected explicitly".into()
            } else {
                "unusable: PulsarPad driver package is not staged (unbuilt, unsigned)".into()
            }),
        }
    }

    fn create(&self, req: &PadCreate, _fb: Arc<dyn FeedbackSink>) -> Result<Box<dyn VirtualPad>, PadError> {
        if !self.api.driver_probe().staged {
            return Err(PadError::DriverMissing("PulsarPad driver package is not staged".into()));
        }
        let spec = device_spec(req, &self.host_salt)?;
        let handle = self.api.create(&spec, CREATE_TIMEOUT)?;
        let instance = handle.instance_id();
        Ok(Box::new(PulsarPadDevice { id: req.pad_id()?, kind: req.kind, instance, handle: Some(handle) }))
    }
}

pub struct PulsarPadDevice {
    id: PadId,
    kind: PadKind,
    instance: String,
    handle: Option<Box<dyn SwDeviceHandle>>,
}

impl VirtualPad for PulsarPadDevice {
    fn id(&self) -> &PadId {
        &self.id
    }
    fn kind(&self) -> PadKind {
        self.kind
    }
    fn instance_id(&self) -> Option<String> {
        Some(self.instance.clone())
    }
    fn submit(&mut self, _s: &PadState) -> Result<(), PadError> {
        Err(PadError::Unsupported("PulsarPad input flows through the shared channel".into()))
    }
    fn submit_report(&mut self, _r: &[u8]) -> Result<(), PadError> {
        Err(PadError::Unsupported("PulsarPad input flows through the shared channel".into()))
    }
    fn driver_reads_channel(&self) -> bool {
        true
    }
    fn remove(mut self: Box<Self>) -> Result<(), PadError> {
        if let Some(h) = self.handle.take() {
            h.close();
        }
        Ok(())
    }
}

impl Drop for PulsarPadDevice {
    fn drop(&mut self) {
        if let Some(h) = self.handle.take() {
            h.close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeSwDevice, RecordingSink};

    fn req(kind: PadKind) -> PadCreate {
        PadCreate {
            seat_id: "0a1b2c3d-0000-4000-8000-00000000abcd".into(),
            slot: 2,
            kind,
            session_id: 3,
            owner_sid: "S-1-5-21-1-2-3-1001".into(),
            caps: Default::default(),
            channel: String::new(),
        }
    }

    #[test]
    fn spec_properties() {
        let s = device_spec(&req(PadKind::XOne), &[3; 16]).unwrap();
        assert_eq!(s.mac, ids::mac_for("0A1B2C3D00004000800000000000ABCD", 2, &[3; 16]));
        assert_eq!(s.enumerator, "PulsarSplitPad");
        assert!(s.instance.contains("&IG_00&PULSARPAD&0A1B2C3D00004000800000000000ABCD&02"));
        assert_eq!(s.hardware_ids[0], "PULSARPAD\\VID_045E&PID_02FF&IG_00");
        assert_eq!(s.kind, layout::KIND_XONE);
        assert_eq!(s.session_id, 3);
        assert_eq!(s.protocol, 1);
        assert!(s.sddl.ends_with("(A;;GRGW;;;S-1-5-21-1-2-3-1001)"));
        assert!(device_spec(&req(PadKind::X360), &[0; 16]).is_err());
    }

    #[test]
    fn lifecycle_against_fake() {
        let api = Arc::new(FakeSwDevice::default());
        let b = PulsarPadBackend::new(api.clone(), [0; 16]);
        assert!(!b.probe().usable());
        assert!(matches!(b.create(&req(PadKind::Ds5), Arc::new(RecordingSink::default())), Err(PadError::DriverMissing(_))));
        api.set_staged(true);
        assert!(b.probe().usable());
        assert!(!b.probe().signed);
        let mut pad = b.create(&req(PadKind::Ds5), Arc::new(RecordingSink::default())).unwrap();
        assert!(pad.driver_reads_channel());
        assert!(pad.instance_id().unwrap().starts_with("SWD\\PulsarSplitPad\\VID_054C&PID_0CE6&PULSARPAD&"));
        assert!(pad.submit(&PadState::neutral()).is_err());
        assert_eq!(api.live(), 1);
        pad.remove().unwrap();
        assert_eq!(api.live(), 0);
    }
}
