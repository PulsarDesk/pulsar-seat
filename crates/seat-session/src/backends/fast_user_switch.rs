// SPDX-License-Identifier: GPL-3.0-only
//! `fast_user_switch` (spec 30 §6.1 C): every seat has its own Windows session, but only
//! one is on the console (all monitors) at a time. Works on every edition and on ARM64;
//! the runtime fallback when the configured backend is not available.
//!
//! Start of seat N:
//! - seat 1 → the console session, which the backend remembers (session id + user SID,
//!   also from `adopt`). When seat 1 later starts again while another seat holds the
//!   console, that remembered session is connected back (`WTSConnectSessionW`) under the
//!   same lock / password rules as below; with no session to return to and the console
//!   being another seat's, the start is refused (`console_in_use`);
//! - the console session has a signed-in user whose desktop is **not locked** → refused
//!   (`console_in_use`): switching it away would hand that desktop over unlocked and kick
//!   its user without consent. This also makes unattended starts (boot, auto-start of
//!   seats 2..N) safe: they never switch an active user away; the service should not
//!   auto-start seats on this non-concurrent backend at all;
//! - a session of the seat's account exists (matched by **account SID**, never by name)
//!   → with the stored password `WTSConnectSessionW(that, console)` brings it to the
//!   console (the current console session is disconnected, not logged off). Without one the
//!   target must be locked (`session_not_locked` otherwise) and is **not** connected: a
//!   SYSTEM connect without a password shows a locked session unlocked (VM test
//!   2026-11-15). The console goes to a fresh logon screen instead, where the user signs in
//!   and Windows reconnects their session;
//! - otherwise → `WTSDisconnectSession(console)`: Windows shows a fresh logon screen on
//!   the console (a new session), where the seat's user signs in.
//!
//! Auto-logon: Pulsar Seat never writes the Winlogon auto-logon values for user accounts
//! (spec 30 §6.2.1). Instead, while the Pulsar credential provider is registered
//! ([`crate::credprov`], spec 35 §5.4), the service arms a one-time credential for the fresh
//! logon session this backend reports (`SessionState::AtLogon`, no user) and LogonUI's
//! provider in that session submits it; `caps().auto_logon` says whether it is registered.
//! Seen in the ARM64 Hyper-V test VM (2026-11-15, Windows 11 Pro 26300): the console
//! disconnect, the fresh logon session, the credential-provider auto-logon and
//! `WTSConnectSessionW` from session 0 as SYSTEM (which needs no password and unlocks the
//! target, hence the rule above). Not on hardware yet.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use seat_core::SeatNumber;
use seat_core::backend::session::{
    HostProbe, KnownSession, SeatStartRequest, SessionBackend, SessionBackendKind, SessionCaps, SessionInfo, SessionState,
    StartStep,
};
use seat_core::backend::{Availability, BackendResult, ErrorKind};
use seat_core::config::GlobalSettings;

use super::SessionDeps;
use crate::edition::arch_name;
use crate::error::{err, reason};
use crate::rds::fast_user_switching_disabled;
use crate::wts::WtsSession;

/// seat → (session id, `(domain, user)` of the seat account).
type SeatMap = HashMap<SeatNumber, (u32, Option<(String, String)>)>;

pub struct FastUserSwitchBackend {
    d: SessionDeps,
    /// seat → (session id, account) of the seat's session.
    seats: Mutex<SeatMap>,
    /// Seat 1's own session (id, user SID when known at record time): the console session
    /// when seat 1 started (or was adopted). Lets a later start of seat 1 connect it back.
    home: Mutex<Option<(u32, Option<String>)>>,
}

impl FastUserSwitchBackend {
    pub fn new(d: SessionDeps) -> Self {
        Self { d, seats: Mutex::new(HashMap::new()), home: Mutex::new(None) }
    }

    fn seats(&self) -> std::sync::MutexGuard<'_, SeatMap> {
        self.seats.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn home(&self) -> std::sync::MutexGuard<'_, Option<(u32, Option<String>)>> {
        self.home.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Seat 1's recorded session when it exists off the console: interactive, signed in,
    /// same user SID as recorded (a reused session id of another user is never taken), and
    /// not another seat's session.
    fn seat1_away(&self, console: u32) -> BackendResult<Option<u32>> {
        let Some((id, sid)) = self.home().clone() else { return Ok(None) };
        if id == console {
            return Ok(None);
        }
        let all = self.d.wts.sessions()?;
        let Some(s) = all.iter().find(|s| s.id == id && s.is_interactive() && !s.user.is_empty()) else { return Ok(None) };
        let now_sid = self.d.wts.user_sid(s.id);
        let same_user = match (&sid, &now_sid) {
            (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
            (Some(_), None) => false,
            // Seat 1 signed in after it was recorded: take the session's user from now on.
            (None, _) => true,
        };
        if !same_user || self.seats().iter().any(|(n, (x, _))| !n.is_console() && *x == id) {
            return Ok(None);
        }
        if sid.is_none() {
            *self.home() = Some((id, now_sid));
        }
        Ok(Some(id))
    }

    /// Record the console session as seat 1's when no live record exists, it has a user and
    /// it is no other seat's session.
    fn remember_home(&self, console: u32, all: &[WtsSession]) {
        let alive = |id: u32| all.iter().any(|s| s.id == id && s.is_interactive() && !s.user.is_empty());
        if self.home().as_ref().is_some_and(|(id, _)| alive(*id)) {
            return;
        }
        let other_seat = self.seats().iter().any(|(n, (id, _))| !n.is_console() && *id == console);
        if alive(console) && !other_seat {
            *self.home() = Some((console, self.d.wts.user_sid(console)));
            log::info!("seat 1: remembered console session {console} as seat 1's own");
        }
    }

    fn info_of(&self, id: u32) -> BackendResult<Option<SessionInfo>> {
        Ok(self.d.wts.sessions()?.iter().find(|s| s.id == id).map(|s| s.info(self.d.wts.user_sid(id))))
    }

    /// The console may be switched away: nobody signed in, or the desktop is locked.
    fn console_switchable(&self, console: u32, all: &[WtsSession]) -> BackendResult<()> {
        match all.iter().find(|s| s.id == console) {
            Some(s) if !s.user.is_empty() && s.locked != Some(true) => Err(err(
                ErrorKind::Policy,
                reason::CONSOLE_IN_USE,
                format!("console session {console} is signed in and not locked; lock it first (one seat at a time)"),
            )),
            _ => Ok(()),
        }
    }

    /// Interactive session of the seat account, matched by SID.
    fn existing_session(&self, account: &(String, String), all: &[WtsSession]) -> BackendResult<Option<u32>> {
        let (dom, user) = account;
        let sid = self
            .d
            .wts
            .account_sid(dom, user)
            .ok_or_else(|| err(ErrorKind::NotFound, reason::ACCOUNT_UNRESOLVED, format!("account {dom}\\{user} has no SID")))?;
        Ok(all
            .iter()
            .filter(|s| s.is_interactive() && !s.user.is_empty())
            .find(|s| self.d.wts.user_sid(s.id).is_some_and(|x| x.eq_ignore_ascii_case(&sid)))
            .map(|s| s.id))
    }

    /// Bring the existing session `id` onto the console. With the seat's stored password:
    /// `WTSConnectSessionW`. Without one, never: a connect from the SYSTEM service without a
    /// password shows a **locked** session's desktop unlocked (VM test 2026-11-15, Windows 11
    /// 26300 ARM64; the earlier HYPOTHESIS that Windows then asks for the password was
    /// false). The console is handed to a fresh logon screen instead, where the session's
    /// user signs in and Windows reconnects that session (its own fast user switching).
    /// A console at the logon screen already (no user) is left as it is.
    fn bring_to_console(&self, id: u32, console: u32, pw: Option<&seat_core::Secret>, all: &[WtsSession]) -> BackendResult<()> {
        if pw.is_some() {
            return Ok(self.d.wts.connect(id, console, pw)?);
        }
        if all.iter().any(|s| s.id == console && !s.user.is_empty()) {
            self.d.wts.disconnect(console)?;
            log::info!("session {id}: no password; console session {console} handed to the logon screen for its user");
        }
        Ok(())
    }

    fn timeout(settings: &GlobalSettings) -> Duration {
        Duration::from_secs(u64::from(settings.timings.seat_start_timeout_s.clamp(30, 120)))
    }
}

impl SessionBackend for FastUserSwitchBackend {
    fn kind(&self) -> SessionBackendKind {
        SessionBackendKind::FastUserSwitch
    }

    fn caps(&self) -> SessionCaps {
        SessionCaps {
            concurrent: false,
            max_seats: seat_core::MAX_SEATS,
            per_user: true,
            needs_seat_host: false,
            remote_display: false,
            remote_input: false,
            remote_audio: false,
            // The credential provider does the auto-logon (spec 35 §5.4).
            auto_logon: crate::credprov::active(self.d.registry.as_ref()),
            stop_one: true,
        }
    }

    fn probe(&self) -> BackendResult<HostProbe> {
        let e = self.d.edition.edition()?;
        // Spec 35 §5.1 / §5.3 / §6.2: say what U1 really is. Seats off the console are frozen
        // (no display, input or audio; GPU apps may lose their device), and each seat's user
        // picks the default audio device once in their own session.
        let notes = vec!["one_seat_at_a_time".to_string(), "fus_inactive_frozen".to_string(), "fus_audio_per_user".to_string()];
        // Spec 35 §6.2: hidden / turned-off fast user switching is a refusal, not a note.
        let disabled = fast_user_switching_disabled(self.d.registry.as_ref()).unwrap_or(false);
        let availability = if disabled {
            Availability::Unavailable { reason: reason::FSU_DISABLED.into() }
        } else if self.d.wts.console_session().is_some() {
            Availability::Available
        } else {
            Availability::Unavailable { reason: reason::CONSOLE_NOT_READY.into() }
        };
        Ok(HostProbe {
            kind: SessionBackendKind::FastUserSwitch,
            availability,
            edition: e.edition_id,
            build: e.build,
            arch: arch_name(self.d.edition.native_machine()).into(),
            notes,
        })
    }

    fn prepare_host(&self, _settings: &GlobalSettings) -> BackendResult<Vec<String>> {
        Ok(Vec::new())
    }

    fn restore_host(&self) -> BackendResult<()> {
        Ok(())
    }

    fn start(&self, req: &SeatStartRequest<'_>) -> BackendResult<StartStep> {
        let console = self.d.wts.console_session().ok_or_else(|| err(ErrorKind::NotReady, reason::CONSOLE_NOT_READY, "no console session"))?;
        if req.seat.is_console() {
            // Seat 1 went off the console for another seat earlier: bring its own session
            // back, with the same lock / password rules as every other seat.
            if let Some(id) = self.seat1_away(console)? {
                let all = self.d.wts.sessions()?;
                self.console_switchable(console, &all)?;
                let pw = req.credentials.as_ref().and_then(|c| c.password.as_ref());
                let target_locked = all.iter().find(|s| s.id == id).and_then(|s| s.locked) == Some(true);
                if pw.is_none() && !target_locked {
                    return Err(err(
                        ErrorKind::Policy,
                        reason::SESSION_NOT_LOCKED,
                        format!("seat 1's session {id} is not locked; it is not switched onto the console"),
                    ));
                }
                self.bring_to_console(id, console, pw, &all)?;
                log::info!("seat 1: switched its session {id} back onto the console");
                self.seats().insert(req.seat, (id, None));
                let info = self.info_of(id)?.unwrap_or(SessionInfo { session_id: id, state: SessionState::Active, user_sid: None, user_name: None, client_name: None });
                return Ok(StartStep::Started(info));
            }
            // The console belongs to another seat right now: seat 1 is not "whatever holds
            // the console" then (that would record seat N's session as seat 1's).
            if self.seats().iter().any(|(s, (id, _))| !s.is_console() && *id == console) {
                return Err(err(
                    ErrorKind::Policy,
                    reason::CONSOLE_IN_USE,
                    format!("console session {console} is another seat's and seat 1 has no session to return to"),
                ));
            }
            let info = self.d.console_info().ok_or_else(|| err(ErrorKind::NotReady, reason::CONSOLE_NOT_READY, "no console session"))?;
            *self.home() = Some((info.session_id, self.d.wts.user_sid(info.session_id)));
            self.seats().insert(req.seat, (info.session_id, None));
            return Ok(StartStep::Started(info));
        }
        if fast_user_switching_disabled(self.d.registry.as_ref()).unwrap_or(false) {
            return Err(err(ErrorKind::Policy, reason::FSU_DISABLED, "fast user switching is hidden or turned off by policy"));
        }
        let account = req.config.account.domain_user().map(|(a, b)| (a.to_owned(), b.to_owned()));
        // One distinct account per seat: sessions are matched by account, so two seats with
        // one account would map to one session (spec 35 §6.2).
        if let Some((dom, user)) = &account {
            let same = self.seats().iter().any(|(s, (_, a))| {
                *s != req.seat
                    && a.as_ref().is_some_and(|(d2, u2)| u2.eq_ignore_ascii_case(user) && d2.eq_ignore_ascii_case(dom))
            });
            if same {
                return Err(err(
                    ErrorKind::Policy,
                    reason::SAME_ACCOUNT_DENIED,
                    format!("{dom}\\{user} is already another seat's account; fast user switching needs one account per seat"),
                ));
            }
        }
        let all = self.d.wts.sessions()?;

        let existing = match &account {
            Some(a) => self.existing_session(a, &all)?,
            None => None,
        };
        if existing != Some(console) {
            self.console_switchable(console, &all)?;
            // The console is about to go away for seat N: remember seat 1's session (the
            // signed-in console session that is no other seat's) so it can come back.
            self.remember_home(console, &all);
        }
        let id = match existing {
            Some(id) if id == console => id,
            Some(id) => {
                let pw = req.credentials.as_ref().and_then(|c| c.password.as_ref());
                let target_locked = all.iter().find(|s| s.id == id).and_then(|s| s.locked) == Some(true);
                if pw.is_none() && !target_locked {
                    return Err(err(
                        ErrorKind::Policy,
                        reason::SESSION_NOT_LOCKED,
                        format!("session {id} is not locked and no password is stored; it is not switched onto the console"),
                    ));
                }
                self.bring_to_console(id, console, pw, &all)?;
                log::info!("seat {}: switched session {id} onto the console", req.seat);
                id
            }
            None => {
                self.d.wts.disconnect(console)?;
                log::info!("seat {}: console session {console} disconnected; Windows shows a new logon screen", req.seat);
                self.d
                    .wait_for(Self::timeout(req.settings), || self.d.wts.console_session().filter(|c| *c != console))
                    .ok_or_else(|| err(ErrorKind::Timeout, reason::SESSION_TIMEOUT, "no new console session"))?
            }
        };
        self.seats().insert(req.seat, (id, account));
        let info = self.info_of(id)?.unwrap_or(SessionInfo { session_id: id, state: SessionState::AtLogon, user_sid: None, user_name: None, client_name: None });
        Ok(StartStep::Started(info))
    }

    fn wait_session(&self, seat: SeatNumber, _client_name: &str, timeout: Duration) -> BackendResult<SessionInfo> {
        let rec = self.seats().get(&seat).cloned();
        let found = self.d.wait_for(timeout, || {
            let all = self.d.wts.sessions().ok()?;
            match &rec {
                Some((id, _)) => all.iter().find(|s| s.id == *id).cloned(),
                None => None,
            }
        });
        found
            .map(|s| s.info(self.d.wts.user_sid(s.id)))
            .ok_or_else(|| err(ErrorKind::Timeout, reason::SESSION_TIMEOUT, format!("seat {seat}: no session")))
    }

    fn query(&self, _seat: SeatNumber, session_id: u32) -> BackendResult<SessionInfo> {
        let mut info = self
            .info_of(session_id)?
            .unwrap_or(SessionInfo { session_id, state: SessionState::Gone, user_sid: None, user_name: None, client_name: None });
        // Off the console = waiting for its turn.
        if info.state == SessionState::Active && self.d.wts.console_session() != Some(session_id) {
            info.state = SessionState::Disconnected;
        }
        Ok(info)
    }

    fn log_off(&self, seat: SeatNumber, session_id: u32) -> BackendResult<()> {
        if seat.is_console() {
            return Err(err(ErrorKind::Policy, reason::CONSOLE_SEAT, "seat 1 is never logged off by us"));
        }
        let rec = self.seats().get(&seat).cloned();
        let all = self.d.wts.sessions()?;
        let Some(s) = all.iter().find(|s| s.id == session_id) else {
            self.seats().remove(&seat);
            return Ok(());
        };
        let ours = match rec {
            Some((id, Some((dom, user)))) => id == session_id && (s.user.is_empty() || s.user_matches(&dom, &user)),
            Some((id, None)) => id == session_id,
            None => false,
        };
        if !ours || !s.is_interactive() {
            return Err(err(ErrorKind::Policy, reason::NOT_OUR_SESSION, format!("session {session_id} is not seat {seat}'s")));
        }
        self.d.wts.logoff(session_id, false)?;
        self.seats().remove(&seat);
        Ok(())
    }

    fn adopt(&self, known: &[KnownSession]) -> BackendResult<Vec<(SeatNumber, SessionInfo)>> {
        let all = self.d.wts.sessions()?;
        let mut out = Vec::new();
        for k in known {
            let Some(s) = all.iter().find(|s| s.id == k.session_id && s.is_interactive()) else { continue };
            let sid = self.d.wts.user_sid(s.id);
            if k.user_sid.is_some() && !sid.as_deref().zip(k.user_sid.as_deref()).is_some_and(|(a, b)| a.eq_ignore_ascii_case(b)) {
                continue;
            }
            self.seats().insert(k.seat, (s.id, None));
            if k.seat.is_console() {
                // Seat 1's session from the journal: it can be connected back later.
                *self.home() = Some((s.id, sid.clone().or_else(|| k.user_sid.clone())));
            }
            out.push((k.seat, s.info(sid)));
        }
        Ok(out)
    }

    fn enumerate(&self) -> BackendResult<Vec<SessionInfo>> {
        Ok(self.d.wts.sessions()?.iter().filter(|s| s.is_interactive()).map(|s| s.info(self.d.wts.user_sid(s.id))).collect())
    }

    fn console_seat_away(&self) -> Option<u32> {
        let console = self.d.wts.console_session()?;
        self.seat1_away(console).ok().flatten()
    }
}

/// Boxed constructor.
pub fn fast_user_switch(d: SessionDeps) -> Arc<dyn SessionBackend> {
    Arc::new(FastUserSwitchBackend::new(d))
}
