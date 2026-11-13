// SPDX-License-Identifier: GPL-3.0-only
//! The configuration document, owned by the service (spec 20 §13.2).
//!
//! File: `%ProgramData%\Pulsar Seat\config.json` (ACL SYSTEM + Administrators full, Users
//! read). JSON, `schema_version` + `revision`. Migrations run on read
//! ([`Config::from_json`]). Secrets never appear here (they live in the
//! [`crate::store::SecretStore`]); the manager's UI preferences live in the app's own
//! per-user store.

use crate::backend::{
    AudioBackendKind, DisplayBackendKind, GateBackendKind, InputBackendKind, SessionBackendKind,
    session::HostTopology,
};
use crate::device::DeviceKind;
use crate::ids::{DeviceKey, MAX_SEATS, SeatMask, SeatNumber};
use serde::{Deserialize, Serialize};

/// Current schema. Bump with a migration in [`Config::from_json`].
pub const CONFIG_SCHEMA_VERSION: u32 = 1;

/// Version of the experimental-backend warning text. The manager shows the warning and
/// sends this number back; a changed warning needs a fresh acknowledgement.
pub const EXPERIMENTAL_WARNING_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub schema_version: u32,
    /// Incremented by the service on every successful apply (optimistic concurrency:
    /// `apply_config` carries the revision it was based on).
    pub revision: u64,
    /// Seat 1 always exists. Seats are kept sorted by number.
    pub seats: Vec<SeatConfig>,
    /// Device assignments keyed by stable device key. Devices not listed get their kind's
    /// default (or inherit from a hub).
    pub devices: Vec<DeviceAssignment>,
    pub settings: GlobalSettings,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            schema_version: CONFIG_SCHEMA_VERSION,
            revision: 0,
            seats: vec![SeatConfig::new(SeatNumber::CONSOLE)],
            devices: Vec::new(),
            settings: GlobalSettings::default(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("config schema {0} is newer than this build supports ({CONFIG_SCHEMA_VERSION})")]
    TooNew(u32),
}

impl Config {
    /// Parse and migrate. Unknown fields are ignored; missing fields take defaults.
    pub fn from_json(text: &str) -> Result<Self, ConfigError> {
        let mut v: serde_json::Value = serde_json::from_str(text)?;
        let schema = v.get("schema_version").and_then(|s| s.as_u64()).unwrap_or(1) as u32;
        if schema > CONFIG_SCHEMA_VERSION {
            return Err(ConfigError::TooNew(schema));
        }
        // Migrations go here: `if schema < 2 { migrate_1_to_2(&mut v); }`.
        if let Some(obj) = v.as_object_mut() {
            obj.insert("schema_version".into(), CONFIG_SCHEMA_VERSION.into());
        }
        let mut c: Config = serde_json::from_value(v)?;
        c.normalize();
        Ok(c)
    }

    /// Pretty JSON for the file.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("config serializes")
    }

    /// Sort seats, ensure seat 1 exists, sort devices by key.
    pub fn normalize(&mut self) {
        self.seats.sort_by_key(|s| s.number);
        if self.seats.first().map(|s| s.number) != Some(SeatNumber::CONSOLE) {
            self.seats.insert(0, SeatConfig::new(SeatNumber::CONSOLE));
        }
        self.seats[0].autostart = true;
        self.devices.sort_by(|a, b| a.key.cmp(&b.key));
    }

    pub fn seat(&self, n: SeatNumber) -> Option<&SeatConfig> {
        self.seats.iter().find(|s| s.number == n)
    }

    pub fn seat_mut(&mut self, n: SeatNumber) -> Option<&mut SeatConfig> {
        self.seats.iter_mut().find(|s| s.number == n)
    }

    pub fn device(&self, key: &DeviceKey) -> Option<&DeviceAssignment> {
        self.devices.iter().find(|d| &d.key == key)
    }

    /// Highest configured seat number.
    pub fn seat_count(&self) -> u8 {
        self.seats.iter().map(|s| s.number.get()).max().unwrap_or(1)
    }

    /// Seats with `autostart` (seat 1 always included).
    pub fn autostart_mask(&self) -> SeatMask {
        SeatMask::from_seats(self.seats.iter().filter(|s| s.autostart).map(|s| s.number))
    }
}

/// One seat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatConfig {
    pub number: SeatNumber,
    /// Display name, 1..=64 characters; default = the number.
    pub name: String,
    /// Start automatically (per start mode). Always true for seat 1.
    #[serde(default = "yes")]
    pub autostart: bool,
    #[serde(default)]
    pub account: SeatAccount,
    /// CPU-set ids the seat's launcher-started processes may use (spec 80 §11.2).
    /// Empty = all.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cpu_sets: Vec<u32>,
    /// Hard limit through a job (one processor group) instead of soft CPU sets.
    #[serde(default)]
    pub cpu_hard_limit: bool,
    /// Our generated wallpaper in the seat's session (spec 10 §4.4).
    #[serde(default)]
    pub wallpaper: bool,
    /// Kiosk app for the `Desktops` backend (ignored by others).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kiosk: Option<KioskApp>,
    /// Source IPv4 address for the seat's programs (spec 10 §7.4, spec 80 §4.2 / §11.3).
    /// The service stores it; enforcement is user-mode best effort (seat-compat). `None` =
    /// not assigned. Added 2026-11-13 (additive).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip_binding: Option<IpBinding>,
    /// Apps the service starts in the seat's session at each logon, inside the seat's job
    /// object, so `cpu_sets` reach them and their children (spec 80 §11.2). Added
    /// 2026-11-13 (additive).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub startup_apps: Vec<KioskApp>,
    /// RDP virtual-channel redirection for the seat's session (clipboard, drives, smart
    /// cards; spec 70, spec 00 §5.2.1). Everything off by default. Added 2026-11-13
    /// (additive).
    #[serde(default, skip_serializing_if = "SeatRedirect::is_default")]
    pub redirect: SeatRedirect,
    /// Concurrent kiosk seat in the console session (`desktops` backend, spec 35 §5.1 U3):
    /// the Pulsar kiosk shell owns the seat's monitors and starts only these apps. Takes
    /// precedence over [`SeatConfig::kiosk`]. Added 2026-11-13 (additive).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kiosk_shell: Option<KioskShell>,
}

/// Kiosk shell of one seat (spec 35 §5.1 U3, D′). Same Windows user, profile and clipboard
/// as the console; input reaches the apps as posted window messages (see
/// `seat_input::kiosk` for the limits).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct KioskShell {
    /// Apps the seat may start from its launcher, at most [`MAX_KIOSK_APPS`].
    pub apps: Vec<KioskShellApp>,
    /// Reset the seat (close its apps, show the launcher, start the autostart apps again)
    /// after this many minutes without input. 0 = off; else 1..=1440.
    pub idle_reset_minutes: u32,
}

/// One allowed app of a kiosk seat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KioskShellApp {
    /// Tile caption, 1..=64 characters.
    pub title: String,
    /// Full path of the program.
    pub command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Start when the seat starts and after each reset.
    #[serde(default)]
    pub autostart: bool,
    /// Index of the seat monitor the app's window goes to (clamped to the seat's monitors).
    #[serde(default)]
    pub monitor: u8,
}

/// Most apps per kiosk seat.
pub const MAX_KIOSK_APPS: usize = 16;

/// Machine-wide kiosk-seat options (spec 35 §5.1). Added 2026-11-13 (additive).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct KioskSettings {
    /// Seat 1 stays the console user's normal desktop next to the kiosk seats (U3+U1).
    /// The kiosk shells then never take the foreground. Without the input filter the kiosk
    /// seats' keys also reach seat 1's focused window (`input_not_isolated`). `false`: seat
    /// 1 is a kiosk seat too and the shells keep the foreground on their own sink window.
    pub seat1_desktop: bool,
}

impl Default for KioskSettings {
    fn default() -> Self {
        Self { seat1_desktop: true }
    }
}

/// Per-seat redirection policy of the RDP seat session (IronRDP host backend). Off by
/// default: a redirected clipboard is bridged to the **host session's** clipboard (the
/// console, where seat 1's user also works), so it is shared with every other seat that has
/// it on.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SeatRedirect {
    #[serde(default)]
    pub clipboard: ClipboardRedirect,
    #[serde(default)]
    pub drives: DriveRedirect,
    /// Smart-card API redirected to the readers assigned to this seat only (MS-RDPESC).
    #[serde(default)]
    pub smartcards: bool,
}

impl SeatRedirect {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// Direction of the clipboard bridge, seen from the seat session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipboardDirection {
    #[default]
    Off,
    /// Host session clipboard -> seat session (paste into the seat only).
    ToSeat,
    /// Seat session -> host session clipboard (copy out of the seat only).
    FromSeat,
    Both,
}

impl ClipboardDirection {
    pub fn to_seat(self) -> bool {
        matches!(self, Self::ToSeat | Self::Both)
    }
    pub fn from_seat(self) -> bool {
        matches!(self, Self::FromSeat | Self::Both)
    }
}

/// Clipboard redirection and the format classes it may carry (MS-RDPECLIP).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardRedirect {
    #[serde(default)]
    pub direction: ClipboardDirection,
    /// Plain, OEM and Unicode text (and the locale that goes with it).
    #[serde(default = "yes")]
    pub text: bool,
    /// `HTML Format`, rich text.
    #[serde(default = "yes")]
    pub html: bool,
    /// Bitmaps (DIB, DIBV5, PNG and other image formats).
    #[serde(default = "yes")]
    pub images: bool,
    /// File lists and file contents (copy / paste of files).
    #[serde(default)]
    pub files: bool,
    /// Data another seat put on the shared host-session clipboard may reach this seat.
    /// Off by default: a console copy whose clipboard owner is another seat host is refused
    /// (`clipboard_from_other_seat`). Added 2026-11-13 (additive).
    #[serde(default)]
    pub cross_seat: bool,
}

impl Default for ClipboardRedirect {
    fn default() -> Self {
        Self { direction: ClipboardDirection::Off, text: true, html: true, images: true, files: false, cross_seat: false }
    }
}

/// Drive redirection (MS-RDPEFS) of the volumes this seat owns alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DriveRedirect {
    #[default]
    Off,
    ReadOnly,
    ReadWrite,
}

/// Per-seat source address and the rule that picks the programs it applies to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IpBinding {
    /// Dotted IPv4 (`192.168.1.20`); one of the PC's static addresses.
    pub address: String,
    #[serde(default)]
    pub rule: IpRule,
}

/// Which programs of the seat use [`IpBinding::address`] (spec 10 §7.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IpRule {
    #[default]
    AllPrograms,
    OnlyListed,
    AllExceptListed,
}

/// One row of the program list used by [`IpRule::OnlyListed`] / [`IpRule::AllExceptListed`].
/// The list is common to all seats (spec 10 §7.4). A folder path means every program in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IpProgram {
    pub title: String,
    pub path: String,
    /// Unticked = switched off for a while, kept in the list.
    #[serde(default = "yes")]
    pub enabled: bool,
}

/// Longest program list kept on save (longer lists get a warning, spec 10 §7.4).
pub const MAX_IP_PROGRAMS: usize = 256;

fn yes() -> bool {
    true
}

impl SeatConfig {
    pub fn new(number: SeatNumber) -> Self {
        Self {
            number,
            name: number.to_string(),
            autostart: true,
            account: SeatAccount::LogonScreen,
            cpu_sets: Vec::new(),
            cpu_hard_limit: false,
            wallpaper: false,
            kiosk: None,
            ip_binding: None,
            startup_apps: Vec::new(),
            redirect: SeatRedirect::default(),
            kiosk_shell: None,
        }
    }
}

/// The seat's sign-in account (spec 10 §7.11). The password is in the secret store.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SeatAccount {
    /// Show the normal logon screen (default; recommended for seat 1).
    #[default]
    LogonScreen,
    Local { user: String },
    Domain { domain: String, user: String },
}

impl SeatAccount {
    /// `(domain, user)`; local accounts use `"."`.
    pub fn domain_user(&self) -> Option<(&str, &str)> {
        match self {
            Self::LogonScreen => None,
            Self::Local { user } => Some((".", user)),
            Self::Domain { domain, user } => Some((domain, user)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KioskApp {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

/// One device's assignment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceAssignment {
    pub key: DeviceKey,
    pub kind: DeviceKind,
    pub mask: SeatMask,
    /// Friendly name at save time (display only).
    #[serde(default)]
    pub name: String,
    /// Port chain / surface label at save time (display only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Secondary identity for "device moved to a new port" (spec 70 §5.1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hardware_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_id: Option<String>,
    #[serde(default, skip_serializing_if = "AssignmentOptions::is_default")]
    pub options: AssignmentOptions,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AssignmentOptions {
    /// Monitors: software cursor instead of hardware cursor.
    pub software_cursor: bool,
    /// Custom picture file name (manager), from the category folder.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub picture: Option<String>,
    /// Monitors: CCD target ids of the adapter outputs that should feed this monitor's
    /// surface (spec 10 §7.5). Empty = automatic; more than one = clone (needs
    /// [`OutputSettings::allow_clone`]). Applied at the next seat start / reboot. Added
    /// 2026-11-13 (additive).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<u32>,
}

impl AssignmentOptions {
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

/// When extra seats start (spec 10 §4.2, spec 30 §6.2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartMode {
    /// Only from the manager / tray (safest, recommended at first).
    #[default]
    Manual,
    /// When the service and its dependencies are ready.
    AtBoot,
    /// After the first interactive logon on seat 1.
    AfterSeatOneLogon,
}

/// One Windows account on several seats (spec 30 §6.2.13).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SameAccountPolicy {
    Allow,
    #[default]
    Warn,
    Deny,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Timings {
    /// Delay before the first automatic seat start.
    pub start_delay_ms: u32,
    /// Pause between seats (default 2 s).
    pub between_seats_ms: u32,
    /// Per-seat start time-out, 30..=120 s (default 60).
    pub seat_start_timeout_s: u32,
    /// Re-logon delay after sign-out with auto-logon (default 3 s).
    pub relogon_delay_ms: u32,
    /// Wait for audio services before starting bridges.
    pub audio_wait_timeout_s: u32,
    /// Monitor idle saver: switch an extra seat's monitors off after this many seconds
    /// without input on that seat, wake them on its next input (spec 40 §6.8). 0 = off
    /// (default). 0 or 60..=86400.
    pub monitor_idle_off_s: u32,
}

impl Default for Timings {
    fn default() -> Self {
        Self {
            start_delay_ms: 0,
            between_seats_ms: 2_000,
            seat_start_timeout_s: 60,
            relogon_delay_ms: 3_000,
            audio_wait_timeout_s: 60,
            monitor_idle_off_s: 0,
        }
    }
}

/// Boot safety (spec 20 §13.3, spec 80 §11.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Safety {
    /// After an unclean previous shutdown, do not auto-start extra seats.
    pub unclean_shutdown_guard: bool,
    /// After this many failed service starts in a row, stay in safe mode (seat 1 only).
    pub crash_loop_limit: u32,
}

impl Default for Safety {
    fn default() -> Self {
        Self { unclean_shutdown_guard: true, crash_loop_limit: 3 }
    }
}

/// Which backend implements each trait.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BackendSelection {
    pub session: SessionBackendKind,
    pub host_topology: HostTopology,
    pub display: DisplayBackendKind,
    pub input: InputBackendKind,
    pub audio: AudioBackendKind,
    pub gates: Vec<GateBackendKind>,
    /// [`EXPERIMENTAL_WARNING_VERSION`] the admin acknowledged; 0 = never. Experimental
    /// backends are refused unless this matches.
    pub experimental_ack: u32,
    /// Second RDP listener for seats without a stored password (`rds_loopback`,
    /// architecture §4.1 design (a)). Added 2026-11-13 (additive).
    pub loopback_listener: LoopbackListenerSettings,
}

/// Default port of the loopback-only, NLA-off listener for passwordless seats.
pub const DEFAULT_LOOPBACK_LISTENER_PORT: u16 = 33389;

/// The second, loopback-only RDP listener (`rds_loopback` only). It exists only while a
/// seat without a stored password exists and at least one account may use it; its
/// security descriptor allows only the seat accounts plus [`Self::extra_accounts`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LoopbackListenerSettings {
    /// Off: passwordless seats are refused with `nla_required` (the behaviour before).
    pub enabled: bool,
    /// TCP port (1024..=65535, not the RDP-Tcp port).
    pub port: u16,
    /// More accounts that may sign in at a logon-screen seat through this listener
    /// (`user` = local, `DOMAIN\user`). Seat accounts are always allowed.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub extra_accounts: Vec<String>,
}

impl Default for LoopbackListenerSettings {
    fn default() -> Self {
        Self { enabled: true, port: DEFAULT_LOOPBACK_LISTENER_PORT, extra_accounts: Vec::new() }
    }
}

impl Default for BackendSelection {
    fn default() -> Self {
        Self {
            session: SessionBackendKind::RdsLoopback,
            host_topology: HostTopology::SeatOneConsole,
            display: DisplayBackendKind::WindowPresenter,
            input: InputBackendKind::RawInputHost,
            audio: AudioBackendKind::RdpBridge,
            gates: vec![GateBackendKind::DeviceSecurity, GateBackendKind::SessionDriveLetters],
            experimental_ack: 0,
            loopback_listener: LoopbackListenerSettings::default(),
        }
    }
}

/// Input options (spec 50 §4.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct InputSettings {
    /// Ctrl + this set-1 scan code moves the pressing keyboard (and its mice) to the next
    /// seat. `None` = off. Default Ctrl+F12 (0x58).
    pub switch_hotkey_scan: Option<u16>,
    /// Power / sleep / wake from non-console seats (always act on the whole PC).
    pub allow_system_buttons: bool,
    /// HID `(usage_page, usage)` never filtered (default: lamp arrays, page 0x59).
    pub unfiltered_hid_usages: Vec<(u16, u16)>,
    /// Service-gap buffer per unit (spec 50 §4.1.5).
    pub gap_buffer_ms: u32,
    pub gap_buffer_packets: u32,
}

impl Default for InputSettings {
    fn default() -> Self {
        Self {
            switch_hotkey_scan: Some(0x58),
            allow_system_buttons: false,
            unfiltered_hid_usages: vec![(0x59, 0)],
            gap_buffer_ms: 1_000,
            gap_buffer_packets: 256,
        }
    }
}

/// Device-gate options (spec 70 §5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DeviceSettings {
    /// Administrators pass every seat's isolation.
    pub admins_bypass: bool,
    /// Class-level default descriptors (race-free, system change: off until signed off).
    pub class_default_descriptors: bool,
    /// Classes for `class_default_descriptors`; empty = [`DeviceClassTarget::DEFAULT`].
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub class_default_classes: Vec<crate::backend::gate::DeviceClassTarget>,
}

impl Default for DeviceSettings {
    fn default() -> Self {
        Self { admins_bypass: true, class_default_descriptors: false, class_default_classes: Vec::new() }
    }
}

impl DeviceSettings {
    /// The classes the gate should close now (empty when the feature is off).
    pub fn class_defaults(&self) -> Vec<crate::backend::gate::DeviceClassTarget> {
        if !self.class_default_descriptors {
            return Vec::new();
        }
        let mut v = if self.class_default_classes.is_empty() {
            crate::backend::gate::DeviceClassTarget::DEFAULT.to_vec()
        } else {
            self.class_default_classes.clone()
        };
        v.sort();
        v.dedup();
        v
    }
}

/// Optional, visible, reversible system tweaks (spec 80 §11.4, spec 90 §11 item 7).
/// Each one the service applies is journaled and reverted on disable / uninstall.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Tweaks {
    /// Machine-wide lock-screen image (PersonalizationCSP), our own generated image.
    pub lock_screen_image: bool,
    /// Hide fast user switching (documented policy, machine-wide).
    pub hide_fast_user_switching: bool,
    /// Exclude driver updates from quality updates (protects a working GPU setup).
    pub exclude_driver_updates: bool,
    /// Touchpad palm-check threshold per seat user (`AAPThreshold`).
    pub touchpad_palm_check: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GlobalSettings {
    /// Product switch: service state, not a rewrite of filter lists (spec 90 §11 item 5).
    pub enabled: bool,
    pub start_mode: StartMode,
    pub timings: Timings,
    pub safety: Safety,
    pub same_account: SameAccountPolicy,
    /// Only Administrators may change settings (enforced by the service on the pipe).
    pub admin_only: bool,
    /// Non-admin interactive users may read status and start / re-logon their own seat.
    pub users_may_control_own_seat: bool,
    pub backends: BackendSelection,
    pub input: InputSettings,
    pub devices: DeviceSettings,
    pub tweaks: Tweaks,
    /// Update check against GitHub Releases (spec 90 §11 item 10). Added 2026-11-13.
    pub updates: UpdateSettings,
    /// Program list for the per-seat source address rules (spec 10 §7.4). Added 2026-11-13.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ip_programs: Vec<IpProgram>,
    /// Log verbosity (spec 10 §7.10). Errors and warnings are always logged. Added 2026-11-13.
    pub log: LogSettings,
    /// Video-output assignment options (spec 10 §7.5). Added 2026-11-13.
    pub outputs: OutputSettings,
    /// Kiosk-seat options (spec 35 §5.1). Added 2026-11-13 (additive).
    pub kiosk: KioskSettings,
}

/// Log verbosity switches (spec 10 §7.10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LogSettings {
    pub info: bool,
    pub debug: bool,
    pub trace: bool,
}

impl Default for LogSettings {
    fn default() -> Self {
        Self { info: true, debug: false, trace: false }
    }
}

/// Machine-wide output binding options (spec 10 §7.5, spec 40).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct OutputSettings {
    /// Re-bind every monitor to its recorded output at seat start.
    pub force_assign_at_start: bool,
    /// Allow more than one output per monitor (clone).
    pub allow_clone: bool,
    /// Seat monitors follow the seat session's resolution (spec 40 §6.5): `Some(true)` also
    /// for monitors shown by a window on the console desktop (dynamic mode change, undone
    /// when the seat stops), `Some(false)` never, `None` = only monitors outside the
    /// console desktop. Added 2026-11-13 (additive).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub follow_session_mode: Option<bool>,
}

/// Update check (opt-in; both off by default: no network access unless the admin turns
/// it on). Installing always needs the administrator's confirmation.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateSettings {
    /// The service may ask the release feed (on demand from the manager).
    pub check: bool,
    /// Also check once a week by itself (needs `check`). Never downloads or installs.
    pub weekly: bool,
}

impl Default for GlobalSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            start_mode: StartMode::Manual,
            timings: Timings::default(),
            safety: Safety::default(),
            same_account: SameAccountPolicy::Warn,
            admin_only: false,
            users_may_control_own_seat: true,
            backends: BackendSelection::default(),
            input: InputSettings::default(),
            devices: DeviceSettings::default(),
            tweaks: Tweaks::default(),
            updates: UpdateSettings::default(),
            ip_programs: Vec::new(),
            log: LogSettings::default(),
            outputs: OutputSettings::default(),
            kiosk: KioskSettings::default(),
        }
    }
}

/// Upper bound of configurable seats (re-exported for validation messages).
pub const SEAT_LIMIT: u8 = MAX_SEATS;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seat_redirect_is_additive_and_off_by_default() {
        let s = SeatConfig::new(SeatNumber::new(2).unwrap());
        let j = serde_json::to_value(&s).unwrap();
        assert!(j.get("redirect").is_none(), "default policy is not written");
        let back: SeatConfig = serde_json::from_value(j).unwrap();
        assert_eq!(back.redirect, SeatRedirect::default());
        assert_eq!(back.redirect.clipboard.direction, ClipboardDirection::Off);
        let r: SeatRedirect = serde_json::from_str(r#"{"clipboard":{"direction":"both"},"drives":"read_only","smartcards":true}"#).unwrap();
        assert!(r.clipboard.direction.to_seat() && r.clipboard.direction.from_seat());
        assert!(r.clipboard.text && r.clipboard.html && r.clipboard.images && !r.clipboard.files);
        assert_eq!(r.drives, DriveRedirect::ReadOnly);
    }

    #[test]
    fn default_has_console_seat() {
        let c = Config::default();
        assert_eq!(c.seats.len(), 1);
        assert_eq!(c.autostart_mask(), SeatMask::CONSOLE);
    }

    #[test]
    fn roundtrip_and_migrate() {
        let mut c = Config::default();
        c.seats.push(SeatConfig::new(SeatNumber::new(2).unwrap()));
        c.seats[1].account = SeatAccount::Local { user: "ali".into() };
        let back = Config::from_json(&c.to_json()).unwrap();
        assert_eq!(back, c);
        // Minimal / old document: defaults fill in, seat 1 is added.
        let m = Config::from_json(r#"{"seats":[{"number":3,"name":"Ç"}]}"#).unwrap();
        assert_eq!(m.seats[0].number, SeatNumber::CONSOLE);
        assert_eq!(m.seats[1].name, "Ç");
        assert_eq!(m.settings.timings.between_seats_ms, 2000);
        assert!(matches!(Config::from_json(r#"{"schema_version":99}"#), Err(ConfigError::TooNew(99))));
    }

    #[test]
    fn account_json_shape() {
        let a = SeatAccount::Domain { domain: "EV".into(), user: "x".into() };
        assert_eq!(serde_json::to_string(&a).unwrap(), r#"{"kind":"domain","domain":"EV","user":"x"}"#);
        assert_eq!(serde_json::to_string(&SeatAccount::LogonScreen).unwrap(), r#"{"kind":"logon_screen"}"#);
    }
}
