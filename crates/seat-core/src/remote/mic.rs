// SPDX-License-Identifier: GPL-3.0-only
//! Microphone frames of a remote seat (spec/remote 70 §5.1).
//!
//! Path: Pulsar client mic → `pulsar-host` (decodes the client's stream) → the per-seat
//! mic pipe `\\.\pipe\PulsarSplit\seat\<seat-id>\pulsar-host-mic` (service-owned, same
//! client check as the control pipe) → the service → the seat's headless hold
//! (`pulsar-seat-host --headless-hold --audio-capture`, on its stdin after the password
//! frame) → MS-RDPEAI `AUDIO_INPUT` → the seat session's capture endpoint.
//!
//! One codec for both hops. A frame is `u32 LE length` + payload; the payload is one
//! kind byte and its data:
//!
//! | kind | data | meaning |
//! | --- | --- | --- |
//! | `0x01` start | 1 byte: channels (1 or 2) | a client mic opened; PCM follows |
//! | `0x02` pcm | signed 16-bit LE samples, interleaved, [`MIC_RATE`] Hz | audio |
//! | `0x03` stop | – | the client mic closed (or the client left) |
//!
//! The rate is fixed at 48 kHz (Opus' native rate, so `pulsar-host` never resamples).
//! A pcm frame holds whole sample frames and at most [`MAX_MIC_PAYLOAD`] bytes.

use std::io::{self, Read, Write};

/// Sample rate of every pcm frame.
pub const MIC_RATE: u32 = 48_000;
/// Largest payload (kind byte + data): 100 ms of 48 kHz stereo is 19 200 bytes.
pub const MAX_MIC_PAYLOAD: usize = 32 * 1024;
/// `pulsar-host` capability that says it can send mic frames.
pub const MIC_CAPABILITY: &str = "mic";

const K_START: u8 = 0x01;
const K_PCM: u8 = 0x02;
const K_STOP: u8 = 0x03;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MicFrame {
    Start { channels: u8 },
    /// s16le interleaved at [`MIC_RATE`], channel count of the last `Start`.
    Pcm(Vec<u8>),
    Stop,
}

fn bad(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("mic frame: {what}"))
}

impl MicFrame {
    /// Payload (without the length prefix).
    pub fn encode(&self) -> Vec<u8> {
        match self {
            MicFrame::Start { channels } => vec![K_START, *channels],
            MicFrame::Pcm(d) => {
                let mut v = Vec::with_capacity(1 + d.len());
                v.push(K_PCM);
                v.extend_from_slice(d);
                v
            }
            MicFrame::Stop => vec![K_STOP],
        }
    }

    /// Checks a payload (without the length prefix).
    pub fn decode(p: &[u8]) -> io::Result<Self> {
        if p.len() > MAX_MIC_PAYLOAD {
            return Err(bad("too large"));
        }
        match p.split_first() {
            Some((&K_START, [c @ (1 | 2)])) => Ok(MicFrame::Start { channels: *c }),
            Some((&K_START, _)) => Err(bad("start needs 1 or 2 channels")),
            Some((&K_PCM, d)) if d.len() % 2 == 0 => Ok(MicFrame::Pcm(d.to_vec())),
            Some((&K_PCM, _)) => Err(bad("odd pcm length")),
            Some((&K_STOP, [])) => Ok(MicFrame::Stop),
            Some((k, _)) => Err(bad(&format!("unknown kind {k:#04x}"))),
            None => Err(bad("empty")),
        }
    }

    /// Length prefix + payload.
    pub fn to_wire(&self) -> Vec<u8> {
        let p = self.encode();
        let mut v = Vec::with_capacity(4 + p.len());
        v.extend_from_slice(&(p.len() as u32).to_le_bytes());
        v.extend_from_slice(&p);
        v
    }
}

/// Writes one frame (length prefix + payload) and flushes.
pub fn write_mic_frame(w: &mut impl Write, f: &MicFrame) -> io::Result<()> {
    w.write_all(&f.to_wire())?;
    w.flush()
}

/// Reads one frame. `Ok(None)` at a clean end of stream (between frames).
pub fn read_mic_frame(r: &mut impl Read) -> io::Result<Option<MicFrame>> {
    let mut len = [0u8; 4];
    let mut got = 0;
    while got < 4 {
        match r.read(&mut len[got..])? {
            0 if got == 0 => return Ok(None),
            0 => return Err(io::ErrorKind::UnexpectedEof.into()),
            n => got += n,
        }
    }
    let len = u32::from_le_bytes(len) as usize;
    if len == 0 || len > MAX_MIC_PAYLOAD {
        return Err(bad("length out of range"));
    }
    let mut p = vec![0u8; len];
    r.read_exact(&mut p)?;
    MicFrame::decode(&p).map(Some)
}

/// `pcm` (s16le, `from` channels) as `to` channels: mono is duplicated, stereo is averaged.
pub fn convert_channels(pcm: &[u8], from: u8, to: u8) -> Vec<u8> {
    if from == to || !(1..=2).contains(&from) || !(1..=2).contains(&to) {
        return pcm.to_vec();
    }
    let s = |i: usize| i16::from_le_bytes([pcm[i], pcm[i + 1]]);
    let mut out = Vec::with_capacity(if to == 2 { pcm.len() * 2 } else { pcm.len() / 2 });
    if from == 1 {
        for c in pcm.chunks_exact(2) {
            out.extend_from_slice(c);
            out.extend_from_slice(c);
        }
    } else {
        for i in (0..pcm.len() / 4).map(|k| k * 4) {
            let m = ((i32::from(s(i)) + i32::from(s(i + 2))) / 2) as i16;
            out.extend_from_slice(&m.to_le_bytes());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip_on_the_wire() {
        let frames = [MicFrame::Start { channels: 2 }, MicFrame::Pcm(vec![1, 0, 2, 0]), MicFrame::Stop];
        let mut buf = Vec::new();
        for f in &frames {
            write_mic_frame(&mut buf, f).unwrap();
        }
        let mut r = buf.as_slice();
        for f in &frames {
            assert_eq!(read_mic_frame(&mut r).unwrap().as_ref(), Some(f));
        }
        assert_eq!(read_mic_frame(&mut r).unwrap(), None, "clean end");
    }

    #[test]
    fn bad_frames_are_refused() {
        assert!(MicFrame::decode(&[]).is_err());
        assert!(MicFrame::decode(&[K_START, 3]).is_err());
        assert!(MicFrame::decode(&[K_START]).is_err());
        assert!(MicFrame::decode(&[K_PCM, 1]).is_err());
        assert!(MicFrame::decode(&[K_STOP, 0]).is_err());
        assert!(MicFrame::decode(&[0x7F]).is_err());
        assert!(MicFrame::decode(&vec![K_PCM; MAX_MIC_PAYLOAD + 1]).is_err());
        // Length prefix out of range / truncated body / truncated prefix.
        assert!(read_mic_frame(&mut [0u8, 0, 0, 0].as_slice()).is_err());
        assert!(read_mic_frame(&mut [0xFFu8, 0xFF, 0xFF, 0x7F].as_slice()).is_err());
        assert!(read_mic_frame(&mut [3u8, 0, 0, 0, K_PCM].as_slice()).is_err());
        assert!(read_mic_frame(&mut [3u8, 0].as_slice()).is_err());
    }

    #[test]
    fn channel_conversion() {
        let mono: Vec<u8> = [100i16, -200].iter().flat_map(|s| s.to_le_bytes()).collect();
        let st = convert_channels(&mono, 1, 2);
        let want: Vec<u8> = [100i16, 100, -200, -200].iter().flat_map(|s| s.to_le_bytes()).collect();
        assert_eq!(st, want);
        let back = convert_channels(&st, 2, 1);
        assert_eq!(back, mono);
        let mixed: Vec<u8> = [100i16, 300].iter().flat_map(|s| s.to_le_bytes()).collect();
        assert_eq!(convert_channels(&mixed, 2, 1), 200i16.to_le_bytes().to_vec());
        assert_eq!(convert_channels(&mono, 1, 1), mono);
    }
}
