// SPDX-License-Identifier: GPL-3.0-only
//! In-memory mocks of every trait (feature `mock`, and in this crate's tests).
//!
//! Each mock records its calls as short strings in `calls()` (e.g. `"start 2"`,
//! `"apply_routes 3"`), serves the devices put into it, and can fail the next call with
//! `fail_next(err)`. Hot-plug / activity / packet sinks are stored so a test can fire
//! events with `emit_*`.

use crate::Clock;
use crate::backend::audio::{AudioBackendKind, AudioBinding, AudioCaps, AudioRoute, AudioRouter};
use crate::backend::display::{
    DisplayBackendKind, DisplayBinding, DisplayCaps, OutputPlacement, SeatOutputs, TopologyPlan, TopologyReport,
};
use crate::backend::gate::{DeviceGate, GateBackendKind, GateCaps, GatePlan, GateReport, IsolationStatus};
use crate::backend::input::{
    ActivityEvent, ActivitySink, HotkeySink, InputBackendKind, InputCaps, InputPacket, InputRoute, InputRouter,
    PacketSink, RouteReport,
};
use crate::backend::session::{
    HostLaunch, HostProbe, KioskShellProcess, KnownSession, RdpConnection, SeatStartRequest, SessionBackend, SessionBackendKind,
    SessionCaps, SessionInfo, SessionState, StartStep,
};
use crate::backend::{Availability, BackendError, BackendResult, HotplugEvent, HotplugSink};
use crate::config::GlobalSettings;
use crate::device::{Device, DeviceDetails, DeviceKind, Presentation, Rect};
use crate::ids::{DeviceKey, SeatNumber};
use crate::pnp::{PnpNode, PnpTree};
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Shared call log + failure injection used by every mock.
#[derive(Default)]
pub struct Recorder {
    calls: Mutex<Vec<String>>,
    fail: Mutex<Option<BackendError>>,
}

impl Recorder {
    fn hit(&self, call: impl Into<String>) -> BackendResult<()> {
        lock(&self.calls).push(call.into());
        match lock(&self.fail).take() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
    pub fn calls(&self) -> Vec<String> {
        lock(&self.calls).clone()
    }
    pub fn fail_next(&self, e: BackendError) {
        *lock(&self.fail) = Some(e);
    }
}

/// A plain device for tests.
pub fn device(key: &str, kind: DeviceKind) -> Device {
    Device {
        key: DeviceKey::new(key).expect("test key"),
        kind,
        name: key.to_string(),
        label: None,
        instance_id: None,
        container_id: None,
        parent_hub: None,
        present: true,
        hardware_id: None,
        serial: None,
        warnings: Vec::new(),
        details: DeviceDetails::None,
    }
}

/// Manually advanced clock.
#[derive(Debug, Default)]
pub struct MockClock(AtomicU64);

impl MockClock {
    pub fn advance(&self, ms: u64) {
        self.0.fetch_add(ms, Ordering::SeqCst);
    }
}

impl Clock for MockClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// Device tree from a map of nodes.
#[derive(Debug, Default)]
pub struct MockPnpTree {
    nodes: Mutex<BTreeMap<String, PnpNode>>,
    interfaces: Mutex<HashMap<String, String>>,
}

impl MockPnpTree {
    pub fn add(&self, n: PnpNode) {
        lock(&self.nodes).insert(n.instance_id.clone(), n);
    }
    pub fn add_interface(&self, path: &str, owner: &str) {
        lock(&self.interfaces).insert(path.to_string(), owner.to_string());
    }
}

impl PnpTree for MockPnpTree {
    fn node(&self, id: &str) -> Option<PnpNode> {
        lock(&self.nodes).get(id).cloned()
    }
    fn children(&self, id: &str) -> Vec<String> {
        lock(&self.nodes).values().filter(|n| n.parent.as_deref() == Some(id)).map(|n| n.instance_id.clone()).collect()
    }
    fn interface_owner(&self, path: &str) -> Option<String> {
        lock(&self.interfaces).get(path).cloned()
    }
    fn refresh(&self) -> Result<(), String> {
        Ok(())
    }
}

/// Session backend mock: `start` returns `LaunchHost` for RDP kinds, `Started` otherwise;
/// `wait_session` hands out session ids 10, 11, ….
pub struct MockSessionBackend {
    pub kind: SessionBackendKind,
    pub rec: Recorder,
    pub availability: Mutex<Availability>,
    sessions: Mutex<BTreeMap<SeatNumber, SessionInfo>>,
    next_id: AtomicU64,
    /// What `kiosk_shells` reports (tests set it).
    pub kiosk_shells: Mutex<Vec<KioskShellProcess>>,
    /// What `kiosk_users` reports (tests set it).
    pub kiosk_users: Mutex<Vec<crate::backend::session::KioskUser>>,
    /// Every `start`: (seat, a password was passed).
    pub start_passwords: Mutex<Vec<(SeatNumber, bool)>>,
    /// What `console_seat_away` reports (tests set it).
    pub console_seat_away: Mutex<Option<u32>>,
    /// `start` takes this long (ms; race tests).
    pub start_delay_ms: AtomicU64,
    /// Every `set_frame_interval` value, in order.
    pub frame_intervals: Mutex<Vec<Option<u32>>>,
}

impl MockSessionBackend {
    pub fn new(kind: SessionBackendKind) -> Self {
        Self {
            kind,
            rec: Recorder::default(),
            availability: Mutex::new(Availability::Available),
            sessions: Mutex::new(BTreeMap::new()),
            next_id: AtomicU64::new(10),
            kiosk_shells: Mutex::new(Vec::new()),
            kiosk_users: Mutex::new(Vec::new()),
            start_passwords: Mutex::new(Vec::new()),
            console_seat_away: Mutex::new(None),
            start_delay_ms: AtomicU64::new(0),
            frame_intervals: Mutex::new(Vec::new()),
        }
    }
    /// The seat's session ended (a crashed kiosk shell): `query` answers `Gone`.
    pub fn end_session(&self, seat: SeatNumber) {
        if let Some(i) = lock(&self.sessions).get_mut(&seat) {
            i.state = SessionState::Gone;
        }
    }
    fn new_session(&self, seat: SeatNumber, client: &str) -> SessionInfo {
        let info = SessionInfo {
            session_id: self.next_id.fetch_add(1, Ordering::SeqCst) as u32,
            state: SessionState::AtLogon,
            user_sid: None,
            user_name: None,
            client_name: Some(client.to_string()),
        };
        lock(&self.sessions).insert(seat, info.clone());
        info
    }
}

impl SessionBackend for MockSessionBackend {
    fn kind(&self) -> SessionBackendKind {
        self.kind
    }
    fn caps(&self) -> SessionCaps {
        let rdp = self.kind.uses_rdp();
        SessionCaps {
            concurrent: self.kind != SessionBackendKind::FastUserSwitch,
            max_seats: if self.kind == SessionBackendKind::FastUserSwitch { 1 } else { 16 },
            per_user: self.kind != SessionBackendKind::Desktops,
            needs_seat_host: rdp,
            remote_display: rdp,
            remote_input: rdp,
            remote_audio: rdp,
            auto_logon: rdp,
            stop_one: true,
        }
    }
    fn probe(&self) -> BackendResult<HostProbe> {
        self.rec.hit("probe")?;
        Ok(HostProbe {
            kind: self.kind,
            availability: lock(&self.availability).clone(),
            edition: "Mock".into(),
            build: 26100,
            arch: "arm64".into(),
            notes: Vec::new(),
        })
    }
    fn prepare_host(&self, _s: &GlobalSettings) -> BackendResult<Vec<String>> {
        self.rec.hit("prepare_host")?;
        Ok(Vec::new())
    }
    fn restore_host(&self) -> BackendResult<()> {
        self.rec.hit("restore_host")
    }
    fn set_frame_interval(&self, ms: Option<u32>) -> Vec<String> {
        lock(&self.frame_intervals).push(ms);
        Vec::new()
    }
    fn start(&self, req: &SeatStartRequest<'_>) -> BackendResult<StartStep> {
        self.rec.hit(format!("start {}", req.seat))?;
        lock(&self.start_passwords).push((req.seat, req.credentials.as_ref().is_some_and(|c| c.password.is_some())));
        let delay = self.start_delay_ms.load(Ordering::SeqCst);
        if delay > 0 {
            std::thread::sleep(Duration::from_millis(delay));
        }
        if self.kind.uses_rdp() {
            Ok(StartStep::LaunchHost(HostLaunch {
                seat: req.seat,
                target_session: 1,
                connection: RdpConnection {
                    address: crate::consts::RDP_LOOPBACK_ADDRESS.into(),
                    port: 3389,
                    client_name: req.client_name.clone(),
                    user_hint: String::new(),
                    domain_hint: String::new(),
                    monitors: vec![Rect { x: 0, y: 0, width: 1920, height: 1080 }],
                    audio_playback: true,
                    audio_capture: true,
                    redirect_drives: false,
                    redirect_smartcards: false,
                },
            }))
        } else {
            Ok(StartStep::Started(self.new_session(req.seat, &req.client_name)))
        }
    }
    fn wait_session(&self, seat: SeatNumber, client_name: &str, _t: Duration) -> BackendResult<SessionInfo> {
        self.rec.hit(format!("wait_session {seat}"))?;
        Ok(self.new_session(seat, client_name))
    }
    fn query(&self, seat: SeatNumber, _id: u32) -> BackendResult<SessionInfo> {
        self.rec.hit(format!("query {seat}"))?;
        lock(&self.sessions)
            .get(&seat)
            .cloned()
            .ok_or_else(|| BackendError::new(crate::backend::ErrorKind::NotFound, "no_session", "no session"))
    }
    fn log_off(&self, seat: SeatNumber, _id: u32) -> BackendResult<()> {
        self.rec.hit(format!("log_off {seat}"))?;
        lock(&self.sessions).remove(&seat);
        Ok(())
    }
    fn adopt(&self, known: &[KnownSession]) -> BackendResult<Vec<(SeatNumber, SessionInfo)>> {
        self.rec.hit(format!("adopt {}", known.len()))?;
        let s = lock(&self.sessions);
        Ok(known.iter().filter_map(|k| s.get(&k.seat).map(|i| (k.seat, i.clone()))).collect())
    }
    fn enumerate(&self) -> BackendResult<Vec<SessionInfo>> {
        self.rec.hit("enumerate")?;
        Ok(lock(&self.sessions).values().cloned().collect())
    }
    fn kiosk_shells(&self) -> Vec<KioskShellProcess> {
        lock(&self.kiosk_shells).clone()
    }
    fn kiosk_users(&self) -> Vec<crate::backend::session::KioskUser> {
        lock(&self.kiosk_users).clone()
    }
    fn console_seat_away(&self) -> Option<u32> {
        *lock(&self.console_seat_away)
    }
    fn restart_termservice(&self) -> BackendResult<Vec<String>> {
        if !self.kind.uses_rdp() {
            return Err(crate::backend::BackendError::unsupported("Remote Desktop Services restart"));
        }
        self.rec.hit("restart_termservice")?;
        Ok(vec!["termservice_restarted".into()])
    }
}

/// Holder of a hot-plug sink so tests can emit events.
#[derive(Default)]
pub struct SinkSlot(Mutex<Option<HotplugSink>>);

impl SinkSlot {
    fn set(&self, s: HotplugSink) {
        *lock(&self.0) = Some(s);
    }
    pub fn emit(&self, ev: HotplugEvent) {
        if let Some(s) = lock(&self.0).clone() {
            s(ev);
        }
    }
}

/// Display mock: serves `devices`, records topology / bind calls.
#[derive(Default)]
pub struct MockDisplay {
    pub rec: Recorder,
    pub devices: Mutex<Vec<Device>>,
    pub hotplug: SinkSlot,
}

impl DisplayBinding for MockDisplay {
    fn kind(&self) -> DisplayBackendKind {
        DisplayBackendKind::WindowPresenter
    }
    fn caps(&self) -> DisplayCaps {
        DisplayCaps {
            isolates_console: false,
            multi_monitor: true,
            clone: false,
            hardware_cursor: false,
            monitor_power: false,
            driver: None,
        }
    }
    fn availability(&self) -> Availability {
        Availability::Available
    }
    fn enumerate(&self) -> BackendResult<Vec<Device>> {
        self.rec.hit("enumerate")?;
        Ok(lock(&self.devices).clone())
    }
    fn apply_topology(&self, plan: &TopologyPlan) -> BackendResult<TopologyReport> {
        self.rec.hit(format!("apply_topology {}", plan.console.len()))?;
        Ok(TopologyReport::default())
    }
    fn configure_seat(&self, seat: SeatNumber, prefs: &crate::backend::display::SeatDisplayPrefs) {
        let follow = match prefs.follow_session_mode {
            Some(true) => "on",
            Some(false) => "off",
            None => "auto",
        };
        let _ = self.rec.hit(format!("configure_seat {seat} follow={follow}"));
    }
    fn bind_seat(&self, seat: SeatNumber, targets: &[DeviceKey]) -> BackendResult<SeatOutputs> {
        self.rec.hit(format!("bind_seat {seat} {}", targets.len()))?;
        Ok(SeatOutputs {
            seat,
            outputs: targets
                .iter()
                .enumerate()
                .map(|(i, k)| OutputPlacement {
                    key: k.clone(),
                    rect: Rect { x: 1920 * i as i32, y: 0, width: 1920, height: 1080 },
                    target_id: i as u32,
                    adapter_luid: 1,
                    mode: None,
                    presentation: Presentation::Fallback,
                    software_cursor: false,
                    clone_of: None,
                    follow_mode: false,
                    hdr: false,
                    vrr: false,
                    area: None,
                    console_share: false,
                })
                .collect(),
        })
    }
    fn release_seat(&self, seat: SeatNumber) -> BackendResult<()> {
        self.rec.hit(format!("release_seat {seat}"))
    }
    fn identify(&self, targets: &[(DeviceKey, String)], on: bool) -> BackendResult<()> {
        self.rec.hit(format!("identify {} {on}", targets.len()))
    }
    fn set_power(&self, key: &DeviceKey, on: bool) -> BackendResult<()> {
        self.rec.hit(format!("set_power {key} {on}"))
    }
    fn set_hotplug_sink(&self, sink: HotplugSink) {
        self.hotplug.set(sink);
    }
}

/// Input mock.
#[derive(Default)]
pub struct MockInput {
    pub rec: Recorder,
    pub devices: Mutex<Vec<Device>>,
    pub routes: Mutex<Vec<InputRoute>>,
    pub hotplug: SinkSlot,
    activity: Mutex<Option<ActivitySink>>,
    packets: Mutex<Option<PacketSink>>,
    hotkey: Mutex<Option<HotkeySink>>,
}

impl MockInput {
    pub fn emit_activity(&self, ev: ActivityEvent) {
        if let Some(s) = lock(&self.activity).clone() {
            s(ev);
        }
    }
    pub fn emit_packets(&self, seat: SeatNumber, p: &[InputPacket]) {
        if let Some(s) = lock(&self.packets).clone() {
            s(seat, p);
        }
    }
    pub fn emit_hotkey(&self, ev: crate::backend::input::HotkeyEvent) {
        if let Some(s) = lock(&self.hotkey).clone() {
            s(ev);
        }
    }
}

impl InputRouter for MockInput {
    fn kind(&self) -> InputBackendKind {
        InputBackendKind::RawInputHost
    }
    fn caps(&self) -> InputCaps {
        InputCaps {
            isolates_console: false,
            hid_visibility: false,
            activity: true,
            hotkey: true,
            wizard_capture: true,
            driver: None,
        }
    }
    fn availability(&self) -> Availability {
        Availability::Available
    }
    fn enumerate(&self) -> BackendResult<Vec<Device>> {
        self.rec.hit("enumerate")?;
        Ok(lock(&self.devices).clone())
    }
    fn apply_routes(&self, routes: &[InputRoute]) -> BackendResult<RouteReport> {
        self.rec.hit(format!("apply_routes {}", routes.len()))?;
        *lock(&self.routes) = routes.to_vec();
        Ok(RouteReport { applied: routes.iter().map(|r| r.key.clone()).collect(), ..Default::default() })
    }
    fn set_wizard_capture(&self, on: bool) -> BackendResult<()> {
        self.rec.hit(format!("wizard {on}"))
    }
    fn set_hotplug_sink(&self, sink: HotplugSink) {
        self.hotplug.set(sink);
    }
    fn set_activity_sink(&self, sink: ActivitySink) {
        *lock(&self.activity) = Some(sink);
    }
    fn set_packet_sink(&self, sink: PacketSink) {
        *lock(&self.packets) = Some(sink);
    }
    fn set_hotkey_sink(&self, sink: HotkeySink) {
        *lock(&self.hotkey) = Some(sink);
    }
}

/// Audio mock: endpoint id = the device key string.
#[derive(Default)]
pub struct MockAudio {
    pub rec: Recorder,
    pub devices: Mutex<Vec<Device>>,
    pub hotplug: SinkSlot,
}

impl AudioRouter for MockAudio {
    fn kind(&self) -> AudioBackendKind {
        AudioBackendKind::RdpBridge
    }
    fn caps(&self) -> AudioCaps {
        AudioCaps { per_seat_render: true, per_seat_capture: true, console_sees_all: true, test_tone: true }
    }
    fn availability(&self) -> Availability {
        Availability::Available
    }
    fn enumerate(&self) -> BackendResult<Vec<Device>> {
        self.rec.hit("enumerate")?;
        Ok(lock(&self.devices).clone())
    }
    fn bind_seat(&self, seat: SeatNumber, b: &AudioBinding) -> BackendResult<AudioRoute> {
        self.rec.hit(format!("bind_seat {seat}"))?;
        Ok(AudioRoute {
            seat: Some(seat),
            render_endpoint_id: b.render.as_ref().map(|k| k.to_string()),
            capture_endpoint_id: b.capture.as_ref().map(|k| k.to_string()),
            notes: Vec::new(),
        })
    }
    fn release_seat(&self, seat: SeatNumber) -> BackendResult<()> {
        self.rec.hit(format!("release_seat {seat}"))
    }
    fn test_tone(&self, key: &DeviceKey, ms: u32) -> BackendResult<()> {
        self.rec.hit(format!("test_tone {key} {ms}"))
    }
    fn set_hotplug_sink(&self, sink: HotplugSink) {
        self.hotplug.set(sink);
    }
}

/// Device-gate mock: every planned device reported as `Descriptor`.
pub struct MockGate {
    pub kind: GateBackendKind,
    pub rec: Recorder,
    pub devices: Mutex<Vec<Device>>,
    pub last_plan: Mutex<Option<GatePlan>>,
    pub hotplug: SinkSlot,
}

impl MockGate {
    pub fn new(kind: GateBackendKind) -> Self {
        Self {
            kind,
            rec: Recorder::default(),
            devices: Mutex::new(Vec::new()),
            last_plan: Mutex::new(None),
            hotplug: SinkSlot::default(),
        }
    }
}

impl DeviceGate for MockGate {
    fn kind(&self) -> GateBackendKind {
        self.kind
    }
    fn caps(&self) -> GateCaps {
        GateCaps { kinds: vec![DeviceKind::Storage, DeviceKind::Other], revokes_handles: true, driver: None }
    }
    fn availability(&self) -> Availability {
        Availability::Available
    }
    fn enumerate(&self) -> BackendResult<Vec<Device>> {
        self.rec.hit("enumerate")?;
        Ok(lock(&self.devices).clone())
    }
    fn apply(&self, plan: &GatePlan) -> BackendResult<GateReport> {
        self.rec.hit(format!("apply {}", plan.devices.len()))?;
        *lock(&self.last_plan) = Some(plan.clone());
        Ok(GateReport {
            applied: plan.devices.iter().map(|(k, _)| (k.clone(), IsolationStatus::Descriptor)).collect(),
            ..Default::default()
        })
    }
    fn revert_all(&self) -> BackendResult<()> {
        self.rec.hit("revert_all")
    }
    fn set_hotplug_sink(&self, sink: HotplugSink) {
        self.hotplug.set(sink);
    }
}

/// Every mock, boxed as the service holds them.
pub struct MockBackends {
    pub session: Arc<MockSessionBackend>,
    pub display: Arc<MockDisplay>,
    pub input: Arc<MockInput>,
    pub audio: Arc<MockAudio>,
    pub gates: Vec<Arc<MockGate>>,
}

impl Default for MockBackends {
    fn default() -> Self {
        Self {
            session: Arc::new(MockSessionBackend::new(SessionBackendKind::RdsLoopback)),
            display: Arc::new(MockDisplay::default()),
            input: Arc::new(MockInput::default()),
            audio: Arc::new(MockAudio::default()),
            gates: vec![Arc::new(MockGate::new(GateBackendKind::DeviceSecurity))],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{GlobalSettings, SeatConfig};

    #[test]
    fn session_mock_flow() {
        let m = MockSessionBackend::new(SessionBackendKind::RdsLoopback);
        let two = SeatNumber::new(2).unwrap();
        let cfg = SeatConfig::new(two);
        let g = GlobalSettings::default();
        let req = SeatStartRequest { seat: two, config: &cfg, settings: &g, client_name: "PSEAT-02".into(), credentials: None, idd_outputs: None };
        assert!(matches!(m.start(&req).unwrap(), StartStep::LaunchHost(_)));
        let info = m.wait_session(two, "PSEAT-02", Duration::from_secs(1)).unwrap();
        assert_eq!(m.query(two, info.session_id).unwrap().session_id, 10);
        m.rec.fail_next(BackendError::unsupported("x"));
        assert!(m.log_off(two, 10).is_err());
        assert_eq!(m.rec.calls(), ["start 2", "wait_session 2", "query 2", "log_off 2"]);
    }

    #[test]
    fn sinks_fire() {
        let i = MockInput::default();
        let hits = Arc::new(AtomicU64::new(0));
        let h = hits.clone();
        i.set_packet_sink(Arc::new(move |_, p| {
            h.fetch_add(p.len() as u64, Ordering::SeqCst);
        }));
        i.emit_packets(SeatNumber::new(2).unwrap(), &[InputPacket::Sas, InputPacket::Key { sc: 0x1e, f: 0 }]);
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        let d = MockDisplay::default();
        let got = Arc::new(Mutex::new(0));
        let g = got.clone();
        d.set_hotplug_sink(Arc::new(move |_| *lock(&g) += 1));
        d.hotplug.emit(HotplugEvent::Removed { key: DeviceKey::new("mon;x").unwrap() });
        assert_eq!(*lock(&got), 1);
    }
}
