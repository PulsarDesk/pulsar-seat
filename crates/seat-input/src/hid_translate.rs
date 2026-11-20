// SPDX-License-Identifier: GPL-3.0-only
//! Raw HID input reports of a captured USB interface → [`RawUnitEvent`]s (osms G2).
//!
//! The `winusb_capture` reader gets report bytes from the interrupt-IN pipe; this module
//! decodes them with the interface's [`ReportLayout`] and produces the same events raw
//! input would: keys as set-1 scan codes (diff of the pressed set), mouse motion / buttons
//! / wheel, consumer and system-control usages (full state). From there the shared
//! [`crate::convert::UnitProcessor`] does hotkey, emergency, SAS and key-up bookkeeping.
//!
//! Pure and unit-tested. Written from the HID 1.11 spec and the Keyboard Scan Code
//! Specification only.

use crate::convert::{RawUnitEvent, button_flags, mouse_flags};
use crate::hid_keys::{self, PAUSE};
use crate::hid_layout::{AC_PAN, Field, ReportLayout, gd, page, value};
use std::collections::BTreeSet;

/// One notch of wheel in Windows units (`WHEEL_DELTA`).
pub const WHEEL_DELTA: i64 = 120;

/// Keyboard usage `ErrorRollOver` (too many keys): the report carries no key state.
const ERROR_ROLL_OVER: u16 = 0x01;

fn split(ext: u32) -> (u16, u16) {
    ((ext >> 16) as u16, (ext & 0xFFFF) as u16)
}

/// Stateful decoder of one captured interface.
#[derive(Debug, Clone)]
pub struct ReportTranslator {
    layout: ReportLayout,
    keys: BTreeSet<u16>,
    /// Button bits 0..4 (HID buttons 1..5).
    buttons: u8,
}

/// What one report said, before the diff.
#[derive(Debug, Default)]
struct Decoded {
    keys: Option<BTreeSet<u16>>,
    rollover: bool,
    buttons: Option<u8>,
    rel: (i64, i64),
    abs: (Option<u16>, Option<u16>),
    wheel: i64,
    pan: i64,
    consumer: Option<Vec<u16>>,
    system: Option<Vec<u16>>,
}

fn normalize(v: i64, f: &Field) -> u16 {
    let (lo, hi) = (i64::from(f.logical_min), i64::from(f.logical_max));
    if hi <= lo {
        return 0;
    }
    (((v.clamp(lo, hi) - lo) * 65535) / (hi - lo)) as u16
}

impl ReportTranslator {
    pub fn new(layout: ReportLayout) -> Self {
        Self { layout, keys: BTreeSet::new(), buttons: 0 }
    }

    pub fn layout(&self) -> &ReportLayout {
        &self.layout
    }

    /// Whether the layout has keyboard keys (hotkey attribution, LED follow-up).
    pub fn has_keys(&self) -> bool {
        self.layout.inputs.iter().any(|f| self.field_pages(f).contains(&page::KEYBOARD))
    }

    fn field_pages(&self, f: &Field) -> Vec<u16> {
        let mut p: Vec<u16> = f.usages.iter().map(|u| (u >> 16) as u16).collect();
        if let Some((lo, _)) = f.range {
            p.push((lo >> 16) as u16);
        }
        p
    }

    fn decode(&self, report: &[u8]) -> Decoded {
        let (id, body) = if self.layout.uses_report_ids {
            match report.split_first() {
                Some((id, rest)) => (*id, rest),
                None => return Decoded::default(),
            }
        } else {
            (0, report)
        };
        let mut d = Decoded::default();
        for f in self.layout.inputs.iter().filter(|f| f.report_id == id && !f.is_constant()) {
            if f.is_variable() {
                for i in 0..f.count {
                    let Some(ext) = f.usage_at(i) else { continue };
                    let v = value(f, body, i);
                    self.variable(&mut d, f, ext, v);
                }
            } else {
                for i in 0..f.count {
                    let v = value(f, body, i);
                    let usage = f.usage_for_index(v);
                    self.array(&mut d, f, usage);
                }
            }
        }
        d
    }

    fn variable(&self, d: &mut Decoded, f: &Field, ext: u32, v: i64) {
        match split(ext) {
            (page::KEYBOARD, u) => {
                let set = d.keys.get_or_insert_with(BTreeSet::new);
                if v != 0 && u > 3 {
                    set.insert(u);
                }
            }
            (page::BUTTON, u @ 1..=5) => {
                let b = d.buttons.get_or_insert(0);
                if v != 0 {
                    *b |= 1 << (u - 1);
                }
            }
            (page::GENERIC_DESKTOP, gd::X) => {
                if f.is_relative() {
                    d.rel.0 += v;
                } else {
                    d.abs.0 = Some(normalize(v, f));
                }
            }
            (page::GENERIC_DESKTOP, gd::Y) => {
                if f.is_relative() {
                    d.rel.1 += v;
                } else {
                    d.abs.1 = Some(normalize(v, f));
                }
            }
            (page::GENERIC_DESKTOP, gd::WHEEL) => d.wheel += v,
            (page::GENERIC_DESKTOP, u @ 0x81..=0x83) => {
                let s = d.system.get_or_insert_with(Vec::new);
                if v != 0 {
                    s.push(u);
                }
            }
            (page::CONSUMER, AC_PAN) => d.pan += v,
            (page::CONSUMER, u) => {
                let c = d.consumer.get_or_insert_with(Vec::new);
                if v != 0 {
                    c.push(u);
                }
            }
            _ => {}
        }
    }

    fn array(&self, d: &mut Decoded, f: &Field, usage: Option<u32>) {
        // The field's page decides which state the report carries, even when every slot
        // is empty (all keys up).
        let field_page = self.field_pages(f).first().copied();
        match field_page {
            Some(page::KEYBOARD) => {
                let set = d.keys.get_or_insert_with(BTreeSet::new);
                if let Some((page::KEYBOARD, u)) = usage.map(split) {
                    if u == ERROR_ROLL_OVER {
                        d.rollover = true;
                    } else if u > 3 {
                        set.insert(u);
                    }
                }
            }
            Some(page::CONSUMER) => {
                let c = d.consumer.get_or_insert_with(Vec::new);
                if let Some((page::CONSUMER, u)) = usage.map(split) {
                    if u != 0 && u != AC_PAN {
                        c.push(u);
                    }
                }
            }
            Some(page::GENERIC_DESKTOP) => {
                let s = d.system.get_or_insert_with(Vec::new);
                if let Some((page::GENERIC_DESKTOP, u @ 0x81..=0x83)) = usage.map(split) {
                    s.push(u);
                }
            }
            _ => {}
        }
    }

    /// Events of one input report (report id first when the layout uses ids).
    pub fn translate(&mut self, report: &[u8]) -> Vec<RawUnitEvent> {
        let d = self.decode(report);
        let mut out = Vec::new();
        if let Some(now) = d.keys.filter(|_| !d.rollover) {
            self.diff_keys(&now, &mut out);
        }
        let mut bflags = 0u16;
        if let Some(b) = d.buttons {
            let pairs = [
                (button_flags::LEFT_DOWN, button_flags::LEFT_UP),
                (button_flags::RIGHT_DOWN, button_flags::RIGHT_UP),
                (button_flags::MIDDLE_DOWN, button_flags::MIDDLE_UP),
                (button_flags::X1_DOWN, button_flags::X1_UP),
                (button_flags::X2_DOWN, button_flags::X2_UP),
            ];
            for (i, (down, up)) in pairs.iter().enumerate() {
                let bit = 1u8 << i;
                match (self.buttons & bit != 0, b & bit != 0) {
                    (false, true) => bflags |= down,
                    (true, false) => bflags |= up,
                    _ => {}
                }
            }
            self.buttons = b;
        }
        let (mflags, x, y) = match d.abs {
            (Some(x), Some(y)) => (mouse_flags::MOVE_ABSOLUTE, i32::from(x), i32::from(y)),
            _ => (0, d.rel.0.clamp(i32::MIN.into(), i32::MAX.into()) as i32, d.rel.1.clamp(i32::MIN.into(), i32::MAX.into()) as i32),
        };
        if bflags != 0 || x != 0 || y != 0 || mflags != 0 {
            out.push(RawUnitEvent::Mouse { flags: mflags, buttons: bflags, data: 0, x, y });
        }
        if d.wheel != 0 {
            let delta = (d.wheel * WHEEL_DELTA).clamp(i16::MIN.into(), i16::MAX.into()) as i16;
            out.push(RawUnitEvent::Mouse { flags: 0, buttons: button_flags::WHEEL, data: delta as u16, x: 0, y: 0 });
        }
        if d.pan != 0 {
            let delta = (d.pan * WHEEL_DELTA).clamp(i16::MIN.into(), i16::MAX.into()) as i16;
            out.push(RawUnitEvent::Mouse { flags: 0, buttons: button_flags::HWHEEL, data: delta as u16, x: 0, y: 0 });
        }
        if let Some(usages) = d.consumer {
            out.push(RawUnitEvent::Consumer { usages });
        }
        if let Some(usages) = d.system {
            out.push(RawUnitEvent::System { usages });
        }
        out
    }

    fn diff_keys(&mut self, now: &BTreeSet<u16>, out: &mut Vec<RawUnitEvent>) {
        let is_mod = |u: &u16| (0xE0..=0xE7).contains(u);
        // Releases first (non-modifiers before modifiers), then presses (modifiers first),
        // so Ctrl+key arrives as Ctrl down, key down like raw input.
        let mut ups: Vec<u16> = self.keys.difference(now).copied().collect();
        ups.sort_by_key(|u| is_mod(u));
        let mut downs: Vec<u16> = now.difference(&self.keys).copied().collect();
        downs.sort_by_key(|u| !is_mod(u));
        for u in ups {
            if let Some((sc, f)) = hid_keys::usage_to_scan(u) {
                out.push(RawUnitEvent::Key { sc, flags: f | 1 });
            }
        }
        for u in downs {
            if u == PAUSE {
                // Raw input reports Pause as E1 1D then 45, make and break at once.
                out.push(RawUnitEvent::Key { sc: 0x1D, flags: hid_keys::E1 });
                out.push(RawUnitEvent::Key { sc: 0x45, flags: 0 });
                out.push(RawUnitEvent::Key { sc: 0x1D, flags: hid_keys::E1 | 1 });
                out.push(RawUnitEvent::Key { sc: 0x45, flags: 1 });
            } else if let Some((sc, f)) = hid_keys::usage_to_scan(u) {
                out.push(RawUnitEvent::Key { sc, flags: f });
            }
        }
        self.keys = now.clone();
    }

    /// Forget all state (device reopened): the next report starts from "nothing held".
    /// Held keys in the seat are released by the caller's `UnitProcessor::release_all`.
    pub fn reset(&mut self) {
        self.keys.clear();
        self.buttons = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hid_layout::tests::{KEYBOARD_DESC, MOUSE_DESC};
    use crate::hid_layout::{boot_keyboard, boot_mouse, parse};

    fn key(sc: u16, flags: u16) -> RawUnitEvent {
        RawUnitEvent::Key { sc, flags }
    }

    #[test]
    fn keyboard_reports_diff_into_scan_codes() {
        let mut t = ReportTranslator::new(parse(KEYBOARD_DESC).unwrap());
        assert!(t.has_keys());
        // Left Ctrl + a.
        assert_eq!(t.translate(&[0x01, 0, 0x04, 0, 0, 0, 0, 0]), vec![key(0x1D, 0), key(0x1E, 0)]);
        // Same state again: nothing.
        assert!(t.translate(&[0x01, 0, 0x04, 0, 0, 0, 0, 0]).is_empty());
        // a released, Delete (E0) pressed.
        assert_eq!(t.translate(&[0x01, 0, 0x4C, 0, 0, 0, 0, 0]), vec![key(0x1E, 1), key(0x53, 2)]);
        // Rollover: state kept.
        assert!(t.translate(&[0x01, 0, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01]).is_empty());
        // Everything up: non-modifiers first.
        assert_eq!(t.translate(&[0, 0, 0, 0, 0, 0, 0, 0]), vec![key(0x53, 3), key(0x1D, 1)]);
    }

    #[test]
    fn pause_is_make_and_break_at_once() {
        let mut t = ReportTranslator::new(boot_keyboard());
        let ev = t.translate(&[0, 0, 0x48, 0, 0, 0, 0, 0]);
        assert_eq!(ev, vec![key(0x1D, 4), key(0x45, 0), key(0x1D, 5), key(0x45, 1)]);
        assert!(t.translate(&[0, 0, 0, 0, 0, 0, 0, 0]).is_empty(), "no second break");
    }

    #[test]
    fn mouse_reports_with_ids() {
        let mut t = ReportTranslator::new(parse(MOUSE_DESC).unwrap());
        assert!(!t.has_keys());
        // id 1: left button, X = -2, Y = 300, wheel -1, pan 0.
        let ev = t.translate(&[1, 0x01, 0xFE, 0xFF, 0x2C, 0x01, 0xFF, 0x00]);
        assert_eq!(
            ev,
            vec![
                RawUnitEvent::Mouse { flags: 0, buttons: button_flags::LEFT_DOWN, data: 0, x: -2, y: 300 },
                RawUnitEvent::Mouse { flags: 0, buttons: button_flags::WHEEL, data: (-120i16) as u16, x: 0, y: 0 },
            ]
        );
        // Button released, pan +1.
        let ev = t.translate(&[1, 0x00, 0, 0, 0, 0, 0, 0x01]);
        assert_eq!(
            ev,
            vec![
                RawUnitEvent::Mouse { flags: 0, buttons: button_flags::LEFT_UP, data: 0, x: 0, y: 0 },
                RawUnitEvent::Mouse { flags: 0, buttons: button_flags::HWHEEL, data: 120, x: 0, y: 0 },
            ]
        );
        // id 2: consumer volume up, then nothing.
        assert_eq!(t.translate(&[2, 0xE9, 0x00]), vec![RawUnitEvent::Consumer { usages: vec![0xE9] }]);
        assert_eq!(t.translate(&[2, 0x00, 0x00]), vec![RawUnitEvent::Consumer { usages: vec![] }]);
        // Unknown report id or empty report: nothing.
        assert!(t.translate(&[9, 1, 2]).is_empty());
        assert!(t.translate(&[]).is_empty());
    }

    #[test]
    fn boot_mouse_buttons() {
        let mut t = ReportTranslator::new(boot_mouse());
        assert_eq!(
            t.translate(&[0x06, 0x05, 0xFB]),
            vec![RawUnitEvent::Mouse { flags: 0, buttons: button_flags::RIGHT_DOWN | button_flags::MIDDLE_DOWN, data: 0, x: 5, y: -5 }]
        );
        t.reset();
        assert_eq!(t.translate(&[0x02, 0, 0]), vec![RawUnitEvent::Mouse { flags: 0, buttons: button_flags::RIGHT_DOWN, data: 0, x: 0, y: 0 }]);
    }

    #[test]
    fn absolute_pointer_is_normalized() {
        // GD X/Y absolute, 0..4095, 16 bits each.
        let d = [0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x00, 0x26, 0xFF, 0x0F, 0x75, 0x10, 0x95, 0x02, 0x81, 0x02, 0xC0];
        let mut t = ReportTranslator::new(parse(&d).unwrap());
        assert_eq!(
            t.translate(&[0xFF, 0x0F, 0x00, 0x00]),
            vec![RawUnitEvent::Mouse { flags: mouse_flags::MOVE_ABSOLUTE, buttons: 0, data: 0, x: 65535, y: 0 }]
        );
    }

    #[test]
    fn system_control_usages() {
        // GD system control, array of one 2-bit slot over usages 0x81..0x83.
        let d = [
            0x05, 0x01, 0x09, 0x80, 0xA1, 0x01, 0x19, 0x81, 0x29, 0x83, 0x15, 0x01, 0x25, 0x03, 0x75, 0x02, 0x95, 0x01, 0x81, 0x00, 0x75, 0x06, 0x81, 0x03,
            0xC0,
        ];
        let mut t = ReportTranslator::new(parse(&d).unwrap());
        assert_eq!(t.translate(&[0x02]), vec![RawUnitEvent::System { usages: vec![0x82] }]);
        assert_eq!(t.translate(&[0x00]), vec![RawUnitEvent::System { usages: vec![] }]);
    }
}
