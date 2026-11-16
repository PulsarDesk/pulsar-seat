// SPDX-License-Identifier: GPL-3.0-only
//! The service core: shared state, boot sequence (architecture §7), inventory refresh,
//! device ownership, the 500 ms tick (watchdog, hot-plug debounce, stable-run marker) and
//! the ordered stop.
//!
//! Lock discipline: `state` is never held while a backend is called (backends may block
//! and may call our hot-plug sinks from their own threads). Work is snapshotted under the
//! lock, done outside it, and the result is written back with a generation check.

use crate::backends::{self, BackendFactory, Backends};
use crate::events::EventHub;
use crate::inventory::{self, Changes, Inventory};
use crate::platform::{Platform, SessionUser};
use crate::runstate::{self, RunState, RunStore};
use crate::seats::Slot;
use crate::wizard::Wizard;
use crate::{eventid, log};
use seat_core::backend::display::TopologyPlan;
use seat_core::backend::gate::{GatePlan, IsolationStatus, SeatLogon};
use seat_core::backend::input::InputRoute;
use seat_core::backend::{HotplugEvent, HotplugSink};
use seat_core::config::{Config, StartMode};
use seat_core::device::DeviceKind;
use seat_core::state::{ProductState, SafeModeReason, SeatRuntime, SeatState};
use seat_core::store::{ConfigStore, SecretStore};
use seat_core::{Clock, DeviceKey, SeatNumber};
use seat_proto::{AgentTask, AgentTaskKind, Event};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};

pub const SERVICE_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Tick period of the background timer.
pub const TICK_MS: u64 = 500;
/// How often running seat sessions are queried.
pub const SESSION_POLL_MS: u64 = 5_000;
/// Services the boot sequence waits for (spec 20 §13.1), with the RDP-only ones.
pub const WAIT_SERVICES: [&str; 1] = ["PlugPlay"];
pub const WAIT_AUDIO_SERVICES: [&str; 2] = ["AudioEndpointBuilder", "Audiosrv"];
pub const WAIT_RDP_SERVICES: [&str; 1] = ["TermService"];

/// Runs work off the caller's thread (threads in production, inline in tests).
pub trait Spawner: Send + Sync {
    fn spawn(&self, f: Box<dyn FnOnce() + Send>);
}

pub struct ThreadSpawner;

impl Spawner for ThreadSpawner {
    fn spawn(&self, f: Box<dyn FnOnce() + Send>) {
        std::thread::spawn(f);
    }
}

/// Everything the service is built from.
pub struct Deps {
    pub store: Box<dyn ConfigStore>,
    pub secrets: Box<dyn SecretStore>,
    pub run_store: RunStore,
    pub platform: Arc<dyn Platform>,
    pub factory: Arc<dyn BackendFactory>,
    pub clock: Arc<dyn Clock>,
    pub spawner: Arc<dyn Spawner>,
    pub data_dir: PathBuf,
    /// `--console` dev mode (memory secrets, current user on the pipe).
    pub console_mode: bool,
    /// The remote seat worker (merge step 6); `None` = remote seats are refused with
    /// `unsupported` / `remote_worker_unavailable`.
    pub remote: Option<RemoteDeps>,
}

/// Builds the remote seat worker once the service's document view and secret store exist.
pub type RemoteWorkerFactory = Box<dyn FnOnce(crate::remote::DocStore) -> Box<dyn crate::remote::RemoteWorker> + Send>;

pub struct RemoteDeps {
    pub worker: RemoteWorkerFactory,
    /// Engine boot / stop hooks (Windows: the real engine).
    pub host: Option<Arc<dyn crate::remote::RemoteHost>>,
}

pub(crate) struct State {
    pub config: Config,
    pub product: ProductState,
    pub safe_reason: Option<SafeModeReason>,
    pub os_build_changed: bool,
    pub run: RunState,
    pub inventory: Inventory,
    pub seats: BTreeMap<SeatNumber, Slot>,
    pub wizard: Option<Wizard>,
    pub agent_tasks: BTreeMap<u32, Vec<AgentTask>>,
    pub next_task_id: u64,
    pub devices_dirty: bool,
    pub ownership_dirty: bool,
    pub started_ms: u64,
    pub stable_marked: bool,
    /// `after_seat_one_logon`: start the autostart seats at the next console logon.
    pub waiting_seat_one_logon: bool,
    pub last_session_poll_ms: u64,
    pub reboot_pending: bool,
    pub backend_errors: Vec<String>,
    /// Boot phase 1 result ([`Service::boot_prepare`]); `None` until it ran.
    pub boot_decision: Option<runstate::BootDecision>,
    /// Same-account `deny`: session -> (log-off due at ms, user SID). The service logs the
    /// session off itself when due; the agent notice is UI only.
    pub deny_logoff: BTreeMap<u32, (u64, String)>,
    /// The keep-awake power request is held (an extra seat is in use).
    pub keep_awake: bool,
    /// Last input per seat (activity events), for the monitor idle saver.
    pub last_input_ms: BTreeMap<SeatNumber, u64>,
    /// Seats whose monitors the idle saver switched off.
    pub idle_blanked: std::collections::BTreeSet<SeatNumber>,
    /// Hotkey moves applied to the input routes only, not saved yet (spec 50 §4.1.9):
    /// device -> new mask. Saved by `resolve_input_move(true)`, dropped on decline or
    /// service restart.
    pub input_overrides: BTreeMap<DeviceKey, seat_core::SeatMask>,
    /// Console raw-input listener (`raw_input_host`, `crate::listener`).
    pub listener: crate::listener::ListenerSlot,
    /// Launcher requests (`start_seat_app`) per session: start times (ms) inside
    /// [`crate::seats::APP_RATE_WINDOW_MS`], for the per-session rate limit.
    pub app_launches: BTreeMap<u32, std::collections::VecDeque<u64>>,
    /// Credential provider registration and one-time grants (`crate::credprov`).
    pub credprov: crate::credprov::CredprovState,
    /// Remote seats' runtimes from the remote seat worker (`crate::remote::glue`).
    pub remote_rt: BTreeMap<SeatNumber, SeatRuntime>,
    /// Seat 1's kiosk shell under `seat1_desktop = false` (`crate::seat1_kiosk`).
    pub seat1_kiosk: crate::seat1_kiosk::Seat1Kiosk,
    /// Last source-IP publish (`Service::publish_source_ip`, net-wfp); `None` before the
    /// first ownership run.
    pub source_ip: Option<seat_proto::SourceIpReport>,
}

pub struct Service {
    pub(crate) store: Box<dyn ConfigStore>,
    pub(crate) secrets: Arc<dyn SecretStore>,
    pub(crate) run_store: RunStore,
    pub(crate) platform: Arc<dyn Platform>,
    pub(crate) factory: Arc<dyn BackendFactory>,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) spawner: Arc<dyn Spawner>,
    pub(crate) data_dir: PathBuf,
    pub(crate) console_mode: bool,
    pub(crate) state: Mutex<State>,
    backends: Mutex<Backends>,
    pub events: EventHub,
    stopping: AtomicBool,
    /// Grace for a seat host to exit on its own before it is terminated.
    pub(crate) host_grace_ms: std::sync::atomic::AtomicU64,
    /// Cache of "a password is stored" per seat, so `get_config` never decrypts secrets
    /// (spec 00 §5.5.1). Filled by the first `has` and kept up to date by `set`.
    pub(crate) password_known: crate::remote::PasswordCache,
    /// The remote seat worker (`crate::remote::glue`).
    pub(crate) remote: Option<crate::remote::glue::RemoteSlot>,
    /// Longest wait for remote seats to stop (tests set 0).
    pub(crate) remote_stop_wait_ms: std::sync::atomic::AtomicU64,
    weak: Weak<Service>,
    /// Update check / download / install (spec 90 §11 item 10, [`crate::update`]).
    pub(crate) updater: crate::update::Updater,
    /// Wizard work from input events (overlays, progress, save / quit), run in event order
    /// by one worker at a time, so the manager never gets an older progress after a newer
    /// one or after `done`.
    wizard_jobs: Mutex<WizardQueue>,
    /// Serialises wizard start / stop across their backend calls (capture on / off), so
    /// a stop cannot slip in between "wizard stored" and "capture on".
    pub(crate) wizard_op: Mutex<()>,
    /// Serialises `sync_seat1_kiosk` (enable thread, tick, boot, shutdown): one owner of
    /// seat 1's kiosk shell at a time (VM 2026-11-15: disable + enable started two).
    pub(crate) seat1_kiosk_op: Mutex<()>,
}

/// One piece of wizard work queued by an input event.
enum WizardJob {
    Overlays(Vec<(DeviceKey, String)>),
    Action(crate::wizard::WizardAction),
}

#[derive(Default)]
struct WizardQueue {
    jobs: std::collections::VecDeque<WizardJob>,
    running: bool,
}

impl Service {
    /// Build the service; loads the config (a broken file is kept aside and defaults used).
    pub fn new(d: Deps) -> Arc<Self> {
        let config = match d.store.load() {
            Ok(c) => c,
            Err(e) => {
                log::error_id(eventid::CONFIG_STORE_FAILED, &format!("config unreadable, using defaults: {e}"));
                Config::default()
            }
        };
        log::set_levels(&config.settings.log);
        let backends = backends::select(d.factory.as_ref(), &config.settings);
        let now = d.clock.now_ms();
        let seats = config.seats.iter().map(|s| (s.number, Slot::new(s.number))).collect();
        let product = if config.settings.enabled { ProductState::Enabled { started: false } } else { ProductState::Disabled };
        let secrets: Arc<dyn SecretStore> = Arc::from(d.secrets);
        let password_known = crate::remote::PasswordCache::default();
        let remote = d.remote.map(|r| {
            let doc = crate::remote::SharedDoc::default();
            let store = crate::remote::DocStore::new(doc.clone(), secrets.clone(), password_known.clone());
            crate::remote::glue::RemoteSlot::new((r.worker)(store), doc, r.host)
        });
        let events = EventHub::new(d.clock.clone());
        events.set_remote_seats(remote_seats(&config));
        Arc::new_cyclic(|weak| Service {
            events,
            store: d.store,
            secrets,
            run_store: d.run_store,
            platform: d.platform,
            factory: d.factory,
            clock: d.clock,
            spawner: d.spawner,
            data_dir: d.data_dir,
            console_mode: d.console_mode,
            state: Mutex::new(State {
                config,
                product,
                safe_reason: None,
                os_build_changed: false,
                run: RunState::default(),
                inventory: Inventory::default(),
                seats,
                wizard: None,
                agent_tasks: BTreeMap::new(),
                next_task_id: 1,
                devices_dirty: true,
                ownership_dirty: false,
                started_ms: now,
                stable_marked: false,
                waiting_seat_one_logon: false,
                last_session_poll_ms: now,
                reboot_pending: false,
                backend_errors: Vec::new(),
                boot_decision: None,
                deny_logoff: BTreeMap::new(),
                keep_awake: false,
                last_input_ms: BTreeMap::new(),
                idle_blanked: Default::default(),
                input_overrides: BTreeMap::new(),
                listener: Default::default(),
                app_launches: BTreeMap::new(),
                credprov: Default::default(),
                remote_rt: BTreeMap::new(),
                seat1_kiosk: Default::default(),
                source_ip: None,
            }),

            password_known,
            remote,
            remote_stop_wait_ms: std::sync::atomic::AtomicU64::new(crate::remote::glue::REMOTE_STOP_WAIT_MS),
            wizard_jobs: Mutex::new(WizardQueue::default()),
            wizard_op: Mutex::new(()),
            seat1_kiosk_op: Mutex::new(()),
            backends: Mutex::new(backends),
            stopping: AtomicBool::new(false),
            host_grace_ms: std::sync::atomic::AtomicU64::new(crate::seats::HOST_EXIT_GRACE_MS),
            weak: weak.clone(),
            updater: crate::update::Updater::default(),
        })
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn backends(&self) -> Backends {
        self.backends.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub(crate) fn arc(&self) -> Arc<Service> {
        self.weak.upgrade().expect("service alive")
    }

    pub(crate) fn now(&self) -> u64 {
        self.clock.now_ms()
    }

    pub(crate) fn spawn(&self, f: impl FnOnce(Arc<Service>) + Send + 'static) {
        let me = self.arc();
        self.spawner.spawn(Box::new(move || f(me)));
    }

    pub fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }

    // ---- topology -----------------------------------------------------------------

    /// `dedicated_host`: every seat, seat 1 included, is a session of its own and the
    /// console session belongs to the host account (architecture §3).
    pub(crate) fn dedicated(st: &State) -> bool {
        st.config.settings.backends.host_topology == seat_core::backend::session::HostTopology::DedicatedHost
    }

    /// The service starts and stops this seat itself (everything except the console
    /// seat 1 of `seat_one_console`).
    pub(crate) fn managed(st: &State, seat: SeatNumber) -> bool {
        !seat.is_console() || Self::dedicated(st)
    }

    // ---- product state ------------------------------------------------------------

    pub(crate) fn recompute_product(&self, st: &mut State) {
        let next = if !st.config.settings.enabled {
            ProductState::Disabled
        } else if let Some(reason) = st.safe_reason {
            ProductState::SafeMode { reason }
        } else {
            let started = st.seats.values().any(|s| Self::managed(st, s.seat) && s.rt.session_id.is_some());
            ProductState::Enabled { started }
        };
        if next != st.product {
            st.product = next.clone();
            self.events.publish(Event::ProductState { state: next });
        }
    }

    pub(crate) fn notice(&self, code: &str, seat: Option<SeatNumber>, detail: impl Into<String>) {
        self.events.publish(Event::Notice { code: code.into(), seat, detail: detail.into() });
    }

    pub(crate) fn save_run(&self, st: &State) {
        if let Err(e) = self.run_store.save(&st.run) {
            log::warn(&format!("run state not saved: {e}"));
        }
    }

    // ---- boot ---------------------------------------------------------------------

    /// Boot phase 1, synchronous and quick: load the run state and decide the safe mode
    /// (Windows Safe Mode, unclean shutdown, crash loop). Runs before the control pipe is
    /// claimed, so no request sees a default run state or overwrites `state.json` with
    /// one. Idempotent.
    pub fn boot_prepare(&self) {
        let p = self.platform.clone();
        let mut st = self.lock();
        if st.boot_decision.is_some() {
            return;
        }
        let prev = self.run_store.load();
        let decision =
            runstate::boot_decision(&prev, p.boot_unix_s(), p.os_build(), p.windows_safe_mode(), &st.config.settings);
        st.run = decision.next.clone();
        st.safe_reason = decision.safe_mode;
        st.os_build_changed = decision.os_build_changed;
        st.boot_decision = Some(decision.clone());
        self.save_run(&st);
        self.recompute_product(&mut st);
        drop(st);
        if let Some(r) = decision.safe_mode {
            log::error_id(eventid::SAFE_MODE, &format!("safe mode: {}", backends::kind_name(&r)));
            self.notice("safe_mode", None, backends::kind_name(&r));
        }
        if decision.os_build_changed {
            log::info_id(eventid::OS_BUILD_CHANGED, &format!("Windows build changed {} -> {}", prev.os_build, p.os_build()));
            self.notice("os_build_changed", None, format!("{} -> {}", prev.os_build, p.os_build()));
        }
    }

    /// Boot sequence. Blocks while it waits for Windows services; run it off the SCM thread.
    pub fn boot(&self) {
        let p = self.platform.clone();
        self.boot_prepare();
        let (settings, decision) = {
            let st = self.lock();
            (st.config.settings.clone(), st.boot_decision.clone().expect("boot_prepare ran"))
        };
        if decision.safe_mode == Some(SafeModeReason::WindowsSafeMode) {
            log::info("Windows Safe Mode: serving status only");
            return;
        }

        // Wait for the device and session layers (SCM status, no fixed sleeps).
        let b = self.backends();
        let mut names: Vec<&str> = WAIT_SERVICES.to_vec();
        if b.session.kind().uses_rdp() {
            names.extend(WAIT_RDP_SERVICES);
        }
        let late = p.wait_services(&names, std::time::Duration::from_secs(120));
        if !late.is_empty() {
            log::warn(&format!("services not running after wait: {late:?}"));
        }
        if b.audio.kind() == seat_core::backend::AudioBackendKind::RdpBridge {
            let t = std::time::Duration::from_secs(u64::from(settings.timings.audio_wait_timeout_s));
            let late = p.wait_services(&WAIT_AUDIO_SERVICES, t);
            if !late.is_empty() {
                log::warn(&format!("audio services not running: {late:?}"));
            }
        }
        self.install_sinks();
        self.refresh_inventory();
        self.adopt_sessions();
        self.refresh_console_seat();

        let enabled = settings.enabled;
        self.ensure_images();
        self.sync_tweaks();
        if enabled {
            // Who may use the passwordless-seat listener (before it is created).
            self.sync_listener_plan();
            match b.session.prepare_host(&settings) {
                Ok(notes) if !notes.is_empty() => log::info(&format!("host prepared: {notes:?}")),
                Ok(_) => {}
                Err(e) => log::error_id(eventid::BACKEND_FAILED, &format!("prepare_host: {e}")),
            }
            self.apply_ownership();
            if decision.safe_mode.is_none() {
                self.sync_seat1_kiosk();
            }
        }
        // Remote seats: the engine prepares the host (coexistence, termwrap, RDS, sweeps)
        // before any remote seat starts (contracts §9.4).
        if enabled {
            self.remote_boot();
        }
        let autostart = enabled && decision.safe_mode.is_none();
        if autostart {
            match settings.start_mode {
                StartMode::Manual => log::info("start mode manual: waiting for the manager"),
                StartMode::AtBoot => {
                    let delay = u64::from(settings.timings.start_delay_ms);
                    self.spawn(move |s| {
                        if delay > 0 {
                            std::thread::sleep(std::time::Duration::from_millis(delay));
                        }
                        s.start_autostart_seats();
                    });
                }
                StartMode::AfterSeatOneLogon => self.start_after_seat_one_logon(),
            }
        }
        log::info_id(eventid::SERVICE_STARTED, &format!("service {SERVICE_VERSION} ready"));
    }

    /// `after_seat_one_logon`: start the autostart seats now if somebody is signed in at
    /// the console, else at the next console logon. The flag is raised before the console
    /// is checked, and both paths take it with a swap, so a logon racing this call starts
    /// the seats exactly once.
    pub(crate) fn start_after_seat_one_logon(&self) {
        self.lock().waiting_seat_one_logon = true;
        let signed_in = self.platform.console_session().and_then(|c| self.platform.session_user(c)).is_some();
        if signed_in && self.take_seat_one_wait() {
            self.spawn(|s| s.start_autostart_seats());
        }
    }

    /// Clear the "waiting for the console logon" flag; true when it was set.
    pub(crate) fn take_seat_one_wait(&self) -> bool {
        std::mem::replace(&mut self.lock().waiting_seat_one_logon, false)
    }

    /// Hot-plug sinks of every backend mark the inventory dirty (debounced in `tick`).
    pub(crate) fn install_sinks(&self) {
        let b = self.backends();
        let weak = self.weak.clone();
        let sink: HotplugSink = Arc::new(move |ev: HotplugEvent| {
            if let Some(s) = weak.upgrade() {
                s.on_hotplug(ev);
            }
        });
        b.display.set_hotplug_sink(sink.clone());
        b.input.set_hotplug_sink(sink.clone());
        b.audio.set_hotplug_sink(sink.clone());
        for g in &b.gates {
            g.set_hotplug_sink(sink.clone());
        }
        let weak = self.weak.clone();
        b.input.set_activity_sink(Arc::new(move |ev| {
            if let Some(s) = weak.upgrade() {
                s.on_activity(ev);
            }
        }));
        let weak = self.weak.clone();
        b.input.set_packet_sink(Arc::new(move |seat, packets| {
            if let Some(s) = weak.upgrade() {
                s.events.publish(Event::InputPackets { seat, packets: packets.to_vec() });
            }
        }));
        let weak = self.weak.clone();
        b.input.set_hotkey_sink(Arc::new(move |ev| {
            if let Some(s) = weak.upgrade() {
                s.on_hotkey(ev);
            }
        }));
        if let Some(c) = self.factory.input_control(b.input.kind()) {
            let weak = self.weak.clone();
            c.set_emergency_sink(Arc::new(move || {
                if let Some(s) = weak.upgrade() {
                    s.on_emergency();
                }
            }));
        }
    }

    /// Emergency combination (seat-input): every keyboard, mouse and HID unit back to seat 1,
    /// as a config change with a new revision. Pending hotkey moves are dropped (they would
    /// win over the config in the routes), and a running capture wizard is cancelled.
    pub(crate) fn on_emergency(&self) {
        log::warn("emergency combination: all input units back to seat 1");
        if self.lock().wizard.is_some() {
            if let Err(e) = self.wizard_stop(false) {
                log::warn(&format!("emergency: wizard not stopped: {}", e.message));
            }
        }
        let result = {
            let mut st = self.lock();
            let overrides = std::mem::take(&mut st.input_overrides);
            let mut cfg = st.config.clone();
            let units: Vec<(DeviceKey, DeviceKind, String)> = st
                .inventory
                .live()
                .filter(|d| matches!(d.kind, DeviceKind::Keyboard | DeviceKind::Mouse | DeviceKind::Hid))
                .map(|d| (d.key.clone(), d.kind, d.name.clone()))
                .collect();
            for (k, kind, name) in &units {
                set_assignment(&mut cfg, k, *kind, SeatNumber::CONSOLE.mask(), name);
            }
            let r = self.commit_config(&mut st, cfg);
            if r.is_err() {
                st.input_overrides = overrides;
            }
            r
        };
        match result {
            Ok(_) => self.apply_ownership(),
            Err(e) => log::warn(&format!("emergency move not saved: {e}")),
        }
    }

    fn on_hotplug(&self, ev: HotplugEvent) {
        log::debug(&format!("hot-plug: {:?}", std::mem::discriminant(&ev)));
        self.lock().devices_dirty = true;
    }

    pub(crate) fn on_activity(&self, ev: seat_core::backend::input::ActivityEvent) {
        let now = self.now();
        // Never the scan code: during the wizard every key is reported, and a trace-level
        // log must not turn into a keystroke log.
        log::trace(&format!("activity {} key={} click={}", ev.key, ev.key_sc.is_some(), ev.click));
        let start_worker = {
            let mut st = self.lock();
            let State { wizard, inventory, .. } = &mut *st;
            let event = wizard.as_mut().and_then(|w| {
                let picked = w.picked;
                let a = w.on_activity(&ev, inventory, now)?;
                Some((a, (!picked && w.picked).then(|| w.overlay_labels())))
            });
            // Queued while the state lock is still held: the queue order is the order the
            // wizard saw the events in.
            event.is_some_and(|(action, relabel)| {
                let mut q = self.wizard_jobs.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(labels) = relabel {
                    q.jobs.push_back(WizardJob::Overlays(labels));
                }
                q.jobs.push_back(WizardJob::Action(action));
                !std::mem::replace(&mut q.running, true)
            })
        };
        if start_worker {
            // Overlays (console helper) and save / quit (capture off, config write) must not
            // block the input thread this event came from: run them on a worker.
            self.spawn(|s| s.drain_wizard_jobs());
        }
        self.note_input(&ev.key);
        self.events.publish(Event::DeviceActivity { key: ev.key });
    }

    /// Run queued wizard jobs in order until the queue is empty.
    fn drain_wizard_jobs(&self) {
        loop {
            let job = {
                let mut q = self.wizard_jobs.lock().unwrap_or_else(|e| e.into_inner());
                match q.jobs.pop_front() {
                    Some(j) => j,
                    None => {
                        q.running = false;
                        return;
                    }
                }
            };
            match job {
                WizardJob::Overlays(labels) => self.wizard_overlays(&labels, true),
                WizardJob::Action(action) => {
                    if let Err(e) = self.wizard_action(action) {
                        log::warn(&format!("wizard key action: {}", e.message));
                    }
                }
            }
        }
    }

    /// Ctrl+hotkey: move the pressing keyboard to the next seat together with the mouse
    /// units of the same physical device or seat (spec 50 §4.1.9): mice and HID collections
    /// (consumer / system control) on the same parent, and mice explicitly on the keyboard's
    /// seat (by a saved assignment or a pending move; not from seat 1, whose other mice
    /// always stay). The move
    /// applies at once to the input routes only and is saved only when the admin confirms it
    /// in the manager (`resolve_input_move`); the `input_move_pending` notice asks for that.
    /// Ignored while the capture wizard runs (spec 50 §4.1.8: units go to the service only).
    pub(crate) fn on_hotkey(&self, ev: seat_core::backend::input::HotkeyEvent) {
        let (next, moved) = {
            let mut st = self.lock();
            if st.wizard.is_some() {
                log::info("hotkey ignored while the capture wizard runs");
                return;
            }
            let count = st.config.seat_count();
            let next = SeatNumber::new(if ev.from.get() >= count { 1 } else { ev.from.get() + 1 }).unwrap_or(SeatNumber::CONSOLE);
            let mut moved: Vec<DeviceKey> = vec![ev.key.clone()];
            let parent = st.inventory.get(&ev.key).and_then(|e| e.device.parent_hub.clone());
            if let Some(p) = parent {
                moved.extend(
                    st.inventory
                        .live()
                        .filter(|d| matches!(d.kind, DeviceKind::Mouse | DeviceKind::Hid) && d.parent_hub.as_ref() == Some(&p))
                        .map(|d| d.key.clone()),
                );
            }
            // Seat 1 is the admin's console seat (and every unit's fallback): its other mice
            // never follow a keyboard away from it.
            let from_mask = ev.from.mask();
            let seat_mice: Vec<DeviceKey> = st
                .inventory
                .effective(&st.config)
                .into_iter()
                .filter(|e| e.kind == DeviceKind::Mouse && ev.from != SeatNumber::CONSOLE)
                .filter(|e| match st.input_overrides.get(&e.key) {
                    Some(m) => *m == from_mask,
                    None => e.source == seat_core::assign::MaskSource::Explicit && e.mask == from_mask,
                })
                .map(|e| e.key)
                .collect();
            for k in seat_mice {
                if !moved.contains(&k) {
                    moved.push(k);
                }
            }
            for k in &moved {
                st.input_overrides.insert(k.clone(), next.mask());
            }
            (next, moved)
        };
        log::info(&format!("hotkey: {} input unit(s) moved to seat {next} until confirmed", moved.len()));
        self.apply_ownership();
        self.notice("input_move_pending", Some(next), moved.iter().map(|k| k.to_string()).collect::<Vec<_>>().join(","));
    }

    /// Confirm (`accept`: saved as assignments, new config revision) or decline (routes
    /// back to the saved config) the pending hotkey moves.
    pub(crate) fn resolve_input_move(&self, accept: bool) -> Result<usize, seat_core::store::StoreError> {
        let n = {
            let mut st = self.lock();
            let moves = std::mem::take(&mut st.input_overrides);
            if accept && !moves.is_empty() {
                let mut cfg = st.config.clone();
                for (k, mask) in &moves {
                    let kind = st.inventory.get(k).map(|e| e.device.kind).unwrap_or(DeviceKind::Keyboard);
                    let name = st.inventory.get(k).map(|e| e.device.name.clone()).unwrap_or_default();
                    set_assignment(&mut cfg, k, kind, *mask, &name);
                }
                if let Err(e) = self.commit_config(&mut st, cfg) {
                    st.input_overrides = moves;
                    return Err(e);
                }
            }
            moves.len()
        };
        self.apply_ownership();
        Ok(n)
    }

    /// Save a config as the next revision (caller validated it) and publish the change.
    pub(crate) fn commit_config(&self, st: &mut State, mut cfg: Config) -> Result<u64, seat_core::store::StoreError> {
        cfg.normalize();
        cfg.revision = st.config.revision + 1;
        cfg.schema_version = seat_core::config::CONFIG_SCHEMA_VERSION;
        self.store.save(&cfg)?;
        let rev = cfg.revision;
        for s in &cfg.seats {
            st.seats.entry(s.number).or_insert_with(|| Slot::new(s.number));
        }
        log::set_levels(&cfg.settings.log);
        self.events.set_remote_seats(remote_seats(&cfg));
        let old = std::mem::replace(&mut st.config, cfg);
        self.update_seat_jobs(st, &old);
        st.run.applied_revision = rev;
        self.save_run(st);
        self.events.publish(Event::ConfigChanged { revision: rev });
        Ok(rev)
    }

    // ---- inventory and ownership --------------------------------------------------

    /// Re-enumerate every backend + the baseline; publish device events.
    pub fn refresh_inventory(&self) -> Changes {
        let b = self.backends();
        let baseline = self.platform.scan_devices();
        let (live, errors) = inventory::collect(&b, baseline);
        let now = self.now();
        let mut st = self.lock();
        st.devices_dirty = false;
        st.backend_errors = errors;
        let cfg = st.config.clone();
        let ch = st.inventory.rebuild(live, &cfg, now);
        for k in &ch.arrived {
            if let Some(v) = st.inventory.view(k, &cfg) {
                self.events.publish(Event::DeviceArrived { view: Box::new(v) });
            }
        }
        for k in &ch.removed {
            self.events.publish(Event::DeviceRemoved { key: k.clone() });
        }
        for k in &ch.changed {
            if let Some(v) = st.inventory.view(k, &cfg) {
                self.events.publish(Event::DeviceChanged { view: Box::new(v) });
            }
        }
        if !ch.is_empty() {
            st.ownership_dirty = true;
            if ch.touches_kind(&st.inventory, DeviceKind::Monitor) {
                for s in st.seats.values_mut() {
                    if s.rt.state == SeatState::WaitingForDisplay {
                        s.retry_at = Some(now);
                    }
                }
            }
        }
        ch
    }

    /// Logons of every seat with a signed-in user (gate plans need the logon SIDs).
    fn logons(st: &State) -> Vec<SeatLogon> {
        st.seats
            .values()
            .filter_map(|s| {
                let u: &SessionUser = s.logon.as_ref()?;
                Some(SeatLogon { seat: s.seat, session_id: s.rt.session_id?, logon_sid: u.logon_sid.clone(), user_sid: u.sid.clone() })
            })
            .collect()
    }

    /// Apply input routes, console topology and gate plans for the current config
    /// (idempotent; boot, config apply, hot-plug, every seat logon / logoff).
    /// The console topology of the effective monitor masks: console monitors stay in the
    /// desktop, single-seat monitors are reserved for their seat.
    pub(crate) fn display_plan(eff: &[seat_core::assign::Effective], dedicated: bool) -> TopologyPlan {
        let mut plan = TopologyPlan { console: Vec::new(), seats: Vec::new() };
        let mut per_seat: BTreeMap<SeatNumber, Vec<DeviceKey>> = BTreeMap::new();
        for e in eff.iter().filter(|e| e.kind == DeviceKind::Monitor) {
            if dedicated && e.mask.single() == Some(SeatNumber::CONSOLE) {
                // dedicated_host: seat 1 is a session of its own; its monitors are bound.
                per_seat.entry(SeatNumber::CONSOLE).or_default().push(e.key.clone());
            } else if e.mask.contains(SeatNumber::CONSOLE) {
                plan.console.push(e.key.clone());
            } else if let Some(s) = e.mask.single() {
                per_seat.entry(s).or_default().push(e.key.clone());
            }
        }
        plan.seats = per_seat.into_iter().collect();
        plan
    }

    /// `OutputSettings::force_assign_at_start` (spec 10 §7.5): at each seat start, apply
    /// the recorded monitor-to-seat topology again through the CCD path, even when nothing
    /// changed since the last ownership run (Windows may have rearranged the outputs, e.g.
    /// after a driver reset). Per-surface output choices (`AssignmentOptions::outputs`) are
    /// applied by `bind_seat` itself on every start. Untested on hardware.
    pub(crate) fn force_assign_outputs(&self) {
        let (eff, dedicated) = {
            let st = self.lock();
            if !st.config.settings.outputs.force_assign_at_start {
                return;
            }
            (st.inventory.effective(&st.config), Self::dedicated(&st))
        };
        let plan = Self::display_plan(&eff, dedicated);
        match self.backends().display.apply_topology(&plan) {
            Ok(_) => log::info("outputs force-assigned at seat start"),
            Err(e) => log::warn(&format!("force-assign at seat start: {e}")),
        }
    }

    /// Source IP per seat (spec 80 §11.3, net-wfp 2026-11-17): push the session ->
    /// address map of the signed-in seats (local logons plus running remote seats) to the
    /// WFP callout and remember how the addresses are applied (`get_backends`). Seats that
    /// share a session are left out (the driver decides per session). Without the driver
    /// the `PULSAR_SEAT_SOURCE_IP` fallback of the launcher stays the only path.
    pub(crate) fn publish_source_ip(&self, logons: &[SeatLogon]) {
        use seat_proto::{SourceIpEnforcement, SourceIpReport};
        let (entries, shared) = {
            let st = self.lock();
            let mut all: Vec<(SeatNumber, u32)> = logons.iter().map(|l| (l.seat, l.session_id)).collect();
            for (n, rt) in &st.remote_rt {
                if let Some(sid) = rt.session_id
                    && !all.iter().any(|(s, _)| s == n)
                {
                    all.push((*n, sid));
                }
            }
            source_ip_entries(&all, &st.config)
        };
        let r = self.platform.publish_source_ip_map(&entries);
        let report = match r.enforcement {
            seat_compat::wfp::Enforcement::Driver { seats, .. } => SourceIpReport {
                enforcement: SourceIpEnforcement::Driver,
                reason: None,
                enforced: seats.into_iter().filter_map(|n| u8::try_from(n).ok().and_then(SeatNumber::new)).collect(),
                shared_session: shared,
                dropped_paths: r.dropped_paths,
            },
            seat_compat::wfp::Enforcement::EnvironmentOnly { reason } => SourceIpReport {
                enforcement: SourceIpEnforcement::Environment,
                reason: Some(reason),
                enforced: Vec::new(),
                shared_session: shared,
                dropped_paths: r.dropped_paths,
            },
        };
        let mut st = self.lock();
        if st.source_ip.as_ref() != Some(&report) {
            let seats: Vec<String> = report.enforced.iter().map(|s| s.get().to_string()).collect();
            log::info(&format!(
                "source ip: {:?}{} enforced=[{}] shared_session={} dropped_paths={}",
                report.enforcement,
                report.reason.as_deref().map(|r| format!(" ({r})")).unwrap_or_default(),
                seats.join(","),
                report.shared_session.len(),
                report.dropped_paths.len()
            ));
        }
        st.source_ip = Some(report);
    }

    pub fn apply_ownership(&self) {

        let (eff, settings, logons, dedicated, revision) = {
            let mut st = self.lock();
            st.ownership_dirty = false;
            if !st.config.settings.enabled || st.safe_reason == Some(SafeModeReason::WindowsSafeMode) {
                drop(st);
                // Multiseat off: no seat address is enforced either.
                self.publish_source_ip(&[]);
                return;
            }
            let eff = st.inventory.effective(&st.config);
            (eff, st.config.settings.clone(), Self::logons(&st), Self::dedicated(&st), st.config.revision)
        };
        let b = self.backends();
        if let Some(c) = self.factory.input_control(b.input.kind()) {
            c.set_config_revision(revision);
            let all: Vec<(SeatNumber, u32)> = logons.iter().map(|l| (l.seat, l.session_id)).collect();
            let sessions = filter_session_map(&all, self.platform.console_session(), dedicated);
            if let Err(e) = c.set_seat_sessions(&sessions) {
                log::warn(&format!("input seat sessions: {e}"));
            }
        }
        // Kiosk seat -> device map for the per-process shim (round 4 kiosk-isolation).
        {
            let all: Vec<(SeatNumber, u32)> = logons.iter().map(|l| (l.seat, l.session_id)).collect();
            let entries = {
                let st = self.lock();
                let kiosk: std::collections::BTreeSet<SeatNumber> = st
                    .config
                    .seats
                    .iter()
                    .filter(|c| seat_session::kiosk::wants_shell(c.number, c, &st.config.settings))
                    .map(|c| c.number)
                    .collect();
                kiosk_map_entries(&all, &kiosk, self.platform.console_session(), dedicated, &eff, |k| {
                    st.inventory.get(k).and_then(|e| e.device.instance_id.clone())
                })
            };
            if let Err(e) = self.platform.publish_kiosk_map(&entries) {
                log::warn(&format!("kiosk seat map: {e}"));
            }
        }
        // Source IP per seat (net-wfp): session -> address map for the WFP callout.
        self.publish_source_ip(&logons);
        let mut iso: HashMap<DeviceKey, IsolationStatus> = HashMap::new();
        let overrides = self.lock().input_overrides.clone();
        let routes: Vec<InputRoute> = eff
            .iter()
            .filter(|e| matches!(e.kind, DeviceKind::Keyboard | DeviceKind::Mouse | DeviceKind::Hid))
            .map(|e| InputRoute { key: e.key.clone(), mask: overrides.get(&e.key).copied().unwrap_or(e.mask) })
            .collect();
        match b.input.apply_routes(&routes) {
            Ok(r) => {
                for k in r.not_routable {
                    iso.insert(k, IsolationStatus::NotSupported);
                }
            }
            Err(e) => log::error_id(eventid::OWNERSHIP_FAILED, &format!("input routes: {e}")),
        }
        // Touch screens and pens (HID digitizer page 0x0D) on a seat-host seat: the seat
        // host's RDP control has no touch / pen input API, so their packets are dropped
        // (seat-host `translate_input`). Show them as not routable until a client with
        // RDPEI support exists (spec 50 §4.1.4).
        {
            let st = self.lock();
            for r in &routes {
                let hosted = r.mask.single().is_some_and(|s| !s.is_console() || dedicated);
                let digitizer = st.inventory.get(&r.key).is_some_and(|e| {
                    matches!(&e.device.details, seat_core::device::DeviceDetails::Input(i) if i.usage_page == 0x0d)
                });
                if hosted && digitizer {
                    iso.insert(r.key.clone(), IsolationStatus::NotSupported);
                }
            }
        }
        let audio_masks: Vec<(DeviceKey, seat_core::SeatMask)> =
            eff.iter().filter(|e| matches!(e.kind, DeviceKind::AudioRender | DeviceKind::AudioCapture)).map(|e| (e.key.clone(), e.mask)).collect();
        self.factory.set_audio_masks(b.audio.kind(), audio_masks);
        let plan = Self::display_plan(&eff, dedicated);
        if let Err(e) = b.display.apply_topology(&plan) {
            log::error_id(eventid::OWNERSHIP_FAILED, &format!("display topology: {e}"));
        }
        let gate_plan = GatePlan {
            devices: eff.iter().filter(|e| e.kind != DeviceKind::Network).map(|e| (e.key.clone(), e.mask)).collect(),
            logons,
            admins_bypass: settings.devices.admins_bypass,
            class_defaults: settings.devices.class_defaults(),
            printer_hosts: self.printer_hosts(),
        };
        for g in &b.gates {
            match g.apply(&gate_plan) {
                Ok(r) => {
                    for (k, s) in r.applied {
                        // Several gates may report one device: an enforcing status wins
                        // over another gate's "not supported" (printers: `printer_acl`
                        // gates them, `device_security` does not).
                        match iso.get(&k) {
                            Some(old) if s == IsolationStatus::NotSupported && *old != IsolationStatus::NotSupported => {}
                            _ => {
                                iso.insert(k, s);
                            }
                        }
                    }
                    for (k, e) in r.failed {
                        log::warn(&format!("gate {} failed on {k}: {e}", backends::kind_name(&g.kind())));
                    }
                    for k in r.vetoed {
                        self.notice("device_change_vetoed", None, k.to_string());
                    }
                }
                Err(e) => log::error_id(eventid::OWNERSHIP_FAILED, &format!("gate {}: {e}", backends::kind_name(&g.kind()))),
            }
        }
        // Session drive letters: the seat's agent defines / removes them in its logon session.
        let tasks = self.factory.take_gate_agent_tasks();
        if !tasks.is_empty() {
            let mut st = self.lock();
            for (session_id, task) in tasks {
                self.queue_agent_task(&mut st, session_id, task);
            }
        }
        for e in &eff {
            if e.kind == DeviceKind::Network {
                iso.insert(e.key.clone(), IsolationStatus::Network);
            } else if e.kind == DeviceKind::Printer && !e.mask.is_shared() {
                // Without the optional `printer_acl` gate (spec 70 §5.2(5)) a printer given
                // to one seat is still usable from every session, so say so; with it the
                // gate's `descriptor` status is already in `iso`.
                iso.entry(e.key.clone()).or_insert(IsolationStatus::NotSupported);
            } else if e.mask.is_shared() {
                iso.entry(e.key.clone()).or_insert(IsolationStatus::Shared);
            }
        }
        self.lock().inventory.set_isolation(iso);
    }

    /// Give everything back to seat 1 (product disabled / reset).
    pub(crate) fn release_ownership(&self) {
        let monitors: Vec<DeviceKey> = {
            let st = self.lock();
            st.inventory.live().filter(|d| d.kind == DeviceKind::Monitor).map(|d| d.key.clone()).collect()
        };
        let b = self.backends();
        if let Err(e) = b.input.apply_routes(&[]) {
            log::warn(&format!("input routes reset: {e}"));
        }
        if let Err(e) = b.display.apply_topology(&TopologyPlan { console: monitors, seats: Vec::new() }) {
            log::warn(&format!("display topology reset: {e}"));
        }
        for g in &b.gates {
            if let Err(e) = g.revert_all() {
                log::warn(&format!("gate revert {}: {e}", backends::kind_name(&g.kind())));
            }
        }
        let tasks = self.factory.take_gate_agent_tasks();
        if !tasks.is_empty() {
            let mut st = self.lock();
            for (session_id, task) in tasks {
                self.queue_agent_task(&mut st, session_id, task);
            }
        }
        if let Err(e) = b.session.restore_host() {
            log::warn(&format!("restore_host: {e}"));
        }
        self.lock().inventory.set_isolation(HashMap::new());
    }

    /// Backends chosen with a fallback are chosen again once its reason may be gone: on
    /// enable and on a console logon (VM 2026-11-15: the service starts before the console
    /// user's auto-logon, `desktops` then needs a signed-in console and the service fell
    /// back to `fast_user_switch` for good, so a later enable never got `desktops` and its
    /// input / display backends). Re-selects only when a fresh selection differs in a
    /// backend kind, so working backends are not rebuilt. Returns whether `prepare_host` ran.
    pub(crate) fn reselect_after_fallback(&self) -> bool {
        let old = self.backends();
        if old.fallback_notes.is_empty() {
            return false;
        }
        let settings = self.lock().config.settings.clone();
        let fresh = backends::select(self.factory.as_ref(), &settings);
        if backends::same_kinds(&old, &fresh) {
            return false;
        }
        log::info(&format!("backends re-selected after a fallback: {:?} -> {:?}", old.fallback_notes, fresh.fallback_notes));
        self.reselect_backends()
    }

    /// Re-select backends after a backend setting changed.
    /// A gate that leaves the active set (unticked, or no longer available) is reverted
    /// first (spec 70 §5.2(2) "restored on disable", §5.2(5)): nothing would restore its
    /// descriptors while the service runs otherwise.
    ///
    /// When the session backend kind changes while the product is enabled, the old one's
    /// host changes are undone (`restore_host`: RDS registry values, the second loopback
    /// listener, …) and the new one is prepared (`sync_listener_plan` + `prepare_host`), as
    /// a disable / enable would. Returns whether `prepare_host` ran.
    pub(crate) fn reselect_backends(&self) -> bool {
        let settings = self.lock().config.settings.clone();
        let b = backends::select(self.factory.as_ref(), &settings);
        let old = self.backends();
        let session_changed = old.session.kind() != b.session.kind();
        let enabled = settings.enabled && self.lock().safe_reason.is_none();
        if session_changed && enabled {
            match old.session.restore_host() {
                Ok(()) => log::info(&format!("session backend {} deselected: host restored", backends::kind_name(&old.session.kind()))),
                Err(e) => log::error_id(eventid::BACKEND_FAILED, &format!("restore_host on deselect: {e}")),
            }
        }
        let kept: std::collections::HashSet<_> = b.gates.iter().map(|g| g.kind()).collect();
        for g in old.gates.iter().filter(|g| !kept.contains(&g.kind())) {
            match g.revert_all() {
                Ok(()) => log::info(&format!("gate {} deselected: reverted", backends::kind_name(&g.kind()))),
                Err(e) => log::warn(&format!("gate revert {} on deselect: {e}", backends::kind_name(&g.kind()))),
            }
        }
        if old.gates.iter().any(|g| !kept.contains(&g.kind())) {
            // Session drive letters: the agents remove their per-session letters.
            let tasks = self.factory.take_gate_agent_tasks();
            if !tasks.is_empty() {
                let mut st = self.lock();
                for (session_id, task) in tasks {
                    self.queue_agent_task(&mut st, session_id, task);
                }
            }
        }
        let new_session = b.session.clone();
        *self.backends.lock().unwrap_or_else(|e| e.into_inner()) = b;
        self.install_sinks();
        self.lock().devices_dirty = true;
        let prepared = session_changed && enabled;
        if prepared {
            self.sync_listener_plan();
            match new_session.prepare_host(&settings) {
                Ok(notes) if !notes.is_empty() => log::info(&format!("host prepared: {notes:?}")),
                Ok(_) => {}
                Err(e) => log::error_id(eventid::BACKEND_FAILED, &format!("prepare_host: {e}")),
            }
        }
        // Registered only under `fast_user_switch`.
        self.sync_credprov();
        prepared
    }

    // ---- agent tasks --------------------------------------------------------------

    pub(crate) fn queue_agent_task(&self, st: &mut State, session_id: u32, task: AgentTaskKind) {
        let t = AgentTask { id: st.next_task_id, task };
        st.next_task_id += 1;
        st.agent_tasks.entry(session_id).or_default().push(t.clone());
        self.events.publish(Event::AgentTask { session_id, task: t });
    }

    // ---- tick ---------------------------------------------------------------------

    /// Periodic work: hot-plug debounce, ownership re-apply, seat watchdog, stable marker.
    pub fn tick(&self) {
        if self.is_stopping() {
            return;
        }
        let now = self.now();
        let (refresh, mark_stable) = {
            let mut st = self.lock();
            let refresh = st.devices_dirty || self.platform.take_devices_changed();
            let mark = !st.stable_marked && now.saturating_sub(st.started_ms) >= runstate::STABLE_AFTER_MS;
            if mark {
                st.stable_marked = true;
                st.run.unstable_starts = 0;
                self.save_run(&st);
            }
            (refresh, mark)
        };
        if mark_stable {
            log::debug("service start marked stable");
        }
        if refresh {
            self.refresh_inventory();
        }
        if self.lock().ownership_dirty {
            self.apply_ownership();
            self.reconfigure_hosts();
        }
        self.watchdog(now);
        self.sync_kiosk_console();
        self.sync_seat1_kiosk();
        self.remote_tick();
        self.platform.reap_exited();
        self.enforce_deny_logoffs(now);
        self.update_keep_awake();
        self.idle_saver(now);
        self.ensure_console_listener(now);
        self.update_tick();
    }

    // ---- power, images, tweaks -------------------------------------------------------

    /// Hold the keep-awake power request while any extra seat (not the console seat) is
    /// active, release it when none is (spec 40 §6.8).
    pub(crate) fn update_keep_awake(&self) {
        let (want, have) = {
            let st = self.lock();
            let want = !self.is_stopping()
                && st.config.settings.enabled
                && st.seats.values().any(|s| !s.seat.is_console() && Self::managed(&st, s.seat) && (s.is_active() || s.host.is_some()));
            (want, st.keep_awake)
        };
        if want == have {
            return;
        }
        match self.platform.set_keep_awake(want) {
            Ok(()) => {
                self.lock().keep_awake = want;
                log::info(if want { "keep-awake power request set (extra seat in use)" } else { "keep-awake power request cleared" });
            }
            Err(e) => {
                // Record the wanted state anyway so the failure is logged once, not per tick.
                self.lock().keep_awake = want;
                log::warn(&format!("keep-awake power request: {e}"));
                self.notice("keep_awake_failed", None, e.to_string());
            }
        }
    }

    /// Input seen on a device: its seat is not idle; wake its monitors if the idle saver
    /// switched them off.
    fn note_input(&self, key: &DeviceKey) {
        let now = self.now();
        let wake = {
            let mut st = self.lock();
            let mask = st.input_overrides.get(key).copied().or_else(|| {
                st.inventory.effective(&st.config).into_iter().find(|e| &e.key == key).map(|e| e.mask)
            });
            let Some(seat) = mask.and_then(|m| m.single()) else { return };
            st.last_input_ms.insert(seat, now);
            st.idle_blanked.remove(&seat).then(|| Self::seat_monitors(&st, seat))
        };
        if let Some(monitors) = wake {
            let d = self.backends().display;
            for m in monitors {
                if let Err(e) = d.set_power(&m, true) {
                    log::warn(&format!("idle saver: wake {m}: {e}"));
                }
            }
        }
    }

    /// Monitor idle saver (spec 40 §6.8, spec 10 §4.4): after
    /// `timings.monitor_idle_off_s` without input on an active extra seat, switch its
    /// monitors off (DDC/CI or presenter blank); [`Self::note_input`] wakes them. 0 = off.
    /// Needs activity events, so it works only where the input backend reports activity.
    fn idle_saver(&self, now: u64) {
        let (blank, wake) = {
            let mut st = self.lock();
            let timeout_ms = u64::from(st.config.settings.timings.monitor_idle_off_s) * 1000;
            let active: Vec<SeatNumber> =
                st.seats.values().filter(|s| !s.seat.is_console() && Self::managed(&st, s.seat) && s.is_active()).map(|s| s.seat).collect();
            if timeout_ms == 0 || !st.config.settings.enabled {
                let wake: Vec<DeviceKey> =
                    std::mem::take(&mut st.idle_blanked).into_iter().flat_map(|s| Self::seat_monitors(&st, s)).collect();
                (Vec::new(), wake)
            } else {
                let mut blank = Vec::new();
                for seat in active {
                    let last = *st.last_input_ms.entry(seat).or_insert(now);
                    if now.saturating_sub(last) >= timeout_ms && st.idle_blanked.insert(seat) {
                        log::info(&format!("seat {seat}: idle, monitors off"));
                        blank.extend(Self::seat_monitors(&st, seat));
                    }
                }
                (blank, Vec::new())
            }
        };
        if blank.is_empty() && wake.is_empty() {
            return;
        }
        let d = self.backends().display;
        for (m, on) in blank.iter().map(|m| (m, false)).chain(wake.iter().map(|m| (m, true))) {
            if let Err(e) = d.set_power(m, on) {
                log::warn(&format!("idle saver: {m} power {on}: {e}"));
            }
        }
    }

    /// Our generated seat and lock-screen images exist (spec 80 §11.4).
    pub(crate) fn ensure_images(&self) {
        let seats: Vec<SeatNumber> = self.lock().config.seats.iter().map(|s| s.number).collect();
        let mut failed = None;
        for seat in seats {
            if let Err(e) = crate::images::ensure_seat_image(&self.data_dir, seat) {
                failed = Some(format!("seat {seat}: {e}"));
            }
        }
        if let Err(e) = crate::images::ensure_lock_image(&self.data_dir) {
            failed = Some(format!("lock: {e}"));
        }
        if let Some(e) = failed {
            log::warn(&format!("seat images not written: {e}"));
        }
    }

    /// Config-driven machine tweaks in line with the config: while the product is enabled
    /// the `settings.tweaks` flags are applied / reverted, when disabled all are reverted.
    /// Journaled by the platform, so uninstall reverts them too.
    pub(crate) fn sync_tweaks(&self) {
        let (enabled, tweaks) = {
            let st = self.lock();
            (st.config.settings.enabled, st.config.settings.tweaks.clone())
        };
        let out = if enabled {
            let lock = tweaks
                .lock_screen_image
                .then(|| crate::images::ensure_lock_image(&self.data_dir))
                .and_then(|r| r.map_err(|e| log::warn(&format!("lock-screen image: {e}"))).ok());
            self.platform.sync_tweaks(&tweaks, lock.as_deref())
        } else {
            self.platform.revert_tweaks()
        };
        // The credential provider follows the same switch (and the active backend).
        self.sync_credprov();
        for o in out {
            match o.result {
                Ok(()) => log::debug(&format!("tweak {} in line with the config", o.tweak)),
                Err(e) => {
                    log::warn(&format!("tweak {}: {e}", o.tweak));
                    self.notice("tweak_failed", None, format!("{}: {e}", o.tweak));
                }
            }
        }
    }

    /// Ordered stop (service stop / pre-shutdown): seats one at a time, then mark clean.
    #[cfg(test)]
    pub fn shutdown(&self) {
        self.shutdown_with(&|_, _| {});
    }

    /// [`Self::shutdown`] with a progress callback `(seats done, seats total)`, called
    /// before the first and after each seat (the SCM handler advances its checkpoint).
    pub fn shutdown_with(&self, progress: &dyn Fn(usize, usize)) {
        if self.stopping.swap(true, Ordering::SeqCst) {
            return;
        }
        let seats: Vec<SeatNumber> = {
            let st = self.lock();
            st.seats.values().filter(|s| Self::managed(&st, s.seat) && s.is_active()).map(|s| s.seat).collect()
        };
        let total = seats.len();
        progress(0, total);
        for (i, seat) in seats.into_iter().enumerate() {
            self.stop_seat_now(seat, true);
            progress(i + 1, total);
        }
        // Seat 1's kiosk shell (is_stopping is set: no longer wanted), then the kiosk
        // console user's default terminal goes back.
        self.sync_seat1_kiosk();
        self.sync_kiosk_console();
        // Remote seats: stop, wait (bounded), final sweep.
        self.remote_shutdown();
        // Release the keep-awake power request (is_stopping is set).
        self.update_keep_awake();
        let mut st = self.lock();
        st.run.running = false;
        st.run.unstable_starts = 0;
        self.save_run(&st);
        log::info_id(eventid::SERVICE_STOPPED, "service stopped cleanly");
    }

    /// Resume from sleep / hibernate: re-read devices, re-apply ownership, hosts re-place
    /// their outputs (spec 20 §7.5). Seats keep their sessions.
    pub fn on_resume(&self) {
        log::info("resume: re-checking devices and displays");
        self.refresh_inventory();
        self.apply_ownership();
        self.reconfigure_hosts();
        // Remote seats stopped at suspend start again (Pulsar Split, spec 20 §7).
        self.remote_resume();
    }

    /// Tell every running host to re-read its assignment.
    pub(crate) fn reconfigure_hosts(&self) {
        let seats: Vec<SeatNumber> = self.lock().seats.values().filter(|s| s.host.is_some()).map(|s| s.seat).collect();
        for seat in seats {
            self.events.publish(Event::HostCommand { seat, command: seat_proto::HostCommand::Reconfigure });
        }
    }

    pub(crate) fn runtime_list(st: &State) -> Vec<SeatRuntime> {
        st.config
            .seats
            .iter()
            .filter_map(|c| match (c.kind, st.remote_rt.get(&c.number)) {
                (seat_core::config::SeatKind::Remote, Some(r)) => Some(r.clone()),
                _ => st.seats.get(&c.number).map(|s| seat_core::state::SeatRuntime { kind: c.kind, ..s.rt.clone() }),
            })
            .collect()
    }
}

/// Insert or update one explicit assignment in a config copy.
pub(crate) fn set_assignment(cfg: &mut Config, key: &DeviceKey, kind: DeviceKind, mask: seat_core::SeatMask, name: &str) {
    if let Some(a) = cfg.devices.iter_mut().find(|a| &a.key == key) {
        a.mask = mask;
        return;
    }
    cfg.devices.push(seat_core::config::DeviceAssignment {
        key: key.clone(),
        kind,
        mask,
        name: name.to_string(),
        label: None,
        hardware_id: None,
        serial: None,
        container_id: None,
        options: Default::default(),
    });
}

/// Configured remote seat numbers (for the admin-only event filter).
pub(crate) fn remote_seats(cfg: &Config) -> std::collections::BTreeSet<seat_core::SeatNumber> {
    cfg.seats.iter().filter(|s| s.is_remote()).map(|s| s.number).collect()
}

/// The input filter's seat -> session map (`InputControl::set_seat_sessions`, round 4
/// local-gaps). The HID filter lets a requestor open a seat's collections when its session
/// is the seat's session, so a seat that has no session of its own must not be mapped:
/// kiosk seats (`desktops`) all report the console session, and mapping them would let
/// every console program (seat 1's desktop included) open the kiosk seat's touch screens,
/// consumer keys and pads. Such seats get their input only through the service
/// (`input_packets` to the verified kiosk shell). Left out: every seat but seat 1 whose
/// session is the console session (unless `dedicated`, where the console is the host
/// account's and seat 1 is a session of its own) or is shared with another seat.
pub(crate) fn filter_session_map(all: &[(SeatNumber, u32)], console: Option<u32>, dedicated: bool) -> Vec<(SeatNumber, u32)> {
    all.iter()
        .filter(|(seat, sid)| {
            if seat.is_console() && !dedicated {
                return true;
            }
            let on_console = !dedicated && console == Some(*sid);
            let shared = all.iter().any(|(s, x)| s != seat && x == sid);
            !on_console && !shared
        })
        .copied()
        .collect()
}

/// Source-IP map entries (net-wfp): one per signed-in seat with a valid address whose
/// session (never 0) no other signed-in seat shares, plus the seats left out because
/// they share it.
pub(crate) fn source_ip_entries(logons: &[(SeatNumber, u32)], config: &Config) -> (Vec<seat_compat::wfp::SessionSeat>, Vec<SeatNumber>) {
    let programs = &config.settings.ip_programs;
    let mut out = Vec::new();
    let mut shared = Vec::new();
    for (seat, sid) in logons {
        let Some(address) = config.seat(*seat).and_then(|c| seat_compat::srcip::from_config(c, programs)) else { continue };
        if *sid == 0 || logons.iter().any(|(s, x)| s != seat && x == sid) {
            shared.push(*seat);
            continue;
        }
        out.push(seat_compat::wfp::SessionSeat { session_id: *sid, address });
    }
    (out, shared)
}

/// Kiosk seat map entries (round 4 kiosk-isolation, seat-shim-core `kiosk`): one per kiosk
/// shell seat that runs in the console session (the seats [`filter_session_map`] leaves
/// out), with the instance ids of the keyboards, mice and HID devices whose effective mask
/// names exactly that seat. Never under `dedicated` (every seat has its own session then).
/// The shim acts on it only in processes that loaded the module. HYPOTHESIS: never run on
/// hardware.
pub(crate) fn kiosk_map_entries(
    logons: &[(SeatNumber, u32)],
    kiosk: &std::collections::BTreeSet<SeatNumber>,
    console: Option<u32>,
    dedicated: bool,
    eff: &[seat_core::assign::Effective],
    instance_of: impl Fn(&DeviceKey) -> Option<String>,
) -> Vec<seat_shim_core::KioskSeatEntry> {
    if dedicated {
        return Vec::new();
    }
    logons
        .iter()
        .filter(|(seat, sid)| kiosk.contains(seat) && *sid != 0 && console == Some(*sid))
        .map(|(seat, sid)| {
            let n = u32::from(seat.get());
            let mut devices: Vec<String> = eff
                .iter()
                // Parity 2026-11-17: also the seat's USB hubs / ports and other USB devices,
                // so XInput pads (XUSB, not HID) and pads on a seat's hub are the seat's in
                // the module's chain walk too (the rule only looks at HID, XUSB and keyboard
                // / mouse interface paths, so a storage device listed here changes nothing).
                .filter(|e| matches!(e.kind, DeviceKind::Keyboard | DeviceKind::Mouse | DeviceKind::Hid | DeviceKind::UsbHub | DeviceKind::Other))
                .filter(|e| e.mask.single() == Some(*seat))
                .filter_map(|e| instance_of(&e.key))
                .collect();
            devices.sort();
            devices.dedup();
            seat_shim_core::KioskSeatEntry {
                seat: n,
                session_id: *sid,
                generation: 1,
                devices,
                flags: seat_shim_core::KioskFlags::default(),
            }
        })
        .collect()
}

#[cfg(test)]
mod kiosk_map_tests {
    use super::*;
    use seat_core::assign::{Effective, MaskSource};
    use seat_core::SeatMask;

    fn s(n: u8) -> SeatNumber {
        SeatNumber::new(n).unwrap()
    }

    fn eff(key: &str, kind: DeviceKind, mask: SeatMask) -> Effective {
        Effective { key: DeviceKey::new(key).unwrap(), kind, mask, source: MaskSource::Explicit }
    }

    #[test]
    fn kiosk_seats_get_their_own_input_devices() {
        let devs = vec![
            eff("kb2", DeviceKind::Keyboard, SeatMask::from_seats([s(2)])),
            eff("ms2", DeviceKind::Mouse, SeatMask::from_seats([s(2)])),
            eff("pad3", DeviceKind::Hid, SeatMask::from_seats([s(3)])),
            eff("mon2", DeviceKind::Monitor, SeatMask::from_seats([s(2)])),
            eff("shared", DeviceKind::Keyboard, SeatMask::SHARED),
            eff("noid", DeviceKind::Mouse, SeatMask::from_seats([s(2)])),
            eff("xpad3", DeviceKind::Other, SeatMask::from_seats([s(3)])),
            eff("hub3", DeviceKind::UsbHub, SeatMask::from_seats([s(3)])),
            eff("cam3", DeviceKind::Webcam, SeatMask::from_seats([s(3)])),
        ];
        let inst = |k: &DeviceKey| (k.as_str() != "noid").then(|| format!(r"HID\{}\1", k.as_str()));
        let kiosk = [s(2), s(3)].into_iter().collect();
        // Seat 1 desktop, kiosk seats 2 and 3 in console session 1, RDP seat 4.
        let logons = [(s(1), 1), (s(2), 1), (s(3), 1), (s(4), 5)];
        let e = kiosk_map_entries(&logons, &kiosk, Some(1), false, &devs, inst);
        assert_eq!(e.len(), 2);
        assert_eq!((e[0].seat, e[0].session_id), (2, 1));
        assert_eq!(e[0].devices, vec![r"HID\kb2\1".to_string(), r"HID\ms2\1".into()]);
        // XInput pads (USB "other") and the seat's hub too; never a webcam.
        assert_eq!(e[1].devices, vec![r"HID\hub3\1".to_string(), r"HID\pad3\1".into(), r"HID\xpad3\1".into()]);
        // Not in the console session (switching) or dedicated host: nothing.
        assert!(kiosk_map_entries(&logons, &kiosk, None, false, &devs, inst).is_empty());
        assert!(kiosk_map_entries(&logons, &kiosk, Some(1), true, &devs, inst).is_empty());
    }
}

#[cfg(test)]
mod session_map_tests {
    use super::*;

    fn s(n: u8) -> SeatNumber {
        SeatNumber::new(n).unwrap()
    }

    #[test]
    fn kiosk_seats_on_the_console_session_are_not_mapped() {
        // Seat 1 = console session 1; seats 2 and 3 = kiosk shells in session 1; seat 4 = RDP.
        let all = [(s(1), 1), (s(2), 1), (s(3), 1), (s(4), 5)];
        assert_eq!(filter_session_map(&all, Some(1), false), vec![(s(1), 1), (s(4), 5)]);
        // Seat 1 at the logon screen (no logon entry): a lone kiosk seat still is not mapped.
        assert_eq!(filter_session_map(&[(s(2), 1)], Some(1), false), vec![]);
        // Two seats sharing a non-console session are left out too.
        assert_eq!(filter_session_map(&[(s(2), 7), (s(3), 7), (s(4), 8)], Some(1), false), vec![(s(4), 8)]);
    }

    #[test]
    fn dedicated_host_keeps_seat_sessions() {
        let all = [(s(1), 3), (s(2), 4)];
        assert_eq!(filter_session_map(&all, Some(1), true), all.to_vec());
        // Under dedicated_host seat 1 is an ordinary seat: shared sessions are still dropped.
        assert_eq!(filter_session_map(&[(s(1), 3), (s(2), 3)], Some(1), true), vec![]);
    }
}

impl Service {
    /// `(seat, host account SID)` of the seats whose host prints to the seat's own queues
    /// (`redirect.printers = seat`) as the seat's own account, for the `printer_acl` gate.
    /// The account is set up here when it is not yet (no state lock held).
    pub(crate) fn printer_hosts(&self) -> Vec<(SeatNumber, String)> {
        let seats: Vec<SeatNumber> = {
            let st = self.lock();
            st.config
                .seats
                .iter()
                .filter(|s| s.is_local() && !s.number.is_console() && s.local_settings().redirect.printers == seat_core::config::PrinterRedirect::Seat)
                .map(|s| s.number)
                .collect()
        };
        seats.into_iter().filter_map(|s| self.platform.host_account_sid(s).map(|sid| (s, sid))).collect()
    }

    /// Seats whose host may run as a seat account (round 4 local-gaps): the configured local
    /// seats, seat 1 only under `dedicated_host`. The service start removes the accounts of
    /// every other seat number (`host_account::prune`).
    pub(crate) fn host_account_seats(&self) -> Vec<SeatNumber> {
        host_account_seats(&self.lock().config)
    }

    /// Seats whose Pulsar kiosk account is kept (2026-11-17): local seats 2+ with a kiosk
    /// shell. The service start removes the others
    /// (`host_account::prune_kiosk`, profile included).
    pub(crate) fn kiosk_account_seats(&self) -> Vec<SeatNumber> {
        kiosk_account_seats(&self.lock().config)
    }
}

/// Pure part of [`Service::kiosk_account_seats`].
/// The mode is not looked at: switching to `shared` for a while must not delete the seats'
/// profiles.
pub(crate) fn kiosk_account_seats(cfg: &Config) -> Vec<SeatNumber> {
    cfg.seats
        .iter()
        .filter(|s| s.is_local() && !s.number.is_console() && s.local_settings().kiosk_shell.is_some())
        .map(|s| s.number)
        .collect()
}

/// Pure part of [`Service::host_account_seats`].
pub(crate) fn host_account_seats(cfg: &Config) -> Vec<SeatNumber> {
    let dedicated = cfg.settings.backends.host_topology == seat_core::backend::session::HostTopology::DedicatedHost;
    cfg.seats.iter().filter(|s| s.is_local() && (!s.number.is_console() || dedicated)).map(|s| s.number).collect()
}

#[cfg(test)]
mod host_account_seat_tests {
    use super::*;

    #[test]
    fn local_seats_with_a_host_keep_their_account() {
        let s = |n| SeatNumber::new(n).unwrap();
        let mut c = Config { seats: vec![seat_core::config::SeatConfig::new(s(1)), seat_core::config::SeatConfig::new(s(2)), seat_core::config::SeatConfig::new(s(3))], ..Config::default() };
        assert_eq!(host_account_seats(&c), vec![s(2), s(3)]);
        c.settings.backends.host_topology = seat_core::backend::session::HostTopology::DedicatedHost;
        assert_eq!(host_account_seats(&c), vec![s(1), s(2), s(3)]);
    }

    #[test]
    fn kiosk_seats_keep_their_kiosk_account() {
        let s = |n| SeatNumber::new(n).unwrap();
        let mut c = Config { seats: vec![seat_core::config::SeatConfig::new(s(1)), seat_core::config::SeatConfig::new(s(2)), seat_core::config::SeatConfig::new(s(3))], ..Config::default() };
        assert!(kiosk_account_seats(&c).is_empty());
        for n in [0, 2] {
            c.seats[n].local_mut().unwrap().kiosk_shell = Some(seat_core::config::KioskShell::default());
        }
        assert_eq!(kiosk_account_seats(&c), vec![s(3)], "seat 1 never has one");
        c.settings.kiosk.accounts = seat_core::config::KioskAccountMode::Shared;
        assert_eq!(kiosk_account_seats(&c), vec![s(3)], "shared for a while keeps the profiles");
    }
}
