// SPDX-License-Identifier: GPL-3.0-only
//! Route table: which seat gets each input unit (spec 50 §4.1.2, §4.1.7).
//!
//! Units not in the table go to seat 1 (`InputRouter::apply_routes` contract). Exclusive
//! input kinds resolve to one seat; a shareable HID unit with several seats is
//! [`UnitRoute::Shared`] (visible to each of them, delivered to none).

use crate::units::{input_info, normalize_interface_path};
use seat_core::backend::input::InputRoute;
use seat_core::device::Device;
use seat_core::ids::{DeviceKey, SeatMask, SeatNumber};
use std::collections::HashMap;

/// Where one unit's input goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitRoute {
    /// Seat 1 / the console: passes through unchanged.
    Console,
    /// An extra seat: delivered to that seat's host only.
    Seat(SeatNumber),
    /// No seat (pool): delivered to nobody (the wizard still sees it).
    Spare,
    /// Several seats or every seat (HID only): visible to those sessions, no delivery.
    Shared(SeatMask),
}

impl UnitRoute {
    pub fn of(mask: SeatMask) -> Self {
        if mask.is_spare() {
            return Self::Spare;
        }
        match mask.single() {
            Some(s) if s.is_console() => Self::Console,
            Some(s) => Self::Seat(s),
            None => Self::Shared(mask),
        }
    }
    /// Seat whose host receives this unit's packets.
    pub fn delivery_seat(self) -> Option<SeatNumber> {
        match self {
            Self::Console => Some(SeatNumber::CONSOLE),
            Self::Seat(s) => Some(s),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct RouteTable {
    masks: HashMap<DeviceKey, SeatMask>,
}

impl RouteTable {
    pub fn from_routes(routes: &[InputRoute]) -> Self {
        Self { masks: routes.iter().map(|r| (r.key.clone(), r.mask)).collect() }
    }
    pub fn mask(&self, key: &DeviceKey) -> SeatMask {
        self.masks.get(key).copied().unwrap_or(SeatMask::CONSOLE)
    }
    pub fn route(&self, key: &DeviceKey) -> UnitRoute {
        UnitRoute::of(self.mask(key))
    }
    pub fn set(&mut self, key: DeviceKey, mask: SeatMask) {
        self.masks.insert(key, mask);
    }
    pub fn len(&self) -> usize {
        self.masks.len()
    }
    pub fn is_empty(&self) -> bool {
        self.masks.is_empty()
    }
}

/// Device-interface paths (normalized, see [`normalize_interface_path`]) of the units a
/// seat host reads with raw input (`HostAssignment::input_interfaces`).
pub fn interfaces_for_seat(devices: &[Device], table: &RouteTable, seat: SeatNumber) -> Vec<String> {
    let mut v: Vec<String> = devices
        .iter()
        .filter(|d| d.present && table.route(&d.key).delivery_seat() == Some(seat))
        .filter_map(|d| input_info(d).map(|i| normalize_interface_path(&i.interface_path)))
        .collect();
    v.sort();
    v.dedup();
    v
}

/// Keys of the unit that pressed the hotkey and of the mice that move with it: mice of the
/// same physical device (container id) and, failing that, the mice of the keyboard's
/// current seat (spec 50 §4.1.9).
pub fn hotkey_group(devices: &[Device], table: &RouteTable, key: &DeviceKey) -> Vec<DeviceKey> {
    use seat_core::device::DeviceKind;
    let Some(kbd) = devices.iter().find(|d| &d.key == key) else { return vec![key.clone()] };
    let from = table.route(key).delivery_seat();
    let mut out = vec![key.clone()];
    let same_container: Vec<&Device> = devices
        .iter()
        .filter(|d| d.kind == DeviceKind::Mouse && d.container_id.is_some() && d.container_id == kbd.container_id)
        .collect();
    let mice: Vec<&Device> = if !same_container.is_empty() {
        same_container
    } else {
        devices
            .iter()
            .filter(|d| d.kind == DeviceKind::Mouse && from.is_some() && table.route(&d.key).delivery_seat() == from)
            .collect()
    };
    out.extend(mice.into_iter().map(|d| d.key.clone()));
    out
}

/// Next seat after `from` among `1..=seat_count`, wrapping to seat 1.
pub fn next_seat(from: SeatNumber, seat_count: u8) -> SeatNumber {
    let n = from.get() % seat_count.max(1) + 1;
    SeatNumber::new(n).unwrap_or(SeatNumber::CONSOLE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::{build_units, tests as ut};
    use seat_core::device::DeviceKind;

    fn s(n: u8) -> SeatNumber {
        SeatNumber::new(n).unwrap()
    }

    #[test]
    fn route_kinds() {
        assert_eq!(UnitRoute::of(SeatMask::CONSOLE), UnitRoute::Console);
        assert_eq!(UnitRoute::of(s(3).mask()), UnitRoute::Seat(s(3)));
        assert_eq!(UnitRoute::of(SeatMask::SPARE), UnitRoute::Spare);
        assert_eq!(UnitRoute::of(SeatMask::SHARED), UnitRoute::Shared(SeatMask::SHARED));
        assert_eq!(UnitRoute::of(SeatMask(0b110)).delivery_seat(), None);
    }

    #[test]
    fn seat_interfaces_and_default() {
        let t = ut::tree();
        let units = build_units(&t, &ut::list());
        let kbd = units.iter().find(|d| d.kind == DeviceKind::Keyboard).unwrap().key.clone();
        let table = RouteTable::from_routes(&[InputRoute { key: kbd.clone(), mask: s(2).mask() }]);
        assert_eq!(interfaces_for_seat(&units, &table, s(2)), vec![ut::KBD_PATH.to_ascii_lowercase()]);
        // Everything else defaults to the console.
        assert_eq!(interfaces_for_seat(&units, &table, SeatNumber::CONSOLE).len(), units.len() - 1);
        assert!(interfaces_for_seat(&units, &table, s(3)).is_empty());
    }

    #[test]
    fn hotkey_group_and_next_seat() {
        let t = ut::tree();
        let units = build_units(&t, &ut::list());
        let kbd = units.iter().find(|d| d.kind == DeviceKind::Keyboard).unwrap().key.clone();
        let mou = units.iter().find(|d| d.kind == DeviceKind::Mouse).unwrap().key.clone();
        let table = RouteTable::from_routes(&[
            InputRoute { key: kbd.clone(), mask: s(2).mask() },
            InputRoute { key: mou.clone(), mask: s(2).mask() },
        ]);
        assert_eq!(hotkey_group(&units, &table, &kbd), vec![kbd.clone(), mou]);
        assert_eq!(next_seat(s(2), 3), s(3));
        assert_eq!(next_seat(s(3), 3), s(1));
        assert_eq!(next_seat(s(1), 1), s(1));
    }
}
