// SPDX-License-Identifier: GPL-3.0-only
//! Real Windows implementations: CfgMgr32 device nodes, HKLM DWORD reads, node removal.
//!
//! Untested on a seat host: written from the public CfgMgr32 / registry documentation.
//! The dev machine has no RDP seat sessions, so enumeration only ever sees console
//! endpoints there, and removal needs SYSTEM/admin rights (never run by the tests).

use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_GETIDLIST_FILTER_ENUMERATOR, CM_Get_DevNode_PropertyW, CM_Get_Device_ID_ListW,
    CM_Get_Device_ID_List_SizeW, CM_LOCATE_DEVNODE_NORMAL, CM_LOCATE_DEVNODE_PHANTOM,
    CM_Locate_DevNodeW, CM_Uninstall_DevNode, CONFIGRET, CR_ACCESS_DENIED, CR_BUFFER_SMALL,
    CR_INVALID_DEVICE_ID, CR_NO_SUCH_DEVNODE, CR_NO_SUCH_REGISTRY_KEY, CR_NO_SUCH_VALUE,
    CR_SUCCESS,
};
use windows_sys::Win32::Devices::Properties::{
    DEVPKEY_Device_DeviceDesc, DEVPKEY_Device_FriendlyName, DEVPKEY_Device_SessionId,
    DEVPROP_TYPE_STRING, DEVPROP_TYPE_UINT32, DEVPROPTYPE,
};
use windows_sys::Win32::Foundation::DEVPROPKEY;

use crate::AudioError;
use crate::endpoint::DevNode;
use crate::policy::RegistryRead;
use crate::probe::{DeviceNodes, EndpointRemover, SystemProbe};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn cr_err(what: &str, cr: CONFIGRET) -> AudioError {
    if cr == CR_ACCESS_DENIED {
        AudioError::Probe(format!("{what}: access denied"))
    } else {
        AudioError::Probe(format!("{what}: CONFIGRET {cr}"))
    }
}

/// CfgMgr32-backed [`DeviceNodes`].
#[derive(Debug, Default, Clone, Copy)]
pub struct CfgMgrNodes;

impl CfgMgrNodes {
    fn id_list(enumerator: &str) -> Result<Vec<String>, AudioError> {
        let filter = wide(enumerator);
        // The list can grow between the two calls; retry a few times on CR_BUFFER_SMALL.
        for _ in 0..4 {
            let mut len: u32 = 0;
            // SAFETY: `filter` is a NUL-terminated UTF-16 string; `len` is a valid out ptr.
            let cr = unsafe {
                CM_Get_Device_ID_List_SizeW(&mut len, filter.as_ptr(), CM_GETIDLIST_FILTER_ENUMERATOR)
            };
            if matches!(cr, CR_NO_SUCH_DEVNODE | CR_INVALID_DEVICE_ID | CR_NO_SUCH_REGISTRY_KEY) {
                return Ok(Vec::new());
            }
            if cr != CR_SUCCESS {
                return Err(cr_err("CM_Get_Device_ID_List_SizeW", cr));
            }
            let mut buf = vec![0u16; len.max(2) as usize];
            // SAFETY: `buf` holds `buf.len()` u16 elements; the call writes at most that.
            let cr = unsafe {
                CM_Get_Device_ID_ListW(
                    filter.as_ptr(),
                    buf.as_mut_ptr(),
                    buf.len() as u32,
                    CM_GETIDLIST_FILTER_ENUMERATOR,
                )
            };
            if cr == CR_BUFFER_SMALL {
                continue;
            }
            if matches!(cr, CR_NO_SUCH_DEVNODE | CR_INVALID_DEVICE_ID | CR_NO_SUCH_REGISTRY_KEY) {
                return Ok(Vec::new());
            }
            if cr != CR_SUCCESS {
                return Err(cr_err("CM_Get_Device_ID_ListW", cr));
            }
            return Ok(split_multi_sz(&buf));
        }
        Err(AudioError::Probe("CM_Get_Device_ID_ListW: list kept growing".into()))
    }

    /// (devinst, present). `None` if no node (present or phantom) exists.
    fn locate(instance_id: &str) -> Option<(u32, bool)> {
        let w = wide(instance_id);
        let mut dn: u32 = 0;
        // SAFETY: valid NUL-terminated id and out pointer.
        if unsafe { CM_Locate_DevNodeW(&mut dn, w.as_ptr(), CM_LOCATE_DEVNODE_NORMAL) } == CR_SUCCESS {
            return Some((dn, true));
        }
        // SAFETY: as above.
        if unsafe { CM_Locate_DevNodeW(&mut dn, w.as_ptr(), CM_LOCATE_DEVNODE_PHANTOM) } == CR_SUCCESS {
            return Some((dn, false));
        }
        None
    }

    /// Property bytes, `None` when missing or unreadable. A failed read is logged and
    /// yields `None`, which the planner treats as "no session id" (never removed).
    fn prop_raw(dn: u32, key: &DEVPROPKEY) -> Option<(DEVPROPTYPE, Vec<u8>)> {
        // The value can grow between the size query and the read; retry on CR_BUFFER_SMALL.
        for _ in 0..3 {
            let mut ty: DEVPROPTYPE = 0;
            let mut size: u32 = 0;
            // SAFETY: size query with a null buffer is the documented pattern.
            let cr = unsafe { CM_Get_DevNode_PropertyW(dn, key, &mut ty, std::ptr::null_mut(), &mut size, 0) };
            if cr == CR_NO_SUCH_VALUE {
                return None;
            }
            if (cr != CR_BUFFER_SMALL && cr != CR_SUCCESS) || size == 0 {
                log::debug!("audio: CM_Get_DevNode_PropertyW size query: CONFIGRET {cr}");
                return None;
            }
            let mut buf = vec![0u8; size as usize];
            // SAFETY: `buf` has `size` bytes.
            let cr = unsafe { CM_Get_DevNode_PropertyW(dn, key, &mut ty, buf.as_mut_ptr(), &mut size, 0) };
            if cr == CR_BUFFER_SMALL {
                continue;
            }
            if cr != CR_SUCCESS {
                log::debug!("audio: CM_Get_DevNode_PropertyW: CONFIGRET {cr}");
                return None;
            }
            buf.truncate(size as usize);
            return Some((ty, buf));
        }
        log::debug!("audio: CM_Get_DevNode_PropertyW: value kept growing");
        None
    }

    fn prop_u32(dn: u32, key: &DEVPROPKEY) -> Option<u32> {
        match Self::prop_raw(dn, key)? {
            (DEVPROP_TYPE_UINT32, b) if b.len() == 4 => Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]])),
            _ => None,
        }
    }

    fn prop_string(dn: u32, key: &DEVPROPKEY) -> Option<String> {
        match Self::prop_raw(dn, key)? {
            (DEVPROP_TYPE_STRING, b) => {
                let w: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
                let end = w.iter().position(|&c| c == 0).unwrap_or(w.len());
                let s = String::from_utf16_lossy(&w[..end]);
                (!s.is_empty()).then_some(s)
            }
            _ => None,
        }
    }
}

fn split_multi_sz(buf: &[u16]) -> Vec<String> {
    buf.split(|&c| c == 0)
        .take_while(|s| !s.is_empty())
        .map(String::from_utf16_lossy)
        .collect()
}

impl DeviceNodes for CfgMgrNodes {
    fn list(&self, enumerator: &str) -> Result<Vec<DevNode>, AudioError> {
        let mut out = Vec::new();
        for id in Self::id_list(enumerator)? {
            let Some((dn, present)) = Self::locate(&id) else {
                // Disappeared between list and locate (or a key without a node).
                continue;
            };
            out.push(DevNode {
                session_id: Self::prop_u32(dn, &DEVPKEY_Device_SessionId),
                friendly_name: Self::prop_string(dn, &DEVPKEY_Device_FriendlyName)
                    .or_else(|| Self::prop_string(dn, &DEVPKEY_Device_DeviceDesc)),
                instance_id: id,
                present,
            });
        }
        Ok(out)
    }
}

/// `winreg`-backed [`RegistryRead`] (HKLM, read only).
#[derive(Debug, Default, Clone, Copy)]
pub struct WinRegistry;

impl RegistryRead for WinRegistry {
    fn hklm_dword(&self, subkey: &str, value: &str) -> Result<Option<u32>, AudioError> {
        use std::io::ErrorKind;
        use winreg::RegKey;
        use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ, REG_DWORD};
        let key = match RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey_with_flags(subkey, KEY_READ) {
            Ok(k) => k,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(AudioError::Probe(format!("open HKLM\\{subkey}: {e}"))),
        };
        match key.get_raw_value(value) {
            Ok(v) if v.vtype == REG_DWORD && v.bytes.len() == 4 => {
                let b = &v.bytes;
                Ok(Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]])))
            }
            Ok(_) => {
                log::warn!("audio: HKLM\\{subkey}\\{value} is not a DWORD; ignored");
                Ok(None)
            }
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(AudioError::Probe(format!("read HKLM\\{subkey}\\{value}: {e}"))),
        }
    }
}

/// The production probe.
pub type WinAudioProbe = SystemProbe<CfgMgrNodes, WinRegistry>;

/// Production probe for the default listener.
pub fn system_probe() -> WinAudioProbe {
    SystemProbe::new(CfgMgrNodes, WinRegistry)
}

/// CfgMgr32 remover (`CM_Uninstall_DevNode`). Needs SYSTEM / admin. Refuses present
/// nodes and anything that is not an RDP session MMDEVAPI endpoint. Never run by tests.
#[derive(Debug, Default, Clone, Copy)]
pub struct CfgMgrRemover;

impl EndpointRemover for CfgMgrRemover {
    fn remove(&self, instance_id: &str, expected_session_id: u32) -> Result<(), AudioError> {
        crate::check_removable_id(instance_id)?;
        let (dn, present) = CfgMgrNodes::locate(instance_id)
            .ok_or_else(|| AudioError::RemoveRefused(format!("{instance_id} not found")))?;
        if present {
            return Err(AudioError::RemoveRefused(format!("{instance_id} is present")));
        }
        // Re-read the session id right before the uninstall: Windows reuses the node for
        // the same session id (spec 70 §2.2), so a changed or unreadable value means the
        // plan is stale. The caller holds the seat-start lock (see remove_stale_endpoints).
        let sid = CfgMgrNodes::prop_u32(dn, &DEVPKEY_Device_SessionId);
        if sid != Some(expected_session_id) {
            return Err(AudioError::RemoveRefused(format!(
                "{instance_id}: session id {sid:?} != planned {expected_session_id}"
            )));
        }
        // SAFETY: `dn` came from CM_Locate_DevNodeW for a phantom node.
        let cr = unsafe { CM_Uninstall_DevNode(dn, 0) };
        if cr != CR_SUCCESS {
            return Err(cr_err("CM_Uninstall_DevNode", cr));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multi_sz_split() {
        let mut b: Vec<u16> = "A\\1".encode_utf16().collect();
        b.push(0);
        b.extend("B\\2".encode_utf16());
        b.extend([0, 0, 0]);
        assert_eq!(split_multi_sz(&b), vec!["A\\1", "B\\2"]);
        assert!(split_multi_sz(&[0, 0]).is_empty());
    }

    /// Read-only smoke test against the live machine: enumerates MMDEVAPI nodes and reads
    /// the policy. Changes nothing. Ignored by default because results depend on the host.
    #[test]
    #[ignore = "reads the live device tree and HKLM; run manually with --ignored"]
    fn live_read_only_probe() {
        use crate::probe::AudioProbe;
        let p = system_probe();
        let eps = p.endpoints().expect("enumerate");
        for e in &eps {
            println!("{:?} present={} sid={:?} {}", e.flow, e.present, e.session_id, e.endpoint_id);
        }
        println!("policy: {:?}", p.policy().expect("policy"));
        println!("steam present: {}", p.steam_devices_present());
    }
}
