// SPDX-License-Identifier: GPL-3.0-only
//! `PulsarSplitShim.dll`: the per-process isolation module (spec 50 §4–§9, contracts §4.3).
//!
//! Loaded by the launcher (`pulsar-split-launch`) or by AppHelp through our custom shim
//! database. It exports the two shim-engine entry points `GetHookAPIs` / `NotifyShims`
//! (returning no shim-engine hooks: we install our own detours with `retour`) and
//! `PulsarSplitInit` (called synchronously by the launcher before the target resumes).
//!
//! Every hook calls a pure decision function from `seat_shim_core::decide`; the module
//! only gathers inputs (session ids, device parent chains, WNF payloads) and applies the
//! answer. Pure buffer filters live in [`filter`] and are unit-tested on any host.
#![allow(non_snake_case)] // the crate is named after the DLL file: PulsarSplitShim

pub mod filter;

#[cfg(all(windows, any(target_arch = "x86", target_arch = "x86_64")))]
mod rt;
