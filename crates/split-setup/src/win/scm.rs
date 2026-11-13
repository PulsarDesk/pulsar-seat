// SPDX-License-Identifier: GPL-3.0-only
//! SCM registration of our own service for the NSIS path (the MSI uses `ServiceInstall`).
//! Only ever touches the service named in the spec (`PulsarSplit`).

use super::wide;
use crate::error::SetupError;
use crate::system::{ServiceRegistrar, ServiceSpec};
use std::ffi::c_void;
use windows_sys::Win32::Foundation::{ERROR_SERVICE_DOES_NOT_EXIST, GetLastError};
use windows_sys::Win32::System::Services::{
    ChangeServiceConfig2W, ChangeServiceConfigW, CloseServiceHandle, ControlService, CreateServiceW, DeleteService,
    OpenSCManagerW, OpenServiceW, QueryServiceStatus, SC_ACTION, SC_ACTION_RESTART, SC_HANDLE, SC_MANAGER_ALL_ACCESS,
    SERVICE_ALL_ACCESS, SERVICE_CONFIG_DESCRIPTION, SERVICE_CONFIG_FAILURE_ACTIONS, SERVICE_CONFIG_REQUIRED_PRIVILEGES_INFO,
    SERVICE_CONFIG_SERVICE_SID_INFO, SERVICE_CONTROL_STOP, SERVICE_DEMAND_START, SERVICE_DESCRIPTIONW, SERVICE_ERROR_NORMAL,
    SERVICE_FAILURE_ACTIONSW, SERVICE_NO_CHANGE, SERVICE_REQUIRED_PRIVILEGES_INFOW, SERVICE_SID_INFO,
    SERVICE_SID_TYPE_UNRESTRICTED, SERVICE_STATUS, SERVICE_STOPPED, SERVICE_WIN32_OWN_PROCESS,
};

#[derive(Debug, Default, Clone, Copy)]
pub struct WinServices;

struct Sc(SC_HANDLE);

impl Drop for Sc {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: handle from OpenSCManagerW / OpenServiceW / CreateServiceW.
            unsafe { CloseServiceHandle(self.0) };
        }
    }
}

fn last(what: &str) -> SetupError {
    // SAFETY: trivial.
    let code = unsafe { GetLastError() };
    SetupError::Other(format!("{what} failed (win32 error {code})"))
}

fn scm() -> Result<Sc, SetupError> {
    // SAFETY: plain SCM open (needs elevation).
    let h = unsafe { OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_ALL_ACCESS) };
    if h.is_null() { Err(last("OpenSCManagerW")) } else { Ok(Sc(h)) }
}

fn multi_sz(items: &[String]) -> Vec<u16> {
    let mut v = Vec::new();
    for i in items {
        v.extend(i.encode_utf16());
        v.push(0);
    }
    v.push(0);
    v
}

impl ServiceRegistrar for WinServices {
    fn ensure(&self, spec: &ServiceSpec) -> Result<bool, SetupError> {
        let m = scm()?;
        let name = wide(&spec.name);
        let display = wide(&spec.display_name);
        let cmd = wide(&spec.command_line);
        // SAFETY: SCM calls with NUL-terminated buffers that outlive each call.
        unsafe {
            let mut svc = Sc(OpenServiceW(m.0, name.as_ptr(), SERVICE_ALL_ACCESS));
            let created = svc.0.is_null();
            if created {
                svc = Sc(CreateServiceW(
                    m.0,
                    name.as_ptr(),
                    display.as_ptr(),
                    SERVICE_ALL_ACCESS,
                    SERVICE_WIN32_OWN_PROCESS,
                    SERVICE_DEMAND_START,
                    SERVICE_ERROR_NORMAL,
                    cmd.as_ptr(),
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    std::ptr::null(), // LocalSystem
                    std::ptr::null(),
                ));
                if svc.0.is_null() {
                    return Err(last("CreateServiceW"));
                }
            } else if ChangeServiceConfigW(
                svc.0,
                SERVICE_WIN32_OWN_PROCESS,
                SERVICE_NO_CHANGE, // keep the operator's start type
                SERVICE_ERROR_NORMAL,
                cmd.as_ptr(),
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                display.as_ptr(),
            ) == 0
            {
                return Err(last("ChangeServiceConfigW"));
            }
            let mut desc_w = wide(&spec.description);
            let desc = SERVICE_DESCRIPTIONW { lpDescription: desc_w.as_mut_ptr() };
            if ChangeServiceConfig2W(svc.0, SERVICE_CONFIG_DESCRIPTION, &desc as *const _ as *const c_void) == 0 {
                return Err(last("SERVICE_CONFIG_DESCRIPTION"));
            }
            let sid = SERVICE_SID_INFO { dwServiceSidType: SERVICE_SID_TYPE_UNRESTRICTED };
            if ChangeServiceConfig2W(svc.0, SERVICE_CONFIG_SERVICE_SID_INFO, &sid as *const _ as *const c_void) == 0 {
                return Err(last("SERVICE_CONFIG_SERVICE_SID_INFO"));
            }
            let mut actions: Vec<SC_ACTION> =
                spec.restart_delays_ms.iter().map(|&d| SC_ACTION { Type: SC_ACTION_RESTART, Delay: d }).collect();
            let fa = SERVICE_FAILURE_ACTIONSW {
                dwResetPeriod: spec.reset_period_s,
                lpRebootMsg: std::ptr::null_mut(),
                lpCommand: std::ptr::null_mut(),
                cActions: actions.len() as u32,
                lpsaActions: actions.as_mut_ptr(),
            };
            if ChangeServiceConfig2W(svc.0, SERVICE_CONFIG_FAILURE_ACTIONS, &fa as *const _ as *const c_void) == 0 {
                return Err(last("SERVICE_CONFIG_FAILURE_ACTIONS"));
            }
            if !spec.required_privileges.is_empty() {
                let mut privs = multi_sz(&spec.required_privileges);
                let rp = SERVICE_REQUIRED_PRIVILEGES_INFOW { pmszRequiredPrivileges: privs.as_mut_ptr() };
                if ChangeServiceConfig2W(svc.0, SERVICE_CONFIG_REQUIRED_PRIVILEGES_INFO, &rp as *const _ as *const c_void) == 0 {
                    return Err(last("SERVICE_CONFIG_REQUIRED_PRIVILEGES_INFO"));
                }
            }
            Ok(created)
        }
    }

    fn remove(&self, name: &str, timeout_ms: u32) -> Result<(), SetupError> {
        let m = scm()?;
        let wname = wide(name);
        // SAFETY: SCM calls on an opened handle; polling with a bounded wait.
        unsafe {
            let svc = Sc(OpenServiceW(m.0, wname.as_ptr(), SERVICE_ALL_ACCESS));
            if svc.0.is_null() {
                return if GetLastError() == ERROR_SERVICE_DOES_NOT_EXIST { Ok(()) } else { Err(last("OpenServiceW")) };
            }
            let mut st: SERVICE_STATUS = std::mem::zeroed();
            ControlService(svc.0, SERVICE_CONTROL_STOP, &mut st);
            let mut waited = 0;
            while QueryServiceStatus(svc.0, &mut st) != 0 && st.dwCurrentState != SERVICE_STOPPED && waited < timeout_ms {
                std::thread::sleep(std::time::Duration::from_millis(250));
                waited += 250;
            }
            if DeleteService(svc.0) == 0 {
                return Err(last("DeleteService"));
            }
        }
        Ok(())
    }
}
