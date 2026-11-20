// SPDX-License-Identifier: GPL-3.0-only
//! Pure decisions of the seat host flow (testable on any host): command line, what to
//! report for an RDP control event, what to do with a `host_command`, and the window
//! rectangle for an assignment. The Windows driver of the flow is `host.rs`.

use crate::rdp::{RdpEvent, bounding};
use seat_core::SeatNumber;
use seat_core::Secret;
use seat_core::device::Rect;
use seat_proto::{HostAssignment, HostCommand, HostState};
use seat_session::autologon::{HostDisconnect, classify_disconnect};

/// Exit codes (the service restarts the host with back-off on any exit while the seat
/// should run; it reads the reason from the last `host_report`).
pub const EXIT_OK: i32 = 0;
pub const EXIT_USAGE: i32 = 2;
pub const EXIT_SERVICE: i32 = 3;
pub const EXIT_RDP: i32 = 4;

/// `--seat N --token-stdin` (the service's command line, seat-service `seats.rs`).
#[derive(Debug)]
pub struct HostArgs {
    pub seat: SeatNumber,
    pub token: Secret,
}

#[cfg(test)]
pub fn parse_args(args: &[String]) -> Option<HostArgs> {
    parse_args_with(args, || None)
}

/// Longest token accepted on stdin.
pub const MAX_TOKEN_BYTES: u32 = 512;

/// `--seat N` plus the one-time token: `--token-stdin` reads it from stdin (an anonymous
/// pipe the service writes, never visible in the command line), `--token T` is the older
/// form. `stdin` is only called for `--token-stdin`.
pub fn parse_args_with(args: &[String], stdin: impl FnOnce() -> Option<Secret>) -> Option<HostArgs> {
    let value = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1));
    let seat = SeatNumber::new(value("--seat")?.parse().ok()?)?;
    let token = if args.iter().any(|a| a == "--token-stdin") {
        stdin().filter(|t| !t.is_empty())?
    } else {
        Secret::new(value("--token").filter(|t| !t.is_empty())?.clone())
    };
    Some(HostArgs { seat, token })
}

/// One frame from the service: `u32` little-endian length + UTF-8 token (no newline).
pub fn read_token_frame(r: &mut impl std::io::Read) -> Option<Secret> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len).ok()?;
    let len = u32::from_le_bytes(len);
    if len == 0 || len > MAX_TOKEN_BYTES {
        return None;
    }
    let mut buf = zeroize::Zeroizing::new(vec![0u8; len as usize]);
    r.read_exact(&mut buf).ok()?;
    let s = std::str::from_utf8(&buf).ok()?;
    Some(Secret::new(s))
}

/// What the flow does after an RDP control event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Nothing,
    Report(HostState),
    /// Report, then close the window and exit with the code.
    ReportAndExit(HostState, i32),
}

fn disc_reason(d: HostDisconnect) -> &'static str {
    match d {
        HostDisconnect::LoggedOff => "logged_off",
        HostDisconnect::Disconnected => "disconnected",
        HostDisconnect::Replaced => "replaced",
        HostDisconnect::LogonFailed => "logon_failed",
        HostDisconnect::Denied => "rdp_denied",
        HostDisconnect::Transient => "transient",
        HostDisconnect::Local => "local",
    }
}

/// Report for an RDP event. A logon failure or a refusal is `failed` (the service then
/// shows the logon prompt / stops, `seats.rs`); every other end of the connection is
/// `disconnected` and the process exits so the service decides about a reconnect
/// (`seat_session::autologon::after_session_end`).
pub fn on_rdp_event(ev: &RdpEvent) -> Step {
    match ev {
        RdpEvent::Connecting => Step::Report(HostState::Connecting),
        RdpEvent::Connected => Step::Report(HostState::Connected),
        RdpEvent::LoginComplete => Step::Report(HostState::SignedIn { session_id: None }),
        RdpEvent::Disconnected { reason, extended } => {
            let d = classify_disconnect(*reason, *extended);
            let r = disc_reason(d).to_string();
            match d {
                HostDisconnect::LogonFailed | HostDisconnect::Denied => Step::ReportAndExit(HostState::Failed { reason: r }, EXIT_RDP),
                HostDisconnect::Local => Step::ReportAndExit(HostState::Disconnected { reason: r }, EXIT_OK),
                _ => Step::ReportAndExit(HostState::Disconnected { reason: r }, EXIT_RDP),
            }
        }
        RdpEvent::LogonError { .. } => Step::Nothing,
        RdpEvent::FatalError { code } => Step::ReportAndExit(HostState::Failed { reason: format!("rdp_fatal_{code}") }, EXIT_RDP),
        _ => Step::Nothing,
    }
}

/// The loopback endpoint of the connection as an IPv4 address + port (for the peer check).
pub fn rdp_endpoint(c: &seat_core::backend::session::RdpConnection) -> Option<(std::net::Ipv4Addr, u16)> {
    c.address.parse().ok().map(|a| (a, c.port))
}

/// What the event thread does with a `host_command` for this seat.
#[derive(Debug, Clone, PartialEq)]
pub enum CommandStep {
    /// Re-read `host_get_assignment`, move the window, reconfigure input.
    Reconfigure,
    /// Audio route changed: the ActiveX client plays on the host session's default
    /// endpoint, so there is nothing to rebind (logged gap).
    RebindAudio,
    SetVisible(bool),
    Exit,
}

pub fn on_command(c: &HostCommand) -> CommandStep {
    match c {
        HostCommand::Reconfigure => CommandStep::Reconfigure,
        HostCommand::RebindAudio { .. } => CommandStep::RebindAudio,
        HostCommand::Blank { on } => CommandStep::SetVisible(!on),
        HostCommand::Exit => CommandStep::Exit,
    }
}

/// Host window rectangle: the bounding box of the seat's outputs in console desktop
/// coordinates, else of the connection's monitors.
pub fn window_rect(a: &HostAssignment) -> Option<Rect> {
    // Clone outputs are not part of the RDP desktop (they mirror another output).
    let rects: Vec<Rect> = a.outputs.layout_outputs().map(|o| o.rect).collect();
    bounding(&rects).or_else(|| bounding(&a.connection.monitors))
}

#[cfg(test)]
mod tests {
    use super::*;
    use seat_core::backend::audio::AudioRoute;
    use seat_core::backend::display::{OutputPlacement, SeatOutputs};
    use seat_core::backend::input::InputBackendKind;
    use seat_core::backend::session::RdpConnection;
    use seat_core::device::Presentation;
    use seat_core::ids::DeviceKey;

    fn a(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn args() {
        let h = parse_args(&a(&["x.exe", "--seat", "3", "--token", "abc"])).unwrap();
        assert_eq!(h.seat.get(), 3);
        assert_eq!(h.token.expose(), "abc");
        assert!(parse_args(&a(&["x.exe", "--seat", "3"])).is_none());
        assert!(parse_args(&a(&["x.exe", "--seat", "0", "--token", "t"])).is_none());
        assert!(parse_args(&a(&["x.exe", "--seat", "2", "--token", ""])).is_none());
    }

    #[test]
    fn token_from_stdin() {
        let mut frame = 3u32.to_le_bytes().to_vec();
        frame.extend_from_slice(b"xyz");
        let h = parse_args_with(&a(&["x.exe", "--seat", "2", "--token-stdin"]), || read_token_frame(&mut frame.as_slice())).unwrap();
        assert_eq!(h.token.expose(), "xyz");
        // --token-stdin ignores a command-line token; empty, oversized and short frames fail.
        assert!(parse_args_with(&a(&["x.exe", "--seat", "2", "--token-stdin", "--token", "t"]), || None).is_none());
        assert!(read_token_frame(&mut 0u32.to_le_bytes().as_slice()).is_none());
        assert!(read_token_frame(&mut (MAX_TOKEN_BYTES + 1).to_le_bytes().as_slice()).is_none());
        assert!(read_token_frame(&mut [5u8, 0, 0, 0, b'a'].as_slice()).is_none());
    }

    #[test]
    fn rdp_events() {
        assert_eq!(on_rdp_event(&RdpEvent::LoginComplete), Step::Report(HostState::SignedIn { session_id: None }));
        assert_eq!(
            on_rdp_event(&RdpEvent::Disconnected { reason: 2055, extended: 0 }),
            Step::ReportAndExit(HostState::Failed { reason: "logon_failed".into() }, EXIT_RDP)
        );
        assert_eq!(
            on_rdp_event(&RdpEvent::Disconnected { reason: 1, extended: 0 }),
            Step::ReportAndExit(HostState::Disconnected { reason: "local".into() }, EXIT_OK)
        );
        assert!(matches!(on_rdp_event(&RdpEvent::Disconnected { reason: 3, extended: 0 }), Step::ReportAndExit(HostState::Disconnected { .. }, EXIT_RDP)));
        assert_eq!(on_rdp_event(&RdpEvent::Warning { code: 1 }), Step::Nothing);
    }

    #[test]
    fn commands() {
        assert_eq!(on_command(&HostCommand::Blank { on: true }), CommandStep::SetVisible(false));
        assert_eq!(on_command(&HostCommand::Exit), CommandStep::Exit);
    }

    #[test]
    fn rect_from_outputs() {
        let seat = SeatNumber::new(2).unwrap();
        let o = |x: i32| OutputPlacement {
            key: DeviceKey::new(format!("mon;inst=X{x};sub=t1")).unwrap(),
            rect: Rect { x, y: 0, width: 1920, height: 1080 },
            target_id: 1,
            adapter_luid: 1,
            mode: None,
            presentation: Presentation::Presenter,
            software_cursor: false,
            clone_of: None,
            follow_mode: false,
            hdr: false,
            vrr: false,
        };
        let asg = HostAssignment {
            seat,
            connection: RdpConnection {
                address: "127.0.0.2".into(),
                port: 3389,
                client_name: String::new(),
                user_hint: String::new(),
                domain_hint: String::new(),
                monitors: vec![],
                audio_playback: true,
                audio_capture: false,
                redirect_drives: false,
                redirect_smartcards: false,
            },
            outputs: SeatOutputs { seat, outputs: vec![o(1920), o(3840)] },
            audio: AudioRoute::default(),
            input_backend: InputBackendKind::RawInputHost,
            input_interfaces: vec![],
            listener_cert_sha1: vec![],
            input: Default::default(),
            redirection: Default::default(),
            frame_source: Default::default(),
        };
        assert_eq!(window_rect(&asg), Some(Rect { x: 1920, y: 0, width: 3840, height: 1080 }));
    }
}
