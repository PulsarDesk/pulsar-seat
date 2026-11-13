// SPDX-License-Identifier: GPL-3.0-only
//! Game controllers per seat: what each path can and cannot do (spec 50 §4.1.4
//! "Gamepads", spec 00 T3, open question 8).
//!
//! | Pad family | How Windows reaches it | Per-seat today |
//! | --- | --- | --- |
//! | HID pads (DualSense, DualShock 4, generic HID joysticks, usage 0x01/0x04, 0x01/0x05) | `hidclass` collection opened by the game | only with our HID visibility filter (`kmdf_filter`, unsigned → not loaded). Without it every session sees every pad |
//! | XInput pads (Xbox 360 `xusb`, Xbox One/Series wired `xinputhid` / GIP) | `XInput*` / GameInput, opened by shared system processes in session 0 | **shared**: no per-session device visibility exists (HYPOTHESIS: RDP sessions may not see them at all) |
//! | Bluetooth Xbox pads | `xinputhid` over BTHLE HID | as XInput: shared |
//!
//! ViGEmBus (signed, open source, BSD-3) creates *virtual* Xbox 360 / DualShock 4 pads.
//! Its pads are machine-wide device interfaces too, so it cannot give a pad to one session
//! only; it does not help isolation and is therefore not used by the input router. It
//! stays the tool of choice if a later version forwards a physical pad's reports from the
//! seat host into a virtual pad *inside* a remote session that has its own bus (Pulsar
//! Split's model), which is outside Phase 4. Nothing here loads or installs ViGEmBus.
//!
//! The manager shows pads as `gamepads_shared` (raw input host) or `xinput_pads_shared`
//! (filter) so nobody expects isolation that does not exist.

use crate::units::{input_info, vid_pid};
use seat_core::device::{Device, DeviceKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PadFamily {
    /// Plain HID game pad / joystick collection.
    Hid,
    /// Xbox family (XInput / GIP / xinputhid).
    XInput,
}

/// Microsoft's USB vendor id (Xbox controllers).
const VID_MICROSOFT: u16 = 0x045E;

/// Pad family of an input unit, if it is a pad.
pub fn pad_family(d: &Device) -> Option<PadFamily> {
    if d.kind != DeviceKind::Hid {
        return None;
    }
    let i = input_info(d)?;
    if i.usage_page != 0x01 || !matches!(i.usage, 0x04 | 0x05 | 0x08) {
        return None;
    }
    let ids: Vec<String> = d.hardware_id.iter().cloned().collect();
    let xinput_id = ids.iter().any(|h| h.to_ascii_uppercase().contains("IG_"));
    let (vid, _) = if i.vendor_id != 0 { (i.vendor_id, i.product_id) } else { vid_pid(&ids) };
    if xinput_id || vid == VID_MICROSOFT { Some(PadFamily::XInput) } else { Some(PadFamily::Hid) }
}

/// Whether a backend with or without HID visibility can give the pad to one seat.
pub fn isolatable(family: PadFamily, hid_visibility: bool) -> bool {
    family == PadFamily::Hid && hid_visibility
}

#[cfg(test)]
mod tests {
    use super::*;
    use seat_core::device::{DeviceDetails, InputClass, InputInfo};
    use seat_core::ids::DeviceKey;

    fn pad(hw: &str, vid: u16) -> Device {
        Device {
            key: DeviceKey::new("hid;inst=X").unwrap(),
            kind: DeviceKind::Hid,
            name: "pad".into(),
            label: None,
            instance_id: None,
            container_id: None,
            parent_hub: None,
            present: true,
            hardware_id: Some(hw.into()),
            serial: None,
            warnings: vec![],
            details: DeviceDetails::Input(InputInfo {
                class: InputClass::Hid,
                usage_page: 1,
                usage: 5,
                vendor_id: vid,
                product_id: 0,
                interface_path: String::new(),
                bus: "usb".into(),
                built_in: false,
                last_activity_ms: 0,
            }),
        }
    }

    #[test]
    fn families() {
        assert_eq!(pad_family(&pad("HID\\VID_054C&PID_0CE6&MI_03", 0x054C)), Some(PadFamily::Hid));
        assert_eq!(pad_family(&pad("HID\\VID_045E&PID_0B12&IG_00", 0x045E)), Some(PadFamily::XInput));
        assert!(isolatable(PadFamily::Hid, true));
        assert!(!isolatable(PadFamily::Hid, false));
        assert!(!isolatable(PadFamily::XInput, true));
    }
}
