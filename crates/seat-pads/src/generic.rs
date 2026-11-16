// SPDX-License-Identifier: GPL-3.0-only
//! Physical HID game pads that are not PlayStation or Switch Pro pads: generic HID
//! joysticks / gamepads (usage page 0x01, usage 0x04 / 0x05) and Xbox pads as their HID
//! collection shows them. Windows decodes the report (`HidP_*` over the collection's
//! preparsed data, `seat_input::win`); this module only maps the decoded usages to a
//! [`PadState`], so it is pure and testable.
//!
//! Profiles (picked by [`profile_for`] from the collection's usages):
//!
//! | Profile | Seen on | Sticks | Triggers | Buttons |
//! | --- | --- | --- | --- | --- |
//! | [`Profile::XInputCompat`] | Xbox pads through `xusb` / `xinputhid` / GIP (path token `IG_`) | X/Y left, Rx/Ry right | one combined Z axis (Microsoft "XInput and DirectInput" documentation: both triggers share Z, centre = released) | 1 A, 2 B, 3 X, 4 Y, 5 LB, 6 RB, 7 View, 8 Menu, 9 LS, 10 RS; no guide |
//! | [`Profile::XboxBluetooth`] | Xbox pads on Bluetooth without the compat view | X/Y left, Z/Rz right | Brake / Accelerator (simulation page 0x02) | 15-button layout with gaps (1 A, 2 B, 4 X, 5 Y, 7 LB, 8 RB, 11 View, 12 Menu, 13 Guide, 14 LS, 15 RS), else the compat order; consumer AC Home 0x223 = guide, Record 0xB2 = share |
//! | [`Profile::Standard`] | everything else | X/Y left; Z/Rz right (DirectInput convention) or Rx/Ry when Z/Rz are absent | Brake / Accelerator, else Rx/Ry when Z/Rz are the right stick, else none | 1 south, 2 east, 3 west, 4 north, 5 L1, 6 R1, 7 L2 (digital), 8 R2 (digital), 9 back, 10 start, 11 L3, 12 R3, 13 guide |
//!
//! Hat switch (0x01/0x39) becomes the d-pad. Y axes are inverted (HID: +y = down; pad
//! state: +y = up). HYPOTHESIS (untested on hardware): the Xbox Bluetooth button layout per
//! firmware, the direction of the combined trigger axis (left trigger = above centre) and
//! every generic pad's button order (there is no standard order; the table is the common
//! one).

use crate::types::{buttons as b, PadState};

/// HID usage page Generic Desktop.
pub const PAGE_DESKTOP: u16 = 0x01;
/// HID usage page Simulation Controls (Brake / Accelerator).
pub const PAGE_SIMULATION: u16 = 0x02;
/// HID usage page Button.
pub const PAGE_BUTTON: u16 = 0x09;
/// HID usage page Consumer.
pub const PAGE_CONSUMER: u16 = 0x0C;

pub const USAGE_X: u16 = 0x30;
pub const USAGE_Y: u16 = 0x31;
pub const USAGE_Z: u16 = 0x32;
pub const USAGE_RX: u16 = 0x33;
pub const USAGE_RY: u16 = 0x34;
pub const USAGE_RZ: u16 = 0x35;
pub const USAGE_HAT: u16 = 0x39;
pub const USAGE_ACCELERATOR: u16 = 0xC4;
pub const USAGE_BRAKE: u16 = 0xC5;
pub const USAGE_AC_HOME: u16 = 0x0223;
pub const USAGE_RECORD: u16 = 0x00B2;

/// One decoded value usage with its logical range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HidValue {
    pub page: u16,
    pub usage: u16,
    pub value: i32,
    pub min: i32,
    pub max: i32,
}

/// One decoded input report of a pad collection.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HidSample {
    /// Button page (0x09) usages that are pressed (1-based button numbers).
    pub buttons: Vec<u16>,
    /// Consumer page usages that are pressed.
    pub consumer: Vec<u16>,
    pub values: Vec<HidValue>,
}

impl HidSample {
    pub fn value(&self, page: u16, usage: u16) -> Option<HidValue> {
        self.values.iter().copied().find(|v| v.page == page && v.usage == usage)
    }
    fn has(&self, page: u16, usage: u16) -> bool {
        self.value(page, usage).is_some()
    }
}

/// What a collection declares (from its value caps; Windows `HidP_GetValueCaps`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HidLayout {
    /// `(page, usage)` of every input value.
    pub values: Vec<(u16, u16)>,
    /// Highest button number (button page usage maximum).
    pub button_count: u16,
}

impl HidLayout {
    fn has(&self, page: u16, usage: u16) -> bool {
        self.values.contains(&(page, usage))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    Standard,
    XInputCompat,
    XboxBluetooth,
}

/// Profile of a pad collection. `xinput` = the pad is of the Xbox family (path token
/// `IG_` or Microsoft vendor id; `seat_input::gamepad::PadFamily::XInput`).
pub fn profile_for(xinput: bool, layout: &HidLayout) -> Profile {
    if !xinput {
        return Profile::Standard;
    }
    let sim = layout.has(PAGE_SIMULATION, USAGE_BRAKE) || layout.has(PAGE_SIMULATION, USAGE_ACCELERATOR);
    let rz_right = layout.has(PAGE_DESKTOP, USAGE_Z) && layout.has(PAGE_DESKTOP, USAGE_RZ) && !layout.has(PAGE_DESKTOP, USAGE_RX);
    if sim || rz_right { Profile::XboxBluetooth } else { Profile::XInputCompat }
}

/// Value -> signed 16-bit stick (centre of the logical range = 0).
fn stick(v: HidValue) -> i16 {
    let span = i64::from(v.max) - i64::from(v.min);
    if span <= 0 {
        return 0;
    }
    let x = i64::from(v.value).clamp(i64::from(v.min), i64::from(v.max));
    ((2 * x - i64::from(v.min) - i64::from(v.max)) * 32768 / span).clamp(i64::from(i16::MIN), i64::from(i16::MAX)) as i16
}

fn stick_inv(v: HidValue) -> i16 {
    stick(v).saturating_neg()
}

/// Value -> trigger 0..=65535.
fn trigger(v: HidValue) -> u16 {
    let span = i64::from(v.max) - i64::from(v.min);
    if span <= 0 {
        return 0;
    }
    ((i64::from(v.value).clamp(i64::from(v.min), i64::from(v.max)) - i64::from(v.min)) * 65535 / span) as u16
}

/// Hat switch -> d-pad bits. Eight positions (N, NE, E, ...) or four (N, E, S, W); a value
/// outside the logical range is the null state.
pub fn hat_buttons(v: HidValue) -> u32 {
    if v.value < v.min || v.value > v.max {
        return 0;
    }
    let positions = i64::from(v.max) - i64::from(v.min) + 1;
    let pos = i64::from(v.value) - i64::from(v.min);
    let eighth = match positions {
        8 => pos,
        4 => pos * 2,
        _ => return 0,
    };
    match eighth {
        0 => b::DPAD_UP,
        1 => b::DPAD_UP | b::DPAD_RIGHT,
        2 => b::DPAD_RIGHT,
        3 => b::DPAD_DOWN | b::DPAD_RIGHT,
        4 => b::DPAD_DOWN,
        5 => b::DPAD_DOWN | b::DPAD_LEFT,
        6 => b::DPAD_LEFT,
        7 => b::DPAD_UP | b::DPAD_LEFT,
        _ => 0,
    }
}

const COMPAT_BUTTONS: [(u16, u32); 10] =
    [(1, b::SOUTH), (2, b::EAST), (3, b::WEST), (4, b::NORTH), (5, b::L1), (6, b::R1), (7, b::BACK), (8, b::START), (9, b::L3), (10, b::R3)];
const XBOX_BT_GAPPED: [(u16, u32); 11] = [
    (1, b::SOUTH),
    (2, b::EAST),
    (4, b::WEST),
    (5, b::NORTH),
    (7, b::L1),
    (8, b::R1),
    (11, b::BACK),
    (12, b::START),
    (13, b::GUIDE),
    (14, b::L3),
    (15, b::R3),
];
const STANDARD_BUTTONS: [(u16, u32); 11] = [
    (1, b::SOUTH),
    (2, b::EAST),
    (3, b::WEST),
    (4, b::NORTH),
    (5, b::L1),
    (6, b::R1),
    (9, b::BACK),
    (10, b::START),
    (11, b::L3),
    (12, b::R3),
    (13, b::GUIDE),
];

fn map_buttons(pressed: &[u16], table: &[(u16, u32)]) -> u32 {
    table.iter().filter(|(n, _)| pressed.contains(n)).fold(0, |acc, (_, bit)| acc | bit)
}

/// A decoded report -> pad state, for the collection's `profile` and `layout`.
pub fn map_sample(profile: Profile, layout: &HidLayout, s: &HidSample) -> PadState {
    let mut st = PadState::neutral();
    let d = |u| s.value(PAGE_DESKTOP, u);
    if let Some(x) = d(USAGE_X) {
        st.lx = stick(x);
    }
    if let Some(y) = d(USAGE_Y) {
        st.ly = stick_inv(y);
    }
    if let Some(h) = d(USAGE_HAT) {
        st.buttons |= hat_buttons(h);
    }
    let brake = s.value(PAGE_SIMULATION, USAGE_BRAKE);
    let accel = s.value(PAGE_SIMULATION, USAGE_ACCELERATOR);
    match profile {
        Profile::XInputCompat => {
            if let (Some(rx), Some(ry)) = (d(USAGE_RX), d(USAGE_RY)) {
                st.rx = stick(rx);
                st.ry = stick_inv(ry);
            }
            if let Some(z) = d(USAGE_Z) {
                // Combined axis: left trigger above the centre, right trigger below.
                let v = stick(z);
                if v > 0 {
                    st.lt = (i32::from(v) * 2).min(65535) as u16;
                } else {
                    st.rt = (-(i32::from(v)) * 2).min(65535) as u16;
                }
            }
            st.buttons |= map_buttons(&s.buttons, &COMPAT_BUTTONS);
        }
        Profile::XboxBluetooth => {
            if let (Some(z), Some(rz)) = (d(USAGE_Z), d(USAGE_RZ)) {
                st.rx = stick(z);
                st.ry = stick_inv(rz);
            } else if let (Some(rx), Some(ry)) = (d(USAGE_RX), d(USAGE_RY)) {
                st.rx = stick(rx);
                st.ry = stick_inv(ry);
            }
            st.lt = brake.map(trigger).unwrap_or(0);
            st.rt = accel.map(trigger).unwrap_or(0);
            let table: &[(u16, u32)] = if layout.button_count >= 15 { &XBOX_BT_GAPPED } else { &COMPAT_BUTTONS };
            st.buttons |= map_buttons(&s.buttons, table);
            if s.consumer.contains(&USAGE_AC_HOME) {
                st.buttons |= b::GUIDE;
            }
            if s.consumer.contains(&USAGE_RECORD) {
                st.buttons |= b::MISC;
            }
        }
        Profile::Standard => {
            let z_right = s.has(PAGE_DESKTOP, USAGE_Z) && s.has(PAGE_DESKTOP, USAGE_RZ);
            if z_right {
                st.rx = d(USAGE_Z).map(stick).unwrap_or(0);
                st.ry = d(USAGE_RZ).map(stick_inv).unwrap_or(0);
            } else if let (Some(rx), Some(ry)) = (d(USAGE_RX), d(USAGE_RY)) {
                st.rx = stick(rx);
                st.ry = stick_inv(ry);
            }
            match (brake, accel) {
                (None, None) if z_right => {
                    st.lt = d(USAGE_RX).map(trigger).unwrap_or(0);
                    st.rt = d(USAGE_RY).map(trigger).unwrap_or(0);
                }
                (l, r) => {
                    st.lt = l.map(trigger).unwrap_or(0);
                    st.rt = r.map(trigger).unwrap_or(0);
                }
            }
            st.buttons |= map_buttons(&s.buttons, &STANDARD_BUTTONS);
            // Digital triggers (buttons 7 / 8) when the pad has no analog ones.
            if st.lt == 0 && s.buttons.contains(&7) {
                st.lt = 65535;
            }
            if st.rt == 0 && s.buttons.contains(&8) {
                st.rt = 65535;
            }
            if s.consumer.contains(&USAGE_AC_HOME) {
                st.buttons |= b::GUIDE;
            }
        }
    }
    st
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(page: u16, usage: u16, value: i32, min: i32, max: i32) -> HidValue {
        HidValue { page, usage, value, min, max }
    }

    fn layout(values: &[(u16, u16)], buttons: u16) -> HidLayout {
        HidLayout { values: values.to_vec(), button_count: buttons }
    }

    #[test]
    fn profiles() {
        let compat = layout(&[(1, USAGE_X), (1, USAGE_Y), (1, USAGE_RX), (1, USAGE_RY), (1, USAGE_Z), (1, USAGE_HAT)], 10);
        assert_eq!(profile_for(true, &compat), Profile::XInputCompat);
        assert_eq!(profile_for(false, &compat), Profile::Standard);
        let bt = layout(&[(1, USAGE_X), (1, USAGE_Y), (1, USAGE_Z), (1, USAGE_RZ), (2, USAGE_BRAKE), (2, USAGE_ACCELERATOR)], 15);
        assert_eq!(profile_for(true, &bt), Profile::XboxBluetooth);
    }

    #[test]
    fn sticks_and_hat() {
        assert_eq!(stick(v(1, USAGE_X, 0, 0, 65535)), -32768);
        assert_eq!(stick(v(1, USAGE_X, 65535, 0, 65535)), 32767);
        assert_eq!(stick(v(1, USAGE_X, 0, -127, 127)), 0);
        assert_eq!(stick_inv(v(1, USAGE_Y, 0, 0, 255)), 32767);
        assert_eq!(hat_buttons(v(1, USAGE_HAT, 0, 0, 7)), b::DPAD_UP);
        assert_eq!(hat_buttons(v(1, USAGE_HAT, 3, 0, 7)), b::DPAD_DOWN | b::DPAD_RIGHT);
        assert_eq!(hat_buttons(v(1, USAGE_HAT, 8, 1, 8)), b::DPAD_UP | b::DPAD_LEFT);
        assert_eq!(hat_buttons(v(1, USAGE_HAT, 15, 0, 7)), 0, "null state");
        assert_eq!(hat_buttons(v(1, USAGE_HAT, 1, 0, 3)), b::DPAD_RIGHT);
    }

    #[test]
    fn xinput_compat_combined_triggers() {
        let l = layout(&[(1, USAGE_X), (1, USAGE_Y), (1, USAGE_RX), (1, USAGE_RY), (1, USAGE_Z)], 10);
        let mut s = HidSample {
            buttons: vec![1, 6, 8],
            values: vec![
                v(1, USAGE_X, 65535, 0, 65535),
                v(1, USAGE_Y, 0, 0, 65535),
                v(1, USAGE_RX, 32768, 0, 65535),
                v(1, USAGE_RY, 32768, 0, 65535),
                v(1, USAGE_Z, 65535, 0, 65535),
            ],
            ..Default::default()
        };
        let st = map_sample(Profile::XInputCompat, &l, &s);
        assert_eq!(st.buttons, b::SOUTH | b::R1 | b::START);
        assert_eq!((st.lx, st.ly), (32767, 32767));
        assert_eq!((st.lt, st.rt), (65534, 0));
        s.values[4].value = 0;
        let st = map_sample(Profile::XInputCompat, &l, &s);
        assert_eq!((st.lt, st.rt), (0, 65535));
    }

    #[test]
    fn xbox_bluetooth_gapped_buttons() {
        let l = layout(&[(1, USAGE_Z), (1, USAGE_RZ), (2, USAGE_BRAKE), (2, USAGE_ACCELERATOR)], 15);
        let s = HidSample {
            buttons: vec![4, 13, 15],
            consumer: vec![USAGE_RECORD],
            values: vec![v(2, USAGE_BRAKE, 1023, 0, 1023), v(2, USAGE_ACCELERATOR, 0, 0, 1023), v(1, USAGE_Z, 0, 0, 65535), v(1, USAGE_RZ, 0, 0, 65535)],
        };
        let st = map_sample(Profile::XboxBluetooth, &l, &s);
        assert_eq!(st.buttons, b::WEST | b::GUIDE | b::R3 | b::MISC);
        assert_eq!((st.lt, st.rt), (65535, 0));
        assert_eq!((st.rx, st.ry), (-32768, 32767));
        let short = layout(&[], 10);
        let s2 = HidSample { buttons: vec![3], ..Default::default() };
        assert_eq!(map_sample(Profile::XboxBluetooth, &short, &s2).buttons, b::WEST);
    }

    #[test]
    fn standard_directinput_layout() {
        let l = layout(&[], 13);
        let s = HidSample {
            buttons: vec![2, 9, 13],
            values: vec![
                v(1, USAGE_Z, 255, 0, 255),
                v(1, USAGE_RZ, 0, 0, 255),
                v(1, USAGE_RX, 255, 0, 255),
                v(1, USAGE_RY, 0, 0, 255),
                v(1, USAGE_HAT, 6, 0, 7),
            ],
            ..Default::default()
        };
        let st = map_sample(Profile::Standard, &l, &s);
        assert_eq!(st.buttons, b::EAST | b::BACK | b::GUIDE | b::DPAD_LEFT);
        assert_eq!((st.rx, st.ry), (32767, 32767));
        assert_eq!((st.lt, st.rt), (65535, 0));
        // Digital triggers.
        let d = HidSample { buttons: vec![7, 8], ..Default::default() };
        let st = map_sample(Profile::Standard, &l, &d);
        assert_eq!((st.lt, st.rt), (65535, 65535));
    }
}
