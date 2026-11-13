// SPDX-License-Identifier: GPL-3.0-only
//! Pulsar Split setup logic: the bodies of the service sub-commands `install`,
//! `reconcile`, `uninstall [--remove-user-data]` and `check-coexistence --json`
//! (spec 90 §9.1), coexistence detection (spec 90 §8), third-party driver packages
//! (download + SHA-256 verify, `pnputil` plans) and change-journal replay (spec 90 §9.2).
//!
//! Every system access goes through a trait ([`coexist::SystemProbe`],
//! [`system::SetupRegistry`], [`system::SetupFs`], [`system::FirewallGroup`],
//! [`system::DeviceManager`], [`drivers::CommandRunner`], [`drivers::Fetcher`], …). Real
//! implementations live in [`win`] (`#[cfg(windows)]`), in-memory ones in [`fake`].
//!
//! Hard rules honoured here: no root / TrustedPublisher certificates, no AV exclusions,
//! nothing copied to `System32`, no HTTP listener (the fetcher is a client), no secrets in
//! argv / env / logs, the reference product's registry, files, service, rules, shim DB
//! and drivers are never edited.

pub mod applier;
pub mod b64;
pub mod cli;
pub mod cmds;
pub mod coexist;
pub mod drivers;
pub mod error;
pub mod journal;
pub mod layout;
pub mod system;
pub mod timefmt;
#[cfg(windows)]
pub mod win;
#[cfg(any(test, feature = "fake"))]
pub mod fake;

pub use cmds::{
    ExitCode, InstallOptions, ReconcileHook, RestoreHook, RunReport, SetupCtx, cmd_check_coexistence, cmd_install,
    cmd_reconcile, cmd_uninstall,
};
pub use coexist::{
    CoexistMode, CoexistReport, Finding, PassiveReason, RepairItem, ServiceFacts, Signal, SystemProbe, evaluate,
};
pub use drivers::{
    CmdOutput, CommandRunner, DriverDetect, DriverInstall, DriverPackage, Fetcher, PublishedDriver,
    load_driver_manifest, parse_enum_drivers, pnputil_add, pnputil_delete, verify_sha256,
};
pub use error::SetupError;
pub use journal::{JournalRecord, ReplayReport, RestoreApplier, replay_all};
pub use system::Clock;
