// SPDX-License-Identifier: GPL-3.0-only
//! In-memory fakes for tests (this crate's and the integrator's, via feature `fake`).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::AudioError;
use crate::endpoint::{DevNode, Endpoint};
use crate::policy::{AudioPolicy, RegistryRead};
use crate::probe::{AudioProbe, DeviceNodes, EndpointRemover};

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Fake HKLM: `(subkey lower-case, value lower-case) -> DWORD`. Case-insensitive like
/// the registry.
#[derive(Clone, Default)]
pub struct FakeRegistry {
    values: Arc<Mutex<BTreeMap<(String, String), u32>>>,
    fail: Arc<Mutex<bool>>,
}

impl FakeRegistry {
    pub fn set(&self, subkey: &str, value: &str, data: u32) {
        lock(&self.values).insert((subkey.to_ascii_lowercase(), value.to_ascii_lowercase()), data);
    }
    pub fn remove(&self, subkey: &str, value: &str) {
        lock(&self.values).remove(&(subkey.to_ascii_lowercase(), value.to_ascii_lowercase()));
    }
    /// Make every read fail with `AudioError::Probe`.
    pub fn fail(&self, on: bool) {
        *lock(&self.fail) = on;
    }
}

impl RegistryRead for FakeRegistry {
    fn hklm_dword(&self, subkey: &str, value: &str) -> Result<Option<u32>, AudioError> {
        if *lock(&self.fail) {
            return Err(AudioError::Probe("fake registry failure".into()));
        }
        Ok(lock(&self.values)
            .get(&(subkey.to_ascii_lowercase(), value.to_ascii_lowercase()))
            .copied())
    }
}

/// Fake CfgMgr32 tree: enumerator (case-insensitive) -> nodes.
#[derive(Clone, Default)]
pub struct FakeDeviceNodes {
    nodes: Arc<Mutex<BTreeMap<String, Vec<DevNode>>>>,
    fail: Arc<Mutex<bool>>,
}

impl FakeDeviceNodes {
    pub fn add(&self, enumerator: &str, node: DevNode) {
        lock(&self.nodes).entry(enumerator.to_ascii_uppercase()).or_default().push(node);
    }
    pub fn fail(&self, on: bool) {
        *lock(&self.fail) = on;
    }
    /// Node by instance id (case-insensitive), any enumerator.
    pub fn find(&self, instance_id: &str) -> Option<DevNode> {
        lock(&self.nodes)
            .values()
            .flatten()
            .find(|n| n.instance_id.eq_ignore_ascii_case(instance_id))
            .cloned()
    }
    fn delete(&self, instance_id: &str) -> bool {
        let mut g = lock(&self.nodes);
        let mut hit = false;
        for v in g.values_mut() {
            let before = v.len();
            v.retain(|n| !n.instance_id.eq_ignore_ascii_case(instance_id));
            hit |= v.len() != before;
        }
        hit
    }
}

impl DeviceNodes for FakeDeviceNodes {
    fn list(&self, enumerator: &str) -> Result<Vec<DevNode>, AudioError> {
        if *lock(&self.fail) {
            return Err(AudioError::Probe("fake cfgmgr failure".into()));
        }
        Ok(lock(&self.nodes).get(&enumerator.to_ascii_uppercase()).cloned().unwrap_or_default())
    }
}

/// Fake remover over a [`FakeDeviceNodes`]: refuses present / unknown / non-MMDEVAPI
/// nodes like the real one, records what it removed.
#[derive(Clone)]
pub struct FakeRemover {
    nodes: FakeDeviceNodes,
    removed: Arc<Mutex<Vec<String>>>,
    fail_ids: Arc<Mutex<Vec<String>>>,
}

impl FakeRemover {
    pub fn new(nodes: FakeDeviceNodes) -> Self {
        Self { nodes, removed: Arc::default(), fail_ids: Arc::default() }
    }
    /// Make removal of this id fail with `AudioError::Probe`.
    pub fn fail_on(&self, instance_id: &str) {
        lock(&self.fail_ids).push(instance_id.to_string());
    }
    pub fn removed(&self) -> Vec<String> {
        lock(&self.removed).clone()
    }
}

impl EndpointRemover for FakeRemover {
    fn remove(&self, instance_id: &str, expected_session_id: u32) -> Result<(), AudioError> {
        crate::check_removable_id(instance_id)?;
        let node = self
            .nodes
            .find(instance_id)
            .ok_or_else(|| AudioError::RemoveRefused(format!("{instance_id} not found")))?;
        if node.present {
            return Err(AudioError::RemoveRefused(format!("{instance_id} is present")));
        }
        if node.session_id != Some(expected_session_id) {
            return Err(AudioError::RemoveRefused(format!(
                "{instance_id}: session id {:?} != planned {expected_session_id}",
                node.session_id
            )));
        }
        if lock(&self.fail_ids).iter().any(|f| f.eq_ignore_ascii_case(instance_id)) {
            return Err(AudioError::Probe("fake uninstall failure".into()));
        }
        self.nodes.delete(instance_id);
        lock(&self.removed).push(instance_id.to_string());
        Ok(())
    }
}

/// Fixed-answer probe for higher-level tests.
#[derive(Clone, Default)]
pub struct FakeAudioProbe {
    pub endpoints: Arc<Mutex<Vec<Endpoint>>>,
    pub policy: Arc<Mutex<AudioPolicy>>,
    pub steam: Arc<Mutex<bool>>,
    pub fail: Arc<Mutex<bool>>,
}

impl AudioProbe for FakeAudioProbe {
    fn endpoints(&self) -> Result<Vec<Endpoint>, AudioError> {
        if *lock(&self.fail) {
            return Err(AudioError::Probe("fake probe failure".into()));
        }
        Ok(lock(&self.endpoints).clone())
    }
    fn policy(&self) -> Result<AudioPolicy, AudioError> {
        if *lock(&self.fail) {
            return Err(AudioError::Probe("fake probe failure".into()));
        }
        Ok(lock(&self.policy).clone())
    }
    fn steam_devices_present(&self) -> bool {
        *lock(&self.steam)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_probe_roundtrip() {
        let p = FakeAudioProbe::default();
        assert!(p.endpoints().unwrap().is_empty());
        *p.steam.lock().unwrap() = true;
        assert!(p.steam_devices_present());
        *p.fail.lock().unwrap() = true;
        assert!(p.policy().is_err());
    }

    #[test]
    fn fake_registry_case_insensitive() {
        let r = FakeRegistry::default();
        r.set(r"SOFTWARE\X", "fDisableCam", 1);
        assert_eq!(r.hklm_dword(r"software\x", "FDISABLECAM").unwrap(), Some(1));
        r.remove(r"SOFTWARE\X", "fDisableCam");
        assert_eq!(r.hklm_dword(r"software\x", "fDisableCam").unwrap(), None);
    }

    #[test]
    fn remover_refuses_foreign_ids() {
        let n = FakeDeviceNodes::default();
        n.add(r"ROOT\STEAMSTREAMINGSPEAKERS", DevNode { instance_id: r"ROOT\STEAMSTREAMINGSPEAKERS\0000".into(), ..Default::default() });
        let r = FakeRemover::new(n);
        assert!(matches!(r.remove(r"ROOT\STEAMSTREAMINGSPEAKERS\0000", 2), Err(AudioError::RemoveRefused(_))));
        for bad in [
            r"SWD\MMDEVAPI\{3.0.0.00000002}.{x}",
            r"SWD\MMDEVAPI\{3.1.0.00000002}.{6C26BA7D-F0B2-4225-B422-8168C5261E45}",
            r"SWD\MMDEVAPI\{0.0.0.00000002}.{6C26BA7D-F0B2-4225-B422-8168C5261E45}",
            r"SWD\MMDEVAPI\x\{3.0.0.00000002}.{6C26BA7D-F0B2-4225-B422-8168C5261E45}",
            "SWD\\MMDEVAPI\\{3.0.0.00000002}.{6C26BA7D-F0B2-4225-B422-8168C5261E45}\0",
        ] {
            assert!(matches!(r.remove(bad, 2), Err(AudioError::RemoveRefused(_))), "{bad:?}");
        }
        assert!(r.removed().is_empty());
    }
}
