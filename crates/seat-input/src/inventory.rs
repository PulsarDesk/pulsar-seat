// SPDX-License-Identifier: GPL-3.0-only
//! Live unit inventory shared by every router: enumeration, hot-plug diff (spec 50
//! §4.1.7), activity timestamps, key lookups.

use crate::platform::{Guard, InputPlatform};
use crate::units::{build_units, input_info, normalize_interface_path};
use seat_core::backend::{BackendResult, HotplugEvent, HotplugSink};
use seat_core::device::{Device, DeviceDetails};
use seat_core::ids::DeviceKey;
use seat_core::pnp::PnpTree;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, Weak};

pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub struct Inventory {
    platform: Arc<dyn InputPlatform>,
    pnp: Arc<dyn PnpTree>,
    devices: Mutex<Vec<Device>>,
    hotplug: Mutex<Option<HotplugSink>>,
    watch: Mutex<Option<Box<dyn Guard>>>,
}

impl Inventory {
    pub fn new(platform: Arc<dyn InputPlatform>, pnp: Arc<dyn PnpTree>) -> Arc<Self> {
        Arc::new(Self {
            platform,
            pnp,
            devices: Mutex::new(Vec::new()),
            hotplug: Mutex::new(None),
            watch: Mutex::new(None),
        })
    }

    pub fn platform(&self) -> &Arc<dyn InputPlatform> {
        &self.platform
    }

    pub fn pnp(&self) -> &Arc<dyn PnpTree> {
        &self.pnp
    }

    fn scan(&self) -> BackendResult<Vec<Device>> {
        let list = self.platform.list_interfaces()?;
        let mut units = build_units(&*self.pnp, &list);
        let old = lock(&self.devices);
        let last: HashMap<&DeviceKey, u64> =
            old.iter().filter_map(|d| input_info(d).map(|i| (&d.key, i.last_activity_ms))).collect();
        for d in &mut units {
            if let (Some(t), DeviceDetails::Input(i)) = (last.get(&d.key), &mut d.details) {
                i.last_activity_ms = *t;
            }
        }
        Ok(units)
    }

    /// Fresh enumeration (also the cache the lookups use).
    pub fn enumerate(&self) -> BackendResult<Vec<Device>> {
        let units = self.scan()?;
        *lock(&self.devices) = units.clone();
        Ok(units)
    }

    pub fn devices(&self) -> Vec<Device> {
        lock(&self.devices).clone()
    }

    /// Re-read the tree and report arrivals, removals and changes to the hot-plug sink.
    pub fn rescan(&self) -> Vec<HotplugEvent> {
        let _ = self.pnp.refresh();
        let Ok(new) = self.scan() else { return Vec::new() };
        let events = {
            let mut cur = lock(&self.devices);
            let events = diff(&cur, &new);
            *cur = new;
            events
        };
        let sink = lock(&self.hotplug).clone();
        if let Some(s) = sink {
            for e in &events {
                s(e.clone());
            }
        }
        events
    }

    pub fn set_hotplug_sink(self: &Arc<Self>, sink: HotplugSink) {
        *lock(&self.hotplug) = Some(sink);
        let mut w = lock(&self.watch);
        if w.is_none() {
            let weak: Weak<Self> = Arc::downgrade(self);
            // The strong reference taken here may be the last one; then the inventory (and
            // its watch guard) is dropped on the notification thread. `win::WatchGuard`
            // detects that and unregisters from another thread (no self-wait deadlock).
            let cb = Arc::new(move || {
                if let Some(inv) = weak.upgrade() {
                    inv.rescan();
                }
            });
            match self.platform.watch(cb) {
                Ok(g) => *w = Some(g),
                Err(e) => log::warn!("input hot-plug notifications unavailable: {e}"),
            }
        }
    }

    pub fn key_of_interface(&self, path: &str) -> Option<DeviceKey> {
        let p = normalize_interface_path(path);
        lock(&self.devices)
            .iter()
            .find(|d| input_info(d).is_some_and(|i| normalize_interface_path(&i.interface_path) == p))
            .map(|d| d.key.clone())
    }

    pub fn key_of_instance(&self, instance_id: &str) -> Option<DeviceKey> {
        lock(&self.devices)
            .iter()
            .find(|d| d.instance_id.as_deref().is_some_and(|i| i.eq_ignore_ascii_case(instance_id)))
            .map(|d| d.key.clone())
    }

    pub fn touch(&self, key: &DeviceKey, at_ms: u64) {
        for d in lock(&self.devices).iter_mut().filter(|d| &d.key == key) {
            if let DeviceDetails::Input(i) = &mut d.details {
                i.last_activity_ms = at_ms;
            }
        }
    }
}

/// Arrived / removed / changed between two enumerations (by key).
pub fn diff(old: &[Device], new: &[Device]) -> Vec<HotplugEvent> {
    let o: HashMap<&DeviceKey, &Device> = old.iter().map(|d| (&d.key, d)).collect();
    let n: HashMap<&DeviceKey, &Device> = new.iter().map(|d| (&d.key, d)).collect();
    let mut ev = Vec::new();
    for d in new {
        match o.get(&d.key) {
            None => ev.push(HotplugEvent::Arrived { device: Box::new(d.clone()) }),
            Some(prev) if !same(prev, d) => ev.push(HotplugEvent::Changed { device: Box::new(d.clone()) }),
            _ => {}
        }
    }
    for d in old {
        if !n.contains_key(&d.key) {
            ev.push(HotplugEvent::Removed { key: d.key.clone() });
        }
    }
    ev
}

fn same(a: &Device, b: &Device) -> bool {
    a.instance_id == b.instance_id
        && a.present == b.present
        && a.name == b.name
        && a.warnings == b.warnings
        && input_info(a).map(|i| &i.interface_path) == input_info(b).map(|i| &i.interface_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::fake::FakePlatform;
    use crate::units::tests as ut;

    #[test]
    fn hotplug_diff_reports_arrival_and_removal() {
        let p = Arc::new(FakePlatform::default());
        *p.list.lock().unwrap() = ut::list();
        let inv = Inventory::new(p.clone(), Arc::new(ut::tree()));
        let n = inv.enumerate().unwrap().len();
        let seen: Arc<Mutex<Vec<HotplugEvent>>> = Arc::default();
        let s = seen.clone();
        inv.set_hotplug_sink(Arc::new(move |e| s.lock().unwrap().push(e)));
        p.list.lock().unwrap().mice.clear();
        p.fire();
        assert_eq!(inv.devices().len(), n - 1);
        assert!(matches!(seen.lock().unwrap()[0], HotplugEvent::Removed { .. }));
        *p.list.lock().unwrap() = ut::list();
        p.fire();
        assert!(matches!(seen.lock().unwrap()[1], HotplugEvent::Arrived { .. }));
        assert!(inv.key_of_interface(&ut::MOU_PATH.to_ascii_uppercase()).is_some());
        let k = inv.key_of_instance(r"hid\vid_046d&pid_c52b&mi_00\7&1&0&0000").unwrap();
        inv.touch(&k, 42);
        inv.enumerate().unwrap();
        let d = inv.devices().into_iter().find(|d| d.key == k).unwrap();
        assert_eq!(input_info(&d).unwrap().last_activity_ms, 42, "activity survives re-enumeration");
    }
}
