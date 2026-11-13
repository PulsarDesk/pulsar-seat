// SPDX-License-Identifier: GPL-3.0-only
//! `raw_input_host` (default, user mode): the service keeps the route table and the
//! inventory; each seat host reads its own units with raw input (`crate::win::raw`) and
//! feeds them into its RDP connection.
//!
//! Isolation (architecture.md §3): only under `HostTopology::DedicatedHost` does no other
//! seat's input reach a real session window; under `SeatOneConsole` the console's
//! window manager still gets every keyboard and mouse (`input_not_isolated`). This is
//! **untested on hardware** (HYPOTHESIS in the contract).

use crate::inventory::{Inventory, lock};
use crate::routes::{RouteTable, UnitRoute, interfaces_for_seat};
use crate::units::input_info;
use crate::{EmergencySink, InputControl};
use seat_core::backend::input::{
    ActivityEvent, ActivitySink, HotkeySink, InputBackendKind, InputCaps, InputRoute, InputRouter, PacketSink, RouteReport,
};
use seat_core::backend::session::HostTopology;
use seat_core::backend::{Availability, BackendError, BackendResult, HotplugSink};
use seat_core::config::InputSettings;
use seat_core::device::{Device, DeviceKind};
use seat_core::ids::SeatNumber;
use seat_core::{Clock, SystemClock};
use std::sync::{Arc, Mutex};

/// HID usages a seat host can deliver over RDP (consumer, system control, touch, pen).
pub fn raw_deliverable(page: u16, usage: u16) -> bool {
    matches!((page, usage), (0x0C, 0x01) | (0x01, 0x80) | (0x0D, 0x04) | (0x0D, 0x02))
}

pub struct RawInputRouter {
    inv: Arc<Inventory>,
    topology: HostTopology,
    table: Mutex<RouteTable>,
    settings: Mutex<InputSettings>,
    revision: Mutex<u64>,
    activity: Mutex<Option<ActivitySink>>,
    /// A console listener reports activity / hotkeys / wizard keys (spec 50 §4.1.8).
    reporter: std::sync::atomic::AtomicBool,
    clock: Arc<dyn Clock>,
}

impl RawInputRouter {
    pub fn new(inv: Arc<Inventory>, settings: &InputSettings, topology: HostTopology) -> Arc<Self> {
        Arc::new(Self {
            inv,
            topology,
            table: Mutex::new(RouteTable::default()),
            settings: Mutex::new(settings.clone()),
            revision: Mutex::new(0),
            activity: Mutex::new(None),
            reporter: std::sync::atomic::AtomicBool::new(false),
            clock: Arc::new(SystemClock),
        })
    }
    pub fn settings(&self) -> InputSettings {
        lock(&self.settings).clone()
    }
    fn has_reporter(&self) -> bool {
        self.reporter.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// Units an RDP seat host cannot deliver from raw input (pads, precision touchpad
/// collections, other HID).
fn not_deliverable(d: &Device) -> bool {
    d.kind == DeviceKind::Hid && input_info(d).is_some_and(|i| !raw_deliverable(i.usage_page, i.usage))
}

impl InputRouter for RawInputRouter {
    fn kind(&self) -> InputBackendKind {
        InputBackendKind::RawInputHost
    }
    fn caps(&self) -> InputCaps {
        InputCaps {
            isolates_console: self.topology == HostTopology::DedicatedHost,
            hid_visibility: false,
            // Activity, the hotkey and the wizard need a reader outside session 0: the
            // console listener (`console_report`). Wizard capture is then observe-only
            // (units keep reaching their sessions).
            activity: self.has_reporter(),
            hotkey: self.has_reporter(),
            wizard_capture: self.has_reporter(),
            driver: None,
        }
    }
    fn availability(&self) -> Availability {
        match self.inv.platform().check() {
            Ok(()) => Availability::Available,
            Err(reason) => Availability::Unavailable { reason },
        }
    }
    fn enumerate(&self) -> BackendResult<Vec<Device>> {
        self.inv.enumerate()
    }
    fn apply_routes(&self, routes: &[InputRoute]) -> BackendResult<RouteReport> {
        let table = RouteTable::from_routes(routes);
        let rev = *lock(&self.revision);
        let devices = self.inv.devices();
        let mut report = RouteReport::default();
        let mut first_err: Option<BackendError> = None;
        for d in &devices {
            let mask = table.mask(&d.key);
            if let Some(id) = &d.instance_id
                && let Err(e) = self.inv.platform().set_seat_property(id, mask, rev)
            {
                // The devprop only prepares the (future) filter; raw routing still works.
                log::warn!("seat property on {id}: {e}");
                first_err.get_or_insert(e);
            }
            let delivered = matches!(table.route(&d.key), UnitRoute::Seat(_));
            if delivered && not_deliverable(d) {
                report.not_routable.push(d.key.clone());
            } else if routes.iter().any(|r| r.key == d.key) {
                report.applied.push(d.key.clone());
            }
        }
        if let Some(e) = first_err {
            log::info!("seat properties not fully written ({}); raw input routing is unaffected", e.reason);
        }
        *lock(&self.table) = table;
        Ok(report)
    }
    /// Observe-only with a console listener (it counts F-keys and clicks; nothing is
    /// withheld from the sessions); without one the wizard cannot see the units.
    fn set_wizard_capture(&self, on: bool) -> BackendResult<()> {
        if !on || self.has_reporter() {
            return Ok(());
        }
        Err(BackendError::unsupported("wizard capture with raw_input_host needs the console listener"))
    }
    fn set_hotplug_sink(&self, sink: HotplugSink) {
        self.inv.set_hotplug_sink(sink);
    }
    fn set_activity_sink(&self, sink: ActivitySink) {
        *lock(&self.activity) = Some(sink);
    }
    fn set_packet_sink(&self, _sink: PacketSink) {
        // Packets never pass through the service on this backend.
    }
    fn set_hotkey_sink(&self, _sink: HotkeySink) {}
}

impl InputControl for RawInputRouter {
    fn set_settings(&self, settings: &InputSettings) -> BackendResult<()> {
        *lock(&self.settings) = settings.clone();
        Ok(())
    }
    fn set_config_revision(&self, revision: u64) {
        *lock(&self.revision) = revision;
    }
    fn set_seat_sessions(&self, _sessions: &[(SeatNumber, u32)]) -> BackendResult<()> {
        Ok(())
    }
    fn set_emergency_sink(&self, _sink: EmergencySink) {}
    fn interfaces_for_seat(&self, seat: SeatNumber) -> Vec<String> {
        interfaces_for_seat(&self.inv.devices(), &lock(&self.table), seat)
    }
    fn feed_activity(&self, interface_path: &str, key_sc: Option<u16>, click: bool) {
        let Some(key) = self.inv.key_of_interface(interface_path) else { return };
        let at_ms = self.clock.now_ms();
        self.inv.touch(&key, at_ms);
        if let Some(s) = lock(&self.activity).clone() {
            s(ActivityEvent { key, at_ms, key_sc, click });
        }
    }
    fn set_console_reporter(&self, connected: bool) {
        self.reporter.store(connected, std::sync::atomic::Ordering::SeqCst);
    }
    fn notes(&self) -> Vec<String> {
        let mut n = if self.has_reporter() {
            Vec::new()
        } else {
            vec!["activity_needs_console_listener".to_string(), "hotkey_needs_filter".to_string()]
        };
        if self.topology != HostTopology::DedicatedHost {
            n.insert(0, "input_not_isolated".into());
        }
        n.push("gamepads_shared".into());
        n.push("touch_emulated_as_mouse".into());
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::fake::FakePlatform;
    use crate::units::tests as ut;

    fn router(topology: HostTopology) -> (Arc<RawInputRouter>, Arc<FakePlatform>) {
        let p = Arc::new(FakePlatform::default());
        *p.list.lock().unwrap() = ut::list();
        let inv = Inventory::new(p.clone(), Arc::new(ut::tree()));
        (RawInputRouter::new(inv, &InputSettings::default(), topology), p)
    }

    #[test]
    fn routes_mirror_devprops_and_seat_interfaces() {
        let (r, p) = router(HostTopology::SeatOneConsole);
        let units = r.enumerate().unwrap();
        r.set_config_revision(9);
        let kbd = units.iter().find(|d| d.kind == DeviceKind::Keyboard).unwrap();
        let s2 = SeatNumber::new(2).unwrap();
        let rep = r.apply_routes(&[InputRoute { key: kbd.key.clone(), mask: s2.mask() }]).unwrap();
        assert_eq!(rep.applied, vec![kbd.key.clone()]);
        let props = p.props.lock().unwrap().clone();
        assert_eq!(props.len(), units.len());
        assert!(props.iter().any(|(id, m, rev)| Some(id) == kbd.instance_id.as_ref() && *m == 2 && *rev == 9));
        assert!(props.iter().filter(|(id, ..)| Some(id) != kbd.instance_id.as_ref()).all(|(_, m, _)| *m == 1));
        assert_eq!(r.interfaces_for_seat(s2), vec![ut::KBD_PATH.to_ascii_lowercase()]);
        assert!(r.notes().contains(&"input_not_isolated".to_string()));
        assert!(!r.caps().isolates_console);
    }

    #[test]
    fn devprop_failure_does_not_fail_routing() {
        let (r, p) = router(HostTopology::DedicatedHost);
        r.enumerate().unwrap();
        *p.fail_props.lock().unwrap() = true;
        assert!(r.apply_routes(&[]).is_ok());
        assert!(r.caps().isolates_console);
        assert!(!r.notes().contains(&"input_not_isolated".to_string()));
        assert!(r.set_wizard_capture(true).is_err());
        assert!(r.set_wizard_capture(false).is_ok());
    }

    #[test]
    fn console_reporter_enables_activity_hotkey_and_wizard() {
        let (r, _) = router(HostTopology::SeatOneConsole);
        assert!(!r.caps().activity && !r.caps().hotkey && !r.caps().wizard_capture);
        assert!(r.notes().contains(&"activity_needs_console_listener".to_string()));
        r.set_console_reporter(true);
        let c = r.caps();
        assert!(c.activity && c.hotkey && c.wizard_capture);
        assert!(r.set_wizard_capture(true).is_ok());
        assert!(!r.notes().contains(&"activity_needs_console_listener".to_string()));
        assert!(!r.notes().contains(&"hotkey_needs_filter".to_string()));
        assert!(r.notes().contains(&"input_not_isolated".to_string()));
        r.set_console_reporter(false);
        assert!(r.set_wizard_capture(true).is_err());
    }

    #[test]
    fn activity_feed_maps_paths_to_keys() {
        let (r, _) = router(HostTopology::SeatOneConsole);
        r.enumerate().unwrap();
        let got: Arc<Mutex<Vec<ActivityEvent>>> = Arc::default();
        let g = got.clone();
        r.set_activity_sink(Arc::new(move |e| g.lock().unwrap().push(e)));
        r.feed_activity(ut::MOU_PATH, None, true);
        r.feed_activity(r"\\?\unknown", None, false);
        let got = got.lock().unwrap();
        assert_eq!(got.len(), 1);
        assert!(got[0].click);
        assert!(got[0].key.as_str().starts_with("mou;"));
    }
}
