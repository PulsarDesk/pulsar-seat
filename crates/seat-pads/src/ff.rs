// SPDX-License-Identifier: GPL-3.0-only
//! Rumble for physical pads without a HID output report of their own (parity review
//! 2026-11-17): Xbox pads on USB (XUSB interface) and generic HID pads with the HID
//! Physical Interface Device page (force feedback wheels, joysticks).
//!
//! - **XUSB:** the pad's XUSB device interface (on the HID collection's parent node)
//!   takes a 5-byte "set state" request: `[0, 0, left motor, right motor, 2]` (public
//!   interoperability fact from open-source XInput re-implementations; HYPOTHESIS until
//!   run on hardware).
//! - **HID PID** (USB HID "Device Class Definition for Physical Interface Devices",
//!   usage page 0x0F): one periodic (sine) effect in block 1, started with the rumble
//!   magnitude and stopped at zero. The reports are built through the collection's own
//!   descriptor (Windows `HidP_*`), so only usages the device declares are written.
//!   HYPOTHESIS: devices that require "Create New Effect" (a feature report round trip)
//!   before block 1 exists ignore it.

/// XUSB device interface class (`GUID_DEVINTERFACE_XUSB`), `{EC87F1E3-C13B-4100-B5F7-8B84D54260CB}`.
pub const XUSB_INTERFACE_GUID: u128 = 0xEC87F1E3_C13B_4100_B5F7_8B84D54260CB;
/// `IOCTL_XUSB_SET_STATE`.
pub const IOCTL_XUSB_SET_STATE: u32 = 0x8000_A010;

/// The XUSB set-state request for two motor speeds (0..=65535 each).
pub fn xusb_set_state(strong: u16, weak: u16) -> [u8; 5] {
    [0, 0, (strong >> 8) as u8, (weak >> 8) as u8, 2]
}

/// HID PID usages (page 0x0F) used for rumble.
pub mod pid {
    pub const PAGE: u16 = 0x0F;
    pub const SET_EFFECT_REPORT: u16 = 0x21;
    pub const EFFECT_BLOCK_INDEX: u16 = 0x22;
    pub const ET_SINE: u16 = 0x30;
    pub const DURATION: u16 = 0x50;
    pub const GAIN: u16 = 0x52;
    pub const SET_PERIODIC_REPORT: u16 = 0x6E;
    pub const MAGNITUDE: u16 = 0x70;
    pub const PERIOD: u16 = 0x72;
    pub const EFFECT_OPERATION_REPORT: u16 = 0x77;
    pub const OP_EFFECT_START: u16 = 0x79;
    pub const OP_EFFECT_STOP: u16 = 0x7B;
    pub const LOOP_COUNT: u16 = 0x7C;
    pub const DC_ENABLE_ACTUATORS: u16 = 0x97;
    /// Duration "infinite" (the PID spec's null value for a 16-bit duration).
    pub const INFINITE: u32 = 0xFFFF;
    /// Period of the rumble sine (milliseconds, the PID default unit).
    pub const RUMBLE_PERIOD_MS: u32 = 20;
}

/// One rumble command for a PID device: the magnitude (0 = stop) scaled to the field's
/// logical maximum. The stronger motor wins (a PID device has one actuator set).
pub fn pid_magnitude(strong: u16, weak: u16, logical_max: i32) -> u32 {
    let v = u32::from(strong.max(weak));
    let max = logical_max.max(1) as u32;
    v * max / 65535
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xusb_and_pid_values() {
        assert_eq!(xusb_set_state(0xFFFF, 0x8000), [0, 0, 0xFF, 0x80, 2]);
        assert_eq!(xusb_set_state(0, 0), [0, 0, 0, 0, 2]);
        assert_eq!(pid_magnitude(0xFFFF, 0, 10000), 10000);
        assert_eq!(pid_magnitude(0, 0x8000, 255), 127);
        assert_eq!(pid_magnitude(0, 0, 255), 0);
        assert_eq!(pid_magnitude(0x1000, 0, 0), 0);
    }
}
