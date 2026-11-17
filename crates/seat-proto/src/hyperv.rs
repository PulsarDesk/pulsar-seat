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

