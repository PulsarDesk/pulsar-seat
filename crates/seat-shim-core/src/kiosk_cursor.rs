// SPDX-License-Identifier: GPL-3.0-only
//! A kiosk seat's own cursor position for its apps (parity 2026-11-17).
//!
//! Every kiosk seat shares the console session's one system cursor; the shell draws a
//! cursor per seat and posts mouse messages at the seat's position. Apps that ask Windows
//! where the cursor is (`GetCursorPos`, `GetPhysicalCursorPos`, `GetCursorInfo`,
//! `GetMessagePos`), most visibly to place a popup menu with `TrackPopupMenu(Ex)` or a
//! tooltip, got the real cursor (seat 1's, or the focus sink's). The shell now publishes
//! its seat cursor in a small named section and the module (role `kiosk`, flag
//! [`crate::KioskFlags::cursor`]) answers those calls from it.
//!
//! Section `Local\PulsarSeat.KioskCursor.<NN>` ([`section_name`]) in the console session,
//! created by the shell (the seat's own user; its apps run as the same user and open it
//! read-only), [`SECTION_SIZE`] bytes, layout ([`encode`] / [`decode`]):
//!
//! | Offset | Type | Value |
//! | --- | --- | --- |
//! | 0 | u32 LE | [`MAGIC`] (`PSKC`) |
//! | 4 | u32 LE | sequence: odd while the shell writes, even when stable |
//! | 8 | i32 LE | x (virtual-screen pixels) |
//! | 12 | i32 LE | y |
//! | 16 | u32 LE | flags: bit 0 = the shell is alive (cleared on exit) |
//!
//! A reader takes the position only when the magic matches, the sequence is even and the
//! same before and after the read (seqlock), and the alive bit is set; otherwise the real
//! call's answer stands. Not a security boundary (the same user could write it).

/// Size of the section in bytes.
pub const SECTION_SIZE: usize = 20;
/// `PSKC`.
pub const MAGIC: u32 = 0x434B_5350;
/// Flag: the shell is running.
pub const ALIVE: u32 = 1;

/// Section name of a seat as the shell and its apps open it (session-local namespace).
pub fn section_name(seat: u32) -> String {
    format!(r"Local\PulsarSeat.KioskCursor.{seat:02}")
}

/// Bytes of one stable record (`seq` must be even).
pub fn encode(seq: u32, x: i32, y: i32, alive: bool) -> [u8; SECTION_SIZE] {
    let mut b = [0u8; SECTION_SIZE];
    b[0..4].copy_from_slice(&MAGIC.to_le_bytes());
    b[4..8].copy_from_slice(&seq.to_le_bytes());
    b[8..12].copy_from_slice(&x.to_le_bytes());
    b[12..16].copy_from_slice(&y.to_le_bytes());
    b[16..20].copy_from_slice(&(if alive { ALIVE } else { 0 }).to_le_bytes());
    b
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// The sequence word of a snapshot (for the seqlock re-check).
pub fn sequence(b: &[u8]) -> Option<u32> {
    u32_at(b, 4)
}

/// `(seq, x, y)` of a stable, live record; `None` for anything else.
pub fn decode(b: &[u8]) -> Option<(u32, i32, i32)> {
    if b.len() < SECTION_SIZE || u32_at(b, 0)? != MAGIC {
        return None;
    }
    let seq = u32_at(b, 4)?;
    if seq % 2 != 0 || u32_at(b, 16)? & ALIVE == 0 {
        return None;
    }
    Some((seq, u32_at(b, 8)? as i32, u32_at(b, 12)? as i32))
}

/// Seqlock read over a snapshot function: two snapshots with the same even sequence.
pub fn read_stable(mut snap: impl FnMut() -> [u8; SECTION_SIZE]) -> Option<(i32, i32)> {
    for _ in 0..4 {
        let a = snap();
        let Some((seq, x, y)) = decode(&a) else {
            if sequence(&a).is_some_and(|s| s % 2 == 1) {
                continue;
            }
            return None;
        };
        if sequence(&snap()) == Some(seq) {
            return Some((x, y));
        }
    }
    None
}

/// The writer side's sequence: the shell bumps it to odd, writes, bumps it to even.
#[derive(Debug, Default, Clone, Copy)]
pub struct Writer {
    seq: u32,
}

impl Writer {
    /// `(odd seq to write first, even seq to finish with)`.
    pub fn advance(&mut self) -> (u32, u32) {
        let odd = self.seq.wrapping_add(1) | 1;
        self.seq = odd.wrapping_add(1);
        (odd, self.seq)
    }
}

/// `RIDEV_REMOVE`, `RIDEV_INPUTSINK`, `RIDEV_EXINPUTSINK` (winuser.h).
pub const RIDEV_REMOVE: u32 = 0x0000_0001;
pub const RIDEV_INPUTSINK: u32 = 0x0000_0100;
pub const RIDEV_EXINPUTSINK: u32 = 0x0000_1000;

/// Raw-input registration flags for a kiosk app (parity 2026-11-17): an app of a kiosk seat
/// is almost never the foreground window (the focus sink or seat 1 is), so a game that
/// registers raw input the usual way (foreground only) would get no input at all. The
/// module adds `RIDEV_INPUTSINK` (background delivery), which Windows accepts only with a
/// target window; the raw-input data filter then keeps only the seat's devices. Removals,
/// registrations that already ask for background input, and ones without a window are left
/// as they are.
pub fn kiosk_raw_flags(flags: u32, has_target: bool) -> u32 {
    if !has_target || flags & (RIDEV_REMOVE | RIDEV_INPUTSINK | RIDEV_EXINPUTSINK) != 0 {
        return flags;
    }
    flags | RIDEV_INPUTSINK
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kiosk_apps_get_raw_input_in_the_background() {
        assert_eq!(kiosk_raw_flags(0, true), RIDEV_INPUTSINK);
        assert_eq!(kiosk_raw_flags(0x30, true), 0x30 | RIDEV_INPUTSINK, "NOLEGACY | CAPTUREMOUSE kept");
        assert_eq!(kiosk_raw_flags(0, false), 0, "no window: Windows would refuse INPUTSINK");
        assert_eq!(kiosk_raw_flags(RIDEV_REMOVE, true), RIDEV_REMOVE);
        assert_eq!(kiosk_raw_flags(RIDEV_EXINPUTSINK, true), RIDEV_EXINPUTSINK);
    }

    #[test]
    fn names_and_layout() {
        assert_eq!(section_name(2), r"Local\PulsarSeat.KioskCursor.02");
        let b = encode(4, -1920, 1080, true);
        assert_eq!(&b[0..4], b"PSKC");
        assert_eq!(decode(&b), Some((4, -1920, 1080)));
        assert_eq!(decode(&encode(5, 1, 2, true)), None, "odd = being written");
        assert_eq!(decode(&encode(4, 1, 2, false)), None, "shell gone");
        let mut bad = b;
        bad[0] = 0;
        assert_eq!(decode(&bad), None);
        assert_eq!(decode(&b[..10]), None);
    }

    #[test]
    fn seqlock_reads_only_stable_records() {
        assert_eq!(read_stable(|| encode(8, 10, 20, true)), Some((10, 20)));
        // A write in between: the first snapshot is discarded.
        let mut n = 0;
        let r = read_stable(|| {
            n += 1;
            match n {
                1 => encode(8, 1, 1, true),
                2 => encode(10, 2, 2, true),
                _ => encode(10, 2, 2, true),
            }
        });
        assert_eq!(r, Some((2, 2)));
        // Always being written: gives up (the real cursor stands).
        assert_eq!(read_stable(|| encode(9, 1, 1, true)), None);
        assert_eq!(read_stable(|| [0u8; SECTION_SIZE]), None);
        let mut w = Writer::default();
        let (a, b) = w.advance();
        assert!(a % 2 == 1 && b == a + 1);
        let (c, _) = w.advance();
        assert!(c > b && c % 2 == 1);
    }
}
