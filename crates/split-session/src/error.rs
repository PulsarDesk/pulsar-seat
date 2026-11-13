// SPDX-License-Identifier: GPL-3.0-only
//! Error type and seat failure reason codes (spec 30 §7, §14; contracts §1.1).

use serde::{Deserialize, Serialize};
use seat_proto::remote::{ErrorCode, ProtoError};

/// Why a seat session failed. Snake_case on the wire (spec codes with `-` → `_`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureReason {
    EditionUnsupported,
    Coexistence,
    RdsNotReady,
    SeatLimit,
    Account,
    PasswordExpired,
    RduMembership,
    Profile,
    ClientAuth,
    CertMismatch,
    SessionTimeout,
    ClientExited,
    Token,
}

impl FailureReason {
    pub const ALL: [FailureReason; 13] = [
        Self::EditionUnsupported,
        Self::Coexistence,
        Self::RdsNotReady,
        Self::SeatLimit,
        Self::Account,
        Self::PasswordExpired,
        Self::RduMembership,
        Self::Profile,
        Self::ClientAuth,
        Self::CertMismatch,
        Self::SessionTimeout,
        Self::ClientExited,
        Self::Token,
    ];

    pub fn code(self) -> &'static str {
        match self {
            Self::EditionUnsupported => "edition_unsupported",
            Self::Coexistence => "coexistence",
            Self::RdsNotReady => "rds_not_ready",
            Self::SeatLimit => "seat_limit",
            Self::Account => "account",
            Self::PasswordExpired => "password_expired",
            Self::RduMembership => "rdu_membership",
            Self::Profile => "profile",
            Self::ClientAuth => "client_auth",
            Self::CertMismatch => "cert_mismatch",
            Self::SessionTimeout => "session_timeout",
            Self::ClientExited => "client_exited",
            Self::Token => "token",
        }
    }
}

/// Every error of this crate. Messages never contain passwords; user names and SIDs only
/// where the operator needs them (they are not logged above `debug`).
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("edition_unsupported: {0}")]
    EditionUnsupported(String),
    #[error("coexistence: TermService ServiceDll is {service_dll}")]
    Coexistence { service_dll: String },
    #[error("rds_not_ready: {0}")]
    RdsNotReady(String),
    #[error("seat_limit: at most {max} seat sessions allowed")]
    SeatLimit { max: u32 },
    #[error("account: {0}")]
    Account(String),
    #[error("password_expired: the seat user's password has expired")]
    PasswordExpired,
    #[error("rdu_membership: {0}")]
    RduMembership(String),
    #[error("profile: {0}")]
    Profile(String),
    #[error("client_auth: {0}")]
    ClientAuth(String),
    #[error("cert_mismatch: {0}")]
    CertMismatch(String),
    #[error("session_timeout: {0}")]
    SessionTimeout(String),
    #[error("client_exited: {0}")]
    ClientExited(String),
    #[error("token: {0}")]
    Token(String),
    #[error("cancelled: seat start cancelled")]
    Cancelled,
    #[error("not_found: {0}")]
    NotFound(String),
    #[error("invalid_argument: {0}")]
    InvalidArgument(String),
    #[error("helper_protocol: {0}")]
    HelperProtocol(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("win32: {api} failed with {code}")]
    Win32 { api: &'static str, code: u32 },
}

impl SessionError {
    /// Stable snake_case reason code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Cancelled => "cancelled",
            Self::NotFound(_) => "not_found",
            Self::InvalidArgument(_) => "invalid_argument",
            Self::HelperProtocol(_) => "helper_protocol",
            Self::Io(_) => "io",
            Self::Win32 { .. } => "win32",
            other => other.failure_reason().map(FailureReason::code).unwrap_or("internal"),
        }
    }

    /// The seat failure reason this error stands for, if it is one.
    pub fn failure_reason(&self) -> Option<FailureReason> {
        Some(match self {
            Self::EditionUnsupported(_) => FailureReason::EditionUnsupported,
            Self::Coexistence { .. } => FailureReason::Coexistence,
            Self::RdsNotReady(_) => FailureReason::RdsNotReady,
            Self::SeatLimit { .. } => FailureReason::SeatLimit,
            Self::Account(_) => FailureReason::Account,
            Self::PasswordExpired => FailureReason::PasswordExpired,
            Self::RduMembership(_) => FailureReason::RduMembership,
            Self::Profile(_) => FailureReason::Profile,
            Self::ClientAuth(_) => FailureReason::ClientAuth,
            Self::CertMismatch(_) => FailureReason::CertMismatch,
            Self::SessionTimeout(_) => FailureReason::SessionTimeout,
            Self::ClientExited(_) => FailureReason::ClientExited,
            Self::Token(_) => FailureReason::Token,
            _ => return None,
        })
    }

    /// Builds a [`SessionError::Win32`] from the calling thread's last error.
    #[cfg(windows)]
    pub fn last_win32(api: &'static str) -> Self {
        // SAFETY: GetLastError has no preconditions.
        let code = unsafe { windows_sys::Win32::Foundation::GetLastError() };
        Self::Win32 { api, code }
    }
}

impl From<SessionError> for ProtoError {
    fn from(e: SessionError) -> Self {
        // Closest existing code until the integrator adds the new ones (contracts §9.3).
        let code = match &e {
            SessionError::Account(_) | SessionError::PasswordExpired | SessionError::RduMembership(_) => {
                ErrorCode::InvalidUser
            }
            SessionError::InvalidArgument(_) => ErrorCode::InvalidValue,
            SessionError::NotFound(_) => ErrorCode::NotFound,
            SessionError::EditionUnsupported(_) => ErrorCode::NotImplemented,
            SessionError::Io(_) | SessionError::Win32 { .. } | SessionError::HelperProtocol(_) => ErrorCode::Internal,
            _ => ErrorCode::ServiceUnavailable,
        };
        // Display already starts with "<code>: ".
        ProtoError::new(code, e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reason_codes_round_trip_through_serde() {
        for r in FailureReason::ALL {
            let json = serde_json::to_string(&r).unwrap();
            assert_eq!(json, format!("\"{}\"", r.code()));
            let back: FailureReason = serde_json::from_str(&json).unwrap();
            assert_eq!(back, r);
        }
    }

    #[test]
    fn proto_error_message_starts_with_code() {
        let e = SessionError::ClientAuth("NLA failed".into());
        assert_eq!(e.code(), "client_auth");
        let p: ProtoError = e.into();
        assert_eq!(p.code, ErrorCode::ServiceUnavailable);
        assert_eq!(p.message, "client_auth: NLA failed");
        let p: ProtoError = SessionError::PasswordExpired.into();
        assert_eq!(p.code, ErrorCode::InvalidUser);
        assert!(p.message.starts_with("password_expired: "));
        assert_eq!(SessionError::Cancelled.code(), "cancelled");
        assert_eq!(SessionError::Win32 { api: "X", code: 5 }.code(), "win32");
    }
}
