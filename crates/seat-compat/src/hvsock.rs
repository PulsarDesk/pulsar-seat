// SPDX-License-Identifier: GPL-3.0-only
//! Hyper-V sockets (`AF_HYPERV`) for the VM seat guest link (hyperv-seats 2026-11-17).
//!
//! Public, documented interface ("Make your own integration services", Microsoft Learn):
//! the host listens on `HV_GUID_WILDCARD` + a service GUID registered under
//! `...\Virtualization\GuestCommunicationServices`; the guest connects to `HV_GUID_PARENT` +
//! the same service GUID. The peer's VM id comes with `accept`. No network is involved.
//!
//! HYPOTHESIS: not run on a Hyper-V host yet.

use std::io;

/// `AF_HYPERV`.
pub const AF_HYPERV: u16 = 34;
/// `HV_PROTOCOL_RAW`.
pub const HV_PROTOCOL_RAW: i32 = 1;
/// `HV_GUID_PARENT`: the host, seen from a guest.
pub const HV_GUID_PARENT: &str = "a42e7cda-d03f-480c-9cc2-a4de20abb878";

/// A GUID string (`xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`, braces allowed) as the 16 bytes of
/// a Windows `GUID` in memory order (data1..3 little endian).
pub fn guid_bytes(s: &str) -> Option<[u8; 16]> {
    let t = s.trim().trim_start_matches('{').trim_end_matches('}');
    let parts: Vec<&str> = t.split('-').collect();
    let lens = [8, 4, 4, 4, 12];
    if parts.len() != 5 || parts.iter().zip(lens).any(|(p, l)| p.len() != l || !p.chars().all(|c| c.is_ascii_hexdigit())) {
        return None;
    }
    let d1 = u32::from_str_radix(parts[0], 16).ok()?;
    let d2 = u16::from_str_radix(parts[1], 16).ok()?;
    let d3 = u16::from_str_radix(parts[2], 16).ok()?;
    let rest = format!("{}{}", parts[3], parts[4]);
    let mut out = [0u8; 16];
    out[..4].copy_from_slice(&d1.to_le_bytes());
    out[4..6].copy_from_slice(&d2.to_le_bytes());
    out[6..8].copy_from_slice(&d3.to_le_bytes());
    for i in 0..8 {
        out[8 + i] = u8::from_str_radix(&rest[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// The 16 bytes of a Windows `GUID` as a lower-case string.
pub fn guid_string(b: &[u8; 16]) -> String {
    let d1 = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
    let d2 = u16::from_le_bytes([b[4], b[5]]);
    let d3 = u16::from_le_bytes([b[6], b[7]]);
    format!(
        "{d1:08x}-{d2:04x}-{d3:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    )
}

/// `SOCKADDR_HV`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SockaddrHv {
    pub family: u16,
    pub reserved: u16,
    pub vm_id: [u8; 16],
    pub service_id: [u8; 16],
}

fn bad_guid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "invalid GUID")
}

impl SockaddrHv {
    pub fn new(vm_id: &str, service_id: &str) -> io::Result<Self> {
        Ok(Self { family: AF_HYPERV, reserved: 0, vm_id: guid_bytes(vm_id).ok_or_else(bad_guid)?, service_id: guid_bytes(service_id).ok_or_else(bad_guid)? })
    }
    /// Listen address: any VM (`HV_GUID_WILDCARD`).
    pub fn wildcard(service_id: &str) -> io::Result<Self> {
        Ok(Self { family: AF_HYPERV, reserved: 0, vm_id: [0; 16], service_id: guid_bytes(service_id).ok_or_else(bad_guid)? })
    }
}

#[cfg(windows)]
pub use imp::{HvListener, HvStream};

#[cfg(windows)]
mod imp {
    use super::*;
    use std::sync::Once;
    use windows_sys::Win32::Networking::WinSock::{
        INVALID_SOCKET, SOCK_STREAM, SOCKADDR, SOCKET, SOCKET_ERROR, WSADATA, WSAGetLastError, WSAStartup, accept, bind,
        closesocket, connect, listen, recv, send, socket,
    };

    fn startup() {
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            // SAFETY: WSADATA is a plain C struct; all-zero is a valid value.
            let mut d: WSADATA = unsafe { std::mem::zeroed() };
            // SAFETY: plain Winsock initialisation, once per process.
            unsafe { WSAStartup(0x0202, &mut d) };
        });
    }

    fn last() -> io::Error {
        // SAFETY: trivially safe.
        io::Error::from_raw_os_error(unsafe { WSAGetLastError() })
    }

    fn new_socket() -> io::Result<SOCKET> {
        startup();
        // SAFETY: plain socket creation.
        let s = unsafe { socket(i32::from(AF_HYPERV), SOCK_STREAM, HV_PROTOCOL_RAW) };
        if s == INVALID_SOCKET { Err(last()) } else { Ok(s) }
    }

    /// One connection. Reads and writes may come from different threads.
    pub struct HvStream(SOCKET);

    // SAFETY: a Winsock SOCKET is a kernel handle usable from any thread.
    unsafe impl Send for HvStream {}
    unsafe impl Sync for HvStream {}

    impl HvStream {
        /// Guest side: connect to the host's service.
        pub fn connect_parent(service_id: &str) -> io::Result<Self> {
            let addr = SockaddrHv::new(HV_GUID_PARENT, service_id)?;
            let s = new_socket()?;
            // SAFETY: `addr` is a valid SOCKADDR_HV of the size given.
            let r = unsafe { connect(s, &addr as *const SockaddrHv as *const SOCKADDR, std::mem::size_of::<SockaddrHv>() as i32) };
            if r == SOCKET_ERROR {
                let e = last();
                // SAFETY: closing our own socket.
                unsafe { closesocket(s) };
                return Err(e);
            }
            Ok(Self(s))
        }

        pub fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
            // SAFETY: `buf` is valid for `len` bytes.
            let n = unsafe { recv(self.0, buf.as_mut_ptr(), buf.len().min(i32::MAX as usize) as i32, 0) };
            if n == SOCKET_ERROR { Err(last()) } else { Ok(n as usize) }
        }

        pub fn write_all(&self, mut data: &[u8]) -> io::Result<()> {
            while !data.is_empty() {
                // SAFETY: `data` is valid for `len` bytes.
                let n = unsafe { send(self.0, data.as_ptr(), data.len().min(i32::MAX as usize) as i32, 0) };
                if n == SOCKET_ERROR {
                    return Err(last());
                }
                data = &data[n as usize..];
            }
            Ok(())
        }
    }

    impl Drop for HvStream {
        fn drop(&mut self) {
            // SAFETY: closing our own socket once.
            unsafe { closesocket(self.0) };
        }
    }

    /// Host side listener.
    pub struct HvListener(SOCKET);

    // SAFETY: as for HvStream.
    unsafe impl Send for HvListener {}

    impl HvListener {
        pub fn bind(service_id: &str) -> io::Result<Self> {
            let addr = SockaddrHv::wildcard(service_id)?;
            let s = new_socket()?;
            // SAFETY: valid address of the size given; plain listen.
            let ok = unsafe {
                bind(s, &addr as *const SockaddrHv as *const SOCKADDR, std::mem::size_of::<SockaddrHv>() as i32) != SOCKET_ERROR
                    && listen(s, 16) != SOCKET_ERROR
            };
            if !ok {
                let e = last();
                // SAFETY: closing our own socket.
                unsafe { closesocket(s) };
                return Err(e);
            }
            Ok(Self(s))
        }

        /// Next connection and the VM id of its guest.
        pub fn accept(&self) -> io::Result<(HvStream, String)> {
            let mut addr: SockaddrHv = SockaddrHv { family: 0, reserved: 0, vm_id: [0; 16], service_id: [0; 16] };
            let mut len = std::mem::size_of::<SockaddrHv>() as i32;
            // SAFETY: `addr` / `len` describe a writable SOCKADDR_HV.
            let s = unsafe { accept(self.0, &mut addr as *mut SockaddrHv as *mut SOCKADDR, &mut len) };
            if s == INVALID_SOCKET {
                return Err(last());
            }
            Ok((HvStream(s), guid_string(&addr.vm_id)))
        }
    }

    impl Drop for HvListener {
        fn drop(&mut self) {
            // SAFETY: closing our own socket once.
            unsafe { closesocket(self.0) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guids_round_trip_in_memory_order() {
        let b = guid_bytes(HV_GUID_PARENT).unwrap();
        assert_eq!(&b[..4], &[0xda, 0x7c, 0x2e, 0xa4]);
        assert_eq!(guid_string(&b), HV_GUID_PARENT);
        assert_eq!(guid_bytes("{A42E7CDA-D03F-480C-9CC2-A4DE20ABB878}"), Some(b));
        assert!(guid_bytes("a42e7cda-d03f-480c-9cc2").is_none());
        assert!(guid_bytes("zzzzzzzz-d03f-480c-9cc2-a4de20abb878").is_none());
        let a = SockaddrHv::wildcard(seat_core::remote::vm::GUEST_LINK_SERVICE_ID).unwrap();
        assert_eq!(a.family, AF_HYPERV);
        assert_eq!(a.vm_id, [0; 16]);
        assert_eq!(std::mem::size_of::<SockaddrHv>(), 36);
    }
}
