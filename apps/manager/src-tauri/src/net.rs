// SPDX-License-Identifier: GPL-3.0-only
//! IPv4 addresses of this PC for the per-seat address dialog (spec 10 §7.4: the combo lists
//! only *static* addresses). Read with `GetAdaptersAddresses` (IP Helper, no privilege
//! needed). An address counts as static when its prefix origin is "manual" (not DHCP,
//! not link-local / well-known). Untested on hardware.

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalAddress {
    pub address: String,
    pub adapter: String,
    pub static_address: bool,
}

/// Dotted form of the four address bytes of a `SOCKADDR_IN` (network order).
pub fn dotted(b: [u8; 4]) -> String {
    format!("{}.{}.{}.{}", b[0], b[1], b[2], b[3])
}

/// Addresses worth offering: no loopback, no APIPA (169.254/16), no unspecified.
pub fn offerable(b: [u8; 4]) -> bool {
    !(b[0] == 127 || (b[0] == 169 && b[1] == 254) || b == [0, 0, 0, 0])
}

#[cfg(windows)]
pub fn list() -> Result<Vec<LocalAddress>, seat_proto::ProtoError> {
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST, GetAdaptersAddresses,
        IP_ADAPTER_ADDRESSES_LH,
    };
    use windows_sys::Win32::Networking::WinSock::{AF_INET, IpPrefixOriginManual};

    const ERROR_BUFFER_OVERFLOW: u32 = 111;
    let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
    let mut size: u32 = 16 * 1024;
    let mut buf: Vec<u64> = Vec::new();
    for _ in 0..4 {
        buf = vec![0u64; (size as usize).div_ceil(8)];
        // SAFETY: `buf` is a writable, 8-byte aligned buffer of at least `size` bytes.
        let r = unsafe {
            GetAdaptersAddresses(AF_INET as u32, flags, std::ptr::null(), buf.as_mut_ptr().cast(), &mut size)
        };
        match r {
            0 => break,
            ERROR_BUFFER_OVERFLOW => continue,
            232 /* ERROR_NO_DATA */ => return Ok(Vec::new()),
            e => {
                let mut err = seat_proto::ProtoError::new(seat_proto::ErrorCode::Os, "GetAdaptersAddresses failed");
                err.win32 = Some(e);
                return Err(err);
            }
        }
    }
    let mut out = Vec::new();
    let mut a = buf.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
    // SAFETY: the list was written by GetAdaptersAddresses into `buf`, which outlives the
    // walk; every `Next` pointer is null or points into `buf`.
    unsafe {
        while !a.is_null() {
            let ad = &*a;
            let name = pwstr(ad.FriendlyName);
            let mut u = ad.FirstUnicastAddress;
            while !u.is_null() {
                let ua = &*u;
                let sa = ua.Address.lpSockaddr as *const u8;
                if !sa.is_null() && ua.Address.iSockaddrLength >= 8 && *(sa as *const u16) == AF_INET {
                    let b = [*sa.add(4), *sa.add(5), *sa.add(6), *sa.add(7)];
                    if offerable(b) {
                        out.push(LocalAddress {
                            address: dotted(b),
                            adapter: name.clone(),
                            static_address: ua.PrefixOrigin == IpPrefixOriginManual,
                        });
                    }
                }
                u = ua.Next;
            }
            a = ad.Next;
        }
    }
    Ok(out)
}

#[cfg(windows)]
unsafe fn pwstr(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut n = 0;
    // SAFETY: `p` is a NUL-terminated UTF-16 string from the adapter list.
    unsafe {
        while *p.add(n) != 0 && n < 512 {
            n += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(p, n))
    }
}

/// Virtual switch / VPN adapters: a client on the LAN cannot reach these addresses.
fn is_virtual_adapter(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    ["vethernet", "hyper-v", "virtualbox", "vmware", "wsl", "docker", "tailscale", "zerotier", "vpn"]
        .iter()
        .any(|m| n.contains(m))
}

/// Private IPv4 addresses for the remote seat pairing dialog: physical adapters first (the
/// first one is the recommended address), then by address; duplicates removed. Static or
/// not does not matter here (from Pulsar Split's manager, merge step 9).
pub fn order_lan(list: Vec<(String, std::net::Ipv4Addr)>) -> Vec<String> {
    let mut v: Vec<(bool, std::net::Ipv4Addr)> =
        list.into_iter().filter(|(_, ip)| ip.is_private()).map(|(n, ip)| (is_virtual_adapter(&n), ip)).collect();
    v.sort();
    let mut out: Vec<String> = Vec::new();
    for (_, ip) in v {
        let s = ip.to_string();
        if !out.contains(&s) {
            out.push(s);
        }
    }
    out
}

/// [`order_lan`] over the addresses [`list`] reads.
pub fn lan_addresses() -> Result<Vec<String>, seat_proto::ProtoError> {
    let list = list()?
        .into_iter()
        .filter_map(|a| a.address.parse::<std::net::Ipv4Addr>().ok().map(|ip| (a.adapter, ip)))
        .collect();
    Ok(order_lan(list))
}

#[cfg(not(windows))]
pub fn list() -> Result<Vec<LocalAddress>, seat_proto::ProtoError> {
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_and_formats() {
        assert_eq!(dotted([192, 168, 1, 20]), "192.168.1.20");
        assert!(offerable([10, 0, 0, 5]));
        assert!(!offerable([127, 0, 0, 1]));
        assert!(!offerable([169, 254, 3, 4]));
        assert!(!offerable([0, 0, 0, 0]));
    }

    #[test]
    fn physical_adapters_come_first() {
        let ip = |s: &str| s.parse::<std::net::Ipv4Addr>().unwrap();
        let got = order_lan(vec![
            ("vEthernet (Default Switch)".into(), ip("172.20.0.1")),
            ("Wi-Fi".into(), ip("192.168.1.20")),
            ("Ethernet".into(), ip("10.0.0.5")),
            ("Ethernet 2".into(), ip("8.8.8.8")),
            ("Wi-Fi".into(), ip("192.168.1.20")),
        ]);
        assert_eq!(got, ["10.0.0.5", "192.168.1.20", "172.20.0.1"]);
    }

    #[test]
    fn lists_without_error() {
        // Any PC has at least a loopback adapter; the call itself must work unprivileged.
        assert!(list().is_ok());
    }
}
