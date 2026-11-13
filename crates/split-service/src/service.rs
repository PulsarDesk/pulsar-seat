// SPDX-License-Identifier: GPL-3.0-only
//! Wiring: store + engine + platform + controller + pipes + timer, and the SCM entry point.
//!
//! Threads: the control pipe (one thread per client), the shim health pipe, a 1 s timer
//! (engine tick, then controller report drain) and per-seat workers inside the engine.
//! The timer never holds the controller lock while the engine works.

use crate::engine::Engine;
use crate::health::HealthServer;
use crate::pipe::{Handler, PipeServer, Shutdown};
use crate::platform::WindowsPlatform;
use crate::registry_store::RegistryStore;
use crate::security;
use split_core::control::Controller;
use split_core::store::{ConfigStore, FileStore};
use split_proto::{MAX_POLL_WAIT_MS, PIPE_NAME, RequestBody, Response, ResponseBody};
use split_session::manager::SessionChange;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use windows_service::service::{
    PowerEventParam, ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::{define_windows_service, service_dispatcher};

pub const SERVICE_NAME: &str = "PulsarSplit";
pub const SERVICE_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone)]
pub struct Options {
    pub console: bool,
    /// Use `HKLM\SOFTWARE\PulsarSplit` (always on in service mode).
    pub registry: bool,
    pub data_dir: PathBuf,
    /// Dev only: also grant the current user access to the pipe.
    pub allow_current_user: bool,
    pub debug: bool,
}

impl Options {
    pub fn service_defaults() -> Self {
        Self { console: false, registry: true, data_dir: program_data(), allow_current_user: false, debug: false }
    }
}

pub fn program_data() -> PathBuf {
    std::env::var_os("ProgramData").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\ProgramData")).join("PulsarSplit")
}

pub fn dev_data_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("PulsarSplit-dev")
}

/// Folder of this executable (helper, stream-server manifest, shim payload sit next to it).
pub fn install_dir() -> PathBuf {
    std::env::current_exe().ok().and_then(|p| p.parent().map(PathBuf::from)).unwrap_or_else(|| PathBuf::from("."))
}

type SharedController = Arc<Mutex<Controller<Box<dyn ConfigStore>, WindowsPlatform>>>;

pub struct Running {
    pub shutdown: Shutdown,
    pub controller: SharedController,
    pub engine: Option<Engine>,
    server: PipeServer,
    health: Mutex<Option<HealthServer>>,
    health_shutdown: Option<Shutdown>,
    stop: Arc<AtomicBool>,
}

fn lock(c: &SharedController) -> std::sync::MutexGuard<'_, Controller<Box<dyn ConfigStore>, WindowsPlatform>> {
    c.lock().unwrap_or_else(|e| e.into_inner())
}

/// Build everything; the pipe name is claimed here (fails if already owned).
pub fn prepare(opts: &Options) -> Result<Running, String> {
    crate::log::init(Some(&opts.data_dir.join("logs").join("service.log")), opts.debug);
    crate::log::init_bridge();
    let store: Box<dyn ConfigStore> = if opts.registry {
        Box::new(RegistryStore::open_machine().map_err(|e| format!("registry store: {e}"))?)
    } else {
        let path = opts.data_dir.join("config.json");
        crate::log::warn(&format!("dev file store {} (passwords are kept in memory only)", path.display()));
        Box::new(FileStore::open(path).map_err(|e| format!("file store: {e}"))?)
    };
    let global = store.load().map(|c| c.global).unwrap_or_default();
    let engine = match crate::wiring::build(&opts.data_dir, &install_dir(), &global, SERVICE_VERSION, opts.registry) {
        Ok(d) => Some(Engine::new(d)),
        Err(e) => {
            crate::log::error(&format!("seat runtime not available: {e}"));
            None
        }
    };
    let platform = WindowsPlatform { config_dir: opts.data_dir.join("config"), engine: engine.clone(), global };
    let controller = Arc::new(Mutex::new(Controller::new(store, platform, SERVICE_VERSION, opts.console)));

    let mut sddl = security::PIPE_SDDL.to_string();
    if opts.allow_current_user {
        let sid = security::current_user_sid().map_err(|e| e.to_string())?;
        crate::log::warn(&format!("DEV: pipe also open to the current user {sid}"));
        sddl.push_str(&format!("(A;;GA;;;{sid})"));
    }
    let server = PipeServer::new(PIPE_NAME, &sddl).map_err(|e| format!("pipe: {e}"))?;
    let health = match HealthServer::new() {
        Ok(h) => Some(h),
        Err(e) => {
            crate::log::warn(&format!("shim health pipe not available: {e}"));
            None
        }
    };
    let health_shutdown = health.as_ref().map(HealthServer::shutdown_handle);
    Ok(Running {
        shutdown: server.shutdown_handle(),
        controller,
        engine,
        server,
        health: Mutex::new(health),
        health_shutdown,
        stop: Arc::default(),
    })
}

/// `poll_events` waits outside the controller lock (≤ 2 s) so other requests keep flowing.
fn handle(c: &SharedController, req: split_proto::Request) -> Response {
    if let RequestBody::PollEvents { after, wait_ms } = &req.body
        && req.version == split_proto::PROTOCOL_VERSION
    {
        let (after, wait) = (*after, (*wait_ms).min(MAX_POLL_WAIT_MS));
        let end = std::time::Instant::now() + Duration::from_millis(wait as u64);
        loop {
            {
                let g = lock(c);
                if g.has_events_after(after) || std::time::Instant::now() >= end {
                    return Response::new(req.id, Ok(ResponseBody::Events(g.events_after(after))));
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    lock(c).handle(req)
}

impl Running {
    /// Service start sequence (spec 20 §5, contracts §9.4) on a background thread:
    /// coexistence, termwrap boot check, RDS prepare + ready, sweeps, then the seats marked
    /// "start with service". The pipe serves meanwhile.
    pub fn start_background(&self) {
        let c = self.controller.clone();
        let engine = self.engine.clone();
        std::thread::spawn(move || {
            if let Some(e) = &engine {
                let (global, users) = {
                    let mut g = lock(&c);
                    (g.call(RequestBody::GetGlobalSettings), g.seat_users())
                };
                let global = match global {
                    Ok(ResponseBody::GlobalSettings(g)) => g,
                    _ => Default::default(),
                };
                e.service_start(&global, &users);
            }
            let names = lock(&c).autostart_seat_names();
            for name in names {
                match lock(&c).call(RequestBody::StartSeat { name: name.clone() }) {
                    Ok(_) => crate::log::info(&format!("seat '{name}' starting")),
                    Err(e) => crate::log::error(&format!("seat '{name}' not started: {e}")),
                }
            }
        });
        self.start_timer();
        self.start_pad_pump();
        self.start_health();
    }

    fn start_timer(&self) {
        let (c, engine, stop) = (self.controller.clone(), self.engine.clone(), self.stop.clone());
        std::thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_secs(1));
                if let Some(e) = &engine {
                    e.tick(crate::wiring::now_ms());
                }
                lock(&c).drain_reports();
            }
        });
    }

    /// Pad input pump: every 4 ms (HYPOTHESIS: enough for controller input; the pads
    /// package also offers event handles to wait on, open question integrator-2).
    fn start_pad_pump(&self) {
        let (Some(e), stop) = (self.engine.clone(), self.stop.clone()) else { return };
        if !e.has_pads() {
            return;
        }
        std::thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                e.pump_pads();
                std::thread::sleep(Duration::from_millis(4));
            }
        });
    }

    fn start_health(&self) {
        let Some(e) = self.engine.clone() else { return };
        let Some(h) = self.health.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
        std::thread::spawn(move || {
            if let Err(err) = h.run(e) {
                crate::log::warn(&format!("shim health pipe stopped: {err}"));
            }
        });
    }

    /// Serve the control pipe on this thread until shutdown.
    pub fn serve(&self) -> std::io::Result<()> {
        let c = self.controller.clone();
        let handler: Handler = Arc::new(move |req| handle(&c, req));
        crate::log::info(&format!("control pipe {PIPE_NAME} ready"));
        self.server.run(handler)
    }

    pub fn on_session_change(&self, ev: SessionChange) {
        if let Some(e) = &self.engine {
            e.on_session_change(ev);
        }
    }

    /// Power events (spec 20 §7, spec 30 §10): seats stop on suspend; after resume the
    /// enabled seats start again once the old ones finished stopping. Runs off the SCM
    /// handler thread.
    pub fn on_power(&self, action: PowerAction) {
        let c = self.controller.clone();
        match action {
            PowerAction::None => {}
            PowerAction::StopSeats => {
                std::thread::spawn(move || {
                    let stopped = lock(&c).on_suspend();
                    crate::log::info(&format!("suspend: stopping {} seat(s)", stopped.len()));
                });
            }
            PowerAction::StartEnabled => {
                std::thread::spawn(move || {
                    let end = std::time::Instant::now() + crate::engine::STOP_WAIT;
                    loop {
                        let active = {
                            let mut g = lock(&c);
                            g.drain_reports();
                            g.active_seats()
                        };
                        if active == 0 || std::time::Instant::now() >= end {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(500));
                    }
                    let names = lock(&c).resume_seat_names();
                    for name in names {
                        match lock(&c).call(RequestBody::StartSeat { name: name.clone() }) {
                            Ok(_) => crate::log::info(&format!("resume: seat '{name}' starting")),
                            Err(e) => crate::log::error(&format!("resume: seat '{name}' not started: {e}")),
                        }
                    }
                });
            }
        }
    }

    /// Stop seats, wait, sweep (contracts §9.4).
    pub fn stop_all(&self) {
        self.stop.store(true, Ordering::SeqCst);
        let users = {
            let mut g = lock(&self.controller);
            g.stop_all();
            g.seat_users()
        };
        if let Some(e) = &self.engine {
            e.service_stop(&users);
        }
        lock(&self.controller).drain_reports();
        if let Some(h) = &self.health_shutdown {
            h.trigger();
        }
    }
}

/// What a power event means for the seats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerAction {
    None,
    StopSeats,
    StartEnabled,
}

/// `PBT_APMSUSPEND` stops the seats; `PBT_APMRESUMEAUTOMATIC` (always sent on resume, with
/// or without a user) starts the enabled ones. `PBT_APMRESUMESUSPEND` follows it when a
/// user is present and is ignored so seats do not start twice.
pub fn power_action(p: &PowerEventParam) -> PowerAction {
    match p {
        PowerEventParam::Suspend => PowerAction::StopSeats,
        PowerEventParam::ResumeAutomatic => PowerAction::StartEnabled,
        _ => PowerAction::None,
    }
}

define_windows_service!(ffi_service_main, service_main);

pub fn run_dispatcher() -> windows_service::Result<()> {
    service_dispatcher::start(SERVICE_NAME, ffi_service_main)
}

fn service_main(_args: Vec<OsString>) {
    if let Err(e) = run_service() {
        crate::log::error(&format!("service failed: {e}"));
    }
}

fn status(state: ServiceState, accept: ServiceControlAccept, code: u32, wait: Duration) -> ServiceStatus {
    ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: accept,
        exit_code: ServiceExitCode::Win32(code),
        checkpoint: 0,
        wait_hint: wait,
        process_id: None,
    }
}

fn run_service() -> Result<(), String> {
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let running: Arc<Mutex<Option<Arc<Running>>>> = Arc::default();
    let r2 = running.clone();
    let handler = move |ev| match ev {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            let _ = tx.send(());
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::SessionChange(p) => {
            if let Some(r) = r2.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
                r.on_session_change(SessionChange::from_raw(p.reason as u32, p.notification.session_id));
            }
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::PowerEvent(p) => {
            if let Some(r) = r2.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
                r.on_power(power_action(&p));
            }
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    };
    let sh = service_control_handler::register(SERVICE_NAME, handler).map_err(|e| e.to_string())?;
    let none = ServiceControlAccept::empty();
    let _ = sh.set_service_status(status(ServiceState::StartPending, none, 0, Duration::from_secs(10)));

    crate::log::enable_event_log();
    let r = match prepare(&Options::service_defaults()) {
        Ok(r) => Arc::new(r),
        Err(e) => {
            let _ = sh.set_service_status(status(ServiceState::Stopped, none, 1, Duration::ZERO));
            return Err(e);
        }
    };
    *running.lock().unwrap_or_else(|e| e.into_inner()) = Some(r.clone());
    let srv = r.clone();
    let pipe_thread = std::thread::spawn(move || {
        if let Err(e) = srv.serve() {
            crate::log::error(&format!("control pipe stopped: {e}"));
        }
    });
    let accept = ServiceControlAccept::STOP
        | ServiceControlAccept::SHUTDOWN
        | ServiceControlAccept::SESSION_CHANGE
        | ServiceControlAccept::POWER_EVENT;
    let _ = sh.set_service_status(status(ServiceState::Running, accept, 0, Duration::ZERO));
    crate::log::info(&format!("service {SERVICE_VERSION} running"));
    r.start_background();

    let _ = rx.recv();
    let _ = sh.set_service_status(status(ServiceState::StopPending, none, 0, Duration::from_secs(90)));
    r.stop_all();
    r.shutdown.trigger();
    let _ = pipe_thread.join();
    crate::log::info("service stopped");
    let _ = sh.set_service_status(status(ServiceState::Stopped, none, 0, Duration::ZERO));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn power_events_map_to_seat_actions() {
        assert_eq!(power_action(&PowerEventParam::Suspend), PowerAction::StopSeats);
        assert_eq!(power_action(&PowerEventParam::ResumeAutomatic), PowerAction::StartEnabled);
        assert_eq!(power_action(&PowerEventParam::ResumeSuspend), PowerAction::None, "no double start");
        assert_eq!(power_action(&PowerEventParam::PowerStatusChange), PowerAction::None);
    }
}
