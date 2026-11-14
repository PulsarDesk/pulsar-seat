// SPDX-License-Identifier: GPL-3.0-only
//! [`WinPnpTree`]: the CfgMgr32 device tree behind [`seat_core::pnp::PnpTree`].
//!
//! The CfgMgr32 list / locate / property primitives are the shared ones in
//! `seat_audio::cfgmgr` (one copy for both seat kinds, merge step 8).
//!
//! The whole tree (present and phantom devnodes) is read at `refresh` and cached; the
//! service calls `refresh` after hot-plug notifications. Read-only; never changes anything.

use seat_audio::cfgmgr::{self, as_guid, as_list, as_string, devnode_prop, interface_prop, locate};
use seat_core::pnp::{PnpNode, PnpTree};
use std::collections::HashMap;
use std::sync::RwLock;
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{CM_GETIDLIST_FILTER_NONE, CM_GETIDLIST_FILTER_REMOVALRELATIONS};
use windows_sys::Win32::Devices::Properties::{
    DEVPKEY_Device_ClassGuid, DEVPKEY_Device_CompatibleIds, DEVPKEY_Device_ContainerId, DEVPKEY_Device_DeviceDesc,
    DEVPKEY_Device_EnumeratorName, DEVPKEY_Device_FriendlyName, DEVPKEY_Device_HardwareIds, DEVPKEY_Device_InstanceId,
    DEVPKEY_Device_LocationPaths, DEVPKEY_Device_LowerFilters, DEVPKEY_Device_Parent, DEVPKEY_Device_UpperFilters,
};

/// Device id list (`CM_Get_Device_ID_ListW`); `filter` = `None` for every devnode.
pub(crate) fn id_list(filter: Option<&str>, flags: u32) -> Result<Vec<String>, String> {
    cfgmgr::id_list(filter, flags).map_err(|e| e.to_string())
}

/// Devnodes in the removal relations of `instance_id` (a disk's volumes).
pub fn removal_relations(instance_id: &str) -> Result<Vec<String>, String> {
    id_list(Some(instance_id), CM_GETIDLIST_FILTER_REMOVALRELATIONS)
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
