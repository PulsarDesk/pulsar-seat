// SPDX-License-Identifier: GPL-3.0-only
//! Kiosk seat map: which physical input devices belong to which local kiosk seat
//! (round 4, kiosk-isolation; architecture.md §4.1.3, shim/README.md "Kiosk seats").
//!
//! Local kiosk seats (spec 35 §5.1 U3, `desktops` backend) all run in **one** session, the
//! console session, so the session -> seat map ([`crate::seatmap`]) cannot tell them apart.
//! Instead:
//!
//! - the service publishes one volatile key per **running** kiosk shell under
//!   [`crate::REG_KIOSKMAP`], named by the seat number (two decimal digits, `02`..`16`),
//!   with the device-instance-id prefixes of the keyboards, mice and HID pads that are
//!   effectively assigned to exactly that seat;
//! - the kiosk shell puts [`KIOSK_SEAT_ENV`] = seat number into its own environment, so
//!   every app it starts (and their children) inherits it;
//! - the module reads both and takes the role [`crate::decide::Role::Kiosk`] only when the
//!   variable names a published seat whose session is the process's session.
//!
//! The variable is **not a security boundary**: every kiosk seat runs as the same Windows
//! user, who can start any process with any environment anyway. It only routes devices.
//!
//! Values of `<NN>`: `Seat` (DWORD, equals the key name), `SessionId` (DWORD), `Generation`
//! (DWORD), `Devices` (MULTI_SZ, upper-case instance-id prefixes), `Flags` (DWORD, see
//! [`KioskFlags`]).

use crate::error::ShimError;
use crate::regval::{RegValue, find};
use serde::{Deserialize, Serialize};

/// Environment variable the kiosk shell sets for its apps: the seat number (decimal).
pub const KIOSK_SEAT_ENV: &str = "PULSAR_SEAT_KIOSK";
/// Highest seat number (seat-core `SeatNumber` limit).
pub const MAX_KIOSK_SEAT: u32 = 16;
/// Device prefixes per seat that are published (more are dropped, with a warning by the
/// service).
pub const MAX_KIOSK_DEVICES: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KioskFlags {
    /// HID / XUSB opens and HID / raw-input enumeration filtered to the seat's devices.
    pub hid_isolation: bool,
    /// `GetRawInputData` / `GetRawInputBuffer` drop input of other seats' devices.
    pub raw_input: bool,
    /// `GetCursorPos` / `GetPhysicalCursorPos` / `GetCursorInfo` / `GetMessagePos` answer
    /// with the seat's own cursor (published by the shell, [`crate::kiosk_cursor`]), so
    /// popup menus and apps that read the cursor follow the seat's mouse (parity
    /// 2026-11-17). Entries written before that have no bit 4 and read as off.
    #[serde(default)]
    pub cursor: bool,
}

impl Default for KioskFlags {
    fn default() -> Self {
        Self { hid_isolation: true, raw_input: true, cursor: true }
    }
}

impl KioskFlags {
    pub const HID: u32 = 1;
    pub const RAW_INPUT: u32 = 2;
    pub const CURSOR: u32 = 4;

    pub fn bits(self) -> u32 {
        (self.hid_isolation as u32 * Self::HID) | (self.raw_input as u32 * Self::RAW_INPUT) | (self.cursor as u32 * Self::CURSOR)
    }
    pub fn from_bits(b: u32) -> Self {
        Self { hid_isolation: b & Self::HID != 0, raw_input: b & Self::RAW_INPUT != 0, cursor: b & Self::CURSOR != 0 }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KioskSeatEntry {
    /// Seat number, 2..=16 (seat 1 is the console desktop and never a kiosk map entry; a
    /// seat-1 kiosk shell with `seat1_desktop = false` is published too, as 1).
    pub seat: u32,
    /// Session the shell runs in (the console session).
    pub session_id: u32,
    pub generation: u32,
    /// Upper-case device-instance-id prefixes (keyboards, mice, HID pads of this seat).
    pub devices: Vec<String>,
    pub flags: KioskFlags,
}

pub const V_SEAT: &str = "Seat";
pub const V_SESSION_ID: &str = "SessionId";
pub const V_GENERATION: &str = "Generation";
pub const V_DEVICES: &str = "Devices";
pub const V_FLAGS: &str = "Flags";

/// Registry key name of a seat (`02`).
pub fn key_name(seat: u32) -> String {
    format!("{seat:02}")
}

fn norm_device(d: &str) -> Option<String> {
    let t = d.trim().replace('/', "\\").to_ascii_uppercase();
    // A prefix shorter than `HID\X` would match far too much.
    (t.len() >= 5 && t.contains('\\')).then_some(t)
}

pub fn encode_kiosk_entry(e: &KioskSeatEntry) -> Vec<(&'static str, RegValue)> {
    vec![
        (V_SEAT, RegValue::Dword(e.seat)),
        (V_SESSION_ID, RegValue::Dword(e.session_id)),
        (V_GENERATION, RegValue::Dword(e.generation)),
        (V_DEVICES, RegValue::MultiSz(e.devices.iter().filter_map(|d| norm_device(d)).take(MAX_KIOSK_DEVICES).collect())),
        (V_FLAGS, RegValue::Dword(e.flags.bits())),
    ]
}

pub fn decode_kiosk_entry(values: &[(String, RegValue)]) -> Result<KioskSeatEntry, ShimError> {
    let dword = |n: &str| {
        find(values, n).and_then(RegValue::as_dword).ok_or_else(|| ShimError::Decode(format!("kiosk map: {n} missing")))
    };
    let seat = dword(V_SEAT)?;
    if seat == 0 || seat > MAX_KIOSK_SEAT {
        return Err(ShimError::Decode("kiosk map: Seat out of range".into()));
    }
    let session_id = dword(V_SESSION_ID)?;
    if session_id == 0 {
        return Err(ShimError::Decode("kiosk map: session 0 is never a seat".into()));
    }
    let devices = match find(values, V_DEVICES) {
        None => Vec::new(),
        Some(v) => v
            .as_multi()
            .ok_or_else(|| ShimError::Decode("kiosk map: Devices is not MULTI_SZ".into()))?
            .iter()
            .filter_map(|d| norm_device(d))
            .take(MAX_KIOSK_DEVICES)
            .collect(),
    };
    Ok(KioskSeatEntry {
        seat,
        session_id,
        generation: dword(V_GENERATION)?,
        devices,
        flags: find(values, V_FLAGS).and_then(RegValue::as_dword).map(KioskFlags::from_bits).unwrap_or_default(),
    })
}

/// Parses the value of [`KIOSK_SEAT_ENV`] (`"2"`, `"02"`); `None` when absent or invalid.
pub fn parse_kiosk_env(v: Option<&str>) -> Option<u32> {
    let t = v?.trim();
    if t.is_empty() || t.len() > 2 || !t.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: u32 = t.parse().ok()?;
    (1..=MAX_KIOSK_SEAT).contains(&n).then_some(n)
}

/// Seat a device chain (child first, instance ids) belongs to, if any kiosk seat claims a
/// node of it. A node matches a prefix when it equals it or continues it at a `\` or `&`
/// boundary (so `HID\VID_1&PID_2\1` never claims `HID\VID_1&PID_20\1`). If two seats claim
/// the chain, the node nearest to the child decides; on the same node the longer prefix.
pub fn kiosk_owner<'a, I>(chain: I, kiosk: &[KioskSeatEntry]) -> Option<u32>
where
    I: IntoIterator<Item = &'a str>,
{
    for id in chain {
        let up = id.to_ascii_uppercase();
        let best = kiosk
            .iter()
            .flat_map(|e| e.devices.iter().map(move |p| (e.seat, p)))
            .filter(|(_, p)| prefix_matches(&up, p))
            .max_by_key(|(_, p)| p.len());
        if let Some((seat, _)) = best {
            return Some(seat);
        }
    }
    None
}

fn prefix_matches(id_upper: &str, prefix: &str) -> bool {
    if prefix.is_empty() || !id_upper.starts_with(prefix) {
        return false;
    }
    match id_upper.as_bytes().get(prefix.len()) {
        None => true,
        Some(b) => *b == b'\\' || *b == b'&' || prefix.ends_with(['\\', '&']),
    }
}

/// Service side (real impl: [`crate::win::RegKioskMap`]).
pub trait KioskMapWriter: Send + Sync {
    /// Creates (or replaces) the volatile `<NN>` key (Everyone read, SYSTEM write).
    fn publish(&self, e: &KioskSeatEntry) -> Result<(), ShimError>;
    fn remove(&self, seat: u32) -> Result<(), ShimError>;
    fn clear(&self) -> Result<(), ShimError>;
}

/// Module side. Entries that do not decode are skipped.
pub trait KioskMapReader: Send + Sync {
    fn read_all(&self) -> Result<Vec<KioskSeatEntry>, ShimError>;
}

/// What the service changes to go from `current` (published) to `want`: entries to
/// publish (new or changed) and seats to remove. Pure, so the sync loop is tested.
pub fn kiosk_map_diff(current: &[KioskSeatEntry], want: &[KioskSeatEntry]) -> (Vec<KioskSeatEntry>, Vec<u32>) {
    let publish = want.iter().filter(|w| !current.iter().any(|c| c == *w)).cloned().collect();
    let remove = current.iter().filter(|c| !want.iter().any(|w| w.seat == c.seat)).map(|c| c.seat).collect();
    (publish, remove)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owned(v: Vec<(&'static str, RegValue)>) -> Vec<(String, RegValue)> {
        v.into_iter().map(|(n, v)| (n.to_string(), v)).collect()
    }

    fn entry(seat: u32, devices: &[&str]) -> KioskSeatEntry {
        KioskSeatEntry {
            seat,
            session_id: 1,
            generation: 1,
            devices: devices.iter().map(|d| d.to_string()).collect(),
            flags: KioskFlags::default(),
        }
    }

    #[test]
    fn round_trip_normalises_devices() {
        let e = entry(3, &[r"hid\vid_046d&pid_c52b&mi_00\7&1a&0&0000", "x", r" USB/VID_1\2 "]);
        let d = decode_kiosk_entry(&owned(encode_kiosk_entry(&e))).unwrap();
        assert_eq!(d.devices, vec![r"HID\VID_046D&PID_C52B&MI_00\7&1A&0&0000".to_string(), r"USB\VID_1\2".into()]);
        assert_eq!(d.seat, 3);
        assert_eq!(d.flags, KioskFlags::default());
        assert_eq!(key_name(3), "03");
    }

    #[test]
    fn rejects_bad_entries() {
        let base = owned(encode_kiosk_entry(&entry(2, &[])));
        let with = |name: &str, val: RegValue| {
            let mut v = base.clone();
            v.iter_mut().filter(|(n, _)| n == name).for_each(|(_, x)| *x = val.clone());
            v
        };
        assert!(decode_kiosk_entry(&with(V_SEAT, RegValue::Dword(0))).is_err());
        assert!(decode_kiosk_entry(&with(V_SEAT, RegValue::Dword(17))).is_err());
        assert!(decode_kiosk_entry(&with(V_SESSION_ID, RegValue::Dword(0))).is_err());
        assert!(decode_kiosk_entry(&with(V_DEVICES, RegValue::Sz("x".into()))).is_err());
        let mut no_flags = base.clone();
        no_flags.retain(|(n, _)| n != V_FLAGS);
        assert_eq!(decode_kiosk_entry(&no_flags).unwrap().flags, KioskFlags::default());
        let f = KioskFlags { hid_isolation: false, raw_input: true, cursor: false };
        assert_eq!(KioskFlags::from_bits(f.bits()), f);
        assert_eq!(KioskFlags::default().bits(), 7);
        // A round-4 entry (bits 1 | 2) has no cursor bit.
        assert!(!KioskFlags::from_bits(3).cursor && KioskFlags::from_bits(4).cursor);
    }

    #[test]
    fn env_parsing() {
        assert_eq!(parse_kiosk_env(Some("2")), Some(2));
        assert_eq!(parse_kiosk_env(Some(" 05 ")), Some(5));
        assert_eq!(parse_kiosk_env(Some("16")), Some(16));
        assert_eq!(parse_kiosk_env(Some("17")), None);
        assert_eq!(parse_kiosk_env(Some("0")), None);
        assert_eq!(parse_kiosk_env(Some("-1")), None);
        assert_eq!(parse_kiosk_env(Some("002")), None);
        assert_eq!(parse_kiosk_env(Some("")), None);
        assert_eq!(parse_kiosk_env(None), None);
    }

    #[test]
    fn owner_by_prefix_boundary() {
        let k = vec![entry(2, &[r"HID\VID_1&PID_2"]), entry(3, &[r"HID\VID_1&PID_20", r"USB\VID_9&PID_9\5&AB"])];
        assert_eq!(kiosk_owner([r"HID\VID_1&PID_2&MI_00\7&1"], &k), Some(2));
        assert_eq!(kiosk_owner([r"hid\vid_1&pid_20\7&1"], &k), Some(3));
        assert_eq!(kiosk_owner([r"HID\VID_1&PID_21\7&1"], &k), None);
        // Parent chain: the composite USB parent claims its HID children.
        assert_eq!(kiosk_owner([r"HID\VID_9&PID_9&MI_01\8&2", r"USB\VID_9&PID_9\5&AB"], &k), Some(3));
        assert_eq!(kiosk_owner([r"USB\VID_9&PID_9\5&ABC"], &k), None);
        assert_eq!(kiosk_owner(std::iter::empty::<&str>(), &k), None);
        // Longer prefix on the same node wins.
        let k2 = vec![entry(2, &[r"HID\VID_1"]), entry(4, &[r"HID\VID_1&PID_2\7&1"])];
        assert_eq!(kiosk_owner([r"HID\VID_1&PID_2\7&1"], &k2), Some(4));
    }

    #[test]
    fn diff() {
        let a = entry(2, &[r"HID\A\1"]);
        let b = entry(3, &[r"HID\B\1"]);
        let mut b2 = b.clone();
        b2.devices.push(r"HID\B\2".into());
        let (p, r) = kiosk_map_diff(&[a.clone(), b.clone()], &[b2.clone()]);
        assert_eq!(p, vec![b2.clone()]);
        assert_eq!(r, vec![2]);
        let (p, r) = kiosk_map_diff(&[b2.clone()], &[b2]);
        assert!(p.is_empty() && r.is_empty());
    }
}
