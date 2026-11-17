// SPDX-License-Identifier: GPL-3.0-only
//! Sunshine backend: the existing spec 80 path behind [`StreamBackend`] / [`SeatStream`],
//! unchanged in behaviour. `server.conf` from [`crate::conf::generate`], the rule pair of
//! [`crate::firewall::apply_seat_rules`], [`SeatServer`] for launch, watchdog and control.

use super::{Prepared, SeatStream, SeatStreamSpec, StreamBackend};
use crate::apps::{render_apps_json, AppEntry};
use crate::conf::{generate, Capture, ConfInputs, Gamepad, PAIR_TIMEOUT_DEFAULT_S};
use crate::control::upstream::Random;
use crate::control::{PairedClient, SeatControl};
use crate::dist::{ServerDist, ServerFlavour};
use crate::firewall::{apply_seat_rules, remove_seat_rules, Firewall};
use crate::journal::RestoreLog;
use crate::ports::ListenerProbe;
use crate::seatdir::{SecureFs, SeatDir};
use crate::supervise::{caps, AppsReload, ConfForCaps, ControlConnector, SeatServer, SeatServerConfig, ServerLauncher, StreamNotice};
use crate::{Clock, StreamError};
use seat_proto::remote::{
    CapturePref, GamepadPref, RemoteSeatRecord, RemoteSettings, Secret, StreamBackendInfo, StreamBackendKind,
};
use std::path::PathBuf;
use std::sync::Arc;

/// Builds the control connector of one seat (the real one holds the seat dir and port).
pub type ConnectorFactory = dyn Fn(&SeatDir, u16) -> Arc<dyn ControlConnector> + Send + Sync;
/// `(manifest, verified exe path, working dir)`; read at every seat start.
pub type DistSource = dyn Fn() -> Result<(ServerDist, PathBuf, PathBuf), StreamError> + Send + Sync;

/// Everything the Sunshine server of a seat needs (spec 80).
pub struct SunshineEnv {
    pub fs: Arc<dyn SecureFs>,
    pub launcher: Arc<dyn ServerLauncher>,
    pub connector: Arc<ConnectorFactory>,
    pub rng: Arc<dyn Random>,
    pub clock: Arc<dyn Clock>,
    pub firewall: Arc<dyn Firewall>,
    pub journal: Arc<dyn RestoreLog>,
    pub dist: Arc<DistSource>,
    pub verify_binary: bool,
    /// Local listeners, for the port check before a seat start (spec 80 §7).
    pub ports: Arc<dyn ListenerProbe>,
}

pub struct SunshineBackend {
    env: SunshineEnv,
}

impl SunshineBackend {
    pub fn new(env: SunshineEnv) -> Self {
        Self { env }
    }

    pub fn env(&self) -> &SunshineEnv {
        &self.env
    }
}

/// Capture key for the server build: `pulsar_idd` only on the Pulsar build, else the
/// server's default (parity review 2026-11-17).
fn capture_for(pref: CapturePref, flavour: ServerFlavour) -> Capture {
    match pref {
        CapturePref::Default => Capture::Default,
        CapturePref::Ddx => Capture::Ddx,
        CapturePref::Wgc => Capture::Wgc,
        CapturePref::PulsarIdd if flavour == ServerFlavour::PulsarFork => Capture::PulsarIdd,
        CapturePref::PulsarIdd => Capture::Default,
    }
}

/// Gamepad key for the server build: our pads only on the Pulsar build, else `auto`.
fn gamepad_for(pref: GamepadPref, flavour: ServerFlavour) -> Gamepad {
    match pref {
        GamepadPref::Auto => Gamepad::Auto,
        GamepadPref::X360 => Gamepad::X360,
        GamepadPref::Ds4 => Gamepad::Ds4,
        p => match p.pulsar_id() {
            Some(id) if flavour == ServerFlavour::PulsarFork => Gamepad::Pulsar(id.to_string()),
            _ => Gamepad::Auto,
        },
    }
}

/// What a Pulsar build can do, from its `hello` capabilities (`None` = assume the whole
/// patch set, before the first `hello`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ForkFeatures {
    pub pulsar_idd: bool,
    pub pulsar_pad: bool,
    pub pair_timeout: bool,
}

impl ForkFeatures {
    pub const ALL: Self = Self { pulsar_idd: true, pulsar_pad: true, pair_timeout: true };

    pub fn from_caps(caps: &[String]) -> Self {
        let has = |c: &str| caps.iter().any(|x| x == c);
        Self { pulsar_idd: has(caps::PULSAR_IDD), pulsar_pad: has(caps::PULSAR_PAD), pair_timeout: has(caps::PAIR_TIMEOUT) }
    }
}

/// `server.conf` for `flavour`; on the Pulsar build only the features in `f` are used, the
/// rest falls back to the stock values (default capture, `auto` pads, no pair timeout).
fn conf_text_for(
    seat: &RemoteSeatRecord,
    global: &RemoteSettings,
    audio_keys: &[(String, String)],
    dir: &SeatDir,
    flavour: ServerFlavour,
    f: ForkFeatures,
) -> Result<String, StreamError> {
    let mut capture = capture_for(seat.stream.capture, flavour);
    if capture == Capture::PulsarIdd && !f.pulsar_idd {
        capture = Capture::Default;
    }
    let mut gamepad = gamepad_for(seat.stream.gamepad, flavour);
    if matches!(gamepad, Gamepad::Pulsar(_)) && !f.pulsar_pad {
        gamepad = Gamepad::Auto;
    }
    generate(&ConfInputs {
        seat,
        global,
        dir,
        flavour,
        capture,
        gamepad,
        audio_keys: audio_keys.to_vec(),
        min_fps: seat.stream.min_fps,
        pair_timeout_s: if f.pair_timeout { PAIR_TIMEOUT_DEFAULT_S } else { 0 },
    })
}

fn conf_text(spec: &SeatStreamSpec<'_>, dist: &ServerDist, dir: &SeatDir) -> Result<String, StreamError> {
    conf_text_for(spec.seat, spec.global, &spec.audio_keys, dir, dist.flavour, ForkFeatures::ALL)
}

impl StreamBackend for SunshineBackend {
    fn kind(&self) -> StreamBackendKind {
        StreamBackendKind::Sunshine
    }

    fn availability(&self) -> StreamBackendInfo {
        match (self.env.dist)() {
            Ok((d, _, _)) => {
                StreamBackendInfo { backend: self.kind(), installed: true, reason: None, version: Some(d.version) }
            }
            Err(e) => {
                StreamBackendInfo { backend: self.kind(), installed: false, reason: Some(e.code().into()), version: None }
            }
        }
    }

    fn prepare(&self, spec: &SeatStreamSpec<'_>) -> Result<Prepared, StreamError> {
        let s = &self.env;
        let seat = spec.seat;
        let mut warnings = Vec::new();
        // Spec 80 §7: the whole block must be free (no listener, not the upstream default
        // block) before anything is launched; otherwise fail with `port_in_use` + the ports.
        match crate::ports::check_free(s.ports.as_ref(), seat.port, &[crate::ports::UPSTREAM_DEFAULT_BLOCK]) {
            Ok(()) => {}
            Err(e @ StreamError::PortInUse(_)) => return Err(e),
            Err(e) => log::warn!("seat '{}': port check skipped: {e}", seat.name),
        }
        let (dist, exe, cwd) = (s.dist)()?;
        let dir = SeatDir::checked(spec.data_root, &seat.id)?;
        let conf_text = conf_text(spec, &dist, &dir)?;
        if let Err(e) = apply_seat_rules(
            s.firewall.as_ref(),
            s.journal.as_ref(),
            &seat.id,
            &seat.display_name,
            seat.port,
            &exe,
            seat.stream.firewall_domain,
        ) {
            // Loopback and the console still work; LAN clients will not reach the seat.
            log::warn!("seat '{}': firewall rules not applied: {e}", seat.name);
            warnings.push(serde_json::json!({"kind": "firewall", "reason": e.code()}));
        }
        // Pulsar build: config for the features it reports, and an upstream config for a
        // binary that never opens the seat pipe (supervise: StreamNotice::Fallback).
        let (conf_for_caps, upstream_conf_text) = if dist.flavour == ServerFlavour::PulsarFork {
            let (seat_c, global_c, audio_c, dir_c) = (seat.clone(), spec.global.clone(), spec.audio_keys.clone(), dir.clone());
            let f: Arc<ConfForCaps> = Arc::new(move |caps: &[String]| {
                conf_text_for(&seat_c, &global_c, &audio_c, &dir_c, ServerFlavour::PulsarFork, ForkFeatures::from_caps(caps))
            });
            let upstream = conf_text_for(seat, spec.global, &spec.audio_keys, &dir, ServerFlavour::Upstream, ForkFeatures::ALL)?;
            (Some(f), Some(upstream))
        } else {
            (None, None)
        };
        let cfg = SeatServerConfig {
            seat_id: seat.id.clone(),
            dist,
            exe,
            cwd,
            dir: dir.clone(),
            conf_text,
            default_apps_json: render_apps_json(&spec.default_apps, &serde_json::Map::new()),
            service_version: spec.service_version.to_string(),
            verify_binary: s.verify_binary,
            conf_for_caps,
            upstream_conf_text,
        };
        let connector = (s.connector)(&dir, seat.port);
        let server = SeatServer::new(cfg, s.fs.clone(), s.launcher.clone(), connector, s.rng.clone(), s.clock.clone());
        Ok(Prepared { stream: Box::new(SunshineSeat(server)), warnings })
    }

    fn remove_seat(&self, seat_id: &str, _keep_pairing: bool) -> Result<(), StreamError> {
        remove_seat_rules(self.env.firewall.as_ref(), self.env.journal.as_ref(), seat_id)
    }

    fn log_file(&self, dir: &SeatDir) -> PathBuf {
        dir.log.clone()
    }

    fn regenerate_identity(&self, dir: &SeatDir) -> Result<(), StreamError> {
        dir.regenerate_identity(self.env.fs.as_ref())
    }
}

/// [`SeatServer`] as a [`SeatStream`].
pub struct SunshineSeat(pub SeatServer);

impl SunshineSeat {
    fn control(&mut self) -> Result<&mut dyn SeatControl, StreamError> {
        self.0.control_or_err()
    }
}

impl SeatStream for SunshineSeat {
    fn backend(&self) -> StreamBackendKind {
        StreamBackendKind::Sunshine
    }
    fn start(&mut self, session_id: u32) -> Result<(), StreamError> {
        self.0.start(session_id)
    }
    fn stop(&mut self) -> Result<(), StreamError> {
        self.0.stop()
    }
    fn poll(&mut self, now_ms: u64) -> Vec<StreamNotice> {
        self.0.poll(now_ms)
    }
    fn is_running(&self) -> bool {
        self.0.is_running()
    }
    fn crash_count(&self) -> u32 {
        self.0.crash_count()
    }
    fn control_ready(&self) -> bool {
        self.0.has_control()
    }
    fn runtime(&self) -> serde_json::Value {
        let s = &self.0;
        serde_json::json!({
            "backend": "sunshine",
            "flavour": s.flavour(),
            "running": s.is_running(),
            "status": s.last_status(),
            "capabilities": s.capabilities(),
            "crash_count": s.crash_count(),
            // A Pulsar build driven like upstream (it never opened its seat pipe).
            "fallback": s.fell_back(),
        })
    }
    fn reload_apps(&mut self, _apps: &[AppEntry]) -> Result<AppsReload, StreamError> {
        self.0.reload_apps()
    }
    fn pair_submit(&mut self, request_id: Option<u32>, pin: &Secret, name: &str) -> Result<String, StreamError> {
        self.control()?.pair_submit(request_id, pin, name)
    }
    fn pair_cancel(&mut self, request_id: Option<u32>) -> Result<(), StreamError> {
        // A Pulsar build that lists its capabilities without `pair_cancel` is not asked.
        let s = &self.0;
        if s.flavour() == ServerFlavour::PulsarFork && !s.capabilities().is_empty() && !s.has_capability(caps::PAIR_CANCEL) {
            return Err(StreamError::Unsupported("pair.cancel".into()));
        }
        self.control()?.pair_cancel(request_id)
    }
    fn clients_list(&mut self) -> Result<Vec<PairedClient>, StreamError> {
        self.control()?.clients_list()
    }
    fn unpair(&mut self, uuid: &str) -> Result<(), StreamError> {
        self.control()?.unpair(uuid)
    }
    fn unpair_all(&mut self) -> Result<(), StreamError> {
        self.control()?.unpair_all()
    }
    fn reply_mode(&mut self, result: &serde_json::Value) -> Result<(), StreamError> {
        self.control()?.reply_mode(result)
    }
    fn pad_request(&mut self, ty: &str, body: &serde_json::Value) -> Result<(), StreamError> {
        self.control()?.pad_request(ty, body)
    }
}

#[cfg(test)]
mod pref_tests {
    use super::*;

    #[test]
    fn pulsar_build_options_fall_back_on_a_stock_server() {
        let fork = ServerFlavour::PulsarFork;
        let stock = ServerFlavour::Upstream;
        assert_eq!(capture_for(CapturePref::PulsarIdd, fork), Capture::PulsarIdd);
        assert_eq!(capture_for(CapturePref::PulsarIdd, stock), Capture::Default);
        assert_eq!(capture_for(CapturePref::Wgc, stock), Capture::Wgc);
        assert_eq!(gamepad_for(GamepadPref::PulsarDs5, fork), Gamepad::Pulsar("pulsar_ds5".into()));
        assert_eq!(gamepad_for(GamepadPref::PulsarXone, stock), Gamepad::Auto);
        assert_eq!(gamepad_for(GamepadPref::Ds4, fork), Gamepad::Ds4);
        for p in [GamepadPref::PulsarDs4, GamepadPref::PulsarDs5, GamepadPref::PulsarDs5e, GamepadPref::PulsarXone] {
            assert!(crate::conf::PULSAR_GAMEPADS.contains(&p.pulsar_id().unwrap()));
        }
    }
}
