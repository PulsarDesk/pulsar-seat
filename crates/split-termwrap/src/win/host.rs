// SPDX-License-Identifier: GPL-3.0-only
//! Host facts: edition (registry), reference product presence (spec 90 §8.1 signals 1–2),
//! native machine (IsWow64Process2), system root.

use windows_sys::Win32::Foundation::ERROR_SERVICE_DOES_NOT_EXIST;
use windows_sys::Win32::System::Services::{CloseServiceHandle, OpenSCManagerW, OpenServiceW, SC_MANAGER_CONNECT, SERVICE_QUERY_STATUS};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, IsWow64Process2};

use super::registry::WinRegistry;
use super::{last_error, wide};
use crate::journal::RegValue;
use crate::system::{HostEdition, HostFacts, Registry, classify_edition};
use crate::{TermwrapError, pe};

const CURRENT_VERSION: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
const PRODUCT_OPTIONS: &str = r"SYSTEM\CurrentControlSet\Control\ProductOptions";
/// Spec 90 §8.1 signal 1 (ARP key) and signal 2 (service name). Interop identifiers only.
const REFERENCE_UNINSTALL_KEY: &str =
    r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\{DB5AA12E-1299-4244-8651-F363406E89F6}_is1";
const REFERENCE_SERVICE: &str = "DuoService";

pub struct WinHostFacts;

fn string(path: &str, name: &str) -> Result<String, TermwrapError> {
    Ok(match WinRegistry.get(path, name)? {
        RegValue::Sz(s) | RegValue::ExpandSz(s) => s,
        _ => String::new(),
    })
}

fn service_exists(name: &str) -> bool {
    // SAFETY: plain SCM calls; handles closed below.
    unsafe {
        let scm = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT);
        if scm.is_null() {
            // Cannot tell: report present so we fail closed.
            return true;
        }
        let n = wide(name);
        let svc = OpenServiceW(scm, n.as_ptr(), SERVICE_QUERY_STATUS);
        let exists = if svc.is_null() { last_error() != ERROR_SERVICE_DOES_NOT_EXIST } else { true };
        if !svc.is_null() {
            CloseServiceHandle(svc);
        }
        CloseServiceHandle(scm);
        exists
    }
}

fn uninstall_key_exists(path: &str) -> bool {
    use winreg::RegKey;
    use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY};
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    // Inno Setup ARP keys may sit in either view.
    [KEY_WOW64_64KEY, KEY_WOW64_32KEY].iter().any(|view| match hklm.open_subkey_with_flags(path, KEY_READ | view) {
        Ok(_) => true,
        Err(e) => e.kind() != std::io::ErrorKind::NotFound,
    })
}

impl HostFacts for WinHostFacts {
    fn edition(&self) -> Result<HostEdition, TermwrapError> {
        Ok(classify_edition(&string(CURRENT_VERSION, "EditionID")?, &string(PRODUCT_OPTIONS, "ProductType")?))
    }

    fn reference_product_present(&self) -> bool {
        uninstall_key_exists(REFERENCE_UNINSTALL_KEY) || service_exists(REFERENCE_SERVICE)
    }

    fn native_machine(&self) -> u16 {
        let (mut process, mut native) = (0u16, 0u16);
        // SAFETY: pseudo handle of the current process; valid out pointers.
        if unsafe { IsWow64Process2(GetCurrentProcess(), &mut process, &mut native) } != 0 && native != 0 {
            return native;
        }
        if cfg!(target_arch = "aarch64") { pe::IMAGE_FILE_MACHINE_ARM64 } else { pe::IMAGE_FILE_MACHINE_AMD64 }
    }

    fn system_root(&self) -> Option<String> {
        std::env::var("SystemRoot").ok().or_else(|| super::expand_env("%SystemRoot%"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_only_host_probes() {
        // No registry or SCM access in tests (contracts §0.3).
        let h = WinHostFacts;
        let m = h.native_machine();
        assert!(m == pe::IMAGE_FILE_MACHINE_AMD64 || m == pe::IMAGE_FILE_MACHINE_ARM64, "{m:#x}");
        assert!(h.system_root().is_some());
    }
}
