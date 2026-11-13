// SPDX-License-Identifier: GPL-3.0-only
//! [`WinSystemIo`]: SetupAPI device security property (`SPDRP_SECURITY_SDS`, the SDDL form
//! of `SPDRP_SECURITY`) and characteristics, devnode restart (`DIF_PROPERTYCHANGE` /
//! `DICS_PROPCHANGE`), and the mount manager (volume names, global letters, lock +
//! dismount).
//!
//! **Untested on hardware.** Needs LocalSystem / administrator rights; the unit tests never
//! call any setter here.

use super::pnp::removal_relations;
use super::{from_wide, last_error, split_multi_sz, wide};
use crate::gate::{Invalidate, SecurityIo, VolumeIo, VolumeRef};
use seat_core::backend::{BackendError, BackendResult, ErrorKind};
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_GET_DEVICE_INTERFACE_LIST_PRESENT, CM_Get_Device_Interface_ListW, CM_Get_Device_Interface_List_SizeW, CR_BUFFER_SMALL,
    CR_SUCCESS, DI_NEEDREBOOT, DI_NEEDRESTART, DICS_FLAG_GLOBAL, DICS_PROPCHANGE, DIF_PROPERTYCHANGE, HDEVINFO,
    SP_CLASSINSTALL_HEADER, SP_DEVINFO_DATA, SP_DEVINSTALL_PARAMS_W, SP_PROPCHANGE_PARAMS, SPDRP_CHARACTERISTICS,
    SPDRP_SECURITY_SDS, SetupDiCallClassInstaller, SetupDiCreateDeviceInfoList, SetupDiDestroyDeviceInfoList,
    SetupDiGetClassRegistryPropertyW, SetupDiGetDeviceInstallParamsW, SetupDiGetDeviceRegistryPropertyW, SetupDiOpenDeviceInfoW,
    SetupDiSetClassInstallParamsW, SetupDiSetClassRegistryPropertyW, SetupDiSetDeviceRegistryPropertyW, SPCRP_SECURITY_SDS,
};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_INSUFFICIENT_BUFFER, ERROR_INVALID_DATA, ERROR_MORE_DATA, ERROR_SHARING_VIOLATION,
    GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, DeleteVolumeMountPointW, FILE_SHARE_READ, FILE_SHARE_WRITE, GetLogicalDrives, GetVolumeNameForVolumeMountPointW,
    GetVolumePathNamesForVolumeNameW, OPEN_EXISTING, SetVolumeMountPointW,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{FSCTL_DISMOUNT_VOLUME, FSCTL_LOCK_VOLUME};
use windows_sys::core::GUID;

/// `GUID_DEVINTERFACE_VOLUME` {53f5630d-b6bf-11d0-94f2-00a0c91efb8b}.
const GUID_DEVINTERFACE_VOLUME: GUID = GUID::from_u128(0x53f5630d_b6bf_11d0_94f2_00a0c91efb8b);

fn os(reason: &str, what: &str, code: u32) -> BackendError {
    let kind = if code == ERROR_ACCESS_DENIED { ErrorKind::Denied } else { ErrorKind::Os };
    BackendError::new(kind, reason, format!("{what}: Win32 {code}")).with_win32(code)
}

/// One devnode opened in a SetupAPI device information set.
struct DevInfo {
    set: HDEVINFO,
    data: SP_DEVINFO_DATA,
}

impl DevInfo {
    fn open(instance_id: &str) -> BackendResult<Self> {
        // SAFETY: null class GUID and no parent window are allowed.
        let set = unsafe { SetupDiCreateDeviceInfoList(std::ptr::null(), std::ptr::null_mut()) };
        if set == INVALID_HANDLE_VALUE as HDEVINFO {
            return Err(os("setupapi_failed", "SetupDiCreateDeviceInfoList", last_error()));
        }
        let mut data = SP_DEVINFO_DATA { cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32, ..Default::default() };
        let w = wide(instance_id);
        // SAFETY: valid set, NUL-terminated id, out struct with cbSize set.
        if unsafe { SetupDiOpenDeviceInfoW(set, w.as_ptr(), std::ptr::null_mut(), 0, &mut data) } == 0 {
            let e = last_error();
            // SAFETY: `set` is valid.
            unsafe { SetupDiDestroyDeviceInfoList(set) };
            return Err(BackendError::new(ErrorKind::NotFound, "device_not_found", format!("{instance_id}: Win32 {e}")).with_win32(e));
        }
        Ok(Self { set, data })
    }

    fn get(&self, prop: u32) -> BackendResult<Option<Vec<u8>>> {
        let mut ty = 0u32;
        let mut need = 0u32;
        // SAFETY: size query with a null buffer.
        let ok = unsafe { SetupDiGetDeviceRegistryPropertyW(self.set, &self.data, prop, &mut ty, std::ptr::null_mut(), 0, &mut need) };
        if ok == 0 {
            let e = last_error();
            if e == ERROR_INVALID_DATA {
                return Ok(None); // property not set
            }
            if e != ERROR_INSUFFICIENT_BUFFER {
                return Err(os("setupapi_failed", "SetupDiGetDeviceRegistryPropertyW", e));
            }
        }
        let mut buf = vec![0u8; need.max(4) as usize];
        // SAFETY: `buf` has `buf.len()` bytes.
        let ok = unsafe {
            SetupDiGetDeviceRegistryPropertyW(self.set, &self.data, prop, &mut ty, buf.as_mut_ptr(), buf.len() as u32, &mut need)
        };
        if ok == 0 {
            return Err(os("setupapi_failed", "SetupDiGetDeviceRegistryPropertyW", last_error()));
        }
        buf.truncate(need as usize);
        Ok(Some(buf))
    }

    fn set(&mut self, prop: u32, value: Option<&[u8]>) -> BackendResult<()> {
        let (p, n) = value.map_or((std::ptr::null(), 0), |v| (v.as_ptr(), v.len() as u32));
        // SAFETY: `p`/`n` describe a valid buffer, or null/0 to delete the property.
        if unsafe { SetupDiSetDeviceRegistryPropertyW(self.set, &mut self.data, prop, p, n) } == 0 {
            return Err(os("descriptor_write_failed", "SetupDiSetDeviceRegistryPropertyW", last_error()));
        }
        Ok(())
    }
}

impl Drop for DevInfo {
    fn drop(&mut self) {
        // SAFETY: `set` came from SetupDiCreateDeviceInfoList.
        unsafe { SetupDiDestroyDeviceInfoList(self.set) };
    }
}

fn utf16_bytes_to_string(b: &[u8]) -> String {
    let w: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    from_wide(&w)
}

/// Parse `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx` (braces optional).
pub(crate) fn parse_guid(s: &str) -> Option<GUID> {
    let h: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if h.len() != 32 || s.trim_matches(|c| c == '{' || c == '}').len() != 36 {
        return None;
    }
    u128::from_str_radix(&h, 16).ok().map(GUID::from_u128)
}

fn class_guid(s: &str) -> BackendResult<GUID> {
    parse_guid(s).ok_or_else(|| BackendError::new(ErrorKind::Policy, "invalid_class_guid", s.to_string()))
}

/// The production I/O of the gates.
pub struct WinSystemIo;

impl SecurityIo for WinSystemIo {
    fn get_sddl(&self, instance_id: &str) -> BackendResult<Option<String>> {
        let d = DevInfo::open(instance_id)?;
        Ok(d.get(SPDRP_SECURITY_SDS)?.map(|b| utf16_bytes_to_string(&b)).filter(|s| !s.is_empty()))
    }

    fn set_sddl(&self, instance_id: &str, sddl: Option<&str>) -> BackendResult<()> {
        if let Some(s) = sddl {
            // Never hand SetupAPI a string that is not a security descriptor (journal
            // records are restored from disk).
            if !super::acl::sddl_is_valid(s) {
                return Err(BackendError::new(ErrorKind::Policy, "invalid_sddl", "descriptor does not parse"));
            }
        }
        let mut d = DevInfo::open(instance_id)?;
        let bytes: Option<Vec<u8>> = sddl.map(|s| wide(s).iter().flat_map(|c| c.to_le_bytes()).collect());
        d.set(SPDRP_SECURITY_SDS, bytes.as_deref())
    }

    fn get_characteristics(&self, instance_id: &str) -> BackendResult<Option<u32>> {
        let d = DevInfo::open(instance_id)?;
        Ok(d.get(SPDRP_CHARACTERISTICS)?.filter(|b| b.len() >= 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])))
    }

    fn set_characteristics(&self, instance_id: &str, value: Option<u32>) -> BackendResult<()> {
        let mut d = DevInfo::open(instance_id)?;
        let b = value.map(u32::to_le_bytes);
        d.set(SPDRP_CHARACTERISTICS, b.as_ref().map(|b| b.as_slice()))
    }

    fn get_class_sddl(&self, class: &str) -> BackendResult<Option<String>> {
        let g = class_guid(class)?;
        let mut ty = 0u32;
        let mut need = 0u32;
        // SAFETY: size query with a null buffer; local machine (null name), reserved null.
        let ok = unsafe {
            SetupDiGetClassRegistryPropertyW(&g, SPCRP_SECURITY_SDS, &mut ty, std::ptr::null_mut(), 0, &mut need, std::ptr::null(), std::ptr::null())
        };
        if ok == 0 {
            let e = last_error();
            if e == ERROR_INVALID_DATA || e == windows_sys::Win32::Foundation::ERROR_FILE_NOT_FOUND {
                return Ok(None);
            }
            if e != ERROR_INSUFFICIENT_BUFFER {
                return Err(os("setupapi_failed", "SetupDiGetClassRegistryPropertyW", e));
            }
        }
        let mut buf = vec![0u8; need.max(4) as usize];
        // SAFETY: `buf` has `buf.len()` bytes.
        let ok = unsafe {
            SetupDiGetClassRegistryPropertyW(
                &g,
                SPCRP_SECURITY_SDS,
                &mut ty,
                buf.as_mut_ptr(),
                buf.len() as u32,
                &mut need,
                std::ptr::null(),
                std::ptr::null(),
            )
        };
        if ok == 0 {
            return Err(os("setupapi_failed", "SetupDiGetClassRegistryPropertyW", last_error()));
        }
        buf.truncate(need as usize);
        Ok(Some(utf16_bytes_to_string(&buf)).filter(|s| !s.is_empty()))
    }

    fn set_class_sddl(&self, class: &str, sddl: Option<&str>) -> BackendResult<()> {
        let g = class_guid(class)?;
        if let Some(s) = sddl {
            if !super::acl::sddl_is_valid(s) {
                return Err(BackendError::new(ErrorKind::Policy, "invalid_sddl", "descriptor does not parse"));
            }
        }
        let bytes: Option<Vec<u8>> = sddl.map(|s| wide(s).iter().flat_map(|c| c.to_le_bytes()).collect());
        let (p, n) = bytes.as_deref().map_or((std::ptr::null(), 0), |v| (v.as_ptr(), v.len() as u32));
        // SAFETY: `p`/`n` describe a valid buffer or null/0 (delete); local machine.
        if unsafe { SetupDiSetClassRegistryPropertyW(&g, SPCRP_SECURITY_SDS, p, n, std::ptr::null(), std::ptr::null()) } == 0 {
            return Err(os("descriptor_write_failed", "SetupDiSetClassRegistryPropertyW", last_error()));
        }
        Ok(())
    }

    fn restart(&self, instance_id: &str) -> BackendResult<Invalidate> {
        let d = DevInfo::open(instance_id)?;
        let params = SP_PROPCHANGE_PARAMS {
            ClassInstallHeader: SP_CLASSINSTALL_HEADER {
                cbSize: std::mem::size_of::<SP_CLASSINSTALL_HEADER>() as u32,
                InstallFunction: DIF_PROPERTYCHANGE,
            },
            StateChange: DICS_PROPCHANGE,
            Scope: DICS_FLAG_GLOBAL,
            HwProfile: 0,
        };
        // SAFETY: `params` starts with the class-install header, size passed is its full size.
        let ok = unsafe {
            SetupDiSetClassInstallParamsW(
                d.set,
                &d.data,
                &params.ClassInstallHeader,
                std::mem::size_of::<SP_PROPCHANGE_PARAMS>() as u32,
            )
        };
        if ok == 0 {
            return Err(os("restart_failed", "SetupDiSetClassInstallParamsW", last_error()));
        }
        // SAFETY: valid set + element.
        if unsafe { SetupDiCallClassInstaller(DIF_PROPERTYCHANGE, d.set, &d.data) } == 0 {
            let e = last_error();
            return Ok(Invalidate::Vetoed(format!("class installer: Win32 {e}")));
        }
        // SAFETY: zeroed POD with cbSize set.
        let mut ip: SP_DEVINSTALL_PARAMS_W = unsafe { std::mem::zeroed() };
        ip.cbSize = std::mem::size_of::<SP_DEVINSTALL_PARAMS_W>() as u32;
        // SAFETY: valid set, element and out struct.
        if unsafe { SetupDiGetDeviceInstallParamsW(d.set, &d.data, &mut ip) } != 0 && ip.Flags & (DI_NEEDREBOOT | DI_NEEDRESTART) != 0 {
            // The stop was vetoed (open handles); Windows finishes it at the next reboot.
            return Ok(Invalidate::Vetoed("needs_reboot".into()));
        }
        Ok(Invalidate::Done)
    }
}

/// Interface paths of class `guid` exposed by devnode `instance_id`.
pub(crate) fn interfaces_of(guid: &GUID, instance_id: &str) -> Vec<String> {
    let w = wide(instance_id);
    for _ in 0..3 {
        let mut len = 0u32;
        // SAFETY: valid GUID pointer, NUL-terminated device id, out pointer.
        let cr = unsafe { CM_Get_Device_Interface_List_SizeW(&mut len, guid, w.as_ptr(), CM_GET_DEVICE_INTERFACE_LIST_PRESENT) };
        if cr != CR_SUCCESS || len <= 1 {
            return Vec::new();
        }
        let mut buf = vec![0u16; len as usize];
        // SAFETY: `buf` holds `len` u16.
        let cr = unsafe { CM_Get_Device_Interface_ListW(guid, w.as_ptr(), buf.as_mut_ptr(), len, CM_GET_DEVICE_INTERFACE_LIST_PRESENT) };
        if cr == CR_BUFFER_SMALL {
            continue;
        }
        if cr != CR_SUCCESS {
            return Vec::new();
        }
        return split_multi_sz(&buf);
    }
    Vec::new()
}

/// `\\?\Volume{GUID}\` behind a volume interface path.
fn volume_name(interface_path: &str) -> Option<String> {
    let mut p = interface_path.to_string();
    if !p.ends_with('\\') {
        p.push('\\');
    }
    let w = wide(&p);
    let mut out = [0u16; 64];
    // SAFETY: NUL-terminated input, output buffer of 64 chars (a volume name is 49).
    (unsafe { GetVolumeNameForVolumeMountPointW(w.as_ptr(), out.as_mut_ptr(), out.len() as u32) } != 0).then(|| from_wide(&out))
}

fn letters_of(volume: &str) -> Vec<char> {
    let w = wide(volume);
    let mut buf = vec![0u16; 512];
    for _ in 0..3 {
        let mut need = 0u32;
        // SAFETY: NUL-terminated name, buffer of `buf.len()` chars.
        let ok = unsafe { GetVolumePathNamesForVolumeNameW(w.as_ptr(), buf.as_mut_ptr(), buf.len() as u32, &mut need) };
        if ok != 0 {
            return split_multi_sz(&buf)
                .into_iter()
                .filter_map(|p| {
                    let c: Vec<char> = p.chars().collect();
                    (c.len() == 3 && c[1] == ':' && c[0].is_ascii_alphabetic()).then(|| c[0].to_ascii_uppercase())
                })
                .collect();
        }
        if last_error() != ERROR_MORE_DATA {
            return Vec::new();
        }
        buf = vec![0u16; need as usize + 1];
    }
    Vec::new()
}

fn mount_point(letter: char) -> Vec<u16> {
    wide(&format!("{}:\\", letter.to_ascii_uppercase()))
}

impl VolumeIo for WinSystemIo {
    fn volumes_of(&self, disk_instance_id: &str) -> BackendResult<Vec<VolumeRef>> {
        let mut devs = removal_relations(disk_instance_id).unwrap_or_default();
        // Optical drives: the CD-ROM devnode itself carries the volume (HYPOTHESIS).
        devs.push(disk_instance_id.to_string());
        let mut out = Vec::new();
        for id in devs {
            for path in interfaces_of(&GUID_DEVINTERFACE_VOLUME, &id) {
                if let Some(name) = volume_name(&path) {
                    if !out.iter().any(|v: &VolumeRef| v.name.eq_ignore_ascii_case(&name)) {
                        out.push(VolumeRef { instance_id: id.clone(), letters: letters_of(&name), name });
                    }
                }
            }
        }
        Ok(out)
    }

    fn dismount(&self, volume_name: &str) -> BackendResult<Invalidate> {
        let w = wide(volume_name.trim_end_matches('\\'));
        // SAFETY: NUL-terminated path; no security attributes or template.
        let h = unsafe {
            CreateFileW(
                w.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        if h == INVALID_HANDLE_VALUE {
            return Err(os("volume_open_failed", "CreateFileW(volume)", last_error()));
        }
        let mut ret = 0u32;
        // SAFETY: valid handle; FSCTL_LOCK_VOLUME takes no buffers.
        let locked =
            unsafe { DeviceIoControl(h, FSCTL_LOCK_VOLUME, std::ptr::null(), 0, std::ptr::null_mut(), 0, &mut ret, std::ptr::null_mut()) };
        let result = if locked == 0 {
            let e = last_error();
            if e == ERROR_ACCESS_DENIED || e == ERROR_SHARING_VIOLATION {
                Ok(Invalidate::Vetoed(format!("volume in use (Win32 {e})")))
            } else {
                Err(os("volume_lock_failed", "FSCTL_LOCK_VOLUME", e))
            }
        } else {
            // SAFETY: valid, locked handle.
            let ok = unsafe {
                DeviceIoControl(h, FSCTL_DISMOUNT_VOLUME, std::ptr::null(), 0, std::ptr::null_mut(), 0, &mut ret, std::ptr::null_mut())
            };
            if ok == 0 { Err(os("volume_dismount_failed", "FSCTL_DISMOUNT_VOLUME", last_error())) } else { Ok(Invalidate::Done) }
        };
        // SAFETY: valid handle; closing also releases the lock.
        unsafe { CloseHandle(h) };
        result
    }

    fn remove_global_letter(&self, letter: char) -> BackendResult<()> {
        let w = mount_point(letter);
        // SAFETY: NUL-terminated `X:\`.
        if unsafe { DeleteVolumeMountPointW(w.as_ptr()) } == 0 {
            return Err(os("letter_remove_failed", "DeleteVolumeMountPointW", last_error()));
        }
        Ok(())
    }

    fn add_global_letter(&self, letter: char, volume_name: &str) -> BackendResult<()> {
        let (m, v) = (mount_point(letter), wide(volume_name));
        // SAFETY: NUL-terminated mount point and `\\?\Volume{GUID}\` name.
        if unsafe { SetVolumeMountPointW(m.as_ptr(), v.as_ptr()) } == 0 {
            return Err(os("letter_add_failed", "SetVolumeMountPointW", last_error()));
        }
        Ok(())
    }

    fn letter_in_use(&self, letter: char) -> bool {
        let u = letter.to_ascii_uppercase();
        if !u.is_ascii_uppercase() {
            return true;
        }
        // SAFETY: no arguments.
        let mask = unsafe { GetLogicalDrives() };
        mask & (1 << (u as u32 - 'A' as u32)) != 0
    }
}
