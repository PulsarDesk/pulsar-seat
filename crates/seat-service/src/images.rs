// SPDX-License-Identifier: GPL-3.0-only
//! Our own generated seat images (spec 80 §11.4): one wallpaper per seat
//! (`images\seat-NN.png`: the seat colour with the seat number drawn large) and one
//! lock-screen image (`images\lock.png`). Plain drawing, no third-party art, no image crate:
//! a two-colour palette PNG (1 bit per pixel) with stored (uncompressed) deflate blocks,
//! about 260 KB at 1920×1080.
//!
//! The service writes missing files at boot and before it queues a `set_wallpaper` task or
//! applies the lock-screen tweak ([`ensure_seat_image`], [`ensure_lock_image`]).

use seat_core::SeatNumber;
use std::io;
use std::path::{Path, PathBuf};

pub const WIDTH: u32 = 1920;
pub const HEIGHT: u32 = 1080;

/// Seat colours (background), cycled by seat number. Chosen to keep white digits legible.
const PALETTE: [[u8; 3]; 8] = [
    [0x1f, 0x5f, 0xbf], // blue
    [0x2e, 0x8b, 0x3e], // green
    [0xb3, 0x3a, 0x3a], // red
    [0x7a, 0x3d, 0xb8], // purple
    [0xc0, 0x6a, 0x12], // orange
    [0x12, 0x80, 0x88], // teal
    [0xa8, 0x2f, 0x7a], // magenta
    [0x55, 0x60, 0x6e], // slate
];
const FOREGROUND: [u8; 3] = [0xff, 0xff, 0xff];
const LOCK_BACKGROUND: [u8; 3] = [0x14, 0x18, 0x22];
const LOCK_BAND: [u8; 3] = [0x2a, 0x33, 0x47];

/// 5×7 digit glyphs, one row per byte, bit 4 = leftmost column.
const DIGITS: [[u8; 7]; 10] = [
    [0x0e, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0e],
    [0x04, 0x0c, 0x04, 0x04, 0x04, 0x04, 0x0e],
    [0x0e, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1f],
    [0x1f, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0e],
    [0x02, 0x06, 0x0a, 0x12, 0x1f, 0x02, 0x02],
    [0x1f, 0x10, 0x1e, 0x01, 0x01, 0x11, 0x0e],
    [0x06, 0x08, 0x10, 0x1e, 0x11, 0x11, 0x0e],
    [0x1f, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
    [0x0e, 0x11, 0x11, 0x0e, 0x11, 0x11, 0x0e],
    [0x0e, 0x11, 0x11, 0x0f, 0x01, 0x02, 0x0c],
];

/// Background colour of a seat (also usable by the manager for the seat badge).
pub fn seat_colour(seat: SeatNumber) -> [u8; 3] {
    PALETTE[(usize::from(seat.get()).saturating_sub(1)) % PALETTE.len()]
}

/// 1-bit bitmap, rows of `WIDTH / 8` bytes, MSB = leftmost pixel; 1 = foreground.
struct Bitmap {
    w: u32,
    h: u32,
    rows: Vec<u8>,
}

impl Bitmap {
    fn new(w: u32, h: u32) -> Self {
        Self { w, h, rows: vec![0; (w.div_ceil(8) * h) as usize] }
    }
    fn stride(&self) -> u32 {
        self.w.div_ceil(8)
    }
    fn set(&mut self, x: u32, y: u32) {
        if x < self.w && y < self.h {
            let i = (y * self.stride() + x / 8) as usize;
            self.rows[i] |= 0x80 >> (x % 8);
        }
    }
    fn fill(&mut self, x: u32, y: u32, w: u32, h: u32) {
        for yy in y..(y + h).min(self.h) {
            for xx in x..(x + w).min(self.w) {
                self.set(xx, yy);
            }
        }
    }
}

/// Seat wallpaper: seat colour, the seat number centred in white.
pub fn seat_png(seat: SeatNumber) -> Vec<u8> {
    let mut b = Bitmap::new(WIDTH, HEIGHT);
    let text = seat.get().to_string();
    let scale = 40u32;
    let (gw, gh, gap) = (5 * scale, 7 * scale, 2 * scale);
    let n = text.len() as u32;
    let total = n * gw + n.saturating_sub(1) * gap;
    let (x0, y0) = ((WIDTH.saturating_sub(total)) / 2, (HEIGHT - gh) / 2);
    for (i, ch) in text.bytes().enumerate() {
        let glyph = DIGITS[usize::from(ch - b'0')];
        let gx = x0 + i as u32 * (gw + gap);
        for (row, bits) in glyph.iter().enumerate() {
            for col in 0..5u32 {
                if bits & (0x10 >> col) != 0 {
                    b.fill(gx + col * scale, y0 + row as u32 * scale, scale, scale);
                }
            }
        }
    }
    encode(&b, seat_colour(seat), FOREGROUND)
}

/// Lock-screen image: dark background with one lighter band (no text: machine-wide).
pub fn lock_png() -> Vec<u8> {
    let mut b = Bitmap::new(WIDTH, HEIGHT);
    b.fill(0, HEIGHT * 3 / 5, WIDTH, HEIGHT / 9);
    encode(&b, LOCK_BACKGROUND, LOCK_BAND)
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xffff_ffffu32;
    for &x in data {
        c ^= u32::from(x);
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
        }
    }
    !c
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &x in data {
        a = (a + u32::from(x)) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

/// Palette PNG, bit depth 1: index 0 = `bg`, 1 = `fg`.
fn encode(b: &Bitmap, bg: [u8; 3], fg: [u8; 3]) -> Vec<u8> {
    let stride = b.stride() as usize;
    let mut raw = Vec::with_capacity((stride + 1) * b.h as usize);
    for y in 0..b.h as usize {
        raw.push(0); // filter: none
        raw.extend_from_slice(&b.rows[y * stride..(y + 1) * stride]);
    }
    // zlib stream with stored deflate blocks.
    let mut z = vec![0x78, 0x01];
    let mut blocks = raw.chunks(0xffff).peekable();
    while let Some(blk) = blocks.next() {
        z.push(u8::from(blocks.peek().is_none()));
        let len = blk.len() as u16;
        z.extend_from_slice(&len.to_le_bytes());
        z.extend_from_slice(&(!len).to_le_bytes());
        z.extend_from_slice(blk);
    }
    z.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut out = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&b.w.to_be_bytes());
    ihdr.extend_from_slice(&b.h.to_be_bytes());
    ihdr.extend_from_slice(&[1, 3, 0, 0, 0]); // depth 1, palette, deflate, adaptive, no interlace
    chunk(&mut out, b"IHDR", &ihdr);
    let plte: Vec<u8> = bg.iter().chain(fg.iter()).copied().collect();
    chunk(&mut out, b"PLTE", &plte);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

pub fn seat_image_path(data_dir: &Path, seat: SeatNumber) -> PathBuf {
    data_dir.join(seat_core::consts::IMAGES_DIR).join(format!("seat-{}.png", seat.two_digits()))
}

pub fn lock_image_path(data_dir: &Path) -> PathBuf {
    data_dir.join(seat_core::consts::IMAGES_DIR).join("lock.png")
}

/// Write `bytes` to `path` unless a file is already there (temp file + rename).
fn ensure(path: &Path, bytes: impl FnOnce() -> Vec<u8>) -> io::Result<PathBuf> {
    if path.is_file() {
        return Ok(path.to_path_buf());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("png.tmp");
    std::fs::write(&tmp, bytes())?;
    std::fs::rename(&tmp, path)?;
    Ok(path.to_path_buf())
}

/// The seat's wallpaper exists (written now if missing). Returns its path.
pub fn ensure_seat_image(data_dir: &Path, seat: SeatNumber) -> io::Result<PathBuf> {
    ensure(&seat_image_path(data_dir, seat), || seat_png(seat))
}

/// The lock-screen image exists (written now if missing). Returns its path.
pub fn ensure_lock_image(data_dir: &Path) -> io::Result<PathBuf> {
    ensure(&lock_image_path(data_dir), lock_png)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_is_well_formed() {
        let p = seat_png(SeatNumber::new(12).unwrap());
        assert_eq!(&p[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&p[12..16], b"IHDR");
        assert_eq!(u32::from_be_bytes(p[16..20].try_into().unwrap()), WIDTH);
        assert_eq!(&p[p.len() - 8..p.len() - 4], b"IEND");
        assert!(p.len() < 300_000, "{}", p.len());
        // Known CRC-32 / Adler-32 check values.
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
        assert_eq!(adler32(b"Wikipedia"), 0x11e6_0398);
    }

    #[test]
    fn digits_are_drawn_and_colours_differ() {
        let one = seat_png(SeatNumber::new(1).unwrap());
        let two = seat_png(SeatNumber::new(2).unwrap());
        assert_ne!(one, two);
        assert_ne!(seat_colour(SeatNumber::new(1).unwrap()), seat_colour(SeatNumber::new(2).unwrap()));
        assert_ne!(lock_png(), one);
    }

    #[test]
    fn ensure_writes_once() {
        let dir = std::env::temp_dir().join(format!("pulsar-seat-img-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let s = SeatNumber::new(3).unwrap();
        let p = ensure_seat_image(&dir, s).unwrap();
        assert!(p.ends_with("seat-03.png"));
        let len = std::fs::metadata(&p).unwrap().len();
        assert!(len > 1000);
        ensure_seat_image(&dir, s).unwrap();
        assert!(ensure_lock_image(&dir).unwrap().is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
