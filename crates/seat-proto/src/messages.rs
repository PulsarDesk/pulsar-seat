// SPDX-License-Identifier: GPL-3.0-only
//! Request, response and event bodies. Wire shape: `{"type": "<snake_case>", "data": …}`
//! (unit variants have no `data`).

use seat_core::assign::MaskSource;
use seat_core::backend::audio::AudioRoute;
use seat_core::backend::display::SeatOutputs;
use seat_core::backend::gate::IsolationStatus;
use seat_core::backend::input::InputPacket;
use seat_core::backend::session::RdpConnection;
use seat_core::backend::{
    AudioBackendKind, BackendInfo, DisplayBackendKind, DriverId, GateBackendKind, InputBackendKind,
    SessionBackendKind,
};
use seat_core::config::{Config, SeatKind};
use seat_core::device::Device;
use seat_core::state::{ProductState, SeatRuntime};
use seat_core::validate::Issue;
use seat_core::{DeviceKey, SeatMask, SeatNumber, Secret};
use crate::remote;
use serde::{Deserialize, Serialize};

/// Who is connecting (declared in `hello`, verified by the service).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientKind {
    /// apps/manager, any interactive session.
    Manager,
    /// `pulsar-seat-host.exe`, started by the service; PID + one-time token checked.
    SeatHost,
    /// `pulsar-seat-agent.exe` in a seat session (user token).
    Agent,
    /// Third-party "which seat am I on" API.
    AppApi,
    /// `pulsar-seat-service.exe <subcommand>` talking to the running service.
    Cli,
    /// `pulsar-seat-host.exe --console-listener --token T` in the console / host session,
    /// started by the service under `raw_input_host`; PID + one-time token checked.
    /// Reports activity, hotkeys and wizard keys (`console_input_report`). Added
    /// 2026-11-13 (additive, protocol 1).
    ConsoleListener,
    /// The Pulsar credential provider / filter DLL inside LogonUI (`credprov/`). The service
    /// verifies it in `hello`: SYSTEM token, process image `%SystemRoot%\System32\LogonUI.exe`,
    /// token session = process session. Added 2026-11-13 (additive, protocol 1).
    CredentialProvider,
    /// `pulsar-seat-host.exe --kiosk-shell --seat N` in the console session, started by the
    /// `desktops` backend (spec 35 §5.1 U3). `hello` carries `seat`; the service verifies the
    /// PID + session against the shells it started. A verified shell keeps `status` access
    /// under `admin_only` and may subscribe to the `host` topic, receiving only its own
    /// seat's `input_packets` (input filter). Added 2026-11-13 (additive, protocol 1).
    KioskShell,
}

/// What a request needs from its caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "level", content = "seat", rename_all = "snake_case")]
pub enum Access {
    /// Any connected local client.
    Any,
    /// Interactive users may read status unless the admin-only policy is on.
    Status,
    /// An elevated administrator, or the user signed in on this seat when
    /// `users_may_control_own_seat` is on (and admin-only is off).
    OwnSeat(SeatNumber),
    /// An elevated administrator (token has Administrators enabled) or SYSTEM.
    Admin,
    /// The seat host the service started for this seat (PID + token verified).
    Host(SeatNumber),
    /// A session agent; acts only on its own session.
    Agent,
    /// The console raw-input listener the service started (PID + token verified).
    /// Added 2026-11-13 (additive).
    ConsoleListener,
    /// The verified credential provider in LogonUI; acts only on its own session.
    /// Added 2026-11-13 (additive).
    CredentialProvider,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum RequestBody {
    // ---- connection ----
    /// Must be first. Answered with [`ResponseBody::Hello`].
    Hello(Hello),
    /// Switch this connection to also receive pushed events of these topics.
    Subscribe { topics: Vec<Topic> },
    /// For clients that cannot hold a subscription: events with `seq > after`, waiting up
    /// to `wait_ms` (≤ [`crate::MAX_POLL_WAIT_MS`]) when there are none.
    PollEvents {
        after: u64,
        #[serde(default)]
        wait_ms: u32,
        #[serde(default)]
        topics: Vec<Topic>,
    },

    // ---- status ----
    GetStatus,
    /// Backends, their availability and capabilities, our drivers' state.
    GetBackends,
    /// OS build, Secure Boot, Memory Integrity, edition, coexistence checks (spec 80 §11.4).
    Preflight,

    // ---- configuration ----
    /// The config document plus which seats have a stored password.
    GetConfig,
    ValidateConfig { config: Box<Config> },
    /// Replace the config. `base_revision` = the revision the edit started from;
    /// `force` = save despite `Conflict` issues (user confirmed).
    ApplyConfig {
        config: Box<Config>,
        base_revision: u64,
        #[serde(default)]
        force: bool,
    },
    /// Restore defaults (double-confirmed in the UI); reverts every tweak and gate.
    ResetConfig { confirm: bool },
    /// Optional importer of the reference product's `default_configuration.json`
    /// (spec 10 §3.8 field names). Returns a config to review; nothing is saved.
    ImportReference { json: String },
    /// Write-only: set (`Some`) or clear (`None`) a seat's auto-logon password.
    SetSeatPassword { seat: SeatNumber, password: Option<Secret> },
    ListLocalUsers,
    /// CPU sets for the cores-per-seat grid (spec 10 ARM64 notes, spec 80 §11.2).
    ListCpuSets,

    // ---- devices ----
    ListDevices,
    /// Live devices vs. saved assignments (spec 10 §3.8).
    CompareSaved,
    /// Indication on/off: monitor overlay, speaker tone, camera preview (spec 10 §3.5.1).
    IdentifyDevice { key: DeviceKey, on: bool },
    /// Move an assignment to the device's new key (known device on a new port).
    MoveAssignment { from: DeviceKey, to: DeviceKey },

    // ---- product and seats ----
    /// Product switch (service state; reboot only when a driver package changes).
    SetEnabled { enabled: bool },
    /// Confirm (`accept`: save as assignments) or decline (back to the saved routes) the
    /// pending hotkey input moves (notice `input_move_pending`, spec 50 §4.1.9). Answers
    /// `ack`. Added 2026-11-12 (additive, protocol 1).
    ResolveInputMove { accept: bool },
    /// Start these seats (`None` = every autostart seat). Asynchronous: answered when the
    /// starts are queued; progress arrives as `seat_state` events.
    StartSeats { seats: Option<Vec<SeatNumber>> },
    StopSeat { seat: SeatNumber },
    /// Log the seat's session off and start it again (spec 10 §8.5).
    RelogonSeat { seat: SeatNumber },
    /// `fast_user_switch`: put a running local seat's session (or seat 1's own session
    /// while it waits off the console) on the console. No stored password is used: the
    /// console goes to a logon screen with that seat's user preselected (Windows reconnects
    /// the session after the sign-in). An off seat is started as with `start_seats`.
    /// Added 2026-11-15 (protocol 3, admin).
    SwitchSeat { seat: SeatNumber },
    /// Ask the service to restart Windows (it asks nothing itself; the manager confirmed).
    RequestReboot,
    /// Restart the graphics driver for a running seat (spec/local 50: the Windows shortcut
    /// Win+Ctrl+Shift+B sent into the seat session's input desktop by its agent). Answers
    /// `ack` once queued. Added 2026-11-17 (parity review, additive, admin).
    ResetGraphics { seat: SeatNumber },
    /// Delete Pulsar's own Windows user of a kiosk seat (`PulsarKiosk<NN>`) with its
    /// profile and stored password. Refused while the seat runs a kiosk shell. Unused
    /// kiosk users are otherwise only disabled at service start (their data stays). Added
    /// 2026-11-17 (parity review, additive, admin).
    RemoveKioskUser { seat: SeatNumber },
    /// Hyper-V helper of the source-IP tools (status, turn the feature on, a seat's own
    /// virtual adapter with a fixed address on an internal switch). Answers
    /// [`ResponseBody::HyperV`]. Added 2026-11-17 (parity review, additive, admin).
    HyperV { action: crate::hyperv::HyperVAction },
    /// Steam legacy proxy-DLL report, quarantine and restore (spec/remote 50 §6.2).
    /// Answers [`ResponseBody::SteamProxy`]. Added 2026-11-17 (compat-tweaks, additive,
    /// admin).
    SteamProxyDll { action: crate::steam::SteamProxyAction },
    /// Hyper-V VM seats (spec/remote 10 §8): status, VM network, base disk, reset a seat's
    /// disk. Answers [`ResponseBody::VmSeats`]. Added 2026-11-17 (hyperv-seats, additive,
    /// admin).
    VmSeats { action: crate::hyperv::VmAction },
    /// Restart Remote Desktop Services (TermService) on the operator's request, e.g. so a
    /// new or changed second listener (`rds_loopback`) is loaded. `confirm` must be true
    /// (the manager asked). Every running seat except seat 1 is stopped first (their
    /// sessions would drop anyway); the service restarts TermService and its dependants
    /// and answers [`ResponseBody::TermServiceRestart`]. Seats are not started again by
    /// themselves. Added 2026-11-17 (rdp-listener, additive, admin).
    RestartTermService { confirm: bool },

    // ---- first-run wizard (spec 10 §9.2) ----
    WizardStart { variant: WizardVariant },
    WizardCommand { command: WizardCommand },
    WizardStop { save: bool },

    // ---- diagnostics ----
    /// Write a diagnostic report (no secrets; user names masked unless `mask_users` false).
    DiagnosticReport { mask_users: bool },

    // ---- updates (spec 90 §11 item 10; added 2026-11-13, additive, protocol 1) ----
    /// Last known update state (no network access). Answers `update_status`.
    GetUpdateStatus,
    /// Ask GitHub Releases for the latest release. Only when `settings.updates.check` is
    /// on (opt-in); refused with `disabled` otherwise. Asynchronous: answers the status
    /// with `state: checking`; the result arrives through `get_update_status`.
    CheckForUpdate,
    /// Download the installer of the release found by the last check into the service's
    /// private folder and verify it (SHA-256 from the release's checksum file +
    /// Authenticode signature by an accepted publisher). Asynchronous like the check.
    DownloadUpdate,
    /// Run the verified installer (`version` must equal the verified one; `confirm` =
    /// the administrator confirmed that running seats stop). The file is verified again
    /// right before it starts. Never automatic.
    InstallUpdate { version: String, confirm: bool },

    // ---- app API ----
    /// Seat of the caller's session.
    MySeat,
    /// Launcher (spec 80 §11.2; added 2026-11-13, additive, protocol 1): start `path`
    /// (absolute, local) with `args` in the **caller's own** seat session, as that
    /// session's signed-in user, inside the seat's job (the seat's CPU limits) and with the
    /// seat's source-IP variable when its rule picks the program. Only the session's
    /// signed-in user may ask. Answers [`ResponseBody::AppStarted`]. Programs started from
    /// Explorer / the Start menu and UAC-elevated programs are not limited.
    StartSeatApp {
        path: String,
        #[serde(default)]
        args: Vec<String>,
    },
    SeatOfSession { session_id: u32 },
    SeatOfProcess { pid: u32 },

    // ---- seat host only ----
    /// Connection, outputs, audio route and input interfaces for this host.
    HostGetAssignment { seat: SeatNumber },
    /// The seat's credentials, once per host launch (then refused).
    HostTakeCredentials { seat: SeatNumber },
    HostReport {
        seat: SeatNumber,
        state: HostState,
        /// RDP client the host runs: `"ironrdp"` or `"activex"` (added 2026-11-13,
        /// additive). Only the ActiveX client may be bound by the "newest new loopback
        /// session" fallback; others bind by their reported session id or exact client name.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rdp_client: Option<String>,
    },

    /// The seat host refused a clipboard / drive / smart-card request of the seat session
    /// (rate-limited per reason by the host). Answered with `Ack`. Added 2026-11-13
    /// (additive, protocol 1): an older service answers an error the host ignores.
    HostRedirectRefusal {
        seat: SeatNumber,
        channel: String,
        reason: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        detail: String,
        #[serde(default)]
        suppressed: u32,
    },
    /// The seat host's audio bridge state changed: whether the seat's render / capture
    /// stream is open and its snake_case notes (`render_absent`, `render_open_failed`,
    /// `render_lost`, `capture_absent`, ...). Stored in `SeatRuntime.audio_notes` for the
    /// manager. Answered with `Ack`. Added 2026-11-17 (rdp-listener, additive): an older
    /// service answers an error the host ignores.
    HostAudioReport {
        seat: SeatNumber,
        #[serde(default)]
        render_open: bool,
        #[serde(default)]
        capture_open: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        notes: Vec<String>,
    },
    /// The seat host asks for its link to the seat's drive broker (ACL'd volumes served as
    /// the seat user by the seat's agent, architecture §"Drive broker"). The service makes
    /// two fresh anonymous pipes (one per direction), duplicates one end of each into the
    /// host and the other into the seat session's agent (agent task `drive_broker`) and
    /// answers [`ResponseBody::DriveBroker`]. Refused (`not_ready`) while the seat has no
    /// agent, `no_acl_drives` when the seat has no ACL'd volume to share.
    /// Added 2026-11-17 (rdp-listener, additive).
    HostDriveBroker { seat: SeatNumber },

    // ---- session agent only ----
    AgentReport { report: AgentReport },

    // ---- console listener only (raw_input_host; added 2026-11-13, additive, protocol 1) ----
    /// What the console raw-input listener saw since its last report (also sent empty as
    /// a heartbeat, at least every [`crate::CONSOLE_REPORT_HEARTBEAT_MS`]). Answered with
    /// [`ResponseBody::ConsoleListenerState`].
    ConsoleInputReport {
        #[serde(default)]
        events: Vec<ConsoleInputEvent>,
    },

    // ---- credential provider only (fast_user_switch, spec 35 §5.4 / §5.5; added
    // 2026-11-13, additive, protocol 1). Every request acts on the caller's own session. ----
    /// What the provider and the filter should do in this LogonUI session. Answered with
    /// [`ResponseBody::CredprovSession`]. Never contains a secret.
    CredprovQuery {
        #[serde(default)]
        scenario: CredprovScenario,
    },
    /// The armed seat credential of this session, **once** (then refused with reason
    /// `credentials_taken`). Answered with [`ResponseBody::Credentials`].
    CredprovTakeCredentials,
    /// Result of the logon LogonUI attempted with the auto-submitted credential
    /// (`ICredentialProviderCredential::ReportResult`). A failure marks the stored password
    /// failing (no retry loop: lockout risk).
    CredprovReport {
        /// NTSTATUS of the logon (0 = success).
        status: i32,
        #[serde(default)]
        sub_status: i32,
    },
    /// The "switch seat" link on the logon / lock screen: bring seat `seat` onto the console
    /// (same rules as a manual start under `fast_user_switch`). Answers `ack` when queued.
    CredprovSwitch { seat: SeatNumber },

    // ---- session agent: seat switch (fast_user_switch, spec 35 §5.5(a); added 2026-11-13,
    // additive, protocol 1). They act on the agent's own session only. ----
    /// Seats the agent's session may switch to (same rule as the credential provider's
    /// switch links). Answered with [`ResponseBody::SwitchTargets`].
    AgentSwitchTargets,
    /// Bring seat `seat` onto the console from the agent's own (locked) seat session. No
    /// stored password is used: nobody authenticated for the target. Answers `ack`.
    AgentSwitch { seat: SeatNumber },

    // ---- protocol 3: Pulsar Split's v2 requests (architecture.md section 10.2). All
    // `Access::Admin`; seat-scoped ones answer `wrong_seat_kind` on the other kind. ----
    /// Logon test with the stored password (any kind); answers `credential_test`.
    TestSeatCredentials { seat: SeatNumber },
    /// Enable and / or unlock the seat user after the operator confirmed (any kind).
    EnableUnlockAccount { seat: SeatNumber, enable: bool, unlock: bool, confirm: bool },
    /// Remote seat runtime (display, audio, pads, stream, isolation); answers
    /// `remote_runtime`. Split's `get_seat_runtime`.
    GetRemoteRuntime { seat: SeatNumber },
    /// Forward a pairing PIN to the remote seat's stream server; the outcome arrives as a
    /// `stream` event (`pair_ended`) and in `pair_state`.
    SubmitPin {
        seat: SeatNumber,
        pin: Secret,
        client_name: String,
        #[serde(default)]
        request_id: Option<u32>,
    },
    GetPairState { seat: SeatNumber },
    CancelPair { seat: SeatNumber },
    ListPairedClients { seat: SeatNumber },
    UnpairClient { seat: SeatNumber, uuid: String },
    UnpairAll { seat: SeatNumber },
    /// New certificate / identity; stops the seat and unpairs every client.
    RegenerateSeatIdentity { seat: SeatNumber, confirm: bool },
    /// `seat_stream::apps::AppEntry` values; answers `apps`.
    GetApps { seat: SeatNumber },
    SetApps { seat: SeatNumber, apps: Vec<serde_json::Value> },
    /// Pulsar backend: allow / deny a pending connect or pair request.
    AnswerPeerRequest {
        seat: SeatNumber,
        request_id: u32,
        allow: bool,
        #[serde(default)]
        view_only: bool,
    },
    /// Pulsar backend, write-only: the permanent connect password (empty = none).
    SetSeatConnectPassword { seat: SeatNumber, password: Secret },
    /// Pulsar backend: the current one-time password; answers `otp`.
    GetSeatOtp { seat: SeatNumber },
    RotateSeatOtp { seat: SeatNumber },
    /// Pulsar backend: credentials the relay asked for.
    AnswerRelayAuth {
        seat: SeatNumber,
        password: Secret,
        #[serde(default)]
        totp: Option<Secret>,
    },
    /// Session host facts for remote seats (edition, RDS, licensing, coexistence, display /
    /// pad backends); answers `host_info`.
    GetHostInfo,
    /// Prepare (or undo) the host for several remote seat sessions (RDS settings).
    SetMultiSession { enabled: bool },
    GetTermwrapStatus,
    /// EXPERIMENTAL wrapper integration. Stops every remote seat first.
    SetTermwrap {
        enabled: bool,
        #[serde(default)]
        wrapper_path: Option<String>,
        #[serde(default)]
        ack_warning_version: u32,
    },
    ListRenderAdapters,
    GetPadBackends,
    /// Per-process HID shim state; answers `shim_status`. Split's `get_isolation_status`.
    GetShimStatus,
    SetShimTargets { targets: Vec<remote::ShimTargetDto> },
    SetAppMasking { exe_path: String, enabled: bool },
    /// `dlls`: `d3d8` | `d3d9`.
    SetDxvk {
        game_exe: String,
        enabled: bool,
        #[serde(default)]
        dlls: Vec<String>,
    },
    GetDxvkStatus,
    /// Last `lines` (1-1000) redacted log lines; `source`: `service` or `seat:<N>`.
    GetLogTail { source: String, lines: u32 },
    GetStreamBackends,
    /// Account picture of a local user; answers `user_picture`.
    GetUserPicture { user: String },
    /// Remove non-present RDP audio endpoint nodes of ended sessions; `confirm` must be true.
    CleanupAudioEndpoints { confirm: bool },
    /// Apply a display mode to a running remote seat now (operator control, spec/remote 40
    /// §4.4 rules and the seat's display settings apply). Answers `mode_result`. Added
    /// 2026-11-17 (remote-idd-hdr, additive).
    SetSeatDisplayMode {
        seat: SeatNumber,
        width: u32,
        height: u32,
        hz: u32,
        #[serde(default)]
        hdr: bool,
    },
}

impl RequestBody {
    /// Authorisation table. The service enforces it; clients may use it to grey out UI.
    pub fn access(&self) -> Access {
        use RequestBody as R;
        match self {
            // `start_seat_app`: the service checks the caller's own seat and user itself.
            R::Hello(_) | R::MySeat | R::SeatOfSession { .. } | R::SeatOfProcess { .. } | R::StartSeatApp { .. } => {
                Access::Any
            }
            R::Subscribe { .. } | R::PollEvents { .. } | R::GetStatus | R::GetBackends | R::ListDevices => Access::Status,
            R::RelogonSeat { seat } => Access::OwnSeat(*seat),
            R::HostGetAssignment { seat }
            | R::HostTakeCredentials { seat }
            | R::HostReport { seat, .. }
            | R::HostRedirectRefusal { seat, .. }
            | R::HostAudioReport { seat, .. }
            | R::HostDriveBroker { seat } => {
                Access::Host(*seat)
            }
            R::AgentReport { .. } | R::AgentSwitchTargets | R::AgentSwitch { .. } => Access::Agent,
            R::ConsoleInputReport { .. } => Access::ConsoleListener,
            R::CredprovQuery { .. } | R::CredprovTakeCredentials | R::CredprovReport { .. } | R::CredprovSwitch { .. } => {
                Access::CredentialProvider
            }
            R::Preflight
            | R::GetConfig
            | R::ValidateConfig { .. }
            | R::ApplyConfig { .. }
            | R::ResetConfig { .. }
            | R::ImportReference { .. }
            | R::SetSeatPassword { .. }
            | R::ListLocalUsers
            | R::ListCpuSets
            | R::CompareSaved
            | R::IdentifyDevice { .. }
            | R::MoveAssignment { .. }
            | R::SetEnabled { .. }
            | R::ResolveInputMove { .. }
            | R::StartSeats { .. }
            | R::SwitchSeat { .. }
            | R::StopSeat { .. }
            | R::RequestReboot
            | R::ResetGraphics { .. }
            | R::RemoveKioskUser { .. }
            | R::HyperV { .. }
            | R::SteamProxyDll { .. }
            | R::VmSeats { .. }
            | R::RestartTermService { .. }
            | R::WizardStart { .. }
            | R::WizardCommand { .. }
            | R::WizardStop { .. }
            | R::DiagnosticReport { .. }
            | R::GetUpdateStatus
            | R::CheckForUpdate
            | R::DownloadUpdate
            | R::InstallUpdate { .. } => Access::Admin,
            // Protocol 3 (Pulsar Split v2): admin only, remote seat status included.
            R::TestSeatCredentials { .. }
            | R::EnableUnlockAccount { .. }
            | R::GetRemoteRuntime { .. }
            | R::SubmitPin { .. }
            | R::GetPairState { .. }
            | R::CancelPair { .. }
            | R::ListPairedClients { .. }
            | R::UnpairClient { .. }
            | R::UnpairAll { .. }
            | R::RegenerateSeatIdentity { .. }
            | R::GetApps { .. }
            | R::SetApps { .. }
            | R::AnswerPeerRequest { .. }
            | R::SetSeatConnectPassword { .. }
            | R::GetSeatOtp { .. }
            | R::RotateSeatOtp { .. }
            | R::AnswerRelayAuth { .. }
            | R::GetHostInfo
            | R::SetMultiSession { .. }
            | R::GetTermwrapStatus
            | R::SetTermwrap { .. }
            | R::ListRenderAdapters
            | R::GetPadBackends
            | R::GetShimStatus
            | R::SetShimTargets { .. }
            | R::SetAppMasking { .. }
            | R::SetDxvk { .. }
            | R::GetDxvkStatus
            | R::GetLogTail { .. }
            | R::GetStreamBackends
            | R::GetUserPicture { .. }
            | R::CleanupAudioEndpoints { .. }
            | R::SetSeatDisplayMode { .. } => Access::Admin,
        }
    }

    /// The seat and the kind this request needs (`None` = no seat, or any kind). The
    /// service answers `wrong_seat_kind` when the seat has the other kind.
    pub fn seat_kind(&self) -> Option<(SeatNumber, SeatKind)> {
        use RequestBody as R;
        match self {
            R::RelogonSeat { seat }
            | R::SwitchSeat { seat }
            | R::HostGetAssignment { seat }
            | R::HostTakeCredentials { seat }
            | R::HostReport { seat, .. }
            | R::HostRedirectRefusal { seat, .. }
            | R::HostAudioReport { seat, .. }
            | R::HostDriveBroker { seat }
            | R::CredprovSwitch { seat }
            | R::AgentSwitch { seat } => Some((*seat, SeatKind::Local)),
            R::GetRemoteRuntime { seat }
            | R::SubmitPin { seat, .. }
            | R::GetPairState { seat }
            | R::CancelPair { seat }
            | R::ListPairedClients { seat }
            | R::UnpairClient { seat, .. }
            | R::UnpairAll { seat }
            | R::RegenerateSeatIdentity { seat, .. }
            | R::GetApps { seat }
            | R::SetApps { seat, .. }
            | R::AnswerPeerRequest { seat, .. }
            | R::SetSeatConnectPassword { seat, .. }
            | R::GetSeatOtp { seat }
            | R::RotateSeatOtp { seat }
            | R::AnswerRelayAuth { seat, .. }
            | R::SetSeatDisplayMode { seat, .. } => Some((*seat, SeatKind::Remote)),
            _ => None,
        }
    }

    /// Lowest client protocol (from its `hello`) that may send this request: 3 for the
    /// config requests (the seat shape changed in schema 2) and every protocol 3 request,
    /// else [`crate::MIN_PROTOCOL_VERSION`].
    pub fn min_protocol(&self) -> u32 {
        use RequestBody as R;
        match self {
            R::GetConfig | R::ValidateConfig { .. } | R::ApplyConfig { .. } | R::ResetConfig { .. } | R::ImportReference { .. } => 3,
            _ if self.is_remote_kind_request() => 3,
            R::SwitchSeat { .. } => 3,
            // Added after protocol 3 (parity rounds, 2026-11-17): additive, but only
            // protocol 3 clients know them (review 2026-11-17: the docs said so, the code
            // did not).
            R::ResetGraphics { .. }
            | R::SteamProxyDll { .. }
            | R::VmSeats { .. }
            | R::RestartTermService { .. }
            | R::HostAudioReport { .. }
            | R::HostDriveBroker { .. } => 3,
            _ => crate::MIN_PROTOCOL_VERSION,
        }
    }

    /// One of the protocol 3 requests that came from Pulsar Split.
    pub fn is_remote_kind_request(&self) -> bool {
        use RequestBody as R;
        matches!(
            self,
            R::TestSeatCredentials { .. }
                | R::EnableUnlockAccount { .. }
                | R::GetRemoteRuntime { .. }
                | R::SubmitPin { .. }
                | R::GetPairState { .. }
                | R::CancelPair { .. }
                | R::ListPairedClients { .. }
                | R::UnpairClient { .. }
                | R::UnpairAll { .. }
                | R::RegenerateSeatIdentity { .. }
                | R::GetApps { .. }
                | R::SetApps { .. }
                | R::AnswerPeerRequest { .. }
                | R::SetSeatConnectPassword { .. }
                | R::GetSeatOtp { .. }
                | R::RotateSeatOtp { .. }
                | R::AnswerRelayAuth { .. }
                | R::GetHostInfo
                | R::SetMultiSession { .. }
                | R::GetTermwrapStatus
                | R::SetTermwrap { .. }
                | R::ListRenderAdapters
                | R::GetPadBackends
                | R::GetShimStatus
                | R::SetShimTargets { .. }
                | R::SetAppMasking { .. }
                | R::SetDxvk { .. }
                | R::GetDxvkStatus
                | R::GetLogTail { .. }
                | R::GetStreamBackends
                | R::GetUserPicture { .. }
                | R::CleanupAudioEndpoints { .. }
                | R::SetSeatDisplayMode { .. }
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub client: ClientKind,
    /// Client build version (`CARGO_PKG_VERSION` / package.json).
    pub client_version: String,
    /// Protocol the client speaks (normally [`crate::PROTOCOL_VERSION`]).
    pub protocol_version: u32,
    /// Seat hosts: their seat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seat: Option<SeatNumber>,
    /// Seat hosts: the one-time token from their command line (`--token`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<Secret>,
}

/// Event topics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Topic {
    /// Product and seat states, config changes, notices.
    Status,
    /// Device arrival / removal / change.
    Devices,
    /// Per-device activity (identify by touch); high rate.
    Activity,
    Wizard,
    /// Seat host only: input packets and host commands for its seat.
    Host,
    /// Agent only: tasks for its session.
    Agent,
    /// Remote seats: pairing and stream start / end (`stream` events). Admin only.
    /// Added in protocol 3.
    Stream,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ResponseBody {
    Hello(HelloReply),
    Ack,
    /// Answer of `hyper_v` (added 2026-11-17, additive).
    HyperV(crate::hyperv::HyperVStatus),
    /// Answer of `steam_proxy_dll` (added 2026-11-17, additive).
    SteamProxy(crate::steam::SteamProxyStatus),
    /// Answer of `vm_seats` (added 2026-11-17, additive).
    VmSeats(Box<crate::hyperv::VmHostStatus>),
    Status(ServiceStatus),
    Backends(BackendReport),
    Checks(Vec<Check>),
    Config(ConfigView),
    Issues(Vec<Issue>),
    Applied(ApplyResult),
    Users(Vec<LocalUser>),
    CpuSets(Vec<CpuSetInfo>),
    Devices(Vec<DeviceView>),
    Diff(ConfigDiff),
    Events(EventBatch),
    Report { path: String },
    Seat { seat: Option<SeatNumber>, session_id: Option<u32> },
    HostAssignment(HostAssignment),
    Credentials(Credentials),
    AgentTasks(Vec<AgentTask>),
    /// Reply to `console_input_report` (added 2026-11-13, additive).
    ConsoleListenerState(ConsoleListenerState),
    /// Reply to `credprov_query` (added 2026-11-13, additive).
    CredprovSession(CredprovSession),
    /// Answer of the update requests (added 2026-11-13).
    UpdateStatus(UpdateStatus),
    /// Answer of `start_seat_app` (added 2026-11-13, additive).
    AppStarted { pid: u32 },
    /// Answer of `agent_switch_targets` (added 2026-11-13, additive).
    SwitchTargets(Vec<SeatNumber>),
    // ---- protocol 3 (Pulsar Split v2 bodies) ----
    HostInfo(remote::HostInfo),
    /// `seat_session::termwrap::TermwrapStatus` plus `warning_text` (English) and `warning_text_tr` (Turkish), verbatim.
    TermwrapStatus(serde_json::Value),
    RemoteRuntime(remote::SeatRuntime),
    /// `seat_display::remote::RenderAdapter` values.
    RenderAdapters(Vec<serde_json::Value>),
    /// `seat_pads::BackendAvailability` values.
    PadBackends(Vec<serde_json::Value>),
    PairState(remote::PairState),
    PairedClients(Vec<remote::PairedClient>),
    /// `seat_stream::apps::AppEntry` values.
    Apps(Vec<serde_json::Value>),
    ShimStatus(remote::IsolationStatus),
    CredentialTest(remote::CredentialTest),
    LogLines(Vec<String>),
    StreamBackends(Vec<remote::StreamBackendInfo>),
    /// One-time password of a Pulsar-backend remote seat.
    Otp { otp: Secret },
    DxvkStatus(remote::DxvkStatus),
    UserPicture(remote::UserPicture),
    AudioCleanup(remote::AudioCleanup),
    /// Answer of `restart_term_service` (added 2026-11-17, additive).
    TermServiceRestart(TermServiceRestart),
    /// Answer of `host_drive_broker`: the host's ends of the broker pipes (handle values
    /// valid in the host process: `read` from the broker, `write` to it). Added
    /// 2026-11-17, additive.
    DriveBroker { read: u64, write: u64 },
    /// Answer of `set_seat_display_mode`: `seat_display::remote::ModeResult` (`status`,
    /// applied and render size, format). Added 2026-11-17 (additive).
    ModeResult(serde_json::Value),
}

/// Result of `restart_term_service`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TermServiceRestart {
    /// Seats stopped before the restart.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stopped_seats: Vec<SeatNumber>,
    /// TermService was restarted.
    pub restarted: bool,
    /// Snake_case reason when it was not (`restart_needs_reboot`,
    /// `termservice_restart_failed`, `passive`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HelloReply {
    pub service_version: String,
    pub protocol_version: u32,
    pub min_protocol_version: u32,
    pub caller: CallerInfo,
}

/// What the service determined about the caller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallerInfo {
    pub client: ClientKind,
    pub session_id: u32,
    /// Seat of the caller's session, if it is a seat session.
    pub seat: Option<SeatNumber>,
    /// Token has Administrators enabled (elevated) or is SYSTEM.
    pub is_admin: bool,
    /// Admin-only policy is on and the caller is not admin: only `Any` requests work.
    pub restricted: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceStatus {
    pub service_version: String,
    pub product: ProductState,
    pub seats: Vec<SeatRuntime>,
    /// Backends actually in use (may differ from the config when one is unavailable and
    /// the service fell back; `fallback_notes` says why).
    pub active: ActiveBackends,
    pub fallback_notes: Vec<String>,
    pub config_revision: u64,
    /// A saved change needs a reboot (driver package install / removal only).
    pub reboot_pending: bool,
    /// Windows build changed since the config was last applied (re-check prompt).
    pub os_build_changed: bool,
    pub wizard_running: bool,
    /// Last event `seq` (for `poll_events`).
    pub last_event_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveBackends {
    pub session: SessionBackendKind,
    pub display: DisplayBackendKind,
    pub input: InputBackendKind,
    pub audio: AudioBackendKind,
    pub gates: Vec<GateBackendKind>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BackendReport {
    pub session: Vec<BackendInfo>,
    pub display: Vec<BackendInfo>,
    pub input: Vec<BackendInfo>,
    pub audio: Vec<BackendInfo>,
    pub gates: Vec<BackendInfo>,
    pub drivers: Vec<DriverReport>,
    /// How the per-seat source addresses are applied right now (source-IP callout or the
    /// `PULSAR_SEAT_SOURCE_IP` fallback). Added 2026-11-17 (net-wfp, additive); `None`
    /// from older services.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_ip: Option<SourceIpReport>,
}

/// `driver`: the WFP callout `drivers/seat-wfp` holds the session -> address map;
/// `environment`: only programs that read `PULSAR_SEAT_SOURCE_IP` use the address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceIpEnforcement {
    Driver,
    Environment,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceIpReport {
    pub enforcement: SourceIpEnforcement,
    /// Why `environment` (`driver_not_loaded`, `driver_busy`, `driver_version_mismatch`,
    /// `driver_pass_through`, `map_invalid`, `map_refused`); `None` for `driver`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Signed-in seats whose address the driver enforces.
    #[serde(default)]
    pub enforced: Vec<SeatNumber>,
    /// Signed-in seats with an address that share their session with another seat (kiosk
    /// shell seats in the console session): not in the map, the fallback only.
    #[serde(default)]
    pub shared_session: Vec<SeatNumber>,
    /// Program-list entries that have no NT path (unknown drive, relative path): left out
    /// of the driver's list.
    #[serde(default)]
    pub dropped_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriverReport {
    pub driver: DriverId,
    pub status: seat_core::backend::DriverStatus,
}

/// One pre-flight check (`os_build`, `secure_boot`, `memory_integrity`, `edition`,
/// `rdsh_role`, `termservice_stock`, `reference_product`, `foreign_input_filters`,
/// `arch_native`, …). Security states are reported, never changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Check {
    pub id: String,
    pub outcome: CheckOutcome,
    /// English detail for logs; the manager shows localized copy by `id` + `outcome`.
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckOutcome {
    Pass,
    Warn,
    Fail,
    Info,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigView {
    pub config: Config,
    /// Seats with a stored password (marker in the seat table).
    pub password_set: Vec<SeatNumber>,
    /// Seats whose stored password failed at the last logon.
    pub password_failing: Vec<SeatNumber>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApplyResult {
    pub revision: u64,
    /// Warnings / confirmed conflicts that remain.
    pub issues: Vec<Issue>,
    /// Seats that must restart for the change to apply (display moves while started).
    pub restart_seats: Vec<SeatNumber>,
    pub reboot_needed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalUser {
    pub name: String,
    pub full_name: String,
    pub sid: String,
    pub disabled: bool,
    pub is_admin: bool,
}

/// One CPU set (`GetSystemCpuSetInformation`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CpuSetInfo {
    pub id: u32,
    pub group: u16,
    pub logical_index: u8,
    pub core_index: u8,
    /// Higher = faster core class (0 = efficiency on hybrid CPUs).
    pub efficiency_class: u8,
    pub numa_node: u32,
}

/// A device as the manager draws it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceView {
    pub device: Device,
    pub effective: SeatMask,
    pub source: MaskSource,
    /// Which enforcement applies (`None` = not handled by any gate / router).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolation: Option<IsolationStatus>,
    /// Service clock ms of arrival (for the "just connected" frame).
    pub arrived_ms: u64,
    /// Assignment key of the USB port this device is plugged into (`port;loc=...`), so the
    /// manager can assign that one port of a shared hub (spec 70 §5.1). Added 2026-11-13.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port_key: Option<DeviceKey>,
    /// Assignment key of the device's container (`cont;id=...`), present when the container
    /// groups more than one live device (spec 70 §5.1). Added 2026-11-13.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_key: Option<DeviceKey>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigDiff {
    pub matches: bool,
    pub added: Vec<DeviceKey>,
    pub changed: Vec<MaskChange>,
    pub missing: Vec<DeviceKey>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaskChange {
    pub key: DeviceKey,
    pub saved: SeatMask,
    pub live: SeatMask,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventBatch {
    pub events: Vec<crate::EventFrame>,
    /// Highest seq known to the service (may exceed the batch when topics filter).
    pub last_seq: u64,
}

/// Wizard variants (spec 10 §9.2 step 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WizardVariant {
    /// External mice on every seat; capture seats 1..N.
    AllSeats,
    /// Keep the detected seat-1 devices; capture seats 2..N.
    KeepSeatOne,
    /// Seat-1 devices on seat 1, everything else on seat 2, no capture.
    QuickTwoSeats,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum WizardCommand {
    /// The user picked seat 1's monitor (overlay number).
    PickConsoleMonitor { key: DeviceKey },
    /// Extend the desktop to every connected monitor (user consented).
    ExtendDesktop,
    ConfirmSeat { seat: SeatNumber },
    ResetSeat { seat: SeatNumber },
    ResetAll,
    /// Main keyboard / mouse found by touch before the wizard (spec 10 §9.1 "gather",
    /// "Pulsar Seat approach": main-device capture by touch). They stay on seat 1 for
    /// `keep_seat_one` and `quick_two_seats`. Added 2026-11-13 (additive).
    SetMainDevices { keys: Vec<DeviceKey> },
    /// A mouse target on seat `seat`'s page was clicked (the page saw the click). The
    /// service credits the click to the unassigned mouse that clicked last (spec 10 §9.2
    /// step 5). Added 2026-11-13 (additive).
    TargetHit { seat: SeatNumber },
}

/// Seat host connection state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum HostState {
    Connecting,
    Connected,
    /// Session id as reported by the RDP server (if known).
    SignedIn { session_id: Option<u32> },
    Disconnected { reason: String },
    /// `client_auth`, `cert_mismatch`, `display`, `audio`, …
    Failed { reason: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostAssignment {
    pub seat: SeatNumber,
    pub connection: RdpConnection,
    pub outputs: SeatOutputs,
    pub audio: AudioRoute,
    pub input_backend: InputBackendKind,
    /// `RawInputHost`: device-interface paths of this seat's units (match raw input
    /// handles via `RIDI_DEVICENAME`). Empty for `KmdfFilter` (packets come as events).
    pub input_interfaces: Vec<String>,
    /// SHA-1 thumbprints (hex) of the certificates the RDP-Tcp listener may present
    /// (`seat_rdp::pin`); the seat host pins its TLS connection to them and fails closed.
    /// Added 2026-11-13 (additive, protocol 1); empty from an older service.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub listener_cert_sha1: Vec<String>,
    /// The owner's input settings (switch hotkey, system buttons) for the host's own input
    /// forwarder. Added 2026-11-13 (additive, protocol 1); defaults from an older service.
    #[serde(default)]
    pub input: seat_core::config::InputSettings,
    /// Clipboard / drive / smart-card redirection for the IronRDP backend (spec 70),
    /// resolved by the service (`seat_rdp::policy::host_redirection`). Added 2026-11-13
    /// (additive, protocol 1); all off from an older service. Boxed: keeps the response
    /// frame small.
    #[serde(default)]
    pub redirection: Box<seat_core::backend::session::HostRedirection>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Credentials {
    pub domain: String,
    pub user: String,
    pub password: Option<Secret>,
}

/// LogonUI usage scenario the credential provider runs in (subset of
/// `CREDENTIAL_PROVIDER_USAGE_SCENARIO`). Added 2026-11-13.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredprovScenario {
    /// `CPUS_LOGON`: the logon screen of a session.
    #[default]
    Logon,
    /// `CPUS_UNLOCK_WORKSTATION`: the lock screen. Never auto-submitted (spec 35 §5.4).
    Unlock,
}

/// Answer of `credprov_query`: what the Pulsar credential provider and filter do in the
/// caller's LogonUI session (spec 35 §5.4). Added 2026-11-13. Never contains a secret.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CredprovSession {
    /// Seat of this session (`None` = not a Pulsar seat session: show nothing, hide nothing).
    #[serde(default)]
    pub seat: Option<SeatNumber>,
    /// A one-time credential is armed for this session: show one default tile with
    /// auto-logon and take it with `credprov_take_credentials` (logon scenario only).
    #[serde(default)]
    pub auto_submit: bool,
    /// The filter hides every other provider (only together with `auto_submit`, only in
    /// the logon scenario).
    #[serde(default)]
    pub hide_others: bool,
    /// Seat account shown on the tile (`DOMAIN\user`); no secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// Account SID for `ICredentialProviderCredential2::GetUserSid`, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_sid: Option<String>,
    /// Seats the "switch seat" links offer (empty = no link tile).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub switch_targets: Vec<SeatNumber>,
    /// A `fast_user_switch` hand-over to this console session is under way and no grant
    /// is armed yet: ask again shortly (logon scenario only; the provider polls for a
    /// bounded time and refreshes its tiles). Added 2026-11-13, additive.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pending: bool,
    /// A password-less switch handed the console to this logon screen for `seat`: its
    /// tile is the default (selected) one, without auto-logon (logon scenario only).
    /// Added 2026-11-15, additive.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub preselect: bool,
}

/// What a session agent reports (its session id and SIDs come from the pipe token, these
/// fields are cross-checked).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentReport {
    pub session_id: u32,
    pub event: AgentEvent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    Started,
    TaskDone { task_id: u64 },
    TaskFailed { task_id: u64, reason: String, win32: Option<u32> },
    Stopping,
    /// Effective DPI of the session's monitors, read back by the agent
    /// (`GetDpiForMonitor`, `MDT_EFFECTIVE_DPI`) after a `report_dpi` task. Added
    /// 2026-11-17 (compat-tweaks, additive).
    Dpi { monitors: Vec<MonitorDpi> },
}

/// One monitor of a session as the agent sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitorDpi {
    /// GDI device name of the monitor (for example `DISPLAY3` with the usual prefix).
    pub device: String,
    /// Effective DPI (96 = 100 %).
    pub dpi: u32,
    pub width: u32,
    pub height: u32,
}

impl MonitorDpi {
    /// The scale in percent Windows shows for this DPI (96 = 100 %).
    pub fn percent(&self) -> u32 {
        (self.dpi * 100 + 48) / 96
    }
}

/// Prefix of the one-shot agent's output line (`pulsar-seat-agent --report-dpi`).
pub const DPI_REPORT_PREFIX: &str = "PULSAR_SEAT_DPI ";

/// The one-shot agent's output line.
pub fn dpi_report_line(monitors: &[MonitorDpi]) -> String {
    format!("{DPI_REPORT_PREFIX}{}", serde_json::to_string(monitors).unwrap_or_else(|_| "[]".into()))
}

/// The monitors of the last report line in `text` (other output is ignored).
pub fn parse_dpi_report(text: &str) -> Option<Vec<MonitorDpi>> {
    text.lines().rev().find_map(|l| l.trim().strip_prefix(DPI_REPORT_PREFIX).and_then(|j| serde_json::from_str(j).ok()))
}

#[cfg(test)]
mod dpi_tests {
    use super::*;

    #[test]
    fn report_line_roundtrip() {
        let m = vec![MonitorDpi { device: "DISPLAY1".into(), dpi: 144, width: 2560, height: 1440 }];
        let text = format!("noise\r\n{}\r\n", dpi_report_line(&m));
        assert_eq!(parse_dpi_report(&text), Some(m));
        assert_eq!(parse_dpi_report("nothing"), None);
    }
}

/// One observation of the console raw-input listener. `interface` is the unit's
/// device-interface path as raw input reports it (`RIDI_DEVICENAME`); the service maps it
/// to the unit key. Key codes are reported only while the capture wizard runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConsoleInputEvent {
    /// The unit was used (throttled per unit). `key_sc`: only while the wizard runs, only
    /// the wizard keys F1..F12, top-row digits 1..0 and Esc / Backspace / Enter / Shift /
    /// Alt (set 1, no auto-repeat; maintainer decision 2026-11-13: never every key).
    /// `click`: a mouse button went down.
    Activity {
        interface: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key_sc: Option<u16>,
        #[serde(default)]
        click: bool,
    },
    /// Ctrl + the configured hotkey on this unit (spec 50 §4.1.9).
    Hotkey { interface: String },
    /// Ctrl+Shift + the hotkey (F12 when the hotkey is off) three times on this unit:
    /// every unit back to seat 1. Acted on even while the move hotkey is off.
    Emergency { interface: String },
}

/// What the console listener needs to know, returned on every report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsoleListenerState {
    /// The capture wizard runs: report the wizard keys' make codes.
    #[serde(default)]
    pub wizard_running: bool,
    /// Current input settings (hotkey scan code).
    #[serde(default)]
    pub input: seat_core::config::InputSettings,
    /// The service no longer wants a listener (backend changed, product off): exit.
    #[serde(default)]
    pub stop: bool,
    /// Obsolete (kept so old peers still parse): the service always sends `false` and the
    /// listener ignores it. Maintainer decision 2026-11-13: the listener reports only the
    /// wizard keys (F1..F12, digits, Enter, Backspace, Esc, Shift, Alt), never every key.
    #[serde(default)]
    pub wizard_all_keys: bool,
}

/// Work the service gives a session agent (pushed as `agent_task` events, or returned by
/// `agent_report`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentTask {
    pub id: u64,
    pub task: AgentTaskKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentTaskKind {
    /// `DefineDosDevice` in this logon session (spec 70 §5.2(3)).
    DefineDriveLetter { letter: char, volume: String },
    RemoveDriveLetter { letter: char },
    SetWallpaper { path: String },
    /// Show a localized notice by code (`same_account_warning`, `seat_starting`, …).
    ShowNotice { code: String },
    /// Log this session off after a notice (same-account deny).
    LogOff { after_ms: u32 },
    SetPalmCheck { threshold: u32 },
    /// `fast_user_switch` (spec 35 §5.6): this session just became the console; keep only
    /// these display targets (the seat's monitors) in the desktop and turn the others off.
    /// LUIDs are this boot's (from the service inventory). Added 2026-11-13, additive.
    ApplyTopology { targets: Vec<DisplayTargetRef> },
    /// Restart the graphics driver: Win+Ctrl+Shift+B sent into this session's input
    /// desktop (`SendInput`). HYPOTHESIS: Windows honours the shortcut from injected input.
    /// Added 2026-11-17, additive.
    ResetGraphics,
    /// Tell the session's programs that the user environment changed
    /// (`WM_SETTINGCHANGE` with `Environment`, after the service wrote the seat's
    /// variables). Added 2026-11-17 (compat-tweaks, additive).
    RefreshEnvironment,
    /// Read the effective DPI of the session's monitors and send it back
    /// ([`AgentEvent::Dpi`]). Added 2026-11-17 (compat-tweaks, additive).
    ReportDpi,
    /// Serve the seat's ACL'd volumes (at most these, `drives`) as this session's user
    /// over two pipe ends the service duplicated into the agent process (`read` from the
    /// seat host, `write` to it): the seat host forwards the drive IRPs of the seat's RDP
    /// session (`seat_rdp::channels::broker`). A newer task starts a new link; the old one
    /// ends when its host end closes. Added 2026-11-17 (rdp-listener, additive).
    DriveBroker { read: u64, write: u64, drives: Vec<crate::DriveBrokerDrive> },
}

/// One volume the drive broker may serve: the service's allowlist. The host names the
/// drives it activates over the pipe; the broker refuses any root not listed here, so a
/// compromised seat host cannot reach other files of the seat user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriveBrokerDrive {
    /// Drive-letter root (`E:\`).
    pub root: String,
    /// Volume GUID path the letter must still name.
    pub volume: String,
    #[serde(default)]
    pub read_only: bool,
}

/// A CCD display target of this boot (adapter LUID + target id).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayTargetRef {
    pub adapter_luid: u64,
    pub target_id: u32,
}

/// Pushed events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum Event {
    // Status topic
    ProductState { state: ProductState },
    SeatState { runtime: SeatRuntime },
    ConfigChanged { revision: u64 },
    /// Localized copy by `code` (`service_gap_no_input`, `os_build_changed`, …).
    Notice {
        code: String,
        seat: Option<SeatNumber>,
        detail: String,
    },
    // Devices topic
    DeviceArrived { view: Box<DeviceView> },
    DeviceRemoved { key: DeviceKey },
    DeviceChanged { view: Box<DeviceView> },
    // Activity topic
    DeviceActivity { key: DeviceKey },
    // Wizard topic
    WizardProgress(WizardProgress),
    // Host topic
    InputPackets { seat: SeatNumber, packets: Vec<InputPacket> },
    HostCommand { seat: SeatNumber, command: HostCommand },
    // Agent topic
    AgentTask { session_id: u32, task: AgentTask },
    // Stream topic (protocol 3)
    Stream { seat: SeatNumber, event: StreamEvent },
}

/// Pairing and streaming on a remote seat (Split's `pair_pending`, `pair_ended`,
/// `stream_started`, `stream_ended`, and the Pulsar backend's `otp_changed`,
/// `trust_changed`, `relay_auth_required`; payloads as the stream backend reports them).
/// Admin only, like the whole `stream` topic.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum StreamEvent {
    PairPending(serde_json::Value),
    PairEnded(serde_json::Value),
    StreamStarted(serde_json::Value),
    StreamEnded(serde_json::Value),
    /// The seat's one-time password changed (the pair dialog fetches it again).
    OtpChanged(serde_json::Value),
    /// The trusted client list changed.
    TrustChanged(serde_json::Value),
    /// The relay asks for credentials (the pair dialog shows the relay login).
    RelayAuthRequired(serde_json::Value),
}

impl Event {
    pub fn topic(&self) -> Topic {
        match self {
            Self::ProductState { .. } | Self::SeatState { .. } | Self::ConfigChanged { .. } | Self::Notice { .. } => {
                Topic::Status
            }
            Self::DeviceArrived { .. } | Self::DeviceRemoved { .. } | Self::DeviceChanged { .. } => Topic::Devices,
            Self::DeviceActivity { .. } => Topic::Activity,
            Self::WizardProgress(_) => Topic::Wizard,
            Self::InputPackets { .. } | Self::HostCommand { .. } => Topic::Host,
            Self::AgentTask { .. } => Topic::Agent,
            Self::Stream { .. } => Topic::Stream,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WizardProgress {
    /// `pick_console_monitor`, `keyboard`, `mouse`, `waiting`, `done`, `cancelled`,
    /// `invalid` (save refused, wizard still running; see `issues`).
    pub step: String,
    pub seat: Option<SeatNumber>,
    /// F-key presses counted for the current seat (needs 2).
    pub key_presses: u8,
    /// Mouse targets hit (needs 2).
    pub clicks: u8,
    pub assigned: Vec<(SeatNumber, DeviceKey)>,
    /// Monitor of each seat, index 0 = seat 1 (the per-monitor pages and the number
    /// overlays use this order). Added 2026-11-13 (additive).
    #[serde(default)]
    pub monitors: Vec<DeviceKey>,
    /// Seats the user confirmed (Enter); they show the waiting page.
    #[serde(default)]
    pub confirmed: Vec<SeatNumber>,
    /// The service has seen keys other than F1..F12 from the units (filter path, or the
    /// console listener's digits / control keys), so it handles the any-keyboard control
    /// keys itself (spec 10 §9.2 step 6). The
    /// manager then only acts on keys pressed in its own window that the service cannot
    /// see. Added 2026-11-13 (additive).
    #[serde(default)]
    pub keys_seen: bool,
    /// The unit whose key / click was counted last (feedback on the pages).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_unit: Option<DeviceKey>,
    /// The running variant (pages of kept seat 1 say so). Added 2026-11-13 (additive).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<WizardVariant>,
    /// Step `invalid`: error issue codes (snake_case) that stopped the save. The wizard
    /// keeps running so the user can fix the assignment. Added 2026-11-13 (additive).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub issues: Vec<String>,
}

/// Commands to a running seat host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum HostCommand {
    /// Outputs changed (hot-plug, mode): re-run `host_get_assignment`.
    Reconfigure,
    /// Audio endpoints changed: re-read the audio route.
    RebindAudio { route: AudioRoute },
    /// Show the idle frame / blank (monitor power policy).
    Blank { on: bool },
    /// Disconnect and exit (seat stop).
    Exit,
}

// ---- updates (spec 90 §11 item 10; added 2026-11-13) ----------------------------------

/// Where the update flow stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateState {
    /// Nothing checked since the service started (or the opt-in is off).
    #[default]
    Idle,
    Checking,
    /// The latest release is not newer than this build.
    UpToDate,
    /// A newer release with an installer for this PC's architecture exists.
    Available,
    Downloading,
    /// Downloaded and verified; waiting for the administrator's confirmation.
    Ready,
    /// The installer was started (the service stops during the upgrade).
    Installing,
    /// The last step failed (`reason` / `error`).
    Failed,
}

/// A release found on the update feed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateRelease {
    /// `1.2.3` (tag without the leading `v`).
    pub version: String,
    pub tag: String,
    /// Release page (shown as a link; notes are not rendered).
    #[serde(default)]
    pub page_url: String,
    #[serde(default)]
    pub published_at: String,
    /// Installer asset for this PC, e.g. `PulsarSeat_1.2.3_arm64.msi`.
    pub asset_name: String,
    #[serde(default)]
    pub size: u64,
    /// SHA-256 from the release's checksum file (lowercase hex), when it lists the asset.
    #[serde(default)]
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct UpdateStatus {
    /// `settings.updates.check` (opt-in): no network access while false.
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub weekly: bool,
    pub current_version: String,
    /// `arm64` or `x64` (native machine).
    #[serde(default)]
    pub arch: String,
    #[serde(default)]
    pub state: UpdateState,
    #[serde(default)]
    pub latest: Option<UpdateRelease>,
    /// Unix seconds of the last finished check.
    #[serde(default)]
    pub checked_unix_s: Option<u64>,
    /// Bytes downloaded so far (state `downloading`).
    #[serde(default)]
    pub downloaded: u64,
    /// Publisher of the verified installer (state `ready`).
    #[serde(default)]
    pub signer: Option<String>,
    /// Stable id of the last failure for the UI (`network`, `feed`, `no_asset`,
    /// `no_checksum`, `hash_mismatch`, `unsigned`, `untrusted_signer` (Authenticode of the
    /// installer), `sums_unsigned`, `sums_untrusted`, `sums_mismatch`, `sums_rollback`,
    /// `no_release_key` (release signature of the checksum file), `too_large`, `launch`,
    /// `busy`, `not_ready`, `unsupported`).
    #[serde(default)]
    pub reason: Option<String>,
    /// English detail for logs.
    #[serde(default)]
    pub error: Option<String>,
}
