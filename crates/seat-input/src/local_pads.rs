// SPDX-License-Identifier: GPL-3.0-only
//! Seat-owned virtual pads for **local** seats (cargo feature `local-pads`, off by default;
//! merge-plan.md step 8, Seat's "gamepads per seat" gap, option B).
//!
//! A physical pad the input router has routed to a local seat cannot be hidden from the
//! other sessions without the unsigned HID visibility filter (see [`crate::gamepad`]). The
//! shared pads crate (`seat-pads`, from Pulsar Split) can instead create a *virtual* pad
//! owned by that seat: its device node gets the seat user's SDDL
//! ([`seat_pads::pad_sddl`]) and session id, so only that seat's games open it. This module
//! is the bridge: it picks a backend for the physical pad's kind, creates one virtual pad
//! per routed physical pad and forwards the pad's state (or, when the layouts match, its raw
//! HID input report) into it.
//!
//! Limits (also in `crates/seat-pads/README.md` and architecture.md §10):
//! - **ViGEmBus only if already installed.** We never install or bundle it. It gives X360
//!   and DS4 targets.
//! - **usbip-win2 + our loopback USB/IP DualSense** is the DS5 path (usbip-win2 must be
//!   installed; its driver is signed).
//! - **Own pad driver (`drivers/split-pad`, PulsarPad) is unsigned**: used only when the
//!   seat explicitly prefers it ([`LocalPadPolicy::preferred`]), never by default.
//! - The **physical** pad stays visible to every session unless the HID visibility filter
//!   runs (unsigned, not loaded). XInput pads are always shared. So a game in another seat
//!   can still read the physical pad; the virtual pad only gives the owning seat a pad that
//!   is its own. Games should be pointed at the virtual pad (HYPOTHESIS: most games pick
//!   the first XInput slot, which may be the physical pad).
//! - Device securing (SDDL + session id on the devnode), orphan cleanup and feedback
//!   routing back to the physical pad (rumble, LEDs) are `seat_pads::PadManager` work;
//!   this bridge takes a plain [`PadBackend`]. Wiring into the service is not done: the
//!   feature is off by default and nothing in `seat-service` enables it yet.
//! - Untested on hardware.

use std::collections::BTreeMap;
use std::sync::Arc;

use seat_pads::mapping::{self, Resolved};
use seat_pads::{
    BackendAvailability, BackendId, FeedbackSink, PadBackend, PadCaps, PadCreate, PadError, PadId, PadKind, PadState,
    VirtualPad, buttons, layout,
};

use crate::gamepad::PadFamily;

/// `XINPUT_GAMEPAD` as the public XInput documentation defines it (what an XInput or a
/// parsed HID pad report gives the seat host).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct XInputGamepad {
    pub buttons: u16,
    pub left_trigger: u8,
    pub right_trigger: u8,
    pub thumb_lx: i16,
    pub thumb_ly: i16,
    pub thumb_rx: i16,
    pub thumb_ry: i16,
}

/// `XINPUT_GAMEPAD_*` button bits (public XInput documentation).
pub mod xinput {
    pub const DPAD_UP: u16 = 0x0001;
    pub const DPAD_DOWN: u16 = 0x0002;
    pub const DPAD_LEFT: u16 = 0x0004;
    pub const DPAD_RIGHT: u16 = 0x0008;
    pub const START: u16 = 0x0010;
    pub const BACK: u16 = 0x0020;
    pub const LEFT_THUMB: u16 = 0x0040;
    pub const RIGHT_THUMB: u16 = 0x0080;
    pub const LEFT_SHOULDER: u16 = 0x0100;
    pub const RIGHT_SHOULDER: u16 = 0x0200;
    pub const A: u16 = 0x1000;
    pub const B: u16 = 0x2000;
    pub const X: u16 = 0x4000;
    pub const Y: u16 = 0x8000;
}

const XINPUT_TO_PAD: [(u16, u32); 14] = [
    (xinput::DPAD_UP, buttons::DPAD_UP),
    (xinput::DPAD_DOWN, buttons::DPAD_DOWN),
    (xinput::DPAD_LEFT, buttons::DPAD_LEFT),
    (xinput::DPAD_RIGHT, buttons::DPAD_RIGHT),
    (xinput::START, buttons::START),
    (xinput::BACK, buttons::BACK),
    (xinput::LEFT_THUMB, buttons::L3),
    (xinput::RIGHT_THUMB, buttons::R3),
    (xinput::LEFT_SHOULDER, buttons::L1),
    (xinput::RIGHT_SHOULDER, buttons::R1),
    (xinput::A, buttons::SOUTH),
    (xinput::B, buttons::EAST),
    (xinput::X, buttons::WEST),
    (xinput::Y, buttons::NORTH),
];

/// XInput state -> the pads crate's [`PadState`] (sticks +y = up in both; triggers
/// 0..=255 scaled to 0..=65535).
pub fn xinput_to_state(g: &XInputGamepad) -> PadState {
    let mut s = PadState::neutral();
    s.buttons = XINPUT_TO_PAD.iter().filter(|(x, _)| g.buttons & x != 0).fold(0, |acc, (_, p)| acc | p);
    s.lx = g.thumb_lx;
    s.ly = g.thumb_ly;
    s.rx = g.thumb_rx;
    s.ry = g.thumb_ry;
    s.lt = u16::from(g.left_trigger) * 257;
    s.rt = u16::from(g.right_trigger) * 257;
    s
}

/// Virtual pad kind that mirrors a physical pad: PlayStation pads keep their own kind
/// (DS4 / DS5 / DS5 Edge by USB id), everything else becomes an Xbox One pad.
pub fn mirror_kind(family: PadFamily, vendor_id: u16, product_id: u16) -> PadKind {
    if family == PadFamily::Hid && vendor_id == layout::VID_SONY {
        match product_id {
            layout::PID_DS5 => return PadKind::Ds5,
            layout::PID_DS5E => return PadKind::Ds5Edge,
            // DS4 v1 (0x05C4) and v2 (0x09CC, public USB id list).
            layout::PID_DS4 | 0x09CC => return PadKind::Ds4,
            _ => {}
        }
    }
    PadKind::XOne
}

/// Per-seat choice (from the seat's settings once the service wires this in).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LocalPadPolicy {
    /// Seat-owned virtual pads on for this seat.
    pub enabled: bool,
    /// Preferred backend. `PulsarPad` (own, unsigned) is used only when named here.
    pub preferred: Option<BackendId>,
}

/// Why a seat gets no virtual pad.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LocalPadRefusal {
    #[error("disabled: seat-owned pads are off for this seat")]
    Disabled,
    #[error("pad_unavailable: no installed backend can create a {0} pad")]
    NoBackend(PadKind),
    #[error("already_attached: {0}")]
    AlreadyAttached(String),
    #[error("not_attached: {0}")]
    NotAttached(String),
    #[error("report_layout_mismatch: physical {physical}, virtual {wire}")]
    ReportLayout { physical: PadKind, wire: PadKind },
    /// A backend error (`PadError` text: `pad_unavailable: …`, `driver_missing: …`).
    #[error("{0}")]
    Pad(String),
}

impl From<PadError> for LocalPadRefusal {
    fn from(e: PadError) -> Self {
        LocalPadRefusal::Pad(e.to_string())
    }
}

/// Pure backend choice for a local seat's virtual pad (the shared `seat_pads::mapping`
/// rules: ViGEm / usbip-win2 only when installed, PulsarPad only when preferred).
pub fn plan(kind: PadKind, policy: &LocalPadPolicy, av: &[BackendAvailability]) -> Result<Resolved, LocalPadRefusal> {
    if !policy.enabled {
        return Err(LocalPadRefusal::Disabled);
    }
    mapping::resolve(kind, policy.preferred, av).ok_or(LocalPadRefusal::NoBackend(kind))
}

/// One physical pad routed to a local seat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhysicalPad {
    /// The input unit's stable key (`DeviceKey` text).
    pub unit_key: String,
    pub kind: PadKind,
}

/// Seat that owns the virtual pad.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeatOwner {
    /// Seat GUID (any case, with or without braces).
    pub seat_id: String,
    /// Pad slot within the seat (0-based).
    pub slot: u8,
    /// Windows session of the seat.
    pub session_id: u32,
    /// SID of the seat user (device SDDL).
    pub owner_sid: String,
}

struct Attached {
    seat_id: String,
    physical: PadKind,
    pad: Box<dyn VirtualPad>,
}

/// Physical pad -> seat-owned virtual pad bridge (one per seat host or service).
pub struct LocalPads {
    backends: Vec<Arc<dyn PadBackend>>,
    feedback: Arc<dyn FeedbackSink>,
    pads: BTreeMap<String, Attached>,
}

impl LocalPads {
    pub fn new(backends: Vec<Arc<dyn PadBackend>>, feedback: Arc<dyn FeedbackSink>) -> Self {
        Self { backends, feedback, pads: BTreeMap::new() }
    }

    /// Backend availability now (each backend's own probe).
    pub fn availability(&self) -> Vec<BackendAvailability> {
        self.backends.iter().map(|b| b.probe()).collect()
    }

    /// Creates the seat-owned virtual pad for `physical`.
    pub fn attach(&mut self, physical: &PhysicalPad, owner: &SeatOwner, policy: &LocalPadPolicy) -> Result<PadId, LocalPadRefusal> {
        if self.pads.contains_key(&physical.unit_key) {
            return Err(LocalPadRefusal::AlreadyAttached(physical.unit_key.clone()));
        }
        let r = plan(physical.kind, policy, &self.availability())?;
        let backend = self
            .backends
            .iter()
            .find(|b| b.id() == r.backend)
            .ok_or(LocalPadRefusal::NoBackend(physical.kind))?;
        let req = PadCreate {
            seat_id: owner.seat_id.clone(),
            slot: owner.slot,
            kind: r.backend_kind,
            session_id: owner.session_id,
            owner_sid: owner.owner_sid.clone(),
            caps: PadCaps { rumble: true, ..PadCaps::default() },
            channel: String::new(),
        };
        req.pad_id()?;
        let pad = backend.create(&req, self.feedback.clone())?;
        let id = pad.id().clone();
        self.pads.insert(physical.unit_key.clone(), Attached { seat_id: owner.seat_id.clone(), physical: physical.kind, pad });
        Ok(id)
    }

    /// Forwards the physical pad's state.
    pub fn forward_state(&mut self, unit_key: &str, s: &PadState) -> Result<(), LocalPadRefusal> {
        let a = self.pads.get_mut(unit_key).ok_or_else(|| LocalPadRefusal::NotAttached(unit_key.into()))?;
        Ok(a.pad.submit(s)?)
    }

    /// Forwards a raw HID input report unchanged. Only when the physical pad's report
    /// layout is the virtual pad's wire layout (same kind); otherwise parse it into a
    /// [`PadState`] and call [`Self::forward_state`].
    pub fn forward_report(&mut self, unit_key: &str, report: &[u8]) -> Result<(), LocalPadRefusal> {
        let a = self.pads.get_mut(unit_key).ok_or_else(|| LocalPadRefusal::NotAttached(unit_key.into()))?;
        let wire = a.pad.kind();
        if wire != a.physical {
            return Err(LocalPadRefusal::ReportLayout { physical: a.physical, wire });
        }
        Ok(a.pad.submit_report(report)?)
    }

    /// Removes the virtual pad of one physical pad (unrouted or unplugged).
    pub fn detach(&mut self, unit_key: &str) -> Result<(), LocalPadRefusal> {
        let a = self.pads.remove(unit_key).ok_or_else(|| LocalPadRefusal::NotAttached(unit_key.into()))?;
        Ok(a.pad.remove()?)
    }

    /// Removes every virtual pad of a seat (seat stop). Returns the errors; keeps going.
    pub fn detach_seat(&mut self, seat_id: &str) -> Vec<LocalPadRefusal> {
        let keys: Vec<String> =
            self.pads.iter().filter(|(_, a)| a.seat_id.eq_ignore_ascii_case(seat_id)).map(|(k, _)| k.clone()).collect();
        keys.iter().filter_map(|k| self.detach(k).err()).collect()
    }

    /// Unit keys with a virtual pad.
    pub fn attached(&self) -> Vec<String> {
        self.pads.keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use seat_pads::{BackendAvailability, NullSink};
    use std::sync::Mutex;

    #[derive(Default)]
    struct Log {
        created: Vec<PadCreate>,
        states: Vec<PadState>,
        reports: Vec<Vec<u8>>,
        removed: Vec<PadId>,
    }

    struct FakeBackend {
        id: BackendId,
        kinds: Vec<PadKind>,
        installed: bool,
        log: Arc<Mutex<Log>>,
    }

    struct FakePad {
        id: PadId,
        kind: PadKind,
        log: Arc<Mutex<Log>>,
    }

    impl VirtualPad for FakePad {
        fn id(&self) -> &PadId {
            &self.id
        }
        fn kind(&self) -> PadKind {
            self.kind
        }
        fn instance_id(&self) -> Option<String> {
            None
        }
        fn submit(&mut self, s: &PadState) -> Result<(), PadError> {
            self.log.lock().unwrap().states.push(*s);
            Ok(())
        }
        fn submit_report(&mut self, r: &[u8]) -> Result<(), PadError> {
            self.log.lock().unwrap().reports.push(r.to_vec());
            Ok(())
        }
        fn remove(self: Box<Self>) -> Result<(), PadError> {
            self.log.lock().unwrap().removed.push(self.id.clone());
            Ok(())
        }
    }

    impl PadBackend for FakeBackend {
        fn id(&self) -> BackendId {
            self.id
        }
        fn probe(&self) -> BackendAvailability {
            BackendAvailability {
                id: self.id,
                installed: self.installed,
                version: None,
                signed: self.id != BackendId::PulsarPad,
                kinds: self.kinds.clone(),
                reason: None,
            }
        }
        fn create(&self, req: &PadCreate, _fb: Arc<dyn FeedbackSink>) -> Result<Box<dyn VirtualPad>, PadError> {
            self.log.lock().unwrap().created.push(req.clone());
            Ok(Box::new(FakePad { id: req.pad_id()?, kind: req.kind, log: self.log.clone() }))
        }
    }

    const SEAT: &str = "{0a1b2c3d-0000-4000-8000-00000000abcd}";

    fn owner(slot: u8) -> SeatOwner {
        SeatOwner { seat_id: SEAT.into(), slot, session_id: 3, owner_sid: "S-1-5-21-1-2-3-1001".into() }
    }

    fn on() -> LocalPadPolicy {
        LocalPadPolicy { enabled: true, preferred: None }
    }

    fn bridge(vigem: bool, usbip: bool, own: bool) -> (LocalPads, Arc<Mutex<Log>>) {
        let log = Arc::new(Mutex::new(Log::default()));
        let mk = |id, kinds: &[PadKind], installed| -> Arc<dyn PadBackend> {
            Arc::new(FakeBackend { id, kinds: kinds.to_vec(), installed, log: log.clone() })
        };
        let backends = vec![
            mk(BackendId::Vigem, &[PadKind::X360, PadKind::Ds4], vigem),
            mk(BackendId::UsbipDs5, &[PadKind::Ds5, PadKind::Ds5Edge], usbip),
            mk(BackendId::PulsarPad, &[PadKind::Ds4, PadKind::Ds5, PadKind::Ds5Edge, PadKind::XOne], own),
        ];
        (LocalPads::new(backends, Arc::new(NullSink)), log)
    }

    #[test]
    fn xinput_mapping() {
        let g = XInputGamepad {
            buttons: xinput::A | xinput::Y | xinput::DPAD_LEFT | xinput::LEFT_SHOULDER | xinput::BACK,
            left_trigger: 255,
            right_trigger: 1,
            thumb_lx: -32768,
            thumb_ly: 32767,
            thumb_rx: 5,
            thumb_ry: -5,
        };
        let s = xinput_to_state(&g);
        assert_eq!(s.buttons, buttons::SOUTH | buttons::NORTH | buttons::DPAD_LEFT | buttons::L1 | buttons::BACK);
        assert_eq!((s.lt, s.rt), (65535, 257));
        assert_eq!((s.lx, s.ly, s.rx, s.ry), (-32768, 32767, 5, -5));
        assert_eq!(xinput_to_state(&XInputGamepad::default()), PadState::neutral());
    }

    #[test]
    fn mirror_kinds() {
        assert_eq!(mirror_kind(PadFamily::Hid, 0x054C, 0x0CE6), PadKind::Ds5);
        assert_eq!(mirror_kind(PadFamily::Hid, 0x054C, 0x0DF2), PadKind::Ds5Edge);
        assert_eq!(mirror_kind(PadFamily::Hid, 0x054C, 0x09CC), PadKind::Ds4);
        assert_eq!(mirror_kind(PadFamily::Hid, 0x054C, 0x1234), PadKind::XOne);
        assert_eq!(mirror_kind(PadFamily::XInput, 0x045E, 0x0B12), PadKind::XOne);
        assert_eq!(mirror_kind(PadFamily::Hid, 0x2DC8, 0x0001), PadKind::XOne);
    }

    #[test]
    fn plan_rules() {
        let av = |id, kinds: &[PadKind], installed| BackendAvailability {
            id,
            installed,
            version: None,
            signed: true,
            kinds: kinds.to_vec(),
            reason: None,
        };
        let vigem = av(BackendId::Vigem, &[PadKind::X360, PadKind::Ds4], true);
        let own = av(BackendId::PulsarPad, &[PadKind::XOne], true);
        assert_eq!(plan(PadKind::XOne, &LocalPadPolicy::default(), std::slice::from_ref(&vigem)), Err(LocalPadRefusal::Disabled));
        let r = plan(PadKind::XOne, &on(), &[vigem.clone(), own.clone()]).unwrap();
        assert_eq!((r.backend, r.backend_kind), (BackendId::Vigem, PadKind::X360), "own pad never by default");
        let pref = LocalPadPolicy { enabled: true, preferred: Some(BackendId::PulsarPad) };
        assert_eq!(plan(PadKind::XOne, &pref, &[vigem, own.clone()]).unwrap().backend, BackendId::PulsarPad);
        assert_eq!(plan(PadKind::XOne, &on(), &[own]), Err(LocalPadRefusal::NoBackend(PadKind::XOne)));
        let not_installed = av(BackendId::Vigem, &[PadKind::X360], false);
        assert_eq!(plan(PadKind::X360, &on(), &[not_installed]), Err(LocalPadRefusal::NoBackend(PadKind::X360)));
    }

    #[test]
    fn attach_forward_detach() {
        let (mut lp, log) = bridge(true, true, false);
        let ds5 = PhysicalPad { unit_key: "hid;inst=DS5".into(), kind: PadKind::Ds5 };
        let id = lp.attach(&ds5, &owner(0), &on()).unwrap();
        assert_eq!(id.slot, 0);
        assert!(matches!(lp.attach(&ds5, &owner(1), &on()), Err(LocalPadRefusal::AlreadyAttached(_))));
        {
            let l = log.lock().unwrap();
            assert_eq!(l.created.len(), 1);
            assert_eq!(l.created[0].kind, PadKind::Ds5);
            assert_eq!(l.created[0].session_id, 3);
            assert_eq!(l.created[0].owner_sid, "S-1-5-21-1-2-3-1001");
        }
        // Same layout: raw report passes through.
        lp.forward_report("hid;inst=DS5", &[0x01, 0x80]).unwrap();
        lp.forward_state("hid;inst=DS5", &PadState::neutral()).unwrap();

        let xbox = PhysicalPad { unit_key: "hid;inst=XB".into(), kind: PadKind::XOne };
        lp.attach(&xbox, &owner(1), &on()).unwrap();
        // XOne mirrored on ViGEm X360: raw reports have another layout.
        assert_eq!(
            lp.forward_report("hid;inst=XB", &[0]),
            Err(LocalPadRefusal::ReportLayout { physical: PadKind::XOne, wire: PadKind::X360 })
        );
        assert!(matches!(lp.forward_state("nope", &PadState::neutral()), Err(LocalPadRefusal::NotAttached(_))));
        assert_eq!(lp.attached().len(), 2);
        assert!(lp.detach_seat("{0A1B2C3D-0000-4000-8000-00000000ABCD}").is_empty());
        assert!(lp.attached().is_empty());
        let l = log.lock().unwrap();
        assert_eq!(l.reports, vec![vec![0x01, 0x80]]);
        assert_eq!(l.states.len(), 1);
        assert_eq!(l.removed.len(), 2);
    }

    #[test]
    fn no_backend_installed() {
        let (mut lp, log) = bridge(false, false, true);
        let ds4 = PhysicalPad { unit_key: "hid;inst=DS4".into(), kind: PadKind::Ds4 };
        assert_eq!(lp.attach(&ds4, &owner(0), &on()), Err(LocalPadRefusal::NoBackend(PadKind::Ds4)));
        assert!(log.lock().unwrap().created.is_empty());
        assert!(matches!(lp.detach("hid;inst=DS4"), Err(LocalPadRefusal::NotAttached(_))));
    }
}
