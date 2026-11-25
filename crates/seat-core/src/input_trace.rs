// SPDX-License-Identifier: GPL-3.0-only
//! Per-stage input rate meters (added 2026-11-25, gaming PC: the seat pointer moved in
//! jumps about once a second while the picture was smooth). Each stage of the local input
//! path (USB reader, service publish, pipe push, host receive, RDP send, in-session pointer)
//! keeps one [`InputMeter`] and logs one line about once a second, only while events flow:
//! events in the window, rate, the longest gap between two events, and the longest time
//! spent working on / waiting for one event. A stage that holds events back shows up as a
//! large `max_gap` with a low rate at that stage and a normal rate before it.
//!
//! Pure (no I/O); the caller logs the line in its own log.

use std::time::{Duration, Instant};

/// Length of one reporting window.
pub const WINDOW: Duration = Duration::from_secs(1);

/// Counters of one stage for the current window.
#[derive(Debug, Clone)]
pub struct InputMeter {
    stage: &'static str,
    window_start: Option<Instant>,
    last: Option<Instant>,
    count: u64,
    max_gap: Duration,
    busy_max: Duration,
    busy_total: Duration,
    wait_max: Duration,
    depth_max: u64,
}

impl InputMeter {
    pub fn new(stage: &'static str) -> Self {
        Self {
            stage,
            window_start: None,
            last: None,
            count: 0,
            max_gap: Duration::ZERO,
            busy_max: Duration::ZERO,
            busy_total: Duration::ZERO,
            wait_max: Duration::ZERO,
            depth_max: 0,
        }
    }

    /// One event at `now`. The gap to the previous event counts only inside a burst
    /// (gaps over [`WINDOW`] are idle time, not a stall).
    pub fn hit(&mut self, now: Instant) {
        if self.window_start.is_none() {
            self.window_start = Some(now);
        }
        if let Some(l) = self.last {
            let gap = now.saturating_duration_since(l);
            if gap <= WINDOW && gap > self.max_gap {
                self.max_gap = gap;
            }
        }
        self.last = Some(now);
        self.count += 1;
    }

    /// Time spent working on one event (translate, encode, write...).
    pub fn busy(&mut self, d: Duration) {
        self.busy_total += d;
        if d > self.busy_max {
            self.busy_max = d;
        }
    }

    /// Time spent waiting for something before the event could go on (a lock, a blocked
    /// read, a pipe write, other work of the same loop).
    pub fn wait(&mut self, d: Duration) {
        if d > self.wait_max {
            self.wait_max = d;
        }
    }

    /// Items queued at this stage when an event was taken.
    pub fn depth(&mut self, n: u64) {
        if n > self.depth_max {
            self.depth_max = n;
        }
    }

    /// The line for a window that is at least [`WINDOW`] old at `now` and had events;
    /// resets the window. `None` otherwise.
    pub fn take_line(&mut self, now: Instant) -> Option<String> {
        let start = self.window_start?;
        let span = now.saturating_duration_since(start);
        if span < WINDOW || self.count == 0 {
            return None;
        }
        let secs = span.as_secs_f64().max(0.001);
        let line = format!(
            "input-trace {}: n={} in {:.2}s ({:.0}/s) max_gap={:.1}ms busy_max={:.2}ms busy_avg={:.3}ms wait_max={:.1}ms depth_max={}",
            self.stage,
            self.count,
            secs,
            self.count as f64 / secs,
            ms(self.max_gap),
            ms(self.busy_max),
            ms(self.busy_total) / self.count as f64,
            ms(self.wait_max),
            self.depth_max
        );
        let last = self.last;
        *self = Self::new(self.stage);
        // The next window measures its first gap from the last event of this one.
        self.last = last;
        Some(line)
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_after_one_second_with_rate_and_gap() {
        let t0 = Instant::now();
        let mut m = InputMeter::new("usb");
        for i in 0..10u64 {
            m.hit(t0 + Duration::from_millis(i * 10));
            m.busy(Duration::from_micros(100));
        }
        // A 900 ms hole, then one more event.
        m.hit(t0 + Duration::from_millis(990));
        m.wait(Duration::from_millis(5));
        m.depth(3);
        assert!(m.take_line(t0 + Duration::from_millis(500)).is_none(), "window not over");
        let l = m.take_line(t0 + Duration::from_millis(1000)).unwrap();
        assert!(l.starts_with("input-trace usb: n=11 in 1.00s (11/s) max_gap=900.0ms"), "{l}");
        assert!(l.contains("wait_max=5.0ms depth_max=3"), "{l}");
        // Reset, and nothing to say without events.
        assert!(m.take_line(t0 + Duration::from_secs(5)).is_none());
    }

    #[test]
    fn idle_gaps_are_not_stalls() {
        let t0 = Instant::now();
        let mut m = InputMeter::new("x");
        m.hit(t0);
        m.hit(t0 + Duration::from_secs(3));
        m.hit(t0 + Duration::from_millis(3010));
        let l = m.take_line(t0 + Duration::from_secs(4)).unwrap();
        assert!(l.contains("max_gap=10.0ms"), "{l}");
    }
}
