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
    fn lists_without_error() {
        // Any PC has at least a loopback adapter; the call itself must work unprivileged.
        assert!(list().is_ok());
    }
}
