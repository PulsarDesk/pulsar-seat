// SPDX-License-Identifier: GPL-3.0-only
//! Audio backend: per-seat playback and recording endpoints (spec 60 §8). Implemented in
//! `seat-media`.
//!
//! Default design 3: each RDP seat session has its own virtual endpoints; the seat host's
//! RDP audio channels (playback [MS-RDPEA], capture [MS-RDPEAI]) are bridged to the seat's
//! assigned hardware endpoint by endpoint id through WASAPI, in the host process. The
//! Windows Audio service is never modified.

use super::{Availability, BackendResult, HotplugSink};
use crate::device::Device;
use crate::ids::{DeviceKey, SeatNumber};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioBackendKind {
    /// RDP audio redirection bridged to the seat's endpoint (RDP session backends only).
    #[default]
    RdpBridge,
    /// Everything plays on the console's devices; endpoint assignment is informational.
    ConsoleShared,
    /// Audio routing off.
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioCaps {
    pub per_seat_render: bool,
    pub per_seat_capture: bool,
    /// Endpoints given only to other seats stay visible in the console session (always
    /// true without a driver; the manager says so, spec 60 §8.3).
    pub console_sees_all: bool,
    pub test_tone: bool,
}

/// What a seat should use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioBinding {
    pub render: Option<DeviceKey>,
    pub capture: Option<DeviceKey>,
    /// When the assigned endpoint is absent, use a shared endpoint instead (off by default).
    pub fallback_to_shared: bool,
}

/// Resolved endpoints the seat host opens (by MMDevice id).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AudioRoute {
    pub seat: Option<SeatNumber>,
    pub render_endpoint_id: Option<String>,
    pub capture_endpoint_id: Option<String>,
    /// Snake_case notes (`render_absent`, `using_shared_fallback`).
    pub notes: Vec<String>,
}

/// Service-side audio backend.
pub trait AudioRouter: Send + Sync {
    fn kind(&self) -> AudioBackendKind;
    fn caps(&self) -> AudioCaps;
    fn availability(&self) -> Availability;
    /// Live endpoints (`AudioRender` / `AudioCapture`, `DeviceDetails::Audio`).
    fn enumerate(&self) -> BackendResult<Vec<Device>>;
    /// Resolve a seat's binding to endpoint ids (re-run on hot-plug; the host re-binds
    /// within about a second without restarting the session).
    fn bind_seat(&self, seat: SeatNumber, binding: &AudioBinding) -> BackendResult<AudioRoute>;
    fn release_seat(&self, seat: SeatNumber) -> BackendResult<()>;
    /// Short WASAPI test tone on exactly this endpoint (never the default device).
    fn test_tone(&self, key: &DeviceKey, duration_ms: u32) -> BackendResult<()>;
    fn set_hotplug_sink(&self, sink: HotplugSink);
}
