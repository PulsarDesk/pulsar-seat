// SPDX-License-Identifier: GPL-3.0-only
//! Real Windows implementations of the system traits. Nothing that changes system state
//! runs in unit tests; most of it needs SYSTEM in session 0. **Untested on hardware**
//! unless a test below says otherwise.

pub mod accounts;
pub mod desktop;
pub mod events;
pub mod files;
pub mod host_account;
pub mod launch;
pub mod listener;
pub mod loopback_listener;
pub mod probes;
pub mod process;
pub mod registry;
pub mod wmi;
pub mod wts;

use std::os::windows::io::{FromRawHandle, OwnedHandle};
use std::path::Path;
use std::sync::Arc;

use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};

use crate::backends::{SessionDeps, ThreadSleeper};
use crate::error::SysError;
use crate::journal::{JsonlRestoreLog, SystemWallClock};

/// NUL-terminated UTF-16.
pub(crate) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub(crate) fn wide_path(p: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    p.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
}

/// NUL-terminated UTF-16 that is wiped on drop (passwords).
pub(crate) fn wide_secret(s: &str) -> zeroize::Zeroizing<Vec<u16>> {
    zeroize::Zeroizing::new(wide(s))
}

pub(crate) fn last_error() -> u32 {
    // SAFETY: GetLastError has no preconditions.
    unsafe { windows_sys::Win32::Foundation::GetLastError() }
}

/// Reads a NUL-terminated UTF-16 string.
///
/// # Safety
/// `p` is null or points to a NUL-terminated UTF-16 string.
pub(crate) unsafe fn from_wide_ptr(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut n = 0usize;
    // SAFETY: caller guarantees NUL termination.
    unsafe {
        while *p.add(n) != 0 {
            n += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(p, n))
    }
}

/// Takes ownership of a raw handle returned by an API (null / INVALID = error).
pub(crate) fn owned(h: HANDLE, api: &'static str) -> Result<OwnedHandle, SysError> {
    if h.is_null() || h == INVALID_HANDLE_VALUE {
        return Err(SysError::last_win32(api));
    }
    // SAFETY: the API just returned this handle and nobody else owns it.
    Ok(unsafe { OwnedHandle::from_raw_handle(h as _) })
}

pub(crate) fn raw(h: &OwnedHandle) -> HANDLE {
    std::os::windows::io::AsRawHandle::as_raw_handle(h) as HANDLE
}

/// `ExpandEnvironmentStringsW`.
pub fn expand_env(s: &str) -> Option<String> {
    use windows_sys::Win32::System::Environment::ExpandEnvironmentStringsW;
    let src = wide(s);
    let mut buf = vec![0u16; 1024];
    for _ in 0..2 {
        // SAFETY: src is NUL-terminated; buf has buf.len() elements.
        let n = unsafe { ExpandEnvironmentStringsW(src.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) } as usize;
        if n == 0 {
            return None;
        }
        if n <= buf.len() {
            return Some(String::from_utf16_lossy(&buf[..n - 1]));
        }
        buf.resize(n, 0);
    }
    None
}

/// `HKLM\SYSTEM\CurrentControlSet\Services\<name>` exists.
pub fn service_key_exists(name: &str) -> bool {
    use crate::registry::Registry;
    registry::WinRegistry.key_exists(&format!(r"SYSTEM\CurrentControlSet\Services\{name}")).unwrap_or(true)
}

/// SCM state of a service or driver: `Some(true)` = any state but stopped (a driver:
/// loaded), `Some(false)` = stopped or unknown to the SCM, `None` = the query failed.
/// Query rights only.
pub fn service_active(name: &str) -> Option<bool> {
    use windows_sys::Win32::Foundation::ERROR_SERVICE_DOES_NOT_EXIST;
    use windows_sys::Win32::System::Services::{
        CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceStatus, SC_MANAGER_CONNECT, SERVICE_QUERY_STATUS, SERVICE_STATUS,
        SERVICE_STOPPED,
    };
    let n = wide(name);
    // SAFETY: plain SCM calls with valid pointers; both handles are closed below.
    unsafe {
        let scm = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT);
        if scm.is_null() {
            return None;
        }
        let svc = OpenServiceW(scm, n.as_ptr(), SERVICE_QUERY_STATUS);
        if svc.is_null() {
            let e = last_error();
            CloseServiceHandle(scm);
            return (e == ERROR_SERVICE_DOES_NOT_EXIST).then_some(false);
        }
        let mut st: SERVICE_STATUS = std::mem::zeroed();
        let ok = QueryServiceStatus(svc, &mut st) != 0;
        CloseServiceHandle(svc);
        CloseServiceHandle(scm);
        ok.then_some(st.dwCurrentState != SERVICE_STOPPED)
    }
}

/// Coexistence state of the reference product (`seat_core::reference`): its service keys,
/// their `Start` values and the SCM state. Read only; unknown facts fail closed.
pub fn reference_product_state() -> seat_core::reference::ReferenceState {
    use crate::journal::RegValue;
    use crate::registry::Registry;
    let key = |name: &str| format!(r"SYSTEM\CurrentControlSet\Services\{name}");
    let keys = seat_core::reference::probe(
        |name| registry::WinRegistry.key_exists(&key(name)).ok(),
        |name| match registry::WinRegistry.get(&key(name), "Start") {
            Ok(RegValue::Dword(v)) => Some(v),
            _ => None,
        },
        service_active,
    );
    seat_core::reference::classify(&keys)
}

/// Uninstall (service stopped): remove the passwordless-seat listener and its firewall
/// rules (reserved names, works without a journal), then put back the RDS values of
/// `journal\session.jsonl`. Both steps always run; the first error is returned after
/// both (the other is logged). **Untested on hardware.**
pub fn revert_session_host(data_dir: &Path) -> Result<crate::loopback_listener::RemoveReport, SysError> {
    let reg: Arc<dyn crate::registry::Registry> = Arc::new(registry::WinRegistry);
    let l = crate::loopback_listener::LoopbackListener {
        reg: reg.clone(),
        host: Arc::new(loopback_listener::WindowsLoopbackHost),
        wts: Arc::new(wts::WindowsWts),
        log: Arc::new(JsonlRestoreLog::in_data_root(data_dir, crate::loopback_listener::PACKAGE)),
        clock: Arc::new(SystemWallClock),
    };
    let listener = l.remove();
    let rds = crate::rds::RdsApplier::new(reg, Arc::new(JsonlRestoreLog::in_data_root(data_dir, crate::rds::PACKAGE)), Arc::new(SystemWallClock))
        .restore_all();
    match &rds {
        Ok(r) => {
            for s in &r.skipped_changed {
                log::warn!("rds: {s} was changed after us; left as it is");
            }
        }
        Err(e) => log::warn!("rds: values not restored: {e}"),
    }
    match (listener, rds) {
        (Ok(rep), Ok(_)) => Ok(rep),
        (Err(e), r) => {
            log::warn!("loopback listener: not removed: {e}");
            Err(match r {
                Ok(_) => e,
                Err(r) => SysError::Other(format!("{e}; RDS values: {r}")),
            })
        }
        (Ok(_), Err(r)) => Err(r),
    }
}

impl SessionDeps {
    /// Real system wiring for the service (SYSTEM, session 0). `data_dir` is normally
    /// `%ProgramData%\Pulsar Seat`. The service sets `geometry` / `last_user` afterwards.
    pub fn system(data_dir: &Path) -> Self {
        let reg = Arc::new(registry::WinRegistry);
        let wts = Arc::new(wts::WindowsWts);
        SessionDeps {
            registry: reg.clone(),
            edition: Arc::new(probes::WindowsEditionProbe::new(reg.clone())),
            rds: Arc::new(probes::WindowsRdsProbe::new(reg)),
            wts: wts.clone(),
            journal: Arc::new(JsonlRestoreLog::in_data_root(data_dir, crate::rds::PACKAGE)),
            ts_claims: crate::rds::TsClaims::process(),
            loopback: Arc::new(loopback_listener::WindowsLoopbackHost),
            listener_journal: Arc::new(JsonlRestoreLog::in_data_root(data_dir, crate::loopback_listener::PACKAGE)),
            launcher: Arc::new(launch::WindowsLauncher::new(wts)),
            wall_clock: Arc::new(SystemWallClock),
            clock: Arc::new(seat_core::SystemClock),
            sleeper: Arc::new(ThreadSleeper),
            geometry: None,
            last_user: None,
            system_root: std::env::var("SystemRoot").ok().or_else(|| expand_env("%SystemRoot%")),
            reference_product: Arc::new(reference_product_state),
            #[cfg(feature = "experimental-client-sku")]
            termwrap: Some(Arc::new(crate::termwrap::WindowsTermwrap::system(data_dir, crate::termwrap::TermwrapPolicy::default()))),
        }
    }
}
