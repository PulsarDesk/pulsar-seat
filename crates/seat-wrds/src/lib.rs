// SPDX-License-Identifier: GPL-3.0-only
//! Pulsar Seat Remote Desktop protocol provider (`pulsar-seat-wrds.dll`). Added 2026-11-17
//! (package remote-idd-hdr). Written from the public Microsoft documentation of the
//! Remote Desktop Protocol Provider API (`IWRdsProtocolManager`, `IWRdsProtocolListener`,
//! `IWRdsProtocolConnection`, `IWRdsProtocolLicenseConnection`,
//! `IWRdsProtocolLogonErrorRedirector`, `IWRdsWddmIddProps` / `IWRdsWddmIddProps1`) and the
//! Windows SDK headers, through the `windows` crate bindings. No third-party code.
//!
//! What it does: TermService loads this in-process COM server for the WinStation
//! `PulsarSeat-Idd` (registered by the service, `seat_session::wrds`). The provider connects
//! to the service's pipe (`seat_proto::wrds`) and offers TermService exactly the
//! connections the service asks for, one per remote seat start. Each connection names our
//! IddCx remote-role driver (`PulsarSplit_IndirectDisplay`, `drivers/seat-idd`) as the
//! session's display driver, so the seat session's monitor is ours: mode changes
//! (resolution, refresh, super-sampling, scale) and HDR metadata go through our IDD's
//! control interface instead of RdpIdd.
//!
//! Layout: [`state`] (pure, tested everywhere), `com` (COM objects, DLL exports) and `pipe`
//! (pipe client), both Windows only. See README.md for the call sequence and the
//! HYPOTHESIS list (nothing ran: needs Windows Server with the RD Session Host role).

pub mod state;

#[cfg(windows)]
mod com;
#[cfg(windows)]
mod pipe;

#[cfg(windows)]
pub use com::{DllCanUnloadNow, DllGetClassObject};

/// Version string sent in `hello`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Encode `s` into a fixed UTF-16 field with a terminating NUL. Returns `false` (field left
/// all zero) when it does not fit.
pub fn put_wide(dst: &mut [u16], s: &str) -> bool {
    dst.iter_mut().for_each(|c| *c = 0);
    let w: Vec<u16> = s.encode_utf16().collect();
    if w.len() >= dst.len() {
        return false;
    }
    dst[..w.len()].copy_from_slice(&w);
    true
}

/// Desktop color depth TermService gets in `WTS_CLIENT_DATA::ColorDepth` (32 bits per
/// pixel). HYPOTHESIS: the field takes the bits-per-pixel value for a non-RDP protocol.
pub const COLOR_DEPTH: u16 = 32;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_fields_are_terminated_and_bounded() {
        let mut f = [7u16; 6];
        assert!(put_wide(&mut f, "seat"));
        assert_eq!(&f[..5], &[b's' as u16, b'e' as u16, b'a' as u16, b't' as u16, 0]);
        assert!(!put_wide(&mut f, "toolong"));
        assert!(f.iter().all(|c| *c == 0));
        assert!(!put_wide(&mut f, "sixsix"), "no room for the NUL");
    }
}
