// SPDX-License-Identifier: GPL-3.0-only
//! Pulsar Split multi-session support, Path A (Windows Server + RD Session Host).
//!
//! Spec: `spec/remote/30-session-rds.md`, `spec/remote/20-service.md` §5–§7. Contract:
//! `docs/split/agents/contracts.md` §1.
//!
//! Layout: pure decision logic lives in the public modules and runs on any platform. Every
//! system access goes through a trait ([`rds::Registry`], [`users::Accounts`],
//! [`discover::Wts`], …). The real implementations are in the `#[cfg(windows)]` module
//! [`win`]; in-memory fakes are in [`fake`] (feature `fake` or tests).

pub mod acl;
pub mod discover;
pub mod edition;
pub mod error;
pub mod helper_proto;
pub mod journal;
pub mod launch;
pub mod manager;
pub mod rds;
pub mod sweep;
pub mod users;
pub mod wait;

#[cfg(windows)]
pub mod win;

#[cfg(any(test, feature = "fake"))]
pub mod fake;

pub use error::{FailureReason, SessionError};

/// Injected time source (same shape in every Pulsar Split crate).
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
