// SPDX-License-Identifier: GPL-3.0-only
//! Write-side system traits used by the setup commands and the generic restore applier.
//! Each has a `#[cfg(windows)]` implementation in `win` and an in-memory fake in `fake`.

use crate::error::SetupError;
use crate::journal::ValueState;
use std::path::Path;

/// Injected time (same shape in every crate).
pub trait Clock: Send + Sync {
    fn now_ms(&self) -> u64;
}

/// HKLM registry access (64-bit view). Paths are relative to HKLM.
pub trait SetupRegistry: Send + Sync {
    /// `Absent` when the key or value is missing.
    fn get(&self, path: &str, name: &str) -> Result<ValueState, SetupError>;
    /// Creates the key when needed. `Key` / `Absent` are invalid here.
    fn set(&self, path: &str, name: &str, value: &ValueState) -> Result<(), SetupError>;
    /// Missing value or key is not an error.
    fn delete_value(&self, path: &str, name: &str) -> Result<(), SetupError>;
    fn key_exists(&self, path: &str) -> bool;
    fn create_key(&self, path: &str) -> Result<(), SetupError>;
    /// Missing key is not an error.
    fn delete_tree(&self, path: &str) -> Result<(), SetupError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoveOutcome {
    Removed,
    Missing,
    /// In use: scheduled with `MoveFileEx(MOVEFILE_DELAY_UNTIL_REBOOT)`.
    ScheduledForReboot,
}

pub trait SetupFs: Send + Sync {
    /// Create (recursively, with the descriptor from the start) and apply owner +
    /// protected DACL from SDDL. An existing folder that is a reparse point or has an
    /// untrusted owner is moved aside first; untrusted entries below it (reparse points,
    /// hard links, foreign owners) are deleted without following links.
    fn ensure_dir(&self, path: &Path, sddl: &str) -> Result<(), SetupError>;
    /// `Ok` when `path` is missing, or it and everything below it is trusted (no reparse
    /// point, no hard-linked file, owner SYSTEM / Administrators / the running account).
    /// Checked before uninstall replays journals from the data folder.
    fn check_trusted(&self, path: &Path) -> Result<(), SetupError>;
    fn exists(&self, path: &Path) -> bool;
    fn remove_file(&self, path: &Path) -> Result<RemoveOutcome, SetupError>;
    fn remove_dir_all(&self, path: &Path) -> Result<RemoveOutcome, SetupError>;
    fn copy(&self, from: &Path, to: &Path) -> Result<(), SetupError>;
    fn write(&self, path: &Path, bytes: &[u8]) -> Result<(), SetupError>;
}

pub trait FirewallGroup: Send + Sync {
    /// Display names of the rules whose grouping string is `group`.
    fn rules_in_group(&self, group: &str) -> Result<Vec<String>, SetupError>;
    /// Remove every rule of `group`; returns how many were removed.
    fn remove_group(&self, group: &str) -> Result<usize, SetupError>;
    /// Remove the rules named `name` whose grouping is `group` (same-named rules of other
    /// products are never touched); returns how many were removed (0 = none of ours).
    fn remove_rule(&self, name: &str, group: &str) -> Result<usize, SetupError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    pub instance_id: String,
    pub hardware_ids: Vec<String>,
    pub present: bool,
}

pub trait DeviceManager: Send + Sync {
    /// All devnodes, including non-present ones.
    fn list(&self) -> Result<Vec<DeviceInfo>, SetupError>;
    /// Remove a devnode (present or not). Returns `true` when a reboot is needed.
    fn remove(&self, instance_id: &str) -> Result<bool, SetupError>;
    /// Create a root-enumerated devnode with `hardware_id` and install `inf` on it
    /// (SetupAPI register + `UpdateDriverForPlugAndPlayDevices`). Returns
    /// `(instance id, reboot needed)`.
    fn create_root_device(&self, hardware_id: &str, class_guid: &str, inf: &Path) -> Result<(String, bool), SetupError>;
}

pub trait UserAccounts: Send + Sync {
    /// Delete a local account and its profile (`DeleteProfileW` before `NetUserDel`);
    /// missing is not an error. A profile that cannot be deleted keeps the account (error)
    /// so a later run can retry both.
    fn delete_user(&self, name: &str) -> Result<(), SetupError>;
}

/// SCM registration of our own service, used by the NSIS path (`install
/// --register-service`). The MSI registers the service itself with `ServiceInstall`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceSpec {
    pub name: String,
    pub display_name: String,
    pub description: String,
    /// Quoted exe path plus arguments, e.g. `"C:\…\pulsar-split-service.exe" --service`.
    pub command_line: String,
    /// Restart delays in milliseconds for the first, second and later failures.
    pub restart_delays_ms: Vec<u32>,
    pub reset_period_s: u32,
    /// Privileges the service keeps (`RequiredPrivileges`); empty = do not restrict.
    pub required_privileges: Vec<String>,
}

impl ServiceSpec {
    /// Spec 90 §2.7: LocalSystem, own process, demand start, SID type unrestricted,
    /// restart after 5 s / 30 s / 120 s, reset after one day.
    pub fn pulsar_split(exe: &Path) -> Self {
        Self {
            name: crate::layout::SERVICE_NAME.into(),
            display_name: "Pulsar Split".into(),
            description: "Pulsar Split çoklu oturum hizmeti".into(),
            command_line: format!("\"{}\" --service", exe.display()),
            restart_delays_ms: vec![5_000, 30_000, 120_000],
            reset_period_s: 86_400,
            required_privileges: Vec::new(),
        }
    }
}

pub trait ServiceRegistrar: Send + Sync {
    /// Create or update (idempotent). Returns `true` when something changed.
    fn ensure(&self, spec: &ServiceSpec) -> Result<bool, SetupError>;
    /// Stop (waiting up to `timeout_ms`) and delete; missing is not an error.
    fn remove(&self, name: &str, timeout_ms: u32) -> Result<(), SetupError>;
}

pub fn devices_with_hwid(list: &[DeviceInfo], hwid: &str) -> Vec<String> {
    list.iter()
        .filter(|d| d.hardware_ids.iter().any(|h| h.eq_ignore_ascii_case(hwid)))
        .map(|d| d.instance_id.clone())
        .collect()
}
