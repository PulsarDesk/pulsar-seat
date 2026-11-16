// SPDX-License-Identifier: GPL-3.0-only
//! Pulsar Split per-process isolation, the shared part (spec `50-shim-isolation.md`,
//! contract `docs/split/agents/contracts.md` §4.2).
//!
//! Used by the service (seat map writer, installer, DXVK deploy, health decoding) and by
//! the `shim/` workspace (the injected module, the launcher and `shimctl`). Everything a
//! hook decides is a pure function here, so it is unit-tested without Windows:
//!
//! | Module | What |
//! | --- | --- |
//! | [`seatmap`] | authoritative session -> seat map (`HKLM\SOFTWARE\PulsarSplit\Runtime\Seats`) |
//! | [`kiosk`] | local kiosk seat -> device map (`HKLM\SOFTWARE\PulsarSplit\Runtime\KioskSeats`), round 4 |
//! | [`kiosk_cursor`] | a kiosk seat's own cursor position for its apps (named section), parity 2026-11-17 |
//! | [`decide`] | role of a process, device classification, HID access, focus filter, masking, HKCU redirect, hook plan |
//! | [`steam`] | per-seat Steam IPC name: launch arguments, environment, command-line patch |
//! | [`guard`] | crash guard (fault counter per target) |
//! | [`health`] | `ShimHealth` frame on `\\.\pipe\PulsarSplit.ShimHealth` |
//! | [`install`] | AppHelp custom-database installer over the [`install::ShimSystem`] trait |
//! | [`sdb`] | database tree built from the target list (written by apphelp's own writer API) |
//! | [`launch`] | PE bitness and Windows command-line quoting for the launcher |
//! | [`dxvk`] | per-game DXVK D3D8/D3D9 deploy with download + SHA-256 verify, backup and undo |
//! | [`journal`] | restore records (`%ProgramData%\Pulsar Seat\shim\journal\shim.jsonl`) |
//!
//! Real Windows implementations of the traits are in [`win`] (`#[cfg(windows)]`); in-memory
//! fakes are in [`fake`] (tests, or the `fake` feature).

pub mod decide;
pub mod dxvk;
pub mod error;
pub mod guard;
pub mod health;
pub mod install;
pub mod journal;
pub mod kiosk;
pub mod kiosk_cursor;
pub mod launch;
pub mod regval;
pub mod sdb;
pub mod seatmap;
pub mod steam;

#[cfg(any(test, feature = "fake"))]
pub mod fake;
#[cfg(windows)]
pub mod win;
#[cfg(windows)]
pub mod winfs;

pub use decide::{
    Access, DevNodeInfo, DeviceOrigin, HookPlan, ProcessFacts, Role, ShimConfig, classify, classify_full, classify_nodes,
    filter_focus, hid_access, hkcu_redirect, hook_plan, hook_plan_full, mask_remote, role, role_full, wnf_known,
};
pub use kiosk::{KIOSK_SEAT_ENV, KioskFlags, KioskMapReader, KioskMapWriter, KioskSeatEntry};
pub use dxvk::{Bitness, DXVK_NOTICE, DxvkDeployer, DxvkDll, DxvkRelease, FileCopy, dxvk_plan, verify_sha256};
pub use error::ShimError;
pub use guard::FaultCounter;
pub use health::{ShimHealth, decode_health, encode_health};
pub use install::{ModulePaths, ShimInstallStatus, ShimInstaller, ShimTarget, UninstallOutcome, user_target_key};
pub use regval::RegValue;
pub use seatmap::{SeatFlags, SeatMapEntry, SeatMapReader, SeatMapWriter, decode_entry, encode_entry};
pub use steam::{steam_env, steam_launch_args};

/// Our AppHelp custom database id. Generated for Pulsar Split (2026-11-12); never the
/// reference product's id (spec 50 §2.5, §15).
pub const SHIM_DB_GUID: &str = "{d9a4fcd5-83a4-45c2-8caa-b3a4f973ad03}";
/// Database name shown by `sdbinst` / Programs and Features.
pub const SHIM_DB_NAME: &str = "Pulsar Split Isolation";
/// Shim (library entry) and layer name inside our database.
pub const SHIM_NAME: &str = "PulsarSplitIsolation";
pub const LAYER_NAME: &str = "PulsarSplitIsolation";
/// Module file name. Never a Windows-like name (spec 50 §2.5).
/// `MODULE_X64` is the name of the System32 copy: the x64 build on x64 PCs, the ARM64 build
/// on ARM64 PCs (`launch::system32_module`).
pub const MODULE_X64: &str = "PulsarSplitShim.dll"; // System32 (native 64-bit)
pub const MODULE_X86: &str = "PulsarSplitShim.dll"; // SysWOW64 (x86)
pub const MODULE_ARM64: &str = "PulsarSplitShim.dll"; // shim\arm64 (native ARM64, Detours)
/// `ShimDisabled` (DWORD kill switch), `Verbosity` (DWORD 0–4), `MaskApps` (MULTI_SZ,
/// lowercase full paths), `DisabledTargets` (MULTI_SZ, lowercase exe names, written by
/// the crash guard), `WnfBuilds` (MULTI_SZ, Windows builds on which the focus WNF names
/// were verified). Subkey `Targets\<sha256>` = user target list.
pub const REG_SHIM: &str = r"SOFTWARE\PulsarSplit\Shim";
/// Volatile; Everyone read, SYSTEM write. Subkey per running seat: `<session id>`.
pub const REG_SEATMAP: &str = r"SOFTWARE\PulsarSplit\Runtime\Seats";
/// Volatile; Everyone read, SYSTEM write. Subkey per running kiosk shell: `<NN>` (seat
/// number, two digits). See [`kiosk`].
pub const REG_KIOSKMAP: &str = r"SOFTWARE\PulsarSplit\Runtime\KioskSeats";
pub const HEALTH_PIPE: &str = r"\\.\pipe\PulsarSplit.ShimHealth";
/// ETW (TraceLogging) provider of the module. Generated for Pulsar Split.
pub const ETW_PROVIDER_GUID: &str = "81916b1b-e779-43f0-9ba0-6ecf949e9db9";
pub const ETW_PROVIDER_NAME: &str = "PulsarSplit.Shim";
/// Event-log source for errors only (registered by the installer package).
pub const EVENT_SOURCE: &str = "PulsarSplitShim";
/// Module version reported in health frames.
pub const MODULE_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Install root of our binaries; processes under it never get hooks.
pub const INSTALL_DIR_NAME: &str = "Pulsar Split";

/// Registry value names under [`REG_SHIM`].
pub mod values {
    pub const SHIM_DISABLED: &str = "ShimDisabled";
    pub const VERBOSITY: &str = "Verbosity";
    pub const MASK_APPS: &str = "MaskApps";
    pub const DISABLED_TARGETS: &str = "DisabledTargets";
    pub const WNF_BUILDS: &str = "WnfBuilds";
    pub const TARGETS_SUBKEY: &str = "Targets";
}

/// Time source, same shape in every crate (contracts.md §0.3).
pub trait Clock: Send + Sync {
    fn now_ms(&self) -> u64;
}
