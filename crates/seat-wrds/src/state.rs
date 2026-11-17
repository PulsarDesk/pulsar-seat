// SPDX-License-Identifier: GPL-3.0-only
//! Pure state of the provider: the connections the service asked for, what TermService may
//! learn about each one, and the messages that go back to the service. The COM layer
//! (`com.rs`) only translates calls into these methods, so every rule here is unit-tested
//! on any host.
//!
//! Rules:
//! - A connection exists only because the service sent `connect` (validated again here).
//!   TermService never gets a connection we did not offer.
//! - The password leaves the provider once, in `GetUserCredentials`
//!   ([`Core::take_credentials`]); the copy here is wiped then, or when the connection
//!   closes.
//! - `IsUserAllowedToLogon` admits only the account the service named
//!   ([`Core::allowed_to_logon`]).
//! - Messages to the service are queued ([`Core::drain`]) and never block a TermService call.

use seat_proto::wrds::{ConnectRequest, PROTOCOL, ProviderMsg, ServiceMsg, same_principal};
use seat_proto::wrds::Secret;
use std::collections::{HashMap, VecDeque};

/// Connections at the same time (seats are far fewer).
pub const MAX_CONNECTIONS: usize = 64;
/// Queued messages kept while the service is away (oldest dropped first).
pub const MAX_OUTBOX: usize = 256;
/// Longest logon-error text forwarded.
pub const MAX_MESSAGE: usize = 512;

/// Where a connection is in TermService's sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnState {
    Offered,
    Accepted,
    SessionKnown,
    LoggedOn,
    Disconnected,
}

/// What TermService learns about the client (`GetClientData`). No password.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientInfo {
    pub client_name: String,
    pub user: String,
    pub domain: String,
    pub width: u16,
    pub height: u16,
    pub keyboard_layout: u32,
}

/// The seat user's credentials, handed out once.
#[derive(Debug)]
pub struct Credentials {
    pub user: String,
    pub domain: String,
    pub password: Secret,
}

#[derive(Debug)]
struct Conn {
    info: ClientInfo,
    password: Option<Secret>,
    state: ConnState,
    session_id: Option<u32>,
}

/// What the COM layer must do after a service message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    None,
    /// Create the connection object and call `OnConnected` on the listener callback.
    Offer(u64),
    /// Report the connection broken (`BrokenConnection`).
    Break(u64),
}

#[derive(Debug, Default)]
pub struct Core {
    conns: HashMap<u64, Conn>,
    listening: bool,
    welcomed: bool,
    outbox: VecDeque<ProviderMsg>,
}

impl Core {
    pub fn new() -> Self {
        Self::default()
    }

    fn push(&mut self, m: ProviderMsg) {
        if self.outbox.len() >= MAX_OUTBOX {
            self.outbox.pop_front();
        }
        self.outbox.push_back(m);
    }

    /// Messages for the service, oldest first.
    pub fn drain(&mut self) -> Vec<ProviderMsg> {
        self.outbox.drain(..).collect()
    }

    /// First frame of a (re)connected pipe. Anything queued before stays queued behind it.
    pub fn hello(&mut self, pid: u32, version: &str) -> ProviderMsg {
        self.welcomed = false;
        ProviderMsg::Hello { protocol: PROTOCOL, provider_version: version.into(), pid, listening: self.listening }
    }

    pub fn listening(&self) -> bool {
        self.listening
    }

    pub fn set_listening(&mut self, on: bool) {
        if self.listening != on {
            self.listening = on;
            self.push(ProviderMsg::Listening { listening: on });
        }
    }

    pub fn welcomed(&self) -> bool {
        self.welcomed
    }

    pub fn on_service(&mut self, m: ServiceMsg) -> Action {
        match m {
            ServiceMsg::Welcome { protocol } => {
                self.welcomed = protocol == PROTOCOL;
                Action::None
            }
            ServiceMsg::Connect(req) => self.connect(req),
            ServiceMsg::Disconnect { conn_id } => {
                if self.conns.contains_key(&conn_id) {
                    Action::Break(conn_id)
                } else {
                    Action::None
                }
            }
        }
    }

    fn refuse(&mut self, conn_id: u64, reason: &str) -> Action {
        self.push(ProviderMsg::Refused { conn_id, reason: reason.into() });
        Action::None
    }

    fn connect(&mut self, req: ConnectRequest) -> Action {
        let id = req.conn_id;
        if !self.welcomed {
            return self.refuse(id, "not_welcomed");
        }
        if let Err(e) = req.validate() {
            return self.refuse(id, &format!("invalid: {e}"));
        }
        if !self.listening {
            return self.refuse(id, "not_listening");
        }
        if self.conns.contains_key(&id) {
            return self.refuse(id, "duplicate");
        }
        if self.conns.len() >= MAX_CONNECTIONS {
            return self.refuse(id, "busy");
        }
        let info = ClientInfo {
            client_name: req.client_name.clone(),
            user: req.user.clone(),
            domain: req.domain.clone(),
            // validate() bounds both to MIN_SIZE..=MAX_SIZE (8192), so they fit a u16.
            width: req.width as u16,
            height: req.height as u16,
            keyboard_layout: req.keyboard_layout,
        };
        let password = Some(req.password.clone());
        drop(req);
        self.conns.insert(id, Conn { info, password, state: ConnState::Offered, session_id: None });
        Action::Offer(id)
    }

    /// `OnConnected` failed: the connection never reached TermService.
    pub fn offer_failed(&mut self, conn_id: u64, reason: &str) {
        self.conns.remove(&conn_id);
        self.push(ProviderMsg::Refused { conn_id, reason: reason.into() });
    }

    pub fn state(&self, conn_id: u64) -> Option<ConnState> {
        self.conns.get(&conn_id).map(|c| c.state)
    }

    pub fn accepted(&mut self, conn_id: u64) -> bool {
        let Some(c) = self.conns.get_mut(&conn_id) else { return false };
        if c.state == ConnState::Offered {
            c.state = ConnState::Accepted;
        }
        self.push(ProviderMsg::Accepted { conn_id });
        true
    }

    pub fn client_info(&self, conn_id: u64) -> Option<ClientInfo> {
        self.conns.get(&conn_id).map(|c| c.info.clone())
    }

    /// The credentials, once. A second call gets `None` (the password is gone).
    pub fn take_credentials(&mut self, conn_id: u64) -> Option<Credentials> {
        let c = self.conns.get_mut(&conn_id)?;
        let password = c.password.take()?;
        Some(Credentials { user: c.info.user.clone(), domain: c.info.domain.clone(), password })
    }

    /// Only the account the service named may log on through this connection.
    pub fn allowed_to_logon(&self, conn_id: u64, domain: &str, user: &str, computer: &str) -> bool {
        self.conns.get(&conn_id).is_some_and(|c| same_principal(&c.info.domain, &c.info.user, domain, user, computer))
    }

    pub fn session_id(&mut self, conn_id: u64, session_id: u32) {
        let Some(c) = self.conns.get_mut(&conn_id) else { return };
        c.session_id = Some(session_id);
        if matches!(c.state, ConnState::Offered | ConnState::Accepted) {
            c.state = ConnState::SessionKnown;
        }
        self.push(ProviderMsg::SessionId { conn_id, session_id });
    }

    pub fn logged_on(&mut self, conn_id: u64, session_id: u32) {
        let Some(c) = self.conns.get_mut(&conn_id) else { return };
        c.session_id = Some(session_id);
        c.state = ConnState::LoggedOn;
        // Whatever happened, the password is not needed any more.
        c.password = None;
        self.push(ProviderMsg::LoggedOn { conn_id, session_id });
    }

    fn by_session(&self, session_id: u32) -> Option<u64> {
        self.conns.iter().find(|(_, c)| c.session_id == Some(session_id)).map(|(id, _)| *id)
    }

    /// `OnDriverLoad`; the calling connection is known, the session id comes from TermService.
    pub fn driver_loaded(&mut self, conn_id: u64, session_id: u32, device_instance: Option<String>) {
        let conn_id = if self.conns.contains_key(&conn_id) { conn_id } else { self.by_session(session_id).unwrap_or(conn_id) };
        let device_instance = device_instance.filter(|d| !d.is_empty() && d.len() <= 512 && !d.chars().any(char::is_control));
        self.push(ProviderMsg::DriverLoaded { conn_id, session_id, device_instance });
    }

    pub fn driver_unloaded(&mut self, conn_id: u64, session_id: u32) {
        self.push(ProviderMsg::DriverUnloaded { conn_id, session_id });
    }

    pub fn logon_error(&mut self, conn_id: u64, ntstatus: i32, ntsubstatus: i32, message: &str) {
        let message: String = message.chars().filter(|c| !c.is_control()).take(MAX_MESSAGE).collect();
        self.push(ProviderMsg::LogonError { conn_id, ntstatus, ntsubstatus, message });
    }

    pub fn disconnected(&mut self, conn_id: u64, reason: u32) {
        let Some(c) = self.conns.get_mut(&conn_id) else { return };
        if c.state == ConnState::Disconnected {
            return;
        }
        c.state = ConnState::Disconnected;
        c.password = None;
        self.push(ProviderMsg::Disconnected { conn_id, reason });
    }

    /// `Close`: the connection object is released; its data (and any password) is dropped.
    pub fn closed(&mut self, conn_id: u64) {
        if self.conns.remove(&conn_id).is_some() {
            self.push(ProviderMsg::Closed { conn_id });
        }
    }

    pub fn connection_ids(&self) -> Vec<u64> {
        self.conns.keys().copied().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(id: u64) -> ConnectRequest {
        ConnectRequest {
            conn_id: id,
            seat_id: "s1".into(),
            client_name: "PSS00112233".into(),
            user: "seat2".into(),
            domain: ".".into(),
            password: Secret::new("pw"),
            width: 2560,
            height: 1440,
            keyboard_layout: 0x041f,
        }
    }

    fn ready() -> Core {
        let mut c = Core::new();
        c.hello(10, "0.1.0");
        c.on_service(ServiceMsg::Welcome { protocol: PROTOCOL });
        c.set_listening(true);
        c.drain();
        c
    }

    #[test]
    fn full_sequence() {
        let mut c = ready();
        assert_eq!(c.on_service(ServiceMsg::Connect(req(1))), Action::Offer(1));
        assert!(c.accepted(1));
        let info = c.client_info(1).unwrap();
        assert_eq!((info.width, info.height, info.keyboard_layout), (2560, 1440, 0x041f));
        let cred = c.take_credentials(1).unwrap();
        assert_eq!(cred.password.expose(), "pw");
        assert!(c.take_credentials(1).is_none(), "password handed out once");
        assert!(c.allowed_to_logon(1, "PC", "SEAT2", "pc"));
        assert!(!c.allowed_to_logon(1, ".", "admin", "pc"));
        c.session_id(1, 5);
        c.driver_loaded(1, 5, Some(r"SWD\REMOTEDISPLAYENUM\X".into()));
        c.logged_on(1, 5);
        assert_eq!(c.state(1), Some(ConnState::LoggedOn));
        c.disconnected(1, 0);
        c.disconnected(1, 0);
        c.closed(1);
        assert_eq!(c.state(1), None);
        let out = c.drain();
        assert_eq!(
            out,
            vec![
                ProviderMsg::Accepted { conn_id: 1 },
                ProviderMsg::SessionId { conn_id: 1, session_id: 5 },
                ProviderMsg::DriverLoaded { conn_id: 1, session_id: 5, device_instance: Some(r"SWD\REMOTEDISPLAYENUM\X".into()) },
                ProviderMsg::LoggedOn { conn_id: 1, session_id: 5 },
                ProviderMsg::Disconnected { conn_id: 1, reason: 0 },
                ProviderMsg::Closed { conn_id: 1 },
            ]
        );
    }

    #[test]
    fn refusals() {
        let mut c = Core::new();
        assert_eq!(c.on_service(ServiceMsg::Connect(req(1))), Action::None);
        assert!(matches!(&c.drain()[..], [ProviderMsg::Refused { reason, .. }] if reason == "not_welcomed"));
        c.on_service(ServiceMsg::Welcome { protocol: PROTOCOL });
        assert_eq!(c.on_service(ServiceMsg::Connect(req(1))), Action::None);
        assert!(matches!(&c.drain()[..], [ProviderMsg::Refused { reason, .. }] if reason == "not_listening"));
        c.set_listening(true);
        c.drain();
        let mut bad = req(2);
        bad.width = 100;
        assert_eq!(c.on_service(ServiceMsg::Connect(bad)), Action::None);
        assert!(matches!(&c.drain()[..], [ProviderMsg::Refused { reason, .. }] if reason.starts_with("invalid")));
        assert_eq!(c.on_service(ServiceMsg::Connect(req(3))), Action::Offer(3));
        assert_eq!(c.on_service(ServiceMsg::Connect(req(3))), Action::None);
        assert!(matches!(&c.drain()[..], [ProviderMsg::Refused { reason, .. }] if reason == "duplicate"));
        // Another major version is never welcomed.
        let mut d = Core::new();
        d.on_service(ServiceMsg::Welcome { protocol: PROTOCOL + 1 });
        assert!(!d.welcomed());
    }

    #[test]
    fn disconnect_only_known_connections() {
        let mut c = ready();
        assert_eq!(c.on_service(ServiceMsg::Disconnect { conn_id: 9 }), Action::None);
        c.on_service(ServiceMsg::Connect(req(9)));
        assert_eq!(c.on_service(ServiceMsg::Disconnect { conn_id: 9 }), Action::Break(9));
        c.offer_failed(9, "on_connected_failed");
        assert_eq!(c.state(9), None);
    }

    #[test]
    fn logged_on_and_disconnect_wipe_the_password() {
        let mut c = ready();
        c.on_service(ServiceMsg::Connect(req(1)));
        c.logged_on(1, 3);
        assert!(c.take_credentials(1).is_none());
        c.on_service(ServiceMsg::Connect(req(2)));
        c.disconnected(2, 7);
        assert!(c.take_credentials(2).is_none());
    }

    #[test]
    fn driver_events_find_the_connection_by_session() {
        let mut c = ready();
        c.on_service(ServiceMsg::Connect(req(4)));
        c.session_id(4, 8);
        c.drain();
        c.driver_loaded(99, 8, Some("bad\nid".into()));
        assert_eq!(c.drain(), vec![ProviderMsg::DriverLoaded { conn_id: 4, session_id: 8, device_instance: None }]);
    }

    #[test]
    fn outbox_and_messages_are_bounded() {
        let mut c = ready();
        for i in 0..(MAX_OUTBOX as u64 + 10) {
            c.driver_unloaded(i, 1);
        }
        assert_eq!(c.drain().len(), MAX_OUTBOX);
        c.logon_error(1, -1, 0, &"x\n".repeat(2000));
        match &c.drain()[0] {
            ProviderMsg::LogonError { message, .. } => assert_eq!(message.len(), MAX_MESSAGE),
            m => panic!("{m:?}"),
        }
        c.set_listening(true);
        assert!(c.drain().is_empty(), "no message without a change");
    }
}
