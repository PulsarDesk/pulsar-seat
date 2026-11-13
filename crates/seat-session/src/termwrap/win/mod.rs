// SPDX-License-Identifier: GPL-3.0-only
//! Real Windows implementations of the termwrap system traits (adapted from pulsar-split
//! `split-termwrap`).
//!
//! None of this is exercised by the unit tests (they use [`crate::termwrap::fake`]). Every
//! call is read-only except registry writes and `WinTermService::{set_service_dll,
//! restart}`, which the engine only reaches from `enable` / `disable` / `boot_check` /
//! `restore_all`. **Untested on hardware.**

pub mod host;
pub mod inspector;
pub mod scm;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub(crate) use crate::win::{expand_env, last_error, wide};

use crate::error::SysError;
use crate::journal::{JsonlRestoreLog, RegValue};
use crate::registry::Registry;
use crate::termwrap::PACKAGE;
use crate::termwrap::engine::{TermwrapDeps, TermwrapPolicy, WindowsTermwrap};
use crate::termwrap::state::STATE_KEY;
use crate::termwrap::system::{FileInspector, HostFacts, KnownLocationDiscovery, SystemClock};
use crate::win::registry::WinRegistry;

/// Registry view for the engine: our own state key ([`STATE_KEY`]) lives in a JSON file
/// under `run\` (SYSTEM + Administrators only); every other path is the real HKLM.
pub struct RunStateRegistry {
    inner: Arc<dyn Registry>,
    file: PathBuf,
    lock: Mutex<()>,
}

impl RunStateRegistry {
    pub fn new(inner: Arc<dyn Registry>, file: PathBuf) -> Self {
        Self { inner, file, lock: Mutex::new(()) }
    }

    fn is_state(path: &str) -> bool {
        path.eq_ignore_ascii_case(STATE_KEY)
    }

    fn load(&self) -> Result<std::collections::BTreeMap<String, RegValue>, SysError> {
        match std::fs::read_to_string(&self.file) {
            Ok(t) => serde_json::from_str(&t).map_err(|e| SysError::Registry(format!("{}: {e}", self.file.display()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Default::default()),
            Err(e) => Err(SysError::Registry(format!("{}: {e}", self.file.display()))),
        }
    }

    fn store(&self, m: &std::collections::BTreeMap<String, RegValue>) -> Result<(), SysError> {
        let io = |e: std::io::Error| SysError::Registry(format!("{}: {e}", self.file.display()));
        if let Some(d) = self.file.parent() {
            std::fs::create_dir_all(d).map_err(io)?;
        }
        let tmp = self.file.with_extension("json.tmp");
        let text = serde_json::to_string_pretty(m).map_err(|e| SysError::Registry(e.to_string()))?;
        std::fs::write(&tmp, text).map_err(io)?;
        std::fs::rename(&tmp, &self.file).map_err(io)
    }
}

impl Registry for RunStateRegistry {
    fn get(&self, path: &str, name: &str) -> Result<RegValue, SysError> {
        if !Self::is_state(path) {
            return self.inner.get(path, name);
        }
        let _g = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        Ok(self.load()?.get(&name.to_ascii_lowercase()).cloned().unwrap_or(RegValue::Absent))
    }

    fn set(&self, path: &str, name: &str, value: &RegValue) -> Result<(), SysError> {
        if !Self::is_state(path) {
            return self.inner.set(path, name, value);
        }
        let _g = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let mut m = self.load()?;
        if *value == RegValue::Absent {
            m.remove(&name.to_ascii_lowercase());
        } else {
            m.insert(name.to_ascii_lowercase(), value.clone());
        }
        self.store(&m)
    }

    fn key_exists(&self, path: &str) -> Result<bool, SysError> {
        if Self::is_state(path) {
            return Ok(self.file.exists());
        }
        self.inner.key_exists(path)
    }
}

impl WindowsTermwrap {
    /// Real system deps: HKLM, SCM, WinVerifyTrust, the journal
    /// `<data_root>\journal\termwrap.jsonl` and the state file `<data_root>\run\termwrap-state.json`
    /// (data root normally `%ProgramData%\Pulsar Seat`).
    pub fn system(data_root: &Path, policy: TermwrapPolicy) -> Self {
        let files: Arc<dyn FileInspector> = Arc::new(inspector::WinFileInspector);
        let host = Arc::new(host::WinHostFacts);
        let pf = std::env::var("ProgramW6432").or_else(|_| std::env::var("ProgramFiles")).ok();
        let locations = KnownLocationDiscovery::default_locations(pf.as_deref(), host.system_root().as_deref());
        let state_file = data_root.join(seat_core::consts::RUN_DIR).join("termwrap-state.json");
        WindowsTermwrap::new(
            TermwrapDeps {
                scm: Arc::new(scm::WinTermService),
                discovery: Arc::new(KnownLocationDiscovery::new(files.clone(), locations)),
                files,
                registry: Arc::new(RunStateRegistry::new(Arc::new(WinRegistry), state_file)),
                host,
                journal: Arc::new(JsonlRestoreLog::in_data_root(data_root, PACKAGE)),
                clock: Arc::new(SystemClock),
            },
            policy,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::MemoryRegistry;

    #[test]
    fn state_key_goes_to_file_rest_to_registry() {
        let dir = tempfile::tempdir().unwrap();
        let inner = Arc::new(MemoryRegistry::default());
        let r = RunStateRegistry::new(inner.clone(), dir.path().join("run").join("s.json"));
        r.set(STATE_KEY, "Enabled", &RegValue::Dword(1)).unwrap();
        r.set(r"SYSTEM\X", "v", &RegValue::Dword(2)).unwrap();
        assert_eq!(r.get(STATE_KEY, "enabled").unwrap(), RegValue::Dword(1));
        assert_eq!(inner.get(STATE_KEY, "Enabled").unwrap(), RegValue::Absent);
        assert_eq!(inner.get(r"SYSTEM\X", "v").unwrap(), RegValue::Dword(2));
        r.set(STATE_KEY, "Enabled", &RegValue::Absent).unwrap();
        assert_eq!(r.get(STATE_KEY, "Enabled").unwrap(), RegValue::Absent);
    }
}
