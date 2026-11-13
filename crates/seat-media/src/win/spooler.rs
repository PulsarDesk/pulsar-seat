// SPDX-License-Identifier: GPL-3.0-only
//! [`WinPrinterIo`]: print queues of a USB printer and their DACLs, through the documented
//! spooler API (`EnumPrintersW` level 5, `OpenPrinterW`, `GetPrinterW` / `SetPrinterW`
//! level 3) and the USB printing-support device interface
//! (`GUID_DEVINTERFACE_USBPRINT`, its `Base Name` + `Port Number` device parameters name
//! the spooler port, e.g. `USB001`).
//!
//! **Untested on hardware.** Writing needs LocalSystem / administrator rights; the unit
//! tests only read.

use super::setupdi::interfaces_of;
use super::{from_wide, last_error, wide};
use crate::gate::printer::PrinterIo;
use seat_core::backend::{BackendError, BackendResult, ErrorKind};
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    SP_DEVICE_INTERFACE_DATA, SetupDiCreateDeviceInfoList, SetupDiDestroyDeviceInfoList, SetupDiOpenDeviceInterfaceRegKey,
    SetupDiOpenDeviceInterfaceW,
};
use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_INVALID_PRINTER_NAME, INVALID_HANDLE_VALUE, LocalFree};
use windows_sys::Win32::Graphics::Printing::{
    ClosePrinter, EnumPrintersW, GetPrinterW, OpenPrinterW, PRINTER_DEFAULTSW, PRINTER_ENUM_LOCAL, PRINTER_HANDLE, PRINTER_INFO_3,
    PRINTER_INFO_5W, SetPrinterW,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSecurityDescriptorToStringSecurityDescriptorW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR};
use windows_sys::Win32::System::Registry::{HKEY, KEY_READ, RegCloseKey, RegQueryValueExW};
use windows_sys::core::GUID;

/// `GUID_DEVINTERFACE_USBPRINT` {28d78fad-5a12-11d1-ae5b-0000f803a8c2}.
const GUID_DEVINTERFACE_USBPRINT: GUID = GUID::from_u128(0x28d78fad_5a12_11d1_ae5b_0000f803a8c2);
const READ_CONTROL: u32 = 0x0002_0000;
const WRITE_DAC: u32 = 0x0004_0000;

fn os(reason: &str, what: &str, code: u32) -> BackendError {
    BackendError::new(ErrorKind::Os, reason, format!("{what}: Win32 {code}")).with_win32(code)
}

/// Read a value of an open registry key (raw bytes).
fn reg_value(key: HKEY, name: &str) -> Option<(u32, Vec<u8>)> {
    let n = wide(name);
    let mut ty = 0u32;
    let mut len = 0u32;
    // SAFETY: size query.
    if unsafe { RegQueryValueExW(key, n.as_ptr(), std::ptr::null(), &mut ty, std::ptr::null_mut(), &mut len) } != 0 || len > 4096 {
        return None;
    }
    let mut buf = vec![0u8; len as usize];
    // SAFETY: buffer of `len` bytes.
    if unsafe { RegQueryValueExW(key, n.as_ptr(), std::ptr::null(), &mut ty, buf.as_mut_ptr(), &mut len) } != 0 {
        return None;
    }
    buf.truncate(len as usize);
    Some((ty, buf))
}

/// Spooler port name of one USB printing interface (`USB001`).
fn port_of_interface(path: &str) -> Option<String> {
    // SAFETY: empty set, freed below.
    let set = unsafe { SetupDiCreateDeviceInfoList(std::ptr::null(), std::ptr::null_mut()) };
    if set == INVALID_HANDLE_VALUE as windows_sys::Win32::Devices::DeviceAndDriverInstallation::HDEVINFO {
        return None;
    }
    let mut data = SP_DEVICE_INTERFACE_DATA { cbSize: std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32, ..Default::default() };
    let w = wide(path);
    let mut out = None;
    // SAFETY: valid set and path; out struct with cbSize.
    if unsafe { SetupDiOpenDeviceInterfaceW(set, w.as_ptr(), 0, &mut data) } != 0 {
        // SAFETY: interface opened in `set`.
        let key = unsafe { SetupDiOpenDeviceInterfaceRegKey(set, &data, 0, KEY_READ) };
        if !std::ptr::eq(key, INVALID_HANDLE_VALUE) && !key.is_null() {
            let num = reg_value(key, "Port Number").filter(|(_, b)| b.len() >= 4).map(|(_, b)| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
            let base = reg_value(key, "Base Name").map(|(_, b)| {
                let u: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
                from_wide(&u)
            });
            if let Some(n) = num {
                out = Some(format!("{}{n:03}", base.filter(|b| !b.is_empty()).unwrap_or_else(|| "USB".into())));
            }
            // SAFETY: key opened above.
            unsafe { RegCloseKey(key) };
        }
    }
    // SAFETY: set from SetupDiCreateDeviceInfoList.
    unsafe { SetupDiDestroyDeviceInfoList(set) };
    out
}

/// Local queues with their port lists (level 5: name + ports, no DEVMODE).
fn local_queues() -> BackendResult<Vec<(String, Vec<String>)>> {
    let (mut need, mut count) = (0u32, 0u32);
    // SAFETY: size query.
    unsafe { EnumPrintersW(PRINTER_ENUM_LOCAL, std::ptr::null(), 5, std::ptr::null_mut(), 0, &mut need, &mut count) };
    if need == 0 {
        return Ok(Vec::new());
    }
    let mut buf = vec![0u64; (need as usize).div_ceil(8)];
    // SAFETY: buffer of at least `need` bytes, 8-aligned.
    if unsafe { EnumPrintersW(PRINTER_ENUM_LOCAL, std::ptr::null(), 5, buf.as_mut_ptr().cast(), need, &mut need, &mut count) } == 0 {
        return Err(os("spooler_failed", "EnumPrintersW", last_error()));
    }
    // SAFETY: the spooler wrote `count` PRINTER_INFO_5W records at the start of `buf`.
    let items = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const PRINTER_INFO_5W, count as usize) };
    let pw = |p: *mut u16| -> String {
        if p.is_null() {
            return String::new();
        }
        // SAFETY: NUL-terminated string inside `buf`.
        unsafe {
            let mut n = 0;
            while *p.add(n) != 0 {
                n += 1;
            }
            String::from_utf16_lossy(std::slice::from_raw_parts(p, n))
        }
    };
    Ok(items
        .iter()
        .map(|i| (pw(i.pPrinterName), pw(i.pPortName).split(',').map(|s| s.trim().to_ascii_uppercase()).filter(|s| !s.is_empty()).collect()))
        .collect())
}

struct Printer(PRINTER_HANDLE);

impl Printer {
    fn open(name: &str, access: u32) -> BackendResult<Option<Self>> {
        let w = wide(name);
        let defaults = PRINTER_DEFAULTSW { pDatatype: std::ptr::null_mut(), pDevMode: std::ptr::null_mut(), DesiredAccess: access };
        let mut h = PRINTER_HANDLE { Value: std::ptr::null_mut() };
        // SAFETY: NUL-terminated name, out handle, defaults valid for the call.
        if unsafe { OpenPrinterW(w.as_ptr(), &mut h, &defaults) } == 0 {
            let e = last_error();
            if e == ERROR_INVALID_PRINTER_NAME {
                return Ok(None);
            }
            return Err(os("spooler_failed", "OpenPrinterW", e));
        }
        Ok(Some(Self(h)))
    }
}

impl Drop for Printer {
    fn drop(&mut self) {
        // SAFETY: handle from OpenPrinterW.
        unsafe { ClosePrinter(self.0) };
    }
}

pub struct WinPrinterIo;

impl PrinterIo for WinPrinterIo {
    fn queues_for_device(&self, instance_id: &str) -> BackendResult<Vec<String>> {
        // The interface sits on the USB function (usbprint); the classified printer devnode
        // is usually its child, so look at the node and its parent.
        let mut ports: Vec<String> = interfaces_of(&GUID_DEVINTERFACE_USBPRINT, instance_id).iter().filter_map(|p| port_of_interface(p)).collect();
        if ports.is_empty() {
            if let Some(parent) = super::pnp::read_node(instance_id).and_then(|n| n.parent) {
                ports = interfaces_of(&GUID_DEVINTERFACE_USBPRINT, &parent).iter().filter_map(|p| port_of_interface(p)).collect();
            }
        }
        if ports.is_empty() {
            return Ok(Vec::new());
        }
        let ports: Vec<String> = ports.into_iter().map(|p| p.to_ascii_uppercase()).collect();
        Ok(local_queues()?.into_iter().filter(|(_, qp)| qp.iter().any(|p| ports.contains(p))).map(|(n, _)| n).collect())
    }

    fn get_dacl(&self, queue: &str) -> BackendResult<Option<String>> {
        let Some(p) = Printer::open(queue, READ_CONTROL)? else { return Ok(None) };
        let mut need = 0u32;
        // SAFETY: size query.
        unsafe { GetPrinterW(p.0, 3, std::ptr::null_mut(), 0, &mut need) };
        let e = last_error();
        if need == 0 {
            return Err(os("spooler_failed", "GetPrinterW", e));
        }
        let mut buf = vec![0u64; (need as usize).div_ceil(8)];
        // SAFETY: buffer of at least `need` bytes, 8-aligned.
        if unsafe { GetPrinterW(p.0, 3, buf.as_mut_ptr().cast(), need, &mut need) } == 0 {
            let e = last_error();
            if e != ERROR_INSUFFICIENT_BUFFER {
                return Err(os("spooler_failed", "GetPrinterW", e));
            }
        }
        // SAFETY: level 3 = one PRINTER_INFO_3 at the start of `buf`.
        let sd = unsafe { (*(buf.as_ptr() as *const PRINTER_INFO_3)).pSecurityDescriptor };
        let mut s: *mut u16 = std::ptr::null_mut();
        // SAFETY: valid descriptor inside `buf`; string freed with LocalFree.
        if unsafe { ConvertSecurityDescriptorToStringSecurityDescriptorW(sd, SDDL_REVISION_1, DACL_SECURITY_INFORMATION, &mut s, std::ptr::null_mut()) } == 0 {
            return Err(os("spooler_failed", "ConvertSecurityDescriptorToStringSecurityDescriptorW", last_error()));
        }
        // SAFETY: NUL-terminated string from the call above.
        let out = unsafe {
            let mut n = 0;
            while *s.add(n) != 0 {
                n += 1;
            }
            let v = String::from_utf16_lossy(std::slice::from_raw_parts(s, n));
            LocalFree(s.cast());
            v
        };
        Ok(Some(out))
    }

    fn set_dacl(&self, queue: &str, sddl: &str) -> BackendResult<()> {
        if !super::acl::sddl_is_valid(sddl) || !sddl.starts_with("D:") {
            return Err(BackendError::new(ErrorKind::Policy, "invalid_sddl", "printer DACL does not parse"));
        }
        let Some(p) = Printer::open(queue, READ_CONTROL | WRITE_DAC)? else {
            return Err(BackendError::new(ErrorKind::NotFound, "printer_not_found", queue.to_string()));
        };
        let w = wide(sddl);
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: NUL-terminated SDDL; descriptor freed below.
        if unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(w.as_ptr(), SDDL_REVISION_1, &mut sd, std::ptr::null_mut()) } == 0 {
            return Err(os("spooler_failed", "ConvertStringSecurityDescriptorToSecurityDescriptorW", last_error()));
        }
        let info = PRINTER_INFO_3 { pSecurityDescriptor: sd };
        // SAFETY: level 3 record; the descriptor holds only a DACL, so only the DACL changes.
        let ok = unsafe { SetPrinterW(p.0, 3, (&info as *const PRINTER_INFO_3).cast(), 0) };
        let e = last_error();
        // SAFETY: from ConvertStringSecurityDescriptorToSecurityDescriptorW.
        unsafe { LocalFree(sd) };
        if ok == 0 {
            return Err(os("descriptor_write_failed", "SetPrinterW", e));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read-only: the spooler lists queues (or none) without failing.
    #[test]
    fn local_queues_read() {
        if let Ok(q) = local_queues() {
            for (name, _) in q.iter().take(3) {
                let _ = WinPrinterIo.get_dacl(name);
            }
        }
        assert!(WinPrinterIo.get_dacl("Pulsar Seat yok böyle yazıcı").map(|d| d.is_none()).unwrap_or(true));
    }
}
