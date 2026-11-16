// SPDX-License-Identifier: GPL-3.0-only
//! Bluetooth output reports of physical pads (feedback from a local seat's virtual pad back
//! to the pad in the player's hands). The layouts are public interoperability facts (Linux
//! `hid-playstation`, `hid-microsoft`, SDL HIDAPI drivers), written fresh here:
//!
//! | Pad | Report | Body | Integrity |
//! | --- | --- | --- | --- |
//! | DualSense / Edge | 0x31, 78 bytes | byte 1 = sequence << 4, byte 2 = tag 0x10, bytes 3..50 = the USB report 0x02 body | CRC-32 (seed byte 0xA2, then bytes 0..74) at 74..78 |
//! | DualShock 4 | 0x11, 78 bytes | byte 1 = 0xC0 (HID + CRC), byte 2 = 0x00, bytes 3..13 = the USB report 0x05 body | same CRC-32 at 74..78 |
//! | Xbox (BT HID) | 0x03, 9 bytes | enable mask, left / right trigger, strong / weak motor (0..=100), duration, delay, repeat | none |
//!
//! HYPOTHESIS (untested on hardware): every layout here was checked against the public
//! sources only, never written to a real pad.

use crate::layout;
use crate::types::PadKind;

/// Output report length of the PlayStation Bluetooth reports (0x31 / 0x11).
pub const PS_BT_OUTPUT_LEN: usize = 78;
/// Seed byte hashed before a PlayStation Bluetooth output report ("output" HID header).
pub const PS_OUTPUT_CRC_SEED: u8 = 0xA2;
/// Seed byte hashed before a PlayStation Bluetooth input report ("input" HID header).
pub const PS_INPUT_CRC_SEED: u8 = 0xA1;
/// Xbox Bluetooth rumble report length (with id).
pub const XBOX_BT_OUTPUT_LEN: usize = 9;

/// CRC-32 (IEEE 802.3, reflected polynomial 0xEDB88320), as used by the PlayStation pads.
pub fn crc32(data: &[u8]) -> u32 {
    crc32_update(0xFFFF_FFFF, data) ^ 0xFFFF_FFFF
}

fn crc32_update(mut crc: u32, data: &[u8]) -> u32 {
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    crc
}

/// CRC of a PlayStation Bluetooth report: the seed byte, then `body`.
pub fn ps_crc(seed: u8, body: &[u8]) -> u32 {
    crc32_update(crc32_update(0xFFFF_FFFF, &[seed]), body) ^ 0xFFFF_FFFF
}

fn seal(r: &mut [u8; PS_BT_OUTPUT_LEN]) {
    let crc = ps_crc(PS_OUTPUT_CRC_SEED, &r[..PS_BT_OUTPUT_LEN - 4]);
    r[PS_BT_OUTPUT_LEN - 4..].copy_from_slice(&crc.to_le_bytes());
}

/// Whether a PlayStation Bluetooth report carries a valid CRC (input: seed 0xA1).
pub fn ps_crc_ok(seed: u8, report: &[u8]) -> bool {
    if report.len() < 5 {
        return false;
    }
    let (body, tail) = report.split_at(report.len() - 4);
    let want = u32::from_le_bytes([tail[0], tail[1], tail[2], tail[3]]);
    ps_crc(seed, body) == want
}

/// DualSense USB output report 0x02 (48 bytes) -> Bluetooth report 0x31. `seq` is the
/// 4-bit sequence number (the caller counts it per pad). `None` if `usb` is not a 0x02
/// report.
pub fn ds5_bt_output(usb: &[u8], seq: u8) -> Option<[u8; PS_BT_OUTPUT_LEN]> {
    if usb.first() != Some(&0x02) || usb.len() < 2 {
        return None;
    }
    let mut r = [0u8; PS_BT_OUTPUT_LEN];
    r[0] = 0x31;
    r[1] = (seq & 0x0F) << 4;
    r[2] = 0x10;
    let body = &usb[1..usb.len().min(layout::DS5_OUTPUT_LEN)];
    r[3..3 + body.len()].copy_from_slice(body);
    seal(&mut r);
    Some(r)
}

/// DualShock 4 USB output report 0x05 -> Bluetooth report 0x11 (HID + CRC flags).
pub fn ds4_bt_output(usb: &[u8]) -> Option<[u8; PS_BT_OUTPUT_LEN]> {
    if usb.first() != Some(&0x05) || usb.len() < 2 {
        return None;
    }
    let mut r = [0u8; PS_BT_OUTPUT_LEN];
    r[0] = 0x11;
    r[1] = 0xC0;
    r[2] = 0x00;
    // The common part (flags, motors, lightbar, flash) is the USB body's first 10 bytes;
    // the USB report's audio / volume tail has no place in report 0x11.
    let body = &usb[1..usb.len().min(11)];
    r[3..3 + body.len()].copy_from_slice(body);
    seal(&mut r);
    Some(r)
}

/// A PlayStation USB output report rebuilt for Bluetooth (`kind` = the physical pad).
pub fn ps_bt_output(kind: PadKind, usb: &[u8], seq: u8) -> Option<[u8; PS_BT_OUTPUT_LEN]> {
    match kind {
        PadKind::Ds5 | PadKind::Ds5Edge => ds5_bt_output(usb, seq),
        PadKind::Ds4 => ds4_bt_output(usb),
        _ => None,
    }
}

/// Percent (0..=100) of a 16-bit motor value.
fn pct16(v: u16) -> u8 {
    (u32::from(v) * 100 / 65535) as u8
}

/// Xbox Bluetooth rumble report 0x03: `strong` / `weak` body motors and the impulse
/// triggers (`lt` / `rt`), all 0..=65535. Enable mask bits: 0x01 weak, 0x02 strong,
/// 0x04 right trigger, 0x08 left trigger (HYPOTHESIS: bit order from public sources).
pub fn xbox_bt_output(strong: u16, weak: u16, lt: u16, rt: u16) -> [u8; XBOX_BT_OUTPUT_LEN] {
    [0x03, 0x0F, pct16(lt), pct16(rt), pct16(strong), pct16(weak), 0xFF, 0x00, 0x00]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reports::{ds4_output, ds5_output, Ds5Output};

    #[test]
    fn crc32_check_value() {
        // The standard CRC-32 check value of "123456789".
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(ps_crc(0xA2, b"123456789"), crc32(&[b"\xA2".as_slice(), b"123456789"].concat()));
    }

    #[test]
    fn ds5_bt_wraps_usb_body_and_seals() {
        let usb = ds5_output(&Ds5Output { weak: 0x40, strong: 0x80, rgb: Some((1, 2, 3)), ..Default::default() });
        let bt = ds5_bt_output(&usb, 0x1A).unwrap();
        assert_eq!((bt[0], bt[1], bt[2]), (0x31, 0xA0, 0x10));
        assert_eq!(&bt[3..3 + 47], &usb[1..48]);
        assert!(ps_crc_ok(PS_OUTPUT_CRC_SEED, &bt));
        let mut broken = bt;
        broken[10] ^= 1;
        assert!(!ps_crc_ok(PS_OUTPUT_CRC_SEED, &broken));
        assert!(ds5_bt_output(&[0x05, 0], 0).is_none());
    }

    #[test]
    fn ds4_bt_wraps_common_part() {
        let usb = ds4_output(0x11, 0x22, (9, 8, 7));
        let bt = ds4_bt_output(&usb).unwrap();
        assert_eq!((bt[0], bt[1]), (0x11, 0xC0));
        assert_eq!(bt[3], 0x03);
        assert_eq!((bt[6], bt[7]), (0x11, 0x22));
        assert_eq!((bt[8], bt[9], bt[10]), (9, 8, 7));
        assert!(ps_crc_ok(PS_OUTPUT_CRC_SEED, &bt));
        assert!(ps_bt_output(PadKind::Ds4, &usb, 0).is_some());
        assert!(ps_bt_output(PadKind::XOne, &usb, 0).is_none());
    }

    #[test]
    fn xbox_rumble_percent() {
        let r = xbox_bt_output(65535, 32768, 0, 65535);
        assert_eq!(r, [0x03, 0x0F, 0, 100, 100, 50, 0xFF, 0, 0]);
    }
}
