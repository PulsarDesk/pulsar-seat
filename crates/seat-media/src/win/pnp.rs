// SPDX-License-Identifier: GPL-3.0-only
//! [`WinPnpTree`]: the CfgMgr32 device tree behind [`seat_core::pnp::PnpTree`].
//!
//! The CfgMgr32 list / locate / property helpers are adapted from pulsar-split
//! (`crates/seat-audio/src/win.rs`, GPL-3, same author).
//!
//! The whole tree (present and phantom devnodes) is read at `refresh` and cached; the
//! service calls `refresh` after hot-plug notifications. Read-only; never changes anything.

use super::{split_multi_sz, wide};
use seat_core::pnp::{PnpNode, PnpTree};
use std::collections::HashMap;
use std::sync::RwLock;
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_GETIDLIST_FILTER_NONE, CM_GETIDLIST_FILTER_REMOVALRELATIONS, CM_Get_DevNode_PropertyW, CM_Get_Device_ID_ListW,
    CM_Get_Device_ID_List_SizeW, CM_Get_Device_Interface_PropertyW, CM_LOCATE_DEVNODE_NORMAL, CM_LOCATE_DEVNODE_PHANTOM,
    CM_Locate_DevNodeW, CONFIGRET, CR_BUFFER_SMALL, CR_SUCCESS,
};
use windows_sys::Win32::Devices::Properties::{
    DEVPKEY_Device_ClassGuid, DEVPKEY_Device_CompatibleIds, DEVPKEY_Device_ContainerId, DEVPKEY_Device_DeviceDesc,
    DEVPKEY_Device_EnumeratorName, DEVPKEY_Device_FriendlyName, DEVPKEY_Device_HardwareIds, DEVPKEY_Device_InstanceId,
    DEVPKEY_Device_LocationPaths, DEVPKEY_Device_LowerFilters, DEVPKEY_Device_Parent, DEVPKEY_Device_UpperFilters,
    DEVPROP_TYPE_GUID, DEVPROP_TYPE_STRING, DEVPROP_TYPE_STRING_LIST, DEVPROPTYPE,
};
use windows_sys::Win32::Foundation::DEVPROPKEY;
use windows_sys::core::GUID;

/// Device id list (`CM_Get_Device_ID_ListW`); `filter` = `None` for every devnode.
pub(crate) fn id_list(filter: Option<&str>, flags: u32) -> Result<Vec<String>, String> {
    let f = filter.map(wide);
    let fp = f.as_ref().map_or(std::ptr::null(), |w| w.as_ptr());
    // The list can grow between the two calls; retry on CR_BUFFER_SMALL.
    for _ in 0..4 {
        let mut len: u32 = 0;
        // SAFETY: `fp` is null or a NUL-terminated UTF-16 string; `len` is a valid out ptr.
        let cr = unsafe { CM_Get_Device_ID_List_SizeW(&mut len, fp, flags) };
        if cr != CR_SUCCESS {
            return Err(format!("CM_Get_Device_ID_List_SizeW: CONFIGRET {cr}"));
        }
        let mut buf = vec![0u16; len.max(2) as usize];
        // SAFETY: `buf` holds `buf.len()` u16 elements.
        let cr = unsafe { CM_Get_Device_ID_ListW(fp, buf.as_mut_ptr(), buf.len() as u32, flags) };
        if cr == CR_BUFFER_SMALL {
            continue;
        }
        if cr != CR_SUCCESS {
            return Err(format!("CM_Get_Device_ID_ListW: CONFIGRET {cr}"));
        }
        return Ok(split_multi_sz(&buf));
    }
    Err("CM_Get_Device_ID_ListW: list kept growing".into())
}

/// Devnodes in the removal relations of `instance_id` (a disk's volumes).
pub fn removal_relations(instance_id: &str) -> Result<Vec<String>, String> {
    id_list(Some(instance_id), CM_GETIDLIST_FILTER_REMOVALRELATIONS)
}

/// (devinst, present).
pub(crate) fn locate(instance_id: &str) -> Option<(u32, bool)> {
    let w = wide(instance_id);
    let mut dn: u32 = 0;
    // SAFETY: valid NUL-terminated id and out pointer.
    if unsafe { CM_Locate_DevNodeW(&mut dn, w.as_ptr(), CM_LOCATE_DEVNODE_NORMAL) } == CR_SUCCESS {
        return Some((dn, true));
    }
    // SAFETY: as above.
    if unsafe { CM_Locate_DevNodeW(&mut dn, w.as_ptr(), CM_LOCATE_DEVNODE_PHANTOM) } == CR_SUCCESS {
        return Some((dn, false));
    }
    None
}

type PropReader<'a> = dyn FnMut(&mut DEVPROPTYPE, *mut u8, &mut u32) -> CONFIGRET + 'a;

fn read_prop(f: &mut PropReader<'_>) -> Option<(DEVPROPTYPE, Vec<u8>)> {
    for _ in 0..3 {
        let mut ty: DEVPROPTYPE = 0;
        let mut size: u32 = 0;
        let cr = f(&mut ty, std::ptr::null_mut(), &mut size);
        if (cr != CR_BUFFER_SMALL && cr != CR_SUCCESS) || size == 0 {
            return None;
        }
        let mut buf = vec![0u8; size as usize];
        let cr = f(&mut ty, buf.as_mut_ptr(), &mut size);
        if cr == CR_BUFFER_SMALL {
            continue;
        }
        if cr != CR_SUCCESS {
            return None;
        }
        buf.truncate(size as usize);
        return Some((ty, buf));
    }
    None
}

pub(crate) fn devnode_prop(dn: u32, key: &DEVPROPKEY) -> Option<(DEVPROPTYPE, Vec<u8>)> {
    // SAFETY: the closure passes a valid key, out pointers and a buffer of `size` bytes (or
    // null with size 0 for the size query, the documented pattern).
    read_prop(&mut |ty, buf, size| unsafe { CM_Get_DevNode_PropertyW(dn, key, ty, buf, size, 0) })
}

pub(crate) fn interface_prop(path: &str, key: &DEVPROPKEY) -> Option<(DEVPROPTYPE, Vec<u8>)> {
    let w = wide(path);
    // SAFETY: as in `devnode_prop`; `w` is NUL-terminated.
    read_prop(&mut |ty, buf, size| unsafe { CM_Get_Device_Interface_PropertyW(w.as_ptr(), key, ty, buf, size, 0) })
}

fn utf16(b: &[u8]) -> Vec<u16> {
    b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect()
}

pub(crate) fn as_string(p: Option<(DEVPROPTYPE, Vec<u8>)>) -> Option<String> {
    match p? {
        (DEVPROP_TYPE_STRING, b) => {
            let s = super::from_wide(&utf16(&b));
            (!s.is_empty()).then_some(s)
        }
        _ => None,
    }
}

fn as_list(p: Option<(DEVPROPTYPE, Vec<u8>)>) -> Vec<String> {
    match p {
        Some((DEVPROP_TYPE_STRING_LIST, b)) => split_multi_sz(&utf16(&b)),
        Some((DEVPROP_TYPE_STRING, b)) => vec![super::from_wide(&utf16(&b))],
        _ => Vec::new(),
    }
}

pub(crate) fn guid_string(g: &GUID) -> String {
    format!(
        "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        g.data1, g.data2, g.data3, g.data4[0], g.data4[1], g.data4[2], g.data4[3], g.data4[4], g.data4[5], g.data4[6], g.data4[7]
    )
}

fn as_guid(p: Option<(DEVPROPTYPE, Vec<u8>)>) -> Option<String> {
    match p? {
        (DEVPROP_TYPE_GUID, b) if b.len() == 16 => {
            let g = GUID {
                data1: u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
                data2: u16::from_le_bytes([b[4], b[5]]),
                data3: u16::from_le_bytes([b[6], b[7]]),
                data4: [b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]],
            };
            Some(guid_string(&g))
        }
        _ => None,
    }
}

/// USB serial heuristic: Windows uses the device's serial number as the instance-specific
/// part of a USB device id when the serial is unique; generated ids contain `&`.
fn usb_serial(instance_id: &str) -> Option<String> {
    let mut parts = instance_id.split('\\');
    let (e, hw, inst) = (parts.next()?, parts.next()?, parts.next()?);
    if !e.eq_ignore_ascii_case("USB") || hw.to_ascii_uppercase().contains("&MI_") || inst.contains('&') || inst.is_empty() {
        return None;
    }
    Some(inst.to_string())
}

/// Read one devnode.
pub(crate) fn read_node(instance_id: &str) -> Option<PnpNode> {
    let (dn, present) = locate(instance_id)?;
    let enumerator = as_string(devnode_prop(dn, &DEVPKEY_Device_EnumeratorName))
        .unwrap_or_else(|| instance_id.split('\\').next().unwrap_or("").to_string())
        .to_ascii_uppercase();
    Some(PnpNode {
        instance_id: instance_id.to_string(),
        parent: as_string(devnode_prop(dn, &DEVPKEY_Device_Parent)),
        class_guid: as_guid(devnode_prop(dn, &DEVPKEY_Device_ClassGuid)),
        hardware_ids: as_list(devnode_prop(dn, &DEVPKEY_Device_HardwareIds)),
        compatible_ids: as_list(devnode_prop(dn, &DEVPKEY_Device_CompatibleIds)),
        location_paths: as_list(devnode_prop(dn, &DEVPKEY_Device_LocationPaths)),
        container_id: as_guid(devnode_prop(dn, &DEVPKEY_Device_ContainerId)),
        friendly_name: as_string(devnode_prop(dn, &DEVPKEY_Device_FriendlyName))
            .or_else(|| as_string(devnode_prop(dn, &DEVPKEY_Device_DeviceDesc))),
        serial: if enumerator == "USB" { usb_serial(instance_id) } else { None },
        enumerator,
        present,
        upper_filters: as_list(devnode_prop(dn, &DEVPKEY_Device_UpperFilters)),
        lower_filters: as_list(devnode_prop(dn, &DEVPKEY_Device_LowerFilters)),
    })
}

#[derive(Default)]
struct Cache {
    nodes: HashMap<String, PnpNode>,
    children: HashMap<String, Vec<String>>,
    loaded: bool,
}

/// CfgMgr32-backed device tree. `Send + Sync`; cheap reads from a cache.
#[derive(Default)]
pub struct WinPnpTree {
    cache: RwLock<Cache>,
}

impl WinPnpTree {
    pub fn new() -> Self {
        Self::default()
    }

    fn ensure(&self) {
        let loaded = self.cache.read().map(|c| c.loaded).unwrap_or(false);
        if !loaded {
            if let Err(e) = self.refresh() {
                log::warn!("pnp: initial tree read failed: {e}");
            }
        }
    }

    /// Every devnode id in the cache.
    pub fn all_ids(&self) -> Vec<String> {
        self.ensure();
        self.cache.read().map(|c| c.nodes.values().map(|n| n.instance_id.clone()).collect()).unwrap_or_default()
    }
}

impl PnpTree for WinPnpTree {
    fn node(&self, instance_id: &str) -> Option<PnpNode> {
        self.ensure();
        let hit = self.cache.read().ok()?.nodes.get(&instance_id.to_ascii_uppercase()).cloned();
        // Not cached (arrived since the last refresh): read it directly.
        hit.or_else(|| read_node(instance_id))
    }

    fn children(&self, instance_id: &str) -> Vec<String> {
        self.ensure();
        self.cache.read().ok().and_then(|c| c.children.get(&instance_id.to_ascii_uppercase()).cloned()).unwrap_or_default()
    }

    fn interface_owner(&self, interface_path: &str) -> Option<String> {
        as_string(interface_prop(interface_path, &DEVPKEY_Device_InstanceId))
    }

    fn refresh(&self) -> Result<(), String> {
        let ids = id_list(None, CM_GETIDLIST_FILTER_NONE)?;
        let mut c = Cache { loaded: true, ..Default::default() };
        for id in ids {
            let Some(n) = read_node(&id) else { continue };
            if let Some(p) = &n.parent {
                c.children.entry(p.to_ascii_uppercase()).or_default().push(n.instance_id.clone());
            }
            c.nodes.insert(id.to_ascii_uppercase(), n);
        }
        *self.cache.write().map_err(|_| "pnp cache poisoned".to_string())? = c;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serial_heuristic() {
        assert_eq!(usb_serial(r"USB\VID_0781&PID_5581\4C530001").as_deref(), Some("4C530001"));
        assert_eq!(usb_serial(r"USB\VID_0781&PID_5581\5&1A2B&0&2"), None);
        assert_eq!(usb_serial(r"USB\VID_1&PID_2&MI_00\7"), None);
    }

    /// Read-only smoke test of the live tree. Changes nothing.
    #[test]
    #[ignore = "reads the live device tree; run manually with --ignored"]
    fn live_tree() {
        let t = WinPnpTree::new();
        t.refresh().unwrap();
        assert!(t.node(crate::inventory::ROOT_DEVNODE).is_some());
        for g in crate::inventory::scan(&t) {
            println!("{:?} {} {}", g.device.kind, g.device.key, g.instance_id);
        }
    }
}
