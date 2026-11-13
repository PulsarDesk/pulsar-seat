// SPDX-License-Identifier: GPL-3.0-only
//! Input: `seat_input::rdp::RdpInputEvent`s (what the seat's `RdpInputSink` decided to
//! send) into IronRDP PDUs (pure).
//!
//! - keyboard → fast-path scan code events (`KBDFLAGS_*` → fast-path flags);
//! - pointer / pointer-X → fast-path mouse events (the 9-bit wheel rotation moves out of
//!   the flags into the signed rotation field);
//! - relative motion → fast-path relative mouse events (`TS_FP_RELPOINTER_EVENT`, RDP
//!   10.12), split into `i16` steps;
//! - touch / pen → MS-RDPEI touch / pen frames (contact flags normalised to one of the
//!   legal combinations of MS-RDPEI 2.2.3.3.1.1).

use ironrdp_pdu::input::fast_path::{FastPathInputEvent, KeyboardFlags};
use ironrdp_pdu::input::mouse::PointerFlags;
use ironrdp_pdu::input::mouse_rel::PointerRelFlags;
use ironrdp_pdu::input::mouse_x::PointerXFlags;
use ironrdp_pdu::input::{MousePdu, MouseRelPdu, MouseXPdu};
use ironrdp_rdpei::pdu::{PenContact, PenContactFlags, PenEventPdu, PenFrame, TouchContact, TouchContactFlags, TouchEventPdu, TouchFrame};
use seat_input::rdp::{RdpContact, RdpInputEvent, kbdflags, ptrflags};

/// One translated item, in input order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outgoing {
    FastPath(Vec<FastPathInputEvent>),
    Touch(TouchEventPdu),
    Pen(PenEventPdu),
}

fn key_flags(f: u16) -> KeyboardFlags {
    let mut k = KeyboardFlags::empty();
    if f & kbdflags::RELEASE != 0 {
        k |= KeyboardFlags::RELEASE;
    }
    if f & kbdflags::EXTENDED != 0 {
        k |= KeyboardFlags::EXTENDED;
    }
    if f & kbdflags::EXTENDED1 != 0 {
        k |= KeyboardFlags::EXTENDED1;
    }
    k
}

/// Signed wheel rotation from the 9-bit field in `flags`.
pub fn wheel_units(flags: u16) -> i16 {
    let raw = (flags & ptrflags::WHEEL_ROTATION_MASK) as i16;
    if flags & ptrflags::WHEEL_NEGATIVE != 0 { raw - 0x200 } else { raw }
}

fn mouse(flags: u16, x: u16, y: u16) -> FastPathInputEvent {
    let wheel = flags & (ptrflags::WHEEL | ptrflags::HWHEEL) != 0;
    let units = if wheel { wheel_units(flags).clamp(*MousePdu::WHEEL_ROTATION_RANGE.start(), *MousePdu::WHEEL_ROTATION_RANGE.end()) } else { 0 };
    FastPathInputEvent::MouseEvent(MousePdu {
        flags: PointerFlags::from_bits_retain(flags & !ptrflags::WHEEL_ROTATION_MASK),
        number_of_wheel_rotation_units: units,
        x_position: x,
        y_position: y,
    })
}

fn legal_touch(flags: u32) -> TouchContactFlags {
    let f = TouchContactFlags::from_bits_truncate(flags);
    if f.is_legal() {
        return f;
    }
    if f.contains(TouchContactFlags::DOWN) {
        TouchContactFlags::DOWN | TouchContactFlags::INRANGE | TouchContactFlags::INCONTACT
    } else if f.contains(TouchContactFlags::UP) {
        if f.contains(TouchContactFlags::CANCELED) { TouchContactFlags::UP | TouchContactFlags::CANCELED } else { TouchContactFlags::UP }
    } else if f.contains(TouchContactFlags::INCONTACT) {
        TouchContactFlags::UPDATE | TouchContactFlags::INRANGE | TouchContactFlags::INCONTACT
    } else {
        TouchContactFlags::UPDATE | TouchContactFlags::INRANGE
    }
}

/// MS-RDPEI contact ids (one byte, below the contact count the client announced) for the
/// seat's raw contact ids. A slot is held from the first event of a contact until its UP;
/// a contact that finds no free slot is dropped.
#[derive(Debug, Clone)]
pub struct ContactSlots {
    slots: Vec<Option<u32>>,
}

impl ContactSlots {
    pub fn new(max: u16) -> Self {
        Self { slots: vec![None; usize::from(max.clamp(1, 256))] }
    }

    fn slot(&mut self, id: u32, up: bool) -> Option<u8> {
        let i = match self.slots.iter().position(|s| *s == Some(id)) {
            Some(i) => i,
            None if up => return None,
            None => {
                let i = self.slots.iter().position(Option::is_none)?;
                self.slots[i] = Some(id);
                i
            }
        };
        if up {
            self.slots[i] = None;
        }
        u8::try_from(i).ok()
    }

    /// Forget every contact (new connection).
    pub fn clear(&mut self) {
        self.slots.iter_mut().for_each(|s| *s = None);
    }
}

fn touch_contact(c: &RdpContact, slots: &mut ContactSlots) -> Option<TouchContact> {
    let flags = legal_touch(c.flags);
    let id = slots.slot(c.id, flags.contains(TouchContactFlags::UP))?;
    Some(TouchContact::new(id, c.x, c.y, flags))
}

fn pen_contact(c: &RdpContact) -> PenContact {
    let flags = PenContactFlags::from_bits_truncate(legal_touch(c.flags).bits());
    // Without multipen injection every pen uses device id 0 (MS-RDPEI 2.2.3.7.1.1).
    PenContact::new(0, c.x, c.y, flags).with_pressure(u32::from(c.pressure.min(1024)))
}

/// Translate in order; consecutive fast-path events share one batch.
pub fn translate(events: &[RdpInputEvent], slots: &mut ContactSlots) -> Vec<Outgoing> {
    let mut out: Vec<Outgoing> = Vec::new();
    let mut fp: Vec<FastPathInputEvent> = Vec::new();
    let flush = |fp: &mut Vec<FastPathInputEvent>, out: &mut Vec<Outgoing>| {
        if !fp.is_empty() {
            out.push(Outgoing::FastPath(std::mem::take(fp)));
        }
    };
    for e in events {
        match e {
            RdpInputEvent::Scancode { flags, code } => fp.push(FastPathInputEvent::KeyboardEvent(key_flags(*flags), *code)),
            RdpInputEvent::Pointer { flags, x, y } => fp.push(mouse(*flags, *x, *y)),
            RdpInputEvent::PointerX { flags, x, y } => fp.push(FastPathInputEvent::MouseEventEx(MouseXPdu {
                flags: PointerXFlags::from_bits_retain(*flags),
                x_position: *x,
                y_position: *y,
            })),
            RdpInputEvent::Touch { contacts } => {
                flush(&mut fp, &mut out);
                let c: Vec<TouchContact> = contacts.iter().filter_map(|c| touch_contact(c, slots)).collect();
                if !c.is_empty() {
                    out.push(Outgoing::Touch(TouchEventPdu::new(0, vec![TouchFrame::new(0, c)])));
                }
            }
            RdpInputEvent::Pen { contacts } => {
                flush(&mut fp, &mut out);
                if let Some(first) = contacts.first() {
                    // One pen (device id 0): only the first contact of a frame.
                    out.push(Outgoing::Pen(PenEventPdu::new(0, vec![PenFrame::new(0, vec![pen_contact(first)])])));
                }
            }
        }
    }
    flush(&mut fp, &mut out);
    out
}

/// A mouse button (seat `buttons` bit) as a `TS_FP_RELPOINTER_EVENT` with zero motion, for
/// clicks while the pointer is captured (an absolute event would warp the pointer).
pub fn relative_button(bit: u8, down: bool) -> Option<FastPathInputEvent> {
    use seat_core::backend::input::buttons;
    let mut flags = match bit {
        buttons::LEFT => PointerRelFlags::BUTTON1,
        buttons::RIGHT => PointerRelFlags::BUTTON2,
        buttons::MIDDLE => PointerRelFlags::BUTTON3,
        buttons::X1 => PointerRelFlags::XBUTTON1,
        buttons::X2 => PointerRelFlags::XBUTTON2,
        _ => return None,
    };
    if down {
        flags |= PointerRelFlags::DOWN;
    }
    Some(FastPathInputEvent::MouseEventRel(MouseRelPdu { flags, x_delta: 0, y_delta: 0 }))
}

/// Relative motion as `TS_FP_RELPOINTER_EVENT`s (each delta fits an `i16`).
pub fn relative(dx: i32, dy: i32) -> Vec<FastPathInputEvent> {
    let mut v = Vec::new();
    let (mut rx, mut ry) = (dx, dy);
    while rx != 0 || ry != 0 {
        let sx = rx.clamp(i32::from(i16::MIN), i32::from(i16::MAX));
        let sy = ry.clamp(i32::from(i16::MIN), i32::from(i16::MAX));
        v.push(FastPathInputEvent::MouseEventRel(MouseRelPdu { flags: PointerRelFlags::MOVE, x_delta: sx as i16, y_delta: sy as i16 }));
        rx -= sx;
        ry -= sy;
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use seat_input::rdp::contact_flags;

    #[test]
    fn keys_and_pointer() {
        let out = translate(&[
            RdpInputEvent::Scancode { flags: kbdflags::RELEASE | kbdflags::EXTENDED, code: 0x4B },
            RdpInputEvent::Pointer { flags: ptrflags::MOVE, x: 10, y: 20 },
            RdpInputEvent::Pointer { flags: ptrflags::WHEEL | ptrflags::WHEEL_NEGATIVE | (0x200 - 120), x: 1, y: 2 },
            RdpInputEvent::PointerX { flags: ptrflags::XDOWN | ptrflags::XBUTTON1, x: 3, y: 4 },
        ], &mut ContactSlots::new(10));
        assert_eq!(out.len(), 1);
        let Outgoing::FastPath(ev) = &out[0] else { panic!() };
        assert_eq!(ev[0], FastPathInputEvent::KeyboardEvent(KeyboardFlags::RELEASE | KeyboardFlags::EXTENDED, 0x4B));
        assert_eq!(
            ev[1],
            FastPathInputEvent::MouseEvent(MousePdu { flags: PointerFlags::MOVE, number_of_wheel_rotation_units: 0, x_position: 10, y_position: 20 })
        );
        let FastPathInputEvent::MouseEvent(w) = &ev[2] else { panic!() };
        assert_eq!(w.number_of_wheel_rotation_units, -120);
        assert_eq!(w.flags, PointerFlags::VERTICAL_WHEEL);
        let FastPathInputEvent::MouseEventEx(x) = &ev[3] else { panic!() };
        assert_eq!(x.flags, PointerXFlags::DOWN | PointerXFlags::BUTTON1);
        assert_eq!(wheel_units(ptrflags::WHEEL | 120), 120);
    }

    #[test]
    fn touch_and_pen_frames_split_batches() {
        let c = |flags, pen| RdpContact { id: 300, x: 5, y: 6, flags, pen, pressure: 2000 };
        let out = translate(&[
            RdpInputEvent::Scancode { flags: 0, code: 0x1E },
            RdpInputEvent::Touch { contacts: vec![c(contact_flags::DOWN | contact_flags::INRANGE | contact_flags::INCONTACT, false)] },
            RdpInputEvent::Pen { contacts: vec![c(contact_flags::DOWN, true)] },
            RdpInputEvent::Scancode { flags: kbdflags::RELEASE, code: 0x1E },
        ], &mut ContactSlots::new(10));
        assert_eq!(out.len(), 4);
        let Outgoing::Touch(t) = &out[1] else { panic!() };
        let tc = &t.frames[0].contacts[0];
        assert_eq!(tc.contact_id, 0, "first free slot");
        assert!(tc.contact_flags.is_legal());
        let Outgoing::Pen(p) = &out[2] else { panic!() };
        let pc = &p.frames[0].contacts[0];
        assert_eq!(pc.device_id, 0);
        // A bare DOWN is normalised to DOWN|INRANGE|INCONTACT; pressure capped at 1024.
        assert!(pc.contact_flags.is_legal());
        assert_eq!(pc.fields.pressure, Some(1024));
    }

    #[test]
    fn contact_ids_get_distinct_slots() {
        let mut slots = ContactSlots::new(2);
        let c = |id, flags| RdpContact { id, x: 1, y: 1, flags, pen: false, pressure: 0 };
        let down = contact_flags::DOWN | contact_flags::INRANGE | contact_flags::INCONTACT;
        // 44 and 300 collide under `& 0xFF`; slots keep them apart.
        let out = translate(&[RdpInputEvent::Touch { contacts: vec![c(44, down), c(300, down), c(7, down)] }], &mut slots);
        let Outgoing::Touch(t) = &out[0] else { panic!() };
        let ids: Vec<u8> = t.frames[0].contacts.iter().map(|c| c.contact_id).collect();
        assert_eq!(ids, vec![0, 1], "no third slot: contact 7 is dropped");
        // UP frees the slot for the next contact.
        translate(&[RdpInputEvent::Touch { contacts: vec![c(44, contact_flags::UP)] }], &mut slots);
        let out = translate(&[RdpInputEvent::Touch { contacts: vec![c(7, down)] }], &mut slots);
        let Outgoing::Touch(t) = &out[0] else { panic!() };
        assert_eq!(t.frames[0].contacts[0].contact_id, 0);
        // An UP for an unknown contact sends nothing.
        assert!(translate(&[RdpInputEvent::Touch { contacts: vec![c(99, contact_flags::UP)] }], &mut slots).is_empty());
    }

    #[test]
    fn relative_buttons_have_no_motion() {
        use seat_core::backend::input::buttons;
        let FastPathInputEvent::MouseEventRel(e) = relative_button(buttons::LEFT, true).unwrap() else { panic!() };
        assert_eq!((e.flags, e.x_delta, e.y_delta), (PointerRelFlags::BUTTON1 | PointerRelFlags::DOWN, 0, 0));
        let FastPathInputEvent::MouseEventRel(e) = relative_button(buttons::X2, false).unwrap() else { panic!() };
        assert_eq!(e.flags, PointerRelFlags::XBUTTON2);
        assert!(relative_button(0x40, true).is_none());
    }

    #[test]
    fn illegal_flags_are_normalised() {
        assert_eq!(legal_touch(contact_flags::UP | contact_flags::INCONTACT), TouchContactFlags::UP);
        assert_eq!(legal_touch(contact_flags::INCONTACT), TouchContactFlags::UPDATE | TouchContactFlags::INRANGE | TouchContactFlags::INCONTACT);
        assert_eq!(legal_touch(0), TouchContactFlags::UPDATE | TouchContactFlags::INRANGE);
        assert_eq!(legal_touch(contact_flags::UPDATE), TouchContactFlags::UPDATE);
    }

    #[test]
    fn relative_motion_is_split() {
        assert!(relative(0, 0).is_empty());
        let v = relative(40_000, -5);
        assert_eq!(v.len(), 2);
        let FastPathInputEvent::MouseEventRel(a) = v[0] else { panic!() };
        assert_eq!((a.x_delta, a.y_delta), (i16::MAX, -5));
        let FastPathInputEvent::MouseEventRel(b) = v[1] else { panic!() };
        assert_eq!((b.x_delta, b.y_delta), ((40_000 - i32::from(i16::MAX)) as i16, 0));
    }
}
