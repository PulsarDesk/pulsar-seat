// SPDX-License-Identifier: GPL-3.0-only
//! Windows parts of the remote seat worker (from Pulsar Split's `split-service`, merge
//! step 6):
//!
//! | Module | What |
//! | --- | --- |
//! | [`platform`] | the controller's `Platform`: local users, account checks, engine calls |
//! | [`wiring`] | the engine's real package backends (session, display, audio, stream, pads, shim) |
//! | [`health`] | shim health pipe `\\.\pipe\PulsarSplit.ShimHealth` (name kept for `shim/`) |
//! | [`picture`] | local account pictures (`get_user_picture`) |
//! | [`setup_cmd`] | `pulsar-seat-service remote-setup install / reconcile / uninstall / check-coexistence` |
//!
//! [`build`] assembles the worker for the runtime; [`EngineHost`] gives the service the
//! engine's boot and stop hooks.

pub mod health;
pub mod picture;
pub mod platform;
pub mod setup_cmd;
pub mod wiring;
pub mod wrds;

use super::control::Controller;
use super::engine::Engine;
use super::{RemoteHost, StorePulsarSecrets};
use crate::log;
use crate::service::RemoteDeps;
use seat_core::remote::RemoteSettings;
use seat_core::store::SecretStore;
use std::path::Path;
use std::sync::Arc;

/// The engine as the service's [`RemoteHost`].
pub struct EngineHost(pub Engine);

impl RemoteHost for EngineHost {
    fn service_start(&self, global: &RemoteSettings, seat_users: &[String]) {
        self.0.service_start(global, seat_users);
    }
    fn service_stop(&self, seat_users: &[String]) {
        self.0.service_stop(seat_users);
    }
}

/// The remote seat worker on this machine. `secrets` backs the Pulsar secrets (LSA in
/// service mode). Never fails: without the packages (logged) the worker answers seat starts
/// and engine requests with `unsupported`.
pub fn build(
    data_dir: &Path,
    install_dir: &Path,
    global: &RemoteSettings,
    secrets: Arc<dyn SecretStore>,
    console: bool,
) -> (RemoteDeps, Option<Engine>) {
    let pulsar = Arc::new(StorePulsarSecrets(secrets));
    let engine = match wiring::build(data_dir, install_dir, global, crate::service::SERVICE_VERSION, pulsar) {
        Ok(d) => Some(Engine::new(d)),
        Err(e) => {
            log::error(&format!("remote seat runtime not available: {e}"));
            None
        }
    };
    let platform = platform::WindowsPlatform { config_dir: data_dir.join("config"), engine: engine.clone(), global: global.clone() };
    let host = engine.clone().map(|e| Arc::new(EngineHost(e)) as Arc<dyn RemoteHost>);
    let deps = RemoteDeps {
        worker: Box::new(move |doc| Box::new(Controller::new(doc, platform, crate::service::SERVICE_VERSION, console))),
        host,
    };
    (deps, engine)
}
