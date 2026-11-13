// SPDX-License-Identifier: GPL-3.0-only
//! `rds_loopback` (default): every extra seat is an RDP session on Windows Server with the
//! RD Session Host role; a seat host in the console (or dedicated host) session connects
//! to it over loopback `127.0.0.2` and shows it on the seat's monitors (spec 30 §6.1 A,
//! spec 00 §5.2.1). Licence-clean; needs RDS CALs after the grace period.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use seat_core::backend::session::LoopbackListenerPlan;
use seat_core::config::LoopbackListenerSettings;

use crate::loopback_listener::{ListenerState, ListenerWant, LoopbackListener};

use seat_core::SeatNumber;
use seat_core::backend::session::{
    HostProbe, KnownSession, SeatStartRequest, SessionBackend, SessionBackendKind, SessionCaps, SessionInfo, StartStep,
};
use seat_core::backend::{Availability, BackendResult, ErrorKind};
use seat_core::config::GlobalSettings;

use super::SessionDeps;
use super::rdp::RdpCore;
use crate::edition::{HostClass, arch_name, classify};
use crate::error::{err, reason};
use crate::rds::{RdsApplier, rdp_denied, termservice_is_stock};

/// RDP session cap when the listener sets none (the seat model has 16 seats; seat 1 is
/// the console under the default topology).
const DEFAULT_MAX: u8 = seat_core::MAX_SEATS;

pub struct RdsLoopbackBackend {
    d: SessionDeps,
    core: RdpCore,
    /// Seat cap from the last probe.
    max: Mutex<u8>,
    topology: Mutex<seat_core::backend::session::HostTopology>,
    /// Second listener for passwordless seats (`crate::loopback_listener`).
    second: SecondListener,
}

/// Bookkeeping of the second, loopback-only listener (architecture §4.1 design (a)).
struct SecondListener {
    /// Offered by this backend (`rds_loopback` yes, `termsrv_wrapper` no).
    offered: bool,
    plan: Mutex<LoopbackListenerPlan>,
    settings: Mutex<LoopbackListenerSettings>,
    state: Mutex<ListenerState>,
    /// `prepare_host` ran since the last `restore_host` (the plan may then re-apply).
    prepared: AtomicBool,
    /// Serialises ensure / remove / retry.
    busy: Mutex<()>,
    /// Notes beside the state (`loopback_listener_unload_pending:<port>`,
    /// `loopback_listener_exposed:<port>`): a removed listener TermService still serves.
    pending: Mutex<Vec<String>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl RdsLoopbackBackend {
    pub fn new(d: SessionDeps) -> Self {
        Self::build(d, true)
    }

    /// Same backend without the second listener (the experimental wrapper on client SKUs).
    pub fn without_second_listener(d: SessionDeps) -> Self {
        Self::build(d, false)
    }

    fn build(d: SessionDeps, offered: bool) -> Self {
        Self {
            d,
            core: RdpCore::new(),
            max: Mutex::new(DEFAULT_MAX),
            topology: Mutex::new(Default::default()),
            second: SecondListener {
                offered,
                plan: Mutex::new(LoopbackListenerPlan::default()),
                settings: Mutex::new(LoopbackListenerSettings::default()),
                state: Mutex::new(ListenerState::Unknown),
                prepared: AtomicBool::new(false),
                busy: Mutex::new(()),
                pending: Mutex::new(Vec::new()),
            },
        }
    }

    fn listener(&self) -> LoopbackListener {
        LoopbackListener {
            reg: self.d.registry.clone(),
            host: self.d.loopback.clone(),
            wts: self.d.wts.clone(),
            log: self.d.listener_journal.clone(),
            clock: self.d.wall_clock.clone(),
        }
    }

    /// Current state of the second listener.
    pub fn listener_state(&self) -> ListenerState {
        lock(&self.second.state).clone()
    }

    /// Bring the second listener in line with the stored settings and plan.
    fn ensure_listener(&self) -> Vec<String> {
        if !self.second.offered {
            return Vec::new();
        }
        let _g = lock(&self.second.busy);
        self.ensure_listener_locked()
    }

    /// [`Self::ensure_listener`] with `busy` held.
    fn ensure_listener_locked(&self) -> Vec<String> {
        // `restore_host` (disable / uninstall) clears `prepared` before it takes `busy`: a
        // plan update that raced it must not re-create what it just removed.
        if !self.second.prepared.load(Ordering::SeqCst) {
            return Vec::new();
        }
        let s = lock(&self.second.settings).clone();
        let plan = lock(&self.second.plan).clone();
        let want = ListenerWant { enabled: s.enabled, port: s.port, accounts: &plan.accounts, passwordless_seats: plan.passwordless_seats };
        let (state, mut notes) = match self.listener().ensure(&want) {
            Ok((st, rep)) => {
                let mut n = rep.notes;
                n.extend(rep.unresolved.iter().map(|u| format!("loopback_listener_account_unresolved:{u}")));
                (st, n)
            }
            Err(e) => {
                log::warn!("loopback listener: {e}");
                (ListenerState::Unavailable { reason: e.code().into() }, Vec::new())
            }
        };
        log::info!("loopback listener: {}", state.note());
        self.remember_pending(&notes);
        notes.push(state.note());
        *lock(&self.second.state) = state;
        notes
    }

    /// Keep the unload-pending / exposed notes for the probe (and log an exposure).
    fn remember_pending(&self, notes: &[String]) {
        use crate::loopback_listener::notes as n;
        let p: Vec<String> = notes.iter().filter(|x| x.starts_with(n::UNLOAD_PENDING) || x.starts_with(n::EXPOSED)).cloned().collect();
        *lock(&self.second.pending) = p;
    }

    /// What a passwordless seat start may use right now (`None` = listener not offered or
    /// turned off). Re-checks health; a broken or refused listener is ensured again (a
    /// fixed firewall, a removed rule, a listener that bound late), a pending TermService
    /// restart is retried.
    fn second_for_start(&self) -> Option<Result<u16, String>> {
        if !self.second.offered || !lock(&self.second.settings).enabled {
            return None;
        }
        let _g = lock(&self.second.busy);
        let l = self.listener();
        let s = lock(&self.second.settings).clone();
        let plan = lock(&self.second.plan).clone();
        let want = ListenerWant { enabled: s.enabled, port: s.port, accounts: &plan.accounts, passwordless_seats: plan.passwordless_seats };
        let st = lock(&self.second.state).clone();
        let st = match st {
            ListenerState::Ready { port } if port == want.port && l.health(&want).is_ok() => ListenerState::Ready { port },
            ListenerState::PendingRestart { port } if port == want.port => l.retry_pending(&want),
            ListenerState::Disabled | ListenerState::NotNeeded => st,
            _ => {
                self.ensure_listener_locked();
                lock(&self.second.state).clone()
            }
        };
        *lock(&self.second.state) = st.clone();
        Some(st.ready_port().ok_or_else(|| st.note()))
    }

    /// Deferred listener work (seat stopped, session ended): finish a removal TermService
    /// has not caught up with, retry a pending restart. Returns notes.
    fn retry_listener(&self) -> Vec<String> {
        if !self.second.offered {
            return Vec::new();
        }
        let _g = lock(&self.second.busy);
        let l = self.listener();
        let mut notes = Vec::new();
        if !lock(&self.second.pending).is_empty() || matches!(*lock(&self.second.state), ListenerState::Unavailable { .. } | ListenerState::Disabled | ListenerState::NotNeeded | ListenerState::Unknown) {
            match l.finish_unload() {
                Ok((still, restarted)) => {
                    if restarted {
                        notes.push("termservice_restarted".to_string());
                    }
                    let mut p: Vec<String> = Vec::new();
                    let exposed = lock(&self.second.pending).iter().any(|x| x.starts_with(crate::loopback_listener::notes::EXPOSED));
                    for port in still {
                        p.push(format!("{}:{port}", crate::loopback_listener::notes::UNLOAD_PENDING));
                        if exposed {
                            p.push(format!("{}:{port}", crate::loopback_listener::notes::EXPOSED));
                        }
                    }
                    if p.is_empty() && !lock(&self.second.pending).is_empty() {
                        log::info!("loopback listener: the removed listener is unloaded");
                    }
                    notes.extend(p.iter().cloned());
                    *lock(&self.second.pending) = p;
                }
                Err(e) => log::warn!("loopback listener: unload retry failed: {e}"),
            }
        }
        let st = lock(&self.second.state).clone();
        if let ListenerState::PendingRestart { port } = st {
            let s = lock(&self.second.settings).clone();
            let plan = lock(&self.second.plan).clone();
            if port == s.port {
                let want = ListenerWant { enabled: s.enabled, port, accounts: &plan.accounts, passwordless_seats: plan.passwordless_seats };
                let st = l.retry_pending(&want);
                notes.push(st.note());
                *lock(&self.second.state) = st;
            }
        }
        notes
    }

    pub(crate) fn deps(&self) -> &SessionDeps {
        &self.d
    }

    fn max_from_listener(&self) -> u8 {
        match self.d.rds.listener() {
            Ok(l) => l.max_instance_count.map_or(DEFAULT_MAX, |m| u8::try_from(m).unwrap_or(DEFAULT_MAX).min(DEFAULT_MAX)),
            Err(_) => DEFAULT_MAX,
        }
    }

    /// Shared checks of both RDP backends: TermService, listener, policy, coexistence.
    /// `wrapper_ok` = the experimental wrapper vouches for a non-stock ServiceDll.
    pub(crate) fn rdp_readiness(&self, notes: &mut Vec<String>, wrapper_ok: bool) -> Option<Availability> {
        if (self.d.reference_product_present)() {
            return Some(Availability::Unavailable { reason: reason::REFERENCE_PRODUCT_PRESENT.into() });
        }
        match self.d.rds.service_dll() {
            Ok(sd) if !wrapper_ok && !termservice_is_stock(&sd, self.d.system_root.as_deref()) => {
                return Some(Availability::Unavailable { reason: reason::FOREIGN_SERVICE_DLL.into() });
            }
            Ok(_) => {}
            Err(_) => return Some(Availability::Unavailable { reason: reason::RDS_NOT_READY.into() }),
        }
        if !self.d.rds.termservice_running() {
            return Some(Availability::NeedsSetup { what: "termservice_start".into() });
        }
        if self.d.rds.listener().is_err() {
            return Some(Availability::Unavailable { reason: reason::RDS_NOT_READY.into() });
        }
        let pol = self.d.registry.get(crate::rds::POL, "fDenyTSConnections").ok();
        if pol == Some(crate::journal::RegValue::Dword(1)) {
            return Some(Availability::Unavailable { reason: reason::RDP_DENIED_BY_POLICY.into() });
        }
        if rdp_denied(self.d.registry.as_ref()).unwrap_or(true) {
            notes.push("rdp_disabled_until_prepare".into());
            return Some(Availability::NeedsSetup { what: "enable_remote_desktop".into() });
        }
        None
    }

    pub(crate) fn set_cap(&self, m: u8) {
        *self.max.lock().unwrap_or_else(|p| p.into_inner()) = m;
    }

    pub(crate) fn cap(&self) -> u8 {
        *self.max.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub(crate) fn remember_topology(&self, s: &GlobalSettings) {
        *self.topology.lock().unwrap_or_else(|p| p.into_inner()) = s.backends.host_topology;
    }

    pub(crate) fn topology(&self) -> seat_core::backend::session::HostTopology {
        *self.topology.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub(crate) fn rdp_caps(max: u8) -> SessionCaps {
        SessionCaps {
            concurrent: true,
            max_seats: max,
            per_user: true,
            needs_seat_host: true,
            remote_display: true,
            remote_input: true,
            remote_audio: true,
            auto_logon: true,
            stop_one: true,
        }
    }

    /// Licensing notes (`rds_licensing_not_configured`, `rds_grace_period`).
    pub(crate) fn licensing_notes(&self, notes: &mut Vec<String>) {
        if let Ok(l) = self.d.rds.licensing() {
            if !l.configured() {
                notes.push("rds_licensing_not_configured".into());
            }
            if let Some(days) = l.grace_days_left {
                notes.push(format!("rds_grace_period:{days}"));
            }
        }
        // Time-out / GPU policies seat sessions need off / on (spec 00 §5.2.1), reported only.
        if let Ok(n) = crate::rds::session_policy_notes(self.d.registry.as_ref()) {
            notes.extend(n);
        }
    }
}

impl SessionBackend for RdsLoopbackBackend {
    fn kind(&self) -> SessionBackendKind {
        SessionBackendKind::RdsLoopback
    }

    fn caps(&self) -> SessionCaps {
        Self::rdp_caps(self.cap())
    }

    fn probe(&self) -> BackendResult<HostProbe> {
        let e = self.d.edition.edition()?;
        let rdsh = e.product_type.is_server() && self.d.edition.rdsh_role_installed().unwrap_or(false);
        let class = classify(&e, rdsh);
        let mut notes = vec!["rdp_listener_on_all_interfaces".to_string()];
        let availability = match class {
            HostClass::Home | HostClass::Client => Availability::Unavailable { reason: reason::EDITION_UNSUPPORTED.into() },
            HostClass::ServerWithoutRdsh => {
                notes.push("two_admin_sessions_only".into());
                Availability::Unavailable { reason: reason::RDSH_ROLE_MISSING.into() }
            }
            HostClass::ServerRdsh => self.rdp_readiness(&mut notes, false).unwrap_or(Availability::Available),
        };
        if class == HostClass::ServerRdsh {
            self.licensing_notes(&mut notes);
            self.set_cap(self.max_from_listener());
            if self.second.offered {
                notes.push(self.listener_state().note());
                notes.extend(lock(&self.second.pending).iter().cloned());
            }
        }
        Ok(HostProbe {
            kind: SessionBackendKind::RdsLoopback,
            availability,
            edition: e.edition_id,
            build: e.build,
            arch: arch_name(self.d.edition.native_machine()).into(),
            notes,
        })
    }

    fn prepare_host(&self, settings: &GlobalSettings) -> BackendResult<Vec<String>> {
        self.remember_topology(settings);
        let p = self.probe()?;
        if let Availability::Unavailable { reason } = &p.availability {
            return Err(err(ErrorKind::Unsupported, reason, "rds_loopback is not available on this PC"));
        }
        let rep = RdsApplier::new(self.d.registry.clone(), self.d.journal.clone(), self.d.wall_clock.clone()).apply()?;
        let mut notes = rep.notes;
        *lock(&self.second.settings) = settings.backends.loopback_listener.clone();
        self.second.prepared.store(true, Ordering::SeqCst);
        notes.extend(self.ensure_listener());
        Ok(notes)
    }

    fn restore_host(&self) -> BackendResult<()> {
        // The second listener first (it also removes leftovers without a journal).
        self.second.prepared.store(false, Ordering::SeqCst);
        {
            let _g = lock(&self.second.busy);
            match self.listener().remove() {
                Ok(r) if r.firewall_rule_kept => {
                    log::warn!("loopback listener: firewall rule kept until TermService restarts");
                    let p = r.kept_ports.iter().map(|p| format!("{}:{p}", crate::loopback_listener::notes::UNLOAD_PENDING)).collect();
                    *lock(&self.second.pending) = p;
                }
                Ok(_) => lock(&self.second.pending).clear(),
                Err(e) => log::warn!("loopback listener: not removed: {e}"),
            }
            *lock(&self.second.state) = ListenerState::Unknown;
        }
        let rep = RdsApplier::new(self.d.registry.clone(), self.d.journal.clone(), self.d.wall_clock.clone()).restore_all()?;
        for s in rep.skipped_changed {
            log::warn!("rds: {s} was changed after us; left as it is");
        }
        Ok(())
    }

    fn start(&self, req: &SeatStartRequest<'_>) -> BackendResult<StartStep> {
        self.remember_topology(req.settings);
        if !self.d.rds.wait_ready(Duration::from_secs(5)) {
            return Err(err(ErrorKind::NotReady, reason::RDS_NOT_READY, "TermService is not ready"));
        }
        let passwordless = !req.credentials.as_ref().is_some_and(|c| c.password.is_some());
        let second = if passwordless { self.second_for_start() } else { None };
        self.core.start(&self.d, req, self.cap(), second)
    }

    fn set_listener_plan(&self, plan: &LoopbackListenerPlan) {
        if !self.second.offered {
            return;
        }
        let changed = {
            let mut p = lock(&self.second.plan);
            let changed = *p != *plan;
            *p = plan.clone();
            changed
        };
        if changed && self.second.prepared.load(Ordering::SeqCst) {
            self.ensure_listener();
        }
    }

    fn retry_deferred(&self) -> Vec<String> {
        self.retry_listener()
    }

    fn wait_session(&self, seat: SeatNumber, client_name: &str, timeout: Duration) -> BackendResult<SessionInfo> {
        self.core.wait_session(&self.d, seat, client_name, timeout)
    }

    fn host_hint(&self, seat: SeatNumber, hint: seat_core::backend::session::HostHint) {
        self.core.host_hint(seat, hint);
    }

    fn query(&self, seat: SeatNumber, session_id: u32) -> BackendResult<SessionInfo> {
        self.core.query(&self.d, seat, session_id)
    }

    fn log_off(&self, seat: SeatNumber, session_id: u32) -> BackendResult<()> {
        self.core.log_off(&self.d, seat, session_id, self.topology())
    }

    fn adopt(&self, known: &[KnownSession]) -> BackendResult<Vec<(SeatNumber, SessionInfo)>> {
        self.core.adopt(&self.d, known)
    }

    fn enumerate(&self) -> BackendResult<Vec<SessionInfo>> {
        self.core.enumerate(&self.d)
    }
}

/// Boxed constructor.
pub fn rds_loopback(d: SessionDeps) -> Arc<dyn SessionBackend> {
    Arc::new(RdsLoopbackBackend::new(d))
}
