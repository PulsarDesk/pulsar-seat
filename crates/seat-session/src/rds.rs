// SPDX-License-Identifier: GPL-3.0-only
//! Remote Desktop Services host preparation for the RDP loopback backends (spec 00 §5.2.1,
//! spec 30 §6.1.1, §6.2.13). Adapted from pulsar-split `split-session::rds`.
//!
//! Only documented values are written, each journaled (`journal\session.jsonl`, intent →
//! write → done) so [`RdsApplier::restore_all`] can put back exactly what was there,
//! including "absent". A value someone changed after us is left alone. Group-policy values
//! that win over ours are **reported, never fought**.

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::SysError;
use crate::journal::{
    RecordStatus, RegValue, RestoreLog, RestoreRecord, StepAction, WallClock, registry_target, replay_plan, rfc3339_utc,
    split_registry_target, step_action,
};
use crate::registry::{Registry, get_string, get_u32};

pub const PACKAGE: &str = "session";

pub const TS: &str = r"SYSTEM\CurrentControlSet\Control\Terminal Server";
pub const TS_RDP_TCP: &str = r"SYSTEM\CurrentControlSet\Control\Terminal Server\WinStations\RDP-Tcp";
pub const TS_RCM_LICENSING_CORE: &str = r"SYSTEM\CurrentControlSet\Control\Terminal Server\RCM\Licensing Core";
pub const POL: &str = r"SOFTWARE\Policies\Microsoft\Windows NT\Terminal Services";
pub const TERMSERVICE_PARAMETERS: &str = r"SYSTEM\CurrentControlSet\Services\TermService\Parameters";
pub const SYSTEM_POLICIES: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System";

/// "No limit" for `MaxInstanceCount`.
pub const UNLIMITED: u32 = 0xFFFF_FFFF;

/// One value `prepare_host` writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RdsRow {
    pub key: &'static str,
    pub value: &'static str,
    pub data: u32,
    /// Snake_case note shown in the manager when this row was changed.
    pub note: &'static str,
}

impl RdsRow {
    pub fn target(&self) -> String {
        registry_target(self.key, self.value)
    }
}

/// The rows, in apply order:
///
/// - `fDenyTSConnections = 0`: accept RDP connections (the loopback seats).
/// - `fSingleSessionPerUser = 0`: machine-wide; otherwise a second seat with the same
///   account would take over the first seat's session (spec 30 §6.2.13). Deny is enforced
///   by the service with a log-off, never by this value.
/// - `RDP-Tcp\UserAuthentication = 1`: keep NLA on (credentials checked before a session
///   exists; the seat host passes them through CredSSP).
/// - `RDP-Tcp\Shadow = 0`: no shadowing of seat sessions.
/// - `fDisableAudioCapture = 0` / `fDisableCam = 0` are *not* written: audio redirection
///   policy is owned by the media package (HYPOTHESIS that the defaults allow it).
pub fn rds_rows() -> Vec<RdsRow> {
    vec![
        RdsRow { key: TS, value: "fDenyTSConnections", data: 0, note: "rdp_connections_enabled" },
        RdsRow { key: TS, value: "fSingleSessionPerUser", data: 0, note: "single_session_per_user_off" },
        RdsRow { key: TS_RDP_TCP, value: "UserAuthentication", data: 1, note: "nla_on" },
        RdsRow { key: TS_RDP_TCP, value: "Shadow", data: 0, note: "shadow_off" },
    ]
}

/// Policy values that win over the rows above when set.
const POLICY_SHADOWED: [&str; 3] = ["fDenyTSConnections", "fSingleSessionPerUser", "UserAuthentication"];

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyReport {
    pub changed: Vec<String>,
    pub unchanged: Vec<String>,
    /// Policy values that override ours (`HKLM\key\value`).
    pub policy_overrides: Vec<String>,
    /// Snake_case notes for the manager.
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreReport {
    pub restored: Vec<String>,
    /// Someone changed the value after us; left as it is.
    pub skipped_changed: Vec<String>,
}

pub struct RdsApplier {
    reg: Arc<dyn Registry>,
    log: Arc<dyn RestoreLog>,
    clock: Arc<dyn WallClock>,
}

impl RdsApplier {
    pub fn new(reg: Arc<dyn Registry>, log: Arc<dyn RestoreLog>, clock: Arc<dyn WallClock>) -> Self {
        Self { reg, log, clock }
    }

    fn journal(&self, target: &str, prev: &RegValue, new: &RegValue, status: RecordStatus) -> Result<(), SysError> {
        let seq = self.log.read_all()?.iter().map(|r| r.seq).max().unwrap_or(0) + 1;
        let now = self.clock.now_ms();
        self.log.append(&RestoreRecord {
            v: 1,
            seq,
            time: rfc3339_utc(now),
            actor: "service".into(),
            package: PACKAGE.into(),
            kind: "registry".into(),
            target: target.into(),
            previous: prev.clone(),
            new: new.clone(),
            status,
        })
    }

    /// Policy values overriding ours (read-only).
    pub fn policy_overrides(&self) -> Result<Vec<String>, SysError> {
        let mut out = Vec::new();
        for name in POLICY_SHADOWED {
            let pol = self.reg.get(POL, name)?;
            let ours = rds_rows().into_iter().find(|r| r.value == name).map(|r| RegValue::Dword(r.data));
            if pol != RegValue::Absent && Some(&pol) != ours.as_ref() {
                out.push(registry_target(POL, name));
            }
        }
        Ok(out)
    }

    /// Writes every row that differs; journals each change. Idempotent.
    pub fn apply(&self) -> Result<ApplyReport, SysError> {
        let mut rep = ApplyReport { policy_overrides: self.policy_overrides()?, ..Default::default() };
        if self.reg.get(POL, "fDenyTSConnections")? == RegValue::Dword(1) {
            rep.notes.push("rdp_denied_by_policy".into());
        }
        for r in rds_rows() {
            let target = r.target();
            let want = RegValue::Dword(r.data);
            let cur = self.reg.get(r.key, r.value)?;
            if cur == want {
                rep.unchanged.push(target);
                continue;
            }
            self.journal(&target, &cur, &want, RecordStatus::Intent)?;
            self.reg.set(r.key, r.value, &want)?;
            self.journal(&target, &cur, &want, RecordStatus::Done)?;
            log::info!("rds: set {target}");
            rep.notes.push(r.note.into());
            rep.changed.push(target);
        }
        for p in &rep.policy_overrides {
            rep.notes.push(format!("policy_override:{p}"));
        }
        rep.notes.extend(session_policy_notes(self.reg.as_ref())?);
        Ok(rep)
    }

    /// Guarded reverse replay of this package's journal, then clears it. Only the rows of
    /// [`rds_rows`] are ever written back (the journal is data, not authority).
    pub fn restore_all(&self) -> Result<RestoreReport, SysError> {
        let mut rep = RestoreReport::default();
        let allowed: Vec<String> = rds_rows().iter().map(|r| r.target().to_ascii_lowercase()).collect();
        let records = self.log.read_all()?;
        for step in replay_plan(&records, PACKAGE) {
            if !allowed.contains(&step.target.to_ascii_lowercase()) {
                log::warn!("rds: journal names {} which is not ours; ignored", step.target);
                continue;
            }
            let Some((key, value)) = split_registry_target(&step.target) else { continue };
            let cur = self.reg.get(key, value)?;
            match step_action(&step, &cur) {
                StepAction::Restore => {
                    self.reg.set(key, value, &step.previous)?;
                    if !rep.restored.contains(&step.target) {
                        rep.restored.push(step.target.clone());
                    }
                }
                StepAction::AlreadyRestored => {}
                StepAction::Conflict => {
                    if !rep.skipped_changed.contains(&step.target) && !rep.restored.contains(&step.target) {
                        rep.skipped_changed.push(step.target.clone());
                    }
                }
            }
        }
        self.log.clear()?;
        Ok(rep)
    }
}

/// Listener facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListenerInfo {
    pub port: u16,
    /// Effective `MaxInstanceCount` (policy first); `None` = unlimited.
    pub max_instance_count: Option<u32>,
    pub nla: bool,
}

/// RDS licensing facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LicensingInfo {
    /// `LicensingMode`: 2 per device, 4 per user, 5 not configured (policy first).
    pub mode: Option<u32>,
    pub grace_days_left: Option<u32>,
}

impl LicensingInfo {
    pub fn configured(&self) -> bool {
        matches!(self.mode, Some(2 | 4))
    }
}

/// Facts about TermService and its listener.
pub trait RdsProbe: Send + Sync {
    fn listener(&self) -> Result<ListenerInfo, SysError>;
    fn licensing(&self) -> Result<LicensingInfo, SysError>;
    /// Raw (unexpanded) `TermService\Parameters\ServiceDll`.
    fn service_dll(&self) -> Result<String, SysError>;
    /// TermService is running.
    fn termservice_running(&self) -> bool;
    /// Waits for `Global\TermSrvReadyEvent` (opened with SYNCHRONIZE only, never created).
    fn wait_ready(&self, timeout: Duration) -> bool;
}

/// Listener values from the registry (policy first).
pub fn listener_from_registry(reg: &dyn Registry) -> Result<ListenerInfo, SysError> {
    let port = get_u32(reg, TS_RDP_TCP, "PortNumber")?.ok_or_else(|| SysError::Other("RDP-Tcp PortNumber is missing".into()))?;
    let port = u16::try_from(port).ok().filter(|p| *p != 0).ok_or_else(|| SysError::Other(format!("RDP-Tcp PortNumber {port} is invalid")))?;
    let max = get_u32(reg, POL, "MaxInstanceCount")?.or(get_u32(reg, TS_RDP_TCP, "MaxInstanceCount")?);
    let nla = get_u32(reg, POL, "UserAuthentication")?.or(get_u32(reg, TS_RDP_TCP, "UserAuthentication")?) == Some(1);
    Ok(ListenerInfo { port, max_instance_count: max.filter(|m| *m != UNLIMITED), nla })
}

pub fn licensing_mode_from_registry(reg: &dyn Registry) -> Result<Option<u32>, SysError> {
    Ok(get_u32(reg, POL, "LicensingMode")?.or(get_u32(reg, TS_RCM_LICENSING_CORE, "LicensingMode")?))
}

pub fn service_dll_from_registry(reg: &dyn Registry) -> Result<String, SysError> {
    get_string(reg, TERMSERVICE_PARAMETERS, "ServiceDll")?.ok_or_else(|| SysError::Other("TermService ServiceDll is missing".into()))
}

/// `fDenyTSConnections` effective value (policy first): RDP denied.
pub fn rdp_denied(reg: &dyn Registry) -> Result<bool, SysError> {
    Ok(get_u32(reg, POL, "fDenyTSConnections")?.or(get_u32(reg, TS, "fDenyTSConnections")?).unwrap_or(1) == 1)
}

/// Session time-out values (milliseconds, 0 = never) that would end or log off a seat
/// session, e.g. while its seat host reconnects. Policy key first, then the RDP-Tcp
/// listener's own value.
pub const SESSION_TIMEOUT_VALUES: [&str; 3] = ["MaxDisconnectionTime", "MaxIdleTime", "MaxConnectionTime"];

/// Pure: read-only notes about RDS settings spec 00 §5.2.1 wants for seats but that
/// `prepare_host` does not write (they are policies the admin owns). HYPOTHESIS: the exact
/// set (value names under the Terminal Services policy key, `bEnumerateHWBeforeSW` for
/// "use the hardware graphics adapter") still has to be checked on a Server test host.
///
/// - `session_timeout_policy_set:<value>` / `session_timeout_set:<value>`: a time-out is set
///   (policy / listener);
/// - `session_reset_on_timeout`: `fResetBroken = 1` (log off instead of disconnect);
/// - `hardware_gpu_not_enabled`: the policy to render sessions on the hardware GPU is not on.
pub fn session_policy_notes(reg: &dyn Registry) -> Result<Vec<String>, SysError> {
    let mut notes = Vec::new();
    for v in SESSION_TIMEOUT_VALUES {
        if get_u32(reg, POL, v)?.is_some_and(|x| x != 0) {
            notes.push(format!("session_timeout_policy_set:{v}"));
        } else if get_u32(reg, POL, v)?.is_none() && get_u32(reg, TS_RDP_TCP, v)?.is_some_and(|x| x != 0) {
            notes.push(format!("session_timeout_set:{v}"));
        }
    }
    if get_u32(reg, POL, "fResetBroken")?.or(get_u32(reg, TS_RDP_TCP, "fResetBroken")?) == Some(1) {
        notes.push("session_reset_on_timeout".into());
    }
    if get_u32(reg, POL, "bEnumerateHWBeforeSW")? != Some(1) {
        notes.push("hardware_gpu_not_enabled".into());
    }
    Ok(notes)
}

/// `HideFastUserSwitching` policy set (fast user switching hidden from the UI).
pub fn fast_user_switching_hidden(reg: &dyn Registry) -> Result<bool, SysError> {
    Ok(get_u32(reg, SYSTEM_POLICIES, "HideFastUserSwitching")? == Some(1))
}

/// Winlogon key of the machine.
pub const WINLOGON: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon";

/// Fast user switching is hidden by policy or turned off in Winlogon
/// (`AllowMultipleTSSessions` = 0; HYPOTHESIS: on client editions this value turns fast
/// user switching off, as public admin documentation describes). Read-only: the backend
/// never changes either value, it refuses (`fsu_disabled`).
pub fn fast_user_switching_disabled(reg: &dyn Registry) -> Result<bool, SysError> {
    Ok(fast_user_switching_hidden(reg)? || get_u32(reg, WINLOGON, "AllowMultipleTSSessions")? == Some(0))
}

/// True when `ServiceDll` is the stock `%SystemRoot%\System32\termsrv.dll`.
pub fn termservice_is_stock(service_dll: &str, system_root: Option<&str>) -> bool {
    let s = service_dll.trim().trim_matches('"').replace('/', "\\").to_ascii_lowercase();
    const TAIL: &str = r"\system32\termsrv.dll";
    for var in ["%systemroot%", "%windir%"] {
        if let Some(rest) = s.strip_prefix(var) {
            return rest == TAIL;
        }
    }
    match system_root {
        Some(root) if !root.trim().is_empty() => s == format!("{}{TAIL}", root.trim_end_matches('\\').to_ascii_lowercase()),
        _ => false,
    }
}

/// Registry-only probe (tests; the real one adds service state and the ready event).
pub struct RegistryRdsProbe {
    pub reg: Arc<dyn Registry>,
    pub running: bool,
    pub grace_days_left: Option<u32>,
}

impl RdsProbe for RegistryRdsProbe {
    fn listener(&self) -> Result<ListenerInfo, SysError> {
        listener_from_registry(self.reg.as_ref())
    }
    fn licensing(&self) -> Result<LicensingInfo, SysError> {
        Ok(LicensingInfo { mode: licensing_mode_from_registry(self.reg.as_ref())?, grace_days_left: self.grace_days_left })
    }
    fn service_dll(&self) -> Result<String, SysError> {
        service_dll_from_registry(self.reg.as_ref())
    }
    fn termservice_running(&self) -> bool {
        self.running
    }
    fn wait_ready(&self, _timeout: Duration) -> bool {
        self.running
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::{MemoryRestoreLog, SystemWallClock};
    use crate::registry::MemoryRegistry;

    fn applier() -> (Arc<MemoryRegistry>, Arc<MemoryRestoreLog>, RdsApplier) {
        let reg = Arc::new(MemoryRegistry::default());
        let log = Arc::new(MemoryRestoreLog::default());
        let a = RdsApplier::new(reg.clone(), log.clone(), Arc::new(SystemWallClock));
        (reg, log, a)
    }

    fn defaults(reg: &MemoryRegistry) {
        reg.put(TS, "fDenyTSConnections", RegValue::Dword(1));
        reg.put(TS_RDP_TCP, "PortNumber", RegValue::Dword(3389));
        reg.put(TS_RDP_TCP, "UserAuthentication", RegValue::Dword(1));
    }

    #[test]
    fn apply_then_restore_is_exact() {
        let (reg, log, a) = applier();
        defaults(&reg);
        let before = reg.snapshot();
        let rep = a.apply().unwrap();
        assert_eq!(rep.changed.len(), 3, "{rep:?}");
        assert!(rep.notes.contains(&"single_session_per_user_off".to_string()));
        assert!(rep.unchanged.contains(&registry_target(TS_RDP_TCP, "UserAuthentication")));
        assert_eq!(log.records.lock().unwrap().len(), 6);
        assert_eq!(reg.get(TS, "fSingleSessionPerUser").unwrap(), RegValue::Dword(0));
        // Windows flips a value back; a second apply journals it again.
        reg.put(TS, "fDenyTSConnections", RegValue::Dword(1));
        assert_eq!(a.apply().unwrap().changed, vec![registry_target(TS, "fDenyTSConnections")]);
        let r = a.restore_all().unwrap();
        assert!(r.skipped_changed.is_empty(), "{r:?}");
        assert_eq!(reg.snapshot(), before);
        assert!(log.records.lock().unwrap().is_empty());
    }

    #[test]
    fn restore_skips_values_changed_after_us_and_foreign_targets() {
        let (reg, log, a) = applier();
        defaults(&reg);
        a.apply().unwrap();
        reg.put(TS_RDP_TCP, "Shadow", RegValue::Dword(4));
        // A forged journal line for some other value must never be replayed.
        let mut forged = log.records.lock().unwrap()[0].clone();
        forged.target = registry_target(r"SYSTEM\CurrentControlSet\Services\Evil", "ImagePath");
        forged.seq = 99;
        log.records.lock().unwrap().push(forged);
        reg.put(r"SYSTEM\CurrentControlSet\Services\Evil", "ImagePath", RegValue::Dword(0));
        let r = a.restore_all().unwrap();
        assert_eq!(r.skipped_changed, vec![registry_target(TS_RDP_TCP, "Shadow")]);
        assert_eq!(reg.get(TS_RDP_TCP, "Shadow").unwrap(), RegValue::Dword(4));
        assert_eq!(reg.get(r"SYSTEM\CurrentControlSet\Services\Evil", "ImagePath").unwrap(), RegValue::Dword(0));
    }

    #[test]
    fn session_policies_are_reported() {
        let reg = MemoryRegistry::default();
        assert_eq!(session_policy_notes(&reg).unwrap(), vec!["hardware_gpu_not_enabled".to_string()]);
        reg.put(POL, "MaxIdleTime", RegValue::Dword(900_000));
        reg.put(POL, "MaxDisconnectionTime", RegValue::Dword(0));
        reg.put(TS_RDP_TCP, "MaxDisconnectionTime", RegValue::Dword(60_000));
        reg.put(TS_RDP_TCP, "MaxConnectionTime", RegValue::Dword(60_000));
        reg.put(POL, "fResetBroken", RegValue::Dword(1));
        reg.put(POL, "bEnumerateHWBeforeSW", RegValue::Dword(1));
        let n = session_policy_notes(&reg).unwrap();
        // A policy of 0 ("never") wins over the listener's own value.
        assert_eq!(n, ["session_timeout_policy_set:MaxIdleTime", "session_timeout_set:MaxConnectionTime", "session_reset_on_timeout"]);
    }

    #[test]
    fn policies_reported_not_fought() {
        let (reg, _log, a) = applier();
        defaults(&reg);
        reg.put(POL, "fDenyTSConnections", RegValue::Dword(1));
        reg.put(POL, "fSingleSessionPerUser", RegValue::Dword(1));
        let rep = a.apply().unwrap();
        assert!(rep.notes.contains(&"rdp_denied_by_policy".to_string()));
        assert_eq!(rep.policy_overrides.len(), 2);
        assert_eq!(reg.get(POL, "fDenyTSConnections").unwrap(), RegValue::Dword(1), "policy untouched");
        assert!(rdp_denied(reg.as_ref()).unwrap());
    }

    #[test]
    fn listener_and_licensing() {
        let reg = MemoryRegistry::default();
        assert!(listener_from_registry(&reg).is_err());
        reg.put(TS_RDP_TCP, "PortNumber", RegValue::Dword(3390));
        reg.put(TS_RDP_TCP, "MaxInstanceCount", RegValue::Dword(UNLIMITED));
        let l = listener_from_registry(&reg).unwrap();
        assert_eq!((l.port, l.max_instance_count, l.nla), (3390, None, false));
        reg.put(POL, "MaxInstanceCount", RegValue::Dword(4));
        assert_eq!(listener_from_registry(&reg).unwrap().max_instance_count, Some(4));
        reg.put(TS_RCM_LICENSING_CORE, "LicensingMode", RegValue::Dword(5));
        assert_eq!(licensing_mode_from_registry(&reg).unwrap(), Some(5));
        assert!(!LicensingInfo { mode: Some(5), grace_days_left: None }.configured());
        assert!(LicensingInfo { mode: Some(4), grace_days_left: None }.configured());
    }

    #[test]
    fn stock_service_dll() {
        assert!(termservice_is_stock(r"%SystemRoot%\System32\termsrv.dll", None));
        assert!(termservice_is_stock(r"%windir%\system32\TERMSRV.DLL", None));
        assert!(termservice_is_stock(r"C:\Windows\System32\termsrv.dll", Some(r"C:\Windows\")));
        assert!(!termservice_is_stock(r"C:\Program Files\RDP Wrapper\rdpwrap.dll", Some(r"C:\Windows")));
        assert!(!termservice_is_stock("", None));
    }
}
