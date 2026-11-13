// SPDX-License-Identifier: GPL-3.0-only
//! Install / data paths, registry names and ACLs used by setup.

use std::path::{Path, PathBuf};

/// Data folder descriptor: owner SYSTEM (a pre-creating user must not stay owner and keep
/// WRITE_DAC), DACL SYSTEM + Administrators only, protected, inherited by children.
pub const DATA_DIR_SDDL: &str = "O:SYD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)";
/// Our registry root (HKLM). The reference product's root is never touched.
pub const ROOT_KEY: &str = r"SOFTWARE\PulsarSplit";
pub const SETUP_KEY: &str = r"SOFTWARE\PulsarSplit\Setup";
/// REG_MULTI_SZ under `SETUP_KEY`: driver ids chosen at install (empty = none). Reused
/// when a repair / upgrade passes no `--drivers`.
pub const SETUP_DRIVERS: &str = "Drivers";
pub const SECRETS_KEY: &str = r"SOFTWARE\PulsarSplit\Secrets";
/// Written by the MSI/NSIS "ExperimentalMultiSession" option only. Enabling the module
/// happens in the manager, after the warning (contract §2.4), never in the installer.
pub const TERMWRAP_KEY: &str = r"SOFTWARE\PulsarSplit\Termwrap";
pub const TERMWRAP_OFFERED: &str = "Offered";
pub const EVENTLOG_KEY: &str = r"SYSTEM\CurrentControlSet\Services\EventLog\Application\PulsarSplit";
pub const SERVICE_NAME: &str = "PulsarSplit";
/// Firewall grouping string shared by every rule we create (spec 80 §9.2).
pub const FIREWALL_GROUP: &str = "PulsarSplit";
/// Device-id markers of our own devnodes (spec 60 §9). Matched case-insensitively against
/// the instance id; never anything broader.
pub const OWN_DEVICE_MARKERS: [&str; 3] = [r"&PULSARPAD&", r"PULSARSPLITPAD\", r"ROOT\PULSARPAD\"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// `%ProgramFiles%\Pulsar Split` (or wherever the operator installed it).
    pub install_root: PathBuf,
    /// `%ProgramData%\PulsarSplit` (OQ 25: no space).
    pub data_root: PathBuf,
}

impl Layout {
    pub fn new(install_root: impl Into<PathBuf>, data_root: impl Into<PathBuf>) -> Self {
        Self { install_root: install_root.into(), data_root: data_root.into() }
    }

    /// Install root = folder of the running service exe; data root from `%ProgramData%`.
    pub fn from_env() -> Self {
        let install_root = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from(r"C:\Program Files\Pulsar Split"));
        let pd = std::env::var_os("ProgramData").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
        Self { install_root, data_root: pd.join("PulsarSplit") }
    }

    pub fn journal_dir(&self) -> PathBuf {
        self.data_root.join("journal")
    }
    pub fn logs_dir(&self) -> PathBuf {
        self.data_root.join("logs")
    }
    pub fn downloads_dir(&self) -> PathBuf {
        self.data_root.join("downloads")
    }
    /// Resolved driver manifest shipped by the installer (`installer/drivers.json` after
    /// `fetch-deps.ts` resolved INF globs).
    pub fn driver_manifest(&self) -> PathBuf {
        self.install_root.join("drivers.json")
    }
    /// Extracted / downloaded payload of one driver package.
    pub fn driver_payload(&self, id: &str) -> PathBuf {
        self.install_root.join("drivers").join(id)
    }
    pub fn stream_server_manifest(&self) -> PathBuf {
        self.install_root.join("stream-server.json")
    }
}

/// Does an instance id carry one of our markers?
pub fn is_own_device(instance_id: &str) -> bool {
    let up = instance_id.to_ascii_uppercase();
    OWN_DEVICE_MARKERS.iter().any(|m| up.contains(m))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        let l = Layout::new(r"C:\Program Files\Pulsar Split", r"C:\ProgramData\PulsarSplit");
        assert_eq!(l.journal_dir(), PathBuf::from(r"C:\ProgramData\PulsarSplit\journal"));
        assert_eq!(l.driver_payload("vdd"), PathBuf::from(r"C:\Program Files\Pulsar Split\drivers\vdd"));
        assert!(Layout::from_env().data_root.ends_with("PulsarSplit"));
    }

    #[test]
    fn markers_are_exact() {
        assert!(is_own_device(r"HID\VID_054C&PID_0CE6&PULSARPAD&0123\1&2"));
        assert!(is_own_device(r"ROOT\PULSARPAD\0000"));
        assert!(is_own_device(r"PulsarSplitPad\Seat1\0"));
        assert!(!is_own_device(r"HID\VID_054C&PID_0CE6&DUOCONTROLLER&0123\1&2"));
        assert!(!is_own_device(r"ROOT\SYSTEM\0006"));
    }
}
