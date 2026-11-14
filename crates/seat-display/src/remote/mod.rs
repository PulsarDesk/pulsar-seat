// SPDX-License-Identifier: GPL-3.0-only
//! Remote seat kind: per-seat virtual display (contract: `docs/split/agents/contracts.md` §3,
//! behaviour: `spec/remote/40-display-hdr.md`). Was the `split-display` crate until merge
//! step 8; both kinds' backends are listed behind one [`crate::SeatDisplayBackend`] trait.
//!
//! * [`modes`], [`adapters`], [`hdr`], [`seat`], [`frame`], [`ioctl`] are pure and
//!   platform-neutral; their unit tests run anywhere.
//! * [`backend`] holds the [`DisplayBackend`] trait and its three implementations:
//!   [`RdpSessionDisplay`] (stock RDP display of the seat session, SDR, default for remote
//!   seats), [`VddBackend`] (signed third-party Virtual-Display-Driver, console seat only)
//!   and [`PulsarIdd`] (own IddCx driver in `drivers/split-idd`, unsigned, never default).
//! * Every system access sits behind a trait. Real implementations live in `win`
//!   (`#[cfg(windows)]`), in-memory fakes in [`fake`] (feature `fake` or tests).

pub mod adapters;
pub mod backend;
pub mod error;
#[cfg(any(test, feature = "fake"))]
pub mod fake;
pub mod frame;
pub mod hdr;
pub mod ioctl;
pub mod journal;
pub mod modes;
pub mod seat;
#[cfg(windows)]
pub mod win;

pub use adapters::{
    AdapterChoice, AdapterEnumerator, Luid, RenderAdapter, default_adapter, resolve_adapter,
};
pub use backend::pulsar_idd::{DeviceHandle, DeviceIo, DriverEvent, PulsarIdd};
pub use backend::rdp::{RdpSessionDisplay, SessionDisplayProbe};
pub use backend::vdd::{
    ConsoleSession, ModeSetter, VddBackend, VddEnv, VddMonitor, VddMonitors, VddPipe,
    VddSettingsFile,
};
pub use backend::{BackendInfo, BackendKind, DisplayBackend, SeatDisplayHandle, choose_backend};
pub use error::DisplayError;
pub use frame::{FrameHeaderV1, FrameObjectNames, OBJECT_SDDL, object_names, parse_header};
pub use hdr::{HdrCaps, HdrUnavailable, hdr_availability, iddcx_for_build};
pub use ioctl::Guid;
pub use modes::{
    ModeLimits, ModeRequest, ResolvedMode, SCALE_STEPS, ScaleMode, dynamic_scale, resolve_mode,
};
pub use seat::{
    DisplayPhase, DisplayState, FrameFormat, HdrLuminance, ModeResult, ModeStatus, ReadyInfo,
    SeatDisplay,
};

/// Injected wall/monotonic time in milliseconds (same shape in every remote-kind module).
pub trait Clock: Send + Sync {
    fn now_ms(&self) -> u64;
}

/// Injected sleep, so waits are instant in tests (the fake clock advances instead).
pub trait Sleeper: Send + Sync {
    fn sleep_ms(&self, ms: u64);
}

/// Real clock + sleeper (monotonic since construction, plus the Unix time at start so
/// journal timestamps are real dates).
#[derive(Debug)]
pub struct SystemClock {
    start: std::time::Instant,
    unix_start_ms: u64,
}

impl Default for SystemClock {
    fn default() -> Self {
        let unix_start_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        Self {
            start: std::time::Instant::now(),
            unix_start_ms,
        }
    }
}

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        self.unix_start_ms + self.start.elapsed().as_millis() as u64
    }
}

impl Sleeper for SystemClock {
    fn sleep_ms(&self, ms: u64) {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
}
