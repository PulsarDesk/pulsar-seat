// SPDX-License-Identifier: GPL-3.0-only
//! Reference product state for the coexistence guard (spec 90 §11 item 4; maintainer
//! decision 2026-11-24, architecture.md "Coexistence").
//!
//! The guard knows the reference product only by its service keys
//! ([`crate::consts::REFERENCE_SERVICE_KEYS`], interop identifiers). Three states:
//!
//! - [`ReferenceState::Absent`]: no key exists.
//! - [`ReferenceState::Disabled`]: at least one key exists, **every** existing key has
//!   `Start` = 4 (disabled) **and** none is running / loaded (service control manager).
//!   The operator keeps the product installed (for example for its licence) but turned
//!   off. Install and the session backends proceed with a warning
//!   ([`REFERENCE_PRODUCT_DISABLED`]).
//! - [`ReferenceState::Active`]: anything else, including every fact that could not be
//!   read (fail closed). Install and the RDP session backends are refused.
//!
//! Pure: the callers read the facts (registry `Start`, SCM state). Nothing here or in the
//! callers ever writes to those keys.

use serde::{Deserialize, Serialize};

/// `Start` value of a disabled service or driver (`SERVICE_DISABLED`).
pub const SERVICE_START_DISABLED: u32 = 4;

/// Note / issue code of the disabled state (backends' notes, manager copy).
pub const REFERENCE_PRODUCT_DISABLED: &str = "reference_product_disabled";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceState {
    #[default]
    Absent,
    Disabled,
    Active,
}

impl ReferenceState {
    /// Install and the RDP session backends must refuse.
    pub fn blocks(self) -> bool {
        self == Self::Active
    }

    pub fn is_present(self) -> bool {
        self != Self::Absent
    }
}

/// Facts about one existing key. `None` = could not be read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReferenceKey {
    pub name: String,
    /// `HKLM\SYSTEM\CurrentControlSet\Services\<name>\Start`.
    pub start: Option<u32>,
    /// The service control manager reports it in any state but stopped (a driver:
    /// loaded).
    pub active: Option<bool>,
}

impl ReferenceKey {
    pub fn is_disabled(&self) -> bool {
        self.start == Some(SERVICE_START_DISABLED) && self.active == Some(false)
    }
}

/// State of the existing keys (`keys` lists only keys that exist).
pub fn classify(keys: &[ReferenceKey]) -> ReferenceState {
    if keys.is_empty() {
        ReferenceState::Absent
    } else if keys.iter().all(ReferenceKey::is_disabled) {
        ReferenceState::Disabled
    } else {
        ReferenceState::Active
    }
}

/// Read the facts of every key in [`crate::consts::REFERENCE_SERVICE_KEYS`].
/// `exists(name)`: `None` = unknown (counted as existing, fail closed);
/// `start(name)` / `active(name)`: `None` = unknown. `start` and `active` are only asked
/// for keys that exist.
pub fn probe(
    exists: impl Fn(&str) -> Option<bool>,
    start: impl Fn(&str) -> Option<u32>,
    active: impl Fn(&str) -> Option<bool>,
) -> Vec<ReferenceKey> {
    crate::consts::REFERENCE_SERVICE_KEYS
        .iter()
        .filter(|n| exists(n).unwrap_or(true))
        .map(|n| ReferenceKey {
            name: (*n).to_string(),
            start: start(n),
            active: active(n),
        })
        .collect()
}

/// One English line for logs: `NAME (start 4, stopped), ...`.
pub fn describe(keys: &[ReferenceKey]) -> String {
    keys.iter()
        .map(|k| {
            let start = k.start.map_or_else(|| "start unknown".to_string(), |s| format!("start {s}"));
            let state = match k.active {
                Some(true) => "running",
                Some(false) => "stopped",
                None => "state unknown",
            };
            format!("{} ({start}, {state})", k.name)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consts::REFERENCE_SERVICE_KEYS;

    fn key(name: &str, start: Option<u32>, active: Option<bool>) -> ReferenceKey {
        ReferenceKey { name: name.into(), start, active }
    }

    #[test]
    fn absent_without_keys() {
        assert_eq!(classify(&[]), ReferenceState::Absent);
        assert!(!ReferenceState::Absent.blocks());
        assert!(!ReferenceState::Absent.is_present());
    }

    #[test]
    fn disabled_only_when_every_key_is_disabled_and_stopped() {
        let a = REFERENCE_SERVICE_KEYS[0];
        let b = REFERENCE_SERVICE_KEYS[1];
        let both = [key(a, Some(4), Some(false)), key(b, Some(4), Some(false))];
        assert_eq!(classify(&both), ReferenceState::Disabled);
        assert!(!ReferenceState::Disabled.blocks());
        assert!(ReferenceState::Disabled.is_present());
        // One key only (the other is gone), disabled and stopped: still the disabled state.
        assert_eq!(classify(&both[..1]), ReferenceState::Disabled);
        for bad in [
            key(b, Some(3), Some(false)),  // demand start
            key(b, Some(2), Some(false)),  // automatic
            key(b, Some(1), Some(false)),  // system (driver)
            key(b, Some(4), Some(true)),   // disabled but still running / loaded
            key(b, None, Some(false)),     // Start unreadable
            key(b, Some(4), None),         // SCM state unknown
        ] {
            assert_eq!(classify(&[both[0].clone(), bad.clone()]), ReferenceState::Active, "{bad:?}");
        }
        assert!(ReferenceState::Active.blocks());
    }

    #[test]
    fn probe_asks_only_existing_keys_and_fails_closed() {
        let a = REFERENCE_SERVICE_KEYS[0];
        let keys = probe(|n| Some(n == a), |_| Some(4), |_| Some(false));
        assert_eq!(keys, vec![key(a, Some(4), Some(false))]);
        assert_eq!(classify(&keys), ReferenceState::Disabled);
        // Existence unknown: counted as present with what could be read.
        let keys = probe(|_| None, |_| None, |_| None);
        assert_eq!(keys.len(), REFERENCE_SERVICE_KEYS.len());
        assert_eq!(classify(&keys), ReferenceState::Active);
        assert_eq!(classify(&probe(|_| Some(false), |_| None, |_| None)), ReferenceState::Absent);
    }

    #[test]
    fn describe_lists_facts() {
        let s = describe(&[key("A", Some(4), Some(false)), key("B", None, Some(true)), key("C", Some(3), None)]);
        assert_eq!(s, "A (start 4, stopped), B (start unknown, running), C (start 3, state unknown)");
    }

    #[test]
    fn state_serializes_snake_case() {
        assert_eq!(serde_json::to_string(&ReferenceState::Disabled).unwrap(), "\"disabled\"");
    }
}
