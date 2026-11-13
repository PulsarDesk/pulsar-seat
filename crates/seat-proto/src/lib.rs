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
//! Version history: 1 = first contract (2026-11-12).

pub mod framing;
pub mod messages;

use serde::{Deserialize, Serialize};

pub use framing::{FrameError, MAX_FRAME_LEN, read_frame, read_message, write_frame, write_message};
pub use messages::*;

/// Current protocol version. Additive changes (new optional fields, new requests) keep it;
/// incompatible changes bump it and update `MIN_PROTOCOL_VERSION`.
pub const PROTOCOL_VERSION: u32 = 1;
/// Oldest client protocol the service still accepts.
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

/// Machine-readable error. The manager maps `reason` (if set) or `code` to Turkish copy;
/// `message` is an English diagnostic for logs only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{code:?}: {message}")]
pub struct ProtoError {
    pub code: ErrorCode,
    pub message: String,
    /// JSON path of the offending input field, when the error is about one field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// Finer snake_case reason (`BackendError::reason`, `FailureReason`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Windows error code behind the failure, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub win32: Option<u32>,
}

impl ProtoError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self { code, message: message.into(), field: None, reason: None, win32: None }
    }
    pub fn with_field(mut self, f: impl Into<String>) -> Self {
        self.field = Some(f.into());
        self
    }
    pub fn with_reason(mut self, r: impl Into<String>) -> Self {
        self.reason = Some(r.into());
        self
    }
    /// `reason` if set, else the snake_case `code`.
    pub fn reason_or_code(&self) -> String {
        self.reason.clone().unwrap_or_else(|| self.code.as_str().to_string())
    }
}

impl From<seat_core::backend::BackendError> for ProtoError {
    fn from(e: seat_core::backend::BackendError) -> Self {
        use seat_core::backend::ErrorKind as K;
        let code = match e.kind {
            K::Unsupported => ErrorCode::Unsupported,
            K::NotReady => ErrorCode::NotReady,
            K::Denied => ErrorCode::AccessDenied,
            K::NotFound => ErrorCode::NotFound,
            K::Timeout => ErrorCode::Timeout,
            K::Driver => ErrorCode::DriverUnavailable,
            K::Coexistence => ErrorCode::Coexistence,
            K::Policy => ErrorCode::PolicyBlocked,
            K::Logon => ErrorCode::LogonFailed,
            K::Os => ErrorCode::Os,
            K::Internal => ErrorCode::Internal,
        };
        Self { code, message: e.message, field: None, reason: Some(e.reason), win32: e.win32 }
    }
}

macro_rules! error_codes {
    ($($(#[$m:meta])* $v:ident => $s:literal,)*) => {
        /// Error codes (snake_case on the wire).
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum ErrorCode { $($(#[$m])* $v,)* }
        impl ErrorCode {
            pub fn as_str(self) -> &'static str { match self { $(Self::$v => $s,)* } }
        }
    };
}

error_codes! {
    VersionMismatch => "version_mismatch",
    /// First request was not `hello`, or a malformed frame.
    BadRequest => "bad_request",
    /// Caller's token / client kind does not allow this request.
    AccessDenied => "access_denied",
    /// The admin-only policy is on and the caller is not an elevated administrator.
    AdminOnly => "admin_only",
    NotFound => "not_found",
    /// Config failed validation; details in `ResponseBody::Issues` of `validate_config`.
    InvalidConfig => "invalid_config",
    /// `apply_config` was based on an older revision.
    RevisionConflict => "revision_conflict",
    /// Save-time conflicts exist and `force` was false.
    ConflictsNotConfirmed => "conflicts_not_confirmed",
    InvalidValue => "invalid_value",
    /// Product disabled (start refused).
    Disabled => "disabled",
    /// Service is in safe mode (Windows Safe Mode, unclean shutdown, crash loop).
    SafeMode => "safe_mode",
    SeatRunning => "seat_running",
    SeatNotRunning => "seat_not_running",
    Unsupported => "unsupported",
    NotReady => "not_ready",
    Timeout => "timeout",
    DriverUnavailable => "driver_unavailable",
    Coexistence => "coexistence",
    PolicyBlocked => "policy_blocked",
    LogonFailed => "logon_failed",
    /// Experimental backend without the current warning acknowledged.
    WarningNotAcknowledged => "warning_not_acknowledged",
    /// Wizard not running / already running.
    WizardState => "wizard_state",
    Os => "os",
    Store => "store",
    ServiceUnavailable => "service_unavailable",
    NotImplemented => "not_implemented",
    Internal => "internal",
}

#[cfg(test)]
mod tests {
    use super::*;
    use seat_core::{Secret, SeatNumber};

    #[test]
    fn request_json_shape_is_stable() {
        let req = Request::new(7, RequestBody::StopSeat { seat: SeatNumber::new(2).unwrap() });
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(json, r#"{"version":1,"id":7,"body":{"type":"stop_seat","data":{"seat":2}}}"#);
        assert_eq!(serde_json::to_string(&RequestBody::GetStatus).unwrap(), r#"{"type":"get_status"}"#);
    }

    #[test]
    fn server_frame_shapes() {
        let r = ServerFrame::Response(Response::new(1, Ok(ResponseBody::Ack)));
        assert_eq!(serde_json::to_string(&r).unwrap(), r#"{"response":{"version":1,"id":1,"result":{"Ok":{"type":"ack"}}}}"#);
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

    /// The C++ credential provider (`credprov/src/pipe.cpp`) writes and parses these shapes
    /// by hand; keep them stable.
    #[test]
    fn credprov_shapes() {
        let q = Request::new(2, RequestBody::CredprovQuery { scenario: CredprovScenario::Unlock });
        assert_eq!(
            serde_json::to_string(&q).unwrap(),
            r#"{"version":1,"id":2,"body":{"type":"credprov_query","data":{"scenario":"unlock"}}}"#
        );
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
        });
        let j = serde_json::to_string(&Response::new(2, Ok(s))).unwrap();
        assert_eq!(
            j,
            r#"{"version":1,"id":2,"result":{"Ok":{"type":"credprov_session","data":{"seat":2,"auto_submit":true,"hide_others":true,"account":"PC\\ali","switch_targets":[1]}}}}"#
        );
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
            r#"{"version":1,"id":3,"result":{"Err":{"code":"access_denied","message":"x","reason":"credentials_taken"}}}"#
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
