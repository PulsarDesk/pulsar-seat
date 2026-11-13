// SPDX-License-Identifier: GPL-3.0-only
//! Real Windows implementations of the setup traits. Only `#[cfg(windows)]`.
//!
//! Tested on the dev machine only in read-only form (registry/SCM/cert/firewall/PnP
//! enumeration, temp-folder file operations). Writes to HKLM, firewall removal, devnode
//! creation/removal, pnputil and NetUserDel run only inside an elevated installer and are
//! **untested** (see the crate README).

pub mod accounts;
pub mod devices;
pub mod fetch;
pub mod firewall;
pub mod fs;
pub mod probe;
pub mod registry;
pub mod runner;
pub mod scm;

use crate::cmds::{InstallOptions, ReconcileHook, RestoreHook, SetupCtx};
use crate::drivers::machine_arch;
use crate::layout::Layout;
use crate::system::Clock;

pub(crate) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Wall clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
    }
}

/// Context over the real machine; the integrator adds the package hooks.
pub fn windows_ctx(
    layout: Layout,
    options: InstallOptions,
    hooks: Vec<Box<dyn RestoreHook>>,
    reconcile_hooks: Vec<Box<dyn ReconcileHook>>,
) -> SetupCtx {
    SetupCtx {
        layout,
        arch: machine_arch().into(),
        options,
        probe: Box::new(probe::WinProbe),
        registry: Box::new(registry::WinRegistry),
        fs: Box::new(fs::WinFs),
        firewall: Box::new(firewall::WinFirewall),
        devices: Box::new(devices::WinDevices),
        runner: Box::new(runner::WinRunner),
        accounts: Box::new(accounts::WinAccounts),
        fetcher: Box::new(fetch::WinHttpFetcher),
        services: Box::new(scm::WinServices),
        clock: Box::new(SystemClock),
        hooks,
        reconcile_hooks,
    }
}
