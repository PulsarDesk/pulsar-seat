// SPDX-License-Identifier: GPL-3.0-only
//! Device-node queries through SetupAPI (read-only): which display devices exist in which
//! session, and the interface paths of the own driver per session.

use windows::Win32::Devices::DeviceAndDriverInstallation::{
    DIGCF_DEVICEINTERFACE, DIGCF_PRESENT, GUID_DEVCLASS_DISPLAY, HDEVINFO,
    SP_DEVICE_INTERFACE_DATA, SP_DEVICE_INTERFACE_DETAIL_DATA_W, SP_DEVINFO_DATA,
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo, SetupDiEnumDeviceInterfaces,
    SetupDiGetClassDevsW, SetupDiGetDeviceInstanceIdW, SetupDiGetDeviceInterfaceDetailW,
    SetupDiGetDevicePropertyW,
};
use windows::Win32::Devices::Properties::{DEVPKEY_Device_SessionId, DEVPROPTYPE};
use windows::core::{GUID, PCWSTR};

use crate::remote::backend::rdp::SessionDisplayProbe;
use crate::remote::error::DisplayError;
use crate::remote::ioctl::Guid;

use super::{from_wide, win32_of};

/// `DEVPROP_TYPE_UINT32`.
const DEVPROP_TYPE_UINT32: u32 = 0x0000_0007;

pub(crate) fn to_guid(g: &Guid) -> GUID {
    GUID {
        data1: g.data1,
        data2: g.data2,
        data3: g.data3,
        data4: g.data4,
    }
}

/// RAII wrapper of a device information set.
struct DevInfo(HDEVINFO);

impl Drop for DevInfo {
    fn drop(&mut self) {
        // SAFETY: handle came from SetupDiGetClassDevsW and is destroyed once.
        unsafe {
            let _ = SetupDiDestroyDeviceInfoList(self.0);
        }
    }
}

fn class_devs(guid: &GUID, interfaces: bool) -> Result<DevInfo, DisplayError> {
    let flags = if interfaces {
        DIGCF_PRESENT | DIGCF_DEVICEINTERFACE
    } else {
        DIGCF_PRESENT
    };
    // SAFETY: valid GUID pointer, no enumerator / window.
    let h =
        unsafe { SetupDiGetClassDevsW(Some(guid), PCWSTR::null(), None, flags) }.map_err(|e| {
            DisplayError::Io(format!(
                "SetupDiGetClassDevsW: win32 error {}",
                win32_of(&e)
            ))
        })?;
    Ok(DevInfo(h))
}

fn session_of(set: &DevInfo, data: &SP_DEVINFO_DATA) -> Option<u32> {
    let mut ty = DEVPROPTYPE(0);
    let mut buf = [0u8; 4];
    // SAFETY: buffer is 4 bytes, the documented size of a DEVPROP_TYPE_UINT32 value.
    let ok = unsafe {
        SetupDiGetDevicePropertyW(
            set.0,
            data,
            &DEVPKEY_Device_SessionId,
            &mut ty,
            Some(&mut buf),
            None,
            0,
        )
    };
    (ok.is_ok() && ty.0 == DEVPROP_TYPE_UINT32).then(|| u32::from_le_bytes(buf))
}

fn instance_id(set: &DevInfo, data: &SP_DEVINFO_DATA) -> String {
    let mut buf = [0u16; 512];
    // SAFETY: buffer length passed through the slice.
    match unsafe { SetupDiGetDeviceInstanceIdW(set.0, data, Some(&mut buf), None) } {
        Ok(()) => from_wide(&buf),
        Err(_) => String::new(),
    }
}

/// (device instance id, session id) of every present Display-class device that carries
/// `DEVPKEY_Device_SessionId` (remote display adapters do; console GPUs do not).
pub fn display_devices_by_session() -> Result<Vec<(String, u32)>, DisplayError> {
    let set = class_devs(&GUID_DEVCLASS_DISPLAY, false)?;
    let mut out = Vec::new();
    for i in 0.. {
        let mut data = SP_DEVINFO_DATA {
            cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
            ..Default::default()
        };
        // SAFETY: data has its cbSize set; enumeration stops at the first error (no more items).
        if unsafe { SetupDiEnumDeviceInfo(set.0, i, &mut data) }.is_err() {
            break;
        }
        if let Some(s) = session_of(&set, &data) {
            out.push((instance_id(&set, &data), s));
        }
    }
    Ok(out)
}

/// (interface path, session id) of every present instance of `interface`.
pub fn interface_paths(interface: &Guid) -> Result<Vec<(String, Option<u32>)>, DisplayError> {
    let g = to_guid(interface);
    let set = class_devs(&g, true)?;
    let mut out = Vec::new();
    for i in 0.. {
        let mut idata = SP_DEVICE_INTERFACE_DATA {
            cbSize: size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
            ..Default::default()
        };
        // SAFETY: idata has its cbSize set.
        if unsafe { SetupDiEnumDeviceInterfaces(set.0, None, &g, i, &mut idata) }.is_err() {
            break;
        }
        let mut needed = 0u32;
        // SAFETY: size query with no buffer; fails with ERROR_INSUFFICIENT_BUFFER by design.
        let _ = unsafe {
            SetupDiGetDeviceInterfaceDetailW(set.0, &idata, None, 0, Some(&mut needed), None)
        };
        if needed == 0 {
            continue;
        }
        // u32-aligned buffer for SP_DEVICE_INTERFACE_DETAIL_DATA_W
        let mut buf = vec![0u32; (needed as usize).div_ceil(4)];
        let detail = buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
        let mut ddata = SP_DEVINFO_DATA {
            cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
            ..Default::default()
        };
        // SAFETY: buffer is `needed` bytes; cbSize must be the size of the fixed part.
        let ok = unsafe {
            (*detail).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
            SetupDiGetDeviceInterfaceDetailW(
                set.0,
                &idata,
                Some(detail),
                needed,
                None,
                Some(&mut ddata),
            )
        };
        if ok.is_err() {
            continue;
        }
        // DevicePath starts right after cbSize.
        // SAFETY: the API wrote a NUL-terminated path into the buffer.
        let path = unsafe {
            let p = std::ptr::addr_of!((*detail).DevicePath) as *const u16;
            let max = (needed as usize - 4) / 2;
            from_wide(std::slice::from_raw_parts(p, max))
        };
        out.push((path, session_of(&set, &ddata)));
    }
    Ok(out)
}

/// `SessionDisplayProbe` over SetupAPI.
#[derive(Debug, Default, Clone, Copy)]
pub struct WinSessionProbe;

impl SessionDisplayProbe for WinSessionProbe {
    fn session_has_display(&self, session_id: u32) -> Result<bool, DisplayError> {
        Ok(display_devices_by_session()?
            .iter()
            .any(|(_, s)| *s == session_id))
    }
}
