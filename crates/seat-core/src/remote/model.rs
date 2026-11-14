// SPDX-License-Identifier: GPL-3.0-only
//! Remote seat model (from Pulsar Split `split-proto::model`, merge step 4): the per-seat
//! record, the remote machine settings and their value types. Wire-only status types are
//! in `seat_proto::remote`.
//!
//! Renamed at the merge: Split's `RemoteSeatRecord` is [`RemoteSeatRecord`], Split's
//! `RemoteSettings` is [`RemoteSettings`] (stored as `settings.remote` of the config
//! document). Package-owned structures travel as `serde_json::Value`: this crate must not
//! depend on the packages (they depend on it).

use serde::{Deserialize, Serialize};

/// Log level for the service and the per-seat stream servers. Stored as index 0–4.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verbosity {
    None,
    Error,
    Warning,
    #[default]
    Info,
    Debug,
}

impl Verbosity {
    pub fn index(self) -> u32 {
        self as u32
    }

    pub fn from_index(i: u32) -> Option<Self> {
        Some(match i {
            0 => Self::None,
            1 => Self::Error,
            2 => Self::Warning,
            3 => Self::Info,
            4 => Self::Debug,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RemoteSettings {
    /// Service start type automatic (on) or manual (off).
    pub start_with_windows: bool,
    pub hid_isolation: bool,
    pub process_patching: bool,
    /// Only meaningful when `process_patching` is on.
    pub steam_isolation: bool,
    pub verbosity: Verbosity,
    /// v2: prepare the host for several seat sessions (RDS settings, spec 30 §2.3). Off =
    /// the service writes no RDS settings and starts no seat session.
    pub multi_session: bool,
    /// v2: preferred virtual pad backend; `None` = automatic.
    pub pad_backend: Option<PadBackendPref>,
    /// v2: Bluetooth per-user services only in the console session (spec 60 §10).
    pub bluetooth_console_only: bool,
    /// v2: display backend override; `None` = automatic.
    pub display_backend: Option<DisplayBackendPref>,
}

impl Default for RemoteSettings {
    fn default() -> Self {
        Self {
            start_with_windows: false,
            hid_isolation: false,
            process_patching: false,
            steam_isolation: false,
            verbosity: Verbosity::default(),
            multi_session: false,
            pad_backend: None,
            bluetooth_console_only: true,
            display_backend: None,
        }
    }
}

/// Same wire names as `seat_pads::BackendId`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PadBackendPref {
    Vigem,
    UsbipDs5,
    PulsarPad,
}

/// Same wire names as `seat_display::remote::BackendKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayBackendPref {
    RdpSession,
    Vdd,
    PulsarIdd,
}

/// What happens to the seat's Windows session when the seat stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopAction {
    #[default]
    LogOff,
    Disconnect,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DisplaySettings {
    /// Adapter description as shown to the user; `None` = pick the best adapter.
    pub render_adapter: Option<String>,
    /// Static scale (use `scale_percent`) instead of following the client.
    pub scale_static: bool,
    pub scale_percent: u16,
    pub super_sampling_percent: u16,
    pub sdr_white_level_nits: u16,
    pub force_sdr: bool,
    /// v2: PnP instance id of the render adapter (`render_adapter` keeps the description).
    pub render_adapter_id: Option<String>,
    /// v2: default refresh rate, 15–500 Hz.
    pub refresh_hz: u32,
    /// v2: advanced HDR luminance of the seat monitor.
    pub luminance: HdrLuminanceSettings,
}

impl Default for DisplaySettings {
    fn default() -> Self {
        Self {
            render_adapter: None,
            scale_static: false,
            scale_percent: 100,
            super_sampling_percent: 100,
            sdr_white_level_nits: 400,
            force_sdr: false,
            render_adapter_id: None,
            refresh_hz: 60,
            luminance: HdrLuminanceSettings::default(),
        }
    }
}

/// Same fields as `seat_display::remote::HdrLuminance`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HdrLuminanceSettings {
    pub min_nits_x10000: u32,
    pub max_nits: u32,
    pub max_full_frame_nits: u32,
}

impl Default for HdrLuminanceSettings {
    fn default() -> Self {
        Self { min_nits_x10000: 10, max_nits: 1000, max_full_frame_nits: 600 }
    }
}

/// Same fields as `seat_audio::SeatAudio`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SeatAudioSettings {
    pub stream_audio: bool,
    /// Later feature (spec 70 §5.1): stored, not used until the microphone path exists.
    pub microphone: bool,
}

impl Default for SeatAudioSettings {
    fn default() -> Self {
        Self { stream_audio: true, microphone: false }
    }
}

/// Controller the stream server emulates (`gamepad` key, spec 80 §4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GamepadPref {
    #[default]
    Auto,
    X360,
    Ds4,
}

/// Capture method (`capture` key); `Default` = key not written (server chooses).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapturePref {
    #[default]
    Default,
    Ddx,
    Wgc,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SeatStreamSettings {
    pub gamepad: GamepadPref,
    pub capture: CapturePref,
    /// `minimum_fps_target` (spec 80 default 1).
    pub min_fps: u32,
    /// Also open the seat's firewall rules on the Domain profile.
    pub firewall_domain: bool,
    /// Keep the pairing state when the seat is deleted.
    pub keep_pairing_on_delete: bool,
}

impl Default for SeatStreamSettings {
    fn default() -> Self {
        Self {
            gamepad: GamepadPref::Auto,
            capture: CapturePref::Default,
            min_fps: 1,
            firewall_domain: false,
            keep_pairing_on_delete: false,
        }
    }
}

/// Stream server a seat runs (contracts §7.4, docs/split/agents/pulsar-host-backend.md). Each
/// seat runs exactly one. Missing in a v2 config = `sunshine`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamBackendKind {
    /// Upstream Sunshine (Moonlight clients).
    #[default]
    Sunshine,
    /// `pulsar-host` (Pulsar clients).
    Pulsar,
}

impl StreamBackendKind {
    pub const ALL: [StreamBackendKind; 2] = [StreamBackendKind::Sunshine, StreamBackendKind::Pulsar];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sunshine => "sunshine",
            Self::Pulsar => "pulsar",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AutostartApp {
    /// GUID string; stable key of the entry.
    pub id: String,
    pub path: String,
    pub arguments: String,
    /// Run as SYSTEM inside the seat session instead of as the seat user. Ignored with a
    /// warning since v2 (spec 30 §8): autostart apps always run as the seat user.
    pub run_as_system: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RemoteSeatRecord {
    /// GUID string, assigned at creation, never changes (renames keep it).
    pub id: String,
    /// Stable short name: ASCII letters/digits, 1–128 chars, unique (case-insensitive).
    pub name: String,
    /// Host name advertised to clients.
    pub display_name: String,
    /// Local Windows account (SAM name); empty = not chosen yet.
    pub user_name: String,
    /// Stream-server base port; the seat occupies `port-5 ..= port+21`.
    pub port: u16,
    pub start_with_service: bool,
    pub stop_action: StopAction,
    pub display: DisplaySettings,
    pub autostart: Vec<AutostartApp>,
    /// v2: RDP client name of the seat's loopback session (`PSS` + 8 hex), generated by
    /// the service at creation, unique among seats; the manager cannot change it.
    pub rdp_client_name: String,
    /// v2.
    pub audio: SeatAudioSettings,
    /// v2.
    pub stream: SeatStreamSettings,
    /// Stream server of this seat; `sunshine` when missing (v2 configs). Changing it needs
    /// the seat stopped (the whole config is locked while the seat is active).
    pub stream_backend: StreamBackendKind,
    /// Last display mode a client asked for and the display applied (contracts §3.3,
    /// spec 40 §4.4). Service-owned: the manager cannot change it; used for the session
    /// size at the next seat start.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_mode: Option<LastMode>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LastMode {
    pub width: u32,
    pub height: u32,
    pub hz: u32,
}

impl LastMode {
    /// Plausible session size (RDP accepts 200–8192 per side; we keep 640×480 … 7680×4320).
    pub fn is_plausible(&self) -> bool {
        (640..=7680).contains(&self.width) && (480..=4320).contains(&self.height) && (15..=500).contains(&self.hz)
    }
}

