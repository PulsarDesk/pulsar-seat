// SPDX-License-Identifier: GPL-3.0-only
//! Driver swap of one USB devnode to the inbox `winusb.inf` and back (osms G2,
//! [`crate::usb_capture::UsbSwap`]).
//!
//! Written from Microsoft's SetupAPI / `newdev` documentation (`SetupDiOpenDeviceInfoW`,
//! `SetupDiSetDeviceRegistryPropertyW(SPDRP_CLASSGUID)`, `SetupDiSetDeviceInstallParamsW`
//! with `DI_ENUMSINGLEINF`, `SetupDiBuildDriverInfoList`, `DiInstallDevice`) and the
//! WinUSB installation docs (`Device Parameters\DeviceInterfaceGUIDs`). Only the named
//! devnode changes: never a hardware-id-wide update, never a class filter, never an INF
//! of ours. Structures are windows-sys types sized with `size_of` (same code on x64 and
//! ARM64; `newdev.dll` exports `DiInstallDevice` on both, ARM64 HYPOTHESIS until run).
//!
//! **Untested on hardware (HYPOTHESIS):** that `DiInstallDevice` accepts a class-list
//! driver node of `winusb.inf` for a HID interface whose ids `winusb.inf` does not list;
//! that the swap survives a replug of the same port; the restore path through the
//! compatible list of `input.inf`. Test in the VM with USB passthrough first
//! (docs/progress/osms-local-parity.md, G2 point 8).

use super::{cr_error, guid, last_error, wide};
use crate::platform::{ChangeCallback, Guard};
use crate::usb_capture::{INTERFACE_GUID, OriginalDriver, USB_DEVICE_CLASS, UsbSwap};
use seat_core::backend::{BackendError, BackendResult, ErrorKind};
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_GET_DEVICE_INTERFACE_LIST_ALL_DEVICES, CM_Get_Device_Interface_List_SizeW, CM_Get_Device_Interface_ListW, CM_Get_Device_Interface_PropertyW,
    CM_LOCATE_DEVNODE_NORMAL, CM_Locate_DevNodeW, CR_BUFFER_SMALL, CR_SUCCESS, DI_ENUMSINGLEINF, DI_FLAGSEX_ALLOWEXCLUDEDDRVS, DICS_FLAG_GLOBAL, DIREG_DEV,
    DIREG_DRV, DiInstallDevice, HDEVINFO, SETUP_DI_DRIVER_TYPE, SP_DEVINFO_DATA, SP_DEVINSTALL_PARAMS_W, SP_DRVINFO_DATA_V2_W, SPDIT_CLASSDRIVER,
    SPDIT_COMPATDRIVER, SPDRP_CLASSGUID, SetupDiBuildDriverInfoList, SetupDiCreateDeviceInfoList, SetupDiDestroyDeviceInfoList, SetupDiDestroyDriverInfoList,
    SetupDiEnumDriverInfoW, SetupDiGetDeviceInstallParamsW, SetupDiGetDeviceRegistryPropertyW, SetupDiOpenDevRegKey, SetupDiOpenDeviceInfoW,
    SetupDiRemoveDevice, SetupDiSetDeviceInstallParamsW, SetupDiSetDeviceRegistryPropertyW,
};
use windows_sys::Win32::Devices::Properties::DEVPKEY_Device_InstanceId;
use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_NO_SUCH_DEVINST, GetLastError, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Registry::{
    HKEY, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_MULTI_SZ, RegCloseKey, RegDeleteValueW, RegQueryValueExW, RegSetValueExW,
};
use windows_sys::Win32::System::SystemInformation::GetSystemWindowsDirectoryW;

/// SetupAPI's `DIREG_DEV` key value name WinUSB reads its interface GUIDs from.
const INTERFACE_GUIDS_VALUE: &str = "DeviceInterfaceGUIDs";

/// `%windir%\INF\<name>` (the system's own folder, never a path from elsewhere).
fn inf_path(name: &str) -> Option<Vec<u16>> {
    // Only a bare INF file name as Windows records it (`input.inf`, `oem12.inf`).
    if name.is_empty() || name.contains(['\\', '/', ':']) || !name.to_ascii_lowercase().ends_with(".inf") {
        return None;
    }
    let mut buf = [0u16; 260];
    // SAFETY: buffer of 260 UTF-16 units.
    let n = unsafe { GetSystemWindowsDirectoryW(buf.as_mut_ptr(), buf.len() as u32) } as usize;
    if n == 0 || n >= buf.len() {
        return None;
    }
    let dir = String::from_utf16_lossy(&buf[..n]);
    let full = format!("{dir}\\INF\\{name}");
    (full.len() < 259).then(|| wide(&full))
}

/// One devnode opened in its own device-information set.
struct DevInfo {
    set: HDEVINFO,
    data: SP_DEVINFO_DATA,
}

impl DevInfo {
    fn open(node: &str) -> BackendResult<Self> {
        // SAFETY: no class filter, no window.
        let set = unsafe { SetupDiCreateDeviceInfoList(std::ptr::null(), std::ptr::null_mut()) };
        if set == INVALID_HANDLE_VALUE as HDEVINFO {
            return Err(last_error("setupdi_failed", "SetupDiCreateDeviceInfoList"));
        }
        let mut data = SP_DEVINFO_DATA { cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32, ..Default::default() };
        let id = wide(node);
        // SAFETY: set from above, NUL-terminated id, sized out struct.
        if unsafe { SetupDiOpenDeviceInfoW(set, id.as_ptr(), std::ptr::null_mut(), 0, &mut data) } == 0 {
            // SAFETY: no arguments.
            let e = unsafe { GetLastError() };
            // SAFETY: set from above.
            unsafe { SetupDiDestroyDeviceInfoList(set) };
            let kind = if e == ERROR_NO_SUCH_DEVINST { ErrorKind::NotFound } else { ErrorKind::Os };
            return Err(BackendError::new(kind, "device_not_found", format!("SetupDiOpenDeviceInfoW({node}): {e}")).with_win32(e));
        }
        Ok(Self { set, data })
    }

    fn class_guid(&self) -> Option<String> {
        let mut buf = [0u8; 128];
        let mut need = 0u32;
        // SAFETY: sized buffer.
        let ok = unsafe {
            SetupDiGetDeviceRegistryPropertyW(self.set, &self.data, SPDRP_CLASSGUID, std::ptr::null_mut(), buf.as_mut_ptr(), buf.len() as u32, &mut need)
        };
        if ok == 0 {
            return None;
        }
        let w: Vec<u16> = buf[..need as usize].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|c| *c != 0).collect();
        Some(String::from_utf16_lossy(&w).to_ascii_lowercase())
    }

    fn set_class_guid(&mut self, g: &str) -> BackendResult<()> {
        let w = wide(g);
        let bytes: Vec<u8> = w.iter().flat_map(|c| c.to_le_bytes()).collect();
        // SAFETY: REG_SZ bytes incl. the terminating NUL.
        if unsafe { SetupDiSetDeviceRegistryPropertyW(self.set, &mut self.data, SPDRP_CLASSGUID, bytes.as_ptr(), bytes.len() as u32) } == 0 {
            return Err(last_error("set_class_failed", "SetupDiSetDeviceRegistryPropertyW(SPDRP_CLASSGUID)"));
        }
        Ok(())
    }

    fn key(&self, which: u32, access: u32) -> Option<RegKey> {
        // SAFETY: valid set / data; returns INVALID_HANDLE_VALUE on failure.
        let k = unsafe { SetupDiOpenDevRegKey(self.set, &self.data, DICS_FLAG_GLOBAL, 0, which, access) };
        (k as isize != INVALID_HANDLE_VALUE as isize && !k.is_null()).then_some(RegKey(k))
    }

    fn inf_name(&self) -> Option<String> {
        let k = self.key(DIREG_DRV, KEY_QUERY_VALUE)?;
        let name = wide("InfPath");
        let mut buf = [0u16; 260];
        let mut len = (buf.len() * 2) as u32;
        // SAFETY: sized buffer; REG_SZ read.
        let r = unsafe { RegQueryValueExW(k.0, name.as_ptr(), std::ptr::null(), std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut len) };
        if r != 0 {
            return None;
        }
        let w: Vec<u16> = buf[..(len as usize / 2).min(buf.len())].iter().copied().take_while(|c| *c != 0).collect();
        Some(String::from_utf16_lossy(&w))
    }

    fn set_interface_guid(&self, on: bool) -> BackendResult<()> {
        let access = KEY_SET_VALUE;
        let Some(k) = self.key(DIREG_DEV, access) else {
            return Err(last_error("device_key_failed", "SetupDiOpenDevRegKey(DIREG_DEV)"));
        };
        let name = wide(INTERFACE_GUIDS_VALUE);
        if on {
            // REG_MULTI_SZ: one GUID, then the double NUL.
            let mut v: Vec<u16> = INTERFACE_GUID.encode_utf16().collect();
            v.extend([0, 0]);
            // SAFETY: open key; byte length of the multi-sz.
            let r = unsafe { RegSetValueExW(k.0, name.as_ptr(), 0, REG_MULTI_SZ, v.as_ptr().cast(), (v.len() * 2) as u32) };
            if r != 0 {
                return Err(BackendError::new(ErrorKind::Os, "registry_failed", format!("RegSetValueExW({INTERFACE_GUIDS_VALUE}): {r}")).with_win32(r));
            }
        } else {
            // SAFETY: open key.
            let r = unsafe { RegDeleteValueW(k.0, name.as_ptr()) };
            if r != 0 && r != ERROR_FILE_NOT_FOUND {
                return Err(BackendError::new(ErrorKind::Os, "registry_failed", format!("RegDeleteValueW({INTERFACE_GUIDS_VALUE}): {r}")).with_win32(r));
            }
        }
        Ok(())
    }

    /// Install the first driver of `inf` (one INF only) from the list `kind` on this node.
    fn install_from(&mut self, inf: &[u16], kind: SETUP_DI_DRIVER_TYPE) -> BackendResult<bool> {
        let mut p = SP_DEVINSTALL_PARAMS_W { cbSize: std::mem::size_of::<SP_DEVINSTALL_PARAMS_W>() as u32, ..unsafe { std::mem::zeroed() } };
        // SAFETY: sized struct.
        if unsafe { SetupDiGetDeviceInstallParamsW(self.set, &self.data, &mut p) } == 0 {
            return Err(last_error("setupdi_failed", "SetupDiGetDeviceInstallParamsW"));
        }
        p.Flags |= DI_ENUMSINGLEINF;
        p.FlagsEx |= DI_FLAGSEX_ALLOWEXCLUDEDDRVS;
        p.DriverPath = [0; 260];
        let n = inf.len().min(259);
        p.DriverPath[..n].copy_from_slice(&inf[..n]);
        // SAFETY: as above.
        if unsafe { SetupDiSetDeviceInstallParamsW(self.set, &self.data, &p) } == 0 {
            return Err(last_error("setupdi_failed", "SetupDiSetDeviceInstallParamsW"));
        }
        // SAFETY: valid set / data.
        if unsafe { SetupDiBuildDriverInfoList(self.set, &mut self.data, kind) } == 0 {
            return Err(last_error("driver_list_failed", "SetupDiBuildDriverInfoList"));
        }
        let mut drv = SP_DRVINFO_DATA_V2_W { cbSize: std::mem::size_of::<SP_DRVINFO_DATA_V2_W>() as u32, ..unsafe { std::mem::zeroed() } };
        // SAFETY: sized struct.
        let found = unsafe { SetupDiEnumDriverInfoW(self.set, &self.data, kind, 0, &mut drv) } != 0;
        let result = if found {
            let mut reboot = 0;
            // SAFETY: driver node from the list built for this device.
            if unsafe { DiInstallDevice(std::ptr::null_mut(), self.set, &self.data, &drv, 0, &mut reboot) } == 0 {
                Err(last_error("di_install_failed", "DiInstallDevice"))
            } else {
                if reboot != 0 {
                    log::warn!("USB capture: Windows asks for a restart to finish a driver change");
                }
                Ok(true)
            }
        } else {
            Ok(false)
        };
        // SAFETY: list built above.
        unsafe { SetupDiDestroyDriverInfoList(self.set, &self.data, kind) };
        result
    }

    fn install(&mut self, inf_name: &str, lists: &[SETUP_DI_DRIVER_TYPE]) -> BackendResult<()> {
        let inf = inf_path(inf_name).ok_or_else(|| BackendError::new(ErrorKind::Internal, "bad_inf_name", inf_name.to_string()))?;
        for kind in lists {
            if self.install_from(&inf, *kind)? {
                return Ok(());
            }
        }
        Err(BackendError::new(ErrorKind::NotFound, "no_driver_in_inf", format!("{inf_name} has no driver for this device")))
    }
}

impl Drop for DevInfo {
    fn drop(&mut self) {
        // SAFETY: set created in `open`.
        unsafe { SetupDiDestroyDeviceInfoList(self.set) };
    }
}

struct RegKey(HKEY);

impl Drop for RegKey {
    fn drop(&mut self) {
        // SAFETY: key opened by SetupDiOpenDevRegKey.
        unsafe { RegCloseKey(self.0) };
    }
}

/// The node is present now (`CM_Locate_DevNodeW` without the phantom flag).
fn present(node: &str) -> bool {
    let id = wide(node);
    let mut inst = 0u32;
    // SAFETY: NUL-terminated id, out value.
    unsafe { CM_Locate_DevNodeW(&mut inst, id.as_ptr(), CM_LOCATE_DEVNODE_NORMAL) == CR_SUCCESS }
}

/// SetupAPI-backed swapper (the service runs as LocalSystem, which these calls need).
pub struct WinUsbSwap;

impl UsbSwap for WinUsbSwap {
    fn check(&self) -> Result<(), String> {
        match inf_path("winusb.inf") {
            Some(p) => {
                let s = String::from_utf16_lossy(&p[..p.len() - 1]);
                if std::path::Path::new(&s).exists() { Ok(()) } else { Err("winusb_inf_missing".into()) }
            }
            None => Err("windows_dir_unknown".into()),
        }
    }

    fn original(&self, node: &str) -> BackendResult<OriginalDriver> {
        let d = DevInfo::open(node)?;
        let class_guid = d.class_guid().ok_or_else(|| last_error("class_unknown", "SPDRP_CLASSGUID"))?;
        if class_guid == USB_DEVICE_CLASS {
            // Already on WinUSB (a run that crashed between swap and journal cannot
            // happen: the journal comes first; this is someone else's WinUSB device).
            return Err(BackendError::new(ErrorKind::Coexistence, "already_winusb", format!("{node} already runs WinUSB")));
        }
        let inf_path = d.inf_name().unwrap_or_else(|| "input.inf".into());
        Ok(OriginalDriver { class_guid, inf_path })
    }

    fn capture(&self, node: &str) -> BackendResult<()> {
        let mut d = DevInfo::open(node)?;
        d.set_interface_guid(true)?;
        d.set_class_guid(USB_DEVICE_CLASS)?;
        d.install("winusb.inf", &[SPDIT_CLASSDRIVER, SPDIT_COMPATDRIVER])
    }

    fn release(&self, node: &str, original: &OriginalDriver) -> BackendResult<()> {
        let mut d = match DevInfo::open(node) {
            Ok(d) => d,
            Err(e) if e.kind == ErrorKind::NotFound => {
                // The node no longer exists at all (removed in Device Manager, or Windows'
                // clean-up of long-absent devices): nothing of ours is left on it, and a
                // replugged device is enumerated fresh with its own driver. Done, so the
                // journal entry goes instead of failing at every start.
                log::info!("USB release of {node}: device node no longer exists; nothing to restore");
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        if !present(node) {
            // Unplugged: a driver cannot be installed on a phantom node. Remove the node
            // (its `Device Parameters` with our interface GUID go with it); when the
            // device is plugged in again, Windows enumerates it fresh and ranks drivers
            // itself, and `winusb.inf` does not match a HID interface's ids, so it gets
            // its own driver back (HYPOTHESIS until run on hardware).
            // SAFETY: valid set / data.
            if unsafe { SetupDiRemoveDevice(d.set, &mut d.data) } == 0 {
                return Err(last_error("remove_failed", "SetupDiRemoveDevice"));
            }
            log::info!("USB release of {node}: device absent, phantom node removed");
            return Ok(());
        }
        d.set_class_guid(&original.class_guid)?;
        let r = d.install(&original.inf_path, &[SPDIT_COMPATDRIVER, SPDIT_CLASSDRIVER]).or_else(|e| {
            if original.inf_path.eq_ignore_ascii_case("input.inf") {
                return Err(e);
            }
            log::warn!("USB release of {node}: {} not usable ({e}); using input.inf", original.inf_path);
            d.set_class_guid(crate::usb_capture::HID_CLASS)?;
            d.install("input.inf", &[SPDIT_COMPATDRIVER, SPDIT_CLASSDRIVER])
        });
        r?;
        // Last, so a failed reinstall leaves the node findable by the start-up sweep.
        d.set_interface_guid(false)
    }

    fn captured_nodes(&self) -> Vec<String> {
        let Some(g) = guid(INTERFACE_GUID) else {
            return Vec::new();
        };
        let mut paths = Vec::new();
        for _ in 0..8 {
            let mut len = 0u32;
            // SAFETY: valid GUID and out pointer.
            let cr = unsafe { CM_Get_Device_Interface_List_SizeW(&mut len, &g, std::ptr::null(), CM_GET_DEVICE_INTERFACE_LIST_ALL_DEVICES) };
            if cr != CR_SUCCESS {
                log::warn!("{}", cr_error(cr, "CM_Get_Device_Interface_List_SizeW(capture GUID)"));
                return Vec::new();
            }
            let mut buf = vec![0u16; len.max(1) as usize];
            // SAFETY: buffer of `len` units.
            let cr = unsafe { CM_Get_Device_Interface_ListW(&g, std::ptr::null(), buf.as_mut_ptr(), len.max(1), CM_GET_DEVICE_INTERFACE_LIST_ALL_DEVICES) };
            if cr == CR_BUFFER_SMALL {
                continue;
            }
            if cr == CR_SUCCESS {
                paths = buf.split(|c| *c == 0).filter(|s| !s.is_empty()).map(|s| s.to_vec()).collect();
            }
            break;
        }
        let mut out = Vec::new();
        for mut p in paths {
            p.push(0);
            let mut ty = 0u32;
            let mut buf = [0u8; 1024];
            let mut size = buf.len() as u32;
            // SAFETY: NUL-terminated path, sized buffer.
            let cr = unsafe { CM_Get_Device_Interface_PropertyW(p.as_ptr(), &DEVPKEY_Device_InstanceId, &mut ty, buf.as_mut_ptr(), &mut size, 0) };
            if cr == CR_SUCCESS {
                let w: Vec<u16> = buf[..size as usize].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|c| *c != 0).collect();
                let id = String::from_utf16_lossy(&w);
                if !id.is_empty() && !out.iter().any(|o: &String| o.eq_ignore_ascii_case(&id)) {
                    out.push(id);
                }
            }
        }
        out
    }

    fn watch(&self, cb: ChangeCallback) -> BackendResult<Box<dyn Guard>> {
        let g = guid(INTERFACE_GUID).ok_or_else(|| BackendError::new(ErrorKind::Internal, "bad_guid", INTERFACE_GUID))?;
        super::watch_interfaces(&[g], cb)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inf_paths_are_system_only() {
        let p = inf_path("winusb.inf").unwrap();
        let s = String::from_utf16_lossy(&p[..p.len() - 1]).to_ascii_lowercase();
        assert!(s.ends_with("\\inf\\winusb.inf"), "{s}");
        for bad in ["", "..\\x.inf", "C:\\x.inf", "a/b.inf", "x.sys"] {
            assert!(inf_path(bad).is_none(), "{bad}");
        }
    }

    /// Read-only on this PC: the inbox INF exists and nothing is captured.
    #[test]
    fn check_and_sweep_are_read_only() {
        assert_eq!(WinUsbSwap.check(), Ok(()));
        assert!(WinUsbSwap.captured_nodes().is_empty(), "no node carries the Pulsar capture interface");
        assert_eq!(WinUsbSwap.original(r"USB\VID_0000&PID_0000\NOPE").unwrap_err().kind, ErrorKind::NotFound);
        // A journaled node that no longer exists is done (nothing is changed: the open fails).
        assert_eq!(WinUsbSwap.release(r"USB\VID_0000&PID_0000\NOPE", &OriginalDriver::hid_default()).map_err(|e| e.reason), Ok(()));
    }
}
