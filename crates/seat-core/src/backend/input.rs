// SPDX-License-Identifier: GPL-3.0-only
//! Input backend: keyboards, mice and HID collections routed to their seat only (spec 50
//! §4). Implemented in `seat-input`.
//!
//! - [`InputRouter`] runs in the **service** (route table, activity, wizard capture,
//!   hotkey). With `RawInputHost` the actual reading happens in each seat host (raw input
//!   sink in the host session, filtered by the seat's interface paths from
//!   `host_get_assignment`); with `KmdfFilter` the service reads packets from our filter by
//!   inverted call and forwards them to the seat host as `input_packets` events.
//! - [`InputSink`] runs in the **seat host**: it feeds packets into the seat's RDP input
//!   channels (scan codes, pointer, touch / pen, SAS for Ctrl+Alt+Del).

use super::{Availability, BackendResult, DriverId, HotplugSink};
use crate::device::Device;
use crate::ids::{DeviceKey, SeatMask, SeatNumber};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputBackendKind {
    /// Default, user mode: raw input (`RIDEV_INPUTSINK | RIDEV_NOLEGACY` on our own windows)
    /// in the host session, demultiplexed by device. Isolates the console only under
    /// `HostTopology::DedicatedHost` (the host session's foreground window is always ours);
    /// under `SeatOneConsole` other seats' keys also reach seat 1 (manager says so).
    #[default]
    RawInputHost,
    /// Our KMDF filters (`drivers/seat-input-filter`, one driver with keyboard / mouse and HID roles): full isolation.
    /// Unloaded until signed.
    KmdfFilter,
    /// No local input routing (fast user switching; or signing unavailable, spec 50 §4.2).
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputCaps {
    /// Other seats' input never reaches the console session.
    pub isolates_console: bool,
    /// Per-seat visibility of HID collections (pads, touch, pen) by session.
    pub hid_visibility: bool,
    /// Per-device activity reports (identify by touch).
    pub activity: bool,
    /// Ctrl+key move-to-next-seat hotkey from any seat's keyboard.
    pub hotkey: bool,
    /// Wizard capture: unassigned units go to the service only.
    pub wizard_capture: bool,
    pub driver: Option<DriverId>,
}

/// Route of one unit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputRoute {
    pub key: DeviceKey,
    /// Effective mask (exclusive kinds: one seat or spare; shareable HID: any).
    pub mask: SeatMask,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RouteReport {
    /// Units whose devnode property / filter state was updated.
    pub applied: Vec<DeviceKey>,
    /// Units the backend cannot route (shown "not routable").
    pub not_routable: Vec<DeviceKey>,
    /// Open HID handles that survive until replug (spec 50 §4.1.3).
    pub needs_replug: Vec<DeviceKey>,
}

/// Key flags of [`InputPacket::Key`] (bits).
pub mod key_flags {
    pub const UP: u8 = 1;
    /// Extended key (E0 prefix).
    pub const E0: u8 = 2;
    /// E1 prefix (Pause).
    pub const E1: u8 = 4;
}

/// Mouse button bits of [`InputPacket::Buttons`].
pub mod buttons {
    pub const LEFT: u8 = 1;
    pub const RIGHT: u8 = 2;
    pub const MIDDLE: u8 = 4;
    pub const X1: u8 = 8;
    pub const X2: u8 = 16;
}

/// One input event, device-neutral. Serialized compactly (short tags) because it also
/// travels as `input_packets` events on the pipe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum InputPacket {
    /// Set-1 scan code + [`key_flags`].
    Key { sc: u16, f: u8 },
    /// Relative motion.
    Move { dx: i32, dy: i32 },
    /// Absolute position normalized to 0..=65535 over the seat's output `o`.
    Abs { x: u16, y: u16, o: u8 },
    /// Buttons pressed (`down`) / released (`up`) in this packet ([`buttons`] bits).
    Buttons { down: u8, up: u8 },
    /// Wheel delta (120 = one notch); `h` = horizontal.
    Wheel { d: i16, h: bool },
    /// Touch / pen frame: contacts in output-normalized coordinates.
    Touch { o: u8, contacts: Vec<TouchContact> },
    /// HID consumer-page usage (volume, media) for the seat's own session.
    Consumer { usage: u16, down: bool },
    /// Secure attention (Ctrl+Alt+Del from the seat's keyboard) → RDP SAS.
    Sas,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TouchContact {
    pub id: u32,
    pub x: u16,
    pub y: u16,
    /// `down`, `update`, `up`, `hover` (pen).
    pub phase: String,
    /// Pen only: pressure 0..=1024, tilt in degrees, eraser.
    #[serde(default)]
    pub pressure: u16,
    #[serde(default)]
    pub pen: bool,
}

/// Per-unit activity (identify by touch, spec 10 §3.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityEvent {
    pub key: DeviceKey,
    pub at_ms: u64,
    /// Wizard capture only: the key pressed (scan code) / mouse click on output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_sc: Option<u16>,
    #[serde(default)]
    pub click: bool,
}

pub type ActivitySink = Arc<dyn Fn(ActivityEvent) + Send + Sync>;
/// Packets for a non-console seat (KmdfFilter path): the service forwards them to the
/// seat's host.
pub type PacketSink = Arc<dyn Fn(SeatNumber, &[InputPacket]) + Send + Sync>;

/// Configured move-to-next-seat hotkey: Ctrl + `scan_code` (spec 50 §4.1.9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotkeyEvent {
    pub key: DeviceKey,
    pub from: SeatNumber,
}

pub type HotkeySink = Arc<dyn Fn(HotkeyEvent) + Send + Sync>;

/// Service-side input backend.
pub trait InputRouter: Send + Sync {
    fn kind(&self) -> InputBackendKind;
    fn caps(&self) -> InputCaps;
    fn availability(&self) -> Availability;
    /// Live units (`Keyboard`, `Mouse`, `Hid` kinds, `DeviceDetails::Input`).
    fn enumerate(&self) -> BackendResult<Vec<Device>>;
    /// Apply the full route table (idempotent). Units not listed go to seat 1.
    fn apply_routes(&self, routes: &[InputRoute]) -> BackendResult<RouteReport>;
    /// Wizard mode on/off (spec 50 §4.1.8).
    fn set_wizard_capture(&self, on: bool) -> BackendResult<()>;
    fn set_hotplug_sink(&self, sink: HotplugSink);
    fn set_activity_sink(&self, sink: ActivitySink);
    fn set_packet_sink(&self, sink: PacketSink);
    fn set_hotkey_sink(&self, sink: HotkeySink);
}

/// Seat-host-side input delivery into the seat's session.
pub trait InputSink: Send {
    fn send(&mut self, packets: &[InputPacket]) -> BackendResult<()>;
    /// Release every key / button that is down (service gap, seat switch: no stuck keys).
    fn release_all(&mut self) -> BackendResult<()>;
}
