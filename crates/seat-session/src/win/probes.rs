// SPDX-License-Identifier: GPL-3.0-only
//! Real edition and RDS probes (read-only).

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
use windows_sys::Win32::System::Services::{
    CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceStatus, SC_MANAGER_CONNECT, SERVICE_QUERY_STATUS, SERVICE_RUNNING,
    SERVICE_STATUS,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, IsWow64Process2, OpenEventW, WaitForSingleObject};

use super::{expand_env, wide};
use crate::edition::{EditionInfo, EditionProbe, edition_from_registry};
use crate::error::SysError;
use crate::rds::{
    LicensingInfo, ListenerInfo, RdsProbe, TS, licensing_mode_from_registry, listener_from_registry, service_dll_from_registry,
};
use crate::registry::{Registry, get_u32};

/// `SYNCHRONIZE` access right.
const SYNCHRONIZE: u32 = 0x0010_0000;

pub struct WindowsEditionProbe {
    reg: Arc<dyn Registry>,
}

impl WindowsEditionProbe {
    pub fn new(reg: Arc<dyn Registry>) -> Self {
        Self { reg }
    }
}

/// Native machine of the OS (`IsWow64Process2`).
pub fn native_machine() -> u16 {
    let (mut process, mut native) = (0u16, 0u16);
    // SAFETY: pseudo handle of the current process; valid out pointers.
    if unsafe { IsWow64Process2(GetCurrentProcess(), &mut process, &mut native) } != 0 && native != 0 {
        return native;
    }
    if cfg!(target_arch = "aarch64") { crate::pe::IMAGE_FILE_MACHINE_ARM64 } else { crate::pe::IMAGE_FILE_MACHINE_AMD64 }
}

impl EditionProbe for WindowsEditionProbe {
    fn edition(&self) -> Result<EditionInfo, SysError> {
        let tv = expand_env(r"%SystemRoot%\System32\termsrv.dll").and_then(|p| super::files::file_version(Path::new(&p)));
        edition_from_registry(self.reg.as_ref(), tv)
    }

    /// `Terminal Server\TSAppCompat = 1` is set while the RD Session Host role (application
    /// server mode) is installed. HYPOTHESIS on current Server builds; the WMI
    /// `Win32_TerminalServiceSetting.TerminalServerMode` is the documented alternative.
    fn rdsh_role_installed(&self) -> Result<bool, SysError> {
        Ok(get_u32(self.reg.as_ref(), TS, "TSAppCompat")? == Some(1))
    }

    fn native_machine(&self) -> u16 {
        native_machine()
    }
}

/// Service is in the `SERVICE_RUNNING` state.
pub fn service_running(name: &str) -> bool {
    // SAFETY: plain SCM calls; handles closed below.
    unsafe {
        let scm = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT);
        if scm.is_null() {
            return false;
        }
        let n = wide(name);
        let svc = OpenServiceW(scm, n.as_ptr(), SERVICE_QUERY_STATUS);
        let mut st: SERVICE_STATUS = std::mem::zeroed();
        let running = !svc.is_null() && QueryServiceStatus(svc, &mut st) != 0 && st.dwCurrentState == SERVICE_RUNNING;
        if !svc.is_null() {
            CloseServiceHandle(svc);
        }
        CloseServiceHandle(scm);
        running
    }
}

pub struct WindowsRdsProbe {
    reg: Arc<dyn Registry>,
}

impl WindowsRdsProbe {
    pub fn new(reg: Arc<dyn Registry>) -> Self {
        Self { reg }
    }
}

impl RdsProbe for WindowsRdsProbe {
    fn listener(&self) -> Result<ListenerInfo, SysError> {
        listener_from_registry(self.reg.as_ref())
    }

    /// Grace days need the RDS WMI provider; not read here (`None`).
    fn licensing(&self) -> Result<LicensingInfo, SysError> {
        Ok(LicensingInfo { mode: licensing_mode_from_registry(self.reg.as_ref())?, grace_days_left: None })
    }

    fn service_dll(&self) -> Result<String, SysError> {
        service_dll_from_registry(self.reg.as_ref())
    }

    fn termservice_running(&self) -> bool {
        service_running("TermService")
    }

    /// Opens `Global\TermSrvReadyEvent` with SYNCHRONIZE only (never creates it) and waits;
    /// falls back to the service state when the event cannot be opened.
    fn wait_ready(&self, timeout: Duration) -> bool {
        let name = wide(r"Global\TermSrvReadyEvent");
        // SAFETY: plain open; closed below.
        let h = unsafe { OpenEventW(SYNCHRONIZE, 0, name.as_ptr()) };
        if h.is_null() {
            return self.termservice_running();
        }
        let ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
        // SAFETY: valid event handle.
        let r = unsafe { WaitForSingleObject(h, ms) };
        // SAFETY: opened above.
        unsafe { CloseHandle(h) };
        r == WAIT_OBJECT_0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::win::registry::WinRegistry;

    #[test]
    fn readonly_probes_on_this_box() {
        let p = WindowsEditionProbe::new(Arc::new(WinRegistry));
        let e = p.edition().unwrap();
        assert!(!e.edition_id.is_empty());
        let m = p.native_machine();
        assert!(m == crate::pe::IMAGE_FILE_MACHINE_ARM64 || m == crate::pe::IMAGE_FILE_MACHINE_AMD64);
    }
}
