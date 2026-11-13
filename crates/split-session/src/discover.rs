// SPDX-License-Identifier: GPL-3.0-only
//! WTS session discovery by generated client name + user + loopback (spec 30 §6).

use crate::{OwnedHandle, SessionError};
use serde::{Deserialize, Serialize};
use std::net::IpAddr;

pub const CLIENT_NAME_PREFIX: &str = "PSS";
/// Session ids at or above this are listeners, never user sessions.
pub const LISTENER_ID_MIN: u32 = 65_536;

/// `"PSS"` + 8 upper-case hex = 11 ASCII chars (< 15 UTF-16 limit). Deterministic from the
/// seat id; `salt` bumps on collision with an existing client name.
pub fn client_name_for(seat_id: &str, salt: u8) -> String {
    // FNV-1a 32-bit over the lower-cased id, then the salt.
    let mut h: u32 = 0x811C_9DC5;
    for b in seat_id.trim().bytes().map(|b| b.to_ascii_lowercase()).chain(std::iter::once(salt)) {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    format!("{CLIENT_NAME_PREFIX}{h:08X}")
}

pub fn is_our_client_name(name: &str) -> bool {
    name.len() == 11
        && name.starts_with(CLIENT_NAME_PREFIX)
        && name[3..].bytes().all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(&b))
}

/// First salt whose client name is not in `taken` (case-insensitive).
pub fn unique_client_name(seat_id: &str, taken: &[String]) -> String {
    (0..=u8::MAX)
        .map(|s| client_name_for(seat_id, s))
        .find(|n| !taken.iter().any(|t| t.eq_ignore_ascii_case(n)))
        .unwrap_or_else(|| client_name_for(seat_id, 0))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
    /// From `WTS_CONNECTSTATE_CLASS`.
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
}

impl WtsSession {
    /// Services, console and listener sessions are never seat sessions.
    pub fn is_system_or_listener(&self) -> bool {
        self.id == 0
            || self.id >= LISTENER_ID_MIN
            || self.state == WtsState::Listen
            || self.winstation.eq_ignore_ascii_case("Services")
            || self.winstation.eq_ignore_ascii_case("Console")
    }

    pub fn matches(&self, client_name: &str, user: &str) -> bool {
        !self.is_system_or_listener()
            && self.client_name.eq_ignore_ascii_case(client_name)
            && self.user.eq_ignore_ascii_case(user)
            && self.client_address.is_some_and(|a| a.is_loopback())
    }
}

pub trait Wts: Send + Sync {
    fn sessions(&self) -> Result<Vec<WtsSession>, SessionError>;
    /// Callers check `is_remote` first; the real impl refuses non-remote sessions too.
    fn logoff(&self, id: u32, wait: bool) -> Result<(), SessionError>;
    /// `WTSQueryUserToken` (needs SYSTEM + SeTcbPrivilege).
    fn user_token(&self, id: u32) -> Result<OwnedHandle, SessionError>;
    /// SID (`"S-1-5-…"`) of the user signed in at the physical console, `None` when nobody
    /// is (spec 30 §10: the console user is never a seat user).
    fn console_user_sid(&self) -> Result<Option<String>, SessionError>;
}

/// Pure: first session matching client name AND user (case-insensitive) AND loopback
/// address; skips Services/Console/listeners (id >= 65536).
pub fn find_seat_session(all: &[WtsSession], client_name: &str, user: &str) -> Option<u32> {
    all.iter().find(|s| s.matches(client_name, user)).map(|s| s.id)
}

/// Like [`find_seat_session`], but a session id reported by the client wins when it
/// passes the same three checks.
pub fn find_seat_session_preferring(all: &[WtsSession], client_name: &str, user: &str, reported: Option<u32>) -> Option<u32> {
    reported
        .and_then(|id| all.iter().find(|s| s.id == id && s.matches(client_name, user)).map(|s| s.id))
        .or_else(|| find_seat_session(all, client_name, user))
}

#[cfg(test)]
pub(crate) fn session(id: u32, client: &str, user: &str, addr: &str, state: WtsState) -> WtsSession {
    WtsSession {
        id,
        winstation: if id == 0 { "Services".into() } else { format!("RDP-Tcp#{id}") },
        state,
        client_name: client.into(),
        user: user.into(),
        domain: "HOST".into(),
        client_address: addr.parse().ok(),
        is_remote: id != 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_names_are_short_stable_and_ours() {
        let id = "3f2a1b4c-0000-4000-8000-123456789abc";
        let a = client_name_for(id, 0);
        assert_eq!(a.len(), 11);
        assert!(a.encode_utf16().count() < 15);
        assert!(a.starts_with("PSS"));
        assert_eq!(a, client_name_for(&id.to_uppercase(), 0), "case-insensitive id");
        assert_ne!(a, client_name_for(id, 1));
        assert_ne!(a, client_name_for("3f2a1b4c-0000-4000-8000-123456789abd", 0));
        assert!(is_our_client_name(&a));
        for bad in ["PSS1234567", "PSS123456789", "pss1234ABCD", "PSS1234abcd", "XYZ1234ABCD", "DESKTOP-1"] {
            assert!(!is_our_client_name(bad), "{bad}");
        }
    }

    #[test]
    fn unique_name_skips_taken() {
        let id = "seat-id";
        let first = client_name_for(id, 0);
        let n = unique_client_name(id, &[first.to_lowercase()]);
        assert_eq!(n, client_name_for(id, 1));
    }

    #[test]
    fn find_requires_name_user_and_loopback() {
        let n = "PSS0000ABCD";
        let all = vec![
            session(0, "", "", "", WtsState::Disconnected),
            session(1, n, "seat1", "127.0.0.1", WtsState::Active), // console winstation below
            session(2, n, "other", "127.0.0.1", WtsState::Active),
            session(3, n, "seat1", "192.168.1.5", WtsState::Active),
            session(4, "PSS0000FFFF", "seat1", "127.0.0.1", WtsState::Active),
            session(65_536, n, "seat1", "127.0.0.1", WtsState::Listen),
            session(5, n, "SEAT1", "::1", WtsState::Active),
            session(6, n, "seat1", "127.0.0.1", WtsState::Active),
        ];
        let mut all = all;
        all[1].winstation = "Console".into();
        assert_eq!(find_seat_session(&all, n, "seat1"), Some(5));
        assert_eq!(find_seat_session(&all, &n.to_lowercase(), "Seat1"), Some(5));
        assert_eq!(find_seat_session(&all, "PSS99999999", "seat1"), None);
        assert_eq!(find_seat_session_preferring(&all, n, "seat1", Some(6)), Some(6));
        assert_eq!(find_seat_session_preferring(&all, n, "seat1", Some(3)), Some(5), "reported id must still match");
        assert_eq!(find_seat_session_preferring(&all, n, "seat1", None), Some(5));
    }

    #[test]
    fn state_from_raw() {
        assert_eq!(WtsState::from_raw(0), WtsState::Active);
        assert_eq!(WtsState::from_raw(4), WtsState::Disconnected);
        assert_eq!(WtsState::from_raw(6), WtsState::Listen);
        assert_eq!(WtsState::from_raw(99), WtsState::Init);
    }
}
