// SPDX-License-Identifier: GPL-3.0-only
//! The crate's one error type with stable snake_case reason codes (contracts.md §0.3, §4.2).

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ShimError {
    /// WDAC / Smart App Control or an AV refused to load the module.
    #[error("shim module blocked: {0}")]
    Blocked(String),
    /// The reference product's shim database (or layer) is installed (spec 90 §8.1, 6–10).
    #[error("another isolation shim is installed: {0}")]
    Foreign(String),
    #[error("shim kill switch is set")]
    KillSwitch,
    #[error("target disabled after repeated faults: {0}")]
    TargetDisabled(String),
    #[error("target not allowed: {0}")]
    InvalidTarget(String),
    #[error("invalid value: {0}")]
    InvalidValue(String),
    #[error("cannot decode: {0}")]
    Decode(String),
    #[error("dxvk file hash mismatch: {0}")]
    DxvkHashMismatch(String),
    #[error("dxvk release not downloaded: {0}")]
    DxvkNotDownloaded(String),
    #[error("download failed: {0}")]
    Download(String),
    #[error("registry: {0}")]
    Registry(String),
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    #[error("system call failed: {0}")]
    System(String),
    #[error("not supported here: {0}")]
    Unsupported(String),
}

impl ShimError {
    /// Stable reason code (travels as `"<code>: …"` in `ProtoError::message`).
    pub fn code(&self) -> &'static str {
        match self {
            Self::Blocked(_) => "shim_blocked",
            Self::Foreign(_) => "shim_foreign",
            Self::KillSwitch => "shim_kill_switch",
            Self::TargetDisabled(_) => "shim_target_disabled",
            Self::InvalidTarget(_) => "shim_invalid_target",
            Self::InvalidValue(_) => "shim_invalid_value",
            Self::Decode(_) => "shim_decode",
            Self::DxvkHashMismatch(_) => "dxvk_hash_mismatch",
            Self::DxvkNotDownloaded(_) => "dxvk_not_downloaded",
            Self::Download(_) => "dxvk_download_failed",
            Self::Registry(_) => "shim_registry",
            Self::Io(_) => "shim_io",
            Self::System(_) => "shim_system",
            Self::Unsupported(_) => "shim_unsupported",
        }
    }
}

#[cfg(feature = "proto")]
impl From<ShimError> for split_proto::ProtoError {
    fn from(e: ShimError) -> Self {
        use split_proto::ErrorCode;
        // Closest existing codes until the integrator adds new ones (contracts.md §9.3).
        let code = match &e {
            ShimError::InvalidTarget(_) | ShimError::InvalidValue(_) | ShimError::Decode(_) => {
                ErrorCode::InvalidValue
            }
            ShimError::Unsupported(_) => ErrorCode::NotImplemented,
            ShimError::Blocked(_)
            | ShimError::Foreign(_)
            | ShimError::KillSwitch
            | ShimError::TargetDisabled(_)
            | ShimError::DxvkNotDownloaded(_)
            | ShimError::Download(_) => ErrorCode::ServiceUnavailable,
            _ => ErrorCode::Internal,
        };
        split_proto::ProtoError::new(code, format!("{}: {e}", e.code()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_stable() {
        assert_eq!(ShimError::Foreign("x".into()).code(), "shim_foreign");
        assert_eq!(ShimError::KillSwitch.code(), "shim_kill_switch");
        assert_eq!(ShimError::DxvkHashMismatch("d3d9.dll".into()).code(), "dxvk_hash_mismatch");
        assert_eq!(ShimError::DxvkNotDownloaded("2.7".into()).code(), "dxvk_not_downloaded");
        assert_eq!(ShimError::Blocked("wdac".into()).code(), "shim_blocked");
        assert_eq!(ShimError::TargetDisabled("a.exe".into()).code(), "shim_target_disabled");
    }

    #[cfg(feature = "proto")]
    #[test]
    fn proto_message_starts_with_code() {
        let p: split_proto::ProtoError = ShimError::Foreign("reference db".into()).into();
        assert!(p.message.starts_with("shim_foreign: "));
        assert_eq!(p.code, split_proto::ErrorCode::ServiceUnavailable);
    }
}
