// SPDX-License-Identifier: GPL-3.0-only
//! Split screen (OSMS-parity gap G8, 2026-11-18): up to four seats on one physical monitor,
//! each in a half or a quarter of it.
//!
//! The monitor is cut into a 2x2 grid of cells. A [`ScreenArea`] is a rectangle made of
//! cells (a quarter, a half or the whole monitor), stored as a cell bitmask. A monitor
//! assignment whose mask names two to four seats carries one [`SeatArea`] per seat in
//! [`crate::config::AssignmentOptions::areas`]; validation ([`check_split`]) refuses
//! overlapping cells. HDR, VRR, mode and software cursor stay per monitor, so every seat
//! of a split monitor shares them.
//!
//! Console share: when seat 1 (the console) is one of the seats, the console desktop keeps
//! its half and the other seat takes the opposite half. Only halves: the console's part is
//! kept free with an AppBar, which docks to one edge (`seat_display::win::appbar`).
//!
//! Pure data and rules; no platform code.

use crate::config::AssignmentOptions;
use crate::device::{DeviceKind, Rect};
use crate::ids::{SeatMask, SeatNumber};
use serde::{Deserialize, Serialize};

/// Most seats on one monitor (one per cell).
pub const MAX_SEATS_PER_MONITOR: usize = 4;

/// Top-left cell bit.
pub const CELL_TOP_LEFT: u8 = 0b0001;
/// Top-right cell bit.
pub const CELL_TOP_RIGHT: u8 = 0b0010;
/// Bottom-left cell bit.
pub const CELL_BOTTOM_LEFT: u8 = 0b0100;
/// Bottom-right cell bit.
pub const CELL_BOTTOM_RIGHT: u8 = 0b1000;

/// A part of a monitor made of 2x2 grid cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScreenArea {
    #[default]
    Full,
    Left,
    Right,
    Top,
    Bottom,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

/// Monitor edge an area touches with its whole side (AppBar docking edge).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Edge {
    Left,
    Top,
    Right,
    Bottom,
}

impl ScreenArea {
    pub const ALL: [ScreenArea; 9] = [
        Self::Full,
        Self::Left,
        Self::Right,
        Self::Top,
        Self::Bottom,
        Self::TopLeft,
        Self::TopRight,
        Self::BottomLeft,
        Self::BottomRight,
    ];

    /// Cell bitmask ([`CELL_TOP_LEFT`] ...).
    pub const fn cells(self) -> u8 {
        match self {
            Self::Full => 0b1111,
            Self::Left => CELL_TOP_LEFT | CELL_BOTTOM_LEFT,
            Self::Right => CELL_TOP_RIGHT | CELL_BOTTOM_RIGHT,
            Self::Top => CELL_TOP_LEFT | CELL_TOP_RIGHT,
            Self::Bottom => CELL_BOTTOM_LEFT | CELL_BOTTOM_RIGHT,
            Self::TopLeft => CELL_TOP_LEFT,
            Self::TopRight => CELL_TOP_RIGHT,
            Self::BottomLeft => CELL_BOTTOM_LEFT,
            Self::BottomRight => CELL_BOTTOM_RIGHT,
        }
    }

    /// The area made of exactly `cells`, if those cells form one of the shapes.
    pub fn from_cells(cells: u8) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.cells() == cells)
    }

    pub const fn overlaps(self, other: ScreenArea) -> bool {
        self.cells() & other.cells() != 0
    }

    pub const fn is_half(self) -> bool {
        matches!(self, Self::Left | Self::Right | Self::Top | Self::Bottom)
    }

    /// The opposite half (`Left` <-> `Right`, `Top` <-> `Bottom`); `None` for other areas.
    pub const fn opposite_half(self) -> Option<ScreenArea> {
        match self {
            Self::Left => Some(Self::Right),
            Self::Right => Some(Self::Left),
            Self::Top => Some(Self::Bottom),
            Self::Bottom => Some(Self::Top),
            _ => None,
        }
    }

    /// Docking edge of a half (AppBar); `None` for the whole monitor and quarters.
    pub const fn edge(self) -> Option<Edge> {
        match self {
            Self::Left => Some(Edge::Left),
            Self::Right => Some(Edge::Right),
            Self::Top => Some(Edge::Top),
            Self::Bottom => Some(Edge::Bottom),
            _ => None,
        }
    }

    /// The part of `monitor` this area covers. Odd sizes: the left / top part gets the
    /// smaller half, so two halves always tile the monitor exactly.
    pub fn rect_in(self, monitor: &Rect) -> Rect {
        let c = self.cells();
        let (lw, rw) = (monitor.width / 2, monitor.width - monitor.width / 2);
        let (th, bh) = (monitor.height / 2, monitor.height - monitor.height / 2);
        let left = c & (CELL_TOP_LEFT | CELL_BOTTOM_LEFT) != 0;
        let right = c & (CELL_TOP_RIGHT | CELL_BOTTOM_RIGHT) != 0;
        let top = c & (CELL_TOP_LEFT | CELL_TOP_RIGHT) != 0;
        let bottom = c & (CELL_BOTTOM_LEFT | CELL_BOTTOM_RIGHT) != 0;
        let (x, width) = match (left, right) {
            (true, false) => (monitor.x, lw),
            (false, true) => (monitor.x + lw as i32, rw),
            _ => (monitor.x, monitor.width),
        };
        let (y, height) = match (top, bottom) {
            (true, false) => (monitor.y, th),
            (false, true) => (monitor.y + th as i32, bh),
            _ => (monitor.y, monitor.height),
        };
        Rect { x, y, width, height }
    }
}

/// One seat's part of a split monitor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatArea {
    pub seat: SeatNumber,
    pub area: ScreenArea,
}

/// Why a split monitor assignment is refused (validation `invalid_screen_area`,
/// `screen_area_overlap`, `console_share_not_half`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitIssue {
    /// Areas on a device that is not a monitor.
    NotMonitor,
    /// A shared (all seats) monitor cannot be split.
    Shared,
    /// More than [`MAX_SEATS_PER_MONITOR`] seats.
    TooManySeats,
    /// A seat of the mask has no area, an area names a seat outside the mask, or a seat
    /// is listed twice.
    AreaMismatch,
    /// Two seats' areas share a cell.
    Overlap,
    /// Clone outputs on a split monitor.
    CloneOnSplit,
    /// The console (seat 1) shares the monitor but is not on a half with exactly one other
    /// seat on the opposite half.
    ConsoleShareNotHalf,
}

/// Whether the assignment is a split monitor (two or more seats with areas). `Ok(false)`
/// = not split (single seat, spare, or no areas): the usual exclusive rule applies.
pub fn check_split(kind: DeviceKind, mask: SeatMask, options: &AssignmentOptions) -> Result<bool, SplitIssue> {
    if options.areas.is_empty() {
        return Ok(false);
    }
    if kind != DeviceKind::Monitor {
        return Err(SplitIssue::NotMonitor);
    }
    if mask.is_shared() {
        return Err(SplitIssue::Shared);
    }
    // One seat (or none): the areas are ignored, the seat gets the whole monitor.
    if mask.count() < 2 {
        return Ok(false);
    }
    if mask.count() as usize > MAX_SEATS_PER_MONITOR || options.areas.len() > MAX_SEATS_PER_MONITOR {
        return Err(SplitIssue::TooManySeats);
    }
    let mut seen = SeatMask::SPARE;
    for a in &options.areas {
        if !mask.contains(a.seat) || seen.contains(a.seat) {
            return Err(SplitIssue::AreaMismatch);
        }
        seen.insert(a.seat);
    }
    if seen != SeatMask(mask.0 & SeatMask::all_up_to(crate::ids::MAX_SEATS).0) {
        return Err(SplitIssue::AreaMismatch);
    }
    let mut used = 0u8;
    for a in &options.areas {
        if used & a.area.cells() != 0 {
            return Err(SplitIssue::Overlap);
        }
        used |= a.area.cells();
    }
    if !options.outputs.is_empty() {
        return Err(SplitIssue::CloneOnSplit);
    }
    if let Some(c) = options.areas.iter().find(|a| a.seat.is_console()) {
        let ok = options.areas.len() == 2
            && c.area.is_half()
            && options.areas.iter().any(|a| !a.seat.is_console() && Some(a.area) == c.area.opposite_half());
        if !ok {
            return Err(SplitIssue::ConsoleShareNotHalf);
        }
    }
    Ok(true)
}

/// `seat`'s area on a monitor with this assignment: `Some` only for a valid split monitor
/// that lists `seat`.
pub fn seat_area(kind: DeviceKind, mask: SeatMask, options: &AssignmentOptions, seat: SeatNumber) -> Option<ScreenArea> {
    if check_split(kind, mask, options) != Ok(true) {
        return None;
    }
    options.areas.iter().find(|a| a.seat == seat).map(|a| a.area)
}

/// Whether a monitor with effective mask `mask` is one of `seat`'s monitors: the seat's
/// alone, or a valid split monitor (explicit assignment with areas) that lists the seat.
pub fn monitor_shows_seat(config: &crate::config::Config, key: &crate::ids::DeviceKey, mask: SeatMask, seat: SeatNumber) -> bool {
    mask.single() == Some(seat)
        || config
            .devices
            .iter()
            .find(|d| &d.key == key)
            .is_some_and(|d| d.mask == mask && seat_area(d.kind, d.mask, &d.options, seat).is_some())
}

/// The seat areas of a monitor with effective mask `mask`: the explicit assignment's
/// areas when it is a valid split with that same mask, else empty (device views, kiosk
/// shell placement).
pub fn split_areas(config: &crate::config::Config, key: &crate::ids::DeviceKey, mask: SeatMask) -> Vec<SeatArea> {
    config
        .devices
        .iter()
        .find(|d| &d.key == key)
        .filter(|d| d.mask == mask && check_split(d.kind, d.mask, &d.options) == Ok(true))
        .map(|d| d.options.areas.clone())
        .unwrap_or_default()
}

/// The free part of `monitor` next to the seat surfaces `seats` (console clip, AppBar
/// home): the whole monitor when no seat covers it, the uncovered strip when the seats
/// cover one full half (a rectangle), else `None` (nothing rectangular left).
pub fn free_part(monitor: &Rect, seats: &[Rect]) -> Option<Rect> {
    let parts: Vec<Rect> = seats.iter().filter_map(|s| intersection(monitor, s)).collect();
    if parts.is_empty() {
        return Some(*monitor);
    }
    let bbox = bounding(&parts)?;
    let area: u64 = parts.iter().map(|r| r.width as u64 * r.height as u64).sum();
    if area != bbox.width as u64 * bbox.height as u64 {
        return None; // holes: the covered part is not one rectangle
    }
    let (mr, mb) = (monitor.x + monitor.width as i32, monitor.y + monitor.height as i32);
    let (br, bb) = (bbox.x + bbox.width as i32, bbox.y + bbox.height as i32);
    let full_w = bbox.x == monitor.x && br == mr;
    let full_h = bbox.y == monitor.y && bb == mb;
    if full_w && full_h {
        return None;
    }
    if full_w && bbox.y == monitor.y {
        return Some(Rect { x: monitor.x, y: bb, width: monitor.width, height: (mb - bb) as u32 });
    }
    if full_w && bb == mb {
        return Some(Rect { x: monitor.x, y: monitor.y, width: monitor.width, height: (bbox.y - monitor.y) as u32 });
    }
    if full_h && bbox.x == monitor.x {
        return Some(Rect { x: br, y: monitor.y, width: (mr - br) as u32, height: monitor.height });
    }
    if full_h && br == mr {
        return Some(Rect { x: monitor.x, y: monitor.y, width: (bbox.x - monitor.x) as u32, height: monitor.height });
    }
    None
}

fn intersection(a: &Rect, b: &Rect) -> Option<Rect> {
    let x0 = a.x.max(b.x);
    let y0 = a.y.max(b.y);
    let x1 = (a.x + a.width as i32).min(b.x + b.width as i32);
    let y1 = (a.y + a.height as i32).min(b.y + b.height as i32);
    (x1 > x0 && y1 > y0).then(|| Rect { x: x0, y: y0, width: (x1 - x0) as u32, height: (y1 - y0) as u32 })
}

fn bounding(r: &[Rect]) -> Option<Rect> {
    let x0 = r.iter().map(|r| r.x).min()?;
    let y0 = r.iter().map(|r| r.y).min()?;
    let x1 = r.iter().map(|r| r.x + r.width as i32).max()?;
    let y1 = r.iter().map(|r| r.y + r.height as i32).max()?;
    Some(Rect { x: x0, y: y0, width: (x1 - x0) as u32, height: (y1 - y0) as u32 })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(n: u8) -> SeatNumber {
        SeatNumber::new(n).unwrap()
    }

    fn opts(a: &[(u8, ScreenArea)]) -> AssignmentOptions {
        AssignmentOptions { areas: a.iter().map(|&(n, area)| SeatArea { seat: s(n), area }).collect(), ..Default::default() }
    }

    #[test]
    fn cells_and_rects() {
        for a in ScreenArea::ALL {
            assert_eq!(ScreenArea::from_cells(a.cells()), Some(a));
        }
        assert_eq!(ScreenArea::from_cells(CELL_TOP_LEFT | CELL_BOTTOM_RIGHT), None);
        assert!(ScreenArea::Left.overlaps(ScreenArea::TopLeft));
        assert!(!ScreenArea::Left.overlaps(ScreenArea::Right));
        let m = Rect { x: 1920, y: 0, width: 1921, height: 1081 };
        assert_eq!(ScreenArea::Left.rect_in(&m), Rect { x: 1920, y: 0, width: 960, height: 1081 });
        assert_eq!(ScreenArea::Right.rect_in(&m), Rect { x: 2880, y: 0, width: 961, height: 1081 });
        assert_eq!(ScreenArea::BottomRight.rect_in(&m), Rect { x: 2880, y: 540, width: 961, height: 541 });
        assert_eq!(ScreenArea::Full.rect_in(&m), m);
        assert_eq!(ScreenArea::Top.edge(), Some(Edge::Top));
        assert_eq!(ScreenArea::TopLeft.edge(), None);
        assert_eq!(serde_json::to_string(&ScreenArea::BottomLeft).unwrap(), "\"bottom_left\"");
    }

    #[test]
    fn split_rules() {
        let mon = DeviceKind::Monitor;
        let m23 = SeatMask::from_seats([s(2), s(3)]);
        assert_eq!(check_split(mon, m23, &AssignmentOptions::default()), Ok(false));
        assert_eq!(check_split(mon, m23, &opts(&[(2, ScreenArea::Left), (3, ScreenArea::Right)])), Ok(true));
        assert_eq!(check_split(mon, m23, &opts(&[(2, ScreenArea::Left), (3, ScreenArea::TopLeft)])), Err(SplitIssue::Overlap));
        assert_eq!(check_split(mon, m23, &opts(&[(2, ScreenArea::Left)])), Err(SplitIssue::AreaMismatch));
        assert_eq!(check_split(mon, m23, &opts(&[(2, ScreenArea::Left), (4, ScreenArea::Right)])), Err(SplitIssue::AreaMismatch));
        assert_eq!(check_split(DeviceKind::Keyboard, m23, &opts(&[(2, ScreenArea::Left)])), Err(SplitIssue::NotMonitor));
        assert_eq!(check_split(mon, SeatMask::SHARED, &opts(&[(2, ScreenArea::Left)])), Err(SplitIssue::Shared));
        // One seat: areas ignored.
        assert_eq!(check_split(mon, s(2).mask(), &opts(&[(2, ScreenArea::Left)])), Ok(false));
        assert_eq!(seat_area(mon, s(2).mask(), &opts(&[(2, ScreenArea::Left)]), s(2)), None);
        // Four quarters.
        let four = SeatMask::from_seats([s(2), s(3), s(4), s(5)]);
        let q = opts(&[(2, ScreenArea::TopLeft), (3, ScreenArea::TopRight), (4, ScreenArea::BottomLeft), (5, ScreenArea::BottomRight)]);
        assert_eq!(check_split(mon, four, &q), Ok(true));
        assert_eq!(seat_area(mon, four, &q, s(4)), Some(ScreenArea::BottomLeft));
        let five = SeatMask::from_seats([s(2), s(3), s(4), s(5), s(6)]);
        assert_eq!(check_split(mon, five, &q), Err(SplitIssue::TooManySeats));
        // Clone outputs are refused on a split monitor.
        let mut c = opts(&[(2, ScreenArea::Left), (3, ScreenArea::Right)]);
        c.outputs = vec![1, 2];
        assert_eq!(check_split(mon, m23, &c), Err(SplitIssue::CloneOnSplit));
        // Console share: halves only, exactly one other seat on the opposite half.
        let m12 = SeatMask::from_seats([s(1), s(2)]);
        assert_eq!(check_split(mon, m12, &opts(&[(1, ScreenArea::Left), (2, ScreenArea::Right)])), Ok(true));
        assert_eq!(check_split(mon, m12, &opts(&[(1, ScreenArea::TopLeft), (2, ScreenArea::Right)])), Err(SplitIssue::ConsoleShareNotHalf));
        let m123 = SeatMask::from_seats([s(1), s(2), s(3)]);
        let three = opts(&[(1, ScreenArea::Left), (2, ScreenArea::TopRight), (3, ScreenArea::BottomRight)]);
        assert_eq!(check_split(mon, m123, &three), Err(SplitIssue::ConsoleShareNotHalf));
    }

    #[test]
    fn split_monitor_belongs_to_each_listed_seat() {
        let mut c = crate::config::Config::default();
        let k = crate::ids::DeviceKey::new("mon;1").unwrap();
        let mask = SeatMask::from_seats([s(2), s(3)]);
        c.devices.push(crate::config::DeviceAssignment {
            key: k.clone(),
            kind: DeviceKind::Monitor,
            mask,
            name: String::new(),
            label: None,
            hardware_id: None,
            serial: None,
            container_id: None,
            options: opts(&[(2, ScreenArea::Left), (3, ScreenArea::Right)]),
        });
        assert!(monitor_shows_seat(&c, &k, mask, s(2)));
        assert!(monitor_shows_seat(&c, &k, mask, s(3)));
        assert!(!monitor_shows_seat(&c, &k, mask, s(4)));
        assert!(monitor_shows_seat(&c, &k, s(4).mask(), s(4)), "single seat");
        assert_eq!(split_areas(&c, &k, mask).len(), 2);
        assert!(split_areas(&c, &k, s(2).mask()).is_empty(), "effective mask differs from the assignment");
        c.devices[0].options.areas.clear();
        assert!(!monitor_shows_seat(&c, &k, mask, s(2)), "two seats without areas: nobody's");
        assert!(split_areas(&c, &k, mask).is_empty());
    }

    #[test]
    fn free_part_of_a_shared_monitor() {
        let m = Rect { x: 0, y: 0, width: 1920, height: 1080 };
        assert_eq!(free_part(&m, &[]), Some(m));
        assert_eq!(free_part(&m, &[Rect { x: 3000, y: 0, width: 10, height: 10 }]), Some(m));
        let right = ScreenArea::Right.rect_in(&m);
        assert_eq!(free_part(&m, &[right]), Some(ScreenArea::Left.rect_in(&m)));
        let top = [ScreenArea::TopLeft.rect_in(&m), ScreenArea::TopRight.rect_in(&m)];
        assert_eq!(free_part(&m, &top), Some(ScreenArea::Bottom.rect_in(&m)));
        assert_eq!(free_part(&m, &[ScreenArea::Bottom.rect_in(&m)]), Some(ScreenArea::Top.rect_in(&m)));
        assert_eq!(free_part(&m, &[ScreenArea::Left.rect_in(&m)]), Some(ScreenArea::Right.rect_in(&m)));
        // A quarter alone leaves an L shape; diagonal quarters leave holes; all covered.
        assert_eq!(free_part(&m, &[ScreenArea::TopLeft.rect_in(&m)]), None);
        assert_eq!(free_part(&m, &[ScreenArea::TopLeft.rect_in(&m), ScreenArea::BottomRight.rect_in(&m)]), None);
        assert_eq!(free_part(&m, &[m]), None);
    }
}
