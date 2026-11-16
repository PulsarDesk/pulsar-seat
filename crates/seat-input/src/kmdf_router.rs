// SPDX-License-Identifier: GPL-3.0-only
//! `kmdf_filter`: client of our KMDF filter (`drivers/seat-input-filter`, spec 50 §4.1.2,
//! §4.1.3, §4.1.5). **Never loaded until signed**: on every PC today the control device is
//! missing and [`InputRouter::availability`] reports `driver_not_signed`. The code path is
//! complete and tested against an in-memory filter; it is **untested on hardware**.
//!
//! Connect sequence: open the control device → version (major must equal
//! `DRIVER_INTERFACE_MAJOR`, else the filter stays fail-open and we report
//! `driver_version_mismatch`) → attach (ends fail-open for this boot) → config → seat
//! sessions → unit masks → reader thread (inverted call `READ_PACKETS`).
//!
//! Reader: packets of extra-seat units go through a per-unit [`UnitProcessor`] (hotkey,
//! emergency, SAS, key-up synthesis after a `GAP`) and then to the [`PacketSink`] for that
//! seat; console copies and wizard captures only produce activity. Seat-1 `HOTKEY` packets
//! carry the Shift state: without Shift they are the move hotkey, with Shift they count
//! towards the emergency combination (spec 50 §4.1.9).
//!
//! HID collections (spec 50 §4.1.3 rule 2, §4.1.10): the filter denies the console's opens
//! of extra-seat touch screens, pens, touchpads, consumer and system-control collections,
//! so the service reads them itself from session 0 (one [`HidReader`] thread per unit,
//! [`service_reads`]) and forwards them like keyboard packets. A collection another
//! program holds exclusively is reported `not_routable` with the note
//! `hid_hidden_by_other_software` (spec 50 §4.1.6).
//!
//! Lifecycle: [`KmdfFilterRouter::disconnect`] sets a stop flag and repeats the (sticky)
//! cancel until the reader has stopped; a reader that gives up (device gone, repeated
//! failures) marks the connection dead, so [`InputRouter::availability`] reports
//! `filter_reader_stopped` and the next `apply_routes` reconnects.

use crate::convert::{
    EMERGENCY_PRESSES, EMERGENCY_WINDOW_MS, Hotkey, Outcome, UnitOptions, UnitProcessor, button_flags, split,
};
use crate::filter_proto::{FilterConfig, FilterPacket, INTERFACE_MAJOR, hotkey_bits, packet_flags, packet_kind};
use crate::inventory::{Inventory, lock};
use crate::platform::{FilterControl, FilterOpener, HidReader, HidSource, is_fatal_win32};
use crate::routes::{RouteTable, UnitRoute};
use crate::units::input_info;
use crate::{EmergencySink, InputControl};
use seat_core::backend::input::{
    ActivityEvent, ActivitySink, HotkeyEvent, HotkeySink, InputBackendKind, InputCaps, InputPacket, InputRoute, InputRouter,
    PacketSink, RouteReport,
};
use seat_core::backend::{Availability, BackendError, BackendResult, DriverId, ErrorKind, HotplugSink};
use seat_core::config::InputSettings;
use seat_core::device::{Device, DeviceKind};
use seat_core::ids::{DeviceKey, SeatMask, SeatNumber};
use seat_core::{Clock, SystemClock};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Minimum spacing of activity events per unit.
pub const ACTIVITY_THROTTLE_MS: u64 = 100;
/// How long [`KmdfFilterRouter::disconnect`] waits for a reader thread.
pub const STOP_TIMEOUT: Duration = Duration::from_secs(5);
/// Consecutive read failures after which a reader gives up.
pub const MAX_READ_FAILURES: u32 = 20;

/// HID collections the service reads itself for extra seats: touch screen, pen, precision
/// touchpad, consumer control, system control.
pub fn service_reads(page: u16, usage: u16) -> bool {
    matches!((page, usage), (0x0D, 0x04) | (0x0D, 0x02) | (0x0D, 0x05) | (0x0C, 0x01) | (0x01, 0x80))
}

#[derive(Default)]
struct Sinks {
    activity: Option<ActivitySink>,
    packets: Option<PacketSink>,
    hotkey: Option<HotkeySink>,
    emergency: Option<EmergencySink>,
}

struct Shared {
    inv: Arc<Inventory>,
    clock: Arc<dyn Clock>,
    sinks: Mutex<Sinks>,
    settings: Mutex<InputSettings>,
    wizard: Mutex<bool>,
    /// Filter unit id → device key.
    units: Mutex<HashMap<u32, DeviceKey>>,
    processors: Mutex<HashMap<u32, UnitProcessor>>,
    last_activity: Mutex<HashMap<DeviceKey, u64>>,
    /// Emergency presses (Ctrl+Shift+hotkey) of seat-1 keyboards, by filter unit id.
    console_emergency: Mutex<HashMap<u32, Vec<u64>>>,
}

struct Conn {
    ctl: Arc<dyn FilterControl>,
    reader: Option<JoinHandle<()>>,
    /// Set by `disconnect`; the reader checks it before every read.
    stop: Arc<AtomicBool>,
    /// Cleared by the reader when it exits (cancelled or gave up).
    alive: Arc<AtomicBool>,
}

/// Service-side reader of one HID collection of an extra seat.
struct HidWorker {
    seat: SeatNumber,
    path: String,
    reader: Arc<dyn HidReader>,
    thread: Option<JoinHandle<()>>,
}

pub struct KmdfFilterRouter {
    shared: Arc<Shared>,
    opener: Arc<dyn FilterOpener>,
    hid_source: Arc<dyn HidSource>,
    conn: Mutex<Option<Conn>>,
    table: Mutex<RouteTable>,
    revision: Mutex<u64>,
    sessions: Mutex<[Option<u32>; 16]>,
    hid: Mutex<HashMap<DeviceKey, HidWorker>>,
    /// Units another program holds exclusively (last `apply_routes`).
    hidden: Mutex<Vec<DeviceKey>>,
    /// The next `apply_routes` is the first on a new connection: every HID handle the
    /// console opened during fail-open (or under a previous service instance) may still be
    /// open.
    fresh: AtomicBool,
}

fn opts(s: &InputSettings) -> UnitOptions {
    UnitOptions { hotkey: Hotkey::from_setting(s.switch_hotkey_scan), allow_system_buttons: s.allow_system_buttons }
}

fn default_hid_source() -> Arc<dyn HidSource> {
    #[cfg(windows)]
    {
        Arc::new(crate::win::WinHidSource)
    }
    #[cfg(not(windows))]
    {
        Arc::new(crate::platform::NullHidSource)
    }
}

/// Repeat `cancel` until `h` has finished (or [`STOP_TIMEOUT`]), then join it. A cancel can
/// miss a reader that is between two reads, hence the loop.
fn stop_thread(h: JoinHandle<()>, cancel: impl Fn(), what: &str) {
    let t = Instant::now();
    loop {
        cancel();
        if h.is_finished() {
            let _ = h.join();
            return;
        }
        if t.elapsed() >= STOP_TIMEOUT {
            log::error!("{what} did not stop within {STOP_TIMEOUT:?}; left detached");
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn stop_conn(mut c: Conn) {
    c.stop.store(true, Ordering::SeqCst);
    if let Some(h) = c.reader.take() {
        let ctl = c.ctl.clone();
        stop_thread(h, move || ctl.cancel(), "input filter reader");
    }
}

fn stop_hid(mut w: HidWorker) {
    if let Some(h) = w.thread.take() {
        let r = w.reader.clone();
        stop_thread(h, move || r.cancel(), "HID reader");
    }
}

impl KmdfFilterRouter {
    pub fn new(inv: Arc<Inventory>, opener: Arc<dyn FilterOpener>, settings: &InputSettings) -> Arc<Self> {
        Self::with_parts(inv, opener, default_hid_source(), settings, Arc::new(SystemClock))
    }

    pub fn with_clock(
        inv: Arc<Inventory>,
        opener: Arc<dyn FilterOpener>,
        settings: &InputSettings,
        clock: Arc<dyn Clock>,
    ) -> Arc<Self> {
        Self::with_parts(inv, opener, default_hid_source(), settings, clock)
    }

    /// Every OS seam explicit (tests).
    pub fn with_parts(
        inv: Arc<Inventory>,
        opener: Arc<dyn FilterOpener>,
        hid_source: Arc<dyn HidSource>,
        settings: &InputSettings,
        clock: Arc<dyn Clock>,
    ) -> Arc<Self> {
        Arc::new(Self {
            shared: Arc::new(Shared {
                inv,
                clock,
                sinks: Mutex::new(Sinks::default()),
                settings: Mutex::new(settings.clone()),
                wizard: Mutex::new(false),
                units: Mutex::new(HashMap::new()),
                processors: Mutex::new(HashMap::new()),
                last_activity: Mutex::new(HashMap::new()),
                console_emergency: Mutex::new(HashMap::new()),
            }),
            opener,
            hid_source,
            conn: Mutex::new(None),
            table: Mutex::new(RouteTable::default()),
            revision: Mutex::new(0),
            sessions: Mutex::new([None; 16]),
            hid: Mutex::new(HashMap::new()),
            hidden: Mutex::new(Vec::new()),
            fresh: AtomicBool::new(false),
        })
    }

    /// Connected with a running reader.
    pub fn is_connected(&self) -> bool {
        lock(&self.conn).as_ref().is_some_and(|c| c.alive.load(Ordering::SeqCst))
    }

    /// Connected once, but the reader gave up: extra seats get no keyboard / mouse input
    /// until the next reconnect.
    pub fn reader_stopped(&self) -> bool {
        lock(&self.conn).as_ref().is_some_and(|c| !c.alive.load(Ordering::SeqCst))
    }

    fn config(&self) -> FilterConfig {
        FilterConfig::from_settings(&lock(&self.shared.settings), *lock(&self.shared.wizard))
    }

    /// Open, check, attach and start the reader (idempotent; replaces a dead connection).
    pub fn connect(&self) -> BackendResult<Arc<dyn FilterControl>> {
        let mut conn = lock(&self.conn);
        if let Some(c) = conn.as_ref() {
            if c.alive.load(Ordering::SeqCst) {
                return Ok(c.ctl.clone());
            }
        }
        if let Some(dead) = conn.take() {
            log::warn!("input filter reader had stopped; reconnecting");
            stop_conn(dead);
        }
        let ctl = self.opener.open().map_err(not_signed_error)?;
        let v = ctl.version()?;
        if v.major != INTERFACE_MAJOR {
            return Err(BackendError::new(
                ErrorKind::Driver,
                "driver_version_mismatch",
                format!("input filter interface {} (expected {INTERFACE_MAJOR}); left fail-open", v.major),
            ));
        }
        ctl.attach()?;
        ctl.set_config(&self.config())?;
        ctl.set_sessions(&lock(&self.sessions))?;
        self.shared.refresh_units(&*ctl);
        let shared = self.shared.clone();
        let reader_ctl = ctl.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let alive = Arc::new(AtomicBool::new(true));
        let (s, a) = (stop.clone(), alive.clone());
        let reader = std::thread::Builder::new()
            .name("seat-input-filter-reader".into())
            .spawn(move || reader_loop(shared, reader_ctl, s, a))
            .map_err(|e| BackendError::new(ErrorKind::Internal, "thread_spawn", e.to_string()))?;
        *conn = Some(Conn { ctl: ctl.clone(), reader: Some(reader), stop, alive });
        self.fresh.store(true, Ordering::SeqCst);
        Ok(ctl)
    }

    /// Stop the readers and close the control device (filters enter the gap mode: extra
    /// seats' input is dropped, never leaked to seat 1).
    pub fn disconnect(&self) {
        let c = lock(&self.conn).take();
        if let Some(c) = c {
            stop_conn(c);
        }
        let workers: Vec<HidWorker> = lock(&self.hid).drain().map(|(_, w)| w).collect();
        for w in workers {
            stop_hid(w);
        }
    }

    fn ctl(&self) -> Option<Arc<dyn FilterControl>> {
        lock(&self.conn).as_ref().map(|c| c.ctl.clone())
    }

    /// Start / stop the service-side HID readers for `table` (spec 50 §4.1.3 rule 2).
    fn sync_hid(&self, devices: &[Device], table: &RouteTable, report: &mut RouteReport) {
        let mut want: HashMap<DeviceKey, (SeatNumber, String)> = HashMap::new();
        for d in devices.iter().filter(|d| d.present && d.kind == DeviceKind::Hid) {
            let Some(i) = input_info(d) else { continue };
            if !service_reads(i.usage_page, i.usage) || report.not_routable.contains(&d.key) {
                continue;
            }
            if let UnitRoute::Seat(seat) = table.route(&d.key) {
                want.insert(d.key.clone(), (seat, i.interface_path.clone()));
            }
        }
        let mut workers = lock(&self.hid);
        let stale: Vec<DeviceKey> = workers
            .iter()
            .filter(|(k, w)| want.get(*k).is_none_or(|(s, p)| *s != w.seat || *p != w.path))
            .map(|(k, _)| k.clone())
            .collect();
        for k in stale {
            if let Some(w) = workers.remove(&k) {
                stop_hid(w);
            }
        }
        let mut hidden = Vec::new();
        for (key, (seat, path)) in want {
            if workers.contains_key(&key) {
                continue;
            }
            let opened = self.hid_source.open(&path).and_then(|reader| {
                let (shared, k, r) = (self.shared.clone(), key.clone(), reader.clone());
                std::thread::Builder::new()
                    .name("seat-input-hid-reader".into())
                    .spawn(move || hid_loop(shared, k, seat, r))
                    .map(|t| (reader, t))
                    .map_err(|e| BackendError::new(ErrorKind::Internal, "thread_spawn", e.to_string()))
            });
            match opened {
                Ok((reader, thread)) => {
                    workers.insert(key, HidWorker { seat, path, reader, thread: Some(thread) });
                }
                Err(e) => {
                    log::warn!("HID collection {path} not readable: {e}");
                    if e.reason == "hidden_by_other_software" {
                        hidden.push(key.clone());
                    }
                    report.applied.retain(|k| k != &key);
                    report.not_routable.push(key);
                }
            }
        }
        *lock(&self.hidden) = hidden;
    }
}

impl Drop for KmdfFilterRouter {
    fn drop(&mut self) {
        self.disconnect();
    }
}

fn not_signed_error(e: BackendError) -> BackendError {
    if e.kind == ErrorKind::NotFound {
        BackendError::new(ErrorKind::Driver, "driver_not_signed", "input filter is not loaded (unsigned builds are never installed)")
    } else {
        e
    }
}

impl Shared {
    fn refresh_units(&self, ctl: &dyn FilterControl) {
        let Ok(list) = ctl.list_units() else { return };
        let mut map = lock(&self.units);
        for u in list {
            if let Some(k) = self.inv.key_of_instance(&u.instance_id) {
                map.insert(u.unit_id, k);
            }
        }
    }

    fn key(&self, ctl: &dyn FilterControl, unit: u32) -> Option<DeviceKey> {
        if let Some(k) = lock(&self.units).get(&unit) {
            return Some(k.clone());
        }
        self.refresh_units(ctl);
        lock(&self.units).get(&unit).cloned()
    }

    fn activity(&self, key: &DeviceKey, key_sc: Option<u16>, click: bool) {
        let now = self.clock.now_ms();
        let wizard = key_sc.is_some() || click;
        {
            let mut last = lock(&self.last_activity);
            let t = last.entry(key.clone()).or_insert(0);
            if !wizard && *t != 0 && now.saturating_sub(*t) < ACTIVITY_THROTTLE_MS {
                return;
            }
            *t = now;
        }
        self.inv.touch(key, now);
        let s = lock(&self.sinks).activity.clone();
        if let Some(s) = s {
            s(ActivityEvent { key: key.clone(), at_ms: now, key_sc, click });
        }
    }

    fn deliver(&self, seat: SeatNumber, packets: &[InputPacket]) {
        if packets.is_empty() {
            return;
        }
        let sink = lock(&self.sinks).packets.clone();
        if let Some(sink) = sink {
            sink(seat, packets);
        }
    }

    fn fire_hotkey(&self, key: &DeviceKey, from: SeatNumber) {
        let s = lock(&self.sinks).hotkey.clone();
        if let Some(s) = s {
            s(HotkeyEvent { key: key.clone(), from });
        }
    }

    fn fire_emergency(&self) {
        let s = lock(&self.sinks).emergency.clone();
        if let Some(s) = s {
            s();
        }
    }

    /// One Ctrl+Shift+hotkey press of a seat-1 keyboard; `true` on the third within the
    /// window (same rule as `KeyTracker` for extra-seat keyboards).
    fn console_emergency_press(&self, unit: u32, now: u64) -> bool {
        let mut all = lock(&self.console_emergency);
        let presses = all.entry(unit).or_default();
        presses.retain(|t| now.saturating_sub(*t) <= EMERGENCY_WINDOW_MS);
        presses.push(now);
        if presses.len() >= EMERGENCY_PRESSES {
            presses.clear();
            return true;
        }
        false
    }

    fn handle(&self, ctl: &dyn FilterControl, packets: &[FilterPacket]) {
        let wizard = *lock(&self.wizard);
        let opts = opts(&lock(&self.settings));
        // Batch per seat to keep event counts low.
        let mut out: Vec<(SeatNumber, Vec<InputPacket>)> = Vec::new();
        let mut push = |seat: SeatNumber, p: Vec<InputPacket>| {
            if p.is_empty() {
                return;
            }
            match out.last_mut() {
                Some((s, v)) if *s == seat => v.extend(p),
                _ => out.push((seat, p)),
            }
        };
        for p in packets {
            let Some(key) = self.key(ctl, p.unit_id) else {
                log::debug!("packet from unknown filter unit {}", p.unit_id);
                continue;
            };
            // Scan code 0 is no key: console copies outside the wizard carry A = B = 0 (the driver
            // reports only that the unit was used, never which key).
            let make_sc = (p.kind == packet_kind::KEYBOARD && p.b & 1 == 0 && p.a != 0).then_some(p.a);
            let click = p.kind == packet_kind::MOUSE
                && p.b & (button_flags::LEFT_DOWN | button_flags::RIGHT_DOWN | button_flags::MIDDLE_DOWN) != 0;
            if p.kind == packet_kind::HOTKEY {
                let from = SeatNumber::new(p.seat).unwrap_or(SeatNumber::CONSOLE);
                if p.a & hotkey_bits::SHIFT != 0 {
                    // Ctrl+Shift + emergency key: an emergency press, never the move hotkey.
                    // Counted also with the move hotkey off (the driver checks the
                    // emergency key on its own, interface minor 1).
                    if self.console_emergency_press(p.unit_id, self.clock.now_ms()) {
                        self.fire_emergency();
                    }
                } else if opts.hotkey.is_some() {
                    self.fire_hotkey(&key, from);
                }
                continue;
            }

            if p.flags & (packet_flags::CONSOLE_COPY | packet_flags::WIZARD) != 0 {
                self.activity(&key, if wizard { make_sc } else { None }, wizard && click);
                continue;
            }
            let Some(seat) = SeatNumber::new(p.seat).filter(|s| !s.is_console()) else { continue };
            let Some(ev) = p.event() else { continue };
            let now = self.clock.now_ms();
            let outcomes = {
                let mut procs = lock(&self.processors);
                let proc = procs.entry(p.unit_id).or_insert_with(|| UnitProcessor::new(opts));
                proc.set_options(opts);
                let mut o: Vec<Outcome> = Vec::new();
                if p.flags & packet_flags::GAP != 0 {
                    o.extend(proc.release_all().into_iter().map(Outcome::Packet));
                }
                o.extend(proc.process(&ev, now));
                o
            };
            let (packets, hotkey, emergency) = split(outcomes);
            push(seat, packets);
            if hotkey {
                self.fire_hotkey(&key, seat);
            }
            if emergency {
                self.fire_emergency();
            }
            self.activity(&key, if wizard { make_sc } else { None }, wizard && click);
        }
        for (seat, p) in out {
            self.deliver(seat, &p);
        }
    }
}

fn reader_loop(shared: Arc<Shared>, ctl: Arc<dyn FilterControl>, stop: Arc<AtomicBool>, alive: Arc<AtomicBool>) {
    let mut failures = 0u32;
    while !stop.load(Ordering::SeqCst) {
        match ctl.read_packets(256) {
            Ok(p) => {
                failures = 0;
                shared.handle(&*ctl, &p);
            }
            Err(e) if e.reason == "cancelled" => break,
            Err(e) if is_fatal_win32(e.win32) => {
                log::error!("input filter reader stops (device gone): {e}");
                break;
            }
            Err(e) => {
                failures += 1;
                log::warn!("input filter read failed ({failures}): {e}");
                if failures > MAX_READ_FAILURES {
                    log::error!("input filter reader stops; extra seats get no input until reconnect");
                    break;
                }
                std::thread::sleep(Duration::from_millis(250));
            }
        }
    }
    alive.store(false, Ordering::SeqCst);
}

fn hid_loop(shared: Arc<Shared>, key: DeviceKey, seat: SeatNumber, reader: Arc<dyn HidReader>) {
    let mut proc = UnitProcessor::new(opts(&lock(&shared.settings)));
    let mut failures = 0u32;
    loop {
        match reader.read() {
            Ok(Some(ev)) => {
                failures = 0;
                proc.set_options(opts(&lock(&shared.settings)));
                let (packets, _, _) = split(proc.process(&ev, shared.clock.now_ms()));
                if !packets.is_empty() {
                    shared.deliver(seat, &packets);
                    shared.activity(&key, None, false);
                }
            }
            Ok(None) => {}
            Err(e) if e.reason == "cancelled" => break,
            Err(e) => {
                failures += 1;
                log::warn!("HID read failed ({failures}): {e}");
                if failures > MAX_READ_FAILURES || is_fatal_win32(e.win32) {
                    log::error!("HID reader for {key} stops until the next route change");
                    break;
                }
                std::thread::sleep(Duration::from_millis(250));
            }
        }
    }
    // Leave nothing held in the seat (contacts down, buttons, media keys).
    shared.deliver(seat, &proc.release_all());
}

impl InputRouter for KmdfFilterRouter {
    fn kind(&self) -> InputBackendKind {
        InputBackendKind::KmdfFilter
    }
    fn caps(&self) -> InputCaps {
        InputCaps {
            isolates_console: true,
            hid_visibility: true,
            activity: true,
            hotkey: true,
            wizard_capture: true,
            driver: Some(DriverId::KbdMouFilter),
        }
    }
    fn availability(&self) -> Availability {
        if self.is_connected() {
            return Availability::Available;
        }
        if self.reader_stopped() {
            return Availability::Unavailable { reason: "filter_reader_stopped".into() };
        }
        match self.opener.open() {
            Err(e) if e.kind == ErrorKind::NotFound => Availability::DriverNotSigned { driver: DriverId::KbdMouFilter },
            Err(e) => Availability::Unavailable { reason: e.reason },
            Ok(ctl) => match ctl.version() {
                Ok(v) if v.major == INTERFACE_MAJOR => Availability::Available,
                Ok(v) => Availability::DriverVersionMismatch { driver: DriverId::KbdMouFilter, found: v.major },
                Err(e) => Availability::Unavailable { reason: e.reason },
            },
        }
    }
    fn enumerate(&self) -> BackendResult<Vec<Device>> {
        let d = self.shared.inv.enumerate()?;
        if let Some(ctl) = self.ctl() {
            self.shared.refresh_units(&*ctl);
        }
        Ok(d)
    }
    fn apply_routes(&self, routes: &[InputRoute]) -> BackendResult<RouteReport> {
        let ctl = self.connect()?;
        let fresh = self.fresh.swap(false, Ordering::SeqCst);
        let table = RouteTable::from_routes(routes);
        let old = lock(&self.table).clone();
        let rev = *lock(&self.revision);
        let mut report = RouteReport::default();
        let devices = self.shared.inv.devices();
        for d in &devices {
            let Some(id) = d.instance_id.as_deref() else { continue };
            let mask = table.mask(&d.key);
            if let Err(e) = self.shared.inv.platform().set_seat_property(id, mask, rev) {
                log::warn!("seat property on {id}: {e}");
            }
            match ctl.set_unit(id, mask, rev) {
                Ok(()) => report.applied.push(d.key.clone()),
                Err(e) => {
                    log::warn!("filter unit {id}: {e}");
                    report.not_routable.push(d.key.clone());
                    continue;
                }
            }
            // Allowed HID opens stay valid after a change (spec 50 §4.1.3, open question
            // 11). On a new connection every console open made during fail-open (before the
            // first attach since boot) or under the previous service instance may survive.
            if d.kind == DeviceKind::Hid {
                let before = old.mask(&d.key);
                let stale = if fresh {
                    !mask.contains(SeatNumber::CONSOLE)
                } else {
                    before != mask && !lost_nothing(before, mask)
                };
                if stale {
                    report.needs_replug.push(d.key.clone());
                }
            }
        }
        self.sync_hid(&devices, &table, &mut report);
        *lock(&self.table) = table;
        self.shared.refresh_units(&*ctl);
        Ok(report)
    }
    fn set_wizard_capture(&self, on: bool) -> BackendResult<()> {
        *lock(&self.shared.wizard) = on;
        if let Some(ctl) = self.ctl() {
            ctl.set_config(&self.config())?;
        }
        Ok(())
    }
    fn set_hotplug_sink(&self, sink: HotplugSink) {
        self.shared.inv.set_hotplug_sink(sink);
    }
    fn set_activity_sink(&self, sink: ActivitySink) {
        lock(&self.shared.sinks).activity = Some(sink);
    }
    fn set_packet_sink(&self, sink: PacketSink) {
        lock(&self.shared.sinks).packets = Some(sink);
    }
    fn set_hotkey_sink(&self, sink: HotkeySink) {
        lock(&self.shared.sinks).hotkey = Some(sink);
    }
}

/// True when every seat that had the unit still has it (no session lost access).
fn lost_nothing(before: SeatMask, after: SeatMask) -> bool {
    after.is_shared() || (!before.is_shared() && before.0 & !after.0 == 0)
}

impl InputControl for KmdfFilterRouter {
    fn set_settings(&self, settings: &InputSettings) -> BackendResult<()> {
        *lock(&self.shared.settings) = settings.clone();
        if let Some(ctl) = self.ctl() {
            ctl.set_config(&self.config())?;
        }
        Ok(())
    }
    fn set_config_revision(&self, revision: u64) {
        *lock(&self.revision) = revision;
    }
    fn set_seat_sessions(&self, sessions: &[(SeatNumber, u32)]) -> BackendResult<()> {
        let mut arr = [None; 16];
        for (seat, id) in sessions {
            arr[usize::from(seat.index())] = Some(*id);
        }
        *lock(&self.sessions) = arr;
        if let Some(ctl) = self.ctl() {
            ctl.set_sessions(&arr)?;
        }
        Ok(())
    }
    fn set_emergency_sink(&self, sink: EmergencySink) {
        lock(&self.shared.sinks).emergency = Some(sink);
    }
    fn interfaces_for_seat(&self, _seat: SeatNumber) -> Vec<String> {
        // Packets come as `input_packets` events; hosts read no devices themselves.
        Vec::new()
    }
    fn feed_activity(&self, interface_path: &str, key_sc: Option<u16>, click: bool) {
        if let Some(key) = self.shared.inv.key_of_interface(interface_path) {
            self.shared.activity(&key, key_sc, click);
        }
    }
    fn notes(&self) -> Vec<String> {
        let mut n = Vec::new();
        if self.reader_stopped() {
            n.push("filter_reader_stopped".into());
        } else if !self.is_connected() {
            n.push("driver_not_signed".into());
        }
        if !lock(&self.hidden).is_empty() {
            n.push("hid_hidden_by_other_software".into());
        }
        n.push("xinput_pads_shared".into());
        n.push("touch_emulated_as_mouse".into());
        n
    }
}

/// `UnitRoute` of a unit in the current table (diagnostics).
pub fn route_of(router: &KmdfFilterRouter, key: &DeviceKey) -> UnitRoute {
    lock(&router.table).route(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convert::{RawContact, RawUnitEvent};
    use crate::filter_proto::{FilterUnit, FilterVersion};
    use crate::platform::fake::{FakeFilter, FakeHidSource, FakeOpener, FakePlatform};
    use crate::units::tests as ut;
    use seat_core::backend::input::key_flags;
    use seat_core::mock::MockClock;

    fn s(n: u8) -> SeatNumber {
        SeatNumber::new(n).unwrap()
    }

    struct Env {
        r: Arc<KmdfFilterRouter>,
        f: Arc<FakeFilter>,
        hid: Arc<FakeHidSource>,
        units: Vec<Device>,
    }

    fn env(loaded: bool) -> Env {
        let p = Arc::new(FakePlatform::default());
        *p.list.lock().unwrap() = ut::list();
        let inv = Inventory::new(p, Arc::new(ut::tree()));
        let f = Arc::new(FakeFilter::default());
        *f.version.lock().unwrap() = FilterVersion { major: 1, minor: 0, build: 1, flags: 0 };
        let units = inv.enumerate().unwrap();
        *f.units.lock().unwrap() = units
            .iter()
            .enumerate()
            .map(|(i, d)| FilterUnit { unit_id: i as u32 + 1, role: 1, instance_id: d.instance_id.clone().unwrap(), ..Default::default() })
            .collect();
        let opener = Arc::new(FakeOpener(Mutex::new(loaded.then(|| f.clone()))));
        let hid = Arc::new(FakeHidSource::default());
        let r = KmdfFilterRouter::with_parts(inv, opener, hid.clone(), &InputSettings::default(), Arc::new(MockClock::default()));
        Env { r, f, hid, units }
    }

    fn wait_for(cond: impl Fn() -> bool) {
        let t = Instant::now();
        while !cond() {
            assert!(t.elapsed() < Duration::from_secs(5), "timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    type Sent = Arc<Mutex<Vec<(SeatNumber, Vec<InputPacket>)>>>;

    fn record(r: &KmdfFilterRouter) -> Sent {
        let got: Sent = Arc::default();
        let g = got.clone();
        r.set_packet_sink(Arc::new(move |seat, p| g.lock().unwrap().push((seat, p.to_vec()))));
        got
    }

    fn hid_key(e: &Env, usage: u16) -> DeviceKey {
        e.units.iter().find(|d| d.kind == DeviceKind::Hid && input_info(d).unwrap().usage == usage).unwrap().key.clone()
    }

    #[test]
    fn unsigned_filter_reports_driver_not_signed() {
        let e = env(false);
        assert_eq!(e.r.availability(), Availability::DriverNotSigned { driver: DriverId::KbdMouFilter });
        let err = e.r.apply_routes(&[]).unwrap_err();
        assert_eq!((err.kind, err.reason.as_str()), (ErrorKind::Driver, "driver_not_signed"));
        assert!(e.r.notes().contains(&"driver_not_signed".to_string()));
    }

    #[test]
    fn version_mismatch_stays_fail_open() {
        let e = env(true);
        e.f.version.lock().unwrap().major = 2;
        assert_eq!(e.r.availability(), Availability::DriverVersionMismatch { driver: DriverId::KbdMouFilter, found: 2 });
        assert_eq!(e.r.apply_routes(&[]).unwrap_err().reason, "driver_version_mismatch");
        assert!(!e.f.calls().contains(&"attach".to_string()), "never attach to a wrong major");
    }

    #[test]
    fn connect_applies_units_and_forwards_packets() {
        let e = env(true);
        let kbd_i = e.units.iter().position(|d| d.kind == DeviceKind::Keyboard).unwrap();
        let kbd = e.units[kbd_i].key.clone();
        let got = record(&e.r);
        let hk: Arc<Mutex<Vec<HotkeyEvent>>> = Arc::default();
        let h = hk.clone();
        e.r.set_hotkey_sink(Arc::new(move |ev| h.lock().unwrap().push(ev)));
        e.r.set_seat_sessions(&[(s(2), 3)]).unwrap();
        let rep = e.r.apply_routes(&[InputRoute { key: kbd.clone(), mask: s(2).mask() }]).unwrap();
        assert_eq!(rep.applied.len(), e.units.len());
        let calls = e.f.calls();
        assert_eq!(&calls[..2], &["version".to_string(), "attach".to_string()]);
        assert!(calls.iter().any(|c| c.ends_with(" 2") && c.starts_with("unit HID\\VID_046D&PID_C52B&MI_00")));
        assert!(calls.iter().any(|c| c.starts_with("sessions [None, Some(3)")));
        let uid = kbd_i as u32 + 1;
        let pk = |sc: u16, b: u16, flags: u16| FilterPacket { unit_id: uid, kind: packet_kind::KEYBOARD, seat: 2, flags, a: sc, b, ..Default::default() };
        e.f.push(vec![pk(0x1E, 0, 0)]);
        // Ctrl + F12 is the hotkey: swallowed and reported.
        e.f.push(vec![pk(0x1D, 0, 0), pk(0x58, 0, 0)]);
        // After a gap the held keys are released first.
        e.f.push(vec![pk(0x30, 0, packet_flags::GAP)]);
        wait_for(|| got.lock().unwrap().len() >= 3);
        let got = got.lock().unwrap().clone();
        assert_eq!(got[0], (s(2), vec![InputPacket::Key { sc: 0x1E, f: 0 }]));
        assert_eq!(got[1], (s(2), vec![InputPacket::Key { sc: 0x1D, f: 0 }]));
        let ups = &got[2].1;
        assert!(ups.contains(&InputPacket::Key { sc: 0x1E, f: key_flags::UP }));
        assert!(ups.contains(&InputPacket::Key { sc: 0x1D, f: key_flags::UP }));
        assert_eq!(ups.last(), Some(&InputPacket::Key { sc: 0x30, f: 0 }));
        wait_for(|| !hk.lock().unwrap().is_empty());
        assert_eq!(hk.lock().unwrap()[0], HotkeyEvent { key: kbd.clone(), from: s(2) });
        e.r.disconnect();
        assert!(!e.r.is_connected());
    }

    #[test]
    fn seat_one_hotkey_with_shift_is_an_emergency_press() {
        let e = env(true);
        let kbd_i = e.units.iter().position(|d| d.kind == DeviceKind::Keyboard).unwrap();
        let hk: Arc<Mutex<Vec<HotkeyEvent>>> = Arc::default();
        let h = hk.clone();
        e.r.set_hotkey_sink(Arc::new(move |ev| h.lock().unwrap().push(ev)));
        let em: Arc<Mutex<u32>> = Arc::default();
        let m = em.clone();
        e.r.set_emergency_sink(Arc::new(move || *m.lock().unwrap() += 1));
        e.r.apply_routes(&[]).unwrap();
        let hot = |a: u16| FilterPacket { unit_id: kbd_i as u32 + 1, kind: packet_kind::HOTKEY, seat: 1, a, ..Default::default() };
        e.f.push(vec![hot(hotkey_bits::SHIFT), hot(hotkey_bits::SHIFT)]);
        e.f.push(vec![hot(hotkey_bits::SHIFT)]);
        wait_for(|| *em.lock().unwrap() == 1);
        assert!(hk.lock().unwrap().is_empty(), "Ctrl+Shift+hotkey never moves the keyboard");
        e.f.push(vec![hot(0)]);
        wait_for(|| hk.lock().unwrap().len() == 1);
        assert_eq!(hk.lock().unwrap()[0].from, SeatNumber::CONSOLE);
    }

    #[test]
    fn dead_reader_is_reported_and_reconnected() {
        let e = env(true);
        e.r.apply_routes(&[]).unwrap();
        assert!(e.r.is_connected());
        *e.f.fail_win32.lock().unwrap() = Some(1167); // ERROR_DEVICE_NOT_CONNECTED
        wait_for(|| e.r.reader_stopped());
        assert!(!e.r.is_connected());
        assert_eq!(e.r.availability(), Availability::Unavailable { reason: "filter_reader_stopped".into() });
        assert!(e.r.notes().contains(&"filter_reader_stopped".to_string()));
        *e.f.fail_win32.lock().unwrap() = None;
        let before = *e.f.reads.lock().unwrap();
        e.r.apply_routes(&[]).unwrap();
        assert!(e.r.is_connected());
        assert_eq!(e.f.calls().iter().filter(|c| *c == "attach").count(), 2, "reattached");
        wait_for(|| *e.f.reads.lock().unwrap() > before);
        e.r.disconnect();
        assert!(!e.r.is_connected() && !e.r.reader_stopped());
    }

    #[test]
    fn wizard_and_console_copies_only_produce_activity() {
        let e = env(true);
        let act: Arc<Mutex<Vec<ActivityEvent>>> = Arc::default();
        let a = act.clone();
        e.r.set_activity_sink(Arc::new(move |ev| a.lock().unwrap().push(ev)));
        let pk_sink: Arc<Mutex<usize>> = Arc::default();
        let ps = pk_sink.clone();
        e.r.set_packet_sink(Arc::new(move |_, _| *ps.lock().unwrap() += 1));
        e.r.apply_routes(&[]).unwrap();
        e.r.set_wizard_capture(true).unwrap();
        assert!(e.f.calls().iter().any(|c| c == "config 0x5"));
        e.f.push(vec![FilterPacket {
            unit_id: 1,
            kind: packet_kind::KEYBOARD,
            seat: 1,
            flags: packet_flags::CONSOLE_COPY,
            a: 0x3C,
            ..Default::default()
        }]);
        wait_for(|| !act.lock().unwrap().is_empty());
        assert_eq!(act.lock().unwrap()[0].key_sc, Some(0x3C));
        assert_eq!(*pk_sink.lock().unwrap(), 0);
    }

    #[test]
    fn hid_reassignment_needs_replug() {
        let e = env(true);
        let hid = e.units.iter().find(|d| d.kind == DeviceKind::Hid).unwrap().key.clone();
        e.r.apply_routes(&[InputRoute { key: hid.clone(), mask: s(2).mask() }]).unwrap();
        let rep = e.r.apply_routes(&[InputRoute { key: hid.clone(), mask: s(3).mask() }]).unwrap();
        assert_eq!(rep.needs_replug, vec![hid.clone()]);
        let rep = e.r.apply_routes(&[InputRoute { key: hid.clone(), mask: SeatMask(0b110) }]).unwrap();
        assert!(rep.needs_replug.is_empty(), "seat 3 keeps access");
        assert_eq!(route_of(&e.r, &hid), UnitRoute::Shared(SeatMask(0b110)));
    }

    #[test]
    fn first_apply_after_connect_reports_fail_open_handles() {
        let e = env(true);
        let touch = hid_key(&e, 0x04);
        let pen = hid_key(&e, 0x02);
        let rep = e.r.apply_routes(&[
            InputRoute { key: touch.clone(), mask: s(2).mask() },
            InputRoute { key: pen.clone(), mask: SeatMask(0b11) },
        ])
        .unwrap();
        assert_eq!(rep.needs_replug, vec![touch.clone()], "console may still hold the touch screen; the pen stays visible to it");
        // Same table again on the same connection: nothing new.
        let rep = e.r.apply_routes(&[
            InputRoute { key: touch.clone(), mask: s(2).mask() },
            InputRoute { key: pen.clone(), mask: SeatMask(0b11) },
        ])
        .unwrap();
        assert!(rep.needs_replug.is_empty());
    }

    #[test]
    fn service_reads_extra_seat_touch_and_releases_on_move() {
        let e = env(true);
        let got = record(&e.r);
        let touch = hid_key(&e, 0x04);
        e.r.apply_routes(&[InputRoute { key: touch.clone(), mask: s(2).mask() }]).unwrap();
        assert_eq!(*e.hid.opened.lock().unwrap(), vec![ut::TOUCH_PATH.to_ascii_lowercase()]);
        let dev = e.hid.dev(ut::TOUCH_PATH);
        let c = RawContact { id: 1, tip: true, in_range: true, x: 100, y: 200, pressure: 0, ..RawContact::default() };
        dev.push(RawUnitEvent::Digitizer { contacts: vec![c], pen: false });
        wait_for(|| !got.lock().unwrap().is_empty());
        {
            let g = got.lock().unwrap();
            assert_eq!(g[0].0, s(2));
            assert!(matches!(&g[0].1[0], InputPacket::Touch { contacts, .. } if contacts[0].phase == "down"));
        }
        // Back to seat 1: the reader stops and the held contact is lifted in seat 2.
        e.r.apply_routes(&[]).unwrap();
        assert!(dev.is_cancelled());
        let g = got.lock().unwrap();
        let (seat, last) = g.last().unwrap();
        assert_eq!(*seat, s(2));
        assert!(matches!(&last[0], InputPacket::Touch { contacts, .. } if contacts[0].phase == "up"));
    }

    #[test]
    fn hid_held_by_other_software_is_not_routable() {
        let e = env(true);
        let cons = hid_key(&e, 0x01);
        e.hid.held.lock().unwrap().push(ut::CONS_PATH.to_ascii_lowercase());
        let rep = e.r.apply_routes(&[InputRoute { key: cons.clone(), mask: s(2).mask() }]).unwrap();
        assert!(rep.not_routable.contains(&cons));
        assert!(!rep.applied.contains(&cons));
        assert!(e.r.notes().contains(&"hid_hidden_by_other_software".to_string()));
        // Consumer keys of a seat-1 unit are not read by the service.
        e.hid.held.lock().unwrap().clear();
        let rep = e.r.apply_routes(&[]).unwrap();
        assert!(rep.not_routable.is_empty());
        assert!(!e.r.notes().contains(&"hid_hidden_by_other_software".to_string()));
    }

    #[test]
    fn service_read_usages() {
        for (p, u) in [(0x0D, 0x04), (0x0D, 0x02), (0x0D, 0x05), (0x0C, 0x01), (0x01, 0x80)] {
            assert!(service_reads(p, u));
        }
        assert!(!service_reads(0x01, 0x05), "game pads stay with their session (HID visibility)");
        assert!(!service_reads(0x01, 0x06));
    }
}
