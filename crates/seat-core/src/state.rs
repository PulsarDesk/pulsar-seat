// SPDX-License-Identifier: GPL-3.0-only
//! Run-time states (spec 30 §6.2.18, spec 20 §7.1). Held in the service's memory and served
//! over the pipe; never written to volatile registry keys.

use crate::ids::SeatNumber;
use serde::{Deserialize, Serialize};

/// Product-wide state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ProductState {
    /// `settings.enabled == false`: the service only serves status and config.
    Disabled,
    /// Enabled; `started` = at least one extra seat has a session.
    Enabled { started: bool },
    /// Seat 1 only, extra seats are not started automatically; the reason is shown.
    SafeMode { reason: SafeModeReason },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SafeModeReason {
    /// Windows booted in Safe Mode: do nothing but serve status.
    WindowsSafeMode,
    /// Previous shutdown was not clean (crash, power loss) and the guard is on.
    UncleanShutdown,
    /// The service failed to start `crash_loop_limit` times in a row.
    CrashLoop,
}

/// One seat's state as the manager shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SeatState {
    /// Not running (or not configured to start).
    Off,
    /// Set to start but none of its monitors is present; not a failure.
    WaitingForDisplay,
    Starting { phase: StartPhase },
    /// Session exists, at the logon screen.
    AtLogon,
    SignedIn { user: String },
    Locked { user: String },
    Stopping,
    Error { reason: FailureReason, detail: String },
}

/// Progress of a seat start (shown as a step text).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartPhase {
    Queued,
    WaitingForHost,
    BindingDisplay,
    LaunchingHost,
    Connecting,
    WaitingForSession,
    LoggingOn,
}

/// Stable snake_case failure reasons; the manager maps each to Turkish copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureReason {
    /// Edition / role cannot host the backend (Home, client SKU without wrapper, …).
    EditionUnsupported,
    /// RDSH not ready (role, listener, licensing).
    RdsNotReady,
    /// More seats than the backend allows on this PC.
    SeatLimit,
    /// Account missing / disabled / logon hours.
    Account,
    /// Wrong or expired password; stored password marked failing (no retry loop).
    LogonFailed,
    /// NLA / CredSSP refused the loopback connection.
    ClientAuth,
    /// Seat host crashed repeatedly.
    HostExited,
    SessionTimeout,
    /// No monitor bound (display backend refused all targets).
    Display,
    /// Our driver missing / unsigned / wrong version for the chosen backend.
    Driver,
    /// Reference product or foreign TermService DLL present.
    Coexistence,
    /// Same account already on another seat and policy = deny.
    SameAccountDenied,
    /// A Windows policy blocks it.
    Policy,
    Internal,
    // ---- added 2026-11-13 (additive; older managers show the raw code) ----
    /// Fast user switching: the console has a signed-in, unlocked user (lock it first).
    ConsoleInUse,
    /// Fast user switching: another seat is on the console right now.
    ConsoleBusy,
    /// Fast user switching: no stored password and the target session is not locked.
    SessionNotLocked,
    /// The RDP listener needs NLA, but a logon-screen (passwordless) seat cannot do NLA,
    /// or stored credentials would go to a listener without NLA.
    NlaRequired,
    /// Kiosk seat without a kiosk app.
    KioskAppMissing,
    /// No console session yet (boot not finished, between sessions).
    ConsoleNotReady,
    /// The session found is not the seat's (client name / user / address check failed).
    NotOurSession,
    /// Fast user switching is hidden or turned off by policy.
    FsuDisabled,
    /// The second, loopback-only listener for seats without a stored password is not
    /// ready; the seat's `detail` carries `listener=loopback_listener:<state>[:<x>]`
    /// (added 2026-11-13, fix-second-listener).
    LoopbackListenerUnavailable,
}

/// One seat's run-time record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatRuntime {
    pub seat: SeatNumber,
    /// Kind of the seat (protocol 3, architecture.md section 10.2). Missing = local.
    #[serde(default)]
    pub kind: crate::config::SeatKind,
    pub state: SeatState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_sid: Option<String>,
    /// Service clock ms of the last state change.
    pub since_ms: u64,
    /// Snake_case degradations shown next to the seat (`input_not_isolated`,
    /// `display_fallback`, `audio_console_shared`, `same_account_warning`, …).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub degraded: Vec<String>,
    /// Snake_case notes of the seat's resolved redirection (`HostRedirection.notes`:
    /// `clipboard_shared_with_host_session`, `no_seat_drives`, ...), for the manager. Added
    /// 2026-11-13 (additive).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub redirect_notes: Vec<String>,
    /// Latest redirection refusals the seat host reported (newest last, at most
    /// [`MAX_REDIRECT_REFUSALS`]). Added 2026-11-13 (additive).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub redirect_refusals: Vec<RedirectRefusal>,
}

/// How many redirection refusals a seat keeps.
pub const MAX_REDIRECT_REFUSALS: usize = 10;

/// One refused clipboard / drive / smart-card request of a seat session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedirectRefusal {
    /// `clipboard`, `drives`, `smartcard`.
    pub channel: String,
    /// Snake_case reason (`format_not_allowed_to_seat`, `reader_not_assigned`, ...).
    pub reason: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
    /// Same refusals not reported since the previous one (rate limit).
    #[serde(default)]
    pub suppressed: u32,
    /// Service clock ms.
    #[serde(default)]
    pub at_ms: u64,
}

impl SeatRuntime {
    pub fn off(seat: SeatNumber) -> Self {
        Self {
            seat,
            kind: crate::config::SeatKind::Local,
            state: SeatState::Off,
            session_id: None,
            user_sid: None,
            since_ms: 0,
            degraded: Vec::new(),
            redirect_notes: Vec::new(),
            redirect_refusals: Vec::new(),
        }
    }
}
