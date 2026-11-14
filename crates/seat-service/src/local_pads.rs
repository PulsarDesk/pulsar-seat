// SPDX-License-Identifier: GPL-3.0-only
//! Seat-owned virtual pads for local seats (round 4, 2026-11-14): what the service hands
//! to `seat_input::local_pads::LocalPadHub` every tick.
//!
//! A seat gets virtual pads when `LocalSeat::virtual_pads` is on (default off), the seat
//! has a session with a signed-in user, and a pad unit is routed to that seat alone (its
//! effective mask, or a pending hotkey move, names exactly this seat). The hub (Windows:
//! `runtime.rs`) runs on the remote engine's one `PadManager`, so local and remote pads share
//! the ownership registry, the orphan cleanup at service start / stop, and the securing
//! pass (device SDDL with the seat user's SID + `DEVPKEY_Device_SessionId`).
//!
//! Untested on hardware (HYPOTHESIS): every Windows path behind the hub.

use seat_core::config::SeatKind;
use seat_core::device::DeviceKind;
use seat_input::gamepad::pad_family;
use seat_input::local_pads::{LocalPadSeat, RoutedPad};
use seat_input::units::{input_info, vid_pid};

use crate::service::State;

/// Local seats with virtual pads on and their routed pads, in seat order; pads per seat in
/// unit-key order (stable slots).
pub(crate) fn targets(st: &State) -> Vec<LocalPadSeat> {
    let effective = st.inventory.effective(&st.config);
    let mut out = Vec::new();
    for seat in st.config.seats.iter().filter(|s| s.kind == SeatKind::Local && s.local_settings().virtual_pads) {
        let Some(slot) = st.seats.get(&seat.number) else { continue };
        let (Some(session_id), Some(owner_sid)) = (slot.rt.session_id, slot.rt.user_sid.clone()) else { continue };
        let mut pads: Vec<RoutedPad> = Vec::new();
        for d in st.inventory.live().filter(|d| d.kind == DeviceKind::Hid) {
            let Some(family) = pad_family(d) else { continue };
            let mask = st.input_overrides.get(&d.key).copied().or_else(|| effective.iter().find(|e| e.key == d.key).map(|e| e.mask));
            if mask.and_then(|m| m.single()) != Some(seat.number) {
                continue;
            }
            let Some(info) = input_info(d) else { continue };
            if info.interface_path.is_empty() {
                continue;
            }
            let (vendor_id, product_id) = if info.vendor_id != 0 {
                (info.vendor_id, info.product_id)
            } else {
                let ids: Vec<String> = d.hardware_id.iter().cloned().collect();
                vid_pid(&ids)
            };
            pads.push(RoutedPad {
                unit_key: d.key.to_string(),
                interface_path: info.interface_path.clone(),
                family,
                vendor_id,
                product_id,
            });
        }
        if pads.is_empty() {
            continue;
        }
        pads.sort_by(|a, b| a.unit_key.cmp(&b.unit_key));
        out.push(LocalPadSeat { seat: seat.number.get(), session_id, owner_sid, pads });
    }
    out
}

impl crate::service::Service {
    /// [`targets`] under the state lock.
    pub(crate) fn local_pad_targets(&self) -> Vec<LocalPadSeat> {
        targets(&self.lock())
    }
}
