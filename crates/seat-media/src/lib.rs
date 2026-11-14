// SPDX-License-Identifier: GPL-3.0-only
//! Pulsar Seat media and devices — implements [`seat_core::backend::AudioRouter`] and
//! [`seat_core::backend::DeviceGate`], plus the Windows [`seat_core::pnp::PnpTree`].
//!
//! Owner: the **media** package. Spec: `spec/local/60-audio.md` §8, `spec/local/70-usb-devices.md` §5.
//! Contract: `docs/agents/architecture.md` (§ Audio, § Devices).
//!
//! Public API:
//!
//! - [`pnp::WinPnpTree::new`] (Windows): CfgMgr32 tree (parent chain, location paths,
//!   container id, hardware / compatible ids, filters, interface → devnode). The service
//!   creates one and shares it as `Arc<dyn PnpTree>` with every backend.
//! - [`create_audio`]: MMDevice enumeration, endpoint keys (spec 60 §3.2 composite +
//!   tie-breaker, [`keys`]), `IMMNotificationClient` hot-plug, WASAPI test tone on one
//!   endpoint id. [`audio::AudioRouterImpl::rebind_all`] re-resolves bound seats after a
//!   hot-plug (the service then sends `HostCommand::RebindAudio`).
//! - [`bridge::AudioBridge`] — seat-host side: back-end of the RDP client's audio playback /
//!   capture channels that opens the seat's endpoints by id (WASAPI shared mode) and
//!   re-binds on `HostCommand::RebindAudio` within about a second.
//! - [`create_gate`] / [`create_gates`]:
//!   - `DeviceSecurity` ([`gate::security`]): SetupAPI security property + secure-open
//!     characteristic on disk **and** volume devnodes, logon SIDs of the owning seats,
//!     volume lock + dismount / devnode restart to revoke handles (veto reported);
//!   - `SessionDriveLetters` ([`gate::letters`]): remove the global letter of a one-seat
//!     volume, journal the remembered letter, hand [`gate::letters::LetterTask`]s
//!     (`AgentTaskKind::DefineDriveLetter` / `RemoveDriveLetter`) to the owner's agent;
//!   - `CameraConsent` ([`gate::camera`]): per-user camera consent switch (coarse);
//!   - `StorageMinifilter` ([`gate::minifilter`]): client of `drivers/seat-storage-filter`
//!     (needs the maintainer's OK; `DriverNotSigned` until signed);
//!   - `PrinterAcl` ([`gate::printer`]): print-queue DACL per seat (optional, spec 70 §5.2(5));
//!   - class-level default descriptors ([`gate::classdefault`], run by `DeviceSecurity` when
//!     `GatePlan::class_defaults` is set; spec 70 §5.2(2)).
//! - [`kiosk_audio`] + [`create_app_endpoint_policy`]: per-app default endpoint of kiosk
//!   seat apps (trait `AppEndpointPolicy`, planner `KioskAudio`, fake; Windows: the
//!   undocumented `AudioPolicyConfig` factory). The only place that sets an endpoint
//!   default, and only per app process, never the system default.
//! - [`play_tone_on_endpoint`]: the WASAPI test tone itself, for the console helper
//!   (`pulsar-seat-service --audio-helper`), so the tone plays from the console session.
//! - [`redirect::seat_cameras`]: cameras a seat may offer over RDP camera redirection
//!   (clipboard / drive / smart-card rules live in `seat_rdp::policy`).
//!
//! Never modifies the Windows Audio service; never writes the MMDevices endpoint store.
//! Every system change is journaled (`%ProgramData%\Pulsar Seat\run\gate-*.json`) and
//! reverted by `revert_all`.

pub mod audio;
pub mod bridge;
pub mod gate;
pub mod inventory;
pub mod journal;
pub mod keys;
pub mod kiosk_audio;
pub mod redirect;

#[cfg(windows)]
mod win;

/// Version of the loaded storage minifilter (service driver status; Windows only).
#[cfg(windows)]
pub use win::fltport::probe_minifilter_version;

/// Windows device tree (`seat_media::pnp::WinPnpTree`).
pub mod pnp {
    #[cfg(windows)]
    pub use crate::win::pnp::{WinPnpTree, removal_relations};
}

use gate::ChangeSource;
use gate::camera::CameraConsentGate;
use gate::letters::SessionDriveLetterGate;
use gate::minifilter::MinifilterGate;
use gate::printer::PrinterAclGate;
use gate::security::DeviceSecurityGate;
use journal::{JournalLocation, default_location};
use seat_core::backend::{AudioBackendKind, AudioRouter, Availability, DeviceGate, GateBackendKind};
use seat_core::pnp::PnpTree;
use std::path::Path;
use std::sync::Arc;

/// Play the test tone on one endpoint id from **this** process (WASAPI shared mode). The
/// service calls it in the console helper it starts in the console session (spec 60: the
/// tone must come from the console session, not session 0).
pub fn play_tone_on_endpoint(endpoint_id: &str, duration_ms: u32) -> seat_core::backend::BackendResult<()> {
    #[cfg(windows)]
    {
        use audio::EndpointSource;
        win::mmdevice::MmDeviceSource::new().play_tone(endpoint_id, duration_ms.clamp(100, 3000))
    }
    #[cfg(not(windows))]
    {
        let _ = (endpoint_id, duration_ms);
        Err(seat_core::backend::BackendError::unsupported("this platform"))
    }
}

/// The per-app default endpoint policy for kiosk seats (Windows: the undocumented
/// `AudioPolicyConfig` factory, [`kiosk_audio`]; elsewhere always unavailable).
pub fn create_app_endpoint_policy() -> Arc<dyn kiosk_audio::AppEndpointPolicy> {
    #[cfg(windows)]
    return Arc::new(win::policyconfig::WinAppEndpointPolicy::new());
    #[cfg(not(windows))]
    return Arc::new(kiosk_audio::NoPolicy("os_unsupported"));
}

/// The audio router for `kind` (Windows: MMDevice / WASAPI source).
pub fn create_audio(kind: AudioBackendKind, pnp: Arc<dyn PnpTree>) -> Arc<audio::AudioRouterImpl> {
    #[cfg(windows)]
    let source: Arc<dyn audio::EndpointSource> = match kind {
        AudioBackendKind::None => Arc::new(audio::NoEndpoints),
        _ => Arc::new(win::mmdevice::MmDeviceSource::new()),
    };
    #[cfg(not(windows))]
    let source: Arc<dyn audio::EndpointSource> = Arc::new(audio::NoEndpoints);
    Arc::new(audio::AudioRouterImpl::new(kind, pnp, source))
}

/// Same as [`create_audio`], as a trait object.
pub fn create_audio_router(kind: AudioBackendKind, pnp: Arc<dyn PnpTree>) -> Arc<dyn AudioRouter> {
    create_audio(kind, pnp)
}

/// The gates the service runs, with typed handles for the ones it talks to directly.
#[derive(Default)]
pub struct Gates {
    pub all: Vec<Arc<dyn DeviceGate>>,
    /// Present when `SessionDriveLetters` is selected: drain its agent tasks after `apply`.
    pub letters: Option<Arc<SessionDriveLetterGate>>,
}

struct Sys {
    #[cfg(windows)]
    io: Arc<win::setupdi::WinSystemIo>,
}

impl Sys {
    fn new() -> Self {
        Self {
            #[cfg(windows)]
            io: Arc::new(win::setupdi::WinSystemIo),
        }
    }
    fn changes(&self) -> Arc<dyn ChangeSource> {
        #[cfg(windows)]
        return Arc::new(win::notify::CmChanges);
        #[cfg(not(windows))]
        return Arc::new(gate::NoChanges);
    }
    fn security(&self) -> Arc<dyn gate::SecurityIo> {
        #[cfg(windows)]
        return self.io.clone();
        #[cfg(not(windows))]
        return Arc::new(stub::Stub);
    }
    fn volumes(&self) -> Arc<dyn gate::VolumeIo> {
        #[cfg(windows)]
        return self.io.clone();
        #[cfg(not(windows))]
        return Arc::new(stub::Stub);
    }
    fn consent(&self) -> Arc<dyn gate::ConsentIo> {
        #[cfg(windows)]
        return Arc::new(win::consent::WinConsent);
        #[cfg(not(windows))]
        return Arc::new(stub::Stub);
    }
    fn port(&self) -> Arc<dyn gate::minifilter::FilterPort> {
        #[cfg(windows)]
        return Arc::new(win::fltport::WinFilterPort);
        #[cfg(not(windows))]
        return Arc::new(gate::minifilter::NoPort);
    }
    fn printers(&self) -> Arc<dyn gate::printer::PrinterIo> {
        #[cfg(windows)]
        return Arc::new(win::spooler::WinPrinterIo);
        #[cfg(not(windows))]
        return Arc::new(stub::Stub);
    }
    fn availability(&self) -> Availability {
        if cfg!(windows) { Availability::Available } else { Availability::Unavailable { reason: "os_unsupported".into() } }
    }
}

fn journal_at(dir: Option<&Path>, file: &str) -> JournalLocation {
    match dir {
        Some(d) => JournalLocation::File(d.join(file)),
        None => default_location(file),
    }
}

/// One gate, journals under `%ProgramData%\Pulsar Seat\run\`.
pub fn create_gate(kind: GateBackendKind, pnp: Arc<dyn PnpTree>) -> Arc<dyn DeviceGate> {
    create_gate_in(kind, pnp, None)
}

/// One gate with its journal under `run_dir` (`None` = the default data dir).
pub fn create_gate_in(kind: GateBackendKind, pnp: Arc<dyn PnpTree>, run_dir: Option<&Path>) -> Arc<dyn DeviceGate> {
    let sys = Sys::new();
    match kind {
        GateBackendKind::DeviceSecurity => Arc::new(
            DeviceSecurityGate::new(pnp, sys.security(), sys.volumes(), sys.changes(), journal_at(run_dir, "gate-device-security.json"))
                .with_availability(sys.availability()),
        ),
        GateBackendKind::SessionDriveLetters => Arc::new(letters_gate(&sys, pnp, run_dir)),
        GateBackendKind::CameraConsent => Arc::new(
            CameraConsentGate::new(pnp, sys.consent(), sys.changes(), journal_at(run_dir, "gate-camera-consent.json"))
                .with_availability(sys.availability()),
        ),
        GateBackendKind::StorageMinifilter => Arc::new(MinifilterGate::new(pnp, sys.port(), sys.volumes(), sys.changes())),
        GateBackendKind::PrinterAcl => Arc::new(
            PrinterAclGate::new(pnp, sys.printers(), sys.changes(), journal_at(run_dir, "gate-printer-acl.json"))
                .with_availability(sys.availability()),
        ),
    }
}

/// The session drive-letter gate as its concrete type, so the service can drain its agent
/// work ([`SessionDriveLetterGate::take_agent_tasks`]).
pub fn create_letters_gate(pnp: Arc<dyn PnpTree>, run_dir: Option<&Path>) -> Arc<SessionDriveLetterGate> {
    Arc::new(letters_gate(&Sys::new(), pnp, run_dir))
}

fn letters_gate(sys: &Sys, pnp: Arc<dyn PnpTree>, run_dir: Option<&Path>) -> SessionDriveLetterGate {
    SessionDriveLetterGate::new(pnp, sys.volumes(), sys.changes(), journal_at(run_dir, "gate-drive-letters.json"))
        .with_availability(sys.availability())
}

/// Every selected gate (`settings.backends.gates`), duplicates ignored.
pub fn create_gates(kinds: &[GateBackendKind], pnp: Arc<dyn PnpTree>, run_dir: Option<&Path>) -> Gates {
    let sys = Sys::new();
    let mut g = Gates::default();
    let mut done = Vec::new();
    for k in kinds {
        if done.contains(k) {
            continue;
        }
        done.push(*k);
        if *k == GateBackendKind::SessionDriveLetters {
            let l = Arc::new(letters_gate(&sys, pnp.clone(), run_dir));
            g.letters = Some(l.clone());
            g.all.push(l);
        } else {
            g.all.push(create_gate_in(*k, pnp.clone(), run_dir));
        }
    }
    g
}

#[cfg(not(windows))]
mod stub {
    use crate::gate::printer::PrinterIo;
    use crate::gate::{ConsentIo, Invalidate, SecurityIo, VolumeIo, VolumeRef};
    use seat_core::backend::{BackendError, BackendResult};
    pub struct Stub;
    fn no<T>() -> BackendResult<T> {
        Err(BackendError::unsupported("this platform"))
    }
    impl SecurityIo for Stub {
        fn get_sddl(&self, _: &str) -> BackendResult<Option<String>> {
            no()
        }
        fn set_sddl(&self, _: &str, _: Option<&str>) -> BackendResult<()> {
            no()
        }
        fn get_characteristics(&self, _: &str) -> BackendResult<Option<u32>> {
            no()
        }
        fn set_characteristics(&self, _: &str, _: Option<u32>) -> BackendResult<()> {
            no()
        }
        fn restart(&self, _: &str) -> BackendResult<Invalidate> {
            no()
        }
    }
    impl VolumeIo for Stub {
        fn volumes_of(&self, _: &str) -> BackendResult<Vec<VolumeRef>> {
            Ok(Vec::new())
        }
        fn dismount(&self, _: &str) -> BackendResult<Invalidate> {
            no()
        }
        fn remove_global_letter(&self, _: char) -> BackendResult<()> {
            no()
        }
        fn add_global_letter(&self, _: char, _: &str) -> BackendResult<()> {
            no()
        }
        fn letter_in_use(&self, _: char) -> bool {
            false
        }
    }
    impl PrinterIo for Stub {
        fn queues_for_device(&self, _: &str) -> BackendResult<Vec<String>> {
            Ok(Vec::new())
        }
        fn get_dacl(&self, _: &str) -> BackendResult<Option<String>> {
            no()
        }
        fn set_dacl(&self, _: &str, _: &str) -> BackendResult<()> {
            no()
        }
    }
    impl ConsentIo for Stub {
        fn get(&self, _: &str) -> BackendResult<Option<String>> {
            no()
        }
        fn set(&self, _: &str, _: Option<&str>) -> BackendResult<()> {
            no()
        }
        fn hive_loaded(&self, _: &str) -> bool {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use seat_core::mock::MockPnpTree;

    #[test]
    fn factories() {
        let pnp: Arc<dyn PnpTree> = Arc::new(MockPnpTree::default());
        let dir = std::env::temp_dir().join(format!("seat-media-factories-{}", std::process::id()));
        let g = create_gates(
            &[GateBackendKind::DeviceSecurity, GateBackendKind::SessionDriveLetters, GateBackendKind::DeviceSecurity, GateBackendKind::StorageMinifilter],
            pnp.clone(),
            Some(&dir),
        );
        assert_eq!(g.all.len(), 3);
        assert!(g.letters.is_some());
        assert_eq!(g.all[2].kind(), GateBackendKind::StorageMinifilter);
        // The minifilter is never installed on a dev box.
        assert!(!g.all[2].availability().is_available());
        let a = create_audio_router(AudioBackendKind::None, pnp);
        assert_eq!(a.kind(), AudioBackendKind::None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
