// SPDX-License-Identifier: GPL-3.0-only
//! Pulsar Split per-seat audio (spec 70, contracts §6).
//!
//! No audio driver: every seat is an RDP session, and Windows gives it a session-scoped
//! *Remote Audio* render endpoint. This crate
//! - finds that endpoint (CfgMgr32 + `DEVPKEY_Device_SessionId`) and plans optional,
//!   operator-triggered cleanup of phantom nodes,
//! - reads (never writes) the Terminal Services audio policy,
//! - produces the stream-server audio keys and the headless RDP client's audio flags,
//! - computes the seat audio state and the documented limitations shown in the manager.
//!
//! Steam's streaming audio drivers are only detected, never installed or removed.

mod config;
mod endpoint;
mod error;
mod policy;
mod probe;
mod state;

#[cfg(windows)]
mod win;

#[cfg(any(test, feature = "fake"))]
pub mod fake;

pub use config::{
    INFO_AUDIOCAPTURE, INFO_NOAUDIOPLAYBACK, INFO_REMOTECONSOLEAUDIO, K_AUDIO_SINK,
    K_INSTALL_STEAM_AUDIO_DRIVERS, K_STREAM_AUDIO, K_VIRTUAL_SINK, RdpAudioFlags, SeatAudio,
    rdp_client_flags, server_keys,
};
pub use endpoint::{
    DevNode, Endpoint, EndpointIdParts, Flow, MMDEVAPI_PREFIX, RDP_ENDPOINT_KIND,
    RDP_ENDPOINT_SUB, StaleNode, endpoint_from_node, endpoint_guid_suffix, is_braced_guid,
    live_sessions_plausible, mmdevapi_endpoint_id, parse_endpoint_id, plan_stale_endpoints,
    plan_stale_nodes, rdp_guid_suffixes, session_capture_endpoint, session_endpoint,
};
pub use error::AudioError;
pub use policy::{
    AudioPolicy, DEFAULT_LISTENER, POLICY_KEY, PolicySource, RegistryRead, WINSTATIONS_KEY,
    read_policy, resolve_policy,
};
pub use probe::{
    AudioProbe, DeviceNodes, EndpointRemover, STEAM_ENUMERATORS, StaleReport, SystemProbe,
    remove_stale_endpoints,
};
pub use state::{
    AudioLimitation, AudioState, LogScanner, MICROPHONE_PATH_AVAILABLE, SeatAudioStatus,
    limitations, scan_log_line, seat_audio_state, seat_audio_status,
};

#[cfg(windows)]
pub use win::{CfgMgrNodes, CfgMgrRemover, WinAudioProbe, WinRegistry, system_probe};

/// Guard shared by every remover: only `SWD\MMDEVAPI\{3.0.f.hhhhhhhh}.{guid}` ids (RDP
/// session endpoints, well-formed hex GUID, no NUL / control character, no backslash
/// after the prefix) may be removed. The live GUID-suffix match is done by the planner
/// ([`plan_stale_nodes`]); the remover additionally re-reads the node's session id.
pub(crate) fn check_removable_id(instance_id: &str) -> Result<(), AudioError> {
    let refused = || AudioError::RemoveRefused(format!("{instance_id:?} is not an RDP session endpoint"));
    let eid = endpoint::mmdevapi_endpoint_id(instance_id).ok_or_else(refused)?;
    match parse_endpoint_id(eid) {
        Some(p) if p.is_rdp() => Ok(()),
        _ => Err(refused()),
    }
}
