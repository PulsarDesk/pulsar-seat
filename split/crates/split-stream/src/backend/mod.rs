// SPDX-License-Identifier: GPL-3.0-only
//! Stream-server backends (contracts §7.4, docs/agents/pulsar-host-backend.md).
//!
//! Each seat runs exactly one backend, picked by `SeatConfig::stream_backend`:
//!
//! | Kind | Type | Server | Control |
//! | --- | --- | --- | --- |
//! | `sunshine` | [`sunshine::SunshineBackend`] | upstream Sunshine (Moonlight clients) | loopback web UI or fork seat pipe ([`crate::control`]) |
//! | `pulsar` | [`pulsar::PulsarHostBackend`] | `pulsar-host.exe` (Pulsar clients) | service-owned pipe ([`pulsar::control`]) |
//!
//! The service engine only talks to the two traits here:
//!
//! - [`StreamBackend`]: one per kind, shared by all seats. Availability, seat preparation
//!   (port check, binary check, seat files, firewall), seat removal, log path, identity
//!   reset, per-seat secrets.
//! - [`SeatStream`]: one per running seat. Start / stop / poll under the watchdog, status,
//!   pairing (PIN or approve), paired clients, apps, display-mode and pad answers.
//!
//! Both are object safe; the fakes in [`crate::fake`] drive them in tests. A backend whose
//! binary is missing fails `prepare` with `backend_missing`; the engine never falls back to
//! another backend.

pub mod pulsar;
pub mod sunshine;

use crate::apps::AppEntry;
use crate::control::PairedClient;
use crate::seatdir::SeatDir;
use crate::supervise::{AppsReload, StreamNotice};
use crate::StreamError;
use serde::{Deserialize, Serialize};
use split_proto::{GlobalSettings, SeatConfig, Secret, StreamBackendInfo, StreamBackendKind};
use std::path::{Path, PathBuf};

pub use pulsar::PulsarHostBackend;
pub use sunshine::SunshineBackend;

/// Inputs to [`StreamBackend::prepare`] for one seat start.
pub struct SeatStreamSpec<'a> {
    pub seat: &'a SeatConfig,
    pub global: &'a GlobalSettings,
    /// `%ProgramData%\PulsarSplit`.
    pub data_root: &'a Path,
    /// Audio keys for the server config (Sunshine; `split_audio::server_keys`).
    pub audio_keys: Vec<(String, String)>,
    /// App list written when `apps.json` is missing (already launcher-wrapped).
    pub default_apps: Vec<AppEntry>,
    pub service_version: &'a str,
}

/// Result of [`StreamBackend::prepare`].
pub struct Prepared {
    /// Not started yet: call [`SeatStream::start`].
    pub stream: Box<dyn SeatStream>,
    /// Non-fatal problems for the event ring (`host_warning` data, e.g. `{kind: firewall}`).
    pub warnings: Vec<serde_json::Value>,
}

/// One stream-server kind, shared by every seat that uses it.
pub trait StreamBackend: Send + Sync {
    fn kind(&self) -> StreamBackendKind;
    /// Installed (manifest + matching hash) or missing with a reason code.
    fn availability(&self) -> StreamBackendInfo;
    /// Port check, binary check, seat files, firewall; returns the seat's runner. Errors
    /// carry reason codes (`port_in_use`, `backend_missing`, `server_hash_mismatch`, …).
    fn prepare(&self, spec: &SeatStreamSpec<'_>) -> Result<Prepared, StreamError>;
    /// Seat deleted: firewall rules and (unless `keep_pairing`) backend secrets.
    fn remove_seat(&self, seat_id: &str, keep_pairing: bool) -> Result<(), StreamError>;
    /// Current log file of the seat's server (for the log viewer and `GaveUp`).
    fn log_file(&self, dir: &SeatDir) -> PathBuf;
    /// New seat identity (seat stopped): every client must pair again. For Pulsar the
    /// seat's relay id changes too.
    fn regenerate_identity(&self, dir: &SeatDir) -> Result<(), StreamError>;
    /// Pulsar: store / clear the permanent connect password (write-only).
    fn set_connect_password(&self, seat_id: &str, password: Option<&Secret>) -> Result<(), StreamError> {
        let _ = (seat_id, password);
        Err(StreamError::WrongBackend(self.kind()))
    }
}

/// A seat's server under supervision. Methods a backend lacks answer
/// [`StreamError::WrongBackend`] (reason `backend_<kind>`).
pub trait SeatStream: Send {
    fn backend(&self) -> StreamBackendKind;
    /// Launch in `session_id` (stops a previous instance first).
    fn start(&mut self, session_id: u32) -> Result<(), StreamError>;
    /// Graceful stop, then terminate. On error the process stays tracked.
    fn stop(&mut self) -> Result<(), StreamError>;
    /// Exits, back-off restarts, control connect, events, status (call about every second).
    fn poll(&mut self, now_ms: u64) -> Vec<StreamNotice>;
    fn is_running(&self) -> bool;
    fn crash_count(&self) -> u32;
    /// Control channel up: pairing and client management work.
    fn control_ready(&self) -> bool;
    /// `SeatRuntime.stream` (`{backend, running, status, capabilities, crash_count, …}`).
    fn runtime(&self) -> serde_json::Value;
    /// `apps.json` was rewritten with `apps`.
    fn reload_apps(&mut self, apps: &[AppEntry]) -> Result<AppsReload, StreamError>;

    /// Sunshine: PIN for a pending request; returns the new client's uuid.
    fn pair_submit(&mut self, request_id: Option<u32>, pin: &Secret, name: &str) -> Result<String, StreamError> {
        let _ = (request_id, pin, name);
        Err(StreamError::WrongBackend(self.backend()))
    }
    /// Pulsar: allow / deny a pending request.
    fn pair_answer(&mut self, request_id: u32, allow: bool, view_only: bool) -> Result<(), StreamError> {
        let _ = (request_id, allow, view_only);
        Err(StreamError::WrongBackend(self.backend()))
    }
    fn pair_cancel(&mut self, request_id: Option<u32>) -> Result<(), StreamError>;
    fn clients_list(&mut self) -> Result<Vec<PairedClient>, StreamError>;
    fn unpair(&mut self, uuid: &str) -> Result<(), StreamError>;
    fn unpair_all(&mut self) -> Result<(), StreamError>;
    /// `display.mode.result` answer.
    fn reply_mode(&mut self, result: &serde_json::Value) -> Result<(), StreamError>;
    /// `pad.attach` / `pad.denied` / `pad.detach`.
    fn pad_request(&mut self, ty: &str, body: &serde_json::Value) -> Result<(), StreamError>;

    /// Pulsar: current (`rotate = false`) or new one-time password.
    fn otp(&mut self, rotate: bool) -> Result<Secret, StreamError> {
        let _ = rotate;
        Err(StreamError::WrongBackend(self.backend()))
    }
    /// Pulsar: relay credentials asked for by [`PeerEvent::RelayAuthRequired`].
    fn relay_auth(&mut self, password: &Secret, totp: Option<&Secret>) -> Result<(), StreamError> {
        let _ = (password, totp);
        Err(StreamError::WrongBackend(self.backend()))
    }
    /// Pulsar: the stored secrets changed (connect password); hand them over live.
    fn secrets_changed(&mut self) -> Result<(), StreamError> {
        Ok(())
    }
    /// Pulsar: answer to [`PeerEvent::AppLaunch`] (`Ok(Some(pid))`, `Ok(None)` for an entry
    /// without a command such as "Masaüstü", or a reason code).
    fn app_launched(&mut self, request_id: u32, result: Result<Option<u32>, String>) -> Result<(), StreamError> {
        let _ = (request_id, result);
        Err(StreamError::WrongBackend(self.backend()))
    }
    /// Pulsar: the relay waits for credentials.
    fn relay_auth_required(&self) -> bool {
        false
    }
}

/// Events only the Pulsar backend raises (contract §3.4). Shared ones (stream, capture,
/// display mode, pads, audio) arrive as [`crate::control::ServerEvent`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PeerEvent {
    /// A connect or pair request waits for the operator.
    AuthRequest {
        request_id: u32,
        /// `connect` | `pair`.
        request_kind: String,
        label: String,
        device_name: String,
        key_fp: String,
        password_state: String,
        received_at: String,
        expires_in_s: u32,
    },
    AuthEnded { request_id: u32, result: String },
    /// The one-time password rotated (no secret in the event).
    OtpChanged,
    TrustChanged,
    RelayState { state: String, id: Option<String> },
    RelayAuthRequired { password: bool, totp: bool },
    /// A client picked an app; the service launches it as the seat user and answers with
    /// [`SeatStream::app_launched`].
    AppLaunch { request_id: u32, app_id: String, sid: String },
}

/// Pick the backend of `kind` from a list.
pub fn find_backend(backends: &[std::sync::Arc<dyn StreamBackend>], kind: StreamBackendKind) -> Option<std::sync::Arc<dyn StreamBackend>> {
    backends.iter().find(|b| b.kind() == kind).cloned()
}

/// Availability of every kind; a kind without a backend object is missing.
pub fn availability_all(backends: &[std::sync::Arc<dyn StreamBackend>]) -> Vec<StreamBackendInfo> {
    StreamBackendKind::ALL
        .into_iter()
        .map(|k| match find_backend(backends, k) {
            Some(b) => b.availability(),
            None => StreamBackendInfo { backend: k, installed: false, reason: Some("backend_missing".into()), version: None },
        })
        .collect()
}

#[cfg(test)]
mod tests;
