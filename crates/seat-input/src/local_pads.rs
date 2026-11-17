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
//! - The **physical** pad is scoped to its seat by the service since 2026-11-17
//!   ([`crate::pad_scope`]: device SDDL + session id on the pad's HID collection and its
//!   parent, plus the shim seat map for XInput in shimmed processes). Without that (setting
//!   off, ARM64 apps, the session-0 GameInput service) a game in another seat may still
//!   read the physical pad (HYPOTHESIS: most games pick the first XInput slot).
//! - Every HID pad is read since 2026-11-17 ([`RoutedPad::layout`]): PlayStation and
//!   Switch Pro by their public layouts, others through their report descriptor.
//! - Device securing (SDDL + session id on the virtual devnode), orphan cleanup and
//!   feedback routing back to the physical pad (rumble, LEDs, Bluetooth CRC reports) are
//!   `seat_pads::PadManager` / [`LocalFeedbackRouter`] work. The service enables the
//!   feature (round 4); per seat it stays off by default.
//! - Untested on hardware.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use seat_pads::mapping::{self, Cap, ClientType, Resolved};
use seat_pads::generic::{self, HidLayout, HidSample};
use seat_pads::{
    BackendAvailability, BackendId, Feedback, FeedbackSink, PadBackend, PadCaps, PadCreate, PadError, PadId, PadKind,
    PadManager, PadState, PadWanted, SeatPadContext, VirtualPad, bt, buttons, layout, reports, switch_pro,
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

// ---------------------------------------------------------------------------------------
// Service wiring (round 4, 2026-11-14): the hub on the shared `PadManager`
// ---------------------------------------------------------------------------------------

/// Seat id the pads crate uses for a local seat. Local seats are numbered, the pads crate
/// wants a GUID (device-id marker, USB serial, ownership registry), so each local seat
/// gets a fixed one: `5053454c-4f43-4100-8000-<seat number, 12 hex digits>` ("PSELOCA").
/// Remote seat ids are random v4 GUIDs and never take this form.
pub fn local_seat_pad_id(seat: u8) -> String {
    format!("5053454c-4f43-4100-8000-{:012x}", seat)
}

/// Whether a pads-crate seat id is one of [`local_seat_pad_id`].
pub fn is_local_seat_pad_id(seat_id: &str) -> bool {
    seat_id.trim_matches(|c| c == '{' || c == '}').to_ascii_lowercase().starts_with("5053454c-4f43-4100-8000-")
}

/// Bus of a physical pad (`InputInfo::bus` of its unit).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PadBus {
    #[default]
    Usb,
    Bluetooth,
    Other,
}

impl PadBus {
    /// From the inventory's bus text (`usb`, `bluetooth`, `bthle`, ...).
    pub fn from_info(bus: &str) -> Self {
        match bus.to_ascii_lowercase().as_str() {
            "usb" => Self::Usb,
            "bluetooth" | "bt" | "bth" | "bthle" | "ble" | "bthenum" => Self::Bluetooth,
            _ => Self::Other,
        }
    }
}

/// How the hub reads (and gives feedback to) a physical pad.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhysicalLayout {
    /// DualShock 4 / DualSense / Edge: public report layouts, read as they are.
    PlayStation(PadKind),
    /// Nintendo Switch Pro Controller: reports 0x30 / 0x3F (`seat_pads::switch_pro`).
    SwitchPro,
    /// Any other HID pad (Xbox through its HID collection, generic HID joysticks and
    /// gamepads): decoded with the collection's own report descriptor (Windows `HidP_*`,
    /// [`PhysicalPadIo::hid_layout`] / [`PhysicalPadIo::hid_sample`]) and mapped by
    /// `seat_pads::generic`. `xinput` = Xbox family.
    Hid { xinput: bool },
}

/// One physical pad the input router gives to a local seat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutedPad {
    /// The input unit's stable key (`DeviceKey` text).
    pub unit_key: String,
    /// HID interface path the service opens (reads input, writes feedback).
    pub interface_path: String,
    pub family: PadFamily,
    pub vendor_id: u16,
    pub product_id: u16,
    pub bus: PadBus,
}

impl RoutedPad {
    /// How the hub reads this pad. Every HID pad is readable since 2026-11-17: PlayStation
    /// and Switch Pro by their public layouts, everything else through its descriptor.
    pub fn layout(&self) -> PhysicalLayout {
        let k = mirror_kind(self.family, self.vendor_id, self.product_id);
        if k.is_playstation() {
            PhysicalLayout::PlayStation(k)
        } else if switch_pro::is_switch_pro(self.vendor_id, self.product_id) {
            PhysicalLayout::SwitchPro
        } else {
            PhysicalLayout::Hid { xinput: self.family == PadFamily::XInput }
        }
    }

    /// Virtual pad kind for this pad ([`mirror_kind`]).
    pub fn mirror(&self) -> PadKind {
        mirror_kind(self.family, self.vendor_id, self.product_id)
    }
}

/// Everything the hub needs about one local seat with seat-owned virtual pads on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalPadSeat {
    pub seat: u8,
    /// Windows session of the seat (device session id).
    pub session_id: u32,
    /// SID of the signed-in seat user (device SDDL).
    pub owner_sid: String,
    /// The seat's pads in slot order (at most `seat_pads::manager::MAX_SLOTS`).
    pub pads: Vec<RoutedPad>,
}

/// What [`LocalPadHub::sync`] did (logged by the service, shown later in the manager).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalPadNote {
    Attached { unit_key: String, pad_id: String },
    /// Create runs on a worker (usbip-win2): the pad appears a moment later.
    Pending { unit_key: String, pad_id: String },
    Removed { unit_key: String },
    /// The physical pad's reports cannot be decoded (a HID pad whose report descriptor
    /// the source could not read).
    Unsupported { unit_key: String },
    /// No installed backend for the kind, or the create failed.
    Denied { unit_key: String, detail: String },
    OpenFailed { unit_key: String, error: String },
}

/// Opens physical pads (Windows: `win::pad_io::WinPadSource`).
pub trait PhysicalPadSource: Send + Sync {
    fn open(&self, interface_path: &str) -> Result<Box<dyn PhysicalPadIo>, String>;
}

/// An open physical pad.
pub trait PhysicalPadIo: Send {
    /// The next input report, waiting at most `timeout_ms`; `Ok(None)` on timeout.
    fn read(&mut self, timeout_ms: u32) -> Result<Option<Vec<u8>>, String>;
    /// Writer for output reports (feedback), usable from other threads.
    fn writer(&self) -> Arc<dyn PhysicalPadWriter>;
    /// Input usages the collection declares ([`PhysicalLayout::Hid`] pads). `None` = the
    /// descriptor could not be read: the pad is reported unsupported.
    fn hid_layout(&self) -> Option<HidLayout> {
        None
    }
    /// One input report decoded through the collection's descriptor.
    fn hid_sample(&self, _report: &[u8]) -> Option<HidSample> {
        None
    }
}

/// Writes output reports to a physical pad.
pub trait PhysicalPadWriter: Send + Sync {
    fn write(&self, report: &[u8]) -> Result<(), String>;
    /// Rumble for a pad without a usable output report (parity review 2026-11-17): the XUSB
    /// interface of an Xbox pad on USB, or the HID PID force-feedback reports of a generic
    /// pad (`seat_pads::ff`). `Err` = the pad has neither.
    fn rumble_fallback(&self, strong: u16, weak: u16) -> Result<(), String> {
        let _ = (strong, weak);
        Err("no rumble path".into())
    }
}

/// Feeds one physical input report into its virtual pad: unchanged when the wire kind is
/// the physical kind (DualSense on usbip-win2, DS4 on ViGEm DS4), else parsed into a state
/// (e.g. DualSense on a ViGEm X360 pad). Bluetooth reports are cut to the USB layout first.
/// `Ok(false)`: report not used (other report id, or the pad's create is still pending).
pub fn forward_physical(mgr: &mut PadManager, pad: &PadId, physical: PadKind, raw: &[u8]) -> Result<bool, PadError> {
    let Some(usb) = reports::usb_input_from_any(physical, raw) else { return Ok(false) };
    let Some(wire) = mgr.wire_kind(pad) else { return Ok(false) };
    if wire == physical {
        mgr.submit_report(pad, &usb)?;
        return Ok(true);
    }
    let state = match physical {
        PadKind::Ds4 => reports::parse_ds4_input(&usb),
        PadKind::Ds5 => reports::parse_ds5_input(&usb, false),
        PadKind::Ds5Edge => reports::parse_ds5_input(&usb, true),
        _ => None,
    };
    match state {
        Some(s) => mgr.submit(pad, &s).map(|_| true),
        None => Ok(false),
    }
}

/// Feeds a decoded state (Switch Pro, generic HID pads) into its virtual pad.
/// `Ok(false)` while the pad's create is still pending.
pub fn forward_state(mgr: &mut PadManager, pad: &PadId, s: &PadState) -> Result<bool, PadError> {
    if mgr.wire_kind(pad).is_none() {
        return Ok(false);
    }
    mgr.submit(pad, s).map(|_| true)
}

/// How the reader turns one physical report into virtual pad input.
enum Decoder {
    Ps(PadKind),
    Switch,
    Hid { profile: generic::Profile, layout: HidLayout },
}

/// Feedback state kept per physical pad (rebuilt reports carry every field each time).
#[derive(Default)]
struct FbState {
    ds: reports::Ds5Output,
    strong: u16,
    weak: u16,
    lt: u16,
    rt: u16,
    /// 4-bit counter (DualSense Bluetooth sequence, Switch Pro packet number).
    seq: u8,
}

impl FbState {
    fn next_seq(&mut self) -> u8 {
        self.seq = self.seq.wrapping_add(1) & 0x0F;
        self.seq
    }
}

/// Physical side of one virtual pad, for feedback.
struct FeedbackTarget {
    layout: PhysicalLayout,
    writer: Arc<dyn PhysicalPadWriter>,
    /// From the unit's bus, and set by the reader from the first report (PlayStation
    /// Bluetooth reports 0x31 / 0x11).
    bluetooth: AtomicBool,
    state: Mutex<FbState>,
}

/// Feedback (rumble, lightbar, player LEDs, adaptive triggers) from a local seat's virtual
/// pad back to the physical pad. It is the `PadManager`'s sink for every pad, so pads of
/// remote seats (no target here) are ignored, as with the `NullSink` before.
///
/// - PlayStation, same kind as the virtual pad (DualSense on usbip-win2, DS4 on ViGEm DS4):
///   the game's output report goes to the pad unchanged (adaptive triggers included); on
///   Bluetooth it is rewrapped as report 0x31 / 0x11 with its CRC (`seat_pads::bt`).
/// - PlayStation, another kind (a DualSense behind a ViGEm X360 pad): rumble, lightbar,
///   player LEDs and triggers are rebuilt into the physical pad's own report (USB or
///   Bluetooth).
/// - Switch Pro: rumble (report 0x10) and player lights (sub-command 0x30).
/// - Xbox pads on Bluetooth: rumble and impulse triggers (report 0x03).
/// - Other HID pads (and Xbox pads on USB, whose rumble only XInput reaches): none.
#[derive(Default)]
pub struct LocalFeedbackRouter {
    targets: Mutex<BTreeMap<PadId, Arc<FeedbackTarget>>>,
    wires: Mutex<BTreeMap<PadId, PadKind>>,
}

impl LocalFeedbackRouter {
    pub fn new() -> Self {
        Self::default()
    }

    fn add(&self, pad: PadId, layout: PhysicalLayout, bluetooth: bool, writer: Arc<dyn PhysicalPadWriter>) -> Arc<FeedbackTarget> {
        let t = Arc::new(FeedbackTarget {
            layout,
            writer,
            bluetooth: AtomicBool::new(bluetooth),
            state: Mutex::new(FbState::default()),
        });
        lock(&self.targets).insert(pad, t.clone());
        t
    }

    fn remove(&self, pad: &PadId) {
        lock(&self.targets).remove(pad);
        lock(&self.wires).remove(pad);
    }

    /// The virtual pad's wire kind once it is live (reader thread).
    fn set_wire(&self, pad: &PadId, wire: PadKind) {
        lock(&self.wires).insert(pad.clone(), wire);
    }

    fn target(&self, pad: &PadId) -> Option<(Arc<FeedbackTarget>, Option<PadKind>)> {
        let t = lock(&self.targets).get(pad).cloned()?;
        Some((t, lock(&self.wires).get(pad).copied()))
    }

    /// Pads with a physical target (tests, diagnostics).
    pub fn targets(&self) -> usize {
        lock(&self.targets).len()
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl FeedbackSink for LocalFeedbackRouter {
    fn feedback(&self, pad: &PadId, f: Feedback) {
        let Some((t, wire)) = self.target(pad) else { return };
        let bluetooth = t.bluetooth.load(Ordering::SeqCst);
        let report: Option<Vec<u8>> = {
            let mut s = lock(&t.state);
            match t.layout {
                PhysicalLayout::PlayStation(kind) => {
                    if wire == Some(kind) {
                        // Same kind: `raw_output` already forwarded the game's report.
                        return;
                    }
                    match f {
                        Feedback::Rumble { strong, weak } => {
                            s.ds.strong = (strong >> 8) as u8;
                            s.ds.weak = (weak >> 8) as u8;
                        }
                        Feedback::Led { r, g, b } => s.ds.rgb = Some((r, g, b)),
                        Feedback::PlayerLeds { mask } => s.ds.player_leds = Some(mask),
                        Feedback::AdaptiveTrigger { raw } => s.ds.triggers = Some(raw),
                        Feedback::TriggerRumble { .. } => return,
                    }
                    let usb = match kind {
                        PadKind::Ds5 | PadKind::Ds5Edge => reports::ds5_output(&s.ds).to_vec(),
                        PadKind::Ds4 => reports::ds4_output(s.ds.weak, s.ds.strong, s.ds.rgb.unwrap_or((0, 0, 0x40))).to_vec(),
                        _ => return,
                    };
                    if bluetooth {
                        let seq = s.next_seq();
                        bt::ps_bt_output(kind, &usb, seq).map(|r| r.to_vec())
                    } else {
                        Some(usb)
                    }
                }
                PhysicalLayout::SwitchPro => match f {
                    Feedback::Rumble { strong, weak } => {
                        let seq = s.next_seq();
                        Some(switch_pro::rumble_report(seq, strong, weak))
                    }
                    Feedback::PlayerLeds { mask } => {
                        let seq = s.next_seq();
                        Some(switch_pro::subcommand_report(seq, switch_pro::subcmd::SET_PLAYER_LIGHTS, &[switch_pro::player_lights(mask)]))
                    }
                    _ => None,
                },
                PhysicalLayout::Hid { xinput: true } if bluetooth => {
                    match f {
                        Feedback::Rumble { strong, weak } => (s.strong, s.weak) = (strong, weak),
                        Feedback::TriggerRumble { left, right } => (s.lt, s.rt) = (left, right),
                        _ => return,
                    }
                    Some(bt::xbox_bt_output(s.strong, s.weak, s.lt, s.rt).to_vec())
                }
                PhysicalLayout::Hid { .. } => match f {
                    // Xbox on USB (XUSB) and generic HID pads with force feedback (PID).
                    Feedback::Rumble { strong, weak } => {
                        drop(s);
                        if let Err(e) = t.writer.rumble_fallback(strong, weak) {
                            log::debug!("pad {pad}: rumble on the physical pad: {e}");
                        }
                        return;
                    }
                    _ => None,
                },
            }
        };
        let Some(report) = report else { return };
        if let Err(e) = t.writer.write(&report) {
            log::debug!("pad {pad}: feedback to the physical pad failed: {e}");
        }
    }

    fn raw_output(&self, pad: &PadId, source: u8, report: &[u8]) {
        let Some((t, wire)) = self.target(pad) else { return };
        let PhysicalLayout::PlayStation(kind) = t.layout else { return };
        if source == layout::SOURCE_SET_FEATURE || wire != Some(kind) {
            return;
        }
        let out = if t.bluetooth.load(Ordering::SeqCst) {
            let seq = lock(&t.state).next_seq();
            match bt::ps_bt_output(kind, report, seq) {
                Some(r) => r.to_vec(),
                None => return,
            }
        } else {
            report.to_vec()
        };
        if let Err(e) = t.writer.write(&out) {
            log::debug!("pad {pad}: output report to the physical pad failed: {e}");
        }
    }
}

struct LivePhysical {
    pads_seat: String,
    slot: u8,
    pad_id: PadId,
    session_id: u32,
    owner_sid: String,
    stop: Arc<AtomicBool>,
    /// Set by the reader when the physical pad stops answering (unplugged).
    dead: Arc<AtomicBool>,
    reader: Option<std::thread::JoinHandle<()>>,
}

/// Local seats' virtual pads on the service's one `PadManager` (shared with remote seats:
/// one ownership registry, one orphan cleanup, one securing pass). [`Self::sync`] makes
/// the live pads match what the input router routes to the seats; one reader thread per
/// physical pad forwards its reports ([`forward_physical`]).
pub struct LocalPadHub {
    mgr: Arc<Mutex<PadManager>>,
    source: Arc<dyn PhysicalPadSource>,
    router: Arc<LocalFeedbackRouter>,
    live: BTreeMap<String, LivePhysical>,
    generation: u32,
    /// Units already reported as unsupported / denied (log once until they change).
    quiet: BTreeMap<String, String>,
}

/// Reader poll timeout: the thread checks its stop flag at least this often.
const READ_TIMEOUT_MS: u32 = 100;
/// `quiet` tag of a pad whose reports cannot be decoded.
const UNSUPPORTED: &str = "unsupported";

impl LocalPadHub {
    /// `router` must be the `PadManager`'s feedback sink (`PadManager::new`).
    pub fn new(mgr: Arc<Mutex<PadManager>>, source: Arc<dyn PhysicalPadSource>, router: Arc<LocalFeedbackRouter>) -> Self {
        Self { mgr, source, router, live: BTreeMap::new(), generation: 0, quiet: BTreeMap::new() }
    }

    /// Unit keys with a live (or pending) virtual pad.
    pub fn attached(&self) -> Vec<String> {
        self.live.keys().cloned().collect()
    }

    /// Makes the virtual pads match `want` (seats with virtual pads on, a session and a
    /// user). Pads no longer routed, of a stopped seat, or whose seat changed user or
    /// session are removed first; new ones are created.
    pub fn sync(&mut self, want: &[LocalPadSeat]) -> Vec<LocalPadNote> {
        let mut notes = Vec::new();
        let mut desired: BTreeMap<String, (&LocalPadSeat, &RoutedPad, u8)> = BTreeMap::new();
        for seat in want {
            for (slot, p) in seat.pads.iter().enumerate().take(usize::from(seat_pads::manager::MAX_SLOTS)) {
                desired.insert(p.unit_key.clone(), (seat, p, slot as u8));
            }
        }
        // 1. removals
        let gone: Vec<String> = self
            .live
            .iter()
            .filter(|(k, l)| match desired.get(*k) {
                _ if l.dead.load(Ordering::SeqCst) => true,
                None => true,
                Some((s, _, slot)) => {
                    l.pads_seat != local_seat_pad_id(s.seat) || l.slot != *slot || l.session_id != s.session_id || l.owner_sid != s.owner_sid
                }
            })
            .map(|(k, _)| k.clone())
            .collect();
        for k in gone {
            self.remove(&k);
            notes.push(LocalPadNote::Removed { unit_key: k });
        }
        self.quiet.retain(|k, _| desired.contains_key(k));
        // 2. creations
        for (key, (seat, p, slot)) in desired {
            if self.live.contains_key(&key) {
                continue;
            }
            if self.quiet.get(&key).map(String::as_str) == Some(UNSUPPORTED) {
                // Its descriptor could not be read: not re-opened every tick.
                continue;
            }
            match self.attach(seat, p, slot) {
                Ok(n) => {
                    self.quiet.remove(&key);
                    notes.push(n);
                }
                Err(n) => {
                    let tag = if matches!(n, LocalPadNote::Unsupported { .. }) { UNSUPPORTED.to_string() } else { format!("{n:?}") };
                    self.note_once(&mut notes, &key, &tag, n);
                }
            }
        }
        notes
    }

    fn note_once(&mut self, notes: &mut Vec<LocalPadNote>, key: &str, tag: &str, n: LocalPadNote) {
        if self.quiet.get(key).map(String::as_str) != Some(tag) {
            self.quiet.insert(key.to_string(), tag.to_string());
            notes.push(n);
        }
    }

    fn attach(&mut self, seat: &LocalPadSeat, p: &RoutedPad, slot: u8) -> Result<LocalPadNote, LocalPadNote> {
        let unit_key = p.unit_key.clone();
        let mut io = self.source.open(&p.interface_path).map_err(|error| LocalPadNote::OpenFailed { unit_key: unit_key.clone(), error })?;
        let physical = p.layout();
        let kind = p.mirror();
        let decoder = match physical {
            PhysicalLayout::PlayStation(k) => Decoder::Ps(k),
            PhysicalLayout::SwitchPro => Decoder::Switch,
            PhysicalLayout::Hid { xinput } => match io.hid_layout() {
                Some(l) => Decoder::Hid { profile: generic::profile_for(xinput, &l), layout: l },
                None => return Err(LocalPadNote::Unsupported { unit_key }),
            },
        };
        let seat_id = local_seat_pad_id(seat.seat);
        let pad_id = PadId::new(&seat_id, slot).map_err(|e| LocalPadNote::Denied { unit_key: unit_key.clone(), detail: e.to_string() })?;
        self.generation = self.generation.wrapping_add(1);
        let ctx = SeatPadContext {
            seat_id: seat_id.clone(),
            session_id: seat.session_id,
            owner_sid: seat.owner_sid.clone(),
            pads_enabled: true,
            max_slots: None,
        };
        let caps = match physical {
            PhysicalLayout::PlayStation(_) => vec![Cap::Rumble, Cap::TriggerRumble, Cap::Touchpad, Cap::Accel, Cap::Gyro, Cap::Battery, Cap::RgbLed],
            PhysicalLayout::SwitchPro => vec![Cap::Rumble, Cap::Accel, Cap::Gyro],
            PhysicalLayout::Hid { .. } => vec![Cap::Rumble, Cap::TriggerRumble],
        };
        let client_type = match physical {
            PhysicalLayout::PlayStation(_) => ClientType::Ps,
            PhysicalLayout::SwitchPro => ClientType::Nintendo,
            PhysicalLayout::Hid { .. } => ClientType::Xbox,
        };
        let wanted = PadWanted { slot, kind, client_type, caps, generation: self.generation };
        let target = self.router.add(pad_id.clone(), physical, p.bus == PadBus::Bluetooth, io.writer());
        if physical == PhysicalLayout::SwitchPro {
            // Full report mode, IMU, vibration, player light (USB: the 0x80 handshake first).
            let init = {
                let mut st = lock(&target.state);
                let mut c = st.seq;
                let mut r = switch_pro::init_reports(p.bus == PadBus::Usb, &mut c, slot);
                // Factory / user stick calibration (SPI flash); replies come as 0x21.
                r.extend(switch_pro::calibration_reads(&mut c));
                st.seq = c;
                r
            };
            for r in init {
                if let Err(e) = target.writer.write(&r) {
                    log::info!("pad {pad_id}: Switch Pro init write failed: {e}");
                    break;
                }
            }
        }
        let answer = lock(&self.mgr).wanted(&ctx, &wanted);
        let note = match answer {
            Ok(Some(_)) => LocalPadNote::Attached { unit_key: unit_key.clone(), pad_id: pad_id.to_string() },
            Ok(None) => LocalPadNote::Pending { unit_key: unit_key.clone(), pad_id: pad_id.to_string() },
            Err(d) => {
                self.router.remove(&pad_id);
                return Err(LocalPadNote::Denied { unit_key, detail: format!("{:?}: {}", d.code, d.detail) });
            }
        };
        let stop = Arc::new(AtomicBool::new(false));
        let dead = Arc::new(AtomicBool::new(false));
        let reader = {
            let (mgr, router, stop, dead, pad, key) =
                (self.mgr.clone(), self.router.clone(), stop.clone(), dead.clone(), pad_id.clone(), unit_key.clone());
            std::thread::Builder::new()
                .name(format!("local-pad-{slot}"))
                .spawn(move || {
                    read_loop(&mut *io, &mgr, &router, &target, &pad, &decoder, &stop, &key);
                    dead.store(true, Ordering::SeqCst);
                })
                .ok()
        };
        if reader.is_none() {
            log::warn!("pad {pad_id}: reader thread not started");
        }
        self.live.insert(
            unit_key,
            LivePhysical {
                pads_seat: seat_id,
                slot,
                pad_id,
                session_id: seat.session_id,
                owner_sid: seat.owner_sid.clone(),
                stop,
                dead,
                reader,
            },
        );
        Ok(note)
    }

    fn remove(&mut self, unit_key: &str) {
        let Some(mut l) = self.live.remove(unit_key) else { return };
        l.stop.store(true, Ordering::SeqCst);
        if let Some(h) = l.reader.take() {
            let _ = h.join();
        }
        self.router.remove(&l.pad_id);
        lock(&self.mgr).remove_slot(&l.pads_seat, l.slot);
    }

    /// Removes every local virtual pad (service stop).
    pub fn shutdown(&mut self) {
        let keys: Vec<String> = self.live.keys().cloned().collect();
        for k in keys {
            self.remove(&k);
        }
    }
}

impl Drop for LocalPadHub {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[allow(clippy::too_many_arguments)]
fn read_loop(
    io: &mut dyn PhysicalPadIo,
    mgr: &Mutex<PadManager>,
    router: &LocalFeedbackRouter,
    target: &FeedbackTarget,
    pad: &PadId,
    decoder: &Decoder,
    stop: &AtomicBool,
    unit_key: &str,
) {
    let mut wire_known = false;
    // Switch Pro: the pad's own stick calibration once its SPI replies arrive.
    let mut cal = switch_pro::StickCal::default();
    while !stop.load(Ordering::SeqCst) {
        let report = match io.read(READ_TIMEOUT_MS) {
            Ok(Some(r)) => r,
            Ok(None) => continue,
            Err(e) => {
                // Unplugged or the handle broke: the next sync re-opens it while routed.
                log::info!("pad {pad}: physical pad {unit_key} read stopped: {e}");
                break;
            }
        };
        // Decode outside the manager lock.
        let state = match decoder {
            Decoder::Ps(_) => {
                if matches!(report.first(), Some(0x31) | Some(0x11)) {
                    target.bluetooth.store(true, Ordering::SeqCst);
                }
                None
            }
            Decoder::Switch => {
                if let Some((addr, data)) = switch_pro::parse_spi_reply(&report) {
                    if cal.apply_reply(addr, data) {
                        log::info!("pad {pad}: Switch Pro stick calibration read (0x{addr:04X})");
                    }
                    continue;
                }
                match switch_pro::parse_input_cal(&report, Some(&cal)) {
                    Some(s) => Some(s),
                    None => continue,
                }
            }
            Decoder::Hid { profile, layout } => match io.hid_sample(&report) {
                Some(sample) => Some(generic::map_sample(*profile, layout, &sample)),
                None => continue,
            },
        };
        let mut m = lock(mgr);
        if !wire_known && let Some(w) = m.wire_kind(pad) {
            router.set_wire(pad, w);
            wire_known = true;
        }
        let r = match (decoder, state) {
            (Decoder::Ps(kind), _) => forward_physical(&mut m, pad, *kind, &report),
            (_, Some(s)) => forward_state(&mut m, pad, &s),
            (_, None) => Ok(false),
        };
        if let Err(e) = r {
            log::debug!("pad {pad}: forward: {e}");
        }
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

    // ---- hub on the shared PadManager ----

    use seat_pads::fake::{FakeBackend as PadsFake, FakeClock, FakeDeviceTree, FakePadCall, FakeProcessProbe, MemOwnerStore};
    use std::collections::VecDeque;
    use std::time::{Duration, Instant};

    #[derive(Default)]
    struct Phys {
        reports: Mutex<VecDeque<Vec<u8>>>,
        writes: Mutex<Vec<Vec<u8>>>,
        fail_open: std::sync::atomic::AtomicBool,
        /// Descriptor of a generic HID pad (None: not decodable).
        hid: Mutex<Option<HidLayout>>,
        /// `rumble_fallback` calls (XUSB / HID PID path).
        fallback: Mutex<Vec<(u16, u16)>>,
    }

    struct PhysIo(Arc<Phys>);
    struct PhysWriter(Arc<Phys>);

    impl PhysicalPadIo for PhysIo {
        fn read(&mut self, timeout_ms: u32) -> Result<Option<Vec<u8>>, String> {
            if let Some(r) = self.0.reports.lock().unwrap().pop_front() {
                return Ok(Some(r));
            }
            std::thread::sleep(Duration::from_millis(u64::from(timeout_ms.min(5))));
            Ok(None)
        }
        fn writer(&self) -> Arc<dyn PhysicalPadWriter> {
            Arc::new(PhysWriter(self.0.clone()))
        }
        fn hid_layout(&self) -> Option<HidLayout> {
            self.0.hid.lock().unwrap().clone()
        }
        /// Test decoding: byte 0 = report id 0x01, byte 1 = pressed button number,
        /// bytes 2..4 = X (u16, 0..=65535).
        fn hid_sample(&self, r: &[u8]) -> Option<HidSample> {
            if r.len() < 4 || r[0] != 0x01 {
                return None;
            }
            let x = i32::from(u16::from_le_bytes([r[2], r[3]]));
            Some(HidSample {
                buttons: if r[1] != 0 { vec![u16::from(r[1])] } else { vec![] },
                consumer: vec![],
                values: vec![generic::HidValue { page: 1, usage: generic::USAGE_X, value: x, min: 0, max: 65535 }],
            })
        }
    }

    impl PhysicalPadWriter for PhysWriter {
        fn write(&self, report: &[u8]) -> Result<(), String> {
            self.0.writes.lock().unwrap().push(report.to_vec());
            Ok(())
        }
        fn rumble_fallback(&self, strong: u16, weak: u16) -> Result<(), String> {
            self.0.fallback.lock().unwrap().push((strong, weak));
            Ok(())
        }
    }

    struct PhysSource(Arc<Phys>);

    impl PhysicalPadSource for PhysSource {
        fn open(&self, _path: &str) -> Result<Box<dyn PhysicalPadIo>, String> {
            if self.0.fail_open.load(Ordering::SeqCst) {
                return Err("busy".into());
            }
            Ok(Box::new(PhysIo(self.0.clone())))
        }
    }

    struct HubRig {
        hub: LocalPadHub,
        router: Arc<LocalFeedbackRouter>,
        phys: Arc<Phys>,
        vigem: Arc<PadsFake>,
        usbip: Arc<PadsFake>,
        tree: Arc<FakeDeviceTree>,
    }

    fn hub_rig(usbip_installed: bool) -> HubRig {
        let tree = Arc::new(FakeDeviceTree::default());
        let vigem = Arc::new(PadsFake::new(BackendId::Vigem, &[PadKind::X360, PadKind::Ds4], Some(tree.clone())));
        let usbip = Arc::new(PadsFake::new(BackendId::UsbipDs5, &[PadKind::Ds5, PadKind::Ds5Edge], Some(tree.clone())));
        usbip.installed.store(usbip_installed, Ordering::SeqCst);
        let router = Arc::new(LocalFeedbackRouter::new());
        let mgr = PadManager::new(
            vec![vigem.clone(), usbip.clone()],
            Box::new(MemOwnerStore::default()),
            tree.clone(),
            Arc::new(FakeProcessProbe::default()),
            Arc::new(FakeClock::default()),
            Arc::new(seat_pads::channel::MemChannelFactory::default()),
            router.clone(),
            seat_pads::PadManagerConfig::default(),
        );
        let phys = Arc::new(Phys::default());
        let hub = LocalPadHub::new(Arc::new(Mutex::new(mgr)), Arc::new(PhysSource(phys.clone())), router.clone());
        HubRig { hub, router, phys, vigem, usbip, tree }
    }

    fn ds5(key: &str) -> RoutedPad {
        RoutedPad {
            unit_key: key.into(),
            interface_path: format!("\\\\?\\hid#{key}"),
            family: PadFamily::Hid,
            vendor_id: 0x054C,
            product_id: 0x0CE6,
            bus: PadBus::Usb,
        }
    }

    fn seat(n: u8, session: u32, pads: Vec<RoutedPad>) -> LocalPadSeat {
        LocalPadSeat { seat: n, session_id: session, owner_sid: "S-1-5-21-1-2-3-1001".into(), pads }
    }

    fn wait(what: &str, f: impl Fn() -> bool) {
        let t = Instant::now();
        while !f() {
            assert!(t.elapsed() < Duration::from_secs(5), "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn usb_ds5() -> Vec<u8> {
        let mut r = vec![0u8; 64];
        r[0] = 0x01;
        r[1..5].copy_from_slice(&[0x80, 0x80, 0x80, 0x80]);
        r[8] = 0x08 | 0x20; // hat neutral + cross
        r
    }

    #[test]
    fn local_seat_pad_ids() {
        let id = local_seat_pad_id(3);
        assert_eq!(id, "5053454c-4f43-4100-8000-000000000003");
        assert!(PadId::new(&id, 0).is_ok());
        assert!(is_local_seat_pad_id(&id) && is_local_seat_pad_id(&id.to_ascii_uppercase()));
        assert!(!is_local_seat_pad_id("0a1b2c3d-0000-4000-8000-00000000abcd"));
        let xbox = RoutedPad { family: PadFamily::XInput, vendor_id: 0x045E, product_id: 0x0B12, ..ds5("x") };
        assert_eq!(xbox.layout(), PhysicalLayout::Hid { xinput: true });
        assert_eq!(xbox.mirror(), PadKind::XOne);
        assert_eq!(ds5("d").layout(), PhysicalLayout::PlayStation(PadKind::Ds5));
        let sw = RoutedPad { vendor_id: 0x057E, product_id: 0x2009, ..ds5("s") };
        assert_eq!(sw.layout(), PhysicalLayout::SwitchPro);
        let generic_pad = RoutedPad { vendor_id: 0x1234, product_id: 0x5678, ..ds5("g") };
        assert_eq!(generic_pad.layout(), PhysicalLayout::Hid { xinput: false });
        assert_eq!(PadBus::from_info("BTHLE"), PadBus::Bluetooth);
        assert_eq!(PadBus::from_info("usb"), PadBus::Usb);
        assert_eq!(PadBus::from_info("i2c"), PadBus::Other);
    }

    #[test]
    fn hub_same_kind_passes_reports_and_output_through() {
        let mut r = hub_rig(true);
        let notes = r.hub.sync(&[seat(2, 4, vec![ds5("hid;inst=DS5")])]);
        assert!(matches!(&notes[..], [LocalPadNote::Attached { .. }]), "{notes:?}");
        let pad = PadId::new(&local_seat_pad_id(2), 0).unwrap();
        // Seat-owned: created with the seat's session and user (securing is PadManager's).
        assert!(r.usbip.calls().contains(&FakePadCall::Created(pad.clone(), PadKind::Ds5)));
        let inst = PadsFake::instance_for(PadKind::Ds5, &pad);
        assert_eq!(r.tree.session_of(&inst), Some(4));
        assert!(r.tree.security_of(&inst).unwrap().contains("S-1-5-21-1-2-3-1001"));
        // Input: USB report unchanged.
        r.phys.reports.lock().unwrap().push_back(usb_ds5());
        wait("report", || r.usbip.calls().iter().any(|c| matches!(c, FakePadCall::Report(p, b) if *p == pad && b[8] == 0x28)));
        // Output report of the game goes to the physical pad unchanged.
        let fb = r.usbip.sink(&pad).unwrap();
        let mut out = vec![0u8; 48];
        out[0] = 0x02;
        out[1] = 0x03;
        fb.raw_output(&pad, layout::SOURCE_WRITE_REPORT, &out);
        assert_eq!(r.phys.writes.lock().unwrap().last(), Some(&out));
        // Parsed feedback is not written a second time.
        fb.feedback(&pad, Feedback::Rumble { strong: 0xFFFF, weak: 0 });
        assert_eq!(r.phys.writes.lock().unwrap().len(), 1);
        // Unrouted: removed at once, no target left.
        let notes = r.hub.sync(&[]);
        assert_eq!(notes, vec![LocalPadNote::Removed { unit_key: "hid;inst=DS5".into() }]);
        assert!(r.usbip.calls().contains(&FakePadCall::Removed(pad)));
        assert_eq!(r.router.targets(), 0);
        assert!(r.hub.attached().is_empty());
    }

    #[test]
    fn hub_other_kind_parses_input_and_rebuilds_feedback() {
        // Only ViGEm: the DualSense becomes a ViGEm pad of another kind.
        let mut r = hub_rig(false);
        let notes = r.hub.sync(&[seat(2, 4, vec![ds5("hid;inst=DS5")])]);
        assert!(matches!(&notes[..], [LocalPadNote::Attached { .. }]), "{notes:?}");
        let pad = PadId::new(&local_seat_pad_id(2), 0).unwrap();
        let wire = match r.vigem.calls().first() {
            Some(FakePadCall::Created(_, k)) => *k,
            other => panic!("{other:?}"),
        };
        assert_ne!(wire, PadKind::Ds5);
        // Bluetooth report: cut to USB, parsed, submitted as a state.
        let usb = usb_ds5();
        let mut bt = vec![0x31, 0x10];
        bt.extend_from_slice(&usb[1..]);
        bt.extend_from_slice(&[0; 13]);
        r.phys.reports.lock().unwrap().push_back(bt);
        wait("state", || {
            r.vigem.calls().iter().any(|c| matches!(c, FakePadCall::Submit(p, s) if *p == pad && s.buttons & buttons::SOUTH != 0))
        });
        // Bluetooth pad: the rebuilt report goes out as 0x31 with its CRC (2026-11-17).
        let fb = r.vigem.sink(&pad).unwrap();
        fb.feedback(&pad, Feedback::Rumble { strong: 0xFFFF, weak: 0x1000 });
        let w = r.phys.writes.lock().unwrap().clone();
        assert_eq!(w.len(), 1);
        assert_eq!((w[0][0], w[0].len()), (0x31, seat_pads::bt::PS_BT_OUTPUT_LEN));
        assert!(seat_pads::bt::ps_crc_ok(seat_pads::bt::PS_OUTPUT_CRC_SEED, &w[0]));
        // The USB body inside: rumble motors at USB offsets 3 / 4 (+2 for the BT header).
        assert_eq!((w[0][5], w[0][6]), (0x10, 0xFF));
        r.hub.shutdown();
        assert!(r.vigem.calls().contains(&FakePadCall::Removed(pad)));
    }

    #[test]
    fn hub_usb_other_kind_rebuilds_ds5_output() {
        let mut r = hub_rig(false);
        r.hub.sync(&[seat(2, 4, vec![ds5("hid;inst=DS5")])]);
        let pad = PadId::new(&local_seat_pad_id(2), 0).unwrap();
        r.phys.reports.lock().unwrap().push_back(usb_ds5());
        wait("state", || r.vigem.calls().iter().any(|c| matches!(c, FakePadCall::Submit(..))));
        let fb = r.vigem.sink(&pad).unwrap();
        fb.feedback(&pad, Feedback::Rumble { strong: 0xFF00, weak: 0x1000 });
        fb.feedback(&pad, Feedback::Led { r: 1, g: 2, b: 3 });
        let w = r.phys.writes.lock().unwrap().clone();
        assert_eq!(w.len(), 2);
        let parsed = seat_pads::reports::parse_output(PadKind::Ds5, &w[1]);
        assert!(parsed.contains(&Feedback::Rumble { strong: 0xFFFF, weak: 0x1010 }));
        assert!(parsed.contains(&Feedback::Led { r: 1, g: 2, b: 3 }));
    }

    #[test]
    fn hub_bluetooth_same_kind_rewraps_output() {
        let mut r = hub_rig(true);
        let bt_pad = RoutedPad { bus: PadBus::Bluetooth, ..ds5("hid;inst=DS5BT") };
        r.hub.sync(&[seat(2, 4, vec![bt_pad])]);
        let pad = PadId::new(&local_seat_pad_id(2), 0).unwrap();
        r.phys.reports.lock().unwrap().push_back(usb_ds5());
        wait("report", || r.usbip.calls().iter().any(|c| matches!(c, FakePadCall::Report(..))));
        let fb = r.usbip.sink(&pad).unwrap();
        let mut out = vec![0u8; 48];
        out[0] = 0x02;
        out[1] = 0x0C;
        out[11] = 0x26; // a trigger effect byte
        fb.raw_output(&pad, layout::SOURCE_WRITE_REPORT, &out);
        let w = r.phys.writes.lock().unwrap().clone();
        assert_eq!(w.len(), 1);
        assert_eq!(w[0][0], 0x31);
        assert_eq!(&w[0][3..50], &out[1..48]);
        assert!(seat_pads::bt::ps_crc_ok(seat_pads::bt::PS_OUTPUT_CRC_SEED, &w[0]));
    }

    #[test]
    fn hub_switch_pro_init_input_and_rumble() {
        let mut r = hub_rig(false);
        let sw = RoutedPad { vendor_id: 0x057E, product_id: 0x2009, ..ds5("hid;inst=SW") };
        let notes = r.hub.sync(&[seat(2, 4, vec![sw])]);
        assert!(matches!(&notes[..], [LocalPadNote::Attached { .. }]), "{notes:?}");
        // USB init: four 0x80 commands, four sub-commands, then two SPI calibration reads.
        let w = r.phys.writes.lock().unwrap().clone();
        assert_eq!(w.len(), 10);
        assert_eq!(w[0], vec![0x80, 0x02]);
        assert_eq!((w[4][0], w[4][10], w[4][11]), (0x01, 0x03, 0x30));
        assert_eq!((w[8][10], w[8][11], w[8][12]), (seat_pads::switch_pro::subcmd::SPI_READ, 0x3D, 0x60));
        assert_eq!((w[9][10], w[9][11], w[9][12]), (seat_pads::switch_pro::subcmd::SPI_READ, 0x10, 0x80));
        let pad = PadId::new(&local_seat_pad_id(2), 0).unwrap();
        let mut rep = vec![0u8; 49];
        rep[0] = 0x30;
        rep[3] = 0x04; // B = south
        r.phys.reports.lock().unwrap().push_back(rep);
        wait("state", || {
            r.vigem.calls().iter().any(|c| matches!(c, FakePadCall::Submit(p, s) if *p == pad && s.buttons & buttons::SOUTH != 0))
        });
        let fb = r.vigem.sink(&pad).unwrap();
        fb.feedback(&pad, Feedback::Rumble { strong: 0xFFFF, weak: 0 });
        let last = r.phys.writes.lock().unwrap().last().cloned().unwrap();
        assert_eq!(last[0], 0x10);
        assert_eq!(&last[2..6], &seat_pads::switch_pro::rumble_side(0xFFFF));
    }

    #[test]
    fn hub_generic_and_xbox_bluetooth_pads() {
        let mut r = hub_rig(false);
        *r.phys.hid.lock().unwrap() = Some(HidLayout { values: vec![(1, generic::USAGE_X)], button_count: 12 });
        let xb = RoutedPad {
            family: PadFamily::XInput,
            vendor_id: 0x045E,
            product_id: 0x0B13,
            bus: PadBus::Bluetooth,
            ..ds5("hid;inst=XBT")
        };
        let notes = r.hub.sync(&[seat(2, 4, vec![xb])]);
        assert!(matches!(&notes[..], [LocalPadNote::Attached { .. }]), "{notes:?}");
        let pad = PadId::new(&local_seat_pad_id(2), 0).unwrap();
        r.phys.reports.lock().unwrap().push_back(vec![0x01, 1, 0xFF, 0xFF]);
        wait("state", || {
            r.vigem.calls().iter().any(|c| matches!(c, FakePadCall::Submit(p, s) if *p == pad && s.buttons & buttons::SOUTH != 0 && s.lx == 32767))
        });
        let fb = r.vigem.sink(&pad).unwrap();
        fb.feedback(&pad, Feedback::Rumble { strong: 0xFFFF, weak: 0 });
        fb.feedback(&pad, Feedback::TriggerRumble { left: 0, right: 0xFFFF });
        let w = r.phys.writes.lock().unwrap().clone();
        assert_eq!(w.last().unwrap(), &seat_pads::bt::xbox_bt_output(0xFFFF, 0, 0, 0xFFFF).to_vec());
        assert!(r.phys.fallback.lock().unwrap().is_empty(), "Bluetooth Xbox pads use their output report");
        // A generic USB HID pad: no output report of its own; rumble goes to the fallback
        // path (HID PID force feedback / XUSB, parity review 2026-11-17).
        r.hub.sync(&[]);
        let g = RoutedPad { vendor_id: 0x1234, product_id: 0x5678, ..ds5("hid;inst=GEN") };
        r.hub.sync(&[seat(3, 5, vec![g])]);
        let before = r.phys.writes.lock().unwrap().len();
        let pad3 = PadId::new(&local_seat_pad_id(3), 0).unwrap();
        r.vigem.sink(&pad3).unwrap().feedback(&pad3, Feedback::Rumble { strong: 0xFFFF, weak: 0 });
        assert_eq!(r.phys.writes.lock().unwrap().len(), before);
        assert_eq!(*r.phys.fallback.lock().unwrap(), vec![(0xFFFF, 0)]);
    }

    #[test]
    fn hub_session_change_unsupported_and_open_failures() {
        let mut r = hub_rig(true);
        let xbox = RoutedPad { family: PadFamily::XInput, vendor_id: 0x045E, product_id: 0x0B12, ..ds5("hid;inst=XB") };
        let want = vec![seat(2, 4, vec![ds5("hid;inst=DS5"), xbox.clone()])];
        let notes = r.hub.sync(&want);
        assert!(notes.contains(&LocalPadNote::Unsupported { unit_key: "hid;inst=XB".into() }));
        // Reported once, not every tick.
        assert!(r.hub.sync(&want).is_empty());
        // Another user / session on the seat: the pad is created again for it.
        let notes = r.hub.sync(&[seat(2, 9, vec![ds5("hid;inst=DS5"), xbox])]);
        assert!(notes.contains(&LocalPadNote::Removed { unit_key: "hid;inst=DS5".into() }));
        assert!(notes.iter().any(|n| matches!(n, LocalPadNote::Attached { .. })));
        let pad = PadId::new(&local_seat_pad_id(2), 0).unwrap();
        assert_eq!(r.tree.session_of(&PadsFake::instance_for(PadKind::Ds5, &pad)), Some(9));
        // A pad another program holds exclusively: one note, retried on later syncs.
        r.hub.sync(&[]);
        r.phys.fail_open.store(true, Ordering::SeqCst);
        let w = vec![seat(3, 5, vec![ds5("hid;inst=B")])];
        assert!(matches!(&r.hub.sync(&w)[..], [LocalPadNote::OpenFailed { .. }]));
        assert!(r.hub.sync(&w).is_empty());
        r.phys.fail_open.store(false, Ordering::SeqCst);
        assert!(matches!(&r.hub.sync(&w)[..], [LocalPadNote::Attached { .. }]));
    }
}