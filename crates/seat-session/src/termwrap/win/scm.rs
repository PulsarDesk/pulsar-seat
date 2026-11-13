// SPDX-License-Identifier: GPL-3.0-only
//! TermService through the Service Control Manager. Stops active dependents first, then
//! TermService, waits for each with a timeout, starts TermService and the dependents that
//! were running. Never terminates the hosting svchost by process id.
//!
//! Pending states are waited out instead of failing: a stop is only sent to a running (or
//! paused) service, `ERROR_SERVICE_CANNOT_ACCEPT_CTRL` means "busy, wait". If the stop
//! phase fails, everything stopped so far is started again (TermService first, then the
//! dependents in reverse order) before the original error is returned.

use std::thread::sleep;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    ERROR_MORE_DATA, ERROR_SERVICE_ALREADY_RUNNING, ERROR_SERVICE_CANNOT_ACCEPT_CTRL, ERROR_SERVICE_NOT_ACTIVE,
};
use windows_sys::Win32::System::Services::{
    CloseServiceHandle, ControlService, ENUM_SERVICE_STATUSW, EnumDependentServicesW, OpenSCManagerW,
    OpenServiceW, QueryServiceStatus, SC_HANDLE, SC_MANAGER_CONNECT, SERVICE_ACTIVE, SERVICE_CONTROL_STOP,
    SERVICE_ENUMERATE_DEPENDENTS, SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_START, SERVICE_STATUS,
    SERVICE_STOP, SERVICE_STOPPED, StartServiceW,
};

use super::{expand_env, last_error, wide};
use crate::termwrap::TermwrapError;
use crate::journal::RegValue;
use crate::termwrap::paths::STOCK_SERVICE_DLL;
use crate::registry::Registry;
use crate::termwrap::system::{SERVICE_DLL, TERMSERVICE_PARAMS, TermServiceControl};
use crate::version::FileVersion;

pub const TERMSERVICE: &str = "TermService";
const POLL: Duration = Duration::from_millis(250);
/// SERVICE_PAUSED: accepts a stop like a running service.
const SERVICE_PAUSED: u32 = 7;

pub struct WinTermService;

struct Handle(SC_HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: handle came from OpenSCManagerW / OpenServiceW and is closed once.
            unsafe { CloseServiceHandle(self.0) };
        }
    }
}

fn restart_err(msg: String) -> TermwrapError {
    TermwrapError::TermserviceRestart(msg)
}

fn open_scm() -> Result<Handle, TermwrapError> {
    // SAFETY: null machine / database = local active database.
    let h = unsafe { OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT) };
    if h.is_null() {
        return Err(restart_err(format!("OpenSCManagerW failed: {}", last_error())));
    }
    Ok(Handle(h))
}

fn open_service(scm: &Handle, name: &str, access: u32) -> Result<Handle, TermwrapError> {
    let n = wide(name);
    // SAFETY: scm is a valid SCM handle; n is NUL-terminated.
    let h = unsafe { OpenServiceW(scm.0, n.as_ptr(), access) };
    if h.is_null() {
        return Err(restart_err(format!("OpenServiceW({name}) failed: {}", last_error())));
    }
    Ok(Handle(h))
}

fn state(svc: &Handle) -> Result<u32, TermwrapError> {
    let mut st = SERVICE_STATUS::default();
    // SAFETY: svc has SERVICE_QUERY_STATUS; st is a valid out pointer.
    if unsafe { QueryServiceStatus(svc.0, &mut st) } == 0 {
        let e = last_error();
        return Err(restart_err(format!("QueryServiceStatus failed: {e}")));
    }
    Ok(st.dwCurrentState)
}

fn timed_out(name: &str, what: &str, timeout: Duration) -> TermwrapError {
    restart_err(format!("{name}: {what} timed out after {}s", timeout.as_secs()))
}

/// Stops `svc`: waits out pending states, sends a stop only to a running / paused service.
fn stop(svc: &Handle, name: &str, timeout: Duration) -> Result<(), TermwrapError> {
    let deadline = Instant::now() + timeout;
    loop {
        match state(svc)? {
            SERVICE_STOPPED => return Ok(()),
            SERVICE_RUNNING | SERVICE_PAUSED => {
                let mut st = SERVICE_STATUS::default();
                // SAFETY: svc has SERVICE_STOP.
                if unsafe { ControlService(svc.0, SERVICE_CONTROL_STOP, &mut st) } == 0 {
                    let e = last_error();
                    if e != ERROR_SERVICE_NOT_ACTIVE && e != ERROR_SERVICE_CANNOT_ACCEPT_CTRL {
                        return Err(restart_err(format!("stop {name} failed: {e}")));
                    }
                }
            }
            // START_PENDING, STOP_PENDING, CONTINUE_PENDING, PAUSE_PENDING: wait.
            _ => {}
        }
        if Instant::now() >= deadline {
            return Err(timed_out(name, "stop", timeout));
        }
        sleep(POLL);
    }
}

/// Starts `svc`: waits out a pending stop, starts once, waits for RUNNING.
fn start(svc: &Handle, name: &str, timeout: Duration) -> Result<(), TermwrapError> {
    let deadline = Instant::now() + timeout;
    let mut started = false;
    loop {
        match state(svc)? {
            SERVICE_RUNNING => return Ok(()),
            SERVICE_STOPPED if started => {
                return Err(restart_err(format!("{name} stopped again right after start")));
            }
            SERVICE_STOPPED => {
                // SAFETY: svc has SERVICE_START; no arguments.
                if unsafe { StartServiceW(svc.0, 0, std::ptr::null()) } == 0 {
                    let e = last_error();
                    if e != ERROR_SERVICE_ALREADY_RUNNING {
                        return Err(restart_err(format!("start {name} failed: {e}")));
                    }
                }
                started = true;
            }
            _ => {}
        }
        if Instant::now() >= deadline {
            return Err(timed_out(name, "start", timeout));
        }
        sleep(POLL);
    }
}

/// Names of the active services that depend on `svc`, in the order the SCM returns them
/// (the order in which they must be stopped).
fn active_dependents(svc: &Handle) -> Result<Vec<String>, TermwrapError> {
    let mut needed = 0u32;
    let mut count = 0u32;
    // SAFETY: probing call with a zero-size buffer.
    let ok = unsafe { EnumDependentServicesW(svc.0, SERVICE_ACTIVE, std::ptr::null_mut(), 0, &mut needed, &mut count) };
    if ok != 0 {
        return Ok(Vec::new());
    }
    let e = last_error();
    if e != ERROR_MORE_DATA {
        return Err(restart_err(format!("EnumDependentServicesW failed: {e}")));
    }
    let n = (needed as usize).div_ceil(std::mem::size_of::<ENUM_SERVICE_STATUSW>()) + 1;
    let mut buf: Vec<ENUM_SERVICE_STATUSW> = vec![ENUM_SERVICE_STATUSW::default(); n];
    let bytes = (buf.len() * std::mem::size_of::<ENUM_SERVICE_STATUSW>()) as u32;
    // SAFETY: buf is suitably aligned and `bytes` long.
    if unsafe { EnumDependentServicesW(svc.0, SERVICE_ACTIVE, buf.as_mut_ptr(), bytes, &mut needed, &mut count) } == 0 {
        let e = last_error();
        return Err(restart_err(format!("EnumDependentServicesW failed: {e}")));
    }
    let mut names = Vec::with_capacity(count as usize);
    for e in &buf[..count as usize] {
        // SAFETY: lpServiceName points to a NUL-terminated string inside buf.
        let mut len = 0;
        unsafe {
            while *e.lpServiceName.add(len) != 0 {
                len += 1;
            }
            names.push(String::from_utf16_lossy(std::slice::from_raw_parts(e.lpServiceName, len)));
        }
    }
    Ok(names)
}

/// Stop phase: each dependent (pushed before its stop, so a half-stopped one is started
/// again on unwind), then TermService.
fn stop_all(
    scm: &Handle,
    svc: &Handle,
    deps: &[String],
    timeout: Duration,
    stopped: &mut Vec<(String, Handle)>,
) -> Result<(), TermwrapError> {
    for name in deps {
        let h = open_service(scm, name, SERVICE_STOP | SERVICE_START | SERVICE_QUERY_STATUS)?;
        stopped.push((name.clone(), h));
        let (name, h) = stopped.last().expect("just pushed");
        stop(h, name, timeout)?;
    }
    stop(svc, TERMSERVICE, timeout)
}

fn start_dependents(stopped: &[(String, Handle)], timeout: Duration) {
    for (name, h) in stopped.iter().rev() {
        if let Err(e) = start(h, name, timeout) {
            log::warn!("termwrap: dependent service {name} did not start again: {e}");
        }
    }
}

impl TermServiceControl for WinTermService {
    fn service_dll(&self) -> Result<String, TermwrapError> {
        match crate::win::registry::WinRegistry.get(TERMSERVICE_PARAMS, SERVICE_DLL)? {
            RegValue::ExpandSz(s) | RegValue::Sz(s) => Ok(s),
            other => Err(TermwrapError::Registry(format!("ServiceDll has unexpected value {other:?}"))),
        }
    }

    fn set_service_dll(&self, value: &str) -> Result<(), TermwrapError> {
        Ok(crate::win::registry::WinRegistry.set(TERMSERVICE_PARAMS, SERVICE_DLL, &RegValue::ExpandSz(value.to_owned()))?)
    }

    fn restart(&self, timeout: Duration) -> Result<(), TermwrapError> {
        let scm = open_scm()?;
        let svc = open_service(
            &scm,
            TERMSERVICE,
            SERVICE_STOP | SERVICE_START | SERVICE_QUERY_STATUS | SERVICE_ENUMERATE_DEPENDENTS,
        )?;
        let deps = active_dependents(&svc)?;
        let mut stopped = Vec::with_capacity(deps.len());
        if let Err(e) = stop_all(&scm, &svc, &deps, timeout, &mut stopped) {
            log::error!("termwrap: stopping TermService failed ({e}); starting back what was stopped");
            if let Err(s) = start(&svc, TERMSERVICE, timeout) {
                log::error!("termwrap: TermService did not start again: {s}");
            }
            start_dependents(&stopped, timeout);
            return Err(e);
        }
        let res = start(&svc, TERMSERVICE, timeout);
        // Dependents need TermService; try them anyway only if it came up.
        if res.is_ok() {
            start_dependents(&stopped, timeout);
        }
        res
    }

    fn termsrv_version(&self) -> Result<FileVersion, TermwrapError> {
        let path = expand_env(STOCK_SERVICE_DLL)
            .ok_or_else(|| TermwrapError::TermsrvVersionUnreadable("cannot expand %SystemRoot%".into()))?;
        crate::win::files::file_version(std::path::Path::new(&path))
            .ok_or_else(|| TermwrapError::TermsrvVersionUnreadable(format!("{path}: no VS_FIXEDFILEINFO")))
    }
}
