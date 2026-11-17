// SPDX-License-Identifier: GPL-3.0-only
//! Connection parameters of one seat and the IronRDP connector configuration built from
//! them (pure).
//!
//! Security choices (spec 30 §6.1.1 point 3, §6.2 points 6–7):
//!
//! - **With a stored password**: CredSSP / NLA only (`enable_credssp`, no TLS-only
//!   fallback, no standard RDP security). The password reaches the server only inside
//!   CredSSP, after the pinned TLS certificate was verified.
//! - **Without a password** (the seat shows its logon screen): TLS only, the user name as
//!   a hint, no secret on the wire. A listener that requires NLA refuses this
//!   (`nla_required`), which the host reports.
//! - Our own client name `PSEAT-NN` goes into the core data, so the service can match the
//!   seat session by client name (the ActiveX control could not set it).

use crate::gfx::GraphicsMode;
use crate::pin::CertPin;
use seat_core::backend::session::HostRedirection;
use ironrdp_connector::{Config, Credentials, DesktopSize};
use ironrdp_pdu::gcc::{self, ClientMonitorData, ClientMonitorExtendedData, ExtendedMonitorInfo, MonitorFlags, MonitorOrientation};
use ironrdp_pdu::rdp::capability_sets::{MajorPlatformType, RailSupportLevel};
use ironrdp_pdu::rdp::client_info::{PerformanceFlags, TimezoneInfo};
use std::net::IpAddr;

/// Reported to the server as the client build (Windows 11 24H2 class).
pub const CLIENT_BUILD: u32 = 26100;
/// Default keyboard layout when the host knows none (en-US); the session's own layout
/// list still applies inside the session.
pub const DEFAULT_KEYBOARD_LAYOUT: u32 = 0x0409;
/// Desktop and monitor size limits of the RDP core data / display control channel.
pub const MIN_SIZE: u32 = 200;
pub const MAX_SIZE: u32 = 8192;

/// One monitor of the session desktop, primary at (0, 0); right / bottom inclusive
/// (`TS_MONITOR_DEF`). Same shape as `seat_display::RdpMonitor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonitorDef {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
    pub primary: bool,
    /// Desktop scale factor in percent (100..=500).
    pub scale: u32,
    /// Degrees: 0, 90, 180, 270.
    pub orientation: u16,
}

impl MonitorDef {
    pub fn width(&self) -> u32 {
        (self.right - self.left + 1).max(1) as u32
    }
    pub fn height(&self) -> u32 {
        (self.bottom - self.top + 1).max(1) as u32
    }
}

/// Fallback when the host has no layout: one 1920×1080 monitor.
pub fn default_monitor() -> MonitorDef {
    MonitorDef { left: 0, top: 0, right: 1919, bottom: 1079, primary: true, scale: 100, orientation: 0 }
}

/// Rectangle (x, y, width, height) inside the session framebuffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FbRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Session desktop geometry: bounding box of the monitors and each monitor's place in
/// the framebuffer (the server's framebuffer origin is the bounding box's top-left).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Geometry {
    pub width: u32,
    pub height: u32,
    pub origin: (i32, i32),
    pub outputs: Vec<FbRect>,
}

pub fn geometry(monitors: &[MonitorDef]) -> Geometry {
    let mons: Vec<MonitorDef> = if monitors.is_empty() { vec![default_monitor()] } else { monitors.to_vec() };
    let l = mons.iter().map(|m| m.left).min().unwrap_or(0);
    let t = mons.iter().map(|m| m.top).min().unwrap_or(0);
    let r = mons.iter().map(|m| m.right).max().unwrap_or(1919);
    let b = mons.iter().map(|m| m.bottom).max().unwrap_or(1079);
    let width = ((r - l + 1).max(1) as u32).clamp(MIN_SIZE, MAX_SIZE);
    let height = ((b - t + 1).max(1) as u32).clamp(MIN_SIZE, MAX_SIZE);
    let outputs = mons
        .iter()
        .map(|m| FbRect { x: (m.left - l) as u32, y: (m.top - t) as u32, width: m.width(), height: m.height() })
        .collect();
    Geometry { width, height, origin: (l, t), outputs }
}

/// Per-seat device redirection (spec 70; from the seat policy / `RdpConnection` and the
/// service's resolved [`HostRedirection`]).
///
/// The booleans are what the connection contract asks for (`RdpConnection.redirect_*`);
/// `channels` is what the engine actually joins (`crate::channels`): clipboard per its
/// direction and format classes, the listed drives, smart cards on the listed readers.
/// Printers and ports are never redirected (the seat session sees the PC's queues and
/// ports directly, spec 70 §3.8).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RedirectPolicy {
    pub clipboard: bool,
    pub drives: bool,
    pub smartcards: bool,
    pub printers: bool,
    pub ports: bool,
    pub channels: HostRedirection,
}

impl RedirectPolicy {
    /// Clipboard channel joined (some direction on).
    pub fn clipboard_on(&self) -> bool {
        let d = self.channels.clipboard.direction;
        d.to_seat() || d.from_seat()
    }
    /// Device redirection channel (`rdpdr`) joined: drives or smart cards to offer.
    pub fn rdpdr_on(&self) -> bool {
        !self.channels.drives.is_empty() || self.smartcards_on()
    }
    pub fn smartcards_on(&self) -> bool {
        self.channels.smartcards && !self.channels.readers.is_empty()
    }

    /// Notes for what is asked and not offered (the safe direction: never more than the
    /// policy allows): flags without resolved channel data, printers and ports.
    pub fn unsupported_notes(&self) -> Vec<&'static str> {
        let mut v = Vec::new();
        if self.clipboard && !self.clipboard_on() {
            v.push("clipboard_redirect_unavailable");
        }
        if self.drives && self.channels.drives.is_empty() {
            v.push("drive_redirect_unavailable");
        }
        if self.smartcards && !self.smartcards_on() {
            v.push("smartcard_redirect_unavailable");
        }
        if self.printers {
            v.push("printer_redirect_unavailable");
        }
        if self.ports {
            v.push("port_redirect_unavailable");
        }
        v
    }
}

/// Everything one connection needs, except the password.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionParams {
    pub address: IpAddr,
    pub port: u16,
    /// `PSEAT-NN` (at most 15 characters are sent).
    pub client_name: String,
    pub user: String,
    /// Empty = local account (the computer name is used for NTLM).
    pub domain: String,
    pub monitors: Vec<MonitorDef>,
    pub keyboard_layout: u32,
    pub audio_playback: bool,
    pub audio_capture: bool,
    /// Offer relative mouse input (RDP 10.12; used while the session hides the pointer).
    pub relative_mouse: bool,
    pub redirect: RedirectPolicy,
    pub pin: CertPin,
    /// Graphics pipeline (MS-RDPEGFX) offer; bitmap updates when off or not taken.
    pub graphics: GraphicsMode,
}

/// The NTLM domain: the given one, else the computer name.
pub fn effective_domain(domain: &str) -> Option<String> {
    if !domain.is_empty() {
        return Some(domain.to_owned());
    }
    std::env::var("COMPUTERNAME").ok().filter(|d| !d.is_empty())
}

fn orientation(deg: u16) -> MonitorOrientation {
    match deg {
        90 => MonitorOrientation::Portrait,
        180 => MonitorOrientation::LandscapeFlipped,
        270 => MonitorOrientation::PortraitFlipped,
        _ => MonitorOrientation::Landscape,
    }
}

/// Client Monitor Data + Extended Data for a multi-monitor seat (`None` for one monitor:
/// the core data's desktop size and scale describe it).
pub fn monitor_blocks(monitors: &[MonitorDef]) -> Option<(ClientMonitorData, ClientMonitorExtendedData)> {
    if monitors.len() < 2 {
        return None;
    }
    let mut mons: Vec<MonitorDef> = monitors.to_vec();
    // Exactly one primary, at (0, 0) (MS-RDPBCGR 2.2.1.3.6.1).
    if !mons.iter().any(|m| m.primary) {
        mons[0].primary = true;
    }
    let data = ClientMonitorData {
        monitors: mons
            .iter()
            .map(|m| gcc::Monitor {
                left: m.left,
                top: m.top,
                right: m.right,
                bottom: m.bottom,
                flags: if m.primary { MonitorFlags::PRIMARY } else { MonitorFlags::empty() },
            })
            .collect(),
    };
    let ext = ClientMonitorExtendedData {
        extended_monitors_info: mons
            .iter()
            .map(|m| ExtendedMonitorInfo {
                physical_width: 0,
                physical_height: 0,
                orientation: orientation(m.orientation),
                desktop_scale_factor: m.scale.clamp(100, 500),
                device_scale_factor: 100,
            })
            .collect(),
    };
    Some((data, ext))
}

/// Client name as the core data carries it (15 characters).
pub fn wire_client_name(name: &str) -> String {
    name.chars().take(15).collect()
}

/// `clientDir` of the client info PDU: the seat host's own path (empty when unknown).
pub fn own_client_dir() -> String {
    std::env::current_exe().ok().map(|p| p.display().to_string()).unwrap_or_default()
}

/// IronRDP configuration. `password: None` = logon-screen mode (TLS only, no secret).
pub fn build_config(p: &SessionParams, password: Option<&str>) -> Config {
    let g = geometry(&p.monitors);
    let primary = p.monitors.iter().find(|m| m.primary).or(p.monitors.first()).copied().unwrap_or_else(default_monitor);
    let (monitor_layout, monitor_extended_layout) = match monitor_blocks(&p.monitors) {
        Some((d, e)) => (Some(d), Some(e)),
        None => (None, None),
    };
    let nla = password.is_some();
    Config {
        desktop_size: DesktopSize { width: g.width as u16, height: g.height as u16 },
        monitor_layout,
        desktop_scale_factor: primary.scale.clamp(100, 500),
        desktop_physical_size: None,
        desktop_orientation: Some(orientation(primary.orientation)),
        device_scale_factor: Some(100),
        monitor_extended_layout,
        relative_mouse_input: p.relative_mouse,
        enable_tls: !nla,
        enable_credssp: nla,
        enable_standard_rdp_security: false,
        credentials: Credentials::UsernamePassword { username: p.user.clone(), password: password.unwrap_or_default().to_owned() },
        domain: effective_domain(&p.domain),
        client_build: CLIENT_BUILD,
        client_name: wire_client_name(&p.client_name),
        keyboard_type: gcc::KeyboardType::IBM_ENHANCED,
        keyboard_subtype: 0,
        keyboard_functional_keys_count: 12,
        keyboard_layout: if p.keyboard_layout == 0 { DEFAULT_KEYBOARD_LAYOUT } else { p.keyboard_layout },
        connection_type: gcc::ConnectionType::Lan,
        ime_file_name: String::new(),
        // IronRDP default bitmap settings (32 bpp, its own codec list).
        bitmap: None,
        dig_product_id: String::new(),
        // Our own executable (informational for the server; nothing depends on it).
        client_dir: own_client_dir(),
        alternate_shell: String::new(),
        work_dir: String::new(),
        remote_application_mode: false,
        rail_support_level: RailSupportLevel::empty(),
        platform: MajorPlatformType::WINDOWS,
        hardware_id: None,
        request_data: None,
        autologon: false,
        // false would set INFO_NOAUDIOPLAYBACK; the seat host plays it on the seat's endpoint.
        enable_audio_playback: p.audio_playback,
        enable_audio_capture: p.audio_capture,
        // Loopback: full visual quality.
        performance_flags: PerformanceFlags::ENABLE_FONT_SMOOTHING | PerformanceFlags::ENABLE_DESKTOP_COMPOSITION,
        license_cache: None,
        timezone_info: TimezoneInfo::default(),
        compression_type: None,
        // The pointer is drawn into the framebuffer by IronRDP (software), so every
        // presenter shows it without a cursor plane.
        enable_server_pointer: true,
        pointer_software_rendering: true,
        multitransport_flags: None,
        // RNS_UD_CS_SUPPORT_DYNVC_GFX_PROTOCOL: the session registers the EGFX channel
        // exactly when this is set (`session::connect`).
        support_dyn_vc_gfx_protocol: p.graphics.offers_egfx(),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn mon(left: i32, top: i32, w: i32, h: i32, primary: bool) -> MonitorDef {
        MonitorDef { left, top, right: left + w - 1, bottom: top + h - 1, primary, scale: 100, orientation: 0 }
    }

    pub(crate) fn params() -> SessionParams {
        SessionParams {
            address: "127.0.0.2".parse().unwrap(),
            port: 3389,
            client_name: "PSEAT-02".into(),
            user: "ali".into(),
            domain: "PC".into(),
            monitors: vec![mon(0, 0, 1920, 1080, true), mon(-1280, 0, 1280, 1024, false)],
            keyboard_layout: 0x041F,
            audio_playback: true,
            audio_capture: true,
            relative_mouse: true,
            redirect: RedirectPolicy::default(),
            pin: CertPin::default(),
            graphics: GraphicsMode::Egfx,
        }
    }

    #[test]
    fn geometry_spans_and_places_outputs() {
        let g = geometry(&params().monitors);
        assert_eq!((g.width, g.height), (3200, 1080));
        assert_eq!(g.origin, (-1280, 0));
        assert_eq!(g.outputs[0], FbRect { x: 1280, y: 0, width: 1920, height: 1080 });
        assert_eq!(g.outputs[1], FbRect { x: 0, y: 0, width: 1280, height: 1024 });
        let d = geometry(&[]);
        assert_eq!((d.width, d.height, d.outputs.len()), (1920, 1080, 1));
    }

    #[test]
    fn nla_with_password_tls_only_without() {
        let p = params();
        let c = build_config(&p, Some("pw"));
        assert!(c.enable_credssp && !c.enable_tls && !c.enable_standard_rdp_security);
        assert!(matches!(&c.credentials, Credentials::UsernamePassword { username, password } if username == "ali" && password == "pw"));
        let c = build_config(&p, None);
        assert!(!c.enable_credssp && c.enable_tls && !c.enable_standard_rdp_security);
        assert!(matches!(&c.credentials, Credentials::UsernamePassword { password, .. } if password.is_empty()));
    }

    #[test]
    fn seat_values_reach_the_config() {
        let c = build_config(&params(), Some("x"));
        assert_eq!(c.client_name, "PSEAT-02");
        assert_eq!(c.keyboard_layout, 0x041F);
        assert_eq!((c.desktop_size.width, c.desktop_size.height), (3200, 1080));
        assert!(c.enable_audio_playback && c.enable_audio_capture && c.relative_mouse_input);
        assert_eq!(c.domain.as_deref(), Some("PC"));
        let m = c.monitor_layout.unwrap();
        assert_eq!(m.monitors.len(), 2);
        assert!(m.monitors[0].flags.contains(MonitorFlags::PRIMARY));
        assert_eq!(m.monitors[1].left, -1280);
        assert_eq!(c.monitor_extended_layout.unwrap().extended_monitors_info.len(), 2);
        assert_eq!(wire_client_name("PSEAT-02-VERY-LONG-NAME").len(), 15);
        assert!(!c.client_dir.to_ascii_lowercase().ends_with(".dll"), "our own executable");
    }

    #[test]
    fn single_monitor_has_no_monitor_blocks() {
        let mut p = params();
        p.monitors.truncate(1);
        p.monitors[0].scale = 150;
        let c = build_config(&p, None);
        assert!(c.monitor_layout.is_none() && c.monitor_extended_layout.is_none());
        assert_eq!(c.desktop_scale_factor, 150);
        // A layout without a primary gets one.
        let (d, _) = monitor_blocks(&[mon(0, 0, 800, 600, false), mon(800, 0, 800, 600, false)]).unwrap();
        assert!(d.monitors[0].flags.contains(MonitorFlags::PRIMARY));
    }

    #[test]
    fn egfx_flag_follows_the_mode() {
        let mut p = params();
        assert!(build_config(&p, None).support_dyn_vc_gfx_protocol);
        p.graphics = GraphicsMode::EgfxNoH264;
        assert!(build_config(&p, None).support_dyn_vc_gfx_protocol);
        p.graphics = GraphicsMode::Bitmap;
        assert!(!build_config(&p, None).support_dyn_vc_gfx_protocol);
    }

    #[test]
    fn redirect_notes() {
        let r = RedirectPolicy { clipboard: true, drives: true, ..Default::default() };
        assert_eq!(r.unsupported_notes(), vec!["clipboard_redirect_unavailable", "drive_redirect_unavailable"]);
        assert!(RedirectPolicy::default().unsupported_notes().is_empty());
        let mut on = RedirectPolicy { clipboard: true, drives: true, smartcards: true, ..Default::default() };
        on.channels.clipboard.direction = seat_core::config::ClipboardDirection::Both;
        on.channels.drives.push(seat_core::backend::session::RedirectDrive { root: "E:\\".into(), name: "E".into(), read_only: true, volume: String::new(), acl: false });
        on.channels.smartcards = true;
        on.channels.readers.push(seat_core::backend::session::SmartCardReaderRef { instance_id: "X".into(), name: "R".into(), name_unique: false });
        assert!(on.clipboard_on() && on.rdpdr_on() && on.smartcards_on());
        assert!(on.unsupported_notes().is_empty());
        on.printers = true;
        assert_eq!(on.unsupported_notes(), vec!["printer_redirect_unavailable"]);
    }
}
