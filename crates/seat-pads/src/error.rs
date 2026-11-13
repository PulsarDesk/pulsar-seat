// SPDX-License-Identifier: GPL-3.0-only
//! The one error type of `seat-pads`.

use split_proto::{ErrorCode, ProtoError};

#[derive(Debug, thiserror::Error)]
pub enum PadError {
    /// No backend can create this pad kind right now (driver missing, devnode failed).
    #[error("pad_unavailable: {0}")]
    Unavailable(String),
    /// Slot index beyond the seat's limit.
    #[error("limit: slot {slot} >= limit {limit}")]
    Limit { slot: u8, limit: u8 },
    /// Pads are switched off for the seat.
    #[error("disabled: pads are off for this seat")]
    Disabled,
    /// The backend's driver is not installed or not running.
    #[error("driver_missing: {0}")]
    DriverMissing(String),
    /// `usbip.exe attach` / `detach` or the USB/IP server failed.
    #[error("usbip_failed: {0}")]
    Usbip(String),
    /// The device node or the backend connection went away.
    #[error("device_lost: {0}")]
    DeviceLost(String),
    /// A marker device belongs to someone else or two pads claim one marker.
    #[error("marker_conflict: {0}")]
    MarkerConflict(String),
    /// Bad input (seat id, SID, report length, shared-memory header…).
    #[error("invalid: {0}")]
    Invalid(String),
    /// Operation not supported by this backend or pad kind.
    #[error("unsupported: {0}")]
    Unsupported(String),
    /// Windows API failure (function name + Win32/CONFIGRET/HRESULT code).
    #[error("os: {func} failed with {code:#x}")]
    Os { func: &'static str, code: u32 },
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

impl PadError {
    /// Stable snake_case reason code (contracts.md §5.1).
    pub fn code(&self) -> &'static str {
        match self {
            PadError::Unavailable(_) => "pad_unavailable",
            PadError::Limit { .. } => "limit",
            PadError::Disabled => "disabled",
            PadError::DriverMissing(_) => "driver_missing",
            PadError::Usbip(_) => "usbip_failed",
            PadError::DeviceLost(_) => "device_lost",
            PadError::MarkerConflict(_) => "marker_conflict",
            PadError::Invalid(_) => "invalid_value",
            PadError::Unsupported(_) => "unsupported",
            PadError::Os { .. } | PadError::Io(_) | PadError::Json(_) => "internal",
        }
    }

    pub(crate) fn os(func: &'static str, code: u32) -> Self {
        PadError::Os { func, code }
    }
}

impl From<PadError> for ProtoError {
    fn from(e: PadError) -> Self {
        let code = match &e {
            PadError::Invalid(_) | PadError::Limit { .. } => ErrorCode::InvalidValue,
            PadError::Unavailable(_)
            | PadError::Disabled
            | PadError::DriverMissing(_)
            | PadError::Usbip(_)
            | PadError::DeviceLost(_) => ErrorCode::ServiceUnavailable,
            PadError::Unsupported(_) => ErrorCode::NotImplemented,
            PadError::MarkerConflict(_)
            | PadError::Os { .. }
            | PadError::Io(_)
            | PadError::Json(_) => ErrorCode::Internal,
        };
        // Display already starts with the reason code ("driver_missing: …").
        let msg = e.to_string();
        let msg = if msg.starts_with(e.code()) { msg } else { format!("{}: {msg}", e.code()) };
        ProtoError::new(code, msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_and_proto_mapping() {
        let e = PadError::DriverMissing("ViGEmBus".into());
        assert_eq!(e.code(), "driver_missing");
        let p: ProtoError = e.into();
        assert_eq!(p.code, ErrorCode::ServiceUnavailable);
        assert!(p.message.starts_with("driver_missing: "));

        let p: ProtoError = PadError::os("CM_Locate_DevNodeW", 0xD).into();
        assert!(p.message.starts_with("internal: "), "{}", p.message);
        let p: ProtoError = PadError::Limit { slot: 4, limit: 4 }.into();
        assert_eq!(p.code, ErrorCode::InvalidValue);
        assert!(p.message.starts_with("limit: "));
    }
}
