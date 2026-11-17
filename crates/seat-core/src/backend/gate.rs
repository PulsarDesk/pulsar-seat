// SPDX-License-Identifier: GPL-3.0-only
//! Device gate: USB storage, cameras, card readers, serial ports, printers and other
//! devices per seat (spec 70 §5). Implemented in `seat-media`.
//!
//! Several gates can be active at once (`BackendSelection::gates`), each handling the
//! device kinds it supports; the service gives every gate the same plan and merges the
//! reports. Enforcement uses documented security objects first: device security
//! descriptors with the owning seat's logon SID, per-logon-session drive letters, RDP
//! redirection policy. The minifilter is an exception that needs the maintainer's OK.

use super::{Availability, BackendResult, DriverId, HotplugSink};
use crate::device::{Device, DeviceKind};
use crate::ids::{DeviceKey, SeatMask, SeatNumber};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateBackendKind {
    /// Device security descriptor (SetupAPI security property + secure-open characteristic)
    /// allowing SYSTEM, Administrators (configurable) and the owning seats' logon SIDs.
    DeviceSecurity,
    /// Global letter removed for a one-seat volume; the session agent defines it in the
    /// owner's logon-session DOS-device namespace.
    SessionDriveLetters,
    /// Consumer-facing camera switch per seat user (consent store); coarse, per user.
    CameraConsent,
    /// Our FltMgr minifilter (`drivers/seat-storage-filter`): exception, needs the
    /// maintainer's explicit OK; unloaded until signed.
    StorageMinifilter,
    /// Printer security descriptor per seat (spec 70 §5.2(5), optional): the print queues
    /// of a seat-owned USB printer let only the owning seats' logon SIDs print. Per-seat use,
    /// not per-seat visibility (the queue may still be listed).
    PrinterAcl,
}

/// Device setup classes that may get a class-level default descriptor (spec 70 §5.2(2)).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceClassTarget {
    DiskDrive,
    Cdrom,
    Volume,
    Ports,
    /// Raw-open device classes added 2026-11-17 (compat-tweaks): same shape as CD-ROM /
    /// Ports (opened by programs through a raw device path, never part of the system
    /// disk path).
    Modem,
    MultiportSerial,
    Floppy,
    TapeDrive,
    MediumChanger,
}

impl DeviceClassTarget {
    /// The classes used when the admin turned the feature on without choosing: the two with
    /// the least risk for the system disk (raw optical and serial opens).
    pub const DEFAULT: [DeviceClassTarget; 2] = [DeviceClassTarget::Cdrom, DeviceClassTarget::Ports];

    /// Setup class GUID (lowercase, no braces; WDK "System-defined device setup classes").
    pub const fn class_guid(self) -> &'static str {
        match self {
            Self::DiskDrive => "4d36e967-e325-11ce-bfc1-08002be10318",
            Self::Cdrom => "4d36e965-e325-11ce-bfc1-08002be10318",
            Self::Volume => "71a27cdd-812a-11d0-bec7-08002be2092f",
            Self::Ports => "4d36e978-e325-11ce-bfc1-08002be10318",
            Self::Modem => "4d36e96d-e325-11ce-bfc1-08002be10318",
            Self::MultiportSerial => "50906cb8-ba12-11d1-bf5d-0000f805f530",
            Self::Floppy => "4d36e980-e325-11ce-bfc1-08002be10318",
            Self::TapeDrive => "6d807884-7d21-11cf-801c-08002be10318",
            Self::MediumChanger => "ce5939ae-ebde-11d0-b181-0000f8753ec4",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateCaps {
    pub kinds: Vec<DeviceKind>,
    /// Revokes open handles on ownership change (dismount / devnode restart).
    pub revokes_handles: bool,
    pub driver: Option<DriverId>,
}

/// An interactive logon of a seat (logon SIDs change at every logon).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatLogon {
    pub seat: SeatNumber,
    pub session_id: u32,
    /// `S-1-5-5-X-Y` logon SID of the seat's interactive session.
    pub logon_sid: String,
    /// The user SID (for per-user mechanisms like camera consent).
    pub user_sid: String,
}

/// Everything a gate needs to (re)apply: effective masks + current logons.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct GatePlan {
    pub devices: Vec<(DeviceKey, SeatMask)>,
    pub logons: Vec<SeatLogon>,
    /// Let Administrators through every seat's isolation (spec 70 q11; default true).
    pub admins_bypass: bool,
    /// Classes that get a closed class-level default descriptor (spec 70 §5.2(2)); empty =
    /// feature off (the gate restores any class value it changed).
    #[serde(default)]
    pub class_defaults: Vec<DeviceClassTarget>,
    /// Host accounts that print to a seat's own queues for it (`printer_acl` gate only):
    /// `(seat, account SID)` of each seat whose host redirects the seat's printers and runs
    /// as the seat's own account. Added 2026-11-14 (round 4 local-gaps, additive).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub printer_hosts: Vec<(crate::ids::SeatNumber, String)>,
}

/// Which enforcement applies to a device (manager isolation-status row, spec 70 §5.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolationStatus {
    Descriptor,
    SessionLetter,
    Minifilter,
    CameraConsent,
    Shared,
    NotSupported,
    Network,
    /// Smart-card readers (parity review 2026-11-17): RDP seats get only their own
    /// readers through the smart-card redirect; Pulsar-patched processes of every seat see
    /// only theirs (`seat_shim_core::readers`). Programs without the module still see all.
    SeatFilter,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct GateReport {
    pub applied: Vec<(DeviceKey, IsolationStatus)>,
    pub failed: Vec<(DeviceKey, super::BackendError)>,
    /// A volume lock / devnode restart was vetoed; the admin must close files first.
    pub vetoed: Vec<DeviceKey>,
}

/// Service-side device gate.
pub trait DeviceGate: Send + Sync {
    fn kind(&self) -> GateBackendKind;
    fn caps(&self) -> GateCaps;
    fn availability(&self) -> Availability;
    /// Live devices this gate handles (USB hubs, storage, cameras, …).
    fn enumerate(&self) -> BackendResult<Vec<Device>>;
    /// Apply the plan (idempotent; re-run at service start and every seat logon/logoff).
    fn apply(&self, plan: &GatePlan) -> BackendResult<GateReport>;
    /// Undo every change this gate made (disable / uninstall / reset).
    fn revert_all(&self) -> BackendResult<()>;
    fn set_hotplug_sink(&self, sink: HotplugSink);
}
