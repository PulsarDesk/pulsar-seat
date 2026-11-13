// SPDX-License-Identifier: GPL-3.0-only
//! Terminal Services audio policy (spec 70 §2.4). Read only: Pulsar Split never writes
//! these keys; the operator owns policy.

use serde::{Deserialize, Serialize};

use crate::AudioError;

/// Group-policy key (relative to HKLM).
pub const POLICY_KEY: &str = r"SOFTWARE\Policies\Microsoft\Windows NT\Terminal Services";
/// RDP listener keys live below this key (relative to HKLM), one subkey per listener.
pub const WINSTATIONS_KEY: &str = r"SYSTEM\CurrentControlSet\Control\Terminal Server\WinStations";
/// Default RDP listener name.
pub const DEFAULT_LISTENER: &str = "RDP-Tcp";

pub const V_DISABLE_CAM: &str = "fDisableCam";
pub const V_DISABLE_AUDIO_CAPTURE: &str = "fDisableAudioCapture";
pub const V_AUDIO_QUALITY: &str = "AllowedAudioQualityMode";

/// Where the effective values came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PolicySource {
    /// Neither the policy key nor the listener has a value (Windows default: allowed).
    #[default]
    NotConfigured,
    /// All present values came from the RDP listener key.
    Listener,
    /// All present values came from the group-policy key.
    Policy,
    /// Some values from policy, others from the listener.
    Mixed,
}

/// Effective audio redirection settings: policy value first, then the listener value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AudioPolicy {
    /// `fDisableCam`: 0 allow / non-zero block audio **playback** redirection.
    pub f_disable_cam: Option<u32>,
    /// `fDisableAudioCapture`: 0 allow / non-zero block audio **capture** redirection.
    pub f_disable_audio_capture: Option<u32>,
    pub source: PolicySource,
    /// `AllowedAudioQualityMode` (policy only; 1 dynamic, 3 medium, 7 high). Reported,
    /// never changed (spec 70 §2.4).
    #[serde(default)]
    pub allowed_audio_quality_mode: Option<u32>,
}

impl AudioPolicy {
    /// Playback redirection blocked: the seat gets no Remote Audio endpoint.
    pub fn playback_blocked(&self) -> bool {
        self.f_disable_cam.is_some_and(|v| v != 0)
    }
    /// Capture (microphone) redirection blocked.
    pub fn capture_blocked(&self) -> bool {
        self.f_disable_audio_capture.is_some_and(|v| v != 0)
    }
}

/// Minimal HKLM DWORD reader. `Ok(None)` = key or value absent (or not a DWORD).
pub trait RegistryRead: Send + Sync {
    fn hklm_dword(&self, subkey: &str, value: &str) -> Result<Option<u32>, AudioError>;
}

/// Pure merge of the two sources (policy wins per value).
pub fn resolve_policy(
    policy_cam: Option<u32>,
    policy_capture: Option<u32>,
    listener_cam: Option<u32>,
    listener_capture: Option<u32>,
    quality: Option<u32>,
) -> AudioPolicy {
    let mut from_policy = false;
    let mut from_listener = false;
    let mut pick = |p: Option<u32>, l: Option<u32>| match (p, l) {
        (Some(v), _) => {
            from_policy = true;
            Some(v)
        }
        (None, Some(v)) => {
            from_listener = true;
            Some(v)
        }
        (None, None) => None,
    };
    let f_disable_cam = pick(policy_cam, listener_cam);
    let f_disable_audio_capture = pick(policy_capture, listener_capture);
    let source = match (from_policy, from_listener) {
        (false, false) => PolicySource::NotConfigured,
        (true, false) => PolicySource::Policy,
        (false, true) => PolicySource::Listener,
        (true, true) => PolicySource::Mixed,
    };
    AudioPolicy { f_disable_cam, f_disable_audio_capture, source, allowed_audio_quality_mode: quality }
}

/// Reads the effective policy for `listener` through `reg`.
pub fn read_policy(reg: &dyn RegistryRead, listener: &str) -> Result<AudioPolicy, AudioError> {
    if listener.is_empty() || listener.contains(['\\', '/']) {
        return Err(AudioError::Probe(format!("invalid listener name {listener:?}")));
    }
    let lk = format!(r"{WINSTATIONS_KEY}\{listener}");
    Ok(resolve_policy(
        reg.hklm_dword(POLICY_KEY, V_DISABLE_CAM)?,
        reg.hklm_dword(POLICY_KEY, V_DISABLE_AUDIO_CAPTURE)?,
        reg.hklm_dword(&lk, V_DISABLE_CAM)?,
        reg.hklm_dword(&lk, V_DISABLE_AUDIO_CAPTURE)?,
        reg.hklm_dword(POLICY_KEY, V_AUDIO_QUALITY)?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::FakeRegistry;

    #[test]
    fn resolve_matrix() {
        let p = resolve_policy(None, None, None, None, None);
        assert_eq!(p.source, PolicySource::NotConfigured);
        assert!(!p.playback_blocked() && !p.capture_blocked());

        let p = resolve_policy(None, None, Some(0), Some(0), None);
        assert_eq!(p.source, PolicySource::Listener);
        assert!(!p.playback_blocked());

        let p = resolve_policy(Some(1), None, Some(0), Some(0), Some(3));
        assert_eq!(p.source, PolicySource::Mixed);
        assert!(p.playback_blocked(), "policy overrides listener");
        assert!(!p.capture_blocked());
        assert_eq!(p.allowed_audio_quality_mode, Some(3));

        let p = resolve_policy(Some(0), Some(1), Some(1), Some(0), None);
        assert_eq!(p.source, PolicySource::Policy);
        assert!(!p.playback_blocked());
        assert!(p.capture_blocked());
    }

    #[test]
    fn read_from_fake_registry() {
        let reg = FakeRegistry::default();
        reg.set(r"SYSTEM\CurrentControlSet\Control\Terminal Server\WinStations\RDP-Tcp", V_DISABLE_CAM, 0);
        reg.set(r"SYSTEM\CurrentControlSet\Control\Terminal Server\WinStations\RDP-Tcp", V_DISABLE_AUDIO_CAPTURE, 0);
        reg.set(POLICY_KEY, V_AUDIO_QUALITY, 3);
        let p = read_policy(&reg, DEFAULT_LISTENER).unwrap();
        assert_eq!(p.f_disable_cam, Some(0));
        assert_eq!(p.source, PolicySource::Listener);
        reg.set(POLICY_KEY, V_DISABLE_CAM, 1);
        let p = read_policy(&reg, DEFAULT_LISTENER).unwrap();
        assert!(p.playback_blocked());
        assert_eq!(p.source, PolicySource::Mixed);
        assert!(read_policy(&reg, r"..\x").is_err());
        assert!(read_policy(&reg, "").is_err());
        reg.fail(true);
        assert!(matches!(read_policy(&reg, DEFAULT_LISTENER), Err(AudioError::Probe(_))));
    }
}
