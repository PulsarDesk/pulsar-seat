// SPDX-License-Identifier: GPL-3.0-only
//! Core public types and traits (contracts.md §5.1).

use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::error::PadError;
use crate::layout;

/// Kind of a virtual pad. Serde names are the wire ids of spec 60 §3.5 (`ds4`, `ds5`,
/// `ds5e`, `xone`) plus `x360` for the ViGEm Xbox 360 target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum PadKind {
    #[serde(rename = "x360")]
    X360,
    #[serde(rename = "ds4")]
    Ds4,
    #[serde(rename = "ds5")]
    Ds5,
    #[serde(rename = "ds5e")]
    Ds5Edge,
    #[serde(rename = "xone")]
    XOne,
}

impl PadKind {
    pub const fn wire_name(self) -> &'static str {
        match self {
            PadKind::X360 => "x360",
            PadKind::Ds4 => "ds4",
            PadKind::Ds5 => "ds5",
            PadKind::Ds5Edge => "ds5e",
            PadKind::XOne => "xone",
        }
    }

    /// `PKEY_PulsarPad_Kind` / input-section kind value. X360 has none (ViGEm only) and
    /// is reported as the Xbox kind.
    pub const fn kind_value(self) -> u32 {
        match self {
            PadKind::X360 | PadKind::XOne => layout::KIND_XONE,
            PadKind::Ds4 => layout::KIND_DS4,
            PadKind::Ds5 => layout::KIND_DS5,
            PadKind::Ds5Edge => layout::KIND_DS5E,
        }
    }

    pub const fn usb_ids(self) -> (u16, u16) {
        match self {
            PadKind::X360 => (layout::VID_MICROSOFT, layout::PID_X360),
            PadKind::XOne => (layout::VID_MICROSOFT, layout::PID_XONE),
            PadKind::Ds4 => (layout::VID_SONY, layout::PID_DS4),
            PadKind::Ds5 => (layout::VID_SONY, layout::PID_DS5),
            PadKind::Ds5Edge => (layout::VID_SONY, layout::PID_DS5E),
        }
    }

    pub const fn is_playstation(self) -> bool {
        matches!(self, PadKind::Ds4 | PadKind::Ds5 | PadKind::Ds5Edge)
    }

    /// Input report length (with id) a stream server writes for this kind.
    pub const fn input_len(self) -> usize {
        match self {
            PadKind::X360 | PadKind::XOne => layout::XONE_INPUT_LEN,
            _ => layout::DS_INPUT_LEN,
        }
    }
}

impl fmt::Display for PadKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.wire_name())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendId {
    Vigem,
    UsbipDs5,
    PulsarPad,
}

/// `<seat32>-<slot:02X>` (spec 60 §3.6).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PadId {
    pub seat32: String,
    pub slot: u8,
}

impl PadId {
    /// Builds a pad id from a seat GUID (any case, with or without braces/dashes).
    pub fn new(seat_id: &str, slot: u8) -> Result<Self, PadError> {
        Ok(PadId { seat32: crate::ids::seat32(seat_id)?, slot })
    }
}

impl fmt::Display for PadId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{:02X}", self.seat32, self.slot)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendAvailability {
    pub id: BackendId,
    pub installed: bool,
    pub version: Option<String>,
    /// The driver is signed so it loads with Memory Integrity on. For kernel drivers we
    /// report `true` only when the driver service is running (a running kernel driver
    /// passed code integrity); for our own driver always `false` (unsigned).
    pub signed: bool,
    pub kinds: Vec<PadKind>,
    /// Why it is not usable (or a caveat), English, for logs; the manager maps `id` +
    /// state to Turkish copy.
    pub reason: Option<String>,
}

impl BackendAvailability {
    pub fn usable(&self) -> bool {
        self.installed && self.reason.as_deref().is_none_or(|r| !r.starts_with("unusable"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PadCaps {
    pub rumble: bool,
    pub trigger_rumble: bool,
    pub touchpad: bool,
    pub motion: bool,
    pub battery: bool,
    pub rgb_led: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PadCreate {
    pub seat_id: String,
    pub slot: u8,
    pub kind: PadKind,
    pub session_id: u32,
    pub owner_sid: String,
    pub caps: PadCaps,
    /// Channel base the manager created for this pad instance (`ids::channel_name`);
    /// PulsarPad hands it to the driver. Empty = `ids::channel_base` (tests, tools).
    pub channel: String,
}

impl PadCreate {
    pub fn pad_id(&self) -> Result<PadId, PadError> {
        PadId::new(&self.seat_id, self.slot)
    }
}

/// One touch point, coordinates normalised to 0..=65535 over the touchpad surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Touch {
    /// Finger tracking id (0..=127).
    pub id: u8,
    pub x: u16,
    pub y: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Battery {
    /// 0..=100 percent.
    pub percent: u8,
    pub charging: bool,
    pub full: bool,
}

/// Button bits of [`PadState::buttons`]. Names are positional (south = A / cross).
pub mod buttons {
    pub const SOUTH: u32 = 1 << 0;
    pub const EAST: u32 = 1 << 1;
    pub const WEST: u32 = 1 << 2;
    pub const NORTH: u32 = 1 << 3;
    pub const L1: u32 = 1 << 4;
    pub const R1: u32 = 1 << 5;
    pub const L3: u32 = 1 << 6;
    pub const R3: u32 = 1 << 7;
    /// Back / View / Share / Create.
    pub const BACK: u32 = 1 << 8;
    /// Start / Menu / Options.
    pub const START: u32 = 1 << 9;
    /// Guide / Xbox / PS.
    pub const GUIDE: u32 = 1 << 10;
    pub const TOUCHPAD: u32 = 1 << 11;
    /// Misc (Moonlight "misc": DualSense mic button, Xbox share).
    pub const MISC: u32 = 1 << 12;
    pub const DPAD_UP: u32 = 1 << 13;
    pub const DPAD_DOWN: u32 = 1 << 14;
    pub const DPAD_LEFT: u32 = 1 << 15;
    pub const DPAD_RIGHT: u32 = 1 << 16;
    pub const PADDLE1: u32 = 1 << 17;
    pub const PADDLE2: u32 = 1 << 18;
    pub const PADDLE3: u32 = 1 << 19;
    pub const PADDLE4: u32 = 1 << 20;
    /// DualSense Edge function buttons.
    pub const FN_LEFT: u32 = 1 << 21;
    pub const FN_RIGHT: u32 = 1 << 22;
}

/// Current version of [`PadState`].
pub const PAD_STATE_VERSION: u16 = 1;

/// Internal versioned pad state (spec 60 §5.2).
///
/// Units: sticks signed, +y = up (Moonlight convention); triggers 0..=65535; `accel` in
/// raw units with [`crate::features::CAL_ACCEL_1G`] per g; `gyro` in raw units with
/// [`crate::features::CAL_GYRO_RANGE`] per [`crate::features::CAL_GYRO_SPEED`] deg/s
/// (matches the calibration we report); `motion_ts_us` microseconds, any epoch.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PadState {
    pub v: u16,
    pub buttons: u32,
    pub lx: i16,
    pub ly: i16,
    pub rx: i16,
    pub ry: i16,
    pub lt: u16,
    pub rt: u16,
    pub accel: [i16; 3],
    pub gyro: [i16; 3],
    pub motion_ts_us: u64,
    pub touch: [Option<Touch>; 2],
    pub battery: Option<Battery>,
}

impl PadState {
    pub fn neutral() -> Self {
        PadState { v: PAD_STATE_VERSION, ..Default::default() }
    }

    pub fn pressed(&self, bit: u32) -> bool {
        self.buttons & bit != 0
    }
}

/// Feedback from a game to the client controller.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Feedback {
    Rumble { strong: u16, weak: u16 },
    TriggerRumble { left: u16, right: u16 },
    Led { r: u8, g: u8, b: u8 },
    /// DualSense adaptive-trigger effect blocks: `raw[0]` = bit 0 right valid, bit 1 left
    /// valid; `raw[1..12]` right trigger (mode + 10 parameters), `raw[12..23]` left.
    AdaptiveTrigger { raw: Vec<u8> },
    PlayerLeds { mask: u8 },
}

/// Where feedback for the client goes (integrator: stream server, seat pipe, output ring).
pub trait FeedbackSink: Send + Sync {
    fn feedback(&self, pad: &PadId, f: Feedback);
    /// Raw output report in the layout of the pad's wire kind (byte 0 = report id), for
    /// the shared output ring (spec 60 §4.5). `source` = `layout::SOURCE_*`.
    fn raw_output(&self, _pad: &PadId, _source: u8, _report: &[u8]) {}
    /// PCM written by a game to the pad's speaker endpoint (DualSense audio haptics),
    /// interleaved signed 16-bit little-endian.
    fn haptics_pcm(&self, _pad: &PadId, _channels: u8, _rate_hz: u32, _pcm: &[u8]) {}
}

/// A sink that drops everything.
pub struct NullSink;
impl FeedbackSink for NullSink {
    fn feedback(&self, _pad: &PadId, _f: Feedback) {}
}

pub trait VirtualPad: Send {
    fn id(&self) -> &PadId;
    fn kind(&self) -> PadKind;
    /// Device-node instance id once known.
    fn instance_id(&self) -> Option<String>;
    fn submit(&mut self, s: &PadState) -> Result<(), PadError>;
    /// Raw HID input report in the layout of the pad's wire kind (byte 0 = report id), as
    /// the stream server writes it into the input section. Backends convert as needed.
    fn submit_report(&mut self, report: &[u8]) -> Result<(), PadError>;
    /// `true` when the device driver reads the shared channel itself (PulsarPad); the
    /// manager then writes input into the channel instead of calling `submit*`.
    fn driver_reads_channel(&self) -> bool {
        false
    }
    /// Looks the device node up again when it was not known yet (or may have changed);
    /// the manager calls it before each securing pass.
    fn refresh_instance(&mut self) -> Option<String> {
        self.instance_id()
    }
    fn remove(self: Box<Self>) -> Result<(), PadError>;
}

pub trait PadBackend: Send + Sync {
    fn id(&self) -> BackendId;
    fn probe(&self) -> BackendAvailability;
    fn create(&self, req: &PadCreate, fb: Arc<dyn FeedbackSink>) -> Result<Box<dyn VirtualPad>, PadError>;
    /// `true` when `create` / `remove` can block for seconds (usbip-win2 CLI runs); the
    /// manager then runs them on a worker thread so the pad loop keeps pumping.
    fn blocking_ops(&self) -> bool {
        false
    }
}

/// Injected time (same shape in every crate).
pub trait Clock: Send + Sync {
    fn now_ms(&self) -> u64;
}

/// Wall clock (ms since the Unix epoch).
pub struct SystemClock;
impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_wire_names() {
        assert_eq!(serde_json::to_string(&PadKind::Ds5Edge).unwrap(), "\"ds5e\"");
        assert_eq!(serde_json::to_string(&PadKind::XOne).unwrap(), "\"xone\"");
        assert_eq!(serde_json::from_str::<PadKind>("\"ds4\"").unwrap(), PadKind::Ds4);
        assert_eq!(serde_json::to_string(&BackendId::UsbipDs5).unwrap(), "\"usbip_ds5\"");
    }

    #[test]
    fn pad_id_display() {
        let p = PadId::new("{0a1b2c3d-0000-4000-8000-00000000abcd}", 3).unwrap();
        assert_eq!(p.to_string(), "0A1B2C3D00004000800000000000ABCD-03");
    }

    #[test]
    fn feedback_json() {
        let j = serde_json::to_string(&Feedback::Rumble { strong: 1, weak: 2 }).unwrap();
        assert_eq!(j, r#"{"type":"rumble","strong":1,"weak":2}"#);
    }
}
