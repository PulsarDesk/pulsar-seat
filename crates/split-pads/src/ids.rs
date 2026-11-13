// SPDX-License-Identifier: GPL-3.0-only
//! Identifiers and markers: seat32, device-id parts, hardware ids, channel names, USB
//! serial marker, device SDDL, per-pad MAC (spec 60 §3.6, §6.4, §8.2, §9; contracts §5.3).

use sha2::{Digest, Sha256};

use crate::error::PadError;
use crate::layout;
use crate::types::{PadId, PadKind};

/// Seat GUID as 32 upper-case hex digits (no braces, no dashes).
pub fn seat32(seat_id: &str) -> Result<String, PadError> {
    let s: String = seat_id
        .trim()
        .trim_start_matches('{')
        .trim_end_matches('}')
        .chars()
        .filter(|c| *c != '-')
        .collect();
    if s.len() != 32 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(PadError::Invalid(format!("seat id is not a GUID: {seat_id:?}")));
    }
    Ok(s.to_ascii_uppercase())
}

/// Seat GUID bytes in Windows `GUID` memory order (for `DEVPROP_TYPE_GUID`).
pub fn seat_guid_bytes(seat32: &str) -> Result<[u8; 16], PadError> {
    let s = self::seat32(seat32)?;
    let hex = |i: usize| u8::from_str_radix(&s[i..i + 2], 16).unwrap_or(0);
    let mut raw = [0u8; 16];
    for (k, b) in raw.iter_mut().enumerate() {
        *b = hex(k * 2);
    }
    // data1 (LE u32), data2 (LE u16), data3 (LE u16), data4 as is
    Ok([
        raw[3], raw[2], raw[1], raw[0], raw[5], raw[4], raw[7], raw[6], raw[8], raw[9], raw[10], raw[11], raw[12],
        raw[13], raw[14], raw[15],
    ])
}

/// Device-id part `VID_<vid>&PID_<pid>[&IG_00]&PULSARPAD&<seat32>&<slot>` (spec 60 §3.6).
pub fn device_id_part(kind: PadKind, pad: &PadId) -> String {
    let (vid, pid) = kind.usb_ids();
    let ig = if matches!(kind, PadKind::XOne | PadKind::X360) { "&IG_00" } else { "" };
    format!("VID_{vid:04X}&PID_{pid:04X}{ig}{}{}&{:02X}", layout::MARKER_TOKEN, pad.seat32, pad.slot)
}

/// Hardware ids, most specific first; no compatible ids.
pub fn hardware_ids(kind: PadKind) -> Vec<String> {
    let (vid, pid) = kind.usb_ids();
    let ig = if matches!(kind, PadKind::XOne | PadKind::X360) { "&IG_00" } else { "" };
    vec![format!("PULSARPAD\\VID_{vid:04X}&PID_{pid:04X}{ig}"), format!("PULSARPAD\\{}", kind.wire_name())]
}

/// Software-device instance id (method A): `SWD\PulsarSplitPad\<device-id part>`.
pub fn sw_instance_id(kind: PadKind, pad: &PadId) -> String {
    format!("SWD\\{}\\{}", layout::SW_ENUMERATOR, device_id_part(kind, pad))
}

/// Channel base name `Global\PulsarSplit_Pad_<seat32>_<slot>` (spec 60 §4.5).
pub fn channel_base(pad: &PadId) -> String {
    format!("{}{}_{:02X}", layout::CHANNEL_PREFIX, pad.seat32, pad.slot)
}

/// Channel base of one pad *instance*: `<channel_base>_<nonce 8 hex>`. A replaced pad gets
/// a new name, so objects still held open by the stream server or by threads of the old
/// pad never collide with the new ones (spec 60 §4.5 names only the fixed part; pads-8).
pub fn channel_name(pad: &PadId, nonce: u32) -> String {
    format!("{}_{nonce:08X}", channel_base(pad))
}

/// USB serial marker `PSPAD<seat32><slot 2 hex>` (contracts §5.3).
pub fn marker_serial(pad: &PadId) -> String {
    format!("PSPAD{}{:02X}", pad.seat32, pad.slot)
}

/// USB device instance id Windows derives from a unique serial (`HYPOTHESIS`: holds while
/// usbip-win2 reports the serial; 0.9.8.1 has a known serial regression).
pub fn usb_instance_id(kind: PadKind, pad: &PadId) -> String {
    let (vid, pid) = kind.usb_ids();
    format!("USB\\VID_{vid:04X}&PID_{pid:04X}\\{}", marker_serial(pad))
}

/// Which of our markers an instance id carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkerMatch {
    pub seat32: String,
    pub slot: u8,
}

fn parse_seat_slot(seat: &str, slot: &str) -> Option<MarkerMatch> {
    if seat.len() != 32 || !seat.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    if slot.len() != 2 {
        return None;
    }
    let slot = u8::from_str_radix(slot, 16).ok()?;
    Some(MarkerMatch { seat32: seat.to_ascii_uppercase(), slot })
}

/// Recognises only our exact markers: `&PULSARPAD&<seat32>&<slot>` anywhere in the id
/// (root, software device and HID children), or an instance segment that is exactly
/// `PSPAD<seat32><slot>` (USB/IP DualSense). Never matches other products' tokens.
pub fn marker_of(instance_id: &str) -> Option<MarkerMatch> {
    let up = instance_id.to_ascii_uppercase();
    if let Some(pos) = up.find(layout::MARKER_TOKEN) {
        let rest = &up[pos + layout::MARKER_TOKEN.len()..];
        if rest.len() >= 35 && rest.as_bytes()[32] == b'&' {
            return parse_seat_slot(&rest[..32], &rest[33..35]);
        }
        return None;
    }
    for seg in up.split('\\') {
        if seg.len() == 39 && seg.starts_with("PSPAD") {
            return parse_seat_slot(&seg[5..37], &seg[37..39]);
        }
    }
    None
}

/// Prefix of every instance id that belongs to a pad (for the shim seat map).
pub fn instance_prefixes(kind: PadKind, pad: &PadId, backend_instance: Option<&str>) -> Vec<String> {
    let mut v = Vec::new();
    if let Some(i) = backend_instance {
        v.push(i.to_ascii_uppercase());
    }
    let marker = format!("{}{}&{:02X}", layout::MARKER_TOKEN, pad.seat32, pad.slot);
    v.push(marker);
    let usb = usb_instance_id(kind, pad).to_ascii_uppercase();
    if !v.contains(&usb) && kind.is_playstation() {
        v.push(usb);
    }
    v
}

/// Validates a string SID (`S-1-…`) so it can be placed into SDDL safely.
pub fn valid_sid(sid: &str) -> bool {
    let mut parts = sid.split('-');
    parts.next() == Some("S")
        && parts.next() == Some("1")
        && sid.len() <= 184
        && {
            let rest: Vec<&str> = parts.collect();
            rest.len() >= 2 && rest.iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        }
}

/// Device security for a pad's devices: SYSTEM and Administrators full, the owning seat
/// user read + write (spec 60 §8.2). Panics never; an invalid SID yields SY/BA only.
pub fn pad_sddl(owner_sid: &str) -> String {
    if valid_sid(owner_sid) {
        format!("D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;{owner_sid})")
    } else {
        log::warn!("pad_sddl: invalid owner SID, granting SYSTEM/Administrators only");
        "D:P(A;;GA;;;SY)(A;;GA;;;BA)".to_string()
    }
}

/// Per-pad MAC: SHA-256(seat32 || slot || host salt), first 6 bytes; locally
/// administered bit set, multicast bit cleared (spec 60 §6.4).
pub fn mac_for(seat32: &str, slot: u8, host_salt: &[u8; 16]) -> [u8; 6] {
    let mut h = Sha256::new();
    h.update(seat32.to_ascii_uppercase().as_bytes());
    h.update([slot]);
    h.update(host_salt);
    let d = h.finalize();
    let mut mac = [d[0], d[1], d[2], d[3], d[4], d[5]];
    mac[0] = (mac[0] | 0x02) & !0x01;
    mac
}

/// MAC as 12 upper-case hex digits (HID serial string of PulsarPad devices).
pub fn mac_hex(mac: &[u8; 6]) -> String {
    mac.iter().map(|b| format!("{b:02X}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEAT: &str = "0a1b2c3d-0000-4000-8000-00000000abcd";

    fn pad(slot: u8) -> PadId {
        PadId::new(SEAT, slot).unwrap()
    }

    #[test]
    fn seat32_forms() {
        assert_eq!(seat32(SEAT).unwrap(), "0A1B2C3D00004000800000000000ABCD");
        assert_eq!(seat32("{0A1B2C3D-0000-4000-8000-00000000ABCD}").unwrap(), "0A1B2C3D00004000800000000000ABCD");
        assert!(seat32("seat-a").is_err());
        assert!(seat32("0a1b2c3d-0000-4000-8000-00000000abcz").is_err());
    }

    #[test]
    fn guid_bytes_order() {
        let b = seat_guid_bytes(SEAT).unwrap();
        assert_eq!(&b[..4], &[0x3D, 0x2C, 0x1B, 0x0A]);
        assert_eq!(&b[8..], &[0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0xAB, 0xCD]);
    }

    #[test]
    fn device_ids() {
        let p = pad(1);
        assert_eq!(
            device_id_part(PadKind::Ds5, &p),
            "VID_054C&PID_0CE6&PULSARPAD&0A1B2C3D00004000800000000000ABCD&01"
        );
        let x = device_id_part(PadKind::XOne, &p);
        assert!(x.starts_with("VID_045E&PID_02FF&IG_00&PULSARPAD&"));
        assert!(x.len() <= 70);
        assert_eq!(hardware_ids(PadKind::XOne), vec!["PULSARPAD\\VID_045E&PID_02FF&IG_00", "PULSARPAD\\xone"]);
        assert_eq!(channel_base(&p), "Global\\PulsarSplit_Pad_0A1B2C3D00004000800000000000ABCD_01");
        assert!(channel_base(&p).len() <= layout::CHANNEL_MAX_LEN);
        assert_eq!(channel_name(&p, 0xAB), "Global\\PulsarSplit_Pad_0A1B2C3D00004000800000000000ABCD_01_000000AB");
        assert!(channel_name(&p, u32::MAX).len() + 7 <= layout::CHANNEL_MAX_LEN);
        assert_eq!(marker_serial(&p), "PSPAD0A1B2C3D00004000800000000000ABCD01");
    }

    #[test]
    fn markers_only_ours() {
        let p = pad(0x0F);
        let sw = sw_instance_id(PadKind::Ds4, &p);
        assert_eq!(marker_of(&sw).unwrap(), MarkerMatch { seat32: p.seat32.clone(), slot: 0x0F });
        let hid = format!("HID\\{}\\1&2d595ca7&0&0000", device_id_part(PadKind::Ds4, &p));
        assert_eq!(marker_of(&hid).unwrap().slot, 0x0F);
        let usb = usb_instance_id(PadKind::Ds5, &p);
        assert_eq!(marker_of(&usb).unwrap().slot, 0x0F);
        // lower case too
        assert!(marker_of(&usb.to_lowercase()).is_some());
        // never another product's token
        assert_eq!(marker_of("ROOT\\VID_054C&PID_0CE6&DUOCONTROLLER&00000000000000000000205C00000007\\0000"), None);
        assert_eq!(marker_of("USB\\VID_054C&PID_0CE6\\5&1234&0&2"), None);
        assert_eq!(marker_of("HID\\X&PULSARPAD&SHORT"), None);
        assert_eq!(marker_of("USB\\VID_1\\PSPADXX1B2C3D00004000800000000000ABCD01"), None);
    }

    #[test]
    fn sddl() {
        assert_eq!(pad_sddl("S-1-5-21-1-2-3-1001"), "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;S-1-5-21-1-2-3-1001)");
        assert_eq!(pad_sddl("S-1-5-21-1)(A;;GA;;;WD"), "D:P(A;;GA;;;SY)(A;;GA;;;BA)");
        assert!(!valid_sid("S-1"));
        assert!(valid_sid("S-1-5-18"));
    }

    #[test]
    fn mac_bits_and_stability() {
        let salt = [7u8; 16];
        let a = mac_for("0A1B2C3D00004000800000000000ABCD", 0, &salt);
        let b = mac_for("0a1b2c3d00004000800000000000abcd", 0, &salt);
        let c = mac_for("0A1B2C3D00004000800000000000ABCD", 1, &salt);
        let d = mac_for("0A1B2C3D00004000800000000000ABCD", 0, &[8u8; 16]);
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_ne!(a, d);
        for m in [a, c, d] {
            assert_eq!(m[0] & 0x02, 0x02, "locally administered");
            assert_eq!(m[0] & 0x01, 0, "unicast");
        }
        assert_eq!(mac_hex(&[0x02, 0xAB, 0, 1, 2, 3]), "02AB00010203");
    }

    #[test]
    fn prefixes() {
        let p = pad(2);
        let v = instance_prefixes(PadKind::X360, &p, Some("usb\\vid_045e&pid_028e\\1&2&3"));
        assert_eq!(v[0], "USB\\VID_045E&PID_028E\\1&2&3");
        assert!(v[1].starts_with("&PULSARPAD&"));
        assert_eq!(v.len(), 2);
        assert_eq!(instance_prefixes(PadKind::Ds5, &p, None).len(), 2);
    }
}
