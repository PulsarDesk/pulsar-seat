// SPDX-License-Identifier: GPL-3.0-only
//! Registration of Pulsar Seat's own Remote Desktop protocol provider (`crates/seat-wrds`,
//! DLL [`DLL_NAME`]): the COM class of its protocol manager and the WinStation listener
//! [`LISTENER_NAME`] that names it. Added 2026-11-17 (package remote-idd-hdr).
//!
//! Documented layout (Microsoft Learn, "Remote Desktop Protocol Provider API"): a listener is
//! a key under `Terminal Server\WinStations`; its `LoadableProtocol_Object` (REG_SZ) holds the
//! CLSID of the protocol manager (`IWRdsProtocolManager`), an in-process COM server registered
//! under `HKLM\SOFTWARE\Classes\CLSID`. TermService loads the DLL and calls
//! `CreateListener(<key name>)`.
//!
//! Rules:
//! - Only our own two keys are written or deleted ([`listener_key`], [`clsid_key`]). `RDP-Tcp`
//!   and every other listener stay untouched (spec/remote 40 §10).
//! - Registered **only** while the operator chose the `pulsar_idd` display for remote seats
//!   (or automatic with a signed own driver); removed otherwise and at uninstall.
//! - Crash-safe write order: COM class first, then the listener with `fEnableWinStation = 0`,
//!   its `Security` descriptor (logon / connect only for the seat accounts, same builder as
//!   the loopback listener) and the owned values, `fEnableWinStation = 1` **last**. Removal:
//!   listener first, then the class. A half-written state reads as [`Registration::Partial`]
//!   and is rebuilt or removed by [`sync`].
//! - The listener never has a port, a LAN adapter or stored credentials: connections come
//!   only from our provider, which accepts only what the service asked for.
//! - Recovery value `WrdsDisabled=1` (DWORD) under the service's `Parameters` key keeps it
//!   unregistered whatever the config says.
//! - The DLL must be a native image (TermService's svchost is native).
//!
//! HYPOTHESIS (needs Windows Server + RD Session Host, nothing ran): TermService reads new
//! listener keys only when it starts, so a fresh registration needs a TermService restart
//! (the service reports `restart_needed` until the provider says hello); the WinStation
//! flags below (`fInheritAutoLogon = 1`, `fPromptForPassword = 0`) let the credentials our
//! provider returns from `GetUserCredentials` log the seat user on without a prompt.

use crate::error::SysError;
use crate::journal::{RegValue, b64_encode};
use crate::loopback_listener::winstation_security_descriptor;
use crate::pe::{IMAGE_FILE_MACHINE_AMD64, IMAGE_FILE_MACHINE_ARM64, machine_name, parse_machine};
use crate::rds::TS_WINSTATIONS;
use crate::registry::{Registry, get_u32};
use serde::{Deserialize, Serialize};

pub use seat_proto::wrds::{DLL_NAME, LISTENER_NAME, PROVIDER_CLSID};

/// HKLM-relative class root.
pub const CLSID_ROOT: &str = r"SOFTWARE\Classes\CLSID";
/// Service parameters (shared with the credential provider's recovery switch).
pub const SERVICE_PARAMETERS: &str = r"SYSTEM\CurrentControlSet\Services\PulsarSeat\Parameters";
/// Recovery switch: `1` = never register (and remove at the next sync).
pub const DISABLED_VALUE: &str = "WrdsDisabled";
const CLASS_NAME: &str = "Pulsar Seat Remote Desktop protocol provider";
const COMMENT: &str = "Pulsar Seat remote seats on the Pulsar Seat display (no network listener)";

pub fn listener_key() -> String {
    format!(r"{TS_WINSTATIONS}\{LISTENER_NAME}")
}

pub fn clsid_key() -> String {
    format!(r"{CLSID_ROOT}\{PROVIDER_CLSID}")
}

/// Every value registration writes, in write order: `(HKLM path, value name, value)`.
/// `allowed` = SIDs of the seat accounts that may log on through the listener.
pub fn registration_values(dll: &str, allowed: &[String]) -> Result<Vec<(String, &'static str, RegValue)>, SysError> {
    let sd = winstation_security_descriptor(allowed)?;
    let l = listener_key();
    let c = clsid_key();
    let inproc = format!(r"{c}\InprocServer32");
    Ok(vec![
        (c.clone(), "", RegValue::Sz(CLASS_NAME.into())),
        (inproc.clone(), "", RegValue::Sz(dll.into())),
        // TermService calls the provider from several threads (MTA).
        (inproc, "ThreadingModel", RegValue::Sz("Both".into())),
        (l.clone(), "fEnableWinStation", RegValue::Dword(0)),
        (l.clone(), "Security", RegValue::BinaryB64(b64_encode(&sd))),
        (l.clone(), "LoadableProtocol_Object", RegValue::Sz(PROVIDER_CLSID.into())),
        (l.clone(), "Comment", RegValue::Sz(COMMENT.into())),
        (l.clone(), "fInheritAutoLogon", RegValue::Dword(1)),
        (l.clone(), "fPromptForPassword", RegValue::Dword(0)),
        (l.clone(), "fInheritInitialProgram", RegValue::Dword(0)),
        (l.clone(), "Shadow", RegValue::Dword(0)),
        (l.clone(), "MaxInstanceCount", RegValue::Dword(64)),
        (l, "fEnableWinStation", RegValue::Dword(1)),
    ])
}

/// What the registry says about our registration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Registration {
    /// None of our keys exist.
    Absent,
    /// Every value is in place (this DLL path, these accounts, enabled).
    Ours,
    /// Some keys exist, point at another file / other accounts, or were half written.
    Partial,
}

fn same(got: &RegValue, want: &RegValue) -> bool {
    match (got, want) {
        (RegValue::Sz(a) | RegValue::ExpandSz(a), RegValue::Sz(b)) => a.eq_ignore_ascii_case(b),
        (a, b) => a == b,
    }
}

/// Read the current state against what [`register`] would write.
pub fn state(reg: &dyn Registry, dll: &str, allowed: &[String]) -> Result<Registration, SysError> {
    if !reg.key_exists(&listener_key())? && !reg.key_exists(&clsid_key())? {
        return Ok(Registration::Absent);
    }
    let want = registration_values(dll, allowed)?;
    // The first `fEnableWinStation = 0` is a write-order step; the final value is 1.
    for (path, name, value) in want.iter().filter(|(_, n, v)| !(*n == "fEnableWinStation" && *v == RegValue::Dword(0))) {
        if !same(&reg.get(path, name)?, value) {
            return Ok(Registration::Partial);
        }
    }
    Ok(Registration::Ours)
}

/// The recovery switch is set.
pub fn disabled_by_recovery(reg: &dyn Registry) -> bool {
    get_u32(reg, SERVICE_PARAMETERS, DISABLED_VALUE).ok().flatten().unwrap_or(0) != 0
}

/// Check a DLL's PE header against the native machine (`0xAA64` / `0x8664`).
pub fn check_dll(bytes: &[u8], native_machine: u16) -> Result<(), SysError> {
    let m = parse_machine(bytes).ok_or_else(|| SysError::Other(format!("{DLL_NAME} is not a PE image")))?;
    if !matches!(native_machine, IMAGE_FILE_MACHINE_ARM64 | IMAGE_FILE_MACHINE_AMD64) {
        return Err(SysError::Other(format!("native machine {native_machine:#06x} is not supported")));
    }
    if m != native_machine {
        return Err(SysError::Other(format!(
            "{DLL_NAME} is {} but TermService on this PC is {}",
            machine_name(m),
            machine_name(native_machine)
        )));
    }
    Ok(())
}

/// Write the registration in the crash-safe order. On a failure everything written so far
/// is removed again.
pub fn register(reg: &dyn Registry, dll: &str, allowed: &[String]) -> Result<(), SysError> {
    if dll.is_empty() || !dll.contains('\\') {
        return Err(SysError::Other("protocol provider DLL needs an absolute path".into()));
    }
    if allowed.is_empty() {
        return Err(SysError::Other("no seat account may use the protocol provider listener".into()));
    }
    for (path, name, value) in registration_values(dll, allowed)? {
        if let Err(e) = reg.set(&path, name, &value) {
            let _ = unregister(reg);
            return Err(e);
        }
    }
    Ok(())
}

/// Remove our keys: listener first (TermService stops offering it), then the class.
/// Missing keys are fine.
pub fn unregister(reg: &dyn Registry) -> Result<(), SysError> {
    let mut first = None;
    for k in [listener_key(), clsid_key()] {
        if let Err(e) = reg.delete_key(&k)
            && first.is_none()
        {
            first = Some(e);
        }
    }
    first.map_or(Ok(()), Err)
}

/// Bring the registration in line with `want`. The recovery switch overrides `want`; an
/// empty account list means nobody could use it, so it is removed too.
pub fn sync(reg: &dyn Registry, want: bool, dll: &str, allowed: &[String]) -> Result<Registration, SysError> {
    let want = want && !allowed.is_empty() && !disabled_by_recovery(reg);
    let now = if want { state(reg, dll, allowed)? } else { absent_or_partial(reg)? };
    match (want, now) {
        (true, Registration::Ours) | (false, Registration::Absent) => Ok(now),
        (true, _) => {
            if now == Registration::Partial {
                unregister(reg)?;
            }
            register(reg, dll, allowed)?;
            Ok(Registration::Ours)
        }
        (false, _) => {
            unregister(reg)?;
            Ok(Registration::Absent)
        }
    }
}

fn absent_or_partial(reg: &dyn Registry) -> Result<Registration, SysError> {
    Ok(if reg.key_exists(&listener_key())? || reg.key_exists(&clsid_key())? { Registration::Partial } else { Registration::Absent })
}

/// Status for the manager (`host_info.wrds`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WrdsStatus {
    pub registration: Registration,
    /// The provider inside TermService said hello on the pipe.
    pub provider_connected: bool,
    /// TermService started our listener (`StartListen`).
    pub listening: bool,
    /// Registered, but TermService has not loaded the provider yet (restart pending).
    pub restart_needed: bool,
    pub recovery_disabled: bool,
    /// Provider DLL next to the service is present and native.
    pub dll_ok: bool,
    /// Why seats cannot use it right now (`None` = usable).
    pub reason: Option<String>,
}

impl WrdsStatus {
    /// Combine the parts into one status with a reason code.
    pub fn evaluate(registration: Registration, provider_connected: bool, listening: bool, recovery_disabled: bool, dll_ok: bool) -> Self {
        let restart_needed = registration == Registration::Ours && !provider_connected;
        let reason = if recovery_disabled {
            Some("wrds_disabled_by_recovery")
        } else if !dll_ok {
            Some("wrds_dll_missing")
        } else if registration != Registration::Ours {
            Some("wrds_not_registered")
        } else if !provider_connected {
            Some("wrds_restart_needed")
        } else if !listening {
            Some("wrds_not_listening")
        } else {
            None
        };
        Self { registration, provider_connected, listening, restart_needed, recovery_disabled, dll_ok, reason: reason.map(String::from) }
    }

    pub fn usable(&self) -> bool {
        self.reason.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pe::fake_pe;
    use crate::rds::TS_RDP_TCP;
    use crate::registry::MemoryRegistry;

    const DLL: &str = r"C:\Program Files\Pulsar Seat\pulsar-seat-wrds.dll";
    fn sids() -> Vec<String> {
        vec!["S-1-5-21-1-2-3-1001".into()]
    }

    #[test]
    fn register_sync_unregister() {
        let r = MemoryRegistry::default();
        assert_eq!(state(&r, DLL, &sids()).unwrap(), Registration::Absent);
        assert_eq!(sync(&r, true, DLL, &sids()).unwrap(), Registration::Ours);
        assert_eq!(state(&r, DLL, &sids()).unwrap(), Registration::Ours);
        assert_eq!(r.get(&listener_key(), "LoadableProtocol_Object").unwrap(), RegValue::Sz(PROVIDER_CLSID.into()));
        assert_eq!(r.get(&listener_key(), "fEnableWinStation").unwrap(), RegValue::Dword(1));
        assert_eq!(r.get(&format!(r"{}\InprocServer32", clsid_key()), "").unwrap(), RegValue::Sz(DLL.into()));
        // Another seat account: the Security descriptor changes, so it is rebuilt.
        let more = vec!["S-1-5-21-1-2-3-1001".into(), "S-1-5-21-1-2-3-1002".into()];
        assert_eq!(state(&r, DLL, &more).unwrap(), Registration::Partial);
        assert_eq!(sync(&r, true, DLL, &more).unwrap(), Registration::Ours);
        // Off.
        assert_eq!(sync(&r, false, DLL, &more).unwrap(), Registration::Absent);
        assert!(!r.key_exists(&listener_key()).unwrap());
        assert!(!r.key_exists(&clsid_key()).unwrap());
    }

    #[test]
    fn rdp_tcp_is_never_touched() {
        let r = MemoryRegistry::default();
        r.put(TS_RDP_TCP, "LoadableProtocol_Object", RegValue::Sz("{5828227c-20cf-4408-b73f-73ab70b8849f}".into()));
        sync(&r, true, DLL, &sids()).unwrap();
        sync(&r, false, DLL, &sids()).unwrap();
        assert_eq!(
            r.get(TS_RDP_TCP, "LoadableProtocol_Object").unwrap(),
            RegValue::Sz("{5828227c-20cf-4408-b73f-73ab70b8849f}".into())
        );
    }

    #[test]
    fn enable_is_written_last_and_disabled_first() {
        let v = registration_values(DLL, &sids()).unwrap();
        let first_listener = v.iter().position(|(p, _, _)| *p == listener_key()).unwrap();
        assert_eq!(v[first_listener].1, "fEnableWinStation");
        assert_eq!(v[first_listener].2, RegValue::Dword(0));
        assert_eq!(v.last().unwrap().1, "fEnableWinStation");
        assert_eq!(v.last().unwrap().2, RegValue::Dword(1));
        // The class exists before the listener names it.
        assert!(v.iter().position(|(p, _, _)| *p == clsid_key()).unwrap() < first_listener);
        // No port, adapter or stored credentials on our listener.
        for (_, n, _) in &v {
            assert!(!["PortNumber", "LanAdapter", "Password", "UserName", "Domain"].contains(n), "{n}");
        }
    }

    #[test]
    fn half_written_is_partial_and_rebuilt() {
        let r = MemoryRegistry::default();
        r.put(&listener_key(), "fEnableWinStation", RegValue::Dword(0));
        assert_eq!(state(&r, DLL, &sids()).unwrap(), Registration::Partial);
        assert_eq!(sync(&r, true, DLL, &sids()).unwrap(), Registration::Ours);
        let r = MemoryRegistry::default();
        r.put(&listener_key(), "fEnableWinStation", RegValue::Dword(0));
        assert_eq!(sync(&r, false, DLL, &sids()).unwrap(), Registration::Absent);
    }

    #[test]
    fn recovery_switch_and_no_accounts_keep_it_off() {
        let r = MemoryRegistry::default();
        sync(&r, true, DLL, &sids()).unwrap();
        r.put(SERVICE_PARAMETERS, DISABLED_VALUE, RegValue::Dword(1));
        assert_eq!(sync(&r, true, DLL, &sids()).unwrap(), Registration::Absent);
        let r = MemoryRegistry::default();
        assert_eq!(sync(&r, true, DLL, &[]).unwrap(), Registration::Absent);
        assert!(register(&r, DLL, &[]).is_err());
        assert!(register(&r, "relative.dll", &sids()).is_err());
    }

    #[test]
    fn failed_write_leaves_nothing_behind() {
        let r = MemoryRegistry::default();
        r.fail_writes.store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(sync(&r, true, DLL, &sids()).is_err());
        r.fail_writes.store(false, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(state(&r, DLL, &sids()).unwrap(), Registration::Absent);
    }

    #[test]
    fn dll_machine_check() {
        assert!(check_dll(&fake_pe(IMAGE_FILE_MACHINE_ARM64), IMAGE_FILE_MACHINE_ARM64).is_ok());
        assert!(check_dll(&fake_pe(IMAGE_FILE_MACHINE_AMD64), IMAGE_FILE_MACHINE_ARM64).is_err());
        assert!(check_dll(b"nope", IMAGE_FILE_MACHINE_AMD64).is_err());
    }

    #[test]
    fn status_reasons_in_order() {
        let s = WrdsStatus::evaluate(Registration::Ours, true, true, false, true);
        assert!(s.usable());
        let s = WrdsStatus::evaluate(Registration::Ours, false, false, false, true);
        assert_eq!(s.reason.as_deref(), Some("wrds_restart_needed"));
        assert!(s.restart_needed);
        assert_eq!(WrdsStatus::evaluate(Registration::Absent, false, false, false, true).reason.as_deref(), Some("wrds_not_registered"));
        assert_eq!(WrdsStatus::evaluate(Registration::Ours, true, false, false, true).reason.as_deref(), Some("wrds_not_listening"));
        assert_eq!(WrdsStatus::evaluate(Registration::Ours, true, true, false, false).reason.as_deref(), Some("wrds_dll_missing"));
        assert_eq!(WrdsStatus::evaluate(Registration::Ours, true, true, true, true).reason.as_deref(), Some("wrds_disabled_by_recovery"));
    }
}
