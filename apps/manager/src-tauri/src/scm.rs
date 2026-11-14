// SPDX-License-Identifier: GPL-3.0-only
//! Start / stop / restart the one seat service (`PulsarSeat`) through the Service Control
//! Manager. Restored from Pulsar Split's manager (lost at merge step 9).
//!
//! The SCM does the access check: by default only Administrators may start or stop a
//! service, so the buttons work in the elevated manager only. An unelevated manager gets
//! `access_denied` and the UI offers "restart as administrator". This module never touches
//! the control pipe and never changes the service's configuration or start type.

use seat_proto::{ErrorCode, ProtoError};
use serde::Serialize;

/// What the SCM says about the service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    NotInstalled,
    Stopped,
    Running,
    /// Start / stop / pause in progress, or paused.
    Pending,
    Unknown,
}

/// `service_state` answer: SCM state and whether this manager process is elevated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ServiceInfo {
    pub state: RunState,
    pub elevated: bool,
}

/// One button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Start,
    Stop,
    Restart,
}

impl Action {
    pub fn parse(s: &str) -> Result<Self, ProtoError> {
        match s {
            "start" => Ok(Self::Start),
            "stop" => Ok(Self::Stop),
            "restart" => Ok(Self::Restart),
            other => Err(ProtoError::new(ErrorCode::BadRequest, format!("unknown service action {other}"))),
        }
    }
}

/// `dwCurrentState` (SERVICE_STOPPED = 1 .. SERVICE_PAUSED = 7) as a [`RunState`].
pub fn map_state(current: u32) -> RunState {
    match current {
        1 => RunState::Stopped,
        4 => RunState::Running,
        2 | 3 | 5 | 6 | 7 => RunState::Pending,
        _ => RunState::Unknown,
    }
}

/// Win32 error of an SCM call as a protocol error the UI understands.
pub fn map_win32(code: i32, what: &str) -> ProtoError {
    const ERROR_ACCESS_DENIED: i32 = 5;
    const ERROR_SERVICE_DOES_NOT_EXIST: i32 = 1060;
    const ERROR_SERVICE_ALREADY_RUNNING: i32 = 1056;
    const ERROR_SERVICE_NOT_ACTIVE: i32 = 1062;
    const ERROR_SERVICE_CANNOT_ACCEPT_CTRL: i32 = 1061;
    match code {
        ERROR_ACCESS_DENIED => ProtoError::new(ErrorCode::AccessDenied, format!("{what}: administrator rights needed")),
        ERROR_SERVICE_DOES_NOT_EXIST => ProtoError::new(ErrorCode::ServiceUnavailable, "service is not installed"),
        ERROR_SERVICE_ALREADY_RUNNING | ERROR_SERVICE_NOT_ACTIVE | ERROR_SERVICE_CANNOT_ACCEPT_CTRL => {
            ProtoError::new(ErrorCode::NotReady, format!("{what}: service is changing state (error {code})"))
        }
        _ => ProtoError::new(ErrorCode::Internal, format!("{what}: Win32 error {code}")),
    }
}

/// Steps of one action: which control to send and which state to wait for, in order.
pub fn plan(action: Action) -> &'static [(Step, RunState)] {
    match action {
        Action::Start => &[(Step::Start, RunState::Running)],
        Action::Stop => &[(Step::Stop, RunState::Stopped)],
        Action::Restart => &[(Step::Stop, RunState::Stopped), (Step::Start, RunState::Running)],
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Start,
    Stop,
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::time::{Duration, Instant};
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
    use windows_sys::Win32::System::Services::{
        CloseServiceHandle, ControlService, OpenSCManagerW, OpenServiceW, QueryServiceStatus, SC_HANDLE, SC_MANAGER_CONNECT,
        SERVICE_CONTROL_STOP, SERVICE_QUERY_STATUS, SERVICE_START, SERVICE_STATUS, SERVICE_STOP, StartServiceW,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    /// The service stops its seats first (up to its own grace times), so allow a while.
    const WAIT: Duration = Duration::from_secs(60);

    struct Sc(SC_HANDLE);
    impl Drop for Sc {
        fn drop(&mut self) {
            // SAFETY: a handle we opened and still own.
            unsafe { CloseServiceHandle(self.0) };
        }
    }

    fn last() -> i32 {
        std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
    }

    fn open(access: u32) -> Result<Sc, i32> {
        let name: Vec<u16> = seat_core::consts::SERVICE_NAME.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: null machine / database = local SCM, active database.
        let scm = unsafe { OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT) };
        if scm.is_null() {
            return Err(last());
        }
        let scm = Sc(scm);
        // SAFETY: `scm` is open; `name` is NUL-terminated and outlives the call.
        let svc = unsafe { OpenServiceW(scm.0, name.as_ptr(), access) };
        if svc.is_null() { Err(last()) } else { Ok(Sc(svc)) }
    }

    fn query(svc: &Sc) -> Result<u32, i32> {
        // SAFETY: zero is a valid bit pattern for this plain C struct.
        let mut st: SERVICE_STATUS = unsafe { std::mem::zeroed() };
        // SAFETY: open handle with SERVICE_QUERY_STATUS; `st` is a valid out buffer.
        if unsafe { QueryServiceStatus(svc.0, &mut st) } == 0 { Err(last()) } else { Ok(st.dwCurrentState) }
    }

    fn elevated() -> bool {
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: pseudo handle of this process; `token` receives a handle we close below.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return false;
        }
        let mut e = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut len = 0u32;
        // SAFETY: `e` is a TOKEN_ELEVATION of exactly the size passed.
        let ok = unsafe {
            GetTokenInformation(token, TokenElevation, (&mut e as *mut TOKEN_ELEVATION).cast(), std::mem::size_of::<TOKEN_ELEVATION>() as u32, &mut len)
        };
        // SAFETY: opened above.
        unsafe { CloseHandle(token) };
        ok != 0 && e.TokenIsElevated != 0
    }

    pub fn info() -> ServiceInfo {
        const ERROR_SERVICE_DOES_NOT_EXIST: i32 = 1060;
        let state = match open(SERVICE_QUERY_STATUS).and_then(|s| query(&s)) {
            Ok(st) => map_state(st),
            Err(ERROR_SERVICE_DOES_NOT_EXIST) => RunState::NotInstalled,
            Err(_) => RunState::Unknown,
        };
        ServiceInfo { state, elevated: elevated() }
    }

    fn wait_for(svc: &Sc, target: RunState) -> Result<(), ProtoError> {
        let start = Instant::now();
        while start.elapsed() < WAIT {
            if map_state(query(svc).map_err(|c| map_win32(c, "query"))?) == target {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        Err(ProtoError::new(ErrorCode::Timeout, "timed out waiting for the service"))
    }

    pub fn control(action: Action) -> Result<(), ProtoError> {
        let svc = open(SERVICE_QUERY_STATUS | SERVICE_START | SERVICE_STOP).map_err(|c| map_win32(c, "open service"))?;
        for (step, target) in plan(action) {
            let current = map_state(query(&svc).map_err(|c| map_win32(c, "query"))?);
            // Restart of a stopped service, or a click after someone else acted: skip
            // the control, just wait for the wanted state.
            if current != *target {
                let ok = match step {
                    // SAFETY: open handle with SERVICE_START; no arguments.
                    Step::Start => unsafe { StartServiceW(svc.0, 0, std::ptr::null()) },
                    Step::Stop => {
                        // SAFETY: zero is valid; open handle with SERVICE_STOP.
                        let mut st: SERVICE_STATUS = unsafe { std::mem::zeroed() };
                        unsafe { ControlService(svc.0, SERVICE_CONTROL_STOP, &mut st) }
                    }
                };
                if ok == 0 {
                    let code = last();
                    // Already in the wanted direction: wait below.
                    if !matches!(code, 1056 | 1062) {
                        return Err(map_win32(code, if *step == Step::Start { "start" } else { "stop" }));
                    }
                }
            }
            wait_for(&svc, *target)?;
        }
        Ok(())
    }
}

#[cfg(windows)]
pub use imp::{control, info};

#[cfg(not(windows))]
pub fn info() -> ServiceInfo {
    ServiceInfo { state: RunState::NotInstalled, elevated: false }
}

#[cfg(not(windows))]
pub fn control(_action: Action) -> Result<(), ProtoError> {
    Err(ProtoError::new(ErrorCode::NotImplemented, "Windows only"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actions_parse_and_plan() {
        assert_eq!(Action::parse("start").unwrap(), Action::Start);
        assert_eq!(Action::parse("stop").unwrap(), Action::Stop);
        assert_eq!(Action::parse("restart").unwrap(), Action::Restart);
        assert_eq!(Action::parse("delete").unwrap_err().code, ErrorCode::BadRequest);
        assert_eq!(plan(Action::Start), &[(Step::Start, RunState::Running)]);
        assert_eq!(plan(Action::Stop), &[(Step::Stop, RunState::Stopped)]);
        assert_eq!(plan(Action::Restart), &[(Step::Stop, RunState::Stopped), (Step::Start, RunState::Running)]);
    }

    #[test]
    fn scm_states_map() {
        assert_eq!(map_state(1), RunState::Stopped);
        assert_eq!(map_state(4), RunState::Running);
        for s in [2, 3, 5, 6, 7] {
            assert_eq!(map_state(s), RunState::Pending);
        }
        assert_eq!(map_state(0), RunState::Unknown);
        assert_eq!(serde_json::to_value(RunState::NotInstalled).unwrap(), "not_installed");
        let j = serde_json::to_value(ServiceInfo { state: RunState::Running, elevated: true }).unwrap();
        assert_eq!(j, serde_json::json!({"state": "running", "elevated": true}));
    }

    #[test]
    fn win32_errors_map_to_codes() {
        assert_eq!(map_win32(5, "start").code, ErrorCode::AccessDenied);
        assert_eq!(map_win32(1060, "open").code, ErrorCode::ServiceUnavailable);
        assert_eq!(map_win32(1061, "stop").code, ErrorCode::NotReady);
        assert_eq!(map_win32(87, "x").code, ErrorCode::Internal);
    }
}
