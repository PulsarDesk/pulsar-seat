// SPDX-License-Identifier: GPL-3.0-only
//! Control-device contract of our KMDF input filter (`drivers/seat-input-filter`).
//!
//! Mirror of `drivers/seat-input-filter/include/seatinput_ioctl.h`; the layout tests below
//! pin every size and offset. Little-endian, natural alignment, no padding surprises
//! (every struct is a multiple of 8 bytes). Changes inside interface major 1 are additive
//! (new IOCTLs, new trailing fields guarded by `size`).

use seat_core::ids::SeatMask;

/// `\\.\PulsarSeatInput` (kernel `\DosDevices\PulsarSeatInput`), SYSTEM-only DACL,
/// exclusive open: only the service talks to the filter.
pub const CONTROL_DEVICE_PATH: &str = r"\\.\PulsarSeatInput";

pub const INTERFACE_MAJOR: u16 = seat_core::consts::DRIVER_INTERFACE_MAJOR;
/// 1 (2026-11-13): `SEATINPUT_CONFIG.EmergencyScan`. A minor-0 driver reads only the first
/// [`CONFIG_V0_SIZE`] bytes of the config and keeps using the move hotkey as the
/// emergency key.
pub const INTERFACE_MINOR: u16 = 1;

const FILE_DEVICE_UNKNOWN: u32 = 0x22;
const METHOD_BUFFERED: u32 = 0;
const FILE_ANY_ACCESS: u32 = 0;
const FILE_READ_WRITE_ACCESS: u32 = 0x0001 | 0x0002;

const fn ctl_code(function: u32, access: u32) -> u32 {
    (FILE_DEVICE_UNKNOWN << 16) | (access << 14) | (function << 2) | METHOD_BUFFERED
}

/// Out: [`FilterVersion`]. Always the first call.
pub const IOCTL_GET_VERSION: u32 = ctl_code(0x800, FILE_ANY_ACCESS);
/// In: [`encode_attach`]. Ends fail-open mode for this boot (spec 50 §4.1.5).
pub const IOCTL_ATTACH: u32 = ctl_code(0x801, FILE_READ_WRITE_ACCESS);
/// In: [`FilterConfig`].
pub const IOCTL_SET_CONFIG: u32 = ctl_code(0x802, FILE_READ_WRITE_ACCESS);
/// In: [`encode_unit_mask`] (one unit's seat mask by device instance id).
pub const IOCTL_SET_UNIT: u32 = ctl_code(0x803, FILE_READ_WRITE_ACCESS);
/// In: [`encode_sessions`] (session id per seat, for HID create decisions).
pub const IOCTL_SET_SESSIONS: u32 = ctl_code(0x804, FILE_READ_WRITE_ACCESS);
/// Out: [`FilterPacket`] array. Inverted call: pended until packets exist.
pub const IOCTL_READ_PACKETS: u32 = ctl_code(0x805, FILE_READ_WRITE_ACCESS);
/// Out: list header + [`FilterUnit`] entries.
pub const IOCTL_LIST_UNITS: u32 = ctl_code(0x806, FILE_READ_WRITE_ACCESS);

/// Version flags.
pub mod version_flags {
    /// Safe Mode or the recovery value: the filter passes everything.
    pub const PASS_THROUGH: u32 = 0x1;
    /// A service attached since boot (fail-open is over).
    pub const ATTACHED_ONCE: u32 = 0x2;
    /// A service handle is open right now.
    pub const ATTACHED: u32 = 0x4;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FilterVersion {
    pub major: u16,
    pub minor: u16,
    pub build: u32,
    pub flags: u32,
}

pub const VERSION_SIZE: usize = 16;

impl FilterVersion {
    pub fn decode(b: &[u8]) -> Option<Self> {
        if b.len() < VERSION_SIZE {
            return None;
        }
        Some(Self { major: u16le(b, 0), minor: u16le(b, 2), build: u32le(b, 4), flags: u32le(b, 8) })
    }
    pub fn encode(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(VERSION_SIZE);
        v.extend(self.major.to_le_bytes());
        v.extend(self.minor.to_le_bytes());
        v.extend(self.build.to_le_bytes());
        v.extend(self.flags.to_le_bytes());
        v.extend(0u32.to_le_bytes());
        v
    }
}

pub const ATTACH_SIZE: usize = 8;

pub fn encode_attach() -> Vec<u8> {
    let mut v = Vec::with_capacity(ATTACH_SIZE);
    v.extend((ATTACH_SIZE as u32).to_le_bytes());
    v.extend(INTERFACE_MAJOR.to_le_bytes());
    v.extend(INTERFACE_MINOR.to_le_bytes());
    v
}

/// Config flags.
pub mod config_flags {
    pub const WIZARD_CAPTURE: u32 = 0x1;
    pub const ALLOW_SYSTEM_BUTTONS: u32 = 0x2;
    pub const HOTKEY_ENABLED: u32 = 0x4;
}

pub const MAX_UNFILTERED: usize = 16;
pub const CONFIG_SIZE: usize = 96;
/// Size of a minor-0 config (no emergency fields).
pub const CONFIG_V0_SIZE: usize = 88;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FilterConfig {
    pub flags: u32,
    pub hotkey_scan: u16,
    /// Bit 1 (`0x2`) = E0.
    pub hotkey_flags: u16,
    pub gap_ms: u32,
    /// Service-gap buffer size **per unit** while detached (the driver keeps a per-unit
    /// budget inside its ring, spec 50 §4.1.5 / §4.3).
    pub gap_packets: u32,
    /// Collections the HID role never filters: `(page, usage)`; usage 0 (reserved in every
    /// page) matches the whole page, see [`unfiltered_matches`].
    pub unfiltered: Vec<(u16, u16)>,
    /// Emergency key (Ctrl+Shift+key on a seat-1 keyboard), checked by the driver on its
    /// own: [`crate::convert::Hotkey::emergency_key`], so F12 when the move hotkey is off.
    pub emergency_scan: u16,
    /// Bit 1 (`0x2`) = E0.
    pub emergency_flags: u16,
}

/// The driver's unfiltered-usage rule (`hid.c` `SeatHidUnfiltered`): exact match, or
/// usage 0 as a whole-page wildcard.
pub fn unfiltered_matches(list: &[(u16, u16)], page: u16, usage: u16) -> bool {
    list.iter().any(|&(p, u)| p == page && (u == 0 || u == usage))
}

impl FilterConfig {
    pub fn from_settings(s: &seat_core::config::InputSettings, wizard: bool) -> Self {
        let mut flags = 0;
        if wizard {
            flags |= config_flags::WIZARD_CAPTURE;
        }
        if s.allow_system_buttons {
            flags |= config_flags::ALLOW_SYSTEM_BUTTONS;
        }
        let hk = crate::convert::Hotkey::from_setting(s.switch_hotkey_scan);
        if hk.is_some() {
            flags |= config_flags::HOTKEY_ENABLED;
        }
        // System control (power / sleep) goes first so a full user list can never push it
        // out; the driver also honours ALLOW_SYSTEM_BUTTONS by itself (hid.c SeatHidAllow).
        let mut unfiltered: Vec<(u16, u16)> = Vec::with_capacity(MAX_UNFILTERED + 1);
        if s.allow_system_buttons {
            unfiltered.push((0x01, 0x80));
        }
        for &u in &s.unfiltered_hid_usages {
            if !unfiltered.contains(&u) {
                unfiltered.push(u);
            }
        }
        if unfiltered.len() > MAX_UNFILTERED {
            log::warn!(
                "input: {} unfiltered HID usages configured, the filter driver keeps {MAX_UNFILTERED}; dropped: {:?}",
                unfiltered.len(),
                &unfiltered[MAX_UNFILTERED..]
            );
            unfiltered.truncate(MAX_UNFILTERED);
        }
        // Always sent: turning the move hotkey off never removes the way back to seat 1.
        let em = crate::convert::Hotkey::emergency_key(hk);
        Self {
            flags,
            hotkey_scan: hk.map(|h| h.sc).unwrap_or(0),
            hotkey_flags: if hk.is_some_and(|h| h.e0) { 2 } else { 0 },
            gap_ms: s.gap_buffer_ms,
            gap_packets: s.gap_buffer_packets,
            unfiltered,
            emergency_scan: em.sc,
            emergency_flags: if em.e0 { 2 } else { 0 },
        }
    }
    pub fn encode(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(CONFIG_SIZE);
        v.extend((CONFIG_SIZE as u32).to_le_bytes());
        v.extend(self.flags.to_le_bytes());
        v.extend(self.hotkey_scan.to_le_bytes());
        v.extend(self.hotkey_flags.to_le_bytes());
        v.extend(self.gap_ms.to_le_bytes());
        v.extend(self.gap_packets.to_le_bytes());
        let n = self.unfiltered.len().min(MAX_UNFILTERED);
        v.extend((n as u32).to_le_bytes());
        for i in 0..MAX_UNFILTERED {
            let (p, u) = self.unfiltered.get(i).copied().unwrap_or((0, 0));
            v.extend(p.to_le_bytes());
            v.extend(u.to_le_bytes());
        }
        debug_assert_eq!(v.len(), CONFIG_V0_SIZE);
        v.extend(self.emergency_scan.to_le_bytes());
        v.extend(self.emergency_flags.to_le_bytes());
        v.extend(0u32.to_le_bytes());
        v
    }
}

/// `MAX_DEVICE_ID_LEN` (200 UTF-16 units including the NUL).
pub const MAX_INSTANCE_ID: usize = 200;
pub const UNIT_MASK_SIZE: usize = 16 + MAX_INSTANCE_ID * 2;

pub fn encode_unit_mask(instance_id: &str, mask: SeatMask, revision: u64) -> Option<Vec<u8>> {
    let w: Vec<u16> = instance_id.encode_utf16().collect();
    if w.is_empty() || w.len() >= MAX_INSTANCE_ID {
        return None;
    }
    let mut v = Vec::with_capacity(UNIT_MASK_SIZE);
    v.extend((UNIT_MASK_SIZE as u32).to_le_bytes());
    v.extend(mask.0.to_le_bytes());
    v.extend(revision.to_le_bytes());
    for i in 0..MAX_INSTANCE_ID {
        v.extend(w.get(i).copied().unwrap_or(0).to_le_bytes());
    }
    Some(v)
}

/// Session id value meaning "seat has no session".
pub const NO_SESSION: u32 = u32::MAX;
pub const SESSIONS_SIZE: usize = 8 + 16 * 4;

pub fn encode_sessions(sessions: &[Option<u32>; 16]) -> Vec<u8> {
    let mut v = Vec::with_capacity(SESSIONS_SIZE);
    v.extend((SESSIONS_SIZE as u32).to_le_bytes());
    v.extend(16u32.to_le_bytes());
    for s in sessions {
        v.extend(s.unwrap_or(NO_SESSION).to_le_bytes());
    }
    v
}

/// Packet kinds.
pub mod packet_kind {
    pub const KEYBOARD: u8 = 1;
    pub const MOUSE: u8 = 2;
    /// A seat-1 unit pressed Ctrl + the hotkey (the filter swallowed it). `a` carries
    /// [`hotkey_bits`].
    pub const HOTKEY: u8 = 3;
}

/// `FilterPacket::a` of a [`packet_kind::HOTKEY`] packet.
pub mod hotkey_bits {
    /// Shift was held: an emergency press (Ctrl+Shift+hotkey), never the move hotkey.
    pub const SHIFT: u16 = 0x1;
}

/// Packet flags.
pub mod packet_flags {
    /// Packets of this unit were dropped before this one (service gap, ring full):
    /// synthesize key-ups first.
    pub const GAP: u16 = 0x1;
    /// The unit is a seat-1 unit (copy for the wizard / activity, already delivered).
    pub const CONSOLE_COPY: u16 = 0x2;
    /// Spare unit captured for the wizard (delivered to nobody).
    pub const WIZARD: u16 = 0x4;
}

pub const PACKET_SIZE: usize = 32;

/// One record of [`IOCTL_READ_PACKETS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FilterPacket {
    pub unit_id: u32,
    pub kind: u8,
    /// Owning seat (1-based; 0 = none).
    pub seat: u8,
    pub flags: u16,
    /// `KeQueryInterruptTime` (100 ns).
    pub time: u64,
    /// Keyboard: make code; mouse: `Flags`.
    pub a: u16,
    /// Keyboard: key flags; mouse: `ButtonFlags`.
    pub b: u16,
    /// Mouse: `ButtonData`.
    pub c: u16,
    pub last_x: i32,
    pub last_y: i32,
}

impl FilterPacket {
    pub fn decode(b: &[u8]) -> Option<Self> {
        if b.len() < PACKET_SIZE {
            return None;
        }
        Some(Self {
            unit_id: u32le(b, 0),
            kind: b[4],
            seat: b[5],
            flags: u16le(b, 6),
            time: u64le(b, 8),
            a: u16le(b, 16),
            b: u16le(b, 18),
            c: u16le(b, 20),
            last_x: u32le(b, 24) as i32,
            last_y: u32le(b, 28) as i32,
        })
    }
    pub fn encode(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(PACKET_SIZE);
        v.extend(self.unit_id.to_le_bytes());
        v.push(self.kind);
        v.push(self.seat);
        v.extend(self.flags.to_le_bytes());
        v.extend(self.time.to_le_bytes());
        v.extend(self.a.to_le_bytes());
        v.extend(self.b.to_le_bytes());
        v.extend(self.c.to_le_bytes());
        v.extend(0u16.to_le_bytes());
        v.extend(self.last_x.to_le_bytes());
        v.extend(self.last_y.to_le_bytes());
        v
    }
    pub fn decode_all(b: &[u8]) -> Vec<Self> {
        b.chunks_exact(PACKET_SIZE).filter_map(Self::decode).collect()
    }
    /// As a [`crate::convert::RawUnitEvent`].
    pub fn event(&self) -> Option<crate::convert::RawUnitEvent> {
        use crate::convert::RawUnitEvent;
        match self.kind {
            packet_kind::KEYBOARD => Some(RawUnitEvent::Key { sc: self.a, flags: self.b }),
            packet_kind::MOUSE => Some(RawUnitEvent::Mouse {
                flags: self.a,
                buttons: self.b,
                data: self.c,
                x: self.last_x,
                y: self.last_y,
            }),
            _ => None,
        }
    }
}

/// Unit roles.
pub mod unit_role {
    pub const KEYBOARD: u32 = 1;
    pub const MOUSE: u32 = 2;
    pub const HID: u32 = 3;
    /// Excluded (remote-desktop bus, root-enumerated): always passes.
    pub const EXCLUDED: u32 = 4;
}

pub const UNIT_LIST_HEADER: usize = 16;
pub const UNIT_SIZE: usize = 32 + MAX_INSTANCE_ID * 2;

/// One entry of [`IOCTL_LIST_UNITS`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FilterUnit {
    pub unit_id: u32,
    pub role: u32,
    pub mask: u32,
    pub packets: u64,
    pub last_activity: u64,
    pub instance_id: String,
}

impl FilterUnit {
    pub fn decode(b: &[u8]) -> Option<Self> {
        if b.len() < UNIT_SIZE {
            return None;
        }
        let w: Vec<u16> = (0..MAX_INSTANCE_ID).map(|i| u16le(b, 32 + i * 2)).take_while(|c| *c != 0).collect();
        Some(Self {
            unit_id: u32le(b, 0),
            role: u32le(b, 4),
            mask: u32le(b, 8),
            packets: u64le(b, 16),
            last_activity: u64le(b, 24),
            instance_id: String::from_utf16_lossy(&w),
        })
    }
    pub fn encode(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(UNIT_SIZE);
        v.extend(self.unit_id.to_le_bytes());
        v.extend(self.role.to_le_bytes());
        v.extend(self.mask.to_le_bytes());
        v.extend(0u32.to_le_bytes());
        v.extend(self.packets.to_le_bytes());
        v.extend(self.last_activity.to_le_bytes());
        let w: Vec<u16> = self.instance_id.encode_utf16().collect();
        for i in 0..MAX_INSTANCE_ID {
            v.extend(w.get(i).copied().filter(|_| i < MAX_INSTANCE_ID - 1).unwrap_or(0).to_le_bytes());
        }
        v
    }
    /// Header (`size`, `count`, `total`, reserved) + entries.
    pub fn decode_list(b: &[u8]) -> Vec<Self> {
        if b.len() < UNIT_LIST_HEADER {
            return Vec::new();
        }
        let count = u32le(b, 4) as usize;
        b[UNIT_LIST_HEADER..].chunks_exact(UNIT_SIZE).take(count).filter_map(Self::decode).collect()
    }
    pub fn encode_list(units: &[Self]) -> Vec<u8> {
        let mut v = Vec::new();
        let size = UNIT_LIST_HEADER + units.len() * UNIT_SIZE;
        v.extend((size as u32).to_le_bytes());
        v.extend((units.len() as u32).to_le_bytes());
        v.extend((units.len() as u32).to_le_bytes());
        v.extend(0u32.to_le_bytes());
        for u in units {
            v.extend(u.encode());
        }
        v
    }
}

fn u16le(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn u64le(b: &[u8], o: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[o..o + 8]);
    u64::from_le_bytes(a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ioctl_codes_match_header() {
        // CTL_CODE(FILE_DEVICE_UNKNOWN, 0x800, METHOD_BUFFERED, FILE_ANY_ACCESS)
        assert_eq!(IOCTL_GET_VERSION, 0x0022_2000);
        assert_eq!(IOCTL_ATTACH, 0x0022_E004);
        assert_eq!(IOCTL_SET_CONFIG, 0x0022_E008);
        assert_eq!(IOCTL_SET_UNIT, 0x0022_E00C);
        assert_eq!(IOCTL_SET_SESSIONS, 0x0022_E010);
        assert_eq!(IOCTL_READ_PACKETS, 0x0022_E014);
        assert_eq!(IOCTL_LIST_UNITS, 0x0022_E018);
    }

    #[test]
    fn sizes() {
        assert_eq!(FilterVersion::default().encode().len(), VERSION_SIZE);
        assert_eq!(encode_attach().len(), ATTACH_SIZE);
        assert_eq!(FilterConfig::default().encode().len(), CONFIG_SIZE);
        assert_eq!(encode_unit_mask("HID\\X\\1", SeatMask(2), 7).unwrap().len(), UNIT_MASK_SIZE);
        assert_eq!(UNIT_MASK_SIZE, 416);
        assert_eq!(encode_sessions(&[None; 16]).len(), SESSIONS_SIZE);
        assert_eq!(FilterPacket::default().encode().len(), PACKET_SIZE);
        assert_eq!(FilterUnit::default().encode().len(), UNIT_SIZE);
        assert_eq!(UNIT_SIZE, 432);
    }

    #[test]
    fn roundtrips() {
        let v = FilterVersion { major: 1, minor: 2, build: 77, flags: 3 };
        assert_eq!(FilterVersion::decode(&v.encode()), Some(v));
        let p = FilterPacket { unit_id: 5, kind: 2, seat: 3, flags: 1, time: 99, a: 1, b: 0x400, c: 0xFF88, last_x: -4, last_y: 9 };
        assert_eq!(FilterPacket::decode(&p.encode()), Some(p));
        let mut two = p.encode();
        two.extend(p.encode());
        assert_eq!(FilterPacket::decode_all(&two).len(), 2);
        let u = FilterUnit { unit_id: 1, role: 1, mask: 2, packets: 10, last_activity: 20, instance_id: "HID\\VID_1\\7&1".into() };
        assert_eq!(FilterUnit::decode_list(&FilterUnit::encode_list(std::slice::from_ref(&u))), vec![u]);
        assert!(encode_unit_mask("", SeatMask(1), 0).is_none());
        assert!(encode_unit_mask(&"x".repeat(200), SeatMask(1), 0).is_none());
    }

    #[test]
    fn config_from_settings() {
        let s = seat_core::config::InputSettings::default();
        let c = FilterConfig::from_settings(&s, true);
        assert_eq!(c.flags & config_flags::WIZARD_CAPTURE, config_flags::WIZARD_CAPTURE);
        assert_eq!(c.flags & config_flags::HOTKEY_ENABLED, config_flags::HOTKEY_ENABLED);
        assert_eq!(c.hotkey_scan, 0x58);
        let b = c.encode();
        assert_eq!(u32le(&b, 0) as usize, CONFIG_SIZE);
        assert_eq!(u16le(&b, 8), 0x58);
        // Emergency key = the hotkey, right after the minor-0 part.
        assert_eq!(u16le(&b, CONFIG_V0_SIZE), 0x58);
        // Hotkey off: no HOTKEY_ENABLED, but the emergency key stays (F12).
        let off = seat_core::config::InputSettings { switch_hotkey_scan: None, ..Default::default() };
        let c = FilterConfig::from_settings(&off, false);
        assert_eq!(c.flags & config_flags::HOTKEY_ENABLED, 0);
        assert_eq!((c.hotkey_scan, c.emergency_scan, c.emergency_flags), (0, crate::convert::EMERGENCY_FALLBACK_KEY.sc, 0));
        assert_eq!(u16le(&c.encode(), CONFIG_V0_SIZE), 0x58);
        // E0 hotkey: E0 emergency key.
        let e0 = seat_core::config::InputSettings { switch_hotkey_scan: Some(0xE047), ..Default::default() };
        let c = FilterConfig::from_settings(&e0, false);
        assert_eq!((c.emergency_scan, c.emergency_flags), (0x47, 2));
    }

    #[test]
    fn unfiltered_default_keeps_lamp_arrays() {
        // seat-core's default is (0x59, 0): usage 0 is the page wildcard of the driver, so
        // the lamp-array collection (0x59/0x01) of spec 50 §4.1.3 rule 1 stays exempt.
        let c = FilterConfig::from_settings(&seat_core::config::InputSettings::default(), false);
        assert!(unfiltered_matches(&c.unfiltered, 0x59, 0x01));
        assert!(!unfiltered_matches(&c.unfiltered, 0x0D, 0x04));
        assert!(unfiltered_matches(&[(0x01, 0x80)], 0x01, 0x80));
        assert!(!unfiltered_matches(&[(0x01, 0x80)], 0x01, 0x02));
        let b = c.encode();
        // First unfiltered entry right after the count (offset 24).
        assert_eq!((u16le(&b, 24), u16le(&b, 26)), (0x59, 0));
    }

    #[test]
    fn system_buttons_survive_a_full_unfiltered_list() {
        let s = seat_core::config::InputSettings {
            allow_system_buttons: true,
            unfiltered_hid_usages: (1..=20u16).map(|u| (0xFF00, u)).chain([(0x01, 0x80)]).collect(),
            ..Default::default()
        };
        let c = FilterConfig::from_settings(&s, false);
        assert_eq!(c.unfiltered.len(), MAX_UNFILTERED);
        assert_eq!(c.unfiltered[0], (0x01, 0x80));
        assert_eq!(c.unfiltered.iter().filter(|&&u| u == (0x01, 0x80)).count(), 1);
        assert_eq!(c.flags & config_flags::ALLOW_SYSTEM_BUTTONS, config_flags::ALLOW_SYSTEM_BUTTONS);
    }

    #[test]
    fn packet_events() {
        use crate::convert::RawUnitEvent;
        let k = FilterPacket { kind: packet_kind::KEYBOARD, a: 0x1E, b: 1, ..Default::default() };
        assert_eq!(k.event(), Some(RawUnitEvent::Key { sc: 0x1E, flags: 1 }));
        let h = FilterPacket { kind: packet_kind::HOTKEY, ..Default::default() };
        assert_eq!(h.event(), None);
    }
}
