// SPDX-License-Identifier: GPL-3.0-only
//! [`InputSink`] for a seat host: turns [`InputPacket`]s into RDP input events
//! (spec 50 §4.1.4, §4.1.11).
//!
//! The seat host's RDP client (owned by the host package) implements
//! [`RdpInputChannel`]; this module only decides *what* to send:
//!
//! - keyboard → `TS_KEYBOARD_EVENT` (scan code + `KBDFLAGS_*`, MS-RDPBCGR 2.2.8.1.1.3.1.1.1);
//! - pointer → `TS_POINTER_EVENT` / `TS_POINTERX_EVENT` (absolute desktop coordinates; the
//!   sink integrates relative motion and clamps it to the session desktop);
//! - wheel → `PTRFLAGS_WHEEL` / `PTRFLAGS_HWHEEL` with the 9-bit two's-complement rotation;
//! - touch → an MS-RDPEI touch frame when the channel has the multitouch extension; pen →
//!   an MS-RDPEI pen frame (`RDPINPUT_PEN_FRAME`, protocol v3) when the channel also has
//!   pen support; otherwise the first contact emulates the pointer and the left button
//!   (pressure, eraser, hover and multi-touch are then lost: backend note
//!   `touch_emulated_as_mouse`; the ActiveX client of the seat host has neither today);
//! - consumer keys → E0 scan codes inside the session (never the console's volume);
//! - Ctrl+Alt+Del ([`InputPacket::Sas`]) → the Ctrl, Alt, Del scan codes, which the RDP
//!   server turns into the session's own secure attention (no policy change needed).
//!
//! The sink remembers what it pressed, so [`InputSink::release_all`] leaves nothing stuck.

use crate::convert::consumer_scan;
use seat_core::backend::input::{InputPacket, InputSink, TouchContact, buttons, key_flags};
use seat_core::backend::{BackendError, BackendResult, ErrorKind};
use seat_core::device::Rect;
use std::collections::BTreeSet;

/// `TS_KEYBOARD_EVENT.keyboardFlags`.
pub mod kbdflags {
    pub const EXTENDED: u16 = 0x0100;
    pub const EXTENDED1: u16 = 0x0200;
    pub const DOWN: u16 = 0x4000;
    pub const RELEASE: u16 = 0x8000;
}

/// `TS_POINTER_EVENT.pointerFlags` / `TS_POINTERX_EVENT.pointerFlags`.
pub mod ptrflags {
    pub const WHEEL_NEGATIVE: u16 = 0x0100;
    pub const WHEEL_ROTATION_MASK: u16 = 0x01FF;
    pub const WHEEL: u16 = 0x0200;
    pub const HWHEEL: u16 = 0x0400;
    pub const MOVE: u16 = 0x0800;
    pub const BUTTON1: u16 = 0x1000;
    pub const BUTTON2: u16 = 0x2000;
    pub const BUTTON3: u16 = 0x4000;
    pub const DOWN: u16 = 0x8000;
    /// `TS_POINTERX_EVENT`.
    pub const XDOWN: u16 = 0x8000;
    pub const XBUTTON1: u16 = 0x0001;
    pub const XBUTTON2: u16 = 0x0002;
}

/// MS-RDPEI `RDPINPUT_CONTACT_DATA.contactFlags`.
pub mod contact_flags {
    pub const DOWN: u32 = 0x0001;
    pub const UPDATE: u32 = 0x0002;
    pub const UP: u32 = 0x0004;
    pub const INRANGE: u32 = 0x0008;
    pub const INCONTACT: u32 = 0x0010;
    pub const CANCELED: u32 = 0x0020;
}

/// One contact of an MS-RDPEI frame in session-desktop pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RdpContact {
    pub id: u32,
    pub x: i32,
    pub y: i32,
    pub flags: u32,
    pub pen: bool,
    /// Pen pressure 0..=1024.
    pub pressure: u16,
}

/// What the seat host's RDP client sends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RdpInputEvent {
    /// `TS_KEYBOARD_EVENT` (slow path) / `TS_FP_KEYBOARD_EVENT` (fast path).
    Scancode { flags: u16, code: u8 },
    /// `TS_POINTER_EVENT`.
    Pointer { flags: u16, x: u16, y: u16 },
    /// `TS_POINTERX_EVENT` (buttons 4 and 5).
    PointerX { flags: u16, x: u16, y: u16 },
    /// MS-RDPEI touch frame (`RDPINPUT_TOUCH_FRAME`); `pen` is always false.
    Touch { contacts: Vec<RdpContact> },
    /// MS-RDPEI pen frame (`RDPINPUT_PEN_FRAME` / `RDPINPUT_PEN_CONTACT`, protocol v3);
    /// `pen` is always true, `pressure` 0..=1024.
    Pen { contacts: Vec<RdpContact> },
}

/// The seat host's RDP client connection.
pub trait RdpInputChannel: Send {
    fn send(&mut self, events: &[RdpInputEvent]) -> BackendResult<()>;
    /// Session desktop size in pixels (from the connection's desktop size).
    fn desktop_size(&self) -> (u16, u16);
    /// The server accepted the MS-RDPEI multitouch dynamic channel.
    fn supports_touch(&self) -> bool {
        false
    }
    /// The MS-RDPEI channel negotiated protocol v3 (pen frames). Only asked when
    /// [`RdpInputChannel::supports_touch`] is true.
    fn supports_pen(&self) -> bool {
        false
    }
}

/// [`InputSink`] over an [`RdpInputChannel`].
pub struct RdpInputSink<C: RdpInputChannel> {
    channel: C,
    /// Output rectangles in session-desktop coordinates, by output index (`Abs.o`).
    outputs: Vec<Rect>,
    pos: (i32, i32),
    keys: BTreeSet<(u8, u16)>,
    buttons_down: u8,
    touch_down: BTreeSet<(u32, bool)>,
    emulated_touch: Option<u32>,
}

impl<C: RdpInputChannel> RdpInputSink<C> {
    pub fn new(channel: C) -> Self {
        let (w, h) = channel.desktop_size();
        Self {
            channel,
            outputs: vec![Rect { x: 0, y: 0, width: w.into(), height: h.into() }],
            pos: (i32::from(w) / 2, i32::from(h) / 2),
            keys: BTreeSet::new(),
            buttons_down: 0,
            touch_down: BTreeSet::new(),
            emulated_touch: None,
        }
    }
    /// Output layout inside the session desktop (from the seat's `SeatOutputs`, in the order
    /// the host gave the monitors to the RDP client).
    pub fn set_outputs(&mut self, outputs: Vec<Rect>) {
        if !outputs.is_empty() {
            self.outputs = outputs;
        }
    }
    pub fn channel(&self) -> &C {
        &self.channel
    }
    pub fn channel_mut(&mut self) -> &mut C {
        &mut self.channel
    }

    fn bounds(&self) -> (i32, i32) {
        let (w, h) = self.channel.desktop_size();
        (i32::from(w).max(1) - 1, i32::from(h).max(1) - 1)
    }
    fn xy(&self) -> (u16, u16) {
        (self.pos.0.max(0) as u16, self.pos.1.max(0) as u16)
    }
    fn map_abs(&self, x: u16, y: u16, o: u8) -> (i32, i32) {
        let r = self.outputs.get(usize::from(o)).or(self.outputs.first()).copied().unwrap_or_default();
        let w = i64::from(r.width.max(1) - 1);
        let h = i64::from(r.height.max(1) - 1);
        (r.x + (i64::from(x) * w / 65535) as i32, r.y + (i64::from(y) * h / 65535) as i32)
    }
    fn move_to(&mut self, x: i32, y: i32, ev: &mut Vec<RdpInputEvent>) {
        let (mx, my) = self.bounds();
        self.pos = (x.clamp(0, mx), y.clamp(0, my));
        let (x, y) = self.xy();
        ev.push(RdpInputEvent::Pointer { flags: ptrflags::MOVE, x, y });
    }
    fn key(&mut self, sc: u16, f: u8, ev: &mut Vec<RdpInputEvent>) {
        let code = (sc & 0xFF) as u8;
        let mut ext = 0u16;
        if f & key_flags::E0 != 0 || sc & 0xFF00 == 0xE000 {
            ext |= kbdflags::EXTENDED;
        }
        if f & key_flags::E1 != 0 {
            ext |= kbdflags::EXTENDED1;
        }
        let up = f & key_flags::UP != 0;
        if up {
            self.keys.remove(&(code, ext));
        } else {
            self.keys.insert((code, ext));
        }
        ev.push(RdpInputEvent::Scancode { flags: ext | if up { kbdflags::RELEASE } else { 0 }, code });
    }
    fn button(&mut self, bit: u8, down: bool, ev: &mut Vec<RdpInputEvent>) {
        let (x, y) = self.xy();
        let d = if down { ptrflags::DOWN } else { 0 };
        let e = match bit {
            buttons::LEFT => RdpInputEvent::Pointer { flags: ptrflags::BUTTON1 | d, x, y },
            buttons::RIGHT => RdpInputEvent::Pointer { flags: ptrflags::BUTTON2 | d, x, y },
            buttons::MIDDLE => RdpInputEvent::Pointer { flags: ptrflags::BUTTON3 | d, x, y },
            buttons::X1 => RdpInputEvent::PointerX { flags: ptrflags::XBUTTON1 | if down { ptrflags::XDOWN } else { 0 }, x, y },
            buttons::X2 => RdpInputEvent::PointerX { flags: ptrflags::XBUTTON2 | if down { ptrflags::XDOWN } else { 0 }, x, y },
            _ => return,
        };
        if down {
            self.buttons_down |= bit;
        } else {
            self.buttons_down &= !bit;
        }
        ev.push(e);
    }
    fn wheel(&self, d: i16, h: bool, ev: &mut Vec<RdpInputEvent>) {
        let (x, y) = self.xy();
        let mut rest = i32::from(d);
        while rest != 0 {
            let step = rest.clamp(-256, 255);
            rest -= step;
            let rot = (step as i16 as u16) & ptrflags::WHEEL_ROTATION_MASK;
            let kind = if h { ptrflags::HWHEEL } else { ptrflags::WHEEL };
            ev.push(RdpInputEvent::Pointer { flags: kind | rot, x, y });
        }
    }
    fn touch(&mut self, o: u8, contacts: &[TouchContact], ev: &mut Vec<RdpInputEvent>) {
        let native_touch = self.channel.supports_touch();
        let native_pen = native_touch && self.channel.supports_pen();
        let (mut touch, mut pen, mut emulate) = (Vec::new(), Vec::new(), Vec::new());
        for c in contacts {
            let native = if c.pen { native_pen } else { native_touch };
            if !native {
                emulate.push(c);
                continue;
            }
            let (x, y) = self.map_abs(c.x, c.y, o);
            let flags = match c.phase.as_str() {
                "down" => contact_flags::DOWN | contact_flags::INRANGE | contact_flags::INCONTACT,
                "update" => contact_flags::UPDATE | contact_flags::INRANGE | contact_flags::INCONTACT,
                "hover" => contact_flags::UPDATE | contact_flags::INRANGE,
                _ => contact_flags::UP,
            };
            if flags & contact_flags::INCONTACT != 0 {
                self.touch_down.insert((c.id, c.pen));
            } else {
                self.touch_down.remove(&(c.id, c.pen));
            }
            let rc = RdpContact { id: c.id, x, y, flags, pen: c.pen, pressure: if c.pen { c.pressure } else { 0 } };
            if c.pen { pen.push(rc) } else { touch.push(rc) }
        }
        if !touch.is_empty() {
            ev.push(RdpInputEvent::Touch { contacts: touch });
        }
        if !pen.is_empty() {
            ev.push(RdpInputEvent::Pen { contacts: pen });
        }
        // Emulation: the first contact drives the pointer and the left button.
        for c in emulate {
            match self.emulated_touch {
                Some(id) if id != c.id => continue,
                None if c.phase != "down" && c.phase != "hover" => continue,
                _ => {}
            }
            let (x, y) = self.map_abs(c.x, c.y, o);
            self.move_to(x, y, ev);
            match c.phase.as_str() {
                "down" => {
                    self.emulated_touch = Some(c.id);
                    self.button(buttons::LEFT, true, ev);
                }
                "up" => {
                    self.emulated_touch = None;
                    self.button(buttons::LEFT, false, ev);
                }
                _ => {}
            }
        }
    }
    fn sas(&mut self, ev: &mut Vec<RdpInputEvent>) {
        let ctrl = self.keys.contains(&(0x1D, 0)) || self.keys.contains(&(0x1D, kbdflags::EXTENDED));
        let alt = self.keys.contains(&(0x38, 0)) || self.keys.contains(&(0x38, kbdflags::EXTENDED));
        if !ctrl {
            ev.push(RdpInputEvent::Scancode { flags: 0, code: 0x1D });
        }
        if !alt {
            ev.push(RdpInputEvent::Scancode { flags: 0, code: 0x38 });
        }
        ev.push(RdpInputEvent::Scancode { flags: kbdflags::EXTENDED, code: 0x53 });
        ev.push(RdpInputEvent::Scancode { flags: kbdflags::EXTENDED | kbdflags::RELEASE, code: 0x53 });
        if !alt {
            ev.push(RdpInputEvent::Scancode { flags: kbdflags::RELEASE, code: 0x38 });
        }
        if !ctrl {
            ev.push(RdpInputEvent::Scancode { flags: kbdflags::RELEASE, code: 0x1D });
        }
    }

    /// Events for one batch (exposed for tests and for hosts that batch themselves).
    pub fn translate(&mut self, packets: &[InputPacket]) -> Vec<RdpInputEvent> {
        let mut ev = Vec::new();
        for p in packets {
            match p {
                InputPacket::Key { sc, f } => self.key(*sc, *f, &mut ev),
                InputPacket::Move { dx, dy } => {
                    let (x, y) = (self.pos.0.saturating_add(*dx), self.pos.1.saturating_add(*dy));
                    self.move_to(x, y, &mut ev);
                }
                InputPacket::Abs { x, y, o } => {
                    let (x, y) = self.map_abs(*x, *y, *o);
                    self.move_to(x, y, &mut ev);
                }
                InputPacket::Buttons { down, up } => {
                    for bit in [buttons::LEFT, buttons::RIGHT, buttons::MIDDLE, buttons::X1, buttons::X2] {
                        if up & bit != 0 {
                            self.button(bit, false, &mut ev);
                        }
                        if down & bit != 0 {
                            self.button(bit, true, &mut ev);
                        }
                    }
                }
                InputPacket::Wheel { d, h } => self.wheel(*d, *h, &mut ev),
                InputPacket::Touch { o, contacts } => self.touch(*o, contacts, &mut ev),
                InputPacket::Consumer { usage, down } => {
                    if let Some(sc) = consumer_scan(*usage) {
                        let f = key_flags::E0 | if *down { 0 } else { key_flags::UP };
                        self.key(sc, f, &mut ev);
                    }
                }
                InputPacket::Sas => self.sas(&mut ev),
            }
        }
        ev
    }

    /// Events that release everything this sink pressed.
    pub fn release_events(&mut self) -> Vec<RdpInputEvent> {
        let mut ev = Vec::new();
        let keys: Vec<(u8, u16)> = self.keys.iter().rev().copied().collect();
        for (code, ext) in keys {
            ev.push(RdpInputEvent::Scancode { flags: ext | kbdflags::RELEASE, code });
        }
        self.keys.clear();
        for bit in [buttons::LEFT, buttons::RIGHT, buttons::MIDDLE, buttons::X1, buttons::X2] {
            if self.buttons_down & bit != 0 {
                self.button(bit, false, &mut ev);
            }
        }
        self.emulated_touch = None;
        if !self.touch_down.is_empty() {
            let (x, y) = self.pos;
            let (pen, touch): (Vec<RdpContact>, Vec<RdpContact>) = std::mem::take(&mut self.touch_down)
                .into_iter()
                .map(|(id, pen)| RdpContact { id, x, y, flags: contact_flags::CANCELED | contact_flags::UP, pen, pressure: 0 })
                .partition(|c| c.pen);
            if !touch.is_empty() {
                ev.push(RdpInputEvent::Touch { contacts: touch });
            }
            if !pen.is_empty() {
                ev.push(RdpInputEvent::Pen { contacts: pen });
            }
        }
        ev
    }
}

impl<C: RdpInputChannel> InputSink for RdpInputSink<C> {
    fn send(&mut self, packets: &[InputPacket]) -> BackendResult<()> {
        let ev = self.translate(packets);
        if ev.is_empty() {
            return Ok(());
        }
        self.channel.send(&ev)
    }
    fn release_all(&mut self) -> BackendResult<()> {
        let ev = self.release_events();
        if ev.is_empty() {
            return Ok(());
        }
        self.channel.send(&ev)
    }
}

/// A channel that only records (tests, dry runs) or forwards to a closure.
pub struct FnChannel<F: FnMut(&[RdpInputEvent]) -> BackendResult<()> + Send> {
    pub f: F,
    pub size: (u16, u16),
    pub touch: bool,
    pub pen: bool,
}

impl<F: FnMut(&[RdpInputEvent]) -> BackendResult<()> + Send> RdpInputChannel for FnChannel<F> {
    fn send(&mut self, events: &[RdpInputEvent]) -> BackendResult<()> {
        (self.f)(events)
    }
    fn desktop_size(&self) -> (u16, u16) {
        self.size
    }
    fn supports_touch(&self) -> bool {
        self.touch
    }
    fn supports_pen(&self) -> bool {
        self.pen
    }
}

/// Error used by channels whose connection is gone.
pub fn channel_closed() -> BackendError {
    BackendError::new(ErrorKind::NotReady, "rdp_channel_closed", "RDP input channel is not connected")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    type Log = Arc<Mutex<Vec<RdpInputEvent>>>;

    fn sink(touch: bool) -> (RdpInputSink<impl RdpInputChannel>, Log) {
        sink_pen(touch, false)
    }

    fn sink_pen(touch: bool, pen: bool) -> (RdpInputSink<impl RdpInputChannel>, Log) {
        let log: Log = Arc::default();
        let l = log.clone();
        let ch = FnChannel {
            f: move |e: &[RdpInputEvent]| {
                l.lock().unwrap().extend_from_slice(e);
                Ok(())
            },
            size: (1920, 1080),
            touch,
            pen,
        };
        (RdpInputSink::new(ch), log)
    }

    #[test]
    fn keys_and_release() {
        let (mut s, log) = sink(false);
        s.send(&[InputPacket::Key { sc: 0x1E, f: 0 }, InputPacket::Key { sc: 0x48, f: key_flags::E0 }]).unwrap();
        s.release_all().unwrap();
        let l = log.lock().unwrap();
        assert_eq!(
            *l,
            vec![
                RdpInputEvent::Scancode { flags: 0, code: 0x1E },
                RdpInputEvent::Scancode { flags: kbdflags::EXTENDED, code: 0x48 },
                RdpInputEvent::Scancode { flags: kbdflags::EXTENDED | kbdflags::RELEASE, code: 0x48 },
                RdpInputEvent::Scancode { flags: kbdflags::RELEASE, code: 0x1E },
            ]
        );
    }

    #[test]
    fn pointer_is_clamped_and_absolute() {
        let (mut s, _) = sink(false);
        let ev = s.translate(&[InputPacket::Move { dx: -5000, dy: 5000 }]);
        assert_eq!(ev, vec![RdpInputEvent::Pointer { flags: ptrflags::MOVE, x: 0, y: 1079 }]);
        let ev = s.translate(&[InputPacket::Abs { x: 65535, y: 0, o: 0 }]);
        assert_eq!(ev, vec![RdpInputEvent::Pointer { flags: ptrflags::MOVE, x: 1919, y: 0 }]);
        s.set_outputs(vec![Rect { x: 0, y: 0, width: 960, height: 1080 }, Rect { x: 960, y: 0, width: 960, height: 1080 }]);
        let ev = s.translate(&[InputPacket::Abs { x: 0, y: 0, o: 1 }]);
        assert_eq!(ev, vec![RdpInputEvent::Pointer { flags: ptrflags::MOVE, x: 960, y: 0 }]);
    }

    #[test]
    fn buttons_and_wheel() {
        let (mut s, _) = sink(false);
        s.translate(&[InputPacket::Abs { x: 0, y: 0, o: 0 }]);
        let ev = s.translate(&[InputPacket::Buttons { down: buttons::LEFT | buttons::X1, up: 0 }]);
        assert_eq!(
            ev,
            vec![
                RdpInputEvent::Pointer { flags: ptrflags::BUTTON1 | ptrflags::DOWN, x: 0, y: 0 },
                RdpInputEvent::PointerX { flags: ptrflags::XBUTTON1 | ptrflags::XDOWN, x: 0, y: 0 },
            ]
        );
        let ev = s.translate(&[InputPacket::Wheel { d: -120, h: false }]);
        assert_eq!(ev, vec![RdpInputEvent::Pointer { flags: ptrflags::WHEEL | ptrflags::WHEEL_NEGATIVE | 0x88, x: 0, y: 0 }]);
        let ev = s.translate(&[InputPacket::Wheel { d: 600, h: true }]);
        assert_eq!(ev.len(), 3, "split into 9-bit steps");
        let rel = s.release_events();
        assert_eq!(rel.len(), 2);
    }

    #[test]
    fn sas_and_consumer() {
        let (mut s, _) = sink(false);
        let ev = s.translate(&[InputPacket::Sas]);
        assert_eq!(ev.len(), 6);
        assert_eq!(ev[2], RdpInputEvent::Scancode { flags: kbdflags::EXTENDED, code: 0x53 });
        // With Ctrl and Alt already down only Del is sent.
        s.translate(&[InputPacket::Key { sc: 0x1D, f: 0 }, InputPacket::Key { sc: 0x38, f: 0 }]);
        assert_eq!(s.translate(&[InputPacket::Sas]).len(), 2);
        let ev = s.translate(&[InputPacket::Consumer { usage: 0xE9, down: true }]);
        assert_eq!(ev, vec![RdpInputEvent::Scancode { flags: kbdflags::EXTENDED, code: 0x30 }]);
    }

    #[test]
    fn touch_native_and_emulated() {
        let c = |phase: &str| TouchContact { id: 4, x: 32768, y: 32768, phase: phase.into(), pressure: 0, pen: false };
        let (mut s, _) = sink(true);
        let ev = s.translate(&[InputPacket::Touch { o: 0, contacts: vec![c("down")] }]);
        let RdpInputEvent::Touch { contacts } = &ev[0] else { panic!() };
        assert_eq!(contacts[0].flags, contact_flags::DOWN | contact_flags::INRANGE | contact_flags::INCONTACT);
        assert_eq!((contacts[0].x, contacts[0].y), (959, 539));
        let rel = s.release_events();
        assert!(matches!(&rel[0], RdpInputEvent::Touch { contacts } if contacts[0].flags & contact_flags::CANCELED != 0));

        let (mut s, _) = sink(false);
        let ev = s.translate(&[InputPacket::Touch { o: 0, contacts: vec![c("down")] }]);
        assert_eq!(ev[1], RdpInputEvent::Pointer { flags: ptrflags::BUTTON1 | ptrflags::DOWN, x: 959, y: 539 });
        let ev = s.translate(&[InputPacket::Touch { o: 0, contacts: vec![c("up")] }]);
        assert_eq!(ev[1], RdpInputEvent::Pointer { flags: ptrflags::BUTTON1, x: 959, y: 539 });
    }

    #[test]
    fn pen_frames_are_separate() {
        let p = |phase: &str| TouchContact { id: 7, x: 0, y: 0, phase: phase.into(), pressure: 512, pen: true };
        // Touch-capable channel without pen frames: the pen is emulated.
        let (mut s, _) = sink_pen(true, false);
        let ev = s.translate(&[InputPacket::Touch { o: 0, contacts: vec![p("down")] }]);
        assert!(ev.iter().all(|e| !matches!(e, RdpInputEvent::Touch { .. } | RdpInputEvent::Pen { .. })));
        assert!(ev.contains(&RdpInputEvent::Pointer { flags: ptrflags::BUTTON1 | ptrflags::DOWN, x: 0, y: 0 }));
        // With pen frames: one Pen event, pressure kept.
        let (mut s, _) = sink_pen(true, true);
        let ev = s.translate(&[InputPacket::Touch { o: 0, contacts: vec![p("down")] }]);
        let [RdpInputEvent::Pen { contacts }] = ev.as_slice() else { panic!("{ev:?}") };
        assert_eq!((contacts[0].pressure, contacts[0].pen), (512, true));
        let rel = s.release_events();
        assert!(matches!(&rel[0], RdpInputEvent::Pen { contacts } if contacts[0].flags & contact_flags::CANCELED != 0));
    }
}
