// SPDX-License-Identifier: GPL-3.0-only
//! Real Windows implementations: CfgMgr32 device nodes, HKLM DWORD reads, node removal.
//!
//! Untested on a seat host: written from the public CfgMgr32 / registry documentation.
//! The dev machine has no RDP seat sessions, so enumeration only ever sees console
//! endpoints there, and removal needs SYSTEM/admin rights (never run by the tests).

use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_GETIDLIST_FILTER_ENUMERATOR, CM_Uninstall_DevNode, CONFIGRET, CR_ACCESS_DENIED,
    CR_INVALID_DEVICE_ID, CR_NO_SUCH_DEVNODE, CR_NO_SUCH_REGISTRY_KEY, CR_SUCCESS,
};
use windows_sys::Win32::Devices::Properties::{
    DEVPKEY_Device_DeviceDesc, DEVPKEY_Device_FriendlyName, DEVPKEY_Device_SessionId,
};

use crate::AudioError;
use crate::cfgmgr::{self, as_string, as_u32, devnode_prop, locate};
use crate::endpoint::DevNode;
use crate::policy::RegistryRead;
use crate::probe::{DeviceNodes, EndpointRemover, SystemProbe};

fn cr_err(what: &str, cr: CONFIGRET) -> AudioError {
    if cr == CR_ACCESS_DENIED {
        AudioError::Probe(format!("{what}: access denied"))
    } else {
        AudioError::Probe(format!("{what}: CONFIGRET {cr}"))
    }
}

/// CfgMgr32-backed [`DeviceNodes`] (over the shared [`crate::cfgmgr`] primitives).
#[derive(Debug, Default, Clone, Copy)]
pub struct CfgMgrNodes;

impl CfgMgrNodes {
    /// Devnode ids of one enumerator; an unknown enumerator is an empty list.
    fn id_list(enumerator: &str) -> Result<Vec<String>, AudioError> {
        match cfgmgr::id_list(Some(enumerator), CM_GETIDLIST_FILTER_ENUMERATOR) {
            Ok(v) => Ok(v),
            Err(e) if matches!(e.cr, CR_NO_SUCH_DEVNODE | CR_INVALID_DEVICE_ID | CR_NO_SUCH_REGISTRY_KEY) => Ok(Vec::new()),
            Err(e) => Err(cr_err(e.call, e.cr)),
        }
    }

    /// Property read; a failed read yields `None`, which the planner treats as "no
    /// session id" (never removed).
    fn prop_u32(dn: u32, key: &windows_sys::Win32::Foundation::DEVPROPKEY) -> Option<u32> {
        as_u32(devnode_prop(dn, key))
    }

    fn prop_string(dn: u32, key: &windows_sys::Win32::Foundation::DEVPROPKEY) -> Option<String> {
        as_string(devnode_prop(dn, key))
    }
}

impl DeviceNodes for CfgMgrNodes {
    fn list(&self, enumerator: &str) -> Result<Vec<DevNode>, AudioError> {
        let mut out = Vec::new();
        for id in Self::id_list(enumerator)? {
            let Some((dn, present)) = locate(&id) else {
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
        let (dn, present) = locate(instance_id)
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
