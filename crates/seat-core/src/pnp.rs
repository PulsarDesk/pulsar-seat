// SPDX-License-Identifier: GPL-3.0-only
//! Device tree access ([`PnpTree`]) and the pure rules that turn tree data into stable
//! [`DeviceKey`]s.
//!
//! The Windows implementation (CfgMgr32 / SetupDi: parent chain, `DEVPKEY_Device_LocationPaths`,
//! container id, hardware ids, interface → devnode) is `seat_media::pnp::WinPnpTree`. The
//! service builds one and hands `Arc<dyn PnpTree>` to every backend constructor, so the
//! input, display and media crates do not depend on each other.
//!
//! Key format: `<prefix>;<anchor>[;cls=<8 hex>][;if=<n>][;col=<n>:<page>:<usage>][;sub=<x>]`
//! where anchor is `loc=<location path>` (physical bus), `bt=<address>` (Bluetooth, no
//! location path) or `inst=<instance path>` (root-enumerated / virtual). Spec 50 §4.1.1,
//! spec 40 §6.1 (`sub` = `t<target id>` for monitors), spec 60 §8.1 (`sub` = tie-breaker:
//! serial or BT address), spec 70 §5.1.

use crate::device::DeviceKind;
use crate::ids::DeviceKey;
use serde::{Deserialize, Serialize};

/// One devnode as read from the tree.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PnpNode {
    pub instance_id: String,
    pub parent: Option<String>,
    /// Setup class GUID, lowercase, no braces.
    pub class_guid: Option<String>,
    pub hardware_ids: Vec<String>,
    pub compatible_ids: Vec<String>,
    /// `DEVPKEY_Device_LocationPaths` (first entry is the preferred one).
    pub location_paths: Vec<String>,
    pub container_id: Option<String>,
    pub friendly_name: Option<String>,
    /// Enumerator: `USB`, `HID`, `ACPI`, `ROOT`, `SWD`, `BTHENUM`, `BTHLEDEVICE`, …
    pub enumerator: String,
    pub present: bool,
    /// USB serial number / Bluetooth address when the device has one.
    pub serial: Option<String>,
    /// Upper / lower filter service names on the devnode (foreign filter detection).
    pub upper_filters: Vec<String>,
    pub lower_filters: Vec<String>,
}

/// Read access to the device tree. `Send + Sync`; implementations cache and refresh on
/// hot-plug.
pub trait PnpTree: Send + Sync {
    fn node(&self, instance_id: &str) -> Option<PnpNode>;
    fn children(&self, instance_id: &str) -> Vec<String>;
    /// Devnode instance id behind a device-interface path (`\\?\HID#...#{guid}`).
    fn interface_owner(&self, interface_path: &str) -> Option<String>;
    /// Re-read the tree (after an arrival / removal notification).
    fn refresh(&self) -> Result<(), String>;
}

/// The identity anchor of a device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum Anchor {
    Location(String),
    Bluetooth(String),
    Instance(String),
}

/// Parts of a device key. Build with the helpers below, then [`KeyParts::compose`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyParts {
    pub kind: DeviceKind,
    pub anchor: Anchor,
    /// Interface class GUID (only the first 8 hex digits are kept).
    pub interface_class: Option<String>,
    pub usb_interface: Option<u8>,
    /// HID collection discriminator: (collection number, usage page, usage).
    pub collection: Option<(u8, u16, u16)>,
    /// Kind-specific sub-identity (monitor target, endpoint tie-breaker).
    pub sub: Option<String>,
}

impl KeyParts {
    pub fn new(kind: DeviceKind, anchor: Anchor) -> Self {
        Self { kind, anchor, interface_class: None, usb_interface: None, collection: None, sub: None }
    }

    pub fn compose(&self) -> DeviceKey {
        let mut s = String::from(self.kind.key_prefix());
        match &self.anchor {
            Anchor::Location(p) => s.push_str(&format!(";loc={}", clean(p))),
            Anchor::Bluetooth(a) => s.push_str(&format!(";bt={}", clean(a).to_ascii_lowercase())),
            Anchor::Instance(i) => s.push_str(&format!(";inst={}", clean(i).to_ascii_uppercase())),
        }
        if let Some(c) = &self.interface_class {
            let hex: String = c.chars().filter(|ch| ch.is_ascii_hexdigit()).take(8).collect();
            s.push_str(&format!(";cls={}", hex.to_ascii_lowercase()));
        }
        if let Some(n) = self.usb_interface {
            s.push_str(&format!(";if={n}"));
        }
        if let Some((col, page, usage)) = self.collection {
            s.push_str(&format!(";col={col}:{page:04x}:{usage:04x}"));
        }
        if let Some(sub) = &self.sub {
            s.push_str(&format!(";sub={}", clean(sub)));
        }
        DeviceKey::new(s).expect("composed keys are non-empty and printable")
    }
}

/// Keys must not contain `;` inside a part, nor control characters.
fn clean(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).map(|c| if c == ';' { '_' } else { c }).collect()
}

/// Anchor for a devnode: the nearest node (itself or an ancestor) with a location path;
/// Bluetooth devices use their address; root-enumerated / software devices their instance
/// path (spec 50 §4.1.1).
pub fn anchor_for(tree: &dyn PnpTree, instance_id: &str) -> Anchor {
    let mut cur = tree.node(instance_id);
    let mut bt: Option<String> = None;
    let mut depth = 0;
    while let Some(n) = cur {
        if let Some(p) = n.location_paths.first() {
            return Anchor::Location(p.clone());
        }
        let e = n.enumerator.to_ascii_uppercase();
        if (e == "BTHENUM" || e == "BTHLEDEVICE" || e == "BTHHFENUM") && bt.is_none() {
            bt = n.serial.clone().or_else(|| bt_address_from_instance(&n.instance_id));
        }
        if e == "ROOT" || e == "SWD" {
            break;
        }
        depth += 1;
        if depth > 64 {
            break;
        }
        cur = n.parent.as_deref().and_then(|p| tree.node(p));
    }
    match bt {
        Some(a) => Anchor::Bluetooth(a),
        None => Anchor::Instance(instance_id.to_string()),
    }
}

/// Last 12 hex digits after `_` in a Bluetooth instance path, if any.
fn bt_address_from_instance(id: &str) -> Option<String> {
    let tail = id.rsplit(['_', '&', '\\']).find(|t| t.len() >= 12 && t.chars().all(|c| c.is_ascii_hexdigit()))?;
    Some(tail[tail.len() - 12..].to_ascii_lowercase())
}

/// HID collection number from a hardware id (`HID\VID_046D&PID_C52B&MI_02&Col03` → 3).
pub fn hid_collection_number(hardware_ids: &[String]) -> Option<u8> {
    hardware_ids.iter().find_map(|h| {
        let up = h.to_ascii_uppercase();
        let i = up.find("&COL")?;
        let digits: String = up[i + 4..].chars().take_while(|c| c.is_ascii_hexdigit()).collect();
        u8::from_str_radix(&digits, 16).ok()
    })
}

/// USB interface number from a hardware / instance id (`&MI_02` → 2).
pub fn usb_interface_number(ids: &[String]) -> Option<u8> {
    ids.iter().find_map(|h| {
        let up = h.to_ascii_uppercase();
        let i = up.find("MI_")?;
        u8::from_str_radix(up.get(i + 3..i + 5)?, 16).ok()
    })
}

/// Readable port chain from a location path: `...#USBROOT(0)#USB(2)#USB(4)` → `USB-2-4`.
/// ACPI-only paths → `ACPI`. Label only, never an identity.
pub fn port_chain_label(location_path: &str) -> Option<String> {
    let ports: Vec<&str> = location_path
        .split('#')
        .filter_map(|seg| seg.strip_prefix("USB(").and_then(|r| r.strip_suffix(')')))
        .collect();
    if !ports.is_empty() {
        return Some(format!("USB-{}", ports.join("-")));
    }
    if location_path.starts_with("ACPI(") {
        return Some("ACPI".into());
    }
    None
}

// ---- Port and container assignment keys (spec 70 §5.1) ---------------------------------
//
// Besides device and hub keys, an assignment may name
// - one **USB port** of a hub: `port;loc=<location path of the port>`, where the location
//   path ends with the port's `USB(n)` segment (`...#USBROOT(0)#USB(2)#USB(4)` = port 4 of
//   the hub plugged into root port 2). Whatever is plugged into that port, at any depth,
//   inherits the mask;
// - one **container** (one physical product, e.g. a keyboard with a built-in hub and
//   card reader): `cont;id=<container GUID, lower case, no braces>`. Every device of that
//   container inherits the mask.
//
// Neither is a `DeviceKind`; the assignment records them as `UsbHub` (port) and `Other`
// (container). Both are additive: older configs never contain them.

/// Prefix of a port assignment key.
pub const PORT_KEY_PREFIX: &str = "port";
/// Prefix of a container assignment key.
pub const CONTAINER_KEY_PREFIX: &str = "cont";
/// The container id Windows gives every device that is part of the computer itself
/// (internal devices, root-enumerated devices). Never a grouping key.
pub const SYSTEM_CONTAINER_ID: &str = "00000000-0000-0000-ffff-ffffffffffff";

/// Port path of a location path: the prefix up to and including its last `USB(n)` segment
/// (`...#USB(2)#USB(4)#USBMI(0)` → `...#USB(2)#USB(4)`). `None` when the device is not
/// behind a USB port (root hubs, PCI, ACPI).
pub fn port_of_location(location_path: &str) -> Option<String> {
    let segs: Vec<&str> = location_path.split('#').collect();
    let last = segs.iter().rposition(|s| s.starts_with("USB(") && s.ends_with(')'))?;
    Some(segs[..=last].join("#"))
}

/// Assignment key of one USB port (`location_path` = the port path, see
/// [`port_of_location`]).
pub fn port_key(port_path: &str) -> DeviceKey {
    DeviceKey::new(format!("{PORT_KEY_PREFIX};loc={}", clean(port_path))).expect("port keys are non-empty and printable")
}

/// Normalised container id (lower case, no braces), `None` for empty ids and the system
/// container.
pub fn normalize_container(container_id: &str) -> Option<String> {
    let c = container_id.trim().trim_matches(|c| c == '{' || c == '}').to_ascii_lowercase();
    let ok = !c.is_empty() && c != SYSTEM_CONTAINER_ID && c.chars().all(|ch| ch.is_ascii_hexdigit() || ch == '-');
    ok.then_some(c)
}

/// Assignment key of a container, `None` for the system container (never a group).
pub fn container_key(container_id: &str) -> Option<DeviceKey> {
    let c = normalize_container(container_id)?;
    DeviceKey::new(format!("{CONTAINER_KEY_PREFIX};id={c}")).ok()
}

pub fn is_port_key(key: &DeviceKey) -> bool {
    key.as_str().starts_with("port;")
}

pub fn is_container_key(key: &DeviceKey) -> bool {
    key.as_str().starts_with("cont;")
}

/// The `loc=` part of a key, if its anchor is a location path.
pub fn key_location(key: &DeviceKey) -> Option<&str> {
    key.as_str().split(';').find_map(|p| p.strip_prefix("loc="))
}

/// Container id of a container key.
pub fn key_container(key: &DeviceKey) -> Option<&str> {
    key.as_str().strip_prefix("cont;id=")
}

/// Port keys above a location, nearest first (`...#USB(2)#USB(4)` → the port
/// `...#USB(2)#USB(4)`, then `...#USB(2)`).
pub fn port_chain(location_path: &str) -> Vec<DeviceKey> {
    let mut out = Vec::new();
    let mut cur = port_of_location(location_path);
    while let Some(p) = cur {
        out.push(port_key(&p));
        let parent = p.rsplit_once('#').map(|(head, _)| head.to_string());
        cur = parent.and_then(|h| port_of_location(&h));
    }
    out
}

/// Nearest USB hub ancestor (class `usb` hub: hardware id `USB\ROOT_HUB*` or compatible id
/// `USB\Class_09`), excluding the node itself.
pub fn nearest_hub(tree: &dyn PnpTree, instance_id: &str) -> Option<String> {
    let mut cur = tree.node(instance_id).and_then(|n| n.parent);
    let mut depth = 0;
    while let Some(id) = cur {
        let n = tree.node(&id)?;
        if is_usb_hub(&n) {
            return Some(id);
        }
        depth += 1;
        if depth > 64 {
            return None;
        }
        cur = n.parent;
    }
    None
}

pub fn is_usb_hub(n: &PnpNode) -> bool {
    n.hardware_ids.iter().chain(&n.compatible_ids).any(|h| {
        let u = h.to_ascii_uppercase();
        u.starts_with("USB\\ROOT_HUB") || u.starts_with("USB\\CLASS_09")
    })
}

/// Filters on the devnode that are not ours (spec 50 §4.1.6).
pub fn foreign_filters(n: &PnpNode, ours: &[&str]) -> Vec<String> {
    n.upper_filters
        .iter()
        .chain(&n.lower_filters)
        .filter(|f| !ours.iter().any(|o| o.eq_ignore_ascii_case(f)))
        .filter(|f| !matches!(f.to_ascii_lowercase().as_str(), "kbdclass" | "mouclass"))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MockPnpTree;

    fn tree() -> MockPnpTree {
        let t = MockPnpTree::default();
        t.add(PnpNode {
            instance_id: "USB\\ROOT_HUB30\\1".into(),
            hardware_ids: vec!["USB\\ROOT_HUB30".into()],
            location_paths: vec!["PCIROOT(0)#PCI(1400)#USBROOT(0)".into()],
            enumerator: "USB".into(),
            present: true,
            ..Default::default()
        });
        t.add(PnpNode {
            instance_id: "USB\\VID_1&PID_2\\5".into(),
            parent: Some("USB\\ROOT_HUB30\\1".into()),
            location_paths: vec!["PCIROOT(0)#PCI(1400)#USBROOT(0)#USB(2)".into()],
            enumerator: "USB".into(),
            present: true,
            ..Default::default()
        });
        t.add(PnpNode {
            instance_id: "HID\\VID_1&PID_2&MI_01&COL02\\7".into(),
            parent: Some("USB\\VID_1&PID_2\\5".into()),
            hardware_ids: vec!["HID\\VID_1&PID_2&MI_01&Col02".into()],
            enumerator: "HID".into(),
            present: true,
            ..Default::default()
        });
        t.add(PnpNode {
            instance_id: "SWD\\VIRT\\1".into(),
            enumerator: "SWD".into(),
            present: true,
            ..Default::default()
        });
        t
    }

    #[test]
    fn anchors() {
        let t = tree();
        assert_eq!(
            anchor_for(&t, "HID\\VID_1&PID_2&MI_01&COL02\\7"),
            Anchor::Location("PCIROOT(0)#PCI(1400)#USBROOT(0)#USB(2)".into())
        );
        assert_eq!(anchor_for(&t, "SWD\\VIRT\\1"), Anchor::Instance("SWD\\VIRT\\1".into()));
        assert_eq!(nearest_hub(&t, "HID\\VID_1&PID_2&MI_01&COL02\\7").as_deref(), Some("USB\\ROOT_HUB30\\1"));
    }

    #[test]
    fn key_compose_is_stable() {
        let mut p = KeyParts::new(DeviceKind::Mouse, Anchor::Location("A#USB(2)".into()));
        p.interface_class = Some("{378de44c-56ef-11d1-bc8c-00a0c91405dd}".into());
        p.usb_interface = usb_interface_number(&["HID\\VID_1&PID_2&MI_01".into()]);
        p.collection = Some((2, 1, 2));
        assert_eq!(p.compose().as_str(), "mou;loc=A#USB(2);cls=378de44c;if=1;col=2:0001:0002");
    }

    #[test]
    fn helpers() {
        assert_eq!(hid_collection_number(&["HID\\VID_1&PID_2&Col03".into()]), Some(3));
        assert_eq!(port_chain_label("PCIROOT(0)#USBROOT(0)#USB(2)#USB(4)").as_deref(), Some("USB-2-4"));
        assert_eq!(port_chain_label("ACPI(_SB_)#ACPI(I2C1)").as_deref(), Some("ACPI"));
        let n = PnpNode { upper_filters: vec!["kbdclass".into(), "PulsarSeatKbd".into(), "other".into()], ..Default::default() };
        assert_eq!(foreign_filters(&n, &["pulsarseatkbd"]), vec!["other".to_string()]);
    }

    #[test]
    fn port_and_container_keys() {
        let l = "PCIROOT(0)#PCI(1400)#USBROOT(0)#USB(2)#USB(4)#USBMI(0)";
        assert_eq!(port_of_location(l).as_deref(), Some("PCIROOT(0)#PCI(1400)#USBROOT(0)#USB(2)#USB(4)"));
        assert_eq!(port_of_location("PCIROOT(0)#PCI(1400)#USBROOT(0)"), None);
        let chain: Vec<String> = port_chain(l).into_iter().map(String::from).collect();
        assert_eq!(
            chain,
            vec![
                "port;loc=PCIROOT(0)#PCI(1400)#USBROOT(0)#USB(2)#USB(4)".to_string(),
                "port;loc=PCIROOT(0)#PCI(1400)#USBROOT(0)#USB(2)".to_string()
            ]
        );
        let k = DeviceKey::new(format!("stor;loc={l};sub=lun0")).unwrap();
        assert_eq!(key_location(&k), Some(l));
        let c = container_key("{AA-BB}").unwrap();
        assert_eq!(c.as_str(), "cont;id=aa-bb");
        assert!(is_container_key(&c) && !is_port_key(&c));
        assert_eq!(key_container(&c), Some("aa-bb"));
        assert!(container_key("").is_none());
        assert!(container_key("x;y").is_none());
    }
}
