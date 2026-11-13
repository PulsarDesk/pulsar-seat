// SPDX-License-Identifier: GPL-3.0-only
//! Windows side of [`crate::loopback_listener`]: Windows Firewall rules (added / deleted
//! through the documented `netsh advfirewall firewall` command, read back with their
//! properties through the documented `INetFwPolicy2` COM API, so no locale-dependent netsh
//! output is parsed), the TCP listener table, and a TermService restart through the SCM
//! (stop the active dependents, stop TermService, start it, start the dependents again;
//! same shape as the termwrap module's restart).
//!
//! **HYPOTHESIS:** TermService accepts `SERVICE_CONTROL_STOP`. Some Windows builds report
//! that it does not (`SERVICE_ACCEPT_STOP` missing from the controls it accepts). The
//! restart checks that **before** stopping anything and returns
//! [`RestartError::NotStoppable`]; the listener state then asks for a PC restart
//! (`restart_needs_reboot`) instead of timing out on every try.
//!
//! **Untested on hardware.** Nothing here runs in unit tests except argument building:
//! every call changes the system (firewall, services) and needs SYSTEM / an elevated
//! token. Never run it on the dev laptop.

use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    ERROR_MORE_DATA, ERROR_SERVICE_ALREADY_RUNNING, ERROR_SERVICE_CANNOT_ACCEPT_CTRL, ERROR_SERVICE_NOT_ACTIVE,
};
use windows_sys::Win32::System::Services::{
    CloseServiceHandle, ControlService, ENUM_SERVICE_STATUSW, EnumDependentServicesW, OpenSCManagerW, OpenServiceW,
    QueryServiceStatus, SC_HANDLE, SC_MANAGER_CONNECT, SERVICE_ACCEPT_STOP, SERVICE_ACTIVE, SERVICE_CONTROL_STOP, SERVICE_ENUMERATE_DEPENDENTS,
    SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_START, SERVICE_STATUS, SERVICE_STOP, SERVICE_STOPPED, StartServiceW,
};

use super::{expand_env, last_error, wide};
use crate::error::SysError;
use crate::listener::TcpListenerRow;
use crate::loopback_listener::{FirewallBlockRule, FirewallRuleInfo, LoopbackHost, RestartError};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const TERMSERVICE: &str = "TermService";
const FIREWALL_SERVICE: &str = "MpsSvc";
const POLL: Duration = Duration::from_millis(250);
const SERVICE_PAUSED: u32 = 7;

/// Real [`LoopbackHost`].
#[derive(Debug, Default, Clone, Copy)]
pub struct WindowsLoopbackHost;

/// Pure: a value that is safe inside a double-quoted netsh token.
fn quotable(s: &str) -> Result<&str, SysError> {
    if s.is_empty() || s.chars().any(|c| c == '"' || c.is_control()) {
        return Err(SysError::Other(format!("not usable in a netsh argument: {s:?}")));
    }
    Ok(s)
}

/// Pure: the raw netsh arguments to add `rule` (inbound, TCP, block, all profiles).
pub fn add_rule_args(rule: &FirewallBlockRule) -> Result<Vec<String>, SysError> {
    if rule.port == 0 || !rule.remote.chars().all(|c| c.is_ascii_hexdigit() || ".:,-".contains(c)) {
        return Err(SysError::Other("invalid firewall rule".into()));
    }
    Ok(vec![
        "advfirewall".into(),
        "firewall".into(),
        "add".into(),
        "rule".into(),
        format!("name=\"{}\"", quotable(&rule.name)?),
        "dir=in".into(),
        "action=block".into(),
        "protocol=TCP".into(),
        format!("localport={}", rule.port),
        format!("remoteip={}", rule.remote),
        "profile=any".into(),
        "edge=no".into(),
        "enable=yes".into(),
        format!("description=\"{}\"", quotable(&rule.description)?),
    ])
}

fn netsh(args: &[String]) -> Result<i32, SysError> {
    let exe = expand_env(r"%SystemRoot%\System32\netsh.exe").ok_or_else(|| SysError::Other("cannot expand %SystemRoot%".into()))?;
    let mut c = Command::new(exe);
    for a in args {
        // netsh parses its own command line; `name="a b"` must reach it verbatim.
        c.raw_arg(a);
    }
    let out = c
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map_err(|e| SysError::Other(format!("netsh: {e}")))?;
    Ok(out.code().unwrap_or(-1))
}

fn name_args(verb: &str, name: &str) -> Result<Vec<String>, SysError> {
    Ok(vec!["advfirewall".into(), "firewall".into(), verb.into(), "rule".into(), format!("name=\"{}\"", quotable(name)?)])
}

struct Handle(SC_HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: handle from OpenSCManagerW / OpenServiceW, closed once.
            unsafe { CloseServiceHandle(self.0) };
        }
    }
}

fn scm_err(msg: String) -> SysError {
    SysError::Other(format!("TermService restart: {msg}"))
}

fn open_scm() -> Result<Handle, SysError> {
    // SAFETY: null machine / database = local active database.
    let h = unsafe { OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT) };
    if h.is_null() {
        return Err(scm_err(format!("OpenSCManagerW failed: {}", last_error())));
    }
    Ok(Handle(h))
}

fn open_service(scm: &Handle, name: &str, access: u32) -> Result<Handle, SysError> {
    let n = wide(name);
    // SAFETY: valid SCM handle, NUL-terminated name.
    let h = unsafe { OpenServiceW(scm.0, n.as_ptr(), access) };
    if h.is_null() {
        return Err(scm_err(format!("OpenServiceW({name}) failed: {}", last_error())));
    }
    Ok(Handle(h))
}

fn status(svc: &Handle) -> Result<SERVICE_STATUS, SysError> {
    let mut st = SERVICE_STATUS::default();
    // SAFETY: svc has SERVICE_QUERY_STATUS; st is a valid out pointer.
    if unsafe { QueryServiceStatus(svc.0, &mut st) } == 0 {
        return Err(scm_err(format!("QueryServiceStatus failed: {}", last_error())));
    }
    Ok(st)
}

fn state(svc: &Handle) -> Result<u32, SysError> {
    Ok(status(svc)?.dwCurrentState)
}

/// Pure: a running service that does not list stop among the controls it accepts.
pub fn refuses_stop(current_state: u32, controls_accepted: u32) -> bool {
    current_state == SERVICE_RUNNING && controls_accepted & SERVICE_ACCEPT_STOP == 0
}

fn stop(svc: &Handle, name: &str, timeout: Duration) -> Result<(), SysError> {
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
                        return Err(scm_err(format!("stop {name} failed: {e}")));
                    }
                }
            }
            _ => {}
        }
        if Instant::now() >= deadline {
            return Err(scm_err(format!("{name}: stop timed out")));
        }
        sleep(POLL);
    }
}

fn start(svc: &Handle, name: &str, timeout: Duration) -> Result<(), SysError> {
    let deadline = Instant::now() + timeout;
    let mut started = false;
    loop {
        match state(svc)? {
            SERVICE_RUNNING => return Ok(()),
            SERVICE_STOPPED if started => return Err(scm_err(format!("{name} stopped again right after start"))),
            SERVICE_STOPPED => {
                // SAFETY: svc has SERVICE_START; no arguments.
                if unsafe { StartServiceW(svc.0, 0, std::ptr::null()) } == 0 {
                    let e = last_error();
                    if e != ERROR_SERVICE_ALREADY_RUNNING {
                        return Err(scm_err(format!("start {name} failed: {e}")));
                    }
                }
                started = true;
            }
            _ => {}
        }
        if Instant::now() >= deadline {
            return Err(scm_err(format!("{name}: start timed out")));
        }
        sleep(POLL);
    }
}

fn active_dependents(svc: &Handle) -> Result<Vec<String>, SysError> {
    let (mut needed, mut count) = (0u32, 0u32);
    // SAFETY: probing call with a zero-size buffer.
    if unsafe { EnumDependentServicesW(svc.0, SERVICE_ACTIVE, std::ptr::null_mut(), 0, &mut needed, &mut count) } != 0 {
        return Ok(Vec::new());
    }
    let e = last_error();
    if e != ERROR_MORE_DATA {
        return Err(scm_err(format!("EnumDependentServicesW failed: {e}")));
    }
    let n = (needed as usize).div_ceil(std::mem::size_of::<ENUM_SERVICE_STATUSW>()) + 1;
    let mut buf: Vec<ENUM_SERVICE_STATUSW> = vec![ENUM_SERVICE_STATUSW::default(); n];
    let bytes = (buf.len() * std::mem::size_of::<ENUM_SERVICE_STATUSW>()) as u32;
    // SAFETY: buf is aligned and `bytes` long.
    if unsafe { EnumDependentServicesW(svc.0, SERVICE_ACTIVE, buf.as_mut_ptr(), bytes, &mut needed, &mut count) } == 0 {
        return Err(scm_err(format!("EnumDependentServicesW failed: {}", last_error())));
    }
    // SAFETY: each lpServiceName points to a NUL-terminated string inside buf.
    Ok(buf[..count as usize].iter().map(|e| unsafe { super::from_wide_ptr(e.lpServiceName) }).collect())
}

impl LoopbackHost for WindowsLoopbackHost {
    fn firewall_service_running(&self) -> bool {
        super::listener::service_pid(FIREWALL_SERVICE).is_some()
    }

    fn firewall_rules(&self, prefix: &str) -> Result<Vec<FirewallRuleInfo>, SysError> {
        com_firewall_rules(prefix)
    }

    fn add_firewall_rule(&self, rule: &FirewallBlockRule) -> Result<(), SysError> {
        match netsh(&add_rule_args(rule)?)? {
            0 => Ok(()),
            c => Err(SysError::Other(format!("netsh add rule exited with {c}"))),
        }
    }

    fn delete_firewall_rule(&self, name: &str) -> Result<(), SysError> {
        if !com_firewall_rules(name)?.iter().any(|r| r.name == name) {
            return Ok(());
        }
        match netsh(&name_args("delete", name)?)? {
            0 => Ok(()),
            c => Err(SysError::Other(format!("netsh delete rule exited with {c}"))),
        }
    }

    fn tcp_listeners(&self) -> Result<Vec<TcpListenerRow>, SysError> {
        super::listener::tcp_listeners()
    }

    fn termservice_pid(&self) -> Option<u32> {
        super::listener::service_pid(TERMSERVICE)
    }

    fn restart_termservice(&self, timeout: Duration) -> Result<(), RestartError> {
        let scm = open_scm()?;
        let svc = open_service(&scm, TERMSERVICE, SERVICE_STOP | SERVICE_START | SERVICE_QUERY_STATUS | SERVICE_ENUMERATE_DEPENDENTS)?;
        // Nothing is stopped when TermService itself would refuse the stop.
        let st = status(&svc)?;
        if refuses_stop(st.dwCurrentState, st.dwControlsAccepted) {
            return Err(RestartError::NotStoppable);
        }
        let deps = active_dependents(&svc)?;
        let mut stopped: Vec<(String, Handle)> = Vec::with_capacity(deps.len());
        let mut res = Ok(());
        for name in &deps {
            match open_service(&scm, name, SERVICE_STOP | SERVICE_START | SERVICE_QUERY_STATUS) {
                Ok(h) => {
                    let r = stop(&h, name, timeout);
                    stopped.push((name.clone(), h));
                    if let Err(e) = r {
                        res = Err(e);
                        break;
                    }
                }
                Err(e) => {
                    res = Err(e);
                    break;
                }
            }
        }
        if res.is_ok() {
            res = stop(&svc, TERMSERVICE, timeout);
        }
        // Start TermService again in every case (also after a failed stop).
        let up = start(&svc, TERMSERVICE, timeout);
        if up.is_ok() {
            for (name, h) in stopped.iter().rev() {
                if let Err(e) = start(h, name, timeout) {
                    log::warn!("loopback listener: dependent service {name} did not start again: {e}");
                }
            }
        }
        Ok(res.and(up)?)
    }
}

/// Every firewall rule whose name starts with `prefix`, read through `INetFwPolicy2`
/// (`HNetCfg.FwPolicy2`). Initialises COM (MTA) on this thread for the call when it is not
/// initialised yet.
fn com_firewall_rules(prefix: &str) -> Result<Vec<FirewallRuleInfo>, SysError> {
    use windows::Win32::NetworkManagement::WindowsFirewall::{INetFwPolicy2, INetFwRule, NET_FW_ACTION_BLOCK, NET_FW_RULE_DIR_IN, NetFwPolicy2};
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize, IDispatch};
    use windows::Win32::System::Ole::IEnumVARIANT;
    use windows::Win32::System::Variant::{VARIANT, VT_DISPATCH};
    use windows::core::Interface;

    struct ComScope(bool);
    impl Drop for ComScope {
        fn drop(&mut self) {
            if self.0 {
                // SAFETY: balances the successful CoInitializeEx below on this thread.
                unsafe { CoUninitialize() };
            }
        }
    }
    let fw = |what: &str, e: windows::core::Error| SysError::Other(format!("firewall COM {what}: {e}"));
    // SAFETY: plain COM initialisation; S_OK / S_FALSE are balanced by ComScope,
    // RPC_E_CHANGED_MODE (already STA) leaves COM usable and needs no uninit.
    let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    let _scope = ComScope(hr.is_ok());
    // SAFETY: documented in-proc class; the interface pointer is released on drop.
    let policy: INetFwPolicy2 = unsafe { CoCreateInstance(&NetFwPolicy2, None, CLSCTX_INPROC_SERVER) }.map_err(|e| fw("FwPolicy2", e))?;
    // SAFETY: COM calls on valid interfaces; out values are owned by the wrappers.
    let rules = unsafe { policy.Rules() }.map_err(|e| fw("Rules", e))?;
    // SAFETY: as above.
    let en: IEnumVARIANT = unsafe { rules._NewEnum() }.map_err(|e| fw("_NewEnum", e))?.cast().map_err(|e| fw("IEnumVARIANT", e))?;
    let s = |b: windows::core::Result<windows::core::BSTR>| b.map(|b| b.to_string()).unwrap_or_default();
    let mut out = Vec::new();
    loop {
        let mut v = [VARIANT::default()];
        let mut got = 0u32;
        // SAFETY: one-element buffer, `got` receives the count.
        let hr = unsafe { en.Next(&mut v, &mut got) };
        if hr.is_err() {
            return Err(SysError::Other(format!("firewall COM enumerate: {hr:?}")));
        }
        if got == 0 {
            break;
        }
        // SAFETY: the union is read only after checking its tag.
        let disp: Option<IDispatch> = unsafe {
            let inner = &v[0].Anonymous.Anonymous;
            if inner.vt == VT_DISPATCH { (*inner.Anonymous.pdispVal).clone() } else { None }
        };
        let Some(rule) = disp.and_then(|d| d.cast::<INetFwRule>().ok()) else { continue };
        // SAFETY: property getters on a valid rule.
        unsafe {
            let name = s(rule.Name());
            if !name.starts_with(prefix) {
                continue;
            }
            out.push(FirewallRuleInfo {
                name,
                enabled: rule.Enabled().map(|b| b.as_bool()).unwrap_or(false),
                inbound: rule.Direction().is_ok_and(|d| d == NET_FW_RULE_DIR_IN),
                block: rule.Action().is_ok_and(|a| a == NET_FW_ACTION_BLOCK),
                protocol: rule.Protocol().unwrap_or(-1),
                local_ports: s(rule.LocalPorts()),
                local_addresses: s(rule.LocalAddresses()),
                remote_addresses: s(rule.RemoteAddresses()),
                profiles: rule.Profiles().unwrap_or(0),
                interface_types: s(rule.InterfaceTypes()),
                application: s(rule.ApplicationName()),
                service: s(rule.ServiceName()),
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn netsh_arguments() {
        let r = FirewallBlockRule::for_port(33389);
        let a = add_rule_args(&r).unwrap();
        assert!(a.contains(&"localport=33389".to_string()));
        assert!(a.contains(&"action=block".to_string()));
        assert!(a.contains(&format!("name=\"{}\"", crate::loopback_listener::firewall_rule_name(33389))));
        assert!(a.iter().any(|x| x.starts_with("remoteip=0.0.0.0-126.255.255.255,")));
        let bad = FirewallBlockRule { name: "x\" y".into(), ..r.clone() };
        assert!(add_rule_args(&bad).is_err());
        let bad = FirewallBlockRule { remote: "any; del".into(), ..r };
        assert!(add_rule_args(&bad).is_err());
        assert!(name_args("show", "").is_err());
    }

    #[test]
    fn stop_refusal() {
        assert!(refuses_stop(SERVICE_RUNNING, 0));
        assert!(!refuses_stop(SERVICE_RUNNING, SERVICE_ACCEPT_STOP));
        assert!(!refuses_stop(SERVICE_STOPPED, 0));
    }
}
