// SPDX-License-Identifier: GPL-3.0-only
//! Remote seat wire types (from Pulsar Split `split-proto::model`, merge step 4): status,
//! pairing, isolation, DXVK and event bodies. The config types are in
//! `seat_core::remote`.

pub use seat_core::remote::StreamBackendKind;
use serde::{Deserialize, Serialize};

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
    /// Current `seat_core::remote::RemoteSettings::multi_session`.
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
