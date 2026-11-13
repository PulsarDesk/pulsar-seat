// SPDX-License-Identifier: GPL-3.0-only
//! `DisplayBackend` trait, backend descriptors and the pure backend choice.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::DisplayError;
use crate::hdr::HdrUnavailable;
use crate::modes::ModeRequest;
use crate::seat::{DisplayState, ModeResult, ReadyInfo, SeatDisplay};

pub mod pulsar_idd;
pub mod rdp;
pub mod vdd;
pub mod vdd_xml;

/// Default `wait_ready` timeout (contract: 30 s; spec 40 §3.3 allows up to 90 s).
pub const READY_TIMEOUT: Duration = Duration::from_secs(30);
/// Poll interval of the readiness loops.
pub const POLL_MS: u64 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    /// Stock RDP indirect display of the seat session (Phase 2 default; SDR only).
    RdpSession,
    /// Signed third-party Virtual-Display-Driver (console seat only).
    Vdd,
    /// Own IddCx driver in `drivers/split-idd` (unsigned; never chosen automatically
    /// unless it reports installed + signed + remote capable).
    PulsarIdd,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BackendInfo {
    pub kind: BackendKind,
    pub installed: bool,
    pub driver_version: Option<String>,
    pub iddcx_version: Option<u32>,
    pub hdr: Result<(), HdrUnavailable>,
    /// Can put a monitor into a remote (RDP) seat session.
    pub remote_session_capable: bool,
    /// Added: driver package is signed so it loads without test mode. Our own driver
    /// reports `false` until it is attestation / WHQL signed.
    #[serde(default)]
    pub signed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatDisplayHandle {
    pub seat_id: String,
    pub session_id: u32,
    pub kind: BackendKind,
}

pub trait DisplayBackend: Send + Sync {
    fn kind(&self) -> BackendKind;
    fn probe(&self) -> Result<BackendInfo, DisplayError>;
    /// Creates / binds the seat's monitor in `cfg.session_id` (SET_SEAT_CONFIG for PulsarIdd).
    fn attach(&self, cfg: &SeatDisplay) -> Result<SeatDisplayHandle, DisplayError>;
    fn wait_ready(
        &self,
        h: &SeatDisplayHandle,
        timeout: Duration,
    ) -> Result<ReadyInfo, DisplayError>;
    fn request_mode(
        &self,
        h: &SeatDisplayHandle,
        req: &ModeRequest,
    ) -> Result<ModeResult, DisplayError>;
    fn state(&self, h: &SeatDisplayHandle) -> Result<DisplayState, DisplayError>;
    /// SHUTDOWN_SEAT / remove the monitor.
    fn detach(&self, h: &SeatDisplayHandle) -> Result<(), DisplayError>;
}

/// Picks the backend. Pure.
///
/// 1. A configured preference wins (`display_backend` setting), even if not installed —
///    attach then reports the reason.
/// 2. `PulsarIdd` if installed, signed and remote capable (or the seat is the console).
/// 3. `Vdd` if installed and the seat session is the console session (it cannot reach a
///    remote session, spec 40 §2.3 / OQ 20).
/// 4. `RdpSession`.
pub fn choose_backend(
    pref: Option<BackendKind>,
    infos: &[BackendInfo],
    session_is_console: bool,
) -> BackendKind {
    if let Some(p) = pref {
        return p;
    }
    let find = |k: BackendKind| infos.iter().find(|i| i.kind == k);
    if let Some(i) = find(BackendKind::PulsarIdd)
        && i.installed
        && i.signed
        && (i.remote_session_capable || session_is_console)
    {
        return BackendKind::PulsarIdd;
    }
    if session_is_console
        && let Some(i) = find(BackendKind::Vdd)
        && i.installed
    {
        return BackendKind::Vdd;
    }
    BackendKind::RdpSession
}

/// Waits with an injected clock until `f` returns `Some`, or the deadline passes.
pub(crate) fn poll_until<T>(
    clock: &dyn crate::Clock,
    sleeper: &dyn crate::Sleeper,
    timeout: Duration,
    mut f: impl FnMut() -> Result<Option<T>, DisplayError>,
) -> Result<Option<T>, DisplayError> {
    let deadline = clock.now_ms().saturating_add(timeout.as_millis() as u64);
    loop {
        if let Some(v) = f()? {
            return Ok(Some(v));
        }
        if clock.now_ms() >= deadline {
            return Ok(None);
        }
        sleeper.sleep_ms(POLL_MS);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(kind: BackendKind, installed: bool, signed: bool, remote: bool) -> BackendInfo {
        BackendInfo {
            kind,
            installed,
            driver_version: None,
            iddcx_version: None,
            hdr: Err(HdrUnavailable::DriverNotInstalled),
            remote_session_capable: remote,
            signed,
        }
    }

    #[test]
    fn choice_rules() {
        use BackendKind::*;
        let none: [BackendInfo; 0] = [];
        assert_eq!(choose_backend(None, &none, false), RdpSession);
        assert_eq!(choose_backend(Some(PulsarIdd), &none, false), PulsarIdd);
        let vdd = info(Vdd, true, true, false);
        assert_eq!(choose_backend(None, std::slice::from_ref(&vdd), true), Vdd);
        assert_eq!(
            choose_backend(None, std::slice::from_ref(&vdd), false),
            RdpSession
        );
        let own_unsigned = info(PulsarIdd, true, false, true);
        assert_eq!(
            choose_backend(None, &[own_unsigned, vdd.clone()], false),
            RdpSession
        );
        let own = info(PulsarIdd, true, true, true);
        assert_eq!(
            choose_backend(None, &[own.clone(), vdd.clone()], false),
            PulsarIdd
        );
        let own_console_only = info(PulsarIdd, true, true, false);
        assert_eq!(
            choose_backend(None, std::slice::from_ref(&own_console_only), false),
            RdpSession
        );
        assert_eq!(
            choose_backend(None, &[own_console_only, vdd], true),
            PulsarIdd
        );
        let own_missing = info(PulsarIdd, false, true, true);
        assert_eq!(choose_backend(None, &[own_missing], false), RdpSession);
    }
}
