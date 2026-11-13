// SPDX-License-Identifier: GPL-3.0-only
//! Pure display model: the CCD snapshot format shared by the service and the console-side
//! helper, stable monitor keys (spec 40 §6.1), EDID parsing, key resolution with the EDID
//! fallback, `A.S` labels, RDP multi-monitor layout and presenter geometry.
//!
//! Nothing here calls Windows; everything is unit tested on every platform.

use seat_core::device::{Device, DeviceDetails, DisplayInfo, DisplayMode, EdidId, Presentation, Rect};
use seat_core::backend::display::CursorState;
use seat_core::device::DeviceKind;
use seat_core::ids::DeviceKey;
use seat_core::pnp::{anchor_for, nearest_hub, Anchor, KeyParts, PnpTree};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

/// Hardware id of our own IddCx adapter (`drivers/seat-idd`). Its virtual monitors are
/// never offered as seat monitors.
pub const PULSAR_IDD_HWID: &str = "Root\\PulsarSeatIdd";

/// One CCD target as seen from the console session (helper output, JSON).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TargetInfo {
    /// Adapter LUID (runtime only; changes every boot).
    pub adapter_luid: u64,
    /// Adapter device-interface path from `DISPLAYCONFIG_ADAPTER_NAME`.
    pub adapter_path: String,
    /// CCD target id (stable for the same connector).
    pub target_id: u32,
    /// Source id when the path is active.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<u32>,
    /// Part of the console desktop (active path).
    pub active: bool,
    /// A monitor is connected (`targetAvailable`).
    pub available: bool,
    /// Raw `DISPLAYCONFIG_VIDEO_OUTPUT_TECHNOLOGY`.
    pub output_technology: u32,
    pub monitor_name: String,
    /// Monitor device-interface path (`\\?\DISPLAY#...#{e6f07b5f-...}`).
    pub monitor_path: String,
    /// `edidManufactureId` / `edidProductCodeId` from the CCD target name (0 when unknown).
    pub edid_manufacture_id: u16,
    pub edid_product_code: u16,
    pub connector_instance: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desktop_rect: Option<Rect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<DisplayMode>,
    /// Preferred (native) mode of the monitor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred: Option<DisplayMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gdi_name: Option<String>,
    /// Source at desktop (0, 0).
    pub primary: bool,
    /// `Windows.Devices.Display.Core` reports the monitor as special purpose (removed from
    /// the desktop by the user) — the Display Core presenter may acquire it.
    pub specialized: bool,
    /// Monitor specialization (`GET_MONITOR_SPECIALIZATION`): `Some(true)` = Windows can
    /// specialize this monitor, `Some(false)` = not this monitor / not this system, `None`
    /// = the call is unknown (older Windows). Added 2026-11-13.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub specialization_available: Option<bool>,
    /// Effective DPI of the monitor (96 = 100 %), 0 when unknown.
    #[serde(default)]
    pub dpi: u32,
}

/// Result of a console-side display query.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Snapshot {
    pub targets: Vec<TargetInfo>,
    /// Display Core probe outcome: `None` = not probed, `Some(Ok)` = DisplayManager works,
    /// `Some(Err(reason))` = it failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_core: Option<Result<(), String>>,
}

/// Snake_case output technology (`DisplayInfo::output_technology`).
pub fn output_technology_name(raw: u32) -> &'static str {
    match raw {
        0 => "vga",
        4 => "dvi",
        5 => "hdmi",
        6 | 11 | 13 | 0x8000_0000 => "internal",
        10 | 18 => "displayport",
        15 => "miracast",
        16 | 17 => "indirect",
        _ => "other",
    }
}

/// `\\?\DISPLAY#DEL4123#5&1a2b&0&UID4352#{guid}` → `DISPLAY\DEL4123\5&1a2b&0&UID4352`.
pub fn interface_path_to_instance(path: &str) -> Option<String> {
    let p = path.strip_prefix(r"\\?\").or_else(|| path.strip_prefix(r"\\.\"))?;
    let cut = p.rfind("#{").unwrap_or(p.len());
    let body = &p[..cut];
    if body.is_empty() {
        return None;
    }
    Some(body.replace('#', "\\").to_ascii_uppercase())
}

/// Decode the 3-letter PnP manufacturer id from its big-endian EDID form.
pub fn decode_manufacturer(be: u16) -> String {
    let c = |shift: u16| {
        let v = ((be >> shift) & 0x1f) as u8;
        if (1..=26).contains(&v) { (b'A' + v - 1) as char } else { '?' }
    };
    [c(10), c(5), c(0)].iter().collect()
}

/// Parsed EDID base block (only what identity and the virtual monitor need).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Edid {
    pub manufacturer: String,
    pub product_code: u16,
    /// Numeric serial (bytes 12..16), 0 when absent.
    pub serial: u32,
    /// Display product serial string descriptor (0xFF).
    pub serial_text: Option<String>,
    /// Display product name descriptor (0xFC).
    pub name: Option<String>,
    /// Preferred timing from the first detailed timing descriptor.
    pub preferred: Option<DisplayMode>,
}

/// Parse an EDID base block (128 bytes or more). Returns `None` for a bad header.
pub fn parse_edid(b: &[u8]) -> Option<Edid> {
    if b.len() < 128 || b[0..8] != [0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00] {
        return None;
    }
    let mut e = Edid {
        manufacturer: decode_manufacturer(u16::from_be_bytes([b[8], b[9]])),
        product_code: u16::from_le_bytes([b[10], b[11]]),
        serial: u32::from_le_bytes([b[12], b[13], b[14], b[15]]),
        ..Default::default()
    };
    for i in 0..4 {
        let d = &b[54 + i * 18..54 + (i + 1) * 18];
        let pixel_clock = u16::from_le_bytes([d[0], d[1]]);
        if pixel_clock != 0 {
            if e.preferred.is_none() {
                let ha = d[2] as u32 | ((d[4] as u32 & 0xf0) << 4);
                let hb = d[3] as u32 | ((d[4] as u32 & 0x0f) << 8);
                let va = d[5] as u32 | ((d[7] as u32 & 0xf0) << 4);
                let vb = d[6] as u32 | ((d[7] as u32 & 0x0f) << 8);
                let total = (ha + hb) as u64 * (va + vb) as u64;
                if total > 0 && ha > 0 && va > 0 {
                    // pixel clock in 10 kHz units → refresh in mHz.
                    let mhz = (pixel_clock as u64 * 10_000 * 1000) / total;
                    e.preferred = Some(DisplayMode { width: ha, height: va, refresh_mhz: mhz as u32, rotation: 0 });
                }
            }
            continue;
        }
        let text = || {
            let s: String = d[5..18].iter().take_while(|&&c| c != 0x0a).map(|&c| c as char).collect();
            let s = s.trim().to_string();
            (!s.is_empty()).then_some(s)
        };
        match d[3] {
            0xff => e.serial_text = text(),
            0xfc => e.name = text(),
            _ => {}
        }
    }
    Some(e)
}

/// EDID identity: from the EDID blob when we have one, else from the CCD target name
/// (manufacturer id there is the EDID word with its bytes swapped; verified 2026-11-12 on
/// the ARM64 dev box: CCD 0xB306 = "AUS").
pub fn edid_identity(t: &TargetInfo, edid: Option<&Edid>) -> Option<EdidId> {
    if let Some(e) = edid {
        return Some(EdidId { manufacturer: e.manufacturer.clone(), product_code: e.product_code, serial: e.serial });
    }
    if t.edid_manufacture_id == 0 && t.edid_product_code == 0 {
        return None;
    }
    Some(EdidId {
        manufacturer: decode_manufacturer(t.edid_manufacture_id.swap_bytes()),
        product_code: t.edid_product_code,
        serial: 0,
    })
}

/// Adapter anchor per spec 40 §6.1.
///
/// | adapter | anchor |
/// | --- | --- |
/// | PCI GPU | location path |
/// | USB display adapter with a serial | instance path |
/// | USB display adapter without serial | container id + hardware id |
/// | root / software / indirect | instance path |
pub fn adapter_anchor(tree: &dyn PnpTree, adapter_instance: &str) -> Anchor {
    let Some(node) = tree.node(adapter_instance) else {
        return Anchor::Instance(adapter_instance.to_string());
    };
    let enumerator = node.enumerator.to_ascii_uppercase();
    if enumerator == "ROOT" || enumerator == "SWD" {
        return Anchor::Instance(node.instance_id.clone());
    }
    // USB anywhere above (or at) the adapter: the location path follows the port, so key by
    // the instance path when the USB device has a serial number.
    let mut cur = Some(node.clone());
    let mut depth = 0;
    while let Some(n) = cur {
        if n.enumerator.eq_ignore_ascii_case("USB") && !is_usb_interface(&n.instance_id) {
            let has_serial = n.serial.is_some() || instance_has_serial(&n.instance_id);
            if has_serial {
                return Anchor::Instance(node.instance_id.clone());
            }
            let container = node.container_id.clone().or(n.container_id.clone()).unwrap_or_default();
            let hwid = node.hardware_ids.first().cloned().unwrap_or_else(|| node.instance_id.clone());
            return Anchor::Instance(format!("{container}|{hwid}"));
        }
        depth += 1;
        if depth > 32 {
            break;
        }
        cur = n.parent.as_deref().and_then(|p| tree.node(p));
    }
    if let Some(p) = node.location_paths.first() {
        return Anchor::Location(p.clone());
    }
    anchor_for(tree, adapter_instance)
}

fn is_usb_interface(instance: &str) -> bool {
    instance.to_ascii_uppercase().contains("&MI_")
}

/// USB instance ids end in the serial when the device has one; otherwise in a generated
/// `<n>&<hash>&<n>&<n>` id that contains `&`.
pub fn instance_has_serial(instance: &str) -> bool {
    instance.rsplit('\\').next().is_some_and(|tail| !tail.is_empty() && !tail.contains('&'))
}

/// Monitor key: adapter anchor + `sub=t<target id>`.
pub fn monitor_key(anchor: Anchor, target_id: u32) -> DeviceKey {
    let mut p = KeyParts::new(DeviceKind::Monitor, anchor);
    p.sub = Some(format!("t{target_id}"));
    p.compose()
}

/// The anchor part of a monitor key (`mon;loc=...;sub=t5` → `mon;loc=...`).
pub fn key_anchor(key: &DeviceKey) -> &str {
    let s = key.as_str();
    s.find(";sub=").map_or(s, |i| &s[..i])
}

/// Target id encoded in a monitor key.
pub fn key_target_id(key: &DeviceKey) -> Option<u32> {
    key.as_str().rsplit(";sub=t").next().filter(|_| key.as_str().contains(";sub=t"))?.parse().ok()
}

/// EDID blob reader (registry `Device Parameters\EDID` of the monitor devnode).
pub type EdidReader<'a> = &'a dyn Fn(&str) -> Option<Vec<u8>>;

/// A live monitor: device record plus runtime data the binding needs.
#[derive(Debug, Clone, PartialEq)]
pub struct LiveMonitor {
    pub device: Device,
    pub target: TargetInfo,
    pub monitor_instance: Option<String>,
    pub adapter_instance: Option<String>,
    pub edid_blob: Option<Vec<u8>>,
}

impl LiveMonitor {
    pub fn info(&self) -> &DisplayInfo {
        match &self.device.details {
            DeviceDetails::Display(d) => d,
            _ => unreachable!("live monitors always carry display details"),
        }
    }
}

/// Turn a snapshot into live monitors with stable keys. Targets without a monitor, and
/// our own virtual monitors, are left out. Duplicated keys get `KeyCollision`.
pub fn build_monitors(snap: &Snapshot, tree: &dyn PnpTree, read_edid: EdidReader<'_>) -> Vec<LiveMonitor> {
    // Adapter numbers in LUID order (≈ initialisation order), 1-based, for `A.S` labels.
    let mut luids: Vec<u64> = snap.targets.iter().map(|t| t.adapter_luid).collect();
    luids.sort_unstable();
    luids.dedup();
    let adapter_no = |luid: u64| luids.iter().position(|&l| l == luid).map_or(0, |i| i + 1);

    let mut out: Vec<LiveMonitor> = Vec::new();
    for t in &snap.targets {
        if !t.available && !t.active {
            continue;
        }
        let adapter_instance = tree.interface_owner(&t.adapter_path).or_else(|| interface_path_to_instance(&t.adapter_path));
        let adapter_node = adapter_instance.as_deref().and_then(|a| tree.node(a));
        if adapter_node
            .as_ref()
            .is_some_and(|n| n.hardware_ids.iter().any(|h| h.eq_ignore_ascii_case(PULSAR_IDD_HWID)))
        {
            continue;
        }
        let monitor_instance = if t.monitor_path.is_empty() {
            None
        } else {
            tree.interface_owner(&t.monitor_path).or_else(|| interface_path_to_instance(&t.monitor_path))
        };
        let monitor_node = monitor_instance.as_deref().and_then(|m| tree.node(m));
        let anchor = match adapter_instance.as_deref() {
            Some(a) => adapter_anchor(tree, a),
            None => Anchor::Instance(format!("luid{:016x}", t.adapter_luid)),
        };
        let key = monitor_key(anchor, t.target_id);
        let edid_blob = monitor_instance.as_deref().and_then(read_edid);
        let edid = edid_blob.as_deref().and_then(parse_edid);
        let edid_id = edid_identity(t, edid.as_ref());
        let monitor_name = if !t.monitor_name.is_empty() {
            t.monitor_name.clone()
        } else if let Some(n) = edid.as_ref().and_then(|e| e.name.clone()) {
            n
        } else {
            monitor_node.as_ref().and_then(|n| n.friendly_name.clone()).unwrap_or_default()
        };
        let adapter_name = adapter_node.as_ref().and_then(|n| n.friendly_name.clone()).unwrap_or_default();
        let label = match t.source_id {
            Some(s) if t.active => format!("{}.{}", adapter_no(t.adapter_luid), s + 1),
            _ => format!("{}.-", adapter_no(t.adapter_luid)),
        };
        let serial = edid
            .as_ref()
            .and_then(|e| e.serial_text.clone().or((e.serial != 0).then(|| e.serial.to_string())));
        let name = if monitor_name.is_empty() { "Ekran".to_string() } else { monitor_name.clone() };
        let parent_hub = monitor_instance
            .as_deref()
            .or(adapter_instance.as_deref())
            .and_then(|m| nearest_hub(tree, m))
            .map(|hub| KeyParts::new(DeviceKind::UsbHub, anchor_for(tree, &hub)).compose());
        let device = Device {
            key,
            kind: DeviceKind::Monitor,
            name,
            label: Some(label),
            instance_id: monitor_instance.clone(),
            container_id: monitor_node.as_ref().and_then(|n| n.container_id.clone()),
            parent_hub,
            present: true,
            hardware_id: monitor_node.as_ref().and_then(|n| n.hardware_ids.first().cloned()),
            serial,
            warnings: Vec::new(),
            details: DeviceDetails::Display(DisplayInfo {
                adapter_name,
                adapter_luid: t.adapter_luid,
                target_id: t.target_id,
                output_technology: output_technology_name(t.output_technology).to_string(),
                monitor_name,
                edid: edid_id,
                mode: t.mode.or(t.preferred),
                gdi_name: t.gdi_name.clone(),
                desktop_rect: if t.active { t.desktop_rect } else { None },
                in_console_desktop: t.active,
                presentation: if t.active { Presentation::Native } else { Presentation::None },
            }),
        };
        out.push(LiveMonitor { device, target: t.clone(), monitor_instance, adapter_instance, edid_blob });
    }
    // Key collisions (two live targets with one key cannot be told apart).
    let mut count: HashMap<DeviceKey, usize> = HashMap::new();
    for m in &out {
        *count.entry(m.device.key.clone()).or_default() += 1;
    }
    for m in &mut out {
        if count[&m.device.key] > 1 {
            m.device.warnings.push(seat_core::device::DeviceWarning::KeyCollision);
        }
    }
    out
}

/// How a saved key was matched to a live monitor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    Exact,
    /// Same adapter anchor, other target id, same EDID (connector renumbered, MST).
    SameAdapterEdid,
    /// Other adapter, same EDID with a non-zero serial (GPU or dock swapped).
    EdidOnly,
}

/// Resolve a saved monitor key to a live monitor. `hint` is the EDID last seen for the
/// key. Fallbacks never pick a monitor whose own key is in `taken` (claimed exactly by
/// another assignment).
pub fn resolve_key<'a>(
    key: &DeviceKey,
    live: &'a [LiveMonitor],
    hint: Option<&EdidId>,
    taken: &[DeviceKey],
) -> Option<(&'a LiveMonitor, Resolution)> {
    if let Some(m) = live.iter().find(|m| &m.device.key == key) {
        return Some((m, Resolution::Exact));
    }
    let hint = hint?;
    let free = |m: &&LiveMonitor| !taken.contains(&m.device.key);
    let same_edid = |m: &&LiveMonitor| m.info().edid.as_ref() == Some(hint);
    let anchor = key_anchor(key);
    let same: Vec<&LiveMonitor> =
        live.iter().filter(|m| key_anchor(&m.device.key) == anchor).filter(free).filter(same_edid).collect();
    if same.len() == 1 {
        return Some((same[0], Resolution::SameAdapterEdid));
    }
    if hint.serial == 0 {
        return None;
    }
    let any: Vec<&LiveMonitor> = live.iter().filter(free).filter(same_edid).collect();
    (any.len() == 1).then(|| (any[0], Resolution::EdidOnly))
}

/// Lay out targets that are not in the console desktop (specialized / virtual) left to
/// right so each output still has a distinct rectangle for the RDP layout.
pub fn virtual_rects(modes: &[DisplayMode]) -> Vec<Rect> {
    let mut x = 0i32;
    modes
        .iter()
        .map(|m| {
            let (w, h) = rotated(m);
            let r = Rect { x, y: 0, width: w, height: h };
            x += w as i32;
            r
        })
        .collect()
}

/// Width / height after rotation (90 / 270 swap the axes).
pub fn rotated(m: &DisplayMode) -> (u32, u32) {
    if m.rotation == 90 || m.rotation == 270 { (m.height, m.width) } else { (m.width, m.height) }
}

/// One monitor of an RDP client multi-monitor layout (`TS_MONITOR_DEF` + the monitor
/// layout PDU attributes, MS-RDPBCGR 2.2.1.3.6 / MS-RDPEDISP).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RdpMonitor {
    pub left: i32,
    pub top: i32,
    /// Inclusive.
    pub right: i32,
    /// Inclusive.
    pub bottom: i32,
    pub primary: bool,
    /// Desktop scale factor in percent (100..=500).
    pub scale: u32,
    /// Orientation in degrees (0, 90, 180, 270).
    pub orientation: u16,
}

/// RDP layout of a seat's outputs: the first output is the primary monitor at (0, 0);
/// the others keep their position relative to it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RdpLayout {
    pub monitors: Vec<RdpMonitor>,
    /// Bounding box size of all monitors (the RDP desktop size).
    pub desktop_width: u32,
    pub desktop_height: u32,
}

/// Build the RDP layout from placement rects and per-monitor DPI (0 = 96).
pub fn rdp_layout(rects: &[Rect], dpis: &[u32], orientations: &[u16]) -> RdpLayout {
    let Some(first) = rects.first() else { return RdpLayout::default() };
    let (ox, oy) = (first.x, first.y);
    let monitors: Vec<RdpMonitor> = rects
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let dpi = dpis.get(i).copied().filter(|&d| d > 0).unwrap_or(96);
            RdpMonitor {
                left: r.x - ox,
                top: r.y - oy,
                right: r.x - ox + r.width.max(1) as i32 - 1,
                bottom: r.y - oy + r.height.max(1) as i32 - 1,
                primary: i == 0,
                scale: (dpi * 100 / 96).clamp(100, 500),
                orientation: orientations.get(i).copied().unwrap_or(0),
            }
        })
        .collect();
    let min_x = monitors.iter().map(|m| m.left).min().unwrap_or(0);
    let min_y = monitors.iter().map(|m| m.top).min().unwrap_or(0);
    let max_x = monitors.iter().map(|m| m.right).max().unwrap_or(0);
    let max_y = monitors.iter().map(|m| m.bottom).max().unwrap_or(0);
    RdpLayout { monitors, desktop_width: (max_x - min_x + 1) as u32, desktop_height: (max_y - min_y + 1) as u32 }
}

/// Destination rectangle (relative to the window) for a `src_w`×`src_h` frame shown in a
/// `dst_w`×`dst_h` window, aspect ratio kept, centred (letterbox / pillarbox).
pub fn letterbox(src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> Rect {
    if src_w == 0 || src_h == 0 || dst_w == 0 || dst_h == 0 {
        return Rect { x: 0, y: 0, width: dst_w, height: dst_h };
    }
    if src_w == dst_w && src_h == dst_h {
        return Rect { x: 0, y: 0, width: dst_w, height: dst_h };
    }
    let (sw, sh, dw, dh) = (src_w as u64, src_h as u64, dst_w as u64, dst_h as u64);
    let (w, h) = if sw * dh > dw * sh { (dw, (sh * dw / sw).max(1)) } else { ((sw * dh / sh).max(1), dh) };
    Rect { x: ((dw - w) / 2) as i32, y: ((dh - h) / 2) as i32, width: w as u32, height: h as u32 }
}

pub fn intersects(a: &Rect, b: &Rect) -> bool {
    let (ar, ab) = (a.x + a.width as i32, a.y + a.height as i32);
    let (br, bb) = (b.x + b.width as i32, b.y + b.height as i32);
    a.x < br && b.x < ar && a.y < bb && b.y < ab
}

pub fn contains_point(r: &Rect, x: i32, y: i32) -> bool {
    x >= r.x && y >= r.y && x < r.x + r.width as i32 && y < r.y + r.height as i32
}

fn bounding(rects: &[Rect]) -> Option<Rect> {
    let first = rects.first()?;
    let (mut l, mut t, mut r, mut b) = (first.x, first.y, first.x + first.width as i32, first.y + first.height as i32);
    for x in &rects[1..] {
        l = l.min(x.x);
        t = t.min(x.y);
        r = r.max(x.x + x.width as i32);
        b = b.max(x.y + x.height as i32);
    }
    Some(Rect { x: l, y: t, width: (r - l) as u32, height: (b - t) as u32 })
}

/// Cursor clip for the console user (spec 40 §6.3.7): the console monitors are every
/// monitor not covered by a seat presenter. When their bounding box does not touch a seat
/// monitor it is used whole; otherwise the console monitor holding the cursor (or the
/// primary one) is used. `None` = nothing to clip (no seat monitors, or no console left).
pub fn console_clip(monitors: &[Rect], seat_rects: &[Rect], cursor: (i32, i32), primary: Option<Rect>) -> Option<Rect> {
    if seat_rects.is_empty() {
        return None;
    }
    let console: Vec<Rect> =
        monitors.iter().copied().filter(|m| !seat_rects.iter().any(|s| intersects(m, s))).collect();
    let bbox = bounding(&console)?;
    if !seat_rects.iter().any(|s| intersects(&bbox, s)) {
        return Some(bbox);
    }
    console
        .iter()
        .find(|m| contains_point(m, cursor.0, cursor.1))
        .copied()
        .or_else(|| primary.filter(|p| console.contains(p)))
        .or_else(|| console.first().copied())
}

/// Where a foreign window that landed on a seat monitor is moved back to: same size
/// (clamped), centred on `home`.
pub fn move_back(window: &Rect, home: &Rect) -> Rect {
    let w = window.width.min(home.width);
    let h = window.height.min(home.height);
    Rect { x: home.x + ((home.width - w) / 2) as i32, y: home.y + ((home.height - h) / 2) as i32, width: w, height: h }
}

/// Centre of a rectangle.
pub fn centre(r: &Rect) -> (i32, i32) {
    (r.x + r.width as i32 / 2, r.y + r.height as i32 / 2)
}

/// Diff two inventories by key (hot-plug events): (arrived, removed, changed).
pub fn diff_devices(old: &[Device], new: &[Device]) -> (Vec<Device>, Vec<DeviceKey>, Vec<Device>) {
    let o: BTreeMap<&DeviceKey, &Device> = old.iter().map(|d| (&d.key, d)).collect();
    let n: BTreeMap<&DeviceKey, &Device> = new.iter().map(|d| (&d.key, d)).collect();
    let arrived = new.iter().filter(|d| !o.contains_key(&d.key)).cloned().collect();
    let removed = old.iter().filter(|d| !n.contains_key(&d.key)).map(|d| d.key.clone()).collect();
    let changed = new.iter().filter(|d| o.get(&d.key).is_some_and(|od| *od != *d)).cloned().collect();
    (arrived, removed, changed)
}

/// Rotate a tight BGRA frame `w`×`h` clockwise by `deg` (0 / 90 / 180 / 270; anything
/// else = 0). Returns the frame and its new width / height.
pub fn rotate_bgra(src: &[u8], w: u32, h: u32, deg: u16) -> (Vec<u8>, u32, u32) {
    let (w, h) = (w as usize, h as usize);
    if src.len() < w * h * 4 {
        return (vec![0; w * h * 4], w as u32, h as u32);
    }
    let (nw, nh) = if deg == 90 || deg == 270 { (h, w) } else { (w, h) };
    let mut out = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let (dx, dy) = match deg {
                90 => (h - 1 - y, x),
                180 => (w - 1 - x, h - 1 - y),
                270 => (y, w - 1 - x),
                _ => (x, y),
            };
            let si = (y * w + x) * 4;
            let di = (dy * nw + dx) * 4;
            out[di..di + 4].copy_from_slice(&src[si..si + 4]);
        }
    }
    (out, nw as u32, nh as u32)
}

/// Nearest-neighbour scale of a BGRA frame (`stride` bytes per source row) to a tight
/// `dw`×`dh` frame, aspect ratio kept (black bars).
pub fn scale_nearest(src: &[u8], sw: u32, sh: u32, stride: u32, dw: u32, dh: u32) -> Vec<u8> {
    let mut out = vec![0u8; (dw * dh * 4) as usize];
    if sw == 0 || sh == 0 {
        return out;
    }
    let r = letterbox(sw, sh, dw, dh);
    for y in 0..r.height {
        let sy = (y as u64 * sh as u64 / r.height as u64) as usize;
        for x in 0..r.width {
            let sx = (x as u64 * sw as u64 / r.width as u64) as usize;
            let si = sy * stride as usize + sx * 4;
            let di = (((r.y as u32 + y) * dw + r.x as u32 + x) * 4) as usize;
            if si + 4 <= src.len() {
                out[di..di + 4].copy_from_slice(&src[si..si + 4]);
            }
        }
    }
    out
}

/// Pick the mode closest to `want` (same size first, then nearest refresh).
pub fn closest_mode(modes: &[DisplayMode], want: &DisplayMode) -> Option<DisplayMode> {
    modes
        .iter()
        .min_by_key(|m| {
            let size = (m.width as i64 - want.width as i64).abs() + (m.height as i64 - want.height as i64).abs();
            (size, (m.refresh_mhz as i64 - want.refresh_mhz as i64).abs())
        })
        .copied()
}

// ---- presenter math (flip-model path, clone, mode follow) ---------------------------

/// Swap-chain buffer for a `fw`×`fh` frame in a `ww`×`wh` window, with
/// `DXGI_SCALING_STRETCH` doing the scaling: the buffer has the window's aspect ratio and
/// is at least as large as the frame in both axes, and the frame sits centred in it (black
/// bars). Returns `(buffer_w, buffer_h, frame_x, frame_y)`.
pub fn flip_buffer(fw: u32, fh: u32, ww: u32, wh: u32) -> (u32, u32, u32, u32) {
    if fw == 0 || fh == 0 || ww == 0 || wh == 0 {
        return (ww.max(1), wh.max(1), 0, 0);
    }
    let (f_w, f_h, w_w, w_h) = (fw as u64, fh as u64, ww as u64, wh as u64);
    let (bw, bh) = if f_w * w_h >= w_w * f_h {
        // Frame wider than the window: full width, taller buffer.
        (f_w, (f_w * w_h).div_ceil(w_w).max(f_h))
    } else {
        ((f_h * w_w).div_ceil(w_h).max(f_w), f_h)
    };
    let (bw, bh) = (bw.min(16384) as u32, bh.min(16384) as u32);
    (bw, bh, (bw.saturating_sub(fw)) / 2, (bh.saturating_sub(fh)) / 2)
}

/// Clip dirty rects to the frame. `None` = upload the whole frame (no rects given, too
/// many of them, or they cover most of it).
pub fn coalesce_dirty(rects: &[Rect], fw: u32, fh: u32) -> Option<Vec<Rect>> {
    if rects.is_empty() || rects.len() > 32 {
        return None;
    }
    let mut out = Vec::with_capacity(rects.len());
    let mut area = 0u64;
    for r in rects {
        let x0 = r.x.clamp(0, fw as i32);
        let y0 = r.y.clamp(0, fh as i32);
        let x1 = (r.x.saturating_add(r.width as i32)).clamp(0, fw as i32);
        let y1 = (r.y.saturating_add(r.height as i32)).clamp(0, fh as i32);
        if x1 > x0 && y1 > y0 {
            let c = Rect { x: x0, y: y0, width: (x1 - x0) as u32, height: (y1 - y0) as u32 };
            area += c.width as u64 * c.height as u64;
            out.push(c);
        }
    }
    if area * 2 > fw as u64 * fh as u64 {
        return None;
    }
    Some(out)
}

/// Copy `region` out of a tight `fw`-wide BGRA frame (region must lie inside the frame).
pub fn copy_region(frame: &[u8], fw: u32, region: &Rect) -> Vec<u8> {
    let row = fw as usize * 4;
    let (x, y, w, h) = (region.x.max(0) as usize, region.y.max(0) as usize, region.width as usize, region.height as usize);
    let mut out = Vec::with_capacity(w * h * 4);
    for yy in y..y + h {
        let s = yy * row + x * 4;
        match frame.get(s..s + w * 4) {
            Some(src) => out.extend_from_slice(src),
            None => out.extend(std::iter::repeat_n(0u8, w * 4)),
        }
    }
    out
}

/// Alpha-blend a premultiplied BGRA cursor into a tight BGRA buffer that holds the frame
/// area `region` (frame coordinates). Skips a shape shorter than its size says. The
/// buffer's alpha stays untouched.
pub fn blend_cursor_region(dst: &mut [u8], region: &Rect, c: &CursorState, pm: &[u8]) {
    let need = (c.width as usize).checked_mul(c.height as usize).and_then(|n| n.checked_mul(4));
    if !c.visible || !need.is_some_and(|n| n > 0 && pm.len() >= n) || dst.len() < region.width as usize * region.height as usize * 4 {
        return;
    }
    let ox = c.x as i64 - c.hotspot_x as i64 - region.x as i64;
    let oy = c.y as i64 - c.hotspot_y as i64 - region.y as i64;
    let (w, h) = (region.width as i64, region.height as i64);
    for cy in 0..c.height as i64 {
        let y = oy + cy;
        if y < 0 || y >= h {
            continue;
        }
        for cx in 0..c.width as i64 {
            let x = ox + cx;
            if x < 0 || x >= w {
                continue;
            }
            let si = ((cy * c.width as i64 + cx) * 4) as usize;
            let di = ((y * w + x) * 4) as usize;
            let a = pm[si + 3] as u32;
            for k in 0..3 {
                dst[di + k] = (pm[si + k] as u32 + dst[di + k] as u32 * (255 - a) / 255).min(255) as u8;
            }
        }
    }
}

/// Screen area of a cursor in frame coordinates (`None` when hidden).
pub fn cursor_rect(c: &CursorState) -> Option<Rect> {
    (c.visible && c.width > 0 && c.height > 0).then(|| Rect { x: c.x - c.hotspot_x as i32, y: c.y - c.hotspot_y as i32, width: c.width, height: c.height })
}

/// The mode to switch a target to so it matches the seat session's resolution
/// (`want` = frame size, desktop orientation; spec 40 §6.5). Only an exact size match
/// counts (otherwise the presenter scales); among those the refresh rate closest to the
/// current one wins.
pub fn pick_follow_mode(modes: &[DisplayMode], want: (u32, u32), current_refresh_mhz: u32) -> Option<DisplayMode> {
    modes
        .iter()
        .filter(|m| rotated(m) == want)
        .min_by_key(|m| (m.refresh_mhz as i64 - current_refresh_mhz as i64).abs())
        .copied()
}

/// Debounce for mode follow: the session resolution must stay the same for `settle`
/// before the target's mode is switched, and a size that could not be applied is not
/// tried again until the session moves to another size.
#[derive(Debug, Clone)]
pub struct ModeFollower {
    current: (u32, u32),
    seen: Option<((u32, u32), std::time::Instant)>,
    tried: Option<(u32, u32)>,
    settle: std::time::Duration,
}

impl ModeFollower {
    pub fn new(current: (u32, u32), settle: std::time::Duration) -> Self {
        Self { current, seen: None, tried: None, settle }
    }

    /// The target now shows `size` (after a mode change or a re-acquire).
    pub fn set_current(&mut self, size: (u32, u32)) {
        self.current = size;
        self.seen = None;
    }

    pub fn current(&self) -> (u32, u32) {
        self.current
    }

    /// Feed one frame size. `Some(size)` once when it is time to try switching to it.
    pub fn observe(&mut self, size: (u32, u32), now: std::time::Instant) -> Option<(u32, u32)> {
        if size == self.current || size.0 == 0 || size.1 == 0 {
            self.seen = None;
            self.tried = None;
            return None;
        }
        if self.tried == Some(size) {
            return None;
        }
        match self.seen {
            Some((s, since)) if s == size => {
                if now.duration_since(since) >= self.settle {
                    self.tried = Some(size);
                    self.seen = None;
                    return Some(size);
                }
                None
            }
            _ => {
                self.seen = Some((size, now));
                None
            }
        }
    }
}

/// Seconds the session resolution must be stable before the presenter follows it.
pub const MODE_FOLLOW_SETTLE: std::time::Duration = std::time::Duration::from_millis(1500);

#[cfg(test)]
pub(crate) mod tests_support {
    pub(crate) use super::tests::{target, tree};
    pub(crate) fn edid(serial: u32) -> Vec<u8> {
        super::tests::sample_edid(serial, "DELL")
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use seat_core::mock::MockPnpTree;
    use seat_core::pnp::PnpNode;

    pub(crate) fn sample_edid(serial: u32, name: &str) -> Vec<u8> {
        let mut b = vec![0u8; 128];
        b[0..8].copy_from_slice(&[0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00]);
        // "DEL" = 4, 5, 12 → 0b00100_00101_01100 = 0x10AC
        b[8..10].copy_from_slice(&0x10acu16.to_be_bytes());
        b[10..12].copy_from_slice(&0x4123u16.to_le_bytes());
        b[12..16].copy_from_slice(&serial.to_le_bytes());
        // Detailed timing 1920x1080@60: pixel clock 148.5 MHz, totals 2200 x 1125.
        let d = &mut b[54..72];
        d[0..2].copy_from_slice(&14850u16.to_le_bytes());
        d[2] = (1920 & 0xff) as u8;
        d[3] = (280 & 0xff) as u8;
        d[4] = (((1920 >> 8) << 4) | (280 >> 8)) as u8;
        d[5] = (1080 & 0xff) as u8;
        d[6] = 45;
        d[7] = ((1080 >> 8) << 4) as u8;
        // Name descriptor.
        let n = &mut b[72..90];
        n[3] = 0xfc;
        for (i, c) in name.bytes().take(13).enumerate() {
            n[5 + i] = c;
        }
        if name.len() < 13 {
            n[5 + name.len()] = 0x0a;
        }
        b
    }

    pub(crate) fn tree() -> MockPnpTree {
        let t = MockPnpTree::default();
        t.add(PnpNode {
            instance_id: "PCI\\VEN_10DE&DEV_2484\\4&1".into(),
            location_paths: vec!["PCIROOT(0)#PCI(0100)#PCI(0000)".into()],
            enumerator: "PCI".into(),
            friendly_name: Some("GPU".into()),
            present: true,
            ..Default::default()
        });
        t.add(PnpNode {
            instance_id: "DISPLAY\\DEL4123\\5&A&0&UID4352".into(),
            parent: Some("PCI\\VEN_10DE&DEV_2484\\4&1".into()),
            hardware_ids: vec!["MONITOR\\DEL4123".into()],
            enumerator: "DISPLAY".into(),
            present: true,
            ..Default::default()
        });
        t.add(PnpNode {
            instance_id: "USB\\VID_17E9&PID_6006\\ABC123".into(),
            enumerator: "USB".into(),
            location_paths: vec!["PCIROOT(0)#PCI(1400)#USBROOT(0)#USB(3)".into()],
            container_id: Some("{c0}".into()),
            present: true,
            ..Default::default()
        });
        t.add(PnpNode {
            instance_id: "USB\\VID_17E9&PID_6006&MI_00\\7&B&0&0000".into(),
            parent: Some("USB\\VID_17E9&PID_6006\\ABC123".into()),
            enumerator: "USB".into(),
            hardware_ids: vec!["USB\\VID_17E9&PID_6006&MI_00".into()],
            location_paths: vec!["PCIROOT(0)#PCI(1400)#USBROOT(0)#USB(3)#USBMI(0)".into()],
            present: true,
            ..Default::default()
        });
        t.add(PnpNode {
            instance_id: "USB\\VID_17E9&PID_6007\\6&C&0&2".into(),
            enumerator: "USB".into(),
            hardware_ids: vec!["USB\\VID_17E9&PID_6007".into()],
            container_id: Some("{c1}".into()),
            location_paths: vec!["PCIROOT(0)#PCI(1400)#USBROOT(0)#USB(4)".into()],
            present: true,
            ..Default::default()
        });
        t.add(PnpNode {
            instance_id: "ROOT\\DISPLAY\\0000".into(),
            enumerator: "ROOT".into(),
            hardware_ids: vec![PULSAR_IDD_HWID.into()],
            present: true,
            ..Default::default()
        });
        t
    }

    pub(crate) fn target(luid: u64, tid: u32, active: bool, x: i32) -> TargetInfo {
        TargetInfo {
            adapter_luid: luid,
            adapter_path: r"\\?\PCI#VEN_10DE&DEV_2484#4&1#{5b45201d-f2f2-4f3b-85bb-30ff1f953599}".into(),
            target_id: tid,
            source_id: active.then_some(0),
            active,
            available: true,
            output_technology: 10,
            monitor_name: "DELL U2720Q".into(),
            monitor_path: r"\\?\DISPLAY#DEL4123#5&A&0&UID4352#{e6f07b5f-ee97-4a90-b076-33f57bf4eaa7}".into(),
            edid_manufacture_id: 0xac10,
            edid_product_code: 0x4123,
            connector_instance: 0,
            desktop_rect: active.then_some(Rect { x, y: 0, width: 1920, height: 1080 }),
            mode: Some(DisplayMode { width: 1920, height: 1080, refresh_mhz: 60000, rotation: 0 }),
            preferred: None,
            gdi_name: active.then(|| "\\\\.\\DISPLAY1".into()),
            primary: x == 0 && active,
            specialized: false,
            specialization_available: Some(true),
            dpi: 144,
        }
    }

    #[test]
    fn edid_parse() {
        let e = parse_edid(&sample_edid(77, "DELL U2720Q")).unwrap();
        assert_eq!(e.manufacturer, "DEL");
        assert_eq!(e.product_code, 0x4123);
        assert_eq!(e.serial, 77);
        assert_eq!(e.name.as_deref(), Some("DELL U2720Q"));
        let p = e.preferred.unwrap();
        assert_eq!((p.width, p.height, p.refresh_mhz), (1920, 1080, 60000));
        assert!(parse_edid(&[0u8; 128]).is_none());
        let t = target(1, 1, true, 0);
        assert_eq!(edid_identity(&t, None).unwrap().manufacturer, "DEL");
        // Real CCD value from the ARM64 dev box (ASUS VG27A).
        assert_eq!(decode_manufacturer(0xb306u16.swap_bytes()), "AUS");
    }

    #[test]
    fn paths_and_tech() {
        assert_eq!(
            interface_path_to_instance(r"\\?\DISPLAY#DEL4123#5&a&0&UID4352#{e6f07b5f-ee97-4a90-b076-33f57bf4eaa7}").as_deref(),
            Some("DISPLAY\\DEL4123\\5&A&0&UID4352")
        );
        assert_eq!(interface_path_to_instance("garbage"), None);
        assert_eq!(output_technology_name(5), "hdmi");
        assert_eq!(output_technology_name(0x8000_0000), "internal");
        assert_eq!(output_technology_name(17), "indirect");
    }

    #[test]
    fn anchors_per_adapter_kind() {
        let t = tree();
        assert_eq!(adapter_anchor(&t, "PCI\\VEN_10DE&DEV_2484\\4&1"), Anchor::Location("PCIROOT(0)#PCI(0100)#PCI(0000)".into()));
        // USB adapter function under a device with a serial → instance path.
        assert_eq!(
            adapter_anchor(&t, "USB\\VID_17E9&PID_6006&MI_00\\7&B&0&0000"),
            Anchor::Instance("USB\\VID_17E9&PID_6006&MI_00\\7&B&0&0000".into())
        );
        // USB adapter without serial → container + hardware id.
        assert_eq!(adapter_anchor(&t, "USB\\VID_17E9&PID_6007\\6&C&0&2"), Anchor::Instance("{c1}|USB\\VID_17E9&PID_6007".into()));
        assert_eq!(adapter_anchor(&t, "ROOT\\DISPLAY\\0000"), Anchor::Instance("ROOT\\DISPLAY\\0000".into()));
        assert!(instance_has_serial("USB\\VID_1&PID_2\\ABC"));
        assert!(!instance_has_serial("USB\\VID_1&PID_2\\6&1&0&2"));
    }

    #[test]
    fn build_keys_labels_edid() {
        let t = tree();
        let snap = Snapshot { targets: vec![target(5, 4352, true, 0), target(5, 4353, false, 0)], display_core: None };
        let read = |inst: &str| (inst == "DISPLAY\\DEL4123\\5&A&0&UID4352").then(|| sample_edid(9, "DELL"));
        let mons = build_monitors(&snap, &t, &read);
        assert_eq!(mons.len(), 2);
        let k = mons[0].device.key.as_str();
        assert_eq!(k, "mon;loc=PCIROOT(0)#PCI(0100)#PCI(0000);sub=t4352");
        assert_eq!(key_target_id(&mons[0].device.key), Some(4352));
        assert_eq!(key_anchor(&mons[0].device.key), "mon;loc=PCIROOT(0)#PCI(0100)#PCI(0000)");
        assert_eq!(mons[0].device.label.as_deref(), Some("1.1"));
        assert_eq!(mons[1].device.label.as_deref(), Some("1.-"));
        assert_eq!(mons[0].device.hardware_id.as_deref(), Some("MONITOR\\DEL4123"));
        assert_eq!(mons[0].device.serial.as_deref(), Some("9"));
        assert_eq!(mons[0].info().edid.as_ref().unwrap().serial, 9);
        assert!(mons[0].info().in_console_desktop);
        assert_eq!(mons[1].info().desktop_rect, None);
        assert!(mons[0].device.warnings.is_empty());
    }

    #[test]
    fn skips_own_idd_and_unplugged() {
        let t = tree();
        let mut own = target(7, 1, true, 0);
        own.adapter_path = r"\\?\ROOT#DISPLAY#0000#{5b45201d-f2f2-4f3b-85bb-30ff1f953599}".into();
        let mut gone = target(5, 2, false, 0);
        gone.available = false;
        let mons = build_monitors(&Snapshot { targets: vec![own, gone], display_core: None }, &t, &|_| None);
        assert!(mons.is_empty());
    }

    #[test]
    fn collisions_flagged() {
        let t = tree();
        let mons = build_monitors(&Snapshot { targets: vec![target(5, 1, true, 0), target(5, 1, true, 1920)], display_core: None }, &t, &|_| None);
        assert!(mons.iter().all(|m| m.device.warnings.contains(&seat_core::device::DeviceWarning::KeyCollision)));
    }

    #[test]
    fn resolve_with_edid_fallback() {
        let t = tree();
        let read = |_: &str| Some(sample_edid(9, "DELL"));
        let mons = build_monitors(&Snapshot { targets: vec![target(5, 4400, true, 0)], display_core: None }, &t, &read);
        let old = DeviceKey::new("mon;loc=PCIROOT(0)#PCI(0100)#PCI(0000);sub=t4352").unwrap();
        let hint = mons[0].info().edid.clone().unwrap();
        assert!(resolve_key(&old, &mons, None, &[]).is_none());
        let (m, how) = resolve_key(&old, &mons, Some(&hint), &[]).unwrap();
        assert_eq!(how, Resolution::SameAdapterEdid);
        assert_eq!(m.target.target_id, 4400);
        let other = DeviceKey::new("mon;loc=PCIROOT(0)#PCI(0300);sub=t1").unwrap();
        assert_eq!(resolve_key(&other, &mons, Some(&hint), &[]).unwrap().1, Resolution::EdidOnly);
        assert!(resolve_key(&other, &mons, Some(&hint), std::slice::from_ref(&mons[0].device.key)).is_none());
        let exact = mons[0].device.key.clone();
        assert_eq!(resolve_key(&exact, &mons, None, &[]).unwrap().1, Resolution::Exact);
    }

    #[test]
    fn rdp_layout_relative_to_primary() {
        let rects = [Rect { x: 1920, y: 0, width: 1920, height: 1080 }, Rect { x: 3840, y: -200, width: 1080, height: 1920 }];
        let l = rdp_layout(&rects, &[144, 0], &[0, 90]);
        assert_eq!(l.monitors[0], RdpMonitor { left: 0, top: 0, right: 1919, bottom: 1079, primary: true, scale: 150, orientation: 0 });
        assert_eq!((l.monitors[1].left, l.monitors[1].top, l.monitors[1].scale), (1920, -200, 100));
        assert_eq!((l.desktop_width, l.desktop_height), (3000, 1920));
        assert_eq!(rdp_layout(&[], &[], &[]), RdpLayout::default());
        let v = virtual_rects(&[DisplayMode { width: 100, height: 50, refresh_mhz: 0, rotation: 90 }, DisplayMode { width: 10, height: 10, refresh_mhz: 0, rotation: 0 }]);
        assert_eq!(v[1], Rect { x: 50, y: 0, width: 10, height: 10 });
    }

    #[test]
    fn geometry() {
        assert_eq!(letterbox(1920, 1080, 1920, 1200), Rect { x: 0, y: 60, width: 1920, height: 1080 });
        assert_eq!(letterbox(1280, 1024, 1920, 1080), Rect { x: 285, y: 0, width: 1350, height: 1080 });
        let mons = [
            Rect { x: 0, y: 0, width: 1920, height: 1080 },
            Rect { x: 1920, y: 0, width: 1920, height: 1080 },
            Rect { x: -1920, y: 0, width: 1920, height: 1080 },
        ];
        // Seat on the right: console = left two, bbox does not touch the seat.
        assert_eq!(console_clip(&mons, &[mons[1]], (5, 5), Some(mons[0])), Some(Rect { x: -1920, y: 0, width: 3840, height: 1080 }));
        // Seat in the middle: bbox would cover it → the monitor under the cursor.
        assert_eq!(console_clip(&mons, &[mons[0]], (-5, 5), Some(mons[0])), Some(mons[2]));
        assert_eq!(console_clip(&mons, &[], (0, 0), None), None);
        assert_eq!(console_clip(&mons[..1], &[mons[0]], (0, 0), None), None);
        let w = Rect { x: 2000, y: 10, width: 3000, height: 500 };
        assert_eq!(move_back(&w, &mons[0]), Rect { x: 0, y: 290, width: 1920, height: 500 });
        assert_eq!(centre(&mons[1]), (2880, 540));
    }

    #[test]
    fn diffs_and_modes() {
        let t = tree();
        let a = build_monitors(&Snapshot { targets: vec![target(5, 1, true, 0)], display_core: None }, &t, &|_| None);
        let b = build_monitors(&Snapshot { targets: vec![target(5, 2, true, 0)], display_core: None }, &t, &|_| None);
        let da: Vec<Device> = a.iter().map(|m| m.device.clone()).collect();
        let db: Vec<Device> = b.iter().map(|m| m.device.clone()).collect();
        let (arr, rem, ch) = diff_devices(&da, &db);
        assert_eq!((arr.len(), rem.len(), ch.len()), (1, 1, 0));
        let modes = [
            DisplayMode { width: 1920, height: 1080, refresh_mhz: 60000, rotation: 0 },
            DisplayMode { width: 1920, height: 1080, refresh_mhz: 144000, rotation: 0 },
            DisplayMode { width: 2560, height: 1440, refresh_mhz: 60000, rotation: 0 },
        ];
        let want = DisplayMode { width: 1920, height: 1080, refresh_mhz: 120000, rotation: 0 };
        assert_eq!(closest_mode(&modes, &want).unwrap().refresh_mhz, 144000);
        // 2x1 red/blue → 4x2.
        let src = [0, 0, 255, 255, 255, 0, 0, 255];
        let out = scale_nearest(&src, 2, 1, 8, 4, 2);
        assert_eq!(&out[0..4], &[0, 0, 255, 255]);
        assert_eq!(&out[12..16], &[255, 0, 0, 255]);
    }

    #[test]
    fn rotates_geometry() {
        // 2x1: pixel A (left) then B (right).
        let a = [1u8, 1, 1, 1];
        let b = [2u8, 2, 2, 2];
        let src = [a, b].concat();
        let (r, w, h) = rotate_bgra(&src, 2, 1, 90);
        assert_eq!((w, h), (1, 2));
        assert_eq!(r, [a, b].concat(), "clockwise 90: left pixel goes to the top");
        let (r, w, h) = rotate_bgra(&src, 2, 1, 270);
        assert_eq!((w, h), (1, 2));
        assert_eq!(r, [b, a].concat());
        let (r, w, h) = rotate_bgra(&src, 2, 1, 180);
        assert_eq!((w, h), (2, 1));
        assert_eq!(r, [b, a].concat());
        assert_eq!(rotate_bgra(&src, 2, 1, 0).0, src.to_vec());
        // Round trip 90 + 270 on a 3x2 frame.
        let src: Vec<u8> = (0..24).collect();
        let (r, w, h) = rotate_bgra(&src, 3, 2, 90);
        let (back, w2, h2) = rotate_bgra(&r, w, h, 270);
        assert_eq!((w2, h2), (3, 2));
        assert_eq!(back, src);
    }

    #[test]
    fn flip_buffer_keeps_aspect() {
        assert_eq!(flip_buffer(1920, 1080, 1920, 1080), (1920, 1080, 0, 0));
        // 16:9 frame in a 16:10 window: taller buffer, bars top and bottom.
        assert_eq!(flip_buffer(1920, 1080, 1920, 1200), (1920, 1200, 0, 60));
        // 4:3 frame in a 16:9 window: wider buffer, bars left and right.
        assert_eq!(flip_buffer(1024, 768, 1920, 1080), (1366, 768, 171, 0));
        // Small frame in a big window: buffer stays frame-sized (DXGI stretches).
        assert_eq!(flip_buffer(1280, 720, 3840, 2160), (1280, 720, 0, 0));
        assert_eq!(flip_buffer(0, 0, 800, 600), (800, 600, 0, 0));
    }

    #[test]
    fn dirty_rects_and_regions() {
        let r = |x, y, w, h| Rect { x, y, width: w, height: h };
        assert_eq!(coalesce_dirty(&[], 100, 100), None);
        assert_eq!(coalesce_dirty(&[r(-5, -5, 10, 10), r(90, 90, 20, 20), r(200, 0, 5, 5)], 100, 100), Some(vec![r(0, 0, 5, 5), r(90, 90, 10, 10)]));
        assert_eq!(coalesce_dirty(&[r(0, 0, 80, 80)], 100, 100), None, "most of the frame: full upload");
        // 2x2 frame, pixel value = index.
        let f: Vec<u8> = (0..16u8).collect();
        assert_eq!(copy_region(&f, 2, &r(1, 1, 1, 1)), vec![12, 13, 14, 15]);
        let c = CursorState { visible: true, x: 1, y: 1, width: 1, height: 1, ..Default::default() };
        let mut d = vec![0u8; 16];
        blend_cursor_region(&mut d, &r(0, 0, 2, 2), &c, &[200, 100, 50, 255]);
        assert_eq!(&d[12..16], &[200, 100, 50, 0]);
        // Same cursor into a region that starts at the cursor.
        let mut d = vec![0u8; 4];
        blend_cursor_region(&mut d, &r(1, 1, 1, 1), &c, &[9, 9, 9, 255]);
        assert_eq!(d, vec![9, 9, 9, 0]);
        blend_cursor_region(&mut d, &r(1, 1, 1, 1), &CursorState { width: 2, ..c.clone() }, &[1; 4]);
        assert_eq!(d, vec![9, 9, 9, 0], "short shape skipped");
        assert_eq!(cursor_rect(&CursorState { hotspot_x: 1, ..c }), Some(r(0, 1, 1, 1)));
    }

    #[test]
    fn mode_follow_debounce_and_pick() {
        use std::time::{Duration, Instant};
        let m = |w, h, hz| DisplayMode { width: w, height: h, refresh_mhz: hz, rotation: 0 };
        let modes = [m(1920, 1080, 60000), m(1920, 1080, 144000), m(1280, 720, 60000)];
        assert_eq!(pick_follow_mode(&modes, (1920, 1080), 120000), Some(m(1920, 1080, 144000)));
        assert_eq!(pick_follow_mode(&modes, (1600, 900), 60000), None);
        let portrait = [DisplayMode { rotation: 90, ..m(1920, 1080, 60000) }];
        assert_eq!(pick_follow_mode(&portrait, (1080, 1920), 60000), Some(portrait[0]));
        let t0 = Instant::now();
        let mut f = ModeFollower::new((1920, 1080), Duration::from_secs(1));
        assert_eq!(f.observe((1920, 1080), t0), None);
        assert_eq!(f.observe((1280, 720), t0), None);
        assert_eq!(f.observe((1280, 720), t0 + Duration::from_millis(500)), None);
        assert_eq!(f.observe((1280, 720), t0 + Duration::from_secs(1)), Some((1280, 720)));
        // Failed switch: not retried for the same size.
        assert_eq!(f.observe((1280, 720), t0 + Duration::from_secs(5)), None);
        // Back to the current size resets; a new size starts over.
        assert_eq!(f.observe((1920, 1080), t0 + Duration::from_secs(6)), None);
        assert_eq!(f.observe((1280, 720), t0 + Duration::from_secs(7)), None);
        f.set_current((1280, 720));
        assert_eq!(f.current(), (1280, 720));
        assert_eq!(f.observe((1280, 720), t0 + Duration::from_secs(9)), None);
    }
}
