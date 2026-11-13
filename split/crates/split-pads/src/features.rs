// SPDX-License-Identifier: GPL-3.0-only
//! Feature report answers (spec 60 §6.4). Layouts follow the public drivers (Linux
//! `hid-playstation`): calibration with plausible non-zero ranges, pairing info with the
//! per-pad MAC (byte order reversed, as in the PlayStation reports), firmware blocks.
//! `core` only: shared with the UMDF driver through `#[path]`.

use crate::layout::{KIND_DS4, KIND_DS5, KIND_DS5E};

/// Buffer too small for the requested feature report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BufferTooSmall {
    pub needed: usize,
}

/// Gyro range used in the calibration blocks (raw units at the plus/minus reference).
pub const CAL_GYRO_RANGE: i16 = 8192;
/// Gyro reference speed (deg/s) matching `CAL_GYRO_RANGE`.
pub const CAL_GYRO_SPEED: i16 = 540;
/// Accelerometer raw units for 1 g.
pub const CAL_ACCEL_1G: i16 = 8192;

/// Size (with report id) of a feature report this crate fills with real content, or the
/// minimum size 1 for ids answered with zeros.
pub const fn feature_len(kind: u32, id: u8) -> usize {
    match (kind, id) {
        (KIND_DS4, 0x02) => 37,
        (KIND_DS4, 0x12) => 16,
        (KIND_DS4, 0x81) => 7,
        (KIND_DS4, 0xA3) => 49,
        (KIND_DS5 | KIND_DS5E, 0x05) => 41,
        (KIND_DS5 | KIND_DS5E, 0x09) => 20,
        (KIND_DS5 | KIND_DS5E, 0x20) => 64,
        _ => 1,
    }
}

fn put_i16(buf: &mut [u8], at: usize, v: i16) {
    let b = v.to_le_bytes();
    buf[at] = b[0];
    buf[at + 1] = b[1];
}

fn put_u16(buf: &mut [u8], at: usize, v: u16) {
    let b = v.to_le_bytes();
    buf[at] = b[0];
    buf[at + 1] = b[1];
}

fn put_u32(buf: &mut [u8], at: usize, v: u32) {
    let b = v.to_le_bytes();
    buf[at..at + 4].copy_from_slice(&b);
}

fn put_str(buf: &mut [u8], at: usize, s: &str) {
    let b = s.as_bytes();
    buf[at..at + b.len()].copy_from_slice(b);
}

/// USB-order calibration block shared by DS4 0x02 and DualSense 0x05: three gyro biases,
/// then plus/minus per gyro axis, gyro speed plus/minus, then plus/minus per accel axis.
fn calibration(buf: &mut [u8]) {
    let mut at = 1;
    for _ in 0..3 {
        put_i16(buf, at, 0);
        at += 2;
    }
    for _ in 0..3 {
        put_i16(buf, at, CAL_GYRO_RANGE);
        put_i16(buf, at + 2, -CAL_GYRO_RANGE);
        at += 4;
    }
    put_i16(buf, at, CAL_GYRO_SPEED);
    put_i16(buf, at + 2, CAL_GYRO_SPEED);
    at += 4;
    for _ in 0..3 {
        put_i16(buf, at, CAL_ACCEL_1G);
        put_i16(buf, at + 2, -CAL_ACCEL_1G);
        at += 4;
    }
}

fn mac_reversed(buf: &mut [u8], at: usize, mac: &[u8; 6]) {
    for i in 0..6 {
        buf[at + i] = mac[5 - i];
    }
}

/// Fixed host MAC reported in DS4 pairing info (locally administered, never a real NIC).
pub const HOST_MAC: [u8; 6] = [0x02, 0x50, 0x53, 0x50, 0x41, 0x44];

/// Fills `buf` with feature report `id` for `kind`. Returns the number of bytes written
/// (with the report id in byte 0). Unknown ids return zeros with the id, of the size the
/// caller asked for (spec 60 §6.4 "any | unknown id").
pub fn feature_report(kind: u32, id: u8, mac: &[u8; 6], buf: &mut [u8]) -> Result<usize, BufferTooSmall> {
    let needed = feature_len(kind, id);
    if buf.len() < needed || buf.is_empty() {
        return Err(BufferTooSmall { needed });
    }
    let n = if needed == 1 { buf.len() } else { needed };
    for b in buf[..n].iter_mut() {
        *b = 0;
    }
    buf[0] = id;
    match (kind, id) {
        (KIND_DS4, 0x02) | (KIND_DS5 | KIND_DS5E, 0x05) => calibration(buf),
        (KIND_DS4, 0x12) => {
            mac_reversed(buf, 1, mac);
            // bytes 7..9: link-key class bytes (0x08 0x25 0x00 on real pads)
            buf[7] = 0x08;
            buf[8] = 0x25;
            mac_reversed(buf, 10, &HOST_MAC);
        }
        (KIND_DS4, 0x81) => mac_reversed(buf, 1, mac),
        (KIND_DS4, 0xA3) => {
            put_str(buf, 1, "Jan  1 2026");
            put_str(buf, 17, "00:00:00");
            put_u16(buf, 35, 0x0100); // hardware version
            put_u16(buf, 41, 0x0801); // firmware version
        }
        (KIND_DS5 | KIND_DS5E, 0x09) => mac_reversed(buf, 1, mac),
        (KIND_DS5 | KIND_DS5E, 0x20) => {
            put_str(buf, 1, "Jan 01 2026");
            put_str(buf, 12, "00:00:00");
            put_u32(buf, 24, 0x0000_0613); // hardware info
            put_u32(buf, 28, 0x0110_002A); // firmware version
            put_u16(buf, 44, 0x0630); // update version (enables newer features in drivers)
        }
        _ => {}
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAC: [u8; 6] = [0x02, 0x11, 0x22, 0x33, 0x44, 0x55];

    #[test]
    fn ds5_pairing_mac_reversed() {
        let mut b = [0u8; 20];
        assert_eq!(feature_report(KIND_DS5, 0x09, &MAC, &mut b), Ok(20));
        assert_eq!(b[0], 0x09);
        assert_eq!(&b[1..7], &[0x55, 0x44, 0x33, 0x22, 0x11, 0x02]);
    }

    #[test]
    fn calibration_non_zero_ranges() {
        let mut b = [0u8; 41];
        feature_report(KIND_DS5, 0x05, &MAC, &mut b).unwrap();
        let gp = i16::from_le_bytes([b[7], b[8]]);
        let gm = i16::from_le_bytes([b[9], b[10]]);
        assert!(gp > 0 && gm < 0);
        let mut d = [0u8; 37];
        feature_report(KIND_DS4, 0x02, &MAC, &mut d).unwrap();
        assert_eq!(&d[1..35], &b[1..35]);
    }

    #[test]
    fn too_small_and_unknown() {
        let mut b = [0u8; 10];
        assert_eq!(feature_report(KIND_DS5, 0x20, &MAC, &mut b), Err(BufferTooSmall { needed: 64 }));
        let mut u = [0xFFu8; 12];
        assert_eq!(feature_report(KIND_DS5E, 0x70, &MAC, &mut u), Ok(12));
        assert_eq!(u[0], 0x70);
        assert!(u[1..].iter().all(|&x| x == 0));
        assert!(feature_report(KIND_DS4, 0x81, &MAC, &mut []).is_err());
    }

    #[test]
    fn ds5_firmware_fields() {
        let mut b = [0u8; 64];
        feature_report(KIND_DS5, 0x20, &MAC, &mut b).unwrap();
        assert_eq!(u32::from_le_bytes([b[28], b[29], b[30], b[31]]), 0x0110_002A);
    }
}
