// SPDX-License-Identifier: GPL-3.0-only
//! ViGEmBus IOCTL interface, encoded by hand (no ViGEmClient code is used). Layout as
//! documented by the public ViGEmClient headers (MIT, Nefarius Software Solutions e.U.;
//! `include/ViGEm/km/BusShared.h`, `include/ViGEm/Common.h`), checked 2026-11-12.

use crate::reports::XusbReport;

/// `GUID_DEVINTERFACE_BUSENUM_VIGEM` {96E42B22-F5E9-42F8-B043-ED0F932F014F}.
pub const BUS_INTERFACE_GUID: (u32, u16, u16, [u8; 8]) =
    (0x96E4_2B22, 0xF5E9, 0x42F8, [0xB0, 0x43, 0xED, 0x0F, 0x93, 0x2F, 0x01, 0x4F]);
pub const COMMON_VERSION: u32 = 0x0001;

const FILE_DEVICE_BUS_EXTENDER: u32 = 0x2A;
const METHOD_BUFFERED: u32 = 0;
const FILE_READ_DATA: u32 = 1;
const FILE_WRITE_DATA: u32 = 2;
const BASE: u32 = 0x801;

const fn ctl(func: u32, access: u32) -> u32 {
    (FILE_DEVICE_BUS_EXTENDER << 16) | (access << 14) | (func << 2) | METHOD_BUFFERED
}

pub const IOCTL_PLUGIN_TARGET: u32 = ctl(BASE, FILE_WRITE_DATA);
pub const IOCTL_UNPLUG_TARGET: u32 = ctl(BASE + 0x001, FILE_WRITE_DATA);
pub const IOCTL_CHECK_VERSION: u32 = ctl(BASE + 0x002, FILE_WRITE_DATA);
pub const IOCTL_WAIT_DEVICE_READY: u32 = ctl(BASE + 0x003, FILE_WRITE_DATA);
pub const IOCTL_XUSB_REQUEST_NOTIFICATION: u32 = ctl(BASE + 0x200, FILE_WRITE_DATA | FILE_READ_DATA);
pub const IOCTL_XUSB_SUBMIT_REPORT: u32 = ctl(BASE + 0x201, FILE_WRITE_DATA);
pub const IOCTL_DS4_SUBMIT_REPORT: u32 = ctl(BASE + 0x202, FILE_WRITE_DATA);
pub const IOCTL_DS4_REQUEST_NOTIFICATION: u32 = ctl(BASE + 0x203, FILE_WRITE_DATA);
pub const IOCTL_XUSB_GET_USER_INDEX: u32 = ctl(BASE + 0x206, FILE_WRITE_DATA | FILE_READ_DATA);
pub const IOCTL_DS4_AWAIT_OUTPUT_AVAILABLE: u32 = ctl(BASE + 0x207, FILE_WRITE_DATA | FILE_READ_DATA);

/// `VIGEM_TARGET_TYPE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum TargetType {
    Xbox360Wired = 0,
    DualShock4Wired = 2,
}

fn hdr<const N: usize>(serial: u32) -> [u8; N] {
    let mut b = [0u8; N];
    b[0..4].copy_from_slice(&(N as u32).to_le_bytes());
    b[4..8].copy_from_slice(&serial.to_le_bytes());
    b
}

/// `VIGEM_PLUGIN_TARGET` (16 bytes). VID/PID 0 = bus default.
pub fn plugin(serial: u32, t: TargetType, vid: u16, pid: u16) -> [u8; 16] {
    let mut b = hdr::<16>(serial);
    b[8..12].copy_from_slice(&(t as u32).to_le_bytes());
    b[12..14].copy_from_slice(&vid.to_le_bytes());
    b[14..16].copy_from_slice(&pid.to_le_bytes());
    b
}

/// `VIGEM_UNPLUG_TARGET` (8 bytes).
pub fn unplug(serial: u32) -> [u8; 8] {
    hdr::<8>(serial)
}

/// `VIGEM_WAIT_DEVICE_READY` (8 bytes).
pub fn wait_ready(serial: u32) -> [u8; 8] {
    hdr::<8>(serial)
}

/// `VIGEM_CHECK_VERSION` (8 bytes): Size, Version.
pub fn check_version() -> [u8; 8] {
    let mut b = [0u8; 8];
    b[0..4].copy_from_slice(&8u32.to_le_bytes());
    b[4..8].copy_from_slice(&COMMON_VERSION.to_le_bytes());
    b
}

/// `XUSB_SUBMIT_REPORT` (20 bytes: header + XUSB_REPORT 12).
pub fn xusb_submit(serial: u32, r: &XusbReport) -> [u8; 20] {
    let mut b = hdr::<20>(serial);
    b[8..10].copy_from_slice(&r.buttons.to_le_bytes());
    b[10] = r.left_trigger;
    b[11] = r.right_trigger;
    b[12..14].copy_from_slice(&r.lx.to_le_bytes());
    b[14..16].copy_from_slice(&r.ly.to_le_bytes());
    b[16..18].copy_from_slice(&r.rx.to_le_bytes());
    b[18..20].copy_from_slice(&r.ry.to_le_bytes());
    b
}

/// `XUSB_REQUEST_NOTIFICATION` (12 bytes: header, LargeMotor, SmallMotor, LedNumber, pad).
pub fn xusb_notification(serial: u32) -> [u8; 12] {
    hdr::<12>(serial)
}

/// Decoded notification: (large motor, small motor, led number).
pub fn decode_xusb_notification(b: &[u8]) -> Option<(u8, u8, u8)> {
    (b.len() >= 11).then(|| (b[8], b[9], b[10]))
}

/// `DS4_SUBMIT_REPORT_EX` (packed, 71 bytes): header + the 63 bytes after the report id
/// of a DS4 USB input report.
pub fn ds4_submit_ex(serial: u32, usb_report: &[u8]) -> Option<[u8; 71]> {
    if usb_report.len() < 64 || usb_report[0] != 0x01 {
        return None;
    }
    let mut b = hdr::<71>(serial);
    b[8..71].copy_from_slice(&usb_report[1..64]);
    Some(b)
}

/// `DS4_REQUEST_NOTIFICATION` (16 bytes: header, SmallMotor, LargeMotor, R, G, B, pad).
pub fn ds4_notification(serial: u32) -> [u8; 16] {
    hdr::<16>(serial)
}

/// (small motor, large motor, (r, g, b)).
pub fn decode_ds4_notification(b: &[u8]) -> Option<(u8, u8, (u8, u8, u8))> {
    (b.len() >= 13).then(|| (b[8], b[9], (b[10], b[11], b[12])))
}

/// `DS4_AWAIT_OUTPUT` (packed, 72 bytes: header + 64-byte raw output buffer). The bus
/// fills `SerialNo` with the target that produced the output.
pub fn ds4_await_output(serial: u32) -> [u8; 72] {
    hdr::<72>(serial)
}

pub fn decode_ds4_await_output(b: &[u8]) -> Option<(u32, [u8; 64])> {
    if b.len() < 72 {
        return None;
    }
    let serial = u32::from_le_bytes([b[4], b[5], b[6], b[7]]);
    let mut out = [0u8; 64];
    out.copy_from_slice(&b[8..72]);
    Some((serial, out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ioctl_codes() {
        // CTL_CODE(0x2A, 0x801, 0, FILE_WRITE_DATA) = 0x2AA004
        assert_eq!(IOCTL_PLUGIN_TARGET, 0x002A_A004);
        assert_eq!(IOCTL_UNPLUG_TARGET, 0x002A_A008);
        assert_eq!(IOCTL_CHECK_VERSION, 0x002A_A00C);
        assert_eq!(IOCTL_WAIT_DEVICE_READY, 0x002A_A010);
        assert_eq!(IOCTL_XUSB_REQUEST_NOTIFICATION, 0x002A_E804);
        assert_eq!(IOCTL_XUSB_SUBMIT_REPORT, 0x002A_A808);
        assert_eq!(IOCTL_DS4_AWAIT_OUTPUT_AVAILABLE, 0x002A_E820);
    }

    #[test]
    fn struct_encoding() {
        let p = plugin(3, TargetType::DualShock4Wired, 0, 0);
        assert_eq!(&p[..12], &[16, 0, 0, 0, 3, 0, 0, 0, 2, 0, 0, 0]);
        let x = xusb_submit(1, &XusbReport { buttons: 0x1001, left_trigger: 5, right_trigger: 6, lx: -1, ly: 2, rx: 0, ry: 0 });
        assert_eq!(&x[..12], &[20, 0, 0, 0, 1, 0, 0, 0, 0x01, 0x10, 5, 6]);
        assert_eq!(&x[12..16], &[0xFF, 0xFF, 2, 0]);
        let mut rep = [0u8; 64];
        rep[0] = 1;
        rep[1] = 0x80;
        let d = ds4_submit_ex(9, &rep).unwrap();
        assert_eq!(u32::from_le_bytes([d[0], d[1], d[2], d[3]]), 71);
        assert_eq!(d[8], 0x80);
        assert!(ds4_submit_ex(9, &rep[..10]).is_none());
        assert_eq!(decode_xusb_notification(&[12, 0, 0, 0, 1, 0, 0, 0, 200, 100, 1, 0]), Some((200, 100, 1)));
        let mut a = ds4_await_output(0);
        a[4] = 7;
        a[8] = 0x05;
        let (s, o) = decode_ds4_await_output(&a).unwrap();
        assert_eq!((s, o[0]), (7, 0x05));
        assert_eq!(check_version(), [8, 0, 0, 0, 1, 0, 0, 0]);
    }
}
