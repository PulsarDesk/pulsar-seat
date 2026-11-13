// SPDX-License-Identifier: GPL-3.0-only
//! Caller identity and enforcement of the authorisation table
//! ([`seat_proto::RequestBody::access`], architecture §5).
//!
//! The pipe layer fills [`Caller`] from the client's token (impersonation) and process id;
//! nothing here trusts what the client claims about itself except its `ClientKind`, and
//! the privileged kinds are verified in `hello`: a seat host by PID + one-time token, an
//! agent by PID (the agent process the service launched) + session.
//!
//! `admin_only` restricts settings and status for users, never the service's own verified
//! processes: a seat host and an agent keep `status` access (subscribe / poll of their own
//! scoped topics), spec 00 §5.5.

use seat_core::config::GlobalSettings;
use seat_core::SeatNumber;
use seat_proto::{Access, ClientKind, ErrorCode, ProtoError};

/// What the service knows about one pipe connection's peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    pub pid: u32,
    /// Session of the caller's token.
    pub session_id: u32,
    pub user_sid: Option<String>,
    /// Token has the Administrators group enabled (elevated).
    pub is_admin: bool,
    pub is_system: bool,
    /// Declared in `hello` (`None` before hello).
    pub client: Option<ClientKind>,
    /// Set when a seat host proved PID + token for this seat.
    pub host_seat: Option<SeatNumber>,
    /// Set when the caller is the agent the service launched in this session.
    pub agent_session: Option<u32>,
    /// Set when the caller is the console listener the service launched (PID + token).
    pub console_listener: bool,
    /// Set when the caller is the Pulsar credential provider in LogonUI (SYSTEM, LogonUI
    /// image, this session); the session it may act on. Added 2026-11-13.
    pub credprov_session: Option<u32>,
    /// Set when the caller is a kiosk shell the `desktops` backend started for this seat
    /// (PID + session). Added 2026-11-13.
    pub kiosk_seat: Option<SeatNumber>,
}

impl Caller {
    pub fn admin(&self) -> bool {
        self.is_admin || self.is_system
    }

    /// For tests.
    #[cfg(test)]
    pub fn system() -> Self {
        Self {
            pid: 0,
            session_id: 0,
            user_sid: Some("S-1-5-18".into()),
            is_admin: true,
            is_system: true,
            client: None,
            host_seat: None,
            agent_session: None,
            console_listener: false,
            credprov_session: None,
            kiosk_seat: None,
        }
    }

    /// A seat host, agent, console listener or kiosk shell the service started and
    /// verified.
    pub fn service_process(&self) -> bool {
        // The console listener too: as the host account it is no longer SYSTEM (= admin),
        // so `admin_only` must not cut it off its status / subscribe access. The kiosk
        // shell runs as the (maybe non-admin) console user and needs `list_devices`.
        self.host_seat.is_some() || self.agent_session.is_some() || self.console_listener || self.kiosk_seat.is_some()
    }
}

/// Check one request's access level. `caller_seat` = seat of the caller's session.
pub fn authorize(
    access: Access,
    caller: &Caller,
    settings: &GlobalSettings,
    caller_seat: Option<SeatNumber>,
) -> Result<(), ProtoError> {
    if access == Access::Any {
        return Ok(());
    }
    if caller.client.is_none() {
        return Err(ProtoError::new(ErrorCode::BadRequest, "hello must be the first request"));
    }
    let admin = caller.admin();
    let deny = |why: &str| Err(ProtoError::new(ErrorCode::AccessDenied, why.to_string()));
    match access {
        Access::Any => Ok(()),
        Access::Host(seat) => {
            if caller.host_seat == Some(seat) {
                Ok(())
            } else {
                deny("not the seat host of this seat")
            }
        }
        Access::Agent => {
            if caller.client == Some(ClientKind::Agent) && caller.agent_session.is_some() {
                Ok(())
            } else {
                deny("agent requests need the verified agent")
            }
        }
        Access::ConsoleListener => {
            if caller.client == Some(ClientKind::ConsoleListener) && caller.console_listener {
                Ok(())
            } else {
                deny("console input reports need the verified console listener")
            }
        }
        Access::CredentialProvider => {
            if caller.client == Some(ClientKind::CredentialProvider) && caller.credprov_session == Some(caller.session_id) {
                Ok(())
            } else {
                deny("credential provider requests need the verified LogonUI provider")
            }
        }
        // Verified service processes keep status / their own topics under admin_only.
        Access::Status if caller.service_process() => Ok(()),
        _ if settings.admin_only && !admin => {
            Err(ProtoError::new(ErrorCode::AdminOnly, "settings are restricted to administrators"))
        }
        Access::Status => Ok(()),
        Access::Admin => {
            if admin {
                Ok(())
            } else {
                deny("needs an elevated administrator")
            }
        }
        Access::OwnSeat(seat) => {
            if admin {
                return Ok(());
            }
            let interactive = matches!(caller.client, Some(ClientKind::Manager | ClientKind::Cli));
            if settings.users_may_control_own_seat && interactive && caller_seat == Some(seat) {
                Ok(())
            } else {
                deny("only the seat's own user or an administrator")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(seat_session: u32) -> Caller {
        Caller {
            pid: 42,
            session_id: seat_session,
            user_sid: Some("S-1-5-21-1-1001".into()),
            is_admin: false,
            is_system: false,
            client: Some(ClientKind::Manager),
            host_seat: None,
            agent_session: None,
            console_listener: false,
            credprov_session: None,
            kiosk_seat: None,
        }
    }

    fn s(n: u8) -> SeatNumber {
        SeatNumber::new(n).unwrap()
    }

    #[test]
    fn table() {
        let g = GlobalSettings::default();
        let u = user(3);
        assert!(authorize(Access::Any, &u, &g, None).is_ok());
        assert!(authorize(Access::Status, &u, &g, None).is_ok());
        assert_eq!(authorize(Access::Admin, &u, &g, None).unwrap_err().code, ErrorCode::AccessDenied);
        assert!(authorize(Access::OwnSeat(s(2)), &u, &g, Some(s(2))).is_ok());
        assert!(authorize(Access::OwnSeat(s(3)), &u, &g, Some(s(2))).is_err());
        assert!(authorize(Access::Admin, &Caller { client: Some(ClientKind::Manager), ..Caller::system() }, &g, None).is_ok());
        assert!(authorize(Access::Host(s(2)), &u, &g, None).is_err());
        let host = Caller { client: Some(ClientKind::SeatHost), host_seat: Some(s(2)), ..u.clone() };
        assert!(authorize(Access::Host(s(2)), &host, &g, None).is_ok());
        assert!(authorize(Access::Host(s(3)), &host, &g, None).is_err());
    }

    #[test]
    fn admin_only_and_hello() {
        let g = GlobalSettings { admin_only: true, ..Default::default() };
        let u = user(1);
        assert_eq!(authorize(Access::Status, &u, &g, None).unwrap_err().code, ErrorCode::AdminOnly);
        assert!(authorize(Access::Any, &u, &g, None).is_ok());
        let no_hello = Caller { client: None, ..u };
        assert_eq!(authorize(Access::Status, &no_hello, &GlobalSettings::default(), None).unwrap_err().code, ErrorCode::BadRequest);
        let g2 = GlobalSettings { users_may_control_own_seat: false, ..Default::default() };
        assert!(authorize(Access::OwnSeat(s(1)), &user(1), &g2, Some(s(1))).is_err());
    }

    #[test]
    fn admin_only_keeps_service_processes_working() {
        let g = GlobalSettings { admin_only: true, ..Default::default() };
        let host = Caller { client: Some(ClientKind::SeatHost), host_seat: Some(s(2)), ..user(3) };
        assert!(authorize(Access::Status, &host, &g, None).is_ok());
        assert!(authorize(Access::Host(s(2)), &host, &g, None).is_ok());
        // The console listener as the host account (not SYSTEM, not admin) keeps status.
        let listener = Caller { client: Some(ClientKind::ConsoleListener), console_listener: true, ..user(1) };
        assert!(authorize(Access::Status, &listener, &g, None).is_ok());
        assert_eq!(authorize(Access::Admin, &listener, &g, None).unwrap_err().code, ErrorCode::AdminOnly);
        // ...but not settings.
        assert_eq!(authorize(Access::Admin, &host, &g, None).unwrap_err().code, ErrorCode::AdminOnly);
        let agent = Caller { client: Some(ClientKind::Agent), agent_session: Some(3), ..user(3) };
        assert!(authorize(Access::Status, &agent, &g, None).is_ok());
        assert!(authorize(Access::Agent, &agent, &g, None).is_ok());
        // A process that only declares itself an agent gets nothing.
        let fake = Caller { client: Some(ClientKind::Agent), ..user(3) };
        assert!(authorize(Access::Agent, &fake, &GlobalSettings::default(), None).is_err());
        // The console listener: only once verified, also under admin_only.
        let fake = Caller { client: Some(ClientKind::ConsoleListener), ..user(1) };
        assert!(authorize(Access::ConsoleListener, &fake, &g, None).is_err());
        let real = Caller { console_listener: true, ..fake };
        assert!(authorize(Access::ConsoleListener, &real, &g, None).is_ok());
        assert!(authorize(Access::ConsoleListener, &host, &g, None).is_err());
        // The credential provider: only once verified, only for its own session.
        let fake = Caller { client: Some(ClientKind::CredentialProvider), ..Caller::system() };
        assert!(authorize(Access::CredentialProvider, &fake, &g, None).is_err());
        let real = Caller { credprov_session: Some(0), ..fake.clone() };
        assert!(authorize(Access::CredentialProvider, &real, &g, None).is_ok());
        let moved = Caller { credprov_session: Some(4), ..fake };
        assert!(authorize(Access::CredentialProvider, &moved, &g, None).is_err());
        assert!(authorize(Access::CredentialProvider, &host, &g, None).is_err());
    }

    #[test]
    fn admin_only_keeps_the_verified_kiosk_shell_working() {
        let g = GlobalSettings { admin_only: true, ..Default::default() };
        // Declared only: an ordinary non-admin user under admin_only.
        let fake = Caller { client: Some(ClientKind::KioskShell), ..user(1) };
        assert_eq!(authorize(Access::Status, &fake, &g, None).unwrap_err().code, ErrorCode::AdminOnly);
        let real = Caller { kiosk_seat: Some(s(2)), ..fake };
        assert!(authorize(Access::Status, &real, &g, None).is_ok());
        // Status only: no settings, no seat-host requests.
        assert_eq!(authorize(Access::Admin, &real, &g, None).unwrap_err().code, ErrorCode::AdminOnly);
        assert!(authorize(Access::Host(s(2)), &real, &g, None).is_err());
    }
}
