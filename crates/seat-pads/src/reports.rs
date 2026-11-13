// SPDX-License-Identifier: GPL-3.0-only
//! Pure conversions between [`PadState`] and the public USB HID report layouts
//! (spec 60 §5), plus output-report parsing into [`Feedback`] (spec 60 §5.3).
//!
//! Layouts follow the public open-source drivers (Linux `hid-playstation`,
//! `hid-microsoft`, SDL HIDAPI); the Xbox HID layout is the one declared by our own
//! descriptor (`descriptors::XONE_REPORT_DESCRIPTOR`).

use crate::layout;
use crate::types::{buttons as b, Battery, Feedback, PadKind, PadState, Touch};

/// Report counters that must advance per pad (DS4 counter, DualSense sequence,
/// touch packet counters).
#[derive(Debug, Clone, Default)]
pub struct Counters {
    report: u8,
    touch_packet: u8,
    last_touch: [Option<Touch>; 2],
}

fn stick8(v: i16) -> u8 {
    ((v as i32 + 32768) >> 8) as u8
}

fn stick8_inv(v: i16) -> u8 {
    255 - stick8(v)
}

fn trig8(v: u16) -> u8 {
    (v >> 8) as u8
}

/// Hat value 0 = N … 7 = NW, 8 = neutral.
fn hat(buttons: u32) -> u8 {
    let up = buttons & b::DPAD_UP != 0;
    let down = buttons & b::DPAD_DOWN != 0;
    let left = buttons & b::DPAD_LEFT != 0;
    let right = buttons & b::DPAD_RIGHT != 0;
    match (up && !down, down && !up, left && !right, right && !left) {
        (true, _, false, false) => 0,
        (true, _, false, true) => 1,
        (false, false, false, true) => 2,
        (_, true, false, true) => 3,
        (_, true, false, false) => 4,
        (_, true, true, false) => 5,
        (false, false, true, false) => 6,
        (true, _, true, false) => 7,
        _ => 8,
    }
}

fn hat_to_buttons(h: u8) -> u32 {
    match h {
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

fn bit(state: &PadState, mask: u32, out: u8) -> u8 {
    if state.buttons & mask != 0 { out } else { 0 }
}

fn put_i16(buf: &mut [u8], at: usize, v: i16) {
    buf[at..at + 2].copy_from_slice(&v.to_le_bytes());
}

fn put_u16(buf: &mut [u8], at: usize, v: u16) {
    buf[at..at + 2].copy_from_slice(&v.to_le_bytes());
}

/// Touch point bytes (4): contact (bit 7 = no touch, low 7 bits = id), 12-bit x, 12-bit y.
fn touch_bytes(t: Option<Touch>, w: u32, h: u32) -> [u8; 4] {
    match t {
        None => [0x80, 0, 0, 0],
        Some(t) => {
            let x = ((t.x as u32) * (w - 1) / 65535) as u16;
            let y = ((t.y as u32) * (h - 1) / 65535) as u16;
            [t.id & 0x7F, (x & 0xFF) as u8, (((x >> 8) & 0x0F) as u8) | (((y & 0x0F) as u8) << 4), (y >> 4) as u8]
        }
    }
}

/// DS4 touchpad resolution.
pub const DS4_TOUCH_W: u32 = 1920;
pub const DS4_TOUCH_H: u32 = 942;
/// DualSense touchpad resolution.
pub const DS5_TOUCH_W: u32 = 1920;
pub const DS5_TOUCH_H: u32 = 1080;

impl Counters {
    fn touch_changed(&mut self, s: &PadState) -> bool {
        let changed = self.last_touch != s.touch;
        self.last_touch = s.touch;
        if changed {
            self.touch_packet = self.touch_packet.wrapping_add(1);
        }
        changed
    }
}

/// DualShock 4 USB input report 0x01 (64 bytes).
pub fn ds4_input(s: &PadState, c: &mut Counters) -> [u8; layout::DS_INPUT_LEN] {
    let mut r = [0u8; layout::DS_INPUT_LEN];
    r[0] = 0x01;
    r[1] = stick8(s.lx);
    r[2] = stick8_inv(s.ly);
    r[3] = stick8(s.rx);
    r[4] = stick8_inv(s.ry);
    r[5] = hat(s.buttons)
        | bit(s, b::WEST, 0x10)
        | bit(s, b::SOUTH, 0x20)
        | bit(s, b::EAST, 0x40)
        | bit(s, b::NORTH, 0x80);
    let l2 = trig8(s.lt);
    let r2 = trig8(s.rt);
    r[6] = bit(s, b::L1, 0x01)
        | bit(s, b::R1, 0x02)
        | if l2 > 0 { 0x04 } else { 0 }
        | if r2 > 0 { 0x08 } else { 0 }
        | bit(s, b::BACK, 0x10)
        | bit(s, b::START, 0x20)
        | bit(s, b::L3, 0x40)
        | bit(s, b::R3, 0x80);
    r[7] = bit(s, b::GUIDE, 0x01) | bit(s, b::TOUCHPAD | b::MISC, 0x02) | ((c.report & 0x3F) << 2);
    c.report = c.report.wrapping_add(1) & 0x3F;
    r[8] = l2;
    r[9] = r2;
    // 5.33 µs per tick = 16/3 µs
    put_u16(&mut r, 10, ((s.motion_ts_us.wrapping_mul(3) / 16) & 0xFFFF) as u16);
    r[12] = 0; // temperature
    for i in 0..3 {
        put_i16(&mut r, 13 + 2 * i, s.gyro[i]);
        put_i16(&mut r, 19 + 2 * i, s.accel[i]);
    }
    // byte 30: battery level 0..10 (low nibble) + cable bit 4
    let bat = s.battery.unwrap_or(Battery { percent: 100, charging: false, full: true });
    let cable = 0x10;
    let level = if bat.full { 11 } else { (bat.percent.min(100) / 10).min(10) };
    r[30] = level | cable;
    c.touch_changed(s);
    r[33] = 1; // one touch report
    r[34] = c.touch_packet;
    r[35..39].copy_from_slice(&touch_bytes(s.touch[0], DS4_TOUCH_W, DS4_TOUCH_H));
    r[39..43].copy_from_slice(&touch_bytes(s.touch[1], DS4_TOUCH_W, DS4_TOUCH_H));
    r
}

/// DualSense / Edge USB input report 0x01 (64 bytes).
pub fn ds5_input(s: &PadState, edge: bool, c: &mut Counters) -> [u8; layout::DS_INPUT_LEN] {
    let mut r = [0u8; layout::DS_INPUT_LEN];
    r[0] = 0x01;
    r[1] = stick8(s.lx);
    r[2] = stick8_inv(s.ly);
    r[3] = stick8(s.rx);
    r[4] = stick8_inv(s.ry);
    let l2 = trig8(s.lt);
    let r2 = trig8(s.rt);
    r[5] = l2;
    r[6] = r2;
    r[7] = c.report;
    c.report = c.report.wrapping_add(1);
    r[8] = hat(s.buttons)
        | bit(s, b::WEST, 0x10)
        | bit(s, b::SOUTH, 0x20)
        | bit(s, b::EAST, 0x40)
        | bit(s, b::NORTH, 0x80);
    r[9] = bit(s, b::L1, 0x01)
        | bit(s, b::R1, 0x02)
        | if l2 > 0 { 0x04 } else { 0 }
        | if r2 > 0 { 0x08 } else { 0 }
        | bit(s, b::BACK, 0x10)
        | bit(s, b::START, 0x20)
        | bit(s, b::L3, 0x40)
        | bit(s, b::R3, 0x80);
    r[10] = bit(s, b::GUIDE, 0x01) | bit(s, b::TOUCHPAD, 0x02) | bit(s, b::MISC, 0x04);
    if edge {
        r[10] |= bit(s, b::FN_LEFT, 0x10)
            | bit(s, b::FN_RIGHT, 0x20)
            | bit(s, b::PADDLE1 | b::PADDLE3, 0x40)
            | bit(s, b::PADDLE2 | b::PADDLE4, 0x80);
    }
    for i in 0..3 {
        put_i16(&mut r, 16 + 2 * i, s.gyro[i]);
        put_i16(&mut r, 22 + 2 * i, s.accel[i]);
    }
    // sensor timestamp, ~0.33 µs per tick
    let ts = (s.motion_ts_us.wrapping_mul(3) & 0xFFFF_FFFF) as u32;
    r[28..32].copy_from_slice(&ts.to_le_bytes());
    c.touch_changed(s);
    r[33..37].copy_from_slice(&touch_bytes(s.touch[0], DS5_TOUCH_W, DS5_TOUCH_H));
    r[37..41].copy_from_slice(&touch_bytes(s.touch[1], DS5_TOUCH_W, DS5_TOUCH_H));
    // byte 53: battery (low nibble 0..10) + charging state (high nibble: 0 discharging,
    // 1 charging, 2 full). Default when the client sent none: full, on cable.
    let bat = s.battery.unwrap_or(Battery { percent: 100, charging: false, full: true });
    let state = if bat.full {
        2
    } else if bat.charging {
        1
    } else {
        0
    };
    r[53] = (bat.percent.min(100) / 10).min(10) | (state << 4);
    // byte 54: plug state (bit 3 = USB data, bit 4 = USB power), matches a cabled pad
    r[54] = 0x08 | 0x10;
    r
}

/// Xbox HID input report 0x01 (62 bytes) of our descriptor.
pub fn xone_input(s: &PadState) -> [u8; layout::XONE_INPUT_LEN] {
    let mut r = [0u8; layout::XONE_INPUT_LEN];
    r[0] = 0x01;
    let u = |v: i16| (v as i32 + 32768) as u16;
    let ui = |v: i16| 65535 - (v as i32 + 32768) as u16;
    put_u16(&mut r, 1, u(s.lx));
    put_u16(&mut r, 3, ui(s.ly));
    put_u16(&mut r, 5, u(s.rx));
    put_u16(&mut r, 7, ui(s.ry));
    put_u16(&mut r, 9, s.lt >> 6);
    put_u16(&mut r, 11, s.rt >> 6);
    let h = hat(s.buttons);
    r[13] = if h == 8 { 0 } else { h + 1 };
    let mut btn: u16 = 0;
    let order = [
        b::SOUTH,
        b::EAST,
        b::WEST,
        b::NORTH,
        b::L1,
        b::R1,
        b::BACK,
        b::START,
        b::L3,
        b::R3,
        b::GUIDE,
        b::MISC,
        b::PADDLE1,
        b::PADDLE2,
        b::PADDLE3,
        b::PADDLE4,
    ];
    for (i, m) in order.iter().enumerate() {
        if s.buttons & m != 0 {
            btn |= 1 << i;
        }
    }
    put_u16(&mut r, 14, btn);
    r
}

/// Parses our Xbox input report back into a state (used to feed ViGEm X360 targets from
/// `xone` reports written by the stream server).
pub fn parse_xone_input(r: &[u8]) -> Option<PadState> {
    if r.len() < 16 || r[0] != 0x01 {
        return None;
    }
    let g = |at: usize| u16::from_le_bytes([r[at], r[at + 1]]);
    let s16 = |v: u16| (v as i32 - 32768) as i16;
    let s16i = |v: u16| (32767 - v as i32) as i16;
    let order = [
        b::SOUTH,
        b::EAST,
        b::WEST,
        b::NORTH,
        b::L1,
        b::R1,
        b::BACK,
        b::START,
        b::L3,
        b::R3,
        b::GUIDE,
        b::MISC,
        b::PADDLE1,
        b::PADDLE2,
        b::PADDLE3,
        b::PADDLE4,
    ];
    let raw = g(14);
    let mut buttons = 0u32;
    for (i, m) in order.iter().enumerate() {
        if raw & (1 << i) != 0 {
            buttons |= m;
        }
    }
    let h = r[13] & 0x0F;
    buttons |= if h == 0 { 0 } else { hat_to_buttons(h - 1) };
    Some(PadState {
        v: crate::types::PAD_STATE_VERSION,
        buttons,
        lx: s16(g(1)),
        ly: s16i(g(3)),
        rx: s16(g(5)),
        ry: s16i(g(7)),
        lt: (g(9).min(1023) << 6) | (g(9).min(1023) >> 4),
        rt: (g(11).min(1023) << 6) | (g(11).min(1023) >> 4),
        ..Default::default()
    })
}

/// ViGEm XUSB report (Xbox 360): buttons, 8-bit triggers, signed sticks (+y up).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct XusbReport {
    pub buttons: u16,
    pub left_trigger: u8,
    pub right_trigger: u8,
    pub lx: i16,
    pub ly: i16,
    pub rx: i16,
    pub ry: i16,
}

pub fn x360_report(s: &PadState) -> XusbReport {
    let mut w: u16 = 0;
    let map: [(u32, u16); 15] = [
        (b::DPAD_UP, 0x0001),
        (b::DPAD_DOWN, 0x0002),
        (b::DPAD_LEFT, 0x0004),
        (b::DPAD_RIGHT, 0x0008),
        (b::START, 0x0010),
        (b::BACK, 0x0020),
        (b::L3, 0x0040),
        (b::R3, 0x0080),
        (b::L1, 0x0100),
        (b::R1, 0x0200),
        (b::GUIDE, 0x0400),
        (b::SOUTH, 0x1000),
        (b::EAST, 0x2000),
        (b::WEST, 0x4000),
        (b::NORTH, 0x8000),
    ];
    for (m, x) in map {
        if s.buttons & m != 0 {
            w |= x;
        }
    }
    XusbReport {
        buttons: w,
        left_trigger: trig8(s.lt),
        right_trigger: trig8(s.rt),
        lx: s.lx,
        ly: s.ly,
        rx: s.rx,
        ry: s.ry,
    }
}

/// Builds the input report for a wire kind (X360 has no HID report: use [`x360_report`]).
pub fn input_report(kind: PadKind, s: &PadState, c: &mut Counters) -> Vec<u8> {
    match kind {
        PadKind::Ds4 => ds4_input(s, c).to_vec(),
        PadKind::Ds5 => ds5_input(s, false, c).to_vec(),
        PadKind::Ds5Edge => ds5_input(s, true, c).to_vec(),
        PadKind::XOne | PadKind::X360 => xone_input(s).to_vec(),
    }
}

fn scale8(v: u8) -> u16 {
    (v as u16) * 257
}

fn scale_pct(v: u8) -> u16 {
    ((v.min(100) as u32) * 65535 / 100) as u16
}

/// Parses a game's output report (byte 0 = report id) for a wire kind into feedback.
/// Unknown ids or short reports give no feedback. DS4 motors are reported in the right
/// order (strong = left/large).
pub fn parse_output(kind: PadKind, r: &[u8]) -> Vec<Feedback> {
    let mut out = Vec::new();
    match kind {
        PadKind::Ds4 => {
            if r.len() < 9 || r[0] != 0x05 {
                return out;
            }
            let flags = r[1];
            if flags & 0x01 != 0 {
                out.push(Feedback::Rumble { strong: scale8(r[5]), weak: scale8(r[4]) });
            }
            if flags & 0x02 != 0 {
                out.push(Feedback::Led { r: r[6], g: r[7], b: r[8] });
            }
        }
        PadKind::Ds5 | PadKind::Ds5Edge => {
            if r.len() < layout::DS5_OUTPUT_LEN || r[0] != 0x02 {
                return out;
            }
            let p = &r[1..];
            let (vf0, vf1) = (p[0], p[1]);
            if vf0 & 0x03 != 0 {
                out.push(Feedback::Rumble { strong: scale8(p[3]), weak: scale8(p[2]) });
            }
            if vf0 & 0x0C != 0 {
                let mut raw = vec![0u8; 23];
                raw[0] = (if vf0 & 0x04 != 0 { 1 } else { 0 }) | (if vf0 & 0x08 != 0 { 2 } else { 0 });
                raw[1..12].copy_from_slice(&p[10..21]);
                raw[12..23].copy_from_slice(&p[21..32]);
                out.push(Feedback::AdaptiveTrigger { raw });
            }
            if vf1 & 0x04 != 0 {
                out.push(Feedback::Led { r: p[44], g: p[45], b: p[46] });
            }
            if vf1 & 0x10 != 0 {
                out.push(Feedback::PlayerLeds { mask: p[43] & 0x1F });
            }
        }
        PadKind::XOne | PadKind::X360 => {
            if r.len() < 6 || r[0] != 0x03 {
                return out;
            }
            let enable = r[1];
            if enable & 0x0C != 0 {
                out.push(Feedback::TriggerRumble { left: scale_pct(r[2]), right: scale_pct(r[3]) });
            }
            if enable & 0x03 != 0 {
                out.push(Feedback::Rumble { strong: scale_pct(r[4]), weak: scale_pct(r[5]) });
            }
        }
    }
    out
}

/// Synthesises an Xbox output report 0x03 for rumble that arrived from a non-HID
/// backend (ViGEm X360), so the output ring always carries the wire kind's layout.
pub fn xone_output_from_rumble(large: u8, small: u8) -> [u8; layout::XONE_OUTPUT_LEN] {
    let pct = |v: u8| ((v as u32) * 100 / 255) as u8;
    [0x03, 0x03, 0, 0, pct(large), pct(small), 0xFF, 0, 0]
}

/// Synthesises a DS4 output report 0x05 (rumble + lightbar).
pub fn ds4_output(small: u8, large: u8, rgb: (u8, u8, u8)) -> [u8; layout::DS4_OUTPUT_LEN] {
    let mut r = [0u8; layout::DS4_OUTPUT_LEN];
    r[0] = 0x05;
    r[1] = 0x03;
    r[4] = small;
    r[5] = large;
    r[6] = rgb.0;
    r[7] = rgb.1;
    r[8] = rgb.2;
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st() -> PadState {
        PadState {
            buttons: b::SOUTH | b::DPAD_UP | b::DPAD_RIGHT | b::GUIDE | b::L1,
            lx: i16::MIN,
            ly: i16::MAX,
            rx: 0,
            ry: 0,
            lt: 0xFFFF,
            rt: 0,
            gyro: [1, -2, 3],
            accel: [100, 200, -8192],
            motion_ts_us: 1_000_000,
            touch: [Some(Touch { id: 5, x: 65535, y: 0 }), None],
            ..PadState::neutral()
        }
    }

    #[test]
    fn hat_values() {
        assert_eq!(hat(0), 8);
        assert_eq!(hat(b::DPAD_UP), 0);
        assert_eq!(hat(b::DPAD_UP | b::DPAD_RIGHT), 1);
        assert_eq!(hat(b::DPAD_DOWN | b::DPAD_LEFT), 5);
        assert_eq!(hat(b::DPAD_LEFT), 6);
        assert_eq!(hat(b::DPAD_UP | b::DPAD_DOWN), 8);
        for h in 0..8 {
            assert_eq!(hat(hat_to_buttons(h)), h);
        }
    }

    #[test]
    fn ds5_layout() {
        let mut c = Counters::default();
        let r = ds5_input(&st(), false, &mut c);
        assert_eq!(r.len(), 64);
        assert_eq!(r[0], 1);
        assert_eq!(r[1], 0); // lx min
        assert_eq!(r[2], 0); // ly max (up) -> 0
        assert_eq!(r[3], 0x80);
        assert_eq!(r[5], 0xFF);
        assert_eq!(r[7], 0); // first sequence
        assert_eq!(r[8], 0x01 | 0x20); // NE + cross
        assert_eq!(r[9] & 0x05, 0x05); // L1 + L2 digital
        assert_eq!(r[10] & 0x01, 0x01); // PS
        assert_eq!(i16::from_le_bytes([r[16], r[17]]), 1);
        assert_eq!(i16::from_le_bytes([r[26], r[27]]), -8192);
        assert_eq!(u32::from_le_bytes([r[28], r[29], r[30], r[31]]), 3_000_000);
        assert_eq!(r[33], 5); // touch id, active
        assert_eq!(r[37], 0x80); // second point inactive
        assert_eq!(r[53] >> 4, 2); // full
        let r2 = ds5_input(&st(), false, &mut c);
        assert_eq!(r2[7], 1, "sequence increments");
        assert_eq!(r2[33], 5);
    }

    #[test]
    fn ds5_edge_buttons() {
        let mut c = Counters::default();
        let s = PadState { buttons: b::FN_LEFT | b::PADDLE2, ..PadState::neutral() };
        let r = ds5_input(&s, true, &mut c);
        assert_eq!(r[10] & 0xF0, 0x10 | 0x80);
        let r = ds5_input(&s, false, &mut c);
        assert_eq!(r[10] & 0xF0, 0);
    }

    #[test]
    fn ds4_layout_counters() {
        let mut c = Counters::default();
        let r = ds4_input(&st(), &mut c);
        assert_eq!(r[5], 0x01 | 0x20);
        assert_eq!(r[7] & 0x01, 0x01);
        assert_eq!(r[7] >> 2, 0);
        assert_eq!(r[8], 0xFF);
        assert_eq!(u16::from_le_bytes([r[10], r[11]]), ((1_000_000u64 * 3 / 16) & 0xFFFF) as u16);
        assert_eq!(r[30] & 0x10, 0x10);
        assert_eq!(r[34], 1, "touch packet counter moved");
        let x = (r[36] as u16) | (((r[37] & 0x0F) as u16) << 8);
        assert_eq!(x, 1919);
        let r = ds4_input(&st(), &mut c);
        assert_eq!(r[7] >> 2, 1);
        assert_eq!(r[34], 1, "no touch change");
        for _ in 0..70 {
            ds4_input(&st(), &mut c);
        }
        let r = ds4_input(&st(), &mut c);
        assert!(r[7] >> 2 < 64);
    }

    #[test]
    fn xone_roundtrip() {
        let s = st();
        let r = xone_input(&s);
        assert_eq!(r.len(), 62);
        let p = parse_xone_input(&r).unwrap();
        assert_eq!(p.buttons, s.buttons);
        assert_eq!(p.lx, s.lx);
        assert_eq!(p.ly, s.ly);
        assert_eq!(p.lt, 0xFFFF);
        assert_eq!(p.rt, 0);
        assert!(parse_xone_input(&r[..10]).is_none());
    }

    #[test]
    fn x360_mapping() {
        let x = x360_report(&st());
        assert_eq!(x.buttons, 0x0001 | 0x0008 | 0x0400 | 0x1000 | 0x0100);
        assert_eq!(x.left_trigger, 255);
        assert_eq!(x.ly, i16::MAX);
    }

    #[test]
    fn output_parsing() {
        let ds4 = ds4_output(10, 200, (1, 2, 3));
        assert_eq!(
            parse_output(PadKind::Ds4, &ds4),
            vec![Feedback::Rumble { strong: 200 * 257, weak: 10 * 257 }, Feedback::Led { r: 1, g: 2, b: 3 }]
        );
        let mut ds5 = [0u8; 48];
        ds5[0] = 0x02;
        ds5[1] = 0x01 | 0x04;
        ds5[2] = 0x04 | 0x10;
        ds5[3] = 7; // weak (right)
        ds5[4] = 9; // strong (left)
        ds5[11] = 0x21; // right trigger mode
        ds5[44] = 0x1F;
        ds5[45] = 10;
        ds5[46] = 20;
        ds5[47] = 30;
        let fb = parse_output(PadKind::Ds5, &ds5);
        assert_eq!(fb[0], Feedback::Rumble { strong: 9 * 257, weak: 7 * 257 });
        match &fb[1] {
            Feedback::AdaptiveTrigger { raw } => {
                assert_eq!(raw[0], 1);
                assert_eq!(raw[1], 0x21);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(fb[2], Feedback::Led { r: 10, g: 20, b: 30 });
        assert_eq!(fb[3], Feedback::PlayerLeds { mask: 0x1F });
        let x = xone_output_from_rumble(255, 0);
        assert_eq!(parse_output(PadKind::XOne, &x), vec![Feedback::Rumble { strong: 65535, weak: 0 }]);
        let t = [0x03, 0x0C, 50, 100, 0, 0, 0, 0, 0];
        assert_eq!(parse_output(PadKind::XOne, &t), vec![Feedback::TriggerRumble { left: 32767, right: 65535 }]);
        assert!(parse_output(PadKind::Ds5, &[0x02, 1]).is_empty());
        assert!(parse_output(PadKind::Ds4, &[0x11; 40]).is_empty());
    }
}
