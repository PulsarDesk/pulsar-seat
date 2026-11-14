// SPDX-License-Identifier: GPL-3.0-only
//! Seat life cycle (architecture §7 step 5–6, spec 20 §7, §13.4): start, stop, re-logon,
//! session changes, the same-account policy, and the watchdog with back-off.
//!
//! Every seat has a generation counter. A start worker captures it and re-checks it after
//! each blocking backend call; a stop bumps it. A stale worker then releases only what it
//! created itself (the session it obtained) and never touches the slot, so it cannot tear
//! down a newer start. Releases of the slot ([`Service::cleanup_seat`]) name the generation
//! they belong to.
//!
//! Every state change of a start has a time limit: entering `starting` records a deadline,
//! and the watchdog fails a seat that is still starting after it (spec 20 §7.2, §13.3).

use crate::backends::kind_name;
use crate::platform::{LaunchSpec, SeatJobSpec, SessionUser};
use crate::service::{SESSION_POLL_MS, Service, State};
use crate::{eventid, log};
use seat_core::backend::audio::{AudioBinding, AudioRoute};
use seat_core::backend::display::SeatOutputs;
use seat_core::backend::session::{
    HostLaunch, KnownSession, SeatCredentials, SeatStartRequest, SessionInfo, SessionState, StartStep,
};
use seat_core::backend::{BackendError, ErrorKind};
use seat_core::config::{SameAccountPolicy, SeatAccount};
use seat_core::consts::{AGENT_EXE, HOST_EXE, RDP_CLIENT_NAME_PREFIX};

/// Launcher request limits (`start_seat_app`): argument count and command-line length
/// (`CreateProcess` allows 32767 characters).
const MAX_APP_ARGS: usize = 256;
const MAX_APP_COMMAND_LINE: usize = 32_000;
/// Launcher requests one session may make per [`APP_RATE_WINDOW_MS`].
pub(crate) const MAX_APP_STARTS_PER_WINDOW: usize = 10;
pub(crate) const APP_RATE_WINDOW_MS: u64 = 10_000;
/// Live launcher / startup apps one session may have that the service started.
pub(crate) const MAX_LIVE_APPS_PER_SESSION: usize = 64;
use seat_core::device::DeviceKind;
use seat_core::state::{FailureReason, SafeModeReason, SeatRuntime, SeatState, StartPhase};
use seat_core::{DeviceKey, SeatNumber, Secret};
use seat_proto::{AgentTaskKind, ErrorCode, Event, HostCommand, ProtoError};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Host crashes within this window count towards the limit.
pub const CRASH_WINDOW_MS: u64 = 10 * 60_000;
/// Host crashes in the window before the seat is marked failed.
pub const CRASH_LIMIT: usize = 3;
/// Longest restart back-off.
pub const MAX_BACKOFF_MS: u64 = 60_000;
/// Grace for a host to exit after `HostCommand::Exit` before it is terminated.
pub const HOST_EXIT_GRACE_MS: u64 = 5_000;
/// Same-account deny: notice shown this long before the session is logged off.
pub const DENY_LOGOFF_AFTER_MS: u32 = 15_000;
/// Extra time on top of `seat_start_timeout_s` for the steps before and after the session
/// wait (display bind, host launch, audio bind).
pub const START_DEADLINE_SLACK_MS: u64 = 60_000;

/// A seat host the service started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostProc {
    pub pid: u32,
    /// Creation time of the process (FILETIME), checked on adoption (PID reuse).
    #[serde(default)]
    pub created: u64,
    pub token: Secret,
    /// `host_take_credentials` already answered (once per launch).
    pub creds_taken: bool,
    #[serde(skip)]
    pub connected: bool,
}

/// Service-side record of one seat.
#[derive(Debug, Clone)]
pub struct Slot {
    pub seat: SeatNumber,
    pub rt: SeatRuntime,
    pub generation: u64,
    /// The seat should be running (watchdog restarts it).
    pub want: bool,
    pub host: Option<HostProc>,
    pub launch: Option<HostLaunch>,
    pub outputs: Option<SeatOutputs>,
    pub audio: Option<AudioRoute>,
    pub crashes: Vec<u64>,
    pub retry_at: Option<u64>,
    pub agent_pid: Option<u32>,
    /// Session whose startup apps were started (once per logon session).
    pub apps_started: Option<u32>,
    pub logon: Option<SessionUser>,
    /// While starting: the seat fails with `session_timeout` after this time.
    pub deadline_ms: Option<u64>,
    /// Session id the seat host reported (`host_report` `signed_in`), for this launch.
    pub reported_session: Option<u32>,
}

impl Slot {
    pub fn new(seat: SeatNumber) -> Self {
        Self {
            seat,
            rt: SeatRuntime::off(seat),
            generation: 0,
            want: false,
            host: None,
            launch: None,
            outputs: None,
            audio: None,
            crashes: Vec::new(),
            retry_at: None,
            agent_pid: None,
            apps_started: None,
            logon: None,
            deadline_ms: None,
            reported_session: None,
        }
    }

    /// Starting, running or stopping (anything but off / error / waiting).
    pub fn is_active(&self) -> bool {
        !matches!(self.rt.state, SeatState::Off | SeatState::Error { .. } | SeatState::WaitingForDisplay)
    }
}

/// `PSEAT-NN`.
pub fn client_name(seat: SeatNumber) -> String {
    format!("{RDP_CLIENT_NAME_PREFIX}{}", seat.two_digits())
}

/// Map a backend error to the seat's failure reason.
pub fn failure_reason(e: &BackendError) -> FailureReason {
    match (e.kind, e.reason.as_str()) {
        (_, "edition_unsupported" | "not_built") => FailureReason::EditionUnsupported,
        (_, "rds_not_ready" | "listener_missing" | "rds_licensing") => FailureReason::RdsNotReady,
        (_, "seat_limit") => FailureReason::SeatLimit,
        (_, "client_auth") => FailureReason::ClientAuth,
        (_, "console_in_use") => FailureReason::ConsoleInUse,
        (_, "console_busy") => FailureReason::ConsoleBusy,
        (_, "session_not_locked") => FailureReason::SessionNotLocked,
        (_, "nla_required") => FailureReason::NlaRequired,
        (_, "loopback_listener_unavailable") => FailureReason::LoopbackListenerUnavailable,
        (_, "kiosk_app_missing") => FailureReason::KioskAppMissing,
        (_, "console_not_ready") => FailureReason::ConsoleNotReady,
        (_, "not_our_session") => FailureReason::NotOurSession,
        (_, "fsu_disabled") => FailureReason::FsuDisabled,
        (_, "same_account_denied") => FailureReason::SameAccountDenied,

        (_, r) if r.starts_with("account") || r == "no_account" => FailureReason::Account,
        (ErrorKind::Logon, _) => FailureReason::LogonFailed,
        (ErrorKind::Timeout, _) => FailureReason::SessionTimeout,
        (ErrorKind::Driver, _) => FailureReason::Driver,
        (ErrorKind::Coexistence, _) => FailureReason::Coexistence,
        (ErrorKind::Policy, _) => FailureReason::Policy,
        (ErrorKind::NotReady, _) => FailureReason::RdsNotReady,
        _ => FailureReason::Internal,
    }
}

fn backoff_ms(crashes: usize) -> u64 {
    (1_000u64 << crashes.min(6)).min(MAX_BACKOFF_MS)
}

fn map_session_state(s: SessionState, user: Option<&SessionUser>) -> SeatState {
    let name = user.map(SessionUser::display).unwrap_or_default();
    match s {
        SessionState::Active if user.is_some() => SeatState::SignedIn { user: name },
        SessionState::Locked if user.is_some() => SeatState::Locked { user: name },
        SessionState::Gone => SeatState::Off,
        _ => SeatState::AtLogon,
    }
}

/// Kind of WTS session change the SCM reported (subset we act on).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionChange {
    Logon,
    Logoff,
    Lock,
    Unlock,
    Terminate,
    ConsoleConnect,
}

impl Service {
    pub(crate) fn set_seat_state(&self, st: &mut State, seat: SeatNumber, state: SeatState) {
        let now = self.now();
        let budget = Self::start_budget_ms(st);
        let Some(slot) = st.seats.get_mut(&seat) else { return };
        if slot.rt.state == state {
            return;
        }
        match (&slot.rt.state, &state) {
            (SeatState::Starting { .. }, SeatState::Starting { .. }) => {}
            (_, SeatState::Starting { .. }) => slot.deadline_ms = Some(now + budget),
            _ => slot.deadline_ms = None,
        }
        if let SeatState::Error { reason, detail } = &state {
            log::error_id(
                eventid::SEAT_START_FAILED,
                &format!("seat {seat}: {} ({detail})", kind_name(reason)),
            );
        }
        slot.rt.state = state;
        slot.rt.since_ms = now;
        let rt = slot.rt.clone();
        self.events.publish(Event::SeatState { runtime: rt });
        self.recompute_product(st);
    }

    /// Time a start may take from `queued` to a session (see [`START_DEADLINE_SLACK_MS`]).
    pub(crate) fn start_budget_ms(st: &State) -> u64 {
        u64::from(st.config.settings.timings.seat_start_timeout_s) * 1000 + START_DEADLINE_SLACK_MS
    }

    pub(crate) fn seat_of_session(&self, st: &State, session_id: u32) -> Option<SeatNumber> {
        if let Some(s) = st.seats.values().find(|s| Self::managed(st, s.seat) && s.rt.session_id == Some(session_id)) {
            return Some(s.seat);
        }
        let console = self.platform.console_session();
        let topo = st.config.settings.backends.host_topology;
        // Seat 1 bound to another session (its own, waiting off the console under
        // `fast_user_switch`): the console session is a seat N hand-over, not seat 1.
        let seat1 = st.seats.get(&SeatNumber::CONSOLE).and_then(|s| s.rt.session_id);
        (console == Some(session_id)
            && topo == seat_core::backend::session::HostTopology::SeatOneConsole
            && seat1.is_none_or(|id| id == session_id))
        .then_some(SeatNumber::CONSOLE)
    }

    /// Seat 1 mirrors the console session (`seat_one_console` only; under
    /// `dedicated_host` seat 1 is a started seat and the console is the host account's).
    /// `fast_user_switch`: while another seat holds the console, seat 1 stays bound to its
    /// own session waiting off the console (`SessionBackend::console_seat_away`), never to
    /// the seat N session on the console (VM test 2026-11-15: seat 1 took seat 2's fresh
    /// logon session, so the credential provider there was answered for seat 1).
    pub(crate) fn refresh_console_seat(&self) {
        let away = self.backends().session.console_seat_away();
        let console = away.or_else(|| self.platform.console_session());
        let user = console.and_then(|c| self.platform.session_user(c));
        let mut st = self.lock();
        if !st.config.settings.enabled || Self::dedicated(&st) {
            return;
        }
        let state = match (&console, &user) {
            (Some(_), Some(u)) => SeatState::SignedIn { user: u.display() },
            (Some(_), None) => SeatState::AtLogon,
            (None, _) => SeatState::Off,
        };
        if let Some(slot) = st.seats.get_mut(&SeatNumber::CONSOLE) {
            slot.rt.session_id = console;
            slot.rt.user_sid = user.as_ref().map(|u| u.sid.clone());
            slot.logon = user;
        }
        self.set_seat_state(&mut st, SeatNumber::CONSOLE, state);
    }

    /// Re-bind sessions that survived a service restart (spec 30 §6.2.14).
    pub(crate) fn adopt_sessions(&self) {
        let (known, hosts) = {
            let st = self.lock();
            (st.run.known_sessions.clone(), st.run_hosts())
        };
        if known.is_empty() {
            return;
        }
        let b = self.backends();
        let host_image = self.platform.install_dir().join(HOST_EXE);
        let adopted = match b.session.adopt(&known) {
            Ok(a) => a,
            Err(e) => {
                log::warn(&format!("adopt: {e}"));
                Vec::new()
            }
        };
        let mut st = self.lock();
        for (seat, info) in &adopted {
            let user = info.user_sid.as_ref().and_then(|_| self.platform.session_user(info.session_id));
            if let Some(slot) = st.seats.get_mut(seat) {
                slot.want = true;
                slot.rt.session_id = Some(info.session_id);
                slot.rt.user_sid = info.user_sid.clone();
                slot.logon = user.clone();
                // Only a host whose PID still names the same process (creation time and
                // image path) is adopted; a reused PID is never killed or trusted.
                if let Some(h) = hosts.iter().find(|(s, _)| s == seat).map(|(_, h)| h.clone()) {
                    if self.platform.adopt_process(h.pid, h.created, &host_image) {
                        slot.host = Some(HostProc { creds_taken: true, ..h });
                    } else {
                        log::warn(&format!("seat {seat}: recorded host pid {} is not ours any more", h.pid));
                    }
                }
            }
            let state = map_session_state(info.state, user.as_ref());
            self.set_seat_state(&mut st, *seat, state);
            log::info(&format!("seat {seat}: adopted session {}", info.session_id));
        }
        st.run.known_sessions.retain(|k| adopted.iter().any(|(s, _)| *s == k.seat));
        let keep: Vec<SeatNumber> = st.seats.values().filter(|s| s.host.is_some()).map(|s| s.seat).collect();
        st.run.hosts.retain(|s, _| keep.contains(s));
        self.save_run(&st);
    }

    // ---- start --------------------------------------------------------------------

    /// Check the product allows starting seats. `manual` = a user asked (not autostart).
    pub(crate) fn may_start(&self, st: &State, manual: bool) -> Result<(), ProtoError> {
        if !st.config.settings.enabled {
            return Err(ProtoError::new(ErrorCode::Disabled, "product is disabled"));
        }
        match st.safe_reason {
            Some(SafeModeReason::WindowsSafeMode) => Err(ProtoError::new(ErrorCode::SafeMode, "Windows Safe Mode")),
            Some(r) if !manual => Err(ProtoError::new(ErrorCode::SafeMode, kind_name(&r))),
            _ => Ok(()),
        }
    }

    /// Autostart seats (seat 1 is the console under `seat_one_console`), one after another.
    ///
    /// Never on a non-concurrent session backend (`fast_user_switch`, the fallback on
    /// client editions and ARM64): there every start switches the console away, so an
    /// `at_boot` autostart would churn through one logon session per seat and an
    /// `after_seat_one_logon` one would fail every seat with `console_in_use`. Only a
    /// manual `start_seats` (the switch trigger, spec 35 §5.5) starts seats there.
    pub fn start_autostart_seats(&self) {
        // Remote seats first: the remote seat worker starts them (it has its own checks,
        // `crate::remote::glue::start_remote_autostart`).
        self.start_remote_autostart();
        let b = self.backends();
        if !b.session.caps().concurrent {
            log::info(&format!(
                "autostart skipped: session backend {:?} cannot run seats at the same time; start seats from the manager",
                b.session.kind()
            ));
            return;
        }
        let seats: Vec<SeatNumber> = {

            let st = self.lock();
            if self.may_start(&st, false).is_err() {
                return;
            }
            // Remote seats were handed to the remote seat worker above.
            st.config.seats.iter().filter(|s| s.autostart && s.is_local() && Self::managed(&st, s.number)).map(|s| s.number).collect()
        };
        self.start_sequence(seats);
    }

    /// Queue seats and start them with `between_seats_ms` pauses on a worker thread.
    pub(crate) fn start_sequence(&self, seats: Vec<SeatNumber>) {
        self.start_sequence_with(seats, false);
    }

    /// [`Self::start_sequence`]; `unattended` = the start comes from a credential-provider
    /// switch link (nobody authenticated): no stored password is used for it and no
    /// one-time logon is armed, so the seat's user signs in on the seat tile.
    pub(crate) fn start_sequence_with(&self, seats: Vec<SeatNumber>, unattended: bool) {
        let gens: Vec<(SeatNumber, u64)> = {
            let mut st = self.lock();
            let mut out = Vec::new();
            for seat in seats {
                if !Self::managed(&st, seat) {
                    continue;
                }
                let Some(slot) = st.seats.get_mut(&seat) else { continue };
                if slot.is_active() {
                    continue;
                }
                slot.generation += 1;
                slot.want = true;
                slot.retry_at = None;
                slot.reported_session = None;
                let g = slot.generation;
                if unattended {
                    st.credprov.unattended.insert(seat, g);
                } else {
                    st.credprov.unattended.remove(&seat);
                }

                self.set_seat_state(&mut st, seat, SeatState::Starting { phase: StartPhase::Queued });
                out.push((seat, g));
            }
            out
        };
        if gens.is_empty() {
            return;
        }
        self.spawn(move |s| {
            let pause = Duration::from_millis(u64::from(s.lock().config.settings.timings.between_seats_ms));
            for (i, (seat, g)) in gens.into_iter().enumerate() {
                if s.is_stopping() {
                    return;
                }
                if i > 0 && !pause.is_zero() {
                    std::thread::sleep(pause);
                }
                s.run_start(seat, g);
            }
        });
    }

    fn current(&self, seat: SeatNumber, generation: u64) -> bool {
        self.lock().seats.get(&seat).is_some_and(|s| s.generation == generation) && !self.is_stopping()
    }

    fn phase(&self, seat: SeatNumber, generation: u64, phase: StartPhase) -> bool {
        let mut st = self.lock();
        if st.seats.get(&seat).is_none_or(|s| s.generation != generation) {
            return false;
        }
        self.set_seat_state(&mut st, seat, SeatState::Starting { phase });
        true
    }

    /// Present monitors whose effective mask is exactly this seat.
    pub(crate) fn seat_monitors(st: &State, seat: SeatNumber) -> Vec<DeviceKey> {
        st.inventory
            .effective(&st.config)
            .into_iter()
            .filter(|e| e.kind == DeviceKind::Monitor && e.mask.single() == Some(seat))
            .map(|e| e.key)
            .collect()
    }

    /// Endpoints for a seat: one that is this seat's alone first, else one it shares
    /// with some (not all) seats.
    pub(crate) fn audio_binding(st: &State, seat: SeatNumber) -> AudioBinding {
        let eff = st.inventory.effective(&st.config);
        let pick = |kind: DeviceKind| {
            eff.iter()
                .filter(|e| e.kind == kind)
                .find(|e| e.mask.single() == Some(seat))
                .or_else(|| eff.iter().filter(|e| e.kind == kind).find(|e| e.mask.contains(seat) && !e.mask.is_shared()))
                .map(|e| e.key.clone())
        };
        AudioBinding { render: pick(DeviceKind::AudioRender), capture: pick(DeviceKind::AudioCapture), fallback_to_shared: false }
    }

    pub(crate) fn credentials(&self, st: &State, seat: SeatNumber) -> Option<SeatCredentials> {
        let cfg = st.config.seat(seat)?;
        let (domain, user) = cfg.account.domain_user()?;
        let failing = st.run.failing_passwords.contains(&seat);
        let password = if failing {
            None
        } else {
            match self.secrets.get(seat) {
                Ok(p) => p,
                Err(e) => {
                    log::error_id(eventid::SECRET_STORE_FAILED, &format!("seat {seat}: secret read failed: {e}"));
                    None
                }
            }
        };
        Some(SeatCredentials { domain: domain.to_string(), user: user.to_string(), password })
    }

    /// [`Self::credentials`], reading the secret store outside the state lock (a slow LSA
    /// call must not block every pipe request).
    pub(crate) fn credentials_unlocked(&self, seat: SeatNumber) -> Option<SeatCredentials> {
        let (domain, user, failing) = {
            let st = self.lock();
            let cfg = st.config.seat(seat)?;
            let (d, u) = cfg.account.domain_user()?;
            (d.to_string(), u.to_string(), st.run.failing_passwords.contains(&seat))
        };
        let password = if failing {
            None
        } else {
            match self.secrets.get(seat) {
                Ok(p) => p,
                Err(e) => {
                    log::error_id(eventid::SECRET_STORE_FAILED, &format!("seat {seat}: secret read failed: {e}"));
                    None
                }
            }
        };
        Some(SeatCredentials { domain, user, password })
    }

    /// The current start of `seat` came from a switch link (see [`Self::start_sequence_with`]).
    pub(crate) fn unattended_start(st: &State, seat: SeatNumber) -> bool {
        st.credprov.unattended.get(&seat).is_some_and(|g| st.seats.get(&seat).is_some_and(|s| s.generation == *g))
    }

    /// `step=<phase> reason=<reason> [win32=<code>]`: the structured failure detail
    /// (spec 20 §13.3 step 5, spec 00 §5.7 step 6).
    pub(crate) fn failure_detail(state: &SeatState, e: &BackendError) -> String {
        let step = match state {
            SeatState::Starting { phase } => kind_name(phase),
            SeatState::Stopping => "stopping".into(),
            _ => "running".into(),
        };
        let mut d = match e.win32 {
            Some(w) => format!("step={step} reason={} win32={w}", e.reason),
            None => format!("step={step} reason={}", e.reason),
        };
        // The listener's state note (the manager shows per-state Turkish text).
        if e.reason == "loopback_listener_unavailable"
            && let Some(note) = e.message.split_whitespace().find(|w| w.starts_with("loopback_listener:"))
        {
            d.push_str(&format!(" listener={}", note.trim_matches(|c: char| c == '(' || c == ')')));
        }
        d
    }

    fn fail(&self, seat: SeatNumber, generation: u64, e: &BackendError) {
        let reason = failure_reason(e);
        let mut st = self.lock();
        let Some(slot) = st.seats.get_mut(&seat) else { return };
        if slot.generation != generation {
            return;
        }
        slot.want = false;
        let detail = Self::failure_detail(&slot.rt.state, e);
        if reason == FailureReason::LogonFailed && !st.run.failing_passwords.contains(&seat) {
            st.run.failing_passwords.push(seat);
            self.save_run(&st);
            log::error_id(eventid::SEAT_LOGON_FAILED, &format!("seat {seat}: stored password marked failing"));
        }
        self.set_seat_state(&mut st, seat, SeatState::Error { reason, detail });
        drop(st);
        self.cleanup_seat(seat, false, Some(generation));
    }

    /// A start worker found its generation outdated (stopped, re-logon, watchdog). It
    /// releases only what it created itself: `session`, when no seat records it. The slot
    /// belongs to the newer generation and is left alone (with the current generation
    /// during a service stop it is released as usual).
    fn abandon_start(&self, seat: SeatNumber, generation: u64, session: Option<u32>) {
        let current = self.lock().seats.get(&seat).is_some_and(|s| s.generation == generation);
        if let Some(id) = session {
            let recorded = self.lock().seats.values().any(|s| s.rt.session_id == Some(id));
            if !recorded {
                let _ = self.backends().session.log_off(seat, id);
            }
        }
        if current {
            self.cleanup_seat(seat, false, Some(generation));
        } else {
            log::debug(&format!("seat {seat}: start generation {generation} abandoned"));
        }
    }

    /// The start sequence of one seat (runs on a worker thread).
    pub(crate) fn run_start(&self, seat: SeatNumber, generation: u64) {
        let b = self.backends();
        {
            // The time limit of this start begins now.
            let mut st = self.lock();
            let deadline = self.now() + Self::start_budget_ms(&st);
            if let Some(s) = st.seats.get_mut(&seat).filter(|s| s.generation == generation) {
                s.deadline_ms = Some(deadline);
            }
        }
        // 1. Display: the seat's monitors.
        let (monitors, needs_display, prefs) = {
            let st = self.lock();
            let needs = b.display.kind() != seat_core::backend::DisplayBackendKind::ConsoleOnly;
            let prefs = seat_core::backend::display::SeatDisplayPrefs::from_config(&st.config, seat);
            (Self::seat_monitors(&st, seat), needs, prefs)
        };
        if needs_display && monitors.is_empty() {
            let mut st = self.lock();
            if st.seats.get(&seat).is_some_and(|s| s.generation == generation) {
                self.set_seat_state(&mut st, seat, SeatState::WaitingForDisplay);
            }
            return;
        }
        if !self.phase(seat, generation, StartPhase::BindingDisplay) {
            return;
        }
        if needs_display {
            self.force_assign_outputs();
        }

        // Per-seat preferences (clone groups, software cursor, mode follow) first: the
        // binding reads them in `bind_seat`. The frame sections of `pulsar_idd` must be
        // readable by the seat hosts' account (host containment).
        let prefs = seat_core::backend::display::SeatDisplayPrefs { frame_reader_sid: self.platform.host_frame_reader_sid(seat), ..prefs };
        b.display.configure_seat(seat, &prefs);
        let outputs = match b.display.bind_seat(seat, &monitors) {
            Ok(o) => o,
            Err(e) => return self.fail(seat, generation, &e),
        };
        {
            let mut st = self.lock();
            if let Some(s) = st.seats.get_mut(&seat).filter(|s| s.generation == generation) {
                s.outputs = Some(outputs);
            }
        }
        // 2. Session backend.
        if !self.phase(seat, generation, StartPhase::LaunchingHost) {
            return self.abandon_start(seat, generation, None);
        }
        let (cfg, settings, creds) = {
            let st = self.lock();
            let Some(cfg) = st.config.seat(seat).cloned() else { return };
            let unattended = Self::unattended_start(&st, seat);
            drop(st);
            // Secret store outside the state lock.
            let mut creds = self.credentials_unlocked(seat);
            // A switch-link start authenticated nobody: never hand the stored password to the
            // backend (it would connect an existing, unlocked session without a prompt).
            if unattended && let Some(c) = creds.as_mut() {
                c.password = None;
            }
            let settings = self.lock().config.settings.clone();
            (cfg, settings, creds)
        };
        let req = SeatStartRequest { seat, config: &cfg, settings: &settings, client_name: client_name(seat), credentials: creds };
        // fast_user_switch: LogonUI of the fresh session enumerates before the grant is
        // armed; it keeps asking while this hand-over is noted (spec 35 §5.4).
        self.note_credprov_handover(seat);
        let step = b.session.start(&req);
        drop(req);
        let info: SessionInfo = match step {
            Err(e) => return self.fail(seat, generation, &e),
            Ok(StartStep::Started(info)) => info,
            Ok(StartStep::LaunchHost(hl)) => {
                if let Err(e) = self.launch_host(seat, generation, hl) {
                    return self.fail(seat, generation, &e);
                }
                if !self.phase(seat, generation, StartPhase::WaitingForSession) {
                    return self.abandon_start(seat, generation, None);
                }
                let timeout = Duration::from_secs(u64::from(settings.timings.seat_start_timeout_s));
                match b.session.wait_session(seat, &client_name(seat), timeout) {
                    Ok(i) => i,
                    Err(e) => return self.fail(seat, generation, &e),
                }
            }
        };
        if !self.current(seat, generation) {
            // Stopped meanwhile: end the session we just got, leave the slot alone.
            return self.abandon_start(seat, generation, Some(info.session_id));
        }
        // 3. Audio route.
        let binding = Self::audio_binding(&self.lock(), seat);
        let route = match b.audio.bind_seat(seat, &binding) {
            Ok(r) => Some(r),
            Err(e) => {
                log::warn(&format!("seat {seat}: audio not bound: {e}"));
                None
            }
        };
        // 4. Record the session.
        let user = info.user_sid.as_ref().and_then(|_| self.platform.session_user(info.session_id));
        let host_connected = {
            let mut st = self.lock();
            let Some(slot) = st.seats.get_mut(&seat).filter(|s| s.generation == generation) else { return };
            slot.rt.session_id = Some(info.session_id);
            slot.rt.user_sid = info.user_sid.clone();
            slot.logon = user.clone();
            slot.audio = route.clone();
            // Host crashes are not cleared on success: they expire after CRASH_WINDOW_MS,
            // so a host that dies right after every start still reaches the limit.
            let connected = slot.host.as_ref().is_some_and(|h| h.connected);
            let mut degraded = Vec::new();
            if b.display.kind() == seat_core::backend::DisplayBackendKind::WindowPresenter {
                degraded.push("display_fallback".to_string());
            }
            if b.input.kind() == seat_core::backend::InputBackendKind::RawInputHost && !Self::dedicated(&st) {
                degraded.push("input_not_isolated".to_string());
            }
            if route.as_ref().is_none_or(|r| r.render_endpoint_id.is_none()) {
                degraded.push("audio_console_shared".to_string());
            }
            if let Some(slot) = st.seats.get_mut(&seat) {
                slot.rt.degraded = degraded;
            }
            st.run.known_sessions.retain(|k| k.seat != seat);
            st.run.known_sessions.push(KnownSession {
                seat,
                session_id: info.session_id,
                client_name: client_name(seat),
                user_sid: info.user_sid.clone(),
            });
            self.save_run(&st);
            let state = map_session_state(info.state, user.as_ref());
            self.set_seat_state(&mut st, seat, state);
            connected
        };
        if let (true, Some(r)) = (host_connected, route) {
            self.events.publish(Event::HostCommand { seat, command: HostCommand::RebindAudio { route: r } });
        }
        log::info(&format!("seat {seat}: session {} ready", info.session_id));
        let at_logon = user.is_none() && info.state == SessionState::AtLogon;
        // fast_user_switch: the console notification of a fresh logon session usually
        // arrives before the session is bound to this seat, so its topology is queued here.
        if at_logon
            && b.session.kind() == seat_core::backend::SessionBackendKind::FastUserSwitch
            && self.platform.console_session() == Some(info.session_id)
        {
            self.queue_switch_topology(info.session_id);
        }
        if let Some(u) = user {
            self.on_seat_logon(seat, info.session_id, u);
        }
        // fast_user_switch: the fresh logon session gets the one-time credential for the
        // Pulsar credential provider (spec 35 §5.4).
        self.maybe_arm_credprov(seat, info.session_id, at_logon);
        self.lock().ownership_dirty = true;
    }

    /// The seat's policy redirects the clipboard (some direction on), as the host's
    /// assignment resolves it (`seat_rdp::policy::host_redirection`: the seat's clipboard
    /// policy, nothing for the console seat). Decides the clipboard right of a contained
    /// host's window-station grant; a later policy change takes effect at the host's next
    /// start.
    fn seat_redirects_clipboard(&self, seat: SeatNumber) -> bool {
        if seat.is_console() {
            return false;
        }
        let st = self.lock();
        let Some(cfg) = st.config.seat(seat) else { return false };
        let d = cfg.local_settings().redirect.clipboard.direction;
        d.to_seat() || d.from_seat()
    }

    /// The seat's policy redirects printers: its host account's user profile is loaded for
    /// the host's lifetime (per-user default printer and printer connections).
    fn seat_redirects_printers(&self, seat: SeatNumber) -> bool {
        if seat.is_console() {
            return false;
        }
        let st = self.lock();
        st.config.seat(seat).is_some_and(|c| c.local_settings().redirect.printers != seat_core::config::PrinterRedirect::Off)
    }

    fn launch_host(&self, seat: SeatNumber, generation: u64, hl: HostLaunch) -> Result<(), BackendError> {
        let token = self.platform.random_token()?;
        // The host receives the seat's credentials, so it runs with a SYSTEM token in the
        // target session and a SYSTEM-only process DACL: the session user cannot read its
        // memory or command line (where the one-time token is).
        let spec = LaunchSpec {
            session_id: hl.target_session,
            exe: self.platform.install_dir().join(HOST_EXE),
            // The token goes to the host's stdin (an inherited anonymous pipe), never into
            // the command line.
            args: vec!["--seat".into(), seat.get().to_string(), "--token-stdin".into()],
            log_name: format!("host-{}.log", seat.two_digits()),
            as_system: true,
            // Our infrastructure, not a seat app: never CPU-limited.
            job: None,
            env: Vec::new(),
            stdin_secret: Some(token.clone()),
            service_child: true,
            host_clipboard: self.seat_redirects_clipboard(seat),
            host_seat: Some(seat),
            host_profile: self.seat_redirects_printers(seat),
        };
        let pid = self.platform.launch(&spec)?;
        let created = self.platform.process_created(pid).unwrap_or(0);
        let mut st = self.lock();
        let Some(slot) = st.seats.get_mut(&seat).filter(|s| s.generation == generation) else {
            drop(st);
            self.platform.terminate(pid);
            return Err(BackendError::new(ErrorKind::Internal, "stopped", "seat stopped during launch"));
        };
        slot.host = Some(HostProc { pid, created, token, creds_taken: false, connected: false });
        slot.launch = Some(hl);
        let rec = st.seats.get(&seat).and_then(|s| s.host.clone());
        st.run.set_host(seat, rec);
        self.save_run(&st);
        log::info(&format!("seat {seat}: host started (pid {pid})"));
        self.set_seat_state(&mut st, seat, SeatState::Starting { phase: StartPhase::Connecting });
        Ok(())
    }

    // ---- stop ---------------------------------------------------------------------

    /// Stop a seat now on this thread: host exit, log off, release display / audio.
    pub(crate) fn stop_seat_now(&self, seat: SeatNumber, log_off: bool) {
        let (session, generation) = {
            let mut st = self.lock();
            let Some(slot) = st.seats.get_mut(&seat) else { return };
            slot.generation += 1;
            slot.want = false;
            slot.retry_at = None;
            let session = slot.rt.session_id;
            let g = slot.generation;
            st.deny_logoff.retain(|s, _| Some(*s) != session);
            self.set_seat_state(&mut st, seat, SeatState::Stopping);
            (session, g)
        };
        self.events.publish(Event::HostCommand { seat, command: HostCommand::Exit });
        if log_off && let Some(id) = session {
            let b = self.backends();
            if let Err(e) = b.session.log_off(seat, id) {
                log::warn(&format!("seat {seat}: log off: {e}"));
            }
        }
        self.cleanup_seat(seat, true, Some(generation));
        let mut st = self.lock();
        // A newer start (re-logon, a new start request) owns the state now.
        if st.seats.get(&seat).is_some_and(|s| s.generation == generation) {
            self.set_seat_state(&mut st, seat, SeatState::Off);
        }        drop(st);
        self.spawn_retry_deferred();
    }

    /// Deferred session-backend work (a TermService restart skipped while seats ran, so a
    /// removed passwordless-seat listener is still served), off this thread.
    pub(crate) fn spawn_retry_deferred(&self) {
        self.spawn(|s| {
            let notes = s.backends().session.retry_deferred();
            if !notes.is_empty() {
                log::info(&format!("session backend deferred work: {notes:?}"));
            }
        });
    }

    /// Release what a seat holds. `wait_host` gives the host a short grace to exit.
    /// With `generation`, nothing happens unless the slot is still at that generation
    /// (a newer start owns it otherwise).
    pub(crate) fn cleanup_seat(&self, seat: SeatNumber, wait_host: bool, generation: Option<u64>) {
        let b = self.backends();
        let (host, agent) = {
            let mut st = self.lock();
            let Some(slot) = st.seats.get_mut(&seat) else { return };
            if generation.is_some_and(|g| g != slot.generation) {
                return;
            }
            let host = slot.host.take().map(|h| h.pid);
            let agent = slot.agent_pid.take();
            slot.apps_started = None;
            self.drop_credprov_seat(&mut st, seat);
            let Some(slot) = st.seats.get_mut(&seat) else { return };
            if let Some(s) = slot.rt.session_id {
                self.platform.drop_session_job(s);
            }
            slot.launch = None;
            slot.outputs = None;
            slot.audio = None;
            slot.logon = None;
            slot.rt.session_id = None;
            slot.rt.user_sid = None;
            slot.rt.degraded.clear();
            st.run.known_sessions.retain(|k| k.seat != seat);
            st.run.set_host(seat, None);
            st.ownership_dirty = true;
            self.save_run(&st);
            self.recompute_product(&mut st);
            (host, agent)
        };
        if let Some(pid) = host {
            if wait_host {
                let grace = Duration::from_millis(self.host_grace_ms.load(std::sync::atomic::Ordering::Relaxed));
                let end = std::time::Instant::now() + grace;
                while self.platform.process_alive(pid) && std::time::Instant::now() < end {
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
            if self.platform.process_alive(pid) {
                self.platform.terminate(pid);
            }
        }
        if let Some(pid) = agent
            && self.platform.process_alive(pid)
        {
            self.platform.terminate(pid);
        }
        let _ = b.audio.release_seat(seat);
        let _ = b.display.release_seat(seat);
    }

    /// Re-logon: end the session and start the seat again (spec 10 §8.5).
    pub(crate) fn relogon(&self, seat: SeatNumber) -> Result<(), ProtoError> {
        if !Self::managed(&self.lock(), seat) {
            let console = self.platform.console_session().ok_or_else(|| ProtoError::new(ErrorCode::NotReady, "no console session"))?;
            return self.platform.logoff_session(console).map_err(ProtoError::from);
        }
        self.spawn(move |s| {
            s.stop_seat_now(seat, true);
            let delay = u64::from(s.lock().config.settings.timings.relogon_delay_ms);
            if delay > 0 {
                std::thread::sleep(Duration::from_millis(delay));
            }
            s.start_sequence(vec![seat]);
        });
        Ok(())
    }

    // ---- session changes ----------------------------------------------------------

    /// SCM session-change notification (WTS).
    pub fn on_session_change(&self, change: SessionChange, session_id: u32) {
        if matches!(change, SessionChange::Logon | SessionChange::Logoff | SessionChange::Terminate) {
            // A one-time credential never outlives the logon screen it was armed for.
            self.drop_credprov_grant(session_id);
        }
        if matches!(change, SessionChange::Logoff | SessionChange::Terminate) {
            // Every logon gets a fresh seat job (console seat included): Windows reuses
            // session ids, and a hard CPU limit cannot be cleared from a job, so a job kept
            // past its logon would confine the next seat's processes.
            self.platform.drop_session_job(session_id);
            // Startup apps are per logon: the next logon in this session starts them again.
            {
                let mut st = self.lock();
                for s in st.seats.values_mut().filter(|s| s.apps_started == Some(session_id)) {
                    s.apps_started = None;
                }
            }
            self.spawn_retry_deferred();
        }
        let console = self.platform.console_session();
        let dedicated = Self::dedicated(&self.lock());
        // `fast_user_switch` with seat 1 off the console: the console session is another
        // seat's, so its events go to that seat below, not to seat 1.
        let seat1_away = self.backends().session.console_seat_away().is_some();
        let console_event = !seat1_away && (console == Some(session_id) || change == SessionChange::ConsoleConnect);
        if seat1_away && change == SessionChange::ConsoleConnect && !dedicated {
            self.refresh_console_seat();
        }
        if console_event && change == SessionChange::Logon && self.take_seat_one_wait() {
            // `after_seat_one_logon`: somebody signed in at the console (the host account
            // under `dedicated_host`).
            self.spawn(|s| s.start_autostart_seats());
        }
        if change == SessionChange::ConsoleConnect && self.backends().session.kind() == seat_core::backend::SessionBackendKind::FastUserSwitch {
            self.queue_switch_topology(session_id);
        }
        if console_event && !dedicated {
            self.refresh_console_seat();
            if change == SessionChange::Logon
                && console == Some(session_id)
                && let Some(u) = self.platform.session_user(session_id)
            {
                self.on_seat_logon(SeatNumber::CONSOLE, session_id, u);
            }
            if change == SessionChange::Logoff {
                self.lock().ownership_dirty = true;
            }
            return;
        }
        // dedicated_host: the console is the host account's session, not a seat; seat
        // sessions (seat 1 included) are found by their session id below.
        let seat = {
            let st = self.lock();
            st.seats.values().find(|s| Self::managed(&st, s.seat) && s.rt.session_id == Some(session_id)).map(|s| s.seat)
        };
        let Some(seat) = seat else { return };
        match change {
            SessionChange::Logon => {
                if let Some(u) = self.platform.session_user(session_id) {
                    {
                        let mut st = self.lock();
                        let name = u.display();
                        self.set_seat_state(&mut st, seat, SeatState::SignedIn { user: name });
                    }
                    self.on_seat_logon(seat, session_id, u);
                }
            }
            SessionChange::Lock | SessionChange::Unlock => {
                let mut st = self.lock();
                let user = st.seats.get(&seat).and_then(|s| s.logon.as_ref()).map(SessionUser::display).unwrap_or_default();
                let state = if change == SessionChange::Lock { SeatState::Locked { user } } else { SeatState::SignedIn { user } };
                self.set_seat_state(&mut st, seat, state);
            }
            SessionChange::Logoff => {
                let mut st = self.lock();
                st.deny_logoff.remove(&session_id);
                if let Some(slot) = st.seats.get_mut(&seat) {
                    slot.logon = None;
                    slot.rt.user_sid = None;
                }
                self.set_seat_state(&mut st, seat, SeatState::AtLogon);
                st.ownership_dirty = true;
            }
            SessionChange::Terminate => self.on_session_gone(seat),
            SessionChange::ConsoleConnect => {}
        }
    }

    /// A seat's user signed in: last user, same-account policy, agent, gate re-plan.
    pub(crate) fn on_seat_logon(&self, seat: SeatNumber, session_id: u32, user: SessionUser) {
        let agent_needed = {
            let mut st = self.lock();
            if let Some(slot) = st.seats.get_mut(&seat) {
                slot.logon = Some(user.clone());
                slot.rt.user_sid = Some(user.sid.clone());
            }
            st.run.last_user_sid.insert(seat, user.sid.clone());
            // A successful logon clears a failing-password marker.
            st.run.failing_passwords.retain(|s| *s != seat);
            self.save_run(&st);
            let others: Vec<SeatNumber> = st
                .seats
                .values()
                .filter(|s| s.seat != seat && s.logon.as_ref().is_some_and(|u| u.sid.eq_ignore_ascii_case(&user.sid)))
                .map(|s| s.seat)
                .collect();
            if !others.is_empty() {
                match st.config.settings.same_account {
                    SameAccountPolicy::Allow => {}
                    SameAccountPolicy::Warn => {
                        if let Some(s) = st.seats.get_mut(&seat)
                            && !s.rt.degraded.iter().any(|d| d == "same_account_warning")
                        {
                            s.rt.degraded.push("same_account_warning".into());
                        }
                        self.queue_agent_task(&mut st, session_id, AgentTaskKind::ShowNotice { code: "same_account_warning".into() });
                    }
                    SameAccountPolicy::Deny if Self::managed(&st, seat) => {
                        // The agent only shows the notice (UI); the service itself logs the
                        // session off when the timer is due (`enforce_deny_logoffs`).
                        self.queue_agent_task(&mut st, session_id, AgentTaskKind::ShowNotice { code: "same_account_denied".into() });
                        self.queue_agent_task(&mut st, session_id, AgentTaskKind::LogOff { after_ms: DENY_LOGOFF_AFTER_MS });
                        let due = self.now() + u64::from(DENY_LOGOFF_AFTER_MS);
                        st.deny_logoff.insert(session_id, (due, user.sid.clone()));
                        self.set_seat_state(
                            &mut st,
                            seat,
                            SeatState::Error { reason: FailureReason::SameAccountDenied, detail: "same_account".into() },
                        );
                    }
                    SameAccountPolicy::Deny => {}
                }
            }
            let cfg = st.config.seat(seat).cloned();
            if let Some(c) = &cfg {
                if c.wallpaper {
                    // Our generated image (written now if missing); no task for a missing file.
                    match crate::images::ensure_seat_image(&self.data_dir, seat) {
                        Ok(path) => {
                            self.queue_agent_task(&mut st, session_id, AgentTaskKind::SetWallpaper { path: path.display().to_string() })
                        }
                        Err(e) => log::warn(&format!("seat {seat}: wallpaper image not written: {e}")),
                    }
                }
                if let Some(t) = st.config.settings.tweaks.touchpad_palm_check {
                    self.queue_agent_task(&mut st, session_id, AgentTaskKind::SetPalmCheck { threshold: t });
                }
            }
            st.ownership_dirty = true;
            st.seats.get(&seat).is_some_and(|s| s.agent_pid.is_none_or(|p| !self.platform.process_alive(p)))
        };
        let (job, addr, apps) = {
            let mut st = self.lock();
            let cfg = st.config.seat(seat).cloned();
            let programs = st.config.settings.ip_programs.clone();
            let job = cfg.as_ref().map(|c| SeatJobSpec { cpu_sets: c.cpu_sets.clone(), hard_limit: c.cpu_hard_limit });
            let addr = cfg.as_ref().and_then(|c| seat_compat::srcip::from_config(c, &programs));
            // Startup apps once per logon session (a re-report of the same logon does not
            // start them twice).
            let apps = match st.seats.get_mut(&seat) {
                Some(s) if s.apps_started != Some(session_id) => {
                    s.apps_started = Some(session_id);
                    cfg.map(|c| c.startup_apps).unwrap_or_default()
                }
                _ => Vec::new(),
            };
            (job, addr, apps)
        };
        if agent_needed {
            let exe = self.platform.install_dir().join(AGENT_EXE);
            let spec = LaunchSpec {
                session_id,
                // Never the source-IP variable: the agent's children would all inherit it,
                // whatever the seat's rule says about them (srcip.rs, limits).
                env: Vec::new(),
                exe,
                args: Vec::new(),
                log_name: format!("agent-{session_id}.log"),
                as_system: false,
                // The agent and everything it starts run in the seat's job.
                job: job.clone(),
                stdin_secret: None,
                service_child: true,
                host_clipboard: false,
                host_seat: None,
                host_profile: false,
            };
            match self.platform.launch(&spec) {
                Ok(pid) => {
                    if let Some(s) = self.lock().seats.get_mut(&seat) {
                        s.agent_pid = Some(pid);
                    }
                }
                Err(e) => log::warn(&format!("seat {seat}: agent not started: {e}")),
            }
        }
        for (i, app) in apps.into_iter().enumerate() {
            let exe = std::path::PathBuf::from(app.command.trim());
            if !exe.is_absolute() {
                // Never resolve a bare name through a search path from a SYSTEM service.
                log::warn(&format!("seat {seat}: startup app {i} skipped: path is not absolute"));
                continue;
            }
            let spec = LaunchSpec {
                session_id,
                env: seat_compat::srcip::launch_env(addr.as_ref(), &exe.display().to_string()).into_iter().collect(),
                exe,
                args: app.args.clone(),
                log_name: format!("app-{session_id}-{i}.log"),
                as_system: false,
                job: job.clone(),
                stdin_secret: None,
                service_child: false,
                host_clipboard: false,
                host_seat: None,
                host_profile: false,
            };
            match self.platform.launch(&spec) {
                Ok(pid) => log::info(&format!("seat {seat}: startup app {i} started (pid {pid})")),
                Err(e) => log::warn(&format!("seat {seat}: startup app {i} not started: {e}")),
            }
        }
    }

    /// The seat's CPU limits as a job spec.
    pub(crate) fn seat_job_spec(st: &State, seat: SeatNumber) -> Option<SeatJobSpec> {
        st.config.seat(seat).map(|c| SeatJobSpec { cpu_sets: c.cpu_sets.clone(), hard_limit: c.cpu_hard_limit })
    }

    /// Session of a seat right now: the started session, or the console session for the
    /// console seat.
    pub(crate) fn seat_session_now(&self, st: &State, seat: SeatNumber) -> Option<u32> {
        if Self::managed(st, seat) {
            st.seats.get(&seat).and_then(|s| s.rt.session_id)
        } else if seat == SeatNumber::CONSOLE {
            self.platform.console_session()
        } else {
            None
        }
    }

    /// The config changed: running seats whose CPU limits changed get them now (the job
    /// of their session, spec 80 §11.2). Called with the old config still in `old`.
    pub(crate) fn update_seat_jobs(&self, st: &State, old: &seat_core::config::Config) {
        for c in &st.config.seats {
            let before = old.seat(c.number).map(|o| (o.cpu_sets.clone(), o.cpu_hard_limit));
            if before == Some((c.cpu_sets.clone(), c.cpu_hard_limit)) {
                continue;
            }
            let (Some(session), Some(spec)) = (self.seat_session_now(st, c.number), Self::seat_job_spec(st, c.number)) else {
                continue;
            };
            match self.platform.update_session_job(session, &spec) {
                Ok(notes) => {
                    for n in notes {
                        log::info(&format!("seat {}: cpu limit: {n}", c.number));
                    }
                }
                Err(e) => log::warn(&format!("seat {}: cpu limit not updated: {e}", c.number)),
            }
        }
    }

    /// Launcher request (`start_seat_app`, spec 80 §11.2): start a program in the caller's
    /// own seat session, as the session user, inside the seat's job (CPU limits) and with
    /// the seat's source-IP variable when the rule picks it. Only the session's signed-in
    /// user may ask (a process of another account in the same session may not); the path
    /// must be absolute (never resolved through a search path by a SYSTEM service).
    pub(crate) fn start_seat_app(&self, caller: &crate::auth::Caller, path: &str, args: &[String]) -> Result<u32, ProtoError> {
        if caller.client.is_none() {
            return Err(ProtoError::new(ErrorCode::BadRequest, "hello must be the first request"));
        }
        let exe = std::path::PathBuf::from(path.trim());
        if path.trim().is_empty() || !exe.is_absolute() || path.trim().starts_with(r"\\") {
            return Err(ProtoError::new(ErrorCode::InvalidValue, "path must be an absolute local path").with_field("path"));
        }
        if args.len() > MAX_APP_ARGS || args.iter().map(|a| a.len() + 3).sum::<usize>() + path.len() > MAX_APP_COMMAND_LINE {
            return Err(ProtoError::new(ErrorCode::InvalidValue, "command line too long").with_field("args"));
        }
        let (seat, job, addr) = {
            let mut st = self.lock();
            if !st.config.settings.enabled {
                return Err(ProtoError::new(ErrorCode::NotReady, "product is disabled"));
            }
            let seat = self
                .seat_of_session(&st, caller.session_id)
                .ok_or_else(|| ProtoError::new(ErrorCode::AccessDenied, "caller is not in a seat session"))?;
            // Rate and concurrency limits per session: a seat user looping the request
            // must not grow the SYSTEM service's handle table or the session's process count.
            let now = self.now();
            let starts = st.app_launches.entry(caller.session_id).or_default();
            while starts.front().is_some_and(|t| now.saturating_sub(*t) >= APP_RATE_WINDOW_MS) {
                starts.pop_front();
            }
            if starts.len() >= MAX_APP_STARTS_PER_WINDOW {
                return Err(ProtoError::new(ErrorCode::NotReady, "too many app starts; try again in a few seconds"));
            }
            if self.platform.live_app_count(caller.session_id) >= MAX_LIVE_APPS_PER_SESSION {
                return Err(ProtoError::new(ErrorCode::NotReady, "too many apps started through the launcher are running"));
            }
            starts.push_back(now);

            let programs = st.config.settings.ip_programs.clone();
            let addr = st.config.seat(seat).and_then(|c| seat_compat::srcip::from_config(c, &programs));
            (seat, Self::seat_job_spec(&st, seat), addr)
        };
        let user = self
            .platform
            .session_user(caller.session_id)
            .ok_or_else(|| ProtoError::new(ErrorCode::NotReady, "nobody is signed in to this seat"))?;
        if !caller.user_sid.as_deref().is_some_and(|s| s.eq_ignore_ascii_case(&user.sid)) {
            return Err(ProtoError::new(ErrorCode::AccessDenied, "only the seat's signed-in user may start apps in it"));
        }
        let spec = LaunchSpec {
            session_id: caller.session_id,
            env: seat_compat::srcip::launch_env(addr.as_ref(), &exe.display().to_string()).into_iter().collect(),
            exe,
            args: args.to_vec(),
            log_name: format!("app-{}-launcher.log", caller.session_id),
            as_system: false,
            job: Some(job.unwrap_or_default()),
            stdin_secret: None,
            service_child: false,
            host_clipboard: false,
            host_seat: None,
            host_profile: false,
        };

        let pid = self.platform.launch(&spec).map_err(ProtoError::from)?;
        log::info(&format!("seat {seat}: app started through the launcher (pid {pid})"));
        Ok(pid)
    }

    /// Same-account `deny`: log off every session whose timer is due and that still has
    /// the denied user, whatever the agent did or reported.
    pub(crate) fn enforce_deny_logoffs(&self, now: u64) {
        let due: Vec<(u32, String)> = {
            let mut st = self.lock();
            let due: Vec<(u32, String)> =
                st.deny_logoff.iter().filter(|(_, (at, _))| now >= *at).map(|(s, (_, sid))| (*s, sid.clone())).collect();
            for (s, _) in &due {
                st.deny_logoff.remove(s);
            }
            due
        };
        for (session, sid) in due {
            let same_user = self.platform.session_user(session).is_some_and(|u| u.sid.eq_ignore_ascii_case(&sid));
            if !same_user {
                continue;
            }
            log::info(&format!("session {session}: same account denied, logging off"));
            if let Err(e) = self.platform.logoff_session(session) {
                log::warn(&format!("session {session}: same-account log off: {e}"));
            }
        }
    }

    /// The seat's session ended: return to the logon screen after the re-logon delay.
    pub(crate) fn on_session_gone(&self, seat: SeatNumber) {
        let want = self.lock().seats.get(&seat).is_some_and(|s| s.want);
        log::info(&format!("seat {seat}: session ended"));
        self.cleanup_seat(seat, false, None);
        let mut st = self.lock();
        let delay = u64::from(st.config.settings.timings.relogon_delay_ms);
        let now = self.now();
        if let Some(slot) = st.seats.get_mut(&seat) {
            slot.generation += 1;
            if want {
                slot.retry_at = Some(now + delay);
            }
        }
        let state = if want { SeatState::Starting { phase: StartPhase::Queued } } else { SeatState::Off };
        self.set_seat_state(&mut st, seat, state);
    }

    // ---- watchdog -----------------------------------------------------------------

    /// Host crashes (restart with back-off, give up after [`CRASH_LIMIT`]), due retries,
    /// and a periodic session query.
    pub(crate) fn watchdog(&self, now: u64) {
        let mut restart = Vec::new();
        let mut poll = Vec::new();
        let mut timed_out = Vec::new();
        {
            let mut st = self.lock();
            let budget = Self::start_budget_ms(&st);
            let poll_due = now.saturating_sub(st.last_session_poll_ms) >= SESSION_POLL_MS;
            if poll_due {
                st.last_session_poll_ms = now;
            }
            let seats: Vec<SeatNumber> = st.seats.keys().copied().collect();
            for seat in seats {
                let Some(slot) = st.seats.get(&seat).cloned() else { continue };
                if !Self::managed(&st, seat) {
                    continue;
                }
                // A start that hangs in a backend call: fail it with its step, bump the
                // generation (the stuck worker abandons when it returns) and release.
                // `queued` = not picked up by a worker yet (a sequence waits for earlier
                // seats); the time limit starts when the worker begins the seat.
                if let (SeatState::Starting { phase }, None, Some(deadline)) = (&slot.rt.state, slot.retry_at, slot.deadline_ms)
                    && *phase != StartPhase::Queued
                    && now >= deadline
                {
                    let e = BackendError::new(ErrorKind::Timeout, "session_timeout", "seat start exceeded its time limit");
                    let detail = Self::failure_detail(&slot.rt.state, &e);
                    let g = {
                        let s = st.seats.get_mut(&seat).expect("present");
                        s.generation += 1;
                        s.want = false;
                        s.generation
                    };
                    self.set_seat_state(&mut st, seat, SeatState::Error { reason: FailureReason::SessionTimeout, detail });
                    timed_out.push((seat, g));
                    continue;
                }
                if let Some(h) = &slot.host
                    && slot.want
                    && !self.platform.process_alive(h.pid)
                {
                    let mut crashes = slot.crashes.clone();
                    crashes.retain(|t| now.saturating_sub(*t) < CRASH_WINDOW_MS);
                    crashes.push(now);
                    log::error_id(eventid::SEAT_HOST_EXITED, &format!("seat {seat}: host exited (pid {})", h.pid));
                    let give_up = crashes.len() >= CRASH_LIMIT;
                    let next = now + backoff_ms(crashes.len());
                    if let Some(s) = st.seats.get_mut(&seat) {
                        s.crashes = crashes;
                        s.host = None;
                        s.generation += 1;
                        if give_up {
                            s.want = false;
                        } else {
                            s.retry_at = Some(next);
                        }
                    }
                    let state = if give_up {
                        SeatState::Error { reason: FailureReason::HostExited, detail: format!("{CRASH_LIMIT} exits") }
                    } else {
                        SeatState::Starting { phase: StartPhase::Queued }
                    };
                    self.set_seat_state(&mut st, seat, state);
                    restart.push((seat, true));
                    continue;
                }
                if let Some(at) = slot.retry_at
                    && now >= at
                {
                    if let Some(s) = st.seats.get_mut(&seat) {
                        s.retry_at = None;
                        // The start begins now: a fresh time limit.
                        s.deadline_ms = Some(now + budget);
                    }
                    restart.push((seat, false));
                    continue;
                }
                if poll_due
                    && let Some(id) = slot.rt.session_id
                    && matches!(slot.rt.state, SeatState::AtLogon | SeatState::SignedIn { .. } | SeatState::Locked { .. })
                {
                    poll.push((seat, id));
                }
            }
        }
        for (seat, g) in timed_out {
            log::error_id(eventid::SEAT_START_FAILED, &format!("seat {seat}: start timed out"));
            self.cleanup_seat(seat, false, Some(g));
        }
        for (seat, crashed) in restart {
            if crashed {
                // Release what the dead host held; the session (if any) is logged off so
                // the restart gets a fresh one.
                let (session, retry, g) = {
                    let st = self.lock();
                    let s = st.seats.get(&seat);
                    (s.and_then(|s| s.rt.session_id), s.and_then(|s| s.retry_at), s.map(|s| s.generation))
                };
                if let Some(id) = session {
                    let _ = self.backends().session.log_off(seat, id);
                }
                self.cleanup_seat(seat, false, g);
                if let Some(s) = self.lock().seats.get_mut(&seat).filter(|s| Some(s.generation) == g) {
                    s.retry_at = retry;
                }
            } else {
                let mut st = self.lock();
                let Some(slot) = st.seats.get_mut(&seat) else { continue };
                slot.generation += 1;
                slot.want = true;
                let g = slot.generation;
                self.set_seat_state(&mut st, seat, SeatState::Starting { phase: StartPhase::Queued });
                drop(st);
                self.spawn(move |s| s.run_start(seat, g));
            }
        }
        let b = self.backends();
        for (seat, id) in poll {
            match b.session.query(seat, id) {
                Ok(info) if info.state == SessionState::Gone => self.on_session_gone(seat),
                Ok(info) => {
                    let mut st = self.lock();
                    let user = st.seats.get(&seat).and_then(|s| s.logon.clone());
                    let state = map_session_state(info.state, user.as_ref());
                    if !matches!(st.seats.get(&seat).map(|s| &s.rt.state), Some(SeatState::Error { .. })) {
                        self.set_seat_state(&mut st, seat, state);
                    }
                }
                Err(e) if e.kind == ErrorKind::NotFound => self.on_session_gone(seat),
                Err(e) => log::debug(&format!("seat {seat}: query: {e}")),
            }
        }
    }

    /// A host reported its connection state (`host_report`).
    /// A seat host's `host_report`. `rdp_client` = `"ironrdp"` / `"activex"` (older hosts
    /// send none). The reported session id and the client kind go to the session backend
    /// as a [`HostHint`](seat_core::backend::session::HostHint): it binds the seat's session
    /// only when the id is both reported and confirmed by WTS, and allows the "newest new
    /// loopback session" fallback only for the ActiveX client.
    pub(crate) fn on_host_report(&self, seat: SeatNumber, state: &seat_proto::HostState, rdp_client: Option<&str>) {
        use seat_proto::HostState as H;
        let reported = match state {
            H::SignedIn { session_id } => *session_id,
            _ => None,
        };
        let legacy_client = rdp_client == Some("activex");
        if reported.is_some() || legacy_client {
            if let Some(slot) = self.lock().seats.get_mut(&seat)
                && reported.is_some()
            {
                slot.reported_session = reported;
            }
            self.backends().session.host_hint(seat, seat_core::backend::session::HostHint { session_id: reported, legacy_client });
        }
        match state {
            H::Connected | H::SignedIn { .. } => {
                if let Some(h) = self.lock().seats.get_mut(&seat).and_then(|s| s.host.as_mut()) {
                    h.connected = true;
                }
            }
            H::Connecting | H::Disconnected { .. } => {}
            H::Failed { reason } => {
                let generation = self.lock().seats.get(&seat).map(|s| s.generation).unwrap_or(0);
                let kind = if reason == "logon_failed" { ErrorKind::Logon } else { ErrorKind::Internal };
                self.fail(seat, generation, &BackendError::new(kind, reason.clone(), "seat host reported a failure"));
            }
        }
    }

    /// `fast_user_switch` (spec 35 §5.6): `session_id` just became the console. Its seat's
    /// monitors (present, effective mask with the seat, CCD target of this boot) go to the
    /// session's agent as `apply_topology`; the agent keeps only those in the desktop. No
    /// task when the session is no seat's or the seat has no present monitor (never black
    /// out the console). Untested on hardware.
    pub(crate) fn queue_switch_topology(&self, session_id: u32) {
        let mut st = self.lock();
        let Some(seat) = self.seat_of_session(&st, session_id) else { return };
        let mine: Vec<DeviceKey> = st
            .inventory
            .effective(&st.config)
            .into_iter()
            .filter(|e| e.kind == DeviceKind::Monitor && e.mask.contains(seat))
            .map(|e| e.key)
            .collect();
        let targets: Vec<seat_proto::DisplayTargetRef> = mine
            .iter()
            .filter_map(|k| st.inventory.get(k))
            .filter_map(|e| match &e.device.details {
                seat_core::device::DeviceDetails::Display(d) => {
                    Some(seat_proto::DisplayTargetRef { adapter_luid: d.adapter_luid, target_id: d.target_id })
                }
                _ => None,
            })
            .collect();
        if targets.is_empty() {
            log::info(&format!("seat {seat}: no monitor known for the switch topology; left as Windows restored it"));
            return;
        }
        self.queue_agent_task(&mut st, session_id, AgentTaskKind::ApplyTopology { targets });
    }

    /// Credentials stored for a seat changed: clear the failing marker.
    pub(crate) fn password_changed(&self, seat: SeatNumber) {
        let mut st = self.lock();
        st.run.failing_passwords.retain(|s| *s != seat);
        self.save_run(&st);
    }

    pub(crate) fn account_of(st: &State, seat: SeatNumber) -> SeatAccount {
        st.config.seat(seat).map(|s| s.account.clone()).unwrap_or_default()
    }
}

impl State {
    pub(crate) fn run_hosts(&self) -> Vec<(SeatNumber, HostProc)> {
        self.run.hosts.iter().map(|(s, h)| (*s, h.clone())).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_caps() {
        assert_eq!(backoff_ms(1), 2_000);
        assert_eq!(backoff_ms(2), 4_000);
        assert_eq!(backoff_ms(10), MAX_BACKOFF_MS);
    }

    #[test]
    fn reasons() {
        assert_eq!(failure_reason(&BackendError::new(ErrorKind::Logon, "bad_password", "")), FailureReason::LogonFailed);
        assert_eq!(failure_reason(&BackendError::new(ErrorKind::NotReady, "edition_unsupported", "")), FailureReason::EditionUnsupported);
        assert_eq!(failure_reason(&BackendError::new(ErrorKind::Timeout, "x", "")), FailureReason::SessionTimeout);
        assert_eq!(client_name(SeatNumber::new(3).unwrap()), "PSEAT-03");
    }
}
