// SPDX-License-Identifier: GPL-3.0-only
//! Launching processes into a seat session (spec 30 §8). Pure helpers here; the Windows
//! implementation is `win::launch::WindowsLauncher`.

use crate::remote::{OwnedHandle, SessionError};
use std::path::PathBuf;

/// Stream server = `SystemInSession`; autostart and user apps = `SeatUser`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    /// The service's SYSTEM token with `TokenSessionId` set to the seat session.
    SystemInSession,
    /// `WTSQueryUserToken` for the seat session (retried while logon runs).
    SeatUser,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchSpec {
    pub exe: PathBuf,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub session_id: u32,
    pub token: TokenKind,
    pub env_extra: Vec<(String, String)>,
    /// Normally `winsta0\default`.
    pub desktop: String,
    /// Redirect stdout + stderr to this file (appended).
    pub stdout: Option<PathBuf>,
}

pub const DEFAULT_DESKTOP: &str = r"winsta0\default";

#[derive(Debug)]
pub struct LaunchedProcess {
    pub pid: u32,
    /// FILETIME of process creation; (pid, creation_time) identifies the process.
    pub creation_time: u64,
    pub handle: OwnedHandle,
}

pub trait SessionLauncher: Send + Sync {
    /// Creates (once) the per-seat job object `PulsarSeat.RemoteSeat.<seat-id>` (KILL_ON_JOB_CLOSE)
    /// and assigns the process to it. User token: WTSQueryUserToken (retried while logon
    /// runs), CreateEnvironmentBlock, profile loaded.
    fn launch(&self, seat_id: &str, spec: &LaunchSpec) -> Result<LaunchedProcess, SessionError>;
    /// Ends every process in the seat's job.
    fn kill_job(&self, seat_id: &str) -> Result<(), SessionError>;
}

/// Name of a remote seat's job object (`PulsarSeat.RemoteSeat.<seat-id>`; local seats use
/// [`crate::launch::job_name`], `PulsarSeat.Seat.NN`).
pub fn job_name(seat_id: &str) -> String {
    format!("PulsarSeat.RemoteSeat.{seat_id}")
}

/// Command-line quoting and environment blocks: the shared helpers (merge step 7).
pub use crate::launch::{parse_env_block, quote_arg};

/// [`crate::launch::command_line`] with the remote error type.
pub fn command_line(exe: &std::path::Path, args: &[String]) -> Result<String, SessionError> {
    Ok(crate::launch::command_line(exe, args)?)
}

/// [`crate::launch::env_block`] with the remote error type.
pub fn env_block(base: &[(String, String)], extra: &[(String, String)]) -> Result<Vec<u16>, SessionError> {
    Ok(crate::launch::env_block(base, extra)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_names() {
        assert_eq!(job_name("3f2a"), "PulsarSeat.RemoteSeat.3f2a");
        assert_ne!(job_name("03"), crate::launch::job_name(seat_core::SeatNumber::new(3).unwrap()), "never a local seat's job");
    }

    #[test]
    fn shared_helpers_keep_the_remote_error_type() {
        assert!(matches!(command_line(std::path::Path::new("a\"b"), &[]), Err(SessionError::Sys(_))));
        assert_eq!(env_block(&[], &[]).unwrap(), vec![0, 0]);
    }
}
