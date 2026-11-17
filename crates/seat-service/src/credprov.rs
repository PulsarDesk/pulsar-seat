// SPDX-License-Identifier: GPL-3.0-only
//! Service side of the Pulsar credential provider (`credprov/`, spec 35 §5.4 / §5.5) for
//! `fast_user_switch` seats.
//!
//! - **Registration** ([`Service::sync_credprov`]): the provider + filter are registered
//!   only while the product is enabled and the active session backend is
//!   `fast_user_switch`; otherwise (and on uninstall) they are removed. Recovery value
//!   `CredProvDisabled=1` keeps them off (`seat_session::credprov`).
//! - **Caller check** (`hello` with `credential_provider`): SYSTEM token, process image
//!   `%SystemRoot%\System32\LogonUI.exe`, the process's session = the token's session. The
//!   provider can then act on that one session only.
//! - **One-time grant**: when a `fast_user_switch` start hands the console to a fresh logon
//!   session of seat N (no user yet) and seat N has a stored, non-failing password, the
//!   service arms a grant for exactly that session id, valid [`GRANT_TTL_MS`]. The provider
//!   in that session takes the credential once (`credprov_take_credentials`); a second take,
//!   another session, an expired grant or an unknown caller are refused. The grant goes
//!   away on take + report, logon, logoff, seat stop and expiry.
//! - **Hand-over** (`pending`): the backend disconnects the console and LogonUI of the
//!   fresh session enumerates its providers at once, before the start is through and the
//!   grant is armed. From just before the backend call until the grant is armed (or
//!   [`HANDOVER_MS`]) the query answers `pending` to the LogonUI of the new console
//!   session; the provider polls (bounded) and refreshes its tiles when the grant appears.
//! - **Auto-submit needs the account**: no `auto_submit` / `hide_others` without an
//!   account (the provider shows the seat tile only with one; the filter would otherwise
//!   hide every tile).
//! - **User SID**: the seat account's SID (`LookupAccountNameW`, outside the state lock,
//!   positive lookups cached) for the V2 tile; the provider uses it only when LogonUI's
//!   user array lists it.
//! - **Report**: a failed logon (`credprov_report` with a non-zero NTSTATUS) marks the stored
//!   password failing, like a failed RDP logon (no retry loop: lockout risk).
//! - **Switch link** (`credprov_switch`): the "switch seat" link on the logon / lock screen
//!   of a seat session asks for another seat (rate-limited). Nobody authenticated there,
//!   so a switch never uses the stored password: a start from the link arms no grant (the
//!   seat user types the password on the seat tile) and a reconnect passes no password
//!   (the target must be locked, Windows asks). Seat 1 is a target while its own session
//!   runs off the console (the backend connects it back).
//!
//! **Untested on hardware** (no LogonUI run): the whole flow; it is unit-tested with mocks.

use crate::auth::Caller;
use crate::log;
use crate::service::{Service, State};
use seat_core::backend::SessionBackendKind;
use seat_core::backend::session::SeatStartRequest;
use seat_core::state::SeatState;
use seat_core::SeatNumber;
use seat_proto::{CredprovScenario, CredprovSession, Credentials, ErrorCode, ProtoError, ResponseBody};

/// How long an armed grant stays valid after the fresh logon session appeared.
pub const GRANT_TTL_MS: u64 = 120_000;
/// At most one switch request per this many ms (per service; the lock screen is shared).
pub const SWITCH_MIN_INTERVAL_MS: u64 = 3_000;

/// How long a hand-over keeps the provider asking (`CredprovSession::pending`).
pub const HANDOVER_MS: u64 = 60_000;

/// A `fast_user_switch` start that is handing the console to a fresh logon session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handover {
    pub seat: SeatNumber,
    /// Console session before the switch (`None` = none at that moment).
    pub from_session: Option<u32>,
    pub since_ms: u64,
}

/// One armed credential for one logon session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    pub seat: SeatNumber,
    pub armed_ms: u64,
    /// Handed out (waits for the report).
    pub taken: bool,
}

/// Credential-provider state in [`State`].
#[derive(Debug, Default)]
pub struct CredprovState {
    /// session id -> grant.
    pub grants: std::collections::BTreeMap<u32, Grant>,
    /// Registration is in place (last sync).
    pub registered: bool,
    /// Time of the last accepted switch request.
    pub last_switch_ms: Option<u64>,
    /// Hand-over in progress (cleared when the grant is armed or not, on stop, by time).
    pub handover: Option<Handover>,
    /// `(domain, user)` -> account SID (successful lookups only).
    pub sids: std::collections::BTreeMap<(String, String), String>,
    /// seat -> start generation of a start that came from a switch link (nobody
    /// authenticated): no stored password, no grant for it.
    pub unattended: std::collections::BTreeMap<SeatNumber, u64>,
    /// A password-less switch handed the console to a logon screen for this seat: the
    /// provider there preselects the seat's tile (`CredprovSession::preselect`) until
    /// [`HANDOVER_MS`] or another switch. Added 2026-11-15.
    pub switch_to: Option<Handover>,
}

fn err(code: ErrorCode, msg: impl Into<String>) -> ProtoError {
    ProtoError::new(code, msg)
}

impl Service {
    /// Bring the registration in line with "enabled + `fast_user_switch` active".
    pub(crate) fn sync_credprov(&self) {
        let (enabled, safe) = {
            let st = self.lock();
            (st.config.settings.enabled, st.safe_reason)
        };
        let fus = self.backends().session.kind() == SessionBackendKind::FastUserSwitch;
        // Windows Safe Mode: leave the registry alone (the provider is inert without the
        // running service anyway).
        if safe == Some(seat_core::state::SafeModeReason::WindowsSafeMode) {
            return;
        }
        let want = enabled && fus;
        let registered = match self.platform.sync_credential_provider(want) {
            Ok(r) => r,
            Err(e) => {
                log::warn(&format!("credential provider registration (want {want}): {e}"));
                self.notice("credprov_failed", None, e);
                false
            }
        };
        let mut st = self.lock();
        if st.credprov.registered != registered {
            log::info(&format!("credential provider {}", if registered { "registered" } else { "not registered" }));
        }
        st.credprov.registered = registered;
        if !registered {
            st.credprov.grants.clear();
            st.credprov.handover = None;
        }
    }

    /// After a start: arm a grant when the seat's fresh session sits at the logon screen.
    pub(crate) fn maybe_arm_credprov(&self, seat: SeatNumber, session_id: u32, at_logon: bool) {
        // The hand-over of this seat is over either way (the provider's polling ends with
        // the next answer: a grant, or nothing pending).
        {
            let mut st = self.lock();
            if st.credprov.handover.as_ref().is_some_and(|h| h.seat == seat) {
                st.credprov.handover = None;
            }
        }
        if seat.is_console() || !at_logon || self.backends().session.kind() != SessionBackendKind::FastUserSwitch {
            return;
        }
        let now = self.now();
        let mut st = self.lock();
        if !st.credprov.registered || st.run.failing_passwords.contains(&seat) {
            return;
        }
        // A switch-link start authenticated nobody: the seat user types the password.
        if Self::unattended_start(&st, seat) {
            st.credprov.unattended.remove(&seat);
            log::info(&format!("seat {seat}: started from a switch link; no one-time logon armed"));
            return;
        }
        if st.config.seat(seat).and_then(|c| c.account.domain_user()).is_none() {
            return;
        }
        drop(st);
        // Never while holding the state lock (secret store).
        if !self.password_is_set(seat) {
            return;
        }
        st = self.lock();
        st.credprov.grants.retain(|_, g| g.seat != seat && now.saturating_sub(g.armed_ms) < GRANT_TTL_MS);
        st.credprov.grants.insert(session_id, Grant { seat, armed_ms: now, taken: false });
        log::info(&format!("seat {seat}: one-time logon armed for session {session_id}"));
    }

    /// Drop the grant of a session (logon, logoff, end).
    pub(crate) fn drop_credprov_grant(&self, session_id: u32) {
        self.lock().credprov.grants.remove(&session_id);
    }

    /// Drop every grant of a seat (stop, re-logon).
    pub(crate) fn drop_credprov_seat(&self, st: &mut State, seat: SeatNumber) {
        st.credprov.grants.retain(|_, g| g.seat != seat);
        if st.credprov.handover.as_ref().is_some_and(|h| h.seat == seat) {
            st.credprov.handover = None;
        }
    }

    /// `hello` of a `credential_provider` client: SYSTEM + LogonUI image + same session.
    pub(crate) fn verify_credprov(&self, caller: &Caller) -> bool {
        if !caller.is_system {
            return false;
        }
        let Some(image) = self.platform.process_image(caller.pid) else { return false };
        seat_session::credprov::is_logonui_image(&image, &self.platform.system_root())
            && self.platform.process_session(caller.pid) == Some(caller.session_id)
    }

    fn live_grant(&self, st: &State, session_id: u32) -> Option<Grant> {
        let now = self.now();
        st.credprov
            .grants
            .get(&session_id)
            .filter(|g| st.credprov.registered && now.saturating_sub(g.armed_ms) < GRANT_TTL_MS)
            .cloned()
    }

    fn credprov_session_of(caller: &Caller) -> Result<u32, ProtoError> {
        caller.credprov_session.ok_or_else(|| err(ErrorCode::AccessDenied, "not the verified credential provider"))
    }

    pub(crate) fn credprov_query(&self, caller: &Caller, scenario: CredprovScenario) -> Result<ResponseBody, ProtoError> {
        let session = Self::credprov_session_of(caller)?;
        let b = self.backends();
        let fus = b.session.kind() == SessionBackendKind::FastUserSwitch;
        let seat1_away = if fus { b.session.console_seat_away() } else { None };
        let st = self.lock();
        if !fus || !st.config.settings.enabled || !st.credprov.registered {
            return Ok(ResponseBody::CredprovSession(CredprovSession::default()));
        }
        let own = self.seat_of_session(&st, session);
        // Logon screen a password-less switch opened for another seat: show that seat's
        // tile, selected, without auto-logon (the user types the password).
        let switch_to = if own.is_none() && scenario == CredprovScenario::Logon { self.switch_to(&st, session) } else { None };
        let preselect = switch_to.is_some();
        let seat = own.or(switch_to);
        let grant = self.live_grant(&st, session).filter(|g| !g.taken && Some(g.seat) == own);
        let configured = seat
            .and_then(|s| st.config.seat(s))
            .and_then(|c| c.account.domain_user())
            .map(|(d, u)| (d.to_owned(), u.to_owned()));
        // A seat without an account (seat 1 at the logon screen setting) gets its session
        // user's tile: Windows 11 lists only tiles of a user with a password field on a
        // seat's lock / logon screen, so a user-less switch tile was never shown there
        // (VM test 2026-11-15, credprov.log). The target of a switch: that seat's session.
        let user_session = match (configured.is_some(), switch_to) {
            (true, _) => None,
            (false, Some(t)) if t.is_console() => seat1_away,
            (false, Some(t)) => st.seats.get(&t).and_then(|s| s.rt.session_id),
            (false, None) => seat.map(|_| session),
        };
        // Auto-submit (and with it the filter) only when the provider will really show the
        // seat tile: that needs the account. Without it the filter would hide every tile.
        let auto = scenario == CredprovScenario::Logon && grant.is_some() && configured.is_some();
        // A fast_user_switch hand-over to this console session is under way and no grant is
        // armed yet: the provider asks again shortly (LogonUI enumerates before we arm).
        let pending = scenario == CredprovScenario::Logon && grant.is_none() && self.handover_pending(&st, session, seat);
        // Switch links on every seat session's logon / lock screen (spec 35 §5.5), see
        // [`Self::switch_target`].
        let switch_targets: Vec<SeatNumber> =
            st.config.seats.iter().map(|c| c.number).filter(|n| Self::switch_target(&st, own, *n, seat1_away)).collect();
        let cached_sid = configured.as_ref().and_then(|k| st.credprov.sids.get(k).cloned());
        drop(st);
        // Session user outside the state lock.
        let session_user = user_session.and_then(|id| self.platform.session_user(id));
        let domain_user = configured.clone().or_else(|| session_user.as_ref().map(|u| (u.domain.clone(), u.name.clone())));
        let account = domain_user
            .as_ref()
            .map(|(d, u)| if d.is_empty() || d == "." { format!(r".\{u}") } else { format!(r"{d}\{u}") });
        // SID of the seat account for the V2 tile. LookupAccountNameW never under the state
        // lock (it can wait for a domain controller); successful lookups are cached. The
        // provider uses it only when LogonUI's user array lists that SID.
        let user_sid = match (cached_sid, &configured) {
            (Some(s), _) => Some(s),
            (None, None) => session_user.map(|u| u.sid),
            (None, Some((d, u))) => {
                let sid = self.platform.account_sid(d, u);
                if let Some(s) = &sid {
                    self.lock().credprov.sids.insert((d.clone(), u.clone()), s.clone());
                }
                sid
            }
        };
        Ok(ResponseBody::CredprovSession(CredprovSession {
            seat,
            auto_submit: auto,
            hide_others: auto,
            account: if seat.is_some() { account } else { None },
            user_sid: if seat.is_some() { user_sid } else { None },
            switch_targets,
            pending,
            preselect,
        }))
    }

    /// Seat whose password-less switch opened the logon screen of `session` (the console,
    /// not the session the switch came from, within [`HANDOVER_MS`]).
    fn switch_to(&self, st: &State, session: u32) -> Option<SeatNumber> {
        let h = st.credprov.switch_to.as_ref()?;
        (self.now().saturating_sub(h.since_ms) < HANDOVER_MS
            && h.from_session != Some(session)
            && self.platform.console_session() == Some(session))
        .then_some(h.seat)
    }

    /// `switch_seat` (admin, protocol 3): the console to a running seat (or seat 1's own
    /// session while it waits off the console); an off seat is started. Same backend rules
    /// as the switch links: no stored password, so the target's user signs in on a logon
    /// screen with the seat's tile preselected.
    pub(crate) fn switch_seat(&self, seat: SeatNumber) -> Result<ResponseBody, ProtoError> {
        if self.backends().session.kind() != SessionBackendKind::FastUserSwitch {
            return Err(err(ErrorCode::Unsupported, "switching the console needs fast user switching"));
        }
        let seat1_away = if seat.is_console() { self.backends().session.console_seat_away() } else { None };
        let active = {
            let st = self.lock();
            self.may_start(&st, true)?;
            if st.config.seat(seat).is_none() {
                return Err(err(ErrorCode::NotFound, "no such seat"));
            }
            if seat.is_console() && seat1_away.is_none() {
                return Err(err(ErrorCode::InvalidValue, "seat 1 already holds the console").with_reason("on_console"));
            }
            st.seats.get(&seat).filter(|s| s.is_active() && !matches!(s.rt.state, SeatState::Stopping)).map(|s| s.rt.session_id)
        };
        log::info(&format!("switch to seat {seat} requested over the pipe"));
        match active {
            _ if seat.is_console() => self.spawn(move |s| s.reconnect_seat(seat)),
            Some(Some(_)) => self.spawn(move |s| s.reconnect_seat(seat)),
            Some(None) => {}
            None => self.start_sequence(vec![seat]),
        }
        Ok(ResponseBody::Ack)
    }

    /// Start of a `fast_user_switch` hand-over of the console to `seat` (before the backend
    /// disconnects the console): remember it, so the provider in the fresh session keeps
    /// asking until the grant is armed. Only when a grant can follow.
    pub(crate) fn note_credprov_handover(&self, seat: SeatNumber) {
        if seat.is_console() || self.backends().session.kind() != SessionBackendKind::FastUserSwitch {
            return;
        }
        // A new hand-over of the console supersedes an earlier password-less switch: its
        // record would still name the old target for HANDOVER_MS and the fresh logon screen
        // would preselect that seat instead of waiting for this one (VM 2026-11-16:
        // switch_seat 1, then switch_seat 3 -> seat 3 never signed in).
        self.lock().credprov.switch_to = None;
        let console = self.platform.console_session();
        {
            let st = self.lock();
            if !st.credprov.registered
                || Self::unattended_start(&st, seat)
                || st.run.failing_passwords.contains(&seat)
                || st.config.seat(seat).and_then(|c| c.account.domain_user()).is_none()
            {
                return;
            }
        }
        if !self.password_is_set(seat) {
            return;
        }
        let now = self.now();
        self.lock().credprov.handover = Some(Handover { seat, from_session: console, since_ms: now });
    }

    /// A hand-over to `seat` is under way and `session` is its fresh console session.
    fn handover_pending(&self, st: &State, session: u32, seat: Option<SeatNumber>) -> bool {
        let Some(h) = st.credprov.handover.as_ref() else { return false };
        // Seat 1 with nobody signed in follows the console session, so LogonUI of the fresh
        // hand-over session can be answered for seat 1 before the seat's session id is
        // recorded (VM 2026-11-17: console at a logon screen after a service restart, the
        // provider asked 26 ms before the grant was armed, saw "seat 1, not pending" and
        // never asked again).
        let console_unclaimed = |s: SeatNumber| s.is_console() && st.seats.get(&s).is_none_or(|c| c.logon.is_none());
        self.now().saturating_sub(h.since_ms) < HANDOVER_MS
            && h.from_session != Some(session)
            && seat.is_none_or(|s| s == h.seat || console_unclaimed(s))
            && self.platform.console_session() == Some(session)
    }

    pub(crate) fn credprov_take(&self, caller: &Caller) -> Result<ResponseBody, ProtoError> {
        let session = Self::credprov_session_of(caller)?;
        // TTL, seat match and `taken` in one critical section (a concurrent second take is
        // refused). The secret store is read after the lock is released (a slow LSA call
        // must not block every pipe request); then the grant is checked again: a seat
        // stop, re-arm or expiry in between drops the hand-out.
        let grant = {
            let mut st = self.lock();
            let g = self.live_grant(&st, session).ok_or_else(|| {
                err(ErrorCode::AccessDenied, format!("no armed logon for session {session}")).with_reason("no_grant")
            })?;
            if g.taken {
                return Err(err(ErrorCode::AccessDenied, "credentials were already handed out").with_reason("credentials_taken"));
            }
            if self.seat_of_session(&st, session) != Some(g.seat) {
                return Err(err(ErrorCode::AccessDenied, "session is not the seat's").with_reason("not_our_session"));
            }
            match st.credprov.grants.get_mut(&session) {
                Some(live) if !live.taken && live.seat == g.seat => live.taken = true,
                _ => return Err(err(ErrorCode::AccessDenied, "credentials were already handed out").with_reason("credentials_taken")),
            }
            g
        };
        let creds = self.credentials_unlocked(grant.seat);
        let still_live = {
            let st = self.lock();
            self.live_grant(&st, session).is_some_and(|g| g.taken && g.seat == grant.seat && g.armed_ms == grant.armed_ms)
                && self.seat_of_session(&st, session) == Some(grant.seat)
        };
        if !still_live {
            // Drop only our grant (a re-armed one for the same session stays).
            self.lock().credprov.grants.retain(|k, g| !(*k == session && g.armed_ms == grant.armed_ms && g.seat == grant.seat));
            return Err(err(ErrorCode::AccessDenied, format!("the armed logon for session {session} ended")).with_reason("no_grant"));
        }
        match creds {
            Some(c) if c.password.is_some() => {
                log::info(&format!("seat {}: one-time logon handed to LogonUI in session {session}", grant.seat));
                Ok(ResponseBody::Credentials(Credentials { domain: c.domain, user: c.user, password: c.password }))
            }
            _ => {
                self.lock().credprov.grants.remove(&session);
                Err(err(ErrorCode::NotFound, "no stored password").with_reason("no_password"))
            }
        }
    }

    pub(crate) fn credprov_report(&self, caller: &Caller, status: i32, sub_status: i32) -> Result<ResponseBody, ProtoError> {
        let session = Self::credprov_session_of(caller)?;
        let mut st = self.lock();
        let Some(g) = st.credprov.grants.get(&session).filter(|g| g.taken).cloned() else {
            return Ok(ResponseBody::Ack);
        };
        st.credprov.grants.remove(&session);
        if status == 0 {
            log::info(&format!("seat {}: LogonUI reported the auto-logon as accepted", g.seat));
            return Ok(ResponseBody::Ack);
        }
        if !st.run.failing_passwords.contains(&g.seat) {
            st.run.failing_passwords.push(g.seat);
            self.save_run(&st);
        }
        drop(st);
        log::error_id(
            crate::eventid::SEAT_LOGON_FAILED,
            &format!("seat {}: auto-logon refused (status {status:#010x}, sub {sub_status:#010x}); stored password marked failing", g.seat),
        );
        self.notice("credprov_logon_failed", Some(g.seat), format!("{status:#010x}"));
        Ok(ResponseBody::Ack)
    }

    pub(crate) fn credprov_switch(&self, caller: &Caller, seat: SeatNumber) -> Result<ResponseBody, ProtoError> {
        let session = Self::credprov_session_of(caller)?;
        self.switch_from_session(session, seat, "the logon / lock screen")
    }

    /// `agent_switch_targets`: what the agent's tray menu offers (spec 35 §5.5(a)).
    pub(crate) fn agent_switch_targets(&self, caller: &Caller) -> Result<ResponseBody, ProtoError> {
        let session = caller.agent_session.ok_or_else(|| err(ErrorCode::AccessDenied, "not the verified agent"))?;
        if self.backends().session.kind() != SessionBackendKind::FastUserSwitch {
            return Ok(ResponseBody::SwitchTargets(Vec::new()));
        }
        let seat1_away = self.backends().session.console_seat_away();
        let st = self.lock();
        if !st.config.settings.enabled {
            return Ok(ResponseBody::SwitchTargets(Vec::new()));
        }
        let own = self.seat_of_session(&st, session);
        Ok(ResponseBody::SwitchTargets(
            st.config.seats.iter().map(|c| c.number).filter(|n| Self::switch_target(&st, own, *n, seat1_away)).collect(),
        ))
    }

    /// `agent_switch`: the seat user's own desktop asks for another seat (tray item /
    /// hotkey of `pulsar-seat-agent`, spec 35 §5.5(a)). Same rules as the switch link: own
    /// seat session only, managed target, `fast_user_switch`, rate-limited, no stored
    /// password (the agent's user did not authenticate for the target seat). The agent locks
    /// its session first; an unlocked one is refused by the backend (`console_in_use`).
    pub(crate) fn agent_switch(&self, caller: &Caller, seat: SeatNumber) -> Result<ResponseBody, ProtoError> {
        let session = caller.agent_session.ok_or_else(|| err(ErrorCode::AccessDenied, "not the verified agent"))?;
        self.switch_from_session(session, seat, "the seat's desktop (agent)")
    }

    fn switch_from_session(&self, session: u32, seat: SeatNumber, origin: &str) -> Result<ResponseBody, ProtoError> {
        if self.backends().session.kind() != SessionBackendKind::FastUserSwitch {
            return Err(err(ErrorCode::Unsupported, "switch links only work with fast user switching"));
        }
        let now = self.now();
        // Backend query outside the state lock.
        let seat1_away = if seat.is_console() { self.backends().session.console_seat_away() } else { None };
        let active = {
            let mut st = self.lock();
            self.may_start(&st, true)?;
            let own = self.seat_of_session(&st, session);
            if own.is_none() {
                return Err(err(ErrorCode::AccessDenied, "not a seat session").with_reason("not_our_session"));
            }
            if !Self::switch_target(&st, own, seat, seat1_away) {
                return Err(err(ErrorCode::InvalidValue, format!("seat {seat} is not a switch target")));
            }
            if st.credprov.last_switch_ms.is_some_and(|t| now.saturating_sub(t) < SWITCH_MIN_INTERVAL_MS) {
                return Err(err(ErrorCode::Timeout, "switch requested too often").with_reason("busy"));
            }
            st.credprov.last_switch_ms = Some(now);
            st.seats.get(&seat).filter(|s| s.is_active() && !matches!(s.rt.state, SeatState::Stopping)).map(|s| s.rt.session_id)
        };
        log::info(&format!("switch to seat {seat} requested from {origin} of session {session}"));
        match active {
            // Seat 1 is never started by the service here: its remembered session comes back.
            _ if seat.is_console() => self.spawn(move |s| s.reconnect_seat(seat)),
            // Running seat whose session waits off the console: connect it again.
            Some(Some(_)) => self.spawn(move |s| s.reconnect_seat(seat)),
            // Starting: nothing to do.
            Some(None) => {}
            // Nobody authenticated at this lock / logon screen: the start uses no stored
            // password and arms no one-time logon; seat N's user signs in on the seat tile.
            None => self.start_sequence_with(vec![seat], true),
        }
        Ok(ResponseBody::Ack)
    }

    /// `target` may be offered / switched to from the lock or logon screen of `own`'s
    /// session: a managed, configured seat other than `own`. Seat 1 (never started by the
    /// service under `fast_user_switch`) only while the backend reports its own session
    /// waiting off the console (`seat1_away`, `SessionBackend::console_seat_away`); the
    /// backend then connects that session back (spec 35 §5.5).
    pub(crate) fn switch_target(st: &State, own: Option<SeatNumber>, target: SeatNumber, seat1_away: Option<u32>) -> bool {
        let Some(own) = own else { return false };
        if own == target || st.config.seat(target).is_none() {
            return false;
        }
        if target.is_console() {
            let own_session = st.seats.get(&own).and_then(|s| s.rt.session_id);
            return seat1_away.is_some_and(|id| Some(id) != own_session);
        }
        Self::managed(st, target)
    }

    /// Bring a running `fast_user_switch` seat's session back onto the console (the backend
    /// connects the existing session; same refusals as a start).
    ///
    /// The request comes from a lock / logon screen where nobody authenticated, so the
    /// stored password is **never** passed: the backend's `session_not_locked` refusal then
    /// always applies and Windows asks for the target session's password.
    pub(crate) fn reconnect_seat(&self, seat: SeatNumber) {
        let (cfg, settings) = {
            let st = self.lock();
            let Some(cfg) = st.config.seat(seat).cloned() else { return };
            (cfg, st.config.settings.clone())
        };
        let creds = self.credentials_unlocked(seat).map(|mut c| {
            c.password = None;
            c
        });
        let req = SeatStartRequest {
            seat,
            config: &cfg,
            settings: &settings,
            client_name: crate::seats::client_name(seat),
            credentials: creds,
            idd_outputs: None,
        };
        let before = self.platform.console_session();
        match self.backends().session.start(&req) {
            Err(e) => {
                log::warn(&format!("seat {seat}: switch refused: {e}"));
                self.notice("switch_refused", Some(seat), e.reason.clone());
            }
            // No password: the console went to a logon screen (the session itself is not on
            // the console); the provider there preselects this seat's tile.
            Ok(seat_core::backend::session::StartStep::Started(info)) if self.platform.console_session() != Some(info.session_id) => {
                let mut st = self.lock();
                st.credprov.switch_to = Some(Handover { seat, from_session: before, since_ms: self.now() });
                // This switch now owns the console's logon screen.
                st.credprov.handover = None;
            }
            Ok(_) => self.lock().credprov.switch_to = None,
        }
    }
}
