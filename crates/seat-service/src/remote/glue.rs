// SPDX-License-Identifier: GPL-3.0-only
//! The service side of the remote seat worker (merge step 6): requests, starts and stops of
//! remote seats, boot / stop / power hooks, the remote part of `get_status` and the mapping
//! of the worker's events onto the event hub.
//!
//! Lock order: worker, then (briefly) the service state; the state lock is never held while
//! the worker is called. The config document is copied into the worker's [`DocView`] under
//! the worker lock, and worker-owned fields come back through
//! [`super::merge_worker_fields`] + `commit_config`.

use super::{DocView, RemoteHost, RemoteWorker, SharedDoc, from_legacy, runtime_of, to_legacy};
use crate::log;
use crate::service::Service;
use seat_core::config::SeatKind;
use seat_core::state::SeatRuntime;
use seat_core::SeatNumber;
use seat_proto::remote::legacy::RequestBody as L;
use seat_proto::remote::{MappedEvent, map_legacy_event};
use seat_proto::{ErrorCode, Event, ProtoError, RequestBody, ResponseBody};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

/// Reason of remote-kind answers when the service runs without a remote seat worker (the
/// remote packages could not be wired, see the service log).
pub const REMOTE_WORKER_UNAVAILABLE: &str = "remote_worker_unavailable";

/// Longest wait for the remote seats to finish stopping (service stop, resume), as in
/// Pulsar Split (`engine::STOP_WAIT`).
pub const REMOTE_STOP_WAIT_MS: u64 = 90_000;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The worker and what the service keeps next to it.
pub struct RemoteSlot {
    worker: Mutex<Box<dyn RemoteWorker>>,
    doc: SharedDoc,
    host: Option<Arc<dyn RemoteHost>>,
    /// Last worker event `seq` already published.
    seq: AtomicU64,
}

impl RemoteSlot {
    pub fn new(worker: Box<dyn RemoteWorker>, doc: SharedDoc, host: Option<Arc<dyn RemoteHost>>) -> Self {
        Self { worker: Mutex::new(worker), doc, host, seq: AtomicU64::new(0) }
    }
}

fn unavailable(what: &str) -> ProtoError {
    ProtoError::new(ErrorCode::Unsupported, format!("{what}: the remote seat worker is not available (see the service log)"))
        .with_reason(REMOTE_WORKER_UNAVAILABLE)
}

fn not_configured(seat: SeatNumber) -> ProtoError {
    ProtoError::new(ErrorCode::NotFound, format!("seat {seat} is not configured"))
}

impl Service {
    /// Run `f` on the worker with the current config document, then write worker-owned
    /// fields back, refresh the remote runtimes and publish the worker's new events.
    /// `None` without a worker.
    pub(crate) fn with_remote<T>(&self, f: impl FnOnce(&mut dyn RemoteWorker) -> T) -> Option<T> {
        let slot = self.remote.as_ref()?;
        let (out, status, events, changed) = {
            let mut w = lock(&slot.worker);
            let snapshot = self.lock().config.clone();
            *lock(&slot.doc) = DocView { config: snapshot, dirty: false };
            let out = f(w.as_mut());
            let status = w.status().ok();
            let events = w.events_after(slot.seq.load(Ordering::SeqCst));
            let changed = {
                let mut d = lock(&slot.doc);
                std::mem::take(&mut d.dirty).then(|| d.config.clone())
            };
            (out, status, events, changed)
        };
        let now = self.now();
        let names: BTreeMap<String, SeatNumber> = {
            let mut st = self.lock();
            if let Some(doc) = changed {
                let mut cfg = st.config.clone();
                if super::merge_worker_fields(&mut cfg, &doc)
                    && let Err(e) = self.commit_config(&mut st, cfg)
                {
                    log::error(&format!("remote seat fields not saved: {e}"));
                }
            }
            let names: BTreeMap<String, SeatNumber> = st
                .config
                .seats
                .iter()
                .filter(|s| s.kind == SeatKind::Remote)
                .map(|s| (s.name.to_ascii_lowercase(), s.number))
                .collect();
            if let Some(status) = status {
                let mut rt: BTreeMap<SeatNumber, SeatRuntime> = BTreeMap::new();
                for s in &status.seats {
                    if let Some(n) = names.get(&s.name.to_ascii_lowercase()) {
                        let mut r = runtime_of(*n, s, now);
                        // Keep `since_ms` while the state stays the same.
                        if let Some(old) = st.remote_rt.get(n)
                            && old.state == r.state
                        {
                            r.since_ms = old.since_ms;
                        }
                        rt.insert(*n, r);
                    }
                }
                // VM seats are not the worker's: keep their runtimes.
                let vm_seats: Vec<SeatNumber> =
                    st.config.seats.iter().filter(|s| s.remote.as_ref().is_some_and(|r| r.is_vm())).map(|s| s.number).collect();
                for n in vm_seats {
                    if let Some(r) = st.remote_rt.get(&n) {
                        rt.insert(n, r.clone());
                    }
                }
                st.remote_rt = rt;
            }
            names
        };
        if events.next > slot.seq.load(Ordering::SeqCst) {
            slot.seq.store(events.next, Ordering::SeqCst);
        }
        for ev in &events.events {
            let seat = ev.seat.as_ref().and_then(|n| names.get(&n.to_ascii_lowercase()).copied());
            match map_legacy_event(ev, seat) {
                MappedEvent::SeatState(n) => {
                    let rt = self.lock().remote_rt.get(&n).cloned();
                    if let Some(runtime) = rt {
                        self.events.publish(Event::SeatState { runtime });
                    }
                }
                MappedEvent::Event(e) => {
                    self.events.publish(e);
                }
                MappedEvent::Dropped => {}
            }
        }
        Some(out)
    }

    pub(crate) fn has_remote_worker(&self) -> bool {
        self.remote.is_some()
    }

    /// Name of a configured remote seat.
    fn remote_name(&self, seat: SeatNumber) -> Option<String> {
        self.lock().config.seat(seat).filter(|s| s.is_remote()).map(|s| s.name.clone())
    }

    /// Protocol 3 remote-kind requests (access and seat kind already checked).
    pub(crate) fn remote_kind_request(&self, body: &RequestBody) -> Result<ResponseBody, ProtoError> {
        let seat = match body {
            RequestBody::TestSeatCredentials { seat } | RequestBody::EnableUnlockAccount { seat, .. } => Some(*seat),
            // `seat:<N>` of a VM seat: the stream log lives in the guest.
            RequestBody::GetLogTail { source, .. } => source
                .strip_prefix("seat:")
                .and_then(|x| x.trim().parse::<u8>().ok())
                .and_then(SeatNumber::new)
                .filter(|s| self.is_vm_seat(*s)),
            other => other.seat_kind().map(|(s, _)| s),
        };
        let kind = match seat {
            Some(s) => Some(self.lock().config.seat(s).map(|c| c.kind).ok_or_else(|| not_configured(s))?),
            None => None,
        };
        let what = serde_json::to_value(body).ok().and_then(|v| v["type"].as_str().map(str::to_owned)).unwrap_or_default();
        // The two account requests also serve local seats.
        if kind == Some(SeatKind::Local) {
            return self.local_account_request(body, &what);
        }
        if let Some(n) = seat
            && self.is_vm_seat(n)
        {
            return self.vm_kind_request(n, body, &what);
        }
        let legacy = to_legacy(body, |s| self.remote_name(s)).ok_or_else(|| match body {
            // `seat:<N>` that is not a configured remote seat.
            RequestBody::GetLogTail { .. } => {
                ProtoError::new(ErrorCode::InvalidValue, "source: service | seat:<remote seat number>").with_field("source")
            }
            _ => ProtoError::new(ErrorCode::Internal, format!("{what}: no remote mapping")).with_reason("unmapped"),
        })?;
        let r = self.with_remote(|w| w.call(legacy)).ok_or_else(|| unavailable(&what))?;
        from_legacy(r?)
    }

    /// Remote-kind requests of a VM seat (review 2026-11-17: full seat page). What the host
    /// keeps (app list, connect password, a pending identity reset) is answered here and
    /// handed to the guest; the rest is answered by the guest's stream server or account
    /// over the guest link (`vm_guest_forward`). With the VM off, the answers match an
    /// off host seat.
    fn vm_kind_request(&self, n: SeatNumber, body: &RequestBody, what: &str) -> Result<ResponseBody, ProtoError> {
        use crate::vm_seats::GUEST_REQUEST_MS;
        let not_running = || ProtoError::new(ErrorCode::ServiceUnavailable, "seat is not running");
        let forward = |off: &dyn Fn() -> Result<ResponseBody, ProtoError>| match self.vm_guest_forward(n, body, GUEST_REQUEST_MS) {
            Some(r) => r,
            None => off(),
        };
        match body {
            RequestBody::SubmitPin { .. } => self.vm_submit_pin(n, body),
            RequestBody::GetApps { .. } => self.vm_get_apps(n),
            RequestBody::SetApps { .. } => self.vm_set_apps(n, body),
            RequestBody::SetSeatConnectPassword { .. } => self.vm_set_connect_password(n, body),
            RequestBody::RegenerateSeatIdentity { confirm, .. } => self.vm_regenerate_identity(n, *confirm),
            RequestBody::GetPairState { .. } => forward(&|| {
                let backend = self.lock().config.seat(n).and_then(|s| s.remote.as_ref().map(|r| r.stream_backend)).unwrap_or_default();
                Ok(ResponseBody::PairState(seat_proto::remote::PairState { backend, ..Default::default() }))
            }),
            RequestBody::GetRemoteRuntime { .. } => forward(&|| Ok(ResponseBody::RemoteRuntime(Default::default()))),
            RequestBody::GetLogTail { .. } => forward(&|| Ok(ResponseBody::LogLines(Vec::new()))),
            // The guest re-applies the seat user (enabled, stored password) at every start.
            RequestBody::EnableUnlockAccount { confirm, .. } => forward(&|| {
                if !confirm {
                    return Err(ProtoError::new(ErrorCode::BadRequest, "confirm must be true").with_field("confirm"));
                }
                Ok(ResponseBody::Ack)
            }),
            RequestBody::TestSeatCredentials { .. } => forward(&|| {
                let user = self.lock().config.seat(n).map(|s| match &s.account {
                    seat_core::config::SeatAccount::Local { user } | seat_core::config::SeatAccount::Domain { user, .. } => user.clone(),
                    seat_core::config::SeatAccount::LogonScreen => String::new(),
                });
                if user.unwrap_or_default().is_empty() || !self.secrets.has(n) {
                    return Err(ProtoError::new(ErrorCode::MissingCredentials, "seat needs a user and a password").with_field("user_name"));
                }
                // The guest creates the user with exactly this password when the VM starts.
                Ok(ResponseBody::CredentialTest(seat_proto::remote::CredentialTest { ok: true, ..Default::default() }))
            }),
            RequestBody::CancelPair { .. }
            | RequestBody::ListPairedClients { .. }
            | RequestBody::UnpairClient { .. }
            | RequestBody::UnpairAll { .. }
            | RequestBody::AnswerPeerRequest { .. }
            | RequestBody::GetSeatOtp { .. }
            | RequestBody::RotateSeatOtp { .. }
            | RequestBody::AnswerRelayAuth { .. }
            | RequestBody::SetSeatDisplayMode { .. } => forward(&|| Err(not_running())),
            _ => Err(ProtoError::new(ErrorCode::Unsupported, format!("{what}: not a seat request")).with_reason("vm_seat")),
        }
    }

    /// `test_seat_credentials` / `enable_unlock_account` for a local seat: the worker's
    /// platform does the account work, the user and password come from the Seat side.
    fn local_account_request(&self, body: &RequestBody, what: &str) -> Result<ResponseBody, ProtoError> {
        let user_of = |seat: SeatNumber| -> String {
            match self.lock().config.seat(seat).map(|s| s.account.clone()) {
                Some(seat_core::config::SeatAccount::Local { user }) => user,
                Some(seat_core::config::SeatAccount::Domain { user, .. }) => user,
                _ => String::new(),
            }
        };
        match body {
            RequestBody::TestSeatCredentials { seat } => {
                let user = user_of(*seat);
                let password = self.secrets.get(*seat).map_err(|e| ProtoError::new(ErrorCode::Store, e.to_string()))?;
                let r = self.with_remote(|w| w.test_user_credentials(&user, password)).ok_or_else(|| unavailable(what))?;
                r.map(ResponseBody::CredentialTest)
            }
            RequestBody::EnableUnlockAccount { seat, enable, unlock, confirm } => {
                if !confirm {
                    return Err(ProtoError::new(ErrorCode::BadRequest, "confirm must be true").with_field("confirm"));
                }
                let user = user_of(*seat);
                let r = self.with_remote(|w| w.enable_unlock_user(&user, *enable, *unlock)).ok_or_else(|| unavailable(what))?;
                r.map(|_| ResponseBody::Ack)
            }
            _ => Err(ProtoError::new(ErrorCode::Internal, "not an account request")),
        }
    }

    /// Start the remote seats among `seats` (explicit `start_seats`): `session_unavailable`
    /// without concurrent sessions; the worker's first refusal is the answer.
    pub(crate) fn start_remote_seats(&self, seats: &[SeatNumber]) -> Result<(), ProtoError> {
        let (remote, hosts): (Vec<(SeatNumber, String)>, bool) = {
            let st = self.lock();
            let r = seats
                .iter()
                .filter_map(|n| st.config.seat(*n).filter(|s| s.is_remote()).map(|s| (*n, s.name.clone())))
                .collect();
            (r, st.config.settings.backends.session.hosts_remote_seats())
        };
        // VM seats run their own VM (no concurrent host sessions needed).
        type Named = Vec<(SeatNumber, String)>;
        let (vms, remote): (Named, Named) = remote.into_iter().partition(|(n, _)| self.is_vm_seat(*n));
        for (n, _) in &vms {
            self.start_vm_seat(*n)?;
        }
        let Some((first, _)) = remote.first() else { return Ok(()) };
        if !hosts {
            return Err(ProtoError::new(ErrorCode::SessionUnavailable, format!("seat {first}: remote seats need concurrent sessions"))
                .with_reason("session_unavailable"));
        }
        for (n, name) in remote {
            let r = self.with_remote(|w| w.call(L::StartSeat { name })).ok_or_else(|| unavailable("start_seats"))?;
            if let Err(e) = r {
                log::warn(&format!("remote seat {n} not started: {e}"));
                return Err(e);
            }
            log::info(&format!("remote seat {n} starting"));
        }
        Ok(())
    }

    /// Remote seats of `old` that `new` no longer has (matched by seat id, so a seat whose
    /// kind changed to local counts as removed).
    pub(crate) fn removed_remote_seats(
        old: &seat_core::config::Config,
        new: &seat_core::config::Config,
    ) -> Vec<(SeatNumber, seat_core::remote::RemoteSeatRecord)> {
        let kept: Vec<String> = new.seats.iter().filter_map(|s| s.remote_record()).map(|r| r.id).collect();
        old.seats
            .iter()
            .filter_map(|s| s.remote_record().map(|r| (s.number, r)))
            .filter(|(_, r)| !kept.contains(&r.id))
            .collect()
    }

    /// `apply_config` must not delete a remote seat that is booting, running or stopping
    /// (Split's `delete_seat` answered `seat_running` too).
    pub(crate) fn check_remote_removal(&self, removed: &[(SeatNumber, seat_core::remote::RemoteSeatRecord)]) -> Result<(), ProtoError> {
        if removed.is_empty() {
            return Ok(());
        }
        let busy = removed
            .iter()
            .find(|(n, r)| r.vm.is_some() && self.vm_active(*n))
            .map(|(n, _)| *n)
            .or_else(|| self.with_remote(|w| removed.iter().find(|(_, r)| r.vm.is_none() && w.seat_active(&r.id)).map(|(n, _)| *n)).flatten());
        match busy {
            Some(n) => Err(ProtoError::new(ErrorCode::SeatRunning, format!("seat {n}: stop the seat before deleting it"))
                .with_reason("seat_running")
                .with_field(format!("seats.{n}"))),
            None => Ok(()),
        }
    }

    /// After `apply_config` removed remote seats: the worker deletes their stream backend
    /// secrets (LSA identity, connect password, relay key), seat folder, logs and firewall
    /// rules, and drops their runtime.
    pub(crate) fn forget_remote_seats(&self, removed: &[(SeatNumber, seat_core::remote::RemoteSeatRecord)]) {
        self.forget_vm_seats(removed);
        for (n, record) in removed.iter().filter(|(_, r)| r.vm.is_none()) {
            match self.with_remote(|w| w.forget_seat(record)) {
                Some(Ok(())) => log::info(&format!("remote seat {n} deleted: files and secrets removed")),
                Some(Err(e)) => log::error(&format!("remote seat {n} deleted, cleanup failed: {e}")),
                None => log::warn(&format!("remote seat {n} deleted without the remote seat worker; files and secrets kept")),
            }
            self.lock().remote_rt.remove(n);
        }
    }

    /// After `apply_config` changed `settings.remote`: the worker applies what lives
    /// outside the document (VM round 2, 2026-11-17).
    pub(crate) fn remote_global_changed(&self, old: &seat_core::remote::RemoteSettings, new: &seat_core::remote::RemoteSettings) {
        if old == new {
            return;
        }
        match self.with_remote(|w| w.global_changed(old, new)) {
            Some(Ok(())) | None => {}
            Some(Err(e)) => log::warn(&format!("remote settings not applied outside the config: {e}")),
        }
    }

    /// After `apply_config` changed the seat list: the worker refreshes the remote seat
    /// accounts the protocol provider admits (VM round 2, 2026-11-17).
    pub(crate) fn remote_seats_changed(&self) {
        let _ = self.with_remote(|w| w.seats_changed());
    }

    /// `stop_seat` on a remote seat.
    pub(crate) fn stop_remote_seat(&self, seat: SeatNumber) -> Result<ResponseBody, ProtoError> {
        if self.is_vm_seat(seat) {
            return self.stop_vm_seat(seat);
        }
        let name = self.remote_name(seat).ok_or_else(|| not_configured(seat))?;
        let active = self.lock().remote_rt.get(&seat).is_some_and(|r| {
            use seat_core::state::SeatState as S;
            matches!(r.state, S::Starting { .. } | S::AtLogon | S::SignedIn { .. } | S::Locked { .. } | S::Stopping)
        });
        if !active {
            return Err(ProtoError::new(ErrorCode::SeatNotRunning, "seat is not running"));
        }
        let r = self.with_remote(|w| w.call(L::StopSeat { name })).ok_or_else(|| unavailable("stop_seat"))?;
        r.map(|_| ResponseBody::Ack)
    }

    /// Autostart remote seats (boot, resume, `start_seats` without a list). Needs the
    /// product enabled, no safe mode and concurrent sessions; failures are logged.
    pub(crate) fn start_remote_autostart(&self) {
        self.vm_autostart();
        if !self.has_remote_worker() {
            return;
        }
        {
            let st = self.lock();
            if self.may_start(&st, false).is_err() {
                return;
            }
            if !st.config.settings.backends.session.hosts_remote_seats() {
                if st.config.seats.iter().any(|s| s.is_remote() && s.autostart && !s.remote.as_ref().is_some_and(|r| r.is_vm())) {
                    log::info("remote seats not started: the session backend has no concurrent sessions");
                }
                return;
            }
        }
        let names = self.with_remote(|w| w.autostart_seat_names()).unwrap_or_default();
        for name in names {
            match self.with_remote(|w| w.call(L::StartSeat { name: name.clone() })) {
                Some(Ok(_)) => log::info(&format!("remote seat '{name}' starting")),
                Some(Err(e)) => log::error(&format!("remote seat '{name}' not started: {e}")),
                None => {}
            }
        }
    }

    /// Boot: the engine's host preparation (coexistence, termwrap check, RDS, sweeps).
    pub(crate) fn remote_boot(&self) {
        let Some(slot) = &self.remote else { return };
        let Some(host) = slot.host.clone() else { return };
        let global = self.lock().config.settings.remote.clone();
        let users = self.with_remote(|w| w.seat_users()).unwrap_or_default();
        host.service_start(&global, &users);
    }

    /// Tick: apply the engine's progress reports.
    pub(crate) fn remote_tick(&self) {
        self.vm_tick();
        self.with_remote(|w| w.drain_reports());
    }

    /// Wait until no remote seat is booting, running or stopping (at most
    /// `remote_stop_wait_ms`).
    fn wait_remote_idle(&self) {
        let wait = self.remote_stop_wait_ms.load(Ordering::SeqCst);
        let end = std::time::Instant::now() + Duration::from_millis(wait);
        loop {
            let active = self
                .with_remote(|w| {
                    w.drain_reports();
                    w.active_seats()
                })
                .unwrap_or(0);
            if active == 0 || std::time::Instant::now() >= end {
                if active > 0 {
                    log::warn(&format!("{active} remote seat(s) still active after {wait} ms"));
                }
                return;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    /// Service stop: stop every remote seat, wait, final sweep.
    pub(crate) fn remote_shutdown(&self) {
        self.vm_stop_all();
        let Some(slot) = &self.remote else { return };
        self.with_remote(|w| w.stop_all());
        self.wait_remote_idle();
        let users = self.with_remote(|w| w.seat_users()).unwrap_or_default();
        if let Some(h) = &slot.host {
            h.service_stop(&users);
        }
    }

    /// Power suspend (Split, spec 20 §7): remote seats stop. Local seats keep their sessions.
    pub fn on_suspend(&self) {
        let vms = self.vm_stop_all();
        if !vms.is_empty() {
            log::info(&format!("suspend: stopping {} VM seat(s)", vms.len()));
        }
        if let Some(stopped) = self.with_remote(|w| w.on_suspend()) {
            log::info(&format!("suspend: stopping {} remote seat(s)", stopped.len()));
        }
    }

    /// Resume: once the old remote seats finished stopping, start the autostart ones again
    /// (off the caller's thread).
    pub(crate) fn remote_resume(&self) {
        if !self.has_remote_worker() {
            self.spawn(|s| s.vm_autostart());
            return;
        }
        self.spawn(|s| {
            s.wait_remote_idle();
            s.start_remote_autostart();
        });
    }
}
