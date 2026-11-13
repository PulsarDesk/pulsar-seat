// SPDX-License-Identifier: GPL-3.0-only
//! Length-prefixed frames over any byte stream (a named pipe in byte mode in production,
//! an in-memory buffer in tests). Adapted from pulsar-split (`split-proto::framing`).
//!
//! Frame = 4-byte little-endian payload length + UTF-8 JSON payload, at most
//! [`MAX_FRAME_LEN`] bytes.

use serde::{Serialize, de::DeserializeOwned};
use std::io::{self, Read, Write};

/// Upper bound for one frame's payload. Control messages are small; this stops a
/// misbehaving peer from making the reader allocate unbounded memory.
pub const MAX_FRAME_LEN: u32 = 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("i/o: {0}")]
    Io(#[from] io::Error),
    #[error("frame of {0} bytes exceeds the limit")]
    TooLarge(u32),
    #[error("stream ended inside a frame")]
    Truncated,
    #[error("invalid payload: {0}")]
    Json(#[from] serde_json::Error),
}

/// Write one frame. Header and payload go out in a single `write_all` so that a message
/// is never split across two pipe writes by us.
pub fn write_frame<W: Write>(w: &mut W, payload: &[u8]) -> Result<(), FrameError> {
    let len = u32::try_from(payload.len()).map_err(|_| FrameError::TooLarge(u32::MAX))?;
    if len > MAX_FRAME_LEN {
        return Err(FrameError::TooLarge(len));
    }
    let mut buf = Vec::with_capacity(4 + payload.len());
    buf.extend_from_slice(&len.to_le_bytes());
    buf.extend_from_slice(payload);
    w.write_all(&buf)?;
    w.flush()?;
    Ok(())
}

/// Read one frame. `Ok(None)` means the peer closed the stream cleanly between frames.
pub fn read_frame<R: Read>(r: &mut R) -> Result<Option<Vec<u8>>, FrameError> {
    let mut header = [0u8; 4];
    let mut got = 0;
    while got < header.len() {
        match r.read(&mut header[got..]) {
            Ok(0) if got == 0 => return Ok(None),
            Ok(0) => return Err(FrameError::Truncated),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            // A closed pipe reports BrokenPipe instead of a zero-length read.
            Err(e) if e.kind() == io::ErrorKind::BrokenPipe && got == 0 => return Ok(None),
            Err(e) => return Err(e.into()),
        }
    }
    let len = u32::from_le_bytes(header);
    if len > MAX_FRAME_LEN {
        return Err(FrameError::TooLarge(len));
    }
    let mut payload = vec![0u8; len as usize];
    r.read_exact(&mut payload).map_err(|e| match e.kind() {
        io::ErrorKind::UnexpectedEof | io::ErrorKind::BrokenPipe => FrameError::Truncated,
        _ => FrameError::Io(e),
    })?;
    Ok(Some(payload))
}

pub fn write_message<W: Write, T: Serialize>(w: &mut W, msg: &T) -> Result<(), FrameError> {
    let payload = serde_json::to_vec(msg)?;
    write_frame(w, &payload)
}

pub fn read_message<R: Read, T: DeserializeOwned>(r: &mut R) -> Result<Option<T>, FrameError> {
    match read_frame(r)? {
        None => Ok(None),
        Some(payload) => Ok(Some(serde_json::from_slice(&payload)?)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn roundtrip_several_frames() {
        let mut buf = Vec::new();
        write_frame(&mut buf, b"hello").unwrap();
        write_frame(&mut buf, b"").unwrap();
        write_frame(&mut buf, &[0xAB; 1000]).unwrap();
        assert_eq!(&buf[..4], &5u32.to_le_bytes());
        let mut r = Cursor::new(buf);
        assert_eq!(read_frame(&mut r).unwrap().unwrap(), b"hello");
        assert_eq!(read_frame(&mut r).unwrap().unwrap(), b"");
        assert_eq!(read_frame(&mut r).unwrap().unwrap(), vec![0xAB; 1000]);
        assert!(read_frame(&mut r).unwrap().is_none());
    }

    #[test]
    fn oversized_header_rejected_without_allocating() {
        let mut buf = (MAX_FRAME_LEN + 1).to_le_bytes().to_vec();
        buf.extend_from_slice(b"junk");
        assert!(matches!(read_frame(&mut Cursor::new(buf)), Err(FrameError::TooLarge(_))));
    }

    #[test]
    fn oversized_write_rejected() {
        let big = vec![0u8; MAX_FRAME_LEN as usize + 1];
        assert!(matches!(write_frame(&mut Vec::new(), &big), Err(FrameError::TooLarge(_))));
    }

    #[test]
    fn truncated_header_and_payload() {
        assert!(matches!(read_frame(&mut Cursor::new(vec![1u8, 0])), Err(FrameError::Truncated)));
        let mut buf = 10u32.to_le_bytes().to_vec();
        buf.extend_from_slice(b"abc");
        assert!(matches!(read_frame(&mut Cursor::new(buf)), Err(FrameError::Truncated)));
    }

    #[test]
    fn garbage_payload_is_json_error() {
        let mut buf = Vec::new();
        write_frame(&mut buf, b"not json").unwrap();
        let r: Result<Option<serde_json::Value>, _> = read_message(&mut Cursor::new(buf));
        assert!(matches!(r, Err(FrameError::Json(_))));
    }
}
