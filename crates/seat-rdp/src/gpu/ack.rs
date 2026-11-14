// SPDX-License-Identifier: GPL-3.0-only
//! Frame acknowledgement flow control (MS-RDPEGFX 2.2.2.13 RDPGFX_FRAME_ACKNOWLEDGE_PDU,
//! 3.3.5.12). Pure logic.
//!
//! The server limits how many frames it sends ahead of their acknowledgements. IronRDP's
//! pipeline acknowledges each frame as soon as it is decoded (`EndFrame`); when the
//! seat's presenter cannot keep up (its last frame is not on screen yet), we hold the
//! acknowledgements back so the server slows down instead of queueing pictures we would
//! skip anyway:
//!
//! - presenter not behind and nothing held: acknowledge at once (`queueDepth`
//!   unavailable, as before);
//! - presenter behind: hold, at most [`MAX_HELD`] frames and [`MAX_HOLD`] for the oldest,
//!   so a stuck presenter never stalls the session (the server would stop sending);
//! - when the presenter catches up, every held frame is acknowledged in order.
//!
//! `queueDepth` of a released acknowledgement is the size of the frames still held (bytes
//! received and not yet shown, the field's meaning in 2.2.2.13), or unavailable (0) when
//! nothing is held. We never send `SUSPEND_FRAME_ACKNOWLEDGEMENT`.
//!
//! HYPOTHESIS: Windows servers throttle on unacknowledged frames and use `queueDepth` as
//! documented; how many frames ahead they go is not measured. Untested on a server.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Frames held before the oldest is acknowledged anyway.
pub const MAX_HELD: usize = 3;
/// Longest hold of one acknowledgement.
pub const MAX_HOLD: Duration = Duration::from_millis(100);

/// One acknowledgement to send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ack {
    pub frame_id: u32,
    pub total_frames_decoded: u32,
    /// Raw `queueDepth` (0 = unavailable).
    pub queue_depth: u32,
}

#[derive(Debug, Clone, Copy)]
struct Held {
    frame_id: u32,
    total: u32,
    bytes: u64,
    at: Instant,
}

/// Counters for the metrics.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct AckCounts {
    /// Acknowledgements that waited for the presenter.
    pub held: u64,
    /// Held acknowledgements released by the limits, not by the presenter.
    pub forced: u64,
}

#[derive(Debug, Default)]
pub struct AckGate {
    held: VecDeque<Held>,
    behind: bool,
    counts: AckCounts,
}

impl AckGate {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn counts(&self) -> AckCounts {
        self.counts
    }

    pub fn held(&self) -> usize {
        self.held.len()
    }

    /// The presenter state before the next frames arrive.
    pub fn set_behind(&mut self, behind: bool) {
        self.behind = behind;
    }

    fn depth(&self) -> u32 {
        let b: u64 = self.held.iter().map(|h| h.bytes).sum();
        // 0 means "unavailable"; 0xFFFFFFFF means "suspend": never send that by accident.
        u32::try_from(b).unwrap_or(u32::MAX - 1).min(u32::MAX - 1)
    }

    fn pop(&mut self) -> Option<Ack> {
        let h = self.held.pop_front()?;
        Some(Ack { frame_id: h.frame_id, total_frames_decoded: h.total, queue_depth: self.depth() })
    }

    fn release_due(&mut self, now: Instant, out: &mut Vec<Ack>) {
        while self.held.len() > MAX_HELD || self.held.front().is_some_and(|h| now.saturating_duration_since(h.at) >= MAX_HOLD) {
            if let Some(a) = self.pop() {
                self.counts.forced += 1;
                out.push(a);
            }
        }
    }

    /// IronRDP acknowledged `frame_id` (`EndFrame` decoded); `bytes`: the frame's PDUs.
    pub fn end_frame(&mut self, frame_id: u32, total_frames_decoded: u32, bytes: u64, now: Instant) -> Vec<Ack> {
        if !self.behind && self.held.is_empty() {
            return vec![Ack { frame_id, total_frames_decoded, queue_depth: 0 }];
        }
        self.counts.held += 1;
        self.held.push_back(Held { frame_id, total: total_frames_decoded, bytes, at: now });
        let mut out = Vec::new();
        if !self.behind {
            // Caught up while frames were held: everything in order.
            while let Some(a) = self.pop() {
                out.push(a);
            }
        } else {
            self.release_due(now, &mut out);
        }
        out
    }

    /// After a present (or on a quiet socket): release what the presenter state allows.
    pub fn poll(&mut self, behind: bool, now: Instant) -> Vec<Ack> {
        self.behind = behind;
        let mut out = Vec::new();
        if !behind {
            while let Some(a) = self.pop() {
                out.push(a);
            }
        } else {
            self.release_due(now, &mut out);
        }
        out
    }

    /// A graphics protocol reset or a closed channel: held frames belong to the old
    /// stream and are dropped.
    pub fn reset(&mut self) {
        self.held.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acknowledges_at_once_when_the_presenter_keeps_up() {
        let mut g = AckGate::new();
        let t = Instant::now();
        assert_eq!(g.end_frame(1, 1, 500, t), vec![Ack { frame_id: 1, total_frames_decoded: 1, queue_depth: 0 }]);
        assert_eq!(g.counts(), AckCounts::default());
    }

    #[test]
    fn holds_while_behind_and_releases_in_order() {
        let mut g = AckGate::new();
        let t = Instant::now();
        g.set_behind(true);
        assert!(g.end_frame(1, 1, 100, t).is_empty());
        assert!(g.end_frame(2, 2, 200, t).is_empty());
        assert_eq!(g.held(), 2);
        assert!(g.poll(true, t + Duration::from_millis(10)).is_empty(), "still behind");
        let out = g.poll(false, t + Duration::from_millis(20));
        assert_eq!(
            out,
            vec![
                Ack { frame_id: 1, total_frames_decoded: 1, queue_depth: 200 },
                Ack { frame_id: 2, total_frames_decoded: 2, queue_depth: 0 },
            ]
        );
        assert_eq!(g.counts(), AckCounts { held: 2, forced: 0 });
        // Order is kept even when a new frame comes while older ones are held.
        g.set_behind(true);
        g.end_frame(3, 3, 10, t);
        g.set_behind(false);
        let out = g.end_frame(4, 4, 10, t);
        assert_eq!(out.iter().map(|a| a.frame_id).collect::<Vec<_>>(), vec![3, 4]);
    }

    #[test]
    fn limits_release_the_oldest() {
        let mut g = AckGate::new();
        let t = Instant::now();
        g.set_behind(true);
        for i in 1..=MAX_HELD as u32 {
            assert!(g.end_frame(i, i, 1, t).is_empty());
        }
        let out = g.end_frame(9, 9, 1, t);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].frame_id, 1);
        assert_eq!(out[0].queue_depth, MAX_HELD as u32);
        let out = g.poll(true, t + MAX_HOLD);
        assert_eq!(out.len(), MAX_HELD, "all aged out");
        assert_eq!(g.counts().forced, 1 + MAX_HELD as u64);
        assert_eq!(g.held(), 0);
    }

    #[test]
    fn depth_never_reads_as_suspend_and_reset_drops() {
        let mut g = AckGate::new();
        let t = Instant::now();
        g.set_behind(true);
        g.end_frame(1, 1, u64::MAX / 4, t);
        g.end_frame(2, 2, u64::MAX / 4, t);
        let out = g.poll(false, t);
        assert_eq!(out[0].queue_depth, u32::MAX - 1);
        g.set_behind(true);
        g.end_frame(3, 3, 1, t);
        g.reset();
        assert!(g.poll(false, t).is_empty());
    }
}
