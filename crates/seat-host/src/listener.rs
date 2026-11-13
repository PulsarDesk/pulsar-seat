// SPDX-License-Identifier: GPL-3.0-only
//! `pulsar-seat-host --console-listener --token-stdin` (owner: the **input** package; spec 50
//! §4.1.8 / §4.1.9, spec 10 §3.1 / §9.2, spec 20 service handlers).
//!
//! Under `raw_input_host` the service in session 0 sees no input, and each seat host reads
//! only its own seat's units. This process closes the gap: the service starts it in the
//! console / host session (restricted SYSTEM token, SYSTEM-only process DACL, like the seat
//! host), it reads raw input of **every** unit as a sink (no `RIDEV_NOLEGACY`, nothing
//! suppressed) and reports to the service with `console_input_report`:
//!
//! - activity per unit (throttled) for identify-by-touch and the monitor idle saver;
//! - Ctrl+hotkey (move to the next seat) and the emergency combination;
//! - key make codes (only the wizard keys: F1..F12, top-row digits, Esc, Backspace, Enter,
//!   Shift, Alt; never every key, even if a reply sets the obsolete `wizard_all_keys`) and
//!   mouse clicks while the capture wizard runs.
//!
//! Outside the wizard no key code leaves the process (`seat_input::console_report`). Every reply carries
//! the wizard flag and the input settings; `stop` or a lost pipe ends the process (the
//! service starts a new one when it wants one). **Untested on hardware.**

use crate::pipe::PipeError;
use seat_core::Secret;
use seat_input::console_report::{ConsoleReporter, ConsoleSignal, batches};
use seat_proto::{
    CONSOLE_REPORT_HEARTBEAT_MS, ConsoleInputEvent, ConsoleListenerState, ErrorCode, MAX_CONSOLE_REPORT_EVENTS, RequestBody, ResponseBody,
};

/// Command-line flag.
pub const FLAG: &str = "--console-listener";
/// How often pending signals are sent (activity latency).
pub const FLUSH_MS: u64 = 100;
/// `hello` attempts while the service may not have recorded our PID yet.
pub const HELLO_ATTEMPTS: u32 = 10;
/// Pause between two `hello` attempts.
pub const HELLO_RETRY_MS: u64 = 200;

/// The token when this process was started as the console listener: `--token-stdin`
/// reads one frame from stdin (`flow::read_token_frame`, what the service sends),
/// `--token T` is the older form. `stdin` is only called for `--token-stdin`.
pub fn parse_with(args: &[String], stdin: impl FnOnce() -> Option<Secret>) -> Option<Option<Secret>> {
    args.iter().position(|a| a == FLAG)?;
    if args.iter().any(|a| a == "--token-stdin") {
        return Some(stdin().filter(|t| !t.is_empty()));
    }
    let token = args
        .iter()
        .position(|a| a == "--token")
        .and_then(|i| args.get(i + 1))
        .filter(|t| !t.is_empty())
        .map(|t| Secret::new(t.clone()));
    Some(token)
}

pub fn to_proto(s: ConsoleSignal) -> ConsoleInputEvent {
    match s {
        ConsoleSignal::Activity { unit, key_sc, click } => ConsoleInputEvent::Activity { interface: unit, key_sc, click },
        ConsoleSignal::Hotkey { unit } => ConsoleInputEvent::Hotkey { interface: unit },
        ConsoleSignal::Emergency { unit } => ConsoleInputEvent::Emergency { interface: unit },
    }
}

/// Send drained signals in batches of at most [`MAX_CONSOLE_REPORT_EVENTS`] (hotkeys and
/// emergencies first, nothing dropped), or one empty report when `heartbeat` is due and
/// nothing is pending. `Ok(None)`: nothing was due. `Ok(Some(state))`: the last reply; a
/// reply with `stop` ends the sending at once (the caller exits).
pub fn send(
    signals: Vec<ConsoleSignal>,
    heartbeat: bool,
    call: &mut dyn FnMut(RequestBody) -> Result<ResponseBody, PipeError>,
) -> Result<Option<ConsoleListenerState>, PipeError> {
    let mut parts: Vec<Vec<ConsoleInputEvent>> =
        batches(signals, MAX_CONSOLE_REPORT_EVENTS).into_iter().map(|b| b.into_iter().map(to_proto).collect()).collect();
    if parts.is_empty() {
        if !heartbeat {
            return Ok(None);
        }
        parts.push(Vec::new());
    }
    let mut last = None;
    for events in parts {
        match call(RequestBody::ConsoleInputReport { events })? {
            ResponseBody::ConsoleListenerState(st) => {
                let stop = st.stop;
                last = Some(st);
                if stop {
                    break;
                }
            }
            _ => return Err(PipeError::Unexpected),
        }
    }
    Ok(last)
}

/// Send what is pending (or a heartbeat when `heartbeat` is due) and apply the reply.
/// `Ok(None)`: nothing was due. `Ok(Some(state))`: the reply (the caller exits on `stop`).
pub fn flush(
    reporter: &mut ConsoleReporter,
    heartbeat: bool,
    call: &mut dyn FnMut(RequestBody) -> Result<ResponseBody, PipeError>,
) -> Result<Option<ConsoleListenerState>, PipeError> {
    let st = send(reporter.drain(), heartbeat, call)?;
    if let Some(st) = &st {
        apply(reporter, st);
    }
    Ok(st)
}

fn apply(reporter: &mut ConsoleReporter, st: &ConsoleListenerState) {
    reporter.set_settings(&st.input);
    reporter.set_wizard(st.wizard_running);
}

/// `hello` refused with `access_denied`: the service may not have stored our PID and token
/// yet (it records them only after its launch call returns), so that refusal is retried.
pub fn is_hello_race(e: &PipeError) -> bool {
    matches!(e, PipeError::Service(p) if p.code == ErrorCode::AccessDenied)
}

/// Run `attempt` (open the pipe + `hello`) up to [`HELLO_ATTEMPTS`] times while it fails
/// with [`is_hello_race`], calling `pause` in between. Other errors end it at once.
pub fn hello_with_retry<T>(mut attempt: impl FnMut() -> Result<T, PipeError>, mut pause: impl FnMut()) -> Result<T, PipeError> {
    let mut n = 1;
    loop {
        match attempt() {
            Err(e) if is_hello_race(&e) && n < HELLO_ATTEMPTS => {
                n += 1;
                pause();
            }
            r => return r,
        }
    }
}

/// Runs the listener; returns the process exit code.
#[cfg(windows)]
pub fn run(token: Option<Secret>) -> i32 {
    use crate::flow;
    use seat_input::win::raw::{RawNotice, RawReader, RawReaderConfig};
    use seat_core::{Clock, SystemClock};
    use seat_proto::ClientKind;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    let Some(token) = token else {
        eprintln!("pulsar-seat-host: --console-listener needs --token-stdin (started by the Pulsar Seat service)");
        return flow::EXIT_USAGE;
    };
    // A fresh connection per attempt: the service may close the pipe after a refusal.
    let attempt = || {
        let mut c = crate::pipe::open()?;
        c.hello(ClientKind::ConsoleListener, None, Some(token.clone()))?;
        Ok(c)
    };
    let mut conn = match hello_with_retry(attempt, || std::thread::sleep(Duration::from_millis(HELLO_RETRY_MS))) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("console listener: service connection / hello failed: {e}");
            return flow::EXIT_SERVICE;
        }
    };
    let clock = Arc::new(SystemClock);
    let reporter = Arc::new(Mutex::new(ConsoleReporter::new(&Default::default())));
    fn lock(r: &Mutex<ConsoleReporter>) -> std::sync::MutexGuard<'_, ConsoleReporter> {
        r.lock().unwrap_or_else(|e| e.into_inner())
    }
    // First report: settings + wizard state before any input is classified.
    match flush(&mut lock(&reporter), true, &mut |b| conn.call(b)) {
        Ok(Some(st)) if st.stop => return flow::EXIT_OK,
        Ok(_) => {}
        Err(e) => {
            eprintln!("console listener: first report failed: {e}");
            return flow::EXIT_SERVICE;
        }
    }
    let (r2, c2) = (reporter.clone(), clock.clone());
    let _reader = match RawReader::start(
        RawReaderConfig::listener(),
        Box::new(move |n| {
            let mut r = r2.lock().unwrap_or_else(|e| e.into_inner());
            match n {
                RawNotice::Event { unit, event } => r.on_raw(&unit, &event, c2.now_ms()),
                RawNotice::Removed { unit } => r.on_removed(&unit),
                RawNotice::Arrived { .. } => {}
            }
        }),
    ) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("console listener: raw input not started: {e}");
            return flow::EXIT_SERVICE;
        }
    };
    let mut last_sent = clock.now_ms();
    loop {
        std::thread::sleep(Duration::from_millis(FLUSH_MS));
        let now = clock.now_ms();
        let heartbeat = now.saturating_sub(last_sent) >= CONSOLE_REPORT_HEARTBEAT_MS;
        // Drain under the lock, send without it (the reader keeps queueing meanwhile).
        let batch = lock(&reporter).drain();
        match send(batch, heartbeat, &mut |b| conn.call(b)) {
            Ok(None) => {}
            Ok(Some(st)) => {
                last_sent = now;
                if st.stop {
                    return flow::EXIT_OK;
                }
                apply(&mut lock(&reporter), &st);
            }
            Err(e) => {
                eprintln!("console listener: report failed: {e}");
                return flow::EXIT_SERVICE;
            }
        }
    }
}

#[cfg(not(windows))]
pub fn run(_token: Option<Secret>) -> i32 {
    eprintln!("pulsar-seat-host: --console-listener needs Windows");
    1
}

#[cfg(test)]
mod tests {
    use super::*;
    use seat_input::convert::{RawUnitEvent, sc};

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_flag_and_token() {
        let none = || -> Option<Secret> { panic!("stdin read without --token-stdin") };
        let t = parse_with(&args(&["x", FLAG, "--token", "abc"]), none).unwrap().unwrap();
        assert_eq!(t.expose(), "abc");
        assert!(parse_with(&args(&["x", FLAG]), none).unwrap().is_none());
        assert!(parse_with(&args(&["x", "--seat", "2"]), none).is_none());
        // --token-stdin: the frame the service writes; a command-line token is ignored.
        let frame = [3u8, 0, 0, 0, b'x', b'y', b'z'];
        let t = parse_with(&args(&["x", FLAG, "--token-stdin"]), || crate::flow::read_token_frame(&mut frame.as_slice())).unwrap().unwrap();
        assert_eq!(t.expose(), "xyz");
        assert!(parse_with(&args(&["x", FLAG, "--token-stdin", "--token", "t"]), || None).unwrap().is_none());
    }

    #[test]
    fn flush_sends_signals_and_applies_the_reply() {
        let mut r = ConsoleReporter::new(&Default::default());
        let mut sent: Vec<RequestBody> = Vec::new();
        // An old service asking for every key gets the wizard keys only.
        let reply = ConsoleListenerState { wizard_running: true, input: Default::default(), stop: false, wizard_all_keys: true };
        let mut call = |b: RequestBody| -> Result<ResponseBody, PipeError> {
            sent.push(b);
            Ok(ResponseBody::ConsoleListenerState(reply.clone()))
        };
        // Nothing pending, no heartbeat due: nothing sent.
        assert!(flush(&mut r, false, &mut call).unwrap().is_none());
        // Heartbeat: an empty report; the reply turns wizard reporting on.
        assert!(flush(&mut r, true, &mut call).unwrap().is_some());
        assert_eq!(r.wizard(), seat_input::console_report::WizardKeys::Controls);
        r.on_raw("k1", &RawUnitEvent::Key { sc: 0x3c, flags: 0 }, 0);
        r.on_raw("k1", &RawUnitEvent::Key { sc: 0x1e, flags: 0 }, 1); // A: no code
        r.on_raw("k1", &RawUnitEvent::Key { sc: sc::LCTRL, flags: 0 }, 1);
        r.on_raw("k1", &RawUnitEvent::Key { sc: sc::F12, flags: 0 }, 2);
        flush(&mut r, false, &mut call).unwrap();
        drop(call);
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[0], RequestBody::ConsoleInputReport { events: vec![] });
        let RequestBody::ConsoleInputReport { events } = &sent[1] else { panic!() };
        assert_eq!(events[0], ConsoleInputEvent::Hotkey { interface: "k1".into() }, "hotkeys go first");
        assert_eq!(events[1], ConsoleInputEvent::Activity { interface: "k1".into(), key_sc: Some(0x3c), click: false });
    }

    fn state(stop: bool) -> ResponseBody {
        ResponseBody::ConsoleListenerState(ConsoleListenerState { wizard_running: false, input: Default::default(), stop, wizard_all_keys: false })
    }

    fn activity(n: usize) -> Vec<ConsoleSignal> {
        (0..n).map(|i| ConsoleSignal::Activity { unit: format!("m{i}"), key_sc: None, click: false }).collect()
    }

    #[test]
    fn send_splits_large_drains_and_stops_on_stop() {
        let mut many = activity(MAX_CONSOLE_REPORT_EVENTS * 2 + 1);
        many.push(ConsoleSignal::Emergency { unit: "k1".into() });
        let mut sizes = Vec::new();
        let mut first = None;
        let mut call = |b: RequestBody| -> Result<ResponseBody, PipeError> {
            let RequestBody::ConsoleInputReport { events } = b else { panic!() };
            first.get_or_insert_with(|| events[0].clone());
            sizes.push(events.len());
            Ok(state(false))
        };
        assert!(send(many, false, &mut call).unwrap().is_some());
        drop(call);
        assert_eq!(sizes, vec![MAX_CONSOLE_REPORT_EVENTS, MAX_CONSOLE_REPORT_EVENTS, 2], "nothing dropped");
        assert_eq!(first, Some(ConsoleInputEvent::Emergency { interface: "k1".into() }), "emergency goes first");

        // `stop` in a reply ends the sending.
        let mut n = 0;
        let mut stop_call = |_b: RequestBody| -> Result<ResponseBody, PipeError> {
            n += 1;
            Ok(state(true))
        };
        assert!(send(activity(MAX_CONSOLE_REPORT_EVENTS + 1), false, &mut stop_call).unwrap().unwrap().stop);
        assert_eq!(n, 1);
    }

    #[test]
    fn hello_is_retried_only_on_access_denied() {
        let denied = || PipeError::Service(seat_proto::ProtoError::new(ErrorCode::AccessDenied, "unknown console listener"));
        // Refused twice (the service has not stored our PID yet), then accepted.
        let (mut tries, mut pauses) = (0, 0);
        let r = hello_with_retry(
            || {
                tries += 1;
                if tries < 3 { Err(denied()) } else { Ok(tries) }
            },
            || pauses += 1,
        );
        assert_eq!(r.unwrap(), 3);
        assert_eq!(pauses, 2);
        // Always refused: gives up after HELLO_ATTEMPTS.
        let mut tries = 0;
        let r: Result<(), _> = hello_with_retry(
            || {
                tries += 1;
                Err(denied())
            },
            || {},
        );
        assert!(is_hello_race(&r.unwrap_err()));
        assert_eq!(tries, HELLO_ATTEMPTS);
        // Other errors are not retried.
        let mut tries = 0;
        let r: Result<(), _> = hello_with_retry(
            || {
                tries += 1;
                Err(PipeError::Closed)
            },
            || {},
        );
        assert!(matches!(r, Err(PipeError::Closed)));
        assert_eq!(tries, 1);
    }

    #[test]
    fn flush_rejects_other_replies() {
        let mut r = ConsoleReporter::new(&Default::default());
        let mut call = |_b: RequestBody| -> Result<ResponseBody, PipeError> { Ok(ResponseBody::Ack) };
        assert!(matches!(flush(&mut r, true, &mut call), Err(PipeError::Unexpected)));
    }
}
