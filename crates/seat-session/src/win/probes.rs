// SPDX-License-Identifier: GPL-3.0-only
//! Real edition and RDS probes (read-only), for both seat kinds. Remote-seat parts
//! (listener TCP table + certificate thumbprint, WMI role / grace days, ready polling)
//! merged from Pulsar Split's `split-session::win::{edition, rds_probe}` (step 7).

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
use windows_sys::Win32::NetworkManagement::IpHelper::{GetTcp6Table, GetTcpTable, MIB_TCP_STATE_LISTEN, MIB_TCP6TABLE, MIB_TCPTABLE};
use windows_sys::Win32::System::RemoteDesktop::{WTS_CURRENT_SERVER_HANDLE, WTS_SESSION_INFOW, WTSEnumerateSessionsW, WTSFreeMemory};
use windows_sys::Win32::System::Services::{
    CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceStatus, SC_MANAGER_CONNECT, SERVICE_QUERY_STATUS, SERVICE_RUNNING,
    SERVICE_STATUS,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, IsWow64Process2, OpenEventW, WaitForSingleObject};

use super::{expand_env, wide};
use crate::edition::{EditionInfo, EditionProbe, edition_from_registry};
use crate::error::SysError;
use super::wmi::{Wmi, WmiValue};
use crate::journal::{RegValue, b64_decode};
use crate::rds::{
    LicensingInfo, ListenerInfo, RdsProbe, TS, TS_RDP_TCP, licensing_mode_from_registry, listener_from_registry,
    service_dll_from_registry,
};
use crate::registry::{Registry, get_u32};

/// `SYNCHRONIZE` access right.
const SYNCHRONIZE: u32 = 0x0010_0000;
const TS_NAMESPACE: &str = r"root\cimv2\TerminalServices";

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

    /// `Win32_TerminalServiceSetting.TerminalServerMode = 1` (documented; Pulsar Split's
    /// probe) means the RD Session Host role is installed. When WMI cannot answer,
    /// `Terminal Server\TSAppCompat = 1` (Seat's probe, HYPOTHESIS on current Server
    /// builds) is used.
    fn rdsh_role_installed(&self) -> Result<bool, SysError> {
        let wmi = Wmi::connect(TS_NAMESPACE)
            .and_then(|w| w.query_first("SELECT TerminalServerMode FROM Win32_TerminalServiceSetting", &["TerminalServerMode"]));
        match wmi {
            Ok(Some(row)) if row[0].as_u32().is_some() => Ok(row[0].as_u32() == Some(1)),
            _ => Ok(get_u32(self.reg.as_ref(), TS, "TSAppCompat")? == Some(1)),
        }
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

/// (IPv4 listener on `port`, IPv6 listener on `port`).
fn tcp_listeners(port: u16) -> (bool, bool) {
    let mut v4 = false;
    let mut v6 = false;
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
fn listener_cert_sha1(reg: &dyn Registry) -> Option<[u8; 20]> {
    let from_wmi = Wmi::connect(TS_NAMESPACE)
        .and_then(|w| {
            w.query_first("SELECT SSLCertificateSHA1Hash FROM Win32_TSGeneralSetting WHERE TerminalName='RDP-Tcp'", &["SSLCertificateSHA1Hash"])
        })
        .ok()
        .flatten()
        .and_then(|r| r[0].as_str().and_then(seat_rdp::headless::proto::parse_hex20));
    from_wmi.or_else(|| match reg.get(TS_RDP_TCP, "SSLCertificateSHA1Hash").ok()? {
        RegValue::BinaryB64(b) => b64_decode(&b).filter(|b| b.len() == 20).and_then(|b| b.try_into().ok()),
        _ => None,
    })
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
/// `None` when the event does not exist.
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
    /// Registry values plus the TCP listener table (IPv6-only listener) and the TLS
    /// certificate thumbprint the headless hold pins.
    fn listener(&self) -> Result<ListenerInfo, SysError> {
        let mut l = listener_from_registry(self.reg.as_ref())?;
        let (v4, v6) = tcp_listeners(l.port);
        l.ipv6_only = !v4 && v6;
        l.cert_sha1 = listener_cert_sha1(self.reg.as_ref());
        Ok(l)
    }

    /// Grace days through the RDS WMI provider (RDSH servers only; any failure = unknown).
    fn licensing(&self) -> Result<LicensingInfo, SysError> {
        let mode = licensing_mode_from_registry(self.reg.as_ref())?;
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

    fn service_dll(&self) -> Result<String, SysError> {
        service_dll_from_registry(self.reg.as_ref())
    }

    fn termservice_running(&self) -> bool {
        service_running("TermService")
    }

    /// Opens `Global\TermSrvReadyEvent` with SYNCHRONIZE only (never creates it) and waits.
    /// Without the event: polls until TermService runs and WTS enumerates sessions.
    fn wait_ready(&self, timeout: Duration) -> bool {
        let ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
        if let Some(signalled) = wait_event(ms) {
            return signalled;
        }
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if wait_event(0) == Some(true) || (self.termservice_running() && wts_enumerates()) {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
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
