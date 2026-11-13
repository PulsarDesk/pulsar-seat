// SPDX-License-Identifier: GPL-3.0-only
//! Per-seat automatic logon (spec 30 §6.2.6–6.2.10, §6.3).
//!
//! Under the RDP backends a seat logs on by handing the stored credentials to its seat
//! host's RDP connection (`host_take_credentials`, once). No credential provider, no
//! Winlogon change: Pulsar Seat never writes `AutoAdminLogon` / `DefaultPassword` for user
//! accounts. This module is the pure policy the service and the host share:
//!
//! - which credentials (if any) a start uses ([`credentials_for`]);
//! - how a seat host's disconnect is classified ([`classify_disconnect`]);
//! - what happens after a seat session ends ([`after_session_end`]): re-logon after
//!   `relogon_delay_ms`, logon screen, reconnect with back-off, or stop — and **never** a
//!   retry loop after a logon failure (lockout risk; the seat shows its logon prompt and
//!   the password is marked failing instead, spec 30 §6.3).

use std::time::Duration;

use seat_core::Secret;
use seat_core::backend::session::SeatCredentials;
use seat_core::config::{SeatAccount, SeatConfig, Timings};
use serde::{Deserialize, Serialize};

/// Credentials for a seat start. `None` = the seat shows its logon screen (no account set).
/// An account without a stored password still pre-fills the user name.
pub fn credentials_for(seat: &SeatConfig, password: Option<Secret>) -> Option<SeatCredentials> {
    let (domain, user) = seat.account.domain_user()?;
    Some(SeatCredentials { domain: domain.to_owned(), user: user.to_owned(), password })
}

/// RDP logon domain: a local account (`"."`) is sent with an empty domain, which the
/// server resolves to the local machine (HYPOTHESIS for every Server build; the computer
/// name works as well).
pub fn rdp_domain(domain: &str) -> &str {
    if domain == "." { "" } else { domain }
}

/// User-name hint for the logon screen: the account, else the last user of this seat
/// (spec 30 §6.2.8), else nothing.
pub fn user_hint(account: &SeatAccount, last_user: Option<&(String, String)>) -> (String, String) {
    match account.domain_user() {
        Some((d, u)) => (rdp_domain(d).to_owned(), u.to_owned()),
        None => last_user.map(|(d, u)| (rdp_domain(d).to_owned(), u.clone())).unwrap_or_default(),
    }
}

/// Why a seat host's RDP connection ended, from the control's documented disconnect
/// reason and `ExtendedDisconnectReason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostDisconnect {
    /// The user signed out (or an API log-off).
    LoggedOff,
    /// The session was disconnected (switch user, disconnect, idle / logon time-out).
    Disconnected,
    /// Another connection took the session over (`REPLACED_BY_OTHER_CONNECTION`): the user
    /// chose to use it elsewhere; taking it back would ping-pong.
    Replaced,
    /// Wrong / expired password, disabled account, logon hours (no retry!).
    LogonFailed,
    /// The server refused the connection (no licence, policy, not in Remote Desktop Users).
    Denied,
    /// Network / protocol problem; reconnecting is fine.
    Transient,
    /// We disconnected on purpose (stop, exit command).
    Local,
}

/// `ExtendedDisconnectReasonCode` values (documented enum of the RDP control).
pub mod ext {
    pub const NO_INFO: i32 = 0;
    pub const API_INITIATED_DISCONNECT: i32 = 1;
    pub const API_INITIATED_LOGOFF: i32 = 2;
    pub const SERVER_IDLE_TIMEOUT: i32 = 3;
    pub const SERVER_LOGON_TIMEOUT: i32 = 4;
    pub const REPLACED_BY_OTHER_CONNECTION: i32 = 5;
    pub const OUT_OF_MEMORY: i32 = 6;
    pub const SERVER_DENIED_CONNECTION: i32 = 7;
    pub const SERVER_DENIED_CONNECTION_FIPS: i32 = 8;
    pub const SERVER_INSUFFICIENT_PRIVILEGES: i32 = 9;
    pub const SERVER_FRESH_CREDS_REQUIRED: i32 = 10;
    pub const RPC_INITIATED_DISCONNECT_BY_USER: i32 = 11;
    pub const LOGOFF_BY_USER: i32 = 12;
    /// 0x100..=0x10F: licensing errors.
    pub const LICENSE_FIRST: i32 = 0x100;
    pub const LICENSE_LAST: i32 = 0x10F;
}

/// Top-level disconnect reasons of the control (`OnDisconnected(discReason)`).
pub mod disc {
    /// Local disconnect (we called `Disconnect`).
    pub const LOCAL_NOT_ERROR: i32 = 1;
    /// Remote disconnect by the user (log-off / disconnect in the session).
    pub const REMOTE_BY_USER: i32 = 2;
    /// Disconnected by the server.
    pub const BY_SERVER: i32 = 3;
    /// CredSSP / NLA logon failure ("the logon attempt failed"). HYPOTHESIS: the value most
    /// public reports show for a wrong password with NLA on; verify on a Server test host.
    pub const LOGON_FAILURE: i32 = 2055;
    /// Account locked / password expired / must change: reported with other codes in the
    /// 0x07 low byte family (HYPOTHESIS, treated as logon failures).
    pub const LOW_BYTE_SECURITY: i32 = 0x07;
}

/// Pure classification. Logon failures are recognised first so they never loop.
pub fn classify_disconnect(reason: i32, extended: i32) -> HostDisconnect {
    use HostDisconnect::*;
    match extended {
        ext::API_INITIATED_LOGOFF | ext::LOGOFF_BY_USER => return LoggedOff,
        ext::SERVER_DENIED_CONNECTION | ext::SERVER_DENIED_CONNECTION_FIPS | ext::SERVER_INSUFFICIENT_PRIVILEGES => return Denied,
        ext::LICENSE_FIRST..=ext::LICENSE_LAST => return Denied,
        ext::SERVER_FRESH_CREDS_REQUIRED => return LogonFailed,
        ext::REPLACED_BY_OTHER_CONNECTION => return Replaced,
        ext::RPC_INITIATED_DISCONNECT_BY_USER | ext::API_INITIATED_DISCONNECT => return Disconnected,
        ext::SERVER_IDLE_TIMEOUT | ext::SERVER_LOGON_TIMEOUT => return Disconnected,
        _ => {}
    }
    match reason {
        disc::LOCAL_NOT_ERROR => Local,
        disc::REMOTE_BY_USER => LoggedOff,
        disc::BY_SERVER => Disconnected,
        disc::LOGON_FAILURE => LogonFailed,
        r if r > 0xFF && (r & 0xFF) == disc::LOW_BYTE_SECURITY => LogonFailed,
        _ => Transient,
    }
}

/// What the service does after a seat session ended or its host disconnected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NextStep {
    /// Start the seat again after this delay (auto-logon, or reconnect the host).
    Restart { after: Duration, with_credentials: bool },
    /// Show the seat's logon prompt: start without credentials, user-name hint only. With
    /// `mark_password_failing` the stored password is marked failing first.
    LogonScreen { mark_password_failing: bool },
    /// Leave the seat off / in error; the manager offers a retry.
    Stop { mark_password_failing: bool },
}

/// Consecutive reconnects (disconnect / transient failure, no successful logon between)
/// before the seat is stopped.
pub const MAX_RECONNECT_ATTEMPTS: u32 = 5;
/// Cap of the reconnect back-off.
pub const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(60);

/// Back-off before reconnect number `attempt` (0-based): 1, 2, 4, 8 … s, capped at
/// [`MAX_RECONNECT_DELAY`].
pub fn reconnect_delay(attempt: u32) -> Duration {
    Duration::from_secs(1u64 << attempt.min(6)).min(MAX_RECONNECT_DELAY)
}

/// Pure decision (spec 30 §6.2.10–6.2.12, §6.3). `attempt` = reconnects already made
/// since the seat last reached a logged-on session (the service resets it on
/// `LoginComplete`).
///
/// - `stopping`: the service itself stopped the seat → stop.
/// - logon failure → logon prompt on the seat + mark the password failing (no retry).
/// - denied → stop (licence / policy / group membership; a retry would fail again).
/// - replaced by another connection → stop (no ping-pong with the user's other client).
/// - logged off with auto-logon → restart after `relogon_delay_ms`; without → logon screen.
/// - disconnected → reconnect the host (same session, spec 30 §6.2.11) with back-off;
///   transient → the same, but stored credentials go out at most once (`attempt == 0`);
///   after [`MAX_RECONNECT_ATTEMPTS`] → stop.
pub fn after_session_end(
    event: HostDisconnect,
    has_password: bool,
    password_failing: bool,
    stopping: bool,
    attempt: u32,
    t: &Timings,
) -> NextStep {
    use HostDisconnect::*;
    if stopping || event == Local {
        return NextStep::Stop { mark_password_failing: false };
    }
    let creds = has_password && !password_failing;
    match event {
        LogonFailed => NextStep::LogonScreen { mark_password_failing: has_password },
        Denied | Replaced => NextStep::Stop { mark_password_failing: false },
        LoggedOff if creds => NextStep::Restart { after: Duration::from_millis(u64::from(t.relogon_delay_ms)), with_credentials: true },
        LoggedOff => NextStep::LogonScreen { mark_password_failing: false },
        Disconnected | Transient if attempt >= MAX_RECONNECT_ATTEMPTS => NextStep::Stop { mark_password_failing: false },
        Disconnected => NextStep::Restart { after: reconnect_delay(attempt), with_credentials: creds },
        Transient => NextStep::Restart { after: reconnect_delay(attempt), with_credentials: creds && attempt == 0 },
        Local => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use seat_core::SeatNumber;

    #[test]
    fn credentials() {
        let mut s = SeatConfig::new(SeatNumber::new(2).unwrap());
        assert!(credentials_for(&s, Some(Secret::new("x"))).is_none());
        s.account = SeatAccount::Local { user: "ali".into() };
        let c = credentials_for(&s, None).unwrap();
        assert_eq!((c.domain.as_str(), c.user.as_str(), c.password.is_none()), (".", "ali", true));
        assert_eq!(rdp_domain("."), "");
        assert_eq!(rdp_domain("EV"), "EV");
        assert_eq!(user_hint(&s.account, None), (String::new(), "ali".into()));
        let last = ("EV".to_string(), "veli".to_string());
        assert_eq!(user_hint(&SeatAccount::LogonScreen, Some(&last)), ("EV".into(), "veli".into()));
    }

    #[test]
    fn disconnect_classes() {
        assert_eq!(classify_disconnect(2, ext::LOGOFF_BY_USER), HostDisconnect::LoggedOff);
        assert_eq!(classify_disconnect(3, ext::REPLACED_BY_OTHER_CONNECTION), HostDisconnect::Replaced);
        assert_eq!(classify_disconnect(3, ext::API_INITIATED_DISCONNECT), HostDisconnect::Disconnected);
        assert_eq!(classify_disconnect(3, 0x105), HostDisconnect::Denied);
        assert_eq!(classify_disconnect(disc::LOGON_FAILURE, 0), HostDisconnect::LogonFailed);
        assert_eq!(classify_disconnect(0x0D07, 0), HostDisconnect::LogonFailed);
        assert_eq!(classify_disconnect(1, 0), HostDisconnect::Local);
        assert_eq!(classify_disconnect(516, 0), HostDisconnect::Transient);
    }

    #[test]
    fn never_loops_on_logon_failure() {
        let t = Timings::default();
        // Logon failure: the logon prompt, password marked failing, no retry with it.
        assert_eq!(
            after_session_end(HostDisconnect::LogonFailed, true, false, false, 0, &t),
            NextStep::LogonScreen { mark_password_failing: true }
        );
        assert_eq!(
            after_session_end(HostDisconnect::LoggedOff, true, false, false, 0, &t),
            NextStep::Restart { after: Duration::from_millis(3000), with_credentials: true }
        );
        assert_eq!(after_session_end(HostDisconnect::LoggedOff, true, true, false, 0, &t), NextStep::LogonScreen { mark_password_failing: false });
        assert_eq!(after_session_end(HostDisconnect::LoggedOff, false, false, false, 0, &t), NextStep::LogonScreen { mark_password_failing: false });
        assert_eq!(
            after_session_end(HostDisconnect::Disconnected, false, false, false, 0, &t),
            NextStep::Restart { after: Duration::from_secs(1), with_credentials: false }
        );
        assert_eq!(after_session_end(HostDisconnect::Transient, true, false, true, 0, &t), NextStep::Stop { mark_password_failing: false });
        assert_eq!(after_session_end(HostDisconnect::Denied, true, false, false, 0, &t), NextStep::Stop { mark_password_failing: false });
        assert_eq!(after_session_end(HostDisconnect::Replaced, true, false, false, 0, &t), NextStep::Stop { mark_password_failing: false });
    }

    #[test]
    fn reconnects_back_off_and_stop() {
        let t = Timings::default();
        let delays: Vec<u64> = (0..8).map(|a| reconnect_delay(a).as_secs()).collect();
        assert_eq!(delays, [1, 2, 4, 8, 16, 32, 60, 60]);
        // Disconnected: credentials on every attempt (same session), back-off, then stop.
        for a in 0..MAX_RECONNECT_ATTEMPTS {
            assert_eq!(
                after_session_end(HostDisconnect::Disconnected, true, false, false, a, &t),
                NextStep::Restart { after: reconnect_delay(a), with_credentials: true }
            );
        }
        assert_eq!(
            after_session_end(HostDisconnect::Disconnected, true, false, false, MAX_RECONNECT_ATTEMPTS, &t),
            NextStep::Stop { mark_password_failing: false }
        );
        // Transient (unclassified, e.g. 516 socket connect failed): credentials once only.
        assert_eq!(
            after_session_end(HostDisconnect::Transient, true, false, false, 0, &t),
            NextStep::Restart { after: Duration::from_secs(1), with_credentials: true }
        );
        assert_eq!(
            after_session_end(HostDisconnect::Transient, true, false, false, 1, &t),
            NextStep::Restart { after: Duration::from_secs(2), with_credentials: false }
        );
        assert_eq!(
            after_session_end(HostDisconnect::Transient, true, false, false, MAX_RECONNECT_ATTEMPTS, &t),
            NextStep::Stop { mark_password_failing: false }
        );
    }
}
