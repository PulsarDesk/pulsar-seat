// SPDX-License-Identifier: GPL-3.0-only
//! The merged device inventory the manager draws (spec 10 §3): every backend's
//! `enumerate()` plus the service's own CfgMgr / CCD baseline scan for the kinds no active
//! backend covers, plus saved assignments of devices that are not plugged in now.
//!
//! - Backend devices win over baseline devices with the same key or the same PnP instance.
//! - A live device that is not assigned but matches an absent assignment by hardware id +
//!   serial (or container id) gets [`DeviceWarning::MovedFrom`] so the manager can offer
//!   "move the assignment to the new port" (spec 70 §5.1).
//! - Port (`port;loc=...`) and container (`cont;id=...`) assignments (spec 70 §5.1) appear
//!   as entries of their own: present while something is plugged into that port / while a
//!   device of that container is live. An empty assigned port is not "missing" in the
//!   compare. Every view carries the port key and (for multi-device containers) the
//!   container key of its device so the manager can offer those assignments.

use crate::backends::Backends;
use seat_core::assign::{Effective, MaskSource, effective_masks};
use seat_core::backend::audio::AudioBackendKind;
use seat_core::backend::display::DisplayBackendKind;
use seat_core::backend::gate::IsolationStatus;
use seat_core::backend::input::InputBackendKind;
use seat_core::config::Config;
use seat_core::device::{Device, DeviceDetails, DeviceKind, DeviceWarning};
use seat_core::pnp::{
    container_key, is_container_key, is_port_key, key_container, key_location, normalize_container, port_chain_label,
    port_key, port_of_location,
};
use seat_core::{DeviceKey, SeatMask};
use seat_proto::{ConfigDiff, DeviceView, MaskChange};
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub device: Device,
    pub arrived_ms: u64,
}

/// What changed in a rebuild.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Changes {
    pub arrived: Vec<DeviceKey>,
    pub removed: Vec<DeviceKey>,
    pub changed: Vec<DeviceKey>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.arrived.is_empty() && self.removed.is_empty() && self.changed.is_empty()
    }
    /// A monitor arrived or left (seats waiting for a display may start).
    pub fn touches_kind(&self, inv: &Inventory, kind: DeviceKind) -> bool {
        self.arrived.iter().chain(&self.removed).chain(&self.changed).any(|k| inv.get(k).is_none_or(|e| e.device.kind == kind))
    }
}

#[derive(Debug, Default)]
pub struct Inventory {
    devices: BTreeMap<DeviceKey, Entry>,
    /// Isolation status per device from the last gate / route run.
    isolation: HashMap<DeviceKey, IsolationStatus>,
}

/// Kinds the active backends enumerate themselves.
pub fn covered_kinds(b: &Backends) -> HashSet<DeviceKind> {
    let mut k = HashSet::new();
    if b.display.kind() != DisplayBackendKind::ConsoleOnly && b.display.availability().is_available() {
        k.insert(DeviceKind::Monitor);
    }
    if b.input.kind() != InputBackendKind::None && b.input.availability().is_available() {
        k.extend([DeviceKind::Keyboard, DeviceKind::Mouse, DeviceKind::Hid]);
    }
    if b.audio.kind() == AudioBackendKind::RdpBridge && b.audio.availability().is_available() {
        k.extend([DeviceKind::AudioRender, DeviceKind::AudioCapture]);
    }
    for g in &b.gates {
        k.extend(g.caps().kinds);
    }
    k
}

/// Every live device: backend enumerations first, then the baseline for uncovered kinds.
/// Returns the devices and snake_case errors of backends that failed to enumerate.
pub fn collect(b: &Backends, baseline: Vec<Device>) -> (Vec<Device>, Vec<String>) {
    let mut out: Vec<Device> = Vec::new();
    let mut errors = Vec::new();
    let mut push_all = |name: &str, r: seat_core::backend::BackendResult<Vec<Device>>, out: &mut Vec<Device>| match r {
        Ok(v) => out.extend(v),
        Err(e) => errors.push(format!("{name}:{}", e.reason)),
    };
    push_all("display", b.display.enumerate(), &mut out);
    push_all("input", b.input.enumerate(), &mut out);
    push_all("audio", b.audio.enumerate(), &mut out);
    for g in &b.gates {
        push_all("gate", g.enumerate(), &mut out);
    }
    let covered = covered_kinds(b);
    let keys: HashSet<DeviceKey> = out.iter().map(|d| d.key.clone()).collect();
    let instances: HashSet<String> =
        out.iter().filter_map(|d| d.instance_id.as_ref().map(|i| i.to_ascii_uppercase())).collect();
    for d in baseline {
        let dup_inst = d.instance_id.as_ref().is_some_and(|i| instances.contains(&i.to_ascii_uppercase()));
        if covered.contains(&d.kind) || keys.contains(&d.key) || dup_inst {
            continue;
        }
        out.push(d);
    }
    // First occurrence of a key wins (a gate and the input backend may both list a hub).
    let mut seen = HashSet::new();
    out.retain(|d| seen.insert(d.key.clone()));
    (out, errors)
}

impl Inventory {
    pub fn get(&self, key: &DeviceKey) -> Option<&Entry> {
        self.devices.get(key)
    }

    pub fn devices(&self) -> Vec<Device> {
        self.devices.values().map(|e| e.device.clone()).collect()
    }

    pub fn live(&self) -> impl Iterator<Item = &Device> {
        self.devices.values().map(|e| &e.device).filter(|d| d.present)
    }

    pub fn set_isolation(&mut self, iso: HashMap<DeviceKey, IsolationStatus>) {
        self.isolation = iso;
    }

    /// Replace the content with `live` (+ absent saved devices) and report the changes.
    pub fn rebuild(&mut self, live: Vec<Device>, config: &Config, now_ms: u64) -> Changes {
        let mut next: BTreeMap<DeviceKey, Entry> = BTreeMap::new();
        for d in live {
            let arrived_ms = match self.devices.get(&d.key) {
                Some(e) if e.device.present => e.arrived_ms,
                _ => now_ms,
            };
            next.insert(d.key.clone(), Entry { device: d, arrived_ms });
        }
        let live_devices: Vec<Device> = next.values().map(|e| e.device.clone()).collect();
        for a in &config.devices {
            if next.contains_key(&a.key) {
                continue;
            }
            let (present, parent_hub, label) = pseudo_presence(&a.key, &live_devices);
            let arrived_ms = match self.devices.get(&a.key) {
                Some(e) if e.device.present && present => e.arrived_ms,
                _ if present => now_ms,
                _ => 0,
            };
            let device = Device {
                key: a.key.clone(),
                kind: a.kind,
                name: a.name.clone(),
                label: a.label.clone().or(label),
                instance_id: None,
                container_id: a.container_id.clone(),
                parent_hub,
                present,
                hardware_id: a.hardware_id.clone(),
                serial: a.serial.clone(),
                warnings: Vec::new(),
                details: DeviceDetails::None,
            };
            next.insert(a.key.clone(), Entry { device, arrived_ms });
        }
        mark_moved(&mut next, config);
        let mut ch = Changes::default();
        for (k, e) in &next {
            match self.devices.get(k) {
                None if e.device.present => ch.arrived.push(k.clone()),
                Some(old) if !old.device.present && e.device.present => ch.arrived.push(k.clone()),
                Some(old) if old.device.present && !e.device.present => ch.removed.push(k.clone()),
                Some(old) if old.device != e.device => ch.changed.push(k.clone()),
                _ => {}
            }
        }
        for (k, old) in &self.devices {
            if !next.contains_key(k) && old.device.present {
                ch.removed.push(k.clone());
            }
        }
        self.devices = next;
        ch
    }

    /// Effective masks of the present devices.
    pub fn effective(&self, config: &Config) -> Vec<Effective> {
        let live: Vec<Device> = self.live().cloned().collect();
        effective_masks(config, &live)
    }

    pub fn view(&self, key: &DeviceKey, config: &Config) -> Option<DeviceView> {
        let e = self.devices.get(key)?;
        let eff = if e.device.present {
            self.effective(config).into_iter().find(|x| &x.key == key)
        } else {
            None
        };
        Some(self.make_view(e, eff, config))
    }

    /// Live device count per normalised container id.
    fn container_counts(&self) -> HashMap<String, usize> {
        let mut m = HashMap::new();
        for d in self.live().filter(|d| !is_pseudo(&d.key)) {
            if let Some(c) = d.container_id.as_deref().and_then(normalize_container) {
                *m.entry(c).or_insert(0) += 1;
            }
        }
        m
    }

    fn make_view(&self, e: &Entry, eff: Option<Effective>, config: &Config) -> DeviceView {
        self.make_view_with(e, eff, config, &self.container_counts())
    }

    fn make_view_with(&self, e: &Entry, eff: Option<Effective>, config: &Config, containers: &HashMap<String, usize>) -> DeviceView {
        let d = &e.device;
        let pseudo = is_pseudo(&d.key);
        let port = if pseudo || d.kind == DeviceKind::Network {
            None
        } else {
            key_location(&d.key).and_then(port_of_location).map(|p| port_key(&p))
        };
        let container = if pseudo {
            None
        } else {
            d.container_id
                .as_deref()
                .and_then(normalize_container)
                .filter(|c| containers.get(c).copied().unwrap_or(0) > 1)
                .and_then(|c| container_key(&c))
        };
        let (effective, source) = match eff {
            Some(x) => (x.mask, x.source),
            None => match config.device(&e.device.key) {
                Some(a) => (a.mask, MaskSource::Explicit),
                None => (e.device.kind.default_mask(), MaskSource::Default),
            },
        };
        DeviceView {
            device: e.device.clone(),
            effective,
            source,
            isolation: self.isolation.get(&e.device.key).copied(),
            arrived_ms: e.arrived_ms,
            port_key: port,
            container_key: container,
        }
    }

    pub fn views(&self, config: &Config) -> Vec<DeviceView> {
        let eff: HashMap<DeviceKey, Effective> = self.effective(config).into_iter().map(|e| (e.key.clone(), e)).collect();
        let containers = self.container_counts();
        self.devices.values().map(|e| self.make_view_with(e, eff.get(&e.device.key).cloned(), config, &containers)).collect()
    }

    /// Live devices vs. saved assignments (spec 10 §3.8).
    pub fn diff(&self, config: &Config) -> ConfigDiff {
        let saved: HashMap<&DeviceKey, SeatMask> = config.devices.iter().map(|d| (&d.key, d.mask)).collect();
        let eff: HashMap<DeviceKey, SeatMask> = self.effective(config).into_iter().map(|e| (e.key, e.mask)).collect();
        let mut added = Vec::new();
        let mut changed = Vec::new();
        for d in self.live() {
            if saved.contains_key(&d.key) {
                continue;
            }
            match d.warnings.iter().find_map(|w| match w {
                DeviceWarning::MovedFrom { old_key } => Some(old_key),
                _ => None,
            }) {
                Some(old) => changed.push(MaskChange {
                    key: d.key.clone(),
                    saved: saved.get(old).copied().unwrap_or_default(),
                    live: eff.get(&d.key).copied().unwrap_or_else(|| d.kind.default_mask()),
                }),
                None if d.kind != DeviceKind::Network => added.push(d.key.clone()),
                None => {}
            }
        }
        let missing: Vec<DeviceKey> = config
            .devices
            .iter()
            .filter(|a| !is_pseudo(&a.key) && !self.devices.get(&a.key).is_some_and(|e| e.device.present))
            .map(|a| a.key.clone())
            .collect();
        ConfigDiff { matches: added.is_empty() && changed.is_empty() && missing.is_empty(), added, changed, missing }
    }
}

/// Port and container assignment keys (not devices themselves).
pub fn is_pseudo(key: &DeviceKey) -> bool {
    is_port_key(key) || is_container_key(key)
}

/// Presence, parent hub and label of a port / container assignment over the live devices.
fn pseudo_presence(key: &DeviceKey, live: &[Device]) -> (bool, Option<DeviceKey>, Option<String>) {
    if is_port_key(key) {
        let Some(port) = key_location(key) else { return (false, None, None) };
        let below = |d: &Device| {
            key_location(&d.key).is_some_and(|l| l == port || l.strip_prefix(port).is_some_and(|r| r.starts_with('#')))
        };
        let present = live.iter().any(|d| d.present && !is_pseudo(&d.key) && below(d));
        // The hub that owns the port sits at the port path without its last segment.
        let hub = port.rsplit_once('#').map(|(h, _)| h).and_then(|h| {
            live.iter().find(|d| d.kind == DeviceKind::UsbHub && key_location(&d.key) == Some(h)).map(|d| d.key.clone())
        });
        return (present, hub, port_chain_label(port));
    }
    if let Some(c) = key_container(key) {
        let present = live.iter().any(|d| {
            d.present && !is_pseudo(&d.key) && d.container_id.as_deref().and_then(normalize_container).as_deref() == Some(c)
        });
        return (present, None, None);
    }
    (false, None, None)
}

fn mark_moved(next: &mut BTreeMap<DeviceKey, Entry>, config: &Config) {
    let absent: Vec<_> =
        config.devices.iter().filter(|a| !is_pseudo(&a.key) && next.get(&a.key).is_some_and(|e| !e.device.present)).collect();
    if absent.is_empty() {
        return;
    }
    for e in next.values_mut() {
        let d = &mut e.device;
        if !d.present || config.device(&d.key).is_some() {
            continue;
        }
        let hit = absent.iter().find(|a| {
            a.kind == d.kind
                && ((a.serial.is_some() && a.serial == d.serial && a.hardware_id == d.hardware_id)
                    || (a.container_id.is_some() && a.container_id == d.container_id))
        });
        if let Some(a) = hit {
            let w = DeviceWarning::MovedFrom { old_key: a.key.clone() };
            if !d.warnings.contains(&w) {
                d.warnings.push(w);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::{NoneAudio, NoneDisplay, NoneInput};
    use seat_core::config::DeviceAssignment;
    use seat_core::mock::{MockSessionBackend, device};
    use seat_core::{SeatNumber, backend::session::SessionBackendKind};
    use std::sync::Arc;

    fn none_backends() -> Backends {
        Backends {
            session: Arc::new(MockSessionBackend::new(SessionBackendKind::FastUserSwitch)),
            display: Arc::new(NoneDisplay),
            input: Arc::new(NoneInput),
            audio: Arc::new(NoneAudio(AudioBackendKind::None)),
            gates: Vec::new(),
            fallback_notes: Vec::new(),
        }
    }

    fn assign(key: &str, kind: DeviceKind, mask: SeatMask) -> DeviceAssignment {
        DeviceAssignment {
            key: DeviceKey::new(key).unwrap(),
            kind,
            mask,
            name: key.into(),
            label: None,
            hardware_id: Some("USB\\VID_1&PID_2".into()),
            serial: Some("S1".into()),
            container_id: None,
            options: Default::default(),
        }
    }

    #[test]
    fn baseline_used_when_uncovered_and_deduped() {
        let b = none_backends();
        let (devs, errs) = collect(&b, vec![device("kbd;a", DeviceKind::Keyboard), device("kbd;a", DeviceKind::Keyboard)]);
        assert!(errs.is_empty());
        assert_eq!(devs.len(), 1);
    }

    #[test]
    fn rebuild_reports_changes_and_absent_devices() {
        let mut inv = Inventory::default();
        let mut c = Config::default();
        c.seats.push(seat_core::config::SeatConfig::new(SeatNumber::new(2).unwrap()));
        c.devices.push(assign("kbd;old", DeviceKind::Keyboard, SeatNumber::new(2).unwrap().mask()));
        let ch = inv.rebuild(vec![device("mon;1", DeviceKind::Monitor)], &c, 5);
        assert_eq!(ch.arrived.len(), 1);
        assert!(!inv.get(&DeviceKey::new("kbd;old").unwrap()).unwrap().device.present);
        // The keyboard turns up on a new port.
        let mut moved = device("kbd;new", DeviceKind::Keyboard);
        moved.hardware_id = Some("USB\\VID_1&PID_2".into());
        moved.serial = Some("S1".into());
        let ch = inv.rebuild(vec![device("mon;1", DeviceKind::Monitor), moved], &c, 9);
        assert_eq!(ch.arrived, vec![DeviceKey::new("kbd;new").unwrap()]);
        let d = &inv.get(&DeviceKey::new("kbd;new").unwrap()).unwrap().device;
        assert!(matches!(&d.warnings[0], DeviceWarning::MovedFrom { old_key } if old_key.as_str() == "kbd;old"));
        let diff = inv.diff(&c);
        assert!(!diff.matches);
        assert_eq!(diff.changed[0].saved, SeatNumber::new(2).unwrap().mask());
        assert_eq!(diff.missing.len(), 1);
        assert_eq!(diff.added, vec![DeviceKey::new("mon;1").unwrap()]);
        // Monitor kept its arrival time.
        assert_eq!(inv.get(&DeviceKey::new("mon;1").unwrap()).unwrap().arrived_ms, 5);
        let ch = inv.rebuild(vec![], &c, 10);
        assert_eq!(ch.removed.len(), 2);
    }

    #[test]
    fn views_have_effective_masks() {
        let mut inv = Inventory::default();
        let c = Config::default();
        inv.rebuild(vec![device("aout;x", DeviceKind::AudioRender)], &c, 1);
        let v = inv.views(&c);
        assert_eq!(v[0].effective, SeatMask::SHARED);
        assert_eq!(v[0].source, MaskSource::Default);
    }

    #[test]
    fn port_and_container_entries() {
        let two = SeatNumber::new(2).unwrap();
        let hub_loc = "PCIROOT(0)#USBROOT(0)#USB(2)";
        let mut c = Config::default();
        c.devices.push(assign(&format!("port;loc={hub_loc}#USB(4)"), DeviceKind::UsbHub, two.mask()));
        c.devices.push(assign("cont;id=aa-bb", DeviceKind::Other, two.mask()));
        let hub = device(&format!("hub;loc={hub_loc}"), DeviceKind::UsbHub);
        let mut kbd = device(&format!("kbd;loc={hub_loc}#USB(4)#USBMI(0)"), DeviceKind::Keyboard);
        kbd.parent_hub = Some(hub.key.clone());
        kbd.container_id = Some("{AA-BB}".into());
        let mut mouse = device(&format!("mou;loc={hub_loc}#USB(4)#USBMI(1)"), DeviceKind::Mouse);
        mouse.parent_hub = Some(hub.key.clone());
        mouse.container_id = Some("{aa-bb}".into());
        let mut inv = Inventory::default();
        inv.rebuild(vec![hub.clone()], &c, 1);
        let pk = DeviceKey::new(format!("port;loc={hub_loc}#USB(4)")).unwrap();
        let port = &inv.get(&pk).unwrap().device;
        assert!(!port.present);
        assert_eq!(port.parent_hub.as_ref(), Some(&hub.key));
        // An empty assigned port is not missing.
        assert!(!inv.diff(&c).missing.contains(&pk));
        let ch = inv.rebuild(vec![hub, kbd.clone(), mouse], &c, 5);
        assert!(ch.arrived.contains(&pk));
        let views = inv.views(&c);
        let kv = views.iter().find(|v| v.device.key == kbd.key).unwrap();
        assert_eq!(kv.port_key.as_ref(), Some(&pk));
        assert_eq!(kv.container_key.as_ref().map(|k| k.as_str()), Some("cont;id=aa-bb"));
        assert_eq!(kv.effective, two.mask());
        assert!(inv.get(&DeviceKey::new("cont;id=aa-bb").unwrap()).unwrap().device.present);
    }
}
