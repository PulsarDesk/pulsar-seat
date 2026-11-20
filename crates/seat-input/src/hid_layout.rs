// SPDX-License-Identifier: GPL-3.0-only
//! HID report-descriptor parser for the `winusb_capture` reader (spec 50 §4, osms G2).
//!
//! A captured USB keyboard / mouse interface runs on the inbox WinUSB driver, so Windows'
//! HID class driver no longer parses its reports for us (`HidP_*` needs a HID collection).
//! The service reads the interface's report descriptor (`GET_DESCRIPTOR`, type 0x22) and
//! this module turns it into a [`ReportLayout`]: every input field with its report id, bit
//! offset, size, count, usages and logical range.
//!
//! Written from the public *Device Class Definition for HID 1.11* (§6.2.2 items, §8
//! report format) and the *HID Usage Tables*; no other implementation was used. Pure and
//! unit-tested. Long items are skipped, `Push` / `Pop` are honoured, delimiters ignored.

/// Main item flag bits (HID 1.11 §6.2.2.5).
pub mod field_flags {
    pub const CONSTANT: u32 = 1 << 0;
    pub const VARIABLE: u32 = 1 << 1;
    pub const RELATIVE: u32 = 1 << 2;
}

/// Usage pages the reader understands.
pub mod page {
    pub const GENERIC_DESKTOP: u16 = 0x01;
    pub const KEYBOARD: u16 = 0x07;
    pub const LED: u16 = 0x08;
    pub const BUTTON: u16 = 0x09;
    pub const CONSUMER: u16 = 0x0C;
}

/// Generic-desktop usages the reader understands.
pub mod gd {
    pub const POINTER: u16 = 0x01;
    pub const MOUSE: u16 = 0x02;
    pub const KEYBOARD: u16 = 0x06;
    pub const X: u16 = 0x30;
    pub const Y: u16 = 0x31;
    pub const WHEEL: u16 = 0x38;
    pub const SYSTEM_CONTROL: u16 = 0x80;
}

/// Consumer AC Pan (horizontal wheel).
pub const AC_PAN: u16 = 0x0238;

/// One input field (one main item).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    /// 0 when the descriptor uses no report ids.
    pub report_id: u8,
    /// Bit offset in the report **after** the report id byte.
    pub bit_offset: u32,
    pub size: u32,
    pub count: u32,
    pub flags: u32,
    /// Extended usages (page << 16 | usage), one per element; for array fields the list
    /// the index selects from. Empty for padding.
    pub usages: Vec<u32>,
    /// Usage minimum / maximum (extended) when given as a range.
    pub range: Option<(u32, u32)>,
    pub logical_min: i32,
    pub logical_max: i32,
}

impl Field {
    pub fn is_constant(&self) -> bool {
        self.flags & field_flags::CONSTANT != 0
    }
    pub fn is_variable(&self) -> bool {
        self.flags & field_flags::VARIABLE != 0
    }
    pub fn is_relative(&self) -> bool {
        self.flags & field_flags::RELATIVE != 0
    }
    /// Values are signed when the logical minimum is negative (HID 1.11 §6.2.2.7).
    pub fn signed(&self) -> bool {
        self.logical_min < 0
    }

    /// Extended usage of element `i` of a variable field (the last usage repeats, §6.2.2.8).
    pub fn usage_at(&self, i: u32) -> Option<u32> {
        if let Some((lo, hi)) = self.range {
            let u = lo.checked_add(i)?;
            return Some(u.min(hi));
        }
        let i = usize::try_from(i).ok()?;
        self.usages.get(i).or_else(|| self.usages.last()).copied()
    }

    /// Extended usage an array field's value selects (`None` = no key / out of range).
    pub fn usage_for_index(&self, value: i64) -> Option<u32> {
        let idx = value - i64::from(self.logical_min);
        if idx < 0 || value > i64::from(self.logical_max) {
            return None;
        }
        if let Some((lo, hi)) = self.range {
            let u = i64::from(lo) + idx;
            return (u <= i64::from(hi)).then_some(u as u32);
        }
        self.usages.get(usize::try_from(idx).ok()?).copied()
    }
}

/// Top-level application collection (usage page and usage).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Application {
    pub page: u16,
    pub usage: u16,
}

/// Parsed descriptor: the input fields plus the application collections.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReportLayout {
    pub inputs: Vec<Field>,
    /// Output fields (keyboard LEDs; used by the P1 LED follow-up, kept for completeness).
    pub outputs: Vec<Field>,
    pub applications: Vec<Application>,
    pub uses_report_ids: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    Truncated,
    /// More than 64 nested `Push` items.
    StackOverflow,
    /// Report size × count over 64 KiB bits, or a field past the report id range.
    Unreasonable,
    Empty,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Truncated => "report descriptor truncated",
            Self::StackOverflow => "report descriptor nests Push too deep",
            Self::Unreasonable => "report descriptor field size not plausible",
            Self::Empty => "report descriptor has no input fields",
        })
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct Globals {
    page: u16,
    logical_min: i32,
    logical_max: i32,
    size: u32,
    count: u32,
    report_id: u8,
}

#[derive(Debug, Default)]
struct Locals {
    usages: Vec<u32>,
    min: Option<u32>,
    max: Option<u32>,
}

impl Locals {
    fn extend(&self, page: u16, v: u32, size: usize) -> u32 {
        // A 4-byte usage carries its own page in the high half (§6.2.2.8).
        if size == 4 { v } else { (u32::from(page) << 16) | (v & 0xFFFF) }
    }
}

fn unsigned(data: &[u8]) -> u32 {
    data.iter().rev().fold(0u32, |a, b| (a << 8) | u32::from(*b))
}

fn signed(data: &[u8]) -> i32 {
    match data.len() {
        0 => 0,
        1 => i32::from(data[0] as i8),
        2 => i32::from(i16::from_le_bytes([data[0], data[1]])),
        _ => i32::from_le_bytes([data[0], data[1], data[2], data[3]]),
    }
}

/// Parse a report descriptor.
pub fn parse(desc: &[u8]) -> Result<ReportLayout, ParseError> {
    let mut out = ReportLayout::default();
    let mut g = Globals::default();
    let mut stack: Vec<Globals> = Vec::new();
    let mut l = Locals::default();
    // Bit cursor per (report id) for input and output reports.
    let mut in_bits: std::collections::HashMap<u8, u32> = std::collections::HashMap::new();
    let mut out_bits: std::collections::HashMap<u8, u32> = std::collections::HashMap::new();
    let mut depth = 0u32;
    let mut i = 0usize;
    while i < desc.len() {
        let prefix = desc[i];
        if prefix == 0xFE {
            // Long item: bDataSize, bLongItemTag, data.
            let n = *desc.get(i + 1).ok_or(ParseError::Truncated)? as usize;
            i += 3 + n;
            if i > desc.len() {
                return Err(ParseError::Truncated);
            }
            continue;
        }
        let size = match prefix & 3 {
            3 => 4,
            n => n as usize,
        };
        let kind = (prefix >> 2) & 3;
        let tag = prefix >> 4;
        let data = desc.get(i + 1..i + 1 + size).ok_or(ParseError::Truncated)?;
        i += 1 + size;
        let u = unsigned(data);
        match kind {
            // Main.
            0 => {
                match tag {
                    0x8 | 0x9 => {
                        let bits = g.size.checked_mul(g.count).ok_or(ParseError::Unreasonable)?;
                        if g.size > 32 || bits > 65_536 {
                            return Err(ParseError::Unreasonable);
                        }
                        let cursor = if tag == 0x8 { &mut in_bits } else { &mut out_bits };
                        let at = cursor.entry(g.report_id).or_insert(0);
                        let range = match (l.min, l.max) {
                            (Some(a), Some(b)) => Some((a, b.max(a))),
                            _ => None,
                        };
                        let field = Field {
                            report_id: g.report_id,
                            bit_offset: *at,
                            size: g.size,
                            count: g.count,
                            flags: u,
                            usages: l.usages.clone(),
                            range,
                            logical_min: g.logical_min,
                            logical_max: g.logical_max,
                        };
                        *at += bits;
                        if bits > 0 {
                            if tag == 0x8 {
                                out.inputs.push(field);
                            } else {
                                out.outputs.push(field);
                            }
                        }
                    }
                    0xA => {
                        // Collection: application (1) at the top level only.
                        if depth == 0 && u == 1 {
                            let usage = l.usages.first().copied().or(l.min).unwrap_or(0);
                            out.applications.push(Application { page: (usage >> 16) as u16, usage: (usage & 0xFFFF) as u16 });
                        }
                        depth += 1;
                    }
                    0xC => depth = depth.saturating_sub(1),
                    _ => {}
                }
                l = Locals::default();
            }
            // Global.
            1 => match tag {
                0x0 => g.page = u as u16,
                0x1 => g.logical_min = signed(data),
                0x2 => {
                    // Logical maximum is signed only when the minimum is (common 8-bit
                    // descriptors write 0xFF meaning 255 with a minimum of 0).
                    g.logical_max = if g.logical_min < 0 { signed(data) } else { u as i32 }
                }
                0x7 => g.size = u,
                0x8 => {
                    g.report_id = u as u8;
                    out.uses_report_ids = true;
                }
                0x9 => g.count = u,
                0xA => {
                    if stack.len() >= 64 {
                        return Err(ParseError::StackOverflow);
                    }
                    stack.push(g);
                }
                0xB => {
                    if let Some(p) = stack.pop() {
                        g = p;
                    }
                }
                _ => {}
            },
            // Local.
            2 => match tag {
                0x0 => {
                    let v = l.extend(g.page, u, size);
                    l.usages.push(v);
                }
                0x1 => l.min = Some(l.extend(g.page, u, size)),
                0x2 => l.max = Some(l.extend(g.page, u, size)),
                _ => {}
            },
            _ => {}
        }
    }
    if out.inputs.is_empty() {
        return Err(ParseError::Empty);
    }
    Ok(out)
}

/// Read `size` bits at `bit` of `data` (little endian, HID 1.11 §8.4). Missing bytes read
/// as zero.
pub fn read_bits(data: &[u8], bit: u32, size: u32) -> u32 {
    let mut v = 0u64;
    for k in 0..size.min(32) {
        let b = bit + k;
        let byte = data.get((b / 8) as usize).copied().unwrap_or(0);
        if byte >> (b % 8) & 1 != 0 {
            v |= 1 << k;
        }
    }
    v as u32
}

/// Field value `i` of `f` in `data`, sign-extended when the field is signed.
pub fn value(f: &Field, data: &[u8], i: u32) -> i64 {
    let raw = read_bits(data, f.bit_offset + i * f.size, f.size);
    if f.signed() { crate::convert::sign_extend(raw, f.size as u16) } else { i64::from(raw) }
}

/// Boot-protocol keyboard (HID 1.11 appendix B.1): modifiers byte, reserved byte, six key
/// slots. Used when the descriptor cannot be read.
pub fn boot_keyboard() -> ReportLayout {
    let k = u32::from(page::KEYBOARD) << 16;
    ReportLayout {
        inputs: vec![
            Field {
                report_id: 0,
                bit_offset: 0,
                size: 1,
                count: 8,
                flags: field_flags::VARIABLE,
                usages: Vec::new(),
                range: Some((k | 0xE0, k | 0xE7)),
                logical_min: 0,
                logical_max: 1,
            },
            Field {
                report_id: 0,
                bit_offset: 8,
                size: 8,
                count: 1,
                flags: field_flags::CONSTANT,
                usages: Vec::new(),
                range: None,
                logical_min: 0,
                logical_max: 0,
            },
            Field {
                report_id: 0,
                bit_offset: 16,
                size: 8,
                count: 6,
                flags: 0,
                usages: Vec::new(),
                range: Some((k, k | 0xFF)),
                logical_min: 0,
                logical_max: 255,
            },
        ],
        outputs: Vec::new(),
        applications: vec![Application { page: page::GENERIC_DESKTOP, usage: gd::KEYBOARD }],
        uses_report_ids: false,
    }
}

/// Boot-protocol mouse (HID 1.11 appendix B.2): three buttons, X, Y.
pub fn boot_mouse() -> ReportLayout {
    let b = u32::from(page::BUTTON) << 16;
    let d = u32::from(page::GENERIC_DESKTOP) << 16;
    let rel = field_flags::VARIABLE | field_flags::RELATIVE;
    ReportLayout {
        inputs: vec![
            Field {
                report_id: 0,
                bit_offset: 0,
                size: 1,
                count: 3,
                flags: field_flags::VARIABLE,
                usages: Vec::new(),
                range: Some((b | 1, b | 3)),
                logical_min: 0,
                logical_max: 1,
            },
            Field {
                report_id: 0,
                bit_offset: 3,
                size: 5,
                count: 1,
                flags: field_flags::CONSTANT,
                usages: Vec::new(),
                range: None,
                logical_min: 0,
                logical_max: 0,
            },
            Field {
                report_id: 0,
                bit_offset: 8,
                size: 8,
                count: 2,
                flags: rel,
                usages: vec![d | u32::from(gd::X), d | u32::from(gd::Y)],
                range: None,
                logical_min: -127,
                logical_max: 127,
            },
        ],
        outputs: Vec::new(),
        applications: vec![Application { page: page::GENERIC_DESKTOP, usage: gd::MOUSE }],
        uses_report_ids: false,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// HID 1.11 appendix E.6 keyboard (boot compatible), as a device would send it.
    pub const KEYBOARD_DESC: &[u8] = &[
        0x05, 0x01, 0x09, 0x06, 0xA1, 0x01, // GD, Keyboard, Application
        0x05, 0x07, 0x19, 0xE0, 0x29, 0xE7, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x08, 0x81, 0x02, // modifiers
        0x95, 0x01, 0x75, 0x08, 0x81, 0x01, // reserved
        0x95, 0x05, 0x75, 0x01, 0x05, 0x08, 0x19, 0x01, 0x29, 0x05, 0x91, 0x02, // LEDs
        0x95, 0x01, 0x75, 0x03, 0x91, 0x01, // LED padding
        0x95, 0x06, 0x75, 0x08, 0x15, 0x00, 0x25, 0x65, 0x05, 0x07, 0x19, 0x00, 0x29, 0x65, 0x81, 0x00, // keys
        0xC0,
    ];

    /// Five-button wheel mouse with report id 1, 16-bit X/Y, plus a consumer collection
    /// (report id 2) carrying AC Pan and media keys.
    pub const MOUSE_DESC: &[u8] = &[
        0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x85, 0x01, 0x09, 0x01, 0xA1, 0x00, //
        0x05, 0x09, 0x19, 0x01, 0x29, 0x05, 0x15, 0x00, 0x25, 0x01, 0x95, 0x05, 0x75, 0x01, 0x81, 0x02, // buttons
        0x95, 0x01, 0x75, 0x03, 0x81, 0x01, // padding
        0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x16, 0x01, 0x80, 0x26, 0xFF, 0x7F, 0x75, 0x10, 0x95, 0x02, 0x81, 0x06, // X, Y
        0x09, 0x38, 0x15, 0x81, 0x25, 0x7F, 0x75, 0x08, 0x95, 0x01, 0x81, 0x06, // wheel
        0x05, 0x0C, 0x0A, 0x38, 0x02, 0x95, 0x01, 0x81, 0x06, // AC Pan
        0xC0, 0xC0, //
        0x05, 0x0C, 0x09, 0x01, 0xA1, 0x01, 0x85, 0x02, //
        0x19, 0x00, 0x2A, 0x3C, 0x02, 0x15, 0x00, 0x26, 0x3C, 0x02, 0x95, 0x01, 0x75, 0x10, 0x81, 0x00, // one usage array
        0xC0,
    ];

    #[test]
    fn keyboard_descriptor() {
        let l = parse(KEYBOARD_DESC).unwrap();
        assert!(!l.uses_report_ids);
        assert_eq!(l.applications, vec![Application { page: 1, usage: 6 }]);
        assert_eq!(l.inputs.len(), 3);
        let keys = &l.inputs[2];
        assert_eq!((keys.bit_offset, keys.size, keys.count), (16, 8, 6));
        assert!(!keys.is_variable());
        assert_eq!(keys.usage_for_index(0x04), Some(0x0007_0004));
        assert_eq!(keys.usage_for_index(0x66), None, "past logical max");
        assert_eq!(l.outputs.len(), 2);
        assert_eq!(l.outputs[0].range, Some((0x0008_0001, 0x0008_0005)));
        let m = &l.inputs[0];
        assert_eq!(m.usage_at(3), Some(0x0007_00E3));
        assert_eq!(m.usage_at(9), Some(0x0007_00E7), "clamped to the range");
    }

    #[test]
    fn mouse_descriptor_with_report_ids() {
        let l = parse(MOUSE_DESC).unwrap();
        assert!(l.uses_report_ids);
        assert_eq!(l.applications, vec![Application { page: 1, usage: 2 }, Application { page: 0x0C, usage: 1 }]);
        let xy = l.inputs.iter().find(|f| f.usages.first() == Some(&0x0001_0030)).unwrap();
        assert_eq!((xy.report_id, xy.bit_offset, xy.size, xy.count), (1, 8, 16, 2));
        assert!(xy.signed() && xy.is_relative());
        assert_eq!((xy.logical_min, xy.logical_max), (-32767, 32767));
        let pan = l.inputs.iter().find(|f| f.usages.first() == Some(&0x000C_0238)).unwrap();
        assert_eq!(pan.bit_offset, 48);
        let cons = l.inputs.iter().find(|f| f.report_id == 2).unwrap();
        assert_eq!(cons.bit_offset, 0, "own cursor per report id");
        assert_eq!(cons.usage_for_index(0xE9), Some(0x000C_00E9));
    }

    #[test]
    fn bits_and_signs() {
        assert_eq!(read_bits(&[0b1010_0000, 0b0000_0001], 5, 4), 0b1101);
        assert_eq!(read_bits(&[0xFF], 4, 8), 0x0F, "missing bytes read as zero");
        let l = parse(MOUSE_DESC).unwrap();
        let xy = l.inputs.iter().find(|f| f.usages.first() == Some(&0x0001_0030)).unwrap();
        // Report body (after the id): buttons, X = -2, Y = 300.
        let body = [0x00, 0xFE, 0xFF, 0x2C, 0x01];
        assert_eq!(value(xy, &body, 0), -2);
        assert_eq!(value(xy, &body, 1), 300);
    }

    #[test]
    fn malformed_descriptors() {
        assert_eq!(parse(&[0x05]), Err(ParseError::Truncated));
        assert_eq!(parse(&[0x05, 0x01, 0x09, 0x06, 0xA1, 0x01, 0xC0]), Err(ParseError::Empty));
        assert_eq!(parse(&[0x75, 0x40, 0x95, 0x01, 0x81, 0x02]), Err(ParseError::Unreasonable));
        let pushes = [0xA4u8; 70];
        assert_eq!(parse(&pushes), Err(ParseError::StackOverflow));
        // Long item skipped.
        let mut d = vec![0xFE, 0x02, 0x10, 0xAA, 0xBB];
        d.extend_from_slice(KEYBOARD_DESC);
        assert_eq!(parse(&d).unwrap().inputs.len(), 3);
    }

    #[test]
    fn push_pop_and_four_byte_usage() {
        // Page GD, push, page Button, pop -> page GD again; usage given with its page.
        let d = [0x05, 0x01, 0xA4, 0x05, 0x09, 0xB4, 0x0B, 0x30, 0x00, 0x01, 0x00, 0x09, 0x31, 0x75, 0x08, 0x95, 0x02, 0x81, 0x06];
        let l = parse(&d).unwrap();
        assert_eq!(l.inputs[0].usages, vec![0x0001_0030, 0x0001_0031]);
    }

    #[test]
    fn boot_layouts() {
        assert_eq!(boot_keyboard().inputs[2].usage_for_index(0x29), Some(0x0007_0029));
        assert!(boot_mouse().inputs[2].signed());
    }
}
