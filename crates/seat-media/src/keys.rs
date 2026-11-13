// SPDX-License-Identifier: GPL-3.0-only
//! Pure identity rules for the devices this crate owns: audio endpoints (spec 60 §3.2 / §8.1),
//! storage, cameras, serial ports, hubs and other USB devices (spec 70 §5.1).
//!
//! Every key is built with [`seat_core::pnp::KeyParts`], so the format is the shared one:
//! `<prefix>;<anchor>[;if=<n>][;sub=<x>]`.

use seat_core::DeviceKey;
use seat_core::device::{AudioFlow, DeviceKind};
use seat_core::pnp::{Anchor, KeyParts, PnpTree, anchor_for, usb_interface_number};

/// The parent-device property of an endpoint (`{b3f8fa53-0004-438e-9003-51a46e139bfc},2`,
/// spec 60 §3.2), parsed.
///
/// Two raw shapes exist:
/// - `{n}.\\?\<KS filter interface path>\<refstring>/<pin 8 hex>` (spec 60 §3.2);
/// - `{n}.<parent devnode instance id>`: seen on the ARM64 dev box (Qualcomm ACX,
///   SoundWire, USB audio, Bluetooth), 2026-11-12; no filter / pin part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KsFilterRef {
    Interface {
        /// Device-interface path of the KS filter (`\\?\HDAUDIO#...#{6994ad04-...}\<ref>`),
        /// as used by [`PnpTree::interface_owner`].
        interface_path: String,
        /// `WAVE/00010001`, `TOPOLOGY/00010002`, … (upper-cased reference string + pin).
        ref_pin: Option<String>,
    },
    Devnode {
        instance_id: String,
    },
}

impl KsFilterRef {
    pub fn ref_pin(&self) -> Option<&str> {
        match self {
            Self::Interface { ref_pin, .. } => ref_pin.as_deref(),
            Self::Devnode { .. } => None,
        }
    }
    /// Parent audio devnode.
    pub fn parent(&self, tree: &dyn PnpTree) -> Option<String> {
        match self {
            Self::Interface { interface_path, .. } => tree.interface_owner(interface_path),
            Self::Devnode { instance_id } => Some(instance_id.clone()),
        }
    }
}

/// Parse the parent-device property value.
pub fn parse_ks_filter_ref(raw: &str) -> Option<KsFilterRef> {
    let s = raw.trim();
    // Strip the `{n}.` provider prefix.
    let s = match (s.starts_with('{'), s.find("}.")) {
        (true, Some(i)) => &s[i + 2..],
        _ => s,
    };
    if !s.starts_with(r"\\?\") {
        // A devnode instance id is `<enumerator>\<device>\<instance>`.
        let ok = s.split('\\').count() >= 3 && !s.contains('/') && !s.starts_with('\\');
        return ok.then(|| KsFilterRef::Devnode { instance_id: s.to_string() });
    }
    // Interface path = up to and including the `{class guid}` segment; the rest is
    // `\<refstring>/<pin>`.
    let (path, tail) = match s.rfind('}') {
        Some(i) if i + 1 < s.len() => (&s[..=i], &s[i + 1..]),
        Some(i) => (&s[..=i], ""),
        None => (s, ""),
    };
    let tail = tail.trim_start_matches('\\');
    let ref_pin = (!tail.is_empty()).then(|| tail.to_ascii_uppercase());
    // The interface manager names the interface including its reference string.
    let ref_only = tail.split('/').next().unwrap_or("");
    let interface_path = if ref_only.is_empty() { path.to_string() } else { format!(r"{path}\{ref_only}") };
    Some(KsFilterRef::Interface { interface_path, ref_pin })
}

/// `EndpointFormFactor` (Windows enum, documented values) → our snake_case name.
pub fn form_factor_name(ff: u32) -> &'static str {
    match ff {
        0 => "remote",
        1 => "speakers",
        2 => "line",
        3 => "headphones",
        4 => "microphone",
        5 => "headset",
        6 => "handset",
        7 => "digital",
        8 => "spdif",
        9 => "hdmi",
        _ => "other",
    }
}

/// `DEVICE_STATE_*` low bits → our state name.
pub fn endpoint_state_name(state: u32) -> &'static str {
    match state & 0xF {
        1 => "active",
        2 => "disabled",
        4 => "not_present",
        8 => "unplugged",
        _ => "unknown",
    }
}

/// Inputs of an endpoint key (spec 60 §3.2 composite, our own format).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointIdentity {
    pub flow: AudioFlow,
    /// Anchor of the parent audio devnode (location path, or root instance path).
    pub anchor: Anchor,
    /// USB composite interface number of the parent devnode, if any.
    pub usb_interface: Option<u8>,
    pub form_factor: u32,
    /// KS node type / jack sub-type GUID of the endpoint (any case, braces optional).
    pub node_type: Option<String>,
    /// KS filter reference string + pin (`WAVE/00010001`).
    pub ref_pin: Option<String>,
    /// Tie-breaker the reference lacks: Bluetooth address or USB serial (spec 60 §8.1).
    pub tie_breaker: Option<String>,
}

impl EndpointIdentity {
    pub fn key(&self) -> DeviceKey {
        let kind = match self.flow {
            AudioFlow::Render => DeviceKind::AudioRender,
            AudioFlow::Capture => DeviceKind::AudioCapture,
        };
        let mut p = KeyParts::new(kind, self.anchor.clone());
        p.usb_interface = self.usb_interface;
        let node: String = self
            .node_type
            .as_deref()
            .unwrap_or("")
            .chars()
            .filter(|c| c.is_ascii_hexdigit())
            .take(8)
            .collect::<String>()
            .to_ascii_lowercase();
        let mut sub = format!("fr{}", self.form_factor);
        if !node.is_empty() {
            sub.push('.');
            sub.push_str(&node);
        }
        if let Some(rp) = &self.ref_pin {
            sub.push('.');
            sub.push_str(&rp.to_ascii_lowercase());
        }
        if let Some(t) = &self.tie_breaker {
            sub.push_str(".t");
            sub.push_str(&t.to_ascii_lowercase());
        }
        p.sub = Some(sub);
        p.compose()
    }
}

/// Tie-breaker for a devnode: the Bluetooth address of the nearest Bluetooth node, else the
/// USB serial of the nearest USB node that has one (walks up at most 16 levels).
pub fn tie_breaker(tree: &dyn PnpTree, instance_id: &str) -> Option<String> {
    let mut cur = tree.node(instance_id);
    let mut depth = 0;
    while let Some(n) = cur {
        let e = n.enumerator.to_ascii_uppercase();
        if e.starts_with("BTH") {
            if let Some(s) = n.serial.clone().or_else(|| bt_address(&n.instance_id)) {
                return Some(s);
            }
        }
        if e == "USB" {
            if let Some(s) = n.serial.clone() {
                return Some(s);
            }
        }
        depth += 1;
        if depth > 16 {
            break;
        }
        cur = n.parent.as_deref().and_then(|p| tree.node(p));
    }
    None
}

/// Last 12-hex-digit token of a Bluetooth instance id (`..._8099E76D0A29_C00000000` →
/// `8099e76d0a29`).
fn bt_address(instance_id: &str) -> Option<String> {
    instance_id
        .split(['_', '&', '\\'])
        .rfind(|t| t.len() == 12 && t.chars().all(|c| c.is_ascii_hexdigit()))
        .map(str::to_ascii_lowercase)
}

/// Function part of a software-enumerated audio devnode without its own location path
/// (`QCASD\DYNAMICENUMEXTERNALDISPLAYXXXX0000000000\6&...` → the middle segment). Audio
/// class extensions (ACX, SoundWire) put every monitor / jack function of one codec under
/// one parent; without this they all share a key (seen on the ARM64 dev box, 2026-11-12).
fn function_part(tree: &dyn PnpTree, parent: &str) -> Option<String> {
    let n = tree.node(parent)?;
    if !n.location_paths.is_empty() || matches!(n.enumerator.as_str(), "USB" | "HDAUDIO" | "PCI") {
        return None;
    }
    let mid = parent.split('\\').nth(1)?;
    (!mid.is_empty()).then(|| mid.to_ascii_lowercase())
}

/// Key of an endpoint whose parent devnode is known.
pub fn endpoint_key(
    tree: &dyn PnpTree,
    flow: AudioFlow,
    parent_devnode: Option<&str>,
    endpoint_devnode: &str,
    form_factor: u32,
    node_type: Option<&str>,
    ref_pin: Option<&str>,
) -> DeviceKey {
    let (anchor, usb_interface, tie) = match parent_devnode {
        Some(p) => {
            let n = tree.node(p);
            let ids: Vec<String> = n
                .as_ref()
                .map(|n| std::iter::once(n.instance_id.clone()).chain(n.hardware_ids.iter().cloned()).collect())
                .unwrap_or_default();
            let tie = tie_breaker(tree, p).or_else(|| function_part(tree, p));
            (anchor_for(tree, p), usb_interface_number(&ids), tie)
        }
        None => (Anchor::Instance(endpoint_devnode.to_string()), None, None),
    };
    EndpointIdentity {
        flow,
        anchor,
        usb_interface,
        form_factor,
        node_type: node_type.map(str::to_string),
        ref_pin: ref_pin.map(str::to_string),
        tie_breaker: tie,
    }
    .key()
}

/// Key of a gated devnode (storage, camera, serial port, smart-card reader, printer,
/// network adapter, other USB device) or a USB hub.
pub fn device_key(tree: &dyn PnpTree, kind: DeviceKind, instance_id: &str) -> DeviceKey {
    let mut p = KeyParts::new(kind, anchor_for(tree, instance_id));
    if let Some(n) = tree.node(instance_id) {
        let ids: Vec<String> = std::iter::once(n.instance_id.clone()).chain(n.hardware_ids.iter().cloned()).collect();
        p.usb_interface = usb_interface_number(&ids);
        // The anchor of a child without its own location path is its ancestor's; a disk and
        // its USB mass-storage parent then share the anchor, which is fine because the prefix
        // differs. Two disks of one multi-LUN reader do collide: tell them apart by the LUN
        // suffix of the instance id (`...&0`, `...&1`).
        if kind == DeviceKind::Storage && n.location_paths.is_empty() {
            if let Some(lun) = n.instance_id.rsplit('&').next().filter(|t| t.len() <= 2 && t.chars().all(|c| c.is_ascii_digit())) {
                p.sub = Some(format!("lun{lun}"));
            }
        }
    }
    p.compose()
}

/// Key of the USB hub at `instance_id` (same rule every crate must use for `parent_hub`).
pub fn hub_key(tree: &dyn PnpTree, instance_id: &str) -> DeviceKey {
    KeyParts::new(DeviceKind::UsbHub, anchor_for(tree, instance_id)).compose()
}

/// NT target for `DefineDosDevice(DDD_RAW_TARGET_PATH, "E:", ...)` in the seat agent:
/// `\\?\Volume{GUID}\` → `\??\Volume{GUID}`.
pub fn raw_target_for_volume(volume_name: &str) -> String {
    let v = volume_name.trim_end_matches('\\');
    match v.strip_prefix(r"\\?\") {
        Some(rest) => format!(r"\??\{rest}"),
        None => v.to_string(),
    }
}

/// `{GUID}` part of a volume name (`\\?\Volume{GUID}\` → `{GUID}`, lower case).
pub fn volume_guid(volume_name: &str) -> Option<String> {
    let s = volume_name.find('{')?;
    let e = volume_name[s..].find('}')? + s;
    Some(volume_name[s..=e].to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use seat_core::mock::MockPnpTree;
    use seat_core::pnp::PnpNode;

    #[test]
    fn ks_ref_parsing() {
        let r = parse_ks_filter_ref(r"{2}.\\?\hdaudio#func_01&ven_10ec#4&1&0001#{6994ad04-93ef-11d0-a3cc-00a0c9223196}\elineouttopo/00010001").unwrap();
        assert_eq!(
            r,
            KsFilterRef::Interface {
                interface_path: r"\\?\hdaudio#func_01&ven_10ec#4&1&0001#{6994ad04-93ef-11d0-a3cc-00a0c9223196}\elineouttopo".into(),
                ref_pin: Some("ELINEOUTTOPO/00010001".into()),
            }
        );
        assert!(parse_ks_filter_ref("garbage").is_none());
        let r = parse_ks_filter_ref(r"\\?\usb#vid_1&pid_2&mi_00#7&1#{6994ad04-93ef-11d0-a3cc-00a0c9223196}").unwrap();
        assert_eq!(r.ref_pin(), None);
        let r = parse_ks_filter_ref(r"{1}.USB\VID_046D&PID_085E&MI_03\8&1FAA9F65&0&0003").unwrap();
        assert_eq!(r, KsFilterRef::Devnode { instance_id: r"USB\VID_046D&PID_085E&MI_03\8&1FAA9F65&0&0003".into() });
    }

    #[test]
    fn endpoint_keys_are_stable_and_distinct() {
        let base = EndpointIdentity {
            flow: AudioFlow::Render,
            anchor: Anchor::Location("PCIROOT(0)#PCI(1F03)".into()),
            usb_interface: None,
            form_factor: 1,
            node_type: Some("{DFF21CE1-F70F-11D0-B917-00A0C9223196}".into()),
            ref_pin: Some("WAVE/00010001".into()),
            tie_breaker: None,
        };
        assert_eq!(base.key().as_str(), "aout;loc=PCIROOT(0)#PCI(1F03);sub=fr1.dff21ce1.wave/00010001");
        let mut cap = base.clone();
        cap.flow = AudioFlow::Capture;
        assert!(cap.key().as_str().starts_with("ain;"));
        // Two identical BT headphones on one radio differ by the tie-breaker.
        let mut a = base.clone();
        a.tie_breaker = Some("A1B2C3D4E5F6".into());
        let mut b = base.clone();
        b.tie_breaker = Some("001122334455".into());
        assert_ne!(a.key(), b.key());
    }

    #[test]
    fn endpoint_key_from_tree() {
        let t = MockPnpTree::default();
        t.add(PnpNode {
            instance_id: r"USB\VID_1&PID_2\SER123".into(),
            location_paths: vec!["PCIROOT(0)#USBROOT(0)#USB(3)".into()],
            enumerator: "USB".into(),
            serial: Some("SER123".into()),
            present: true,
            ..Default::default()
        });
        t.add(PnpNode {
            instance_id: r"USB\VID_1&PID_2&MI_00\7&1&0000".into(),
            parent: Some(r"USB\VID_1&PID_2\SER123".into()),
            enumerator: "USB".into(),
            present: true,
            ..Default::default()
        });
        let k = endpoint_key(&t, AudioFlow::Render, Some(r"USB\VID_1&PID_2&MI_00\7&1&0000"), "SWD", 3, None, None);
        assert_eq!(k.as_str(), "aout;loc=PCIROOT(0)#USBROOT(0)#USB(3);if=0;sub=fr3.tser123");
        let k = endpoint_key(&t, AudioFlow::Capture, None, r"SWD\MMDEVAPI\{0.0.1.00000000}.{X}", 4, None, None);
        assert!(k.as_str().starts_with(r"ain;inst=SWD\MMDEVAPI\"));
    }

    #[test]
    fn volume_helpers() {
        assert_eq!(raw_target_for_volume(r"\\?\Volume{abc}\"), r"\??\Volume{abc}");
        assert_eq!(volume_guid(r"\\?\Volume{ABC-1}\").as_deref(), Some("{abc-1}"));
        assert_eq!(form_factor_name(9), "hdmi");
        assert_eq!(endpoint_state_name(0x1000_0008), "unplugged");
    }
}
