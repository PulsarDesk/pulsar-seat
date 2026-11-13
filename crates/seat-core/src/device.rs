// SPDX-License-Identifier: GPL-3.0-only
//! Device model: kinds, share policies and the live inventory record.
//!
//! Every backend's `enumerate()` returns [`Device`] values; the service merges them into
//! one inventory, which the manager shows on the Seats canvas (spec 10 §3).

use crate::ids::{DeviceKey, SeatMask};
use serde::{Deserialize, Serialize};

/// What a device is. The unit of assignment is one of these (spec 50 §4.1.1: one device
/// interface = a keyboard devnode, a mouse devnode or a HID top-level collection).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    /// A monitor on a display target (key = adapter + target, spec 40 §6.1).
    Monitor,
    Keyboard,
    Mouse,
    /// Other HID top-level collection (touch, pen, pad, consumer, system control).
    Hid,
    /// Audio render endpoint.
    AudioRender,
    /// Audio capture endpoint.
    AudioCapture,
    Webcam,
    SmartCardReader,
    /// USB hub (or one port of a hub): ownership is inherited by what is plugged into it.
    UsbHub,
    /// Disk / volume / optical drive.
    Storage,
    Printer,
    SerialPort,
    /// Network adapter: shown, never gated (spec 70 §5.1).
    Network,
    /// Any other USB / PnP device gated by descriptor.
    Other,
}

impl DeviceKind {
    /// Default share policy of the kind (spec 10 §3.4, spec 70 §5.1).
    pub const fn policy(self) -> SharePolicy {
        match self {
            Self::Monitor | Self::Keyboard | Self::Mouse => SharePolicy::Exclusive,
            Self::Hid => SharePolicy::Exclusive,
            Self::Network => SharePolicy::NeverGated,
            _ => SharePolicy::Shareable,
        }
    }
    /// Mask a device of this kind gets when it is new and nothing is inherited
    /// (spec 50 §4.1.7: input to seat 1; spec 40 §6.7: unknown monitor joins the console;
    /// spec 60 §8.1: audio shared; spec 70: other devices shared).
    pub const fn default_mask(self) -> SeatMask {
        match self {
            Self::Monitor | Self::Keyboard | Self::Mouse | Self::Hid => SeatMask::CONSOLE,
            _ => SeatMask::SHARED,
        }
    }
    /// Key prefix used by [`crate::pnp::KeyParts::compose`].
    pub const fn key_prefix(self) -> &'static str {
        match self {
            Self::Monitor => "mon",
            Self::Keyboard => "kbd",
            Self::Mouse => "mou",
            Self::Hid => "hid",
            Self::AudioRender => "aout",
            Self::AudioCapture => "ain",
            Self::Webcam => "cam",
            Self::SmartCardReader => "scard",
            Self::UsbHub => "hub",
            Self::Storage => "stor",
            Self::Printer => "prn",
            Self::SerialPort => "com",
            Self::Network => "net",
            Self::Other => "dev",
        }
    }
    /// Device-interface class GUID the kind is enumerated by (spec 10 §3.1), if any.
    pub const fn interface_class(self) -> Option<&'static str> {
        match self {
            Self::Monitor => Some("e6f07b5f-ee97-4a90-b076-33f57bf4eaa7"),
            Self::Keyboard => Some("884b96c3-56ef-11d1-bc8c-00a0c91405dd"),
            Self::Mouse => Some("378de44c-56ef-11d1-bc8c-00a0c91405dd"),
            Self::Hid => Some("4d1e55b2-f16f-11cf-88cb-001111000030"),
            Self::Webcam => Some("e5323777-f976-4f5b-9b55-b94699c46e44"),
            Self::SmartCardReader => Some("50dd5230-ba8a-11d1-bf5d-0000f805f530"),
            Self::UsbHub => Some("f18a0e88-c30c-11d0-8815-00a0c906bed8"),
            Self::Storage => Some("53f56307-b6bf-11d0-94f2-00a0c91efb8b"),
            _ => None,
        }
    }
}

/// How many seats a device may belong to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SharePolicy {
    /// At most one seat (or spare).
    Exclusive,
    /// Any mask, including shared.
    Shareable,
    /// Shown for information only; always shared, never gated.
    NeverGated,
}

/// One device of the live inventory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Device {
    pub key: DeviceKey,
    pub kind: DeviceKind,
    /// Friendly name (Windows' own, or the endpoint name).
    pub name: String,
    /// Human-readable position: USB port chain `USB-1-2-4`, surface `1.2`, `I2C`, … Label
    /// only, never an identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// PnP device instance id at enumeration time (runtime data, changes across ports).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_id: Option<String>,
    /// Key of the nearest USB hub (or hub port) above the device, for inheritance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_hub: Option<DeviceKey>,
    /// `false` = known from config but not plugged in now.
    pub present: bool,
    /// Secondary identity (spec 70 §5.1): hardware id, serial. Used to offer "move the
    /// assignment to the new port" when a known device turns up elsewhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hardware_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial: Option<String>,
    /// Problems the manager draws as an attention sign.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<DeviceWarning>,
    pub details: DeviceDetails,
}

/// Per-device problems (attention sign on the canvas).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum DeviceWarning {
    /// Two live units share one key (spec 50 §4.1.1): they cannot be split.
    KeyCollision,
    /// A foreign filter sits in the device stack below ours (spec 50 §4.1.6).
    ForeignFilter { name: String },
    /// Another driver hides the device from the service (open fails from session 0).
    HiddenByOtherSoftware,
    /// The backend cannot route / gate this device.
    NotRoutable,
    /// A known device appeared on a new port (offer to move the assignment).
    MovedFrom { old_key: DeviceKey },
}

/// Kind-specific data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DeviceDetails {
    None,
    Display(DisplayInfo),
    Input(InputInfo),
    Audio(AudioInfo),
    Usb(UsbInfo),
    Storage(StorageInfo),
}

/// A monitor on a display target.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DisplayInfo {
    pub adapter_name: String,
    /// Adapter LUID at enumeration time (runtime only).
    pub adapter_luid: u64,
    /// CCD target id.
    pub target_id: u32,
    /// `internal`, `hdmi`, `displayport`, `dvi`, `vga`, `miracast`, `indirect`, `other`.
    pub output_technology: String,
    pub monitor_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edid: Option<EdidId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<DisplayMode>,
    /// GDI name (`\\.\DISPLAY3`), runtime only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gdi_name: Option<String>,
    /// Desktop rectangle while the target is part of the console desktop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desktop_rect: Option<Rect>,
    /// Whether the monitor currently is part of the console desktop.
    pub in_console_desktop: bool,
    pub presentation: Presentation,
}

/// Monitor identity from the CCD target name query.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EdidId {
    pub manufacturer: String,
    pub product_code: u16,
    pub serial: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DisplayMode {
    pub width: u32,
    pub height: u32,
    /// Refresh rate in millihertz (59940 = 59.94 Hz).
    pub refresh_mhz: u32,
    /// Degrees: 0, 90, 180, 270.
    pub rotation: u16,
}

/// Desktop-coordinate rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// How a monitor shows its seat (spec 40 §6.10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Presentation {
    /// Not in use by any seat.
    #[default]
    None,
    /// Part of the console desktop (seat 1, or the host session itself).
    Native,
    /// Removed from the desktop and scanned out by our presenter (specialized display).
    Presenter,
    /// Fullscreen top-most presenter window on a monitor that stays in the console desktop
    /// (spec 40 §6.3.7: degraded, no privacy from the console).
    Fallback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputClass {
    Keyboard,
    Mouse,
    Hid,
}

/// An input unit (keyboard devnode, mouse devnode, HID collection).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputInfo {
    pub class: InputClass,
    /// HID usage page / usage of the collection (0 when not HID).
    pub usage_page: u16,
    pub usage: u16,
    pub vendor_id: u16,
    pub product_id: u16,
    /// Device interface path (runtime only; raw input maps handles to it).
    pub interface_path: String,
    /// `usb`, `i2c`, `spi`, `bluetooth`, `ps2`, `virtual`, `other`.
    pub bus: String,
    /// Built-in (laptop keyboard, touchpad): seat 1 by default (spec 10 ARM64 notes).
    pub built_in: bool,
    /// Last activity, service clock ms (0 = never). Drives the activity flash.
    pub last_activity_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioFlow {
    Render,
    Capture,
}

/// An audio endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioInfo {
    pub flow: AudioFlow,
    /// MMDevice endpoint id (runtime: may change when the device moves ports).
    pub endpoint_id: String,
    /// `speakers`, `headphones`, `headset`, `microphone`, `line`, `hdmi`, `spdif`, `other`.
    pub form_factor: String,
    /// Active / unplugged / disabled at enumeration.
    pub state: String,
}

/// A USB device or hub.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct UsbInfo {
    pub vendor_id: u16,
    pub product_id: u16,
    /// Number of downstream ports (hubs only).
    pub port_count: u8,
    /// 0 = root hub.
    pub hub_depth: u8,
    /// Port number on the parent hub.
    pub port: u8,
}

/// A disk / volume / optical drive.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct StorageInfo {
    pub removable: bool,
    pub optical: bool,
    /// `\\?\Volume{...}\` names of the volumes on it.
    pub volumes: Vec<String>,
    /// Drive letters currently visible (global or in the owner's session).
    pub letters: Vec<char>,
    pub file_system: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policies() {
        assert_eq!(DeviceKind::Keyboard.policy(), SharePolicy::Exclusive);
        assert_eq!(DeviceKind::AudioRender.policy(), SharePolicy::Shareable);
        assert_eq!(DeviceKind::Network.policy(), SharePolicy::NeverGated);
        assert_eq!(DeviceKind::Mouse.default_mask(), SeatMask::CONSOLE);
        assert_eq!(DeviceKind::Webcam.default_mask(), SeatMask::SHARED);
    }

    #[test]
    fn details_json_shape() {
        let d = DeviceDetails::Usb(UsbInfo { port_count: 4, ..Default::default() });
        let j = serde_json::to_string(&d).unwrap();
        assert!(j.starts_with(r#"{"type":"usb""#), "{j}");
    }
}
