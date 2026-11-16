// SPDX-License-Identifier: GPL-3.0-only
//! Per-pad media section (shared-memory protocol 2, 2026-11-17; Pulsar-defined, additive to
//! spec 60 §4.5): controller audio between the service and the seat's stream server.
//!
//! - **out** ring (service -> stream server): what games play on the pad's speaker
//!   endpoint: DualSense speaker / headset jack and the audio haptics channels
//!   (interleaved s16le, 48 kHz, [`layout::MEDIA_OUT_CHANNELS`] = 4 channels: front L / R =
//!   speaker / headset, rear L / R = haptic actuators).
//! - **in** ring (stream server -> service): the client controller's microphone
//!   (interleaved s16le, 48 kHz, [`layout::MEDIA_IN_CHANNELS`] = 2 channels), played into the
//!   pad's microphone endpoint.
//!
//! Each ring is single-producer / single-consumer: byte counters `write` and `read` only
//! grow (wrapping u32), the data offset is `counter % capacity`. A producer that finds too
//! little room drops the whole block and counts it (`dropped`), so the consumer never
//! sees half a frame. The service creates the section `<base>_Media` and the event
//! `<base>_MediaEvt` (set after each out write) next to the protocol 1 objects, with the
//! same DACL; stream servers that only speak protocol 1 never open them. Which streams
//! carry it: the Pulsar stream server (`pulsar-host`, contract §5 media) only; Moonlight /
//! Sunshine has no controller-audio channel, so Sunshine seats stay HID-only.
//!
//! HYPOTHESIS (untested on hardware): end to end (no stream server speaks protocol 2 yet).

use crate::layout;
use crate::shm::ShmView;

/// Which ring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ring {
    /// Service -> stream server (speaker, haptics).
    Out,
    /// Stream server -> service (microphone).
    In,
}

impl Ring {
    const fn offsets(self) -> (usize, usize, usize, usize, usize, usize) {
        // (cap, write, read, dropped, data offset, capacity)
        match self {
            Ring::Out => (
                layout::MEDIA_OFF_OUT_CAP,
                layout::MEDIA_OFF_OUT_WRITE,
                layout::MEDIA_OFF_OUT_READ,
                layout::MEDIA_OFF_OUT_DROPPED,
                layout::MEDIA_OFF_DATA,
                layout::MEDIA_OUT_CAP,
            ),
            Ring::In => (
                layout::MEDIA_OFF_IN_CAP,
                layout::MEDIA_OFF_IN_WRITE,
                layout::MEDIA_OFF_IN_READ,
                layout::MEDIA_OFF_IN_DROPPED,
                layout::MEDIA_OFF_DATA + layout::MEDIA_OUT_CAP,
                layout::MEDIA_IN_CAP,
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MediaError {
    #[error("media section too small")]
    TooSmall,
    #[error("media section header invalid")]
    BadHeader,
}

/// Writes the header and empties both rings (service side, at channel creation).
pub fn init_media(v: &dyn ShmView) -> Result<(), MediaError> {
    if v.len() < layout::MEDIA_SECTION_SIZE {
        return Err(MediaError::TooSmall);
    }
    v.write_u16(layout::MEDIA_OFF_VERSION, layout::PROTOCOL_MEDIA as u16);
    v.write_u16(layout::MEDIA_OFF_HEADER_SIZE, layout::HEADER_SIZE);
    v.write_u32(layout::MEDIA_OFF_RATE, layout::MEDIA_RATE);
    v.write_u16(layout::MEDIA_OFF_OUT_CHANNELS, layout::MEDIA_OUT_CHANNELS);
    v.write_u16(layout::MEDIA_OFF_IN_CHANNELS, layout::MEDIA_IN_CHANNELS);
    for ring in [Ring::Out, Ring::In] {
        let (cap, w, r, d, _, size) = ring.offsets();
        v.store_u32(cap, size as u32);
        v.store_u32(w, 0);
        v.store_u32(r, 0);
        v.store_u32(d, 0);
    }
    v.store_u32(layout::MEDIA_OFF_FLAGS, 0);
    // Magic last: a reader that sees it sees a complete header.
    v.store_u32(layout::MEDIA_OFF_MAGIC, layout::MEDIA_MAGIC);
    Ok(())
}

/// Checks the header (stream-server side).
pub fn validate_media(v: &dyn ShmView) -> Result<(), MediaError> {
    if v.len() < layout::MEDIA_SECTION_SIZE {
        return Err(MediaError::TooSmall);
    }
    let ok = v.load_u32(layout::MEDIA_OFF_MAGIC) == layout::MEDIA_MAGIC
        && v.read_u16(layout::MEDIA_OFF_VERSION) == layout::PROTOCOL_MEDIA as u16
        && v.load_u32(layout::MEDIA_OFF_OUT_CAP) as usize == layout::MEDIA_OUT_CAP
        && v.load_u32(layout::MEDIA_OFF_IN_CAP) as usize == layout::MEDIA_IN_CAP;
    if ok { Ok(()) } else { Err(MediaError::BadHeader) }
}

/// Producer: appends `data` to `ring` if all of it fits, else drops it and counts the
/// drop. Returns whether it was written.
pub fn write_ring(v: &dyn ShmView, ring: Ring, data: &[u8]) -> bool {
    let (_, w_off, r_off, d_off, base, cap) = ring.offsets();
    let w = v.load_u32(w_off);
    let r = v.load_u32(r_off);
    let used = w.wrapping_sub(r) as usize;
    if data.is_empty() {
        return true;
    }
    if used > cap || cap - used < data.len() {
        v.store_u32(d_off, v.load_u32(d_off).wrapping_add(1));
        return false;
    }
    let at = w as usize % cap;
    let first = data.len().min(cap - at);
    v.write(base + at, &data[..first]);
    if first < data.len() {
        v.write(base, &data[first..]);
    }
    v.store_u32(w_off, w.wrapping_add(data.len() as u32));
    true
}

/// Consumer: takes at most `max` bytes (a multiple of `align`, the frame size) from `ring`.
pub fn read_ring(v: &dyn ShmView, ring: Ring, max: usize, align: usize) -> Vec<u8> {
    let (_, w_off, r_off, _, base, cap) = ring.offsets();
    let w = v.load_u32(w_off);
    let r = v.load_u32(r_off);
    let avail = (w.wrapping_sub(r) as usize).min(cap);
    let align = align.max(1);
    let n = (avail.min(max) / align) * align;
    if n == 0 {
        return Vec::new();
    }
    let mut out = vec![0u8; n];
    let at = r as usize % cap;
    let first = n.min(cap - at);
    v.read(base + at, &mut out[..first]);
    if first < n {
        v.read(base, &mut out[first..]);
    }
    v.store_u32(r_off, r.wrapping_add(n as u32));
    out
}

/// Blocks dropped by the producer of `ring` so far.
pub fn dropped(v: &dyn ShmView, ring: Ring) -> u32 {
    v.load_u32(ring.offsets().3)
}

/// Header flags (service writes): [`layout::MEDIA_FLAG_SPEAKER`] while a game streams to
/// the speaker interface, [`layout::MEDIA_FLAG_MIC`] while one records from the pad's
/// microphone.
pub fn set_flags(v: &dyn ShmView, flags: u32) {
    v.store_u32(layout::MEDIA_OFF_FLAGS, flags);
}

pub fn flags(v: &dyn ShmView) -> u32 {
    v.load_u32(layout::MEDIA_OFF_FLAGS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shm::MemShm;

    #[test]
    fn header_and_rings() {
        let m = MemShm::new(layout::MEDIA_SECTION_SIZE);
        assert_eq!(validate_media(&*m), Err(MediaError::BadHeader));
        init_media(&*m).unwrap();
        validate_media(&*m).unwrap();
        assert!(init_media(&*MemShm::new(64)).is_err());
        // Out: service writes, server reads in frame-aligned pieces.
        assert!(write_ring(&*m, Ring::Out, &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]));
        assert_eq!(read_ring(&*m, Ring::Out, 100, 8), vec![1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(read_ring(&*m, Ring::Out, 100, 8), Vec::<u8>::new(), "2 bytes are no frame");
        // In ring is separate.
        assert!(write_ring(&*m, Ring::In, &[9; 4]));
        assert_eq!(read_ring(&*m, Ring::In, 2, 2), vec![9, 9]);
        set_flags(&*m, layout::MEDIA_FLAG_MIC);
        assert_eq!(flags(&*m), layout::MEDIA_FLAG_MIC);
    }

    #[test]
    fn full_ring_drops_whole_blocks_and_wraps() {
        let m = MemShm::new(layout::MEDIA_SECTION_SIZE);
        init_media(&*m).unwrap();
        let block = vec![7u8; layout::MEDIA_IN_CAP / 2 + 1];
        assert!(write_ring(&*m, Ring::In, &block));
        assert!(!write_ring(&*m, Ring::In, &block), "no room: dropped");
        assert_eq!(dropped(&*m, Ring::In), 1);
        assert_eq!(read_ring(&*m, Ring::In, usize::MAX, 1).len(), block.len());
        // Now the write position is past the middle: the next block wraps around.
        let wrap: Vec<u8> = (0..block.len()).map(|i| i as u8).collect();
        assert!(write_ring(&*m, Ring::In, &wrap));
        assert_eq!(read_ring(&*m, Ring::In, usize::MAX, 1), wrap);
    }
}
