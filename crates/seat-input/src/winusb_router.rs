// SPDX-License-Identifier: GPL-3.0-only
//! `winusb_capture` (osms G2): a hybrid router. Every unit is handled as with
//! `raw_input_host` ([`RawInputRouter`], seat hosts read raw input); in addition, USB
//! keyboard / mouse interfaces routed to one **running** extra seat are captured
//! ([`crate::usb_capture`]): moved to the inbox WinUSB driver, read in the service and
//! delivered as `input_packets` events, so the console never sees them.
//!
//! Captured units vanish from raw input, so the router lists them itself (from the
//! journal record) and the console listener no longer reports them: their activity,
//! move hotkey and emergency combination come from the service-side reader. The wizard
//! releases every capture while it runs (the units must reach the console listener), and
//! the next route apply after it captures again.
//!
//! **Untested on hardware (HYPOTHESIS)**; the logic is tested against fakes below.

use crate::convert::{Hotkey, UnitOptions};
use crate::inventory::{Inventory, lock};
use crate::raw_router::RawInputRouter;
use crate::routes::{RouteTable, UnitRoute};
use crate::usb_capture::{self, CaptureHandler, Refusal, UsbCapture};
use crate::{EmergencySink, InputControl};
use seat_core::backend::input::{
    ActivityEvent, ActivitySink, HotkeyEvent, HotkeySink, InputBackendKind, InputCaps, InputPacket, InputRoute, InputRouter, PacketSink, RouteReport,
};
use seat_core::backend::session::HostTopology;
use seat_core::backend::{Availability, BackendResult, HotplugSink};
use seat_core::config::InputSettings;
use seat_core::device::{Device, DeviceKind};
use seat_core::ids::{DeviceKey, SeatNumber};
use seat_core::{Clock, SystemClock};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Arc, Mutex, Weak};

/// Minimum spacing of activity events per captured unit (as `kmdf_router`).
pub const ACTIVITY_THROTTLE_MS: u64 = 100;
/// Activity events waiting for the delivery worker; more are dropped (activity is a hint).
const ACTIVITY_QUEUE: usize = 64;

/// Sinks and settings the capture readers deliver through.
struct Shared {
    settings: Mutex<InputSettings>,
    packets: Mutex<Option<PacketSink>>,
    hotkey: Mutex<Option<HotkeySink>>,
    emergency: Mutex<Option<EmergencySink>>,
    activity: Arc<Mutex<Option<ActivitySink>>>,
    /// Last activity time per unit (throttle).
    last_activity: Mutex<BTreeMap<DeviceKey, u64>>,
    /// To the activity worker. gaming PC 2026-11-25: the service's activity sink takes the
    /// service state lock; called on the USB reader thread for every report it held the
    /// next `WinUsb_ReadPipe` back, and the seat pointer moved in jumps about once a second.
    activity_tx: Mutex<Option<SyncSender<ActivityEvent>>>,
    inv: Arc<Inventory>,
    clock: Arc<dyn Clock>,
}

impl Shared {
    /// Worker that touches the inventory and calls the activity sink off the reader threads.
    /// Ends when `Shared` (the sender) is dropped.
    fn start_activity_worker(&self) {
        let (tx, rx) = sync_channel::<ActivityEvent>(ACTIVITY_QUEUE);
        let (inv, sink) = (self.inv.clone(), self.activity.clone());
        let spawned = std::thread::Builder::new().name("seat-input-usb-activity".into()).spawn(move || {
            for ev in rx {
                inv.touch(&ev.key, ev.at_ms);
                let s = lock(&sink).clone();
                if let Some(s) = s {
                    s(ev);
                }
            }
        });
        match spawned {
            Ok(_) => *lock(&self.activity_tx) = Some(tx),
            Err(e) => log::error!("winusb_capture: activity worker not started ({e}); activity is not reported"),
        }
    }
}

impl CaptureHandler for Shared {
    fn packets(&self, seat: SeatNumber, packets: &[InputPacket]) {
        let s = lock(&self.packets).clone();
        if let Some(s) = s {
            s(seat, packets);
        }
    }
    fn hotkey(&self, key: &DeviceKey, seat: SeatNumber) {
        let s = lock(&self.hotkey).clone();
        if let Some(s) = s {
            s(HotkeyEvent { key: key.clone(), from: seat });
        }
    }
    fn emergency(&self) {
        let s = lock(&self.emergency).clone();
        if let Some(s) = s {
            s();
        }
    }
    /// Called on the USB reader thread for every report: throttled, then handed to the
    /// activity worker (never blocks the reader).
    fn activity(&self, key: &DeviceKey) {
        let at_ms = self.clock.now_ms();
        {
            let mut last = lock(&self.last_activity);
            let t = last.entry(key.clone()).or_insert(0);
            if *t != 0 && at_ms.saturating_sub(*t) < ACTIVITY_THROTTLE_MS {
                return;
            }
            *t = at_ms;
        }
        if let Some(tx) = lock(&self.activity_tx).as_ref() {
            let _ = tx.try_send(ActivityEvent { key: key.clone(), at_ms, key_sc: None, click: false });
        }
    }
    fn options(&self) -> UnitOptions {
        let s = lock(&self.settings);
        UnitOptions { hotkey: Hotkey::from_setting(s.switch_hotkey_scan), allow_system_buttons: s.allow_system_buttons }
    }
}

pub struct WinusbCaptureRouter {
    raw: Arc<RawInputRouter>,
    inv: Arc<Inventory>,
    capture: Arc<UsbCapture>,
    shared: Arc<Shared>,
    topology: HostTopology,
    running: Mutex<BTreeSet<SeatNumber>>,
    wizard: Mutex<bool>,
    table: Mutex<RouteTable>,
    refused: Mutex<Vec<(DeviceKey, Refusal)>>,
    /// Swap / journal errors of the last apply.
    errors: Mutex<Vec<String>>,
}

impl WinusbCaptureRouter {
    pub fn new(inv: Arc<Inventory>, settings: &InputSettings, topology: HostTopology, capture: Arc<UsbCapture>) -> Arc<Self> {
        Self::with_clock(inv, settings, topology, capture, Arc::new(SystemClock))
    }

    pub fn with_clock(inv: Arc<Inventory>, settings: &InputSettings, topology: HostTopology, capture: Arc<UsbCapture>, clock: Arc<dyn Clock>) -> Arc<Self> {
        let raw = RawInputRouter::new(inv.clone(), settings, topology);
        let shared = Arc::new(Shared {
            settings: Mutex::new(settings.clone()),
            packets: Mutex::new(None),
            hotkey: Mutex::new(None),
            emergency: Mutex::new(None),
            activity: Arc::new(Mutex::new(None)),
            last_activity: Mutex::new(BTreeMap::new()),
            activity_tx: Mutex::new(None),
            inv: inv.clone(),
            clock,
        });
        shared.start_activity_worker();
        let weak: Weak<dyn CaptureHandler> = Arc::downgrade(&(shared.clone() as Arc<dyn CaptureHandler>));
        capture.set_handler(weak);
        // A new router = the input backend (or its settings) was chosen again: an operator's
        // hold ends here.
        capture.resume();
        Arc::new(Self {
            raw,
            inv,
            capture,
            shared,
            topology,
            running: Mutex::new(BTreeSet::new()),
            wizard: Mutex::new(false),
            table: Mutex::new(RouteTable::default()),
            refused: Mutex::new(Vec::new()),
            errors: Mutex::new(Vec::new()),
        })
    }

    /// Keys of the units captured now.
    pub fn captured_keys(&self) -> BTreeSet<DeviceKey> {
        self.capture.captured_units().into_iter().map(|d| d.key).collect()
    }

    fn sync(&self) {
        let table = lock(&self.table).clone();
        let live = self.inv.devices();
        let groups_live = usb_capture::groups(&**self.inv.pnp(), &live);
        let mut groups = self.capture.captured_groups();
        for g in groups_live {
            if !groups.iter().any(|c| c.node.eq_ignore_ascii_case(&g.node)) {
                groups.push(g);
            }
        }
        groups.sort_by(|a, b| a.node.cmp(&b.node));
        let on_groups: BTreeSet<&DeviceKey> = groups.iter().flat_map(|g| g.units.iter().map(|d| &d.key)).collect();
        let others: Vec<Device> =
            live.iter().filter(|d| d.present && matches!(d.kind, DeviceKind::Keyboard | DeviceKind::Mouse) && !on_groups.contains(&d.key)).cloned().collect();
        let plan = usb_capture::plan(&groups, &table, &lock(&self.running), *lock(&self.wizard), &others);
        let errors: Vec<String> = self.capture.sync(&plan.capture).into_iter().map(|(n, e)| format!("{n}: {e}")).collect();
        for e in &errors {
            log::warn!("winusb_capture: {e}");
        }
        *lock(&self.errors) = errors;
        *lock(&self.refused) = plan.refused;
    }

    /// Every unit routed to an extra seat is captured (none left on raw input).
    fn all_seat_units_captured(&self) -> bool {
        let table = lock(&self.table);
        let captured = self.captured_keys();
        self.inv
            .devices()
            .iter()
            .filter(|d| matches!(d.kind, DeviceKind::Keyboard | DeviceKind::Mouse))
            .all(|d| !matches!(table.route(&d.key), UnitRoute::Seat(_)) || captured.contains(&d.key))
    }
}

impl InputRouter for WinusbCaptureRouter {
    fn kind(&self) -> InputBackendKind {
        InputBackendKind::WinusbCapture
    }
    fn caps(&self) -> InputCaps {
        let mut c = self.raw.caps();
        // Captured units never reach the console under any topology; the rest only under
        // `dedicated_host` (as with raw_input_host).
        c.isolates_console = c.isolates_console || self.all_seat_units_captured();
        c
    }
    fn availability(&self) -> Availability {
        match self.capture.check() {
            Ok(()) => self.raw.availability(),
            Err(reason) => Availability::Unavailable { reason },
        }
    }
    fn enumerate(&self) -> BackendResult<Vec<Device>> {
        let mut d = self.raw.enumerate()?;
        for u in self.capture.captured_units() {
            if !d.iter().any(|x| x.key == u.key) {
                d.push(u);
            }
        }
        Ok(d)
    }
    fn apply_routes(&self, routes: &[InputRoute]) -> BackendResult<RouteReport> {
        let mut report = self.raw.apply_routes(routes)?;
        *lock(&self.table) = RouteTable::from_routes(routes);
        self.sync();
        for k in self.captured_keys() {
            report.not_routable.retain(|x| x != &k);
            if routes.iter().any(|r| r.key == k) && !report.applied.contains(&k) {
                report.applied.push(k);
            }
        }
        Ok(report)
    }
    fn set_wizard_capture(&self, on: bool) -> BackendResult<()> {
        *lock(&self.wizard) = on;
        if on {
            // The units must be seen by the console listener during the wizard.
            for e in self.capture.release_all() {
                log::warn!("winusb_capture: wizard release: {e}");
            }
        }
        self.raw.set_wizard_capture(on)
    }
    fn set_hotplug_sink(&self, sink: HotplugSink) {
        self.raw.set_hotplug_sink(sink);
    }
    fn set_activity_sink(&self, sink: ActivitySink) {
        *lock(&self.shared.activity) = Some(sink.clone());
        self.raw.set_activity_sink(sink);
    }
    fn set_packet_sink(&self, sink: PacketSink) {
        *lock(&self.shared.packets) = Some(sink);
    }
    fn set_hotkey_sink(&self, sink: HotkeySink) {
        *lock(&self.shared.hotkey) = Some(sink);
    }
}

impl InputControl for WinusbCaptureRouter {
    fn set_settings(&self, settings: &InputSettings) -> BackendResult<()> {
        *lock(&self.shared.settings) = settings.clone();
        self.raw.set_settings(settings)
    }
    fn set_config_revision(&self, revision: u64) {
        self.raw.set_config_revision(revision);
    }
    fn set_seat_sessions(&self, sessions: &[(SeatNumber, u32)]) -> BackendResult<()> {
        self.raw.set_seat_sessions(sessions)
    }
    fn set_running_seats(&self, seats: &[SeatNumber]) {
        let next: BTreeSet<SeatNumber> = seats.iter().copied().collect();
        let mut running = lock(&self.running);
        // A seat that starts ends the operator's hold (`restore_usb_input`); seats that kept
        // running since do not.
        if next.iter().any(|s| !running.contains(s)) {
            self.capture.resume();
        }
        *running = next;
    }
    fn set_emergency_sink(&self, sink: EmergencySink) {
        *lock(&self.shared.emergency) = Some(sink);
    }
    fn interfaces_for_seat(&self, seat: SeatNumber) -> Vec<String> {
        self.raw.interfaces_for_seat(seat)
    }
    fn feed_activity(&self, interface_path: &str, key_sc: Option<u16>, click: bool) {
        self.raw.feed_activity(interface_path, key_sc, click);
    }
    fn set_console_reporter(&self, connected: bool) {
        self.raw.set_console_reporter(connected);
    }
    fn release_captures(&self) -> Vec<String> {
        self.capture.release_all()
    }
    fn notes(&self) -> Vec<String> {
        let mut n = self.raw.notes();
        if self.topology != HostTopology::DedicatedHost && self.all_seat_units_captured() {
            n.retain(|x| x != "input_not_isolated");
        }
        if !self.capture.captured().is_empty() {
            n.push("usb_input_captured".into());
        }
        if self.capture.is_held() {
            n.push("usb_capture_held".into());
        }
        if !self.capture.inexact_restores().is_empty() {
            n.push("usb_restore_inexact".into());
        }
        let mut why: Vec<&'static str> = lock(&self.refused).iter().map(|(_, r)| r.note()).collect();
        why.sort_unstable();
        why.dedup();
        n.extend(why.into_iter().map(String::from));
        if !lock(&self.errors).is_empty() || !self.capture.pending_nodes().is_empty() {
            n.push("usb_capture_failed".into());
        }
        n
    }
}

/// `BackendInfo::notes` of the kind without building it.
pub fn static_notes(topology: HostTopology) -> Vec<String> {
    let mut n = Vec::new();
    if topology != HostTopology::DedicatedHost {
        n.push("input_not_isolated_non_usb".to_string());
    }
    n.extend(["gamepads_shared", "touch_emulated_as_mouse"].map(String::from));
    n
}

/// Seats (and their captured node count) for diagnostics.
pub fn captured_by_seat(c: &UsbCapture) -> BTreeMap<SeatNumber, usize> {
    let mut m = BTreeMap::new();
    for s in c.captured().into_values() {
        *m.entry(s).or_insert(0) += 1;
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hid_layout::tests::KEYBOARD_DESC;
    use crate::platform::fake::FakePlatform;
    use crate::units::tests as ut;
    use crate::usb_capture::fake::{FakeSwap, FakeUsbSource};
    use crate::usb_capture::{JournalStore, MemoryJournal};
    use seat_core::backend::input::key_flags;
    use std::time::{Duration, Instant};

    const KBD_NODE: &str = r"USB\VID_046D&PID_C52B&MI_00\6&0";

    /// Packets the router delivered, by seat.
    type Got = Arc<Mutex<Vec<(SeatNumber, Vec<InputPacket>)>>>;

    fn s(n: u8) -> SeatNumber {
        SeatNumber::new(n).unwrap()
    }

    struct Env {
        r: Arc<WinusbCaptureRouter>,
        p: Arc<FakePlatform>,
        swap: Arc<FakeSwap>,
        src: Arc<FakeUsbSource>,
        journal: Arc<MemoryJournal>,
        clock: Arc<seat_core::mock::MockClock>,
    }

    /// The fake tree (`units::tests::tree`): one composite USB receiver (keyboard on MI_00,
    /// mouse + consumer on MI_01). Tests that capture add a second USB keyboard, so the
    /// console keeps one; the mouse always stays with the console.
    fn env(topology: HostTopology) -> Env {
        let p = Arc::new(FakePlatform::default());
        *p.list.lock().unwrap() = ut::list();
        let inv = Inventory::new(p.clone(), Arc::new(ut::tree()));
        let swap = Arc::new(FakeSwap::default());
        let src = Arc::new(FakeUsbSource::default());
        let journal = Arc::new(MemoryJournal::default());
        let cap = UsbCapture::with_clock(swap.clone(), src.clone(), journal.clone(), Arc::new(seat_core::mock::MockClock::default()));
        let clock = Arc::new(seat_core::mock::MockClock::default());
        clock.advance(1_000);
        let r = WinusbCaptureRouter::with_clock(inv, &InputSettings::default(), topology, cap, clock.clone());
        r.enumerate().unwrap();
        Env { r, p, swap, src, journal, clock }
    }

    fn kbd(e: &Env) -> DeviceKey {
        e.r.enumerate().unwrap().into_iter().find(|d| d.kind == DeviceKind::Keyboard).unwrap().key
    }

    fn wait_for(cond: impl Fn() -> bool) {
        let t = Instant::now();
        while !cond() {
            assert!(t.elapsed() < Duration::from_secs(5), "timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn last_console_keyboard_is_never_captured() {
        let e = env(HostTopology::SeatOneConsole);
        let k = kbd(&e);
        e.r.set_running_seats(&[s(2)]);
        e.r.apply_routes(&[InputRoute { key: k.clone(), mask: s(2).mask() }]).unwrap();
        assert!(e.swap.calls().is_empty(), "the only keyboard stays with the console");
        assert!(e.r.notes().contains(&"usb_capture_last_console_input".to_string()));
        assert!(e.r.notes().contains(&"input_not_isolated".to_string()));
    }

    /// Adds a second USB keyboard on another port, so one can be captured.
    fn add_second_keyboard(e: &Env) {
        let mut l = ut::list();
        l.keyboards.push(ut::KBD2_PATH.into());
        *e.p.list.lock().unwrap() = l;
        e.r.enumerate().unwrap();
    }

    #[test]
    fn captures_running_seat_keyboard_and_forwards_packets() {
        let e = env(HostTopology::SeatOneConsole);
        add_second_keyboard(&e);
        let k = kbd(&e);
        let got: Got = Arc::default();
        let g = got.clone();
        e.r.set_packet_sink(Arc::new(move |seat, p| g.lock().unwrap().push((seat, p.to_vec()))));
        let dev = e.src.plug(KBD_NODE, Some(KEYBOARD_DESC));
        // Seat not running yet: nothing captured.
        e.r.apply_routes(&[InputRoute { key: k.clone(), mask: s(2).mask() }]).unwrap();
        assert!(e.swap.calls().is_empty());
        e.r.set_running_seats(&[s(2)]);
        let rep = e.r.apply_routes(&[InputRoute { key: k.clone(), mask: s(2).mask() }]).unwrap();
        assert!(rep.applied.contains(&k));
        assert!(e.swap.is_captured(KBD_NODE));
        assert_eq!(e.journal.load().entries.len(), 1);
        assert!(e.r.notes().contains(&"usb_input_captured".to_string()));
        // The captured unit vanishes from raw input; the router still lists it.
        let mut l = ut::list();
        l.keyboards.retain(|p| p != ut::KBD_PATH);
        l.keyboards.push(ut::KBD2_PATH.into());
        *e.p.list.lock().unwrap() = l;
        let listed = e.r.enumerate().unwrap();
        assert!(listed.iter().any(|d| d.key == k && d.present));
        assert!(e.r.interfaces_for_seat(s(2)).is_empty(), "raw input no longer reads it");
        dev.push(&[0, 0, 0x04, 0, 0, 0, 0, 0]);
        wait_for(|| !got.lock().unwrap().is_empty());
        assert_eq!(got.lock().unwrap()[0], (s(2), vec![InputPacket::Key { sc: 0x1E, f: 0 }]));
        // Seat stops: released at the next apply; the held key goes up in the seat first.
        e.r.set_running_seats(&[]);
        e.r.apply_routes(&[InputRoute { key: k.clone(), mask: s(2).mask() }]).unwrap();
        assert!(!e.swap.is_captured(KBD_NODE));
        assert!(e.journal.load().entries.is_empty());
        assert_eq!(got.lock().unwrap().last().unwrap().1, vec![InputPacket::Key { sc: 0x1E, f: key_flags::UP }]);
    }

    #[test]
    fn wizard_releases_and_backend_release_restores() {
        let e = env(HostTopology::SeatOneConsole);
        add_second_keyboard(&e);
        let k = kbd(&e);
        e.src.plug(KBD_NODE, None);
        e.r.set_running_seats(&[s(2)]);
        e.r.apply_routes(&[InputRoute { key: k.clone(), mask: s(2).mask() }]).unwrap();
        assert!(e.swap.is_captured(KBD_NODE));
        e.r.set_console_reporter(true);
        e.r.set_wizard_capture(true).unwrap();
        assert!(!e.swap.is_captured(KBD_NODE));
        e.r.apply_routes(&[InputRoute { key: k.clone(), mask: s(2).mask() }]).unwrap();
        assert!(!e.swap.is_captured(KBD_NODE), "no capture while the wizard runs");
        e.r.set_wizard_capture(false).unwrap();
        e.r.apply_routes(&[InputRoute { key: k.clone(), mask: s(2).mask() }]).unwrap();
        assert!(e.swap.is_captured(KBD_NODE));
        assert!(e.r.release_captures().is_empty());
        assert!(!e.swap.is_captured(KBD_NODE));
    }

    /// e2e gaming PC 2026-11-24: the operator's restore while seat 2 runs stays in force at
    /// the following route applies; a seat start ends it.
    #[test]
    fn operator_hold_survives_applies_until_a_seat_starts() {
        let e = env(HostTopology::SeatOneConsole);
        add_second_keyboard(&e);
        let k = kbd(&e);
        e.src.plug(KBD_NODE, None);
        let routes = [InputRoute { key: k.clone(), mask: s(2).mask() }];
        e.r.set_running_seats(&[s(2)]);
        e.r.apply_routes(&routes).unwrap();
        assert!(e.swap.is_captured(KBD_NODE));
        e.r.capture.hold();
        assert!(e.r.capture.restore_all().is_empty());
        // Hot-plug / tick applies with seat 2 still running.
        e.r.set_running_seats(&[s(2)]);
        e.r.apply_routes(&routes).unwrap();
        e.r.apply_routes(&routes).unwrap();
        assert!(!e.swap.is_captured(KBD_NODE));
        assert!(e.r.notes().contains(&"usb_capture_held".to_string()));
        // Seat 2 stops and starts again.
        e.r.set_running_seats(&[]);
        e.r.apply_routes(&routes).unwrap();
        e.r.set_running_seats(&[s(2)]);
        e.r.apply_routes(&routes).unwrap();
        assert!(e.swap.is_captured(KBD_NODE));
        assert!(!e.r.notes().contains(&"usb_capture_held".to_string()));
        e.r.release_captures();
    }

    /// gaming PC 2026-11-25 (pointer moved about once a second): a slow activity sink (the
    /// service takes its state lock there) must not hold the reader back, and activity is
    /// throttled per unit.
    #[test]
    fn slow_activity_sink_does_not_stall_packets() {
        let e = env(HostTopology::SeatOneConsole);
        add_second_keyboard(&e);
        let k = kbd(&e);
        let got: Got = Arc::default();
        let g = got.clone();
        e.r.set_packet_sink(Arc::new(move |seat, p| g.lock().unwrap().push((seat, p.to_vec()))));
        let gate = Arc::new(Mutex::new(()));
        let seen = Arc::new(Mutex::new(0usize));
        let (g2, s2) = (gate.clone(), seen.clone());
        e.r.set_activity_sink(Arc::new(move |_| {
            let _held = g2.lock().unwrap();
            *s2.lock().unwrap() += 1;
        }));
        let dev = e.src.plug(KBD_NODE, Some(KEYBOARD_DESC));
        e.r.set_running_seats(&[s(2)]);
        e.r.apply_routes(&[InputRoute { key: k.clone(), mask: s(2).mask() }]).unwrap();
        let blocked = gate.lock().unwrap();
        for i in 0..6u8 {
            let key = if i % 2 == 0 { 0x04 } else { 0 };
            dev.push(&[0, 0, key, 0, 0, 0, 0, 0]);
        }
        wait_for(|| got.lock().unwrap().len() >= 6);
        drop(blocked);
        wait_for(|| *seen.lock().unwrap() >= 1);
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(*seen.lock().unwrap(), 1, "one activity per throttle window");
        e.clock.advance(ACTIVITY_THROTTLE_MS);
        dev.push(&[0, 0, 0x04, 0, 0, 0, 0, 0]);
        wait_for(|| *seen.lock().unwrap() == 2);
        e.r.release_captures();
    }

    #[test]
    fn kind_and_static_notes() {
        let e = env(HostTopology::DedicatedHost);
        assert_eq!(e.r.kind(), InputBackendKind::WinusbCapture);
        assert!(e.r.availability().is_available());
        assert!(e.r.caps().isolates_console);
        assert!(static_notes(HostTopology::SeatOneConsole).contains(&"input_not_isolated_non_usb".to_string()));
        assert!(!static_notes(HostTopology::DedicatedHost).contains(&"input_not_isolated_non_usb".to_string()));
        assert!(captured_by_seat(&e.r.capture).is_empty());
    }
}
