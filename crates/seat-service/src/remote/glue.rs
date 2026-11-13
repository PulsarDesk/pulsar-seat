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

    /// `stop_seat` on a remote seat.
    pub(crate) fn stop_remote_seat(&self, seat: SeatNumber) -> Result<ResponseBody, ProtoError> {
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
        if !self.has_remote_worker() {
            return;
        }
        {
            let st = self.lock();
            if self.may_start(&st, false).is_err() {
                return;
            }
            if !st.config.settings.backends.session.hosts_remote_seats() {
                if st.config.seats.iter().any(|s| s.is_remote() && s.autostart) {
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
        if let Some(stopped) = self.with_remote(|w| w.on_suspend()) {
            log::info(&format!("suspend: stopping {} remote seat(s)", stopped.len()));
        }
    }

    /// Resume: once the old remote seats finished stopping, start the autostart ones again
    /// (off the caller's thread).
    pub(crate) fn remote_resume(&self) {
        if !self.has_remote_worker() {
            return;
        }
        self.spawn(|s| {
            s.wait_remote_idle();
            s.start_remote_autostart();
        });
    }
}
