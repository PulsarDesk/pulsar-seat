// SPDX-License-Identifier: GPL-3.0-only
//! Remote seats on the `rds_loopback` path (Windows Server + RD Session Host, or the
//! experimental wrapper kind): seat users, the headless session hold, WTS discovery by
//! `PSS…` client name, launch into the seat session, run state and sweeps. Merged from
//! Pulsar Split's `split-session` at merge-plan.md step 7.
//!
//! Shared with the local kind (one implementation each): edition probe and host class
//! ([`crate::edition`]), the Terminal Server settings writer and its journal
//! ([`crate::rds`], claim [`crate::rds::Claim::Remote`]), the registry
//! ([`crate::registry`]), the WTS session model and both client-name prefixes
//! ([`crate::wts`]), command-line / environment helpers ([`crate::launch`]), the
//! real probes ([`crate::win::probes`]). The headless hold itself is
//! `pulsar-seat-host.exe --headless-hold` (`seat_rdp::headless`).
//!
//! Spec: `spec/remote/30-session-rds.md`, `spec/remote/20-service.md` §5–§7. Contract:
//! `docs/split/agents/contracts.md` §1 (names updated in `docs/agents/architecture.md`
//! section 10).
//!
//! Layout as in the rest of the crate: pure decision logic in the public modules, every
//! system access behind a trait, real implementations in [`win`] (`cfg(windows)`),
//! in-memory fakes in [`fake`] (feature `fake` or tests).

pub mod acl;
pub mod discover;
pub mod error;
pub mod launch;
pub mod manager;
pub mod sweep;
pub mod users;
pub mod wait;

#[cfg(windows)]
pub mod win;

#[cfg(any(test, feature = "fake"))]
pub mod fake;

pub use error::{FailureReason, SessionError};
/// The headless hold's wire protocol (argv, stdin password frame, JSON Lines events).
pub use seat_rdp::headless::proto as helper_proto;

/// Injected time source.
pub trait Clock: Send + Sync {
    /// Milliseconds since any fixed origin (monotonic).
    fn now_ms(&self) -> u64;
}

/// Injected sleep, so wait loops can run against a fake clock in tests.
pub trait Sleeper: Send + Sync {
    fn sleep(&self, d: std::time::Duration);
}

/// Real monotonic clock + thread sleep.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

fn origin() -> std::time::Instant {
    static ORIGIN: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    *ORIGIN.get_or_init(std::time::Instant::now)
}

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        u64::try_from(origin().elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

impl Sleeper for SystemClock {
    fn sleep(&self, d: std::time::Duration) {
        std::thread::sleep(d);
    }
}

/// A kernel handle. On Windows this is `std::os::windows::io::OwnedHandle`; elsewhere a
/// placeholder so the pure parts and the traits compile on any host.
#[cfg(windows)]
pub type OwnedHandle = std::os::windows::io::OwnedHandle;

#[cfg(not(windows))]
#[derive(Debug)]
pub struct OwnedHandle(pub u64);
