// SPDX-License-Identifier: GPL-3.0-only
//! Per-seat audio settings, stream-server config keys (spec 70 §3.5) and the headless
//! RDP client's audio flags (spec 70 §2.6, §5.1).

use serde::{Deserialize, Serialize};

use crate::endpoint::{Endpoint, Flow};
use crate::policy::AudioPolicy;
use crate::state::MICROPHONE_PATH_AVAILABLE;

/// Seat audio settings (goes into `RemoteSeatRecord::audio`, contracts §9.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SeatAudio {
    /// Stream the seat's audio (`stream_audio`). The only audio switch the manager shows.
    pub stream_audio: bool,
    /// Microphone redirection into the seat (later feature, spec 70 §5.1). Default off.
    pub microphone: bool,
}

impl Default for SeatAudio {
    fn default() -> Self {
        Self { stream_audio: true, microphone: false }
    }
}

/// Written by `seat-stream` (`conf::generate`, always `disabled`), **not** by
/// [`server_keys`]: the stream writer owns the key and rejects audio keys that duplicate
/// its own (open question audio-7). Kept for reference.
pub const K_INSTALL_STEAM_AUDIO_DRIVERS: &str = "install_steam_audio_drivers";
pub const K_STREAM_AUDIO: &str = "stream_audio";
pub const K_VIRTUAL_SINK: &str = "virtual_sink";
pub const K_AUDIO_SINK: &str = "audio_sink";

/// Audio keys for the seat's stream-server config (`ConfInputs::audio_keys`).
///
/// - `install_steam_audio_drivers` is **not** emitted here: `seat-stream` writes
///   `install_steam_audio_drivers = disabled` itself (a machine-wide Steam sink would mix
///   all seats, spec 70 §3.1) and refuses duplicates (open question audio-7).
/// - `stream_audio = enabled|disabled`.
/// - `audio_sink` / `virtual_sink` are **omitted** (upstream default: the session's
///   default render device, which in a seat is its Remote Audio endpoint), unless
///   `pinned` is a present render RDP session endpoint read from the live node; then both
///   are set to its endpoint id (never the friendly name, which may be localized).
///   An unusable pin is ignored with a warning.
pub fn server_keys(s: &SeatAudio, pinned: Option<&Endpoint>) -> Vec<(String, String)> {
    let mut keys = vec![(
        K_STREAM_AUDIO.to_string(),
        if s.stream_audio { "enabled" } else { "disabled" }.to_string(),
    )];
    if let Some(ep) = pinned {
        if pin_usable(ep) {
            keys.push((K_AUDIO_SINK.to_string(), ep.endpoint_id.clone()));
            keys.push((K_VIRTUAL_SINK.to_string(), ep.endpoint_id.clone()));
        } else {
            log::warn!("audio: pinned endpoint not usable, leaving sinks at the session default");
        }
    }
    keys
}

fn pin_usable(ep: &Endpoint) -> bool {
    ep.present
        && ep.flow == Flow::Render
        && ep.is_rdp_session_endpoint()
        && !ep.endpoint_id.is_empty()
        && !ep.endpoint_id.chars().any(|c| c.is_control() || c == '=' || c == '#')
}

/// MS-RDPBCGR TS_INFO_PACKET flags relevant for audio.
pub const INFO_REMOTECONSOLEAUDIO: u32 = 0x0000_2000;
pub const INFO_NOAUDIOPLAYBACK: u32 = 0x0008_0000;
pub const INFO_AUDIOCAPTURE: u32 = 0x0020_0000;

/// What the seat's headless RDP client must do for audio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RdpAudioFlags {
    /// Always true: audio mode "redirect to client"; join the playback channel and
    /// acknowledge/discard wave PDUs so the server paces the endpoint (spec 70 §2.6).
    pub playback: bool,
    /// Set `INFO_AUDIOCAPTURE` and open the audio-input channel (MS-RDPEAI).
    pub capture: bool,
}

impl RdpAudioFlags {
    /// Audio bits to OR into TS_INFO_PACKET.flags. Never sets `INFO_REMOTECONSOLEAUDIO`
    /// or `INFO_NOAUDIOPLAYBACK`.
    pub fn info_flags(&self) -> u32 {
        if self.capture { INFO_AUDIOCAPTURE } else { 0 }
    }

    /// Mask of audio bits the client must clear before sending its info packet.
    pub const fn forbidden_info_flags() -> u32 {
        INFO_REMOTECONSOLEAUDIO | INFO_NOAUDIOPLAYBACK
    }

    /// Arguments for the headless session hold (`pulsar-seat-host --headless-hold`, contracts §6 integration).
    pub fn helper_args(&self) -> Vec<String> {
        let mut v = Vec::new();
        if self.playback {
            v.push("--audio-playback".to_string());
        }
        if self.capture {
            v.push("--audio-capture".to_string());
        }
        v
    }
}

/// RDP client flags for a seat. Capture only when the end-to-end mic path exists in this
/// build ([`MICROPHONE_PATH_AVAILABLE`], spec 70 §5.1), the seat's mic feature is on
/// **and** capture redirection is not blocked by policy. Otherwise `INFO_AUDIOCAPTURE`
/// would create a capture endpoint nobody feeds (a silent mic, spec 70 §2.6).
pub fn rdp_client_flags(s: &SeatAudio, policy: &AudioPolicy) -> RdpAudioFlags {
    RdpAudioFlags {
        playback: true,
        capture: MICROPHONE_PATH_AVAILABLE && s.microphone && !policy.capture_blocked(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::endpoint::tests::rdp;
    use crate::policy::resolve_policy;

    fn kv(keys: &[(String, String)]) -> Vec<(&str, &str)> {
        keys.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect()
    }

    #[test]
    fn default_seat_audio() {
        let s = SeatAudio::default();
        assert!(s.stream_audio && !s.microphone);
        let s: SeatAudio = serde_json::from_str("{}").unwrap();
        assert_eq!(s, SeatAudio::default());
        let s: SeatAudio = serde_json::from_str(r#"{"stream_audio":false}"#).unwrap();
        assert!(!s.stream_audio);
    }

    #[test]
    fn keys_default_omit_sinks() {
        let k = server_keys(&SeatAudio::default(), None);
        assert_eq!(kv(&k), vec![("stream_audio", "enabled")]);
        assert!(!k.iter().any(|(k, _)| k == K_INSTALL_STEAM_AUDIO_DRIVERS), "seat-stream owns it");
        let off = SeatAudio { stream_audio: false, microphone: false };
        assert_eq!(kv(&server_keys(&off, None)), vec![("stream_audio", "disabled")]);
    }

    #[test]
    fn keys_pin_uses_endpoint_id_not_name() {
        let ep = rdp(3, Flow::Render, true);
        let k = server_keys(&SeatAudio::default(), Some(&ep));
        assert_eq!(k.len(), 3);
        assert!(k.contains(&("audio_sink".into(), ep.endpoint_id.clone())));
        assert!(k.contains(&("virtual_sink".into(), ep.endpoint_id.clone())));
        assert!(k.iter().all(|(_, v)| !v.contains("Remote Audio")));
    }

    #[test]
    fn bad_pins_ignored() {
        let mut bad = vec![rdp(3, Flow::Render, false), rdp(3, Flow::Capture, true)];
        let mut crlf = rdp(3, Flow::Render, true);
        crlf.endpoint_id.push_str("\r\nx=y");
        bad.push(crlf);
        let mut console = rdp(1, Flow::Render, true);
        console.endpoint_id = "{0.0.0.00000000}.{11111111-2222-3333-4444-555555555555}".into();
        bad.push(console);
        for ep in &bad {
            assert_eq!(server_keys(&SeatAudio::default(), Some(ep)).len(), 1, "{ep:?}");
        }
    }

    #[test]
    fn rdp_flags() {
        let open = resolve_policy(None, None, Some(0), Some(0), None);
        let mic_blocked = resolve_policy(None, Some(1), None, None, None);
        let f = rdp_client_flags(&SeatAudio::default(), &open);
        assert_eq!(f, RdpAudioFlags { playback: true, capture: false });
        assert_eq!(f.info_flags(), 0);
        assert_eq!(f.helper_args(), vec!["--audio-playback"]);
        let mic = SeatAudio { stream_audio: true, microphone: true };
        let f = rdp_client_flags(&mic, &open);
        // The mic path is not built (spec 70 §5.1): no INFO_AUDIOCAPTURE even with the
        // seat's mic switch on, so Windows creates no silent capture endpoint.
        assert_eq!(f.capture, MICROPHONE_PATH_AVAILABLE);
        assert!(!f.capture);
        assert_eq!(f.info_flags(), 0);
        assert_eq!(f.helper_args(), vec!["--audio-playback"]);
        assert!(!rdp_client_flags(&mic, &mic_blocked).capture);
        // Once the path exists, the flags map like this:
        let cap = RdpAudioFlags { playback: true, capture: true };
        assert_eq!(cap.info_flags(), INFO_AUDIOCAPTURE);
        assert_eq!(cap.helper_args(), vec!["--audio-playback", "--audio-capture"]);
        // Even with streaming off, the session keeps "redirect to client" so apps do not
        // play on the console speakers.
        let off = SeatAudio { stream_audio: false, microphone: false };
        assert!(rdp_client_flags(&off, &open).playback);
        assert_eq!(f.info_flags() & RdpAudioFlags::forbidden_info_flags(), 0);
    }
}
