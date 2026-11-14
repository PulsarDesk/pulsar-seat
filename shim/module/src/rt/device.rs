// SPDX-License-Identifier: GPL-3.0-only
//! Device-interface path -> parent chain (child first) with hardware ids, via CfgMgr32
//! (`CM_Get_Device_Interface_PropertyW`, `CM_Locate_DevNodeW`, `CM_Get_Parent`,
//! `CM_Get_Device_IDW`, `CM_Get_DevNode_Registry_PropertyW`), cached per path.

use seat_shim_core::decide::{
    Access, DevNodeInfo, DeviceOrigin, classify_full, hid_access, interface_path_to_instance_id, rule_applies,
};
use seat_shim_core::regval::utf16_to_multi_sz;
use std::collections::HashMap;
use std::sync::Mutex;
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_DRP_HARDWAREID, CM_Get_DevNode_Registry_PropertyW, CM_Get_Device_IDW, CM_Get_Device_Interface_PropertyW,
    CM_Get_Parent, CM_LOCATE_DEVNODE_PHANTOM, CM_Locate_DevNodeW, CR_SUCCESS,
};
use windows_sys::Win32::Devices::Properties::DEVPKEY_Device_InstanceId;

static CACHE: Mutex<Option<HashMap<String, Vec<DevNodeInfo>>>> = Mutex::new(None);
const MAX_DEPTH: usize = 16;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn utf16_until_nul(b: &[u16]) -> String {
    let end = b.iter().position(|c| *c == 0).unwrap_or(b.len());
    String::from_utf16_lossy(&b[..end])
}

/// `\??\…` and `\\.\…` -> `\\?\…` (the form CfgMgr32 accepts).
fn normalise(path: &str) -> String {
    for p in [r"\??\", r"\\.\"] {
        if let Some(rest) = path.strip_prefix(p) {
            return format!(r"\\?\{rest}");
        }
    }
    path.to_string()
}

fn instance_id(path: &str) -> Option<String> {
    let w = wide(&normalise(path));
    let mut ty = 0u32;
    let mut buf = vec![0u16; 512];
    let mut size = (buf.len() * 2) as u32;
    // SAFETY: buffers valid for the given sizes.
    let cr = unsafe {
        CM_Get_Device_Interface_PropertyW(w.as_ptr(), &DEVPKEY_Device_InstanceId, &mut ty, buf.as_mut_ptr().cast(), &mut size, 0)
    };
    if cr == CR_SUCCESS {
        return Some(utf16_until_nul(&buf));
    }
    interface_path_to_instance_id(path)
}

fn node(devinst: u32) -> Option<DevNodeInfo> {
    let mut idbuf = vec![0u16; 512];
    // SAFETY: buffer valid for its length.
    if unsafe { CM_Get_Device_IDW(devinst, idbuf.as_mut_ptr(), idbuf.len() as u32, 0) } != CR_SUCCESS {
        return None;
    }
    let mut hw = vec![0u16; 1024];
    let mut len = (hw.len() * 2) as u32;
    let mut ty = 0u32;
    // SAFETY: buffer valid for `len` bytes.
    let cr = unsafe {
        CM_Get_DevNode_Registry_PropertyW(devinst, CM_DRP_HARDWAREID, &mut ty, hw.as_mut_ptr().cast(), &mut len, 0)
    };
    let hardware_ids = if cr == CR_SUCCESS { utf16_to_multi_sz(&hw[..(len as usize / 2).min(hw.len())]) } else { Vec::new() };
    Some(DevNodeInfo { instance_id: utf16_until_nul(&idbuf), hardware_ids })
}

fn walk(path: &str) -> Vec<DevNodeInfo> {
    let Some(id) = instance_id(path) else { return Vec::new() };
    let w = wide(&id);
    let mut devinst = 0u32;
    // SAFETY: valid NUL-terminated id.
    if unsafe { CM_Locate_DevNodeW(&mut devinst, w.as_ptr(), CM_LOCATE_DEVNODE_PHANTOM) } != CR_SUCCESS {
        // The id alone still carries the markers of our pads and RDS devices.
        return vec![DevNodeInfo { instance_id: id, hardware_ids: Vec::new() }];
    }
    let mut chain = Vec::new();
    for _ in 0..MAX_DEPTH {
        match node(devinst) {
            Some(n) => chain.push(n),
            None => break,
        }
        let mut parent = 0u32;
        // SAFETY: plain out parameter.
        if unsafe { CM_Get_Parent(&mut parent, devinst, 0) } != CR_SUCCESS {
            break;
        }
        devinst = parent;
    }
    chain
}

pub fn chain(path: &str) -> Vec<DevNodeInfo> {
    let key = path.to_ascii_lowercase();
    if let Some(c) = CACHE.lock().ok().and_then(|g| g.as_ref().and_then(|m| m.get(&key).cloned())) {
        return c;
    }
    let c = walk(path);
    if let Ok(mut g) = CACHE.lock() {
        let m = g.get_or_insert_with(HashMap::new);
        if m.len() > 512 {
            m.clear();
        }
        m.insert(key, c.clone());
    }
    c
}

fn origin_label(o: &DeviceOrigin) -> &'static str {
    match o {
        DeviceOrigin::Console => "console",
        DeviceOrigin::RemoteSession(_) => "remote_session",
        DeviceOrigin::SeatPad { .. } => "seat_pad",
        DeviceOrigin::ViGEmUnowned => "vigem_unowned",
        DeviceOrigin::KioskDevice { .. } => "kiosk_device",
        DeviceOrigin::Unknown => "unknown",
    }
}

/// Which hook group asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    /// An open (`NtCreateFile`, `CreateFile*`).
    Open,
    /// An enumeration (CfgMgr32, SetupAPI, `GetRawInputDeviceList`).
    Enumeration,
    /// A raw-input record (`GetRawInputData` / `GetRawInputBuffer`, kiosk role only).
    RawData,
}

/// HID / XUSB (and, while kiosk seats run, keyboard / mouse class) access decision for a
/// device-interface path; `None` when the group is off right now or the rule does not
/// look at this path (pass through).
pub fn decide(path: &str, check: Check) -> Option<Access> {
    let (role, plan, map, kiosk) = super::live()?;
    let on = match check {
        Check::Open => plan.hid_open,
        Check::Enumeration => plan.hid_enumeration,
        Check::RawData => plan.raw_input_data,
    };
    if !on || !rule_applies(&role, !kiosk.is_empty(), path) {
        return None;
    }
    let origin = classify_full(&chain(path), &map, &kiosk);
    let a = hid_access(&role, &origin);
    // Raw-input records arrive at input rate: not logged one by one.
    if check != Check::RawData {
        super::log::hid(path, origin_label(&origin), a == Access::Grant);
    }
    Some(a)
}
