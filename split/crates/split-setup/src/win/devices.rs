// SPDX-License-Identifier: GPL-3.0-only
//! PnP devnodes through SetupAPI / CfgMgr / newdev (public Win32 API, the same calls the
//! open-source `devcon` sample documents): enumerate including non-present nodes, remove
//! a node (`DiUninstallDevice`), create a root-enumerated node and install an INF on it.

use super::wide;
use crate::error::SetupError;
use crate::system::{DeviceInfo, DeviceManager};
use std::path::Path;
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_DevNode_Status, CR_SUCCESS, DICD_GENERATE_ID, DIF_REGISTERDEVICE, DIF_REMOVE, DIGCF_ALLCLASSES, DiUninstallDevice,
    HDEVINFO, INSTALLFLAG_FORCE, SP_DEVINFO_DATA, SPDRP_HARDWAREID, SetupDiCallClassInstaller, SetupDiCreateDeviceInfoList,
    SetupDiCreateDeviceInfoW, SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo, SetupDiGetClassDevsW,
    SetupDiGetDeviceInstanceIdW, SetupDiGetDeviceRegistryPropertyW, SetupDiGetINFClassW, SetupDiOpenDeviceInfoW,
    SetupDiSetDeviceRegistryPropertyW, UpdateDriverForPlugAndPlayDevicesW,
};
use windows_sys::Win32::Foundation::{GetLastError, INVALID_HANDLE_VALUE};
use windows_sys::core::GUID;

#[derive(Debug, Default, Clone, Copy)]
pub struct WinDevices;

fn last(what: &str) -> SetupError {
    // SAFETY: trivial.
    let code = unsafe { GetLastError() };
    SetupError::Device(format!("{what} failed (win32 error {code})"))
}

/// Owned device information set.
struct DevInfo(HDEVINFO);

impl Drop for DevInfo {
    fn drop(&mut self) {
        // SAFETY: handle from SetupDiGetClassDevsW / SetupDiCreateDeviceInfoList.
        unsafe { SetupDiDestroyDeviceInfoList(self.0) };
    }
}

fn new_data() -> SP_DEVINFO_DATA {
    SP_DEVINFO_DATA { cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32, ..unsafe { std::mem::zeroed() } }
}

pub fn parse_guid(s: &str) -> Option<GUID> {
    let h: String = s.trim_matches(|c| c == '{' || c == '}').chars().filter(|c| *c != '-').collect();
    if h.len() != 32 {
        return None;
    }
    u128::from_str_radix(&h, 16).ok().map(GUID::from_u128)
}

fn multi_sz(bytes: &[u8]) -> Vec<String> {
    let w: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    w.split(|&c| c == 0).filter(|s| !s.is_empty()).map(String::from_utf16_lossy).collect()
}

unsafe fn instance_id(set: HDEVINFO, data: &SP_DEVINFO_DATA) -> Option<String> {
    let mut buf = vec![0u16; 512];
    let mut req = 0u32;
    // SAFETY: buffer length passed in u16 units.
    let ok = unsafe { SetupDiGetDeviceInstanceIdW(set, data, buf.as_mut_ptr(), buf.len() as u32, &mut req) };
    if ok == 0 {
        return None;
    }
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..n]))
}

unsafe fn hardware_ids(set: HDEVINFO, data: &SP_DEVINFO_DATA) -> Vec<String> {
    let mut buf = vec![0u8; 4096];
    let (mut ty, mut req) = (0u32, 0u32);
    // SAFETY: buffer length passed in bytes.
    let ok = unsafe {
        SetupDiGetDeviceRegistryPropertyW(set, data, SPDRP_HARDWAREID, &mut ty, buf.as_mut_ptr(), buf.len() as u32, &mut req)
    };
    if ok == 0 { Vec::new() } else { multi_sz(&buf[..(req as usize).min(buf.len())]) }
}

impl DeviceManager for WinDevices {
    fn list(&self) -> Result<Vec<DeviceInfo>, SetupError> {
        // No DIGCF_PRESENT: include non-present (phantom) devnodes.
        // SAFETY: SetupAPI enumeration; the set is destroyed by the guard.
        unsafe {
            let h = SetupDiGetClassDevsW(std::ptr::null(), std::ptr::null(), std::ptr::null_mut(), DIGCF_ALLCLASSES);
            if h == INVALID_HANDLE_VALUE as HDEVINFO {
                return Err(last("SetupDiGetClassDevsW"));
            }
            let set = DevInfo(h);
            let mut out = Vec::new();
            let mut i = 0;
            loop {
                let mut d = new_data();
                if SetupDiEnumDeviceInfo(set.0, i, &mut d) == 0 {
                    break;
                }
                i += 1;
                let Some(id) = instance_id(set.0, &d) else { continue };
                let (mut st, mut prob) = (0u32, 0u32);
                let present = CM_Get_DevNode_Status(&mut st, &mut prob, d.DevInst, 0) == CR_SUCCESS;
                out.push(DeviceInfo { instance_id: id, hardware_ids: hardware_ids(set.0, &d), present });
            }
            Ok(out)
        }
    }

    fn remove(&self, instance_id: &str) -> Result<bool, SetupError> {
        let wid = wide(instance_id);
        // SAFETY: open one devnode by instance id in a fresh set, uninstall it.
        unsafe {
            let h = SetupDiCreateDeviceInfoList(std::ptr::null(), std::ptr::null_mut());
            if h == INVALID_HANDLE_VALUE as HDEVINFO {
                return Err(last("SetupDiCreateDeviceInfoList"));
            }
            let set = DevInfo(h);
            let mut d = new_data();
            if SetupDiOpenDeviceInfoW(set.0, wid.as_ptr(), std::ptr::null_mut(), 0, &mut d) == 0 {
                return Ok(false); // already gone
            }
            let mut reboot = 0;
            if DiUninstallDevice(std::ptr::null_mut(), set.0, &d, 0, &mut reboot) == 0 {
                return Err(last("DiUninstallDevice"));
            }
            Ok(reboot != 0)
        }
    }

    fn create_root_device(&self, hardware_id: &str, class_guid: &str, inf: &Path) -> Result<(String, bool), SetupError> {
        let winf = wide(&inf.display().to_string());
        let whw = wide(hardware_id);
        // SAFETY: devcon-style "install" sequence; on failure after registration the new
        // devnode is removed again (DIF_REMOVE).
        unsafe {
            let mut guid = parse_guid(class_guid).ok_or_else(|| SetupError::Device(format!("bad class guid {class_guid}")))?;
            let mut class_name = vec![0u16; 64];
            let mut req = 0u32;
            if SetupDiGetINFClassW(winf.as_ptr(), &mut guid, class_name.as_mut_ptr(), class_name.len() as u32, &mut req) == 0 {
                return Err(last("SetupDiGetINFClassW"));
            }
            let h = SetupDiCreateDeviceInfoList(&guid, std::ptr::null_mut());
            if h == INVALID_HANDLE_VALUE as HDEVINFO {
                return Err(last("SetupDiCreateDeviceInfoList"));
            }
            let set = DevInfo(h);
            let mut d = new_data();
            if SetupDiCreateDeviceInfoW(
                set.0,
                class_name.as_ptr(),
                &guid,
                std::ptr::null(),
                std::ptr::null_mut(),
                DICD_GENERATE_ID,
                &mut d,
            ) == 0
            {
                return Err(last("SetupDiCreateDeviceInfoW"));
            }
            // REG_MULTI_SZ: id NUL NUL.
            let mut hw: Vec<u16> = hardware_id.encode_utf16().collect();
            hw.extend([0, 0]);
            let bytes: Vec<u8> = hw.iter().flat_map(|c| c.to_le_bytes()).collect();
            if SetupDiSetDeviceRegistryPropertyW(set.0, &mut d, SPDRP_HARDWAREID, bytes.as_ptr(), bytes.len() as u32) == 0 {
                return Err(last("SetupDiSetDeviceRegistryPropertyW"));
            }
            if SetupDiCallClassInstaller(DIF_REGISTERDEVICE, set.0, &d) == 0 {
                return Err(last("DIF_REGISTERDEVICE"));
            }
            let id = instance_id(set.0, &d).unwrap_or_default();
            let mut reboot = 0;
            if UpdateDriverForPlugAndPlayDevicesW(std::ptr::null_mut(), whw.as_ptr(), winf.as_ptr(), INSTALLFLAG_FORCE, &mut reboot)
                == 0
            {
                let e = last("UpdateDriverForPlugAndPlayDevicesW");
                SetupDiCallClassInstaller(DIF_REMOVE, set.0, &d);
                return Err(e);
            }
            Ok((id, reboot != 0))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guid_parse() {
        let g = parse_guid("{4d36e968-e325-11ce-bfc1-08002be10318}").unwrap();
        assert_eq!(g.data1, 0x4d36e968);
        assert_eq!(g.data4, [0xbf, 0xc1, 0x08, 0x00, 0x2b, 0xe1, 0x03, 0x18]);
        assert!(parse_guid("x").is_none());
    }

    /// Read-only enumeration of the dev machine's PnP tree.
    #[test]
    fn list_read_only() {
        let v = WinDevices.list().unwrap();
        assert!(v.iter().any(|d| d.present));
    }
}
