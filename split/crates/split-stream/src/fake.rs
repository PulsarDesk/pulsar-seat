// SPDX-License-Identifier: GPL-3.0-only
//! In-memory fakes of every system trait (`cfg(test)` or feature `fake`). Nothing here
//! touches the file system, the firewall, processes or the network.

use crate::control::upstream::{HttpsLoopback, LogSource, Random, WebUiCredentials};
use crate::control::{Hello, PairedClient, PipeTransport, SeatControl, ServerEvent, ServerStatus};
use crate::dist::ServerFlavour;
use crate::firewall::{Firewall, FirewallRuleSpec};
use crate::journal::{collapse_reverse, RestoreLog, RestoreRecord};
use crate::ports::ListenerProbe;
use crate::seatdir::{rotated_names, SecureFs};
use crate::supervise::{ControlConnector, Launched, ServerLauncher};
use crate::{Clock, StreamError};
use split_core::ports::Proto;
use split_proto::Secret;
use std::collections::{HashMap, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------- clock / random

#[derive(Clone, Default)]
pub struct FakeClock(Arc<AtomicU64>);

impl FakeClock {
    pub fn new(ms: u64) -> Self {
        Self(Arc::new(AtomicU64::new(ms)))
    }
    pub fn set(&self, ms: u64) {
        self.0.store(ms, Ordering::SeqCst);
    }
    pub fn advance(&self, ms: u64) -> u64 {
        self.0.fetch_add(ms, Ordering::SeqCst) + ms
    }
}

impl Clock for FakeClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// Deterministic "random" bytes (counter). Never use outside tests.
#[derive(Default)]
pub struct FakeRandom(AtomicU64);

impl Random for FakeRandom {
    fn fill(&self, buf: &mut [u8]) -> Result<(), StreamError> {
        for b in buf {
            *b = (self.0.fetch_add(1, Ordering::Relaxed) * 37 % 251) as u8;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------- files

#[derive(Default)]
struct MemFsState {
    files: HashMap<PathBuf, Vec<u8>>,
    dirs: HashMap<PathBuf, String>,
    writes: Vec<PathBuf>,
    planted: HashMap<PathBuf, Planted>,
    ensured: Vec<PathBuf>,
}

/// Something that already exists before the service touches the path (owner as an SDDL
/// alias or SID string: `SY` and `BA` are trusted, anything else is foreign).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Planted {
    Dir { owner: String },
    File { owner: String },
    /// Junction / symlink.
    Reparse,
}

fn trusted_owner(o: &str) -> bool {
    o == "SY" || o == "BA"
}

#[derive(Default)]
pub struct MemFs(Mutex<MemFsState>);

impl MemFs {
    pub fn dir_sddl(&self, p: &Path) -> Option<String> {
        lock(&self.0).dirs.get(p).cloned()
    }
    pub fn get(&self, p: &Path) -> Option<Vec<u8>> {
        lock(&self.0).files.get(p).cloned()
    }
    pub fn get_string(&self, p: &Path) -> Option<String> {
        self.get(p).map(|b| String::from_utf8_lossy(&b).into_owned())
    }
    pub fn put(&self, p: &Path, b: &[u8]) {
        lock(&self.0).files.insert(p.to_path_buf(), b.to_vec());
    }
    /// Pre-existing object at `p` (as if created by someone else before the service).
    pub fn plant(&self, p: &Path, what: Planted) {
        lock(&self.0).planted.insert(p.to_path_buf(), what);
    }
    /// Every directory passed to a successful `ensure_dir`, in order.
    pub fn ensured(&self) -> Vec<PathBuf> {
        lock(&self.0).ensured.clone()
    }
    /// Every path written with `write_atomic`, in order.
    pub fn writes(&self) -> Vec<PathBuf> {
        lock(&self.0).writes.clone()
    }
}

impl SecureFs for MemFs {
    fn ensure_dir(&self, p: &Path, sddl: &str) -> Result<(), StreamError> {
        let mut s = lock(&self.0);
        match s.planted.get(p) {
            None => {}
            Some(Planted::Dir { owner }) if trusted_owner(owner) => {}
            Some(other) => return Err(StreamError::Io(format!("untrusted existing directory {p:?}: {other:?}"))),
        }
        s.dirs.insert(p.to_path_buf(), sddl.to_string());
        s.ensured.push(p.to_path_buf());
        Ok(())
    }
    fn check_file(&self, p: &Path) -> Result<(), StreamError> {
        match lock(&self.0).planted.get(p) {
            None => Ok(()),
            Some(Planted::File { owner }) if trusted_owner(owner) => Ok(()),
            Some(other) => Err(StreamError::Io(format!("untrusted existing file {p:?}: {other:?}"))),
        }
    }
    fn write_atomic(&self, p: &Path, bytes: &[u8]) -> Result<(), StreamError> {
        let mut s = lock(&self.0);
        s.files.insert(p.to_path_buf(), bytes.to_vec());
        s.writes.push(p.to_path_buf());
        Ok(())
    }
    fn rotate_log(&self, p: &Path, keep: usize) -> Result<(), StreamError> {
        let mut s = lock(&self.0);
        let names = rotated_names(p, keep);
        if keep == 0 {
            s.files.remove(p);
            return Ok(());
        }
        s.files.remove(&names[keep - 1]);
        for i in (0..keep - 1).rev() {
            if let Some(b) = s.files.remove(&names[i]) {
                s.files.insert(names[i + 1].clone(), b);
            }
        }
        if let Some(b) = s.files.remove(p) {
            s.files.insert(names[0].clone(), b);
        }
        Ok(())
    }
    fn remove_dir_all(&self, p: &Path) -> Result<(), StreamError> {
        let mut s = lock(&self.0);
        s.files.retain(|k, _| !k.starts_with(p));
        s.dirs.retain(|k, _| !k.starts_with(p));
        Ok(())
    }
    fn read(&self, p: &Path) -> Result<Option<Vec<u8>>, StreamError> {
        Ok(lock(&self.0).files.get(p).cloned())
    }
    fn remove_file(&self, p: &Path) -> Result<(), StreamError> {
        lock(&self.0).files.remove(p);
        Ok(())
    }
}

// ---------------------------------------------------------------- https / log

#[derive(Default)]
struct HttpsState {
    responses: VecDeque<(u16, Vec<u8>)>,
    requests: Vec<(String, String, Option<Vec<u8>>)>,
}

#[derive(Clone, Default)]
pub struct FakeHttps(Arc<Mutex<HttpsState>>);

impl FakeHttps {
    pub fn push(&self, code: u16, body: &str) {
        lock(&self.0).responses.push_back((code, body.as_bytes().to_vec()));
    }
    pub fn requests(&self) -> Vec<(String, String, Option<Vec<u8>>)> {
        lock(&self.0).requests.clone()
    }
}

impl HttpsLoopback for FakeHttps {
    fn request(&mut self, method: &str, path: &str, body: Option<&[u8]>) -> Result<(u16, Vec<u8>), StreamError> {
        let mut s = lock(&self.0);
        s.requests.push((method.into(), path.into(), body.map(<[u8]>::to_vec)));
        s.responses.pop_front().ok_or_else(|| StreamError::WebUi("connection refused".into()))
    }
}

#[derive(Clone, Default)]
pub struct VecLog(Arc<Mutex<VecDeque<String>>>);

impl VecLog {
    pub fn push(&self, line: &str) {
        lock(&self.0).push_back(line.into());
    }
}

impl LogSource for VecLog {
    fn read_new(&mut self) -> Vec<String> {
        lock(&self.0).drain(..).collect()
    }
}

// ---------------------------------------------------------------- seat pipe

type Responder = Box<dyn FnMut(&serde_json::Value) -> Vec<serde_json::Value> + Send>;

#[derive(Default)]
struct PipeState {
    sent: Vec<serde_json::Value>,
    inbox: VecDeque<Vec<u8>>,
    responder: Option<Responder>,
    broken: bool,
}

/// Scriptable server end of the seat pipe. `recv_timeout` never waits.
#[derive(Clone, Default)]
pub struct FakePipe(Arc<Mutex<PipeState>>);

impl FakePipe {
    /// Called for every request; returned messages are queued for the client.
    pub fn respond_with(&self, f: impl FnMut(&serde_json::Value) -> Vec<serde_json::Value> + Send + 'static) {
        lock(&self.0).responder = Some(Box::new(f));
    }
    pub fn inject(&self, msg: serde_json::Value) {
        lock(&self.0).inbox.push_back(serde_json::to_vec(&msg).unwrap());
    }
    pub fn inject_raw(&self, bytes: &[u8]) {
        lock(&self.0).inbox.push_back(bytes.to_vec());
    }
    pub fn sent(&self) -> Vec<serde_json::Value> {
        lock(&self.0).sent.clone()
    }
    pub fn break_pipe(&self) {
        lock(&self.0).broken = true;
    }
}

impl PipeTransport for FakePipe {
    fn send(&mut self, frame: &[u8]) -> io::Result<()> {
        let mut s = lock(&self.0);
        if s.broken {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "pipe closed"));
        }
        let v: serde_json::Value = serde_json::from_slice(frame).map_err(io::Error::other)?;
        s.sent.push(v.clone());
        if let Some(r) = s.responder.as_mut() {
            let out = r(&v);
            for m in out {
                s.inbox.push_back(serde_json::to_vec(&m).unwrap());
            }
        }
        Ok(())
    }
    fn recv(&mut self) -> io::Result<Vec<u8>> {
        self.recv_timeout(Duration::ZERO)?.ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "no message"))
    }
    fn recv_timeout(&mut self, _timeout: Duration) -> io::Result<Option<Vec<u8>>> {
        let mut s = lock(&self.0);
        if let Some(m) = s.inbox.pop_front() {
            return Ok(Some(m));
        }
        if s.broken {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "pipe closed"));
        }
        Ok(None)
    }
}

/// A responder that behaves like the fork for `hello` (given seat id) and answers every
/// other request with `ok: true` and an empty body.
pub fn fork_responder(seat_id: &str) -> impl FnMut(&serde_json::Value) -> Vec<serde_json::Value> + Send + 'static {
    let seat_id = seat_id.to_string();
    move |req| {
        let id = req["id"].clone();
        let body = match req["type"].as_str() {
            Some("hello") => serde_json::json!({
                "protocol": 1, "server_version": "test", "seat_id": seat_id, "capabilities": ["pulsar_idd"]
            }),
            _ => serde_json::json!({}),
        };
        vec![serde_json::json!({ "id": id, "ok": true, "body": body })]
    }
}

// ---------------------------------------------------------------- firewall / journal / probe

#[derive(Default)]
pub struct FakeFirewall(Mutex<Vec<FirewallRuleSpec>>);

impl FakeFirewall {
    pub fn rules(&self) -> Vec<FirewallRuleSpec> {
        lock(&self.0).clone()
    }
}

impl Firewall for FakeFirewall {
    fn list_group(&self, group: &str) -> Result<Vec<FirewallRuleSpec>, StreamError> {
        Ok(lock(&self.0).iter().filter(|r| r.group == group).cloned().collect())
    }
    fn upsert(&self, r: &FirewallRuleSpec) -> Result<(), StreamError> {
        let mut v = lock(&self.0);
        v.retain(|e| e.name != r.name);
        v.push(r.clone());
        Ok(())
    }
    fn remove(&self, name: &str) -> Result<(), StreamError> {
        lock(&self.0).retain(|e| e.name != name);
        Ok(())
    }
    fn remove_group(&self, group: &str) -> Result<u32, StreamError> {
        let mut v = lock(&self.0);
        let before = v.len();
        v.retain(|e| e.group != group);
        Ok((before - v.len()) as u32)
    }
}

#[derive(Default)]
pub struct MemJournal(Mutex<Vec<RestoreRecord>>);

impl MemJournal {
    pub fn records(&self) -> Vec<RestoreRecord> {
        lock(&self.0).clone()
    }
}

impl RestoreLog for MemJournal {
    fn intent(&self, r: &RestoreRecord) -> Result<u64, StreamError> {
        let mut v = lock(&self.0);
        let seq = v.iter().map(|r| r.seq).max().unwrap_or(0) + 1;
        v.push(RestoreRecord { seq, status: "intent".into(), ..r.clone() });
        Ok(seq)
    }
    fn done(&self, seq: u64, r: &RestoreRecord) -> Result<(), StreamError> {
        lock(&self.0).push(RestoreRecord { seq, status: "done".into(), ..r.clone() });
        Ok(())
    }
    fn replay_reverse(&self) -> Result<Vec<RestoreRecord>, StreamError> {
        Ok(collapse_reverse(self.records()))
    }
}

pub struct FakeProbe(pub Mutex<Vec<(u16, Proto)>>);

impl FakeProbe {
    pub fn new(v: Vec<(u16, Proto)>) -> Self {
        Self(Mutex::new(v))
    }
}

impl ListenerProbe for FakeProbe {
    fn listening(&self) -> Result<Vec<(u16, Proto)>, StreamError> {
        Ok(lock(&self.0).clone())
    }
}

// ---------------------------------------------------------------- launcher / connector / control

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchCall {
    pub seat_id: String,
    pub session_id: u32,
    pub exe: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
}

#[derive(Default)]
struct LauncherState {
    next_pid: u32,
    calls: Vec<LaunchCall>,
    exited: HashMap<u32, i32>,
    terminated: Vec<u32>,
    fail_next: u32,
    /// `terminate` does not make the process exit (default false).
    stubborn: bool,
}

#[derive(Clone, Default)]
pub struct FakeLauncher(Arc<Mutex<LauncherState>>);

impl FakeLauncher {
    pub fn calls(&self) -> Vec<LaunchCall> {
        lock(&self.0).calls.clone()
    }
    /// Make the process with `pid` exit with `code`.
    pub fn exit(&self, pid: u32, code: i32) {
        lock(&self.0).exited.insert(pid, code);
    }
    /// The next `n` launches fail.
    pub fn fail_launches(&self, n: u32) {
        lock(&self.0).fail_next = n;
    }
    pub fn terminated(&self) -> Vec<u32> {
        lock(&self.0).terminated.clone()
    }
    pub fn last_pid(&self) -> u32 {
        lock(&self.0).next_pid + 99
    }
    /// `terminate` does not make the process exit.
    pub fn set_stubborn(&self, v: bool) {
        lock(&self.0).stubborn = v;
    }
}

impl ServerLauncher for FakeLauncher {
    fn launch_system_in_session(
        &self,
        seat_id: &str,
        session_id: u32,
        exe: &Path,
        args: &[String],
        cwd: &Path,
    ) -> Result<Launched, StreamError> {
        let mut s = lock(&self.0);
        s.calls.push(LaunchCall {
            seat_id: seat_id.into(),
            session_id,
            exe: exe.into(),
            args: args.to_vec(),
            cwd: cwd.into(),
        });
        if s.fail_next > 0 {
            s.fail_next -= 1;
            return Err(StreamError::Io("CreateProcessAsUserW: access denied".into()));
        }
        s.next_pid += 1;
        Ok(Launched { pid: s.next_pid + 99, creation_time: u64::from(s.next_pid) })
    }
    fn terminate(&self, l: &Launched) -> Result<(), StreamError> {
        let mut s = lock(&self.0);
        s.terminated.push(l.pid);
        if !s.stubborn {
            s.exited.entry(l.pid).or_insert(1);
        }
        Ok(())
    }
    fn wait_exit(&self, l: &Launched, _timeout: Duration) -> Option<i32> {
        lock(&self.0).exited.get(&l.pid).copied()
    }
}

#[derive(Default)]
pub struct FakeControlState {
    pub calls: Vec<String>,
    pub events: VecDeque<ServerEvent>,
    pub status: ServerStatus,
    pub clients: Vec<PairedClient>,
    pub broken: bool,
    /// `apps_reload` answers `unsupported` (upstream behaviour).
    pub no_reload: bool,
    pub no_shutdown: bool,
    pub capabilities: Vec<String>,
    pub last_pin: Option<String>,
}

/// Scriptable [`SeatControl`]; clone the handle to inspect calls.
#[derive(Clone, Default)]
pub struct FakeControl(pub Arc<Mutex<FakeControlState>>);

impl FakeControl {
    pub fn state(&self) -> MutexGuard<'_, FakeControlState> {
        lock(&self.0)
    }
    fn call(&self, name: &str) -> Result<MutexGuard<'_, FakeControlState>, StreamError> {
        let mut s = lock(&self.0);
        s.calls.push(name.into());
        if s.broken {
            return Err(StreamError::ControlPipe("broken pipe".into()));
        }
        Ok(s)
    }
}

impl SeatControl for FakeControl {
    fn hello(&mut self, _v: &str) -> Result<Hello, StreamError> {
        let s = self.call("hello")?;
        Ok(Hello { protocol: 1, server_version: "fake".into(), seat_id: String::new(), capabilities: s.capabilities.clone() })
    }
    fn status(&mut self) -> Result<ServerStatus, StreamError> {
        Ok(self.call("status")?.status.clone())
    }
    fn pair_submit(&mut self, _r: Option<u32>, pin: &Secret, _name: &str) -> Result<String, StreamError> {
        self.call("pair_submit")?.last_pin = Some(pin.0.clone());
        Ok("uuid-1".into())
    }
    fn pair_cancel(&mut self, _r: Option<u32>) -> Result<(), StreamError> {
        self.call("pair_cancel").map(drop)
    }
    fn clients_list(&mut self) -> Result<Vec<PairedClient>, StreamError> {
        Ok(self.call("clients_list")?.clients.clone())
    }
    fn unpair(&mut self, _u: &str) -> Result<(), StreamError> {
        self.call("unpair").map(drop)
    }
    fn unpair_all(&mut self) -> Result<(), StreamError> {
        self.call("unpair_all").map(drop)
    }
    fn apps_reload(&mut self) -> Result<u32, StreamError> {
        let s = self.call("apps_reload")?;
        if s.no_reload { Err(StreamError::Unsupported("apps.reload".into())) } else { Ok(1) }
    }
    fn session_close(&mut self, _q: bool) -> Result<(), StreamError> {
        self.call("session_close").map(drop)
    }
    fn shutdown(&mut self, _g: u32) -> Result<(), StreamError> {
        let s = self.call("shutdown")?;
        if s.no_shutdown { Err(StreamError::Unsupported("server.shutdown".into())) } else { Ok(()) }
    }
    fn log_level(&mut self, _l: &str) -> Result<(), StreamError> {
        self.call("log_level").map(drop)
    }
    fn reply_mode(&mut self, _r: &serde_json::Value) -> Result<(), StreamError> {
        self.call("reply_mode").map(drop)
    }
    fn pad_request(&mut self, _t: &str, _b: &serde_json::Value) -> Result<(), StreamError> {
        self.call("pad_request").map(drop)
    }
    fn next_event(&mut self, _t: Duration) -> Result<Option<ServerEvent>, StreamError> {
        let mut s = lock(&self.0);
        if s.broken {
            return Err(StreamError::ControlPipe("broken pipe".into()));
        }
        Ok(s.events.pop_front())
    }
}

#[derive(Default)]
struct ConnectorState {
    /// Fail this many connect attempts first.
    fail_first: u32,
    attempts: u32,
    saw_creds: Vec<bool>,
    controls: Vec<FakeControl>,
    template: Option<FakeControlState>,
}

/// Hands out a fresh [`FakeControl`] per successful connect.
#[derive(Clone, Default)]
pub struct FakeConnector(Arc<Mutex<ConnectorState>>);

impl FakeConnector {
    pub fn fail_first(&self, n: u32) {
        lock(&self.0).fail_first = n;
    }
    pub fn attempts(&self) -> u32 {
        lock(&self.0).attempts
    }
    pub fn saw_creds(&self) -> Vec<bool> {
        lock(&self.0).saw_creds.clone()
    }
    /// The control handed out by the n-th successful connect.
    pub fn control(&self, n: usize) -> FakeControl {
        lock(&self.0).controls[n].clone()
    }
    pub fn controls(&self) -> usize {
        lock(&self.0).controls.len()
    }
    /// Initial state for every new control.
    pub fn set_template(&self, f: impl FnOnce(&mut FakeControlState)) {
        let mut st = FakeControlState::default();
        f(&mut st);
        lock(&self.0).template = Some(st);
    }
}

impl ControlConnector for FakeConnector {
    fn connect(
        &self,
        _seat_id: &str,
        _flavour: ServerFlavour,
        _server: &Launched,
        creds: Option<&WebUiCredentials>,
    ) -> Result<Box<dyn SeatControl>, StreamError> {
        let mut s = lock(&self.0);
        s.attempts += 1;
        s.saw_creds.push(creds.is_some());
        if s.fail_first > 0 {
            s.fail_first -= 1;
            return Err(StreamError::ControlPipe("pipe not found".into()));
        }
        let st = match &s.template {
            Some(t) => FakeControlState {
                no_reload: t.no_reload,
                no_shutdown: t.no_shutdown,
                capabilities: t.capabilities.clone(),
                status: t.status.clone(),
                ..Default::default()
            },
            None => FakeControlState::default(),
        };
        let c = FakeControl(Arc::new(Mutex::new(st)));
        s.controls.push(c.clone());
        Ok(Box::new(c))
    }
}

// ---------------------------------------------------------------- Pulsar backend

use crate::backend::pulsar::{HostPipe, HostPipeListener, PulsarSecretStore, SecretKind};

/// In-memory Pulsar seat secrets.
#[derive(Default)]
pub struct MemSecrets(Mutex<HashMap<(String, SecretKind), String>>);

impl MemSecrets {
    pub fn value(&self, seat_id: &str, kind: SecretKind) -> Option<String> {
        lock(&self.0).get(&(seat_id.to_string(), kind)).cloned()
    }
}

impl PulsarSecretStore for MemSecrets {
    fn get(&self, seat_id: &str, kind: SecretKind) -> Result<Option<Secret>, StreamError> {
        Ok(self.value(seat_id, kind).map(Secret))
    }
    fn set(&self, seat_id: &str, kind: SecretKind, value: Option<&Secret>) -> Result<(), StreamError> {
        let mut m = lock(&self.0);
        match value {
            Some(v) => m.insert((seat_id.to_string(), kind), v.0.clone()),
            None => m.remove(&(seat_id.to_string(), kind)),
        };
        Ok(())
    }
}

#[derive(Default)]
struct HostPipesState {
    created: Vec<String>,
    pipes: Vec<FakePipe>,
    /// `accept` calls that answer "nobody yet" before the client shows up.
    accept_delay: u32,
    /// Never connect (the host does not start its client).
    never: bool,
    fail_create: bool,
    accepted_for: Vec<Launched>,
}

/// Service-side host pipes. Each `create` makes a [`FakePipe`] answered by
/// [`pulsar_responder`]; `accept` hands it out after `accept_delay` polls.
#[derive(Clone, Default)]
pub struct FakeHostPipes(Arc<Mutex<HostPipesState>>);

impl FakeHostPipes {
    pub fn created(&self) -> Vec<String> {
        lock(&self.0).created.clone()
    }
    /// The `n`-th pipe created (0-based).
    pub fn pipe(&self, n: usize) -> FakePipe {
        lock(&self.0).pipes[n].clone()
    }
    pub fn count(&self) -> usize {
        lock(&self.0).pipes.len()
    }
    pub fn set_accept_delay(&self, n: u32) {
        lock(&self.0).accept_delay = n;
    }
    pub fn set_never_connect(&self, v: bool) {
        lock(&self.0).never = v;
    }
    pub fn fail_create(&self, v: bool) {
        lock(&self.0).fail_create = v;
    }
    pub fn accepted_for(&self) -> Vec<Launched> {
        lock(&self.0).accepted_for.clone()
    }
}

struct FakeHostPipe {
    pipe: FakePipe,
    state: Arc<Mutex<HostPipesState>>,
    polls: u32,
}

impl HostPipeListener for FakeHostPipes {
    fn create(&self, seat_id: &str) -> Result<Box<dyn HostPipe>, StreamError> {
        let mut s = lock(&self.0);
        if s.fail_create {
            return Err(StreamError::ControlPipe("pipe name already exists".into()));
        }
        s.created.push(crate::backend::pulsar::control::pipe_name(seat_id));
        let pipe = FakePipe::default();
        pipe.respond_with(pulsar_responder(seat_id));
        s.pipes.push(pipe.clone());
        Ok(Box::new(FakeHostPipe { pipe, state: self.0.clone(), polls: 0 }))
    }
}

impl HostPipe for FakeHostPipe {
    fn accept(&mut self, expected: &Launched) -> Result<Option<Box<dyn PipeTransport>>, StreamError> {
        let mut s = lock(&self.state);
        if s.never {
            return Ok(None);
        }
        if self.polls < s.accept_delay {
            self.polls += 1;
            return Ok(None);
        }
        s.accepted_for.push(*expected);
        Ok(Some(Box::new(self.pipe.clone())))
    }
}

/// Behaves like `pulsar-host` protocol 1 for the given seat.
pub fn pulsar_responder(seat_id: &str) -> impl FnMut(&serde_json::Value) -> Vec<serde_json::Value> + Send + 'static {
    let seat_id = seat_id.to_string();
    let mut otp = 0u32;
    move |req| {
        let id = req["id"].clone();
        let body = match req["type"].as_str() {
            Some("hello") => serde_json::json!({
                "protocol": 1, "host_version": "test", "core_version": "test", "seat_id": seat_id,
                "capabilities": ["approve", "connect_password", "otp", "trust_store", "apps", "app_launch_via_service", "split_pads"]
            }),
            Some("config.apply") => serde_json::json!({ "applied": ["display_name"], "restart_needed": [] }),
            Some("apps.set") => serde_json::json!({ "count": req["body"]["apps"].as_array().map_or(0, Vec::len) }),
            Some("auth.otp.get") => serde_json::json!({ "otp": format!("otp-{otp}") }),
            Some("auth.otp.rotate") => {
                otp += 1;
                serde_json::json!({ "otp": format!("otp-{otp}") })
            }
            Some("trust.list") => serde_json::json!({ "devices": [
                { "key": "ab".repeat(32), "name": "Telefon", "label": "123456789", "paired_at": 1, "last_seen": 2 }
            ]}),
            Some("status") => serde_json::json!({
                "state": "online", "relay": { "state": "ok", "id": "123456789" }, "node_port": 48100,
                "clients_connected": 0, "stream_active": false, "pending_auth": false
            }),
            _ => serde_json::json!({}),
        };
        vec![serde_json::json!({ "id": id, "ok": true, "body": body })]
    }
}
