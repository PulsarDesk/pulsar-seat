// SPDX-License-Identifier: GPL-3.0-only
//! Latency measurement hooks (added 2026-11-17): one [`LatencyStats`] per pipeline stage
//! (EGFX PDU received to decoded, decoded to handed to the presenter, IddCx frame acquired
//! to presented, ...). Samples go into a fixed ring; [`LatencyStats::summary`] gives count,
//! p50, p95, p99 and max for the log line / metrics event. Pure and allocation-free after
//! creation, so it can sit on every frame path.
//!
//! Clocks: callers pass durations they measured with `Instant`, or QPC tick pairs through
//! [`ticks_to_duration`] (the IddCx driver stamps frames with QueryPerformanceCounter; the
//! seat host reads the counter itself, `seat_display::win::qpc_now`).

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Samples kept per stage (the percentiles cover the most recent ones).
pub const LATENCY_WINDOW: usize = 512;

/// Rolling latency samples of one stage.
#[derive(Debug, Clone)]
pub struct LatencyStats {
    ring: Vec<u32>, // microseconds
    next: usize,
    filled: usize,
    total: u64,
    max_us: u32,
}

impl Default for LatencyStats {
    fn default() -> Self {
        Self::new()
    }
}

/// Snapshot of one stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LatencySummary {
    /// Samples since creation (not only the window).
    pub count: u64,
    pub p50_us: u32,
    pub p95_us: u32,
    pub p99_us: u32,
    /// Largest sample since creation.
    pub max_us: u32,
}

impl LatencyStats {
    pub fn new() -> Self {
        Self { ring: vec![0; LATENCY_WINDOW], next: 0, filled: 0, total: 0, max_us: 0 }
    }

    pub fn record(&mut self, d: Duration) {
        let us = d.as_micros().min(u32::MAX as u128) as u32;
        self.ring[self.next] = us;
        self.next = (self.next + 1) % LATENCY_WINDOW;
        self.filled = (self.filled + 1).min(LATENCY_WINDOW);
        self.total += 1;
        self.max_us = self.max_us.max(us);
    }

    pub fn count(&self) -> u64 {
        self.total
    }

    pub fn summary(&self) -> LatencySummary {
        if self.filled == 0 {
            return LatencySummary::default();
        }
        let mut v: Vec<u32> = self.ring[..self.filled].to_vec();
        v.sort_unstable();
        let pick = |q: f64| v[(((v.len() - 1) as f64) * q).round() as usize];
        LatencySummary { count: self.total, p50_us: pick(0.50), p95_us: pick(0.95), p99_us: pick(0.99), max_us: self.max_us }
    }

    /// Start over (a new stream / connection).
    pub fn reset(&mut self) {
        *self = Self::new();
    }
}

impl std::fmt::Display for LatencySummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "n={} p50={:.1}ms p95={:.1}ms p99={:.1}ms max={:.1}ms",
            self.count,
            self.p50_us as f64 / 1000.0,
            self.p95_us as f64 / 1000.0,
            self.p99_us as f64 / 1000.0,
            self.max_us as f64 / 1000.0
        )
    }
}

/// `later - earlier` QPC ticks at `freq` ticks per second; `None` when the order is wrong
/// or a stamp is missing (0).
pub fn ticks_to_duration(earlier: u64, later: u64, freq: u64) -> Option<Duration> {
    if earlier == 0 || later == 0 || later < earlier || freq == 0 {
        return None;
    }
    let d = later - earlier;
    Some(Duration::from_nanos(((d as u128 * 1_000_000_000) / freq as u128).min(u64::MAX as u128) as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_over_the_window() {
        let mut s = LatencyStats::new();
        assert_eq!(s.summary(), LatencySummary::default());
        for ms in 1..=100u64 {
            s.record(Duration::from_millis(ms));
        }
        let m = s.summary();
        assert_eq!(m.count, 100);
        assert_eq!(m.p50_us, 51_000);
        assert_eq!(m.p95_us, 95_000);
        assert_eq!(m.max_us, 100_000);
        // The window rolls; the max does not.
        for _ in 0..LATENCY_WINDOW {
            s.record(Duration::from_millis(2));
        }
        let m = s.summary();
        assert_eq!((m.p50_us, m.p99_us, m.max_us), (2_000, 2_000, 100_000));
        assert!(m.to_string().contains("p95=2.0ms"));
        s.reset();
        assert_eq!(s.count(), 0);
    }

    #[test]
    fn qpc_tick_math() {
        assert_eq!(ticks_to_duration(100, 1_100, 10_000_000), Some(Duration::from_micros(100)));
        assert_eq!(ticks_to_duration(0, 10, 10), None, "missing stamp");
        assert_eq!(ticks_to_duration(10, 5, 10), None, "wrong order");
        assert_eq!(ticks_to_duration(1, 2, 0), None);
    }
}
