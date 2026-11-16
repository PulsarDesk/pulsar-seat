// SPDX-License-Identifier: GPL-3.0-only
//! One IddCx adapter per render GPU (spec 40 §6.3.4 / §6.4; driver interface minor 3).
//!
//! `IddCxAdapterSetRenderAdapter` is adapter-wide: every virtual monitor of one adapter
//! composes on one GPU. So each render GPU gets its own adapter (devnode): the installer's
//! root devnode `Root\PulsarSeatIdd` is the first, further ones are software devnodes
//! `PulsarSeatIdd_Adapter` the service creates on demand (`SwDeviceCreate`, lifetime =
//! the service's handle). The driver binds an adapter to the GPU of its first monitor and
//! refuses monitors of another GPU while it still has monitors (`STATUS_DEVICE_BUSY`).
//!
//! This module is the pure part: which adapter takes a monitor of a given GPU
//! ([`pick_adapter`]), the instance name of a new devnode ([`next_instance`]) and the
//! per-GPU summary the service reports ([`gpu_summary`]). The Windows part is
//! `crate::win::idd::WinIdd`. Untested on hardware (the driver is unsigned).

use crate::idd_proto::{PSIDD_ADAPTER_BOUND, PSIDD_ADAPTER_FP16, PSIDD_ADAPTER_READY, PSIDD_MAX_ADAPTERS};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// What the service knows about one adapter (control interface).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AdapterState {
    /// Control interface path (identity for routing remove calls).
    pub path: String,
    /// Driver interface minor of this adapter (all adapters run the same DLL, but a
    /// devnode may still host an older one until it restarts).
    pub minor: u16,
    /// `PSIDD_ADAPTER_*` (minor 3; older drivers: ready only).
    pub flags: u32,
    /// Bound render GPU, 0 = none.
    pub render_luid: u64,
    pub monitors: u32,
}

impl AdapterState {
    pub fn ready(&self) -> bool {
        self.flags & PSIDD_ADAPTER_READY != 0
    }
    pub fn bound(&self) -> bool {
        self.flags & PSIDD_ADAPTER_BOUND != 0 && self.render_luid != 0
    }
    /// IddCx 1.10 at run time: HDR monitors possible.
    pub fn fp16(&self) -> bool {
        self.flags & PSIDD_ADAPTER_FP16 != 0
    }
}

/// Where a new monitor goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterPick {
    /// `adapters[i]`.
    Existing(usize),
    /// No adapter fits: create one more devnode (then pick again).
    Create,
    /// No adapter fits and [`PSIDD_MAX_ADAPTERS`] exist already.
    Full,
}

/// The adapter for a monitor rendered on `luid` (0 = GPU unknown).
///
/// 1. A pre-minor-3 driver has one adapter and no binding: always the first adapter.
/// 2. GPU unknown: the first ready adapter (the driver keeps its binding).
/// 3. A ready adapter bound to `luid`.
/// 4. A ready adapter with no monitors (unbound first, then one bound elsewhere: the
///    driver re-binds an empty adapter).
/// 5. Otherwise a new adapter, up to [`PSIDD_MAX_ADAPTERS`].
///
/// Adapters that are not ready (initialising) are only used when nothing else is left
/// and no new one may be created; then the add fails `STATUS_DEVICE_NOT_READY` and the
/// service retries later.
pub fn pick_adapter(adapters: &[AdapterState], luid: u64) -> AdapterPick {
    if adapters.is_empty() {
        return AdapterPick::Create;
    }
    if adapters.iter().all(|a| a.minor < 3) {
        return AdapterPick::Existing(0);
    }
    let ready = |a: &AdapterState| a.ready() && a.minor >= 3;
    if luid == 0 {
        return adapters.iter().position(ready).map_or(AdapterPick::Existing(0), AdapterPick::Existing);
    }
    if let Some(i) = adapters.iter().position(|a| ready(a) && a.bound() && a.render_luid == luid) {
        return AdapterPick::Existing(i);
    }
    if let Some(i) = adapters.iter().position(|a| ready(a) && a.monitors == 0 && !a.bound()) {
        return AdapterPick::Existing(i);
    }
    if let Some(i) = adapters.iter().position(|a| ready(a) && a.monitors == 0) {
        return AdapterPick::Existing(i);
    }
    if adapters.len() < PSIDD_MAX_ADAPTERS {
        return AdapterPick::Create;
    }
    adapters.iter().position(|a| !a.ready()).map_or(AdapterPick::Full, AdapterPick::Existing)
}

/// Instance id (`SWD\PulsarSeatIdd\<instance>`) for the next software adapter: the
/// lowest `gpuN` (N = 1..) not in `taken`. N = 0 is the installer's root devnode.
pub fn next_instance(taken: &[String]) -> String {
    (1..=PSIDD_MAX_ADAPTERS)
        .map(|n| format!("gpu{n}"))
        .find(|s| !taken.iter().any(|t| t.eq_ignore_ascii_case(s)))
        .unwrap_or_else(|| format!("gpu{}", PSIDD_MAX_ADAPTERS + 1))
}

/// Per render GPU: the seats whose virtual monitors compose there and whether HDR is
/// possible on that adapter. For the service's diagnostics and the manager.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct GpuAdapterSummary {
    pub render_luid: u64,
    pub monitors: u32,
    pub hdr_capable: bool,
}

/// [`GpuAdapterSummary`] per bound adapter, sorted by LUID (unbound adapters left out).
pub fn gpu_summary(adapters: &[AdapterState]) -> Vec<GpuAdapterSummary> {
    let mut by: BTreeMap<u64, GpuAdapterSummary> = BTreeMap::new();
    for a in adapters.iter().filter(|a| a.bound()) {
        let e = by.entry(a.render_luid).or_insert_with(|| GpuAdapterSummary { render_luid: a.render_luid, ..Default::default() });
        e.monitors += a.monitors;
        e.hdr_capable |= a.fp16();
    }
    by.into_values().collect()
}

/// Render GPUs a seat set needs (distinct non-zero LUIDs), for the start-up log.
pub fn distinct_gpus(luids: impl IntoIterator<Item = u64>) -> Vec<u64> {
    let mut v: Vec<u64> = luids.into_iter().filter(|&l| l != 0).collect();
    v.sort_unstable();
    v.dedup();
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(minor: u16, flags: u32, luid: u64, monitors: u32) -> AdapterState {
        AdapterState { path: format!("p{luid}-{monitors}"), minor, flags, render_luid: luid, monitors }
    }
    const R: u32 = PSIDD_ADAPTER_READY;
    const RB: u32 = PSIDD_ADAPTER_READY | PSIDD_ADAPTER_BOUND;

    #[test]
    fn same_gpu_shares_an_adapter_other_gpu_gets_its_own() {
        // Root adapter bound to GPU 7 with two monitors.
        let list = vec![a(3, RB, 7, 2)];
        assert_eq!(pick_adapter(&list, 7), AdapterPick::Existing(0));
        assert_eq!(pick_adapter(&list, 9), AdapterPick::Create, "a second GPU needs a second adapter");
        let list = vec![a(3, RB, 7, 2), a(3, RB, 9, 1)];
        assert_eq!(pick_adapter(&list, 9), AdapterPick::Existing(1));
        assert_eq!(pick_adapter(&list, 7), AdapterPick::Existing(0));
    }

    #[test]
    fn empty_adapters_are_reused_before_new_ones() {
        let list = vec![a(3, RB, 7, 1), a(3, RB, 9, 0), a(3, R, 0, 0)];
        assert_eq!(pick_adapter(&list, 11), AdapterPick::Existing(2), "unbound empty first");
        let list = vec![a(3, RB, 7, 1), a(3, RB, 9, 0)];
        assert_eq!(pick_adapter(&list, 11), AdapterPick::Existing(1), "then an empty one bound elsewhere");
        let list = vec![a(3, R, 0, 0)];
        assert_eq!(pick_adapter(&list, 5), AdapterPick::Existing(0), "fresh root adapter binds to the first GPU");
    }

    #[test]
    fn old_driver_unknown_gpu_and_limits() {
        assert_eq!(pick_adapter(&[a(2, R, 0, 3)], 9), AdapterPick::Existing(0), "minor 2: one adapter, no binding");
        assert_eq!(pick_adapter(&[], 9), AdapterPick::Create);
        assert_eq!(pick_adapter(&[a(3, 0, 0, 0), a(3, RB, 4, 1)], 0), AdapterPick::Existing(1), "unknown GPU: first ready");
        let full: Vec<AdapterState> = (1..=PSIDD_MAX_ADAPTERS as u64).map(|l| a(3, RB, l, 1)).collect();
        assert_eq!(pick_adapter(&full, 99), AdapterPick::Full);
        let mut busy = full.clone();
        busy[3].flags = 0; // initialising
        assert_eq!(pick_adapter(&busy, 99), AdapterPick::Existing(3), "wait for the initialising one");
        // A not-ready adapter is not used while a new one may be created.
        assert_eq!(pick_adapter(&[a(3, 0, 0, 0)], 4), AdapterPick::Create);
    }

    #[test]
    fn instance_names_and_summary() {
        assert_eq!(next_instance(&[]), "gpu1");
        assert_eq!(next_instance(&["GPU1".into(), "gpu3".into()]), "gpu2");
        let list = vec![a(3, RB | PSIDD_ADAPTER_FP16, 7, 2), a(3, RB, 9, 1), a(3, R, 0, 0)];
        let s = gpu_summary(&list);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0], GpuAdapterSummary { render_luid: 7, monitors: 2, hdr_capable: true });
        assert_eq!(s[1], GpuAdapterSummary { render_luid: 9, monitors: 1, hdr_capable: false });
        assert_eq!(distinct_gpus([0, 9, 7, 9]), vec![7, 9]);
    }
}
