// SPDX-License-Identifier: GPL-3.0-only
//! Console access for the service-side binding.
//!
//! CCD (`QueryDisplayConfig`, `SetDisplayConfig`, `DisplayConfigGetDeviceInfo`), GDI monitor
//! handles, DDC/CI and overlay windows only work from a process that owns the console
//! session's input desktop. The service runs in session 0, where `QueryDisplayConfig`
//! fails with `ERROR_ACCESS_DENIED` (documented on Microsoft Learn). So the binding talks
//! to the console through a [`Console`]:
//!
//! - [`LocalConsole`](crate::win::LocalConsole): direct calls, for processes that already
//!   run in an interactive session (seat host, manager tools, tests on a dev box);
//! - [`RunnerConsole`]: runs `pulsar-seat-host.exe --display-helper <command>` in the
//!   console session through a [`ConsoleRunner`] the service provides (it owns
//!   launch-into-session, `seat-session`), and parses one JSON [`HelperReply`] from its
//!   stdout. The helper side is [`crate::helper::run`].

use crate::model::Snapshot;
use crate::topology::{TopologyOutcome, TopologyRequest};
use seat_core::backend::{BackendError, BackendResult, ErrorKind};
use seat_core::device::Rect;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

/// One identify overlay: where and what to draw.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentifyItem {
    pub rect: Rect,
    pub label: String,
}

/// A side-effecting console command (helper `exec`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum ConsoleCommand {
    /// Show (`on`) or hide every identify overlay. Overlays close by themselves after
    /// `seconds`.
    Identify { items: Vec<IdentifyItem>, on: bool, seconds: u32 },
    /// DDC/CI power (VCP 0xD6) of the monitor shown as `gdi_name`.
    Power { gdi_name: String, on: bool },
    /// Wizard step: extend the desktop to every connected monitor (`SDC_TOPOLOGY_EXTEND`).
    ExtendDesktop,
    /// Take targets out of / give them back to the console desktop (monitor
    /// specialization + `SetDisplayConfig`, validated, rolled back on failure). Replies
    /// with [`HelperReply::Topology`].
    ApplyTopology { request: TopologyRequest },
}

/// Helper stdout (one JSON line).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum HelperReply {
    Snapshot { snapshot: Snapshot },
    Topology { outcome: Box<TopologyOutcome> },
    Ok,
    Error { error: BackendError },
}

/// What the binding needs from the console session.
pub trait Console: Send + Sync {
    /// CCD snapshot; `display_core` also probes `Windows.Devices.Display.Core` targets.
    fn snapshot(&self, display_core: bool) -> BackendResult<Snapshot>;
    fn exec(&self, cmd: &ConsoleCommand) -> BackendResult<()>;
    /// Run a topology change in the console session.
    fn apply_topology(&self, request: &TopologyRequest) -> BackendResult<TopologyOutcome> {
        let _ = request;
        Err(BackendError::unsupported("apply_topology"))
    }
}

/// Runs the seat host binary in the active console session and returns its stdout.
/// Implemented by the service (seat-session launch helpers): `CreateProcessAsUser` with
/// the console user's (or the dedicated host account's) token, stdout on a pipe, the
/// process killed after `timeout`.
pub trait ConsoleRunner: Send + Sync {
    fn run_in_console(&self, args: &[String], timeout: Duration) -> BackendResult<Vec<u8>>;
}

/// Argument that switches `pulsar-seat-host.exe` into helper mode.
pub const HELPER_FLAG: &str = "--display-helper";

/// Helper time limit for snapshots and short commands.
pub const HELPER_TIMEOUT: Duration = Duration::from_secs(10);
/// Helper time limit for `apply_topology`: per-target specialization, a settle sleep,
/// validate + apply (a mode set per monitor can take seconds), re-query and a possible
/// rollback apply. Killing the helper half-way would leave a half-applied topology.
pub const TOPOLOGY_TIMEOUT: Duration = Duration::from_secs(45);

/// Console reached through the helper process.
pub struct RunnerConsole {
    runner: Arc<dyn ConsoleRunner>,
    timeout: Duration,
    topology_timeout: Duration,
}

impl RunnerConsole {
    pub fn new(runner: Arc<dyn ConsoleRunner>) -> Self {
        Self { runner, timeout: HELPER_TIMEOUT, topology_timeout: TOPOLOGY_TIMEOUT }
    }

    fn call(&self, args: Vec<String>) -> BackendResult<HelperReply> {
        self.call_with_timeout(args, self.timeout)
    }

    fn call_with_timeout(&self, args: Vec<String>, timeout: Duration) -> BackendResult<HelperReply> {
        let mut full = vec![HELPER_FLAG.to_string()];
        full.extend(args);
        let out = self.runner.run_in_console(&full, timeout)?;
        parse_reply(&out)
    }
}

/// Parse the last non-empty JSON line of the helper output.
pub fn parse_reply(out: &[u8]) -> BackendResult<HelperReply> {
    let text = String::from_utf8_lossy(out);
    let line = text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("");
    serde_json::from_str(line.trim()).map_err(|e| {
        BackendError::new(ErrorKind::Internal, "display_helper_output", format!("bad helper output: {e}"))
    })
}

impl Console for RunnerConsole {
    fn snapshot(&self, display_core: bool) -> BackendResult<Snapshot> {
        let mut args = vec!["snapshot".to_string()];
        if display_core {
            args.push("--display-core".into());
        }
        match self.call(args)? {
            HelperReply::Snapshot { snapshot } => Ok(snapshot),
            HelperReply::Error { error } => Err(error),
            _ => Err(BackendError::new(ErrorKind::Internal, "display_helper_output", "expected a snapshot")),
        }
    }

    fn apply_topology(&self, request: &TopologyRequest) -> BackendResult<TopologyOutcome> {
        let cmd = ConsoleCommand::ApplyTopology { request: request.clone() };
        let json = serde_json::to_string(&cmd).map_err(|e| BackendError::new(ErrorKind::Internal, "serialize", e.to_string()))?;
        match self.call_with_timeout(vec!["exec".into(), json], self.topology_timeout)? {
            HelperReply::Topology { outcome } => Ok(*outcome),
            HelperReply::Error { error } => Err(error),
            _ => Err(BackendError::new(ErrorKind::Internal, "display_helper_output", "expected a topology outcome")),
        }
    }

    fn exec(&self, cmd: &ConsoleCommand) -> BackendResult<()> {
        let json = serde_json::to_string(cmd)
            .map_err(|e| BackendError::new(ErrorKind::Internal, "serialize", e.to_string()))?;
        match self.call(vec!["exec".into(), json])? {
            HelperReply::Ok => Ok(()),
            HelperReply::Error { error } => Err(error),
            _ => Err(BackendError::new(ErrorKind::Internal, "display_helper_output", "expected ok")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct Fake(Mutex<Vec<Vec<String>>>, Vec<u8>);
    impl ConsoleRunner for Fake {
        fn run_in_console(&self, args: &[String], t: Duration) -> BackendResult<Vec<u8>> {
            let mut a = args.to_vec();
            a.push(format!("timeout={}", t.as_secs()));
            self.0.lock().unwrap().push(a);
            Ok(self.1.clone())
        }
    }

    #[test]
    fn runner_round_trip() {
        let reply = serde_json::to_vec(&HelperReply::Snapshot { snapshot: Snapshot::default() }).unwrap();
        let mut out = b"log line\n".to_vec();
        out.extend(reply);
        let f = Arc::new(Fake(Mutex::new(vec![]), out));
        let c = RunnerConsole::new(f.clone());
        assert_eq!(c.snapshot(true).unwrap(), Snapshot::default());
        assert_eq!(f.0.lock().unwrap()[0], vec!["--display-helper", "snapshot", "--display-core", "timeout=10"]);
        assert!(c.exec(&ConsoleCommand::ExtendDesktop).is_err());

        let err = serde_json::to_vec(&HelperReply::Error { error: BackendError::unsupported("x") }).unwrap();
        let c = RunnerConsole::new(Arc::new(Fake(Mutex::new(vec![]), err)));
        assert_eq!(c.exec(&ConsoleCommand::ExtendDesktop).unwrap_err().reason, "unsupported");
        assert!(parse_reply(b"").is_err());

        let out = TopologyOutcome { applied: true, ..Default::default() };
        let reply = serde_json::to_vec(&HelperReply::Topology { outcome: Box::new(out.clone()) }).unwrap();
        let f = Arc::new(Fake(Mutex::new(vec![]), reply));
        let c = RunnerConsole::new(f.clone());
        assert_eq!(c.apply_topology(&TopologyRequest::default()).unwrap(), out);
        assert!(f.0.lock().unwrap()[0][2].contains(r#""cmd":"apply_topology""#));
        assert_eq!(f.0.lock().unwrap()[0][3], format!("timeout={}", TOPOLOGY_TIMEOUT.as_secs()), "topology gets its own longer limit");
    }

    #[test]
    fn command_json_shape() {
        let j = serde_json::to_string(&ConsoleCommand::Power { gdi_name: "\\\\.\\DISPLAY2".into(), on: false }).unwrap();
        assert!(j.starts_with(r#"{"cmd":"power""#), "{j}");
    }
}
