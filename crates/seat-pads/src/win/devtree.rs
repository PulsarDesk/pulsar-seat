// SPDX-License-Identifier: GPL-3.0-only
//! Device tree through CfgMgr32 and SetupAPI (public Microsoft APIs), including
//! non-present ("phantom") nodes. Only nodes with our exact markers are listed.

use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_Child, CM_Get_Device_IDW, CM_Get_Device_ID_ListW, CM_Get_Device_ID_List_SizeW, CM_Get_Parent,
    CM_Get_Sibling, CM_Locate_DevNodeW, CM_Query_And_Remove_SubTreeW, CM_Set_DevNode_PropertyW,
    CM_Uninstall_DevNode, SetupDiCreateDeviceInfoList, SetupDiDestroyDeviceInfoList, SetupDiOpenDeviceInfoW,
    SetupDiGetDeviceRegistryPropertyW, SetupDiSetDeviceRegistryPropertyW, CM_GETIDLIST_FILTER_NONE, CM_LOCATE_DEVNODE_NORMAL, CM_LOCATE_DEVNODE_PHANTOM,
    CM_REMOVE_NO_RESTART, CR_BUFFER_SMALL, CR_SUCCESS, SPDRP_SECURITY_SDS, SP_DEVINFO_DATA,
};
use windows_sys::Win32::Devices::Properties::DEVPROP_TYPE_UINT32;
use windows_sys::Win32::Foundation::DEVPROPKEY;

use super::{from_wide, guid, last_error, split_multi_sz, wide};
use crate::error::PadError;
use crate::ids;
use crate::layout;
use crate::owners::{DeviceNode, DeviceTree};

pub struct CmDeviceTree;

fn cr(func: &'static str, r: u32) -> Result<(), PadError> {
    if r == CR_SUCCESS { Ok(()) } else { Err(PadError::os(func, r)) }
}

fn locate(id: &str, phantom: bool) -> Result<u32, PadError> {
    let w = wide(id);
    let mut dn = 0u32;
    let flags = if phantom { CM_LOCATE_DEVNODE_PHANTOM } else { CM_LOCATE_DEVNODE_NORMAL };
    // SAFETY: valid string and out pointer.
    cr("CM_Locate_DevNodeW", unsafe { CM_Locate_DevNodeW(&mut dn, w.as_ptr(), flags) })?;
    Ok(dn)
}

fn present(id: &str) -> bool {
    locate(id, false).is_ok()
}

fn device_id(dn: u32) -> Result<String, PadError> {
    let mut buf = vec![0u16; 512];
    // SAFETY: buffer length in characters.
    cr("CM_Get_Device_IDW", unsafe { CM_Get_Device_IDW(dn, buf.as_mut_ptr(), buf.len() as u32, 0) })?;
    Ok(from_wide(&buf))
}

fn parent_of(dn: u32) -> Option<String> {
    let mut p = 0u32;
    // SAFETY: out pointer valid.
    if unsafe { CM_Get_Parent(&mut p, dn, 0) } != CR_SUCCESS {
        return None;
    }
    device_id(p).ok()
}

fn session_key() -> DEVPROPKEY {
    DEVPROPKEY { fmtid: guid(layout::DEVPKEY_SESSION_ID_GUID), pid: layout::DEVPKEY_SESSION_ID_PID }
}

impl DeviceTree for CmDeviceTree {
    fn devices_with_marker(&self) -> Result<Vec<DeviceNode>, PadError> {
        let mut out = Vec::new();
        for _ in 0..4 {
            let mut len = 0u32;
            // SAFETY: out pointer valid; null filter = all devices (present or not).
            cr("CM_Get_Device_ID_List_SizeW", unsafe {
                CM_Get_Device_ID_List_SizeW(&mut len, std::ptr::null(), CM_GETIDLIST_FILTER_NONE)
            })?;
            let mut buf = vec![0u16; len as usize + 2];
            // SAFETY: buffer of `len` characters.
            let r = unsafe {
                CM_Get_Device_ID_ListW(std::ptr::null(), buf.as_mut_ptr(), buf.len() as u32, CM_GETIDLIST_FILTER_NONE)
            };
            if r == CR_BUFFER_SMALL {
                continue; // devices arrived between the two calls
            }
            cr("CM_Get_Device_ID_ListW", r)?;
            for id in split_multi_sz(&buf) {
                if ids::marker_of(&id).is_none() {
                    continue;
                }
                let parent = locate(&id, true).ok().and_then(parent_of);
                out.push(DeviceNode { present: present(&id), instance_id: id, parent });
            }
            return Ok(out);
        }
        Err(PadError::os("CM_Get_Device_ID_ListW", CR_BUFFER_SMALL))
    }

    fn children(&self, instance_id: &str) -> Result<Vec<DeviceNode>, PadError> {
        let dn = locate(instance_id, true)?;
        let mut out = Vec::new();
        let mut c = 0u32;
        // SAFETY: out pointer valid.
        if unsafe { CM_Get_Child(&mut c, dn, 0) } != CR_SUCCESS {
            return Ok(out);
        }
        loop {
            if let Ok(id) = device_id(c) {
                out.push(DeviceNode { present: present(&id), instance_id: id, parent: Some(instance_id.to_string()) });
            }
            let mut n = 0u32;
            // SAFETY: out pointer valid.
            if unsafe { CM_Get_Sibling(&mut n, c, 0) } != CR_SUCCESS {
                break;
            }
            c = n;
        }
        Ok(out)
    }

    fn remove(&self, instance_id: &str) -> Result<(), PadError> {
        if ids::marker_of(instance_id).is_none() && !instance_id.to_ascii_uppercase().starts_with("SWD\\PULSARSPLITPAD\\") {
            // Safety net: this tree never removes nodes that are not ours.
            return Err(PadError::MarkerConflict(format!("refusing to remove unmarked device {instance_id}")));
        }
        if let Ok(dn) = locate(instance_id, false) {
            let mut veto = 0i32;
            let mut name = vec![0u16; 260];
            // SAFETY: valid devnode and buffers.
            let r = unsafe {
                CM_Query_And_Remove_SubTreeW(dn, &mut veto, name.as_mut_ptr(), name.len() as u32, CM_REMOVE_NO_RESTART)
            };
            if r != CR_SUCCESS {
                log::info!("remove {instance_id}: vetoed ({veto}) by {}", from_wide(&name));
            }
        }
        let dn = locate(instance_id, true)?;
        // SAFETY: valid devnode.
        cr("CM_Uninstall_DevNode", unsafe { CM_Uninstall_DevNode(dn, 0) })
    }

    fn set_security(&self, instance_id: &str, sddl: &str) -> Result<(), PadError> {
        // SAFETY: SetupAPI calls with valid buffers; the set is destroyed on every path.
        unsafe {
            let set = SetupDiCreateDeviceInfoList(std::ptr::null(), std::ptr::null_mut());
            if set == -1 {
                return Err(PadError::os("SetupDiCreateDeviceInfoList", last_error()));
            }
            let id = wide(instance_id);
            let mut data = SP_DEVINFO_DATA { cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32, ..Default::default() };
            let r = if SetupDiOpenDeviceInfoW(set, id.as_ptr(), std::ptr::null_mut(), 0, &mut data) == 0 {
                Err(PadError::os("SetupDiOpenDeviceInfoW", last_error()))
            } else {
                let s = wide(sddl);
                if SetupDiSetDeviceRegistryPropertyW(
                    set,
                    &mut data,
                    SPDRP_SECURITY_SDS,
                    s.as_ptr() as *const u8,
                    (s.len() * 2) as u32,
                ) == 0
                {
                    Err(PadError::os("SetupDiSetDeviceRegistryPropertyW", last_error()))
                } else {
                    Ok(())
                }
            };
            SetupDiDestroyDeviceInfoList(set);
            r
        }
    }

    fn set_session_id(&self, instance_id: &str, session_id: u32) -> Result<(), PadError> {
        let dn = locate(instance_id, false)?;
        let key = session_key();
        let v = session_id.to_le_bytes();
        // SAFETY: valid devnode, key and 4-byte buffer.
        cr("CM_Set_DevNode_PropertyW", unsafe {
            CM_Set_DevNode_PropertyW(dn, &key, DEVPROP_TYPE_UINT32, v.as_ptr(), 4, 0)
        })
    }
}

// ---- physical pad nodes (seat_input::pad_scope, 2026-11-17) ----
//
// The functions below act on any device node (physical pads included), so they are not
// part of [`DeviceTree`] (whose `remove` only ever touches our own markers). The service
// uses them only for the HID collection and parent of a pad that `pad_scope` journaled.

/// Instance id of a node's parent (present nodes only).
pub fn parent_instance(instance_id: &str) -> Option<String> {
    locate(instance_id, false).ok().and_then(parent_of)
}

/// Opens one node in a fresh SetupAPI set and runs `f`; the set is destroyed afterwards.
fn with_devinfo<T>(instance_id: &str, f: impl FnOnce(isize, &mut SP_DEVINFO_DATA) -> Result<T, PadError>) -> Result<T, PadError> {
    // SAFETY: SetupAPI calls with valid buffers; the set is destroyed on every path.
    unsafe {
        let set = SetupDiCreateDeviceInfoList(std::ptr::null(), std::ptr::null_mut());
        if set == -1 {
            return Err(PadError::os("SetupDiCreateDeviceInfoList", last_error()));
        }
        let id = wide(instance_id);
        let mut data = SP_DEVINFO_DATA { cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32, ..Default::default() };
        let r = if SetupDiOpenDeviceInfoW(set, id.as_ptr(), std::ptr::null_mut(), 0, &mut data) == 0 {
            Err(PadError::os("SetupDiOpenDeviceInfoW", last_error()))
        } else {
            f(set, &mut data)
        };
        SetupDiDestroyDeviceInfoList(set);
        r
    }
}

/// The node's own security descriptor (`SPDRP_SECURITY_SDS`), `None` when it has none.
pub fn security_sds(instance_id: &str) -> Result<Option<String>, PadError> {
    with_devinfo(instance_id, |set, data| {
        let mut buf = vec![0u16; 2048];
        let mut ty = 0u32;
        let mut need = 0u32;
        // SAFETY: buffer of the given byte size.
        let ok = unsafe {
            SetupDiGetDeviceRegistryPropertyW(set, data, SPDRP_SECURITY_SDS, &mut ty, buf.as_mut_ptr().cast(), (buf.len() * 2) as u32, &mut need)
        };
        if ok == 0 {
            let e = last_error();
            // ERROR_INVALID_DATA (13): the node has no own descriptor.
            return if e == 13 { Ok(None) } else { Err(PadError::os("SetupDiGetDeviceRegistryPropertyW", e)) };
        }
        Ok(Some(from_wide(&buf)).filter(|s| !s.is_empty()))
    })
}

/// Sets (`Some`) or deletes (`None`) the node's own security descriptor.
pub fn set_security_sds(instance_id: &str, sddl: Option<&str>) -> Result<(), PadError> {
    with_devinfo(instance_id, |set, data| {
        let s = sddl.map(wide);
        let (ptr, len) = match &s {
            Some(w) => (w.as_ptr() as *const u8, (w.len() * 2) as u32),
            // Documented: a null buffer with size 0 deletes the property.
            None => (std::ptr::null(), 0),
        };
        // SAFETY: valid set / data; buffer of `len` bytes or null.
        if unsafe { SetupDiSetDeviceRegistryPropertyW(set, data, SPDRP_SECURITY_SDS, ptr, len) } == 0 {
            return Err(PadError::os("SetupDiSetDeviceRegistryPropertyW", last_error()));
        }
        Ok(())
    })
}

/// `DEVPKEY_Device_SessionId` of a node, `None` when unset.
pub fn session_id_of(instance_id: &str) -> Result<Option<u32>, PadError> {
    use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{CM_Get_DevNode_PropertyW, CR_NO_SUCH_VALUE};
    let dn = locate(instance_id, false)?;
    let key = session_key();
    let mut ty = 0u32;
    let mut v = [0u8; 4];
    let mut size = 4u32;
    // SAFETY: valid devnode, key and 4-byte buffer.
    let r = unsafe { CM_Get_DevNode_PropertyW(dn, &key, &mut ty, v.as_mut_ptr(), &mut size, 0) };
    if r == CR_NO_SUCH_VALUE {
        return Ok(None);
    }
    cr("CM_Get_DevNode_PropertyW", r)?;
    Ok((ty == DEVPROP_TYPE_UINT32 && size == 4).then(|| u32::from_le_bytes(v)))
}

/// Sets (`Some`) or deletes (`None`) `DEVPKEY_Device_SessionId` of a node.
pub fn set_session_id_of(instance_id: &str, session_id: Option<u32>) -> Result<(), PadError> {
    use windows_sys::Win32::Devices::Properties::DEVPROP_TYPE_EMPTY;
    let dn = locate(instance_id, false)?;
    let key = session_key();
    let r = match session_id {
        Some(s) => {
            let v = s.to_le_bytes();
            // SAFETY: valid devnode, key and 4-byte buffer.
            unsafe { CM_Set_DevNode_PropertyW(dn, &key, DEVPROP_TYPE_UINT32, v.as_ptr(), 4, 0) }
        }
        // SAFETY: documented delete: type EMPTY, no buffer.
        None => unsafe { CM_Set_DevNode_PropertyW(dn, &key, DEVPROP_TYPE_EMPTY, std::ptr::null(), 0, 0) },
    };
    cr("CM_Set_DevNode_PropertyW", r)
}

/// Restarts a node so changed properties (its security descriptor) apply: the documented
/// `DIF_PROPERTYCHANGE` / `DICS_PROPCHANGE` class-installer call (what Device Manager does
/// after a property change). HYPOTHESIS: never run on a physical pad.
pub fn property_change_restart(instance_id: &str) -> Result<(), PadError> {
    use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
        DICS_FLAG_GLOBAL, DICS_PROPCHANGE, DIF_PROPERTYCHANGE, SP_CLASSINSTALL_HEADER, SP_PROPCHANGE_PARAMS,
        SetupDiCallClassInstaller, SetupDiSetClassInstallParamsW,
    };
    with_devinfo(instance_id, |set, data| {
        let params = SP_PROPCHANGE_PARAMS {
            ClassInstallHeader: SP_CLASSINSTALL_HEADER {
                cbSize: std::mem::size_of::<SP_CLASSINSTALL_HEADER>() as u32,
                InstallFunction: DIF_PROPERTYCHANGE,
            },
            StateChange: DICS_PROPCHANGE,
            Scope: DICS_FLAG_GLOBAL,
            HwProfile: 0,
        };
        // SAFETY: valid set / data; params of the documented size.
        unsafe {
            if SetupDiSetClassInstallParamsW(set, data, &params.ClassInstallHeader, std::mem::size_of::<SP_PROPCHANGE_PARAMS>() as u32) == 0 {
                return Err(PadError::os("SetupDiSetClassInstallParamsW", last_error()));
            }
            if SetupDiCallClassInstaller(DIF_PROPERTYCHANGE, set, data) == 0 {
                return Err(PadError::os("SetupDiCallClassInstaller", last_error()));
            }
        }
        Ok(())
    })
}

/// Counts device-interface arrivals of every class (HID, XUSB, ...) through
/// `CM_Register_Notification`, so the pad manager secures new pad children at once
/// instead of waiting for its next periodic pass (`PadManager::set_arrival_counter`).
/// The callback only bumps a counter; all securing happens on the pad loop.
pub struct DeviceArrivalWatch {
    handle: windows_sys::Win32::Devices::DeviceAndDriverInstallation::HCMNOTIFICATION,
    counter: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// Raw `Arc` handed to the callback as its context; reclaimed after unregister.
    ctx: *const std::sync::atomic::AtomicU64,
}

// SAFETY: the notification handle may be unregistered from any thread.
unsafe impl Send for DeviceArrivalWatch {}

unsafe extern "system" fn on_arrival(
    _h: windows_sys::Win32::Devices::DeviceAndDriverInstallation::HCMNOTIFICATION,
    context: *const core::ffi::c_void,
    action: windows_sys::Win32::Devices::DeviceAndDriverInstallation::CM_NOTIFY_ACTION,
    _data: *const windows_sys::Win32::Devices::DeviceAndDriverInstallation::CM_NOTIFY_EVENT_DATA,
    _size: u32,
) -> u32 {
    use windows_sys::Win32::Devices::DeviceAndDriverInstallation::CM_NOTIFY_ACTION_DEVICEINTERFACEARRIVAL;
    if action == CM_NOTIFY_ACTION_DEVICEINTERFACEARRIVAL && !context.is_null() {
        // SAFETY: context is the AtomicU64 kept alive until after unregistration.
        unsafe { (*(context as *const std::sync::atomic::AtomicU64)).fetch_add(1, std::sync::atomic::Ordering::SeqCst) };
    }
    0
}

impl DeviceArrivalWatch {
    pub fn start() -> Result<Self, PadError> {
        use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
            CM_Register_Notification, CM_NOTIFY_FILTER, CM_NOTIFY_FILTER_FLAG_ALL_INTERFACE_CLASSES,
            CM_NOTIFY_FILTER_TYPE_DEVICEINTERFACE,
        };
        let counter = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let ctx = std::sync::Arc::into_raw(counter.clone());
        let filter = CM_NOTIFY_FILTER {
            cbSize: std::mem::size_of::<CM_NOTIFY_FILTER>() as u32,
            Flags: CM_NOTIFY_FILTER_FLAG_ALL_INTERFACE_CLASSES,
            FilterType: CM_NOTIFY_FILTER_TYPE_DEVICEINTERFACE,
            ..Default::default()
        };
        let mut handle = std::ptr::null_mut();
        // SAFETY: valid filter, callback and out pointer; ctx stays alive until Drop.
        let r = unsafe { CM_Register_Notification(&filter, ctx.cast(), Some(on_arrival), &mut handle) };
        if r != CR_SUCCESS {
            // SAFETY: reclaim the Arc we leaked above; the callback was never registered.
            drop(unsafe { std::sync::Arc::from_raw(ctx) });
            return Err(PadError::os("CM_Register_Notification", r));
        }
        Ok(DeviceArrivalWatch { handle, counter, ctx })
    }

    pub fn counter(&self) -> std::sync::Arc<std::sync::atomic::AtomicU64> {
        self.counter.clone()
    }
}

impl Drop for DeviceArrivalWatch {
    fn drop(&mut self) {
        // Waits for callbacks in progress (documented), so ctx can be freed after it.
        // SAFETY: handle from CM_Register_Notification, unregistered once.
        unsafe { windows_sys::Win32::Devices::DeviceAndDriverInstallation::CM_Unregister_Notification(self.handle) };
        // SAFETY: reclaims the Arc leaked in start.
        drop(unsafe { std::sync::Arc::from_raw(self.ctx) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read-only: lists marker devices (normally none) and refuses unmarked removals.
    #[test]
    fn read_only_queries() {
        let t = CmDeviceTree;
        let devs = t.devices_with_marker().unwrap();
        assert!(devs.iter().all(|d| ids::marker_of(&d.instance_id).is_some()));
        assert!(matches!(t.remove("ROOT\\SYSTEM\\0000"), Err(PadError::MarkerConflict(_))));
    }

    /// Reads of a device node that does not exist fail cleanly (nothing is created).
    #[test]
    fn missing_node_reads_fail() {
        let id = r"ROOT\PULSAR_NO_SUCH_NODE\0000";
        assert!(parent_instance(id).is_none());
        assert!(session_id_of(id).is_err());
        assert!(security_sds(id).is_err());
    }

    /// Registers and unregisters a notification (no system state changes).
    #[test]
    fn arrival_watch_registers() {
        let w = DeviceArrivalWatch::start().unwrap();
        let _ = w.counter().load(std::sync::atomic::Ordering::SeqCst);
        drop(w);
    }
}
