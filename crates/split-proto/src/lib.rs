// SPDX-License-Identifier: GPL-3.0-only
//! Pulsar Split control protocol: the messages the manager and the seat service exchange
//! over a local named pipe, and the framing used on that pipe.
//!
//! Wire format: every message is one frame = 4-byte little-endian payload length followed
//! by a UTF-8 JSON payload (at most [`MAX_FRAME_LEN`] bytes). One request frame is answered
//! by exactly one response frame carrying the same `id`. Every frame carries
//! [`PROTOCOL_VERSION`]; the service rejects requests with another version.
//!
//! Version history: 1 = Phase 1. 2 = contracts §9 (host info, multi-session, termwrap,
//! seat runtime, pairing, apps, isolation, event polling; new error codes + `reason`).

pub mod framing;
pub mod model;
pub mod spawn;

use serde::{Deserialize, Serialize};
use std::fmt;

pub use framing::{FrameError, MAX_FRAME_LEN, read_frame, read_message, write_frame, write_message};
pub use model::*;

/// Bumped on every incompatible change to [`Request`] / [`Response`].
pub const PROTOCOL_VERSION: u32 = 2;

/// Name of the control pipe. Only Administrators and SYSTEM may open it (see the service).
pub const PIPE_NAME: &str = r"\\.\pipe\PulsarSplit.Control";

/// Longest `poll_events` wait the service accepts.
pub const MAX_POLL_WAIT_MS: u32 = 2_000;

/// A string whose content is never printed by `Debug` (passwords, PINs) and is wiped
/// (zeroized) when dropped, so copies do not linger in service memory. Avoid needless
/// clones; every clone is wiped on its own drop.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Secret(pub String);

impl Drop for Secret {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.0.zeroize();
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub version: u32,
    pub id: u64,
    pub body: RequestBody,
}

impl Request {
    pub fn new(id: u64, body: RequestBody) -> Self {
        Self { version: PROTOCOL_VERSION, id, body }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum RequestBody {
    /// Handshake; answered with [`ResponseBody::Hello`].
    Hello,
    GetStatus,
    ListSeats,
    GetSeat { name: String },
    CreateSeat(NewSeat),
    /// Replace the seat called `name` with `seat`. `seat.name` may differ (rename).
    /// `stream_backend` (2026-11-13): the backend to store; when absent (a client from
    /// before stream backends) the stored one is kept and `seat.stream_backend` is ignored.
    UpdateSeat {
        name: String,
        seat: SeatConfig,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stream_backend: Option<StreamBackendKind>,
    },
    DeleteSeat { name: String },
    /// Write-only: the password is stored by the service and never returned.
    SetSeatPassword { name: String, password: Secret },
    /// Asynchronous since v2: answered once the start is under way; progress arrives in
    /// the status (`state`, `phase`, `failure`) and as `seat_phase` / `seat_failed` events.
    StartSeat { name: String },
    StopSeat { name: String },
    GetGlobalSettings,
    SetGlobalSettings(GlobalSettings),
    ListLocalUsers,
    /// Forward a pairing PIN to the seat's stream server. Answered with `ack` once the PIN
    /// is on its way; the outcome arrives as a `pair_ended` event / in `pair_state`.
    SubmitPin {
        name: String,
        pin: Secret,
        client_name: String,
        /// v2: the pending request to answer; `None` = the current one.
        #[serde(default)]
        request_id: Option<u32>,
    },
    // ---- v2 ----
    GetHostInfo,
    SetMultiSession { enabled: bool },
    GetTermwrapStatus,
    /// EXPERIMENTAL module. Stops every seat first.
    SetTermwrap {
        enabled: bool,
        #[serde(default)]
        wrapper_path: Option<String>,
        #[serde(default)]
        ack_warning_version: u32,
    },
    GetSeatRuntime { name: String },
    ListRenderAdapters,
    GetPadBackends,
    GetPairState { name: String },
    CancelPair { name: String },
    ListPairedClients { name: String },
    UnpairClient { name: String, uuid: String },
    UnpairAll { name: String },
    /// New certificate / identity; stops the seat and unpairs every client.
    RegenerateSeatIdentity { name: String, confirm: bool },
    GetApps { name: String },
    /// `split_stream::apps::AppEntry` values.
    SetApps { name: String, apps: Vec<serde_json::Value> },
    GetIsolationStatus,
    SetShimTargets { targets: Vec<ShimTargetDto> },
    SetAppMasking { exe_path: String, enabled: bool },
    /// `dlls`: `d3d8` | `d3d9`.
    SetDxvk { game_exe: String, enabled: bool, #[serde(default)] dlls: Vec<String> },
    /// Events with `seq > after`; waits up to `wait_ms` (≤ [`MAX_POLL_WAIT_MS`]) when none.
    PollEvents { after: u64, #[serde(default)] wait_ms: u32 },
    // ---- v2 additions (backward compatible: new variants only) ----
    /// Spec 10 §4.4: logon test with the stored password; answered with
    /// [`ResponseBody::CredentialTest`]. Changes nothing on the account.
    TestSeatCredentials { name: String },
    /// Enable and / or unlock the seat user. Sent only after the operator confirmed;
    /// `confirm` must be true.
    EnableUnlockAccount { name: String, enable: bool, unlock: bool, confirm: bool },
    /// Spec 10 §10 log viewer: last `lines` (1–1000) lines, redacted (no secrets, no
    /// pairing material). `source`: `service` or `seat:<seat name>`. Answered with
    /// [`ResponseBody::LogLines`].
    GetLogTail { source: String, lines: u32 },
    /// Restart Windows (after the process-patching change, spec 10 §3.2). Stops every seat
    /// first; `confirm` must be true.
    RebootHost { confirm: bool },
    // ---- v2 additions: stream backends (docs/agents/pulsar-host-backend.md §3.7) ----
    /// Availability of every stream backend; answered with [`ResponseBody::StreamBackends`].
    GetStreamBackends,
    /// Pulsar seats: allow / deny a pending connect or pair request. Sunshine seats answer
    /// `unsupported` (reason `backend_sunshine`).
    AnswerPeerRequest {
        name: String,
        request_id: u32,
        allow: bool,
        #[serde(default)]
        view_only: bool,
    },
    /// Pulsar seats, write-only: the permanent connect password (empty = none). Stored by
    /// the service (DPAPI) and never returned.
    SetSeatConnectPassword { name: String, password: Secret },
    /// Pulsar seats: current one-time password; answered with [`ResponseBody::Otp`]. The
    /// manager shows it and never stores it.
    GetSeatOtp { name: String },
    /// Pulsar seats: new one-time password; answered with [`ResponseBody::Otp`].
    RotateSeatOtp { name: String },
    /// Pulsar seats: credentials the relay asked for (`relay_auth_required`).
    AnswerRelayAuth {
        name: String,
        password: Secret,
        #[serde(default)]
        totp: Option<Secret>,
    },
    // ---- v2 additions (2026-11-13, final-10) ----
    /// Pinned DXVK release and the games with a DXVK redirect; answered with
    /// [`ResponseBody::DxvkStatus`].
    GetDxvkStatus,
    /// Account picture of a local user (spec 10 §2.1); answered with
    /// [`ResponseBody::UserPicture`] (empty when the user has none).
    GetUserPicture { user: String },
    /// Spec 70 §2.6 maintenance: remove non-present RDP audio endpoint nodes of ended
    /// sessions. Refused while a seat is starting; `confirm` must be true. Answered with
    /// [`ResponseBody::AudioCleanup`].
    CleanupAudioEndpoints { confirm: bool },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub version: u32,
    pub id: u64,
    pub result: Result<ResponseBody, ProtoError>,
}

impl Response {
    pub fn new(id: u64, result: Result<ResponseBody, ProtoError>) -> Self {
        Self { version: PROTOCOL_VERSION, id, result }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ResponseBody {
    Hello { service_version: String, protocol_version: u32 },
    Status(ServiceStatus),
    Seats(Vec<SeatConfig>),
    Seat(SeatConfig),
    GlobalSettings(GlobalSettings),
    Users(Vec<LocalUser>),
    /// Generic success without data.
    Ack,
    // ---- v2 ----
    HostInfo(HostInfo),
    /// `split_termwrap::TermwrapStatus` plus `warning_text` (Turkish, verbatim).
    TermwrapStatus(serde_json::Value),
    SeatRuntime(SeatRuntime),
    /// `split_display::RenderAdapter` values.
    RenderAdapters(Vec<serde_json::Value>),
    /// `split_pads::BackendAvailability` values.
    PadBackends(Vec<serde_json::Value>),
    PairState(PairState),
    Paired { uuid: String },
    PairedClients(Vec<PairedClient>),
    /// `split_stream::apps::AppEntry` values.
    Apps(Vec<serde_json::Value>),
    IsolationStatus(IsolationStatus),
    Events(Events),
    CredentialTest(CredentialTest),
    LogLines(Vec<String>),
    StreamBackends(Vec<StreamBackendInfo>),
    /// One-time password of a Pulsar seat.
    Otp { otp: Secret },
    DxvkStatus(DxvkStatus),
    UserPicture(UserPicture),
    AudioCleanup(AudioCleanup),
}

/// Machine-readable error. The manager maps `reason` (if set) or `code` to Turkish copy;
/// `message` is an English diagnostic for logs only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{code:?}: {message}")]
pub struct ProtoError {
    pub code: ErrorCode,
    pub message: String,
    /// Name of the offending field, when the error is about one input field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// v2: package reason code (`client_auth`, `wrapper_unsigned`, …), contracts §9.3.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl ProtoError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self { code, message: message.into(), field: None, reason: None }
    }

    pub fn with_field(mut self, field: impl Into<String>) -> Self {
        self.field = Some(field.into());
        self
    }

    pub fn with_reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }

    /// Packages put their reason code first in `message` (`"client_auth: NLA failed"`).
    /// Fills `reason` from that prefix when it is a snake_case word and `reason` is unset.
    pub fn with_reason_from_message(mut self) -> Self {
        if self.reason.is_none()
            && let Some((head, _)) = self.message.split_once(':')
            && !head.is_empty()
            && head.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        {
            self.reason = Some(head.to_string());
        }
        self
    }

    /// `reason` if set, else the snake_case name of `code`.
    pub fn reason_or_code(&self) -> String {
        self.reason.clone().unwrap_or_else(|| self.code.as_str().to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    VersionMismatch,
    BadRequest,
    NotFound,
    InvalidName,
    DuplicateName,
    InvalidDisplayName,
    InvalidPort,
    PortOverlap,
    NoFreePort,
    InvalidValue,
    InvalidUser,
    UserInUse,
    MissingCredentials,
    SeatRunning,
    NotImplemented,
    Store,
    ServiceUnavailable,
    Internal,
    // ---- v2 (contracts §9.3) ----
    Unsupported,
    EditionUnsupported,
    Coexistence,
    SeatFailed,
    Timeout,
    DriverMissing,
    PolicyBlocked,
    WarningNotAcknowledged,
    NoPendingRequest,
    WrongPin,
    HashMismatch,
    Firewall,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::VersionMismatch => "version_mismatch",
            Self::BadRequest => "bad_request",
            Self::NotFound => "not_found",
            Self::InvalidName => "invalid_name",
            Self::DuplicateName => "duplicate_name",
            Self::InvalidDisplayName => "invalid_display_name",
            Self::InvalidPort => "invalid_port",
            Self::PortOverlap => "port_overlap",
            Self::NoFreePort => "no_free_port",
            Self::InvalidValue => "invalid_value",
            Self::InvalidUser => "invalid_user",
            Self::UserInUse => "user_in_use",
            Self::MissingCredentials => "missing_credentials",
            Self::SeatRunning => "seat_running",
            Self::NotImplemented => "not_implemented",
            Self::Store => "store",
            Self::ServiceUnavailable => "service_unavailable",
            Self::Internal => "internal",
            Self::Unsupported => "unsupported",
            Self::EditionUnsupported => "edition_unsupported",
            Self::Coexistence => "coexistence",
            Self::SeatFailed => "seat_failed",
            Self::Timeout => "timeout",
            Self::DriverMissing => "driver_missing",
            Self::PolicyBlocked => "policy_blocked",
            Self::WarningNotAcknowledged => "warning_not_acknowledged",
            Self::NoPendingRequest => "no_pending_request",
            Self::WrongPin => "wrong_pin",
            Self::HashMismatch => "hash_mismatch",
            Self::Firewall => "firewall",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_json_shape_is_stable() {
        let req = Request::new(7, RequestBody::StartSeat { name: "seatA".into() });
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(json, r#"{"version":2,"id":7,"body":{"type":"start_seat","data":{"name":"seatA"}}}"#);
        let unit = serde_json::to_string(&RequestBody::GetStatus).unwrap();
        assert_eq!(unit, r#"{"type":"get_status"}"#);
        let unit = serde_json::to_string(&RequestBody::GetHostInfo).unwrap();
        assert_eq!(unit, r#"{"type":"get_host_info"}"#);
    }

    #[test]
    fn response_roundtrip_with_error() {
        let resp = Response::new(
            3,
            Err(ProtoError::new(ErrorCode::PortOverlap, "overlaps seatB").with_field("port")),
        );
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains(r#""code":"port_overlap""#), "{json}");
        assert!(!json.contains("reason"), "{json}");
        let back: Response = serde_json::from_str(&json).unwrap();
        assert_eq!(back, resp);
    }

    #[test]
    fn reason_from_message_prefix() {
        let e = ProtoError::new(ErrorCode::ServiceUnavailable, "client_auth: NLA failed").with_reason_from_message();
        assert_eq!(e.reason.as_deref(), Some("client_auth"));
        let e = ProtoError::new(ErrorCode::Internal, "Writing seat files: x").with_reason_from_message();
        assert_eq!(e.reason, None);
        assert_eq!(e.reason_or_code(), "internal");
        let json = serde_json::to_string(&ProtoError::new(ErrorCode::WrongPin, "x").with_reason("wrong_pin")).unwrap();
        assert!(json.contains(r#""reason":"wrong_pin""#), "{json}");
    }

    #[test]
    fn error_code_names_match_serde() {
        for c in [ErrorCode::VersionMismatch, ErrorCode::Unsupported, ErrorCode::WarningNotAcknowledged, ErrorCode::Firewall] {
            assert_eq!(serde_json::to_string(&c).unwrap(), format!("\"{}\"", c.as_str()));
        }
    }

    #[test]
    fn v1_config_json_still_loads_with_v2_defaults() {
        let seat: SeatConfig = serde_json::from_str(r#"{"id":"x","name":"A","port":47989}"#).unwrap();
        assert_eq!(seat.display.refresh_hz, 60);
        assert!(seat.audio.stream_audio);
        assert_eq!(seat.stream.min_fps, 1);
        let g: GlobalSettings = serde_json::from_str(r#"{"hid_isolation":true}"#).unwrap();
        assert!(g.bluetooth_console_only && !g.multi_session && g.hid_isolation);
    }

    #[test]
    fn submit_pin_request_id_is_optional() {
        let b: RequestBody =
            serde_json::from_str(r#"{"type":"submit_pin","data":{"name":"A","pin":"1234","client_name":"tv"}}"#).unwrap();
        assert!(matches!(b, RequestBody::SubmitPin { request_id: None, .. }));
    }

    #[test]
    fn stream_backend_defaults_to_sunshine() {
        let seat: SeatConfig = serde_json::from_str(r#"{"id":"x","name":"A","port":48100}"#).unwrap();
        assert_eq!(seat.stream_backend, StreamBackendKind::Sunshine);
        let seat: SeatConfig = serde_json::from_str(r#"{"name":"A","stream_backend":"pulsar"}"#).unwrap();
        assert_eq!(seat.stream_backend, StreamBackendKind::Pulsar);
        assert!(serde_json::from_str::<SeatConfig>(r#"{"stream_backend":"other"}"#).is_err());
        for k in StreamBackendKind::ALL {
            assert_eq!(serde_json::to_value(k).unwrap(), k.as_str());
            assert_eq!(StreamBackendKind::parse(k.as_str()), Some(k));
        }
        let p: PendingPair = serde_json::from_str(r#"{"request_id":1,"device_name":"TV"}"#).unwrap();
        assert_eq!(p.mode, PairMode::Pin);
    }

    #[test]
    fn new_backend_requests_keep_secrets_out_of_debug() {
        let b = RequestBody::SetSeatConnectPassword { name: "a".into(), password: Secret("hunter2".into()) };
        assert!(!format!("{b:?}").contains("hunter2"));
        let b = RequestBody::AnswerRelayAuth { name: "a".into(), password: Secret("pw9".into()), totp: Some(Secret("123456".into())) };
        let dbg = format!("{b:?}");
        assert!(!dbg.contains("pw9") && !dbg.contains("123456"), "{dbg}");
        let r = ResponseBody::Otp { otp: Secret("abc123".into()) };
        assert!(!format!("{r:?}").contains("abc123"));
        let b: RequestBody =
            serde_json::from_str(r#"{"type":"answer_peer_request","data":{"name":"A","request_id":3,"allow":true}}"#).unwrap();
        assert_eq!(b, RequestBody::AnswerPeerRequest { name: "A".into(), request_id: 3, allow: true, view_only: false });
        assert_eq!(serde_json::to_string(&RequestBody::GetStreamBackends).unwrap(), r#"{"type":"get_stream_backends"}"#);
    }

    #[test]
    fn secret_is_not_debug_printed() {
        let body = RequestBody::SetSeatPassword { name: "a".into(), password: Secret("hunter2".into()) };
        let dbg = format!("{body:?}");
        assert!(!dbg.contains("hunter2"), "{dbg}");
        // ...but it is still transported.
        assert!(serde_json::to_string(&body).unwrap().contains("hunter2"));
    }
}
