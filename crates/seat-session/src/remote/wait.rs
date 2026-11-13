// SPDX-License-Identifier: GPL-3.0-only
//! Deadline-bounded polling with an injected clock (testable timeouts).

use crate::remote::{Clock, Sleeper};
use std::time::Duration;

/// Polls `probe` every `step` until it returns `Some`, or `timeout` elapses (then `None`).
/// `probe` runs at least once.
pub fn poll_until<T>(clock: &dyn Clock, sleeper: &dyn Sleeper, timeout: Duration, step: Duration, mut probe: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = clock.now_ms().saturating_add(u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX));
    loop {
        if let Some(v) = probe() {
            return Some(v);
        }
        let now = clock.now_ms();
        if now >= deadline {
            return None;
        }
        let left = Duration::from_millis(deadline - now);
        sleeper.sleep(step.min(left));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::fake::FakeClock;

    #[test]
    fn times_out_on_fake_clock() {
        let c = FakeClock::default();
        let mut n = 0;
        let r: Option<()> = poll_until(&c, &c, Duration::from_secs(2), Duration::from_millis(300), || {
            n += 1;
            None
        });
        assert!(r.is_none());
        assert_eq!(c.now_ms(), 2000);
        assert_eq!(n, 8); // t = 0, 300, …, 1800, 2000
    }

    #[test]
    fn returns_first_hit() {
        let c = FakeClock::default();
        let r = poll_until(&c, &c, Duration::from_secs(10), Duration::from_secs(1), || (c.now_ms() >= 3000).then_some(c.now_ms()));
        assert_eq!(r, Some(3000));
    }
}
