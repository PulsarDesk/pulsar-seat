// SPDX-License-Identifier: GPL-3.0-only
//! Nintendo Switch Pro Controller as a **physical** pad of a local seat (USB 057E:2009 or
//! Bluetooth). The report formats are public interoperability facts (community protocol
//! documentation, Linux `hid-nintendo`, SDL HIDAPI); written fresh here.
//!
//! - Input: full report 0x30 (buttons, 12-bit sticks, three IMU samples) after the pad was
//!   switched to it, or the simple HID report 0x3F (default on Bluetooth).
//! - Output: report 0x01 = rumble + sub-command, 0x10 = rumble only, each with a 4-bit
//!   packet counter. USB only: report 0x80 commands (handshake, "HID only" mode) first.
//! - Rumble: 4 bytes per side (high band frequency / amplitude, low band frequency /
//!   amplitude). We use fixed frequencies (high 320 Hz, low 160 Hz, the neutral values) and
//!   scale only the amplitudes.
//!
//! - Stick calibration (parity review 2026-11-17): at open the pad is asked for its SPI
//!   flash calibration (sub-command 0x10: factory sticks at 0x603D, user sticks at 0x8010);
//!   the replies (report 0x21) set the centre and range of each axis ([`StickCal`]). Until
//!   they arrive, and when a block is blank, centre 2048 and ±1600 are used.
//!
//! HYPOTHESIS (untested on hardware): the init sequence timing (we write it once at open,
//! without waiting for each reply), the calibration block layout (public protocol notes),
//! the amplitude encoding (linear in the encoded value, capped at the documented safe
//! maximum) and the IMU axis order.

use crate::types::{buttons as b, PadState};

pub const VID_NINTENDO: u16 = 0x057E;
pub const PID_SWITCH_PRO: u16 = 0x2009;

/// Neutral rumble data of one side (320 Hz / 160 Hz, amplitude 0).
pub const RUMBLE_NEUTRAL: [u8; 4] = [0x00, 0x01, 0x40, 0x40];
/// Highest encoded amplitude we send (documented safe maximum, amplitude 1.0).
pub const MAX_AMP: u8 = 0xC8;

/// Sub-commands used at open.
pub mod subcmd {
    pub const SET_REPORT_MODE: u8 = 0x03;
    pub const SPI_READ: u8 = 0x10;
    pub const SET_PLAYER_LIGHTS: u8 = 0x30;
    pub const ENABLE_IMU: u8 = 0x40;
    pub const ENABLE_VIBRATION: u8 = 0x48;
}

/// Report mode argument "full report 0x30".
pub const MODE_FULL: u8 = 0x30;

/// Whether a USB id is a Switch Pro Controller.
pub fn is_switch_pro(vid: u16, pid: u16) -> bool {
    vid == VID_NINTENDO && pid == PID_SWITCH_PRO
}

/// One side's rumble bytes for an amplitude 0..=65535 at the neutral frequencies.
pub fn rumble_side(amp: u16) -> [u8; 4] {
    if amp == 0 {
        return RUMBLE_NEUTRAL;
    }
    // Encoded amplitude: even values 0..=MAX_AMP. High band: added to byte 1. Low band:
    // amp / 2 + 0x40 in byte 3 (bit 8 would go into byte 2; never set below MAX_AMP).
    let enc = ((u32::from(amp) * u32::from(MAX_AMP) / 65535) as u8) & !1;
    let lf_amp = 0x40 + u16::from(enc / 2);
    [0x00, 0x01u8.wrapping_add(enc), 0x40 | ((lf_amp >> 8) as u8 & 0x80), (lf_amp & 0xFF) as u8]
}

/// Rumble data (8 bytes): left side = `strong`, right side = `weak`.
pub fn rumble_data(strong: u16, weak: u16) -> [u8; 8] {
    let mut out = [0u8; 8];
    out[..4].copy_from_slice(&rumble_side(strong));
    out[4..].copy_from_slice(&rumble_side(weak));
    out
}

/// Output report 0x10 (rumble only).
pub fn rumble_report(counter: u8, strong: u16, weak: u16) -> Vec<u8> {
    let mut r = vec![0x10, counter & 0x0F];
    r.extend_from_slice(&rumble_data(strong, weak));
    r
}

/// Output report 0x01 (neutral rumble + one sub-command with its arguments).
pub fn subcommand_report(counter: u8, id: u8, args: &[u8]) -> Vec<u8> {
    let mut r = vec![0x01, counter & 0x0F];
    r.extend_from_slice(&RUMBLE_NEUTRAL);
    r.extend_from_slice(&RUMBLE_NEUTRAL);
    r.push(id);
    r.extend_from_slice(args);
    r
}

/// Player lights from a DualSense-style player LED mask (bits 0..=4) -> the pad's four
/// LEDs (low nibble = on, as many as the mask has bits).
pub fn player_lights(mask: u8) -> u8 {
    let n = (mask & 0x1F).count_ones().min(4);
    ((1u16 << n) - 1) as u8
}

/// Reports to write when the pad is opened, in order. `counter` is advanced for each
/// sub-command. USB pads first get the 0x80 handshake and "HID only" commands.
pub fn init_reports(usb: bool, counter: &mut u8, slot: u8) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    if usb {
        // Handshake, 3 Mbit/s, handshake again, then HID only (no USB timeout).
        out.extend([vec![0x80, 0x02], vec![0x80, 0x03], vec![0x80, 0x02], vec![0x80, 0x04]]);
    }
    let mut next = |id: u8, args: &[u8]| {
        let r = subcommand_report(*counter, id, args);
        *counter = counter.wrapping_add(1) & 0x0F;
        r
    };
    out.push(next(subcmd::SET_REPORT_MODE, &[MODE_FULL]));
    out.push(next(subcmd::ENABLE_IMU, &[0x01]));
    out.push(next(subcmd::ENABLE_VIBRATION, &[0x01]));
    // Player N lights LED N (slot 0 = LED 1).
    out.push(next(subcmd::SET_PLAYER_LIGHTS, &[1u8 << (slot % 4)]));
    out
}

/// SPI address of the factory stick calibration (left 9 bytes, right 9 bytes).
pub const SPI_FACTORY_STICKS: u32 = 0x603D;
/// SPI address of the user stick calibration (left: magic `B2 A1` + 9 bytes, right: same).
pub const SPI_USER_STICKS: u32 = 0x8010;

/// One axis: centre and the reach below / above it (12-bit units).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AxisCal {
    pub center: u16,
    pub below: u16,
    pub above: u16,
}

impl Default for AxisCal {
    fn default() -> Self {
        Self { center: 2048, below: 1600, above: 1600 }
    }
}

impl AxisCal {
    /// 12-bit value -> signed 16-bit.
    pub fn scale(&self, v: u16) -> i16 {
        let d = i32::from(v) - i32::from(self.center);
        let out = if d >= 0 { d * 32767 / i32::from(self.above.max(1)) } else { d * 32768 / i32::from(self.below.max(1)) };
        out.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
    }
}

/// Both sticks' calibration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StickCal {
    pub lx: AxisCal,
    pub ly: AxisCal,
    pub rx: AxisCal,
    pub ry: AxisCal,
}

/// Six packed 12-bit values from 9 bytes.
fn unpack6(d: &[u8]) -> [u16; 6] {
    let mut out = [0u16; 6];
    for i in 0..3 {
        let (a, b) = unpack_stick(&d[i * 3..i * 3 + 3]);
        out[i * 2] = a;
        out[i * 2 + 1] = b;
    }
    out
}

/// A calibration block that is all 0xFF (never written) or too small is ignored.
fn usable(d: &[u8]) -> bool {
    d.len() >= 9 && !d[..9].iter().all(|&b| b == 0xFF)
}

/// Left stick block: x / y above centre, x / y centre, x / y below centre.
pub fn left_cal(d: &[u8]) -> Option<(AxisCal, AxisCal)> {
    if !usable(d) {
        return None;
    }
    let v = unpack6(d);
    Some((AxisCal { above: v[0], center: v[2], below: v[4] }, AxisCal { above: v[1], center: v[3], below: v[5] }))
}

/// Right stick block: x / y centre, x / y below centre, x / y above centre.
pub fn right_cal(d: &[u8]) -> Option<(AxisCal, AxisCal)> {
    if !usable(d) {
        return None;
    }
    let v = unpack6(d);
    Some((AxisCal { center: v[0], below: v[2], above: v[4] }, AxisCal { center: v[1], below: v[3], above: v[5] }))
}

/// Output report 0x01 with sub-command 0x10: read `len` bytes of SPI flash at `addr`.
pub fn spi_read_report(counter: u8, addr: u32, len: u8) -> Vec<u8> {
    let mut args = addr.to_le_bytes().to_vec();
    args.push(len);
    subcommand_report(counter, subcmd::SPI_READ, &args)
}

/// The calibration reads sent after the init reports, in order.
pub fn calibration_reads(counter: &mut u8) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for (addr, len) in [(SPI_FACTORY_STICKS, 18u8), (SPI_USER_STICKS, 22u8)] {
        out.push(spi_read_report(*counter, addr, len));
        *counter = counter.wrapping_add(1) & 0x0F;
    }
    out
}

/// `(address, data)` of an SPI read reply (report 0x21, ACK with data, sub-command 0x10).
pub fn parse_spi_reply(r: &[u8]) -> Option<(u32, &[u8])> {
    if r.first() != Some(&0x21) || r.len() < 20 || r[13] & 0x80 == 0 || r[14] != subcmd::SPI_READ {
        return None;
    }
    let addr = u32::from_le_bytes(r[15..19].try_into().ok()?);
    let len = usize::from(r[19]);
    Some((addr, r.get(20..20 + len)?))
}

impl StickCal {
    /// Applies an SPI read reply; `true` when it carried calibration. User calibration
    /// (magic `B2 A1`) wins over the factory one.
    pub fn apply_reply(&mut self, addr: u32, data: &[u8]) -> bool {
        match addr {
            SPI_FACTORY_STICKS => {
                let mut any = false;
                if let Some((x, y)) = left_cal(data) {
                    (self.lx, self.ly) = (x, y);
                    any = true;
                }
                if let Some((x, y)) = data.get(9..).and_then(right_cal) {
                    (self.rx, self.ry) = (x, y);
                    any = true;
                }
                any
            }
            SPI_USER_STICKS => {
                let mut any = false;
                if data.get(0..2) == Some(&[0xB2, 0xA1]) && let Some((x, y)) = data.get(2..).and_then(left_cal) {
                    (self.lx, self.ly) = (x, y);
                    any = true;
                }
                if data.get(11..13) == Some(&[0xB2, 0xA1]) && let Some((x, y)) = data.get(13..).and_then(right_cal) {
                    (self.rx, self.ry) = (x, y);
                    any = true;
                }
                any
            }
            _ => false,
        }
    }
}

/// 12-bit stick value -> signed 16-bit (centre 2048, ±1600 assumed).
fn stick12(v: u16) -> i16 {
    AxisCal::default().scale(v)
}

fn unpack_stick(d: &[u8]) -> (u16, u16) {
    let x = u16::from(d[0]) | (u16::from(d[1] & 0x0F) << 8);
    let y = u16::from(d[1] >> 4) | (u16::from(d[2]) << 4);
    (x, y)
}

fn get_i16(r: &[u8], at: usize) -> i16 {
    i16::from_le_bytes([r[at], r[at + 1]])
}

/// Full input report 0x30 -> pad state. Positional face buttons (B = south, A = east,
/// Y = west, X = north); ZL / ZR are digital triggers; Capture = misc. Motion: the newest
/// IMU sample, accel scaled from 4096 / g to 8192 / g, gyro from 2000 dps full scale to the
/// pad state's calibration (HYPOTHESIS: axis order).
pub fn parse_full(r: &[u8]) -> Option<PadState> {
    parse_full_cal(r, None)
}

/// [`parse_full`] with the pad's own stick calibration.
pub fn parse_full_cal(r: &[u8], cal: Option<&StickCal>) -> Option<PadState> {
    if r.len() < 49 || r[0] != 0x30 {
        return None;
    }
    let (right, shared, left) = (r[3], r[4], r[5]);
    let mut buttons = 0u32;
    for (byte, mask, bit) in [
        (right, 0x01, b::WEST),
        (right, 0x02, b::NORTH),
        (right, 0x04, b::SOUTH),
        (right, 0x08, b::EAST),
        (right, 0x40, b::R1),
        (shared, 0x01, b::BACK),
        (shared, 0x02, b::START),
        (shared, 0x04, b::R3),
        (shared, 0x08, b::L3),
        (shared, 0x10, b::GUIDE),
        (shared, 0x20, b::MISC),
        (left, 0x01, b::DPAD_DOWN),
        (left, 0x02, b::DPAD_UP),
        (left, 0x04, b::DPAD_RIGHT),
        (left, 0x08, b::DPAD_LEFT),
        (left, 0x40, b::L1),
    ] {
        if byte & mask != 0 {
            buttons |= bit;
        }
    }
    let (lx, ly) = unpack_stick(&r[6..9]);
    let (rx, ry) = unpack_stick(&r[9..12]);
    let (lxs, lys, rxs, rys) = match cal {
        Some(c) => (c.lx.scale(lx), c.ly.scale(ly), c.rx.scale(rx), c.ry.scale(ry)),
        None => (stick12(lx), stick12(ly), stick12(rx), stick12(ry)),
    };
    let mut s = PadState {
        buttons,
        lx: lxs,
        ly: lys,
        rx: rxs,
        ry: rys,
        lt: if left & 0x80 != 0 { 65535 } else { 0 },
        rt: if right & 0x80 != 0 { 65535 } else { 0 },
        ..PadState::neutral()
    };
    // Third (newest) IMU sample at 13 + 2 * 12.
    let at = 13 + 24;
    for i in 0..3 {
        s.accel[i] = get_i16(r, at + 2 * i).saturating_mul(2);
        // 2000 dps full scale (16.384 LSB per dps) -> CAL_GYRO_RANGE per CAL_GYRO_SPEED.
        let g = i32::from(get_i16(r, at + 6 + 2 * i));
        let scaled = g * 2000 / 32768 * i32::from(crate::features::CAL_GYRO_RANGE) / i32::from(crate::features::CAL_GYRO_SPEED);
        s.gyro[i] = scaled.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
    }
    Some(s)
}

/// Simple HID report 0x3F -> pad state (no motion). Sticks are 16-bit, HID direction
/// (+y = down).
pub fn parse_simple(r: &[u8]) -> Option<PadState> {
    if r.len() < 12 || r[0] != 0x3F {
        return None;
    }
    let mut buttons = 0u32;
    for (byte, mask, bit) in [
        (r[1], 0x01, b::SOUTH),
        (r[1], 0x02, b::EAST),
        (r[1], 0x04, b::WEST),
        (r[1], 0x08, b::NORTH),
        (r[1], 0x10, b::L1),
        (r[1], 0x20, b::R1),
        (r[2], 0x01, b::BACK),
        (r[2], 0x02, b::START),
        (r[2], 0x04, b::L3),
        (r[2], 0x08, b::R3),
        (r[2], 0x10, b::GUIDE),
        (r[2], 0x20, b::MISC),
    ] {
        if byte & mask != 0 {
            buttons |= bit;
        }
    }
    buttons |= match r[3] {
        0 => b::DPAD_UP,
        1 => b::DPAD_UP | b::DPAD_RIGHT,
        2 => b::DPAD_RIGHT,
        3 => b::DPAD_DOWN | b::DPAD_RIGHT,
        4 => b::DPAD_DOWN,
        5 => b::DPAD_DOWN | b::DPAD_LEFT,
        6 => b::DPAD_LEFT,
        7 => b::DPAD_UP | b::DPAD_LEFT,
        _ => 0,
    };
    let s16 = |at: usize| (i32::from(u16::from_le_bytes([r[at], r[at + 1]])) - 32768) as i16;
    Some(PadState {
        buttons,
        lx: s16(4),
        ly: s16(6).saturating_neg(),
        rx: s16(8),
        ry: s16(10).saturating_neg(),
        lt: if r[1] & 0x40 != 0 { 65535 } else { 0 },
        rt: if r[1] & 0x80 != 0 { 65535 } else { 0 },
        ..PadState::neutral()
    })
}

/// Either input report -> pad state; `None` for replies (0x21, 0x81) and other reports.
pub fn parse_input(r: &[u8]) -> Option<PadState> {
    parse_input_cal(r, None)
}

/// [`parse_input`] with the pad's own stick calibration (full reports only).
pub fn parse_input_cal(r: &[u8], cal: Option<&StickCal>) -> Option<PadState> {
    match r.first() {
        Some(0x30) => parse_full_cal(r, cal),
        Some(0x3F) => parse_simple(r),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rumble_encoding() {
        assert_eq!(rumble_side(0), RUMBLE_NEUTRAL);
        let full = rumble_side(65535);
        assert_eq!(full, [0x00, 0xC9, 0x40, 0xA4]);
        let half = rumble_side(32768);
        assert_eq!(half[1], 0x01 + 0x64);
        let r = rumble_report(0x13, 65535, 0);
        assert_eq!(r.len(), 10);
        assert_eq!((r[0], r[1]), (0x10, 0x03));
        assert_eq!(&r[6..10], &RUMBLE_NEUTRAL);
    }

    #[test]
    fn init_sequence() {
        let mut c = 0;
        let usb = init_reports(true, &mut c, 0);
        assert_eq!(usb[0], vec![0x80, 0x02]);
        assert_eq!(usb[3], vec![0x80, 0x04]);
        assert_eq!(usb.len(), 8);
        assert_eq!(&usb[4][..2], &[0x01, 0x00]);
        assert_eq!((usb[4][10], usb[4][11]), (subcmd::SET_REPORT_MODE, MODE_FULL));
        assert_eq!(c, 4);
        let bt = init_reports(false, &mut c, 1);
        assert_eq!(bt.len(), 4);
        assert_eq!(bt[0][1], 4);
        assert_eq!(player_lights(0b0_0011), 0b0011);
        assert_eq!(player_lights(0x1F), 0x0F);
    }

    #[test]
    fn full_report() {
        let mut r = vec![0u8; 49];
        r[0] = 0x30;
        r[3] = 0x08 | 0x80; // A + ZR
        r[4] = 0x10 | 0x02; // Home + Plus
        r[5] = 0x02 | 0x40; // Up + L
        // Left stick x = 0xFFF, y = 2048; right stick centred.
        r[6] = 0xFF;
        r[7] = 0x0F;
        r[8] = 0x80;
        r[9] = 0x00;
        r[10] = 0x08;
        r[11] = 0x80;
        r[37] = 0x00;
        r[38] = 0x10; // accel x = 4096 (1 g)
        let s = parse_full(&r).unwrap();
        assert_eq!(s.buttons, b::EAST | b::GUIDE | b::START | b::DPAD_UP | b::L1);
        assert_eq!((s.lt, s.rt), (0, 65535));
        assert_eq!(s.lx, 32767);
        assert_eq!(s.ly, 0);
        assert_eq!((s.rx, s.ry), (0, 0));
        assert_eq!(s.accel[0], 8192);
        assert!(parse_input(&[0x21, 0]).is_none());
    }

    #[test]
    fn simple_report() {
        let mut r = vec![0u8; 12];
        r[0] = 0x3F;
        r[1] = 0x01 | 0x40;
        r[3] = 8;
        r[4..6].copy_from_slice(&0xFFFFu16.to_le_bytes());
        r[6..8].copy_from_slice(&0u16.to_le_bytes());
        r[8..10].copy_from_slice(&0x8000u16.to_le_bytes());
        r[10..12].copy_from_slice(&0x8000u16.to_le_bytes());
        let s = parse_input(&r).unwrap();
        assert_eq!(s.buttons, b::SOUTH);
        assert_eq!((s.lx, s.ly, s.rx, s.ry), (32767, 32767, 0, 0));
        assert_eq!(s.lt, 65535);
        assert!(is_switch_pro(0x057E, 0x2009));
    }

    #[test]
    fn spi_calibration_reads_and_replies() {
        let mut c = 3u8;
        let reads = calibration_reads(&mut c);
        assert_eq!(c, 5);
        assert_eq!(&reads[0][10..16], &[subcmd::SPI_READ, 0x3D, 0x60, 0, 0, 18]);
        assert_eq!(&reads[1][10..16], &[subcmd::SPI_READ, 0x10, 0x80, 0, 0, 22]);
        // Factory block: left above (1500, 1400), centre (2000, 2100), below (1300, 1200);
        // right centre (2050, 2040), below (1450, 1460), above (1550, 1560).
        let pack = |a: u16, b: u16| [(a & 0xFF) as u8, ((a >> 8) as u8 & 0x0F) | ((b & 0x0F) << 4) as u8, (b >> 4) as u8];
        let mut data = Vec::new();
        for (a, b) in [(1500, 1400), (2000, 2100), (1300, 1200), (2050, 2040), (1450, 1460), (1550, 1560)] {
            data.extend_from_slice(&pack(a, b));
        }
        let mut reply = vec![0u8; 20];
        reply[0] = 0x21;
        reply[13] = 0x90;
        reply[14] = subcmd::SPI_READ;
        reply[15..19].copy_from_slice(&SPI_FACTORY_STICKS.to_le_bytes());
        reply[19] = 18;
        reply.extend_from_slice(&data);
        let (addr, d) = parse_spi_reply(&reply).unwrap();
        assert_eq!(addr, SPI_FACTORY_STICKS);
        let mut cal = StickCal::default();
        assert!(cal.apply_reply(addr, d));
        assert_eq!(cal.lx, AxisCal { center: 2000, below: 1300, above: 1500 });
        assert_eq!(cal.ry, AxisCal { center: 2040, below: 1460, above: 1560 });
        assert_eq!(cal.lx.scale(2000), 0);
        assert_eq!(cal.lx.scale(3500), i16::MAX);
        assert_eq!(cal.lx.scale(700), i16::MIN);
        // A blank user block (all 0xFF) changes nothing.
        let mut blank = cal;
        assert!(!blank.apply_reply(SPI_USER_STICKS, &[0xFF; 22]));
        assert_eq!(blank, cal);
        // Not a reply.
        assert!(parse_spi_reply(&[0x30; 64]).is_none());
    }
}
