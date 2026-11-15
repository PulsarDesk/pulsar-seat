// SPDX-License-Identifier: GPL-3.0-only
//! Display helper entry point: `pulsar-seat-host.exe --display-helper <command>`, started
//! by the service in the console session (see [`crate::console`]). Prints exactly one JSON
//! [`HelperReply`] line on stdout and exits.
//!
//! ```text
//! --display-helper snapshot [--display-core]
//! --display-helper exec <ConsoleCommand JSON>
//! --display-helper identify-show <ConsoleCommand JSON>   (internal: detached overlay child)
//! ```
//!
//! The seat host's `main` dispatches with:
//! `if let Some(code) = seat_display::helper::dispatch(&args) { std::process::exit(code) }`.

use crate::console::{Console, ConsoleCommand, HelperReply, HELPER_FLAG};
use crate::win::{overlay, LocalConsole};
use seat_core::backend::{BackendError, ErrorKind};
use std::io::Write;
use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};

const DETACHED_PROCESS: u32 = 0x0000_0008;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// `Some(exit code)` when `args` (including argv\[0\]) is a helper invocation.
pub fn dispatch(args: &[String]) -> Option<i32> {
    let pos = args.iter().position(|a| a == HELPER_FLAG)?;
    Some(run(&args[pos + 1..]))
}

fn print(reply: &HelperReply) -> i32 {
    let code = if matches!(reply, HelperReply::Error { .. }) { 1 } else { 0 };
    let line = serde_json::to_string(reply).unwrap_or_else(|_| r#"{"reply":"error"}"#.into());
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
    code
}

fn bad(msg: &str) -> HelperReply {
    HelperReply::Error { error: BackendError::new(ErrorKind::Internal, "display_helper_args", msg) }
}

fn parse_cmd(s: Option<&String>) -> Result<ConsoleCommand, HelperReply> {
    let s = s.ok_or_else(|| bad("missing command JSON"))?;
    serde_json::from_str(s).map_err(|e| bad(&format!("bad command JSON: {e}")))
}

/// Run one helper command (arguments after `--display-helper`).
pub fn run(args: &[String]) -> i32 {
    let console = LocalConsole;
    match args.first().map(String::as_str) {
        Some("snapshot") => {
            let dc = args.iter().any(|a| a == "--display-core");
            print(&match console.snapshot(dc) {
                Ok(snapshot) => HelperReply::Snapshot { snapshot },
                Err(error) => HelperReply::Error { error },
            })
        }
        Some("exec") => {
            let cmd = match parse_cmd(args.get(1)) {
                Ok(c) => c,
                Err(r) => return print(&r),
            };
            if let ConsoleCommand::ApplyTopology { request } = &cmd {
                return print(&match console.apply_topology(request) {
                    Ok(outcome) => HelperReply::Topology { outcome: Box::new(outcome) },
                    Err(error) => HelperReply::Error { error },
                });
            }
            let result = match &cmd {
                // Overlays must outlive this short-lived process: hand them to a child.
                ConsoleCommand::Identify { on: true, .. } => spawn_overlay_child(args.get(1).expect("checked")),
                other => console.exec(other),
            };
            print(&match result {
                Ok(()) => HelperReply::Ok,
                Err(error) => HelperReply::Error { error },
            })
        }
        Some("identify-show") => match parse_cmd(args.get(1)) {
            Ok(ConsoleCommand::Identify { items, seconds, .. }) => {
                crate::win::gdi::ensure_dpi_aware();
                i32::from(overlay::show_blocking(&items, seconds).is_err())
            }
            _ => 2,
        },
        _ => print(&bad("unknown helper command")),
    }
}

/// Clear `HANDLE_FLAG_INHERIT` on this process's standard handles.
fn no_inherit_std_handles() {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{HANDLE_FLAG_INHERIT, SetHandleInformation};
    for h in [std::io::stdin().as_raw_handle(), std::io::stdout().as_raw_handle(), std::io::stderr().as_raw_handle()] {
        if !h.is_null() {
            // SAFETY: our own standard handle; only its inherit flag changes.
            unsafe { SetHandleInformation(h as _, HANDLE_FLAG_INHERIT, 0) };
        }
    }
}

fn spawn_overlay_child(json: &str) -> seat_core::backend::BackendResult<()> {
    let exe = std::env::current_exe().map_err(|e| BackendError::new(ErrorKind::Os, "identify", e.to_string()))?;
    // Our stdout is the service's reply pipe and was inherited as an inheritable handle;
    // std passes every inheritable handle to the child, so the long-lived overlay kept the
    // pipe open and the service waited for its end forever (wizard_start never answered,
    // VM test 2026-11-15). The overlay must not inherit any of our standard handles.
    no_inherit_std_handles();
    Command::new(exe)
        .args([HELPER_FLAG, "identify-show", json])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW)
        .spawn()
        .map(|_| ())
        .map_err(|e| BackendError::new(ErrorKind::Os, "identify", format!("spawn overlay: {e}")))
}
