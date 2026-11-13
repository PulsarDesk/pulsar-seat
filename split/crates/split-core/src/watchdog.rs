// SPDX-License-Identifier: GPL-3.0-only
//! Stream-server watchdog (spec 20 §6.3) as a pure state machine.
//!
//! Unlike a plain restart loop it backs off exponentially, resets the back-off after the
//! process ran stably for a while, and gives up after too many crashes (consecutive, or
//! within a sliding window: spec 80 §10 "5 failures in 5 min") so the failure is surfaced
//! to the manager instead of filling the log. Time is passed in by the
//! caller as milliseconds since any fixed origin, which keeps this testable.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// Delay before the first restart.
    pub initial_ms: u64,
    /// Upper bound for the delay.
    pub max_ms: u64,
    /// A run at least this long counts as stable and resets the consecutive-crash counter.
    pub stable_ms: u64,
    /// Give up after this many consecutive crashes.
    pub max_consecutive: u32,
    /// Give up when `max_in_window` crashes fall within `window_ms` (0 = rule off).
    pub window_ms: u64,
    pub max_in_window: u32,
}

impl Default for Policy {
    /// Spec 80 §10: back-off 1 s doubling to 60 s, give up after 5 failures in 5 min.
    fn default() -> Self {
        Self {
            initial_ms: 1_000,
            max_ms: 60_000,
            stable_ms: 60_000,
            max_consecutive: u32::MAX,
            window_ms: 300_000,
            max_in_window: 5,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Not supervising (seat stopped).
    Idle,
    /// Caller was told to start the process and has not reported back yet.
    Starting,
    Running { since_ms: u64 },
    /// Waiting until `until_ms` before the next start.
    Backoff { until_ms: u64 },
    /// Gave up; stays here until [`Watchdog::disarm`] / [`Watchdog::arm`].
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    None,
    /// Start the process now and report with `on_started` / `on_start_failed`.
    StartNow,
    /// Restart is scheduled; call `poll` again at or after this time.
    WaitUntil(u64),
    /// Too many crashes; mark the seat failed and tell the operator.
    GiveUp,
}

#[derive(Debug, Clone)]
pub struct Watchdog {
    policy: Policy,
    phase: Phase,
    consecutive: u32,
    total: u32,
    /// Times of recent crashes (only those inside `window_ms`).
    recent: std::collections::VecDeque<u64>,
}

impl Watchdog {
    pub fn new(policy: Policy) -> Self {
        Self { policy, phase: Phase::Idle, consecutive: 0, total: 0, recent: Default::default() }
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// Crashes since the last `arm`.
    pub fn total_crashes(&self) -> u32 {
        self.total
    }

    pub fn consecutive_crashes(&self) -> u32 {
        self.consecutive
    }

    /// Begin supervising (seat start). Resets the counters.
    pub fn arm(&mut self) -> Action {
        self.consecutive = 0;
        self.total = 0;
        self.recent.clear();
        self.phase = Phase::Starting;
        Action::StartNow
    }

    /// Stop supervising (seat stop). Exits reported afterwards are ignored.
    pub fn disarm(&mut self) {
        self.phase = Phase::Idle;
    }

    pub fn on_started(&mut self, now_ms: u64) {
        if self.phase == Phase::Starting {
            self.phase = Phase::Running { since_ms: now_ms };
        }
    }

    /// The process could not be created at all; handled like an immediate crash.
    pub fn on_start_failed(&mut self, now_ms: u64) -> Action {
        if self.phase != Phase::Starting {
            return Action::None;
        }
        self.crashed(now_ms)
    }

    /// The supervised process exited.
    pub fn on_exit(&mut self, now_ms: u64) -> Action {
        match self.phase {
            Phase::Running { since_ms } => {
                if now_ms.saturating_sub(since_ms) >= self.policy.stable_ms {
                    self.consecutive = 0;
                }
                self.crashed(now_ms)
            }
            _ => Action::None,
        }
    }

    /// Call periodically / when a scheduled time is reached.
    pub fn poll(&mut self, now_ms: u64) -> Action {
        match self.phase {
            Phase::Backoff { until_ms } if now_ms >= until_ms => {
                self.phase = Phase::Starting;
                Action::StartNow
            }
            Phase::Backoff { until_ms } => Action::WaitUntil(until_ms),
            _ => Action::None,
        }
    }

    /// Delay before restart number `n` (1-based) of a crash streak.
    pub fn delay_for(&self, n: u32) -> u64 {
        let shift = n.saturating_sub(1).min(32);
        self.policy.initial_ms.saturating_mul(1u64 << shift).min(self.policy.max_ms)
    }

    fn crashed(&mut self, now_ms: u64) -> Action {
        self.consecutive += 1;
        self.total += 1;
        let windowed = self.policy.window_ms > 0 && self.policy.max_in_window > 0 && {
            self.recent.push_back(now_ms);
            while self.recent.front().is_some_and(|&t| now_ms.saturating_sub(t) >= self.policy.window_ms) {
                self.recent.pop_front();
            }
            self.recent.len() as u32 >= self.policy.max_in_window
        };
        if windowed || self.consecutive >= self.policy.max_consecutive {
            self.phase = Phase::Failed;
            return Action::GiveUp;
        }
        let until_ms = now_ms.saturating_add(self.delay_for(self.consecutive));
        self.phase = Phase::Backoff { until_ms };
        Action::WaitUntil(until_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wd() -> Watchdog {
        Watchdog::new(Policy { initial_ms: 1000, max_ms: 8000, stable_ms: 30_000, max_consecutive: 6, window_ms: 0, max_in_window: 0 })
    }

    #[test]
    fn default_gives_up_after_five_failures_in_five_minutes() {
        let mut w = Watchdog::new(Policy::default());
        w.arm();
        let mut now = 0;
        let mut crashes = 0;
        // Each run lasts 61 s (stable: resets the streak), so only the window rule can fire.
        loop {
            w.on_started(now);
            now += 61_000;
            crashes += 1;
            match w.on_exit(now) {
                Action::WaitUntil(t) => {
                    now = t;
                    assert_eq!(w.poll(now), Action::StartNow);
                }
                Action::GiveUp => break,
                o => panic!("{o:?}"),
            }
        }
        assert_eq!(crashes, 5, "5 crashes within 5 min");
        // Crashes spaced further apart never give up.
        let mut w = Watchdog::new(Policy::default());
        w.arm();
        let mut now = 0;
        for _ in 0..20 {
            w.on_started(now);
            now += 80_000;
            let Action::WaitUntil(t) = w.on_exit(now) else { panic!("gave up") };
            now = t;
            w.poll(now);
        }
    }

    #[test]
    fn exponential_backoff_with_cap() {
        let w = wd();
        let delays: Vec<u64> = (1..=6).map(|n| w.delay_for(n)).collect();
        assert_eq!(delays, [1000, 2000, 4000, 8000, 8000, 8000]);
        assert_eq!(w.delay_for(1000), 8000, "huge n must not overflow");
    }

    #[test]
    fn crash_loop_backs_off_then_gives_up() {
        let mut w = wd();
        let mut now = 0;
        assert_eq!(w.arm(), Action::StartNow);
        let mut waits = Vec::new();
        loop {
            w.on_started(now);
            now += 100; // crashes quickly
            match w.on_exit(now) {
                Action::WaitUntil(t) => {
                    waits.push(t - now);
                    assert_eq!(w.poll(t - 1), Action::WaitUntil(t), "must not restart early");
                    now = t;
                    assert_eq!(w.poll(now), Action::StartNow);
                }
                Action::GiveUp => break,
                other => panic!("unexpected {other:?}"),
            }
        }
        assert_eq!(waits, [1000, 2000, 4000, 8000, 8000]);
        assert_eq!(w.phase(), Phase::Failed);
        assert_eq!(w.total_crashes(), 6);
        assert_eq!(w.poll(now + 1_000_000), Action::None, "failed stays failed");
    }

    #[test]
    fn stable_run_resets_backoff() {
        let mut w = wd();
        w.arm();
        w.on_started(0);
        assert_eq!(w.on_exit(10), Action::WaitUntil(1010));
        w.poll(1010);
        w.on_started(1010);
        assert_eq!(w.on_exit(1020), Action::WaitUntil(1020 + 2000));
        w.poll(3020);
        w.on_started(3020);
        // Runs stably for longer than stable_ms, then crashes: back to the initial delay.
        let t = 3020 + 30_000;
        assert_eq!(w.on_exit(t), Action::WaitUntil(t + 1000));
        assert_eq!(w.consecutive_crashes(), 1);
        assert_eq!(w.total_crashes(), 3);
    }

    #[test]
    fn start_failure_counts_as_crash() {
        let mut w = wd();
        w.arm();
        assert_eq!(w.on_start_failed(0), Action::WaitUntil(1000));
        assert_eq!(w.on_start_failed(5), Action::None, "only valid while Starting");
    }

    #[test]
    fn disarm_ignores_late_exit_and_arm_resets() {
        let mut w = wd();
        w.arm();
        w.on_started(0);
        w.disarm();
        assert_eq!(w.on_exit(5), Action::None);
        assert_eq!(w.phase(), Phase::Idle);
        w.arm();
        assert_eq!(w.total_crashes(), 0);
        assert_eq!(w.phase(), Phase::Starting);
    }
}
