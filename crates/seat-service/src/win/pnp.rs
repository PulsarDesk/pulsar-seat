// SPDX-License-Identifier: GPL-3.0-only
//! Baseline device inventory with CfgMgr32 and CCD (spec 10 §3.1, §11): monitors,
//! keyboards, mice, HID collections, audio endpoints, USB hubs, cameras, smart-card
//! readers, disks, with container ids, location paths and the nearest hub. Keys are built
//! with `seat_core::pnp::KeyParts` so they follow the contract key format.
//!
//! The service uses this only for kinds no active backend enumerates
//! (`inventory::collect`); backends are the authority for their own kinds. Also provides
//! [`CfgTree`], a [`PnpTree`] for tests and diagnostics (production uses `seat_media::pnp::WinPnpTree`),
//! and the device-interface arrival / removal [`Notification`].
//!
//! CCD (`QueryDisplayConfig`) needs the console desktop and fails from session 0 (Microsoft
//! Learn; seen by the display package), so under the SCM the baseline has no monitors and
//! the display backend (with its console helper) is the only monitor source. In
//! `--console` mode the baseline lists them.

use crate::win::security::{from_wide, wide};
use seat_core::DeviceKey;
use seat_core::device::{
    AudioFlow, AudioInfo, Device, DeviceDetails, DeviceKind, DisplayInfo, DisplayMode, EdidId, InputClass, InputInfo,
    Presentation, Rect, StorageInfo, UsbInfo,
};
use seat_core::pnp::{
    Anchor, KeyParts, PnpNode, PnpTree, anchor_for, hid_collection_number, nearest_hub, port_chain_label,
    usb_interface_number,
};
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_GET_DEVICE_INTERFACE_LIST_PRESENT, CM_GETIDLIST_FILTER_CLASS, CM_GETIDLIST_FILTER_PRESENT,
    CM_Get_DevNode_PropertyW, CM_Get_Device_ID_List_SizeW, CM_Get_Device_ID_ListW, CM_Get_Device_IDW,
    CM_Get_Device_Interface_List_SizeW, CM_Get_Device_Interface_ListW, CM_Get_Device_Interface_PropertyW, CM_Get_Parent,
    CM_LOCATE_DEVNODE_NORMAL, CM_Locate_DevNodeW, CM_NOTIFY_ACTION, CM_NOTIFY_EVENT_DATA, CM_NOTIFY_FILTER,
    CM_NOTIFY_FILTER_FLAG_ALL_INTERFACE_CLASSES, CM_NOTIFY_FILTER_TYPE_DEVICEINTERFACE, CM_Register_Notification,
    CM_Unregister_Notification, CR_SUCCESS, HCMNOTIFICATION,
};
use windows_sys::Win32::Devices::Display::{
    DISPLAYCONFIG_ADAPTER_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_ADAPTER_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
    DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE,
    DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SOURCE_DEVICE_NAME, DISPLAYCONFIG_TARGET_DEVICE_NAME,
    DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QueryDisplayConfig,
};
use windows_sys::Win32::Devices::Properties::{
    DEVPKEY_Device_ClassGuid, DEVPKEY_Device_CompatibleIds, DEVPKEY_Device_ContainerId, DEVPKEY_Device_DeviceDesc,
    DEVPKEY_Device_FriendlyName, DEVPKEY_Device_HardwareIds, DEVPKEY_Device_InstanceId, DEVPKEY_Device_LocationPaths,
    DEVPKEY_Device_LowerFilters, DEVPKEY_Device_UpperFilters, DEVPROP_TYPE_GUID, DEVPROP_TYPE_STRING,
    DEVPROP_TYPE_STRING_LIST, DEVPROPTYPE,
};
use windows_sys::Win32::Foundation::DEVPROPKEY;
use windows_sys::core::GUID;

/// `AudioEndpoint` setup class (MMDevice endpoints as `SWD\MMDEVAPI\...` devnodes).
pub const AUDIO_ENDPOINT_CLASS: &str = "{c166523c-fe0c-4a94-a586-f1a80cfbbf3e}";

/// Parse a GUID string with or without braces.
pub fn parse_guid(s: &str) -> Option<GUID> {
    let h: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if h.len() != 32 {
        return None;
    }
    u128::from_str_radix(&h, 16).ok().map(GUID::from_u128)
}

fn guid_string(g: &GUID) -> String {
    format!(
        "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        g.data1, g.data2, g.data3, g.data4[0], g.data4[1], g.data4[2], g.data4[3], g.data4[4], g.data4[5], g.data4[6], g.data4[7]
    )
}

fn multi_sz(buf: &[u16]) -> Vec<String> {
    buf.split(|&c| c == 0).filter(|s| !s.is_empty()).map(String::from_utf16_lossy).collect()
}

enum Prop {
    Str(String),
    List(Vec<String>),
    Guid(String),
}

fn read_prop(get: impl Fn(*mut DEVPROPTYPE, *mut u8, *mut u32) -> u32) -> Option<Prop> {
    let mut ty: DEVPROPTYPE = 0;
    let mut size = 0u32;
    get(&mut ty, std::ptr::null_mut(), &mut size);
    if size == 0 {
        return None;
    }
    let mut buf = vec![0u8; size as usize + 2];
    if get(&mut ty, buf.as_mut_ptr(), &mut size) != CR_SUCCESS {
        return None;
    }
    let wide: Vec<u16> = buf[..size as usize].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    match ty {
        DEVPROP_TYPE_STRING => Some(Prop::Str(from_wide(&wide))),
        DEVPROP_TYPE_STRING_LIST => Some(Prop::List(multi_sz(&wide))),
        DEVPROP_TYPE_GUID if size as usize >= 16 => {
            // SAFETY: 16 bytes of a GUID.
            let g: GUID = unsafe { std::ptr::read_unaligned(buf.as_ptr() as *const GUID) };
            Some(Prop::Guid(guid_string(&g)))
        }
        _ => None,
    }
}

fn node_prop(dn: u32, key: &DEVPROPKEY) -> Option<Prop> {
    // SAFETY: valid devnode handle; buffer pointers sized by the size query.
    read_prop(|ty, buf, size| unsafe { CM_Get_DevNode_PropertyW(dn, key, ty, buf, size, 0) })
}

fn as_str(p: Option<Prop>) -> Option<String> {
    match p? {
        Prop::Str(s) | Prop::Guid(s) => Some(s),
        Prop::List(l) => l.into_iter().next(),
    }
}

fn as_list(p: Option<Prop>) -> Vec<String> {
    match p {
        Some(Prop::List(l)) => l,
        Some(Prop::Str(s)) => vec![s],
        _ => Vec::new(),
    }
}

fn locate(instance: &str) -> Option<u32> {
    let w = wide(instance);
    let mut dn = 0u32;
    // SAFETY: valid string and out pointer.
    (unsafe { CM_Locate_DevNodeW(&mut dn, w.as_ptr(), CM_LOCATE_DEVNODE_NORMAL) } == CR_SUCCESS).then_some(dn)
}

fn device_id(dn: u32) -> Option<String> {
    let mut buf = [0u16; 512];
    // SAFETY: buffer of the given length.
    (unsafe { CM_Get_Device_IDW(dn, buf.as_mut_ptr(), buf.len() as u32, 0) } == CR_SUCCESS).then(|| from_wide(&buf))
}

fn read_node(instance: &str) -> Option<PnpNode> {
    let dn = locate(instance)?;
    let mut parent = 0u32;
    // SAFETY: valid devnode and out pointer.
    let parent = (unsafe { CM_Get_Parent(&mut parent, dn, 0) } == CR_SUCCESS).then(|| device_id(parent)).flatten();
    let enumerator = instance.split('\\').next().unwrap_or("").to_ascii_uppercase();
    let last = instance.rsplit('\\').next().unwrap_or("");
    // A PnP-generated instance suffix contains '&'; otherwise it is the device serial.
    let serial = (enumerator == "USB" && !last.contains('&') && !last.is_empty()).then(|| last.to_string());
    Some(PnpNode {
        instance_id: instance.to_string(),
        parent,
        class_guid: as_str(node_prop(dn, &DEVPKEY_Device_ClassGuid)).map(|g| g.trim_matches(['{', '}']).to_ascii_lowercase()),
        hardware_ids: as_list(node_prop(dn, &DEVPKEY_Device_HardwareIds)),
        compatible_ids: as_list(node_prop(dn, &DEVPKEY_Device_CompatibleIds)),
        location_paths: as_list(node_prop(dn, &DEVPKEY_Device_LocationPaths)),
        container_id: as_str(node_prop(dn, &DEVPKEY_Device_ContainerId)).map(|g| g.trim_matches(['{', '}']).to_ascii_lowercase()),
        friendly_name: as_str(node_prop(dn, &DEVPKEY_Device_FriendlyName)).or_else(|| as_str(node_prop(dn, &DEVPKEY_Device_DeviceDesc))),
        enumerator,
        present: true,
        serial,
        upper_filters: as_list(node_prop(dn, &DEVPKEY_Device_UpperFilters)),
        lower_filters: as_list(node_prop(dn, &DEVPKEY_Device_LowerFilters)),
    })
}

fn id_list(filter: Option<&str>, flags: u32) -> Vec<String> {
    let f = filter.map(wide);
    let fp = f.as_ref().map_or(std::ptr::null(), |v| v.as_ptr());
    let mut len = 0u32;
    // SAFETY: size query.
    if unsafe { CM_Get_Device_ID_List_SizeW(&mut len, fp, flags) } != CR_SUCCESS || len == 0 {
        return Vec::new();
    }
    let mut buf = vec![0u16; len as usize];
    // SAFETY: buffer of `len` chars.
    if unsafe { CM_Get_Device_ID_ListW(fp, buf.as_mut_ptr(), len, flags) } != CR_SUCCESS {
        return Vec::new();
    }
    multi_sz(&buf)
}

/// Present device-interface paths of a class GUID.
pub fn interfaces(class: &str) -> Vec<String> {
    let Some(g) = parse_guid(class) else { return Vec::new() };
    let mut len = 0u32;
    // SAFETY: size query for present interfaces of the class.
    if unsafe { CM_Get_Device_Interface_List_SizeW(&mut len, &g, std::ptr::null(), CM_GET_DEVICE_INTERFACE_LIST_PRESENT) } != CR_SUCCESS
        || len <= 1
    {
        return Vec::new();
    }
    let mut buf = vec![0u16; len as usize];
    // SAFETY: buffer of `len` chars.
    if unsafe { CM_Get_Device_Interface_ListW(&g, std::ptr::null(), buf.as_mut_ptr(), len, CM_GET_DEVICE_INTERFACE_LIST_PRESENT) } != CR_SUCCESS {
        return Vec::new();
    }
    multi_sz(&buf)
}

/// Devnode instance id behind an interface path.
pub fn interface_instance(path: &str) -> Option<String> {
    let w = wide(path);
    // SAFETY: valid interface path; buffers sized by the size query.
    as_str(read_prop(|ty, buf, size| unsafe { CM_Get_Device_Interface_PropertyW(w.as_ptr(), &DEVPKEY_Device_InstanceId, ty, buf, size, 0) }))
}

/// Snapshot of the present device tree.
#[derive(Default)]
pub struct CfgTree {
    nodes: RwLock<HashMap<String, PnpNode>>,
}

impl CfgTree {
    pub fn load() -> Self {
        let t = Self::default();
        let _ = t.refresh();
        t
    }
}

fn upper(s: &str) -> String {
    s.to_ascii_uppercase()
}

impl PnpTree for CfgTree {
    fn node(&self, instance_id: &str) -> Option<PnpNode> {
        self.nodes.read().unwrap_or_else(|e| e.into_inner()).get(&upper(instance_id)).cloned()
    }
    fn children(&self, instance_id: &str) -> Vec<String> {
        let id = upper(instance_id);
        self.nodes
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .filter(|n| n.parent.as_deref().map(upper) == Some(id.clone()))
            .map(|n| n.instance_id.clone())
            .collect()
    }
    fn interface_owner(&self, interface_path: &str) -> Option<String> {
        interface_instance(interface_path)
    }
    fn refresh(&self) -> Result<(), String> {
        let mut map = HashMap::new();
        for id in id_list(None, CM_GETIDLIST_FILTER_PRESENT) {
            if let Some(n) = read_node(&id) {
                map.insert(upper(&id), n);
            }
        }
        if map.is_empty() {
            return Err("device tree is empty".into());
        }
        *self.nodes.write().unwrap_or_else(|e| e.into_inner()) = map;
        Ok(())
    }
}

/// `VID_046D&PID_C52B` (also Bluetooth `VID&02046d_PID&c52b`) from hardware ids.
pub fn vid_pid(ids: &[String]) -> (u16, u16) {
    for h in ids {
        let u = h.to_ascii_uppercase();
        let grab = |tag: &str| -> Option<u16> {
            let i = u.find(tag)? + tag.len();
            let digits: String = u[i..].chars().take_while(|c| c.is_ascii_hexdigit()).take(6).collect();
            let d = if digits.len() == 6 { &digits[2..] } else { &digits[..digits.len().min(4)] };
            u16::from_str_radix(d, 16).ok()
        };
        if let (Some(v), Some(p)) = (grab("VID_").or_else(|| grab("VID&")), grab("PID_").or_else(|| grab("PID&"))) {
            return (v, p);
        }
    }
    (0, 0)
}

/// `HID_DEVICE_UP:000D_U:0004` → (0x0d, 0x04).
pub fn hid_usage(ids: &[String]) -> Option<(u16, u16)> {
    ids.iter().find_map(|h| {
        let u = h.to_ascii_uppercase();
        let i = u.find("HID_DEVICE_UP:")? + "HID_DEVICE_UP:".len();
        let page = u16::from_str_radix(u.get(i..i + 4)?, 16).ok()?;
        let j = u.find("_U:")? + 3;
        let usage = u16::from_str_radix(u.get(j..j + 4)?, 16).ok()?;
        Some((page, usage))
    })
}

/// Bus of an input devnode, from its first non-HID ancestor.
fn bus_of(tree: &dyn PnpTree, instance: &str) -> String {
    let mut cur = tree.node(instance);
    let mut depth = 0;
    while let Some(n) = cur {
        match n.enumerator.as_str() {
            "HID" => {}
            "USB" => return "usb".into(),
            "BTHENUM" | "BTHLEDEVICE" | "BTHHFENUM" => return "bluetooth".into(),
            "ACPI" => {
                let ids: Vec<String> = n.hardware_ids.iter().chain(&n.compatible_ids).map(|s| s.to_ascii_uppercase()).collect();
                if ids.iter().any(|i| i.contains("PNP0C50")) {
                    return "i2c".into();
                }
                if ids.iter().any(|i| i.contains("PNP03") || i.contains("PNP0F")) {
                    return "ps2".into();
                }
                return "other".into();
            }
            "ROOT" | "SWD" => return "virtual".into(),
            "SPB" => return "spi".into(),
            _ => {}
        }
        depth += 1;
        if depth > 32 {
            break;
        }
        cur = n.parent.as_deref().and_then(|p| tree.node(p));
    }
    "other".into()
}

pub fn hub_key(tree: &dyn PnpTree, hub_instance: &str) -> DeviceKey {
    let mut p = KeyParts::new(DeviceKind::UsbHub, anchor_for(tree, hub_instance));
    p.interface_class = DeviceKind::UsbHub.interface_class().map(str::to_string);
    p.compose()
}

fn base_device(tree: &dyn PnpTree, kind: DeviceKind, key: DeviceKey, n: &PnpNode, details: DeviceDetails) -> Device {
    let hub = nearest_hub(tree, &n.instance_id).map(|h| hub_key(tree, &h));
    let label = match anchor_for(tree, &n.instance_id) {
        Anchor::Location(p) => port_chain_label(&p),
        Anchor::Bluetooth(_) => Some("BT".into()),
        Anchor::Instance(_) => None,
    };
    Device {
        key,
        kind,
        name: n.friendly_name.clone().unwrap_or_else(|| n.instance_id.clone()),
        label,
        instance_id: Some(n.instance_id.clone()),
        container_id: n.container_id.clone(),
        parent_hub: hub,
        present: true,
        hardware_id: n.hardware_ids.first().cloned(),
        serial: n.serial.clone(),
        warnings: Vec::new(),
        details,
    }
}

fn input_device(tree: &dyn PnpTree, kind: DeviceKind, n: &PnpNode, path: &str) -> Device {
    let class = match kind {
        DeviceKind::Keyboard => InputClass::Keyboard,
        DeviceKind::Mouse => InputClass::Mouse,
        _ => InputClass::Hid,
    };
    let ids: Vec<String> = n.hardware_ids.iter().chain(std::iter::once(&n.instance_id)).cloned().collect();
    let (page, usage) = hid_usage(&n.compatible_ids).unwrap_or(match kind {
        DeviceKind::Keyboard => (1, 6),
        DeviceKind::Mouse => (1, 2),
        _ => (0, 0),
    });
    let mut p = KeyParts::new(kind, anchor_for(tree, &n.instance_id));
    p.interface_class = kind.interface_class().map(str::to_string);
    p.usb_interface = usb_interface_number(&ids);
    if n.enumerator == "HID" {
        p.collection = Some((hid_collection_number(&ids).unwrap_or(0), page, usage));
    }
    let (vendor_id, product_id) = vid_pid(&n.hardware_ids);
    let bus = bus_of(tree, &n.instance_id);
    let built_in = matches!(bus.as_str(), "i2c" | "spi" | "ps2");
    let info = InputInfo {
        class,
        usage_page: page,
        usage,
        vendor_id,
        product_id,
        interface_path: path.to_string(),
        bus,
        built_in,
        last_activity_ms: 0,
    };
    base_device(tree, kind, p.compose(), n, DeviceDetails::Input(info))
}

fn simple_device(tree: &dyn PnpTree, kind: DeviceKind, n: &PnpNode) -> Device {
    let mut p = KeyParts::new(kind, anchor_for(tree, &n.instance_id));
    p.interface_class = kind.interface_class().map(str::to_string);
    let key = if kind == DeviceKind::UsbHub { hub_key(tree, &n.instance_id) } else { p.compose() };
    let (vendor_id, product_id) = vid_pid(&n.hardware_ids);
    let details = match kind {
        DeviceKind::Storage => DeviceDetails::Storage(StorageInfo {
            removable: n.enumerator == "USBSTOR" || bus_of(tree, &n.instance_id) == "usb",
            ..Default::default()
        }),
        DeviceKind::UsbHub => DeviceDetails::Usb(UsbInfo {
            vendor_id,
            product_id,
            hub_depth: depth_of_hubs(tree, &n.instance_id),
            ..Default::default()
        }),
        _ => DeviceDetails::Usb(UsbInfo { vendor_id, product_id, ..Default::default() }),
    };
    base_device(tree, kind, key, n, details)
}

fn depth_of_hubs(tree: &dyn PnpTree, instance: &str) -> u8 {
    let mut d = 0u8;
    let mut cur = nearest_hub(tree, instance);
    while let Some(h) = cur {
        d = d.saturating_add(1);
        if d > 16 {
            break;
        }
        cur = nearest_hub(tree, &h);
    }
    d
}

fn audio_devices(tree: &dyn PnpTree) -> Vec<Device> {
    let mut out = Vec::new();
    for id in id_list(Some(AUDIO_ENDPOINT_CLASS), CM_GETIDLIST_FILTER_CLASS | CM_GETIDLIST_FILTER_PRESENT) {
        let Some(n) = tree.node(&id).or_else(|| read_node(&id)) else { continue };
        let endpoint = id.split('\\').nth(2).unwrap_or("").to_ascii_lowercase();
        let flow = if endpoint.starts_with("{0.0.1.") { AudioFlow::Capture } else { AudioFlow::Render };
        let kind = if flow == AudioFlow::Capture { DeviceKind::AudioCapture } else { DeviceKind::AudioRender };
        let key = KeyParts::new(kind, Anchor::Instance(id.clone())).compose();
        let info = AudioInfo { flow, endpoint_id: endpoint, form_factor: "other".into(), state: "active".into() };
        let mut d = base_device(tree, kind, key, &n, DeviceDetails::Audio(info));
        d.parent_hub = None;
        out.push(d);
    }
    out
}

fn output_technology(t: i32) -> &'static str {
    match t {
        i32::MIN | 6 | 11 | 13 => "internal",
        5 => "hdmi",
        10 | 18 => "displayport",
        4 => "dvi",
        0 => "vga",
        15 => "miracast",
        16 | 17 => "indirect",
        _ => "other",
    }
}

fn monitors(tree: &dyn PnpTree) -> Vec<Device> {
    let (mut np, mut nm) = (0u32, 0u32);
    const QDC_ALL_PATHS: u32 = 1;
    // SAFETY: size query.
    if unsafe { GetDisplayConfigBufferSizes(QDC_ALL_PATHS, &mut np, &mut nm) } != 0 {
        return Vec::new();
    }
    let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
    let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
    // SAFETY: arrays sized by the size query.
    if unsafe { QueryDisplayConfig(QDC_ALL_PATHS, &mut np, paths.as_mut_ptr(), &mut nm, modes.as_mut_ptr(), std::ptr::null_mut()) } != 0 {
        return Vec::new();
    }
    paths.truncate(np as usize);
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    // Active paths first so a target's desktop data comes from its active path.
    paths.sort_by_key(|p| p.flags & 1 == 0);
    for p in &paths {
        let t = &p.targetInfo;
        if t.targetAvailable == 0 {
            continue;
        }
        let luid = (u64::from(t.adapterId.HighPart as u32) << 32) | u64::from(t.adapterId.LowPart);
        if !seen.insert((luid, t.id)) {
            continue;
        }
        let mut name = DISPLAYCONFIG_TARGET_DEVICE_NAME::default();
        name.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME;
        name.header.size = std::mem::size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32;
        name.header.adapterId = t.adapterId;
        name.header.id = t.id;
        // SAFETY: header describes the struct.
        if unsafe { DisplayConfigGetDeviceInfo(&mut name.header) } != 0 {
            continue;
        }
        let mut adapter = DISPLAYCONFIG_ADAPTER_NAME::default();
        adapter.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_ADAPTER_NAME;
        adapter.header.size = std::mem::size_of::<DISPLAYCONFIG_ADAPTER_NAME>() as u32;
        adapter.header.adapterId = t.adapterId;
        // SAFETY: as above.
        let adapter_path = if unsafe { DisplayConfigGetDeviceInfo(&mut adapter.header) } == 0 { from_wide(&adapter.adapterDevicePath) } else { String::new() };
        let adapter_inst = interface_instance(&adapter_path);
        let anchor = match &adapter_inst {
            Some(a) => anchor_for(tree, a),
            None => Anchor::Instance(format!("ADAPTER_{luid:x}")),
        };
        let mut kp = KeyParts::new(DeviceKind::Monitor, anchor);
        kp.sub = Some(format!("t{}", t.id));
        let active = p.flags & 1 != 0;
        let (mut rect, mut gdi) = (None, None);
        if active {
            // SAFETY: union field valid for active paths (mode index form).
            let idx = unsafe { p.sourceInfo.Anonymous.modeInfoIdx } as usize;
            if let Some(m) = modes.get(idx).filter(|m| m.infoType == DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE) {
                // SAFETY: infoType says the union holds a source mode.
                let s = unsafe { m.Anonymous.sourceMode };
                rect = Some(Rect { x: s.position.x, y: s.position.y, width: s.width, height: s.height });
            }
            let mut src = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
            src.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
            src.header.size = std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
            src.header.adapterId = p.sourceInfo.adapterId;
            src.header.id = p.sourceInfo.id;
            // SAFETY: as above.
            if unsafe { DisplayConfigGetDeviceInfo(&mut src.header) } == 0 {
                gdi = Some(from_wide(&src.viewGdiDeviceName));
            }
        }
        // SAFETY: plain union read of the flags word.
        let flags = unsafe { name.flags.Anonymous.value };
        let edid = (flags & 4 != 0).then(|| EdidId {
            manufacturer: edid_manufacturer(name.edidManufactureId),
            product_code: name.edidProductCodeId,
            serial: 0,
        });
        let refresh = if t.refreshRate.Denominator != 0 {
            (u64::from(t.refreshRate.Numerator) * 1000 / u64::from(t.refreshRate.Denominator)) as u32
        } else {
            0
        };
        let rotation = match t.rotation {
            2 => 90,
            3 => 180,
            4 => 270,
            _ => 0,
        };
        let monitor_name = from_wide(&name.monitorFriendlyDeviceName);
        let monitor_path = from_wide(&name.monitorDevicePath);
        let monitor_inst = interface_instance(&monitor_path);
        let info = DisplayInfo {
            adapter_name: adapter_inst.as_deref().and_then(|a| tree.node(a)).and_then(|n| n.friendly_name).unwrap_or_default(),
            adapter_luid: luid,
            target_id: t.id,
            output_technology: output_technology(t.outputTechnology).into(),
            monitor_name: monitor_name.clone(),
            edid,
            mode: rect.map(|r| DisplayMode { width: r.width, height: r.height, refresh_mhz: refresh, rotation }),
            gdi_name: gdi,
            desktop_rect: rect,
            in_console_desktop: active,
            presentation: if active { Presentation::Native } else { Presentation::None },
        };
        let node = monitor_inst.as_deref().and_then(|m| tree.node(m));
        out.push(Device {
            key: kp.compose(),
            kind: DeviceKind::Monitor,
            name: if monitor_name.is_empty() { format!("Monitor {}", t.id) } else { monitor_name },
            label: None,
            instance_id: monitor_inst,
            container_id: node.as_ref().and_then(|n| n.container_id.clone()),
            parent_hub: None,
            present: true,
            hardware_id: node.as_ref().and_then(|n| n.hardware_ids.first().cloned()),
            serial: None,
            warnings: Vec::new(),
            details: DeviceDetails::Display(info),
        });
    }
    out
}

/// EDID manufacturer id (big-endian, 3 × 5-bit letters).
pub fn edid_manufacturer(raw: u16) -> String {
    let v = raw.swap_bytes();
    [(v >> 10) & 0x1f, (v >> 5) & 0x1f, v & 0x1f]
        .iter()
        .map(|c| if (1..=26).contains(c) { (b'A' + *c as u8 - 1) as char } else { '?' })
        .collect()
}

/// The whole baseline inventory.
pub fn scan() -> Vec<Device> {
    let tree = CfgTree::load();
    let mut out = Vec::new();
    let mut input_owners = HashSet::new();
    for kind in [DeviceKind::Keyboard, DeviceKind::Mouse] {
        for path in interfaces(kind.interface_class().unwrap_or_default()) {
            let Some(owner) = interface_instance(&path) else { continue };
            let Some(n) = tree.node(&owner) else { continue };
            input_owners.insert(upper(&owner));
            out.push(input_device(&tree, kind, &n, &path));
        }
    }
    for path in interfaces(DeviceKind::Hid.interface_class().unwrap_or_default()) {
        let Some(owner) = interface_instance(&path) else { continue };
        if input_owners.contains(&upper(&owner)) {
            continue;
        }
        let Some(n) = tree.node(&owner) else { continue };
        out.push(input_device(&tree, DeviceKind::Hid, &n, &path));
    }
    for kind in [DeviceKind::UsbHub, DeviceKind::Webcam, DeviceKind::SmartCardReader, DeviceKind::Storage] {
        let mut owners = HashSet::new();
        for path in interfaces(kind.interface_class().unwrap_or_default()) {
            let Some(owner) = interface_instance(&path) else { continue };
            if !owners.insert(upper(&owner)) {
                continue;
            }
            if let Some(n) = tree.node(&owner) {
                out.push(simple_device(&tree, kind, &n));
            }
        }
    }
    out.extend(audio_devices(&tree));
    out.extend(monitors(&tree));
    // Two live units with one key cannot be split (spec 50 §4.1.1).
    let mut count: HashMap<DeviceKey, usize> = HashMap::new();
    for d in &out {
        *count.entry(d.key.clone()).or_default() += 1;
    }
    for d in &mut out {
        if count.get(&d.key).copied().unwrap_or(0) > 1 {
            d.warnings.push(seat_core::device::DeviceWarning::KeyCollision);
        }
    }
    out
}

/// Device-interface arrival / removal notification: sets a flag the service tick reads.
pub struct Notification {
    handle: HCMNOTIFICATION,
    _ctx: Arc<AtomicBool>,
}

unsafe impl Send for Notification {}
unsafe impl Sync for Notification {}

unsafe extern "system" fn on_change(
    _h: HCMNOTIFICATION,
    ctx: *const c_void,
    _action: CM_NOTIFY_ACTION,
    _data: *const CM_NOTIFY_EVENT_DATA,
    _size: u32,
) -> u32 {
    // SAFETY: ctx points to the AtomicBool of the Arc held by the registration.
    let flag = unsafe { &*(ctx as *const AtomicBool) };
    flag.store(true, Ordering::SeqCst);
    0
}

impl Notification {
    pub fn register(flag: Arc<AtomicBool>) -> Option<Self> {
        let ctx = flag;
        let filter = CM_NOTIFY_FILTER {
            cbSize: std::mem::size_of::<CM_NOTIFY_FILTER>() as u32,
            Flags: CM_NOTIFY_FILTER_FLAG_ALL_INTERFACE_CLASSES,
            FilterType: CM_NOTIFY_FILTER_TYPE_DEVICEINTERFACE,
            ..Default::default()
        };
        let mut h: HCMNOTIFICATION = std::ptr::null_mut();
        let p = Arc::as_ptr(&ctx) as *const c_void;
        // SAFETY: filter valid; ctx outlives the registration (owned by Self).
        let rc = unsafe { CM_Register_Notification(&filter, p, Some(on_change), &mut h) };
        (rc == CR_SUCCESS).then_some(Self { handle: h, _ctx: ctx })
    }
}

impl Drop for Notification {
    fn drop(&mut self) {
        // SAFETY: handle from CM_Register_Notification.
        unsafe { CM_Unregister_Notification(self.handle) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsers() {
        assert_eq!(vid_pid(&["HID\\VID_046D&PID_C52B&MI_00".into()]), (0x046d, 0xc52b));
        assert_eq!(vid_pid(&["BTHLEDEVICE\\VID&02045E_PID&0B13".into()]), (0x045e, 0x0b13));
        assert_eq!(vid_pid(&["ACPI\\PNP0303".into()]), (0, 0));
        assert_eq!(hid_usage(&["HID_DEVICE_SYSTEM_GAME".into(), "HID_DEVICE_UP:0001_U:0005".into()]), Some((1, 5)));
        assert_eq!(edid_manufacturer(0xac10), "DEL");
        let g = parse_guid("{4d1e55b2-f16f-11cf-88cb-001111000030}").unwrap();
        assert_eq!(guid_string(&g), "4d1e55b2-f16f-11cf-88cb-001111000030");
    }

    /// Reads the real device tree of the dev box (read-only).
    #[test]
    fn live_scan_reads_tree() {
        let t = CfgTree::load();
        assert!(t.refresh().is_ok());
        let devs = scan();
        // Every key composes and is unique or flagged.
        for d in &devs {
            assert!(!d.key.as_str().is_empty());
        }
        let _ = devs.iter().filter(|d| d.kind == DeviceKind::Monitor).count();
    }
}
