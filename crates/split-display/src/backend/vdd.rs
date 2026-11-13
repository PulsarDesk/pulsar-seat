// SPDX-License-Identifier: GPL-3.0-only
//! Backend 1: Virtual-Display-Driver (VirtualDrivers, MIT, SignPath-signed; see
//! `docs/split/agents/third-party.md`) through its documented interface only:
//!
//! * `vdd_settings.xml` (path from `HKLM\SOFTWARE\MikeTheTech\VirtualDisplayDriver\VDDPATH`,
//!   default `C:\VirtualDisplayDriver`): this backend owns `monitors/count`,
//!   `gpu/friendlyname`, the `resolutions` list and the `colour` toggles, records the
//!   original file in the restore journal and restores it when the last seat detaches.
//! * pipe `\\.\pipe\MTTVirtualDisplayPipe`, one UTF-16 command per connection
//!   (`PING` -> `PONG`, `SETDISPLAYCOUNT n`, `RELOAD_DRIVER`, `IDDCXVERSION`, …).
//!
//! Limits (by design of the third-party driver):
//! * The monitor count is global; one monitor per attached seat. A seat binds to a concrete
//!   `(adapter LUID, target id)` the first time it is seen (in attach order = ascending
//!   target id, `HYPOTHESIS`: VDD creates monitors in index order and keeps target ids
//!   stable per index; verify on hardware) and is looked up by that id afterwards. When a
//!   seat detaches, the driver removes *its highest* monitor; a seat whose monitor
//!   vanished is moved to the freed one and `state()` reports `vdd_monitor_rebound`.
//! * VDD monitors belong to the console session, so only a seat whose session is the
//!   console session can attach (`session_attach_unsupported` otherwise; OQ 20).
//! * The pipe is Everyone-writable (OQ 19): `state()` re-checks the file and the monitor
//!   count on every call and restores our state on drift.
//! * Mode / HDR changes use the documented display-config APIs through [`ModeSetter`],
//!   which act on the **calling** session: the real implementation must run in the
//!   console session (the integrator runs it there; from session 0 the OS refuses).
//! * Desktop scale cannot be set through VDD; `scale_percent` is reported but not applied.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use crate::adapters::{AdapterChoice, AdapterEnumerator, Luid};
use crate::backend::vdd_xml::{self, VddDesired};
use crate::backend::{BackendInfo, BackendKind, DisplayBackend, SeatDisplayHandle, poll_until};
use crate::error::DisplayError;
use crate::hdr::{HdrCaps, hdr_availability, iddcx_for_build};
use crate::journal::{self, RestoreLog, Status};
use crate::modes::{ModeLimits, ModeRequest, pinned_luid, resolve_mode};
use crate::seat::{
    DisplayPhase, DisplayState, FrameFormat, ModeResult, ModeStatus, ReadyInfo, SeatDisplay,
};
use crate::{Clock, Sleeper};

pub const VDD_PIPE_NAME: &str = r"\\.\pipe\MTTVirtualDisplayPipe";
pub const VDD_DEFAULT_DIR: &str = r"C:\VirtualDisplayDriver";
pub const VDD_SETTINGS_FILE: &str = "vdd_settings.xml";
pub const VDD_REGISTRY_KEY: &str = r"SOFTWARE\MikeTheTech\VirtualDisplayDriver";
/// EDID manufacturer + product of VDD monitors ("MTT", 0x1337), as seen in the monitor
/// device path `\\?\DISPLAY#MTT1337#…`.
pub const VDD_MONITOR_ID: &str = "MTT1337";
/// Time the driver gets to recreate its monitors after a reload.
pub const RELOAD_TIMEOUT: Duration = Duration::from_secs(15);

/// One command per connection; returns the driver's reply text (may be empty).
pub trait VddPipe: Send + Sync {
    fn command(&self, cmd: &str) -> Result<String, DisplayError>;
}

pub trait VddSettingsFile: Send + Sync {
    fn read(&self) -> Result<String, DisplayError>;
    fn write(&self, xml: &str) -> Result<(), DisplayError>;
    /// Full path (journal target).
    fn path(&self) -> String {
        format!("{VDD_DEFAULT_DIR}\\{VDD_SETTINGS_FILE}")
    }
}

/// A VDD monitor as the display configuration reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VddMonitor {
    pub adapter_luid: Luid,
    pub target_id: u32,
    /// Part of an active path (has a desktop).
    pub active: bool,
    pub width: u32,
    pub height: u32,
    pub hz: u32,
    pub hdr_supported: bool,
    pub hdr_active: bool,
}

/// Lists the VDD monitors (`QueryDisplayConfig` filtered by [`VDD_MONITOR_ID`]).
pub trait VddMonitors: Send + Sync {
    fn monitors(&self) -> Result<Vec<VddMonitor>, DisplayError>;
}

/// Applies a mode / advanced colour to one monitor (runs in the console session).
pub trait ModeSetter: Send + Sync {
    fn set_mode(
        &self,
        m: &VddMonitor,
        width: u32,
        height: u32,
        hz: u32,
    ) -> Result<(), DisplayError>;
    fn set_advanced_color(&self, m: &VddMonitor, enable: bool) -> Result<(), DisplayError>;
}

pub trait ConsoleSession: Send + Sync {
    /// Session attached to the physical console, `None` if none.
    fn console_session_id(&self) -> Option<u32>;
}

/// Everything besides the pipe and the file.
pub struct VddEnv {
    pub monitors: Box<dyn VddMonitors>,
    pub modes: Box<dyn ModeSetter>,
    pub console: Box<dyn ConsoleSession>,
    pub adapters: Box<dyn AdapterEnumerator>,
    pub journal: Box<dyn RestoreLog>,
    pub clock: Arc<dyn Clock>,
    pub sleeper: Arc<dyn Sleeper>,
    pub os_build: u32,
    pub server: bool,
    pub limits: ModeLimits,
}

#[derive(Debug, Clone)]
struct VddSeat {
    cfg: SeatDisplay,
    mode: Option<ModeResult>,
    /// The VDD monitor this seat uses, `(adapter LUID, target id)`, bound the first time
    /// it is seen (wait_ready / request_mode / state). Lookups go by this id, never by
    /// position, so a concurrent detach of another seat cannot redirect a mode change.
    monitor: Option<(Luid, u32)>,
    /// The bound monitor disappeared (another seat detached and the driver removed a
    /// different monitor than ours) and the seat moved to a free one. Reported once by
    /// `state()` so the integrator re-sends the mode.
    rebound: bool,
}

fn mon_id(m: &VddMonitor) -> (Luid, u32) {
    (m.adapter_luid, m.target_id)
}

#[derive(Default)]
struct Inner {
    /// Attach order = slot order.
    seats: Vec<VddSeat>,
    desired: VddDesired,
    /// File content before our first write (also journaled).
    original: Option<String>,
    seq: u64,
    /// `seq` continued from the journal (after a service restart).
    seq_loaded: bool,
    iddcx_version: Option<u32>,
}

impl Inner {
    /// The monitor of the seat at `slot`, binding it if needed. `mons` in slot order.
    ///
    /// An unbound seat takes the k-th monitor not bound by another seat, k = number of
    /// unbound seats before it (so attach order still decides on first use). A bound
    /// monitor that is gone while the driver shows at least one monitor per seat (count
    /// settled) is replaced the same way and the seat is marked `rebound`.
    fn resolve(&mut self, slot: usize, mons: &[VddMonitor]) -> Option<VddMonitor> {
        let present = |id: (Luid, u32)| mons.iter().any(|m| mon_id(m) == id);
        if let Some(id) = self.seats[slot].monitor {
            if let Some(m) = mons.iter().find(|m| mon_id(m) == id) {
                return Some(*m);
            }
            if mons.len() < self.seats.len() {
                return None; // driver still reloading
            }
            log::warn!(
                "vdd: monitor {id:?} of seat {} is gone; rebinding",
                self.seats[slot].cfg.seat_id
            );
            let s = &mut self.seats[slot];
            s.monitor = None;
            s.mode = None;
            s.rebound = true;
        }
        let taken: Vec<(Luid, u32)> = self
            .seats
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != slot)
            .filter_map(|(_, s)| s.monitor)
            .filter(|id| present(*id))
            .collect();
        let k = self.seats[..slot]
            .iter()
            .filter(|s| s.monitor.is_none_or(|id| !present(id)))
            .count();
        let m = *mons.iter().filter(|m| !taken.contains(&mon_id(m))).nth(k)?;
        self.seats[slot].monitor = Some(mon_id(&m));
        Some(m)
    }
}

pub struct VddBackend<P: VddPipe, F: VddSettingsFile> {
    pipe: P,
    file: F,
    env: VddEnv,
    inner: Mutex<Inner>,
}

/// Parses an IddCx version from a VDD reply (`0x1A80`, `1A80`, or `1.10`).
pub fn parse_iddcx_version(reply: &str) -> Option<u32> {
    for tok in reply.split(|c: char| {
        c.is_whitespace() || c == ':' || c == ',' || c == '=' || c == '(' || c == ')'
    }) {
        let t = tok.trim();
        if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
            if let Ok(v) = u32::from_str_radix(h, 16)
                && (0x1000..0x2000).contains(&v)
            {
                return Some(v);
            }
        } else if let Some((maj, min)) = t.split_once('.')
            && maj == "1"
            && let Ok(m) = min.parse::<u32>()
            && m < 16
        {
            return Some(0x1000 + (m << 8));
        }
    }
    None
}

impl<P: VddPipe, F: VddSettingsFile> VddBackend<P, F> {
    pub fn new(pipe: P, file: F, env: VddEnv) -> Self {
        VddBackend {
            pipe,
            file,
            env,
            inner: Mutex::new(Inner::default()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn caps(&self, inner: &Inner) -> HdrCaps {
        HdrCaps {
            os_build: self.env.os_build,
            iddcx_version: inner
                .iddcx_version
                .or_else(|| iddcx_for_build(self.env.os_build)),
            // VDD declares FP16 surfaces (IDDCX_ADAPTER_FLAGS_CAN_PROCESS_FP16, upstream source).
            driver_fp16: true,
            gpu_ok: true,
            driver_installed: true,
            server: self.env.server,
        }
    }

    /// Description of the first pinned seat GPU (VDD renders all monitors on one GPU).
    fn desired_gpu(&self, seats: &[VddSeat]) -> Option<String> {
        let luid = seats.iter().find_map(|s| pinned_luid(&s.cfg.adapter))?;
        let list = self.env.adapters.adapters().ok()?;
        let d = list
            .into_iter()
            .find(|a| a.luid == luid)
            .map(|a| a.description);
        if seats
            .iter()
            .filter_map(|s| pinned_luid(&s.cfg.adapter))
            .any(|l| l != luid)
        {
            log::warn!(
                "vdd: seats pin different GPUs; the driver renders all monitors on the first seat's GPU"
            );
        }
        d
    }

    /// Writes the desired state to the file (journaled). Returns whether it changed.
    fn write_desired(&self, inner: &mut Inner) -> Result<bool, DisplayError> {
        let current = self.file.read()?;
        if inner.original.is_none() {
            inner.original = Some(current.clone());
        }
        let next = vdd_xml::apply(&current, &inner.desired)?;
        if next == current {
            return Ok(false);
        }
        let target = self.file.path();
        self.load_seq(inner, None)?;
        inner.seq += 1;
        let now = self.env.clock.now_ms();
        self.env.journal.append(&journal::file_record(
            inner.seq,
            now,
            &target,
            Some(current.as_bytes()),
            Some(next.as_bytes()),
            Status::Intent,
        ))?;
        self.file.write(&next)?;
        self.env.journal.append(&journal::file_record(
            inner.seq,
            now,
            &target,
            Some(current.as_bytes()),
            Some(next.as_bytes()),
            Status::Done,
        ))?;
        Ok(true)
    }

    /// Continues `seq` from the journal once per process.
    fn load_seq(&self, inner: &mut Inner, lines: Option<&[String]>) -> Result<(), DisplayError> {
        if !inner.seq_loaded {
            let last = match lines {
                Some(l) => journal::last_seq(l),
                None => journal::last_seq(&self.env.journal.lines()?),
            };
            inner.seq = inner.seq.max(last);
            inner.seq_loaded = true;
        }
        Ok(())
    }

    /// File + driver to the desired state. `count_changed` -> SETDISPLAYCOUNT (reloads);
    /// otherwise a changed file -> RELOAD_DRIVER.
    fn sync(&self, inner: &mut Inner, count_changed: bool) -> Result<bool, DisplayError> {
        let changed = self.write_desired(inner)?;
        if count_changed {
            self.pipe
                .command(&format!("SETDISPLAYCOUNT {}", inner.desired.count))?;
            Ok(true)
        } else if changed {
            self.pipe.command("RELOAD_DRIVER")?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Current VDD monitors sorted by target id (slot order).
    fn slots(&self) -> Result<Vec<VddMonitor>, DisplayError> {
        let mut m = self.env.monitors.monitors()?;
        m.sort_by_key(|m| (m.adapter_luid.to_u64(), m.target_id));
        Ok(m)
    }

    fn slot_of(inner: &Inner, seat_id: &str) -> Result<usize, DisplayError> {
        inner
            .seats
            .iter()
            .position(|s| s.cfg.seat_id == seat_id)
            .ok_or_else(|| DisplayError::NoDisplay(format!("seat {seat_id} is not attached")))
    }

    fn wait_monitors(
        &self,
        count: usize,
        timeout: Duration,
    ) -> Result<Option<Vec<VddMonitor>>, DisplayError> {
        poll_until(&*self.env.clock, &*self.env.sleeper, timeout, || {
            let m = self.slots()?;
            Ok((m.len() >= count && m.iter().take(count).all(|m| m.active)).then_some(m))
        })
    }

    fn result_from_monitor(
        &self,
        req_id: u32,
        status: ModeStatus,
        m: &VddMonitor,
        adapter: &AdapterChoice,
    ) -> ModeResult {
        ModeResult {
            request_id: req_id,
            status,
            width: m.width,
            height: m.height,
            hz: m.hz,
            hdr: m.hdr_active,
            render_width: m.width,
            render_height: m.height,
            format: FrameFormat::for_hdr(m.hdr_active),
            adapter_luid: pinned_luid(adapter).or(Some(m.adapter_luid)),
        }
    }

    /// Re-applies our state when someone changed the file or the monitor count through
    /// the Everyone-writable pipe (OQ 19). Returns true if it had to repair.
    pub fn reconcile(&self) -> Result<bool, DisplayError> {
        let mut inner = self.lock();
        if inner.seats.is_empty() {
            return Ok(false);
        }
        let xml = self.file.read()?;
        let file_ok = vdd_xml::matches(&xml, &inner.desired);
        let count_ok = self.slots()?.len() == inner.desired.count as usize;
        if file_ok && count_ok {
            return Ok(false);
        }
        log::warn!("vdd: settings drifted (file ok: {file_ok}, count ok: {count_ok}); restoring");
        self.sync(&mut inner, !count_ok)?;
        Ok(true)
    }

    /// Restores the settings file to its content before this epoch's first change and
    /// reloads the driver. The content comes from the journal (records after the last
    /// `restored` marker, so it also covers a crashed earlier run), else from the copy
    /// taken at our first write. The restore write is journaled itself and followed by a
    /// `restored` marker, so the next session starts a new epoch.
    pub fn restore_all(&self) -> Result<(), DisplayError> {
        let mut inner = self.lock();
        let target = self.file.path();
        let lines = self.env.journal.lines()?;
        self.load_seq(&mut inner, Some(&lines))?;
        let planned = journal::plan_restore(&lines)
            .into_iter()
            .find(|r| r.target.eq_ignore_ascii_case(&target));
        let touched = planned.is_some() || inner.original.is_some();
        let original: Option<Vec<u8>> = match planned {
            Some(r) => {
                if r.previous.is_none() {
                    log::warn!(
                        "vdd: settings file was absent before our first change; left in place"
                    );
                }
                r.previous
            }
            None => inner.original.clone().map(String::into_bytes),
        };
        if let Some(prev) = original {
            let text = String::from_utf8(prev)
                .map_err(|_| DisplayError::Protocol("journal: settings not UTF-8".into()))?;
            let current = match self.file.read() {
                // The settings folder itself is gone (driver removed, `VDDPATH` deleted):
                // nothing is left to restore. Close the epoch below so the next run is a
                // no-op instead of failing and appending another Intent each time.
                Err(DisplayError::DriverNotInstalled(m)) => {
                    log::warn!("vdd: settings folder gone, nothing to restore: {m}");
                    None
                }
                r => Some(r.ok()),
            };
            if let Some(current) = current.filter(|c| c.as_deref() != Some(text.as_str())) {
                inner.seq += 1;
                let now = self.env.clock.now_ms();
                let cur = current.as_deref().map(str::as_bytes);
                self.env.journal.append(&journal::file_record(
                    inner.seq,
                    now,
                    &target,
                    cur,
                    Some(text.as_bytes()),
                    Status::Intent,
                ))?;
                self.file.write(&text)?;
                self.env.journal.append(&journal::file_record(
                    inner.seq,
                    now,
                    &target,
                    cur,
                    Some(text.as_bytes()),
                    Status::Done,
                ))?;
                match self.pipe.command("RELOAD_DRIVER") {
                    Ok(_) => {}
                    // Uninstall / reconcile after the driver is gone: the file is back,
                    // the driver reads it at its next load.
                    Err(DisplayError::DriverNotInstalled(m)) => {
                        log::warn!("vdd: settings restored, driver not reachable for reload: {m}");
                    }
                    Err(e) => return Err(e),
                }
            }
        }
        if touched {
            inner.seq += 1;
            let now = self.env.clock.now_ms();
            self.env
                .journal
                .append(&journal::restored_marker(inner.seq, now, &target))?;
        }
        inner.original = None;
        inner.desired = VddDesired::default();
        inner.seats.clear();
        Ok(())
    }
}

impl<P: VddPipe, F: VddSettingsFile> DisplayBackend for VddBackend<P, F> {
    fn kind(&self) -> BackendKind {
        BackendKind::Vdd
    }

    fn probe(&self) -> Result<BackendInfo, DisplayError> {
        let installed = match self.pipe.command("PING") {
            Ok(reply) => {
                if !reply.contains("PONG") {
                    log::debug!("vdd: PING without PONG (logging off?)");
                }
                true
            }
            Err(DisplayError::DriverNotInstalled(_)) => false,
            Err(e) => return Err(e),
        };
        let mut inner = self.lock();
        if installed && inner.iddcx_version.is_none() {
            // Only answered when the driver's pipe logging is on; else estimated from the build.
            inner.iddcx_version = self
                .pipe
                .command("IDDCXVERSION")
                .ok()
                .and_then(|r| parse_iddcx_version(&r));
        }
        let mut caps = self.caps(&inner);
        caps.driver_installed = installed;
        Ok(BackendInfo {
            kind: BackendKind::Vdd,
            installed,
            driver_version: None,
            iddcx_version: caps.iddcx_version.filter(|_| installed),
            hdr: hdr_availability(&caps, BackendKind::Vdd, false),
            remote_session_capable: false,
            // SignPath Foundation Authenticode per upstream (third-party.md); not verified here.
            signed: installed,
        })
    }

    fn attach(&self, cfg: &SeatDisplay) -> Result<SeatDisplayHandle, DisplayError> {
        match self.env.console.console_session_id() {
            Some(c) if c == cfg.session_id => {}
            other => {
                return Err(DisplayError::SessionAttachUnsupported(format!(
                    "Virtual-Display-Driver monitors belong to the console session ({other:?}); seat session {} is not it",
                    cfg.session_id
                )));
            }
        }
        let mut inner = self.lock();
        let handle = SeatDisplayHandle {
            seat_id: cfg.seat_id.clone(),
            session_id: cfg.session_id,
            kind: BackendKind::Vdd,
        };
        if let Some(s) = inner
            .seats
            .iter_mut()
            .find(|s| s.cfg.seat_id == cfg.seat_id)
        {
            s.cfg = cfg.clone();
            return Ok(handle);
        }
        inner.seats.push(VddSeat {
            cfg: cfg.clone(),
            mode: None,
            monitor: None,
            rebound: false,
        });
        let count = inner.seats.len() as u32;
        let gpu = self.desired_gpu(&inner.seats);
        inner.desired.count = count;
        if gpu.is_some() {
            inner.desired.gpu = gpu;
        }
        if let Some((w, h, hz)) = cfg.last_mode
            && !inner.desired.resolutions.contains(&(w, h, hz))
        {
            inner.desired.resolutions.push((w, h, hz));
        }
        if let Err(e) = self.sync(&mut inner, true) {
            inner.seats.retain(|s| s.cfg.seat_id != cfg.seat_id);
            inner.desired.count = inner.seats.len() as u32;
            return Err(e);
        }
        Ok(handle)
    }

    fn wait_ready(
        &self,
        h: &SeatDisplayHandle,
        timeout: Duration,
    ) -> Result<ReadyInfo, DisplayError> {
        let found = poll_until(&*self.env.clock, &*self.env.sleeper, timeout, || {
            let mons = self.slots()?;
            let mut inner = self.lock();
            let slot = Self::slot_of(&inner, &h.seat_id)?;
            let adapter = inner.seats[slot].cfg.adapter.clone();
            Ok(inner
                .resolve(slot, &mons)
                .filter(|m| m.active)
                .map(|m| (m, adapter)))
        })?;
        match found {
            Some((m, adapter)) => Ok(ReadyInfo {
                phase: DisplayPhase::PathCommitted,
                mode: Some(self.result_from_monitor(0, ModeStatus::Ok, &m, &adapter)),
            }),
            None => Err(DisplayError::Timeout(format!(
                "VDD monitor of seat {} did not become active within {timeout:?}",
                h.seat_id
            ))),
        }
    }

    fn request_mode(
        &self,
        h: &SeatDisplayHandle,
        req: &ModeRequest,
    ) -> Result<ModeResult, DisplayError> {
        let mut inner = self.lock();
        let slot = Self::slot_of(&inner, &h.seat_id)?;
        let cfg = inner.seats[slot].cfg.clone();
        let caps = self.caps(&inner);
        let r = resolve_mode(req, &cfg, &caps, &self.env.limits);

        // The render size must be one of the driver's offered modes.
        let wanted = (r.render_width, r.render_height, r.hz);
        if !inner.desired.resolutions.contains(&wanted) {
            inner.desired.resolutions.push(wanted);
        }
        let reloaded = self.sync(&mut inner, false)?;
        let count = inner.seats.len();
        drop(inner);
        let mons = if reloaded {
            self.wait_monitors(count, RELOAD_TIMEOUT)?
        } else {
            Some(self.slots()?)
        };
        let Some(mons) = mons else {
            return Ok(ModeResult::empty(req.request_id, ModeStatus::NoDisplay));
        };
        // Re-lock and find this seat's own monitor by id; the lock is held from here to
        // the stored result, so a concurrent detach cannot move the seat in between.
        let mut inner = self.lock();
        let Ok(slot) = Self::slot_of(&inner, &h.seat_id) else {
            return Ok(ModeResult::empty(req.request_id, ModeStatus::NoDisplay));
        };
        let Some(mon) = inner.resolve(slot, &mons) else {
            return Ok(ModeResult::empty(req.request_id, ModeStatus::NoDisplay));
        };

        if let Err(e) = self
            .env
            .modes
            .set_mode(&mon, r.render_width, r.render_height, r.hz)
        {
            log::warn!(
                "vdd: mode {}x{}@{} rejected: {e}",
                r.render_width,
                r.render_height,
                r.hz
            );
            return Ok(ModeResult {
                status: ModeStatus::Rejected,
                ..self.result_from_monitor(req.request_id, ModeStatus::Rejected, &mon, &cfg.adapter)
            });
        }
        let mut hdr = r.hdr && mon.hdr_supported;
        if mon.hdr_supported
            && let Err(e) = self.env.modes.set_advanced_color(&mon, r.hdr)
        {
            log::warn!("vdd: advanced colour {} failed: {e}", r.hdr);
            hdr = false;
        }
        let mut res = r.to_result(req, &cfg.adapter);
        res.hdr = hdr;
        res.format = FrameFormat::for_hdr(hdr);
        if res.adapter_luid.is_none() {
            res.adapter_luid = Some(mon.adapter_luid);
        }
        res.status = if r.clamped {
            ModeStatus::Clamped
        } else if req.hdr && !hdr {
            ModeStatus::OkSdrFallback
        } else {
            ModeStatus::Ok
        };
        inner.seats[slot].mode = Some(res.clone());
        inner.seats[slot].cfg.last_mode = Some((r.width, r.height, r.hz));
        Ok(res)
    }

    fn state(&self, h: &SeatDisplayHandle) -> Result<DisplayState, DisplayError> {
        let repaired = self.reconcile()?;
        let mons = self.slots()?;
        let mut inner = self.lock();
        let slot = Self::slot_of(&inner, &h.seat_id)?;
        let m = inner.resolve(slot, &mons);
        let rebound = std::mem::take(&mut inner.seats[slot].rebound);
        let mode = inner.seats[slot].mode.clone();
        drop(inner);
        let mut notes = Vec::new();
        if repaired {
            notes.push("vdd_settings_drift: restored");
        }
        if rebound {
            notes
                .push("vdd_monitor_rebound: seat moved to another monitor; request the mode again");
        }
        Ok(DisplayState {
            ready: m.is_some_and(|m| m.active),
            monitor_present: m.is_some(),
            path_active: m.is_some_and(|m| m.active),
            mode,
            hdr_active: m.is_some_and(|m| m.hdr_active),
            frame_number: 0, // the stream server captures; no frame counter here
            last_error: (!notes.is_empty()).then(|| notes.join("; ")),
        })
    }

    fn detach(&self, h: &SeatDisplayHandle) -> Result<(), DisplayError> {
        let mut inner = self.lock();
        let before = inner.seats.len();
        inner.seats.retain(|s| s.cfg.seat_id != h.seat_id);
        if inner.seats.len() == before {
            return Ok(());
        }
        if inner.seats.is_empty() {
            drop(inner);
            // Last seat: give the user's original configuration back.
            return self.restore_all();
        }
        inner.desired.count = inner.seats.len() as u32;
        let gpu = self.desired_gpu(&inner.seats);
        if gpu.is_some() {
            inner.desired.gpu = gpu;
        }
        self.sync(&mut inner, true)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::RenderAdapter;
    use crate::fake::*;
    use seat_proto::remote::RemoteSeatRecord;

    struct Rig {
        b: VddBackend<FakeVddPipe, FakeVddSettings>,
        pipe: FakeVddPipe,
        file: FakeVddSettings,
        mons: FakeVddMonitors,
        modes: FakeModeSetter,
        journal: FakeRestoreLog,
    }

    fn rig(console: u32) -> Rig {
        let pipe = FakeVddPipe::default();
        let file = FakeVddSettings::new(vdd_xml::SAMPLE);
        let mons = FakeVddMonitors::default();
        // the fake driver creates `count` monitors when told to
        pipe.link_monitors(mons.clone());
        let modes = FakeModeSetter::linked(mons.clone());
        let journal = FakeRestoreLog::default();
        rig_on(console, pipe, file, mons, modes, journal)
    }

    /// A backend over existing fakes (a second process reading the same file + journal).
    fn rig_on(
        console: u32,
        pipe: FakeVddPipe,
        file: FakeVddSettings,
        mons: FakeVddMonitors,
        modes: FakeModeSetter,
        journal: FakeRestoreLog,
    ) -> Rig {
        let clock = Arc::new(FakeClock::default());
        let gpu = RenderAdapter {
            pnp_instance_id: "PCI\\X".into(),
            description: "GPU X".into(),
            luid: Luid { low: 42, high: 0 },
            dedicated_video_memory: 1,
            hardware: true,
            vendor_id: 0,
            device_id: 0,
        };
        let env = VddEnv {
            monitors: Box::new(mons.clone()),
            modes: Box::new(modes.clone()),
            console: Box::new(FakeConsole(Some(console))),
            adapters: Box::new(FakeAdapters(vec![gpu])),
            journal: Box::new(journal.clone()),
            clock: clock.clone(),
            sleeper: clock,
            os_build: 26100,
            server: false,
            limits: ModeLimits::default(),
        };
        Rig {
            b: VddBackend::new(pipe.clone(), file.clone(), env),
            pipe,
            file,
            mons,
            modes,
            journal,
        }
    }

    fn seat(id: &str, session: u32, adapter: AdapterChoice) -> SeatDisplay {
        SeatDisplay::from_seat(
            &RemoteSeatRecord {
                id: id.into(),
                ..Default::default()
            },
            session,
            adapter,
        )
    }

    const A: &str = "aaaaaaaa-0000-4000-8000-000000000001";
    const B: &str = "bbbbbbbb-0000-4000-8000-000000000002";

    #[test]
    fn probe_reports_install_and_hdr() {
        let r = rig(1);
        let i = r.b.probe().unwrap();
        assert!(i.installed && !i.remote_session_capable);
        assert_eq!(i.iddcx_version, Some(0x1A80));
        assert_eq!(i.hdr, Ok(()));
        r.pipe.set_absent(true);
        let i = r.b.probe().unwrap();
        assert!(!i.installed);
        assert_eq!(i.hdr, Err(crate::hdr::HdrUnavailable::DriverNotInstalled));
    }

    #[test]
    fn remote_session_is_refused() {
        let r = rig(1);
        let e =
            r.b.attach(&seat(A, 3, AdapterChoice::Default { warning: None }))
                .unwrap_err();
        assert_eq!(e.code(), "session_attach_unsupported");
        assert!(r.pipe.commands().is_empty());
    }

    #[test]
    fn attach_ready_mode_detach_restores() {
        let r = rig(1);
        let h =
            r.b.attach(&seat(
                A,
                1,
                AdapterChoice::Pinned(Luid { low: 42, high: 0 }),
            ))
            .unwrap();
        assert_eq!(r.pipe.commands(), vec!["SETDISPLAYCOUNT 1"]);
        let xml = r.file.content();
        assert_eq!(
            vdd_xml::get(&xml, &["vdd_settings", "gpu", "friendlyname"]).as_deref(),
            Some("GPU X")
        );
        assert_eq!(r.journal.lines().unwrap().len(), 2); // intent + done

        let ready = r.b.wait_ready(&h, Duration::from_secs(5)).unwrap();
        assert_eq!(ready.phase, DisplayPhase::PathCommitted);

        let m =
            r.b.request_mode(
                &h,
                &ModeRequest {
                    request_id: 9,
                    width: 2560,
                    height: 1440,
                    hz: 144,
                    hdr: true,
                },
            )
            .unwrap();
        assert_eq!(m.status, ModeStatus::Ok);
        assert!(m.hdr);
        assert_eq!((m.render_width, m.render_height, m.hz), (2560, 1440, 144));
        assert_eq!(m.adapter_luid, Some(Luid { low: 42, high: 0 }));
        assert!(vdd_xml::resolutions(&r.file.content()).contains(&(2560, 1440, 144)));
        assert_eq!(r.pipe.commands().last().unwrap(), "RELOAD_DRIVER");
        assert_eq!(
            r.modes.calls(),
            vec!["mode 0 2560x1440@144".to_string(), "hdr 0 true".to_string()]
        );

        let st = r.b.state(&h).unwrap();
        assert!(st.ready && st.hdr_active);
        assert!(st.last_error.is_none());

        r.b.detach(&h).unwrap();
        assert_eq!(r.file.content(), vdd_xml::SAMPLE);
        assert_eq!(r.pipe.commands().last().unwrap(), "RELOAD_DRIVER");
    }

    #[test]
    fn two_seats_share_the_count_and_drift_is_repaired() {
        let r = rig(1);
        let ha =
            r.b.attach(&seat(A, 1, AdapterChoice::Default { warning: None }))
                .unwrap();
        // second console seat (same console session id; e.g. a second monitor of the console user)
        let hb =
            r.b.attach(&seat(B, 1, AdapterChoice::Default { warning: None }))
                .unwrap();
        assert_eq!(
            r.pipe.commands(),
            vec!["SETDISPLAYCOUNT 1", "SETDISPLAYCOUNT 2"]
        );
        r.b.wait_ready(&hb, Duration::from_secs(5)).unwrap();
        assert_eq!(r.mons.count(), 2);

        // somebody uses the open pipe: count -> 5 and the file is changed
        r.mons.set_count(5);
        r.file.set_content(
            &vdd_xml::set(
                &r.file.content(),
                &["vdd_settings", "monitors", "count"],
                "5",
            )
            .unwrap(),
        );
        let st = r.b.state(&ha).unwrap();
        assert_eq!(
            st.last_error.as_deref(),
            Some("vdd_settings_drift: restored")
        );
        assert_eq!(r.mons.count(), 2);
        assert_eq!(
            vdd_xml::get(&r.file.content(), &["vdd_settings", "monitors", "count"]).as_deref(),
            Some("2")
        );

        r.b.detach(&ha).unwrap();
        assert_eq!(r.pipe.commands().last().unwrap(), "SETDISPLAYCOUNT 1");
        assert_eq!(r.b.state(&ha).unwrap_err().code(), "no_display");
        r.b.detach(&hb).unwrap();
        assert_eq!(r.file.content(), vdd_xml::SAMPLE);
    }

    #[test]
    fn rejected_mode_and_force_sdr() {
        let r = rig(1);
        let mut s = seat(A, 1, AdapterChoice::Default { warning: None });
        s.hdr_allowed = false;
        let h = r.b.attach(&s).unwrap();
        r.b.wait_ready(&h, Duration::from_secs(5)).unwrap();
        let m =
            r.b.request_mode(
                &h,
                &ModeRequest {
                    request_id: 1,
                    width: 1920,
                    height: 1080,
                    hz: 60,
                    hdr: true,
                },
            )
            .unwrap();
        assert_eq!(m.status, ModeStatus::OkSdrFallback);
        assert!(!m.hdr);
        r.modes.fail_modes(true);
        let m =
            r.b.request_mode(
                &h,
                &ModeRequest {
                    request_id: 2,
                    width: 1920,
                    height: 1080,
                    hz: 60,
                    hdr: false,
                },
            )
            .unwrap();
        assert_eq!(m.status, ModeStatus::Rejected);
    }

    #[test]
    fn second_session_restores_admin_changes_not_first_run() {
        let r = rig(1);
        let h =
            r.b.attach(&seat(
                A,
                1,
                AdapterChoice::Pinned(Luid { low: 42, high: 0 }),
            ))
            .unwrap();
        r.b.detach(&h).unwrap();
        assert_eq!(r.file.content(), vdd_xml::SAMPLE);
        // the admin edits the file between two sessions
        let edited = vdd_xml::set(
            vdd_xml::SAMPLE,
            &["vdd_settings", "gpu", "friendlyname"],
            "Admin GPU",
        )
        .unwrap();
        r.file.set_content(&edited);
        let h =
            r.b.attach(&seat(
                A,
                1,
                AdapterChoice::Pinned(Luid { low: 42, high: 0 }),
            ))
            .unwrap();
        assert_ne!(r.file.content(), edited);
        r.b.detach(&h).unwrap();
        assert_eq!(r.file.content(), edited);
        // the journal ends with a marker; seq keeps counting
        let lines = r.journal.lines().unwrap();
        assert!(lines.last().unwrap().contains("\"restored\""));
        assert!(journal::plan_restore(&lines).is_empty());
    }

    #[test]
    fn crashed_run_is_restored_by_a_fresh_process() {
        let r = rig(1);
        let a = AdapterChoice::Pinned(Luid { low: 42, high: 0 });
        r.b.attach(&seat(A, 1, a)).unwrap();
        assert_ne!(r.file.content(), vdd_xml::SAMPLE);
        // the service dies here; setup (uninstall / reconcile) builds its own backend
        let s = rig_on(1, r.pipe.clone(), r.file.clone(), r.mons.clone(), r.modes.clone(), r.journal.clone());
        s.b.restore_all().unwrap();
        assert_eq!(s.file.content(), vdd_xml::SAMPLE);
        assert_eq!(
            vdd_xml::get(&s.file.content(), &["vdd_settings", "monitors", "count"]),
            vdd_xml::get(vdd_xml::SAMPLE, &["vdd_settings", "monitors", "count"])
        );
        assert_eq!(s.pipe.commands().last().map(String::as_str), Some("RELOAD_DRIVER"));
        // a second run has nothing left to do and writes nothing
        let n = s.journal.lines().unwrap().len();
        let t = rig_on(1, s.pipe.clone(), s.file.clone(), s.mons.clone(), s.modes.clone(), s.journal.clone());
        t.b.restore_all().unwrap();
        assert_eq!(t.journal.lines().unwrap().len(), n);
    }

    #[test]
    fn restore_without_driver_still_restores_the_file() {
        let r = rig(1);
        r.b.attach(&seat(A, 1, AdapterChoice::Pinned(Luid { low: 42, high: 0 }))).unwrap();
        r.pipe.set_absent(true);
        let s = rig_on(1, r.pipe.clone(), r.file.clone(), r.mons.clone(), r.modes.clone(), r.journal.clone());
        s.b.restore_all().unwrap();
        assert_eq!(s.file.content(), vdd_xml::SAMPLE);
        assert!(journal::plan_restore(&s.journal.lines().unwrap()).is_empty());
    }

    #[test]
    fn restore_with_settings_folder_gone_closes_the_epoch() {
        let r = rig(1);
        r.b.attach(&seat(A, 1, AdapterChoice::Pinned(Luid { low: 42, high: 0 }))).unwrap();
        r.pipe.set_absent(true);
        r.file.set_folder_absent(true);
        let s = rig_on(1, r.pipe.clone(), r.file.clone(), r.mons.clone(), r.modes.clone(), r.journal.clone());
        s.b.restore_all().unwrap();
        let lines = s.journal.lines().unwrap();
        assert!(journal::plan_restore(&lines).is_empty());
        // a second run (next reconcile / uninstall retry) appends nothing
        let t = rig_on(1, r.pipe.clone(), r.file.clone(), r.mons.clone(), r.modes.clone(), r.journal.clone());
        t.b.restore_all().unwrap();
        assert_eq!(t.journal.lines().unwrap().len(), lines.len());
    }

    #[test]
    fn restore_with_empty_journal_touches_nothing() {
        let r = rig(1);
        r.b.restore_all().unwrap();
        assert!(r.journal.lines().unwrap().is_empty());
        assert!(r.pipe.commands().is_empty());
        assert_eq!(r.file.content(), vdd_xml::SAMPLE);
    }

    fn last_mode_call(r: &Rig) -> String {
        r.modes
            .calls()
            .into_iter()
            .rev()
            .find(|c| c.starts_with("mode"))
            .unwrap()
    }

    #[test]
    fn middle_detach_keeps_ids_and_reports_rebind() {
        let r = rig(1);
        let c = "cccccccc-0000-4000-8000-000000000003";
        let hs: Vec<_> = [A, B, c]
            .iter()
            .map(|id| {
                r.b.attach(&seat(id, 1, AdapterChoice::Default { warning: None }))
                    .unwrap()
            })
            .collect();
        for h in &hs {
            r.b.wait_ready(h, Duration::from_secs(5)).unwrap();
        }
        let req = |id| ModeRequest {
            request_id: id,
            width: 1280,
            height: 720,
            hz: 60,
            hdr: false,
        };
        r.b.request_mode(&hs[2], &req(1)).unwrap();
        assert_eq!(last_mode_call(&r), "mode 2 1280x720@60");
        // B leaves: the driver drops target 2 (highest); C must move to target 1
        r.b.detach(&hs[1]).unwrap();
        assert_eq!(r.mons.count(), 2);
        let st = r.b.state(&hs[2]).unwrap();
        assert!(st.last_error.unwrap().contains("vdd_monitor_rebound"));
        assert!(st.mode.is_none());
        assert!(r.b.state(&hs[2]).unwrap().last_error.is_none()); // reported once
        r.b.request_mode(&hs[2], &req(2)).unwrap();
        assert_eq!(last_mode_call(&r), "mode 1 1280x720@60");
        // A keeps target 0 and was never rebound
        assert!(r.b.state(&hs[0]).unwrap().last_error.is_none());
        r.b.request_mode(&hs[0], &req(3)).unwrap();
        assert_eq!(last_mode_call(&r), "mode 0 1280x720@60");
    }

    #[test]
    fn iddcx_reply_parsing() {
        assert_eq!(parse_iddcx_version("IddCx Version: 0x1A80"), Some(0x1A80));
        assert_eq!(parse_iddcx_version("version 1.10"), Some(0x1A00));
        assert_eq!(parse_iddcx_version("nothing"), None);
        assert_eq!(parse_iddcx_version("0xFFFF"), None);
    }
}
