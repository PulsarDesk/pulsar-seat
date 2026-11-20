// SPDX-License-Identifier: GPL-3.0-only
//! Drives the Guide-button guard (OSMS parity G5, 2026-11-18; `seat_compat::guide_guard`)
//! from the service's 1 s pad tick.
//!
//! The wanted state ([`Service::guide_guard_want`]) is recomputed every tick; the platform
//! (journal + registry) is called only when it changes, once at the first tick (which
//! reverts whatever a crashed run left: the want then holds only what is running now), and
//! every [`RETRY`] after a failure. Service stop syncs to "nothing wanted" (everything ours
//! restored). Seat stop is covered by the want shrinking.

use std::time::{Duration, Instant};

use seat_compat::guide_guard::GuardWant;

use crate::log;
use crate::service::Service;

/// Retry a failed sync after this long.
pub const RETRY: Duration = Duration::from_secs(30);

#[derive(Default)]
pub struct GuideGuard {
    /// Want as last handed to the platform (`None` before the first sync).
    last: Option<GuardWant>,
    /// Set after a failure: sync again at this time even without a change.
    retry_at: Option<Instant>,
    last_error: Option<String>,
}

impl GuideGuard {
    pub fn new() -> Self {
        Self::default()
    }

    /// One tick. Returns `true` when the platform was called.
    pub fn tick(&mut self, s: &Service, remote_seats_running: bool, now: Instant) -> bool {
        let want = s.guide_guard_want(remote_seats_running);
        let due = self.last.as_ref() != Some(&want) || self.retry_at.is_some_and(|t| now >= t);
        if !due {
            return false;
        }
        self.apply(s, want, now);
        true
    }

    fn apply(&mut self, s: &Service, want: GuardWant, now: Instant) {
        match s.platform.sync_guide_guard(&want) {
            Ok(summary) => {
                if !summary.is_empty() {
                    log::info(&format!("guide guard: {summary} ({} users, controller VK mapping {})", want.sids.len(), if want.vk_mapping_off { "off" } else { "ours restored" }));
                }
                self.retry_at = None;
                self.last_error = None;
            }
            Err(e) => {
                if self.last_error.as_deref() != Some(e.as_str()) {
                    log::warn(&format!("guide guard: {e}"));
                }
                self.last_error = Some(e);
                self.retry_at = Some(now + RETRY);
            }
        }
        self.last = Some(want);
    }

    /// Service stop: every value ours goes back.
    pub fn shutdown(&mut self, s: &Service) {
        self.apply(s, GuardWant::default(), Instant::now());
    }
}
