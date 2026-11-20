// SPDX-License-Identifier: GPL-3.0-only
//! Service side of the console lock guard (OSMS-style local seat parity, gap G4,
//! 2026-11-18). The values themselves are written and journaled by the platform
//! (`seat_compat::console_guard`); this module decides **whose** and **when**:
//!
//! - `dedicated_host` + `settings.console_guard.host_lock_off` (default on): lock and screen
//!   saver off for the account signed in at the console (the host account). Permanent while
//!   both stay on; undone when either is turned off or the product is disabled. While nobody
//!   is signed in at the console the last state is kept (no churn at boot before the
//!   auto-logon).
//! - `seat_one_console` + `settings.console_guard.seat_one_lock_off` (opt-in): lock off for
//!   seat 1's user while at least one other seat is in use (engage), undone when the last
//!   one stops, on service stop and on disable (release). The first sync of every service
//!   run releases an entry a crash left behind when no seat runs.
//!
//! Synced from the tick (cheap: the platform is called only when the wanted state changes)
//! and again after every config change (`sync_tweaks`). Uninstall and reset undo everything
//! through the journal.

use crate::log;
use crate::platform::GuardOwner;
use crate::service::{Service, State};
use seat_core::backend::session::HostTopology;
use seat_core::config::ConsoleGuard;

/// What the platform was last asked for (SIDs; `None` = off).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct GuardState {
    pub host: Option<String>,
    pub seat_one: Option<String>,
}

/// Which guards the config wants on now: (host, seat one).
pub(crate) fn wanted(enabled: bool, topology: HostTopology, guard: ConsoleGuard, stopping: bool, seats_in_use: bool) -> (bool, bool) {
    let dedicated = topology == HostTopology::DedicatedHost;
    let host = enabled && dedicated && guard.host_lock_off;
    let seat_one = enabled && !dedicated && guard.seat_one_lock_off && seats_in_use && !stopping;
    (host, seat_one)
}

/// The next sync: (host target, seat-one target). Host `None` = leave it as it is (nobody
/// signed in at the console, guard on); `Some(x)` = sync to `x`.
pub(crate) fn targets(
    prev: Option<&GuardState>,
    host_on: bool,
    seat_one_on: bool,
    console_sid: Option<&str>,
) -> (Option<Option<String>>, Option<Option<String>>) {
    let host_want: Option<Option<String>> = if !host_on { Some(None) } else { console_sid.map(|s| Some(s.to_string())) };
    let seat_want: Option<String> = if seat_one_on { console_sid.map(str::to_string) } else { None };
    let host = host_want.filter(|h| prev.is_none_or(|p| p.host != *h));
    let seat = Some(seat_want).filter(|s| prev.is_none_or(|p| p.seat_one != *s));
    (host, seat)
}

impl Service {
    /// Another seat than the console seat is in use (the same rule as the keep-awake
    /// request).
    fn guard_seats_in_use(st: &State) -> bool {
        st.seats.values().any(|s| !s.seat.is_console() && Self::managed(st, s.seat) && (s.is_active() || s.host.is_some()))
    }

    /// The console checks of the preflight list (`seat_compat::preflight::console_checks`).
    pub(crate) fn console_checks(&self) -> Vec<seat_proto::messages::Check> {
        let (dedicated, guard, others) = {
            let st = self.lock();
            let c = &st.config;
            let others = c.seats.iter().any(|s| s.is_local() && !s.number.is_console());
            (c.settings.backends.host_topology == HostTopology::DedicatedHost, c.settings.console_guard, others)
        };
        let sleep = if dedicated { self.platform.sleep_off_state() } else { None };
        seat_compat::preflight::console_checks(dedicated, guard.host_lock_off, guard.seat_one_lock_off, others, sleep)
    }

    /// Bring the console lock guard in line with the config and the running seats.
    pub(crate) fn update_console_guard(&self) {
        let (host_on, seat_one_on, prev) = {
            let st = self.lock();
            let s = &st.config.settings;
            let (h, o) = wanted(s.enabled, s.backends.host_topology, s.console_guard, self.is_stopping(), Self::guard_seats_in_use(&st));
            (h, o, st.console_guard.clone())
        };
        let console_sid = if host_on || seat_one_on {
            self.platform.console_session().and_then(|id| self.platform.session_user(id)).map(|u| u.sid)
        } else {
            None
        };
        let (host, seat_one) = targets(prev.as_ref(), host_on, seat_one_on, console_sid.as_deref());
        if host.is_none() && seat_one.is_none() && prev.is_some() {
            return;
        }
        let mut next = prev.unwrap_or_default();
        for (owner, target) in [(GuardOwner::Host, host), (GuardOwner::SeatOne, seat_one)] {
            let Some(target) = target else { continue };
            let what = match owner {
                GuardOwner::Host => "host account",
                GuardOwner::SeatOne => "seat 1",
            };
            match self.platform.sync_console_guard(owner, target.as_deref()) {
                Ok(s) if !s.is_empty() => log::info(&format!("console lock guard ({what}): {s}")),
                Ok(_) => {}
                Err(e) => {
                    // Recorded as done anyway: the failure is logged once, retried at the next
                    // config change or service start (the journal keeps what was written).
                    log::warn(&format!("console lock guard ({what}): {e}"));
                    self.notice("tweak_failed", None, format!("console_guard: {e}"));
                }
            }
            match owner {
                GuardOwner::Host => next.host = target,
                GuardOwner::SeatOne => next.seat_one = target,
            }
        }
        self.lock().console_guard = Some(next);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SID: &str = "S-1-5-21-1-2-3-1001";

    #[test]
    fn wanted_follows_topology_and_options() {
        let g = ConsoleGuard::default();
        assert_eq!(wanted(true, HostTopology::DedicatedHost, g, false, false), (true, false));
        assert_eq!(wanted(false, HostTopology::DedicatedHost, g, false, true), (false, false), "product off");
        assert_eq!(wanted(true, HostTopology::SeatOneConsole, g, false, true), (false, false), "seat 1 opt-in off");
        let opt = ConsoleGuard { host_lock_off: false, seat_one_lock_off: true };
        assert_eq!(wanted(true, HostTopology::SeatOneConsole, opt, false, true), (false, true));
        assert_eq!(wanted(true, HostTopology::SeatOneConsole, opt, false, false), (false, false), "no seat runs");
        assert_eq!(wanted(true, HostTopology::SeatOneConsole, opt, true, true), (false, false), "service stopping");
        assert_eq!(wanted(true, HostTopology::DedicatedHost, opt, false, true), (false, false), "host guard off");
    }

    #[test]
    fn targets_sync_only_changes_and_keep_host_without_console_user() {
        // First sync of a run: both are synced (a crash's seat-1 entry is released).
        assert_eq!(targets(None, false, false, None), (Some(None), Some(None)));
        // Host on, nobody at the console yet: keep the host guard, still release seat 1.
        assert_eq!(targets(None, true, false, None), (None, Some(None)));
        let on = GuardState { host: Some(SID.into()), seat_one: None };
        assert_eq!(targets(Some(&on), true, false, Some(SID)), (None, None));
        assert_eq!(targets(Some(&on), true, false, None), (None, None), "console user signed out: kept");
        assert_eq!(targets(Some(&on), false, false, None), (Some(None), None), "topology changed: undone");
        let seat = GuardState { host: None, seat_one: Some(SID.into()) };
        assert_eq!(targets(Some(&GuardState::default()), false, true, Some(SID)), (None, Some(Some(SID.into()))));
        assert_eq!(targets(Some(&seat), false, false, Some(SID)), (None, Some(None)), "last seat stopped");
    }
}
