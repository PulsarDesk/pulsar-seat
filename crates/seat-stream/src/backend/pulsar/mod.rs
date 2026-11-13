// SPDX-License-Identifier: GPL-3.0-only
//! Pulsar backend: `pulsar-host.exe` per seat (docs/split/agents/pulsar-host-backend.md).
//!
//! Launch (§1): same launcher, seat job, watchdog policy and binary lock as Sunshine. argv
//! is only `--split-seat <id> --data-dir <abs> --log <abs>`; no secret ever goes on argv,
//! in the environment or in a file. The data dir `seats\<id>\pulsar\` holds only
//! `paired_devices.json` (written by the host) and the rotated log.
//!
//! Control (§3): the service creates the pipe **before** the launch, accepts only the
//! launched child (pid + creation time, checked by [`HostPipe::accept`]), then sends
//! `hello`, `secrets.load` (seat identity, connect password, relay key from the
//! [`PulsarSecretStore`]), `config.apply` (built from the seat record) and `apps.set`.
//! A pipe that does not come up within [`CONNECT_WINDOW_MS`], or breaks, counts as a crash.
//!
//! `pulsar-host` does not exist yet: without its manifest, [`PulsarHostBackend::prepare`]
//! fails with `backend_missing`. Nothing falls back to Sunshine.

pub mod control;

use self::control::{HostApp, HostConfig, HostMessage, HostSecrets, PulsarControl};
use super::{PeerEvent, Prepared, SeatStream, SeatStreamSpec, StreamBackend};
use crate::apps::{render_apps_json, AppEntry};
use crate::control::upstream::Random;
use crate::control::{PairedClient, PipeTransport};
use crate::dist::HostDist;
use crate::firewall::{apply_pulsar_seat_rules, remove_seat_rules, Firewall};
use crate::journal::RestoreLog;
use crate::ports::ListenerProbe;
use crate::seatdir::{last_lines, SecureFs, SeatDir, LOG_KEEP, SEAT_DIR_SDDL};
use crate::supervise::{
    server_policy, AppsReload, Launched, ServerLauncher, StreamNotice, CONNECT_WINDOW_MS, SHUTDOWN_GRACE_MS, STATUS_EVERY_MS,
};
use crate::{Clock, StreamError};
use split_core::watchdog::{Action, Phase, Watchdog};
use split_proto::{CapturePref, SeatConfig, Secret, StreamBackendInfo, StreamBackendKind};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const LAST_LOG_LINES: usize = 20;
/// Pipe DACL: SYSTEM + Administrators (rules.md); only the pid check grants control.
pub const HOST_PIPE_SDDL: &str = "D:P(A;;GA;;;SY)(A;;GA;;;BA)";
/// Name of the data folder inside the seat folder.
pub const DATA_DIR: &str = "pulsar";
pub const LOG_FILE: &str = "pulsar-host.log";
pub const TRUST_FILE: &str = "paired_devices.json";

/// `seats\<id>\pulsar`.
pub fn data_dir(dir: &SeatDir) -> PathBuf {
    dir.root.join(DATA_DIR)
}

/// `seats\<id>\pulsar\pulsar-host.log`.
pub fn log_path(dir: &SeatDir) -> PathBuf {
    data_dir(dir).join(LOG_FILE)
}

/// §1: the complete argv after the program name. Nothing secret, nothing else.
pub fn host_args(seat_id: &str, data_dir: &Path, log: &Path) -> Result<Vec<String>, StreamError> {
    crate::validate_seat_id(seat_id)?;
    let abs = |p: &Path, f: &str| -> Result<String, StreamError> {
        if !p.is_absolute() {
            return Err(StreamError::invalid(f, "must be absolute"));
        }
        p.to_str().map(str::to_string).ok_or_else(|| StreamError::invalid(f, "path is not UTF-8"))
    };
    Ok(vec!["--split-seat".into(), seat_id.into(), "--data-dir".into(), abs(data_dir, "data_dir")?, "--log".into(), abs(log, "log")?])
}

/// `config.apply` from the seat record (§2, §3.3). Seat options for relay, network mode,
/// port mapping and discovery are not in the seat record yet: contract defaults.
pub fn host_config(seat: &SeatConfig) -> HostConfig {
    HostConfig {
        display_name: seat.display_name.clone(),
        relay: None,
        network_mode: "auto".into(),
        node_port: seat.port,
        port_mapping: false,
        lan_discovery: true,
        access: "approve_or_password".into(),
        transmit_audio: seat.audio.stream_audio,
        audio_endpoint_id: None,
        render_adapter_luid: None,
        capture: match seat.stream.capture {
            CapturePref::Default => "auto",
            CapturePref::Ddx => "dxgi",
            CapturePref::Wgc => "wgc",
        }
        .into(),
        codecs: Vec::new(),
        max_clients: 1,
    }
}

/// App id: the upstream `uuid` key when present, else the list index.
pub fn app_id(index: usize, a: &AppEntry) -> String {
    a.extra.get("uuid").and_then(serde_json::Value::as_str).map(str::to_string).unwrap_or_else(|| index.to_string())
}

/// `apps.set` list (ids and names only).
pub fn host_apps(apps: &[AppEntry]) -> Vec<HostApp> {
    apps.iter().enumerate().map(|(i, a)| HostApp { id: app_id(i, a), name: a.name.clone(), image: None }).collect()
}

/// The app a client picked (`app.launch`).
pub fn find_app<'a>(apps: &'a [AppEntry], id: &str) -> Option<&'a AppEntry> {
    apps.iter().enumerate().find(|(i, a)| app_id(*i, a) == id).map(|(_, a)| a)
}

/// Per-seat secrets of a Pulsar seat (DPAPI under `HKLM\SOFTWARE\PulsarSplit\Secrets` in
/// the service; [`crate::fake::MemSecrets`] in tests). Values never reach logs or files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SecretKind {
    /// 32 random bytes, base64: the seat's own identity (its relay id follows from it).
    Identity,
    ConnectPassword,
    /// Relay access key issued to this seat.
    RelayKey,
}

impl SecretKind {
    pub const ALL: [SecretKind; 3] = [SecretKind::Identity, SecretKind::ConnectPassword, SecretKind::RelayKey];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Identity => "identity",
            Self::ConnectPassword => "connect_password",
            Self::RelayKey => "relay_key",
        }
    }
}

pub trait PulsarSecretStore: Send + Sync {
    fn get(&self, seat_id: &str, kind: SecretKind) -> Result<Option<Secret>, StreamError>;
    /// `None` deletes.
    fn set(&self, seat_id: &str, kind: SecretKind, value: Option<&Secret>) -> Result<(), StreamError>;
}

/// Service-owned server end of the host pipe (§3.1). Real:
/// `crate::win::host_pipe::WinHostPipes`; fake: [`crate::fake::FakeHostPipes`].
pub trait HostPipeListener: Send + Sync {
    /// Create the pipe (first instance only, remote clients rejected, [`HOST_PIPE_SDDL`]).
    /// Fails if the name already exists.
    fn create(&self, seat_id: &str) -> Result<Box<dyn HostPipe>, StreamError>;
}

pub trait HostPipe: Send {
    /// Non-blocking. `Ok(None)`: nobody (or only a foreign client, which is disconnected)
    /// has connected yet. `Ok(Some)`: the client is `expected` (pid + creation time).
    fn accept(&mut self, expected: &Launched) -> Result<Option<Box<dyn PipeTransport>>, StreamError>;
}

/// `(manifest, verified exe path, working dir)`; read at every seat start. A missing
/// manifest or binary is [`StreamError::BackendMissing`].
pub type HostDistSource = dyn Fn() -> Result<(HostDist, PathBuf, PathBuf), StreamError> + Send + Sync;

pub struct PulsarEnv {
    pub fs: Arc<dyn SecureFs>,
    pub launcher: Arc<dyn ServerLauncher>,
    pub pipes: Arc<dyn HostPipeListener>,
    pub secrets: Arc<dyn PulsarSecretStore>,
    pub rng: Arc<dyn Random>,
    pub clock: Arc<dyn Clock>,
    pub firewall: Arc<dyn Firewall>,
    pub journal: Arc<dyn RestoreLog>,
    pub dist: Arc<HostDistSource>,
    pub verify_binary: bool,
    pub ports: Arc<dyn ListenerProbe>,
}

pub struct PulsarHostBackend {
    env: Arc<PulsarEnv>,
}

impl PulsarHostBackend {
    pub fn new(env: PulsarEnv) -> Self {
        Self { env: Arc::new(env) }
    }

    fn new_identity(&self) -> Result<Secret, StreamError> {
        use base64::Engine as _;
        let mut raw = zeroize::Zeroizing::new([0u8; 32]);
        self.env.rng.fill(raw.as_mut())?;
        Ok(Secret(base64::engine::general_purpose::STANDARD.encode(raw.as_ref())))
    }

    /// The seat's identity, created and stored on first use.
    fn ensure_identity(&self, seat_id: &str) -> Result<(), StreamError> {
        if self.env.secrets.get(seat_id, SecretKind::Identity)?.is_none() {
            let id = self.new_identity()?;
            self.env.secrets.set(seat_id, SecretKind::Identity, Some(&id))?;
        }
        Ok(())
    }

    fn dist(&self) -> Result<(HostDist, PathBuf, PathBuf), StreamError> {
        (self.env.dist)().map_err(|e| match e {
            StreamError::ServerBinaryMissing(p) => StreamError::BackendMissing(p.display().to_string()),
            e => e,
        })
    }
}

impl StreamBackend for PulsarHostBackend {
    fn kind(&self) -> StreamBackendKind {
        StreamBackendKind::Pulsar
    }

    fn availability(&self) -> StreamBackendInfo {
        match self.dist() {
            Ok((d, _, _)) => StreamBackendInfo { backend: self.kind(), installed: true, reason: None, version: Some(d.version) },
            Err(e) => StreamBackendInfo { backend: self.kind(), installed: false, reason: Some(e.code().into()), version: None },
        }
    }

    fn prepare(&self, spec: &SeatStreamSpec<'_>) -> Result<Prepared, StreamError> {
        let s = &self.env;
        let seat = spec.seat;
        let mut warnings = Vec::new();
        // The node port (= base) lives in the seat block; the whole block must be free.
        match crate::ports::check_free(s.ports.as_ref(), seat.port, &[]) {
            Ok(()) => {}
            Err(e @ StreamError::PortInUse(_)) => return Err(e),
            Err(e) => log::warn!("seat '{}': port check skipped: {e}", seat.name),
        }
        let (dist, exe, cwd) = self.dist()?;
        let dir = SeatDir::checked(spec.data_root, &seat.id)?;
        self.ensure_identity(&seat.id)?;
        let cfg = host_config(seat);
        if let Err(e) = apply_pulsar_seat_rules(
            s.firewall.as_ref(),
            s.journal.as_ref(),
            &seat.id,
            &seat.display_name,
            cfg.node_port,
            &exe,
            seat.stream.firewall_domain,
        ) {
            log::warn!("seat '{}': firewall rule not applied: {e}", seat.name);
            warnings.push(serde_json::json!({"kind": "firewall", "reason": e.code()}));
        }
        let seat_cfg = PulsarSeatConfig {
            seat_id: seat.id.clone(),
            exe,
            cwd,
            sha256: dist.sha256,
            data_dir: data_dir(&dir),
            log: log_path(&dir),
            dir,
            host: cfg,
            default_apps: spec.default_apps.clone(),
            service_version: spec.service_version.to_string(),
            verify_binary: s.verify_binary,
        };
        Ok(Prepared { stream: Box::new(PulsarSeat::new(seat_cfg, self.env.clone())), warnings })
    }

    /// Secrets first and every step attempted: a firewall / journal error must not leave
    /// the seat's secrets behind. Returns the first error.
    fn remove_seat(&self, seat_id: &str, keep_pairing: bool) -> Result<(), StreamError> {
        let mut first: Option<StreamError> = None;
        if !keep_pairing {
            for k in SecretKind::ALL {
                if let Err(e) = self.env.secrets.set(seat_id, k, None) {
                    first.get_or_insert(e);
                }
            }
        }
        if let Err(e) = remove_seat_rules(self.env.firewall.as_ref(), self.env.journal.as_ref(), seat_id) {
            first.get_or_insert(e);
        }
        first.map_or(Ok(()), Err)
    }

    fn log_file(&self, dir: &SeatDir) -> PathBuf {
        log_path(dir)
    }

    /// New identity (new relay id), trust store and relay key dropped. Seat is stopped.
    fn regenerate_identity(&self, dir: &SeatDir) -> Result<(), StreamError> {
        let seat_id = dir.root.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string();
        crate::validate_seat_id(&seat_id)?;
        let id = self.new_identity()?;
        self.env.secrets.set(&seat_id, SecretKind::Identity, Some(&id))?;
        self.env.secrets.set(&seat_id, SecretKind::RelayKey, None)?;
        self.env.fs.remove_file(&data_dir(dir).join(TRUST_FILE))
    }

    fn set_connect_password(&self, seat_id: &str, password: Option<&Secret>) -> Result<(), StreamError> {
        crate::validate_seat_id(seat_id)?;
        self.env.secrets.set(seat_id, SecretKind::ConnectPassword, password)
    }
}

struct PulsarSeatConfig {
    seat_id: String,
    exe: PathBuf,
    cwd: PathBuf,
    sha256: String,
    dir: SeatDir,
    data_dir: PathBuf,
    log: PathBuf,
    host: HostConfig,
    /// Written only when `apps.json` does not exist yet.
    default_apps: Vec<AppEntry>,
    service_version: String,
    verify_binary: bool,
}

type Control = PulsarControl<Box<dyn PipeTransport>>;

/// One seat's `pulsar-host` under supervision (same watchdog policy as Sunshine).
pub struct PulsarSeat {
    cfg: PulsarSeatConfig,
    env: Arc<PulsarEnv>,
    watchdog: Watchdog,
    session_id: Option<u32>,
    child: Option<Launched>,
    terminating: Option<Launched>,
    pipe: Option<Box<dyn HostPipe>>,
    control: Option<Control>,
    apps: Vec<HostApp>,
    capabilities: Vec<String>,
    last_status: Option<serde_json::Value>,
    relay_state: Option<String>,
    relay_id: Option<String>,
    relay_auth_required: bool,
    next_status_ms: u64,
    connect_deadline_ms: u64,
}

impl PulsarSeat {
    fn new(cfg: PulsarSeatConfig, env: Arc<PulsarEnv>) -> Self {
        Self {
            cfg,
            env,
            watchdog: Watchdog::new(server_policy()),
            session_id: None,
            child: None,
            terminating: None,
            pipe: None,
            control: None,
            apps: Vec::new(),
            capabilities: Vec::new(),
            last_status: None,
            relay_state: None,
            relay_id: None,
            relay_auth_required: false,
            next_status_ms: 0,
            connect_deadline_ms: 0,
        }
    }

    pub fn watchdog_phase(&self) -> Phase {
        self.watchdog.phase()
    }

    fn control(&mut self) -> Result<&mut Control, StreamError> {
        self.control.as_mut().ok_or(StreamError::ServerWithoutControlPipe)
    }

    /// Current `apps.json` (or the default list) as `apps.set` entries.
    fn load_apps(&self) -> Vec<HostApp> {
        match self.env.fs.read(&self.cfg.dir.apps) {
            Ok(Some(b)) => match crate::apps::parse_apps_json(&String::from_utf8_lossy(&b)) {
                Ok(f) => host_apps(&f.apps),
                Err(e) => {
                    log::warn!("apps.json unreadable, sending no apps: {e}");
                    Vec::new()
                }
            },
            _ => host_apps(&self.cfg.default_apps),
        }
    }

    fn send_secrets(&mut self) -> Result<(), StreamError> {
        let store = self.env.secrets.clone();
        let id = &self.cfg.seat_id;
        let identity = store
            .get(id, SecretKind::Identity)?
            .ok_or_else(|| StreamError::invalid("identity", "seat identity missing"))?;
        let pw = store.get(id, SecretKind::ConnectPassword)?;
        let key = store.get(id, SecretKind::RelayKey)?;
        let s = HostSecrets { identity: &identity, connect_password: pw.as_ref(), relay_key: key.as_ref() };
        self.control()?.secrets_load(&s)
    }

    fn launch_now(&mut self, now: u64) -> Result<(), StreamError> {
        let session_id = self.session_id.ok_or_else(|| StreamError::invalid("session_id", "seat not started"))?;
        if self.terminating.is_some() {
            return Err(StreamError::StreamServer("previous pulsar-host has not exited yet".into()));
        }
        let _guard = if self.cfg.verify_binary { Some(self.env.launcher.lock_binary(&self.cfg.exe, &self.cfg.sha256)?) } else { None };
        self.env.fs.ensure_dir(&self.cfg.data_dir, SEAT_DIR_SDDL)?;
        self.env.fs.check_file(&self.cfg.data_dir.join(TRUST_FILE))?;
        self.env.fs.rotate_log(&self.cfg.log, LOG_KEEP)?;
        let args = host_args(&self.cfg.seat_id, &self.cfg.data_dir, &self.cfg.log)?;
        // The pipe exists before the child does: no squatting window (§3.1).
        self.control = None;
        self.pipe = None;
        let pipe = self.env.pipes.create(&self.cfg.seat_id)?;
        let l = self.env.launcher.launch_system_in_session(&self.cfg.seat_id, session_id, &self.cfg.exe, &args, &self.cfg.cwd)?;
        log::info!("pulsar-host started (pid {})", l.pid);
        self.watchdog.on_started(now);
        self.child = Some(l);
        self.pipe = Some(pipe);
        self.connect_deadline_ms = now + CONNECT_WINDOW_MS;
        self.next_status_ms = now + STATUS_EVERY_MS;
        Ok(())
    }

    fn last_log(&self) -> Vec<String> {
        let bytes = self.env.fs.read(&self.cfg.log).ok().flatten().unwrap_or_default();
        last_lines(&bytes, LAST_LOG_LINES)
    }

    fn after_failure(&mut self, now: u64, action: Action, out: &mut Vec<StreamNotice>) {
        match action {
            Action::WaitUntil(t) => out.push(StreamNotice::Restarting { in_ms: t.saturating_sub(now) }),
            Action::GiveUp => {
                log::warn!("pulsar-host gave up after {} failures", self.watchdog.total_crashes());
                out.push(StreamNotice::GaveUp { last_log: self.last_log() });
            }
            Action::StartNow | Action::None => {}
        }
    }

    fn on_exited(&mut self, now: u64, code: Option<i32>, out: &mut Vec<StreamNotice>) {
        log::warn!("pulsar-host exited (code {code:?})");
        self.child = None;
        self.control = None;
        self.pipe = None;
        self.relay_auth_required = false;
        let a = self.watchdog.on_exit(now);
        self.after_failure(now, a, out);
    }

    fn server_gone(&mut self, now: u64, why: &StreamError, out: &mut Vec<StreamNotice>) {
        log::warn!("pulsar-host control lost: {why}");
        let mut code = None;
        if let Some(c) = self.child {
            if let Err(e) = self.env.launcher.terminate(&c) {
                log::warn!("terminate pulsar-host: {e}");
            }
            code = self.env.launcher.wait_exit(&c, Duration::from_secs(1));
            if code.is_none() {
                self.terminating = Some(c);
            }
        }
        self.on_exited(now, code, out);
    }

    fn poll_terminating(&mut self) {
        let Some(t) = self.terminating else { return };
        if self.env.launcher.wait_exit(&t, Duration::ZERO).is_some() {
            self.terminating = None;
        } else if let Err(e) = self.env.launcher.terminate(&t) {
            log::debug!("terminate (retry): {e}");
        }
    }

    /// `hello` → `secrets.load` → `config.apply` → `apps.set`.
    fn handshake(&mut self, t: Box<dyn PipeTransport>) -> Result<Vec<String>, StreamError> {
        let mut c = PulsarControl::new(t, &self.cfg.seat_id);
        let h = c.hello(&self.cfg.service_version)?;
        self.control = Some(c);
        self.send_secrets()?;
        let host = self.cfg.host.clone();
        let applied = self.control()?.config_apply(&host)?;
        if !applied.restart_needed.is_empty() {
            log::debug!("pulsar-host: settings needing a restart at first apply: {:?}", applied.restart_needed);
        }
        self.apps = self.load_apps();
        let apps = self.apps.clone();
        self.control()?.apps_set(&apps)?;
        Ok(h.capabilities)
    }

    fn try_connect(&mut self, now: u64, out: &mut Vec<StreamNotice>) {
        let (Some(child), Some(pipe)) = (self.child, self.pipe.as_mut()) else { return };
        match pipe.accept(&child) {
            Ok(Some(t)) => match self.handshake(t) {
                Ok(caps) => {
                    self.capabilities = caps.clone();
                    out.push(StreamNotice::ControlReady { capabilities: caps });
                }
                Err(e) => {
                    self.control = None;
                    self.server_gone(now, &e, out);
                }
            },
            Ok(None) if now >= self.connect_deadline_ms => {
                self.server_gone(now, &StreamError::ControlPipe("pulsar-host did not connect in time".into()), out);
            }
            Ok(None) => {}
            Err(e) => self.server_gone(now, &e, out),
        }
    }

    fn host_message(&mut self, m: HostMessage, out: &mut Vec<StreamNotice>) {
        match m {
            HostMessage::Shared(ev) => out.push(StreamNotice::Event(ev)),
            HostMessage::RelayKeyIssued(key) => {
                if let Err(e) = self.env.secrets.set(&self.cfg.seat_id, SecretKind::RelayKey, Some(&key)) {
                    log::warn!("relay key not stored: {e}");
                }
            }
            HostMessage::Peer(p) => {
                match &p {
                    PeerEvent::RelayState { state, id } => {
                        self.relay_state = Some(state.clone());
                        if id.is_some() {
                            self.relay_id = id.clone();
                        }
                        if state != "auth_required" {
                            self.relay_auth_required = false;
                        }
                    }
                    PeerEvent::RelayAuthRequired { .. } => self.relay_auth_required = true,
                    _ => {}
                }
                out.push(StreamNotice::Peer(p));
            }
        }
    }

    fn kill_and_wait(&mut self, l: Launched) -> Result<(), StreamError> {
        let res = self.env.launcher.terminate(&l).and_then(|()| match self.env.launcher.wait_exit(&l, Duration::from_secs(5)) {
            Some(_) => Ok(()),
            None => Err(StreamError::StreamServer("pulsar-host did not exit after terminate".into())),
        });
        if res.is_err() {
            self.terminating = Some(l);
        }
        res
    }
}

impl SeatStream for PulsarSeat {
    fn backend(&self) -> StreamBackendKind {
        StreamBackendKind::Pulsar
    }

    fn start(&mut self, session_id: u32) -> Result<(), StreamError> {
        crate::validate_seat_id(&self.cfg.seat_id)?;
        self.stop()?;
        self.session_id = Some(session_id);
        self.cfg.dir.ensure(self.env.fs.as_ref())?;
        if self.env.fs.read(&self.cfg.dir.apps)?.is_none() {
            let json = render_apps_json(&self.cfg.default_apps, &serde_json::Map::new());
            self.env.fs.write_atomic(&self.cfg.dir.apps, json.as_bytes())?;
        }
        let now = self.env.clock.now_ms();
        self.watchdog.arm();
        if let Err(e) = self.launch_now(now) {
            self.watchdog.disarm();
            self.session_id = None;
            self.pipe = None;
            return Err(e);
        }
        Ok(())
    }

    fn stop(&mut self) -> Result<(), StreamError> {
        self.watchdog.disarm();
        if let Some(t) = self.terminating.take()
            && self.env.launcher.wait_exit(&t, Duration::ZERO).is_none()
        {
            self.kill_and_wait(t)?;
        }
        let Some(child) = self.child.take() else {
            self.control = None;
            self.pipe = None;
            return Ok(());
        };
        let graceful = match self.control.as_mut() {
            Some(c) => match c.shutdown(SHUTDOWN_GRACE_MS) {
                Ok(()) => true,
                Err(e) => {
                    log::debug!("pulsar-host graceful shutdown failed: {e}");
                    false
                }
            },
            None => false,
        };
        self.control = None;
        self.pipe = None;
        self.relay_auth_required = false;
        if graceful
            && self.env.launcher.wait_exit(&child, Duration::from_millis(u64::from(SHUTDOWN_GRACE_MS) + 1_000)).is_some()
        {
            return Ok(());
        }
        if let Err(e) = self.kill_and_wait(child) {
            self.child = self.terminating.take();
            return Err(e);
        }
        Ok(())
    }

    fn poll(&mut self, now_ms: u64) -> Vec<StreamNotice> {
        let now = now_ms;
        let mut out = Vec::new();
        if let Some(c) = self.child
            && let Some(code) = self.env.launcher.wait_exit(&c, Duration::ZERO)
        {
            self.on_exited(now, Some(code), &mut out);
        }
        self.poll_terminating();
        if self.child.is_none() && self.terminating.is_none() && self.watchdog.poll(now) == Action::StartNow {
            match self.launch_now(now) {
                Ok(()) => out.push(StreamNotice::Running),
                Err(e) => {
                    log::warn!("pulsar-host restart failed: {e}");
                    self.pipe = None;
                    let a = self.watchdog.on_start_failed(now);
                    self.after_failure(now, a, &mut out);
                }
            }
        }
        if self.child.is_some() && self.control.is_none() {
            self.try_connect(now, &mut out);
        }
        let mut broken: Option<StreamError> = None;
        let mut msgs = Vec::new();
        if let Some(c) = self.control.as_mut() {
            for _ in 0..64 {
                match c.next_event(Duration::ZERO) {
                    Ok(Some(m)) => msgs.push(m),
                    Ok(None) => break,
                    Err(e @ StreamError::ControlPipe(_)) => {
                        broken = Some(e);
                        break;
                    }
                    Err(e) => {
                        log::debug!("pulsar-host event read: {e}");
                        break;
                    }
                }
            }
            if broken.is_none() && now >= self.next_status_ms {
                self.next_status_ms = now + STATUS_EVERY_MS;
                match c.status() {
                    Ok(s) => self.last_status = Some(s),
                    Err(e @ StreamError::ControlPipe(_)) => broken = Some(e),
                    Err(e) => log::debug!("pulsar-host status: {e}"),
                }
            }
        }
        for m in msgs {
            self.host_message(m, &mut out);
        }
        if let Some(e) = broken {
            self.server_gone(now, &e, &mut out);
        }
        out
    }

    fn is_running(&self) -> bool {
        self.child.is_some()
    }

    fn crash_count(&self) -> u32 {
        self.watchdog.total_crashes()
    }

    fn control_ready(&self) -> bool {
        self.control.is_some()
    }

    fn runtime(&self) -> serde_json::Value {
        serde_json::json!({
            "backend": "pulsar",
            "running": self.child.is_some(),
            "status": self.last_status,
            "capabilities": self.capabilities,
            "crash_count": self.watchdog.total_crashes(),
            "relay_state": self.relay_state,
            "relay_id": self.relay_id,
            "node_port": self.cfg.host.node_port,
            "relay_auth_required": self.relay_auth_required,
        })
    }

    fn reload_apps(&mut self, apps: &[AppEntry]) -> Result<AppsReload, StreamError> {
        self.apps = host_apps(apps);
        if self.child.is_none() {
            return Ok(AppsReload::NotRunning);
        }
        let list = self.apps.clone();
        match self.control.as_mut() {
            Some(c) => c.apps_set(&list).map(AppsReload::Reloaded),
            // Sent with the handshake once the host connects.
            None => Ok(AppsReload::Deferred),
        }
    }

    fn pair_answer(&mut self, request_id: u32, allow: bool, view_only: bool) -> Result<(), StreamError> {
        self.control()?.auth_decide(request_id, allow, view_only)
    }

    fn pair_cancel(&mut self, request_id: Option<u32>) -> Result<(), StreamError> {
        let id = request_id.ok_or(StreamError::NoPendingRequest)?;
        self.control()?.auth_decide(id, false, false)
    }

    fn clients_list(&mut self) -> Result<Vec<PairedClient>, StreamError> {
        Ok(self.control()?.trust_list()?.into_iter().map(|d| PairedClient { uuid: d.key, name: d.name }).collect())
    }

    fn unpair(&mut self, uuid: &str) -> Result<(), StreamError> {
        self.control()?.trust_revoke(uuid)
    }

    fn unpair_all(&mut self) -> Result<(), StreamError> {
        self.control()?.trust_revoke_all()
    }

    fn reply_mode(&mut self, result: &serde_json::Value) -> Result<(), StreamError> {
        self.control()?.display_mode_result(result)
    }

    fn pad_request(&mut self, ty: &str, body: &serde_json::Value) -> Result<(), StreamError> {
        self.control()?.pad_request(ty, body)
    }

    fn otp(&mut self, rotate: bool) -> Result<Secret, StreamError> {
        self.control()?.otp(rotate)
    }

    fn relay_auth(&mut self, password: &Secret, totp: Option<&Secret>) -> Result<(), StreamError> {
        self.control()?.relay_auth(password, totp)
    }

    fn secrets_changed(&mut self) -> Result<(), StreamError> {
        if self.control.is_none() {
            return Ok(()); // loaded at the next handshake
        }
        self.send_secrets()
    }

    fn app_launched(&mut self, request_id: u32, result: Result<Option<u32>, String>) -> Result<(), StreamError> {
        self.control()?.app_launched(request_id, &result)
    }

    fn relay_auth_required(&self) -> bool {
        self.relay_auth_required
    }
}

impl Drop for PulsarSeat {
    fn drop(&mut self) {
        if self.child.is_some() || self.terminating.is_some() {
            let _ = self.stop();
        }
        for l in [self.child.take(), self.terminating.take()].into_iter().flatten() {
            self.env.launcher.forget(&l);
        }
    }
}

#[cfg(test)]
mod tests;
