// SPDX-License-Identifier: GPL-3.0-only
//! Our own stored state (adapted from pulsar-split). Stored under the *virtual* key
//! [`STATE_KEY`]: on Windows the service wires [`crate::termwrap::win::RunStateRegistry`],
//! which keeps this key in `%ProgramData%\Pulsar Seat\run\termwrap-state.json` (Pulsar Seat
//! keeps no settings in the registry, architecture §6):
//! `Enabled` (DWORD), `WrapperPath` (SZ), `AckWarningVersion` (DWORD), plus `Blocked`
//! (SZ, JSON of [`BlockReason`]) so `status()` can report why we switched ourselves off,
//! and `RestorePending` (DWORD): set before a disable / block / uninstall starts restoring
//! and cleared only once it finished (journal replayed and cleared, TermService
//! restarted), so a crash in between is finished on the next disable / boot check instead
//! of being mistaken for "enabled, redirection lost".
//! This is our own key, so it is not journaled; uninstall removes the whole root.

use crate::journal::RegValue;
use crate::termwrap::system::Registry;
use crate::termwrap::{BlockReason, TermwrapError};

pub const STATE_KEY: &str = r"SOFTWARE\Pulsar Seat\Termwrap";
pub const V_ENABLED: &str = "Enabled";
pub const V_WRAPPER_PATH: &str = "WrapperPath";
pub const V_ACK: &str = "AckWarningVersion";
pub const V_BLOCKED: &str = "Blocked";
pub const V_RESTORE_PENDING: &str = "RestorePending";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StoredState {
    /// The operator enabled the module and we own the ServiceDll redirection.
    pub enabled: bool,
    /// The wrapper we point (or pointed) ServiceDll at. Kept after a disable for the UI;
    /// it means "ours" only while `enabled || restore_pending`.
    pub wrapper_path: Option<String>,
    pub ack_warning_version: u32,
    pub blocked: Option<BlockReason>,
    /// A switch-off started and has not finished yet.
    pub restore_pending: bool,
}

impl StoredState {
    /// We own a ServiceDll that equals `wrapper_path` (enabled, or a switch-off is half done).
    pub fn owns_redirection(&self) -> bool {
        self.enabled || self.restore_pending
    }
}

pub fn load(reg: &dyn Registry) -> Result<StoredState, TermwrapError> {
    let dword = |name| -> Result<u32, TermwrapError> {
        Ok(match reg.get(STATE_KEY, name)? {
            RegValue::Dword(v) => v,
            _ => 0,
        })
    };
    let string = |name| -> Result<Option<String>, TermwrapError> {
        Ok(match reg.get(STATE_KEY, name)? {
            RegValue::Sz(s) | RegValue::ExpandSz(s) if !s.is_empty() => Some(s),
            _ => None,
        })
    };
    let blocked = match string(V_BLOCKED)? {
        Some(j) => match serde_json::from_str(&j) {
            Ok(b) => Some(b),
            Err(e) => {
                log::warn!("termwrap: unreadable Blocked value ignored: {e}");
                None
            }
        },
        None => None,
    };
    Ok(StoredState {
        enabled: dword(V_ENABLED)? != 0,
        wrapper_path: string(V_WRAPPER_PATH)?,
        ack_warning_version: dword(V_ACK)?,
        blocked,
        restore_pending: dword(V_RESTORE_PENDING)? != 0,
    })
}

pub fn save(reg: &dyn Registry, s: &StoredState) -> Result<(), TermwrapError> {
    reg.set(STATE_KEY, V_ENABLED, &RegValue::Dword(u32::from(s.enabled)))?;
    reg.set(
        STATE_KEY,
        V_WRAPPER_PATH,
        &s.wrapper_path.clone().map(RegValue::Sz).unwrap_or(RegValue::Absent),
    )?;
    reg.set(STATE_KEY, V_ACK, &RegValue::Dword(s.ack_warning_version))?;
    let blocked = match &s.blocked {
        Some(b) => RegValue::Sz(serde_json::to_string(b).map_err(|e| TermwrapError::Registry(e.to_string()))?),
        None => RegValue::Absent,
    };
    reg.set(STATE_KEY, V_BLOCKED, &blocked)?;
    let pending = if s.restore_pending { RegValue::Dword(1) } else { RegValue::Absent };
    Ok(reg.set(STATE_KEY, V_RESTORE_PENDING, &pending)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::termwrap::fake::FakeRegistry;
    use crate::version::FileVersion;

    #[test]
    fn roundtrip() {
        let reg = FakeRegistry::default();
        assert_eq!(load(&reg).unwrap(), StoredState::default());
        let s = StoredState {
            enabled: true,
            wrapper_path: Some(r"C:\w\rdpwrap.dll".into()),
            ack_warning_version: 1,
            blocked: Some(BlockReason::UnknownTermsrvBuild { version: FileVersion::new(10, 0, 1, 2) }),
            restore_pending: true,
        };
        save(&reg, &s).unwrap();
        assert_eq!(load(&reg).unwrap(), s);
        save(&reg, &StoredState::default()).unwrap();
        assert_eq!(load(&reg).unwrap(), StoredState::default());
        assert_eq!(reg.get(STATE_KEY, V_WRAPPER_PATH).unwrap(), RegValue::Absent);
    }
}
