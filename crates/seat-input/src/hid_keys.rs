// SPDX-License-Identifier: GPL-3.0-only
//! HID Keyboard/Keypad page (0x07) usage → PC set-1 scan code, for the `winusb_capture`
//! reader. Source: Microsoft's public *Keyboard Scan Code Specification* (the USB HID to
//! PS/2 translation table, appendix C) and the HID Usage Tables, keyboard page. No other
//! implementation was used.
//!
//! The result feeds [`crate::convert::RawUnitEvent::Key`], so it uses the raw-input form:
//! a scan code plus the raw flags (`2` = E0). Pause is the one key raw input reports as two
//! events (`0x1D` with E1, then `0x45`); [`PAUSE`] says so and the translator emits both.

/// Usage of the Pause key.
pub const PAUSE: u16 = 0x48;

/// Raw flag of an E0-extended key (`RI_KEY_E0`).
pub const E0: u16 = 2;
/// Raw flag of the E1 prefix (`RI_KEY_E1`).
pub const E1: u16 = 4;

/// Set-1 make code and raw E0 flag of a keyboard-page usage (`None` = no PC key, or Pause,
/// which needs [`PAUSE`] handling).
pub fn usage_to_scan(usage: u16) -> Option<(u16, u16)> {
    let plain = |sc: u16| Some((sc, 0));
    let ext = |sc: u16| Some((sc, E0));
    match usage {
        // Letters a..z.
        0x04 => plain(0x1E),
        0x05 => plain(0x30),
        0x06 => plain(0x2E),
        0x07 => plain(0x20),
        0x08 => plain(0x12),
        0x09 => plain(0x21),
        0x0A => plain(0x22),
        0x0B => plain(0x23),
        0x0C => plain(0x17),
        0x0D => plain(0x24),
        0x0E => plain(0x25),
        0x0F => plain(0x26),
        0x10 => plain(0x32),
        0x11 => plain(0x31),
        0x12 => plain(0x18),
        0x13 => plain(0x19),
        0x14 => plain(0x10),
        0x15 => plain(0x13),
        0x16 => plain(0x1F),
        0x17 => plain(0x14),
        0x18 => plain(0x16),
        0x19 => plain(0x2F),
        0x1A => plain(0x11),
        0x1B => plain(0x2D),
        0x1C => plain(0x15),
        0x1D => plain(0x2C),
        // Digits 1..9, 0.
        0x1E..=0x26 => plain(usage - 0x1E + 0x02),
        0x27 => plain(0x0B),
        0x28 => plain(0x1C), // Enter
        0x29 => plain(0x01), // Escape
        0x2A => plain(0x0E), // Backspace
        0x2B => plain(0x0F), // Tab
        0x2C => plain(0x39), // Space
        0x2D => plain(0x0C), // - _
        0x2E => plain(0x0D), // = +
        0x2F => plain(0x1A), // [ {
        0x30 => plain(0x1B), // ] }
        0x31 => plain(0x2B), // \ |
        0x32 => plain(0x2B), // Europe 1 (# ~)
        0x33 => plain(0x27), // ; :
        0x34 => plain(0x28), // ' "
        0x35 => plain(0x29), // ` ~
        0x36 => plain(0x33), // , <
        0x37 => plain(0x34), // . >
        0x38 => plain(0x35), // / ?
        0x39 => plain(0x3A), // Caps Lock
        // F1..F10.
        0x3A..=0x43 => plain(usage - 0x3A + 0x3B),
        0x44 => plain(0x57), // F11
        0x45 => plain(0x58), // F12
        0x46 => ext(0x37),   // Print Screen
        0x47 => plain(0x46), // Scroll Lock
        0x49 => ext(0x52),   // Insert
        0x4A => ext(0x47),   // Home
        0x4B => ext(0x49),   // Page Up
        0x4C => ext(0x53),   // Delete
        0x4D => ext(0x4F),   // End
        0x4E => ext(0x51),   // Page Down
        0x4F => ext(0x4D),   // Right
        0x50 => ext(0x4B),   // Left
        0x51 => ext(0x50),   // Down
        0x52 => ext(0x48),   // Up
        0x53 => plain(0x45), // Num Lock
        0x54 => ext(0x35),   // Keypad /
        0x55 => plain(0x37), // Keypad *
        0x56 => plain(0x4A), // Keypad -
        0x57 => plain(0x4E), // Keypad +
        0x58 => ext(0x1C),   // Keypad Enter
        0x59 => plain(0x4F), // Keypad 1
        0x5A => plain(0x50), // Keypad 2
        0x5B => plain(0x51), // Keypad 3
        0x5C => plain(0x4B), // Keypad 4
        0x5D => plain(0x4C), // Keypad 5
        0x5E => plain(0x4D), // Keypad 6
        0x5F => plain(0x47), // Keypad 7
        0x60 => plain(0x48), // Keypad 8
        0x61 => plain(0x49), // Keypad 9
        0x62 => plain(0x52), // Keypad 0
        0x63 => plain(0x53), // Keypad .
        0x64 => plain(0x56), // Europe 2 (\ |)
        0x65 => ext(0x5D),   // Application
        0x66 => ext(0x5E),   // Power
        0x67 => plain(0x59), // Keypad =
        // F13..F23, F24.
        0x68..=0x72 => plain(usage - 0x68 + 0x64),
        0x73 => plain(0x76),
        0x7F => ext(0x20),   // Mute
        0x80 => ext(0x30),   // Volume up
        0x81 => ext(0x2E),   // Volume down
        0x85 => plain(0x7E), // Keypad , (Brazil)
        0x87 => plain(0x73), // International 1 (Ro)
        0x88 => plain(0x70), // International 2 (Katakana / Hiragana)
        0x89 => plain(0x7D), // International 3 (Yen)
        0x8A => plain(0x79), // International 4 (Henkan)
        0x8B => plain(0x7B), // International 5 (Muhenkan)
        0x8C => plain(0x5C), // International 6
        0x92 => plain(0x78), // LANG3
        0x93 => plain(0x77), // LANG4
        0x94 => plain(0x76), // LANG5
        0xE0 => plain(0x1D), // Left Ctrl
        0xE1 => plain(0x2A), // Left Shift
        0xE2 => plain(0x38), // Left Alt
        0xE3 => ext(0x5B),   // Left GUI
        0xE4 => ext(0x1D),   // Right Ctrl
        0xE5 => plain(0x36), // Right Shift
        0xE6 => ext(0x38),   // Right Alt
        0xE7 => ext(0x5C),   // Right GUI
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convert::sc;

    #[test]
    fn common_keys() {
        assert_eq!(usage_to_scan(0x04), Some((0x1E, 0)), "a");
        assert_eq!(usage_to_scan(0x1D), Some((0x2C, 0)), "z");
        assert_eq!(usage_to_scan(0x1E), Some((0x02, 0)), "1");
        assert_eq!(usage_to_scan(0x26), Some((0x0A, 0)), "9");
        assert_eq!(usage_to_scan(0x27), Some((0x0B, 0)), "0");
        assert_eq!(usage_to_scan(0x3A), Some((0x3B, 0)), "F1");
        assert_eq!(usage_to_scan(0x43), Some((0x44, 0)), "F10");
        assert_eq!(usage_to_scan(0x45), Some((sc::F12, 0)));
        assert_eq!(usage_to_scan(0x4C), Some((sc::DELETE, E0)));
        assert_eq!(usage_to_scan(0xE0), Some((sc::LCTRL, 0)));
        assert_eq!(usage_to_scan(0xE2), Some((sc::LALT, 0)));
        assert_eq!(usage_to_scan(0xE6), Some((0x38, E0)), "AltGr");
        assert_eq!(usage_to_scan(0x68), Some((0x64, 0)), "F13");
        assert_eq!(usage_to_scan(0x72), Some((0x6E, 0)), "F23");
        assert_eq!(usage_to_scan(0x73), Some((0x76, 0)), "F24");
    }

    #[test]
    fn non_keys() {
        for u in [0x00, 0x01, 0x02, 0x03, PAUSE, 0x74, 0xA5, 0xE8, 0xFF] {
            assert_eq!(usage_to_scan(u), None, "{u:#x}");
        }
    }

    #[test]
    fn every_scan_code_fits_one_byte() {
        for u in 0..=0xFFu16 {
            if let Some((s, f)) = usage_to_scan(u) {
                assert!(s > 0 && s < 0x80, "{u:#x} -> {s:#x}");
                assert!(f == 0 || f == E0);
            }
        }
    }
}
