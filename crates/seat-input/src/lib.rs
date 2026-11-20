// SPDX-License-Identifier: GPL-3.0-only
//! Pulsar Seat input — [`seat_core::backend::InputRouter`] implementations (service side)
//! and the seat host's input path (raw input → [`seat_core::backend::InputSink`]).
//!
//! Owner: the **input** package. Spec: `spec/local/50-input.md` §4, `spec/local/30-sessions.md`
//! §6.1.1 point 6, `spec/local/00-architecture.md` §5.6. Contract: `docs/agents/architecture.md`
//! §4.3. Driver: `drivers/seat-input-filter` (KMDF, unsigned, never loaded).
//!
//! | Module | Side | What |
//! | --- | --- | --- |
//! | [`units`] | service | unit enumeration and keys (spec 50 §4.1.1), pure |
//! | [`inventory`] | service | live inventory, hot-plug diff |
//! | [`routes`] | both | route table, seat interface lists, hotkey groups |
//! | [`raw_router`] | service | `raw_input_host` (default) |
//! | [`kmdf_router`] | service | `kmdf_filter` client (unsigned driver → `driver_not_signed`) |
//! | [`winusb_router`] | service | `winusb_capture`: raw input plus USB keyboard / mouse interfaces of running extra seats on inbox WinUSB (osms G2) |
//! | [`usb_capture`] | service | capture plan, journal, swap / reader state machine of `winusb_capture` |
//! | [`hid_layout`], [`hid_translate`], [`hid_keys`] | service | HID report descriptor parser, report → events, keyboard usage → set-1 (pure) |
//! | [`NoneRouter`] | service | `none`: inventory only |
//! | [`convert`] | both | scan codes, mouse, consumer, touch → [`seat_core::backend::InputPacket`]; hotkey, emergency, SAS |
//! | [`filter_proto`] | service | IOCTL contract of the filter (mirror of the C header) |
//! | [`forward`] | host | per-unit processing into the seat's sink |
//! | [`console_report`] | console listener | activity / hotkey / wizard F-keys of every unit for the service (`raw_input_host`) |
//! | [`kiosk`] | kiosk shell | one kiosk seat's packets → posted window messages + software cursor (spec 35 §5.1 U3), pure |
//! | [`rdp`] | host | [`seat_core::backend::InputSink`] over an RDP client ([`rdp::RdpInputChannel`]) |
//! | [`gamepad`] | both | pad families and their limits |
//! | `local_pads` (feature `local-pads`) | host / service | physical pad routed to a seat -> seat-owned virtual pad (`seat-pads`) |
//! | `win` | both | Windows: CfgMgr32 platform, filter control device, raw input reader, HID parsing |
//!
//! No global low-level hooks, no injection into other sessions, no console-state changes.

pub mod console_report;
pub mod convert;
pub mod filter_proto;
pub mod forward;
pub mod gamepad;
pub mod hid_keys;
pub mod hid_layout;
pub mod hid_translate;
pub mod hotkey_guard;
pub mod inventory;
pub mod kiosk;
pub mod kmdf_router;
#[cfg(feature = "local-pads")]
pub mod local_pads;
pub mod pad_jail;
pub mod pad_scope;
pub mod platform;
pub mod raw_router;
pub mod rdp;
pub mod routes;
pub mod units;
pub mod usb_capture;
#[cfg(windows)]
pub mod win;
pub mod winusb_router;

use inventory::{Inventory, lock};
use platform::{FilterOpener, InputPlatform};
use seat_core::backend::input::{
    ActivitySink, HotkeySink, InputBackendKind, InputCaps, InputRoute, InputRouter, PacketSink, RouteReport,
};
use seat_core::backend::session::HostTopology;
use seat_core::backend::{Availability, BackendResult, HotplugSink};
use seat_core::config::InputSettings;
use seat_core::device::Device;
use seat_core::ids::SeatNumber;
use seat_core::pnp::PnpTree;
use std::sync::{Arc, Mutex};

pub type EmergencySink = Arc<dyn Fn() + Send + Sync>;

/// Service-side controls beyond the [`InputRouter`] contract (every router implements
/// them; the service keeps the [`InputBackend::control`] handle next to the router).
pub trait InputControl: Send + Sync {
    /// New input settings (hotkey, system buttons, unfiltered usages, gap buffer).
    fn set_settings(&self, settings: &InputSettings) -> BackendResult<()>;
    /// Config revision written next to the seat mask (DEVPROPKEY pid 3).
    fn set_config_revision(&self, revision: u64);
    /// Session id of each running seat (HID visibility decisions in the filter).
    fn set_seat_sessions(&self, sessions: &[(SeatNumber, u32)]) -> BackendResult<()>;
    /// Ctrl+Shift+hotkey three times: return every unit to seat 1.
    fn set_emergency_sink(&self, sink: EmergencySink);
    /// Interface paths for `HostAssignment::input_interfaces` (raw input host only).
    fn interfaces_for_seat(&self, seat: SeatNumber) -> Vec<String>;
    /// Activity seen outside the service (a console raw-input listener).
    fn feed_activity(&self, interface_path: &str, key_sc: Option<u16>, click: bool);
    /// Whether a console raw-input listener currently reports to the service
    /// (`raw_input_host`: activity, hotkey and wizard capture then work, see
    /// [`console_report`]). Default: ignored (backends that see input themselves).
    fn set_console_reporter(&self, _connected: bool) {}
    /// Seats that have a session now (`winusb_capture` captures only their units, so a
    /// stopped seat's keyboard returns to the console). Default: ignored.
    fn set_running_seats(&self, _seats: &[SeatNumber]) {}
    /// Give every captured USB unit back to its own driver (`winusb_capture`; service
    /// stop). Returns error texts of units that could not be restored now (they stay
    /// journaled). Default: nothing captured.
    fn release_captures(&self) -> Vec<String> {
        Vec::new()
    }
    /// Snake_case notes for `BackendInfo::notes` (`input_not_isolated`, …).
    fn notes(&self) -> Vec<String>;
}

/// What [`create`] returns: the trait object plus its controls (same object).
#[derive(Clone)]
pub struct InputBackend {
    pub router: Arc<dyn InputRouter>,
    pub control: Arc<dyn InputControl>,
}

/// Build the selected backend on this PC.
pub fn create(
    kind: InputBackendKind,
    pnp: Arc<dyn PnpTree>,
    settings: &InputSettings,
    topology: HostTopology,
) -> InputBackend {
    create_with(kind, pnp, settings, topology, default_platform(), default_opener())
}

/// [`create`] for `winusb_capture` with the service's shared [`usb_capture::UsbCapture`]
/// (one per service, so router rebuilds keep the captured nodes and their journal).
/// Other kinds ignore `capture`.
pub fn create_with_capture(
    kind: InputBackendKind,
    pnp: Arc<dyn PnpTree>,
    settings: &InputSettings,
    topology: HostTopology,
    capture: Arc<usb_capture::UsbCapture>,
) -> InputBackend {
    if kind == InputBackendKind::WinusbCapture {
        let inv = Inventory::new(default_platform(), pnp);
        let r = winusb_router::WinusbCaptureRouter::new(inv, settings, topology, capture);
        return InputBackend { router: r.clone(), control: r };
    }
    create(kind, pnp, settings, topology)
}

/// [`create`] with explicit OS seams (tests, diagnostics). `winusb_capture` gets a capture
/// state over the default journal folder (`%ProgramData%\Pulsar Seat\journal`); the
/// service uses [`create_with_capture`] instead.
pub fn create_with(
    kind: InputBackendKind,
    pnp: Arc<dyn PnpTree>,
    settings: &InputSettings,
    topology: HostTopology,
    platform: Arc<dyn InputPlatform>,
    opener: Arc<dyn FilterOpener>,
) -> InputBackend {
    let inv = Inventory::new(platform, pnp);
    match kind {
        InputBackendKind::WinusbCapture => {
            let cap = usb_capture::UsbCapture::system(&usb_capture::default_journal_dir());
            let r = winusb_router::WinusbCaptureRouter::new(inv, settings, topology, cap);
            InputBackend { router: r.clone(), control: r }
        }
        InputBackendKind::RawInputHost => {
            let r = raw_router::RawInputRouter::new(inv, settings, topology);
            InputBackend { router: r.clone(), control: r }
        }
        InputBackendKind::KmdfFilter => {
            let r = kmdf_router::KmdfFilterRouter::new(inv, opener, settings);
            InputBackend { router: r.clone(), control: r }
        }
        InputBackendKind::None => {
            let r = Arc::new(NoneRouter { inv, activity: Mutex::new(None) });
            InputBackend { router: r.clone(), control: r }
        }
    }
}

fn default_platform() -> Arc<dyn InputPlatform> {
    #[cfg(windows)]
    {
        Arc::new(win::WinPlatform)
    }
    #[cfg(not(windows))]
    {
        Arc::new(platform::NullPlatform)
    }
}

fn default_opener() -> Arc<dyn FilterOpener> {
    #[cfg(windows)]
    {
        Arc::new(win::WinFilterOpener)
    }
    #[cfg(not(windows))]
    {
        Arc::new(platform::NullFilterOpener)
    }
}

/// `BackendInfo::notes` for a kind without building it (manager backend page).
pub fn backend_notes(kind: InputBackendKind, topology: HostTopology) -> Vec<String> {
    match kind {
        InputBackendKind::RawInputHost => {
            let mut n = Vec::new();
            if topology != HostTopology::DedicatedHost {
                n.push("input_not_isolated".to_string());
            }
            // No listener notes here: the service starts the console listener itself with
            // this backend, so activity and the hotkey work. The live router adds
            // `activity_needs_console_listener` only while no listener reports.
            n.extend(["gamepads_shared", "touch_emulated_as_mouse"].map(String::from));
            n
        }
        InputBackendKind::KmdfFilter => {
            vec!["driver_not_signed".into(), "xinput_pads_shared".into(), "touch_emulated_as_mouse".into()]
        }
        InputBackendKind::WinusbCapture => winusb_router::static_notes(topology),
        InputBackendKind::None => vec!["input_not_routed".into()],
    }
}

/// `none`: devices are listed, nothing is routed (fast user switching, or the signing
/// fallback of spec 50 §4.2).
pub struct NoneRouter {
    inv: Arc<Inventory>,
    activity: Mutex<Option<ActivitySink>>,
}

impl InputRouter for NoneRouter {
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
        self.inv.enumerate()
    }
    fn apply_routes(&self, routes: &[InputRoute]) -> BackendResult<RouteReport> {
        let not_routable = routes
            .iter()
            .filter(|r| !matches!(routes::UnitRoute::of(r.mask), routes::UnitRoute::Console))
            .map(|r| r.key.clone())
            .collect();
        Ok(RouteReport { not_routable, ..Default::default() })
    }
    fn set_wizard_capture(&self, _on: bool) -> BackendResult<()> {
        Err(seat_core::backend::BackendError::unsupported("wizard capture without input routing"))
    }
    fn set_hotplug_sink(&self, sink: HotplugSink) {
        self.inv.set_hotplug_sink(sink);
    }
    fn set_activity_sink(&self, sink: ActivitySink) {
        *lock(&self.activity) = Some(sink);
    }
    fn set_packet_sink(&self, _sink: PacketSink) {}
    fn set_hotkey_sink(&self, _sink: HotkeySink) {}
}

impl InputControl for NoneRouter {
    fn set_settings(&self, _settings: &InputSettings) -> BackendResult<()> {
        Ok(())
    }
    fn set_config_revision(&self, _revision: u64) {}
    fn set_seat_sessions(&self, _sessions: &[(SeatNumber, u32)]) -> BackendResult<()> {
        Ok(())
    }
    fn set_emergency_sink(&self, _sink: EmergencySink) {}
    fn interfaces_for_seat(&self, _seat: SeatNumber) -> Vec<String> {
        Vec::new()
    }
    fn feed_activity(&self, interface_path: &str, key_sc: Option<u16>, click: bool) {
        let Some(key) = self.inv.key_of_interface(interface_path) else { return };
        if let Some(s) = lock(&self.activity).clone() {
            s(seat_core::backend::input::ActivityEvent { key, at_ms: 0, key_sc, click });
        }
    }
    fn notes(&self) -> Vec<String> {
        backend_notes(InputBackendKind::None, HostTopology::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::fake::FakePlatform;
    use crate::platform::NullFilterOpener;
    use crate::units::tests as ut;
    use seat_core::ids::SeatMask;

    fn backend(kind: InputBackendKind) -> InputBackend {
        let p = Arc::new(FakePlatform::default());
        *p.list.lock().unwrap() = ut::list();
        create_with(kind, Arc::new(ut::tree()), &InputSettings::default(), HostTopology::SeatOneConsole, p, Arc::new(NullFilterOpener))
    }

    #[test]
    fn factory_kinds() {
        for k in [InputBackendKind::RawInputHost, InputBackendKind::KmdfFilter, InputBackendKind::None] {
            let b = backend(k);
            assert_eq!(b.router.kind(), k);
            assert_eq!(b.router.enumerate().unwrap().len(), 5);
        }
        assert_eq!(
            backend(InputBackendKind::KmdfFilter).router.availability(),
            Availability::DriverNotSigned { driver: seat_core::backend::DriverId::KbdMouFilter }
        );
    }

    #[test]
    fn none_router_routes_nothing() {
        let b = backend(InputBackendKind::None);
        let d = b.router.enumerate().unwrap();
        let rep = b
            .router
            .apply_routes(&[
                InputRoute { key: d[0].key.clone(), mask: SeatMask(2) },
                InputRoute { key: d[1].key.clone(), mask: SeatMask::CONSOLE },
            ])
            .unwrap();
        assert_eq!(rep.not_routable, vec![d[0].key.clone()]);
        assert_eq!(b.control.notes(), vec!["input_not_routed".to_string()]);
    }

    #[test]
    fn notes_by_topology() {
        assert_eq!(backend_notes(InputBackendKind::RawInputHost, HostTopology::SeatOneConsole)[0], "input_not_isolated");
        assert!(!backend_notes(InputBackendKind::RawInputHost, HostTopology::DedicatedHost).contains(&"input_not_isolated".into()));
    }
}
