// SPDX-License-Identifier: GPL-3.0-only
//! The one Terminal Server settings writer of Pulsar Seat (spec/local 00 §5.2.1, spec/local
//! 30 §6.1.1 / §6.2.13, spec/remote 30 §2.3). Both seat kinds use it: the `rds_loopback` /
//! `termsrv_wrapper` backends for local seats and [`crate::remote`] for remote seats
//! (merged from Pulsar Split's `split-session::rds` at merge-plan.md step 7).
//!
//! Only documented values are written, each journaled in **one** journal
//! (`journal\session.jsonl`, package [`PACKAGE`], intent → write → done) so a restore can
//! put back exactly what was there, including "absent". A value someone changed after us
//! is left alone. Group-policy values that win over ours are **reported, never fought**.
//!
//! ## Who needs which rows ([`Claim`])
//!
//! Each seat kind *claims* the writer; the rows written are the union of the rows of every
//! current claim ([`rows_for`]). Releasing a claim restores only the rows no remaining
//! claim needs, so stopping the local backend never takes a remote seat's rows away (and
//! the other way round). Claims live in the service process ([`TsClaims::process`]); the
//! journal outlives them, so an uninstall in another process still restores everything.
//!
//! ## Fixed order with the other Terminal Server writers (merge-plan.md §4 risk 2)
//!
//! 1. **These rows** (this module, `journal\session.jsonl`).
//! 2. **Second listener** `PulsarSeat-Loopback` with NLA off (local passwordless seats,
//!    [`crate::loopback_listener`], `journal\loopback-listener.jsonl`). It is created after
//!    step 1 (it copies RDP-Tcp's protocol values) and sets its own `UserAuthentication`,
//!    `SecurityLayer` and `Shadow`; this writer never touches the listener's key.
//! 3. **Termsrv wrapper** (experimental, [`crate::termwrap`], `journal\termwrap.jsonl`) owns
//!    `TermService\Parameters\ServiceDll` and the two wrapper rows
//!    (`EnableConcurrentSessions`, `AllowMultipleTSSessions`). This writer never writes
//!    them (Pulsar Split's "path B" rows here wrote them a second time; removed).
//!
//! Restore runs in reverse: termwrap (its own journal, on disable / uninstall), then the
//! listener, then these rows. The three value sets are disjoint, so a crash between two
//! steps leaves nothing that a later replay of the others would undo wrongly.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::SysError;
use crate::journal::{
    RecordStatus, RegValue, RestoreLog, RestoreRecord, StepAction, WallClock, registry_target, replay_plan, rfc3339_utc,
    split_registry_target, step_action,
};
use crate::registry::{Registry, get_string, get_u32};

pub const PACKAGE: &str = "session";
/// Journal record kind of a seat account the service created (remote seats). Never
/// replayed: accounts are never deleted automatically (their profiles hold data).
pub const ACCOUNT_KIND: &str = "account";

pub const TS: &str = r"SYSTEM\CurrentControlSet\Control\Terminal Server";
pub const TS_WINSTATIONS: &str = r"SYSTEM\CurrentControlSet\Control\Terminal Server\WinStations";
pub const TS_RDP_TCP: &str = r"SYSTEM\CurrentControlSet\Control\Terminal Server\WinStations\RDP-Tcp";
pub const TS_LICENSING_CORE: &str = r"SYSTEM\CurrentControlSet\Control\Terminal Server\Licensing Core";
pub const TS_RCM_LICENSING_CORE: &str = r"SYSTEM\CurrentControlSet\Control\Terminal Server\RCM\Licensing Core";
pub const TS_ADDINS: &str = r"SYSTEM\CurrentControlSet\Control\Terminal Server\AddIns";
pub const POL: &str = r"SOFTWARE\Policies\Microsoft\Windows NT\Terminal Services";
pub const KEYBOARD_LAYOUT: &str = r"SYSTEM\CurrentControlSet\Control\Keyboard Layout";
pub const TERMSERVICE_PARAMETERS: &str = r"SYSTEM\CurrentControlSet\Services\TermService\Parameters";
pub const SYSTEM_POLICIES: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System";

/// "No limit" for `MaxInstanceCount`.
pub const UNLIMITED: u32 = 0xFFFF_FFFF;

/// Which seat kind needs a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RowScope {
    /// Every RDP-based seat (local and remote).
    Common,
    /// Local seats only (`fSingleSessionPerUser`).
    Local,
    /// Remote seats only (spec/remote 30 §2.3).
    Remote,
    /// Remote seats with `SetGpuRealtimePriority` on (optional, default on).
    RemoteGpu,
}

/// One value the writer may write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RdsRow {
    pub key: &'static str,
    pub value: &'static str,
    pub data: u32,
    /// Snake_case note shown in the manager when this row was changed.
    pub note: &'static str,
    pub scope: RowScope,
}

impl RdsRow {
    pub fn target(&self) -> String {
        registry_target(self.key, self.value)
    }

    /// A row under the Terminal Services **policy** key: written only while the value is
    /// absent; a present value is someone's (local or domain GPO) and wins.
    pub fn is_policy(&self) -> bool {
        self.key == POL
    }
}

/// A claim on the writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Claim {
    /// RDP-based local seat backends (`rds_loopback`, `termsrv_wrapper`).
    Local,
    /// Remote seats; `gpu_realtime` adds `SetGpuRealtimePriority`.
    Remote { gpu_realtime: bool },
}

/// Union of claims.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RowSet {
    pub local: bool,
    pub remote: bool,
    pub gpu_realtime: bool,
}

impl RowSet {
    pub fn of(c: Claim) -> Self {
        match c {
            Claim::Local => Self { local: true, ..Self::default() },
            Claim::Remote { gpu_realtime } => Self { remote: true, gpu_realtime, ..Self::default() },
        }
    }

    pub fn is_empty(&self) -> bool {
        !self.local && !self.remote
    }

    fn includes(&self, scope: RowScope) -> bool {
        match scope {
            RowScope::Common => !self.is_empty(),
            RowScope::Local => self.local,
            RowScope::Remote => self.remote,
            RowScope::RemoteGpu => self.remote && self.gpu_realtime,
        }
    }
}

/// Every row, in apply order:
///
/// - `fDenyTSConnections = 0`: accept RDP connections (the loopback seats).
/// - `fSingleSessionPerUser = 0` (local): machine-wide; otherwise a second local seat with
///   the same account would take over the first seat's session (spec/local 30 §6.2.13).
///   Remote seats never share an account (the service refuses it), so they do not need it.
/// - `WinStations\SetGpuRealtimePriority = 1` (remote, optional).
/// - `RDP-Tcp\UserAuthentication = 1`: keep NLA on.
/// - `RDP-Tcp\Shadow = 0`: no shadowing of seat sessions.
/// - `RDP-Tcp\SecurityLayer = 2` (remote): TLS; the headless hold pins the certificate.
/// - `Keyboard Layout\IgnoreRemoteKeyboardLayout = 1` (remote): the session keeps the
///   user's own layout.
/// - Policy `bEnumerateHWBeforeSW = 1` and `Shadow = 0` (remote): only while absent.
/// - `fDisableAudioCapture` / `fDisableCam` are *not* written (media package).
pub fn rds_rows() -> Vec<RdsRow> {
    use RowScope::*;
    let r = |key, value, data, note, scope| RdsRow { key, value, data, note, scope };
    vec![
        r(TS, "fDenyTSConnections", 0, "rdp_connections_enabled", Common),
        r(TS, "fSingleSessionPerUser", 0, "single_session_per_user_off", Local),
        r(TS_WINSTATIONS, "SetGpuRealtimePriority", 1, "gpu_realtime_priority_on", RemoteGpu),
        r(TS_RDP_TCP, "UserAuthentication", 1, "nla_on", Common),
        r(TS_RDP_TCP, "Shadow", 0, "shadow_off", Common),
        r(TS_RDP_TCP, "SecurityLayer", 2, "tls_on", Remote),
        r(KEYBOARD_LAYOUT, "IgnoreRemoteKeyboardLayout", 1, "remote_keyboard_layout_ignored", Remote),
        r(POL, "bEnumerateHWBeforeSW", 1, "hardware_gpu_enabled", Remote),
        r(POL, "Shadow", 0, "shadow_policy_off", Remote),
    ]
}

/// The rows a set of claims needs, in apply order.
pub fn rows_for(set: RowSet) -> Vec<RdsRow> {
    rds_rows().into_iter().filter(|r| set.includes(r.scope)).collect()
}

/// Policy values that win over our listener / Terminal Server rows when set.
const POLICY_SHADOWED: [&str; 4] = ["fDenyTSConnections", "fSingleSessionPerUser", "UserAuthentication", "SecurityLayer"];

/// Read-only findings (remote seats' host info, apply reports).
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
    pub changed: Vec<String>,
    pub unchanged: Vec<String>,
    /// Policy values that override ours (`HKLM\key\value`).
    pub policy_overrides: Vec<String>,
    /// Snake_case notes for the manager.
    pub notes: Vec<String>,
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

/// Current claims of the service process. Also serialises the writers: a claim or release
/// holds [`TsClaims::writing`] for its whole journal + registry work, so the local backend
/// and the remote seat worker never interleave records in the shared journal.
#[derive(Debug, Default)]
pub struct TsClaims(Mutex<Vec<Claim>>, Mutex<()>);

impl TsClaims {
    /// The claims shared by every writer of this process (real wiring: the local session
    /// backend and the remote seat worker get the same object).
    pub fn process() -> Arc<TsClaims> {
        static P: OnceLock<Arc<TsClaims>> = OnceLock::new();
        P.get_or_init(|| Arc::new(TsClaims::default())).clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Claim>> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn writing(&self) -> std::sync::MutexGuard<'_, ()> {
        self.1.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Union of the current claims.
    pub fn set(&self) -> RowSet {
        union(self.lock().iter().copied())
    }
}

fn union(claims: impl Iterator<Item = Claim>) -> RowSet {
    claims.fold(RowSet::default(), |mut acc, c| {
        let s = RowSet::of(c);
        acc.local |= s.local;
        acc.remote |= s.remote;
        acc.gpu_realtime |= s.gpu_realtime;
        acc
    })
}

fn same_kind(a: &Claim, b: &Claim) -> bool {
    matches!((a, b), (Claim::Local, Claim::Local) | (Claim::Remote { .. }, Claim::Remote { .. }))
}

pub struct RdsApplier {
    reg: Arc<dyn Registry>,
    log: Arc<dyn RestoreLog>,
    clock: Arc<dyn WallClock>,
    claims: Arc<TsClaims>,
}

impl RdsApplier {
    /// Writer with its own (empty) claims. Real wiring adds [`Self::with_claims`].
    pub fn new(reg: Arc<dyn Registry>, log: Arc<dyn RestoreLog>, clock: Arc<dyn WallClock>) -> Self {
        Self { reg, log, clock, claims: Arc::new(TsClaims::default()) }
    }

    /// Shares `claims` with the other writers of this process.
    pub fn with_claims(mut self, claims: Arc<TsClaims>) -> Self {
        self.claims = claims;
        self
    }

    pub fn registry(&self) -> &Arc<dyn Registry> {
        &self.reg
    }

    fn record(&self, kind: &str, target: &str, prev: &RegValue, new: &RegValue, status: RecordStatus) -> Result<(), SysError> {
        let seq = self.log.read_all()?.iter().map(|r| r.seq).max().unwrap_or(0) + 1;
        self.log.append(&RestoreRecord {
            v: 1,
            seq,
            time: rfc3339_utc(self.clock.now_ms()),
            actor: "service".into(),
            package: PACKAGE.into(),
            kind: kind.into(),
            target: target.into(),
            previous: prev.clone(),
            new: new.clone(),
            status,
        })
    }

    /// Journals a seat account the service created (intent + done; never replayed).
    pub fn record_account(&self, user: &str) -> Result<(), SysError> {
        let _w = self.claims.writing();
        let created = RegValue::Sz("created".into());
        self.record(ACCOUNT_KIND, user, &RegValue::Absent, &created, RecordStatus::Intent)?;
        self.record(ACCOUNT_KIND, user, &RegValue::Absent, &created, RecordStatus::Done)
    }

    /// Policy values overriding ours for `set` (read-only).
    fn shadowing(&self, set: RowSet) -> Result<Vec<String>, SysError> {
        let rows = rows_for(set);
        let mut out = Vec::new();
        for name in POLICY_SHADOWED {
            let Some(ours) = rows.iter().find(|r| r.value == name && !r.is_policy()) else { continue };
            let pol = self.reg.get(POL, name)?;
            if pol != RegValue::Absent && pol != RegValue::Dword(ours.data) {
                out.push(registry_target(POL, name));
            }
        }
        Ok(out)
    }

    /// Policy values overriding the local rows (read-only).
    pub fn policy_overrides(&self) -> Result<Vec<String>, SysError> {
        self.shadowing(RowSet::of(Claim::Local))
    }

    /// Local claim (the RDP-based local backends). Idempotent.
    pub fn apply(&self) -> Result<ApplyReport, SysError> {
        self.claim(Claim::Local)
    }

    /// Adds `who` to the claims and writes every row of the union that differs; journals
    /// each change. Idempotent.
    pub fn claim(&self, who: Claim) -> Result<ApplyReport, SysError> {
        let _w = self.claims.writing();
        let set = {
            let mut c = self.claims.lock();
            c.retain(|x| !same_kind(x, &who));
            c.push(who);
            union(c.iter().copied())
        };
        self.write(set)
    }

    fn write(&self, set: RowSet) -> Result<ApplyReport, SysError> {
        let mut rep = ApplyReport { policy_overrides: self.shadowing(set)?, ..Default::default() };
        if self.reg.get(POL, "fDenyTSConnections")? == RegValue::Dword(1) {
            rep.notes.push("rdp_denied_by_policy".into());
            rep.warnings.push(RdsWarning::PolicyDeniesRdp);
        }
        for p in &rep.policy_overrides {
            rep.warnings.push(RdsWarning::PolicyOverride { value: p.clone() });
        }
        for r in rows_for(set) {
            let target = r.target();
            let want = RegValue::Dword(r.data);
            let cur = self.reg.get(r.key, r.value)?;
            if cur == want {
                rep.unchanged.push(target);
                continue;
            }
            if r.is_policy() && cur != RegValue::Absent {
                // Someone's GPO value (also when we wrote this row first and a GPO changed
                // it later): the policy wins (spec/remote 30 §2.3). Only absent is written.
                rep.warnings.push(RdsWarning::PolicyOverride { value: target.clone() });
                rep.policy_overrides.push(target);
                continue;
            }
            self.record("registry", &target, &cur, &want, RecordStatus::Intent)?;
            self.reg.set(r.key, r.value, &want)?;
            self.record("registry", &target, &cur, &want, RecordStatus::Done)?;
            log::info!("rds: set {target}");
            rep.notes.push(r.note.into());
            rep.changed.push(target);
        }
        for p in &rep.policy_overrides {
            rep.notes.push(format!("policy_override:{p}"));
        }
        rep.notes.extend(session_policy_notes(self.reg.as_ref())?);
        if set.remote {
            rep.warnings.extend(self.verify_addins());
        }
        Ok(rep)
    }

    /// Drops `who`'s claim and restores the rows no remaining claim needs.
    pub fn release(&self, who: Claim) -> Result<RestoreReport, SysError> {
        let _w = self.claims.writing();
        let remaining = {
            let mut c = self.claims.lock();
            c.retain(|x| !same_kind(x, &who));
            union(c.iter().copied())
        };
        self.restore_except(remaining)
    }

    /// Restores every row (uninstall, reset) and clears the claims.
    pub fn restore_all(&self) -> Result<RestoreReport, SysError> {
        let _w = self.claims.writing();
        self.claims.lock().clear();
        self.restore_except(RowSet::default())
    }

    /// Guarded reverse replay of the rows not in `keep`. Only targets of [`rds_rows`] are
    /// ever written back (the journal is data, not authority). Records of kept rows and
    /// account records stay in the journal; the rest is dropped.
    fn restore_except(&self, keep: RowSet) -> Result<RestoreReport, SysError> {
        let mut rep = RestoreReport::default();
        let lower = |rows: Vec<RdsRow>| rows.iter().map(|r| r.target().to_ascii_lowercase()).collect::<Vec<_>>();
        let allowed = lower(rds_rows());
        let kept = lower(rows_for(keep));
        let records = self.log.read_all()?;
        let registry_records: Vec<RestoreRecord> = records.iter().filter(|r| r.kind != ACCOUNT_KIND).cloned().collect();
        for step in replay_plan(&registry_records, PACKAGE) {
            let t = step.target.to_ascii_lowercase();
            if kept.contains(&t) {
                continue;
            }
            if !allowed.contains(&t) {
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
        let retained: Vec<RestoreRecord> = records
            .into_iter()
            .filter(|r| r.package != PACKAGE || r.kind == ACCOUNT_KIND || kept.contains(&r.target.to_ascii_lowercase()))
            .collect();
        for r in retained.iter().filter(|r| r.kind == ACCOUNT_KIND && r.status == RecordStatus::Done) {
            if !rep.accounts_left.contains(&r.target) {
                rep.accounts_left.push(r.target.clone());
            }
        }
        self.log.clear()?;
        for r in &retained {
            self.log.append(r)?;
        }
        Ok(rep)
    }

    /// Read-only policy check (remote seats' host info): shadowing policies plus our
    /// policy rows that hold someone else's value.
    pub fn verify_policy(&self, gpu_realtime: bool) -> Vec<RdsWarning> {
        let set = RowSet::of(Claim::Remote { gpu_realtime });
        let mut w = Vec::new();
        if self.reg.get(POL, "fDenyTSConnections").ok() == Some(RegValue::Dword(1)) {
            w.push(RdsWarning::PolicyDeniesRdp);
        }
        for p in self.shadowing(set).unwrap_or_default() {
            w.push(RdsWarning::PolicyOverride { value: p });
        }
        for r in rows_for(set).into_iter().filter(RdsRow::is_policy) {
            let cur = self.reg.get(r.key, r.value).unwrap_or(RegValue::Absent);
            if cur != RegValue::Absent && cur != RegValue::Dword(r.data) {
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
        check(registry_target(TS_ADDINS, "").trim_end_matches('\\').to_owned(), self.reg.key_exists(TS_ADDINS).unwrap_or(false));
        let expect: [(&str, Option<&str>, u32); 3] =
            [("Clip Redirector", Some("RDPClip"), 3), ("DND Redirector", Some("RDPDND"), 3), ("Dynamic VC", None, UNLIMITED)];
        for (sub, name, ty) in expect {
            let key = format!(r"{TS_ADDINS}\{sub}");
            let name_ok = match name {
                Some(n) => matches!(get_string(self.reg.as_ref(), &key, "Name"), Ok(Some(s)) if s.eq_ignore_ascii_case(n)),
                None => true,
            };
            let type_ok = self.reg.get(&key, "Type").ok() == Some(RegValue::Dword(ty));
            check(format!(r"HKLM\{key}"), name_ok && type_ok);
        }
        w
    }
}

/// Listener facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListenerInfo {
    pub port: u16,
    /// Effective `MaxInstanceCount` (policy first); `None` = unlimited.
    pub max_instance_count: Option<u32>,
    pub nla: bool,
    /// The listener only accepts IPv6 (the hold then connects to `::1`). Filled by the
    /// real probe from the TCP table.
    #[serde(default)]
    pub ipv6_only: bool,
    /// SHA-1 thumbprint of the listener's TLS certificate (pinned by the headless hold).
    /// Filled by the real probe.
    #[serde(default)]
    pub cert_sha1: Option<[u8; 20]>,
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
    /// Re-read at every start (the port may change while running).
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
    Ok(ListenerInfo { port, max_instance_count: max.filter(|m| *m != UNLIMITED), nla, ipv6_only: false, cert_sha1: None })
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
/// - `hardware_gpu_not_enabled`: the policy to render sessions on the hardware GPU is not on
///   (remote seats write it while absent).
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

    /// Windows defaults incl. the add-in keys (Split's fixture).
    fn windows_defaults(reg: &MemoryRegistry) {
        defaults(reg);
        reg.put(TS_RDP_TCP, "SecurityLayer", RegValue::Dword(1));
        reg.put(&format!(r"{TS_ADDINS}\Clip Redirector"), "Name", RegValue::Sz("RDPClip".into()));
        reg.put(&format!(r"{TS_ADDINS}\Clip Redirector"), "Type", RegValue::Dword(3));
        reg.put(&format!(r"{TS_ADDINS}\DND Redirector"), "Name", RegValue::Sz("RDPDND".into()));
        reg.put(&format!(r"{TS_ADDINS}\DND Redirector"), "Type", RegValue::Dword(3));
        reg.put(&format!(r"{TS_ADDINS}\Dynamic VC"), "Type", RegValue::Dword(UNLIMITED));
    }

    const REMOTE: Claim = Claim::Remote { gpu_realtime: true };

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
        assert!(!termservice_is_stock(r"%ProgramFiles%\Other\TermWrap.dll", None));
        assert!(!termservice_is_stock("", None));
    }

    // ---- remote seats (from Pulsar Split's split-session::rds) ----

    #[test]
    fn rows_per_kind_and_never_the_wrapper_rows() {
        let all: Vec<_> = rds_rows().iter().map(|r| r.value).collect();
        for forbidden in ["EnableConcurrentSessions", "AllowMultipleTSSessions", "LimitBlankPasswordUse", "PortNumber", "ContainerMode"] {
            assert!(!all.contains(&forbidden), "{forbidden}");
        }
        let names = |s| rows_for(s).iter().map(|r| r.value).collect::<Vec<_>>();
        let local = names(RowSet::of(Claim::Local));
        assert_eq!(local, ["fDenyTSConnections", "fSingleSessionPerUser", "UserAuthentication", "Shadow"]);
        let remote = names(RowSet::of(REMOTE));
        assert!(!remote.contains(&"fSingleSessionPerUser"), "remote seats never share an account");
        assert!(remote.contains(&"SetGpuRealtimePriority") && remote.contains(&"SecurityLayer"));
        assert!(!names(RowSet::of(Claim::Remote { gpu_realtime: false })).contains(&"SetGpuRealtimePriority"));
        assert!(rows_for(RowSet::default()).is_empty());
        let deny = rds_rows().into_iter().find(|r| r.value == "fDenyTSConnections").unwrap();
        assert_eq!((deny.key, deny.data), (TS, 0));
    }

    #[test]
    fn remote_apply_and_restore_put_originals_back() {
        let (reg, log, a) = applier();
        windows_defaults(&reg);
        let before = reg.snapshot();
        let rep = a.claim(REMOTE).unwrap();
        assert!(rep.changed.contains(&registry_target(TS, "fDenyTSConnections")));
        assert!(rep.unchanged.contains(&registry_target(TS_RDP_TCP, "UserAuthentication")));
        assert!(rep.warnings.is_empty(), "{:?}", rep.warnings);
        // intent + done per changed row
        assert_eq!(log.records.lock().unwrap().len(), rep.changed.len() * 2);
        // Windows flips a value back: written again; the restore still finds the original.
        reg.put(TS, "fDenyTSConnections", RegValue::Dword(1));
        assert_eq!(a.claim(REMOTE).unwrap().changed, vec![registry_target(TS, "fDenyTSConnections")]);
        let r = a.release(REMOTE).unwrap();
        assert!(r.skipped_changed.is_empty(), "{:?}", r.skipped_changed);
        assert_eq!(r.restored.len(), rep.changed.len());
        assert_eq!(reg.snapshot(), before, "registry back to the original state");
    }

    #[test]
    fn gpo_change_after_our_write_is_reported_not_rewritten() {
        let (reg, _log, a) = applier();
        windows_defaults(&reg);
        reg.put(POL, "Shadow", RegValue::Dword(2)); // someone's GPO
        let rep = a.claim(REMOTE).unwrap();
        assert_eq!(reg.get(POL, "Shadow").unwrap(), RegValue::Dword(2));
        assert!(rep.policy_overrides.contains(&registry_target(POL, "Shadow")));
        // An absent policy value is ours to write.
        assert_eq!(reg.get(POL, "bEnumerateHWBeforeSW").unwrap(), RegValue::Dword(1));
        reg.put(POL, "bEnumerateHWBeforeSW", RegValue::Dword(0)); // a domain GPO later
        let rep = a.claim(REMOTE).unwrap();
        assert_eq!(reg.get(POL, "bEnumerateHWBeforeSW").unwrap(), RegValue::Dword(0), "not fought");
        assert!(!rep.changed.contains(&registry_target(POL, "bEnumerateHWBeforeSW")));
        assert!(a.verify_policy(true).contains(&RdsWarning::PolicyOverride { value: registry_target(POL, "bEnumerateHWBeforeSW") }));
        reg.put(POL, "UserAuthentication", RegValue::Dword(0));
        reg.put(POL, "SecurityLayer", RegValue::Dword(0));
        let w = a.verify_policy(true);
        assert!(w.contains(&RdsWarning::PolicyOverride { value: registry_target(POL, "SecurityLayer") }), "{w:?}");
    }

    #[test]
    fn addins_are_verified_only() {
        let (reg, _log, a) = applier();
        windows_defaults(&reg);
        reg.put(&format!(r"{TS_ADDINS}\DND Redirector"), "Name", RegValue::Absent);
        let w = a.verify_addins();
        assert_eq!(w, vec![RdsWarning::AddInMissing { key: registry_target(TS_ADDINS, "DND Redirector") }]);
        let rep = a.claim(REMOTE).unwrap();
        assert!(rep.warnings.contains(&w[0]));
        assert_eq!(reg.get(&format!(r"{TS_ADDINS}\DND Redirector"), "Name").unwrap(), RegValue::Absent, "not rewritten");
        // The local claim alone does not report add-ins.
        a.release(REMOTE).unwrap();
        assert!(a.apply().unwrap().warnings.is_empty());
    }

    // ---- one writer for both kinds ----

    #[test]
    fn releasing_one_kind_keeps_the_rows_the_other_needs() {
        let (reg, log, a) = applier();
        windows_defaults(&reg);
        let before = reg.snapshot();
        a.claim(Claim::Local).unwrap();
        a.claim(REMOTE).unwrap();
        // Local backend deselected while remote seats run: only fSingleSessionPerUser goes.
        let r = a.release(Claim::Local).unwrap();
        assert_eq!(r.restored, vec![registry_target(TS, "fSingleSessionPerUser")]);
        assert_eq!(reg.get(TS, "fDenyTSConnections").unwrap(), RegValue::Dword(0), "remote still needs it");
        assert_eq!(reg.get(TS_RDP_TCP, "SecurityLayer").unwrap(), RegValue::Dword(2));
        assert!(!log.records.lock().unwrap().iter().any(|x| x.target.contains("fSingleSessionPerUser")));
        // Remote released last: everything back.
        a.release(REMOTE).unwrap();
        assert_eq!(reg.snapshot(), before);
        assert!(log.records.lock().unwrap().is_empty());
    }

    #[test]
    fn remote_release_keeps_local_rows_and_accounts_are_kept() {
        let claims = Arc::new(TsClaims::default());
        let reg = Arc::new(MemoryRegistry::default());
        let log = Arc::new(MemoryRestoreLog::default());
        windows_defaults(&reg);
        // Two writers (local backend, remote worker) on one journal and one claim set.
        let local = RdsApplier::new(reg.clone(), log.clone(), Arc::new(SystemWallClock)).with_claims(claims.clone());
        let remote = RdsApplier::new(reg.clone(), log.clone(), Arc::new(SystemWallClock)).with_claims(claims.clone());
        local.apply().unwrap();
        remote.claim(REMOTE).unwrap();
        remote.record_account("koltuk3").unwrap();
        assert_eq!(claims.set(), RowSet { local: true, remote: true, gpu_realtime: true });
        let r = remote.release(REMOTE).unwrap();
        assert!(r.restored.contains(&registry_target(TS_RDP_TCP, "SecurityLayer")));
        assert!(!r.restored.contains(&registry_target(TS, "fDenyTSConnections")), "local still needs it");
        assert_eq!(r.accounts_left, vec!["koltuk3".to_string()]);
        assert_eq!(reg.get(TS, "fSingleSessionPerUser").unwrap(), RegValue::Dword(0));
        // Uninstall: everything, accounts are reported and never replayed.
        let r = local.restore_all().unwrap();
        assert!(r.restored.contains(&registry_target(TS, "fDenyTSConnections")));
        assert_eq!(r.accounts_left, vec!["koltuk3".to_string()]);
        assert_eq!(reg.get(TS, "fDenyTSConnections").unwrap(), RegValue::Dword(1));
        assert!(claims.set().is_empty());
    }

    #[test]
    fn claims_replace_their_own_kind() {
        let c = TsClaims::default();
        assert!(c.set().is_empty());
        let (reg, _log, a) = applier();
        windows_defaults(&reg);
        let a = a.with_claims(Arc::new(c));
        a.claim(REMOTE).unwrap();
        a.claim(Claim::Remote { gpu_realtime: false }).unwrap();
        // The gpu row of the first claim is no longer needed but stays until a release;
        // the second claim alone does not write it.
        assert_eq!(a.claims.set(), RowSet { local: false, remote: true, gpu_realtime: false });
        let r = a.release(Claim::Remote { gpu_realtime: false }).unwrap();
        assert!(r.restored.contains(&registry_target(TS_WINSTATIONS, "SetGpuRealtimePriority")));
    }
}
