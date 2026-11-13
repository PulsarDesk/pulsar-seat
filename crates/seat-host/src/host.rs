// SPDX-License-Identifier: GPL-3.0-only
//! Windows driver of the seat host flow (see `main.rs` for the steps; decisions in
//! `flow.rs`). **Untested on hardware**: it needs the service, an RDS loopback listener and a
//! seat user, none of which exist on the dev box.
//!
//! Threads:
//!
//! - main (STA): the RDP ActiveX control window and its message loop. Control events are
//!   reported over the request connection.
//! - events: second connection, `subscribe {host}`; input packets go to [`HostInput`],
//!   `host_command`s move / hide / close the window through [`RdpChannel`].
//!
//! The window closes on `exit`, on the end of the RDP connection and when the service pipe
//! is lost; the process then exits and the service decides about a restart.

use crate::flow::{self, CommandStep, HostArgs, Step};
use crate::input::HostInput;
use crate::pipe::{self, Conn, PipeError};
use crate::rdp::ClientPolicy;
use crate::rdp::win::{RdpChannel, RdpClient, run_message_loop};
use seat_proto::messages::{ClientKind, RequestBody, ResponseBody, Topic};
use seat_proto::{Event, HostAssignment, HostState};
use std::fs::File;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex};

pub(crate) type Shared<T> = Arc<Mutex<T>>;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Open the pipe and `hello` as the seat host. The service records our PID and token only
/// after its launch call returns, so an early `access_denied` ("unknown seat host") is the
/// startup race and is retried, like the console listener does
/// ([`crate::listener::hello_with_retry`]).
pub(crate) fn connect(args: &HostArgs) -> Result<Conn<File>, PipeError> {
    use crate::listener::{HELLO_RETRY_MS, hello_with_retry};
    hello_with_retry(
        || {
            let mut c = pipe::open()?;
            c.hello(ClientKind::SeatHost, Some(args.seat), Some(args.token.clone()))?;
            Ok(c)
        },
        || std::thread::sleep(std::time::Duration::from_millis(HELLO_RETRY_MS)),
    )
}

pub(crate) fn assignment(ctl: &Shared<Conn<File>>, args: &HostArgs) -> Result<HostAssignment, PipeError> {
    match lock(ctl).call(RequestBody::HostGetAssignment { seat: args.seat })? {
        ResponseBody::HostAssignment(a) => Ok(a),
        _ => Err(PipeError::Unexpected),
    }
}

/// RDP client of this process (`"ironrdp"` / `"activex"`), set once in `main`; sent with
/// every `host_report` so the service knows which session-binding rules apply.
pub(crate) static RDP_CLIENT: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();

pub(crate) fn report(ctl: &Shared<Conn<File>>, args: &HostArgs, state: HostState) {
    let rdp_client = RDP_CLIENT.get().map(|s| (*s).to_string());
    if let Err(e) = lock(ctl).call(RequestBody::HostReport { seat: args.seat, state, rdp_client }) {

        eprintln!("seat host: host_report failed: {e}");
    }
}

/// Runs the whole host; returns the process exit code.
pub fn run(args: HostArgs) -> i32 {
    let ctl = match connect(&args) {
        Ok(c) => Arc::new(Mutex::new(c)),
        Err(e) => {
            eprintln!("seat host: service connection failed: {e}");
            return flow::EXIT_SERVICE;
        }
    };
    let asg = match assignment(&ctl, &args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("seat host: host_get_assignment failed: {e}");
            return flow::EXIT_SERVICE;
        }
    };
    let creds = match lock(&ctl).call(RequestBody::HostTakeCredentials { seat: args.seat }) {
        Ok(ResponseBody::Credentials(c)) => c,
        Ok(_) => return flow::EXIT_SERVICE,
        Err(e) => {
            eprintln!("seat host: host_take_credentials failed: {e}");
            return flow::EXIT_SERVICE;
        }
    };
    let Some(rect) = flow::window_rect(&asg) else {
        report(&ctl, &args, HostState::Failed { reason: "display".into() });
        return flow::EXIT_RDP;
    };
    // Cursor clip + foreign-window guard over the seat's monitors (the control draws its
    // own window, so no presenter windows). Kept alive until the end of `run`.
    let _guard = match crate::display::SeatDisplay::guard_only(&asg.outputs) {
        Ok(g) => Some(g),
        Err(e) => {
            eprintln!("seat host: display guard not started: {e}");
            None
        }
    };

    let exit = Arc::new(AtomicI32::new(flow::EXIT_OK));
    // The control's event callback runs on this (STA) thread; it only needs the channel to
    // close the window, which exists once the client does.
    let channel_slot: Shared<Option<RdpChannel>> = Arc::new(Mutex::new(None));
    let peer = flow::rdp_endpoint(&asg.connection);
    let on_event = {
        let (ctl, exit, slot) = (ctl.clone(), exit.clone(), channel_slot.clone());
        let a = HostArgs { seat: args.seat, token: args.token.clone() };
        Box::new(move |ev: crate::rdp::RdpEvent| {
            // Second check after the TCP connect: our connection must end at TermService
            // (closes the TOCTOU window of the pre-Connect listener snapshot).
            if ev == crate::rdp::RdpEvent::Connected {
                let checked = peer.ok_or_else(|| "endpoint is not an IPv4 address".to_string()).and_then(|(addr, port)| {
                    seat_session::win::listener::verify_rdp_peer(addr, port, std::process::id()).map_err(|e| e.to_string())
                });
                if let Err(why) = checked {
                    eprintln!("seat host: {why}; disconnecting");
                    report(&ctl, &a, HostState::Failed { reason: "rdp_peer_untrusted".into() });
                    exit.store(flow::EXIT_RDP, Ordering::SeqCst);
                    if let Some(c) = lock(&slot).as_ref() {
                        c.close();
                    }
                    return;
                }
            }
            match flow::on_rdp_event(&ev) {
            Step::Nothing => {}
            Step::Report(s) => report(&ctl, &a, s),
            Step::ReportAndExit(s, code) => {
                eprintln!("seat host: RDP connection ended: {ev:?}");
                report(&ctl, &a, s);
                exit.store(code, Ordering::SeqCst);
                if let Some(c) = lock(&slot).as_ref() {
                    c.close();
                }
            }
            }
        })
    };
    let mut client = match RdpClient::create(rect, on_event) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("seat host: RDP control: {e}");
            report(&ctl, &args, HostState::Failed { reason: e.reason.clone() });
            return flow::EXIT_RDP;
        }
    };
    let channel = client.channel();
    *lock(&channel_slot) = Some(channel.clone());
    // The password goes to the control and is wiped with `creds` right after.
    // The control cannot filter redirection: only what the seat policy allows unfiltered.
    let (policy, refused) = ClientPolicy::from_redirection(&asg.redirection);
    for reason in refused {
        eprintln!("seat host: redirection refused: {reason} (ActiveX backend)");
        let _ = lock(&ctl).call(RequestBody::HostRedirectRefusal {
            seat: args.seat,
            channel: "activex".into(),
            reason: reason.into(),
            detail: String::new(),
            suppressed: 0,
        });
    }
    let configured = client.configure(&asg.connection, &creds.user, &creds.domain, &policy, creds.password.as_ref());
    drop(creds);
    if let Err(e) = configured.and_then(|_| client.connect()) {
        eprintln!("seat host: connect failed: {e}");
        report(&ctl, &args, HostState::Failed { reason: e.reason.clone() });
        return flow::EXIT_RDP;
    }

    // Input: raw reader (raw_input_host) or service packets (kmdf_filter) into the control.
    let sink = Box::new(seat_input::rdp::RdpInputSink::new(channel.clone()));
    let input = match HostInput::start(&asg, &asg.input, sink) {
        Ok(i) => Some(Arc::new(Mutex::new(i))),
        Err(e) => {
            eprintln!("seat host: input forwarding not started: {e}");
            None
        }
    };

    // Event thread: input packets and host commands.
    {
        let (ctl, channel, input, exit) = (ctl.clone(), channel.clone(), input.clone(), exit.clone());
        let a = HostArgs { seat: args.seat, token: args.token.clone() };
        std::thread::spawn(move || {
            let lost = |why: String| {
                eprintln!("seat host: service events lost: {why}");
                if let Some(i) = &input {
                    let _ = lock(i).service_lost();
                }
                exit.store(flow::EXIT_SERVICE, Ordering::SeqCst);
                channel.close();
            };
            let mut ev = match connect(&a).and_then(|mut c| c.subscribe(vec![Topic::Host]).map(|_| c)) {
                Ok(c) => c,
                Err(e) => return lost(e.to_string()),
            };
            loop {
                let frame = match ev.next_event() {
                    Ok(Some(f)) => f,
                    Ok(None) => return lost("pipe closed".into()),
                    Err(e) => return lost(e.to_string()),
                };
                match &frame.event {
                    Event::InputPackets { .. } => {
                        if let Some(i) = &input
                            && let Err(e) = lock(i).on_event(&frame.event)
                        {
                            eprintln!("seat host: input forward failed: {e}");
                        }
                    }
                    Event::HostCommand { seat, command } if *seat == a.seat => match flow::on_command(command) {
                        CommandStep::Exit => {
                            if let Some(i) = &input {
                                let _ = lock(i).stop();
                            }
                            channel.close();
                            return;
                        }
                        CommandStep::SetVisible(v) => channel.set_visible(v),
                        CommandStep::RebindAudio => {
                            eprintln!("seat host: audio route changed; the RDP control plays on the host session's default endpoint")
                        }
                        CommandStep::Reconfigure => match assignment(&ctl, &a) {
                            Ok(n) => {
                                if let Some(r) = flow::window_rect(&n) {
                                    channel.place(r);
                                }
                                if let Some(i) = &input
                                    && let Err(e) = lock(i).reconfigure(&n)
                                {
                                    eprintln!("seat host: input reconfigure failed: {e}");
                                }
                            }
                            Err(e) => eprintln!("seat host: reconfigure failed: {e}"),
                        },
                    },
                    _ => {}
                }
            }
        });
    }

    run_message_loop();
    if let Some(i) = &input {
        let _ = lock(i).stop();
    }
    drop(client);
    exit.load(Ordering::SeqCst)
}
