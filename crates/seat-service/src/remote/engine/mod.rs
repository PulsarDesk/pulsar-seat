// SPDX-License-Identifier: GPL-3.0-only
//! Seat orchestration (contracts §1.3, §2.3, §3.3, §4.4, §5.4, §6, §7.3, §8.3, §9.4).
//!
//! The [`Engine`] owns no Windows code: it drives the packages through their public traits
//! (`SessionManager`, `DisplayBackend`, `AudioProbe`, `StreamBackend` / `SeatStream`, `PadManager`,
//! `SeatMapWriter`, `Termwrap`) plus the small [`IsolationPort`] defined here. The real
//! implementations are assembled in `crate::remote::win::wiring`; the tests use the packages' fakes.
//!
//! Seat start runs on a worker thread per seat (start order: user → session → display →
//! audio keys → stream → pads → isolation → autostart). Every step that changed something
//! is undone in reverse when a later step fails or the seat stops. Progress goes out as
//! [`SeatEvent`]s that the controller drains ([`Engine::take_reports`]).
//!
//! Locks: `seats` is never held across a blocking package call. Each seat's stream server
//! has its own mutex (tick / pairing / apps reload).

use seat_audio::{AudioProbe, AudioState, SeatAudio, rdp_client_flags, seat_audio_status, server_keys, session_endpoint};
use crate::remote::control::SeatEvent;
use crate::remote::wrds::{HostPlan, WrdsConnect, WrdsPort, WrdsSession, plan, wants_provider};
use seat_display::remote::backend::pulsar_idd::OWN_DRIVER_SIGNED;
use seat_display::remote::{
    AdapterChoice, AdapterEnumerator, BackendInfo, BackendKind, DisplayBackend, DisplayError, HdrLuminance, HdrUnavailable,
    ModeRequest, ModeResult, ModeStatus, SeatDisplay, SeatDisplayHandle, choose_backend, resolve_adapter,
};
use seat_pads::{BackendId, PadManager, PadReleased, PadWanted, SeatPadContext};
use seat_proto::remote::{
    DisplayBackendPref, ErrorCode, RemoteSettings, HostInfo, IsolationStatus, PadBackendPref, PairMode, PairState, PairedClient,
    PendingPair, ProtoError, RemoteSeatRecord, SeatRuntime, Secret, ShimTargetDto, StopAction, StreamBackendInfo, StreamBackendKind,
};
use seat_session::remote::launch::{DEFAULT_DESKTOP, LaunchSpec, TokenKind};
use seat_session::remote::manager::{CancelToken, SeatSession, SessionChange, SessionManager, SessionNotice, SessionRequest};
use seat_shim_core::{SeatFlags, SeatMapEntry, SeatMapWriter, ShimHealth, ShimTarget};
use seat_stream::StreamError;
use seat_stream::apps::{AppEntry, default_apps, parse_apps_json, render_apps_json, validate_apps, wrap_with_launcher};
use seat_stream::backend::{PeerEvent, Prepared, SeatStream, SeatStreamSpec, StreamBackend, availability_all, find_backend};
use seat_stream::control::ServerEvent;
use seat_stream::seatdir::{SecureFs, SeatDir};
use seat_stream::supervise::StreamNotice;
use seat_session::termwrap::{Termwrap, TermwrapState};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

#[cfg(test)]
mod tests;

/// Seat session size requested from the RDP server before the client asks for a mode.
pub const DEFAULT_SIZE: (u32, u32) = (1920, 1080);
/// How long shutdown / termwrap wait for seats to finish stopping.
pub const STOP_WAIT: Duration = Duration::from_secs(60);
/// Most health entries kept (rate limit for the health pipe).
pub const MAX_HEALTH: usize = 256;
/// Bluetooth re-check period (spec 60 §10.4); session changes trigger one sooner.
pub const BT_EVERY_MS: u64 = 60_000;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn perr(e: impl Into<ProtoError>) -> ProtoError {
    e.into().with_reason_from_message()
}

fn err(code: ErrorCode, reason: &str, msg: impl std::fmt::Display) -> ProtoError {
    ProtoError::new(code, format!("{reason}: {msg}")).with_reason(reason)
}

/// Isolation operations the service needs (spec 50; contracts §4.4). Real: AppHelp
/// installer + `HKLM\SOFTWARE\PulsarSplit\Shim` + DXVK deployer (`crate::remote::win::wiring`).
pub trait IsolationPort: Send + Sync {
    fn status(&self) -> Result<IsolationStatus, ProtoError>;
    /// Install the AppHelp database for the stored target list and copy the modules.
    fn install(&self) -> Result<(), ProtoError>;
    fn uninstall(&self) -> Result<(), ProtoError>;
    /// Store the target list; rebuilds the database when installed.
    fn set_targets(&self, targets: &[ShimTarget]) -> Result<(), ProtoError>;
    fn set_mask(&self, exe_lower: &str, enabled: bool) -> Result<(), ProtoError>;
    fn dxvk(&self, game: &Path, enabled: bool, dlls: &[String]) -> Result<(), ProtoError>;
    /// Pinned DXVK release + the games with a DXVK redirect (spec 50 §10.2).
    fn dxvk_status(&self) -> Result<seat_proto::remote::DxvkStatus, ProtoError>;
    /// `pulsar-split-launch.exe` when shipped (x64 hosts only).
    fn launcher_path(&self) -> Option<PathBuf>;
    /// Seat-map kill switch etc. live in the module; nothing else needed here.
    fn restore_all(&self) -> Result<(), ProtoError>;
}

/// Stream servers of the seats (spec 80, docs/split/agents/pulsar-host-backend.md).
pub struct StreamEnv {
    /// Seat folder access (`apps.json`, logs, seat folder removal).
    pub fs: Arc<dyn SecureFs>,
    /// One per kind (`seat_stream::backend::SunshineBackend`, `PulsarHostBackend`). Each
    /// seat uses the one named by `RemoteSeatRecord::stream_backend`; a kind without an entry
    /// fails the start with `backend_missing`.
    pub backends: Vec<Arc<dyn StreamBackend>>,
}

/// A seat's running stream server (own lock: tick / pairing / apps reload).
type Server = Arc<Mutex<Box<dyn SeatStream>>>;

pub struct Deps {
    pub session: Arc<dyn SessionManager>,
    pub displays: Vec<Arc<dyn DisplayBackend>>,
    pub adapters: Arc<dyn AdapterEnumerator>,
    pub audio: Arc<dyn AudioProbe>,
    pub stream: StreamEnv,
    /// `None` when the pad manager could not be built (logged; seats run without pads).
    pub pads: Option<Arc<Mutex<PadManager>>>,
    /// Bluetooth per-user service handling (spec 60 §10.4, global `bluetooth_console_only`).
    pub bluetooth: Option<Arc<Mutex<seat_pads::bluetooth::BtController>>>,
    /// The pad manager's feedback sink when it routes local seats' virtual pads back to
    /// their physical pads (seat_input::local_pads, round 4). None: no local pads.
    pub local_pad_feedback: Option<Arc<seat_input::local_pads::LocalFeedbackRouter>>,
    pub isolation: Arc<dyn IsolationPort>,
    pub seatmap: Arc<dyn SeatMapWriter>,
    pub termwrap: Arc<dyn Termwrap>,
    /// Coexistence evaluation (spec 90 §8), JSON of `seat_setup::CoexistReport`.
    pub coexist: Arc<dyn Fn() -> CoexistOutcome + Send + Sync>,
    /// Session attached to the physical console, if any.
    pub console_session: Arc<dyn Fn() -> Option<u32> + Send + Sync>,
    /// Spec 70 §2.6 operator-triggered stale RDP audio endpoint cleanup
    /// (`seat_audio::remove_stale_endpoints` over the real probe, remover and WTS list).
    pub audio_cleanup: Arc<dyn Fn() -> Result<seat_audio::StaleReport, seat_audio::AudioError> + Send + Sync>,
    /// Our Remote Desktop protocol provider (remote seats on the own IddCx display,
    /// `crate::remote::wrds`). `None` = not available on this build / host.
    pub wrds: Option<Arc<dyn WrdsPort>>,
    /// `%ProgramData%\PulsarSplit`.
    pub data_root: PathBuf,
    pub service_version: String,
}

/// Result of the coexistence check.
#[derive(Debug, Clone, Default)]
pub struct CoexistOutcome {
    pub passive: bool,
    pub report: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Booting,
    Running,
    Stopping,
}

struct DisplayRun {
    backend: Arc<dyn DisplayBackend>,
    handle: SeatDisplayHandle,
    hdr: Result<(), HdrUnavailable>,
}

#[derive(Default)]
struct Resources {
    session: Option<SeatSession>,
    /// The session came through our protocol provider (ended through it, not the hold).
    wrds: Option<WrdsSession>,
    /// The provider path was wanted but not taken: why (manager runtime display info).
    fallback: Option<String>,
    display: Option<DisplayRun>,
    server: Option<Server>,
    pads: bool,
    seatmap: Option<u32>,
    autostart: bool,
}

struct SeatRun {
    generation: u64,
    seat: RemoteSeatRecord,
    global: RemoteSettings,
    cancel: CancelToken,
    stage: Stage,
    res: Resources,
    pair: PairState,
    audio_report: Option<AudioState>,
    map_generation: u32,
    map_prefixes: Vec<String>,
}

#[derive(Debug)]
struct Failure {
    reason: String,
    message: String,
}

impl Failure {
    fn new(reason: &str, message: impl std::fmt::Display) -> Self {
        Self { reason: reason.into(), message: format!("{reason}: {message}") }
    }
    fn cancelled() -> Self {
        Self::new("cancelled", "seat stop requested during start")
    }
}

struct Inner {
    d: Deps,
    seats: Mutex<HashMap<String, SeatRun>>,
    reports: Mutex<Vec<(String, SeatEvent)>>,
    health: Mutex<HashMap<(u32, String), ShimHealth>>,
    next_gen: AtomicU64,
    passive: AtomicBool,
    coexist: Mutex<Option<serde_json::Value>>,
    host_class: Mutex<Option<String>>,
    workers: Mutex<Vec<std::thread::JoinHandle<()>>>,
    /// Next Bluetooth refresh (ms, tick clock); 0 = at the next tick.
    bt_next_ms: AtomicU64,
    /// USB ids of the PC's physical pads (the service's inventory), for the SDL ignore list.
    physical_pads: Mutex<Option<PadIdSource>>,
    /// Seat accounts known at service start (+ seats started since), for the provider
    /// listener's logon list.
    seat_users: Mutex<Vec<String>>,
}

/// Source of the physical pads' USB ids (`Engine::set_physical_pads`).
pub type PadIdSource = Arc<dyn Fn() -> Vec<(u16, u16)> + Send + Sync>;

/// Cheap to clone; every clone drives the same seats.
#[derive(Clone)]
pub struct Engine(Arc<Inner>);

fn to_kind(p: DisplayBackendPref) -> BackendKind {
    match p {
        DisplayBackendPref::RdpSession => BackendKind::RdpSession,
        DisplayBackendPref::Vdd => BackendKind::Vdd,
        DisplayBackendPref::PulsarIdd => BackendKind::PulsarIdd,
    }
}

pub fn to_backend_id(p: PadBackendPref) -> BackendId {
    match p {
        PadBackendPref::Vigem => BackendId::Vigem,
        PadBackendPref::UsbipDs5 => BackendId::UsbipDs5,
        PadBackendPref::PulsarPad => BackendId::PulsarPad,
    }
}

fn seat_audio(seat: &RemoteSeatRecord) -> SeatAudio {
    SeatAudio { stream_audio: seat.audio.stream_audio, microphone: seat.audio.microphone }
}

/// Whether the seat's backend can deliver a client microphone (spec/remote 70 §5.1).
fn mic_path(seat: &RemoteSeatRecord) -> bool {
    seat.stream_backend == StreamBackendKind::Pulsar
}

/// Client mic frames of `seat_id` (from its `pulsar-host` mic pipe) go to its hold's stdin.
fn mic_sink(session: Arc<dyn SessionManager>, seat_id: String) -> seat_stream::backend::pulsar::mic::MicSink {
    Arc::new(move |f: &seat_core::remote::mic::MicFrame| {
        // Dropped when the hold is gone or its queue is full (never blocks the reader).
        let _ = session.send_mic(&seat_id, f);
    })
}

pub fn to_target(t: &ShimTargetDto) -> ShimTarget {
    ShimTarget { exe_name: t.exe_name.clone(), product_name: t.product_name.clone(), path: t.path.as_ref().map(PathBuf::from) }
}

pub fn from_target(t: &ShimTarget) -> ShimTargetDto {
    ShimTargetDto {
        exe_name: t.exe_name.clone(),
        product_name: t.product_name.clone(),
        path: t.path.as_ref().map(|p| p.display().to_string()),
    }
}

fn json<T: serde::Serialize>(v: &T) -> serde_json::Value {
    serde_json::to_value(v).unwrap_or(serde_json::Value::Null)
}

/// Session size at start: the seat's fixed resolution, else its last applied mode, else
/// [`DEFAULT_SIZE`].
pub fn session_size(seat: &RemoteSeatRecord) -> (u32, u32) {
    if let Some(s) = seat.display.fixed_size() {
        return s;
    }
    match seat.last_mode {
        Some(m) if m.is_plausible() => (m.width, m.height),
        _ => DEFAULT_SIZE,
    }
}

/// A mode request after the seat's own display settings (2026-11-17): a fixed resolution
/// replaces the asked size, a locked refresh the asked rate. Everything else (clamps,
/// super-sampling, scale, HDR gate) is the backend's `resolve_mode`.
pub fn seat_mode_request(d: &seat_proto::remote::DisplaySettings, mut req: ModeRequest) -> ModeRequest {
    if let Some((w, h)) = d.fixed_size() {
        req.width = w;
        req.height = h;
    }
    if d.lock_refresh {
        req.hz = d.refresh_hz;
    }
    req
}

fn mode_status_for(e: &DisplayError) -> ModeStatus {
    match e {
        DisplayError::Timeout(_) => ModeStatus::Timeout,
        DisplayError::NoDisplay(_) => ModeStatus::NoDisplay,
        DisplayError::Unsupported(_) | DisplayError::SessionAttachUnsupported(_) => ModeStatus::Unsupported,
        _ => ModeStatus::Rejected,
    }
}

/// Splits an autostart argument string the way `CommandLineToArgvW` does for the simple
/// cases (whitespace separates, double quotes group, `\"` is a literal quote).
pub fn split_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut any = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
                any = true;
            }
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            c => {
                cur.push(c);
                any = true;
            }
        }
    }
    if any {
        out.push(cur);
    }
    out
}

/// USB ids every remote seat's own virtual pads can have (they must stay visible to SDL).
fn virtual_pad_ids() -> Vec<(u16, u16)> {
    use seat_pads::PadKind;
    [PadKind::X360, PadKind::XOne, PadKind::Ds4, PadKind::Ds5, PadKind::Ds5Edge].iter().map(|k| k.usb_ids()).collect()
}

/// Environment of the processes the service starts in a remote seat: the Steam IPC
/// override (when on) and the SDL ignore list: the seat's `pad_ignore_devices` plus the USB
/// ids of the PC's physical pads (parity review 2026-11-17; device scoping is opt-in and
/// the module does not cover every app, so a remote seat's SDL games could otherwise list
/// host pads). An id one of the seat's virtual pad kinds shares stays visible (SDL matches
/// by VID/PID only).
fn seat_env(seat_id: &str, steam: bool, pad_ignore: &[String], physical: &[(u16, u16)]) -> Vec<(String, String)> {
    let mut env = if steam { seat_shim_core::steam_env(seat_id) } else { Vec::new() };
    let list = seat_input::pad_scope::sdl_ignore_list(pad_ignore, physical, &virtual_pad_ids());
    env.extend(seat_input::pad_scope::sdl_env(&list));
    env
}

impl Engine {
    pub fn new(d: Deps) -> Self {
        Engine(Arc::new(Inner {
            d,
            seats: Mutex::new(HashMap::new()),
            reports: Mutex::new(Vec::new()),
            health: Mutex::new(HashMap::new()),
            next_gen: AtomicU64::new(1),
            passive: AtomicBool::new(false),
            coexist: Mutex::new(None),
            host_class: Mutex::new(None),
            workers: Mutex::new(Vec::new()),
            bt_next_ms: AtomicU64::new(0),
            physical_pads: Mutex::new(None),
            seat_users: Mutex::new(Vec::new()),
        }))
    }

    /// Where the SDL ignore list of remote seats gets the PC's physical pad ids from (the
    /// service's device inventory; set once at service start).
    pub fn set_physical_pads(&self, f: PadIdSource) {
        *self.0.physical_pads.lock().unwrap_or_else(|e| e.into_inner()) = Some(f);
    }

    fn physical_pad_ids(&self) -> Vec<(u16, u16)> {
        let f = self.0.physical_pads.lock().unwrap_or_else(|e| e.into_inner()).clone();
        f.map(|f| f()).unwrap_or_default()
    }

    fn d(&self) -> &Deps {
        &self.0.d
    }

    fn report(&self, seat_id: &str, ev: SeatEvent) {
        lock(&self.0.reports).push((seat_id.to_string(), ev));
    }

    fn notice(&self, seat_id: &str, kind: &str, data: serde_json::Value) {
        self.report(seat_id, SeatEvent::Notice { kind: kind.into(), data });
    }

    pub fn take_reports(&self) -> Vec<(String, SeatEvent)> {
        std::mem::take(&mut *lock(&self.0.reports))
    }

    fn spawn(&self, name: &str, f: impl FnOnce() + Send + 'static) {
        let h = std::thread::Builder::new().name(name.into()).spawn(f);
        match h {
            Ok(h) => {
                let mut w = lock(&self.0.workers);
                w.retain(|h| !h.is_finished());
                w.push(h);
            }
            Err(e) => log::error!("cannot start worker thread {name}: {e}"),
        }
    }

    pub fn passive(&self) -> bool {
        self.0.passive.load(Ordering::SeqCst)
    }

    pub fn host_class(&self) -> Option<String> {
        lock(&self.0.host_class).clone()
    }

    /// Seats booting, running or stopping in the engine.
    pub fn active_seats(&self) -> usize {
        lock(&self.0.seats).len()
    }

    /// Waits until no seat is left (all stops finished) or `timeout` passes.
    pub fn wait_idle(&self, timeout: Duration) -> bool {
        let end = Instant::now() + timeout;
        loop {
            if self.active_seats() == 0 {
                return true;
            }
            if Instant::now() >= end {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    // ---- service lifecycle (contracts §9.4) ----

    /// Service start: coexistence → termwrap boot check → host info → RDS prepare (if
    /// multi-seat is on and not passive) → RDS ready → sweep → pad orphans → seat map clear.
    /// Blocking (up to the RDS ready timeout); run it off the pipe thread.
    pub fn service_start(&self, global: &RemoteSettings, seat_users: &[String]) {
        *lock(&self.0.seat_users) = seat_users.to_vec();
        let co = (self.d().coexist)();
        self.0.passive.store(co.passive, Ordering::SeqCst);
        *lock(&self.0.coexist) = co.report;
        if co.passive {
            log::warn!("coexistence: passive mode, no system changes and no seat starts");
            return;
        }
        let wrapper_active = match self.d().termwrap.boot_check() {
            Ok(st) => {
                if let TermwrapState::Blocked { reason } = &st.state {
                    log::warn!("experimental multi-session switched off at boot: {reason:?}");
                }
                matches!(st.state, TermwrapState::On { .. })
            }
            Err(e) => {
                log::warn!("termwrap boot check: {e}");
                false
            }
        };
        match self.d().session.host_info(wrapper_active) {
            Ok(h) => *lock(&self.0.host_class) = serde_json::to_value(h.class).ok().and_then(|v| v.as_str().map(String::from)),
            Err(e) => log::warn!("host info: {e}"),
        }
        if global.multi_session {
            match self.d().session.prepare_host(wrapper_active) {
                Ok(r) => log::info!("RDS settings prepared: {r:?}"),
                Err(e) => log::error!("RDS settings not prepared: {e}"),
            }
            if let Err(e) = self.d().session.wait_rds_ready() {
                log::error!("remote desktop not ready: {e}");
            }
        }
        if let Err(e) = self.d().session.sweep(seat_users) {
            log::warn!("session sweep: {e}");
        }
        if let Some(p) = &self.d().pads {
            match lock(p).cleanup_orphans() {
                Ok(r) => log::info!("pad orphans: {} devices removed", r.removed_devices.len()),
                Err(e) => log::warn!("pad orphan cleanup: {e}"),
            }
        }
        if let Err(e) = self.d().seatmap.clear() {
            log::warn!("seat map clear: {e}");
        }
        self.set_bluetooth_console_only(global.bluetooth_console_only);
        self.sync_wrds(global);
    }

    // ---- our protocol provider (crate::remote::wrds) ----

    /// Register the provider while remote seats want our display, remove it otherwise.
    /// Passive (coexistence) mode never changes it.
    pub fn sync_wrds(&self, global: &RemoteSettings) {
        let Some(w) = &self.d().wrds else { return };
        if self.passive() {
            return;
        }
        let want = wants_provider(global.display_backend.map(to_kind), OWN_DRIVER_SIGNED);
        let users = lock(&self.0.seat_users).clone();
        match w.sync_registration(want, &users) {
            Ok(st) => log::info!("protocol provider registration: {:?} (reason {:?})", st.registration, st.reason),
            Err(e) => log::warn!("protocol provider registration: {e}"),
        }
    }

    /// A seat account the provider listener must admit (seat added after service start).
    fn note_seat_user(&self, user: &str, global: &RemoteSettings) {
        let added = {
            let mut u = lock(&self.0.seat_users);
            if u.iter().any(|x| x.eq_ignore_ascii_case(user)) {
                false
            } else {
                u.push(user.to_string());
                true
            }
        };
        if added {
            self.sync_wrds(global);
        }
    }

    // ---- Bluetooth per-user services (spec 60 §10.4) ----

    /// Turn the handling on or off; off gives back every instance we disabled at the next
    /// tick.
    pub fn set_bluetooth_console_only(&self, on: bool) {
        if let Some(bt) = &self.d().bluetooth {
            lock(bt).enabled = on;
            self.0.bt_next_ms.store(0, Ordering::SeqCst);
        }
    }

    /// Sessions of seats that have one (booting with a session, running, stopping).
    fn seat_sessions(&self) -> Vec<u32> {
        lock(&self.0.seats).values().filter_map(|r| r.res.session.as_ref().map(|s| s.session_id)).collect()
    }

    fn bluetooth_tick(&self, now_ms: u64) {
        let Some(bt) = &self.d().bluetooth else { return };
        if now_ms < self.0.bt_next_ms.load(Ordering::SeqCst) {
            return;
        }
        self.0.bt_next_ms.store(now_ms + BT_EVERY_MS, Ordering::SeqCst);
        let console = (self.d().console_session)().unwrap_or(u32::MAX);
        let sessions = self.seat_sessions();
        match lock(bt).refresh(console, &sessions) {
            Ok(r) => {
                for (name, e) in r.errors {
                    log::warn!("bluetooth: {name}: {e}");
                }
            }
            Err(e) => log::warn!("bluetooth refresh: {e}"),
        }
    }

    /// Service stop: every seat is already asked to stop by the controller; wait, then
    /// sweep, pad orphans, seat map clear.
    pub fn service_stop(&self, seat_users: &[String]) {
        if !self.wait_idle(STOP_WAIT) {
            log::warn!("{} seat(s) did not finish stopping", self.active_seats());
        }
        if self.passive() {
            return;
        }
        if let Err(e) = self.d().session.sweep(seat_users) {
            log::warn!("session sweep: {e}");
        }
        if let Some(p) = &self.d().pads {
            let mut p = lock(p);
            if let Err(e) = p.cleanup_orphans() {
                log::warn!("pad orphan cleanup: {e}");
            }
            p.shutdown();
        }
        let _ = self.d().seatmap.clear();
        if let Some(bt) = &self.d().bluetooth
            && let Err(e) = lock(bt).restore_all()
        {
            log::warn!("bluetooth restore: {e}");
        }
    }

    // ---- seat start ----

    pub fn start_seat(&self, seat: &RemoteSeatRecord, global: &RemoteSettings, password: Option<Secret>) -> Result<(), ProtoError> {
        if self.passive() {
            return Err(err(ErrorCode::Coexistence, "coexistence", "passive mode"));
        }
        if !global.multi_session {
            return Err(err(ErrorCode::ServiceUnavailable, "multi_session_off", "turn on multi-seat first"));
        }
        let password =
            password.ok_or_else(|| ProtoError::new(ErrorCode::MissingCredentials, "no stored password").with_field("password"))?;
        let generation = self.0.next_gen.fetch_add(1, Ordering::SeqCst);
        let cancel = CancelToken::new();
        {
            let mut seats = lock(&self.0.seats);
            if seats.contains_key(&seat.id) {
                return Err(ProtoError::new(ErrorCode::SeatRunning, "seat is still stopping"));
            }
            seats.insert(
                seat.id.clone(),
                SeatRun {
                    generation,
                    seat: seat.clone(),
                    global: global.clone(),
                    cancel: cancel.clone(),
                    stage: Stage::Booting,
                    res: Resources::default(),
                    pair: PairState::default(),
                    audio_report: None,
                    map_generation: 0,
                    map_prefixes: Vec::new(),
                },
            );
        }
        let me = self.clone();
        let seat = seat.clone();
        let global = global.clone();
        self.spawn(&format!("seat-start-{}", seat.name), move || me.boot(seat, global, password, generation, cancel));
        Ok(())
    }

    fn boot(&self, seat: RemoteSeatRecord, global: RemoteSettings, password: Secret, generation: u64, cancel: CancelToken) {
        let mut res = Resources::default();
        let r = self.boot_steps(&seat, &global, &password, &cancel, &mut res);
        drop(password);
        let mut seats = lock(&self.0.seats);
        let current = seats.get(&seat.id).is_some_and(|r| r.generation == generation);
        match r {
            Ok(()) if current && !cancel.is_cancelled() => {
                let run = seats.get_mut(&seat.id).expect("checked");
                let sid = res.session.as_ref().map(|s| s.session_id).unwrap_or(0);
                run.stage = Stage::Running;
                run.res = res;
                drop(seats);
                log::info!("seat '{}' running in session {sid}", seat.name);
                self.report(&seat.id, SeatEvent::Running { session_id: sid });
            }
            other => {
                drop(seats);
                self.undo(&seat, res);
                self.forget(&seat.id, generation);
                match other {
                    Err(f) if f.reason != "cancelled" => {
                        log::warn!("seat '{}' failed: {}", seat.name, f.message);
                        self.report(&seat.id, SeatEvent::Failed { reason: f.reason, message: f.message });
                    }
                    _ => self.report(&seat.id, SeatEvent::Stopped),
                }
            }
        }
    }

    fn forget(&self, seat_id: &str, generation: u64) {
        let mut seats = lock(&self.0.seats);
        if seats.get(seat_id).is_some_and(|r| r.generation == generation) {
            seats.remove(seat_id);
        }
    }

    fn phase(&self, seat: &RemoteSeatRecord, cancel: &CancelToken, p: &str) -> Result<(), Failure> {
        if cancel.is_cancelled() {
            return Err(Failure::cancelled());
        }
        self.report(&seat.id, SeatEvent::Phase(p.into()));
        Ok(())
    }

    fn boot_steps(
        &self,
        seat: &RemoteSeatRecord,
        global: &RemoteSettings,
        password: &Secret,
        cancel: &CancelToken,
        res: &mut Resources,
    ) -> Result<(), Failure> {
        let d = self.d();
        // 0. preflight: the seat's backend must be installed before anything is logged on or
        // attached (a missing `pulsar-host` is a certain failure).
        let info = self.backend_for(seat.stream_backend).map_err(|e| Failure::new(e.code(), &e))?.availability();
        if !info.installed {
            let reason = info.reason.as_deref().unwrap_or("backend_missing");
            return Err(Failure::new(reason, format!("{} stream backend not installed", seat.stream_backend.as_str())));
        }
        // 1. seat user: facts, Remote Desktop Users, profile.
        self.phase(seat, cancel, "user")?;
        let user = d.session.ensure_seat_user(&seat.user_name, password).map_err(|e| Failure::new(e.code(), &e))?;
        if user.is_admin {
            self.notice(&seat.id, "host_warning", serde_json::json!({"kind": "seat_user_is_admin"}));
        }

        // 2. session. Our display (`pulsar_idd`) needs the seat session to come through our
        // protocol provider (crate::remote::wrds); anything else, and every failure of that
        // path, uses the loopback RDP client (helper with the password on its stdin pipe)
        // and the stock RDP display (`display_fallback` notice with the reason).
        self.phase(seat, cancel, "session")?;
        let audio = seat_audio(seat);
        let policy = d.audio.policy().ok();
        let pref = global.display_backend.map(to_kind);
        let want_provider = wants_provider(pref, OWN_DRIVER_SIGNED);
        if want_provider {
            self.note_seat_user(&seat.user_name, global);
            let status = d.wrds.as_ref().map(|w| w.status());
            let fallback = match plan(pref, &[], false, OWN_DRIVER_SIGNED, status.as_ref()) {
                HostPlan::Provider => match self.start_provider(seat, global, password, &user.sid, cancel, res) {
                    Ok(()) => None,
                    Err(f) if f.reason == "cancelled" => return Err(f),
                    Err(f) => {
                        log::warn!("seat '{}': own display path failed, falling back to RDP: {}", seat.name, f.message);
                        Some(f.reason)
                    }
                },
                HostPlan::Loopback { fallback, .. } => fallback,
            };
            if let Some(reason) = fallback {
                self.notice(
                    &seat.id,
                    "host_warning",
                    serde_json::json!({"kind": "display_fallback", "from": "pulsar_idd", "to": "rdp_session", "reason": reason}),
                );
                res.fallback = Some(reason);
            }
        }
        // Microphone (spec/remote 70 §5.1): Pulsar seats only (pulsar-host mic pipe -> the
        // loopback hold's stdin -> MS-RDPEAI); Moonlight sends no mic. Provider sessions have
        // no RDP audio channel, so no mic either.
        let capture = res.wrds.is_none()
            && policy.as_ref().map(|p| rdp_client_flags(&audio, p, mic_path(seat)).capture).unwrap_or(false);
        if res.session.is_none() {
            let req = SessionRequest {
                seat_id: &seat.id,
                client_name: &seat.rdp_client_name,
                user: &seat.user_name,
                width: session_size(seat).0,
                height: session_size(seat).1,
                keyboard_layout: None,
                audio_capture: capture,
            };
            let session = d.session.start_session(&req, password, cancel).map_err(|e| Failure::new(e.code(), &e))?;
            res.session = Some(session);
        }
        let (session_id, user_sid) = res.session.as_ref().map(|s| (s.session_id, s.user_sid.clone())).unwrap_or_default();

        // 3. display (already attached on the provider path).
        self.phase(seat, cancel, "display")?;
        if res.display.is_none() {
            let force = want_provider.then_some(BackendKind::RdpSession);
            res.display = Some(self.attach_display(seat, global, session_id, force, false, cancel)?);
        }

        // 4. audio keys for the stream server (default device = the session's Remote Audio).
        self.phase(seat, cancel, "audio")?;
        let audio_keys = server_keys(&audio, None);

        // 5. stream server.
        self.phase(seat, cancel, "stream")?;
        res.server = Some(self.start_stream(seat, global, session_id, audio_keys, capture)?);

        // 6. pads: the seat context is registered on the first `pad.wanted`.
        self.phase(seat, cancel, "pads")?;
        res.pads = d.pads.is_some();

        // 7. isolation seat map (always published: the module also needs it for the role).
        self.phase(seat, cancel, "isolation")?;
        let entry = SeatMapEntry {
            session_id,
            seat_id: seat.id.clone(),
            user_sid,
            generation: 1,
            pad_instance_prefixes: Vec::new(),
            flags: SeatFlags {
                hid_isolation: global.hid_isolation,
                xinput_focus: global.process_patching,
                steam: global.steam_isolation,
                shared_account: false,
            },
        };
        match d.seatmap.publish(&entry) {
            Ok(()) => res.seatmap = Some(session_id),
            Err(e) => {
                if global.hid_isolation || global.process_patching {
                    return Err(Failure::new(e.code(), &e));
                }
                log::warn!("seat map publish failed (isolation off): {e}");
            }
        }

        // 8. autostart apps, always as the seat user (spec 30 §8).
        self.phase(seat, cancel, "autostart")?;
        res.autostart = true;
        for app in &seat.autostart {
            if app.run_as_system {
                self.notice(&seat.id, "host_warning", serde_json::json!({"kind": "autostart_run_as_system_ignored", "path": app.path}));
            }
            let exe = PathBuf::from(&app.path);
            let spec = LaunchSpec {
                cwd: exe.parent().map(Path::to_path_buf),
                exe,
                args: split_args(&app.arguments),
                session_id,
                token: TokenKind::SeatUser,
                env_extra: seat_env(&seat.id, global.steam_isolation, &seat.pad_ignore_devices, &self.physical_pad_ids())
                    .into_iter()
                    .chain(seat_compat::user_env::launch_env(&seat.environment))
                    .collect(),
                desktop: DEFAULT_DESKTOP.into(),
                stdout: None,
            };
            if let Err(e) = d.session.launcher().launch(&seat.id, &spec) {
                log::warn!("seat '{}': autostart app not started: {e}", seat.name);
                self.notice(&seat.id, "host_warning", serde_json::json!({"kind": "autostart_failed", "path": app.path, "reason": e.code()}));
            }
        }
        if cancel.is_cancelled() {
            return Err(Failure::cancelled());
        }
        // 9. DPI read-back (spec/remote 40 §4.4; compat-tweaks 2026-11-17): only for a
        // static scale, the case where the seat asks for a number Windows may override.
        if seat.display.scale_static {
            self.dpi_readback(seat, session_id);
        }
        Ok(())
    }

    /// Starts `pulsar-seat-agent --report-dpi` in the seat session as the seat user with
    /// its output in the seat folder, reads it back a few seconds later and raises a
    /// `host_warning` notice (`dpi_mismatch`) when the effective scale differs from the
    /// seat's static scale. HYPOTHESIS: untested on hardware (needs a remote seat).
    fn dpi_readback(&self, seat: &RemoteSeatRecord, session_id: u32) {
        let Ok(dir) = SeatDir::checked(&self.d().data_root, &seat.id) else { return };
        let Some(exe) = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join(seat_core::consts::AGENT_EXE))) else {
            return;
        };
        let out = dir.root.join("dpi.log");
        let _ = std::fs::remove_file(&out);
        let spec = LaunchSpec {
            cwd: None,
            exe,
            args: vec!["--report-dpi".into()],
            session_id,
            token: TokenKind::SeatUser,
            env_extra: Vec::new(),
            desktop: DEFAULT_DESKTOP.into(),
            stdout: Some(out.clone()),
        };
        if let Err(e) = self.d().session.launcher().launch(&seat.id, &spec) {
            log::debug!("seat '{}': DPI read-back not started: {e}", seat.name);
            return;
        }
        let me = self.clone();
        let (id, name, want) = (seat.id.clone(), seat.name.clone(), u32::from(seat.display.scale_percent));
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(10));
            let Some(mons) = std::fs::read_to_string(&out).ok().and_then(|t| seat_proto::parse_dpi_report(&t)) else {
                log::debug!("seat '{name}': no DPI read-back");
                return;
            };
            let bad = crate::compat::dpi_mismatches(Some(want), &mons);
            if !bad.is_empty() {
                log::warn!("seat '{name}': effective scale differs from the seat setting: {}", bad.join("; "));
                me.notice(&id, "host_warning", serde_json::json!({"kind": "dpi_mismatch", "detail": bad.join("; ")}));
            }
        });
    }

    fn probe_displays(&self) -> Vec<BackendInfo> {
        self.d()
            .displays
            .iter()
            .filter_map(|b| match b.probe() {
                Ok(i) => Some(i),
                Err(e) => {
                    log::debug!("display backend {:?} probe: {e}", b.kind());
                    None
                }
            })
            .collect()
    }

    /// The provider path: a session through our protocol provider, then our display in it.
    /// On a failure everything it did is undone here, so the caller can fall back.
    fn start_provider(
        &self,
        seat: &RemoteSeatRecord,
        global: &RemoteSettings,
        password: &Secret,
        user_sid: &str,
        cancel: &CancelToken,
        res: &mut Resources,
    ) -> Result<(), Failure> {
        let Some(w) = self.d().wrds.clone() else {
            return Err(Failure::new("wrds_unavailable", "no protocol provider on this host"));
        };
        let (width, height) = session_size(seat);
        let req = WrdsConnect {
            seat_id: seat.id.clone(),
            client_name: seat.rdp_client_name.clone(),
            user: seat.user_name.clone(),
            width,
            height,
        };
        let ws = w.connect(&req, password, cancel).map_err(|f| Failure { reason: f.reason, message: f.message })?;
        log::info!("seat '{}': session {} through the protocol provider", seat.name, ws.session_id);
        self.phase(seat, cancel, "display").inspect_err(|_| {
            let _ = w.end(&ws, StopAction::LogOff);
        })?;
        match self.attach_display(seat, global, ws.session_id, Some(BackendKind::PulsarIdd), true, cancel) {
            Ok(disp) => {
                res.session = Some(SeatSession {
                    seat_id: seat.id.clone(),
                    session_id: ws.session_id,
                    user_sid: user_sid.to_string(),
                    helper_pid: 0,
                    client_name: seat.rdp_client_name.clone(),
                });
                res.wrds = Some(ws);
                res.display = Some(disp);
                Ok(())
            }
            Err(f) => {
                if let Err(e) = w.end(&ws, StopAction::LogOff) {
                    log::warn!("seat '{}': provider session not ended: {e}", seat.name);
                }
                Err(f)
            }
        }
    }

    /// `force` = this backend whatever the setting says. `wait_device`: the session's display
    /// device may still be arriving (provider path), so `driver_not_installed` is retried
    /// until the ready timeout.
    fn attach_display(
        &self,
        seat: &RemoteSeatRecord,
        global: &RemoteSettings,
        session_id: u32,
        force: Option<BackendKind>,
        wait_device: bool,
        cancel: &CancelToken,
    ) -> Result<DisplayRun, Failure> {
        let infos = self.probe_displays();
        let console = (self.d().console_session)() == Some(session_id);
        let kind = force.unwrap_or_else(|| choose_backend(global.display_backend.map(to_kind), &infos, console));
        let backend = self
            .d()
            .displays
            .iter()
            .find(|b| b.kind() == kind)
            .cloned()
            .ok_or_else(|| Failure::new("driver_not_installed", format!("display backend {kind:?} is not available")))?;
        let adapters = self.d().adapters.adapters().unwrap_or_default();
        let choice = resolve_adapter(
            seat.display.render_adapter_id.as_deref(),
            seat.display.render_adapter.as_deref(),
            &adapters,
        );
        if let AdapterChoice::Default { warning: Some(w) } = &choice {
            self.notice(&seat.id, "host_warning", serde_json::json!({"kind": "adapter_fallback", "detail": w}));
        }
        let l = seat.display.luminance;
        let cfg = SeatDisplay::from_seat(seat, session_id, choice)
            .with_refresh(seat.display.refresh_hz)
            .with_luminance(HdrLuminance {
                min_nits_x10000: l.min_nits_x10000,
                max_nits: l.max_nits,
                max_full_frame_nits: l.max_full_frame_nits,
            });
        let deadline = Instant::now() + seat_display::remote::backend::READY_TIMEOUT;
        let handle = loop {
            match backend.attach(&cfg) {
                Ok(h) => break h,
                Err(e) if wait_device && e.code() == "driver_not_installed" && Instant::now() < deadline => {
                    if cancel.is_cancelled() {
                        return Err(Failure::cancelled());
                    }
                    std::thread::sleep(Duration::from_millis(250));
                }
                Err(e) => return Err(Failure::new(e.code(), &e)),
            }
        };
        if let Err(e) = backend.wait_ready(&handle, seat_display::remote::backend::READY_TIMEOUT) {
            let _ = backend.detach(&handle);
            let reason = if matches!(e, DisplayError::Timeout(_)) { "display_timeout" } else { e.code() };
            return Err(Failure::new(reason, &e));
        }
        // The provider path's device exists only now: probe again for its HDR facts.
        let infos = if wait_device { self.probe_displays() } else { infos };
        let hdr = if seat.display.force_sdr {
            Err(HdrUnavailable::ForceSdr)
        } else {
            infos.iter().find(|i| i.kind == kind).map(|i| i.hdr).unwrap_or(Err(HdrUnavailable::DriverNotInstalled))
        };
        Ok(DisplayRun { backend, handle, hdr })
    }

    /// The seat's backend; `backend_missing` when this host has none of that kind.
    fn backend_for(&self, kind: StreamBackendKind) -> Result<Arc<dyn StreamBackend>, StreamError> {
        find_backend(&self.d().stream.backends, kind)
            .ok_or_else(|| StreamError::BackendMissing(format!("no {} stream backend on this host", kind.as_str())))
    }

    /// Prepare (port check, binary, files, firewall) and launch the seat's own backend.
    /// Never falls back to another backend.
    fn start_stream(
        &self,
        seat: &RemoteSeatRecord,
        global: &RemoteSettings,
        session_id: u32,
        audio_keys: Vec<(String, String)>,
        mic: bool,
    ) -> Result<Server, Failure> {
        let backend = self.backend_for(seat.stream_backend).map_err(|e| Failure::new(e.code(), &e))?;
        let mut apps = default_apps();
        if global.process_patching
            && let Some(l) = self.d().isolation.launcher_path()
        {
            wrap_with_launcher(&mut apps, &l);
        }
        let spec = SeatStreamSpec {
            seat,
            global,
            data_root: &self.d().data_root,
            audio_keys,
            default_apps: apps,
            service_version: &self.d().service_version,
            mic_sink: mic.then(|| mic_sink(self.d().session.clone(), seat.id.clone())),
        };
        let Prepared { mut stream, warnings } = backend.prepare(&spec).map_err(|e| Failure::new(e.code(), &e))?;
        for w in warnings {
            self.notice(&seat.id, "host_warning", w);
        }
        stream.start(session_id).map_err(|e| Failure::new(e.code(), &e))?;
        Ok(Arc::new(Mutex::new(stream)))
    }

    /// Reverse of the start order. Every step is best effort and logged.
    fn undo(&self, seat: &RemoteSeatRecord, res: Resources) {
        let d = self.d();
        if res.autostart
            && let Err(e) = d.session.launcher().kill_job(&seat.id)
        {
            log::debug!("seat '{}': kill job: {e}", seat.name);
        }
        if let Some(sid) = res.seatmap
            && let Err(e) = d.seatmap.remove(sid)
        {
            log::warn!("seat '{}': seat map remove: {e}", seat.name);
        }
        if res.pads
            && let Some(p) = &d.pads
        {
            lock(p).remove_seat(&seat.id);
        }
        if let Some(server) = res.server
            && let Err(e) = lock(&server).stop()
        {
            log::warn!("seat '{}': stream server stop: {e}", seat.name);
        }
        if let Some(disp) = res.display
            && let Err(e) = disp.backend.detach(&disp.handle)
        {
            log::warn!("seat '{}': display detach: {e}", seat.name);
        }
        if let Some(ws) = &res.wrds {
            // Provider sessions: log off / disconnect through WTS, then drop the connection.
            if let Some(w) = &d.wrds
                && let Err(e) = w.end(ws, seat.stop_action)
            {
                log::warn!("seat '{}': provider session end: {e}", seat.name);
            }
        } else if res.session.is_some()
            && let Err(e) = d.session.stop_session(&seat.id, seat.stop_action)
        {
            log::warn!("seat '{}': session stop: {e}", seat.name);
            // Last resort: at least end the helper and disconnect.
            if seat.stop_action == StopAction::LogOff {
                let _ = d.session.stop_session(&seat.id, StopAction::Disconnect);
            }
        }
    }

    // ---- seat stop ----

    pub fn stop_seat(&self, seat: &RemoteSeatRecord) -> Result<(), ProtoError> {
        self.take_down(&seat.id, None)
    }

    /// Stops a seat; `failure` = report `Failed` instead of `Stopped` when done.
    fn take_down(&self, seat_id: &str, failure: Option<Failure>) -> Result<(), ProtoError> {
        let mut seats = lock(&self.0.seats);
        let Some(run) = seats.get_mut(seat_id) else {
            drop(seats);
            self.report(seat_id, SeatEvent::Stopped);
            return Ok(());
        };
        match run.stage {
            Stage::Booting => {
                // The boot worker undoes what it did and reports `Stopped`.
                run.cancel.cancel();
                run.stage = Stage::Stopping;
            }
            Stage::Stopping => {}
            Stage::Running => {
                run.stage = Stage::Stopping;
                let res = std::mem::take(&mut run.res);
                let seat = run.seat.clone();
                let generation = run.generation;
                drop(seats);
                let me = self.clone();
                self.spawn(&format!("seat-stop-{}", seat.name), move || {
                    me.undo(&seat, res);
                    me.forget(&seat.id, generation);
                    match failure {
                        Some(f) => me.report(&seat.id, SeatEvent::Failed { reason: f.reason, message: f.message }),
                        None => me.report(&seat.id, SeatEvent::Stopped),
                    }
                });
            }
        }
        Ok(())
    }

    // ---- periodic work (1 s timer, not under the controller lock) ----

    pub fn tick(&self, now_ms: u64) {
        let d = self.d();
        for n in d.session.tick(now_ms) {
            match n {
                SessionNotice::Failed { seat_id, reason } => {
                    let _ = self.take_down(&seat_id, Some(Failure::new(reason.code(), "seat session failed")));
                }
                SessionNotice::SessionGone { seat_id } => {
                    let _ = self.take_down(&seat_id, Some(Failure::new("client_exited", "seat session ended")));
                }
                SessionNotice::Reconnected { seat_id, session_id } => self.reconnected(&seat_id, session_id),
            }
        }
        if let Some(w) = &d.wrds {
            for (conn_id, reason) in w.take_lost() {
                let seat_id = lock(&self.0.seats)
                    .iter()
                    .find(|(_, r)| r.res.wrds.as_ref().is_some_and(|ws| ws.conn_id == conn_id))
                    .map(|(id, _)| id.clone());
                if let Some(seat_id) = seat_id {
                    let _ = self.take_down(&seat_id, Some(Failure::new("client_exited", format!("provider connection ended: {reason}"))));
                }
            }
        }
        let servers: Vec<(String, Server)> = lock(&self.0.seats)
            .iter()
            .filter(|(_, r)| r.stage == Stage::Running)
            .filter_map(|(id, r)| r.res.server.clone().map(|s| (id.clone(), s)))
            .collect();
        for (seat_id, server) in servers {
            let (notices, crashes) = {
                let mut s = lock(&server);
                (s.poll(now_ms), s.crash_count())
            };
            if !notices.is_empty() {
                self.report(&seat_id, SeatEvent::Crashes(crashes));
            }
            for n in notices {
                self.stream_notice(&seat_id, &server, n);
            }
        }
        if let Some(p) = &d.pads {
            let out = {
                let mut p = lock(p);
                p.tick(now_ms);
                p.pump();
                p.take_outgoing()
            };
            for o in out {
                let (seat_id, ty, body) = match o {
                    seat_pads::Outgoing::Detach { seat_id, msg } => (seat_id, "pad.detach", json(&msg)),
                    seat_pads::Outgoing::Attach { seat_id, msg } => (seat_id, "pad.attach", json(&msg)),
                    seat_pads::Outgoing::Denied { seat_id, msg } => (seat_id, "pad.denied", json(&msg)),
                };
                self.pad_request(&seat_id, ty, &body);
            }
            self.refresh_seat_maps();
        }
        if !self.passive() {
            self.bluetooth_tick(now_ms);
        }
    }

    fn server_of(&self, seat_id: &str) -> Option<Server> {
        lock(&self.0.seats).get(seat_id).and_then(|r| r.res.server.clone())
    }

    fn pad_request(&self, seat_id: &str, ty: &str, body: &serde_json::Value) {
        if let Some(s) = self.server_of(seat_id)
            && let Err(e) = lock(&s).pad_request(ty, body)
        {
            log::debug!("{ty} not delivered: {e}");
        }
    }

    fn reconnected(&self, seat_id: &str, session_id: u32) {
        let (server, entry_session) = {
            let mut seats = lock(&self.0.seats);
            let Some(run) = seats.get_mut(seat_id) else { return };
            let old = run.res.session.as_ref().map(|s| s.session_id);
            if let Some(s) = run.res.session.as_mut() {
                s.session_id = session_id;
            }
            (if old != Some(session_id) { run.res.server.clone() } else { None }, old)
        };
        self.report(seat_id, SeatEvent::Session(session_id));
        if let Some(s) = server {
            // A new Windows session: the server must run inside it.
            if let Err(e) = lock(&s).start(session_id) {
                let _ = self.take_down(seat_id, Some(Failure::new(e.code(), &e)));
                return;
            }
            if let Some(old) = entry_session {
                let _ = self.d().seatmap.remove(old);
            }
            self.republish(seat_id, true);
        }
    }

    /// Re-publishes the seat map when pad instance prefixes changed (or `force`).
    fn republish(&self, seat_id: &str, force: bool) {
        let prefixes = self.d().pads.as_ref().map(|p| lock(p).seat_instance_prefixes(seat_id)).unwrap_or_default();
        let entry = {
            let mut seats = lock(&self.0.seats);
            let Some(run) = seats.get_mut(seat_id) else { return };
            if run.stage != Stage::Running || (!force && run.map_prefixes == prefixes) {
                return;
            }
            let Some(s) = run.res.session.as_ref() else { return };
            run.map_generation += 1;
            run.map_prefixes = prefixes.clone();
            run.res.seatmap = Some(s.session_id);
            SeatMapEntry {
                session_id: s.session_id,
                seat_id: seat_id.to_string(),
                user_sid: s.user_sid.clone(),
                generation: run.map_generation + 1,
                pad_instance_prefixes: prefixes,
                flags: SeatFlags {
                    hid_isolation: run.global.hid_isolation,
                    xinput_focus: run.global.process_patching,
                    steam: run.global.steam_isolation,
                    shared_account: false,
                },
            }
        };
        if let Err(e) = self.d().seatmap.publish(&entry) {
            log::warn!("seat map publish: {e}");
        }
    }

    fn refresh_seat_maps(&self) {
        let ids: Vec<String> = lock(&self.0.seats).keys().cloned().collect();
        for id in ids {
            self.republish(&id, false);
        }
    }

    fn stream_notice(&self, seat_id: &str, server: &Server, n: StreamNotice) {
        match n {
            StreamNotice::GaveUp { .. } => {
                let _ = self.take_down(seat_id, Some(Failure::new("stream_server", "stream server keeps crashing")));
            }
            StreamNotice::ControlUnavailable { reason } => {
                self.notice(seat_id, "host_warning", serde_json::json!({"kind": "control_unavailable", "reason": reason}));
            }
            StreamNotice::Fallback { missing } => {
                // The Pulsar stream-server build lacks some patched features (or its seat
                // pipe): the seat keeps streaming with upstream behaviour for them.
                self.notice(seat_id, "host_warning", serde_json::json!({"kind": "stream_fallback", "missing": missing}));
            }
            StreamNotice::Running | StreamNotice::ControlReady { .. } | StreamNotice::Restarting { .. } => {}
            StreamNotice::Event(ev) => self.server_event(seat_id, server, ev),
            StreamNotice::Peer(p) => self.peer_event(seat_id, server, p),
        }
    }

    /// Pulsar-only events (contract §3.4, §3.7): operator prompts, relay, trust, apps.
    fn peer_event(&self, seat_id: &str, server: &Server, p: PeerEvent) {
        match p {
            PeerEvent::AuthRequest { request_id, request_kind, label, device_name, key_fp, password_state, received_at, expires_in_s } => {
                if let Some(run) = lock(&self.0.seats).get_mut(seat_id) {
                    run.pair.pending = Some(PendingPair {
                        request_id: Some(request_id),
                        device_name: device_name.clone(),
                        received_at,
                        mode: PairMode::Approve,
                        kind: Some(request_kind.clone()),
                        key_fp: Some(key_fp.clone()),
                        password_state: Some(password_state),
                        label: Some(label),
                        expires_in_s: Some(expires_in_s),
                    });
                }
                self.notice(
                    seat_id,
                    "pair_pending",
                    serde_json::json!({"request_id": request_id, "device_name": device_name, "mode": "approve",
                        "kind": request_kind, "key_fp": key_fp, "backend": "pulsar"}),
                );
            }
            PeerEvent::AuthEnded { request_id, result } => {
                if let Some(run) = lock(&self.0.seats).get_mut(seat_id) {
                    if run.pair.pending.as_ref().and_then(|p| p.request_id) == Some(request_id) {
                        run.pair.pending = None;
                    }
                    run.pair.last_result = Some(result.clone());
                }
                self.notice(seat_id, "pair_ended", serde_json::json!({"request_id": request_id, "result": result, "backend": "pulsar"}));
            }
            PeerEvent::OtpChanged => self.notice(seat_id, "otp_changed", serde_json::json!({"backend": "pulsar"})),
            PeerEvent::TrustChanged => self.notice(seat_id, "trust_changed", serde_json::json!({"backend": "pulsar"})),
            PeerEvent::RelayState { state, .. } => log::info!("seat {seat_id}: relay {state}"),
            PeerEvent::RelayAuthRequired { password, totp } => {
                self.notice(seat_id, "relay_auth_required", serde_json::json!({"password": password, "totp": totp, "backend": "pulsar"}));
            }
            PeerEvent::AppLaunch { request_id, app_id, .. } => {
                let result = self.launch_seat_app(seat_id, &app_id);
                if let Err(e) = &result {
                    log::warn!("seat {seat_id}: app {app_id} not started: {e}");
                }
                if let Err(e) = lock(server).app_launched(request_id, result) {
                    log::debug!("app.launched not delivered: {e}");
                }
            }
        }
    }

    /// Contract §3.5: an app a Pulsar client picked runs as the **seat user** (never as
    /// SYSTEM), like autostart. The "Masaüstü" entry (no command) launches nothing.
    /// `Ok(None)`: the entry has no command (the "Masaüstü" entry), nothing is launched.
    fn launch_seat_app(&self, seat_id: &str, app_id: &str) -> Result<Option<u32>, String> {
        let (session_id, steam, pad_ignore, seat_vars) = {
            let seats = lock(&self.0.seats);
            let run = seats.get(seat_id).ok_or("seat_not_running")?;
            (
                run.res.session.as_ref().map(|s| s.session_id).ok_or("seat_not_running")?,
                run.global.steam_isolation,
                run.seat.pad_ignore_devices.clone(),
                run.seat.environment.clone(),
            )
        };
        let dir = SeatDir::checked(&self.d().data_root, seat_id).map_err(|e| e.code().to_string())?;
        let apps = match self.d().stream.fs.read(&dir.apps).map_err(|e| e.code().to_string())? {
            Some(b) => parse_apps_json(&String::from_utf8_lossy(&b)).map_err(|e| e.code().to_string())?.apps,
            None => default_apps(),
        };
        let app = seat_stream::backend::pulsar::find_app(&apps, app_id).ok_or("app_not_found")?;
        let Some(cmd) = app.cmd.as_deref().filter(|c| !c.trim().is_empty()) else {
            return Ok(None);
        };
        let mut argv = split_args(cmd);
        if argv.is_empty() {
            return Ok(None);
        }
        let exe = PathBuf::from(argv.remove(0));
        let spec = LaunchSpec {
            cwd: app.working_dir.as_ref().map(PathBuf::from).or_else(|| exe.parent().map(Path::to_path_buf)),
            exe,
            args: argv,
            session_id,
            token: TokenKind::SeatUser,
            env_extra: seat_env(seat_id, steam, &pad_ignore, &self.physical_pad_ids())
                .into_iter()
                .chain(seat_compat::user_env::launch_env(&seat_vars))
                .collect(),
            desktop: DEFAULT_DESKTOP.into(),
            stdout: None,
        };
        self.d().session.launcher().launch(seat_id, &spec).map(|p| Some(p.pid)).map_err(|e| e.code().to_string())
    }

    fn server_event(&self, seat_id: &str, server: &Server, ev: ServerEvent) {
        match ev {
            ServerEvent::PairPending { request_id, device_name, received_at, .. } => {
                if let Some(run) = lock(&self.0.seats).get_mut(seat_id) {
                    run.pair.pending = Some(PendingPair {
                        request_id: Some(request_id),
                        device_name: device_name.clone(),
                        received_at,
                        ..Default::default()
                    });
                }
                self.notice(seat_id, "pair_pending", serde_json::json!({"request_id": request_id, "device_name": device_name}));
            }
            ServerEvent::PairEnded { request_id, result } => {
                let result = json(&result).as_str().unwrap_or("unknown").to_string();
                if let Some(run) = lock(&self.0.seats).get_mut(seat_id) {
                    run.pair.pending = None;
                    run.pair.last_result = Some(result.clone());
                }
                self.notice(seat_id, "pair_ended", serde_json::json!({"request_id": request_id, "result": result}));
            }
            ServerEvent::StreamStarted { app, video } => {
                self.notice(seat_id, "stream_started", serde_json::json!({"app": app, "video": video}));
            }
            ServerEvent::StreamEnded { reason } => {
                self.notice(seat_id, "stream_ended", serde_json::json!({"reason": reason}));
            }
            ServerEvent::CaptureError { code } => {
                self.notice(seat_id, "host_warning", serde_json::json!({"kind": "capture_error", "code": code}));
            }
            ServerEvent::DisplayModeRequest { request_id, w, h, hz, hdr } => {
                let result = self.apply_mode(seat_id, ModeRequest { request_id, width: w, height: h, hz, hdr });
                if let Err(e) = lock(server).reply_mode(&json(&result)) {
                    log::debug!("display.mode.result not delivered: {e}");
                }
            }
            ServerEvent::PadWanted(v) => self.pad_wanted(seat_id, server, v),
            ServerEvent::PadReleased(v) => {
                if let (Some(p), Ok(r)) = (&self.d().pads, serde_json::from_value::<PadReleased>(v)) {
                    lock(p).released(seat_id, r.slot, r.generation, r.reason);
                }
            }
            ServerEvent::AudioState { state } => {
                let st = serde_json::from_value::<AudioState>(serde_json::Value::String(state)).ok();
                if let Some(run) = lock(&self.0.seats).get_mut(seat_id) {
                    run.audio_report = st;
                }
            }
        }
    }

    /// Apply a mode to a seat's display after its own settings ([`seat_mode_request`]); a
    /// success is persisted as the seat's `last_mode`.
    fn apply_mode(&self, seat_id: &str, req: ModeRequest) -> ModeResult {
        let found = lock(&self.0.seats)
            .get(seat_id)
            .and_then(|r| r.res.display.as_ref().map(|d| (d.backend.clone(), d.handle.clone(), r.seat.display.clone())));
        let request_id = req.request_id;
        let result = match found {
            Some((b, handle, settings)) => {
                let req = seat_mode_request(&settings, req);
                b.request_mode(&handle, &req).unwrap_or_else(|e| ModeResult::empty(request_id, mode_status_for(&e)))
            }
            None => ModeResult::empty(request_id, ModeStatus::NoDisplay),
        };
        if matches!(result.status, ModeStatus::Ok | ModeStatus::OkSdrFallback | ModeStatus::Clamped) {
            let m = seat_proto::remote::LastMode { width: result.width, height: result.height, hz: result.hz };
            self.report(seat_id, SeatEvent::ModeApplied(m));
        }
        result
    }

    /// Operator mode control (`set_seat_display_mode`): the same path as a client's request.
    /// The stream server follows the new display mode by itself. Request ids of the operator
    /// have the top bit set so they never collide with the stream server's.
    pub fn set_display_mode(&self, seat: &RemoteSeatRecord, width: u32, height: u32, hz: u32, hdr: bool) -> Result<serde_json::Value, ProtoError> {
        if !lock(&self.0.seats).get(&seat.id).is_some_and(|r| r.stage == Stage::Running) {
            return Err(ProtoError::new(ErrorCode::ServiceUnavailable, "seat is not running"));
        }
        let request_id = 0x8000_0000 | (self.0.next_gen.fetch_add(1, Ordering::SeqCst) as u32 & 0x7fff_ffff);
        let r = self.apply_mode(&seat.id, ModeRequest { request_id, width, height, hz, hdr });
        self.notice(&seat.id, "display_mode", json(&r));
        Ok(json(&r))
    }

    fn pad_wanted(&self, seat_id: &str, server: &Server, v: serde_json::Value) {
        let Some(p) = &self.d().pads else { return };
        let Ok(w) = serde_json::from_value::<PadWanted>(v) else {
            log::debug!("pad.wanted with an unknown body");
            return;
        };
        let ctx = {
            let seats = lock(&self.0.seats);
            let Some(run) = seats.get(seat_id) else { return };
            let Some(s) = run.res.session.as_ref() else { return };
            SeatPadContext {
                seat_id: seat_id.to_string(),
                session_id: s.session_id,
                owner_sid: s.user_sid.clone(),
                pads_enabled: true,
                max_slots: None,
            }
        };
        let answer = lock(p).wanted(&ctx, &w);
        let (ty, body) = match answer {
            Ok(Some(a)) => ("pad.attach", json(&a)),
            Ok(None) => return, // created on a worker; the answer comes via `take_outgoing`
            Err(denied) => ("pad.denied", json(&denied)),
        };
        if let Err(e) = lock(server).pad_request(ty, &body) {
            log::debug!("{ty} not delivered: {e}");
        }
    }

    /// Moves pad input from the seat channels to the virtual pads (fast loop, see
    /// `service.rs`). Returns the number of reports forwarded.
    pub fn pump_pads(&self) -> usize {
        self.d().pads.as_ref().map(|p| lock(p).pump()).unwrap_or(0)
    }

    /// Runs `f` with the pad manager's channel generation and its live input-event handles,
    /// under the pad lock (so the handles stay valid for the call; the pad pump duplicates
    /// them there). `None` without pads. Added 2026-11-17 (Split OQ integrator-2).
    pub fn with_pad_events<R>(&self, f: impl FnOnce(u64, &[isize]) -> R) -> Option<R> {
        self.d().pads.as_ref().map(|p| {
            let g = lock(p);
            f(g.channels_generation(), &g.input_events())
        })
    }

    pub fn has_pads(&self) -> bool {
        self.d().pads.is_some()
    }

    /// The shared pad manager and its feedback router, for the local seats' pad hub
    /// (seat_input::local_pads::LocalPadHub). None without pads.
    pub fn local_pad_parts(&self) -> Option<(Arc<Mutex<PadManager>>, Arc<seat_input::local_pads::LocalFeedbackRouter>)> {
        Some((self.d().pads.clone()?, self.d().local_pad_feedback.clone()?))
    }

    pub fn on_session_change(&self, ev: SessionChange) {
        self.d().session.on_session_change(ev);
        // Logon / console moves change the suffix map: re-check at the next tick.
        self.0.bt_next_ms.store(0, Ordering::SeqCst);
    }

    pub fn record_health(&self, h: ShimHealth) {
        let mut m = lock(&self.0.health);
        if m.len() >= MAX_HEALTH && !m.contains_key(&(h.session_id, h.exe.clone())) {
            return;
        }
        m.insert((h.session_id, h.exe.clone()), h);
    }

    // ---- queries and v2 requests ----

    pub fn host_info(&self, global: &RemoteSettings) -> HostInfo {
        let tw = self.d().termwrap.status().ok();
        let wrapper_active = tw.as_ref().is_some_and(|s| matches!(s.state, TermwrapState::On { .. }));
        let mut out = HostInfo {
            coexistence: lock(&self.0.coexist).clone(),
            termwrap: tw.as_ref().map(termwrap_json),
            display_backends: self.probe_displays().iter().map(json).collect(),
            pad_backends: self.d().pads.as_ref().map(|p| lock(p).backends().iter().map(json).collect()).unwrap_or_default(),
            multi_session: global.multi_session,
            wrds: self.d().wrds.as_ref().map(|w| {
                let mut v = json(&w.status());
                v["wanted"] = serde_json::Value::Bool(wants_provider(global.display_backend.map(to_kind), OWN_DRIVER_SIGNED));
                v
            }),
            ..Default::default()
        };
        match self.d().session.host_info(wrapper_active) {
            Ok(h) => {
                out.edition = h.edition.edition_id.clone();
                out.build = h.edition.build;
                out.class = json(&h.class).as_str().unwrap_or("").to_string();
                *lock(&self.0.host_class) = Some(out.class.clone());
                out.rds_ready = h.rds_ready;
                out.listener_port = h.listener.as_ref().map(|l| l.port);
                out.licensing = h.licensing.as_ref().map(json);
                out.max_seats = h.max_seats;
                out.warnings = h.warnings.iter().map(json).collect();
            }
            Err(e) => out.error = Some(e.code().to_string()),
        }
        out
    }

    pub fn set_multi_session(&self, enabled: bool) -> Result<(), ProtoError> {
        let s = &self.d().session;
        if enabled {
            let wrapper = self.d().termwrap.status().is_ok_and(|s| matches!(s.state, TermwrapState::On { .. }));
            s.prepare_host(wrapper).map_err(perr)?;
        } else {
            if !self.wait_idle(STOP_WAIT) {
                return Err(ProtoError::new(ErrorCode::SeatRunning, "seats are still stopping"));
            }
            s.restore_host().map_err(perr)?;
        }
        Ok(())
    }

    pub fn termwrap_status(&self) -> Result<serde_json::Value, ProtoError> {
        self.d().termwrap.status().map(|s| termwrap_json(&s)).map_err(perr)
    }

    /// Seats were asked to stop by the controller; wait for them, then switch.
    pub fn set_termwrap(&self, enabled: bool, wrapper: Option<&str>, ack: u32) -> Result<serde_json::Value, ProtoError> {
        if !self.wait_idle(STOP_WAIT) {
            return Err(ProtoError::new(ErrorCode::SeatRunning, "seats are still stopping"));
        }
        let tw = &self.d().termwrap;
        let st = if enabled {
            if ack != seat_session::termwrap::WARNING_VERSION {
                return Err(err(ErrorCode::WarningNotAcknowledged, "warning_not_acknowledged", "read and accept the warning first"));
            }
            let path = match wrapper {
                Some(p) if !p.trim().is_empty() => PathBuf::from(p),
                _ => tw
                    .status()
                    .map_err(perr)?
                    .candidates
                    .first()
                    .map(|c| c.service_dll.clone())
                    .ok_or_else(|| err(ErrorCode::InvalidValue, "wrapper_missing", "no wrapper found").with_field("wrapper_path"))?,
            };
            tw.enable(&path, ack).map_err(perr)?
        } else {
            tw.disable().map_err(perr)?
        };
        Ok(termwrap_json(&st))
    }

    pub fn seat_runtime(&self, seat: &RemoteSeatRecord) -> SeatRuntime {
        let mut out = SeatRuntime { client_name: seat.rdp_client_name.clone(), ..Default::default() };
        let (disp, server, session, audio_report, host) = {
            let seats = lock(&self.0.seats);
            let Some(run) = seats.get(&seat.id) else { return out };
            (
                run.res.display.as_ref().map(|d| (d.backend.clone(), d.handle.clone(), d.hdr)),
                run.res.server.clone(),
                run.res.session.as_ref().map(|s| s.session_id),
                run.audio_report,
                (
                    if run.res.wrds.is_some() { "provider" } else { "loopback" },
                    run.res.fallback.clone(),
                    run.res.wrds.as_ref().and_then(|w| w.device_instance.clone()),
                ),
            )
        };
        out.session_id = session;
        if let Some((b, h, hdr)) = disp {
            let state = b.state(&h).ok();
            let hdr = match hdr {
                Ok(()) => serde_json::Value::String("ok".into()),
                Err(u) => serde_json::Value::String(u.code().into()),
            };
            // `host`: how the session was created (`provider` = our protocol provider and
            // display, `loopback` = RDP client); `fallback`: why the provider path was not
            // taken (2026-11-17, additive).
            out.display = Some(serde_json::json!({
                "backend": h.kind, "state": state, "hdr": hdr,
                "host": host.0, "fallback": host.1, "device_instance": host.2,
            }));
        }
        if let Some(sid) = session {
            let audio = seat_audio(seat);
            if let Ok(policy) = self.d().audio.policy() {
                let endpoints = self.d().audio.endpoints().unwrap_or_default();
                let st = seat_audio_status(&audio, &policy, session_endpoint(&endpoints, sid), audio_report, mic_path(seat));
                out.audio = Some(json(&st));
            }
            out.isolation =
                lock(&self.0.health).values().filter(|h| h.session_id == sid).map(json).collect();
        }
        if let Some(p) = &self.d().pads {
            out.pads = lock(p).status(&seat.id).iter().map(json).collect();
        }
        if let Some(s) = server {
            out.stream = Some(lock(&s).runtime());
        }
        out
    }

    pub fn render_adapters(&self) -> Result<Vec<serde_json::Value>, ProtoError> {
        Ok(self.d().adapters.adapters().map_err(perr)?.iter().map(json).collect())
    }

    pub fn pad_backends(&self) -> Vec<serde_json::Value> {
        self.d().pads.as_ref().map(|p| lock(p).refresh_backends().iter().map(json).collect()).unwrap_or_default()
    }

    pub fn set_pad_preference(&self, pref: Option<PadBackendPref>) {
        if let Some(p) = &self.d().pads {
            lock(p).set_preferred(pref.map(to_backend_id));
        }
    }

    fn running_server(&self, seat: &RemoteSeatRecord) -> Result<Server, ProtoError> {
        self.server_of(&seat.id).ok_or_else(|| ProtoError::new(ErrorCode::ServiceUnavailable, "seat is not running"))
    }

    pub fn pair_state(&self, seat: &RemoteSeatRecord) -> PairState {
        let (mut st, server) = match lock(&self.0.seats).get(&seat.id) {
            Some(r) => (r.pair.clone(), r.res.server.clone()),
            None => return PairState::default(),
        };
        st.backend = seat.stream_backend;
        if let Some(s) = server {
            let s = lock(&s);
            st.available = s.control_ready();
            st.relay_auth_required = s.relay_auth_required();
        }
        st
    }

    /// Asynchronous: the outcome arrives as a `pair_ended` notice.
    pub fn submit_pin(&self, seat: &RemoteSeatRecord, pin: &Secret, client_name: &str, request_id: Option<u32>) -> Result<(), ProtoError> {
        let server = self.running_server(seat)?;
        if lock(&server).backend() != StreamBackendKind::Sunshine {
            return Err(perr(StreamError::WrongBackend(lock(&server).backend())));
        }
        if !lock(&server).control_ready() {
            return Err(err(ErrorCode::Unsupported, "server_without_control_pipe", "pairing is not available on this seat"));
        }
        let me = self.clone();
        let seat_id = seat.id.clone();
        let pin = pin.clone();
        let name = client_name.to_string();
        self.spawn("pair-submit", move || {
            let r = lock(&server).pair_submit(request_id, &pin, &name);
            drop(pin);
            let (result, uuid) = match r {
                Ok(uuid) => ("paired".to_string(), Some(uuid)),
                Err(e) => (e.code().to_string(), None),
            };
            if let Some(run) = lock(&me.0.seats).get_mut(&seat_id) {
                run.pair.last_result = Some(result.clone());
                if uuid.is_some() {
                    run.pair.pending = None;
                }
            }
            me.notice(&seat_id, "pair_ended", serde_json::json!({"request_id": request_id, "result": result, "uuid": uuid}));
        });
        Ok(())
    }

    fn with_stream<T>(&self, seat: &RemoteSeatRecord, f: impl FnOnce(&mut dyn SeatStream) -> Result<T, StreamError>) -> Result<T, ProtoError> {
        let server = self.running_server(seat)?;
        let mut s = lock(&server);
        f(s.as_mut()).map_err(perr)
    }

    pub fn cancel_pair(&self, seat: &RemoteSeatRecord) -> Result<(), ProtoError> {
        let id = lock(&self.0.seats).get(&seat.id).and_then(|r| r.pair.pending.as_ref().and_then(|p| p.request_id));
        self.with_stream(seat, |c| c.pair_cancel(id))?;
        if let Some(run) = lock(&self.0.seats).get_mut(&seat.id) {
            run.pair.pending = None;
        }
        Ok(())
    }

    pub fn paired_clients(&self, seat: &RemoteSeatRecord) -> Result<Vec<PairedClient>, ProtoError> {
        let v = self.with_stream(seat, |c| c.clients_list())?;
        Ok(v.into_iter().map(|c| PairedClient { uuid: c.uuid, name: c.name }).collect())
    }

    pub fn unpair_client(&self, seat: &RemoteSeatRecord, uuid: &str) -> Result<(), ProtoError> {
        self.with_stream(seat, |c| c.unpair(uuid))
    }

    pub fn unpair_all(&self, seat: &RemoteSeatRecord) -> Result<(), ProtoError> {
        self.with_stream(seat, |c| c.unpair_all())
    }

    /// Seat stopped (controller): Sunshine drops its TLS pair; Pulsar gets a new identity,
    /// so the seat's relay id changes.
    pub fn regenerate_identity(&self, seat: &RemoteSeatRecord) -> Result<(), ProtoError> {
        let dir = SeatDir::checked(&self.d().data_root, &seat.id).map_err(perr)?;
        self.backend_for(seat.stream_backend).map_err(perr)?.regenerate_identity(&dir).map_err(perr)
    }

    // ---- stream backends (contracts §7.4, §9.2) ----

    pub fn stream_backends(&self) -> Vec<StreamBackendInfo> {
        availability_all(&self.d().stream.backends)
    }

    pub fn answer_peer_request(&self, seat: &RemoteSeatRecord, request_id: u32, allow: bool, view_only: bool) -> Result<(), ProtoError> {
        self.with_stream(seat, |c| c.pair_answer(request_id, allow, view_only))?;
        if let Some(run) = lock(&self.0.seats).get_mut(&seat.id)
            && run.pair.pending.as_ref().and_then(|p| p.request_id) == Some(request_id)
        {
            run.pair.pending = None;
        }
        Ok(())
    }

    /// Store (DPAPI) then hand over live when the seat runs.
    pub fn set_connect_password(&self, seat: &RemoteSeatRecord, password: Option<&Secret>) -> Result<(), ProtoError> {
        self.backend_for(seat.stream_backend).map_err(perr)?.set_connect_password(&seat.id, password).map_err(perr)?;
        if let Some(s) = self.server_of(&seat.id)
            && let Err(e) = lock(&s).secrets_changed()
        {
            log::warn!("seat '{}': connect password not handed over live: {e}", seat.name);
        }
        Ok(())
    }

    pub fn seat_otp(&self, seat: &RemoteSeatRecord, rotate: bool) -> Result<Secret, ProtoError> {
        self.with_stream(seat, |c| c.otp(rotate))
    }

    pub fn answer_relay_auth(&self, seat: &RemoteSeatRecord, password: &Secret, totp: Option<&Secret>) -> Result<(), ProtoError> {
        self.with_stream(seat, |c| c.relay_auth(password, totp))
    }

    pub fn get_apps(&self, seat: &RemoteSeatRecord) -> Result<Vec<serde_json::Value>, ProtoError> {
        let dir = SeatDir::checked(&self.d().data_root, &seat.id).map_err(perr)?;
        let apps = match self.d().stream.fs.read(&dir.apps).map_err(perr)? {
            Some(b) => parse_apps_json(&String::from_utf8_lossy(&b)).map_err(perr)?.apps,
            None => default_apps(),
        };
        Ok(apps.iter().map(json).collect())
    }

    pub fn set_apps(&self, seat: &RemoteSeatRecord, apps: Vec<serde_json::Value>, global: &RemoteSettings) -> Result<Vec<serde_json::Value>, ProtoError> {
        let mut entries: Vec<AppEntry> = apps
            .into_iter()
            .map(serde_json::from_value)
            .collect::<Result<_, _>>()
            .map_err(|e| err(ErrorCode::InvalidValue, "bad_app_list", e).with_field("apps"))?;
        validate_apps(&entries).map_err(perr)?;
        if global.process_patching
            && let Some(l) = self.d().isolation.launcher_path()
        {
            wrap_with_launcher(&mut entries, &l);
        }
        let fs = self.d().stream.fs.as_ref();
        let dir = SeatDir::checked(&self.d().data_root, &seat.id).map_err(perr)?;
        dir.ensure(fs).map_err(perr)?;
        let env = match fs.read(&dir.apps).map_err(perr)? {
            Some(b) => parse_apps_json(&String::from_utf8_lossy(&b)).map(|f| f.env).unwrap_or_default(),
            None => serde_json::Map::new(),
        };
        fs.write_atomic(&dir.apps, render_apps_json(&entries, &env).as_bytes()).map_err(perr)?;
        if let Some(s) = self.server_of(&seat.id)
            && let Err(e) = lock(&s).reload_apps(&entries)
        {
            log::warn!("apps reload: {e}");
        }
        Ok(entries.iter().map(json).collect())
    }

    /// Last `n` lines of the seat's stream-server log (current file), redacted
    /// (`seat_stream::seatdir::last_lines` + the service's own line filter).
    pub fn seat_log_tail(&self, seat: &RemoteSeatRecord, n: usize) -> Vec<String> {
        let Ok(dir) = SeatDir::checked(&self.d().data_root, &seat.id) else { return Vec::new() };
        let log = match self.backend_for(seat.stream_backend) {
            Ok(b) => b.log_file(&dir),
            Err(_) => dir.log.clone(),
        };
        let bytes = self.d().stream.fs.read(&log).ok().flatten().unwrap_or_default();
        seat_stream::seatdir::last_lines(&bytes, n).iter().map(|l| redact(l)).collect()
    }

    pub fn remove_seat_files(&self, seat: &RemoteSeatRecord) {
        let s = &self.d().stream;
        // Every backend: the seat may have run with the other one before.
        for b in &s.backends {
            if let Err(e) = b.remove_seat(&seat.id, seat.stream.keep_pairing_on_delete) {
                log::warn!("seat '{}': {} cleanup: {e}", seat.name, b.kind().as_str());
            }
        }
        match SeatDir::checked(&self.d().data_root, &seat.id) {
            Ok(dir) => {
                if let Err(e) = dir.delete(s.fs.as_ref(), seat.stream.keep_pairing_on_delete) {
                    log::warn!("seat '{}': seat folder not removed: {e}", seat.name);
                }
                // Pairing kept: the folder stays, but no backend's log (and rotations) may.
                if seat.stream.keep_pairing_on_delete {
                    for b in &s.backends {
                        let lf = b.log_file(&dir);
                        let mut files = vec![lf.clone()];
                        files.extend(seat_stream::seatdir::rotated_names(&lf, seat_stream::seatdir::LOG_KEEP));
                        for f in files {
                            if let Err(e) = s.fs.remove_file(&f) {
                                log::warn!("seat '{}': {} not removed: {e}", seat.name, f.display());
                            }
                        }
                    }
                }
            }
            Err(e) => log::warn!("seat '{}': {e}", seat.name),
        }
    }

    pub fn isolation_status(&self) -> Result<IsolationStatus, ProtoError> {
        let mut s = self.d().isolation.status()?;
        s.health = lock(&self.0.health).values().map(json).collect();
        Ok(s)
    }

    /// `process_patching` on → install (refused when a foreign shim is present), off →
    /// uninstall. HID / Steam switches only change the seat-map flags of new seats.
    pub fn apply_isolation(&self, old: &RemoteSettings, new: &RemoteSettings) -> Result<(), ProtoError> {
        if old.process_patching == new.process_patching {
            return Ok(());
        }
        let iso = &self.d().isolation;
        if new.process_patching {
            if let Some(f) = iso.status()?.foreign {
                return Err(err(ErrorCode::Coexistence, "shim_foreign", f));
            }
            iso.install()
        } else {
            iso.uninstall()
        }
    }

    pub fn set_shim_targets(&self, targets: &[ShimTargetDto]) -> Result<IsolationStatus, ProtoError> {
        let t: Vec<ShimTarget> = targets.iter().map(to_target).collect();
        self.d().isolation.set_targets(&t)?;
        self.isolation_status()
    }

    pub fn set_app_masking(&self, exe_path: &str, enabled: bool) -> Result<IsolationStatus, ProtoError> {
        let p = exe_path.trim();
        if p.is_empty() || !p.to_ascii_lowercase().ends_with(".exe") || p.chars().any(char::is_control) {
            return Err(ProtoError::new(ErrorCode::InvalidValue, "full path of an .exe file expected").with_field("exe_path"));
        }
        self.d().isolation.set_mask(&p.to_lowercase(), enabled)?;
        self.isolation_status()
    }

    pub fn set_dxvk(&self, game_exe: &str, enabled: bool, dlls: &[String]) -> Result<(), ProtoError> {
        for d in dlls {
            if d != "d3d8" && d != "d3d9" {
                return Err(ProtoError::new(ErrorCode::InvalidValue, "dlls: d3d8 or d3d9").with_field("dlls"));
            }
        }
        if enabled && dlls.is_empty() {
            return Err(ProtoError::new(ErrorCode::InvalidValue, "dlls: choose d3d8 and / or d3d9").with_field("dlls"));
        }
        let p = game_exe.trim();
        if p.is_empty() || !Path::new(p).is_absolute() || !p.to_ascii_lowercase().ends_with(".exe") || p.chars().any(char::is_control) {
            return Err(ProtoError::new(ErrorCode::InvalidValue, "full path of the game's .exe file expected").with_field("game_exe"));
        }
        self.d().isolation.dxvk(Path::new(p), enabled, dlls)
    }

    pub fn dxvk_status(&self) -> Result<seat_proto::remote::DxvkStatus, ProtoError> {
        self.d().isolation.dxvk_status()
    }

    /// Spec 70 §2.6: refused while a seat is starting (a new session could take a node's
    /// session id between the plan and the removal).
    pub fn cleanup_audio_endpoints(&self) -> Result<seat_proto::remote::AudioCleanup, ProtoError> {
        if lock(&self.0.seats).values().any(|r| r.stage == Stage::Booting) {
            return Err(err(ErrorCode::SeatRunning, "seat_booting", "wait until every seat finished starting"));
        }
        let r = (self.d().audio_cleanup)().map_err(|e| err(ErrorCode::Internal, e.code(), e))?;
        Ok(seat_proto::remote::AudioCleanup { removed: r.removed, failed: r.failed })
    }
}

/// Status row of one DXVK game from its marker file (`None` = no marker = not deployed).
pub fn dxvk_game(exe: &str, marker: Option<std::io::Result<Vec<u8>>>) -> seat_proto::remote::DxvkGame {
    let mut g = seat_proto::remote::DxvkGame { exe: exe.to_string(), ..Default::default() };
    match marker {
        None => {}
        Some(Err(_)) => g.error = Some("marker_unreadable".into()),
        Some(Ok(b)) => match serde_json::from_slice::<seat_shim_core::dxvk::GameMarker>(&b) {
            Ok(m) => {
                g.deployed = true;
                g.version = Some(m.version);
                let mut dlls: Vec<String> = m
                    .files
                    .iter()
                    .filter_map(|f| f.name.to_ascii_lowercase().strip_suffix(".dll").map(str::to_owned))
                    .collect();
                dlls.sort();
                dlls.dedup();
                g.dlls = dlls;
            }
            Err(_) => g.error = Some("marker_unreadable".into()),
        },
    }
    g
}

/// Largest DXVK game marker the service reads (real ones are well under 1 KiB).
pub const DXVK_MARKER_MAX: u64 = 64 * 1024;

/// Reads a game's DXVK marker for the status page. Game folders are often writable by
/// standard users and this runs as SYSTEM, so: no reparse points (checked with
/// `symlink_metadata`, not followed), regular files only, at most [`DXVK_MARKER_MAX`]
/// bytes (also enforced while reading). Absent = `None`; any refused or failed read is
/// `Some(Err)`, which [`dxvk_game`] reports as `marker_unreadable`.
pub fn read_dxvk_marker(path: &std::path::Path) -> Option<std::io::Result<Vec<u8>>> {
    use std::io::{Error, ErrorKind, Read};
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == ErrorKind::NotFound => return None,
        Err(e) => return Some(Err(e)),
    };
    if !meta.is_file() || meta.len() > DXVK_MARKER_MAX {
        return Some(Err(Error::new(ErrorKind::InvalidData, "marker not a small regular file")));
    }
    let mut bytes = Vec::new();
    Some(
        std::fs::File::open(path)
            .and_then(|f| f.take(DXVK_MARKER_MAX + 1).read_to_end(&mut bytes))
            .and_then(|n| {
                if n as u64 > DXVK_MARKER_MAX {
                    Err(Error::new(ErrorKind::InvalidData, "marker too large"))
                } else {
                    Ok(bytes)
                }
            }),
    )
}

/// Keeps the remembered DXVK game list (case-insensitive, at most 256 entries): a deploy adds
/// the game, an undo removes it.
pub fn dxvk_games_update(list: &mut Vec<String>, exe: &str, enabled: bool) {
    list.retain(|x| !x.eq_ignore_ascii_case(exe));
    if enabled {
        list.push(exe.to_string());
        if list.len() > 256 {
            list.remove(0);
        }
    }
}

/// Log-viewer line filter (service log and seat logs): lines that may carry secrets or
/// pairing material are replaced as a whole.
pub fn redact(l: &str) -> String {
    // Includes the Pulsar secret kinds (one-time password, relay access key, identity).
    const MARKERS: [&str; 14] = [
        "password",
        "passwd",
        "şifre",
        "authorization",
        "secret",
        "private key",
        "-----begin",
        "credential",
        "token",
        "tek kullanımlık",
        "relay_key",
        "access key",
        "access_key",
        "identity",
    ];
    let lower = l.to_lowercase();
    // Whole words only: "otp" is also inside "hotplug".
    let pin_word = lower.split(|c: char| !c.is_alphanumeric()).any(|w| matches!(w, "pin" | "pins" | "otp" | "totp"));
    if pin_word || MARKERS.iter().any(|m| lower.contains(m)) { seat_stream::seatdir::REDACTED_LINE.to_string() } else { l.trim_end().to_string() }
}

fn termwrap_json(s: &seat_session::termwrap::TermwrapStatus) -> serde_json::Value {
    let mut v = json(s);
    if let Some(o) = v.as_object_mut() {
        // English text (the manager shows its own copy in the UI language; same files).
        o.insert("warning_text".into(), serde_json::Value::String(seat_session::termwrap::WARNING_EN.into()));
        o.insert("warning_text_tr".into(), serde_json::Value::String(seat_session::termwrap::WARNING_TR.into()));
    }
    v
}
