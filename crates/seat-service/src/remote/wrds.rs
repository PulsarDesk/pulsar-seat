// SPDX-License-Identifier: GPL-3.0-only
//! Remote seats on our own display: the service side of the Remote Desktop protocol
//! provider (`crates/seat-wrds`, pipe contract `seat_proto::wrds`). Added 2026-11-17
//! (package remote-idd-hdr).
//!
//! - [`plan`] decides per seat start how the seat session is created: through our provider
//!   (display = our IddCx remote role, `pulsar_idd`) or through the loopback RDP client
//!   (display = the stock RDP display, `rdp_session`). The provider path is taken when the
//!   operator chose `pulsar_idd` (or automatic with a signed own driver) and the provider
//!   is registered, loaded by TermService and listening; otherwise the seat falls back to
//!   loopback + `rdp_session` and the manager is told why (`display_fallback` notice).
//! - [`Hub`] is the pure state of the pipe: provider hello, outstanding connects, events.
//!   The Windows pipe server (`crate::remote::win::wrds`) feeds it; the engine talks to it
//!   through [`WrdsPort`].
//!
//! HYPOTHESIS (needs Windows Server + RD Session Host; nothing ran): the provider callback
//! order, that the seat session's display device appears right after `LoggedOn`, and that
//! `BrokenConnection` ends the session's connection cleanly.

use seat_display::remote::{BackendInfo, BackendKind, choose_backend};
use seat_proto::remote::{Secret, StopAction};
use seat_proto::wrds::{ConnectRequest, PROTOCOL, ProviderMsg, ServiceMsg};
use seat_session::remote::manager::CancelToken;
use seat_session::wrds::WrdsStatus;
use std::collections::{HashMap, VecDeque};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// How long a provider connect may take until the seat user is logged on.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(90);
/// Lost-connection reports kept for the engine (oldest dropped first).
const MAX_LOST: usize = 64;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// What one seat start asks the provider for (the password travels separately).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrdsConnect {
    pub seat_id: String,
    pub client_name: String,
    pub user: String,
    pub width: u32,
    pub height: u32,
    /// The seat streams sound (`audio.stream_audio`).
    pub audio: bool,
}

/// A seat session created through the provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrdsSession {
    pub conn_id: u64,
    pub session_id: u32,
    /// Device instance of the seat's display (from `IWRdsWddmIddProps1::OnDriverLoad`).
    pub device_instance: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrdsFailure {
    pub reason: String,
    pub message: String,
}

impl WrdsFailure {
    pub fn new(reason: &str, message: impl std::fmt::Display) -> Self {
        Self { reason: reason.into(), message: format!("{reason}: {message}") }
    }
}

/// What the engine needs from the provider path. Real: `crate::remote::win::wrds::WinWrds`.
pub trait WrdsPort: Send + Sync {
    fn status(&self) -> WrdsStatus;
    /// Blocks until the seat user is logged on in a new session (or failure / cancel /
    /// [`CONNECT_TIMEOUT`]).
    fn connect(&self, req: &WrdsConnect, password: &Secret, cancel: &CancelToken) -> Result<WrdsSession, WrdsFailure>;
    /// End a provider seat session: log off (or disconnect) and drop the connection.
    fn end(&self, s: &WrdsSession, action: StopAction) -> Result<(), String>;
    /// Connections that ended without the engine asking: `(conn_id, reason)`.
    fn take_lost(&self) -> Vec<(u64, String)>;
    /// Register (or remove) the provider for these seat accounts (user names).
    fn sync_registration(&self, want: bool, seat_users: &[String]) -> Result<WrdsStatus, String>;
}

/// How a seat session is created and which display backend it gets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostPlan {
    /// Through our provider; display `pulsar_idd`.
    Provider,
    /// Through the loopback RDP client. `fallback` = why the provider path was wanted but
    /// not taken.
    Loopback { display: BackendKind, fallback: Option<String> },
}

/// The provider path is wanted: chosen explicitly, or automatic with a signed own driver.
pub fn wants_provider(pref: Option<BackendKind>, signed: bool) -> bool {
    match pref {
        Some(BackendKind::PulsarIdd) => true,
        Some(_) => false,
        None => signed,
    }
}

/// Decide the host path of one seat start.
pub fn plan(pref: Option<BackendKind>, infos: &[BackendInfo], console: bool, signed: bool, wrds: Option<&WrdsStatus>) -> HostPlan {
    if !wants_provider(pref, signed) {
        return HostPlan::Loopback { display: choose_backend(pref, infos, console), fallback: None };
    }
    let reason = match wrds {
        None => Some("wrds_unavailable".to_string()),
        Some(s) => s.reason.clone(),
    };
    match reason {
        None => HostPlan::Provider,
        Some(r) => HostPlan::Loopback { display: BackendKind::RdpSession, fallback: Some(r) },
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Pending {
    session_id: Option<u32>,
    logged_on: bool,
    device_instance: Option<String>,
    /// Terminal outcome before logon (refused, disconnected, closed).
    failed: Option<String>,
    last_logon_error: Option<String>,
    /// The engine owns the connection (connect returned it).
    running: bool,
    /// The engine asked for the end; no "lost" report then.
    ending: bool,
}

#[derive(Debug, Default)]
struct HubState {
    provider: Option<(u32, String)>,
    listening: bool,
    next_id: u64,
    conns: HashMap<u64, Pending>,
    outgoing: VecDeque<ServiceMsg>,
    lost: VecDeque<(u64, String)>,
    /// Pid of the provider whose pipe broke: a hello from the same process keeps its
    /// connections, one from a new process (TermService restarted) loses them.
    gone_pid: Option<u32>,
}

/// Pure pipe state shared by the pipe thread and seat workers.
#[derive(Debug, Default)]
pub struct Hub {
    st: Mutex<HubState>,
    cv: Condvar,
}

impl Hub {
    pub fn new() -> Self {
        Self::default()
    }

    fn with<R>(&self, f: impl FnOnce(&mut HubState) -> R) -> R {
        let r = f(&mut lock(&self.st));
        self.cv.notify_all();
        r
    }

    /// Provider said hello: answer `welcome` (or `None` for another major version).
    pub fn provider_hello(&self, m: &ProviderMsg) -> Option<ServiceMsg> {
        let ProviderMsg::Hello { protocol, provider_version, pid, listening } = m else { return None };
        if *protocol != PROTOCOL {
            return None;
        }
        self.with(|s| {
            if let Some(old) = s.gone_pid.take()
                && old != *pid
            {
                // A new provider process (TermService restarted): it knows none of the old
                // connections, so it will never report them closed. They are lost now, and
                // the kept `disconnect` frames mean nothing to it.
                let ids: Vec<u64> = s.conns.iter().filter(|(_, p)| p.running).map(|(id, _)| *id).collect();
                for id in ids {
                    if s.conns.remove(&id).is_some_and(|p| !p.ending) {
                        s.lost.push_back((id, "provider_restarted".into()));
                    }
                }
                s.outgoing.retain(|m| !matches!(m, ServiceMsg::Disconnect { .. }));
            }
            s.provider = Some((*pid, provider_version.clone()));
            s.listening = *listening;
        });
        Some(ServiceMsg::Welcome { protocol: PROTOCOL })
    }

    /// The pipe broke: every connect still waiting fails. Running sessions stay (their
    /// Windows sessions live on): when the same provider process comes back it still has
    /// them and reports their end; when a new process comes back (TermService restarted)
    /// [`Self::provider_hello`] reports them lost. Queued `disconnect` frames are kept for
    /// the same process (review 2026-11-17).
    pub fn provider_gone(&self) {
        self.with(|s| {
            s.gone_pid = s.provider.take().map(|p| p.0).or(s.gone_pid);
            s.listening = false;
            s.outgoing.retain(|m| matches!(m, ServiceMsg::Disconnect { .. }));
            for p in s.conns.values_mut().filter(|p| !p.running && p.failed.is_none()) {
                p.failed = Some("provider_gone".into());
            }
        });
    }

    pub fn connected(&self) -> bool {
        lock(&self.st).provider.is_some()
    }

    pub fn listening(&self) -> bool {
        let s = lock(&self.st);
        s.provider.is_some() && s.listening
    }

    /// Frames for the provider (pipe thread).
    pub fn take_outgoing(&self) -> Vec<ServiceMsg> {
        lock(&self.st).outgoing.drain(..).collect()
    }

    /// Wait up to `t` for something to send (pipe thread).
    pub fn wait_outgoing(&self, t: Duration) {
        let g = lock(&self.st);
        if g.outgoing.is_empty() {
            let _ = self.cv.wait_timeout(g, t);
        }
    }

    pub fn on_provider(&self, m: ProviderMsg) {
        self.with(|s| {
            let fail = |s: &mut HubState, id: u64, why: String| {
                let Some(p) = s.conns.get_mut(&id) else { return };
                if p.running {
                    if !p.ending {
                        p.ending = true;
                        if s.lost.len() >= MAX_LOST {
                            s.lost.pop_front();
                        }
                        s.lost.push_back((id, why));
                    }
                } else if p.failed.is_none() {
                    p.failed = Some(why);
                }
            };
            match m {
                ProviderMsg::Hello { listening, .. } => s.listening = listening,
                ProviderMsg::Listening { listening } => s.listening = listening,
                ProviderMsg::Accepted { .. } => {}
                ProviderMsg::SessionId { conn_id, session_id } => {
                    if let Some(p) = s.conns.get_mut(&conn_id) {
                        p.session_id = Some(session_id);
                    }
                }
                ProviderMsg::LoggedOn { conn_id, session_id } => {
                    if let Some(p) = s.conns.get_mut(&conn_id) {
                        p.session_id = Some(session_id);
                        p.logged_on = true;
                    }
                }
                ProviderMsg::DriverLoaded { conn_id, device_instance, .. } => {
                    if let Some(p) = s.conns.get_mut(&conn_id) {
                        p.device_instance = device_instance;
                    }
                }
                ProviderMsg::DriverUnloaded { .. } => {}
                ProviderMsg::LogonError { conn_id, ntstatus, ntsubstatus, message } => {
                    if let Some(p) = s.conns.get_mut(&conn_id) {
                        p.last_logon_error = Some(format!("{ntstatus:#010x}/{ntsubstatus:#010x} {message}"));
                    }
                }
                ProviderMsg::Refused { conn_id, reason } => fail(s, conn_id, format!("refused: {reason}")),
                ProviderMsg::Disconnected { conn_id, reason } => fail(s, conn_id, format!("disconnected ({reason:#x})")),
                ProviderMsg::Closed { conn_id } => {
                    fail(s, conn_id, "closed".into());
                    if s.conns.get(&conn_id).is_some_and(|p| p.running) {
                        s.conns.remove(&conn_id);
                    }
                }
            }
        });
    }

    /// Send a connect and wait for the logon. `timeout` normally [`CONNECT_TIMEOUT`].
    pub fn connect(
        &self,
        req: &WrdsConnect,
        password: &Secret,
        cancel: &CancelToken,
        timeout: Duration,
    ) -> Result<WrdsSession, WrdsFailure> {
        let msg = ConnectRequest {
            conn_id: 0,
            seat_id: req.seat_id.clone(),
            client_name: req.client_name.clone(),
            user: req.user.clone(),
            domain: ".".into(),
            password: password.clone(),
            width: req.width.clamp(seat_proto::wrds::MIN_SIZE, seat_proto::wrds::MAX_SIZE),
            height: req.height.clamp(seat_proto::wrds::MIN_SIZE, seat_proto::wrds::MAX_SIZE),
            keyboard_layout: 0,
            audio: req.audio,
        };
        let id = {
            let mut s = lock(&self.st);
            if s.provider.is_none() {
                return Err(WrdsFailure::new("wrds_restart_needed", "the protocol provider is not connected"));
            }
            if !s.listening {
                return Err(WrdsFailure::new("wrds_not_listening", "TermService has not started the provider's listener"));
            }
            s.next_id += 1;
            let id = s.next_id;
            let mut msg = msg;
            msg.conn_id = id;
            msg.validate().map_err(|e| WrdsFailure::new("invalid_value", e))?;
            s.conns.insert(id, Pending::default());
            s.outgoing.push_back(ServiceMsg::Connect(msg));
            id
        };
        self.cv.notify_all();
        let end = Instant::now() + timeout;
        let mut g = lock(&self.st);
        loop {
            let p = g.conns.get(&id).cloned().unwrap_or_else(|| Pending { failed: Some("closed".into()), ..Default::default() });
            if p.logged_on
                && let Some(session_id) = p.session_id
            {
                if let Some(c) = g.conns.get_mut(&id) {
                    c.running = true;
                }
                return Ok(WrdsSession { conn_id: id, session_id, device_instance: p.device_instance });
            }
            let why = if let Some(f) = p.failed {
                Some(WrdsFailure::new(
                    "wrds_connect_failed",
                    match &p.last_logon_error {
                        Some(l) => format!("{f}; logon error {l}"),
                        None => f,
                    },
                ))
            } else if cancel.is_cancelled() {
                Some(WrdsFailure::new("cancelled", "seat stop requested during start"))
            } else if Instant::now() >= end {
                Some(WrdsFailure::new("wrds_timeout", format!("no logon through the protocol provider within {timeout:?}")))
            } else {
                None
            };
            if let Some(f) = why {
                g.conns.remove(&id);
                g.outgoing.push_back(ServiceMsg::Disconnect { conn_id: id });
                drop(g);
                self.cv.notify_all();
                return Err(f);
            }
            g = self.cv.wait_timeout(g, Duration::from_millis(100)).map(|(g, _)| g).unwrap_or_else(|e| e.into_inner().0);
        }
    }

    /// The engine ends a running connection: ask the provider to break it; no lost report.
    pub fn disconnect(&self, conn_id: u64) {
        self.with(|s| {
            if let Some(p) = s.conns.get_mut(&conn_id) {
                p.ending = true;
            }
            s.conns.remove(&conn_id);
            // Sent now, or after the provider's pipe comes back (same process).
            if s.provider.is_some() || s.gone_pid.is_some() {
                s.outgoing.push_back(ServiceMsg::Disconnect { conn_id });
            }
        });
    }

    pub fn take_lost(&self) -> Vec<(u64, String)> {
        lock(&self.st).lost.drain(..).collect()
    }

    /// `(pid, version)` of the provider when connected.
    pub fn provider(&self) -> Option<(u32, String)> {
        lock(&self.st).provider.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use seat_display::remote::HdrUnavailable;
    use seat_session::wrds::Registration;
    use std::sync::Arc;

    fn hello() -> ProviderMsg {
        ProviderMsg::Hello { protocol: PROTOCOL, provider_version: "0.1.0".into(), pid: 4, listening: true }
    }

    fn req() -> WrdsConnect {
        WrdsConnect { seat_id: "s1".into(), client_name: "PSS00112233".into(), user: "seat2".into(), width: 1920, height: 1080, audio: true }
    }

    fn usable() -> WrdsStatus {
        WrdsStatus::evaluate(Registration::Ours, true, true, false, true)
    }

    #[test]
    fn plan_rules() {
        let infos: [BackendInfo; 0] = [];
        // Not wanted: today's behaviour.
        assert_eq!(plan(None, &infos, false, false, Some(&usable())), HostPlan::Loopback { display: BackendKind::RdpSession, fallback: None });
        assert_eq!(plan(Some(BackendKind::Vdd), &infos, true, true, None), HostPlan::Loopback { display: BackendKind::Vdd, fallback: None });
        // Wanted and usable.
        assert_eq!(plan(Some(BackendKind::PulsarIdd), &infos, false, false, Some(&usable())), HostPlan::Provider);
        assert_eq!(plan(None, &infos, false, true, Some(&usable())), HostPlan::Provider);
        // Wanted, not usable: loopback + rdp_session with the reason.
        let restart = WrdsStatus::evaluate(Registration::Ours, false, false, false, true);
        assert_eq!(
            plan(Some(BackendKind::PulsarIdd), &infos, false, false, Some(&restart)),
            HostPlan::Loopback { display: BackendKind::RdpSession, fallback: Some("wrds_restart_needed".into()) }
        );
        assert_eq!(
            plan(Some(BackendKind::PulsarIdd), &infos, false, false, None),
            HostPlan::Loopback { display: BackendKind::RdpSession, fallback: Some("wrds_unavailable".into()) }
        );
        let _ = HdrUnavailable::ForceSdr;
    }

    /// A provider stand-in on another thread answering the hub's frames.
    fn provider(hub: Arc<Hub>, answer: impl Fn(u64) -> Vec<ProviderMsg> + Send + 'static) -> std::thread::JoinHandle<Vec<ServiceMsg>> {
        std::thread::spawn(move || {
            let mut seen = Vec::new();
            let end = Instant::now() + Duration::from_secs(5);
            while Instant::now() < end {
                hub.wait_outgoing(Duration::from_millis(20));
                for m in hub.take_outgoing() {
                    if let ServiceMsg::Connect(c) = &m {
                        for a in answer(c.conn_id) {
                            hub.on_provider(a);
                        }
                    }
                    let done = matches!(m, ServiceMsg::Disconnect { .. });
                    seen.push(m);
                    if done {
                        return seen;
                    }
                }
            }
            seen
        })
    }

    #[test]
    fn connect_logs_on_and_reports_the_session() {
        let hub = Arc::new(Hub::new());
        assert!(hub.provider_hello(&hello()).is_some());
        let t = provider(hub.clone(), |id| {
            vec![
                ProviderMsg::Accepted { conn_id: id },
                ProviderMsg::SessionId { conn_id: id, session_id: 6 },
                ProviderMsg::DriverLoaded { conn_id: id, session_id: 6, device_instance: Some("SWD\\X".into()) },
                ProviderMsg::LoggedOn { conn_id: id, session_id: 6 },
            ]
        });
        let s = hub.connect(&req(), &Secret::new("pw"), &CancelToken::new(), Duration::from_secs(5)).unwrap();
        assert_eq!((s.session_id, s.device_instance.as_deref()), (6, Some("SWD\\X")));
        // A later disconnect the engine did not ask for is reported once.
        hub.on_provider(ProviderMsg::Disconnected { conn_id: s.conn_id, reason: 3 });
        hub.on_provider(ProviderMsg::Closed { conn_id: s.conn_id });
        assert_eq!(hub.take_lost(), vec![(s.conn_id, "disconnected (0x3)".to_string())]);
        hub.disconnect(s.conn_id);
        let seen = t.join().unwrap();
        assert!(matches!(&seen[0], ServiceMsg::Connect(c) if c.width == 1920 && c.domain == "."));
        assert!(matches!(seen.last(), Some(ServiceMsg::Disconnect { .. })));
    }

    #[test]
    fn refusal_and_logon_error_fail_the_connect() {
        let hub = Arc::new(Hub::new());
        hub.provider_hello(&hello());
        let t = provider(hub.clone(), |id| {
            vec![
                ProviderMsg::LogonError { conn_id: id, ntstatus: -1073741715, ntsubstatus: 0, message: "bad password".into() },
                ProviderMsg::Disconnected { conn_id: id, reason: 0 },
            ]
        });
        let e = hub.connect(&req(), &Secret::new("pw"), &CancelToken::new(), Duration::from_secs(5)).unwrap_err();
        assert_eq!(e.reason, "wrds_connect_failed");
        assert!(e.message.contains("bad password"), "{}", e.message);
        // The hub tells the provider to drop it.
        assert!(matches!(t.join().unwrap().last(), Some(ServiceMsg::Disconnect { .. })));
        assert!(hub.take_lost().is_empty());
    }

    #[test]
    fn no_provider_timeout_and_cancel() {
        let hub = Hub::new();
        let e = hub.connect(&req(), &Secret::new("pw"), &CancelToken::new(), Duration::from_millis(10)).unwrap_err();
        assert_eq!(e.reason, "wrds_restart_needed");
        hub.provider_hello(&ProviderMsg::Hello { protocol: PROTOCOL, provider_version: "x".into(), pid: 1, listening: false });
        assert_eq!(hub.connect(&req(), &Secret::new("pw"), &CancelToken::new(), Duration::ZERO).unwrap_err().reason, "wrds_not_listening");
        hub.on_provider(ProviderMsg::Listening { listening: true });
        assert_eq!(hub.connect(&req(), &Secret::new("pw"), &CancelToken::new(), Duration::from_millis(50)).unwrap_err().reason, "wrds_timeout");
        let c = CancelToken::new();
        c.cancel();
        assert_eq!(hub.connect(&req(), &Secret::new("pw"), &c, Duration::from_secs(5)).unwrap_err().reason, "cancelled");
        // Another major version is not welcomed.
        assert!(hub.provider_hello(&ProviderMsg::Hello { protocol: 9, provider_version: "x".into(), pid: 1, listening: true }).is_none());
    }

    #[test]
    fn provider_gone_fails_waiting_connects() {
        let hub = Arc::new(Hub::new());
        hub.provider_hello(&hello());
        let h2 = hub.clone();
        let t = std::thread::spawn(move || h2.connect(&req(), &Secret::new("pw"), &CancelToken::new(), Duration::from_secs(5)));
        while hub.take_outgoing().is_empty() {
            std::thread::sleep(Duration::from_millis(5));
        }
        hub.provider_gone();
        let e = t.join().unwrap().unwrap_err();
        assert!(e.message.contains("provider_gone"), "{}", e.message);
        assert!(!hub.connected());
    }

    /// Logged-on connection `id` (session 6) without a provider thread.
    fn running(hub: &Hub) -> u64 {
        let mut s = lock(&hub.st);
        s.next_id += 1;
        let id = s.next_id;
        s.conns.insert(id, Pending { session_id: Some(6), logged_on: true, running: true, ..Default::default() });
        id
    }

    #[test]
    fn same_provider_process_keeps_connections_and_queued_disconnects() {
        let hub = Hub::new();
        hub.provider_hello(&hello());
        let a = running(&hub);
        let b = running(&hub);
        hub.provider_gone();
        hub.disconnect(a);
        hub.provider_hello(&hello());
        assert_eq!(hub.take_outgoing(), vec![ServiceMsg::Disconnect { conn_id: a }], "kept across the break");
        assert!(hub.take_lost().is_empty(), "the same process still has them");
        hub.on_provider(ProviderMsg::Closed { conn_id: b });
        assert_eq!(hub.take_lost().len(), 1);
    }

    #[test]
    fn new_provider_process_loses_the_old_connections() {
        let hub = Hub::new();
        hub.provider_hello(&hello());
        let a = running(&hub);
        let b = running(&hub);
        hub.provider_gone();
        hub.disconnect(a);
        let restarted = ProviderMsg::Hello { protocol: PROTOCOL, provider_version: "0.1.0".into(), pid: 99, listening: true };
        hub.provider_hello(&restarted);
        assert_eq!(hub.take_lost(), vec![(b, "provider_restarted".to_string())], "only the one the engine did not end");
        assert!(hub.take_outgoing().is_empty(), "old disconnects mean nothing to the new process");
    }
}
