// SPDX-License-Identifier: GPL-3.0-only
//! The remote seat worker's request handling (Pulsar Split's controller). Moved from
//! `split-core` at merge step 4 and from `split-service` into `seat-service` at step 6. It
//! still speaks Split's v2 enums (`seat_proto::remote::legacy`); the service maps protocol
//! 3 requests onto them (`crate::remote::to_legacy`). The service wires a [`Controller`] to
//! `crate::remote::DocStore` (the Seat config document) and the Windows [`Platform`]; tests
//! use [`tests::FakePlatform`] and `seat_core::remote::store::MemoryStore`.
//!
//! Since protocol v2 seat start and stop are asynchronous (contracts §9.4): the platform
//! starts a per-seat worker and returns at once; progress comes back as [`SeatEvent`]s that
//! the controller drains after every request and on the service's 1 s tick
//! ([`Controller::drain_reports`]). Every state change also lands in a bounded event ring
//! that the manager reads with `poll_events`.

use seat_core::remote::ports;
use seat_core::remote::store::{ConfigStore, StoreError};
use seat_core::remote::validate::{self, UserFacts};
use seat_proto::PROTOCOL_VERSION;
use seat_proto::remote::legacy::{Request, RequestBody, Response, ResponseBody};
use seat_proto::remote::{
    CredentialTest, ErrorCode, Events, HostInfo, IsolationStatus, LocalUser, NewSeat, PairState, PairedClient, ProtoError,
    RemoteSeatRecord, RemoteSettings, SeatRuntime, SeatState, SeatStatus, Secret, ServiceEvent, ServiceStatus, ShimTargetDto,
    StreamBackendInfo, StreamBackendKind,
};
use std::collections::{HashMap, VecDeque};

/// Events the ring keeps (oldest dropped first).
pub const EVENT_RING: usize = 256;

/// Progress of a seat, reported by the platform's workers (keyed by seat id).
#[derive(Debug, Clone, PartialEq)]
pub enum SeatEvent {
    /// Boot step now running (`user`, `session`, `display`, `audio`, `stream`, `pads`,
    /// `isolation`, `autostart`).
    Phase(String),
    Running { session_id: u32 },
    /// The seat is down after a failure (cleanup done). `reason` = package reason code.
    Failed { reason: String, message: String },
    /// Stop finished.
    Stopped,
    /// Total stream-server crashes since the seat started.
    Crashes(u32),
    /// The seat's Windows session id changed (helper reconnect).
    Session(u32),
    /// A client display mode was applied; persisted as the seat's `last_mode`.
    ModeApplied(seat_proto::remote::LastMode),
    /// Anything else for the event ring (`pair_pending`, `pair_ended`, `stream_started`,
    /// `stream_ended`, `host_warning`).
    Notice { kind: String, data: serde_json::Value },
}

fn unsupported(what: &str) -> ProtoError {
    ProtoError::new(ErrorCode::Unsupported, format!("unsupported: {what} is not available")).with_reason("unsupported")
}

/// Everything that touches Windows. Implemented by the service. Methods with a default
/// body are protocol v2 features a platform may lack (they answer `unsupported`).
pub trait Platform: Send {
    /// Start bringing a seat up (session, display, audio, stream server, pads, isolation,
    /// autostart apps). Returns once the work is under way; progress via `take_reports`.
    fn start_seat(&mut self, seat: &RemoteSeatRecord, global: &RemoteSettings, password: Option<Secret>) -> Result<(), ProtoError>;
    /// Start taking the seat down; finishes with [`SeatEvent::Stopped`].
    fn stop_seat(&mut self, seat: &RemoteSeatRecord) -> Result<(), ProtoError>;
    /// Remove the seat's files and firewall rules after deletion.
    fn remove_seat_files(&mut self, seat: &RemoteSeatRecord);
    /// Hand a pairing PIN to the seat's stream server (asynchronous; outcome as a
    /// `pair_ended` notice).
    fn submit_pin(&mut self, seat: &RemoteSeatRecord, pin: &Secret, client_name: &str, request_id: Option<u32>) -> Result<(), ProtoError>;
    /// Apply global settings that live outside our store (service start type, isolation).
    fn apply_global(&mut self, old: &RemoteSettings, new: &RemoteSettings) -> Result<(), ProtoError>;
    fn list_local_users(&self) -> Result<Vec<LocalUser>, ProtoError>;
    fn user_facts(&self, user: &str) -> UserFacts;
    /// User logged on at the physical console, if any.
    fn console_user(&self) -> Option<String>;

    /// RDP client name for a new seat (`PSS` + 8 hex), unique among `taken`.
    fn client_name_for(&self, seat_id: &str, taken: &[String]) -> String {
        seat_core::remote::client_name_for(seat_id, taken)
    }
    /// Local ports that are listened on right now (TCP listeners and bound UDP ports), for
    /// port allocation (spec 80 §7.2). Default: none known.
    fn listening_ports(&self) -> Vec<u16> {
        Vec::new()
    }
    /// Progress since the last call.
    fn take_reports(&mut self) -> Vec<(String, SeatEvent)> {
        Vec::new()
    }
    /// Coexistence passive mode: no system change, no seat start.
    fn passive(&self) -> bool {
        false
    }
    fn host_class(&self) -> Option<String> {
        None
    }
    fn host_info(&mut self, _global: &RemoteSettings) -> Result<HostInfo, ProtoError> {
        Err(unsupported("host information"))
    }
    /// Prepare (on) or restore (off) the host's RDS settings.
    fn set_multi_session(&mut self, _enabled: bool) -> Result<(), ProtoError> {
        Err(unsupported("multi-session"))
    }
    fn termwrap_status(&mut self) -> Result<serde_json::Value, ProtoError> {
        Err(unsupported("experimental multi-session"))
    }
    fn set_termwrap(&mut self, _enabled: bool, _wrapper: Option<&str>, _ack: u32) -> Result<serde_json::Value, ProtoError> {
        Err(unsupported("experimental multi-session"))
    }
    fn seat_runtime(&mut self, _seat: &RemoteSeatRecord) -> Result<SeatRuntime, ProtoError> {
        Ok(SeatRuntime::default())
    }
    fn render_adapters(&mut self) -> Result<Vec<serde_json::Value>, ProtoError> {
        Err(unsupported("render adapters"))
    }
    fn pad_backends(&mut self) -> Result<Vec<serde_json::Value>, ProtoError> {
        Err(unsupported("pad backends"))
    }
    fn pair_state(&mut self, _seat: &RemoteSeatRecord) -> Result<PairState, ProtoError> {
        Ok(PairState::default())
    }
    fn cancel_pair(&mut self, _seat: &RemoteSeatRecord) -> Result<(), ProtoError> {
        Err(unsupported("pairing"))
    }
    fn paired_clients(&mut self, _seat: &RemoteSeatRecord) -> Result<Vec<PairedClient>, ProtoError> {
        Err(unsupported("pairing"))
    }
    fn unpair_client(&mut self, _seat: &RemoteSeatRecord, _uuid: &str) -> Result<(), ProtoError> {
        Err(unsupported("pairing"))
    }
    fn unpair_all(&mut self, _seat: &RemoteSeatRecord) -> Result<(), ProtoError> {
        Err(unsupported("pairing"))
    }
    /// Seat is stopped (checked by the controller).
    fn regenerate_identity(&mut self, _seat: &RemoteSeatRecord) -> Result<(), ProtoError> {
        Err(unsupported("seat identity"))
    }
    fn get_apps(&mut self, _seat: &RemoteSeatRecord) -> Result<Vec<serde_json::Value>, ProtoError> {
        Err(unsupported("app list"))
    }
    fn set_apps(&mut self, _seat: &RemoteSeatRecord, _apps: Vec<serde_json::Value>) -> Result<Vec<serde_json::Value>, ProtoError> {
        Err(unsupported("app list"))
    }
    fn isolation_status(&mut self) -> Result<IsolationStatus, ProtoError> {
        Err(unsupported("isolation"))
    }
    fn set_shim_targets(&mut self, _targets: Vec<ShimTargetDto>, _global: &RemoteSettings) -> Result<IsolationStatus, ProtoError> {
        Err(unsupported("isolation"))
    }
    fn set_app_masking(&mut self, _exe_path: &str, _enabled: bool) -> Result<IsolationStatus, ProtoError> {
        Err(unsupported("isolation"))
    }
    fn set_dxvk(&mut self, _game_exe: &str, _enabled: bool, _dlls: &[String]) -> Result<(), ProtoError> {
        Err(unsupported("DXVK"))
    }
    /// Logon test against the local machine (spec 10 §4.4); `Err(win32 code)` on failure.
    /// Must not change the account.
    fn test_logon(&self, _user: &str, _password: &Secret) -> Result<Result<(), u32>, ProtoError> {
        Err(unsupported("credential test"))
    }
    /// Windows text for a Win32 error code (system language).
    fn win32_message(&self, code: u32) -> String {
        format!("Windows error {code}")
    }
    /// Clear the disabled / locked-out flags of a local account.
    fn enable_unlock_account(&mut self, _user: &str, _enable: bool, _unlock: bool) -> Result<(), ProtoError> {
        Err(unsupported("account enable / unlock"))
    }
    /// Last `n` redacted lines of the service log (`None`) or a seat's stream-server log.
    fn log_tail(&mut self, _seat: Option<&RemoteSeatRecord>, _n: usize) -> Result<Vec<String>, ProtoError> {
        Err(unsupported("log viewer"))
    }
    /// Restart Windows (the controller already asked every seat to stop).
    fn reboot_host(&mut self) -> Result<(), ProtoError> {
        Err(unsupported("restart"))
    }
    /// Availability of each stream backend (installed / missing + reason).
    fn stream_backends(&mut self) -> Result<Vec<StreamBackendInfo>, ProtoError> {
        Err(unsupported("stream backends"))
    }
    /// Pulsar seats (checked by the controller): allow / deny a pending request.
    fn answer_peer_request(&mut self, _seat: &RemoteSeatRecord, _request_id: u32, _allow: bool, _view_only: bool) -> Result<(), ProtoError> {
        Err(unsupported("peer approval"))
    }
    /// Pulsar seats: store (`Some`) or clear (`None`) the permanent connect password; the
    /// seat may be stopped. A running seat gets it live.
    fn set_connect_password(&mut self, _seat: &RemoteSeatRecord, _password: Option<&Secret>) -> Result<(), ProtoError> {
        Err(unsupported("connect password"))
    }
    /// Pulsar seats (running): current (`rotate = false`) or new one-time password.
    fn seat_otp(&mut self, _seat: &RemoteSeatRecord, _rotate: bool) -> Result<Secret, ProtoError> {
        Err(unsupported("one-time password"))
    }
    /// Pulsar seats (running): relay credentials.
    fn answer_relay_auth(&mut self, _seat: &RemoteSeatRecord, _password: &Secret, _totp: Option<&Secret>) -> Result<(), ProtoError> {
        Err(unsupported("relay sign-in"))
    }
    /// Pinned DXVK release and the games with a DXVK redirect (spec 50 §10.2).
    fn dxvk_status(&mut self) -> Result<seat_proto::remote::DxvkStatus, ProtoError> {
        Err(unsupported("DXVK"))
    }
    /// Account picture of a local user (spec 10 §2.1); empty when there is none.
    fn user_picture(&mut self, _user: &str) -> Result<seat_proto::remote::UserPicture, ProtoError> {
        Ok(seat_proto::remote::UserPicture::default())
    }
    /// Spec 70 §2.6 stale RDP audio endpoint cleanup (operator-triggered).
    fn cleanup_audio_endpoints(&mut self) -> Result<seat_proto::remote::AudioCleanup, ProtoError> {
        Err(unsupported("audio endpoint cleanup"))
    }
}

/// `unsupported` with reason `backend_<kind>`: the request does not exist for the seat's
/// stream backend (e.g. a PIN for a Pulsar seat).
pub fn wrong_backend(kind: StreamBackendKind) -> ProtoError {
    let reason = format!("backend_{}", kind.as_str());
    ProtoError::new(ErrorCode::Unsupported, format!("{reason}: not available for this seat's stream server")).with_reason(reason)
}

/// Client name of a `stream_started` notice (`client` or `client_name`, 1-64 printable
/// characters), if the stream server sent one.
fn stream_client_name(data: &serde_json::Value) -> Option<String> {
    let n = data.get("client").or_else(|| data.get("client_name"))?.as_str()?.trim();
    (!n.is_empty() && n.chars().count() <= 64 && !n.chars().any(char::is_control)).then(|| n.to_string())
}

/// Pulsar secrets typed by the operator: 1-128 characters, no control characters.
fn check_operator_secret(s: &Secret, field: &str) -> Result<(), ProtoError> {
    let n = s.0.chars().count();
    if n == 0 || n > 128 || s.0.chars().any(char::is_control) {
        return Err(ProtoError::new(ErrorCode::InvalidValue, format!("{field}: 1-128 characters, no control characters")).with_field(field));
    }
    Ok(())
}

#[derive(Debug, Default)]
struct Runtime {
    state: SeatState,
    phase: Option<String>,
    failure: Option<String>,
    last_error: Option<String>,
    session_id: Option<u32>,
    crash_count: u32,
    /// Between `stream_started` and `stream_ended` / seat stop.
    streaming: bool,
    stream_client: Option<String>,
}

impl Runtime {
    fn is_active(&self) -> bool {
        matches!(self.state, SeatState::Booting | SeatState::Running | SeatState::Stopping)
    }
}

pub struct Controller<S: ConfigStore, P: Platform> {
    store: S,
    platform: P,
    /// Keyed by seat id (stable across renames).
    runtime: HashMap<String, Runtime>,
    service_version: String,
    console_mode: bool,
    events: VecDeque<ServiceEvent>,
    next_seq: u64,
}

fn store_err(e: StoreError) -> ProtoError {
    match e {
        StoreError::NotFound(n) => not_found(&n),
        other => ProtoError::new(ErrorCode::Store, other.to_string()),
    }
}

fn not_found(name: &str) -> ProtoError {
    ProtoError::new(ErrorCode::NotFound, format!("seat '{name}' does not exist"))
}

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn passive_err() -> ProtoError {
    ProtoError::new(
        ErrorCode::Coexistence,
        "coexistence: the reference multi-seat product is present; Pulsar Split is in passive mode",
    )
    .with_reason("coexistence")
}

impl<S: ConfigStore, P: Platform> Controller<S, P> {
    pub fn new(store: S, platform: P, service_version: impl Into<String>, console_mode: bool) -> Self {
        Self {
            store,
            platform,
            runtime: HashMap::new(),
            service_version: service_version.into(),
            console_mode,
            events: VecDeque::new(),
            next_seq: 1,
        }
    }

    // Used by the tests (and by split-core users before the move into this crate).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn platform(&self) -> &P {
        &self.platform
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn platform_mut(&mut self) -> &mut P {
        &mut self.platform
    }

    // Split's own pipe is gone (merge step 6); the tests still drive the controller this way.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn handle(&mut self, req: Request) -> Response {
        if req.version != PROTOCOL_VERSION {
            let e = ProtoError::new(
                ErrorCode::VersionMismatch,
                format!("protocol {} requested, service speaks {PROTOCOL_VERSION}", req.version),
            );
            return Response::new(req.id, Err(e));
        }
        let result = self.dispatch(req.body).map_err(ProtoError::with_reason_from_message);
        self.drain_reports();
        Response::new(req.id, result)
    }

    fn dispatch(&mut self, body: RequestBody) -> Result<ResponseBody, ProtoError> {
        match body {
            RequestBody::Hello => Ok(ResponseBody::Hello {
                service_version: self.service_version.clone(),
                protocol_version: PROTOCOL_VERSION,
            }),
            RequestBody::GetStatus => self.status().map(ResponseBody::Status),
            RequestBody::ListSeats => Ok(ResponseBody::Seats(self.config()?.seats)),
            RequestBody::GetSeat { name } => self.seat(&name).map(ResponseBody::Seat),
            RequestBody::CreateSeat(new) => self.create_seat(new).map(ResponseBody::Seat),
            RequestBody::UpdateSeat { name, seat, stream_backend } => {
                self.update_seat(&name, seat, stream_backend).map(ResponseBody::Seat)
            }
            RequestBody::SetSeatPassword { name, password } => {
                self.set_password(&name, &password).map(|_| ResponseBody::Ack)
            }
            RequestBody::StartSeat { name } => self.start_seat(&name).map(|_| ResponseBody::Ack),
            RequestBody::StopSeat { name } => self.stop_seat(&name).map(|_| ResponseBody::Ack),
            RequestBody::GetGlobalSettings => Ok(ResponseBody::GlobalSettings(self.config()?.global)),
            RequestBody::SetGlobalSettings(g) => self.set_global(g).map(ResponseBody::GlobalSettings),
            RequestBody::ListLocalUsers => self.platform.list_local_users().map(ResponseBody::Users),
            RequestBody::SubmitPin { name, pin, client_name, request_id } => {
                let seat = self.running_seat(&name)?;
                if seat.stream_backend != StreamBackendKind::Sunshine {
                    return Err(wrong_backend(seat.stream_backend));
                }
                if pin.0.len() != 4 || !pin.0.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(ProtoError::new(ErrorCode::InvalidValue, "PIN must be 4 digits").with_field("pin"));
                }
                if client_name.trim().is_empty() || client_name.chars().count() > 64 || client_name.chars().any(char::is_control) {
                    return Err(ProtoError::new(ErrorCode::InvalidValue, "client name 1-64 characters").with_field("client_name"));
                }
                self.platform.submit_pin(&seat, &pin, &client_name, request_id).map(|_| ResponseBody::Ack)
            }
            RequestBody::GetHostInfo => {
                let g = self.config()?.global;
                self.platform.host_info(&g).map(ResponseBody::HostInfo)
            }
            RequestBody::SetMultiSession { enabled } => self.set_multi_session(enabled).map(ResponseBody::HostInfo),
            RequestBody::GetTermwrapStatus => self.platform.termwrap_status().map(ResponseBody::TermwrapStatus),
            RequestBody::SetTermwrap { enabled, wrapper_path, ack_warning_version } => {
                if self.platform.passive() {
                    return Err(passive_err());
                }
                // Every remote session drops when TermService restarts (spec 90 §4.3).
                self.stop_all();
                self.platform
                    .set_termwrap(enabled, wrapper_path.as_deref(), ack_warning_version)
                    .map(ResponseBody::TermwrapStatus)
            }
            RequestBody::GetSeatRuntime { name } => self.seat_runtime(&name).map(ResponseBody::SeatRuntime),
            RequestBody::ListRenderAdapters => self.platform.render_adapters().map(ResponseBody::RenderAdapters),
            RequestBody::GetPadBackends => self.platform.pad_backends().map(ResponseBody::PadBackends),
            RequestBody::GetPairState { name } => {
                let seat = self.seat(&name)?;
                let running = self.rt(&seat.id).is_some_and(|r| r.state == SeatState::Running);
                if !running {
                    return Ok(ResponseBody::PairState(PairState::default()));
                }
                self.platform.pair_state(&seat).map(ResponseBody::PairState)
            }
            RequestBody::CancelPair { name } => {
                let seat = self.running_seat(&name)?;
                self.platform.cancel_pair(&seat).map(|_| ResponseBody::Ack)
            }
            RequestBody::ListPairedClients { name } => {
                let seat = self.running_seat(&name)?;
                self.platform.paired_clients(&seat).map(ResponseBody::PairedClients)
            }
            RequestBody::UnpairClient { name, uuid } => {
                let seat = self.running_seat(&name)?;
                if uuid.is_empty() || uuid.len() > 64 || !uuid.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
                    return Err(ProtoError::new(ErrorCode::InvalidValue, "bad client uuid").with_field("uuid"));
                }
                self.platform.unpair_client(&seat, &uuid).map(|_| ResponseBody::Ack)
            }
            RequestBody::UnpairAll { name } => {
                let seat = self.running_seat(&name)?;
                self.platform.unpair_all(&seat).map(|_| ResponseBody::Ack)
            }
            RequestBody::RegenerateSeatIdentity { name, confirm } => {
                if !confirm {
                    return Err(ProtoError::new(ErrorCode::BadRequest, "confirm must be true").with_field("confirm"));
                }
                let seat = self.seat(&name)?;
                self.ensure_stopped(&seat)?;
                self.platform.regenerate_identity(&seat).map(|_| ResponseBody::Ack)
            }
            RequestBody::GetApps { name } => {
                let seat = self.seat(&name)?;
                self.platform.get_apps(&seat).map(ResponseBody::Apps)
            }
            RequestBody::SetApps { name, apps } => {
                let seat = self.seat(&name)?;
                self.platform.set_apps(&seat, apps).map(ResponseBody::Apps)
            }
            RequestBody::GetIsolationStatus => self.platform.isolation_status().map(ResponseBody::IsolationStatus),
            RequestBody::SetShimTargets { targets } => {
                if self.platform.passive() {
                    return Err(passive_err());
                }
                let g = self.config()?.global;
                self.platform.set_shim_targets(targets, &g).map(ResponseBody::IsolationStatus)
            }
            RequestBody::SetAppMasking { exe_path, enabled } => {
                if self.platform.passive() {
                    return Err(passive_err());
                }
                self.platform.set_app_masking(&exe_path, enabled).map(ResponseBody::IsolationStatus)
            }
            RequestBody::SetDxvk { game_exe, enabled, dlls } => {
                if self.platform.passive() {
                    return Err(passive_err());
                }
                self.platform.set_dxvk(&game_exe, enabled, &dlls).map(|_| ResponseBody::Ack)
            }
            RequestBody::PollEvents { after, .. } => Ok(ResponseBody::Events(self.events_after(after))),
            RequestBody::TestSeatCredentials { name } => self.test_credentials(&name).map(ResponseBody::CredentialTest),
            RequestBody::GetLogTail { source, lines } => {
                let n = lines.clamp(1, 1000) as usize;
                let lines = if source == "service" {
                    self.platform.log_tail(None, n)?
                } else if let Some(name) = source.strip_prefix("seat:") {
                    let seat = self.seat(name)?;
                    self.platform.log_tail(Some(&seat), n)?
                } else {
                    return Err(ProtoError::new(ErrorCode::InvalidValue, "source: service | seat:<name>").with_field("source"));
                };
                Ok(ResponseBody::LogLines(lines))
            }
            RequestBody::GetStreamBackends => self.platform.stream_backends().map(ResponseBody::StreamBackends),
            RequestBody::AnswerPeerRequest { name, request_id, allow, view_only } => {
                let seat = self.pulsar_seat(&name, true)?;
                self.platform.answer_peer_request(&seat, request_id, allow, view_only).map(|_| ResponseBody::Ack)
            }
            RequestBody::SetSeatConnectPassword { name, password } => {
                let seat = self.pulsar_seat(&name, false)?;
                if password.0.is_empty() {
                    self.platform.set_connect_password(&seat, None)
                } else {
                    check_operator_secret(&password, "password")?;
                    self.platform.set_connect_password(&seat, Some(&password))
                }
                .map(|_| ResponseBody::Ack)
            }
            RequestBody::GetSeatOtp { name } => {
                let seat = self.pulsar_seat(&name, true)?;
                self.platform.seat_otp(&seat, false).map(|otp| ResponseBody::Otp { otp })
            }
            RequestBody::RotateSeatOtp { name } => {
                let seat = self.pulsar_seat(&name, true)?;
                self.platform.seat_otp(&seat, true).map(|otp| ResponseBody::Otp { otp })
            }
            RequestBody::AnswerRelayAuth { name, password, totp } => {
                let seat = self.pulsar_seat(&name, true)?;
                check_operator_secret(&password, "password")?;
                if let Some(t) = &totp
                    && (t.0.len() != 6 || !t.0.bytes().all(|b| b.is_ascii_digit()))
                {
                    return Err(ProtoError::new(ErrorCode::InvalidValue, "totp: 6 digits").with_field("totp"));
                }
                self.platform.answer_relay_auth(&seat, &password, totp.as_ref()).map(|_| ResponseBody::Ack)
            }
            RequestBody::GetDxvkStatus => self.platform.dxvk_status().map(ResponseBody::DxvkStatus),
            RequestBody::GetUserPicture { user } => {
                let u = user.trim();
                if u.is_empty() || u.chars().count() > 256 || u.chars().any(|c| c.is_control() || matches!(c, '\\' | '/' | '@')) {
                    return Err(ProtoError::new(ErrorCode::InvalidUser, "local account name expected").with_field("user"));
                }
                self.platform.user_picture(u).map(ResponseBody::UserPicture)
            }
            RequestBody::CleanupAudioEndpoints { confirm } => {
                if !confirm {
                    return Err(ProtoError::new(ErrorCode::BadRequest, "confirm must be true").with_field("confirm"));
                }
                if self.platform.passive() {
                    return Err(passive_err());
                }
                if self.runtime.values().any(|r| r.state == SeatState::Booting) {
                    return Err(ProtoError::new(ErrorCode::SeatRunning, "seat_booting: wait until every seat finished starting")
                        .with_reason("seat_booting"));
                }
                self.platform.cleanup_audio_endpoints().map(ResponseBody::AudioCleanup)
            }
            RequestBody::RebootHost { confirm } => {
                if !confirm {
                    return Err(ProtoError::new(ErrorCode::BadRequest, "confirm must be true").with_field("confirm"));
                }
                self.stop_all();
                self.platform.reboot_host().map(|_| ResponseBody::Ack)
            }
            RequestBody::EnableUnlockAccount { name, enable, unlock, confirm } => {
                if !confirm {
                    return Err(ProtoError::new(ErrorCode::BadRequest, "confirm must be true").with_field("confirm"));
                }
                let seat = self.seat(&name)?;
                self.enable_unlock_user(&seat.user_name, enable, unlock).map(|_| ResponseBody::Ack)
            }
        }
    }

    fn config(&self) -> Result<seat_core::remote::store::Config, ProtoError> {
        self.store.load().map_err(store_err)
    }

    fn seat(&self, name: &str) -> Result<RemoteSeatRecord, ProtoError> {
        self.config()?.seat(name).cloned().ok_or_else(|| not_found(name))
    }

    /// Seat whose backend is Pulsar (`unsupported` / `backend_sunshine` otherwise);
    /// `running` = it must also be running.
    fn pulsar_seat(&self, name: &str, running: bool) -> Result<RemoteSeatRecord, ProtoError> {
        let seat = if running { self.running_seat(name)? } else { self.seat(name)? };
        if seat.stream_backend != StreamBackendKind::Pulsar {
            return Err(wrong_backend(seat.stream_backend));
        }
        Ok(seat)
    }

    fn running_seat(&self, name: &str) -> Result<RemoteSeatRecord, ProtoError> {
        let seat = self.seat(name)?;
        if !self.rt(&seat.id).is_some_and(|r| r.state == SeatState::Running) {
            return Err(ProtoError::new(ErrorCode::ServiceUnavailable, "seat is not running"));
        }
        Ok(seat)
    }

    fn rt(&self, id: &str) -> Option<&Runtime> {
        self.runtime.get(id)
    }

    fn rt_mut(&mut self, id: &str) -> &mut Runtime {
        self.runtime.entry(id.to_string()).or_default()
    }

    fn ensure_stopped(&self, seat: &RemoteSeatRecord) -> Result<(), ProtoError> {
        if self.rt(&seat.id).is_some_and(Runtime::is_active) {
            return Err(ProtoError::new(ErrorCode::SeatRunning, "stop the seat before changing it"));
        }
        Ok(())
    }

    pub fn status(&self) -> Result<ServiceStatus, ProtoError> {
        let cfg = self.config()?;
        let seats = cfg
            .seats
            .iter()
            .map(|s| {
                let rt = self.rt(&s.id);
                SeatStatus {
                    name: s.name.clone(),
                    display_name: s.display_name.clone(),
                    state: rt.map_or(SeatState::Stopped, |r| r.state),
                    port: s.port,
                    user_name: s.user_name.clone(),
                    has_password: self.store.has_password(&s.name),
                    crash_count: rt.map_or(0, |r| r.crash_count),
                    last_error: rt.and_then(|r| r.last_error.clone()),
                    failure: rt.and_then(|r| r.failure.clone()),
                    session_id: rt.and_then(|r| r.session_id),
                    phase: rt.and_then(|r| r.phase.clone()),
                    streaming: rt.is_some_and(|r| r.streaming),
                    stream_client: rt.and_then(|r| r.stream_client.clone()),
                }
            })
            .collect();
        Ok(ServiceStatus {
            service_version: self.service_version.clone(),
            protocol_version: PROTOCOL_VERSION,
            console_mode: self.console_mode,
            seats,
            passive: self.platform.passive(),
            host_class: self.platform.host_class(),
        })
    }

    fn create_seat(&mut self, new: NewSeat) -> Result<RemoteSeatRecord, ProtoError> {
        validate::seat_name(&new.name)?;
        let cfg = self.config()?;
        let port = match new.port {
            Some(p) => p,
            None => {
                let taken: Vec<u16> = cfg.seats.iter().map(|s| s.port).collect();
                ports::allocate(&taken, &self.platform.listening_ports())
                    .ok_or_else(|| ProtoError::new(ErrorCode::NoFreePort, "no free port block left"))?
            }
        };
        let id = new_id();
        let taken: Vec<String> = cfg.seats.iter().map(|s| s.rdp_client_name.clone()).collect();
        let seat = RemoteSeatRecord {
            rdp_client_name: self.platform.client_name_for(&id, &taken),
            id,
            name: new.name.clone(),
            display_name: new.name,
            port,
            start_with_service: true,
            ..Default::default()
        };
        validate::seat_against(&seat, &cfg.seats)?;
        self.store.put_seat(&seat).map_err(store_err)?;
        Ok(seat)
    }

    fn update_seat(
        &mut self,
        name: &str,
        mut seat: RemoteSeatRecord,
        stream_backend: Option<StreamBackendKind>,
    ) -> Result<RemoteSeatRecord, ProtoError> {
        let cfg = self.config()?;
        let current = cfg.seat(name).cloned().ok_or_else(|| not_found(name))?;
        self.ensure_stopped(&current)?;
        if !seat.id.is_empty() && seat.id != current.id {
            return Err(ProtoError::new(ErrorCode::BadRequest, "seat id cannot change").with_field("id"));
        }
        seat.id = current.id.clone();
        // Service-owned: the manager cannot change the RDP client name or the last mode.
        seat.rdp_client_name = current.rdp_client_name.clone();
        seat.last_mode = current.last_mode;
        // A request without the field (older client) must not reset the backend silently.
        seat.stream_backend = stream_backend.unwrap_or(current.stream_backend);
        for app in &mut seat.autostart {
            if app.id.is_empty() {
                app.id = new_id();
            }
        }
        let others: Vec<RemoteSeatRecord> = cfg.seats.iter().filter(|s| s.id != current.id).cloned().collect();
        validate::seat_against(&seat, &others)?;
        if seat.name != current.name {
            self.store.rename_seat(&current.name, &seat.name).map_err(store_err)?;
        }
        self.store.put_seat(&seat).map_err(store_err)?;
        Ok(seat)
    }

    /// Whether the seat with this id is booting, running or stopping.
    pub fn seat_active(&self, id: &str) -> bool {
        self.rt(id).is_some_and(Runtime::is_active)
    }

    /// A seat deleted from the config document (`apply_config` removed it; the record is
    /// the old one): refuse while it is active, else remove its stream backend secrets,
    /// files and firewall rules (honouring `keep_pairing_on_delete`) and drop its runtime.
    /// Split's `delete_seat` without the store write, which `apply_config` already did.
    pub fn forget_seat(&mut self, seat: &RemoteSeatRecord) -> Result<(), ProtoError> {
        self.ensure_stopped(seat)?;
        self.platform.remove_seat_files(seat);
        self.runtime.remove(&seat.id);
        Ok(())
    }

    /// Spec 10 §4.4 with "ask first": the test never enables or unlocks the account; it
    /// reports the state so the manager can offer `enable_unlock_account`.
    fn test_credentials(&mut self, name: &str) -> Result<CredentialTest, ProtoError> {
        let seat = self.seat(name)?;
        if seat.user_name.is_empty() || !self.store.has_password(&seat.name) {
            return Err(ProtoError::new(ErrorCode::MissingCredentials, "seat needs a user and a password").with_field("user_name"));
        }
        let password = self.store.password(&seat.name).map_err(store_err)?;
        self.test_user_credentials(&seat.user_name, password)
    }

    /// The logon test for any user (a local seat's user, merge step 6): same checks and
    /// answer as `test_seat_credentials`, with the password the caller read.
    pub fn test_user_credentials(&mut self, user: &str, password: Option<Secret>) -> Result<CredentialTest, ProtoError> {
        if user.is_empty() {
            return Err(ProtoError::new(ErrorCode::MissingCredentials, "seat needs a user and a password").with_field("user_name"));
        }
        let facts = self.platform.user_facts(user);
        if !facts.exists {
            return Err(ProtoError::new(ErrorCode::InvalidUser, "user does not exist").with_field("user_name"));
        }
        let password =
            password.ok_or_else(|| ProtoError::new(ErrorCode::MissingCredentials, "no stored password").with_field("password"))?;
        let outcome = self.platform.test_logon(user, &password)?;
        drop(password);
        let mut t = CredentialTest {
            ok: outcome.is_ok(),
            disabled: !facts.enabled,
            locked_out: facts.locked_out,
            password_expired: facts.password_expired,
            ..Default::default()
        };
        if let Err(code) = outcome {
            t.error_code = Some(code);
            t.message = Some(self.platform.win32_message(code));
        }
        Ok(t)
    }

    /// Enable and / or unlock `user` (the operator confirmed; any seat kind).
    pub fn enable_unlock_user(&mut self, user: &str, enable: bool, unlock: bool) -> Result<(), ProtoError> {
        if user.is_empty() {
            return Err(ProtoError::new(ErrorCode::MissingCredentials, "seat has no user").with_field("user_name"));
        }
        if !self.platform.user_facts(user).exists {
            return Err(ProtoError::new(ErrorCode::InvalidUser, "user does not exist").with_field("user_name"));
        }
        if self.platform.passive() {
            return Err(passive_err());
        }
        self.platform.enable_unlock_account(user, enable, unlock)
    }

    fn set_password(&mut self, name: &str, password: &Secret) -> Result<(), ProtoError> {
        let seat = self.seat(name)?;
        if password.0.is_empty() {
            return Err(ProtoError::new(ErrorCode::InvalidValue, "password must not be empty").with_field("password"));
        }
        self.store.set_password(&seat.name, password).map_err(store_err)
    }

    fn start_seat(&mut self, name: &str) -> Result<(), ProtoError> {
        let cfg = self.config()?;
        let mut seat = cfg.seat(name).cloned().ok_or_else(|| not_found(name))?;
        if self.rt(&seat.id).is_some_and(Runtime::is_active) {
            return Ok(()); // already up: nothing to do
        }
        if self.platform.passive() {
            return Err(passive_err());
        }
        // Seats created by protocol v1 have no client name yet.
        if seat.rdp_client_name.is_empty() {
            let taken: Vec<String> = cfg.seats.iter().map(|s| s.rdp_client_name.clone()).collect();
            seat.rdp_client_name = self.platform.client_name_for(&seat.id, &taken);
            self.store.put_seat(&seat).map_err(store_err)?;
        }
        let result = (|| {
            validate::seat_alone(&seat)?;
            let facts = self.platform.user_facts(&seat.user_name);
            let console = self.platform.console_user();
            validate::user_for_start(&seat, self.store.has_password(&seat.name), facts, console.as_deref())?;
            let password = self.store.password(&seat.name).map_err(store_err)?;
            self.platform.start_seat(&seat, &cfg.global, password)
        })();
        let rt = self.rt_mut(&seat.id);
        match result {
            Ok(()) => {
                *rt = Runtime { state: SeatState::Booting, ..Default::default() };
                self.push_event(Some(&seat.name), "seat_phase", serde_json::json!({"state": "booting"}));
                Ok(())
            }
            Err(e) => {
                let e = e.with_reason_from_message();
                rt.state = SeatState::Stopped;
                rt.failure = Some(e.reason_or_code());
                rt.last_error = Some(e.message.clone());
                Err(e)
            }
        }
    }

    fn stop_seat(&mut self, name: &str) -> Result<(), ProtoError> {
        let seat = self.seat(name)?;
        if !self.rt(&seat.id).is_some_and(Runtime::is_active) {
            let rt = self.rt_mut(&seat.id);
            rt.state = SeatState::Stopped;
            rt.phase = None;
            rt.session_id = None;
            return Ok(());
        }
        if self.rt(&seat.id).is_some_and(|r| r.state == SeatState::Stopping) {
            return Ok(());
        }
        let result = self.platform.stop_seat(&seat);
        let rt = self.rt_mut(&seat.id);
        match &result {
            Ok(()) => rt.state = SeatState::Stopping,
            Err(e) => {
                rt.state = SeatState::Stopped;
                rt.last_error = Some(e.message.clone());
            }
        }
        self.push_event(Some(&seat.name), "seat_phase", serde_json::json!({"state": "stopping"}));
        result
    }

    fn set_global(&mut self, g: RemoteSettings) -> Result<RemoteSettings, ProtoError> {
        validate::global(&g)?;
        let old = self.config()?.global;
        let isolation_changed = old.process_patching != g.process_patching
            || old.hid_isolation != g.hid_isolation
            || old.steam_isolation != g.steam_isolation;
        if isolation_changed && self.platform.passive() {
            return Err(passive_err());
        }
        // `multi_session` is changed only through `set_multi_session`.
        let mut g = g;
        g.multi_session = old.multi_session;
        self.platform.apply_global(&old, &g)?;
        self.store.put_global(&g).map_err(store_err)?;
        Ok(g)
    }

    fn set_multi_session(&mut self, enabled: bool) -> Result<HostInfo, ProtoError> {
        if self.platform.passive() {
            return Err(passive_err());
        }
        if !enabled {
            // Restoring the RDS settings drops seat sessions; stop them first.
            self.stop_all();
        }
        self.platform.set_multi_session(enabled)?;
        let mut g = self.config()?.global;
        g.multi_session = enabled;
        self.store.put_global(&g).map_err(store_err)?;
        self.platform.host_info(&g)
    }

    fn seat_runtime(&mut self, name: &str) -> Result<SeatRuntime, ProtoError> {
        let seat = self.seat(name)?;
        let mut r = self.platform.seat_runtime(&seat)?;
        if r.client_name.is_empty() {
            r.client_name = seat.rdp_client_name.clone();
        }
        if let Some(rt) = self.rt(&seat.id) {
            r.state = rt.state;
            r.phase = rt.phase.clone();
            r.failure = rt.failure.clone();
            r.session_id = r.session_id.or(rt.session_id);
        }
        Ok(r)
    }

    fn push_event(&mut self, seat: Option<&str>, kind: &str, data: serde_json::Value) {
        let ev = ServiceEvent { seq: self.next_seq, time: now_ms(), seat: seat.map(str::to_owned), kind: kind.into(), data };
        self.next_seq += 1;
        self.events.push_back(ev);
        while self.events.len() > EVENT_RING {
            self.events.pop_front();
        }
    }

    /// Events with `seq > after` (oldest first) and the cursor for the next call.
    pub fn events_after(&self, after: u64) -> Events {
        let events: Vec<ServiceEvent> = self.events.iter().filter(|e| e.seq > after).cloned().collect();
        Events { next: self.next_seq - 1, events }
    }

    /// `true` when an event with `seq > after` exists (used by the waiting `poll_events`).
    // Split's own pipe is gone (merge step 6); the tests still drive the controller this way.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn has_events_after(&self, after: u64) -> bool {
        self.next_seq - 1 > after
    }

    /// Apply the platform's progress reports. Called after every request and on the tick.
    pub fn drain_reports(&mut self) {
        let reports = self.platform.take_reports();
        if reports.is_empty() {
            return;
        }
        let names: HashMap<String, String> = self
            .config()
            .map(|c| c.seats.into_iter().map(|s| (s.id, s.name)).collect())
            .unwrap_or_default();
        for (id, ev) in reports {
            let name = names.get(&id).cloned();
            self.report(&id, name.as_deref(), ev);
        }
    }

    fn report(&mut self, id: &str, name: Option<&str>, ev: SeatEvent) {
        let rt = self.rt_mut(id);
        let event = match ev {
            SeatEvent::Phase(p) => {
                if rt.state != SeatState::Booting {
                    return; // stale worker output after a stop
                }
                rt.phase = Some(p.clone());
                ("seat_phase", serde_json::json!({"state": "booting", "phase": p}))
            }
            SeatEvent::Running { session_id } => {
                if rt.state != SeatState::Booting {
                    return;
                }
                rt.state = SeatState::Running;
                rt.phase = None;
                rt.session_id = Some(session_id);
                ("seat_phase", serde_json::json!({"state": "running", "session_id": session_id}))
            }
            SeatEvent::Failed { reason, message } => {
                rt.state = SeatState::Failed;
                rt.phase = None;
                rt.session_id = None;
                rt.failure = Some(reason.clone());
                rt.last_error = Some(message);
                rt.streaming = false;
                rt.stream_client = None;
                ("seat_failed", serde_json::json!({"reason": reason}))
            }
            SeatEvent::Stopped => {
                if rt.state == SeatState::Failed {
                    return; // cleanup after a failure keeps the failure visible
                }
                rt.state = SeatState::Stopped;
                rt.phase = None;
                rt.session_id = None;
                rt.streaming = false;
                rt.stream_client = None;
                ("seat_phase", serde_json::json!({"state": "stopped"}))
            }
            SeatEvent::Crashes(n) => {
                rt.crash_count = n;
                return;
            }
            SeatEvent::Session(s) => {
                rt.session_id = Some(s);
                return;
            }
            SeatEvent::ModeApplied(m) => {
                if !m.is_plausible() {
                    return;
                }
                if let Ok(cfg) = self.config()
                    && let Some(mut seat) = cfg.seats.into_iter().find(|s| s.id == id)
                    && seat.last_mode != Some(m)
                {
                    seat.last_mode = Some(m);
                    if let Err(e) = self.store.put_seat(&seat) {
                        self.rt_mut(id).last_error = Some(format!("last mode not stored: {e}"));
                    }
                }
                return;
            }
            SeatEvent::Notice { kind, data } => {
                match kind.as_str() {
                    "stream_started" => {
                        rt.streaming = true;
                        rt.stream_client = stream_client_name(&data);
                    }
                    "stream_ended" => {
                        rt.streaming = false;
                        rt.stream_client = None;
                    }
                    _ => {}
                }
                let kind: &'static str = match kind.as_str() {
                    "pair_pending" => "pair_pending",
                    "pair_ended" => "pair_ended",
                    "stream_started" => "stream_started",
                    "stream_ended" => "stream_ended",
                    "relay_auth_required" => "relay_auth_required",
                    "otp_changed" => "otp_changed",
                    "trust_changed" => "trust_changed",
                    _ => "host_warning",
                };
                (kind, data)
            }
        };
        self.push_event(name, event.0, event.1);
    }

    /// Seats marked "start with service", for the boot sequence.
    pub fn autostart_seat_names(&self) -> Vec<String> {
        self.config()
            .map(|c| c.seats.into_iter().filter(|s| s.start_with_service).map(|s| s.name).collect())
            .unwrap_or_default()
    }

    /// Ask every active seat to stop (service shutdown, termwrap switch).
    pub fn stop_all(&mut self) {
        let names: Vec<String> = self.config().map(|c| c.seats.into_iter().map(|s| s.name).collect()).unwrap_or_default();
        for n in names {
            let _ = self.stop_seat(&n);
        }
        self.drain_reports();
    }

    /// Power suspend (spec 20 §7, spec 30 §10): ask every active seat to stop. Returns the
    /// names of the seats that were active.
    pub fn on_suspend(&mut self) -> Vec<String> {
        let active: Vec<String> = self
            .config()
            .map(|c| c.seats.into_iter().filter(|s| self.rt(&s.id).is_some_and(Runtime::is_active)).map(|s| s.name).collect())
            .unwrap_or_default();
        self.stop_all();
        active
    }

    /// Seats to start after resume: the enabled ones (`start_with_service`).
    // Split's own pipe is gone (merge step 6); the tests still drive the controller this way.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn resume_seat_names(&self) -> Vec<String> {
        self.autostart_seat_names()
    }

    /// Seats still booting, running or stopping.
    pub fn active_seats(&self) -> usize {
        self.runtime.values().filter(|r| r.is_active()).count()
    }

    /// User names of all configured seats (session sweep).
    pub fn seat_users(&self) -> Vec<String> {
        self.config()
            .map(|c| c.seats.into_iter().map(|s| s.user_name).filter(|u| !u.is_empty()).collect())
            .unwrap_or_default()
    }

    /// Handle a request from inside the service (boot sequence) the same way as a pipe call.
    pub fn call(&mut self, body: RequestBody) -> Result<ResponseBody, ProtoError> {
        let r = self.dispatch(body).map_err(ProtoError::with_reason_from_message);
        self.drain_reports();
        r
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use seat_core::remote::store::MemoryStore;
    use seat_proto::remote::AutostartApp;

    #[derive(Default)]
    pub(crate) struct FakePlatform {
        pub(crate) started: Vec<String>,
        pub(crate) passwords: Vec<Option<String>>,
        pub(crate) stopped: Vec<String>,
        pub(crate) removed: Vec<String>,
        pub(crate) fail_start: bool,
        pub(crate) console: Option<String>,
        pub(crate) passive: bool,
        pub(crate) reports: Vec<(String, SeatEvent)>,
        pub(crate) pins: Vec<Option<u32>>,
        pub(crate) listening: Vec<u16>,
        pub(crate) unlocked: Vec<(String, bool, bool)>,
        pub(crate) rebooted: bool,
        /// Ids of removed seats, shared with a test that boxed the controller.
        pub(crate) removed_ids: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl Platform for FakePlatform {
        fn start_seat(&mut self, seat: &RemoteSeatRecord, _: &RemoteSettings, pw: Option<Secret>) -> Result<(), ProtoError> {
            if self.fail_start {
                return Err(ProtoError::new(ErrorCode::ServiceUnavailable, "rds_not_ready: no sessions yet"));
            }
            self.started.push(seat.name.clone());
            self.passwords.push(pw.map(|p| p.0.clone()));
            Ok(())
        }
        fn stop_seat(&mut self, seat: &RemoteSeatRecord) -> Result<(), ProtoError> {
            self.stopped.push(seat.name.clone());
            self.reports.push((seat.id.clone(), SeatEvent::Stopped));
            Ok(())
        }
        fn remove_seat_files(&mut self, s: &RemoteSeatRecord) {
            self.removed.push(s.name.clone());
            self.removed_ids.lock().unwrap().push(s.id.clone());
        }
        fn submit_pin(&mut self, _: &RemoteSeatRecord, _: &Secret, _: &str, r: Option<u32>) -> Result<(), ProtoError> {
            self.pins.push(r);
            Ok(())
        }
        fn apply_global(&mut self, _: &RemoteSettings, _: &RemoteSettings) -> Result<(), ProtoError> {
            Ok(())
        }
        fn list_local_users(&self) -> Result<Vec<LocalUser>, ProtoError> {
            Ok(vec![LocalUser { name: "ali".into(), enabled: true }])
        }
        fn user_facts(&self, user: &str) -> UserFacts {
            UserFacts { exists: user == "ali" || user == "veli", enabled: true, ..Default::default() }
        }
        fn console_user(&self) -> Option<String> {
            self.console.clone()
        }
        fn take_reports(&mut self) -> Vec<(String, SeatEvent)> {
            std::mem::take(&mut self.reports)
        }
        fn passive(&self) -> bool {
            self.passive
        }
        fn listening_ports(&self) -> Vec<u16> {
            self.listening.clone()
        }
        fn test_logon(&self, user: &str, pw: &Secret) -> Result<Result<(), u32>, ProtoError> {
            Ok(if user == "ali" && pw.0 == "pw" { Ok(()) } else { Err(1326) })
        }
        fn enable_unlock_account(&mut self, user: &str, enable: bool, unlock: bool) -> Result<(), ProtoError> {
            self.unlocked.push((user.into(), enable, unlock));
            Ok(())
        }
        fn log_tail(&mut self, seat: Option<&RemoteSeatRecord>, n: usize) -> Result<Vec<String>, ProtoError> {
            Ok(vec![format!("{}:{n}", seat.map_or("service", |s| s.name.as_str()))])
        }
        fn reboot_host(&mut self) -> Result<(), ProtoError> {
            self.rebooted = true;
            Ok(())
        }
    }

    fn ctl() -> Controller<MemoryStore, FakePlatform> {
        Controller::new(MemoryStore::new(), FakePlatform::default(), "0.1.0", true)
    }

    fn call(c: &mut Controller<MemoryStore, FakePlatform>, body: RequestBody) -> Result<ResponseBody, ProtoError> {
        let resp = c.handle(Request::new(1, body));
        assert_eq!(resp.id, 1);
        resp.result
    }

    fn create(c: &mut Controller<MemoryStore, FakePlatform>, name: &str) -> RemoteSeatRecord {
        match call(c, RequestBody::CreateSeat(NewSeat { name: name.into(), port: None })).unwrap() {
            ResponseBody::Seat(s) => s,
            other => panic!("{other:?}"),
        }
    }

    fn ready(c: &mut Controller<MemoryStore, FakePlatform>, name: &str, user: &str) -> RemoteSeatRecord {
        let mut s = create(c, name);
        s.user_name = user.into();
        call(c, RequestBody::UpdateSeat { name: name.into(), seat: s.clone(), stream_backend: None }).unwrap();
        call(c, RequestBody::SetSeatPassword { name: name.into(), password: Secret("pw".into()) }).unwrap();
        s
    }

    fn code(r: Result<ResponseBody, ProtoError>) -> ErrorCode {
        r.unwrap_err().code
    }

    #[test]
    fn version_mismatch_rejected() {
        let mut c = ctl();
        let mut req = Request::new(9, RequestBody::Hello);
        req.version = 1;
        assert_eq!(c.handle(req).result.unwrap_err().code, ErrorCode::VersionMismatch);
    }

    #[test]
    fn create_allocates_ports_client_names_and_rejects_duplicates() {
        let mut c = ctl();
        let a = create(&mut c, "Salon");
        let b = create(&mut c, "Oda");
        assert_eq!((a.port, b.port), (48100, 48200));
        assert!(a.start_with_service);
        assert_eq!(a.display_name, "Salon");
        assert!(!a.id.is_empty() && a.id != b.id);
        validate::rdp_client_name(&a.rdp_client_name).unwrap();
        assert!(a.rdp_client_name.starts_with("PSS") && a.rdp_client_name != b.rdp_client_name);
        assert_eq!(code(call(&mut c, RequestBody::CreateSeat(NewSeat { name: "salon".into(), port: None }))), ErrorCode::DuplicateName);
        assert_eq!(code(call(&mut c, RequestBody::CreateSeat(NewSeat { name: "x y".into(), port: None }))), ErrorCode::InvalidName);
        assert_eq!(
            code(call(&mut c, RequestBody::CreateSeat(NewSeat { name: "c".into(), port: Some(48100) }))),
            ErrorCode::PortOverlap
        );
    }

    #[test]
    fn create_skips_blocks_with_listening_ports() {
        let mut c = ctl();
        c.platform_mut().listening = vec![48105, 48215];
        assert_eq!(create(&mut c, "A").port, 48300);
    }

    #[test]
    fn display_names_must_be_unique_on_create_and_update() {
        let mut c = ctl();
        let mut a = create(&mut c, "A");
        create(&mut c, "B");
        // Create: the new seat's display name defaults to its name.
        a.display_name = "c".into();
        call(&mut c, RequestBody::UpdateSeat { name: "A".into(), seat: a, stream_backend: None }).unwrap();
        let e = call(&mut c, RequestBody::CreateSeat(NewSeat { name: "C".into(), port: None })).unwrap_err();
        assert_eq!((e.code, e.field.as_deref()), (ErrorCode::DuplicateName, Some("display_name")));
        // Update: renaming B's display name onto A's is refused.
        let mut b = match call(&mut c, RequestBody::GetSeat { name: "B".into() }).unwrap() {
            ResponseBody::Seat(s) => s,
            o => panic!("{o:?}"),
        };
        b.display_name = "C".into();
        let e = call(&mut c, RequestBody::UpdateSeat { name: "B".into(), seat: b, stream_backend: None }).unwrap_err();
        assert_eq!((e.code, e.field.as_deref()), (ErrorCode::DuplicateName, Some("display_name")));
    }

    #[test]
    fn applied_mode_is_persisted_and_service_owned() {
        let mut c = ctl();
        let a = create(&mut c, "A");
        let m = seat_proto::remote::LastMode { width: 2560, height: 1440, hz: 120 };
        c.platform_mut().reports.push((a.id.clone(), SeatEvent::ModeApplied(m)));
        c.platform_mut().reports.push((a.id.clone(), SeatEvent::ModeApplied(seat_proto::remote::LastMode { width: 1, height: 1, hz: 1 })));
        c.drain_reports();
        let got = match call(&mut c, RequestBody::GetSeat { name: "A".into() }).unwrap() {
            ResponseBody::Seat(s) => s,
            o => panic!("{o:?}"),
        };
        assert_eq!(got.last_mode, Some(m), "implausible modes are ignored");
        // The manager cannot overwrite it.
        let mut edit = got.clone();
        edit.last_mode = None;
        let saved = match call(&mut c, RequestBody::UpdateSeat { name: "A".into(), seat: edit, stream_backend: None }).unwrap() {
            ResponseBody::Seat(s) => s,
            o => panic!("{o:?}"),
        };
        assert_eq!(saved.last_mode, Some(m));
    }

    #[test]
    fn log_tail_sources_and_reboot_confirm() {
        let mut c = ctl();
        create(&mut c, "A");
        let lines = |r| match r {
            Ok(ResponseBody::LogLines(l)) => l,
            o => panic!("{o:?}"),
        };
        assert_eq!(lines(call(&mut c, RequestBody::GetLogTail { source: "service".into(), lines: 0 })), ["service:1"]);
        assert_eq!(lines(call(&mut c, RequestBody::GetLogTail { source: "seat:a".into(), lines: 5000 })), ["A:1000"]);
        assert_eq!(code(call(&mut c, RequestBody::GetLogTail { source: "seat:zz".into(), lines: 5 })), ErrorCode::NotFound);
        assert_eq!(code(call(&mut c, RequestBody::GetLogTail { source: "other".into(), lines: 5 })), ErrorCode::InvalidValue);
        assert_eq!(code(call(&mut c, RequestBody::RebootHost { confirm: false })), ErrorCode::BadRequest);
        assert!(!c.platform().rebooted);
        call(&mut c, RequestBody::RebootHost { confirm: true }).unwrap();
        assert!(c.platform().rebooted);
    }

    #[test]
    fn suspend_stops_active_seats_and_resume_lists_enabled_ones() {
        let mut c = ctl();
        ready(&mut c, "A", "ali");
        let mut b = ready(&mut c, "B", "veli");
        b.start_with_service = false;
        call(&mut c, RequestBody::UpdateSeat { name: "B".into(), seat: b, stream_backend: None }).unwrap();
        call(&mut c, RequestBody::StartSeat { name: "A".into() }).unwrap();
        assert_eq!(c.on_suspend(), vec!["A".to_string()]);
        assert_eq!(c.platform().stopped, vec!["A".to_string()]);
        assert_eq!(c.active_seats(), 0, "fake stop reports Stopped at once");
        assert_eq!(c.resume_seat_names(), vec!["A".to_string()], "only enabled seats restart");
    }

    #[test]
    fn credential_test_reports_and_never_changes_the_account() {
        let mut c = ctl();
        create(&mut c, "A");
        let e = call(&mut c, RequestBody::TestSeatCredentials { name: "A".into() }).unwrap_err();
        assert_eq!(e.code, ErrorCode::MissingCredentials);
        ready(&mut c, "B", "ali");
        let t = match call(&mut c, RequestBody::TestSeatCredentials { name: "B".into() }).unwrap() {
            ResponseBody::CredentialTest(t) => t,
            o => panic!("{o:?}"),
        };
        assert!(t.ok && t.error_code.is_none() && !t.disabled);
        call(&mut c, RequestBody::SetSeatPassword { name: "B".into(), password: Secret("bad".into()) }).unwrap();
        let t = match call(&mut c, RequestBody::TestSeatCredentials { name: "B".into() }).unwrap() {
            ResponseBody::CredentialTest(t) => t,
            o => panic!("{o:?}"),
        };
        assert_eq!((t.ok, t.error_code), (false, Some(1326)));
        assert!(t.message.is_some());
        assert!(c.platform().unlocked.is_empty(), "the test itself never enables / unlocks");
        // Enable / unlock only with confirm.
        assert_eq!(
            code(call(&mut c, RequestBody::EnableUnlockAccount { name: "B".into(), enable: true, unlock: true, confirm: false })),
            ErrorCode::BadRequest
        );
        call(&mut c, RequestBody::EnableUnlockAccount { name: "B".into(), enable: true, unlock: false, confirm: true }).unwrap();
        assert_eq!(c.platform().unlocked, vec![("ali".to_string(), true, false)]);
    }

    #[test]
    fn default_client_names_are_unique_on_collision() {
        let p = FakePlatform::default();
        let first = p.client_name_for("0f8fad5b-d9cb-469f-a165-70867728950e", &[]);
        assert_eq!(first, "PSS0F8FAD5B");
        let second = p.client_name_for("0f8fad5b-d9cb-469f-a165-70867728950e", std::slice::from_ref(&first));
        assert_eq!(second, "PSS0F8FAD5C");
    }

    #[test]
    fn update_validates_renames_and_keeps_id_and_client_name() {
        let mut c = ctl();
        let a = create(&mut c, "A");
        create(&mut c, "B");
        let mut edit = a.clone();
        edit.name = "Salon".into();
        edit.display_name = "Salon PC".into();
        edit.user_name = "ali".into();
        edit.rdp_client_name = "PSS00000000".into();
        edit.autostart.push(AutostartApp { path: r"C:\x.exe".into(), ..Default::default() });
        let got = match call(&mut c, RequestBody::UpdateSeat { name: "a".into(), seat: edit, stream_backend: None }).unwrap() {
            ResponseBody::Seat(s) => s,
            o => panic!("{o:?}"),
        };
        assert_eq!(got.id, a.id);
        assert_eq!(got.rdp_client_name, a.rdp_client_name);
        assert!(!got.autostart[0].id.is_empty());
        assert!(matches!(call(&mut c, RequestBody::GetSeat { name: "A".into() }), Err(e) if e.code == ErrorCode::NotFound));
        let mut b = match call(&mut c, RequestBody::GetSeat { name: "B".into() }).unwrap() {
            ResponseBody::Seat(s) => s,
            o => panic!("{o:?}"),
        };
        b.user_name = "ALI".into();
        assert_eq!(code(call(&mut c, RequestBody::UpdateSeat { name: "B".into(), seat: b.clone(), stream_backend: None })), ErrorCode::UserInUse);
        b.user_name.clear();
        b.id = "other".into();
        assert_eq!(code(call(&mut c, RequestBody::UpdateSeat { name: "B".into(), seat: b, stream_backend: None })), ErrorCode::BadRequest);
    }

    #[test]
    fn streaming_notices_show_in_status_and_reset_on_stop() {
        let mut c = ctl();
        let mut s = create(&mut c, "A");
        s.user_name = "ali".into();
        call(&mut c, RequestBody::UpdateSeat { name: "A".into(), seat: s.clone(), stream_backend: None }).unwrap();
        call(&mut c, RequestBody::SetSeatPassword { name: "A".into(), password: Secret("pw".into()) }).unwrap();
        call(&mut c, RequestBody::StartSeat { name: "A".into() }).unwrap();
        // Cleanup is refused while a seat boots, and needs confirm.
        assert_eq!(code(call(&mut c, RequestBody::CleanupAudioEndpoints { confirm: false })), ErrorCode::BadRequest);
        let e = call(&mut c, RequestBody::CleanupAudioEndpoints { confirm: true }).unwrap_err();
        assert_eq!(e.reason.as_deref(), Some("seat_booting"));
        c.platform_mut().reports.push((s.id.clone(), SeatEvent::Running { session_id: 3 }));
        let notice = |client: serde_json::Value| SeatEvent::Notice { kind: "stream_started".into(), data: client };
        c.platform_mut().reports.push((s.id.clone(), notice(serde_json::json!({"app": null, "client": "Salon TV"}))));
        c.drain_reports();
        let st = &c.status().unwrap().seats[0];
        assert!(st.streaming);
        assert_eq!(st.stream_client.as_deref(), Some("Salon TV"));
        c.platform_mut().reports.push((s.id.clone(), notice(serde_json::json!({"client": "bad\u{7}"}))));
        c.drain_reports();
        assert_eq!(c.status().unwrap().seats[0].stream_client, None);
        c.platform_mut().reports.push((s.id.clone(), SeatEvent::Notice { kind: "stream_ended".into(), data: serde_json::json!({}) }));
        c.drain_reports();
        assert!(!c.status().unwrap().seats[0].streaming);
        c.platform_mut().reports.push((s.id.clone(), notice(serde_json::json!({}))));
        c.platform_mut().reports.push((s.id.clone(), SeatEvent::Stopped));
        c.drain_reports();
        assert!(!c.status().unwrap().seats[0].streaming);
        // Default platform: no cleanup, no DXVK.
        assert_eq!(code(call(&mut c, RequestBody::CleanupAudioEndpoints { confirm: true })), ErrorCode::Unsupported);
        assert_eq!(code(call(&mut c, RequestBody::GetDxvkStatus)), ErrorCode::Unsupported);
    }

    #[test]
    fn user_picture_validates_the_name() {
        let mut c = ctl();
        for bad in ["", "  ", r"a\b", "x/y", "u@domain", "a\u{1}"] {
            assert_eq!(code(call(&mut c, RequestBody::GetUserPicture { user: bad.into() })), ErrorCode::InvalidUser, "{bad:?}");
        }
        assert_eq!(
            call(&mut c, RequestBody::GetUserPicture { user: "ali".into() }).unwrap(),
            ResponseBody::UserPicture(seat_proto::remote::UserPicture::default())
        );
    }

    #[test]
    fn async_start_reports_and_locks_config() {
        let mut c = ctl();
        let mut s = create(&mut c, "A");
        assert_eq!(code(call(&mut c, RequestBody::StartSeat { name: "A".into() })), ErrorCode::MissingCredentials);
        s.user_name = "ali".into();
        call(&mut c, RequestBody::UpdateSeat { name: "A".into(), seat: s.clone(), stream_backend: None }).unwrap();
        assert_eq!(
            code(call(&mut c, RequestBody::SetSeatPassword { name: "A".into(), password: Secret(String::new()) })),
            ErrorCode::InvalidValue
        );
        call(&mut c, RequestBody::SetSeatPassword { name: "A".into(), password: Secret("pw".into()) }).unwrap();
        call(&mut c, RequestBody::StartSeat { name: "A".into() }).unwrap();
        assert_eq!(c.platform().started, ["A"]);
        assert_eq!(c.platform().passwords, [Some("pw".to_string())]);
        // Idempotent start.
        call(&mut c, RequestBody::StartSeat { name: "a".into() }).unwrap();
        assert_eq!(c.platform().started.len(), 1);
        assert_eq!(c.status().unwrap().seats[0].state, SeatState::Booting);
        // Progress from the worker.
        c.platform_mut().reports.push((s.id.clone(), SeatEvent::Phase("session".into())));
        c.drain_reports();
        assert_eq!(c.status().unwrap().seats[0].phase.as_deref(), Some("session"));
        c.platform_mut().reports.push((s.id.clone(), SeatEvent::Running { session_id: 3 }));
        c.drain_reports();
        let st = c.status().unwrap();
        assert_eq!((st.seats[0].state, st.seats[0].session_id), (SeatState::Running, Some(3)));
        assert!(st.seats[0].has_password);
        // Locked while running.
        assert_eq!(code(call(&mut c, RequestBody::UpdateSeat { name: "A".into(), seat: s.clone(), stream_backend: None })), ErrorCode::SeatRunning);
        assert!(c.seat_active(&s.id));
        assert_eq!(c.forget_seat(&s).unwrap_err().code, ErrorCode::SeatRunning);
        call(&mut c, RequestBody::StopSeat { name: "A".into() }).unwrap();
        assert_eq!(c.platform().stopped, ["A"]);
        assert_eq!(c.status().unwrap().seats[0].state, SeatState::Stopped);
        c.forget_seat(&s).unwrap();
        assert_eq!(c.platform().removed, ["A"]);
        // The ring saw booting, session, running, stopping, stopped.
        let ev = c.events_after(0);
        let kinds: Vec<_> = ev.events.iter().map(|e| e.data["state"].as_str().unwrap_or("").to_string()).collect();
        assert_eq!(kinds, ["booting", "booting", "running", "stopping", "stopped"]);
        assert_eq!(ev.next, 5);
        assert!(c.events_after(5).events.is_empty());
        assert!(!c.has_events_after(5));
    }

    #[test]
    fn failure_report_keeps_reason_and_needs_new_start() {
        let mut c = ctl();
        let s = ready(&mut c, "A", "ali");
        call(&mut c, RequestBody::StartSeat { name: "A".into() }).unwrap();
        c.platform_mut().reports.push((
            s.id.clone(),
            SeatEvent::Failed { reason: "client_auth".into(), message: "client_auth: NLA failed".into() },
        ));
        c.platform_mut().reports.push((s.id.clone(), SeatEvent::Stopped));
        c.drain_reports();
        let st = &c.status().unwrap().seats[0];
        assert_eq!(st.state, SeatState::Failed);
        assert_eq!(st.failure.as_deref(), Some("client_auth"));
        assert!(c.events_after(0).events.iter().any(|e| e.kind == "seat_failed" && e.seat.as_deref() == Some("A")));
        // A failed seat is not active: config may change, start works again.
        call(&mut c, RequestBody::UpdateSeat { name: "A".into(), seat: s, stream_backend: None }).unwrap();
        call(&mut c, RequestBody::StartSeat { name: "A".into() }).unwrap();
        assert_eq!(c.platform().started.len(), 2);
    }

    #[test]
    fn start_refuses_console_user_unknown_user_and_passive_mode() {
        let mut c = ctl();
        c.platform.console = Some("Ali".into());
        let mut s = ready(&mut c, "A", "ali");
        assert_eq!(code(call(&mut c, RequestBody::StartSeat { name: "A".into() })), ErrorCode::UserInUse);
        s.user_name = "nobody".into();
        call(&mut c, RequestBody::UpdateSeat { name: "A".into(), seat: s, stream_backend: None }).unwrap();
        assert_eq!(code(call(&mut c, RequestBody::StartSeat { name: "A".into() })), ErrorCode::InvalidUser);
        let st = c.status().unwrap();
        assert_eq!(st.seats[0].state, SeatState::Stopped);
        assert!(st.seats[0].last_error.is_some());
        c.platform.passive = true;
        let e = call(&mut c, RequestBody::StartSeat { name: "A".into() }).unwrap_err();
        assert_eq!((e.code, e.reason.as_deref()), (ErrorCode::Coexistence, Some("coexistence")));
        assert!(c.status().unwrap().passive);
        assert_eq!(code(call(&mut c, RequestBody::SetMultiSession { enabled: true })), ErrorCode::Coexistence);
        let g = RemoteSettings { process_patching: true, ..Default::default() };
        assert_eq!(code(call(&mut c, RequestBody::SetGlobalSettings(g))), ErrorCode::Coexistence);
    }

    #[test]
    fn platform_start_error_carries_reason() {
        let mut c = ctl();
        c.platform.fail_start = true;
        ready(&mut c, "A", "veli");
        let e = call(&mut c, RequestBody::StartSeat { name: "A".into() }).unwrap_err();
        assert_eq!(e.reason.as_deref(), Some("rds_not_ready"));
        let st = &c.status().unwrap().seats[0];
        assert_eq!((st.state, st.failure.as_deref()), (SeatState::Stopped, Some("rds_not_ready")));
        // Stopping a stopped seat is a no-op.
        call(&mut c, RequestBody::StopSeat { name: "A".into() }).unwrap();
        assert!(c.platform().stopped.is_empty());
    }

    #[test]
    fn global_settings_roundtrip_validation_and_multi_session_guard() {
        let mut c = ctl();
        let bad = RemoteSettings { steam_isolation: true, ..Default::default() };
        assert_eq!(code(call(&mut c, RequestBody::SetGlobalSettings(bad))), ErrorCode::InvalidValue);
        let good = RemoteSettings { start_with_windows: true, ..Default::default() };
        call(&mut c, RequestBody::SetGlobalSettings(good.clone())).unwrap();
        assert_eq!(call(&mut c, RequestBody::GetGlobalSettings).unwrap(), ResponseBody::GlobalSettings(good.clone()));
        // multi_session only through its own request.
        let sneaky = RemoteSettings { multi_session: true, ..good };
        match call(&mut c, RequestBody::SetGlobalSettings(sneaky)).unwrap() {
            ResponseBody::GlobalSettings(g) => assert!(!g.multi_session),
            o => panic!("{o:?}"),
        }
        // The fake platform has no multi-session support.
        assert_eq!(code(call(&mut c, RequestBody::SetMultiSession { enabled: true })), ErrorCode::Unsupported);
    }

    #[test]
    fn pin_needs_running_seat_and_four_digits() {
        let mut c = ctl();
        let s = ready(&mut c, "A", "ali");
        let pin = |p: &str| RequestBody::SubmitPin {
            name: "A".into(),
            pin: Secret(p.into()),
            client_name: "tv".into(),
            request_id: Some(7),
        };
        assert_eq!(code(call(&mut c, pin("1234"))), ErrorCode::ServiceUnavailable);
        call(&mut c, RequestBody::StartSeat { name: "A".into() }).unwrap();
        c.platform_mut().reports.push((s.id.clone(), SeatEvent::Running { session_id: 2 }));
        c.drain_reports();
        assert_eq!(code(call(&mut c, pin("12a4"))), ErrorCode::InvalidValue);
        call(&mut c, pin("1234")).unwrap();
        assert_eq!(c.platform().pins, [Some(7)]);
        // Pair state of a stopped seat is empty, not an error.
        call(&mut c, RequestBody::StopSeat { name: "A".into() }).unwrap();
        assert_eq!(call(&mut c, RequestBody::GetPairState { name: "A".into() }).unwrap(), ResponseBody::PairState(PairState::default()));
    }

    #[test]
    fn backend_specific_requests_are_routed_by_the_seat_backend() {
        let mut c = ctl();
        let s = ready(&mut c, "A", "ali");
        call(&mut c, RequestBody::StartSeat { name: "A".into() }).unwrap();
        c.platform_mut().reports.push((s.id.clone(), SeatEvent::Running { session_id: 2 }));
        c.drain_reports();
        // Sunshine seat: Pulsar-only requests are refused with backend_sunshine.
        let e = call(&mut c, RequestBody::AnswerPeerRequest { name: "A".into(), request_id: 1, allow: true, view_only: false }).unwrap_err();
        assert_eq!((e.code, e.reason.as_deref()), (ErrorCode::Unsupported, Some("backend_sunshine")));
        let e = call(&mut c, RequestBody::GetSeatOtp { name: "A".into() }).unwrap_err();
        assert_eq!(e.reason.as_deref(), Some("backend_sunshine"));
        // Changing the backend needs the seat stopped (config locked while active).
        let mut p = s.clone();
        p.stream_backend = StreamBackendKind::Pulsar;
        assert_eq!(code(call(&mut c, RequestBody::UpdateSeat { name: "A".into(), seat: p.clone(), stream_backend: None })), ErrorCode::SeatRunning);
        call(&mut c, RequestBody::StopSeat { name: "A".into() }).unwrap();
        // A request without the field (older client) keeps the stored backend.
        let kept = match call(&mut c, RequestBody::UpdateSeat { name: "A".into(), seat: p.clone(), stream_backend: None }).unwrap() {
            ResponseBody::Seat(s) => s,
            o => panic!("{o:?}"),
        };
        assert_eq!(kept.stream_backend, StreamBackendKind::Sunshine);
        let saved = match call(
            &mut c,
            RequestBody::UpdateSeat { name: "A".into(), seat: p.clone(), stream_backend: Some(StreamBackendKind::Pulsar) },
        )
        .unwrap()
        {
            ResponseBody::Seat(s) => s,
            o => panic!("{o:?}"),
        };
        assert_eq!(saved.stream_backend, StreamBackendKind::Pulsar);
        // ...and a later old-style update does not reset it to Sunshine.
        let mut old = p;
        old.stream_backend = StreamBackendKind::Sunshine;
        let again = match call(&mut c, RequestBody::UpdateSeat { name: "A".into(), seat: old, stream_backend: None }).unwrap() {
            ResponseBody::Seat(s) => s,
            o => panic!("{o:?}"),
        };
        assert_eq!(again.stream_backend, StreamBackendKind::Pulsar);
        // Connect password works on a stopped Pulsar seat (platform default: unsupported).
        let e = call(&mut c, RequestBody::SetSeatConnectPassword { name: "A".into(), password: Secret("x\u{7}".into()) }).unwrap_err();
        assert_eq!(e.field.as_deref(), Some("password"));
        let e = call(&mut c, RequestBody::SetSeatConnectPassword { name: "A".into(), password: Secret("geheim".into()) }).unwrap_err();
        assert_eq!(e.code, ErrorCode::Unsupported);
        call(&mut c, RequestBody::StartSeat { name: "A".into() }).unwrap();
        c.platform_mut().reports.push((s.id.clone(), SeatEvent::Running { session_id: 2 }));
        c.drain_reports();
        // Pulsar seat: the PIN path is refused with backend_pulsar.
        let pin = RequestBody::SubmitPin { name: "A".into(), pin: Secret("1234".into()), client_name: "tv".into(), request_id: None };
        assert_eq!(call(&mut c, pin).unwrap_err().reason.as_deref(), Some("backend_pulsar"));
        let bad = RequestBody::AnswerRelayAuth { name: "A".into(), password: Secret("pw".into()), totp: Some(Secret("12".into())) };
        assert_eq!(call(&mut c, bad).unwrap_err().field.as_deref(), Some("totp"));
        assert_eq!(code(call(&mut c, RequestBody::GetStreamBackends)), ErrorCode::Unsupported);
    }

    #[test]
    fn identity_regeneration_needs_confirm_and_stopped_seat() {
        let mut c = ctl();
        create(&mut c, "A");
        assert_eq!(code(call(&mut c, RequestBody::RegenerateSeatIdentity { name: "A".into(), confirm: false })), ErrorCode::BadRequest);
        assert_eq!(code(call(&mut c, RequestBody::RegenerateSeatIdentity { name: "A".into(), confirm: true })), ErrorCode::Unsupported);
    }

    #[test]
    fn event_ring_is_bounded_and_notices_are_mapped() {
        let mut c = ctl();
        let s = create(&mut c, "A");
        for i in 0..(EVENT_RING + 10) {
            c.platform_mut().reports.push((
                s.id.clone(),
                SeatEvent::Notice { kind: if i % 2 == 0 { "pair_pending".into() } else { "weird".into() }, data: serde_json::json!(i) },
            ));
        }
        c.drain_reports();
        let ev = c.events_after(0);
        assert_eq!(ev.events.len(), EVENT_RING);
        assert_eq!(ev.next, (EVENT_RING + 10) as u64);
        assert!(ev.events.iter().all(|e| e.kind == "pair_pending" || e.kind == "host_warning"));
    }
}
