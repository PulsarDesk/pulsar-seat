// SPDX-License-Identifier: GPL-3.0-only
//! Pulsar Split's protocol 2 request and response enums. Since merge step 6 no pipe serves
//! them: `seat-service`'s remote seat worker uses them internally and `apps/split-manager`
//! compiles against them until step 9.
//! **Transitional**: new code uses the v3 [`crate::RequestBody`] on the one Pulsar Seat
//! pipe (architecture.md section 10.2). Frames are stamped with [`crate::PROTOCOL_VERSION`]
//! (3), the one protocol version of the product.

use super::*;
use crate::PROTOCOL_VERSION;
use serde::{Deserialize, Serialize};

/// Name of the control pipe. Only Administrators and SYSTEM may open it (see the service).
pub const PIPE_NAME: &str = r"\\.\pipe\PulsarSplit.Control";

/// Longest `poll_events` wait the service accepts.
pub const MAX_POLL_WAIT_MS: u32 = 2_000;

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
        seat: RemoteSeatRecord,
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
    SetGlobalSettings(RemoteSettings),
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
    /// `seat_stream::apps::AppEntry` values.
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
    // ---- v2 additions: stream backends (docs/split/agents/pulsar-host-backend.md §3.7) ----
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
    Seats(Vec<RemoteSeatRecord>),
    Seat(RemoteSeatRecord),
    GlobalSettings(RemoteSettings),
    Users(Vec<LocalUser>),
    /// Generic success without data.
    Ack,
    // ---- v2 ----
    HostInfo(HostInfo),
    /// `seat_session::termwrap::TermwrapStatus` plus `warning_text` (Turkish, verbatim).
    TermwrapStatus(serde_json::Value),
    SeatRuntime(SeatRuntime),
    /// `split_display::RenderAdapter` values.
    RenderAdapters(Vec<serde_json::Value>),
    /// `seat_pads::BackendAvailability` values.
    PadBackends(Vec<serde_json::Value>),
    PairState(PairState),
    Paired { uuid: String },
    PairedClients(Vec<PairedClient>),
    /// `seat_stream::apps::AppEntry` values.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_json_shape_is_stable() {
        let req = Request::new(7, RequestBody::StartSeat { name: "seatA".into() });
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(json, r#"{"version":3,"id":7,"body":{"type":"start_seat","data":{"name":"seatA"}}}"#);
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

    // Moved from split-proto (merge step 4): `reason_from_message_prefix` is now
    // `seat_core::error::tests::reason_from_message_prefix`.

    #[test]
    fn error_code_names_match_serde() {
        for c in [ErrorCode::VersionMismatch, ErrorCode::Unsupported, ErrorCode::WarningNotAcknowledged, ErrorCode::Firewall] {
            assert_eq!(serde_json::to_string(&c).unwrap(), format!("\"{}\"", c.as_str()));
        }
    }

    /// Moved from split-proto `framing::tests::message_roundtrip`.
    #[test]
    fn message_roundtrip() {
        let req = Request::new(1, RequestBody::DeleteSeat { name: "x1".into() });
        let mut buf = Vec::new();
        crate::write_message(&mut buf, &req).unwrap();
        let back: Request = crate::read_message(&mut std::io::Cursor::new(buf)).unwrap().unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn v1_config_json_still_loads_with_v2_defaults() {
        let seat: RemoteSeatRecord = serde_json::from_str(r#"{"id":"x","name":"A","port":47989}"#).unwrap();
        assert_eq!(seat.display.refresh_hz, 60);
        assert!(seat.audio.stream_audio);
        assert_eq!(seat.stream.min_fps, 1);
        let g: RemoteSettings = serde_json::from_str(r#"{"hid_isolation":true}"#).unwrap();
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
        let seat: RemoteSeatRecord = serde_json::from_str(r#"{"id":"x","name":"A","port":48100}"#).unwrap();
        assert_eq!(seat.stream_backend, StreamBackendKind::Sunshine);
        let seat: RemoteSeatRecord = serde_json::from_str(r#"{"name":"A","stream_backend":"pulsar"}"#).unwrap();
        assert_eq!(seat.stream_backend, StreamBackendKind::Pulsar);
        assert!(serde_json::from_str::<RemoteSeatRecord>(r#"{"stream_backend":"other"}"#).is_err());
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
