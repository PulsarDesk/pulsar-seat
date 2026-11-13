// SPDX-License-Identifier: GPL-3.0-only
//! `SessionManager`: the API the service uses for seat sessions (contracts §1.1, §1.3).
//!
//! [`Manager`] holds the whole logic over trait objects, so it runs against fakes in tests.
//! `WindowsSessionManager` (Windows only) wires the real implementations.

use crate::discover::{Wts, find_seat_session_preferring};
use crate::edition::{EditionInfo, EditionProbe, HostClass, classify};
use crate::helper_proto::{HELPER_PROTOCOL, HelperArgs, HelperEvent, HelperFailure, MAX_SIZE, MIN_SIZE};
use crate::journal::{JournalValue, RecordKind, RecordStatus, RestoreLog, RestoreRecord};
use crate::launch::SessionLauncher;
use crate::rds::{ApplyReport, LicensingInfo, ListenerInfo, RdsApplier, RdsProbe, RdsWarning, Registry, RestoreReport, termservice_is_stock};
use crate::sweep::{OwnedSessionRecord, ProcessTable, RunState, SweepPlan, plan_sweep};
use crate::users::{Accounts, Profiles, UserCheck, check_password};
use crate::wait::poll_until;
use crate::{Clock, FailureReason, SessionError, Sleeper};
use serde::{Deserialize, Serialize};
use split_proto::{Secret, StopAction};
use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use zeroize::{Zeroize, Zeroizing};

/// Default keyboard layout id sent to the server. The session keeps the user's own layout
/// anyway because we write `IgnoreRemoteKeyboardLayout` = 1 (spec 30 §2.3).
pub const DEFAULT_LAYOUT: u32 = 0x0000_0409;
const POLL_STEP: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionTimeouts {
    /// Session must appear after the client starts (spec 30 §7: 60 s).
    pub session_appear: Duration,
    /// Logoff must finish (spec 30 §9.1: 30 s).
    pub logoff_wait: Duration,
    /// RDS readiness (spec 30 §3: 120 s).
    pub rds_ready: Duration,
    /// Client restart window (spec 30 §9.1: 3 failures in 5 min).
    pub client_restart_window: Duration,
    pub client_restart_max: u32,
}

impl Default for SessionTimeouts {
    fn default() -> Self {
        Self {
            session_appear: Duration::from_secs(60),
            logoff_wait: Duration::from_secs(30),
            rds_ready: Duration::from_secs(120),
            client_restart_window: Duration::from_secs(300),
            client_restart_max: 3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionRequest<'a> {
    pub seat_id: &'a str,
    /// The seat's generated `rdp_client_name` (`client_name_for`).
    pub client_name: &'a str,
    pub user: &'a str,
    pub width: u32,
    pub height: u32,
    pub keyboard_layout: Option<u32>,
    pub audio_capture: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatSession {
    pub seat_id: String,
    pub session_id: u32,
    pub user_sid: String,
    pub helper_pid: u32,
    pub client_name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", content = "reason", rename_all = "snake_case")]
pub enum SessionPhase {
    Connecting,
    Authenticated,
    WaitingForSession,
    Ready,
    Reconnecting,
    Stopping,
    Failed(FailureReason),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HostWarning {
    RdshMissing,
    LicensingNotConfigured,
    GraceEnds { days: u32 },
    PolicyOverride { value: String },
    SeatUserIsAdmin { user: String },
    ForeignWrapper { service_dll: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostInfo {
    pub edition: EditionInfo,
    pub class: HostClass,
    pub listener: Option<ListenerInfo>,
    pub licensing: Option<LicensingInfo>,
    pub termservice_stock: bool,
    pub service_dll: String,
    pub rds_ready: bool,
    /// Seats this host can run at once; `None` = no limit known.
    pub max_seats: Option<u32>,
    pub warnings: Vec<HostWarning>,
}

/// Cancels a blocking [`SessionManager::start_session`].
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// `SERVICE_CONTROL_SESSIONCHANGE` events (`WTS_*` codes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionChange {
    Logon(u32),
    Logoff(u32),
    RemoteConnect(u32),
    RemoteDisconnect(u32),
    ConsoleConnect(u32),
    /// (event type, session id) for every other code.
    Other(u32, u32),
}

impl SessionChange {
    /// From the `dwEventType` of `SERVICE_CONTROL_SESSIONCHANGE` and the session id.
    pub fn from_raw(event: u32, session_id: u32) -> Self {
        match event {
            0x1 => Self::ConsoleConnect(session_id),
            0x3 => Self::RemoteConnect(session_id),
            0x4 => Self::RemoteDisconnect(session_id),
            0x5 => Self::Logon(session_id),
            0x6 => Self::Logoff(session_id),
            e => Self::Other(e, session_id),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "notice", rename_all = "snake_case")]
pub enum SessionNotice {
    Reconnected { seat_id: String, session_id: u32 },
    Failed { seat_id: String, reason: FailureReason },
    SessionGone { seat_id: String },
}

pub trait SessionManager: Send + Sync {
    fn host_info(&self, wrapper_active: bool) -> Result<HostInfo, SessionError>;
    /// Refuses Home (`edition_unsupported`) and foreign ServiceDll (`coexistence`) before any write.
    fn prepare_host(&self, include_wrapper_rows: bool) -> Result<ApplyReport, SessionError>;
    fn restore_host(&self) -> Result<RestoreReport, SessionError>;
    fn wait_rds_ready(&self) -> Result<(), SessionError>;
    /// Facts, RDU membership, profile.
    fn ensure_seat_user(&self, user: &str, password: &Secret) -> Result<UserCheck, SessionError>;
    fn create_seat_user(&self, user: &str, password: &Secret) -> Result<(), SessionError>;
    /// Blocking up to `session_appear`; spawns the helper (child of the service, password on
    /// its stdin pipe), waits for `Connected`, discovers the session, records RunState.
    fn start_session(&self, req: &SessionRequest<'_>, password: &Secret, cancel: &CancelToken) -> Result<SeatSession, SessionError>;
    fn phase(&self, seat_id: &str) -> Option<SessionPhase>;
    /// Ends the helper; `StopAction::LogOff` logs the session off and waits for it to vanish.
    fn stop_session(&self, seat_id: &str, action: StopAction) -> Result<(), SessionError>;
    /// Restarts the helper after an unexpected exit (3 per 5 min, then Failed(client_exited)).
    fn tick(&self, now_ms: u64) -> Vec<SessionNotice>;
    fn on_session_change(&self, ev: SessionChange);
    /// Plans + executes the start/stop sweep.
    fn sweep(&self, seat_users: &[String]) -> Result<SweepPlan, SessionError>;
    fn launcher(&self) -> &dyn SessionLauncher;
}

// ---- helper process abstraction ----

/// A running helper child process.
pub trait HelperChild: Send {
    fn pid(&self) -> u32;
    fn creation_time(&self) -> u64;
    /// Next stdout event, waiting at most `timeout` (zero = no wait). `None` on timeout or
    /// once stdout is closed.
    fn next_event(&mut self, timeout: Duration) -> Option<HelperEvent>;
    /// Exit code once the process has ended.
    fn try_wait(&mut self) -> Option<i32>;
    fn kill(&mut self);
}

/// Starts the helper with `args` on argv and the password on its stdin pipe only.
pub trait HelperSpawner: Send + Sync {
    fn spawn(&self, args: &HelperArgs, password: &Secret) -> Result<Box<dyn HelperChild>, SessionError>;
}

/// Everything the manager talks to.
pub struct Deps {
    pub edition: Box<dyn EditionProbe>,
    pub rds: Box<dyn RdsProbe>,
    pub registry: Arc<dyn Registry>,
    pub journal: Arc<dyn RestoreLog>,
    pub accounts: Box<dyn Accounts>,
    pub profiles: Box<dyn Profiles>,
    pub wts: Box<dyn Wts>,
    pub launcher: Box<dyn SessionLauncher>,
    pub run_state: Box<dyn RunState>,
    pub processes: Box<dyn ProcessTable>,
    pub helper: Box<dyn HelperSpawner>,
    pub clock: Arc<dyn Clock>,
    pub sleeper: Arc<dyn Sleeper>,
}

struct SeatSlot {
    /// Per start attempt; every later write checks it, so a stale attempt never touches a
    /// newer attempt's slot or session.
    generation: u64,
    phase: SessionPhase,
    client_name: String,
    user: String,
    user_sid: String,
    width: u32,
    height: u32,
    layout: u32,
    audio_capture: bool,
    /// Kept for helper restarts only; wiped on drop.
    password: Zeroizing<String>,
    child: Option<Box<dyn HelperChild>>,
    session_id: Option<u32>,
    /// Session id the helper reported during a reconnect.
    reported_session: Option<u32>,
    reconnect_connected: bool,
    reconnect_deadline_ms: u64,
    failures: VecDeque<u64>,
}

impl SeatSlot {
    fn is_live(&self) -> bool {
        !matches!(self.phase, SessionPhase::Failed(_))
    }
}

/// Blocking work `tick` decided on under the seats lock and runs without it.
enum TickAction {
    /// Kill the old helper and log off the seat session (phase already `Failed`).
    Fail { old: Option<Box<dyn HelperChild>>, session: Option<u32>, user: String },
    /// Kill the old helper and start a new one (phase already `Reconnecting`, no child).
    Restart { seat_id: String, generation: u64, old: Option<Box<dyn HelperChild>>, meta: SeatSlot, password: Zeroizing<String> },
    /// The restarted helper is connected: find its session.
    Discover { seat_id: String, generation: u64, client_name: String, user: String, reported: Option<u32> },
}

pub struct Manager {
    d: Deps,
    t: SessionTimeouts,
    set_gpu_realtime: bool,
    seats: Mutex<HashMap<String, SeatSlot>>,
    pending: Mutex<Vec<SessionNotice>>,
    next_generation: AtomicU64,
    /// `max_seats` of the last `host_info` (spec 30 §2.2, §2.5).
    host_max_seats: Mutex<Option<Option<u32>>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn ms(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

fn helper_failure(code: HelperFailure, detail: String) -> SessionError {
    match code {
        HelperFailure::ClientAuth => SessionError::ClientAuth(detail),
        HelperFailure::CertMismatch => SessionError::CertMismatch(detail),
        HelperFailure::BadArgs => SessionError::ClientExited(format!("helper rejected its arguments: {detail}")),
        HelperFailure::Tls | HelperFailure::Network | HelperFailure::Protocol => {
            SessionError::ClientExited(format!("{code:?}: {detail}").to_lowercase())
        }
    }
}

/// A session that is still the seat user's remote session (client name / address are
/// not required: Windows may blank them once the client is gone).
fn same_user_session(s: &crate::discover::WtsSession, user: &str) -> bool {
    s.is_remote && !s.is_system_or_listener() && !user.is_empty() && s.user.eq_ignore_ascii_case(user)
}

impl Manager {
    pub fn new(d: Deps, t: SessionTimeouts) -> Self {
        Self {
            d,
            t,
            set_gpu_realtime: true,
            seats: Mutex::new(HashMap::new()),
            pending: Mutex::new(Vec::new()),
            next_generation: AtomicU64::new(1),
            host_max_seats: Mutex::new(None),
        }
    }

    /// `SetGpuRealtimePriority` row on/off (spec 30 §2.3, "optional, default on").
    pub fn with_gpu_realtime(mut self, on: bool) -> Self {
        self.set_gpu_realtime = on;
        self
    }

    fn applier(&self) -> RdsApplier<Arc<dyn Registry>, Arc<dyn RestoreLog>> {
        RdsApplier::new(self.d.registry.clone(), self.d.journal.clone()).with_gpu_realtime(self.set_gpu_realtime)
    }

    /// Start cancelled by the caller, or the seat was stopped / restarted meanwhile.
    fn aborted(&self, seat_id: &str, generation: u64, cancel: &CancelToken) -> bool {
        cancel.is_cancelled()
            || !lock(&self.seats).get(seat_id).is_some_and(|s| s.generation == generation && s.phase != SessionPhase::Stopping)
    }

    fn set_phase(&self, seat_id: &str, generation: u64, p: SessionPhase) {
        if let Some(s) = lock(&self.seats).get_mut(seat_id).filter(|s| s.generation == generation) {
            s.phase = p;
        }
    }

    fn helper_args(&self, listener: &ListenerInfo, slot: &SeatSlot) -> Result<HelperArgs, SessionError> {
        let cert = listener
            .cert_sha1
            .ok_or_else(|| SessionError::CertMismatch("RDP listener certificate thumbprint is unknown".into()))?;
        let address = if listener.ipv6_only { IpAddr::V6(Ipv6Addr::LOCALHOST) } else { IpAddr::V4(Ipv4Addr::LOCALHOST) };
        let a = HelperArgs {
            protocol: HELPER_PROTOCOL,
            port: listener.port,
            address,
            client_name: slot.client_name.clone(),
            user: slot.user.clone(),
            width: slot.width.clamp(MIN_SIZE, MAX_SIZE),
            height: slot.height.clamp(MIN_SIZE, MAX_SIZE),
            layout: slot.layout,
            cert_sha1: cert,
            // Spec 70 §2.6: always join rdpsnd so the session gets its Remote Audio endpoint.
            audio_playback: true,
            audio_capture: slot.audio_capture,
            dry_run: false,
        };
        a.validate()?;
        Ok(a)
    }

    /// Ownership of session `id` right now: `Some(true)` = still our seat session (remote,
    /// client name + user + loopback), `Some(false)` = someone else's, `None` = gone.
    fn ownership(&self, id: u32, client_name: &str, user: &str) -> Result<Option<bool>, SessionError> {
        let all = self.d.wts.sessions()?;
        Ok(all.iter().find(|s| s.id == id).map(|s| s.is_remote && s.matches(client_name, user)))
    }

    /// Log-off of a session whose ownership was established earlier (our discovery or a
    /// check before the helper was ended): only the id, still remote, still the seat user.
    fn logoff_same_user(&self, id: u32, user: &str) -> Result<bool, SessionError> {
        let all = self.d.wts.sessions()?;
        match all.iter().find(|s| s.id == id) {
            Some(s) if same_user_session(s, user) => {
                self.d.wts.logoff(id, false)?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn wait_session_gone(&self, id: u32, user: &str) -> bool {
        poll_until(self.d.clock.as_ref(), self.d.sleeper.as_ref(), self.t.logoff_wait, POLL_STEP, || match self.d.wts.sessions() {
            Ok(all) => (!all.iter().any(|s| s.id == id && same_user_session(s, user))).then_some(()),
            Err(_) => None,
        })
        .is_some()
    }

    fn save_run_state(&self) {
        let recs: Vec<OwnedSessionRecord> = lock(&self.seats)
            .iter()
            .filter(|(_, s)| s.is_live() && s.child.is_some())
            .map(|(id, s)| {
                let c = s.child.as_ref().expect("filtered");
                OwnedSessionRecord {
                    seat_id: id.clone(),
                    client_name: s.client_name.clone(),
                    user_sid: s.user_sid.clone(),
                    session_id: s.session_id,
                    helper_pid: c.pid(),
                    helper_creation_time: c.creation_time(),
                }
            })
            .collect();
        if let Err(e) = self.d.run_state.save(&recs) {
            log::warn!("saving run state failed: {e}");
        }
    }

    /// Seat cap from the host class (cached by `host_info`; computed once, conservatively
    /// without a wrapper, when the service never asked).
    fn host_max_seats(&self) -> Result<Option<u32>, SessionError> {
        if let Some(m) = *lock(&self.host_max_seats) {
            return Ok(m);
        }
        Ok(self.host_info(false)?.max_seats)
    }

    /// Spawn + wait for `Connected` + discover the session. Runs without the seats lock.
    fn connect(
        &self,
        seat_id: &str,
        generation: u64,
        args: &HelperArgs,
        password: &Secret,
        cancel: &CancelToken,
        child_out: &mut Option<Box<dyn HelperChild>>,
    ) -> Result<u32, SessionError> {
        let deadline = self.d.clock.now_ms().saturating_add(ms(self.t.session_appear));
        let left = |clock: &dyn Clock| Duration::from_millis(deadline.saturating_sub(clock.now_ms()));
        *child_out = Some(self.d.helper.spawn(args, password)?);
        let child = child_out.as_mut().expect("just set");
        let reported = loop {
            if self.aborted(seat_id, generation, cancel) {
                return Err(SessionError::Cancelled);
            }
            let wait = left(self.d.clock.as_ref()).min(POLL_STEP);
            match child.next_event(wait) {
                Some(HelperEvent::Connecting) => {}
                Some(HelperEvent::Authenticated) => self.set_phase(seat_id, generation, SessionPhase::Authenticated),
                Some(HelperEvent::Connected { session_id }) => break session_id,
                Some(HelperEvent::Failed { code, detail }) => return Err(helper_failure(code, detail)),
                Some(HelperEvent::Disconnected { reason }) => return Err(SessionError::ClientExited(format!("disconnected: {reason}"))),
                Some(HelperEvent::Exited { code }) => return Err(SessionError::ClientExited(format!("helper exited with {code}"))),
                None => {
                    if let Some(code) = child.try_wait() {
                        return Err(SessionError::ClientExited(format!("helper exited with {code}")));
                    }
                    if self.d.clock.now_ms() >= deadline {
                        return Err(SessionError::SessionTimeout("client did not connect in time".into()));
                    }
                }
            }
        };
        self.set_phase(seat_id, generation, SessionPhase::WaitingForSession);
        let mut exited = None;
        let mut cancelled = false;
        let found = poll_until(self.d.clock.as_ref(), self.d.sleeper.as_ref(), left(self.d.clock.as_ref()), POLL_STEP, || {
            if self.aborted(seat_id, generation, cancel) {
                cancelled = true;
                return Some(None);
            }
            if let Some(code) = child.try_wait() {
                exited = Some(code);
                return Some(None);
            }
            let all = self.d.wts.sessions().ok()?;
            find_seat_session_preferring(&all, &args.client_name, &args.user, reported).map(Some)
        });
        if cancelled {
            return Err(SessionError::Cancelled);
        }
        if let Some(code) = exited {
            return Err(SessionError::ClientExited(format!("helper exited with {code}")));
        }
        found.flatten().ok_or_else(|| SessionError::SessionTimeout("seat session did not appear".into()))
    }

    /// Under the lock: mark the seat failed now; the kill + logoff run later without it.
    fn fail_now(
        &self,
        seat_id: &str,
        slot: &mut SeatSlot,
        reason: FailureReason,
        prev: Option<Box<dyn HelperChild>>,
        notices: &mut Vec<SessionNotice>,
    ) -> TickAction {
        let old = slot.child.take().or(prev);
        slot.phase = SessionPhase::Failed(reason);
        notices.push(SessionNotice::Failed { seat_id: seat_id.to_owned(), reason });
        // A failed seat logs off its session (spec 30 §7).
        TickAction::Fail { old, session: slot.session_id.take(), user: slot.user.clone() }
    }

    /// Under the lock: unexpected helper end while the seat runs: restart or give up.
    fn helper_gone(&self, seat_id: &str, slot: &mut SeatSlot, now_ms: u64, notices: &mut Vec<SessionNotice>) -> TickAction {
        let old = slot.child.take();
        let window = ms(self.t.client_restart_window);
        slot.failures.push_back(now_ms);
        while slot.failures.front().is_some_and(|t| now_ms.saturating_sub(*t) > window) {
            slot.failures.pop_front();
        }
        if slot.failures.len() as u32 >= self.t.client_restart_max {
            log::warn!("seat {seat_id}: RDP client failed {} times, giving up", slot.failures.len());
            return self.fail_now(seat_id, slot, FailureReason::ClientExited, old, notices);
        }
        slot.phase = SessionPhase::Reconnecting;
        slot.reported_session = None;
        slot.reconnect_connected = false;
        slot.reconnect_deadline_ms = now_ms.saturating_add(ms(self.t.session_appear));
        TickAction::Restart {
            seat_id: seat_id.to_owned(),
            generation: slot.generation,
            old,
            meta: clone_meta(slot),
            password: Zeroizing::new(slot.password.to_string()),
        }
    }

    /// Under the lock: non-blocking look at one seat (events, exit, deadline).
    fn plan_slot(&self, seat_id: &str, slot: &mut SeatSlot, now_ms: u64, notices: &mut Vec<SessionNotice>) -> Option<TickAction> {
        match slot.phase {
            SessionPhase::Ready => {
                let c = slot.child.as_mut()?;
                let mut gone = c.try_wait().is_some();
                while let Some(ev) = c.next_event(Duration::ZERO) {
                    if matches!(ev, HelperEvent::Disconnected { .. } | HelperEvent::Exited { .. } | HelperEvent::Failed { .. }) {
                        gone = true;
                    }
                }
                gone.then(|| self.helper_gone(seat_id, slot, now_ms, notices))
            }
            SessionPhase::Reconnecting => {
                // No child: a restart is in flight in another tick.
                let c = slot.child.as_mut()?;
                let events: Vec<HelperEvent> = std::iter::from_fn(|| c.next_event(Duration::ZERO)).collect();
                let mut gone = false;
                for ev in events {
                    match ev {
                        HelperEvent::Connected { session_id } => {
                            slot.reconnect_connected = true;
                            slot.reported_session = session_id;
                        }
                        HelperEvent::Failed { code: code @ (HelperFailure::ClientAuth | HelperFailure::CertMismatch), detail } => {
                            let reason =
                                if code == HelperFailure::ClientAuth { FailureReason::ClientAuth } else { FailureReason::CertMismatch };
                            log::warn!("seat {seat_id}: reconnect failed: {detail}");
                            return Some(self.fail_now(seat_id, slot, reason, None, notices));
                        }
                        HelperEvent::Failed { .. } | HelperEvent::Disconnected { .. } | HelperEvent::Exited { .. } => gone = true,
                        _ => {}
                    }
                }
                let exited = slot.child.as_mut().is_some_and(|c| c.try_wait().is_some());
                if gone || exited || now_ms >= slot.reconnect_deadline_ms {
                    return Some(self.helper_gone(seat_id, slot, now_ms, notices));
                }
                slot.reconnect_connected.then(|| TickAction::Discover {
                    seat_id: seat_id.to_owned(),
                    generation: slot.generation,
                    client_name: slot.client_name.clone(),
                    user: slot.user.clone(),
                    reported: slot.reported_session,
                })
            }
            _ => None,
        }
    }

    /// Without the lock: the blocking part of one action. Returns follow-up actions (a
    /// failed restart turns into a `Fail`).
    fn run_action(&self, a: TickAction, now_ms: u64, sessions: &mut Option<Vec<crate::discover::WtsSession>>, notices: &mut Vec<SessionNotice>) -> Option<TickAction> {
        match a {
            TickAction::Fail { old, session, user } => {
                if let Some(mut c) = old {
                    c.kill();
                }
                if let Some(id) = session {
                    if let Err(e) = self.logoff_same_user(id, &user) {
                        log::warn!("logoff of failed seat session {id} failed: {e}");
                    }
                }
                None
            }
            TickAction::Restart { seat_id, generation, old, meta, password } => {
                if let Some(mut c) = old {
                    c.kill();
                }
                let spawned = self.d.rds.listener().and_then(|l| {
                    let args = self.helper_args(&l, &meta)?;
                    let mut pw = Secret(password.to_string());
                    let r = self.d.helper.spawn(&args, &pw);
                    pw.0.zeroize();
                    r
                });
                let mut seats = lock(&self.seats);
                let slot = seats
                    .get_mut(&seat_id)
                    .filter(|s| s.generation == generation && s.phase == SessionPhase::Reconnecting && s.child.is_none());
                match (slot, spawned) {
                    (Some(slot), Ok(c)) => {
                        log::info!("seat {seat_id}: RDP client restarted");
                        slot.child = Some(c);
                        slot.reconnect_deadline_ms = now_ms.saturating_add(ms(self.t.session_appear));
                        None
                    }
                    (Some(slot), Err(e)) => {
                        log::warn!("seat {seat_id}: RDP client restart failed: {e}");
                        let reason = e.failure_reason().unwrap_or(FailureReason::ClientExited);
                        Some(self.fail_now(&seat_id, slot, reason, None, notices))
                    }
                    (None, Ok(mut c)) => {
                        // Stopped or restarted meanwhile: the new helper is not wanted.
                        drop(seats);
                        c.kill();
                        None
                    }
                    (None, Err(_)) => None,
                }
            }
            TickAction::Discover { seat_id, generation, client_name, user, reported } => {
                if sessions.is_none() {
                    *sessions = self.d.wts.sessions().ok();
                }
                let id = find_seat_session_preferring(sessions.as_deref()?, &client_name, &user, reported)?;
                let mut seats = lock(&self.seats);
                if let Some(slot) = seats.get_mut(&seat_id).filter(|s| {
                    s.generation == generation && s.phase == SessionPhase::Reconnecting && s.reconnect_connected && s.child.is_some()
                }) {
                    slot.session_id = Some(id);
                    slot.phase = SessionPhase::Ready;
                    notices.push(SessionNotice::Reconnected { seat_id, session_id: id });
                }
                None
            }
        }
    }

    /// Failed start: undo what this attempt (generation) owns, never a newer attempt's.
    fn undo_failed_start(&self, req: &SessionRequest<'_>, generation: u64, e: &SessionError) {
        // Claim the seat while logging off, so no new attempt starts in between; a newer
        // attempt already there owns the seat (and its session): leave both alone.
        let claimed = {
            let mut seats = lock(&self.seats);
            match seats.get_mut(req.seat_id) {
                Some(s) if s.generation != generation => false,
                Some(s) => {
                    s.phase = SessionPhase::Stopping;
                    true
                }
                None => {
                    let mut claim = new_slot(generation, req, String::new(), &Secret(String::new()));
                    claim.phase = SessionPhase::Stopping;
                    seats.insert(req.seat_id.to_owned(), claim);
                    true
                }
            }
        };
        if !claimed {
            log::info!("seat {}: start attempt superseded ({e})", req.seat_id);
            return;
        }
        // Spec 30 §14: on timeout any matching session is logged off.
        if let Ok(all) = self.d.wts.sessions() {
            for s in all.iter().filter(|s| s.is_remote && s.matches(req.client_name, req.user)) {
                if let Err(le) = self.d.wts.logoff(s.id, false) {
                    log::warn!("seat {}: logoff after failed start failed: {le}", req.seat_id);
                }
            }
        }
        let mut seats = lock(&self.seats);
        if seats.get(req.seat_id).is_some_and(|s| s.generation == generation) {
            match e.failure_reason() {
                _ if matches!(e, SessionError::Cancelled) => {
                    seats.remove(req.seat_id);
                }
                r => {
                    if let Some(s) = seats.get_mut(req.seat_id) {
                        s.phase = SessionPhase::Failed(r.unwrap_or(FailureReason::ClientExited));
                    }
                }
            }
        }
    }
}

fn new_slot(generation: u64, req: &SessionRequest<'_>, user_sid: String, password: &Secret) -> SeatSlot {
    SeatSlot {
        generation,
        phase: SessionPhase::Connecting,
        client_name: req.client_name.to_owned(),
        user: req.user.to_owned(),
        user_sid,
        width: req.width,
        height: req.height,
        layout: req.keyboard_layout.filter(|l| *l != 0).unwrap_or(DEFAULT_LAYOUT),
        audio_capture: req.audio_capture,
        password: Zeroizing::new(password.0.clone()),
        child: None,
        session_id: None,
        reported_session: None,
        reconnect_connected: false,
        reconnect_deadline_ms: 0,
        failures: VecDeque::new(),
    }
}

impl SessionManager for Manager {
    fn host_info(&self, wrapper_active: bool) -> Result<HostInfo, SessionError> {
        let edition = self.d.edition.edition()?;
        let rdsh = edition.product_type.is_server() && self.d.edition.rdsh_role_installed().unwrap_or(false);
        let class = classify(&edition, rdsh);
        let listener = self.d.rds.listener().ok();
        let licensing = self.d.rds.licensing().ok();
        let service_dll = self.d.rds.service_dll().unwrap_or_default();
        let stock = termservice_is_stock(&service_dll);
        let rds_ready = self.d.rds.wait_ready(Duration::ZERO).is_ok();
        let licensed = licensing.as_ref().is_some_and(LicensingInfo::configured);
        let mut warnings = Vec::new();
        if class == HostClass::ServerWithoutRdsh {
            warnings.push(HostWarning::RdshMissing);
        }
        if edition.product_type.is_server() {
            if !licensed {
                warnings.push(HostWarning::LicensingNotConfigured);
            }
            if let Some(days) = licensing.as_ref().and_then(|l| l.grace_days_left) {
                warnings.push(HostWarning::GraceEnds { days });
            }
        }
        if !stock && !wrapper_active {
            warnings.push(HostWarning::ForeignWrapper { service_dll: service_dll.clone() });
        }
        // Policy overrides are detected read-only (no write here).
        for w in self.applier().verify_policy() {
            if let RdsWarning::PolicyOverride { value } = w {
                warnings.push(HostWarning::PolicyOverride { value });
            }
        }
        let listener_limit = listener.as_ref().and_then(|l| l.max_instance_count);
        let max_seats = match class {
            HostClass::Home => Some(0),
            HostClass::ClientMultiSessionUnavailable if !wrapper_active => Some(1),
            HostClass::ClientMultiSessionUnavailable => listener_limit,
            // Spec 30 §2.2 / §2.5: RDSH role or licensing mode missing: warn, and let one
            // test seat run (grace period).
            HostClass::ServerWithoutRdsh => Some(1),
            HostClass::ServerRdsh if !licensed => Some(1),
            HostClass::ServerRdsh => listener_limit,
        };
        *lock(&self.host_max_seats) = Some(max_seats);
        Ok(HostInfo { edition, class, listener, licensing, termservice_stock: stock, service_dll, rds_ready, max_seats, warnings })
    }

    fn prepare_host(&self, include_wrapper_rows: bool) -> Result<ApplyReport, SessionError> {
        let e = self.d.edition.edition()?;
        if crate::edition::is_home_edition(&e.edition_id) {
            return Err(SessionError::EditionUnsupported(format!(
                "Windows edition '{}' has no Remote Desktop host",
                e.edition_id
            )));
        }
        let dll = self.d.rds.service_dll()?;
        if !termservice_is_stock(&dll) && !include_wrapper_rows {
            return Err(SessionError::Coexistence { service_dll: dll });
        }
        self.applier().apply(include_wrapper_rows)
    }

    fn restore_host(&self) -> Result<RestoreReport, SessionError> {
        self.applier().restore_all()
    }

    fn wait_rds_ready(&self) -> Result<(), SessionError> {
        self.d.rds.wait_ready(self.t.rds_ready)
    }

    fn ensure_seat_user(&self, user: &str, password: &Secret) -> Result<UserCheck, SessionError> {
        check_password(password)?;
        let c = crate::users::ensure_seat_user(self.d.accounts.as_ref(), user)?;
        self.d.profiles.ensure_profile(user, password)?;
        Ok(c)
    }

    fn create_seat_user(&self, user: &str, password: &Secret) -> Result<(), SessionError> {
        split_core::validate::user_name(user).map_err(|e| SessionError::InvalidArgument(e.message))?;
        if user.is_empty() {
            return Err(SessionError::InvalidArgument("user name is empty".into()));
        }
        check_password(password)?;
        if self.d.accounts.facts(user)?.exists {
            return Err(SessionError::InvalidArgument("user already exists".into()));
        }
        let rec = |st| RestoreRecord::new(RecordKind::Account, user.to_owned(), JournalValue::Absent, JournalValue::Sz("created".into()), st);
        self.d.journal.append(rec(RecordStatus::Intent))?;
        self.d.accounts.create(user, password, crate::users::SEAT_USER_COMMENT)?;
        self.d.journal.append(rec(RecordStatus::Done))?;
        log::info!("created a seat user account");
        self.d.accounts.add_to_remote_desktop_users(user).map_err(|e| SessionError::RduMembership(e.to_string()))
    }

    fn start_session(&self, req: &SessionRequest<'_>, password: &Secret, cancel: &CancelToken) -> Result<SeatSession, SessionError> {
        check_password(password)?;
        if req.seat_id.is_empty() {
            return Err(SessionError::InvalidArgument("seat id is empty".into()));
        }
        let listener = self.d.rds.listener()?;
        let user_sid = self.d.accounts.sid(req.user)?;
        // Spec 30 §10: the console user is never a seat user.
        if self.d.wts.console_user_sid()?.is_some_and(|c| c.eq_ignore_ascii_case(&user_sid)) {
            return Err(SessionError::Account("the seat user is signed in at the console".into()));
        }
        let max = [self.host_max_seats()?, listener.max_instance_count].into_iter().flatten().min();
        let generation = self.next_generation.fetch_add(1, Ordering::SeqCst);
        let mut slot = new_slot(generation, req, user_sid.clone(), password);
        let args = self.helper_args(&listener, &slot)?;
        {
            let mut seats = lock(&self.seats);
            if seats.get(req.seat_id).is_some_and(SeatSlot::is_live) {
                return Err(SessionError::InvalidArgument("seat already has a session".into()));
            }
            let others = seats.values().filter(|s| s.is_live()).collect::<Vec<_>>();
            if others.iter().any(|s| s.user_sid.eq_ignore_ascii_case(&user_sid) || s.client_name.eq_ignore_ascii_case(req.client_name)) {
                return Err(SessionError::InvalidArgument("user or client name is used by another running seat".into()));
            }
            if let Some(max) = max {
                if others.len() as u32 >= max {
                    return Err(SessionError::SeatLimit { max });
                }
            }
            seats.insert(req.seat_id.to_owned(), clone_meta(&slot));
        }
        let mut child = None;
        let result = self.connect(req.seat_id, generation, &args, password, cancel, &mut child);
        match result {
            Ok(session_id) => {
                let c = child.expect("connected child");
                let out = SeatSession {
                    seat_id: req.seat_id.to_owned(),
                    session_id,
                    user_sid,
                    helper_pid: c.pid(),
                    client_name: req.client_name.to_owned(),
                };
                slot.phase = SessionPhase::Ready;
                slot.session_id = Some(session_id);
                slot.child = Some(c);
                {
                    let mut seats = lock(&self.seats);
                    let current = seats.get(req.seat_id).map(|s| (s.generation, s.phase));
                    if current.is_none_or(|(g, p)| g != generation || p == SessionPhase::Stopping) {
                        // Stopped (or superseded) while we were connecting: undo, do not
                        // resurrect, and never touch a newer attempt's session.
                        let superseded = current.is_some_and(|(g, _)| g != generation);
                        drop(seats);
                        if let Some(mut c) = slot.child.take() {
                            c.kill();
                        }
                        if !superseded {
                            let _ = self.logoff_same_user(session_id, req.user);
                        }
                        return Err(SessionError::Cancelled);
                    }
                    seats.insert(req.seat_id.to_owned(), slot);
                }
                self.save_run_state();
                log::info!("seat {}: session {session_id} ready", req.seat_id);
                Ok(out)
            }
            Err(e) => {
                if let Some(mut c) = child {
                    c.kill();
                }
                self.undo_failed_start(req, generation, &e);
                if !matches!(e, SessionError::Cancelled) {
                    log::warn!("seat {}: session start failed: {e}", req.seat_id);
                }
                Err(e)
            }
        }
    }

    fn phase(&self, seat_id: &str) -> Option<SessionPhase> {
        lock(&self.seats).get(seat_id).map(|s| s.phase)
    }

    fn stop_session(&self, seat_id: &str, action: StopAction) -> Result<(), SessionError> {
        let (generation, child, session_id, client_name, user) = {
            let mut seats = lock(&self.seats);
            let s = seats.get_mut(seat_id).ok_or_else(|| SessionError::NotFound(format!("no session for seat {seat_id}")))?;
            s.phase = SessionPhase::Stopping;
            (s.generation, s.child.take(), s.session_id, s.client_name.clone(), s.user.clone())
        };
        let mut result = Ok(());
        // Ownership is checked while our client is still connected (client name + address
        // are reliable then); after the kill only the id + user are checked.
        let owned = match (action, session_id) {
            (StopAction::LogOff, Some(id)) => match self.ownership(id, &client_name, &user) {
                Ok(Some(true)) => Some(id),
                Ok(Some(false)) => {
                    log::warn!("seat {seat_id}: session {id} is no longer ours; not logged off");
                    None
                }
                Ok(None) => {
                    log::info!("seat {seat_id}: session {id} already gone");
                    None
                }
                Err(e) => {
                    result = Err(e);
                    None
                }
            },
            _ => None,
        };
        if let Some(mut c) = child {
            c.kill();
        }
        if let Err(e) = self.d.launcher.kill_job(seat_id) {
            log::warn!("seat {seat_id}: ending job failed: {e}");
        }
        if let Some(id) = owned {
            match self.logoff_same_user(id, &user) {
                Ok(true) => {
                    if !self.wait_session_gone(id, &user) {
                        result = Err(SessionError::SessionTimeout(format!("session {id} did not log off in time")));
                    }
                }
                Ok(false) => log::info!("seat {seat_id}: session {id} already gone"),
                Err(e) => result = Err(e),
            }
        }
        {
            let mut seats = lock(&self.seats);
            if seats.get(seat_id).is_some_and(|s| s.generation == generation) {
                seats.remove(seat_id);
            }
        }
        self.save_run_state();
        result
    }

    fn tick(&self, now_ms: u64) -> Vec<SessionNotice> {
        let mut notices = std::mem::take(&mut *lock(&self.pending));
        // 1. Under the lock: non-blocking checks only; blocking work is collected.
        let mut actions: Vec<TickAction> = {
            let mut seats = lock(&self.seats);
            seats.iter_mut().filter_map(|(id, slot)| self.plan_slot(id, slot, now_ms, &mut notices)).collect()
        };
        let changed = !actions.is_empty();
        // 2. Without the lock: kill, spawn, logoff, WTS enumeration; each commit re-checks
        //    generation + phase under the lock.
        let mut sessions = None;
        while let Some(a) = actions.pop() {
            if let Some(next) = self.run_action(a, now_ms, &mut sessions, &mut notices) {
                actions.push(next);
            }
        }
        if changed {
            self.save_run_state();
        }
        notices
    }

    fn on_session_change(&self, ev: SessionChange) {
        if let SessionChange::Logoff(id) = ev {
            let mut seats = lock(&self.seats);
            for (seat_id, s) in seats.iter_mut() {
                if s.session_id == Some(id) && matches!(s.phase, SessionPhase::Ready | SessionPhase::Reconnecting) {
                    // The seat user signed out inside the session; the helper drops and
                    // `tick` restarts it, which creates a fresh session.
                    s.session_id = None;
                    lock(&self.pending).push(SessionNotice::SessionGone { seat_id: seat_id.clone() });
                }
            }
        }
    }

    fn sweep(&self, seat_users: &[String]) -> Result<SweepPlan, SessionError> {
        let (active_sessions, active_pids): (Vec<u32>, Vec<(u32, u64)>) = {
            let seats = lock(&self.seats);
            let s: Vec<_> = seats.values().filter(|s| s.is_live()).collect();
            (
                s.iter().filter_map(|s| s.session_id).collect(),
                s.iter().filter_map(|s| s.child.as_ref().map(|c| (c.pid(), c.creation_time()))).collect(),
            )
        };
        let recs = self.d.run_state.load();
        let stale: Vec<_> = recs.into_iter().filter(|r| !active_pids.contains(&(r.helper_pid, r.helper_creation_time))).collect();
        let live: Vec<_> = self.d.wts.sessions()?.into_iter().filter(|s| !active_sessions.contains(&s.id)).collect();
        // SIDs of the live sessions a stale record names (their client name may be blank).
        let live_sids: Vec<(u32, String)> = live
            .iter()
            .filter(|s| stale.iter().any(|r| r.session_id == Some(s.id)) && !s.user.is_empty())
            .filter_map(|s| self.d.accounts.sid(&s.user).ok().map(|sid| (s.id, sid)))
            .collect();
        let pids: Vec<u32> = stale.iter().map(|r| r.helper_pid).collect();
        let live_pids = self.d.processes.alive(&pids);
        let plan = plan_sweep(&live, seat_users, &stale, &live_pids, &live_sids);
        for (pid, ct) in &plan.kill {
            if let Err(e) = self.d.processes.kill(*pid, *ct) {
                log::warn!("sweep: ending helper {pid} failed: {e}");
            }
        }
        for id in &plan.logoff {
            if let Err(e) = self.d.wts.logoff(*id, false) {
                log::warn!("sweep: logoff of session {id} failed: {e}");
            }
        }
        self.save_run_state();
        if !plan.logoff.is_empty() || !plan.kill.is_empty() {
            log::info!("sweep: logged off {} session(s), ended {} helper(s)", plan.logoff.len(), plan.kill.len());
        }
        Ok(plan)
    }

    fn launcher(&self) -> &dyn SessionLauncher {
        self.d.launcher.as_ref()
    }
}

fn clone_meta(s: &SeatSlot) -> SeatSlot {
    SeatSlot {
        generation: s.generation,
        phase: s.phase,
        client_name: s.client_name.clone(),
        user: s.user.clone(),
        user_sid: s.user_sid.clone(),
        width: s.width,
        height: s.height,
        layout: s.layout,
        audio_capture: s.audio_capture,
        password: Zeroizing::new(String::new()),
        child: None,
        session_id: None,
        reported_session: None,
        reconnect_connected: false,
        reconnect_deadline_ms: 0,
        failures: VecDeque::new(),
    }
}

// ---- Windows wiring ----

#[cfg(windows)]
pub use win_mgr::WindowsSessionManager;

#[cfg(windows)]
mod win_mgr {
    use super::*;
    use crate::win;
    use std::path::Path;

    /// The real manager: every dependency is the Windows implementation.
    pub struct WindowsSessionManager(Manager);

    impl WindowsSessionManager {
        /// `data_dir` = `%ProgramData%\PulsarSplit`; `helper_exe` = path of
        /// `pulsar-split-rdp-helper.exe`.
        pub fn new(data_dir: &Path, helper_exe: &Path, t: SessionTimeouts) -> Result<Self, SessionError> {
            let reg: Arc<dyn Registry> = Arc::new(win::registry::HklmRegistry);
            let clock = Arc::new(crate::SystemClock);
            let d = Deps {
                edition: Box::new(win::edition::WindowsEditionProbe::new(reg.clone())),
                rds: Box::new(win::rds_probe::WindowsRdsProbe::new(reg.clone())),
                registry: reg,
                journal: Arc::new(crate::journal::JsonlRestoreLog::new(data_dir)),
                accounts: Box::new(win::accounts::NetAccounts),
                profiles: Box::new(win::accounts::WindowsProfiles),
                wts: Box::new(win::wts::WindowsWts),
                launcher: Box::new(win::launch::WindowsLauncher::new(win::wts::WindowsWts)),
                run_state: Box::new(crate::sweep::FileRunState::new(data_dir)),
                processes: Box::new(win::process::WindowsProcessTable),
                helper: Box::new(win::helper::WindowsHelperSpawner::new(helper_exe)?),
                clock: clock.clone(),
                sleeper: clock,
            };
            Ok(Self(Manager::new(d, t)))
        }

        pub fn into_inner(self) -> Manager {
            self.0
        }
    }

    impl std::ops::Deref for WindowsSessionManager {
        type Target = Manager;
        fn deref(&self) -> &Manager {
            &self.0
        }
    }

    impl SessionManager for WindowsSessionManager {
        fn host_info(&self, w: bool) -> Result<HostInfo, SessionError> {
            self.0.host_info(w)
        }
        fn prepare_host(&self, w: bool) -> Result<ApplyReport, SessionError> {
            self.0.prepare_host(w)
        }
        fn restore_host(&self) -> Result<RestoreReport, SessionError> {
            self.0.restore_host()
        }
        fn wait_rds_ready(&self) -> Result<(), SessionError> {
            self.0.wait_rds_ready()
        }
        fn ensure_seat_user(&self, u: &str, p: &Secret) -> Result<UserCheck, SessionError> {
            self.0.ensure_seat_user(u, p)
        }
        fn create_seat_user(&self, u: &str, p: &Secret) -> Result<(), SessionError> {
            self.0.create_seat_user(u, p)
        }
        fn start_session(&self, r: &SessionRequest<'_>, p: &Secret, c: &CancelToken) -> Result<SeatSession, SessionError> {
            self.0.start_session(r, p, c)
        }
        fn phase(&self, s: &str) -> Option<SessionPhase> {
            self.0.phase(s)
        }
        fn stop_session(&self, s: &str, a: StopAction) -> Result<(), SessionError> {
            self.0.stop_session(s, a)
        }
        fn tick(&self, now: u64) -> Vec<SessionNotice> {
            self.0.tick(now)
        }
        fn on_session_change(&self, ev: SessionChange) {
            self.0.on_session_change(ev)
        }
        fn sweep(&self, u: &[String]) -> Result<SweepPlan, SessionError> {
            self.0.sweep(u)
        }
        fn launcher(&self) -> &dyn SessionLauncher {
            self.0.launcher()
        }
    }
}

#[cfg(test)]
#[path = "manager_tests.rs"]
mod tests;
