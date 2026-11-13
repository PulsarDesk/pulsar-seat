// SPDX-License-Identifier: GPL-3.0-only
//! Seat audio state and documented limitations surfaced to the manager (spec 70 §3.5,
//! §4, §5, §6). Pure.

use serde::{Deserialize, Serialize};

use crate::config::SeatAudio;
use crate::endpoint::Endpoint;
use crate::policy::AudioPolicy;

/// Audio capture state of a seat (seat runtime status, contracts §9.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioState {
    Capturing,
    NoDevice,
    BlockedByPolicy,
    Disabled,
    Unknown,
}

impl AudioState {
    pub fn code(&self) -> &'static str {
        match self {
            AudioState::Capturing => "capturing",
            AudioState::NoDevice => "no_device",
            AudioState::BlockedByPolicy => "blocked_by_policy",
            AudioState::Disabled => "disabled",
            AudioState::Unknown => "unknown",
        }
    }

    /// Suggested Turkish manager copy (the integrator owns `i18n.ts`).
    pub fn text_tr(&self) -> &'static str {
        match self {
            AudioState::Capturing => "Ses aktarılıyor",
            AudioState::NoDevice => "Ses aygıtı bulunamadı; bu koltukta ses yok",
            AudioState::BlockedByPolicy => {
                "Ses yönlendirmesi grup ilkesiyle engellenmiş (fDisableCam); bu koltukta ses yok"
            }
            AudioState::Disabled => "Ses aktarımı kapalı",
            AudioState::Unknown => "Ses durumu bilinmiyor",
        }
    }
}

/// Effective state. Order: disabled by the seat setting, blocked by policy, then the
/// stream server's own report (authoritative when it has one), then the endpoint check.
/// A present endpoint without a server report is `Unknown` (not proven to capture).
pub fn seat_audio_state(
    s: &SeatAudio,
    policy: &AudioPolicy,
    endpoint: Option<&Endpoint>,
    server_report: Option<AudioState>,
) -> AudioState {
    if !s.stream_audio {
        return AudioState::Disabled;
    }
    if policy.playback_blocked() {
        return AudioState::BlockedByPolicy;
    }
    match server_report {
        Some(AudioState::Unknown) | None => {}
        Some(r) => return r,
    }
    match endpoint {
        Some(e) if e.present => AudioState::Unknown,
        _ => AudioState::NoDevice,
    }
}

/// Best-effort match of one upstream stream-server log line (for an unmodified upstream
/// build that cannot report over the seat pipe). The strings come from the public
/// upstream Sunshine source (GPL-3, `src/audio.cpp`, `src/platform/windows/audio.cpp`,
/// master as of 2026-11); they are version-dependent and not verified against the pinned
/// tag. Returns `None` for lines that say nothing decisive.
///
/// Matching is anchored on the upstream line layout `[timestamp]: Level: message`: the
/// message must **start** with a known text at the expected level. The server also logs
/// text a client or seat user controls (client / device names, app names, sink names),
/// so a marker anywhere else in a line must not flip the seat status.
pub fn scan_log_line(line: &str) -> Option<AudioState> {
    let (level, msg) = split_log_line(line)?;
    let problem = matches!(level, "Warning" | "Error" | "Fatal");
    // Messages that start with fixed upstream text (nothing user-controlled before it).
    const NO_AUDIO_START: &[&str] = &[
        "Unable to initialize audio capture",
        "There will be no audio",
        "The stream will not have audio",
        "Couldn't re-initialize audio input",
        "Couldn't find supported format for audio",
    ];
    if problem && NO_AUDIO_START.iter().any(|m| msg.starts_with(m)) {
        return Some(AudioState::NoDevice);
    }
    // `Couldn't resolve audio sink [<sink name>] to a capture device`: the sink name is
    // operator/user text, so require both the fixed start and the fixed end.
    if problem
        && msg.starts_with("Couldn't resolve audio sink")
        && msg.trim_end_matches('.').ends_with("to a capture device")
    {
        return Some(AudioState::NoDevice);
    }
    if level == "Info" && msg.starts_with("Opus initialized") {
        return Some(AudioState::Capturing);
    }
    None
}

/// Splits an upstream log line `[<timestamp>]: <Level>: <message>` into (level, message).
/// `None` when the line does not have that layout.
fn split_log_line(line: &str) -> Option<(&str, &str)> {
    let line = line.trim_end_matches(['\r', '\n']);
    let rest = line.strip_prefix('[')?;
    let (ts, rest) = rest.split_once("]: ")?;
    if ts.is_empty() || ts.contains(']') {
        return None;
    }
    let (level, msg) = rest.split_once(": ")?;
    const LEVELS: &[&str] = &["Verbose", "Debug", "Info", "Warning", "Error", "Fatal"];
    LEVELS.contains(&level).then_some((level, msg))
}

fn is_reinit_line(line: &str) -> bool {
    split_log_line(line).is_some_and(|(_, m)| m.starts_with("Reinitializing audio capture"))
}

/// Folds log lines in order; the last decisive line wins. A "Reinitializing audio
/// capture" line resets to `Unknown` until the next decisive line.
#[derive(Debug, Default, Clone)]
pub struct LogScanner {
    state: Option<AudioState>,
}

impl LogScanner {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn feed(&mut self, line: &str) {
        if is_reinit_line(line) {
            self.state = Some(AudioState::Unknown);
        } else if let Some(s) = scan_log_line(line) {
            self.state = Some(s);
        }
    }
    /// Current verdict, `None` if nothing decisive was seen.
    pub fn state(&self) -> Option<AudioState> {
        self.state
    }
}

/// Documented limitations shown on the seat / controller pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioLimitation {
    /// Apps in the seat see no microphone (spec 70 §5).
    NoMicrophone,
    /// No controller speaker / headset jack / audio haptics in seats (spec 70 §6).
    NoControllerAudio,
    /// Steam Remote Play inside a seat is not a supported audio path (spec 70 §4).
    SteamRemotePlayUnsupported,
    /// 5.1/7.1 requests carry stereo content (spec 70 §3.3, until tested).
    SurroundIsStereo,
    /// `fDisableCam` blocks playback redirection (spec 70 §2.4).
    PolicyBlocksAudio,
    /// Mic feature on, but `fDisableAudioCapture` blocks it (spec 70 §5.1).
    MicPolicyBlocked,
}

impl AudioLimitation {
    pub fn code(&self) -> &'static str {
        match self {
            AudioLimitation::NoMicrophone => "no_microphone",
            AudioLimitation::NoControllerAudio => "no_controller_audio",
            AudioLimitation::SteamRemotePlayUnsupported => "steam_remote_play_unsupported",
            AudioLimitation::SurroundIsStereo => "surround_is_stereo",
            AudioLimitation::PolicyBlocksAudio => "policy_blocks_audio",
            AudioLimitation::MicPolicyBlocked => "mic_policy_blocked",
        }
    }

    /// Suggested Turkish manager copy (the integrator owns `i18n.ts`).
    pub fn text_tr(&self) -> &'static str {
        match self {
            AudioLimitation::NoMicrophone => "Bu koltuktaki uygulamalar mikrofon göremez.",
            AudioLimitation::NoControllerAudio => {
                "Kol hoparlörü, kulaklık girişi ve ses tabanlı titreşim koltuklarda çalışmaz."
            }
            AudioLimitation::SteamRemotePlayUnsupported => {
                "Koltuk içinde Steam Remote Play sesi desteklenmez."
            }
            AudioLimitation::SurroundIsStereo => {
                "5.1/7.1 istense de ses içeriği stereodur."
            }
            AudioLimitation::PolicyBlocksAudio => {
                "Grup ilkesi ses yönlendirmesini engelliyor; ayarı yönetici değiştirmelidir."
            }
            AudioLimitation::MicPolicyBlocked => {
                "Grup ilkesi mikrofon yönlendirmesini engelliyor (fDisableAudioCapture)."
            }
        }
    }
}

/// Whether an end-to-end microphone path exists in this build. The Pulsar-client mic
/// extension stream and the helper's MS-RDPEAI channel are not built yet (spec 70 §5.1),
/// so this is `false` and `NoMicrophone` is always reported.
pub const MICROPHONE_PATH_AVAILABLE: bool = false;

/// Limitations for a seat, in a stable order.
pub fn limitations(s: &SeatAudio, policy: &AudioPolicy) -> Vec<AudioLimitation> {
    let mut v = Vec::new();
    if policy.playback_blocked() {
        v.push(AudioLimitation::PolicyBlocksAudio);
    }
    let mic_works = MICROPHONE_PATH_AVAILABLE && s.microphone && !policy.capture_blocked();
    if !mic_works {
        v.push(AudioLimitation::NoMicrophone);
    }
    if s.microphone && policy.capture_blocked() {
        v.push(AudioLimitation::MicPolicyBlocked);
    }
    v.push(AudioLimitation::NoControllerAudio);
    v.push(AudioLimitation::SteamRemotePlayUnsupported);
    if s.stream_audio {
        v.push(AudioLimitation::SurroundIsStereo);
    }
    v
}

/// `seat_runtime.audio` block (contracts §9.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatAudioStatus {
    pub state: AudioState,
    pub limitations: Vec<AudioLimitation>,
}

/// Convenience: state + limitations in one call.
pub fn seat_audio_status(
    s: &SeatAudio,
    policy: &AudioPolicy,
    endpoint: Option<&Endpoint>,
    server_report: Option<AudioState>,
) -> SeatAudioStatus {
    SeatAudioStatus {
        state: seat_audio_state(s, policy, endpoint, server_report),
        limitations: limitations(s, policy),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::endpoint::{tests::rdp, Flow};
    use crate::policy::resolve_policy;

    fn open() -> AudioPolicy {
        resolve_policy(None, None, Some(0), Some(0), None)
    }
    fn blocked() -> AudioPolicy {
        resolve_policy(Some(1), None, Some(0), Some(0), None)
    }
    const ON: SeatAudio = SeatAudio { stream_audio: true, microphone: false };
    const OFF: SeatAudio = SeatAudio { stream_audio: false, microphone: false };

    #[test]
    fn state_matrix() {
        let ep = rdp(3, Flow::Render, true);
        let phantom = rdp(3, Flow::Render, false);
        use AudioState::*;
        assert_eq!(seat_audio_state(&OFF, &blocked(), Some(&ep), Some(Capturing)), Disabled);
        assert_eq!(seat_audio_state(&ON, &blocked(), Some(&ep), Some(Capturing)), BlockedByPolicy);
        assert_eq!(seat_audio_state(&ON, &open(), Some(&ep), Some(Capturing)), Capturing);
        assert_eq!(seat_audio_state(&ON, &open(), None, Some(Capturing)), Capturing, "server wins");
        assert_eq!(seat_audio_state(&ON, &open(), Some(&ep), Some(NoDevice)), NoDevice);
        assert_eq!(seat_audio_state(&ON, &open(), Some(&ep), None), Unknown);
        assert_eq!(seat_audio_state(&ON, &open(), Some(&ep), Some(Unknown)), Unknown);
        assert_eq!(seat_audio_state(&ON, &open(), None, None), NoDevice);
        assert_eq!(seat_audio_state(&ON, &open(), Some(&phantom), None), NoDevice);
        assert_eq!(seat_audio_state(&ON, &open(), None, Some(Unknown)), NoDevice);
    }

    const TS: &str = "[2026-11-12 10:00:00.000]: ";

    fn l(rest: &str) -> String {
        format!("{TS}{rest}")
    }

    #[test]
    fn log_lines() {
        assert_eq!(
            scan_log_line(&l("Error: Unable to initialize audio capture. The stream will not have audio.")),
            Some(AudioState::NoDevice)
        );
        assert_eq!(scan_log_line(&l("Warning: There will be no audio")), Some(AudioState::NoDevice));
        assert_eq!(
            scan_log_line(&l("Error: Couldn't resolve audio sink [x] to a capture device")),
            Some(AudioState::NoDevice)
        );
        assert_eq!(
            scan_log_line(&l("Info: Opus initialized: 48 kHz, 2 channels, 96 kbps (total), LOWDELAY\r\n")),
            Some(AudioState::Capturing)
        );
        assert_eq!(scan_log_line(&l("Info: Reinitializing audio capture")), None);
        assert_eq!(scan_log_line(&l("Info: New streaming session started")), None);
        assert_eq!(scan_log_line(""), None);
        // Without the upstream prefix, or at the wrong level, nothing is decisive.
        assert_eq!(scan_log_line("Info: Opus initialized: 48 kHz"), None);
        assert_eq!(scan_log_line("Opus initialized"), None);
        assert_eq!(scan_log_line(&l("Error: Opus initialized")), None);
        assert_eq!(scan_log_line(&l("Info: There will be no audio")), None);
        assert_eq!(scan_log_line(&l("Bogus: Opus initialized")), None);
    }

    #[test]
    fn log_lines_ignore_user_controlled_text() {
        // A paired client / device / app name carrying the marker text.
        for line in [
            l("Info: New client paired: Opus initialized"),
            l("Info: Client name: [Opus initialized]"),
            l("Warning: Unknown app [There will be no audio]"),
            l("Info: Device 'to a capture device' connected"),
            l("Error: Failed to launch [The stream will not have audio]"),
            // Sink name spoofing the end but the line starts elsewhere.
            l("Warning: client x: Couldn't resolve audio sink [y] to a capture device"),
            // A name with an embedded prefix is still inside one line.
            format!("{TS}Info: Paired {TS}Info: Opus initialized"),
        ] {
            assert_eq!(scan_log_line(&line), None, "{line}");
        }
        // The fixed start alone is not enough for the sink message.
        assert_eq!(scan_log_line(&l("Error: Couldn't resolve audio sink [x]")), None);
    }

    #[test]
    fn log_scanner_last_wins() {
        let mut s = LogScanner::new();
        assert_eq!(s.state(), None);
        s.feed(&l("Info: Opus initialized: 48 kHz"));
        assert_eq!(s.state(), Some(AudioState::Capturing));
        s.feed(&l("Info: Reinitializing audio capture"));
        assert_eq!(s.state(), Some(AudioState::Unknown));
        s.feed(&l("Error: Couldn't re-initialize audio input"));
        assert_eq!(s.state(), Some(AudioState::NoDevice));
        s.feed("unrelated");
        s.feed(&l("Info: Paired client Reinitializing audio capture"));
        assert_eq!(s.state(), Some(AudioState::NoDevice));
    }

    #[test]
    fn limitation_sets() {
        use AudioLimitation::*;
        assert_eq!(
            limitations(&ON, &open()),
            vec![NoMicrophone, NoControllerAudio, SteamRemotePlayUnsupported, SurroundIsStereo]
        );
        assert_eq!(
            limitations(&OFF, &blocked()),
            vec![PolicyBlocksAudio, NoMicrophone, NoControllerAudio, SteamRemotePlayUnsupported]
        );
        let mic = SeatAudio { stream_audio: true, microphone: true };
        let mic_blocked = resolve_policy(None, Some(1), None, None, None);
        let l = limitations(&mic, &mic_blocked);
        assert!(l.contains(&MicPolicyBlocked) && l.contains(&NoMicrophone));
        assert!(!limitations(&mic, &open()).contains(&MicPolicyBlocked));
        // Path not built yet: mic still listed as missing.
        assert!(limitations(&mic, &open()).contains(&NoMicrophone));
    }

    #[test]
    fn serde_codes_match_code_fn() {
        for s in [AudioState::Capturing, AudioState::NoDevice, AudioState::BlockedByPolicy, AudioState::Disabled, AudioState::Unknown] {
            assert_eq!(serde_json::to_string(&s).unwrap(), format!("\"{}\"", s.code()));
            assert!(!s.text_tr().is_empty());
        }
        use AudioLimitation::*;
        for l in [NoMicrophone, NoControllerAudio, SteamRemotePlayUnsupported, SurroundIsStereo, PolicyBlocksAudio, MicPolicyBlocked] {
            assert_eq!(serde_json::to_string(&l).unwrap(), format!("\"{}\"", l.code()));
            assert!(!l.text_tr().is_empty());
        }
        let st = seat_audio_status(&ON, &open(), None, None);
        let j = serde_json::to_value(&st).unwrap();
        assert_eq!(j["state"], "no_device");
        assert_eq!(j["limitations"][0], "no_microphone");
    }
}
