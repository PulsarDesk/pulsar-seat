// SPDX-License-Identifier: GPL-3.0-only
//! Error type of the audio package.

use seat_proto::remote::{ErrorCode, ProtoError};

/// Errors of `seat-audio`. Every variant has a stable snake_case reason code
/// (`AudioError::code`), see docs/split/agents/contracts.md §0.3 / §6.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AudioError {
    /// `fDisableCam` (policy or RDP listener) blocks audio playback redirection, so the
    /// seat session gets no "Remote Audio" endpoint. The operator owns the policy.
    #[error("audio playback redirection is blocked by policy (fDisableCam)")]
    BlockedByPolicy,
    /// No session render endpoint exists for the seat's session.
    #[error("no audio endpoint for session {0}")]
    NoDevice(u32),
    /// A system query (CfgMgr32, registry) failed.
    #[error("audio probe failed: {0}")]
    Probe(String),
    /// The remover refused a node (present, wrong class, not an RDP session endpoint).
    #[error("refused to remove device node: {0}")]
    RemoveRefused(String),
}

impl AudioError {
    /// Stable reason code for the IPC `reason` field / manager i18n.
    pub fn code(&self) -> &'static str {
        match self {
            AudioError::BlockedByPolicy => "audio_blocked_by_policy",
            AudioError::NoDevice(_) => "audio_no_device",
            AudioError::Probe(_) => "audio_probe",
            AudioError::RemoveRefused(_) => "audio_remove_refused",
        }
    }
}

impl From<AudioError> for ProtoError {
    fn from(e: AudioError) -> Self {
        // Closest existing codes until the integrator adds `policy_blocked` etc. (§9.3).
        let code = match e {
            AudioError::BlockedByPolicy | AudioError::NoDevice(_) => ErrorCode::ServiceUnavailable,
            AudioError::Probe(_) => ErrorCode::Internal,
            AudioError::RemoveRefused(_) => ErrorCode::InvalidValue,
        };
        ProtoError::new(code, format!("{}: {}", e.code(), e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_stable_and_prefix_message() {
        assert_eq!(AudioError::BlockedByPolicy.code(), "audio_blocked_by_policy");
        assert_eq!(AudioError::NoDevice(3).code(), "audio_no_device");
        assert_eq!(AudioError::Probe("x".into()).code(), "audio_probe");
        assert_eq!(AudioError::RemoveRefused("x".into()).code(), "audio_remove_refused");
        let p: ProtoError = AudioError::NoDevice(3).into();
        assert_eq!(p.code, ErrorCode::ServiceUnavailable);
        assert!(p.message.starts_with("audio_no_device: "));
        let p: ProtoError = AudioError::Probe("cfgmgr".into()).into();
        assert_eq!(p.code, ErrorCode::Internal);
    }
}
