// SPDX-License-Identifier: GPL-3.0-only
//! Authoritative session -> seat map (spec 50 §3.3).
//!
//! The service writes one volatile subkey per **running** seat under
//! [`crate::REG_SEATMAP`], named by the session id, and deletes it when the seat stops (and
//! clears the whole map at boot), so no stale session ids remain. The module only reads it.
//!
//! Values of `<session id>`: `SessionId` (DWORD, equals the key name), `SeatId` (SZ,
//! lowercase hyphenated GUID), `UserSid` (SZ), `Generation` (DWORD), `PadInstances`
//! (MULTI_SZ, upper-case instance-id prefixes of pads the service owns, e.g. ViGEm pads),
//! `Flags` (DWORD bits, see [`SeatFlags`]).

use crate::error::ShimError;
use crate::regval::{RegValue, find};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SeatFlags {
    pub hid_isolation: bool,
    pub xinput_focus: bool,
    pub steam: bool,
    /// Two seats share one Windows account (forbidden by our seat validation; only then
    /// the HKCU redirect fallback runs). Addition to the contract.
    #[serde(default)]
    pub shared_account: bool,
    /// Identity-only entry of the local seat that holds the console session right now (fast
    /// user switching, VM round 2, 2026-11-17). Its processes keep the console role (HID,
    /// focus and every other rule ignore the entry); only the per-seat names of single-
    /// instance programs and Steam's IPC override use it, so a seat's names stay the same
    /// whether or not it has the console. Addition to the contract (bit 16).
    #[serde(default)]
    pub console: bool,
}

/// Seat id of local seat 1 in the seat map (`seat_input::local_pads::local_seat_pad_id(1)`).
/// Seat 1 keeps the programs' own names (no per-seat suffix, no Steam override).
pub const LOCAL_SEAT_ONE_ID: &str = "5053454c-4f43-4100-8000-000000000001";

impl SeatFlags {
    pub const HID: u32 = 1;
    pub const XINPUT_FOCUS: u32 = 2;
    pub const STEAM: u32 = 4;
    pub const SHARED_ACCOUNT: u32 = 8;
    pub const CONSOLE: u32 = 16;

    pub fn bits(self) -> u32 {
        (self.hid_isolation as u32 * Self::HID)
            | (self.xinput_focus as u32 * Self::XINPUT_FOCUS)
            | (self.steam as u32 * Self::STEAM)
            | (self.shared_account as u32 * Self::SHARED_ACCOUNT)
            | (self.console as u32 * Self::CONSOLE)
    }
    pub fn from_bits(b: u32) -> Self {
        Self {
            hid_isolation: b & Self::HID != 0,
            xinput_focus: b & Self::XINPUT_FOCUS != 0,
            steam: b & Self::STEAM != 0,
            shared_account: b & Self::SHARED_ACCOUNT != 0,
            console: b & Self::CONSOLE != 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatMapEntry {
    pub session_id: u32,
    pub seat_id: String,
    pub user_sid: String,
    pub generation: u32,
    pub pad_instance_prefixes: Vec<String>,
    pub flags: SeatFlags,
}

impl SeatMapEntry {
    /// `seat32` form of the seat id (32 upper-case hex digits, spec 60 §3.6).
    pub fn seat32(&self) -> String {
        seat32(&self.seat_id)
    }
}

/// Seat GUID -> 32 upper-case hex digits without braces or dashes.
pub fn seat32(seat_id: &str) -> String {
    seat_id.chars().filter(|c| c.is_ascii_hexdigit()).map(|c| c.to_ascii_uppercase()).collect()
}

/// 32 hex digits -> lowercase hyphenated GUID (inverse of [`seat32`]).
pub fn seat_id_from_seat32(s: &str) -> Option<String> {
    if s.len() != 32 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let l = s.to_ascii_lowercase();
    Some(format!("{}-{}-{}-{}-{}", &l[0..8], &l[8..12], &l[12..16], &l[16..20], &l[20..32]))
}

pub fn is_seat_id(s: &str) -> bool {
    let b = s.as_bytes();
    s.len() == 36
        && b.iter().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => *c == b'-',
            _ => c.is_ascii_digit() || (b'a'..=b'f').contains(c),
        })
}

fn is_sid(s: &str) -> bool {
    s.starts_with("S-1-") && s.len() <= 184 && s[4..].chars().all(|c| c.is_ascii_digit() || c == '-')
}

pub const V_SESSION_ID: &str = "SessionId";
pub const V_SEAT_ID: &str = "SeatId";
pub const V_USER_SID: &str = "UserSid";
pub const V_GENERATION: &str = "Generation";
pub const V_PAD_INSTANCES: &str = "PadInstances";
pub const V_FLAGS: &str = "Flags";

pub fn encode_entry(e: &SeatMapEntry) -> Vec<(&'static str, RegValue)> {
    vec![
        (V_SESSION_ID, RegValue::Dword(e.session_id)),
        (V_SEAT_ID, RegValue::Sz(e.seat_id.clone())),
        (V_USER_SID, RegValue::Sz(e.user_sid.clone())),
        (V_GENERATION, RegValue::Dword(e.generation)),
        (
            V_PAD_INSTANCES,
            RegValue::MultiSz(e.pad_instance_prefixes.iter().map(|p| p.to_ascii_uppercase()).collect()),
        ),
        (V_FLAGS, RegValue::Dword(e.flags.bits())),
    ]
}

pub fn decode_entry(values: &[(String, RegValue)]) -> Result<SeatMapEntry, ShimError> {
    let dword = |n: &str| {
        find(values, n).and_then(RegValue::as_dword).ok_or_else(|| ShimError::Decode(format!("seat map: {n} missing")))
    };
    let string = |n: &str| {
        find(values, n)
            .and_then(RegValue::as_str)
            .map(str::to_string)
            .ok_or_else(|| ShimError::Decode(format!("seat map: {n} missing")))
    };
    let session_id = dword(V_SESSION_ID)?;
    if session_id == 0 {
        return Err(ShimError::Decode("seat map: session 0 is never a seat".into()));
    }
    let seat_id = string(V_SEAT_ID)?.to_ascii_lowercase();
    if !is_seat_id(&seat_id) {
        return Err(ShimError::Decode("seat map: SeatId is not a GUID".into()));
    }
    let user_sid = string(V_USER_SID)?;
    if !is_sid(&user_sid) {
        return Err(ShimError::Decode("seat map: UserSid is not a SID".into()));
    }
    let pads = match find(values, V_PAD_INSTANCES) {
        None => Vec::new(),
        Some(v) => v
            .as_multi()
            .ok_or_else(|| ShimError::Decode("seat map: PadInstances is not MULTI_SZ".into()))?
            .iter()
            .filter(|p| !p.is_empty())
            .map(|p| p.to_ascii_uppercase())
            .collect(),
    };
    Ok(SeatMapEntry {
        session_id,
        seat_id,
        user_sid,
        generation: dword(V_GENERATION)?,
        pad_instance_prefixes: pads,
        // Absent flags = isolation on (spec 50 §3.3: "absent switch = isolation on").
        flags: find(values, V_FLAGS)
            .and_then(RegValue::as_dword)
            .map(SeatFlags::from_bits)
            .unwrap_or(SeatFlags { hid_isolation: true, xinput_focus: true, steam: true, shared_account: false, console: false }),
    })
}

/// Service side (real impl: [`crate::win::RegSeatMap`]).
pub trait SeatMapWriter: Send + Sync {
    /// Creates (or replaces) the volatile `<session id>` key with an Everyone-read,
    /// SYSTEM-write DACL.
    fn publish(&self, e: &SeatMapEntry) -> Result<(), ShimError>;
    fn remove(&self, session_id: u32) -> Result<(), ShimError>;
    fn clear(&self) -> Result<(), ShimError>;
}

/// Module side. Entries that do not decode are skipped (and logged by the caller).
pub trait SeatMapReader: Send + Sync {
    fn read_all(&self) -> Result<Vec<SeatMapEntry>, ShimError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn sample() -> SeatMapEntry {
        SeatMapEntry {
            session_id: 3,
            seat_id: "0f8e2c1a-1b2c-4d5e-8f90-a1b2c3d4e5f6".into(),
            user_sid: "S-1-5-21-1-2-3-1001".into(),
            generation: 7,
            pad_instance_prefixes: vec![r"usb\vid_045e&pid_028e\1&2d595ca7&0&01".into()],
            flags: SeatFlags { hid_isolation: true, xinput_focus: false, steam: true, shared_account: false, console: false },
        }
    }

    fn owned(v: Vec<(&'static str, RegValue)>) -> Vec<(String, RegValue)> {
        v.into_iter().map(|(n, v)| (n.to_string(), v)).collect()
    }

    #[test]
    fn encode_decode_round_trip() {
        let e = sample();
        let d = decode_entry(&owned(encode_entry(&e))).unwrap();
        assert_eq!(d.seat_id, e.seat_id);
        assert_eq!(d.pad_instance_prefixes, vec![r"USB\VID_045E&PID_028E\1&2D595CA7&0&01".to_string()]);
        assert_eq!(d.flags, e.flags);
        assert_eq!(d.generation, 7);
    }

    #[test]
    fn flags_bits() {
        let f = SeatFlags { hid_isolation: true, xinput_focus: true, steam: false, shared_account: true, console: false };
        assert_eq!(f.bits(), 1 | 2 | 8);
        assert_eq!(SeatFlags::from_bits(f.bits()), f);
    }

    #[test]
    fn absent_flags_mean_on() {
        let mut v = owned(encode_entry(&sample()));
        v.retain(|(n, _)| n != V_FLAGS && n != V_PAD_INSTANCES);
        let d = decode_entry(&v).unwrap();
        assert!(d.flags.hid_isolation && d.flags.xinput_focus && d.flags.steam && !d.flags.shared_account);
        assert!(d.pad_instance_prefixes.is_empty());
    }

    #[test]
    fn rejects_bad_entries() {
        let base = owned(encode_entry(&sample()));
        let with = |name: &str, val: RegValue| {
            let mut v = base.clone();
            for (n, x) in v.iter_mut() {
                if n == name {
                    *x = val.clone();
                }
            }
            v
        };
        assert!(decode_entry(&with(V_SESSION_ID, RegValue::Dword(0))).is_err());
        assert!(decode_entry(&with(V_SEAT_ID, RegValue::Sz("seat-a".into()))).is_err());
        assert!(decode_entry(&with(V_USER_SID, RegValue::Sz("bob".into()))).is_err());
        assert!(decode_entry(&with(V_PAD_INSTANCES, RegValue::Sz("x".into()))).is_err());
        let mut missing = base.clone();
        missing.retain(|(n, _)| n != V_GENERATION);
        assert!(decode_entry(&missing).is_err());
        // Upper-case seat id is accepted and normalised.
        let up = decode_entry(&with(V_SEAT_ID, RegValue::Sz("0F8E2C1A-1B2C-4D5E-8F90-A1B2C3D4E5F6".into()))).unwrap();
        assert_eq!(up.seat_id, sample().seat_id);
    }

    #[test]
    fn seat32_conversions() {
        let s = seat32("0f8e2c1a-1b2c-4d5e-8f90-a1b2c3d4e5f6");
        assert_eq!(s, "0F8E2C1A1B2C4D5E8F90A1B2C3D4E5F6");
        assert_eq!(seat_id_from_seat32(&s).unwrap(), "0f8e2c1a-1b2c-4d5e-8f90-a1b2c3d4e5f6");
        assert!(seat_id_from_seat32("XYZ").is_none());
    }
}
