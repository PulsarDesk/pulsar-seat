// SPDX-License-Identifier: GPL-3.0-only
//! Production wiring of the package backends into the service.
//!
//! [`PackageFactory`] is the one place that maps a kind to a package backend:
//!
//! | Kind | Constructor |
//! | --- | --- |
//! | session | `seat_session::create_backend(kind, SessionDeps::system(data_dir))` |
//! | display | `seat_display::create_binding_with_runner(kind, pnp, runner)`, runner = `win::console_runner` |
//! | input | `seat_input::create(kind, pnp, &settings.input, topology)`: `.router` + `.control`; `winusb_capture` through `create_with_capture` with the factory's one `UsbCapture` |
//! | audio | `seat_media::create_audio(kind, pnp)` (+ `set_endpoint_masks` from `apply_ownership`; in session 0 test tones go through `win::console_runner::play_tone_in_console`) |
//! | gates | `seat_media::create_gate_in(kind, pnp, Some(data_dir\run))`; drive letters: `create_letters_gate` (its agent tasks go to the seat agents) |
//! | PnP tree | `seat_media::pnp::WinPnpTree` |
//! | importer | `seat_compat::importer::import_reference` |
//!
//! Backends are built once per kind and cached: `get_backends` reports every kind and
//! `reselect_backends` asks again after a settings change, and a backend may register
//! device notifications when it is built. The input router depends on the input settings
//! and the host topology, so its cache is dropped when those change ([`BackendFactory::configure`]).
//!
//! Untested on hardware: every package backend's system calls (see each package).

use crate::backends::BackendFactory;
use seat_core::backend::audio::{AudioBackendKind, AudioRouter};
use seat_core::backend::display::{DisplayBackendKind, DisplayBinding};
use seat_core::backend::gate::{DeviceGate, GateBackendKind};
use seat_core::backend::input::{InputBackendKind, InputRouter};
use seat_core::backend::session::{HostTopology, SessionBackend, SessionBackendKind};
use seat_core::config::{Config, GlobalSettings, InputSettings};
use seat_core::device::Device;
use seat_core::pnp::PnpTree;
use seat_proto::{ErrorCode, ProtoError};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Default)]
struct Cache {
    /// `HostTopology` is not `Hash`: a short list.
    session: Vec<((SessionBackendKind, HostTopology), Arc<dyn SessionBackend>)>,
    display: HashMap<DisplayBackendKind, Arc<dyn DisplayBinding>>,
    input: HashMap<InputBackendKind, seat_input::InputBackend>,
    audio: HashMap<AudioBackendKind, Arc<seat_media::audio::AudioRouterImpl>>,
    gate: HashMap<GateBackendKind, Arc<dyn DeviceGate>>,
    /// The same object as `gate[SessionDriveLetters]`, for its agent tasks.
    letters: Option<Arc<seat_media::gate::letters::SessionDriveLetterGate>>,
    input_settings: Option<(InputSettings, HostTopology)>,
}

/// Maps kinds to package backends (see the module doc).
pub struct PackageFactory {
    pub pnp: Arc<dyn PnpTree>,
    /// `%ProgramData%\Pulsar Seat` (journals of the session and gate packages).
    pub data_dir: PathBuf,
    cache: Mutex<Cache>,
    /// `winusb_capture` state, shared by every router instance ([`Self::usb_capture`]).
    usb: Mutex<Option<Arc<seat_input::usb_capture::UsbCapture>>>,
    /// Per-seat users of kiosk shells (desktops backend; `None` in console dev mode, where
    /// no account is created).
    kiosk_logons: Option<Arc<dyn seat_session::kiosk_account::SeatLogons>>,
}

impl PackageFactory {
    pub fn new(pnp: Arc<dyn PnpTree>, data_dir: PathBuf) -> Self {
        Self { pnp, data_dir, cache: Mutex::default(), usb: Mutex::default(), kiosk_logons: None }
    }

    /// With per-seat kiosk users (the service, not the console dev mode).
    pub fn with_kiosk_logons(mut self, logons: Option<Arc<dyn seat_session::kiosk_account::SeatLogons>>) -> Self {
        self.kiosk_logons = logons;
        self
    }

    /// Production factory: the media package's PnP tree.
    #[cfg(windows)]
    pub fn system(data_dir: PathBuf) -> Self {
        Self::new(Arc::new(seat_media::pnp::WinPnpTree::new()), data_dir)
    }

    fn audio_router(&self, kind: AudioBackendKind) -> Arc<seat_media::audio::AudioRouterImpl> {
        if let Some(b) = lock(&self.cache).audio.get(&kind) {
            return b.clone();
        }
        let b = seat_media::create_audio(kind, self.pnp.clone());
        // The real service lives in session 0: play test tones from the console session
        // through the audio helper (spec 60), not here.
        #[cfg(windows)]
        if crate::win::console_runner::in_session_zero() {
            b.set_tone_player(Some(Arc::new(crate::win::console_runner::play_tone_in_console)));
        }
        lock(&self.cache).audio.insert(kind, b.clone());
        b
    }

    /// Router and its controls (one object), built once per kind and settings.
    fn input_backend(&self, kind: InputBackendKind) -> seat_input::InputBackend {
        if let Some(b) = lock(&self.cache).input.get(&kind) {
            return b.clone();
        }
        let (settings, topology) = self.input_config();
        let b = if kind == InputBackendKind::WinusbCapture {
            seat_input::create_with_capture(kind, self.pnp.clone(), &settings, topology, self.usb_capture())
        } else {
            seat_input::create(kind, self.pnp.clone(), &settings, topology)
        };
        lock(&self.cache).input.insert(kind, b.clone());
        b
    }

    /// The one USB capture state of this service (`winusb_capture`, osms G2): built on
    /// first use over `<data>\journal\usb-capture.json`; what an earlier run journaled is
    /// pending in it from the start (restored by [`BackendFactory::restore_usb_input`] at
    /// boot, or by the next sync). Router rebuilds share it, so captures survive them.
    pub fn usb_capture(&self) -> Arc<seat_input::usb_capture::UsbCapture> {
        let mut g = lock(&self.usb);
        if let Some(c) = g.as_ref() {
            return c.clone();
        }
        let c = seat_input::usb_capture::UsbCapture::system(&self.data_dir.join(seat_core::consts::JOURNAL_DIR));
        c.watch();
        *g = Some(c.clone());
        c
    }

    fn input_config(&self) -> (InputSettings, HostTopology) {
        lock(&self.cache).input_settings.clone().unwrap_or_else(|| (InputSettings::default(), HostTopology::default()))
    }
}

impl BackendFactory for PackageFactory {
    fn configure(&self, settings: &GlobalSettings) {
        let next = (settings.input.clone(), settings.backends.host_topology);
        let mut c = lock(&self.cache);
        if c.input_settings.as_ref() != Some(&next) {
            c.input.clear();
            c.input_settings = Some(next);
        }
    }

    fn session(&self, kind: SessionBackendKind, topology: HostTopology) -> Option<Arc<dyn SessionBackend>> {
        if let Some((_, b)) = lock(&self.cache).session.iter().find(|(k, _)| *k == (kind, topology)) {
            return Some(b.clone());
        }
        let b = build_session(kind, &self.data_dir, self.kiosk_logons.clone())?;
        lock(&self.cache).session.push(((kind, topology), b.clone()));
        Some(b)
    }

    fn display(&self, kind: DisplayBackendKind) -> Option<Arc<dyn DisplayBinding>> {
        if let Some(b) = lock(&self.cache).display.get(&kind) {
            return Some(b.clone());
        }
        let b = build_display(kind, self.pnp.clone())?;
        lock(&self.cache).display.insert(kind, b.clone());
        Some(b)
    }

    fn input(&self, kind: InputBackendKind) -> Option<Arc<dyn InputRouter>> {
        Some(self.input_backend(kind).router)
    }

    fn input_control(&self, kind: InputBackendKind) -> Option<Arc<dyn seat_input::InputControl>> {
        Some(self.input_backend(kind).control)
    }

    /// raw_input_host sees no input in session 0: the console listener reports it (also
    /// under winusb_capture, for every unit that is not captured).
    fn console_listener(&self, kind: InputBackendKind) -> bool {
        matches!(kind, InputBackendKind::RawInputHost | InputBackendKind::WinusbCapture)
    }

    fn restore_usb_input(&self) -> Vec<String> {
        let c = self.usb_capture();
        if !c.needs_restore() {
            return Vec::new();
        }
        c.restore_all()
    }

    fn hold_usb_capture(&self) -> bool {
        self.usb_capture().hold();
        true
    }

    /// The cached `winusb_capture` router is not rebuilt when the backend is chosen again
    /// (one router per kind and settings), so its constructor's resume never ran (gaming
    /// PC E2E round 2, bug J).
    fn resume_usb_capture(&self) {
        if let Some(c) = lock(&self.usb).clone() {
            c.resume();
        }
    }

    fn release_usb_input(&self) -> Vec<String> {
        let c = lock(&self.usb).clone();
        c.map(|c| c.release_all()).unwrap_or_default()
    }

    fn audio(&self, kind: AudioBackendKind) -> Option<Arc<dyn AudioRouter>> {
        Some(self.audio_router(kind))
    }

    fn set_audio_masks(&self, kind: AudioBackendKind, masks: Vec<(seat_core::DeviceKey, seat_core::SeatMask)>) {
        self.audio_router(kind).set_endpoint_masks(masks);
    }

    fn gate(&self, kind: GateBackendKind) -> Option<Arc<dyn DeviceGate>> {
        if let Some(b) = lock(&self.cache).gate.get(&kind) {
            return Some(b.clone());
        }
        let run = self.data_dir.join(seat_core::consts::RUN_DIR);
        let b: Arc<dyn DeviceGate> = if kind == GateBackendKind::SessionDriveLetters {
            let l = seat_media::create_letters_gate(self.pnp.clone(), Some(&run));
            lock(&self.cache).letters = Some(l.clone());
            l
        } else {
            seat_media::create_gate_in(kind, self.pnp.clone(), Some(&run))
        };
        lock(&self.cache).gate.insert(kind, b.clone());
        Some(b)
    }

    fn take_gate_agent_tasks(&self) -> Vec<(u32, seat_proto::AgentTaskKind)> {
        use seat_media::gate::letters::LetterAction;
        let Some(l) = lock(&self.cache).letters.clone() else { return Vec::new() };
        l.take_agent_tasks()
            .into_iter()
            .map(|t| {
                let task = match t.action {
                    LetterAction::Define { letter, volume } => seat_proto::AgentTaskKind::DefineDriveLetter { letter, volume },
                    LetterAction::Remove { letter } => seat_proto::AgentTaskKind::RemoveDriveLetter { letter },
                };
                (t.session_id, task)
            })
            .collect()
    }
}

#[cfg(windows)]
fn build_session(
    kind: SessionBackendKind,
    data_dir: &std::path::Path,
    logons: Option<Arc<dyn seat_session::kiosk_account::SeatLogons>>,
) -> Option<Arc<dyn SessionBackend>> {
    let opts = seat_session::DesktopsOptions { logons, ..seat_session::DesktopsOptions::platform() };
    Some(seat_session::create_backend_with(kind, seat_session::SessionDeps::system(data_dir), opts))
}

#[cfg(not(windows))]
fn build_session(
    _kind: SessionBackendKind,
    _data_dir: &std::path::Path,
    _logons: Option<Arc<dyn seat_session::kiosk_account::SeatLogons>>,
) -> Option<Arc<dyn SessionBackend>> {
    None
}

#[cfg(windows)]
fn build_display(kind: DisplayBackendKind, pnp: Arc<dyn PnpTree>) -> Option<Arc<dyn DisplayBinding>> {
    let runner = Arc::new(crate::win::console_runner::SessionConsoleRunner::new());
    Some(seat_display::create_binding_with_runner(kind, pnp, runner))
}

#[cfg(not(windows))]
fn build_display(_kind: DisplayBackendKind, _pnp: Arc<dyn PnpTree>) -> Option<Arc<dyn DisplayBinding>> {
    None
}

/// Reference-config importer (spec 10 §3.8), owned by `seat-compat`: maps the export onto
/// `base` with the live `devices`. Unmatched records are logged, not fatal.
pub fn import_reference(json: &str, base: &Config, devices: &[Device]) -> Result<Config, ProtoError> {
    match seat_compat::importer::import_reference(json, base, devices) {
        Ok(r) => {
            if !r.unmatched.is_empty() {
                crate::log::info(&format!("import: {} matched, {} unmatched records", r.matched.len(), r.unmatched.len()));
            }
            Ok(r.config)
        }
        Err(e) => Err(ProtoError::new(ErrorCode::InvalidValue, e.to_string()).with_field("json")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use seat_core::config::BackendSelection;

    /// The factory resolves the default kinds (and the input cache follows the settings).
    #[cfg(windows)]
    #[test]
    fn default_kinds_resolve() {
        let dir = std::env::temp_dir().join(format!("seat-service-wiring-{}", std::process::id()));
        let f = PackageFactory::new(Arc::new(crate::win::pnp::CfgTree::load()), dir.clone());
        let mut g = GlobalSettings::default();
        f.configure(&g);
        let sel = BackendSelection::default();
        assert!(f.session(sel.session, sel.host_topology).is_some());
        assert!(f.display(sel.display).is_some());
        let i1 = f.input(sel.input).unwrap();
        assert!(f.audio(sel.audio).is_some());
        for k in &sel.gates {
            assert!(f.gate(*k).is_some());
        }
        // Cached: the same instance while the settings stay.
        assert!(Arc::ptr_eq(&i1, &f.input(sel.input).unwrap()));
        assert!(f.input_control(sel.input).is_some());
        g.backends.host_topology = HostTopology::DedicatedHost;
        f.configure(&g);
        assert!(!Arc::ptr_eq(&i1, &f.input(sel.input).unwrap()));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn importer_errors_are_invalid_value() {
        let e = import_reference("{}", &Config::default(), &[]).unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidValue);
    }
}
