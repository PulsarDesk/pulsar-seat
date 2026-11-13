// SPDX-License-Identifier: GPL-3.0-only
//! CCD (Connecting and Configuring Displays): `QueryDisplayConfig`,
//! `DisplayConfigGetDeviceInfo`, `SetDisplayConfig` (Microsoft Learn, "CCD APIs").
//!
//! Needs access to the console session's desktop: from session 0 these calls fail with
//! `ERROR_ACCESS_DENIED`, hence the helper process (`crate::console`).

use super::{from_wide, luid_u64, os_err};
use crate::model::TargetInfo;
use seat_core::backend::BackendResult;
use seat_core::device::{DisplayMode, Rect};
use std::collections::BTreeMap;
use windows::Win32::Devices::Display::*;
use windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, LUID};

const PATH_ACTIVE: u32 = 0x1; // DISPLAYCONFIG_PATH_ACTIVE
const ERROR_NOT_SUPPORTED: u32 = 50;
const ERROR_INVALID_PARAMETER: u32 = 87;
const MODE_IDX_INVALID: u32 = 0xffff_ffff;

fn header<T>(ty: DISPLAYCONFIG_DEVICE_INFO_TYPE, adapter: LUID, id: u32) -> DISPLAYCONFIG_DEVICE_INFO_HEADER {
    DISPLAYCONFIG_DEVICE_INFO_HEADER { r#type: ty, size: std::mem::size_of::<T>() as u32, adapterId: adapter, id }
}

fn rational_mhz(r: DISPLAYCONFIG_RATIONAL) -> u32 {
    if r.Denominator == 0 { 0 } else { ((r.Numerator as u64 * 1000) / r.Denominator as u64) as u32 }
}

fn rotation_deg(r: DISPLAYCONFIG_ROTATION) -> u16 {
    match r.0 {
        2 => 90,
        3 => 180,
        4 => 270,
        _ => 0,
    }
}

/// Raw query, retried when the topology changes between the size call and the query.
pub fn raw(flags: QUERY_DISPLAY_CONFIG_FLAGS) -> BackendResult<(Vec<DISPLAYCONFIG_PATH_INFO>, Vec<DISPLAYCONFIG_MODE_INFO>)> {
    for _ in 0..5 {
        let (mut np, mut nm) = (0u32, 0u32);
        let r = unsafe { GetDisplayConfigBufferSizes(flags, &mut np, &mut nm) };
        if r != ERROR_SUCCESS {
            return Err(os_err("ccd_query", "GetDisplayConfigBufferSizes", r.0));
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
        let r = unsafe { QueryDisplayConfig(flags, &mut np, paths.as_mut_ptr(), &mut nm, modes.as_mut_ptr(), None) };
        if r == ERROR_INSUFFICIENT_BUFFER {
            continue;
        }
        if r != ERROR_SUCCESS {
            return Err(os_err("ccd_query", "QueryDisplayConfig", r.0));
        }
        paths.truncate(np as usize);
        modes.truncate(nm as usize);
        return Ok((paths, modes));
    }
    Err(os_err("ccd_query", "QueryDisplayConfig (topology kept changing)", ERROR_INSUFFICIENT_BUFFER.0))
}

/// Every target with a monitor (active or not), one entry per (adapter, target).
pub fn query() -> BackendResult<Vec<TargetInfo>> {
    let (paths, modes) = raw(QDC_ALL_PATHS | QDC_INCLUDE_HMD)?;
    let mut out: BTreeMap<(u64, u32), TargetInfo> = BTreeMap::new();
    for p in &paths {
        let ti = &p.targetInfo;
        let si = &p.sourceInfo;
        let key = (luid_u64(ti.adapterId), ti.id);
        let active = p.flags & PATH_ACTIVE != 0;
        if let Some(existing) = out.get(&key) {
            // QDC_ALL_PATHS lists every possible source per target; keep the active one.
            if existing.active || !active {
                continue;
            }
        }
        let available = ti.targetAvailable.as_bool();
        if !available && !active {
            continue;
        }
        let mut t = TargetInfo {
            adapter_luid: key.0,
            target_id: ti.id,
            active,
            available,
            output_technology: ti.outputTechnology.0 as u32,
            ..Default::default()
        };
        target_name(ti.adapterId, ti.id, &mut t);
        t.adapter_path = adapter_path(ti.adapterId);
        t.preferred = preferred_mode(ti.adapterId, ti.id);
        (t.specialized, t.specialization_available) = specialization(ti.adapterId, ti.id);
        if active {
            t.source_id = Some(si.id);
            t.gdi_name = source_gdi_name(si.adapterId, si.id);
            let smi = unsafe { si.Anonymous.modeInfoIdx };
            let tmi = unsafe { ti.Anonymous.modeInfoIdx };
            let refresh = rational_mhz(ti.refreshRate);
            if smi != MODE_IDX_INVALID && (smi as usize) < modes.len() {
                let m = &modes[smi as usize];
                if m.infoType == DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE {
                    let sm = unsafe { m.Anonymous.sourceMode };
                    t.desktop_rect = Some(Rect { x: sm.position.x, y: sm.position.y, width: sm.width, height: sm.height });
                    t.primary = sm.position.x == 0 && sm.position.y == 0;
                    t.mode = Some(DisplayMode { width: sm.width, height: sm.height, refresh_mhz: refresh, rotation: rotation_deg(ti.rotation) });
                }
            }
            if tmi != MODE_IDX_INVALID && (tmi as usize) < modes.len() {
                let m = &modes[tmi as usize];
                if m.infoType == DISPLAYCONFIG_MODE_INFO_TYPE_TARGET {
                    let sig = unsafe { m.Anonymous.targetMode.targetVideoSignalInfo };
                    let hz = rational_mhz(sig.vSyncFreq);
                    if let Some(mode) = t.mode.as_mut() {
                        if hz != 0 {
                            mode.refresh_mhz = hz;
                        }
                    }
                }
            }
        }
        out.insert(key, t);
    }
    Ok(out.into_values().collect())
}

fn target_name(adapter: LUID, id: u32, t: &mut TargetInfo) {
    let mut n = DISPLAYCONFIG_TARGET_DEVICE_NAME {
        header: header::<DISPLAYCONFIG_TARGET_DEVICE_NAME>(DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, adapter, id),
        ..Default::default()
    };
    if unsafe { DisplayConfigGetDeviceInfo(&mut n.header) } == 0 {
        t.monitor_name = from_wide(&n.monitorFriendlyDeviceName);
        t.monitor_path = from_wide(&n.monitorDevicePath);
        t.edid_manufacture_id = n.edidManufactureId;
        t.edid_product_code = n.edidProductCodeId;
        t.connector_instance = n.connectorInstance;
    }
}

fn adapter_path(adapter: LUID) -> String {
    let mut n = DISPLAYCONFIG_ADAPTER_NAME {
        header: header::<DISPLAYCONFIG_ADAPTER_NAME>(DISPLAYCONFIG_DEVICE_INFO_GET_ADAPTER_NAME, adapter, 0),
        ..Default::default()
    };
    if unsafe { DisplayConfigGetDeviceInfo(&mut n.header) } == 0 { from_wide(&n.adapterDevicePath) } else { String::new() }
}

fn source_gdi_name(adapter: LUID, id: u32) -> Option<String> {
    let mut n = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
        header: header::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>(DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, adapter, id),
        ..Default::default()
    };
    (unsafe { DisplayConfigGetDeviceInfo(&mut n.header) } == 0).then(|| from_wide(&n.viewGdiDeviceName))
}

fn preferred_mode(adapter: LUID, id: u32) -> Option<DisplayMode> {
    let mut n = DISPLAYCONFIG_TARGET_PREFERRED_MODE {
        header: header::<DISPLAYCONFIG_TARGET_PREFERRED_MODE>(DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_PREFERRED_MODE, adapter, id),
        ..Default::default()
    };
    if unsafe { DisplayConfigGetDeviceInfo(&mut n.header) } != 0 || n.width == 0 {
        return None;
    }
    let hz = rational_mhz(n.targetMode.targetVideoSignalInfo.vSyncFreq);
    Some(DisplayMode { width: n.width, height: n.height, refresh_mhz: hz, rotation: 0 })
}

/// Monitor specialization state (Windows 11 builds with the documented device-info types
/// 12 / 13). Bits of the get call: 0 = enabled, 1 = available for this monitor, 2 =
/// available on this system. Older systems fail the call: `(false, None)`.
/// HYPOTHESIS: "enabled" matches the Settings switch that removes a display from the
/// desktop; Display Core's `UsageKind` stays the authoritative signal (`super::dcore`).
fn specialization(adapter: LUID, id: u32) -> (bool, Option<bool>) {
    let mut n = DISPLAYCONFIG_GET_MONITOR_SPECIALIZATION {
        header: header::<DISPLAYCONFIG_GET_MONITOR_SPECIALIZATION>(DISPLAYCONFIG_DEVICE_INFO_GET_MONITOR_SPECIALIZATION, adapter, id),
        ..Default::default()
    };
    if unsafe { DisplayConfigGetDeviceInfo(&mut n.header) } != 0 {
        return (false, None);
    }
    let v = unsafe { n.Anonymous.value };
    (v & 1 != 0, Some(v & 0b110 == 0b110))
}

/// Specialization type Pulsar sets (our own GUIDs; HYPOTHESIS: Windows accepts an
/// application-defined type, the documented field only says "a GUID associated with the
/// monitor specialization type").
const PULSAR_SPECIALIZATION_TYPE: windows_core::GUID = windows_core::GUID::from_u128(0x1d4f9de3_cb93_490d_8ae2_fe657ad1ae2a);
const PULSAR_SPECIALIZATION_SUBTYPE: windows_core::GUID = windows_core::GUID::from_u128(0x43cadcd1_a3b7_4ec4_9e65_e739ffc98baf);
/// Shown by Settings as the app that owns the specialized display.
const PULSAR_SPECIALIZATION_APP: &str = "Pulsar Seat";

/// Turn monitor specialization on or off (`DisplayConfigSetDeviceInfo`). Untested on
/// hardware. Returns the Win32 error on refusal.
pub fn set_specialization(adapter_luid: u64, target_id: u32, on: bool) -> Result<(), u32> {
    let adapter = super::luid_from(adapter_luid);
    let mut n = DISPLAYCONFIG_SET_MONITOR_SPECIALIZATION {
        header: header::<DISPLAYCONFIG_SET_MONITOR_SPECIALIZATION>(DISPLAYCONFIG_DEVICE_INFO_SET_MONITOR_SPECIALIZATION, adapter, target_id),
        specializationType: PULSAR_SPECIALIZATION_TYPE,
        specializationSubType: PULSAR_SPECIALIZATION_SUBTYPE,
        ..Default::default()
    };
    n.Anonymous.value = u32::from(on);
    for (d, c) in n.specializationApplicationName.iter_mut().zip(PULSAR_SPECIALIZATION_APP.encode_utf16()) {
        *d = c;
    }
    match unsafe { DisplayConfigSetDeviceInfo(&n.header) } {
        0 => Ok(()),
        e => Err(e as u32),
    }
}

/// Wizard: extend the desktop to every connected monitor (spec 40 §3.13).
pub fn extend_desktop() -> BackendResult<()> {
    let r = unsafe { SetDisplayConfig(None, None, SDC_APPLY | SDC_TOPOLOGY_EXTEND) };
    if r != 0 {
        return Err(os_err("ccd_apply", "SetDisplayConfig(extend)", r as u32));
    }
    Ok(())
}

// ---- topology changes (spec 40 §6.2) ------------------------------------------------

use crate::topology::{plan_paths, verify_topology, ActivePath, PathPlan, TargetRef, TopologyOutcome, TopologyRequest};

fn tref(ti: &DISPLAYCONFIG_PATH_TARGET_INFO) -> TargetRef {
    TargetRef { adapter_luid: luid_u64(ti.adapterId), target_id: ti.id }
}

fn active_targets() -> BackendResult<Vec<TargetRef>> {
    let (paths, _) = raw(QDC_ONLY_ACTIVE_PATHS)?;
    Ok(paths.iter().filter(|p| p.flags & PATH_ACTIVE != 0).map(|p| tref(&p.targetInfo)).collect())
}

/// Path / mode arrays for the planned topology (kept paths keep their modes, re-indexed;
/// new paths get invalid mode indices so `SDC_ALLOW_CHANGES` picks a mode and position).
fn build_arrays(
    active: &[DISPLAYCONFIG_PATH_INFO],
    modes: &[DISPLAYCONFIG_MODE_INFO],
    all: &[DISPLAYCONFIG_PATH_INFO],
    plan: &PathPlan,
) -> (Vec<DISPLAYCONFIG_PATH_INFO>, Vec<DISPLAYCONFIG_MODE_INFO>) {
    let mut out_modes: Vec<DISPLAYCONFIG_MODE_INFO> = Vec::new();
    let mut remap: BTreeMap<u32, u32> = BTreeMap::new();
    let mut map_mode = |idx: u32, out_modes: &mut Vec<DISPLAYCONFIG_MODE_INFO>| -> u32 {
        if idx == MODE_IDX_INVALID || idx as usize >= modes.len() {
            return MODE_IDX_INVALID;
        }
        *remap.entry(idx).or_insert_with(|| {
            let mut m = modes[idx as usize];
            if m.infoType == DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE && plan.shift != (0, 0) {
                // SAFETY: the tag says this is a source mode.
                unsafe {
                    m.Anonymous.sourceMode.position.x += plan.shift.0;
                    m.Anonymous.sourceMode.position.y += plan.shift.1;
                }
            }
            out_modes.push(m);
            (out_modes.len() - 1) as u32
        })
    };
    let mut out_paths = Vec::new();
    for &i in &plan.keep {
        let mut p = active[i];
        // SAFETY: plain u32 union members (no QDC_VIRTUAL_MODE_AWARE).
        let (s, t) = unsafe { (p.sourceInfo.Anonymous.modeInfoIdx, p.targetInfo.Anonymous.modeInfoIdx) };
        p.sourceInfo.Anonymous.modeInfoIdx = map_mode(s, &mut out_modes);
        p.targetInfo.Anonymous.modeInfoIdx = map_mode(t, &mut out_modes);
        out_paths.push(p);
    }
    for (t, source) in &plan.add {
        if let Some(c) = all.iter().find(|p| tref(&p.targetInfo) == *t && p.sourceInfo.id == *source) {
            let mut p = *c;
            p.flags = PATH_ACTIVE;
            p.sourceInfo.Anonymous.modeInfoIdx = MODE_IDX_INVALID;
            p.targetInfo.Anonymous.modeInfoIdx = MODE_IDX_INVALID;
            out_paths.push(p);
        }
    }
    (out_paths, out_modes)
}

fn set_config(paths: &[DISPLAYCONFIG_PATH_INFO], modes: &[DISPLAYCONFIG_MODE_INFO], validate_only: bool) -> i32 {
    let base = SDC_USE_SUPPLIED_DISPLAY_CONFIG | SDC_ALLOW_CHANGES;
    let flags = if validate_only { base | SDC_VALIDATE } else { base | SDC_APPLY | SDC_SAVE_TO_DATABASE };
    unsafe { SetDisplayConfig(Some(paths), (!modes.is_empty()).then_some(modes), flags) }
}

/// Put an earlier `QDC_ONLY_ACTIVE_PATHS` snapshot back exactly (no `SDC_ALLOW_CHANGES`,
/// so the earlier modes and positions return); when Windows refuses the exact set, once
/// more with `SDC_ALLOW_CHANGES`.
fn restore_config(paths: &[DISPLAYCONFIG_PATH_INFO], modes: &[DISPLAYCONFIG_MODE_INFO]) -> i32 {
    if paths.is_empty() {
        return 0;
    }
    let exact = SDC_USE_SUPPLIED_DISPLAY_CONFIG | SDC_APPLY | SDC_SAVE_TO_DATABASE;
    let r = unsafe { SetDisplayConfig(Some(paths), (!modes.is_empty()).then_some(modes), exact) };
    if r == 0 {
        return 0;
    }
    log::warn!("display: exact restore of the previous paths refused (0x{:08x}); retrying with changes allowed", r as u32);
    set_config(paths, modes, false)
}

fn undo_specialization(out: &mut TopologyOutcome) {
    for t in &out.specialized {
        let _ = set_specialization(t.adapter_luid, t.target_id, false);
    }
    for t in &out.unspecialized {
        let _ = set_specialization(t.adapter_luid, t.target_id, true);
    }
    if !out.specialized.is_empty() || !out.unspecialized.is_empty() {
        out.rolled_back = true;
    }
    out.specialized.clear();
    out.unspecialized.clear();
}

/// Snapshot taken before anything changed (rollback target).
struct PreSnapshot {
    paths: Vec<DISPLAYCONFIG_PATH_INFO>,
    modes: Vec<DISPLAYCONFIG_MODE_INFO>,
}

/// Undo this call: specialization first, then the earlier active paths (when anything
/// changed them: our `SetDisplayConfig` or specialization taking a path away). When the
/// restore fails, targets that were active before and are not now are reported as
/// `deactivated` so the journal keeps them for the next try.
fn roll_back(out: &mut TopologyOutcome, pre: &PreSnapshot, paths_changed: bool) {
    let spec_changed = !out.specialized.is_empty() || !out.unspecialized.is_empty();
    undo_specialization(out);
    if !paths_changed && !spec_changed {
        return;
    }
    if spec_changed {
        // Let Windows finish re-evaluating the topology before we set it.
        std::thread::sleep(std::time::Duration::from_millis(400));
    }
    out.rolled_back = true;
    let before: Vec<TargetRef> = pre.paths.iter().filter(|p| p.flags & PATH_ACTIVE != 0).map(|p| tref(&p.targetInfo)).collect();
    let now = active_targets().unwrap_or_default();
    if before.iter().all(|t| now.contains(t)) && !paths_changed {
        return; // specialization undo alone brought everything back
    }
    let r = restore_config(&pre.paths, &pre.modes);
    if r != 0 {
        log::error!("display: restoring the previous paths failed: 0x{:08x}", r as u32);
    }
    let now = active_targets().unwrap_or_default();
    for t in before.into_iter().filter(|t| !now.contains(t)) {
        if !out.deactivated.contains(&t) {
            out.deactivated.push(t);
        }
    }
}

/// Run a [`TopologyRequest`] (console session only). Order: specialization changes, then
/// one `SetDisplayConfig` for the path changes: `SDC_VALIDATE` first, then apply with
/// `SDC_SAVE_TO_DATABASE`, then re-query. A refused validation, a failed apply or a removed
/// target that is still active puts the previous paths back and undoes this call's
/// specialization changes (`rolled_back`). Untested on hardware.
pub fn apply_request(req: &TopologyRequest) -> BackendResult<TopologyOutcome> {
    let mut out = TopologyOutcome::default();
    // 0. Rollback snapshot, before anything changes (specialization can take paths away).
    let (pre_paths, pre_modes) = raw(QDC_ONLY_ACTIVE_PATHS)?;
    let pre = PreSnapshot { paths: pre_paths, modes: pre_modes };
    // 1. Specialization (feature-detected per call: refusal keeps the target in the desktop).
    let mut path_remove: Vec<TargetRef> = Vec::new();
    for r in &req.remove {
        if !r.specialize {
            path_remove.push(r.target);
            continue;
        }
        match set_specialization(r.target.adapter_luid, r.target.target_id, true) {
            Ok(()) => {
                out.specialization_supported = true;
                out.specialized.push(r.target);
                path_remove.push(r.target);
            }
            Err(e) => {
                let why = if e == ERROR_NOT_SUPPORTED || e == ERROR_INVALID_PARAMETER { "specialization_unsupported" } else { "specialization_refused" };
                log::warn!("display: specialization of target {} refused: 0x{e:08x}", r.target.target_id);
                out.failed.push((r.target, why.into()));
            }
        }
    }
    for r in req.restore.iter().filter(|r| r.unspecialize) {
        match set_specialization(r.target.adapter_luid, r.target.target_id, false) {
            Ok(()) => out.unspecialized.push(r.target),
            Err(e) => {
                log::warn!("display: removing specialization of target {} failed: 0x{e:08x}", r.target.target_id);
                out.failed.push((r.target, "unspecialization_refused".into()));
            }
        }
    }

    // 2. Paths. Specialization changes re-evaluate the topology asynchronously: give
    // Windows a moment before reading the active paths.
    if !out.specialized.is_empty() || !out.unspecialized.is_empty() {
        std::thread::sleep(std::time::Duration::from_millis(400));
    }
    let add: Vec<TargetRef> = req.restore.iter().filter(|r| r.activate).map(|r| r.target).collect();
    // Post-specialization view: used for planning only, never for rollback.
    let (active, modes, all) = match raw(QDC_ONLY_ACTIVE_PATHS).and_then(|(a, m)| raw(QDC_ALL_PATHS).map(|(all, _)| (a, m, all))) {
        Ok(v) => v,
        Err(e) => {
            roll_back(&mut out, &pre, false);
            return Err(e);
        }
    };
    let aps: Vec<ActivePath> = active
        .iter()
        .map(|p| {
            let smi = unsafe { p.sourceInfo.Anonymous.modeInfoIdx };
            let origin = (smi != MODE_IDX_INVALID && (smi as usize) < modes.len() && modes[smi as usize].infoType == DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE)
                .then(|| unsafe { modes[smi as usize].Anonymous.sourceMode.position })
                .map(|pt| (pt.x, pt.y));
            ActivePath { target: tref(&p.targetInfo), source_id: p.sourceInfo.id, origin }
        })
        .collect();
    let cands: Vec<(TargetRef, u32)> = all.iter().map(|p| (tref(&p.targetInfo), p.sourceInfo.id)).collect();
    let fail_all = |out: &mut TopologyOutcome, why: &str| {
        for t in path_remove.iter().chain(&add) {
            out.failed.push((*t, why.to_string()));
        }
    };
    let plan = match plan_paths(&aps, &cands, &path_remove, &add) {
        Ok(p) => p,
        Err(reason) => {
            roll_back(&mut out, &pre, false);
            fail_all(&mut out, reason);
            return Ok(out);
        }
    };
    for t in &plan.no_source {
        out.failed.push((*t, "no_free_source".into()));
    }
    let is_active = |t: &TargetRef| aps.iter().any(|a| &a.target == t);
    if !plan.changes {
        // Specialization alone took them out (or they already were).
        out.deactivated = path_remove.iter().copied().filter(|t| !is_active(t)).collect();
        out.activated = add.iter().copied().filter(|t| is_active(t)).collect();
        return Ok(out);
    }
    let (paths, new_modes) = build_arrays(&active, &modes, &all, &plan);
    let v = set_config(&paths, &new_modes, true);
    if v != 0 {
        log::warn!("display: SetDisplayConfig(validate) refused the topology: 0x{:08x}", v as u32);
        // Nothing applied yet; specialization may still have taken paths away.
        roll_back(&mut out, &pre, false);
        fail_all(&mut out, "topology_invalid");
        return Ok(out);
    }
    out.applied = true;
    let r = set_config(&paths, &new_modes, false);
    let after = if r == 0 { active_targets().unwrap_or_default() } else { Vec::new() };
    let keep: Vec<TargetRef> = plan.keep.iter().map(|&i| aps[i].target).collect();
    let ok = r == 0 && verify_topology(&after, &keep, &path_remove);
    if !ok {
        let why = if r != 0 { "apply_failed" } else { "verify_failed" };
        log::warn!("display: topology change {why} (0x{:08x}); restoring the previous paths", r as u32);
        roll_back(&mut out, &pre, true);
        fail_all(&mut out, why);
        return Ok(out);
    }
    out.deactivated = path_remove.clone();
    for t in &add {
        if after.contains(t) {
            out.activated.push(*t);
        } else if !plan.no_source.contains(t) {
            out.failed.push((*t, "not_activated".into()));
        }
    }
    Ok(out)
}

/// `fast_user_switch` seat topology (spec 35 §5.6), run by the seat agent in the session
/// that just became the console: keep only `keep` in the desktop
/// ([`crate::topology::seat_only_request`]). `Ok(None)` = nothing to change. Untested on
/// hardware.
pub fn apply_seat_only(keep: &[TargetRef]) -> BackendResult<Option<TopologyOutcome>> {
    let targets = query()?;
    match crate::topology::seat_only_request(&targets, keep) {
        Some(req) => apply_request(&req).map(Some),
        None => Ok(None),
    }
}
