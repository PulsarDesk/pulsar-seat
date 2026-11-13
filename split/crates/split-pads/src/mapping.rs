// SPDX-License-Identifier: GPL-3.0-only
//! Moonlight client controller -> pad kind (spec 60 §5.3) and backend resolution.

use serde::{Deserialize, Serialize};

use crate::types::{BackendAvailability, BackendId, PadCaps, PadKind};

/// Moonlight controller type (spec 60 §3.5 `client_type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientType {
    Xbox,
    Ps,
    Nintendo,
    #[serde(other)]
    Unknown,
}

/// Capability names of `pad.wanted` (spec 60 §3.5 `caps`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cap {
    Rumble,
    TriggerRumble,
    Touchpad,
    Accel,
    Gyro,
    Battery,
    RgbLed,
}

pub fn caps_from(list: &[Cap]) -> PadCaps {
    PadCaps {
        rumble: list.contains(&Cap::Rumble),
        trigger_rumble: list.contains(&Cap::TriggerRumble),
        touchpad: list.contains(&Cap::Touchpad),
        motion: list.contains(&Cap::Accel) || list.contains(&Cap::Gyro),
        battery: list.contains(&Cap::Battery),
        rgb_led: list.contains(&Cap::RgbLed),
    }
}

/// Kind selection for `gamepad = auto`, or the forced kind (spec 60 §5.3 table).
pub fn choose_kind(client: ClientType, caps: &[Cap], forced: Option<PadKind>) -> PadKind {
    if let Some(k) = forced {
        return k;
    }
    let c = caps_from(caps);
    match client {
        ClientType::Ps => PadKind::Ds5,
        _ if c.touchpad || c.motion => PadKind::Ds5,
        _ => PadKind::XOne,
    }
}

/// Client capabilities the kind cannot carry (logged once by the caller).
pub fn unsupported_features(kind: PadKind, caps: &[Cap]) -> Vec<Cap> {
    caps.iter()
        .copied()
        .filter(|c| {
            matches!(
                (kind, c),
                (PadKind::X360 | PadKind::XOne, Cap::Touchpad | Cap::Accel | Cap::Gyro | Cap::Battery | Cap::RgbLed)
                    | (PadKind::X360 | PadKind::Ds4 | PadKind::Ds5 | PadKind::Ds5Edge, Cap::TriggerRumble)
            )
        })
        .collect()
}

/// Which backend makes a pad for a requested wire kind, and the kind the backend
/// creates (`backend_kind`) plus the wire kind the stream server must write (`wire`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolved {
    pub backend: BackendId,
    pub backend_kind: PadKind,
    pub wire: PadKind,
}

fn supports(a: &BackendAvailability, k: PadKind) -> bool {
    a.usable() && a.kinds.contains(&k)
}

fn find(av: &[BackendAvailability], id: BackendId) -> Option<&BackendAvailability> {
    av.iter().find(|a| a.id == id)
}

/// Candidates per requested kind, in order (backend, backend kind, wire kind).
fn candidates(kind: PadKind) -> &'static [(BackendId, PadKind, PadKind)] {
    use BackendId::*;
    use PadKind::*;
    match kind {
        Ds5 => &[(PulsarPad, Ds5, Ds5), (UsbipDs5, Ds5, Ds5), (Vigem, Ds4, Ds4)],
        Ds5Edge => &[(PulsarPad, Ds5Edge, Ds5Edge), (UsbipDs5, Ds5Edge, Ds5Edge), (UsbipDs5, Ds5, Ds5), (Vigem, Ds4, Ds4)],
        Ds4 => &[(PulsarPad, Ds4, Ds4), (Vigem, Ds4, Ds4), (UsbipDs5, Ds5, Ds5)],
        XOne => &[(PulsarPad, XOne, XOne), (Vigem, X360, XOne), (UsbipDs5, Ds5, Ds5)],
        X360 => &[(Vigem, X360, XOne), (PulsarPad, XOne, XOne), (UsbipDs5, Ds5, Ds5)],
    }
}

/// Picks a backend. `PulsarPad` is used only when it is the preferred backend (never by
/// default). A preferred backend wins when it supports the kind; otherwise the fixed
/// fallback order applies.
pub fn resolve(kind: PadKind, preferred: Option<BackendId>, av: &[BackendAvailability]) -> Option<Resolved> {
    let list = candidates(kind);
    if let Some(p) = preferred {
        for &(b, bk, w) in list {
            if b == p && find(av, b).is_some_and(|a| supports(a, bk)) {
                return Some(Resolved { backend: b, backend_kind: bk, wire: w });
            }
        }
    }
    for &(b, bk, w) in list {
        if b == BackendId::PulsarPad && preferred != Some(BackendId::PulsarPad) {
            continue;
        }
        if find(av, b).is_some_and(|a| supports(a, bk)) {
            return Some(Resolved { backend: b, backend_kind: bk, wire: w });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn av(id: BackendId, kinds: &[PadKind], installed: bool) -> BackendAvailability {
        BackendAvailability { id, installed, version: None, signed: true, kinds: kinds.to_vec(), reason: None }
    }

    #[test]
    fn auto_mapping_table() {
        assert_eq!(choose_kind(ClientType::Ps, &[], None), PadKind::Ds5);
        assert_eq!(choose_kind(ClientType::Xbox, &[Cap::Rumble], None), PadKind::XOne);
        assert_eq!(choose_kind(ClientType::Nintendo, &[], None), PadKind::XOne);
        assert_eq!(choose_kind(ClientType::Unknown, &[Cap::Gyro], None), PadKind::Ds5);
        assert_eq!(choose_kind(ClientType::Xbox, &[Cap::Touchpad], None), PadKind::Ds5);
        assert_eq!(choose_kind(ClientType::Ps, &[], Some(PadKind::Ds4)), PadKind::Ds4);
    }

    #[test]
    fn client_type_unknown_fallback() {
        assert_eq!(serde_json::from_str::<ClientType>("\"steam_deck\"").unwrap(), ClientType::Unknown);
        assert_eq!(serde_json::from_str::<Cap>("\"trigger_rumble\"").unwrap(), Cap::TriggerRumble);
    }

    #[test]
    fn unsupported() {
        assert_eq!(unsupported_features(PadKind::XOne, &[Cap::Rumble, Cap::Gyro]), vec![Cap::Gyro]);
        assert!(unsupported_features(PadKind::Ds5, &[Cap::Gyro, Cap::Touchpad, Cap::RgbLed]).is_empty());
    }

    #[test]
    fn resolve_rules() {
        let vigem = av(BackendId::Vigem, &[PadKind::X360, PadKind::Ds4], true);
        let usbip = av(BackendId::UsbipDs5, &[PadKind::Ds5, PadKind::Ds5Edge], true);
        let own = av(BackendId::PulsarPad, &[PadKind::XOne, PadKind::Ds4, PadKind::Ds5, PadKind::Ds5Edge], true);
        let all = [vigem.clone(), usbip.clone(), own.clone()];
        let r = resolve(PadKind::Ds5, None, &all).unwrap();
        assert_eq!((r.backend, r.backend_kind, r.wire), (BackendId::UsbipDs5, PadKind::Ds5, PadKind::Ds5));
        let r = resolve(PadKind::XOne, None, &all).unwrap();
        assert_eq!((r.backend, r.backend_kind, r.wire), (BackendId::Vigem, PadKind::X360, PadKind::XOne));
        // PulsarPad only when preferred
        let r = resolve(PadKind::XOne, Some(BackendId::PulsarPad), &all).unwrap();
        assert_eq!(r.backend, BackendId::PulsarPad);
        // only ViGEm: DualSense falls back to DS4
        let r = resolve(PadKind::Ds5, None, std::slice::from_ref(&vigem)).unwrap();
        assert_eq!((r.backend, r.wire), (BackendId::Vigem, PadKind::Ds4));
        // preferred usbip for Ds4 request -> usbip ds5
        let r = resolve(PadKind::Ds4, Some(BackendId::UsbipDs5), &all).unwrap();
        assert_eq!((r.backend, r.wire), (BackendId::UsbipDs5, PadKind::Ds5));
        // nothing installed
        let none = [av(BackendId::Vigem, &[PadKind::X360], false)];
        assert_eq!(resolve(PadKind::XOne, None, &none), None);
        // unusable reason
        let mut bad = usbip.clone();
        bad.reason = Some("unusable: usbip.exe missing".into());
        assert_eq!(resolve(PadKind::Ds5, None, &[bad]), None);
    }
}
