// SPDX-License-Identifier: GPL-3.0-only
//! Seat 1's kiosk shell (maintainer decision 2026-11-15, docs/agents/plan.md, option a).
//!
//! Under `seat_one_console` seat 1 is the console and is never started through
//! `start_sequence`, so with `settings.kiosk.seat1_desktop = false` its kiosk shell was
//! never launched (VM test 2026-11-15). The service now runs it here, under the same rules
//! as the other kiosk seats: started while the product is enabled (enable, boot, every
//! tick), stopped on disable or when the config no longer wants it (the backend's
//! `log_off` stops the shell and its app job), restarted after a crash with the seat-host
//! back-off and the same crash limit inside [`crate::seats::CRASH_WINDOW_MS`].
//!
//! Seat 1 stays the console seat otherwise: its status still mirrors the console session.

use crate::seats::{CRASH_LIMIT, CRASH_WINDOW_MS, backoff_ms, client_name};
use crate::service::{Service, State};
use crate::{eventid, log};
use seat_core::SeatNumber;
use seat_core::backend::session::{SeatStartRequest, SessionBackendKind, SessionState, StartStep};
use seat_core::backend::ErrorKind;

/// Retry pause when the backend is not ready yet (no console user at boot).
pub const NOT_READY_RETRY_MS: u64 = 5_000;

#[derive(Debug, Default, Clone)]
pub(crate) struct Seat1Kiosk {
    /// Console session the running shell was started in.
    pub running: Option<u32>,
    /// Crash times inside [`CRASH_WINDOW_MS`].
    pub crashes: Vec<u64>,
    /// No start before this time (back-off).
    pub retry_at: Option<u64>,
    /// [`CRASH_LIMIT`] crashes: no more restarts until the shell is no longer wanted
    /// (disable, other backend or config) and wanted again.
    pub gave_up: bool,
}

impl Service {
    /// The product wants seat 1's kiosk shell now.
    pub(crate) fn seat1_kiosk_wanted(&self, st: &State) -> bool {
        let s = &st.config.settings;
        s.enabled
            && st.safe_reason.is_none()
            && !Self::dedicated(st)
            && self.backends().session.kind() == SessionBackendKind::Desktops
            && st.config.seat(SeatNumber::CONSOLE).is_some_and(|c| seat_session::kiosk::wants_shell(SeatNumber::CONSOLE, c, s))
    }

    /// Start, watch, restart or stop seat 1's kiosk shell (enable, boot, tick, disable).
    ///
    /// Serialised by `seat1_kiosk_op`: the enable thread and the tick both call this, and
    /// the backend start takes a while, so two unserialised calls both saw no running
    /// shell and both started one (VM 2026-11-15).
    pub(crate) fn sync_seat1_kiosk(&self) {
        let _op = self.seat1_kiosk_op.lock().unwrap_or_else(|p| p.into_inner());
        let now = self.now();
        let b = self.backends();
        let (wanted, k) = {
            let st = self.lock();
            (!self.is_stopping() && self.seat1_kiosk_wanted(&st), st.seat1_kiosk.clone())
        };
        if !wanted {
            // Not wanted (disabled, other backend, seat 1 is the desktop again): stop it and
            // forget the crash history, so a later enable starts fresh.
            if let Some(session) = k.running {
                match b.session.log_off(SeatNumber::CONSOLE, session) {
                    Ok(()) => log::info("seat 1: kiosk shell stopped"),
                    Err(e) => log::warn(&format!("seat 1: kiosk shell stop: {e}")),
                }
            }
            self.lock().seat1_kiosk = Seat1Kiosk::default();
            return;
        }
        if let Some(session) = k.running {
            let alive = b.session.query(SeatNumber::CONSOLE, session).is_ok_and(|i| i.state != SessionState::Gone);
            if alive {
                return;
            }
            let mut st = self.lock();
            let s = &mut st.seat1_kiosk;
            s.running = None;
            s.crashes.retain(|t| now.saturating_sub(*t) < CRASH_WINDOW_MS);
            s.crashes.push(now);
            if s.crashes.len() >= CRASH_LIMIT {
                s.gave_up = true;
                log::error_id(eventid::SEAT_HOST_EXITED, &format!("seat 1: kiosk shell exited {CRASH_LIMIT} times; not restarted"));
            } else {
                s.retry_at = Some(now + backoff_ms(s.crashes.len()));
                log::error_id(eventid::SEAT_HOST_EXITED, "seat 1: kiosk shell exited; restarting after a pause");
            }
            return;
        }
        if k.gave_up || k.retry_at.is_some_and(|t| now < t) {
            return;
        }
        let (cfg, settings) = {
            let st = self.lock();
            let Some(cfg) = st.config.seat(SeatNumber::CONSOLE).cloned() else { return };
            (cfg, st.config.settings.clone())
        };
        // Console apps of the kiosk console user open in conhost (crate::kiosk_console).
        self.sync_kiosk_console();
        let req = SeatStartRequest {
            seat: SeatNumber::CONSOLE,
            config: &cfg,
            settings: &settings,
            client_name: client_name(SeatNumber::CONSOLE),
            credentials: None,
            idd_outputs: None,
        };
        match b.session.start(&req) {
            Ok(StartStep::Started(info)) => {
                log::info(&format!("seat 1: kiosk shell running in session {}", info.session_id));
                let mut st = self.lock();
                st.seat1_kiosk.running = Some(info.session_id);
                st.seat1_kiosk.retry_at = None;
            }
            Ok(StartStep::LaunchHost(_)) => log::warn("seat 1: kiosk shell backend asked for a seat host"),
            // No console user yet (boot, logon screen): try again shortly, not a crash.
            Err(e) if e.kind == ErrorKind::NotReady => self.lock().seat1_kiosk.retry_at = Some(now + NOT_READY_RETRY_MS),
            Err(e) => {
                log::warn(&format!("seat 1: kiosk shell start: {e}"));
                let mut st = self.lock();
                let s = &mut st.seat1_kiosk;
                s.crashes.retain(|t| now.saturating_sub(*t) < CRASH_WINDOW_MS);
                s.crashes.push(now);
                s.gave_up = s.crashes.len() >= CRASH_LIMIT;
                s.retry_at = Some(now + backoff_ms(s.crashes.len()));
            }
        }
    }
}
