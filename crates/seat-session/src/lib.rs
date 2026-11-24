// SPDX-License-Identifier: GPL-3.0-only
//! Pulsar Seat session backends — implements [`seat_core::backend::SessionBackend`].
//!
//! Spec: `spec/local/30-sessions.md` §6, `spec/local/00-architecture.md` §5.2 / §5.2.1, plan.md
//! "Decisions for phases 1–5". Contract: `docs/agents/architecture.md` §4.1. Details and
//! what is untested: `README.md` next to this crate.
//!
//! | Backend | Module | Where |
//! | --- | --- | --- |
//! | `rds_loopback` (default) | [`backends::rds_loopback`] | Windows Server + RD Session Host |
//! | `termsrv_wrapper` (experimental, off) | [`backends::termsrv_wrapper`] + [`termwrap`] (feature `experimental-client-sku`) | client SKUs with an operator-installed wrapper |
//! | `fast_user_switch` | [`backends::fast_user_switch`] | everywhere (runtime fallback) |
//! | `desktops` | [`backends::desktops`] | kiosk seats in the console session |
//!
//! Remote seats (merge-plan.md step 7) run on the same `rds_loopback` path: [`remote`]
//! holds their session manager (headless hold, `PSS…` discovery, sweeps) and shares the
//! edition probe, the one Terminal Server settings writer + journal ([`rds`]) and the WTS
//! model ([`wts`]) with the local backends. `fast_user_switch` and `desktops` are local
//! only.
//!
//! Layout: pure decision logic in the public modules (runs and is tested on any host);
//! every system access goes through a trait ([`registry::Registry`], [`wts::Wts`],
//! [`edition::EditionProbe`], [`rds::RdsProbe`], [`launch::SessionLauncher`],
//! [`accounts::Accounts`]). Real implementations are in [`win`] (`cfg(windows)`), fakes in
//! [`fake`] (feature `fake`, and in tests).
//!
//! Rules: never modify termsrv / LSM, never patch the kernel, never write the Winlogon
//! console auto-logon values for user accounts, no undocumented RPC; every system write is
//! journaled and reverted exactly.

pub mod accounts;
pub mod acl;
pub mod alias;
pub mod autologon;
pub mod backends;
pub mod console_account;
pub mod credprov;
pub mod edition;
pub mod error;
pub mod host_account;
pub mod journal;
pub mod kiosk;
pub mod kiosk_account;
pub mod launch;
pub mod listener;
pub mod loopback_listener;
pub mod pe;
pub mod rds;
pub mod registry;
pub mod remote;
pub mod version;
pub mod wrds;
pub mod wts;

/// The one termsrv-wrapper integration (Seat's port merged with Pulsar Split's
/// `split-termwrap` at step 7). Always compiled so that status, disable, the boot gate and
/// the uninstall restore work in every build; turning it **on** needs the cargo feature
/// `experimental-client-sku` ([`termwrap::TermwrapPolicy::enable_built_in`]), an x64 host
/// and the admin's warning ack. Off by default.
pub mod termwrap;

#[cfg(windows)]
pub mod win;

#[cfg(any(test, feature = "fake"))]
pub mod fake;

use std::sync::Arc;

pub use backends::desktops::{DesktopMode, DesktopsOptions};
pub use backends::{SessionDeps, Sleeper, ThreadSleeper};
pub use error::SysError;
use seat_core::backend::session::{SessionBackend, SessionBackendKind};

/// Builds the backend for `kind`. The service keeps the config unchanged and falls back
/// to [`SessionBackendKind::FastUserSwitch`] when the probe of the configured kind is not
/// available (architecture §4.1); experimental kinds are never a fallback.
pub fn create_backend(kind: SessionBackendKind, deps: SessionDeps) -> Arc<dyn SessionBackend> {
    create_backend_with(kind, deps, DesktopsOptions::platform())
}

/// [`create_backend`] with options for the desktops backend.
pub fn create_backend_with(kind: SessionBackendKind, deps: SessionDeps, desktops: DesktopsOptions) -> Arc<dyn SessionBackend> {
    match kind {
        SessionBackendKind::RdsLoopback => backends::rds_loopback::rds_loopback(deps),
        SessionBackendKind::TermsrvWrapper => backends::termsrv_wrapper::termsrv_wrapper(deps),
        SessionBackendKind::FastUserSwitch => backends::fast_user_switch::fast_user_switch(deps),
        SessionBackendKind::Desktops => backends::desktops::desktops(deps, desktops),
    }
}

/// The kind the service falls back to when the configured one is not available.
pub const FALLBACK_KIND: SessionBackendKind = SessionBackendKind::FastUserSwitch;

/// Warning text for the experimental backend in `lang` (`None` without the feature).
pub fn experimental_warning(lang: seat_core::i18n::Lang) -> Option<&'static str> {
    backends::termsrv_wrapper::TermsrvWrapperBackend::warning_text(lang)
}
