// SPDX-License-Identifier: GPL-3.0-only
//! WTS session model, mapping to [`SessionInfo`] and the seat ↔ session matching rules
//! (spec 30 §6.2.14; adapted from pulsar-split `split-session::discover`).
//!
//! Matching a seat's RDP session:
//!
//! 1. never Services (id 0), listeners (id ≥ 65536 or state `Listen`) or the console
//!    winstation;
//! 2. remote session from a loopback client address;
//! 3. client name `PSEAT-NN` **or** — because the documented RDP ActiveX control sends the
//!    local computer name and has no client-name property — a session that did not exist
//!    when the seat start began (the service starts seats one at a time, never in
//!    parallel) and is not claimed by another seat;
//! 4. when the seat has an account: the session user matches it.

use std::net::IpAddr;

use seat_core::SeatNumber;
use seat_core::backend::session::{SessionInfo, SessionState};
use serde::{Deserialize, Serialize};

use crate::error::SysError;

/// Session ids at or above this are listeners, never user sessions.
pub const LISTENER_ID_MIN: u32 = 65_536;

/// `PSEAT-NN` (8 chars, below the 15-character RDP client-name limit).
pub fn client_name_for(seat: SeatNumber) -> String {
    format!("{}{}", seat_core::consts::RDP_CLIENT_NAME_PREFIX, seat.two_digits())
}

/// Seat number of a `PSEAT-NN` client name (case-insensitive).
pub fn seat_of_client_name(name: &str) -> Option<SeatNumber> {
    let p = seat_core::consts::RDP_CLIENT_NAME_PREFIX;
    if name.len() != p.len() + 2 || !name[..p.len()].eq_ignore_ascii_case(p) {
        return None;
    }
    let n: u8 = name[p.len()..].parse().ok()?;
    SeatNumber::new(n)
}

/// `WTS_CONNECTSTATE_CLASS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WtsState {
    Active,
    Connected,
    ConnectQuery,
    Shadow,
    Disconnected,
    Idle,
    Listen,
    Reset,
    Down,
    Init,
}

impl WtsState {
    pub fn from_raw(v: i32) -> Self {
        match v {
            0 => Self::Active,
            1 => Self::Connected,
            2 => Self::ConnectQuery,
            3 => Self::Shadow,
            4 => Self::Disconnected,
            5 => Self::Idle,
            6 => Self::Listen,
            7 => Self::Reset,
            8 => Self::Down,
            _ => Self::Init,
        }
    }
}

/// One row of `WTSEnumerateSessionsW` plus the per-session queries we need.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WtsSession {
    pub id: u32,
    pub winstation: String,
    pub state: WtsState,
    pub client_name: String,
    pub user: String,
    pub domain: String,
    pub client_address: Option<IpAddr>,
    pub is_remote: bool,
    /// `WTSINFOEX` session flags: `Some(true)` = locked, `None` = unknown.
    pub locked: Option<bool>,
}

impl WtsSession {
    /// Services, console and listener sessions are never RDP seat sessions.
    pub fn is_system_or_listener(&self) -> bool {
        self.id == 0 || self.id >= LISTENER_ID_MIN || self.state == WtsState::Listen || self.winstation.eq_ignore_ascii_case("Services")
    }

    pub fn is_console_winstation(&self) -> bool {
        self.winstation.eq_ignore_ascii_case("Console")
    }

    /// An interactive session (local or remote) that can belong to a seat.
    pub fn is_interactive(&self) -> bool {
        !self.is_system_or_listener() && !matches!(self.state, WtsState::Down | WtsState::Init | WtsState::Reset)
    }

    /// Remote session from a loopback client address (RDP loopback seat candidate).
    pub fn is_loopback_remote(&self) -> bool {
        !self.is_system_or_listener()
            && !self.is_console_winstation()
            && self.is_remote
            && self.client_address.is_some_and(|a| a.is_loopback())
    }

    /// User name matches `(domain, user)` of a seat account (`"."` = this PC).
    pub fn user_matches(&self, domain: &str, user: &str) -> bool {
        if user.is_empty() || !self.user.eq_ignore_ascii_case(user) {
            return false;
        }
        domain.is_empty() || domain == "." || self.domain.is_empty() || self.domain.eq_ignore_ascii_case(domain)
    }

    /// Seat-visible state.
    pub fn seat_state(&self) -> SessionState {
        match self.state {
            WtsState::Disconnected => SessionState::Disconnected,
            WtsState::Down | WtsState::Reset => SessionState::Gone,
            _ if self.user.is_empty() => SessionState::AtLogon,
            _ if self.locked == Some(true) => SessionState::Locked,
            WtsState::Active => SessionState::Active,
            _ => SessionState::AtLogon,
        }
    }

    pub fn info(&self, user_sid: Option<String>) -> SessionInfo {
        SessionInfo {
            session_id: self.id,
            state: self.seat_state(),
            user_sid,
            user_name: (!self.user.is_empty()).then(|| {
                if self.domain.is_empty() { self.user.clone() } else { format!(r"{}\{}", self.domain, self.user) }
            }),
            client_name: (!self.client_name.is_empty()).then(|| self.client_name.clone()),
        }
    }
}

/// WTS access (real impl: `crate::win::wts::WindowsWts`).
pub trait Wts: Send + Sync {
    fn sessions(&self) -> Result<Vec<WtsSession>, SysError>;
    /// `WTSLogoffSession`. With `require_remote` the real impl refuses a non-remote session.
    fn logoff(&self, id: u32, require_remote: bool) -> Result<(), SysError>;
    /// `WTSDisconnectSession`.
    fn disconnect(&self, id: u32) -> Result<(), SysError>;
    /// `WTSConnectSessionW(id, target, password)`: connect session `id` to `target`
    /// (fast user switching onto the console).
    fn connect(&self, id: u32, target: u32, password: Option<&seat_core::Secret>) -> Result<(), SysError>;
    /// `WTSGetActiveConsoleSessionId`, `None` while no session owns the console.
    fn console_session(&self) -> Option<u32>;
    /// `TokenUser` SID of the session's user (`WTSQueryUserToken`, SYSTEM only); `None`
    /// when nobody is signed in.
    fn user_sid(&self, id: u32) -> Option<String>;
    /// SID of a seat account (`LookupAccountNameW`; `"."` = this computer only, never a
    /// domain account of the same name). `None` if it does not resolve.
    fn account_sid(&self, _domain: &str, _user: &str) -> Option<String> {
        None
    }
}

/// Ids of every session present now (for the "new since start" rule).
pub fn session_ids(all: &[WtsSession]) -> Vec<u32> {
    all.iter().map(|s| s.id).collect()
}

/// Expected identity of a seat's RDP session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeatMatch<'a> {
    pub client_name: &'a str,
    /// `(domain, user)` of the seat account, `None` = logon screen seat.
    pub account: Option<(&'a str, &'a str)>,
    /// Session ids that existed when the start began.
    pub before: &'a [u32],
    /// Session ids already bound to other seats.
    pub claimed: &'a [u32],
    /// Session id the seat host reported (IronRDP). When set, only that session can
    /// match, and only with our exact client name (both reported and confirmed by WTS).
    pub reported: Option<u32>,
    /// Allow the "newest loopback session not in `before`" fallback: only for the legacy
    /// ActiveX client (it reports no session id). Off otherwise, so two seats starting
    /// close together, or an unrelated loopback session, cannot be bound to the wrong seat.
    pub newest_fallback: bool,
}

/// Pure: the seat's RDP session per the module rules. With a reported id: that session,
/// if WTS confirms it (loopback, our client name, the account). Without: our exact client
/// name; the newest new session only when `newest_fallback`.
pub fn find_rdp_seat_session(all: &[WtsSession], m: &SeatMatch<'_>) -> Option<u32> {
    let base = |s: &&WtsSession| {
        s.is_loopback_remote()
            && !m.claimed.contains(&s.id)
            && match m.account {
                // The user name is empty while the session is still at the logon / NLA stage.
                Some((d, u)) => s.user.is_empty() || s.user_matches(d, u),
                None => true,
            }
    };
    let named = |s: &&WtsSession| s.client_name.eq_ignore_ascii_case(m.client_name);
    if let Some(id) = m.reported {
        return all.iter().filter(base).filter(named).find(|s| s.id == id).map(|s| s.id);
    }
    all.iter()
        .filter(base)
        .find(named)
        .or_else(|| {
            if !m.newest_fallback {
                return None;
            }
            all.iter().filter(base).filter(|s| !m.before.contains(&s.id)).max_by_key(|s| s.id)
        })
        .map(|s| s.id)
}

#[cfg(test)]
pub(crate) fn session(id: u32, client: &str, user: &str, addr: &str, state: WtsState) -> WtsSession {
    WtsSession {
        id,
        winstation: if id == 0 { "Services".into() } else if id == 1 { "Console".into() } else { format!("RDP-Tcp#{id}") },
        state,
        client_name: client.into(),
        user: user.into(),
        domain: if user.is_empty() { String::new() } else { "HOST".into() },
        client_address: addr.parse().ok(),
        is_remote: id > 1,
        locked: Some(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seat(n: u8) -> SeatNumber {
        SeatNumber::new(n).unwrap()
    }

    #[test]
    fn client_names() {
        assert_eq!(client_name_for(seat(2)), "PSEAT-02");
        assert_eq!(client_name_for(seat(16)), "PSEAT-16");
        assert_eq!(seat_of_client_name("pseat-03"), Some(seat(3)));
        for bad in ["PSEAT-3", "PSEAT-99", "PSEAT-00", "HOST", "PSEAT-0x"] {
            assert_eq!(seat_of_client_name(bad), None, "{bad}");
        }
    }

    #[test]
    fn states_map() {
        let mut s = session(3, "PSEAT-02", "ali", "127.0.0.1", WtsState::Active);
        assert_eq!(s.seat_state(), SessionState::Active);
        s.locked = Some(true);
        assert_eq!(s.seat_state(), SessionState::Locked);
        s.user.clear();
        assert_eq!(s.seat_state(), SessionState::AtLogon);
        s.state = WtsState::Disconnected;
        assert_eq!(s.seat_state(), SessionState::Disconnected);
        let i = session(3, "PSEAT-02", "ali", "127.0.0.1", WtsState::Active).info(Some("S-1".into()));
        assert_eq!(i.user_name.as_deref(), Some(r"HOST\ali"));
        assert_eq!(WtsState::from_raw(4), WtsState::Disconnected);
        assert_eq!(WtsState::from_raw(99), WtsState::Init);
    }

    #[test]
    fn match_prefers_client_name_then_new_session() {
        let all = vec![
            session(0, "", "", "", WtsState::Disconnected),
            session(1, "", "owner", "", WtsState::Active),
            session(2, "HOSTPC", "ali", "127.0.0.1", WtsState::Active),
            session(3, "HOSTPC", "ali", "192.168.1.5", WtsState::Active),
            session(4, "HOSTPC", "veli", "127.0.0.1", WtsState::Active),
            session(5, "HOSTPC", "ali", "127.0.0.1", WtsState::Active),
            session(65_536, "", "", "", WtsState::Listen),
        ];
        let m = SeatMatch {
            client_name: "PSEAT-02",
            account: Some((".", "ali")),
            before: &[0, 1, 2, 3, 4],
            claimed: &[],
            reported: None,
            newest_fallback: true,
        };
        assert_eq!(find_rdp_seat_session(&all, &m), Some(5));
        // Without the ActiveX fallback, no client name match = no session.
        assert_eq!(find_rdp_seat_session(&all, &SeatMatch { newest_fallback: false, ..m.clone() }), None);
        // Seat 2 already owns 5: nothing left for another seat with the same account.
        let m2 = SeatMatch { claimed: &[5], ..m.clone() };
        assert_eq!(find_rdp_seat_session(&all, &m2), None);
        // Exact client name wins even if the session is old.
        let mut all2 = all.clone();
        all2[2].client_name = "pseat-02".into();
        assert_eq!(find_rdp_seat_session(&all2, &m), Some(2));
        // Logon-screen seat: user empty while at logon, any new loopback session.
        let mut all3 = all.clone();
        all3.push(session(6, "HOSTPC", "", "127.0.0.1", WtsState::Connected));
        let m3 = SeatMatch { client_name: "PSEAT-03", account: None, before: &[0, 1, 2, 3, 4, 5], ..m.clone() };
        assert_eq!(find_rdp_seat_session(&all3, &m3), Some(6));
        // Remote address is required.
        let m4 = SeatMatch { before: &[0, 1, 2, 4, 5], ..m.clone() };
        assert_eq!(find_rdp_seat_session(&all, &m4), None);
    }

    #[test]
    fn reported_session_id_must_be_confirmed() {
        let mut all = vec![
            session(1, "", "owner", "", WtsState::Active),
            session(5, "PSEAT-02", "ali", "127.0.0.1", WtsState::Active),
            session(6, "PSEAT-03", "", "127.0.0.1", WtsState::Connected),
            session(7, "OTHER", "", "127.0.0.1", WtsState::Connected),
        ];
        let m = SeatMatch {
            client_name: "PSEAT-02",
            account: Some((".", "ali")),
            before: &[1],
            claimed: &[],
            reported: Some(5),
            newest_fallback: false,
        };
        assert_eq!(find_rdp_seat_session(&all, &m), Some(5));
        // Reported id with another seat's client name, or an unrelated session: refused.
        assert_eq!(find_rdp_seat_session(&all, &SeatMatch { reported: Some(6), ..m.clone() }), None);
        assert_eq!(find_rdp_seat_session(&all, &SeatMatch { reported: Some(7), ..m.clone() }), None);
        // A reported id never falls back to another session.
        all[1].client_name = "x".into();
        assert_eq!(find_rdp_seat_session(&all, &SeatMatch { newest_fallback: true, ..m.clone() }), None);
        // IronRDP before the id is known: exact name only, never the newest session.
        let n = SeatMatch { client_name: "PSEAT-03", account: None, reported: None, ..m.clone() };
        assert_eq!(find_rdp_seat_session(&all, &n), Some(6));
        let n2 = SeatMatch { client_name: "PSEAT-04", ..n.clone() };
        assert_eq!(find_rdp_seat_session(&all, &n2), None);
        assert_eq!(find_rdp_seat_session(&all, &SeatMatch { newest_fallback: true, ..n2 }), Some(7));
    }

    #[test]
    fn user_match_rules() {
        let s = session(3, "", "Ali", "127.0.0.1", WtsState::Active);
        assert!(s.user_matches(".", "ali"));
        assert!(s.user_matches("host", "ALI"));
        assert!(!s.user_matches("EV", "ali"));
        assert!(!s.user_matches(".", ""));
    }
}
