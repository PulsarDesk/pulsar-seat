// SPDX-License-Identifier: GPL-3.0-only
//! The [`Termwrap`] state machine over the system traits.
//!
//! Ordering rules (crash safety):
//! - enable: store `Enabled=1` + `WrapperPath` first, then journal `intent` → write →
//!   `done` for each value, then restart TermService. A failed restart rolls back.
//! - disable / block / uninstall: store `Enabled=0` + `RestorePending=1` first, then replay
//!   the journal (guarded, idempotent, allow-listed targets only), fall back to the stock
//!   ServiceDll if the journal is lost, clear the journal, restart TermService if
//!   ServiceDll changed, and only then clear `RestorePending`. A crash anywhere in between
//!   leaves `Enabled=0`, so a boot check never re-enables what the operator disabled; the
//!   next disable / boot check / uninstall finishes the job.
//! - A ServiceDll we did not write is never touched (hands off). "We wrote it" comes only
//!   from our own HKLM state (`Enabled` or `RestorePending`, plus `WrapperPath`), never from
//!   the journal file.
//! - The journal is data, not authority: only the three values this package writes are
//!   replayed, and a ServiceDll `previous` is restored only if it is the stock value or a
//!   wrapper that passes every check now; anything else becomes the stock value.

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use crate::journal::{
    PACKAGE, RecordStatus, RegValue, ReplayStep, RestoreLog, RestoreRecord, StepAction, registry_target,
    replay_plan, rfc3339_utc, split_registry_target, step_action,
};
use crate::paths::{STOCK_SERVICE_DLL, is_stock_service_dll, paths_equal, validate_wrapper_path};
use crate::state::{self, StoredState};
use crate::support::{GateResult, WrapperKind, gate, parse_support_table};
use crate::system::{
    ALLOW_MULTIPLE_TS_SESSIONS, Clock, ENABLE_CONCURRENT_SESSIONS, FileInspector, HostEdition, HostFacts,
    LICENSING_CORE, Registry, SERVICE_DLL, TERMSERVICE_PARAMS, TermServiceControl, WINLOGON, WrapperDiscovery,
    describe_candidate,
};
use crate::version::FileVersion;
use crate::{
    BlockReason, SignatureState, TermwrapError, TermwrapState, TermwrapStatus, WARNING_VERSION, WrapperCandidate,
    pe,
};

pub trait Termwrap: Send + Sync {
    fn status(&self) -> Result<TermwrapStatus, TermwrapError>;
    /// Needs `ack_warning_version == WARNING_VERSION`, a client SKU, stock ServiceDll
    /// (else ForeignActive), a passing gate and a present wrapper. Journals the original
    /// ServiceDll + the two "path B only" RDS rows, then restarts TermService.
    fn enable(&self, wrapper: &Path, ack_warning_version: u32) -> Result<TermwrapStatus, TermwrapError>;
    /// Restores the saved original ServiceDll (or %SystemRoot%\System32\termsrv.dll if the
    /// journal is lost) and the RDS rows; restarts TermService.
    fn disable(&self) -> Result<TermwrapStatus, TermwrapError>;
    /// Boot-time check: if we enabled it and the gate now fails (Windows update), disable
    /// automatically and return Blocked.
    fn boot_check(&self) -> Result<TermwrapStatus, TermwrapError>;
}

#[derive(Debug, Clone)]
pub struct TermwrapPolicy {
    /// Accept a wrapper DLL with no Authenticode signature. Default `false` (fail closed).
    /// A signed-but-untrusted DLL is refused regardless. The upstream RDP Wrapper release
    /// is, as far as public pages say, unsigned (open question termwrap-1).
    pub allow_unsigned: bool,
    /// Per SCM wait (each stop, the start).
    pub restart_timeout: Duration,
}

impl Default for TermwrapPolicy {
    fn default() -> Self {
        Self { allow_unsigned: false, restart_timeout: Duration::from_secs(60) }
    }
}

/// Everything the engine touches.
pub struct TermwrapDeps {
    pub scm: Arc<dyn TermServiceControl>,
    pub discovery: Arc<dyn WrapperDiscovery>,
    pub files: Arc<dyn FileInspector>,
    pub registry: Arc<dyn Registry>,
    pub host: Arc<dyn HostFacts>,
    pub journal: Arc<dyn RestoreLog>,
    pub clock: Arc<dyn Clock>,
}

/// The termwrap implementation. Real Windows deps: [`WindowsTermwrap::system`].
pub struct WindowsTermwrap {
    d: TermwrapDeps,
    policy: TermwrapPolicy,
    op: Mutex<()>,
}

type Block = (BlockReason, String);

impl WindowsTermwrap {
    pub fn new(deps: TermwrapDeps, policy: TermwrapPolicy) -> Self {
        Self { d: deps, policy, op: Mutex::new(()) }
    }

    fn lock(&self) -> MutexGuard<'_, ()> {
        self.op.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn is_stock(&self, sd: &str) -> bool {
        is_stock_service_dll(sd, self.d.host.system_root().as_deref())
    }

    /// ServiceDll currently points at something we wrote. Only our own HKLM state counts
    /// (the journal file is never evidence of ownership).
    fn owned(&self, st: &StoredState, sd: &str) -> Result<bool, TermwrapError> {
        Ok(!self.is_stock(sd)
            && st.owns_redirection()
            && st.wrapper_path.as_deref().is_some_and(|w| paths_equal(w, sd)))
    }

    fn host_block(&self) -> Result<Option<Block>, TermwrapError> {
        if self.d.host.reference_product_present() {
            return Ok(Some((BlockReason::ReferenceProductPresent, String::new())));
        }
        Ok(match self.d.host.edition()? {
            HostEdition::Client { .. } => None,
            HostEdition::Home { edition_id } => Some((BlockReason::EditionUnsupported, edition_id)),
            HostEdition::Server { .. } => Some((BlockReason::NotClientSku, String::new())),
        })
    }

    /// Every per-wrapper check, fail closed. Returns the candidate on success.
    fn check_wrapper(
        &self,
        dll: &Path,
        termsrv: &Result<FileVersion, String>,
    ) -> Result<WrapperCandidate, Block> {
        let f = self.d.files.as_ref();
        let shown = dll.display().to_string();
        if !f.exists(dll) {
            return Err((BlockReason::WrapperMissing, shown));
        }
        let cand = describe_candidate(f, dll);
        if cand.kind == WrapperKind::Unknown {
            return Err((BlockReason::WrapperConfigInvalid, format!("{shown}: unknown wrapper, no support table")));
        }
        let host = self.d.host.native_machine();
        match f.pe_machine(dll) {
            Some(m) if m == host => {}
            m => {
                let w = m.map(pe::machine_name).unwrap_or("not a PE image");
                return Err((
                    BlockReason::WrapperWrongArchitecture,
                    format!("{shown}: wrapper {w}, host {}", pe::machine_name(host)),
                ));
            }
        }
        match cand.signed {
            SignatureState::Trusted => {}
            SignatureState::Unsigned if self.policy.allow_unsigned => {
                log::warn!("termwrap: wrapper has no signature; accepted by policy");
            }
            SignatureState::Unsigned => return Err((BlockReason::WrapperUnsigned, format!("{shown}: unsigned"))),
            SignatureState::Untrusted => {
                return Err((BlockReason::WrapperUnsigned, format!("{shown}: signature does not verify")));
            }
        }
        let Some(config) = cand.config_file.clone() else {
            return Err((BlockReason::WrapperConfigInvalid, format!("{shown}: support file missing")));
        };
        for p in [dll, config.as_path()] {
            match f.writable_by_non_admins(p) {
                Ok(false) => {}
                Ok(true) => return Err((BlockReason::WrapperInsecureAcl, p.display().to_string())),
                Err(e) => {
                    return Err((BlockReason::WrapperInsecureAcl, format!("{}: ACL unreadable: {e}", p.display())));
                }
            }
        }
        let text = f
            .read_text(&config)
            .map_err(|e| (BlockReason::WrapperConfigInvalid, format!("{}: {e}", config.display())))?;
        let table = parse_support_table(&cand.kind, &text)
            .map_err(|e| (BlockReason::WrapperConfigInvalid, format!("{}: {e}", config.display())))?;
        let version = termsrv.clone().map_err(|e| (BlockReason::TermsrvVersionUnreadable, e))?;
        match gate(version, &table) {
            GateResult::Supported => Ok(cand),
            GateResult::Unknown => Err((BlockReason::UnknownTermsrvBuild { version }, String::new())),
        }
    }

    fn termsrv(&self) -> Result<FileVersion, String> {
        self.d.scm.termsrv_version().map_err(|e| e.to_string())
    }

    fn status_locked(&self) -> Result<TermwrapStatus, TermwrapError> {
        let st = state::load(self.d.registry.as_ref())?;
        let sd = self.d.scm.service_dll()?;
        let termsrv = self.termsrv();
        let mut candidates = self.d.discovery.candidates();
        if let Some(wp) = &st.wrapper_path {
            let p = Path::new(wp);
            let known = candidates.iter().any(|c| c.service_dll.to_str().is_some_and(|s| paths_equal(s, wp)));
            if !known && self.d.files.exists(p) {
                candidates.push(describe_candidate(self.d.files.as_ref(), p));
            }
        }
        let state = if self.is_stock(&sd) {
            match &st.blocked {
                Some(reason) => TermwrapState::Blocked { reason: reason.clone() },
                None => TermwrapState::Off,
            }
        } else if self.owned(&st, &sd)? {
            TermwrapState::On { wrapper: describe_candidate(self.d.files.as_ref(), Path::new(&sd)) }
        } else {
            TermwrapState::ForeignActive { service_dll: sd }
        };
        let supported = self.host_block()?.is_none()
            && candidates.iter().any(|c| self.check_wrapper(&c.service_dll, &termsrv).is_ok());
        Ok(TermwrapStatus {
            state,
            termsrv_version: termsrv.ok(),
            supported,
            candidates,
            warning_version: WARNING_VERSION,
        })
    }

    // ---- journaled writes ----

    fn is_service_dll(path: &str, name: &str) -> bool {
        path.eq_ignore_ascii_case(TERMSERVICE_PARAMS) && name.eq_ignore_ascii_case(SERVICE_DLL)
    }

    fn read_value(&self, path: &str, name: &str) -> Result<RegValue, TermwrapError> {
        if Self::is_service_dll(path, name) {
            Ok(RegValue::ExpandSz(self.d.scm.service_dll()?))
        } else {
            self.d.registry.get(path, name)
        }
    }

    fn write_value(&self, path: &str, name: &str, v: &RegValue) -> Result<(), TermwrapError> {
        if Self::is_service_dll(path, name) {
            match v {
                RegValue::ExpandSz(s) | RegValue::Sz(s) if !s.trim().is_empty() => self.d.scm.set_service_dll(s),
                // Never delete or blank ServiceDll: TermService would not start.
                _ => self.d.scm.set_service_dll(STOCK_SERVICE_DLL),
            }
        } else {
            self.d.registry.set(path, name, v)
        }
    }

    fn record(&self, seq: u64, target: &str, prev: &RegValue, new: &RegValue, status: RecordStatus) -> RestoreRecord {
        RestoreRecord {
            v: 1,
            seq,
            time: rfc3339_utc(self.d.clock.now_ms()),
            actor: "service".into(),
            package: PACKAGE.into(),
            kind: "registry".into(),
            target: target.into(),
            previous: prev.clone(),
            new: new.clone(),
            status,
        }
    }

    /// Returns true if the value changed.
    fn journaled_set(&self, path: &str, name: &str, new: RegValue) -> Result<bool, TermwrapError> {
        let prev = self.read_value(path, name)?;
        if prev == new {
            return Ok(false);
        }
        let seq = self.d.journal.read_all()?.iter().map(|r| r.seq).max().map_or(1, |m| m + 1);
        let target = registry_target(path, name);
        self.d.journal.append(&self.record(seq, &target, &prev, &new, RecordStatus::Intent))?;
        self.write_value(path, name, &new)?;
        self.d.journal.append(&self.record(seq + 1, &target, &prev, &new, RecordStatus::Done))?;
        Ok(true)
    }

    /// Point ServiceDll at `wrapper` and set the two RDS rows. True if ServiceDll changed.
    fn apply(&self, wrapper: &str) -> Result<bool, TermwrapError> {
        let changed = self.journaled_set(TERMSERVICE_PARAMS, SERVICE_DLL, RegValue::ExpandSz(wrapper.to_owned()))?;
        self.journaled_set(LICENSING_CORE, ENABLE_CONCURRENT_SESSIONS, RegValue::Dword(1))?;
        self.journaled_set(WINLOGON, ALLOW_MULTIPLE_TS_SESSIONS, RegValue::Dword(1))?;
        Ok(changed)
    }

    /// The values this package writes, as `(key path, value name)`. Journal records for
    /// any other target are ignored.
    fn allowed_target(target: &str) -> Option<(&'static str, &'static str)> {
        let (path, name) = split_registry_target(target)?;
        [
            (TERMSERVICE_PARAMS, SERVICE_DLL),
            (LICENSING_CORE, ENABLE_CONCURRENT_SESSIONS),
            (WINLOGON, ALLOW_MULTIPLE_TS_SESSIONS),
        ]
        .into_iter()
        .find(|(p, n)| p.eq_ignore_ascii_case(path) && n.eq_ignore_ascii_case(name))
    }

    /// A ServiceDll `previous` from the journal: kept only if it is the stock value or a
    /// wrapper that passes every check now; anything else becomes the stock value.
    fn safe_service_dll_previous(&self, prev: &RegValue, termsrv: &Result<FileVersion, String>) -> RegValue {
        if let RegValue::ExpandSz(v) | RegValue::Sz(v) = prev {
            if self.is_stock(v) {
                return prev.clone();
            }
            if let Ok(w) = validate_wrapper_path(Path::new(v)) {
                if self.check_wrapper(Path::new(&w), termsrv).is_ok() {
                    return RegValue::ExpandSz(w);
                }
            }
            log::warn!("termwrap journal: ServiceDll {v:?} does not pass the checks; restoring stock instead");
        }
        RegValue::ExpandSz(STOCK_SERVICE_DLL.into())
    }

    /// Guarded reverse replay + lost-journal fallback, then clear the journal.
    /// True if ServiceDll changed (TermService needs a restart).
    fn restore_values(&self, st: &StoredState) -> Result<bool, TermwrapError> {
        let before = self.d.scm.service_dll()?;
        // ServiceDll is replayed only if it is stock or ours right now; a foreign value is
        // never touched, whatever the journal says.
        let service_dll_ours = self.is_stock(&before) || self.owned(st, &before)?;
        let (records, journal_ok) = match self.d.journal.read_all() {
            Ok(r) => (r, true),
            Err(e) => {
                log::error!("termwrap: journal not usable ({e}); restoring the stock ServiceDll only");
                (Vec::new(), false)
            }
        };
        let termsrv = self.termsrv();
        for step in replay_plan(&records) {
            let Some((path, name)) = Self::allowed_target(&step.target) else {
                log::warn!("termwrap journal: target {:?} is not ours; skipped", step.target);
                continue;
            };
            let previous = if Self::is_service_dll(path, name) {
                if !service_dll_ours {
                    log::warn!("termwrap: ServiceDll is foreign; journal entry left alone");
                    continue;
                }
                self.safe_service_dll_previous(&step.previous, &termsrv)
            } else {
                match &step.previous {
                    RegValue::Dword(_) | RegValue::Absent => step.previous.clone(),
                    other => {
                        log::warn!("termwrap journal: {} has unexpected previous {other:?}; skipped", step.target);
                        continue;
                    }
                }
            };
            let step = ReplayStep { previous, ..step };
            let current = self.read_value(path, name)?;
            match step_action(&step, &current) {
                StepAction::Restore => self.write_value(path, name, &step.previous)?,
                StepAction::AlreadyRestored => {}
                StepAction::Conflict => {
                    log::warn!("termwrap: {} changed by someone else since we set it; left as is", step.target);
                }
            }
        }
        // Journal lost or incomplete: ServiceDll still points at our wrapper.
        let now = self.d.scm.service_dll()?;
        if self.owned(st, &now)? {
            log::warn!("termwrap: no journal entry for ServiceDll; writing the stock value");
            self.d.scm.set_service_dll(STOCK_SERVICE_DLL)?;
        }
        match self.d.journal.clear() {
            Ok(()) => {}
            Err(e) if !journal_ok => log::error!("termwrap: untrusted journal not cleared: {e}"),
            Err(e) => return Err(e),
        }
        Ok(!paths_equal(&before, &self.d.scm.service_dll()?))
    }

    fn restart(&self) -> Result<(), TermwrapError> {
        self.d.scm.restart(self.policy.restart_timeout).map_err(|e| match e {
            TermwrapError::TermserviceRestart(_) => e,
            other => TermwrapError::TermserviceRestart(other.to_string()),
        })
    }

    /// Store "off" (optionally with a block reason) first, then restore everything.
    /// `RestorePending` stays set until the restore and the restart are done.
    fn switch_off(&self, st: &StoredState, blocked: Option<BlockReason>) -> Result<(), TermwrapError> {
        let resumed = st.restore_pending;
        let pending = StoredState {
            enabled: false,
            wrapper_path: st.wrapper_path.clone(),
            ack_warning_version: st.ack_warning_version,
            blocked,
            restore_pending: st.owns_redirection(),
        };
        state::save(self.d.registry.as_ref(), &pending)?;
        let changed = self.restore_values(&pending)?;
        // A resumed switch-off may have changed ServiceDll before the crash without the
        // restart that should have followed.
        if changed || resumed {
            self.restart()?;
        }
        if pending.restore_pending {
            state::save(self.d.registry.as_ref(), &StoredState { restore_pending: false, ..pending })?;
        }
        Ok(())
    }

    /// Apply + restart; on any failure roll back (best effort) and return the error.
    fn apply_or_rollback(&self, wrapper: &str) -> Result<(), TermwrapError> {
        let res = self.apply(wrapper).and_then(|changed| if changed { self.restart() } else { Ok(()) });
        if let Err(e) = res {
            log::error!("termwrap: enabling failed ({}), rolling back", e.code());
            let st = state::load(self.d.registry.as_ref()).unwrap_or_default();
            if let Err(r) = self.switch_off(&st, None) {
                log::error!("termwrap: rollback failed: {} ({r})", r.code());
            }
            return Err(e);
        }
        Ok(())
    }

    /// Uninstall / `RestoreHook`: replay this package's journal in reverse, store off,
    /// restart TermService if ServiceDll changed. Never touches a foreign ServiceDll.
    pub fn restore_all(&self) -> Result<(), TermwrapError> {
        let _g = self.lock();
        let st = state::load(self.d.registry.as_ref())?;
        let sd = self.d.scm.service_dll()?;
        if !self.is_stock(&sd) && !self.owned(&st, &sd)? {
            log::warn!("termwrap: ServiceDll {sd:?} is not ours; uninstall leaves it and the RDS rows alone");
            return Ok(());
        }
        self.switch_off(&st, st.blocked.clone())
    }
}

impl Termwrap for WindowsTermwrap {
    fn status(&self) -> Result<TermwrapStatus, TermwrapError> {
        let _g = self.lock();
        self.status_locked()
    }

    fn enable(&self, wrapper: &Path, ack_warning_version: u32) -> Result<TermwrapStatus, TermwrapError> {
        if ack_warning_version != WARNING_VERSION {
            return Err(TermwrapError::WarningNotAcknowledged { got: ack_warning_version, expected: WARNING_VERSION });
        }
        let _g = self.lock();
        if let Some((r, d)) = self.host_block()? {
            return Err(r.into_error(d));
        }
        let wrapper = validate_wrapper_path(wrapper)?;
        let st = state::load(self.d.registry.as_ref())?;
        let sd = self.d.scm.service_dll()?;
        let owned = self.owned(&st, &sd)?;
        if !self.is_stock(&sd) && !owned {
            return Err(TermwrapError::ForeignWrapper(sd));
        }
        self.check_wrapper(Path::new(&wrapper), &self.termsrv()).map_err(|(r, d)| r.into_error(d))?;
        state::save(
            self.d.registry.as_ref(),
            &StoredState {
                enabled: true,
                wrapper_path: Some(wrapper.clone()),
                ack_warning_version,
                blocked: None,
                restore_pending: false,
            },
        )?;
        self.apply_or_rollback(&wrapper)?;
        log::info!("termwrap: enabled");
        self.status_locked()
    }

    fn disable(&self) -> Result<TermwrapStatus, TermwrapError> {
        let _g = self.lock();
        let st = state::load(self.d.registry.as_ref())?;
        let sd = self.d.scm.service_dll()?;
        if !self.is_stock(&sd) && !self.owned(&st, &sd)? {
            return Err(TermwrapError::ForeignWrapper(sd));
        }
        self.switch_off(&st, None)?;
        log::info!("termwrap: disabled");
        self.status_locked()
    }

    fn boot_check(&self) -> Result<TermwrapStatus, TermwrapError> {
        let _g = self.lock();
        let st = state::load(self.d.registry.as_ref())?;
        let sd = self.d.scm.service_dll()?;
        if !self.is_stock(&sd) && !self.owned(&st, &sd)? {
            if st.owns_redirection() {
                log::warn!("termwrap: ServiceDll was redirected by someone else; hands off");
            }
            return self.status_locked();
        }
        if !st.enabled {
            if st.restore_pending {
                log::warn!("termwrap: an earlier switch-off did not finish; finishing it");
                self.switch_off(&st, st.blocked.clone())?;
            }
            return self.status_locked();
        }
        let verdict = match (self.host_block()?, st.wrapper_path.as_deref()) {
            (Some(b), _) => Err(b),
            (None, None) => Err((BlockReason::WrapperMissing, "no wrapper path stored".into())),
            (None, Some(wp)) => self.check_wrapper(Path::new(wp), &self.termsrv()).map(|_| wp.to_owned()),
        };
        match verdict {
            Err((reason, detail)) => {
                log::warn!("termwrap: boot gate failed ({reason:?} {detail}); restoring stock TermService");
                self.switch_off(&st, Some(reason))?;
            }
            Ok(wp) if !paths_equal(&sd, &wp) => {
                log::warn!("termwrap: redirection was lost; re-applying");
                self.apply_or_rollback(&wp)?;
            }
            Ok(_) => {}
        }
        self.status_locked()
    }
}
