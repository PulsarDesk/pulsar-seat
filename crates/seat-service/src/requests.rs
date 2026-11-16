// SPDX-License-Identifier: GPL-3.0-only
//! Pipe request dispatch: version check, `hello`, the authorisation table, then one
//! handler per request (architecture §5, spec 10 "What each action changes").

use crate::auth::{Caller, authorize};
use crate::backends;
use crate::events::Scope;
use crate::service::{SERVICE_VERSION, Service};
use crate::wizard::Wizard;
use crate::{diag, log};
use seat_core::backend::DriverId;
use seat_core::config::{AssignmentOptions, Config, DeviceAssignment, SeatKind};
use seat_core::device::{Device, DeviceKind};
use seat_core::state::SeatState;
use seat_core::validate::{IssueCode, Severity, has_conflicts, has_errors, validate};
use seat_core::{DeviceKey, SeatMask, SeatNumber};
use seat_proto::{
    AgentEvent, ApplyResult, CallerInfo, ClientKind, ConfigView, Credentials, DriverReport, ErrorCode, EventFrame,
    Hello, HelloReply, HostAssignment, MIN_PROTOCOL_VERSION, PROTOCOL_VERSION, ProtoError, Request, RequestBody,
    Response, ResponseBody, ServiceStatus, Topic,
};
use std::sync::mpsc::Receiver;
use std::time::Duration;

/// Result of one request: the response plus, for `subscribe`, the event stream the pipe
/// layer must start pushing.
pub struct Handled {
    pub response: Response,
    pub subscription: Option<Receiver<EventFrame>>,
}

type R = Result<ResponseBody, ProtoError>;

/// Longest seat password accepted (characters; the Windows limit is 256).
pub const MAX_PASSWORD_CHARS: usize = 256;

fn err(code: ErrorCode, msg: impl Into<String>) -> ProtoError {
    ProtoError::new(code, msg)
}

fn store_err(e: seat_core::store::StoreError) -> ProtoError {
    log::error_id(crate::eventid::CONFIG_STORE_FAILED, &format!("store: {e}"));
    err(ErrorCode::Store, e.to_string())
}

/// A fresh assignment record for a device (identity fields copied from the live device).
pub fn assignment_for(key: &DeviceKey, kind: DeviceKind, mask: SeatMask, d: Option<&Device>) -> DeviceAssignment {
    DeviceAssignment {
        key: key.clone(),
        kind,
        mask,
        name: d.map(|d| d.name.clone()).unwrap_or_default(),
        label: d.and_then(|d| d.label.clone()),
        hardware_id: d.and_then(|d| d.hardware_id.clone()),
        serial: d.and_then(|d| d.serial.clone()),
        container_id: d.and_then(|d| d.container_id.clone()),
        options: AssignmentOptions::default(),
    }
}

impl Service {
    /// Handle one request from a pipe connection. `caller` persists for the connection.
    pub fn handle(&self, caller: &mut Caller, req: Request) -> Handled {
        let id = req.id;
        let mut subscription = None;
        let result = if !(MIN_PROTOCOL_VERSION..=PROTOCOL_VERSION).contains(&req.version) {
            Err(err(ErrorCode::VersionMismatch, format!("protocol {} not in {MIN_PROTOCOL_VERSION}..={PROTOCOL_VERSION}", req.version)))
        } else {
            self.dispatch(caller, req.body, &mut subscription)
        };
        if let Err(e) = &result
            && !matches!(e.code, ErrorCode::NotFound | ErrorCode::SeatRunning | ErrorCode::SeatNotRunning)
        {
            log::debug(&format!("request {id} from pid {} failed: {e}", caller.pid));
        }
        Handled { response: Response::new(id, result), subscription }
    }

    fn caller_seat(&self, caller: &Caller) -> Option<SeatNumber> {
        let st = self.lock();
        self.seat_of_session(&st, caller.session_id)
    }

    fn dispatch(&self, caller: &mut Caller, body: RequestBody, sub: &mut Option<Receiver<EventFrame>>) -> R {
        if let RequestBody::Hello(h) = &body {
            return self.hello(caller, h);
        }
        let (settings, booted) = {
            let st = self.lock();
            (st.config.settings.clone(), st.boot_decision.is_some())
        };
        let access = body.access();
        // Until boot phase 1 decided the run state and safe mode, only status is served.
        if !booted && !matches!(access, seat_proto::Access::Any | seat_proto::Access::Status) {
            return Err(err(ErrorCode::NotReady, "service is starting"));
        }
        let seat = self.caller_seat(caller);
        authorize(access, caller, &settings, seat)?;
        if body.min_protocol() > caller.protocol {
            return Err(err(
                ErrorCode::VersionMismatch,
                format!("request needs protocol {} (client said {})", body.min_protocol(), caller.protocol),
            )
            .with_reason("protocol_too_old"));
        }
        self.check_seat_kind(&body)?;
        if body.is_remote_kind_request() {
            return self.remote_kind_request(&body);
        }
        use RequestBody as B;
        match body {
            B::Hello(_) => unreachable!("handled above"),
            B::Subscribe { topics } => {
                let scope = self.scope_for(caller, &topics)?;
                *sub = Some(self.events.subscribe(topics, scope));
                Ok(ResponseBody::Ack)
            }
            B::PollEvents { after, wait_ms, topics } => {
                let scope = self.scope_for(caller, &topics)?;
                let wait = Duration::from_millis(u64::from(wait_ms.min(seat_proto::MAX_POLL_WAIT_MS)));
                Ok(ResponseBody::Events(self.events.poll(after, &topics, scope, wait)))
            }
            B::GetStatus => {
                let mut status = self.status();
                // Remote seat status is admin only (merge-plan.md section 5).
                if !caller.admin() {
                    status.seats.retain(|r| r.kind == SeatKind::Local);
                }
                Ok(ResponseBody::Status(status))
            }
            B::GetBackends => {
                let settings = self.lock().config.settings.clone();
                let mut r = backends::report(self.factory.as_ref(), &settings, &self.backends());
                r.drivers = [DriverId::KbdMouFilter, DriverId::HidFilter, DriverId::Idd, DriverId::StorageMinifilter, DriverId::SourceIpCallout]
                    .into_iter()
                    .map(|d| DriverReport { driver: d, status: self.platform.driver_status(d) })
                    .collect();
                r.source_ip = self.lock().source_ip.clone();
                Ok(ResponseBody::Backends(r))
            }
            B::Preflight => Ok(ResponseBody::Checks(self.platform.preflight())),
            B::GetConfig => Ok(ResponseBody::Config(self.config_view())),
            B::ValidateConfig { config } => {
                let mut c = *config;
                c.normalize();
                let st = self.lock();
                let inv = st.inventory.devices();
                Ok(ResponseBody::Issues(self.validate_full(&c, &inv)))
            }
            B::ApplyConfig { config, base_revision, force } => self.apply_config(*config, base_revision, force),
            B::ResetConfig { confirm } => self.reset_config(confirm),
            B::ImportReference { json } => self.import_reference(&json),
            B::SetSeatPassword { seat, password } => {
                if self.lock().config.seat(seat).is_none() {
                    return Err(err(ErrorCode::NotFound, "no such seat"));
                }
                let p = password.filter(|p| !p.is_empty());
                if p.as_ref().is_some_and(|p| p.expose().chars().count() > MAX_PASSWORD_CHARS) {
                    return Err(err(ErrorCode::InvalidValue, format!("password longer than {MAX_PASSWORD_CHARS} characters"))
                        .with_field("password"));
                }
                self.secrets.set(seat, p.as_ref()).map_err(|e| {
                    log::error_id(crate::eventid::SECRET_STORE_FAILED, &format!("seat {seat}: {e}"));
                    err(ErrorCode::Store, "secret store failed")
                })?;
                self.password_known.lock().unwrap_or_else(|e| e.into_inner()).insert(seat, p.is_some());
                self.password_changed(seat);
                // A seat with / without a password moves between the two listeners.
                self.spawn(|s| s.sync_listener_plan());
                log::info(&format!("seat {seat}: password {}", if p.is_some() { "set" } else { "cleared" }));
                Ok(ResponseBody::Ack)
            }
            B::ListLocalUsers => Ok(ResponseBody::Users(self.platform.local_users()?)),
            B::ListCpuSets => Ok(ResponseBody::CpuSets(self.platform.cpu_sets())),
            B::ListDevices => {
                let st = self.lock();
                Ok(ResponseBody::Devices(st.inventory.views(&st.config)))
            }
            B::CompareSaved => {
                let st = self.lock();
                Ok(ResponseBody::Diff(st.inventory.diff(&st.config)))
            }
            B::IdentifyDevice { key, on } => self.identify(&key, on),
            B::MoveAssignment { from, to } => self.move_assignment(&from, &to),
            B::SetEnabled { enabled } => self.set_enabled(enabled),
            B::ResolveInputMove { accept } => {
                let n = self.resolve_input_move(accept).map_err(store_err)?;
                log::info(&format!("hotkey moves {} ({n} unit(s))", if accept { "saved" } else { "declined" }));
                Ok(ResponseBody::Ack)
            }
            B::StartSeats { seats } => {
                {
                    let st = self.lock();
                    self.may_start(&st, true)?;
                    if let Some(list) = &seats
                        && let Some(bad) = list.iter().find(|s| st.config.seat(**s).is_none())
                    {
                        return Err(err(ErrorCode::NotFound, format!("seat {bad} is not configured")));
                    }
                }
                let list = match seats {
                    Some(l) => {
                        // Remote seats go to the remote seat worker (`session_unavailable`
                        // without concurrent sessions); the local ones follow below.
                        self.start_remote_seats(&l)?;
                        let st = self.lock();
                        l.into_iter().filter(|n| st.config.seat(*n).is_some_and(|s| s.is_local())).collect()
                    }
                    None => {
                        self.start_remote_autostart();
                        let st = self.lock();
                        st.config.seats.iter().filter(|s| s.autostart && s.is_local()).map(|s| s.number).collect()
                    }
                };
                // start_sequence skips seats the service does not start (the console seat 1
                // of seat_one_console).
                self.start_sequence(list);
                Ok(ResponseBody::Ack)
            }
            B::StopSeat { seat } => {
                if self.lock().config.seat(seat).is_some_and(|s| s.is_remote()) {
                    return self.stop_remote_seat(seat);
                }
                if !Service::managed(&self.lock(), seat) {
                    return Err(err(ErrorCode::Unsupported, "seat 1 is the console").with_reason("console_seat"));
                }
                let active = self.lock().seats.get(&seat).map(|s| s.is_active() || s.want || s.retry_at.is_some());
                match active {
                    None => Err(err(ErrorCode::NotFound, "no such seat")),
                    Some(false) => Err(err(ErrorCode::SeatNotRunning, "seat is not running")),
                    Some(true) => {
                        self.spawn(move |s| s.stop_seat_now(seat, true));
                        Ok(ResponseBody::Ack)
                    }
                }
            }
            B::SwitchSeat { seat } => self.switch_seat(seat),
            B::RelogonSeat { seat } => {
                {
                    let st = self.lock();
                    self.may_start(&st, true)?;
                    if st.config.seat(seat).is_none() {
                        return Err(err(ErrorCode::NotFound, "no such seat"));
                    }
                }
                self.relogon(seat)?;
                Ok(ResponseBody::Ack)
            }
            B::RequestReboot => {
                log::info(&format!("reboot requested by pid {}", caller.pid));
                self.platform.reboot()?;
                Ok(ResponseBody::Ack)
            }
            B::WizardStart { variant } => self.wizard_start(variant),
            B::WizardCommand { command } => {
                let now = self.now();
                let mut st = self.lock();
                let crate::service::State { wizard, inventory, .. } = &mut *st;
                let w = wizard.as_mut().ok_or_else(|| err(ErrorCode::WizardState, "wizard is not running"))?;
                let extend = matches!(command, seat_proto::WizardCommand::ExtendDesktop);
                let picked_before = w.picked;
                let action = w.command(&command, inventory, now).map_err(|r| err(ErrorCode::InvalidValue, r).with_reason(r))?;
                let monitors: Vec<DeviceKey> = if extend { w.monitors.clone() } else { Vec::new() };
                let relabel = (!picked_before && w.picked).then(|| w.overlay_labels());
                drop(st);
                if extend {
                    let plan = seat_core::backend::display::TopologyPlan { console: monitors, seats: Vec::new() };
                    self.backends().display.apply_topology(&plan)?;
                }
                if let Some(labels) = relabel {
                    self.wizard_overlays(&labels, true);
                }
                self.wizard_action(action)?;
                Ok(ResponseBody::Ack)
            }
            B::WizardStop { save } => self.wizard_stop(save),
            B::GetUpdateStatus => Ok(ResponseBody::UpdateStatus(self.update_status())),
            B::CheckForUpdate => {
                log::info(&format!("update check requested by pid {}", caller.pid));
                self.update_check().map(ResponseBody::UpdateStatus)
            }
            B::DownloadUpdate => {
                log::info(&format!("update download requested by pid {}", caller.pid));
                self.update_download().map(ResponseBody::UpdateStatus)
            }
            B::InstallUpdate { version, confirm } => {
                log::info(&format!("update {version} install requested by pid {} (confirm {confirm})", caller.pid));
                self.update_install(&version, confirm).map(ResponseBody::UpdateStatus)
            }
            B::DiagnosticReport { mask_users } => {
                let path = diag::write_report(self, mask_users).map_err(|e| err(ErrorCode::Os, e.to_string()))?;
                Ok(ResponseBody::Report { path: path.display().to_string() })
            }
            B::StartSeatApp { path, args } => {
                let pid = self.start_seat_app(caller, &path, &args)?;
                Ok(ResponseBody::AppStarted { pid })
            }
            B::MySeat => {
                let st = self.lock();
                let seat = self.seat_of_session(&st, caller.session_id);
                Ok(ResponseBody::Seat { seat, session_id: Some(caller.session_id) })
            }
            B::SeatOfSession { session_id } => {
                let st = self.lock();
                Ok(ResponseBody::Seat { seat: self.seat_of_session(&st, session_id), session_id: Some(session_id) })
            }
            B::SeatOfProcess { pid } => {
                let session = self.platform.process_session(pid);
                let st = self.lock();
                Ok(ResponseBody::Seat { seat: session.and_then(|s| self.seat_of_session(&st, s)), session_id: session })
            }
            B::HostGetAssignment { seat } => self.host_assignment(seat),
            B::HostTakeCredentials { seat } => self.host_credentials(seat),
            B::HostReport { seat, state, rdp_client } => {
                self.on_host_report(seat, &state, rdp_client.as_deref());

                Ok(ResponseBody::Ack)
            }
            B::HostRedirectRefusal { seat, channel, reason, detail, suppressed } => {
                self.on_redirect_refusal(seat, channel, reason, detail, suppressed);
                Ok(ResponseBody::Ack)
            }
            B::ConsoleInputReport { events } => self.console_input_report(caller.pid, events),
            B::CredprovQuery { scenario } => self.credprov_query(caller, scenario),
            B::CredprovTakeCredentials => self.credprov_take(caller),
            B::CredprovReport { status, sub_status } => self.credprov_report(caller, status, sub_status),
            B::CredprovSwitch { seat } => self.credprov_switch(caller, seat),
            B::AgentSwitchTargets => self.agent_switch_targets(caller),
            B::AgentSwitch { seat } => self.agent_switch(caller, seat),
            B::TestSeatCredentials { .. }
            | B::EnableUnlockAccount { .. }
            | B::GetRemoteRuntime { .. }
            | B::SubmitPin { .. }
            | B::GetPairState { .. }
            | B::CancelPair { .. }
            | B::ListPairedClients { .. }
            | B::UnpairClient { .. }
            | B::UnpairAll { .. }
            | B::RegenerateSeatIdentity { .. }
            | B::GetApps { .. }
            | B::SetApps { .. }
            | B::AnswerPeerRequest { .. }
            | B::SetSeatConnectPassword { .. }
            | B::GetSeatOtp { .. }
            | B::RotateSeatOtp { .. }
            | B::AnswerRelayAuth { .. }
            | B::GetHostInfo
            | B::SetMultiSession { .. }
            | B::GetTermwrapStatus
            | B::SetTermwrap { .. }
            | B::ListRenderAdapters
            | B::GetPadBackends
            | B::GetShimStatus
            | B::SetShimTargets { .. }
            | B::SetAppMasking { .. }
            | B::SetDxvk { .. }
            | B::GetDxvkStatus
            | B::GetLogTail { .. }
            | B::GetStreamBackends
            | B::GetUserPicture { .. }
            | B::CleanupAudioEndpoints { .. } => unreachable!("answered by remote_kind_request above"),
            B::AgentReport { report } => {
                if caller.agent_session != Some(report.session_id) {
                    return Err(err(ErrorCode::AccessDenied, "agent may report only its own session"));
                }
                let mut st = self.lock();
                match &report.event {
                    AgentEvent::Started => {}
                    AgentEvent::TaskDone { task_id } => {
                        if let Some(q) = st.agent_tasks.get_mut(&report.session_id) {
                            q.retain(|t| t.id != *task_id);
                        }
                    }
                    AgentEvent::TaskFailed { task_id, reason, win32 } => {
                        log::warn(&format!("agent task {task_id} in session {} failed: {reason} {win32:?}", report.session_id));
                        if let Some(q) = st.agent_tasks.get_mut(&report.session_id) {
                            q.retain(|t| t.id != *task_id);
                        }
                    }
                    AgentEvent::Stopping => {
                        st.agent_tasks.remove(&report.session_id);
                    }
                }
                let tasks = st.agent_tasks.get(&report.session_id).cloned().unwrap_or_default();
                Ok(ResponseBody::AgentTasks(tasks))
            }
        }
    }

    fn hello(&self, caller: &mut Caller, h: &Hello) -> R {
        if !(MIN_PROTOCOL_VERSION..=PROTOCOL_VERSION).contains(&h.protocol_version) {
            return Err(err(ErrorCode::VersionMismatch, format!("client protocol {}", h.protocol_version)));
        }
        if h.client == ClientKind::SeatHost {
            let (Some(seat), Some(token)) = (h.seat, h.token.as_ref()) else {
                return Err(err(ErrorCode::AccessDenied, "seat host needs seat and token"));
            };
            let ok = self
                .lock()
                .seats
                .get(&seat)
                .and_then(|s| s.host.as_ref())
                .is_some_and(|host| host.pid == caller.pid && host.token == *token);
            if !ok {
                log::warn(&format!("seat host hello refused for seat {seat} (pid {})", caller.pid));
                return Err(err(ErrorCode::AccessDenied, "unknown seat host"));
            }
            caller.host_seat = Some(seat);
            if let Some(hp) = self.lock().seats.get_mut(&seat).and_then(|s| s.host.as_mut()) {
                hp.connected = true;
            }
        }
        if h.client == ClientKind::ConsoleListener {
            if !self.verify_listener(caller.pid, caller.session_id, h.token.as_ref()) {
                log::warn(&format!("console listener hello refused (pid {}, session {})", caller.pid, caller.session_id));
                return Err(err(ErrorCode::AccessDenied, "unknown console listener"));
            }
            caller.console_listener = true;
        }
        if h.client == ClientKind::CredentialProvider {
            if !self.verify_credprov(caller) {
                log::warn(&format!("credential provider hello refused (pid {}, session {})", caller.pid, caller.session_id));
                return Err(err(ErrorCode::AccessDenied, "unknown credential provider"));
            }
            caller.credprov_session = Some(caller.session_id);
        }
        if h.client == ClientKind::Agent {
            // Only the agent the service launched into this session (same pattern as the
            // seat host: the PID the service started, in the session it was started in).
            let verified = self
                .lock()
                .seats
                .values()
                .any(|s| s.agent_pid == Some(caller.pid) && s.rt.session_id == Some(caller.session_id));
            if !verified {
                log::warn(&format!("agent hello refused (pid {}, session {})", caller.pid, caller.session_id));
                return Err(err(ErrorCode::AccessDenied, "unknown agent"));
            }
            caller.agent_session = Some(caller.session_id);
        }
        if h.client == ClientKind::KioskShell {
            // Only a shell the desktops backend started: its PID, in the session it was
            // started in, for the seat it names.
            let shells = self.backends().session.kiosk_shells();
            let verified = h.seat.is_some_and(|seat| shells.iter().any(|k| k.seat == seat && k.pid == caller.pid && k.session_id == caller.session_id));
            if !verified {
                log::warn(&format!("kiosk shell hello refused (pid {}, session {})", caller.pid, caller.session_id));
                return Err(err(ErrorCode::AccessDenied, "unknown kiosk shell"));
            }
            caller.kiosk_seat = h.seat;
        }
        caller.client = Some(h.client);
        caller.protocol = h.protocol_version;
        let st = self.lock();
        let seat = self.seat_of_session(&st, caller.session_id);
        let admin = caller.admin();
        Ok(ResponseBody::Hello(HelloReply {
            service_version: SERVICE_VERSION.into(),
            protocol_version: PROTOCOL_VERSION,
            min_protocol_version: MIN_PROTOCOL_VERSION,
            caller: CallerInfo {
                client: h.client,
                session_id: caller.session_id,
                seat,
                is_admin: admin,
                restricted: st.config.settings.admin_only && !admin,
            },
        }))
    }

    /// Host / agent topics are only for verified hosts and agents.
    fn scope_for(&self, caller: &Caller, topics: &[Topic]) -> Result<Scope, ProtoError> {
        let host = topics.is_empty() || topics.contains(&Topic::Host);
        let agent = topics.is_empty() || topics.contains(&Topic::Agent);
        if topics.contains(&Topic::Host) && caller.host_seat.is_none() && caller.kiosk_seat.is_none() {
            return Err(err(ErrorCode::AccessDenied, "host topic is for seat hosts"));
        }
        if topics.contains(&Topic::Agent) && caller.agent_session.is_none() {
            return Err(err(ErrorCode::AccessDenied, "agent topic is for agents"));
        }
        // Remote seat status (pairing, streams) is admin only.
        if topics.contains(&Topic::Stream) && !caller.admin() {
            return Err(err(ErrorCode::AccessDenied, "stream topic is for administrators"));
        }
        Ok(Scope {
            host_seat: if host { caller.host_seat } else { None },
            agent_session: if agent { caller.agent_session } else { None },
            kiosk_seat: if host { caller.kiosk_seat } else { None },
            // Non-admins never get remote seat status, even with empty topics (= all).
            admin: caller.admin(),
        })
    }

    /// `wrong_seat_kind` when the request names a configured seat of the other kind
    /// (architecture.md section 10.2). Unknown seats are left to the handler.
    fn check_seat_kind(&self, body: &RequestBody) -> Result<(), ProtoError> {
        let Some((seat, need)) = body.seat_kind() else { return Ok(()) };
        let have = self.lock().config.seat(seat).map(|s| s.kind);
        match have {
            Some(k) if k != need => Err(err(
                ErrorCode::WrongSeatKind,
                format!("seat {seat} is a {} seat; this request needs a {} seat", k.as_str(), need.as_str()),
            )
            .with_reason(need.as_str())),
            _ => Ok(()),
        }
    }

    pub fn status(&self) -> ServiceStatus {
        let b = self.backends();
        let st = self.lock();
        let mut notes = b.fallback_notes.clone();
        notes.extend(st.backend_errors.iter().map(|e| format!("enumerate:{e}")));
        ServiceStatus {
            service_version: SERVICE_VERSION.into(),
            product: st.product.clone(),
            seats: Service::runtime_list(&st),
            active: b.active(),
            fallback_notes: notes,
            config_revision: st.config.revision,
            reboot_pending: st.reboot_pending,
            os_build_changed: st.os_build_changed,
            wizard_running: st.wizard.is_some(),
            last_event_seq: self.events.last_seq(),
        }
    }

    /// Whether a password is stored, from the cache (the secret store is asked at most
    /// once per seat and never while the state lock is held).
    pub(crate) fn password_is_set(&self, seat: SeatNumber) -> bool {
        if let Some(v) = self.password_known.lock().unwrap_or_else(|e| e.into_inner()).get(&seat) {
            return *v;
        }
        let v = self.secrets.has(seat);
        self.password_known.lock().unwrap_or_else(|e| e.into_inner()).insert(seat, v);
        v
    }

    pub(crate) fn config_view(&self) -> ConfigView {
        let (config, failing) = {
            let st = self.lock();
            (st.config.clone(), st.run.failing_passwords.clone())
        };
        let password_set = config.seats.iter().map(|s| s.number).filter(|s| self.password_is_set(*s)).collect();
        ConfigView { config, password_set, password_failing: failing }
    }

    /// seat-core validation plus the service-side checks that need this PC: cores per seat
    /// (seat-compat `cpu::validate_sets`, spec 80 §11.2).
    pub(crate) fn validate_full(&self, cfg: &Config, inv: &[seat_core::device::Device]) -> Vec<seat_core::validate::Issue> {
        let mut issues = validate(cfg, Some(inv));
        issues.extend(cpu_issues(cfg, &self.platform.cpu_sets()));
        issues
    }

    fn apply_config(&self, mut cfg: Config, base: u64, force: bool) -> R {
        cfg.normalize();
        let (issues, old) = {
            let st = self.lock();
            if base != st.config.revision {
                return Err(err(ErrorCode::RevisionConflict, format!("stored revision {}, edit based on {base}", st.config.revision)));
            }
            let inv = st.inventory.devices();
            (self.validate_full(&cfg, &inv), st.config.clone())
        };
        if let Some(i) = issues.iter().find(|i| i.severity == Severity::Error && i.code == IssueCode::ExperimentalNotAcknowledged) {
            return Err(err(ErrorCode::WarningNotAcknowledged, "experimental backend warning not acknowledged")
                .with_field(i.field.clone().unwrap_or_default()));
        }
        if has_errors(&issues) {
            let codes: Vec<String> =
                issues.iter().filter(|i| i.severity == Severity::Error).map(|i| backends::kind_name(&i.code)).collect();
            let mut e = err(ErrorCode::InvalidConfig, codes.join(","));
            if let Some(f) = issues.iter().find(|i| i.severity == Severity::Error).and_then(|i| i.field.clone()) {
                e = e.with_field(f);
            }
            return Err(e);
        }
        if has_conflicts(&issues) && !force {
            return Err(err(ErrorCode::ConflictsNotConfirmed, "save-time conflicts need confirmation"));
        }
        // The product switch has its own request (it stops / starts things).
        cfg.settings.enabled = old.settings.enabled;
        // Deleted remote seats: refused while active, cleaned up after the save.
        let removed_remote = Service::removed_remote_seats(&old, &cfg);
        self.check_remote_removal(&removed_remote)?;
        let (rev, restart, removed) = {
            let mut st = self.lock();
            if st.config.revision != base {
                return Err(err(ErrorCode::RevisionConflict, "config changed meanwhile"));
            }
            // Running seats whose monitors change must restart (spec 10 §8.2).
            let before: Vec<(SeatNumber, Vec<DeviceKey>)> = st
                .seats
                .values()
                .filter(|s| Service::managed(&st, s.seat) && s.is_active())
                .map(|s| (s.seat, Service::seat_monitors(&st, s.seat)))
                .collect();
            let rev = self.commit_config(&mut st, cfg.clone()).map_err(store_err)?;
            let restart: Vec<SeatNumber> = before
                .iter()
                .filter(|(s, mons)| st.config.seat(*s).is_some() && Service::seat_monitors(&st, *s) != *mons)
                .map(|(s, _)| *s)
                .collect();
            let removed: Vec<SeatNumber> =
                st.seats.keys().copied().filter(|s| st.config.seat(*s).is_none() && Service::managed(&st, *s)).collect();
            st.ownership_dirty = true;
            (rev, restart, removed)
        };
        log::info(&format!("config revision {rev} applied"));
        self.forget_remote_seats(&removed_remote);
        // A session-backend change prepares the new backend itself (restore + prepare).
        let prepared = old.settings.backends != cfg.settings.backends && self.reselect_backends();
        if old.settings.tweaks != cfg.settings.tweaks {
            self.spawn(|s| s.sync_tweaks());
        }
        if old.seats.len() != cfg.seats.len() {
            self.spawn(|s| s.ensure_images());
        }
        for seat in &removed {
            let seat = *seat;
            self.spawn(move |s| {
                s.stop_seat_now(seat, true);
                s.lock().seats.remove(&seat);
            });
        }
        let listener_changed = old.settings.backends.loopback_listener != cfg.settings.backends.loopback_listener;
        self.spawn(move |s| {
            s.refresh_inventory();
            s.apply_ownership();
            s.reconfigure_hosts();
            // Seat accounts / extra accounts / port of the passwordless-seat listener.
            if s.lock().config.settings.enabled {
                if listener_changed && !prepared {
                    let settings = s.lock().config.settings.clone();
                    s.sync_listener_plan();
                    if let Err(e) = s.backends().session.prepare_host(&settings) {
                        log::error_id(crate::eventid::BACKEND_FAILED, &format!("prepare_host: {e}"));
                    }
                } else if !prepared {
                    s.sync_listener_plan();
                }
            }
        });
        let remaining: Vec<_> = issues.into_iter().filter(|i| i.severity != Severity::Error).collect();
        Ok(ResponseBody::Applied(ApplyResult { revision: rev, issues: remaining, restart_seats: restart, reboot_needed: false }))
    }

    fn reset_config(&self, confirm: bool) -> R {
        if !confirm {
            return Err(err(ErrorCode::InvalidValue, "reset needs confirm").with_field("confirm"));
        }
        let seats: Vec<SeatNumber> = {
            let st = self.lock();
            st.seats.keys().copied().filter(|s| Service::managed(&st, *s)).collect()
        };
        for s in seats {
            self.stop_seat_now(s, true);
        }
        self.release_ownership();
        let numbers: Vec<SeatNumber> = self.lock().config.seats.iter().map(|s| s.number).collect();
        for s in numbers {
            if let Err(e) = self.secrets.set(s, None) {
                log::warn(&format!("seat {s}: secret not cleared: {e}"));
            }
            self.password_known.lock().unwrap_or_else(|e| e.into_inner()).insert(s, false);
        }
        let rev = {
            let mut st = self.lock();
            let rev = self.commit_config(&mut st, Config::default()).map_err(store_err)?;
            st.seats.retain(|s, _| s.is_console());
            st.run.failing_passwords.clear();
            st.run.last_user_sid.clear();
            self.save_run(&st);
            self.recompute_product(&mut st);
            rev
        };
        log::info(&format!("config reset to defaults (revision {rev})"));
        self.spawn(|s| s.sync_listener_plan());
        Ok(ResponseBody::Ack)
    }

    /// Optional importer of the reference product's export (spec 10 §3.8), mapped by
    /// `seat-compat` onto the stored config and the live devices. Returns the proposal; the
    /// manager saves it with `apply_config` (nothing is stored here).
    fn import_reference(&self, json: &str) -> R {
        if serde_json::from_str::<serde_json::Value>(json).is_err() {
            return Err(err(ErrorCode::InvalidValue, "not JSON").with_field("json"));
        }
        let (base, devices) = {
            let st = self.lock();
            (st.config.clone(), st.inventory.devices())
        };
        crate::wiring::import_reference(json, &base, &devices).map(|c| ResponseBody::Config(ConfigView {
            config: c,
            password_set: Vec::new(),
            password_failing: Vec::new(),
        }))
    }

    fn identify(&self, key: &DeviceKey, on: bool) -> R {
        let (kind, label) = {
            let st = self.lock();
            let e = st.inventory.get(key).filter(|e| e.device.present).ok_or_else(|| err(ErrorCode::NotFound, "device not present"))?;
            let eff = st.inventory.effective(&st.config).into_iter().find(|x| &x.key == key);
            let label = eff.and_then(|x| x.mask.single()).map(|s| s.to_string()).unwrap_or_else(|| "–".into());
            (e.device.kind, label)
        };
        let b = self.backends();
        match kind {
            DeviceKind::Monitor => b.display.identify(&[(key.clone(), label)], on)?,
            DeviceKind::AudioRender if on => b.audio.test_tone(key, 1_500)?,
            DeviceKind::AudioRender => {}
            // Input devices flash on activity; nothing to switch on.
            DeviceKind::Keyboard | DeviceKind::Mouse | DeviceKind::Hid => {}
            _ => return Err(err(ErrorCode::Unsupported, "no indication for this device kind")),
        }
        Ok(ResponseBody::Ack)
    }

    fn move_assignment(&self, from: &DeviceKey, to: &DeviceKey) -> R {
        let mut st = self.lock();
        let mut cfg = st.config.clone();
        let i = cfg.devices.iter().position(|a| &a.key == from).ok_or_else(|| err(ErrorCode::NotFound, "no assignment for the old key"))?;
        let live = st.inventory.get(to).filter(|e| e.device.present).map(|e| e.device.clone());
        let live = live.ok_or_else(|| err(ErrorCode::NotFound, "new device not present"))?;
        if live.kind != cfg.devices[i].kind {
            return Err(err(ErrorCode::InvalidValue, "device kinds differ").with_field("to"));
        }
        let mut a = cfg.devices.remove(i);
        cfg.devices.retain(|x| &x.key != to);
        let fresh = assignment_for(to, live.kind, a.mask, Some(&live));
        a = DeviceAssignment { options: a.options, ..fresh };
        cfg.devices.push(a);
        self.commit_config(&mut st, cfg).map_err(store_err)?;
        st.devices_dirty = true;
        st.ownership_dirty = true;
        Ok(ResponseBody::Ack)
    }

    fn set_enabled(&self, enabled: bool) -> R {
        {
            let mut st = self.lock();
            if st.config.settings.enabled == enabled && !(enabled && st.safe_reason.is_some()) {
                return Ok(ResponseBody::Ack);
            }
            let mut cfg = st.config.clone();
            cfg.settings.enabled = enabled;
            self.commit_config(&mut st, cfg).map_err(store_err)?;
            // Enabling again is the user's acknowledgement of an unclean-shutdown / crash
            // loop safe mode (Windows Safe Mode stays).
            if enabled && st.safe_reason != Some(seat_core::state::SafeModeReason::WindowsSafeMode) {
                st.safe_reason = None;
            }
            self.recompute_product(&mut st);
        }
        self.spawn(|s| s.sync_tweaks());
        if enabled {
            log::info("product enabled");
            self.spawn(|s| {
                let settings = s.lock().config.settings.clone();
                // A fallback chosen earlier (at boot before the console user signed in) may
                // be gone now; re-selecting prepares the new session backend itself.
                if !s.reselect_after_fallback() {
                    s.sync_listener_plan();
                    if let Err(e) = s.backends().session.prepare_host(&settings) {
                        log::error_id(crate::eventid::BACKEND_FAILED, &format!("prepare_host: {e}"));
                    }
                }
                s.refresh_inventory();
                s.refresh_console_seat();
                s.apply_ownership();
                s.sync_seat1_kiosk();
                match settings.start_mode {
                    seat_core::config::StartMode::Manual => {}
                    seat_core::config::StartMode::AtBoot => s.start_autostart_seats(),
                    // Same "wait or start" rule as at boot: only with a console user.
                    seat_core::config::StartMode::AfterSeatOneLogon => s.start_after_seat_one_logon(),
                }
            });
        } else {
            log::info("product disabled");
            self.spawn(|s| {
                s.lock().waiting_seat_one_logon = false;
                let seats: Vec<SeatNumber> = {
                    let st = s.lock();
                    st.seats.keys().copied().filter(|x| Service::managed(&st, *x)).collect()
                };
                for seat in seats {
                    s.stop_seat_now(seat, true);
                }
                s.sync_seat1_kiosk();
                s.sync_kiosk_console();
                s.release_ownership();
                let mut st = s.lock();
                s.set_seat_state(&mut st, SeatNumber::CONSOLE, SeatState::Off);
            });
        }
        Ok(ResponseBody::Ack)
    }

    fn wizard_start(&self, variant: seat_proto::WizardVariant) -> R {
        // Held across capture on, so a stop cannot run between "stored" and "capture on".
        let _op = self.wizard_op.lock().unwrap_or_else(|e| e.into_inner());
        let w = {
            let st = self.lock();
            if st.wizard.is_some() {
                return Err(err(ErrorCode::WizardState, "wizard already running"));
            }
            Wizard::check_start(&st.inventory).map_err(|e| err(ErrorCode::WizardState, e.reason()).with_reason(e.reason()))?;
            Wizard::start(variant, &st.inventory)
        };
        let p = w.progress();
        let labels = w.overlay_labels();
        let capture = variant != seat_proto::WizardVariant::QuickTwoSeats;
        // Capture first: the wizard is published only once its input is on.
        if capture {
            self.backends().input.set_wizard_capture(true)?;
        }
        self.lock().wizard = Some(w);
        if capture {
            // A big number on every monitor (spec 10 §9.2 step 2); the pages show it too.
            self.wizard_overlays(&labels, true);
        }
        self.events.publish(seat_proto::Event::WizardProgress(p));
        Ok(ResponseBody::Ack)
    }

    /// Capture off and overlays down for a wizard that has left the state.
    fn wizard_teardown(&self, w: &Wizard) {
        if w.variant != seat_proto::WizardVariant::QuickTwoSeats {
            if let Err(e) = self.backends().input.set_wizard_capture(false) {
                log::warn(&format!("wizard capture off: {e}"));
            }
            self.wizard_overlays(&w.overlay_labels(), false);
        }
    }

    /// Number overlays on the wizard's monitors (best effort: the manager's pages carry
    /// the same numbers).
    pub(crate) fn wizard_overlays(&self, labels: &[(DeviceKey, String)], on: bool) {
        if let Err(e) = self.backends().display.identify(labels, on) {
            log::debug(&format!("wizard overlays: {e}"));
        }
    }

    /// Carry out what a wizard input or command asked for.
    pub(crate) fn wizard_action(&self, action: crate::wizard::WizardAction) -> Result<(), ProtoError> {
        use crate::wizard::WizardAction as A;
        match action {
            A::Progress(p) => {
                self.events.publish(seat_proto::Event::WizardProgress(p));
                Ok(())
            }
            A::Save => self.wizard_stop(true).map(|_| ()),
            A::Quit => self.wizard_stop(false).map(|_| ()),
        }
    }

    /// End the wizard. With `save` the result is validated and committed **before** the
    /// wizard leaves the state: on an error the wizard keeps running (capture and overlays
    /// stay on) and a progress with step `invalid` and the issue codes is published, so the
    /// pages can show what is wrong instead of hanging on a stale step.
    pub(crate) fn wizard_stop(&self, save: bool) -> R {
        let _op = self.wizard_op.lock().unwrap_or_else(|e| e.into_inner());
        let not_running = || err(ErrorCode::WizardState, "wizard is not running");
        if !save {
            let w = self.lock().wizard.take().ok_or_else(not_running)?;
            self.wizard_teardown(&w);
            let mut p = w.progress();
            p.step = "cancelled".into();
            self.events.publish(seat_proto::Event::WizardProgress(p));
            return Ok(ResponseBody::Ack);
        }
        let saved = {
            let mut st = self.lock();
            let w = st.wizard.as_ref().ok_or_else(not_running)?;
            let cfg = w.result(&st.config, &st.inventory);
            let inv = st.inventory.devices();
            let issues = self.validate_full(&cfg, &inv);
            let outcome = if has_errors(&issues) {
                let codes: Vec<String> =
                    issues.iter().filter(|i| i.severity == Severity::Error).map(|i| backends::kind_name(&i.code)).collect();
                Err((codes.clone(), err(ErrorCode::InvalidConfig, codes.join(","))))
            } else {
                self.commit_config(&mut st, cfg).map_err(|e| (vec!["store_failed".to_string()], store_err(e)))
            };
            match outcome {
                Ok(rev) => {
                    st.ownership_dirty = true;
                    Ok((st.wizard.take().ok_or_else(not_running)?, rev))
                }
                Err((codes, e)) => {
                    let w = st.wizard.as_mut().ok_or_else(not_running)?;
                    // Keys keep working: the user fixes the assignment and saves again.
                    w.done = false;
                    let mut p = w.progress();
                    p.step = "invalid".into();
                    p.issues = codes;
                    Err((p, e))
                }
            }
        };
        match saved {
            Ok((w, rev)) => {
                self.wizard_teardown(&w);
                let mut p = w.progress();
                p.step = "done".into();
                self.events.publish(seat_proto::Event::WizardProgress(p));
                log::info(&format!("wizard saved revision {rev}"));
                Ok(ResponseBody::Ack)
            }
            Err((p, e)) => {
                log::warn(&format!("wizard result not saved: {}", e.message));
                self.events.publish(seat_proto::Event::WizardProgress(p));
                Err(e)
            }
        }
    }

    fn host_assignment(&self, seat: SeatNumber) -> R {
        let b = self.backends();
        let mut st = self.lock();
        let slot = st.seats.get(&seat).ok_or_else(|| err(ErrorCode::NotFound, "no such seat"))?;
        let launch = slot.launch.clone().ok_or_else(|| err(ErrorCode::SeatNotRunning, "seat host not launched"))?;
        let outputs = slot.outputs.clone().unwrap_or(seat_core::backend::display::SeatOutputs { seat, outputs: Vec::new() });
        let audio = slot.audio.clone().unwrap_or_default();
        let input_backend = b.input.kind();
        let input_interfaces = if input_backend == seat_core::backend::InputBackendKind::RawInputHost {
            let eff = st.inventory.effective(&st.config);
            // Pending hotkey moves (runtime overrides) count too, so a moved keyboard is
            // read by its new seat's host at once (spec 50 §4.1.9).
            eff.iter()
                .filter(|e| st.input_overrides.get(&e.key).copied().unwrap_or(e.mask).single() == Some(seat))
                .filter_map(|e| st.inventory.get(&e.key))
                .filter_map(|e| match &e.device.details {
                    seat_core::device::DeviceDetails::Input(i) if !i.interface_path.is_empty() => Some(i.interface_path.clone()),
                    _ => None,
                })
                .collect()
        } else {
            Vec::new()
        };
        // Thumbprints the host pins the listener TLS certificate to (read-only lookup).
        let listener_cert_sha1 = seat_rdp::pin::listener_pins().to_hex();
        // The host's own forwarder applies the owner's hotkey / system-button settings.
        let input = st.config.settings.input.clone();
        // Clipboard / drive / smart-card redirection for the host's IronRDP client: the
        // seat's policy narrowed to the devices it owns (spec 70).
        let redirection = match st.config.seat(seat) {
            Some(cfg) => {
                let live: Vec<seat_core::device::Device> = st.inventory.live().cloned().collect();
                // Each drive letter is pinned to the volume it names now; the host refuses it
                // when the letter names another volume by the time it opens it.
                #[cfg(windows)]
                let volume_of = seat_rdp::policy::volume::name_of_letter;
                #[cfg(not(windows))]
                let volume_of = |_: char| None::<String>;
                // Print queues of seat-owned printers (spooler lookup, only when the seat's
                // printer policy is `seat`).
                let queues_of = |id: &str| seat_media::printer_queues_for_device(id);
                seat_rdp::policy::host_redirection_with(
                    seat,
                    &cfg.local_settings().redirect,
                    &st.inventory.effective(&st.config),
                    &live,
                    &volume_of,
                    &queues_of,
                )
            }
            None => Default::default(),
        };
        // The manager shows the notes next to the seat.
        if let Some(slot) = st.seats.get_mut(&seat)
            && slot.rt.redirect_notes != redirection.notes
        {
            slot.rt.redirect_notes = redirection.notes.clone();
            self.events.publish(seat_proto::Event::SeatState { runtime: slot.rt.clone() });
        }
        Ok(ResponseBody::HostAssignment(HostAssignment {
            seat,
            connection: launch.connection,
            outputs,
            audio,
            input_backend,
            input_interfaces,
            listener_cert_sha1,
            input,
            redirection: Box::new(redirection),
        }))
    }

    /// A refusal of the seat host: kept on the seat's runtime (newest last, capped) and
    /// published, so the manager can show it; also logged.
    fn on_redirect_refusal(&self, seat: SeatNumber, channel: String, reason: String, detail: String, suppressed: u32) {
        let clip = |s: String, n: usize| s.chars().take(n).collect::<String>();
        let r = seat_core::state::RedirectRefusal {
            channel: clip(channel, 16),
            reason: clip(reason, 64),
            detail: clip(detail, 200),
            suppressed,
            at_ms: self.now(),
        };
        log::info(&format!("seat {seat}: redirection refused: {} {} {} (+{})", r.channel, r.reason, r.detail, r.suppressed));
        let mut st = self.lock();
        let Some(slot) = st.seats.get_mut(&seat) else { return };
        slot.rt.redirect_refusals.push(r);
        let n = slot.rt.redirect_refusals.len();
        if n > seat_core::state::MAX_REDIRECT_REFUSALS {
            slot.rt.redirect_refusals.drain(..n - seat_core::state::MAX_REDIRECT_REFUSALS);
        }
        let rt = slot.rt.clone();
        drop(st);
        self.events.publish(seat_proto::Event::SeatState { runtime: rt });
    }

    fn host_credentials(&self, seat: SeatNumber) -> R {
        let mut st = self.lock();
        let creds = self.credentials(&st, seat);
        let host = st.seats.get_mut(&seat).and_then(|s| s.host.as_mut()).ok_or_else(|| err(ErrorCode::SeatNotRunning, "no host"))?;
        if host.creds_taken {
            return Err(err(ErrorCode::AccessDenied, "credentials were already handed out").with_reason("credentials_taken"));
        }
        host.creds_taken = true;
        let c = match creds {
            Some(c) => Credentials { domain: c.domain, user: c.user, password: c.password },
            None => {
                // Logon screen: no account; hint the last user (spec 30 §6.2.8).
                let _ = Service::account_of(&st, seat);
                Credentials { domain: String::new(), user: String::new(), password: None }
            }
        };
        Ok(ResponseBody::Credentials(c))
    }
}

/// Cores-per-seat issues as config issues. No CPU-set list (query failed): no check.
pub(crate) fn cpu_issues(cfg: &Config, infos: &[seat_proto::CpuSetInfo]) -> Vec<seat_core::validate::Issue> {
    use seat_compat::cpu::{CpuIssueCode, SeatCpu, validate_sets};
    use seat_core::validate::{Issue, IssueCode};
    if infos.is_empty() {
        return Vec::new();
    }
    let seats: Vec<SeatCpu<'_>> = cfg
        .seats
        .iter()
        .filter(|s| !s.cpu_sets.is_empty())
        .map(|s| SeatCpu { seat: s.number, cpu_sets: &s.cpu_sets, hard: s.cpu_hard_limit })
        .collect();
    validate_sets(&seats, infos)
        .into_iter()
        .filter_map(|i| {
            let (severity, code) = match i.code {
                CpuIssueCode::NoCore => (Severity::Error, IssueCode::CpuNoCore),
                CpuIssueCode::UnknownId => (Severity::Warning, IssueCode::CpuUnknownId),
                CpuIssueCode::OnlyEfficiencyCores => (Severity::Warning, IssueCode::CpuOnlyEfficiencyCores),
                // Informational only (supported / allowed).
                CpuIssueCode::HardLimitMultiGroup | CpuIssueCode::SharedCore => return None,
            };
            Some(Issue { severity, code, seat: Some(i.seat), device: None, field: Some("seats.cpu_sets".into()), reason: None })
        })
        .collect()
}
