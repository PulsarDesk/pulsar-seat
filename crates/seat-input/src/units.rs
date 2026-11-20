// SPDX-License-Identifier: GPL-3.0-only
//! Input units and their stable keys (spec 50 §2.1, §4.1.1).
//!
//! A unit is one device interface: a keyboard devnode, a mouse devnode or a HID top-level
//! collection that is neither. The key is built by [`seat_core::pnp::KeyParts`]:
//! ancestor location path (or Bluetooth address / instance path) + interface class + USB
//! interface number + collection discriminator (collection number, usage page, usage).
//!
//! Pure: the caller supplies the interface paths (SetupDi / CfgMgr32 on Windows, a list in
//! tests) and a [`PnpTree`].

use seat_core::device::{Device, DeviceDetails, DeviceKind, DeviceWarning, InputClass, InputInfo};
use seat_core::ids::DeviceKey;
use seat_core::pnp::{
    Anchor, KeyParts, PnpNode, PnpTree, anchor_for, foreign_filters, hid_collection_number, nearest_hub,
    port_chain_label, usb_interface_number,
};
use std::collections::{HashMap, HashSet};

/// Service name of our own filter driver (`drivers/seat-input-filter`), ignored by the
/// foreign-filter check.
pub const OUR_FILTER_SERVICE: &str = "PulsarSeatInput";

/// Live device-interface paths per class.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InterfaceList {
    pub keyboards: Vec<String>,
    pub mice: Vec<String>,
    pub hid: Vec<String>,
}

/// HID usage `(page, usage)` from a hardware / compatible id of the form
/// `HID_DEVICE_UP:pppp_U:uuuu` (spec 50 §2.5), falling back to the well-known
/// `HID_DEVICE_SYSTEM_*` ids.
pub fn hid_usage(ids: &[String]) -> Option<(u16, u16)> {
    for id in ids {
        let up = id.to_ascii_uppercase();
        if let Some(i) = up.find("HID_DEVICE_UP:") {
            let rest = &up[i + "HID_DEVICE_UP:".len()..];
            let page = u16::from_str_radix(rest.get(..4)?, 16).ok()?;
            let j = rest.find("_U:")?;
            let usage = u16::from_str_radix(rest.get(j + 3..j + 7)?, 16).ok()?;
            return Some((page, usage));
        }
    }
    for id in ids {
        let up = id.to_ascii_uppercase();
        let v = match up.as_str() {
            "HID_DEVICE_SYSTEM_KEYBOARD" => (0x01, 0x06),
            "HID_DEVICE_SYSTEM_MOUSE" => (0x01, 0x02),
            "HID_DEVICE_SYSTEM_GAME" => (0x01, 0x05),
            "HID_DEVICE_SYSTEM_CONSUMER" => (0x0C, 0x01),
            "HID_DEVICE_SYSTEM_CONTROL" => (0x01, 0x80),
            _ => continue,
        };
        return Some(v);
    }
    None
}

/// `VID_xxxx` / `PID_xxxx` (USB, HID) or `VID&0002xxxx_PID&xxxx` (Bluetooth) from ids.
pub fn vid_pid(ids: &[String]) -> (u16, u16) {
    fn after(up: &str, tag: &str) -> Option<u16> {
        let i = up.find(tag)?;
        let rest = &up[i + tag.len()..];
        let rest = rest.trim_start_matches(['_', '&']);
        // Bluetooth form `VID&0002045E`: the last four hex digits of the 8-digit group.
        let hex: String = rest.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
        let hex = if hex.len() == 8 { &hex[4..] } else { hex.get(..4)? };
        u16::from_str_radix(hex, 16).ok()
    }
    for id in ids {
        let up = id.to_ascii_uppercase();
        if let (Some(v), Some(p)) = (after(&up, "VID"), after(&up, "PID")) {
            return (v, p);
        }
    }
    (0, 0)
}

/// Devices the filter and the router never handle: the remote-desktop input bus (spec 50
/// §2.1 exclusion list). Root-enumerated units are kept (seat 1 by default, key `inst=`).
pub fn is_excluded(tree: &dyn PnpTree, node: &PnpNode) -> bool {
    let mut cur = Some(node.clone());
    let mut depth = 0;
    while let Some(n) = cur {
        let id = n.instance_id.to_ascii_uppercase();
        if n.enumerator.eq_ignore_ascii_case("TERMINPUT_BUS")
            || id.starts_with("TERMINPUT_BUS\\")
            || n.hardware_ids.iter().any(|h| h.to_ascii_uppercase().contains("TERMINPUT"))
        {
            return true;
        }
        depth += 1;
        if depth > 64 {
            break;
        }
        cur = n.parent.as_deref().and_then(|p| tree.node(p));
    }
    false
}

/// Vendor-defined collections are not assignable units (spec 50 §2.1).
pub fn is_vendor_defined(page: u16) -> bool {
    page >= 0xFF00
}

/// Bus name for [`InputInfo::bus`].
pub fn bus_of(tree: &dyn PnpTree, node: &PnpNode) -> &'static str {
    let mut chain = Vec::new();
    let mut cur = Some(node.clone());
    while let Some(n) = cur {
        if chain.len() > 64 {
            break;
        }
        cur = n.parent.as_deref().and_then(|p| tree.node(p));
        chain.push(n);
    }
    let has = |f: &dyn Fn(&PnpNode) -> bool| chain.iter().any(f);
    let ids_contain = |n: &PnpNode, s: &str| {
        n.hardware_ids.iter().chain(&n.compatible_ids).any(|h| h.to_ascii_uppercase().contains(s))
    };
    if has(&|n| matches!(n.enumerator.to_ascii_uppercase().as_str(), "BTHENUM" | "BTHLEDEVICE" | "BTHHFENUM")) {
        return "bluetooth";
    }
    if has(&|n| n.enumerator.eq_ignore_ascii_case("USB")) {
        return "usb";
    }
    if has(&|n| ids_contain(n, "PNP0C50")) {
        return "i2c";
    }
    if has(&|n| ids_contain(n, "SPI") || n.enumerator.to_ascii_uppercase().contains("SPI")) {
        return "spi";
    }
    if node.enumerator.eq_ignore_ascii_case("ACPI") {
        // Non-HID keyboard / mouse on ACPI: the i8042 (PS/2) port.
        return "ps2";
    }
    if has(&|n| matches!(n.enumerator.to_ascii_uppercase().as_str(), "ROOT" | "SWD")) {
        return "virtual";
    }
    "other"
}

/// Key of a USB hub devnode, used for [`Device::parent_hub`]. Same composition as any
/// other key: `hub;<anchor>`.
pub fn hub_key(tree: &dyn PnpTree, hub_instance: &str) -> DeviceKey {
    KeyParts::new(DeviceKind::UsbHub, anchor_for(tree, hub_instance)).compose()
}

fn is_hid_collection(node: &PnpNode) -> bool {
    node.enumerator.eq_ignore_ascii_case("HID") || node.instance_id.to_ascii_uppercase().starts_with("HID\\")
}

fn kind_of(class: InputClass) -> DeviceKind {
    match class {
        InputClass::Keyboard => DeviceKind::Keyboard,
        InputClass::Mouse => DeviceKind::Mouse,
        InputClass::Hid => DeviceKind::Hid,
    }
}

/// Fallback names when Windows has no friendly name (UI copy, localized by `seat_core::i18n`).
pub mod fallback {
    use seat_core::i18n::Text;
    pub const KEYBOARD: Text = Text::new("Keyboard", "Klavye");
    pub const MOUSE: Text = Text::new("Mouse", "Fare");
    pub const TOUCHSCREEN: Text = Text::new("Touch screen", "Dokunmatik ekran");
    pub const PEN: Text = Text::new("Pen", "Kalem");
    pub const TOUCHPAD: Text = Text::new("Touchpad", "Dokunmatik yüzey");
    pub const GAMEPAD: Text = Text::new("Gamepad", "Oyun kumandası");
    pub const MEDIA_KEYS: Text = Text::new("Media keys", "Medya tuşları");
    pub const SYSTEM_BUTTONS: Text = Text::new("System buttons", "Sistem düğmeleri");
    pub const LIGHTING: Text = Text::new("Lighting", "Aydınlatma");
    pub const HID: Text = Text::new("HID device", "HID aygıtı");
    #[cfg(test)]
    pub const ALL: &[(&str, &Text)] =
        seat_core::text_table![KEYBOARD, MOUSE, TOUCHSCREEN, PEN, TOUCHPAD, GAMEPAD, MEDIA_KEYS, SYSTEM_BUTTONS, LIGHTING, HID];
}

/// Fallback name when Windows has no friendly name, in the process language.
pub fn fallback_name(class: InputClass, page: u16, usage: u16) -> &'static str {
    fallback_text(class, page, usage).get()
}

pub fn fallback_text(class: InputClass, page: u16, usage: u16) -> &'static seat_core::i18n::Text {
    match (class, page, usage) {
        (InputClass::Keyboard, ..) => &fallback::KEYBOARD,
        (InputClass::Mouse, ..) => &fallback::MOUSE,
        (_, 0x0D, 0x04) => &fallback::TOUCHSCREEN,
        (_, 0x0D, 0x02) => &fallback::PEN,
        (_, 0x0D, 0x05) => &fallback::TOUCHPAD,
        (_, 0x01, 0x04) | (_, 0x01, 0x05) => &fallback::GAMEPAD,
        (_, 0x0C, _) => &fallback::MEDIA_KEYS,
        (_, 0x01, 0x80) => &fallback::SYSTEM_BUTTONS,
        (_, 0x59, _) => &fallback::LIGHTING,
        _ => &fallback::HID,
    }
}

/// Build one unit.
pub fn build_unit(tree: &dyn PnpTree, class: InputClass, interface_path: &str) -> Option<Device> {
    let owner = tree.interface_owner(interface_path)?;
    let node = tree.node(&owner)?;
    if is_excluded(tree, &node) {
        return None;
    }
    let kind = kind_of(class);
    let hid = is_hid_collection(&node);
    let ids: Vec<String> = node.hardware_ids.iter().chain(&node.compatible_ids).cloned().collect();
    let (page, usage) = if hid { hid_usage(&ids).unwrap_or((0, 0)) } else { (0, 0) };
    if class == InputClass::Hid && is_vendor_defined(page) {
        return None;
    }
    let anchor = anchor_for(tree, &owner);
    let mut parts = KeyParts::new(kind, anchor.clone());
    parts.interface_class = kind.interface_class().map(str::to_string);
    let mut mi_ids = node.hardware_ids.clone();
    mi_ids.push(node.instance_id.clone());
    parts.usb_interface = usb_interface_number(&mi_ids);
    if hid {
        parts.collection = Some((hid_collection_number(&node.hardware_ids).unwrap_or(0), page, usage));
    }
    let parent = node.parent.as_deref().and_then(|p| tree.node(p));
    let bus = bus_of(tree, &node);
    let label = match &anchor {
        Anchor::Location(p) => port_chain_label(p).or_else(|| Some(bus.to_ascii_uppercase())),
        Anchor::Bluetooth(_) => Some("Bluetooth".into()),
        Anchor::Instance(_) => None,
    };
    let name = node
        .friendly_name
        .clone()
        .or_else(|| parent.as_ref().and_then(|p| p.friendly_name.clone()))
        .unwrap_or_else(|| fallback_name(class, page, usage).to_string());
    let mut vp_ids = ids.clone();
    if let Some(p) = &parent {
        vp_ids.extend(p.hardware_ids.iter().cloned());
    }
    let (vendor_id, product_id) = vid_pid(&vp_ids);
    let serial = {
        let mut cur = Some(node.clone());
        let mut found = None;
        let mut depth = 0;
        while let Some(n) = cur {
            if n.serial.is_some() {
                found = n.serial.clone();
                break;
            }
            depth += 1;
            if depth > 8 {
                break;
            }
            cur = n.parent.as_deref().and_then(|p| tree.node(p));
        }
        found
    };
    let warnings = foreign_filters(&node, &[OUR_FILTER_SERVICE])
        .into_iter()
        .map(|name| DeviceWarning::ForeignFilter { name })
        .collect();
    Some(Device {
        key: parts.compose(),
        kind,
        name,
        label,
        instance_id: Some(owner.clone()),
        container_id: node.container_id.clone().or_else(|| parent.as_ref().and_then(|p| p.container_id.clone())),
        parent_hub: nearest_hub(tree, &owner).map(|h| hub_key(tree, &h)),
        present: node.present,
        hardware_id: node.hardware_ids.first().cloned(),
        serial,
        warnings,
        details: DeviceDetails::Input(InputInfo {
            class,
            usage_page: page,
            usage,
            vendor_id,
            product_id,
            interface_path: interface_path.to_string(),
            bus: bus.to_string(),
            built_in: matches!(bus, "i2c" | "spi" | "ps2"),
            last_activity_ms: 0,
        }),
    })
}

/// All units of the interface list. HID interfaces of devnodes that already have a
/// keyboard or mouse interface are skipped (same devnode, one unit). Units sharing a key
/// get [`DeviceWarning::KeyCollision`] (spec 50 §4.1.1: same seat, one config entry).
pub fn build_units(tree: &dyn PnpTree, list: &InterfaceList) -> Vec<Device> {
    let mut out = Vec::new();
    let mut kbdmou_owners = HashSet::new();
    for (class, paths) in [(InputClass::Keyboard, &list.keyboards), (InputClass::Mouse, &list.mice)] {
        for p in paths {
            if let Some(d) = build_unit(tree, class, p) {
                if let Some(i) = &d.instance_id {
                    kbdmou_owners.insert(i.to_ascii_uppercase());
                }
                out.push(d);
            }
        }
    }
    for p in &list.hid {
        let Some(owner) = tree.interface_owner(p) else { continue };
        if kbdmou_owners.contains(&owner.to_ascii_uppercase()) {
            continue;
        }
        if let Some(d) = build_unit(tree, InputClass::Hid, p) {
            out.push(d);
        }
    }
    let mut count: HashMap<DeviceKey, usize> = HashMap::new();
    for d in &out {
        *count.entry(d.key.clone()).or_default() += 1;
    }
    for d in &mut out {
        if count[&d.key] > 1 && !d.warnings.contains(&DeviceWarning::KeyCollision) {
            d.warnings.push(DeviceWarning::KeyCollision);
        }
    }
    out.sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| a.key.cmp(&b.key)));
    out
}

/// Input info of a device, if it is an input unit.
pub fn input_info(d: &Device) -> Option<&InputInfo> {
    match &d.details {
        DeviceDetails::Input(i) => Some(i),
        _ => None,
    }
}

/// Interface path normalized for comparison with raw input device names: lowercase,
/// `\??\` prefix (older raw input names) turned into `\\?\`.
pub fn normalize_interface_path(p: &str) -> String {
    let p = p.trim_end_matches('\0');
    let p = if let Some(rest) = p.strip_prefix("\\??\\") { format!("\\\\?\\{rest}") } else { p.to_string() };
    p.to_ascii_lowercase()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use seat_core::mock::MockPnpTree;

    pub const KBD_PATH: &str = r"\\?\HID#VID_046D&PID_C52B&MI_00#7&1&0&0000#{884b96c3-56ef-11d1-bc8c-00a0c91405dd}";
    pub const KBD_HID_PATH: &str = r"\\?\HID#VID_046D&PID_C52B&MI_00#7&1&0&0000#{4d1e55b2-f16f-11cf-88cb-001111000030}";
    pub const MOU_PATH: &str = r"\\?\HID#VID_046D&PID_C52B&MI_01&Col01#7&2&0&0000#{378de44c-56ef-11d1-bc8c-00a0c91405dd}";
    pub const CONS_PATH: &str = r"\\?\HID#VID_046D&PID_C52B&MI_01&Col02#7&2&0&0001#{4d1e55b2-f16f-11cf-88cb-001111000030}";
    pub const VENDOR_PATH: &str = r"\\?\HID#VID_046D&PID_C52B&MI_02&Col01#7&3&0&0000#{4d1e55b2-f16f-11cf-88cb-001111000030}";
    pub const TOUCH_PATH: &str = r"\\?\HID#ELAN0001&Col01#5&1&0&0000#{4d1e55b2-f16f-11cf-88cb-001111000030}";
    pub const PEN_PATH: &str = r"\\?\HID#ELAN0001&Col02#5&1&0&0001#{4d1e55b2-f16f-11cf-88cb-001111000030}";
    pub const KBD2_PATH: &str = r"\\?\HID#VID_04F2&PID_0112#6&9&0&0000#{884b96c3-56ef-11d1-bc8c-00a0c91405dd}";
    pub const RDP_PATH: &str =r"\\?\TERMINPUT_BUS#UMB#1&1&0&RDP_KBD#{884b96c3-56ef-11d1-bc8c-00a0c91405dd}";

    fn node(id: &str, parent: Option<&str>, enumerator: &str) -> PnpNode {
        PnpNode {
            instance_id: id.into(),
            parent: parent.map(str::to_string),
            enumerator: enumerator.into(),
            present: true,
            ..Default::default()
        }
    }

    pub fn tree() -> MockPnpTree {
        let t = MockPnpTree::default();
        let mut hub = node(r"USB\ROOT_HUB30\4&1", None, "USB");
        hub.hardware_ids = vec![r"USB\ROOT_HUB30".into()];
        hub.location_paths = vec!["PCIROOT(0)#PCI(1400)#USBROOT(0)".into()];
        t.add(hub);
        let mut comp = node(r"USB\VID_046D&PID_C52B\5&1", Some(r"USB\ROOT_HUB30\4&1"), "USB");
        comp.location_paths = vec!["PCIROOT(0)#PCI(1400)#USBROOT(0)#USB(3)".into()];
        comp.friendly_name = Some("USB Receiver".into());
        comp.container_id = Some("c1".into());
        t.add(comp);
        for (mi, loc) in [("00", 0), ("01", 1), ("02", 2)] {
            let mut n = node(&format!(r"USB\VID_046D&PID_C52B&MI_{mi}\6&{loc}"), Some(r"USB\VID_046D&PID_C52B\5&1"), "USB");
            n.location_paths = vec![format!("PCIROOT(0)#PCI(1400)#USBROOT(0)#USB(3)#USBMI({loc})")];
            t.add(n);
        }
        let mut kbd = node(r"HID\VID_046D&PID_C52B&MI_00\7&1&0&0000", Some(r"USB\VID_046D&PID_C52B&MI_00\6&0"), "HID");
        kbd.hardware_ids = vec![r"HID\VID_046D&PID_C52B&MI_00".into()];
        kbd.compatible_ids = vec!["HID_DEVICE_SYSTEM_KEYBOARD".into(), "HID_DEVICE_UP:0001_U:0006".into()];
        kbd.upper_filters = vec!["kbdclass".into(), "OtherKbdFilter".into()];
        kbd.friendly_name = Some("HID Keyboard Device".into());
        t.add(kbd);
        let mut mou = node(r"HID\VID_046D&PID_C52B&MI_01&COL01\7&2&0&0000", Some(r"USB\VID_046D&PID_C52B&MI_01\6&1"), "HID");
        mou.hardware_ids = vec![r"HID\VID_046D&PID_C52B&MI_01&Col01".into()];
        mou.compatible_ids = vec!["HID_DEVICE_UP:0001_U:0002".into()];
        t.add(mou);
        let mut cons = node(r"HID\VID_046D&PID_C52B&MI_01&COL02\7&2&0&0001", Some(r"USB\VID_046D&PID_C52B&MI_01\6&1"), "HID");
        cons.hardware_ids = vec![r"HID\VID_046D&PID_C52B&MI_01&Col02".into()];
        cons.compatible_ids = vec!["HID_DEVICE_UP:000C_U:0001".into()];
        t.add(cons);
        let mut vend = node(r"HID\VID_046D&PID_C52B&MI_02&COL01\7&3&0&0000", Some(r"USB\VID_046D&PID_C52B&MI_02\6&2"), "HID");
        vend.hardware_ids = vec![r"HID\VID_046D&PID_C52B&MI_02&Col01".into()];
        vend.compatible_ids = vec!["HID_DEVICE_UP:FF00_U:0001".into()];
        t.add(vend);
        // I2C touch controller: touch + pen siblings (spec 50 §2.2 collision case).
        let mut i2c = node(r"ACPI\ELAN0001\1", None, "ACPI");
        i2c.compatible_ids = vec!["PNP0C50".into()];
        i2c.location_paths = vec!["ACPI(_SB_)#ACPI(I2C1)#ACPI(TPL0)".into()];
        t.add(i2c);
        for (col, usage) in [("01", "0004"), ("02", "0002")] {
            let mut n = node(&format!(r"HID\ELAN0001&COL{col}\5&1&0&00{}", col.parse::<u8>().unwrap() - 1), Some(r"ACPI\ELAN0001\1"), "HID");
            n.hardware_ids = vec![format!(r"HID\ELAN0001&Col{col}")];
            n.compatible_ids = vec![format!("HID_DEVICE_UP:000D_U:{usage}")];
            t.add(n);
        }
        let rdp = node(r"TERMINPUT_BUS\UMB\1&1&0&RDP_KBD", None, "TERMINPUT_BUS");
        t.add(rdp);
        // A second, single-interface USB keyboard on port 4 (not in `list()`; the
        // winusb_capture tests add it).
        let mut kb2_usb = node(r"USB\VID_04F2&PID_0112\5&2", Some(r"USB\ROOT_HUB30\4&1"), "USB");
        kb2_usb.location_paths = vec!["PCIROOT(0)#PCI(1400)#USBROOT(0)#USB(4)".into()];
        t.add(kb2_usb);
        let mut kb2 = node(r"HID\VID_04F2&PID_0112\6&9&0&0000", Some(r"USB\VID_04F2&PID_0112\5&2"), "HID");
        kb2.hardware_ids = vec![r"HID\VID_04F2&PID_0112".into()];
        kb2.compatible_ids = vec!["HID_DEVICE_SYSTEM_KEYBOARD".into(), "HID_DEVICE_UP:0001_U:0006".into()];
        t.add(kb2);
        t.add_interface(KBD2_PATH, r"HID\VID_04F2&PID_0112\6&9&0&0000");
        t.add_interface(KBD_PATH, r"HID\VID_046D&PID_C52B&MI_00\7&1&0&0000");
        t.add_interface(KBD_HID_PATH, r"HID\VID_046D&PID_C52B&MI_00\7&1&0&0000");
        t.add_interface(MOU_PATH, r"HID\VID_046D&PID_C52B&MI_01&COL01\7&2&0&0000");
        t.add_interface(CONS_PATH, r"HID\VID_046D&PID_C52B&MI_01&COL02\7&2&0&0001");
        t.add_interface(VENDOR_PATH, r"HID\VID_046D&PID_C52B&MI_02&COL01\7&3&0&0000");
        t.add_interface(TOUCH_PATH, r"HID\ELAN0001&COL01\5&1&0&000");
        t.add_interface(PEN_PATH, r"HID\ELAN0001&COL02\5&1&0&001");
        t.add_interface(RDP_PATH, r"TERMINPUT_BUS\UMB\1&1&0&RDP_KBD");
        t
    }

    pub fn list() -> InterfaceList {
        InterfaceList {
            keyboards: vec![KBD_PATH.into(), RDP_PATH.into()],
            mice: vec![MOU_PATH.into()],
            hid: vec![KBD_HID_PATH.into(), CONS_PATH.into(), VENDOR_PATH.into(), TOUCH_PATH.into(), PEN_PATH.into()],
        }
    }

    #[test]
    fn fallback_names_are_complete() {
        let p = seat_core::i18n::check_table(fallback::ALL);
        assert!(p.is_empty(), "{p:?}");
        assert_eq!(fallback_text(InputClass::Keyboard, 1, 6).en, "Keyboard");
        assert_eq!(fallback_text(InputClass::Hid, 0x0D, 0x05).tr, "Dokunmatik yüzey");
    }

    #[test]
    fn usage_parsing() {
        assert_eq!(hid_usage(&["HID_DEVICE_UP:000D_U:0004".into()]), Some((0x0D, 0x04)));
        assert_eq!(hid_usage(&["hid_device_system_mouse".into()]), Some((1, 2)));
        assert_eq!(hid_usage(&["HID\\VID_1".into()]), None);
        assert_eq!(vid_pid(&["HID\\VID_046D&PID_C52B&MI_00".into()]), (0x046D, 0xC52B));
        assert_eq!(vid_pid(&["BTHENUM\\{00001124}_VID&0002054C_PID&0CE6".into()]), (0x054C, 0x0CE6));
    }

    #[test]
    fn units_and_keys() {
        let t = tree();
        let units = build_units(&t, &list());
        let keys: Vec<&str> = units.iter().map(|d| d.key.as_str()).collect();
        assert_eq!(units.len(), 5, "{keys:#?}");
        let kbd = units.iter().find(|d| d.kind == DeviceKind::Keyboard).unwrap();
        assert_eq!(
            kbd.key.as_str(),
            "kbd;loc=PCIROOT(0)#PCI(1400)#USBROOT(0)#USB(3)#USBMI(0);cls=884b96c3;if=0;col=0:0001:0006"
        );
        assert_eq!(kbd.label.as_deref(), Some("USB-3"));
        assert_eq!(kbd.parent_hub.as_ref().unwrap().as_str(), "hub;loc=PCIROOT(0)#PCI(1400)#USBROOT(0)");
        assert_eq!(kbd.warnings, vec![DeviceWarning::ForeignFilter { name: "OtherKbdFilter".into() }]);
        assert_eq!(kbd.container_id, None);
        let info = input_info(kbd).unwrap();
        assert_eq!((info.vendor_id, info.product_id, info.bus.as_str()), (0x046D, 0xC52B, "usb"));
        assert!(!info.built_in);
        let mou = units.iter().find(|d| d.kind == DeviceKind::Mouse).unwrap();
        assert!(mou.key.as_str().ends_with(";cls=378de44c;if=1;col=1:0001:0002"), "{}", mou.key);
        // Consumer collection is a HID unit; vendor-defined and RDP units are gone.
        let hids: Vec<&Device> = units.iter().filter(|d| d.kind == DeviceKind::Hid).collect();
        assert_eq!(hids.len(), 3);
        assert!(hids.iter().any(|d| d.key.as_str().ends_with("col=2:000c:0001")));
        // Touch and pen of one I2C device are told apart by the collection discriminator.
        let touch = hids.iter().find(|d| input_info(d).unwrap().usage == 4).unwrap();
        let pen = hids.iter().find(|d| input_info(d).unwrap().usage == 2).unwrap();
        assert_ne!(touch.key, pen.key);
        assert!(touch.warnings.is_empty());
        assert_eq!(input_info(touch).unwrap().bus, "i2c");
        assert!(input_info(touch).unwrap().built_in);
        assert_eq!(touch.name, fallback::TOUCHSCREEN.get());
        assert_eq!(touch.label.as_deref(), Some("ACPI"));
    }

    #[test]
    fn collisions_are_flagged() {
        let t = tree();
        let l = InterfaceList { hid: vec![CONS_PATH.into(), CONS_PATH.into()], ..Default::default() };
        let units = build_units(&t, &l);
        assert_eq!(units.len(), 2);
        assert!(units.iter().all(|d| d.warnings.contains(&DeviceWarning::KeyCollision)));
    }

    #[test]
    fn path_normalization() {
        assert_eq!(normalize_interface_path("\\??\\HID#A#{X}\0"), "\\\\?\\hid#a#{x}");
        assert_eq!(normalize_interface_path(KBD_PATH), KBD_PATH.to_ascii_lowercase());
    }
}
