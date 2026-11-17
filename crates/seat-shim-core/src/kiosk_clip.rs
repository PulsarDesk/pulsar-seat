// SPDX-License-Identifier: GPL-3.0-only
//! A kiosk seat's own clipboard (parity review 2026-11-17).
//!
//! Every kiosk seat shares the console session's window station, so Windows gives all of
//! them, and seat 1, one clipboard. The module (role `kiosk`, the seat features flag
//! [`crate::KioskFlags::cursor`]) answers the clipboard calls of a seat's apps from a
//! per-seat store instead, so a copy on one seat is pasted only on that seat and never
//! reaches seat 1 or another seat:
//!
//! - `EmptyClipboard` + `SetClipboardData` + `CloseClipboard` write a new set of formats
//!   into the seat's section ([`section_name`]); delayed-rendered formats (`NULL` handle,
//!   OLE clipboard) are rendered at `CloseClipboard` with `WM_RENDERFORMAT`;
//! - `GetClipboardData`, `IsClipboardFormatAvailable`, `CountClipboardFormats`,
//!   `EnumClipboardFormats`, `GetPriorityClipboardFormat`, `GetUpdatedClipboardFormats` and
//!   `GetClipboardSequenceNumber` read it, with the usual synthesized formats
//!   ([`available`]);
//! - `AddClipboardFormatListener` windows of the seat get `WM_CLIPBOARDUPDATE` after a new
//!   set (the listener table in the section).
//!
//! Stored: every HGLOBAL format as bytes, `CF_BITMAP` as `CF_DIB`, `CF_ENHMETAFILE` as its
//! metafile bits. Not stored (the call fails): `CF_METAFILEPICT`, `CF_PALETTE`,
//! `CF_DSP*` handle formats and sets larger than the section ([`CAPACITY`]).
//!
//! Section `Local\PulsarSeat.KioskClip.<NN>`, created by the seat's shell (the seat's
//! user; its apps run as the same user), [`SECTION_SIZE`] bytes, little-endian:
//!
//! | Offset | Type | Value |
//! | --- | --- | --- |
//! | 0 | u32 | [`MAGIC`] (`PSCB`) |
//! | 4 | u32 | spin lock (0 free, 1 held) |
//! | 8 | u32 | generation (the seat's clipboard sequence number, bumped per new set) |
//! | 12 | u32 | number of formats |
//! | 16 | u32 | bytes used in the data area |
//! | 64 | 32 x u64 | listener windows (0 = free) |
//! | 320 | ... | data: per format `u32 format, u32 length, bytes`, each entry 8-aligned |
//!
//! Not a security boundary (same user, same desktop). HYPOTHESIS: never run on hardware
//! (OLE clipboard delayed rendering through `WM_RENDERFORMAT`, drag and drop is not the
//! clipboard and is not covered).

/// Size of the section (pagefile-backed; pages are only used when written).
pub const SECTION_SIZE: usize = 32 * 1024 * 1024;
/// `PSCB`.
pub const MAGIC: u32 = 0x4243_5350;
pub const LOCK_AT: usize = 4;
pub const GENERATION_AT: usize = 8;
pub const COUNT_AT: usize = 12;
pub const USED_AT: usize = 16;
pub const LISTENERS_AT: usize = 64;
pub const MAX_LISTENERS: usize = 32;
pub const DATA_AT: usize = LISTENERS_AT + MAX_LISTENERS * 8;
/// Largest data area.
pub const CAPACITY: usize = SECTION_SIZE - DATA_AT;

/// Standard clipboard formats (winuser.h).
pub mod cf {
    pub const TEXT: u32 = 1;
    pub const BITMAP: u32 = 2;
    pub const METAFILEPICT: u32 = 3;
    pub const OEMTEXT: u32 = 7;
    pub const DIB: u32 = 8;
    pub const PALETTE: u32 = 9;
    pub const UNICODETEXT: u32 = 13;
    pub const ENHMETAFILE: u32 = 14;
    pub const LOCALE: u32 = 16;
    pub const DIBV5: u32 = 17;
    pub const DSPBITMAP: u32 = 0x82;
    pub const DSPMETAFILEPICT: u32 = 0x83;
    pub const DSPENHMETAFILE: u32 = 0x8E;
}

/// `WM_CLIPBOARDUPDATE`, `WM_RENDERFORMAT`.
pub const WM_CLIPBOARDUPDATE: u32 = 0x031D;
pub const WM_RENDERFORMAT: u32 = 0x0305;

/// Section name of a seat (session-local namespace).
pub fn section_name(seat: u32) -> String {
    format!(r"Local\PulsarSeat.KioskClip.{seat:02}")
}

/// How the module stores a format given to `SetClipboardData`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Storage {
    /// HGLOBAL: its bytes as they are.
    Global,
    /// `CF_BITMAP`: converted to `CF_DIB`.
    BitmapAsDib,
    /// `CF_ENHMETAFILE`: the metafile bits.
    EnhMetafile,
    /// Not supported (GDI handles without a byte form here).
    Unsupported,
}

pub fn storage_of(format: u32) -> Storage {
    match format {
        cf::BITMAP => Storage::BitmapAsDib,
        cf::ENHMETAFILE => Storage::EnhMetafile,
        cf::METAFILEPICT | cf::PALETTE | cf::DSPBITMAP | cf::DSPMETAFILEPICT | cf::DSPENHMETAFILE => Storage::Unsupported,
        _ => Storage::Global,
    }
}

/// Formats an app sees for a stored set, in order: the stored ones, then the ones Windows
/// would synthesize from them (`CF_UNICODETEXT` <-> `CF_TEXT` / `CF_OEMTEXT` (+
/// `CF_LOCALE`), `CF_DIB` <-> `CF_DIBV5`, `CF_BITMAP` from either DIB).
pub fn available(stored: &[u32]) -> Vec<u32> {
    let mut out: Vec<u32> = Vec::new();
    fn add(f: u32, out: &mut Vec<u32>) {
        if !out.contains(&f) {
            out.push(f);
        }
    }
    for &f in stored {
        add(f, &mut out);
    }
    let has = |f| stored.contains(&f);
    if has(cf::UNICODETEXT) || has(cf::TEXT) || has(cf::OEMTEXT) {
        for f in [cf::UNICODETEXT, cf::TEXT, cf::OEMTEXT, cf::LOCALE] {
            add(f, &mut out);
        }
    }
    if has(cf::DIB) || has(cf::DIBV5) {
        for f in [cf::DIB, cf::DIBV5, cf::BITMAP] {
            add(f, &mut out);
        }
    }
    out
}

/// Where a requested format comes from: stored directly, or converted from another one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Stored(u32),
    /// UTF-16 text to an 8-bit code page (`oem` = `CF_OEMTEXT`).
    TextFromUnicode { oem: bool },
    /// 8-bit text (`from` = `CF_TEXT` / `CF_OEMTEXT`) to UTF-16.
    UnicodeFromText { from: u32 },
    /// `CF_DIB` / `CF_DIBV5` bytes into a device-dependent bitmap.
    BitmapFromDib(u32),
    /// `CF_DIBV5` and `CF_DIB` share the pixel layout; the header is converted.
    DibFromDib(u32),
    /// `CF_LOCALE` of synthesized text: the thread's locale.
    Locale,
}

/// How to answer `GetClipboardData(format)` for a stored set; `None` = not available.
pub fn source_for(format: u32, stored: &[u32]) -> Option<Source> {
    let has = |f| stored.contains(&f);
    if has(format) {
        return Some(match format {
            cf::BITMAP => Source::BitmapFromDib(cf::DIB),
            _ => Source::Stored(format),
        });
    }
    match format {
        cf::TEXT | cf::OEMTEXT if has(cf::UNICODETEXT) => Some(Source::TextFromUnicode { oem: format == cf::OEMTEXT }),
        cf::TEXT if has(cf::OEMTEXT) => Some(Source::Stored(cf::OEMTEXT)),
        cf::OEMTEXT if has(cf::TEXT) => Some(Source::Stored(cf::TEXT)),
        cf::UNICODETEXT if has(cf::TEXT) => Some(Source::UnicodeFromText { from: cf::TEXT }),
        cf::UNICODETEXT if has(cf::OEMTEXT) => Some(Source::UnicodeFromText { from: cf::OEMTEXT }),
        cf::LOCALE if has(cf::UNICODETEXT) || has(cf::TEXT) || has(cf::OEMTEXT) => Some(Source::Locale),
        cf::BITMAP if has(cf::DIB) => Some(Source::BitmapFromDib(cf::DIB)),
        cf::BITMAP if has(cf::DIBV5) => Some(Source::BitmapFromDib(cf::DIBV5)),
        cf::DIB if has(cf::DIBV5) => Some(Source::DibFromDib(cf::DIBV5)),
        cf::DIBV5 if has(cf::DIB) => Some(Source::DibFromDib(cf::DIB)),
        _ => None,
    }
}

/// One format of a set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub format: u32,
    pub data: Vec<u8>,
}

fn align8(n: usize) -> usize {
    (n + 7) & !7
}

/// Bytes of the data area for a set, `None` when it does not fit [`CAPACITY`].
pub fn encode_data(entries: &[Entry]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    for e in entries {
        out.extend_from_slice(&e.format.to_le_bytes());
        out.extend_from_slice(&u32::try_from(e.data.len()).ok()?.to_le_bytes());
        out.extend_from_slice(&e.data);
        out.resize(align8(out.len()), 0);
        if out.len() > CAPACITY {
            return None;
        }
    }
    Some(out)
}

/// The set in a data area of `count` entries (`used` bytes); stops at the first bad entry.
pub fn decode_data(data: &[u8], count: u32) -> Vec<Entry> {
    let mut out = Vec::new();
    let mut at = 0usize;
    for _ in 0..count {
        let Some(h) = data.get(at..at + 8) else { break };
        let format = u32::from_le_bytes([h[0], h[1], h[2], h[3]]);
        let len = u32::from_le_bytes([h[4], h[5], h[6], h[7]]) as usize;
        let Some(d) = data.get(at + 8..at + 8 + len) else { break };
        out.push(Entry { format, data: d.to_vec() });
        at = align8(at + 8 + len);
    }
    out
}

/// Formats of a data area of `count` entries, without copying the data.
pub fn formats_of(data: &[u8], count: u32) -> Vec<u32> {
    let mut out = Vec::new();
    let mut at = 0usize;
    for _ in 0..count {
        let Some(h) = data.get(at..at + 8) else { break };
        let len = u32::from_le_bytes([h[4], h[5], h[6], h[7]]) as usize;
        if data.len() < at + 8 + len {
            break;
        }
        out.push(u32::from_le_bytes([h[0], h[1], h[2], h[3]]));
        at = align8(at + 8 + len);
    }
    out
}

/// The bytes of `format` in a data area, without copying the others.
pub fn find(data: &[u8], count: u32, format: u32) -> Option<&[u8]> {
    let mut at = 0usize;
    for _ in 0..count {
        let h = data.get(at..at + 8)?;
        let len = u32::from_le_bytes([h[4], h[5], h[6], h[7]]) as usize;
        let d = data.get(at + 8..at + 8 + len)?;
        if u32::from_le_bytes([h[0], h[1], h[2], h[3]]) == format {
            return Some(d);
        }
        at = align8(at + 8 + len);
    }
    None
}

/// Offset of the pixel bits in a packed DIB (`CF_DIB` / `CF_DIBV5`): header, the three
/// `BI_BITFIELDS` masks after a 40-byte header, then the colour table.
pub fn dib_bits_offset(dib: &[u8]) -> Option<usize> {
    let size = u32::from_le_bytes(dib.get(0..4)?.try_into().ok()?) as usize;
    if size < 40 {
        return None;
    }
    let bits = u16::from_le_bytes(dib.get(14..16)?.try_into().ok()?);
    let compression = u32::from_le_bytes(dib.get(16..20)?.try_into().ok()?);
    let used = u32::from_le_bytes(dib.get(32..36)?.try_into().ok()?) as usize;
    let masks = if size == 40 && (compression == 3 || compression == 6) { if compression == 6 { 16 } else { 12 } } else { 0 };
    let colours = if used != 0 { used } else if bits <= 8 { 1usize << bits } else { 0 };
    let off = size + masks + colours * 4;
    (off <= dib.len()).then_some(off)
}

/// A set with `format` replaced (or added): for `SetClipboardData` outside an
/// `EmptyClipboard` (a `WM_RENDERFORMAT` answer).
pub fn with_format(mut set: Vec<Entry>, e: Entry) -> Vec<Entry> {
    set.retain(|x| x.format != e.format);
    set.push(e);
    set
}

/// `BITMAPV5HEADER` -> `BITMAPINFOHEADER` (`CF_DIBV5` -> `CF_DIB`): the first 40 bytes of the
/// header with `biSize` 40, then whatever follows the V5 header (masks of `BI_BITFIELDS`
/// are kept by the caller when `biCompression` asks for them). `None` for a short buffer.
pub fn dib_from_dibv5(v5: &[u8]) -> Option<Vec<u8>> {
    let size = u32::from_le_bytes(v5.get(0..4)?.try_into().ok()?) as usize;
    if size < 40 || v5.len() < size {
        return None;
    }
    let compression = u32::from_le_bytes(v5.get(16..20)?.try_into().ok()?);
    let mut out = v5[..40].to_vec();
    out[0..4].copy_from_slice(&40u32.to_le_bytes());
    if compression == 3 {
        // BI_BITFIELDS: the three masks follow a BITMAPINFOHEADER; in a V5 header they are
        // at offsets 40..52.
        out.extend_from_slice(v5.get(40..52)?);
    }
    out.extend_from_slice(&v5[size..]);
    Some(out)
}

/// `BITMAPINFOHEADER` -> `BITMAPV5HEADER` (`CF_DIB` -> `CF_DIBV5`), sRGB, no profile.
pub fn dibv5_from_dib(dib: &[u8]) -> Option<Vec<u8>> {
    let size = u32::from_le_bytes(dib.get(0..4)?.try_into().ok()?) as usize;
    if size < 40 || dib.len() < size {
        return None;
    }
    let compression = u32::from_le_bytes(dib.get(16..20)?.try_into().ok()?);
    let mut h = vec![0u8; 124];
    h[..40].copy_from_slice(&dib[..40]);
    h[0..4].copy_from_slice(&124u32.to_le_bytes());
    let mut rest = &dib[size..];
    if compression == 3 && size == 40 {
        h[40..52].copy_from_slice(rest.get(0..12)?);
        rest = &rest[12..];
    }
    // bV5CSType = LCS_sRGB ('sRGB'), bV5Intent = LCS_GM_IMAGES (4).
    h[56..60].copy_from_slice(&0x7352_4742u32.to_le_bytes());
    h[108..112].copy_from_slice(&4u32.to_le_bytes());
    h.extend_from_slice(rest);
    Some(h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout() {
        assert_eq!(section_name(3), r"Local\PulsarSeat.KioskClip.03");
        const { assert!(DATA_AT % 8 == 0 && DATA_AT > USED_AT + 4) };
    }

    #[test]
    fn sets_round_trip_and_replace() {
        let set = vec![Entry { format: cf::UNICODETEXT, data: vec![b'h', 0, b'i', 0, 0, 0] }, Entry { format: 0xC123, data: vec![1, 2, 3] }];
        let data = encode_data(&set).unwrap();
        assert_eq!(data.len() % 8, 0);
        assert_eq!(decode_data(&data, 2), set);
        assert_eq!(decode_data(&data[..10], 2).len(), 0, "truncated entry dropped");
        let set = with_format(set, Entry { format: 0xC123, data: vec![9] });
        assert_eq!(set.len(), 2);
        assert_eq!(set[1].data, vec![9]);
        assert!(encode_data(&[Entry { format: 1, data: vec![0; CAPACITY] }]).is_none(), "too large");
    }

    #[test]
    fn synthesized_formats() {
        let a = available(&[cf::UNICODETEXT]);
        assert_eq!(a, vec![cf::UNICODETEXT, cf::TEXT, cf::OEMTEXT, cf::LOCALE]);
        assert_eq!(source_for(cf::TEXT, &[cf::UNICODETEXT]), Some(Source::TextFromUnicode { oem: false }));
        assert_eq!(source_for(cf::UNICODETEXT, &[cf::TEXT]), Some(Source::UnicodeFromText { from: cf::TEXT }));
        assert_eq!(source_for(cf::UNICODETEXT, &[cf::UNICODETEXT]), Some(Source::Stored(cf::UNICODETEXT)));
        assert_eq!(source_for(cf::BITMAP, &[cf::DIB]), Some(Source::BitmapFromDib(cf::DIB)));
        assert_eq!(source_for(cf::DIBV5, &[cf::DIB]), Some(Source::DibFromDib(cf::DIB)));
        assert_eq!(available(&[cf::DIB]), vec![cf::DIB, cf::DIBV5, cf::BITMAP]);
        assert_eq!(source_for(cf::DIB, &[cf::UNICODETEXT]), None);
        assert_eq!(storage_of(cf::BITMAP), Storage::BitmapAsDib);
        assert_eq!(storage_of(cf::PALETTE), Storage::Unsupported);
        assert_eq!(storage_of(0xC0FF), Storage::Global);
    }

    #[test]
    fn walking_without_copying() {
        let set = vec![Entry { format: 1, data: vec![7; 5] }, Entry { format: 0xC001, data: vec![8; 3] }];
        let data = encode_data(&set).unwrap();
        assert_eq!(formats_of(&data, 2), vec![1, 0xC001]);
        assert_eq!(find(&data, 2, 0xC001), Some(&[8u8, 8, 8][..]));
        assert_eq!(find(&data, 2, 13), None);
        assert_eq!(formats_of(&data[..12], 2), Vec::<u32>::new());
    }

    #[test]
    fn dib_bits_offsets() {
        let mut h = vec![0u8; 40];
        h[0..4].copy_from_slice(&40u32.to_le_bytes());
        h[14..16].copy_from_slice(&8u16.to_le_bytes());
        h.resize(40 + 256 * 4 + 1, 0);
        assert_eq!(dib_bits_offset(&h), Some(40 + 1024), "8-bit: full palette");
        h[14..16].copy_from_slice(&32u16.to_le_bytes());
        h[16..20].copy_from_slice(&3u32.to_le_bytes());
        assert_eq!(dib_bits_offset(&h), Some(52), "BI_BITFIELDS masks");
        h[16..20].copy_from_slice(&0u32.to_le_bytes());
        assert_eq!(dib_bits_offset(&h), Some(40));
        assert_eq!(dib_bits_offset(&h[..20]), None);
    }

    #[test]
    fn dib_header_conversion() {
        // 1x1 32-bit BI_RGB DIB.
        let mut dib = vec![0u8; 40];
        dib[0..4].copy_from_slice(&40u32.to_le_bytes());
        dib[4..8].copy_from_slice(&1i32.to_le_bytes());
        dib[8..12].copy_from_slice(&1i32.to_le_bytes());
        dib[12..14].copy_from_slice(&1u16.to_le_bytes());
        dib[14..16].copy_from_slice(&32u16.to_le_bytes());
        dib.extend_from_slice(&[1, 2, 3, 4]);
        let v5 = dibv5_from_dib(&dib).unwrap();
        assert_eq!(v5.len(), 124 + 4);
        assert_eq!(u32::from_le_bytes(v5[0..4].try_into().unwrap()), 124);
        let back = dib_from_dibv5(&v5).unwrap();
        assert_eq!(back, dib);
        assert!(dib_from_dibv5(&[1, 2]).is_none());
    }
}
