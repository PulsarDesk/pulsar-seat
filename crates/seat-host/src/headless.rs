// SPDX-License-Identifier: GPL-3.0-only
//! `pulsar-seat-host --headless-hold …`: headless loopback RDP client that creates and holds
//! one remote seat's Remote Desktop session (spec/remote 30 §5, Split contracts §1.2).
//! Replaces `pulsar-split-rdp-helper.exe` (merge-plan.md step 7).
//!
//! - argv: `seat_rdp::headless::proto::HelperArgs` after the subcommand (never a password).
//! - stdin: exactly one password frame, then EOF. With `--audio-capture` stdin stays open
//!   and carries microphone frames (`seat_core::remote::mic`), read on their own thread.
//! - stdout: JSON Lines `HelperEvent`s.
//! - exit 0 when the server ends the connection, non-zero on failure. No reconnect logic;
//!   the service restarts the hold.
//!
//! Started by the service as its own child (restricted, Low-integrity token, kill-on-close
//! job), so the seat host's containment checks for `--seat` hosts do not apply.

use std::io::Write;

use seat_rdp::headless::mic::HoldMic;
use seat_rdp::headless::proto::{HelperArgs, HelperEvent, HelperFailure, exit, read_password_frame, read_password_frame_open};

/// Writes one event line and flushes (the service reads line by line).
fn emit(ev: &HelperEvent) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{}", ev.to_line());
    let _ = out.flush();
}

fn finish(code: i32) -> i32 {
    emit(&HelperEvent::Exited { code });
    code
}

fn fail(code: HelperFailure, detail: impl Into<String>, exit_code: i32) -> i32 {
    emit(&HelperEvent::Failed { code, detail: detail.into() });
    finish(exit_code)
}

/// `Some(exit code)` when `argv` (without the program name) selects the headless mode.
pub fn dispatch(argv: &[String]) -> Option<i32> {
    let rest = HelperArgs::strip_subcommand(argv)?;
    Some(run(rest))
}

fn run(rest: &[String]) -> i32 {
    let args = match HelperArgs::parse(rest) {
        Ok(a) => a,
        Err(e) => return fail(HelperFailure::BadArgs, e.to_string(), exit::BAD_ARGS),
    };
    // The password arrives only on stdin; it is read once and wiped when dropped.
    let mut stdin = std::io::stdin().lock();
    let read = if args.audio_capture { read_password_frame_open(&mut stdin) } else { read_password_frame(&mut stdin) };
    drop(stdin);
    let password = match read {
        Ok(p) => p,
        Err(e) => return fail(HelperFailure::BadArgs, e.to_string(), exit::BAD_ARGS),
    };
    emit(&HelperEvent::Connecting);
    if args.dry_run {
        drop(password);
        return finish(exit::OK);
    }
    // Same `Stdin` (and its buffer) as the password read, so no mic byte is lost.
    let mic = args.audio_capture.then(|| {
        let mic = HoldMic::new();
        let feed = mic.clone();
        let _ = std::thread::Builder::new().name("hold-mic".into()).spawn(move || feed.pump(std::io::stdin()));
        mic
    });
    match seat_rdp::headless::client::run(&args, password, mic, &mut emit) {
        Ok(reason) => {
            emit(&HelperEvent::Disconnected { reason });
            finish(exit::OK)
        }
        Err((code, detail)) => fail(code, detail, exit::FAILED),
    }
}
