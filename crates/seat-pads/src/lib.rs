// SPDX-License-Identifier: GPL-3.0-only
//! Pulsar Split virtual controllers (contracts.md §5, spec 60).
//!
//! - [`PadBackend`] / [`VirtualPad`]: one trait pair, three backends:
//!   [`vigem::VigemBackend`] (installed ViGEmBus, X360 + DS4),
//!   [`usbip::UsbipDs5Backend`] (usbip-win2 + our loopback USB/IP DualSense emulator) and
//!   [`pulsarpad::PulsarPadBackend`] (own UMDF driver, unsigned, never default).
//! - [`manager::PadManager`]: pad lifecycle per seat (spec 60 §3.5), ownership registry,
//!   orphan cleanup, device security + session id, channel pump, feedback.
//! - Pure modules: [`reports`], [`shm`], [`mapping`], [`ids`], [`descriptors`],
//!   [`features`], [`layout`], [`control`], [`bluetooth`] (plan only).
//! - [`win`]: the real Windows implementations of every system trait.
//! - [`fake`]: in-memory fakes (feature `fake`).

pub mod bluetooth;
pub mod channel;
pub mod control;
pub mod descriptors;
pub mod error;
pub mod features;
pub mod ids;
pub mod journal;
pub mod layout;
pub mod manager;
pub mod mapping;
pub mod nonce;
pub mod owners;
pub mod pulsarpad;
pub mod reports;
pub mod shm;
pub mod types;
pub mod usbip;
pub mod vigem;

#[cfg(windows)]
pub mod win;

#[cfg(any(test, feature = "fake"))]
pub mod fake;

#[cfg(test)]
mod manager_tests;

pub use control::{DenyCode, DetachReason, PadAttach, PadDenied, PadDetach, PadReleased, PadWanted, ReleaseReason};
pub use error::PadError;
pub use ids::{mac_for, marker_serial, pad_sddl};
pub use manager::{OrphanReport, Outgoing, PadManager, PadManagerConfig, PadPhase, PadStatus, SeatPadContext};
pub use owners::{plan_orphans, DeviceNode, DeviceTree, FileOwnerStore, OrphanPlan, OwnerStore, PadOwner, ProcessProbe};
pub use pulsarpad::PulsarPadBackend;
pub use types::{
    buttons, BackendAvailability, BackendId, Battery, Clock, Feedback, FeedbackSink, NullSink, PadBackend, PadCaps,
    PadCreate, PadId, PadKind, PadState, SystemClock, Touch, VirtualPad,
};
pub use usbip::UsbipDs5Backend;
pub use vigem::VigemBackend;
