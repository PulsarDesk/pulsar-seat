// SPDX-License-Identifier: GPL-3.0-only
//! Errors of the system-access layer and the stable reason codes this crate puts into
//! [`BackendError::reason`] (the manager maps `kind` + `reason` to localized copy).

use seat_core::backend::{BackendError, ErrorKind};

/// Failure of one system access (registry, journal, Win32 call).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SysError {
    #[error("registry: {0}")]
    Registry(String),
    #[error("journal: {0}")]
    Journal(String),
    #[error("{api} failed (win32 {code})")]
    Win32 { api: &'static str, code: u32 },
    #[error("{0}")]
    Other(String),
}

impl SysError {
    /// Stable snake_case code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Registry(_) => "registry",
            Self::Journal(_) => "journal",
            Self::Win32 { .. } => "os",
            Self::Other(_) => "internal",
        }
    }

    /// `GetLastError()` after a failed call to `api`.
    #[cfg(windows)]
    pub fn last_win32(api: &'static str) -> Self {
        // SAFETY: GetLastError has no preconditions.
        let code = unsafe { windows_sys::Win32::Foundation::GetLastError() };
        Self::Win32 { api, code }
    }
}

impl From<SysError> for BackendError {
    fn from(e: SysError) -> Self {
        match &e {
            SysError::Registry(_) => BackendError::new(ErrorKind::Os, "registry", e.to_string()),
            SysError::Journal(_) => BackendError::new(ErrorKind::Internal, "journal", e.to_string()),
            SysError::Win32 { code, .. } => {
                let kind = if *code == 5 { ErrorKind::Denied } else { ErrorKind::Os };
                BackendError::new(kind, "os", e.to_string()).with_win32(*code)
            }
            SysError::Other(_) => BackendError::new(ErrorKind::Internal, "internal", e.to_string()),
        }
    }
}

/// Stable reason codes (snake_case) used by the backends of this crate.
pub mod reason {
    /// Not Windows Server with the RD Session Host role (client SKU, Home, …).
    pub const EDITION_UNSUPPORTED: &str = "edition_unsupported";
    /// Windows Server without the RD Session Host role.
    pub const RDSH_ROLE_MISSING: &str = "rdsh_role_missing";
    /// TermService / listener not usable yet (stopped, port missing, ready event not set).
    pub const RDS_NOT_READY: &str = "rds_not_ready";
    /// Remote connections are denied by policy (`fDenyTSConnections` policy = 1).
    pub const RDP_DENIED_BY_POLICY: &str = "rdp_denied_by_policy";
    /// `TermService\Parameters\ServiceDll` is not the stock value and not ours.
    pub const FOREIGN_SERVICE_DLL: &str = "foreign_service_dll";
    /// The reference multi-seat product is installed.
    pub const REFERENCE_PRODUCT_PRESENT: &str = "reference_product_present";
    /// No console session (boot not finished, or between sessions).
    pub const CONSOLE_NOT_READY: &str = "console_not_ready";
    /// The console session has no signed-in user (desktops backend needs one).
    pub const CONSOLE_NOT_SIGNED_IN: &str = "console_not_signed_in";
    /// The seat's session did not appear in time.
    pub const SESSION_TIMEOUT: &str = "session_timeout";
    /// The session is not one of ours (client name / user / address check failed).
    pub const NOT_OUR_SESSION: &str = "not_our_session";
    /// The session id does not exist (any more).
    pub const SESSION_GONE: &str = "session_gone";
    /// More seats than the listener / licence allows.
    pub const SEAT_LIMIT: &str = "seat_limit";
    /// Experimental backend selected without the current warning acknowledged.
    pub const EXPERIMENTAL_NOT_ACKNOWLEDGED: &str = "experimental_not_acknowledged";
    /// Built without the `experimental-client-sku` cargo feature.
    pub const FEATURE_NOT_BUILT: &str = "feature_not_built";
    /// Kiosk seat without a kiosk app (desktops backend).
    pub const KIOSK_APP_MISSING: &str = "kiosk_app_missing";
    /// Seat 1 is the console; this backend never logs it off.
    pub const CONSOLE_SEAT: &str = "console_seat";
    /// Fast user switching: another seat is on the console right now.
    pub const CONSOLE_BUSY: &str = "console_busy";
    /// The seat has no account to find its session by.
    pub const NO_ACCOUNT: &str = "no_account";
    /// Logon failed (wrong / expired password, disabled account, logon hours).
    pub const LOGON_FAILED: &str = "logon_failed";
    /// Fast user switching: the console session has a signed-in user whose desktop is not
    /// locked; switching it away would hand an unlocked desktop over and kick that user
    /// without consent (the user locks it first, Win+L). Also returned for unattended
    /// starts (boot / auto-start), which this one-seat-at-a-time backend never does.
    pub const CONSOLE_IN_USE: &str = "console_in_use";
    /// Fast user switching: no password is stored and the target session is not locked,
    /// so connecting it would skip authentication.
    pub const SESSION_NOT_LOCKED: &str = "session_not_locked";
    /// The seat account's SID could not be resolved (sessions are matched by SID only).
    pub const ACCOUNT_UNRESOLVED: &str = "account_unresolved";
    /// The RDP listener does not require NLA; stored credentials are never sent to it.
    /// Also: the listener requires NLA and the seat has no stored password.
    pub const NLA_REQUIRED: &str = "nla_required";
    /// Fast user switching is hidden (`HideFastUserSwitching`) or turned off.
    pub const FSU_DISABLED: &str = "fsu_disabled";
    /// Fast user switching: another seat already uses this account.
    pub const SAME_ACCOUNT_DENIED: &str = "same_account_denied";
    /// A passwordless seat needs the second, loopback-only listener, and it is not ready
    /// (pending TermService restart, firewall off, no allowed account, ...; the probe note
    /// `loopback_listener:<state>` says which).
    pub const LOOPBACK_LISTENER_UNAVAILABLE: &str = "loopback_listener_unavailable";
}

pub(crate) fn err(kind: ErrorKind, reason: &str, message: impl Into<String>) -> BackendError {
    BackendError::new(kind, reason, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_to_backend_error() {
        let b: BackendError = SysError::Win32 { api: "WTSLogoffSession", code: 5 }.into();
        assert_eq!((b.kind, b.reason.as_str(), b.win32), (ErrorKind::Denied, "os", Some(5)));
        let b: BackendError = SysError::Registry("x".into()).into();
        assert_eq!((b.kind, b.reason.as_str()), (ErrorKind::Os, "registry"));
        assert_eq!(SysError::Journal("j".into()).code(), "journal");
    }
}
