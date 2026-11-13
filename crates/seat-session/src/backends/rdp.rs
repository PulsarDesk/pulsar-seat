// SPDX-License-Identifier: GPL-3.0-only
//! Session bookkeeping shared by the two RDP loopback backends (`rds_loopback`,
//! `termsrv_wrapper`): start → `HostLaunch`, `wait_session` matching, ownership-checked
//! log-off, adoption after a service restart (spec 30 §6.2.12–6.2.14).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use seat_core::SeatNumber;
use seat_core::backend::session::{
    HostHint, HostLaunch, HostTopology, KnownSession, RdpConnection, SeatStartRequest, SessionInfo, SessionState, StartStep,
};
use seat_core::backend::{AudioBackendKind, BackendResult, ErrorKind};

use super::SessionDeps;
use crate::autologon::{rdp_domain, user_hint};
use crate::error::{err, reason};
use crate::wts::{SeatMatch, WtsSession, find_rdp_seat_session, session_ids};

#[derive(Debug, Clone)]
struct Pending {
    client_name: String,
    account: Option<(String, String)>,
    before: Vec<u32>,
}

#[derive(Debug, Clone)]
struct Bound {
    session_id: u32,
    client_name: String,
    account: Option<(String, String)>,
}

pub(crate) struct RdpCore {
    pending: Mutex<HashMap<SeatNumber, Pending>>,
    bound: Mutex<HashMap<SeatNumber, Bound>>,
    /// Latest `host_report` hint per seat (cleared at each start).
    hints: Mutex<HashMap<SeatNumber, HostHint>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl RdpCore {
    pub(crate) fn new() -> Self {
        Self { pending: Mutex::new(HashMap::new()), bound: Mutex::new(HashMap::new()), hints: Mutex::new(HashMap::new()) }
    }

    /// Merge a host hint: a reported session id is kept once known. A bound seat whose
    /// host reports a different session id is only logged (the binding stays; `query`
    /// keeps checking ownership).
    pub(crate) fn host_hint(&self, seat: SeatNumber, hint: HostHint) {
        let mut hints = lock(&self.hints);
        let h = hints.entry(seat).or_default();
        h.legacy_client |= hint.legacy_client;
        if hint.session_id.is_some() {
            h.session_id = hint.session_id;
        }
        if let (Some(id), Some(b)) = (hint.session_id, lock(&self.bound).get(&seat))
            && b.session_id != id
        {
            log::warn!("seat {seat}: host reports session {id}, but session {} is bound", b.session_id);
        }
    }

    fn claimed_except(&self, seat: SeatNumber) -> Vec<u32> {
        lock(&self.bound).iter().filter(|(s, _)| **s != seat).map(|(_, b)| b.session_id).collect()
    }

    pub(crate) fn bound_count(&self) -> usize {
        lock(&self.bound).len()
    }

    /// Seat 1 under `seat_one_console` is the physical console, not an RDP seat.
    pub(crate) fn is_console_seat(seat: SeatNumber, topology: HostTopology) -> bool {
        seat.is_console() && topology == HostTopology::SeatOneConsole
    }

    /// Begins a start: snapshots existing sessions and returns the host launch.
    /// `second`: the second, loopback-only NLA-off listener for passwordless seats
    /// (`rds_loopback`, architecture §4.1 design (a)): `None` = not offered by this
    /// backend / turned off; `Some(Ok(port))` = healthy; `Some(Err(state note))` = wanted
    /// but not ready.
    pub(crate) fn start(
        &self,
        d: &SessionDeps,
        req: &SeatStartRequest<'_>,
        max_rdp_seats: u8,
        second: Option<Result<u16, String>>,
    ) -> BackendResult<StartStep> {
        let topology = req.settings.backends.host_topology;
        if Self::is_console_seat(req.seat, topology) {
            let info = d.console_info().ok_or_else(|| err(ErrorKind::NotReady, reason::CONSOLE_NOT_READY, "no console session"))?;
            return Ok(StartStep::Started(info));
        }
        let already = lock(&self.bound).contains_key(&req.seat);
        if !already && self.bound_count() + 1 > usize::from(max_rdp_seats) {
            return Err(err(ErrorKind::Policy, reason::SEAT_LIMIT, format!("listener / licence allows {max_rdp_seats} seat sessions")));
        }
        let listener = d.rds.listener().map_err(|e| err(ErrorKind::NotReady, reason::RDS_NOT_READY, e.to_string()))?;
        // A stored password is only ever sent with NLA (CredSSP) on: without it the
        // password travels in the Client Info PDU to whatever answers on the port.
        if !listener.nla && req.credentials.as_ref().is_some_and(|c| c.password.is_some()) {
            return Err(err(ErrorKind::Policy, reason::NLA_REQUIRED, "the RDP listener does not require NLA; stored credentials are not sent"));
        }
        // The reverse: a logon-screen seat (no stored password) cannot pass NLA (CredSSP
        // needs the password before the session exists). Such a seat goes to the second,
        // loopback-only listener without NLA (architecture §4.1 design (a)); without one it
        // is refused up front instead of failing later in the host with `nla_required`.
        let mut port = listener.port;
        if listener.nla && !req.credentials.as_ref().is_some_and(|c| c.password.is_some()) {
            match second {
                Some(Ok(p)) => port = p,
                Some(Err(note)) => {
                    return Err(err(
                        ErrorKind::NotReady,
                        reason::LOOPBACK_LISTENER_UNAVAILABLE,
                        format!("the listener for seats without a stored password is not ready ({note})"),
                    ));
                }
                None => {
                    return Err(err(
                        ErrorKind::Policy,
                        reason::NLA_REQUIRED,
                        "the RDP listener requires NLA and the listener for seats without a stored password is off",
                    ));
                }
            }
        }

        let console = d.wts.console_session().ok_or_else(|| err(ErrorKind::NotReady, reason::CONSOLE_NOT_READY, "no console session"))?;
        let all = d.wts.sessions()?;
        let account = req
            .credentials
            .as_ref()
            .map(|c| (c.domain.clone(), c.user.clone()))
            .or_else(|| req.config.account.domain_user().map(|(a, b)| (a.to_owned(), b.to_owned())));
        let last = d.last_user.as_ref().and_then(|f| f(req.seat));
        let (domain_hint, user_hint) = match &req.credentials {
            Some(c) => (rdp_domain(&c.domain).to_owned(), c.user.clone()),
            None => user_hint(&req.config.account, last.as_ref()),
        };
        let monitors = d.geometry_of(req.seat);
        let audio = req.settings.backends.audio == AudioBackendKind::RdpBridge;
        let connection = RdpConnection {
            address: seat_core::consts::RDP_LOOPBACK_ADDRESS.into(),
            port,
            client_name: req.client_name.clone(),
            user_hint,
            domain_hint,
            monitors: if monitors.is_empty() { Vec::new() } else { monitors },
            audio_playback: audio,
            audio_capture: audio,
            // The seat session is on this PC: it already sees local drives; per-seat
            // drive visibility is the session_drive_letters gate's job (spec 70).
            redirect_drives: false,
            // Smart cards: the RDP control redirects every reader of the host session;
            // per-seat readers need the gate design of spec 70 (not done here).
            redirect_smartcards: false,
        };
        lock(&self.pending).insert(req.seat, Pending { client_name: req.client_name.clone(), account, before: session_ids(&all) });
        lock(&self.hints).remove(&req.seat);
        log::info!("seat {}: launch host in session {console} for {}:{}", req.seat, connection.address, connection.port);
        Ok(StartStep::LaunchHost(HostLaunch { seat: req.seat, target_session: console, connection }))
    }

    pub(crate) fn wait_session(&self, d: &SessionDeps, seat: SeatNumber, client_name: &str, timeout: Duration) -> BackendResult<SessionInfo> {
        let p = lock(&self.pending).get(&seat).cloned().unwrap_or(Pending {
            client_name: client_name.to_owned(),
            account: None,
            before: Vec::new(),
        });
        let claimed = self.claimed_except(seat);
        let mut last_err = None;
        let found = d.wait_for(timeout, || {
            let all = match d.wts.sessions() {
                Ok(a) => a,
                Err(e) => {
                    last_err = Some(e);
                    return None;
                }
            };
            let hint = lock(&self.hints).get(&seat).copied().unwrap_or_default();
            let m = SeatMatch {
                client_name: if client_name.is_empty() { &p.client_name } else { client_name },
                account: p.account.as_ref().map(|(a, b)| (a.as_str(), b.as_str())),
                before: &p.before,
                claimed: &claimed,
                reported: hint.session_id,
                newest_fallback: hint.legacy_client,
            };

            let id = find_rdp_seat_session(&all, &m)?;
            all.into_iter().find(|s| s.id == id)
        });
        let Some(s) = found else {
            let detail = last_err.map(|e| e.to_string()).unwrap_or_else(|| "no matching loopback session".into());
            return Err(err(ErrorKind::Timeout, reason::SESSION_TIMEOUT, format!("seat {seat}: {detail}")));
        };
        lock(&self.pending).remove(&seat);
        lock(&self.bound).insert(seat, Bound { session_id: s.id, client_name: p.client_name.clone(), account: p.account.clone() });
        Ok(s.info(d.wts.user_sid(s.id)))
    }

    fn is_ours(&self, seat: SeatNumber, s: &WtsSession) -> bool {
        if !s.is_loopback_remote() {
            return false;
        }
        let b = lock(&self.bound).get(&seat).cloned();
        match b {
            Some(b) if b.session_id == s.id => match &b.account {
                // Windows may blank the user of a session that is logging off: only a
                // different user is a mismatch.
                Some((dom, user)) => s.user.is_empty() || s.user_matches(dom, user),
                None => true,
            },
            Some(b) => s.client_name.eq_ignore_ascii_case(&b.client_name) && b.client_name.starts_with(seat_core::consts::RDP_CLIENT_NAME_PREFIX),
            None => crate::wts::seat_of_client_name(&s.client_name) == Some(seat),
        }
    }

    pub(crate) fn query(&self, d: &SessionDeps, seat: SeatNumber, session_id: u32) -> BackendResult<SessionInfo> {
        let all = d.wts.sessions()?;
        let Some(s) = all.iter().find(|s| s.id == session_id) else {
            lock(&self.bound).retain(|k, b| !(*k == seat && b.session_id == session_id));
            return Ok(SessionInfo { session_id, state: SessionState::Gone, user_sid: None, user_name: None, client_name: None });
        };
        // No "nothing bound yet" escape: without a binding only a `PSEAT-NN` client name of
        // this seat (or the console) qualifies (spec 30 §6.2.14, the service is the only
        // authority for seat -> session).
        if !s.is_console_winstation() && !self.is_ours(seat, s) {
            return Err(err(ErrorKind::Policy, reason::NOT_OUR_SESSION, format!("session {session_id} is not seat {seat}'s")));
        }
        Ok(s.info(d.wts.user_sid(session_id)))
    }

    pub(crate) fn log_off(&self, d: &SessionDeps, seat: SeatNumber, session_id: u32, topology: HostTopology) -> BackendResult<()> {
        if Self::is_console_seat(seat, topology) {
            return Err(err(ErrorKind::Policy, reason::CONSOLE_SEAT, "seat 1 is the console session; it is never logged off by us"));
        }
        let all = d.wts.sessions()?;
        let Some(s) = all.iter().find(|s| s.id == session_id) else {
            lock(&self.bound).remove(&seat);
            return Ok(());
        };
        if !self.is_ours(seat, s) {
            return Err(err(ErrorKind::Policy, reason::NOT_OUR_SESSION, format!("session {session_id} is not seat {seat}'s")));
        }
        d.wts.logoff(session_id, true)?;
        lock(&self.bound).remove(&seat);
        log::info!("seat {seat}: logged off session {session_id}");
        Ok(())
    }

    pub(crate) fn adopt(&self, d: &SessionDeps, known: &[KnownSession]) -> BackendResult<Vec<(SeatNumber, SessionInfo)>> {
        let all = d.wts.sessions()?;
        let mut out = Vec::new();
        for k in known {
            let Some(s) = all.iter().find(|s| s.id == k.session_id) else { continue };
            if !s.is_loopback_remote() || matches!(s.seat_state(), SessionState::Gone) {
                continue;
            }
            let sid = d.wts.user_sid(s.id);
            // The same user (by SID) must still own it; a re-used id with another user is not ours.
            if let (Some(want), Some(have)) = (&k.user_sid, &sid)
                && !want.eq_ignore_ascii_case(have)
            {
                continue;
            }
            if k.user_sid.is_some() && sid.is_none() && !s.user.is_empty() {
                continue;
            }
            lock(&self.bound).insert(k.seat, Bound { session_id: s.id, client_name: k.client_name.clone(), account: None });
            out.push((k.seat, s.info(sid)));
        }
        Ok(out)
    }

    pub(crate) fn enumerate(&self, d: &SessionDeps) -> BackendResult<Vec<SessionInfo>> {
        Ok(d.wts.sessions()?.iter().filter(|s| s.is_interactive()).map(|s| s.info(d.wts.user_sid(s.id))).collect())
    }
}
