// SPDX-License-Identifier: GPL-3.0-only
//! Start / stop the seat service through the Service Control Manager.

use seat_proto::remote::{ErrorCode, ProtoError};

pub const SERVICE_NAME: &str = "PulsarSplit";

#[cfg(windows)]
mod imp {
    use super::*;
    use std::ffi::OsStr;
    use std::time::{Duration, Instant};
    use windows_service::service::{Service, ServiceAccess, ServiceState};
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

    const ERROR_SERVICE_DOES_NOT_EXIST: i32 = 1060;
    const WAIT: Duration = Duration::from_secs(30);

    fn err(e: windows_service::Error) -> ProtoError {
        ProtoError::new(ErrorCode::Internal, e.to_string())
    }

    fn not_installed(e: &windows_service::Error) -> bool {
        matches!(e, windows_service::Error::Winapi(io) if io.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST))
    }

    fn open(access: ServiceAccess) -> Result<Service, windows_service::Error> {
        ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?.open_service(SERVICE_NAME, access)
    }

    pub fn state() -> String {
        match open(ServiceAccess::QUERY_STATUS).and_then(|s| s.query_status()) {
            Ok(st) => match st.current_state {
                ServiceState::Running => "running",
                ServiceState::Stopped => "stopped",
                _ => "pending",
            }
            .into(),
            Err(e) if not_installed(&e) => "not_installed".into(),
            Err(_) => "unknown".into(),
        }
    }

    fn wait_for(svc: &Service, target: ServiceState) -> Result<(), ProtoError> {
        let start = Instant::now();
        while start.elapsed() < WAIT {
            if svc.query_status().map_err(err)?.current_state == target {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        Err(ProtoError::new(ErrorCode::ServiceUnavailable, "timed out waiting for the service"))
    }

    fn start(svc: &Service) -> Result<(), ProtoError> {
        svc.start(&[] as &[&OsStr]).map_err(err)?;
        wait_for(svc, ServiceState::Running)
    }

    fn stop(svc: &Service) -> Result<(), ProtoError> {
        svc.stop().map_err(err)?;
        wait_for(svc, ServiceState::Stopped)
    }

    pub fn control(action: &str) -> Result<(), ProtoError> {
        let svc = open(ServiceAccess::QUERY_STATUS | ServiceAccess::START | ServiceAccess::STOP).map_err(|e| {
            if not_installed(&e) {
                ProtoError::new(ErrorCode::ServiceUnavailable, "service is not installed")
            } else {
                err(e)
            }
        })?;
        match action {
            "start" => start(&svc),
            "stop" => stop(&svc),
            "restart" => {
                stop(&svc)?;
                start(&svc)
            }
            other => Err(ProtoError::new(ErrorCode::BadRequest, format!("unknown action {other}"))),
        }
    }
}

#[cfg(windows)]
pub use imp::{control, state};

#[cfg(not(windows))]
pub fn state() -> String {
    "not_installed".into()
}

#[cfg(not(windows))]
pub fn control(_action: &str) -> Result<(), ProtoError> {
    Err(ProtoError::new(ErrorCode::NotImplemented, "Windows only"))
}
