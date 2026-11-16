// SPDX-License-Identifier: GPL-3.0-only
//! In-memory fakes of every system trait (feature `fake`, and unit tests).

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::Read;
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crate::bluetooth::{StartType, UserServiceInstance, UserServices};
use crate::error::PadError;
use crate::ids;
use crate::owners::{DeviceNode, DeviceTree, OwnerStore, PadOwner, ProcessProbe};
use crate::pulsarpad::{DriverProbe, SwDeviceApi, SwDeviceHandle, SwDeviceSpec};
use crate::types::{
    BackendAvailability, BackendId, Clock, Feedback, FeedbackSink, PadBackend, PadCreate, PadId, PadKind, PadState,
    VirtualPad,
};
use crate::usbip::{proto, ConsoleSession, ToolProbe, UsbipTool};
use crate::vigem::{proto as vp, BusProbe, VigemConnection, VigemDriver};

// ---------------- clock / process / store ----------------

#[derive(Default)]
pub struct FakeClock(pub AtomicU64);
impl FakeClock {
    pub fn set(&self, ms: u64) {
        self.0.store(ms, Ordering::SeqCst)
    }
}
impl Clock for FakeClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

#[derive(Default)]
pub struct FakeProcessProbe(pub Mutex<BTreeSet<(u32, u64)>>);
impl FakeProcessProbe {
    pub fn add(&self, pid: u32, start: u64) {
        self.0.lock().unwrap().insert((pid, start));
    }
}
impl ProcessProbe for FakeProcessProbe {
    fn alive(&self, pid: u32, start_time: u64) -> bool {
        self.0.lock().unwrap().contains(&(pid, start_time))
    }
}

#[derive(Default, Clone)]
pub struct MemOwnerStore(pub Arc<Mutex<Vec<PadOwner>>>);
impl OwnerStore for MemOwnerStore {
    fn load(&self) -> Vec<PadOwner> {
        self.0.lock().unwrap().clone()
    }
    fn save(&self, all: &[PadOwner]) -> Result<(), PadError> {
        *self.0.lock().unwrap() = all.to_vec();
        Ok(())
    }
}

// ---------------- device tree ----------------

#[derive(Default)]
pub struct FakeDeviceTree {
    nodes: Mutex<BTreeMap<String, DeviceNode>>,
    pub security: Mutex<BTreeMap<String, String>>,
    pub sessions: Mutex<BTreeMap<String, u32>>,
    pub removed: Mutex<Vec<String>>,
}

impl FakeDeviceTree {
    pub fn add(&self, id: &str, parent: Option<&str>, present: bool) {
        self.nodes.lock().unwrap().insert(
            id.to_ascii_uppercase(),
            DeviceNode { instance_id: id.to_string(), present, parent: parent.map(str::to_string) },
        );
    }
    pub fn contains(&self, id: &str) -> bool {
        self.nodes.lock().unwrap().contains_key(&id.to_ascii_uppercase())
    }
    pub fn removed(&self) -> Vec<String> {
        self.removed.lock().unwrap().clone()
    }
    pub fn security_of(&self, id: &str) -> Option<String> {
        self.security.lock().unwrap().get(&id.to_ascii_uppercase()).cloned()
    }
    pub fn session_of(&self, id: &str) -> Option<u32> {
        self.sessions.lock().unwrap().get(&id.to_ascii_uppercase()).copied()
    }
}

impl DeviceTree for FakeDeviceTree {
    fn devices_with_marker(&self) -> Result<Vec<DeviceNode>, PadError> {
        Ok(self.nodes.lock().unwrap().values().filter(|n| ids::marker_of(&n.instance_id).is_some()).cloned().collect())
    }
    fn children(&self, instance_id: &str) -> Result<Vec<DeviceNode>, PadError> {
        Ok(self
            .nodes
            .lock()
            .unwrap()
            .values()
            .filter(|n| n.parent.as_deref().is_some_and(|p| p.eq_ignore_ascii_case(instance_id)))
            .cloned()
            .collect())
    }
    fn remove(&self, instance_id: &str) -> Result<(), PadError> {
        match self.nodes.lock().unwrap().remove(&instance_id.to_ascii_uppercase()) {
            Some(_) => {
                self.removed.lock().unwrap().push(instance_id.to_string());
                Ok(())
            }
            None => Err(PadError::DeviceLost(instance_id.to_string())),
        }
    }
    fn set_security(&self, instance_id: &str, sddl: &str) -> Result<(), PadError> {
        if !self.contains(instance_id) {
            return Err(PadError::DeviceLost(instance_id.into()));
        }
        self.security.lock().unwrap().insert(instance_id.to_ascii_uppercase(), sddl.into());
        Ok(())
    }
    fn set_session_id(&self, instance_id: &str, session_id: u32) -> Result<(), PadError> {
        if !self.contains(instance_id) {
            return Err(PadError::DeviceLost(instance_id.into()));
        }
        self.sessions.lock().unwrap().insert(instance_id.to_ascii_uppercase(), session_id);
        Ok(())
    }
}

// ---------------- feedback sink ----------------

#[derive(Default)]
pub struct RecordingSink {
    state: Mutex<(Vec<Feedback>, Vec<Vec<u8>>, usize)>,
    cv: Condvar,
}

impl RecordingSink {
    pub fn feedback(&self) -> Vec<Feedback> {
        self.state.lock().unwrap().0.clone()
    }
    pub fn raw(&self) -> Vec<Vec<u8>> {
        self.state.lock().unwrap().1.clone()
    }
    pub fn pcm_bytes(&self) -> usize {
        self.state.lock().unwrap().2
    }
    /// Waits (up to 2 s) until at least `n` feedback items arrived.
    pub fn wait_for(&self, n: usize) {
        let g = self.state.lock().unwrap();
        let _ = self.cv.wait_timeout_while(g, Duration::from_secs(2), |s| s.0.len() < n).unwrap();
    }
    /// Waits (up to 2 s) until at least `n` raw output reports arrived.
    pub fn wait_for_raw(&self, n: usize) {
        let g = self.state.lock().unwrap();
        let _ = self.cv.wait_timeout_while(g, Duration::from_secs(2), |s| s.1.len() < n).unwrap();
    }
}

impl FeedbackSink for RecordingSink {
    fn feedback(&self, _pad: &PadId, f: Feedback) {
        self.state.lock().unwrap().0.push(f);
        self.cv.notify_all();
    }
    fn raw_output(&self, _pad: &PadId, _source: u8, report: &[u8]) {
        self.state.lock().unwrap().1.push(report.to_vec());
        self.cv.notify_all();
    }
    fn haptics_pcm(&self, _pad: &PadId, _c: u8, _r: u32, pcm: &[u8]) {
        self.state.lock().unwrap().2 += pcm.len();
        self.cv.notify_all();
    }
}

// ---------------- ViGEm ----------------

pub const FAKE_VIGEM_BUS: &str = "ROOT\\SYSTEM\\0001";

struct VigemState {
    present: bool,
    busy: BTreeSet<u32>,
    plugged: BTreeSet<u32>,
    unplugged: Vec<u32>,
    submits: Vec<(u32, Vec<u8>)>,
    notifications: VecDeque<Vec<u8>>,
}

pub struct FakeVigem {
    state: Arc<(Mutex<VigemState>, Condvar)>,
    pub tree: Arc<FakeDeviceTree>,
}

impl Default for FakeVigem {
    fn default() -> Self {
        let tree = Arc::new(FakeDeviceTree::default());
        tree.add(FAKE_VIGEM_BUS, None, true);
        FakeVigem {
            state: Arc::new((
                Mutex::new(VigemState {
                    present: true,
                    busy: BTreeSet::new(),
                    plugged: BTreeSet::new(),
                    unplugged: Vec::new(),
                    submits: Vec::new(),
                    notifications: VecDeque::new(),
                }),
                Condvar::new(),
            )),
            tree,
        }
    }
}

impl FakeVigem {
    pub fn set_present(&self, p: bool) {
        self.state.0.lock().unwrap().present = p;
    }
    pub fn busy_serials(&self, s: &[u32]) {
        self.state.0.lock().unwrap().busy.extend(s);
    }
    pub fn last_submit(&self) -> Option<(u32, Vec<u8>)> {
        self.state.0.lock().unwrap().submits.last().cloned()
    }
    pub fn unplugged(&self) -> Vec<u32> {
        self.state.0.lock().unwrap().unplugged.clone()
    }
    /// Queues one completion for the next pended notification / await-output IOCTL.
    pub fn push_notification(&self, b: &[u8]) {
        self.state.0.lock().unwrap().notifications.push_back(b.to_vec());
        self.state.1.notify_all();
    }
}

impl VigemDriver for FakeVigem {
    fn probe(&self) -> BusProbe {
        let p = self.state.0.lock().unwrap().present;
        BusProbe { present: p, running: p, version: p.then(|| "1.22.0".to_string()) }
    }
    fn connect(&self) -> Result<Arc<dyn VigemConnection>, PadError> {
        if !self.state.0.lock().unwrap().present {
            return Err(PadError::DriverMissing("ViGEmBus not present".into()));
        }
        Ok(Arc::new(FakeVigemConn { state: self.state.clone(), tree: self.tree.clone(), cancelled: AtomicBool::new(false) }))
    }
}

struct FakeVigemConn {
    state: Arc<(Mutex<VigemState>, Condvar)>,
    tree: Arc<FakeDeviceTree>,
    cancelled: AtomicBool,
}

fn serial_of(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[4], b[5], b[6], b[7]])
}

impl VigemConnection for FakeVigemConn {
    fn ioctl(&self, code: u32, input: &[u8], _output: &mut [u8]) -> Result<usize, PadError> {
        let mut s = self.state.0.lock().unwrap();
        match code {
            vp::IOCTL_CHECK_VERSION => Ok(0),
            vp::IOCTL_PLUGIN_TARGET => {
                let serial = serial_of(input);
                if s.busy.contains(&serial) || s.plugged.contains(&serial) {
                    return Err(PadError::os("DeviceIoControl", 87));
                }
                s.plugged.insert(serial);
                let t = u32::from_le_bytes([input[8], input[9], input[10], input[11]]);
                let id = if t == 0 {
                    format!("USB\\VID_045E&PID_028E\\1&2D595CA7&0&{serial:02}")
                } else {
                    format!("USB\\VID_054C&PID_05C4\\1&2D595CA7&0&{serial:02}")
                };
                self.tree.add(&id, Some(FAKE_VIGEM_BUS), true);
                Ok(0)
            }
            vp::IOCTL_UNPLUG_TARGET => {
                let serial = serial_of(input);
                s.plugged.remove(&serial);
                s.unplugged.push(serial);
                Ok(0)
            }
            vp::IOCTL_WAIT_DEVICE_READY => Ok(0),
            _ => {
                s.submits.push((code, input.to_vec()));
                Ok(0)
            }
        }
    }
    fn ioctl_wait(&self, _code: u32, _input: &[u8], output: &mut [u8]) -> Result<Option<usize>, PadError> {
        let (m, cv) = &*self.state;
        let mut g = m.lock().unwrap();
        loop {
            if self.cancelled.load(Ordering::SeqCst) {
                return Ok(None);
            }
            if let Some(n) = g.notifications.pop_front() {
                let k = n.len().min(output.len());
                output[..k].copy_from_slice(&n[..k]);
                return Ok(Some(k));
            }
            g = cv.wait_timeout(g, Duration::from_millis(20)).unwrap().0;
        }
    }
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.state.1.notify_all();
    }
    fn bus_instance_id(&self) -> Option<String> {
        Some(FAKE_VIGEM_BUS.to_string())
    }
}

// ---------------- usbip ----------------

pub struct FakeConsole(pub u32);
impl ConsoleSession for FakeConsole {
    fn active_console_session(&self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ToolMode {
    Importing,
    Failing,
    NotInstalled,
}

/// Plays `usbip.exe`: `importing()` connects to our server, imports the busid and keeps
/// the socket for the test (as the usbip-win2 driver would).
pub struct FakeUsbipTool {
    mode: ToolMode,
    stream: Mutex<Option<TcpStream>>,
    detached: Mutex<Vec<u32>>,
}

impl FakeUsbipTool {
    pub fn importing() -> Self {
        FakeUsbipTool { mode: ToolMode::Importing, stream: Mutex::new(None), detached: Mutex::new(Vec::new()) }
    }
    pub fn failing() -> Self {
        FakeUsbipTool { mode: ToolMode::Failing, ..Self::importing() }
    }
    pub fn not_installed() -> Self {
        FakeUsbipTool { mode: ToolMode::NotInstalled, ..Self::importing() }
    }
    pub fn take_stream(&self) -> Option<TcpStream> {
        self.stream.lock().unwrap().take()
    }
    pub fn detached(&self) -> Vec<u32> {
        self.detached.lock().unwrap().clone()
    }
}

impl UsbipTool for FakeUsbipTool {
    fn probe(&self) -> ToolProbe {
        let ok = self.mode != ToolMode::NotInstalled;
        ToolProbe { installed: ok, running: ok, version: ok.then(|| "0.9.7.7".into()) }
    }
    fn attach(&self, host: &str, port: u16, busid: &str) -> Result<u32, PadError> {
        if self.mode != ToolMode::Importing {
            return Err(PadError::Usbip("usbip: error: cannot attach".into()));
        }
        let mut s = TcpStream::connect((host, port)).map_err(|e| PadError::Usbip(e.to_string()))?;
        proto::write_all(&mut s, &proto::encode_op_request(&proto::OpRequest::Import { busid: busid.into() }))
            .map_err(|e| PadError::Usbip(e.to_string()))?;
        let mut h = vec![0u8; 8];
        s.read_exact(&mut h).map_err(|e| PadError::Usbip(e.to_string()))?;
        if h[4..8] != [0, 0, 0, 0] {
            return Err(PadError::Usbip("import refused".into()));
        }
        let mut d = vec![0u8; 312];
        s.read_exact(&mut d).map_err(|e| PadError::Usbip(e.to_string()))?;
        *self.stream.lock().unwrap() = Some(s);
        Ok(1)
    }
    fn detach(&self, port: u32) -> Result<(), PadError> {
        self.detached.lock().unwrap().push(port);
        Ok(())
    }
}

// ---------------- software device ----------------

#[derive(Default)]
pub struct FakeSwDevice {
    staged: AtomicBool,
    live: Arc<Mutex<BTreeSet<String>>>,
    pub specs: Mutex<Vec<SwDeviceSpec>>,
}

impl FakeSwDevice {
    pub fn set_staged(&self, s: bool) {
        self.staged.store(s, Ordering::SeqCst)
    }
    pub fn live(&self) -> usize {
        self.live.lock().unwrap().len()
    }
}

struct FakeSwHandle {
    id: String,
    live: Arc<Mutex<BTreeSet<String>>>,
}

impl SwDeviceHandle for FakeSwHandle {
    fn instance_id(&self) -> String {
        self.id.clone()
    }
    fn close(self: Box<Self>) {
        self.live.lock().unwrap().remove(&self.id);
    }
}

impl SwDeviceApi for FakeSwDevice {
    fn driver_probe(&self) -> DriverProbe {
        let s = self.staged.load(Ordering::SeqCst);
        DriverProbe { staged: s, version: s.then(|| "0.1.0.0".into()) }
    }
    fn create(&self, spec: &SwDeviceSpec, _t: Duration) -> Result<Box<dyn SwDeviceHandle>, PadError> {
        let id = format!("SWD\\{}\\{}", spec.enumerator, spec.instance);
        if !self.live.lock().unwrap().insert(id.clone()) {
            return Err(PadError::MarkerConflict(format!("{id} exists")));
        }
        self.specs.lock().unwrap().push(spec.clone());
        Ok(Box::new(FakeSwHandle { id, live: self.live.clone() }))
    }
}

// ---------------- generic backend (manager tests) ----------------

#[derive(Debug, Clone, PartialEq)]
pub enum FakePadCall {
    Created(PadId, PadKind),
    Submit(PadId, PadState),
    Report(PadId, Vec<u8>),
    /// Microphone PCM (media section protocol 2).
    Mic(PadId, Vec<u8>),
    Removed(PadId),
}

pub struct FakeBackend {
    pub id: BackendId,
    pub kinds: Vec<PadKind>,
    pub installed: AtomicBool,
    pub fail_create: AtomicBool,
    pub calls: Arc<Mutex<Vec<FakePadCall>>>,
    pub sinks: Mutex<BTreeMap<PadId, Arc<dyn FeedbackSink>>>,
    pub tree: Option<Arc<FakeDeviceTree>>,
    pub lose_next_report: Arc<AtomicBool>,
    pub reads_channel: bool,
    /// `blocking_ops()`: the manager runs create/remove on workers.
    pub blocking: AtomicBool,
    /// While set, new pads report no instance id (node not found yet).
    pub hide_instance: Arc<AtomicBool>,
    /// While set, `create` waits (simulates a slow CLI run without relying on timing).
    pub hold_create: Arc<AtomicBool>,
}

impl FakeBackend {
    pub fn new(id: BackendId, kinds: &[PadKind], tree: Option<Arc<FakeDeviceTree>>) -> Self {
        FakeBackend {
            id,
            kinds: kinds.to_vec(),
            installed: AtomicBool::new(true),
            fail_create: AtomicBool::new(false),
            calls: Arc::new(Mutex::new(Vec::new())),
            sinks: Mutex::new(BTreeMap::new()),
            tree,
            lose_next_report: Arc::new(AtomicBool::new(false)),
            reads_channel: id == BackendId::PulsarPad,
            blocking: AtomicBool::new(false),
            hide_instance: Arc::new(AtomicBool::new(false)),
            hold_create: Arc::new(AtomicBool::new(false)),
        }
    }
    pub fn calls(&self) -> Vec<FakePadCall> {
        self.calls.lock().unwrap().clone()
    }
    pub fn sink(&self, id: &PadId) -> Option<Arc<dyn FeedbackSink>> {
        self.sinks.lock().unwrap().get(id).cloned()
    }
    pub fn instance_for(kind: PadKind, id: &PadId) -> String {
        ids::sw_instance_id(kind, id)
    }
}

impl PadBackend for FakeBackend {
    fn id(&self) -> BackendId {
        self.id
    }
    fn probe(&self) -> BackendAvailability {
        BackendAvailability {
            id: self.id,
            installed: self.installed.load(Ordering::SeqCst),
            version: Some("fake".into()),
            signed: true,
            kinds: self.kinds.clone(),
            reason: None,
        }
    }
    fn create(&self, req: &PadCreate, fb: Arc<dyn FeedbackSink>) -> Result<Box<dyn VirtualPad>, PadError> {
        while self.hold_create.load(Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        if self.fail_create.load(Ordering::SeqCst) {
            return Err(PadError::DriverMissing("fake failure".into()));
        }
        let id = req.pad_id()?;
        let inst = Self::instance_for(req.kind, &id);
        if let Some(t) = &self.tree {
            t.add(&inst, None, true);
        }
        self.sinks.lock().unwrap().insert(id.clone(), fb);
        self.calls.lock().unwrap().push(FakePadCall::Created(id.clone(), req.kind));
        Ok(Box::new(FakePad {
            id,
            kind: req.kind,
            inst,
            calls: self.calls.clone(),
            tree: self.tree.clone(),
            lose: self.lose_next_report.clone(),
            reads_channel: self.reads_channel,
            hide: self.hide_instance.clone(),
        }))
    }
    fn blocking_ops(&self) -> bool {
        self.blocking.load(Ordering::SeqCst)
    }
}

pub struct FakePad {
    id: PadId,
    kind: PadKind,
    inst: String,
    calls: Arc<Mutex<Vec<FakePadCall>>>,
    tree: Option<Arc<FakeDeviceTree>>,
    lose: Arc<AtomicBool>,
    reads_channel: bool,
    hide: Arc<AtomicBool>,
}

impl VirtualPad for FakePad {
    fn id(&self) -> &PadId {
        &self.id
    }
    fn kind(&self) -> PadKind {
        self.kind
    }
    fn instance_id(&self) -> Option<String> {
        (!self.hide.load(Ordering::SeqCst)).then(|| self.inst.clone())
    }
    fn submit(&mut self, s: &PadState) -> Result<(), PadError> {
        self.calls.lock().unwrap().push(FakePadCall::Submit(self.id.clone(), *s));
        Ok(())
    }
    fn submit_mic(&mut self, pcm: &[u8]) -> Result<(), PadError> {
        self.calls.lock().unwrap().push(FakePadCall::Mic(self.id.clone(), pcm.to_vec()));
        Ok(())
    }
    fn submit_report(&mut self, report: &[u8]) -> Result<(), PadError> {
        if self.lose.swap(false, Ordering::SeqCst) {
            return Err(PadError::DeviceLost("fake unplug".into()));
        }
        self.calls.lock().unwrap().push(FakePadCall::Report(self.id.clone(), report.to_vec()));
        Ok(())
    }
    fn driver_reads_channel(&self) -> bool {
        self.reads_channel
    }
    fn remove(self: Box<Self>) -> Result<(), PadError> {
        if let Some(t) = &self.tree {
            let _ = t.remove(&self.inst);
        }
        self.calls.lock().unwrap().push(FakePadCall::Removed(self.id.clone()));
        Ok(())
    }
}

// ---------------- bluetooth ----------------

#[derive(Default)]
pub struct FakeUserServices(pub Mutex<Vec<UserServiceInstance>>);
impl UserServices for FakeUserServices {
    fn instances(&self) -> Result<Vec<UserServiceInstance>, PadError> {
        Ok(self.0.lock().unwrap().clone())
    }
    fn set_start_type(&self, name: &str, t: StartType) -> Result<(), PadError> {
        let mut g = self.0.lock().unwrap();
        let i = g.iter_mut().find(|i| i.name == name).ok_or_else(|| PadError::Invalid(name.into()))?;
        i.start_type = t;
        Ok(())
    }
    fn stop(&self, name: &str) -> Result<(), PadError> {
        let mut g = self.0.lock().unwrap();
        let i = g.iter_mut().find(|i| i.name == name).ok_or_else(|| PadError::Invalid(name.into()))?;
        i.running = false;
        i.pid = None;
        i.session_id = None;
        Ok(())
    }
}
