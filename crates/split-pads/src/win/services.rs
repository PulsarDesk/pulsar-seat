// SPDX-License-Identifier: GPL-3.0-only
//! SCM access: per-user service instances (Bluetooth plan), driver-service facts and
//! file versions for backend probes.

use windows_sys::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW, VS_FIXEDFILEINFO};
use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows_sys::Win32::System::Services::{
    ChangeServiceConfigW, CloseServiceHandle, ControlService, EnumServicesStatusExW, OpenSCManagerW, OpenServiceW,
    QueryServiceConfigW, QueryServiceStatusEx, ENUM_SERVICE_STATUS_PROCESSW, QUERY_SERVICE_CONFIGW, SC_ENUM_PROCESS_INFO,
    SC_HANDLE, SC_MANAGER_CONNECT, SC_MANAGER_ENUMERATE_SERVICE, SC_STATUS_PROCESS_INFO, SERVICE_CHANGE_CONFIG,
    SERVICE_CONTROL_STOP, SERVICE_NO_CHANGE, SERVICE_QUERY_CONFIG, SERVICE_QUERY_STATUS, SERVICE_RUNNING,
    SERVICE_STATE_ALL, SERVICE_STATUS, SERVICE_STATUS_PROCESS, SERVICE_STOP, SERVICE_WIN32,
};

use super::{from_wide, last_error, wide};

/// winnt.h service type bits (not in this windows-sys module).
const SERVICE_USER_SERVICE: u32 = 0x40;
const SERVICE_USERSERVICE_INSTANCE: u32 = 0x80;
use crate::bluetooth::{StartType, UserServiceInstance, UserServices};
use crate::error::PadError;

struct Sc(SC_HANDLE);
impl Drop for Sc {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: owned SCM handle.
            unsafe { CloseServiceHandle(self.0) };
        }
    }
}

fn scm(access: u32) -> Result<Sc, PadError> {
    // SAFETY: local SCM, default database.
    let h = unsafe { OpenSCManagerW(std::ptr::null(), std::ptr::null(), access) };
    if h.is_null() {
        return Err(PadError::os("OpenSCManagerW", last_error()));
    }
    Ok(Sc(h))
}

fn open(scm: &Sc, name: &str, access: u32) -> Result<Sc, PadError> {
    let w = wide(name);
    // SAFETY: valid SCM handle and name.
    let h = unsafe { OpenServiceW(scm.0, w.as_ptr(), access) };
    if h.is_null() {
        return Err(PadError::os("OpenServiceW", last_error()));
    }
    Ok(Sc(h))
}

fn query_config(svc: &Sc) -> Result<(u32, String), PadError> {
    let mut need = 0u32;
    // SAFETY: size query.
    unsafe { QueryServiceConfigW(svc.0, std::ptr::null_mut(), 0, &mut need) };
    let mut buf = vec![0u64; (need as usize).div_ceil(8).max(1)];
    let p = buf.as_mut_ptr() as *mut QUERY_SERVICE_CONFIGW;
    // SAFETY: buffer of `need` bytes, 8-byte aligned.
    if unsafe { QueryServiceConfigW(svc.0, p, (buf.len() * 8) as u32, &mut need) } == 0 {
        return Err(PadError::os("QueryServiceConfigW", last_error()));
    }
    // SAFETY: filled by the call.
    let cfg = unsafe { &*p };
    let path = if cfg.lpBinaryPathName.is_null() {
        String::new()
    } else {
        // SAFETY: NUL-terminated string inside buf.
        unsafe { from_wide(std::slice::from_raw_parts(cfg.lpBinaryPathName, wcslen(cfg.lpBinaryPathName) + 1)) }
    };
    Ok((cfg.dwStartType, path))
}

unsafe fn wcslen(p: *const u16) -> usize {
    let mut n = 0;
    // SAFETY: caller passes a NUL-terminated string.
    while unsafe { *p.add(n) } != 0 {
        n += 1;
    }
    n
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServiceFacts {
    pub exists: bool,
    pub running: bool,
    /// Image path with `\SystemRoot\` / `System32\` prefixes expanded.
    pub image_path: Option<String>,
}

fn expand_image(p: &str) -> String {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
    let t = p.trim().trim_matches('"');
    let lower = t.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("\\systemroot\\") {
        format!("{root}\\{}", &t[t.len() - rest.len()..])
    } else if lower.starts_with("system32\\") {
        format!("{root}\\{t}")
    } else if let Some(rest) = lower.strip_prefix("\\??\\") {
        t[t.len() - rest.len()..].to_string()
    } else {
        t.to_string()
    }
}

/// Read-only facts about a (driver) service.
pub fn service_facts(name: &str) -> ServiceFacts {
    let Ok(m) = scm(SC_MANAGER_CONNECT) else { return ServiceFacts::default() };
    let Ok(s) = open(&m, name, SERVICE_QUERY_STATUS | SERVICE_QUERY_CONFIG) else { return ServiceFacts::default() };
    let mut st = SERVICE_STATUS_PROCESS::default();
    let mut need = 0u32;
    // SAFETY: buffer is one SERVICE_STATUS_PROCESS.
    let ok = unsafe {
        QueryServiceStatusEx(
            s.0,
            SC_STATUS_PROCESS_INFO,
            &mut st as *mut _ as *mut u8,
            std::mem::size_of::<SERVICE_STATUS_PROCESS>() as u32,
            &mut need,
        )
    };
    let image_path = query_config(&s).ok().map(|(_, p)| expand_image(&p)).filter(|p| !p.is_empty());
    ServiceFacts { exists: true, running: ok != 0 && st.dwCurrentState == SERVICE_RUNNING, image_path }
}

/// `a.b.c.d` from `VS_FIXEDFILEINFO` (never the string resource).
pub fn file_version(path: &str) -> Option<String> {
    let w = wide(path);
    let mut h = 0u32;
    // SAFETY: valid path.
    let n = unsafe { GetFileVersionInfoSizeW(w.as_ptr(), &mut h) };
    if n == 0 {
        return None;
    }
    let mut buf = vec![0u8; n as usize];
    // SAFETY: buffer of n bytes.
    if unsafe { GetFileVersionInfoW(w.as_ptr(), 0, n, buf.as_mut_ptr() as *mut _) } == 0 {
        return None;
    }
    let root = wide("\\");
    let mut p: *mut core::ffi::c_void = std::ptr::null_mut();
    let mut len = 0u32;
    // SAFETY: buf holds the version resource.
    if unsafe { VerQueryValueW(buf.as_ptr() as *const _, root.as_ptr(), &mut p, &mut len) } == 0
        || (len as usize) < std::mem::size_of::<VS_FIXEDFILEINFO>()
    {
        return None;
    }
    // SAFETY: points into buf, size checked.
    let f = unsafe { &*(p as *const VS_FIXEDFILEINFO) };
    Some(format!(
        "{}.{}.{}.{}",
        f.dwFileVersionMS >> 16,
        f.dwFileVersionMS & 0xFFFF,
        f.dwFileVersionLS >> 16,
        f.dwFileVersionLS & 0xFFFF
    ))
}

pub struct WinUserServices;

impl UserServices for WinUserServices {
    fn instances(&self) -> Result<Vec<UserServiceInstance>, PadError> {
        let m = scm(SC_MANAGER_ENUMERATE_SERVICE | SC_MANAGER_CONNECT)?;
        let kinds = SERVICE_WIN32 | SERVICE_USER_SERVICE | SERVICE_USERSERVICE_INSTANCE;
        let mut need = 0u32;
        let mut count = 0u32;
        let mut resume = 0u32;
        // SAFETY: size query.
        unsafe {
            EnumServicesStatusExW(
                m.0,
                SC_ENUM_PROCESS_INFO,
                kinds,
                SERVICE_STATE_ALL,
                std::ptr::null_mut(),
                0,
                &mut need,
                &mut count,
                &mut resume,
                std::ptr::null(),
            )
        };
        let mut buf = vec![0u64; (need as usize).div_ceil(8) + 64];
        resume = 0;
        // SAFETY: buffer large enough (size from the first call plus margin).
        let ok = unsafe {
            EnumServicesStatusExW(
                m.0,
                SC_ENUM_PROCESS_INFO,
                kinds,
                SERVICE_STATE_ALL,
                buf.as_mut_ptr() as *mut u8,
                (buf.len() * 8) as u32,
                &mut need,
                &mut count,
                &mut resume,
                std::ptr::null(),
            )
        };
        if ok == 0 {
            return Err(PadError::os("EnumServicesStatusExW", last_error()));
        }
        let items = buf.as_ptr() as *const ENUM_SERVICE_STATUS_PROCESSW;
        let mut out = Vec::new();
        for i in 0..count as usize {
            // SAFETY: `count` entries were written.
            let e = unsafe { &*items.add(i) };
            let st = &e.ServiceStatusProcess;
            if st.dwServiceType & SERVICE_USERSERVICE_INSTANCE == 0 {
                continue;
            }
            // SAFETY: NUL-terminated name inside buf.
            let name = unsafe { from_wide(std::slice::from_raw_parts(e.lpServiceName, wcslen(e.lpServiceName) + 1)) };
            let running = st.dwCurrentState == SERVICE_RUNNING;
            let pid = (st.dwProcessId != 0).then_some(st.dwProcessId);
            let session_id = pid.and_then(|p| {
                let mut s = 0u32;
                // SAFETY: out pointer valid.
                (unsafe { ProcessIdToSessionId(p, &mut s) } != 0).then_some(s)
            });
            let start_type = open(&m, &name, SERVICE_QUERY_CONFIG)
                .and_then(|s| query_config(&s))
                .map(|(t, _)| StartType::from_raw(t))
                .unwrap_or(StartType::Other(u32::MAX));
            out.push(UserServiceInstance { name, pid, session_id, start_type, running });
        }
        Ok(out)
    }

    fn set_start_type(&self, name: &str, t: StartType) -> Result<(), PadError> {
        let m = scm(SC_MANAGER_CONNECT)?;
        let s = open(&m, name, SERVICE_CHANGE_CONFIG)?;
        // SAFETY: only the start type changes; every other field SERVICE_NO_CHANGE / null.
        let ok = unsafe {
            ChangeServiceConfigW(
                s.0,
                SERVICE_NO_CHANGE,
                t.to_raw(),
                SERVICE_NO_CHANGE,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
            )
        };
        if ok == 0 {
            return Err(PadError::os("ChangeServiceConfigW", last_error()));
        }
        Ok(())
    }

    fn stop(&self, name: &str) -> Result<(), PadError> {
        let m = scm(SC_MANAGER_CONNECT)?;
        let s = open(&m, name, SERVICE_STOP)?;
        let mut st = SERVICE_STATUS::default();
        // SAFETY: valid handle and out struct.
        if unsafe { ControlService(s.0, SERVICE_CONTROL_STOP, &mut st) } == 0 {
            return Err(PadError::os("ControlService", last_error()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_path_expansion() {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
        assert_eq!(expand_image("\\SystemRoot\\System32\\drivers\\x.sys"), format!("{root}\\System32\\drivers\\x.sys"));
        assert_eq!(expand_image("System32\\drivers\\x.sys"), format!("{root}\\System32\\drivers\\x.sys"));
        assert_eq!(expand_image("\\??\\C:\\a\\b.sys"), "C:\\a\\b.sys");
        assert_eq!(expand_image("\"C:\\p q\\x.exe\""), "C:\\p q\\x.exe");
    }

    /// Read-only queries of this machine (no change).
    #[test]
    fn read_only_probes() {
        assert!(!service_facts("PulsarSplitDefinitelyMissingSvc").exists);
        let k = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into()) + "\\System32\\kernel32.dll";
        assert!(file_version(&k).is_some());
    }
}
