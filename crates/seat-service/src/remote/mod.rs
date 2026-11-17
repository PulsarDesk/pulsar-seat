// SPDX-License-Identifier: GPL-3.0-only
//! The remote seat worker (merge step 6, architecture.md section 10.5): Pulsar Split's
//! controller ([`control::Controller`]) and seat engine ([`engine::Engine`]) inside the one
//! Pulsar Seat service.
//!
//! - [`RemoteWorker`]: what the service needs from the worker (implemented by every
//!   `Controller<S, P>`). The service holds one behind a mutex and maps every protocol 3
//!   remote request to Split's request enum ([`to_legacy`]) and the answer back
//!   ([`from_legacy`]).
//! - [`DocStore`]: Split's record store (`seat_core::remote::store::ConfigStore`) over the
//!   Seat config document and the service's `SecretStore`. Remote seats are created, edited
//!   and deleted with `apply_config` only; the worker writes back service-owned fields
//!   (`rdp_client_name`, `last_mode`, `settings.remote.multi_session`), which the service
//!   merges into its document ([`merge_worker_fields`]).
//! - [`StorePulsarSecrets`]: the stream package's `PulsarSecretStore` over the service's
//!   `SecretStore` (LSA in production, merge step 5).
//! - [`runtime_of`]: a remote seat's status as a Seat `SeatRuntime` (`get_status`).
//! - `win`: the Windows parts (platform, wiring, shim health pipe, user pictures, the
//!   `remote-setup` sub-commands).

pub mod control;
pub mod engine;
pub mod glue;
#[cfg(windows)]
pub mod win;

use seat_core::config::{Config, SeatKind};
use seat_core::remote::store::{self as rstore, ConfigStore as RecordStore, StoreError as RecordError};
use seat_core::remote::{RemoteSeatRecord, RemoteSettings};
use seat_core::state::{FailureReason, SeatRuntime, SeatState, StartPhase};
use seat_core::store::{RemoteSecretSlot, SecretStore};
use seat_core::{SeatNumber, Secret};
use seat_proto::remote::legacy::{RequestBody as L, ResponseBody as LR};
use seat_proto::remote::{self as rp, CredentialTest, Events};
use seat_proto::{ErrorCode, ProtoError, RequestBody as B, ResponseBody as V};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// What the service needs from the remote seat worker.
pub trait RemoteWorker: Send {
    /// One Split request (seats by name).
    fn call(&mut self, body: L) -> Result<LR, ProtoError>;
    /// Apply the engine's progress reports (tick).
    fn drain_reports(&mut self);
    /// Events of the worker's ring with `seq > after`.
    fn events_after(&self, after: u64) -> Events;
    fn status(&self) -> Result<rp::ServiceStatus, ProtoError>;
    /// Seats marked "start with service".
    fn autostart_seat_names(&self) -> Vec<String>;
    /// Ask every active remote seat to stop.
    fn stop_all(&mut self);
    /// Power suspend: stop every active seat; names of the seats that were active.
    fn on_suspend(&mut self) -> Vec<String>;
    /// Seats booting, running or stopping.
    fn active_seats(&self) -> usize;
    /// User names of every remote seat (session sweep).
    fn seat_users(&self) -> Vec<String>;
    /// `test_seat_credentials` for a local seat (user and password from the caller).
    fn test_user_credentials(&mut self, user: &str, password: Option<Secret>) -> Result<CredentialTest, ProtoError>;
    /// `enable_unlock_account` for a local seat's user.
    fn enable_unlock_user(&mut self, user: &str, enable: bool, unlock: bool) -> Result<(), ProtoError>;
    /// Whether the remote seat with this id is booting, running or stopping.
    fn seat_active(&self, id: &str) -> bool;
    /// A remote seat removed by `apply_config`: delete its secrets, files and runtime
    /// (`seat_running` while it is active).
    fn forget_seat(&mut self, seat: &RemoteSeatRecord) -> Result<(), ProtoError>;
}

impl<S: RecordStore, P: control::Platform> RemoteWorker for control::Controller<S, P> {
    fn call(&mut self, body: L) -> Result<LR, ProtoError> {
        control::Controller::call(self, body)
    }
    fn drain_reports(&mut self) {
        control::Controller::drain_reports(self)
    }
    fn events_after(&self, after: u64) -> Events {
        control::Controller::events_after(self, after)
    }
    fn status(&self) -> Result<rp::ServiceStatus, ProtoError> {
        control::Controller::status(self)
    }
    fn autostart_seat_names(&self) -> Vec<String> {
        control::Controller::autostart_seat_names(self)
    }
    fn stop_all(&mut self) {
        control::Controller::stop_all(self)
    }
    fn on_suspend(&mut self) -> Vec<String> {
        control::Controller::on_suspend(self)
    }
    fn active_seats(&self) -> usize {
        control::Controller::active_seats(self)
    }
    fn seat_users(&self) -> Vec<String> {
        control::Controller::seat_users(self)
    }
    fn test_user_credentials(&mut self, user: &str, password: Option<Secret>) -> Result<CredentialTest, ProtoError> {
        control::Controller::test_user_credentials(self, user, password)
    }
    fn enable_unlock_user(&mut self, user: &str, enable: bool, unlock: bool) -> Result<(), ProtoError> {
        control::Controller::enable_unlock_user(self, user, enable, unlock)
    }
    fn seat_active(&self, id: &str) -> bool {
        control::Controller::seat_active(self, id)
    }
    fn forget_seat(&mut self, seat: &RemoteSeatRecord) -> Result<(), ProtoError> {
        control::Controller::forget_seat(self, seat)
    }
}

/// Engine-level hooks the service calls around boot and stop (Windows: the real engine;
/// tests: a recorder).
pub trait RemoteHost: Send + Sync {
    /// Coexistence, termwrap boot check, RDS prepare, session sweeps (contracts §9.4).
    fn service_start(&self, global: &RemoteSettings, seat_users: &[String]);
    /// Final sweep after the remote seats stopped.
    fn service_stop(&self, seat_users: &[String]);
}

/// The config document as the worker sees it. The service refreshes `config` before each
/// worker call; the worker's store writes set `dirty`.
#[derive(Debug, Default)]
pub struct DocView {
    pub config: Config,
    pub dirty: bool,
}

pub type SharedDoc = Arc<Mutex<DocView>>;
/// The service's "a password is stored" cache (shared so status never decrypts secrets).
pub type PasswordCache = Arc<Mutex<BTreeMap<SeatNumber, bool>>>;

/// Split's record store over the Seat config document (see the module doc).
pub struct DocStore {
    doc: SharedDoc,
    secrets: Arc<dyn SecretStore>,
    known: PasswordCache,
}

fn secret_err(e: seat_core::store::StoreError) -> RecordError {
    RecordError::Data(e.to_string())
}

impl DocStore {
    pub fn new(doc: SharedDoc, secrets: Arc<dyn SecretStore>, known: PasswordCache) -> Self {
        Self { doc, secrets, known }
    }

    /// Number of the remote seat called `name` (case-insensitive).
    fn number_of(&self, name: &str) -> Option<SeatNumber> {
        lock(&self.doc)
            .config
            .seats
            .iter()
            .find(|s| s.kind == SeatKind::Remote && s.name.eq_ignore_ascii_case(name))
            .map(|s| s.number)
    }

    fn edited_elsewhere() -> RecordError {
        RecordError::Data("remote seats are created, renamed and deleted with apply_config".into())
    }
}

impl RecordStore for DocStore {
    fn load(&self) -> Result<rstore::Config, RecordError> {
        let d = lock(&self.doc);
        Ok(rstore::Config {
            global: (*d.config.settings.remote).clone(),
            // VM seats belong to `crate::vm_seats`, never to the worker.
            seats: d.config.seats.iter().filter_map(|s| s.remote_record()).filter(|r| r.vm.is_none()).collect(),
        })
    }

    fn put_global(&mut self, global: &RemoteSettings) -> Result<(), RecordError> {
        let mut d = lock(&self.doc);
        *d.config.settings.remote = global.clone();
        d.dirty = true;
        Ok(())
    }

    fn put_seat(&mut self, seat: &RemoteSeatRecord) -> Result<(), RecordError> {
        let mut d = lock(&self.doc);
        let s = d
            .config
            .seats
            .iter_mut()
            .find(|s| s.kind == SeatKind::Remote && s.name.eq_ignore_ascii_case(&seat.name))
            .ok_or_else(|| RecordError::NotFound(seat.name.clone()))?;
        s.remote = Some(seat_core::remote::RemoteSeat::from_record(seat));
        d.dirty = true;
        Ok(())
    }

    fn delete_seat(&mut self, _name: &str) -> Result<(), RecordError> {
        Err(Self::edited_elsewhere())
    }

    fn rename_seat(&mut self, _old: &str, _new: &str) -> Result<(), RecordError> {
        Err(Self::edited_elsewhere())
    }

    fn set_password(&mut self, name: &str, password: &Secret) -> Result<(), RecordError> {
        let n = self.number_of(name).ok_or_else(|| RecordError::NotFound(name.into()))?;
        self.secrets.set(n, Some(password)).map_err(secret_err)?;
        lock(&self.known).insert(n, true);
        Ok(())
    }

    fn has_password(&self, name: &str) -> bool {
        let Some(n) = self.number_of(name) else { return false };
        if let Some(v) = lock(&self.known).get(&n) {
            return *v;
        }
        let v = self.secrets.has(n);
        lock(&self.known).insert(n, v);
        v
    }

    fn password(&self, name: &str) -> Result<Option<Secret>, RecordError> {
        let n = self.number_of(name).ok_or_else(|| RecordError::NotFound(name.into()))?;
        self.secrets.get(n).map_err(secret_err)
    }
}

/// Copy what the worker owns from `worker` into `into` (same seat numbers, still remote):
/// each remote seat's `rdp_client_name` and `last_mode`, and `settings.remote.multi_session`.
/// Returns whether anything changed.
pub fn merge_worker_fields(into: &mut Config, worker: &Config) -> bool {
    let mut changed = false;
    if into.settings.remote.multi_session != worker.settings.remote.multi_session {
        into.settings.remote.multi_session = worker.settings.remote.multi_session;
        changed = true;
    }
    for w in worker.seats.iter().filter(|s| s.kind == SeatKind::Remote) {
        let (Some(wr), Some(s)) = (w.remote.as_ref(), into.seats.iter_mut().find(|s| s.number == w.number)) else { continue };
        let Some(r) = s.remote.as_mut().filter(|r| r.id == wr.id) else { continue };
        if r.rdp_client_name != wr.rdp_client_name && !wr.rdp_client_name.is_empty() {
            r.rdp_client_name = wr.rdp_client_name.clone();
            changed = true;
        }
        if r.last_mode != wr.last_mode {
            r.last_mode = wr.last_mode;
            changed = true;
        }
    }
    changed
}

/// The stream package's Pulsar secret store over the service's `SecretStore`.
pub struct StorePulsarSecrets(pub Arc<dyn SecretStore>);

fn slot(kind: seat_stream::backend::pulsar::SecretKind) -> RemoteSecretSlot {
    use seat_stream::backend::pulsar::SecretKind as K;
    match kind {
        K::Identity => RemoteSecretSlot::Identity,
        K::ConnectPassword => RemoteSecretSlot::ConnectPassword,
        K::RelayKey => RemoteSecretSlot::RelayKey,
    }
}

impl seat_stream::backend::pulsar::PulsarSecretStore for StorePulsarSecrets {
    fn get(&self, seat_id: &str, kind: seat_stream::backend::pulsar::SecretKind) -> Result<Option<Secret>, seat_stream::StreamError> {
        self.0.get_remote(seat_id, slot(kind)).map_err(|e| seat_stream::StreamError::Io(format!("secret store: {e}")))
    }
    fn set(
        &self,
        seat_id: &str,
        kind: seat_stream::backend::pulsar::SecretKind,
        value: Option<&Secret>,
    ) -> Result<(), seat_stream::StreamError> {
        self.0.set_remote(seat_id, slot(kind), value).map_err(|e| seat_stream::StreamError::Io(format!("secret store: {e}")))
    }
}

fn unmapped() -> ProtoError {
    ProtoError::new(ErrorCode::Internal, "request has no remote worker mapping")
}

/// A protocol 3 remote request as Split's request; `name_of` gives a configured seat's
/// name. `None` for requests that are not remote-kind requests or name an unknown seat.
/// `test_seat_credentials` / `enable_unlock_account` are mapped here for remote seats only;
/// the service answers them for local seats itself.
pub fn to_legacy(body: &B, name_of: impl Fn(SeatNumber) -> Option<String>) -> Option<L> {
    let n = |s: &SeatNumber| name_of(*s);
    Some(match body.clone() {
        B::TestSeatCredentials { seat } => L::TestSeatCredentials { name: n(&seat)? },
        B::EnableUnlockAccount { seat, enable, unlock, confirm } => {
            L::EnableUnlockAccount { name: n(&seat)?, enable, unlock, confirm }
        }
        B::GetRemoteRuntime { seat } => L::GetSeatRuntime { name: n(&seat)? },
        B::SubmitPin { seat, pin, client_name, request_id } => L::SubmitPin { name: n(&seat)?, pin, client_name, request_id },
        B::GetPairState { seat } => L::GetPairState { name: n(&seat)? },
        B::CancelPair { seat } => L::CancelPair { name: n(&seat)? },
        B::ListPairedClients { seat } => L::ListPairedClients { name: n(&seat)? },
        B::UnpairClient { seat, uuid } => L::UnpairClient { name: n(&seat)?, uuid },
        B::UnpairAll { seat } => L::UnpairAll { name: n(&seat)? },
        B::RegenerateSeatIdentity { seat, confirm } => L::RegenerateSeatIdentity { name: n(&seat)?, confirm },
        B::GetApps { seat } => L::GetApps { name: n(&seat)? },
        B::SetApps { seat, apps } => L::SetApps { name: n(&seat)?, apps },
        B::AnswerPeerRequest { seat, request_id, allow, view_only } => {
            L::AnswerPeerRequest { name: n(&seat)?, request_id, allow, view_only }
        }
        B::SetSeatConnectPassword { seat, password } => L::SetSeatConnectPassword { name: n(&seat)?, password },
        B::GetSeatOtp { seat } => L::GetSeatOtp { name: n(&seat)? },
        B::RotateSeatOtp { seat } => L::RotateSeatOtp { name: n(&seat)? },
        B::AnswerRelayAuth { seat, password, totp } => L::AnswerRelayAuth { name: n(&seat)?, password, totp },
        B::GetHostInfo => L::GetHostInfo,
        B::SetMultiSession { enabled } => L::SetMultiSession { enabled },
        B::GetTermwrapStatus => L::GetTermwrapStatus,
        B::SetTermwrap { enabled, wrapper_path, ack_warning_version } => L::SetTermwrap { enabled, wrapper_path, ack_warning_version },
        B::ListRenderAdapters => L::ListRenderAdapters,
        B::GetPadBackends => L::GetPadBackends,
        B::GetShimStatus => L::GetIsolationStatus,
        B::SetShimTargets { targets } => L::SetShimTargets { targets },
        B::SetAppMasking { exe_path, enabled } => L::SetAppMasking { exe_path, enabled },
        B::SetDxvk { game_exe, enabled, dlls } => L::SetDxvk { game_exe, enabled, dlls },
        B::GetDxvkStatus => L::GetDxvkStatus,
        B::GetLogTail { source, lines } => {
            // `seat:<N>` names a seat by number on protocol 3; Split wants its name.
            let source = match source.strip_prefix("seat:") {
                Some(num) => format!("seat:{}", name_of(num.trim().parse::<u8>().ok().and_then(SeatNumber::new)?)?),
                None => source,
            };
            L::GetLogTail { source, lines }
        }
        B::GetStreamBackends => L::GetStreamBackends,
        B::GetUserPicture { user } => L::GetUserPicture { user },
        B::CleanupAudioEndpoints { confirm } => L::CleanupAudioEndpoints { confirm },
        _ => return None,
    })
}

/// Split's answer as a protocol 3 response body.
pub fn from_legacy(r: LR) -> Result<V, ProtoError> {
    Ok(match r {
        LR::Ack | LR::Paired { .. } => V::Ack,
        LR::HostInfo(h) => V::HostInfo(h),
        LR::TermwrapStatus(v) => V::TermwrapStatus(v),
        LR::SeatRuntime(r) => V::RemoteRuntime(r),
        LR::RenderAdapters(v) => V::RenderAdapters(v),
        LR::PadBackends(v) => V::PadBackends(v),
        LR::PairState(p) => V::PairState(p),
        LR::PairedClients(v) => V::PairedClients(v),
        LR::Apps(v) => V::Apps(v),
        LR::IsolationStatus(s) => V::ShimStatus(s),
        LR::CredentialTest(t) => V::CredentialTest(t),
        LR::LogLines(v) => V::LogLines(v),
        LR::StreamBackends(v) => V::StreamBackends(v),
        LR::Otp { otp } => V::Otp { otp },
        LR::DxvkStatus(d) => V::DxvkStatus(d),
        LR::UserPicture(p) => V::UserPicture(p),
        LR::AudioCleanup(a) => V::AudioCleanup(a),
        _ => return Err(unmapped()),
    })
}

/// A remote seat's Split status as a Seat runtime (`since_ms` = `now`).
pub fn runtime_of(seat: SeatNumber, s: &rp::SeatStatus, now: u64) -> SeatRuntime {
    let state = match s.state {
        rp::SeatState::Stopped => SeatState::Off,
        rp::SeatState::Booting => SeatState::Starting {
            phase: match s.phase.as_deref() {
                Some("user") => StartPhase::LoggingOn,
                Some("session") => StartPhase::WaitingForSession,
                Some("display") => StartPhase::BindingDisplay,
                Some("stream") => StartPhase::LaunchingHost,
                Some(_) => StartPhase::Connecting,
                None => StartPhase::Queued,
            },
        },
        rp::SeatState::Running => SeatState::SignedIn { user: s.user_name.clone() },
        rp::SeatState::Stopping => SeatState::Stopping,
        rp::SeatState::Failed => {
            let code = s.failure.clone().unwrap_or_default();
            let reason = serde_json::from_value::<FailureReason>(serde_json::Value::String(code.clone()))
                .unwrap_or(FailureReason::Internal);
            let detail = match &s.last_error {
                Some(m) if !code.is_empty() => format!("{code}: {m}"),
                Some(m) => m.clone(),
                None => code,
            };
            SeatState::Error { reason, detail }
        }
    };
    let mut degraded = Vec::new();
    if s.streaming {
        degraded.push("streaming".to_string());
    }
    SeatRuntime {
        seat,
        kind: SeatKind::Remote,
        state,
        session_id: s.session_id,
        user_sid: None,
        since_ms: now,
        degraded,
        redirect_notes: Vec::new(),
        redirect_refusals: Vec::new(),
    }
}

/// Last `n` lines of a log file for the log viewer (spec 10 §10), redacted by
/// [`redact_line`].
pub fn tail_file(path: &Path, n: usize) -> Vec<String> {
    let bytes = std::fs::read(path).unwrap_or_default();
    // Only the end of a big file matters.
    let start = bytes.len().saturating_sub(256 * 1024);
    let text = String::from_utf8_lossy(&bytes[start..]);
    let mut v: Vec<String> = text.lines().rev().filter(|l| !l.trim().is_empty()).take(n).map(redact_line).collect();
    v.reverse();
    v
}

/// Lines that may carry secrets or pairing material are replaced as a whole. The service
/// never logs secrets; this is defence in depth for lines a package logged verbatim.
/// "pin" counts as a whole word only ("mapping", "ping" stay). Same rule as the engine's
/// seat-log filter.
pub fn redact_line(l: &str) -> String {
    engine::redact(l)
}

/// Remote seat ids whose secrets an uninstall deletes: every remote seat of the config
/// document in `data_dir` plus every `seats\<id>` folder name that is a valid seat id (so
/// a seat whose config entry is gone is still found). Sorted, lower case, no duplicates.
pub fn uninstall_remote_seat_ids(data_dir: &Path) -> Vec<String> {
    use seat_core::store::ConfigStore as _;
    let mut ids = std::collections::BTreeSet::new();
    let store = seat_core::store::FileStore::new(data_dir.join(seat_core::consts::CONFIG_FILE));
    if let Ok(cfg) = store.load() {
        ids.extend(cfg.seats.iter().filter_map(|s| s.remote_record()).map(|r| r.id.to_ascii_lowercase()));
    }
    if let Ok(rd) = std::fs::read_dir(data_dir.join("seats")) {
        for e in rd.flatten() {
            if e.file_type().is_ok_and(|t| t.is_dir())
                && let Some(name) = e.file_name().to_str()
            {
                ids.insert(name.to_ascii_lowercase());
            }
        }
    }
    // Only names that can be secret names (GUID characters), never a path trick.
    ids.into_iter().filter(|id| seat_core::store::remote_secret_name(id, RemoteSecretSlot::Identity).is_ok()).collect()
}

/// Delete every remote secret slot (identity, connect password, relay key) of `ids`; a
/// missing secret is fine. Returns `(seat id, error)` for the deletes that failed.
pub fn delete_remote_secrets(store: &dyn SecretStore, ids: &[String]) -> Vec<(String, String)> {
    let mut failed = Vec::new();
    for id in ids {
        for slot in RemoteSecretSlot::ALL {
            if let Err(e) = store.set_remote(id, slot, None) {
                failed.push((id.clone(), format!("{}: {e}", slot.as_str())));
            }
        }
    }
    failed
}

#[cfg(test)]
mod tests {
    use super::*;
    use seat_core::config::SeatConfig;
    use seat_core::remote::{LastMode, RemoteSeat};
    use seat_core::store::MemorySecretStore;

    fn n(x: u8) -> SeatNumber {
        SeatNumber::new(x).unwrap()
    }

    fn doc() -> Config {
        let mut c = Config::default();
        c.seats.push(SeatConfig::new(n(2)));
        let r = RemoteSeat { id: "0f8fad5b-d9cb-469f-a165-70867728950e".into(), display_name: "Salon".into(), port: 48100, ..Default::default() };
        let mut s = SeatConfig::new_remote(n(3), r);
        s.name = "Salon".into();
        c.seats.push(s);
        c
    }

    fn store() -> (DocStore, SharedDoc, Arc<MemorySecretStore>) {
        let shared: SharedDoc = Arc::new(Mutex::new(DocView { config: doc(), dirty: false }));
        let secrets = Arc::new(MemorySecretStore::default());
        (DocStore::new(shared.clone(), secrets.clone(), PasswordCache::default()), shared, secrets)
    }

    #[test]
    fn uninstall_deletes_remote_secrets_of_config_and_seat_folders() {
        use seat_core::store::ConfigStore as _;
        let dir = std::env::temp_dir().join(format!("seat-uninstall-remote-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("seats").join("AAAA-0001")).unwrap();
        std::fs::create_dir_all(dir.join("seats").join("not a guid")).unwrap();
        seat_core::store::FileStore::new(dir.join(seat_core::consts::CONFIG_FILE)).save(&doc()).unwrap();
        let ids = uninstall_remote_seat_ids(&dir);
        assert_eq!(ids, ["0f8fad5b-d9cb-469f-a165-70867728950e", "aaaa-0001"]);
        let secrets = MemorySecretStore::default();
        let other = "bbbb-0002";
        for id in ids.iter().map(String::as_str).chain([other]) {
            for slot in RemoteSecretSlot::ALL {
                secrets.set_remote(id, slot, Some(&Secret::new("x"))).unwrap();
            }
        }
        secrets.set(n(2), Some(&Secret::new("pw"))).unwrap();
        assert!(delete_remote_secrets(&secrets, &ids).is_empty());
        for id in &ids {
            for slot in RemoteSecretSlot::ALL {
                assert!(secrets.get_remote(id, slot).unwrap().is_none(), "{id} {slot:?}");
            }
        }
        // Only the listed seats: another id and the seat passwords are not touched here.
        assert!(secrets.get_remote(other, RemoteSecretSlot::RelayKey).unwrap().is_some());
        assert!(secrets.has(n(2)));
        // No data folder: nothing found, nothing fails.
        let _ = std::fs::remove_dir_all(&dir);
        assert!(uninstall_remote_seat_ids(&dir).is_empty());
    }

    #[test]
    fn doc_store_shows_remote_seats_only_and_writes_back() {
        let (mut st, shared, secrets) = store();
        let c = st.load().unwrap();
        assert_eq!(c.seats.len(), 1);
        assert_eq!(c.seats[0].name, "Salon");
        let mut rec = c.seats[0].clone();
        rec.last_mode = Some(LastMode { width: 1920, height: 1080, hz: 60 });
        st.put_seat(&rec).unwrap();
        assert!(shared.lock().unwrap().dirty);
        assert_eq!(shared.lock().unwrap().config.seat(n(3)).unwrap().remote.as_ref().unwrap().last_mode, rec.last_mode);
        // Unknown / local seats and structural edits are refused.
        assert!(matches!(st.put_seat(&RemoteSeatRecord { name: "Seat 2".into(), ..Default::default() }), Err(RecordError::NotFound(_))));
        assert!(st.delete_seat("Salon").is_err() && st.rename_seat("Salon", "B").is_err());
        // Passwords go to the seat's slot of the secret store.
        assert!(!st.has_password("salon"));
        st.set_password("salon", &Secret::new("pw")).unwrap();
        assert!(st.has_password("SALON"));
        assert_eq!(secrets.get(n(3)).unwrap().unwrap().expose(), "pw");
        assert_eq!(st.password("Salon").unwrap().unwrap().expose(), "pw");
        st.put_global(&RemoteSettings { multi_session: true, ..Default::default() }).unwrap();
        assert!(shared.lock().unwrap().config.settings.remote.multi_session);
    }

    #[test]
    fn merge_takes_only_worker_owned_fields() {
        let mut base = doc();
        let mut worker = doc();
        {
            let r = worker.seats.iter_mut().find(|s| s.number == n(3)).unwrap().remote.as_mut().unwrap();
            r.rdp_client_name = "PSS0F8FAD5B".into();
            r.last_mode = Some(LastMode { width: 2560, height: 1440, hz: 120 });
            r.port = 49999; // not worker-owned
        }
        worker.settings.remote.multi_session = true;
        worker.settings.remote.hid_isolation = true; // not worker-owned
        assert!(merge_worker_fields(&mut base, &worker));
        let r = base.seat(n(3)).unwrap().remote.as_ref().unwrap();
        assert_eq!((r.rdp_client_name.as_str(), r.port), ("PSS0F8FAD5B", 48100));
        assert_eq!(r.last_mode.unwrap().hz, 120);
        assert!(base.settings.remote.multi_session && !base.settings.remote.hid_isolation);
        assert!(!merge_worker_fields(&mut base, &worker), "stable");
        // A seat replaced meanwhile (other id) is left alone.
        base.seats.iter_mut().find(|s| s.number == n(3)).unwrap().remote.as_mut().unwrap().id = "other".into();
        worker.seats.iter_mut().find(|s| s.number == n(3)).unwrap().remote.as_mut().unwrap().last_mode = None;
        assert!(!merge_worker_fields(&mut base, &worker));
    }

    #[test]
    fn pulsar_secrets_use_the_remote_slots() {
        use seat_stream::backend::pulsar::{PulsarSecretStore, SecretKind};
        let store = Arc::new(MemorySecretStore::default());
        let p = StorePulsarSecrets(store.clone());
        let id = "0f8fad5b-d9cb-469f-a165-70867728950e";
        for k in SecretKind::ALL {
            p.set(id, k, Some(&Secret::new(k.as_str()))).unwrap();
            assert_eq!(p.get(id, k).unwrap().unwrap().expose(), k.as_str());
        }
        assert_eq!(store.get_remote(id, RemoteSecretSlot::RelayKey).unwrap().unwrap().expose(), "relay_key");
        p.set(id, SecretKind::RelayKey, None).unwrap();
        assert!(p.get(id, SecretKind::RelayKey).unwrap().is_none());
        assert!(p.get("../x", SecretKind::Identity).is_err());
    }

    #[test]
    fn requests_map_by_seat_name_and_answers_map_back() {
        let name_of = |s: SeatNumber| (s == n(3)).then(|| "Salon".to_string());
        assert_eq!(to_legacy(&B::GetPairState { seat: n(3) }, name_of), Some(L::GetPairState { name: "Salon".into() }));
        assert_eq!(to_legacy(&B::GetPairState { seat: n(4) }, name_of), None);
        assert_eq!(to_legacy(&B::GetShimStatus, name_of), Some(L::GetIsolationStatus));
        assert_eq!(
            to_legacy(&B::GetLogTail { source: "seat:3".into(), lines: 5 }, name_of),
            Some(L::GetLogTail { source: "seat:Salon".into(), lines: 5 })
        );
        assert_eq!(to_legacy(&B::GetLogTail { source: "seat:x".into(), lines: 5 }, name_of), None);
        assert_eq!(
            to_legacy(&B::GetLogTail { source: "service".into(), lines: 5 }, name_of),
            Some(L::GetLogTail { source: "service".into(), lines: 5 })
        );
        assert_eq!(to_legacy(&B::GetStatus, name_of), None);
        // Every remote-kind request maps (seat 3 known).
        let all = [
            B::TestSeatCredentials { seat: n(3) },
            B::EnableUnlockAccount { seat: n(3), enable: true, unlock: true, confirm: true },
            B::GetRemoteRuntime { seat: n(3) },
            B::SubmitPin { seat: n(3), pin: Secret::new("1234"), client_name: "tv".into(), request_id: None },
            B::CancelPair { seat: n(3) },
            B::ListPairedClients { seat: n(3) },
            B::UnpairClient { seat: n(3), uuid: "u".into() },
            B::UnpairAll { seat: n(3) },
            B::RegenerateSeatIdentity { seat: n(3), confirm: true },
            B::GetApps { seat: n(3) },
            B::SetApps { seat: n(3), apps: vec![] },
            B::AnswerPeerRequest { seat: n(3), request_id: 1, allow: true, view_only: false },
            B::SetSeatConnectPassword { seat: n(3), password: Secret::new("x") },
            B::GetSeatOtp { seat: n(3) },
            B::RotateSeatOtp { seat: n(3) },
            B::AnswerRelayAuth { seat: n(3), password: Secret::new("x"), totp: None },
            B::GetHostInfo,
            B::SetMultiSession { enabled: true },
            B::GetTermwrapStatus,
            B::SetTermwrap { enabled: false, wrapper_path: None, ack_warning_version: 0 },
            B::ListRenderAdapters,
            B::GetPadBackends,
            B::SetShimTargets { targets: vec![] },
            B::SetAppMasking { exe_path: "x".into(), enabled: true },
            B::SetDxvk { game_exe: "x".into(), enabled: true, dlls: vec![] },
            B::GetDxvkStatus,
            B::GetStreamBackends,
            B::GetUserPicture { user: "u".into() },
            B::CleanupAudioEndpoints { confirm: true },
        ];
        for b in all {
            assert!(b.is_remote_kind_request(), "{b:?}");
            assert!(to_legacy(&b, name_of).is_some(), "{b:?}");
        }
        assert_eq!(from_legacy(LR::Ack).unwrap(), V::Ack);
        assert!(matches!(from_legacy(LR::IsolationStatus(Default::default())).unwrap(), V::ShimStatus(_)));
        assert!(matches!(from_legacy(LR::SeatRuntime(Default::default())).unwrap(), V::RemoteRuntime(_)));
        assert!(from_legacy(LR::Seats(vec![])).is_err());
    }

    #[test]
    fn remote_status_maps_to_seat_runtime() {
        let mut s = rp::SeatStatus { name: "Salon".into(), user_name: "oyuncu".into(), ..Default::default() };
        assert_eq!(runtime_of(n(3), &s, 5).state, SeatState::Off);
        s.state = rp::SeatState::Booting;
        s.phase = Some("display".into());
        assert_eq!(runtime_of(n(3), &s, 5).state, SeatState::Starting { phase: StartPhase::BindingDisplay });
        s.state = rp::SeatState::Running;
        s.session_id = Some(7);
        s.streaming = true;
        let r = runtime_of(n(3), &s, 5);
        assert_eq!((r.kind, r.session_id, r.state.clone()), (SeatKind::Remote, Some(7), SeatState::SignedIn { user: "oyuncu".into() }));
        assert_eq!(r.degraded, ["streaming"]);
        s.state = rp::SeatState::Failed;
        s.failure = Some("client_auth".into());
        s.last_error = Some("NLA refused".into());
        assert_eq!(
            runtime_of(n(3), &s, 5).state,
            SeatState::Error { reason: FailureReason::ClientAuth, detail: "client_auth: NLA refused".into() }
        );
        s.failure = Some("display_timeout".into());
        assert!(matches!(runtime_of(n(3), &s, 5).state, SeatState::Error { reason: FailureReason::Internal, .. }));
    }

    /// Moved from split-service `log::tests::tail_redacts_sensitive_lines`.
    #[test]
    fn tail_redacts_sensitive_lines() {
        let dir = std::env::temp_dir().join(format!("ps-logtail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("service.log");
        std::fs::write(&p, "1 INFO a\n2 INFO PIN accepted 1234\n\n3 WARN Authorization: Basic x\n4 INFO b\n").unwrap();
        let t = tail_file(&p, 3);
        assert_eq!(t, ["[line hidden]", "[line hidden]", "4 INFO b"]);
        assert_eq!(tail_file(&p, 10).len(), 4);
        assert_eq!(redact_line("pad mapping ok, ping 3 ms"), "pad mapping ok, ping 3 ms");
        for l in [
            "auth: otp=123456",
            "Tek kullanımlık kod 123456",
            "relay_key issued",
            "relay access key AbC",
            "seat identity loaded",
            "relay totp 000000",
        ] {
            assert_eq!(redact_line(l), "[line hidden]", "{l}");
        }
        assert_eq!(redact_line("usb hotplug event"), "usb hotplug event");
        assert!(tail_file(&dir.join("missing.log"), 5).is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
