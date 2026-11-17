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
//! created by the shell (the seat's own user; its apps run as the same user and open it),
//! [`SECTION_SIZE`] bytes. Layout v2 (parity review 2026-11-17), little-endian:
//!
//! **Shell part** (written only by the shell, seqlock, [`encode`] / [`decode`]):
//!
//! | Offset | Type | Value |
//! | --- | --- | --- |
//! | 0 | u32 | [`MAGIC`] (`PSKC`) |
//! | 4 | u32 | sequence: odd while the shell writes, even when stable |
//! | 8 | i32 | x (virtual-screen **physical** pixels; the shell is per-monitor aware) |
//! | 12 | i32 | y |
//! | 16 | u32 | flags: bit 0 = the shell is alive (cleared on exit), bit 1 = clip set |
//! | 20 | 4 x i32 | clip rectangle left, top, right, bottom (valid with bit 1) |
//! | 36 | 256 x u8 | the seat's key state, `GetKeyboardState` layout (bit 7 down, bit 0 toggled; mouse buttons as `VK_LBUTTON` ...) |
//!
//! At 312 (u32) the shell writes its UI thread id once: after a request an app posts
//! [`WAKE_MESSAGE`] to that thread, so the shell handles it at once instead of on its tick.
//!
//! **Request part** (written by the seat's apps through the module, read by the shell):
//!
//! | Offset | Type | Value |
//! | --- | --- | --- |
//! | 320 | u32 | spin lock (0 free, 1 held; held only for a few stores) |
//! | 324 | u32 | `SetCursorPos` sequence (bumped per request) |
//! | 328 | 2 x i32 | requested x, y (physical pixels) |
//! | 336 | u32 | `ClipCursor` sequence |
//! | 340 | u32 | 1 = clip to the rectangle, 0 = free |
//! | 344 | 4 x i32 | requested clip left, top, right, bottom |
//!
//! A reader takes the shell part only when the magic matches, the sequence is even and the
//! same before and after the read (seqlock), and the alive bit is set; otherwise the real
//! call's answer stands. Not a security boundary (the same user could write it).

/// Size of the section in bytes.
pub const SECTION_SIZE: usize = 360;
/// Size of the shell part (the seqlocked snapshot).
pub const SHELL_SIZE: usize = 292;
/// `PSKC`.
pub const MAGIC: u32 = 0x434B_5350;
/// Flag: the shell is running.
pub const ALIVE: u32 = 1;
/// Flag: an app clipped the seat cursor.
pub const CLIPPED: u32 = 2;
/// Offset of the key state table.
pub const KEYS_AT: usize = 36;
/// Offset of the shell's UI thread id (u32, written once at create).
pub const SHELL_THREAD: usize = 312;
/// Thread message an app posts to the shell after a request (`WM_APP + 0x61`, the
/// shell's wake message).
pub const WAKE_MESSAGE: u32 = 0x8000 + 0x61;
/// Offsets of the request part.
pub const REQ_LOCK: usize = 320;
pub const REQ_SET_SEQ: usize = 324;
pub const REQ_SET_X: usize = 328;
pub const REQ_SET_Y: usize = 332;
pub const REQ_CLIP_SEQ: usize = 336;
pub const REQ_CLIP_ON: usize = 340;
pub const REQ_CLIP_RECT: usize = 344;

/// Section name of a seat as the shell and its apps open it (session-local namespace).
pub fn section_name(seat: u32) -> String {
    format!(r"Local\PulsarSeat.KioskCursor.{seat:02}")
}

/// What the shell publishes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeatCursor {
    pub x: i32,
    pub y: i32,
    /// `(left, top, right, bottom)` of an app's `ClipCursor`.
    pub clip: Option<(i32, i32, i32, i32)>,
    pub keys: [u8; 256],
}

impl Default for SeatCursor {
    fn default() -> Self {
        Self { x: 0, y: 0, clip: None, keys: [0; 256] }
    }
}

fn put_u32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

/// Bytes of one shell record (`seq` even = stable).
pub fn encode(seq: u32, c: &SeatCursor, alive: bool) -> [u8; SHELL_SIZE] {
    let mut b = [0u8; SHELL_SIZE];
    put_u32(&mut b, 0, MAGIC);
    put_u32(&mut b, 4, seq);
    put_u32(&mut b, 8, c.x as u32);
    put_u32(&mut b, 12, c.y as u32);
    let flags = if alive { ALIVE } else { 0 } | if c.clip.is_some() { CLIPPED } else { 0 };
    put_u32(&mut b, 16, flags);
    let (l, t, r, bt) = c.clip.unwrap_or_default();
    for (i, v) in [l, t, r, bt].into_iter().enumerate() {
        put_u32(&mut b, 20 + 4 * i, v as u32);
    }
    b[KEYS_AT..KEYS_AT + 256].copy_from_slice(&c.keys);
    b
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// The sequence word of a snapshot (for the seqlock re-check).
pub fn sequence(b: &[u8]) -> Option<u32> {
    u32_at(b, 4)
}

/// `(seq, state)` of a stable, live record; `None` for anything else.
pub fn decode(b: &[u8]) -> Option<(u32, SeatCursor)> {
    if b.len() < SHELL_SIZE || u32_at(b, 0)? != MAGIC {
        return None;
    }
    let seq = u32_at(b, 4)?;
    let flags = u32_at(b, 16)?;
    if seq % 2 != 0 || flags & ALIVE == 0 {
        return None;
    }
    let i = |at| u32_at(b, at).map(|v| v as i32);
    let clip = if flags & CLIPPED != 0 { Some((i(20)?, i(24)?, i(28)?, i(32)?)) } else { None };
    let mut keys = [0u8; 256];
    keys.copy_from_slice(&b[KEYS_AT..KEYS_AT + 256]);
    Some((seq, SeatCursor { x: i(8)?, y: i(12)?, clip, keys }))
}

/// Seqlock read over a snapshot function: two snapshots with the same even sequence.
pub fn read_stable(mut snap: impl FnMut() -> [u8; SHELL_SIZE]) -> Option<SeatCursor> {
    for _ in 0..4 {
        let a = snap();
        let Some((seq, c)) = decode(&a) else {
            if sequence(&a).is_some_and(|s| s % 2 == 1) {
                continue;
            }
            return None;
        };
        if sequence(&snap()) == Some(seq) {
            return Some(c);
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

/// The request part as the shell reads it (under the request lock).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Requests {
    pub set_seq: u32,
    pub set: (i32, i32),
    pub clip_seq: u32,
    pub clip: Option<(i32, i32, i32, i32)>,
}

/// Decodes the request part (`b` = the whole section).
pub fn decode_requests(b: &[u8]) -> Option<Requests> {
    let i = |at| u32_at(b, at).map(|v| v as i32);
    let clip = if u32_at(b, REQ_CLIP_ON)? != 0 {
        Some((i(REQ_CLIP_RECT)?, i(REQ_CLIP_RECT + 4)?, i(REQ_CLIP_RECT + 8)?, i(REQ_CLIP_RECT + 12)?))
    } else {
        None
    };
    Some(Requests { set_seq: u32_at(b, REQ_SET_SEQ)?, set: (i(REQ_SET_X)?, i(REQ_SET_Y)?), clip_seq: u32_at(b, REQ_CLIP_SEQ)?, clip })
}

/// The shell's view of the request sequences it handled.
#[derive(Debug, Default, Clone, Copy)]
pub struct RequestCursor {
    set_seq: u32,
    clip_seq: u32,
}

/// A new request of an app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    SetPos(i32, i32),
    Clip(Option<(i32, i32, i32, i32)>),
}

impl RequestCursor {
    /// Requests that arrived since the last call, clip first (a `SetCursorPos` right after
    /// a `ClipCursor` is confined by it).
    pub fn take(&mut self, r: &Requests) -> Vec<Request> {
        let mut out = Vec::new();
        if r.clip_seq != self.clip_seq {
            self.clip_seq = r.clip_seq;
            out.push(Request::Clip(r.clip));
        }
        if r.set_seq != self.set_seq {
            self.set_seq = r.set_seq;
            out.push(Request::SetPos(r.set.0, r.set.1));
        }
        out
    }
}

/// `GetAsyncKeyState` answer from the seat's table (down = bit 15; the "pressed since the
/// last call" bit is not tracked).
pub fn async_key_state(keys: &[u8; 256], vk: i32) -> i16 {
    match usize::try_from(vk).ok().and_then(|i| keys.get(i)) {
        Some(k) if k & 0x80 != 0 => i16::MIN,
        _ => 0,
    }
}

/// `GetKeyState` answer from the seat's table (down = high bit, toggled = bit 0).
pub fn key_state(keys: &[u8; 256], vk: i32) -> i16 {
    let Some(k) = usize::try_from(vk).ok().and_then(|i| keys.get(i)) else { return 0 };
    let mut v: u16 = 0;
    if k & 0x80 != 0 {
        v |= 0xFF80;
    }
    if k & 0x01 != 0 {
        v |= 1;
    }
    v as i16
}

/// DPI awareness of the calling thread (`DPI_AWARENESS`): whether `GetCursorPos`,
/// `GetCursorInfo` and `GetMessagePos` must answer in logical (DPI-virtualised)
/// coordinates. Per-monitor aware threads get physical pixels as published; unaware and
/// system-aware threads get logical ones, like the real functions give them.
pub fn wants_logical(dpi_awareness: i32) -> bool {
    // DPI_AWARENESS_PER_MONITOR_AWARE = 2; INVALID (-1), UNAWARE (0), SYSTEM_AWARE (1).
    dpi_awareness != 2
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
        assert_eq!(kiosk_raw_flags(0x30, true), 0x30 | RIDEV_INPUTSINK, "RIDEV_NOLEGACY kept");
        assert_eq!(kiosk_raw_flags(0x30 | 0x200, true), 0x30 | 0x200 | RIDEV_INPUTSINK, "RIDEV_NOLEGACY | RIDEV_CAPTUREMOUSE kept");
        assert_eq!(kiosk_raw_flags(0, false), 0, "no window: Windows would refuse INPUTSINK");
        assert_eq!(kiosk_raw_flags(RIDEV_REMOVE, true), RIDEV_REMOVE);
        assert_eq!(kiosk_raw_flags(RIDEV_EXINPUTSINK, true), RIDEV_EXINPUTSINK);
    }

    #[test]
    fn names_and_layout() {
        assert_eq!(section_name(2), r"Local\PulsarSeat.KioskCursor.02");
        let mut c = SeatCursor { x: -1920, y: 1080, clip: Some((1, 2, 3, 4)), ..Default::default() };
        c.keys[0x10] = 0x80;
        let b = encode(4, &c, true);
        assert_eq!(&b[0..4], b"PSKC");
        assert_eq!(decode(&b), Some((4, c.clone())));
        assert_eq!(decode(&encode(5, &c, true)), None, "odd = being written");
        assert_eq!(decode(&encode(4, &c, false)), None, "shell gone");
        let mut bad = b;
        bad[0] = 0;
        assert_eq!(decode(&bad), None);
        assert_eq!(decode(&b[..10]), None);
        const { assert!(SHELL_SIZE <= REQ_LOCK && REQ_CLIP_RECT + 16 <= SECTION_SIZE) };
    }

    #[test]
    fn seqlock_reads_only_stable_records() {
        let at = |x: i32| SeatCursor { x, y: x, ..Default::default() };
        assert_eq!(read_stable(|| encode(8, &at(10), true)).map(|c| c.x), Some(10));
        // A write in between: the first snapshot is discarded.
        let mut n = 0;
        let r = read_stable(|| {
            n += 1;
            match n {
                1 => encode(8, &at(1), true),
                _ => encode(10, &at(2), true),
            }
        });
        assert_eq!(r.map(|c| (c.x, c.y)), Some((2, 2)));
        // Always being written: gives up (the real cursor stands).
        assert_eq!(read_stable(|| encode(9, &at(1), true)), None);
        assert_eq!(read_stable(|| [0u8; SHELL_SIZE]), None);
        let mut w = Writer::default();
        let (a, b) = w.advance();
        assert!(a % 2 == 1 && b == a + 1);
        let (c, _) = w.advance();
        assert!(c > b && c % 2 == 1);
    }

    #[test]
    fn app_requests_reach_the_shell_once() {
        let mut b = [0u8; SECTION_SIZE];
        let mut rc = RequestCursor::default();
        assert!(rc.take(&decode_requests(&b).unwrap()).is_empty());
        put_u32(&mut b, REQ_SET_X, 50);
        put_u32(&mut b, REQ_SET_Y, (-7i32) as u32);
        put_u32(&mut b, REQ_SET_SEQ, 1);
        put_u32(&mut b, REQ_CLIP_ON, 1);
        for (i, v) in [0u32, 0, 100, 100].into_iter().enumerate() {
            put_u32(&mut b, REQ_CLIP_RECT + 4 * i, v);
        }
        put_u32(&mut b, REQ_CLIP_SEQ, 1);
        let got = rc.take(&decode_requests(&b).unwrap());
        assert_eq!(got, vec![Request::Clip(Some((0, 0, 100, 100))), Request::SetPos(50, -7)]);
        assert!(rc.take(&decode_requests(&b).unwrap()).is_empty(), "handled once");
        put_u32(&mut b, REQ_CLIP_ON, 0);
        put_u32(&mut b, REQ_CLIP_SEQ, 2);
        assert_eq!(rc.take(&decode_requests(&b).unwrap()), vec![Request::Clip(None)]);
    }

    #[test]
    fn key_state_answers() {
        let mut k = [0u8; 256];
        k[0x10] = 0x80;
        k[0x14] = 0x01;
        assert_eq!(async_key_state(&k, 0x10), i16::MIN);
        assert_eq!(async_key_state(&k, 0x14), 0, "toggled but not down");
        assert_eq!(async_key_state(&k, 300), 0);
        assert!(key_state(&k, 0x10) < 0);
        assert_eq!(key_state(&k, 0x14), 1);
        assert_eq!(key_state(&k, 0x41), 0);
        assert!(wants_logical(0) && wants_logical(1) && wants_logical(-1));
        assert!(!wants_logical(2));
    }
}
