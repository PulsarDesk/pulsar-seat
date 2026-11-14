// SPDX-License-Identifier: GPL-3.0-only
//! Install / data paths, registry names and ACLs used by the remote-kind setup
//! (`pulsar-seat-service remote-setup …`). Since merge step 10 the data root is the one
//! product's data folder (`%ProgramData%\Pulsar Seat`, shared with `seat-compat`) and the
//! third-party remote payload (driver manifest, driver packages, stream server) lives in
//! `<install>\remote\`, apart from the signed driver packages of `<install>\drivers\`.

use std::path::{Path, PathBuf};

/// Data folder root: owner SYSTEM (a pre-creating user must not stay owner and keep
/// WRITE_DAC), the Seat DACL (SYSTEM + Administrators full, Users read; seat-core
/// `DATA_DIR_SDDL`), protected, inherited by children.
pub const DATA_DIR_SDDL: &str = "O:SYD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FR;;;BU)";
/// `journal\`: owner SYSTEM, SYSTEM + Administrators only (seat-core `PRIVATE_DIR_SDDL`).
pub const PRIVATE_DIR_SDDL: &str = "O:SYD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)";
/// Our registry root (HKLM) for remote-kind state. The name stays `PulsarSplit` because
/// `shim/` reads `PulsarSplit\Shim` and `PulsarSplit\Runtime\Seats`. The reference
/// product's root is never touched.
pub const ROOT_KEY: &str = r"SOFTWARE\PulsarSplit";
pub const SETUP_KEY: &str = r"SOFTWARE\PulsarSplit\Setup";
/// REG_MULTI_SZ under `SETUP_KEY`: driver ids chosen at install (empty = none). Reused
/// when a repair / upgrade passes no `--drivers`.
pub const SETUP_DRIVERS: &str = "Drivers";
pub const SECRETS_KEY: &str = r"SOFTWARE\PulsarSplit\Secrets";
/// Experimental wrapper module state (enabled in the manager after its warning, never in
/// the installer).
pub const TERMWRAP_KEY: &str = r"SOFTWARE\PulsarSplit\Termwrap";
pub const TERMWRAP_OFFERED: &str = "Offered";
/// The one product's Event Log source (seat-core `EVENT_SOURCE`). The Seat `install`
/// step creates it first; remote setup then finds it and leaves it alone.
pub const EVENTLOG_KEY: &str = r"SYSTEM\CurrentControlSet\Services\EventLog\Application\PulsarSeat";
/// The one product's service (seat-core `SERVICE_NAME`).
pub const SERVICE_NAME: &str = seat_core::consts::SERVICE_NAME;
/// Pulsar Split's own service before the merge. `install` / `reconcile` stop and delete
/// a leftover one (installs from a pre-merge Split build).
pub const LEGACY_SERVICE_NAME: &str = "PulsarSplit";
/// Firewall grouping string shared by every rule we create (spec 80 §9.2).
pub const FIREWALL_GROUP: &str = "PulsarSplit";
/// Device-id markers of our own devnodes (spec 60 §9). Matched case-insensitively against
/// the instance id; never anything broader.
pub const OWN_DEVICE_MARKERS: [&str; 3] = [r"&PULSARPAD&", r"PULSARSPLITPAD\", r"ROOT\PULSARPAD\"];
/// Folder under the install root that holds the pinned third-party remote payload
/// (`installer/scripts/fetch-deps.ts` output): `drivers.json`, `drivers\<id>\`,
/// `stream-server.json`, `stream-server\`.
pub const REMOTE_PAYLOAD_DIR: &str = "remote";
/// Setup log of the remote kind under `logs\` (Seat's own `setup.log` has its own format
/// that the manager reads; the two never share a file).
pub const SETUP_LOG: &str = "remote-setup.log";
/// Journal files in `journal\` that belong to another format and are never replayed
/// here: `seat-compat`'s `changes.jsonl` (replayed by the Seat `uninstall`).
pub const FOREIGN_JOURNALS: [&str; 1] = ["changes.jsonl"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// `%ProgramFiles%\Pulsar Seat` (or wherever the operator installed it).
    pub install_root: PathBuf,
    /// `%ProgramData%\Pulsar Seat`.
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
            .unwrap_or_else(|| PathBuf::from(r"C:\Program Files").join(seat_core::consts::PRODUCT_DIR));
        let pd = std::env::var_os("ProgramData").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
        Self { install_root, data_root: pd.join(seat_core::consts::PRODUCT_DIR) }
    }

    pub fn journal_dir(&self) -> PathBuf {
        self.data_root.join(seat_core::consts::JOURNAL_DIR)
    }
    pub fn logs_dir(&self) -> PathBuf {
        self.data_root.join(seat_core::consts::LOGS_DIR)
    }
    pub fn downloads_dir(&self) -> PathBuf {
        self.data_root.join("downloads")
    }
    /// `<install>\remote\`: the pinned third-party remote payload.
    pub fn remote_payload(&self) -> PathBuf {
        self.install_root.join(REMOTE_PAYLOAD_DIR)
    }
    /// Resolved driver manifest shipped by the installer (`installer/remote/drivers.json`
    /// after `fetch-deps.ts` resolved INF globs).
    pub fn driver_manifest(&self) -> PathBuf {
        self.remote_payload().join("drivers.json")
    }
    /// Extracted / downloaded payload of one driver package.
    pub fn driver_payload(&self, id: &str) -> PathBuf {
        self.remote_payload().join("drivers").join(id)
    }
    pub fn stream_server_manifest(&self) -> PathBuf {
        self.remote_payload().join("stream-server.json")
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
        let l = Layout::new(r"C:\Program Files\Pulsar Seat", r"C:\ProgramData\Pulsar Seat");
        assert_eq!(l.journal_dir(), PathBuf::from(r"C:\ProgramData\Pulsar Seat\journal"));
        assert_eq!(l.driver_payload("vdd"), PathBuf::from(r"C:\Program Files\Pulsar Seat\remote\drivers\vdd"));
        assert_eq!(l.driver_manifest(), PathBuf::from(r"C:\Program Files\Pulsar Seat\remote\drivers.json"));
        assert_eq!(l.stream_server_manifest(), PathBuf::from(r"C:\Program Files\Pulsar Seat\remote\stream-server.json"));
        assert!(Layout::from_env().data_root.ends_with("Pulsar Seat"));
    }

    #[test]
    fn names_follow_the_one_product() {
        assert!(EVENTLOG_KEY.ends_with(seat_core::consts::EVENT_SOURCE));
        assert_eq!(SERVICE_NAME, "PulsarSeat");
        assert_ne!(LEGACY_SERVICE_NAME, SERVICE_NAME);
        // Same DACLs as seat-core, plus owner SYSTEM.
        assert_eq!(DATA_DIR_SDDL, format!("O:SY{}", seat_core::consts::DATA_DIR_SDDL));
        assert_eq!(PRIVATE_DIR_SDDL, format!("O:SY{}", seat_core::consts::PRIVATE_DIR_SDDL));
        assert_eq!(FOREIGN_JOURNALS[0], "changes.jsonl");
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
