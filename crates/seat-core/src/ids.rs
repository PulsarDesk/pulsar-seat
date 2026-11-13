// SPDX-License-Identifier: GPL-3.0-only
//! Seat numbers, seat masks and stable device keys.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Highest seat number Pulsar Seat supports (spec 10, Pulsar approach: 16).
pub const MAX_SEATS: u8 = 16;

/// A seat number, `1..=MAX_SEATS`. Seat 1 is the console seat and always exists.
/// Serialized as a plain number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct SeatNumber(u8);

impl SeatNumber {
    /// Seat 1, the console seat.
    pub const CONSOLE: SeatNumber = SeatNumber(1);

    /// `None` unless `1 <= n <= MAX_SEATS`.
    pub const fn new(n: u8) -> Option<Self> {
        if n >= 1 && n <= MAX_SEATS { Some(Self(n)) } else { None }
    }
    pub const fn get(self) -> u8 {
        self.0
    }
    /// Zero-based index (seat 1 = 0): bit position in a [`SeatMask`].
    pub const fn index(self) -> u8 {
        self.0 - 1
    }
    /// The mask with only this seat.
    pub const fn mask(self) -> SeatMask {
        SeatMask(1 << (self.0 - 1))
    }
    pub const fn is_console(self) -> bool {
        self.0 == 1
    }
    /// Two-digit form used in names (`02`): LSA secret names, RDP client names, log files.
    pub fn two_digits(self) -> String {
        format!("{:02}", self.0)
    }
    /// All seats `1..=count` (count clamped to `MAX_SEATS`).
    pub fn range(count: u8) -> impl Iterator<Item = SeatNumber> {
        (1..=count.min(MAX_SEATS)).map(SeatNumber)
    }
}

impl TryFrom<u8> for SeatNumber {
    type Error = String;
    fn try_from(n: u8) -> Result<Self, String> {
        Self::new(n).ok_or_else(|| format!("seat number {n} out of range 1..={MAX_SEATS}"))
    }
}

impl From<SeatNumber> for u8 {
    fn from(s: SeatNumber) -> u8 {
        s.0
    }
}

impl fmt::Display for SeatNumber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// One bit per seat, bit 0 = seat 1 (spec 00 §1).
///
/// - `0` = **spare** (no seat uses the device, the "System" pool in the manager).
/// - [`SeatMask::SHARED`] (all 32 bits) = shared by every seat, including seats added later.
/// - Any other value = the listed seats. Exclusive device kinds allow at most one bit.
///
/// Serialized as a plain integer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SeatMask(pub u32);

impl SeatMask {
    pub const SPARE: SeatMask = SeatMask(0);
    pub const SHARED: SeatMask = SeatMask(u32::MAX);
    pub const CONSOLE: SeatMask = SeatMask(1);

    pub fn from_seats(seats: impl IntoIterator<Item = SeatNumber>) -> Self {
        Self(seats.into_iter().fold(0, |m, s| m | s.mask().0))
    }
    pub const fn is_spare(self) -> bool {
        self.0 == 0
    }
    pub const fn is_shared(self) -> bool {
        self.0 == u32::MAX
    }
    pub const fn contains(self, seat: SeatNumber) -> bool {
        self.0 & seat.mask().0 != 0
    }
    pub fn insert(&mut self, seat: SeatNumber) {
        self.0 |= seat.mask().0;
    }
    pub fn remove(&mut self, seat: SeatNumber) {
        if self.is_shared() {
            self.0 = Self::all_up_to(MAX_SEATS).0;
        }
        self.0 &= !seat.mask().0;
    }
    /// Mask of seats `1..=count`.
    pub fn all_up_to(count: u8) -> Self {
        Self::from_seats(SeatNumber::range(count))
    }
    /// Number of seats in the mask, counting only seats `1..=MAX_SEATS`.
    pub fn count(self) -> u32 {
        (self.0 & Self::all_up_to(MAX_SEATS).0).count_ones()
    }
    /// The only seat, if exactly one bit (within `1..=MAX_SEATS`) is set and the mask is
    /// not shared.
    pub fn single(self) -> Option<SeatNumber> {
        if self.is_shared() || self.count() != 1 {
            return None;
        }
        SeatNumber::new(self.0.trailing_zeros() as u8 + 1)
    }
    /// Seats in the mask, ascending (shared = every seat up to `MAX_SEATS`).
    pub fn seats(self) -> impl Iterator<Item = SeatNumber> {
        SeatNumber::range(MAX_SEATS).filter(move |s| self.contains(*s))
    }
    /// Bits above `seat_count` (other than for SHARED): such seats do not exist.
    pub fn beyond(self, seat_count: u8) -> bool {
        !self.is_shared() && self.0 & !Self::all_up_to(seat_count).0 != 0
    }
    /// Binary string, seat 1 right-most, `width` digits (report and importer format).
    pub fn to_binary_string(self, width: u8) -> String {
        let w = width.clamp(1, 32) as usize;
        let v = if self.is_shared() { Self::all_up_to(width.min(MAX_SEATS)).0 } else { self.0 };
        format!("{v:0w$b}")
    }
    /// Parse the binary string form (all ones = shared, `0` = spare).
    pub fn from_binary_string(s: &str) -> Option<Self> {
        let s = s.trim();
        if s.is_empty() || s.len() > 32 || !s.bytes().all(|b| b == b'0' || b == b'1') {
            return None;
        }
        if s.bytes().all(|b| b == b'1') {
            return Some(Self::SHARED);
        }
        u32::from_str_radix(s, 2).ok().map(Self)
    }
}

/// Stable identity of a device across reboots and driver updates (spec 00 §5.3 P1, spec 40
/// §6.1, spec 50 §4.1.1, spec 60 §8.1, spec 70 §5.1). Built with
/// [`crate::pnp::KeyParts::compose`]. Opaque to every consumer except the builder;
/// compared byte-wise. At most 1024 bytes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct DeviceKey(String);

impl DeviceKey {
    pub const MAX_LEN: usize = 1024;

    pub fn new(s: impl Into<String>) -> Result<Self, String> {
        let s = s.into();
        if s.is_empty() || s.len() > Self::MAX_LEN || s.chars().any(char::is_control) {
            return Err(format!("invalid device key ({} bytes)", s.len()));
        }
        Ok(Self(s))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    /// The kind prefix (text before the first `;`), e.g. `kbd`, `mon`, `aout`.
    pub fn prefix(&self) -> &str {
        self.0.split(';').next().unwrap_or("")
    }
}

impl TryFrom<String> for DeviceKey {
    type Error = String;
    fn try_from(s: String) -> Result<Self, String> {
        Self::new(s)
    }
}

impl From<DeviceKey> for String {
    fn from(k: DeviceKey) -> String {
        k.0
    }
}

impl fmt::Display for DeviceKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(n: u8) -> SeatNumber {
        SeatNumber::new(n).unwrap()
    }

    #[test]
    fn seat_number_range() {
        assert!(SeatNumber::new(0).is_none());
        assert!(SeatNumber::new(17).is_none());
        assert_eq!(s(16).index(), 15);
        assert_eq!(s(2).two_digits(), "02");
        assert!(serde_json::from_str::<SeatNumber>("0").is_err());
        assert_eq!(serde_json::to_string(&s(3)).unwrap(), "3");
    }

    #[test]
    fn mask_basics() {
        let m = SeatMask::from_seats([s(1), s(3)]);
        assert_eq!(m.0, 0b101);
        assert!(m.contains(s(3)) && !m.contains(s(2)));
        assert_eq!(m.count(), 2);
        assert_eq!(m.single(), None);
        assert_eq!(s(4).mask().single(), Some(s(4)));
        assert!(SeatMask::SHARED.contains(s(16)));
        assert_eq!(SeatMask::SHARED.single(), None);
        assert!(m.beyond(2) && !m.beyond(3));
        assert!(!SeatMask::SHARED.beyond(1));
        let mut sh = SeatMask::SHARED;
        sh.remove(s(1));
        assert!(!sh.contains(s(1)) && sh.contains(s(16)));
    }

    #[test]
    fn mask_binary_string() {
        assert_eq!(SeatMask(0b11).to_binary_string(12), "000000000011");
        assert_eq!(SeatMask::SHARED.to_binary_string(4), "1111");
        assert_eq!(SeatMask::from_binary_string("111111111111"), Some(SeatMask::SHARED));
        assert_eq!(SeatMask::from_binary_string("0"), Some(SeatMask::SPARE));
        assert_eq!(SeatMask::from_binary_string("010"), Some(SeatMask(2)));
        assert_eq!(SeatMask::from_binary_string("12"), None);
    }

    #[test]
    fn device_key_rules() {
        assert!(DeviceKey::new("").is_err());
        assert!(DeviceKey::new("a\nb").is_err());
        let k = DeviceKey::new("kbd;loc=X").unwrap();
        assert_eq!(k.prefix(), "kbd");
        assert_eq!(serde_json::to_string(&k).unwrap(), "\"kbd;loc=X\"");
    }
}
