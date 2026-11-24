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
//! Hardware (x64 gaming PC, 2026-11-24): capture of a Logitech receiver's MI_00 / MI_01 and
//! the swap back worked. Found there (bug 5): Windows keeps a device interface registered
//! after the driver that registered it is gone, so `CM_Get_Device_Interface_ListW(…_ALL_DEVICES)`
//! still lists restored nodes; the old sweep then reinstalled `input.inf` over the vendor
//! driver (`oem128.inf`) the release had just put back, and the extension INF
//! (`oem185.inf`) and its software component were gone. Now [`WinUsbSwap::still_captured`]
//! reads the node's own state, the restore installs the journaled INF's matching section
//! and version, falls back to the driver Windows ranks best (never a fixed `input.inf`),
//! and re-applies the journaled extension INFs.
//!
//! **Untested on hardware (HYPOTHESIS):** the section / version pick, `install_best`, the
//! extension re-apply through `DiInstallDriverW`; that the swap survives a replug of the
//! same port.

use super::{cr_error, guid, last_error, wide};
use crate::platform::{ChangeCallback, Guard};
use crate::usb_capture::{INTERFACE_GUID, OriginalDriver, USB_DEVICE_CLASS, UsbSwap};
use seat_core::backend::{BackendError, BackendResult, ErrorKind};
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_GET_DEVICE_INTERFACE_LIST_ALL_DEVICES, CM_Get_Device_Interface_List_SizeW, CM_Get_Device_Interface_ListW, CM_Get_Device_Interface_PropertyW,
    CM_LOCATE_DEVNODE_NORMAL, CM_Locate_DevNodeW, CR_BUFFER_SMALL, CR_SUCCESS, DI_ENUMSINGLEINF, DI_FLAGSEX_ALLOWEXCLUDEDDRVS, DICS_FLAG_GLOBAL, DIREG_DEV,
    DIREG_DRV, DiInstallDevice, HDEVINFO, SETUP_DI_DRIVER_TYPE, SP_DEVINFO_DATA, SP_DEVINSTALL_PARAMS_W, SP_DRVINFO_DATA_V2_W, SPDIT_CLASSDRIVER,
    SPDIT_COMPATDRIVER, SPDRP_CLASSGUID, SetupDiBuildDriverInfoList, SetupDiCreateDeviceInfoList, SetupDiDestroyDeviceInfoList, SetupDiDestroyDriverInfoList,
    DiInstallDriverW, SP_DRVINFO_DETAIL_DATA_W, SetupDiEnumDriverInfoW, SetupDiGetDeviceInstallParamsW, SetupDiGetDevicePropertyW,
    SetupDiGetDeviceRegistryPropertyW, SetupDiGetDriverInfoDetailW, SetupDiGetSelectedDriverW, SetupDiOpenDevRegKey, SetupDiOpenDeviceInfoW,
    SetupDiRemoveDevice, SetupDiSelectBestCompatDrv, SetupDiSetDeviceInstallParamsW, SetupDiSetDeviceRegistryPropertyW,
};
use windows_sys::Win32::Devices::Properties::{
    DEVPKEY_Device_DriverInfPath, DEVPKEY_Device_DriverInfSection, DEVPKEY_Device_DriverProvider, DEVPKEY_Device_DriverVersion,
    DEVPKEY_Device_ExtendedConfigurationIds, DEVPKEY_Device_InstanceId, DEVPKEY_Device_MatchingDeviceId, DEVPROP_TYPE_STRING, DEVPROP_TYPE_STRING_LIST,
    DEVPROPTYPE,
};
use windows_sys::Win32::Foundation::{
    DEVPROPKEY, ERROR_FILE_NOT_FOUND, ERROR_INSUFFICIENT_BUFFER, ERROR_NO_SUCH_DEVINST, ERROR_SHUTDOWN_IN_PROGRESS, GetLastError, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::System::Registry::{
    HKEY, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_MULTI_SZ, RegCloseKey, RegDeleteValueW, RegQueryValueExW, RegSetValueExW,
};
use windows_sys::Win32::System::SystemInformation::GetSystemWindowsDirectoryW;

/// SetupAPI's `DIREG_DEV` key value name WinUSB reads its interface GUIDs from.
const INTERFACE_GUIDS_VALUE: &str = "DeviceInterfaceGUIDs";

/// `%windir%\INF\<name>` (the system's own folder, never a path from elsewhere).
fn inf_path(name: &str) -> Option<Vec<u16>> {
    // Only a bare INF file name as Windows records it (`input.inf`, `oem12.inf`).
    if !crate::usb_capture::is_bare_inf_name(name) {
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

    /// A device property as strings (`DEVPROP_TYPE_STRING` or `_STRING_LIST`); `None` when
    /// it is not set or has another type.
    fn prop_strings(&self, key: &DEVPROPKEY) -> Option<Vec<String>> {
        let mut ty: DEVPROPTYPE = 0;
        let mut need = 0u32;
        // SAFETY: size query (null buffer, zero size).
        unsafe { SetupDiGetDevicePropertyW(self.set, &self.data, key, &mut ty, std::ptr::null_mut(), 0, &mut need, 0) };
        if need == 0 || need > 64 * 1024 {
            return None;
        }
        let mut buf = vec![0u16; (need as usize).div_ceil(2)];
        // SAFETY: buffer of `need` bytes (u16-aligned).
        if unsafe { SetupDiGetDevicePropertyW(self.set, &self.data, key, &mut ty, buf.as_mut_ptr().cast(), need, &mut need, 0) } == 0 {
            return None;
        }
        if ty != DEVPROP_TYPE_STRING && ty != DEVPROP_TYPE_STRING_LIST {
            return None;
        }
        let v: Vec<String> = buf.split(|c| *c == 0).filter(|s| !s.is_empty()).map(String::from_utf16_lossy).collect();
        Some(v)
    }

    fn prop_string(&self, key: &DEVPROPKEY) -> String {
        self.prop_strings(key).and_then(|v| v.into_iter().next()).unwrap_or_default()
    }

    /// What the node runs now, as Windows reports it.
    fn driver_now(&self) -> OriginalDriver {
        let mut inf_path = self.prop_string(&DEVPKEY_Device_DriverInfPath);
        if inf_path.is_empty() {
            inf_path = self.inf_name().unwrap_or_default();
        }
        OriginalDriver {
            class_guid: self.class_guid().unwrap_or_default(),
            inf_path,
            inf_section: self.prop_string(&DEVPKEY_Device_DriverInfSection),
            driver_version: self.prop_string(&DEVPKEY_Device_DriverVersion),
            provider: self.prop_string(&DEVPKEY_Device_DriverProvider),
            matching_id: self.prop_string(&DEVPKEY_Device_MatchingDeviceId),
            extensions: self.prop_strings(&DEVPKEY_Device_ExtendedConfigurationIds).unwrap_or_default(),
        }
    }

    /// Our GUID is in the node's `Device Parameters\DeviceInterfaceGUIDs` value.
    fn has_interface_guid(&self) -> bool {
        let Some(k) = self.key(DIREG_DEV, KEY_QUERY_VALUE) else { return false };
        let name = wide(INTERFACE_GUIDS_VALUE);
        let mut buf = [0u16; 512];
        let mut len = (buf.len() * 2) as u32;
        // SAFETY: sized buffer.
        let r = unsafe { RegQueryValueExW(k.0, name.as_ptr(), std::ptr::null(), std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut len) };
        if r != 0 {
            return false;
        }
        buf[..(len as usize / 2).min(buf.len())]
            .split(|c| *c == 0)
            .any(|s| String::from_utf16_lossy(s).trim().eq_ignore_ascii_case(INTERFACE_GUID))
    }

    fn set_install_params(&self, single_inf: Option<&[u16]>) -> BackendResult<()> {
        let mut p = SP_DEVINSTALL_PARAMS_W { cbSize: std::mem::size_of::<SP_DEVINSTALL_PARAMS_W>() as u32, ..unsafe { std::mem::zeroed() } };
        // SAFETY: sized struct.
        if unsafe { SetupDiGetDeviceInstallParamsW(self.set, &self.data, &mut p) } == 0 {
            return Err(last_error("setupdi_failed", "SetupDiGetDeviceInstallParamsW"));
        }
        p.DriverPath = [0; 260];
        match single_inf {
            Some(inf) => {
                p.Flags |= DI_ENUMSINGLEINF;
                p.FlagsEx |= DI_FLAGSEX_ALLOWEXCLUDEDDRVS;
                let n = inf.len().min(259);
                p.DriverPath[..n].copy_from_slice(&inf[..n]);
            }
            None => {
                p.Flags &= !DI_ENUMSINGLEINF;
                p.FlagsEx &= !DI_FLAGSEX_ALLOWEXCLUDEDDRVS;
            }
        }
        // SAFETY: as above.
        if unsafe { SetupDiSetDeviceInstallParamsW(self.set, &self.data, &p) } == 0 {
            return Err(last_error("setupdi_failed", "SetupDiSetDeviceInstallParamsW"));
        }
        Ok(())
    }

    /// Install section and INF file name of a driver node of the list built now.
    fn driver_detail(&self, drv: &SP_DRVINFO_DATA_V2_W) -> Option<(String, String)> {
        // u64 words: the detail structure is 8-byte aligned on 64-bit targets.
        let mut buf: Vec<u64> = vec![0; 1024];
        for _ in 0..2 {
            let p = buf.as_mut_ptr().cast::<SP_DRVINFO_DETAIL_DATA_W>();
            // SAFETY: the buffer is larger than the fixed part of the structure.
            unsafe { (*p).cbSize = std::mem::size_of::<SP_DRVINFO_DETAIL_DATA_W>() as u32 };
            let mut need = 0u32;
            // SAFETY: buffer of `buf.len() * 8` bytes.
            let ok = unsafe { SetupDiGetDriverInfoDetailW(self.set, &self.data, drv, p, (buf.len() * 8) as u32, &mut need) } != 0;
            // SAFETY: no arguments.
            if !ok && unsafe { GetLastError() } == ERROR_INSUFFICIENT_BUFFER && need as usize > buf.len() * 8 {
                buf = vec![0; (need as usize).div_ceil(8) + 1];
                continue;
            }
            if !ok {
                return None;
            }
            // SAFETY: filled by the call above.
            let d = unsafe { &*p };
            let z = |w: &[u16]| String::from_utf16_lossy(&w[..w.iter().position(|c| *c == 0).unwrap_or(w.len())]);
            let inf = z(&d.InfFileName);
            let file = inf.rsplit(['\\', '/']).next().unwrap_or_default().to_string();
            return Some((z(&d.SectionName), file));
        }
        None
    }

    /// Install the driver node of `inf` (one INF only) from the list `kind` whose install
    /// section and version match `want` (the first one when none matches). `Ok(None)` =
    /// the INF has no driver for this node in that list; `Ok(Some(section))` = installed.
    fn install_from(&mut self, inf: &[u16], kind: SETUP_DI_DRIVER_TYPE, want: &OriginalDriver) -> BackendResult<Option<String>> {
        self.set_install_params(Some(inf))?;
        // SAFETY: valid set / data.
        if unsafe { SetupDiBuildDriverInfoList(self.set, &mut self.data, kind) } == 0 {
            return Err(last_error("driver_list_failed", "SetupDiBuildDriverInfoList"));
        }
        let mut chosen: Option<(SP_DRVINFO_DATA_V2_W, String)> = None;
        for i in 0..256u32 {
            let mut drv = SP_DRVINFO_DATA_V2_W { cbSize: std::mem::size_of::<SP_DRVINFO_DATA_V2_W>() as u32, ..unsafe { std::mem::zeroed() } };
            // SAFETY: sized struct.
            if unsafe { SetupDiEnumDriverInfoW(self.set, &self.data, kind, i, &mut drv) } == 0 {
                break;
            }
            let section = self.driver_detail(&drv).map(|(s, _)| s).unwrap_or_default();
            let version = version_string(drv.DriverVersion);
            let section_ok = want.inf_section.is_empty() || section.eq_ignore_ascii_case(&want.inf_section);
            let version_ok = want.driver_version.is_empty() || version == want.driver_version;
            if section_ok && version_ok {
                chosen = Some((drv, section));
                break;
            }
            if chosen.is_none() {
                chosen = Some((drv, section));
            }
        }
        let result = match chosen {
            Some((drv, section)) => {
                let mut reboot = 0;
                // SAFETY: driver node from the list built for this device.
                if unsafe { DiInstallDevice(std::ptr::null_mut(), self.set, &self.data, &drv, 0, &mut reboot) } == 0 {
                    Err(last_error("di_install_failed", "DiInstallDevice"))
                } else {
                    if reboot != 0 {
                        log::warn!("USB capture: Windows asks for a restart to finish a driver change");
                    }
                    Ok(Some(section))
                }
            }
            None => Ok(None),
        };
        // SAFETY: list built above.
        unsafe { SetupDiDestroyDriverInfoList(self.set, &self.data, kind) };
        result
    }

    fn install(&mut self, inf_name: &str, lists: &[SETUP_DI_DRIVER_TYPE], want: &OriginalDriver) -> BackendResult<String> {
        let inf = inf_path(inf_name).ok_or_else(|| BackendError::new(ErrorKind::Internal, "bad_inf_name", inf_name.to_string()))?;
        for kind in lists {
            if let Some(section) = self.install_from(&inf, *kind, want)? {
                return Ok(section);
            }
        }
        Err(BackendError::new(ErrorKind::NotFound, "no_driver_in_inf", format!("{inf_name} has no driver for this device")))
    }

    /// Install the compatible driver Windows ranks best from its whole store (the node's
    /// class must already be its own again; `winusb.inf` is never taken).
    fn install_best(&mut self) -> BackendResult<String> {
        self.set_install_params(None)?;
        // SAFETY: valid set / data.
        if unsafe { SetupDiBuildDriverInfoList(self.set, &mut self.data, SPDIT_COMPATDRIVER) } == 0 {
            return Err(last_error("driver_list_failed", "SetupDiBuildDriverInfoList"));
        }
        let result = (|| {
            // SAFETY: list built above.
            if unsafe { SetupDiSelectBestCompatDrv(self.set, &mut self.data) } == 0 {
                return Err(last_error("no_driver_in_inf", "SetupDiSelectBestCompatDrv"));
            }
            let mut drv = SP_DRVINFO_DATA_V2_W { cbSize: std::mem::size_of::<SP_DRVINFO_DATA_V2_W>() as u32, ..unsafe { std::mem::zeroed() } };
            // SAFETY: sized struct.
            if unsafe { SetupDiGetSelectedDriverW(self.set, &self.data, &mut drv) } == 0 {
                return Err(last_error("no_driver_in_inf", "SetupDiGetSelectedDriverW"));
            }
            let (section, file) = self.driver_detail(&drv).unwrap_or_default();
            if file.eq_ignore_ascii_case("winusb.inf") {
                return Err(BackendError::new(ErrorKind::NotFound, "no_driver_in_inf", "only winusb.inf matches"));
            }
            let mut reboot = 0;
            // SAFETY: the selected driver node of this device's list.
            if unsafe { DiInstallDevice(std::ptr::null_mut(), self.set, &self.data, &drv, 0, &mut reboot) } == 0 {
                return Err(last_error("di_install_failed", "DiInstallDevice"));
            }
            if reboot != 0 {
                log::warn!("USB capture: Windows asks for a restart to finish a driver change");
            }
            Ok(format!("{file} [{section}]"))
        })();
        // SAFETY: list built above.
        unsafe { SetupDiDestroyDriverInfoList(self.set, &self.data, SPDIT_COMPATDRIVER) };
        result
    }

    /// Extension INFs of `want` that the node does not carry after the core driver install
    /// are installed again with `DiInstallDriverW` (what `pnputil /add-driver <inf> /install`
    /// does; the gaming PC needed it by hand for the Logitech `oem185.inf`, whose software
    /// component then came back). The INF is already in the driver store and was applied to
    /// this node before; `DiInstallDriverW` applies it to every present device it lists,
    /// which are the devices it was applied to already (HYPOTHESIS: that the reinstall of
    /// the core driver dropped the extension, and that this brings back exactly it).
    fn reapply_extensions(&self, node: &str, want: &OriginalDriver) {
        let infs = want.extension_infs();
        if infs.is_empty() {
            return;
        }
        let now = OriginalDriver { extensions: self.prop_strings(&DEVPKEY_Device_ExtendedConfigurationIds).unwrap_or_default(), ..OriginalDriver::default() };
        let have = now.extension_infs();
        for inf in infs {
            if have.iter().any(|h| h.eq_ignore_ascii_case(&inf)) {
                log::info!("USB release of {node}: extension {inf} is back by itself");
                continue;
            }
            let Some(path) = inf_path(&inf) else { continue };
            let shown = String::from_utf16_lossy(&path[..path.len() - 1]);
            if !std::path::Path::new(&shown).exists() {
                log::warn!("USB release of {node}: extension {inf} is no longer in %windir%\\INF; not re-applied");
                continue;
            }
            let mut reboot = 0;
            // SAFETY: NUL-terminated path of an INF in the system's INF folder.
            if unsafe { DiInstallDriverW(std::ptr::null_mut(), path.as_ptr(), 0, &mut reboot) } == 0 {
                let e = last_error("extension_failed", "DiInstallDriverW");
                log::warn!("USB release of {node}: extension {inf} not re-applied: {e}");
            } else {
                log::info!("USB release of {node}: extension {inf} re-applied{}", if reboot != 0 { " (restart asked)" } else { "" });
            }
        }
    }
}

/// `DriverVersion` of a driver node (`a.b.c.d`, 16 bits each) as Windows prints it.
fn version_string(v: u64) -> String {
    format!("{}.{}.{}.{}", v >> 48, (v >> 32) & 0xFFFF, (v >> 16) & 0xFFFF, v & 0xFFFF)
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
        // INF, section, version and extension INFs as Windows reports them; an INF that
        // cannot be read stays empty (the restore then lets Windows rank the drivers).
        let mut o = d.driver_now();
        o.class_guid = class_guid;
        Ok(o)
    }

    fn current(&self, node: &str) -> Option<OriginalDriver> {
        DevInfo::open(node).ok().map(|d| d.driver_now())
    }

    fn capture(&self, node: &str) -> BackendResult<()> {
        let mut d = DevInfo::open(node)?;
        d.set_interface_guid(true)?;
        d.set_class_guid(USB_DEVICE_CLASS)?;
        d.install("winusb.inf", &[SPDIT_CLASSDRIVER, SPDIT_COMPATDRIVER], &OriginalDriver::default()).map(|_| ())
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
        let class = if original.class_guid.is_empty() { crate::usb_capture::HID_CLASS } else { original.class_guid.as_str() };
        d.set_class_guid(class)?;
        let installed = if original.inf_path.is_empty() {
            d.install_best()?
        } else {
            match d.install(&original.inf_path, &[SPDIT_COMPATDRIVER, SPDIT_CLASSDRIVER], original) {
                Ok(section) => format!("{} [{section}]", original.inf_path),
                // Windows is shutting down (ERROR_SHUTDOWN_IN_PROGRESS): no driver installs
                // now. Gaming PC E2E round 3: the "best driver" fallback below then blocked
                // the service stop for ~3 min and failed anyway; the node stays journaled
                // and the boot-time recovery restores it (it did, "restore check: exact").
                Err(e) if e.win32 == Some(ERROR_SHUTDOWN_IN_PROGRESS) => {
                    log::warn!("USB release of {node}: Windows is shutting down ({e}); left for the restore at the next start");
                    return Err(e);
                }
                Err(e) => {
                    // The package left the driver store, or does not match any more: the
                    // driver Windows itself would pick, never a fixed input.inf (bug 5).
                    log::warn!("USB release of {node}: {} not usable ({e}); installing the best driver Windows ranks", original.describe());
                    d.set_class_guid(crate::usb_capture::HID_CLASS)?;
                    d.install_best()?
                }
            }
        };
        log::info!("USB release of {node}: installed {installed} (journaled {})", original.describe());
        // Before the extensions, so a failed reinstall leaves the node findable by the
        // start-up sweep, and a finished one is not ours any more.
        d.set_interface_guid(false)?;
        d.reapply_extensions(node, original);
        Ok(())
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

    fn still_captured(&self, node: &str) -> bool {
        match DevInfo::open(node) {
            Ok(d) => d.class_guid().is_some_and(|c| c == USB_DEVICE_CLASS) || d.has_interface_guid(),
            Err(e) => {
                log::debug!("USB capture: {node} not readable ({e}); not counted as captured");
                false
            }
        }
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
        assert!(!WinUsbSwap.still_captured(r"USB\VID_0000&PID_0000\NOPE"));
        assert!(WinUsbSwap.current(r"USB\VID_0000&PID_0000\NOPE").is_none());
    }

    /// Read-only: the driver of a present USB node reads like `pnputil` / Device Manager
    /// show it (an INF name, a section, a dotted version), and it is not "still captured".
    #[test]
    fn current_driver_of_a_usb_hub_reads() {
        use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
            CM_GETIDLIST_FILTER_ENUMERATOR, CM_GETIDLIST_FILTER_PRESENT, CM_Get_Device_ID_List_SizeW, CM_Get_Device_ID_ListW,
        };
        let filter = wide("USB");
        let flags = CM_GETIDLIST_FILTER_ENUMERATOR | CM_GETIDLIST_FILTER_PRESENT;
        let mut len = 0u32;
        // SAFETY: NUL-terminated filter, out length.
        if unsafe { CM_Get_Device_ID_List_SizeW(&mut len, filter.as_ptr(), flags) } != CR_SUCCESS || len < 2 {
            return; // no USB on this machine
        }
        let mut buf = vec![0u16; len as usize];
        // SAFETY: buffer of `len` units.
        if unsafe { CM_Get_Device_ID_ListW(filter.as_ptr(), buf.as_mut_ptr(), len, flags) } != CR_SUCCESS {
            return;
        }
        let ids: Vec<String> = buf.split(|c| *c == 0).filter(|s| !s.is_empty()).map(String::from_utf16_lossy).collect();
        let Some(d) = ids.iter().find_map(|id| {
            WinUsbSwap.current(id).filter(|d| !d.inf_path.is_empty() && !d.class_guid.eq_ignore_ascii_case(USB_DEVICE_CLASS)).map(|d| (id.clone(), d))
        }) else {
            return;
        };
        let (id, drv) = d;
        assert!(crate::usb_capture::is_bare_inf_name(&drv.inf_path), "{id}: {drv:?}");
        assert!(drv.driver_version.split('.').count() == 4, "{id}: {drv:?}");
        assert!(!WinUsbSwap.still_captured(&id), "{id}");
    }

    #[test]
    fn driver_versions_print_like_windows() {
        assert_eq!(version_string((1 << 48) | (10 << 32) | (80 << 16)), "1.10.80.0");
        assert_eq!(version_string((10 << 48) | (26100 << 16) | 9974), "10.0.26100.9974");
    }
}
