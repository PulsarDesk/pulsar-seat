// SPDX-License-Identifier: GPL-3.0-only
//! Hyper-V VM seats in the service (hyperv-seats 2026-11-17; spec/remote 10 §8, 90 §2.4;
//! architecture.md "Hyper-V VM seats").
//!
//! A VM seat is a remote seat with `remote.vm` set. The remote seat worker never sees it
//! (`DocStore` leaves VM seats out); this module starts, stops, watches and deletes it:
//!
//! - **Start** (`start_seats`, autostart, resume): the Hyper-V feature, the VM network and
//!   the base disk must be there, the seat needs a user and a stored password. The service
//!   ensures the seat's VM (`VmOp::Ensure`: differencing disk, memory, processors, GPU
//!   partition, port forwards, firewall rule, SMB shares; the share account password goes
//!   on the script's stdin), offers the seat's [`GuestConfig`] on the guest link and starts
//!   the VM. The guest service (`vm-guest`) takes the config, creates / signs in the seat
//!   user and runs the stream server; its phases come back over the link.
//! - **Status**: every tick (at most every [`VM_POLL_MS`]) while a VM seat is active, the
//!   Hyper-V state and the guest phase become the seat's `SeatRuntime` (`remote_rt`).
//! - **Stop**: `Shutdown` over the link, then the seat's stop mode (`Stop-VM`, `Save-VM`,
//!   turn off); optional GPU restart afterwards (`restart_gpus_on_shutdown`).
//! - **Delete** (`apply_config` removed the seat): VM, disk, forwards, rule, shares.
//! - **`vm_seats` request**: status, VM network, base disk build (background), reset base,
//!   reset one seat's disk.
//!
//! Everything Pulsar creates is journaled (`journal\hyperv-vm.json`) before it is made;
//! uninstall removes it (`win::platform::hyperv_vm_uninstall`).
//!
//! HYPOTHESIS: never ran on a Hyper-V host.

use crate::log;
use crate::service::{Service, State};
use seat_compat::hyperv_vm::{self as hv, GpuChoice, HostFacts, HostSpec, ShareSpec, VmOp, VmSpec};
use seat_core::config::{SeatAccount, SeatConfig};
use seat_core::remote::vm::{self, GuestConfig, GuestMsg, GuestPhase, GuestShare};
use seat_core::state::{FailureReason, SeatRuntime, SeatState, StartPhase};
use seat_core::{SeatNumber, Secret};
use seat_proto::hyperv::{VmAction, VmHostStatus, VmSeatInfo};
use seat_proto::{ErrorCode, Event, ProtoError, ResponseBody};
use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard};

/// Poll interval of the VM states while a VM seat is active.
pub const VM_POLL_MS: u64 = 2_000;
/// Host-side stream app list of a VM seat (JSON array, `vm_seat_dir`).
const APPS_FILE: &str = "apps.json";
/// Marker of a pending identity reset (`vm_seat_dir`).
const REGENERATE_FLAG: &str = "regenerate-identity";
/// How long a forwarded seat page request waits for the guest's reply.
pub const GUEST_REQUEST_MS: u64 = 15_000;
/// How long a forwarded pairing PIN may take (the client finishes the pairing meanwhile).
pub const GUEST_PAIR_MS: u64 = 120_000;

fn err(code: ErrorCode, msg: impl Into<String>) -> ProtoError {
    ProtoError::new(code, msg)
}

fn backend(e: seat_core::backend::BackendError) -> ProtoError {
    ProtoError::new(ErrorCode::Os, format!("hyper-v: {e}")).with_reason("hyperv_failed")
}

/// One VM seat the service started.
#[derive(Debug, Clone, Default)]
struct Active {
    seat_id: String,
    vm_id: String,
    user: String,
    gpu: bool,
    stopping: bool,
    /// Ensure / Start-VM still running: a poll must not take the VM's "off" for a
    /// finished stop (review 2026-11-17).
    starting: bool,
    /// Service clock when Start-VM returned. A poll whose status read began before this
    /// may still show the VM off.
    start_done_ms: u64,
}

#[derive(Default)]
struct Inner {
    building: bool,
    last_error: Option<String>,
    facts: Option<HostFacts>,
    last_poll_ms: u64,
    polling: bool,
    active: BTreeMap<SeatNumber, Active>,
}

/// VM seat state kept next to the service state.
#[derive(Default)]
pub struct VmSlot {
    inner: Mutex<Inner>,
}

impl VmSlot {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// A seat's runtime state from the VM state and the guest phase (pure).
pub fn vm_seat_state(vm_state: Option<&str>, guest: Option<&(GuestPhase, Option<String>)>, user: &str, stopping: bool) -> SeatState {
    match vm_state.unwrap_or("missing") {
        "running" if stopping => SeatState::Stopping,
        "running" => match guest {
            Some((GuestPhase::SignedIn | GuestPhase::Streaming, _)) => SeatState::SignedIn { user: user.to_string() },
            Some((GuestPhase::Failed, d)) => {
                SeatState::Error { reason: FailureReason::Internal, detail: d.clone().unwrap_or_else(|| "guest failed".into()) }
            }
            Some((GuestPhase::Configuring, _)) => SeatState::Starting { phase: StartPhase::LoggingOn },
            _ => SeatState::Starting { phase: StartPhase::WaitingForSession },
        },
        "starting" | "resuming" => SeatState::Starting { phase: StartPhase::WaitingForHost },
        "stopping" | "saving" | "pausing" => SeatState::Stopping,
        "paused" => SeatState::Error { reason: FailureReason::HostExited, detail: "VM paused (host out of resources?)".into() },
        // off, saved, missing: the VM is down (a stop finished).
        _ => SeatState::Off,
    }
}

fn is_vm(s: &SeatConfig) -> bool {
    s.remote.as_ref().is_some_and(|r| r.is_vm())
}

fn user_of(s: &SeatConfig) -> String {
    match &s.account {
        SeatAccount::Local { user } | SeatAccount::Domain { user, .. } => user.clone(),
        SeatAccount::LogonScreen => String::new(),
    }
}

impl Service {
    /// Whether `seat` is a configured VM seat.
    pub(crate) fn is_vm_seat(&self, seat: SeatNumber) -> bool {
        self.lock().config.seat(seat).is_some_and(is_vm)
    }

    fn vm_storage_of(&self, st: &State) -> String {
        let s = &st.config.settings.remote.vm.storage_location;
        if s.is_empty() { self.platform.vm_default_storage() } else { s.clone() }
    }

    fn vm_host_spec(&self, st: &State) -> HostSpec {
        let v = &st.config.settings.remote.vm;
        HostSpec {
            storage: self.vm_storage_of(st),
            nat_prefix: v.nat_prefix.clone(),
            payload_dir: self.platform.vm_payload_dir(),
            settings: (**v).clone(),
        }
    }

    /// The scripts' view of one VM seat.
    fn vm_spec(&self, st: &State, seat: &SeatConfig, facts: Option<&HostFacts>) -> Option<VmSpec> {
        let r = seat.remote.as_ref()?;
        let v = r.vm.as_ref()?;
        let vm_seats = st.config.seats.iter().filter(|s| is_vm(s)).count();
        let (mem, cpus) = facts.map(|f| (f.host_memory_mb, f.logical_processors)).unwrap_or((16 * 1024, 4));
        Some(VmSpec {
            seat_id: r.id.clone(),
            storage: self.vm_storage_of(st),
            memory_mb: vm::memory_for(v, st.config.settings.remote.vm.memory_percent, mem, vm_seats),
            processors: vm::processors_for(v, cpus),
            gpu: GpuChoice::from_seat(v.gpu, v.gpu_adapter.as_deref()),
            forwards: vm::all_forwards(v, r.port),
            guest_address: v.guest_address.clone(),
            shares: v
                .shared_folders
                .iter()
                .enumerate()
                .map(|(i, f)| ShareSpec { name: hv::share_name(&r.id, i), host_path: f.host_path.clone(), read_only: f.read_only })
                .collect(),
            stop: v.stop,
        })
    }

    fn vm_journal_path(&self) -> std::path::PathBuf {
        self.data_dir.join("journal").join(hv::JOURNAL_FILE)
    }

    fn vm_journal(&self, f: impl FnOnce(&mut hv::VmJournal)) {
        let path = self.vm_journal_path();
        let mut j: hv::VmJournal = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        f(&mut j);
        if let Some(d) = path.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        if let Err(e) = std::fs::write(&path, serde_json::to_vec_pretty(&j).unwrap_or_default()) {
            log::error(&format!("vm journal not written: {e}"));
        }
    }

    fn vm_run(&self, op: &VmOp, stdin: Option<&Secret>) -> Result<HostFacts, ProtoError> {
        let f = self.platform.hyperv_vm(op, stdin).map_err(backend)?;
        let mut v = self.vm.lock();
        v.facts = Some(f.clone());
        v.last_poll_ms = self.now();
        Ok(f)
    }

    fn vm_refresh(&self) -> Result<HostFacts, ProtoError> {
        let storage = self.vm_storage_of(&self.lock());
        self.vm_run(&VmOp::Status { storage }, None)
    }

    fn vm_status_body(&self, f: &HostFacts) -> VmHostStatus {
        let (seats, storage) = {
            let st = self.lock();
            let seats: Vec<(u8, String, String)> = st
                .config
                .seats
                .iter()
                .filter(|s| is_vm(s))
                .filter_map(|s| {
                    let r = s.remote.as_ref()?;
                    Some((s.number.get(), r.id.clone(), r.vm.as_ref()?.guest_address.clone()))
                })
                .collect();
            (seats, self.vm_storage_of(&st))
        };
        let v = self.vm.lock();
        VmHostStatus {
            feature: f.feature.clone(),
            vmms: f.vmms,
            network_ready: f.switch_present && f.nat_present,
            base_present: f.base_present,
            building: v.building,
            last_error: v.last_error.clone(),
            storage,
            gpus: f.gpus.clone(),
            host_memory_mb: f.host_memory_mb,
            logical_processors: f.logical_processors,
            seats: seats
                .into_iter()
                .map(|(seat, id, guest_address)| {
                    let vm = f.vms.iter().find(|x| x.seat_id == id);
                    let phase = vm.and_then(|x| self.platform.guest_link_phase(&x.vm_id));
                    VmSeatInfo {
                        seat,
                        state: vm.map(|x| x.state.clone()).unwrap_or_else(|| "missing".into()),
                        guest_address,
                        guest_phase: phase.as_ref().and_then(|(p, _)| serde_json::to_value(p).ok()?.as_str().map(str::to_owned)),
                        guest_detail: phase.and_then(|(_, d)| d),
                    }
                })
                .collect(),
        }
    }

    /// `vm_seats` request.
    pub(crate) fn vm_seats_request(&self, action: VmAction) -> Result<ResponseBody, ProtoError> {
        let need = |c: bool| if c { Ok(()) } else { Err(err(ErrorCode::InvalidValue, "confirmation needed").with_field("confirm")) };
        match &action {
            VmAction::Status => {}
            VmAction::PrepareHost { confirm } => {
                need(*confirm)?;
                let host = self.vm_host_spec(&self.lock());
                vm::parse_prefix(&host.nat_prefix)
                    .ok_or_else(|| err(ErrorCode::InvalidValue, "invalid NAT prefix").with_field("settings.remote.vm.nat_prefix"))?;
                self.vm_journal(|j| j.host_prepared = true);
                log::info("vm seats: preparing the VM network");
                self.vm_run(&VmOp::PrepareHost { host }, None)?;
            }
            VmAction::BuildBase { confirm } => {
                need(*confirm)?;
                let host = self.vm_host_spec(&self.lock());
                if host.settings.image_kind == vm::ImageKind::None {
                    return Err(err(ErrorCode::NotReady, "choose a Windows image first").with_reason("vm_no_image"));
                }
                {
                    let mut v = self.vm.lock();
                    if v.building {
                        return Err(err(ErrorCode::NotReady, "a base disk build runs").with_reason("vm_building"));
                    }
                    v.building = true;
                    v.last_error = None;
                }
                let storage = host.storage.clone();
                self.vm_journal(|j| {
                    j.base_built = true;
                    j.storage = storage;
                });
                log::info("vm seats: building the base disk");
                self.spawn(move |s| {
                    let r = s.vm_run(&VmOp::BuildBase { host }, None);
                    let mut v = s.vm.lock();
                    v.building = false;
                    match r {
                        Ok(_) => log::info("vm seats: base disk built"),
                        Err(e) => {
                            log::error(&format!("vm seats: base disk build failed: {e}"));
                            v.last_error = Some(e.message.clone());
                        }
                    }
                    drop(v);
                    s.notice("vm_base_build_done", None, "");
                });
            }
            VmAction::ResetBase { confirm } => {
                need(*confirm)?;
                if !self.vm.lock().active.is_empty() {
                    return Err(err(ErrorCode::SeatRunning, "stop every VM seat first").with_reason("seat_running"));
                }
                if self.vm.lock().building {
                    return Err(err(ErrorCode::NotReady, "a base disk build runs").with_reason("vm_building"));
                }
                let (storage, seat_ids) = {
                    let st = self.lock();
                    let ids: Vec<String> = st.config.seats.iter().filter(|s| is_vm(s)).filter_map(|s| s.remote.as_ref().map(|r| r.id.clone())).collect();
                    (self.vm_storage_of(&st), ids)
                };
                log::info("vm seats: base disk and seat disks reset");
                self.vm_run(&VmOp::ResetBase { storage, seat_ids: seat_ids.clone() }, None)?;
                self.vm_journal(|j| {
                    j.base_built = false;
                    for id in &seat_ids {
                        j.remove_seat(id);
                    }
                });
            }
            VmAction::ResetSandbox { seat, confirm } => {
                need(*confirm)?;
                let n = SeatNumber::new(*seat).ok_or_else(|| err(ErrorCode::InvalidValue, "bad seat"))?;
                if self.vm.lock().active.contains_key(&n) {
                    return Err(err(ErrorCode::SeatRunning, "stop the seat first").with_reason("seat_running"));
                }
                let spec = {
                    let st = self.lock();
                    let s = st.config.seat(n).filter(|s| is_vm(s)).ok_or_else(|| err(ErrorCode::NotFound, "not a VM seat"))?;
                    let facts = self.vm.lock().facts.clone();
                    self.vm_spec(&st, s, facts.as_ref()).ok_or_else(|| err(ErrorCode::NotFound, "not a VM seat"))?
                };
                log::info(&format!("vm seat {n}: disk reset"));
                self.vm_run(&VmOp::ResetSandbox { spec }, None)?;
            }
        }
        let f = self.vm_refresh()?;
        Ok(ResponseBody::VmSeats(Box::new(self.vm_status_body(&f))))
    }

    /// Start one VM seat (checks first; the first refusal is the answer).
    pub(crate) fn start_vm_seat(&self, n: SeatNumber) -> Result<(), ProtoError> {
        if self.vm.lock().active.contains_key(&n) {
            return Err(err(ErrorCode::SeatRunning, format!("seat {n} is running")).with_reason("seat_running"));
        }
        let facts = self.vm_refresh()?;
        if facts.feature != "enabled" || !facts.vmms {
            return Err(err(ErrorCode::NotReady, "Hyper-V is not enabled (Network tab)").with_reason("hyperv_unavailable"));
        }
        if !(facts.switch_present && facts.nat_present) {
            return Err(err(ErrorCode::NotReady, "the VM network is not prepared").with_reason("vm_network_missing"));
        }
        if !facts.base_present {
            return Err(err(ErrorCode::NotReady, "the base disk is not built").with_reason("vm_base_missing"));
        }
        let (spec, seat) = {
            let st = self.lock();
            let s = st.config.seat(n).filter(|s| is_vm(s)).ok_or_else(|| err(ErrorCode::NotFound, "not a VM seat"))?.clone();
            (self.vm_spec(&st, &s, Some(&facts)).ok_or_else(|| err(ErrorCode::NotFound, "not a VM seat"))?, s)
        };
        if spec.guest_address.is_empty() || spec.seat_id.is_empty() {
            return Err(err(ErrorCode::NotReady, "the seat has no id or guest address yet; save the config").with_reason("vm_not_assigned"));
        }
        if let GpuChoice::Adapter(a) = &spec.gpu
            && !facts.gpus.iter().any(|g| g.eq_ignore_ascii_case(a))
        {
            return Err(err(ErrorCode::NotReady, "the chosen GPU cannot be partitioned").with_reason("vm_gpu_missing"));
        }
        let user = user_of(&seat);
        let password = self.secrets.get(n).map_err(|e| err(ErrorCode::Store, e.to_string()))?;
        let (true, Some(password)) = (!user.is_empty(), password) else {
            return Err(err(ErrorCode::MissingCredentials, "seat needs a user and a password").with_reason("missing_credentials"));
        };
        let share_password = self.platform.random_token().map_err(backend)?;
        self.vm_journal(|j| {
            j.add_seat(&spec.seat_id);
            j.add_shares(&spec.shares);
            if j.storage.is_empty() {
                j.storage = spec.storage.clone();
            }
        });
        self.set_vm_rt(n, SeatState::Starting { phase: StartPhase::Queued }, Vec::new());
        let facts = match self.vm_run(&VmOp::Ensure { spec: spec.clone() }, Some(&share_password)) {
            Ok(f) => f,
            Err(e) => {
                self.set_vm_rt(n, SeatState::Error { reason: FailureReason::Internal, detail: e.message.clone() }, Vec::new());
                return Err(e);
            }
        };
        let vm_id = facts.vms.iter().find(|v| v.seat_id == spec.seat_id).map(|v| v.vm_id.clone()).unwrap_or_default();
        let config = self.guest_config(&seat, &spec, user.clone(), password, share_password);
        self.platform.guest_link_offer(&vm_id, Some(config));
        self.vm.lock().active.insert(
            n,
            Active {
                seat_id: spec.seat_id.clone(),
                vm_id: vm_id.clone(),
                user,
                gpu: spec.gpu.is_on(),
                stopping: false,
                starting: true,
                start_done_ms: 0,
            },
        );
        if let Err(e) = self.vm_run(&VmOp::Start { seat_id: spec.seat_id.clone() }, None) {
            self.vm.lock().active.remove(&n);
            self.platform.guest_link_offer(&vm_id, None);
            self.set_vm_rt(n, SeatState::Error { reason: FailureReason::Internal, detail: e.message.clone() }, Vec::new());
            return Err(e);
        }
        let done = self.now();
        if let Some(a) = self.vm.lock().active.get_mut(&n) {
            a.starting = false;
            a.start_done_ms = done;
        }
        // The guest got the identity reset with this config.
        let _ = std::fs::remove_file(self.vm_seat_dir(&spec.seat_id).join(REGENERATE_FLAG));
        log::info(&format!("vm seat {n} starting"));
        self.vm_poll_now();
        Ok(())
    }

    fn guest_config(&self, seat: &SeatConfig, spec: &VmSpec, user: String, password: Secret, share_password: Secret) -> GuestConfig {
        let st = self.lock();
        let r = seat.remote.clone().unwrap_or_default();
        let v = r.vm.clone().unwrap_or_default();
        let prefix = &st.config.settings.remote.vm.nat_prefix;
        let gw = vm::gateway(prefix).map(|g| g.to_string()).unwrap_or_default();
        GuestConfig {
            seat_id: r.id.clone(),
            seat_name: seat.name.clone(),
            advertised_name: vm::advertised_name(&r.display_name, &v),
            port: r.port,
            stream_backend: r.stream_backend,
            stream: r.stream.clone(),
            audio: r.audio,
            verbosity: st.config.settings.remote.verbosity,
            user_name: user,
            password,
            address: v.guest_address.clone(),
            prefix_len: vm::parse_prefix(prefix).map(|(_, l)| l).unwrap_or(24),
            gateway: gw.clone(),
            dns: self.vm.lock().facts.as_ref().map(|f| f.dns.clone()).unwrap_or_default(),
            shares: spec
                .shares
                .iter()
                .zip(v.shared_folders.iter())
                .map(|(s, f)| GuestShare { unc: format!(r"\\{gw}\{}", s.name), guest_path: f.guest_path.clone(), read_only: f.read_only })
                .collect(),
            share_user: hv::share_account(&r.id),
            share_password,
            apps: r.apps.clone(),
            stream_apps: self.vm_stream_apps(&r.id),
            connect_password: self.secrets.get_remote(&r.id, seat_core::store::RemoteSecretSlot::ConnectPassword).ok().flatten(),
            regenerate_identity: self.vm_seat_dir(&r.id).join(REGENERATE_FLAG).exists(),
            start_nonce: self.platform.random_token().map(|t| t.expose().to_string()).unwrap_or_default(),
        }
    }

    /// Host-side files of a VM seat (`<data>\vm-seats\<seat id>`): the stream app list and
    /// the pending identity reset. The guest gets them with its config.
    fn vm_seat_dir(&self, seat_id: &str) -> std::path::PathBuf {
        // Seat ids are GUIDs; anything else never names a folder.
        let safe: String = seat_id.chars().filter(|c| c.is_ascii_hexdigit() || *c == '-').collect();
        self.data_dir.join("vm-seats").join(if safe.is_empty() { "_".into() } else { safe })
    }

    /// The VM seat's stream app list as last set (`None` = the stream server's defaults).
    fn vm_stream_apps(&self, seat_id: &str) -> Option<Vec<serde_json::Value>> {
        let b = std::fs::read(self.vm_seat_dir(seat_id).join(APPS_FILE)).ok()?;
        serde_json::from_slice(&b).ok()
    }

    fn vm_seat_id(&self, n: SeatNumber) -> Result<String, ProtoError> {
        self.lock()
            .config
            .seat(n)
            .and_then(|s| s.remote.as_ref().map(|r| r.id.clone()))
            .filter(|id| !id.is_empty())
            .ok_or_else(|| err(ErrorCode::NotReady, "the seat has no id yet; save the config").with_reason("vm_not_assigned"))
    }

    /// `get_apps` on a VM seat: the list kept on the host, or the defaults.
    pub(crate) fn vm_get_apps(&self, n: SeatNumber) -> Result<ResponseBody, ProtoError> {
        let id = self.vm_seat_id(n)?;
        let apps = match self.vm_stream_apps(&id) {
            Some(a) => a,
            None => seat_stream::apps::default_apps().iter().filter_map(|a| serde_json::to_value(a).ok()).collect(),
        };
        Ok(ResponseBody::Apps(apps))
    }

    /// `set_apps` on a VM seat: checked and kept on the host (the next config carries it),
    /// handed to the running guest at once.
    pub(crate) fn vm_set_apps(&self, n: SeatNumber, body: &seat_proto::RequestBody) -> Result<ResponseBody, ProtoError> {
        let seat_proto::RequestBody::SetApps { apps, .. } = body else {
            return Err(err(ErrorCode::Internal, "not set_apps"));
        };
        let entries: Vec<seat_stream::apps::AppEntry> = apps
            .iter()
            .cloned()
            .map(serde_json::from_value)
            .collect::<Result<_, _>>()
            .map_err(|e| err(ErrorCode::InvalidValue, format!("bad app list: {e}")).with_field("apps").with_reason("bad_app_list"))?;
        seat_stream::apps::validate_apps(&entries)
            .map_err(|e| err(ErrorCode::InvalidValue, e.to_string()).with_field("apps").with_reason(e.code()))?;
        let list: Vec<serde_json::Value> = entries.iter().filter_map(|a| serde_json::to_value(a).ok()).collect();
        let id = self.vm_seat_id(n)?;
        let dir = self.vm_seat_dir(&id);
        std::fs::create_dir_all(&dir)
            .and_then(|_| std::fs::write(dir.join(APPS_FILE), serde_json::to_vec_pretty(&list).unwrap_or_default()))
            .map_err(|e| err(ErrorCode::Os, format!("app list not stored: {e}")))?;
        if let Some(Err(e)) = self.vm_guest_forward(n, body, GUEST_REQUEST_MS) {
            log::warn(&format!("vm seat {n}: app list stored, guest not updated yet: {e}"));
        }
        Ok(ResponseBody::Apps(list))
    }

    /// `set_seat_connect_password` on a VM seat: kept in the host's secret store (the next
    /// config carries it), handed to the running guest at once.
    pub(crate) fn vm_set_connect_password(&self, n: SeatNumber, body: &seat_proto::RequestBody) -> Result<ResponseBody, ProtoError> {
        let seat_proto::RequestBody::SetSeatConnectPassword { password, .. } = body else {
            return Err(err(ErrorCode::Internal, "not set_seat_connect_password"));
        };
        let backend = self.lock().config.seat(n).and_then(|s| s.remote.as_ref().map(|r| r.stream_backend)).unwrap_or_default();
        if backend != seat_core::remote::StreamBackendKind::Pulsar {
            return Err(err(ErrorCode::Unsupported, "connect passwords are a Pulsar backend option").with_reason("backend_sunshine"));
        }
        let n_chars = password.expose().chars().count();
        if n_chars > 128 || password.expose().chars().any(char::is_control) {
            return Err(err(ErrorCode::InvalidValue, "password: 1-128 characters, no control characters").with_field("password"));
        }
        let id = self.vm_seat_id(n)?;
        let value = (n_chars > 0).then_some(password);
        self.secrets
            .set_remote(&id, seat_core::store::RemoteSecretSlot::ConnectPassword, value)
            .map_err(|e| err(ErrorCode::Store, e.to_string()))?;
        match self.vm_guest_forward(n, body, GUEST_REQUEST_MS) {
            Some(Err(e)) => Err(e),
            _ => Ok(ResponseBody::Ack),
        }
    }

    /// `regenerate_seat_identity` on a VM seat: only while the seat is off (as on the
    /// host); the guest makes the new identity before its stream server next starts.
    pub(crate) fn vm_regenerate_identity(&self, n: SeatNumber, confirm: bool) -> Result<ResponseBody, ProtoError> {
        if !confirm {
            return Err(err(ErrorCode::BadRequest, "confirm must be true").with_field("confirm"));
        }
        if self.vm_active(n) {
            return Err(err(ErrorCode::SeatRunning, "stop the seat first").with_reason("seat_running"));
        }
        let dir = self.vm_seat_dir(&self.vm_seat_id(n)?);
        std::fs::create_dir_all(&dir)
            .and_then(|_| std::fs::write(dir.join(REGENERATE_FLAG), b"1"))
            .map_err(|e| err(ErrorCode::Os, format!("identity reset not stored: {e}")))?;
        Ok(ResponseBody::Ack)
    }

    fn set_vm_rt(&self, n: SeatNumber, state: SeatState, degraded: Vec<String>) {
        let now = self.now();
        let runtime = {
            let mut st = self.lock();
            let since = st.remote_rt.get(&n).filter(|o| o.state == state).map(|o| o.since_ms).unwrap_or(now);
            let rt = SeatRuntime {
                seat: n,
                kind: seat_core::config::SeatKind::Remote,
                state,
                since_ms: since,
                degraded,
                ..SeatRuntime::off(n)
            };
            if st.remote_rt.get(&n) == Some(&rt) {
                return;
            }
            st.remote_rt.insert(n, rt.clone());
            rt
        };
        self.events.publish(Event::SeatState { runtime });
    }

    /// `stop_seat` on a VM seat.
    pub(crate) fn stop_vm_seat(&self, n: SeatNumber) -> Result<ResponseBody, ProtoError> {
        let a = {
            let mut v = self.vm.lock();
            let Some(a) = v.active.get_mut(&n) else {
                return Err(err(ErrorCode::SeatNotRunning, "seat is not running"));
            };
            a.stopping = true;
            a.clone()
        };
        let mode = self.lock().config.seat(n).and_then(|s| s.remote.as_ref()?.vm.as_ref().map(|v| v.stop)).unwrap_or_default();
        let _ = self.platform.guest_link_send(&a.vm_id, &GuestMsg::Shutdown);
        self.set_vm_rt(n, SeatState::Stopping, Vec::new());
        log::info(&format!("vm seat {n} stopping ({mode:?})"));
        self.spawn(move |s| {
            if let Err(e) = s.vm_run(&VmOp::Stop { seat_id: a.seat_id.clone(), mode }, None) {
                log::error(&format!("vm seat {n}: stop failed: {e}"));
            }
            s.vm_poll_now();
        });
        Ok(ResponseBody::Ack)
    }

    /// Start the autostart VM seats (product enabled, no safe mode); failures are logged.
    pub(crate) fn vm_autostart(&self) {
        let seats: Vec<SeatNumber> = {
            let st = self.lock();
            if self.may_start(&st, false).is_err() {
                return;
            }
            st.config.seats.iter().filter(|s| is_vm(s) && s.autostart).map(|s| s.number).collect()
        };
        for n in seats {
            if self.vm.lock().active.contains_key(&n) {
                continue;
            }
            if let Err(e) = self.start_vm_seat(n) {
                log::error(&format!("vm seat {n} not started: {e}"));
            }
        }
    }

    /// Stop every active VM seat (service stop, suspend). Returns the seats asked to stop.
    pub(crate) fn vm_stop_all(&self) -> Vec<SeatNumber> {
        let seats: Vec<SeatNumber> = self.vm.lock().active.iter().filter(|(_, a)| !a.stopping).map(|(n, _)| *n).collect();
        for n in &seats {
            let _ = self.stop_vm_seat(*n);
        }
        seats
    }

    /// Any VM seat booting, running or stopping.
    pub(crate) fn vm_active(&self, n: SeatNumber) -> bool {
        self.vm.lock().active.contains_key(&n)
    }

    /// Tick: poll the VM states (off the tick thread) at most every [`VM_POLL_MS`] while a
    /// VM seat is active.
    pub(crate) fn vm_tick(&self) {
        let due = {
            let v = self.vm.lock();
            !v.active.is_empty() && !v.polling && self.now().saturating_sub(v.last_poll_ms) >= VM_POLL_MS
        };
        if due {
            self.vm.lock().polling = true;
            self.spawn(|s| {
                s.vm_poll_now();
                s.vm.lock().polling = false;
            });
        }
        self.vm_publish_notices();
    }

    /// The guests' stream notices as the seats' `stream` events (pair dialog, streaming).
    fn vm_publish_notices(&self) {
        use seat_proto::StreamEvent as E;
        let active: Vec<(SeatNumber, String)> = self.vm.lock().active.iter().map(|(n, a)| (*n, a.vm_id.clone())).collect();
        for (seat, vm_id) in active {
            for (kind, data) in self.platform.guest_link_notices(&vm_id) {
                let event = match kind.as_str() {
                    "pair_pending" => E::PairPending(data),
                    "pair_ended" => E::PairEnded(data),
                    "stream_started" => E::StreamStarted(data),
                    "stream_ended" => E::StreamEnded(data),
                    "otp_changed" => E::OtpChanged(data),
                    "trust_changed" => E::TrustChanged(data),
                    "relay_auth_required" => E::RelayAuthRequired(data),
                    _ => continue,
                };
                self.events.publish(Event::Stream { seat, event });
            }
        }
    }

    /// Read the VM states and update the VM seats' runtimes; finished stops are cleaned up.
    pub(crate) fn vm_poll_now(&self) {
        let began = self.now();
        let facts = match self.vm_refresh() {
            Ok(f) => f,
            Err(e) => {
                log::warn(&format!("vm seats: status failed: {e}"));
                return;
            }
        };
        let active: Vec<(SeatNumber, Active)> = self.vm.lock().active.iter().map(|(n, a)| (*n, a.clone())).collect();
        let restart_gpus = self.lock().config.settings.remote.vm.restart_gpus_on_shutdown;
        let mut gpu_stopped = false;
        for (n, a) in active {
            let vm = facts.vms.iter().find(|v| v.seat_id == a.seat_id);
            let vm_id = vm.map(|v| v.vm_id.clone()).unwrap_or_else(|| a.vm_id.clone());
            let guest = self.platform.guest_link_phase(&vm_id);
            let state = vm_seat_state(vm.map(|v| v.state.as_str()), guest.as_ref(), &a.user, a.stopping);
            // A start in flight (or one that finished after this status read began) is not
            // a finished stop: leave the seat to the next poll.
            if state == SeatState::Off && !a.stopping && (a.starting || began < a.start_done_ms) {
                continue;
            }
            let degraded = if guest.as_ref().is_some_and(|(p, _)| *p == GuestPhase::Streaming) { vec!["streaming".to_string()] } else { Vec::new() };
            if state == SeatState::Off {
                self.vm.lock().active.remove(&n);
                self.platform.guest_link_offer(&vm_id, None);
                gpu_stopped |= a.gpu;
                log::info(&format!("vm seat {n} stopped"));
            }
            self.set_vm_rt(n, state, degraded);
        }
        if gpu_stopped && restart_gpus {
            log::info("vm seats: restarting the display adapters after a GPU-P VM stopped");
            if let Err(e) = self.vm_run(&VmOp::RestartGpus, None) {
                log::warn(&format!("vm seats: GPU restart failed: {e}"));
            }
        }
    }

    /// VM seats removed by `apply_config`: VM, disk, forwards, rule, shares.
    pub(crate) fn forget_vm_seats(&self, removed: &[(SeatNumber, seat_core::remote::RemoteSeatRecord)]) {
        for (n, rec) in removed.iter().filter(|(_, r)| r.vm.is_some()) {
            let mut seat = rec.to_seat(*n);
            seat.number = *n;
            let spec = {
                let st = self.lock();
                let facts = self.vm.lock().facts.clone();
                self.vm_spec(&st, &seat, facts.as_ref())
            };
            let Some(spec) = spec else { continue };
            match self.vm_run(&VmOp::Remove { spec: spec.clone() }, None) {
                Ok(_) => {
                    log::info(&format!("vm seat {n} deleted: VM, disk, forwards and shares removed"));
                    self.vm_journal(|j| j.remove_seat(&spec.seat_id));
                }
                Err(e) => log::error(&format!("vm seat {n} deleted, cleanup failed: {e}")),
            }
            self.lock().remote_rt.remove(n);
        }
    }

    /// VM id of a running (not stopping) VM seat.
    fn vm_running_id(&self, n: SeatNumber) -> Option<String> {
        self.vm.lock().active.get(&n).filter(|a| !a.stopping).map(|a| a.vm_id.clone())
    }

    /// A seat page request for a VM seat, answered by the guest (its stream server, its
    /// account) over the guest link. `None` = the seat's VM is not running.
    pub(crate) fn vm_guest_forward(&self, n: SeatNumber, body: &seat_proto::RequestBody, timeout_ms: u64) -> Option<Result<ResponseBody, ProtoError>> {
        let vm_id = self.vm_running_id(n)?;
        let request = match serde_json::to_value(body) {
            Ok(v) => v,
            Err(e) => return Some(Err(err(ErrorCode::Internal, format!("request not encoded: {e}")))),
        };
        Some(self.platform.guest_link_request(&vm_id, request, timeout_ms).and_then(|v| {
            serde_json::from_value::<ResponseBody>(v)
                .map_err(|e| err(ErrorCode::Internal, format!("guest reply not understood: {e}")).with_reason("vm_guest_reply"))
        }))
    }

    /// `submit_pin` on a VM seat: like the host engine, the answer is an `ack` and the
    /// outcome arrives as a `pair_ended` stream event. The guest sends that event itself; a
    /// link failure is reported here.
    pub(crate) fn vm_submit_pin(&self, n: SeatNumber, body: &seat_proto::RequestBody) -> Result<ResponseBody, ProtoError> {
        if self.vm_running_id(n).is_none() {
            return Err(err(ErrorCode::SeatNotRunning, "seat is not running"));
        }
        let request_id = match body {
            seat_proto::RequestBody::SubmitPin { request_id, .. } => *request_id,
            _ => None,
        };
        let body = body.clone();
        self.spawn(move |s| {
            if let Some(Err(e)) = s.vm_guest_forward(n, &body, GUEST_PAIR_MS) {
                let result = e.reason.clone().unwrap_or_else(|| "vm_guest_unreachable".into());
                log::warn(&format!("vm seat {n}: pairing not forwarded: {e}"));
                s.events.publish(Event::Stream {
                    seat: n,
                    event: seat_proto::StreamEvent::PairEnded(serde_json::json!({"request_id": request_id, "result": result})),
                });
            }
        });
        Ok(ResponseBody::Ack)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states_map_from_vm_and_guest() {
        assert_eq!(vm_seat_state(None, None, "u", false), SeatState::Off);
        assert_eq!(vm_seat_state(Some("off"), None, "u", true), SeatState::Off, "stop finished");
        assert_eq!(vm_seat_state(Some("running"), None, "u", false), SeatState::Starting { phase: StartPhase::WaitingForSession });
        assert_eq!(
            vm_seat_state(Some("running"), Some(&(GuestPhase::Configuring, None)), "u", false),
            SeatState::Starting { phase: StartPhase::LoggingOn }
        );
        assert_eq!(vm_seat_state(Some("running"), Some(&(GuestPhase::Streaming, None)), "u", false), SeatState::SignedIn { user: "u".into() });
        assert!(matches!(vm_seat_state(Some("running"), Some(&(GuestPhase::Failed, Some("x".into()))), "u", false), SeatState::Error { .. }));
        assert_eq!(vm_seat_state(Some("running"), None, "u", true), SeatState::Stopping);
        assert_eq!(vm_seat_state(Some("saved"), None, "u", false), SeatState::Off);
        assert_eq!(vm_seat_state(Some("starting"), None, "u", false), SeatState::Starting { phase: StartPhase::WaitingForHost });
    }
}
