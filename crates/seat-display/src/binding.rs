// SPDX-License-Identifier: GPL-3.0-only
//! [`DisplayBinding`] for every [`DisplayBackendKind`] (service side, spec 40 §6).
//!
//! One implementation, parameterised by the kind:
//!
//! | kind | seat monitors | presentation |
//! | --- | --- | --- |
//! | `window_presenter` | stay in the console desktop | `fallback` (top-most window, degraded) |
//! | `display_core` | special-purpose targets (removed from the desktop by the user) | `presenter`; desktop targets fall back to `fallback` |
//! | `pulsar_idd` | as `display_core`, plus one virtual monitor per output from our IddCx driver | as `display_core` |
//! | `console_only` | none (fast user switching) | seat 1 `native` only |
//!
//! The binding adds a monitor to the console desktop only when the plan asks for it (a seat
//! that presents it through the desktop, or `TopologyPlan::attach`: seat 1's explicitly
//! assigned monitors and every monitor in the wizard's extend step) and it is connected but
//! outside the desktop: one CCD path with explicit paths, journaled first as `attached`,
//! removed again once neither a seat nor `attach` names it (gaming PC E2E bug 9,
//! 2026-11-24: Windows' extend topology kept a user-detached monitor out; round 2 bug H:
//! an unassigned monitor merely listed in `console` is never attached, and a monitor the
//! product attached goes back out when its seat lets it go or the product is turned off).
//! It never touches graphics-kernel state.
//!
//! - `apply_topology` takes the seat targets of `display_core` / `pulsar_idd` out of the
//!   console desktop where Windows offers monitor specialization (feature-detected; see
//!   [`crate::topology`]), journals every target it takes out **before** the change
//!   ([`Binding::set_journal_path`]), and gives journaled targets back once the plan no
//!   longer reserves them. The CCD part runs in the console session (validated, verified,
//!   rolled back on failure, `crate::win::ccd::apply_request`). Without specialization the
//!   target stays in the desktop and is reported in `fallback`.
//! - Per-seat options the trait cannot carry ([`SeatDisplayOptions`]: frame-reader SID,
//!   software cursor per monitor) are set with [`Binding::set_seat_options`] before
//!   `bind_seat`.
//! - `identify` / `set_power` on presenter targets (no HMONITOR) are routed to the seat
//!   host through [`Binding::set_presenter_router`]: the presenter draws the label / blanks
//!   the scan-out itself (spec 40 §6.8, §6.9). `set_power(off)` there returns
//!   `Unsupported` / `blanked_only` after blanking: DDC/CI needs an HMONITOR.
//! - Clone (one seat surface on several targets, spec 40 §6.3.6): a surface monitor with
//!   `outputs` ([`SeatDisplayOptions::outputs`], from `AssignmentOptions::outputs`) gets
//!   one extra placement per further target on the same adapter, marked `clone_of`, after
//!   all non-clone placements (RDP monitor order is unchanged). The presenter shows the
//!   surface's frames on each of them.
//! - Split screen (OSMS-parity G8, 2026-11-18): a monitor may be bound by up to four seats
//!   at once, each with a [`ScreenArea`] ([`SeatDisplayOptions::areas`]) whose cells do not
//!   overlap the cells already bound there. The binding keeps one entry per seat, so the
//!   monitor stays bound until its last seat is released (reference count by seat). A
//!   split output must be a console-desktop target (`fallback`: one window per seat on the
//!   shared monitor, no independent flip, about one frame more latency); its `rect` is the
//!   area's part of the monitor, its mode never follows the session, and its `pulsar_idd`
//!   virtual monitor gets the area size as its only mode.

use crate::console::{Console, ConsoleCommand, IdentifyItem};
use crate::model::{build_monitors, diff_devices, resolve_key, virtual_rects, LiveMonitor, Resolution};
use crate::topology::{journal_after, journal_before, plan_topology, Candidate, TargetRef, TopologyJournal};
use seat_core::backend::display::{
    DisplayBackendKind, DisplayBinding, DisplayCaps, OutputPlacement, SeatDisplayPrefs, SeatOutputs, TopologyPlan, TopologyReport,
};
use seat_core::backend::{Availability, BackendError, BackendResult, DriverId, ErrorKind, HotplugEvent, HotplugSink};
use seat_core::area::ScreenArea;
use seat_core::device::{Device, DeviceDetails, DisplayMode, EdidId, Presentation, Rect};
use seat_core::ids::{DeviceKey, SeatNumber};
use seat_core::pnp::PnpTree;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

/// Reads the EDID blob of a monitor devnode (`Device Parameters\EDID`).
pub type EdidSource = Box<dyn Fn(&str) -> Option<Vec<u8>> + Send + Sync>;

/// One virtual monitor to plug ([`IddControl::add_monitor`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IddMonitor<'a> {
    pub seat: SeatNumber,
    /// Output index of the seat.
    pub connector: u32,
    /// Physical monitor's EDID (raw registry blob; the client trims it to whole blocks).
    pub edid: &'a [u8],
    pub modes: &'a [DisplayMode],
    /// GPU of the physical monitor (DWM of the virtual monitor renders there).
    pub render_luid: u64,
    /// String SID granted read access to the frame section (the seat host's token user).
    /// `None` = SYSTEM + Administrators only, and the seat host cannot read frames.
    pub reader_sid: Option<&'a str>,
    /// Per-seat software cursor option: no hardware cursor, DWM draws the pointer.
    pub software_cursor: bool,
    /// Offer HDR / SDR wide colour on the virtual monitor (`AssignmentOptions::hdr`,
    /// driver minor 3, IddCx 1.10). The EDID's HDR blocks tell Windows what the panel can
    /// do; without IddCx 1.10 the monitor stays SDR.
    pub hdr: bool,
}

/// Client of our IddCx driver (`drivers/seat-idd`), see `crate::win::idd`.
pub trait IddControl: Send + Sync {
    fn availability(&self) -> Availability;
    /// Plug one virtual monitor with the physical monitor's EDID and modes, on the
    /// adapter of its render GPU (one adapter per GPU, [`crate::idd_adapters`]).
    fn add_monitor(&self, m: &IddMonitor<'_>) -> BackendResult<()>;
    fn remove_monitor(&self, seat: SeatNumber, connector: u32) -> BackendResult<()>;
    /// The adapters (one per render GPU) as last seen. Default: unknown (empty).
    fn adapters(&self) -> Vec<crate::idd_adapters::AdapterState> {
        Vec::new()
    }
}

/// Per-seat display options the `DisplayBinding` trait cannot carry. Set with
/// [`Binding::set_seat_options`] before `bind_seat`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SeatDisplayOptions {
    /// String SID (`S-1-5-…`) of the seat host's token user, for the `pulsar_idd` frame
    /// sections' ACL. Per seat, so binding another seat never changes this seat's ACL.
    pub frame_reader_sid: Option<String>,
    /// Monitors whose assignment selects the software cursor
    /// (`AssignmentOptions::software_cursor`, spec 40 §3.3 / §6.3.5).
    pub software_cursor: HashSet<DeviceKey>,
    /// Clone: surface monitor key to the CCD target ids on its adapter that show it
    /// (`AssignmentOptions::outputs`). Empty / one id = no clone.
    pub outputs: Vec<(DeviceKey, Vec<u32>)>,
    /// Follow the seat session's resolution (spec 40 §6.5); `None` = presenter targets only.
    pub follow_session_mode: Option<bool>,
    /// Monitors whose assignment asks for HDR on the seat's virtual monitor and the
    /// presenter (`AssignmentOptions::hdr`). Added 2026-11-17.
    pub hdr: HashSet<DeviceKey>,
    /// Monitors whose assignment asks for variable refresh through the presenter
    /// (`AssignmentOptions::vrr`). Added 2026-11-17.
    pub vrr: HashSet<DeviceKey>,
    /// Split monitors: this seat's part (`AssignmentOptions::areas`). Added 2026-11-18 (G8).
    pub areas: HashMap<DeviceKey, ScreenArea>,
    /// Split monitors shared with the console (seat 1). Added 2026-11-18 (G8).
    pub console_share: HashSet<DeviceKey>,
}

/// Command for the seat host that presents a target the console cannot reach (presenter
/// targets: no HMONITOR, not in the console desktop).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PresenterCommand {
    /// Draw `label` over output `output` for `seconds` (0 = hide now), spec 40 §6.9.
    Identify { output: usize, label: String, seconds: u32 },
    /// Blank (`on: false`) or restore (`on: true`) the scan-out of output `output`,
    /// spec 40 §6.8. No DDC/CI without an HMONITOR.
    Power { output: usize, on: bool },
}

/// Delivers a [`PresenterCommand`] to the seat host of `seat` (service: `host_command`).
pub type PresenterRouter = Arc<dyn Fn(SeatNumber, PresenterCommand) -> BackendResult<()> + Send + Sync>;

/// `seconds` of an identify that stays up until it is hidden (`identify(.., false)`):
/// spec 10 §3.5.1, indication stays on until the user stops it; the wizard's number
/// overlays last for the whole wizard. (`u32::MAX` s is ~136 years, so the overlay and
/// presenter deadlines need no special case.)
pub const IDENTIFY_UNTIL_HIDDEN: u32 = u32::MAX;

struct State {
    bound: BTreeMap<SeatNumber, Vec<OutputPlacement>>,
    options: HashMap<SeatNumber, SeatDisplayOptions>,
    /// Last EDID seen per key (fallback resolution after a renumbered target).
    edid_hints: HashMap<DeviceKey, EdidId>,
    last: Vec<Device>,
    /// Targets Pulsar took out of the console desktop ([`crate::topology`]).
    journal: TopologyJournal,
    journal_path: Option<std::path::PathBuf>,
    /// Check owner / DACL / links of the journal folder and file (service, Windows).
    journal_check: bool,
    /// The journal folder could not be made trusted: nothing is written and no target is
    /// taken out of the desktop (there would be no trusted undo record).
    journal_refused: Option<String>,
}

pub(crate) struct Inner {
    kind: DisplayBackendKind,
    console: Arc<dyn Console>,
    pnp: Arc<dyn PnpTree>,
    edid: EdidSource,
    idd: Option<Arc<dyn IddControl>>,
    state: Mutex<State>,
    /// Serialises `bind_seat` / `release_seat` (busy check, IddCx add / remove, insert).
    bind: Mutex<()>,
    /// The session backend renders a display of its own (RDP loopback, backend A): no
    /// IddCx monitors then (spec 00 T1). Default true until the service says otherwise.
    session_has_display: AtomicBool,
    router: Mutex<Option<PresenterRouter>>,
    sink: Mutex<Option<HotplugSink>>,
    listener: Mutex<Option<Box<dyn Send + Sync>>>,
    /// `apply_topology` may specialize seat targets (backends that present outside the
    /// desktop). Default true.
    auto_specialize: AtomicBool,
}

/// The service-side display binding. Cheap to clone (shared inner state).
#[derive(Clone)]
pub struct Binding {
    inner: Arc<Inner>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn err(kind: ErrorKind, reason: &str, msg: impl Into<String>) -> BackendError {
    BackendError::new(kind, reason, msg)
}

impl Binding {
    pub fn new(kind: DisplayBackendKind, console: Arc<dyn Console>, pnp: Arc<dyn PnpTree>, edid: EdidSource) -> Self {
        Self {
            inner: Arc::new(Inner {
                kind,
                console,
                pnp,
                edid,
                idd: None,
                state: Mutex::new(State {
                    bound: BTreeMap::new(),
                    options: HashMap::new(),
                    edid_hints: HashMap::new(),
                    last: Vec::new(),
                    journal: TopologyJournal::default(),
                    journal_path: None,
                    journal_check: false,
                    journal_refused: None,
                }),
                bind: Mutex::new(()),
                session_has_display: AtomicBool::new(true),
                router: Mutex::new(None),
                sink: Mutex::new(None),
                listener: Mutex::new(None),
                auto_specialize: AtomicBool::new(true),
            }),
        }
    }

    /// Attach the IddCx driver client (`pulsar_idd` kind).
    pub fn with_idd(self, idd: Arc<dyn IddControl>) -> Self {
        let mut inner = Arc::try_unwrap(self.inner).unwrap_or_else(|_| panic!("with_idd must be called before sharing"));
        inner.idd = Some(idd);
        Self { inner: Arc::new(inner) }
    }

    /// Per-seat options for the next `bind_seat` of `seat` (kept until changed).
    pub fn set_seat_options(&self, seat: SeatNumber, options: SeatDisplayOptions) {
        lock(&self.inner.state).options.insert(seat, options);
    }

    /// Persist the topology journal at `path` (service: `%ProgramData%\Pulsar Seat\journal\
    /// display-topology.json`). Loads what a previous run left, so targets taken out before
    /// a crash are given back by the next `apply_topology` that no longer reserves them.
    ///
    /// On Windows the folder is made private (`PRIVATE_DIR_SDDL`, owner Administrators) and
    /// the file is trusted only when owned and writable by SYSTEM / Administrators, not a
    /// link and with one hard link. An untrusted file is moved aside
    /// (`display-topology.json.untrusted-<ms>`); an untrusted folder refuses the journal
    /// (no target is taken out without a trusted undo record).
    pub fn set_journal_path(&self, path: std::path::PathBuf) {
        self.set_journal_path_with(path, cfg!(windows));
    }

    /// Same without any trust check (tests, tools running as the file's owner; never the
    /// service).
    pub fn set_journal_path_unchecked(&self, path: std::path::PathBuf) {
        self.set_journal_path_with(path, false);
    }

    fn set_journal_path_with(&self, path: std::path::PathBuf, check: bool) {
        let (j, refused) = if check { load_trusted_journal(&path) } else { (TopologyJournal::load(&path), None) };
        if let Some(why) = &refused {
            log::error!("display: topology journal {} refused: {why}; seat targets stay in the console desktop", path.display());
        }
        if !j.entries.is_empty() {
            // Gaming PC E2E round 2 (bug H): an `attached` entry was counted as taken out.
            let attached = j.entries.iter().filter(|e| e.attached).count();
            log::info!(
                "display: topology journal has {} target(s) taken out of the desktop and {attached} attached to it",
                j.entries.len() - attached
            );
        }
        let mut st = lock(&self.inner.state);
        st.journal = j;
        st.journal_path = Some(path);
        st.journal_check = check;
        st.journal_refused = refused;
    }

    /// Current journal (diagnostics).
    pub fn topology_journal(&self) -> TopologyJournal {
        lock(&self.inner.state).journal.clone()
    }

    /// Whether `apply_topology` may specialize seat targets (default true; only used by
    /// `display_core` / `pulsar_idd`). Off = seat targets stay in the desktop (fallback).
    pub fn set_auto_specialize(&self, on: bool) {
        self.inner.auto_specialize.store(on, Ordering::SeqCst);
    }

    /// Whether the session backend has a display of its own (RDP loopback: yes). IddCx
    /// virtual monitors are only plugged for backends without one (spec 00 T1); until the
    /// service clears this, `pulsar_idd` binds like `display_core`.
    pub fn set_session_has_display_flag(&self, has: bool) {
        self.inner.session_has_display.store(has, Ordering::SeqCst);
    }

    /// Route identify / power commands for presenter targets to the seat hosts.
    pub fn set_presenter_router(&self, router: PresenterRouter) {
        *lock(&self.inner.router) = Some(router);
    }

    /// Live monitors with runtime data (diagnostics, wizard).
    pub fn live_monitors(&self) -> BackendResult<Vec<LiveMonitor>> {
        self.inner.live()
    }

    /// Current placement of a bound seat.
    pub fn seat_outputs(&self, seat: SeatNumber) -> Option<SeatOutputs> {
        lock(&self.inner.state).bound.get(&seat).map(|o| SeatOutputs { seat, outputs: o.clone() })
    }

    /// Wizard step (spec 40 §3.13): extend the console desktop to every connected monitor.
    /// Only after the user agreed in the wizard.
    pub fn extend_desktop(&self) -> BackendResult<()> {
        self.inner.console.exec(&ConsoleCommand::ExtendDesktop)
    }

    /// Re-enumerate and push hot-plug events (called by the device-change listener; public
    /// so the service can also trigger it after an adapter reset).
    pub fn refresh(&self) {
        self.inner.refresh_and_notify();
    }

    pub(crate) fn weak(&self) -> std::sync::Weak<Inner> {
        Arc::downgrade(&self.inner)
    }

    pub(crate) fn from_inner(inner: Arc<Inner>) -> Self {
        Self { inner }
    }
}

impl Inner {
    fn needs_display_core(&self) -> bool {
        matches!(self.kind, DisplayBackendKind::DisplayCore | DisplayBackendKind::PulsarIdd)
    }

    fn live(&self) -> BackendResult<Vec<LiveMonitor>> {
        let snap = self.console.snapshot(self.needs_display_core())?;
        let read = |i: &str| (self.edid)(i);
        let mut mons = build_monitors(&snap, self.pnp.as_ref(), &read);
        let mut st = lock(&self.state);
        for m in &mons {
            if let Some(e) = &m.info().edid {
                st.edid_hints.insert(m.device.key.clone(), e.clone());
            }
        }
        // Presentation of targets bound to a seat.
        let bound: HashMap<DeviceKey, Presentation> =
            st.bound.values().flatten().map(|o| (o.key.clone(), o.presentation)).collect();
        for m in &mut mons {
            if let (Some(p), DeviceDetails::Display(d)) = (bound.get(&m.device.key), &mut m.device.details) {
                d.presentation = *p;
            }
        }
        Ok(mons)
    }

    pub(crate) fn refresh_and_notify(&self) {
        let _ = self.pnp.refresh();
        let Ok(mons) = self.live() else { return };
        let new: Vec<Device> = mons.into_iter().map(|m| m.device).collect();
        let old = std::mem::replace(&mut lock(&self.state).last, new.clone());
        let Some(sink) = lock(&self.sink).clone() else { return };
        let (arrived, removed, changed) = diff_devices(&old, &new);
        for k in removed {
            sink(HotplugEvent::Removed { key: k });
        }
        for d in arrived {
            sink(HotplugEvent::Arrived { device: Box::new(d) });
        }
        for d in changed {
            sink(HotplugEvent::Changed { device: Box::new(d) });
        }
    }

    /// How a live target can show a seat under this kind.
    fn presentation_for(&self, m: &LiveMonitor) -> Option<Presentation> {
        let t = &m.target;
        match self.kind {
            DisplayBackendKind::WindowPresenter => t.active.then_some(Presentation::Fallback),
            DisplayBackendKind::DisplayCore | DisplayBackendKind::PulsarIdd => {
                if t.specialized {
                    Some(Presentation::Presenter)
                } else if t.active {
                    Some(Presentation::Fallback)
                } else {
                    None
                }
            }
            DisplayBackendKind::ConsoleOnly => t.active.then_some(Presentation::Native),
        }
    }

    /// `(seat, output index)` of a key bound as a presenter output.
    fn presenter_output(&self, key: &DeviceKey) -> Option<(SeatNumber, usize)> {
        let st = lock(&self.state);
        st.bound.iter().find_map(|(s, outs)| {
            outs.iter().position(|o| &o.key == key && o.presentation == Presentation::Presenter).map(|i| (*s, i))
        })
    }

    fn save_journal(&self, j: &TopologyJournal) {
        let mut st = lock(&self.state);
        st.journal = j.clone();
        if st.journal_refused.is_some() {
            return;
        }
        if let Some(p) = &st.journal_path {
            if st.journal_check {
                if let Err(why) = check_journal_dir(p) {
                    log::error!("display: topology journal folder of {} not trusted ({why}); not written", p.display());
                    return;
                }
            }
            if let Err(e) = j.save(p) {
                log::error!("display: writing the topology journal {} failed: {e}", p.display());
            }
        }
    }

    fn resolve_all<'a>(&self, keys: &[DeviceKey], live: &'a [LiveMonitor]) -> Vec<(DeviceKey, Option<&'a LiveMonitor>)> {
        let hints = lock(&self.state).edid_hints.clone();
        let exact: Vec<DeviceKey> = keys.iter().filter(|k| live.iter().any(|m| &m.device.key == *k)).cloned().collect();
        let mut taken = exact.clone();
        keys.iter()
            .map(|k| {
                let r = resolve_key(k, live, hints.get(k), &taken);
                if let Some((m, how)) = r {
                    if how != Resolution::Exact {
                        log::info!("display: key {k} resolved to {} by EDID ({how:?})", m.device.key);
                        taken.push(m.device.key.clone());
                    }
                }
                (k.clone(), r.map(|(m, _)| m))
            })
            .collect()
    }
}

/// Load the journal with the trust checks (see [`Binding::set_journal_path`]): `(journal,
/// refused reason)`.
#[cfg(windows)]
fn load_trusted_journal(path: &std::path::Path) -> (TopologyJournal, Option<String>) {
    use std::io::Read;
    if let Err(why) = check_journal_dir(path) {
        return (TopologyJournal::default(), Some(why));
    }
    match crate::win::winsec::open_trusted(path) {
        Ok(None) => (TopologyJournal::default(), None),
        Ok(Some(mut f)) => {
            let mut b = Vec::new();
            match f.read_to_end(&mut b) {
                Ok(_) => (TopologyJournal::parse(&b, path), None),
                Err(e) => {
                    log::warn!("display: topology journal {} unreadable ({e}); starting empty", path.display());
                    (TopologyJournal::default(), None)
                }
            }
        }
        Err(why) => {
            let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
            let q = path.with_file_name(format!("{}.untrusted-{ms}", crate::topology::JOURNAL_FILE));
            log::error!("display: topology journal {} not trusted ({why}); moved to {} and ignored", path.display(), q.display());
            if let Err(e) = std::fs::rename(path, &q) {
                // Cannot move it: refuse, so the planted file is never acted on or overwritten.
                return (TopologyJournal::default(), Some(format!("untrusted file could not be moved aside: {e}")));
            }
            (TopologyJournal::default(), None)
        }
    }
}

#[cfg(not(windows))]
fn load_trusted_journal(path: &std::path::Path) -> (TopologyJournal, Option<String>) {
    (TopologyJournal::load(path), None)
}

/// Journal folder trusted (made private when needed).
#[cfg(windows)]
fn check_journal_dir(path: &std::path::Path) -> Result<(), String> {
    let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) else { return Ok(()) };
    crate::win::winsec::prepare_private_dir(dir, seat_core::consts::PRIVATE_DIR_SDDL)
}

#[cfg(not(windows))]
fn check_journal_dir(_: &std::path::Path) -> Result<(), String> {
    Ok(())
}

impl DisplayBinding for Binding {
    fn kind(&self) -> DisplayBackendKind {
        self.inner.kind
    }

    fn caps(&self) -> DisplayCaps {
        caps_for(self.inner.kind)
    }

    fn availability(&self) -> Availability {
        match self.inner.kind {
            DisplayBackendKind::WindowPresenter | DisplayBackendKind::ConsoleOnly => Availability::Available,
            DisplayBackendKind::DisplayCore => display_core_availability(self.inner.console.as_ref()),
            DisplayBackendKind::PulsarIdd => match &self.inner.idd {
                None => Availability::DriverNotSigned { driver: DriverId::Idd },
                Some(i) => i.availability(),
            },
        }
    }

    fn enumerate(&self) -> BackendResult<Vec<Device>> {
        let devs: Vec<Device> = self.inner.live()?.into_iter().map(|m| m.device).collect();
        lock(&self.inner.state).last = devs.clone();
        Ok(devs)
    }

    /// Takes reserved seat targets out of the console desktop where the backend presents
    /// them outside it, gives journaled targets back, then reports what is missing / in
    /// fallback (see the module doc).
    fn apply_topology(&self, plan: &TopologyPlan) -> BackendResult<TopologyReport> {
        let inner = &self.inner;
        let _bind = lock(&inner.bind);
        let mut live = inner.live()?;
        let mut report = TopologyReport::default();
        let takes_out = inner.needs_display_core() && inner.auto_specialize.load(Ordering::SeqCst) && lock(&inner.state).journal_refused.is_none();
        {
            let console: HashSet<DeviceKey> =
                inner.resolve_all(&plan.console, &live).into_iter().filter_map(|(_, m)| m.map(|m| m.device.key.clone())).collect();
            let seat_keys: Vec<DeviceKey> =
                if inner.kind == DisplayBackendKind::ConsoleOnly { Vec::new() } else { plan.seats.iter().flat_map(|(_, k)| k.iter().cloned()).collect() };
            let cand = |m: &LiveMonitor| Candidate {
                key: m.device.key.clone(),
                target: TargetRef { adapter_luid: m.target.adapter_luid, target_id: m.target.target_id },
                monitor_path: m.target.monitor_path.clone(),
                active: m.target.active,
                available: m.target.available,
                specialized: m.target.specialized,
                specialization_available: m.target.specialization_available,
                rect: m.target.desktop_rect,
            };
            let reserved: Vec<Candidate> = inner
                .resolve_all(&seat_keys, &live)
                .into_iter()
                .filter_map(|(_, m)| m)
                .filter(|m| !console.contains(&m.device.key))
                .map(cand)
                .collect();
            let all: Vec<Candidate> = live.iter().map(cand).collect();
            let journal = lock(&inner.state).journal.clone();
            // Console targets the plan asks for (`attach`: seat 1's explicit monitors, the
            // wizard's extend step) outside the desktop are attached too; never under
            // console_only (its seat agent owns the topology). Other console targets stay
            // where the user put them, and a target Pulsar attached that neither `attach`
            // nor a seat names any more is detached again (gaming PC E2E round 2 bug H).
            let attach_keys: HashSet<DeviceKey> =
                inner.resolve_all(&plan.attach, &live).into_iter().filter_map(|(_, m)| m.map(|m| m.device.key.clone())).collect();
            let console_cands: Vec<Candidate> = if inner.kind == DisplayBackendKind::ConsoleOnly {
                Vec::new()
            } else {
                live.iter().filter(|m| attach_keys.contains(&m.device.key)).map(cand).collect()
            };
            let attach = inner.kind != DisplayBackendKind::ConsoleOnly && lock(&inner.state).journal_refused.is_none();
            let actions = plan_topology(takes_out, &reserved, &console_cands, attach, &all, &journal);
            if actions.kept_for_console {
                log::warn!("display: seat targets stay in the console desktop: seat 1 would have no monitor left");
            }
            if actions.detach_kept_for_console {
                log::warn!("display: monitors Pulsar attached stay in the console desktop for now: seat 1 would have no monitor left");
            }
            if !actions.request.is_empty() {
                // Journal first: a crash during the change still leaves a record to undo.
                inner.save_journal(&journal_before(&journal, &actions));
                match inner.console.apply_topology(&actions.request) {
                    Ok(outcome) => {
                        let target_of = |k: &DeviceKey| all.iter().find(|c| &c.key == k).map(|c| c.target);
                        inner.save_journal(&journal_after(&journal, &actions, &outcome, target_of));
                        for (t, why) in &outcome.failed {
                            log::warn!("display: topology change for target {} on adapter {:x}: {why}", t.target_id, t.adapter_luid);
                        }
                        for t in &actions.attaching {
                            let key = all.iter().find(|c| &c.target == t).map(|c| c.key.to_string()).unwrap_or_default();
                            if outcome.activated.contains(t) {
                                let why = if reserved.iter().any(|c| &c.target == t) { "a seat presents it through the desktop" } else { "seat 1 or the wizard needs it" };
                                log::info!(
                                    "display: attached {key} (target {} of adapter {:x}) to the console desktop ({why}); journaled",
                                    t.target_id,
                                    t.adapter_luid
                                );
                            } else {
                                log::warn!("display: {key} (target {} of adapter {:x}) is connected but could not be attached", t.target_id, t.adapter_luid);
                            }
                        }
                        let detaching: Vec<TargetRef> = actions.detaching.iter().map(|(_, t)| *t).collect();
                        for t in detaching.iter().filter(|t| outcome.deactivated.contains(t)) {
                            log::info!("display: target {} of adapter {:x} detached again (no seat uses it)", t.target_id, t.adapter_luid);
                        }
                        report.changed = outcome.applied || !outcome.specialized.is_empty() || !outcome.unspecialized.is_empty();
                        report.rolled_back = outcome.rolled_back;
                        report.removed = all
                            .iter()
                            .filter(|c| !detaching.contains(&c.target))
                            .filter(|c| actions.request.remove.iter().any(|r| r.target == c.target) && outcome.took_out(&c.target))
                            .map(|c| c.key.clone())
                            .collect();
                        report.restored = all
                            .iter()
                            .filter(|c| !actions.attaching.contains(&c.target))
                            .filter(|c| {
                                actions.request.restore.iter().any(|r| {
                                    r.target == c.target
                                        && (!r.unspecialize || outcome.unspecialized.contains(&c.target))
                                        && (!r.activate || outcome.activated.contains(&c.target))
                                })
                            })
                            .map(|c| c.key.clone())
                            .collect();
                    }
                    // The journal written above stays: undoing an entry that never took
                    // effect is harmless.
                    Err(e) => log::warn!("display: topology change not applied: {e}"),
                }
            }
        }
        if report.changed {
            live = inner.live()?;
        }
        for (k, m) in inner.resolve_all(&plan.console, &live) {
            if !m.is_some_and(|m| m.target.active) {
                report.missing.push(k);
            }
        }
        if inner.kind == DisplayBackendKind::ConsoleOnly {
            return Ok(report);
        }
        for (_, keys) in &plan.seats {
            for (k, m) in inner.resolve_all(keys, &live) {
                match m.and_then(|m| inner.presentation_for(m)) {
                    None => report.missing.push(k),
                    Some(Presentation::Fallback) => report.fallback.push(k),
                    Some(_) => {}
                }
            }
        }
        Ok(report)
    }

    fn bind_seat(&self, seat: SeatNumber, targets: &[DeviceKey]) -> BackendResult<SeatOutputs> {
        let inner = &self.inner;
        if inner.kind == DisplayBackendKind::ConsoleOnly && !seat.is_console() {
            return Err(err(ErrorKind::Unsupported, "console_only", "console_only shows the console session on every monitor"));
        }
        if inner.kind == DisplayBackendKind::PulsarIdd && inner.idd.is_none() {
            return Err(err(ErrorKind::Driver, "driver_not_signed", "Pulsar IddCx driver is not available"));
        }
        // One bind / release at a time: the busy check below stays valid until the insert.
        let _bind = lock(&inner.bind);
        let live = inner.live()?;
        let resolved = inner.resolve_all(targets, &live);
        type BoundBy = HashMap<DeviceKey, Vec<(SeatNumber, Option<ScreenArea>)>>;
        let (bound_by, opts): (BoundBy, SeatDisplayOptions) = {
            let st = lock(&inner.state);
            let mut bound_by: BoundBy = HashMap::new();
            for (s, o) in st.bound.iter().filter(|(s, _)| **s != seat) {
                for p in o {
                    bound_by.entry(p.key.clone()).or_default().push((*s, p.area));
                }
            }
            (bound_by, st.options.get(&seat).cloned().unwrap_or_default())
        };
        // Whole-monitor users of a target block it; split users block only their cells.
        let busy: HashMap<DeviceKey, SeatNumber> = bound_by.iter().map(|(k, v)| (k.clone(), v[0].0)).collect();
        let conflict = |key: &DeviceKey, mine: Option<ScreenArea>| -> Option<SeatNumber> {
            bound_by
                .get(key)?
                .iter()
                .find(|(_, other)| match (mine, other) {
                    (Some(a), Some(b)) => a.overlaps(*b),
                    _ => true,
                })
                .map(|(s, _)| *s)
        };
        let resolved_key = |k: &DeviceKey, m: &Option<&LiveMonitor>| m.map_or_else(|| k.clone(), |m| m.device.key.clone());
        let areas: HashMap<DeviceKey, ScreenArea> =
            resolved.iter().filter_map(|(k, m)| opts.areas.get(k).map(|a| (resolved_key(k, m), *a))).collect();
        let share: HashSet<DeviceKey> =
            resolved.iter().filter(|(k, _)| opts.console_share.contains(k)).map(|(k, m)| resolved_key(k, m)).collect();

        // Software-cursor option by configured key, mapped to the resolved monitor.
        let sw: HashSet<DeviceKey> = resolved
            .iter()
            .filter(|(k, _)| opts.software_cursor.contains(k))
            .map(|(k, m)| m.map_or_else(|| k.clone(), |m| m.device.key.clone()))
            .collect();
        // HDR / VRR options the same way (seats 2..N, spec 40 §6.6).
        let by_key = |set: &HashSet<DeviceKey>| -> HashSet<DeviceKey> {
            resolved
                .iter()
                .filter(|(k, _)| set.contains(k))
                .map(|(k, m)| m.map_or_else(|| k.clone(), |m| m.device.key.clone()))
                .collect()
        };
        let hdr = by_key(&opts.hdr);
        let vrr = by_key(&opts.vrr);
        let mut chosen: Vec<(&LiveMonitor, Presentation)> = Vec::new();
        for (k, m) in &resolved {
            let Some(m) = m else {
                log::warn!("display: seat {seat}: monitor {k} not present");
                continue;
            };
            let area = areas.get(&m.device.key).copied();
            if let Some(other) = conflict(&m.device.key, area) {
                return Err(err(ErrorKind::NotReady, "target_busy", format!("{} is bound to seat {other}", m.device.key)));
            }
            if area.is_some() {
                // A split monitor is shown by several seat hosts at once: only windows on a
                // console-desktop target can share it (one owner per scan-out otherwise).
                if !m.target.active || m.target.desktop_rect.is_none() {
                    return Err(err(
                        ErrorKind::NotReady,
                        "split_needs_desktop",
                        format!("{} is split between seats but not part of the console desktop", m.device.key),
                    ));
                }
                chosen.push((m, Presentation::Fallback));
                continue;
            }
            match inner.presentation_for(m) {
                Some(p) => chosen.push((m, p)),
                None => log::warn!("display: seat {seat}: {k} is not in the desktop and not special purpose"),
            }
        }
        if chosen.is_empty() {
            return Err(err(ErrorKind::NotReady, "no_display", format!("seat {seat} has no usable monitor")));
        }

        // Clone groups (spec 40 §6.3.6): further targets of a surface on the same adapter.
        let mut clones: Vec<(&LiveMonitor, Presentation, usize)> = Vec::new();
        for (surface, ids) in opts.outputs.iter().filter(|(_, ids)| !ids.is_empty()) {
            let resolved_surface = resolved.iter().find(|(k, _)| k == surface).and_then(|(_, m)| *m);
            let Some(sm) = resolved_surface else { continue };
            let Some(mi) = chosen.iter().position(|(m, _)| m.device.key == sm.device.key) else { continue };
            let luid = sm.target.adapter_luid;
            let on_adapter = |id: u32| live.iter().find(|m| m.target.adapter_luid == luid && m.target.target_id == id);
            // The surface's own target leads; when the list names other targets only, the
            // first live one takes its place.
            if !ids.contains(&sm.target.target_id) {
                if let Some((m, p)) = ids.iter().filter_map(|&id| on_adapter(id)).find_map(|m| inner.presentation_for(m).map(|p| (m, p))) {
                    if !busy.contains_key(&m.device.key) {
                        chosen[mi] = (m, p);
                    }
                }
            }
            let lead = chosen[mi].0.target.target_id;
            for &id in ids.iter().filter(|&&id| id != lead) {
                let Some(m) = on_adapter(id) else {
                    log::warn!("display: seat {seat}: clone target {id} of {surface} not present");
                    continue;
                };
                if let Some(other) = busy.get(&m.device.key) {
                    return Err(err(ErrorKind::NotReady, "target_busy", format!("{} is bound to seat {other}", m.device.key)));
                }
                match inner.presentation_for(m) {
                    Some(p) if !clones.iter().any(|(c, _, _)| c.device.key == m.device.key) => clones.push((m, p, mi)),
                    Some(_) => {}
                    None => log::warn!("display: seat {seat}: clone target {id} is not in the desktop and not special purpose"),
                }
            }
        }
        // A target used as a clone is not also a surface of its own.
        let clone_keys: HashSet<DeviceKey> = clones.iter().map(|(m, _, _)| m.device.key.clone()).collect();
        let mut new_index = Vec::with_capacity(chosen.len());
        let mut kept = Vec::with_capacity(chosen.len());
        for (m, p) in chosen.into_iter() {
            if clone_keys.contains(&m.device.key) {
                new_index.push(None);
            } else {
                new_index.push(Some(kept.len()));
                kept.push((m, p));
            }
        }
        let chosen = kept;
        let clones: Vec<(&LiveMonitor, Presentation, usize)> =
            clones.into_iter().filter_map(|(m, p, mi)| new_index[mi].map(|i| (m, p, i))).collect();
        let follow = |p: Presentation| opts.follow_session_mode.unwrap_or(p == Presentation::Presenter);

        // Presenter targets get mode-sized rects right of the desktop ones.
        let right = chosen
            .iter()
            .filter_map(|(m, p)| (*p != Presentation::Presenter).then_some(m.target.desktop_rect).flatten())
            .map(|r| r.x + r.width as i32)
            .max()
            .unwrap_or(0);
        let pres_modes: Vec<DisplayMode> = chosen
            .iter()
            .filter(|(_, p)| *p == Presentation::Presenter)
            .map(|(m, _)| m.target.preferred.or(m.target.mode).unwrap_or(DisplayMode { width: 1920, height: 1080, refresh_mhz: 60000, rotation: 0 }))
            .collect();
        let mut vrects = virtual_rects(&pres_modes).into_iter().map(|r| Rect { x: r.x + right, ..r });

        let mut outputs = Vec::new();
        for (m, p) in &chosen {
            let t = &m.target;
            let area = areas.get(&m.device.key).copied();
            let rect = if *p == Presentation::Presenter {
                vrects.next().unwrap_or_default()
            } else {
                let r = t.desktop_rect.unwrap_or_default();
                area.map_or(r, |a| a.rect_in(&r))
            };
            outputs.push(OutputPlacement {
                key: m.device.key.clone(),
                rect,
                target_id: t.target_id,
                adapter_luid: t.adapter_luid,
                mode: if *p == Presentation::Presenter { t.preferred.or(t.mode) } else { t.mode },
                presentation: *p,
                software_cursor: sw.contains(&m.device.key),
                clone_of: None,
                // A shared monitor keeps its mode (the other seats use it too).
                follow_mode: area.is_none() && follow(*p),
                hdr: !seat.is_console() && hdr.contains(&m.device.key),
                vrr: !seat.is_console() && vrr.contains(&m.device.key),
                area,
                console_share: area.is_some_and(|a| a.edge().is_some()) && share.contains(&m.device.key),
            });
        }
        for (m, p, master) in &clones {
            let t = &m.target;
            let mode = if *p == Presentation::Presenter { t.preferred.or(t.mode) } else { t.mode };
            let rect = match p {
                Presentation::Presenter => mode.map(|md| {
                    let (w, h) = crate::model::rotated(&md);
                    Rect { x: 0, y: 0, width: w, height: h }
                }),
                _ => t.desktop_rect,
            };
            outputs.push(OutputPlacement {
                key: m.device.key.clone(),
                rect: rect.unwrap_or_default(),
                target_id: t.target_id,
                adapter_luid: t.adapter_luid,
                mode,
                presentation: *p,
                software_cursor: sw.contains(&m.device.key) || outputs[*master].software_cursor,
                clone_of: Some(*master),
                follow_mode: false,
                // A clone shows its surface's frames: same colour mode; its own VRR choice.
                hdr: outputs[*master].hdr,
                vrr: !seat.is_console() && (vrr.contains(&m.device.key) || outputs[*master].vrr),
                area: None,
                console_share: false,
            });
        }

        let plug = inner.kind == DisplayBackendKind::PulsarIdd && !inner.session_has_display.load(Ordering::SeqCst);
        if inner.kind == DisplayBackendKind::PulsarIdd && !plug {
            log::info!("display: seat {seat}: session backend has its own display, no IddCx monitors (spec 00 T1)");
        }
        if plug {
            let idd = inner.idd.as_ref().expect("checked above");
            // Re-binding: drop the seat's old virtual monitors first. From here on the old
            // placement is stale, so it leaves `bound` before any re-add can fail.
            if let Some(old) = lock(&inner.state).bound.remove(&seat) {
                for i in 0..surfaces(&old) {
                    let _ = idd.remove_monitor(seat, i as u32);
                }
            }
            // One IddCx adapter per render GPU (driver minor 3, crate::idd_adapters): each
            // virtual monitor goes to the adapter bound to its physical monitor's GPU, so
            // every seat composes on the GPU that scans it out.
            {
                let gpus = crate::idd_adapters::distinct_gpus(chosen.iter().map(|(m, _)| m.target.adapter_luid));
                if gpus.len() > 1 {
                    log::info!("display: seat {seat}: virtual monitors on {} GPUs, one IddCx adapter each", gpus.len());
                }
            }
            for (i, ((m, _), o)) in chosen.iter().zip(&outputs).enumerate() {
                let mut modes: Vec<DisplayMode> = [m.target.preferred, m.target.mode].into_iter().flatten().collect();
                if o.area.is_some() {
                    // Split monitor: the virtual monitor is the size of the seat's part.
                    // HYPOTHESIS: Windows picks this mode although the EDID's preferred
                    // timing is the full panel (untested on hardware).
                    let refresh_mhz = modes.first().map_or(60_000, |md| md.refresh_mhz);
                    modes = vec![DisplayMode { width: o.rect.width, height: o.rect.height, refresh_mhz, rotation: 0 }];
                }
                let edid = m.edid_blob.clone().unwrap_or_default();
                let req = IddMonitor {
                    seat,
                    connector: i as u32,
                    edid: &edid,
                    modes: &modes,
                    render_luid: m.target.adapter_luid,
                    reader_sid: opts.frame_reader_sid.as_deref(),
                    software_cursor: o.software_cursor,
                    hdr: o.hdr,
                };
                if let Err(e) = idd.add_monitor(&req) {
                    for j in 0..i {
                        let _ = idd.remove_monitor(seat, j as u32);
                    }
                    return Err(e);
                }
            }
        }

        lock(&inner.state).bound.insert(seat, outputs.clone());
        Ok(SeatOutputs { seat, outputs })
    }

    fn release_seat(&self, seat: SeatNumber) -> BackendResult<()> {
        let _bind = lock(&self.inner.bind);
        let old = lock(&self.inner.state).bound.remove(&seat);
        let plugged = self.inner.kind == DisplayBackendKind::PulsarIdd && !self.inner.session_has_display.load(Ordering::SeqCst);
        if let (Some(old), Some(idd), true) = (old, self.inner.idd.as_ref(), plugged) {
            let mut first_err = None;
            for i in 0..surfaces(&old) {
                if let Err(e) = idd.remove_monitor(seat, i as u32) {
                    first_err.get_or_insert(e);
                }
            }
            if let Some(e) = first_err {
                return Err(e);
            }
        }
        Ok(())
    }

    fn identify(&self, targets: &[(DeviceKey, String)], on: bool) -> BackendResult<()> {
        let router = lock(&self.inner.router).clone();
        if !on {
            // Hide everywhere: console overlays and every presenter output.
            if let Some(r) = &router {
                let outs: Vec<(SeatNumber, usize)> = lock(&self.inner.state)
                    .bound
                    .iter()
                    .flat_map(|(s, o)| o.iter().enumerate().filter(|(_, p)| p.presentation == Presentation::Presenter).map(move |(i, _)| (*s, i)))
                    .collect();
                for (s, i) in outs {
                    let _ = r(s, PresenterCommand::Identify { output: i, label: String::new(), seconds: 0 });
                }
            }
            return self.inner.console.exec(&ConsoleCommand::Identify { items: vec![], on: false, seconds: 0 });
        }
        let live = self.inner.live()?;
        let keys: Vec<DeviceKey> = targets.iter().map(|(k, _)| k.clone()).collect();
        let resolved = self.inner.resolve_all(&keys, &live);
        let mut routed = 0usize;
        let mut items: Vec<IdentifyItem> = Vec::new();
        for ((k, m), (_, label)) in resolved.iter().zip(targets) {
            if let Some(rect) = m.and_then(|m| m.target.active.then_some(m.target.desktop_rect).flatten()) {
                items.push(IdentifyItem { rect, label: label.clone() });
                continue;
            }
            // Presenter target: the seat host draws the number into its scan-out.
            let key = m.map_or(k, |m| &m.device.key);
            if let (Some(r), Some((seat, output))) = (&router, self.inner.presenter_output(key)) {
                match r(seat, PresenterCommand::Identify { output, label: label.clone(), seconds: IDENTIFY_UNTIL_HIDDEN }) {
                    Ok(()) => routed += 1,
                    Err(e) => log::warn!("display: identify {key} via seat {seat}: {e}"),
                }
            }
        }
        if items.is_empty() {
            if routed > 0 {
                return Ok(());
            }
            return Err(err(ErrorKind::NotFound, "target_not_in_desktop", "none of the monitors is part of the console desktop or presented by a seat"));
        }
        self.inner.console.exec(&ConsoleCommand::Identify { items, on: true, seconds: IDENTIFY_UNTIL_HIDDEN })
    }

    /// Desktop monitors: DDC/CI through the console helper. Presenter targets: the seat
    /// host blanks / restores the scan-out; powering off then returns `Unsupported` /
    /// `blanked_only` (blanked, not powered down: DDC/CI needs an HMONITOR).
    fn set_power(&self, key: &DeviceKey, on: bool) -> BackendResult<()> {
        let live = self.inner.live()?;
        let resolved = self.inner.resolve_all(std::slice::from_ref(key), &live);
        let m = resolved[0].1.ok_or_else(|| err(ErrorKind::NotFound, "not_found", format!("{key} not present")))?;
        if !m.target.active {
            let router = lock(&self.inner.router).clone();
            if let (Some(r), Some((seat, output))) = (router, self.inner.presenter_output(&m.device.key)) {
                r(seat, PresenterCommand::Power { output, on })?;
                if on {
                    return Ok(());
                }
                return Err(err(ErrorKind::Unsupported, "blanked_only", "presenter target blanked; DDC/CI needs the monitor in the console desktop"));
            }
        }
        let gdi = m
            .target
            .gdi_name
            .clone()
            .filter(|_| m.target.active)
            .ok_or_else(|| err(ErrorKind::Unsupported, "not_in_desktop", "DDC/CI needs the monitor in the console desktop"))?;
        self.inner.console.exec(&ConsoleCommand::Power { gdi_name: gdi, on })
    }

    fn set_session_has_display(&self, has: bool) {
        self.set_session_has_display_flag(has);
    }

    fn idd_frames(&self, seat: SeatNumber) -> bool {
        self.inner.kind == DisplayBackendKind::PulsarIdd
            && !self.inner.session_has_display.load(Ordering::SeqCst)
            && lock(&self.inner.state).bound.contains_key(&seat)
    }

    fn configure_seat(&self, seat: SeatNumber, prefs: &SeatDisplayPrefs) {
        let mut st = lock(&self.inner.state);
        let o = st.options.entry(seat).or_default();
        o.software_cursor = prefs.software_cursor.iter().cloned().collect();
        o.outputs = prefs.outputs.clone();
        o.follow_session_mode = prefs.follow_session_mode;
        o.frame_reader_sid = prefs.frame_reader_sid.clone();
        o.hdr = prefs.hdr.iter().cloned().collect();
        o.vrr = prefs.vrr.iter().cloned().collect();
        o.areas = prefs.areas.iter().cloned().collect();
        o.console_share = prefs.console_share.iter().cloned().collect();

    }

    fn set_hotplug_sink(&self, sink: HotplugSink) {
        *lock(&self.inner.sink) = Some(sink);
        let mut l = lock(&self.inner.listener);
        if l.is_none() {
            #[cfg(windows)]
            {
                match crate::win::hotplug::start(self.weak()) {
                    Ok(h) => *l = Some(Box::new(h)),
                    Err(e) => log::warn!("display: hot-plug listener failed: {e}"),
                }
            }
        }
    }
}

/// Number of non-clone placements (one IddCx virtual monitor each).
fn surfaces(o: &[OutputPlacement]) -> usize {
    o.iter().filter(|p| p.clone_of.is_none()).count()
}

/// Capabilities per kind.
pub fn caps_for(kind: DisplayBackendKind) -> DisplayCaps {
    match kind {
        DisplayBackendKind::WindowPresenter => DisplayCaps {
            isolates_console: false,
            multi_monitor: true,
            clone: true,
            hardware_cursor: false,
            monitor_power: true,
            driver: None,
        },
        DisplayBackendKind::DisplayCore => DisplayCaps {
            isolates_console: true,
            multi_monitor: true,
            clone: true,
            hardware_cursor: false,
            monitor_power: true,
            driver: None,
        },
        // Experimental: the IddCx monitor is plugged PC-wide today, so the console desktop
        // may extend onto it (spec 40 §6.3.1 / open question 1 unsolved, see
        // drivers/seat-idd/README.md "Known gaps").
        DisplayBackendKind::PulsarIdd => DisplayCaps {
            isolates_console: false,
            multi_monitor: true,
            clone: true,
            hardware_cursor: true,
            monitor_power: true,
            driver: Some(DriverId::Idd),
        },
        DisplayBackendKind::ConsoleOnly => DisplayCaps {
            isolates_console: false,
            multi_monitor: false,
            clone: false,
            hardware_cursor: false,
            monitor_power: true,
            driver: None,
        },
    }
}

fn display_core_availability(console: &dyn Console) -> Availability {
    match console.snapshot(true) {
        Err(e) => Availability::Unavailable { reason: e.reason },
        Ok(s) => match s.display_core {
            Some(Err(_)) | None => Availability::Unavailable { reason: "display_core_unavailable".into() },
            Some(Ok(())) if s.targets.iter().any(|t| t.specialized) => Availability::Available,
            Some(Ok(())) => Availability::NeedsSetup { what: "specialized_display".into() },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::tests_support::{edid, target, tree};
    use crate::model::Snapshot;
    use std::sync::atomic::AtomicUsize;

    struct FakeConsole {
        snap: Mutex<Snapshot>,
        cmds: Mutex<Vec<ConsoleCommand>>,
    }
    impl Console for FakeConsole {
        fn snapshot(&self, dc: bool) -> BackendResult<Snapshot> {
            let mut s = lock(&self.snap).clone();
            if dc {
                s.display_core = Some(Ok(()));
            }
            Ok(s)
        }
        fn exec(&self, cmd: &ConsoleCommand) -> BackendResult<()> {
            lock(&self.cmds).push(cmd.clone());
            Ok(())
        }
        /// Simulates CCD: specialization takes the target out of the desktop.
        fn apply_topology(&self, req: &crate::topology::TopologyRequest) -> BackendResult<crate::topology::TopologyOutcome> {
            lock(&self.cmds).push(ConsoleCommand::ApplyTopology { request: req.clone() });
            let mut out = crate::topology::TopologyOutcome { specialization_supported: true, applied: true, ..Default::default() };
            let mut snap = lock(&self.snap);
            for r in &req.remove {
                if let Some(t) = snap.targets.iter_mut().find(|t| t.adapter_luid == r.target.adapter_luid && t.target_id == r.target.target_id) {
                    if r.specialize {
                        t.specialized = true;
                        out.specialized.push(r.target);
                    }
                    t.active = false;
                    t.desktop_rect = None;
                    out.deactivated.push(r.target);
                }
            }
            for r in &req.restore {
                if let Some(t) = snap.targets.iter_mut().find(|t| t.adapter_luid == r.target.adapter_luid && t.target_id == r.target.target_id) {
                    if r.unspecialize {
                        t.specialized = false;
                        out.unspecialized.push(r.target);
                    }
                    if r.activate {
                        t.active = true;
                        t.desktop_rect = Some(Rect { x: 3840, y: 0, width: 1920, height: 1080 });
                        out.activated.push(r.target);
                    }
                }
            }
            Ok(out)
        }
    }

    fn setup(kind: DisplayBackendKind, targets: Vec<crate::model::TargetInfo>) -> (Binding, Arc<FakeConsole>) {
        let c = Arc::new(FakeConsole { snap: Mutex::new(Snapshot { targets, display_core: None }), cmds: Mutex::new(vec![]) });
        let b = Binding::new(kind, c.clone(), Arc::new(tree()), Box::new(|_| Some(edid(9))));
        (b, c)
    }

    fn s(n: u8) -> SeatNumber {
        SeatNumber::new(n).unwrap()
    }

    #[test]
    fn split_monitor_is_shared_by_cells_and_released_per_seat() {
        let (b, _c) = setup(DisplayBackendKind::WindowPresenter, vec![target(5, 1, true, 0), target(5, 2, true, 1920), target(5, 3, false, 0)]);
        let devs = b.enumerate().unwrap();
        let k = devs[1].key.clone();
        let monitor = Rect { x: 1920, y: 0, width: 1920, height: 1080 };
        let prefs = |a: ScreenArea| SeatDisplayPrefs { areas: vec![(k.clone(), a)], follow_session_mode: Some(true), ..Default::default() };
        b.configure_seat(s(2), &prefs(ScreenArea::Left));
        b.configure_seat(s(3), &prefs(ScreenArea::TopRight));
        b.configure_seat(s(4), &prefs(ScreenArea::Right));
        let o2 = b.bind_seat(s(2), std::slice::from_ref(&k)).unwrap();
        assert_eq!(o2.outputs[0].area, Some(ScreenArea::Left));
        assert_eq!(o2.outputs[0].rect, ScreenArea::Left.rect_in(&monitor));
        assert_eq!(o2.outputs[0].presentation, Presentation::Fallback);
        assert!(!o2.outputs[0].follow_mode, "a shared monitor keeps its mode");
        let o3 = b.bind_seat(s(3), std::slice::from_ref(&k)).unwrap();
        assert_eq!(o3.outputs[0].rect, Rect { x: 2880, y: 0, width: 960, height: 540 });
        // Right overlaps seat 3's top-right quarter.
        assert_eq!(b.bind_seat(s(4), std::slice::from_ref(&k)).unwrap_err().reason, "target_busy");
        // A whole-monitor seat cannot join a split monitor, nor split a whole one.
        assert_eq!(b.bind_seat(s(5), std::slice::from_ref(&k)).unwrap_err().reason, "target_busy");
        // Releasing seat 3 frees its cells only; the monitor stays bound for seat 2.
        b.release_seat(s(3)).unwrap();
        assert!(b.seat_outputs(s(2)).is_some());
        assert_eq!(b.bind_seat(s(4), std::slice::from_ref(&k)).unwrap().outputs[0].rect, ScreenArea::Right.rect_in(&monitor));
        b.release_seat(s(2)).unwrap();
        b.release_seat(s(4)).unwrap();
        assert!(b.bind_seat(s(5), std::slice::from_ref(&k)).is_ok(), "free again after the last seat");
        // A split needs a console-desktop target.
        b.configure_seat(s(6), &SeatDisplayPrefs { areas: vec![(devs[2].key.clone(), ScreenArea::Left)], ..Default::default() });
        assert_eq!(b.bind_seat(s(6), &[devs[2].key.clone()]).unwrap_err().reason, "split_needs_desktop");
        // Console share is set only for halves the console shares.
        b.release_seat(s(5)).unwrap();
        b.configure_seat(s(2), &SeatDisplayPrefs { areas: vec![(k.clone(), ScreenArea::Right)], console_share: vec![k.clone()], ..Default::default() });
        assert!(b.bind_seat(s(2), std::slice::from_ref(&k)).unwrap().outputs[0].console_share);
    }

    #[test]
    fn window_presenter_binds_desktop_targets() {
        let (b, c) = setup(DisplayBackendKind::WindowPresenter, vec![target(5, 1, true, 0), target(5, 2, true, 1920), target(5, 3, false, 0)]);
        let devs = b.enumerate().unwrap();
        assert_eq!(devs.len(), 3);
        let k2 = devs[1].key.clone();
        let out = b.bind_seat(s(2), std::slice::from_ref(&k2)).unwrap();
        assert_eq!(out.outputs.len(), 1);
        assert_eq!(out.outputs[0].presentation, Presentation::Fallback);
        assert_eq!(out.outputs[0].rect.x, 1920);
        // Busy for another seat.
        assert_eq!(b.bind_seat(s(3), std::slice::from_ref(&k2)).unwrap_err().reason, "target_busy");
        // Inactive target cannot be used by the window presenter.
        assert_eq!(b.bind_seat(s(3), &[devs[2].key.clone()]).unwrap_err().reason, "no_display");
        // Enumerate now shows the seat's presentation.
        let devs = b.enumerate().unwrap();
        match &devs[1].details {
            DeviceDetails::Display(d) => assert_eq!(d.presentation, Presentation::Fallback),
            _ => panic!(),
        }
        b.identify(&[(devs[0].key.clone(), "1".into())], true).unwrap();
        b.set_power(&devs[0].key, false).unwrap();
        assert!(b.set_power(&devs[2].key, false).is_err());
        let cmds = lock(&c.cmds).clone();
        assert!(matches!(&cmds[0], ConsoleCommand::Identify { items, on: true, .. } if items.len() == 1));
        assert!(matches!(&cmds[1], ConsoleCommand::Power { on: false, .. }));
        // Bug 9 (gaming PC E2E): the connected target outside the desktop is attached by the
        // seat's plan (explicit CCD paths, journaled), then binds as a desktop target.
        let k3 = devs[2].key.clone();
        let plan = TopologyPlan { console: vec![devs[0].key.clone()], seats: vec![(s(2), vec![k2.clone()]), (s(3), vec![k3.clone()])], attach: vec![] };
        let r = b.apply_topology(&plan).unwrap();
        assert!(r.changed);
        assert!(r.missing.is_empty(), "{:?}", r.missing);
        assert_eq!(r.fallback, vec![k2.clone(), k3.clone()]);
        assert!(r.restored.is_empty(), "an attach is no journal undo");
        let req = lock(&c.cmds).iter().rev().find_map(|c| match c {
            ConsoleCommand::ApplyTopology { request } => Some(request.clone()),
            _ => None,
        });
        assert_eq!(req.unwrap().restore, vec![crate::topology::RestoreTarget { target: TargetRef { adapter_luid: 5, target_id: 3 }, unspecialize: false, activate: true }]);
        let j = b.topology_journal();
        assert_eq!(j.entries.len(), 1);
        assert!(j.entries[0].attached && j.entries[0].key == k3);
        assert_eq!(b.bind_seat(s(3), std::slice::from_ref(&k3)).unwrap().outputs[0].presentation, Presentation::Fallback);
        b.release_seat(s(3)).unwrap();
        // Same plan again: nothing to do.
        assert!(!b.apply_topology(&plan).unwrap().changed);
        // Seat 3 no longer uses it: the path goes again and the journal is empty.
        let r = b.apply_topology(&TopologyPlan { console: vec![devs[0].key.clone()], seats: vec![(s(2), vec![k2.clone()])], attach: vec![] }).unwrap();
        assert!(r.changed && r.removed.is_empty());
        assert!(b.topology_journal().entries.is_empty());
        assert_eq!(b.bind_seat(s(3), std::slice::from_ref(&k3)).unwrap_err().reason, "no_display");
        b.release_seat(s(2)).unwrap();
        assert!(b.seat_outputs(s(2)).is_none());
        // console_only never attaches (its seat agent owns the topology).
        let (b, c) = setup(DisplayBackendKind::ConsoleOnly, vec![target(5, 1, true, 0), target(5, 3, false, 0)]);
        let devs = b.enumerate().unwrap();
        let all: Vec<DeviceKey> = devs.iter().map(|d| d.key.clone()).collect();
        let r = b.apply_topology(&TopologyPlan { console: all.clone(), seats: vec![], attach: all }).unwrap();
        assert!(!r.changed && r.missing == vec![devs[1].key.clone()]);
        assert!(lock(&c.cmds).is_empty());
    }

    /// Gaming PC E2E round 2 bug H: an unassigned monitor the user keeps out of the desktop
    /// is not attached at product enable; a monitor attached for a seat goes back out when
    /// no seat needs it (seat config dropped it, product turned off).
    #[test]
    fn attached_monitor_is_detached_when_no_seat_needs_it() {
        let (b, c) = setup(DisplayBackendKind::WindowPresenter, vec![target(5, 1, true, 0), target(5, 3, false, 0)]);
        let devs = b.enumerate().unwrap();
        let (k1, k3) = (devs[0].key.clone(), devs[1].key.clone());
        let active = |c: &FakeConsole| lock(&c.snap).targets.iter().find(|t| t.target_id == 3).unwrap().active;
        let both = vec![k1.clone(), k3.clone()];
        // Product enabled, no seat uses the monitor: the service lists it as a console
        // monitor (default mask) without `attach`. Left alone.
        let idle = TopologyPlan { console: both.clone(), seats: vec![], attach: vec![] };
        assert!(!b.apply_topology(&idle).unwrap().changed);
        assert!(!active(&c) && b.topology_journal().entries.is_empty());
        // Seat 2 gets it: attached and journaled.
        let seat2 = TopologyPlan { console: vec![k1.clone()], seats: vec![(s(2), vec![k3.clone()])], attach: vec![] };
        assert!(b.apply_topology(&seat2).unwrap().changed);
        assert!(active(&c) && b.topology_journal().entries[0].attached);
        // Dropped from the seat config (unassigned again): detached, journal empty.
        let r = b.apply_topology(&idle).unwrap();
        assert!(r.changed && r.removed.is_empty() && r.restored.is_empty());
        assert!(!active(&c) && b.topology_journal().entries.is_empty());
        // Product off (`release_ownership`: every monitor a console monitor, nothing to
        // attach): detached the same way.
        b.apply_topology(&seat2).unwrap();
        assert!(active(&c));
        b.apply_topology(&TopologyPlan { console: both.clone(), seats: vec![], attach: vec![] }).unwrap();
        assert!(!active(&c) && b.topology_journal().entries.is_empty());
        // Seat 1's explicit monitor (or the wizard's extend step) is attached, and detached
        // again once the plan stops asking.
        b.apply_topology(&TopologyPlan { console: both.clone(), seats: vec![], attach: vec![k3.clone()] }).unwrap();
        assert!(active(&c) && b.topology_journal().entries[0].attached);
        b.apply_topology(&idle).unwrap();
        assert!(!active(&c) && b.topology_journal().entries.is_empty());
    }

    #[test]
    fn display_core_presenter_and_availability() {
        let mut sp = target(5, 7, false, 0);
        sp.specialized = true;
        sp.preferred = Some(DisplayMode { width: 2560, height: 1440, refresh_mhz: 144000, rotation: 0 });
        let (b, _) = setup(DisplayBackendKind::DisplayCore, vec![target(5, 1, true, 0), sp]);
        assert_eq!(b.availability(), Availability::Available);
        let devs = b.enumerate().unwrap();
        let out = b.bind_seat(s(2), &[devs[0].key.clone(), devs[1].key.clone()]).unwrap();
        assert_eq!(out.outputs[0].presentation, Presentation::Fallback);
        assert_eq!(out.outputs[1].presentation, Presentation::Presenter);
        assert_eq!(out.outputs[1].rect, Rect { x: 1920, y: 0, width: 2560, height: 1440 });
        let (b, _) = setup(DisplayBackendKind::DisplayCore, vec![target(5, 1, true, 0)]);
        assert!(matches!(b.availability(), Availability::NeedsSetup { .. }));
    }

    struct FakeIdd(AtomicUsize, AtomicUsize, Mutex<Vec<(Option<String>, bool)>>, AtomicBool);
    impl IddControl for FakeIdd {
        fn availability(&self) -> Availability {
            Availability::Available
        }
        fn add_monitor(&self, r: &IddMonitor<'_>) -> BackendResult<()> {
            assert!(!r.edid.is_empty() && !r.modes.is_empty());
            if self.3.load(Ordering::SeqCst) {
                return Err(err(ErrorKind::Driver, "driver_ioctl", "collision"));
            }
            lock(&self.2).push((r.reader_sid.map(str::to_string), r.software_cursor));
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn remove_monitor(&self, _: SeatNumber, _: u32) -> BackendResult<()> {
            self.1.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    fn fake_idd() -> Arc<FakeIdd> {
        Arc::new(FakeIdd(AtomicUsize::new(0), AtomicUsize::new(0), Mutex::new(vec![]), AtomicBool::new(false)))
    }

    #[test]
    fn idd_adds_and_removes_virtual_monitors() {
        let (b, _) = setup(DisplayBackendKind::PulsarIdd, vec![target(5, 1, true, 0)]);
        assert_eq!(b.availability(), Availability::DriverNotSigned { driver: DriverId::Idd });
        let idd = fake_idd();
        let b = b.with_idd(idd.clone());
        let devs = b.enumerate().unwrap();
        // Session backend with its own display (default): no virtual monitors.
        b.bind_seat(s(2), &[devs[0].key.clone()]).unwrap();
        b.release_seat(s(2)).unwrap();
        assert_eq!(idd.0.load(Ordering::SeqCst), 0);
        assert_eq!(idd.1.load(Ordering::SeqCst), 0);

        b.set_session_has_display_flag(false);
        // The service path: `configure_seat` carries the reader SID (host account).
        let prefs = SeatDisplayPrefs {
            frame_reader_sid: Some("S-1-5-21-1-2-3-1001".into()),
            software_cursor: vec![devs[0].key.clone()],
            ..Default::default()
        };
        b.configure_seat(s(2), &prefs);
        let out = b.bind_seat(s(2), &[devs[0].key.clone()]).unwrap();
        assert!(out.outputs[0].software_cursor);
        assert_eq!(lock(&idd.2).clone(), vec![(Some("S-1-5-21-1-2-3-1001".to_string()), true)]);
        b.release_seat(s(2)).unwrap();
        assert_eq!(idd.0.load(Ordering::SeqCst), 1);
        assert_eq!(idd.1.load(Ordering::SeqCst), 1);

        // A failed re-add leaves no stale placement behind.
        b.bind_seat(s(2), &[devs[0].key.clone()]).unwrap();
        idd.3.store(true, Ordering::SeqCst);
        assert!(b.bind_seat(s(2), &[devs[0].key.clone()]).is_err());
        assert!(b.seat_outputs(s(2)).is_none());
        let removes = idd.1.load(Ordering::SeqCst);
        b.release_seat(s(2)).unwrap();
        assert_eq!(idd.1.load(Ordering::SeqCst), removes, "nothing left to remove");
        assert!(!b.caps().isolates_console);
    }

    /// Records (seat, connector, render GPU, hdr) per add.
    struct GpuIdd(Mutex<Vec<(u8, u32, u64, bool)>>);
    impl IddControl for GpuIdd {
        fn availability(&self) -> Availability {
            Availability::Available
        }
        fn add_monitor(&self, r: &IddMonitor<'_>) -> BackendResult<()> {
            lock(&self.0).push((r.seat.get(), r.connector, r.render_luid, r.hdr));
            Ok(())
        }
        fn remove_monitor(&self, _: SeatNumber, _: u32) -> BackendResult<()> {
            Ok(())
        }
    }

    #[test]
    fn hdr_vrr_options_and_render_gpu_per_monitor() {
        // Two GPUs (LUID 5 and 6), one monitor each, plus seat 1's monitor on GPU 5.
        let mut other_gpu = target(6, 1, true, 3840);
        other_gpu.adapter_path = r"\\?\PCI#VEN_1002&DEV_73BF#4&2#{5b45201d-f2f2-4f3b-85bb-30ff1f953599}".into();
        other_gpu.gdi_name = Some("\\\\.\\DISPLAY3".into());
        let mut second = target(5, 2, true, 1920);
        second.gdi_name = Some("\\\\.\\DISPLAY2".into());
        let (b, _) = setup(DisplayBackendKind::PulsarIdd, vec![target(5, 1, true, 0), second, other_gpu]);
        let idd = Arc::new(GpuIdd(Mutex::new(vec![])));
        let b = b.with_idd(idd.clone());
        b.set_session_has_display_flag(false);
        let devs = b.enumerate().unwrap();
        let prefs = SeatDisplayPrefs { hdr: vec![devs[1].key.clone()], vrr: vec![devs[1].key.clone(), devs[2].key.clone()], ..Default::default() };
        b.configure_seat(s(2), &prefs);
        let out = b.bind_seat(s(2), &[devs[1].key.clone(), devs[2].key.clone()]).unwrap();
        assert_eq!((out.outputs[0].hdr, out.outputs[0].vrr), (true, true));
        assert_eq!((out.outputs[1].hdr, out.outputs[1].vrr), (false, true));
        // Each virtual monitor names its own physical monitor's GPU (the client picks the
        // adapter bound to it).
        assert_eq!(lock(&idd.0).clone(), vec![(2, 0, 5, true), (2, 1, 6, false)]);
        // Seat 1 never gets HDR / VRR through the presenter (Windows' own settings apply).
        b.configure_seat(s(1), &SeatDisplayPrefs { hdr: vec![devs[0].key.clone()], vrr: vec![devs[0].key.clone()], ..Default::default() });
        let one = b.bind_seat(s(1), &[devs[0].key.clone()]).unwrap();
        assert!(!one.outputs[0].hdr && !one.outputs[0].vrr);
    }

    #[test]
    fn presenter_targets_are_routed_to_the_seat_host() {
        let mut sp = target(5, 7, false, 0);
        sp.specialized = true;
        let (b, c) = setup(DisplayBackendKind::DisplayCore, vec![target(5, 1, true, 0), sp]);
        let devs = b.enumerate().unwrap();
        // Without a router a presenter target cannot be identified.
        b.bind_seat(s(2), &[devs[1].key.clone()]).unwrap();
        assert_eq!(b.identify(&[(devs[1].key.clone(), "2".into())], true).unwrap_err().reason, "target_not_in_desktop");
        let got = Arc::new(Mutex::new(Vec::new()));
        let g = got.clone();
        b.set_presenter_router(Arc::new(move |seat, cmd| {
            lock(&g).push((seat, cmd));
            Ok(())
        }));
        b.identify(&[(devs[1].key.clone(), "2".into())], true).unwrap();
        assert_eq!(b.set_power(&devs[1].key, false).unwrap_err().reason, "blanked_only");
        b.set_power(&devs[1].key, true).unwrap();
        b.identify(&[], false).unwrap();
        let got = lock(&got).clone();
        assert_eq!(
            got,
            vec![
                (s(2), PresenterCommand::Identify { output: 0, label: "2".into(), seconds: IDENTIFY_UNTIL_HIDDEN }),
                (s(2), PresenterCommand::Power { output: 0, on: false }),
                (s(2), PresenterCommand::Power { output: 0, on: true }),
                (s(2), PresenterCommand::Identify { output: 0, label: String::new(), seconds: 0 }),
            ]
        );
        // The console got only the hide command.
        assert!(matches!(lock(&c.cmds).as_slice(), [ConsoleCommand::Identify { on: false, .. }]));
    }

    #[test]
    fn concurrent_binds_cannot_share_a_target() {
        let (b, _) = setup(DisplayBackendKind::WindowPresenter, vec![target(5, 1, true, 0)]);
        let k = b.enumerate().unwrap()[0].key.clone();
        let ok = Arc::new(AtomicUsize::new(0));
        let hs: Vec<_> = (2..=9)
            .map(|n| {
                let (b, k, ok) = (b.clone(), k.clone(), ok.clone());
                std::thread::spawn(move || {
                    if b.bind_seat(s(n), std::slice::from_ref(&k)).is_ok() {
                        ok.fetch_add(1, Ordering::SeqCst);
                    }
                })
            })
            .collect();
        for h in hs {
            h.join().unwrap();
        }
        assert_eq!(ok.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn console_only_and_hotplug_diff() {
        let (b, c) = setup(DisplayBackendKind::ConsoleOnly, vec![target(5, 1, true, 0)]);
        let devs = b.enumerate().unwrap();
        assert_eq!(b.bind_seat(s(2), &[devs[0].key.clone()]).unwrap_err().kind, ErrorKind::Unsupported);
        assert_eq!(b.bind_seat(s(1), &[devs[0].key.clone()]).unwrap().outputs[0].presentation, Presentation::Native);
        let events = Arc::new(Mutex::new(Vec::new()));
        let ev = events.clone();
        *lock(&b.inner.sink) = Some(Arc::new(move |e| lock(&ev).push(e)));
        lock(&c.snap).targets.push(target(5, 2, true, 1920));
        b.refresh();
        let got = lock(&events).clone();
        assert_eq!(got.len(), 1);
        assert!(matches!(&got[0], HotplugEvent::Arrived { .. }));
    }

    #[test]
    fn renumbered_target_resolves_by_edid() {
        let (b, c) = setup(DisplayBackendKind::WindowPresenter, vec![target(5, 1, true, 1920)]);
        let old_key = b.enumerate().unwrap()[0].key.clone();
        lock(&c.snap).targets = vec![target(5, 99, true, 1920)];
        let out = b.bind_seat(s(2), &[old_key]).unwrap();
        assert_eq!(out.outputs[0].target_id, 99);
    }

    #[test]
    fn display_core_topology_specializes_journals_and_restores() {
        let dir = std::env::temp_dir().join(format!("pulsar-seat-binding-topo-{}", std::process::id()));
        let path = dir.join(crate::topology::JOURNAL_FILE);
        let (b, c) = setup(DisplayBackendKind::DisplayCore, vec![target(5, 1, true, 0), target(5, 2, true, 1920)]);
        b.set_journal_path_unchecked(path.clone());
        let devs = b.enumerate().unwrap();
        let (k1, k2) = (devs[0].key.clone(), devs[1].key.clone());
        let plan = TopologyPlan { console: vec![k1.clone()], seats: vec![(s(2), vec![k2.clone()])], attach: vec![] };
        let r = b.apply_topology(&plan).unwrap();
        assert!(r.changed && !r.rolled_back);
        assert_eq!(r.removed, vec![k2.clone()]);
        assert!(r.fallback.is_empty(), "specialized target is presented, not a fallback");
        assert_eq!(b.topology_journal().entries.len(), 1);
        // Journal survives a restart of the service.
        let (b2, _) = setup(DisplayBackendKind::DisplayCore, vec![]);
        b2.set_journal_path_unchecked(path.clone());
        assert_eq!(b2.topology_journal().entries.len(), 1);
        // Same plan again: nothing to do.
        let r = b.apply_topology(&plan).unwrap();
        assert!(!r.changed);
        // Everything back to seat 1 (disable): restored and journal empty.
        let r = b.apply_topology(&TopologyPlan { console: vec![k1.clone(), k2.clone()], seats: vec![], attach: vec![] }).unwrap();
        assert_eq!(r.restored, vec![k2.clone()]);
        assert!(r.missing.is_empty());
        assert!(b.topology_journal().entries.is_empty());
        assert!(crate::topology::TopologyJournal::load(&path).entries.is_empty());
        let n = lock(&c.cmds).iter().filter(|c| matches!(c, ConsoleCommand::ApplyTopology { .. })).count();
        assert_eq!(n, 2);
        // No specialization on this system: the seat target stays in the desktop.
        let mut t2 = target(5, 2, true, 1920);
        t2.specialization_available = None;
        let (b, c) = setup(DisplayBackendKind::DisplayCore, vec![target(5, 1, true, 0), t2]);
        let devs = b.enumerate().unwrap();
        let r = b.apply_topology(&TopologyPlan { console: vec![devs[0].key.clone()], seats: vec![(s(2), vec![devs[1].key.clone()])], attach: vec![] }).unwrap();
        assert!(!r.changed);
        assert_eq!(r.fallback, vec![devs[1].key.clone()]);
        assert!(lock(&c.cmds).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clone_targets_follow_their_surface() {
        let (b, _) = setup(DisplayBackendKind::WindowPresenter, vec![target(5, 1, true, 0), target(5, 2, true, 1920), target(5, 3, true, 3840)]);
        let devs = b.enumerate().unwrap();
        let prefs = SeatDisplayPrefs { outputs: vec![(devs[1].key.clone(), vec![2, 3])], ..Default::default() };
        b.configure_seat(s(2), &prefs);
        // Target 3 is also listed as a monitor of its own: it becomes a clone only.
        let out = b.bind_seat(s(2), &[devs[1].key.clone(), devs[2].key.clone()]).unwrap();
        assert_eq!(out.outputs.len(), 2);
        assert_eq!(out.outputs[0].target_id, 2);
        assert_eq!((out.outputs[1].target_id, out.outputs[1].clone_of), (3, Some(0)));
        assert_eq!(out.layout_outputs().count(), 1);
        assert!(!out.outputs[0].follow_mode, "desktop targets do not follow by default");
        // A clone target bound to another seat is busy.
        b.release_seat(s(2)).unwrap();
        b.bind_seat(s(3), &[devs[2].key.clone()]).unwrap();
        assert_eq!(b.bind_seat(s(2), &[devs[1].key.clone()]).unwrap_err().reason, "target_busy");
        assert!(b.caps().clone);
    }

    #[test]
    fn follow_session_mode_setting_reaches_desktop_targets() {
        use seat_core::config::Config;
        let (b, _) = setup(DisplayBackendKind::WindowPresenter, vec![target(5, 1, true, 0), target(5, 2, true, 1920)]);
        let devs = b.enumerate().unwrap();
        let mut cfg = Config::default();
        cfg.settings.outputs.follow_session_mode = Some(true);
        let prefs = SeatDisplayPrefs::from_config(&cfg, s(2));
        assert_eq!(prefs.follow_session_mode, Some(true));
        b.configure_seat(s(2), &prefs);
        let out = b.bind_seat(s(2), &[devs[1].key.clone()]).unwrap();
        assert_eq!(out.outputs[0].presentation, Presentation::Fallback);
        assert!(out.outputs[0].follow_mode, "allowed: the window presenter creates a mode follower for it");
        b.release_seat(s(2)).unwrap();
        cfg.settings.outputs.follow_session_mode = Some(false);
        b.configure_seat(s(2), &SeatDisplayPrefs::from_config(&cfg, s(2)));
        assert!(!b.bind_seat(s(2), &[devs[1].key.clone()]).unwrap().outputs[0].follow_mode);
    }
}
