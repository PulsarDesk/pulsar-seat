// SPDX-License-Identifier: GPL-3.0-only
//! RDP device redirection per seat: what is left here, and where the rules live.
//!
//! **Single source of truth for clipboard / drive / smart-card redirection:**
//! `seat_rdp::policy::host_redirection` (the service resolves it into
//! `HostAssignment.redirection`; the seat host's IronRDP client applies it). This module
//! used to hold an older rule set (`seat_redirection`: drives always off, every reader shared
//! with the seat). It was removed on 2026-11-13; its two objections are answered by the new
//! rules (architecture §4.8):
//!
//! - *"Drive redirection adds a second, ungated path (`\\tsclient\X`)."* The redirected
//!   path is gated by the same ownership: only volumes whose effective mask is exactly the
//!   seat are offered, each pinned to its volume GUID (a letter that moves to another volume
//!   is refused), removed from the session when the assignment changes, never the system
//!   drive, and only FAT-family volumes (no ACLs, so the seat host's token cannot read more
//!   than the seat user could). The `session_drive_letters` / minifilter gates still decide
//!   what the seat session reaches directly; a volume they moved into the seat's own logon
//!   session has no global letter and is not redirected (`drive_without_global_letter`).
//!   Drive redirection is off by default (`SeatRedirect.drives`).
//! - *"Readers cannot be split per seat."* The host filters the WinSCard reader list and
//!   refuses calls naming another reader, matched by device instance id
//!   (`seat_rdp::channels::rdpdr`), so a seat sees only its readers.
//!
//! What stays here: the cameras a seat may offer over the camera-redirection channel
//! (MS-RDPECAM) when it owns them; HYPOTHESIS that a loopback session cannot open local
//! cameras directly through the frame server — untested on hardware.

use seat_core::assign::Effective;
use seat_core::device::{Device, DeviceKind};
use seat_core::ids::{DeviceKey, SeatNumber};

/// Cameras the seat may offer over camera redirection: present, mask contains the seat.
/// Seat 1 on the console needs none.
pub fn seat_cameras(seat: SeatNumber, effective: &[Effective], devices: &[Device]) -> Vec<DeviceKey> {
    if seat.is_console() {
        return Vec::new();
    }
    let present = |k: &DeviceKey| devices.iter().any(|d| &d.key == k && d.present);
    effective
        .iter()
        .filter(|e| e.kind == DeviceKind::Webcam && e.mask.contains(seat) && present(&e.key))
        .map(|e| e.key.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use seat_core::assign::MaskSource;
    use seat_core::device::DeviceDetails;
    use seat_core::ids::SeatMask;

    fn dev(k: &str, kind: DeviceKind) -> Device {
        Device {
            key: DeviceKey::new(k).unwrap(),
            kind,
            name: k.into(),
            label: None,
            instance_id: None,
            container_id: None,
            parent_hub: None,
            present: true,
            hardware_id: None,
            serial: None,
            warnings: vec![],
            details: DeviceDetails::None,
        }
    }

    fn eff(k: &str, kind: DeviceKind, mask: SeatMask) -> Effective {
        Effective { key: DeviceKey::new(k).unwrap(), kind, mask, source: MaskSource::Default }
    }

    #[test]
    fn cameras_of_the_seat() {
        let s2 = SeatNumber::new(2).unwrap();
        let devices = vec![dev("cam;inst=B", DeviceKind::Webcam), dev("cam;inst=C", DeviceKind::Webcam), dev("stor;inst=D", DeviceKind::Storage)];
        let effective = vec![
            eff("cam;inst=B", DeviceKind::Webcam, SeatMask::from_seats([s2])),
            eff("cam;inst=C", DeviceKind::Webcam, SeatMask::from_seats([SeatNumber::new(3).unwrap()])),
            eff("stor;inst=D", DeviceKind::Storage, SeatMask::from_seats([s2])),
        ];
        assert_eq!(seat_cameras(s2, &effective, &devices), vec![DeviceKey::new("cam;inst=B").unwrap()]);
        assert!(seat_cameras(SeatNumber::CONSOLE, &effective, &devices).is_empty());
    }
}
