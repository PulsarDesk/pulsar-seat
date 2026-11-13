// SPDX-License-Identifier: GPL-3.0-only
//! USB descriptors of the emulated DualSense composite device, written for Pulsar Split
//! from the USB 2.0, USB Audio Class 1.0 and HID 1.11 specifications. Interop
//! identifiers only (VID/PID, manufacturer/product strings); layout is ours.
//!
//! Full composite (console session): IF0 Audio Control, IF1 speaker streaming
//! (4 ch, 48 kHz, 16 bit, iso OUT EP1), IF2 microphone streaming (2 ch, 48 kHz, 16 bit,
//! iso IN EP2), IF3 HID (interrupt IN EP4, interrupt OUT EP3).
//! HID only (remote seat sessions, where USB audio endpoints are not usable): IF0 HID.

use crate::descriptors::{hid_descriptor, report_descriptor};
use crate::types::PadKind;

pub const EP_SPEAKER: u8 = 0x01;
pub const EP_MIC: u8 = 0x82;
pub const EP_HID_IN: u8 = 0x84;
pub const EP_HID_OUT: u8 = 0x03;
pub const HID_PACKET: u16 = 64;
pub const SPEAKER_CHANNELS: u8 = 4;
pub const MIC_CHANNELS: u8 = 2;
pub const SAMPLE_RATE: u32 = 48_000;
/// Max packet: 49 frames per 1 ms (adaptive margin).
pub const SPEAKER_PACKET: u16 = 4 * 2 * 49;
pub const MIC_PACKET: u16 = 2 * 2 * 49;
/// Bytes of one millisecond of microphone audio (48 frames).
pub const MIC_BYTES_PER_MS: usize = 2 * 2 * 48;
pub const HID_INTERVAL_MS: u8 = 4;

pub const MANUFACTURER: &str = "Sony Interactive Entertainment";
pub const PRODUCT_DS5: &str = "DualSense Wireless Controller";
pub const PRODUCT_DS5E: &str = "DualSense Edge Wireless Controller";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub audio: bool,
    pub hid_interface: u8,
    pub ac_interface: Option<u8>,
    pub speaker_interface: Option<u8>,
    pub mic_interface: Option<u8>,
}

impl Layout {
    pub fn new(audio: bool) -> Self {
        if audio {
            Layout { audio, hid_interface: 3, ac_interface: Some(0), speaker_interface: Some(1), mic_interface: Some(2) }
        } else {
            Layout { audio, hid_interface: 0, ac_interface: None, speaker_interface: None, mic_interface: None }
        }
    }
    pub fn num_interfaces(&self) -> u8 {
        if self.audio { 4 } else { 1 }
    }
    /// (class, subclass, protocol) per interface for the USB/IP device record.
    pub fn interface_classes(&self) -> Vec<(u8, u8, u8)> {
        if self.audio { vec![(1, 1, 0), (1, 2, 0), (1, 2, 0), (3, 0, 0)] } else { vec![(3, 0, 0)] }
    }
}

pub fn device_descriptor(kind: PadKind) -> [u8; 18] {
    let (vid, pid) = kind.usb_ids();
    [
        18,
        0x01,
        0x00,
        0x02, // bcdUSB 2.00
        0x00,
        0x00,
        0x00, // class per interface
        64,   // bMaxPacketSize0
        (vid & 0xFF) as u8,
        (vid >> 8) as u8,
        (pid & 0xFF) as u8,
        (pid >> 8) as u8,
        0x00,
        0x01, // bcdDevice 1.00
        1,    // iManufacturer
        2,    // iProduct
        3,    // iSerialNumber
        1,    // bNumConfigurations
    ]
}

fn le16(v: u16) -> [u8; 2] {
    v.to_le_bytes()
}

fn audio_control(out: &mut Vec<u8>) {
    // IF0 Audio Control
    out.extend([9, 0x04, 0, 0, 0, 0x01, 0x01, 0x00, 0]);
    let mut cs = Vec::new();
    // Input terminal 1: USB streaming, 4 channels (L, R, Ls, Rs)
    cs.extend([12, 0x24, 0x02, 1, 0x01, 0x01, 0, SPEAKER_CHANNELS, 0x33, 0x00, 0, 0]);
    // Output terminal 3: speaker, source 1
    cs.extend([9, 0x24, 0x03, 3, 0x01, 0x03, 0, 1, 0]);
    // Input terminal 4: microphone, 2 channels (L, R)
    cs.extend([12, 0x24, 0x02, 4, 0x01, 0x02, 0, MIC_CHANNELS, 0x03, 0x00, 0, 0]);
    // Output terminal 5: USB streaming, source 4
    cs.extend([9, 0x24, 0x03, 5, 0x01, 0x01, 0, 4, 0]);
    let total = 10 + cs.len() as u16;
    out.extend([10, 0x24, 0x01, 0x00, 0x01]);
    out.extend(le16(total));
    out.extend([2, 1, 2]); // two streaming interfaces: 1, 2
    out.extend(cs);
}

fn streaming(out: &mut Vec<u8>, iface: u8, terminal: u8, channels: u8, ep: u8, attrs: u8, maxp: u16) {
    out.extend([9, 0x04, iface, 0, 0, 0x01, 0x02, 0x00, 0]); // alt 0, zero bandwidth
    out.extend([9, 0x04, iface, 1, 1, 0x01, 0x02, 0x00, 0]); // alt 1
    out.extend([7, 0x24, 0x01, terminal, 1, 0x01, 0x00]); // AS general, PCM
    let sr = SAMPLE_RATE.to_le_bytes();
    out.extend([11, 0x24, 0x02, 0x01, channels, 2, 16, 1, sr[0], sr[1], sr[2]]); // type I
    let mp = le16(maxp);
    out.extend([9, 0x05, ep, attrs, mp[0], mp[1], 1, 0, 0]); // iso endpoint (audio 9-byte form)
    out.extend([7, 0x25, 0x01, 0x01, 0, 0, 0]); // CS endpoint: sampling frequency control
}

fn hid_interface(out: &mut Vec<u8>, kind: PadKind, iface: u8) {
    let rd = report_descriptor(kind.kind_value());
    out.extend([9, 0x04, iface, 0, 2, 0x03, 0x00, 0x00, 0]);
    out.extend(hid_descriptor(0x0111, rd.len() as u16));
    let mp = le16(HID_PACKET);
    out.extend([7, 0x05, EP_HID_IN, 0x03, mp[0], mp[1], HID_INTERVAL_MS]);
    out.extend([7, 0x05, EP_HID_OUT, 0x03, mp[0], mp[1], HID_INTERVAL_MS]);
}

/// Full configuration descriptor (with all interfaces and endpoints).
pub fn configuration_descriptor(kind: PadKind, layout: &Layout) -> Vec<u8> {
    let mut body = Vec::new();
    if layout.audio {
        audio_control(&mut body);
        streaming(&mut body, 1, 1, SPEAKER_CHANNELS, EP_SPEAKER, 0x09, SPEAKER_PACKET); // adaptive OUT
        streaming(&mut body, 2, 5, MIC_CHANNELS, EP_MIC, 0x05, MIC_PACKET); // async IN
    }
    hid_interface(&mut body, kind, layout.hid_interface);
    let total = 9 + body.len() as u16;
    let t = le16(total);
    let mut out = vec![9, 0x02, t[0], t[1], layout.num_interfaces(), 1, 0, 0x80, 250];
    out.extend(body);
    out
}

/// String descriptor `index` (0 = language list).
pub fn string_descriptor(index: u8, kind: PadKind, serial: &str) -> Option<Vec<u8>> {
    let s = match index {
        0 => return Some(vec![4, 0x03, 0x09, 0x04]),
        1 => MANUFACTURER,
        2 if kind == PadKind::Ds5Edge => PRODUCT_DS5E,
        2 => PRODUCT_DS5,
        3 => serial,
        _ => return None,
    };
    let units: Vec<u16> = s.encode_utf16().take(126).collect();
    let mut v = vec![(2 + units.len() * 2) as u8, 0x03];
    for u in units {
        v.extend(u.to_le_bytes());
    }
    Some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Walks a configuration descriptor and checks every length byte.
    fn walk(d: &[u8]) -> Vec<(u8, u8)> {
        let mut i = 0;
        let mut v = Vec::new();
        while i < d.len() {
            let len = d[i] as usize;
            assert!(len >= 2 && i + len <= d.len(), "bad descriptor length at {i}");
            v.push((d[i + 1], if d.len() > i + 2 { d[i + 2] } else { 0 }));
            i += len;
        }
        v
    }

    #[test]
    fn full_composite() {
        let l = Layout::new(true);
        let c = configuration_descriptor(PadKind::Ds5, &l);
        assert_eq!(u16::from_le_bytes([c[2], c[3]]) as usize, c.len());
        assert_eq!(c[4], 4);
        let items = walk(&c);
        let ifaces = items.iter().filter(|(t, _)| *t == 0x04).count();
        assert_eq!(ifaces, 1 + 2 + 2 + 1);
        let eps: Vec<u8> = items.iter().filter(|(t, _)| *t == 0x05).map(|(_, a)| *a).collect();
        assert_eq!(eps, vec![EP_SPEAKER, EP_MIC, EP_HID_IN, EP_HID_OUT]);
        // AC header total length covers its class-specific descriptors
        let ac_total = u16::from_le_bytes([c[9 + 9 + 5], c[9 + 9 + 6]]);
        assert_eq!(ac_total, 10 + 12 + 9 + 12 + 9);
    }

    #[test]
    fn hid_only() {
        let l = Layout::new(false);
        let c = configuration_descriptor(PadKind::Ds5Edge, &l);
        assert_eq!(c[4], 1);
        assert_eq!(c.len(), 9 + 9 + 9 + 7 + 7);
        assert_eq!(c[9 + 2], 0, "HID is interface 0");
        let rd_len = u16::from_le_bytes([c[9 + 9 + 7], c[9 + 9 + 8]]) as usize;
        assert_eq!(rd_len, crate::descriptors::DS5E_REPORT_DESCRIPTOR.len());
    }

    #[test]
    fn device_and_strings() {
        let d = device_descriptor(PadKind::Ds5Edge);
        assert_eq!(&d[8..12], &[0x4C, 0x05, 0xF2, 0x0D]);
        let s = string_descriptor(3, PadKind::Ds5, "PSPAD01").unwrap();
        assert_eq!(s[0] as usize, s.len());
        assert_eq!(s[2], b'P');
        assert_eq!(string_descriptor(0, PadKind::Ds5, "").unwrap(), vec![4, 3, 9, 4]);
        assert!(string_descriptor(9, PadKind::Ds5, "").is_none());
    }
}
