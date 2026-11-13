// SPDX-License-Identifier: GPL-3.0-only
//! Crate error type with stable snake_case reason codes (contracts §0.3, §7.2).

use split_proto::{ErrorCode, ProtoError, StreamBackendKind};
use std::path::PathBuf;

/// Errors never carry secrets (PINs, passwords, request bodies).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StreamError {
    /// The watchdog gave up (spec 80 §10).
    #[error("stream server keeps failing: {0}")]
    StreamServer(String),
    #[error("stream server binary missing: {}", .0.display())]
    ServerBinaryMissing(PathBuf),
    #[error("stream server hash mismatch (expected {expected}, got {actual})")]
    ServerHashMismatch { expected: String, actual: String },
    #[error("bad stream server manifest: {0}")]
    BadManifest(String),
    /// Neither the fork's seat pipe nor the upstream credential step is available.
    #[error("stream server has no control channel")]
    ServerWithoutControlPipe,
    #[error("no pairing request is pending")]
    NoPendingRequest,
    #[error("wrong PIN")]
    WrongPin,
    #[error("pairing request timed out")]
    PairTimeout,
    #[error("ports in use: {0:?}")]
    PortInUse(Vec<u16>),
    #[error("firewall: {0}")]
    Firewall(String),
    #[error("bad app list: {0}")]
    BadAppList(String),
    #[error("seat control pipe: {0}")]
    ControlPipe(String),
    /// The server answered a control request with an error we do not map more precisely.
    #[error("stream server rejected request: {code}: {detail}")]
    Remote { code: String, detail: String },
    /// Loopback web-UI transport failure (upstream path only).
    #[error("stream server web UI: {0}")]
    WebUi(String),
    /// The current server flavour has no equivalent of this operation.
    #[error("not supported by this stream server: {0}")]
    Unsupported(String),
    #[error("invalid {field}: {detail}")]
    InvalidValue { field: String, detail: String },
    #[error("i/o: {0}")]
    Io(String),
    /// The seat's stream backend has no such operation (a PIN for a Pulsar seat, an
    /// approval for a Sunshine seat). Code `backend_<kind>`.
    #[error("not available for the {} stream server", .0.as_str())]
    WrongBackend(StreamBackendKind),
    /// The backend's binary or manifest is not installed (never falls back to another
    /// backend).
    #[error("stream backend not installed: {0}")]
    BackendMissing(String),
}

impl StreamError {
    /// Stable reason code (`ProtoError.message` prefix, later `ProtoError.reason`).
    pub fn code(&self) -> &'static str {
        match self {
            Self::StreamServer(_) => "stream_server",
            Self::ServerBinaryMissing(_) => "server_binary_missing",
            Self::ServerHashMismatch { .. } => "server_hash_mismatch",
            Self::BadManifest(_) => "bad_manifest",
            Self::ServerWithoutControlPipe => "server_without_control_pipe",
            Self::NoPendingRequest => "no_pending_request",
            Self::WrongPin => "wrong_pin",
            Self::PairTimeout => "pair_timeout",
            Self::PortInUse(_) => "port_in_use",
            Self::Firewall(_) => "firewall",
            Self::BadAppList(_) => "bad_app_list",
            Self::ControlPipe(_) => "control_pipe",
            Self::Remote { .. } => "server_rejected",
            Self::WebUi(_) => "web_ui",
            Self::Unsupported(_) => "unsupported",
            Self::InvalidValue { .. } => "invalid_value",
            Self::Io(_) => "io",
            Self::WrongBackend(StreamBackendKind::Sunshine) => "backend_sunshine",
            Self::WrongBackend(StreamBackendKind::Pulsar) => "backend_pulsar",
            Self::BackendMissing(_) => "backend_missing",
        }
    }

    pub(crate) fn io(context: &str, e: impl std::fmt::Display) -> Self {
        Self::Io(format!("{context}: {e}"))
    }

    pub(crate) fn invalid(field: &str, detail: impl Into<String>) -> Self {
        Self::InvalidValue { field: field.into(), detail: detail.into() }
    }
}

impl From<StreamError> for ProtoError {
    fn from(e: StreamError) -> Self {
        // Closest existing code until the integrator adds the v2 codes (contracts §9.3).
        let code = match &e {
            StreamError::InvalidValue { .. }
            | StreamError::BadAppList(_)
            | StreamError::WrongPin
            | StreamError::NoPendingRequest
            | StreamError::PairTimeout => ErrorCode::InvalidValue,
            StreamError::PortInUse(_) => ErrorCode::PortOverlap,
            StreamError::Unsupported(_) | StreamError::ServerWithoutControlPipe => ErrorCode::NotImplemented,
            StreamError::WrongBackend(_) => ErrorCode::Unsupported,
            StreamError::BackendMissing(_) => ErrorCode::ServiceUnavailable,
            StreamError::StreamServer(_)
            | StreamError::ServerBinaryMissing(_)
            | StreamError::ServerHashMismatch { .. }
            | StreamError::BadManifest(_)
            | StreamError::ControlPipe(_)
            | StreamError::WebUi(_) => ErrorCode::ServiceUnavailable,
            StreamError::Firewall(_) | StreamError::Remote { .. } | StreamError::Io(_) => ErrorCode::Internal,
        };
        let field = match &e {
            StreamError::InvalidValue { field, .. } => Some(field.clone()),
            _ => None,
        };
        let mut p = ProtoError::new(code, format!("{}: {e}", e.code()));
        p.field = field;
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proto_message_starts_with_reason_code() {
        let p: ProtoError = StreamError::WrongPin.into();
        assert_eq!(p.code, ErrorCode::InvalidValue);
        assert!(p.message.starts_with("wrong_pin: "));
        let p: ProtoError = StreamError::ServerWithoutControlPipe.into();
        assert_eq!(p.code, ErrorCode::NotImplemented);
        assert!(p.message.starts_with("server_without_control_pipe"));
        let p: ProtoError = StreamError::invalid("pin", "x").into();
        assert_eq!(p.field.as_deref(), Some("pin"));
        let p = ProtoError::from(StreamError::WrongBackend(StreamBackendKind::Pulsar)).with_reason_from_message();
        assert_eq!((p.code, p.reason.as_deref()), (ErrorCode::Unsupported, Some("backend_pulsar")));
        let p = ProtoError::from(StreamError::BackendMissing("pulsar-host.json".into())).with_reason_from_message();
        assert_eq!(p.reason.as_deref(), Some("backend_missing"));
    }

    #[test]
    fn contract_codes_exist() {
        let codes = [
            StreamError::StreamServer(String::new()).code(),
            StreamError::ServerBinaryMissing(PathBuf::new()).code(),
            StreamError::ServerHashMismatch { expected: String::new(), actual: String::new() }.code(),
            StreamError::ServerWithoutControlPipe.code(),
            StreamError::NoPendingRequest.code(),
            StreamError::WrongPin.code(),
            StreamError::PairTimeout.code(),
            StreamError::PortInUse(vec![]).code(),
            StreamError::Firewall(String::new()).code(),
            StreamError::BadAppList(String::new()).code(),
            StreamError::ControlPipe(String::new()).code(),
        ];
        assert_eq!(
            codes,
            [
                "stream_server",
                "server_binary_missing",
                "server_hash_mismatch",
                "server_without_control_pipe",
                "no_pending_request",
                "wrong_pin",
                "pair_timeout",
                "port_in_use",
                "firewall",
                "bad_app_list",
                "control_pipe"
            ]
        );
    }
}
