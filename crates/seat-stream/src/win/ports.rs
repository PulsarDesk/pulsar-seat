// SPDX-License-Identifier: GPL-3.0-only
//! [`ListenerProbe`] via IP Helper (`GetExtendedTcpTable` listeners, `GetExtendedUdpTable`),
//! IPv4 and IPv6. Read-only; needs no privilege.

use crate::ports::ListenerProbe;
use crate::StreamError;
use seat_core::remote::ports::Proto;
use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, NO_ERROR};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    GetExtendedTcpTable, GetExtendedUdpTable, MIB_TCP6TABLE_OWNER_PID, MIB_TCPTABLE_OWNER_PID,
    MIB_UDP6TABLE_OWNER_PID, MIB_UDPTABLE_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER, UDP_TABLE_OWNER_PID,
};
use windows_sys::Win32::Networking::WinSock::{AF_INET, AF_INET6};

#[derive(Debug, Default, Clone, Copy)]
pub struct IpHelperProbe;

/// Calls `f(buf, &mut size)` until the buffer is big enough. `u64` storage keeps the
/// tables 8-byte aligned.
fn fetch(mut f: impl FnMut(*mut core::ffi::c_void, &mut u32) -> u32, what: &str) -> Result<Vec<u64>, StreamError> {
    let mut size = 0u32;
    let mut buf: Vec<u64> = Vec::new();
    for _ in 0..8 {
        let rc = f(buf.as_mut_ptr().cast(), &mut size);
        match rc {
            NO_ERROR => return Ok(buf),
            ERROR_INSUFFICIENT_BUFFER => buf = vec![0u64; (size as usize).div_ceil(8) + 16],
            e => return Err(StreamError::io(what, std::io::Error::from_raw_os_error(e as i32))),
        }
    }
    Err(StreamError::Io(format!("{what}: table keeps growing")))
}

fn port(dw: u32) -> u16 {
    u16::from_be((dw & 0xFFFF) as u16)
}

impl ListenerProbe for IpHelperProbe {
    fn listening(&self) -> Result<Vec<(u16, Proto)>, StreamError> {
        let mut out = Vec::new();
        for af in [AF_INET, AF_INET6] {
            let af = u32::from(af);
            // SAFETY (all four calls): buffer pointer and size are kept consistent by `fetch`.
            let tcp = fetch(
                |p, s| unsafe { GetExtendedTcpTable(p, s, 0, af, TCP_TABLE_OWNER_PID_LISTENER, 0) },
                "GetExtendedTcpTable",
            )?;
            let udp = fetch(|p, s| unsafe { GetExtendedUdpTable(p, s, 0, af, UDP_TABLE_OWNER_PID, 0) }, "GetExtendedUdpTable")?;
            // SAFETY: on success the buffers hold the documented table structs; rows are read
            // through raw pointers within dwNumEntries.
            unsafe {
                if af == u32::from(AF_INET) {
                    let t = tcp.as_ptr().cast::<MIB_TCPTABLE_OWNER_PID>();
                    let rows = std::ptr::addr_of!((*t).table).cast::<windows_sys::Win32::NetworkManagement::IpHelper::MIB_TCPROW_OWNER_PID>();
                    for i in 0..(*t).dwNumEntries as usize {
                        out.push((port((*rows.add(i)).dwLocalPort), Proto::Tcp));
                    }
                    let u = udp.as_ptr().cast::<MIB_UDPTABLE_OWNER_PID>();
                    let rows = std::ptr::addr_of!((*u).table).cast::<windows_sys::Win32::NetworkManagement::IpHelper::MIB_UDPROW_OWNER_PID>();
                    for i in 0..(*u).dwNumEntries as usize {
                        out.push((port((*rows.add(i)).dwLocalPort), Proto::Udp));
                    }
                } else {
                    let t = tcp.as_ptr().cast::<MIB_TCP6TABLE_OWNER_PID>();
                    let rows = std::ptr::addr_of!((*t).table).cast::<windows_sys::Win32::NetworkManagement::IpHelper::MIB_TCP6ROW_OWNER_PID>();
                    for i in 0..(*t).dwNumEntries as usize {
                        out.push((port((*rows.add(i)).dwLocalPort), Proto::Tcp));
                    }
                    let u = udp.as_ptr().cast::<MIB_UDP6TABLE_OWNER_PID>();
                    let rows = std::ptr::addr_of!((*u).table).cast::<windows_sys::Win32::NetworkManagement::IpHelper::MIB_UDP6ROW_OWNER_PID>();
                    for i in 0..(*u).dwNumEntries as usize {
                        out.push((port((*rows.add(i)).dwLocalPort), Proto::Udp));
                    }
                }
            }
        }
        out.sort_unstable_by_key(|(p, proto)| (*p, matches!(proto, Proto::Udp)));
        out.dedup();
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sees_own_listener() {
        // Read-only query; binds a loopback listener on an ephemeral port to have something
        // known to look for.
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let p = l.local_addr().unwrap().port();
        let got = IpHelperProbe.listening().unwrap();
        assert!(got.contains(&(p, Proto::Tcp)), "port {p} not in {} entries", got.len());
        assert_eq!(super::port(0x5000), 80);
    }
}
