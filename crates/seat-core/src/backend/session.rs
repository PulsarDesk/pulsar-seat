// SPDX-License-Identifier: GPL-3.0-only
//! Session backend: how an extra seat gets its own Windows session (spec 30 §6, spec 00
//! §5.2). Implemented in `seat-session`.
//!
//! Start of one seat, as the service drives it:
//!
//! 1. `probe()` once at boot (and on request) → availability on this PC.
//! 2. `prepare_host(settings)` once before the first seat (documented policy values,
//!    journaled so `restore_host` can undo them exactly).
//! 3. `start(req)`:
//!    - `StartStep::LaunchHost(spec)`: the backend needs a seat host (RDP backends). The
//!      service launches `pulsar-seat-host.exe` in `spec.target` (seat-session's launcher),
//!      hands it the credentials once over the pipe, then calls `wait_session`.
//!    - `StartStep::Started(info)`: the session exists already (fast user switch, desktops).
//! 4. `wait_session(seat, client_name, timeout)` → the seat's session (matched by our RDP
//!    client name `PSEAT-NN` + user, never Services / Console / listeners).
//! 5. During the seat's life: `query`, and the service's own WTS session-change handling.
//! 6. `log_off(seat, session_id)` for stop / force re-logon.
//! 7. After a service restart: `adopt(known)` re-binds running sessions (spec 30 §6.2.14).

use super::{Availability, BackendResult};
use crate::config::{GlobalSettings, SeatConfig};
use crate::ids::SeatNumber;
use crate::secret::Secret;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Which session backend (spec 30 §6.1 options; plan.md decisions).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionBackendKind {
    /// Option A: RDP loopback sessions on Windows Server with the RD Session Host role.
    /// Licence-clean; the default where it is available (x64 Server in practice).
    #[default]
    RdsLoopback,
    /// EXPERIMENTAL, off by default: client-SKU multi-session through a TermService
    /// wrapper the *operator* installed (we ship none, never patch termsrv / LSM). Needs
    /// the manager's warning acknowledged ([`crate::config::BackendSelection::experimental_ack`]).
    /// Seats are RDP loopback sessions exactly like `RdsLoopback`.
    TermsrvWrapper,
    /// Option C: one active session at a time; seats take turns on the console.
    FastUserSwitch,
    /// Option D: kiosk / guest seats as apps on the seat's monitors inside the console
    /// session (no per-user isolation).
    Desktops,
}

impl SessionBackendKind {
    pub const fn is_experimental(self) -> bool {
        matches!(self, Self::TermsrvWrapper)
    }
    /// RDP-based backends need a seat host per seat.
    pub const fn uses_rdp(self) -> bool {
        matches!(self, Self::RdsLoopback | Self::TermsrvWrapper)
    }
    /// Remote seats need concurrent per-user sessions: Server + RDSH (`rds_loopback`) or the
    /// wrapper kind. `fast_user_switch` (U1) and `desktops` (U3) are local only; a remote
    /// seat start under them fails with `session_unavailable` (architecture.md section 10).
    pub const fn hosts_remote_seats(self) -> bool {
        self.uses_rdp()
    }
}

/// Where seat hosts run under the RDP backends (spec 00 T6, spec 30 §6.1.1 point 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostTopology {
    /// Seat hosts run in seat 1's own console session. Seat 1 keeps native GPU speed, but
    /// its lock / UAC / sign-out hide the other seats and, without our signed input
    /// filter, other seats' keys also reach seat 1's foreground window. Default (no
    /// Winlogon change).
    #[default]
    SeatOneConsole,
    /// A dedicated host account owns the console session and runs only the seat hosts;
    /// every seat including seat 1 is an RDP session. User-mode input isolation works
    /// here (raw input into our own windows only). Needs the documented auto-logon of the
    /// host account (opt-in, maintainer decision).
    DedicatedHost,
}

/// What a session backend can do (manager greys out the rest).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCaps {
    /// Several seats signed in at the same time.
    pub concurrent: bool,
    /// Seats this backend can run on this PC (licence / listener cap); 1 = console only.
    pub max_seats: u8,
    /// Each seat has its own Windows user session (profiles, logon screen).
    pub per_user: bool,
    /// Needs `pulsar-seat-host.exe` per seat.
    pub needs_seat_host: bool,
    /// Session has a remote-protocol display of its own (frames come through RDP).
    pub remote_display: bool,
    /// Session has remote-protocol input channels (keyboard, mouse, touch, pen).
    pub remote_input: bool,
    /// Session has a remote-protocol audio channel (per-seat audio, spec 60 design 3).
    pub remote_audio: bool,
    /// Per-seat automatic logon with the stored account.
    pub auto_logon: bool,
    /// A single seat can be stopped / re-logged on without touching others.
    pub stop_one: bool,
}

/// Result of probing the PC for one backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostProbe {
    pub kind: SessionBackendKind,
    pub availability: Availability,
    /// `EditionID` / product name, e.g. `ServerStandard`, `Professional`.
    pub edition: String,
    pub build: u32,
    /// `arm64` / `x64`.
    pub arch: String,
    /// Snake_case notes for the manager (`rds_grace_period`, `fsu_policy_hidden`, …).
    pub notes: Vec<String>,
}

/// Credentials of a seat's account, read from the [`crate::store::SecretStore`] only for
/// the start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeatCredentials {
    pub domain: String,
    pub user: String,
    /// `None` = no stored password: show the seat's logon prompt.
    pub password: Option<Secret>,
}

/// Input to [`SessionBackend::start`].
#[derive(Debug, Clone)]
pub struct SeatStartRequest<'a> {
    pub seat: SeatNumber,
    pub config: &'a SeatConfig,
    pub settings: &'a GlobalSettings,
    /// `PSEAT-NN`, see [`crate::consts::RDP_CLIENT_NAME_PREFIX`].
    pub client_name: String,
    /// `None` when the seat shows the normal logon screen.
    pub credentials: Option<SeatCredentials>,
    /// The seat's outputs when the display binding plugged our IddCx monitors for it
    /// (`DisplayBinding::idd_frames`): the `desktops` backend hands them to the kiosk shell,
    /// which presents the virtual monitors' frames (HDR included) on the seat's targets.
    /// Added 2026-11-17 (parity review).
    pub idd_outputs: Option<crate::backend::display::SeatOutputs>,
}

/// What the service must do next to finish a seat start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartStep {
    /// Launch a seat host; then call [`SessionBackend::wait_session`].
    LaunchHost(HostLaunch),
    /// The session is ready (non-RDP backends).
    Started(SessionInfo),
}

/// Where and how to launch a seat host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostLaunch {
    pub seat: SeatNumber,
    /// Session that runs the host (console / dedicated host session).
    pub target_session: u32,
    /// Connection the host opens. Credentials never appear here: the host fetches them
    /// once over the pipe (`host_take_credentials`).
    pub connection: RdpConnection,
}

/// Parameters of one loopback RDP connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RdpConnection {
    pub address: String,
    pub port: u16,
    pub client_name: String,
    /// Logon name hint (pre-filled; last user per seat, spec 30 §6.2.8).
    pub user_hint: String,
    pub domain_hint: String,
    /// Seat output size (from the display backend), multimon list in placement order.
    pub monitors: Vec<crate::device::Rect>,
    pub audio_playback: bool,
    pub audio_capture: bool,
    /// Drive / smart-card / USB redirection per seat policy (spec 70).
    pub redirect_drives: bool,
    pub redirect_smartcards: bool,
}

/// Virtual-channel redirection the seat host applies (IronRDP backend), resolved by the
/// service from the seat's `SeatRedirect` and the device ownership (spec 70). Everything
/// empty / off by default, which is what an older service sends. Added 2026-11-13.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct HostRedirection {
    #[serde(default)]
    pub clipboard: crate::config::ClipboardRedirect,
    /// Volumes the seat owns alone, offered over MS-RDPEFS.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub drives: Vec<RedirectDrive>,
    /// Smart-card redirection on (MS-RDPESC); only `readers` are visible to the session.
    #[serde(default)]
    pub smartcards: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub readers: Vec<SmartCardReaderRef>,
    /// Print queues announced as redirected printers (MS-RDPEPC), by queue name. Added
    /// 2026-11-14 (round 4 local-gaps, additive).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub printers: Vec<String>,
    /// Announce every local queue and printer connection the host can open (overrides
    /// `printers`). Added 2026-11-14 (additive).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub all_printers: bool,
    /// The seat's drive policy is on (even with no drive now): the host joins the device
    /// channel so drives assigned later reach the open session. Added 2026-11-17 (additive).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub drives_on: bool,
    /// The seat's printer policy is on (even with no queue now): same for printers. Added
    /// 2026-11-17 (additive).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub printers_on: bool,
    /// Snake_case notes for logs / the manager (`clipboard_shared_with_host_session`, ...).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// One redirected volume: a drive-letter root (`E:\`) and the volume it must still be.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedirectDrive {
    pub root: String,
    /// Name the session shows (`E` on `PSEAT-02`); at most 7 ASCII characters are used.
    pub name: String,
    #[serde(default)]
    pub read_only: bool,
    /// Volume GUID path (`\\?\Volume{...}\`) the letter named when the service resolved
    /// it. The host compares it with `GetVolumeNameForVolumeMountPointW(root)` before it
    /// opens the root (every connection) and refuses the drive (`drive_letter_moved`) when
    /// the letter now names another volume. Empty (older service) = refused
    /// (`drive_volume_unknown`). Added 2026-11-13 (additive).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub volume: String,
    /// The volume keeps ACLs (NTFS, ReFS): served as the seat user by the seat's drive
    /// broker (the session agent), never by the seat host. `false` = a FAT-family volume the
    /// host serves itself. Added 2026-11-17 (rdp-listener, additive).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub acl: bool,
}

/// A smart-card reader assigned to the seat: its device instance id and friendly name;
/// the host maps them to WinSCard reader names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SmartCardReaderRef {
    pub instance_id: String,
    pub name: String,
    /// No other reader of the PC (any seat, present or not) has this friendly name, so the
    /// host may match the WinSCard reader by name when it cannot read the reader's device
    /// instance id. `false` (also from an older service) = never matched by name. Added
    /// 2026-11-13 (additive).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub name_unique: bool,
}

/// A Windows session as the backend sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub session_id: u32,
    pub state: SessionState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_sid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_name: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    /// Signed in and connected.
    Active,
    /// Connected, at the logon screen.
    AtLogon,
    Locked,
    Disconnected,
    /// Session ended.
    Gone,
}

/// A session the service recorded before it restarted (from `run\sessions.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnownSession {
    pub seat: SeatNumber,
    pub session_id: u32,
    pub client_name: String,
    #[serde(default)]
    pub user_sid: Option<String>,
}

/// What a seat host told the service about its connection (`host_report`), passed on to
/// the session backend for binding the seat's session (added 2026-11-13).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HostHint {
    /// Session id the RDP server reported to the client (IronRDP: Save Session Info).
    pub session_id: Option<u32>,
    /// The host runs the legacy ActiveX client, which reports no session id: only then
    /// may the "newest new loopback session" fallback bind a session.
    pub legacy_client: bool,
}

/// Swappable session backend. Implemented in `seat-session`: `RdsLoopbackBackend`,
/// `TermsrvWrapperBackend` (experimental), `FastUserSwitchBackend`, `DesktopsBackend`.
pub trait SessionBackend: Send + Sync {
    fn kind(&self) -> SessionBackendKind;
    fn caps(&self) -> SessionCaps;
    /// Can this PC run the backend? Read-only, cheap enough for every status poll.
    fn probe(&self) -> BackendResult<HostProbe>;
    /// One-time host preparation (documented policy values only; every change journaled).
    /// Returns snake_case notes of what was set (shown in the manager).
    fn prepare_host(&self, settings: &GlobalSettings) -> BackendResult<Vec<String>>;
    /// Undo everything `prepare_host` changed (values still ours only).
    fn restore_host(&self) -> BackendResult<()>;
    /// Begin a seat start; see the module doc.
    fn start(&self, req: &SeatStartRequest<'_>) -> BackendResult<StartStep>;
    /// Wait until the seat's session exists (RDP backends, after the host connected).
    fn wait_session(&self, seat: SeatNumber, client_name: &str, timeout: Duration) -> BackendResult<SessionInfo>;
    /// Current state of the seat's session.
    fn query(&self, seat: SeatNumber, session_id: u32) -> BackendResult<SessionInfo>;
    /// Log the seat's session off (only after checking it is ours: client name + user).
    fn log_off(&self, seat: SeatNumber, session_id: u32) -> BackendResult<()>;
    /// Re-adopt sessions after a service restart; returns the ones that still match.
    fn adopt(&self, known: &[KnownSession]) -> BackendResult<Vec<(SeatNumber, SessionInfo)>>;
    /// All interactive sessions (for the same-account check and the status view).
    fn enumerate(&self) -> BackendResult<Vec<SessionInfo>>;
    /// The seat host reported its client and / or session id (see [`HostHint`]). RDP
    /// backends use it in `wait_session`; others ignore it.
    fn host_hint(&self, _seat: SeatNumber, _hint: HostHint) {}
    /// Who may use the second, loopback-only listener for passwordless seats and whether
    /// one is needed at all (`rds_loopback`, architecture §4.1 design (a); added
    /// 2026-11-13). The service calls it before `prepare_host` and whenever seat accounts
    /// or stored passwords change. Others ignore it.
    fn set_listener_plan(&self, _plan: &LoopbackListenerPlan) {}
    /// Composition interval of the RDP seat sessions in ms (`DWMFRAMEINTERVAL`,
    /// [`crate::frame_rate::interval_for`]); `None` = leave Windows' default. RDP-based
    /// backends write it with their host rows (journaled; applied at once when the host is
    /// already prepared; reaches a session when it next connects) and return snake_case
    /// notes. The service calls it before `prepare_host` and after a config apply or an
    /// inventory change. Others ignore it. Added 2026-11-18 (OSMS parity G3, additive).
    fn set_frame_interval(&self, _ms: Option<u32>) -> Vec<String> {
        Vec::new()
    }
    /// Retry host work that had to wait (added 2026-11-13, fix-second-listener): e.g. a
    /// TermService restart that was skipped while seat sessions ran, so a removed listener
    /// is still served. The service calls it after a seat stops and after a session logs
    /// off / ends (off the request thread). Returns snake_case notes. Others ignore it.
    fn retry_deferred(&self) -> Vec<String> {
        Vec::new()
    }
    /// Restart Remote Desktop Services now (operator request `restart_term_service`, added
    /// 2026-11-17): the service stopped the seats first. `Ok(notes)` after the restart
    /// (the second listener re-checked); `Err` with reason `restart_needs_reboot` /
    /// `termservice_restart_failed`. Backends without TermService: unsupported.
    fn restart_termservice(&self) -> BackendResult<Vec<String>> {
        Err(crate::backend::BackendError::unsupported("Remote Desktop Services restart"))
    }
    /// Kiosk shell processes this backend started and that still run (`desktops` backend,
    /// spec 35 §5.1 U3): the service verifies a `kiosk_shell` client in `hello` against
    /// this list (PID + session). Others: none. Added 2026-11-13 (additive).
    fn kiosk_shells(&self) -> Vec<KioskShellProcess> {
        Vec::new()
    }
    /// User SIDs of the kiosk shells that run as their seat's own Windows user
    /// (`desktops`, `settings.kiosk.accounts = per_seat`): the service writes per-user
    /// settings (conhost delegation) into these users' loaded hives. Others: none. Added
    /// 2026-11-17 (additive).
    fn kiosk_users(&self) -> Vec<KioskUser> {
        Vec::new()
    }
    /// `fast_user_switch`: session id of seat 1's own session while it waits off the
    /// console (the backend remembered it when it first switched the console away); a
    /// `start` of seat 1 then connects it back (spec 35 §5.5). Others: `None`. Added
    /// 2026-11-13 (additive).
    fn console_seat_away(&self) -> Option<u32> {
        None
    }
}

/// A kiosk shell running as its seat's own Windows user (see
/// [`SessionBackend::kiosk_users`]).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct KioskUser {
    pub seat: SeatNumber,
    pub user_sid: String,
    /// A Pulsar-made kiosk account (kiosk-only user), not the seat's configured account.
    pub pulsar_account: bool,
}

/// One running kiosk shell (see [`SessionBackend::kiosk_shells`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KioskShellProcess {
    pub seat: SeatNumber,
    pub session_id: u32,
    pub pid: u32,
}

/// Input of [`SessionBackend::set_listener_plan`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LoopbackListenerPlan {
    /// `(domain, user)` of every RDP seat's account plus the configured extra accounts
    /// (`"."` = local). Only these may sign in through the listener.
    pub accounts: Vec<(String, String)>,
    /// At least one RDP seat has no stored password (logon screen); without one the
    /// listener is removed.
    pub passwordless_seats: bool,
}

