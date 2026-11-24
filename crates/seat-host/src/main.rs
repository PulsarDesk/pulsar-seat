// SPDX-License-Identifier: GPL-3.0-only
//! Pulsar Seat host (`pulsar-seat-host.exe`): shows one extra seat on its monitors.
//!
//! Owner: the **host** package. Spec: `spec/local/00-architecture.md` §5.3 P9, §5.2.1,
//! `spec/local/30-sessions.md` §6.1.1, `spec/local/40-display.md` §6.3, `spec/local/50-input.md` §4.1.4,
//! `spec/local/60-audio.md` §8.3. Contract: `docs/agents/architecture.md` (§ Seat host).
//!
//! Flow (`host.rs`, decisions in `flow.rs`; **untested on hardware**):
//!
//! ```text
//! pulsar-seat-host --seat <N> --token-stdin   (started by the service only; the one-time
//!                                               token arrives once on stdin)
//!   1. connect to seat_proto::PIPE_NAME (server verified as the service), hello
//!      {client: seat_host, seat, token}
//!   2. host_get_assignment -> RDP connection, SeatOutputs, AudioRoute, input interfaces
//!   3. host_take_credentials (once) -> Remote Desktop ActiveX control over the seat's
//!      monitors (rdp::win::RdpClient, listener check before Connect), password wiped
//!      right after it is handed to the control
//!   4. presenter guards over the seat's monitors (display::SeatDisplay::guard_only);
//!      raw input (seat_input::win::raw) or `input_packets` events -> RdpInputSink over
//!      the control (input::HostInput)
//!   5. host_report on every connection state; host_command: reconfigure (re-read the
//!      assignment, move the window, new unit set), blank (hide / show), exit; the end of
//!      the RDP connection or a lost pipe closes the window and the process exits (the
//!      service restarts it with back-off, seat_session::autologon::after_session_end)
//! ```
//!
//! Headless hold (`--headless-hold …`, `headless.rs`): remote seats only. Holds one seat's
//! RDS session without frames (password over stdin, JSON Lines on stdout); replaces
//! Pulsar Split's `pulsar-split-rdp-helper.exe`.
//!
//! RDP backends (`--rdp-backend` / `PULSAR_SEAT_RDP_BACKEND`):
//!
//! - `ironrdp` (default, `ironrdp.rs` over `seat_rdp`): pinned listener certificate,
//!   forced NLA, frames into the presenter per seat monitor, per-seat audio + microphone
//!   through `seat_media::bridge::AudioBridge`, touch / pen and relative mouse, in-host
//!   reconnect for transient failures (passwordless seats only). The service passes the
//!   token on stdin (`--token-stdin`, an inherited anonymous pipe); `--token T` is still
//!   accepted for manual runs.
//! - `activex` (fallback, `host.rs` above): the Remote Desktop ActiveX control. It plays
//!   audio on the host session's default endpoint, has no certificate pinning, touch or
//!   relative mouse. Never chosen automatically.
//!
//! GUI subsystem: the service starts this binary into user sessions (console listener,
//! kiosk shell, display helper, seat host); as a console binary each got a visible conhost
//! window there (VM test 2026-11-15). Token, helper replies and the headless hold use
//! inherited pipe handles, which work without a console.

#![cfg_attr(not(test), windows_subsystem = "windows")]

// `--headless-hold …`: remote seat session hold (no frames), started by the service.
mod headless;

// Start-up containment checks and the `--desktop-acl` helper (host-containment).
mod containment;
mod flow;
// `log` records of the libraries to stderr (host-NN.log), see the module.
mod hostlog;
#[cfg(windows)]
mod host;
#[cfg_attr(not(windows), allow(dead_code))]
mod ironrdp;
// In-session capture reader (`local.frame_source = session_capture`, OSMS-parity G1).
#[cfg_attr(not(windows), allow(dead_code))]
mod capture;
mod pipe;

// Input forwarding (input package).
#[cfg_attr(not(windows), allow(dead_code))]
mod input;

// Display helper + presenter (display package). `SeatDisplay::new` / `layout_for` serve the
// IronRDP client (frames); the ActiveX client uses `guard_only`.
mod display;

// Loopback RDP client: Remote Desktop ActiveX control host (session package).
#[cfg_attr(not(windows), allow(dead_code))]
mod rdp;

// `--desktop-keeper --seat N` for the desktops backend's per-seat mode (session package).
mod keeper;

// `--kiosk-shell --seat N --plan HEX`: concurrent kiosk seat shell for the desktops backend
// (spec 35 §5.1 U3, kiosk-shell package).
#[cfg_attr(not(windows), allow(dead_code))]
mod kiosk;

// `--console-listener --token-stdin`: console raw-input reporter under raw_input_host (input
// package).
#[cfg_attr(not(windows), allow(dead_code))]
mod listener;

fn main() {
    // `--display-helper …`: console-side CCD / identify / DDC-CI work for the service.
    if let Some(code) = display::dispatch_helper() {
        std::process::exit(code);
    }
    // After the display helper (its stdout carries the reply): presenter / display records
    // reach host-NN.log (gaming PC E2E bug 3).
    hostlog::install();
    let args: Vec<String> = std::env::args().collect();
    // `--headless-hold …`: the service's per-remote-seat session hold (JSON Lines on stdout).
    if let Some(code) = headless::dispatch(args.get(1..).unwrap_or_default()) {
        std::process::exit(code);
    }
    // `--desktop-acl grant|revoke --sid S`: window-station / desktop ACEs for a contained
    // host, run by the service in the target session.
    if let Some(code) = containment::dispatch_acl(&args) {
        std::process::exit(code);
    }
    // Started by the service: refuse a token that is not contained, report a host account
    // that cannot reach its desktop or the pipe (the service then falls back).
    if containment::service_launched(&args)
        && let Err(code) = containment::startup_check()
    {
        std::process::exit(code);
    }
    if let Some(token) = listener::parse_with(&args, || flow::read_token_frame(&mut std::io::stdin().lock())) {
        std::process::exit(listener::run(token));
    }
    if args.iter().any(|a| a == seat_session::kiosk::RELEASE_FLAG) {
        // One shot after a killed focus-sink shell: free the console's cursor clip and
        // foreground lock (spec 35 §5.1 U3, kiosk-shell package).
        #[cfg(windows)]
        kiosk::win::release_console_cursor();
        std::process::exit(0);
    }
    // `--hotkey-guard --seat N`: the kiosk shell's hotkey guard helper (no raw input).
    if let Some(parsed) = kiosk::guard::parse_guard_args(&args) {
        let code = match parsed {
            #[cfg(windows)]
            Ok(seat) => kiosk::guard_win::run(seat, kiosk::guard::parse_guard_mode(&args)),
            #[cfg(not(windows))]
            Ok(_) => 1,
            Err(e) => {
                eprintln!("pulsar-seat-host: --hotkey-guard: {e}");
                2
            }
        };
        std::process::exit(code);
    }
    if let Some(parsed) = kiosk::parse_args(&args) {
        let code = match parsed {
            #[cfg(windows)]
            Ok(a) => kiosk::win::run(a),
            #[cfg(not(windows))]
            Ok(_) => 1,
            Err(e) => {
                eprintln!("pulsar-seat-host: --kiosk-shell: {e}");
                2
            }
        };
        std::process::exit(code);
    }
    if args.iter().any(|a| a == "--desktop-keeper") {
        let code = match keeper::seat_arg(&args) {
            Some(seat) => keeper::run(seat),
            None => {
                eprintln!("pulsar-seat-host: --desktop-keeper needs --seat N");
                2
            }
        };
        std::process::exit(code);
    }
    let Some(host_args) = flow::parse_args_with(&args, || flow::read_token_frame(&mut std::io::stdin().lock())) else {
        eprintln!("pulsar-seat-host: usage: --seat N --token-stdin | --token T (started by the Pulsar Seat service)");
        std::process::exit(flow::EXIT_USAGE);
    };
    let backend = ironrdp::Backend::choose(&args, std::env::var("PULSAR_SEAT_RDP_BACKEND").ok().as_deref());
    #[cfg(windows)]
    std::process::exit(match backend {
        ironrdp::Backend::IronRdp => {
            let _ = host::RDP_CLIENT.set("ironrdp");
            ironrdp::run(host_args)
        }
        ironrdp::Backend::ActiveX => {
            let _ = host::RDP_CLIENT.set("activex");
            host::run(host_args)
        }
    });

    #[cfg(not(windows))]
    {
        drop((host_args, backend));
        eprintln!("pulsar-seat-host: needs Windows");
        std::process::exit(1);
    }
}
