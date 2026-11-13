// SPDX-License-Identifier: GPL-3.0-only
//! Kiosk shell launch plan (spec 35 §5.1 U3 / D′, desktops backend): what the
//! `desktops` backend hands to `pulsar-seat-host --kiosk-shell --seat N --plan <hex>` in the
//! console session. Pure; tested on any host.
//!
//! The plan travels hex-encoded on the command line (no file, no ACL, nothing secret in it;
//! the command line is visible to the console user, who runs the shell anyway). Devices and
//! the live monitor rectangles are not in the plan: the shell reads them from the service
//! (`list_devices`), because only the service knows the effective assignment.

use std::path::{Path, PathBuf};

use seat_core::SeatNumber;
use seat_core::config::{GlobalSettings, KioskShellApp, SeatConfig};
use seat_core::device::Rect;
use serde::{Deserialize, Serialize};

use crate::error::SysError;
use crate::launch::{DEFAULT_DESKTOP, LaunchSpec, TokenKind};

/// Command-line flag of the kiosk shell mode of the seat host binary.
pub const SHELL_FLAG: &str = "--kiosk-shell";
/// Longest hex plan on the command line (the whole line must stay below 32 767 characters).
pub const MAX_PLAN_HEX: usize = 24_000;

/// Everything the shell needs besides the service's device list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KioskPlan {
    pub seat: SeatNumber,
    #[serde(default)]
    pub apps: Vec<KioskShellApp>,
    #[serde(default)]
    pub idle_reset_minutes: u32,
    /// Seat 1 is the console user's normal desktop: never take the foreground.
    #[serde(default)]
    pub seat1_desktop: bool,
    /// Monitor rectangles known to the service at launch (fallback when the device list
    /// cannot be read). Empty = unknown.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub monitors: Vec<Rect>,
}

/// The seat runs as a kiosk shell seat: it has a shell config, and seat 1 only when it is
/// not kept as the normal desktop.
pub fn wants_shell(seat: SeatNumber, config: &SeatConfig, settings: &GlobalSettings) -> bool {
    config.kiosk_shell.is_some() && (!seat.is_console() || !settings.kiosk.seat1_desktop)
}

/// The plan of a seat (`None` when the seat is not a kiosk shell seat).
pub fn plan_for(seat: SeatNumber, config: &SeatConfig, settings: &GlobalSettings, monitors: Vec<Rect>) -> Option<KioskPlan> {
    if !wants_shell(seat, config, settings) {
        return None;
    }
    let k = config.kiosk_shell.as_ref()?;
    Some(KioskPlan {
        seat,
        // Trimmed, absolute commands only: the shell launches `command` as
        // `lpApplicationName` (no PATH search). Validation refuses the others; a config
        // that skipped validation loses those apps instead of starting the wrong file.
        apps: k
            .apps
            .iter()
            .map(|a| KioskShellApp { command: a.command.trim().to_string(), ..a.clone() })
            .filter(|a| seat_core::validate::is_absolute_windows_path(&a.command))
            .take(seat_core::config::MAX_KIOSK_APPS)
            .collect(),
        idle_reset_minutes: k.idle_reset_minutes.min(1440),
        seat1_desktop: settings.kiosk.seat1_desktop,
        monitors,
    })
}

pub fn encode_plan(plan: &KioskPlan) -> Result<String, SysError> {
    let json = serde_json::to_vec(plan).map_err(|e| SysError::Other(format!("kiosk plan: {e}")))?;
    let mut s = String::with_capacity(json.len() * 2);
    for b in json {
        s.push_str(&format!("{b:02x}"));
    }
    if s.len() > MAX_PLAN_HEX {
        return Err(SysError::Other(format!("kiosk plan too long ({} characters)", s.len())));
    }
    Ok(s)
}

pub fn decode_plan(hex: &str) -> Result<KioskPlan, String> {
    if hex.len() > MAX_PLAN_HEX || hex.len() % 2 != 0 {
        return Err("bad plan length".into());
    }
    let bytes = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(hex.get(i..i + 2).ok_or("bad hex")?, 16).map_err(|e| e.to_string()))
        .collect::<Result<Vec<u8>, String>>()?;
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}

/// The shell launch: seat host binary in the console session with the console user's token
/// (the shell starts the seat's apps as that user and posts messages to them; a SYSTEM or
/// Low-integrity shell could do neither).
pub fn shell_spec(install_dir: &Path, session_id: u32, plan: &KioskPlan) -> Result<LaunchSpec, SysError> {
    Ok(LaunchSpec {
        exe: install_dir.join(seat_core::consts::HOST_EXE),
        args: vec![SHELL_FLAG.into(), "--seat".into(), plan.seat.get().to_string(), "--plan".into(), encode_plan(plan)?],
        cwd: Some(install_dir.to_path_buf()),
        session_id,
        token: TokenKind::SessionUser,
        env_extra: Vec::new(),
        desktop: DEFAULT_DESKTOP.into(),
        stdout: None,
        placement: None,
    })
}

/// Command-line flag of the one-shot "release the console cursor" helper
/// (`pulsar-seat-host --kiosk-release`): `ClipCursor(NULL)` + `LSFW_UNLOCK` in the console
/// session, run after a focus-sink shell had to be killed. Added 2026-11-13.
pub const RELEASE_FLAG: &str = "--kiosk-release";

/// Name (without namespace prefix) of the manual-reset event a kiosk shell creates in its
/// own session and waits on: set = end gracefully (release the cursor clip, the foreground
/// lock and the focus mutex). The shell opens it as `Local\<name>`; the service in
/// session 0 sets it as `Session\<id>\<name>` ([`session_stop_event`]).
pub fn stop_event_name(seat: SeatNumber) -> String {
    format!("PulsarSeat.KioskStop.{}", seat.two_digits())
}

/// The stop event of a seat's shell as seen from another session.
pub fn session_stop_event(session_id: u32, seat: SeatNumber) -> String {
    format!(r"Session\{session_id}\{}", stop_event_name(seat))
}

/// The release helper launch (console user's token, console session).
pub fn release_spec(install_dir: &Path, session_id: u32) -> LaunchSpec {
    LaunchSpec {
        exe: install_dir.join(seat_core::consts::HOST_EXE),
        args: vec![RELEASE_FLAG.into()],
        cwd: Some(install_dir.to_path_buf()),
        session_id,
        token: TokenKind::SessionUser,
        env_extra: Vec::new(),
        desktop: DEFAULT_DESKTOP.into(),
        stdout: None,
        placement: None,
    }
}

/// Install dir for helper binaries: the configured one, else the running binary's folder.
pub fn install_dir_or_exe(configured: &Path) -> PathBuf {
    if !configured.as_os_str().is_empty() {
        return configured.to_path_buf();
    }
    std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use seat_core::config::KioskShell;

    fn seat(n: u8) -> SeatNumber {
        SeatNumber::new(n).unwrap()
    }
    fn app(t: &str) -> KioskShellApp {
        KioskShellApp { title: t.into(), command: r"C:\Program Files\Ç\a b.exe".into(), args: vec!["--x \"y\"".into()], autostart: true, monitor: 1 }
    }

    #[test]
    fn plan_roundtrip_through_the_command_line() {
        let mut c = SeatConfig::new(seat(2));
        c.kiosk_shell = Some(KioskShell { apps: vec![app("Tarayıcı")], idle_reset_minutes: 15 });
        let g = GlobalSettings::default();
        let p = plan_for(seat(2), &c, &g, vec![Rect { x: 1920, y: 0, width: 1280, height: 1024 }]).unwrap();
        assert!(p.seat1_desktop);
        let spec = shell_spec(Path::new(r"C:\PS"), 3, &p).unwrap();
        assert_eq!(spec.exe, Path::new(r"C:\PS").join(seat_core::consts::HOST_EXE));
        assert_eq!(&spec.args[..4], &[SHELL_FLAG.to_string(), "--seat".into(), "2".into(), "--plan".into()]);
        assert!(spec.args[4].bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(spec.token, TokenKind::SessionUser);
        assert_eq!(decode_plan(&spec.args[4]).unwrap(), p);
        assert!(decode_plan("0").is_err());
        assert!(decode_plan("zz").is_err());
        assert_eq!(stop_event_name(seat(2)), "PulsarSeat.KioskStop.02");
        assert_eq!(session_stop_event(3, seat(12)), r"Session\3\PulsarSeat.KioskStop.12");
        assert_eq!(release_spec(Path::new(r"C:\PS"), 1).args, vec![RELEASE_FLAG.to_string()]);
    }

    #[test]
    fn seat_one_is_a_shell_only_when_not_the_desktop() {
        let mut c = SeatConfig::new(SeatNumber::CONSOLE);
        let mut g = GlobalSettings::default();
        assert!(!wants_shell(SeatNumber::CONSOLE, &c, &g));
        c.kiosk_shell = Some(KioskShell::default());
        assert!(!wants_shell(SeatNumber::CONSOLE, &c, &g));
        g.kiosk.seat1_desktop = false;
        assert!(wants_shell(SeatNumber::CONSOLE, &c, &g));
        assert!(!plan_for(SeatNumber::CONSOLE, &c, &g, vec![]).unwrap().seat1_desktop);
        assert!(!wants_shell(seat(3), &SeatConfig::new(seat(3)), &g));
    }

    #[test]
    fn oversized_plans_are_refused_and_lists_capped() {
        let mut c = SeatConfig::new(seat(2));
        c.kiosk_shell = Some(KioskShell { apps: (0..40).map(|i| app(&format!("a{i}"))).collect(), idle_reset_minutes: 99_999 });
        let p = plan_for(seat(2), &c, &GlobalSettings::default(), vec![]).unwrap();
        assert_eq!(p.apps.len(), seat_core::config::MAX_KIOSK_APPS);
        assert_eq!(p.idle_reset_minutes, 1440);
        // Relative commands are dropped, padded ones trimmed.
        let mut rel = app("r");
        rel.command = "notepad.exe".into();
        let mut pad = app("p");
        pad.command = r"  C:\a.exe ".into();
        c.kiosk_shell = Some(KioskShell { apps: vec![rel, pad], idle_reset_minutes: 0 });
        let p2 = plan_for(seat(2), &c, &GlobalSettings::default(), vec![]).unwrap();
        assert_eq!(p2.apps.iter().map(|a| a.command.as_str()).collect::<Vec<_>>(), vec![r"C:\a.exe"]);
        let mut big = p.clone();
        big.apps[0].args = vec!["x".repeat(MAX_PLAN_HEX)];
        assert!(encode_plan(&big).is_err());
        assert!(decode_plan(&"00".repeat(MAX_PLAN_HEX)).is_err());
    }
}
