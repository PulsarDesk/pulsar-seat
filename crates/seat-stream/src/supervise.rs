// SPDX-License-Identifier: GPL-3.0-only
//! Seat stream-server supervision (spec 80 §10, spec 20 §6.2–§6.4).
//!
//! [`SeatServer`] owns one seat's server process: it writes `server.conf` (and `apps.json`
//! if missing), rotates `server.log`, hands upstream its per-start web-UI hash, launches
//! the binary as SYSTEM inside the seat session, connects the control channel, restarts
//! with exponential back-off (`seat_core::remote::watchdog`, 1 s doubling to 60 s), gives up and
//! reports the last log lines, and stops gracefully (`server.shutdown`, wait `grace` + 1 s,
//! then terminate).
//!
//! It is driven by the service's per-seat worker: [`SeatServer::start`] once, then
//! [`SeatServer::poll`] periodically (every ~250 ms is fine), [`SeatServer::stop`] at the
//! end. All system access goes through [`ServerLauncher`], [`ControlConnector`],
//! [`SecureFs`], [`Random`] and [`Clock`].
//!
//! **Token choice.** The server runs with a duplicated SYSTEM token whose session id is the
//! seat session (contracts `launch_system_in_session`, spec 80 §4.3): upstream needs SYSTEM
//! to send input to the seat's secure desktop (lock screen, UAC). The seat user's token is
//! used only for apps the server launches (upstream does that itself). The real launcher
//! (`crate::win::launch::SessionLauncher`) also puts the process in a per-seat job object
//! (kill on close), so a crashed service never leaves orphan servers behind.

use crate::control::upstream::{merge_web_ui_credentials, Random, WebUiCredentials};
use crate::control::{Hello, SeatControl, ServerEvent, ServerStatus};
use crate::dist::{ServerDist, ServerFlavour};
use crate::seatdir::{last_lines, SecureFs, SeatDir, LOG_KEEP};
use crate::{Clock, StreamError};
use seat_core::remote::watchdog::{Action, Phase, Policy, Watchdog};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Launched {
    pub pid: u32,
    /// Process creation time (FILETIME) to tell a reused pid apart.
    pub creation_time: u64,
}

/// Implemented by the integrator with seat_session::remote's launcher, or by
/// `crate::win::launch::SessionLauncher`.
pub trait ServerLauncher: Send + Sync {
    fn launch_system_in_session(
        &self,
        seat_id: &str,
        session_id: u32,
        exe: &Path,
        args: &[String],
        cwd: &Path,
    ) -> Result<Launched, StreamError>;
    fn terminate(&self, l: &Launched) -> Result<(), StreamError>;
    /// Exit code once the process has exited; `None` if still running after `timeout`.
    fn wait_exit(&self, l: &Launched, timeout: Duration) -> Option<i32>;
    /// Verify `exe` against the manifest digest and keep it from being replaced until the
    /// returned guard is dropped (held across [`Self::launch_system_in_session`]). The
    /// default only hashes by path; `crate::win::SessionLauncher` hashes through a handle
    /// that denies write/delete sharing and checks the install folder ACL.
    fn lock_binary(&self, exe: &Path, sha256: &str) -> Result<BinaryGuard, StreamError> {
        crate::dist::verify_file(exe, sha256)?;
        Ok(BinaryGuard(None))
    }
    /// Drop any bookkeeping for `l` (the supervisor stops tracking it while it may still
    /// run; the per-seat job still kills it). Default: nothing.
    fn forget(&self, _l: &Launched) {}
}

/// Keeps the verified server binary locked while it is launched.
pub struct BinaryGuard(pub Option<Box<dyn Send>>);

/// Opens the control channel to a freshly launched server (one attempt, short timeout).
/// Fork: seat pipe + server-pid check. Upstream: loopback HTTPS with `creds`.
pub trait ControlConnector: Send + Sync {
    fn connect(
        &self,
        seat_id: &str,
        flavour: ServerFlavour,
        server: &Launched,
        creds: Option<&WebUiCredentials>,
    ) -> Result<Box<dyn SeatControl>, StreamError>;
}

/// Spec 80 §10: back-off 1 s doubling to 60 s; give up after 5 failures in 5 min (sliding
/// window) or 5 consecutive failures (OQ 29 resolved).
pub fn server_policy() -> Policy {
    Policy { max_consecutive: 5, ..Policy::default() }
}

/// Status poll period (spec 80 §10).
pub const STATUS_EVERY_MS: u64 = 5_000;
/// Grace for `server.shutdown`.
pub const SHUTDOWN_GRACE_MS: u32 = 5_000;
/// How long after launch the control channel may take to come up.
pub const CONNECT_WINDOW_MS: u64 = 30_000;
const CONNECT_RETRY_MS: u64 = 1_000;
const LAST_LOG_LINES: usize = 20;

#[derive(Debug, Clone, PartialEq)]
pub enum StreamNotice {
    /// Process launched (first start or restart).
    Running,
    /// Control channel up; capabilities from `hello`.
    ControlReady { capabilities: Vec<String> },
    /// Upstream only: the web-UI channel never came up; pairing is unavailable.
    ControlUnavailable { reason: String },
    Restarting { in_ms: u64 },
    GaveUp { last_log: Vec<String> },
    Event(ServerEvent),
    /// Pulsar backend only: operator prompts, relay and trust changes, app launches
    /// (docs/split/agents/pulsar-host-backend.md §3.4).
    Peer(crate::backend::PeerEvent),
}

/// Outcome of [`SeatServer::reload_apps`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppsReload {
    /// Server re-read the list (fork `apps.reload`).
    Reloaded(u32),
    /// Server not running: the new file is used at the next start.
    NotRunning,
    /// Upstream without reload: restarted because no stream was active.
    Restarted,
    /// Upstream without reload and a stream is active, or the stream state cannot be
    /// known (no control channel, status failed): applies at the next start.
    Deferred,
}

pub struct SeatServerConfig {
    pub seat_id: String,
    pub dist: ServerDist,
    /// Absolute path of the verified server binary.
    pub exe: PathBuf,
    /// Working directory (install folder).
    pub cwd: PathBuf,
    pub dir: SeatDir,
    /// From [`crate::conf::generate`]; rewritten at every start.
    pub conf_text: String,
    /// Written only when `apps.json` does not exist yet.
    pub default_apps_json: String,
    pub service_version: String,
    /// Re-check the binary's SHA-256 before every launch (off only in tests).
    pub verify_binary: bool,
}

pub struct SeatServer {
    cfg: SeatServerConfig,
    fs: Arc<dyn SecureFs>,
    launcher: Arc<dyn ServerLauncher>,
    connector: Arc<dyn ControlConnector>,
    rng: Arc<dyn Random>,
    clock: Arc<dyn Clock>,
    watchdog: Watchdog,
    session_id: Option<u32>,
    child: Option<Launched>,
    /// Terminated but not yet confirmed gone (may still hold the ports and the seat pipe):
    /// no new server is launched until it has exited.
    terminating: Option<Launched>,
    control: Option<Box<dyn SeatControl>>,
    creds: Option<WebUiCredentials>,
    capabilities: Vec<String>,
    last_status: Option<ServerStatus>,
    next_status_ms: u64,
    next_connect_ms: u64,
    connect_deadline_ms: u64,
    connect_abandoned: bool,
}

impl SeatServer {
    pub fn new(
        cfg: SeatServerConfig,
        fs: Arc<dyn SecureFs>,
        launcher: Arc<dyn ServerLauncher>,
        connector: Arc<dyn ControlConnector>,
        rng: Arc<dyn Random>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            cfg,
            fs,
            launcher,
            connector,
            rng,
            clock,
            watchdog: Watchdog::new(server_policy()),
            session_id: None,
            child: None,
            terminating: None,
            control: None,
            creds: None,
            capabilities: Vec::new(),
            last_status: None,
            next_status_ms: 0,
            next_connect_ms: 0,
            connect_deadline_ms: 0,
            connect_abandoned: false,
        }
    }

    pub fn flavour(&self) -> ServerFlavour {
        self.cfg.dist.flavour
    }

    pub fn is_running(&self) -> bool {
        self.child.is_some()
    }

    /// A terminated server has not exited yet (relaunch is held back).
    pub fn is_terminating(&self) -> bool {
        self.terminating.is_some()
    }

    pub fn pid(&self) -> Option<u32> {
        self.child.map(|c| c.pid)
    }

    pub fn watchdog_phase(&self) -> Phase {
        self.watchdog.phase()
    }

    pub fn crash_count(&self) -> u32 {
        self.watchdog.total_crashes()
    }

    /// New `server.conf` contents for the next (re)start (settings change).
    pub fn set_conf(&mut self, conf_text: String) {
        self.cfg.conf_text = conf_text;
    }

    pub fn last_status(&self) -> Option<&ServerStatus> {
        self.last_status.as_ref()
    }

    pub fn capabilities(&self) -> &[String] {
        &self.capabilities
    }

    /// The control channel is up.
    pub fn has_control(&self) -> bool {
        self.control.is_some()
    }

    pub fn control(&mut self) -> Option<&mut dyn SeatControl> {
        match self.control.as_mut() {
            Some(c) => Some(c.as_mut()),
            None => None,
        }
    }

    /// Like [`Self::control`], but `server_without_control_pipe` when there is none.
    pub fn control_or_err(&mut self) -> Result<&mut dyn SeatControl, StreamError> {
        self.control().ok_or(StreamError::ServerWithoutControlPipe)
    }

    /// Prepare the seat directory and files, then launch. A launch failure here is returned
    /// (bad install or missing privilege, not a crash) and nothing keeps running.
    pub fn start(&mut self, session_id: u32) -> Result<(), StreamError> {
        crate::validate_seat_id(&self.cfg.seat_id)?;
        // Never start a second server while the old one may still run.
        self.stop()?;
        self.session_id = Some(session_id);
        self.cfg.dir.ensure(self.fs.as_ref())?;
        if self.fs.read(&self.cfg.dir.apps)?.is_none() {
            self.fs.write_atomic(&self.cfg.dir.apps, self.cfg.default_apps_json.as_bytes())?;
        }
        let now = self.clock.now_ms();
        self.watchdog.arm();
        if let Err(e) = self.launch_now(now) {
            self.watchdog.disarm();
            self.session_id = None;
            return Err(e);
        }
        Ok(())
    }

    fn launch_now(&mut self, now: u64) -> Result<(), StreamError> {
        let session_id = self.session_id.ok_or_else(|| StreamError::invalid("session_id", "seat not started"))?;
        if self.terminating.is_some() {
            return Err(StreamError::StreamServer("previous server has not exited yet".into()));
        }
        // Held until the launch call returns: the checked file is the launched file.
        let _guard = if self.cfg.verify_binary {
            Some(self.launcher.lock_binary(&self.cfg.exe, &self.cfg.dist.sha256)?)
        } else {
            None
        };
        let dir = &self.cfg.dir;
        self.fs.write_atomic(&dir.conf, self.cfg.conf_text.as_bytes())?;
        self.fs.rotate_log(&dir.log, LOG_KEEP)?;
        self.creds = None;
        if self.cfg.dist.flavour == ServerFlavour::Upstream {
            let creds = WebUiCredentials::generate(self.rng.as_ref())?;
            let existing = self.fs.read(&dir.state)?;
            let doc = merge_web_ui_credentials(existing.as_deref(), &creds.user, &creds.password, &creds.salt)?;
            self.fs.write_atomic(&dir.state, doc.as_bytes())?;
            self.creds = Some(creds);
        }
        let conf = dir.conf.to_str().ok_or_else(|| StreamError::invalid("conf", "path is not UTF-8"))?.to_string();
        let l = self.launcher.launch_system_in_session(
            &self.cfg.seat_id,
            session_id,
            &self.cfg.exe,
            &[conf],
            &self.cfg.cwd,
        )?;
        log::info!("stream server started (pid {})", l.pid);
        self.watchdog.on_started(now);
        self.child = Some(l);
        self.control = None;
        self.connect_abandoned = false;
        self.next_connect_ms = now;
        self.connect_deadline_ms = now + CONNECT_WINDOW_MS;
        self.next_status_ms = now + STATUS_EVERY_MS;
        Ok(())
    }

    fn last_log(&self) -> Vec<String> {
        let bytes = self.fs.read(&self.cfg.dir.log).ok().flatten().unwrap_or_default();
        last_lines(&bytes, LAST_LOG_LINES)
    }

    fn after_failure(&mut self, now: u64, action: Action, out: &mut Vec<StreamNotice>) {
        match action {
            Action::WaitUntil(t) => out.push(StreamNotice::Restarting { in_ms: t.saturating_sub(now) }),
            Action::GiveUp => {
                log::warn!("stream server gave up after {} failures", self.watchdog.total_crashes());
                out.push(StreamNotice::GaveUp { last_log: self.last_log() });
            }
            Action::StartNow | Action::None => {}
        }
    }

    fn on_exited(&mut self, now: u64, code: Option<i32>, out: &mut Vec<StreamNotice>) {
        log::warn!("stream server exited (code {code:?})");
        self.child = None;
        self.control = None;
        self.creds = None;
        let a = self.watchdog.on_exit(now);
        self.after_failure(now, a, out);
    }

    /// The control channel broke while the process may still run: treat as a crash. If it
    /// does not exit within 1 s it is parked in `terminating` and polled; no relaunch
    /// happens until it is gone.
    fn server_gone(&mut self, now: u64, why: &StreamError, out: &mut Vec<StreamNotice>) {
        log::warn!("seat control channel lost: {why}");
        let mut code = None;
        if let Some(c) = self.child {
            if let Err(e) = self.launcher.terminate(&c) {
                log::warn!("terminate stream server: {e}");
            }
            code = self.launcher.wait_exit(&c, Duration::from_secs(1));
            if code.is_none() {
                self.terminating = Some(c);
            }
        }
        self.on_exited(now, code, out);
    }

    /// Check a parked server; retry terminate while it still runs.
    fn poll_terminating(&mut self) {
        let Some(t) = self.terminating else { return };
        if self.launcher.wait_exit(&t, Duration::ZERO).is_some() {
            self.terminating = None;
        } else if let Err(e) = self.launcher.terminate(&t) {
            log::debug!("terminate (retry): {e}");
        }
    }

    fn try_connect(&mut self, now: u64, out: &mut Vec<StreamNotice>) {
        let Some(child) = self.child else { return };
        let flavour = self.cfg.dist.flavour;
        let res = self
            .connector
            .connect(&self.cfg.seat_id, flavour, &child, self.creds.as_ref())
            .and_then(|mut c| c.hello(&self.cfg.service_version).map(|h: Hello| (c, h)));
        match res {
            Ok((c, h)) => {
                self.capabilities = h.capabilities.clone();
                self.control = Some(c);
                out.push(StreamNotice::ControlReady { capabilities: h.capabilities });
            }
            Err(e) if now >= self.connect_deadline_ms => match flavour {
                // The fork must create its pipe before it opens listeners (spec 80 §8.4).
                ServerFlavour::PulsarFork => self.server_gone(now, &e, out),
                ServerFlavour::Upstream => {
                    log::warn!("upstream web-UI channel unavailable: {e}");
                    self.connect_abandoned = true;
                    out.push(StreamNotice::ControlUnavailable { reason: e.code().into() });
                }
            },
            Err(e) => {
                log::debug!("control channel not up yet: {e}");
                self.next_connect_ms = now + CONNECT_RETRY_MS;
            }
        }
    }

    /// Drive the state machine: exits, back-off restarts, control connect, events, status.
    pub fn poll(&mut self, now_ms: u64) -> Vec<StreamNotice> {
        let now = now_ms;
        let mut out = Vec::new();

        if let Some(c) = self.child {
            if let Some(code) = self.launcher.wait_exit(&c, Duration::ZERO) {
                self.on_exited(now, Some(code), &mut out);
            }
        }
        self.poll_terminating();

        if self.child.is_none() && self.terminating.is_none() && self.watchdog.poll(now) == Action::StartNow {
            match self.launch_now(now) {
                Ok(()) => out.push(StreamNotice::Running),
                Err(e) => {
                    log::warn!("stream server restart failed: {e}");
                    let a = self.watchdog.on_start_failed(now);
                    self.after_failure(now, a, &mut out);
                }
            }
        }

        if self.child.is_some() && self.control.is_none() && !self.connect_abandoned && now >= self.next_connect_ms {
            self.try_connect(now, &mut out);
        }

        let mut broken: Option<StreamError> = None;
        if let Some(c) = self.control.as_mut() {
            for _ in 0..64 {
                match c.next_event(Duration::ZERO) {
                    Ok(Some(ev)) => out.push(StreamNotice::Event(ev)),
                    Ok(None) => break,
                    Err(e @ StreamError::ControlPipe(_)) => {
                        broken = Some(e);
                        break;
                    }
                    Err(e) => {
                        log::debug!("event read: {e}");
                        break;
                    }
                }
            }
            if broken.is_none() && now >= self.next_status_ms {
                self.next_status_ms = now + STATUS_EVERY_MS;
                match c.status() {
                    Ok(s) => self.last_status = Some(s),
                    Err(e @ StreamError::ControlPipe(_)) => broken = Some(e),
                    Err(e) => log::debug!("status: {e}"),
                }
            }
        }
        if let Some(e) = broken {
            self.server_gone(now, &e, &mut out);
        }
        out
    }

    /// After `apps.json` was rewritten: reload, or restart when idle (upstream).
    pub fn reload_apps(&mut self) -> Result<AppsReload, StreamError> {
        if self.child.is_none() {
            return Ok(AppsReload::NotRunning);
        }
        if let Some(c) = self.control.as_mut() {
            match c.apps_reload() {
                Ok(n) => return Ok(AppsReload::Reloaded(n)),
                Err(StreamError::Unsupported(_)) => {}
                Err(e) => return Err(e),
            }
        }
        // Fresh status, not the last 5-s poll: a restart must never cut a live stream.
        let streaming = match self.control.as_mut() {
            Some(c) => match c.status() {
                Ok(s) => {
                    let active = s.stream_active;
                    self.last_status = Some(s);
                    Some(active)
                }
                Err(e) => {
                    log::debug!("status before restart: {e}");
                    None
                }
            },
            None => None,
        };
        if streaming != Some(false) {
            return Ok(AppsReload::Deferred);
        }
        let session = self.session_id.ok_or_else(|| StreamError::invalid("session_id", "seat not started"))?;
        self.stop()?;
        self.start(session)?;
        Ok(AppsReload::Restarted)
    }

    fn stop_quiet(&mut self) {
        if self.child.is_some() || self.terminating.is_some() {
            let _ = self.stop();
        }
    }

    /// Terminate and wait for a process; on failure it stays tracked in `terminating`.
    fn kill_and_wait(&mut self, l: Launched) -> Result<(), StreamError> {
        let res = self.launcher.terminate(&l).and_then(|()| match self.launcher.wait_exit(&l, Duration::from_secs(5)) {
            Some(_) => Ok(()),
            None => Err(StreamError::StreamServer("server did not exit after terminate".into())),
        });
        if res.is_err() {
            self.terminating = Some(l);
        }
        res
    }

    /// Graceful stop: `server.shutdown` (fork), wait grace + 1 s, then terminate. On error
    /// the process stays tracked, so a later `stop()` retries.
    pub fn stop(&mut self) -> Result<(), StreamError> {
        self.watchdog.disarm();
        if let Some(t) = self.terminating.take() {
            if self.launcher.wait_exit(&t, Duration::ZERO).is_none() {
                self.kill_and_wait(t)?;
            }
        }
        let Some(child) = self.child.take() else {
            self.control = None;
            self.creds = None;
            return Ok(());
        };
        let graceful = match self.control.as_mut() {
            Some(c) => match c.shutdown(SHUTDOWN_GRACE_MS) {
                Ok(()) => true,
                Err(e) => {
                    log::debug!("graceful shutdown not available: {e}");
                    false
                }
            },
            None => false,
        };
        self.control = None;
        self.creds = None;
        if graceful
            && self.launcher.wait_exit(&child, Duration::from_millis(u64::from(SHUTDOWN_GRACE_MS) + 1_000)).is_some()
        {
            return Ok(());
        }
        if let Err(e) = self.kill_and_wait(child) {
            // Still running: keep it as the child so callers see it and can retry.
            self.child = self.terminating.take();
            return Err(e);
        }
        Ok(())
    }
}

impl Drop for SeatServer {
    fn drop(&mut self) {
        self.stop_quiet();
        // Whatever could not be stopped is left to the per-seat job (kill on close).
        for l in [self.child.take(), self.terminating.take()].into_iter().flatten() {
            self.launcher.forget(&l);
        }
    }
}

#[cfg(test)]
mod tests;
