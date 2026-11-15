// SPDX-License-Identifier: GPL-3.0-only
//! [`seat_display::ConsoleRunner`] for the session-0 service: runs
//! `pulsar-seat-host.exe --display-helper <args>` in the active console session and returns
//! its stdout (one JSON reply, parsed by `seat_display::RunnerConsole`).
//!
//! The helper runs with a **restricted SYSTEM token** moved into the console session
//! (`seat_session::win::launch::restricted_system_token_in_session`: SYSTEM SID, no
//! privileges), on `winsta0\default`, stdout on an anonymous pipe, killed after the
//! timeout. A restricted SYSTEM process cannot be opened by the console user, and it needs
//! no user to be signed in (the logon screen still has an input desktop).
//!
//! The same runner starts the **audio helper** (`pulsar-seat-service.exe --audio-helper
//! tone <endpoint> <ms>`, [`play_tone_in_console`]): the test tone is played from the
//! console session, not from session 0 (spec 60).
//!
//! Untested on hardware (needs SYSTEM + SeTcbPrivilege). HYPOTHESIS: CCD calls succeed
//! from a privilege-less SYSTEM token on the console's input desktop, and so does WASAPI
//! shared-mode playback on the console's endpoints.

use seat_core::backend::{BackendError, BackendResult, ErrorKind};
use seat_display::ConsoleRunner;
use std::io::Read;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::PathBuf;
use std::time::Duration;
use windows_sys::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::RemoteDesktop::WTSGetActiveConsoleSessionId;
use windows_sys::Win32::System::Threading::{CREATE_NO_WINDOW, TerminateProcess, WaitForSingleObject};

/// How long the reply pipe may stay open after the helper ended.
const REPLY_EOF_WAIT: Duration = Duration::from_secs(3);

/// The reader thread's output, or `None` when the pipe did not end within `limit`.
fn reply_within(rx: &std::sync::mpsc::Receiver<Vec<u8>>, limit: Duration) -> Option<Vec<u8>> {
    rx.recv_timeout(limit).ok()
}

/// Largest helper reply read (a CCD snapshot is a few KiB).
const MAX_REPLY: u64 = 4 * 1024 * 1024;

pub struct SessionConsoleRunner {
    exe: PathBuf,
}

fn install_dir() -> PathBuf {
    std::env::current_exe().ok().and_then(|p| p.parent().map(PathBuf::from)).unwrap_or_else(|| PathBuf::from("."))
}

impl SessionConsoleRunner {
    /// The display helper is the seat host next to the service executable. The caller
    /// (`seat_display::RunnerConsole`) passes `--display-helper` itself; a second copy here
    /// made every helper call fail with `unknown helper command` (VM test 2026-11-15).
    pub fn new() -> Self {
        Self { exe: install_dir().join(seat_core::consts::HOST_EXE) }
    }

    /// The audio helper is this service executable (`--audio-helper ...`).
    pub fn audio() -> Self {
        let exe = std::env::current_exe().unwrap_or_else(|_| install_dir().join("pulsar-seat-service.exe"));
        Self { exe }
    }

    /// Command line of one helper run: the exe and exactly the caller's arguments.
    fn command_line(&self, args: &[String]) -> BackendResult<String> {
        seat_session::launch::command_line(&self.exe, args).map_err(|e| os("helper_command_line", e))
    }
}

/// Whether this process runs in session 0 (the real service). A `--console` dev run lives
/// in the user's session and plays tones itself.
pub fn in_session_zero() -> bool {
    let mut sid = u32::MAX;
    // SAFETY: out pointer valid.
    let ok = unsafe {
        windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId(windows_sys::Win32::System::Threading::GetCurrentProcessId(), &mut sid)
    };
    ok != 0 && sid == 0
}

/// Play the test tone on `endpoint_id` from the console session through the audio helper.
pub fn play_tone_in_console(endpoint_id: &str, duration_ms: u32) -> BackendResult<()> {
    if !crate::cli::valid_endpoint_id(endpoint_id) {
        return Err(BackendError::new(ErrorKind::Policy, "invalid_endpoint", "endpoint id rejected"));
    }
    let args = vec!["--audio-helper".to_string(), "tone".into(), endpoint_id.to_string(), duration_ms.to_string()];
    let out = SessionConsoleRunner::audio().run_in_console(&args, Duration::from_millis(u64::from(duration_ms) + 10_000))?;
    parse_tone_reply(&out)
}

/// The helper's one JSON line.
pub fn parse_tone_reply(out: &[u8]) -> BackendResult<()> {
    let text = String::from_utf8_lossy(out);
    let v: serde_json::Value = text
        .lines()
        .rev()
        .find_map(|l| serde_json::from_str(l.trim()).ok())
        .ok_or_else(|| BackendError::new(ErrorKind::Os, "helper_bad_reply", "audio helper gave no reply"))?;
    if v.get("ok").and_then(|b| b.as_bool()) == Some(true) {
        return Ok(());
    }
    let reason = v.get("reason").and_then(|r| r.as_str()).unwrap_or("tone_failed").to_string();
    let detail = v.get("detail").and_then(|r| r.as_str()).unwrap_or("").to_string();
    Err(BackendError::new(ErrorKind::Os, &reason, detail))
}

impl Default for SessionConsoleRunner {
    fn default() -> Self {
        Self::new()
    }
}

fn os(reason: &str, e: impl std::fmt::Display) -> BackendError {
    BackendError::new(ErrorKind::Os, reason, e.to_string())
}

impl ConsoleRunner for SessionConsoleRunner {
    fn run_in_console(&self, args: &[String], timeout: Duration) -> BackendResult<Vec<u8>> {
        // SAFETY: trivially safe.
        let console = unsafe { WTSGetActiveConsoleSessionId() };
        if console == u32::MAX {
            return Err(BackendError::new(ErrorKind::NotReady, "no_console_session", "console session is switching"));
        }
        let token = seat_session::win::launch::restricted_system_token_in_session(console)
            .map_err(|e| os("helper_token_failed", e))?;
        let (mut r, mut w): (HANDLE, HANDLE) = (std::ptr::null_mut(), std::ptr::null_mut());
        // SAFETY: out pointers valid; handles are created non-inheritable (null attributes),
        // `spawn` makes the write end inheritable only around CreateProcess.
        if unsafe { CreatePipe(&mut r, &mut w, std::ptr::null(), 0) } == 0 {
            return Err(os("helper_pipe_failed", std::io::Error::last_os_error()));
        }
        // SAFETY: fresh handles we own from here on.
        let (reader, writer) = unsafe { (std::fs::File::from_raw_handle(r as _), std::fs::File::from_raw_handle(w as _)) };
        let cmd = self.command_line(args)?;
        let wh = writer.as_raw_handle() as HANDLE;
        let spawned = seat_session::win::process::spawn(&seat_session::win::process::SpawnSpec {
            token: Some(token.as_raw_handle() as HANDLE),
            command_line: &cmd,
            cwd: self.exe.parent(),
            env: None,
            desktop: Some(r"winsta0\default"),
            stdio: Some([std::ptr::null_mut(), wh, std::ptr::null_mut()]),
            // Console-subsystem helper on the user's desktop: no conhost window (stdout is
            // the pipe above).
            extra_flags: CREATE_NO_WINDOW,
            placement: None,
            // SYSTEM helper in the console user's session: SYSTEM-only process DACL.
            object_sddl: Some(seat_session::win::process::SYSTEM_ONLY_SDDL),
        })
        .map_err(|e| os("helper_launch_failed", e))?;
        spawned.resume().map_err(|e| os("helper_launch_failed", e))?;
        let process = spawned.into_process();
        // Our copy of the write end must close, or the read never sees EOF.
        drop(writer);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut out = Vec::new();
            let _ = reader.take(MAX_REPLY).read_to_end(&mut out);
            let _ = tx.send(out);
        });
        let ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX - 1);
        let ph = process.as_raw_handle() as HANDLE;
        // SAFETY: owned process handle.
        let done = unsafe { WaitForSingleObject(ph, ms) } == WAIT_OBJECT_0;
        if !done {
            // SAFETY: our child; ends it so the pipe closes and the reader returns.
            unsafe { TerminateProcess(ph, 1) };
        }
        // The reply ends when every write end of the pipe is closed; a process the helper
        // started could keep one open, so this wait is bounded too (VM test 2026-11-15).
        let out = reply_within(&rx, REPLY_EOF_WAIT);
        if !done {
            return Err(BackendError::new(ErrorKind::Timeout, "helper_timeout", "display helper did not answer in time"));
        }
        out.ok_or_else(|| BackendError::new(ErrorKind::Timeout, "helper_output_open", "display helper output was not closed"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `RunnerConsole` hands the runner already starts with `--display-helper`; the
    /// helper's `dispatch` takes the arguments after the first flag as the command.
    #[test]
    fn display_helper_flag_passed_once() {
        use seat_display::{Console, HELPER_FLAG, RunnerConsole};
        use std::sync::{Arc, Mutex};
        struct Rec(Mutex<Vec<String>>);
        impl ConsoleRunner for Rec {
            fn run_in_console(&self, args: &[String], _t: Duration) -> BackendResult<Vec<u8>> {
                *self.0.lock().unwrap() = args.to_vec();
                Err(BackendError::new(ErrorKind::Internal, "rec", "recorded"))
            }
        }
        let rec = Arc::new(Rec(Mutex::new(Vec::new())));
        let _ = RunnerConsole::new(rec.clone()).snapshot(true);
        let args = rec.0.lock().unwrap().clone();
        let cmd = SessionConsoleRunner::new().command_line(&args).unwrap();
        assert_eq!(cmd.matches(HELPER_FLAG).count(), 1, "{cmd}");
        assert!(cmd.contains(&format!("{HELPER_FLAG} snapshot")), "{cmd}");
    }

    /// A pipe kept open by another process (the identify overlay held the helper's stdout)
    /// must not stall the caller forever.
    #[test]
    fn reply_wait_is_bounded() {
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let t = std::time::Instant::now();
        assert_eq!(reply_within(&rx, Duration::from_millis(50)), None);
        assert!(t.elapsed() < Duration::from_secs(2));
        tx.send(b"{}".to_vec()).unwrap();
        assert_eq!(reply_within(&rx, Duration::from_millis(50)), Some(b"{}".to_vec()));
    }

    #[test]
    fn tone_reply() {
        assert!(parse_tone_reply(b"{\"ok\":true}\r\n").is_ok());
        let e = parse_tone_reply(br#"{"ok":false,"reason":"endpoint_not_active","detail":"x"}"#).unwrap_err();
        assert_eq!(e.reason, "endpoint_not_active");
        assert_eq!(parse_tone_reply(b"").unwrap_err().reason, "helper_bad_reply");
        assert!(play_tone_in_console("bad id", 100).is_err());
        // The test process is not the session-0 service.
        assert!(!in_session_zero());
    }
}
