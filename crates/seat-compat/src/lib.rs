// SPDX-License-Identifier: GPL-3.0-only
//! Pulsar Seat compatibility helpers.
//!
//! Owner: the **compat** package. Spec: `spec/local/80-apps-compat.md` §11, `spec/local/90-install.md`
//! §11, `spec/local/10-manager.md` §3.8 (importer). Contract: `docs/agents/architecture.md`.
//!
//! | Module | What |
//! | --- | --- |
//! | [`ops`] | `SystemOps` trait (registry, power, IP Helper, pnputil) + `MemoryOps` for tests |
//! | [`journal`] | undo journal `journal\changes.jsonl`: every system change with its previous value |
//! | [`tweaks`] | named reversible tweaks (lock image, hide FUS, driver updates, palm check, fast startup, sleep, dumps) |
//! | [`preflight`] | explicit check list (build, arch, admin, Secure Boot / HVCI / SAC reported only, Safe Mode) |
//! | [`coexist`] | reference product keys, non-default TermService DLL, foreign input filters |
//! | [`cpu`] | CPU sets per seat: list, validate, balanced split, group affinities |
//! | `launcher` (Windows) | suspended start in a job with hard / soft CPU limits |
//! | [`srcip`] | source IP per seat: address list, journaled extra addresses, per-seat rule model |
//! | [`drivers`] | signed driver package staging through the driver store |
//! | [`wfp`] | client of the source-IP callout driver `drivers/seat-wfp`: session -> seat address map, DOS -> NT paths |
//! | [`importer`] | reference `default_configuration.json` -> our config |
//! | [`seatapi`] | "which seat am I on" pipe client + drop-in name mapping |
//! | [`apps`] | known single-instance / profile-locking apps, `Local\` object names |
//! | [`defender`] | opt-in Microsoft Defender exclusions for our folders / processes (`MSFT_MpPreference`), journaled |
//! | [`rights`] | opt-in "Deny log on locally" for RDP-signed-in seat accounts (`LsaAddAccountRights`), journaled |
//! | [`user_env`] | per-seat environment variables in the seat user's `Environment` key, journaled |
//! | [`steam`] | Steam folders, legacy proxy-DLL report, journaled quarantine / restore |
//! | [`certs`] | opt-in import of the package's own publisher certificate at install (visible choice), journaled |
//! | [`features`] | opt-in optional Windows features at install (VirtualMachinePlatform, Hyper-V, Sandbox) via DISM, journaled |
//! | [`setup`] | install / uninstall steps for the service sub-commands, exit codes |
//! | `winsec` (Windows) | trust checks for SYSTEM-only files: owner, DACL, links; owner + DACL hardening |
//!
//! The app-facing helper DLL (`pulsar-seat-api.dll`, x86 / x64 / ARM64 / ARM64EC) is the
//! separate crate `apihelper/` (own workspace) on top of [`seatapi`].
//!
//! No kernel components of its own (the [`wfp`] driver is a separate package), no injection, no Windows Update blocking, no silent policy
//! writes, no firewall rules, no security-state changes. Policies, Defender exclusions and
//! logon rights are written only as visible opt-ins the administrator turns on, each
//! journaled and undone on disable / uninstall (compat-tweaks 2026-11-17).

pub mod apps;
pub mod certs;
pub mod coexist;
pub mod cpu;
pub mod defender;
pub mod drivers;
pub mod features;
pub mod gpu_pref;
pub mod guide_guard;
pub mod hvsock;
pub mod hyperv;
pub mod hyperv_vm;
pub mod importer;
pub mod journal;
pub mod ops;
pub mod preflight;
pub mod rights;
pub mod seatapi;
pub mod setup;
pub mod srcip;
pub mod steam;
pub mod tweaks;
pub mod user_env;
pub mod wfp;

#[cfg(windows)]
pub mod launcher;
#[cfg(windows)]
pub mod win;
#[cfg(windows)]
pub mod win_certs;
#[cfg(windows)]
pub mod win_extra;
#[cfg(windows)]
pub mod winsec;

pub use journal::Journal;
pub use ops::{MemoryOps, SystemOps};
#[cfg(windows)]
pub use win::WinOps;

/// Create a restore point (Windows; untested on hardware).
#[cfg(windows)]
pub fn create_restore_point(description: &str) -> Result<i64, ops::OpsError> {
    win::create_restore_point(description)
}
