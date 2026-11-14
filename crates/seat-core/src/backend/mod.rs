// SPDX-License-Identifier: GPL-3.0-only
//! Swappable backends (spec 00 §5.1 principle 4, plan.md "Decisions for phases 1–5").
//!
//! The service owns one instance of each trait, chosen from
//! [`crate::config::BackendSelection`]. Every backend:
//!
//! - reports [`Availability`] on this PC (the manager greys out what is missing);
//! - declares capabilities, so the service and manager know what it can do;
//! - returns [`BackendError`] with a machine-readable [`ErrorKind`] and an optional Win32
//!   code (spec 20 §13.6: our own stable ids + the Windows code);
//! - is `Send + Sync` and synchronous (no async runtime); blocking calls take explicit
//!   timeouts.
//!
//! | Trait | Default backend | Crate |
//! | --- | --- | --- |
//! | [`session::SessionBackend`] | `RdsLoopback` on Server + RDSH; `FastUserSwitch` elsewhere | `seat-session` |
//! | [`display::DisplayBinding`] | `WindowPresenter` (CCD + fullscreen window) | `seat-display` |
//! | [`input::InputRouter`] | `RawInputHost` (user mode, in the host session) | `seat-input` |
//! | [`audio::AudioRouter`] | `RdpBridge` (RDP audio redirection to the seat endpoint) | `seat-media` |
//! | [`gate::DeviceGate`] | `DeviceSecurity` + `SessionDriveLetters` | `seat-media` |
//!
//! Our own drivers (`drivers/`) sit behind the same traits (`KmdfFilter`, `PulsarIdd`,
//! `StorageMinifilter`) and report [`Availability::DriverNotSigned`] until they are signed.

pub mod audio;
pub mod display;
pub mod gate;
pub mod input;
pub mod session;

use crate::device::Device;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub use audio::{AudioBackendKind, AudioBinding, AudioCaps, AudioRoute, AudioRouter};
pub use display::{DisplayBackendKind, DisplayBinding, DisplayCaps, FrameRef, FrameSink, GpuFrameRef, SeatOutputs};
pub use gate::{DeviceGate, GateBackendKind, GateCaps, GatePlan, GateReport, IsolationStatus};
pub use input::{InputBackendKind, InputCaps, InputPacket, InputRoute, InputRouter, InputSink};
pub use session::{SessionBackend, SessionBackendKind, SessionCaps};

/// Whether a backend can be used on this PC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Availability {
    Available,
    /// Usable after a setup step the service can do (with the admin's consent).
    NeedsSetup { what: String },
    /// Our own driver is written but not signed: never loaded (plan.md).
    DriverNotSigned { driver: DriverId },
    /// Our driver is installed but reports another interface major version (fail open).
    DriverVersionMismatch { driver: DriverId, found: u16 },
    /// Wrong Windows edition / role / architecture; `reason` is a snake_case code.
    Unavailable { reason: String },
}

impl Availability {
    pub fn is_available(&self) -> bool {
        matches!(self, Self::Available)
    }
}

/// Our own drivers (`drivers/`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DriverId {
    /// `drivers/seat-input-filter`, keyboard / mouse role (KMDF device upper filter below
    /// kbdclass/mouclass).
    KbdMouFilter,
    /// `drivers/seat-input-filter`, HID role (upper filter on HID collections, create-time
    /// decisions). Same driver and service as [`Self::KbdMouFilter`].
    HidFilter,
    /// `drivers/seat-idd` (IddCx UMDF 2 virtual display).
    Idd,
    /// `drivers/seat-storage-filter` (FltMgr; exception, needs the maintainer's OK).
    StorageMinifilter,
}

/// Load state of one of our drivers, reported by the service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum DriverStatus {
    NotInstalled,
    /// Package in the driver store, not loaded (unsigned builds are never staged).
    InstalledNotLoaded,
    Loaded { interface_major: u16, interface_minor: u16, build: String },
    VersionMismatch { found: u16 },
}

/// Machine-readable error kind. The manager maps it (plus `BackendError::reason`) to
/// Turkish copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// The backend cannot do this at all (capability missing).
    Unsupported,
    /// Prerequisite not ready yet (service, role, display); may succeed later.
    NotReady,
    /// Access denied by Windows.
    Denied,
    NotFound,
    Timeout,
    /// Our driver missing / unsigned / wrong version.
    Driver,
    /// Another product conflicts (reference product, foreign TermService DLL, filters).
    Coexistence,
    /// A Windows policy blocks the action (we never work around it).
    Policy,
    /// Logon failed (wrong / expired password, disabled account, logon hours).
    Logon,
    /// A Windows call failed; see `win32`.
    Os,
    Internal,
}

/// Error from any backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{kind:?}/{reason}: {message}")]
pub struct BackendError {
    pub kind: ErrorKind,
    /// Stable snake_case reason (`rds_not_ready`, `edition_unsupported`, `target_busy`, …).
    pub reason: String,
    /// English diagnostic text for logs (never shown verbatim, never contains secrets).
    pub message: String,
    /// Windows error / HRESULT, when one exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub win32: Option<u32>,
}

impl BackendError {
    pub fn new(kind: ErrorKind, reason: impl Into<String>, message: impl Into<String>) -> Self {
        Self { kind, reason: reason.into(), message: message.into(), win32: None }
    }
    pub fn with_win32(mut self, code: u32) -> Self {
        self.win32 = Some(code);
        self
    }
    pub fn unsupported(what: &str) -> Self {
        Self::new(ErrorKind::Unsupported, "unsupported", format!("{what} is not supported by this backend"))
    }
}

pub type BackendResult<T> = Result<T, BackendError>;

/// One backend as the manager's backend page shows it (`get_backends`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BackendInfo {
    /// Snake_case kind name (`rds_loopback`, `window_presenter`, …).
    pub kind: String,
    pub availability: Availability,
    pub experimental: bool,
    /// Selected in the config.
    pub selected: bool,
    /// Actually in use now.
    pub active: bool,
    /// The backend's caps struct serialized (`SessionCaps`, `DisplayCaps`, …).
    pub caps: serde_json::Value,
    /// Snake_case notes (`seat_one_sees_other_endpoints`, `input_not_isolated`, …).
    pub notes: Vec<String>,
}

/// Device arrival / removal / change, pushed by a backend's hot-plug listener.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HotplugEvent {
    Arrived { device: Box<Device> },
    Removed { key: crate::ids::DeviceKey },
    Changed { device: Box<Device> },
}

/// Callback a backend calls from its own listener thread. Must not block.
pub type HotplugSink = Arc<dyn Fn(HotplugEvent) + Send + Sync>;
