// SPDX-License-Identifier: GPL-3.0-only
//! Effective seat of each device: explicit assignment, else inherited from the nearest
//! assigned container, USB port or USB hub above it (nearest first), else the kind's
//! default (spec 50 §4.1.7, spec 70 §5.1).

use crate::config::Config;
use crate::device::{Device, DeviceKind, SharePolicy};
use crate::ids::{DeviceKey, SeatMask};
use crate::pnp::{container_key, key_location, port_of_location, port_key};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "hub", rename_all = "snake_case")]
pub enum MaskSource {
    Explicit,
    Inherited(DeviceKey),
    Default,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Effective {
    pub key: DeviceKey,
    pub kind: DeviceKind,
    pub mask: SeatMask,
    pub source: MaskSource,
}

/// Effective masks for every device of the live inventory.
///
/// Exclusive kinds inherit a hub's mask only when it names exactly one seat; a shared or
/// multi-seat hub leaves them at their default. Network adapters are always shared.
pub fn effective_masks(config: &Config, inventory: &[Device]) -> Vec<Effective> {
    let explicit: HashMap<&DeviceKey, SeatMask> = config.devices.iter().map(|d| (&d.key, d.mask)).collect();
    let by_key: HashMap<&DeviceKey, &Device> = inventory.iter().map(|d| (&d.key, d)).collect();
    inventory
        .iter()
        .map(|d| {
            let (mask, source) = resolve(d, &explicit, &by_key);
            Effective { key: d.key.clone(), kind: d.kind, mask, source }
        })
        .collect()
}

fn resolve(
    d: &Device,
    explicit: &HashMap<&DeviceKey, SeatMask>,
    by_key: &HashMap<&DeviceKey, &Device>,
) -> (SeatMask, MaskSource) {
    if d.kind.policy() == SharePolicy::NeverGated {
        return (SeatMask::SHARED, MaskSource::Default);
    }
    if let Some(m) = explicit.get(&d.key) {
        return (*m, MaskSource::Explicit);
    }
    let exclusive = d.kind.policy() == SharePolicy::Exclusive;
    // A candidate (container, port or hub assignment) decides when it is not spare. An
    // exclusive kind takes it only when it names one seat; otherwise the walk stops and the
    // kind default applies (the nearest assignment wins, a farther one never overrides it).
    let take = |k: &DeviceKey| -> Option<Option<(SeatMask, MaskSource)>> {
        let m = explicit.get(k).filter(|m| !m.is_spare())?;
        if !exclusive || m.single().is_some() {
            Some(Some((*m, MaskSource::Inherited(k.clone()))))
        } else {
            Some(None)
        }
    };
    let default = (d.kind.default_mask(), MaskSource::Default);
    // 1. Container grouping (spec 70 §5.1): one physical product moves as a whole.
    if let Some(ck) = d.container_id.as_deref().and_then(container_key) {
        if let Some(r) = take(&ck) {
            return r.unwrap_or(default);
        }
    }
    // 2. Nearest first: the device's own port, its hub, that hub's port, the next hub, ...
    let mut node_key = &d.key;
    let mut hub = d.parent_hub.as_ref();
    let mut guard = 0;
    loop {
        if let Some(loc) = key_location(node_key) {
            if let Some(p) = port_of_location(loc) {
                let pk = port_key(&p);
                if let Some(r) = take(&pk) {
                    return r.unwrap_or(default);
                }
            }
        }
        let Some(h) = hub else { break };
        if let Some(r) = take(h) {
            return r.unwrap_or(default);
        }
        guard += 1;
        if guard > 32 {
            break;
        }
        node_key = h;
        hub = by_key.get(h).and_then(|hd| hd.parent_hub.as_ref());
    }
    default
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DeviceAssignment;
    use crate::device::DeviceDetails;
    use crate::ids::SeatNumber;

    fn dev(key: &str, kind: DeviceKind, hub: Option<&str>) -> Device {
        Device {
            key: DeviceKey::new(key).unwrap(),
            kind,
            name: key.into(),
            label: None,
            instance_id: None,
            container_id: None,
            parent_hub: hub.map(|h| DeviceKey::new(h).unwrap()),
            present: true,
            hardware_id: None,
            serial: None,
            warnings: vec![],
            details: DeviceDetails::None,
        }
    }

    #[test]
    fn explicit_inherited_default() {
        let two = SeatNumber::new(2).unwrap();
        let mut c = Config::default();
        c.devices.push(DeviceAssignment {
            key: DeviceKey::new("hub;a").unwrap(),
            kind: DeviceKind::UsbHub,
            mask: two.mask(),
            name: String::new(),
            label: None,
            hardware_id: None,
            serial: None,
            container_id: None,
            options: Default::default(),
        });
        let inv = vec![
            dev("hub;a", DeviceKind::UsbHub, None),
            dev("hub;b", DeviceKind::UsbHub, Some("hub;a")),
            dev("kbd;x", DeviceKind::Keyboard, Some("hub;b")),
            dev("stor;y", DeviceKind::Storage, None),
            dev("net;z", DeviceKind::Network, Some("hub;a")),
        ];
        let e = effective_masks(&c, &inv);
        assert_eq!(e[0].source, MaskSource::Explicit);
        assert_eq!(e[2].mask, two.mask());
        assert_eq!(e[2].source, MaskSource::Inherited(DeviceKey::new("hub;a").unwrap()));
        assert_eq!(e[3].mask, SeatMask::SHARED);
        assert_eq!(e[4].mask, SeatMask::SHARED);
        // A shared hub does not pull an exclusive keyboard off seat 1.
        c.devices[0].mask = SeatMask::SHARED;
        assert_eq!(effective_masks(&c, &inv)[2].mask, SeatMask::CONSOLE);
    }

    fn assignment(key: &str, kind: DeviceKind, mask: SeatMask) -> DeviceAssignment {
        DeviceAssignment {
            key: DeviceKey::new(key).unwrap(),
            kind,
            mask,
            name: String::new(),
            label: None,
            hardware_id: None,
            serial: None,
            container_id: None,
            options: Default::default(),
        }
    }

    #[test]
    fn ports_and_containers() {
        let (two, three) = (SeatNumber::new(2).unwrap(), SeatNumber::new(3).unwrap());
        let root = "PCIROOT(0)#PCI(1400)#USBROOT(0)";
        let hub_loc = format!("{root}#USB(2)");
        let hub = format!("hub;loc={hub_loc}");
        let mut kbd = dev(&format!("kbd;loc={hub_loc}#USB(4)#USBMI(0)"), DeviceKind::Keyboard, Some(&hub));
        let stor = dev(&format!("stor;loc={hub_loc}#USB(1);sub=lun0"), DeviceKind::Storage, Some(&hub));
        let inv_hub = dev(&hub, DeviceKind::UsbHub, None);
        kbd.container_id = Some("{ABCDEF01-0000-0000-0000-000000000001}".into());
        let mut c = Config::default();
        // Port 4 of the shared hub belongs to seat 2; port 1 is untouched.
        c.devices.push(assignment(&format!("port;loc={hub_loc}#USB(4)"), DeviceKind::UsbHub, two.mask()));
        let inv = vec![inv_hub.clone(), kbd.clone(), stor.clone()];
        let e = effective_masks(&c, &inv);
        assert_eq!(e[1].mask, two.mask());
        assert!(matches!(&e[1].source, MaskSource::Inherited(k) if k.as_str().starts_with("port;")));
        assert_eq!(e[2].source, MaskSource::Default);
        // The root port the hub sits in reaches everything below, nearer port wins.
        c.devices.push(assignment(&format!("port;loc={hub_loc}"), DeviceKind::UsbHub, three.mask()));
        let e = effective_masks(&c, &inv);
        assert_eq!(e[1].mask, two.mask());
        assert_eq!(e[2].mask, three.mask());
        assert_eq!(e[0].mask, three.mask(), "the hub itself sits in that root port");
        // A container assignment beats the ports.
        let ck = crate::pnp::container_key("{ABCDEF01-0000-0000-0000-000000000001}").unwrap();
        c.devices.push(assignment(ck.as_str(), DeviceKind::Other, three.mask()));
        assert_eq!(effective_masks(&c, &inv)[1].mask, three.mask());
        // A shared container leaves an exclusive keyboard at its default.
        c.devices.last_mut().unwrap().mask = SeatMask::SHARED;
        assert_eq!(effective_masks(&c, &inv)[1].mask, SeatMask::CONSOLE);
        // The system container is never a group.
        assert!(crate::pnp::container_key("{00000000-0000-0000-FFFF-FFFFFFFFFFFF}").is_none());
    }
}
