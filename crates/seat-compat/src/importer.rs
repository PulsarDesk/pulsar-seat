// SPDX-License-Identifier: GPL-3.0-only
//! Optional importer of the reference product's assignment snapshot
//! (`default_configuration.json`, spec 10 §3.8 field names — interop identifiers).
//!
//! The snapshot keys devices by physical position, like we do, but its exact path
//! syntax is not ours. So records are **matched against the live devices**:
//!
//! 1. kind from `DeviceTypeGUID` (spec 10 §3.1 category GUIDs);
//! 2. position: the snapshot's `CanonicalName2` (prefix letter `A` / `B` dropped) is
//!    compared with the `loc=` / `inst=` anchor of our key, case-insensitively, and the
//!    8-hex interface class and `USBMI(n)` interface number when both sides have them;
//! 3. otherwise a unique friendly-name match (`InitialName` == device name), flagged as
//!    a weaker match.
//!
//! Unmatched records are reported, never guessed. Nothing is saved: the service returns
//! the proposed config for the manager to review (`import_reference`).

use seat_core::config::{Config, DeviceAssignment};
use seat_core::device::{Device, DeviceKind};
use seat_core::{DeviceKey, SeatMask, SeatNumber};
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("no `DeviceAsignment` array in the file")]
    NoAssignments,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct RefMask {
    #[serde(rename = "Initialized")]
    initialized: Option<serde_json::Value>,
    #[serde(rename = "Mask")]
    mask: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct RefRecord {
    #[serde(rename = "CanonicalName")]
    canonical_name: Option<String>,
    #[serde(rename = "CanonicalName2")]
    canonical_name2: Option<String>,
    #[serde(rename = "DeviceTypeGUID")]
    device_type_guid: Option<String>,
    #[serde(rename = "InitialName")]
    initial_name: Option<String>,
    #[serde(rename = "Type")]
    kind: Option<String>,
    #[serde(rename = "StationMask")]
    station_mask: Option<RefMask>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchQuality {
    /// Position (and interface parts) matched.
    Position,
    /// Only a unique friendly name matched; review advised.
    NameOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Matched {
    pub record: usize,
    pub key: DeviceKey,
    pub mask: SeatMask,
    pub quality: MatchQuality,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnmatchedReason {
    UnknownCategory,
    BadMask,
    NotInitialized,
    NoLiveDevice,
    Ambiguous,
    /// Mask uses seats above our limit.
    SeatOutOfRange,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unmatched {
    pub record: usize,
    pub name: Option<String>,
    pub position: Option<String>,
    pub reason: UnmatchedReason,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportReport {
    pub config: Config,
    pub matched: Vec<Matched>,
    pub unmatched: Vec<Unmatched>,
}

/// Category GUID -> kind (spec 10 §3.1). Display-adapter surfaces have no kind of
/// their own in our model (monitors follow them) and are reported unmatched.
pub fn kind_of_category(guid: &str) -> Option<DeviceKind> {
    let g = guid.trim().trim_matches(['{', '}']).to_ascii_lowercase();
    let known = [
        DeviceKind::Monitor,
        DeviceKind::Keyboard,
        DeviceKind::Mouse,
        DeviceKind::Hid,
        DeviceKind::SmartCardReader,
        DeviceKind::UsbHub,
    ];
    if let Some(k) = known
        .into_iter()
        .find(|k| k.interface_class() == Some(g.as_str()))
    {
        return Some(k);
    }
    match g.as_str() {
        "b5b6dcec-79e1-483e-a46d-1e1f85eedf2f" => Some(DeviceKind::AudioRender),
        "f3f459b9-597a-4f0a-9e46-71642eff8048" => Some(DeviceKind::AudioCapture),
        // Camera category (HYPOTHESIS in the spec) and our own camera interface class.
        "eaca086a-320a-4b0e-850f-6ed61be1b612" | "e5323777-f976-4f5b-9b55-b94699c46e44" => {
            Some(DeviceKind::Webcam)
        }
        _ => None,
    }
}

fn mask_of(m: &RefMask) -> Option<SeatMask> {
    match m.mask.as_ref()? {
        serde_json::Value::String(s) => SeatMask::from_binary_string(s),
        // Some files may carry a number; accept it as the integer mask.
        serde_json::Value::Number(n) => {
            n.as_u64().and_then(|v| u32::try_from(v).ok()).map(SeatMask)
        }
        _ => None,
    }
}

fn initialized(m: &RefMask) -> bool {
    match &m.initialized {
        None => true,
        Some(serde_json::Value::Bool(b)) => *b,
        Some(serde_json::Value::Number(n)) => n.as_u64() != Some(0),
        Some(serde_json::Value::String(s)) => !matches!(s.trim(), "0" | "false" | ""),
        _ => true,
    }
}

/// Parts of a reference position: (path without prefix, 8-hex class, interface number).
fn ref_parts(cn2: &str) -> (String, Option<String>, Option<u8>) {
    let s = cn2.trim();
    // Char-aware: the second character may be non-ASCII (user-supplied file).
    let mut it = s.char_indices();
    let first = it.next().map(|(_, c)| c);
    let s = match (first, it.next()) {
        (Some('A' | 'B'), Some((i, c))) if !c.is_ascii_alphanumeric() => &s[i + c.len_utf8()..],
        (Some('A' | 'B'), Some(_)) if s[1..].starts_with("PCIROOT") => &s[1..],
        _ => s,
    };
    let lower = s.to_ascii_lowercase();
    let iface = lower.find("usbmi(").and_then(|i| {
        let rest = &lower[i + 6..];
        rest.split(')').next().and_then(|n| n.trim().parse().ok())
    });
    // Last run of exactly 8 hex digits delimited by non-alphanumerics = class prefix.
    let cls = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .rfind(|t| t.len() == 8 && t.chars().all(|c| c.is_ascii_hexdigit()))
        .map(str::to_string);
    (lower, cls, iface)
}

fn key_part<'a>(key: &'a str, name: &str) -> Option<&'a str> {
    key.split(';')
        .find_map(|p| p.strip_prefix(name).and_then(|r| r.strip_prefix('=')))
}

fn position_matches(dev: &Device, path: &str, cls: &Option<String>, iface: Option<u8>) -> bool {
    let k = dev.key.as_str();
    let anchor = key_part(k, "loc")
        .or_else(|| key_part(k, "inst"))
        .map(|a| a.to_ascii_lowercase());
    let Some(anchor) = anchor else { return false };
    if anchor.is_empty() || !(path.contains(&anchor) || (path.len() >= 8 && anchor.contains(path)))
    {
        return false;
    }
    if let (Some(c), Some(ours)) = (cls, key_part(k, "cls"))
        && !ours.eq_ignore_ascii_case(c)
    {
        return false;
    }
    if let (Some(n), Some(ours)) = (iface, key_part(k, "if"))
        && ours.parse::<u8>().ok() != Some(n)
    {
        return false;
    }
    true
}

/// Map the snapshot onto `base` using the live `devices`.
pub fn import_reference(
    json: &str,
    base: &Config,
    devices: &[Device],
) -> Result<ImportReport, ImportError> {
    let v: serde_json::Value = serde_json::from_str(json)?;
    let arr = v
        .get("DeviceAsignment")
        .and_then(|a| a.as_array())
        .ok_or(ImportError::NoAssignments)?;
    let mut cfg = base.clone();
    let mut matched = Vec::new();
    let mut unmatched = Vec::new();
    let mut max_seat = 1u8;
    for (i, raw) in arr.iter().enumerate() {
        let r: RefRecord = serde_json::from_value(raw.clone()).unwrap_or_default();
        let position = r
            .canonical_name2
            .clone()
            .or_else(|| r.canonical_name.clone());
        let um = |reason| Unmatched {
            record: i,
            name: r.initial_name.clone(),
            position: position.clone(),
            reason,
        };
        let Some(kind) = r.device_type_guid.as_deref().and_then(kind_of_category) else {
            unmatched.push(um(UnmatchedReason::UnknownCategory));
            continue;
        };
        let sm = r.station_mask.clone().unwrap_or_default();
        if !initialized(&sm) {
            unmatched.push(um(UnmatchedReason::NotInitialized));
            continue;
        }
        let Some(mask) = mask_of(&sm) else {
            unmatched.push(um(UnmatchedReason::BadMask));
            continue;
        };
        if !mask.is_shared() && mask.0 & !SeatMask::all_up_to(seat_core::MAX_SEATS).0 != 0 {
            unmatched.push(um(UnmatchedReason::SeatOutOfRange));
            continue;
        }
        let same_kind: Vec<&Device> = devices.iter().filter(|d| d.kind == kind).collect();
        let mut cands: Vec<&Device> = match r.canonical_name2.as_deref() {
            Some(cn2) => {
                let (path, cls, iface) = ref_parts(cn2);
                same_kind
                    .iter()
                    .copied()
                    .filter(|d| position_matches(d, &path, &cls, iface))
                    .collect()
            }
            None => Vec::new(),
        };
        let mut quality = MatchQuality::Position;
        if cands.is_empty()
            && let Some(n) = r.initial_name.as_deref()
        {
            cands = same_kind
                .iter()
                .copied()
                .filter(|d| d.name.trim().eq_ignore_ascii_case(n.trim()))
                .collect();
            quality = MatchQuality::NameOnly;
        }
        match cands.len() {
            0 => unmatched.push(um(UnmatchedReason::NoLiveDevice)),
            1 => {
                let d = cands[0];
                if !mask.is_shared()
                    && let Some(top) = mask.seats().map(|s| s.get()).max()
                {
                    max_seat = max_seat.max(top);
                }
                cfg.devices.retain(|a| a.key != d.key);
                cfg.devices.push(DeviceAssignment {
                    key: d.key.clone(),
                    kind,
                    mask,
                    name: d.name.clone(),
                    label: d.label.clone(),
                    hardware_id: d.hardware_id.clone(),
                    serial: d.serial.clone(),
                    container_id: d.container_id.clone(),
                    options: Default::default(),
                });
                matched.push(Matched {
                    record: i,
                    key: d.key.clone(),
                    mask,
                    quality,
                });
            }
            _ => unmatched.push(um(UnmatchedReason::Ambiguous)),
        }
    }
    // Seats referenced by masks exist in the proposal.
    for n in 1..=max_seat {
        let s = SeatNumber::new(n).expect("1..=MAX_SEATS");
        if cfg.seat(s).is_none() {
            cfg.seats.push(seat_core::config::SeatConfig::new(s));
        }
    }
    cfg.normalize();
    Ok(ImportReport {
        config: cfg,
        matched,
        unmatched,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use seat_core::device::DeviceDetails;

    fn dev(key: &str, kind: DeviceKind, name: &str) -> Device {
        Device {
            key: DeviceKey::new(key.to_string()).unwrap(),
            kind,
            name: name.into(),
            label: None,
            instance_id: None,
            container_id: None,
            parent_hub: None,
            present: true,
            hardware_id: None,
            serial: None,
            warnings: Vec::new(),
            details: DeviceDetails::None,
        }
    }

    #[test]
    fn imports_by_position_and_name() {
        let devices = vec![
            dev(
                "kbd;loc=PCIROOT(0)#PCI(1400)#USBROOT(0)#USB(3);cls=884b96c3;if=0",
                DeviceKind::Keyboard,
                "USB Klavye",
            ),
            dev(
                "kbd;loc=PCIROOT(0)#PCI(1400)#USBROOT(0)#USB(4);cls=884b96c3;if=0",
                DeviceKind::Keyboard,
                "USB Klavye",
            ),
            dev(
                "mou;loc=PCIROOT(0)#PCI(1400)#USBROOT(0)#USB(5);cls=378de44c;if=1",
                DeviceKind::Mouse,
                "Fare X",
            ),
        ];
        let json = r#"{"DeviceAsignment":[
          {"CanonicalName":"0-4","CanonicalName2":"A-PCIROOT(0)#PCI(1400)#USBROOT(0)#USB(4)-884B96C3-USBMI(0)",
           "DeviceTypeGUID":"{884b96c3-56ef-11d1-bc8c-00a0c91405dd}","InitialName":"USB Klavye","Type":"USB_ConnectedDevice",
           "StationMask":{"Initialized":true,"Mask":"10"}},
          {"CanonicalName2":"A-somewhere-else","DeviceTypeGUID":"{378de44c-56ef-11d1-bc8c-00a0c91405dd}",
           "InitialName":"Fare X","StationMask":{"Initialized":1,"Mask":"100"}},
          {"CanonicalName2":"A-x","DeviceTypeGUID":"{884b96c3-56ef-11d1-bc8c-00a0c91405dd}","InitialName":"USB Klavye",
           "StationMask":{"Mask":"1"}},
          {"DeviceTypeGUID":"{5b45201d-f2f2-4f3b-85bb-30ff1f953599}","StationMask":{"Mask":"1"}},
          {"DeviceTypeGUID":"{378de44c-56ef-11d1-bc8c-00a0c91405dd}","StationMask":{"Mask":"x1"}}
        ]}"#;
        let r = import_reference(json, &Config::default(), &devices).unwrap();
        assert_eq!(r.matched.len(), 2, "{r:?}");
        assert_eq!(r.matched[0].key, devices[1].key);
        assert_eq!(r.matched[0].mask, SeatMask(0b10));
        assert_eq!(r.matched[0].quality, MatchQuality::Position);
        assert_eq!(r.matched[1].quality, MatchQuality::NameOnly);
        let reasons: Vec<_> = r.unmatched.iter().map(|u| u.reason).collect();
        assert_eq!(
            reasons,
            vec![
                UnmatchedReason::Ambiguous,
                UnmatchedReason::UnknownCategory,
                UnmatchedReason::BadMask
            ]
        );
        // Seat 3 referenced -> seats 1..=3 proposed.
        assert_eq!(r.config.seats.len(), 3);
        assert_eq!(r.config.devices.len(), 2);
    }

    #[test]
    fn rejects_bad_files() {
        assert!(matches!(
            import_reference("{}", &Config::default(), &[]),
            Err(ImportError::NoAssignments)
        ));
        assert!(matches!(
            import_reference("nope", &Config::default(), &[]),
            Err(ImportError::Json(_))
        ));
    }

    #[test]
    fn category_map() {
        assert_eq!(
            kind_of_category("{E6F07B5F-EE97-4A90-B076-33F57BF4EAA7}"),
            Some(DeviceKind::Monitor)
        );
        assert_eq!(
            kind_of_category("b5b6dcec-79e1-483e-a46d-1e1f85eedf2f"),
            Some(DeviceKind::AudioRender)
        );
        assert_eq!(
            kind_of_category("5b45201d-f2f2-4f3b-85bb-30ff1f953599"),
            None
        );
    }

    #[test]
    fn ref_parts_non_ascii_does_not_panic() {
        assert_eq!(ref_parts("Aé#USB").0, "#usb");
        assert_eq!(ref_parts("A#X").0, "x");
        assert_eq!(ref_parts("APCIROOT(0)").0, "pciroot(0)");
        assert_eq!(ref_parts("é").0, "é");
    }
}
