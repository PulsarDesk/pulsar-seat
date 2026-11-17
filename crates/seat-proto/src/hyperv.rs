// SPDX-License-Identifier: GPL-3.0-only
//! Hyper-V helper messages (`hyper_v` request, parity review 2026-11-17; the helper itself
//! is `seat_compat::hyperv`).

use serde::{Deserialize, Serialize};

/// What the manager can ask for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HyperVAction {
    Status,
    /// Turn the Hyper-V feature on (reboot needed). `confirm` = the operator agreed.
    EnableFeature { confirm: bool },
    /// Create (or update) the seat's adapter with this address.
    SetSeatNic { seat: u8, address: String, prefix_len: u8 },
    RemoveSeatNic { seat: u8 },
}

/// Answer of every action.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct HyperVStatus {
    /// `enabled`, `disabled`, `enable_pending`, `unavailable` (no such feature on this SKU).
    pub feature: String,
    pub switch_present: bool,
    pub seat_nics: Vec<SeatNic>,
    /// A reboot is needed before the feature works.
    #[serde(default)]
    pub reboot_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SeatNic {
    pub seat: u8,
    pub addresses: Vec<String>,
}


/// Hyper-V VM seat actions (`vm_seats` request, hyperv-seats 2026-11-17; spec/remote 10 §8).
/// Starting and stopping a VM seat is `start_seats` / `stop_seat` as for every seat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum VmAction {
    /// Host facts, base disk, every VM seat's VM state.
    Status,
    /// Create the VM network (internal switch, NAT, host address) and register the guest
    /// link service. `confirm` = the operator agreed.
    PrepareHost { confirm: bool },
    /// Build the base disk from `settings.remote.vm` (image); runs in the background,
    /// `building` in the status until done.
    BuildBase { confirm: bool },
    /// Delete the base disk and every VM seat's VM and disk (all VM seats stopped).
    ResetBase { confirm: bool },
    /// Delete and recreate one VM seat's disk (seat stopped): its guest starts fresh.
    ResetSandbox { seat: u8, confirm: bool },
}

/// One VM seat as the host sees it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct VmSeatInfo {
    pub seat: u8,
    /// Hyper-V state (`off`, `running`, `saved`, ...) or `missing` (no VM yet).
    pub state: String,
    pub guest_address: String,
    /// Last phase the guest reported over the guest link (`waiting`, `configuring`,
    /// `signed_in`, `streaming`, `failed`), `None` = no link.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guest_phase: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guest_detail: Option<String>,
}

/// Answer of every `vm_seats` action.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct VmHostStatus {
    /// Hyper-V feature: `enabled`, `disabled`, `enable_pending`, `unavailable`.
    pub feature: String,
    /// Hyper-V management service running.
    pub vmms: bool,
    pub network_ready: bool,
    pub base_present: bool,
    /// A base disk build runs.
    pub building: bool,
    /// Last base build / host error (English technical text).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// Folder of the disks actually used.
    pub storage: String,
    pub gpus: Vec<String>,
    pub host_memory_mb: u64,
    pub logical_processors: u32,
    pub seats: Vec<VmSeatInfo>,
}
