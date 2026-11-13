// SPDX-License-Identifier: GPL-3.0-only
//! Crate error type with stable snake_case reason codes (contract §3.1).

use split_proto::{ErrorCode, ProtoError};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DisplayError {
    /// No ready signal / driver confirmation in time.
    #[error("display timeout: {0}")]
    Timeout(String),
    /// Seat display not present (device gone, session without display, seat not attached).
    #[error("no display: {0}")]
    NoDisplay(String),
    /// The backend cannot put a monitor into this (remote) session (spec 40 §2.3, OQ 20).
    #[error("session attach unsupported: {0}")]
    SessionAttachUnsupported(String),
    #[error("driver not installed: {0}")]
    DriverNotInstalled(String),
    #[error("device not ready: {0}")]
    DeviceNotReady(String),
    #[error("busy: {0}")]
    Busy(String),
    /// The OS or driver refused the configuration.
    #[error("rejected: {0}")]
    Rejected(String),
    #[error("adapter not found: {0}")]
    AdapterNotFound(String),
    #[error("invalid value: {0}")]
    InvalidValue(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
    /// Malformed buffer or reply from a driver / pipe peer.
    #[error("protocol: {0}")]
    Protocol(String),
    /// OS call failed (Win32 error code in the text).
    #[error("io: {0}")]
    Io(String),
}

impl DisplayError {
    pub fn code(&self) -> &'static str {
        match self {
            DisplayError::Timeout(_) => "display_timeout",
            DisplayError::NoDisplay(_) => "no_display",
            DisplayError::SessionAttachUnsupported(_) => "session_attach_unsupported",
            DisplayError::DriverNotInstalled(_) => "driver_not_installed",
            DisplayError::DeviceNotReady(_) => "device_not_ready",
            DisplayError::Busy(_) => "busy",
            DisplayError::Rejected(_) => "rejected",
            DisplayError::AdapterNotFound(_) => "adapter_not_found",
            DisplayError::InvalidValue(_) => "invalid_value",
            DisplayError::Unsupported(_) => "unsupported",
            DisplayError::Protocol(_) => "protocol",
            DisplayError::Io(_) => "io",
        }
    }

    fn detail(&self) -> &str {
        match self {
            DisplayError::Timeout(s)
            | DisplayError::NoDisplay(s)
            | DisplayError::SessionAttachUnsupported(s)
            | DisplayError::DriverNotInstalled(s)
            | DisplayError::DeviceNotReady(s)
            | DisplayError::Busy(s)
            | DisplayError::Rejected(s)
            | DisplayError::AdapterNotFound(s)
            | DisplayError::InvalidValue(s)
            | DisplayError::Unsupported(s)
            | DisplayError::Protocol(s)
            | DisplayError::Io(s) => s,
        }
    }
}

impl From<DisplayError> for ProtoError {
    fn from(e: DisplayError) -> Self {
        // Closest existing code until the integrator adds the §9.3 variants; the reason
        // code always leads the message.
        let code = match &e {
            DisplayError::Timeout(_)
            | DisplayError::NoDisplay(_)
            | DisplayError::DriverNotInstalled(_)
            | DisplayError::DeviceNotReady(_)
            | DisplayError::Busy(_) => ErrorCode::ServiceUnavailable,
            DisplayError::SessionAttachUnsupported(_) | DisplayError::Unsupported(_) => {
                ErrorCode::NotImplemented
            }
            DisplayError::Rejected(_)
            | DisplayError::AdapterNotFound(_)
            | DisplayError::InvalidValue(_) => ErrorCode::InvalidValue,
            DisplayError::Protocol(_) | DisplayError::Io(_) => ErrorCode::Internal,
        };
        ProtoError::new(code, format!("{}: {}", e.code(), e.detail()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_stable_and_lead_the_message() {
        let e = DisplayError::SessionAttachUnsupported("session 3 is remote".into());
        assert_eq!(e.code(), "session_attach_unsupported");
        let p: ProtoError = e.into();
        assert_eq!(p.code, ErrorCode::NotImplemented);
        assert_eq!(p.message, "session_attach_unsupported: session 3 is remote");
        assert_eq!(
            DisplayError::Timeout(String::new()).code(),
            "display_timeout"
        );
        assert_eq!(
            ProtoError::from(DisplayError::Busy("x".into())).code,
            ErrorCode::ServiceUnavailable
        );
    }
}
