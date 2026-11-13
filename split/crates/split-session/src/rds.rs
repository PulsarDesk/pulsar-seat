// SPDX-License-Identifier: GPL-3.0-only
//! Remote Desktop Services registry rows, save/restore of originals, listener and
//! licensing facts (spec 30 §2.3, §2.5 path A, §3, §13).

use crate::SessionError;
use crate::journal::{JournalValue, RecordKind, RecordStatus, RestoreLog, RestoreRecord};
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub const TS: &str = r"SYSTEM\CurrentControlSet\Control\Terminal Server";
pub const TS_WINSTATIONS: &str = r"SYSTEM\CurrentControlSet\Control\Terminal Server\WinStations";
pub const TS_RDP_TCP: &str = r"SYSTEM\CurrentControlSet\Control\Terminal Server\WinStations\RDP-Tcp";
pub const TS_LICENSING_CORE: &str = r"SYSTEM\CurrentControlSet\Control\Terminal Server\Licensing Core";
pub const TS_RCM_LICENSING_CORE: &str = r"SYSTEM\CurrentControlSet\Control\Terminal Server\RCM\Licensing Core";
pub const TS_ADDINS: &str = r"SYSTEM\CurrentControlSet\Control\Terminal Server\AddIns";
pub const POL: &str = r"SOFTWARE\Policies\Microsoft\Windows NT\Terminal Services";
pub const KEYBOARD_LAYOUT: &str = r"SYSTEM\CurrentControlSet\Control\Keyboard Layout";
pub const WINLOGON: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon";
pub const TERMSERVICE_PARAMETERS: &str = r"SYSTEM\CurrentControlSet\Services\TermService\Parameters";
/// Our own copy of the original values (one REG_SZ per row, JSON `JournalValue`).
pub const ORIGINALS_KEY: &str = r"SOFTWARE\PulsarSplit\Session\Originals";

/// "No limit" for `MaxInstanceCount`.
pub const UNLIMITED: u32 = 0xFFFF_FFFF;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RegData {
    Dword(u32),
    Sz(String),
    ExpandSz(String),
    MultiSz(Vec<String>),
}

/// Which hosts a row is written on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RdsPath {
    /// Path A (and every host that runs seats).
    Always,
    /// Only when a client-SKU wrapper is active (spec 30 §2.3 "path B only").
    ClientWrapperOnly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RdsRow {
    pub key: &'static str,
    pub value: &'static str,
    pub data: RegData,
    pub path: RdsPath,
}

impl RdsRow {
    /// `HKLM\<key>\<value>`, used in journals and reports.
    pub fn target(&self) -> String {
        format!(r"HKLM\{}\{}", self.key, self.value)
    }

    fn original_name(&self) -> String {
        format!(r"{}\{}", self.key, self.value)
    }
}

fn row(key: &'static str, value: &'static str, data: u32, path: RdsPath) -> RdsRow {
    RdsRow { key, value, data: RegData::Dword(data), path }
}

/// Only the "write" rows of spec 30 §2.3. Skipped / verify-only / read-only rows are not
/// listed; `fEnableWddmDriver` and `TerminateIndirectOnStall` belong to the display package.
pub fn rds_rows(set_gpu_realtime: bool) -> Vec<RdsRow> {
    use RdsPath::*;
    let mut rows = vec![
        row(TS, "fDenyTSConnections", 0, Always),
        row(TS_RDP_TCP, "Shadow", 0, Always),
        row(TS_RDP_TCP, "UserAuthentication", 1, Always),
        row(TS_RDP_TCP, "SecurityLayer", 2, Always),
        row(KEYBOARD_LAYOUT, "IgnoreRemoteKeyboardLayout", 1, Always),
        row(POL, "bEnumerateHWBeforeSW", 1, Always),
        row(POL, "Shadow", 0, Always),
        row(TS_LICENSING_CORE, "EnableConcurrentSessions", 1, ClientWrapperOnly),
        row(WINLOGON, "AllowMultipleTSSessions", 1, ClientWrapperOnly),
    ];
    if set_gpu_realtime {
        rows.insert(1, row(TS_WINSTATIONS, "SetGpuRealtimePriority", 1, Always));
    }
    rows
}

/// HKLM registry access; key paths are relative to HKLM.
pub trait Registry: Send + Sync {
    fn get(&self, key: &str, value: &str) -> Result<Option<RegData>, SessionError>;
    /// Creates the key if needed.
    fn set(&self, key: &str, value: &str, data: &RegData) -> Result<(), SessionError>;
    /// Deleting an absent value is not an error.
    fn delete(&self, key: &str, value: &str) -> Result<(), SessionError>;
    fn key_exists(&self, key: &str) -> Result<bool, SessionError>;
}

impl<T: Registry + ?Sized> Registry for std::sync::Arc<T> {
    fn get(&self, key: &str, value: &str) -> Result<Option<RegData>, SessionError> {
        (**self).get(key, value)
    }
    fn set(&self, key: &str, value: &str, data: &RegData) -> Result<(), SessionError> {
        (**self).set(key, value, data)
    }
    fn delete(&self, key: &str, value: &str) -> Result<(), SessionError> {
        (**self).delete(key, value)
    }
    fn key_exists(&self, key: &str) -> Result<bool, SessionError> {
        (**self).key_exists(key)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RdsWarning {
    /// A Windows default add-in key is missing or differs (verify only, not rewritten).
    AddInMissing { key: String },
    /// A policy value overrides one of our rows; reported, not fought.
    PolicyOverride { value: String },
    /// Policy denies incoming RDP (`Pol\fDenyTSConnections` = 1): no seat can connect.
    PolicyDeniesRdp,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyReport {
    /// Rows written now (`HKLM\key\value`).
    pub changed: Vec<String>,
    /// Rows that already had our value.
    pub unchanged: Vec<String>,
    /// Policy values that win over ours (`HKLM\key\value`).
    pub policy_overrides: Vec<String>,
    pub warnings: Vec<RdsWarning>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreReport {
    pub restored: Vec<String>,
    /// Someone changed the value after us; left as it is.
    pub skipped_changed: Vec<String>,
    /// Seat accounts we created; never deleted automatically (their profiles hold data).
    pub accounts_left: Vec<String>,
}

/// Policy values that, when set, win over the listener values we write.
const POLICY_SHADOWED: [&str; 3] = ["fDenyTSConnections", "UserAuthentication", "SecurityLayer"];

pub struct RdsApplier<R: Registry, L: RestoreLog> {
    reg: R,
    log: L,
    set_gpu_realtime: bool,
}

impl<R: Registry, L: RestoreLog> RdsApplier<R, L> {
    pub fn new(reg: R, log: L) -> Self {
        Self { reg, log, set_gpu_realtime: true }
    }

    /// `SetGpuRealtimePriority` is optional (default on).
    pub fn with_gpu_realtime(mut self, on: bool) -> Self {
        self.set_gpu_realtime = on;
        self
    }

    pub fn registry(&self) -> &R {
        &self.reg
    }

    fn saved_original(&self, r: &RdsRow) -> Result<Option<JournalValue>, SessionError> {
        match self.reg.get(ORIGINALS_KEY, &r.original_name())? {
            Some(RegData::Sz(json)) => serde_json::from_str(&json)
                .map(Some)
                .map_err(|e| SessionError::InvalidArgument(format!("bad saved original for {}: {e}", r.target()))),
            Some(_) => Err(SessionError::InvalidArgument(format!("bad saved original for {}", r.target()))),
            None => Ok(None),
        }
    }

    fn journal(&self, r: &RdsRow, prev: &JournalValue, new: &JournalValue, st: RecordStatus) -> Result<(), SessionError> {
        self.log.append(RestoreRecord::new(RecordKind::Registry, r.target(), prev.clone(), new.clone(), st))
    }

    /// Saves originals (once, first change only), writes rows, returns what changed and
    /// which rows are overridden by policy – reported, not fought.
    pub fn apply(&self, include_wrapper_rows: bool) -> Result<ApplyReport, SessionError> {
        let mut rep = ApplyReport::default();
        for w in self.shadowing_policies()? {
            if let RdsWarning::PolicyOverride { value } = &w {
                rep.policy_overrides.push(value.clone());
            }
            rep.warnings.push(w);
        }
        for r in rds_rows(self.set_gpu_realtime) {
            if r.path == RdsPath::ClientWrapperOnly && !include_wrapper_rows {
                continue;
            }
            let current = self.reg.get(r.key, r.value)?;
            if current.as_ref() == Some(&r.data) {
                rep.unchanged.push(r.target());
                continue;
            }
            let saved = self.saved_original(&r)?;
            if r.key == POL && current.is_some() {
                // A present policy value that is not ours is someone else's (local or domain
                // GPO), also when we wrote this row first and a GPO changed it later: the
                // domain value wins (spec 30 §2.3). Only an absent value is (re)written.
                rep.warnings.push(RdsWarning::PolicyOverride { value: r.target() });
                rep.policy_overrides.push(r.target());
                continue;
            }
            let prev = JournalValue::from(current.as_ref());
            let new = JournalValue::from(Some(&r.data));
            if saved.is_none() {
                let json = serde_json::to_string(&prev).map_err(|e| SessionError::InvalidArgument(e.to_string()))?;
                self.reg.set(ORIGINALS_KEY, &r.original_name(), &RegData::Sz(json))?;
            }
            self.journal(&r, &prev, &new, RecordStatus::Intent)?;
            self.reg.set(r.key, r.value, &r.data)?;
            self.journal(&r, &prev, &new, RecordStatus::Done)?;
            log::info!("rds: set {}", r.target());
            rep.changed.push(r.target());
        }
        rep.warnings.extend(self.verify_addins());
        Ok(rep)
    }

    /// Puts every value we changed back to its original, unless someone changed it after us.
    pub fn restore_all(&self) -> Result<RestoreReport, SessionError> {
        let mut rep = RestoreReport::default();
        // Reverse order of application; both GPU variants and both paths are covered.
        for r in rds_rows(true).into_iter().rev() {
            let Some(orig) = self.saved_original(&r)? else { continue };
            let current = self.reg.get(r.key, r.value)?;
            if current.as_ref() == Some(&r.data) {
                let cur = JournalValue::from(current.as_ref());
                self.journal(&r, &cur, &orig, RecordStatus::Intent)?;
                match orig.to_reg()? {
                    Some(d) => self.reg.set(r.key, r.value, &d)?,
                    None => self.reg.delete(r.key, r.value)?,
                }
                self.journal(&r, &cur, &orig, RecordStatus::Done)?;
                rep.restored.push(r.target());
            } else {
                rep.skipped_changed.push(r.target());
            }
            self.reg.delete(ORIGINALS_KEY, &r.original_name())?;
        }
        for rec in self.log.records()? {
            if rec.kind == RecordKind::Account && rec.status == RecordStatus::Done && !rep.accounts_left.contains(&rec.target) {
                rep.accounts_left.push(rec.target);
            }
        }
        Ok(rep)
    }

    /// Policy values that win over the listener rows we write (`PolicyDeniesRdp` first).
    fn shadowing_policies(&self) -> Result<Vec<RdsWarning>, SessionError> {
        let mut w = Vec::new();
        if self.reg.get(POL, "fDenyTSConnections")? == Some(RegData::Dword(1)) {
            w.push(RdsWarning::PolicyDeniesRdp);
        }
        for name in POLICY_SHADOWED {
            let pol = self.reg.get(POL, name)?;
            let ours = rds_rows(false).into_iter().find(|r| r.value == name && r.key != POL).map(|r| r.data);
            if matches!((&pol, &ours), (Some(p), Some(o)) if p != o) {
                w.push(RdsWarning::PolicyOverride { value: format!(r"HKLM\{POL}\{name}") });
            }
        }
        Ok(w)
    }

    /// Read-only policy check (for host info): shadowing policies plus our `Policies` rows
    /// that hold someone else's value.
    pub fn verify_policy(&self) -> Vec<RdsWarning> {
        let mut w = self.shadowing_policies().unwrap_or_default();
        for r in rds_rows(self.set_gpu_realtime).into_iter().filter(|r| r.key == POL) {
            let current = self.reg.get(r.key, r.value).ok().flatten();
            // Saved original or not: a present value other than ours is an override.
            if current.is_some() && current.as_ref() != Some(&r.data) {
                w.push(RdsWarning::PolicyOverride { value: r.target() });
            }
        }
        w
    }

    /// The "verify only" add-in rows (Windows defaults; never rewritten).
    pub fn verify_addins(&self) -> Vec<RdsWarning> {
        let mut w = Vec::new();
        let mut check = |key: String, ok: bool| {
            if !ok {
                w.push(RdsWarning::AddInMissing { key });
            }
        };
        check(format!(r"HKLM\{TS_ADDINS}"), self.reg.key_exists(TS_ADDINS).unwrap_or(false));
        let expect: [(&str, Option<&str>, u32); 3] =
            [("Clip Redirector", Some("RDPClip"), 3), ("DND Redirector", Some("RDPDND"), 3), ("Dynamic VC", None, UNLIMITED)];
        for (sub, name, ty) in expect {
            let key = format!(r"{TS_ADDINS}\{sub}");
            let name_ok = match name {
                Some(n) => matches!(self.reg.get(&key, "Name"), Ok(Some(RegData::Sz(s))) if s.eq_ignore_ascii_case(n)),
                None => true,
            };
            let type_ok = matches!(self.reg.get(&key, "Type"), Ok(Some(RegData::Dword(t))) if t == ty);
            check(format!(r"HKLM\{key}"), name_ok && type_ok);
        }
        w
    }
}

// ---- listener / licensing ----

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListenerInfo {
    pub port: u16,
    pub ipv6_only: bool,
    /// Thumbprint of the listener's TLS certificate (pinned by the helper).
    pub cert_sha1: Option<[u8; 20]>,
    /// Effective `MaxInstanceCount` (policy first); `None` = unlimited.
    pub max_instance_count: Option<u32>,
    pub nla: bool,
}

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

pub trait RdsProbe: Send + Sync {
    /// Re-read at every client start (the port may change while running).
    fn listener(&self) -> Result<ListenerInfo, SessionError>;
    fn licensing(&self) -> Result<LicensingInfo, SessionError>;
    /// `TermService\Parameters\ServiceDll`, unexpanded.
    fn service_dll(&self) -> Result<String, SessionError>;
    /// Opens `Global\TermSrvReadyEvent` with SYNCHRONIZE only, else polls; never creates it.
    fn wait_ready(&self, timeout: Duration) -> Result<(), SessionError>;
}

fn dword(reg: &dyn Registry, key: &str, value: &str) -> Result<Option<u32>, SessionError> {
    Ok(match reg.get(key, value)? {
        Some(RegData::Dword(d)) => Some(d),
        _ => None,
    })
}

/// Listener values that live in the registry (`ipv6_only` / `cert_sha1` come from the
/// system and are filled by the real probe).
pub fn listener_from_registry(reg: &dyn Registry) -> Result<ListenerInfo, SessionError> {
    let port = dword(reg, TS_RDP_TCP, "PortNumber")?
        .ok_or_else(|| SessionError::RdsNotReady("RDP-Tcp PortNumber is missing".into()))?;
    let port = u16::try_from(port)
        .ok()
        .filter(|p| *p != 0)
        .ok_or_else(|| SessionError::RdsNotReady(format!("RDP-Tcp PortNumber {port} is invalid")))?;
    let max = dword(reg, POL, "MaxInstanceCount")?.or(dword(reg, TS_RDP_TCP, "MaxInstanceCount")?);
    let nla = dword(reg, POL, "UserAuthentication")?.or(dword(reg, TS_RDP_TCP, "UserAuthentication")?) == Some(1);
    Ok(ListenerInfo { port, ipv6_only: false, cert_sha1: None, max_instance_count: max.filter(|m| *m != UNLIMITED), nla })
}

pub fn licensing_mode_from_registry(reg: &dyn Registry) -> Result<Option<u32>, SessionError> {
    Ok(dword(reg, POL, "LicensingMode")?.or(dword(reg, TS_RCM_LICENSING_CORE, "LicensingMode")?))
}

pub fn service_dll_from_registry(reg: &dyn Registry) -> Result<String, SessionError> {
    match reg.get(TERMSERVICE_PARAMETERS, "ServiceDll")? {
        Some(RegData::Sz(s) | RegData::ExpandSz(s)) => Ok(s),
        _ => Err(SessionError::RdsNotReady("TermService ServiceDll is missing".into())),
    }
}

/// True when `ServiceDll` is the stock `%SystemRoot%\System32\termsrv.dll` (case-insensitive).
pub fn termservice_is_stock(service_dll: &str) -> bool {
    let s = service_dll.trim().trim_matches('"').replace('/', "\\").to_ascii_lowercase();
    const TAIL: &str = r"\system32\termsrv.dll";
    for var in ["%systemroot%", "%windir%"] {
        if let Some(rest) = s.strip_prefix(var) {
            return rest == TAIL;
        }
    }
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into()).to_ascii_lowercase();
    s == format!("{}{TAIL}", root.trim_end_matches('\\'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeRegistry, FakeRestoreLog};
    use std::sync::Arc;

    type Fixture = (Arc<FakeRegistry>, Arc<FakeRestoreLog>, RdsApplier<Arc<FakeRegistry>, Arc<FakeRestoreLog>>);

    fn applier() -> Fixture {
        let reg = Arc::new(FakeRegistry::default());
        let log = Arc::new(FakeRestoreLog::default());
        let a = RdsApplier::new(reg.clone(), log.clone());
        (reg, log, a)
    }

    fn windows_defaults(reg: &FakeRegistry) {
        reg.put(TS, "fDenyTSConnections", RegData::Dword(1));
        reg.put(TS_RDP_TCP, "PortNumber", RegData::Dword(3389));
        reg.put(TS_RDP_TCP, "UserAuthentication", RegData::Dword(1));
        reg.put(TS_RDP_TCP, "SecurityLayer", RegData::Dword(1));
        reg.put(&format!(r"{TS_ADDINS}\Clip Redirector"), "Name", RegData::Sz("RDPClip".into()));
        reg.put(&format!(r"{TS_ADDINS}\Clip Redirector"), "Type", RegData::Dword(3));
        reg.put(&format!(r"{TS_ADDINS}\DND Redirector"), "Name", RegData::Sz("RDPDND".into()));
        reg.put(&format!(r"{TS_ADDINS}\DND Redirector"), "Type", RegData::Dword(3));
        reg.put(&format!(r"{TS_ADDINS}\Dynamic VC"), "Type", RegData::Dword(UNLIMITED));
    }

    #[test]
    fn rows_are_the_write_rows_only() {
        let rows = rds_rows(true);
        let names: Vec<_> = rows.iter().map(|r| r.value).collect();
        for forbidden in ["fSingleSessionPerUser", "LimitBlankPasswordUse", "PortNumber", "ContainerMode", "IddCxDebugCtrl"] {
            assert!(!names.contains(&forbidden), "{forbidden}");
        }
        assert!(names.contains(&"SetGpuRealtimePriority"));
        assert!(!rds_rows(false).iter().any(|r| r.value == "SetGpuRealtimePriority"));
        let wrapper: Vec<_> = rows.iter().filter(|r| r.path == RdsPath::ClientWrapperOnly).map(|r| r.value).collect();
        assert_eq!(wrapper, vec!["EnableConcurrentSessions", "AllowMultipleTSSessions"]);
        let deny = rows.iter().find(|r| r.value == "fDenyTSConnections").unwrap();
        assert_eq!((deny.key, &deny.data), (TS, &RegData::Dword(0)));
    }

    #[test]
    fn apply_saves_originals_once_and_restore_puts_them_back() {
        let (reg, log, a) = applier();
        windows_defaults(&reg);
        let before = reg.snapshot();
        let rep = a.apply(false).unwrap();
        assert!(rep.changed.contains(&format!(r"HKLM\{TS}\fDenyTSConnections")));
        assert!(rep.unchanged.contains(&format!(r"HKLM\{TS_RDP_TCP}\UserAuthentication")));
        assert!(!rep.changed.iter().any(|c| c.contains("EnableConcurrentSessions")));
        assert!(rep.warnings.is_empty(), "{:?}", rep.warnings);
        assert_eq!(reg.get(TS, "fDenyTSConnections").unwrap(), Some(RegData::Dword(0)));
        // intent + done per changed row
        assert_eq!(log.all().len(), rep.changed.len() * 2);
        assert_eq!(log.all()[0].status, RecordStatus::Intent);

        // Second apply: nothing changes, originals are not overwritten with our values.
        reg.put(TS, "fDenyTSConnections", RegData::Dword(1)); // Windows flipped it back
        let rep2 = a.apply(false).unwrap();
        assert_eq!(rep2.changed, vec![format!(r"HKLM\{TS}\fDenyTSConnections")]);
        let saved = reg.get(ORIGINALS_KEY, &format!(r"{TS}\fDenyTSConnections")).unwrap().unwrap();
        assert_eq!(saved, RegData::Sz(r#"{"type":"dword","value":1}"#.into()));

        let r = a.restore_all().unwrap();
        assert!(r.skipped_changed.is_empty(), "{:?}", r.skipped_changed);
        assert_eq!(r.restored.len(), rep.changed.len());
        assert_eq!(reg.snapshot(), before, "registry back to the original state");
    }

    #[test]
    fn restore_skips_values_changed_after_us() {
        let (reg, _log, a) = applier();
        windows_defaults(&reg);
        a.apply(false).unwrap();
        reg.put(TS_RDP_TCP, "Shadow", RegData::Dword(4));
        let r = a.restore_all().unwrap();
        assert_eq!(r.skipped_changed, vec![format!(r"HKLM\{TS_RDP_TCP}\Shadow")]);
        assert_eq!(reg.get(TS_RDP_TCP, "Shadow").unwrap(), Some(RegData::Dword(4)));
        assert_eq!(reg.get(ORIGINALS_KEY, &format!(r"{TS_RDP_TCP}\Shadow")).unwrap(), None);
    }

    #[test]
    fn policy_values_are_reported_not_fought() {
        let (reg, _log, a) = applier();
        windows_defaults(&reg);
        reg.put(POL, "Shadow", RegData::Dword(2)); // someone's GPO
        reg.put(POL, "fDenyTSConnections", RegData::Dword(1));
        reg.put(POL, "UserAuthentication", RegData::Dword(0));
        let rep = a.apply(false).unwrap();
        assert_eq!(reg.get(POL, "Shadow").unwrap(), Some(RegData::Dword(2)));
        assert!(rep.policy_overrides.contains(&format!(r"HKLM\{POL}\Shadow")));
        assert!(rep.policy_overrides.contains(&format!(r"HKLM\{POL}\fDenyTSConnections")));
        assert!(rep.policy_overrides.contains(&format!(r"HKLM\{POL}\UserAuthentication")));
        assert!(rep.warnings.contains(&RdsWarning::PolicyDeniesRdp));
        // An absent policy value is ours to write.
        assert_eq!(reg.get(POL, "bEnumerateHWBeforeSW").unwrap(), Some(RegData::Dword(1)));
        // We never wrote Pol\fDenyTSConnections.
        assert_eq!(reg.get(POL, "fDenyTSConnections").unwrap(), Some(RegData::Dword(1)));
    }

    #[test]
    fn gpo_change_after_our_write_is_reported_not_rewritten() {
        let (reg, _log, a) = applier();
        windows_defaults(&reg);
        a.apply(false).unwrap();
        assert_eq!(reg.get(POL, "Shadow").unwrap(), Some(RegData::Dword(0)));
        // A domain GPO later sets the value we wrote first (saved original = absent).
        reg.put(POL, "Shadow", RegData::Dword(2));
        let rep = a.apply(false).unwrap();
        assert_eq!(reg.get(POL, "Shadow").unwrap(), Some(RegData::Dword(2)), "not fought");
        assert!(rep.policy_overrides.contains(&format!(r"HKLM\{POL}\Shadow")));
        assert!(!rep.changed.contains(&format!(r"HKLM\{POL}\Shadow")));
        assert!(a.verify_policy().contains(&RdsWarning::PolicyOverride { value: format!(r"HKLM\{POL}\Shadow") }));
        // Deleted again (GPO removed): absent means ours is written again.
        reg.delete(POL, "Shadow").unwrap();
        let rep = a.apply(false).unwrap();
        assert!(rep.changed.contains(&format!(r"HKLM\{POL}\Shadow")));
    }

    #[test]
    fn wrapper_rows_only_on_request() {
        let (reg, _log, a) = applier();
        windows_defaults(&reg);
        let rep = a.apply(true).unwrap();
        assert!(rep.changed.contains(&format!(r"HKLM\{TS_LICENSING_CORE}\EnableConcurrentSessions")));
        assert!(rep.changed.contains(&format!(r"HKLM\{WINLOGON}\AllowMultipleTSSessions")));
        a.restore_all().unwrap();
        assert_eq!(reg.get(WINLOGON, "AllowMultipleTSSessions").unwrap(), None);
    }

    #[test]
    fn addins_are_verified_only() {
        let (reg, _log, a) = applier();
        windows_defaults(&reg);
        reg.remove(&format!(r"{TS_ADDINS}\DND Redirector"), "Name");
        let w = a.verify_addins();
        assert_eq!(w, vec![RdsWarning::AddInMissing { key: format!(r"HKLM\{TS_ADDINS}\DND Redirector") }]);
        a.apply(false).unwrap();
        assert_eq!(reg.get(&format!(r"{TS_ADDINS}\DND Redirector"), "Name").unwrap(), None, "not rewritten");
    }

    #[test]
    fn listener_values() {
        let reg = FakeRegistry::default();
        assert_eq!(listener_from_registry(&reg).unwrap_err().code(), "rds_not_ready");
        reg.put(TS_RDP_TCP, "PortNumber", RegData::Dword(3390));
        reg.put(TS_RDP_TCP, "UserAuthentication", RegData::Dword(1));
        reg.put(TS_RDP_TCP, "MaxInstanceCount", RegData::Dword(UNLIMITED));
        let l = listener_from_registry(&reg).unwrap();
        assert_eq!((l.port, l.nla, l.max_instance_count), (3390, true, None));
        reg.put(POL, "MaxInstanceCount", RegData::Dword(4));
        reg.put(POL, "UserAuthentication", RegData::Dword(0));
        let l = listener_from_registry(&reg).unwrap();
        assert_eq!((l.nla, l.max_instance_count), (false, Some(4)));
        reg.put(TS_RDP_TCP, "PortNumber", RegData::Dword(70000));
        assert!(listener_from_registry(&reg).is_err());
    }

    #[test]
    fn licensing_mode_policy_first() {
        let reg = FakeRegistry::default();
        assert_eq!(licensing_mode_from_registry(&reg).unwrap(), None);
        reg.put(TS_RCM_LICENSING_CORE, "LicensingMode", RegData::Dword(5));
        assert_eq!(licensing_mode_from_registry(&reg).unwrap(), Some(5));
        reg.put(POL, "LicensingMode", RegData::Dword(4));
        assert_eq!(licensing_mode_from_registry(&reg).unwrap(), Some(4));
        assert!(LicensingInfo { mode: Some(4), grace_days_left: None }.configured());
        assert!(!LicensingInfo { mode: Some(5), grace_days_left: None }.configured());
    }

    #[test]
    fn stock_service_dll() {
        assert!(termservice_is_stock(r"%SystemRoot%\System32\termsrv.dll"));
        assert!(termservice_is_stock(r"%systemroot%\system32\TERMSRV.DLL"));
        assert!(termservice_is_stock(r"%windir%\System32\termsrv.dll"));
        assert!(!termservice_is_stock(r"%ProgramFiles%\Other\TermWrap.dll"));
        assert!(!termservice_is_stock(r"%SystemRoot%\System32\rdpwrap.dll"));
        assert!(!termservice_is_stock(""));
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        assert!(termservice_is_stock(&format!(r"{root}\System32\termsrv.dll")));
    }
}
