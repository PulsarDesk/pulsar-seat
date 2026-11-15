// SPDX-License-Identifier: GPL-3.0-only
//! The backend set the service runs with, its selection with fallbacks (architecture §4.1),
//! the built-in `none` backends, and the `get_backends` report.
//!
//! Package backends come from a [`BackendFactory`] (`wiring.rs`). A kind the factory does
//! not provide is represented by an [`Unbuilt`] placeholder that reports
//! `unavailable{not_built}`, so selection and the manager behave the same either way.

use seat_core::backend::audio::{AudioBackendKind, AudioBinding, AudioCaps, AudioRoute, AudioRouter};
use seat_core::backend::display::{
    DisplayBackendKind, DisplayBinding, DisplayCaps, SeatOutputs, TopologyPlan, TopologyReport,
};
use seat_core::backend::gate::{DeviceGate, GateBackendKind, GateCaps, GatePlan, GateReport};
use seat_core::backend::input::{
    ActivitySink, HotkeySink, InputBackendKind, InputCaps, InputRoute, InputRouter, PacketSink, RouteReport,
};
use seat_core::backend::session::{
    HostProbe, HostTopology, KnownSession, SeatStartRequest, SessionBackend, SessionBackendKind, SessionCaps,
    SessionInfo, StartStep,
};
use seat_core::backend::{Availability, BackendError, BackendInfo, BackendResult, ErrorKind, HotplugSink};
use seat_core::config::{BackendSelection, EXPERIMENTAL_WARNING_VERSION, GlobalSettings};
use seat_core::device::Device;
use seat_core::{DeviceKey, SeatNumber};
use seat_proto::ActiveBackends;
use serde::Serialize;
use std::sync::Arc;
use std::time::Duration;

/// Builds package backends by kind. `None` = not available in this build.
pub trait BackendFactory: Send + Sync {
    fn session(&self, kind: SessionBackendKind, topology: HostTopology) -> Option<Arc<dyn SessionBackend>>;
    fn display(&self, kind: DisplayBackendKind) -> Option<Arc<dyn DisplayBinding>>;
    fn input(&self, kind: InputBackendKind) -> Option<Arc<dyn InputRouter>>;
    fn audio(&self, kind: AudioBackendKind) -> Option<Arc<dyn AudioRouter>>;
    fn gate(&self, kind: GateBackendKind) -> Option<Arc<dyn DeviceGate>>;
    /// The settings the next backends are built for (input settings, host topology).
    /// Called at the start of every [`select`].
    fn configure(&self, _settings: &GlobalSettings) {}
    /// Service-side controls of the input backend of `kind` beyond the contract trait
    /// (config revision, seat sessions, emergency combination); `None` for built-in and
    /// mock routers.
    fn input_control(&self, _kind: InputBackendKind) -> Option<Arc<dyn seat_input::InputControl>> {
        None
    }
    /// The input backend of `kind` needs the console raw-input listener
    /// (`pulsar-seat-host --console-listener`, `crate::listener`) for activity, hotkey and
    /// wizard input. Default: no (mocks, backends that see input themselves).
    fn console_listener(&self, _kind: InputBackendKind) -> bool {
        false
    }
    /// Agent work the gates produced since the last call (session drive letters), as
    /// `(session_id, task)`. Drained.
    fn take_gate_agent_tasks(&self) -> Vec<(u32, seat_proto::AgentTaskKind)> {
        Vec::new()
    }
    /// Effective seat masks of the audio endpoints, for the audio backend of `kind`
    /// (its shared fallback never picks another seat's endpoint, spec 60 §8.3).
    fn set_audio_masks(&self, _kind: AudioBackendKind, _masks: Vec<(seat_core::DeviceKey, seat_core::SeatMask)>) {}
}

/// snake_case name of a serde enum (`rds_loopback`).
pub fn kind_name<T: Serialize>(k: &T) -> String {
    serde_json::to_value(k).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_else(|| "unknown".into())
}

#[derive(Clone)]
pub struct Backends {
    pub session: Arc<dyn SessionBackend>,
    pub display: Arc<dyn DisplayBinding>,
    pub input: Arc<dyn InputRouter>,
    pub audio: Arc<dyn AudioRouter>,
    pub gates: Vec<Arc<dyn DeviceGate>>,
    /// Why the active set differs from the config (snake_case).
    pub fallback_notes: Vec<String>,
}

impl Backends {
    pub fn active(&self) -> ActiveBackends {
        ActiveBackends {
            session: self.session.kind(),
            display: self.display.kind(),
            input: self.input.kind(),
            audio: self.audio.kind(),
            gates: self.gates.iter().map(|g| g.kind()).collect(),
        }
    }
}

fn session_available(s: &dyn SessionBackend) -> Result<(), String> {
    match s.probe() {
        Ok(p) if p.availability.is_available() => Ok(()),
        Ok(p) => Err(reason_of(&p.availability)),
        Err(e) => Err(e.reason),
    }
}

/// Short reason of a non-available state.
pub fn reason_of(a: &Availability) -> String {
    match a {
        Availability::Available => "available".into(),
        Availability::NeedsSetup { what } => format!("needs_setup_{what}"),
        Availability::DriverNotSigned { driver } => format!("driver_not_signed_{}", kind_name(driver)),
        Availability::DriverVersionMismatch { driver, .. } => format!("driver_version_mismatch_{}", kind_name(driver)),
        Availability::Unavailable { reason } => reason.clone(),
    }
}

fn session_or_unbuilt(f: &dyn BackendFactory, k: SessionBackendKind, t: HostTopology) -> Arc<dyn SessionBackend> {
    f.session(k, t).unwrap_or_else(|| Arc::new(Unbuilt::session(k)))
}

/// Pick the active backends for these settings (boot, and after a backend change).
pub fn select(f: &dyn BackendFactory, settings: &GlobalSettings) -> Backends {
    f.configure(settings);
    let sel: &BackendSelection = &settings.backends;
    let mut notes = Vec::new();

    // Session: configured kind, else fast user switching (never an experimental fallback).
    let wanted = sel.session;
    let mut session = session_or_unbuilt(f, wanted, sel.host_topology);
    let mut problem = if wanted.is_experimental() && sel.experimental_ack != EXPERIMENTAL_WARNING_VERSION {
        Some("experimental_not_acknowledged".to_string())
    } else {
        session_available(session.as_ref()).err()
    };
    if let Some(why) = problem.take()
        && wanted != SessionBackendKind::FastUserSwitch
    {
        let fus = session_or_unbuilt(f, SessionBackendKind::FastUserSwitch, sel.host_topology);
        if session_available(fus.as_ref()).is_ok() {
            notes.push(format!("session:{}->fast_user_switch:{why}", kind_name(&wanted)));
            session = fus;
        } else {
            notes.push(format!("session:{}:{why}", kind_name(&wanted)));
            if wanted.is_experimental() && sel.experimental_ack != EXPERIMENTAL_WARNING_VERSION {
                session = Arc::new(Unbuilt::session_with(wanted, "experimental_not_acknowledged"));
            }
        }
    }
    let fus = session.kind() == SessionBackendKind::FastUserSwitch;

    // Display: configured, else window presenter, else console only.
    let display: Arc<dyn DisplayBinding> = if fus {
        if sel.display != DisplayBackendKind::ConsoleOnly {
            notes.push("display:console_only:fast_user_switch".into());
        }
        Arc::new(NoneDisplay)
    } else {
        pick(
            sel.display,
            &[DisplayBackendKind::WindowPresenter],
            |k| if k == DisplayBackendKind::ConsoleOnly { Some(Arc::new(NoneDisplay) as Arc<dyn DisplayBinding>) } else { f.display(k) },
            |d| d.availability(),
            Arc::new(NoneDisplay),
            "display",
            &mut notes,
        )
    };
    let input: Arc<dyn InputRouter> = if fus {
        Arc::new(NoneInput)
    } else {
        pick(
            sel.input,
            &[InputBackendKind::RawInputHost],
            |k| if k == InputBackendKind::None { Some(Arc::new(NoneInput) as Arc<dyn InputRouter>) } else { f.input(k) },
            |i| i.availability(),
            Arc::new(NoneInput),
            "input",
            &mut notes,
        )
    };
    let audio: Arc<dyn AudioRouter> = if fus {
        Arc::new(NoneAudio(AudioBackendKind::ConsoleShared))
    } else {
        pick(
            sel.audio,
            &[AudioBackendKind::ConsoleShared],
            |k| match k {
                AudioBackendKind::RdpBridge => f.audio(k),
                other => Some(Arc::new(NoneAudio(other)) as Arc<dyn AudioRouter>),
            },
            |a| a.availability(),
            Arc::new(NoneAudio(AudioBackendKind::None)),
            "audio",
            &mut notes,
        )
    };
    let mut gates = Vec::new();
    for k in &sel.gates {
        match f.gate(*k) {
            Some(g) if g.availability().is_available() => gates.push(g),
            Some(g) => notes.push(format!("gate:{}:{}", kind_name(k), reason_of(&g.availability()))),
            None => notes.push(format!("gate:{}:not_built", kind_name(k))),
        }
    }
    Backends { session, display, input, audio, gates, fallback_notes: notes }
}

/// Same backend kinds in every role (session, display, input, audio, gates).
pub fn same_kinds(a: &Backends, b: &Backends) -> bool {
    let gates = |x: &Backends| x.gates.iter().map(|g| g.kind()).collect::<Vec<_>>();
    a.session.kind() == b.session.kind()
        && a.display.kind() == b.display.kind()
        && a.input.kind() == b.input.kind()
        && a.audio.kind() == b.audio.kind()
        && gates(a) == gates(b)
}

#[allow(clippy::too_many_arguments)]
fn pick<K: Copy + PartialEq + Serialize, T: ?Sized>(
    wanted: K,
    fallbacks: &[K],
    make: impl Fn(K) -> Option<Arc<T>>,
    avail: impl Fn(&T) -> Availability,
    last: Arc<T>,
    what: &str,
    notes: &mut Vec<String>,
) -> Arc<T> {
    let first_reason = match make(wanted) {
        Some(b) if avail(&b).is_available() => return b,
        Some(b) => reason_of(&avail(&b)),
        None => "not_built".into(),
    };
    for fb in fallbacks.iter().copied().filter(|k| *k != wanted) {
        if let Some(b) = make(fb)
            && avail(&b).is_available()
        {
            notes.push(format!("{what}:{}->{}:{first_reason}", kind_name(&wanted), kind_name(&fb)));
            return b;
        }
    }
    notes.push(format!("{what}:{}:{first_reason}", kind_name(&wanted)));
    last
}

/// `get_backends` rows for every kind the service knows.
pub fn report(
    f: &dyn BackendFactory,
    settings: &GlobalSettings,
    active: &Backends,
) -> seat_proto::BackendReport {
    let sel = &settings.backends;
    let a = active.active();
    let info = |kind: String, availability: Availability, experimental: bool, selected: bool, is_active: bool, caps: serde_json::Value, notes: Vec<String>| BackendInfo {
        kind,
        availability,
        experimental,
        selected,
        active: is_active,
        caps,
        notes,
    };
    let session = [
        SessionBackendKind::RdsLoopback,
        SessionBackendKind::TermsrvWrapper,
        SessionBackendKind::FastUserSwitch,
        SessionBackendKind::Desktops,
    ]
    .into_iter()
    .map(|k| {
        let b = session_or_unbuilt(f, k, sel.host_topology);
        let (availability, notes) = match b.probe() {
            Ok(p) => (p.availability, p.notes),
            Err(e) => (Availability::Unavailable { reason: e.reason }, Vec::new()),
        };
        info(kind_name(&k), availability, k.is_experimental(), sel.session == k, a.session == k, serde_json::to_value(b.caps()).unwrap_or_default(), notes)
    })
    .collect();
    let display = [
        DisplayBackendKind::WindowPresenter,
        DisplayBackendKind::DisplayCore,
        DisplayBackendKind::PulsarIdd,
        DisplayBackendKind::ConsoleOnly,
    ]
    .into_iter()
    .map(|k| {
        let b: Arc<dyn DisplayBinding> = if k == DisplayBackendKind::ConsoleOnly {
            Arc::new(NoneDisplay)
        } else {
            f.display(k).unwrap_or_else(|| Arc::new(Unbuilt::display(k)))
        };
        let mut notes = Vec::new();
        if k == DisplayBackendKind::WindowPresenter {
            notes.push("no_privacy_from_console".into());
        }
        info(kind_name(&k), b.availability(), false, sel.display == k, a.display == k, serde_json::to_value(b.caps()).unwrap_or_default(), notes)
    })
    .collect();
    let input = [InputBackendKind::RawInputHost, InputBackendKind::KmdfFilter, InputBackendKind::None]
        .into_iter()
        .map(|k| {
            let b: Arc<dyn InputRouter> = if k == InputBackendKind::None {
                Arc::new(NoneInput)
            } else {
                f.input(k).unwrap_or_else(|| Arc::new(Unbuilt::input(k)))
            };
            let mut notes = Vec::new();
            if k == InputBackendKind::RawInputHost && sel.host_topology == HostTopology::SeatOneConsole {
                notes.push("input_not_isolated".into());
            }
            info(kind_name(&k), b.availability(), false, sel.input == k, a.input == k, serde_json::to_value(b.caps()).unwrap_or_default(), notes)
        })
        .collect();
    let audio = [AudioBackendKind::RdpBridge, AudioBackendKind::ConsoleShared, AudioBackendKind::None]
        .into_iter()
        .map(|k| {
            let b: Arc<dyn AudioRouter> = match k {
                AudioBackendKind::RdpBridge => f.audio(k).unwrap_or_else(|| Arc::new(Unbuilt::audio(k))),
                other => Arc::new(NoneAudio(other)),
            };
            let notes = if b.caps().console_sees_all { vec!["seat_one_sees_other_endpoints".into()] } else { Vec::new() };
            info(kind_name(&k), b.availability(), false, sel.audio == k, a.audio == k, serde_json::to_value(b.caps()).unwrap_or_default(), notes)
        })
        .collect();
    let gates = [
        GateBackendKind::DeviceSecurity,
        GateBackendKind::SessionDriveLetters,
        GateBackendKind::CameraConsent,
        GateBackendKind::StorageMinifilter,
        GateBackendKind::PrinterAcl,
    ]
    .into_iter()
    .map(|k| {
        let b = f.gate(k).unwrap_or_else(|| Arc::new(Unbuilt::gate(k)));
        info(kind_name(&k), b.availability(), false, sel.gates.contains(&k), a.gates.contains(&k), serde_json::to_value(b.caps()).unwrap_or_default(), Vec::new())
    })
    .collect();
    seat_proto::BackendReport { session, display, input, audio, gates, drivers: Vec::new() }
}

// ---------------------------------------------------------------------------------------
// Built-in backends

fn unsupported<T>(what: &str) -> BackendResult<T> {
    Err(BackendError::unsupported(what))
}

/// `console_only`: no per-seat display (fast user switching shows the console everywhere).
pub struct NoneDisplay;

impl DisplayBinding for NoneDisplay {
    fn kind(&self) -> DisplayBackendKind {
        DisplayBackendKind::ConsoleOnly
    }
    fn caps(&self) -> DisplayCaps {
        DisplayCaps { isolates_console: false, multi_monitor: false, clone: false, hardware_cursor: true, monitor_power: false, driver: None }
    }
    fn availability(&self) -> Availability {
        Availability::Available
    }
    fn enumerate(&self) -> BackendResult<Vec<Device>> {
        Ok(Vec::new())
    }
    fn apply_topology(&self, _plan: &TopologyPlan) -> BackendResult<TopologyReport> {
        Ok(TopologyReport::default())
    }
    fn bind_seat(&self, seat: SeatNumber, _targets: &[DeviceKey]) -> BackendResult<SeatOutputs> {
        Ok(SeatOutputs { seat, outputs: Vec::new() })
    }
    fn release_seat(&self, _seat: SeatNumber) -> BackendResult<()> {
        Ok(())
    }
    fn identify(&self, _t: &[(DeviceKey, String)], _on: bool) -> BackendResult<()> {
        unsupported("monitor identification")
    }
    fn set_power(&self, _k: &DeviceKey, _on: bool) -> BackendResult<()> {
        unsupported("monitor power")
    }
    fn set_hotplug_sink(&self, _s: HotplugSink) {}
}

/// `none`: no local input routing (every unit stays with the console).
pub struct NoneInput;

impl InputRouter for NoneInput {
    fn kind(&self) -> InputBackendKind {
        InputBackendKind::None
    }
    fn caps(&self) -> InputCaps {
        InputCaps { isolates_console: false, hid_visibility: false, activity: false, hotkey: false, wizard_capture: false, driver: None }
    }
    fn availability(&self) -> Availability {
        Availability::Available
    }
    fn enumerate(&self) -> BackendResult<Vec<Device>> {
        Ok(Vec::new())
    }
    fn apply_routes(&self, routes: &[InputRoute]) -> BackendResult<RouteReport> {
        // Every unit not on seat 1 alone cannot be routed.
        let not_routable = routes.iter().filter(|r| r.mask.single() != Some(SeatNumber::CONSOLE)).map(|r| r.key.clone()).collect();
        Ok(RouteReport { applied: Vec::new(), not_routable, needs_replug: Vec::new() })
    }
    fn set_wizard_capture(&self, _on: bool) -> BackendResult<()> {
        unsupported("wizard capture")
    }
    fn set_hotplug_sink(&self, _s: HotplugSink) {}
    fn set_activity_sink(&self, _s: ActivitySink) {}
    fn set_packet_sink(&self, _s: PacketSink) {}
    fn set_hotkey_sink(&self, _s: HotkeySink) {}
}

/// `console_shared` / `none` audio: no per-seat routing.
pub struct NoneAudio(pub AudioBackendKind);

impl AudioRouter for NoneAudio {
    fn kind(&self) -> AudioBackendKind {
        self.0
    }
    fn caps(&self) -> AudioCaps {
        AudioCaps { per_seat_render: false, per_seat_capture: false, console_sees_all: true, test_tone: false }
    }
    fn availability(&self) -> Availability {
        Availability::Available
    }
    fn enumerate(&self) -> BackendResult<Vec<Device>> {
        Ok(Vec::new())
    }
    fn bind_seat(&self, seat: SeatNumber, _b: &AudioBinding) -> BackendResult<AudioRoute> {
        let note = if self.0 == AudioBackendKind::ConsoleShared { "audio_console_shared" } else { "audio_off" };
        Ok(AudioRoute { seat: Some(seat), render_endpoint_id: None, capture_endpoint_id: None, notes: vec![note.into()] })
    }
    fn release_seat(&self, _seat: SeatNumber) -> BackendResult<()> {
        Ok(())
    }
    fn test_tone(&self, _k: &DeviceKey, _ms: u32) -> BackendResult<()> {
        unsupported("test tone")
    }
    fn set_hotplug_sink(&self, _s: HotplugSink) {}
}

/// Placeholder for a kind this build does not provide.
pub struct Unbuilt {
    reason: String,
    session: SessionBackendKind,
    display: DisplayBackendKind,
    input: InputBackendKind,
    audio: AudioBackendKind,
    gate: GateBackendKind,
}

impl Unbuilt {
    fn base() -> Self {
        Self {
            reason: "not_built".into(),
            session: SessionBackendKind::RdsLoopback,
            display: DisplayBackendKind::WindowPresenter,
            input: InputBackendKind::RawInputHost,
            audio: AudioBackendKind::RdpBridge,
            gate: GateBackendKind::DeviceSecurity,
        }
    }
    pub fn session(k: SessionBackendKind) -> Self {
        Self { session: k, ..Self::base() }
    }
    pub fn session_with(k: SessionBackendKind, reason: &str) -> Self {
        Self { session: k, reason: reason.into(), ..Self::base() }
    }
    pub fn display(k: DisplayBackendKind) -> Self {
        Self { display: k, ..Self::base() }
    }
    pub fn input(k: InputBackendKind) -> Self {
        Self { input: k, ..Self::base() }
    }
    pub fn audio(k: AudioBackendKind) -> Self {
        Self { audio: k, ..Self::base() }
    }
    pub fn gate(k: GateBackendKind) -> Self {
        Self { gate: k, ..Self::base() }
    }
    fn unavailable(&self) -> Availability {
        Availability::Unavailable { reason: self.reason.clone() }
    }
    fn err<T>(&self) -> BackendResult<T> {
        Err(BackendError::new(ErrorKind::NotReady, self.reason.clone(), "backend not available in this build"))
    }
}

impl SessionBackend for Unbuilt {
    fn kind(&self) -> SessionBackendKind {
        self.session
    }
    fn caps(&self) -> SessionCaps {
        SessionCaps {
            concurrent: false,
            max_seats: 1,
            per_user: false,
            needs_seat_host: false,
            remote_display: false,
            remote_input: false,
            remote_audio: false,
            auto_logon: false,
            stop_one: false,
        }
    }
    fn probe(&self) -> BackendResult<HostProbe> {
        Ok(HostProbe {
            kind: self.session,
            availability: self.unavailable(),
            edition: String::new(),
            build: 0,
            arch: std::env::consts::ARCH.into(),
            notes: Vec::new(),
        })
    }
    fn prepare_host(&self, _s: &GlobalSettings) -> BackendResult<Vec<String>> {
        Ok(Vec::new())
    }
    fn restore_host(&self) -> BackendResult<()> {
        Ok(())
    }
    fn start(&self, _r: &SeatStartRequest<'_>) -> BackendResult<StartStep> {
        self.err()
    }
    fn wait_session(&self, _s: SeatNumber, _c: &str, _t: Duration) -> BackendResult<SessionInfo> {
        self.err()
    }
    fn query(&self, _s: SeatNumber, _id: u32) -> BackendResult<SessionInfo> {
        self.err()
    }
    fn log_off(&self, _s: SeatNumber, _id: u32) -> BackendResult<()> {
        self.err()
    }
    fn adopt(&self, _k: &[KnownSession]) -> BackendResult<Vec<(SeatNumber, SessionInfo)>> {
        Ok(Vec::new())
    }
    fn enumerate(&self) -> BackendResult<Vec<SessionInfo>> {
        Ok(Vec::new())
    }
}

impl DisplayBinding for Unbuilt {
    fn kind(&self) -> DisplayBackendKind {
        self.display
    }
    fn caps(&self) -> DisplayCaps {
        NoneDisplay.caps()
    }
    fn availability(&self) -> Availability {
        self.unavailable()
    }
    fn enumerate(&self) -> BackendResult<Vec<Device>> {
        Ok(Vec::new())
    }
    fn apply_topology(&self, _p: &TopologyPlan) -> BackendResult<TopologyReport> {
        self.err()
    }
    fn bind_seat(&self, _s: SeatNumber, _t: &[DeviceKey]) -> BackendResult<SeatOutputs> {
        self.err()
    }
    fn release_seat(&self, _s: SeatNumber) -> BackendResult<()> {
        Ok(())
    }
    fn identify(&self, _t: &[(DeviceKey, String)], _on: bool) -> BackendResult<()> {
        self.err()
    }
    fn set_power(&self, _k: &DeviceKey, _on: bool) -> BackendResult<()> {
        self.err()
    }
    fn set_hotplug_sink(&self, _s: HotplugSink) {}
}

impl InputRouter for Unbuilt {
    fn kind(&self) -> InputBackendKind {
        self.input
    }
    fn caps(&self) -> InputCaps {
        NoneInput.caps()
    }
    fn availability(&self) -> Availability {
        self.unavailable()
    }
    fn enumerate(&self) -> BackendResult<Vec<Device>> {
        Ok(Vec::new())
    }
    fn apply_routes(&self, _r: &[InputRoute]) -> BackendResult<RouteReport> {
        self.err()
    }
    fn set_wizard_capture(&self, _on: bool) -> BackendResult<()> {
        self.err()
    }
    fn set_hotplug_sink(&self, _s: HotplugSink) {}
    fn set_activity_sink(&self, _s: ActivitySink) {}
    fn set_packet_sink(&self, _s: PacketSink) {}
    fn set_hotkey_sink(&self, _s: HotkeySink) {}
}

impl AudioRouter for Unbuilt {
    fn kind(&self) -> AudioBackendKind {
        self.audio
    }
    fn caps(&self) -> AudioCaps {
        NoneAudio(self.audio).caps()
    }
    fn availability(&self) -> Availability {
        self.unavailable()
    }
    fn enumerate(&self) -> BackendResult<Vec<Device>> {
        Ok(Vec::new())
    }
    fn bind_seat(&self, _s: SeatNumber, _b: &AudioBinding) -> BackendResult<AudioRoute> {
        self.err()
    }
    fn release_seat(&self, _s: SeatNumber) -> BackendResult<()> {
        Ok(())
    }
    fn test_tone(&self, _k: &DeviceKey, _ms: u32) -> BackendResult<()> {
        self.err()
    }
    fn set_hotplug_sink(&self, _s: HotplugSink) {}
}

impl DeviceGate for Unbuilt {
    fn kind(&self) -> GateBackendKind {
        self.gate
    }
    fn caps(&self) -> GateCaps {
        GateCaps { kinds: Vec::new(), revokes_handles: false, driver: None }
    }
    fn availability(&self) -> Availability {
        self.unavailable()
    }
    fn enumerate(&self) -> BackendResult<Vec<Device>> {
        Ok(Vec::new())
    }
    fn apply(&self, _p: &GatePlan) -> BackendResult<GateReport> {
        self.err()
    }
    fn revert_all(&self) -> BackendResult<()> {
        Ok(())
    }
    fn set_hotplug_sink(&self, _s: HotplugSink) {}
}

#[cfg(test)]
pub mod testing {
    //! Factory over the seat-core mocks.
    use super::*;
    use seat_core::mock::{MockAudio, MockDisplay, MockGate, MockInput, MockSessionBackend};

    pub struct MockFactory {
        pub session: Arc<MockSessionBackend>,
        pub fus: Arc<MockSessionBackend>,
        pub desktops: Arc<MockSessionBackend>,
        pub display: Arc<MockDisplay>,
        pub input: Arc<MockInput>,
        pub audio: Arc<MockAudio>,
        pub gate: Arc<MockGate>,
        /// `console_listener` answer (tests of `crate::listener`).
        pub listener: std::sync::atomic::AtomicBool,
    }

    impl Default for MockFactory {
        fn default() -> Self {
            Self {
                session: Arc::new(MockSessionBackend::new(SessionBackendKind::RdsLoopback)),
                fus: Arc::new(MockSessionBackend::new(SessionBackendKind::FastUserSwitch)),
                desktops: Arc::new(MockSessionBackend::new(SessionBackendKind::Desktops)),
                display: Arc::new(MockDisplay::default()),
                input: Arc::new(MockInput::default()),
                audio: Arc::new(MockAudio::default()),
                gate: Arc::new(MockGate::new(GateBackendKind::DeviceSecurity)),
                listener: std::sync::atomic::AtomicBool::new(false),
            }
        }
    }

    impl BackendFactory for MockFactory {
        fn session(&self, kind: SessionBackendKind, _t: HostTopology) -> Option<Arc<dyn SessionBackend>> {
            match kind {
                SessionBackendKind::RdsLoopback => Some(self.session.clone()),
                SessionBackendKind::FastUserSwitch => Some(self.fus.clone()),
                SessionBackendKind::Desktops => Some(self.desktops.clone()),
                _ => None,
            }
        }
        fn display(&self, kind: DisplayBackendKind) -> Option<Arc<dyn DisplayBinding>> {
            (kind == DisplayBackendKind::WindowPresenter).then(|| self.display.clone() as Arc<dyn DisplayBinding>)
        }
        fn input(&self, kind: InputBackendKind) -> Option<Arc<dyn InputRouter>> {
            (kind == InputBackendKind::RawInputHost).then(|| self.input.clone() as Arc<dyn InputRouter>)
        }
        fn audio(&self, kind: AudioBackendKind) -> Option<Arc<dyn AudioRouter>> {
            (kind == AudioBackendKind::RdpBridge).then(|| self.audio.clone() as Arc<dyn AudioRouter>)
        }
        fn gate(&self, kind: GateBackendKind) -> Option<Arc<dyn DeviceGate>> {
            (kind == GateBackendKind::DeviceSecurity).then(|| self.gate.clone() as Arc<dyn DeviceGate>)
        }
        fn console_listener(&self, kind: InputBackendKind) -> bool {
            kind == InputBackendKind::RawInputHost && self.listener.load(std::sync::atomic::Ordering::SeqCst)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::MockFactory;
    use super::*;

    #[test]
    fn defaults_select_configured() {
        let f = MockFactory::default();
        let b = select(&f, &GlobalSettings::default());
        assert_eq!(b.session.kind(), SessionBackendKind::RdsLoopback);
        assert_eq!(b.display.kind(), DisplayBackendKind::WindowPresenter);
        assert_eq!(b.gates.len(), 1);
        assert!(b.fallback_notes.iter().any(|n| n.starts_with("gate:session_drive_letters")));
    }

    #[test]
    fn unavailable_session_falls_back_to_fus() {
        let f = MockFactory::default();
        *f.session.availability.lock().unwrap() = Availability::Unavailable { reason: "edition_unsupported".into() };
        let b = select(&f, &GlobalSettings::default());
        assert_eq!(b.session.kind(), SessionBackendKind::FastUserSwitch);
        assert_eq!(b.display.kind(), DisplayBackendKind::ConsoleOnly);
        assert_eq!(b.input.kind(), InputBackendKind::None);
        assert!(b.fallback_notes.contains(&"session:rds_loopback->fast_user_switch:edition_unsupported".to_string()));
    }

    #[test]
    fn experimental_needs_ack_and_is_never_a_fallback() {
        let f = MockFactory::default();
        let mut g = GlobalSettings::default();
        g.backends.session = SessionBackendKind::TermsrvWrapper;
        let b = select(&f, &g);
        assert_eq!(b.session.kind(), SessionBackendKind::FastUserSwitch);
        assert!(b.fallback_notes[0].ends_with("experimental_not_acknowledged"));
    }

    #[test]
    fn report_lists_every_kind() {
        let f = MockFactory::default();
        let g = GlobalSettings::default();
        let b = select(&f, &g);
        let r = report(&f, &g, &b);
        assert_eq!(r.session.len(), 4);
        assert!(r.session.iter().any(|i| i.kind == "rds_loopback" && i.active && i.selected));
        assert!(r.input.iter().any(|i| i.notes.contains(&"input_not_isolated".to_string())));
        assert_eq!(r.display.iter().find(|i| i.kind == "pulsar_idd").unwrap().availability, Availability::Unavailable { reason: "not_built".into() });
    }
}
