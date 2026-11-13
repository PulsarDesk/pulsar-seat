// SPDX-License-Identifier: GPL-3.0-only
//! System-facing traits and the probe built on top of them.
//!
//! Layering: [`DeviceNodes`] (CfgMgr32) + [`RegistryRead`] (HKLM) are thin system
//! traits; [`SystemProbe`] turns them into the contract's [`AudioProbe`]. The real
//! implementations live in `crate::win`, the in-memory ones in `crate::fake`.

use serde::{Deserialize, Serialize};

use crate::AudioError;
use crate::endpoint::{
    DevNode, Endpoint, MMDEVAPI_PREFIX, endpoint_from_node, live_sessions_plausible, plan_stale_nodes,
};
use crate::policy::{AudioPolicy, DEFAULT_LISTENER, RegistryRead, read_policy};

/// Device-node enumeration (present **and** non-present nodes).
pub trait DeviceNodes: Send + Sync {
    /// Nodes below an enumerator filter such as `SWD\MMDEVAPI` or
    /// `ROOT\STEAMSTREAMINGSPEAKERS` (CM_GETIDLIST_FILTER_ENUMERATOR semantics).
    fn list(&self, enumerator: &str) -> Result<Vec<DevNode>, AudioError>;
}

/// Contract probe (contracts §6).
pub trait AudioProbe: Send + Sync {
    /// All MMDEVAPI endpoints, present and phantom, with `DEVPKEY_Device_SessionId`.
    fn endpoints(&self) -> Result<Vec<Endpoint>, AudioError>;
    /// Effective `fDisableCam` / `fDisableAudioCapture` (policy first, then listener).
    fn policy(&self) -> Result<AudioPolicy, AudioError>;
    /// Steam's streaming speaker / microphone devices are present. Report only: Pulsar
    /// Split never installs, removes or reconfigures them (spec 70 §4).
    fn steam_devices_present(&self) -> bool;
}

/// Removes one device node. Operator-triggered maintenance only.
///
/// `expected_session_id` is the `DEVPKEY_Device_SessionId` the node had when it was
/// planned. Implementations re-check right before uninstalling that the node still
/// exists, is not present, is an RDP session endpoint id, and still reads exactly that
/// session id from the property; otherwise they return `AudioError::RemoveRefused`.
/// (Signature extends contracts §6, see open question audio-6.)
pub trait EndpointRemover: Send + Sync {
    fn remove(&self, instance_id: &str, expected_session_id: u32) -> Result<(), AudioError>;
}

/// Enumerators of Steam's global streaming devices (spec 70 §4).
pub const STEAM_ENUMERATORS: &[&str] =
    &[r"ROOT\STEAMSTREAMINGSPEAKERS", r"ROOT\STEAMSTREAMINGMICROPHONE"];

/// [`AudioProbe`] over the system traits.
pub struct SystemProbe<N: DeviceNodes, R: RegistryRead> {
    nodes: N,
    reg: R,
    listener: String,
}

impl<N: DeviceNodes, R: RegistryRead> SystemProbe<N, R> {
    /// Probe for the default `RDP-Tcp` listener.
    pub fn new(nodes: N, reg: R) -> Self {
        Self::with_listener(nodes, reg, DEFAULT_LISTENER)
    }
    pub fn with_listener(nodes: N, reg: R, listener: &str) -> Self {
        Self { nodes, reg, listener: listener.to_string() }
    }
}

impl<N: DeviceNodes, R: RegistryRead> AudioProbe for SystemProbe<N, R> {
    fn endpoints(&self) -> Result<Vec<Endpoint>, AudioError> {
        let enumerator = MMDEVAPI_PREFIX.trim_end_matches('\\');
        let nodes = self.nodes.list(enumerator)?;
        Ok(nodes.iter().filter_map(endpoint_from_node).collect())
    }

    fn policy(&self) -> Result<AudioPolicy, AudioError> {
        read_policy(&self.reg, &self.listener)
    }

    fn steam_devices_present(&self) -> bool {
        STEAM_ENUMERATORS.iter().any(|e| match self.nodes.list(e) {
            Ok(v) => v.iter().any(|n| n.present),
            Err(err) => {
                log::debug!("audio: steam device query {e} failed: {err}");
                false
            }
        })
    }
}

/// Result of [`remove_stale_endpoints`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct StaleReport {
    pub removed: Vec<String>,
    /// (instance id, reason code)
    pub failed: Vec<(String, String)>,
}

fn checked_live(
    live_sessions: &dyn Fn() -> Result<Vec<u32>, AudioError>,
) -> Result<Vec<u32>, AudioError> {
    let live = live_sessions()?;
    if !live_sessions_plausible(&live) {
        return Err(AudioError::Probe("live session list is empty or lacks session 0".into()));
    }
    Ok(live)
}

/// Operator-triggered cleanup: probes, plans with [`plan_stale_nodes`] and removes each
/// planned node, logging every removal.
///
/// `live_sessions` returns the session ids that exist **now** (WTS). It is called once
/// after the probe for planning and again right before each removal; a node whose
/// session became live in between is skipped (`audio_remove_refused`). An empty list, or
/// one without session 0, is an error (WTS enumeration most likely failed). The remover
/// re-reads presence and `DEVPKEY_Device_SessionId` just before uninstalling.
///
/// A small window between the last check and `CM_Uninstall_DevNode` remains. The caller
/// (integrator) must hold its seat-start lock (contracts §9 start order) for the whole
/// call so no seat session can log on meanwhile.
pub fn remove_stale_endpoints(
    probe: &dyn AudioProbe,
    remover: &dyn EndpointRemover,
    live_sessions: &dyn Fn() -> Result<Vec<u32>, AudioError>,
) -> Result<StaleReport, AudioError> {
    let all = probe.endpoints()?;
    let live = checked_live(live_sessions)?;
    let mut report = StaleReport::default();
    for node in plan_stale_nodes(&all, &live) {
        let id = node.instance_id;
        let now = match checked_live(live_sessions) {
            Ok(v) => v,
            Err(e) => {
                log::warn!("audio: stopping stale cleanup, session list unavailable: {e}");
                report.failed.push((id, e.code().to_string()));
                break;
            }
        };
        if now.contains(&node.session_id) {
            log::info!("audio: session {} became live, keeping {id}", node.session_id);
            report.failed.push((id, AudioError::RemoveRefused(String::new()).code().to_string()));
            continue;
        }
        match remover.remove(&id, node.session_id) {
            Ok(()) => {
                log::info!("audio: removed stale session endpoint node {id}");
                report.removed.push(id);
            }
            Err(e) => {
                log::warn!("audio: could not remove {id}: {e}");
                report.failed.push((id, e.code().to_string()));
            }
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::endpoint::{Flow, tests::GUID};
    use crate::fake::{FakeDeviceNodes, FakeRegistry, FakeRemover};
    use crate::policy::{POLICY_KEY, V_DISABLE_CAM};

    fn node(sid: u32, flow: u32, present: bool) -> DevNode {
        DevNode {
            instance_id: format!(r"SWD\MMDEVAPI\{{3.0.{flow}.{sid:08X}}}.{GUID}"),
            present,
            session_id: Some(sid),
            friendly_name: Some("Remote Audio".into()),
        }
    }

    #[test]
    fn system_probe_over_fakes() {
        let nodes = FakeDeviceNodes::default();
        nodes.add("SWD\\MMDEVAPI", node(2, 0, false));
        nodes.add("SWD\\MMDEVAPI", node(3, 0, true));
        nodes.add(
            "SWD\\MMDEVAPI",
            DevNode { instance_id: r"SWD\MMDEVAPI\not-an-endpoint".into(), ..Default::default() },
        );
        let reg = FakeRegistry::default();
        reg.set(POLICY_KEY, V_DISABLE_CAM, 1);
        let p = SystemProbe::new(nodes.clone(), reg);
        let eps = p.endpoints().unwrap();
        assert_eq!(eps.len(), 2);
        assert_eq!(crate::session_endpoint(&eps, 3).unwrap().flow, Flow::Render);
        assert!(p.policy().unwrap().playback_blocked());
        assert!(!p.steam_devices_present());
        nodes.add(
            r"ROOT\STEAMSTREAMINGSPEAKERS",
            DevNode { instance_id: r"ROOT\STEAMSTREAMINGSPEAKERS\0000".into(), present: false, ..Default::default() },
        );
        assert!(!p.steam_devices_present(), "phantom steam node is not present");
        nodes.add(
            r"ROOT\STEAMSTREAMINGMICROPHONE",
            DevNode { instance_id: r"ROOT\STEAMSTREAMINGMICROPHONE\0000".into(), present: true, ..Default::default() },
        );
        assert!(p.steam_devices_present());
        nodes.fail(true);
        assert!(matches!(p.endpoints(), Err(AudioError::Probe(_))));
        assert!(!p.steam_devices_present());
    }

    #[test]
    fn stale_removal_flow() {
        let nodes = FakeDeviceNodes::default();
        for n in [node(2, 0, false), node(3, 0, true), node(4, 0, false), node(5, 1, false)] {
            nodes.add("SWD\\MMDEVAPI", n);
        }
        let p = SystemProbe::new(nodes.clone(), FakeRegistry::default());
        let remover = FakeRemover::new(nodes.clone());
        remover.fail_on(&node(5, 1, false).instance_id);
        let live = || Ok(vec![0, 1, 3, 4]);
        let r = remove_stale_endpoints(&p, &remover, &live).unwrap();
        assert_eq!(r.removed, vec![node(2, 0, false).instance_id]);
        assert_eq!(r.failed, vec![(node(5, 1, false).instance_id, "audio_probe".to_string())]);
        assert_eq!(remover.removed(), vec![node(2, 0, false).instance_id]);
        // Present node still there; removed one gone.
        let eps = p.endpoints().unwrap();
        assert!(eps.iter().any(|e| e.session_id == Some(3) && e.present));
        assert!(!eps.iter().any(|e| e.session_id == Some(2)));
        // Fake remover refuses present nodes like the real one.
        assert!(matches!(
            remover.remove(&node(3, 0, true).instance_id, 3),
            Err(AudioError::RemoveRefused(_))
        ));
    }

    #[test]
    fn stale_removal_refuses_bad_session_lists() {
        let nodes = FakeDeviceNodes::default();
        for n in [node(2, 0, false), node(3, 0, true)] {
            nodes.add("SWD\\MMDEVAPI", n);
        }
        let p = SystemProbe::new(nodes.clone(), FakeRegistry::default());
        let remover = FakeRemover::new(nodes.clone());
        for bad in [vec![], vec![1, 3]] {
            let f = move || Ok(bad.clone());
            assert!(matches!(remove_stale_endpoints(&p, &remover, &f), Err(AudioError::Probe(_))));
        }
        let fail = || Err(AudioError::Probe("wts".into()));
        assert!(remove_stale_endpoints(&p, &remover, &fail).is_err());
        assert!(remover.removed().is_empty());
    }

    #[test]
    fn stale_removal_rechecks_liveness_before_each_node() {
        let nodes = FakeDeviceNodes::default();
        for n in [node(2, 0, false), node(3, 0, true)] {
            nodes.add("SWD\\MMDEVAPI", n);
        }
        let p = SystemProbe::new(nodes.clone(), FakeRegistry::default());
        let remover = FakeRemover::new(nodes.clone());
        // First call (planning): session 2 is gone. Second call (before removal): a seat
        // is logging on and got session id 2 again.
        let calls = std::sync::atomic::AtomicU32::new(0);
        let live = || {
            let n = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(if n == 0 { vec![0, 1, 3] } else { vec![0, 1, 2, 3] })
        };
        let r = remove_stale_endpoints(&p, &remover, &live).unwrap();
        assert!(r.removed.is_empty());
        assert_eq!(r.failed, vec![(node(2, 0, false).instance_id, "audio_remove_refused".to_string())]);
        assert!(remover.removed().is_empty());
    }

    #[test]
    fn remover_rechecks_session_property() {
        let nodes = FakeDeviceNodes::default();
        let mut no_prop = node(7, 0, false);
        no_prop.session_id = None;
        nodes.add("SWD\\MMDEVAPI", node(2, 0, false));
        nodes.add("SWD\\MMDEVAPI", no_prop.clone());
        let remover = FakeRemover::new(nodes.clone());
        let id2 = node(2, 0, false).instance_id;
        assert!(matches!(remover.remove(&id2, 5), Err(AudioError::RemoveRefused(_))), "sid changed");
        assert!(
            matches!(remover.remove(&no_prop.instance_id, 7), Err(AudioError::RemoveRefused(_))),
            "no property"
        );
        assert!(remover.remove(&id2, 2).is_ok());
    }

    #[test]
    fn node_without_property_is_never_planned() {
        let nodes = FakeDeviceNodes::default();
        let mut no_prop = node(7, 0, false);
        no_prop.session_id = None; // e.g. the property read failed
        for n in [no_prop, node(3, 0, true)] {
            nodes.add("SWD\\MMDEVAPI", n);
        }
        let p = SystemProbe::new(nodes.clone(), FakeRegistry::default());
        let remover = FakeRemover::new(nodes);
        let live = || Ok(vec![0, 3]);
        let r = remove_stale_endpoints(&p, &remover, &live).unwrap();
        assert_eq!(r, StaleReport::default());
    }
}
