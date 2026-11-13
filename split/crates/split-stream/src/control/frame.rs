// SPDX-License-Identifier: GPL-3.0-only
//! Seat-pipe framing (spec 80 §8.4): u32 little-endian length, then that many bytes of UTF-8
//! JSON. Frames above 1 MiB close the connection. Same in both directions.

use std::io::{self, Read, Write};

pub const MAX_FRAME: usize = 1024 * 1024;

fn too_large() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "frame larger than 1 MiB")
}

pub fn write_frame<W: Write>(w: &mut W, payload: &[u8]) -> io::Result<()> {
    if payload.len() > MAX_FRAME {
        return Err(too_large());
    }
    let mut buf = Vec::with_capacity(4 + payload.len());
    buf.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    buf.extend_from_slice(payload);
    // One write so a message pipe never sees a split header.
    w.write_all(&buf)?;
    w.flush()
}

pub fn read_frame<R: Read>(r: &mut R) -> io::Result<Vec<u8>> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let n = u32::from_le_bytes(len) as usize;
    if n > MAX_FRAME {
        return Err(too_large());
    }
    let mut buf = vec![0u8; n];
    r.read_exact(&mut buf)?;
    Ok(buf)
}

/// Incremental frame decoder for non-blocking transports: feed bytes as they arrive,
/// take complete frames out. Rejects an oversize length as soon as the header is in.
#[derive(Debug, Default)]
pub struct FrameBuf {
    bytes: Vec<u8>,
}

impl FrameBuf {
    pub fn push(&mut self, b: &[u8]) {
        self.bytes.extend_from_slice(b);
    }

    fn declared(&self) -> Option<usize> {
        let h: [u8; 4] = self.bytes.get(..4)?.try_into().ok()?;
        Some(u32::from_le_bytes(h) as usize)
    }

    /// Bytes still needed to finish the current frame (4 while no header is in).
    pub fn wanted(&self) -> usize {
        match self.declared() {
            Some(n) => (4 + n.min(MAX_FRAME)).saturating_sub(self.bytes.len()).max(1),
            None => 4 - self.bytes.len(),
        }
    }

    /// Some bytes of a frame are buffered but the frame is incomplete.
    pub fn is_partial(&self) -> bool {
        !self.bytes.is_empty()
    }

    pub fn take_frame(&mut self) -> io::Result<Option<Vec<u8>>> {
        let Some(n) = self.declared() else { return Ok(None) };
        if n > MAX_FRAME {
            return Err(too_large());
        }
        if self.bytes.len() < 4 + n {
            return Ok(None);
        }
        let rest = self.bytes.split_off(4 + n);
        let mut frame = std::mem::replace(&mut self.bytes, rest);
        frame.drain(..4);
        Ok(Some(frame))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_buf_assembles_partial_frames() {
        let mut out = Vec::new();
        write_frame(&mut out, b"hello").unwrap();
        write_frame(&mut out, b"x").unwrap();
        let mut b = FrameBuf::default();
        assert_eq!(b.wanted(), 4);
        b.push(&out[..2]);
        assert_eq!(b.take_frame().unwrap(), None);
        assert_eq!(b.wanted(), 2);
        b.push(&out[2..6]);
        assert!(b.is_partial());
        assert_eq!(b.wanted(), 3);
        assert_eq!(b.take_frame().unwrap(), None);
        b.push(&out[6..]);
        assert_eq!(b.take_frame().unwrap().unwrap(), b"hello");
        assert_eq!(b.take_frame().unwrap().unwrap(), b"x");
        assert!(!b.is_partial());
        let mut b = FrameBuf::default();
        b.push(&((MAX_FRAME + 1) as u32).to_le_bytes());
        assert!(b.take_frame().is_err());
    }

    #[test]
    fn round_trip_and_le_header() {
        let mut out = Vec::new();
        write_frame(&mut out, b"{\"a\":1}").unwrap();
        assert_eq!(&out[..4], &[7, 0, 0, 0]);
        let mut r = io::Cursor::new(out);
        assert_eq!(read_frame(&mut r).unwrap(), b"{\"a\":1}");
        assert_eq!(read_frame(&mut r).unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn oversize_rejected_both_ways() {
        let big = vec![b' '; MAX_FRAME + 1];
        assert!(write_frame(&mut Vec::new(), &big).is_err());
        let mut hdr = ((MAX_FRAME + 1) as u32).to_le_bytes().to_vec();
        hdr.extend_from_slice(b"xx");
        assert_eq!(read_frame(&mut io::Cursor::new(hdr)).unwrap_err().kind(), io::ErrorKind::InvalidData);
        // Exactly 1 MiB is allowed.
        let max = vec![b' '; MAX_FRAME];
        let mut out = Vec::new();
        write_frame(&mut out, &max).unwrap();
        assert_eq!(read_frame(&mut io::Cursor::new(out)).unwrap().len(), MAX_FRAME);
    }
}
