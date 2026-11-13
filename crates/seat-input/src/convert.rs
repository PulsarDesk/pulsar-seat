// SPDX-License-Identifier: GPL-3.0-only
//! Device-neutral input processing (spec 50 §4.1.4, §4.1.9–§4.1.11).
//!
//! Both input paths end here:
//!
//! - **raw input** in the seat host (`RAWKEYBOARD` / `RAWMOUSE` / `RAWHID` fields), and
//! - **kernel filter** packets read by the service (`KEYBOARD_INPUT_DATA` /
//!   `MOUSE_INPUT_DATA` fields, which use the same flag values as raw input).
//!
//! A [`UnitProcessor`] per input unit turns [`RawUnitEvent`]s into
//! [`InputPacket`]s plus side events: the move-to-next-seat hotkey, the emergency
//! combination and the secure attention sequence (Ctrl+Alt+Del → [`InputPacket::Sas`]).
//! Everything here is pure and unit-tested; no Windows calls.

use seat_core::backend::input::{InputPacket, TouchContact, buttons, key_flags};
use std::collections::{BTreeMap, BTreeSet};

/// Set-1 scan codes used by the trackers.
pub mod sc {
    pub const LCTRL: u16 = 0x1D;
    pub const LALT: u16 = 0x38;
    pub const LSHIFT: u16 = 0x2A;
    pub const RSHIFT: u16 = 0x36;
    pub const DELETE: u16 = 0x53;
    pub const F12: u16 = 0x58;
}

/// Raw mouse flag values (`RAWMOUSE.usFlags` = `MOUSE_INPUT_DATA.Flags`).
pub mod mouse_flags {
    pub const MOVE_ABSOLUTE: u16 = 0x0001;
    pub const VIRTUAL_DESKTOP: u16 = 0x0002;
    pub const ATTRIBUTES_CHANGED: u16 = 0x0004;
}

/// Raw mouse button flag values (`RAWMOUSE.usButtonFlags` = `MOUSE_INPUT_DATA.ButtonFlags`).
pub mod button_flags {
    pub const LEFT_DOWN: u16 = 0x0001;
    pub const LEFT_UP: u16 = 0x0002;
    pub const RIGHT_DOWN: u16 = 0x0004;
    pub const RIGHT_UP: u16 = 0x0008;
    pub const MIDDLE_DOWN: u16 = 0x0010;
    pub const MIDDLE_UP: u16 = 0x0020;
    pub const X1_DOWN: u16 = 0x0040;
    pub const X1_UP: u16 = 0x0080;
    pub const X2_DOWN: u16 = 0x0100;
    pub const X2_UP: u16 = 0x0200;
    pub const WHEEL: u16 = 0x0400;
    pub const HWHEEL: u16 = 0x0800;
}

/// One contact of a digitizer report (touch screen finger or pen), normalized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawContact {
    pub id: u32,
    /// Tip switch (touching the surface).
    pub tip: bool,
    /// In range (pen hover).
    pub in_range: bool,
    /// 0..=65535 over the digitizer's logical range.
    pub x: u16,
    pub y: u16,
    /// 0..=1024.
    pub pressure: u16,
}

/// One input event of one unit, as read from raw input or from the kernel filter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RawUnitEvent {
    /// Scan code + raw flags (`RI_KEY_BREAK` 1, `RI_KEY_E0` 2, `RI_KEY_E1` 4; the kernel's
    /// `KEY_BREAK` / `KEY_E0` / `KEY_E1` have the same values).
    Key { sc: u16, flags: u16 },
    Mouse { flags: u16, buttons: u16, data: u16, x: i32, y: i32 },
    /// Consumer page (0x0C) usages pressed in this report.
    Consumer { usages: Vec<u16> },
    /// Generic desktop system-control usages (0x81 power, 0x82 sleep, 0x83 wake) pressed.
    System { usages: Vec<u16> },
    /// Digitizer frame: every contact the report carries.
    Digitizer { contacts: Vec<RawContact>, pen: bool },
    /// Precision touchpad frame (0x0D/0x05): contacts plus the pad's button (0x09/0x01).
    /// Delivered as a relative pointer (first finger moves, the button clicks).
    Touchpad { contacts: Vec<RawContact>, button: bool },
}

/// Normalized touchpad units (0..=65535 over the pad) per pointer count: a full swipe
/// across the pad moves about 2700 counts.
pub const TOUCHPAD_DIVISOR: i32 = 24;

/// Relative pointer from precision-touchpad frames: the lowest-id finger touching the pad
/// drives the motion; the pad button is the left button.
#[derive(Debug, Default, Clone)]
pub struct TouchpadTracker {
    /// Finger in control and its last position.
    lead: Option<(u32, u16, u16)>,
    /// Remainders below one count, so slow moves are not lost.
    rest: (i32, i32),
    button: bool,
}

impl TouchpadTracker {
    pub fn frame(&mut self, contacts: &[RawContact], button: bool) -> Vec<InputPacket> {
        let mut out = Vec::new();
        let touching = contacts.iter().filter(|c| c.tip).min_by_key(|c| c.id);
        match (self.lead, touching) {
            (Some((id, lx, ly)), Some(c)) if c.id == id => {
                let dx = self.rest.0 + (i32::from(c.x) - i32::from(lx));
                let dy = self.rest.1 + (i32::from(c.y) - i32::from(ly));
                let (mx, my) = (dx / TOUCHPAD_DIVISOR, dy / TOUCHPAD_DIVISOR);
                self.rest = (dx - mx * TOUCHPAD_DIVISOR, dy - my * TOUCHPAD_DIVISOR);
                if mx != 0 || my != 0 {
                    out.push(InputPacket::Move { dx: mx, dy: my });
                }
                self.lead = Some((id, c.x, c.y));
            }
            (_, Some(c)) => {
                // New lead finger: no jump.
                self.lead = Some((c.id, c.x, c.y));
                self.rest = (0, 0);
            }
            (_, None) => {
                self.lead = None;
                self.rest = (0, 0);
            }
        }
        if button != self.button {
            self.button = button;
            out.push(if button {
                InputPacket::Buttons { down: buttons::LEFT, up: 0 }
            } else {
                InputPacket::Buttons { down: 0, up: buttons::LEFT }
            });
        }
        out
    }
    pub fn release_all(&mut self) -> Vec<InputPacket> {
        self.lead = None;
        self.rest = (0, 0);
        if std::mem::take(&mut self.button) { vec![InputPacket::Buttons { down: 0, up: buttons::LEFT }] } else { Vec::new() }
    }
}

/// Sign-extend a HID logical value of `bits` bits (`HidP_GetUsageValue` returns the raw
/// field bits, no sign extension). `bits` 0 or >= 32 leaves the value as an i32.
pub fn sign_extend(v: u32, bits: u16) -> i64 {
    if bits == 0 || bits >= 32 {
        return i64::from(v as i32);
    }
    let shift = 32 - u32::from(bits);
    i64::from(((v << shift) as i32) >> shift)
}

/// Result of processing one event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Packet(InputPacket),
    /// Ctrl + the configured key: move this unit (and its seat's mice) to the next seat.
    Hotkey,
    /// Ctrl+Shift + the emergency key ([`Hotkey::emergency_key`]: the configured hotkey, F12
    /// when the hotkey is off) three times within [`EMERGENCY_WINDOW_MS`]: return every unit
    /// to seat 1 (spec 50 §4.1.9, "fixed emergency combination"; the combination is our
    /// choice because the spec leaves it open). Works with the move hotkey turned off.
    Emergency,
}

/// Emergency key when the move hotkey is off (F12, set 1).
pub const EMERGENCY_FALLBACK_KEY: Hotkey = Hotkey { sc: sc::F12, e0: false };

/// Window for the three emergency presses.
pub const EMERGENCY_WINDOW_MS: u64 = 3000;
/// Presses needed for the emergency combination.
pub const EMERGENCY_PRESSES: usize = 3;

/// Hotkey value as stored in [`seat_core::config::InputSettings::switch_hotkey_scan`]: a
/// set-1 scan code; `0xE0xx` marks an E0-extended key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hotkey {
    pub sc: u16,
    pub e0: bool,
}

impl Hotkey {
    pub fn from_setting(v: Option<u16>) -> Option<Self> {
        let v = v?;
        if v == 0 {
            return None;
        }
        if v & 0xFF00 == 0xE000 { Some(Self { sc: v & 0xFF, e0: true }) } else { Some(Self { sc: v, e0: false }) }
    }
    fn matches(self, sc: u16, e0: bool) -> bool {
        self.sc == sc && self.e0 == e0
    }
    /// Key of the emergency combination for a hotkey setting: the hotkey itself, or
    /// [`EMERGENCY_FALLBACK_KEY`] when the hotkey is off. Turning the move hotkey off never
    /// removes the way back to seat 1.
    pub fn emergency_key(hotkey: Option<Self>) -> Self {
        hotkey.unwrap_or(EMERGENCY_FALLBACK_KEY)
    }
}

/// Keyboard state of one unit: forwarding, hotkey, emergency and SAS detection, and the
/// key-ups needed to leave no key stuck.
#[derive(Debug, Default, Clone)]
pub struct KeyTracker {
    hotkey: Option<Hotkey>,
    /// Keys forwarded as down (scan code, E0).
    down: BTreeSet<(u16, bool)>,
    /// Keys whose make was swallowed (hotkey, Del of Ctrl+Alt+Del); their break is too.
    swallowed: BTreeSet<(u16, bool)>,
    emergency: Vec<u64>,
}

impl KeyTracker {
    pub fn new(hotkey: Option<Hotkey>) -> Self {
        Self { hotkey, ..Default::default() }
    }
    pub fn set_hotkey(&mut self, hotkey: Option<Hotkey>) {
        self.hotkey = hotkey;
    }
    fn held(&self, sc: u16) -> bool {
        self.down.contains(&(sc, false)) || self.down.contains(&(sc, true))
    }
    pub fn ctrl(&self) -> bool {
        self.held(sc::LCTRL)
    }
    pub fn alt(&self) -> bool {
        self.held(sc::LALT)
    }
    pub fn shift(&self) -> bool {
        self.down.contains(&(sc::LSHIFT, false)) || self.down.contains(&(sc::RSHIFT, false))
    }
    pub fn is_down(&self, sc: u16, e0: bool) -> bool {
        self.down.contains(&(sc, e0))
    }

    /// One key event (raw flags: 1 break, 2 E0, 4 E1).
    pub fn key(&mut self, scan: u16, raw_flags: u16, now_ms: u64) -> Vec<Outcome> {
        let up = raw_flags & 1 != 0;
        let e0 = raw_flags & 2 != 0;
        let e1 = raw_flags & 4 != 0;
        let id = (scan, e0);
        let mut f = 0u8;
        if up {
            f |= key_flags::UP;
        }
        if e0 {
            f |= key_flags::E0;
        }
        if e1 {
            f |= key_flags::E1;
        }
        if up {
            if self.swallowed.remove(&id) {
                return Vec::new();
            }
            self.down.remove(&id);
            return vec![Outcome::Packet(InputPacket::Key { sc: scan, f })];
        }
        // Auto-repeat of a swallowed key stays swallowed.
        if self.swallowed.contains(&id) {
            return Vec::new();
        }
        if !e1 && self.ctrl() && self.shift() && Hotkey::emergency_key(self.hotkey).matches(scan, e0) {
            // Ctrl+Shift+emergency key: an emergency press, never the move hotkey.
            self.swallowed.insert(id);
            self.emergency.retain(|t| now_ms.saturating_sub(*t) <= EMERGENCY_WINDOW_MS);
            self.emergency.push(now_ms);
            if self.emergency.len() >= EMERGENCY_PRESSES {
                self.emergency.clear();
                return vec![Outcome::Emergency];
            }
            return Vec::new();
        }
        if !e1 && self.hotkey.is_some_and(|h| h.matches(scan, e0)) && self.ctrl() {
            self.swallowed.insert(id);
            return vec![Outcome::Hotkey];
        }
        if scan == sc::DELETE && !e1 && self.ctrl() && self.alt() {
            self.swallowed.insert(id);
            return vec![Outcome::Packet(InputPacket::Sas)];
        }
        self.down.insert(id);
        vec![Outcome::Packet(InputPacket::Key { sc: scan, f })]
    }

    /// Key-ups for every key forwarded as down (service gap, seat switch).
    pub fn release_all(&mut self) -> Vec<InputPacket> {
        let out: Vec<InputPacket> = self
            .down
            .iter()
            .rev()
            .map(|(s, e0)| InputPacket::Key { sc: *s, f: key_flags::UP | if *e0 { key_flags::E0 } else { 0 } })
            .collect();
        self.down.clear();
        self.swallowed.clear();
        out
    }
}

/// Relative / absolute motion, buttons and wheel of one mouse event.
pub fn mouse_packets(flags: u16, bflags: u16, data: u16, x: i32, y: i32) -> Vec<InputPacket> {
    use button_flags as b;
    let mut out = Vec::new();
    if flags & mouse_flags::ATTRIBUTES_CHANGED == 0 {
        if flags & mouse_flags::MOVE_ABSOLUTE != 0 {
            out.push(InputPacket::Abs { x: x.clamp(0, 65535) as u16, y: y.clamp(0, 65535) as u16, o: 0 });
        } else if x != 0 || y != 0 {
            out.push(InputPacket::Move { dx: x, dy: y });
        }
    }
    let pairs = [
        (b::LEFT_DOWN, b::LEFT_UP, buttons::LEFT),
        (b::RIGHT_DOWN, b::RIGHT_UP, buttons::RIGHT),
        (b::MIDDLE_DOWN, b::MIDDLE_UP, buttons::MIDDLE),
        (b::X1_DOWN, b::X1_UP, buttons::X1),
        (b::X2_DOWN, b::X2_UP, buttons::X2),
    ];
    let (mut down, mut up) = (0u8, 0u8);
    for (d, u, bit) in pairs {
        if bflags & d != 0 {
            down |= bit;
        }
        if bflags & u != 0 {
            up |= bit;
        }
    }
    if down != 0 || up != 0 {
        out.push(InputPacket::Buttons { down, up });
    }
    if bflags & b::WHEEL != 0 {
        out.push(InputPacket::Wheel { d: data as i16, h: false });
    }
    if bflags & b::HWHEEL != 0 {
        out.push(InputPacket::Wheel { d: data as i16, h: true });
    }
    out
}

/// Consumer-page usage → set-1 scan code with E0 (Microsoft keyboard scan code
/// specification, public). Returns the low byte; every entry is E0-extended.
pub fn consumer_scan(usage: u16) -> Option<u16> {
    Some(match usage {
        0x00B5 => 0x19, // scan next track
        0x00B6 => 0x10, // scan previous track
        0x00B7 => 0x24, // stop
        0x00CD => 0x22, // play / pause
        0x00E2 => 0x20, // mute
        0x00E9 => 0x30, // volume up
        0x00EA => 0x2E, // volume down
        0x0183 => 0x6D, // media select
        0x018A => 0x6C, // mail
        0x0192 => 0x21, // calculator
        0x0194 => 0x6B, // my computer
        0x0221 => 0x65, // browser search
        0x0223 => 0x32, // browser home
        0x0224 => 0x6A, // browser back
        0x0225 => 0x69, // browser forward
        0x0226 => 0x68, // browser stop
        0x0227 => 0x67, // browser refresh
        0x022A => 0x66, // browser favorites
        _ => return None,
    })
}

/// Generic-desktop system control usage → set-1 scan code with E0.
pub fn system_scan(usage: u16) -> Option<u16> {
    Some(match usage {
        0x81 => 0x5E, // power
        0x82 => 0x5F, // sleep
        0x83 => 0x63, // wake
        _ => return None,
    })
}

/// Pressed-usage set of a consumer or system-control collection; reports are full state,
/// so a diff gives downs and ups.
#[derive(Debug, Default, Clone)]
pub struct UsageTracker {
    down: BTreeSet<u16>,
}

impl UsageTracker {
    /// `(usage, down)` changes against the previous report.
    pub fn update(&mut self, now: &[u16]) -> Vec<(u16, bool)> {
        let now: BTreeSet<u16> = now.iter().copied().filter(|u| *u != 0).collect();
        let mut out: Vec<(u16, bool)> = self.down.difference(&now).map(|u| (*u, false)).collect();
        out.extend(now.difference(&self.down).map(|u| (*u, true)));
        self.down = now;
        out
    }
    pub fn release_all(&mut self) -> Vec<u16> {
        std::mem::take(&mut self.down).into_iter().collect()
    }
}

/// Digitizer contact phases (down / update / up / hover) across reports.
#[derive(Debug, Default, Clone)]
pub struct TouchTracker {
    /// id → last position, for contacts that are down.
    down: BTreeMap<u32, (u16, u16)>,
}

impl TouchTracker {
    pub fn frame(&mut self, contacts: &[RawContact], pen: bool) -> Vec<TouchContact> {
        let mut out = Vec::new();
        let mut seen = BTreeSet::new();
        for c in contacts {
            seen.insert(c.id);
            let was = self.down.contains_key(&c.id);
            let phase = match (c.tip, was) {
                (true, false) => "down",
                (true, true) => "update",
                (false, true) => "up",
                (false, false) if pen && c.in_range => "hover",
                _ => continue,
            };
            if c.tip {
                self.down.insert(c.id, (c.x, c.y));
            } else {
                self.down.remove(&c.id);
            }
            out.push(TouchContact {
                id: c.id,
                x: c.x,
                y: c.y,
                phase: phase.into(),
                pressure: if pen { c.pressure.min(1024) } else { 0 },
                pen,
            });
        }
        // Pens report the contact every frame; touch screens may stop reporting a lifted
        // finger only in hybrid mode, so a down contact that is no longer listed in a pen
        // frame is lifted.
        if pen {
            let gone: Vec<u32> = self.down.keys().copied().filter(|id| !seen.contains(id)).collect();
            for id in gone {
                let (x, y) = self.down.remove(&id).unwrap_or_default();
                out.push(TouchContact { id, x, y, phase: "up".into(), pressure: 0, pen });
            }
        }
        out
    }
    /// Lift every contact that is down.
    pub fn release_all(&mut self, pen: bool) -> Vec<TouchContact> {
        std::mem::take(&mut self.down)
            .into_iter()
            .map(|(id, (x, y))| TouchContact { id, x, y, phase: "up".into(), pressure: 0, pen })
            .collect()
    }
}

/// Per-unit options.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UnitOptions {
    pub hotkey: Option<Hotkey>,
    /// Power / sleep / wake from this unit are forwarded (they act on the whole PC).
    pub allow_system_buttons: bool,
}

/// Everything one input unit needs between events.
#[derive(Debug, Default, Clone)]
pub struct UnitProcessor {
    pub opts: UnitOptions,
    keys: KeyTracker,
    buttons_down: u8,
    consumer: UsageTracker,
    system: UsageTracker,
    touch: TouchTracker,
    pen: bool,
    touchpad: TouchpadTracker,
}

impl UnitProcessor {
    pub fn new(opts: UnitOptions) -> Self {
        Self { opts, keys: KeyTracker::new(opts.hotkey), ..Default::default() }
    }
    pub fn set_options(&mut self, opts: UnitOptions) {
        self.opts = opts;
        self.keys.set_hotkey(opts.hotkey);
    }

    pub fn process(&mut self, ev: &RawUnitEvent, now_ms: u64) -> Vec<Outcome> {
        match ev {
            RawUnitEvent::Key { sc, flags } => self.keys.key(*sc, *flags, now_ms),
            RawUnitEvent::Mouse { flags, buttons, data, x, y } => {
                let p = mouse_packets(*flags, *buttons, *data, *x, *y);
                for pk in &p {
                    if let InputPacket::Buttons { down, up } = pk {
                        self.buttons_down = (self.buttons_down | down) & !up;
                    }
                }
                p.into_iter().map(Outcome::Packet).collect()
            }
            RawUnitEvent::Consumer { usages } => self
                .consumer
                .update(usages)
                .into_iter()
                .filter(|(u, _)| consumer_scan(*u).is_some())
                .map(|(usage, down)| Outcome::Packet(InputPacket::Consumer { usage, down }))
                .collect(),
            RawUnitEvent::System { usages } => {
                let changes = self.system.update(usages);
                if !self.opts.allow_system_buttons {
                    return Vec::new();
                }
                changes
                    .into_iter()
                    .filter_map(|(u, down)| {
                        let s = system_scan(u)?;
                        let f = key_flags::E0 | if down { 0 } else { key_flags::UP };
                        Some(Outcome::Packet(InputPacket::Key { sc: s, f }))
                    })
                    .collect()
            }
            RawUnitEvent::Digitizer { contacts, pen } => {
                self.pen = *pen;
                let c = self.touch.frame(contacts, *pen);
                if c.is_empty() { Vec::new() } else { vec![Outcome::Packet(InputPacket::Touch { o: 0, contacts: c })] }
            }
            RawUnitEvent::Touchpad { contacts, button } => {
                self.touchpad.frame(contacts, *button).into_iter().map(Outcome::Packet).collect()
            }
        }
    }

    /// Ups for everything this unit holds down.
    pub fn release_all(&mut self) -> Vec<InputPacket> {
        let mut out = self.keys.release_all();
        if self.buttons_down != 0 {
            out.push(InputPacket::Buttons { down: 0, up: self.buttons_down });
            self.buttons_down = 0;
        }
        for usage in self.consumer.release_all() {
            if consumer_scan(usage).is_some() {
                out.push(InputPacket::Consumer { usage, down: false });
            }
        }
        let held_system = self.system.release_all();
        if self.opts.allow_system_buttons {
            for u in held_system {
                if let Some(s) = system_scan(u) {
                    out.push(InputPacket::Key { sc: s, f: key_flags::E0 | key_flags::UP });
                }
            }
        }
        out.extend(self.touchpad.release_all());
        let lifted = self.touch.release_all(self.pen);
        if !lifted.is_empty() {
            out.push(InputPacket::Touch { o: 0, contacts: lifted });
        }
        out
    }
}

/// Split outcomes into packets and side events.
pub fn split(outcomes: Vec<Outcome>) -> (Vec<InputPacket>, bool, bool) {
    let mut packets = Vec::new();
    let (mut hotkey, mut emergency) = (false, false);
    for o in outcomes {
        match o {
            Outcome::Packet(p) => packets.push(p),
            Outcome::Hotkey => hotkey = true,
            Outcome::Emergency => emergency = true,
        }
    }
    (packets, hotkey, emergency)
}

/// Normalize a logical value to 0..=65535.
pub fn normalize(v: i64, min: i64, max: i64) -> u16 {
    if max <= min {
        return 0;
    }
    let v = v.clamp(min, max);
    (((v - min) * 65535) / (max - min)) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(t: &mut KeyTracker, scan: u16, flags: u16) -> Vec<Outcome> {
        t.key(scan, flags, 0)
    }

    #[test]
    fn plain_keys_forward_and_release() {
        let mut t = KeyTracker::new(None);
        assert_eq!(k(&mut t, 0x1E, 0), vec![Outcome::Packet(InputPacket::Key { sc: 0x1E, f: 0 })]);
        assert_eq!(k(&mut t, 0x48, 2), vec![Outcome::Packet(InputPacket::Key { sc: 0x48, f: key_flags::E0 })]);
        let ups = t.release_all();
        assert_eq!(
            ups,
            vec![
                InputPacket::Key { sc: 0x48, f: key_flags::UP | key_flags::E0 },
                InputPacket::Key { sc: 0x1E, f: key_flags::UP }
            ]
        );
        assert!(t.release_all().is_empty());
    }

    #[test]
    fn hotkey_is_swallowed_and_reported() {
        let mut t = KeyTracker::new(Hotkey::from_setting(Some(0x58)));
        k(&mut t, sc::LCTRL, 0);
        assert_eq!(k(&mut t, sc::F12, 0), vec![Outcome::Hotkey]);
        assert!(k(&mut t, sc::F12, 0).is_empty(), "auto-repeat swallowed");
        assert!(k(&mut t, sc::F12, 1).is_empty(), "break swallowed");
        // Without Ctrl F12 is a normal key.
        k(&mut t, sc::LCTRL, 1);
        assert_eq!(k(&mut t, sc::F12, 0), vec![Outcome::Packet(InputPacket::Key { sc: sc::F12, f: 0 })]);
    }

    #[test]
    fn hotkey_off_and_extended_setting() {
        assert_eq!(Hotkey::from_setting(None), None);
        assert_eq!(Hotkey::from_setting(Some(0)), None);
        assert_eq!(Hotkey::from_setting(Some(0xE047)), Some(Hotkey { sc: 0x47, e0: true }));
        let mut t = KeyTracker::new(Hotkey::from_setting(Some(0xE047)));
        k(&mut t, sc::LCTRL, 2); // right ctrl
        assert_eq!(k(&mut t, 0x47, 0).len(), 1, "numpad 7 is not the E0 key");
        assert_eq!(k(&mut t, 0x47, 2), vec![Outcome::Hotkey]);
    }

    #[test]
    fn emergency_needs_three_presses_in_window() {
        let mut t = KeyTracker::new(Hotkey::from_setting(Some(0x58)));
        t.key(sc::LCTRL, 0, 0);
        t.key(sc::LSHIFT, 0, 0);
        for i in 0..2 {
            assert!(t.key(sc::F12, 0, i * 100).is_empty());
            assert!(t.key(sc::F12, 1, i * 100 + 50).is_empty());
        }
        assert_eq!(t.key(sc::F12, 0, 300), vec![Outcome::Emergency]);
        t.key(sc::F12, 1, 350);
        // Presses too far apart do not count.
        t.key(sc::F12, 0, 10_000);
        t.key(sc::F12, 1, 10_001);
        t.key(sc::F12, 0, 20_000);
        t.key(sc::F12, 1, 20_001);
        assert!(t.key(sc::F12, 0, 30_000).is_empty());
    }

    #[test]
    fn emergency_works_with_the_hotkey_off() {
        let mut t = KeyTracker::new(None);
        t.key(sc::LCTRL, 0, 0);
        // Ctrl+F12 without Shift is a normal key when the hotkey is off.
        assert_eq!(t.key(sc::F12, 0, 1), vec![Outcome::Packet(InputPacket::Key { sc: sc::F12, f: 0 })]);
        t.key(sc::F12, 1, 2);
        t.key(sc::LSHIFT, 0, 3);
        assert!(t.key(sc::F12, 0, 10).is_empty());
        assert!(t.key(sc::F12, 1, 11).is_empty());
        assert!(t.key(sc::F12, 0, 20).is_empty());
        assert!(t.key(sc::F12, 1, 21).is_empty());
        assert_eq!(t.key(sc::F12, 0, 30), vec![Outcome::Emergency]);
        // With a configured hotkey the emergency key follows it.
        assert_eq!(Hotkey::emergency_key(Hotkey::from_setting(Some(0x44))), Hotkey { sc: 0x44, e0: false });
        assert_eq!(Hotkey::emergency_key(None), EMERGENCY_FALLBACK_KEY);
    }

    #[test]
    fn ctrl_alt_del_becomes_sas() {
        let mut t = KeyTracker::new(None);
        k(&mut t, sc::LCTRL, 0);
        k(&mut t, sc::LALT, 0);
        assert_eq!(k(&mut t, sc::DELETE, 2), vec![Outcome::Packet(InputPacket::Sas)]);
        assert!(k(&mut t, sc::DELETE, 3).is_empty());
        // Numpad Del (no E0) too.
        assert_eq!(k(&mut t, sc::DELETE, 0), vec![Outcome::Packet(InputPacket::Sas)]);
    }

    #[test]
    fn mouse_conversion() {
        use button_flags as b;
        assert_eq!(mouse_packets(0, 0, 0, 3, -2), vec![InputPacket::Move { dx: 3, dy: -2 }]);
        assert!(mouse_packets(0, 0, 0, 0, 0).is_empty());
        assert_eq!(
            mouse_packets(mouse_flags::MOVE_ABSOLUTE, b::LEFT_DOWN | b::RIGHT_UP, 0, 70_000, 100),
            vec![InputPacket::Abs { x: 65535, y: 100, o: 0 }, InputPacket::Buttons { down: buttons::LEFT, up: buttons::RIGHT }]
        );
        assert_eq!(mouse_packets(0, b::WHEEL, (-120i16) as u16, 0, 0), vec![InputPacket::Wheel { d: -120, h: false }]);
        assert_eq!(mouse_packets(0, b::HWHEEL, 120, 0, 0), vec![InputPacket::Wheel { d: 120, h: true }]);
        assert!(mouse_packets(mouse_flags::ATTRIBUTES_CHANGED, 0, 0, 5, 5).is_empty());
    }

    #[test]
    fn consumer_and_system_buttons() {
        let mut p = UnitProcessor::new(UnitOptions::default());
        let o = p.process(&RawUnitEvent::Consumer { usages: vec![0xE9] }, 0);
        assert_eq!(o, vec![Outcome::Packet(InputPacket::Consumer { usage: 0xE9, down: true })]);
        let o = p.process(&RawUnitEvent::Consumer { usages: vec![] }, 0);
        assert_eq!(o, vec![Outcome::Packet(InputPacket::Consumer { usage: 0xE9, down: false })]);
        // Unknown consumer usages are dropped.
        assert!(p.process(&RawUnitEvent::Consumer { usages: vec![0x1234] }, 0).is_empty());
        // System buttons are dropped by default.
        assert!(p.process(&RawUnitEvent::System { usages: vec![0x82] }, 0).is_empty());
        let mut p = UnitProcessor::new(UnitOptions { allow_system_buttons: true, ..Default::default() });
        assert_eq!(
            p.process(&RawUnitEvent::System { usages: vec![0x82] }, 0),
            vec![Outcome::Packet(InputPacket::Key { sc: 0x5F, f: key_flags::E0 })]
        );
        assert_eq!(p.release_all(), vec![InputPacket::Key { sc: 0x5F, f: key_flags::E0 | key_flags::UP }]);
    }

    #[test]
    fn touch_phases() {
        let mut t = TouchTracker::default();
        let c = |id, tip, x| RawContact { id, tip, in_range: true, x, y: 10, pressure: 500 };
        let f = t.frame(&[c(1, true, 5)], false);
        assert_eq!(f[0].phase, "down");
        let f = t.frame(&[c(1, true, 6), c(2, true, 7)], false);
        assert_eq!((f[0].phase.as_str(), f[1].phase.as_str()), ("update", "down"));
        let f = t.frame(&[c(1, false, 6)], false);
        assert_eq!(f[0].phase, "up");
        let lifted = t.release_all(false);
        assert_eq!(lifted.len(), 1);
        assert_eq!(lifted[0].id, 2);
        // Pen hover and implicit lift.
        let f = t.frame(&[c(9, false, 1)], true);
        assert_eq!(f[0].phase, "hover");
        assert_eq!(f[0].pressure, 500);
        t.frame(&[c(9, true, 1)], true);
        let f = t.frame(&[], true);
        assert_eq!(f[0].phase, "up");
    }

    #[test]
    fn processor_release_all_covers_everything() {
        let mut p = UnitProcessor::new(UnitOptions::default());
        p.process(&RawUnitEvent::Key { sc: 0x1E, flags: 0 }, 0);
        p.process(&RawUnitEvent::Mouse { flags: 0, buttons: button_flags::LEFT_DOWN, data: 0, x: 0, y: 0 }, 0);
        p.process(&RawUnitEvent::Consumer { usages: vec![0xCD] }, 0);
        let r = p.release_all();
        assert!(r.contains(&InputPacket::Key { sc: 0x1E, f: key_flags::UP }));
        assert!(r.contains(&InputPacket::Buttons { down: 0, up: buttons::LEFT }));
        assert!(r.contains(&InputPacket::Consumer { usage: 0xCD, down: false }));
        assert!(p.release_all().is_empty());
    }

    #[test]
    fn touchpad_is_relative() {
        let c = |id, tip, x, y| RawContact { id, tip, in_range: tip, x, y, pressure: 0 };
        let mut p = UnitProcessor::new(UnitOptions::default());
        assert!(p.process(&RawUnitEvent::Touchpad { contacts: vec![c(1, true, 1000, 1000)], button: false }, 0).is_empty());
        let o = p.process(&RawUnitEvent::Touchpad { contacts: vec![c(1, true, 1000 + 240, 1000 - 48)], button: false }, 0);
        assert_eq!(o, vec![Outcome::Packet(InputPacket::Move { dx: 10, dy: -2 })]);
        // A second finger landing does not jump; lifting the lead hands over without a jump.
        let o = p.process(&RawUnitEvent::Touchpad { contacts: vec![c(1, false, 0, 0), c(2, true, 9000, 9000)], button: true }, 0);
        assert_eq!(o, vec![Outcome::Packet(InputPacket::Buttons { down: buttons::LEFT, up: 0 })]);
        assert_eq!(p.release_all(), vec![InputPacket::Buttons { down: 0, up: buttons::LEFT }]);
    }

    #[test]
    fn sign_extension_by_bit_size() {
        assert_eq!(sign_extend(0xFFF, 12), -1);
        assert_eq!(sign_extend(0x800, 12), -2048);
        assert_eq!(sign_extend(0x7FF, 12), 2047);
        assert_eq!(sign_extend(0xFF, 8), -1);
        assert_eq!(sign_extend(0xFFFF_FFFF, 32), -1);
        assert_eq!(sign_extend(5, 0), 5);
    }

    #[test]
    fn normalize_range() {
        assert_eq!(normalize(0, 0, 4095), 0);
        assert_eq!(normalize(4095, 0, 4095), 65535);
        assert_eq!(normalize(-5, 0, 10), 0);
        assert_eq!(normalize(5, 5, 5), 0);
    }
}
