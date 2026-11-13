// SPDX-License-Identifier: GPL-3.0-only
//! Real RDS probe: listener (registry + TCP table + certificate thumbprint), licensing,
//! ServiceDll, readiness (spec 30 §2.2, §3, §5.2, §13).

use super::wide;
use super::wmi::{Wmi, WmiValue};
use crate::rds::{
    LicensingInfo, ListenerInfo, RdsProbe, Registry, TS_RDP_TCP, licensing_mode_from_registry, listener_from_registry,
    service_dll_from_registry,
};
use crate::wait::poll_until;
use crate::{SessionError, SystemClock};
use std::sync::Arc;
use std::time::Duration;
use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
use windows_sys::Win32::NetworkManagement::IpHelper::{GetTcp6Table, GetTcpTable, MIB_TCP_STATE_LISTEN, MIB_TCP6TABLE, MIB_TCPTABLE};
use windows_sys::Win32::System::RemoteDesktop::{WTS_CURRENT_SERVER_HANDLE, WTS_SESSION_INFOW, WTSEnumerateSessionsW, WTSFreeMemory};
use windows_sys::Win32::System::Services::{
    CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceStatus, SC_MANAGER_CONNECT, SERVICE_QUERY_STATUS, SERVICE_RUNNING,
    SERVICE_STATUS,
};
use windows_sys::Win32::System::Threading::{OpenEventW, WaitForSingleObject};

const TS_NAMESPACE: &str = r"root\cimv2\TerminalServices";
const SYNCHRONIZE: u32 = 0x0010_0000;

pub struct WindowsRdsProbe {
    reg: Arc<dyn Registry>,
}

impl WindowsRdsProbe {
    pub fn new(reg: Arc<dyn Registry>) -> Self {
        Self { reg }
    }
}

/// (IPv4 listener on `port`, IPv6 listener on `port`).
fn tcp_listeners(port: u16) -> (bool, bool) {
    let mut v4 = false;
    let mut v6 = false;
    // IPv4
    let mut size = 0u32;
    // SAFETY: size query with a null table.
    unsafe { GetTcpTable(std::ptr::null_mut(), &mut size, 0) };
    if size > 0 {
        let mut buf = vec![0u64; (size as usize).div_ceil(8)];
        let t = buf.as_mut_ptr().cast::<MIB_TCPTABLE>();
        // SAFETY: buffer of at least `size` bytes, 8-byte aligned.
        if unsafe { GetTcpTable(t, &mut size, 0) } == 0 {
            // SAFETY: the API filled dwNumEntries rows.
            let rows = unsafe { std::slice::from_raw_parts((*t).table.as_ptr(), (*t).dwNumEntries as usize) };
            v4 = rows.iter().any(|r| {
                // SAFETY: dwState is the documented member of the union.
                let st = unsafe { r.Anonymous.dwState };
                st == MIB_TCP_STATE_LISTEN as u32 && u16::from_be((r.dwLocalPort & 0xFFFF) as u16) == port
            });
        }
    }
    // IPv6
    let mut size = 0u32;
    // SAFETY: size query.
    unsafe { GetTcp6Table(std::ptr::null_mut(), &mut size, 0) };
    if size > 0 {
        let mut buf = vec![0u64; (size as usize).div_ceil(8)];
        let t = buf.as_mut_ptr().cast::<MIB_TCP6TABLE>();
        // SAFETY: as above.
        if unsafe { GetTcp6Table(t, &mut size, 0) } == 0 {
            // SAFETY: as above.
            let rows = unsafe { std::slice::from_raw_parts((*t).table.as_ptr(), (*t).dwNumEntries as usize) };
            v6 = rows.iter().any(|r| r.State == MIB_TCP_STATE_LISTEN && u16::from_be((r.dwLocalPort & 0xFFFF) as u16) == port);
        }
    }
    (v4, v6)
}

/// Thumbprint of the listener certificate: WMI `Win32_TSGeneralSetting.SSLCertificateSHA1Hash`,
/// else the `SSLCertificateSHA1Hash` binary value of the RDP-Tcp key.
fn listener_cert_sha1() -> Option<[u8; 20]> {
    let from_wmi = Wmi::connect(TS_NAMESPACE)
        .and_then(|w| w.query_first("SELECT SSLCertificateSHA1Hash FROM Win32_TSGeneralSetting WHERE TerminalName='RDP-Tcp'", &["SSLCertificateSHA1Hash"]))
        .ok()
        .flatten()
        .and_then(|r| r[0].as_str().and_then(crate::helper_proto::parse_hex20));
    from_wmi.or_else(|| {
        super::registry::get_binary(TS_RDP_TCP, "SSLCertificateSHA1Hash")
            .filter(|b| b.len() == 20)
            .map(|b| b.try_into().expect("20 bytes"))
    })
}

fn termservice_running() -> bool {
    let svc = wide("TermService");
    // SAFETY: plain SCM queries; handles closed below.
    unsafe {
        let scm = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT);
        if scm.is_null() {
            return false;
        }
        let s = OpenServiceW(scm, svc.as_ptr(), SERVICE_QUERY_STATUS);
        let mut st: SERVICE_STATUS = std::mem::zeroed();
        let ok = !s.is_null() && QueryServiceStatus(s, &mut st) != 0 && st.dwCurrentState == SERVICE_RUNNING;
        if !s.is_null() {
            CloseServiceHandle(s);
        }
        CloseServiceHandle(scm);
        ok
    }
}

fn wts_enumerates() -> bool {
    let mut p: *mut WTS_SESSION_INFOW = std::ptr::null_mut();
    let mut n = 0u32;
    // SAFETY: output freed with WTSFreeMemory.
    unsafe {
        if WTSEnumerateSessionsW(WTS_CURRENT_SERVER_HANDLE, 0, 1, &mut p, &mut n) == 0 {
            return false;
        }
        WTSFreeMemory(p.cast());
    }
    true
}

/// Waits on `Global\TermSrvReadyEvent` if it exists (SYNCHRONIZE only, never created).
/// Returns `None` when the event does not exist.
fn wait_event(ms: u32) -> Option<bool> {
    let name = wide(r"Global\TermSrvReadyEvent");
    // SAFETY: open-only; handle closed below.
    unsafe {
        let h = OpenEventW(SYNCHRONIZE, 0, name.as_ptr());
        if h.is_null() {
            return None;
        }
        let r = WaitForSingleObject(h, ms) == WAIT_OBJECT_0;
        CloseHandle(h);
        Some(r)
    }
}

impl RdsProbe for WindowsRdsProbe {
    fn listener(&self) -> Result<ListenerInfo, SessionError> {
        let mut l = listener_from_registry(self.reg.as_ref())?;
        let (v4, v6) = tcp_listeners(l.port);
        l.ipv6_only = !v4 && v6;
        l.cert_sha1 = listener_cert_sha1();
        Ok(l)
    }

    fn licensing(&self) -> Result<LicensingInfo, SessionError> {
        let mode = licensing_mode_from_registry(self.reg.as_ref())?;
        // Grace days exist only on RDSH servers; any failure means "unknown".
        let grace = Wmi::connect(TS_NAMESPACE)
            .and_then(|w| w.call_on_first("SELECT * FROM Win32_TerminalServiceSetting", "GetGracePeriodDays", "DaysLeft"))
            .ok()
            .flatten()
            .and_then(|v| match v {
                WmiValue::Null => None,
                other => other.as_u32(),
            });
        Ok(LicensingInfo { mode, grace_days_left: grace })
    }

    fn service_dll(&self) -> Result<String, SessionError> {
        service_dll_from_registry(self.reg.as_ref())
    }

    fn wait_ready(&self, timeout: Duration) -> Result<(), SessionError> {
        let total = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
        if let Some(signalled) = wait_event(total) {
            return if signalled { Ok(()) } else { Err(SessionError::RdsNotReady("TermSrvReadyEvent not signalled in time".into())) };
        }
        let c = SystemClock;
        poll_until(&c, &c, timeout, Duration::from_millis(500), || {
            if wait_event(0) == Some(true) {
                return Some(());
            }
            (termservice_running() && wts_enumerates()).then_some(())
        })
        .ok_or_else(|| SessionError::RdsNotReady("Remote Desktop Services not ready".into()))
    }
}
