// SPDX-License-Identifier: GPL-3.0-only
//! `pulsar-split-rdp-helper.exe`: headless loopback RDP client that creates and holds one
//! seat's Remote Desktop session (spec 30 §5, contracts §1.2).
//!
//! - argv: `split_session::helper_proto::HelperArgs` (never a password).
//! - stdin: exactly one password frame, then EOF.
//! - stdout: JSON Lines `HelperEvent`s.
//! - exit 0 when the server ends the connection, non-zero on failure. No reconnect logic;
//!   the service restarts the helper.

mod rdp;

use split_session::helper_proto::{HelperArgs, HelperEvent, HelperFailure, exit, read_password_frame};
use std::io::Write;

/// Writes one event line and flushes (the service reads line by line).
pub(crate) fn emit(ev: &HelperEvent) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{}", ev.to_line());
    let _ = out.flush();
}

fn finish(code: i32) -> ! {
    emit(&HelperEvent::Exited { code });
    std::process::exit(code);
}

fn fail(code: HelperFailure, detail: impl Into<String>, exit_code: i32) -> ! {
    emit(&HelperEvent::Failed { code, detail: detail.into() });
    finish(exit_code);
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args = match HelperArgs::parse(&argv) {
        Ok(a) => a,
        Err(e) => fail(HelperFailure::BadArgs, e.to_string(), exit::BAD_ARGS),
    };
    // The password arrives only on stdin; it is read once and wiped when dropped.
    let password = match read_password_frame(&mut std::io::stdin().lock()) {
        Ok(p) => p,
        Err(e) => fail(HelperFailure::BadArgs, e.to_string(), exit::BAD_ARGS),
    };
    emit(&HelperEvent::Connecting);
    if args.dry_run {
        drop(password);
        finish(exit::OK);
    }
    match rdp::run(&args, password) {
        Ok(reason) => {
            emit(&HelperEvent::Disconnected { reason });
            finish(exit::OK);
        }
        Err((code, detail)) => fail(code, detail, exit::FAILED),
    }
}
