// SPDX-License-Identifier: GPL-3.0-only
//! USB keyboard / mouse isolation without a driver of ours (osms G2, `winusb_capture`).
//!
//! An extra seat's USB keyboard or mouse interface is moved from the HID stack to the
//! **inbox** `winusb.inf` (Microsoft's WinUSB driver): the console's keyboard / mouse class
//! drivers never see it again, so nothing of it reaches seat 1. The service then reads the
//! interface's interrupt-IN pipe itself (WinUSB), decodes the HID reports
//! ([`crate::hid_layout`], [`crate::hid_translate`]) and sends the packets to the seat
//! host like the KMDF filter path does. No kernel code of ours, no class-wide filter, no
//! third-party driver; works with Memory Integrity and Secure Boot on.
//!
//! This module holds the pure parts and the state machine:
//!
//! - [`groups`]: which USB devnode (the interface node `USB\…&MI_xx`, or the device node
//!   of a single-interface device) each USB input unit sits on. Swapping that node takes
//!   **every** HID collection under it, so units are grouped by it.
//! - [`plan`]: which groups to capture for the current routes (one seat ≥ 2, running, no
//!   foreign collections, never the last keyboard or mouse the console still has).
//! - [`CaptureJournal`] / [`FileJournal`]: `%ProgramData%\Pulsar Seat\journal\usb-capture.json`,
//!   written **before** each swap (temporary file + `MoveFileExW(REPLACE_EXISTING |
//!   WRITE_THROUGH)`), so a crash or power loss always leaves enough to restore.
//! - [`UsbCapture`]: captures / releases nodes, runs one reader thread per captured node,
//!   restores everything at service start ([`restore_at_start`]), at service stop, when a
//!   seat stops, when the backend changes, on the manual "restore input devices" command
//!   and at uninstall. Nodes that still carry our interface GUID but are missing from the
//!   journal (journal lost) are restored too.
//!
//! The OS seams ([`UsbSwap`], [`UsbReportSource`]) have Windows implementations in
//! `win::usb_swap` and `win::winusb_reader`. **Everything that touches a device is untested
//! on hardware (HYPOTHESIS)**; the tests here run against fakes.

use crate::convert::{Outcome, RawUnitEvent, UnitOptions, UnitProcessor, split};
use crate::hid_layout::{self, ReportLayout};
use crate::hid_translate::ReportTranslator;
use crate::inventory::lock;
use crate::platform::{ChangeCallback, Guard, is_fatal_win32};
use crate::routes::{RouteTable, UnitRoute};
use crate::units::input_info;
use seat_core::backend::input::InputPacket;
use seat_core::backend::{BackendError, BackendResult, ErrorKind};
use seat_core::device::{Device, DeviceKind};
use seat_core::ids::{DeviceKey, SeatNumber};
use seat_core::pnp::{PnpNode, PnpTree};
use seat_core::{Clock, SystemClock};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Device interface GUID we register on a captured node (`Device Parameters\
/// DeviceInterfaceGUIDs`), so the reader can find it and the start-up sweep can find
/// nodes a lost journal no longer lists. Ours; used for nothing else.
pub const INTERFACE_GUID: &str = "{6f1d3a52-9c47-4b8e-a5d0-2e7c41b98f36}";
/// Setup class `USBDevice` (inbox `winusb.inf`).
pub const USB_DEVICE_CLASS: &str = "{88bae032-5a81-49f0-bc3d-a4ff138216d6}";
/// Setup class `HIDClass` (inbox `input.inf`), the restore fallback.
pub const HID_CLASS: &str = "{745a17a0-74d3-11d0-b6fe-00a0c90f57da}";
/// Journal file name under `%ProgramData%\Pulsar Seat\journal`.
pub const JOURNAL_FILE: &str = "usb-capture.json";
/// How long [`UsbCapture`] waits for a reader thread to stop.
pub const STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// The driver a node had before we captured it (or has now, [`UsbSwap::current`]).
///
/// e2e gaming PC 2026-11-24 (bug 5): a Logitech receiver interface ran the vendor package
/// `oem128.inf` (section `LogiLDA`, 1.10.80.0) plus the extension INF `oem185.inf`, whose
/// software component hangs below the node. The restore must put back that exact package
/// and re-apply the extension, so everything Windows reports for the node is journaled.
/// Fields after `inf_path` were added then (additive: older journals load with them empty,
/// and an empty field is not checked).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OriginalDriver {
    /// Setup class GUID with braces, lowercase.
    pub class_guid: String,
    /// INF of the installed driver as Windows names it in `%windir%\INF` (`input.inf`,
    /// `oem12.inf`; `DEVPKEY_Device_DriverInfPath`). Empty = unknown (lost journal): the
    /// restore lets Windows rank the drivers in its store.
    pub inf_path: String,
    /// Install section (`DEVPKEY_Device_DriverInfSection`, e.g. `LogiLDA`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub inf_section: String,
    /// Driver version (`DEVPKEY_Device_DriverVersion`, e.g. `1.10.80.0`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub driver_version: String,
    /// Provider (`DEVPKEY_Device_DriverProvider`). Diagnostics only.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub provider: String,
    /// Matching device id (`DEVPKEY_Device_MatchingDeviceId`). Diagnostics only.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub matching_id: String,
    /// Extension INFs applied to the node, as `DEVPKEY_Device_ExtendedConfigurationIds`
    /// lists them (`oem185.inf:USB\VID_046D&PID_C52B&MI_01,<section>,<date>,<version>`).
    /// Software components of an extension come back with it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<String>,
}

impl OriginalDriver {
    /// What a USB HID interface normally runs: `HIDClass` from the inbox `input.inf`.
    pub fn hid_default() -> Self {
        Self { class_guid: HID_CLASS.into(), inf_path: "input.inf".into(), ..Self::default() }
    }

    /// Original driver not known (a node that carries our interface but has no journal
    /// entry): `HIDClass` and the best driver Windows ranks from its store, never a fixed
    /// `input.inf` (that replaced a vendor driver on the gaming PC, bug 5).
    pub fn unknown() -> Self {
        Self { class_guid: HID_CLASS.into(), ..Self::default() }
    }

    /// The INF file names of [`Self::extensions`] (bare `oemNN.inf` names only, deduplicated).
    pub fn extension_infs(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for e in &self.extensions {
            let Some((inf, _)) = e.split_once(':') else { continue };
            let inf = inf.trim();
            if !is_bare_inf_name(inf) || out.iter().any(|o| o.eq_ignore_ascii_case(inf)) {
                continue;
            }
            out.push(inf.to_string());
        }
        out
    }

    /// One line for the log: `oem128.inf [LogiLDA] 1.10.80.0 Logitech; extensions: oem185.inf`.
    pub fn describe(&self) -> String {
        let mut s = if self.inf_path.is_empty() { "(best ranked driver)".to_string() } else { self.inf_path.clone() };
        if !self.inf_section.is_empty() {
            s.push_str(&format!(" [{}]", self.inf_section));
        }
        for v in [&self.driver_version, &self.provider] {
            if !v.is_empty() {
                s.push(' ');
                s.push_str(v);
            }
        }
        let ext = self.extension_infs();
        if !ext.is_empty() {
            s.push_str(&format!("; extensions: {}", ext.join(", ")));
        }
        s
    }
}

/// A bare INF file name as Windows records it (`input.inf`, `oem12.inf`): no path.
pub fn is_bare_inf_name(name: &str) -> bool {
    !name.is_empty() && !name.contains(['\\', '/', ':']) && name.to_ascii_lowercase().ends_with(".inf")
}

/// What differs between the journaled driver `want` and the driver on the node `now` after
/// a restore (empty = exact). Empty fields of `want` are not checked; an unknown original
/// only has to be off WinUSB. Logged, and kept for the notes (`usb_restore_inexact`).
pub fn restore_differences(want: &OriginalDriver, now: &OriginalDriver) -> Vec<String> {
    let mut d = Vec::new();
    if now.class_guid.eq_ignore_ascii_case(USB_DEVICE_CLASS) {
        d.push("still in the USBDevice class (WinUSB)".to_string());
        return d;
    }
    if want.inf_path.is_empty() {
        return d;
    }
    if !want.class_guid.is_empty() && !want.class_guid.eq_ignore_ascii_case(&now.class_guid) {
        d.push(format!("class {} instead of {}", now.class_guid, want.class_guid));
    }
    if !want.inf_path.eq_ignore_ascii_case(&now.inf_path) {
        d.push(format!("driver {} instead of {}", now.inf_path, want.inf_path));
    }
    if !want.inf_section.is_empty() && !want.inf_section.eq_ignore_ascii_case(&now.inf_section) {
        d.push(format!("section {} instead of {}", now.inf_section, want.inf_section));
    }
    if !want.driver_version.is_empty() && want.driver_version != now.driver_version {
        d.push(format!("version {} instead of {}", now.driver_version, want.driver_version));
    }
    let have = now.extension_infs();
    for e in want.extension_infs() {
        if !have.iter().any(|h| h.eq_ignore_ascii_case(&e)) {
            d.push(format!("extension {e} missing"));
        }
    }
    d
}

/// One captured node in the journal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaptureEntry {
    /// Instance id of the swapped USB devnode.
    pub node: String,
    pub seat: u8,
    pub original: OriginalDriver,
    /// The input units that sat on the node (shown as present while captured).
    #[serde(default)]
    pub units: Vec<Device>,
    #[serde(default)]
    pub at_ms: u64,
}

/// `usb-capture.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaptureJournal {
    pub version: u32,
    #[serde(default)]
    pub entries: Vec<CaptureEntry>,
}

impl Default for CaptureJournal {
    fn default() -> Self {
        Self { version: 1, entries: Vec::new() }
    }
}

/// Where the journal lives.
pub trait JournalStore: Send + Sync {
    /// Unreadable or missing = empty (logged).
    fn load(&self) -> CaptureJournal;
    fn save(&self, j: &CaptureJournal) -> std::io::Result<()>;
}

/// The journal file (atomic replace).
pub struct FileJournal {
    pub path: PathBuf,
}

impl FileJournal {
    pub fn in_dir(journal_dir: &Path) -> Self {
        Self { path: journal_dir.join(JOURNAL_FILE) }
    }
}

/// `%ProgramData%\Pulsar Seat\journal`.
pub fn default_journal_dir() -> PathBuf {
    std::env::var_os("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
        .join(seat_core::consts::PRODUCT_DIR)
        .join(seat_core::consts::JOURNAL_DIR)
}

impl JournalStore for FileJournal {
    fn load(&self) -> CaptureJournal {
        match std::fs::read(&self.path) {
            Ok(b) => serde_json::from_slice(&b).unwrap_or_else(|e| {
                log::warn!("usb capture journal {} unreadable ({e}); starting empty", self.path.display());
                CaptureJournal::default()
            }),
            Err(_) => CaptureJournal::default(),
        }
    }

    /// Temporary file created new (a stale one is removed first, never followed), flushed,
    /// then moved over the journal.
    fn save(&self, j: &CaptureJournal) -> std::io::Result<()> {
        use std::io::Write;
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        let body = serde_json::to_vec_pretty(j).map_err(std::io::Error::other)?;
        if std::fs::symlink_metadata(&tmp).is_ok() {
            std::fs::remove_file(&tmp)?;
        }
        {
            let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
            f.write_all(&body)?;
            f.sync_all()?;
        }
        replace_file(&tmp, &self.path)
    }
}

#[cfg(windows)]
fn replace_file(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW};
    let w = |p: &Path| p.as_os_str().encode_wide().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let (a, b) = (w(from), w(to));
    // SAFETY: NUL-terminated paths valid for the call.
    if unsafe { MoveFileExW(a.as_ptr(), b.as_ptr(), MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(windows))]
fn replace_file(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::rename(from, to)
}

/// In-memory journal (tests, console dev runs).
#[derive(Default)]
pub struct MemoryJournal {
    pub doc: Mutex<CaptureJournal>,
    pub saves: Mutex<u32>,
    pub fail: Mutex<bool>,
}

impl JournalStore for MemoryJournal {
    fn load(&self) -> CaptureJournal {
        lock(&self.doc).clone()
    }
    fn save(&self, j: &CaptureJournal) -> std::io::Result<()> {
        if *lock(&self.fail) {
            return Err(std::io::Error::other("journal write refused (test)"));
        }
        *lock(&self.doc) = j.clone();
        *lock(&self.saves) += 1;
        Ok(())
    }
}

/// Driver swap of one USB devnode (Windows: `win::usb_swap::WinUsbSwap`, SetupAPI +
/// `newdev!DiInstallDevice`).
pub trait UsbSwap: Send + Sync {
    /// `Err(reason)` when swapping cannot work here (no `winusb.inf`, no `newdev.dll`).
    fn check(&self) -> Result<(), String>;
    /// The node's current driver package (class, INF, section, version, extensions),
    /// journaled before [`UsbSwap::capture`]. `Err` when the node already runs WinUSB.
    fn original(&self, node: &str) -> BackendResult<OriginalDriver>;
    /// What the node runs now, whatever it is (the check after a restore). `None` when it
    /// cannot be read (node gone).
    fn current(&self, node: &str) -> Option<OriginalDriver>;
    /// Register [`INTERFACE_GUID`], set the class to `USBDevice` and install `winusb.inf`
    /// on this node only.
    fn capture(&self, node: &str) -> BackendResult<()>;
    /// Put `original` back on the node: the same INF and install section (falls back to the
    /// best driver Windows ranks), then the journaled extension INFs that did not come back
    /// by themselves, and remove our interface GUID value. An empty `inf_path` installs the
    /// best ranked driver. `Err(NotFound)` while the device is unplugged.
    fn release(&self, node: &str, original: &OriginalDriver) -> BackendResult<()>;
    /// Devnodes (present or not) with a registration of [`INTERFACE_GUID`]. Windows keeps a
    /// device interface registered after the driver that registered it is gone, so this
    /// list also holds nodes restored long ago: [`UsbSwap::still_captured`] decides.
    fn captured_nodes(&self) -> Vec<String>;
    /// The node still carries our capture: our GUID in its `DeviceInterfaceGUIDs` value or
    /// the `USBDevice` class. A node that is back on its own driver is not ours any more
    /// (e2e gaming PC 2026-11-24, bug 5: the sweep reinstalled `input.inf` over the vendor
    /// driver a release had just put back, at every later start too).
    fn still_captured(&self, node: &str) -> bool;
    /// Call `cb` when a node with our interface arrives (replug of a captured device).
    fn watch(&self, cb: ChangeCallback) -> BackendResult<Box<dyn Guard>>;
}

/// Opens a captured node's WinUSB interface (Windows: `win::winusb_reader`).
pub trait UsbReportSource: Send + Sync {
    fn open(&self, node: &str) -> BackendResult<Arc<dyn UsbReportReader>>;
}

/// One open WinUSB interface.
pub trait UsbReportReader: Send + Sync {
    /// The HID report descriptor (`GET_DESCRIPTOR` 0x22), `None` when the device did not
    /// answer (the boot-protocol layout is used then).
    fn report_descriptor(&self) -> Option<Vec<u8>>;
    /// Blocks until the next input report. `Err(cancelled)` after [`Self::cancel`].
    fn read(&self) -> BackendResult<Vec<u8>>;
    /// Sticky cancel of the current and every later read.
    fn cancel(&self);
}

// ---------------------------------------------------------------------------------------
// Pure planning

/// Input units sharing one USB devnode.
#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    pub node: String,
    pub units: Vec<Device>,
}

/// Why a seat-routed USB unit is not captured (also the snake_case note).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Refusal {
    /// The node's units are routed to different seats, shared, or partly to seat 1.
    MixedSeats,
    /// Capturing would leave the console without any keyboard or mouse.
    LastConsoleInput,
    /// The node also carries a collection we cannot translate (pad, touch, pen).
    UnsupportedCollection,
}

impl Refusal {
    pub fn note(self) -> &'static str {
        match self {
            Self::MixedSeats => "usb_capture_mixed_seats",
            Self::LastConsoleInput => "usb_capture_last_console_input",
            Self::UnsupportedCollection => "usb_capture_unsupported_collection",
        }
    }
}

/// Plan result: node → (seat, units) to hold captured, and refused units.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Plan {
    pub capture: BTreeMap<String, (SeatNumber, Vec<Device>)>,
    pub refused: Vec<(DeviceKey, Refusal)>,
}

fn is_hub(n: &PnpNode) -> bool {
    n.hardware_ids.iter().chain(&n.compatible_ids).any(|h| {
        let u = h.to_ascii_uppercase();
        u.contains("ROOT_HUB") || u.contains("USB\\CLASS_09")
    })
}

/// The USB devnode whose driver a swap replaces for this unit: the HID collection's
/// direct parent when that is a USB (interface or device) node and not a hub.
pub fn usb_node_of(tree: &dyn PnpTree, d: &Device) -> Option<String> {
    let info = input_info(d)?;
    if info.bus != "usb" {
        return None;
    }
    let owner = tree.node(d.instance_id.as_deref()?)?;
    let parent = tree.node(owner.parent.as_deref()?)?;
    (parent.enumerator.eq_ignore_ascii_case("USB") && !is_hub(&parent)).then_some(parent.instance_id)
}

/// Group the present USB input units by their swap node (sorted by node).
pub fn groups(tree: &dyn PnpTree, devices: &[Device]) -> Vec<Group> {
    let mut by: BTreeMap<String, Vec<Device>> = BTreeMap::new();
    for d in devices.iter().filter(|d| d.present && matches!(d.kind, DeviceKind::Keyboard | DeviceKind::Mouse | DeviceKind::Hid)) {
        if let Some(n) = usb_node_of(tree, d) {
            by.entry(n).or_default().push(d.clone());
        }
    }
    by.into_iter().map(|(node, units)| Group { node, units }).collect()
}

/// HID collections the reader translates besides keyboards and mice: consumer control and
/// system control (they travel with the keyboard / mouse on the same interface).
fn translatable_hid(d: &Device) -> bool {
    input_info(d).is_some_and(|i| matches!((i.usage_page, i.usage), (0x0C, 0x01) | (0x01, 0x80)))
}

/// Decide what to capture. `groups` must contain every capturable group (live ones from
/// [`groups`] plus the units of nodes captured now, which no longer enumerate as HID);
/// `uncaptured_inputs` = every present keyboard / mouse unit that is not on any of
/// `groups`' nodes (Bluetooth, built-in, PS/2): they keep the console usable.
pub fn plan(groups: &[Group], table: &RouteTable, running: &BTreeSet<SeatNumber>, wizard: bool, uncaptured_inputs: &[Device]) -> Plan {
    let mut out = Plan::default();
    if wizard {
        return out;
    }
    let count = |units: &[Device], k: DeviceKind| units.iter().filter(|d| d.kind == k).count();
    let mut console_kbd = count(uncaptured_inputs, DeviceKind::Keyboard);
    let mut console_mouse = count(uncaptured_inputs, DeviceKind::Mouse);
    // Units of groups we do not capture stay with the console too.
    let mut candidates: Vec<(&Group, SeatNumber)> = Vec::new();
    for g in groups {
        let mut seats: Vec<UnitRoute> = Vec::new();
        for d in g.units.iter().filter(|d| matches!(d.kind, DeviceKind::Keyboard | DeviceKind::Mouse)) {
            let r = table.route(&d.key);
            if !seats.contains(&r) {
                seats.push(r);
            }
        }
        let target = match seats.as_slice() {
            [UnitRoute::Seat(s)] => Some(*s),
            _ => None,
        };
        let any_extra = g.units.iter().any(|d| matches!(table.route(&d.key), UnitRoute::Seat(_) | UnitRoute::Shared(_)));
        let refuse = |out: &mut Plan, why: Refusal| {
            for d in g.units.iter().filter(|d| !matches!(table.route(&d.key), UnitRoute::Console | UnitRoute::Spare)) {
                out.refused.push((d.key.clone(), why));
            }
        };
        let Some(seat) = target else {
            if any_extra {
                refuse(&mut out, Refusal::MixedSeats);
            }
            console_kbd += count(&g.units, DeviceKind::Keyboard);
            console_mouse += count(&g.units, DeviceKind::Mouse);
            continue;
        };
        // Other collections: consumer / system follow the group when they are on seat 1
        // (the default) or on the same seat; anything else blocks the swap.
        let mut blocked = None;
        for d in g.units.iter().filter(|d| d.kind == DeviceKind::Hid) {
            let r = table.route(&d.key);
            if !translatable_hid(d) {
                blocked = Some(Refusal::UnsupportedCollection);
            } else if !matches!(r, UnitRoute::Console) && r != UnitRoute::Seat(seat) {
                blocked.get_or_insert(Refusal::MixedSeats);
            }
        }
        if let Some(why) = blocked {
            refuse(&mut out, why);
            console_kbd += count(&g.units, DeviceKind::Keyboard);
            console_mouse += count(&g.units, DeviceKind::Mouse);
            continue;
        }
        if !running.contains(&seat) {
            // Not a refusal: the units stay on raw input until the seat has a session.
            console_kbd += count(&g.units, DeviceKind::Keyboard);
            console_mouse += count(&g.units, DeviceKind::Mouse);
            continue;
        }
        candidates.push((g, seat));
    }
    for (g, seat) in candidates {
        out.capture.insert(g.node.clone(), (seat, g.units.clone()));
    }
    // Never the last keyboard / mouse the console has: drop captures from the end (node
    // order) while the console would have no keyboard (or mouse) left. A PC that has no
    // keyboard at all is not made worse by us, so a total of zero passes.
    let mut kept: Vec<String> = out.capture.keys().cloned().collect();
    let has =
        |kind: DeviceKind, nodes: &[String], cap: &BTreeMap<String, (SeatNumber, Vec<Device>)>| nodes.iter().map(|n| count(&cap[n].1, kind)).sum::<usize>();
    let total_kbd = console_kbd + has(DeviceKind::Keyboard, &kept, &out.capture);
    let total_mouse = console_mouse + has(DeviceKind::Mouse, &kept, &out.capture);
    while let Some(last) = kept.last().cloned() {
        let cap_k = has(DeviceKind::Keyboard, &kept, &out.capture);
        let cap_m = has(DeviceKind::Mouse, &kept, &out.capture);
        let kbd_ok = total_kbd == 0 || total_kbd > cap_k;
        let mouse_ok = total_mouse == 0 || total_mouse > cap_m;
        if kbd_ok && mouse_ok {
            break;
        }
        // Drop the last group that holds the missing kind.
        let need_k = !kbd_ok;
        let victim = kept
            .iter()
            .rev()
            .find(|n| {
                let u = &out.capture[*n].1;
                (need_k && count(u, DeviceKind::Keyboard) > 0) || (!mouse_ok && count(u, DeviceKind::Mouse) > 0)
            })
            .cloned()
            .unwrap_or(last);
        kept.retain(|n| *n != victim);
        if let Some((_, units)) = out.capture.remove(&victim) {
            for d in units.iter().filter(|d| table.route(&d.key) != UnitRoute::Console) {
                out.refused.push((d.key.clone(), Refusal::LastConsoleInput));
            }
        }
    }
    out.refused.sort_by(|a, b| a.0.cmp(&b.0));
    out.refused.dedup();
    out
}

// ---------------------------------------------------------------------------------------
// State machine

/// Receives what the readers produce (the `winusb_capture` router).
pub trait CaptureHandler: Send + Sync {
    fn packets(&self, seat: SeatNumber, packets: &[InputPacket]);
    fn hotkey(&self, key: &DeviceKey, seat: SeatNumber);
    fn emergency(&self);
    fn activity(&self, key: &DeviceKey);
    fn options(&self) -> UnitOptions;
}

struct Worker {
    stop: Arc<AtomicBool>,
    reader: Arc<Mutex<Option<Arc<dyn UsbReportReader>>>>,
    thread: Option<JoinHandle<()>>,
}

impl Worker {
    fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let Some(h) = self.thread.take() else { return };
        let t = Instant::now();
        loop {
            if let Some(r) = lock(&self.reader).clone() {
                r.cancel();
            }
            if h.is_finished() {
                let _ = h.join();
                return;
            }
            if t.elapsed() >= STOP_TIMEOUT {
                log::error!("USB capture reader did not stop within {STOP_TIMEOUT:?}; left detached");
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

struct Active {
    entry: CaptureEntry,
    worker: Option<Worker>,
}

/// Captured nodes, their journal and reader threads. One per service (shared by every
/// `winusb_capture` router instance the factory builds).
pub struct UsbCapture {
    swap: Arc<dyn UsbSwap>,
    source: Arc<dyn UsbReportSource>,
    journal: Arc<dyn JournalStore>,
    clock: Arc<dyn Clock>,
    active: Mutex<BTreeMap<String, Active>>,
    /// Released in our state but not yet on the device (unplugged): retried.
    pending: Mutex<Vec<CaptureEntry>>,
    handler: Arc<Mutex<Option<Weak<dyn CaptureHandler>>>>,
    watch: Mutex<Option<Box<dyn Guard>>>,
    /// Serialises sync / restore (a swap takes about a second).
    op: Mutex<()>,
    /// The operator's restore holds captures off ([`Self::hold`]) until [`Self::resume`].
    held: AtomicBool,
    /// `<node>: <differences>` of restores that did not bring back the exact original
    /// driver ([`restore_differences`]), newest last, at most [`MAX_INEXACT`].
    inexact: Mutex<Vec<String>>,
}

/// How many inexact restores [`UsbCapture::inexact_restores`] keeps.
const MAX_INEXACT: usize = 16;

impl UsbCapture {
    pub fn new(swap: Arc<dyn UsbSwap>, source: Arc<dyn UsbReportSource>, journal: Arc<dyn JournalStore>) -> Arc<Self> {
        Self::with_clock(swap, source, journal, Arc::new(SystemClock))
    }

    pub fn with_clock(swap: Arc<dyn UsbSwap>, source: Arc<dyn UsbReportSource>, journal: Arc<dyn JournalStore>, clock: Arc<dyn Clock>) -> Arc<Self> {
        // What an earlier run (or instance) journaled and nobody restored yet is pending
        // from the start: it is retried with the next sync and never dropped from the
        // journal by a later save.
        let pending = journal.load().entries;
        Arc::new(Self {
            swap,
            source,
            journal,
            clock,
            active: Mutex::new(BTreeMap::new()),
            pending: Mutex::new(pending),
            handler: Arc::new(Mutex::new(None)),
            watch: Mutex::new(None),
            op: Mutex::new(()),
            held: AtomicBool::new(false),
            inexact: Mutex::new(Vec::new()),
        })
    }

    /// The operator's "restore input devices" (e2e gaming PC 2026-11-24: the restore was
    /// undone 1-3 s later by the next route apply while seat 2 still ran): from now on
    /// [`Self::sync`] only releases, until [`Self::resume`] (a seat starts, the input backend
    /// is chosen again, the service restarts).
    pub fn hold(&self) {
        if !self.held.swap(true, Ordering::SeqCst) {
            log::info!("USB capture held: nothing is captured again until a seat starts or the input backend is chosen again");
        }
    }

    /// End [`Self::hold`]; the next sync captures again.
    pub fn resume(&self) {
        if self.held.swap(false, Ordering::SeqCst) {
            log::info!("USB capture resumed");
        }
    }

    pub fn is_held(&self) -> bool {
        self.held.load(Ordering::SeqCst)
    }

    /// Restores that did not bring back the exact original driver (`<node>: <what differs>`).
    pub fn inexact_restores(&self) -> Vec<String> {
        lock(&self.inexact).clone()
    }

    /// Compare what the node runs after a restore with what was journaled; log either way
    /// (round 2 of the gaming PC test reads these lines).
    fn verify_restore(&self, node: &str, want: &OriginalDriver) {
        let Some(now) = self.swap.current(node) else {
            log::info!("USB input node {node}: driver after the restore not readable (node absent)");
            return;
        };
        let d = restore_differences(want, &now);
        if d.is_empty() {
            log::info!("USB input node {node} restore check: exact ({})", now.describe());
            return;
        }
        log::warn!("USB input node {node} restore check: NOT exact: {}; journaled {}, now {}", d.join("; "), want.describe(), now.describe());
        let mut v = lock(&self.inexact);
        v.retain(|x| !x.starts_with(&format!("{node}: ")));
        v.push(format!("{node}: {}", d.join("; ")));
        let over = v.len().saturating_sub(MAX_INEXACT);
        v.drain(..over);
    }

    /// Windows parts over the journal in `journal_dir`.
    #[cfg(windows)]
    pub fn system(journal_dir: &Path) -> Arc<Self> {
        Self::new(Arc::new(crate::win::usb_swap::WinUsbSwap), Arc::new(crate::win::winusb_reader::WinUsbSource), Arc::new(FileJournal::in_dir(journal_dir)))
    }

    /// Non-Windows builds: nothing can be captured.
    #[cfg(not(windows))]
    pub fn system(journal_dir: &Path) -> Arc<Self> {
        Self::new(Arc::new(NullSwap), Arc::new(NullSource), Arc::new(FileJournal::in_dir(journal_dir)))
    }

    pub fn check(&self) -> Result<(), String> {
        self.swap.check()
    }

    /// Where the readers deliver (the newest router wins).
    pub fn set_handler(&self, h: Weak<dyn CaptureHandler>) {
        *lock(&self.handler) = Some(h);
    }

    /// Start watching for replugged captured devices (retries pending releases).
    pub fn watch(self: &Arc<Self>) {
        let mut w = lock(&self.watch);
        if w.is_some() {
            return;
        }
        let weak = Arc::downgrade(self);
        match self.swap.watch(Arc::new(move || {
            if let Some(me) = weak.upgrade() {
                // Off the notification thread: a release reinstalls a driver.
                std::thread::spawn(move || me.retry_pending());
            }
        })) {
            Ok(g) => *w = Some(g),
            Err(e) => log::warn!("USB capture: arrival notifications unavailable: {e}"),
        }
    }

    /// Units of the captured nodes, shown as present (they no longer enumerate as HID).
    pub fn captured_units(&self) -> Vec<Device> {
        lock(&self.active)
            .values()
            .flat_map(|a| a.entry.units.iter().cloned())
            .map(|mut d| {
                d.present = true;
                d
            })
            .collect()
    }

    /// Nodes captured now, with their seat.
    pub fn captured(&self) -> BTreeMap<String, SeatNumber> {
        lock(&self.active).iter().filter_map(|(n, a)| SeatNumber::new(a.entry.seat).map(|s| (n.clone(), s))).collect()
    }

    /// Groups of the nodes captured now (for [`plan`]).
    pub fn captured_groups(&self) -> Vec<Group> {
        lock(&self.active).iter().map(|(n, a)| Group { node: n.clone(), units: a.entry.units.clone() }).collect()
    }

    /// Something is left to restore: a journaled or failed release, or a node that still
    /// carries our interface GUID (lost journal).
    pub fn needs_restore(&self) -> bool {
        !lock(&self.active).is_empty()
            || !lock(&self.pending).is_empty()
            || !self.journal.load().entries.is_empty()
            || !self.orphans(&BTreeSet::new()).is_empty()
    }

    /// Nodes that still carry our capture ([`UsbSwap::still_captured`]) but are not in
    /// `known` (uppercase ids). A stale interface registration alone is not a capture.
    fn orphans(&self, known: &BTreeSet<String>) -> Vec<String> {
        let mut out = Vec::new();
        for node in self.swap.captured_nodes() {
            if known.contains(&node.to_ascii_uppercase()) || lock(&self.active).keys().any(|n| n.eq_ignore_ascii_case(&node)) {
                continue;
            }
            if self.swap.still_captured(&node) {
                out.push(node);
            } else {
                log::debug!("USB input node {node}: only a stale registration of the capture interface; not ours any more, left alone");
            }
        }
        out
    }

    /// Releases that wait for their device.
    pub fn pending_nodes(&self) -> Vec<String> {
        lock(&self.pending).iter().map(|e| e.node.clone()).collect()
    }

    fn save(&self) -> std::io::Result<()> {
        let mut entries: Vec<CaptureEntry> = lock(&self.active).values().map(|a| a.entry.clone()).collect();
        entries.extend(lock(&self.pending).iter().cloned());
        entries.sort_by(|a, b| a.node.cmp(&b.node));
        entries.dedup_by(|a, b| a.node == b.node);
        self.journal.save(&CaptureJournal { version: 1, entries })
    }

    /// Make the captured set equal `want` (node → seat, units). Returns errors per node.
    pub fn sync(&self, want: &BTreeMap<String, (SeatNumber, Vec<Device>)>) -> Vec<(String, BackendError)> {
        let _op = lock(&self.op);
        let mut errors = Vec::new();
        // Releases first (never two seats on one node in between). Held: everything goes.
        let held = self.is_held();
        let gone: Vec<String> = lock(&self.active)
            .iter()
            .filter(|(n, a)| held || want.get(*n).is_none_or(|(s, _)| s.get() != a.entry.seat))
            .map(|(n, _)| n.clone())
            .collect();
        for node in gone {
            if let Err(e) = self.release_one(&node) {
                errors.push((node, e));
            }
        }
        for (node, (seat, units)) in want {
            if held {
                log::debug!("USB input node {node} not captured for seat {seat}: held after the operator's restore");
                continue;
            }
            let exists = lock(&self.active).contains_key(node);
            if exists {
                // Keep the record fresh (names, keys) and the reader alive.
                if let Some(a) = lock(&self.active).get_mut(node) {
                    a.entry.units = units.clone();
                }
                self.ensure_worker(node);
                continue;
            }
            // Still on WinUSB from an earlier capture whose release failed (or an earlier
            // run): take it over as it is, with its journaled original driver.
            let earlier = {
                let mut p = lock(&self.pending);
                p.iter().position(|e| e.node.eq_ignore_ascii_case(node)).map(|i| p.remove(i))
            };
            if let Some(mut entry) = earlier {
                entry.seat = seat.get();
                entry.units = units.clone();
                lock(&self.active).insert(node.clone(), Active { entry, worker: None });
                if let Err(e) = self.save() {
                    log::warn!("USB capture journal not written: {e}");
                }
                self.ensure_worker(node);
                continue;
            }
            if let Err(e) = self.capture_one(node, *seat, units) {
                errors.push((node.clone(), e));
            }
        }
        drop(_op);
        self.retry_pending();
        errors
    }

    fn capture_one(&self, node: &str, seat: SeatNumber, units: &[Device]) -> BackendResult<()> {
        let original = self.swap.original(node)?;
        let entry = CaptureEntry { node: node.into(), seat: seat.get(), original: original.clone(), units: units.to_vec(), at_ms: self.clock.now_ms() };
        lock(&self.active).insert(node.into(), Active { entry, worker: None });
        // Journal before the swap: a crash in between leaves an entry to restore.
        if let Err(e) = self.save() {
            lock(&self.active).remove(node);
            return Err(BackendError::new(ErrorKind::Os, "usb_capture_journal_failed", e.to_string()));
        }
        if let Err(e) = self.swap.capture(node) {
            log::warn!("USB capture of {node} failed: {e}; restoring");
            if let Err(r) = self.swap.release(node, &original) {
                log::error!("USB capture rollback of {node} failed: {r}; kept in the journal");
                let a = lock(&self.active).remove(node);
                if let Some(a) = a {
                    lock(&self.pending).push(a.entry);
                }
            } else {
                lock(&self.active).remove(node);
            }
            let _ = self.save();
            return Err(e);
        }
        log::info!("USB input node {node} captured for seat {seat} (journaled original driver: {})", original.describe());
        self.ensure_worker(node);
        Ok(())
    }

    fn release_one(&self, node: &str) -> BackendResult<()> {
        let a = lock(&self.active).remove(node);
        let Some(mut a) = a else { return Ok(()) };
        if let Some(w) = a.worker.take() {
            w.stop();
        }
        // The entry stays in the journal (as pending) until the swap-back is done: a save
        // in between, or a crash, never loses the original driver.
        let original = a.entry.original.clone();
        lock(&self.pending).push(a.entry);
        let r = self.swap.release(node, &original);
        match r {
            Ok(()) => {
                lock(&self.pending).retain(|e| !e.node.eq_ignore_ascii_case(node));
                log::info!("USB input node {node} back on its own driver");
                self.verify_restore(node, &original);
                self.save().map_err(|e| BackendError::new(ErrorKind::Os, "usb_capture_journal_failed", e.to_string()))
            }
            Err(e) => {
                log::warn!("USB input node {node} not restored yet: {e}");
                let _ = self.save();
                Err(e)
            }
        }
    }

    /// Retry releases of nodes that were unplugged (or failed) when released.
    pub fn retry_pending(&self) {
        let _op = lock(&self.op);
        // A copy: the entries stay pending (and journaled) until their release is done.
        let list = lock(&self.pending).clone();
        if list.is_empty() {
            return;
        }
        let mut done = Vec::new();
        for e in list {
            match self.swap.release(&e.node, &e.original) {
                Ok(()) => {
                    log::info!("USB input node {} restored after replug", e.node);
                    self.verify_restore(&e.node, &e.original);
                    done.push(e.node);
                }
                Err(err) => log::debug!("USB input node {} still not restorable: {err}", e.node),
            }
        }
        lock(&self.pending).retain(|e| !done.iter().any(|n| n.eq_ignore_ascii_case(&e.node)));
        if let Err(e) = self.save() {
            log::warn!("USB capture journal not written: {e}");
        }
    }

    /// Release every captured node (service stop, backend change, manual restore).
    /// Returns error texts of nodes that could not be restored now (kept journaled).
    pub fn release_all(&self) -> Vec<String> {
        let none = BTreeMap::new();
        self.sync(&none).into_iter().map(|(n, e)| format!("{n}: {e}")).collect()
    }

    /// Service start / uninstall / manual command: restore every journaled node and every
    /// node that still carries our interface GUID (lost journal). Nothing is captured
    /// afterwards until the next [`Self::sync`].
    pub fn restore_all(&self) -> Vec<String> {
        let mut errors = self.release_all();
        let _op = lock(&self.op);
        let mut todo: Vec<CaptureEntry> = std::mem::take(&mut *lock(&self.pending));
        for e in self.journal.load().entries {
            if !todo.iter().any(|t| t.node.eq_ignore_ascii_case(&e.node)) {
                todo.push(e);
            }
        }
        let known: BTreeSet<String> = todo.iter().map(|e| e.node.to_ascii_uppercase()).collect();
        // Only nodes that still carry the capture: a node the release above (or an earlier
        // run) put back on its own driver keeps a stale interface registration, and must
        // not get a second driver install over its vendor driver (gaming PC, bug 5).
        for node in self.orphans(&known) {
            log::warn!("USB input node {node} still carries the Pulsar capture but has no journal entry; restoring with the best driver Windows ranks");
            todo.push(CaptureEntry { node, seat: 0, original: OriginalDriver::unknown(), units: Vec::new(), at_ms: 0 });
        }
        let mut keep = Vec::new();
        for e in todo {
            match self.swap.release(&e.node, &e.original) {
                Ok(()) => {
                    log::info!("USB input node {} restored", e.node);
                    self.verify_restore(&e.node, &e.original);
                }
                Err(err) => {
                    errors.push(format!("{}: {err}", e.node));
                    keep.push(e);
                }
            }
        }
        *lock(&self.pending) = keep;
        if let Err(e) = self.save() {
            errors.push(format!("journal: {e}"));
        }
        errors
    }

    fn ensure_worker(&self, node: &str) {
        let mut act = lock(&self.active);
        let Some(a) = act.get_mut(node) else { return };
        if a.worker.as_ref().is_some_and(|w| w.thread.as_ref().is_some_and(|t| !t.is_finished())) {
            return;
        }
        if let Some(old) = a.worker.take() {
            old.stop();
        }
        let Some(seat) = SeatNumber::new(a.entry.seat) else {
            return;
        };
        let stop = Arc::new(AtomicBool::new(false));
        let reader: Arc<Mutex<Option<Arc<dyn UsbReportReader>>>> = Arc::new(Mutex::new(None));
        let ctx = WorkerCtx {
            node: node.to_string(),
            seat,
            units: a.entry.units.clone(),
            source: self.source.clone(),
            handler: self.handler.clone(),
            clock: self.clock.clone(),
            stop: stop.clone(),
            reader: reader.clone(),
        };
        match std::thread::Builder::new().name("seat-input-usb-reader".into()).spawn(move || ctx.run()) {
            Ok(t) => a.worker = Some(Worker { stop, reader, thread: Some(t) }),
            Err(e) => log::error!("USB reader thread for {node} not started: {e}"),
        }
    }
}

impl Drop for UsbCapture {
    fn drop(&mut self) {
        // Readers only; the devices stay captured until an explicit release (the journal
        // restores them at the next start if nobody does).
        let workers: Vec<Worker> = lock(&self.active).values_mut().filter_map(|a| a.worker.take()).collect();
        for w in workers {
            w.stop();
        }
    }
}

/// Restore everything a previous run left captured (service start, uninstall): the
/// Windows parts over `journal_dir`. Returns error texts (logged by the caller).
pub fn restore_at_start(journal_dir: &Path) -> Vec<String> {
    let c = UsbCapture::system(journal_dir);
    if !c.needs_restore() {
        return Vec::new();
    }
    c.restore_all()
}

struct WorkerCtx {
    node: String,
    seat: SeatNumber,
    units: Vec<Device>,
    source: Arc<dyn UsbReportSource>,
    handler: Arc<Mutex<Option<Weak<dyn CaptureHandler>>>>,
    clock: Arc<dyn Clock>,
    stop: Arc<AtomicBool>,
    reader: Arc<Mutex<Option<Arc<dyn UsbReportReader>>>>,
}

/// Consecutive read failures after which the reader reopens the device.
const MAX_READ_FAILURES: u32 = 20;

impl WorkerCtx {
    fn handler(&self) -> Option<Arc<dyn CaptureHandler>> {
        lock(&self.handler).as_ref().and_then(Weak::upgrade)
    }

    fn key_of(&self, kind: DeviceKind) -> Option<DeviceKey> {
        self.units.iter().find(|d| d.kind == kind).or_else(|| self.units.first()).map(|d| d.key.clone())
    }

    fn has(&self, kind: DeviceKind) -> bool {
        self.units.iter().any(|d| d.kind == kind)
    }

    fn sleep(&self, ms: u64) {
        let t = Instant::now();
        while !self.stop.load(Ordering::SeqCst) && t.elapsed() < Duration::from_millis(ms) {
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn layout(&self, r: &dyn UsbReportReader) -> ReportLayout {
        if let Some(d) = r.report_descriptor() {
            match hid_layout::parse(&d) {
                Ok(l) => return l,
                Err(e) => log::warn!("USB input node {}: report descriptor not usable ({e}); boot protocol", self.node),
            }
        }
        if self.has(DeviceKind::Keyboard) { hid_layout::boot_keyboard() } else { hid_layout::boot_mouse() }
    }

    fn run(self) {
        let opts = || self.handler().map(|h| h.options()).unwrap_or_default();
        let mut proc = UnitProcessor::new(opts());
        let kbd = self.key_of(DeviceKind::Keyboard);
        let mouse = self.key_of(DeviceKind::Mouse);
        while !self.stop.load(Ordering::SeqCst) {
            let r = match self.source.open(&self.node) {
                Ok(r) => r,
                Err(e) => {
                    // The interface appears a moment after the driver swap, and again after
                    // a replug: keep trying.
                    log::debug!("USB input node {} not open yet: {e}", self.node);
                    self.sleep(500);
                    continue;
                }
            };
            *lock(&self.reader) = Some(r.clone());
            if self.stop.load(Ordering::SeqCst) {
                r.cancel();
            }
            let mut tr = ReportTranslator::new(self.layout(&*r));
            let mut failures = 0u32;
            loop {
                match r.read() {
                    Ok(report) => {
                        failures = 0;
                        proc.set_options(opts());
                        for ev in tr.translate(&report) {
                            self.dispatch(&mut proc, &ev, kbd.as_ref(), mouse.as_ref());
                        }
                    }
                    Err(e) if e.reason == "cancelled" => break,
                    Err(e) => {
                        failures += 1;
                        if is_fatal_win32(e.win32) || failures > MAX_READ_FAILURES {
                            log::warn!("USB input node {} gone or failing ({e}); reopening", self.node);
                            break;
                        }
                        self.sleep(50);
                    }
                }
            }
            *lock(&self.reader) = None;
            // Nothing stays held in the seat while the device is away.
            let held = proc.release_all();
            if let Some(h) = self.handler()
                && !held.is_empty()
            {
                h.packets(self.seat, &held);
            }
            if !self.stop.load(Ordering::SeqCst) {
                self.sleep(500);
            }
        }
    }

    fn dispatch(&self, proc: &mut UnitProcessor, ev: &RawUnitEvent, kbd: Option<&DeviceKey>, mouse: Option<&DeviceKey>) {
        let Some(h) = self.handler() else { return };
        let outcomes: Vec<Outcome> = proc.process(ev, self.clock.now_ms());
        let (packets, hotkey, emergency) = split(outcomes);
        let key = match ev {
            RawUnitEvent::Mouse { .. } => mouse.or(kbd),
            _ => kbd.or(mouse),
        };
        if !packets.is_empty() {
            h.packets(self.seat, &packets);
            if let Some(k) = key {
                h.activity(k);
            }
        }
        // Hotkey and emergency run off this reader thread: their handlers re-plan the routes,
        // and a release of this very node stops (joins) this thread and reinstalls the
        // driver, which must not happen while this thread still holds the WinUSB handle.
        if hotkey && let Some(k) = kbd {
            let (h, k, seat) = (h.clone(), k.clone(), self.seat);
            Self::off_thread("seat-input-usb-hotkey", move || h.hotkey(&k, seat));
        }
        if emergency {
            let h = h.clone();
            Self::off_thread("seat-input-usb-emergency", move || h.emergency());
        }
    }

    /// Run `f` on its own thread; inline when no thread can be started (never dropped).
    fn off_thread(name: &str, f: impl FnOnce() + Send + 'static) {
        let slot = Arc::new(Mutex::new(Some(f)));
        let theirs = slot.clone();
        let spawned = std::thread::Builder::new().name(name.into()).spawn(move || {
            let f = lock(&theirs).take();
            if let Some(f) = f {
                f();
            }
        });
        if let Err(e) = spawned {
            log::error!("USB capture: {name} thread not started ({e}); running it inline");
            let f = lock(&slot).take();
            if let Some(f) = f {
                f();
            }
        }
    }
}

/// No swapping (non-Windows builds).
pub struct NullSwap;

impl UsbSwap for NullSwap {
    fn check(&self) -> Result<(), String> {
        Err("not_windows".into())
    }
    fn original(&self, _: &str) -> BackendResult<OriginalDriver> {
        Err(BackendError::unsupported("USB driver swap"))
    }
    fn current(&self, _: &str) -> Option<OriginalDriver> {
        None
    }
    fn capture(&self, _: &str) -> BackendResult<()> {
        Err(BackendError::unsupported("USB driver swap"))
    }
    fn release(&self, _: &str, _: &OriginalDriver) -> BackendResult<()> {
        Err(BackendError::unsupported("USB driver swap"))
    }
    fn captured_nodes(&self) -> Vec<String> {
        Vec::new()
    }
    fn still_captured(&self, _: &str) -> bool {
        false
    }
    fn watch(&self, _: ChangeCallback) -> BackendResult<Box<dyn Guard>> {
        Ok(Box::new(()))
    }
}

/// No WinUSB (non-Windows builds).
pub struct NullSource;

impl UsbReportSource for NullSource {
    fn open(&self, _: &str) -> BackendResult<Arc<dyn UsbReportReader>> {
        Err(BackendError::unsupported("WinUSB"))
    }
}

#[cfg(test)]
pub(crate) mod fake {
    //! In-memory swapper and WinUSB devices.
    use super::*;
    use crate::platform::cancelled;
    use std::collections::{HashMap, VecDeque};
    use std::sync::Condvar;

    #[derive(Default)]
    pub struct FakeSwap {
        /// node → current driver (absent = never touched: HIDClass / input.inf).
        pub drivers: Mutex<HashMap<String, OriginalDriver>>,
        pub calls: Mutex<Vec<String>>,
        /// Nodes that are unplugged (release fails NotFound).
        pub absent: Mutex<BTreeSet<String>>,
        pub fail_capture: Mutex<bool>,
        pub cb: Mutex<Option<ChangeCallback>>,
        /// Nodes our interface was ever registered on. Like Windows, the registration stays
        /// after the release (`captured_nodes` keeps listing them).
        pub registered: Mutex<BTreeSet<String>>,
        /// What Windows would rank best for a node (absent = `input.inf`).
        pub best: Mutex<HashMap<String, OriginalDriver>>,
        /// What a release really leaves on a node instead of the asked driver (a lost
        /// extension, a different package).
        pub release_leaves: Mutex<HashMap<String, OriginalDriver>>,
    }

    impl FakeSwap {
        pub fn is_captured(&self, node: &str) -> bool {
            lock(&self.drivers).get(node).is_some_and(|d| d.class_guid == USB_DEVICE_CLASS)
        }
        pub fn calls(&self) -> Vec<String> {
            lock(&self.calls).clone()
        }
        pub fn driver(&self, node: &str) -> OriginalDriver {
            lock(&self.drivers).get(node).cloned().unwrap_or_else(OriginalDriver::hid_default)
        }
    }

    impl UsbSwap for FakeSwap {
        fn check(&self) -> Result<(), String> {
            Ok(())
        }
        fn original(&self, node: &str) -> BackendResult<OriginalDriver> {
            let d = self.driver(node);
            if d.class_guid == USB_DEVICE_CLASS {
                return Err(BackendError::new(ErrorKind::Coexistence, "already_winusb", node.to_string()));
            }
            Ok(d)
        }
        fn current(&self, node: &str) -> Option<OriginalDriver> {
            Some(self.driver(node))
        }
        fn capture(&self, node: &str) -> BackendResult<()> {
            lock(&self.calls).push(format!("capture {node}"));
            if *lock(&self.fail_capture) {
                return Err(BackendError::new(ErrorKind::Os, "di_install_failed", "test"));
            }
            lock(&self.registered).insert(node.into());
            lock(&self.drivers)
                .insert(node.into(), OriginalDriver { class_guid: USB_DEVICE_CLASS.into(), inf_path: "winusb.inf".into(), ..OriginalDriver::default() });
            Ok(())
        }
        fn release(&self, node: &str, original: &OriginalDriver) -> BackendResult<()> {
            lock(&self.calls).push(format!("release {node}"));
            if lock(&self.absent).contains(node) {
                return Err(BackendError::new(ErrorKind::NotFound, "device_absent", "unplugged"));
            }
            let put = if let Some(d) = lock(&self.release_leaves).get(node).cloned() {
                d
            } else if original.inf_path.is_empty() {
                lock(&self.best).get(node).cloned().unwrap_or_else(OriginalDriver::hid_default)
            } else {
                original.clone()
            };
            lock(&self.drivers).insert(node.into(), put);
            Ok(())
        }
        fn captured_nodes(&self) -> Vec<String> {
            let mut out: Vec<String> = lock(&self.registered).iter().cloned().collect();
            for (n, d) in lock(&self.drivers).iter() {
                if d.class_guid == USB_DEVICE_CLASS && !out.contains(n) {
                    out.push(n.clone());
                }
            }
            out
        }
        fn still_captured(&self, node: &str) -> bool {
            self.is_captured(node)
        }
        fn watch(&self, cb: ChangeCallback) -> BackendResult<Box<dyn Guard>> {
            *lock(&self.cb) = Some(cb);
            Ok(Box::new(()))
        }
    }

    #[derive(Default)]
    pub struct FakeUsbDev {
        pub desc: Mutex<Option<Vec<u8>>>,
        pub queue: Mutex<VecDeque<BackendResult<Vec<u8>>>>,
        pub cv: Condvar,
        pub cancelled: Mutex<bool>,
    }

    impl FakeUsbDev {
        pub fn push(&self, report: &[u8]) {
            lock(&self.queue).push_back(Ok(report.to_vec()));
            self.cv.notify_all();
        }
        pub fn fail(&self, win32: u32) {
            lock(&self.queue).push_back(Err(BackendError::new(ErrorKind::Os, "read_failed", "gone").with_win32(win32)));
            self.cv.notify_all();
        }
    }

    impl UsbReportReader for FakeUsbDev {
        fn report_descriptor(&self) -> Option<Vec<u8>> {
            lock(&self.desc).clone()
        }
        fn read(&self) -> BackendResult<Vec<u8>> {
            let mut q = lock(&self.queue);
            loop {
                if *lock(&self.cancelled) {
                    return Err(cancelled());
                }
                if let Some(r) = q.pop_front() {
                    return r;
                }
                q = self.cv.wait_timeout(q, Duration::from_millis(20)).unwrap_or_else(|e| e.into_inner()).0;
            }
        }
        fn cancel(&self) {
            *lock(&self.cancelled) = true;
            self.cv.notify_all();
        }
    }

    /// Devices by node; `open` hands out the current one (a fresh one per [`Self::plug`]).
    #[derive(Default)]
    pub struct FakeUsbSource {
        pub devs: Mutex<HashMap<String, Arc<FakeUsbDev>>>,
        pub opens: Mutex<Vec<String>>,
    }

    impl FakeUsbSource {
        pub fn plug(&self, node: &str, desc: Option<&[u8]>) -> Arc<FakeUsbDev> {
            let d = Arc::new(FakeUsbDev::default());
            *lock(&d.desc) = desc.map(<[u8]>::to_vec);
            lock(&self.devs).insert(node.into(), d.clone());
            d
        }
    }

    impl UsbReportSource for FakeUsbSource {
        fn open(&self, node: &str) -> BackendResult<Arc<dyn UsbReportReader>> {
            lock(&self.opens).push(node.into());
            match lock(&self.devs).get(node) {
                Some(d) if !*lock(&d.cancelled) => Ok(d.clone()),
                _ => Err(BackendError::new(ErrorKind::NotFound, "not_present", "no WinUSB interface")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::*;
    use super::*;
    use crate::hid_layout::tests::KEYBOARD_DESC;
    use crate::units::{build_units, tests as ut};
    use seat_core::backend::input::{InputRoute, key_flags};
    use seat_core::ids::SeatMask;

    fn s(n: u8) -> SeatNumber {
        SeatNumber::new(n).unwrap()
    }

    const KBD_NODE: &str = r"USB\VID_046D&PID_C52B&MI_00\6&0";
    const MOU_NODE: &str = r"USB\VID_046D&PID_C52B&MI_01\6&1";

    fn units() -> Vec<Device> {
        build_units(&ut::tree(), &ut::list())
    }

    fn key(units: &[Device], kind: DeviceKind) -> DeviceKey {
        units.iter().find(|d| d.kind == kind).unwrap().key.clone()
    }

    fn running(seats: &[u8]) -> BTreeSet<SeatNumber> {
        seats.iter().map(|n| s(*n)).collect()
    }

    /// A built-in keyboard and mouse that stay with the console.
    fn console_inputs() -> Vec<Device> {
        let mut k = units().into_iter().find(|d| d.kind == DeviceKind::Keyboard).unwrap();
        k.key = DeviceKey::new("kbd;builtin").unwrap();
        let mut m = units().into_iter().find(|d| d.kind == DeviceKind::Mouse).unwrap();
        m.key = DeviceKey::new("mou;builtin").unwrap();
        vec![k, m]
    }

    #[test]
    fn groups_by_usb_interface_node() {
        let t = ut::tree();
        let g = groups(&t, &units());
        let nodes: Vec<&str> = g.iter().map(|g| g.node.as_str()).collect();
        assert_eq!(nodes, vec![KBD_NODE, MOU_NODE], "touch screen (I2C) is not a USB group");
        assert_eq!(g[1].units.len(), 2, "mouse and consumer collection share MI_01");
    }

    #[test]
    fn plan_captures_single_seat_running_groups() {
        let u = units();
        let t = ut::tree();
        let g = groups(&t, &u);
        let table = RouteTable::from_routes(&[
            InputRoute { key: key(&u, DeviceKind::Keyboard), mask: s(2).mask() },
            InputRoute { key: key(&u, DeviceKind::Mouse), mask: s(2).mask() },
        ]);
        let p = plan(&g, &table, &running(&[2]), false, &console_inputs());
        assert_eq!(p.capture.keys().map(String::as_str).collect::<Vec<_>>(), vec![KBD_NODE, MOU_NODE]);
        assert!(p.refused.is_empty());
        // Seat 2 not running: nothing captured, nothing refused.
        let p = plan(&g, &table, &running(&[]), false, &console_inputs());
        assert!(p.capture.is_empty() && p.refused.is_empty());
        // Wizard: nothing.
        assert!(plan(&g, &table, &running(&[2]), true, &console_inputs()).capture.is_empty());
    }

    #[test]
    fn plan_never_takes_the_last_console_keyboard_or_mouse() {
        let u = units();
        let t = ut::tree();
        let g = groups(&t, &u);
        let table = RouteTable::from_routes(&[
            InputRoute { key: key(&u, DeviceKind::Keyboard), mask: s(2).mask() },
            InputRoute { key: key(&u, DeviceKind::Mouse), mask: s(2).mask() },
        ]);
        let p = plan(&g, &table, &running(&[2]), false, &[]);
        assert!(p.capture.is_empty(), "{p:?}");
        assert!(p.refused.iter().all(|(_, r)| *r == Refusal::LastConsoleInput));
        assert_eq!(p.refused.len(), 2);
        // With only a console keyboard left elsewhere, the keyboard node goes, the mouse
        // node stays.
        let only_kbd: Vec<Device> = console_inputs().into_iter().filter(|d| d.kind == DeviceKind::Keyboard).collect();
        let p = plan(&g, &table, &running(&[2]), false, &only_kbd);
        assert_eq!(p.capture.keys().map(String::as_str).collect::<Vec<_>>(), vec![KBD_NODE]);
        assert_eq!(p.refused, vec![(key(&u, DeviceKind::Mouse), Refusal::LastConsoleInput)]);
    }

    #[test]
    fn plan_refuses_mixed_and_foreign_collections() {
        let u = units();
        let t = ut::tree();
        let g = groups(&t, &u);
        let cons = u.iter().find(|d| d.kind == DeviceKind::Hid && d.key.as_str().ends_with("000c:0001")).unwrap().key.clone();
        // Consumer collection on seat 3 while the mouse is on seat 2.
        let table =
            RouteTable::from_routes(&[InputRoute { key: key(&u, DeviceKind::Mouse), mask: s(2).mask() }, InputRoute { key: cons.clone(), mask: s(3).mask() }]);
        let p = plan(&g, &table, &running(&[2, 3]), false, &console_inputs());
        assert!(p.capture.is_empty());
        assert!(p.refused.contains(&(cons.clone(), Refusal::MixedSeats)));
        // Consumer left on seat 1: it follows the mouse.
        let table = RouteTable::from_routes(&[InputRoute { key: key(&u, DeviceKind::Mouse), mask: s(2).mask() }]);
        let p = plan(&g, &table, &running(&[2]), false, &console_inputs());
        assert_eq!(p.capture[MOU_NODE].1.len(), 2);
        // Shared keyboard: mixed.
        let table = RouteTable::from_routes(&[InputRoute { key: key(&u, DeviceKind::Keyboard), mask: SeatMask(0b110) }]);
        let p = plan(&g, &table, &running(&[2, 3]), false, &console_inputs());
        assert_eq!(p.refused, vec![(key(&u, DeviceKind::Keyboard), Refusal::MixedSeats)]);
    }

    struct Rec {
        packets: Mutex<Vec<(SeatNumber, Vec<InputPacket>)>>,
        hotkeys: Mutex<Vec<DeviceKey>>,
        emergencies: Mutex<u32>,
        activity: Mutex<Vec<DeviceKey>>,
    }

    impl CaptureHandler for Rec {
        fn packets(&self, seat: SeatNumber, p: &[InputPacket]) {
            lock(&self.packets).push((seat, p.to_vec()));
        }
        fn hotkey(&self, key: &DeviceKey, _seat: SeatNumber) {
            lock(&self.hotkeys).push(key.clone());
        }
        fn emergency(&self) {
            *lock(&self.emergencies) += 1;
        }
        fn activity(&self, key: &DeviceKey) {
            lock(&self.activity).push(key.clone());
        }
        fn options(&self) -> UnitOptions {
            UnitOptions { hotkey: crate::convert::Hotkey::from_setting(Some(0x58)), allow_system_buttons: false }
        }
    }

    fn rec() -> Arc<Rec> {
        Arc::new(Rec { packets: Mutex::default(), hotkeys: Mutex::default(), emergencies: Mutex::default(), activity: Mutex::default() })
    }

    fn wait_for(cond: impl Fn() -> bool) {
        let t = Instant::now();
        while !cond() {
            assert!(t.elapsed() < Duration::from_secs(5), "timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    struct Env {
        c: Arc<UsbCapture>,
        swap: Arc<FakeSwap>,
        src: Arc<FakeUsbSource>,
        journal: Arc<MemoryJournal>,
        h: Arc<Rec>,
    }

    fn env() -> Env {
        let swap = Arc::new(FakeSwap::default());
        let src = Arc::new(FakeUsbSource::default());
        let journal = Arc::new(MemoryJournal::default());
        let c = UsbCapture::with_clock(swap.clone(), src.clone(), journal.clone(), Arc::new(seat_core::mock::MockClock::default()));
        let h = rec();
        let weak: Weak<dyn CaptureHandler> = Arc::downgrade(&(h.clone() as Arc<dyn CaptureHandler>));
        c.set_handler(weak);
        Env { c, swap, src, journal, h }
    }

    fn want(node: &str, seat: u8, kind: DeviceKind) -> BTreeMap<String, (SeatNumber, Vec<Device>)> {
        let u: Vec<Device> = units().into_iter().filter(|d| d.kind == kind).collect();
        BTreeMap::from([(node.to_string(), (s(seat), u))])
    }

    #[test]
    fn capture_journals_first_then_reads_and_forwards() {
        let e = env();
        let dev = e.src.plug(KBD_NODE, Some(KEYBOARD_DESC));
        assert!(e.c.sync(&want(KBD_NODE, 2, DeviceKind::Keyboard)).is_empty());
        assert!(e.swap.is_captured(KBD_NODE));
        let j = e.journal.load();
        assert_eq!(j.entries.len(), 1);
        assert_eq!(j.entries[0].original, OriginalDriver::hid_default());
        assert_eq!(j.entries[0].seat, 2);
        assert_eq!(e.c.captured_units().len(), 1);
        // a down, a up.
        dev.push(&[0, 0, 0x04, 0, 0, 0, 0, 0]);
        dev.push(&[0, 0, 0, 0, 0, 0, 0, 0]);
        wait_for(|| lock(&e.h.packets).len() >= 2);
        let p = lock(&e.h.packets).clone();
        assert_eq!(p[0], (s(2), vec![InputPacket::Key { sc: 0x1E, f: 0 }]));
        assert_eq!(p[1], (s(2), vec![InputPacket::Key { sc: 0x1E, f: key_flags::UP }]));
        assert!(!lock(&e.h.activity).is_empty());
        // Ctrl + F12 = the configured hotkey.
        dev.push(&[0x01, 0, 0x45, 0, 0, 0, 0, 0]);
        wait_for(|| !lock(&e.h.hotkeys).is_empty());
        // Release: device back, journal empty, held Ctrl released in the seat.
        assert!(e.c.release_all().is_empty());
        assert!(!e.swap.is_captured(KBD_NODE));
        assert!(e.journal.load().entries.is_empty());
        let last = lock(&e.h.packets).last().cloned().unwrap();
        assert_eq!(last, (s(2), vec![InputPacket::Key { sc: 0x1D, f: key_flags::UP }]));
    }

    #[test]
    fn failed_swap_is_rolled_back() {
        let e = env();
        *lock(&e.swap.fail_capture) = true;
        let errs = e.c.sync(&want(KBD_NODE, 2, DeviceKind::Keyboard));
        assert_eq!(errs.len(), 1);
        assert_eq!(e.swap.calls(), vec![format!("capture {KBD_NODE}"), format!("release {KBD_NODE}")]);
        assert!(e.journal.load().entries.is_empty());
        assert!(e.c.captured().is_empty());
    }

    #[test]
    fn journal_failure_blocks_the_swap() {
        let e = env();
        *lock(&e.journal.fail) = true;
        assert_eq!(e.c.sync(&want(KBD_NODE, 2, DeviceKind::Keyboard))[0].1.reason, "usb_capture_journal_failed");
        assert!(e.swap.calls().is_empty(), "never swap without a journal entry");
    }

    #[test]
    fn seat_change_releases_then_recaptures() {
        let e = env();
        e.src.plug(KBD_NODE, None);
        e.c.sync(&want(KBD_NODE, 2, DeviceKind::Keyboard));
        e.c.sync(&want(KBD_NODE, 3, DeviceKind::Keyboard));
        assert_eq!(e.swap.calls(), vec![format!("capture {KBD_NODE}"), format!("release {KBD_NODE}"), format!("capture {KBD_NODE}")]);
        assert_eq!(e.c.captured()[KBD_NODE], s(3));
        e.c.release_all();
    }

    #[test]
    fn unplugged_release_stays_journaled_and_retries() {
        let e = env();
        e.src.plug(KBD_NODE, None);
        e.c.sync(&want(KBD_NODE, 2, DeviceKind::Keyboard));
        lock(&e.swap.absent).insert(KBD_NODE.into());
        assert_eq!(e.c.release_all().len(), 1);
        assert_eq!(e.c.pending_nodes(), vec![KBD_NODE.to_string()]);
        assert_eq!(e.journal.load().entries.len(), 1, "kept for the next try");
        lock(&e.swap.absent).clear();
        e.c.watch();
        let cb = lock(&e.swap.cb).clone().unwrap();
        cb();
        // The retry runs on its own thread; the journal is written after the release.
        wait_for(|| e.c.pending_nodes().is_empty() && e.journal.load().entries.is_empty());
        assert!(!e.swap.is_captured(KBD_NODE));
    }

    #[test]
    fn restore_all_covers_journal_and_orphans() {
        let e = env();
        // A previous run's entry with its own INF, plus an orphan without a journal entry.
        let orig = OriginalDriver { class_guid: HID_CLASS.into(), inf_path: "oem7.inf".into(), ..OriginalDriver::default() };
        *lock(&e.journal.doc) =
            CaptureJournal { version: 1, entries: vec![CaptureEntry { node: KBD_NODE.into(), seat: 2, original: orig.clone(), units: Vec::new(), at_ms: 1 }] };
        e.swap.capture(KBD_NODE).unwrap();
        e.swap.capture(MOU_NODE).unwrap();
        assert!(e.c.restore_all().is_empty());
        assert_eq!(lock(&e.swap.drivers)[KBD_NODE], orig);
        assert_eq!(lock(&e.swap.drivers)[MOU_NODE], OriginalDriver::hid_default());
        assert!(!e.swap.still_captured(KBD_NODE) && !e.swap.still_captured(MOU_NODE));
        assert!(e.journal.load().entries.is_empty());
    }

    #[test]
    fn a_new_instance_keeps_and_restores_an_earlier_runs_entries() {
        let e = env();
        let old = CaptureEntry { node: MOU_NODE.into(), seat: 2, original: OriginalDriver::hid_default(), units: Vec::new(), at_ms: 1 };
        *lock(&e.journal.doc) = CaptureJournal { version: 1, entries: vec![old.clone()] };
        e.swap.capture(MOU_NODE).unwrap();
        lock(&e.swap.absent).insert(MOU_NODE.into());
        let c = UsbCapture::with_clock(e.swap.clone(), e.src.clone(), e.journal.clone(), Arc::new(seat_core::mock::MockClock::default()));
        assert_eq!(c.pending_nodes(), vec![MOU_NODE.to_string()]);
        // A capture of another node keeps the old entry in the journal.
        e.src.plug(KBD_NODE, None);
        assert!(c.sync(&want(KBD_NODE, 2, DeviceKind::Keyboard)).is_empty());
        let nodes: Vec<String> = e.journal.load().entries.into_iter().map(|x| x.node).collect();
        assert_eq!(nodes, vec![KBD_NODE.to_string(), MOU_NODE.to_string()]);
        // Wanted again while still on WinUSB: taken over, no second swap.
        let calls = e.swap.calls().len();
        assert!(c.sync(&BTreeMap::from([(MOU_NODE.to_string(), (s(3), Vec::new()))])).is_empty());
        assert_eq!(c.captured()[MOU_NODE], s(3));
        assert!(c.pending_nodes().is_empty());
        assert!(!e.swap.calls()[calls..].iter().any(|x| x.starts_with("capture")));
        // Not wanted any more, and the device comes back: the next sync restores it.
        lock(&e.swap.absent).clear();
        c.sync(&BTreeMap::new());
        assert!(c.pending_nodes().is_empty());
        assert!(!e.swap.still_captured(MOU_NODE) && !e.swap.still_captured(KBD_NODE));
        assert!(e.journal.load().entries.is_empty());
        // restore_all releases each node once.
        e.swap.calls.lock().unwrap().clear();
        *lock(&e.journal.doc) = CaptureJournal { version: 1, entries: vec![old] };
        e.swap.capture(MOU_NODE).unwrap();
        let c = UsbCapture::with_clock(e.swap.clone(), e.src.clone(), e.journal.clone(), Arc::new(seat_core::mock::MockClock::default()));
        assert!(c.restore_all().is_empty());
        let releases = e.swap.calls().iter().filter(|x| x.starts_with("release")).count();
        assert_eq!(releases, 1);
    }

    #[test]
    fn reader_reopens_after_unplug() {
        let e = env();
        let dev = e.src.plug(KBD_NODE, Some(KEYBOARD_DESC));
        e.c.sync(&want(KBD_NODE, 2, DeviceKind::Keyboard));
        dev.push(&[0, 0, 0x04, 0, 0, 0, 0, 0]);
        wait_for(|| lock(&e.h.packets).len() == 1);
        dev.fail(1167); // ERROR_DEVICE_NOT_CONNECTED
        // The held key is released in the seat.
        wait_for(|| lock(&e.h.packets).len() == 2);
        assert_eq!(lock(&e.h.packets)[1].1, vec![InputPacket::Key { sc: 0x1E, f: key_flags::UP }]);
        let dev2 = e.src.plug(KBD_NODE, Some(KEYBOARD_DESC));
        dev2.push(&[0, 0, 0x05, 0, 0, 0, 0, 0]);
        wait_for(|| lock(&e.h.packets).len() == 3);
        assert_eq!(lock(&e.h.packets)[2].1, vec![InputPacket::Key { sc: 0x30, f: 0 }]);
        e.c.release_all();
    }

    /// A hotkey handler that re-plans at once (the service moves the unit and applies the
    /// routes, which releases this node) does not run on the node's own reader thread: the
    /// release joins that thread without waiting for [`STOP_TIMEOUT`].
    #[test]
    fn hotkey_handler_may_release_the_node_it_came_from() {
        struct Replan {
            cap: Mutex<Option<Weak<UsbCapture>>>,
            took: Mutex<Option<Duration>>,
        }
        impl CaptureHandler for Replan {
            fn packets(&self, _: SeatNumber, _: &[InputPacket]) {}
            fn hotkey(&self, _: &DeviceKey, _: SeatNumber) {
                let cap = lock(&self.cap).as_ref().and_then(Weak::upgrade);
                if let Some(c) = cap {
                    let t = Instant::now();
                    assert!(c.release_all().is_empty());
                    *lock(&self.took) = Some(t.elapsed());
                }
            }
            fn emergency(&self) {}
            fn activity(&self, _: &DeviceKey) {}
            fn options(&self) -> UnitOptions {
                UnitOptions { hotkey: crate::convert::Hotkey::from_setting(Some(0x58)), allow_system_buttons: false }
            }
        }
        let e = env();
        let h = Arc::new(Replan { cap: Mutex::new(Some(Arc::downgrade(&e.c))), took: Mutex::new(None) });
        let weak: Weak<dyn CaptureHandler> = Arc::downgrade(&(h.clone() as Arc<dyn CaptureHandler>));
        e.c.set_handler(weak);
        let dev = e.src.plug(KBD_NODE, Some(KEYBOARD_DESC));
        assert!(e.c.sync(&want(KBD_NODE, 2, DeviceKind::Keyboard)).is_empty());
        // Ctrl + F12 = the configured hotkey.
        dev.push(&[0x01, 0, 0x45, 0, 0, 0, 0, 0]);
        wait_for(|| lock(&h.took).is_some());
        let took = lock(&h.took).unwrap();
        assert!(took < STOP_TIMEOUT / 2, "release waited for its own reader thread: {took:?}");
        assert!(!e.swap.is_captured(KBD_NODE));
    }

    #[test]
    fn file_journal_round_trip() {
        let dir = std::env::temp_dir().join(format!("seat-usb-capture-{}", std::process::id()));
        let j = FileJournal::in_dir(&dir);
        assert!(j.load().entries.is_empty());
        let doc = CaptureJournal {
            version: 1,
            entries: vec![CaptureEntry { node: KBD_NODE.into(), seat: 2, original: OriginalDriver::hid_default(), units: units(), at_ms: 5 }],
        };
        j.save(&doc).unwrap();
        j.save(&doc).unwrap();
        assert_eq!(j.load(), doc);
        std::fs::write(&j.path, b"{nope").unwrap();
        assert!(j.load().entries.is_empty(), "unreadable = empty");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn refusal_notes() {
        assert_eq!(Refusal::LastConsoleInput.note(), "usb_capture_last_console_input");
        assert_eq!(Refusal::MixedSeats.note(), "usb_capture_mixed_seats");
        assert_eq!(Refusal::UnsupportedCollection.note(), "usb_capture_unsupported_collection");
    }

    /// The setup classes the swap writes are the documented ones (`devguid.h`:
    /// `GUID_DEVCLASS_HIDCLASS`, `GUID_DEVCLASS_USBDEVICE`); a wrong `HIDClass` GUID would
    /// leave a restored node in a class that does not exist.
    #[test]
    fn setup_class_guids_are_the_documented_ones() {
        assert_eq!(HID_CLASS, "{745a17a0-74d3-11d0-b6fe-00a0c90f57da}");
        assert!(HID_CLASS.eq_ignore_ascii_case(crate::pad_scope::HID_CLASS));
        assert_eq!(USB_DEVICE_CLASS, "{88bae032-5a81-49f0-bc3d-a4ff138216d6}");
        assert_eq!(OriginalDriver::hid_default().class_guid, HID_CLASS);
    }

    /// The Logitech receiver of the gaming PC (2026-11-24): MI_00 on the vendor package,
    /// MI_01 on `input.inf` plus the vendor extension.
    fn logitech_kbd() -> OriginalDriver {
        OriginalDriver {
            class_guid: HID_CLASS.into(),
            inf_path: "oem128.inf".into(),
            inf_section: "LogiLDA".into(),
            driver_version: "1.10.80.0".into(),
            provider: "Logitech".into(),
            matching_id: r"usb\vid_046d&pid_c52b&mi_00".into(),
            extensions: Vec::new(),
        }
    }

    fn logitech_mouse() -> OriginalDriver {
        OriginalDriver {
            inf_section: "HID_Inst.NT".into(),
            driver_version: "10.0.26100.9974".into(),
            extensions: vec![r"oem185.inf:USB\VID_046D&PID_C52B&MI_01,LogiDownloadAssistantExtension_Install.NT,05/06/2026,2.0.998.0".into()],
            ..OriginalDriver::hid_default()
        }
    }

    fn both(seat: u8) -> BTreeMap<String, (SeatNumber, Vec<Device>)> {
        let mut w = want(KBD_NODE, seat, DeviceKind::Keyboard);
        w.extend(want(MOU_NODE, seat, DeviceKind::Mouse));
        w
    }

    /// e2e gaming PC 2026-11-24, bug 5. Windows keeps our interface registered after a
    /// release, so the sweep of the operator's restore found both nodes again right after
    /// they were back on their own drivers and installed `input.inf` over the vendor driver;
    /// the next capture then journaled `input.inf` as the original, and every later start
    /// did the same again. Now: one exact release per node, nothing on stale registrations.
    #[test]
    fn restore_puts_back_the_exact_driver_once_and_ignores_stale_registrations() {
        let e = env();
        lock(&e.swap.drivers).insert(KBD_NODE.into(), logitech_kbd());
        lock(&e.swap.drivers).insert(MOU_NODE.into(), logitech_mouse());
        e.src.plug(KBD_NODE, None);
        e.src.plug(MOU_NODE, None);
        assert!(e.c.sync(&both(2)).is_empty());
        let j = e.journal.load();
        assert_eq!(j.entries.iter().find(|x| x.node == KBD_NODE).unwrap().original, logitech_kbd(), "the vendor package is journaled");
        assert_eq!(j.entries.iter().find(|x| x.node == MOU_NODE).unwrap().original.extension_infs(), vec!["oem185.inf".to_string()]);
        // The operator's restore.
        lock(&e.swap.calls).clear();
        assert!(e.c.restore_all().is_empty());
        assert_eq!(e.swap.driver(KBD_NODE), logitech_kbd());
        assert_eq!(e.swap.driver(MOU_NODE), logitech_mouse());
        let releases: Vec<String> = e.swap.calls().into_iter().filter(|c| c.starts_with("release")).collect();
        assert_eq!(releases, vec![format!("release {KBD_NODE}"), format!("release {MOU_NODE}")], "one install per node, none over the restored driver");
        assert!(e.c.inexact_restores().is_empty());
        assert!(e.journal.load().entries.is_empty());
        // Both nodes are still listed by the interface registration, but they are not ours.
        assert_eq!(e.swap.captured_nodes().len(), 2);
        // The next service start does nothing at all.
        lock(&e.swap.calls).clear();
        let boot = UsbCapture::with_clock(e.swap.clone(), e.src.clone(), e.journal.clone(), Arc::new(seat_core::mock::MockClock::default()));
        assert!(!boot.needs_restore());
        assert!(boot.restore_all().is_empty());
        assert!(e.swap.calls().is_empty(), "{:?}", e.swap.calls());
        assert_eq!(e.swap.driver(KBD_NODE), logitech_kbd());
    }

    /// The same incident: `restore_usb_input` while seat 2 still runs was undone 1-3 s later
    /// by the next route apply. Held now until a resume; the capture after the resume
    /// journals the vendor driver again, not `input.inf`.
    #[test]
    fn operator_restore_holds_captures_until_resumed() {
        let e = env();
        lock(&e.swap.drivers).insert(KBD_NODE.into(), logitech_kbd());
        e.src.plug(KBD_NODE, None);
        assert!(e.c.sync(&want(KBD_NODE, 2, DeviceKind::Keyboard)).is_empty());
        e.c.hold();
        assert!(e.c.restore_all().is_empty());
        lock(&e.swap.calls).clear();
        // Seat 2 still runs: the next apply wants the node again.
        assert!(e.c.sync(&want(KBD_NODE, 2, DeviceKind::Keyboard)).is_empty());
        assert!(e.swap.calls().is_empty(), "held: {:?}", e.swap.calls());
        assert!(!e.swap.is_captured(KBD_NODE) && e.c.captured().is_empty());
        assert!(e.c.is_held());
        e.c.resume();
        assert!(e.c.sync(&want(KBD_NODE, 2, DeviceKind::Keyboard)).is_empty());
        assert!(e.swap.is_captured(KBD_NODE));
        assert_eq!(e.journal.load().entries[0].original, logitech_kbd());
        // Hold while captured: the next sync releases.
        e.c.hold();
        e.c.sync(&want(KBD_NODE, 2, DeviceKind::Keyboard));
        assert!(!e.swap.is_captured(KBD_NODE));
        assert_eq!(e.swap.driver(KBD_NODE), logitech_kbd());
    }

    /// A restore that does not bring back the journaled package (here: the extension INF did
    /// not come back) is logged and listed, so round 2 on hardware can see it.
    #[test]
    fn inexact_restore_is_reported() {
        let e = env();
        lock(&e.swap.drivers).insert(MOU_NODE.into(), logitech_mouse());
        e.src.plug(MOU_NODE, None);
        assert!(e.c.sync(&want(MOU_NODE, 2, DeviceKind::Mouse)).is_empty());
        lock(&e.swap.release_leaves).insert(MOU_NODE.into(), OriginalDriver { extensions: Vec::new(), ..logitech_mouse() });
        assert!(e.c.release_all().is_empty());
        assert_eq!(e.c.inexact_restores(), vec![format!("{MOU_NODE}: extension oem185.inf missing")]);
    }

    /// A node that still runs WinUSB with our interface but has no journal entry (journal
    /// lost) gets the driver Windows ranks best, not a fixed `input.inf`.
    #[test]
    fn orphan_without_journal_gets_the_best_ranked_driver() {
        let e = env();
        e.swap.capture(KBD_NODE).unwrap();
        lock(&e.swap.best).insert(KBD_NODE.into(), logitech_kbd());
        assert!(e.c.needs_restore());
        assert!(e.c.restore_all().is_empty());
        assert_eq!(e.swap.driver(KBD_NODE), logitech_kbd());
    }

    #[test]
    fn original_driver_parts() {
        let m = logitech_mouse();
        assert_eq!(m.extension_infs(), vec!["oem185.inf".to_string()]);
        let odd = OriginalDriver { extensions: vec!["nocolon".into(), r"..\x.inf:id".into(), "oem1.inf:a".into(), "OEM1.INF:b".into()], ..OriginalDriver::default() };
        assert_eq!(odd.extension_infs(), vec!["oem1.inf".to_string()], "bare names only, deduplicated");
        assert_eq!(logitech_kbd().describe(), "oem128.inf [LogiLDA] 1.10.80.0 Logitech");
        assert_eq!(m.describe(), "input.inf [HID_Inst.NT] 10.0.26100.9974; extensions: oem185.inf");
        assert_eq!(OriginalDriver::unknown().describe(), "(best ranked driver)");
        // Differences.
        assert!(restore_differences(&logitech_kbd(), &logitech_kbd()).is_empty());
        let now = OriginalDriver { driver_version: "10.0.26100.9974".into(), inf_section: "HID_Inst.NT".into(), ..OriginalDriver::hid_default() };
        assert_eq!(
            restore_differences(&logitech_kbd(), &now),
            vec![
                "driver input.inf instead of oem128.inf".to_string(),
                "section HID_Inst.NT instead of LogiLDA".to_string(),
                "version 10.0.26100.9974 instead of 1.10.80.0".to_string()
            ]
        );
        let winusb = OriginalDriver { class_guid: USB_DEVICE_CLASS.into(), inf_path: "winusb.inf".into(), ..OriginalDriver::default() };
        assert_eq!(restore_differences(&OriginalDriver::unknown(), &winusb).len(), 1);
        assert!(restore_differences(&OriginalDriver::unknown(), &now).is_empty());
        // Journals written before the extra fields load, and old fields only are checked.
        let old: CaptureEntry =
            serde_json::from_str(r#"{"node":"USB\\X","seat":2,"original":{"class_guid":"{745a17a0-74d3-11d0-b6fe-00a0c90f57da}","inf_path":"input.inf"}}"#).unwrap();
        assert_eq!(old.original, OriginalDriver::hid_default());
        assert!(restore_differences(&old.original, &now).is_empty());
        // Empty extra fields are not written.
        assert!(!serde_json::to_string(&OriginalDriver::hid_default()).unwrap().contains("extensions"));
    }
}
