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
//!    nodes belong to (`seat_input::pad_scope`, setting `input.pad_device_scope`). Since
//!    OSMS parity G5 (2026-11-18) the pads are found by class (XUSB, GIP, HID game
//!    collections) with their whole pad subtree, routed by their HID units or by the
//!    inventory device / USB hub above them, per seat [`PadMode`], with the logon SID when
//!    one user is signed in to two sessions; and the Guide-button guard
//!    ([`guide_guard_want`], `seat_compat::guide_guard`) follows the same switch.
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

use seat_core::config::{PadMode, SeatKind};
use seat_core::device::{Device, DeviceKind};
use seat_core::SeatNumber;
use seat_input::gamepad::pad_family;
use seat_input::local_pads::{LocalPadSeat, PadBus, RoutedPad, local_seat_pad_id};
use seat_input::pad_scope::{PadGroup, PadNodes, PadTree, ScopeOwner, discover, principal, sdl_ignore_list};
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

/// Pad hand-over of a seat that pads may be routed to: `None` for a seat that is not a
/// local seat.
fn pad_mode_of(st: &State, seat: SeatNumber) -> Option<PadMode> {
    st.config.seats.iter().find(|s| s.number == seat && s.kind == SeatKind::Local).map(|s| s.local_settings().pad_mode)
}

/// Owner principals per session: user SIDs signed in to more than one session use the
/// session's logon SID (`seat_input::pad_scope::principal`). `logon` = session -> logon SID
/// for the sessions the service asked about ([`shared_user_sessions`]).
pub(crate) type LogonSids = std::collections::BTreeMap<u32, String>;

/// Sessions whose user is signed in to another session too (running local seats and the
/// console), i.e. the sessions whose logon SID scoping needs.
fn shared_user_sessions(st: &State, kiosk: &KioskSids, console: Option<&ScopeOwner>) -> std::collections::BTreeSet<u32> {
    let mut by_user: std::collections::BTreeMap<String, std::collections::BTreeSet<u32>> = Default::default();
    let owners = st.config.seats.iter().filter_map(|c| seat_owner(st, kiosk, c.number)).chain(console.cloned());
    for o in owners {
        by_user.entry(o.user_sid.to_ascii_uppercase()).or_default().insert(o.session_id);
    }
    by_user.into_values().filter(|s| s.len() > 1).flatten().collect()
}

/// [`seat_owner`] for scoping: only seats in [`PadMode::Passthrough`], with the logon SID
/// when the seat's session shares its user with another session.
fn scope_owner(st: &State, kiosk: &KioskSids, seat: SeatNumber, logon: &LogonSids) -> Option<ScopeOwner> {
    if pad_mode_of(st, seat)? != PadMode::Passthrough {
        return None;
    }
    let o = seat_owner(st, kiosk, seat)?;
    let l = logon.get(&o.session_id).map(String::as_str);
    Some(principal(o, l.is_some(), l))
}

/// The single seat a pad group is routed to: one of its HID units in the inventory
/// (pending hotkey move first), else the nearest inventory device on its node chain (the
/// pad's own USB device, or the hub / port it is plugged into).
fn group_seat(st: &State, effective: &[seat_core::assign::Effective], g: &PadGroup) -> Option<SeatNumber> {
    let up = |d: &Device| d.instance_id.as_ref().map(|i| i.to_ascii_uppercase());
    let live: Vec<&Device> = st.inventory.live().collect();
    if let Some(s) = live.iter().filter(|d| up(d).is_some_and(|i| g.nodes.contains(&i))).find_map(|d| routed_seat(st, effective, d)) {
        return Some(s);
    }
    g.ancestors.iter().find_map(|a| live.iter().find(|d| up(d).as_deref() == Some(a.as_str())).and_then(|d| routed_seat(st, effective, d)))
}

/// Every physical pad's device nodes and its owning local seat, for
/// `seat_input::pad_scope::plan`: the pads `tree` finds by class (OSMS parity G5: XUSB,
/// GIP and HID game controllers with their whole pad subtree), plus, for an inventory pad
/// unit the class walk did not find, its HID collection and parent. A pad routed to a
/// local seat in [`PadMode::Shared`] is left out (never scoped, not even to the console).
pub(crate) fn scope_pads(st: &State, kiosk: &KioskSids, tree: &dyn PadTree, logon: &LogonSids) -> Vec<PadNodes> {
    let effective = st.inventory.effective(&st.config);
    let shared = |seat: Option<SeatNumber>| seat.and_then(|s| pad_mode_of(st, s)) == Some(PadMode::Shared);
    let mut out = Vec::new();
    let mut covered: std::collections::BTreeSet<String> = Default::default();
    for g in discover(tree) {
        covered.extend(g.nodes.iter().cloned());
        let seat = group_seat(st, &effective, &g);
        if shared(seat) {
            continue;
        }
        out.push(PadNodes { nodes: g.nodes, seat_owner: seat.and_then(|s| scope_owner(st, kiosk, s, logon)) });
    }
    for (d, routed) in pads(st) {
        let Some(inst) = d.instance_id.clone() else { continue };
        if covered.contains(&inst.to_ascii_uppercase()) || shared(routed) {
            continue;
        }
        let mut nodes = vec![inst.clone()];
        nodes.extend(tree.parent(&inst));
        out.push(PadNodes { nodes, seat_owner: routed.and_then(|s| scope_owner(st, kiosk, s, logon)) });
    }
    out
}

/// Whether pad scoping matters now: another session than the console could open the pads
/// (a second running local seat with its own session, or a remote seat).
fn scoping_matters(st: &State, console: Option<u32>, remote_seats_running: bool) -> bool {
    let sessions: std::collections::BTreeSet<u32> = st
        .seats
        .iter()
        .filter(|(n, _)| st.config.seats.iter().any(|c| c.number == **n && c.kind == SeatKind::Local))
        .filter_map(|(_, s)| s.rt.session_id)
        .chain(console)
        .collect();
    sessions.len() >= 2 || remote_seats_running
}

/// What the Guide-button guard wants (`seat_compat::guide_guard`): while pad scoping is on
/// and matters, the users of every running seat with a session (local seats in
/// passthrough, remote seats) and the console user; plus `ControllerToVKMapping` off.
/// Empty when the product or `input.pad_device_scope` is off, or no seat needs it.
pub(crate) fn guide_guard_want(st: &State, kiosk: &KioskSids, console: Option<&ScopeOwner>, remote_seats_running: bool) -> seat_compat::guide_guard::GuardWant {
    use seat_compat::guide_guard::GuardWant;
    if !st.config.settings.enabled || !st.config.settings.input.pad_device_scope {
        return GuardWant::default();
    }
    if !scoping_matters(st, console.map(|c| c.session_id), remote_seats_running) {
        return GuardWant::default();
    }
    let console_session = console.map(|c| c.session_id);
    let mut sids: Vec<String> = Vec::new();
    for c in &st.config.seats {
        let user = match c.kind {
            SeatKind::Local if c.local_settings().pad_mode == PadMode::Passthrough => seat_owner(st, kiosk, c.number).map(|o| (o.session_id, o.user_sid)),
            SeatKind::Local => None,
            _ => st.seats.get(&c.number).and_then(|s| Some((s.rt.session_id?, s.rt.user_sid.clone()?))),
        };
        // The seat running in the console session is the console user (added below), not
        // a seat that needs the guard.
        sids.extend(user.filter(|(sess, _)| Some(*sess) != console_session).map(|(_, sid)| sid));
    }
    if sids.is_empty() {
        return GuardWant::default();
    }
    sids.extend(console.map(|c| c.user_sid.clone()));
    sids.iter_mut().for_each(|s| *s = s.to_ascii_uppercase());
    sids.sort();
    sids.dedup();
    GuardWant { sids, vk_mapping_off: true }
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
        if owner.session_id == 0 {
            continue;
        }
        let shared = st.seats.iter().any(|(n, s)| *n != seat.number && s.rt.session_id == Some(owner.session_id));
        if shared {
            continue;
        }
        if Some(owner.session_id) == console {
            // The seat on the console keeps the console role (no isolation entry), but a
            // seat other than seat 1 gets an identity entry, so its single-instance names
            // and Steam override do not change with the console (VM round 2, 2026-11-17).
            if seat.number != SeatNumber::CONSOLE {
                out.push(seat_shim_core::SeatMapEntry {
                    session_id: owner.session_id,
                    seat_id: local_seat_pad_id(seat.number.get()),
                    user_sid: owner.user_sid,
                    generation: 1,
                    pad_instance_prefixes: Vec::new(),
                    flags: seat_shim_core::SeatFlags {
                        hid_isolation: false,
                        xinput_focus: false,
                        steam: st.config.settings.apps.steam_per_seat,
                        shared_account: false,
                        console: true,
                    },
                });
            }
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
                console: false,
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

    /// The console session's user as a scope owner.
    fn console_owner(&self) -> Option<ScopeOwner> {
        let console = self.platform.console_session()?;
        self.platform.session_user(console).map(|u| ScopeOwner { session_id: console, user_sid: u.sid })
    }

    /// Pad scoping plan (`None` = scoping off: everything restored).
    pub(crate) fn pad_scope_plan(&self, remote_seats_running: bool, tree: &dyn PadTree) -> Option<Vec<seat_input::pad_scope::ScopeWant>> {
        let console_owner = self.console_owner();
        let kiosk = self.kiosk_seat_sids();
        // Sessions whose user is signed in twice: their logon SIDs, asked outside the lock.
        let shared = {
            let st = self.lock();
            if !st.config.settings.enabled || !st.config.settings.input.pad_device_scope {
                return None;
            }
            // Scoping only matters while another session could open the pads.
            if !scoping_matters(&st, console_owner.as_ref().map(|c| c.session_id), remote_seats_running) {
                return Some(Vec::new());
            }
            shared_user_sessions(&st, &kiosk, console_owner.as_ref())
        };
        let logon: LogonSids = shared.into_iter().filter_map(|s| self.platform.session_user(s).map(|u| (s, u.logon_sid))).collect();
        let console_owner = console_owner.map(|c| {
            let l = logon.get(&c.session_id).map(String::as_str);
            principal(c, l.is_some(), l)
        });
        let st = self.lock();
        let nodes = scope_pads(&st, &kiosk, tree, &logon);
        Some(seat_input::pad_scope::plan(&nodes, console_owner.as_ref(), remote_seats_running))
    }

    /// [`guide_guard_want`] (user SIDs, not logon SIDs: the value lives in the user's hive).
    pub(crate) fn guide_guard_want(&self, remote_seats_running: bool) -> seat_compat::guide_guard::GuardWant {
        let console_owner = self.console_owner();
        let kiosk = self.kiosk_seat_sids();
        guide_guard_want(&self.lock(), &kiosk, console_owner.as_ref(), remote_seats_running)
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
