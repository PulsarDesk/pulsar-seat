// SPDX-License-Identifier: GPL-3.0-only
//! Physical game pads of the PC, service side.
//!
//! 1. Seat-owned virtual pads for local seats (round 4, 2026-11-14): what the service
//!    hands to `seat_input::local_pads::LocalPadHub` every tick ([`targets`]). A seat gets
//!    virtual pads when `LocalSeat::virtual_pads` is on (default off), the seat has a
//!    session with a signed-in user, and a pad unit is routed to that seat alone (its
//!    effective mask, or a pending hotkey move, names exactly this seat). Since 2026-11-17
//!    every HID pad is read (PlayStation, Switch Pro, Xbox HID collections, generic HID),
//!    not only PlayStation pads.
//! 2. Pad scoping (2026-11-17, [`scope_pads`]): which session each physical pad's device
//!    nodes belong to (`seat_input::pad_scope`, setting `input.pad_device_scope`).
//! 3. The shim seat map for local seats with their own session ([`local_seat_map`]): their
//!    routed HID units (and the parents, where the XInput interface of Xbox pads lives) are
//!    the seat's `PadInstances`, so XInput / HID opens in shimmed processes follow the
//!    seat (x64 / x86 shim only).
//! 4. SDL hints for a seat's processes ([`sdl_env`]).
//!
//! The hub (Windows: `runtime.rs`) runs on the remote engine's one `PadManager`, so local
//! and remote pads share the ownership registry, the orphan cleanup at service start /
//! stop, and the securing pass (device SDDL with the seat user's SID +
//! `DEVPKEY_Device_SessionId`).
//!
//! Untested on hardware (HYPOTHESIS): every Windows path behind these.

use seat_core::config::SeatKind;
use seat_core::device::{Device, DeviceKind};
use seat_core::SeatNumber;
use seat_input::gamepad::pad_family;
use seat_input::local_pads::{LocalPadSeat, PadBus, RoutedPad, local_seat_pad_id};
use seat_input::pad_scope::{PadNodes, ScopeOwner, sdl_ignore_list};
use seat_input::units::{input_info, vid_pid};

use crate::service::State;

/// USB ids of a pad unit (`InputInfo`, else the hardware ids).
fn ids_of(d: &Device) -> Option<(u16, u16)> {
    let info = input_info(d)?;
    if info.vendor_id != 0 {
        return Some((info.vendor_id, info.product_id));
    }
    let ids: Vec<String> = d.hardware_id.iter().cloned().collect();
    Some(vid_pid(&ids))
}

/// The single seat a pad unit is routed to now (pending hotkey move first).
fn routed_seat(st: &State, effective: &[seat_core::assign::Effective], d: &Device) -> Option<SeatNumber> {
    let mask = st.input_overrides.get(&d.key).copied().or_else(|| effective.iter().find(|e| e.key == d.key).map(|e| e.mask));
    mask.and_then(|m| m.single())
}

/// Live HID pad units with their routed seat.
fn pads(st: &State) -> Vec<(&Device, Option<SeatNumber>)> {
    let effective = st.inventory.effective(&st.config);
    st.inventory
        .live()
        .filter(|d| d.kind == DeviceKind::Hid && pad_family(d).is_some())
        .map(|d| (d, routed_seat(st, &effective, d)))
        .collect()
}

/// Windows users of kiosk seats that run as their own user (`desktops` backend,
/// `SessionBackend::kiosk_users`): seat -> user SID. A kiosk seat shares the console
/// session, so `rt.user_sid` holds the console user; the seat's shell and its apps run as
/// this user instead, and the seat's pads must belong to it.
pub(crate) type KioskSids = std::collections::BTreeMap<SeatNumber, String>;

/// `(session, user SID)` of a running local seat with a signed-in user. A kiosk seat with
/// its own Windows user (`kiosk`) gets that user's SID with the (console) session id.
fn seat_owner(st: &State, kiosk: &KioskSids, seat: SeatNumber) -> Option<ScopeOwner> {
    let cfg = st.config.seats.iter().find(|s| s.number == seat)?;
    if cfg.kind != SeatKind::Local {
        return None;
    }
    let slot = st.seats.get(&seat)?;
    let session_id = slot.rt.session_id?;
    let user_sid = match kiosk.get(&seat) {
        Some(sid) => sid.clone(),
        None => slot.rt.user_sid.clone()?,
    };
    Some(ScopeOwner { session_id, user_sid })
}

/// Local seats with virtual pads on and their routed pads, in seat order; pads per seat in
/// unit-key order (stable slots).
pub(crate) fn targets(st: &State, kiosk: &KioskSids) -> Vec<LocalPadSeat> {
    let all = pads(st);
    let mut out = Vec::new();
    for seat in st.config.seats.iter().filter(|s| s.kind == SeatKind::Local && s.local_settings().virtual_pads) {
        let Some(owner) = seat_owner(st, kiosk, seat.number) else { continue };
        let mut list: Vec<RoutedPad> = Vec::new();
        for (d, routed) in &all {
            if *routed != Some(seat.number) {
                continue;
            }
            let (Some(family), Some(info)) = (pad_family(d), input_info(d)) else { continue };
            if info.interface_path.is_empty() {
                continue;
            }
            let (vendor_id, product_id) = ids_of(d).unwrap_or_default();
            list.push(RoutedPad {
                unit_key: d.key.to_string(),
                interface_path: info.interface_path.clone(),
                family,
                vendor_id,
                product_id,
                bus: PadBus::from_info(&info.bus),
            });
        }
        if list.is_empty() {
            continue;
        }
        list.sort_by(|a, b| a.unit_key.cmp(&b.unit_key));
        out.push(LocalPadSeat { seat: seat.number.get(), session_id: owner.session_id, owner_sid: owner.user_sid, pads: list });
    }
    out
}

/// Every physical pad's device nodes (HID collection + parent, via `parent_of`) and its
/// owning local seat, for `seat_input::pad_scope::plan`.
pub(crate) fn scope_pads(st: &State, kiosk: &KioskSids, parent_of: &dyn Fn(&str) -> Option<String>) -> Vec<PadNodes> {
    pads(st)
        .into_iter()
        .filter_map(|(d, routed)| {
            let inst = d.instance_id.clone()?;
            let mut nodes = vec![inst.clone()];
            nodes.extend(parent_of(&inst));
            Some(PadNodes { nodes, seat_owner: routed.and_then(|s| seat_owner(st, kiosk, s)) })
        })
        .collect()
}

/// Shim seat map entries for local seats that have a session of their own (not the
/// console's, not shared with another seat): seat id `local_seat_pad_id(n)`, the seat
/// user, and as `PadInstances` the instance ids of the HID units routed to the seat alone
/// plus the parents of its pads (XUSB interface of Xbox pads).
pub(crate) fn local_seat_map(st: &State, console: Option<u32>, parent_of: &dyn Fn(&str) -> Option<String>) -> Vec<seat_shim_core::SeatMapEntry> {
    let effective = st.inventory.effective(&st.config);
    let mut out = Vec::new();
    for seat in st.config.seats.iter().filter(|s| s.kind == SeatKind::Local) {
        let Some(owner) = seat_owner(st, &KioskSids::new(), seat.number) else { continue };
        if Some(owner.session_id) == console || owner.session_id == 0 {
            continue;
        }
        let shared = st.seats.iter().any(|(n, s)| *n != seat.number && s.rt.session_id == Some(owner.session_id));
        if shared {
            continue;
        }
        let mut prefixes: Vec<String> = Vec::new();
        for d in st.inventory.live().filter(|d| d.kind == DeviceKind::Hid) {
            if routed_seat(st, &effective, d) != Some(seat.number) {
                continue;
            }
            let Some(inst) = d.instance_id.clone() else { continue };
            if pad_family(d).is_some() {
                prefixes.extend(parent_of(&inst));
            }
            prefixes.push(inst);
        }
        prefixes.iter_mut().for_each(|p| *p = p.to_ascii_uppercase());
        prefixes.sort();
        prefixes.dedup();
        out.push(seat_shim_core::SeatMapEntry {
            session_id: owner.session_id,
            seat_id: local_seat_pad_id(seat.number.get()),
            user_sid: owner.user_sid,
            generation: 1,
            pad_instance_prefixes: prefixes,
            // Steam per local seat (compat-tweaks 2026-11-17): the module adds the seat's
            // own `-master_ipc_name_override` to `steam.exe` in this session.
            flags: seat_shim_core::SeatFlags {
                hid_isolation: true,
                xinput_focus: false,
                steam: st.config.settings.apps.steam_per_seat,
                shared_account: false,
            },
        });
    }
    out
}

/// SDL hint environment for the processes the service starts in `seat`: the seat's own
/// `pad_ignore_devices` plus the USB ids of physical pads routed elsewhere (or nowhere)
/// that no pad of this seat shares.
pub(crate) fn sdl_env(st: &State, seat: SeatNumber) -> Vec<(String, String)> {
    let Some(cfg) = st.config.seats.iter().find(|s| s.number == seat) else { return Vec::new() };
    let mut own = Vec::new();
    let mut others = Vec::new();
    for (d, routed) in pads(st) {
        let Some(ids) = ids_of(d) else { continue };
        if routed == Some(seat) { own.push(ids) } else { others.push(ids) }
    }
    if cfg.kind == SeatKind::Local && cfg.local_settings().virtual_pads {
        // The seat's virtual pads mirror its own pads; their ids must stay visible.
        let kinds: Vec<(u16, u16)> = own
            .iter()
            .map(|&(v, p)| seat_input::local_pads::mirror_kind(seat_input::gamepad::PadFamily::Hid, v, p).usb_ids())
            .collect();
        own.extend(kinds);
        own.push(seat_pads::PadKind::XOne.usb_ids());
    }
    seat_input::pad_scope::sdl_env(&sdl_ignore_list(&cfg.pad_ignore_devices, &others, &own))
}

impl crate::service::Service {
    /// Kiosk seats running as their own Windows user (read before the state lock).
    pub(crate) fn kiosk_seat_sids(&self) -> KioskSids {
        self.backends().session.kiosk_users().into_iter().map(|u| (u.seat, u.user_sid)).collect()
    }

    /// [`targets`] under the state lock.
    pub(crate) fn local_pad_targets(&self) -> Vec<LocalPadSeat> {
        let kiosk = self.kiosk_seat_sids();
        targets(&self.lock(), &kiosk)
    }

    /// Pad scoping plan (`None` = scoping off: everything restored).
    pub(crate) fn pad_scope_plan(&self, remote_seats_running: bool, parent_of: &dyn Fn(&str) -> Option<String>) -> Option<Vec<seat_input::pad_scope::ScopeWant>> {
        let console = self.platform.console_session();
        let kiosk = self.kiosk_seat_sids();
        let console_owner = console.and_then(|c| self.platform.session_user(c)).map(|u| ScopeOwner {
            session_id: console.unwrap_or_default(),
            user_sid: u.sid,
        });
        let st = self.lock();
        if !st.config.settings.enabled || !st.config.settings.input.pad_device_scope {
            return None;
        }
        // Scoping only matters while another session could open the pads: a second running
        // local seat with its own session, or a remote seat.
        let sessions: std::collections::BTreeSet<u32> = st
            .seats
            .iter()
            .filter(|(n, _)| st.config.seats.iter().any(|c| c.number == **n && c.kind == SeatKind::Local))
            .filter_map(|(_, s)| s.rt.session_id)
            .chain(console)
            .collect();
        if sessions.len() < 2 && !remote_seats_running {
            return Some(Vec::new());
        }
        let nodes = scope_pads(&st, &kiosk, parent_of);
        Some(seat_input::pad_scope::plan(&nodes, console_owner.as_ref(), remote_seats_running))
    }

    /// [`local_seat_map`] under the state lock.
    pub(crate) fn local_seat_map(&self, parent_of: &dyn Fn(&str) -> Option<String>) -> Vec<seat_shim_core::SeatMapEntry> {
        let console = self.platform.console_session();
        let st = self.lock();
        if !st.config.settings.enabled {
            return Vec::new();
        }
        local_seat_map(&st, console, parent_of)
    }

    /// USB ids of every live physical pad of the PC (remote seats' SDL ignore list).
    pub(crate) fn physical_pad_ids(&self) -> Vec<(u16, u16)> {
        let st = self.lock();
        let mut v: Vec<(u16, u16)> = pads(&st).into_iter().filter_map(|(d, _)| ids_of(d)).filter(|&(v, p)| v != 0 || p != 0).collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    /// [`sdl_env`] under the state lock.
    pub(crate) fn pad_sdl_env(&self, seat: SeatNumber) -> Vec<(String, String)> {
        sdl_env(&self.lock(), seat)
    }
}
