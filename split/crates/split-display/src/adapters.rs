// SPDX-License-Identifier: GPL-3.0-only
//! Render-GPU enumeration and resolution (spec 40 §5, Pulsar approach).
//!
//! The seat stores the GPU as PnP instance id + description. LUIDs change between boots,
//! so the current LUID is resolved at seat start: PnP id first, then description, else
//! the system default plus a warning for the manager.

use serde::{Deserialize, Serialize};

use crate::error::DisplayError;

/// Locally unique adapter id (`LUID`), as DXGI / IddCx report it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Luid {
    pub low: u32,
    pub high: i32,
}

impl Luid {
    pub fn to_u64(self) -> u64 {
        ((self.high as u32 as u64) << 32) | self.low as u64
    }
    pub fn from_u64(v: u64) -> Self {
        Luid {
            low: v as u32,
            high: (v >> 32) as u32 as i32,
        }
    }
    pub fn is_zero(self) -> bool {
        self.low == 0 && self.high == 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderAdapter {
    /// PnP device instance id of the GPU (`PCI\VEN_…`), stable across boots.
    pub pnp_instance_id: String,
    /// DXGI description, shown to the user and kept as fallback key.
    pub description: String,
    pub luid: Luid,
    pub dedicated_video_memory: u64,
    pub hardware: bool,
    pub vendor_id: u32,
    pub device_id: u32,
}

/// Lists render adapters: DXGI 1.6, hardware only, duplicates removed.
pub trait AdapterEnumerator: Send + Sync {
    fn adapters(&self) -> Result<Vec<RenderAdapter>, DisplayError>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AdapterChoice {
    /// Use this adapter for rendering (passed to the driver's render-adapter call).
    Pinned(Luid),
    /// Let the OS pick. `warning` is set when a stored adapter is no longer present.
    Default { warning: Option<String> },
}

/// Pure: stored (PnP id, description) -> current LUID. PnP id first, then description;
/// nothing stored -> default without warning; stored but gone -> default + warning.
pub fn resolve_adapter(
    stored_pnp: Option<&str>,
    stored_desc: Option<&str>,
    now: &[RenderAdapter],
) -> AdapterChoice {
    let pnp = stored_pnp.map(str::trim).filter(|s| !s.is_empty());
    let desc = stored_desc.map(str::trim).filter(|s| !s.is_empty());
    if pnp.is_none() && desc.is_none() {
        return AdapterChoice::Default { warning: None };
    }
    if let Some(p) = pnp
        && let Some(a) = now
            .iter()
            .find(|a| a.pnp_instance_id.eq_ignore_ascii_case(p))
    {
        return AdapterChoice::Pinned(a.luid);
    }
    if let Some(d) = desc {
        let matches: Vec<&RenderAdapter> = now.iter().filter(|a| a.description == d).collect();
        if let Some(a) = matches.first() {
            if matches.len() > 1 {
                log::warn!("several render adapters share the description; using the first");
            }
            return AdapterChoice::Pinned(a.luid);
        }
    }
    let shown = desc.or(pnp).unwrap_or_default();
    AdapterChoice::Default {
        warning: Some(format!(
            "adapter_not_found: stored render adapter '{shown}' is not present; using the system default"
        )),
    }
}

/// Default adapter = most dedicated video memory (hardware only); ties keep the first.
pub fn default_adapter(now: &[RenderAdapter]) -> Option<&RenderAdapter> {
    now.iter()
        .filter(|a| a.hardware)
        .fold(None, |best: Option<&RenderAdapter>, a| match best {
            Some(b) if b.dedicated_video_memory >= a.dedicated_video_memory => Some(b),
            _ => Some(a),
        })
}

/// Pure filter used by the DXGI enumerator: drop software adapters (Basic Render Driver,
/// WARP) and duplicates (same LUID, or same PnP id reported twice).
pub fn filter_adapters(raw: Vec<RenderAdapter>) -> Vec<RenderAdapter> {
    let mut out: Vec<RenderAdapter> = Vec::new();
    for a in raw.into_iter().filter(|a| a.hardware) {
        let dup = out.iter().any(|b| {
            b.luid == a.luid
                || (!a.pnp_instance_id.is_empty()
                    && b.pnp_instance_id.eq_ignore_ascii_case(&a.pnp_instance_id))
        });
        if !dup {
            out.push(a);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gpu(pnp: &str, desc: &str, low: u32, mem: u64) -> RenderAdapter {
        RenderAdapter {
            pnp_instance_id: pnp.into(),
            description: desc.into(),
            luid: Luid { low, high: 0 },
            dedicated_video_memory: mem,
            hardware: true,
            vendor_id: 0x10de,
            device_id: 1,
        }
    }

    #[test]
    fn pnp_id_wins_over_description() {
        let now = [gpu("PCI\\A", "GPU X", 1, 8), gpu("PCI\\B", "GPU X", 2, 8)];
        assert_eq!(
            resolve_adapter(Some("pci\\b"), Some("GPU X"), &now),
            AdapterChoice::Pinned(Luid { low: 2, high: 0 })
        );
        // pnp gone -> description match
        assert_eq!(
            resolve_adapter(Some("PCI\\C"), Some("GPU X"), &now),
            AdapterChoice::Pinned(Luid { low: 1, high: 0 })
        );
    }

    #[test]
    fn missing_adapter_falls_back_with_warning() {
        let now = [gpu("PCI\\A", "GPU X", 1, 8)];
        match resolve_adapter(Some("PCI\\Z"), Some("Old GPU"), &now) {
            AdapterChoice::Default { warning: Some(w) } => assert!(w.contains("Old GPU")),
            other => panic!("{other:?}"),
        }
        assert_eq!(
            resolve_adapter(None, Some("  "), &now),
            AdapterChoice::Default { warning: None }
        );
    }

    #[test]
    fn default_is_most_memory_hardware_only() {
        let mut sw = gpu("", "Basic", 9, 1 << 40);
        sw.hardware = false;
        let now = [
            gpu("PCI\\A", "small", 1, 2),
            sw,
            gpu("PCI\\B", "big", 2, 16),
            gpu("PCI\\C", "big2", 3, 16),
        ];
        assert_eq!(default_adapter(&now).unwrap().description, "big");
        assert!(default_adapter(&[]).is_none());
    }

    #[test]
    fn filter_drops_software_and_duplicates() {
        let mut sw = gpu("", "Basic", 9, 0);
        sw.hardware = false;
        let out = filter_adapters(vec![
            gpu("PCI\\A", "a", 1, 1),
            gpu("PCI\\A", "a", 1, 1),
            sw,
            gpu("PCI\\B", "b", 2, 1),
        ]);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn luid_round_trip() {
        let l = Luid {
            low: 0xdead_beef,
            high: -2,
        };
        assert_eq!(Luid::from_u64(l.to_u64()), l);
    }
}
