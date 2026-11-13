// SPDX-License-Identifier: GPL-3.0-only
//! Windows side of [`crate::listener`]: TermService's process id and the IPv4 TCP
//! listeners with their owning processes. Read-only, needs no privileges.
//! **Untested on hardware** (needs an RDS host); the table read runs in a unit test.

use std::net::Ipv4Addr;

use windows_sys::Win32::NetworkManagement::IpHelper::{
    GetExtendedTcpTable, MIB_TCP_STATE_ESTAB, MIB_TCP_STATE_LISTEN, MIB_TCPROW_OWNER_PID, MIB_TCPTABLE_OWNER_PID,
    TCP_TABLE_OWNER_PID_CONNECTIONS, TCP_TABLE_OWNER_PID_LISTENER,
};
use windows_sys::Win32::Networking::WinSock::AF_INET;
use windows_sys::Win32::System::Services::{
    CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceStatusEx, SC_MANAGER_CONNECT, SC_STATUS_PROCESS_INFO,
    SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_STATUS_PROCESS,
};

use super::wide;
use crate::error::SysError;
use crate::listener::{TcpConnRow, TcpListenerRow, connection_untrusted_reason, listener_untrusted_reason};

const ERROR_INSUFFICIENT_BUFFER: u32 = 122;

/// Process id of a running service, `None` if it is not running.
pub fn service_pid(name: &str) -> Option<u32> {
    // SAFETY: plain SCM calls; handles closed below; out struct sized correctly.
    unsafe {
        let scm = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT);
        if scm.is_null() {
            return None;
        }
        let n = wide(name);
        let svc = OpenServiceW(scm, n.as_ptr(), SERVICE_QUERY_STATUS);
        let mut st: SERVICE_STATUS_PROCESS = std::mem::zeroed();
        let mut need = 0u32;
        let ok = !svc.is_null()
            && QueryServiceStatusEx(
                svc,
                SC_STATUS_PROCESS_INFO,
                (&mut st as *mut SERVICE_STATUS_PROCESS).cast(),
                std::mem::size_of::<SERVICE_STATUS_PROCESS>() as u32,
                &mut need,
            ) != 0;
        if !svc.is_null() {
            CloseServiceHandle(svc);
        }
        CloseServiceHandle(scm);
        (ok && st.dwCurrentState == SERVICE_RUNNING && st.dwProcessId != 0).then_some(st.dwProcessId)
    }
}

/// Every IPv4 TCP listener with its owning process.
pub fn tcp_listeners() -> Result<Vec<TcpListenerRow>, SysError> {
    Ok(tcp_table(TCP_TABLE_OWNER_PID_LISTENER)?
        .iter()
        .filter(|r| r.dwState == MIB_TCP_STATE_LISTEN as u32)
        // dwLocalAddr / dwLocalPort are in network byte order.
        .map(|r| TcpListenerRow {
            addr: Ipv4Addr::from(r.dwLocalAddr.to_ne_bytes()),
            port: u16::from_be(r.dwLocalPort as u16),
            pid: r.dwOwningPid,
        })
        .collect())
}

/// Every established IPv4 TCP connection with its owning process.
pub fn tcp_connections() -> Result<Vec<TcpConnRow>, SysError> {
    Ok(tcp_table(TCP_TABLE_OWNER_PID_CONNECTIONS)?
        .iter()
        .filter(|r| r.dwState == MIB_TCP_STATE_ESTAB as u32)
        .map(|r| TcpConnRow {
            local: (Ipv4Addr::from(r.dwLocalAddr.to_ne_bytes()), u16::from_be(r.dwLocalPort as u16)),
            remote: (Ipv4Addr::from(r.dwRemoteAddr.to_ne_bytes()), u16::from_be(r.dwRemotePort as u16)),
            pid: r.dwOwningPid,
        })
        .collect())
}

fn tcp_table(class: i32) -> Result<Vec<MIB_TCPROW_OWNER_PID>, SysError> {
    let mut size = 0u32;
    let mut buf: Vec<u64> = Vec::new();
    for _ in 0..4 {
        let p = if buf.is_empty() { std::ptr::null_mut() } else { buf.as_mut_ptr().cast() };
        // SAFETY: `p` is null (size query) or holds `size` bytes, u64-aligned.
        let rc = unsafe { GetExtendedTcpTable(p, &mut size, 0, u32::from(AF_INET), class, 0) };
        if rc == 0 && !buf.is_empty() {
            let t = buf.as_ptr() as *const MIB_TCPTABLE_OWNER_PID;
            // SAFETY: the API filled a MIB_TCPTABLE_OWNER_PID with `dwNumEntries` rows.
            let rows = unsafe {
                let n = (*t).dwNumEntries as usize;
                std::slice::from_raw_parts(std::ptr::addr_of!((*t).table) as *const MIB_TCPROW_OWNER_PID, n)
            };
            return Ok(rows.to_vec());
        }
        if rc != 0 && rc != ERROR_INSUFFICIENT_BUFFER {
            return Err(SysError::Win32 { api: "GetExtendedTcpTable", code: rc });
        }
        buf = vec![0u64; (size as usize).div_ceil(8) + 64];
        size = (buf.len() * 8) as u32;
    }
    Err(SysError::Other("GetExtendedTcpTable: table keeps growing".into()))
}

/// Fails unless every listener reachable at `addr:port` belongs to TermService.
pub fn verify_rdp_listener(addr: Ipv4Addr, port: u16) -> Result<(), SysError> {
    let pid = service_pid("TermService");
    let rows = tcp_listeners()?;
    match listener_untrusted_reason(&rows, addr, port, pid) {
        None => Ok(()),
        Some(why) => Err(SysError::Other(format!("RDP listener {addr}:{port} not trusted: {why}"))),
    }
}

/// Fails unless every connection this process (`client_pid`) holds to `addr:port` ends at
/// a TermService socket. Run on `OnConnected` to close the TOCTOU window of
/// [`verify_rdp_listener`].
pub fn verify_rdp_peer(addr: Ipv4Addr, port: u16, client_pid: u32) -> Result<(), SysError> {
    let pid = service_pid("TermService");
    let rows = tcp_connections()?;
    match connection_untrusted_reason(&rows, client_pid, addr, port, pid) {
        None => Ok(()),
        Some(why) => Err(SysError::Other(format!("RDP peer {addr}:{port} not trusted: {why}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listener_table_reads() {
        // Read-only. A sandboxed test run may see a filtered table, so our own listener is
        // checked only when it is listed at all.
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let rows = tcp_listeners().unwrap();
        if let Some(me) = rows.iter().find(|r| r.port == port) {
            assert_eq!(me.addr, Ipv4Addr::LOCALHOST);
            assert_eq!(me.pid, std::process::id());
        }
        assert!(rows.iter().all(|r| r.port != 0));
    }
}
