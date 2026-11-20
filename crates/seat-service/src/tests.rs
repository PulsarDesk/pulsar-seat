// SPDX-License-Identifier: GPL-3.0-only
//! Service-level tests over the seat-core mocks and [`MockPlatform`]: requests,
//! authorisation, config apply, seat life cycle, host handshake, watchdog, safety, secrets.

use crate::auth::Caller;
use crate::backends::testing::MockFactory;
use crate::platform::mock::MockPlatform;
use crate::runstate::{RunState, RunStore};
use crate::seats::SessionChange;
use crate::service::{Deps, Service, Spawner};
use seat_core::backend::Availability;
use seat_core::backend::{BackendError, ErrorKind};
use seat_core::config::{Config, DeviceAssignment, SameAccountPolicy, SeatAccount, SeatConfig};
use seat_core::device::DeviceKind;
use seat_core::mock::{MockClock, device};
use seat_core::state::{FailureReason, ProductState, SafeModeReason, SeatState};
use seat_core::store::{MemorySecretStore, MemoryStore};
use seat_core::{Clock, DeviceKey, SeatMask, SeatNumber, Secret};
use seat_proto::{
    AgentTaskKind, ClientKind, ErrorCode, Event, Hello, Request, RequestBody, ResponseBody, Topic,
};
use std::sync::{Arc, Mutex};
use std::sync::atomic::Ordering;

struct Inline;
impl Spawner for Inline {
    fn spawn(&self, f: Box<dyn FnOnce() + Send>) {
        f();
    }
}

struct Harness {
    s: Arc<Service>,
    f: Arc<MockFactory>,
    p: Arc<MockPlatform>,
    clock: Arc<MockClock>,
    dir: std::path::PathBuf,
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn s(n: u8) -> SeatNumber {
    SeatNumber::new(n).unwrap()
}

fn k(x: &str) -> DeviceKey {
    DeviceKey::new(x).unwrap()
}

fn assign(key: &str, kind: DeviceKind, mask: SeatMask) -> DeviceAssignment {
    DeviceAssignment {
        key: k(key),
        kind,
        mask,
        name: key.into(),
        label: None,
        hardware_id: None,
        serial: None,
        container_id: None,
        options: Default::default(),
    }
}

/// Two seats, seat 2 with its own monitor / keyboard / mouse / speaker, product enabled.
fn two_seat_config() -> Config {
    let mut c = Config::default();
    c.settings.enabled = true;
    c.settings.timings.between_seats_ms = 0;
    c.settings.timings.relogon_delay_ms = 0;
    let mut two = SeatConfig::new(s(2));
    two.account = SeatAccount::Local { user: "ali".into() };
    c.seats.push(two);
    c.devices.push(assign("mon;2", DeviceKind::Monitor, s(2).mask()));
    c.devices.push(assign("kbd;2", DeviceKind::Keyboard, s(2).mask()));
    c.devices.push(assign("mou;2", DeviceKind::Mouse, s(2).mask()));
    c.devices.push(assign("aout;2", DeviceKind::AudioRender, s(2).mask()));
    c
}

static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn harness_with(config: Config, prev: Option<RunState>) -> Harness {
    harness_full(config, prev, None)
}

fn harness_full(config: Config, prev: Option<RunState>, remote: Option<crate::service::RemoteDeps>) -> Harness {
    let f = Arc::new(MockFactory::default());
    *f.display.devices.lock().unwrap() = vec![device("mon;1", DeviceKind::Monitor), device("mon;2", DeviceKind::Monitor)];
    *f.input.devices.lock().unwrap() = vec![
        device("kbd;1", DeviceKind::Keyboard),
        device("mou;1", DeviceKind::Mouse),
        device("kbd;2", DeviceKind::Keyboard),
        device("mou;2", DeviceKind::Mouse),
    ];
    *f.audio.devices.lock().unwrap() = vec![device("aout;2", DeviceKind::AudioRender)];
    let p = Arc::new(MockPlatform::new());
    p.sign_in(1, "konsol");
    let clock = Arc::new(MockClock::default());
    let dir = std::env::temp_dir().join(format!("seat-service-t-{}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::SeqCst)));
    let run_store = RunStore::new(&dir.join("run"));
    if let Some(r) = prev {
        run_store.save(&r).unwrap();
    }
    let svc = Service::new(Deps {
        store: Box::new(MemoryStore::with(config)),
        secrets: Box::new(MemorySecretStore::default()),
        run_store,
        platform: p.clone(),
        factory: f.clone(),
        clock: clock.clone(),
        spawner: Arc::new(Inline),
        data_dir: dir.clone(),
        console_mode: true,
        remote,
    });
    svc.host_grace_ms.store(0, Ordering::SeqCst);
    svc.remote_stop_wait_ms.store(0, Ordering::SeqCst);
    Harness { s: svc, f, p, clock, dir }
}

fn harness() -> Harness {
    let h = harness_with(two_seat_config(), None);
    h.s.boot();
    h
}

fn admin() -> Caller {
    Caller {
        pid: 7,
        session_id: 1,
        user_sid: Some("S-1-5-21-9-konsol".into()),
        is_admin: true,
        is_system: false,
        client: None,
        host_seat: None,
        agent_session: None,
        console_listener: false,
        credprov_session: None,
        kiosk_seat: None,
        // Overwritten by `hello`; callers built with a client kind count as current.
        protocol: seat_proto::PROTOCOL_VERSION,
    }
}

fn user(session: u32) -> Caller {
    Caller { is_admin: false, session_id: session, ..admin() }
}

fn hello_body(client: ClientKind) -> RequestBody {
    RequestBody::Hello(Hello { client, client_version: "t".into(), protocol_version: seat_proto::PROTOCOL_VERSION, seat: None, token: None })
}

fn call(h: &Harness, c: &mut Caller, body: RequestBody) -> Result<ResponseBody, seat_proto::ProtoError> {
    if c.client.is_none() && !matches!(body, RequestBody::Hello(_)) {
        h.s.handle(c, Request::new(0, hello_body(ClientKind::Manager))).response.result.unwrap();
    }
    h.s.handle(c, Request::new(1, body)).response.result
}

fn state(h: &Harness, seat: SeatNumber) -> SeatState {
    h.s.status().seats.into_iter().find(|r| r.seat == seat).unwrap().state
}

#[test]
fn hello_and_version() {
    let h = harness();
    let mut c = user(1);
    let r = call(&h, &mut c, hello_body(ClientKind::Manager)).unwrap();
    let ResponseBody::Hello(reply) = r else { panic!() };
    assert_eq!(reply.caller.seat, Some(SeatNumber::CONSOLE));
    assert!(!reply.caller.is_admin);
    let bad = Request { version: 99, id: 3, body: RequestBody::GetStatus };
    assert_eq!(h.s.handle(&mut c, bad).response.result.unwrap_err().code, ErrorCode::VersionMismatch);
    // Status before hello on a fresh connection is refused.
    let mut fresh = user(1);
    let r = h.s.handle(&mut fresh, Request::new(1, RequestBody::GetStatus)).response.result;
    assert_eq!(r.unwrap_err().code, ErrorCode::BadRequest);
}

#[test]
fn authorisation_table_enforced() {
    let h = harness();
    let mut u = user(1);
    assert!(matches!(call(&h, &mut u, RequestBody::GetStatus), Ok(ResponseBody::Status(_))));
    assert_eq!(call(&h, &mut u, RequestBody::GetConfig).unwrap_err().code, ErrorCode::AccessDenied);
    assert_eq!(call(&h, &mut u, RequestBody::StartSeats { seats: None }).unwrap_err().code, ErrorCode::AccessDenied);
    let mut a = admin();
    assert!(matches!(call(&h, &mut a, RequestBody::GetConfig), Ok(ResponseBody::Config(_))));
    // A non-admin may re-logon their own seat (seat 1 = console session 1) only.
    assert!(call(&h, &mut u, RequestBody::RelogonSeat { seat: s(2) }).is_err());
    assert!(call(&h, &mut u, RequestBody::RelogonSeat { seat: s(1) }).is_ok());
    assert_eq!(h.p.logged_off.lock().unwrap().as_slice(), &[1]);
}

#[test]
fn boot_enabled_manual_and_status() {
    let h = harness();
    let st = h.s.status();
    assert_eq!(st.product, ProductState::Enabled { started: false });
    assert_eq!(state(&h, s(1)), SeatState::SignedIn { user: "PC\\konsol".into() });
    assert_eq!(state(&h, s(2)), SeatState::Off);
    // Ownership applied at boot: input routes and console topology.
    assert!(h.f.input.rec.calls().iter().any(|c| c.starts_with("apply_routes")));
    assert!(h.f.display.rec.calls().iter().any(|c| c == "apply_topology 1"));
    assert!(h.f.gate.rec.calls().iter().any(|c| c.starts_with("apply")));
    assert!(st.fallback_notes.iter().any(|n| n.starts_with("gate:session_drive_letters")));
}

#[test]
fn frame_interval_reaches_the_session_backend_before_prepare_and_follows_monitors() {
    use seat_core::device::{DeviceDetails, DisplayInfo, DisplayMode};
    let h = harness();
    // Boot: sent with the listener plan, before prepare_host. No mode known = 60 Hz.
    assert_eq!(h.f.session.frame_intervals.lock().unwrap().last(), Some(&Some(16)));
    assert!(h.f.session.rec.calls().contains(&"prepare_host".to_string()));
    // Seat 2's monitor reports 144 Hz: the interval follows (seat 1's console monitor does not count).
    let mon = |key: &str, mhz: u32| {
        let mut d = device(key, DeviceKind::Monitor);
        d.details = DeviceDetails::Display(DisplayInfo {
            mode: Some(DisplayMode { width: 2560, height: 1440, refresh_mhz: mhz, rotation: 0 }),
            ..Default::default()
        });
        d
    };
    *h.f.display.devices.lock().unwrap() = vec![mon("mon;1", 240_000), mon("mon;2", 144_000)];
    h.s.refresh_inventory();
    assert_eq!(h.f.session.frame_intervals.lock().unwrap().last(), Some(&Some(6)));
    // Turned off in the config: `None` goes out with the next plan.
    let mut a = admin();
    let base = h.s.status().config_revision;
    let ResponseBody::Config(view) = call(&h, &mut a, RequestBody::GetConfig).unwrap() else { panic!() };
    let mut cfg = view.config.clone();
    cfg.settings.outputs.session_frame_rate.mode = seat_core::config::SessionFrameRateMode::Off;
    call(&h, &mut a, RequestBody::ApplyConfig { config: Box::new(cfg), base_revision: base, force: false }).unwrap();
    assert_eq!(h.f.session.frame_intervals.lock().unwrap().last(), Some(&None));
}

#[test]
fn start_seat_full_flow_with_host_handshake() {
    let h = harness();
    h.s.secrets.set(s(2), Some(&Secret::new("hunter2"))).unwrap();
    let mut a = admin();
    call(&h, &mut a, RequestBody::StartSeats { seats: None }).unwrap();
    let launched = h.p.launched();
    assert_eq!(launched.len(), 1);
    assert!(launched[0].exe.ends_with("pulsar-seat-host.exe"));
    assert_eq!(&launched[0].args[..2], &["--seat".to_string(), "2".to_string()]);
    // The token goes over stdin, never into the command line.
    assert_eq!(launched[0].args, vec!["--seat".to_string(), "2".to_string(), "--token-stdin".to_string()]);
    let token = launched[0].stdin_secret.clone().expect("host token on stdin").expose().to_string();
    assert!(crate::platform::token_frame(&Secret::new(token.clone())).is_some_and(|f| f.len() == 4 + token.len()));
    assert_eq!(state(&h, s(2)), SeatState::AtLogon);
    assert_eq!(h.s.status().product, ProductState::Enabled { started: true });
    assert!(h.f.audio.rec.calls().contains(&"bind_seat 2".to_string()));
    assert!(h.f.display.rec.calls().contains(&"bind_seat 2 1".to_string()));
    {
        // Display preferences reach the binding before the seat's targets are bound.
        let calls = h.f.display.rec.calls();
        let conf = calls.iter().position(|c| c.starts_with("configure_seat 2 ")).expect("configure_seat recorded");
        let bind = calls.iter().position(|c| c == "bind_seat 2 1").unwrap();
        assert!(conf < bind, "{calls:?}");
    }

    // Host handshake: wrong token / pid refused, right one accepted.
    let host_hello = |tok: &str| {
        RequestBody::Hello(Hello {
            client: ClientKind::SeatHost,
            client_version: "t".into(),
            protocol_version: 1,
            seat: Some(s(2)),
            token: Some(Secret::new(tok)),
        })
    };
    let mut fake = Caller { pid: 1000, ..user(1) };
    assert_eq!(call(&h, &mut fake, host_hello("nope")).unwrap_err().code, ErrorCode::AccessDenied);
    let mut wrong_pid = Caller { pid: 4242, ..user(1) };
    assert!(call(&h, &mut wrong_pid, host_hello(&token)).is_err());
    let mut host = Caller { pid: 1000, ..user(1) };
    call(&h, &mut host, host_hello(&token)).unwrap();
    let ResponseBody::HostAssignment(asg) = call(&h, &mut host, RequestBody::HostGetAssignment { seat: s(2) }).unwrap() else {
        panic!()
    };
    assert_eq!(asg.connection.client_name, "PSEAT-02");
    assert_eq!(asg.outputs.outputs.len(), 1);
    assert_eq!(asg.audio.render_endpoint_id.as_deref(), Some("aout;2"));
    assert!(call(&h, &mut host, RequestBody::HostGetAssignment { seat: s(3) }).is_err());
    let ResponseBody::Credentials(cr) = call(&h, &mut host, RequestBody::HostTakeCredentials { seat: s(2) }).unwrap() else {
        panic!()
    };
    assert_eq!(cr.user, "ali");
    assert_eq!(cr.password.unwrap().expose(), "hunter2");
    // Only once per launch.
    let again = call(&h, &mut host, RequestBody::HostTakeCredentials { seat: s(2) }).unwrap_err();
    assert_eq!(again.reason.as_deref(), Some("credentials_taken"));
    // Host topic only for the verified host.
    assert!(call(&h, &mut host, RequestBody::Subscribe { topics: vec![Topic::Host] }).is_ok());
    let mut other = user(1);
    assert!(call(&h, &mut other, RequestBody::Subscribe { topics: vec![Topic::Host] }).is_err());

    // Known session persisted for adoption.
    let run = RunStore::new(&h.dir.join("run")).load();
    assert_eq!(run.known_sessions.len(), 1);
    assert_eq!(run.known_sessions[0].client_name, "PSEAT-02");

    // Sign in on the seat: agent launched, gate re-planned with the logon SID.
    h.p.sign_in(10, "ali");
    h.s.on_session_change(SessionChange::Logon, 10);
    assert_eq!(state(&h, s(2)), SeatState::SignedIn { user: "PC\\ali".into() });
    assert!(h.p.launched().iter().any(|l| l.exe.ends_with("pulsar-seat-agent.exe") && l.session_id == 10));
    h.s.tick();
    let plan = h.f.gate.last_plan.lock().unwrap().clone().unwrap();
    assert!(plan.logons.iter().any(|l| l.seat == s(2) && l.logon_sid == "S-1-5-5-0-10"));

    // Stop: host told to exit, session logged off, display / audio released.
    call(&h, &mut a, RequestBody::StopSeat { seat: s(2) }).unwrap();
    assert_eq!(state(&h, s(2)), SeatState::Off);
    assert!(h.f.session.rec.calls().contains(&"log_off 2".to_string()));
    assert!(h.f.display.rec.calls().contains(&"release_seat 2".to_string()));
    assert!(!h.p.process_alive_pub(1000));
    assert_eq!(call(&h, &mut a, RequestBody::StopSeat { seat: s(2) }).unwrap_err().code, ErrorCode::SeatNotRunning);
    assert_eq!(call(&h, &mut a, RequestBody::StopSeat { seat: s(1) }).unwrap_err().code, ErrorCode::Unsupported);
}

impl MockPlatform {
    fn process_alive_pub(&self, pid: u32) -> bool {
        crate::platform::Platform::process_alive(self, pid)
    }
}

#[test]
fn seat_without_monitor_waits_then_starts_on_arrival() {
    let mut c = two_seat_config();
    c.devices.retain(|d| d.key.as_str() != "mon;2");
    c.devices.push(assign("mon;3", DeviceKind::Monitor, s(2).mask()));
    let h = harness_with(c, None);
    h.s.boot();
    let mut a = admin();
    call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(2)]) }).unwrap();
    assert_eq!(state(&h, s(2)), SeatState::WaitingForDisplay);
    // The monitor is plugged in.
    h.f.display.devices.lock().unwrap().push(device("mon;3", DeviceKind::Monitor));
    h.p.devices_changed.store(true, Ordering::SeqCst);
    h.s.tick(); // inventory refresh marks the retry
    h.s.tick(); // retry due
    assert_eq!(state(&h, s(2)), SeatState::AtLogon);
}

#[test]
fn watchdog_restarts_with_backoff_then_gives_up() {
    let h = harness();
    let mut a = admin();
    call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(2)]) }).unwrap();
    for round in 0..3 {
        let pid = *h.p.alive.lock().unwrap().iter().max().unwrap();
        h.p.kill(pid);
        h.s.tick();
        if round < 2 {
            assert_eq!(state(&h, s(2)), SeatState::Starting { phase: seat_core::state::StartPhase::Queued }, "round {round}");
            // Not before the back-off.
            h.s.tick();
            assert_eq!(h.p.launched().len(), round + 1);
            h.clock.advance(70_000);
            h.s.tick();
            assert_eq!(h.p.launched().len(), round + 2, "restarted after back-off");
            assert_eq!(state(&h, s(2)), SeatState::AtLogon);
        }
    }
    assert!(matches!(state(&h, s(2)), SeatState::Error { reason: FailureReason::HostExited, .. }));
}

#[test]
fn logon_failure_marks_password_failing_and_set_clears() {
    let h = harness();
    h.s.secrets.set(s(2), Some(&Secret::new("old"))).unwrap();
    h.f.session.rec.fail_next(BackendError::new(ErrorKind::Logon, "bad_password", "wrong password"));
    let mut a = admin();
    call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(2)]) }).unwrap();
    assert!(matches!(state(&h, s(2)), SeatState::Error { reason: FailureReason::LogonFailed, .. }));
    let ResponseBody::Config(v) = call(&h, &mut a, RequestBody::GetConfig).unwrap() else { panic!() };
    assert_eq!(v.password_failing, vec![s(2)]);
    assert_eq!(v.password_set, vec![s(2)]);
    // No retry loop: the failing password is not handed out.
    assert!(h.s.credentials(&h.s.lock(), s(2)).unwrap().password.is_none());
    call(&h, &mut a, RequestBody::SetSeatPassword { seat: s(2), password: Some(Secret::new("new")) }).unwrap();
    let ResponseBody::Config(v) = call(&h, &mut a, RequestBody::GetConfig).unwrap() else { panic!() };
    assert!(v.password_failing.is_empty());
    call(&h, &mut a, RequestBody::SetSeatPassword { seat: s(2), password: None }).unwrap();
    assert!(!h.s.secrets.has(s(2)));
}

#[test]
fn apply_config_rules() {
    let h = harness();
    let mut a = admin();
    let base = h.s.status().config_revision;
    let ResponseBody::Config(view) = call(&h, &mut a, RequestBody::GetConfig).unwrap() else { panic!() };
    let mut cfg = view.config.clone();
    // Stale revision.
    let r = call(&h, &mut a, RequestBody::ApplyConfig { config: Box::new(cfg.clone()), base_revision: base + 5, force: false });
    assert_eq!(r.unwrap_err().code, ErrorCode::RevisionConflict);
    // Exclusive device on two seats is an error.
    cfg.devices.iter_mut().find(|d| d.key.as_str() == "kbd;2").unwrap().mask = SeatMask(0b11);
    let r = call(&h, &mut a, RequestBody::ApplyConfig { config: Box::new(cfg.clone()), base_revision: base, force: false });
    let e = r.unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidConfig);
    assert!(e.message.contains("exclusive_on_several_seats"));
    // Experimental backend without the acknowledgement.
    let mut exp = view.config.clone();
    exp.settings.backends.session = seat_core::backend::SessionBackendKind::TermsrvWrapper;
    let r = call(&h, &mut a, RequestBody::ApplyConfig { config: Box::new(exp), base_revision: base, force: false });
    assert_eq!(r.unwrap_err().code, ErrorCode::WarningNotAcknowledged);
    // A valid change: new seat 3; events and store.
    let rx = h.s.events.subscribe(vec![Topic::Status], Default::default());
    let mut ok = view.config.clone();
    ok.seats.push(SeatConfig::new(s(3)));
    let ResponseBody::Applied(res) =
        call(&h, &mut a, RequestBody::ApplyConfig { config: Box::new(ok), base_revision: base, force: false }).unwrap()
    else {
        panic!()
    };
    assert_eq!(res.revision, base + 1);
    assert!(res.issues.iter().any(|i| i.code == seat_core::validate::IssueCode::SeatWithoutDisplay));
    assert!(rx.try_iter().any(|f| f.event == Event::ConfigChanged { revision: base + 1 }));
    assert_eq!(h.s.store.load().unwrap().seats.len(), 3);
    assert!(h.s.status().seats.iter().any(|r| r.seat == s(3)));
}

#[test]
fn conflicts_need_force() {
    let h = harness();
    let mut a = admin();
    let ResponseBody::Config(view) = call(&h, &mut a, RequestBody::GetConfig).unwrap() else { panic!() };
    let base = view.config.revision;
    // Put a hub on seat 2 and a device below it on seat 1 explicitly.
    let mut hub = device("hub;h", DeviceKind::UsbHub);
    hub.parent_hub = None;
    let mut stor = device("stor;x", DeviceKind::Storage);
    stor.parent_hub = Some(k("hub;h"));
    *h.f.gate.devices.lock().unwrap() = vec![hub, stor];
    h.s.refresh_inventory();
    let mut cfg = view.config.clone();
    cfg.devices.push(assign("hub;h", DeviceKind::UsbHub, s(2).mask()));
    cfg.devices.push(assign("stor;x", DeviceKind::Storage, s(1).mask()));
    let r = call(&h, &mut a, RequestBody::ApplyConfig { config: Box::new(cfg.clone()), base_revision: base, force: false });
    assert_eq!(r.unwrap_err().code, ErrorCode::ConflictsNotConfirmed);
    assert!(call(&h, &mut a, RequestBody::ApplyConfig { config: Box::new(cfg), base_revision: base, force: true }).is_ok());
}

#[test]
fn same_account_deny_logs_the_second_seat_off() {
    let mut c = two_seat_config();
    c.settings.same_account = SameAccountPolicy::Deny;
    let h = harness_with(c, None);
    h.s.boot();
    let mut a = admin();
    call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(2)]) }).unwrap();
    // The console user signs in on seat 2 too.
    h.p.sign_in(10, "konsol");
    h.s.on_session_change(SessionChange::Logon, 10);
    assert!(matches!(state(&h, s(2)), SeatState::Error { reason: FailureReason::SameAccountDenied, .. }));
    let tasks = h.s.lock().agent_tasks.get(&10).cloned().unwrap();
    assert!(tasks.iter().any(|t| matches!(t.task, AgentTaskKind::LogOff { .. })));
    // A process that only claims to be an agent is refused.
    let mut fake = Caller { pid: 4242, ..user(10) };
    assert_eq!(call(&h, &mut fake, hello_body(ClientKind::Agent)).unwrap_err().code, ErrorCode::AccessDenied);
    // The agent the service launched fetches them, for its own session only.
    let agent_pid = h.s.lock().seats.get(&s(2)).unwrap().agent_pid.unwrap();
    let mut agent = Caller { pid: agent_pid, ..user(10) };
    call(&h, &mut agent, hello_body(ClientKind::Agent)).unwrap();
    let report = seat_proto::AgentReport { session_id: 10, event: seat_proto::AgentEvent::Started };
    let ResponseBody::AgentTasks(t) = call(&h, &mut agent, RequestBody::AgentReport { report }).unwrap() else { panic!() };
    // The DPI read-back task (compat-tweaks 2026-11-17) is queued at every logon too.
    assert_eq!(t.iter().filter(|t| t.task != AgentTaskKind::ReportDpi).count(), 2);
    let foreign = seat_proto::AgentReport { session_id: 1, event: seat_proto::AgentEvent::Started };
    assert!(call(&h, &mut agent, RequestBody::AgentReport { report: foreign }).is_err());
    // Whatever the agent does (here: acks the log-off task and stops), the service logs
    // the session off itself when the timer is due.
    let id = t.iter().find(|t| matches!(t.task, AgentTaskKind::LogOff { .. })).unwrap().id;
    let done = seat_proto::AgentReport { session_id: 10, event: seat_proto::AgentEvent::TaskDone { task_id: id } };
    call(&h, &mut agent, RequestBody::AgentReport { report: done }).unwrap();
    h.s.tick();
    assert!(h.p.logged_off.lock().unwrap().is_empty(), "not before the notice time");
    h.clock.advance(u64::from(crate::seats::DENY_LOGOFF_AFTER_MS) + 1);
    h.s.tick();
    assert_eq!(h.p.logged_off.lock().unwrap().as_slice(), &[10]);
}

#[test]
fn stale_start_worker_leaves_a_newer_start_alone() {
    let h = harness();
    let mut a = admin();
    call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(2)]) }).unwrap();
    let old = h.s.lock().seats.get(&s(2)).unwrap().generation;
    // Re-logon: stop + start again (new generation, new host).
    call(&h, &mut a, RequestBody::RelogonSeat { seat: s(2) }).unwrap();
    let (newer, host) = {
        let st = h.s.lock();
        let slot = st.seats.get(&s(2)).unwrap();
        (slot.generation, slot.host.clone().unwrap().pid)
    };
    assert!(newer > old);
    assert_eq!(state(&h, s(2)), SeatState::AtLogon);
    // The old worker returns late from a backend call.
    let releases = h.f.display.rec.calls().iter().filter(|c| *c == "release_seat 2").count();
    h.s.run_start(s(2), old);
    assert!(h.p.process_alive_pub(host), "the newer host survives");
    assert_eq!(state(&h, s(2)), SeatState::AtLogon);
    assert_eq!(h.f.display.rec.calls().iter().filter(|c| *c == "release_seat 2").count(), releases);
    assert!(h.s.lock().seats.get(&s(2)).unwrap().rt.session_id.is_some());
}

#[test]
fn hosts_run_as_system_agents_as_the_user() {
    let h = harness();
    let mut a = admin();
    call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(2)]) }).unwrap();
    h.p.sign_in(10, "ali");
    h.s.on_session_change(SessionChange::Logon, 10);
    let l = h.p.launched();
    assert!(l.iter().any(|x| x.exe.ends_with("pulsar-seat-host.exe") && x.as_system));
    assert!(l.iter().any(|x| x.exe.ends_with("pulsar-seat-agent.exe") && !x.as_system));
}

#[test]
fn rng_failure_never_launches_a_host() {
    let h = harness();
    h.p.fail_rng.store(true, Ordering::SeqCst);
    let mut a = admin();
    call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(2)]) }).unwrap();
    assert!(h.p.launched().is_empty());
    let SeatState::Error { detail, .. } = state(&h, s(2)) else { panic!("{:?}", state(&h, s(2))) };
    assert!(detail.contains("reason=rng_failed"), "{detail}");
}

#[test]
fn failure_detail_names_the_step() {
    let h = harness();
    h.f.session.rec.fail_next(BackendError::new(ErrorKind::Logon, "bad_password", "wrong password").with_win32(1326));
    let mut a = admin();
    call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(2)]) }).unwrap();
    let SeatState::Error { detail, .. } = state(&h, s(2)) else { panic!() };
    assert_eq!(detail, "step=launching_host reason=bad_password win32=1326");
}

#[test]
fn hung_start_times_out_with_its_step() {
    let h = harness();
    let mut a = admin();
    call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(2)]) }).unwrap();
    // Simulate a worker stuck in wait_session.
    {
        let mut st = h.s.lock();
        h.s.set_seat_state(&mut st, s(2), SeatState::Starting { phase: seat_core::state::StartPhase::WaitingForSession });
    }
    let budget = Service::start_budget_ms(&h.s.lock());
    h.clock.advance(budget - 1);
    h.s.tick();
    assert!(matches!(state(&h, s(2)), SeatState::Starting { .. }));
    h.clock.advance(2);
    h.s.tick();
    let SeatState::Error { reason, detail } = state(&h, s(2)) else { panic!() };
    assert_eq!(reason, FailureReason::SessionTimeout);
    assert!(detail.starts_with("step=waiting_for_session"), "{detail}");
    assert!(h.s.lock().seats.get(&s(2)).unwrap().host.is_none(), "released");
}

#[test]
fn admin_only_keeps_hosts_subscribed() {
    let mut c = two_seat_config();
    c.settings.admin_only = true;
    let h = harness_with(c, None);
    h.s.boot();
    let mut a = admin();
    call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(2)]) }).unwrap();
    let (pid, token) = {
        let st = h.s.lock();
        let hp = st.seats.get(&s(2)).unwrap().host.clone().unwrap();
        (hp.pid, hp.token)
    };
    let mut host = Caller { pid, ..user(1) };
    let hello = RequestBody::Hello(Hello {
        client: ClientKind::SeatHost,
        client_version: "t".into(),
        protocol_version: 1,
        seat: Some(s(2)),
        token: Some(token),
    });
    call(&h, &mut host, hello).unwrap();
    assert!(call(&h, &mut host, RequestBody::Subscribe { topics: vec![Topic::Host] }).is_ok());
    assert!(call(&h, &mut host, RequestBody::PollEvents { after: 0, wait_ms: 0, topics: vec![Topic::Host] }).is_ok());
    // A plain user is still restricted.
    let mut u = user(1);
    assert_eq!(call(&h, &mut u, RequestBody::GetStatus).unwrap_err().code, ErrorCode::AdminOnly);
}

#[test]
fn password_length_is_limited() {
    let h = harness();
    let mut a = admin();
    let long = "x".repeat(crate::requests::MAX_PASSWORD_CHARS + 1);
    let e = call(&h, &mut a, RequestBody::SetSeatPassword { seat: s(2), password: Some(Secret::new(long)) }).unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidValue);
    let ok = "x".repeat(crate::requests::MAX_PASSWORD_CHARS);
    call(&h, &mut a, RequestBody::SetSeatPassword { seat: s(2), password: Some(Secret::new(ok)) }).unwrap();
    let ResponseBody::Config(v) = call(&h, &mut a, RequestBody::GetConfig).unwrap() else { panic!() };
    assert_eq!(v.password_set, vec![s(2)]);
}

#[test]
fn requests_wait_for_the_boot_decision() {
    let h = harness_with(two_seat_config(), Some(RunState { running: true, boot_unix_s: 1_000_000, unstable_starts: 1, ..Default::default() }));
    let mut a = admin();
    assert!(matches!(call(&h, &mut a, RequestBody::GetStatus), Ok(ResponseBody::Status(_))));
    assert_eq!(call(&h, &mut a, RequestBody::StartSeats { seats: None }).unwrap_err().code, ErrorCode::NotReady);
    // Phase 1 alone (as runtime::prepare does before the pipe) loads the run state.
    h.s.boot_prepare();
    assert_eq!(h.s.lock().run.unstable_starts, 2);
    call(&h, &mut a, RequestBody::SetSeatPassword { seat: s(2), password: None }).unwrap();
    // A request that saves the run state keeps the loaded counters.
    assert_eq!(RunStore::new(&h.dir.join("run")).load().unstable_starts, 2);
    h.s.boot();
    assert_eq!(h.s.lock().run.unstable_starts, 2, "boot does not decide twice");
}

#[test]
fn after_seat_one_logon_waits_for_the_console_user() {
    let mut c = two_seat_config();
    c.settings.start_mode = seat_core::config::StartMode::AfterSeatOneLogon;
    c.settings.enabled = false;
    let h = harness_with(c, None);
    h.p.users.lock().unwrap().remove(&1);
    h.s.boot();
    let mut a = admin();
    call(&h, &mut a, RequestBody::SetEnabled { enabled: true }).unwrap();
    assert!(h.p.launched().is_empty(), "nobody at the console yet");
    h.p.sign_in(1, "konsol");
    let hosts = || h.p.launched().iter().filter(|l| l.exe.ends_with("pulsar-seat-host.exe")).count();
    h.s.on_session_change(SessionChange::Logon, 1);
    assert_eq!(hosts(), 1);
    // A second logon event does not start them again.
    h.s.on_session_change(SessionChange::Logon, 1);
    assert_eq!(hosts(), 1);
}

#[test]
fn dedicated_host_starts_seat_one_as_a_session() {
    let mut c = two_seat_config();
    c.settings.backends.host_topology = seat_core::backend::session::HostTopology::DedicatedHost;
    c.devices.push(assign("mon;1", DeviceKind::Monitor, SeatMask::CONSOLE));
    let h = harness_with(c, None);
    h.s.boot();
    // The console user is the host account, not seat 1.
    assert_eq!(state(&h, s(1)), SeatState::Off);
    let mut a = admin();
    call(&h, &mut a, RequestBody::StartSeats { seats: None }).unwrap();
    let hosts: Vec<_> = h.p.launched().into_iter().filter(|l| l.exe.ends_with("pulsar-seat-host.exe")).collect();
    assert_eq!(hosts.len(), 2, "seat 1 and seat 2 both get a host");
    assert_eq!(state(&h, s(1)), SeatState::AtLogon);
    // A console logon is a host-session event, not a seat-1 logon.
    h.s.on_session_change(SessionChange::Logon, 1);
    assert_eq!(state(&h, s(1)), SeatState::AtLogon);
    // Seat 1 is stoppable here.
    call(&h, &mut a, RequestBody::StopSeat { seat: s(1) }).unwrap();
    assert_eq!(state(&h, s(1)), SeatState::Off);
}

#[test]
fn adoption_refuses_a_reused_pid() {
    let h = harness();
    let mut a = admin();
    call(&h, &mut a, RequestBody::StartSeats { seats: None }).unwrap();
    let pid = h.s.lock().seats.get(&s(2)).unwrap().host.clone().unwrap().pid;
    h.p.reused.lock().unwrap().insert(pid);
    let svc2 = Service::new(Deps {
        store: Box::new(MemoryStore::with(two_seat_config())),
        secrets: Box::new(MemorySecretStore::default()),
        run_store: RunStore::new(&h.dir.join("run")),
        platform: h.p.clone(),
        factory: h.f.clone(),
        clock: h.clock.clone(),
        spawner: Arc::new(Inline),
        data_dir: h.dir.clone(),
        console_mode: true,
        remote: None,
    });
    svc2.boot();
    assert!(svc2.lock().seats.get(&s(2)).unwrap().host.is_none());
    assert!(RunStore::new(&h.dir.join("run")).load().hosts.is_empty());
}

#[test]
fn same_account_warn_marks_degraded() {
    let h = harness();
    let mut a = admin();
    call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(2)]) }).unwrap();
    h.p.sign_in(10, "konsol");
    h.s.on_session_change(SessionChange::Logon, 10);
    let rt = h.s.status().seats.into_iter().find(|r| r.seat == s(2)).unwrap();
    assert!(rt.degraded.contains(&"same_account_warning".to_string()));
}

#[test]
fn crash_loop_and_unclean_shutdown_block_autostart() {
    let mut c = two_seat_config();
    c.settings.start_mode = seat_core::config::StartMode::AtBoot;
    // Three crashed starts in this boot.
    let prev = RunState { running: true, boot_unix_s: 1_000_000, unstable_starts: 3, os_build: 26100, ..Default::default() };
    let h = harness_with(c.clone(), Some(prev));
    h.s.boot();
    assert_eq!(h.s.status().product, ProductState::SafeMode { reason: SafeModeReason::CrashLoop });
    assert!(h.p.launched().is_empty(), "no automatic start in safe mode");
    // A manual start is still allowed; enabling again clears the safe mode.
    let mut a = admin();
    call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(2)]) }).unwrap();
    assert_eq!(h.p.launched().len(), 1);
    // Unclean shutdown in an earlier boot.
    let prev = RunState { running: true, boot_unix_s: 10, os_build: 26000, ..Default::default() };
    let h2 = harness_with(c.clone(), Some(prev));
    h2.s.boot();
    assert_eq!(h2.s.status().product, ProductState::SafeMode { reason: SafeModeReason::UncleanShutdown });
    assert!(h2.s.status().os_build_changed);
    // Clean previous stop: autostart runs.
    let prev = RunState { running: false, boot_unix_s: 10, os_build: 26100, ..Default::default() };
    let h3 = harness_with(c, Some(prev));
    h3.s.boot();
    assert_eq!(h3.p.launched().len(), 1);
}

#[test]
fn windows_safe_mode_serves_status_only() {
    let h = harness_with(two_seat_config(), None);
    h.p.safe_mode.store(true, Ordering::SeqCst);
    h.s.boot();
    assert_eq!(h.s.status().product, ProductState::SafeMode { reason: SafeModeReason::WindowsSafeMode });
    let mut a = admin();
    assert_eq!(call(&h, &mut a, RequestBody::StartSeats { seats: None }).unwrap_err().code, ErrorCode::SafeMode);
    assert!(h.f.input.rec.calls().is_empty(), "nothing applied in Windows Safe Mode");
}

#[test]
fn disabled_product_refuses_start_and_enable_works() {
    let mut c = two_seat_config();
    c.settings.enabled = false;
    let h = harness_with(c, None);
    h.s.boot();
    assert_eq!(h.s.status().product, ProductState::Disabled);
    let mut a = admin();
    assert_eq!(call(&h, &mut a, RequestBody::StartSeats { seats: None }).unwrap_err().code, ErrorCode::Disabled);
    call(&h, &mut a, RequestBody::SetEnabled { enabled: true }).unwrap();
    assert_eq!(h.s.status().product, ProductState::Enabled { started: false });
    assert!(h.f.session.rec.calls().contains(&"prepare_host".to_string()));
    call(&h, &mut a, RequestBody::StartSeats { seats: None }).unwrap();
    assert_eq!(h.s.status().product, ProductState::Enabled { started: true });
    call(&h, &mut a, RequestBody::SetEnabled { enabled: false }).unwrap();
    assert_eq!(h.s.status().product, ProductState::Disabled);
    assert_eq!(state(&h, s(2)), SeatState::Off);
    assert!(h.f.gate.rec.calls().contains(&"revert_all".to_string()));
    assert!(h.f.session.rec.calls().contains(&"restore_host".to_string()));
}

#[test]
fn session_fallback_reported() {
    let h = harness_with(two_seat_config(), None);
    *h.f.session.availability.lock().unwrap() = Availability::Unavailable { reason: "edition_unsupported".into() };
    h.s.reselect_backends();
    let st = h.s.status();
    assert_eq!(st.active.session, seat_core::backend::SessionBackendKind::FastUserSwitch);
    assert!(st.fallback_notes.iter().any(|n| n.contains("edition_unsupported")));
}

#[test]
fn session_end_returns_to_logon_screen() {
    let h = harness();
    let mut a = admin();
    call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(2)]) }).unwrap();
    assert_eq!(h.p.launched().len(), 1);
    h.s.on_session_change(SessionChange::Terminate, 10);
    assert!(matches!(state(&h, s(2)), SeatState::Starting { .. }));
    h.s.tick();
    assert_eq!(state(&h, s(2)), SeatState::AtLogon);
    assert_eq!(h.p.launched().len(), 2);
}

#[test]
fn devices_list_diff_move_and_identify() {
    let h = harness();
    let mut a = admin();
    let ResponseBody::Devices(v) = call(&h, &mut a, RequestBody::ListDevices).unwrap() else { panic!() };
    assert!(v.iter().any(|d| d.device.key.as_str() == "kbd;2" && d.effective == s(2).mask()));
    assert!(v.iter().any(|d| d.device.key.as_str() == "kbd;1" && d.effective == SeatMask::CONSOLE));
    let ResponseBody::Diff(d) = call(&h, &mut a, RequestBody::CompareSaved).unwrap() else { panic!() };
    assert!(d.added.contains(&k("kbd;1")));
    assert!(call(&h, &mut a, RequestBody::IdentifyDevice { key: k("mon;2"), on: true }).is_ok());
    assert!(h.f.display.rec.calls().contains(&"identify 1 true".to_string()));
    assert!(call(&h, &mut a, RequestBody::IdentifyDevice { key: k("aout;2"), on: true }).is_ok());
    assert!(h.f.audio.rec.calls().iter().any(|c| c.starts_with("test_tone aout;2")));
    // Move kbd;2's assignment to kbd;1 (known device on a new port).
    call(&h, &mut a, RequestBody::MoveAssignment { from: k("kbd;2"), to: k("kbd;1") }).unwrap();
    let cfg = h.s.store.load().unwrap();
    assert!(cfg.devices.iter().any(|d| d.key.as_str() == "kbd;1" && d.mask == s(2).mask()));
    assert!(!cfg.devices.iter().any(|d| d.key.as_str() == "kbd;2"));
    assert!(call(&h, &mut a, RequestBody::MoveAssignment { from: k("kbd;9"), to: k("kbd;1") }).is_err());
}

#[test]
fn baseline_inventory_fills_uncovered_kinds() {
    let h = harness_with(two_seat_config(), None);
    *h.p.devices.lock().unwrap() = vec![device("cam;x", DeviceKind::Webcam), device("kbd;dup", DeviceKind::Keyboard)];
    h.s.boot();
    let st = h.s.lock();
    // Webcams are not covered by the mock gate (storage + other) → baseline used.
    assert!(st.inventory.get(&k("cam;x")).is_some());
    // Keyboards are covered by the input backend → baseline ignored.
    assert!(st.inventory.get(&k("kbd;dup")).is_none());
}

#[test]
fn diagnostic_report_has_no_secrets_and_masks_users() {
    let h = harness();
    h.s.secrets.set(s(2), Some(&Secret::new("hunter2"))).unwrap();
    let mut a = admin();
    let ResponseBody::Report { path } = call(&h, &mut a, RequestBody::DiagnosticReport { mask_users: true }).unwrap() else {
        panic!()
    };
    let text = std::fs::read_to_string(path).unwrap();
    assert!(!text.contains("hunter2"));
    assert!(text.contains("local a***"));
    assert!(text.contains("password_set=true"));
    assert!(!text.contains("konsol"));
}

#[test]
fn reset_config_clears_everything() {
    let h = harness();
    h.s.secrets.set(s(2), Some(&Secret::new("x"))).unwrap();
    let mut a = admin();
    call(&h, &mut a, RequestBody::StartSeats { seats: None }).unwrap();
    assert_eq!(call(&h, &mut a, RequestBody::ResetConfig { confirm: false }).unwrap_err().code, ErrorCode::InvalidValue);
    call(&h, &mut a, RequestBody::ResetConfig { confirm: true }).unwrap();
    let cfg = h.s.store.load().unwrap();
    assert_eq!(cfg.seats.len(), 1);
    assert!(cfg.devices.is_empty());
    assert!(!h.s.secrets.has(s(2)));
    assert!(h.f.gate.rec.calls().contains(&"revert_all".to_string()));
    assert_eq!(h.s.status().product, ProductState::Disabled);
}

#[test]
fn shutdown_stops_seats_and_marks_clean() {
    let h = harness();
    let mut a = admin();
    call(&h, &mut a, RequestBody::StartSeats { seats: None }).unwrap();
    h.s.shutdown();
    assert_eq!(state(&h, s(2)), SeatState::Off);
    let run = RunStore::new(&h.dir.join("run")).load();
    assert!(!run.running);
    assert!(run.known_sessions.is_empty());
}

#[test]
fn adoption_after_restart() {
    let h = harness();
    let mut a = admin();
    call(&h, &mut a, RequestBody::StartSeats { seats: None }).unwrap();
    let run = RunStore::new(&h.dir.join("run")).load();
    assert_eq!(run.known_sessions.len(), 1);
    // Same backend instance still knows the session; a new service adopts it.
    let svc2 = Service::new(Deps {
        store: Box::new(MemoryStore::with(two_seat_config())),
        secrets: Box::new(MemorySecretStore::default()),
        run_store: RunStore::new(&h.dir.join("run")),
        platform: h.p.clone(),
        factory: h.f.clone(),
        clock: h.clock.clone(),
        spawner: Arc::new(Inline),
        data_dir: h.dir.clone(),
        console_mode: true,
        remote: None,
    });
    svc2.boot();
    let rt = svc2.status().seats.into_iter().find(|r| r.seat == s(2)).unwrap();
    assert_eq!(rt.session_id, Some(10));
    assert_eq!(rt.state, SeatState::AtLogon);
    assert!(h.f.session.rec.calls().iter().any(|c| c == "adopt 1"));
    // The surviving host keeps its pid and is still known.
    assert!(svc2.lock().seats.get(&s(2)).unwrap().host.is_some());
    let _ = h.clock.now_ms();
}

#[test]
fn poll_events_and_app_api() {
    let h = harness();
    let mut u = user(1);
    let ResponseBody::Events(b) =
        call(&h, &mut u, RequestBody::PollEvents { after: 0, wait_ms: 0, topics: vec![Topic::Status] }).unwrap()
    else {
        panic!()
    };
    assert!(b.events.iter().any(|f| matches!(f.event, Event::SeatState { .. })));
    let ResponseBody::Seat { seat, .. } = call(&h, &mut u, RequestBody::MySeat).unwrap() else { panic!() };
    assert_eq!(seat, Some(SeatNumber::CONSOLE));
    let ResponseBody::Seat { seat, .. } = call(&h, &mut u, RequestBody::SeatOfSession { session_id: 77 }).unwrap() else {
        panic!()
    };
    assert_eq!(seat, None);
}

#[test]
fn wizard_through_requests() {
    let h = harness();
    let mut a = admin();
    call(&h, &mut a, RequestBody::WizardStart { variant: seat_proto::WizardVariant::AllSeats }).unwrap();
    assert!(h.s.status().wizard_running);
    assert!(h.f.input.rec.calls().contains(&"wizard true".to_string()));
    assert_eq!(
        call(&h, &mut a, RequestBody::WizardStart { variant: seat_proto::WizardVariant::AllSeats }).unwrap_err().code,
        ErrorCode::WizardState
    );
    // More than one monitor: seat 1's monitor is picked first (spec 10 §9.2 step 2).
    let first = h.s.lock().wizard.as_ref().unwrap().monitors[0].clone();
    call(&h, &mut a, RequestBody::WizardCommand { command: seat_proto::WizardCommand::PickConsoleMonitor { key: first } }).unwrap();
    let act = |key: &str, sc: Option<u16>, click: bool| seat_core::backend::input::ActivityEvent {
        key: k(key),
        at_ms: 0,
        key_sc: sc,
        click,
    };
    for _ in 0..2 {
        h.f.input.emit_activity(act("kbd;1", Some(0x3b), false));
    }
    for _ in 0..2 {
        h.f.input.emit_activity(act("mou;1", None, true));
    }
    for _ in 0..2 {
        h.f.input.emit_activity(act("kbd;2", Some(0x3c), false));
    }
    for _ in 0..2 {
        h.f.input.emit_activity(act("mou;2", None, true));
    }
    call(&h, &mut a, RequestBody::WizardStop { save: true }).unwrap();
    assert!(!h.s.status().wizard_running);
    let cfg = h.s.store.load().unwrap();
    assert!(cfg.devices.iter().any(|d| d.key.as_str() == "kbd;1" && d.mask == SeatMask::CONSOLE));
    assert!(cfg.devices.iter().any(|d| d.key.as_str() == "mou;2" && d.mask == s(2).mask()));
}

#[test]
fn wizard_invalid_save_keeps_the_wizard_running() {
    let h = harness();
    let mut a = admin();
    call(&h, &mut a, RequestBody::WizardStart { variant: seat_proto::WizardVariant::AllSeats }).unwrap();
    let first = h.s.lock().wizard.as_ref().unwrap().monitors[0].clone();
    call(&h, &mut a, RequestBody::WizardCommand { command: seat_proto::WizardCommand::PickConsoleMonitor { key: first } }).unwrap();
    // A base config the result cannot pass validation with.
    h.s.lock().config.settings.timings.monitor_idle_off_s = 5;
    let rx = h.s.events.subscribe(vec![Topic::Wizard], Default::default());
    let act = |key: &str, sc: u16| seat_core::backend::input::ActivityEvent { key: k(key), at_ms: 0, key_sc: Some(sc), click: false };
    // Shift+Enter (key path): the save fails, the wizard stays up with capture on.
    h.f.input.emit_activity(act("kbd;2", 0x2a));
    h.f.input.emit_activity(act("kbd;2", 0x1c));
    assert!(h.s.status().wizard_running);
    assert!(!h.f.input.rec.calls().contains(&"wizard false".to_string()));
    let invalid: Vec<seat_proto::WizardProgress> = rx
        .try_iter()
        .filter_map(|f| match f.event {
            Event::WizardProgress(p) if p.step == "invalid" => Some(p),
            _ => None,
        })
        .collect();
    assert_eq!(invalid.len(), 1);
    assert!(invalid[0].issues.iter().any(|c| c == "timeout_out_of_range"), "{:?}", invalid[0].issues);
    // Request path: same error, wizard still there; fixed config then saves.
    assert_eq!(call(&h, &mut a, RequestBody::WizardStop { save: true }).unwrap_err().code, ErrorCode::InvalidConfig);
    assert!(h.s.status().wizard_running);
    h.s.lock().config.settings.timings.monitor_idle_off_s = 0;
    call(&h, &mut a, RequestBody::WizardStop { save: true }).unwrap();
    assert!(!h.s.status().wizard_running);
    assert!(h.f.input.rec.calls().contains(&"wizard false".to_string()));
}

#[test]
fn wizard_control_keys_and_target_hits_through_the_service() {
    let h = harness();
    let mut a = admin();
    call(&h, &mut a, RequestBody::WizardStart { variant: seat_proto::WizardVariant::AllSeats }).unwrap();
    let first = h.s.lock().wizard.as_ref().unwrap().monitors[0].clone();
    call(&h, &mut a, RequestBody::WizardCommand { command: seat_proto::WizardCommand::PickConsoleMonitor { key: first } }).unwrap();
    let act = |key: &str, sc: Option<u16>, click: bool| seat_core::backend::input::ActivityEvent {
        key: k(key),
        at_ms: 0,
        key_sc: sc,
        click,
    };
    // "Any key" twice on kbd;1 gives it to seat 1; a page hit steers the mouse clicks.
    h.f.input.emit_activity(act("kbd;1", Some(0x1e), false));
    h.f.input.emit_activity(act("kbd;1", Some(0x1e), false));
    call(&h, &mut a, RequestBody::WizardCommand { command: seat_proto::WizardCommand::TargetHit { seat: SeatNumber::CONSOLE } }).unwrap();
    h.f.input.emit_activity(act("mou;1", None, true));
    call(&h, &mut a, RequestBody::WizardCommand { command: seat_proto::WizardCommand::TargetHit { seat: SeatNumber::CONSOLE } }).unwrap();
    h.f.input.emit_activity(act("mou;1", None, true));
    {
        let st = h.s.lock();
        let w = st.wizard.as_ref().unwrap();
        assert_eq!(w.keyboards.get(&SeatNumber::CONSOLE), Some(&k("kbd;1")));
        assert_eq!(w.mice.get(&SeatNumber::CONSOLE), Some(&k("mou;1")));
        assert!(w.keys_seen);
    }
    // Shift, then Enter on any keyboard: save all and end the wizard.
    h.f.input.emit_activity(act("kbd;2", Some(0x2a), false));
    h.f.input.emit_activity(act("kbd;2", Some(0x1c), false));
    assert!(!h.s.status().wizard_running);
    assert!(h.f.input.rec.calls().contains(&"wizard false".to_string()));
    let cfg = h.s.store.load().unwrap();
    assert!(cfg.devices.iter().any(|d| d.key.as_str() == "mou;1" && d.mask == SeatMask::CONSOLE));
}

#[test]
fn hotkey_moves_keyboard_to_next_seat() {
    let h = harness();
    let mut a = admin();
    let rev = h.s.store.load().unwrap().revision;
    h.f.input.emit_hotkey(seat_core::backend::input::HotkeyEvent { key: k("kbd;1"), from: SeatNumber::CONSOLE });
    // Applied at once as a runtime override, not saved (spec 50 §4.1.9).
    assert_eq!(h.s.lock().input_overrides.get(&k("kbd;1")), Some(&s(2).mask()));
    assert_eq!(h.s.store.load().unwrap().revision, rev);
    call(&h, &mut a, RequestBody::ResolveInputMove { accept: true }).unwrap();
    let cfg = h.s.store.load().unwrap();
    assert!(cfg.devices.iter().any(|d| d.key.as_str() == "kbd;1" && d.mask == s(2).mask()));
    assert!(h.s.lock().input_overrides.is_empty());
}

#[test]
fn declined_hotkey_move_is_dropped() {
    let h = harness();
    let mut a = admin();
    let rev = h.s.store.load().unwrap().revision;
    h.f.input.emit_hotkey(seat_core::backend::input::HotkeyEvent { key: k("kbd;1"), from: SeatNumber::CONSOLE });
    call(&h, &mut a, RequestBody::ResolveInputMove { accept: false }).unwrap();
    assert!(h.s.lock().input_overrides.is_empty());
    assert_eq!(h.s.store.load().unwrap().revision, rev);
}

#[test]
fn tweaks_follow_config_and_product_switch() {
    let h = harness();
    let mut a = admin();
    h.s.sync_tweaks();
    let enabled = h.s.lock().config.settings.enabled;
    if enabled {
        assert!(h.p.tweaks.lock().unwrap().is_some());
    }
    call(&h, &mut a, RequestBody::SetEnabled { enabled: !enabled }).unwrap();
    // set_enabled syncs on a worker; run it directly as well (idempotent).
    h.s.sync_tweaks();
    assert_eq!(h.p.tweaks.lock().unwrap().is_some(), !enabled);
    assert!(h.p.tweak_reverts.load(std::sync::atomic::Ordering::SeqCst) >= 1);
    // Generated images exist after ensure_images.
    h.s.ensure_images();
    assert!(crate::images::lock_image_path(&h.dir).is_file());
}

/// Host console preset (OSMS parity G6): wanted only with the product enabled,
/// `dedicated_host` and `host_autologon`; never touched in a safe mode (a power loss must
/// not take the auto-logon away); the status reports it.
#[test]
fn host_console_follows_topology_preset_and_product_switch() {
    use seat_core::backend::session::HostTopology;
    let h = harness();
    h.s.lock().config.settings.enabled = true;
    h.s.sync_host_console();
    assert_eq!(h.p.host_console_calls.lock().unwrap().last(), Some(&false), "off by default");
    assert!(h.s.status().host_console.is_none());
    // The preset without dedicated_host does nothing.
    h.s.lock().config.settings.backends.host_autologon = true;
    h.s.sync_host_console();
    assert_eq!(h.p.host_console_calls.lock().unwrap().last(), Some(&false));
    h.s.lock().config.settings.backends.host_topology = HostTopology::DedicatedHost;
    h.s.sync_host_console();
    assert_eq!(h.p.host_console_calls.lock().unwrap().last(), Some(&true));
    assert_eq!(h.s.status().host_console.map(|r| r.state), Some(seat_proto::HostConsoleState::On));
    // Any safe mode: not touched at all (even with the product disabled).
    h.s.lock().config.settings.enabled = false;
    for r in [
        seat_core::state::SafeModeReason::UncleanShutdown,
        seat_core::state::SafeModeReason::CrashLoop,
        seat_core::state::SafeModeReason::WindowsSafeMode,
    ] {
        let n = h.p.host_console_calls.lock().unwrap().len();
        h.s.lock().safe_reason = Some(r);
        h.s.sync_host_console();
        assert_eq!(h.p.host_console_calls.lock().unwrap().len(), n, "{r:?}");
    }
    // A disabled product restores it.
    h.s.lock().safe_reason = None;
    h.s.sync_host_console();
    assert_eq!(h.p.host_console_calls.lock().unwrap().last(), Some(&false));
    assert!(h.s.status().host_console.is_none());
}

#[test]
fn cpu_issues_map_to_config_issues() {
    use seat_proto::CpuSetInfo;
    let mut cfg = Config::default();
    cfg.seats[0].cpu_sets = vec![999];
    let infos = vec![CpuSetInfo { id: 256, group: 0, logical_index: 0, core_index: 0, efficiency_class: 0, numa_node: 0 }];
    let i = crate::requests::cpu_issues(&cfg, &infos);
    assert!(i.iter().any(|i| i.code == seat_core::validate::IssueCode::CpuNoCore && i.severity == seat_core::validate::Severity::Error));
    assert!(crate::requests::cpu_issues(&cfg, &[]).is_empty());
}

// ---- console raw-input listener (raw_input_host, crate::listener) -------------------

const KBD1: &str = r"\\?\HID#VID_1&PID_1#kbd1#{884b96c3-56ef-11d1-bc8c-00a0c91405dd}";
const MOU1: &str = r"\\?\HID#VID_1&PID_2#mou1#{378de44c-56ef-11d1-bc8c-00a0c91405dd}";
const KBD2: &str = r"\\?\HID#VID_2&PID_1#kbd2#{884b96c3-56ef-11d1-bc8c-00a0c91405dd}";

fn input_dev(key: &str, kind: DeviceKind, path: &str) -> seat_core::device::Device {
    use seat_core::device::{DeviceDetails, InputClass, InputInfo};
    let mut d = device(key, kind);
    let class = if kind == DeviceKind::Keyboard { InputClass::Keyboard } else { InputClass::Mouse };
    d.details = DeviceDetails::Input(InputInfo {
        class,
        usage_page: 0,
        usage: 0,
        vendor_id: 0,
        product_id: 0,
        interface_path: path.into(),
        bus: "usb".into(),
        built_in: false,
        last_activity_ms: 0,
    });
    d
}

/// Harness with interface paths on the input units and the console listener wanted.
fn listener_harness() -> Harness {
    let h = harness_with(two_seat_config(), None);
    *h.f.input.devices.lock().unwrap() = vec![
        input_dev("kbd;1", DeviceKind::Keyboard, KBD1),
        input_dev("mou;1", DeviceKind::Mouse, MOU1),
        input_dev("kbd;2", DeviceKind::Keyboard, KBD2),
        device("mou;2", DeviceKind::Mouse),
    ];
    h.f.listener.store(true, Ordering::SeqCst);
    h.s.boot();
    h
}

fn listener_spec(h: &Harness) -> Option<crate::platform::LaunchSpec> {
    h.p.launched().into_iter().rev().find(|l| l.args.first().is_some_and(|a| a == "--console-listener"))
}

/// Tick until the listener runs, then hello as it; returns the verified caller.
fn connect_listener(h: &Harness) -> Caller {
    h.s.tick();
    let spec = listener_spec(h).expect("listener launched");
    let pid = h.s.lock().listener.proc.as_ref().unwrap().pid;
    let hello = RequestBody::Hello(Hello {
        client: ClientKind::ConsoleListener,
        client_version: "t".into(),
        protocol_version: 1,
        seat: None,
        token: spec.stdin_secret.clone(),
    });
    let mut c = Caller { pid, session_id: spec.session_id, is_admin: false, is_system: true, ..admin() };
    call(h, &mut c, hello).unwrap();
    assert!(c.console_listener);
    c
}

fn report(h: &Harness, c: &mut Caller, events: Vec<seat_proto::ConsoleInputEvent>) -> seat_proto::ConsoleListenerState {
    match call(h, c, RequestBody::ConsoleInputReport { events }).unwrap() {
        ResponseBody::ConsoleListenerState(st) => st,
        other => panic!("{other:?}"),
    }
}

fn act(path: &str, key_sc: Option<u16>, click: bool) -> seat_proto::ConsoleInputEvent {
    seat_proto::ConsoleInputEvent::Activity { interface: path.into(), key_sc, click }
}

#[test]
fn console_listener_is_launched_and_verified() {
    let h = listener_harness();
    h.s.tick();
    let spec = listener_spec(&h).expect("listener launched in the console session");
    assert_eq!(spec.session_id, 1);
    assert!(spec.as_system);
    assert!(spec.exe.ends_with("pulsar-seat-host.exe"));
    assert_eq!(spec.args, vec!["--console-listener".to_string(), "--token-stdin".to_string()]);
    assert!(spec.stdin_secret.is_some());
    // One listener only, also over more ticks.
    h.s.tick();
    assert_eq!(h.p.launched().iter().filter(|l| l.args[0] == "--console-listener").count(), 1);

    let pid = h.s.lock().listener.proc.as_ref().unwrap().pid;
    let hello = |tok: &str| {
        RequestBody::Hello(Hello {
            client: ClientKind::ConsoleListener,
            client_version: "t".into(),
            protocol_version: 1,
            seat: None,
            token: Some(Secret::new(tok)),
        })
    };
    let mut wrong_token = Caller { pid, ..user(1) };
    assert_eq!(call(&h, &mut wrong_token, hello("nope")).unwrap_err().code, ErrorCode::AccessDenied);
    let mut wrong_pid = Caller { pid: pid + 77, ..user(1) };
    assert_eq!(call(&h, &mut wrong_pid, hello(spec.stdin_secret.as_ref().unwrap().expose())).unwrap_err().code, ErrorCode::AccessDenied);
    let mut wrong_session = Caller { pid, ..user(5) };
    assert_eq!(call(&h, &mut wrong_session, hello(spec.stdin_secret.as_ref().unwrap().expose())).unwrap_err().code, ErrorCode::AccessDenied);
    // A manager (even an admin) cannot send reports.
    let mut a = admin();
    assert_eq!(call(&h, &mut a, RequestBody::ConsoleInputReport { events: vec![] }).unwrap_err().code, ErrorCode::AccessDenied);

    let mut c = connect_listener(&h);
    let st = report(&h, &mut c, vec![]);
    assert!(!st.stop && !st.wizard_running);
    assert_eq!(st.input.switch_hotkey_scan, Some(0x58));
    assert!(h.s.lock().listener.reporting);
}

#[test]
fn console_listener_not_wanted_without_the_factory_flag() {
    let h = harness();
    h.s.tick();
    assert!(listener_spec(&h).is_none());
}

#[test]
fn console_activity_reaches_the_idle_saver_and_events() {
    let h = listener_harness();
    let mut c = connect_listener(&h);
    let before = h.s.events.poll(0, &[Topic::Activity], Default::default(), std::time::Duration::ZERO).last_seq;
    h.clock.advance(1234);
    report(&h, &mut c, vec![act(KBD1, None, false), act(&KBD2.to_ascii_lowercase(), None, false), act(r"\\?\unknown", None, true)]);
    let st = h.s.lock();
    assert_eq!(st.last_input_ms.get(&SeatNumber::CONSOLE), Some(&1234));
    assert_eq!(st.last_input_ms.get(&s(2)), Some(&1234), "interface paths match case-insensitively");
    drop(st);
    let batch = h.s.events.poll(before, &[Topic::Activity], Default::default(), std::time::Duration::ZERO);
    let keys: Vec<String> = batch
        .events
        .iter()
        .filter_map(|e| match &e.event {
            Event::DeviceActivity { key } => Some(key.to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(keys, vec!["kbd;1".to_string(), "kbd;2".to_string()], "unknown units are ignored");
}

#[test]
fn console_hotkey_moves_unit_and_host_reads_it() {
    let h = listener_harness();
    let mut c = connect_listener(&h);
    let rev = h.s.store.load().unwrap().revision;
    report(&h, &mut c, vec![seat_proto::ConsoleInputEvent::Hotkey { interface: KBD1.into() }]);
    assert_eq!(h.s.lock().input_overrides.get(&k("kbd;1")), Some(&s(2).mask()));
    assert_eq!(h.s.store.load().unwrap().revision, rev, "runtime override only until confirmed");

    // Seat 2's host gets the moved keyboard's interface at once.
    let mut a = admin();
    call(&h, &mut a, RequestBody::StartSeats { seats: None }).unwrap();
    let host_spec = h.p.launched().into_iter().find(|l| l.args.first().is_some_and(|x| x == "--seat")).unwrap();
    let pid = h.s.lock().seats.get(&s(2)).unwrap().host.as_ref().unwrap().pid;
    let mut host = Caller { pid, ..user(1) };
    call(
        &h,
        &mut host,
        RequestBody::Hello(Hello {
            client: ClientKind::SeatHost,
            client_version: "t".into(),
            protocol_version: 1,
            seat: Some(s(2)),
            token: host_spec.stdin_secret.clone(),
        }),
    )
    .unwrap();
    let ResponseBody::HostAssignment(asg) = call(&h, &mut host, RequestBody::HostGetAssignment { seat: s(2) }).unwrap() else {
        panic!()
    };
    assert!(asg.input_interfaces.contains(&KBD1.to_string()), "{:?}", asg.input_interfaces);
    assert!(asg.input_interfaces.contains(&KBD2.to_string()));

    // From seat 2 the next press goes round to seat 1.
    report(&h, &mut c, vec![seat_proto::ConsoleInputEvent::Hotkey { interface: KBD1.into() }]);
    assert_eq!(h.s.lock().input_overrides.get(&k("kbd;1")), Some(&SeatMask::CONSOLE));
}

#[test]
fn console_hotkey_off_still_allows_the_emergency() {
    let h = listener_harness();
    let mut c = connect_listener(&h);
    // Hotkey off: the hotkey does nothing, the emergency combination still works.
    {
        let mut st = h.s.lock();
        st.config.settings.input.switch_hotkey_scan = None;
    }
    let rev = h.s.store.load().unwrap().revision;
    let st = report(&h, &mut c, vec![seat_proto::ConsoleInputEvent::Hotkey { interface: KBD2.into() }]);
    assert_eq!(st.input.switch_hotkey_scan, None);
    assert!(h.s.lock().input_overrides.is_empty());
    assert_eq!(h.s.store.load().unwrap().revision, rev);
    report(&h, &mut c, vec![seat_proto::ConsoleInputEvent::Emergency { interface: KBD2.into() }]);
    let cfg = h.s.store.load().unwrap();
    assert!(cfg.revision > rev);
    for key in ["kbd;2", "mou;2"] {
        assert!(cfg.devices.iter().any(|d| d.key.as_str() == key && d.mask == SeatMask::CONSOLE), "{key}");
    }
}

#[test]
fn console_emergency_drops_pending_hotkey_moves() {
    let h = listener_harness();
    let mut c = connect_listener(&h);
    // Ctrl+F12 puts keyboard 1 on seat 2 (pending), then the emergency combination.
    report(&h, &mut c, vec![seat_proto::ConsoleInputEvent::Hotkey { interface: KBD1.into() }]);
    assert_eq!(h.s.lock().input_overrides.get(&k("kbd;1")), Some(&s(2).mask()));
    report(&h, &mut c, vec![seat_proto::ConsoleInputEvent::Emergency { interface: KBD1.into() }]);
    assert!(h.s.lock().input_overrides.is_empty(), "the override would win over the saved seat 1");
    assert_eq!(h.s.unit_seat(&k("kbd;1")), SeatNumber::CONSOLE);
    assert_eq!(h.s.unit_seat(&k("kbd;2")), SeatNumber::CONSOLE);
    // Seat 2's host is not handed keyboard 1 (nor its own former keyboard).
    let mut a = admin();
    call(&h, &mut a, RequestBody::StartSeats { seats: None }).unwrap();
    if let Some(host_spec) = h.p.launched().into_iter().find(|l| l.args.first().is_some_and(|x| x == "--seat")) {
        let pid = h.s.lock().seats.get(&s(2)).unwrap().host.as_ref().unwrap().pid;
        let mut host = Caller { pid, ..user(1) };
        call(
            &h,
            &mut host,
            RequestBody::Hello(Hello {
                client: ClientKind::SeatHost,
                client_version: "t".into(),
                protocol_version: 1,
                seat: Some(s(2)),
                token: host_spec.stdin_secret.clone(),
            }),
        )
        .unwrap();
        let ResponseBody::HostAssignment(asg) = call(&h, &mut host, RequestBody::HostGetAssignment { seat: s(2) }).unwrap() else {
            panic!()
        };
        assert!(!asg.input_interfaces.contains(&KBD1.to_string()), "{:?}", asg.input_interfaces);
        assert!(!asg.input_interfaces.contains(&KBD2.to_string()), "{:?}", asg.input_interfaces);
    }
}

#[test]
fn console_hotkey_moves_hid_on_the_same_parent_and_seat_mice() {
    let h = harness_with(two_seat_config(), None);
    let hub = Some(k("hub;2"));
    let mut kbd2 = input_dev("kbd;2", DeviceKind::Keyboard, KBD2);
    kbd2.parent_hub = hub.clone();
    let mut hid2 = device("hid;2", DeviceKind::Hid);
    hid2.parent_hub = hub.clone();
    // mou;2 is on another hub but assigned to seat 2; mou;1 is unassigned (seat 1).
    *h.f.input.devices.lock().unwrap() = vec![
        input_dev("kbd;1", DeviceKind::Keyboard, KBD1),
        input_dev("mou;1", DeviceKind::Mouse, MOU1),
        kbd2,
        hid2,
        device("mou;2", DeviceKind::Mouse),
    ];
    h.f.listener.store(true, Ordering::SeqCst);
    h.s.boot();
    let mut c = connect_listener(&h);
    report(&h, &mut c, vec![seat_proto::ConsoleInputEvent::Hotkey { interface: KBD2.into() }]);
    let st = h.s.lock();
    // Two seats: from seat 2 the next is seat 1.
    for key in ["kbd;2", "hid;2", "mou;2"] {
        assert_eq!(st.input_overrides.get(&k(key)), Some(&SeatMask::CONSOLE), "{key}");
    }
    assert!(!st.input_overrides.contains_key(&k("mou;1")));
    drop(st);
    // From seat 1 only same-parent units follow: the other seat-1 mice stay.
    h.s.lock().input_overrides.clear();
    report(&h, &mut c, vec![seat_proto::ConsoleInputEvent::Hotkey { interface: KBD1.into() }]);
    let st = h.s.lock();
    assert_eq!(st.input_overrides.get(&k("kbd;1")), Some(&s(2).mask()));
    assert!(!st.input_overrides.contains_key(&k("mou;1")));
}

#[test]
fn console_hotkey_is_ignored_and_emergency_ends_the_wizard() {
    let h = listener_harness();
    let mut c = connect_listener(&h);
    let mut a = admin();
    call(&h, &mut a, RequestBody::WizardStart { variant: seat_proto::WizardVariant::AllSeats }).unwrap();
    report(&h, &mut c, vec![seat_proto::ConsoleInputEvent::Hotkey { interface: KBD1.into() }]);
    assert!(h.s.lock().input_overrides.is_empty(), "no route change under the wizard");
    assert!(h.s.lock().wizard.is_some());
    let rev = h.s.store.load().unwrap().revision;
    let st = report(&h, &mut c, vec![seat_proto::ConsoleInputEvent::Emergency { interface: KBD1.into() }]);
    assert!(h.s.lock().wizard.is_none(), "the emergency cancels the wizard");
    assert!(!st.wizard_running);
    assert!(h.s.store.load().unwrap().revision > rev);
}

#[test]
fn console_wizard_never_asks_for_every_key() {
    // Maintainer decision 2026-11-13: only the wizard keys, in every step.
    let h = listener_harness();
    let mut c = connect_listener(&h);
    let st = report(&h, &mut c, vec![]);
    assert!(!st.wizard_running && !st.wizard_all_keys, "no wizard: no keys at all");
    let mut a = admin();
    call(&h, &mut a, RequestBody::WizardStart { variant: seat_proto::WizardVariant::AllSeats }).unwrap();
    let st = report(&h, &mut c, vec![]);
    assert!(st.wizard_running && !st.wizard_all_keys);
    // Seat 1's monitor picked with the digit 1 from the listener.
    let st = report(&h, &mut c, vec![act(KBD1, Some(0x02), false)]);
    assert!(!st.wizard_all_keys);
    assert!(h.s.lock().wizard.as_ref().unwrap().progress().step != "pick_console_monitor");
    // "Press any key" works with a digit (seat next in line) as well as an F-key.
    report(&h, &mut c, vec![act(KBD1, Some(0x03), false), act(KBD1, Some(0x03), false)]);
    report(&h, &mut c, vec![act(KBD2, Some(0x3c), false), act(KBD2, Some(0x3c), false)]);
    let st = report(&h, &mut c, vec![]);
    assert!(st.wizard_running && !st.wizard_all_keys);
    assert!(h.s.lock().wizard.as_ref().unwrap().progress().assigned.len() >= 2, "both keyboards assigned");
    call(&h, &mut a, RequestBody::WizardStop { save: false }).unwrap();
}

#[test]
fn console_wizard_keys_and_clicks_assign_units() {
    let h = listener_harness();
    let mut c = connect_listener(&h);
    let mut a = admin();
    call(&h, &mut a, RequestBody::WizardStart { variant: seat_proto::WizardVariant::AllSeats }).unwrap();
    // More than one monitor: seat 1's monitor is picked first (spec 10 §9.2 step 2).
    let first = h.s.lock().wizard.as_ref().unwrap().monitors[0].clone();
    call(&h, &mut a, RequestBody::WizardCommand { command: seat_proto::WizardCommand::PickConsoleMonitor { key: first } }).unwrap();
    let st = report(&h, &mut c, vec![]);
    assert!(st.wizard_running, "the listener is told to report F-keys");
    report(&h, &mut c, vec![act(KBD1, Some(0x3b), false), act(KBD1, Some(0x3b), false)]);
    report(&h, &mut c, vec![act(MOU1, None, true), act(MOU1, None, true)]);
    report(&h, &mut c, vec![act(KBD2, Some(0x3c), false), act(KBD2, Some(0x3c), false)]);
    {
        let st = h.s.lock();
        let w = st.wizard.as_ref().unwrap();
        assert_eq!(w.keyboards.get(&SeatNumber::CONSOLE), Some(&k("kbd;1")));
        assert_eq!(w.mice.get(&SeatNumber::CONSOLE), Some(&k("mou;1")));
        assert_eq!(w.keyboards.get(&s(2)), Some(&k("kbd;2")));
    }
    call(&h, &mut a, RequestBody::WizardStop { save: false }).unwrap();
    assert!(!report(&h, &mut c, vec![]).wizard_running);
}

#[test]
fn console_listener_stops_and_restarts() {
    let h = listener_harness();
    let mut c = connect_listener(&h);
    report(&h, &mut c, vec![]);
    let first = h.s.lock().listener.proc.as_ref().unwrap().pid;

    // Silent for too long: terminated, relaunched after the back-off.
    h.clock.advance(seat_proto::CONSOLE_LISTENER_TIMEOUT_MS * 2 + 1);
    h.s.tick();
    assert!(!crate::platform::Platform::process_alive(&*h.p, first));
    assert!(!h.s.lock().listener.reporting);
    let second = h.s.lock().listener.proc.as_ref().map(|p| p.pid).expect("relaunched (back-off passed)");
    assert_ne!(first, second);
    // The old listener's reports are answered with stop.
    assert!(report(&h, &mut c, vec![act(KBD1, None, false)]).stop);

    // Died: relaunched only after the back-off.
    h.p.kill(second);
    h.s.tick();
    assert!(h.s.lock().listener.proc.is_none());
    h.clock.advance(crate::listener::LISTENER_RELAUNCH_MS);
    h.s.tick();
    let third = h.s.lock().listener.proc.as_ref().map(|p| p.pid).expect("relaunched");

    // Product off: the listener is stopped and not relaunched.
    h.s.lock().config.settings.enabled = false;
    h.clock.advance(crate::listener::LISTENER_RELAUNCH_MS);
    h.s.tick();
    assert!(!crate::platform::Platform::process_alive(&*h.p, third));
    assert!(h.s.lock().listener.proc.is_none());
}

/// Seat logon: the agent and the seat's startup apps go into the seat job with the seat's
/// CPU limits and the source-IP variable; apps start once per logon session; relative app
/// paths are refused; hosts never get a job (spec 80 §11.2, §11.3).
#[test]
fn seat_job_agent_and_startup_apps() {
    let mut c = two_seat_config();
    c.seats[1].cpu_sets = vec![256, 257];
    c.seats[1].cpu_hard_limit = true;
    c.seats[1].startup_apps = vec![
        seat_core::config::KioskApp { command: r"C:\Games\Lan\game.exe".into(), args: vec!["-lan".into()] },
        seat_core::config::KioskApp { command: "notepad.exe".into(), args: vec![] },
    ];
    c.seats[1].ip_binding = Some(seat_core::config::IpBinding { address: "192.168.1.52".into(), rule: seat_core::config::IpRule::AllPrograms });
    let h = harness_with(c, None);
    h.s.boot();
    let mut a = admin();
    call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(2)]) }).unwrap();
    h.p.sign_in(10, "ali");
    h.s.on_session_change(SessionChange::Logon, 10);
    let l = h.p.launched();
    let want = crate::platform::SeatJobSpec { cpu_sets: vec![256, 257], hard_limit: true };
    let agent = l.iter().find(|x| x.exe.ends_with("pulsar-seat-agent.exe")).unwrap();
    assert_eq!(agent.job.as_ref(), Some(&want));
    // The agent never carries the source-IP variable (its children would inherit it).
    assert!(!agent.env.iter().any(|(n, _)| n == "PULSAR_SEAT_SOURCE_IP"));
    let apps: Vec<_> = l.iter().filter(|x| x.exe.ends_with("game.exe")).collect();
    assert_eq!(apps.len(), 1);
    assert!(apps[0].env.iter().any(|(n, v)| n == "PULSAR_SEAT_SOURCE_IP" && v == "192.168.1.52"));
    assert_eq!(apps[0].args, vec!["-lan".to_string()]);
    assert_eq!(apps[0].job.as_ref(), Some(&want));
    assert!(!l.iter().any(|x| x.exe.ends_with("notepad.exe")), "relative path refused");
    assert!(l.iter().filter(|x| x.exe.ends_with("pulsar-seat-host.exe")).all(|x| x.job.is_none()));
    // The same logon reported again: no second copy of the app.
    h.s.on_session_change(SessionChange::Logon, 10);
    assert_eq!(h.p.launched().iter().filter(|x| x.exe.ends_with("game.exe")).count(), 1);
}

/// The class-default switch reaches the gates as the class list (spec 70 §5.2(2)).
#[test]
fn class_defaults_reach_the_gate_plan() {
    let mut c = two_seat_config();
    c.settings.devices.class_default_descriptors = true;
    let h = harness_with(c, None);
    h.s.boot();
    h.s.tick();
    let plan = h.f.gate.last_plan.lock().unwrap().clone().unwrap();
    assert_eq!(plan.class_defaults, seat_core::backend::gate::DeviceClassTarget::DEFAULT.to_vec());
}

/// Every logoff / session end drops the session's seat job (the console seat's too):
/// session ids are reused and a hard limit cannot be cleared from a job. The next logon
/// in the session starts the startup apps again.
#[test]
fn session_jobs_dropped_on_every_logoff() {
    let mut c = two_seat_config();
    c.seats[1].startup_apps = vec![seat_core::config::KioskApp { command: r"C:\Games\Lan\game.exe".into(), args: vec![] }];
    let h = harness_with(c, None);
    h.s.boot();
    let mut a = admin();
    call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(2)]) }).unwrap();
    h.p.sign_in(10, "ali");
    h.s.on_session_change(SessionChange::Logon, 10);
    let games = |h: &Harness| h.p.launched().iter().filter(|x| x.exe.ends_with("game.exe")).count();
    assert_eq!(games(&h), 1);
    h.s.on_session_change(SessionChange::Logoff, 10);
    assert!(h.p.dropped_jobs.lock().unwrap().contains(&10));
    h.p.sign_in(10, "ali");
    h.s.on_session_change(SessionChange::Logon, 10);
    assert_eq!(games(&h), 2, "a new logon starts the startup apps again");
    // Console seat (early-return path).
    h.s.on_session_change(SessionChange::Logoff, 1);
    assert!(h.p.dropped_jobs.lock().unwrap().contains(&1));
    h.s.on_session_change(SessionChange::Terminate, 33);
    assert!(h.p.dropped_jobs.lock().unwrap().contains(&33));
}

/// A changed cpu_sets / cpu_hard_limit reaches the running seat's job at once.
#[test]
fn cpu_limit_change_reaches_the_running_seat_job() {
    let h = harness();
    let mut a = admin();
    call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(2)]) }).unwrap();
    h.p.sign_in(10, "ali");
    h.s.on_session_change(SessionChange::Logon, 10);
    let mut cfg = h.s.lock().config.clone();
    // Unrelated change: no job update.
    cfg.seats[1].wallpaper = !cfg.seats[1].wallpaper;
    {
        let mut st = h.s.lock();
        h.s.commit_config(&mut st, cfg.clone()).unwrap();
    }
    assert!(h.p.job_updates.lock().unwrap().is_empty());
    cfg.seats[1].cpu_sets = vec![256];
    cfg.seats[1].cpu_hard_limit = true;
    {
        let mut st = h.s.lock();
        h.s.commit_config(&mut st, cfg).unwrap();
    }
    let want = crate::platform::SeatJobSpec { cpu_sets: vec![256], hard_limit: true };
    assert_eq!(h.p.job_updates.lock().unwrap().as_slice(), [(10, want)]);
}

/// Launcher request: the caller's own seat, its user only, absolute paths, seat job and
/// source-IP variable.
#[test]
fn start_seat_app_runs_in_the_callers_seat_job() {
    let mut c = two_seat_config();
    c.seats[1].cpu_sets = vec![256, 257];
    c.seats[1].ip_binding = Some(seat_core::config::IpBinding { address: "192.168.1.52".into(), rule: seat_core::config::IpRule::AllPrograms });
    let h = harness_with(c, None);
    h.s.boot();
    let mut a = admin();
    call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(2)]) }).unwrap();
    h.p.sign_in(10, "ali");
    h.s.on_session_change(SessionChange::Logon, 10);
    let start = |c: &mut Caller, path: &str| {
        call(&h, c, RequestBody::StartSeatApp { path: path.into(), args: vec!["-lan".into()] })
    };
    let mut ali = Caller { user_sid: Some("S-1-5-21-9-ali".into()), ..user(10) };
    assert_eq!(start(&mut ali, "game.exe").unwrap_err().code, ErrorCode::InvalidValue);
    assert_eq!(start(&mut ali, r"\\server\share\game.exe").unwrap_err().code, ErrorCode::InvalidValue);
    // Another account's process in the same session.
    let mut other = Caller { user_sid: Some("S-1-5-21-9-veli".into()), ..user(10) };
    assert_eq!(start(&mut other, r"C:\Games\game.exe").unwrap_err().code, ErrorCode::AccessDenied);
    // Not a seat session.
    let mut stray = Caller { user_sid: Some("S-1-5-21-9-ali".into()), ..user(77) };
    assert_eq!(start(&mut stray, r"C:\Games\game.exe").unwrap_err().code, ErrorCode::AccessDenied);
    let r = start(&mut ali, r"C:\Games\game.exe").unwrap();
    assert!(matches!(r, ResponseBody::AppStarted { .. }));
    let l = h.p.launched();
    let app = l.iter().rev().find(|x| x.exe.ends_with("game.exe")).unwrap();
    assert_eq!(app.session_id, 10);
    assert!(!app.as_system);
    assert_eq!(app.args, vec!["-lan".to_string()]);
    assert_eq!(app.job.as_ref(), Some(&crate::platform::SeatJobSpec { cpu_sets: vec![256, 257], hard_limit: false }));
    assert!(app.env.iter().any(|(n, v)| n == "PULSAR_SEAT_SOURCE_IP" && v == "192.168.1.52"));
}

/// A gate leaving the active set is reverted (spec 70 §5.2(2), §5.2(5)).
#[test]
fn deselected_gate_is_reverted() {
    let mut c = two_seat_config();
    c.settings.backends.gates = vec![seat_core::backend::gate::GateBackendKind::DeviceSecurity];
    let h = harness_with(c, None);
    h.s.boot();
    let reverts = |h: &Harness| h.f.gate.rec.calls().iter().filter(|c| *c == "revert_all").count();
    let before = reverts(&h);
    // Same set again: nothing reverted.
    h.s.reselect_backends();
    assert_eq!(reverts(&h), before);
    h.s.lock().config.settings.backends.gates.clear();
    h.s.reselect_backends();
    assert_eq!(reverts(&h), before + 1);
}

/// `fast_user_switch` (not concurrent): no autostart at boot or after the console logon,
/// a manual start still works.
#[test]
fn no_autostart_on_non_concurrent_backend() {
    for mode in [seat_core::config::StartMode::AtBoot, seat_core::config::StartMode::AfterSeatOneLogon] {
        let mut c = two_seat_config();
        c.settings.start_mode = mode;
        let h = harness_with(c, None);
        *h.f.session.availability.lock().unwrap() = Availability::Unavailable { reason: "edition_unsupported".into() };
        h.s.reselect_backends();
        h.s.boot();

        assert_eq!(h.s.status().active.session, seat_core::backend::SessionBackendKind::FastUserSwitch);
        h.s.on_session_change(SessionChange::Logon, 1);
        assert!(!h.f.fus.rec.calls().iter().any(|c| c.starts_with("start")), "{mode:?}: {:?}", h.f.fus.rec.calls());
        assert_eq!(state(&h, s(2)), SeatState::Off, "{mode:?}");
        let mut a = admin();
        call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(2)]) }).unwrap();
        assert!(h.f.fus.rec.calls().iter().any(|c| c == "start 2"), "{:?}", h.f.fus.rec.calls());
    }
}

/// `fast_user_switch`: when a seat's session becomes the console, its agent gets the
/// seat's monitors as `apply_topology` (spec 35 §5.6).
#[test]
fn switch_queues_seat_topology_for_the_agent() {
    let h = harness_with(two_seat_config(), None);
    let mut mon2 = device("mon;2", DeviceKind::Monitor);
    mon2.details = seat_core::device::DeviceDetails::Display(seat_core::device::DisplayInfo {
        adapter_luid: 77,
        target_id: 4,
        ..Default::default()
    });
    // Console-only display under the fallback: monitors come from the baseline PnP scan.
    *h.f.display.devices.lock().unwrap() = vec![device("mon;1", DeviceKind::Monitor), mon2.clone()];
    *h.p.devices.lock().unwrap() = vec![mon2];
    *h.f.session.availability.lock().unwrap() = Availability::Unavailable { reason: "edition_unsupported".into() };
    h.s.reselect_backends();
    h.s.boot();
    let mut a = admin();
    call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(2)]) }).unwrap();
    let session = h.s.status().seats.into_iter().find(|r| r.seat == s(2)).and_then(|r| r.session_id).unwrap();
    h.s.on_session_change(SessionChange::ConsoleConnect, session);
    let tasks = h.s.lock().agent_tasks.get(&session).cloned().unwrap_or_default();
    let topo: Vec<_> = tasks.iter().filter_map(|t| match &t.task {
        AgentTaskKind::ApplyTopology { targets } => Some(targets.clone()),
        _ => None,
    }).collect();
    assert_eq!(topo, vec![vec![seat_proto::DisplayTargetRef { adapter_luid: 77, target_id: 4 }]]);
}

/// `force_assign_at_start`: each seat start applies the console topology again.
#[test]
fn force_assign_reapplies_topology_at_seat_start() {
    for force in [false, true] {
        let mut c = two_seat_config();
        c.settings.outputs.force_assign_at_start = force;
        let h = harness_with(c, None);
        h.s.boot();
        let count = || h.f.display.rec.calls().iter().filter(|c| c.starts_with("apply_topology")).count();
        let before = count();
        let mut a = admin();
        call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(2)]) }).unwrap();
        assert_eq!(count() > before, force, "force={force}: {:?}", h.f.display.rec.calls());
    }
}

// ---- credential provider (fast_user_switch, spec 35 §5.4) -----------------------------

/// Harness on the `fast_user_switch` fallback with seat 2's password stored.
fn fus_harness() -> Harness {
    let h = harness_with(two_seat_config(), None);
    *h.f.session.availability.lock().unwrap() = Availability::Unavailable { reason: "edition_unsupported".into() };
    h.s.reselect_backends();
    h.s.boot();
    let mut a = admin();
    call(&h, &mut a, RequestBody::SetSeatPassword { seat: s(2), password: Some(Secret::new("pw2")) }).unwrap();
    h
}

const LOGONUI: &str = r"C:\Windows\System32\LogonUI.exe";

/// A LogonUI process in `session` with the Pulsar provider loaded (not yet said hello).
fn logonui(h: &Harness, pid: u32, session: u32) -> Caller {
    h.p.images.lock().unwrap().insert(pid, LOGONUI.into());
    h.p.pid_session.lock().unwrap().insert(pid, session);
    Caller {
        pid,
        session_id: session,
        user_sid: Some("S-1-5-18".into()),
        is_admin: true,
        is_system: true,
        client: None,
        host_seat: None,
        agent_session: None,
        console_listener: false,
        credprov_session: None,
        kiosk_seat: None,
        // Overwritten by `hello`; callers built with a client kind count as current.
        protocol: seat_proto::PROTOCOL_VERSION,
    }
}

fn credprov_hello(h: &Harness, c: &mut Caller) -> Result<ResponseBody, seat_proto::ProtoError> {
    call(h, c, hello_body(ClientKind::CredentialProvider))
}

fn seat_session_id(h: &Harness, seat: SeatNumber) -> u32 {
    h.s.status().seats.into_iter().find(|r| r.seat == seat).and_then(|r| r.session_id).unwrap()
}

fn query(h: &Harness, c: &mut Caller) -> seat_proto::CredprovSession {
    let ResponseBody::CredprovSession(q) =
        call(h, c, RequestBody::CredprovQuery { scenario: seat_proto::CredprovScenario::Logon }).unwrap()
    else {
        panic!()
    };
    q
}

#[test]
fn credprov_registered_only_enabled_under_fast_user_switch() {
    // Default backend (RDS loopback mock available): never registered.
    let h = harness();
    assert!(!h.p.credprov.load(Ordering::SeqCst));
    let h = fus_harness();
    assert!(h.p.credprov.load(Ordering::SeqCst));
    let mut a = admin();
    call(&h, &mut a, RequestBody::SetEnabled { enabled: false }).unwrap();
    assert!(!h.p.credprov.load(Ordering::SeqCst), "disable removes the registration");
    call(&h, &mut a, RequestBody::SetEnabled { enabled: true }).unwrap();
    assert!(h.p.credprov.load(Ordering::SeqCst));
    // Back on an RDP backend: removed again.
    *h.f.session.availability.lock().unwrap() = Availability::Available;
    h.s.reselect_backends();
    assert!(!h.p.credprov.load(Ordering::SeqCst));
}

#[test]
fn credprov_hello_needs_system_logonui_in_its_session() {
    let h = fus_harness();
    // A user process claiming to be the provider.
    let mut u = user(3);
    assert_eq!(credprov_hello(&h, &mut u).unwrap_err().code, ErrorCode::AccessDenied);
    // SYSTEM, but not LogonUI.
    let mut c = logonui(&h, 500, 3);
    h.p.images.lock().unwrap().insert(500, r"C:\Windows\System32\cmd.exe".into());
    assert_eq!(credprov_hello(&h, &mut c).unwrap_err().code, ErrorCode::AccessDenied);
    // LogonUI of another session than the token's.
    let mut c = logonui(&h, 501, 3);
    h.p.pid_session.lock().unwrap().insert(501, 4);
    assert_eq!(credprov_hello(&h, &mut c).unwrap_err().code, ErrorCode::AccessDenied);
    // An unverified caller gets nothing from the credprov requests.
    let mut m = user(3);
    assert_eq!(call(&h, &mut m, RequestBody::CredprovTakeCredentials).unwrap_err().code, ErrorCode::AccessDenied);
    // The real one.
    let mut c = logonui(&h, 502, 3);
    credprov_hello(&h, &mut c).unwrap();
    assert_eq!(c.credprov_session, Some(3));
}

#[test]
fn credprov_one_time_credential_for_the_fresh_seat_session() {
    let h = fus_harness();
    let mut a = admin();
    call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(2)]) }).unwrap();
    let sess = seat_session_id(&h, s(2));
    // LogonUI of the console seat's session: not a grant session, nothing hidden.
    let mut other = logonui(&h, 600, 1);
    credprov_hello(&h, &mut other).unwrap();
    let q = query(&h, &mut other);
    assert!(!q.auto_submit && !q.hide_others);
    assert!(call(&h, &mut other, RequestBody::CredprovTakeCredentials).is_err());
    // Seat 2's LogonUI.
    let mut c = logonui(&h, 601, sess);
    credprov_hello(&h, &mut c).unwrap();
    let q = query(&h, &mut c);
    assert_eq!(q.seat, Some(s(2)));
    assert!(q.auto_submit && q.hide_others);
    assert!(q.account.as_deref().is_some_and(|a| a.ends_with("ali")), "{q:?}");
    // The lock screen never auto-submits.
    let ResponseBody::CredprovSession(lock) =
        call(&h, &mut c, RequestBody::CredprovQuery { scenario: seat_proto::CredprovScenario::Unlock }).unwrap()
    else {
        panic!()
    };
    assert!(!lock.auto_submit && !lock.hide_others);
    let ResponseBody::Credentials(cr) = call(&h, &mut c, RequestBody::CredprovTakeCredentials).unwrap() else { panic!() };
    assert_eq!(cr.user, "ali");
    assert_eq!(cr.password.as_ref().map(|p| p.expose().to_string()), Some("pw2".into()));
    // Once only, and the filter stops hiding once it was taken.
    let again = call(&h, &mut c, RequestBody::CredprovTakeCredentials).unwrap_err();
    assert_eq!(again.reason.as_deref(), Some("credentials_taken"));
    assert!(!query(&h, &mut c).hide_others);
    // Success report: the grant is gone.
    call(&h, &mut c, RequestBody::CredprovReport { status: 0, sub_status: 0 }).unwrap();
    assert!(h.s.lock().credprov.grants.is_empty());
}

#[test]
fn credprov_failed_logon_marks_password_failing_and_grant_expires() {
    let h = fus_harness();
    let mut a = admin();
    call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(2)]) }).unwrap();
    let sess = seat_session_id(&h, s(2));
    let mut c = logonui(&h, 700, sess);
    credprov_hello(&h, &mut c).unwrap();
    call(&h, &mut c, RequestBody::CredprovTakeCredentials).unwrap();
    // STATUS_LOGON_FAILURE
    call(&h, &mut c, RequestBody::CredprovReport { status: 0xC000_006Du32 as i32, sub_status: 0 }).unwrap();
    let ResponseBody::Config(v) = call(&h, &mut a, RequestBody::GetConfig).unwrap() else { panic!() };
    assert_eq!(v.password_failing, vec![s(2)]);
    // No new grant for a failing password.
    h.s.maybe_arm_credprov(s(2), sess, true);
    assert!(h.s.lock().credprov.grants.is_empty());
    // A fresh password: armed again, but only for the TTL.
    call(&h, &mut a, RequestBody::SetSeatPassword { seat: s(2), password: Some(Secret::new("pw3")) }).unwrap();
    h.s.maybe_arm_credprov(s(2), sess, true);
    assert!(query(&h, &mut c).auto_submit);
    h.clock.advance(crate::credprov::GRANT_TTL_MS + 1);
    assert!(!query(&h, &mut c).auto_submit);
    assert_eq!(call(&h, &mut c, RequestBody::CredprovTakeCredentials).unwrap_err().reason.as_deref(), Some("no_grant"));
}

#[test]
fn credprov_grant_dropped_on_logon_and_stop() {
    let h = fus_harness();
    let mut a = admin();
    call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(2)]) }).unwrap();
    let sess = seat_session_id(&h, s(2));
    assert_eq!(h.s.lock().credprov.grants.len(), 1);
    h.s.on_session_change(SessionChange::Logon, sess);
    assert!(h.s.lock().credprov.grants.is_empty());
    h.s.maybe_arm_credprov(s(2), sess, true);
    call(&h, &mut a, RequestBody::StopSeat { seat: s(2) }).unwrap();
    assert!(h.s.lock().credprov.grants.is_empty());
    // Without a stored password nothing is armed.
    call(&h, &mut a, RequestBody::SetSeatPassword { seat: s(2), password: None }).unwrap();
    h.s.maybe_arm_credprov(s(2), sess, true);
    assert!(h.s.lock().credprov.grants.is_empty());
}

#[test]
fn credprov_query_before_arm_is_pending_until_the_grant() {
    let h = fus_harness();
    let mut a = admin();
    call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(2)]) }).unwrap();
    let sess = seat_session_id(&h, s(2));
    assert!(h.s.lock().credprov.handover.is_none(), "arming ends the hand-over");
    // Replay the race: the hand-over is noted, the console moved to the fresh session,
    // LogonUI there asks before the grant is armed.
    h.s.lock().credprov.grants.clear();
    *h.p.console.lock().unwrap() = Some(1);
    h.s.note_credprov_handover(s(2));
    assert!(h.s.lock().credprov.handover.is_some());
    *h.p.console.lock().unwrap() = Some(sess);
    let mut c = logonui(&h, 900, sess);
    credprov_hello(&h, &mut c).unwrap();
    let q = query(&h, &mut c);
    assert!(q.pending && !q.auto_submit && !q.hide_others, "{q:?}");
    // The lock screen and a session off the console are never pending.
    let ResponseBody::CredprovSession(lock) =
        call(&h, &mut c, RequestBody::CredprovQuery { scenario: seat_proto::CredprovScenario::Unlock }).unwrap()
    else {
        panic!()
    };
    assert!(!lock.pending);
    let mut old = logonui(&h, 901, 1);
    credprov_hello(&h, &mut old).unwrap();
    assert!(!query(&h, &mut old).pending, "the session the console came from");
    // The grant arrives: auto-submit, no longer pending.
    h.s.maybe_arm_credprov(s(2), sess, true);
    let q = query(&h, &mut c);
    assert!(q.auto_submit && q.hide_others && !q.pending, "{q:?}");
    // A hand-over that never gets its grant stops being pending after HANDOVER_MS.
    h.s.lock().credprov.grants.clear();
    h.s.note_credprov_handover(s(2));
    h.s.lock().credprov.handover.as_mut().unwrap().from_session = Some(1);
    assert!(query(&h, &mut c).pending);
    h.clock.advance(crate::credprov::HANDOVER_MS + 1);
    assert!(!query(&h, &mut c).pending);
}

/// VM test 2026-11-15: under `fast_user_switch` the console notification of seat 2's
/// fresh logon session arrived before the session was bound to seat 2, and seat 1 (which
/// mirrored the console) took it. LogonUI there was then answered for seat 1: no seat tile,
/// not pending, so no auto-logon. Seat 1 must stay on its own session while it is away.
#[test]
fn fus_hand_over_keeps_seat1_on_its_own_session() {
    let h = fus_harness();
    let home = seat_session_id(&h, s(1));
    let fresh = home + 40;
    // Start of seat 2: the hand-over is noted, the backend remembered seat 1's session and
    // disconnected the console; Windows reports the new console before the start binds it.
    h.s.note_credprov_handover(s(2));
    *h.f.fus.console_seat_away.lock().unwrap() = Some(home);
    *h.p.console.lock().unwrap() = Some(fresh);
    h.s.on_session_change(SessionChange::ConsoleConnect, fresh);
    assert_eq!(seat_session_id(&h, s(1)), home, "seat 1 keeps its own session");
    // LogonUI of the fresh session asks now: not seat 1, and pending (polls for the grant).
    let mut c = logonui(&h, 950, fresh);
    credprov_hello(&h, &mut c).unwrap();
    let q = query(&h, &mut c);
    assert_ne!(q.seat, Some(s(1)), "{q:?}");
    assert!(q.pending, "{q:?}");
    // Seat 2 is bound to the session and its user signs in there: seat 2's logon, not seat 1's.
    h.s.lock().seats.get_mut(&s(2)).unwrap().rt.session_id = Some(fresh);
    let ali = crate::platform::SessionUser { domain: "PC".into(), name: "ali".into(), sid: "S-1-5-21-9-ali".into(), logon_sid: "S-1-5-5-0-9".into() };
    h.p.users.lock().unwrap().insert(fresh, ali);
    h.s.on_session_change(SessionChange::Logon, fresh);
    assert!(matches!(state(&h, s(2)), SeatState::SignedIn { ref user } if user.ends_with("ali")), "{:?}", state(&h, s(2)));
    assert_eq!(seat_session_id(&h, s(1)), home);
    assert!(!matches!(state(&h, s(1)), SeatState::SignedIn { ref user } if user.ends_with("ali")));
    // Seat 1 back on the console: it mirrors the console again.
    *h.f.fus.console_seat_away.lock().unwrap() = None;
    *h.p.console.lock().unwrap() = Some(home);
    h.s.on_session_change(SessionChange::ConsoleConnect, home);
    assert_eq!(seat_session_id(&h, s(1)), home);
}

/// Seat 1 (no account) locked on the console: the answer carries the session user's SID,
/// so the provider's switch tile is listed on that user's lock screen (VM test 2026-11-15).
#[test]
fn credprov_seat_without_account_gets_the_session_user_sid() {
    let h = fus_harness();
    let one = seat_session_id(&h, s(1));
    let me = crate::platform::SessionUser { domain: "PC".into(), name: "admin".into(), sid: "S-1-5-21-9-500".into(), logon_sid: "S-1-5-5-0-1".into() };
    h.p.users.lock().unwrap().insert(one, me);
    let mut c = logonui(&h, 960, one);
    credprov_hello(&h, &mut c).unwrap();
    let ResponseBody::CredprovSession(q) =
        call(&h, &mut c, RequestBody::CredprovQuery { scenario: seat_proto::CredprovScenario::Unlock }).unwrap()
    else {
        panic!()
    };
    assert_eq!(q.seat, Some(s(1)), "{q:?}");
    // Its session user's tile (password field + links): Windows 11 never showed the
    // user-less switch tile there (credprov.log, VM test 2026-11-15).
    assert_eq!(q.account.as_deref(), Some(r"PC\admin"), "{q:?}");
    assert!(q.switch_targets.contains(&s(2)), "{q:?}");
    assert_eq!(q.user_sid.as_deref(), Some("S-1-5-21-9-500"));
}

/// `switch_seat` (admin, protocol 3) puts a running seat back on the console without its
/// password; the logon screen it opens preselects that seat's tile without auto-logon.
#[test]
fn switch_seat_over_the_pipe_preselects_the_target_tile() {
    let h = fus_harness();
    let mut a = admin();
    // Seat 1 holds the console: nothing to switch back to.
    assert_eq!(call(&h, &mut a, RequestBody::SwitchSeat { seat: s(1) }).unwrap_err().code, ErrorCode::InvalidValue);
    call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(2)]) }).unwrap();
    let two = seat_session_id(&h, s(2));
    // A standard user may not.
    let mut u = user(1);
    assert_eq!(call(&h, &mut u, RequestBody::SwitchSeat { seat: s(2) }).unwrap_err().code, ErrorCode::AccessDenied);
    let n = h.f.fus.start_passwords.lock().unwrap().len();
    call(&h, &mut a, RequestBody::SwitchSeat { seat: s(2) }).unwrap();
    let starts = h.f.fus.start_passwords.lock().unwrap().clone();
    assert_eq!(starts.len(), n + 1);
    assert_eq!(starts.last(), Some(&(s(2), false)), "never with the stored password");
    // The console (a fresh logon screen, not seat 2's session) preselects seat 2's tile.
    let fresh = two + 70;
    *h.p.console.lock().unwrap() = Some(fresh);
    let mut c = logonui(&h, 970, fresh);
    credprov_hello(&h, &mut c).unwrap();
    let q = query(&h, &mut c);
    assert_eq!(q.seat, Some(s(2)), "{q:?}");
    assert!(q.preselect && !q.auto_submit && !q.hide_others, "{q:?}");
    assert!(q.account.as_deref().is_some_and(|x| x.ends_with("ali")), "{q:?}");
    assert!(q.switch_targets.is_empty(), "not a seat session: no links");
    // The lock screen of another session never preselects; and it ends after HANDOVER_MS.
    let ResponseBody::CredprovSession(lock) =
        call(&h, &mut c, RequestBody::CredprovQuery { scenario: seat_proto::CredprovScenario::Unlock }).unwrap()
    else {
        panic!()
    };
    assert!(!lock.preselect);
    h.clock.advance(crate::credprov::HANDOVER_MS + 1);
    assert!(!query(&h, &mut c).preselect);
}

/// A password-less switch to seat 1 followed by a start of seat 2: the fresh console's
/// logon screen must not still preselect seat 1 (VM 2026-11-16: `switch_seat 1`, then
/// `switch_seat 3` within the hand-over window; LogonUI was told "seat 1, preselect" and
/// seat 3 never signed in).
#[test]
fn a_new_handover_supersedes_an_earlier_password_less_switch() {
    let h = fus_harness();
    let mut a = admin();
    let home = seat_session_id(&h, s(1));
    // Seat 1 is off the console; switching back to it has no password (preselect).
    *h.f.fus.console_seat_away.lock().unwrap() = Some(home);
    call(&h, &mut a, RequestBody::SwitchSeat { seat: s(1) }).unwrap();
    assert!(h.s.lock().credprov.switch_to.as_ref().is_some_and(|t| t.seat == s(1)));
    // Seat 2 (password stored) starts within the hand-over window.
    *h.f.fus.console_seat_away.lock().unwrap() = None;
    call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(2)]) }).unwrap();
    assert!(h.s.lock().credprov.switch_to.is_none(), "the start superseded the old switch");
    // A fresh console session not bound to a seat yet: no stale seat 1 preselect.
    let fresh = 900;
    *h.p.console.lock().unwrap() = Some(fresh);
    let mut c = logonui(&h, 990, fresh);
    credprov_hello(&h, &mut c).unwrap();
    let q = query(&h, &mut c);
    assert!(!q.preselect && q.seat != Some(s(1)), "{q:?}");
}

/// Console at a logon screen (nobody signed in at seat 1, no seat 1 session of its own) when
/// seat 2's hand-over starts: LogonUI of the fresh console session asks before seat 2's
/// session id is recorded. Seat 1 follows the console there, so the answer named seat 1;
/// it must still say "pending" so the provider asks again (VM 2026-11-17).
#[test]
fn handover_pending_while_seat_one_has_nobody_signed_in() {
    let h = fus_harness();
    h.p.users.lock().unwrap().remove(&1);
    h.s.refresh_console_seat();
    h.s.note_credprov_handover(s(2));
    let fresh = 77;
    *h.p.console.lock().unwrap() = Some(fresh);
    h.s.refresh_console_seat();
    let mut c = logonui(&h, 980, fresh);
    credprov_hello(&h, &mut c).unwrap();
    let q = query(&h, &mut c);
    assert!(q.pending, "{q:?}");
    // Somebody signed in at seat 1 (its own session on the console): not seat 2's hand-over.
    h.p.sign_in(fresh, "konsol");
    h.s.refresh_console_seat();
    assert!(!query(&h, &mut c).pending);
}

/// `fast_user_switch`: seat 1 never takes the console session of a seat N hand-over (it
/// has no session of its own then), so the two seats never share a session.
#[test]
fn seat_one_does_not_claim_a_handover_session() {
    let h = fus_harness();
    h.p.users.lock().unwrap().remove(&1);
    h.s.refresh_console_seat();
    // The console-connect of the fresh session comes before the seat knows it (the mock
    // backend hands out session 10 next): seat 1 follows the console there first.
    *h.p.console.lock().unwrap() = Some(10);
    h.s.refresh_console_seat();
    let mut a = admin();
    call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(2)]) }).unwrap();
    let sess = seat_session_id(&h, s(2));
    assert_eq!(sess, 10);
    let row = h.s.status().seats.into_iter().find(|r| r.seat == s(1)).unwrap();
    assert_ne!(row.session_id, Some(sess), "seat 1 gave the session back: {row:?}");
    h.s.refresh_console_seat();
    let row = h.s.status().seats.into_iter().find(|r| r.seat == s(1)).unwrap();
    assert_ne!(row.session_id, Some(sess), "{row:?}");
    // Seat 2's user signs in on the console: the logon is seat 2's, not seat 1's.
    h.p.sign_in(sess, "ali");
    h.s.on_session_change(SessionChange::Logon, sess);
    let rows = h.s.status().seats;
    let two = rows.iter().find(|r| r.seat == s(2)).unwrap();
    assert!(matches!(two.state, SeatState::SignedIn { .. }), "{two:?}");
    let one = rows.iter().find(|r| r.seat == s(1)).unwrap();
    assert!(!matches!(one.state, SeatState::SignedIn { .. }) && one.session_id != Some(sess), "{one:?}");
}

/// `fast_user_switch`: seat 2 still at its logon screen when seat 3's hand-over takes the
/// console. Seat 2's session ends; restarting it would take the console back, and seat 3
/// would do the same: an endless swap (VM 2026-11-17). Seat 2 stops instead. A seat whose
/// user had signed in is restarted as before.
#[test]
fn superseded_logon_screen_is_not_restarted() {
    let mut cfg = two_seat_config();
    let mut three = SeatConfig::new(s(3));
    three.account = SeatAccount::Local { user: "veli".into() };
    cfg.seats.push(three);
    let h = harness_with(cfg, None);
    *h.f.session.availability.lock().unwrap() = Availability::Unavailable { reason: "edition_unsupported".into() };
    h.s.reselect_backends();
    h.s.boot();
    let mut a = admin();
    call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(2)]) }).unwrap();
    call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(3)]) }).unwrap();
    let sess3 = seat_session_id(&h, s(3));
    *h.p.console.lock().unwrap() = Some(sess3);
    h.s.on_session_gone(s(2));
    let state = |h: &Harness, n: u8| h.s.status().seats.into_iter().find(|r| r.seat == s(n)).unwrap().state;
    assert_eq!(state(&h, 2), SeatState::Off);
    assert!(!h.s.lock().seats[&s(2)].want);
    // Seat 3 signed in, then its session ended: restarted (re-logon).
    h.p.sign_in(sess3, "veli");
    h.s.on_session_change(SessionChange::Logon, sess3);
    *h.p.console.lock().unwrap() = Some(1);
    h.s.on_session_gone(s(3));
    assert!(h.s.lock().seats[&s(3)].want);
    assert!(matches!(state(&h, 3), SeatState::Starting { .. }), "{:?}", state(&h, 3));
}

#[test]
fn credprov_no_auto_submit_without_an_account() {
    let h = fus_harness();
    let mut a = admin();
    call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(2)]) }).unwrap();
    let sess = seat_session_id(&h, s(2));
    let mut c = logonui(&h, 950, sess);
    credprov_hello(&h, &mut c).unwrap();
    let q = query(&h, &mut c);
    assert!(q.auto_submit && q.hide_others);
    assert_eq!(q.user_sid.as_deref(), Some("S-1-5-21-9-ali"), "the V2 tile gets the account SID");
    // SID lookups are cached.
    let n = h.p.account_lookups.load(Ordering::SeqCst);
    query(&h, &mut c);
    assert_eq!(h.p.account_lookups.load(Ordering::SeqCst), n);
    // The account goes away while the grant is armed: nothing is hidden any more (the
    // provider would show no seat tile, so the filter must not hide the others).
    {
        let mut st = h.s.lock();
        let seat = st.config.seats.iter_mut().find(|c| c.number == s(2)).unwrap();
        seat.account = SeatAccount::LogonScreen;
    }
    assert_eq!(h.s.lock().credprov.grants.len(), 1);
    let q = query(&h, &mut c);
    assert!(!q.auto_submit && !q.hide_others && q.account.is_none() && q.user_sid.is_none(), "{q:?}");
}

#[test]
fn credprov_switch_link() {
    let mut cfg = two_seat_config();
    let mut three = SeatConfig::new(s(3));
    three.account = SeatAccount::Local { user: "ayse".into() };
    cfg.seats.push(three);
    let h = harness_with(cfg, None);
    *h.f.session.availability.lock().unwrap() = Availability::Unavailable { reason: "edition_unsupported".into() };
    h.s.reselect_backends();
    h.s.boot();
    let mut a = admin();
    call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(2)]) }).unwrap();
    let sess = seat_session_id(&h, s(2));
    let mut c = logonui(&h, 800, sess);
    credprov_hello(&h, &mut c).unwrap();
    assert_eq!(query(&h, &mut c).switch_targets, vec![s(3)]);
    // Own seat and seat 1 are not targets.
    assert!(call(&h, &mut c, RequestBody::CredprovSwitch { seat: s(2) }).is_err());
    assert!(call(&h, &mut c, RequestBody::CredprovSwitch { seat: s(1) }).is_err());
    call(&h, &mut c, RequestBody::CredprovSwitch { seat: s(3) }).unwrap();
    assert!(h.f.fus.rec.calls().iter().any(|x| x == "start 3"), "{:?}", h.f.fus.rec.calls());
    // Rate limit.
    let busy = call(&h, &mut c, RequestBody::CredprovSwitch { seat: s(3) }).unwrap_err();
    assert_eq!(busy.reason.as_deref(), Some("busy"));
    // A LogonUI outside any seat session gets no links and cannot switch.
    let mut stray = logonui(&h, 801, 77);
    credprov_hello(&h, &mut stray).unwrap();
    assert!(query(&h, &mut stray).switch_targets.is_empty());
    h.clock.advance(crate::credprov::SWITCH_MIN_INTERVAL_MS + 1);
    assert!(call(&h, &mut stray, RequestBody::CredprovSwitch { seat: s(3) }).is_err());
}

/// A session-backend change undoes the old backend's host changes and prepares the new one
/// (rds_loopback -> fast_user_switch -> rds_loopback), without a restart.
#[test]
fn reselect_backends_restores_the_old_and_prepares_the_new_session_backend() {
    let h = harness_with(two_seat_config(), None);
    h.s.boot();
    let n = |m: &seat_core::mock::MockSessionBackend, what: &str| m.rec.calls().iter().filter(|c| c.as_str() == what).count();
    let (rds_restore, rds_prepare) = (n(&h.f.session, "restore_host"), n(&h.f.session, "prepare_host"));
    let (fus_restore, fus_prepare) = (n(&h.f.fus, "restore_host"), n(&h.f.fus, "prepare_host"));
    *h.f.session.availability.lock().unwrap() = Availability::Unavailable { reason: "edition_unsupported".into() };
    assert!(h.s.reselect_backends());
    assert_eq!(h.s.backends().session.kind(), seat_core::backend::SessionBackendKind::FastUserSwitch);
    assert_eq!(n(&h.f.session, "restore_host"), rds_restore + 1);
    assert_eq!(n(&h.f.fus, "prepare_host"), fus_prepare + 1);
    *h.f.session.availability.lock().unwrap() = Availability::Available;
    assert!(h.s.reselect_backends());
    assert_eq!(h.s.backends().session.kind(), seat_core::backend::SessionBackendKind::RdsLoopback);
    assert_eq!(n(&h.f.fus, "restore_host"), fus_restore + 1);
    assert_eq!(n(&h.f.session, "prepare_host"), rds_prepare + 1);
    // Same kind again: nothing restored or prepared.
    assert!(!h.s.reselect_backends());
    assert_eq!(n(&h.f.session, "prepare_host"), rds_prepare + 1);
    assert_eq!(n(&h.f.session, "restore_host"), rds_restore + 1);
}

/// The switch link authenticates nobody: a start from it uses no stored password and arms
/// no one-time logon; a later manager start does again.
#[test]
fn credprov_switch_link_never_logs_on_by_itself() {
    let mut cfg = two_seat_config();
    let mut three = SeatConfig::new(s(3));
    three.account = SeatAccount::Local { user: "ayse".into() };
    cfg.seats.push(three);
    let h = harness_with(cfg, None);
    *h.f.session.availability.lock().unwrap() = Availability::Unavailable { reason: "edition_unsupported".into() };
    h.s.reselect_backends();
    h.s.boot();
    let mut a = admin();
    call(&h, &mut a, RequestBody::SetSeatPassword { seat: s(3), password: Some(Secret::new("pw3")) }).unwrap();
    call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(2)]) }).unwrap();
    let sess = seat_session_id(&h, s(2));
    let mut c = logonui(&h, 810, sess);
    credprov_hello(&h, &mut c).unwrap();
    call(&h, &mut c, RequestBody::CredprovSwitch { seat: s(3) }).unwrap();
    assert_eq!(h.f.fus.start_passwords.lock().unwrap().last().copied(), Some((s(3), false)));
    assert!(h.s.lock().credprov.grants.values().all(|g| g.seat != s(3)), "no grant from a switch link");
    // Reconnect of the now running seat 3: no password either.
    h.clock.advance(crate::credprov::SWITCH_MIN_INTERVAL_MS + 1);
    call(&h, &mut c, RequestBody::CredprovSwitch { seat: s(3) }).unwrap();
    assert_eq!(h.f.fus.start_passwords.lock().unwrap().last().copied(), Some((s(3), false)));
    // The admin's start of seat 3 uses the stored password and arms the grant again.
    call(&h, &mut a, RequestBody::StopSeat { seat: s(3) }).unwrap();
    call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(3)]) }).unwrap();
    assert_eq!(h.f.fus.start_passwords.lock().unwrap().last().copied(), Some((s(3), true)));
    assert!(h.s.lock().credprov.grants.values().any(|g| g.seat == s(3)));
}

/// Seat 1 is a switch target while it runs with its own session; the agent of a seat
/// session can switch too (spec 35 §5.5(a)).
#[test]
fn seat1_switch_target_and_agent_switch() {
    let h = fus_harness();
    let mut a = admin();
    call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(2)]) }).unwrap();
    let two = seat_session_id(&h, s(2));
    let mut c = logonui(&h, 820, two);
    credprov_hello(&h, &mut c).unwrap();
    // The backend has no seat-1 session off the console: no link to seat 1.
    assert!(!query(&h, &mut c).switch_targets.contains(&s(1)));
    assert!(call(&h, &mut c, RequestBody::CredprovSwitch { seat: s(1) }).is_err());
    *h.f.fus.console_seat_away.lock().unwrap() = Some(1);
    assert!(query(&h, &mut c).switch_targets.contains(&s(1)));
    h.clock.advance(crate::credprov::SWITCH_MIN_INTERVAL_MS + 1);
    // The agent of seat 2's session.
    let mut agent = Caller { client: Some(ClientKind::Agent), agent_session: Some(two), ..user(two) };
    let ResponseBody::SwitchTargets(t) = call(&h, &mut agent, RequestBody::AgentSwitchTargets).unwrap() else { panic!() };
    assert_eq!(t, vec![s(1)]);
    assert!(call(&h, &mut agent, RequestBody::AgentSwitch { seat: s(2) }).is_err(), "own seat");
    let n = h.f.fus.rec.calls().iter().filter(|x| *x == "start 1").count();
    call(&h, &mut agent, RequestBody::AgentSwitch { seat: s(1) }).unwrap();
    // Runs on a worker: wait for the reconnect start.
    for _ in 0..200 {
        if h.f.fus.rec.calls().iter().filter(|x| *x == "start 1").count() > n {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(h.f.fus.rec.calls().iter().filter(|x| *x == "start 1").count() > n);
    assert_eq!(h.f.fus.start_passwords.lock().unwrap().last().copied(), Some((s(1), false)));
    // A user process that is not the verified agent gets nothing.
    let mut fake = Caller { client: Some(ClientKind::Agent), ..user(two) };
    assert!(call(&h, &mut fake, RequestBody::AgentSwitch { seat: s(1) }).is_err());
}

#[test]
fn credprov_registration_failure_is_a_notice_not_a_crash() {
    let h = harness_with(two_seat_config(), None);
    h.p.credprov_fail.store(true, Ordering::SeqCst);
    *h.f.session.availability.lock().unwrap() = Availability::Unavailable { reason: "edition_unsupported".into() };
    h.s.reselect_backends();
    h.s.boot();
    assert!(!h.s.lock().credprov.registered);
    let mut a = admin();
    call(&h, &mut a, RequestBody::SetSeatPassword { seat: s(2), password: Some(Secret::new("pw2")) }).unwrap();
    call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(2)]) }).unwrap();
    assert!(h.s.lock().credprov.grants.is_empty(), "nothing armed without a registration");
}

#[test]
fn kiosk_shell_hello_is_verified_and_survives_admin_only() {
    use seat_core::backend::session::KioskShellProcess;
    let mut cfg = two_seat_config();
    cfg.settings.admin_only = true;
    let h = harness_with(cfg, None);
    h.s.boot();
    let shell_hello = |seat: Option<SeatNumber>| {
        RequestBody::Hello(Hello { client: ClientKind::KioskShell, client_version: "t".into(), protocol_version: 1, seat, token: None })
    };
    // Not started by the backend: refused.
    let mut fake = Caller { pid: 5150, ..user(1) };
    assert_eq!(call(&h, &mut fake, shell_hello(Some(s(2)))).unwrap_err().code, ErrorCode::AccessDenied);
    h.f.session.kiosk_shells.lock().unwrap().push(KioskShellProcess { seat: s(2), session_id: 1, pid: 5150 });
    // Wrong seat or session: refused.
    let mut wrong = Caller { pid: 5150, ..user(1) };
    assert_eq!(call(&h, &mut wrong, shell_hello(Some(s(3)))).unwrap_err().code, ErrorCode::AccessDenied);
    let mut other_session = Caller { pid: 5150, ..user(4) };
    assert_eq!(call(&h, &mut other_session, shell_hello(Some(s(2)))).unwrap_err().code, ErrorCode::AccessDenied);
    let mut shell = Caller { pid: 5150, ..user(1) };
    call(&h, &mut shell, shell_hello(Some(s(2)))).unwrap();
    assert_eq!(shell.kiosk_seat, Some(s(2)));
    // admin_only: status (device list) still works for the verified shell...
    assert!(call(&h, &mut shell, RequestBody::ListDevices).is_ok());
    // ...but not for an ordinary user.
    let mut u = user(1);
    assert_eq!(call(&h, &mut u, RequestBody::ListDevices).unwrap_err().code, ErrorCode::AdminOnly);
    // The host topic is allowed (own seat's input packets only).
    assert!(call(&h, &mut shell, RequestBody::Subscribe { topics: vec![Topic::Host] }).is_ok());
}

// ---- seat kinds (merge step 4, architecture.md section 10) ----

/// `two_seat_config` plus remote seat 3 (no devices).
fn with_remote_seat() -> Config {
    let mut c = two_seat_config();
    let r = seat_core::remote::RemoteSeat { display_name: "Salon".into(), port: 48100, ..Default::default() };
    let mut three = SeatConfig::new_remote(s(3), r);
    three.name = "Salon".into();
    c.seats.push(three);
    c
}

#[test]
fn remote_requests_check_kind_and_need_the_worker() {
    let h = harness_with(with_remote_seat(), None);
    h.s.boot();
    let mut a = admin();
    // Local-only request on the remote seat, remote-only request on a local seat.
    let e = call(&h, &mut a, RequestBody::RelogonSeat { seat: s(3) }).unwrap_err();
    assert_eq!(e.code, ErrorCode::WrongSeatKind);
    assert_eq!(e.reason.as_deref(), Some("local"));
    let e = call(&h, &mut a, RequestBody::GetPairState { seat: s(2) }).unwrap_err();
    assert_eq!(e.code, ErrorCode::WrongSeatKind);
    assert_eq!(e.reason.as_deref(), Some("remote"));
    // Right kind / host-wide, but this service has no remote seat worker.
    for body in [RequestBody::GetPairState { seat: s(3) }, RequestBody::GetHostInfo, RequestBody::TestSeatCredentials { seat: s(2) }] {
        let e = call(&h, &mut a, body).unwrap_err();
        assert_eq!(e.code, ErrorCode::Unsupported);
        assert_eq!(e.reason.as_deref(), Some(crate::remote::glue::REMOTE_WORKER_UNAVAILABLE));
    }
    // Unknown seat.
    assert_eq!(call(&h, &mut a, RequestBody::GetSeatOtp { seat: s(9) }).unwrap_err().code, ErrorCode::NotFound);
    // Admin only.
    let mut u = user(1);
    assert_eq!(call(&h, &mut u, RequestBody::GetHostInfo).unwrap_err().code, ErrorCode::AccessDenied);
}

#[test]
fn remote_seats_are_not_started_by_the_local_worker() {
    let h = harness_with(with_remote_seat(), None);
    h.s.boot();
    let mut a = admin();
    let e = call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(3)]) }).unwrap_err();
    assert_eq!(e.code, ErrorCode::Unsupported);
    // Autostart (`None`) starts the local seat 2 only.
    call(&h, &mut a, RequestBody::StartSeats { seats: None }).unwrap();
    assert_ne!(state(&h, s(2)), SeatState::Off);
    assert_eq!(state(&h, s(3)), SeatState::Off);
    // Without concurrent sessions a remote start is `session_unavailable`.
    let mut c = with_remote_seat();
    c.settings.backends.session = seat_core::backend::SessionBackendKind::FastUserSwitch;
    let h = harness_with(c, None);
    h.s.boot();
    let e = call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(3)]) }).unwrap_err();
    assert_eq!(e.code, ErrorCode::SessionUnavailable);
}

// ---- remote seat worker (merge step 6) ----

/// Records the engine hooks.
#[derive(Default)]
struct HostRec(std::sync::Mutex<Vec<String>>);

impl crate::remote::RemoteHost for HostRec {
    fn service_start(&self, global: &seat_core::remote::RemoteSettings, users: &[String]) {
        self.0.lock().unwrap().push(format!("start multi={} users={users:?}", global.multi_session));
    }
    fn service_stop(&self, users: &[String]) {
        self.0.lock().unwrap().push(format!("stop users={users:?}"));
    }
}

/// `with_remote_seat` with seat 3 = user "veli", autostart on.
fn remote_ready_config() -> Config {
    let mut c = with_remote_seat();
    let three = c.seats.iter_mut().find(|x| x.number == s(3)).unwrap();
    three.account = SeatAccount::Local { user: "veli".into() };
    three.autostart = true;
    c.settings.start_mode = seat_core::config::StartMode::AtBoot;
    c.settings.timings.start_delay_ms = 0;
    let three = c.seats.iter_mut().find(|x| x.number == s(3)).unwrap();
    let r = three.remote.as_mut().unwrap();
    r.id = "0f8fad5b-d9cb-469f-a165-70867728950e".into();
    r.rdp_client_name = "PSS0F8FAD5B".into();
    c
}

/// A service with the Split controller over the fake platform as its remote worker.
fn remote_harness(config: Config) -> (Harness, Arc<HostRec>) {
    let (h, rec, _) = remote_harness_removed(config);
    (h, rec)
}

/// [`remote_harness`] plus the ids of the seats whose files the fake platform removed.
fn remote_harness_removed(config: Config) -> (Harness, Arc<HostRec>, Arc<Mutex<Vec<String>>>) {
    let removed: Arc<Mutex<Vec<String>>> = Arc::default();
    let shared = removed.clone();
    let rec = Arc::new(HostRec::default());
    let host: Arc<dyn crate::remote::RemoteHost> = rec.clone();
    let remote = crate::service::RemoteDeps {
        worker: Box::new(|doc| {
            Box::new(crate::remote::control::Controller::new(
                doc,
                crate::remote::control::tests::FakePlatform { removed_ids: shared, ..Default::default() },
                "t",
                true,
            ))
        }),
        host: Some(host),
    };
    (harness_full(config, None, Some(remote)), rec, removed)
}

/// VM round 2 (2026-11-17): a `settings.remote` change saved with `apply_config` (the
/// manager's path) reaches the worker's `apply_global` (provider registration, isolation),
/// not only Split's `set_global_settings`. An apply that leaves it alone does not.
#[test]
fn remote_settings_saved_with_apply_config_reach_the_worker() {
    let globals: Arc<Mutex<Vec<seat_core::remote::RemoteSettings>>> = Arc::default();
    let shared = globals.clone();
    let remote = crate::service::RemoteDeps {
        worker: Box::new(move |doc| {
            Box::new(crate::remote::control::Controller::new(
                doc,
                crate::remote::control::tests::FakePlatform { globals: shared, ..Default::default() },
                "t",
                true,
            ))
        }),
        host: None,
    };
    let h = harness_full(remote_ready_config(), None, Some(remote));
    h.s.boot();
    let mut a = admin();
    let apply = |a: &mut Caller, f: &dyn Fn(&mut Config)| {
        let ResponseBody::Config(view) = call(&h, a, RequestBody::GetConfig).unwrap() else { panic!() };
        let mut cfg = view.config.clone();
        f(&mut cfg);
        call(&h, a, RequestBody::ApplyConfig { config: Box::new(cfg), base_revision: view.config.revision, force: false }).unwrap();
    };
    apply(&mut a, &|c| c.seats[0].name = "renamed".into());
    assert!(globals.lock().unwrap().is_empty(), "no settings.remote change");
    apply(&mut a, &|c| c.settings.remote.display_backend = Some(seat_core::remote::DisplayBackendPref::PulsarIdd));
    let g = globals.lock().unwrap();
    assert_eq!(g.len(), 1);
    assert_eq!(g[0].display_backend, Some(seat_core::remote::DisplayBackendPref::PulsarIdd));
}

/// VM round 2 run 3 (2026-11-17): a remote seat added with `apply_config` was unknown to
/// the engine until its first start, so `pulsar_idd` left the protocol provider
/// unregistered. A seat-list change hands the remote seats' accounts to the worker; an
/// apply that changes neither seats nor `settings.remote` does not.
#[test]
fn seat_list_saved_with_apply_config_reaches_the_worker() {
    let users: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
    let shared = users.clone();
    let remote = crate::service::RemoteDeps {
        worker: Box::new(move |doc| {
            Box::new(crate::remote::control::Controller::new(
                doc,
                crate::remote::control::tests::FakePlatform { user_sets: shared, ..Default::default() },
                "t",
                true,
            ))
        }),
        host: None,
    };
    let h = harness_full(remote_ready_config(), None, Some(remote));
    h.s.boot();
    let mut a = admin();
    let apply = |a: &mut Caller, f: &dyn Fn(&mut Config)| {
        let ResponseBody::Config(view) = call(&h, a, RequestBody::GetConfig).unwrap() else { panic!() };
        let mut cfg = view.config.clone();
        f(&mut cfg);
        call(&h, a, RequestBody::ApplyConfig { config: Box::new(cfg), base_revision: view.config.revision, force: false }).unwrap();
    };
    apply(&mut a, &|c| c.settings.tweaks.lock_screen_image = !c.settings.tweaks.lock_screen_image);
    assert!(users.lock().unwrap().is_empty(), "no seat change");
    apply(&mut a, &|c| c.seats[0].name = "renamed".into());
    let u = users.lock().unwrap();
    assert_eq!(u.len(), 1);
    assert!(!u[0].is_empty(), "remote seat accounts handed over: {u:?}");
}

#[test]
fn deleting_a_remote_seat_with_apply_config_stops_first_then_removes_files() {
    let mut c = remote_ready_config();
    c.seats.iter_mut().find(|x| x.number == s(3)).unwrap().autostart = false;
    let id = c.seat(s(3)).unwrap().remote.as_ref().unwrap().id.clone();
    let (h, _rec, removed) = remote_harness_removed(c);
    h.s.boot();
    let mut a = admin();
    call(&h, &mut a, RequestBody::SetSeatPassword { seat: s(3), password: Some(Secret::new("pw3")) }).unwrap();
    call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(3)]) }).unwrap();
    assert!(matches!(state(&h, s(3)), SeatState::Starting { .. }));
    let delete = |a: &mut Caller| {
        let ResponseBody::Config(view) = call(&h, a, RequestBody::GetConfig).unwrap() else { panic!() };
        let mut cfg = view.config.clone();
        cfg.seats.retain(|x| x.number != s(3));
        call(&h, a, RequestBody::ApplyConfig { config: Box::new(cfg), base_revision: view.config.revision, force: true })
    };
    // Running: refused like Split's delete_seat, nothing removed, the seat stays.
    let e = delete(&mut a).unwrap_err();
    assert_eq!(e.code, ErrorCode::SeatRunning);
    assert!(removed.lock().unwrap().is_empty());
    assert!(h.s.lock().config.seat(s(3)).is_some());
    call(&h, &mut a, RequestBody::StopSeat { seat: s(3) }).unwrap();
    h.s.tick();
    assert_eq!(state(&h, s(3)), SeatState::Off);
    // Stopped: saved, and the worker removed its stream secrets, folder and rules.
    assert!(delete(&mut a).is_ok());
    assert_eq!(removed.lock().unwrap().as_slice(), [id]);
    assert!(h.s.lock().config.seat(s(3)).is_none());
    assert!(!h.s.lock().remote_rt.contains_key(&s(3)));
}

#[test]
fn mixed_local_and_remote_seats_start_and_stop_with_fakes() {
    let mut c = remote_ready_config();
    c.seats.iter_mut().find(|x| x.number == s(3)).unwrap().autostart = false;
    let (h, rec) = remote_harness(c);
    h.s.boot();
    assert_eq!(rec.0.lock().unwrap().as_slice(), [r#"start multi=false users=["veli"]"#]);
    let mut a = admin();
    // No password yet: the worker refuses the start with Split's error code.
    let e = call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(3)]) }).unwrap_err();
    assert_eq!(e.code, ErrorCode::MissingCredentials);
    call(&h, &mut a, RequestBody::SetSeatPassword { seat: s(3), password: Some(Secret::new("pw3")) }).unwrap();
    call(&h, &mut a, RequestBody::SetSeatPassword { seat: s(2), password: Some(Secret::new("pw2")) }).unwrap();
    let rx = h.s.events.subscribe(vec![Topic::Status], crate::events::Scope { admin: true, ..Default::default() });
    call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(2), s(3)]) }).unwrap();
    assert_ne!(state(&h, s(2)), SeatState::Off);
    assert!(matches!(state(&h, s(3)), SeatState::Starting { .. }), "{:?}", state(&h, s(3)));
    let ResponseBody::Status(st) = call(&h, &mut a, RequestBody::GetStatus).unwrap() else { panic!() };
    let three = st.seats.iter().find(|r| r.seat == s(3)).unwrap();
    assert_eq!(three.kind, seat_core::config::SeatKind::Remote);
    // The worker's `seat_phase` event became a v3 `seat_state` of the remote seat.
    let mut saw = false;
    while let Ok(f) = rx.try_recv() {
        if let Event::SeatState { runtime } = f.event
            && runtime.seat == s(3)
        {
            saw = true;
        }
    }
    assert!(saw, "remote seat_state published");
    // Stop both; the fake reports the remote seat stopped.
    call(&h, &mut a, RequestBody::StopSeat { seat: s(3) }).unwrap();
    call(&h, &mut a, RequestBody::StopSeat { seat: s(2) }).unwrap();
    h.s.tick();
    assert_eq!(state(&h, s(3)), SeatState::Off);
    assert_eq!(call(&h, &mut a, RequestBody::StopSeat { seat: s(3) }).unwrap_err().code, ErrorCode::SeatNotRunning);
    // Service stop: remote seats stopped, final sweep with the remote users.
    h.s.shutdown();
    assert_eq!(rec.0.lock().unwrap().last().map(String::as_str), Some(r#"stop users=["veli"]"#));
}

#[test]
fn remote_autostart_suspend_and_resume() {
    let (h, _rec) = remote_harness(remote_ready_config());
    h.s.secrets.set(s(3), Some(&Secret::new("pw3"))).unwrap();
    h.s.boot();
    assert!(matches!(state(&h, s(3)), SeatState::Starting { .. }), "autostart: {:?}", state(&h, s(3)));
    h.s.on_suspend();
    h.s.tick();
    assert_eq!(state(&h, s(3)), SeatState::Off);
    h.s.on_resume();
    assert!(matches!(state(&h, s(3)), SeatState::Starting { .. }), "resume: {:?}", state(&h, s(3)));
}

#[test]
fn remote_requests_reach_the_worker_by_seat_number() {
    let (h, _rec) = remote_harness(remote_ready_config());
    h.s.boot();
    let mut a = admin();
    // A stopped seat has no pairing.
    let ResponseBody::PairState(p) = call(&h, &mut a, RequestBody::GetPairState { seat: s(3) }).unwrap() else { panic!() };
    assert_eq!(p, Default::default());
    // Seat-scoped requests that need a running seat answer Split's error.
    let e = call(&h, &mut a, RequestBody::ListPairedClients { seat: s(3) }).unwrap_err();
    assert_eq!(e.code, ErrorCode::ServiceUnavailable);
    // `seat:<N>` log sources become Split's seat names.
    let ResponseBody::LogLines(l) = call(&h, &mut a, RequestBody::GetLogTail { source: "seat:3".into(), lines: 5 }).unwrap() else {
        panic!()
    };
    assert_eq!(l, ["Salon:5"]);
    assert_eq!(call(&h, &mut a, RequestBody::GetLogTail { source: "seat:2".into(), lines: 5 }).unwrap_err().code, ErrorCode::InvalidValue);
    // Account requests serve both kinds.
    call(&h, &mut a, RequestBody::SetSeatPassword { seat: s(2), password: Some(Secret::new("pw")) }).unwrap();
    let ResponseBody::CredentialTest(t) = call(&h, &mut a, RequestBody::TestSeatCredentials { seat: s(2) }).unwrap() else { panic!() };
    assert!(t.ok, "local seat 2 = ali / pw");
    call(&h, &mut a, RequestBody::SetSeatPassword { seat: s(3), password: Some(Secret::new("wrong")) }).unwrap();
    let ResponseBody::CredentialTest(t) = call(&h, &mut a, RequestBody::TestSeatCredentials { seat: s(3) }).unwrap() else { panic!() };
    assert_eq!((t.ok, t.error_code), (false, Some(1326)));
    let e = call(&h, &mut a, RequestBody::EnableUnlockAccount { seat: s(2), enable: true, unlock: true, confirm: false }).unwrap_err();
    assert_eq!(e.code, ErrorCode::BadRequest);
    assert!(call(&h, &mut a, RequestBody::EnableUnlockAccount { seat: s(3), enable: true, unlock: false, confirm: true }).is_ok());
    // Host-wide requests.
    assert!(matches!(call(&h, &mut a, RequestBody::GetShimStatus), Err(_) | Ok(ResponseBody::ShimStatus(_))));
    let e = call(&h, &mut a, RequestBody::CleanupAudioEndpoints { confirm: false }).unwrap_err();
    assert_eq!(e.code, ErrorCode::BadRequest);
    // Worker-owned fields written by the worker land in the document: multi-session.
    let r = call(&h, &mut a, RequestBody::SetMultiSession { enabled: true });
    assert!(r.is_err() || h.s.lock().config.settings.remote.multi_session, "{r:?}");
}

#[test]
fn remote_seat_status_is_admin_only() {
    let h = harness_with(with_remote_seat(), None);
    h.s.boot();
    let mut a = admin();
    let ResponseBody::Status(st) = call(&h, &mut a, RequestBody::GetStatus).unwrap() else { panic!() };
    let kinds: Vec<_> = st.seats.iter().map(|r| (r.seat, r.kind)).collect();
    use seat_core::config::SeatKind;
    assert_eq!(kinds, vec![(s(1), SeatKind::Local), (s(2), SeatKind::Local), (s(3), SeatKind::Remote)]);
    let mut u = user(1);
    let ResponseBody::Status(st) = call(&h, &mut u, RequestBody::GetStatus).unwrap() else { panic!() };
    assert!(st.seats.iter().all(|r| r.kind == SeatKind::Local) && st.seats.len() == 2);
    assert_eq!(call(&h, &mut u, RequestBody::Subscribe { topics: vec![Topic::Stream] }).unwrap_err().code, ErrorCode::AccessDenied);
    assert!(call(&h, &mut a, RequestBody::Subscribe { topics: vec![Topic::Stream] }).is_ok());
}

#[test]
fn remote_seat_events_are_admin_only_with_empty_topics() {
    use seat_core::config::SeatKind;
    use seat_proto::{Event, StreamEvent};
    let h = harness_with(with_remote_seat(), None);
    h.s.boot();
    let mut u = user(1);
    let mut a = admin();
    call(&h, &mut u, RequestBody::GetStatus).unwrap();
    call(&h, &mut a, RequestBody::GetStatus).unwrap();
    let sub_u = h.s.handle(&mut u, Request::new(2, RequestBody::Subscribe { topics: vec![] })).subscription.unwrap();
    let sub_a = h.s.handle(&mut a, Request::new(2, RequestBody::Subscribe { topics: vec![] })).subscription.unwrap();
    let after = h.s.events.last_seq();
    let rt = |seat: SeatNumber, kind: SeatKind| {
        let mut r = h.s.status().seats.into_iter().find(|r| r.seat == seat).unwrap();
        r.kind = kind;
        r
    };
    h.s.events.publish(Event::Stream { seat: s(3), event: StreamEvent::PairPending(serde_json::json!({"client": "tv"})) });
    h.s.events.publish(Event::SeatState { runtime: rt(s(3), SeatKind::Remote) });
    h.s.events.publish(Event::Notice { code: "x".into(), seat: Some(s(3)), detail: String::new() });
    h.s.events.publish(Event::Notice { code: seat_proto::remote::HOST_WARNING_NOTICE.into(), seat: None, detail: String::new() });
    h.s.events.publish(Event::SeatState { runtime: rt(s(2), SeatKind::Local) });
    h.s.events.publish(Event::Notice { code: "y".into(), seat: Some(s(2)), detail: String::new() });
    let seen_u: Vec<_> = sub_u.try_iter().map(|f| f.event).collect();
    assert_eq!(seen_u.len(), 2, "{seen_u:?}");
    assert!(seen_u.iter().all(|e| !matches!(e, Event::Stream { .. })));
    assert!(matches!(&seen_u[0], Event::SeatState { runtime } if runtime.seat == s(2)));
    assert_eq!(sub_a.try_iter().count(), 6);
    let poll = |c: &mut Caller| {
        let body = RequestBody::PollEvents { after, wait_ms: 0, topics: vec![] };
        let ResponseBody::Events(b) = call(&h, c, body).unwrap() else { panic!() };
        b.events.len()
    };
    assert_eq!(poll(&mut u), 2);
    assert_eq!(poll(&mut a), 6);
    // Explicit status topic: still no remote seat state for a standard user.
    let body = RequestBody::PollEvents { after, wait_ms: 0, topics: vec![Topic::Status] };
    let ResponseBody::Events(b) = call(&h, &mut u, body).unwrap() else { panic!() };
    assert_eq!(b.events.len(), 2);
}

#[test]
fn config_requests_need_protocol_3() {
    let h = harness();
    let mut old = admin();
    let hello = RequestBody::Hello(Hello { client: ClientKind::Manager, client_version: "t".into(), protocol_version: 1, seat: None, token: None });
    call(&h, &mut old, hello).unwrap();
    assert!(call(&h, &mut old, RequestBody::GetStatus).is_ok());
    let e = call(&h, &mut old, RequestBody::GetConfig).unwrap_err();
    assert_eq!(e.code, ErrorCode::VersionMismatch);
    assert_eq!(e.reason.as_deref(), Some("protocol_too_old"));
    let mut cur = admin();
    assert!(matches!(call(&h, &mut cur, RequestBody::GetConfig), Ok(ResponseBody::Config(_))));
}

#[test]
fn apply_config_accepts_remote_seats_and_validates_them() {
    let h = harness();
    let mut a = admin();
    let ResponseBody::Config(view) = call(&h, &mut a, RequestBody::GetConfig).unwrap() else { panic!() };
    let base = view.config.revision;
    let mut cfg = with_remote_seat();
    cfg.revision = base;
    let r = call(&h, &mut a, RequestBody::ApplyConfig { config: Box::new(cfg.clone()), base_revision: base, force: false });
    assert!(r.is_ok(), "{r:?}");
    let ResponseBody::Config(view) = call(&h, &mut a, RequestBody::GetConfig).unwrap() else { panic!() };
    assert!(view.config.seat(s(3)).is_some_and(|x| x.is_remote()));
    // A device on the remote seat is refused.
    let mut bad = view.config.clone();
    bad.devices.push(assign("kbd;9", DeviceKind::Keyboard, s(3).mask()));
    let r = call(&h, &mut a, RequestBody::ApplyConfig { config: Box::new(bad), base_revision: view.config.revision, force: false });
    assert_eq!(r.unwrap_err().code, ErrorCode::InvalidConfig);
}

/// A DualSense on USB: HID game pad collection (usage page 1, usage 5).
fn ds5_dev(key: &str, path: &str) -> seat_core::device::Device {
    use seat_core::device::{DeviceDetails, InputClass, InputInfo};
    let mut d = device(key, DeviceKind::Hid);
    d.details = DeviceDetails::Input(InputInfo {
        class: InputClass::Hid,
        usage_page: 1,
        usage: 5,
        vendor_id: 0x054c,
        product_id: 0x0ce6,
        interface_path: path.into(),
        bus: "usb".into(),
        built_in: false,
        last_activity_ms: 0,
    });
    d
}

#[test]
fn local_pad_targets_follow_routing_session_and_the_seat_switch() {
    let mut c = two_seat_config();
    c.seats[1].local_mut().unwrap().virtual_pads = true;
    let h = harness_with(c, None);
    *h.f.input.devices.lock().unwrap() = vec![
        input_dev("kbd;1", DeviceKind::Keyboard, KBD1),
        ds5_dev("pad;1", r"\\?\HID#VID_054C&PID_0CE6&MI_03#8&1&0&0000#{4d1e55b2-f16f-11cf-88cb-001111000030}"),
        ds5_dev("pad;2", r"\\?\HID#VID_054C&PID_0CE6&MI_03#8&2&0&0000#{4d1e55b2-f16f-11cf-88cb-001111000030}"),
    ];
    h.s.boot();
    // Routed to seat 2 but the seat has no session yet: nothing.
    h.s.lock().input_overrides.insert(k("pad;2"), s(2).mask());
    assert!(h.s.local_pad_targets().is_empty());
    let mut a = admin();
    call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(2)]) }).unwrap();
    h.p.sign_in(10, "ali");
    h.s.on_session_change(SessionChange::Logon, 10);
    let t = h.s.local_pad_targets();
    assert_eq!(t.len(), 1, "{t:?}");
    assert_eq!((t[0].seat, t[0].session_id), (2, 10));
    assert!(!t[0].owner_sid.is_empty());
    assert_eq!(t[0].pads.len(), 1, "only the pad routed to seat 2 alone");
    assert_eq!(t[0].pads[0].unit_key, "pad;2");
    assert_eq!((t[0].pads[0].vendor_id, t[0].pads[0].product_id), (0x054c, 0x0ce6));
    // Routed to several seats (or back to the console seat): no virtual pad.
    h.s.lock().input_overrides.insert(k("pad;2"), SeatMask::CONSOLE);
    assert!(h.s.local_pad_targets().is_empty());
    // The seat switch off: nothing even when routed.
    h.s.lock().input_overrides.insert(k("pad;2"), s(2).mask());
    assert_eq!(h.s.local_pad_targets().len(), 1);
    h.s.lock().config.seats[1].local_mut().unwrap().virtual_pads = false;
    assert!(h.s.local_pad_targets().is_empty());
}

#[test]
fn hyperv_helper_checks_before_it_acts() {
    use seat_proto::hyperv::HyperVAction as A;
    let h = harness();
    let mut a = admin();
    assert_eq!(call(&h, &mut a, RequestBody::HyperV { action: A::EnableFeature { confirm: false } }).unwrap_err().code, ErrorCode::InvalidValue);
    let bad = A::SetSeatNic { seat: 2, address: "127.0.0.1".into(), prefix_len: 24 };
    assert_eq!(call(&h, &mut a, RequestBody::HyperV { action: bad }).unwrap_err().code, ErrorCode::InvalidValue);
    let unknown = A::SetSeatNic { seat: 9, address: "10.0.0.9".into(), prefix_len: 24 };
    assert_eq!(call(&h, &mut a, RequestBody::HyperV { action: unknown }).unwrap_err().code, ErrorCode::NotFound);
    assert!(h.p.hyperv_actions.lock().unwrap().is_empty(), "nothing ran");
    let ok = A::SetSeatNic { seat: 2, address: "10.0.0.2".into(), prefix_len: 24 };
    let ResponseBody::HyperV(st) = call(&h, &mut a, RequestBody::HyperV { action: ok.clone() }).unwrap() else { panic!() };
    assert_eq!(st.feature, "enabled");
    assert_eq!(*h.p.hyperv_actions.lock().unwrap(), vec![ok]);
    let mut u = user(1);
    assert_eq!(call(&h, &mut u, RequestBody::HyperV { action: A::Status }).unwrap_err().code, ErrorCode::AccessDenied);
}

#[test]
fn reader_rules_follow_the_assignment() {
    use seat_shim_core::readers::{ReaderGrant, ReaderRule};
    let mut c = two_seat_config();
    c.devices.push(assign("scard;a", DeviceKind::SmartCardReader, s(2).mask()));
    c.devices.push(assign("scard;b", DeviceKind::SmartCardReader, SeatMask::SHARED));
    let h = harness_with(c, None);
    let mut ra = device("scard;a", DeviceKind::SmartCardReader);
    ra.name = "Reader A".into();
    let mut rb = device("scard;b", DeviceKind::SmartCardReader);
    rb.name = "Reader B".into();
    h.f.input.devices.lock().unwrap().extend([ra, rb]);
    h.s.boot();
    let mut a = admin();
    call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(2)]) }).unwrap();
    h.p.sign_in(10, "ali");
    h.s.on_session_change(SessionChange::Logon, 10);
    h.s.lock().ownership_dirty = true;
    h.s.tick();
    let rules = h.p.reader_rules.lock().unwrap().clone();
    let sid = seat_session_id(&h, s(2));
    assert_eq!(rules, vec![ReaderRule { name: "Reader A".into(), allowed: vec![ReaderGrant::Session(sid)] }], "shared readers get no rule");
    // Webcams are shared by default: no camera rule.
    assert!(h.p.camera_rules.lock().unwrap().is_empty());
}

#[test]
fn seat_sessions_with_cpu_limits_are_covered_every_tick() {
    let mut c = two_seat_config();
    c.seats[1].cpu_sets = vec![256, 257];
    c.seats[1].cpu_seat_wide = true;
    let h = harness_with(c, None);
    h.s.boot();
    h.s.tick();
    assert!(h.p.covered.lock().unwrap().is_empty(), "no session yet");
    let mut a = admin();
    call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(2)]) }).unwrap();
    let sid = seat_session_id(&h, s(2));
    h.s.tick();
    let covered = h.p.covered.lock().unwrap().clone();
    assert_eq!(covered, vec![(sid, vec![256, 257], true)], "seat 1 has no limit: not covered");
}

#[test]
fn reset_graphics_reaches_the_seats_agent_and_needs_a_session() {
    let h = harness();
    let mut a = admin();
    // Not running: no session to send the shortcut into.
    let e = call(&h, &mut a, RequestBody::ResetGraphics { seat: s(2) }).unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidValue);
    assert_eq!(call(&h, &mut a, RequestBody::ResetGraphics { seat: s(9) }).unwrap_err().code, ErrorCode::NotFound);
    call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(2)]) }).unwrap();
    let sid = seat_session_id(&h, s(2));
    call(&h, &mut a, RequestBody::ResetGraphics { seat: s(2) }).unwrap();
    let tasks = h.s.lock().agent_tasks.get(&sid).cloned().unwrap_or_default();
    assert!(tasks.iter().any(|t| t.task == AgentTaskKind::ResetGraphics), "{tasks:?}");
    // Seat 1: the console session's agent.
    call(&h, &mut a, RequestBody::ResetGraphics { seat: s(1) }).unwrap();
    assert!(h.s.lock().agent_tasks.get(&1).is_some_and(|t| t.iter().any(|t| t.task == AgentTaskKind::ResetGraphics)));
    // Standard users may not.
    let mut u = user(1);
    assert_eq!(call(&h, &mut u, RequestBody::ResetGraphics { seat: s(2) }).unwrap_err().code, ErrorCode::AccessDenied);
}

#[test]
fn removing_a_kiosk_user_is_explicit_and_refused_while_in_use() {
    let mut c = two_seat_config();
    c.seats[1].local_mut().unwrap().kiosk_shell = Some(Default::default());
    let h = harness_with(c, None);
    h.s.boot();
    let mut a = admin();
    let e = call(&h, &mut a, RequestBody::RemoveKioskUser { seat: s(2) }).unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidValue, "seat 2 still has a kiosk shell");
    assert!(h.p.kiosk_users_removed.lock().unwrap().is_empty());
    h.s.lock().config.seats[1].local_mut().unwrap().kiosk_shell = None;
    call(&h, &mut a, RequestBody::RemoveKioskUser { seat: s(2) }).unwrap();
    assert_eq!(*h.p.kiosk_users_removed.lock().unwrap(), vec![s(2)]);
    assert_eq!(call(&h, &mut a, RequestBody::RemoveKioskUser { seat: s(1) }).unwrap_err().code, ErrorCode::InvalidValue);
    let mut u = user(1);
    assert_eq!(call(&h, &mut u, RequestBody::RemoveKioskUser { seat: s(2) }).unwrap_err().code, ErrorCode::AccessDenied);
}

#[test]
fn kiosk_seat_pads_belong_to_the_seat_user_not_the_console_user() {
    use seat_core::backend::session::KioskUser;
    let mut c = two_seat_config();
    c.seats[1].local_mut().unwrap().virtual_pads = true;
    c.settings.input.pad_device_scope = true;
    let h = harness_with(c, None);
    let mut pad = ds5_dev("pad;2", r"\\?\HID#VID_054C&PID_0CE6&MI_03#8&2&0&0000#{4d1e55b2-f16f-11cf-88cb-001111000030}");
    pad.instance_id = Some(r"HID\VID_054C&PID_0CE6&MI_03\8&2&0&0000".into());
    *h.f.input.devices.lock().unwrap() = vec![input_dev("kbd;1", DeviceKind::Keyboard, KBD1), pad];
    h.s.boot();
    h.s.lock().input_overrides.insert(k("pad;2"), s(2).mask());
    let mut a = admin();
    call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(2)]) }).unwrap();
    h.p.sign_in(10, "ali");
    h.s.on_session_change(SessionChange::Logon, 10);
    let console_sid = h.s.lock().seats.get(&s(2)).and_then(|x| x.rt.user_sid.clone()).unwrap();
    // The seat's kiosk shell runs as its own Windows user.
    *h.f.session.kiosk_users.lock().unwrap() =
        vec![KioskUser { seat: s(2), user_sid: "S-1-5-21-9-9-9-1002".into(), pulsar_account: true }];
    let t = h.s.local_pad_targets();
    assert_eq!(t.len(), 1, "{t:?}");
    assert_eq!(t[0].owner_sid, "S-1-5-21-9-9-9-1002");
    assert_ne!(t[0].owner_sid, console_sid);
    let parent = |id: &str| Some(format!(r"USB\PARENT_OF_{}", id.rsplit('\\').next().unwrap_or("")));
    let want = h.s.pad_scope_plan(true, &parent).unwrap();
    let seat2: Vec<_> = want.iter().filter(|w| w.owner.user_sid =="S-1-5-21-9-9-9-1002").collect();
    assert_eq!(seat2.len(), 2, "collection + parent scoped to the kiosk user: {want:?}");
    // Without a seat logon the session user stays the owner.
    h.f.session.kiosk_users.lock().unwrap().clear();
    assert_eq!(h.s.local_pad_targets()[0].owner_sid, console_sid);
}

#[test]
fn pad_scope_seat_map_and_sdl_hints_follow_routing() {
    let mut c = two_seat_config();
    c.seats[1].pad_ignore_devices = vec!["0x1234/0x5678".into()];
    c.settings.input.pad_device_scope = true;
    let h = harness_with(c, None);
    let mut pad1 = ds5_dev("pad;1", r"\\?\HID#VID_054C&PID_0CE6&MI_03#8&1&0&0000#{4d1e55b2-f16f-11cf-88cb-001111000030}");
    pad1.instance_id = Some(r"HID\VID_054C&PID_0CE6&MI_03\8&1&0&0000".into());
    let mut pad2 = ds5_dev("pad;2", r"\\?\HID#VID_057E&PID_2009#8&2&0&0000#{4d1e55b2-f16f-11cf-88cb-001111000030}");
    pad2.instance_id = Some(r"HID\VID_057E&PID_2009\8&2&0&0000".into());
    if let seat_core::device::DeviceDetails::Input(i) = &mut pad2.details {
        (i.vendor_id, i.product_id) = (0x057E, 0x2009);
    }
    *h.f.input.devices.lock().unwrap() = vec![input_dev("kbd;1", DeviceKind::Keyboard, KBD1), pad1, pad2];
    h.s.boot();
    h.s.lock().input_overrides.insert(k("pad;2"), s(2).mask());
    let parent = |id: &str| Some(format!(r"USB\PARENT_OF_{}", id.rsplit('\\').next().unwrap_or("")));
    // Only the console session: nobody else could open the pads, nothing scoped.
    assert_eq!(h.s.pad_scope_plan(false, &parent).unwrap(), vec![]);
    // Opt-in: the default config never scopes physical pads.
    assert!(!seat_core::config::InputSettings::default().pad_device_scope);
    let mut a = admin();
    call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(2)]) }).unwrap();
    h.p.sign_in(10, "ali");
    h.s.on_session_change(SessionChange::Logon, 10);
    let want = h.s.pad_scope_plan(false, &parent).unwrap();
    let seat2: Vec<_> = want.iter().filter(|w| w.owner.session_id == 10).collect();
    assert_eq!(seat2.len(), 2, "the Switch Pad's collection and parent: {want:?}");
    assert!(seat2.iter().any(|w| w.instance_id.starts_with(r"USB\PARENT_OF_")));
    // Remote seats running: every pad has an owner (seat 2's, or seat 1 / the console).
    let want = h.s.pad_scope_plan(true, &parent).unwrap();
    assert_eq!(want.len(), 4, "{want:?}");
    assert_eq!(want.iter().filter(|w| w.owner.session_id == 1).count(), 2);
    // Setting off: plan None (the runtime restores everything).
    h.s.lock().config.settings.input.pad_device_scope = false;
    assert!(h.s.pad_scope_plan(true, &parent).is_none());
    // Shim seat map: seat 2 has its own session; its pad and the pad's parent are listed.
    let map = h.s.local_seat_map(&parent);
    assert_eq!(map.len(), 1, "{map:?}");
    assert_eq!(map[0].session_id, 10);
    assert_eq!(map[0].seat_id, seat_input::local_pads::local_seat_pad_id(2));
    assert!(map[0].pad_instance_prefixes.contains(&r"HID\VID_057E&PID_2009\8&2&0&0000".to_string()));
    assert!(map[0].pad_instance_prefixes.iter().any(|p| p.starts_with(r"USB\PARENT_OF_")));
    // SDL hints: seat 2 ignores its configured id and the DualSense routed elsewhere, but
    // not its own Switch Pad.
    let env = h.s.pad_sdl_env(s(2));
    let v = &env.iter().find(|(n, _)| n == "SDL_GAMECONTROLLER_IGNORE_DEVICES").unwrap().1;
    assert_eq!(v, "0x054C/0x0CE6,0x1234/0x5678");
    let env1 = h.s.pad_sdl_env(s(1));
    assert!(env1.iter().any(|(_, v)| v == "0x057E/0x2009"));
}
#[test]
fn pulsar_pairing_events_reach_admins_on_the_stream_topic_only() {
    use seat_proto::{Event, StreamEvent};
    let h = harness_with(with_remote_seat(), None);
    h.s.boot();
    let mut u = user(1);
    let mut a = admin();
    call(&h, &mut u, RequestBody::GetStatus).unwrap();
    call(&h, &mut a, RequestBody::GetStatus).unwrap();
    let after = h.s.events.last_seq();
    for e in [
        StreamEvent::OtpChanged(serde_json::json!({"backend": "pulsar"})),
        StreamEvent::TrustChanged(serde_json::json!({"backend": "pulsar"})),
        StreamEvent::RelayAuthRequired(serde_json::json!({"password": true, "totp": false, "backend": "pulsar"})),
    ] {
        h.s.events.publish(Event::Stream { seat: s(3), event: e });
    }
    let poll = |c: &mut Caller, topics: Vec<Topic>| {
        let body = RequestBody::PollEvents { after, wait_ms: 0, topics };
        call(&h, c, body)
    };
    let ResponseBody::Events(b) = poll(&mut a, vec![Topic::Stream]).unwrap() else { panic!() };
    let kinds: Vec<String> =
        b.events.iter().map(|f| serde_json::to_value(&f.event).unwrap()["data"]["event"]["type"].as_str().unwrap().to_string()).collect();
    assert_eq!(kinds, ["otp_changed", "trust_changed", "relay_auth_required"]);
    // No secret travels with them (the relay event says only what is asked for).
    assert!(!serde_json::to_string(&b.events).unwrap().contains("secret"));
    // A standard user cannot ask for the stream topic, and sees none of them otherwise.
    assert_eq!(poll(&mut u, vec![Topic::Stream]).unwrap_err().code, ErrorCode::AccessDenied);
    let ResponseBody::Events(b) = poll(&mut u, vec![]).unwrap() else { panic!() };
    assert!(b.events.is_empty(), "{:?}", b.events);
}

// ---- seat 1 kiosk shell (maintainer decision 2026-11-15, option a) ----------------------

/// `desktops` with `seat1_desktop = false` and a kiosk shell on seat 1.
fn seat1_kiosk_config(seat1_desktop: bool) -> Config {
    let mut c = two_seat_config();
    c.settings.backends.session = seat_core::backend::SessionBackendKind::Desktops;
    c.settings.kiosk.seat1_desktop = seat1_desktop;
    let app = seat_core::config::KioskShellApp {
        title: "Komut".into(),
        command: r"C:\Windows\System32\cmd.exe".into(),
        args: Vec::new(),
        autostart: true,
        monitor: 0,
        app_id: None,
        fullscreen: false,
    };
    c.seats[0].local = Some(seat_core::config::LocalSeat {
        kiosk_shell: Some(seat_core::config::KioskShell { apps: vec![app], idle_reset_minutes: 0 }),
        ..Default::default()
    });
    c
}

fn seat1_starts(h: &Harness) -> usize {
    h.f.desktops.rec.calls().iter().filter(|c| *c == "start 1").count()
}

fn seat1_stops(h: &Harness) -> usize {
    h.f.desktops.rec.calls().iter().filter(|c| *c == "log_off 1").count()
}

/// Started at boot, left alone while alive, restarted after a crash (after the back-off),
/// stopped on disable, started again on enable, stopped at service shutdown.
#[test]
fn seat1_kiosk_shell_follows_the_product() {
    let h = harness_with(seat1_kiosk_config(false), None);
    h.s.boot();
    assert_eq!(h.s.status().active.session, seat_core::backend::SessionBackendKind::Desktops);
    assert_eq!(seat1_starts(&h), 1, "{:?}", h.f.desktops.rec.calls());
    h.s.tick();
    assert_eq!(seat1_starts(&h), 1, "alive: not started again");
    // Crash: restarted after the back-off, not before.
    h.f.desktops.end_session(s(1));
    h.s.tick();
    h.s.tick();
    assert_eq!(seat1_starts(&h), 1);
    h.clock.advance(70_000);
    h.s.tick();
    assert_eq!(seat1_starts(&h), 2, "restarted after the back-off");
    // Disable stops it, enable starts it again.
    let mut a = admin();
    call(&h, &mut a, RequestBody::SetEnabled { enabled: false }).unwrap();
    assert_eq!(seat1_stops(&h), 1);
    h.s.tick();
    assert_eq!(seat1_starts(&h), 2, "not while disabled");
    call(&h, &mut a, RequestBody::SetEnabled { enabled: true }).unwrap();
    assert_eq!(seat1_starts(&h), 3);
    h.s.shutdown();
    assert_eq!(seat1_stops(&h), 2, "service stop cleans up");
}

/// The enable thread and the tick race on seat 1's shell: one start only (VM 2026-11-15:
/// disable + enable started two shells).
#[test]
fn seat1_kiosk_shell_concurrent_syncs_start_one_shell() {
    let h = harness_with(seat1_kiosk_config(false), None);
    h.s.boot();
    assert_eq!(seat1_starts(&h), 1);
    let mut a = admin();
    call(&h, &mut a, RequestBody::SetEnabled { enabled: false }).unwrap();
    assert_eq!(seat1_stops(&h), 1);
    h.f.desktops.start_delay_ms.store(100, std::sync::atomic::Ordering::SeqCst);
    call(&h, &mut a, RequestBody::SetEnabled { enabled: true }).unwrap();
    let before = seat1_starts(&h);
    std::thread::scope(|sc| {
        for _ in 0..3 {
            sc.spawn(|| h.s.sync_seat1_kiosk());
        }
        sc.spawn(|| h.s.tick());
    });
    assert_eq!(seat1_starts(&h), before, "running already: no second shell {:?}", h.f.desktops.rec.calls());
    assert_eq!(before, 2);
    // Not running yet when the racers arrive: still exactly one start.
    call(&h, &mut a, RequestBody::SetEnabled { enabled: false }).unwrap();
    let stopped = seat1_starts(&h);
    std::thread::scope(|sc| {
        sc.spawn(|| call(&h, &mut admin(), RequestBody::SetEnabled { enabled: true }).unwrap());
        for _ in 0..3 {
            sc.spawn(|| h.s.sync_seat1_kiosk());
        }
    });
    assert_eq!(seat1_starts(&h), stopped + 1, "{:?}", h.f.desktops.rec.calls());
}

/// A session-backend fallback chosen while the console had no user (boot before the
/// auto-logon) is reconsidered on enable and on a console logon (VM 2026-11-15).
#[test]
fn a_console_sign_in_fallback_is_reconsidered_on_enable_and_logon() {
    use seat_core::backend::{Availability, SessionBackendKind};
    let mut c = seat1_kiosk_config(true);
    c.settings.enabled = false;
    let h = harness_with(c, None);
    let not_ready = || Availability::NeedsSetup { what: "console_sign_in".into() };
    *h.f.desktops.availability.lock().unwrap() = not_ready();
    h.s.reselect_backends();
    h.s.boot();
    assert_eq!(h.s.status().active.session, SessionBackendKind::FastUserSwitch, "fallback while nobody is signed in");
    *h.f.desktops.availability.lock().unwrap() = Availability::Available;
    let mut a = admin();
    call(&h, &mut a, RequestBody::SetEnabled { enabled: true }).unwrap();
    assert_eq!(h.s.status().active.session, SessionBackendKind::Desktops, "enable re-selects");
    assert!(!h.s.status().fallback_notes.iter().any(|n| n.starts_with("session:")), "{:?}", h.s.status().fallback_notes);

    // Enabled, fallback again (console signed out), then the console user signs in.
    *h.f.desktops.availability.lock().unwrap() = not_ready();
    h.s.reselect_backends();
    assert_eq!(h.s.status().active.session, SessionBackendKind::FastUserSwitch);
    *h.f.desktops.availability.lock().unwrap() = Availability::Available;
    h.s.on_session_change(crate::seats::SessionChange::Logon, 1);
    assert_eq!(h.s.status().active.session, SessionBackendKind::Desktops, "console logon re-selects");

    // A fallback that is still there (the mock's unbuilt drive-letter gate): the same
    // kinds would come back, so the working backends are not rebuilt.
    let before = h.s.backends();
    h.s.on_session_change(crate::seats::SessionChange::Logon, 1);
    assert!(Arc::ptr_eq(&before.session, &h.s.backends().session));
}

/// Seat 1 kept as the desktop, or another session backend: no shell for seat 1.
#[test]
fn seat1_kiosk_shell_only_when_seat1_is_not_the_desktop() {
    let h = harness_with(seat1_kiosk_config(true), None);
    h.s.boot();
    h.s.tick();
    assert_eq!(seat1_starts(&h), 0);
    let mut c = seat1_kiosk_config(false);
    c.settings.backends.session = seat_core::backend::SessionBackendKind::RdsLoopback;
    let h = harness_with(c, None);
    h.s.boot();
    h.s.tick();
    assert_eq!(seat1_starts(&h), 0);
    assert!(h.f.session.rec.calls().iter().all(|c| c != "start 1"));
}

/// A shell that keeps dying is given up after the crash limit inside the window.
#[test]
fn seat1_kiosk_shell_gives_up_after_the_crash_limit() {
    let h = harness_with(seat1_kiosk_config(false), None);
    h.s.boot();
    for _ in 0..crate::seats::CRASH_LIMIT {
        h.f.desktops.end_session(s(1));
        h.s.tick();
        h.clock.advance(70_000);
        h.s.tick();
    }
    let n = seat1_starts(&h);
    h.clock.advance(70_000);
    h.s.tick();
    assert_eq!(seat1_starts(&h), n, "given up");
    assert!(h.s.lock().seat1_kiosk.gave_up);
}

// ---- kiosk console apps in conhost (maintainer decision 2026-11-15) ---------------------

fn kiosk_user(h: &Harness) -> String {
    let sid = "S-1-5-21-9-1000".to_string();
    let u = crate::platform::SessionUser { domain: "PC".into(), name: "kiosk".into(), sid: sid.clone(), logon_sid: "S-1-5-5-0-7".into() };
    h.p.users.lock().unwrap().insert(1, u);
    sid
}

/// Kiosk-only console (seat1_desktop = false): the console user's default terminal is
/// conhost while the product runs, journaled first, and exactly restored on disable.
#[test]
fn kiosk_console_user_gets_conhost_and_it_is_restored() {
    let h = harness_with(seat1_kiosk_config(false), None);
    let sid = kiosk_user(&h);
    let wt = (Some("{2EACA947-7F5F-4CFA-BA87-8F7FBEEFBE69}".to_string()), Some("{E12CFF52-A866-4C77-9A90-F570A7AA2C6B}".to_string()));
    h.p.delegation.lock().unwrap().insert(sid.clone(), wt.clone());
    h.s.boot();
    let conhost = Some(crate::kiosk_console::CONHOST.to_string());
    assert_eq!(h.p.delegation.lock().unwrap()[&sid], (conhost.clone(), conhost.clone()));
    let path = crate::kiosk_console::journal_path(&h.s.data_dir);
    assert_eq!(crate::kiosk_console::load(&path).unwrap().console, wt.0, "journaled before the change");
    let mut a = admin();
    call(&h, &mut a, RequestBody::SetEnabled { enabled: false }).unwrap();
    assert_eq!(h.p.delegation.lock().unwrap()[&sid], wt, "restored");
    assert!(crate::kiosk_console::load(&path).is_none());
}

/// Seat 1 is the console user's normal desktop: that user's setting is never touched.
#[test]
fn kiosk_console_left_alone_when_seat1_is_the_desktop() {
    let h = harness_with(seat1_kiosk_config(true), None);
    let sid = kiosk_user(&h);
    h.p.delegation.lock().unwrap().insert(sid.clone(), (None, None));
    h.s.boot();
    h.s.tick();
    assert_eq!(h.p.delegation.lock().unwrap()[&sid], (None, None));
}

/// After a crash the journal holds the original values: a new run must not replace them
/// with the conhost values it finds, and absent values come back as absent.
#[test]
fn kiosk_console_journal_survives_a_crash() {
    let h = harness_with(seat1_kiosk_config(false), None);
    let sid = kiosk_user(&h);
    let conhost = Some(crate::kiosk_console::CONHOST.to_string());
    // State after a crash: setting changed, journal says "both absent before".
    h.p.delegation.lock().unwrap().insert(sid.clone(), (conhost.clone(), conhost.clone()));
    let path = crate::kiosk_console::journal_path(&h.s.data_dir);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, format!(r#"{{"sid":"{sid}","console":null,"terminal":null}}"#)).unwrap();
    h.s.boot();
    assert_eq!(crate::kiosk_console::load(&path).unwrap().console, None, "original kept");
    h.s.shutdown();
    assert_eq!(h.p.delegation.lock().unwrap()[&sid], (None, None));
    assert!(crate::kiosk_console::load(&path).is_none());
}

/// Source IP per seat with the WFP callout (net-wfp): the signed-in seat's session goes
/// into the driver's map with its NT program list; `get_backends` reports the driver and
/// the source-IP state; without the driver the report says `environment` and why; a seat
/// that shares its session is left out (spec 80 §11.3).
#[test]
fn source_ip_map_reaches_the_driver() {
    use seat_compat::wfp::{SF_BIND, SF_CONNECT, WfpControl};
    use seat_proto::SourceIpEnforcement;
    let mut c = two_seat_config();
    c.seats[1].ip_binding = Some(seat_core::config::IpBinding { address: "192.168.1.52".into(), rule: seat_core::config::IpRule::OnlyListed });
    c.settings.ip_programs = vec![
        seat_core::config::IpProgram { title: "Oyun".into(), path: r"C:\Games\Lan".into(), enabled: true },
        seat_core::config::IpProgram { title: "Ağ".into(), path: r"Z:\Net\x.exe".into(), enabled: true },
        seat_core::config::IpProgram { title: "Kapalı".into(), path: r"C:\Off".into(), enabled: false },
    ];
    let h = harness_with(c, None);
    // Driver not loaded: the fallback only.
    h.s.boot();
    let mut a = admin();
    call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(2)]) }).unwrap();
    h.p.sign_in(10, "ali");
    h.s.on_session_change(SessionChange::Logon, 10);
    h.s.apply_ownership();
    let ResponseBody::Backends(r) = call(&h, &mut a, RequestBody::GetBackends).unwrap() else { panic!() };
    let ip = r.source_ip.clone().unwrap();
    assert_eq!((ip.enforcement, ip.reason.as_deref()), (SourceIpEnforcement::Environment, Some("driver_not_loaded")));
    assert!(r.drivers.iter().any(|d| d.driver == seat_core::backend::DriverId::SourceIpCallout));
    // Driver loaded: the next ownership run sends the map.
    {
        let mut st = h.p.wfp.state.lock().unwrap();
        st.loaded = true;
        st.major = seat_compat::wfp::INTERFACE_MAJOR;
    }
    h.s.apply_ownership();
    let m = h.p.wfp.seats();
    assert_eq!(m.len(), 1, "{m:?}");
    assert_eq!((m[0].seat, m[0].session_id, m[0].address.to_string()), (2, 10, "192.168.1.52".to_string()));
    assert_eq!(m[0].paths, vec![r"\device\harddiskvolume3\games\lan".to_string()]);
    assert_eq!(m[0].flags, SF_BIND | SF_CONNECT);
    let ResponseBody::Backends(r) = call(&h, &mut a, RequestBody::GetBackends).unwrap() else { panic!() };
    let ip = r.source_ip.unwrap();
    assert_eq!(ip.enforcement, SourceIpEnforcement::Driver);
    assert_eq!(ip.enforced, vec![s(2)]);
    assert_eq!(ip.dropped_paths, vec![r"Z:\Net\x.exe".to_string()]);
    // Unchanged: nothing sent again.
    let calls = h.p.wfp.set_calls();
    h.s.apply_ownership();
    assert_eq!(h.p.wfp.set_calls(), calls);
    // Multiseat off: the map is emptied.
    h.s.lock().config.settings.enabled = false;
    h.s.apply_ownership();
    assert!(h.p.wfp.seats().is_empty());
    assert_eq!(h.p.wfp.stats().unwrap().seat_count, 0);
}

/// Seats that share a session are never put into the map (the driver works per session).
#[test]
fn source_ip_entries_skip_shared_sessions() {
    let mut c = two_seat_config();
    c.seats.push(seat_core::config::SeatConfig::new(s(3)));
    for (i, a) in [(0, "192.168.1.51"), (1, "192.168.1.52"), (2, "192.168.1.53")] {
        c.seats[i].ip_binding = Some(seat_core::config::IpBinding { address: a.into(), rule: seat_core::config::IpRule::AllPrograms });
    }
    // Seats 1 and 3 in console session 1 (kiosk shell), seat 2 in session 10.
    let (e, shared) = crate::service::source_ip_entries(&[(s(1), 1), (s(2), 10), (s(3), 1)], &c);
    assert_eq!(e.iter().map(|x| (x.address.seat, x.session_id)).collect::<Vec<_>>(), vec![(s(2), 10)]);
    assert_eq!(shared, vec![s(1), s(3)]);
    // Session 0 is never a seat session; a seat without an address is skipped silently.
    c.seats[1].ip_binding = None;
    let (e, shared) = crate::service::source_ip_entries(&[(s(2), 10), (s(3), 0)], &c);
    assert!(e.is_empty());
    assert_eq!(shared, vec![s(3)]);
}

#[test]
fn gpu_preference_follows_the_seat_gpu_at_logon_and_reverts_on_disable() {
    use seat_core::config::{GpuPreference, GpuPreferenceMode};
    let mut c = two_seat_config();
    c.seats[1].local_mut().unwrap().gpu_preference = GpuPreference { mode: GpuPreferenceMode::SeatGpu, apps: vec![r"C:\Games\x\game.exe".into()] };
    let h = harness_with(c, None);
    h.s.boot();
    // The mock binds seat monitors on adapter LUID 1; Windows ranks it high performance.
    *h.p.gpu_ranks.lock().unwrap() = (Some(1), Some(2));
    let mut a = admin();
    call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(2)]) }).unwrap();
    let user = crate::platform::SessionUser { domain: "PC".into(), name: "ali".into(), sid: "S-1-5-21-1-2-3-1001".into(), logon_sid: "S-1-5-5-0-1".into() };
    h.s.on_seat_logon(s(2), 10, user.clone());
    assert_eq!(
        h.p.gpu_prefs.lock().unwrap().last().cloned(),
        Some(("S-1-5-21-1-2-3-1001".to_string(), vec![r"C:\Games\x\game.exe".to_string()], Some(2)))
    );
    // A single-GPU PC: nothing to choose, earlier values of the user are undone.
    *h.p.gpu_ranks.lock().unwrap() = (Some(1), Some(1));
    h.s.on_seat_logon(s(2), 10, user);
    assert_eq!(h.p.gpu_prefs.lock().unwrap().last().unwrap().2, None);
    // Disabling the product undoes every user's values.
    call(&h, &mut a, RequestBody::SetEnabled { enabled: false }).unwrap();
    assert!(h.p.gpu_pref_reverts.load(Ordering::SeqCst) >= 1);
}

/// Compat-tweaks (2026-11-17): deny-logon accounts and object rules follow the config
/// while enabled and are undone when the product is turned off.
#[test]
fn compat_options_follow_product_switch() {
    let mut c = two_seat_config();
    c.seats[1].deny_local_logon = true;
    c.settings.apps.object_rules =
        vec![seat_core::config::ObjectRule { exe: "Launcher.exe".into(), names: vec!["One".into()] }];
    let h = harness_with(c, None);
    h.s.boot();
    h.s.sync_tweaks();
    assert_eq!(h.p.deny_logon.lock().unwrap().last().cloned(), Some(vec!["S-1-5-21-9-ali".to_string()]));
    assert_eq!(h.p.object_rules.lock().unwrap().len(), 1);
    let mut a = admin();
    call(&h, &mut a, RequestBody::SetEnabled { enabled: false }).unwrap();
    h.s.sync_tweaks();
    assert_eq!(h.p.deny_logon.lock().unwrap().last().cloned(), Some(Vec::new()));
    assert!(h.p.object_rules.lock().unwrap().is_empty());
    assert!(h.p.user_env_reverts.load(Ordering::SeqCst) >= 1);
}

/// VM round 2 (2026-11-17): a config change of only `deny_local_logon` or only the object
/// rules is applied right away (before, only a `settings.tweaks` change ran the sync).
#[test]
fn compat_options_follow_an_apply_without_tweak_changes() {
    let h = harness_with(two_seat_config(), None);
    h.s.boot();
    h.s.sync_tweaks();
    let mut a = admin();
    let apply = |a: &mut _, f: &dyn Fn(&mut seat_core::config::Config)| {
        let base = h.s.status().config_revision;
        let ResponseBody::Config(view) = call(&h, a, RequestBody::GetConfig).unwrap() else { panic!() };
        let mut cfg = view.config.clone();
        f(&mut cfg);
        call(&h, a, RequestBody::ApplyConfig { config: Box::new(cfg), base_revision: base, force: false }).unwrap();
    };
    assert_eq!(h.p.deny_logon.lock().unwrap().last().cloned(), Some(Vec::new()));
    apply(&mut a, &|c| c.seats[1].deny_local_logon = true);
    assert_eq!(h.p.deny_logon.lock().unwrap().last().cloned(), Some(vec!["S-1-5-21-9-ali".to_string()]));
    assert!(h.p.object_rules.lock().unwrap().is_empty());
    apply(&mut a, &|c| {
        c.settings.apps.object_rules = vec![seat_core::config::ObjectRule { exe: "x.exe".into(), names: vec!["One".into()] }]
    });
    assert_eq!(h.p.object_rules.lock().unwrap().len(), 1);
    apply(&mut a, &|c| c.seats[1].deny_local_logon = false);
    assert_eq!(h.p.deny_logon.lock().unwrap().last().cloned(), Some(Vec::new()));
}

/// A seat that signs in locally never gets the deny right, even if the config says so.
#[test]
fn deny_logon_skips_seats_that_sign_in_locally() {
    let mut c = two_seat_config();
    c.seats[1].deny_local_logon = true;
    c.settings.backends.session = seat_core::backend::SessionBackendKind::FastUserSwitch;
    let h = harness_with(c, None);
    h.s.sync_tweaks();
    assert_eq!(h.p.deny_logon.lock().unwrap().last().cloned(), Some(Vec::new()));
}

/// Per-seat environment: written for the seat user at logon, announced by the agent, and
/// set on the startup apps; the agent is also asked for the DPI read-back.
#[test]
fn seat_environment_at_logon() {
    let mut c = two_seat_config();
    c.seats[1].environment = vec![seat_core::config::EnvVar { name: "GAME_PROFILE".into(), value: "seat2".into() }];
    c.seats[1].startup_apps = vec![seat_core::config::KioskApp { command: r"C:\Games\x.exe".into(), args: vec![] }];
    let h = harness_with(c, None);
    h.s.boot();
    let mut a = admin();
    call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(2)]) }).unwrap();
    h.p.sign_in(10, "ali");
    h.s.on_session_change(SessionChange::Logon, 10);
    let env = h.p.user_env.lock().unwrap().clone();
    assert!(env.iter().any(|(sid, v)| sid == "S-1-5-21-9-ali" && v.len() == 1), "{env:?}");
    let app = h.p.launched().into_iter().find(|x| x.exe.ends_with("x.exe")).unwrap();
    assert!(app.env.iter().any(|(n, v)| n == "GAME_PROFILE" && v == "seat2"));
    let tasks = h.s.lock().agent_tasks.get(&10).cloned().unwrap_or_default();
    assert!(tasks.iter().any(|t| t.task == AgentTaskKind::RefreshEnvironment));
    assert!(tasks.iter().any(|t| t.task == AgentTaskKind::ReportDpi));
}

/// VM round 2 (2026-11-17): a startup app with named-object rules (or Steam with Steam per
/// seat) starts through the shim launcher; other apps start as they are.
#[test]
fn startup_apps_with_object_rules_start_through_the_launcher() {
    let mut c = two_seat_config();
    c.settings.apps.object_rules = vec![seat_core::config::ObjectRule { exe: "Single.exe".into(), names: vec!["One".into()] }];
    c.seats[1].startup_apps = vec![
        seat_core::config::KioskApp { command: r"C:\Apps\single.exe".into(), args: vec!["-x".into()] },
        seat_core::config::KioskApp { command: r"C:\Apps\other.exe".into(), args: vec![] },
    ];
    let h = harness_with(c, None);
    h.s.boot();
    let mut a = admin();
    call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(2)]) }).unwrap();
    h.p.sign_in(10, "ali");
    h.s.on_session_change(SessionChange::Logon, 10);
    let launched = h.p.launched();
    let via = launched.iter().find(|x| x.args.iter().any(|a| a.ends_with("single.exe"))).expect("single.exe started");
    assert!(via.exe.ends_with(seat_shim_core::launch::native_launcher(seat_shim_core::launch::Arch::current())), "{via:?}");
    assert_eq!(via.args, vec!["--".to_string(), r"C:\Apps\single.exe".into(), "-x".into()]);
    assert!(launched.iter().any(|x| x.exe.ends_with("other.exe") && x.args.is_empty()));
}

/// The Steam proxy-DLL request is admin only and refuses paths that are not DLLs.
#[test]
fn steam_proxy_request() {
    use seat_proto::steam::SteamProxyAction as A;
    let h = harness();
    let mut a = admin();
    let ResponseBody::SteamProxy(st) = call(&h, &mut a, RequestBody::SteamProxyDll { action: A::Status }).unwrap() else { panic!() };
    assert_eq!(st.steam_dirs.len(), 1);
    let e = call(&h, &mut a, RequestBody::SteamProxyDll { action: A::Quarantine { path: r"C:\Windows\notepad.exe".into() } }).unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidValue);
    call(&h, &mut a, RequestBody::SteamProxyDll { action: A::Quarantine { path: r"C:\Steam\version.dll".into() } }).unwrap();
    assert_eq!(h.p.steam_actions.lock().unwrap().len(), 2);
    let mut u = user(5);
    assert!(call(&h, &mut u, RequestBody::SteamProxyDll { action: A::Status }).is_err());
}

/// Steam per local seat reaches the module's seat map flags.
#[test]
fn steam_per_local_seat_flag() {
    let mut c = two_seat_config();
    c.settings.apps.steam_per_seat = true;
    let h = harness_with(c, None);
    h.s.boot();
    let mut a = admin();
    call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(2)]) }).unwrap();
    h.p.sign_in(10, "ali");
    h.s.on_session_change(SessionChange::Logon, 10);
    let map = h.s.local_seat_map(&|_| None);
    assert!(map.iter().all(|e| e.flags.steam), "{map:?}");
}

/// VM round 2 (2026-11-17), fast user switching: the seat that holds the console gets an
/// identity-only entry (console flag, no isolation, no pads), so its single-instance names
/// and Steam override stay the same; seat 1 on the console gets none.
#[test]
fn console_seat_gets_an_identity_entry() {
    let mut c = two_seat_config();
    c.settings.apps.steam_per_seat = true;
    let h = harness_with(c, None);
    h.s.boot();
    let mut a = admin();
    call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(2)]) }).unwrap();
    h.p.sign_in(10, "ali");
    h.s.on_session_change(SessionChange::Logon, 10);
    let off = h.s.local_seat_map(&|_| None);
    assert!(off.iter().all(|e| !e.flags.console), "{off:?}");
    *h.p.console.lock().unwrap() = Some(10);
    let on = h.s.local_seat_map(&|_| None);
    let me = on.iter().find(|e| e.session_id == 10).expect("identity entry");
    assert_eq!(me.seat_id, seat_input::local_pads::local_seat_pad_id(2));
    assert!(me.flags.console && me.flags.steam && !me.flags.hid_isolation && !me.flags.xinput_focus);
    assert!(me.pad_instance_prefixes.is_empty());
    assert_eq!(seat_shim_core::seatmap::LOCAL_SEAT_ONE_ID, seat_input::local_pads::local_seat_pad_id(1));
    assert!(on.iter().all(|e| e.seat_id != seat_shim_core::seatmap::LOCAL_SEAT_ONE_ID || !e.flags.console));
}

// ---- Hyper-V VM seats (hyperv-seats 2026-11-17) ----

const VM_ID: &str = "0f8fad5b-d9cb-469f-a165-70867728950e";

/// `remote_ready_config` with seat 3 turned into a VM seat (shared folder, one forward).
fn vm_seat_config() -> Config {
    use seat_core::remote::vm::{ForwardedPort, PortProtocol, SharedFolder, VmSeat};
    let mut c = remote_ready_config();
    let three = c.seats.iter_mut().find(|x| x.number == s(3)).unwrap();
    three.autostart = false;
    three.remote.as_mut().unwrap().vm = Some(Box::new(VmSeat {
        shared_folders: vec![SharedFolder { id: "f".into(), host_path: r"E:\Games".into(), guest_path: r"C:\HostDrive".into(), read_only: true }],
        forwarded_ports: vec![ForwardedPort { id: "p".into(), protocol: PortProtocol::Tcp, host_port: 13389, guest_port: 3389, ..Default::default() }],
        ..Default::default()
    }));
    c
}

fn vm_status(h: &Harness, a: &mut Caller, action: seat_proto::hyperv::VmAction) -> Result<seat_proto::hyperv::VmHostStatus, seat_proto::ProtoError> {
    match call(h, a, RequestBody::VmSeats { action })? {
        ResponseBody::VmSeats(s) => Ok(*s),
        other => panic!("{other:?}"),
    }
}

#[test]
fn vm_seat_start_needs_hyperv_network_base_and_password() {
    use seat_proto::hyperv::VmAction as A;
    let h = harness_with(vm_seat_config(), None);
    h.s.boot();
    let mut a = admin();
    // The guest address was not assigned before the first save: assign it now.
    let ResponseBody::Config(view) = call(&h, &mut a, RequestBody::GetConfig).unwrap() else { panic!() };
    call(&h, &mut a, RequestBody::ApplyConfig { config: Box::new(view.config.clone()), base_revision: view.config.revision, force: true }).unwrap();
    let addr = h.s.lock().config.seat(s(3)).unwrap().remote.as_ref().unwrap().vm.as_ref().unwrap().guest_address.clone();
    assert_eq!(addr, "172.31.240.13");
    let start = |a: &mut Caller| call(&h, a, RequestBody::StartSeats { seats: Some(vec![s(3)]) }).unwrap_err();
    // No Hyper-V: refused before anything is created (no concurrent sessions needed).
    assert_eq!(start(&mut a).reason.as_deref(), Some("hyperv_unavailable"));
    h.p.vm_host_ready();
    h.p.vm_facts.lock().unwrap().nat_present = false;
    assert_eq!(start(&mut a).reason.as_deref(), Some("vm_network_missing"));
    // Preparing the network needs the confirmation.
    assert_eq!(vm_status(&h, &mut a, A::PrepareHost { confirm: false }).unwrap_err().code, ErrorCode::InvalidValue);
    let st = vm_status(&h, &mut a, A::PrepareHost { confirm: true }).unwrap();
    assert!(st.network_ready);
    h.p.vm_facts.lock().unwrap().base_present = false;
    assert_eq!(start(&mut a).reason.as_deref(), Some("vm_base_missing"));
    // Building needs an image.
    assert_eq!(vm_status(&h, &mut a, A::BuildBase { confirm: true }).unwrap_err().reason.as_deref(), Some("vm_no_image"));
    {
        let mut st = h.s.lock();
        let mut cfg = st.config.clone();
        cfg.settings.remote.vm.image_kind = seat_core::remote::vm::ImageKind::Wim;
        cfg.settings.remote.vm.image_path = r"D:\sources\install.wim".into();
        h.s.commit_config(&mut st, cfg).unwrap();
    }
    let st = vm_status(&h, &mut a, A::BuildBase { confirm: true }).unwrap();
    assert!(st.base_present && !st.building, "inline spawner: the build is done");
    assert_eq!(st.seats.len(), 1);
    assert_eq!(st.seats[0].state, "missing");
    // No password yet.
    assert_eq!(start(&mut a).code, ErrorCode::MissingCredentials);
    assert!(!h.p.vm_ops.lock().unwrap().iter().any(|o| matches!(o, seat_compat::hyperv_vm::VmOp::Ensure { .. })), "nothing created");
    let mut u = user(1);
    assert_eq!(call(&h, &mut u, RequestBody::VmSeats { action: A::Status }).unwrap_err().code, ErrorCode::AccessDenied);
    let journal: seat_compat::hyperv_vm::VmJournal =
        serde_json::from_slice(&std::fs::read(h.dir.join("journal").join(seat_compat::hyperv_vm::JOURNAL_FILE)).unwrap()).unwrap();
    assert!(journal.host_prepared && journal.base_built);
}

#[test]
fn vm_seat_runs_through_its_vm_and_guest_link() {
    use seat_compat::hyperv_vm::VmOp;
    use seat_core::remote::vm::{GuestMsg, GuestPhase};
    let h = harness_with(vm_seat_config(), None);
    h.s.boot();
    h.p.vm_host_ready();
    let mut a = admin();
    let ResponseBody::Config(view) = call(&h, &mut a, RequestBody::GetConfig).unwrap() else { panic!() };
    call(&h, &mut a, RequestBody::ApplyConfig { config: Box::new(view.config.clone()), base_revision: view.config.revision, force: true }).unwrap();
    call(&h, &mut a, RequestBody::SetSeatPassword { seat: s(3), password: Some(Secret::new("pw3")) }).unwrap();
    call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(3)]) }).unwrap();
    let ops = h.p.vm_ops.lock().unwrap().clone();
    let Some(VmOp::Ensure { spec }) = ops.iter().find(|o| matches!(o, VmOp::Ensure { .. })) else { panic!("{ops:?}") };
    assert_eq!(spec.seat_id, VM_ID);
    assert_eq!(spec.memory_mb, 16 * 1024, "50 % of 32 GB for the only VM seat");
    assert_eq!(spec.processors, 8);
    assert!(spec.forwards.iter().any(|f| f.host_port == 13389 && f.guest_port == 3389));
    assert!(spec.forwards.iter().any(|f| f.host_port == 48095));
    assert_eq!(spec.shares[0].host_path, r"E:\Games");
    assert!(ops.iter().any(|o| matches!(o, VmOp::Start { seat_id } if seat_id == VM_ID)));
    // The guest gets its config on the link: seat user, password, network, shares.
    let vm_id = format!("vm-{VM_ID}");
    let offer = h.p.guest_offers.lock().unwrap().get(&vm_id).cloned().unwrap();
    assert_eq!((offer.user_name.as_str(), offer.password.expose(), offer.address.as_str()), ("veli", "pw3", "172.31.240.13"));
    assert_eq!(offer.advertised_name, "Salon (VM)");
    assert_eq!(offer.shares[0].unc, r"\\172.31.240.1\PulsarVm0f8fad5b-0$");
    assert!(!offer.share_password.expose().is_empty());
    assert!(offer.check().is_ok());
    // Running VM, guest still configuring: starting. Then streaming: signed in.
    assert!(matches!(state(&h, s(3)), SeatState::Starting { .. }), "{:?}", state(&h, s(3)));
    h.p.guest_phases.lock().unwrap().insert(vm_id.clone(), (GuestPhase::Streaming, None));
    h.clock.advance(crate::vm_seats::VM_POLL_MS);
    h.s.tick();
    assert_eq!(state(&h, s(3)), SeatState::SignedIn { user: "veli".into() });
    // The seat page's requests go to the guest over the link (protocol 2 request / reply).
    call(&h, &mut a, RequestBody::SubmitPin { seat: s(3), pin: Secret::new("1234"), client_name: "tv".into(), request_id: None }).unwrap();
    {
        let rq = h.p.guest_requests.lock().unwrap();
        let (id, body) = &rq[0];
        assert_eq!(id, &vm_id);
        assert_eq!((body["type"].as_str(), body["data"]["pin"].as_str()), (Some("submit_pin"), Some("1234")));
    }
    h.p.guest_replies.lock().unwrap().insert(
        "list_paired_clients".into(),
        Ok(serde_json::json!({"type": "paired_clients", "data": [{"uuid": "u1", "name": "tv"}]})),
    );
    let ResponseBody::PairedClients(c) = call(&h, &mut a, RequestBody::ListPairedClients { seat: s(3) }).unwrap() else { panic!() };
    assert_eq!(c[0].uuid, "u1");
    call(&h, &mut a, RequestBody::UnpairClient { seat: s(3), uuid: "u1".into() }).unwrap();
    h.p.guest_replies.lock().unwrap().insert(
        "set_seat_display_mode".into(),
        Err(seat_proto::ProtoError::new(ErrorCode::ServiceUnavailable, "seat is not running")),
    );
    let e = call(&h, &mut a, RequestBody::SetSeatDisplayMode { seat: s(3), width: 1920, height: 1080, hz: 60, hdr: false }).unwrap_err();
    assert_eq!(e.code, ErrorCode::ServiceUnavailable, "the guest's error comes back as is");
    let types: Vec<String> = h.p.guest_requests.lock().unwrap().iter().map(|(_, b)| b["type"].as_str().unwrap_or_default().to_string()).collect();
    assert_eq!(types, ["submit_pin", "list_paired_clients", "unpair_client", "set_seat_display_mode"]);
    // Apps live on the host and reach the guest: defaults, then the stored list.
    let ResponseBody::Apps(apps) = call(&h, &mut a, RequestBody::GetApps { seat: s(3) }).unwrap() else { panic!() };
    assert!(!apps.is_empty(), "stream server defaults");
    let mine = vec![serde_json::json!({"name": "Steam", "cmd": r"C:\Steam\steam.exe"})];
    call(&h, &mut a, RequestBody::SetApps { seat: s(3), apps: mine.clone() }).unwrap();
    let ResponseBody::Apps(apps) = call(&h, &mut a, RequestBody::GetApps { seat: s(3) }).unwrap() else { panic!() };
    assert_eq!(apps[0]["name"], "Steam");
    assert_eq!(h.p.guest_requests.lock().unwrap().last().unwrap().1["type"], "set_apps", "handed to the running guest");
    let bad = vec![serde_json::json!({"cmd": 5})];
    assert_eq!(call(&h, &mut a, RequestBody::SetApps { seat: s(3), apps: bad }).unwrap_err().code, ErrorCode::InvalidValue);
    // Identity reset only while stopped.
    let e = call(&h, &mut a, RequestBody::RegenerateSeatIdentity { seat: s(3), confirm: true }).unwrap_err();
    assert_eq!(e.code, ErrorCode::SeatRunning);
    // The guest's stream notices become the seat's stream events.
    h.p.guest_notices.lock().unwrap().insert(vm_id.clone(), vec![("pair_pending".into(), serde_json::json!({"request_id": 2}))]);
    h.s.tick();
    assert!(h.p.guest_notices.lock().unwrap().get(&vm_id).is_none(), "drained");
    // A running VM seat cannot be started twice nor deleted.
    assert_eq!(call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(3)]) }).unwrap_err().code, ErrorCode::SeatRunning);
    let ResponseBody::Config(view) = call(&h, &mut a, RequestBody::GetConfig).unwrap() else { panic!() };
    let mut cfg = view.config.clone();
    cfg.seats.retain(|x| x.number != s(3));
    let del = RequestBody::ApplyConfig { config: Box::new(cfg), base_revision: view.config.revision, force: true };
    assert_eq!(call(&h, &mut a, del.clone()).unwrap_err().code, ErrorCode::SeatRunning);
    // Stop: shutdown over the link, then Stop-VM; off after the next poll.
    call(&h, &mut a, RequestBody::StopSeat { seat: s(3) }).unwrap();
    assert!(h.p.guest_sent.lock().unwrap().iter().any(|(_, m)| *m == GuestMsg::Shutdown));
    assert_eq!(h.p.vm_state(VM_ID).as_deref(), Some("off"));
    assert_eq!(state(&h, s(3)), SeatState::Off);
    assert!(h.p.guest_offers.lock().unwrap().is_empty(), "offer withdrawn");
    assert_eq!(call(&h, &mut a, RequestBody::StopSeat { seat: s(3) }).unwrap_err().code, ErrorCode::SeatNotRunning);
    // With the VM off: requests answer like an off host seat; the page keeps working.
    let ResponseBody::PairState(p) = call(&h, &mut a, RequestBody::GetPairState { seat: s(3) }).unwrap() else { panic!() };
    assert!(p.pending.is_none());
    assert!(matches!(call(&h, &mut a, RequestBody::GetRemoteRuntime { seat: s(3) }).unwrap(), ResponseBody::RemoteRuntime(_)));
    let ResponseBody::CredentialTest(t) = call(&h, &mut a, RequestBody::TestSeatCredentials { seat: s(3) }).unwrap() else { panic!() };
    assert!(t.ok, "the guest creates the user with the stored password");
    assert_eq!(call(&h, &mut a, RequestBody::ListPairedClients { seat: s(3) }).unwrap_err().code, ErrorCode::ServiceUnavailable);
    call(&h, &mut a, RequestBody::RegenerateSeatIdentity { seat: s(3), confirm: true }).unwrap();
    // Reset the seat's disk (stopped), then delete the seat: VM, disk, forwards, shares go.
    use seat_proto::hyperv::VmAction as A;
    vm_status(&h, &mut a, A::ResetSandbox { seat: 3, confirm: true }).unwrap();
    assert!(h.p.vm_ops.lock().unwrap().iter().any(|o| matches!(o, VmOp::ResetSandbox { .. })));
    call(&h, &mut a, del).unwrap();
    assert!(h.p.vm_ops.lock().unwrap().iter().any(|o| matches!(o, VmOp::Remove { spec } if spec.seat_id == VM_ID)));
    assert!(h.p.vm_state(VM_ID).is_none());
}

#[test]
fn vm_seats_stop_at_service_stop_and_suspend_and_failures_show() {
    use seat_core::remote::vm::GuestPhase;
    let mut c = vm_seat_config();
    c.seats.iter_mut().find(|x| x.number == s(3)).unwrap().autostart = true;
    c.seats.iter_mut().find(|x| x.number == s(3)).unwrap().remote.as_mut().unwrap().vm.as_mut().unwrap().guest_address = "172.31.240.13".into();
    c.settings.remote.vm.restart_gpus_on_shutdown = true;
    let h = harness_with(c, None);
    h.p.vm_host_ready();
    h.s.secrets.set(s(3), Some(&Secret::new("pw3"))).unwrap();
    h.s.boot();
    h.s.start_remote_autostart();
    assert_eq!(h.p.vm_state(VM_ID).as_deref(), Some("running"), "autostart VM seat");
    // A guest failure shows as the seat's error.
    h.p.guest_phases.lock().unwrap().insert(format!("vm-{VM_ID}"), (GuestPhase::Failed, Some("stream server missing".into())));
    h.s.vm_poll_now();
    assert!(matches!(state(&h, s(3)), SeatState::Error { detail, .. } if detail == "stream server missing"));
    h.s.on_suspend();
    assert_eq!(h.p.vm_state(VM_ID).as_deref(), Some("off"));
    assert!(h.p.vm_ops.lock().unwrap().iter().any(|o| matches!(o, seat_compat::hyperv_vm::VmOp::RestartGpus)), "GPU-P VM stopped");
    // Ensure failing: error state, no VM started.
    h.p.vm_fail.lock().unwrap().push("ensure".into());
    let mut a = admin();
    let e = call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(3)]) }).unwrap_err();
    assert_eq!(e.reason.as_deref(), Some("hyperv_failed"));
    assert!(matches!(state(&h, s(3)), SeatState::Error { .. }));
}

/// rdp-listener 2026-11-17: an NTFS volume of seat 2 goes to the seat user's drive broker;
/// the service links the host and the agent with fresh pipes and the agent's allowlist.
#[test]
fn drive_broker_links_the_host_with_the_seat_agent() {
    use seat_core::config::DriveRedirect;
    use seat_core::device::{DeviceDetails, StorageInfo};
    let mut c = two_seat_config();
    c.seats[1].local_mut().unwrap().redirect.drives = DriveRedirect::ReadOnly;
    c.devices.push(assign("stor;n", DeviceKind::Storage, s(2).mask()));
    let h = harness_with(c, None);
    let mut stor = device("stor;n", DeviceKind::Storage);
    stor.details = DeviceDetails::Storage(StorageInfo {
        removable: false,
        optical: false,
        volumes: vec!["\\\\?\\Volume{n}\\".into()],
        letters: vec!['Q'],
        file_system: "NTFS".into(),
    });
    *h.f.gate.devices.lock().unwrap() = vec![stor];
    h.s.boot();
    h.s.refresh_inventory();
    let mut a = admin();
    call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(2)]) }).unwrap();
    let sid = seat_session_id(&h, s(2));
    let e = h.s.host_drive_broker(s(2)).unwrap_err();
    assert_eq!((e.code, e.reason.as_deref()), (ErrorCode::NotReady, Some("not_ready")), "no agent yet");
    let host_pid = h.s.lock().seats.get(&s(2)).and_then(|x| x.host.as_ref().map(|h| h.pid)).unwrap();
    h.s.lock().seats.get_mut(&s(2)).unwrap().agent_pid = Some(77);
    let ResponseBody::DriveBroker { read, write } = h.s.host_drive_broker(s(2)).unwrap() else { panic!() };
    assert_eq!((read, write), (0x10, 0x14));
    assert_eq!(*h.p.broker_links.lock().unwrap(), vec![(host_pid, 77)]);
    let tasks = h.s.lock().agent_tasks.get(&sid).cloned().unwrap_or_default();
    let want = seat_proto::DriveBrokerDrive { root: "Q:\\".into(), volume: "\\\\?\\Volume{n}\\".into(), read_only: true };
    assert!(
        tasks.iter().any(|t| t.task == AgentTaskKind::DriveBroker { read: 0x20, write: 0x24, drives: vec![want.clone()] }),
        "{tasks:?}"
    );
    // A seat without ACL'd volumes gets no link.
    h.s.lock().config.seats[1].local_mut().unwrap().redirect.drives = DriveRedirect::Off;
    assert_eq!(h.s.host_drive_broker(s(2)).unwrap_err().reason.as_deref(), Some("no_acl_drives"));
}

/// rdp-listener 2026-11-17: the operator restart stops the seats first, then restarts
/// Remote Desktop Services; it needs the confirmation and an RDP session backend.
#[test]
fn termservice_restart_stops_the_seats_first() {
    let h = harness();
    let mut a = admin();
    assert_eq!(call(&h, &mut a, RequestBody::RestartTermService { confirm: false }).unwrap_err().code, ErrorCode::InvalidValue);
    call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(2)]) }).unwrap();
    assert!(state(&h, s(2)) != SeatState::Off);
    let ResponseBody::TermServiceRestart(r) = call(&h, &mut a, RequestBody::RestartTermService { confirm: true }).unwrap() else { panic!() };
    assert!(r.restarted);
    assert_eq!(r.stopped_seats, vec![s(2)]);
    assert_eq!(state(&h, s(2)), SeatState::Off);
    let calls = h.f.session.rec.calls();
    let logoff = calls.iter().position(|c| c.starts_with("log_off")).expect("seat signed out");
    let restart = calls.iter().position(|c| c == "restart_termservice").expect("restarted");
    assert!(logoff < restart, "{calls:?}");
    let mut u = user(1);
    assert_eq!(call(&h, &mut u, RequestBody::RestartTermService { confirm: true }).unwrap_err().code, ErrorCode::AccessDenied);
}

/// rdp-listener 2026-11-17: the host's audio notes reach the seat's runtime (manager).
#[test]
fn host_audio_notes_reach_the_seat_runtime() {
    let h = harness();
    let mut a = admin();
    call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(2)]) }).unwrap();
    h.s.on_host_audio_report(s(2), false, true, vec!["render_absent".into(), "Not A Code".into()], None);
    let rt = h.s.status().seats.into_iter().find(|r| r.seat == s(2)).unwrap();
    assert_eq!(rt.audio_notes, vec!["render_absent".to_string()]);
    h.s.on_host_audio_report(s(2), true, true, vec![], None);
    assert!(h.s.status().seats.into_iter().find(|r| r.seat == s(2)).unwrap().audio_notes.is_empty());
}

/// OSMS parity G4 (2026-11-18): under `dedicated_host` the account signed in at the console
/// gets the lock guard; nothing is undone while nobody is signed in; the service stop keeps
/// it.
#[test]
fn console_guard_follows_the_host_account() {
    use crate::platform::GuardOwner;
    let mut c = two_seat_config();
    c.settings.backends.host_topology = seat_core::backend::session::HostTopology::DedicatedHost;
    let h = harness_with(c, None);
    // Nobody at the console yet: the host guard is left alone.
    let console = h.p.console.lock().unwrap().take();
    h.s.boot();
    h.s.tick();
    assert!(!h.p.console_guard.lock().unwrap().iter().any(|(o, _)| *o == GuardOwner::Host), "{:?}", h.p.console_guard.lock().unwrap());
    *h.p.console.lock().unwrap() = console;
    h.p.sign_in(1, "host");
    h.s.tick();
    assert_eq!(h.p.console_guard.lock().unwrap().last().cloned(), Some((GuardOwner::Host, Some("S-1-5-21-9-host".to_string()))));
    let n = h.p.console_guard.lock().unwrap().len();
    h.s.tick();
    assert_eq!(h.p.console_guard.lock().unwrap().len(), n, "no call without a change");
    h.s.shutdown();
    assert!(!h.p.console_guard.lock().unwrap()[n..].iter().any(|(o, _)| *o == GuardOwner::Host));
}

#[test]
fn console_guard_host_is_undone_when_the_topology_changes() {
    use crate::platform::GuardOwner;
    let mut c = two_seat_config();
    c.settings.backends.host_topology = seat_core::backend::session::HostTopology::DedicatedHost;
    let h = harness_with(c, None);
    h.s.boot();
    h.p.sign_in(1, "host");
    h.s.tick();
    let mut a = admin();
    let base = h.s.status().config_revision;
    let ResponseBody::Config(view) = call(&h, &mut a, RequestBody::GetConfig).unwrap() else { panic!() };
    let mut cfg = view.config.clone();
    cfg.settings.backends.host_topology = seat_core::backend::session::HostTopology::SeatOneConsole;
    call(&h, &mut a, RequestBody::ApplyConfig { config: Box::new(cfg), base_revision: base, force: false }).unwrap();
    assert!(h.p.console_guard.lock().unwrap().contains(&(GuardOwner::Host, None)), "{:?}", h.p.console_guard.lock().unwrap());
}

/// Seat 1's opt-in guard: engaged while seat 2 runs, released when it stops and on service
/// stop; never without the opt-in.
#[test]
fn console_guard_seat_one_engages_while_seats_run() {
    use crate::platform::GuardOwner;
    let seat1 = |h: &Harness| {
        h.p.console_guard.lock().unwrap().iter().filter(|(o, _)| *o == GuardOwner::SeatOne).cloned().collect::<Vec<_>>()
    };
    // Without the opt-in: only releases.
    let h = harness();
    h.p.sign_in(1, "mehmet");
    let mut a = admin();
    call(&h, &mut a, RequestBody::StartSeats { seats: None }).unwrap();
    h.s.tick();
    assert!(seat1(&h).iter().all(|(_, s)| s.is_none()), "{:?}", seat1(&h));
    // With the opt-in.
    let mut c = two_seat_config();
    c.settings.console_guard.seat_one_lock_off = true;
    let h = harness_with(c, None);
    h.s.boot();
    h.p.sign_in(1, "mehmet");
    h.s.tick();
    assert_eq!(seat1(&h).last().cloned(), Some((GuardOwner::SeatOne, None)), "first sync releases a crash leftover");
    let mut a = admin();
    call(&h, &mut a, RequestBody::StartSeats { seats: None }).unwrap();
    h.s.tick();
    assert_eq!(seat1(&h).last().cloned(), Some((GuardOwner::SeatOne, Some("S-1-5-21-9-mehmet".to_string()))));
    call(&h, &mut a, RequestBody::StopSeat { seat: s(2) }).unwrap();
    h.s.tick();
    assert_eq!(seat1(&h).last().cloned(), Some((GuardOwner::SeatOne, None)));
    call(&h, &mut a, RequestBody::StartSeats { seats: None }).unwrap();
    h.s.tick();
    assert_eq!(seat1(&h).last().cloned().unwrap().1.as_deref(), Some("S-1-5-21-9-mehmet"));
    h.s.shutdown();
    assert_eq!(seat1(&h).last().cloned(), Some((GuardOwner::SeatOne, None)), "service stop releases");
}

/// Per-seat `no_power_menu` follows the config and the product switch.
#[test]
fn no_power_menu_follows_config() {
    let mut c = two_seat_config();
    c.seats[1].no_power_menu = true;
    let h = harness_with(c, None);
    h.s.boot();
    h.s.sync_tweaks();
    assert_eq!(h.p.no_power_menu.lock().unwrap().last().cloned(), Some(vec!["S-1-5-21-9-ali".to_string()]));
    let mut a = admin();
    call(&h, &mut a, RequestBody::SetEnabled { enabled: false }).unwrap();
    h.s.sync_tweaks();
    assert_eq!(h.p.no_power_menu.lock().unwrap().last().cloned(), Some(Vec::new()));
}

/// The preflight list carries the console checks of the topology.
#[test]
fn preflight_adds_console_checks() {
    let mut c = two_seat_config();
    c.settings.backends.host_topology = seat_core::backend::session::HostTopology::DedicatedHost;
    let h = harness_with(c, None);
    h.s.boot();
    *h.p.sleep_off.lock().unwrap() = Some(false);
    let mut a = admin();
    let ResponseBody::Checks(checks) = call(&h, &mut a, RequestBody::Preflight).unwrap() else { panic!() };
    let x = checks.iter().find(|c| c.id == "host_sleep_off").expect("host_sleep_off");
    assert_eq!(x.outcome, seat_proto::messages::CheckOutcome::Warn);
    assert!(checks.iter().any(|c| c.id == "console_lock"));
}

/// OSMS parity G7: the playback jitter-buffer counters reach the seat runtime, and a seat
/// stop clears them.
#[test]
fn host_audio_playback_stats_reach_the_seat_runtime() {
    let h = harness();
    let mut a = admin();
    call(&h, &mut a, RequestBody::StartSeats { seats: Some(vec![s(2)]) }).unwrap();
    let p = seat_core::state::AudioPlaybackStats { queued_ms: 31, target_ms: 30, max_ms: 100, underruns: 2, drops: 1, dropped_bytes: 13_440, ..Default::default() };
    h.s.on_host_audio_report(s(2), true, true, vec![], Some(p));
    let rt = h.s.status().seats.into_iter().find(|r| r.seat == s(2)).unwrap();
    assert!(rt.audio_notes.is_empty());
    assert_eq!(rt.audio_playback.as_deref(), Some(&p));
    // The wire form stays additive: an old report without `playback` still parses.
    let old: RequestBody = serde_json::from_str(r#"{"type":"host_audio_report","data":{"seat":2,"render_open":true}}"#).expect("old report parses");
    assert!(matches!(old, RequestBody::HostAudioReport { playback: None, .. }));
    call(&h, &mut a, RequestBody::StopSeat { seat: s(2) }).unwrap();
    assert_eq!(h.s.status().seats.into_iter().find(|r| r.seat == s(2)).unwrap().audio_playback, None);
}

/// `winusb_capture` (osms G2): USB input left on WinUSB is restored at boot, released at
/// service stop, and on the operator's `restore_usb_input` (confirm needed, admin only).
#[test]
fn usb_input_is_restored_at_boot_stop_and_on_request() {
    let h = harness();
    assert_eq!(h.f.usb_restores.load(Ordering::SeqCst), 1, "boot restores what an earlier run left");
    let mut a = admin();
    let e = call(&h, &mut a, RequestBody::RestoreUsbInput { confirm: false }).unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidValue);
    let mut u = user(1);
    assert_eq!(call(&h, &mut u, RequestBody::RestoreUsbInput { confirm: true }).unwrap_err().code, ErrorCode::AccessDenied);
    *h.f.usb_failures.lock().unwrap() = vec![r"USB\VID_1\1: unplugged".into()];
    let ResponseBody::UsbInputRestore(r) = call(&h, &mut a, RequestBody::RestoreUsbInput { confirm: true }).unwrap() else { panic!() };
    assert_eq!(r.failed, vec![r"USB\VID_1\1: unplugged".to_string()]);
    assert_eq!(h.f.usb_restores.load(Ordering::SeqCst), 2);
    h.s.shutdown();
    assert_eq!(h.f.usb_releases.load(Ordering::SeqCst), 1);
}
