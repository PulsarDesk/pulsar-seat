// SPDX-License-Identifier: GPL-3.0-only
//! The one machine-readable error of the product: [`ProtoError`] with an [`ErrorCode`].
//! It travels over the control pipe (re-exported by `seat-proto`) and is also what the
//! remote-kind model validation ([`crate::remote::validate`]) and the remote packages return.
//!
//! The code list is the union of Pulsar Seat's protocol 1 codes and Pulsar Split's
//! protocol 2 codes, plus `wrong_seat_kind` and `session_unavailable` (protocol 3,
//! architecture.md §10.2).

use serde::{Deserialize, Serialize};

/// Machine-readable error. The manager maps `reason` (if set) or `code` to localized copy;
/// `message` is an English diagnostic for logs only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{code:?}: {message}")]
pub struct ProtoError {
    pub code: ErrorCode,
    pub message: String,
    /// JSON path / name of the offending input field, when the error is about one field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// Finer snake_case reason (`BackendError::reason`, `FailureReason`, a package reason
    /// code such as `client_auth`).
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
    pub fn with_win32(mut self, code: u32) -> Self {
        self.win32 = Some(code);
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

    /// `reason` if set, else the snake_case `code`.
    pub fn reason_or_code(&self) -> String {
        self.reason.clone().unwrap_or_else(|| self.code.as_str().to_string())
    }
}

impl From<crate::backend::BackendError> for ProtoError {
    fn from(e: crate::backend::BackendError) -> Self {
        use crate::backend::ErrorKind as K;
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
            /// Every code, in declaration order.
            pub const ALL: &'static [ErrorCode] = &[$(Self::$v,)*];
            pub fn as_str(self) -> &'static str { match self { $(Self::$v => $s,)* } }
        }
    };
}

error_codes! {
    VersionMismatch => "version_mismatch",
    /// First request was not `hello`, or a malformed frame / request.
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
    // ---- from Pulsar Split protocol 2 (remote kind) ----
    InvalidName => "invalid_name",
    DuplicateName => "duplicate_name",
    InvalidDisplayName => "invalid_display_name",
    InvalidPort => "invalid_port",
    PortOverlap => "port_overlap",
    NoFreePort => "no_free_port",
    InvalidUser => "invalid_user",
    UserInUse => "user_in_use",
    MissingCredentials => "missing_credentials",
    EditionUnsupported => "edition_unsupported",
    SeatFailed => "seat_failed",
    DriverMissing => "driver_missing",
    NoPendingRequest => "no_pending_request",
    WrongPin => "wrong_pin",
    HashMismatch => "hash_mismatch",
    Firewall => "firewall",
    // ---- protocol 3 (seat kinds) ----
    /// A seat-scoped request was sent for a seat of the other kind (local / remote).
    WrongSeatKind => "wrong_seat_kind",
    /// A remote seat needs concurrent sessions (Server + RDSH or the wrapper kind) and the
    /// selected session backend has none (`fast_user_switch`, `desktops`).
    SessionUnavailable => "session_unavailable",
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_match_serde_and_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for c in ErrorCode::ALL {
            assert_eq!(serde_json::to_string(c).unwrap(), format!("\"{}\"", c.as_str()));
            assert!(seen.insert(c.as_str()), "duplicate {}", c.as_str());
        }
        assert!(ErrorCode::ALL.contains(&ErrorCode::WrongSeatKind));
        assert_eq!(ErrorCode::SessionUnavailable.as_str(), "session_unavailable");
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
        assert!(!json.contains("win32"), "{json}");
    }

    #[test]
    fn split_v2_error_json_still_parses() {
        let e: ProtoError = serde_json::from_str(r#"{"code":"port_overlap","message":"m","field":"port"}"#).unwrap();
        assert_eq!(e.code, ErrorCode::PortOverlap);
        assert_eq!(e.field.as_deref(), Some("port"));
    }
}
