// SPDX-License-Identifier: GPL-3.0-only
//! Pulsar Seat IPC: the messages exchanged over the service's one named pipe, the framing,
//! and the authorisation table.
//!
//! - Pipe [`PIPE_NAME`], byte mode, `PIPE_REJECT_REMOTE_CLIENTS`, first instance created
//!   with `FILE_FLAG_FIRST_PIPE_INSTANCE`, DACL [`PIPE_SDDL`].
//! - Client → service: one [`Request`] per frame. Service → client: one [`ServerFrame`]
//!   per frame — a [`Response`] (same `id` as the request) or, after `subscribe`, an
//!   [`EventFrame`] pushed at any time.
//! - First request on every connection must be `hello` (version handshake, client kind).
//!   The service answers `version_mismatch` when the client's major
//!   [`PROTOCOL_VERSION`] is not in `MIN_PROTOCOL_VERSION..=PROTOCOL_VERSION`.
//! - Every request is checked against [`RequestBody::access`] using the caller's token
//!   (impersonation on the pipe) and, for service-started processes, its process id.
//! - Secrets travel as [`seat_core::Secret`] and are never printed by `Debug`; no response
//!   ever contains a seat password except `host_take_credentials`, once, to the verified
//!   seat host the service started itself.
//!
//! Version history: 1 = first contract (2026-11-12). 3 = seat kinds (merge step 4,
//! 2026-11-14): Pulsar Split's v2 requests join as `Access::Admin`, config schema 2,
//! `wrong_seat_kind` / `session_unavailable`, topic `stream` (architecture.md section 10.2).
//! 2 was Pulsar Split's own protocol and is never spoken on this pipe.

pub mod framing;
pub mod hyperv;
pub mod messages;
pub mod remote;
pub mod steam;
pub mod wrds;

use serde::{Deserialize, Serialize};

pub use framing::{FrameError, MAX_FRAME_LEN, read_frame, read_message, write_frame, write_message};
pub use messages::*;
pub use seat_core::error::{ErrorCode, ProtoError};

/// Current protocol version. Additive changes (new optional fields, new requests) keep it;
/// incompatible changes bump it and update `MIN_PROTOCOL_VERSION`.
pub const PROTOCOL_VERSION: u32 = 3;
/// Oldest client protocol the service still accepts. Requests whose shape changed in 3
/// also need a hello of 3 ([`RequestBody::min_protocol`]).
pub const MIN_PROTOCOL_VERSION: u32 = 1;

/// The service's control pipe.
pub const PIPE_NAME: &str = r"\\.\pipe\PulsarSeat.Control";

/// Pipe DACL (protected): SYSTEM and Administrators full; interactive users read/write so a
/// non-elevated manager, the session agents and the app API can connect. What a caller may
/// *do* is decided per request by [`Access`] against its token, never by the DACL alone.
///
/// Interactive users get an explicit mask, `0x0012019b` = `FILE_GENERIC_READ |
/// FILE_GENERIC_WRITE` **without** `FILE_APPEND_DATA` (0x4). On a named pipe that bit is
/// `FILE_CREATE_PIPE_INSTANCE`; with `GW` any standard user could create rogue server
/// instances on our name (up to the shared instance limit), starve the service's own
/// instances and make clients land on an untrusted server. Clients therefore open with
/// [`PIPE_CLIENT_ACCESS`], never `GENERIC_WRITE`.
pub const PIPE_SDDL: &str = "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;0x0012019b;;;IU)";

/// SACL part the service appends to the pipe's security descriptor (after any DACL
/// additions): a Low mandatory label with no-write-up, so Low-integrity seat hosts and the
/// console listener may write to the pipe (added 2026-11-13). The DACL still decides access.
pub const PIPE_LOW_LABEL_SDDL: &str = "S:(ML;;NW;;;LW)";

/// Desired access every client uses to open [`PIPE_NAME`]: `FILE_GENERIC_READ |
/// FILE_WRITE_DATA | FILE_WRITE_ATTRIBUTES` (0x0012018b). Asking for `GENERIC_WRITE` would
/// include `FILE_APPEND_DATA` (= `FILE_CREATE_PIPE_INSTANCE`), which [`PIPE_SDDL`] denies to
/// interactive users, so the open would fail. `FILE_WRITE_ATTRIBUTES` lets the client set the
/// pipe read mode.
pub const PIPE_CLIENT_ACCESS: u32 = 0x0012_018b;

/// Longest `poll_events` wait the service accepts.
pub const MAX_POLL_WAIT_MS: u32 = 5_000;

/// The console listener reports at least this often (empty report = heartbeat); the
/// service treats it as gone after [`CONSOLE_LISTENER_TIMEOUT_MS`] without a report.
pub const CONSOLE_REPORT_HEARTBEAT_MS: u64 = 1_000;
/// See [`CONSOLE_REPORT_HEARTBEAT_MS`].
pub const CONSOLE_LISTENER_TIMEOUT_MS: u64 = 5_000;
/// Most events one `console_input_report` may carry (the service ignores the rest).
pub const MAX_CONSOLE_REPORT_EVENTS: usize = 512;

/// Events kept for `poll_events` / late subscribers.
pub const EVENT_BACKLOG: usize = 1024;

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

/// Everything the service sends.
// The config view inside a response grew with the compat-tweaks settings (2026-11-17);
// frames are short-lived and boxing would only churn every call site.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerFrame {
    Response(Response),
    Event(EventFrame),
}

/// One pushed event. `seq` increases by one per event across the service's life, so a
/// client can detect gaps and re-read state with `get_status` / `list_devices`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventFrame {
    pub seq: u64,
    /// Service clock ms.
    pub at_ms: u64,
    pub event: Event,
}

#[cfg(test)]
mod tests {
    use super::*;
    use seat_core::{Secret, SeatNumber};

    #[test]
    fn request_json_shape_is_stable() {
        let req = Request::new(7, RequestBody::StopSeat { seat: SeatNumber::new(2).unwrap() });
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(json, r#"{"version":3,"id":7,"body":{"type":"stop_seat","data":{"seat":2}}}"#);
        assert_eq!(serde_json::to_string(&RequestBody::GetStatus).unwrap(), r#"{"type":"get_status"}"#);
    }

    #[test]
    fn server_frame_shapes() {
        let r = ServerFrame::Response(Response::new(1, Ok(ResponseBody::Ack)));
        assert_eq!(serde_json::to_string(&r).unwrap(), r#"{"response":{"version":3,"id":1,"result":{"Ok":{"type":"ack"}}}}"#);
        let e = ServerFrame::Event(EventFrame { seq: 3, at_ms: 9, event: Event::ConfigChanged { revision: 4 } });
        let j = serde_json::to_string(&e).unwrap();
        assert!(j.starts_with(r#"{"event":{"seq":3"#), "{j}");
        let back: ServerFrame = serde_json::from_str(&j).unwrap();
        assert_eq!(back, e);
    }

    #[test]
    fn error_codes_match_serde() {
        for c in [ErrorCode::VersionMismatch, ErrorCode::AdminOnly, ErrorCode::WarningNotAcknowledged] {
            assert_eq!(serde_json::to_string(&c).unwrap(), format!("\"{}\"", c.as_str()));
        }
        let e: ProtoError = seat_core::backend::BackendError::new(seat_core::backend::ErrorKind::Timeout, "session_timeout", "x")
            .with_win32(1460)
            .into();
        assert_eq!(e.code, ErrorCode::Timeout);
        assert_eq!(e.reason_or_code(), "session_timeout");
        assert_eq!(e.win32, Some(1460));
    }

    #[test]
    fn secrets_not_debug_printed() {
        let b = RequestBody::SetSeatPassword { seat: SeatNumber::new(2).unwrap(), password: Some(Secret::new("hunter2")) };
        assert!(!format!("{b:?}").contains("hunter2"));
        assert!(serde_json::to_string(&b).unwrap().contains("hunter2"));
    }

    #[test]
    fn access_table() {
        let two = SeatNumber::new(2).unwrap();
        assert_eq!(RequestBody::GetStatus.access(), Access::Status);
        assert_eq!(RequestBody::StartSeats { seats: None }.access(), Access::Admin);
        assert_eq!(RequestBody::RelogonSeat { seat: two }.access(), Access::OwnSeat(two));
        assert_eq!(RequestBody::SwitchSeat { seat: two }.access(), Access::Admin);
        assert_eq!(RequestBody::SwitchSeat { seat: two }.min_protocol(), 3);
        assert_eq!(RequestBody::HostTakeCredentials { seat: two }.access(), Access::Host(two));
        assert_eq!(RequestBody::MySeat.access(), Access::Any);
        assert_eq!(RequestBody::ConsoleInputReport { events: vec![] }.access(), Access::ConsoleListener);
        assert_eq!(RequestBody::CredprovTakeCredentials.access(), Access::CredentialProvider);
        assert_eq!(RequestBody::CredprovSwitch { seat: two }.access(), Access::CredentialProvider);
        assert_eq!(RequestBody::AgentSwitch { seat: two }.access(), Access::Agent);
        assert_eq!(RequestBody::AgentSwitchTargets.access(), Access::Agent);
        let j = serde_json::to_string(&RequestBody::AgentSwitch { seat: two }).unwrap();
        assert_eq!(j, r#"{"type":"agent_switch","data":{"seat":2}}"#);
    }

    /// One of every protocol 3 request (Pulsar Split v2 requests on the Seat pipe).
    fn v3_requests() -> Vec<RequestBody> {
        let two = SeatNumber::new(2).unwrap();
        let pw = || Secret::new("pw");
        vec![
            RequestBody::TestSeatCredentials { seat: two },
            RequestBody::EnableUnlockAccount { seat: two, enable: true, unlock: true, confirm: true },
            RequestBody::GetRemoteRuntime { seat: two },
            RequestBody::SubmitPin { seat: two, pin: pw(), client_name: "tv".into(), request_id: None },
            RequestBody::GetPairState { seat: two },
            RequestBody::CancelPair { seat: two },
            RequestBody::ListPairedClients { seat: two },
            RequestBody::UnpairClient { seat: two, uuid: "u".into() },
            RequestBody::UnpairAll { seat: two },
            RequestBody::RegenerateSeatIdentity { seat: two, confirm: true },
            RequestBody::GetApps { seat: two },
            RequestBody::SetApps { seat: two, apps: vec![] },
            RequestBody::AnswerPeerRequest { seat: two, request_id: 1, allow: true, view_only: false },
            RequestBody::SetSeatConnectPassword { seat: two, password: pw() },
            RequestBody::GetSeatOtp { seat: two },
            RequestBody::RotateSeatOtp { seat: two },
            RequestBody::AnswerRelayAuth { seat: two, password: pw(), totp: None },
            RequestBody::GetHostInfo,
            RequestBody::SetMultiSession { enabled: true },
            RequestBody::GetTermwrapStatus,
            RequestBody::SetTermwrap { enabled: false, wrapper_path: None, ack_warning_version: 0 },
            RequestBody::ListRenderAdapters,
            RequestBody::GetPadBackends,
            RequestBody::GetShimStatus,
            RequestBody::SetShimTargets { targets: vec![] },
            RequestBody::SetAppMasking { exe_path: r"C:\g.exe".into(), enabled: true },
            RequestBody::SetDxvk { game_exe: r"C:\g.exe".into(), enabled: true, dlls: vec!["d3d9".into()] },
            RequestBody::GetDxvkStatus,
            RequestBody::GetLogTail { source: "service".into(), lines: 10 },
            RequestBody::GetStreamBackends,
            RequestBody::GetUserPicture { user: "ali".into() },
            RequestBody::CleanupAudioEndpoints { confirm: true },
            RequestBody::SetSeatDisplayMode { seat: two, width: 1920, height: 1080, hz: 60, hdr: false },
        ]
    }

    #[test]
    fn v3_requests_are_admin_and_need_protocol_3() {
        let all = v3_requests();
        assert_eq!(all.len(), 33);
        for r in &all {
            assert_eq!(r.access(), Access::Admin, "{r:?}");
            assert!(r.is_remote_kind_request(), "{r:?}");
            assert_eq!(r.min_protocol(), 3, "{r:?}");
            // Wire shape: snake_case type, round trip.
            let j = serde_json::to_value(r).unwrap();
            assert!(j["type"].as_str().unwrap().bytes().all(|b| b.is_ascii_lowercase() || b == b'_'), "{j}");
            assert_eq!(&serde_json::from_value::<RequestBody>(j).unwrap(), r);
        }
        // Old requests: unchanged minimum, except the config ones (schema 2 seat shape).
        assert_eq!(RequestBody::GetStatus.min_protocol(), 1);
        for r in [
            RequestBody::ResetGraphics { seat: SeatNumber::new(2).unwrap() },
            RequestBody::VmSeats { action: crate::hyperv::VmAction::Status },
            RequestBody::RestartTermService { confirm: true },
            RequestBody::RestoreUsbInput { confirm: true },
            RequestBody::HostDriveBroker { seat: SeatNumber::new(2).unwrap() },
        ] {
            assert_eq!(r.min_protocol(), 3, "parity-round request {r:?}");
        }
        assert_eq!(RequestBody::HostTakeCredentials { seat: SeatNumber::new(2).unwrap() }.min_protocol(), 1);
        assert_eq!(RequestBody::GetConfig.min_protocol(), 3);
        assert_eq!(RequestBody::ResetConfig { confirm: true }.min_protocol(), 3);
        assert!(!RequestBody::GetStatus.is_remote_kind_request());
    }

    #[test]
    fn seat_kind_of_requests() {
        use seat_core::config::SeatKind;
        let two = SeatNumber::new(2).unwrap();
        let remote: Vec<String> = v3_requests()
            .iter()
            .filter(|r| r.seat_kind() == Some((two, SeatKind::Remote)))
            .map(|r| serde_json::to_value(r).unwrap()["type"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(
            remote,
            [
                "get_remote_runtime", "submit_pin", "get_pair_state", "cancel_pair", "list_paired_clients",
                "unpair_client", "unpair_all", "regenerate_seat_identity", "get_apps", "set_apps",
                "answer_peer_request", "set_seat_connect_password", "get_seat_otp", "rotate_seat_otp",
                "answer_relay_auth", "set_seat_display_mode",
            ]
        );
        // Kind-agnostic seat requests and host-wide ones name no kind.
        assert_eq!(RequestBody::TestSeatCredentials { seat: two }.seat_kind(), None);
        assert_eq!(RequestBody::StopSeat { seat: two }.seat_kind(), None);
        assert_eq!(RequestBody::SetSeatPassword { seat: two, password: None }.seat_kind(), None);
        assert_eq!(RequestBody::GetHostInfo.seat_kind(), None);
        // Local-only requests.
        for r in [
            RequestBody::RelogonSeat { seat: two },
            RequestBody::SwitchSeat { seat: two },
            RequestBody::HostGetAssignment { seat: two },
            RequestBody::HostTakeCredentials { seat: two },
            RequestBody::CredprovSwitch { seat: two },
            RequestBody::AgentSwitch { seat: two },
        ] {
            assert_eq!(r.seat_kind(), Some((two, SeatKind::Local)), "{r:?}");
        }
    }

    #[test]
    fn v3_response_and_event_shapes() {
        let r = ResponseBody::Otp { otp: Secret::new("abc123") };
        assert!(!format!("{r:?}").contains("abc123"));
        let r = ResponseBody::ShimStatus(remote::IsolationStatus { supported: true, ..Default::default() });
        let j = serde_json::to_value(&r).unwrap();
        assert_eq!(j["type"], "shim_status");
        assert_eq!(serde_json::from_value::<ResponseBody>(j).unwrap(), r);
        let e = Event::Stream { seat: SeatNumber::new(3).unwrap(), event: StreamEvent::PairEnded(serde_json::json!({"result": "paired"})) };
        assert_eq!(e.topic(), Topic::Stream);
        assert_eq!(serde_json::to_string(&Topic::Stream).unwrap(), r#""stream""#);
        let back: Event = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert_eq!(back, e);
        assert_eq!(serde_json::to_string(&ErrorCode::WrongSeatKind).unwrap(), r#""wrong_seat_kind""#);
        let b = RequestBody::AnswerRelayAuth { seat: SeatNumber::new(2).unwrap(), password: Secret::new("pw9"), totp: Some(Secret::new("123456")) };
        let dbg = format!("{b:?}");
        assert!(!dbg.contains("pw9") && !dbg.contains("123456"), "{dbg}");
    }

    /// The C++ credential provider (`credprov/src/pipe.cpp`) writes and parses these shapes
    /// by hand; keep them stable.
    #[test]
    fn credprov_shapes() {
        let q = Request::new(2, RequestBody::CredprovQuery { scenario: CredprovScenario::Unlock });
        assert_eq!(
            serde_json::to_string(&q).unwrap(),
            r#"{"version":3,"id":2,"body":{"type":"credprov_query","data":{"scenario":"unlock"}}}"#
        );
        // credprov/src/pipe.cpp still stamps protocol 1; the frame parses and the request
        // is allowed for clients below 3.
        let old: Request = serde_json::from_str(r#"{"version":1,"id":2,"body":{"type":"credprov_query","data":{"scenario":"unlock"}}}"#).unwrap();
        assert_eq!(old.version, 1);
        assert_eq!(old.body.min_protocol(), MIN_PROTOCOL_VERSION);
        let min: RequestBody = serde_json::from_str(r#"{"type":"credprov_query","data":{}}"#).unwrap();
        assert_eq!(min, RequestBody::CredprovQuery { scenario: CredprovScenario::Logon });
        assert_eq!(serde_json::to_string(&RequestBody::CredprovTakeCredentials).unwrap(), r#"{"type":"credprov_take_credentials"}"#);
        let r: RequestBody = serde_json::from_str(r#"{"type":"credprov_report","data":{"status":-1073741715}}"#).unwrap();
        assert_eq!(r, RequestBody::CredprovReport { status: -1073741715, sub_status: 0 });
        let sw: RequestBody = serde_json::from_str(r#"{"type":"credprov_switch","data":{"seat":3}}"#).unwrap();
        assert_eq!(sw, RequestBody::CredprovSwitch { seat: SeatNumber::new(3).unwrap() });
        let s = ResponseBody::CredprovSession(CredprovSession {
            seat: SeatNumber::new(2),
            auto_submit: true,
            hide_others: true,
            account: Some(r"PC\ali".into()),
            user_sid: None,
            switch_targets: vec![SeatNumber::new(1).unwrap()],
            pending: false,
            preselect: false,
        });
        let j = serde_json::to_string(&Response::new(2, Ok(s))).unwrap();
        assert_eq!(
            j,
            r#"{"version":3,"id":2,"result":{"Ok":{"type":"credprov_session","data":{"seat":2,"auto_submit":true,"hide_others":true,"account":"PC\\ali","switch_targets":[1]}}}}"#
        );
        let pre = ResponseBody::CredprovSession(CredprovSession { preselect: true, ..Default::default() });
        assert!(serde_json::to_string(&pre).unwrap().contains(r#""preselect":true"#));
        let p = ResponseBody::CredprovSession(CredprovSession { pending: true, ..Default::default() });
        assert_eq!(
            serde_json::to_string(&p).unwrap(),
            r#"{"type":"credprov_session","data":{"seat":null,"auto_submit":false,"hide_others":false,"pending":true}}"#
        );
        let c = ResponseBody::Credentials(Credentials { domain: "PC".into(), user: "ali".into(), password: Some(Secret::new("pw")) });
        assert_eq!(serde_json::to_string(&c).unwrap(), r#"{"type":"credentials","data":{"domain":"PC","user":"ali","password":"pw"}}"#);
        let e = Response::new(3, Err(ProtoError::new(ErrorCode::AccessDenied, "x").with_reason("credentials_taken")));
        assert_eq!(
            serde_json::to_string(&e).unwrap(),
            r#"{"version":3,"id":3,"result":{"Err":{"code":"access_denied","message":"x","reason":"credentials_taken"}}}"#
        );
        assert_eq!(serde_json::to_string(&ClientKind::CredentialProvider).unwrap(), r#""credential_provider""#);
    }

    #[test]
    fn console_report_shapes() {
        let r = RequestBody::ConsoleInputReport {
            events: vec![
                ConsoleInputEvent::Activity { interface: r"\?\hid#a".into(), key_sc: None, click: true },
                ConsoleInputEvent::Hotkey { interface: "k".into() },
            ],
        };
        let j = serde_json::to_string(&r).unwrap();
        assert!(j.starts_with(r#"{"type":"console_input_report","data":{"events":[{"type":"activity""#), "{j}");
        assert!(!j.contains("key_sc"));
        assert_eq!(serde_json::from_str::<RequestBody>(&j).unwrap(), r);
        // Minimal senders: no events field, no click field.
        let min: RequestBody = serde_json::from_str(r#"{"type":"console_input_report","data":{}}"#).unwrap();
        assert_eq!(min, RequestBody::ConsoleInputReport { events: vec![] });
        let a: ConsoleInputEvent = serde_json::from_str(r#"{"type":"activity","interface":"x"}"#).unwrap();
        assert_eq!(a, ConsoleInputEvent::Activity { interface: "x".into(), key_sc: None, click: false });
        let st: ConsoleListenerState = serde_json::from_str("{}").unwrap();
        assert!(!st.stop && !st.wizard_running);
        assert_eq!(st.input.switch_hotkey_scan, Some(0x58));
        assert_eq!(serde_json::to_string(&ClientKind::ConsoleListener).unwrap(), r#""console_listener""#);
    }
}
