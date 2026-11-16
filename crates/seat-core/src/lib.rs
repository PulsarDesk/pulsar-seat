// SPDX-License-Identifier: GPL-3.0-only
//! Pulsar Seat core: the shared model every other crate builds on.
//!
//! - [`ids`]: seat numbers, seat masks, stable device keys.
//! - [`device`]: device kinds, share policies, the live inventory record [`device::Device`].
//! - [`config`]: the versioned configuration document (`%ProgramData%\Pulsar Seat\config.json`).
//! - [`validate`]: config validation and save-time conflict detection.
//! - [`assign`]: effective seat of a device (explicit, inherited from a hub, default).
//! - [`state`]: product and per-seat run-time states, failure reasons.
//! - [`store`]: [`store::ConfigStore`] and [`store::SecretStore`] traits + file/memory impls.
//! - [`pnp`]: [`pnp::PnpTree`] trait (device tree access) and pure key-building helpers.
//! - [`backend`]: the swappable backend traits (session, display, input, audio, device gate).
//! - [`mock`]: in-memory implementations of every trait (feature `mock`, and in tests).
//! - [`consts`]: names shared by service, manager, installer, hosts and drivers.
//! - [`error`]: the one wire error ([`error::ProtoError`], [`error::ErrorCode`]).
//! - [`remote`]: the remote seat kind (from Pulsar Split): `RemoteSeat`, Split model,
//!   validation, ports, stream-server config, watchdog.
//! - [`spawn`]: the process-wide spawn lock (handle-inheritance rule).
//!
//! No platform code lives here; the crate builds and tests on any host. Contract summary:
//! `docs/agents/architecture.md`.

pub mod assign;
pub mod backend;
pub mod config;
pub mod consts;
pub mod device;
pub mod error;
pub mod ids;
pub mod latency;
pub mod pnp;
pub mod remote;
pub mod secret;
pub mod spawn;
pub mod state;
pub mod store;
pub mod validate;

#[cfg(any(test, feature = "mock"))]
pub mod mock;

pub use ids::{DeviceKey, MAX_SEATS, SeatMask, SeatNumber};
pub use secret::Secret;

/// Injected monotonic time source, so timeouts and back-offs are testable.
pub trait Clock: Send + Sync {
    /// Milliseconds since an arbitrary fixed origin (monotonic).
    fn now_ms(&self) -> u64;
}

/// Real monotonic clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        static ORIGIN: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        let o = *ORIGIN.get_or_init(std::time::Instant::now);
        u64::try_from(o.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}
