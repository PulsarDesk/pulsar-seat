// SPDX-License-Identifier: GPL-3.0-only
//! Gate inventory: walks the device tree and classifies the devnodes the device gates handle
//! (spec 70 §3.1, §5.1): USB hubs, removable storage, optical drives, cameras, serial ports,
//! smart-card readers, printers, USB network adapters and other (vendor) USB devices.
//!
//! Pure over [`PnpTree`]; the Windows tree is [`crate::pnp::WinPnpTree`]. Also holds the
//! hot-plug diff used by every listener in this crate.

use crate::keys::{device_key, hub_key};
use seat_core::DeviceKey;
use seat_core::backend::HotplugEvent;
use seat_core::device::{Device, DeviceDetails, DeviceKind, DeviceWarning, StorageInfo, UsbInfo};
use seat_core::pnp::{PnpNode, PnpTree, is_usb_hub, nearest_hub, port_chain_label};
use std::collections::{BTreeMap, HashMap, HashSet};

/// Root devnode of the Windows device tree.
pub const ROOT_DEVNODE: &str = r"HTREE\ROOT\0";

/// Setup class GUIDs (lowercase, no braces; documented in the WDK "System-defined device
/// setup classes" list).
pub mod class {
    pub const DISK_DRIVE: &str = "4d36e967-e325-11ce-bfc1-08002be10318";
    pub const CDROM: &str = "4d36e965-e325-11ce-bfc1-08002be10318";
    pub const VOLUME: &str = "71a27cdd-812a-11d0-bec7-08002be2092f";
    pub const CAMERA: &str = "ca3e7ab9-b4c3-4ae6-8251-579ef933890f";
    pub const IMAGE: &str = "6bdd1fc6-810f-11d0-bec7-08002be2092f";
    pub const PORTS: &str = "4d36e978-e325-11ce-bfc1-08002be10318";
    pub const SMART_CARD_READER: &str = "50dd5230-ba8a-11d1-bf5d-0000f805f530";
    pub const PRINTER: &str = "4d36e979-e325-11ce-bfc1-08002be10318";
    pub const NET: &str = "4d36e972-e325-11ce-bfc1-08002be10318";
    pub const USB_DEVICE: &str = "88bae032-5a81-49f0-bc3d-a4ff138216d6";
}

/// One classified devnode.
#[derive(Debug, Clone, PartialEq)]
pub struct GateNode {
    pub device: Device,
    pub instance_id: String,
}

fn norm_guid(g: &str) -> String {
    g.trim_matches(|c| c == '{' || c == '}').to_ascii_lowercase()
}

fn has_compat(n: &PnpNode, prefix: &str) -> bool {
    n.compatible_ids.iter().chain(&n.hardware_ids).any(|h| h.to_ascii_uppercase().starts_with(prefix))
}

/// Kind of a devnode for the gates, or `None` when no gate cares about it. Internal disks
/// (no USB hub above, not a USB/SD enumerator) are never listed (spec 70 §3.4).
pub fn classify(tree: &dyn PnpTree, n: &PnpNode) -> Option<DeviceKind> {
    if is_usb_hub(n) {
        return Some(DeviceKind::UsbHub);
    }
    let cls = n.class_guid.as_deref().map(norm_guid).unwrap_or_default();
    let on_usb = || nearest_hub(tree, &n.instance_id).is_some();
    let enumr = n.enumerator.to_ascii_uppercase();
    match cls.as_str() {
        class::DISK_DRIVE | class::CDROM => {
            let removable_bus = matches!(enumr.as_str(), "USBSTOR" | "UASPSTOR" | "SD" | "SDBUS");
            (removable_bus || on_usb()).then_some(DeviceKind::Storage)
        }
        class::CAMERA => Some(DeviceKind::Webcam),
        // Image class also holds scanners; only USB video class functions are cameras.
        class::IMAGE if has_compat(n, r"USB\CLASS_0E") => Some(DeviceKind::Webcam),
        class::PORTS => Some(DeviceKind::SerialPort),
        class::SMART_CARD_READER => Some(DeviceKind::SmartCardReader),
        class::PRINTER if on_usb() => Some(DeviceKind::Printer),
        class::NET if on_usb() => Some(DeviceKind::Network),
        class::USB_DEVICE => Some(DeviceKind::Other),
        _ => None,
    }
}

fn hub_depth(tree: &dyn PnpTree, id: &str) -> u8 {
    let mut d = 0u8;
    let mut cur = nearest_hub(tree, id);
    while let Some(h) = cur {
        d = d.saturating_add(1);
        if d > 32 {
            break;
        }
        cur = nearest_hub(tree, &h);
    }
    d
}

fn port_of(n: &PnpNode) -> u8 {
    n.location_paths
        .first()
        .and_then(|p| p.rsplit('#').next())
        .and_then(|seg| seg.strip_prefix("USB(").and_then(|r| r.strip_suffix(')')))
        .and_then(|d| d.parse().ok())
        .unwrap_or(0)
}

fn vid_pid(ids: &[String]) -> (u16, u16) {
    let find = |tag: &str| {
        ids.iter().find_map(|h| {
            let up = h.to_ascii_uppercase();
            let i = up.find(tag)?;
            u16::from_str_radix(up.get(i + tag.len()..i + tag.len() + 4)?, 16).ok()
        })
    };
    (find("VID_").unwrap_or(0), find("PID_").unwrap_or(0))
}

/// Build the inventory record of a classified devnode.
pub fn to_device(tree: &dyn PnpTree, n: &PnpNode, kind: DeviceKind) -> Device {
    let key = if kind == DeviceKind::UsbHub { hub_key(tree, &n.instance_id) } else { device_key(tree, kind, &n.instance_id) };
    let loc = n.location_paths.first().cloned().or_else(|| {
        // Label from the nearest ancestor with a location path.
        let mut cur = n.parent.clone();
        let mut depth = 0;
        while let Some(p) = cur {
            let pn = tree.node(&p)?;
            if let Some(l) = pn.location_paths.first() {
                return Some(l.clone());
            }
            depth += 1;
            if depth > 32 {
                return None;
            }
            cur = pn.parent;
        }
        None
    });
    let ids: Vec<String> = std::iter::once(n.instance_id.clone()).chain(n.hardware_ids.iter().cloned()).collect();
    let (vid, pid) = vid_pid(&ids);
    let details = match kind {
        DeviceKind::UsbHub => DeviceDetails::Usb(UsbInfo {
            vendor_id: vid,
            product_id: pid,
            port_count: 0,
            hub_depth: hub_depth(tree, &n.instance_id),
            port: port_of(n),
        }),
        DeviceKind::Storage => DeviceDetails::Storage(StorageInfo {
            removable: true,
            optical: n.class_guid.as_deref().map(norm_guid).as_deref() == Some(class::CDROM),
            ..Default::default()
        }),
        DeviceKind::Webcam | DeviceKind::Other | DeviceKind::SerialPort | DeviceKind::Printer => {
            DeviceDetails::Usb(UsbInfo { vendor_id: vid, product_id: pid, port: port_of(n), ..Default::default() })
        }
        _ => DeviceDetails::None,
    };
    Device {
        key,
        kind,
        name: n.friendly_name.clone().unwrap_or_else(|| n.instance_id.clone()),
        label: loc.as_deref().and_then(port_chain_label),
        instance_id: Some(n.instance_id.clone()),
        container_id: n.container_id.clone(),
        parent_hub: nearest_hub(tree, &n.instance_id).map(|h| hub_key(tree, &h)),
        present: n.present,
        hardware_id: n.hardware_ids.first().cloned(),
        serial: n.serial.clone(),
        warnings: Vec::new(),
        details,
    }
}

/// Walk the whole tree from [`ROOT_DEVNODE`] and classify every devnode. Duplicate keys get
/// [`DeviceWarning::KeyCollision`] (they cannot be told apart, spec 50 §4.1.1).
pub fn scan(tree: &dyn PnpTree) -> Vec<GateNode> {
    let mut out: Vec<GateNode> = Vec::new();
    let mut stack = vec![ROOT_DEVNODE.to_string()];
    let mut seen = HashSet::new();
    while let Some(id) = stack.pop() {
        if !seen.insert(id.to_ascii_uppercase()) || seen.len() > 100_000 {
            continue;
        }
        if let Some(n) = tree.node(&id) {
            if let Some(kind) = classify(tree, &n) {
                out.push(GateNode { device: to_device(tree, &n, kind), instance_id: n.instance_id.clone() });
            }
        }
        let mut kids = tree.children(&id);
        kids.sort();
        stack.extend(kids.into_iter().rev());
    }
    let mut counts: HashMap<DeviceKey, usize> = HashMap::new();
    for g in &out {
        *counts.entry(g.device.key.clone()).or_default() += 1;
    }
    for g in &mut out {
        if counts[&g.device.key] > 1 {
            g.device.warnings.push(DeviceWarning::KeyCollision);
        }
    }
    out
}

/// Present devnodes of one setup class (`class_guid` lowercase, no braces), whole tree,
/// internal devices included (class-level default descriptors, spec 70 §5.2(2)).
pub fn class_members(tree: &dyn PnpTree, class_guid: &str) -> Vec<String> {
    let want = norm_guid(class_guid);
    let mut out = Vec::new();
    let mut stack = vec![ROOT_DEVNODE.to_string()];
    let mut seen = HashSet::new();
    while let Some(id) = stack.pop() {
        if !seen.insert(id.to_ascii_uppercase()) || seen.len() > 100_000 {
            continue;
        }
        if let Some(n) = tree.node(&id) {
            if n.present && n.class_guid.as_deref().map(norm_guid).as_deref() == Some(want.as_str()) {
                out.push(n.instance_id.clone());
            }
        }
        let mut kids = tree.children(&id);
        kids.sort();
        stack.extend(kids.into_iter().rev());
    }
    out
}

/// Hot-plug diff between two inventories (keyed by device key).
pub fn diff(old: &BTreeMap<DeviceKey, Device>, new: &BTreeMap<DeviceKey, Device>) -> Vec<HotplugEvent> {
    let mut ev = Vec::new();
    for (k, d) in new {
        match old.get(k) {
            None => ev.push(HotplugEvent::Arrived { device: Box::new(d.clone()) }),
            Some(o) if o != d => ev.push(HotplugEvent::Changed { device: Box::new(d.clone()) }),
            _ => {}
        }
    }
    for k in old.keys() {
        if !new.contains_key(k) {
            ev.push(HotplugEvent::Removed { key: k.clone() });
        }
    }
    ev
}

pub fn by_key(devices: impl IntoIterator<Item = Device>) -> BTreeMap<DeviceKey, Device> {
    devices.into_iter().map(|d| (d.key.clone(), d)).collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use seat_core::mock::MockPnpTree;

    /// Root → xHCI root hub → external hub (port 2) → USB stick (port 1) → disk;
    /// plus an internal NVMe disk, a USB webcam, a COM port and a USB NIC.
    pub(crate) fn sample_tree() -> MockPnpTree {
        let t = MockPnpTree::default();
        let add = |id: &str, parent: &str, f: &dyn Fn(&mut PnpNode)| {
            let mut n = PnpNode {
                instance_id: id.into(),
                parent: Some(parent.into()),
                enumerator: id.split('\\').next().unwrap_or("").into(),
                present: true,
                ..Default::default()
            };
            f(&mut n);
            t.add(n);
        };
        t.add(PnpNode { instance_id: ROOT_DEVNODE.into(), enumerator: "HTREE".into(), present: true, ..Default::default() });
        add(r"PCI\VEN_1&DEV_2\3", ROOT_DEVNODE, &|n| n.location_paths = vec!["PCIROOT(0)#PCI(1400)".into()]);
        add(r"USB\ROOT_HUB30\4", r"PCI\VEN_1&DEV_2\3", &|n| {
            n.hardware_ids = vec![r"USB\ROOT_HUB30".into()];
            n.location_paths = vec!["PCIROOT(0)#PCI(1400)#USBROOT(0)".into()];
        });
        add(r"USB\VID_05E3&PID_0610\5", r"USB\ROOT_HUB30\4", &|n| {
            n.compatible_ids = vec![r"USB\Class_09".into()];
            n.location_paths = vec!["PCIROOT(0)#PCI(1400)#USBROOT(0)#USB(2)".into()];
        });
        add(r"USB\VID_0781&PID_5581\SN1", r"USB\VID_05E3&PID_0610\5", &|n| {
            n.hardware_ids = vec![r"USB\VID_0781&PID_5581".into()];
            n.location_paths = vec!["PCIROOT(0)#PCI(1400)#USBROOT(0)#USB(2)#USB(1)".into()];
            n.serial = Some("SN1".into());
        });
        add(r"USBSTOR\DISK&VEN_X\SN1&0", r"USB\VID_0781&PID_5581\SN1", &|n| {
            n.class_guid = Some(class::DISK_DRIVE.into());
            n.friendly_name = Some("USB Stick".into());
        });
        add(r"SCSI\DISK&VEN_NVME\1&0", r"PCI\VEN_1&DEV_2\3", &|n| n.class_guid = Some(class::DISK_DRIVE.into()));
        add(r"USB\VID_046D&PID_085E&MI_00\6", r"USB\VID_05E3&PID_0610\5", &|n| {
            n.class_guid = Some(class::CAMERA.into());
            n.location_paths = vec!["PCIROOT(0)#PCI(1400)#USBROOT(0)#USB(2)#USB(3)".into()];
        });
        add(r"FTDIBUS\VID_0403+A\0000", r"USB\VID_05E3&PID_0610\5", &|n| n.class_guid = Some(class::PORTS.into()));
        add(r"USB\VID_0BDA&PID_8153\7", r"USB\VID_05E3&PID_0610\5", &|n| {
            n.class_guid = Some(class::NET.into());
            n.location_paths = vec!["PCIROOT(0)#PCI(1400)#USBROOT(0)#USB(2)#USB(4)".into()];
        });
        t
    }

    #[test]
    fn scan_classifies() {
        let t = sample_tree();
        let g = scan(&t);
        let kinds: Vec<_> = g.iter().map(|g| (g.device.kind, g.instance_id.as_str())).collect();
        assert!(kinds.contains(&(DeviceKind::UsbHub, r"USB\ROOT_HUB30\4")));
        assert!(kinds.contains(&(DeviceKind::UsbHub, r"USB\VID_05E3&PID_0610\5")));
        assert!(kinds.contains(&(DeviceKind::Storage, r"USBSTOR\DISK&VEN_X\SN1&0")));
        assert!(kinds.contains(&(DeviceKind::Webcam, r"USB\VID_046D&PID_085E&MI_00\6")));
        assert!(kinds.contains(&(DeviceKind::SerialPort, r"FTDIBUS\VID_0403+A\0000")));
        assert!(kinds.contains(&(DeviceKind::Network, r"USB\VID_0BDA&PID_8153\7")));
        assert!(!kinds.iter().any(|(_, id)| id.starts_with("SCSI")), "internal disk must not be listed");
        let disk = g.iter().find(|g| g.device.kind == DeviceKind::Storage).unwrap();
        assert_eq!(disk.device.key.as_str(), "stor;loc=PCIROOT(0)#PCI(1400)#USBROOT(0)#USB(2)#USB(1);sub=lun0");
        assert_eq!(disk.device.parent_hub.as_ref().unwrap().as_str(), "hub;loc=PCIROOT(0)#PCI(1400)#USBROOT(0)#USB(2)");
        assert_eq!(disk.device.label.as_deref(), Some("USB-2-1"));
        let hub = g.iter().find(|g| g.instance_id == r"USB\VID_05E3&PID_0610\5").unwrap();
        match &hub.device.details {
            DeviceDetails::Usb(u) => {
                assert_eq!(u.hub_depth, 1);
                assert_eq!(u.port, 2);
                assert_eq!(u.vendor_id, 0x05e3);
            }
            d => panic!("{d:?}"),
        }
    }

    #[test]
    fn diff_events() {
        let t = sample_tree();
        let a = by_key(scan(&t).into_iter().map(|g| g.device));
        let mut b = a.clone();
        let first = b.keys().next().unwrap().clone();
        b.remove(&first);
        let ev = diff(&a, &b);
        assert_eq!(ev, vec![HotplugEvent::Removed { key: first }]);
        assert!(diff(&a, &a).is_empty());
        let ev = diff(&b, &a);
        assert!(matches!(ev[0], HotplugEvent::Arrived { .. }));
    }
}
