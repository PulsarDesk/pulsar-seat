// SPDX-License-Identifier: GPL-3.0-only
//! Configuration and status types that travel over the pipe. Validation and persistence
//! live in `split-core`.
//!
//! Protocol v2 (contracts §9) added fields with `#[serde(default)]` only, so a v1 config
//! file still loads. Package-owned structures (session host warnings, coexistence report,
//! termwrap status, display / pad backends, shim health) travel as `serde_json::Value`:
//! this crate must not depend on the packages (they depend on it).

use serde::{Deserialize, Serialize};

/// Log level for the service and the per-seat stream servers. Stored as index 0–4.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verbosity {
    None,
    Error,
    Warning,
    #[default]
    Info,
    Debug,
}

impl Verbosity {
    pub fn index(self) -> u32 {
        self as u32
    }

    pub fn from_index(i: u32) -> Option<Self> {
        Some(match i {
            0 => Self::None,
            1 => Self::Error,
            2 => Self::Warning,
            3 => Self::Info,
            4 => Self::Debug,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GlobalSettings {
    /// Service start type automatic (on) or manual (off).
    pub start_with_windows: bool,
    pub hid_isolation: bool,
    pub process_patching: bool,
    /// Only meaningful when `process_patching` is on.
    pub steam_isolation: bool,
    pub verbosity: Verbosity,
    /// v2: prepare the host for several seat sessions (RDS settings, spec 30 §2.3). Off =
    /// the service writes no RDS settings and starts no seat session.
    pub multi_session: bool,
    /// v2: preferred virtual pad backend; `None` = automatic.
    pub pad_backend: Option<PadBackendPref>,
    /// v2: Bluetooth per-user services only in the console session (spec 60 §10).
    pub bluetooth_console_only: bool,
    /// v2: display backend override; `None` = automatic.
    pub display_backend: Option<DisplayBackendPref>,
}

impl Default for GlobalSettings {
    fn default() -> Self {
        Self {
            start_with_windows: false,
            hid_isolation: false,
            process_patching: false,
            steam_isolation: false,
            verbosity: Verbosity::default(),
            multi_session: false,
            pad_backend: None,
            bluetooth_console_only: true,
            display_backend: None,
        }
    }
}

/// Same wire names as `seat_pads::BackendId`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PadBackendPref {
    Vigem,
    UsbipDs5,
    PulsarPad,
}

/// Same wire names as `split_display::BackendKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayBackendPref {
    RdpSession,
    Vdd,
    PulsarIdd,
}

/// What happens to the seat's Windows session when the seat stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopAction {
    #[default]
    LogOff,
    Disconnect,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DisplaySettings {
    /// Adapter description as shown to the user; `None` = pick the best adapter.
    pub render_adapter: Option<String>,
    /// Static scale (use `scale_percent`) instead of following the client.
    pub scale_static: bool,
    pub scale_percent: u16,
    pub super_sampling_percent: u16,
    pub sdr_white_level_nits: u16,
    pub force_sdr: bool,
    /// v2: PnP instance id of the render adapter (`render_adapter` keeps the description).
    pub render_adapter_id: Option<String>,
    /// v2: default refresh rate, 15–500 Hz.
    pub refresh_hz: u32,
    /// v2: advanced HDR luminance of the seat monitor.
    pub luminance: HdrLuminanceSettings,
}

impl Default for DisplaySettings {
    fn default() -> Self {
        Self {
            render_adapter: None,
            scale_static: false,
            scale_percent: 100,
            super_sampling_percent: 100,
            sdr_white_level_nits: 400,
            force_sdr: false,
            render_adapter_id: None,
            refresh_hz: 60,
            luminance: HdrLuminanceSettings::default(),
        }
    }
}

/// Same fields as `split_display::HdrLuminance`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HdrLuminanceSettings {
    pub min_nits_x10000: u32,
    pub max_nits: u32,
    pub max_full_frame_nits: u32,
}

impl Default for HdrLuminanceSettings {
    fn default() -> Self {
        Self { min_nits_x10000: 10, max_nits: 1000, max_full_frame_nits: 600 }
    }
}

/// Same fields as `seat_audio::SeatAudio`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SeatAudioSettings {
    pub stream_audio: bool,
    /// Later feature (spec 70 §5.1): stored, not used until the microphone path exists.
    pub microphone: bool,
}

impl Default for SeatAudioSettings {
    fn default() -> Self {
        Self { stream_audio: true, microphone: false }
    }
}

/// Controller the stream server emulates (`gamepad` key, spec 80 §4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GamepadPref {
    #[default]
    Auto,
    X360,
    Ds4,
}

/// Capture method (`capture` key); `Default` = key not written (server chooses).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapturePref {
    #[default]
    Default,
    Ddx,
    Wgc,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SeatStreamSettings {
    pub gamepad: GamepadPref,
    pub capture: CapturePref,
    /// `minimum_fps_target` (spec 80 default 1).
    pub min_fps: u32,
    /// Also open the seat's firewall rules on the Domain profile.
    pub firewall_domain: bool,
    /// Keep the pairing state when the seat is deleted.
    pub keep_pairing_on_delete: bool,
}

impl Default for SeatStreamSettings {
    fn default() -> Self {
        Self {
            gamepad: GamepadPref::Auto,
            capture: CapturePref::Default,
            min_fps: 1,
            firewall_domain: false,
            keep_pairing_on_delete: false,
        }
    }
}

/// Stream server a seat runs (contracts §7.4, docs/split/agents/pulsar-host-backend.md). Each
/// seat runs exactly one. Missing in a v2 config = `sunshine`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamBackendKind {
    /// Upstream Sunshine (Moonlight clients).
    #[default]
    Sunshine,
    /// `pulsar-host` (Pulsar clients).
    Pulsar,
}

impl StreamBackendKind {
    pub const ALL: [StreamBackendKind; 2] = [StreamBackendKind::Sunshine, StreamBackendKind::Pulsar];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sunshine => "sunshine",
            Self::Pulsar => "pulsar",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

/// `get_stream_backends`: whether a backend can start seats on this host.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct StreamBackendInfo {
    pub backend: StreamBackendKind,
    /// Manifest present and the binary matches its SHA-256.
    pub installed: bool,
    /// Reason code when not installed (`backend_missing`, `server_hash_mismatch`, …).
    pub reason: Option<String>,
    pub version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AutostartApp {
    /// GUID string; stable key of the entry.
    pub id: String,
    pub path: String,
    pub arguments: String,
    /// Run as SYSTEM inside the seat session instead of as the seat user. Ignored with a
    /// warning since v2 (spec 30 §8): autostart apps always run as the seat user.
    pub run_as_system: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SeatConfig {
    /// GUID string, assigned at creation, never changes (renames keep it).
    pub id: String,
    /// Stable short name: ASCII letters/digits, 1–128 chars, unique (case-insensitive).
    pub name: String,
    /// Host name advertised to clients.
    pub display_name: String,
    /// Local Windows account (SAM name); empty = not chosen yet.
    pub user_name: String,
    /// Stream-server base port; the seat occupies `port-5 ..= port+21`.
    pub port: u16,
    pub start_with_service: bool,
    pub stop_action: StopAction,
    pub display: DisplaySettings,
    pub autostart: Vec<AutostartApp>,
    /// v2: RDP client name of the seat's loopback session (`PSS` + 8 hex), generated by
    /// the service at creation, unique among seats; the manager cannot change it.
    pub rdp_client_name: String,
    /// v2.
    pub audio: SeatAudioSettings,
    /// v2.
    pub stream: SeatStreamSettings,
    /// Stream server of this seat; `sunshine` when missing (v2 configs). Changing it needs
    /// the seat stopped (the whole config is locked while the seat is active).
    pub stream_backend: StreamBackendKind,
    /// Last display mode a client asked for and the display applied (contracts §3.3,
    /// spec 40 §4.4). Service-owned: the manager cannot change it; used for the session
    /// size at the next seat start.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_mode: Option<LastMode>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LastMode {
    pub width: u32,
    pub height: u32,
    pub hz: u32,
}

impl LastMode {
    /// Plausible session size (RDP accepts 200–8192 per side; we keep 640×480 … 7680×4320).
    pub fn is_plausible(&self) -> bool {
        (640..=7680).contains(&self.width) && (480..=4320).contains(&self.height) && (15..=500).contains(&self.hz)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct NewSeat {
    pub name: String,
    /// `None` = let the service allocate the next free port block.
    pub port: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeatState {
    #[default]
    Stopped,
    Booting,
    Running,
    Stopping,
    /// Start failed or the seat broke while running; needs an explicit start.
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SeatStatus {
    pub name: String,
    pub display_name: String,
    pub state: SeatState,
    pub port: u16,
    pub user_name: String,
    pub has_password: bool,
    /// Stream-server crashes since the seat was last started.
    pub crash_count: u32,
    pub last_error: Option<String>,
    /// v2: reason code of the last failure (`client_auth`, `display_timeout`, …).
    pub failure: Option<String>,
    /// v2: Windows session id while the seat runs.
    pub session_id: Option<u32>,
    /// v2: boot step while `state == booting` (`user`, `session`, `display`, `stream`, …).
    pub phase: Option<String>,
    /// A client is streaming from the seat (between `stream_started` and `stream_ended`).
    pub streaming: bool,
    /// Name of the streaming client when the stream server reports one.
    pub stream_client: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ServiceStatus {
    pub service_version: String,
    pub protocol_version: u32,
    /// Running in `--console` dev mode rather than under the SCM.
    pub console_mode: bool,
    pub seats: Vec<SeatStatus>,
    /// v2: coexistence passive mode (reference product present): no system changes and
    /// no seat starts.
    pub passive: bool,
    /// v2: `home` | `client_multi_session_unavailable` | `server_without_rdsh` |
    /// `server_rdsh`; `None` until known.
    pub host_class: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalUser {
    pub name: String,
    pub enabled: bool,
}

// ---- v2 response bodies ----

/// `host_info` (contracts §9.2).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HostInfo {
    /// `EditionID` (e.g. `ServerStandard`, `Professional`).
    pub edition: String,
    pub build: u32,
    pub class: String,
    pub rds_ready: bool,
    pub listener_port: Option<u16>,
    /// `{mode, grace_days_left}`.
    pub licensing: Option<serde_json::Value>,
    pub max_seats: Option<u32>,
    /// `split_session::HostWarning` values (`{kind, …}`).
    pub warnings: Vec<serde_json::Value>,
    /// `seat_setup::CoexistReport` (`{mode, findings[], …}`).
    pub coexistence: Option<serde_json::Value>,
    /// `split_termwrap::TermwrapStatus`.
    pub termwrap: Option<serde_json::Value>,
    /// `split_display::BackendInfo[]`: driver backend status and HDR availability.
    pub display_backends: Vec<serde_json::Value>,
    /// `seat_pads::BackendAvailability[]`.
    pub pad_backends: Vec<serde_json::Value>,
    /// Current `GlobalSettings::multi_session`.
    pub multi_session: bool,
    /// Reason code when the session host information could not be read.
    pub error: Option<String>,
}

/// `seat_runtime` (contracts §9.2).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SeatRuntime {
    pub session_id: Option<u32>,
    pub client_name: String,
    pub state: SeatState,
    pub phase: Option<String>,
    pub failure: Option<String>,
    /// `{backend, state?, hdr: "ok" | HdrUnavailable code}`.
    pub display: Option<serde_json::Value>,
    /// `{state, limitations[]}`.
    pub audio: Option<serde_json::Value>,
    /// `PadStatus[]`.
    pub pads: Vec<serde_json::Value>,
    /// `{flavour, status?, capabilities[], crash_count, control}`.
    pub stream: Option<serde_json::Value>,
    /// `ShimHealth[]` reported from the seat session.
    pub isolation: Vec<serde_json::Value>,
}

/// How a pending request is answered in the pair dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairMode {
    /// Sunshine: the operator types the PIN the client shows (`submit_pin`).
    #[default]
    Pin,
    /// Pulsar: the operator allows / denies the request (`answer_peer_request`).
    Approve,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PendingPair {
    pub request_id: Option<u32>,
    pub device_name: String,
    pub received_at: String,
    pub mode: PairMode,
    /// Pulsar: `connect` | `pair`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Pulsar: first 16 hex digits of the client's public key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_fp: Option<String>,
    /// Pulsar: `none` | `wrong` | `wait`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password_state: Option<String>,
    /// Pulsar: relay id or address of the client.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_in_s: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PairState {
    pub pending: Option<PendingPair>,
    /// Result of the last finished pairing (`paired`, `wrong_pin`, `timeout`, …).
    pub last_result: Option<String>,
    /// Pairing can be done from the manager on this seat (control channel up).
    pub available: bool,
    /// Backend of the seat: decides the dialog mode (PIN or approve).
    pub backend: StreamBackendKind,
    /// Pulsar: the relay asks for credentials (`answer_relay_auth`).
    pub relay_auth_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PairedClient {
    pub uuid: String,
    pub name: String,
}

/// Same fields as `seat_shim_core::ShimTarget`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ShimTargetDto {
    pub exe_name: String,
    pub product_name: Option<String>,
    pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct IsolationStatus {
    pub installed: bool,
    /// `none` (device security only) | `apphelp`.
    pub mechanism: String,
    pub kill_switch: bool,
    pub foreign: Option<String>,
    pub targets: Vec<ShimTargetDto>,
    pub mask_apps: Vec<String>,
    pub health: Vec<serde_json::Value>,
    /// Installed module copies (`path (version)`).
    pub modules: Vec<String>,
    /// The shim module exists for this host architecture (x64 / x86 only, spec 50 §14).
    pub supported: bool,
}

/// One game with a DXVK redirect managed by Pulsar Split (spec 50 §10.2).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DxvkGame {
    /// Full path of the game executable.
    pub exe: String,
    /// Our marker file sits next to the game: DXVK files are in place.
    pub deployed: bool,
    /// DXVK version of the deployed files.
    pub version: Option<String>,
    /// Redirected Direct3D DLLs without extension (`d3d8`, `d3d9`).
    pub dlls: Vec<String>,
    /// The marker exists but could not be read (`marker_unreadable`).
    pub error: Option<String>,
}

/// `dxvk_status`: the pinned DXVK release and the games the service has deployed it to.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DxvkStatus {
    /// Version of the downloaded, pinned release; `None` = not downloaded yet
    /// (`pulsar-split-shimctl dxvk download`).
    pub release: Option<String>,
    pub games: Vec<DxvkGame>,
}

/// Seat user's Windows account picture (spec 10 §2.1), small enough for the nav list.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UserPicture {
    /// `image/png` | `image/jpeg` | `image/bmp` | `image/gif`; empty with `data` = no picture.
    pub mime: String,
    /// Base64 of the image file; empty = the user has no picture (the manager shows the
    /// initial).
    pub data: String,
}

/// Result of the operator-triggered stale RDP audio endpoint cleanup (spec 70 §2.6).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioCleanup {
    /// Device instance ids removed.
    pub removed: Vec<String>,
    /// `(instance id, reason code)` of nodes kept.
    pub failed: Vec<(String, String)>,
}

/// One entry of the service event ring (`poll_events`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ServiceEvent {
    pub seq: u64,
    /// Unix milliseconds.
    pub time: u64,
    /// Seat name.
    pub seat: Option<String>,
    /// `seat_phase` | `seat_failed` | `pair_pending` | `pair_ended` | `stream_started` |
    /// `stream_ended` | `host_warning`.
    pub kind: String,
    pub data: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Events {
    pub next: u64,
    pub events: Vec<ServiceEvent>,
}

/// Result of `test_seat_credentials` (spec 10 §4.4). The password never travels back.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CredentialTest {
    pub ok: bool,
    /// Win32 error of the failed logon (1326 wrong password, 1331 disabled, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<u32>,
    /// Windows error text (system language) for the failure, for display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Account state seen by the service; the manager offers `enable_unlock_account`.
    #[serde(default)]
    pub disabled: bool,
    #[serde(default)]
    pub locked_out: bool,
    #[serde(default)]
    pub password_expired: bool,
}
