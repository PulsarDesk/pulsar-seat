// SPDX-License-Identifier: GPL-3.0-only
//! SCM integration (spec 20 §13.1): service main with stop, pre-shutdown, power-event and
//! session-change controls; and the `install` / `uninstall` / `enable` / `disable` service
//! record operations (description, recovery actions, unrestricted SID type, required
//! privileges, explicit security descriptor, Event Log source, data folders with ACLs).
//!
//! Untested on hardware: everything that needs elevation (install, uninstall, running
//! under the SCM). `--console` mode exercises the same runtime without the SCM.

use crate::runtime::{Options, Running};
use crate::seats::SessionChange;
use crate::win::security::SecurityDescriptor;
use crate::{eventid, log};
use seat_core::consts::{SERVICE_DISPLAY_NAME, SERVICE_NAME};
use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use windows_service::service::{
    PowerEventParam, ServiceAccess, ServiceAction, ServiceActionType, ServiceControl, ServiceControlAccept,
    ServiceDependency, ServiceErrorControl, ServiceExitCode, ServiceFailureActions, ServiceFailureResetPeriod,
    ServiceInfo, ServiceSidType, ServiceStartType, ServiceState, ServiceStatus, ServiceType, SessionChangeReason,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
use windows_service::{define_windows_service, service_dispatcher};
use windows_sys::Win32::Security::DACL_SECURITY_INFORMATION;
use windows_sys::Win32::System::Services::{
    ChangeServiceConfig2W, SERVICE_CONFIG_REQUIRED_PRIVILEGES_INFO, SERVICE_REQUIRED_PRIVILEGES_INFOW,
    SetServiceObjectSecurity,
};

/// Explicit service DACL: SYSTEM and Administrators as the Windows default; interactive
/// and service logons may only query (no start / stop, **no user-defined control codes**,
/// spec 00 §5.5).
pub const SERVICE_SDDL: &str =
    "D:(A;;CCLCSWRPWPDTLOCRRC;;;SY)(A;;CCDCLCSWRPWPDTLOCRSDRCWDWO;;;BA)(A;;CCLCSWLORC;;;IU)(A;;CCLCSWLORC;;;SU)";

/// Privileges the service keeps (everything else LocalSystem has is removed from its
/// token). HYPOTHESIS: complete for the default backends; re-check when a backend needs
/// more (e.g. driver load, which we never do).
pub const REQUIRED_PRIVILEGES: [&str; 8] = [
    "SeAssignPrimaryTokenPrivilege", // CreateProcessAsUser
    "SeIncreaseQuotaPrivilege",      // CreateProcessAsUser
    "SeTcbPrivilege",                // WTSQueryUserToken, token session ids
    "SeImpersonatePrivilege",        // ImpersonateNamedPipeClient
    "SeShutdownPrivilege",           // request_reboot
    "SeChangeNotifyPrivilege",
    "SeCreateGlobalPrivilege",       // global named objects
    "SeSecurityPrivilege",           // device security descriptors (gates)
];

/// SCM recovery: one restart per allowed crashed start, then nothing. With the default
/// `crash_loop_limit` of 3, the starts are: first start, restart 1, restart 2 (each counted
/// as unstable when it crashes), restart 3 finds three unstable starts and stays in the
/// crash-loop safe mode (status only). A fourth failure gets no restart. The SCM repeats
/// the last action for later failures, so the list ends with `None`.
pub fn recovery_actions() -> Vec<ServiceAction> {
    let restart = |s| ServiceAction { action_type: ServiceActionType::Restart, delay: Duration::from_secs(s) };
    vec![restart(5), restart(30), restart(60), ServiceAction { action_type: ServiceActionType::None, delay: Duration::ZERO }]
}

/// Turkish service description (UI copy).
pub const SERVICE_DESCRIPTION: &str =
    "Pulsar Seat: bu bilgisayarda birden fazla oturma yeri (monitör, klavye, fare, ses) yönetir. Durdurulursa ek oturma yerleri başlatılmaz.";

fn service_info(exe: &Path) -> ServiceInfo {
    ServiceInfo {
        name: OsString::from(SERVICE_NAME),
        display_name: OsString::from(SERVICE_DISPLAY_NAME),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: exe.to_path_buf(),
        launch_arguments: vec![OsString::from("--service")],
        dependencies: vec![ServiceDependency::Service("RpcSs".into()), ServiceDependency::Service("LSM".into())],
        account_name: None,
        account_password: None,
    }
}

fn s<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

/// Create or update the service record (elevated).
pub fn install(exe: &Path, data_dir: &Path) -> Result<(), String> {
    let m = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE)
        .map_err(s)?;
    let info = service_info(exe);
    let svc = match m.open_service(SERVICE_NAME, ServiceAccess::ALL_ACCESS) {
        Ok(svc) => {
            svc.change_config(&info).map_err(s)?;
            svc
        }
        Err(_) => m.create_service(&info, ServiceAccess::ALL_ACCESS).map_err(s)?,
    };
    configure(&svc, data_dir)
}

/// Service record settings the MSI cannot set itself: Windows Installer opens the service
/// without `SERVICE_START`, so `ChangeServiceConfig2(SERVICE_CONFIG_FAILURE_ACTIONS)` with
/// restart actions fails there with error 5 (seen in the ARM64 test VM on 2026-11-14, MSI
/// error 1939). The MSI creates the service; `install --msi` calls this for the rest.
pub fn configure_existing(data_dir: &Path) -> Result<(), String> {
    let m = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT).map_err(s)?;
    let svc = m.open_service(SERVICE_NAME, ServiceAccess::ALL_ACCESS).map_err(s)?;
    configure(&svc, data_dir)
}

fn configure(svc: &windows_service::service::Service, data_dir: &Path) -> Result<(), String> {
    svc.set_description(SERVICE_DESCRIPTION).map_err(s)?;
    svc.update_failure_actions(ServiceFailureActions {
        reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(24 * 3600)),
        reboot_msg: None,
        command: None,
        actions: Some(recovery_actions()),
    })
    .map_err(s)?;
    svc.set_failure_actions_on_non_crash_failures(true).map_err(s)?;
    svc.set_config_service_sid_info(ServiceSidType::Unrestricted).map_err(s)?;
    svc.set_preshutdown_timeout(Duration::from_secs(120)).map_err(s)?;
    let mut privs: Vec<u16> = REQUIRED_PRIVILEGES.iter().flat_map(|p| p.encode_utf16().chain(std::iter::once(0))).collect();
    privs.push(0);
    let mut info = SERVICE_REQUIRED_PRIVILEGES_INFOW { pmszRequiredPrivileges: privs.as_mut_ptr() };
    // SAFETY: open service handle with CHANGE_CONFIG; multi-sz buffer outlives the call.
    if unsafe { ChangeServiceConfig2W(svc.raw_handle(), SERVICE_CONFIG_REQUIRED_PRIVILEGES_INFO, &mut info as *mut _ as *mut _) } == 0 {
        return Err(format!("required privileges: {}", std::io::Error::last_os_error()));
    }
    let sd = SecurityDescriptor::from_sddl(SERVICE_SDDL).map_err(s)?;
    // SAFETY: handle opened with WRITE_DAC (ALL_ACCESS); descriptor valid.
    if unsafe { SetServiceObjectSecurity(svc.raw_handle(), DACL_SECURITY_INFORMATION, sd.as_ptr()) } == 0 {
        return Err(format!("service security: {}", std::io::Error::last_os_error()));
    }
    crate::win::eventlog::register_source().map_err(|e| format!("event source: {e}"))?;
    crate::runtime::ensure_data_dirs(data_dir, true).map_err(|e| format!("data folder: {e}"))?;
    Ok(())
}

/// Ask a running service to stop and wait up to 2 minutes. `Ok(true)` = stopped (or not
/// installed), `Ok(false)` = still running.
fn stop_and_wait(svc: &windows_service::service::Service) -> bool {
    if let Ok(st) = svc.query_status()
        && st.current_state != ServiceState::Stopped
    {
        let _ = svc.stop();
        for _ in 0..240 {
            if svc.query_status().map(|x| x.current_state == ServiceState::Stopped).unwrap_or(true) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        return false;
    }
    true
}

/// Stop the service (uninstall, before anything its running hosts depend on is removed).
/// `Ok(true)` = stopped or not installed; `Ok(false)` = still running after 2 minutes.
pub fn stop() -> Result<bool, String> {
    let m = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT).map_err(s)?;
    match m.open_service(SERVICE_NAME, ServiceAccess::QUERY_STATUS | ServiceAccess::STOP) {
        Ok(svc) => Ok(stop_and_wait(&svc)),
        // Not installed (or already deleted): nothing runs.
        Err(windows_service::Error::Winapi(e)) if e.raw_os_error() == Some(1060) => Ok(true),
        Err(e) => Err(s(e)),
    }
}

/// Stop and delete the service record and the Event Log source. Data stays unless the
/// caller removes it.
pub fn uninstall() -> Result<(), String> {
    let m = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT).map_err(s)?;
    match m.open_service(SERVICE_NAME, ServiceAccess::ALL_ACCESS) {
        Ok(svc) => {
            stop_and_wait(&svc);
            svc.delete().map_err(s)?;
        }
        Err(e) => log::warn(&format!("service not found: {e}")),
    }
    crate::win::eventlog::unregister_source().map_err(s)
}

/// Set the start type (enable = automatic and start now; disable = manual, keep running
/// so status stays visible).
pub fn set_start(enable: bool) -> Result<(), String> {
    let m = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT).map_err(s)?;
    let svc = m.open_service(SERVICE_NAME, ServiceAccess::ALL_ACCESS).map_err(s)?;
    let mut info = svc.query_config().map_err(s)?;
    let exe = info.executable_path.clone();
    info.start_type = if enable { ServiceStartType::AutoStart } else { ServiceStartType::OnDemand };
    let mut si = service_info(&exe);
    si.start_type = info.start_type;
    svc.change_config(&si).map_err(s)?;
    if enable && svc.query_status().map(|x| x.current_state == ServiceState::Stopped).unwrap_or(false) {
        svc.start::<&OsStr>(&[]).map_err(s)?;
    }
    Ok(())
}

define_windows_service!(ffi_service_main, service_main);

pub fn run_dispatcher() -> windows_service::Result<()> {
    service_dispatcher::start(SERVICE_NAME, ffi_service_main)
}

fn service_main(_args: Vec<OsString>) {
    if let Err(e) = run_service() {
        log::error(&format!("service failed: {e}"));
    }
}

fn status(state: ServiceState, accept: ServiceControlAccept, code: u32, wait: Duration, checkpoint: u32) -> ServiceStatus {
    ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: accept,
        exit_code: ServiceExitCode::Win32(code),
        checkpoint,
        wait_hint: wait,
        process_id: None,
    }
}

fn session_change(r: SessionChangeReason) -> Option<SessionChange> {
    Some(match r {
        SessionChangeReason::SessionLogon => SessionChange::Logon,
        SessionChangeReason::SessionLogoff => SessionChange::Logoff,
        SessionChangeReason::SessionLock => SessionChange::Lock,
        SessionChangeReason::SessionUnlock => SessionChange::Unlock,
        SessionChangeReason::SessionTerminate => SessionChange::Terminate,
        SessionChangeReason::ConsoleConnect => SessionChange::ConsoleConnect,
        _ => return None,
    })
}

/// Notifications handled in arrival order on one worker thread (a Lock / Unlock or Logon /
/// Logoff pair must not be applied in reverse).
enum Notice {
    /// Seat's mapped change (if it handles that reason), the raw `WTS_*` reason (remote
    /// engine) and the session id.
    Session(Option<SessionChange>, u32, u32),
    Suspend,
    Resume,
}

/// What a power event means (ported from Pulsar Split): `PBT_APMSUSPEND` stops the remote
/// seats (local seats keep their sessions); `PBT_APMRESUMEAUTOMATIC` (always sent on
/// resume, with or without a user) re-checks devices and starts the remote autostart seats
/// again. `PBT_APMRESUMESUSPEND` follows it when a user is present and is ignored so seats
/// do not start twice.
fn power_notice(p: &PowerEventParam) -> Option<Notice> {
    match p {
        PowerEventParam::Suspend => Some(Notice::Suspend),
        PowerEventParam::ResumeAutomatic => Some(Notice::Resume),
        _ => None,
    }
}

/// Status updates while stopping: wait hint per update and how often to send one.
const STOP_WAIT_HINT: Duration = Duration::from_secs(30);
const STOP_REPORT_EVERY: Duration = Duration::from_secs(3);

fn run_service() -> Result<(), String> {
    let (tx, rx) = mpsc::channel::<()>();
    let (ntx, nrx) = mpsc::channel::<Notice>();
    let running: Arc<Mutex<Option<Arc<Running>>>> = Arc::default();
    let r2 = running.clone();
    let ntx = Mutex::new(ntx);
    let handler = move |ev| {
        let queue = |n: Notice| {
            let _ = ntx.lock().unwrap_or_else(|e| e.into_inner()).send(n);
        };
        match ev {
            ServiceControl::Stop | ServiceControl::Preshutdown | ServiceControl::Shutdown => {
                let _ = tx.send(());
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::PowerEvent(p) => {
                if let Some(n) = power_notice(&p) {
                    queue(n);
                }
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::SessionChange(p) => {
                let raw = p.reason as u32;
                queue(Notice::Session(session_change(p.reason), raw, p.notification.session_id));
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        }
    };
    // One worker drains the notifications in order (they wait for the service to exist).
    std::thread::spawn(move || {
        for n in nrx {
            let current = r2.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let Some(r) = current else { continue };
            match n {
                Notice::Session(kind, raw, id) => r.on_session_change(kind, raw, id),
                Notice::Suspend => r.service.on_suspend(),
                Notice::Resume => r.service.on_resume(),
            }
        }
    });
    let sh = service_control_handler::register(SERVICE_NAME, handler).map_err(s)?;
    let none = ServiceControlAccept::empty();
    let _ = sh.set_service_status(status(ServiceState::StartPending, none, 0, Duration::from_secs(30), 1));

    let r = match crate::runtime::prepare(&Options::service_defaults()) {
        Ok(r) => Arc::new(r),
        Err(e) => {
            log::error_id(eventid::PIPE_FAILED, &format!("start failed: {e}"));
            let _ = sh.set_service_status(status(ServiceState::Stopped, none, 1, Duration::ZERO, 0));
            return Err(e);
        }
    };
    *running.lock().unwrap_or_else(|e| e.into_inner()) = Some(r.clone());
    let srv = r.clone();
    let pipe_thread = std::thread::spawn(move || {
        if let Err(e) = srv.serve() {
            log::error_id(eventid::PIPE_FAILED, &format!("control pipe stopped: {e}"));
        }
    });
    let accept = ServiceControlAccept::STOP
        | ServiceControlAccept::PRESHUTDOWN
        | ServiceControlAccept::POWER_EVENT
        | ServiceControlAccept::SESSION_CHANGE;
    let _ = sh.set_service_status(status(ServiceState::Running, accept, 0, Duration::ZERO, 0));
    r.start_background();

    let _ = rx.recv();
    // Ordered stop: one seat at a time on a worker; the checkpoint advances every few
    // seconds and after each seat, so the SCM never sees a hung stop.
    let checkpoint = std::sync::atomic::AtomicU32::new(1);
    let report = |cp: &std::sync::atomic::AtomicU32| {
        let n = cp.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let _ = sh.set_service_status(status(ServiceState::StopPending, none, 0, STOP_WAIT_HINT, n));
    };
    report(&checkpoint);
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let (seat_tx, seat_rx) = mpsc::channel::<()>();
    let stopper = {
        let r = r.clone();
        std::thread::spawn(move || {
            r.stop_with(&|_, _| {
                let _ = seat_tx.send(());
            });
            let _ = done_tx.send(());
        })
    };
    loop {
        match done_rx.recv_timeout(STOP_REPORT_EVERY) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => report(&checkpoint),
        }
        while seat_rx.try_recv().is_ok() {
            report(&checkpoint);
        }
    }
    let _ = stopper.join();
    let _ = pipe_thread.join();
    let _ = sh.set_service_status(status(ServiceState::Stopped, none, 0, Duration::ZERO, 0));
    Ok(())
}

/// PID of the running service (for clients that verify the pipe server).
pub fn service_pid() -> Option<u32> {
    let m = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT).ok()?;
    let svc = m.open_service(SERVICE_NAME, ServiceAccess::QUERY_STATUS).ok()?;
    let st = svc.query_status().ok()?;
    (st.current_state == ServiceState::Running).then_some(st.process_id).flatten()
}

/// For `status` without the pipe: the SCM's view.
pub fn scm_state() -> Option<String> {
    let m = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT).ok()?;
    let svc = m.open_service(SERVICE_NAME, ServiceAccess::QUERY_STATUS).ok()?;
    Some(format!("{:?}", svc.query_status().ok()?.current_state))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Moved from split-service `service::tests::power_events_map_to_seat_actions`.
    #[test]
    fn power_events_map_to_seat_actions() {
        assert!(matches!(power_notice(&PowerEventParam::Suspend), Some(Notice::Suspend)));
        assert!(matches!(power_notice(&PowerEventParam::ResumeAutomatic), Some(Notice::Resume)));
        assert!(power_notice(&PowerEventParam::ResumeSuspend).is_none(), "no double start");
        assert!(power_notice(&PowerEventParam::PowerStatusChange).is_none());
    }
}
